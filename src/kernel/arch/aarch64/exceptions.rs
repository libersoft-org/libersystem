// aarch64 exceptions - the EL1 vector table (VBAR_EL1) and synchronous-fault
// decode.
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

// The vector table plus the trap entry/exit. Each 128-byte vector slot does the
// minimum (reserve the frame, save a scratch pair, record its index) and branches
// to the common trampoline, which finishes the full save - x0..x30 + ELR_EL1 +
// SPSR_EL1 + the FP/SIMD state (V0..V31 + FPSR/FPCR) - and calls `aarch64_trap`.
// FP is saved on every trap because the kernel itself uses FP/SIMD (bulk memory
// ops), so an EL0 excursion or preemption must not clobber the interrupted
// context's vector registers. An IRQ returns and `__trap_return` restores the
// frame and `eret`s; a fault halts inside the handler. The heavy save lives in the
// shared trampoline, not each slot, because the full save does not fit in 128 bytes.
global_asm!(
	r#"
.section .text.vectors, "ax"

.balign 2048
.global __exception_vectors
__exception_vectors:

.macro VEC id
.balign 128
	sub     sp, sp, #816
	stp     x0, x1, [sp, #0]
	mov     x0, #\id
	b       __trap_common
.endm

	VEC 0   // Current EL with SP0:  Synchronous / IRQ / FIQ / SError
	VEC 1
	VEC 2
	VEC 3
	VEC 4   // Current EL with SPx  (the kernel runs here after boot)
	VEC 5
	VEC 6
	VEC 7
	VEC 8   // Lower EL, AArch64    (userspace)
	VEC 9
	VEC 10
	VEC 11
	VEC 12  // Lower EL, AArch32
	VEC 13
	VEC 14
	VEC 15

// Finish saving the frame (x0/x1 are already saved by the slot; x0 holds the
// vector index), then dispatch with x0 = index and x1 = frame pointer.
__trap_common:
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
	mrs     x2,  elr_el1
	mrs     x3,  spsr_el1
	stp     x30, x2,  [sp, #240]
	str     x3,  [sp, #256]
	mrs     x2,  fpsr
	mrs     x3,  fpcr
	stp     x2,  x3,  [sp, #272]
	stp     q0,  q1,  [sp, #288]
	stp     q2,  q3,  [sp, #320]
	stp     q4,  q5,  [sp, #352]
	stp     q6,  q7,  [sp, #384]
	stp     q8,  q9,  [sp, #416]
	stp     q10, q11, [sp, #448]
	stp     q12, q13, [sp, #480]
	stp     q14, q15, [sp, #512]
	stp     q16, q17, [sp, #544]
	stp     q18, q19, [sp, #576]
	stp     q20, q21, [sp, #608]
	stp     q22, q23, [sp, #640]
	stp     q24, q25, [sp, #672]
	stp     q26, q27, [sp, #704]
	stp     q28, q29, [sp, #736]
	stp     q30, q31, [sp, #768]
	mrs     x2, sp_el0
	str     x2, [sp, #800]
	mov     x1, sp
	bl      aarch64_trap
	b       __trap_return

// Restore the frame and return to the interrupted context.
__trap_return:
	ldr     x2,  [sp, #800]
	msr     sp_el0, x2
	ldp     x0,  x1,  [sp, #272]
	msr     fpsr, x0
	msr     fpcr, x1
	ldp     q0,  q1,  [sp, #288]
	ldp     q2,  q3,  [sp, #320]
	ldp     q4,  q5,  [sp, #352]
	ldp     q6,  q7,  [sp, #384]
	ldp     q8,  q9,  [sp, #416]
	ldp     q10, q11, [sp, #448]
	ldp     q12, q13, [sp, #480]
	ldp     q14, q15, [sp, #512]
	ldp     q16, q17, [sp, #544]
	ldp     q18, q19, [sp, #576]
	ldp     q20, q21, [sp, #608]
	ldp     q22, q23, [sp, #640]
	ldp     q24, q25, [sp, #672]
	ldp     q26, q27, [sp, #704]
	ldp     q28, q29, [sp, #736]
	ldp     q30, q31, [sp, #768]
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
	add     sp, sp, #816
	eret
"#
);

// EL0 entry / return trampolines.
//
// `aarch64_enter_el0(entry, user_sp, arg, spsr, resume_slot)` saves the kernel's
// callee-saved registers + LR onto the current (per-thread) kernel stack, parks
// that resume stack pointer in `*resume_slot` (the calling thread's syscall_rsp
// slot), sets SP_EL0 / ELR_EL1 / SPSR_EL1, and `eret`s down to EL0 with x0 = arg.
// It does not return here; when the EL0 program makes SYS_USER_EXIT, `aarch64_trap`
// calls `aarch64_exit_el0(resume_sp)` with the parked value, which reloads the
// block and `ret`s - unwinding straight back to the caller of `aarch64_enter_el0`.
// The resume state is per-thread (on each thread's own stack, addressed by its own
// slot), so several user threads can be mid-excursion at once.
global_asm!(
	r#"
.section .text, "ax"
.global aarch64_enter_el0
aarch64_enter_el0:
	stp     x19, x20, [sp, #-96]!
	stp     x21, x22, [sp, #16]
	stp     x23, x24, [sp, #32]
	stp     x25, x26, [sp, #48]
	stp     x27, x28, [sp, #64]
	stp     x29, x30, [sp, #80]
	mov     x5, sp
	str     x5, [x4]           // *resume_slot = resume stack pointer
	msr     sp_el0,   x1
	msr     elr_el1,  x0
	msr     spsr_el1, x3
	mov     x0, x2
	eret

.global aarch64_exit_el0
aarch64_exit_el0:
	mov     sp, x0             // x0 = parked resume stack pointer
	ldp     x19, x20, [sp, #0]
	ldp     x21, x22, [sp, #16]
	ldp     x23, x24, [sp, #32]
	ldp     x25, x26, [sp, #48]
	ldp     x27, x28, [sp, #64]
	ldp     x29, x30, [sp, #80]
	add     sp, sp, #96
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
	let kind_str = match vector % 4 {
		0 => "sync",
		2 => "fiq",
		_ => "serror",
	};

	// A lower-EL (userspace) fault terminates only the faulting process: the kernel
	// records the fault, tears the process down, notifies the supervisor, and
	// unwinds to the kernel thread that entered EL0. A current-EL (kernel) fault is
	// a kernel bug and halts.
	if vector / 4 == 2 {
		// A not-present data abort inside the stack span is demand-paged growth: map a
		// page and `eret` to retry the faulting store (the resumable fault, mirroring
		// the x86 page-fault handler). ESR.DFSC 0b0001xx (0x04..=0x07) is a translation
		// fault (not present); the stack grows on data writes (EC 0x24/0x25).
		let dfsc = esr & 0x3f;
		if (ec == 0x24 || ec == 0x25) && (0x04..=0x07).contains(&dfsc) && crate::fault::grow_user_stack(far, 0) {
			return;
		}
		let kind = match ec {
			0x20 | 0x21 | 0x24 | 0x25 => crate::fault::FAULT_PAGE, // instruction / data abort
			_ => crate::fault::FAULT_GENERAL_PROTECTION,
		};
		crate::fault::terminate_user(crate::fault::FaultInfo { kind, error_code: esr, address: far, instruction_pointer: elr });
	}

	// A current-EL fault reaching here is a kernel bug: report it and halt.
	crate::serial_println!("aarch64 EXCEPTION [{source} {kind_str}] EC={ec:#x} ESR={esr:#x} FAR={far:#x} ELR={elr:#x}");
	crate::serial_println!("aarch64: unhandled exception - halting");
	super::halt_loop()
}
