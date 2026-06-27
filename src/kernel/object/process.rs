// Process kernel object.
//
// A Process is the unit of isolation: it owns an address space, a handle table,
// and is bound to a resource Domain. Its threads share all three - a handle
// opened by one thread is visible to its siblings, and they run in the same
// address space. A thread reaches these through its Process, so the handle table
// that M6 parked on the Thread as a stand-in now lives here, where it belongs.
//
// Threads hold an Arc to their Process, so the Process (and thus its address
// space and table) outlives them; the Process is torn down when its last thread
// is gone. A forward process-to-threads list for bulk termination arrives with
// fault handling and the Domain hierarchy.

#![allow(dead_code)]

use alloc::sync::Arc;
use alloc::sync::Weak;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::address_space::AddressSpace;
use super::domain::Domain;
use super::handle::HandleTable;
use super::rights::Rights;
use super::thread::Thread;
use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::fault::FaultInfo;
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
	// Physical frames backing this process's user image and stack. The address
	// space frees only its page-table structure, not the leaf frames its entries
	// point at, so the Process owns those frames and frees them on drop. Empty for
	// kernel processes (their threads run on the shared kernel mappings).
	user_frames: SpinLock<Vec<u64>>,
	// Forward links to this process's threads (Weak, so they never keep a dead thread
	// alive). Signal delivery wakes each so a blocked thread observes a kill / stop at
	// its next scheduling point.
	threads: SpinLock<Vec<Weak<Thread>>>,
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
}

impl Process {
	// Create a process with a fresh handle table bound to `domain`, running in
	// `address_space`.
	pub fn new(address_space: Arc<AddressSpace>, domain: Arc<Domain>) -> Arc<Self> {
		let mut table = HandleTable::new();
		// Bind the table to the Domain so its handles are accounted there.
		table.set_domain(domain.clone());
		let process = Arc::new(Self { header: ObjectHeader::new(), address_space, handles: SpinLock::new(table), domain, fault: SpinLock::new(None), killed: AtomicBool::new(false), user_frames: SpinLock::new(Vec::new()), threads: SpinLock::new(Vec::new()), stopped: AtomicBool::new(false), int_caught: AtomicBool::new(false), int_pending: AtomicBool::new(false), messages_sent: AtomicU64::new(0), messages_received: AtomicU64::new(0) });
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

	// Record the fault that is terminating this process. The first fault wins:
	// once set it is not overwritten, so the original cause is preserved.
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

	// Whether this process has been killed and its threads should exit.
	pub fn is_killed(&self) -> bool {
		self.killed.load(Ordering::Acquire)
	}

	// Record a thread as belonging to this process (a weak forward link), so signal
	// delivery can reach it. Called as the thread is built.
	pub fn register_thread(&self, thread: &Arc<Thread>) {
		self.threads.lock().push(Arc::downgrade(thread));
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

	// The number of handles this process's table currently holds.
	pub fn handle_count(&self) -> u64 {
		self.handles.lock().len() as u64
	}

	// Terminate this process: mark it killed and close all its handles, refunding
	// their resources (and the memory the objects pinned) to the Domain at once.
	// Its threads observe the kill at their next scheduling point and exit,
	// releasing the last reference to the Process.
	pub fn terminate(&self) {
		self.killed.store(true, Ordering::Release);
		self.handles.lock().close_all();
	}
}

impl Drop for Process {
	fn drop(&mut self) {
		// Release the leaf data frames backing the user image and stack. The address
		// space, dropped alongside, reclaims only the page-table structure.
		let frames = core::mem::take(&mut *self.user_frames.lock());
		for frame in frames {
			crate::mem::frame::deallocate(frame);
		}
	}
}

impl_kernel_object!(Process, Process);
