// Fast system-call entry (the `syscall` instruction path).
//
// On `syscall` the CPU loads RIP from IA32_LSTAR, masks RFLAGS with IA32_FMASK,
// and loads CS/SS from IA32_STAR, saving the return RIP in RCX and RFLAGS in R11.
// syscall_entry (below) marshals the register-convention arguments into the SysV
// calling convention, calls the portable dispatcher, and returns to the caller.
//
// One entry point serves two callers. The kernel still issues `syscall` from
// ring 0 via invoke() (so tests exercise the real instruction); a ring-3 thread
// issues it after usermode::enter dropped it to user mode. The two are told
// apart by the saved return RIP in RCX: kernel code is higher-half (sign bit
// set), user code is lower-half. The ring-0 path stays on the caller's stack and
// returns by restoring RFLAGS and jumping to RCX. The ring-3 path switches to the
// thread's kernel stack (kept in the per-CPU block, reached through GS, which is
// not swapped because the user pages are supervisor-gated), runs the handler, and
// returns to ring 3 with `sysretq`.
//
// Register convention:
//   rax = syscall number          rax = return value
//   rdi, rsi, rdx, r10 = args 0..3 (r10 not rcx, since `syscall` clobbers rcx)

#![allow(dead_code)]

use core::arch::{asm, global_asm};

use super::msr;
use super::percpu;

// Kernel code selector (GDT layout: null, code = 0x08, data = 0x10).
const KERNEL_CS: u64 = 0x08;
// SYSRET selector base (STAR[63:48]); the CPU derives user SS/CS from it.
const SYSRET_BASE: u64 = super::gdt::USER_CODE32_SELECTOR as u64;

// MSRs that configure syscall/sysret.
const IA32_EFER: u32 = 0xc000_0080;
const IA32_STAR: u32 = 0xc000_0081;
const IA32_LSTAR: u32 = 0xc000_0082;
const IA32_FMASK: u32 = 0xc000_0084;

// EFER.SCE: enable the syscall/sysret instruction pair.
const EFER_SCE: u64 = 1 << 0;

// Flags cleared from RFLAGS on entry: trap, interrupt, direction, nested, align.
const FMASK: u64 = (1 << 8) | (1 << 9) | (1 << 10) | (1 << 14) | (1 << 18);

extern "C" {
	fn syscall_entry();
}

global_asm!(
	".text",
	".global syscall_entry",
	"syscall_entry:",
	// Entry: rcx = return rip, r11 = saved rflags, rax = number,
	// rdi/rsi/rdx/r10 = args. A kernel self-call has a higher-half (negative)
	// return rip; a userspace call has a lower-half (positive) one.
	"test rcx, rcx",
	"js 2f",
	// ring-3 path: park the user registers in the per-CPU block, switch to the
	// thread's kernel stack, and flag the syscall as user-originated.
	"mov gs:[{ursp}], rsp",
	"mov gs:[{urip}], rcx",
	"mov gs:[{urfl}], r11",
	"mov rsp, gs:[{krsp}]",
	"and rsp, -16",
	"mov qword ptr gs:[{fu}], 1",
	"mov r8, r10",
	"mov rcx, rdx",
	"mov rdx, rsi",
	"mov rsi, rdi",
	"mov rdi, rax",
	"call syscall_dispatch",
	"mov qword ptr gs:[{fu}], 0",
	// Restore the user registers and return to ring 3 (rip <- rcx, rflags <- r11).
	"mov rcx, gs:[{urip}]",
	"mov r11, gs:[{urfl}]",
	"mov rsp, gs:[{ursp}]",
	"sysretq",
	// ring-0 self-call path: already on a kernel stack, stay on it.
	"2:",
	"push rcx",
	"push r11",
	"push rbp",
	"mov rbp, rsp",
	"and rsp, -16",
	// Marshal (num, a0, a1, a2, a3) into the SysV argument registers. This order
	// never overwrites a source register before it has been read.
	"mov r8, r10",
	"mov rcx, rdx",
	"mov rdx, rsi",
	"mov rsi, rdi",
	"mov rdi, rax",
	"call syscall_dispatch",
	// rax holds the return value (kept for the syscall return register).
	"mov rsp, rbp",
	"pop rbp",
	"pop r11",
	"pop rcx",
	"push r11",
	"popfq",
	"jmp rcx",
	krsp = const percpu::KERNEL_RSP_OFFSET,
	ursp = const percpu::USER_RSP_OFFSET,
	urip = const percpu::USER_RIP_OFFSET,
	urfl = const percpu::USER_RFLAGS_OFFSET,
	fu = const percpu::FROM_USER_OFFSET,
);

// Program the current core's syscall MSRs. Per-core: called on the BSP and on
// every application processor during its bring-up.
pub fn init() {
	let efer = msr::read(IA32_EFER);
	msr::write(IA32_EFER, efer | EFER_SCE);
	// STAR[47:32] = kernel CS, so `syscall` loads CS = 0x08 and SS = 0x10.
	// STAR[63:48] = SYSRET base, so `sysretq` loads user SS = base+8 and
	// CS = base+16 (RPL forced to 3).
	msr::write(IA32_STAR, (SYSRET_BASE << 48) | (KERNEL_CS << 32));
	msr::write(IA32_LSTAR, syscall_entry as *const () as u64);
	msr::write(IA32_FMASK, FMASK);
}

// Issue a system call from kernel mode (ring 0). Exercises the real `syscall`
// instruction and the entry stub. Returns the value the handler left in RAX.
//
// SAFETY: performs a raw `syscall`; the per-core syscall MSRs must be initialized
// first (see init()). The handler runs with interrupts masked.
pub unsafe fn invoke(num: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
	let ret: u64;
	asm!(
		"syscall",
		inlateout("rax") num => ret,
		inlateout("rdi") a0 => _,
		inlateout("rsi") a1 => _,
		inlateout("rdx") a2 => _,
		inlateout("r10") a3 => _,
		lateout("rcx") _,
		lateout("r11") _,
		lateout("r8") _,
		lateout("r9") _,
	);
	ret
}
