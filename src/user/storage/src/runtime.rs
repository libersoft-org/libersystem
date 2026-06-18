// Shared userspace runtime for the storage programs: the ring-3 entry stub, the
// syscall wrapper, the panic handler, and the small helpers both the
// StorageManager and its client need (the LIBERPK1 volume parser and the vol://
// path parser). Each binary includes this module independently, so every program
// carries its own copy of these items.

#![allow(dead_code)]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

// Syscall numbers, error codes, and capability rights bits all come from the
// shared abi crate (the single source of truth), re-exported so this module
// and the binaries that include it keep referring to them directly.
pub use abi::*;

// ELF entry: the kernel drops us into ring 3 here with the bootstrap channel
// handle in rdi. Align the stack to the SysV ABI boundary, then call the Rust
// entry the binary defines (keeping the bootstrap handle in rdi).
global_asm!(".text", ".global _start", "_start:", "and rsp, -16", "call __storage_main", "ud2");

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
	syscall(SYS_YIELD, 0, 0, 0, 0);
}

// Block until the object behind `handle` becomes ready (a channel readable, an
// event signaled, a timer expired) or `deadline` (absolute ticks; 0 = no
// timeout) passes. Returns 0 when ready, a small negative error otherwise. This
// sleeps the thread at ~0% CPU instead of busy-yielding.
pub unsafe fn wait(handle: u64, deadline: u64) -> i64 {
	syscall(SYS_WAIT, handle, deadline, 0, 0) as i64
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

// Receive one message into `buf`, yielding while the channel is empty. Returns
// the payload length and any transferred handle, or Closed once the peer is gone.
pub unsafe fn recv_blocking(channel: u64, buf: &mut [u8]) -> Received {
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

// Send `bytes` (and optionally one transferred handle) to the peer, yielding
// while the queue is full. Returns true on delivery.
pub unsafe fn send_blocking(channel: u64, bytes: &[u8], xfer: u64) -> bool {
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

// The LIBERPK1 archive-layout constants (PKG_MAGIC / PKG_HEADER_LEN / ...) come
// from the shared abi crate, re-exported above.

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
	let slice: &[u8] = bytes.get(at..at + 4)?;
	Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

// Look up `name` in a LIBERPK1 archive, returning its blob. Every access is
// bounds-checked, so a malformed or truncated archive yields None rather than
// reading out of range.
pub fn pkg_lookup<'a>(archive: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
	if archive.len() < PKG_HEADER_LEN || &archive[0..8] != PKG_MAGIC {
		return None;
	}
	let count: usize = read_u32(archive, 8)? as usize;
	for i in 0..count {
		let base: usize = PKG_HEADER_LEN + i * PKG_ENTRY_LEN;
		let name_field: &[u8] = archive.get(base..base + PKG_NAME_LEN)?;
		let entry_name: &[u8] = match name_field.iter().position(|&b: &u8| b == 0) {
			Some(end) => &name_field[..end],
			None => name_field,
		};
		let offset: usize = read_u32(archive, base + PKG_NAME_LEN)? as usize;
		let size: usize = read_u32(archive, base + PKG_NAME_LEN + 4)? as usize;
		if entry_name == name {
			return archive.get(offset..offset + size);
		}
	}
	None
}

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
