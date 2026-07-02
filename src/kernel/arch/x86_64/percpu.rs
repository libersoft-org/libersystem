// Per-CPU data.
//
// Each core keeps a pointer to its own PerCpu block in the IA32_GS_BASE MSR, so
// `this_cpu()` resolves to the running core's data with no locking. The blocks
// live in a fixed static array indexed by our contiguous CPU id (the BSP is 0).

#![allow(dead_code)]

use core::ptr::addr_of_mut;

use super::msr;

const IA32_GS_BASE: u32 = 0xc000_0101;

// Upper bound on supported cores; sizes the static per-CPU, GDT/TSS and scheduler
// arrays, which must exist before any allocator runs (the equivalent of Linux's
// NR_CPUS). The cost is a few hundred bytes of static state per core (~300 KiB at
// 1024), so the bound is deliberately server-scale - large boxes reach 512+ logical
// cores today. Cores beyond it stay parked, loudly reported by smp::init; raise
// this freely if such a machine ever appears.
pub const MAX_CPUS: usize = 1024;

// Byte offsets of the syscall-scratch fields within PerCpu, used by the ring-3
// syscall entry stub to reach them through GS. Pinned by the asserts below.
pub const KERNEL_RSP_OFFSET: usize = 8;
pub const USER_RSP_OFFSET: usize = 16;
pub const USER_RIP_OFFSET: usize = 24;
pub const USER_RFLAGS_OFFSET: usize = 32;
pub const FROM_USER_OFFSET: usize = 40;

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
}

impl PerCpu {
	const fn empty() -> Self {
		Self { cpu_id: 0, lapic_id: 0, kernel_rsp: 0, user_rsp: 0, user_rip: 0, user_rflags: 0, from_user: 0 }
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

static mut PER_CPU: [PerCpu; MAX_CPUS] = [const { PerCpu::empty() }; MAX_CPUS];

// Initialize the running core's per-CPU block and point GS base at it. Each core
// touches only its own slot, so concurrent calls on different cores do not race.
pub fn init(cpu_id: usize, lapic_id: u32) {
	unsafe {
		let slot = addr_of_mut!(PER_CPU).cast::<PerCpu>().add(cpu_id);
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

// True while the running core is servicing a syscall issued from ring 3.
pub fn in_user_syscall() -> bool {
	this_cpu().from_user != 0
}
