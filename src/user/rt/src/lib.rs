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
use core::sync::atomic::{AtomicU64, Ordering};

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
		// If a stdout console channel is set, the program's terminal output goes there
		// (to the userspace ConsoleService, which renders it and mirrors it to serial)
		// as one message; otherwise it falls back to the kernel debug port byte by byte.
		let out: u64 = STDOUT.load(Ordering::Relaxed);
		if out != 0 && send_blocking(out, bytes, 0) {
			return;
		}
		for &b in bytes {
			syscall(SYS_DEBUG_WRITE, b as u64, 0, 0, 0);
		}
	}
}

// The console channel a program's `print` output is routed to (the ConsoleService
// end), or 0 for the kernel debug port. Set by `set_stdout`; inherited by spawned
// programs through their bootstrap.
static STDOUT: AtomicU64 = AtomicU64::new(0);

// Route this program's `print` output to a console channel (a ConsoleService client),
// instead of the kernel debug port. 0 restores the debug-port fallback.
pub fn set_stdout(channel: u64) {
	STDOUT.store(channel, Ordering::Relaxed);
}

// This program's current stdout console channel (0 = the kernel debug port). A
// launcher duplicates it to hand its children the same console.
pub fn stdout() -> u64 {
	STDOUT.load(Ordering::Relaxed)
}

// Adopt a stdout console channel a launcher sent as the first bootstrap message
// ("STDOUT" + the channel handle), so a spawned program's `print` output is routed to
// the same console as its parent. A no-op if the first message is not a STDOUT one
// (the handle 0 then restores the debug-port fallback).
pub unsafe fn inherit_stdout(bootstrap: u64) {
	unsafe {
		let mut buf: [u8; 16] = [0u8; 16];
		if let Received::Message { len, handle } = recv_blocking(bootstrap, &mut buf) {
			if len >= 6 && &buf[..6] == b"STDOUT" {
				set_stdout(handle);
			}
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

// Block until any handle in `handles` is ready, returning the index of the ready
// handle, or a negative error (ERR_TIMED_OUT at `deadline`; absolute ticks, 0 = no
// timeout). Lets a driver wait on its device interrupt and a control channel at
// once, waking on whichever fires first.
pub unsafe fn wait_any(handles: &[u64], deadline: u64) -> i64 {
	unsafe { syscall(SYS_WAIT_ANY, handles.as_ptr() as u64, handles.len() as u64, deadline, 0) as i64 }
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

// A non-blocking receive result: a message, an empty-but-open channel, or a closed
// one. Lets a poller (e.g. the shell reaping finished background jobs) tell an empty
// channel from a closed one without blocking.
pub enum Polled {
	Message { len: usize, handle: u64 },
	Empty,
	Closed,
}

// Receive one message without blocking: returns Empty immediately if the channel has
// nothing queued (and the peer is still open), Closed once the peer is gone, else the
// message. The shell polls a background job's channel this way to detect completion.
pub unsafe fn try_recv(channel: u64, buf: &mut [u8]) -> Polled {
	unsafe {
		let mut handle: u64 = 0;
		let result: u64 = syscall(SYS_CHANNEL_RECV, channel, buf.as_mut_ptr() as u64, buf.len() as u64, &mut handle as *mut u64 as u64);
		let signed: i64 = result as i64;
		if signed == ERR_WOULD_BLOCK {
			Polled::Empty
		} else if signed < 0 {
			Polled::Closed
		} else {
			Polled::Message { len: signed as usize, handle }
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

// The reserved control opcode a holder of a multi-client service's channel sends to
// mint its own independent client connection (the generic equivalent of
// NetworkService's typed `open`). `serve_multi` intercepts it before the typed
// dispatch and replies with a fresh client endpoint; the matching service endpoint
// joins its wait set. No typed interface uses opcode 0xffff, so it never collides
// with a real request.
pub const CONNECT_OP: u16 = 0xffff;

// Multi-client serve loop: like `serve`, but multiplexes a growing set of client
// channels (starting with `root`) with `wait_any`, so several independent clients
// share one service while each keeps its own request/reply stream. A `CONNECT_OP`
// request mints a fresh channel pair, keeps the service end in the set, and replies
// with the client end (so a holder of any channel here can spawn another connection);
// every other request is passed to `handle_request` - the generated `<iface>::dispatch`
// shape, plus the channel it arrived on as the first argument (for ops that stream
// out of band on that connection). A sub-client channel closing (or sending the empty
// quit sentinel) is dropped from the set; the loop ends when the root closes.
pub unsafe fn serve_multi<F>(root: u64, request: &mut [u8], reply: &mut [u8], mut handle_request: F)
where
	F: FnMut(u64, &[u8], u64, &mut [u8], &mut u64) -> Option<usize>,
{
	unsafe {
		let mut chans: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
		chans.push(root);
		while !chans.is_empty() {
			let ready: i64 = wait_any(&chans, 0);
			if ready < 0 {
				continue;
			}
			let idx: usize = ready as usize;
			let chan: u64 = chans[idx];
			match recv_blocking(chan, request) {
				// the empty quit sentinel: the root ends the service, a sub-client drops.
				Received::Message { len, .. } if len == 0 => {
					if idx == 0 {
						break;
					}
					close(chan);
					chans.swap_remove(idx);
				}
				Received::Message { len, handle } => {
					if len >= 2 && u16::from_le_bytes([request[0], request[1]]) == CONNECT_OP {
						// mint a fresh independent client connection for the caller.
						match channel() {
							Some((mine, theirs)) => {
								chans.push(mine);
								send_blocking(chan, &[], theirs);
							}
							None => {
								send_blocking(chan, &[], 0);
							}
						}
					} else {
						let mut reply_handle: u64 = 0;
						if let Some(n) = handle_request(chan, &request[..len], handle, reply, &mut reply_handle) {
							send_blocking(chan, &reply[..n], reply_handle);
						}
					}
				}
				Received::Closed => {
					if idx == 0 {
						break;
					}
					close(chan);
					chans.swap_remove(idx);
				}
			}
		}
	}
}

// Mint an independent client connection to a multi-client service reachable on
// `factory` (a channel served by `serve_multi`): send the reserved connect request
// and return the fresh client channel the service handed back, or None on failure.
pub unsafe fn service_connect(factory: u64) -> Option<u64> {
	unsafe {
		let req: [u8; 2] = CONNECT_OP.to_le_bytes();
		if !send_blocking(factory, &req, 0) {
			return None;
		}
		let mut buf: [u8; 16] = [0u8; 16];
		match recv_blocking(factory, &mut buf) {
			Received::Message { handle, .. } if handle != 0 => Some(handle),
			_ => None,
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

// Create a memory object of `size` bytes (a shared buffer), returning its handle or
// a negative error. Map it with `map_object` to fill or read its bytes; transfer the
// handle to hand the buffer to another process zero-copy.
pub unsafe fn memory_object_create(size: u64) -> i64 {
	unsafe { syscall(SYS_MEMORY_OBJECT_CREATE, size, 0, 0, 0) as i64 }
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

// Read the hardware real-time clock as a Unix timestamp (seconds since the epoch,
// UTC), or 0 if the RTC reports an implausible date. The wall-clock policy
// (NTP discipline, the monotonic combination) lives in the userspace TimeService.
pub unsafe fn clock_rtc() -> u64 {
	unsafe { syscall(SYS_CLOCK_RTC, 0, 0, 0, 0) }
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

// Acquire an MSI-X Interrupt capability for device `index`: the kernel allocates a
// per-device LAPIC vector, programs the device's MSI-X table entry, and enables
// MSI-X, so the driver gets its own edge-triggered interrupt. Returns the Interrupt
// handle, or a negative error. The driver `wait`s on the handle for its device, then
// `interrupt_ack`s it (a no-op clear for MSI) and writes its MSI-X vector into the
// virtio transport (set_msix_vector).
pub unsafe fn device_msix_acquire(index: u64) -> i64 {
	unsafe { syscall(SYS_DEVICE_MSIX_ACQUIRE, index, 0, 0, 0) as i64 }
}

// Acknowledge a serviced device interrupt, re-arming its source so the next `wait`
// on the Interrupt handle blocks until the device interrupts again.
pub unsafe fn interrupt_ack(handle: u64) {
	unsafe {
		syscall(SYS_INTERRUPT_ACK, handle, 0, 0, 0);
	}
}

// Inject one byte into the kernel console input, feeding the interactive shell the
// same way the kernel's serial loop does - used by the virtio-input keyboard driver.
pub unsafe fn console_feed(byte: u8) {
	unsafe {
		syscall(SYS_CONSOLE_FEED, byte as u64, 0, 0, 0);
	}
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

// Map the boot framebuffer into this process and read its geometry into `fb`,
// returning the mapped virtual base (the raw pixel buffer pointer), or a negative
// error. Hands the display to the caller - the kernel console stops drawing to it -
// so only the ConsoleService should call it (once; a second call fails).
pub unsafe fn framebuffer_map(fb: &mut Framebuffer) -> i64 {
	unsafe { syscall(SYS_FRAMEBUFFER_MAP, fb as *mut Framebuffer as u64, core::mem::size_of::<Framebuffer>() as u64, 0, 0) as i64 }
}

// The physical address backing byte `offset` of a DmaBuffer. A buffer larger than
// one page is mapped contiguously in virtual space but its physical frames are
// scattered, so a driver that carves it into several device buffers asks for each
// one's true physical address by its offset rather than adding to the base.
pub unsafe fn dma_buffer_phys_at(handle: u64, offset: u64) -> u64 {
	unsafe { syscall(SYS_DMA_BUFFER_PHYS, handle, offset, 0, 0) }
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
		// The caller drives the child through its Process handle; the thread handle is
		// not returned, so close it here rather than leaking it. A leaked thread handle
		// would hold the child's thread (and thus its Process and handle table) alive
		// even after the child exited cleanly, so a peer watching the child's channels
		// would never observe them close. The scheduler already holds the started thread.
		close(thread);
		process as i64
	}
}

// Deliver a signal to a process via its Process handle (which must carry the MANAGE
// right - `spawn` returns one that does). The kernel applies the default disposition:
// SIG_INT / SIG_TERM / SIG_KILL terminate it, SIG_STOP suspends it, SIG_CONT resumes a
// suspended one. Returns 0 on success or a negative error.
pub unsafe fn signal(process: u64, signal: u64) -> i64 {
	unsafe { syscall(SYS_PROCESS_SIGNAL, process, signal, 0, 0) as i64 }
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
