// Kernel thread context switching.
//
// switch_context saves the callee-saved registers of the running thread onto its
// stack, stores the stack pointer into *old_sp, loads new_sp, restores the new
// thread's callee-saved registers, and returns into it. A brand-new thread is
// given a fabricated stack frame (see init_thread_stack) whose return address is
// thread_trampoline, so the first switch into it lands in Rust at thread_bootstrap.

#![allow(dead_code)]

use core::arch::{asm, global_asm};

extern "C" {
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
	"mov [rdi], rsp",
	"mov rsp, rsi",
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

// First Rust code a freshly scheduled thread runs. Calls the thread entry with
// its argument; when the entry returns, the thread exits and never comes back.
#[no_mangle]
extern "C" fn thread_bootstrap(entry: u64, arg: u64) -> ! {
	let entry_fn: extern "C" fn(u64) = unsafe { core::mem::transmute(entry) };
	entry_fn(arg);
	crate::sched::exit()
}

// Build the initial kernel stack for a new thread. The frame is laid out so the
// first switch_context into it pops entry into r15 and arg into r14, then returns
// into thread_trampoline. Returns the initial saved stack pointer.
pub fn init_thread_stack(stack: &mut [u8], entry: extern "C" fn(u64), arg: u64) -> u64 {
	let base = stack.as_mut_ptr() as u64;
	// 16-byte align the top so the trampoline calls with ABI-correct alignment.
	let top = (base + stack.len() as u64) & !0xf;
	let sp = top - 7 * 8;
	let trampoline: unsafe extern "C" fn() = thread_trampoline;
	unsafe {
		let frame = sp as *mut u64;
		frame.add(0).write(entry as usize as u64); // restored into r15
		frame.add(1).write(arg); // restored into r14
		frame.add(2).write(0); // r13
		frame.add(3).write(0); // r12
		frame.add(4).write(0); // rbx
		frame.add(5).write(0); // rbp
		frame.add(6).write(trampoline as usize as u64); // return address
	}
	sp
}

// Read the active page-table root (CR3) of the running core.
pub fn read_cr3() -> u64 {
	let value: u64;
	unsafe {
		asm!("mov {}, cr3", out(reg) value, options(nomem, nostack, preserves_flags));
	}
	value
}
