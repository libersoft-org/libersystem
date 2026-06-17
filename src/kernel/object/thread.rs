// Thread kernel object.
//
// A Thread is a schedulable flow of execution with its own kernel stack. The
// scheduler keeps a saved stack pointer (kstack_ptr) for each thread that is not
// currently running; switch_context writes it on the way out and reads it on the
// way in. The thread owns its stack memory, which is freed when the last Arc to
// the thread is dropped (after it has exited and been switched away from).

#![allow(dead_code)]

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::address_space::AddressSpace;
use super::domain::Domain;
use super::handle::HandleTable;
use super::process::Process;
use super::{KernelObject, ObjectHeader, ObjectType};
use crate::arch;
use crate::sync::SpinLock;

// Per-thread kernel stack size.
pub const KERNEL_STACK_SIZE: usize = 16 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum ThreadState {
	Ready = 0,
	Running = 1,
	Exited = 2,
}

impl ThreadState {
	fn from_u32(value: u32) -> Self {
		match value {
			1 => ThreadState::Running,
			2 => ThreadState::Exited,
			_ => ThreadState::Ready,
		}
	}
}

static NEXT_TID: AtomicU64 = AtomicU64::new(1);

pub struct Thread {
	header: ObjectHeader,
	tid: u64,
	state: AtomicU32,
	// Saved stack pointer while the thread is not running on a core.
	kstack_ptr: AtomicU64,
	// Owns the kernel stack memory; accessed only through kstack_ptr.
	stack: Box<[u8]>,
	// The process this thread belongs to. It owns the address space, handle table,
	// and Domain the thread runs under, and outlives the thread.
	process: Arc<Process>,
}

impl Thread {
	// Create a ready-to-run kernel thread in `process` that starts at `entry(arg)`,
	// charging one thread slot to the process's Domain unconditionally (the
	// infallible path used for the unlimited root Domain).
	pub fn new(entry: extern "C" fn(u64), arg: u64, process: Arc<Process>) -> Arc<Self> {
		process.domain().account().charge_thread();
		Self::build(entry, arg, process)
	}

	// Like `new`, but enforce the process Domain's thread quota: returns None
	// (charging nothing) if the Domain is already at its thread cap.
	pub fn new_in(entry: extern "C" fn(u64), arg: u64, process: Arc<Process>) -> Option<Arc<Self>> {
		if !process.domain().account().try_charge_thread() {
			return None;
		}
		Some(Self::build(entry, arg, process))
	}

	// Shared constructor tail: fabricate the initial stack and assemble the Thread.
	fn build(entry: extern "C" fn(u64), arg: u64, process: Arc<Process>) -> Arc<Self> {
		let mut stack = alloc::vec![0u8; KERNEL_STACK_SIZE].into_boxed_slice();
		let sp = arch::context::init_thread_stack(&mut stack, entry, arg);
		Arc::new(Self { header: ObjectHeader::new(), tid: NEXT_TID.fetch_add(1, Ordering::Relaxed), state: AtomicU32::new(ThreadState::Ready as u32), kstack_ptr: AtomicU64::new(sp), stack, process })
	}

	pub fn tid(&self) -> u64 {
		self.tid
	}

	pub fn state(&self) -> ThreadState {
		ThreadState::from_u32(self.state.load(Ordering::Acquire))
	}

	pub fn set_state(&self, state: ThreadState) {
		self.state.store(state as u32, Ordering::Release);
	}

	pub fn address_space(&self) -> &Arc<AddressSpace> {
		self.process.address_space()
	}

	// The resource Domain this thread is accounted to (its process's Domain).
	pub fn domain(&self) -> &Arc<Domain> {
		self.process.domain()
	}

	// The process-wide handle table, shared across the process's threads.
	pub fn handles(&self) -> &SpinLock<HandleTable> {
		self.process.handles()
	}

	// The process this thread belongs to.
	pub fn process(&self) -> &Arc<Process> {
		&self.process
	}

	// Address of the saved-stack-pointer slot, handed to switch_context.
	pub fn kstack_ptr_addr(&self) -> *mut u64 {
		self.kstack_ptr.as_ptr()
	}

	pub fn kstack_ptr_load(&self) -> u64 {
		self.kstack_ptr.load(Ordering::Acquire)
	}
}

impl Drop for Thread {
	fn drop(&mut self) {
		// Refund this thread's slot to its process's Domain. When the process's last
		// thread drops, the Arc to the Process drops with it, tearing down the
		// process's handle table (refunding its handles) and address space.
		self.process.domain().account().uncharge_thread();
	}
}

impl KernelObject for Thread {
	fn header(&self) -> &ObjectHeader {
		&self.header
	}

	fn object_type(&self) -> ObjectType {
		ObjectType::Thread
	}

	fn as_any(&self) -> &dyn Any {
		self
	}
}
