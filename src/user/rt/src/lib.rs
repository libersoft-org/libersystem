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

// The global allocator: a lazily-mapped per-process heap that enables `alloc`
// (Box, Vec, String) for programs that need it. Registers itself as the
// #[global_allocator]; dormant until the first allocation.
mod heap;

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

// Write `bytes` to the debug console one byte at a time. The only output path a
// ring-3 program has for now (a real console service arrives with virtio-console).
pub unsafe fn print(bytes: &[u8]) {
	unsafe {
		for &b in bytes {
			syscall(SYS_DEBUG_WRITE, b as u64, 0, 0, 0);
		}
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

// Create a channel pair, returning its two endpoint handles, or None on failure.
pub unsafe fn channel() -> Option<(u64, u64)> {
	unsafe {
		let mut a: u64 = 0;
		let mut b: u64 = 0;
		let result: u64 = syscall(SYS_CHANNEL_CREATE, &mut a as *mut u64 as u64, &mut b as *mut u64 as u64, 0, 0);
		if sys_is_err(result) {
			return None;
		}
		Some((a, b))
	}
}

// Read the monotonic clock (LAPIC ticks since boot), used to timestamp records.
pub unsafe fn clock() -> u64 {
	unsafe { syscall(SYS_CLOCK_GET, 0, 0, 0, 0) }
}

// Duplicate `handle` into a new handle carrying `rights` (a subset of the
// original's). Returns the new handle, or a negative error.
pub unsafe fn duplicate(handle: u64, rights: u32) -> i64 {
	unsafe { syscall(SYS_HANDLE_DUPLICATE, handle, rights as u64, 0, 0) as i64 }
}

// The number of devices the kernel discovered at boot.
pub unsafe fn device_count() -> u64 {
	unsafe { syscall(SYS_DEVICE_COUNT, 0, 0, 0, 0) }
}

// Read the DeviceInfo for device `index` (its virtio type and MMIO struct
// offsets). Returns true on success, false for an out-of-range index.
pub unsafe fn device_info(index: u64, info: &mut DeviceInfo) -> bool {
	unsafe { syscall(SYS_DEVICE_INFO, index, info as *mut DeviceInfo as u64, core::mem::size_of::<DeviceInfo>() as u64, 0) as i64 == 0 }
}

// Acquire a DeviceMemory capability for device `index`'s MMIO BAR, returning the
// handle, or a negative error. The driver maps it with device_memory_map.
pub unsafe fn device_acquire(index: u64) -> i64 {
	unsafe { syscall(SYS_DEVICE_ACQUIRE, index, 0, 0, 0) as i64 }
}

// Allocate a DmaBuffer of `size` bytes (pinned DMA memory charged to our Domain),
// returning its handle, or a negative error.
pub unsafe fn dma_buffer_create(size: u64) -> i64 {
	unsafe { syscall(SYS_DMA_BUFFER_CREATE, size, 0, 0, 0) as i64 }
}

// Map a DmaBuffer into our address space, returning its virtual base (or a
// negative error). The driver fills the virtqueue rings through this mapping.
pub unsafe fn dma_buffer_map(handle: u64) -> i64 {
	unsafe { syscall(SYS_DMA_BUFFER_MAP, handle, 0, 0, 0) as i64 }
}

// The physical base address of a DmaBuffer - the address a driver programs into
// its device for DMA.
pub unsafe fn dma_buffer_phys(handle: u64) -> u64 {
	unsafe { syscall(SYS_DMA_BUFFER_PHYS, handle, 0, 0, 0) }
}

// Spawn a new ring-3 process from an ELF image and start it. `bootstrap` is an
// object handle (0 = none) moved out of this process's table into the child's and
// delivered to the child's first thread in rdi - the way a process is endowed with
// its initial capability. Returns the child Process handle, or a negative error.
pub unsafe fn spawn(elf: &[u8], bootstrap: u64) -> i64 {
	unsafe {
		let process: u64 = syscall(SYS_PROCESS_CREATE, 0, 0, 0, 0);
		if sys_is_err(process) {
			return process as i64;
		}
		let entry: u64 = syscall(SYS_PROCESS_LOAD, process, elf.as_ptr() as u64, elf.len() as u64, 0);
		if sys_is_err(entry) {
			return entry as i64;
		}
		let thread: u64 = syscall(SYS_THREAD_CREATE, process, entry, USER_STACK_TOP, bootstrap);
		if sys_is_err(thread) {
			return thread as i64;
		}
		let started: u64 = syscall(SYS_THREAD_START, thread, 0, 0, 0);
		if sys_is_err(started) {
			return started as i64;
		}
		process as i64
	}
}

// The PKGARCH1 archive reader (`abi::Package`, re-exported above via `pub use
// abi::*`) is the single decoder for the format, shared with the kernel.

// A canonical location on a volume: the resolver's view of a vol:// URI, split
// into the volume name and the path within it. The URI is just the wire form;
// this pair is what the StorageService resolves against.
pub struct VolumePath<'a> {
	pub volume: &'a [u8],
	pub path: RelativePath<'a>,
}

impl<'a> VolumePath<'a> {
	// Parse "vol://<volume>/<path>" into its components. Returns None if the scheme
	// is missing, the volume is empty, or the path is not a valid relative path.
	pub fn parse(uri: &'a [u8]) -> Option<VolumePath<'a>> {
		const SCHEME: &[u8] = b"vol://";
		if uri.len() < SCHEME.len() || &uri[..SCHEME.len()] != SCHEME {
			return None;
		}
		let rest: &[u8] = &uri[SCHEME.len()..];
		let slash: usize = rest.iter().position(|&b: &u8| b == b'/')?;
		let volume: &[u8] = &rest[..slash];
		if volume.is_empty() {
			return None;
		}
		let path: RelativePath<'a> = RelativePath::parse(&rest[slash + 1..])?;
		Some(VolumePath { volume, path })
	}
}

// A path within a volume: a sequence of validated, non-empty segments separated by
// '/'. It is constructed only through validation, so it can never hold an empty
// segment, "." or ".." - path traversal has nowhere to arise. The authority to read
// is the capability, not the string; this is just the canonical name to resolve.
pub struct RelativePath<'a> {
	raw: &'a [u8],
}

impl<'a> RelativePath<'a> {
	// Validate `raw` as a relative path: one or more '/'-separated segments, each
	// non-empty and neither "." nor "..", with no NUL or backslash byte. Returns
	// None if any segment is invalid.
	pub fn parse(raw: &'a [u8]) -> Option<RelativePath<'a>> {
		if raw.is_empty() {
			return None;
		}
		for seg in raw.split(|&b: &u8| b == b'/') {
			if seg.is_empty() || seg == b"." || seg == b".." {
				return None;
			}
			for &c in seg {
				if c == 0 || c == b'\\' {
					return None;
				}
			}
		}
		Some(RelativePath { raw })
	}

	// The path's canonical bytes, e.g. for an exact archive lookup.
	pub fn as_bytes(&self) -> &'a [u8] {
		self.raw
	}

	// The path's segments, in order.
	pub fn segments(&self) -> impl Iterator<Item = &'a [u8]> {
		self.raw.split(|&b: &u8| b == b'/')
	}
}
