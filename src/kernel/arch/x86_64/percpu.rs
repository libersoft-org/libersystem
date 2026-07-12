// Per-CPU data.
//
// Each core keeps a pointer to its own PerCpu block in the IA32_GS_BASE MSR, so
// `this_cpu()` resolves to the running core's data with no locking. The blocks
// are allocated once at SMP bring-up, sized by the machine's real core count
// from the loader's SMP info (the heap is up long before any core - the BSP
// included - initializes its slot), and indexed by our contiguous CPU id (the
// BSP is 0).

#![allow(dead_code)]

use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

use alloc::vec::Vec;

use super::msr;

const IA32_GS_BASE: u32 = 0xc000_0101;

// Byte offsets of the syscall-scratch fields within PerCpu, used by the ring-3
// syscall entry stub to reach them through GS. Pinned by the asserts below.
pub const KERNEL_RSP_OFFSET: usize = 8;
pub const USER_RSP_OFFSET: usize = 16;
pub const USER_RIP_OFFSET: usize = 24;
pub const USER_RFLAGS_OFFSET: usize = 32;
pub const FROM_USER_OFFSET: usize = 40;
pub const TSS_RSP0_OFFSET: usize = 48;

#[repr(C)]
pub struct PerCpu {
	cpu_id: u32,
	lapic_id: u32,
	// Kernel stack pointer to resume on when a ring-3 thread enters the kernel
	// (set by usermode::enter before dropping to ring 3).
	kernel_rsp: u64,
	// Saved user registers across a ring-3 syscall (rcx/r11 are clobbered by the
	// SysV call to the dispatcher, so they are stashed here rather than on stack).
	user_rsp: u64,
	user_rip: u64,
	user_rflags: u64,
	// Non-zero while servicing a syscall that originated in ring 3, so handlers
	// know to validate user-supplied pointers.
	from_user: u64,
	// Address of this core's TSS.RSP0 slot, so the scheduler and the ring-3 entry
	// path can point the ring-3 interrupt stack at the current thread's own kernel
	// stack (per-thread RSP0 is what makes ring-3 preemption safe).
	tss_rsp0: u64,
}

impl PerCpu {
	const fn empty() -> Self {
		Self { cpu_id: 0, lapic_id: 0, kernel_rsp: 0, user_rsp: 0, user_rip: 0, user_rflags: 0, from_user: 0, tss_rsp0: 0 }
	}

	pub fn cpu_id(&self) -> u32 {
		self.cpu_id
	}

	pub fn lapic_id(&self) -> u32 {
		self.lapic_id
	}
}

const _: () = assert!(core::mem::offset_of!(PerCpu, kernel_rsp) == KERNEL_RSP_OFFSET);
const _: () = assert!(core::mem::offset_of!(PerCpu, user_rsp) == USER_RSP_OFFSET);
const _: () = assert!(core::mem::offset_of!(PerCpu, user_rip) == USER_RIP_OFFSET);
const _: () = assert!(core::mem::offset_of!(PerCpu, user_rflags) == USER_RFLAGS_OFFSET);
const _: () = assert!(core::mem::offset_of!(PerCpu, from_user) == FROM_USER_OFFSET);
const _: () = assert!(core::mem::offset_of!(PerCpu, tss_rsp0) == TSS_RSP0_OFFSET);

static PER_CPU: AtomicPtr<PerCpu> = AtomicPtr::new(ptr::null_mut());

// Allocate the per-CPU blocks for `count` cores, sized by the MP response. Called
// once by smp::init before any core - the BSP included - runs its per-CPU init.
pub fn allocate(count: usize) {
	let mut blocks: Vec<PerCpu> = Vec::with_capacity(count);
	blocks.resize_with(count, PerCpu::empty);
	let leaked: &'static mut [PerCpu] = Vec::leak(blocks);
	let prev = PER_CPU.swap(leaked.as_mut_ptr(), Ordering::Release);
	assert!(prev.is_null(), "per-CPU blocks allocated twice");
}

// Initialize the running core's per-CPU block and point GS base at it. Each core
// touches only its own slot, so concurrent calls on different cores do not race.
pub fn init(cpu_id: usize, lapic_id: u32) {
	let base = PER_CPU.load(Ordering::Acquire);
	assert!(!base.is_null(), "per-CPU blocks not allocated");
	unsafe {
		let slot = base.add(cpu_id);
		(*slot).cpu_id = cpu_id as u32;
		(*slot).lapic_id = lapic_id;
		msr::write(IA32_GS_BASE, slot as u64);
	}
}

// The per-CPU block of the running core.
pub fn this_cpu() -> &'static PerCpu {
	let base = msr::read(IA32_GS_BASE);
	unsafe { &*(base as *const PerCpu) }
}

// Set the running core's parked kernel stack pointer, the stack a ring-3 syscall
// switches onto. The scheduler restores it from the incoming thread on every
// context switch, so it always tracks the thread currently able to enter ring 3
// even when cooperative services yield to one another on the same core.
pub fn set_kernel_rsp(value: u64) {
	let base = msr::read(IA32_GS_BASE);
	unsafe { (*(base as *mut PerCpu)).kernel_rsp = value };
}

// Record where this core's TSS.RSP0 slot lives (called once per core after its
// GDT/TSS are installed), so set_rsp0 and the ring-3 entry stub can retarget it.
pub fn set_tss_rsp0_slot(addr: u64) {
	let base = msr::read(IA32_GS_BASE);
	unsafe { (*(base as *mut PerCpu)).tss_rsp0 = addr };
}

// Point this core's TSS.RSP0 at `value` - the incoming thread's parked kernel
// stack position - so a ring-3 interrupt lands on that thread's own stack. A zero
// value (a thread that never entered ring 3) leaves the slot untouched: such a
// thread cannot take a ring-3 interrupt, and its first usermode::enter sets the
// slot itself.
pub fn set_rsp0(value: u64) {
	if value == 0 {
		return;
	}
	let slot = this_cpu().tss_rsp0;
	if slot != 0 {
		// The TSS is #[repr(C, packed)], so the RSP0 slot sits at a 4-byte-aligned
		// address - write unaligned (the CPU reads TSS fields byte-wise anyway, and
		// the opaque context-switch asm that follows keeps the store ordered).
		unsafe { (slot as *mut u64).write_unaligned(value) };
	}
}

// True while the running core is servicing a syscall issued from ring 3.
pub fn in_user_syscall() -> bool {
	this_cpu().from_user != 0
}
