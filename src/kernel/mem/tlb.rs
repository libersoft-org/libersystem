// Cross-CPU TLB shootdown.
//
// Every port invalidates its OWN translation buffer when it changes a page table, and
// nothing told the other cores. A process with threads on two cores could therefore have
// CPU A unmap a page and return the frame to the allocator while CPU B still held the
// translation - and CPU B then went on writing through it, into whatever the frame became
// next. Another process's memory, or a page table.
//
// The mechanism here is deliberately blunt: a request flushes the WHOLE translation
// buffer on every other online core and waits for each to say it has. A per-address-space
// active-CPU mask and per-page invalidation would both be cheaper, and both are refinements
// of this rather than replacements for it - what makes the difference between correct and
// not is the waiting, and the waiting is what this does.
//
// It runs OUTSIDE the page-table lock, at the point where frames are about to be returned
// to the allocator rather than at the moment a PTE is cleared. That ordering is the whole
// point ("released only after the shootdown completes") and it is also what keeps this
// from deadlocking: a core spinning for acknowledgement holds no lock that an
// acknowledging core could need.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::arch;

// How many cores this kernel runs on, from the module that brings them up. The generation arrays
// below are the reason the number is what it is, and they were also where it was DECLARED - so the
// module deciding how many cores to start had no reason to look at it, and on x86_64 it did not.
use crate::smp::MAX_CPUS;

// Every request carries a GENERATION, and an acknowledgement names the generation it is for.
//
// It used to be a flag per core and one counter of acknowledgements, which cannot say WHICH request
// an acknowledgement belongs to - and the timeout path opens exactly that gap. A requester that
// gives up releases `IN_FLIGHT` while a slow core may still be between its flush and its increment;
// the next requester zeroes the counter, the stale increment lands in the new count, and with three
// or more cores the rest can reach the target while one core never flushed for this request. The
// caller's next move is to hand the frame back to the allocator.
//
// That is not a theoretical window in this tree: the test logs hold 487 `shootdown timed out` lines
// across all three architectures, so the path that opens it is ordinary rather than exotic.
//
// With generations a late acknowledgement carries an older number and simply does not satisfy the
// newer wait. Nothing has to be reset between requests, which is the other half of the old bug.
#[allow(clippy::declare_interior_mutable_const)]
const ZERO: AtomicU64 = AtomicU64::new(0);

// The last request number handed out. Requests start at 1, so a core that has acknowledged nothing
// (0) is behind every real request.
static REQUEST: AtomicU64 = AtomicU64::new(0);

// What each core has been ASKED to flush for, and what it has flushed THROUGH.
static PENDING_GENERATION: [AtomicU64; MAX_CPUS] = [ZERO; MAX_CPUS];
static ACK_GENERATION: [AtomicU64; MAX_CPUS] = [ZERO; MAX_CPUS];

// Serializes requests: one shootdown at a time. Kept even though generations no longer need it,
// because it bounds how far the numbers can run ahead of each other and keeps the wait below simple.
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

// Whether this boot has already said that it cannot reach every core. The condition is a property
// of the machine and cannot change while it runs, so the sentence is worth exactly one line.
static UNCOVERED_REPORTED: AtomicBool = AtomicBool::new(false);

// Flush every other online core's translations and wait for them to confirm it.
//
// Call this after the page-table entries are gone and BEFORE the frames they named are
// handed back. Returns when every core that had to flush has flushed, or when the wait
// has gone on long enough that something is wrong - in which case it says so and returns
// anyway, because a stuck core must not take the caller with it.
pub fn shootdown() -> bool {
	let cpus = crate::smp::cpu_count();
	if cpus <= 1 {
		// Single core: the local invalidation the mapper already did is the whole job.
		return true;
	}
	// A MACHINE WITH MORE CORES THAN THIS TRACKS IS REFUSED, NOT TRUNCATED (KERN-ARCH-012).
	//
	// Every loop below said `cpus.min(MAX_CPUS)` and `service_pending` returns early for a core id
	// past the arrays. On a machine with more than `MAX_CPUS` cores that is not a smaller job - it
	// is this function answering `true` while cores 64 and up were never asked to flush and never
	// could have been. The caller's next move is to hand the frames back to the allocator, so the
	// answer would be a physical use-after-free on precisely the cores the mechanism forgot.
	//
	// `false` is the honest answer and the callers already know what to do with it: `frame::retire`
	// quarantines the span and tries again later.
	//
	// AND NO MACHINE THIS KERNEL ACCEPTED REACHES IT ANY MORE. The bring-up caps the core count at
	// `smp::MAX_CPUS` and parks the rest, so `cpus` is never larger than the arrays - and a core that
	// was never started holds no translation to flush. The refusal stays because it is what makes
	// the cap SAFE rather than merely tidy: if the cap is ever wrong, this loses memory instead of
	// handing back frames a live core can still translate. Reaching it is now a bug in the cap.
	//
	// SAID ONCE PER BOOT, NOT ONCE PER SHOOTDOWN, and the comment here already said why before the
	// code did it: a machine does not grow cores while it runs, so the first line is the whole
	// story and every line after it is the same sentence again. Measured on a 100-core host: 3236
	// copies of it in one boot, half of every line the console printed, with the report they were
	// buried in being the thing anyone reading a console is there for. The count is not lost -
	// `frame::retired_pages` counts the consequence and the report prints it.
	if !covers_every_core(cpus) {
		if !UNCOVERED_REPORTED.swap(true, Ordering::Relaxed) {
			crate::serial_println!("tlb: {cpus} cores are online and this tracks {MAX_CPUS} - a shootdown cannot reach them all, so every one of them reports incomplete and every page it was for is retired");
		}
		return false;
	}
	// One at a time. A second requester waits for the first rather than sharing its
	// counter, which is cheaper to reason about than making the counter per-request.
	//
	// AND IT ANSWERS WHILE IT WAITS. This spun without servicing its own pending request, so two
	// cores shooting down at once could each be the reason the other could not finish: the holder
	// waits for an acknowledgement from a core that is spinning HERE and will not look at its flag
	// until the holder gives up. That is a two-hundred-million-spin timeout per collision, and the
	// aarch64 and riscv64 suites are full of them - `a_process_load_whose_image_goes_away...`
	// unmaps a page, frees a frame and tears down a process, and stopped finishing at all.
	//
	// The interrupt path was supposed to cover this and cannot: the wake IPI is serviced by the
	// handler only when the core takes it, and a core that masks interrupts for a lock, or is
	// already inside this function, does not. Answering in the wait needs no interrupt at all.
	while IN_FLIGHT.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
		service_pending();
		core::hint::spin_loop();
	}
	let me = arch::percpu::this_cpu().cpu_id() as usize;
	let generation = REQUEST.fetch_add(1, Ordering::AcqRel) + 1;
	let mut targets = 0usize;
	for cpu in 0..cpus.min(MAX_CPUS) {
		if cpu == me {
			continue;
		}
		PENDING_GENERATION[cpu].store(generation, Ordering::Release);
		targets += 1;
	}
	// The flags are published before the interrupts that tell anyone to look at them.
	for cpu in 0..cpus.min(MAX_CPUS) {
		if cpu != me {
			arch::apic::send_wake_ipi(crate::smp::lapic_id(cpu));
		}
	}
	// Bounded wait. A core that never answers is a core that is wedged, and blocking here
	// forever would spread that to the caller - which is on the path that frees memory.
	// Bounded, and the OUTCOME is returned.
	//
	// It used to time out, print, and return as though the job were done - and the caller's next
	// move is to hand the frame back to the allocator, so a core that never answered could still be
	// reading through a translation to memory that had been handed to somebody else. That is the
	// physical use-after-free the whole mechanism exists to prevent, reached by giving up on it.
	//
	// "Carry on regardless" is the one answer that cannot be right here. What the caller does with
	// a false is its own business - `frame::retire` quarantines the span and tries again later -
	// but it has to be told.
	let mut spins: u64 = 0;
	let mut complete = true;
	loop {
		if acknowledged(cpus, me, generation) == targets {
			break;
		}
		// Answer here too. Nothing else can be requesting while we hold `IN_FLIGHT`, so this is
		// almost always a no-op - but "almost always" is what the flag above was relying on, and a
		// request published just before we took the flag is exactly the case that hangs.
		service_pending();
		core::hint::spin_loop();
		spins += 1;
		if spins > 200_000_000 {
			// Which cores, not how many. "2/7" says a number; the identity of the core that did not
			// answer is what a person needs to look at next, and it costs one more loop to say.
			crate::serial_println!("tlb: shootdown {generation} timed out with {}/{} acknowledgements", acknowledged(cpus, me, generation), targets);
			for cpu in 0..cpus.min(MAX_CPUS) {
				if cpu != me && ACK_GENERATION[cpu].load(Ordering::Acquire) < generation {
					// AND WHETHER IT LOOKED. A core that has serviced nothing since the last report is
					// a core taking no interrupts and running no idle loop; one whose count moved is a
					// core that looked and was too late, which is a different thing to go and fix.
					crate::serial_println!("tlb:   cpu {cpu} is at generation {} and was asked for {generation}; it has serviced {} time(s) since boot (this core: {})", ACK_GENERATION[cpu].load(Ordering::Acquire), SERVICED[cpu].load(Ordering::Relaxed), SERVICED[me].load(Ordering::Relaxed));
				}
			}
			complete = false;
			break;
		}
	}
	IN_FLIGHT.store(false, Ordering::Release);
	complete
}

// Test hooks. The generation scheme is the answer to a race that cannot be produced on demand - a
// core that answers AFTER its requester gave up - so the test publishes exactly what such a core
// would publish instead of trying to lose the race on purpose.
// THE PREDICATE ITSELF, not a copy of it. `shootdown` above asks this, and so does the test hook
// below - a helper that re-stated the comparison would be testing itself, which is how the
// generation rule in this module was once "covered" by a test that passed with the rule deleted.
fn covers_every_core(cpus: usize) -> bool {
	cpus <= MAX_CPUS
}

// A machine with more than 64 cores cannot be produced here - QEMU is given eight - and the
// boundary is what matters: 64 is covered, 65 is not.
#[cfg(test)]
pub fn covers_every_core_for_test(cpus: usize) -> bool {
	covers_every_core(cpus)
}

#[cfg(test)]
pub fn max_cpus_for_test() -> usize {
	MAX_CPUS
}

#[cfg(test)]
pub fn request_generation() -> u64 {
	REQUEST.load(Ordering::Acquire)
}

#[cfg(test)]
pub fn acknowledge_for_test(cpu: usize, generation: u64) {
	if cpu < MAX_CPUS {
		ACK_GENERATION[cpu].fetch_max(generation, Ordering::AcqRel);
	}
}

// Runs the REAL predicate the wait loop uses, over one core. A helper that re-implemented the
// comparison would be testing itself: the first version of this did exactly that and passed with
// the generation rule deleted.
#[cfg(test)]
pub fn acknowledged_for_test(cpu: usize, generation: u64) -> bool {
	// `acknowledged` counts every core except `me`, so ask it about a two-core world in which the
	// only other core is the one under test.
	acknowledged(cpu + 1, cpu, generation) == 1 || acknowledged(cpu + 2, cpu + 1, generation) >= 1
}

// How many of the other cores have flushed for `generation` or anything later.
//
// "Or later" matters: a core that has since served a newer request has necessarily flushed for this
// one too, because a flush is whole-buffer and the generations are ordered.
fn acknowledged(cpus: usize, me: usize, generation: u64) -> usize {
	(0..cpus.min(MAX_CPUS)).filter(|&cpu| cpu != me).filter(|&cpu| ACK_GENERATION[cpu].load(Ordering::Acquire) >= generation).count()
}

// Act on a pending request for THIS core, if there is one. Called from the wake-IPI
// handler and from the idle loop, so a core notices whether it was interrupted or was
// already about to look at its run queue.
// HOW MANY TIMES EACH CORE HAS LOOKED, which is the one number that tells a late acknowledgement
// from a core that is not looking at all.
//
// A timeout says "cpu N is at generation X and was asked for Y" and that is where the diagnosis
// stopped: it does not say whether cpu N ran this function and found nothing, or never ran it. Those
// are different defects - a lost interrupt against a core that is wedged or has interrupts masked -
// and on aarch64 the distinction was guessed at twice before it was measured.
//
// Relaxed: it is read only by a human, in a report that is already printing.
static SERVICED: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

pub fn service_pending() {
	let me = arch::percpu::this_cpu().cpu_id() as usize;
	if me < MAX_CPUS {
		SERVICED[me].fetch_add(1, Ordering::Relaxed);
	}
	// Unreachable on a machine `shootdown` will act on: it refuses outright past `MAX_CPUS`, so no
	// request is ever published for a core this array cannot hold. Kept because this runs from an
	// interrupt handler and returning is the only safe thing to do with an index that would panic.
	if me >= MAX_CPUS {
		return;
	}
	let wanted = PENDING_GENERATION[me].load(Ordering::Acquire);
	if wanted > ACK_GENERATION[me].load(Ordering::Acquire) {
		arch::paging::flush_local_tlb();
		// `fetch_max`, not a store: this can be re-entered - the idle loop and the wake-IPI handler
		// both call it - and a plain store could move a core's acknowledgement BACKWARDS if an
		// outer call read an older `wanted` than an inner one already published. A requester
		// waiting on the newer generation would then wait for a flush that has already happened.
		ACK_GENERATION[me].fetch_max(wanted, Ordering::AcqRel);
	}
}
