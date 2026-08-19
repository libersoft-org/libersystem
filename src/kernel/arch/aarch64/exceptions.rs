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
// The size of the trap frame every vector reserves. One number, used by the reserve, by the
// restore, and by the stack-bounds check that decides whether it fits.
pub const TRAP_FRAME_BYTES: u64 = 816;

global_asm!(
	r#"
.section .text.vectors, "ax"

.balign 2048
.global __exception_vectors
__exception_vectors:

// EVERY VECTOR ASKS WHETHER `SP` IS USABLE BEFORE IT USES IT, because the answer used to be
// discovered by faulting.
//
// The entry was `sub sp, sp, #816` followed by `stp x0, x1, [sp]`, and when `SP` did not point at
// mapped memory that store faulted - which re-entered this same vector, subtracted another 816,
// stored again and faulted again, forever. Measured in a wedged guest: all eight cores at
// `__exception_vectors + 0x200`, `ESR_EL1 = 0x96000044` (a write, translation fault at level 0),
// `FAR_EL1` equal to `SP` to the byte, and `ELR_EL1` on that `stp`. The stack pointer walked down
// through the kernel window at 816 bytes a turn, writing a register pair everywhere it happened to
// land on a mapped page - which is what corrupted a virtio queue register in one run, and why the
// failure was silent in all of them: the loop never reaches a print. A guard page whose fault
// cannot be reported only changes which byte the damage starts at.
//
// The check is a range test, so one subtract and one compare catch a stack pointer that has run off
// the bottom AND one that has been corrupted upward: `sp - floor <= span`, unsigned.
//
// GETTING A REGISTER TO ASK WITH is the whole difficulty, since anything spilled to the stack
// depends on the stack being usable. `TPIDRRO_EL0` is free on this kernel and needs no memory, so
// it frees `x0`; `x0` then reaches the per-CPU block through `TPIDR_EL1`, which frees two more into
// per-CPU memory that is statically allocated and mapped for the life of the kernel.
//
// EVERY FAILURE OF THE CHECK ITSELF FALLS BACK TO THE OLD BEHAVIOUR. A zero `TPIDR_EL1` (an
// exception before per-CPU init) and a zero span (the boot stack, the secondary bring-up stacks,
// the idle loop - contexts the scheduler does not describe) both skip straight to the reserve. The
// check can therefore only ever turn a silent runaway into a report; it cannot turn a working boot
// into a refusal.
.macro VEC id
.balign 128
	msr     tpidrro_el0, x0            // a register, without touching memory
	mrs     x0, tpidr_el1
	cbz     x0, 8f                     // per-CPU block not up yet: nothing to check against
	stp     x1, x2, [x0, #{PC_SCRATCH}]
	ldp     x1, x2, [x0, #{PC_FLOOR}]  // x1 = lowest SP a frame fits under, x2 = how far above it
	cbz     x2, 7f                     // no bounds recorded for this context - counted at 7:
	mov     x0, sp
	sub     x0, x0, x1
	cmp     x0, x2
	b.ls    9f                         // SP is on the stack this thread was given: ordinary path
	// ONLY ON THE WAY TO THE REPORT (KERN-ARCH-002). This was `mov x4, #\id` before the branch,
	// with "harmless otherwise" beside it - and it was not harmless. The `mov` ran on EVERY
	// exception, the ordinary path fell through to `__trap_common`, and `__trap_common` saves x4
	// into the frame it later restores from. So every trap - every timer interrupt, every
	// preemption, every syscall - returned to EL0 with x4 replaced by the vector index.
	//
	// That is both halves of the finding: a kernel value handed to ring 3 for free, and a
	// caller-saved register silently overwritten in code that had every right to keep it across an
	// asynchronous interrupt. Setting it here costs one instruction on the path that is about to
	// print a report and stop, and nothing at all on the path that returns.
	mov     x4, #\id                   // which slot this is, for the report
	b       __trap_bad_stack           // SP is not on the stack this thread was given
7:
	// UNCHECKED, AND COUNTED. An exception taken on a context with no recorded bounds is exactly
	// where the runaway must have started - it is caught hundreds of frames later, on a core whose
	// bounds ARE set, which can only happen if it began somewhere they were not. A bare skip made
	// that invisible; a counter makes "how often does this happen, and to whom" a number the report
	// prints instead of a hypothesis.
	ldr     x1, [x0, #{PC_UNCHECKED}]
	add     x1, x1, #1
	str     x1, [x0, #{PC_UNCHECKED}]
9:
	mrs     x0, tpidr_el1
	ldp     x1, x2, [x0, #{PC_SCRATCH}]
8:
	mrs     x0, tpidrro_el0
	sub     sp, sp, #{FRAME}
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

// Reached when a vector decided the stack it was handed cannot hold a frame. The offending `SP`
// is recoverable because the check computed `sp - floor` in x0 and left the floor in x1; the
// per-CPU reporting stack is a fixed allocation set before `TPIDR_EL1` was ever published, so
// standing on it needs nothing that could itself be broken.
__trap_bad_stack:
	add     x0, x0, x1                 // x0 = the SP that was refused
	mrs     x1, tpidr_el1
	ldr     x1, [x1, #{PC_TRAP_STACK}]
	cbz     x1, 6f
	mov     sp, x1
	mrs     x1, esr_el1
	mrs     x2, far_el1
	mrs     x3, elr_el1
	// x4 already holds the vector index; SPSR says which level and which stack the exception came
	// from, which is what separates "the stack is bad" from "the check is looking at the wrong
	// stack pointer".
	mrs     x5, spsr_el1
	bl      aarch64_bad_stack
6:
	wfi
	b       6b

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
	add     sp, sp, #{FRAME}
	// The vector prologue parks a register in `TPIDRRO_EL0`, which EL0 can READ. Leaving whatever
	// x0 held at the last trap there would hand userspace a kernel value for free, so it goes back
	// as zero - one instruction on the return path, against an information leak on every trap.
	msr     tpidrro_el0, xzr
	eret
"#,
	PC_SCRATCH = const super::percpu::OFF_SCRATCH,
	PC_FLOOR = const super::percpu::OFF_STACK_FLOOR,
	PC_TRAP_STACK = const super::percpu::OFF_TRAP_STACK,
	PC_UNCHECKED = const super::percpu::OFF_UNCHECKED,
	FRAME = const TRAP_FRAME_BYTES,
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
// A trap arrived on a stack pointer that is not on the running thread's kernel stack.
//
// THIS IS THE REPORT THAT DID NOT EXIST, and its absence is what made the aarch64 failure in
// P02M0133 cost four days. The old vector saved its frame wherever `SP` pointed; when that was not
// mapped, the save faulted, re-entered the vector, and looped - producing no output at all while
// walking `SP` down through the kernel window and overwriting whatever it passed. Every hypothesis
// about that failure had to be built from its end state, and every one of them was wrong.
//
// Running on this core's own reporting stack, so nothing here depends on the stack that was
// refused. It halts rather than returning: `SP` is unusable, so there is no frame to `eret` from,
// and continuing would mean guessing which of the two - the pointer or the bounds - is the wrong
// one.
// NO `core::fmt` ANYWHERE ON THIS PATH, and that is the second thing this reporter had to learn.
//
// It was written with `writeln!` at a polled UART, which looked safe: no lock, no allocation, one
// short line. It printed NOTHING - seven cores reached the halt loop below with not a byte on the
// wire - because `core::fmt` unoptimised is many nested frames deep for a single formatted line, so
// the formatting ran off the reporting stack. That is silent rather than a fault (the stacks are a
// plain array, so the neighbour's slice is mapped memory), and the re-entry counted itself as
// another report, which is exactly the state a debugger found.
//
// A fixed string and a hand-written hex digit need one small buffer and no call depth at all. The
// report is uglier to read and it arrives, which is the entire trade.
fn wire_str(text: &str) {
	super::serial::write_bytes(text.as_bytes());
}

fn wire_hex(value: u64) {
	let mut out = [0u8; 18];
	out[0] = b'0';
	out[1] = b'x';
	for index in 0..16 {
		let nibble = ((value >> (60 - index * 4)) & 0xf) as u8;
		out[2 + index] = if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 };
	}
	super::serial::write_bytes(&out);
}

#[unsafe(no_mangle)]
extern "C" fn aarch64_bad_stack(sp: u64, esr: u64, far: u64, elr: u64, vector: u64, spsr: u64) -> ! {
	use core::sync::atomic::{AtomicU32, Ordering};

	// STRAIGHT AT THE WIRE, deliberately bypassing `serial_println!`. `_print` takes a lock to keep
	// two cores' lines from interleaving, and this is the one place in the kernel where that trade
	// is backwards: the machine is already broken, the other cores are usually broken in the same
	// instant, and a report that waits for a lock is a report that does not arrive. The first
	// version used the macro and the guest wedged INSIDE the reporter, with cores spinning on the
	// print lock - the failure this whole mechanism exists to end, reproduced one level up.
	//
	// THE SYNDROME FIRST, before anything that could fail. `this_cpu()` dereferences `TPIDR_EL1`
	// and the bounds come out of the per-CPU block, and if what corrupted this core's stack pointer
	// also reached those, then asking for them is how the report dies before it says anything. So
	// the four values that came from the CPU's own registers go out first, and everything derived
	// comes after.
	// ONE CORE SPEAKS. Whatever corrupts one core's state usually takes its neighbours with it
	// within microseconds, and this path has no lock by design - so letting three cores report
	// produced three reports interleaved at byte granularity, which is the defect this milestone is
	// named for, reproduced by its own diagnostic. The first report is the informative one; a second
	// core's would say the same thing about a different slice, and a garbled pair says nothing.
	static REPORTS: AtomicU32 = AtomicU32::new(0);
	if REPORTS.fetch_add(1, Ordering::AcqRel) == 0 {
		wire_str("\naarch64: BAD KERNEL STACK sp=");
		wire_hex(sp);
		wire_str(" esr=");
		wire_hex(esr);
		wire_str(" far=");
		wire_hex(far);
		wire_str(" elr=");
		wire_hex(elr);
		wire_str("\naarch64:   this core was told its stack is ");
		let (floor, span) = super::percpu::stack_bounds();
		wire_hex(floor);
		wire_str("..=");
		wire_hex(floor + span);

		// BELOW THE FLOOR IS NOT THE SAME AS HAVING GROWN THERE, and the first version of this line
		// said it was - "sp is BELOW it, so this is a stack overflow" - which the very first real
		// report contradicted: three cores with stack pointers hundreds of kilobytes below their
		// floors and NOT 16-byte aligned. A stack pointer that got where it is by making calls moves
		// in 16-byte steps and stays inside its own allocation; one that is misaligned was WRITTEN.
		// The alignment is the evidence, so the report states it instead of concluding from
		// direction alone.
		wire_str(if sp < floor { " - sp is BELOW it by " } else { " - sp is ABOVE it by " });
		wire_hex(if sp < floor { floor - sp } else { sp - (floor + span) });
		if sp & 0xf != 0 {
			wire_str(" and is NOT 16-byte aligned, so it was written rather than grown\n");
		} else {
			wire_str(" and is 16-byte aligned, which is consistent with having grown there\n");
		}
		// AND HOW MANY TRAPS WENT UNCHECKED, which is the number that says whether this was caught
		// where it started. A runaway found hundreds of frames deep on a core with bounds must have
		// begun on one without them; a zero here refutes that outright.
		// THE SLOT AND THE LEVEL, which decide whether this report is about a bad stack at all.
		// Slots 0-3 are "Current EL with SP0", where `SP` means SP_EL0 - the USER stack pointer on
		// this kernel - so a report from one of those is the check being pointed at the wrong
		// register rather than a corrupted kernel stack. Slots 4-7 are the kernel's own SPx.
		wire_str("aarch64:   vector slot ");
		wire_hex(vector);
		wire_str(" spsr=");
		wire_hex(spsr);
		wire_str(if vector < 4 {
			" (Current EL with SP0 - SP here is SP_EL0, NOT a kernel stack)\n"
		} else if vector < 8 {
			" (Current EL with SPx - the kernel's own stack)\n"
		} else {
			" (a lower EL - SP is the kernel stack the entry switched to)\n"
		});
		wire_str("aarch64:   exceptions taken with no recorded stack bounds so far: ");
		wire_hex(super::percpu::unchecked_traps());
		wire_str("\naarch64:   halting this core; a trap frame cannot be saved, and a fault while saving one has no bottom\n");
	}
	loop {
		unsafe { asm!("wfi", options(nomem, nostack, preserves_flags)) };
	}
}

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

	// A current-EL fault is a kernel bug, with ONE exception: an instruction the kernel declared may
	// fault, because it is copying to or from a userspace address another thread can unmap
	// underneath it. Those resume at their fixup with the copy reporting how far it got.
	//
	// Matched on the faulting instruction EXACTLY, never by range, so this can only rescue an
	// address a copy routine put in the table itself; anything else still halts, which is what keeps
	// a real kernel bug loud.
	//
	// The resume is written into the SAVED ELR in the trap frame, not into ELR_EL1.
	//
	// Writing the register looks right and is not: the stub above saved ELR_EL1 to the frame on the
	// way in (`stp x30, x2, [sp, #240]`) and `__trap_return` loads it back out before `eret`
	// (`ldp x30, x0, [sp, #240]` then `msr elr_el1, x0`). An `msr` here is overwritten on the way
	// out and the fault resumes at the faulting instruction - which faults again, forever. That is
	// what it did: the aarch64 suite stopped dead on the fixup test with no diagnostic, because a
	// livelock inside a fault handler produces no output at all.
	//
	// Offset 240 holds x30 and 248 holds the saved ELR, as the `stp` pair above lays them out.
	const FRAME_ELR: usize = 248 / 8;
	if let Some(fixup) = crate::extable::fixup_for(elr, far) {
		// CHECK the slot before writing it, because writing it blind is how this fails silently.
		//
		// The stub saved ELR_EL1 into this slot on the way in, so the slot must already hold `elr` -
		// the address this fault is resuming from. If it holds anything else then this offset is not
		// where the saved ELR lives on THIS entry path, and storing the fixup into it corrupts some
		// other saved register while leaving the resume address untouched. The faulting instruction is
		// then retried forever, and a fault handler that never returns prints NOTHING: the machine
		// goes silent and the suite times out with no diagnostic at all. That is exactly what
		// `a_process_load_whose_image_goes_away_is_an_error_rather_than_a_dead_kernel` does on this
		// target, which is why the check is here rather than in a comment.
		//
		// A mismatch is a kernel bug either way. Refusing it falls through to the report below and
		// halts loudly with both numbers, which is strictly better than resuming into a frame nobody
		// has verified.
		let saved = unsafe { *frame.add(FRAME_ELR) };
		if saved == elr {
			unsafe { *frame.add(FRAME_ELR) = fixup };
			return;
		}
		crate::serial_println!("aarch64: extable fixup for pc {elr:#x} REFUSED - frame slot holds {saved:#x} rather than the faulting ELR, so this frame is not laid out as the fixup path assumes");
	}

	// Anything else reaching here is a kernel bug: report it and halt.
	//
	// WITH THE LINK REGISTER AND THE FRAME POINTER, because the faulting PC is often not enough to
	// name anything. A generic monomorphised into a crate - `Result<&[u8; 8], _>::copied`, say - has
	// one address and a hundred and thirty call sites, so `ELR` resolves to a symbol that tells you
	// which OPERATION faulted and nothing about which code asked for it. `x30` is the caller's
	// return address, which names the call site outright, and `x29` is the frame pointer, which is
	// where a walk starts.
	//
	// The frame already holds both - `stp x28, x29, [sp, #224]` and `stp x30, x2, [sp, #240]` in
	// the prologue above - so this is reading numbers that were being saved and thrown away.
	const FRAME_X29: usize = 232 / 8;
	const FRAME_X30: usize = 240 / 8;
	let (fp, lr) = unsafe { (*frame.add(FRAME_X29), *frame.add(FRAME_X30)) };
	crate::serial_println!("aarch64 EXCEPTION [{source} {kind_str}] EC={ec:#x} ESR={esr:#x} FAR={far:#x} ELR={elr:#x}");
	crate::serial_println!("aarch64:   called from LR={lr:#x}, frame pointer x29={fp:#x}");
	crate::serial_println!("aarch64: unhandled exception - halting");
	super::halt_loop()
}
