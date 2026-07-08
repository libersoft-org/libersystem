// Kernel thread context switching (aarch64).
//
// switch_context saves the callee-saved registers of the running thread onto its
// stack, stores the stack pointer into *old_sp, loads new_sp, restores the new
// thread's callee-saved registers, and returns into it. A brand-new thread is
// given a fabricated stack frame (see init_thread_stack) whose return address is
// thread_trampoline, so the first switch into it lands in Rust at thread_bootstrap.

#![allow(dead_code)]

use core::arch::{asm, global_asm};

unsafe extern "C" {
	// Save the current context to *old_sp and resume the context at new_sp.
	pub fn switch_context(old_sp: *mut u64, new_sp: u64);
	// Return target baked into a new thread's initial stack frame.
	fn thread_trampoline();
}

global_asm!(
	r#"
.text
.global switch_context
switch_context:
	// Save the callee-saved registers: x19..x28, x29 fp, x30 lr, and the
	// callee-saved FP registers d8..d15 (the kernel is built with FP, so these must
	// be preserved across a switch) - 160 bytes.
	stp     x19, x20, [sp, #-160]!
	stp     x21, x22, [sp, #16]
	stp     x23, x24, [sp, #32]
	stp     x25, x26, [sp, #48]
	stp     x27, x28, [sp, #64]
	stp     x29, x30, [sp, #80]
	stp     d8,  d9,  [sp, #96]
	stp     d10, d11, [sp, #112]
	stp     d12, d13, [sp, #128]
	stp     d14, d15, [sp, #144]
	mov     x2, sp
	str     x2, [x0]        // *old_sp = sp
	mov     sp, x1          // load the incoming stack pointer
	ldp     x19, x20, [sp, #0]
	ldp     x21, x22, [sp, #16]
	ldp     x23, x24, [sp, #32]
	ldp     x25, x26, [sp, #48]
	ldp     x27, x28, [sp, #64]
	ldp     x29, x30, [sp, #80]
	ldp     d8,  d9,  [sp, #96]
	ldp     d10, d11, [sp, #112]
	ldp     d12, d13, [sp, #128]
	ldp     d14, d15, [sp, #144]
	add     sp, sp, #160
	ret                     // return into the resumed thread (x30)

.global thread_trampoline
thread_trampoline:
	// The initial frame leaves the entry pointer in x19 and its argument in x20.
	mov     x0, x19
	mov     x1, x20
	bl      thread_bootstrap
	brk     #0
"#
);

// `thread_bootstrap` (the Rust entry the trampoline above calls) is the portable
// arch::common::context symbol - identical on every arch, so it lives there.

// Build the initial kernel stack for a new thread. The frame is laid out so the
// first switch_context into it restores entry into x19 and arg into x20, then
// returns into thread_trampoline. Returns the initial saved stack pointer.
pub fn init_thread_stack(stack: &mut [u8], entry: extern "C" fn(u64), arg: u64) -> u64 {
	let base = stack.as_mut_ptr() as u64;
	// 16-byte align the top so the trampoline runs with ABI-correct alignment.
	let top = (base + stack.len() as u64) & !0xf;
	let sp = top - 160;
	let trampoline: unsafe extern "C" fn() = thread_trampoline;
	unsafe {
		let frame = sp as *mut u64;
		frame.add(0).write(entry as usize as u64); // restored into x19
		frame.add(1).write(arg); // restored into x20
		frame.add(2).write(0); // x21
		frame.add(3).write(0); // x22
		frame.add(4).write(0); // x23
		frame.add(5).write(0); // x24
		frame.add(6).write(0); // x25
		frame.add(7).write(0); // x26
		frame.add(8).write(0); // x27
		frame.add(9).write(0); // x28
		frame.add(10).write(0); // x29 (fp)
		frame.add(11).write(trampoline as usize as u64); // x30 (return address)
		frame.add(12).write(0); // d8
		frame.add(13).write(0); // d9
		frame.add(14).write(0); // d10
		frame.add(15).write(0); // d11
		frame.add(16).write(0); // d12
		frame.add(17).write(0); // d13
		frame.add(18).write(0); // d14
		frame.add(19).write(0); // d15
	}
	sp
}

// Read the active page-table root (TTBR0_EL1) of the running core. Kept named
// `cr3` for the portable contract.
pub fn read_cr3() -> u64 {
	let value: u64;
	unsafe {
		asm!("mrs {}, ttbr0_el1", out(reg) value, options(nomem, nostack, preserves_flags));
	}
	value
}

// Load a page-table root into TTBR0_EL1, switching the active address space, and
// invalidate the TLB. The kernel half of every address space is identical, so
// kernel code and stacks remain mapped across the switch.
//
// SAFETY: `ttbr` must be the physical address of a valid L0 table whose kernel
// half maps the currently executing code and stack.
pub unsafe fn write_cr3(ttbr: u64) {
	unsafe {
		asm!(
			"msr ttbr0_el1, {ttbr}",
			"dsb ish",
			"tlbi vmalle1",
			"dsb ish",
			"isb",
			ttbr = in(reg) ttbr,
			options(nostack, preserves_flags),
		);
	}
}
