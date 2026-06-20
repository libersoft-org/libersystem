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

extern crate alloc;

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

// Receive one message and, if its payload begins with `tag` and it carried a
// transferred handle, return that handle. None if the channel closed, no handle
// accompanied the message, or the payload did not begin with `tag`. This is the
// "expect a tagged capability over a bootstrap/control channel" handshake the
// programs share (e.g. recv_tagged(bootstrap, &mut buf, b"SERVE")).
pub unsafe fn recv_tagged(channel: u64, buf: &mut [u8], tag: &[u8]) -> Option<u64> {
	unsafe {
		match recv_blocking(channel, buf) {
			Received::Message { len, handle } if handle != 0 && len >= tag.len() && &buf[..tag.len()] == tag => Some(handle),
			_ => None,
		}
	}
}

// Receive a "PACKAGE" message - the tag, a u64 little-endian byte length, and a
// transferred MemoryObject holding a PKGARCH1 archive - then map the object and
// return its kept handle plus the mapped archive bytes. The archive stays mapped
// for the life of the process (these consumers never unmap it), so the slice is
// handed back with a 'static lifetime. None if the message is not a well-formed
// PACKAGE or the mapping fails.
pub unsafe fn recv_package(channel: u64, buf: &mut [u8]) -> Option<(u64, &'static [u8])> {
	unsafe {
		match recv_blocking(channel, buf) {
			Received::Message { len, handle } if handle != 0 && len >= 7 + 8 && &buf[..7] == b"PACKAGE" => {
				let length: usize = u64::from_le_bytes([buf[7], buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14]]) as usize;
				let base: u64 = map_object(handle)?;
				let archive: &'static [u8] = core::slice::from_raw_parts(base as *const u8, length);
				Some((handle, archive))
			}
			_ => None,
		}
	}
}

// Run a service's request/reply loop over `service`: receive each request, hand it
// to `handle_request` (the generated `<iface>::dispatch` shape - request bytes plus
// the transferred handle, a reply buffer plus an out-param for the reply's handle),
// and send back the reply it produced. A zero-length message is the explicit quit
// sentinel; the loop also ends when the client side closes. `request` and `reply`
// are the caller's scratch buffers, whose sizes bound the largest request/reply. A
// request that produces no reply (the closure returns None) is simply not answered -
// which is how a streaming op that replies out of band opts out of the byte reply.
pub unsafe fn serve<F>(service: u64, request: &mut [u8], reply: &mut [u8], mut handle_request: F)
where
	F: FnMut(&[u8], u64, &mut [u8], &mut u64) -> Option<usize>,
{
	unsafe {
		loop {
			match recv_blocking(service, request) {
				Received::Message { len, .. } if len == 0 => break,
				Received::Message { len, handle } => {
					let mut reply_handle: u64 = 0;
					if let Some(n) = handle_request(&request[..len], handle, reply, &mut reply_handle) {
						send_blocking(service, &reply[..n], reply_handle);
					}
				}
				Received::Closed => break,
			}
		}
	}
}

// A proto Transport over an rt channel: send the request (with any out-of-band
// handle), then block for the reply (whose own out-of-band handle is returned
// alongside the bytes). Every userspace program that drives a generated service
// client - the shell, the supervisor, the demo clients - reaches its service
// through this one implementation, instead of each repeating the send/recv glue.
pub struct ChannelTransport {
	pub chan: u64,
}

impl proto::codec::Transport for ChannelTransport {
	fn call(&mut self, request: &[u8], request_handle: u64) -> Option<(alloc::vec::Vec<u8>, u64)> {
		unsafe {
			if !send_blocking(self.chan, request, request_handle) {
				return None;
			}
			let mut reply: [u8; 4096] = [0u8; 4096];
			match recv_blocking(self.chan, &mut reply) {
				Received::Message { len, handle } => Some((reply[..len].to_vec(), handle)),
				Received::Closed => None,
			}
		}
	}
}

// Close `handle`, releasing the object reference it names. A no-op-safe wrapper
// over the close syscall (the kernel ignores an unknown handle), so callers need
// not repeat the raw syscall.
pub unsafe fn close(handle: u64) {
	unsafe {
		syscall(SYS_HANDLE_CLOSE, handle, 0, 0, 0);
	}
}

// Map the object behind `handle` into our address space (the kernel picks the
// virtual base), returning that base, or None on failure.
pub unsafe fn map_object(handle: u64) -> Option<u64> {
	unsafe {
		let base: u64 = syscall(SYS_MEMORY_MAP, handle, 0, 0, 0);
		if sys_is_err(base) { None } else { Some(base) }
	}
}

// Unmap the object behind `handle` from our address space.
pub unsafe fn unmap_object(handle: u64) {
	unsafe {
		syscall(SYS_MEMORY_UNMAP, handle, 0, 0, 0);
	}
}

// Read a shared buffer capability into `dst`: map the object behind `file`, copy up
// to `dst.len()` of its `size` bytes into `dst`, then unmap and close it. Returns
// the number of bytes copied, or None if the mapping fails (the handle is still
// closed). The canonical "consume a handed-back file handle" path.
pub unsafe fn read_into(file: u64, size: u64, dst: &mut [u8]) -> Option<usize> {
	unsafe {
		let mapped: u64 = match map_object(file) {
			Some(base) => base,
			None => {
				close(file);
				return None;
			}
		};
		let n: usize = (size as usize).min(dst.len());
		core::ptr::copy_nonoverlapping(mapped as *const u8, dst.as_mut_ptr(), n);
		unmap_object(file);
		close(file);
		Some(n)
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

// Introspect the object behind `handle`: its koid, stable type code (Process = 1,
// ...), the rights the handle confers, and its generation. Returns None if the
// handle is unknown.
pub unsafe fn object_info(handle: u64) -> Option<ObjectInfo> {
	unsafe {
		let mut info: ObjectInfo = ObjectInfo { koid: 0, object_type: 0, rights: 0, generation: 0 };
		let size: u64 = core::mem::size_of::<ObjectInfo>() as u64;
		let ok: i64 = syscall(SYS_OBJECT_INFO_GET, handle, &mut info as *mut ObjectInfo as u64, size, 0) as i64;
		if ok == 1 { Some(info) } else { None }
	}
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

// Allocate a DmaBuffer of `size` bytes and map it, returning its (handle, virtual
// base, physical base): a driver fills the ring/buffer through `virt` and points its
// device at `phys`. None on allocation or mapping failure. The returned handle keeps
// the pinned buffer alive for the life of the driver.
pub unsafe fn dma_buffer(size: u64) -> Option<(u64, u64, u64)> {
	unsafe {
		let handle: i64 = dma_buffer_create(size);
		if handle < 0 {
			return None;
		}
		let virt: i64 = dma_buffer_map(handle as u64);
		if sys_is_err(virt as u64) {
			return None;
		}
		let phys: u64 = dma_buffer_phys(handle as u64);
		Some((handle as u64, virt as u64, phys))
	}
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
