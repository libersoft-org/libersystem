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
	// TWO REGISTERS THE EXCEPTION VECTOR CAN SPILL BEFORE IT TRUSTS `SP`.
	//
	// The vector's first job is to decide whether the stack it is about to save a frame on is
	// usable, and it cannot ask that question without a register to ask it with. `TPIDRRO_EL0`
	// frees the first one without touching memory; this frees two more, into per-CPU memory that
	// is statically allocated and always mapped. See the vector table in `exceptions.rs`.
	scratch: [u64; 2],
	// The range of `SP` values from which a trap frame FITS on the running thread's kernel stack:
	// valid when `sp - stack_floor <= stack_span`, unsigned - one subtract and one compare catches
	// both an overflow past the guard page and a stack pointer that has been corrupted upward.
	//
	// Zero span means "not known", which is how every context that is not a scheduled thread is
	// left: the boot stack, the secondary bring-up stacks and the idle loop. The check is skipped
	// there rather than guessed at.
	stack_floor: u64,
	stack_span: u64,
	// Top of this core's bad-stack reporting stack. Set once in `init` and never changed, so a
	// vector that has decided `SP` is unusable has somewhere to stand while it says so.
	trap_stack: u64,
	// The same pair for this core's IDLE stack - its boot stack, or its slice of the secondary
	// bring-up array. Recorded once, and restored into the live pair every time the scheduler
	// leaves a thread for the idle context.
	//
	// It exists because the alternative was zero, and zero means "do not check". That left the one
	// stack in the system with no guard page below it - `SEC_STACKS` is a plain static array, so an
	// overflow walks into the previous core's slice with no fault at all - as also being the one
	// stack nothing bounded.
	idle_floor: u64,
	idle_span: u64,
}

// The offsets the exception vectors index this block by. Exported as constants and fed to the
// assembler through `const` operands rather than written out as numbers in two places - the last
// time an offset was shared between assembly and Rust by hand it took four days to rule out as the
// cause of a fault the kernel could not report.
pub(crate) const OFF_SCRATCH: usize = core::mem::offset_of!(PerCpu, scratch);
pub(crate) const OFF_STACK_FLOOR: usize = core::mem::offset_of!(PerCpu, stack_floor);
pub(crate) const OFF_TRAP_STACK: usize = core::mem::offset_of!(PerCpu, trap_stack);
// `ldp`/`stp` read the two halves of the range in one instruction, which only works while they are
// adjacent and in this order.
const _: () = assert!(core::mem::offset_of!(PerCpu, stack_span) == OFF_STACK_FLOOR + 8, "the vector loads the floor and the span with one `ldp`");
const _: () = assert!(OFF_SCRATCH % 8 == 0 && OFF_STACK_FLOOR % 8 == 0, "`ldp`/`stp` need 8-byte-aligned offsets");

// How much stack the bad-stack reporter gets.
//
// A page looked generous - the reporter's own frame is 656 bytes - and it is not, because what runs
// underneath it is `core::fmt`, which in a debug build is many nested frames deep for a single
// formatted line. The first version reported nothing at all: seven cores reached the halt loop with
// no bytes on the wire, because the formatting ran off the bottom of the page into the neighbouring
// core's slice (this is a plain array, so that is silent rather than a fault) and the re-entry
// counted itself as another report.
//
// 16 KiB, and the number is not free-form: it is what the deepest formatted line needs with the
// whole of `core::fmt` unoptimised below it, on the one path in the kernel that must not fail
// quietly.
const TRAP_STACK_SIZE: usize = 16384;

#[repr(C, align(16))]
struct TrapStack([u8; TRAP_STACK_SIZE]);

struct TrapStacks([UnsafeCell<TrapStack>; MAX_CPUS]);
unsafe impl Sync for TrapStacks {}
static TRAP_STACKS: TrapStacks = TrapStacks([const { UnsafeCell::new(TrapStack([0; TRAP_STACK_SIZE])) }; MAX_CPUS]);

impl PerCpu {
	const fn empty() -> Self {
		Self { cpu_id: 0, lapic_id: 0, kernel_sp: 0, entry_sp_slot: 0, from_user: 0, scratch: [0; 2], stack_floor: 0, stack_span: 0, trap_stack: 0, idle_floor: 0, idle_span: 0 }
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
		// BEFORE `TPIDR_EL1` IS PUBLISHED, and that order is the whole safety argument for the
		// stack check in the vectors. They read the block through `TPIDR_EL1` and skip every check
		// when it is zero, so an exception taken before this line behaves exactly as it did before
		// the check existed - and an exception taken after it finds a block whose reporting stack
		// is already set. There is no window in which the vector sees half of this.
		(*slot).trap_stack = TRAP_STACKS.0[cpu_id].get() as u64 + TRAP_STACK_SIZE as u64;
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

// Mark (or clear) that the running core is servicing an EL0 syscall, so the
// syscall path validates user pointers.
pub fn set_from_user(from_user: bool) {
	unsafe { (*this_cpu_mut()).from_user = from_user as u64 };
}

// Tell this core which stack the thread it is about to run owns, so the exception vectors can
// refuse to save a frame onto anything else. `base` is the lowest mapped byte and `len` its size;
// `len` of zero means "not known" and disables the check.
//
// THE FLOOR IS RAISED BY A WHOLE FRAME, which is the translation this function exists to do: the
// vectors do not ask "is this pointer on the stack", they ask "can 816 bytes be written below it",
// and only this side of the port knows that number. A stack too small to hold one frame is reported
// as unknown rather than as a range nothing satisfies - which would refuse every trap that thread
// ever takes.
//
// Zero for every context the scheduler does not describe: the boot stack, the secondary bring-up
// stacks and the idle loop. A check that has to guess is worse than no check.
//
// Called with interrupts disabled, from the point in `reschedule` that publishes the incoming
// thread's other per-CPU state. An exception taken between this and `switch_context` would compare
// the OUTGOING stack pointer against the INCOMING bounds, so it may not move out of that window.
pub fn set_stack_bounds(base: u64, len: usize) {
	let (floor, span) = bounds_of(base, len);
	let cpu = this_cpu_mut();
	unsafe {
		(*cpu).stack_floor = floor;
		(*cpu).stack_span = span;
	}
}

// Translate a stack extent into the range of stack pointers a frame still fits under.
fn bounds_of(base: u64, len: usize) -> (u64, u64) {
	match (len as u64).checked_sub(super::exceptions::TRAP_FRAME_BYTES) {
		Some(span) if base != 0 => (base + super::exceptions::TRAP_FRAME_BYTES, span),
		_ => (0, 0),
	}
}

// Record the stack this core's idle and interrupt work runs on - its boot stack, or its slice of
// the secondary bring-up array. Called once per core during bring-up.
pub fn record_idle_stack(base: u64, len: usize) {
	let (floor, span) = bounds_of(base, len);
	let cpu = this_cpu_mut();
	unsafe {
		(*cpu).idle_floor = floor;
		(*cpu).idle_span = span;
		// The core IS on it right now, so make it live as well rather than waiting for the first
		// context switch away from a thread.
		(*cpu).stack_floor = floor;
		(*cpu).stack_span = span;
	}
}

// This core has left a thread for its own idle context. Restores what `record_idle_stack` recorded
// rather than clearing the bounds: "not checked" was the state that hid the failure this whole
// mechanism exists to report.
pub fn use_idle_stack() {
	let cpu = this_cpu_mut();
	unsafe {
		(*cpu).stack_floor = (*cpu).idle_floor;
		(*cpu).stack_span = (*cpu).idle_span;
	}
}

// What this core last recorded, for the bad-stack report to say what it was compared against.
pub fn stack_bounds() -> (u64, u64) {
	let cpu = this_cpu_mut();
	unsafe { ((*cpu).stack_floor, (*cpu).stack_span) }
}
