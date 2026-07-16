// Kernel thread context switching.
//
// switch_context saves the callee-saved registers of the running thread onto its
// stack, stores the stack pointer into *old_sp, loads new_sp, restores the new
// thread's callee-saved registers, and returns into it. A brand-new thread is
// given a fabricated stack frame (see init_thread_stack) whose return address is
// thread_trampoline, so the first switch into it lands in Rust at thread_bootstrap.
// The 512-byte FXSAVE image preserves x87, MMX, XMM0..15 and MXCSR eagerly on every
// switch; it travels on the thread's own kernel stack, including across preemption.

#![allow(dead_code)]

use core::arch::{asm, global_asm};

unsafe extern "C" {
	// Save the current context to *old_sp and resume the context at new_sp.
	pub fn switch_context(old_sp: *mut u64, new_sp: u64);
	// Return target baked into a new thread's initial stack frame.
	fn thread_trampoline();
}

global_asm!(
	".text",
	".global switch_context",
	"switch_context:",
	"push rbp",
	"push rbx",
	"push r12",
	"push r13",
	"push r14",
	"push r15",
	// Save the GPR-frame pointer, then reserve enough room to align a 512-byte
	// FXSAVE image down to 16 bytes independently of the entry-stack convention.
	"mov rax, rsp",
	"sub rsp, 527",
	"and rsp, -16",
	"fxsave64 [rsp]",
	"mov [rsp + 512], rax",
	"mov [rdi], rsp",
	"mov rsp, rsi",
	"fxrstor64 [rsp]",
	"mov rsp, [rsp + 512]",
	"pop r15",
	"pop r14",
	"pop r13",
	"pop r12",
	"pop rbx",
	"pop rbp",
	"ret",
	".global thread_trampoline",
	"thread_trampoline:",
	// The initial frame leaves the entry pointer in r15 and its argument in r14.
	"mov rdi, r15",
	"mov rsi, r14",
	"and rsp, -16",
	"call thread_bootstrap",
	"ud2",
);

// `thread_bootstrap` (the Rust entry the trampoline above calls) is the portable
// arch::common::context symbol - identical on every arch, so it lives there.

// Build the initial kernel stack for a new thread. The frame is laid out so the
// first switch_context into it pops entry into r15 and arg into r14, restores a
// canonical x87/SSE state, then returns into thread_trampoline. Returns the initial
// saved stack pointer.
pub fn init_thread_stack(stack: &mut [u8], entry: extern "C" fn(u64), arg: u64) -> u64 {
	let base = stack.as_mut_ptr() as u64;
	// 16-byte align the top so the trampoline calls with ABI-correct alignment.
	let top = (base + stack.len() as u64) & !0xf;
	let gpr_sp = top - 7 * 8;
	let sp = (gpr_sp - 527) & !0xf;
	let trampoline: unsafe extern "C" fn() = thread_trampoline;
	unsafe {
		let frame = gpr_sp as *mut u64;
		frame.add(0).write(entry as usize as u64); // restored into r15
		frame.add(1).write(arg); // restored into r14
		frame.add(2).write(0); // r13
		frame.add(3).write(0); // r12
		frame.add(4).write(0); // rbx
		frame.add(5).write(0); // rbp
		frame.add(6).write(trampoline as usize as u64); // return address
		// Canonical FXSAVE image lives at the aligned saved SP. Everything is zero
		// except the x87 control word and MXCSR reset values; offset 512 points to
		// the GPR frame restored after FXRSTOR.
		let fx = sp as *mut u8;
		core::ptr::write_bytes(fx, 0, 520);
		fx.cast::<u16>().write(0x037f);
		fx.add(24).cast::<u32>().write(0x1f80);
		fx.add(512).cast::<u64>().write(gpr_sp);
	}
	sp
}

// Enable x87 and SSE execution on the current core. The scheduler eagerly saves
// their complete architectural state, so CR0.TS stays clear (no lazy #NM path).
pub fn enable_fpu() {
	unsafe {
		let mut cr0: u64;
		asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
		cr0 &= !((1 << 2) | (1 << 3)); // EM=0, TS=0
		cr0 |= (1 << 1) | (1 << 5); // MP=1, NE=1
		asm!("mov cr0, {}", in(reg) cr0, options(nostack, preserves_flags));

		let mut cr4: u64;
		asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
		cr4 |= (1 << 9) | (1 << 10); // OSFXSR, OSXMMEXCPT
		asm!("mov cr4, {}", in(reg) cr4, options(nostack, preserves_flags));
		asm!("fninit", options(nomem, nostack));
		let mxcsr = 0x1f80u32;
		asm!("ldmxcsr [{}]", in(reg) &mxcsr, options(readonly, nostack));
	}
}

// Read the active page-table root (CR3) of the running core.
pub fn read_cr3() -> u64 {
	let value: u64;
	unsafe {
		asm!("mov {}, cr3", out(reg) value, options(nomem, nostack, preserves_flags));
	}
	value
}

// Load a page-table root into CR3, switching the active address space. This
// flushes all non-global TLB entries. The kernel half of every address space is
// identical, so kernel code and stacks remain mapped across the switch.
//
// SAFETY: `cr3` must be the physical address of a valid PML4 whose kernel half
// maps the currently executing code and stack.
pub unsafe fn write_cr3(cr3: u64) {
	unsafe {
		asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags));
	}
}
