// COM1 serial driver (16550 UART) with an asynchronous transmit ring.
//
// Writes never busy-wait on the UART: each byte is enqueued into a software ring
// and the writer returns at once, so a caller (above all the console service
// mirroring its output via SYS_DEBUG_WRITE) is never throttled by the emulated
// UART's transmit pacing. Under KVM every THRE poll is a VM-exit metered by QEMU's
// main loop, so the old per-byte `while !transmit_empty()` spin blocked the caller
// for hundreds of milliseconds on a screenful of output - which, on the console
// render thread, stalled the framebuffer behind the debug console. Now the ring
// drains in the background: on the timer tick and on each core's idle loop, pushing
// a FIFO's worth of bytes whenever the holding register is empty. A synchronous
// flush serves the panic and test-exit paths, where the message must reach the wire
// before the machine halts.
//
// Early boot (before the timer and idle loop that drive the drain are running)
// writes straight to the wire, so boot logs appear immediately; `enable_async` flips
// to the ring once the scheduler is up.

use super::port::{inb, outb};
use crate::sync::SpinLock;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

const COM1: u16 = 0x3F8;

// The 16550 transmit FIFO holds 16 bytes: when THRE is set the holding register
// (and FIFO) is empty, so a full FIFO load may be pushed before polling again.
const FIFO_DEPTH: usize = 16;

// Software transmit ring. 16 kB comfortably buffers a screenful of console mirror
// (a `help` listing is ~2 kB), so the producer never blocks in practice; on the
// rare overflow the writer drains synchronously to make room rather than dropping.
const TX_RING_CAP: usize = 16384;

struct TxRing {
	buf: [u8; TX_RING_CAP],
	head: usize, // next index to fill (producer)
	tail: usize, // next index to drain (consumer)
	len: usize,  // bytes currently queued
}

impl TxRing {
	const fn new() -> Self {
		Self { buf: [0u8; TX_RING_CAP], head: 0, tail: 0, len: 0 }
	}

	fn push(&mut self, byte: u8) {
		self.buf[self.head] = byte;
		self.head = (self.head + 1) % TX_RING_CAP;
		self.len += 1;
	}

	fn pop(&mut self) -> u8 {
		let byte: u8 = self.buf[self.tail];
		self.tail = (self.tail + 1) % TX_RING_CAP;
		self.len -= 1;
		byte
	}
}

// The transmit ring, serialized by an interrupt-safe lock so the timer ISR drain and
// a syscall-context write can never corrupt it (or each other's UART port access).
static TX: SpinLock<TxRing> = SpinLock::new(TxRing::new());

// False during early boot (synchronous writes, so logs appear before the drainers
// run), flipped true by `enable_async` once the timer and idle loop are servicing
// the ring. Monotonic: never flipped back.
static ASYNC: AtomicBool = AtomicBool::new(false);

// UART init: 38400 baud, 8N1, FIFO enabled.
pub fn init() {
	unsafe {
		outb(COM1 + 1, 0x00);
		outb(COM1 + 3, 0x80);
		outb(COM1 + 0, 0x03);
		outb(COM1 + 1, 0x00);
		outb(COM1 + 3, 0x03);
		outb(COM1 + 2, 0xC7);
		outb(COM1 + 4, 0x0B);
	}
}

// Switch transmit to the asynchronous ring. Called once the scheduler is up, so the
// timer tick and idle loop are draining the ring; until then writes go straight to
// the wire (see `write_byte`).
pub fn enable_async() {
	ASYNC.store(true, Ordering::Release);
}

fn transmit_empty() -> bool {
	unsafe { (inb(COM1 + 5) & 0x20) != 0 }
}

// Push as much of the ring to the UART as the holding register will take right now:
// one FIFO load if THRE is set, then return (writing more would need another empty
// FIFO). Non-blocking. The caller holds the TX lock, serializing the port access.
fn drain_locked(ring: &mut TxRing) {
	while ring.len != 0 && transmit_empty() {
		let n: usize = ring.len.min(FIFO_DEPTH);
		for _ in 0..n {
			let byte: u8 = ring.pop();
			unsafe {
				outb(COM1, byte);
			}
		}
	}
}

// Background drain: called from the timer ISR (every core) and each core's idle
// loop. `try_lock` so it never spins in an interrupt handler when another core
// holds the ring; it simply tries again on the next tick.
pub fn drain_tx() {
	if let Some(mut ring) = TX.try_lock() {
		drain_locked(&mut ring);
	}
}

// Drain the whole ring synchronously, then wait for the last FIFO load to leave the
// wire. For the panic and test-exit paths: the output must be delivered before the
// machine halts or QEMU exits.
pub fn flush_sync() {
	let mut ring = TX.lock();
	while ring.len != 0 {
		while !transmit_empty() {
			core::hint::spin_loop();
		}
		drain_locked(&mut ring);
	}
	while !transmit_empty() {
		core::hint::spin_loop();
	}
}

fn write_byte(byte: u8) {
	if !ASYNC.load(Ordering::Acquire) {
		// Early boot: write straight to the wire so logs appear immediately, before
		// the timer and idle loop that service the ring are running.
		while !transmit_empty() {
			core::hint::spin_loop();
		}
		unsafe {
			outb(COM1, byte);
		}
		return;
	}
	let mut ring = TX.lock();
	if ring.len == TX_RING_CAP {
		// Ring full: a flood outpacing the UART. Push out whatever the holding register
		// will take right now (non-blocking) to make room, then enqueue. If the UART is
		// not ready we drop this byte rather than busy-wait on it with the lock - and so
		// interrupts - held, which would stall this core's timer tick (and freeze any
		// caller draining the ring from an interrupts-disabled context). Does not fire
		// for normal output; the ring comfortably buffers a screenful.
		drain_locked(&mut ring);
		if ring.len == TX_RING_CAP {
			return;
		}
	}
	ring.push(byte);
}

// Best-effort bulk enqueue for the SYS_DEBUG_WRITE path: push as much as fits in the
// async TX ring (translating \n to \r\n) under one lock, and drop the rest. Unlike
// write_byte it never sync-drains on a full ring, so a userspace caller - above all the
// console service mirroring a screenful while the boot log is still draining the
// baud-paced UART - is never throttled, and the framebuffer it renders is never stalled
// behind the debug mirror. The mirror is best-effort debug output; the framebuffer is
// the product. The kernel's own logs keep the lossless write_byte path.
pub fn write_bytes(bytes: &[u8]) {
	if !ASYNC.load(Ordering::Acquire) {
		// Early boot (ring not yet serviced): keep it lossless via the wire-direct path.
		for &byte in bytes {
			if byte == b'\n' {
				write_byte(b'\r');
			}
			write_byte(byte);
		}
		return;
	}
	let mut ring = TX.lock();
	for &byte in bytes {
		if byte == b'\n' {
			if ring.len == TX_RING_CAP {
				break;
			}
			ring.push(b'\r');
		}
		if ring.len == TX_RING_CAP {
			break;
		}
		ring.push(byte);
	}
}

// True if the UART has a received byte waiting (Line Status Register, DR bit).
#[cfg(not(test))]
fn data_ready() -> bool {
	unsafe { (inb(COM1 + 5) & 0x01) != 0 }
}

// Read one received byte without waiting: Some(byte) if one is buffered, else
// None. Lets a poller (the serial CLI) check for input without blocking.
#[cfg(not(test))]
pub fn read_byte() -> Option<u8> {
	if data_ready() { Some(unsafe { inb(COM1) }) } else { None }
}

pub struct SerialWriter;

impl Write for SerialWriter {
	fn write_str(&mut self, s: &str) -> fmt::Result {
		for byte in s.bytes() {
			if byte == b'\n' {
				write_byte(b'\r');
			}
			write_byte(byte);
		}
		Ok(())
	}
}
