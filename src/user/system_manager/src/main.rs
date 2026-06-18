// SystemManager - the first userspace process.
//
// The kernel loads this program from the init package into a fresh Process and
// drops it into ring 3 at `_start` with a bootstrap channel handle in rdi. For
// this milestone SystemManager simply reports in over that channel - the first
// real userspace-to-kernel IPC from a process loaded off the init package - and
// exits. Later milestones grow it into a standing service.

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

// Kernel syscall numbers from the shared abi crate (the single source of truth).
use abi::{SYS_CHANNEL_SEND, SYS_USER_EXIT};

// ELF entry point. The kernel sets the ELF entry to `_start` and enters ring 3
// here with the bootstrap channel handle in rdi. Align the stack to the SysV ABI
// boundary, then call the Rust entry (which keeps the handle in rdi).
global_asm!(".text", ".global _start", "_start:", "and rsp, -16", "call __sysmgr_main", "ud2");

// Report in to the kernel over the bootstrap channel, then exit.
#[no_mangle]
extern "C" fn __sysmgr_main(bootstrap: u64) -> ! {
	let message = b"SystemManager: online";
	unsafe {
		syscall(SYS_CHANNEL_SEND, bootstrap, message.as_ptr() as u64, message.len() as u64, 0);
		syscall(SYS_USER_EXIT, 0, 0, 0, 0);
	}
	loop {}
}

// Issue a syscall using the kernel's register convention: the number in rax and
// up to four arguments in rdi/rsi/rdx/r10; the `syscall` instruction clobbers rcx
// and r11. Returns the kernel's result from rax.
unsafe fn syscall(number: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
	let result: u64;
	asm!(
		"syscall",
		inlateout("rax") number => result,
		in("rdi") a0,
		in("rsi") a1,
		in("rdx") a2,
		in("r10") a3,
		lateout("rcx") _,
		lateout("r11") _,
		lateout("r8") _,
		lateout("r9") _,
	);
	result
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
	unsafe {
		syscall(SYS_USER_EXIT, 0, 0, 0, 0);
	}
	loop {}
}
