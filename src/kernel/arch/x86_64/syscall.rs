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

use core::arch::global_asm;

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

unsafe extern "C" {
	fn syscall_entry();
}

global_asm!(
	".text",
	".global syscall_entry",
	"syscall_entry:",
	// Entry: rcx = return rip, r11 = saved rflags, rax = number,
	// rdi/rsi/rdx/r10 = args.
	//
	// This stub is reached from RING 3 AND FROM NOWHERE ELSE, so there is nothing to
	// decide here. It used to branch on the sign of the return rip - a higher-half
	// address meant a kernel self-call - and that was a hole rather than a heuristic:
	// nothing stopped ring-3 code from RUNNING at a higher-half address, and a process
	// that arranged to be there was handed the kernel path. That path does not switch to
	// the kernel stack and sets `from_user = 0`, after which every `user_buf_ok` in the
	// kernel returns true for any address at all. Origin must never be inferred from an
	// address the caller controls.
	//
	// The kernel's own `invoke` calls `syscall_dispatch` directly now, exactly as the
	// AArch64 and RISC-V ports have always done, so the `syscall` instruction is a ring-3
	// interface only and this stub can treat every entry as untrusted.
	//
	// ring-3 path: switch to the thread's kernel stack and save the user return
	// state on it. Keeping rsp/rip/rflags on this thread's own stack (rather than a
	// per-CPU slot) lets the handler yield to another cooperative ring-3 service on
	// the same core without the other syscall clobbering this thread's return state.
	"mov r9, rsp",
	"mov rsp, gs:[{krsp}]",
	"and rsp, -16",
	"push r9",
	"push r11",
	"push rcx",
	"push rdi",
	"push rsi",
	"push rdx",
	"push r10",
	"sub rsp, 8",
	"mov qword ptr gs:[{fu}], 1",
	"mov r8, r10",
	"mov rcx, rdx",
	"mov rdx, rsi",
	"mov rsi, rdi",
	"mov rdi, rax",
	"call syscall_dispatch",
	"mov qword ptr gs:[{fu}], 0",
	// Restore the user registers from this thread's stack and return to ring 3
	// (rip <- rcx, rflags <- r11, rsp <- the saved user stack pointer).
	"add rsp, 8",
	"pop r10",
	"pop rdx",
	"pop rsi",
	"pop rdi",
	"pop rcx",
	"pop r11",
	"pop rsp",
	"sysretq",
	krsp = const percpu::KERNEL_RSP_OFFSET,
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

// Issue a system call from kernel mode (ring 0): straight to the portable syscall
// table, the way the in-kernel callers and the test harness use it, and the way the
// AArch64 and RISC-V ports have always done it.
//
// It used to execute a real `syscall` instruction and rely on the entry stub telling
// ring 0 from ring 3 by the sign of the return address. That is the hole documented on
// the stub above: an address the caller controls cannot establish who the caller is. A
// ring-0 caller reaching a ring-3 entry point is not a thing to detect - it is a thing to
// make impossible, and one plain function call does that.
//
// `from_user = false` is set here for the same reason the other two ports set it: the
// buffer checks must accept kernel-owned buffers, and a prior ring-3 syscall that yielded
// may have left the per-CPU flag set.
//
// SAFETY: kept `unsafe` for its callers' sake - the syscall table it reaches expects the
// arguments a syscall's ABI defines, and passing the wrong ones is as unsound here as it
// would be from ring 3.
#[cfg(test)]
pub unsafe fn invoke(num: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
	super::percpu::set_from_user(false);
	crate::syscall::syscall_dispatch(num, a0, a1, a2, a3)
}
