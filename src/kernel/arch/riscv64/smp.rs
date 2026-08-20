// riscv64 SMP bring-up via the SBI HSM extension.
//
// OpenSBI starts only the boot hart; the others reset held in an SBI-parked state.
// `sbi_hart_start(hartid, start_addr, opaque)` (HSM extension, EID 0x48534D) releases
// one at a physical entry point in S-mode with the MMU off, a0 = hartid, a1 = opaque.
// `riscv64_secondary_start` is a low, position-independent stub (like the primary
// `_start`): it turns the MMU on with the boot core's Sv39 table (`__boot_tables`,
// built once by the primary), picks its per-core higher-half stack, then branches into
// the higher half. The secondaries idle in the scheduler until a thread is dispatched.

use core::arch::global_asm;
use core::sync::atomic::{AtomicU32, Ordering};

use alloc::vec::Vec;

const SEC_STACK_SIZE: usize = 16384;

// Count of secondaries that have come online.
static SMP_ONLINE: AtomicU32 = AtomicU32::new(0);

global_asm!(
	r#"
.section .data.boot, "a"
.balign 8
.Ls_main:    .quad riscv64_secondary_main
.Ls_stacksp: .quad riscv64_sec_stacks_ptr

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

	// Per-core higher-half stack: the heap stacks base + cpu_id*SEC_STACK_SIZE, top.
	// The base lives in a higher-half static the BSP filled; the low boot word holds
	// that static's address, so double-dereference (low word -> static -> base).
	la      t0, .Ls_stacksp
	ld      t0, 0(t0)               // higher-half address of riscv64_sec_stacks_ptr
	ld      t0, 0(t0)               // the heap secondary-stacks base
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

// The higher-half base of the heap-allocated secondary stacks, published by the BSP
// before waking any hart; the boot stub reads it (indirectly, through a low boot word
// holding this static's address) to find its stack. It is a higher-half static so the
// BSP's write stays in PC-relative range (a word in the low `.data.boot` could not be
// reached from the higher-half kernel).
#[unsafe(no_mangle)]
static mut riscv64_sec_stacks_ptr: u64 = 0;

// First Rust code a secondary hart runs (MMU on, per-core stack set). It brings up its
// per-CPU block, trap vector, and local timer, records itself online, then idles.
#[unsafe(no_mangle)]
extern "C" fn riscv64_secondary_main(cpu_id: u64, hartid: u64) -> ! {
	// Enable the FPU (FS = Initial) on this hart, matching the boot hart's setup before any context
	// switch. SUM stays CLEAR here too - see `boot.rs` and `paging::user_access`.
	unsafe { core::arch::asm!("csrs sstatus, {}", in(reg) 1u64 << 13, options(nostack, preserves_flags)) };
	unsafe { core::arch::asm!("csrw scounteren, {}", in(reg) 0x7u64, options(nostack, preserves_flags)) };
	super::traps::init();
	super::percpu::init(cpu_id as usize, hartid as u32);
	crate::smp::set_lapic_id(cpu_id as usize, hartid as u32);
	super::imsic::init_hart();
	super::apic::init_ap();
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

// Wake every secondary hart the device tree names, via SBI HSM hart_start, and wait for each to
// report in before starting the next. The OpenSBI boot hart is not necessarily hart 0 (QEMU virt
// often boots on hart 1), so the boot hart id is skipped and the harts that answer get contiguous
// cpu ids 1.. (the boot hart is cpu id 0). Returns how many cores are online, the boot hart
// included - which is the number the rest of the kernel may use.
pub fn bring_up_secondaries(hart_ids: &[u64], boot_hartid: u64) -> usize {
	if hart_ids.len() <= 1 {
		return 1;
	}

	// Allocate the secondary stacks as a single zeroed, 16-byte-aligned heap block: u128
	// elements give the alignment (the RISC-V ABI needs a 16-aligned sp) and let the
	// allocator zero the block directly - building a `[u8; 16384]` on the boot stack
	// would blow it. Index 0 (the boot hart) is left unused; the stacks scale with the
	// hart count, no compile-time cap. Publish the higher-half base to the boot stub
	// through the shared data word before any secondary reads it.
	let words = hart_ids.len() * (SEC_STACK_SIZE / 16);
	// ALLOC-OK: boot, the AP stacks, allocated once before the APs start
	let stacks: Vec<u128> = alloc::vec![0u128; words];
	let base = Vec::leak(stacks).as_mut_ptr() as u64;
	unsafe {
		*(&raw mut riscv64_sec_stacks_ptr) = base;
		core::arch::asm!("fence", options(nostack, preserves_flags));
	}

	// The secondary entry is the low, physical `.text.boot` stub address; the SBI
	// releases each hart there with the MMU off, and the stub adopts the boot page
	// tables. High kernel code cannot address the low symbol directly, so its address
	// is read from a linker-filled data word.
	let entry = unsafe { riscv64_secondary_entry };

	// THE IDS THE TREE GAVE, AND ONE LOGICAL ID PER HART THAT ANSWERS (KERN-ARCH-008).
	//
	// This used to start harts `0..cpu_count` - dense, zero-based and in tree order, none of which
	// a device tree promises - and hand out a logical id per attempt whether or not the hart behind
	// it existed. A board whose harts are 0, 1, 4, 5, or whose middle hart is `disabled`, had ids
	// pointing at harts that never started, and the count published afterwards described neither.
	//
	// The logical id - the index into the per-CPU table, the scheduler's run queues and the stack
	// block - is now handed out only to a hart that has REPORTED IN, so a failure consumes no id
	// and leaves no gap behind it.
	let mut online = 1usize;
	for &hartid in hart_ids {
		if hartid == boot_hartid {
			continue; // the hart this code is running on
		}
		let cpu_id = online as u64;
		let status = sbi_hart_start(hartid, entry, cpu_id);
		if status != 0 {
			crate::serial_println!("riscv64: hart_start hart {hartid} failed (SBI {status})");
			continue;
		}
		// A BOUND, NOT A MEASUREMENT: long enough that a working hart always arrives first, short
		// enough that one which never will does not hold the boot. Waiting for THIS hart before
		// starting the next is what keeps the logical ids contiguous.
		let mut spins: u64 = 0;
		while SMP_ONLINE.load(Ordering::Acquire) < online as u32 && spins < 500_000_000 {
			core::hint::spin_loop();
			spins += 1;
		}
		if SMP_ONLINE.load(Ordering::Acquire) < online as u32 {
			crate::serial_println!("riscv64: hart {hartid} took hart_start and never reported in");
			continue;
		}
		crate::serial_println!("riscv64:   cpu {cpu_id} up (hart {hartid})");
		online += 1;
	}
	crate::serial_println!("riscv64: SMP - {} of {} declared harts online", online, hart_ids.len());
	online
}
