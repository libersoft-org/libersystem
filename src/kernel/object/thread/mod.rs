// Thread kernel object.
//
// A Thread is a schedulable flow of execution with its own kernel stack. The
// scheduler keeps a saved stack pointer (kstack_ptr) for each thread that is not
// currently running; switch_context writes it on the way out and reads it on the
// way in. The thread owns its stack memory, which is freed when the last Arc to
// the thread is dropped (after it has exited and been switched away from).

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
//
// It was 16 KiB, and 16 KiB is what took the aarch64 suite down for four days. The deepest path
// this kernel has - a ring-3 spawn, `SYS_PROCESS_LOAD` into the ELF loader and the mapper - ran off
// the bottom of it in a debug build, and the guard page below caught the overflow exactly as it was
// designed to. What happens NEXT is the part nobody had measured: aarch64's exception entry opens
// with `sub sp, sp, #816` and `stp x0, x1, [sp]`, so the handler for a stack overflow cannot save
// its own frame. It faults on that store, re-enters the same vector, subtracts another 816, faults
// again - forever - walking the stack pointer down through the kernel window and writing a register
// pair everywhere it happens to land on a mapped page. Measured in a wedged guest: all eight cores
// at `__exception_vectors + 0x200`, `ESR_EL1 = 0x96000044` (a WRITE translation fault at level 0),
// `FAR_EL1` equal to `SP` to the byte, and `ELR_EL1` on that `stp`. That runaway is what corrupted
// the virtio queue register, and it is why the failure was silent: the loop never reaches a print.
//
// So the size is set from a MEASUREMENT rather than from a round number:
// `kernel.object.thread.the_deepest_kernel_path_leaves_headroom_on_its_stack` reads the high-water
// mark of the real spawn path and fails if it is within a quarter of the ceiling. Raise this and
// the test says what the headroom bought; lower it and the test says so before a guest does.
//
// The overflow is still a fault rather than silent corruption - the guard page below every stack is
// what makes it one - and making that fault SURVIVABLE is a separate piece of work, recorded in
// A guard page whose fault cannot be reported is a guard that only changes the symptom.
pub const KERNEL_STACK_SIZE: usize = 64 * 1024;

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

#[cfg(test)]
static NEXT_TID: AtomicU64 = AtomicU64::new(1);

pub struct Thread {
	header: ObjectHeader,
	// A second identity beside the header's koid, asserted on by the thread tests and asked for
	// by nothing else.
	#[cfg(test)]
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
	// THE RUN QUEUE'S LINK, so enqueueing a thread allocates nothing.
	//
	// The scheduler's queue was a `VecDeque<Arc<Thread>>` whose push ran under the per-CPU lock, and
	// a push that grows asks the heap for memory while a core is locked out of its own scheduler -
	// which is an allocation a TLB shootdown waits on. A thread is in at most one run queue at a
	// time (it is running, queued on one core, or blocked), so one link here is enough and the
	// scheduler's queue becomes pointer stores.
	//
	// Locked because the queue only ever holds `&Thread`: the field is touched exclusively under the
	// scheduler's own lock, so this one is never contended and exists to make the mutation sound
	// rather than to order anything.
	run_link: SpinLock<Option<Arc<Thread>>>,
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
	// The stack, optionally PREFERRING a node - the node of the CPU this thread is being created for.
	//
	// - the node of the CPU this thread is being created for. `None` is the ordinary spawn, which keeps
	// its previous behaviour exactly.
	//
	// M3 asks that a kernel stack created for a selected CPU prefer that CPU's node, and this used the
	// plain `frame::allocate()`, which prefers the node of the core doing the CREATING. On a
	// two-node machine a thread placed on node 1 by a creator running on node 0 therefore ran on node
	// 1 with its kernel stack in node 0's memory - every entry into the kernel a remote access, for
	// the life of the thread, and nothing in the placement path had said otherwise.
	//
	// PREFERRED, NEVER STRICT. A stack that cannot be allocated is a thread that cannot be created,
	// and refusing to create one because the requested node is full would turn a locality hint into
	// an allocation policy. `allocate_preferred` falls back through increasing firmware distance.
	pub(crate) fn allocate_on(node: Option<topology::NodeId>) -> Option<Self> {
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
			// The frame and the mapping are two failures, not one, and `filter` made them one:
			// a frame that was ALLOCATED and then could not be mapped - because the mapper ran out
			// of frames for an intermediate table - was dropped on the floor by the combinator.
			// `rollback` frees what is MAPPED, so nothing knew about it. That is the single frame
			// `a_load_that_runs_out_of_frames_anywhere_gives_back_everything` reports as kept when
			// a budget lands on the mapper's own allocation.
			let allocated = match node {
				Some(node) => crate::mem::frame::allocate_preferred(node),
				None => crate::mem::frame::allocate(),
			};
			let mapped = match allocated {
				Some(frame) if arch::paging::try_map_page(at, frame, flags).is_ok() => Some(frame),
				Some(frame) => {
					// SAFETY: ours since `allocate` returned it, mapped nowhere - the mapping is
					// exactly what failed - so nothing can reach it and it goes straight back.
					// NEVER-MAPPED: `try_map_page` is what failed, so no page table ever held it.
					unsafe { crate::mem::frame::deallocate(frame) };
					None
				}
				None => None,
			};
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

	// How deep anything has ever gone on this stack, MEASURED rather than tracked.
	//
	// `allocate` zeroes the whole range, so the lowest non-zero byte is the deepest point any call
	// has reached: everything below it has never been written. That costs a scan when somebody asks
	// and nothing at all on the hot path, which is what makes it usable as an assertion rather than
	// as a debugging session - and this kernel needed an assertion, because the number that mattered
	// (how much of a 16 KiB stack the spawn path actually used) was never once measured before the
	// overflow took the machine down.
	//
	// A LOWER BOUND, deliberately: a frame that stored only zeroes leaves no trace, so the true
	// figure can be higher. That is the right direction for the error to lean - it can say a stack
	// is too small and cannot say a stack is safe.
	#[cfg(test)]
	pub fn used_bytes(&self) -> usize {
		let start = self.base + crate::mem::frame::PAGE_SIZE;
		let len = self.pages * crate::mem::frame::PAGE_SIZE as usize;
		for offset in 0..len {
			// SAFETY: mapped by `allocate`, owned by this stack for its whole life, and read one
			// byte at a time as a plain integer - no reference into it is created.
			if unsafe { core::ptr::read_volatile((start + offset as u64) as *const u8) } != 0 {
				return len - offset;
			}
		}
		0
	}

	// The usable bytes above the guard page.
	pub fn capacity(&self) -> usize {
		self.pages * crate::mem::frame::PAGE_SIZE as usize
	}

	// The lowest MAPPED address of this stack: one page above `base`, which is the guard.
	pub fn usable_base(&self) -> u64 {
		self.base + crate::mem::frame::PAGE_SIZE
	}

	// Which node this stack's first mapped frame came from.
	//
	// The evidence for M3's third bullet: a kernel stack created for a selected CPU has to come from
	// that CPU's node, and only the PHYSICAL frame behind the stack can say whether it did.
	#[cfg(test)]
	pub fn node(&self) -> topology::Affinity {
		match arch::paging::translate(self.usable_base()) {
			Some(physical) => crate::mem::topology_node_of(physical),
			None => topology::Affinity::Unknown,
		}
	}

	// One past the highest mapped byte - what a stack pointer starts at.
	// The aarch64 secondary-core bring-up parks its idle stack by top.
	#[cfg(target_arch = "aarch64")]
	pub fn top(&self) -> u64 {
		self.usable_base() + self.capacity() as u64
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
			// The kernel stack's pages were mapped into the kernel window, which every address
			// space shares - so a stale translation to one is reachable from every process on the
			// machine until the shootdown completes.
			unsafe { crate::mem::frame::retire(&[frame]) };
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
	// Which node this thread's kernel stack came from. See `KernelStack::node`.
	#[cfg(test)]
	pub fn stack_node(&self) -> topology::Affinity {
		self.stack.node()
	}

	pub fn new(entry: extern "C" fn(u64), arg: u64, process: Arc<Process>) -> Option<Arc<Self>> {
		// THE CREATING CORE IS A NAMED CORE, and passing `None` here threw that away.
		//
		// `new_for_cpu(.., None)` means "no node to prefer", and every ordinary thread in the
		// shipping kernel went through it - so a kernel that had discovered its topology, bound every
		// logical CPU to a node and published the masks then allocated every kernel stack with no
		// preference at all. The placement mechanism existed and the product never used it, which is
		// the gap between "the test proves the mechanism" and "the kernel uses it".
		//
		// A thread created without a stated core will FIRST run on the core creating it - that is
		// what `spawn` means - so its stack belongs on that core's node. This is a PREFERENCE and not
		// a pin: `new_for_cpu` falls back to an ordinary allocation on a machine with no topology or
		// an unbound cpu, and the thread may later migrate on a wake, which moves no pages and is
		// documented as not doing so.
		Self::new_for_cpu(entry, arg, process, Some(crate::sched::current_cpu_id()))
	}

	// The same, for a thread being created to run on a NAMED cpu: its kernel stack is allocated
	// preferring that cpu's node.
	//
	// This is the production half of M0152's M3. The placement API named a core only AFTER the thread
	// existed - `prepare_with_object` built it, `start_thread_on` chose the core - so the stack was
	// already in the creating core's node and stayed there for the life of the thread. The node has to
	// be known where the frames are taken, which is here.
	pub fn new_for_cpu(entry: extern "C" fn(u64), arg: u64, process: Arc<Process>, cpu: Option<usize>) -> Option<Arc<Self>> {
		let node = match cpu.map(crate::smp::numa::cpu_node) {
			Some(topology::Affinity::Node(node)) => Some(node),
			// An unbound cpu, or a machine with no topology: there is no node to prefer, and the
			// ordinary allocation is the honest answer rather than a guess.
			_ => None,
		};
		process.domain().charge_thread();
		let thread = Self::build_on(entry, arg, process.clone(), node);
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
		Self::build_on(entry, arg, process, None)
	}

	// The same, for a thread being created FOR a particular CPU: its kernel stack prefers that CPU's
	// node. See `KernelStack::allocate_on` for why this exists and why it is a preference.
	fn build_on(entry: extern "C" fn(u64), arg: u64, process: Arc<Process>, node: Option<topology::NodeId>) -> Option<Arc<Self>> {
		let mut stack = KernelStack::allocate_on(node)?;
		let sp = arch::context::init_thread_stack(stack.as_mut_slice(), entry, arg);
		// FALLIBLY: `SYS_THREAD_CREATE` reaches this, and `build` already answers `Option`.
		let thread = crate::mem::heap::try_arc(Self {
			header: ObjectHeader::new(),
			#[cfg(test)]
			tid: NEXT_TID.fetch_add(1, Ordering::Relaxed),
			state: AtomicU32::new(ThreadState::Ready as u32),
			kstack_ptr: AtomicU64::new(sp),
			syscall_rsp: AtomicU64::new(0),
			stack,
			started: AtomicBool::new(false),
			process,
			run_link: SpinLock::new(None),
		})?;
		// Forward-link the thread to its process so signal delivery can reach it - and refuse to
		// build the thread at all if the process is already tearing down. A thread that cannot be
		// registered is a thread nothing can signal, reap or account, inside a process whose handles
		// and mappings are already gone.
		//
		// Through the lifecycle guard, which is the only way to reach the registration: the check
		// and the record are then one step against a teardown, rather than two with a `terminating`
		// flag read between them.
		let registered = match thread.process.begin_extend() {
			Some(guard) => guard.register_thread(&thread),
			None => false,
		};
		if !registered {
			return None;
		}
		Some(thread)
	}

	// The run queue's link, for the scheduler's queue and nothing else.
	pub(crate) fn run_link(&self) -> crate::sync::SpinLockGuard<'_, Option<Arc<Thread>>> {
		self.run_link.lock()
	}

	#[cfg(test)]
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
	// The high-water mark of this thread's kernel stack, and what it had to spend. Read from
	// outside the thread (the spawner keeps an `Arc`), so the measurement does not itself deepen
	// the stack it is measuring.
	#[cfg(test)]
	pub fn kstack_used(&self) -> usize {
		self.stack.used_bytes()
	}

	// Where this thread's kernel stack actually is: `(lowest mapped address, bytes)`.
	//
	// The REGION, not a rule about it. What a usable stack pointer is depends on how big a frame
	// the architecture's exception entry saves before it can do anything else, and that number
	// belongs to the port that saves it - so this hands over the extent and lets the port decide.
	pub fn kstack_region(&self) -> (u64, usize) {
		(self.stack.usable_base(), self.stack.capacity())
	}

	#[cfg(test)]
	pub fn kstack_capacity(&self) -> usize {
		self.stack.capacity()
	}

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
		// And the LIVE-THREAD counter, for a thread that never ran.
		//
		// `register_thread` increments it as the thread is built, and the scheduler decrements it
		// when a thread exits - so a thread that is created and then dropped before it ever starts
		// left the count permanently one high. `sys_thread_create` can do exactly that: it builds
		// the thread and then fails to install a handle to it on the caller's quota. The process is
		// then one short of "last thread exited" forever, and its finaliser never runs.
		//
		// Only for a thread that never started: one that ran has already been counted out on its
		// way through `sched::exit`, and counting it again would take the process below zero.
		if !self.started.load(Ordering::Acquire) && self.process.thread_exited() {
			self.process.mark_exited();
		}
	}
}

impl_kernel_object!(Thread, Thread);

#[cfg(test)]
mod tests;
