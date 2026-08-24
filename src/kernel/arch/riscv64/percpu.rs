// Per-CPU data (riscv64).
//
// Each hart keeps a pointer to its own PerCpu block in the `tp` register, so
// `this_cpu()` resolves to the running hart's data with no locking (a kernel has no
// thread-local storage, so `tp` is free for this). The blocks are heap-allocated once
// at SMP bring-up, sized by the machine's real hart count (the heap is up before any
// hart initializes its slot), and indexed by our contiguous CPU id (the boot hart is
// 0) - no compile-time hart cap.

use core::arch::asm;
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use alloc::vec::Vec;

#[repr(C)]
pub struct PerCpu {
	cpu_id: u32,
	// The hart id (kept named `lapic_id` for the portable contract). A `u64` because that is what
	// the SBI ABI carries: an `unsigned long`, which on rv64 is 64 bits.
	lapic_id: u64,
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

	pub fn lapic_id(&self) -> u64 {
		self.lapic_id
	}
}

// The heap-allocated per-CPU blocks (a leaked slice) and the machine's hart count.
static PER_CPU: AtomicPtr<PerCpu> = AtomicPtr::new(ptr::null_mut());
static CPU_COUNT: AtomicUsize = AtomicUsize::new(0);

// Allocate the per-CPU blocks for `count` harts, sized by the machine. Called once by
// the BSP before any hart initializes its slot (the heap is up by then).
pub fn allocate(count: usize) {
	// ALLOC-OK: boot, one block per core before any process runs
	let mut blocks: Vec<PerCpu> = Vec::with_capacity(count);
	blocks.resize_with(count, PerCpu::empty);
	let leaked: &'static mut [PerCpu] = Vec::leak(blocks);
	let prev = PER_CPU.swap(leaked.as_mut_ptr(), Ordering::Release);
	assert!(prev.is_null(), "per-CPU blocks allocated twice");
	CPU_COUNT.store(count, Ordering::Release);
}

// Initialize the running hart's per-CPU block and point `tp` at it. Each hart touches
// only its own slot, so concurrent calls on different harts do not race.
pub fn init(cpu_id: usize, hartid: u64) {
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

// `tp` IS ESTABLISHED BY THE TRAP PATH, AND THIS IS THE ASSERTION THAT IT WAS (KERN-ARCH-001).
//
// U-mode may put any value in `tp` - it is an ordinary register there - and `__trap_entry` used to
// save the user's `x4` into the frame and call Rust with it still in the register. So every reader
// below dereferenced an address the USER chose, and `set_from_user` WROTE through it on the syscall
// path: an arbitrary S-mode write from ring 3, and the most serious thing in the audit set.
//
// `__trap_entry` now loads the kernel's own pointer immediately after saving the user's, from a
// word parked eight bytes below the trap sp - written by `riscv64_enter_umode` for the first
// excursion and by `__trap_return` for every one after it, each time by the hart that is about to
// return, so a thread resumed on a different hart gets THAT hart's block. The user's value goes
// back from the frame on the way out, so U-mode keeps its own `tp` and never sees the kernel's.
//
// The check below stays, and its job has changed with the fix: it is no longer a bound on what ring
// 3 can select but a statement that the establishment happened. A `tp` that is not exactly a slot
// base means the trap path did not run, or ran and did not park the right word - a kernel defect,
// caught here rather than dereferenced.
fn trusted_tp() -> *mut PerCpu {
	let raw: u64;
	unsafe {
		asm!("mv {}, tp", out(reg) raw, options(nomem, nostack, preserves_flags));
	}
	let base = PER_CPU.load(Ordering::Acquire);
	let count = CPU_COUNT.load(Ordering::Acquire);
	if base.is_null() || count == 0 {
		panic!("per-CPU access before the blocks were allocated");
	}
	// Exactly a slot base: an offset into the middle of a block would let a user choose which FIELD
	// a write lands on, which is most of the original problem back again.
	let offset = (raw as usize).wrapping_sub(base as usize);
	let stride = core::mem::size_of::<PerCpu>();
	if raw as usize % core::mem::align_of::<PerCpu>() != 0 || offset % stride != 0 || offset / stride >= count {
		panic!("tp does not name a per-CPU block: ring 3 may have chosen it");
	}
	raw as *mut PerCpu
}

// The per-CPU block of the running hart (from `tp`).
pub fn this_cpu() -> &'static PerCpu {
	unsafe { &*trusted_tp() }
}

fn this_cpu_mut() -> *mut PerCpu {
	trusted_tp()
}

// Set the running hart's parked kernel stack pointer, the stack a U-mode entry
// switches onto. The scheduler restores it from the incoming thread on every context
// switch.
pub fn set_kernel_rsp(value: u64) {
	unsafe { (*this_cpu_mut()).kernel_sp = value };
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

// The portable hook the scheduler calls with the incoming thread's kernel stack extent. aarch64
// uses it to bound what its exception vectors will save a frame onto, after a stack pointer that
// was not on any stack turned every fault into an unbounded, silent runaway (P02M0133). Nothing
// here does that yet, so this records nothing rather than pretending to check.
//
// Worth keeping in mind before this stays empty by default: the same failure is possible on any
// port whose exception entry writes to the stack before it can validate it. What makes it cheap to
// leave alone here is that these two have not shown it, not that they cannot.
pub fn set_stack_bounds(_base: u64, _len: usize) {}

pub fn use_idle_stack() {}

// No bounds are recorded here, so nothing can be checked against them. `(0, 0)` is the agreed
// "not known" answer every caller already handles by skipping the check.
pub fn stack_bounds() -> (u64, u64) {
	(0, 0)
}
