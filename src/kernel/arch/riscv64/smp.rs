// riscv64 SMP bring-up via the SBI HSM extension (M117).
//
// OpenSBI starts only the boot hart; the others reset held in an SBI-parked state.
// `sbi_hart_start(hartid, start_addr, opaque)` (HSM extension, EID 0x48534D) releases
// one at a physical entry point in S-mode with the MMU off, a0 = hartid, a1 = opaque.
// `riscv64_secondary_start` is a low, position-independent stub (like the primary
// `_start`): it turns the MMU on with the boot core's Sv39 table (`__boot_tables`,
// built once by the primary), picks its per-core higher-half stack, then branches into
// the higher half. The secondaries idle in the scheduler until a thread is dispatched.

use core::arch::global_asm;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// Matches the per-CPU pool size in `percpu`.
const MAX_CPUS: usize = 8;
const SEC_STACK_SIZE: u64 = 16384;

// Per-core boot stacks for the secondaries (indexed by cpu id). No_mangle so the boot
// stub can name it; the .quad literal resolves to its higher-half address.
#[unsafe(no_mangle)]
static mut SEC_STACKS: [[u8; SEC_STACK_SIZE as usize]; MAX_CPUS] = [[0; SEC_STACK_SIZE as usize]; MAX_CPUS];

// Count of secondaries that have come online, and their reported hart ids.
static SMP_ONLINE: AtomicU32 = AtomicU32::new(0);
static SEC_HARTID: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

global_asm!(
	r#"
.section .data.boot, "a"
.balign 8
.Ls_main:   .quad riscv64_secondary_main
.Ls_stacks: .quad SEC_STACKS

.section .text.boot, "ax"
.global riscv64_secondary_start
riscv64_secondary_start:            // a0 = hartid, a1 = opaque = cpu id
	mv      s0, a0                  // hartid
	mv      s1, a1                  // cpu id

	// Adopt the boot Sv39 table the primary already built (its low identity addr is
	// its physical address). SATP = (8 << 60) | (root_phys >> 12), mode 8 = Sv39.
	la      t0, __boot_tables
	srli    t1, t0, 12
	li      t2, 8
	slli    t2, t2, 60
	or      t1, t1, t2
	sfence.vma
	csrw    satp, t1
	sfence.vma

	// Per-core higher-half stack: SEC_STACKS[cpu_id] top.
	la      t0, .Ls_stacks
	ld      t0, 0(t0)               // higher-half base of SEC_STACKS
	li      t1, 16384               // SEC_STACK_SIZE
	mul     t2, s1, t1
	add     t0, t0, t2
	add     t0, t0, t1              // + one stack (top)
	mv      sp, t0

	// Branch into the higher half: riscv64_secondary_main(cpu_id, hartid).
	la      t0, .Ls_main
	ld      t0, 0(t0)
	mv      a0, s1                  // cpu id
	mv      a1, s0                  // hartid
	jr      t0
0:
	wfi
	j       0b

.section .data, "aw"
.balign 8
.global riscv64_secondary_entry
riscv64_secondary_entry:
	.quad riscv64_secondary_start
"#
);

// The low physical address of the secondary boot stub, filled in by the linker.
unsafe extern "C" {
	static riscv64_secondary_entry: u64;
}

// First Rust code a secondary hart runs (MMU on, per-core stack set). It brings up its
// per-CPU block, trap vector, and local timer, records itself online, then idles.
#[unsafe(no_mangle)]
extern "C" fn riscv64_secondary_main(cpu_id: u64, hartid: u64) -> ! {
	// Permit S-mode access to U-pages (SUM) and enable the FPU (FS = Initial) on this
	// hart, matching the boot hart's setup before any context switch.
	unsafe { core::arch::asm!("csrs sstatus, {}", in(reg) (1u64 << 18) | (1u64 << 13), options(nostack, preserves_flags)) };
	super::traps::init();
	super::percpu::init(cpu_id as usize, hartid as u32);
	crate::smp::set_lapic_id(cpu_id as usize, hartid as u32);
	super::plic::init_hart(hartid);
	super::apic::init_ap();
	SEC_HARTID[cpu_id as usize].store(hartid, Ordering::Relaxed);
	SMP_ONLINE.fetch_add(1, Ordering::Release);
	// Also count this core in the portable online tally the scheduler and tests read.
	crate::smp::mark_online();
	super::enable_interrupts();
	// The BSP brings the scheduler up (allocate + init) after waking us: spin until it
	// has, then park in the scheduler idle loop so threads can be scheduled onto this
	// hart (the wake IPI bounces it out of wfi to pick them up).
	while !crate::sched::is_initialized() {
		super::idle_halt();
	}
	crate::sched::cpu_idle_loop()
}

// Issue an SBI HSM hart_start (EID 0x48534D, FID 0). Returns the SBI error (0 = OK).
fn sbi_hart_start(hartid: u64, start_addr: u64, opaque: u64) -> i64 {
	let err: i64;
	unsafe {
		core::arch::asm!(
			"ecall",
			in("a7") 0x48534Dusize, // "HSM"
			in("a6") 0usize,        // hart_start
			inout("a0") hartid => err,
			in("a1") start_addr,
			in("a2") opaque,
			options(nostack),
		);
	}
	err
}

// Wake every secondary hart via SBI HSM hart_start and wait for them to report in. The
// OpenSBI boot hart is not necessarily hart 0 (QEMU virt often boots on hart 1), so the
// boot hart id is skipped and the remaining harts get contiguous cpu ids 1.. (the boot
// hart is cpu id 0). On QEMU virt the hart ids are 0..cpu_count-1.
pub fn bring_up_secondaries(cpu_count: u32, boot_hartid: u64) {
	if cpu_count <= 1 {
		return;
	}

	// The secondary entry is the low, physical `.text.boot` stub address; the SBI
	// releases each hart there with the MMU off, and the stub adopts the boot page
	// tables. High kernel code cannot address the low symbol directly, so its address
	// is read from a linker-filled data word.
	let entry = unsafe { riscv64_secondary_entry };
	let mut cpu_id = 1u64;
	for hartid in 0..cpu_count as u64 {
		if hartid == boot_hartid || cpu_id as usize >= MAX_CPUS {
			continue;
		}
		let status = sbi_hart_start(hartid, entry, cpu_id);
		if status != 0 {
			crate::serial_println!("riscv64: hart_start hart {hartid} failed (SBI {status})");
		}
		cpu_id += 1;
	}
	let want = (cpu_count - 1).min((MAX_CPUS - 1) as u32);

	// Wait for the secondaries to come online.
	let mut spins: u64 = 0;
	while SMP_ONLINE.load(Ordering::Acquire) < want && spins < 2_000_000_000 {
		core::hint::spin_loop();
		spins += 1;
	}

	let online = SMP_ONLINE.load(Ordering::Acquire);
	crate::serial_println!("riscv64: SMP - {}/{} secondary harts online", online, want);
	for cpu in 1..=want as usize {
		crate::serial_println!("riscv64:   cpu {} up (hart {})", cpu, SEC_HARTID[cpu].load(Ordering::Relaxed));
	}
}
