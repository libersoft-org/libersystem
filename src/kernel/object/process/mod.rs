// Process kernel object.
//
// A Process is the unit of isolation: it owns an address space, a handle table,
// and is bound to a resource Domain. Its threads share all three - a handle
// opened by one thread is visible to its siblings, and they run in the same
// address space. A thread reaches these through its Process, so the handle table
// that used to be parked on the Thread as a stand-in now lives here, where it belongs.
//
// Threads hold an Arc to their Process, so the Process (and thus its address
// space and table) outlives them; the Process is torn down when its last thread
// is gone. A forward process-to-threads list for bulk termination arrives with
// fault handling and the Domain hierarchy.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::sync::Weak;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use super::address_space::AddressSpace;
use super::dma_buffer::DmaBuffer;
use super::domain::Domain;
use super::handle::HandleTable;
use super::memory_object::MemoryObject;
use super::rights::Rights;
use super::thread::Thread;
use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::fault::FaultInfo;
use crate::sched;
use crate::sync::SpinLock;

pub struct Process {
	header: ObjectHeader,
	address_space: Arc<AddressSpace>,
	// The process-wide handle table, shared by all of the process's threads.
	handles: SpinLock<HandleTable>,
	// The resource Domain this process and its threads are accounted to.
	domain: Arc<Domain>,
	// The fault that terminated this process, if any (first fault wins).
	fault: SpinLock<Option<FaultInfo>>,
	// Set when the process is killed (by a fault or a Domain kill); its threads
	// observe this at their next scheduling point and exit.
	killed: AtomicBool,
	// Set the moment a process starts going away, BEFORE any cleanup runs, and never
	// cleared. The terminal flags above are published when the process is gone; this is
	// published when it is going - and the difference is a window every mutating syscall
	// could previously slip through.
	//
	// A cleanup takes a snapshot of what to unmap and what to close. A thread that
	// registers a mapping, or creates a thread, after that snapshot and before the flag it
	// checks is set, leaves something behind that nothing will ever collect. Refusing from
	// here on closes the window at its start rather than at its end.
	terminating: AtomicBool,
	// How many resource-EXTENDING operations are inside their critical section right now, and the
	// lock that publishes `terminating` against them.
	//
	// The flag alone was never enough for anything wider than one instruction. `sys_memory_map`
	// read `is_terminating()` at its top and called `record_memory_mapping` at its bottom, with a
	// VA reservation and a page-table walk in between; `terminate` publishes the flag and takes its
	// `core::mem::take` snapshot in between those two. The check and the record were not one step
	// and the teardown ran between them.
	//
	// So an extending operation takes a GUARD - `begin_extend` - which is refused once the flag is
	// up and which `begin_teardown` waits out before its snapshot. "The process is live" is then
	// true for the whole operation rather than at its first instruction, and the record methods
	// live on the guard so there is no way to reach them without holding one.
	extending: SpinLock<usize>,
	// Set when the process's last thread has exited (a clean exit, no kill). Together
	// with `killed` this is the terminal "process gone" condition a Process handle
	// becomes waitable on - the kernel's equivalent of a process-terminated signal,
	// so a holder of the handle can wait for the process to finish instead of polling.
	exited: AtomicBool,
	// The status a clean exit reported, latched by the FIRST thread to call SYS_USER_EXIT.
	//
	// `killed` and `exited` already say HOW a process ended - a fault or kill against a clean
	// exit - and what was missing was the value a clean exit carries. Without it a waiter can
	// see that a program finished but not whether it succeeded, so `pipefail`, a shell's `$?`
	// and any success-gated step have to infer success from mere closure, which is the failure
	// this exists to remove: a program that ran and refused is indistinguishable from one that
	// worked.
	//
	// First writer wins, for the same reason the fault does: a multi-threaded process where two
	// threads exit differently has one answer, and it is the one that arrived first.
	exit_status: AtomicU64,
	exit_status_set: AtomicBool,
	// Who gets to write the status, kept apart from whether one is READABLE. One flag
	// cannot do both: it has to be claimed before the value is written and published
	// after, and those are opposite orders.
	exit_status_claimed: AtomicBool,
	// Physical frames backing this process's user image and stack. The address
	// space frees only its page-table structure, not the leaf frames its entries
	// point at, so the Process owns those frames and frees them on drop. Empty for
	// kernel processes (their threads run on the shared kernel mappings).
	user_frames: SpinLock<Vec<u64>>,
	// Set while an image or module load owns this process's adoption booking.
	//
	// `reserve_adopt` calls `try_reserve` on two vectors and `adopt_frames` extends them later, and
	// `try_reserve` books capacity against the CURRENT length while recording nothing. Two loads can
	// therefore both be told yes - A reserves 50 against len 100, B sees the capacity is already
	// there and reserves nothing, A adopts to len 150, and B's extend reallocates INFALLIBLY. That is
	// the defect the reservation exists to prevent, reached through concurrency rather than through a
	// missing call, and `begin_extend` does not close it: it COUNTS operations so teardown can wait
	// for them and explicitly does not serialise them.
	//
	// NOT A SPINLOCK, deliberately. A lock held across a load is a lock held across allocation and
	// mapping with interrupts masked, which is the hazard that got the scheduler's pre-booking
	// withdrawn earlier in this same milestone: heap growth waits on a TLB shootdown, and a core
	// spinning for the lock cannot acknowledge it. A one-owner gate costs one atomic, holds nothing,
	// and lets the second load be REFUSED - which is an answer `SYS_PROCESS_LOAD` already has and a
	// caller already handles, and a second concurrent load into one process is not a thing a correct
	// spawner does.
	image_load: AtomicBool,
	// Forward links to this process's threads (Weak, so they never keep a dead thread
	// alive). Signal delivery wakes each so a blocked thread observes a kill / stop at
	// its next scheduling point.
	threads: SpinLock<Vec<Weak<Thread>>>,
	// How many threads of this process have been registered and not yet reported exiting.
	// The `threads` vector answers "which threads exist to signal"; this answers "am I the
	// last one out", which is a different question and cannot be read off a snapshot.
	live_thread_count: AtomicUsize,
	// THE GROUPS THIS PROCESS IS A MEMBER OF, weakly, so a job can record what its stage did.
	//
	// A group holds `Weak<Process>` so membership never extends a member's life, and that is right -
	// but it means a group cannot answer "what did stage 2 exit with" after stage 2 is gone, and
	// that question is the whole of a pipeline's status. The answer has to be captured at the moment
	// the process reaches a terminal state, which is here, and the only way to reach the group from
	// here is a link back.
	//
	// Weak in this direction too: a Process that outlives its group must not keep the group alive,
	// and two strong links would be a cycle that never frees either.
	groups: SpinLock<Vec<alloc::sync::Weak<super::process_group::ProcessGroup>>>,
	// Set while the process is suspended (SIGSTOP); its threads park at their next
	// scheduling point until resumed (SIGCONT).
	stopped: AtomicBool,
	// Set when the process has armed itself to catch SIG_INT (SYS_SIGNAL_CATCH). While
	// armed, a delivered SIG_INT sets `int_pending` instead of terminating the process,
	// so a long-running tool can stop cleanly on Ctrl+C rather than being killed.
	int_caught: AtomicBool,
	// Set when a caught SIG_INT has been delivered and not yet consumed; the process
	// polls and clears it with SYS_SIGNAL_TAKE.
	int_pending: AtomicBool,
	// Set once a blocking wait has broken out to report this pending interrupt, so it breaks
	// each wait ONCE rather than every time. Without it a program that does not poll between
	// waits - which every loop that ignores its wait's result is - turns a hang into a spin, and
	// a spin keeps the run queue non-empty, which is strictly the worse of the two. Cleared by a
	// fresh delivery and by taking the flag, so every interrupt gets its one report.
	int_reported: AtomicBool,
	// Per-process IPC volume counters: the number of channel messages this process has
	// sent and received. Bumped on each successful channel send / recv, so a userspace
	// SystemGraphService can read a component's traffic over SYS_PROCESS_STATS_GET.
	messages_sent: AtomicU64,
	messages_received: AtomicU64,
	// Stack bytes charged to the Domain's stack account for this process (the eager
	// top pages plus every demand-paged growth page), refunded once at teardown.
	stack_bytes: AtomicU64,
	// Objects mapped into this address space. Holding an Arc keeps each object alive
	// until termination removes its PTEs and returns the virtual range.
	mapped_memory: SpinLock<Vec<Arc<MemoryObject>>>,
	mapped_dma: SpinLock<Vec<Arc<DmaBuffer>>>,
	// Every DmaBuffer this process CREATED, weakly, whether or not it is mapped and whether or not
	// it is still in the handle table.
	//
	// The orphan pass used to walk the handle table alone, and a buffer can be absent from that at
	// the instant the pass runs: `take_for_transfer` empties the slot and leaves it reserved, so
	// `for_each_object` sees nothing there. An unmarked buffer's frames go straight back into
	// circulation on drop - the rule losing its own precondition. A registry joined at CREATION
	// does not depend on the handle table being the only place a capability lives.
	dma_buffers: SpinLock<Vec<alloc::sync::Weak<DmaBuffer>>>,
	// Eager system-image dynamic symbols registered by successfully relocated
	// provider modules. The registry is process-local: dependency order and symbol
	// visibility cannot leak between security domains or launches.
	// A SORTED VECTOR, not a `BTreeMap`, so registering a module's exports is FALLIBLE.
	//
	// The map's `insert` allocates a node and the name is cloned into it, both infallibly, bounded
	// at 65,536 entries and reachable from `PROCESS_LOAD_MODULE` - a kernel abort on a short heap,
	// triggered by loading a module. `Vec` has `try_reserve` and `String` has one too, so both the
	// storage and the clone can refuse; the map has neither. Lookup stays logarithmic through
	// `binary_search_by`, and registration is a bulk operation per module load rather than a hot
	// path, so the insert shift costs nothing that matters.
	dynamic_symbols: SpinLock<Vec<(String, u64)>>,
	shared_image_pages: SpinLock<Vec<Arc<crate::elf::SharedPage>>>,
	dynamic_modules: AtomicUsize,
	// The biases dynamic modules are loaded at. `dynamic_modules` counts them; this says
	// WHICH, which is what a second load at the same address has to be refused against.
	dynamic_biases: SpinLock<Vec<u64>>,
}

impl Process {
	// Create a process with a fresh handle table bound to `domain`, running in
	// `address_space`.
	// FALLIBLY: `SYS_PROCESS_CREATE` reaches this, and a `Process` is one of the larger objects the
	// kernel builds on a syscall path.
	pub fn new(address_space: Arc<AddressSpace>, domain: Arc<Domain>) -> Option<Arc<Self>> {
		let mut table = HandleTable::new();
		// Bind the table to the Domain so its handles are accounted there.
		table.set_domain(domain.clone());
		let process = crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), address_space, handles: SpinLock::new(table), domain, fault: SpinLock::new(None), killed: AtomicBool::new(false), terminating: AtomicBool::new(false), extending: SpinLock::new(0), exited: AtomicBool::new(false), exit_status: AtomicU64::new(0), exit_status_set: AtomicBool::new(false), exit_status_claimed: AtomicBool::new(false), user_frames: SpinLock::new(Vec::new()), image_load: AtomicBool::new(false), threads: SpinLock::new(Vec::new()), stopped: AtomicBool::new(false), int_caught: AtomicBool::new(false), int_pending: AtomicBool::new(false), int_reported: AtomicBool::new(false), messages_sent: AtomicU64::new(0), messages_received: AtomicU64::new(0), stack_bytes: AtomicU64::new(0), mapped_memory: SpinLock::new(Vec::new()), mapped_dma: SpinLock::new(Vec::new()), dma_buffers: SpinLock::new(Vec::new()), dynamic_symbols: SpinLock::new(Vec::new()), shared_image_pages: SpinLock::new(Vec::new()), dynamic_modules: AtomicUsize::new(0), dynamic_biases: SpinLock::new(Vec::new()), live_thread_count: AtomicUsize::new(0), groups: SpinLock::new(Vec::new()) })?;
		// Register with the Domain so a Domain kill can reach and terminate it. A killed
		// Domain refuses, and the process is terminated at once rather than left running
		// under an authority that no longer accounts for it.
		if !process.domain.register_process(&process) {
			process.terminate();
		}
		Some(process)
	}

	pub fn address_space(&self) -> &Arc<AddressSpace> {
		&self.address_space
	}

	// The process-wide handle table (shared across the process's threads).
	pub fn handles(&self) -> &SpinLock<HandleTable> {
		&self.handles
	}

	// The resource Domain this process is accounted to.
	pub fn domain(&self) -> &Arc<Domain> {
		&self.domain
	}

	// Seed a capability to `object` into the table and return its raw handle, the
	// way a new process is endowed with an initial bootstrap capability.
	// Install a capability in this process's handle table, or `None` when the table could not take
	// it.
	//
	// IT USED TO RETURN A RAW `u64` AND SIGNAL FAILURE AS ZERO, which is not a handle and reads
	// exactly like one. `loader::spawn_elf_process` took that number and passed it to the child
	// without looking, so an out-of-memory at the last step of a spawn produced a process that
	// STARTED, was reported as spawned, and had no bootstrap capability - a failure the parent
	// could not detect and the child could not name. The quota leak on that path was fixed a round
	// earlier; the failure semantics were not, because the type could not express them.
	pub fn install(&self, object: Arc<dyn KernelObject>, rights: Rights, badge: u64) -> Option<u64> {
		match self.handles.lock().insert_object(object, rights, badge).raw() {
			0 => None,
			handle => Some(handle),
		}
	}

	// Take ownership of the physical frames backing this process's user image and
	// stack, so they are freed when the process is dropped.
	// Take ownership of ONE frame, fallibly.
	//
	// The stack-growth fault handler adopted with `adopt_frames(vec![frame])` - an infallible
	// allocation on a path ring 3 reaches by touching a guard page. This asks the allocator instead:
	// false means the frame was not adopted and the caller still owns it.
	pub fn try_adopt_frame(&self, frame: u64) -> bool {
		// CHARGED TO THE DOMAIN, because this frame is physical memory the process holds. The
		// account's own documentation said it counted "physical memory held" while the only
		// production charge was `MemoryObject::create_in` - so a process's image and its
		// fault-grown stack pages were outside the limit that claims to bound them.
		if !self.domain.try_charge_memory(crate::mem::frame::PAGE_SIZE) {
			return false;
		}
		let mut frames = self.user_frames.lock();
		if frames.try_reserve(1).is_err() {
			self.domain.uncharge_memory(crate::mem::frame::PAGE_SIZE);
			return false;
		}
		frames.push(frame);
		true
	}

	// Book the room the two `adopt_*` calls below need, so they cannot fail.
	//
	// "MOVED IN WHOLE AT SPAWN" was the marker on both, and it was false for the caller that matters:
	// `SYS_PROCESS_LOAD_MODULE` reaches `load_module_into`, which adopts into a process that already
	// owns every frame of its main image - so `extend` grows a vector that already has contents, on
	// a ring-3 path, with no way to report a failure. A bound over the image size says nothing about
	// whether the heap can hold the destination.
	//
	// Reserving separately from adopting is what makes the loader's transaction possible: the
	// booking happens while the load can still be unwound, and the adopt happens after the last
	// step that can fail.
	// Claim this process's adoption booking for the duration of one load, or `None` when another
	// load already holds it. The guard releases on drop, including on every rollback path.
	pub fn begin_image_load(&self) -> Option<ImageLoadGuard<'_>> {
		match self.image_load.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire) {
			Ok(_) => Some(ImageLoadGuard { process: self }),
			Err(_) => None,
		}
	}

	pub fn reserve_adopt(&self, frames: usize, pages: usize) -> bool {
		if self.user_frames.lock().try_reserve(frames).is_err() {
			return false;
		}
		if self.shared_image_pages.lock().try_reserve(pages).is_err() {
			return false;
		}
		// The DOMAIN's booking, taken here for the same reason the vector's is: while the load can
		// still be unwound. A caller that reserves and then fails before `adopt_frames` must call
		// `release_adopt_charge`; `load_module_into` is the one that can.
		self.domain.try_charge_memory(frames as u64 * crate::mem::frame::PAGE_SIZE)
	}

	// Give back a booking whose adoption never happened.
	pub fn release_adopt_charge(&self, frames: usize) {
		self.domain.uncharge_memory(frames as u64 * crate::mem::frame::PAGE_SIZE);
	}

	pub fn adopt_frames(&self, frames: Vec<u64>) {
		// ALLOC-OK: `reserve_adopt` booked this room before the caller reached its point of no
		// return, which is what makes the extend infallible rather than assumed to be.
		self.user_frames.lock().extend(frames);
	}

	pub fn adopt_shared_pages(&self, pages: Vec<Arc<crate::elf::SharedPage>>) {
		// ALLOC-OK: booked by `reserve_adopt`, as above.
		self.shared_image_pages.lock().extend(pages);
	}

	pub fn resolve_dynamic_symbol(&self, name: &str) -> Option<u64> {
		let registry = self.dynamic_symbols.lock();
		let at = registry.binary_search_by(|(known, _)| known.as_str().cmp(name)).ok()?;
		Some(registry[at].1)
	}

	// Resolve a registered export by the tail of its mangled name. A Rust v0 symbol
	// carries a crate disambiguator hash derived from the crate's compilation metadata
	// (`_RNvNtCs<hash>_4lico6detect16detect_file_type`), and that hash differs per
	// target, so the full name is not something a caller can spell portably - only the
	// path and item after it are stable. Intended for callers that know which export
	// they mean but cannot know the hash; the tail must be specific enough to be
	// unambiguous, so the first match wins.
	// The address of the one symbol whose name ends with `suffix`, or None.
	//
	// None when there is no match AND when there is more than one. It returned the FIRST
	// match while its own contract asked for uniqueness, so an ambiguous suffix silently
	// resolved to whichever symbol the map happened to yield first - a different one after
	// any change to the registry, with nothing to say the answer was a choice.
	#[cfg(test)]
	pub fn resolve_dynamic_symbol_by_suffix(&self, suffix: &str) -> Option<u64> {
		let registry = self.dynamic_symbols.lock();
		let mut matches = registry.iter().filter(|(name, _)| name.ends_with(suffix));
		let first = *matches.next().map(|(_, address)| address)?;
		if matches.next().is_some() {
			return None;
		}
		Some(first)
	}

	pub fn register_dynamic_symbols(&self, symbols: &[(String, u64)]) -> bool {
		let mut registry = self.dynamic_symbols.lock();
		let known = |registry: &Vec<(String, u64)>, name: &String| registry.binary_search_by(|(known, _)| known.cmp(name)).is_ok();
		if registry.len().checked_add(symbols.len()).is_none_or(|len| len > 65_536) || symbols.iter().any(|(name, _)| name.is_empty() || name.len() > crate::elf::MAX_DYNAMIC_SYMBOL_NAME || known(&registry, name)) {
			return false;
		}
		// ROOM FIRST, FOR ALL OF THEM, and the names cloned fallibly - so a short heap refuses the
		// registration whole rather than aborting partway through it. All-or-nothing matters here:
		// a module whose exports are half registered resolves some of its own symbols and not
		// others, which is worse than not loading.
		if registry.try_reserve(symbols.len()).is_err() {
			return false;
		}
		let mut cloned: Vec<(String, u64)> = Vec::new();
		if cloned.try_reserve(symbols.len()).is_err() {
			return false;
		}
		for (name, address) in symbols {
			let mut copy = String::new();
			if copy.try_reserve(name.len()).is_err() {
				return false;
			}
			copy.push_str(name);
			cloned.push((copy, *address));
		}
		for entry in cloned {
			match registry.binary_search_by(|(known, _)| known.cmp(&entry.0)) {
				// Refused above, so this is unreachable; overwriting is what the map did and is the
				// harmless answer if the check above is ever changed.
				Ok(at) => registry[at] = entry,
				Err(at) => registry.insert(at, entry),
			}
		}
		true
	}

	// Claim a module SLOT at `bias`, not merely a place in the count.
	//
	// The count alone let two loads at the same bias both pass: both mapped the same
	// addresses, the second overwrote the first's page-table entries, and either one's
	// rollback unmapped the other's. A count answers "how many"; it cannot answer "is this
	// address already taken", and that is the question a loader is asking.
	//
	// The mapper refusing to overwrite a live leaf catches the collision now too, but as a
	// mapping failure partway through rather than as a refusal up front - which leaves the
	// first module's rollback and the second's interleaved. This refuses before either
	// touches the address space.
	pub fn reserve_dynamic_module_at(&self, bias: u64) -> bool {
		let mut biases = self.dynamic_biases.lock();
		if biases.len() >= 64 || biases.contains(&bias) {
			return false;
		}
		// A BOUND IS NOT A BOOKING. The marker here read "bounded by the loader's module-graph
		// limit", which says there will never be a sixty-fifth entry and nothing at all about
		// whether the heap can hold the first one. This is on the `SYS_PROCESS_LOAD_MODULE` path,
		// so the growth is ring-3 reachable, and the refusal it needs already exists.
		if biases.try_reserve(1).is_err() {
			return false;
		}
		biases.push(bias);
		self.dynamic_modules.store(biases.len(), Ordering::Release);
		true
	}

	// Give a claimed slot back, for a module load that did not complete.
	pub fn release_dynamic_module_at(&self, bias: u64) {
		let mut biases = self.dynamic_biases.lock();
		biases.retain(|taken| *taken != bias);
		self.dynamic_modules.store(biases.len(), Ordering::Release);
	}

	pub fn has_dynamic_modules(&self) -> bool {
		self.dynamic_modules.load(Ordering::Acquire) != 0
	}

	// Record `bytes` of mapped stack against the Domain's stack account (and this
	// process, for the one-shot refund at teardown).
	pub fn charge_stack(&self, bytes: u64) {
		self.stack_bytes.fetch_add(bytes, Ordering::AcqRel);
		self.domain.charge_stack(bytes);
	}

	// Record the fault that is terminating this process. The first fault wins:
	// once set it is not overwritten, so the original cause is preserved.
	// Latch the status a clean exit reported. First writer wins; later callers are ignored
	// rather than overwriting, so the answer a waiter reads never changes once it exists.
	pub fn set_exit_status(&self, status: u64) {
		// The value first, THEN the flag that says there is one. It was the other way
		// round, so a reader that saw the flag could still read the old status - zero, for
		// every process that had not exited before - and report a clean exit for a value
		// that had not been written yet.
		//
		// The swap still decides who writes (first writer wins), and the store beneath it
		// is what the release below publishes.
		if !self.exit_status_claimed.swap(true, Ordering::AcqRel) {
			self.exit_status.store(status, Ordering::Relaxed);
			self.exit_status_set.store(true, Ordering::Release);
		}
	}

	// The status a clean exit reported, or None if this process never reported one - which is
	// every process that faulted, was killed, or is still running. A caller that needs to tell
	// "succeeded" from "never got to say" must distinguish those, which is why this is an
	// Option rather than a 0 that means both.
	pub fn exit_status(&self) -> Option<u64> {
		self.exit_status_set.load(Ordering::Acquire).then(|| self.exit_status.load(Ordering::Acquire))
	}

	pub fn set_fault(&self, info: FaultInfo) {
		let mut slot = self.fault.lock();
		if slot.is_none() {
			*slot = Some(info);
		}
	}

	// The fault that terminated this process, if one was recorded.
	pub fn fault_info(&self) -> Option<FaultInfo> {
		*self.fault.lock()
	}

	// Whether this process has begun going away. True from the first moment of a kill or a
	// clean exit, and the condition every mutating syscall refuses on: after this, nothing
	// new may be registered that the cleanup already walking past would miss.
	pub fn is_terminating(&self) -> bool {
		self.terminating.load(Ordering::Acquire)
	}

	// Set ONLY the terminating flag. For the test that checks the map syscalls refuse from the
	// first moment of a kill: `terminate` would also close the handle table, and a map refused
	// for a dead handle looks exactly like a map refused for a dying process.
	#[cfg(test)]
	pub fn begin_terminating_for_test(&self) {
		self.terminating.store(true, Ordering::Release);
	}

	// Whether this process has been killed and its threads should exit.
	pub fn is_killed(&self) -> bool {
		self.killed.load(Ordering::Acquire)
	}

	// Mark the process as having exited cleanly (its last thread is gone), close its
	// handle table, and wake anything blocked on the process handle, so a waiter
	// observes the termination at once. Closing the handles here - exactly as the
	// kill path's `terminate` does - is what releases the process's channel endpoints:
	// a peer (a shell relaying a tool's stdout) learns the process is gone by its
	// channel closing, and that must not wait for the LAST Process reference to drop -
	// a supervisor or job table legitimately holds one long after the exit. Idempotent:
	// the scheduler calls it as the final thread retires.
	pub fn mark_exited(&self) {
		// The same order as `terminate`, and now the same WORK: going away, the cleanup, then gone.
		//
		// It closed the handles and stopped there, so a clean exit left the process's MemoryObject
		// and DmaBuffer mappings in place until the `Process` itself dropped - which a supervisor
		// holding a handle can delay for as long as it likes. `Terminated` then meant "the threads
		// are finished" rather than "the address space is frozen", and the two paths to the same
		// state did different amounts of it.
		self.begin_teardown();
		self.unmap_objects();
		self.orphan_dma_buffers();
		self.handles.lock().close_all();
		self.exited.store(true, Ordering::Release);
		self.record_in_groups();
		sched::wake_object(self.header.koid());
	}

	// Take note of a group this process belongs to. Called by `ProcessGroup::create`, which is the
	// only thing that can make one - membership is sealed there, so a given group is added once.
	pub fn join_group(&self, group: &alloc::sync::Arc<super::process_group::ProcessGroup>) -> bool {
		let mut groups = self.groups.lock();
		// DEAD LINKS GO FIRST, so this list is bounded by the groups that still exist rather than by
		// every group this process has ever been in. A long-lived process joined to a succession of
		// short-lived groups walked all of them on every terminal path, and each dead one cost a
		// failed upgrade for nothing.
		groups.retain(|existing| existing.strong_count() > 0);
		// FALLIBLY, like every other allocation a ring-3 caller can drive. A process may be in more
		// than one group and a caller decides how many, so this is a list whose length is chosen by
		// userspace.
		if groups.try_reserve(1).is_err() {
			return false;
		}
		groups.push(alloc::sync::Arc::downgrade(group));
		true
	}

	// Forget a group. The unwind half of `join_group`: `ProcessGroup::create` fails if any member
	// cannot take a back-link, and the members that already took one must not be left pointing at a
	// group that never came into existence.
	pub fn leave_group(&self, group_koid: u64) {
		let mut groups = self.groups.lock();
		groups.retain(|existing| !existing.upgrade().is_some_and(|group| group.header().koid() == group_koid));
	}

	// Tell every group this process is in what it finished as. Called from both terminal paths, and
	// idempotent at the group's end: whichever of the two runs first writes the record and a second
	// call leaves it alone, because a process that was killed after exiting cleanly still exited
	// cleanly.
	// UNDER THE LOCK, and asking the heap for nothing. This was `self.groups.lock().clone()` - an
	// infallible `Vec` allocation on every process teardown, which is a path that runs precisely
	// when memory has already run out. The clone bought nothing: `record_member` takes the GROUP's
	// lock, never this one, so there is no re-entry to avoid.
	fn record_in_groups(&self) {
		// The koids to wake, taken while the list is locked and woken after it is not.
		//
		// `notify_member_terminated` existed and NOTHING called it. A direct `wait` on a group works
		// because it registers on the individual processes; a WaitSet records and blocks on the
		// GROUP's koid alone, so a member reaching a terminal state changed the group's readiness
		// and woke nobody. With no deadline that set sleeps for ever while being ready - which is
		// the one failure a wait primitive may not have.
		//
		// A fixed array rather than a Vec: this runs on the terminal path, where an allocation is the
		// last thing wanted, and a process cannot be in more groups than a group can hold members.
		let mut koids: [u64; crate::object::process_group::MAX_GROUP_MEMBERS] = [0; crate::object::process_group::MAX_GROUP_MEMBERS];
		let mut count = 0usize;
		{
			let groups = self.groups.lock();
			for group in groups.iter().filter_map(alloc::sync::Weak::upgrade) {
				group.record_member(self);
				if count < koids.len() {
					koids[count] = group.header().koid();
					count += 1;
				}
			}
		}
		// Outside the lock: waking takes scheduler locks, and the ordering between those and this
		// one is not a thing to discover on a teardown path.
		for &koid in &koids[..count] {
			crate::sched::wake_object(koid);
		}
	}

	// Whether the process has reached a terminal state - killed by a fault / kill, or
	// exited cleanly. This is the waitable "process terminated" condition: a Process
	// handle becomes ready in `wait` once it holds.
	pub fn is_terminated(&self) -> bool {
		self.killed.load(Ordering::Acquire) || self.exited.load(Ordering::Acquire)
	}

	// Record a thread as belonging to this process (a weak forward link), so signal
	// delivery can reach it. Called as the thread is built.
	// Take a new thread into this process, or refuse because the process is going away.
	//
	// The refusal is decided UNDER the `threads` lock, and `begin_teardown` sets the flag while
	// holding the same lock - so the two orderings that used to race cannot interleave any more. A
	// caller that read `is_terminating() == false` and then built a thread could previously register
	// it after `close_all` had run: the process acquired a thread after its cleanup, with its
	// handles gone and its mappings dropped.
	//
	// This is the contained half of the lifecycle barrier. The general shape - `Live | Terminating |
	// Dead` with a guard every resource-extending syscall holds - is still worth having; what is
	// closed here is the one path that demonstrably ends with a live thread inside a dead process.
	#[must_use]
	// Claim the right to START `thread`, atomically against teardown.
	//
	// `sys_thread_start` read `is_terminating()` and then enqueued in a separate step, so a
	// termination could begin AND FINISH in between - the thread then entered a process whose
	// handles were closed and whose mappings were gone. `begin_teardown` publishes the flag under
	// this same `threads` lock, so checking and claiming under it is what makes the two ordered
	// rather than merely adjacent.
	//
	// Returns false when the process is tearing down or the thread was already started; the caller
	// enqueues only on true, and the enqueue happens outside this lock.
	pub fn claim_thread_start(&self, thread: &Arc<Thread>) -> bool {
		let _threads = self.threads.lock();
		if self.terminating.load(Ordering::Acquire) {
			return false;
		}
		thread.try_start()
	}

	fn register_thread(&self, thread: &Arc<Thread>) -> bool {
		let mut threads = self.threads.lock();
		if self.terminating.load(Ordering::Acquire) {
			return false;
		}
		// FALLIBLY, and under the lock that makes this a barrier - which is precisely where an
		// abort is least welcome and most necessary.
		if threads.try_reserve(1).is_err() {
			return false;
		}
		self.live_thread_count.fetch_add(1, Ordering::AcqRel);
		threads.push(Arc::downgrade(thread));
		true
	}

	// Publish "this process is going away", and WAIT OUT everything already extending it.
	//
	// Two locks, because the flag has to be published against both things that read it. Under
	// `threads`, a `register_thread` either sees the flag and refuses or completes before this
	// returns. Under `extending`, the same holds for every other resource-extending operation -
	// and those are not single instructions, so the flag being up is not enough: this then spins
	// until the count reaches zero, which is what makes the caller's `core::mem::take` snapshot
	// complete rather than merely late.
	//
	// The spin terminates because a guard is only ever held across a bounded syscall body, and it
	// cannot deadlock against this: nothing takes a guard on a process it is tearing down. The
	// lock is dropped between attempts so a holder on this core's queue can still make progress.
	fn begin_teardown(&self) {
		{
			let _threads = self.threads.lock();
			let _extending = self.extending.lock();
			self.terminating.store(true, Ordering::Release);
		}
		// AND IT ANSWERS WHILE IT WAITS - the same rule `tlb::request` had to learn, for the same
		// reason and with the same failure.
		//
		// A user fault enters termination from trap context with interrupts masked, and this spins
		// until every `ExtendGuard` leaves. A concurrent core can hold one of those guards while
		// rolling back mapped frames, and that rollback asks for a TLB shootdown whose
		// acknowledgement must come from THIS core - which is spinning here, with interrupts off, not
		// looking at its flag. The holder then waits out the two-hundred-million-spin timeout before
		// it can release the guard, so a fault teardown deterministically stalls the kernel and
		// pushes reusable frames into quarantine.
		//
		// The interrupt path cannot cover it: interrupts are masked precisely because this is a trap.
		// Answering in the wait needs no interrupt at all.
		while *self.extending.lock() != 0 {
			crate::mem::tlb::service_pending();
			core::hint::spin_loop();
		}
	}

	// One thread of this process has finished. Returns true for the thread that was the
	// LAST one, exactly once, whichever order they arrive in.
	//
	// The scheduler used to decide this by counting the other live threads in a snapshot:
	// two last threads exiting at the same time each saw the other, neither called itself
	// the last, and neither finalised the process. It then stayed formally alive forever -
	// `wait` never returning, handles never closing, peers never seeing `PeerClosed`,
	// quotas never released. A snapshot cannot answer a question about the moment after
	// it was taken; a counter can.
	pub fn thread_exited(&self) -> bool {
		self.live_thread_count.fetch_sub(1, Ordering::AcqRel) == 1
	}

	// This process's currently-live threads, pruning any that have been dropped.
	//
	// ALLOCATES, through `collect`, which is why the two ring-3 paths that used it do not any more:
	// `SYS_PROCESS_SIGNAL` walks this to wake every thread and `SYS_PROCESS_STATS_GET` used it to
	// ask whether any thread is left, so a short heap turned either syscall into a kernel abort. The
	// allocation gate does not look at `collect` at all, which is how it stayed. Kept for the test
	// suites, which are the callers that want an owned list and are not reachable from ring 3.
	#[cfg(test)]
	pub fn live_threads(&self) -> Vec<Arc<Thread>> {
		let mut threads = self.threads.lock();
		threads.retain(|w: &Weak<Thread>| w.strong_count() > 0);
		// ALLOC-OK: the owned-list form, kept for the test suites - `SYS_PROCESS_SIGNAL` walks
		// `live_threads_from` and `SYS_PROCESS_STATS_GET` reads the counter, so no ring-3 path
		// reaches this any more.
		threads.iter().filter_map(Weak::upgrade).collect()
	}

	// A BATCH of live threads, into storage the caller already has.
	//
	// Fills `out` from position `from` onwards and answers how many were written and where to
	// resume, so a caller with a fixed array can walk every thread without allocating. The lock is
	// released before the caller touches what it was given: waking a thread takes the scheduler's
	// lock, and dropping the last reference to one runs `Thread::drop`, neither of which may happen
	// underneath this one.
	pub fn live_threads_from(&self, from: usize, out: &mut [Option<Arc<Thread>>]) -> (usize, usize) {
		let threads = self.threads.lock();
		let mut written = 0;
		let mut at = from;
		while at < threads.len() && written < out.len() {
			if let Some(thread) = threads[at].upgrade() {
				out[written] = Some(thread);
				written += 1;
			}
			at += 1;
		}
		(written, at)
	}

	// How many threads this process has registered and not yet counted out.
	//
	// `SYS_PROCESS_STATS_GET` asked this by building a `Vec<Arc<Thread>>` and testing whether it was
	// empty, which is an allocation to answer a question a counter already holds.
	pub fn live_thread_count(&self) -> usize {
		self.live_thread_count.load(Ordering::Acquire)
	}

	// Whether the process is currently suspended (SIGSTOP).
	pub fn is_stopped(&self) -> bool {
		self.stopped.load(Ordering::Acquire)
	}

	// Set or clear the suspended state (SIGSTOP sets, SIGCONT clears).
	pub fn set_stopped(&self, stopped: bool) {
		self.stopped.store(stopped, Ordering::Release);
	}

	// Arm the process to catch SIG_INT: a subsequent SIG_INT sets the pending flag
	// rather than terminating the process. A self-service disposition; a process only
	// arms itself.
	pub fn catch_int(&self) {
		self.int_caught.store(true, Ordering::Release);
	}

	// Whether the process has armed itself to catch SIG_INT.
	pub fn is_int_caught(&self) -> bool {
		self.int_caught.load(Ordering::Acquire)
	}

	// Record that a caught SIG_INT was delivered (set by signal delivery on an armed
	// process in place of termination). A fresh delivery is reportable again, so a second
	// Ctrl+C breaks a wait even if the program never read the first.
	pub fn set_int_pending(&self) {
		self.int_reported.store(false, Ordering::Release);
		self.int_pending.store(true, Ordering::Release);
	}

	// Poll and clear the pending caught SIG_INT, returning whether one was pending.
	pub fn take_int_pending(&self) -> bool {
		self.int_reported.store(false, Ordering::Release);
		self.int_pending.swap(false, Ordering::AcqRel)
	}

	// Claim the right to break ONE blocking wait for a pending caught interrupt. True at most once
	// per delivery: the flag itself is left alone, because SYS_SIGNAL_TAKE is what consumes it and
	// taking it here would swallow the interrupt on its way to the code whose job is to read it.
	//
	// Once per delivery rather than every wait, because the alternative is a spin. A program that
	// polls between waits (which is why it armed itself at all) gets its one break and acts on it;
	// one that does not - any loop that discards its wait's result and retries - would otherwise
	// come straight back and be released again, forever.
	pub fn take_int_report(&self) -> bool {
		if !self.int_caught.load(Ordering::Acquire) || !self.int_pending.load(Ordering::Acquire) {
			return false;
		}
		!self.int_reported.swap(true, Ordering::AcqRel)
	}

	// Count a channel message this process has sent (one successful send).
	pub fn record_send(&self) {
		self.messages_sent.fetch_add(1, Ordering::Relaxed);
	}

	// Count a channel message this process has received (one successful recv).
	pub fn record_recv(&self) {
		self.messages_received.fetch_add(1, Ordering::Relaxed);
	}

	// The number of channel messages this process has sent.
	pub fn messages_sent(&self) -> u64 {
		self.messages_sent.load(Ordering::Relaxed)
	}

	// The number of channel messages this process has received.
	pub fn messages_received(&self) -> u64 {
		self.messages_received.load(Ordering::Relaxed)
	}

	// The number of bytes of user memory this process has mapped (the leaf frames
	// backing its image and stack).
	pub fn memory_bytes(&self) -> u64 {
		self.user_frames.lock().len() as u64 * crate::mem::frame::PAGE_SIZE
	}

	#[cfg(test)]
	pub fn private_image_pages(&self) -> usize {
		self.user_frames.lock().len()
	}

	#[cfg(test)]
	pub fn shared_image_pages(&self) -> usize {
		self.shared_image_pages.lock().len()
	}

	// The number of handles this process's table currently holds.
	pub fn handle_count(&self) -> u64 {
		self.handles.lock().len() as u64
	}

	// Begin a resource-EXTENDING operation, or refuse because the process is going away.
	//
	// The refusal is decided under `extending`, and `begin_teardown` publishes the flag while
	// holding the same lock and then waits for the count to reach zero - so an operation either
	// sees the flag and refuses, or finishes before the teardown takes its snapshot. That is the
	// difference between a barrier and a second racing flag, and it is the same shape
	// `register_thread` already had against the `threads` lock, generalised.
	pub fn begin_extend(&self) -> Option<ExtendGuard<'_>> {
		let mut extending = self.extending.lock();
		if self.terminating.load(Ordering::Acquire) {
			return None;
		}
		*extending += 1;
		drop(extending);
		Some(ExtendGuard { process: self })
	}

	fn end_extend(&self) {
		*self.extending.lock() -= 1;
	}

	pub fn forget_memory_mapping(&self, object: &Arc<MemoryObject>) {
		self.mapped_memory.lock().retain(|mapped| !Arc::ptr_eq(mapped, object));
	}

	pub fn forget_dma_mapping(&self, object: &Arc<DmaBuffer>) {
		self.mapped_dma.lock().retain(|mapped| !Arc::ptr_eq(mapped, object));
	}

	// Mark every DmaBuffer this process still holds as one its owner never released.
	//
	// THIS PROCESS NEVER SAID ITS DEVICES WERE DONE - not on a kill, and not on an exit that left
	// its handles for the kernel to close. Either way the buffer's physical address may be sitting
	// in a live descriptor, so the drop that follows holds the frames for that device instead of
	// returning them to circulation. A buffer the process closed ITSELF is never marked and is
	// retired exactly as before: that difference is the whole rule, and it is the case a
	// `submit`/`complete` pair cannot express, because the process that would call `complete` is
	// the one that is gone.
	//
	// Before `close_all`, because after it there is nothing left to mark.
	// Runs AFTER `begin_teardown`, which is what makes "every buffer this process ever created" a
	// closed set: a creation that had not joined the registry yet was inside a guard the teardown
	// waited out, and a creation that starts later is refused the guard.
	fn orphan_dma_buffers(&self) {
		self.handles.lock().for_each_object(|object| {
			if let Some(dma) = object.as_any().downcast_ref::<crate::object::dma_buffer::DmaBuffer>() {
				dma.mark_orphaned();
			}
		});
		// AND the registry, which is the half the handle table cannot answer for: a buffer in
		// flight through `take_for_transfer` is in no table at all, and one whose handle was closed
		// while another process still holds a reference is in someone else's.
		for weak in self.dma_buffers.lock().iter() {
			if let Some(dma) = weak.upgrade() {
				dma.mark_orphaned();
			}
		}
	}

	fn unmap_objects(&self) {
		for object in core::mem::take(&mut *self.mapped_memory.lock()) {
			object.remove_mapping(&self.address_space);
		}
		for object in core::mem::take(&mut *self.mapped_dma.lock()) {
			object.remove_mapping(&self.address_space);
		}
	}

	// Terminate this process: mark it killed and close all its handles, refunding
	// their resources (and the memory the objects pinned) to the Domain at once.
	// Its threads observe the kill at their next scheduling point and exit,
	// releasing the last reference to the Process.
	pub fn terminate(&self) {
		// Terminating FIRST, terminal last. Everything that could add to what has to be
		// cleaned up refuses from here, so the cleanup below sees a set that cannot grow
		// under it; `killed` is published at the end, when it is true.
		//
		// Through `begin_teardown`, which publishes under the `threads` lock: the flag alone was a
		// second racing atomic, and a `register_thread` that had already passed its check could
		// still land after `close_all`.
		self.begin_teardown();
		self.unmap_objects();
		self.orphan_dma_buffers();
		self.handles.lock().close_all();
		self.killed.store(true, Ordering::Release);
		self.record_in_groups();
		// A kill is a terminal state, so wake anything blocked on this process handle to
		// observe it - the same process-terminated signal a clean exit delivers.
		sched::wake_object(self.header.koid());
	}
}

impl Drop for Process {
	fn drop(&mut self) {
		// A TERMINAL RECORD FOR A PROCESS THAT NEVER RAN.
		//
		// A process can be added to a ProcessGroup and lose its last strong reference before any
		// thread is started - a spawn that fails after the group is joined, a handle closed on a
		// prepared-but-unstarted process. Nothing ran, so neither `terminate` nor the exit path
		// published a transition, and the group's weak member simply expired. Group COMPLETION then
		// counted the member as gone while a SNAPSHOT found neither a live process nor a record and
		// reported the slot as still running: two answers that contradict each other, and a pipeline
		// whose stage outcome is lost.
		//
		// `record_member` writes at most once and does nothing for a slot that already has a record,
		// so this is a no-op for every process that reached a terminal state the ordinary way.
		if !self.is_terminated() {
			self.killed.store(true, Ordering::Release);
			self.record_in_groups();
		}
		self.unmap_objects();
		// Refund the Domain's stack account for every page this process's stack held.
		let stack = self.stack_bytes.swap(0, Ordering::AcqRel);
		if stack != 0 {
			self.domain.uncharge_stack(stack);
		}
		// Release the leaf data frames backing the user image and stack. The address
		// space, dropped alongside, reclaims only the page-table structure.
		let frames = core::mem::take(&mut *self.user_frames.lock());
		// The image and fault-grown pages go back to the Domain's account, matching the charges
		// `reserve_adopt` and `try_adopt_frame` took.
		if !frames.is_empty() {
			self.domain.uncharge_memory(frames.len() as u64 * crate::mem::frame::PAGE_SIZE);
		}
		// Every core that ever ran a thread of this process may still hold translations for these,
		// so they go back to the allocator only once nobody does - which `retire` is what decides.
		//
		// SAFETY: these are the frames the process adopted, taken out of its list above so nothing
		// else can reach them.
		unsafe {
			crate::mem::frame::retire(&frames);
			// A process going away frees its whole image; take the shootdown here rather than
			// leaving a process's worth of memory quarantined until somebody else fills the queue.
			crate::mem::frame::drain_quarantine();
		}
	}
}

// A resource-EXTENDING operation in progress, and the only way to reach the three records.
//
// The records are methods here rather than on `Process` so that "held the guard across the check
// AND the record" is not a convention a caller can forget: without one there is nothing to call.
// Each is fallible for the same reason every other syscall-reachable allocation in this kernel is -
// a ring-3 caller decides how many of these exist, and a short heap must be a refusal rather than
// an abort.
pub struct ExtendGuard<'a> {
	process: &'a Process,
}

impl ExtendGuard<'_> {
	#[must_use]
	pub fn record_memory_mapping(&self, object: Arc<MemoryObject>) -> bool {
		let mut mapped = self.process.mapped_memory.lock();
		if mapped.try_reserve(1).is_err() {
			return false;
		}
		mapped.push(object);
		true
	}

	#[must_use]
	pub fn record_dma_mapping(&self, object: Arc<DmaBuffer>) -> bool {
		let mut mapped = self.process.mapped_dma.lock();
		if mapped.try_reserve(1).is_err() {
			return false;
		}
		mapped.push(object);
		true
	}

	// Join the process's DMA registry. Called where the buffer is CREATED, not where it is mapped:
	// an unmapped buffer is still a physical address a device may have been given.
	#[must_use]
	pub fn record_dma_buffer(&self, object: &Arc<DmaBuffer>) -> bool {
		let mut buffers = self.process.dma_buffers.lock();
		// Dead entries first, so a driver that cycles buffers does not grow the list without bound
		// and the reservation below is against a real length.
		buffers.retain(|weak| weak.strong_count() != 0);
		if buffers.try_reserve(1).is_err() {
			return false;
		}
		buffers.push(Arc::downgrade(object));
		true
	}

	#[must_use]
	pub fn register_thread(&self, thread: &Arc<Thread>) -> bool {
		self.process.register_thread(thread)
	}
}

impl Drop for ExtendGuard<'_> {
	fn drop(&mut self) {
		self.process.end_extend();
	}
}

impl_kernel_object!(Process, Process);

#[cfg(test)]
mod tests;

// Held for the length of one image or module load - see `Process::image_load`.
pub struct ImageLoadGuard<'a> {
	process: &'a Process,
}

impl Drop for ImageLoadGuard<'_> {
	fn drop(&mut self) {
		self.process.image_load.store(false, Ordering::Release);
	}
}
