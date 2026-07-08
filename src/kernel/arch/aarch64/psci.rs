// aarch64 SMP bring-up via PSCI CPU_ON (M116).
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
const SEC_STACK_SIZE: u64 = 16384;

// Per-core boot stacks for the secondaries (indexed by cpu id).
#[unsafe(no_mangle)]
static mut SEC_STACKS: [[u8; SEC_STACK_SIZE as usize]; MAX_CPUS] = [[0; SEC_STACK_SIZE as usize]; MAX_CPUS];

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
	adrp    x0, .Ls_stack
	ldr     x5, [x0, :lo12:.Ls_stack]
	mov     x6, #16384
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
"#
);

// Issue a PSCI CPU_ON via HVC. Returns the PSCI status (0 = SUCCESS).
fn cpu_on(target_mpidr: u64, entry: u64, context_id: u64) -> i64 {
	let ret: i64;
	unsafe {
		core::arch::asm!(
			"hvc #0",
			inout("x0") PSCI_CPU_ON => ret,
			in("x1") target_mpidr,
			in("x2") entry,
			in("x3") context_id,
			options(nostack),
		);
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
pub fn bring_up_secondaries(cpu_count: u32) {
	if cpu_count <= 1 {
		return;
	}

	// The secondary entry is the low, physical `.text.boot` stub address; PSCI
	// releases each core there with the MMU off, and the stub adopts the boot
	// page tables the primary already built. High kernel code cannot `adrp` the
	// low symbol directly, so its address is read from a linker-filled data word.
	let entry = unsafe { aarch64_secondary_entry };
	let want = (cpu_count - 1).min((MAX_CPUS - 1) as u32);
	for cpu_id in 1..=want as u64 {
		let status = cpu_on(cpu_id, entry, cpu_id);
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
