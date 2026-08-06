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

#![allow(dead_code)]

use alloc::collections::BTreeMap;
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
	// Forward links to this process's threads (Weak, so they never keep a dead thread
	// alive). Signal delivery wakes each so a blocked thread observes a kill / stop at
	// its next scheduling point.
	threads: SpinLock<Vec<Weak<Thread>>>,
	// How many threads of this process have been registered and not yet reported exiting.
	// The `threads` vector answers "which threads exist to signal"; this answers "am I the
	// last one out", which is a different question and cannot be read off a snapshot.
	live_thread_count: AtomicUsize,
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
	// Eager system-image dynamic symbols registered by successfully relocated
	// provider modules. The registry is process-local: dependency order and symbol
	// visibility cannot leak between security domains or launches.
	dynamic_symbols: SpinLock<BTreeMap<String, u64>>,
	shared_image_pages: SpinLock<Vec<Arc<crate::elf::SharedPage>>>,
	dynamic_modules: AtomicUsize,
}

impl Process {
	// Create a process with a fresh handle table bound to `domain`, running in
	// `address_space`.
	pub fn new(address_space: Arc<AddressSpace>, domain: Arc<Domain>) -> Arc<Self> {
		let mut table = HandleTable::new();
		// Bind the table to the Domain so its handles are accounted there.
		table.set_domain(domain.clone());
		let process = Arc::new(Self { header: ObjectHeader::new(), address_space, handles: SpinLock::new(table), domain, fault: SpinLock::new(None), killed: AtomicBool::new(false), terminating: AtomicBool::new(false), exited: AtomicBool::new(false), exit_status: AtomicU64::new(0), exit_status_set: AtomicBool::new(false), exit_status_claimed: AtomicBool::new(false), user_frames: SpinLock::new(Vec::new()), threads: SpinLock::new(Vec::new()), stopped: AtomicBool::new(false), int_caught: AtomicBool::new(false), int_pending: AtomicBool::new(false), messages_sent: AtomicU64::new(0), messages_received: AtomicU64::new(0), stack_bytes: AtomicU64::new(0), mapped_memory: SpinLock::new(Vec::new()), mapped_dma: SpinLock::new(Vec::new()), dynamic_symbols: SpinLock::new(BTreeMap::new()), shared_image_pages: SpinLock::new(Vec::new()), dynamic_modules: AtomicUsize::new(0), live_thread_count: AtomicUsize::new(0) });
		// Register with the Domain so a Domain kill can reach and terminate it.
		process.domain.register_process(&process);
		process
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
	pub fn install(&self, object: Arc<dyn KernelObject>, rights: Rights, badge: u64) -> u64 {
		self.handles.lock().insert_object(object, rights, badge).raw()
	}

	// Take ownership of the physical frames backing this process's user image and
	// stack, so they are freed when the process is dropped.
	pub fn adopt_frames(&self, frames: Vec<u64>) {
		self.user_frames.lock().extend(frames);
	}

	pub fn adopt_shared_pages(&self, pages: Vec<Arc<crate::elf::SharedPage>>) {
		self.shared_image_pages.lock().extend(pages);
	}

	pub fn resolve_dynamic_symbol(&self, name: &str) -> Option<u64> {
		self.dynamic_symbols.lock().get(name).copied()
	}

	// Resolve a registered export by the tail of its mangled name. A Rust v0 symbol
	// carries a crate disambiguator hash derived from the crate's compilation metadata
	// (`_RNvNtCs<hash>_4lico6detect16detect_file_type`), and that hash differs per
	// target, so the full name is not something a caller can spell portably - only the
	// path and item after it are stable. Intended for callers that know which export
	// they mean but cannot know the hash; the tail must be specific enough to be
	// unambiguous, so the first match wins.
	pub fn resolve_dynamic_symbol_by_suffix(&self, suffix: &str) -> Option<u64> {
		self.dynamic_symbols.lock().iter().find(|(name, _)| name.ends_with(suffix)).map(|(_, address)| *address)
	}

	pub fn register_dynamic_symbols(&self, symbols: &[(String, u64)]) -> bool {
		let mut registry = self.dynamic_symbols.lock();
		if registry.len().checked_add(symbols.len()).is_none_or(|len| len > 65_536) || symbols.iter().any(|(name, _)| name.is_empty() || name.len() > crate::elf::MAX_DYNAMIC_SYMBOL_NAME || registry.contains_key(name)) {
			return false;
		}
		for (name, address) in symbols {
			registry.insert(name.clone(), *address);
		}
		true
	}

	pub fn reserve_dynamic_module(&self) -> bool {
		self.dynamic_modules.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| (count < 64).then_some(count + 1)).is_ok()
	}

	pub fn release_dynamic_module(&self) {
		self.dynamic_modules.fetch_sub(1, Ordering::AcqRel);
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
		// same order as `terminate`: going away, then the cleanup, then gone. A waiter that
		// saw `exited` used to be able to observe a finished process whose handles were
		// still open and whose mappings still existed.
		self.terminating.store(true, Ordering::Release);
		self.handles.lock().close_all();
		self.exited.store(true, Ordering::Release);
		sched::wake_object(self.header.koid());
	}

	// Whether the process has reached a terminal state - killed by a fault / kill, or
	// exited cleanly. This is the waitable "process terminated" condition: a Process
	// handle becomes ready in `wait` once it holds.
	pub fn is_terminated(&self) -> bool {
		self.killed.load(Ordering::Acquire) || self.exited.load(Ordering::Acquire)
	}

	// Record a thread as belonging to this process (a weak forward link), so signal
	// delivery can reach it. Called as the thread is built.
	pub fn register_thread(&self, thread: &Arc<Thread>) {
		self.live_thread_count.fetch_add(1, Ordering::AcqRel);
		self.threads.lock().push(Arc::downgrade(thread));
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
	pub fn live_threads(&self) -> Vec<Arc<Thread>> {
		let mut threads = self.threads.lock();
		threads.retain(|w: &Weak<Thread>| w.strong_count() > 0);
		threads.iter().filter_map(Weak::upgrade).collect()
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
	// process in place of termination).
	pub fn set_int_pending(&self) {
		self.int_pending.store(true, Ordering::Release);
	}

	// Poll and clear the pending caught SIG_INT, returning whether one was pending.
	pub fn take_int_pending(&self) -> bool {
		self.int_pending.swap(false, Ordering::AcqRel)
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

	pub fn private_image_pages(&self) -> usize {
		self.user_frames.lock().len()
	}

	pub fn shared_image_pages(&self) -> usize {
		self.shared_image_pages.lock().len()
	}

	// The number of handles this process's table currently holds.
	pub fn handle_count(&self) -> u64 {
		self.handles.lock().len() as u64
	}

	pub fn record_memory_mapping(&self, object: Arc<MemoryObject>) {
		self.mapped_memory.lock().push(object);
	}

	pub fn forget_memory_mapping(&self, object: &Arc<MemoryObject>) {
		self.mapped_memory.lock().retain(|mapped| !Arc::ptr_eq(mapped, object));
	}

	pub fn record_dma_mapping(&self, object: Arc<DmaBuffer>) {
		self.mapped_dma.lock().push(object);
	}

	pub fn forget_dma_mapping(&self, object: &Arc<DmaBuffer>) {
		self.mapped_dma.lock().retain(|mapped| !Arc::ptr_eq(mapped, object));
	}

	fn unmap_objects(&self) {
		let cr3 = self.address_space.cr3();
		for object in core::mem::take(&mut *self.mapped_memory.lock()) {
			object.remove_mapping(cr3);
		}
		for object in core::mem::take(&mut *self.mapped_dma.lock()) {
			object.remove_mapping(cr3);
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
		self.terminating.store(true, Ordering::Release);
		self.unmap_objects();
		self.handles.lock().close_all();
		self.killed.store(true, Ordering::Release);
		// A kill is a terminal state, so wake anything blocked on this process handle to
		// observe it - the same process-terminated signal a clean exit delivers.
		sched::wake_object(self.header.koid());
	}
}

impl Drop for Process {
	fn drop(&mut self) {
		self.unmap_objects();
		// Refund the Domain's stack account for every page this process's stack held.
		let stack = self.stack_bytes.swap(0, Ordering::AcqRel);
		if stack != 0 {
			self.domain.uncharge_stack(stack);
		}
		// Release the leaf data frames backing the user image and stack. The address
		// space, dropped alongside, reclaims only the page-table structure.
		let frames = core::mem::take(&mut *self.user_frames.lock());
		for frame in frames {
			crate::mem::frame::deallocate(frame);
		}
	}
}

impl_kernel_object!(Process, Process);

#[cfg(test)]
mod tests;
