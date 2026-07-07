// aarch64 exceptions - the EL1 vector table (VBAR_EL1) and synchronous-fault
// decode (M116).
//
// AArch64 has one 2 kB-aligned vector table of 16 entries (128 bytes each): four
// exception kinds (Synchronous / IRQ / FIQ / SError) for each of four sources
// (Current EL with SP0, Current EL with SPx, a Lower EL in AArch64, a Lower EL in
// AArch32). Each entry stubs into a common handler that reads the syndrome
// registers (ESR_EL1 = what happened, FAR_EL1 = the faulting address, ELR_EL1 =
// where) and reports.
//
// The fault split the kernel wants: a lower-EL (userspace) fault terminates only
// the faulting process; a current-EL (kernel) fault halts. There is no EL0
// userspace yet, so every exception currently halts after reporting - the split
// point is marked and fills in when the syscall / usermode path lands. IRQ / FIQ
// entries also route here for now; they become the GIC dispatch next.

use core::arch::{asm, global_asm};

global_asm!(
	r#"
.section .text.vectors, "ax"
.balign 2048
.global __exception_vectors
__exception_vectors:

.macro VEC id
.balign 128
	mov     x0, #\id
	b       __exc_common
.endm

	VEC 0   // Current EL with SP0:  Synchronous / IRQ / FIQ / SError
	VEC 1
	VEC 2
	VEC 3
	VEC 4   // Current EL with SPx  (the kernel runs here after boot)
	VEC 5
	VEC 6
	VEC 7
	VEC 8   // Lower EL, AArch64    (userspace, once it exists)
	VEC 9
	VEC 10
	VEC 11
	VEC 12  // Lower EL, AArch32
	VEC 13
	VEC 14
	VEC 15

__exc_common:
	// x0 = vector index (set by the entry). Gather the syndrome and report.
	mrs     x1, esr_el1
	mrs     x2, far_el1
	mrs     x3, elr_el1
	bl      aarch64_exception
0:
	wfe
	b       0b
"#
);

unsafe extern "C" {
	static __exception_vectors: u8;
}

// Point VBAR_EL1 at the vector table. Call once, early on each core.
pub fn init_vectors() {
	let vbar = &raw const __exception_vectors as u64;
	unsafe {
		asm!("msr vbar_el1, {}", "isb", in(reg) vbar, options(nostack, preserves_flags));
	}
}

// The common exception handler (called from every vector-table entry).
#[unsafe(no_mangle)]
extern "C" fn aarch64_exception(vector: u64, esr: u64, far: u64, elr: u64) -> ! {
	let ec = (esr >> 26) & 0x3f; // ESR_EL1.EC - the exception class
	let source = match vector / 4 {
		0 => "cur-EL/SP0",
		1 => "cur-EL/SPx",
		2 => "lower-EL/A64",
		_ => "lower-EL/A32",
	};
	let kind = match vector % 4 {
		0 => "sync",
		1 => "irq",
		2 => "fiq",
		_ => "serror",
	};
	crate::serial_println!("aarch64 EXCEPTION [{source} {kind}] EC={ec:#x} ESR={esr:#x} FAR={far:#x} ELR={elr:#x}");

	// Fault split (fills in with EL0 userspace): a lower-EL fault (vector 8..11)
	// would terminate only the faulting process; a current-EL (kernel) fault
	// halts. No userspace yet, so everything halts.
	crate::serial_println!("aarch64: unhandled exception - halting");
	super::halt_loop()
}
