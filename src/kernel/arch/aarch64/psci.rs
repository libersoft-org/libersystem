// aarch64 SMP bring-up via PSCI CPU_ON (M116).
//
// QEMU's `virt` machine implements PSCI (Power State Coordination Interface) and,
// for the default non-secure, non-virtualized configuration, expects the call via
// HVC from EL1 (QEMU emulates the PSCI service even without a real EL2). Secondary
// cores reset held in a PSCI-parked state; CPU_ON releases one at a physical entry
// point with the MMU off. `aarch64_secondary_start` re-enables the MMU with the
// boot core's page tables, sets a per-core stack + vectors, and calls into Rust.

use core::arch::global_asm;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// PSCI CPU_ON, SMC64 calling convention (function id 0xC400_0003).
const PSCI_CPU_ON: u64 = 0xC400_0003;

// Matches the per-CPU pool size in `percpu`.
const MAX_CPUS: usize = 8;
const SEC_STACK_SIZE: u64 = 16384;

// The boot core's MMU configuration, captured for the secondaries to adopt (their
// system registers reset to zero, so they cannot read the boot core's).
#[unsafe(no_mangle)]
static SEC_MAIR: AtomicU64 = AtomicU64::new(0);
#[unsafe(no_mangle)]
static SEC_TCR: AtomicU64 = AtomicU64::new(0);
#[unsafe(no_mangle)]
static SEC_TTBR0: AtomicU64 = AtomicU64::new(0);
#[unsafe(no_mangle)]
static SEC_SCTLR: AtomicU64 = AtomicU64::new(0);

// Per-core boot stacks for the secondaries (indexed by cpu id).
#[unsafe(no_mangle)]
static mut SEC_STACKS: [[u8; SEC_STACK_SIZE as usize]; MAX_CPUS] = [[0; SEC_STACK_SIZE as usize]; MAX_CPUS];

// Count of secondaries that have come online, and their reported MPIDRs.
static SMP_ONLINE: AtomicU32 = AtomicU32::new(0);
static SEC_MPIDR: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

global_asm!(
	r#"
.section .text, "ax"
.global aarch64_secondary_start
aarch64_secondary_start:
	// x0 = context_id = cpu id (passed by PSCI CPU_ON). MMU is off.
	mov     x19, x0
	// Adopt the boot core's MMU configuration.
	adrp    x1, SEC_MAIR
	ldr     x1, [x1, :lo12:SEC_MAIR]
	adrp    x2, SEC_TCR
	ldr     x2, [x2, :lo12:SEC_TCR]
	adrp    x3, SEC_TTBR0
	ldr     x3, [x3, :lo12:SEC_TTBR0]
	msr     mair_el1, x1
	msr     tcr_el1, x2
	msr     ttbr0_el1, x3
	isb
	tlbi    vmalle1
	dsb     ish
	isb
	adrp    x4, SEC_SCTLR
	ldr     x4, [x4, :lo12:SEC_SCTLR]
	msr     sctlr_el1, x4      // enable the MMU (M bit set in the saved value)
	isb
	// Per-core stack: SEC_STACKS[cpu_id] top.
	adrp    x5, SEC_STACKS
	add     x5, x5, :lo12:SEC_STACKS
	mov     x6, #16384
	madd    x5, x19, x6, x5    // x5 = base + cpu_id * 16384
	add     x5, x5, x6         // + stack size = top
	mov     sp, x5
	// Shared EL1 exception vectors.
	adrp    x7, __exception_vectors
	add     x7, x7, :lo12:__exception_vectors
	msr     vbar_el1, x7
	isb
	mov     x0, x19
	bl      aarch64_secondary_main
0:
	wfe
	b       0b
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
	let mpidr: u64;
	unsafe {
		core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack, preserves_flags));
	}
	super::percpu::init(cpu_id as usize, mpidr as u32);
	super::gic::init_secondary();
	SEC_MPIDR[cpu_id as usize].store(mpidr, Ordering::Relaxed);
	SMP_ONLINE.fetch_add(1, Ordering::Release);
	super::enable_interrupts();
	loop {
		super::idle_halt();
	}
}

// Wake every secondary core (cpu ids 1..cpu_count) via PSCI CPU_ON and wait for
// them to report in. On QEMU virt the MPIDR affinity of cpu N is simply N.
pub fn bring_up_secondaries(cpu_count: u32) {
	if cpu_count <= 1 {
		return;
	}

	// Capture the boot core's MMU configuration for the secondaries.
	unsafe {
		let (mair, tcr, ttbr0, sctlr): (u64, u64, u64, u64);
		core::arch::asm!(
			"mrs {0}, mair_el1",
			"mrs {1}, tcr_el1",
			"mrs {2}, ttbr0_el1",
			"mrs {3}, sctlr_el1",
			out(reg) mair, out(reg) tcr, out(reg) ttbr0, out(reg) sctlr,
			options(nomem, nostack, preserves_flags),
		);
		SEC_MAIR.store(mair, Ordering::Relaxed);
		SEC_TCR.store(tcr, Ordering::Relaxed);
		SEC_TTBR0.store(ttbr0, Ordering::Relaxed);
		SEC_SCTLR.store(sctlr, Ordering::Relaxed);
	}

	let entry = aarch64_secondary_start as u64; // identity-mapped: VA == PA
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
	fn aarch64_secondary_start();
}
