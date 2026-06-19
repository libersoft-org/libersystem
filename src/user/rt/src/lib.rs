// Shared ring-3 userspace runtime: the entry stub, the syscall wrapper, the panic
// handler, and the small helpers the userspace programs need (the PKGARCH1 volume
// parser and the vol:// path parser). Every userspace program links this crate,
// so the boilerplate and the ABI surface live in exactly one place.
//
// Entry contract: the crate provides `_start` (the ELF entry the kernel jumps to
// in ring 3, with the bootstrap channel handle in rdi). It aligns the stack and
// calls `__user_main`, which each program must define:
//
//     #[no_mangle]
//     pub extern "C" fn __user_main(bootstrap: u64) -> ! { ... }

#![no_std]
#![allow(dead_code)]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

// Syscall numbers, error codes, and capability rights bits all come from the
// shared abi crate (the single source of truth), re-exported so the programs that
// link this runtime keep referring to them directly.
pub use abi::*;

// ELF entry: the kernel drops us into ring 3 here with the bootstrap channel
// handle in rdi. Align the stack to the SysV ABI boundary, then call the Rust
// entry the program defines (keeping the bootstrap handle in rdi).
global_asm!(".text", ".global _start", "_start:", "and rsp, -16", "call __user_main", "ud2");

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
	unsafe {
		syscall(SYS_USER_EXIT, 0, 0, 0, 0);
	}
	loop {}
}

// Issue a syscall: number in rax, up to four args in rdi/rsi/rdx/r10. The
// `syscall` instruction clobbers rcx and r11; the kernel also uses r8/r9. The
// result comes back in rax (a success value or a small negative error code).
pub unsafe fn syscall(number: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
	unsafe {
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
}

// Terminate this process. Never returns.
pub fn exit() -> ! {
	unsafe {
		syscall(SYS_USER_EXIT, 0, 0, 0, 0);
	}
	loop {}
}

// Yield the CPU to another runnable thread (used to spin on a would-block call
// without busy-waiting against the kernel).
pub unsafe fn yield_now() {
	unsafe {
		syscall(SYS_YIELD, 0, 0, 0, 0);
	}
}

// Block until the object behind `handle` becomes ready (a channel readable, an
// event signaled, a timer expired) or `deadline` (absolute ticks; 0 = no
// timeout) passes. Returns 0 when ready, a small negative error otherwise. This
// sleeps the thread at ~0% CPU instead of busy-yielding.
pub unsafe fn wait(handle: u64, deadline: u64) -> i64 {
	unsafe { syscall(SYS_WAIT, handle, deadline, 0, 0) as i64 }
}

// The outcome of a blocking receive.
pub enum Received {
	// A message arrived: `len` payload bytes were written to the buffer and a
	// transferred handle (0 = none) accompanied it.
	Message { len: usize, handle: u64 },
	// The peer endpoint is gone (or another error makes further progress
	// impossible); no more messages will arrive.
	Closed,
}

// Receive one message into `buf`, blocking while the channel is empty. Returns
// the payload length and any transferred handle, or Closed once the peer is gone.
pub unsafe fn recv_blocking(channel: u64, buf: &mut [u8]) -> Received {
	unsafe {
		let mut handle: u64 = 0;
		loop {
			let result: u64 = syscall(SYS_CHANNEL_RECV, channel, buf.as_mut_ptr() as u64, buf.len() as u64, &mut handle as *mut u64 as u64);
			let signed: i64 = result as i64;
			if signed == ERR_WOULD_BLOCK {
				// Block until the channel is readable (a message arrives or the peer
				// closes) rather than busy-yielding. No deadline: wait indefinitely.
				wait(channel, 0);
				continue;
			}
			if signed < 0 {
				return Received::Closed;
			}
			return Received::Message { len: signed as usize, handle };
		}
	}
}

// Send `bytes` (and optionally one transferred handle) to the peer, yielding
// while the queue is full. Returns true on delivery.
pub unsafe fn send_blocking(channel: u64, bytes: &[u8], xfer: u64) -> bool {
	unsafe {
		loop {
			let result: u64 = syscall(SYS_CHANNEL_SEND, channel, bytes.as_ptr() as u64, bytes.len() as u64, xfer);
			let signed: i64 = result as i64;
			if signed == ERR_WOULD_BLOCK {
				yield_now();
				continue;
			}
			return signed == 0;
		}
	}
}

// The PKGARCH1 archive reader (`abi::Package`, re-exported above via `pub use
// abi::*`) is the single decoder for the format, shared with the kernel.

// A canonical location on a volume: the resolver's view of a vol:// URI, split
// into the volume name and the path within it. The URI is just the wire form;
// this pair is what the StorageManager resolves against.
pub struct VolumePath<'a> {
	pub volume: &'a [u8],
	pub path: &'a [u8],
}

impl<'a> VolumePath<'a> {
	// Parse "vol://<volume>/<path>" into its components. Returns None if the
	// scheme is missing or either component is empty.
	pub fn parse(uri: &'a [u8]) -> Option<VolumePath<'a>> {
		const SCHEME: &[u8] = b"vol://";
		if uri.len() < SCHEME.len() || &uri[..SCHEME.len()] != SCHEME {
			return None;
		}
		let rest: &[u8] = &uri[SCHEME.len()..];
		let slash: usize = rest.iter().position(|&b: &u8| b == b'/')?;
		let volume: &[u8] = &rest[..slash];
		let path: &[u8] = &rest[slash + 1..];
		if volume.is_empty() || path.is_empty() {
			return None;
		}
		Some(VolumePath { volume, path })
	}
}
