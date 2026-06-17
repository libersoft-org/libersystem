// Ring 3 (userspace) entry and exit.
//
// `enter` drops the calling kernel thread into ring 3 at a user entry point with
// a user stack. It first saves the thread's callee-saved registers and parks the
// resulting kernel stack pointer in the per-CPU block (KERNEL_RSP), then builds an
// `iretq` frame with the user code/data selectors and returns into user mode.
//
// The thread comes back when user code invokes SYS_USER_EXIT, whose handler calls
// `exit_to_kernel`: a one-way longjmp that restores the parked kernel stack, pops
// the callee-saved registers `enter` pushed, and returns to `enter`'s caller. The
// pair behaves like setjmp/longjmp across the user-mode excursion.
//
// GS is deliberately not swapped across the ring boundary: the user thread keeps
// the kernel's GS base, which is safe because the per-CPU block lives in
// supervisor-only pages, so a ring-3 `gs:`-relative access would fault. That lets
// the syscall entry stub reach the kernel stack pointer through GS without swapgs.

#![allow(dead_code)]

use core::arch::global_asm;

use super::gdt;
use super::percpu;

// User selectors with RPL 3 (loaded by iretq into CS/SS).
const USER_CS: u64 = (gdt::USER_CODE64_SELECTOR | 3) as u64;
const USER_DS: u64 = (gdt::USER_DATA_SELECTOR | 3) as u64;
// Initial user RFLAGS: only IF (interrupts enabled) and the reserved bit 1 set.
const USER_RFLAGS: u64 = 0x202;

// Address the embedded fault-probe program writes to. It is intentionally left
// unmapped in every address space, so the write raises a page fault with this
// value in CR2. Tests assert the recorded fault address matches.
pub const FAULT_PROBE_ADDR: u64 = 0x0dea_d000;

extern "C" {
	fn user_enter(entry: u64, user_stack: u64, arg: u64);
	fn user_return() -> !;
	fn user_program_start();
	fn user_program_end();
	fn user_fault_program_start();
	fn user_fault_program_end();
}

// Drop the calling thread into ring 3 at `entry` with `user_stack` and `arg` (the
// arg is delivered to user code in rdi). Returns when the user thread calls
// SYS_USER_EXIT or faults out.
//
// SAFETY: `entry` and `user_stack` must be valid, USER-mapped addresses; the
// per-CPU syscall MSRs must already be programmed.
pub unsafe fn enter(entry: u64, user_stack: u64, arg: u64) {
	// The excursion ends by longjmping through `user_return`, which unwinds with a
	// plain `ret` rather than `iretq`. That restores the callee-saved registers
	// but not RFLAGS, so the interrupt flag - cleared by the syscall or exception
	// entry that ended the excursion - would stay masked on return. Capture the
	// caller's interrupt state and restore it afterwards so the excursion is
	// transparent.
	let rflags: u64;
	core::arch::asm!("pushfq", "pop {}", out(reg) rflags, options(preserves_flags));
	user_enter(entry, user_stack, arg);
	if rflags & (1 << 9) != 0 {
		super::enable_interrupts();
	}
}

// Return from ring 3 back to the kernel thread that called `enter`. Invoked by
// the SYS_USER_EXIT handler; never returns to its own caller.
pub fn exit_to_kernel() -> ! {
	unsafe { user_return() }
}

// The bytes of the embedded ring-3 test program (position-independent machine
// code, copied into a USER page before entering).
pub fn program_bytes() -> &'static [u8] {
	let start = user_program_start as *const () as usize;
	let end = user_program_end as *const () as usize;
	unsafe { core::slice::from_raw_parts(start as *const u8, end - start) }
}

// The bytes of the embedded ring-3 fault-probe program (position-independent
// machine code, copied into a USER page before entering). It writes to an
// unmapped address to raise a page fault from ring 3.
pub fn program_fault_bytes() -> &'static [u8] {
	let start = user_fault_program_start as *const () as usize;
	let end = user_fault_program_end as *const () as usize;
	unsafe { core::slice::from_raw_parts(start as *const u8, end - start) }
}

global_asm!(
	".text",
	".global user_enter",
	"user_enter:",
	// rdi = entry, rsi = user stack top, rdx = bootstrap arg.
	"push rbp",
	"push rbx",
	"push r12",
	"push r13",
	"push r14",
	"push r15",
	"mov gs:[{krsp}], rsp",
	// Build the iretq frame (pushed high to low: SS, RSP, RFLAGS, CS, RIP).
	"push {uds}",
	"push rsi",
	"push {rflags}",
	"push {ucs}",
	"push rdi",
	// Deliver the bootstrap arg in rdi; clear everything else so no kernel
	// register value leaks into ring 3.
	"mov rdi, rdx",
	"xor rsi, rsi",
	"xor rdx, rdx",
	"xor rax, rax",
	"xor rbx, rbx",
	"xor rbp, rbp",
	"xor rcx, rcx",
	"xor r8, r8",
	"xor r9, r9",
	"xor r10, r10",
	"xor r11, r11",
	"xor r12, r12",
	"xor r13, r13",
	"xor r14, r14",
	"xor r15, r15",
	"iretq",
	".global user_return",
	"user_return:",
	"mov qword ptr gs:[{fu}], 0",
	"mov rsp, gs:[{krsp}]",
	"pop r15",
	"pop r14",
	"pop r13",
	"pop r12",
	"pop rbx",
	"pop rbp",
	"ret",
	krsp = const percpu::KERNEL_RSP_OFFSET,
	fu = const percpu::FROM_USER_OFFSET,
	uds = const USER_DS,
	ucs = const USER_CS,
	rflags = const USER_RFLAGS,
);

// Embedded ring-3 test program. Position-independent: it uses only immediates,
// rsp-relative scratch, and the syscall ABI, so it runs unchanged at whatever
// USER virtual address it is copied to.
//
// On entry rdi holds a bootstrap Channel handle. It sends an "OK" message over
// the channel (a capability-gated syscall + IPC), prints one character through
// SYS_DEBUG_WRITE, and exits back to the kernel.
global_asm!(
	".text",
	".global user_program_start",
	"user_program_start:",
	// SYS_CHANNEL_SEND(handle = rdi, bytes = rsp, len = 2, xfer = 0).
	"sub rsp, 16",
	"mov word ptr [rsp], 0x4b4f", // 'O', 'K'
	"mov rsi, rsp",
	"mov edx, 2",
	"xor r10d, r10d",
	"mov eax, {send}",
	"syscall",
	"add rsp, 16",
	// SYS_DEBUG_WRITE('U').
	"mov edi, 0x55",
	"mov eax, {write}",
	"syscall",
	// SYS_USER_EXIT - returns control to the kernel; should not come back.
	"mov eax, {exit}",
	"syscall",
	"3:",
	"jmp 3b",
	".global user_program_end",
	"user_program_end:",
	send = const crate::syscall::SYS_CHANNEL_SEND,
	write = const crate::syscall::SYS_DEBUG_WRITE,
	exit = const crate::syscall::SYS_USER_EXIT,
);

// Embedded ring-3 fault-probe program. Position-independent: it writes to an
// unmapped address, which raises a page fault from ring 3. The kernel records the
// fault and terminates the process, so control never returns into this code; the
// trailing spin is only a guard against running off the end.
global_asm!(
	".text",
	".global user_fault_program_start",
	"user_fault_program_start:",
	"mov rax, {addr}",
	"mov qword ptr [rax], rax",
	"2:",
	"jmp 2b",
	".global user_fault_program_end",
	"user_fault_program_end:",
	addr = const FAULT_PROBE_ADDR,
);
