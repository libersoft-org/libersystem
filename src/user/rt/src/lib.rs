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
#![cfg_attr(feature = "shared-image", no_builtins)]

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

// ELF entry: the kernel drops us into ring 3 / EL0 here with the bootstrap channel
// handle in the first argument register. Align the stack to the ABI boundary, then
// call the runtime entry (keeping the bootstrap handle in that register).
#[cfg(all(target_arch = "x86_64", not(feature = "shared-image")))]
global_asm!(".text", ".global _start", "_start:", "and rsp, -16", "call __rt_start", "ud2");

// aarch64: the kernel enters EL0 with the bootstrap handle in x0. SP is already
// 16-aligned; clear the frame pointer and call the runtime entry.
#[cfg(all(target_arch = "aarch64", not(feature = "shared-image")))]
global_asm!(".text", ".global _start", "_start:", "mov x29, xzr", "bl __rt_start", "brk #0");

// riscv64: the kernel enters U-mode with the bootstrap handle in a0 and sp at the
// user stack top. Align sp to the 16-byte ABI boundary, clear the frame pointer (s0),
// and call the runtime entry (a0 preserved).
#[cfg(all(target_arch = "riscv64", not(feature = "shared-image")))]
global_asm!(".text", ".global _start", "_start:", "andi sp, sp, -16", "mv s0, zero", "call __rt_start", "ebreak");

// The runtime entry the assembly stub calls: verify the kernel's ABI matches the one
// this binary was built against, then hand control to the program's `__user_main` with
// the bootstrap handle still in rdi.
//
// The ABI handshake: a binary built against a different kernel ABI - a renumbered
// syscall, a grown struct - is refused here, before it issues a single call against a
// mismatched table, instead of running on and misbehaving. New syscalls only ever append
// and old ones never renumber, so SYS_ABI_CHECK and this comparison stay valid across
// revisions; a match is silent, a mismatch prints a clear line and exits.
#[unsafe(no_mangle)]
#[cfg(not(feature = "shared-image"))]
pub extern "C" fn __rt_start(bootstrap: u64) -> ! {
	unsafe {
		if sys_is_err(syscall(SYS_ABI_CHECK, ABI_VERSION as u64, 0, 0, 0)) {
			print(b"rt: refusing to run - built against a different kernel ABI revision\n");
			exit();
		}
		__user_main(bootstrap)
	}
}

#[cfg(not(feature = "shared-image"))]
unsafe extern "C" {
	// Each program defines this (a `#[no_mangle] pub extern "C" fn __user_main`); the
	// runtime's `__rt_start` calls it once the ABI handshake passes.
	fn __user_main(bootstrap: u64) -> !;
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
	unsafe {
		syscall(SYS_USER_EXIT, 0, 0, 0, 0);
	}
	loop {}
}

#[cfg(feature = "shared-image")]
#[unsafe(no_mangle)]
pub static __rust_no_alloc_shim_is_unstable_v2: u8 = 0;

#[cfg(feature = "shared-image")]
#[unsafe(no_mangle)]
pub extern "C" fn __rust_alloc_error_handler(_size: usize, _align: usize) -> ! {
	exit()
}

#[cfg(feature = "shared-image")]
#[unsafe(export_name = "_RNvCshfEkAwg4zv6_7___rustc35___rust_no_alloc_shim_is_unstable_v2")]
pub static RUST_NO_ALLOC_SHIM_ALIAS: u8 = 0;

#[cfg(feature = "shared-image")]
#[unsafe(export_name = "_RNvCshfEkAwg4zv6_7___rustc26___rust_alloc_error_handler")]
pub extern "C" fn rust_alloc_error_handler_alias(size: usize, align: usize) -> ! {
	__rust_alloc_error_handler(size, align)
}

#[cfg(feature = "shared-image")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn liber_memcpy_impl(destination: *mut u8, source: *const u8, len: usize) -> *mut u8 {
	unsafe {
		for index in 0..len {
			destination.add(index).write(source.add(index).read());
		}
	}
	destination
}

#[cfg(feature = "shared-image")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn liber_memset_impl(destination: *mut u8, value: i32, len: usize) -> *mut u8 {
	unsafe {
		for index in 0..len {
			destination.add(index).write(value as u8);
		}
	}
	destination
}

#[cfg(feature = "shared-image")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn liber_memcmp_impl(left: *const u8, right: *const u8, len: usize) -> i32 {
	unsafe {
		for index in 0..len {
			let left = left.add(index).read();
			let right = right.add(index).read();
			if left != right {
				return left as i32 - right as i32;
			}
		}
	}
	0
}

#[cfg(all(feature = "shared-image", target_arch = "x86_64"))]
global_asm!(".global memcpy", ".type memcpy,@function", "memcpy:", "jmp liber_memcpy_impl", ".global memset", ".type memset,@function", "memset:", "jmp liber_memset_impl", ".global memcmp", ".type memcmp,@function", "memcmp:", "jmp liber_memcmp_impl",);

#[cfg(all(feature = "shared-image", target_arch = "aarch64"))]
global_asm!(".global memcpy", ".type memcpy,%function", "memcpy:", "b liber_memcpy_impl", ".global memset", ".type memset,%function", "memset:", "b liber_memset_impl", ".global memcmp", ".type memcmp,%function", "memcmp:", "b liber_memcmp_impl",);

#[cfg(all(feature = "shared-image", target_arch = "riscv64"))]
global_asm!(".global memcpy", ".type memcpy,@function", "memcpy:", "tail liber_memcpy_impl", ".global memset", ".type memset,@function", "memset:", "tail liber_memset_impl", ".global memcmp", ".type memcmp,@function", "memcmp:", "tail liber_memcmp_impl",);

// Issue a syscall: number in rax, up to four args in rdi/rsi/rdx/r10. The
// `syscall` instruction clobbers rcx and r11; the kernel also uses r8/r9. The
// result comes back in rax (a success value or a small negative error code).
#[cfg(target_arch = "x86_64")]
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

// aarch64: number in x8, up to four args in x0..x3, result back in x0 (the SVC
// trap path). SVC preserves the general registers, so nothing else is clobbered.
#[cfg(target_arch = "aarch64")]
pub unsafe fn syscall(number: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
	unsafe {
		let result: u64;
		asm!(
			"svc #0",
			in("x8") number,
			inlateout("x0") a0 => result,
			in("x1") a1,
			in("x2") a2,
			in("x3") a3,
			options(nostack),
		);
		result
	}
}

// riscv64: number in a7, up to four args in a0..a3, result back in a0 (the ecall
// trap path). ecall preserves the general registers, so nothing else is clobbered.
#[cfg(target_arch = "riscv64")]
pub unsafe fn syscall(number: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
	unsafe {
		let result: u64;
		asm!(
			"ecall",
			in("a7") number,
			inlateout("a0") a0 => result,
			in("a1") a1,
			in("a2") a2,
			in("a3") a3,
			options(nostack),
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
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn perf_now() -> u64 {
	let lo: u32;
	let hi: u32;
	unsafe {
		asm!("lfence", "rdtsc", out("eax") lo, out("edx") hi, options(nostack, preserves_flags));
	}
	((hi as u64) << 32) | lo as u64
}

// aarch64: the virtual count register is the same monotonic cycle clock; the isb
// keeps the read from being reordered ahead of the bracketed work.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn perf_now() -> u64 {
	let cnt: u64;
	unsafe {
		asm!("isb", "mrs {}, cntvct_el0", out(reg) cnt, options(nostack, preserves_flags));
	}
	cnt
}

// riscv64: the cycle CSR is the monotonic per-hart cycle clock (U-mode reads it via
// rdcycle, which the kernel permits by setting SCOUNTEREN.CY). A fence keeps the read
// from being reordered ahead of the bracketed work.
#[cfg(target_arch = "riscv64")]
#[inline]
pub fn perf_now() -> u64 {
	let cnt: u64;
	unsafe {
		asm!("fence", "rdcycle {}", out(reg) cnt, options(nostack, preserves_flags));
	}
	cnt
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

// Block until a send through `channel` would find room (WAIT_WRITABLE): the
// sender's half of backpressure. Returns 0 when writable (or the peer is gone -
// the send then reports the close), a small negative error otherwise (e.g. a
// handle without the WAIT right).
pub unsafe fn wait_writable(channel: u64) -> i64 {
	unsafe { syscall(SYS_WAIT, channel, 0, WAIT_WRITABLE, 0) as i64 }
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

// A blocking receive whose result owns its exactly-sized payload: a message with
// its bytes and any transferred handle, or Closed once the peer is gone.
pub enum ReceivedVec {
	Message { bytes: alloc::vec::Vec<u8>, handle: u64 },
	Closed,
}

// The byte length of the next pending message without dequeuing it: >= 0, or
// ERR_WOULD_BLOCK / ERR_PEER_CLOSED (negative) - the raw peek.
pub unsafe fn channel_peek(channel: u64) -> i64 {
	unsafe { syscall(SYS_CHANNEL_PEEK, channel, 0, 0, 0) as i64 }
}

// Receive one message sized exactly, blocking while the channel is empty: peek the
// pending message's length, allocate precisely, and receive into it. No ceiling
// stands anywhere in this path - a reply is as large as the sender made it.
pub unsafe fn recv_vec_blocking(channel: u64) -> ReceivedVec {
	unsafe {
		loop {
			let pending: i64 = channel_peek(channel);
			if pending == ERR_WOULD_BLOCK {
				wait(channel, 0);
				continue;
			}
			if pending < 0 {
				return ReceivedVec::Closed;
			}
			let mut bytes: alloc::vec::Vec<u8> = alloc::vec![0u8; pending as usize];
			let mut handle: u64 = 0;
			let got: i64 = syscall(SYS_CHANNEL_RECV, channel, bytes.as_mut_ptr() as u64, bytes.len() as u64, &mut handle as *mut u64 as u64) as i64;
			if got == ERR_WOULD_BLOCK {
				// another receiver raced the peeked message away; go around.
				continue;
			}
			if got < 0 {
				return ReceivedVec::Closed;
			}
			bytes.truncate(got as usize);
			return ReceivedVec::Message { bytes, handle };
		}
	}
}

// Drain a stream's consumer channel to completion: decode each frame with `read`
// and collect the items (the producer closing the channel marks end-of-stream).
// The consumer handle is closed. A consumer that renders incrementally loops over
// `recv_vec_blocking` by hand instead.
pub unsafe fn drain_stream<T, F: Fn(&[u8], &mut u64) -> Option<T>>(consumer: u64, read: F) -> alloc::vec::Vec<T> {
	unsafe {
		let mut items: alloc::vec::Vec<T> = alloc::vec::Vec::new();
		loop {
			match recv_vec_blocking(consumer) {
				ReceivedVec::Message { bytes, mut handle } => {
					if let Some(item) = read(&bytes, &mut handle) {
						items.push(item);
					}
					if handle != 0 {
						close(handle);
					}
				}
				ReceivedVec::Closed => break,
			}
		}
		close(consumer);
		items
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

// Send `bytes` (and optionally one transferred handle) to the peer, blocking in
// `wait` (WAIT_WRITABLE) while the queue is full - real backpressure, not a yield
// spin. Returns true on delivery. A handle without the WAIT right (an attenuated
// send-only stdout dup) falls back to yielding; a send refused by the Domain's
// IPC-queue quota rather than queue room also degrades to the yield pace (the
// writable wait reads ready then, since the queue itself has space).
pub unsafe fn send_blocking(channel: u64, bytes: &[u8], xfer: u64) -> bool {
	unsafe {
		loop {
			let result: u64 = syscall(SYS_CHANNEL_SEND, channel, bytes.as_ptr() as u64, bytes.len() as u64, xfer);
			let signed: i64 = result as i64;
			if signed == ERR_WOULD_BLOCK {
				if wait_writable(channel) < 0 {
					yield_now();
				}
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
	unsafe { channel_with_depth(0) }
}

// Create a channel pair whose endpoints queue up to `depth` messages each (0 =
// the kernel default), so a creator that knows its traffic picks its own
// backpressure point.
pub unsafe fn channel_with_depth(depth: u64) -> Option<(u64, u64)> {
	unsafe {
		let mut a: u64 = 0;
		let mut b: u64 = 0;
		let result: u64 = syscall(SYS_CHANNEL_CREATE, &mut a as *mut u64 as u64, &mut b as *mut u64 as u64, depth, 0);
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

// The bootstrap failure report tag: a child that cannot complete a bootstrap step
// sends this - the failing step and the reason - on its bootstrap channel before it
// exits, so the supervisor records why it went down instead of seeing an unexplained
// peer-close. Read by ServiceManager in place of the service's "online" report.
pub const BOOTSTRAP_FAILURE: &[u8] = b"BOOTFAIL";

// The capability tags of the CapSet bootstrap handshakes, shared by both ends so the
// granting side (ServiceManager's bootstrap sequence) and the receiving side (the
// service's recv_caps / CapSet::take) name each capability through one symbol and
// cannot drift: a renamed or mistyped tag is a compile error, not a capability the
// receiver silently misses. The shell handshake (ServiceManager -> shell):
pub const CAP_STORAGE: &[u8] = b"STORAGE";
pub const CAP_MEDIA: &[u8] = b"MEDIA";
pub const CAP_ISO: &[u8] = b"ISO";
pub const CAP_UDF: &[u8] = b"UDF";
pub const CAP_USB: &[u8] = b"USB";
pub const CAP_LOG: &[u8] = b"LOG";
pub const CAP_DEVICE: &[u8] = b"DEVICE";
pub const CAP_PROCESS: &[u8] = b"PROCESS";
pub const CAP_CONFIG: &[u8] = b"CONFIG";
pub const CAP_NET: &[u8] = b"NET";
pub const CAP_TIME: &[u8] = b"TIME";
pub const CAP_AUDIO: &[u8] = b"AUDIO";
pub const CAP_INPUT: &[u8] = b"INPUT";
pub const CAP_GRAPH: &[u8] = b"GRAPH";
pub const CAP_PERM: &[u8] = b"PERM";
pub const CAP_RESOURCE: &[u8] = b"RESOURCE";
pub const CAP_SESSION: &[u8] = b"SESSION";
pub const CAP_CONSOLE: &[u8] = b"CONSOLE";
pub const CAP_CONTROL: &[u8] = b"CONTROL";
pub const CAP_ADMIN: &[u8] = b"ADMIN";
// The ConsoleService handshake (ServiceManager -> ConsoleService): its own client and
// per-VT control channel, then a factory connection per serve_multi service it spawns
// VTs against (the F-prefixed tags), then the display and pointer-forward channels.
pub const CAP_CLIENT: &[u8] = b"CLIENT";
pub const CAP_FSTORAGE: &[u8] = b"FSTORAGE";
pub const CAP_FLOG: &[u8] = b"FLOG";
pub const CAP_FDEVICE: &[u8] = b"FDEVICE";
pub const CAP_FPROCESS: &[u8] = b"FPROCESS";
pub const CAP_FCONFIG: &[u8] = b"FCONFIG";
pub const CAP_FTIME: &[u8] = b"FTIME";
pub const CAP_FAUDIO: &[u8] = b"FAUDIO";
pub const CAP_FSESSION: &[u8] = b"FSESSION";
pub const CAP_FPERM: &[u8] = b"FPERM";
pub const CAP_FNET: &[u8] = b"FNET";
pub const CAP_DISPLAY: &[u8] = b"DISPLAY";
pub const CAP_GPU: &[u8] = b"GPU";
pub const CAP_POINTER: &[u8] = b"POINTER";

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

// Report a failed bootstrap step and terminate: send BOOTSTRAP_FAILURE with the failing
// step and the reason (rendered as `step: reason`) on the bootstrap channel, then exit.
// A service calls this in place of a bare exit() when a required capability or archive is
// missing, so the supervisor logs the reason and folds it into the service's status
// instead of recording a silent peer-close.
pub unsafe fn fail_bootstrap(bootstrap: u64, step: &[u8], reason: &[u8]) -> ! {
	unsafe {
		let mut buf: [u8; 128] = [0u8; 128];
		let mut n: usize = 0;
		let parts: [&[u8]; 5] = [BOOTSTRAP_FAILURE, b" ", step, b": ", reason];
		for part in parts {
			let copy: usize = part.len().min(buf.len() - n);
			buf[n..n + copy].copy_from_slice(&part[..copy]);
			n += copy;
		}
		send_blocking(bootstrap, &buf[..n], 0);
	}
	exit();
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
	F: FnMut(&[u8], &mut u64, &mut [u8], &mut u64) -> Option<usize>,
{
	unsafe {
		loop {
			match recv_blocking(service, request) {
				Received::Message { len, .. } if len == 0 => break,
				Received::Message { len, mut handle } => {
					// the reserved heartbeat probe: answer it uniformly without invoking the
					// typed dispatch, so the supervisor's watchdog can prove this service is
					// still responsive (a service wedged inside a request never returns here
					// to answer, so the probe times out - that is how a hang is detected).
					if len >= 2 && u16::from_le_bytes([request[0], request[1]]) == HEARTBEAT_OP {
						send_blocking(service, b"PONG", 0);
						continue;
					}
					let mut reply_handle: u64 = 0;
					if let Some(n) = handle_request(&request[..len], &mut handle, reply, &mut reply_handle) {
						if !send_blocking(service, &reply[..n], reply_handle) && reply_handle != 0 {
							close(reply_handle);
						}
					} else if reply_handle != 0 {
						close(reply_handle);
					}
					if handle != 0 {
						close(handle);
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

// The reserved control opcode a holder of a multi-client service's channel sends to
// mint its own independent client connection (the generic equivalent of
// NetworkService's typed `open`). `serve_multi` intercepts it before the typed
// dispatch and replies with a fresh client endpoint; the matching service endpoint
// joins its wait set. No typed interface uses opcode 0xffff, so it never collides
// with a real request.

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
	F: FnMut(u64, &[u8], &mut u64, &mut [u8], &mut u64) -> Option<usize>,
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
pub unsafe fn serve_multi_seeded<F>(root: u64, seed: &[u64], request: &mut [u8], reply: &mut [u8], handle_request: F)
where
	F: FnMut(u64, &[u8], &mut u64, &mut [u8], &mut u64) -> Option<usize>,
{
	unsafe {
		serve_multi_ticked(root, seed, 0, request, reply, handle_request);
	}
}

// Like `serve_multi_seeded`, but with `period` non-zero the loop also wakes every
// `period` ticks even when no request arrives, calling the handler with chan = 0
// and an empty request - a housekeeping tick a service flushes batched state on
// (LogService's on-disk journal). The wake is a WAIT_PERIODIC deadline, so it
// never counts as pending progress for the scheduler's boot driver.
pub unsafe fn serve_multi_ticked<F>(root: u64, seed: &[u64], period: u64, request: &mut [u8], reply: &mut [u8], mut handle_request: F)
where
	F: FnMut(u64, &[u8], &mut u64, &mut [u8], &mut u64) -> Option<usize>,
{
	unsafe {
		let mut chans: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
		chans.push(root);
		chans.extend_from_slice(seed);
		while !chans.is_empty() {
			let ready: i64 = if period != 0 { wait_any_periodic(&chans, clock() + period) } else { wait_any(&chans, 0) };
			if ready < 0 {
				if ready == ERR_TIMED_OUT && period != 0 {
					// the housekeeping tick: no channel is ready, let the handler flush.
					let mut reply_handle: u64 = 0;
					let mut handle: u64 = 0;
					let _ = handle_request(0, &[], &mut handle, reply, &mut reply_handle);
				}
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
				Received::Message { len, mut handle } => {
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
						if let Some(n) = handle_request(chan, &request[..len], &mut handle, reply, &mut reply_handle) {
							if !send_blocking(chan, &reply[..n], reply_handle) && reply_handle != 0 {
								close(reply_handle);
							}
						} else if reply_handle != 0 {
							close(reply_handle);
						}
						if handle != 0 {
							close(handle);
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

// The reserved broker opcode a component sends over its (kept-alive) bootstrap
// channel to (re-)resolve a named service capability: `RESOLVE_OP` + the capability
// tag (e.g. CAP_CONFIG). ServiceManager - the broker, the one process that spawns
// every service, holds its factory root, and runs the restart ladder - answers with
// a fresh client channel to the CURRENT live instance, minted through the service's
// CONNECT_OP factory. The request is answered only if the requester's manifest
// grants that name (the capability discipline enforced on every connect, not just
// once at spawn); an un-granted name gets b"DENIED", a service that is gone for
// good (stopped, or its restart budget spent) gets b"DOWN". A resolve aimed at a
// service the broker is currently restarting is simply answered late (the broker
// is single-threaded), so a client experiences the crash as latency, not an error.
// This is the durable-name / disposable-channel pattern (Fuchsia routing, Genode
// sessions, Minix reincarnation): the client's lasting reference is the NAME.
// No typed interface uses opcode 0xfffd, so it never collides with a real request.

// A managed service announces a CLEAN exit to the supervisor by sending `GOODBYE_OP`
// on its bootstrap / report channel just before it exits, so the supervisor records a
// deliberate stop instead of reading the channel's peer-close as a crash. Only the
// interactive shell exits at runtime (a logout); every other service is meant to stand
// for the life of the system, so a bare peer-close from one of them stays a crash.
// No typed interface uses opcode 0xfffc, so it never collides with a real request.

// Announce a clean exit on `channel` (the bootstrap / report channel the supervisor
// watches). Call it right before `exit()` so a logout reads as a deliberate stop.
pub unsafe fn announce_exit(channel: u64) {
	unsafe {
		send_blocking(channel, &GOODBYE_OP.to_le_bytes(), 0);
	}
}

// Re-resolve a named service capability over the broker (bootstrap) channel:
// send `RESOLVE_OP` + `name` and block for the broker's answer. Returns the fresh
// client channel to the current live instance, or None when the broker denies the
// name, the service is gone for good, or the broker itself is gone.
pub unsafe fn resolve(broker: u64, name: &[u8]) -> Option<u64> {
	unsafe {
		let mut req: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(2 + name.len());
		req.extend_from_slice(&RESOLVE_OP.to_le_bytes());
		req.extend_from_slice(name);
		if !send_blocking(broker, &req, 0) {
			return None;
		}
		let mut buf: [u8; 32] = [0u8; 32];
		match recv_blocking(broker, &mut buf) {
			Received::Message { handle, .. } if handle != 0 => Some(handle),
			_ => None,
		}
	}
}

// Mint a fresh sub-connection from the factory `*held` (a connection to a
// serve_multi service), re-resolving `name` over the broker when the factory is
// dead - its service crashed and was restarted, so the broker answers with a
// connection to the live instance, which replaces `*held`. The pattern every
// standing holder of a restartable service's factory shares (PermissionManager's
// grants, ConsoleService's per-VT connections). None when the mint fails and the
// broker cannot provide a live replacement.
pub unsafe fn connect_or_resolve(held: &mut u64, broker: u64, name: &[u8]) -> Option<u64> {
	unsafe {
		if let Some(minted) = service_connect(*held) {
			return Some(minted);
		}
		let fresh: u64 = resolve(broker, name)?;
		if *held != 0 {
			close(*held);
		}
		*held = fresh;
		service_connect(*held)
	}
}

// A proto Transport over an rt channel: send the request (with any out-of-band
// handle), then block for the reply (whose own out-of-band handle is returned
// alongside the bytes). Every userspace program that drives a generated service
// client - the shell, the supervisor, the demo clients - reaches its service
// through this one implementation, instead of each repeating the send/recv glue.
#[cfg(feature = "proto-transport")]
pub struct ChannelTransport {
	pub chan: u64,
}

#[cfg(feature = "proto-transport")]
impl proto::codec::Transport for ChannelTransport {
	fn call(&mut self, request: &[u8], request_handle: u64) -> Option<(alloc::vec::Vec<u8>, u64)> {
		unsafe {
			if !send_blocking(self.chan, request, request_handle) {
				return None;
			}
			// The reply is received sized exactly (peek + recv), so a typed reply is
			// as large as the service made it - no wire ceiling stands here.
			match recv_vec_blocking(self.chan) {
				ReceivedVec::Message { bytes, handle } => Some((bytes, handle)),
				ReceivedVec::Closed => None,
			}
		}
	}

	fn discard_handle(&mut self, handle: u64) {
		if handle != 0 {
			unsafe { close(handle) };
		}
	}
}

// A proto Transport that survives a service restart: like ChannelTransport, but the
// durable reference is the capability NAME, re-resolved over the broker (bootstrap)
// channel when the live channel dies. When the send itself fails (the service
// crashed BETWEEN requests - the common case, nothing was delivered), the request
// is transparently retried once on a freshly resolved channel: at-most-once
// semantics hold, so the retry is always safe. When the service dies MID-request
// (the send succeeded but the reply never came), the call returns None and only the
// NEXT call reconnects: the request may have been half-applied, so replaying it is
// the caller's protocol-level decision, not the transport's - the honest limit of
// connection-level transparency.
#[cfg(feature = "proto-transport")]
pub struct SvcTransport {
	// The broker (bootstrap) channel resolves are sent over. Borrowed, never closed.
	broker: u64,
	// The capability tag this connection re-resolves as (e.g. CAP_CONFIG).
	name: &'static [u8],
	// The current live channel (0 = not connected; resolved on the next call).
	chan: u64,
}

#[cfg(feature = "proto-transport")]
impl SvcTransport {
	// Wrap a bootstrap-granted channel: `chan` serves until it peer-closes, then the
	// name takes over. `chan` 0 is valid (the first call resolves).
	pub const fn new(broker: u64, name: &'static [u8], chan: u64) -> SvcTransport {
		SvcTransport { broker, name, chan }
	}

	// The current live channel, resolving through the broker when there is none.
	// 0 when the broker cannot (or will not) provide one.
	pub unsafe fn channel(&mut self) -> u64 {
		if self.chan == 0 {
			self.chan = unsafe { resolve(self.broker, self.name) }.unwrap_or(0);
		}
		self.chan
	}

	// Drop the dead channel and resolve a fresh connection to the current live
	// instance. False when the broker denies or the service is gone for good.
	pub unsafe fn reconnect(&mut self) -> bool {
		unsafe {
			if self.chan != 0 {
				close(self.chan);
				self.chan = 0;
			}
			self.channel() != 0
		}
	}
}

#[cfg(feature = "proto-transport")]
impl proto::codec::Transport for SvcTransport {
	fn call(&mut self, request: &[u8], request_handle: u64) -> Option<(alloc::vec::Vec<u8>, u64)> {
		unsafe {
			let chan: u64 = self.channel();
			if chan == 0 {
				return None;
			}
			if !send_blocking(chan, request, request_handle) {
				// Nothing was delivered: reconnect and retry once (at-most-once holds).
				if !self.reconnect() {
					return None;
				}
				if !send_blocking(self.chan, request, request_handle) {
					return None;
				}
			}
			match recv_vec_blocking(self.chan) {
				ReceivedVec::Message { bytes, handle } => Some((bytes, handle)),
				ReceivedVec::Closed => {
					// Died mid-request: reconnect for the NEXT call, report this one
					// failed (replaying a possibly half-applied request is not ours).
					let _ = self.reconnect();
					None
				}
			}
		}
	}

	fn discard_handle(&mut self, handle: u64) {
		if handle != 0 {
			unsafe { close(handle) };
		}
	}
}

// A long-lived holder drives its generated client through a mutable borrow, so the
// transport's reconnect state (the current channel) persists across calls:
// `device::Client::new(&mut self.device)` each snapshot, one SvcTransport for life.
#[cfg(feature = "proto-transport")]
impl proto::codec::Transport for &mut SvcTransport {
	fn call(&mut self, request: &[u8], request_handle: u64) -> Option<(alloc::vec::Vec<u8>, u64)> {
		(**self).call(request, request_handle)
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

// Stage bytes in a shared buffer for a zero-copy typed call (a volume write, a
// framed payload): create a memory object, copy `bytes` in, and return a
// read+map+transfer duplicate as the proto Buffer that travels with the request;
// our own handle is closed. The shared "hand bytes to a service" path (LogService's
// journal flush, ConfigService's persisted tree). None when the object cannot be
// created or mapped.
#[cfg(feature = "proto-transport")]
pub unsafe fn make_buffer(bytes: &[u8]) -> Option<proto::codec::Buffer> {
	unsafe {
		let obj: i64 = memory_object_create(bytes.len().max(1) as u64);
		if obj < 0 {
			return None;
		}
		let obj: u64 = obj as u64;
		let mapped: u64 = match map_object(obj) {
			Some(base) => base,
			None => {
				close(obj);
				return None;
			}
		};
		core::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped as *mut u8, bytes.len());
		unmap_object(obj);
		let granted: i64 = duplicate(obj, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		close(obj);
		if granted < 0 {
			return None;
		}
		Some(proto::codec::Buffer { handle: granted as u64, len: bytes.len() as u64 })
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

// Read the CPU model / brand string into `buf`, returning the byte length written
// (as many bytes as fit). A free syscall feeding the `lscpu` model field.
pub unsafe fn cpu_name(buf: &mut [u8]) -> i64 {
	unsafe { syscall(SYS_CPU_NAME, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0) as i64 }
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

pub unsafe fn dma_buffer_unmap(handle: u64) -> i64 {
	unsafe { syscall(SYS_DMA_BUFFER_UNMAP, handle, 0, 0, 0) as i64 }
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

// Map one dependency into a process returned by SYS_PROCESS_CREATE. ProcessService
// calls this in topological order before loading the main image; no thread or stack is
// created by a module load.
pub unsafe fn process_load_module(process: u64, elf: &[u8], bias: u64) -> i64 {
	unsafe { syscall(SYS_PROCESS_LOAD_MODULE, process, elf.as_ptr() as u64, elf.len() as u64, bias) as i64 }
}

pub unsafe fn process_create(domain: u64) -> i64 {
	unsafe { syscall(SYS_PROCESS_CREATE, domain, 0, 0, 0) as i64 }
}

pub unsafe fn process_load_main(process: u64, elf: &[u8]) -> i64 {
	unsafe { syscall(SYS_PROCESS_LOAD, process, elf.as_ptr() as u64, elf.len() as u64, 0) as i64 }
}

pub unsafe fn process_start(process: u64, entry: u64, bootstrap: u64) -> i64 {
	unsafe {
		let thread = syscall(SYS_THREAD_CREATE, process, entry, USER_STACK_TOP, bootstrap);
		if sys_is_err(thread) {
			return thread as i64;
		}
		let started = syscall(SYS_THREAD_START, thread, 0, 0, 0);
		close(thread);
		if sys_is_err(started) { started as i64 } else { process as i64 }
	}
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
		let process = process_create(domain);
		if process < 0 {
			return process;
		}
		let process = process as u64;
		let entry = process_load_main(process, elf);
		if entry < 0 {
			close(process);
			return entry;
		}
		let started = process_start(process, entry as u64, bootstrap);
		if started < 0 {
			close(process);
		}
		started
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
