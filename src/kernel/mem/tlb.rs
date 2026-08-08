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

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::arch;

// The most cores this kernel tracks. Matches the scheduler's own ceiling.
pub const MAX_CPUS: usize = 64;

// Per-core "flush your translations" flags, set by the requester and cleared by the core
// that acts on them.
#[allow(clippy::declare_interior_mutable_const)]
const NO_FLUSH: AtomicBool = AtomicBool::new(false);
static PENDING: [AtomicBool; MAX_CPUS] = [NO_FLUSH; MAX_CPUS];

// How many cores have acknowledged the request in flight.
static ACKED: AtomicUsize = AtomicUsize::new(0);

// Serializes requests: one shootdown at a time, so `ACKED` counts one thing.
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

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
	// One at a time. A second requester waits for the first rather than sharing its
	// counter, which is cheaper to reason about than making the counter per-request.
	while IN_FLIGHT.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
		core::hint::spin_loop();
	}
	let me = arch::percpu::this_cpu().cpu_id() as usize;
	let mut targets = 0usize;
	ACKED.store(0, Ordering::Release);
	for cpu in 0..cpus.min(MAX_CPUS) {
		if cpu == me {
			continue;
		}
		PENDING[cpu].store(true, Ordering::Release);
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
	while ACKED.load(Ordering::Acquire) < targets {
		core::hint::spin_loop();
		spins += 1;
		if spins > 200_000_000 {
			crate::serial_println!("tlb: shootdown timed out with {}/{} acknowledgements", ACKED.load(Ordering::Acquire), targets);
			complete = false;
			break;
		}
	}
	IN_FLIGHT.store(false, Ordering::Release);
	complete
}

// Act on a pending request for THIS core, if there is one. Called from the wake-IPI
// handler and from the idle loop, so a core notices whether it was interrupted or was
// already about to look at its run queue.
pub fn service_pending() {
	let me = arch::percpu::this_cpu().cpu_id() as usize;
	if me >= MAX_CPUS {
		return;
	}
	if PENDING[me].swap(false, Ordering::AcqRel) {
		arch::paging::flush_local_tlb();
		ACKED.fetch_add(1, Ordering::AcqRel);
	}
}
