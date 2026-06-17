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
use super::rights::Rights;
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
	address_space: Arc<AddressSpace>,
	// Per-process handle table. M6 places it on the thread as a stand-in until a
	// Process object exists to own it and share it across a process's threads.
	handles: SpinLock<HandleTable>,
	// The resource Domain this thread is accounted to. Its thread slot is charged
	// at creation and refunded when the thread is dropped.
	domain: Arc<Domain>,
}

impl Thread {
	// Create a ready-to-run kernel thread that will start at `entry(arg)`,
	// charging one thread slot to `domain` unconditionally (the infallible path
	// used for the unlimited root Domain).
	pub fn new(entry: extern "C" fn(u64), arg: u64, address_space: Arc<AddressSpace>, domain: Arc<Domain>) -> Arc<Self> {
		domain.account().charge_thread();
		Self::build(entry, arg, address_space, domain, HandleTable::new())
	}

	// Like `new`, but enforce the Domain's thread quota: returns None (charging
	// nothing) if the Domain is already at its thread cap.
	pub fn new_in(entry: extern "C" fn(u64), arg: u64, address_space: Arc<AddressSpace>, domain: Arc<Domain>) -> Option<Arc<Self>> {
		if !domain.account().try_charge_thread() {
			return None;
		}
		Some(Self::build(entry, arg, address_space, domain, HandleTable::new()))
	}

	// Create a ready-to-run kernel thread pre-seeded with a single handle to
	// `object` in its table. The thread receives that handle's raw value as its
	// argument - a minimal bootstrap-handle hand-off, the way a new execution
	// context is endowed with its initial capability.
	pub fn new_with_object(entry: extern "C" fn(u64), address_space: Arc<AddressSpace>, object: Arc<dyn KernelObject>, rights: Rights, badge: u64, domain: Arc<Domain>) -> Arc<Self> {
		domain.account().charge_thread();
		let mut table = HandleTable::new();
		// Bind the table to the Domain before seeding so the bootstrap handle is
		// counted like any other.
		table.set_domain(domain.clone());
		let handle = table.insert_object(object, rights, badge);
		Self::build_with_table(entry, handle.raw(), address_space, domain, table)
	}

	// Shared constructor tail: bind the table to the Domain, fabricate the initial
	// stack, and assemble the Thread.
	fn build(entry: extern "C" fn(u64), arg: u64, address_space: Arc<AddressSpace>, domain: Arc<Domain>, mut table: HandleTable) -> Arc<Self> {
		table.set_domain(domain.clone());
		Self::build_with_table(entry, arg, address_space, domain, table)
	}

	fn build_with_table(entry: extern "C" fn(u64), arg: u64, address_space: Arc<AddressSpace>, domain: Arc<Domain>, table: HandleTable) -> Arc<Self> {
		let mut stack = alloc::vec![0u8; KERNEL_STACK_SIZE].into_boxed_slice();
		let sp = arch::context::init_thread_stack(&mut stack, entry, arg);
		Arc::new(Self { header: ObjectHeader::new(), tid: NEXT_TID.fetch_add(1, Ordering::Relaxed), state: AtomicU32::new(ThreadState::Ready as u32), kstack_ptr: AtomicU64::new(sp), stack, address_space, handles: SpinLock::new(table), domain })
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
		&self.address_space
	}

	// The resource Domain this thread is accounted to.
	pub fn domain(&self) -> &Arc<Domain> {
		&self.domain
	}

	// The calling process's handle table (shared across a process's threads once
	// a Process object owns it).
	pub fn handles(&self) -> &SpinLock<HandleTable> {
		&self.handles
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
		// Refund this thread's slot to its Domain. The handle table refunds its
		// own remaining handles in its Drop, and the objects those handles held
		// refund their memory as their last reference drops.
		self.domain.account().uncharge_thread();
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
