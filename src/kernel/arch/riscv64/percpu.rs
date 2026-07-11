// Per-CPU data (riscv64).
//
// Each hart keeps a pointer to its own PerCpu block in the `tp` register, so
// `this_cpu()` resolves to the running hart's data with no locking (a kernel has no
// thread-local storage, so `tp` is free for this). The blocks are heap-allocated once
// at SMP bring-up, sized by the machine's real hart count (the heap is up before any
// hart initializes its slot), and indexed by our contiguous CPU id (the boot hart is
// 0) - no compile-time hart cap.

#![allow(dead_code)]

use core::arch::asm;
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use alloc::vec::Vec;

#[repr(C)]
pub struct PerCpu {
	cpu_id: u32,
	// The hart id (kept named `lapic_id` for the portable contract).
	lapic_id: u32,
	// Kernel stack pointer to resume on when a U-mode thread enters the kernel.
	kernel_sp: u64,
	// Address of the slot holding this hart's U-mode-entry kernel stack (the riscv
	// analogue of x86's TSS.RSP0 slot).
	entry_sp_slot: u64,
	// Non-zero while servicing a syscall that originated at U-mode.
	from_user: u64,
}

impl PerCpu {
	const fn empty() -> Self {
		Self { cpu_id: 0, lapic_id: 0, kernel_sp: 0, entry_sp_slot: 0, from_user: 0 }
	}

	pub fn cpu_id(&self) -> u32 {
		self.cpu_id
	}

	pub fn lapic_id(&self) -> u32 {
		self.lapic_id
	}
}

// The heap-allocated per-CPU blocks (a leaked slice) and the machine's hart count.
static PER_CPU: AtomicPtr<PerCpu> = AtomicPtr::new(ptr::null_mut());
static CPU_COUNT: AtomicUsize = AtomicUsize::new(0);

// Allocate the per-CPU blocks for `count` harts, sized by the machine. Called once by
// the BSP before any hart initializes its slot (the heap is up by then).
pub fn allocate(count: usize) {
	let mut blocks: Vec<PerCpu> = Vec::with_capacity(count);
	blocks.resize_with(count, PerCpu::empty);
	let leaked: &'static mut [PerCpu] = Vec::leak(blocks);
	let prev = PER_CPU.swap(leaked.as_mut_ptr(), Ordering::Release);
	assert!(prev.is_null(), "per-CPU blocks allocated twice");
	CPU_COUNT.store(count, Ordering::Release);
}

// Initialize the running hart's per-CPU block and point `tp` at it. Each hart touches
// only its own slot, so concurrent calls on different harts do not race.
pub fn init(cpu_id: usize, hartid: u32) {
	assert!(cpu_id < CPU_COUNT.load(Ordering::Acquire), "per-CPU slot out of range");
	let base = PER_CPU.load(Ordering::Acquire);
	assert!(!base.is_null(), "per-CPU blocks not allocated");
	unsafe {
		let slot = base.add(cpu_id);
		(*slot).cpu_id = cpu_id as u32;
		(*slot).lapic_id = hartid;
		asm!("mv tp, {}", in(reg) slot as u64, options(nomem, nostack, preserves_flags));
	}
}

// The per-CPU block of the running hart (from `tp`).
pub fn this_cpu() -> &'static PerCpu {
	let base: u64;
	unsafe {
		asm!("mv {}, tp", out(reg) base, options(nomem, nostack, preserves_flags));
		&*(base as *const PerCpu)
	}
}

fn this_cpu_mut() -> *mut PerCpu {
	let base: u64;
	unsafe {
		asm!("mv {}, tp", out(reg) base, options(nomem, nostack, preserves_flags));
	}
	base as *mut PerCpu
}

// Set the running hart's parked kernel stack pointer, the stack a U-mode entry
// switches onto. The scheduler restores it from the incoming thread on every context
// switch.
pub fn set_kernel_rsp(value: u64) {
	unsafe { (*this_cpu_mut()).kernel_sp = value };
}

// Record where this hart's U-mode-entry kernel stack slot lives (the riscv analogue
// of the x86 TSS.RSP0 slot).
pub fn set_tss_rsp0_slot(addr: u64) {
	unsafe { (*this_cpu_mut()).entry_sp_slot = addr };
}

// Point this hart's U-mode-entry kernel stack at `value` (the incoming thread's parked
// kernel stack position). A zero value leaves the slot untouched.
pub fn set_rsp0(value: u64) {
	if value == 0 {
		return;
	}
	let slot = unsafe { (*this_cpu_mut()).entry_sp_slot };
	if slot != 0 {
		unsafe { (slot as *mut u64).write(value) };
	}
}

// True while the running hart is servicing a syscall issued from U-mode.
pub fn in_user_syscall() -> bool {
	unsafe { (*this_cpu_mut()).from_user != 0 }
}

// Mark (or clear) that the running hart is servicing a U-mode syscall, so the syscall
// path validates user pointers.
pub fn set_from_user(from_user: bool) {
	unsafe { (*this_cpu_mut()).from_user = from_user as u64 };
}
