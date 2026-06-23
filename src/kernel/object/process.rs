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
use core::sync::atomic::{AtomicBool, Ordering};

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
}

impl Process {
	// Create a process with a fresh handle table bound to `domain`, running in
	// `address_space`.
	pub fn new(address_space: Arc<AddressSpace>, domain: Arc<Domain>) -> Arc<Self> {
		let mut table = HandleTable::new();
		// Bind the table to the Domain so its handles are accounted there.
		table.set_domain(domain.clone());
		let process = Arc::new(Self { header: ObjectHeader::new(), address_space, handles: SpinLock::new(table), domain, fault: SpinLock::new(None), killed: AtomicBool::new(false), user_frames: SpinLock::new(Vec::new()), threads: SpinLock::new(Vec::new()), stopped: AtomicBool::new(false) });
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
