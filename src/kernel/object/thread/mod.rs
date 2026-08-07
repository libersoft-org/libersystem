// Thread kernel object.
//
// A Thread is a schedulable flow of execution with its own kernel stack. The
// scheduler keeps a saved stack pointer (kstack_ptr) for each thread that is not
// currently running; switch_context writes it on the way out and reads it on the
// way in. The thread owns its stack memory, which is freed when the last Arc to
// the thread is dropped (after it has exited and been switched away from).

#![allow(dead_code)]

use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use super::address_space::AddressSpace;
use super::domain::Domain;
use super::handle::HandleTable;
use super::process::Process;
use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
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
	// Descheduled, waiting for an object to become ready or a deadline to pass.
	// The thread is held alive by the scheduler's wait registry, not a run queue.
	Blocked = 3,
}

impl ThreadState {
	fn from_u32(value: u32) -> Self {
		match value {
			1 => ThreadState::Running,
			2 => ThreadState::Exited,
			3 => ThreadState::Blocked,
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
	// Parked kernel stack pointer a ring-3 syscall switches onto, set by
	// usermode::enter while this thread is in ring 3 (0 otherwise). The scheduler
	// restores it into the per-CPU block on every switch, so cooperative ring-3
	// services that yield to one another on one core keep separate syscall stacks.
	syscall_rsp: AtomicU64,
	// Owns the kernel stack memory; accessed only through kstack_ptr.
	stack: KernelStack,
	// Set the first time the thread is enqueued through thread_start, so a thread
	// built suspended (the userspace spawn path) can be started exactly once and a
	// repeated start is a safe no-op rather than a double-enqueue.
	started: AtomicBool,
	// The process this thread belongs to. It owns the address space, handle table,
	// and Domain the thread runs under, and outlives the thread.
	process: Arc<Process>,
}

// A kernel thread's stack, in its own virtual range with an unmapped page below it.
//
// It was a `Box<[u8]>` from the kernel heap, which means an overflow does not fault: it
// walks into whatever the allocator put underneath - another thread's stack, a heap
// object, allocator metadata - and the damage surfaces somewhere else entirely, at some
// later time, with nothing pointing back at the thread that caused it.
//
// A guard page turns that into a page fault at the moment of the overflow. The frames are
// mapped one page above the base of the range, so the base page is never mapped and the
// first write past the bottom of the stack faults on an address that names the thread it
// belongs to.
pub struct KernelStack {
	// Base of the reserved range - the guard page. Never mapped.
	base: u64,
	pages: usize,
}

impl KernelStack {
	// None when the frames or the kernel virtual range are not there, leaving nothing behind.
	//
	// It used to `expect` both. Thread creation is reachable from ring 3 - SYS_THREAD_CREATE,
	// and every process spawn makes one - so a frame pool that has run low turned an ordinary
	// userspace call into a kernel panic. The Domain thread quota does not help: it bounds how
	// many threads ONE process may have, not whether there is memory for the next one, and the
	// pressure can come from anywhere in the system.
	fn allocate() -> Option<Self> {
		let pages = KERNEL_STACK_SIZE.div_ceil(crate::mem::frame::PAGE_SIZE as usize);
		let len = (pages as u64 + 1) * crate::mem::frame::PAGE_SIZE;
		let base = crate::syscall::alloc_kernel_vrange(len);
		if base == 0 {
			return None;
		}
		let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::NO_EXECUTE;
		for page in 0..pages {
			// one page above `base`, so `base` itself stays unmapped and is the guard. The
			// mapping goes through the address-routing `try_map_page` rather than through an
			// `AddressSpace`: this is a kernel-half range, and on aarch64 a named root is a
			// TTBR0 value that a higher-half address does not live in. Routing by address is
			// correct on every port by construction rather than by a rule applied underneath.
			let at = base + (page as u64 + 1) * crate::mem::frame::PAGE_SIZE;
			let mapped = crate::mem::frame::allocate().filter(|frame| arch::paging::try_map_page(at, *frame, flags).is_ok());
			let Some(frame) = mapped else {
				// Unwind by hand rather than by building a partial `KernelStack` and dropping
				// it. Drop frees `(self.pages + 1)` pages of virtual range, which for a
				// partial stack is SHORTER than what was reserved - so the tail of the
				// reservation would be lost from the kernel window on every failed spawn.
				rollback(base, page, len);
				return None;
			};
			let _ = frame;
		}
		// zeroed through a raw write rather than through a slice taken from `&self`,
		// which would be handing out a `&mut` from a shared borrow.
		unsafe { core::ptr::write_bytes((base + crate::mem::frame::PAGE_SIZE) as *mut u8, 0, pages * crate::mem::frame::PAGE_SIZE as usize) };
		Some(Self { base, pages })
	}

	// The stack bytes, above the guard page. `&mut self` because this hands out the only
	// mutable view of a region this struct owns exclusively.
	fn as_mut_slice(&mut self) -> &mut [u8] {
		let start = self.base + crate::mem::frame::PAGE_SIZE;
		unsafe { core::slice::from_raw_parts_mut(start as *mut u8, self.pages * crate::mem::frame::PAGE_SIZE as usize) }
	}
}

// Take down `mapped` pages of a stack reservation at `base` and return the WHOLE `len`-byte
// range to the kernel window. Shared by the failed-allocation path and by `Drop`, so the two
// cannot disagree about how much was reserved.
fn rollback(base: u64, mapped: usize, len: u64) {
	for page in 0..mapped {
		let at = base + (page as u64 + 1) * crate::mem::frame::PAGE_SIZE;
		if let Some(frame) = arch::paging::unmap_page(at) {
			// SAFETY: a page of this stack, owned by it, just unmapped from the only place it
			// was ever mapped.
			unsafe { crate::mem::frame::deallocate(frame) };
		}
	}
	// No address space: a kernel-window range is routed by address, and there is no per-space
	// pool it could belong to.
	crate::syscall::free_vrange(None, base, len);
}

impl Drop for KernelStack {
	fn drop(&mut self) {
		rollback(self.base, self.pages, (self.pages as u64 + 1) * crate::mem::frame::PAGE_SIZE);
	}
}

impl Thread {
	// Create a ready-to-run kernel thread in `process` that starts at `entry(arg)`,
	// charging one thread slot to the process's Domain unconditionally (the
	// infallible path used for the unlimited root Domain).
	// None if the kernel stack cannot be allocated, with the thread charge refunded.
	pub fn new(entry: extern "C" fn(u64), arg: u64, process: Arc<Process>) -> Option<Arc<Self>> {
		process.domain().charge_thread();
		let thread = Self::build(entry, arg, process.clone());
		if thread.is_none() {
			process.domain().uncharge_thread();
		}
		thread
	}

	// Like `new`, but enforce the process Domain's thread quota: returns None
	// (charging nothing) if the Domain is already at its thread cap.
	pub fn new_in(entry: extern "C" fn(u64), arg: u64, process: Arc<Process>) -> Option<Arc<Self>> {
		if !process.domain().try_charge_thread() {
			return None;
		}
		let thread = Self::build(entry, arg, process.clone());
		if thread.is_none() {
			process.domain().uncharge_thread();
		}
		thread
	}

	// Shared constructor tail: fabricate the initial stack and assemble the Thread.
	fn build(entry: extern "C" fn(u64), arg: u64, process: Arc<Process>) -> Option<Arc<Self>> {
		let mut stack = KernelStack::allocate()?;
		let sp = arch::context::init_thread_stack(stack.as_mut_slice(), entry, arg);
		let thread = Arc::new(Self { header: ObjectHeader::new(), tid: NEXT_TID.fetch_add(1, Ordering::Relaxed), state: AtomicU32::new(ThreadState::Ready as u32), kstack_ptr: AtomicU64::new(sp), syscall_rsp: AtomicU64::new(0), stack, started: AtomicBool::new(false), process });
		// Forward-link the thread to its process so signal delivery can reach it.
		thread.process.register_thread(&thread);
		Some(thread)
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

	// Atomically claim the right to enqueue this thread for the first time. Returns
	// true exactly once; later calls return false, so thread_start cannot enqueue
	// the same thread twice (which would corrupt the run queue).
	pub fn try_start(&self) -> bool {
		self.started.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok()
	}

	// Address of the saved-stack-pointer slot, handed to switch_context.
	//
	// The context switch writes this slot from assembly while Rust reads it atomically,
	// which is only sound because both halves of the pairing are stated and kept: the
	// assembly store is a RELEASE (`stlr` on AArch64, a `fence rw, w` on RISC-V, and the
	// architecture's own guarantee on x86-64), and every reader below uses `Acquire`. The
	// slot is the publication itself - zero means "not parked yet" and a non-zero stack
	// pointer means "this context is complete and may be resumed" - so a store that could
	// be seen before the register frame it describes would hand another core a half-written
	// thread. Any change to either side has to move the other with it.
	pub fn kstack_ptr_addr(&self) -> *mut u64 {
		self.kstack_ptr.as_ptr()
	}

	pub fn kstack_ptr_load(&self) -> u64 {
		self.kstack_ptr.load(Ordering::Acquire)
	}

	// Prepare to block: zero the saved stack pointer (the not-yet-parked marker -
	// the context switch writes the real value as its very first store) and mark
	// the thread Blocked so a waker can claim it. Runs on the thread itself right
	// before it deschedules, with interrupts masked from here to the switch.
	pub fn begin_park(&self) {
		self.kstack_ptr.store(0, Ordering::Release);
		self.set_state(ThreadState::Blocked);
	}

	// Claim a blocked thread for waking: exactly one waker wins the Blocked ->
	// Ready transition, so a thread waiting on several objects at once is enqueued
	// exactly once no matter how many of them fire together on different cores.
	pub fn try_claim_wake(&self) -> bool {
		self.state.compare_exchange(ThreadState::Blocked as u32, ThreadState::Ready as u32, Ordering::AcqRel, Ordering::Acquire).is_ok()
	}

	// Address of the parked-syscall-stack slot, stored into by usermode::enter so
	// the value follows this specific thread rather than the per-CPU block.
	pub fn syscall_rsp_addr(&self) -> *mut u64 {
		self.syscall_rsp.as_ptr()
	}

	pub fn syscall_rsp_load(&self) -> u64 {
		self.syscall_rsp.load(Ordering::Acquire)
	}

	pub fn set_syscall_rsp(&self, value: u64) {
		self.syscall_rsp.store(value, Ordering::Release);
	}
}

impl Drop for Thread {
	fn drop(&mut self) {
		// Refund this thread's slot to its process's Domain. When the process's last
		// thread drops, the Arc to the Process drops with it, tearing down the
		// process's handle table (refunding its handles) and address space.
		self.process.domain().uncharge_thread();
	}
}

impl_kernel_object!(Thread, Thread);

#[cfg(test)]
mod tests;
