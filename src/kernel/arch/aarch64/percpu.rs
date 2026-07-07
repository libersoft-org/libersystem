// Per-CPU data (aarch64).
//
// Each core keeps a pointer to its own PerCpu block in TPIDR_EL1, so `this_cpu()`
// resolves to the running core's data with no locking. The blocks live in a small
// static pool (no heap dependency during early bring-up) indexed by our
// contiguous CPU id (the BSP is 0); `allocate` records how many the machine has.

#![allow(dead_code)]

use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

// Maximum cores the static pool supports (QEMU virt bring-up is small).
const MAX_CPUS: usize = 8;

#[repr(C)]
pub struct PerCpu {
	cpu_id: u32,
	// The core's MPIDR affinity (kept named `lapic_id` for the portable contract).
	lapic_id: u32,
	// Kernel stack pointer to resume on when an EL0 thread enters the kernel (set
	// by the scheduler / usermode entry once EL0 preemption lands).
	kernel_sp: u64,
	// Address of the slot holding this core's EL0-entry kernel stack, the aarch64
	// analogue of x86's TSS.RSP0 slot.
	entry_sp_slot: u64,
	// Non-zero while servicing a syscall that originated at EL0.
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

// The static per-CPU pool. UnsafeCell because each core writes only its own slot.
struct Pool([UnsafeCell<PerCpu>; MAX_CPUS]);
unsafe impl Sync for Pool {}

static POOL: Pool = Pool([const { UnsafeCell::new(PerCpu::empty()) }; MAX_CPUS]);
static CPU_COUNT: AtomicUsize = AtomicUsize::new(0);

// Record how many cores the machine has (the pool is static, so this only bounds
// the valid slots). Called once before any core initializes its slot.
pub fn allocate(count: usize) {
	assert!(count <= MAX_CPUS, "per-CPU pool too small");
	CPU_COUNT.store(count, Ordering::Release);
}

// Initialize the running core's per-CPU block and point TPIDR_EL1 at it. Each
// core touches only its own slot, so concurrent calls on different cores do not
// race.
pub fn init(cpu_id: usize, mpidr: u32) {
	assert!(cpu_id < CPU_COUNT.load(Ordering::Acquire), "per-CPU slot out of range");
	let slot = POOL.0[cpu_id].get();
	unsafe {
		(*slot).cpu_id = cpu_id as u32;
		(*slot).lapic_id = mpidr;
		asm!("msr tpidr_el1, {}", in(reg) slot as u64, options(nomem, nostack, preserves_flags));
	}
}

// The per-CPU block of the running core (from TPIDR_EL1).
pub fn this_cpu() -> &'static PerCpu {
	let base: u64;
	unsafe {
		asm!("mrs {}, tpidr_el1", out(reg) base, options(nomem, nostack, preserves_flags));
		&*(base as *const PerCpu)
	}
}

fn this_cpu_mut() -> *mut PerCpu {
	let base: u64;
	unsafe {
		asm!("mrs {}, tpidr_el1", out(reg) base, options(nomem, nostack, preserves_flags));
	}
	base as *mut PerCpu
}

// Set the running core's parked kernel stack pointer, the stack an EL0 entry
// switches onto. The scheduler restores it from the incoming thread on every
// context switch.
pub fn set_kernel_rsp(value: u64) {
	unsafe { (*this_cpu_mut()).kernel_sp = value };
}

// Record where this core's EL0-entry kernel stack slot lives (the aarch64
// analogue of the x86 TSS.RSP0 slot).
pub fn set_tss_rsp0_slot(addr: u64) {
	unsafe { (*this_cpu_mut()).entry_sp_slot = addr };
}

// Point this core's EL0-entry kernel stack at `value` - the incoming thread's
// parked kernel stack position. A zero value (a thread that never entered EL0)
// leaves the slot untouched.
pub fn set_rsp0(value: u64) {
	if value == 0 {
		return;
	}
	let slot = unsafe { (*this_cpu_mut()).entry_sp_slot };
	if slot != 0 {
		unsafe { (slot as *mut u64).write(value) };
	}
}

// True while the running core is servicing a syscall issued from EL0.
pub fn in_user_syscall() -> bool {
	unsafe { (*this_cpu_mut()).from_user != 0 }
}
