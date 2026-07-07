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

// The vector table plus the trap entry/exit. Every entry saves a full register
// frame, records its vector index, and calls `aarch64_trap`; an IRQ returns and
// the shared `__trap_return` restores the frame and `eret`s back to the
// interrupted code, while a fault halts inside the handler.
global_asm!(
	r#"
.section .text.vectors, "ax"

// Save x0..x30 + ELR_EL1 + SPSR_EL1 into a 272-byte frame on the stack.
.macro KERNEL_ENTRY
	sub     sp, sp, #272
	stp     x0,  x1,  [sp, #0]
	stp     x2,  x3,  [sp, #16]
	stp     x4,  x5,  [sp, #32]
	stp     x6,  x7,  [sp, #48]
	stp     x8,  x9,  [sp, #64]
	stp     x10, x11, [sp, #80]
	stp     x12, x13, [sp, #96]
	stp     x14, x15, [sp, #112]
	stp     x16, x17, [sp, #128]
	stp     x18, x19, [sp, #144]
	stp     x20, x21, [sp, #160]
	stp     x22, x23, [sp, #176]
	stp     x24, x25, [sp, #192]
	stp     x26, x27, [sp, #208]
	stp     x28, x29, [sp, #224]
	mrs     x0,  elr_el1
	mrs     x1,  spsr_el1
	stp     x30, x0,  [sp, #240]
	str     x1,  [sp, #256]
.endm

.balign 2048
.global __exception_vectors
__exception_vectors:

.macro VEC id
.balign 128
	KERNEL_ENTRY
	mov     x0, #\id       // vector index
	mov     x1, sp         // frame pointer
	bl      aarch64_trap
	b       __trap_return
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

// Restore the frame and return to the interrupted context.
__trap_return:
	ldr     x1,  [sp, #256]
	ldp     x30, x0,  [sp, #240]
	msr     spsr_el1, x1
	msr     elr_el1,  x0
	ldp     x0,  x1,  [sp, #0]
	ldp     x2,  x3,  [sp, #16]
	ldp     x4,  x5,  [sp, #32]
	ldp     x6,  x7,  [sp, #48]
	ldp     x8,  x9,  [sp, #64]
	ldp     x10, x11, [sp, #80]
	ldp     x12, x13, [sp, #96]
	ldp     x14, x15, [sp, #112]
	ldp     x16, x17, [sp, #128]
	ldp     x18, x19, [sp, #144]
	ldp     x20, x21, [sp, #160]
	ldp     x22, x23, [sp, #176]
	ldp     x24, x25, [sp, #192]
	ldp     x26, x27, [sp, #208]
	ldp     x28, x29, [sp, #224]
	add     sp, sp, #272
	eret
"#
);

// EL0 entry / return trampolines.
//
// `aarch64_enter_el0(entry, user_sp, arg, spsr)` saves the kernel's callee-saved
// registers + LR + SP into a resume block, sets SP_EL0 / ELR_EL1 / SPSR_EL1, and
// `eret`s down to EL0 with x0 = arg. It does not return here; instead, when the
// EL0 program makes the "exit" syscall, `aarch64_trap` calls `aarch64_exit_el0`,
// which reloads the resume block and `ret`s - unwinding straight back to the
// caller of `aarch64_enter_el0` as if it had returned normally.
global_asm!(
	r#"
.section .bss
.balign 8
__el0_resume:
	.skip 112

.section .text, "ax"
.global aarch64_enter_el0
aarch64_enter_el0:
	adrp    x4, __el0_resume
	add     x4, x4, :lo12:__el0_resume
	stp     x19, x20, [x4, #0]
	stp     x21, x22, [x4, #16]
	stp     x23, x24, [x4, #32]
	stp     x25, x26, [x4, #48]
	stp     x27, x28, [x4, #64]
	stp     x29, x30, [x4, #80]
	mov     x5, sp
	str     x5, [x4, #96]
	msr     sp_el0,   x1
	msr     elr_el1,  x0
	msr     spsr_el1, x3
	mov     x0, x2
	eret

.global aarch64_exit_el0
aarch64_exit_el0:
	adrp    x4, __el0_resume
	add     x4, x4, :lo12:__el0_resume
	ldp     x19, x20, [x4, #0]
	ldp     x21, x22, [x4, #16]
	ldp     x23, x24, [x4, #32]
	ldp     x25, x26, [x4, #48]
	ldp     x27, x28, [x4, #64]
	ldp     x29, x30, [x4, #80]
	ldr     x5, [x4, #96]
	mov     sp, x5
	ret
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

// The common trap handler, called from every vector entry with the vector index
// and a pointer to the saved register frame. An IRQ is acknowledged, dispatched,
// and returns (the caller `eret`s); a synchronous fault is decoded and halts.
#[unsafe(no_mangle)]
extern "C" fn aarch64_trap(vector: u64, frame: *mut u64) {
	// vector index: source = index / 4 (0 cur-EL/SP0, 1 cur-EL/SPx, 2 lower/A64,
	// 3 lower/A32), kind = index % 4 (0 sync, 1 irq, 2 fiq, 3 serror).
	if vector % 4 == 1 {
		super::gic::handle_irq(vector / 4 == 2);
		return; // -> __trap_return erets back to the interrupted code
	}

	let (esr, far, elr): (u64, u64, u64);
	unsafe {
		asm!(
			"mrs {0}, esr_el1",
			"mrs {1}, far_el1",
			"mrs {2}, elr_el1",
			out(reg) esr, out(reg) far, out(reg) elr,
			options(nomem, nostack, preserves_flags),
		);
	}
	let ec = (esr >> 26) & 0x3f; // ESR_EL1.EC - the exception class

	// SVC from AArch64 (EC 0x15): a system call from EL0. Dispatch it against the
	// saved register frame (x8 = number, x0.. = args, x0 = return); the "exit"
	// syscall unwinds back to the kernel that entered EL0, anything else `eret`s
	// back to the user program.
	if ec == 0x15 {
		if unsafe { super::syscall::dispatch(frame) } {
			super::usermode::exit_to_kernel();
		}
		return;
	}

	let source = match vector / 4 {
		0 => "cur-EL/SP0",
		1 => "cur-EL/SPx",
		2 => "lower-EL/A64",
		_ => "lower-EL/A32",
	};
	let kind = match vector % 4 {
		0 => "sync",
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
