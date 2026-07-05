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

// Write `bytes` to the debug console. The only output path a ring-3 program has when no
// stdout console channel is set (the real console service routes through that channel).
pub unsafe fn print(bytes: &[u8]) {
	unsafe {
		// If a stdout console channel is set, the program's terminal output goes there
		// (to the userspace ConsoleService, which renders it and mirrors it to serial)
		// as one message; otherwise it falls back to the kernel debug port in bulk -
		// one syscall per chunk, not one per byte (the per-byte path stalled the console
		// service's serial mirror, and the gpu present queued behind it, for ~500 ms).
		let out: u64 = STDOUT.load(Ordering::Relaxed);
		if out != 0 && send_blocking(out, bytes, 0) {
			return;
		}
		for chunk in bytes.chunks(DEBUG_WRITE_CHUNK) {
			// The kernel reports how much its transmit ring accepted; once a chunk does
			// not fit whole, sending more would only punch out-of-order holes, so the
			// remainder is dropped (print is fire-and-forget; a caller that must not
			// lose bytes paces itself with debug_write).
			let accepted: i64 = syscall(SYS_DEBUG_WRITE, chunk.as_ptr() as u64, chunk.len() as u64, 0, 0) as i64;
			if accepted < chunk.len() as i64 {
				return;
			}
		}
	}
}

// Write bytes to the kernel debug/serial port, returning how many the kernel's
// transmit ring accepted (one syscall, no chunk loop). A caller draining a backlog -
// the console's serial mirror - consumes exactly that many and retries the rest on a
// later wake, so a burst is paced instead of truncated.
pub unsafe fn debug_write(bytes: &[u8]) -> usize {
	unsafe {
		let n = bytes.len().min(DEBUG_WRITE_CHUNK);
		let accepted: i64 = syscall(SYS_DEBUG_WRITE, bytes.as_ptr() as u64, n as u64, 0, 0) as i64;
		if accepted < 0 { 0 } else { accepted as usize }
	}
}

// Largest slice handed to one bulk SYS_DEBUG_WRITE, matching the kernel's cap (the
// serial TX ring size). Longer output is chunked across several syscalls.
const DEBUG_WRITE_CHUNK: usize = 16384;

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

// The console channel a program reads its input (stdin) from, or 0 for none. A
// controlling terminal is full-duplex - the same channel carries the program's output
// and its keyboard input - so a foreground launch hands the child one SEND|RECV dup of
// the console and `inherit_stdout` points both stdout and stdin at it. A background or
// non-interactive launch grants only SEND, so its stdin stays 0 (no input). Set by
// `set_stdin`; inherited through the bootstrap alongside stdout.
static STDIN: AtomicU64 = AtomicU64::new(0);

// Route this program's input reads (`read_line`) to a console channel (its controlling
// terminal). 0 restores no-input (reads return end-of-input at once).
pub fn set_stdin(channel: u64) {
	STDIN.store(channel, Ordering::Relaxed);
}

// This program's current stdin console channel (0 = no input).
pub fn stdin() -> u64 {
	STDIN.load(Ordering::Relaxed)
}

// Lightweight cross-process perf tracing for latency hunting, off by default. `perf_mark`
// emits a `\x1ePERF <label> <tsc>` line straight to the kernel debug serial (NOT the
// console channel), so a marker reaches the host trace tool (boot/perf-trace.py) without
// touching the framebuffer or the console render path. The TSC is read in ring 3 (rdtsc is
// unprivileged) and is a global cycle clock shared by every process and the kernel, so
// markers from the shell, the console service, and the gpu driver sit on one timeline; the
// kernel publishes the TSC frequency at boot (the `tsc_hz` marker) so the host converts
// cycles to time. Flip PERF to true and drop perf_mark calls around the path under study;
// false compiles every marker out.
pub const PERF: bool = false;

// Read the time-stamp counter for a perf marker (the same cycle clock the kernel times
// with). The lfence keeps the read from being reordered ahead of the work it brackets.
#[inline]
pub fn perf_now() -> u64 {
	let lo: u32;
	let hi: u32;
	unsafe {
		asm!("lfence", "rdtsc", out("eax") lo, out("edx") hi, options(nostack, preserves_flags));
	}
	((hi as u64) << 32) | lo as u64
}

// Emit one perf marker (`\x1ePERF <label> <tsc>\n`) to the kernel debug serial. The
// timestamp is taken first so the bulk-syscall emit never inflates it; keep markers
// coarse (not inside tight inner loops) to keep that emit cost off the path.
pub fn perf_mark(label: &[u8]) {
	if !PERF {
		return;
	}
	let tsc = perf_now();
	let mut line = [0u8; 96];
	let mut n = 0usize;
	line[n] = 0x1e; // ASCII record separator: flags a perf line for the host parser
	n += 1;
	for &b in b"PERF " {
		line[n] = b;
		n += 1;
	}
	for &b in label {
		if n >= line.len() - 24 {
			break;
		}
		line[n] = b;
		n += 1;
	}
	line[n] = b' ';
	n += 1;
	let mut digits = [0u8; 20];
	let mut d = 0usize;
	let mut v = tsc;
	if v == 0 {
		digits[d] = b'0';
		d += 1;
	}
	while v > 0 {
		digits[d] = b'0' + (v % 10) as u8;
		d += 1;
		v /= 10;
	}
	while d > 0 {
		d -= 1;
		line[n] = digits[d];
		n += 1;
	}
	line[n] = b'\n';
	n += 1;
	unsafe {
		syscall(SYS_DEBUG_WRITE, line.as_ptr() as u64, n as u64, 0, 0);
	}
}

// Like `perf_mark` but also records a decimal value (a spin/yield count, a byte size,
// ...): emits `\x1ePERF <label> <tsc> <val>\n`. The host parser shows the value next to
// the marker so a slow phase can be correlated with what it was doing (e.g. how many
// times a queue submit yielded while waiting for the device).
pub fn perf_mark_val(label: &[u8], val: u64) {
	if !PERF {
		return;
	}
	let tsc = perf_now();
	let mut line = [0u8; 128];
	let mut n = 0usize;
	line[n] = 0x1e;
	n += 1;
	for &b in b"PERF " {
		line[n] = b;
		n += 1;
	}
	for &b in label {
		if n >= line.len() - 48 {
			break;
		}
		line[n] = b;
		n += 1;
	}
	for v in [tsc, val] {
		line[n] = b' ';
		n += 1;
		let mut digits = [0u8; 20];
		let mut d = 0usize;
		let mut x = v;
		if x == 0 {
			digits[d] = b'0';
			d += 1;
		}
		while x > 0 {
			digits[d] = b'0' + (x % 10) as u8;
			d += 1;
			x /= 10;
		}
		while d > 0 {
			d -= 1;
			line[n] = digits[d];
			n += 1;
		}
	}
	line[n] = b'\n';
	n += 1;
	unsafe {
		syscall(SYS_DEBUG_WRITE, line.as_ptr() as u64, n as u64, 0, 0);
	}
}

// Adopt a stdout console channel a launcher sent as the first bootstrap message
// ("STDOUT" + the channel handle), so a spawned program's `print` output is routed to
// the same console as its parent. The console is the program's controlling terminal, a
// full-duplex channel, so the same handle becomes its stdin too: a foreground launch
// grants it SEND|RECV and `read_line` then reads input from it; a background launch
// grants only SEND, so reads return end-of-input. A no-op if the first message is not a
// STDOUT one (the handle 0 then restores the debug-port fallback and leaves stdin empty).
pub unsafe fn inherit_stdout(bootstrap: u64) {
	unsafe {
		let mut buf: [u8; 16] = [0u8; 16];
		if let Received::Message { len, handle } = recv_blocking(bootstrap, &mut buf) {
			if len >= 6 && &buf[..6] == b"STDOUT" {
				set_stdout(handle);
				set_stdin(handle);
			}
		}
	}
}

// Read one cooked line of input from this program's stdin (its controlling terminal):
// blocks until the terminal delivers a submitted line, copies it (with its trailing
// newline) into `buf` and returns its length, or None at end-of-input - Ctrl+D, the
// terminal closing, or no stdin at all (a background / non-interactive launch). Only a
// foreground program reads here; the line discipline in ConsoleService edits and echoes
// the input exactly as it does for the shell's own prompt.
pub unsafe fn read_line(buf: &mut [u8]) -> Option<usize> {
	unsafe {
		let inp: u64 = STDIN.load(Ordering::Relaxed);
		if inp == 0 {
			return None;
		}
		match recv_blocking(inp, buf) {
			// A zero-byte read is the tty's EOF (Ctrl+D on an empty line).
			Received::Message { len: 0, .. } => None,
			Received::Message { len, .. } => Some(len),
			Received::Closed => None,
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

// `wait` whose deadline is a recurring housekeeping wake (WAIT_PERIODIC): the
// kernel still wakes the caller when it is due, but the wait never counts as
// pending progress - the scheduler's boot driver settles across it, so a service
// may tick forever (a display poll, a blink) without stalling boot or the tests.
pub unsafe fn wait_periodic(handle: u64, deadline: u64) -> i64 {
	unsafe { syscall(SYS_WAIT, handle, deadline, WAIT_PERIODIC, 0) as i64 }
}

// Block until any handle in `handles` is ready, returning the index of the ready
// handle, or a negative error (ERR_TIMED_OUT at `deadline`; absolute ticks, 0 = no
// timeout). Lets a driver wait on its device interrupt and a control channel at
// once, waking on whichever fires first.
pub unsafe fn wait_any(handles: &[u64], deadline: u64) -> i64 {
	unsafe { syscall(SYS_WAIT_ANY, handles.as_ptr() as u64, handles.len() as u64, deadline, 0) as i64 }
}

// `wait_any` whose deadline is a recurring housekeeping wake (see wait_periodic).
pub unsafe fn wait_any_periodic(handles: &[u64], deadline: u64) -> i64 {
	unsafe { syscall(SYS_WAIT_ANY, handles.as_ptr() as u64, handles.len() as u64, deadline, WAIT_PERIODIC) as i64 }
}

// Non-blocking check of whether the object behind `handle` is ready right now - a
// channel readable, a process terminated, an event signaled - without sleeping.
// Waits against the current instant as the deadline, so the kernel runs its readiness
// check and returns at once rather than blocking. The shell reaps finished background
// jobs with it: a terminated child's Process handle reads ready. The handle needs the
// WAIT right.
pub unsafe fn poll_ready(handle: u64) -> bool {
	unsafe { wait(handle, clock().max(1)) == 0 }
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

// Try to send `bytes` (and optionally one transferred handle) without blocking: returns
// true on delivery, false if the queue is full (WOULD_BLOCK) or the peer is gone. Used
// for droppable traffic - e.g. mouse reports to a program that may not be reading them.
pub unsafe fn try_send(channel: u64, bytes: &[u8], xfer: u64) -> bool {
	unsafe {
		let result: u64 = syscall(SYS_CHANNEL_SEND, channel, bytes.as_ptr() as u64, bytes.len() as u64, xfer);
		result == 0
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

// The bootstrap handshake terminator: a parent sends its named capabilities, then
// READY; the child collects the whole set with recv_caps and takes each capability
// by name - no ordering contract, no placeholder sends for capabilities a child
// does not get.
pub const BOOTSTRAP_READY: &[u8] = b"READY";

// A received bootstrap capability set: every named capability the parent sent
// before READY, taken by name. Whatever the receiver does not take is closed when
// the set drops, so an unused grant never lingers as an open handle.
pub struct CapSet {
	entries: alloc::vec::Vec<(alloc::vec::Vec<u8>, u64)>,
}

impl CapSet {
	// Take the named capability out of the set: its handle, or 0 when the parent did
	// not send it (or sent it with no handle).
	pub fn take(&mut self, name: &[u8]) -> u64 {
		match self.entries.iter().position(|(n, _)| n == name) {
			Some(i) => self.entries.swap_remove(i).1,
			None => 0,
		}
	}
}

impl Drop for CapSet {
	fn drop(&mut self) {
		for &(_, handle) in self.entries.iter() {
			if handle != 0 {
				unsafe { close(handle) };
			}
		}
	}
}

// Receive a parent's whole bootstrap capability set: named capability messages up
// to the READY terminator (or the channel closing). The counterpart of send_ready.
pub unsafe fn recv_caps(bootstrap: u64) -> CapSet {
	unsafe {
		let mut entries: alloc::vec::Vec<(alloc::vec::Vec<u8>, u64)> = alloc::vec::Vec::new();
		let mut buf: [u8; 64] = [0u8; 64];
		loop {
			match recv_blocking(bootstrap, &mut buf) {
				Received::Message { len, handle } => {
					let name: &[u8] = &buf[..len];
					if name == BOOTSTRAP_READY {
						break;
					}
					entries.push((name.to_vec(), handle));
				}
				Received::Closed => break,
			}
		}
		CapSet { entries }
	}
}

// End a bootstrap capability handshake: the child's recv_caps returns once this
// terminator arrives.
pub unsafe fn send_ready(bootstrap: u64) -> bool {
	unsafe { send_blocking(bootstrap, BOOTSTRAP_READY, 0) }
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
					// the reserved heartbeat probe: answer it uniformly without invoking the
					// typed dispatch, so the supervisor's watchdog can prove this service is
					// still responsive (a service wedged inside a request never returns here
					// to answer, so the probe times out - that is how a hang is detected).
					if len >= 2 && u16::from_le_bytes([request[0], request[1]]) == HEARTBEAT_OP {
						send_blocking(service, b"PONG", 0);
						continue;
					}
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

// The reserved control opcode the supervisor (ServiceManager) sends over a service's
// channel to probe its liveness: serve / serve_multi answer it with a fixed b"PONG"
// WITHOUT invoking the typed dispatch, so every service answers a heartbeat uniformly.
// A service wedged inside a request never returns to its serve loop to answer, so the
// probe times out - the watchdog's hung (alive-but-unresponsive) detection. No typed
// interface uses opcode 0xfffe, so it never collides with a real request.
pub const HEARTBEAT_OP: u16 = 0xfffe;

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
pub unsafe fn serve_multi<F>(root: u64, request: &mut [u8], reply: &mut [u8], handle_request: F)
where
	F: FnMut(u64, &[u8], u64, &mut [u8], &mut u64) -> Option<usize>,
{
	unsafe {
		serve_multi_seeded(root, &[], request, reply, handle_request);
	}
}

// Like `serve_multi`, but pre-seeds the client set with `seed` channels - service ends the
// server already holds connections to (a self-connection a manager mints to a copy of itself,
// for instance), served exactly like ones minted on demand via `CONNECT_OP`. As with any
// sub-client, a seed channel closing (or sending the empty quit sentinel) is simply dropped
// from the set; only `root` closing ends the service.
pub unsafe fn serve_multi_seeded<F>(root: u64, seed: &[u64], request: &mut [u8], reply: &mut [u8], mut handle_request: F)
where
	F: FnMut(u64, &[u8], u64, &mut [u8], &mut u64) -> Option<usize>,
{
	unsafe {
		let mut chans: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
		chans.push(root);
		chans.extend_from_slice(seed);
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
					if len >= 2 && u16::from_le_bytes([request[0], request[1]]) == HEARTBEAT_OP {
						// the reserved heartbeat probe: answer uniformly, like serve (above).
						send_blocking(chan, b"PONG", 0);
					} else if len >= 2 && u16::from_le_bytes([request[0], request[1]]) == CONNECT_OP {
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

// Probe a service for liveness over `channel` (a connection the supervisor holds to
// it): send the reserved heartbeat opcode and wait up to `deadline` (absolute ticks,
// 0 = forever) for the pong. Returns true if the service answered in time (alive and
// responsive), false on a timeout (hung) or a closed channel. The watchdog's
// hung-detection: a service wedged inside a request never returns to answer the probe.
pub unsafe fn heartbeat(channel: u64, deadline: u64) -> bool {
	unsafe {
		let req: [u8; 2] = HEARTBEAT_OP.to_le_bytes();
		if !send_blocking(channel, &req, 0) {
			return false;
		}
		if wait(channel, deadline) != 0 {
			return false;
		}
		let mut buf: [u8; 8] = [0u8; 8];
		matches!(try_recv(channel, &mut buf), Polled::Message { .. })
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

// Read the monotonic clock in nanoseconds since boot (the calibrated TSC), for
// measuring latencies finer than a `clock()` tick - an IPC round-trip, a ping RTT.
pub unsafe fn clock_ns() -> u64 {
	unsafe { syscall(SYS_CLOCK_MONO_NS, 0, 0, 0, 0) }
}

// Read the online CPU set: fills `ids` with one LAPIC id per core (as many as fit)
// and returns the core count. A free syscall feeding the `lscpu` inventory command.
pub unsafe fn cpu_info(ids: &mut [u32]) -> i64 {
	unsafe { syscall(SYS_CPU_INFO, ids.as_mut_ptr() as u64, ids.len() as u64 * 4, 0, 0) as i64 }
}

// Read the physical-memory and kernel-heap totals into `stats`. A free syscall
// feeding the `free` inventory command.
pub unsafe fn memory_stats(stats: &mut MemoryStats) -> i64 {
	unsafe { syscall(SYS_MEMORY_STATS, stats as *mut MemoryStats as u64, core::mem::size_of::<MemoryStats>() as u64, 0, 0) as i64 }
}

// Read the boot memory-map region at `index` into `region`, returning the region
// count (negative past the end). A free syscall feeding the `lsmem` command.
pub unsafe fn memmap_get(index: u64, region: &mut MemmapRegion) -> i64 {
	unsafe { syscall(SYS_MEMMAP_GET, index, region as *mut MemmapRegion as u64, core::mem::size_of::<MemmapRegion>() as u64, 0) as i64 }
}

// Read the device-interrupt vector state at `index` into `info`, returning the
// vector count (negative past the end). A free syscall feeding the `lsirq` command.
pub unsafe fn irq_info(index: u64, info: &mut IrqInfo) -> i64 {
	unsafe { syscall(SYS_IRQ_INFO, index, info as *mut IrqInfo as u64, core::mem::size_of::<IrqInfo>() as u64, 0) as i64 }
}

// Read the retained PCI function at `index` into `info`, returning the function
// count (negative past the end). A free syscall feeding the `lspci` command.
pub unsafe fn pci_info(index: u64, info: &mut PciInfo) -> i64 {
	unsafe { syscall(SYS_PCI_INFO, index, info as *mut PciInfo as u64, core::mem::size_of::<PciInfo>() as u64, 0) as i64 }
}

// Arm this process to catch Ctrl+C (SIG_INT): once armed, an interrupt sets a pending
// flag `interrupted()` polls instead of terminating us, so a long-running tool can
// stop cleanly and print a summary. Ctrl+\ (SIG_TERM) still force-quits.
pub unsafe fn catch_interrupt() {
	unsafe {
		syscall(SYS_SIGNAL_CATCH, SIG_INT, 0, 0, 0);
	}
}

// Poll and clear a pending caught interrupt (Ctrl+C): true if one arrived since the
// last call. Only meaningful after `catch_interrupt()` has armed the process.
pub unsafe fn interrupted() -> bool {
	unsafe { syscall(SYS_SIGNAL_TAKE, SIG_INT, 0, 0, 0) != 0 }
}

// Duplicate `handle` into a new handle carrying `rights` (a subset of the
// original's). Returns the new handle, or a negative error.
pub unsafe fn duplicate(handle: u64, rights: u32) -> i64 {
	unsafe { syscall(SYS_HANDLE_DUPLICATE, handle, rights as u64, 0, 0) as i64 }
}

// Introspect the object behind `handle`: its koid, stable type code (Process = 1,
// ...), the rights the handle confers, its generation, and its byte size for
// memory-backed objects. Returns None if the handle is unknown.
pub unsafe fn object_info(handle: u64) -> Option<ObjectInfo> {
	unsafe {
		let mut info: ObjectInfo = ObjectInfo { koid: 0, object_type: 0, rights: 0, generation: 0, size: 0 };
		let size: u64 = core::mem::size_of::<ObjectInfo>() as u64;
		let ok: i64 = syscall(SYS_OBJECT_INFO_GET, handle, &mut info as *mut ObjectInfo as u64, size, 0) as i64;
		if ok == 1 { Some(info) } else { None }
	}
}

// Read the live per-process counters and state behind a Process `handle` (the IPC
// volume it has done, the handles and user memory it holds, and its liveness). The
// handle must carry RIGHT_READ. Returns None if the handle is unknown or not a
// process; the SystemGraphService uses this to build the live observability graph
// from the process handles it holds for each component.
pub unsafe fn process_stats(handle: u64) -> Option<ProcessStats> {
	unsafe {
		let mut stats: ProcessStats = ProcessStats { messages_sent: 0, messages_received: 0, handle_count: 0, memory_bytes: 0, state: 0 };
		let size: u64 = core::mem::size_of::<ProcessStats>() as u64;
		let ok: i64 = syscall(SYS_PROCESS_STATS_GET, handle, &mut stats as *mut ProcessStats as u64, size, 0) as i64;
		if ok == 1 { Some(stats) } else { None }
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

// Reboot or power off the machine: `action` is POWER_REBOOT or POWER_OFF. On success
// the machine resets / powers off and this never returns; a negative error otherwise.
pub unsafe fn system_power(action: u64) -> i64 {
	unsafe { syscall(SYS_SYSTEM_POWER, action, 0, 0, 0) as i64 }
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

// Copy the kernel boot console's log text into `buf`, returning the number of bytes
// written (0 when there is no boot console). The kernel and the ConsoleService share
// the same `term` stack, so the boot log is handed across as logical text and replayed
// into VT 1's model at takeover - it stays on screen and in the scrollback afterwards.
pub unsafe fn console_readlog(buf: &mut [u8]) -> i64 {
	unsafe { syscall(SYS_CONSOLE_READLOG, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0) as i64 }
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
	unsafe { spawn_in(elf, bootstrap, 0) }
}

// Spawn a new ring-3 process from an ELF image into a Domain and start it, like
// `spawn` but accounting the child to `domain` (0 = the spawner's own Domain). The
// Domain handle must carry the MANAGE right; the child's image, stack and every
// allocation it later makes are charged to that Domain (and its ancestors), so a
// manager can launch a governed component under a bounded sub-Domain it controls and
// the kernel contains the component's resource use to that Domain. Returns the child
// Process handle, or a negative error.
pub unsafe fn spawn_in(elf: &[u8], bootstrap: u64, domain: u64) -> i64 {
	unsafe {
		let process: u64 = syscall(SYS_PROCESS_CREATE, domain, 0, 0, 0);
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

// Create a child Domain of the caller's Domain with the given resource caps (memory
// and IPC/DMA in bytes, handles and threads as counts; u64::MAX = uncapped) and
// return its handle, or a negative error. The child's limits bind in addition to
// every ancestor's, so it can only subdivide its parent's budget. A ResourceManager
// makes one of these to host a governed component, then sets and adjusts its caps
// with `domain_set_limit` and observes usage with `domain_stats`.
pub unsafe fn domain_create(memory: u64, handles: u64, threads: u64) -> i64 {
	unsafe { syscall(SYS_DOMAIN_CREATE, memory, handles, threads, 0) as i64 }
}

// Set one resource counter's limit on a Domain (handle must carry the MANAGE right):
// `prop` is one of the PROP_*_LIMIT selectors and `limit` the new cap (u64::MAX =
// uncapped). Returns 0 on success or a negative error. The cap takes effect at once -
// the next over-budget allocation in the Domain fails with ERR_RESOURCE_EXHAUSTED.
pub unsafe fn domain_set_limit(domain: u64, prop: u64, limit: u64) -> i64 {
	unsafe { syscall(SYS_OBJECT_PROPERTY_SET, domain, prop, limit, 0) as i64 }
}

// Read the live per-Domain resource counters behind a Domain `handle` (the used and
// limit of memory, handles, threads, IPC queue bytes and DMA). The handle must carry
// RIGHT_READ. Returns None if the handle is unknown or not a Domain; a ResourceManager
// uses this to observe a governed Domain's usage against the budgets it set.
pub unsafe fn domain_stats(handle: u64) -> Option<DomainStats> {
	unsafe {
		let mut stats: DomainStats = DomainStats::default();
		let size: u64 = core::mem::size_of::<DomainStats>() as u64;
		let ok: i64 = syscall(SYS_DOMAIN_STATS_GET, handle, &mut stats as *mut DomainStats as u64, size, 0) as i64;
		if ok == 1 { Some(stats) } else { None }
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
