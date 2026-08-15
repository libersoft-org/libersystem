// aarch64 SMP bring-up via PSCI CPU_ON.
//
// QEMU's `virt` machine implements PSCI (Power State Coordination Interface) and,
// for the default non-secure, non-virtualized configuration, expects the call via
// HVC from EL1 (QEMU emulates the PSCI service even without a real EL2). Secondary
// cores reset held in a PSCI-parked state; CPU_ON releases one at a physical entry
// point with the MMU off. `aarch64_secondary_start` is a low, position-independent
// stub (like the primary `_start`): it turns the MMU on with the boot core's page
// tables (TTBR0 = low identity, TTBR1 = the higher half, built once by the primary
// at `__boot_tables`), then branches into the higher half.

use core::arch::global_asm;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// PSCI CPU_ON, SMC64 calling convention (function id 0xC400_0003).
const PSCI_CPU_ON: u64 = 0xC400_0003;

// Matches the per-CPU pool size in `percpu`.
const MAX_CPUS: usize = 8;
// Per-core idle/bring-up stack for the secondaries.
//
// 16 KiB, and now MEASURED rather than assumed: with the alignment defect below fixed, the deepest
// any core's idle context has ever gone is 4203 bytes - 26% of this - and the other seven sit at
// 2568. `the_deepest_kernel_path_leaves_headroom_on_its_stack` reports every core's high-water from
// the zeroed `.bss` and fails if any of them passes three quarters.
//
// It was briefly raised to 64 KiB on the theory that the idle path - the scheduler plus the reaping
// of exited threads and processes, which walks `Arc::drop_slow` through the object graph in a debug
// build - had outgrown it. THE MEASUREMENT REFUTED THAT, and the size came back down. What was
// actually wrong was the alignment, one declaration below.
//
// What remains true and is not fixed here: there is no guard page between these slices, so an
// overflow walks into the previous core's memory with no fault at all. The exception vectors now
// catch it at the next trap, which is a catch and not a stop - see P02M0133.
const SEC_STACK_SIZE: u64 = 16384;

// ALIGNED TO 16, WHICH IT WAS NOT, AND THAT WAS THE DEFECT THIS MILESTONE IS NAMED FOR.
//
// This was `[[u8; SEC_STACK_SIZE]; MAX_CPUS]`. The element type is `u8`, so the whole array has an
// alignment of ONE, and the linker put it at `0xffff0000404436a9`. The bring-up stub computes each
// core's stack top as `SEC_STACKS + cpu_id * SEC_STACK_SIZE + SEC_STACK_SIZE`, so every secondary
// core has been running its entire life with `SP` congruent to 9 modulo 16.
//
// AAPCS64 requires `SP` to be 16-byte aligned at all times and the compiler generates code that
// assumes it. Most of what a misaligned stack does is invisible, because frame-relative accesses are
// self-consistent - which is exactly why this survived every test the kernel has: the arithmetic
// works, the values are right, and nothing complains. What does NOT survive is any instruction that
// requires natural alignment regardless of `SCTLR_EL1.A`, and `ldxr`/`stxr` - the exclusive pair
// under every atomic - is one of them. An atomic on a stack slot raises an alignment fault, and the
// fault arrives with a stack pointer no handler can save a frame on.
//
// The evidence was in the first measurement ever taken and went unread for four days: EVERY stack
// pointer in every gdb capture and every bad-stack report ends in 9. `...95b9`, `...9ac9`, `...5349`,
// `...0109`. That is not corruption with a pattern, it is one misalignment, present from boot.
//
// The boot stack never had this because the linker script aligns it explicitly; these were a Rust
// static, where the alignment is the element type's and nobody said otherwise.
#[repr(C, align(16))]
struct SecStack([u8; SEC_STACK_SIZE as usize]);

#[unsafe(no_mangle)]
static mut SEC_STACKS: [SecStack; MAX_CPUS] = [const { SecStack([0; SEC_STACK_SIZE as usize]) }; MAX_CPUS];

// Count of secondaries that have come online, and their reported MPIDRs.
static SMP_ONLINE: AtomicU32 = AtomicU32::new(0);
static SEC_MPIDR: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

global_asm!(
	r#"
.section .data.boot, "a"
.balign 8
.Ls_main:  .quad aarch64_secondary_main
.Ls_stack: .quad SEC_STACKS

.section .text.boot, "ax"
.global aarch64_secondary_start
aarch64_secondary_start:
	// x0 = context_id = cpu id (passed by PSCI CPU_ON). MMU is off; PC is the low
	// physical entry. Adopt the boot page tables the primary already built.
	mov     x19, x0
	adrp    x20, __boot_tables
	add     x20, x20, :lo12:__boot_tables
	add     x21, x20, #4096         // L0_LOW  (TTBR0, low identity)
	add     x22, x20, #8192         // L0_HIGH (TTBR1, higher half)
	mov     x0, #0xFF00
	msr     mair_el1, x0
	mrs     x0, id_aa64mmfr0_el1
	and     x0, x0, #0x7
	lsl     x0, x0, #32
	movz    x1, #0x3510
	movk    x1, #0xB510, lsl #16
	orr     x0, x0, x1
	msr     tcr_el1, x0
	msr     ttbr0_el1, x21
	msr     ttbr1_el1, x22
	dsb     sy
	tlbi    vmalle1
	dsb     sy
	isb
	mrs     x0, sctlr_el1
	orr     x0, x0, #1             // enable the MMU (SCTLR_EL1.M)
	msr     sctlr_el1, x0
	isb
	// Per-core higher-half stack: SEC_STACKS[cpu_id] top.
	//
	// THE SIZE COMES FROM `SEC_STACK_SIZE`, not from a number typed here. It was `#16384` written
	// out, beside a Rust constant that said the same thing - so changing the constant would have
	// given every secondary a stack pointer inside core 0's slice, with all seven overlapping, and
	// the first symptom would have been another round of memory corruption with no obvious cause.
	// This is the shared-number hazard an audit of this kernel named about trap-frame offsets, in a
	// second place and one edit away from firing.
	adrp    x0, .Ls_stack
	ldr     x5, [x0, :lo12:.Ls_stack]
	mov     x6, #{STACK}
	madd    x5, x19, x6, x5
	add     x5, x5, x6
	mov     sp, x5
	// Branch into the higher half: aarch64_secondary_main(cpu_id).
	adrp    x0, .Ls_main
	ldr     x4, [x0, :lo12:.Ls_main]
	mov     x0, x19
	br      x4
0:
	wfe
	b       0b

.section .data, "aw"
.balign 8
.global aarch64_secondary_entry
aarch64_secondary_entry:
	.quad aarch64_secondary_start
"#,
	STACK = const SEC_STACK_SIZE,
);

// Which conduit, if any, answers a PSCI call on this machine - what the loader recorded in the
// hand-off. `PSCI_NONE` for a kernel that was dropped from EL2 with nothing left below it.
//
// READ FROM THE HAND-OFF DIRECTLY, not from the published `BootInfo`, for the reason
// `loader_module_at` gives one file over: the secondaries are brought up BEFORE the kernel publishes
// one, and reading it there panics with "boot info read before it was published" - which is what the
// first version of this did.
//
// No hand-off (QEMU's `-kernel`, which enters with a raw DTB pointer) means the guest started at EL1
// under QEMU's own PSCI, which is HVC. That is the path every aarch64 test in this tree takes.
pub(crate) fn conduit(arg: u64) -> u32 {
	if arg == 0 {
		return bootproto::PSCI_NONE;
	}
	let magic = unsafe { core::ptr::read_volatile(super::paging::phys_to_virt(arg) as *const u64) };
	if magic != bootproto::MAGIC {
		// A RAW DEVICE TREE, WHICH IS THE OTHER WAY THIS KERNEL BOOTS - and it answered `PSCI_HVC`
		// unconditionally.
		//
		// The loader learned to read `/psci/method` and the FADT, so a UEFI boot gets the conduit
		// the platform states. Booted directly by firmware with a DTB in `x0` there is no
		// `BootInfo`, and this fell back to the same guess the loader had just stopped making: the
		// identical machine, with `method = "smc"` in its own device tree, executed `hvc #0`.
		//
		// The tree is right there and `Fdt::psci_conduit` already reads it - the same function the
		// loader calls, so the two boot paths cannot answer differently about one platform. A tree
		// that states nothing gets `PSCI_NONE`, which boots single-core with a reason rather than
		// faulting on the first secondary.
		return match fdt::Fdt::new(arg, super::paging::phys_to_virt).psci_conduit() {
			Some(fdt::PsciConduit::Smc) => bootproto::PSCI_SMC,
			Some(fdt::PsciConduit::Hvc) => bootproto::PSCI_HVC,
			None => bootproto::PSCI_NONE,
		};
	}
	let bi = super::paging::phys_to_virt(arg) as *const bootproto::BootInfo;
	unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*bi).psci_conduit)) }
}

// Issue a PSCI CPU_ON on the conduit the loader said exists. Returns the PSCI status (0 = SUCCESS).
//
// THE CONDUIT IS NOT A PROPERTY OF THE ARCHITECTURE. It is a fact about whatever runs below this
// kernel, and this issued a bare `hvc #0` on the assumption that something always does. Under
// `virtualization=on` the guest owns EL2, so the `hvc` landed in the firmware's EL2 vectors - which
// outlive `ExitBootServices` - and the boot died there with fifteen lines of kernel output behind it
// and no explanation. `BootInfo::psci_conduit` is the loader saying what it left behind.
fn cpu_on(conduit: u32, target_mpidr: u64, entry: u64, context_id: u64) -> i64 {
	let ret: i64;
	// TWO INSTRUCTIONS, ONE CALLING CONVENTION. The SMC arm used to be a printed refusal, because
	// nothing in this tree could set `PSCI_SMC` - the loader inferred `PSCI_HVC` from the exception
	// level. Now that the loader reads the conduit off the platform, `PSCI_SMC` is a real answer on
	// most server-class AArch64, where EL2 belongs to a hypervisor and PSCI lives in EL3 firmware.
	// The register conventions are identical, so the difference is the instruction and nothing else.
	unsafe {
		if conduit == bootproto::PSCI_SMC {
			core::arch::asm!(
				"smc #0",
				inout("x0") PSCI_CPU_ON => ret,
				in("x1") target_mpidr,
				in("x2") entry,
				in("x3") context_id,
				options(nostack),
			);
		} else {
			core::arch::asm!(
				"hvc #0",
				inout("x0") PSCI_CPU_ON => ret,
				in("x1") target_mpidr,
				in("x2") entry,
				in("x3") context_id,
				options(nostack),
			);
		}
	}
	ret
}

// First Rust code a secondary core runs (MMU on, stack + vectors set). It brings
// up its per-CPU block and local GIC/timer, records itself online, then idles.
#[unsafe(no_mangle)]
extern "C" fn aarch64_secondary_main(cpu_id: u64) -> ! {
	// Install the shared EL1 exception vectors on this core (VBAR_EL1 resets to 0).
	super::exceptions::init_vectors();
	// Enable FP/SIMD on this core (CPACR_EL1 resets with FP trapped).
	super::enable_fp();
	let mpidr: u64;
	unsafe {
		core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack, preserves_flags));
	}
	super::percpu::init(cpu_id as usize, mpidr as u32);
	// OFF THE BRING-UP SLICE AND ONTO A GUARDED STACK, before anything deep runs on it.
	//
	// `SEC_STACKS` is a static array with NO GUARD PAGES between its slices, so a core that runs off
	// the bottom of its own walks into the previous core's - mapped memory, no fault, silent, and
	// what it overwrites there is another core's saved state. The exception vectors now REPORT a
	// stack pointer outside the recorded bounds, but that is a catch at the next trap rather than a
	// stop at the store that caused it; a guard page is the stop.
	//
	// Threads have had one since they existed, from `KernelStack::allocate` - a kernel virtual range
	// with its base page left unmapped - and this is the same allocation for the same reason. The
	// bring-up slice is only what this core stands on until the frame allocator can give it
	// something better, which by here it can: the BSP wakes secondaries after memory is up.
	//
	// The stack is never freed, so the handle is forgotten deliberately rather than leaked by
	// accident: an idle context lives as long as its core does.
	if let Some(stack) = crate::object::thread::KernelStack::allocate() {
		let top = stack.top();
		super::percpu::record_idle_stack(stack.usable_base(), stack.capacity());
		core::mem::forget(stack);
		unsafe { super::context::run_on_stack(top, secondary_idle, cpu_id) }
	}
	// NO ROOM FOR ONE, WHICH IS NOT A REASON TO REFUSE THE CORE. It keeps the unguarded slice and
	// says so, and the vector check still bounds it - the same trade the rest of this kernel makes
	// when an allocation fails on a path that has a working fallback.
	crate::serial_println!("aarch64: core {cpu_id} has no guarded idle stack (out of memory); it keeps its bring-up slice");
	unsafe {
		let base = (&raw const SEC_STACKS[cpu_id as usize]) as u64;
		super::percpu::record_idle_stack(base, SEC_STACK_SIZE as usize);
	}
	secondary_idle(cpu_id)
}

// Everything a secondary does after it is standing on the stack it will keep. Split out so the
// switch above can hand it a new stack and never return.
extern "C" fn secondary_idle(cpu_id: u64) -> ! {
	let mpidr: u64;
	unsafe {
		core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack, preserves_flags));
	}
	super::gic::init_secondary();
	SEC_MPIDR[cpu_id as usize].store(mpidr, Ordering::Relaxed);
	SMP_ONLINE.fetch_add(1, Ordering::Release);
	// Also count this core in the portable online tally the scheduler and tests read.
	crate::smp::mark_online();
	super::enable_interrupts();
	// The BSP brings the scheduler up (allocate + init) after waking us: spin until it
	// has, then park in the scheduler idle loop so threads can be scheduled onto this
	// core (and the wake IPI bounces us out of the halt to pick them up). Before it is
	// ready the timer IRQ is a no-op (on_timer_preempt is gated on the same flag).
	while !crate::sched::is_initialized() {
		super::idle_halt();
	}
	crate::sched::cpu_idle_loop()
}

// Wake every secondary core (cpu ids 1..cpu_count) via PSCI CPU_ON and wait for
// them to report in. On QEMU virt the MPIDR affinity of cpu N is simply N.
pub fn bring_up_secondaries(cpu_count: u32, boot_arg: u64) {
	if cpu_count <= 1 {
		return;
	}
	// NO CONDUIT, NO SECONDARIES - and a line saying so, rather than a fault into somebody else's
	// exception vectors. A single-core boot is a working system; a machine that dies bringing up its
	// second core is not, and the difference used to be one instruction the kernel had no way to know
	// was unanswered.
	let conduit = conduit(boot_arg);
	match conduit {
		bootproto::PSCI_HVC | bootproto::PSCI_SMC => {}
		_ => {
			crate::serial_println!("aarch64: SMP - no PSCI conduit below this kernel; running on one core");
			return;
		}
	}

	// The secondary entry is the low, physical `.text.boot` stub address; PSCI
	// releases each core there with the MMU off, and the stub adopts the boot
	// page tables the primary already built. High kernel code cannot `adrp` the
	// low symbol directly, so its address is read from a linker-filled data word.
	let entry = unsafe { aarch64_secondary_entry };
	let want = (cpu_count - 1).min((MAX_CPUS - 1) as u32);
	for cpu_id in 1..=want as u64 {
		let status = cpu_on(conduit, cpu_id, entry, cpu_id);
		if status != 0 {
			crate::serial_println!("aarch64: CPU_ON core {cpu_id} failed (PSCI {status})");
		}
	}

	// Wait for the secondaries to come online.
	let mut spins: u64 = 0;
	while SMP_ONLINE.load(Ordering::Acquire) < want && spins < 2_000_000_000 {
		core::hint::spin_loop();
		spins += 1;
	}

	let online = SMP_ONLINE.load(Ordering::Acquire);
	crate::serial_println!("aarch64: SMP - {}/{} secondary cores online", online, want);
	for cpu_id in 1..=want as usize {
		let mpidr = SEC_MPIDR[cpu_id].load(Ordering::Relaxed);
		crate::serial_println!("aarch64:   core {} up (mpidr={:#x})", cpu_id, mpidr & 0xff_ffff);
	}
}

// Declared so the assembly entry point is referenced from Rust.
unsafe extern "C" {
	// The low physical address of the secondary boot stub, filled in by the linker.
	static aarch64_secondary_entry: u64;
}

// How deep anything has ever gone on secondary `cpu`'s idle stack, MEASURED rather than assumed.
//
// `SEC_STACKS` is in `.bss` and therefore zeroed before any core runs, so the lowest non-zero byte
// is the deepest point reached - the same technique the per-thread stacks use, and for the same
// reason: this size had never been checked against anything, and when it turned out to be too small
// the failure was not a fault but another core's memory quietly changing underneath it.
//
// A lower bound, deliberately: a frame that stored only zeroes leaves no trace. That is the right
// direction for the error to lean - it can say a stack is too small and cannot say one is safe.
pub fn secondary_stack_used(cpu: usize) -> usize {
	if cpu >= MAX_CPUS {
		return 0;
	}
	// SAFETY: taking the ADDRESS of the slice, not a reference into it - every read below goes
	// through the raw pointer one byte at a time, so no `&` to memory other cores are using is ever
	// created.
	let base = unsafe { (&raw const SEC_STACKS[cpu]) as *const u8 };
	for offset in 0..SEC_STACK_SIZE as usize {
		// SAFETY: inside a static array that lives for the whole life of the kernel, read one byte
		// at a time as a plain integer.
		if unsafe { core::ptr::read_volatile(base.add(offset)) } != 0 {
			return SEC_STACK_SIZE as usize - offset;
		}
	}
	0
}

pub fn secondary_stack_capacity() -> usize {
	SEC_STACK_SIZE as usize
}
