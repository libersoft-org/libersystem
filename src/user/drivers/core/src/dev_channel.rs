// driver.dev-channel - the development channel port and the guest half of the
// development-control protocol.
//
// The host attaches a SECOND single-port virtio-serial device, pinned to a fixed PCI
// address, and DeviceManager binds this program to it instead of the console driver. That
// is the whole point of a second device rather than a second port: no MULTIPORT
// negotiation, no control-queue port discovery, and the console keeps its own device, so
// either channel can fail without taking the other down.
//
// The transport below the framing is the port's raw byte stream and knows nothing about
// how the port was discovered, so a later multiport driver can host the same protocol on a
// named port without changing anything above this layer.
//
// The receive path is interrupt-driven, like virtio-net and virtio-input: this program
// blocks on its device's MSI-X Interrupt and is woken when the host writes. A control
// channel is idle almost all the time, and a driver that polled it would spend the guest's
// whole life spinning - under a cooperative scheduler that is not a waste of cycles but a
// correctness problem, because a runnable spinner starves the threads that still have boot
// work to do. Blocking on the interrupt costs nothing while nobody is talking.
//
// ---- the wire format ----
//
// Every frame is a 16-byte little-endian header, optionally followed by its payload:
//
//   magic u16 | version u8 | opcode u8 | request u32 | generation u32 | length u16 | status u16
//
// The header is fixed and leads with a magic so a desynchronised stream can be
// resynchronised on the magic alone. The x86_64 channel needs exactly that: UEFI writes
// its console output to every console-class device it enumerates, so the host sees a
// firmware preamble that nobody framed before the guest owns the port.
//
// Every bound is a constant checked on both sides and reported in the handshake, so a peer
// fails at the handshake instead of at the first payload that does not fit. Nothing here
// grows with the session: the receive accumulator holds at most one frame, request IDs are
// tracked with a single watermark, and a frame is dispatched whole or discarded whole.

#![no_std]
#![no_main]

extern crate alloc;

mod common;
mod virtio;

use alloc::vec::Vec;
use rt::*;

use crate::virtio::{Queue, Virtio};

// The protocol's identifying prefix ("LD" on the wire) and the version this guest speaks.
// A version this guest does not know is refused rather than guessed at: the header layout
// after the version byte is what the version defines, so a mismatched frame's length field
// cannot be trusted to skip past it.
const MAGIC: u16 = 0x444c;
const VERSION: u8 = 1;
const HEADER_LEN: usize = 16;

// The frame bound, header included, and the payload bound that follows from it. Both are
// reported in the handshake. The length field is a u16, so a payload larger than the bound
// is still expressible and therefore has to be rejected explicitly rather than assumed
// away.
const MAX_FRAME: usize = 65536;
const MAX_PAYLOAD: usize = MAX_FRAME - HEADER_LEN;
// The most requests a host may leave unanswered. Reported in the handshake; the operations
// this version defines are all answered before the next frame is parsed, so nothing is ever
// outstanding yet and the bound exists for the peer to size itself by.
const MAX_OUTSTANDING: u16 = 16;

// Opcodes. Requests are host to guest, replies guest to host; a guest-to-host opcode
// arriving from the host is an unknown opcode, not a request.
const OP_HELLO: u8 = 0x01;
const OP_HELLO_ACK: u8 = 0x02;
const OP_PING: u8 = 0x03;
const OP_PONG: u8 = 0x04;
const OP_ERROR: u8 = 0xff;

// Statuses. Every rejection names one of these, so a failure is explainable by the frame
// that caused it rather than by a timeout on the host.
const ST_OK: u16 = 0;
const ST_BAD_VERSION: u16 = 1;
const ST_BAD_OPCODE: u16 = 2;
const ST_OVERSIZED: u16 = 3;
const ST_MALFORMED: u16 = 4;
const ST_HANDSHAKE_REQUIRED: u16 = 5;
const ST_DUPLICATE_REQUEST: u16 = 6;
const ST_TIMED_OUT: u16 = 7;

// The receive pool: enough slots that a burst of host writes is absorbed without the
// device running out of buffers between two interrupts, each large enough that an ordinary
// control frame arrives in one.
const RX_SLOTS: u16 = 8;
const RX_SLOT: u64 = 4096;

// The deadline on an incomplete frame, in scheduler ticks (100 Hz). A host that stops
// mid-frame - killed, disconnected, or writing a length it never delivers - must not leave
// the guest holding a fragment forever, so the wait on the interrupt carries this deadline
// whenever the accumulator is non-empty and the fragment is discarded when it expires. The
// accumulator is empty in the idle case, so an idle channel blocks with no deadline at all
// and never wakes.
const PARTIAL_FRAME_TICKS: u64 = 200;

// The deadline on a silent session, in the same ticks. The transport cannot report that a
// host disconnected, so silence is the only signal there is: a session that goes quiet for
// this long is closed and everything it held is released, and the next host has to hand
// shake again rather than inherit it. Comfortably longer than any single operation's own
// deadline, so it never cuts a working host off, and short enough that a crashed one does
// not leave its session standing. An idle guest with no open session arms nothing at all.
const SESSION_IDLE_TICKS: u64 = 3000;

// How long a reply may wait for the transmit buffer to come back, in the same ticks. The
// host end can stop reading at any moment, and QEMU then stops consuming the transmit queue
// rather than discarding what it cannot deliver, so the buffer stays with the device. This
// bounds how long the guest tolerates that before declaring the host gone.
const TX_DRAIN_TICKS: u64 = 200;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// Bring the device up and take its MSI-X Interrupt, then route this device's
		// interrupts to table entry 0 (DeviceManager acquired it and the kernel programmed
		// the table), before the queues are set up so each queue is told the vector.
		let mut device: Virtio = common::bringup(bootstrap);
		let irq: u64 = recv_irq(bootstrap);
		device.set_msix_vector(0);
		// Single port, exactly like the console device: receiveq = 0, transmitq = 1.
		let mut rx: Queue = match device.setup_queue(0) {
			Some(q) => q,
			None => exit(),
		};
		let mut tx: Queue = match device.setup_queue(1) {
			Some(q) => q,
			None => exit(),
		};
		// Both queues are interrupt-driven. The receive side because the channel is idle
		// almost always, and the transmit side because its completions are the only proof
		// the buffer came back and may be filled again.
		rx.enable_interrupts();
		tx.enable_interrupts();
		let (rxpool, rx_virt, _): (u64, u64, u64) = match dma_buffer(RX_SLOTS as u64 * RX_SLOT) {
			Some(t) => t,
			None => exit(),
		};
		// One transmit buffer sized to the frame bound, so the largest reply this version
		// can produce (a ping echoing a maximum payload) is sent in one descriptor.
		let (_txbuf, tx_virt, tx_phys): (u64, u64, u64) = match dma_buffer(MAX_FRAME as u64) {
			Some(t) => t,
			None => exit(),
		};
		// Post the receive pool and go live, each slot at its own physical address with the
		// contiguous virtual mapping read back as `rx_virt + id * RX_SLOT`.
		let mut rx_phys: [u64; RX_SLOTS as usize] = [0u64; RX_SLOTS as usize];
		let mut id: u16 = 0;
		while id < RX_SLOTS {
			rx_phys[id as usize] = dma_buffer_phys_at(rxpool, id as u64 * RX_SLOT);
			rx.post_recv(id, rx_phys[id as usize], RX_SLOT as u32);
			id += 1;
		}
		rx.notify();
		device.driver_ok();
		send_blocking(bootstrap, b"driver.dev-channel: online (protocol v1)", 0);
		let mut port: Port = Port { device: &device, irq, tx: &mut tx, virt: tx_virt, phys: tx_phys, busy: false };
		serve(&device, irq, &mut rx, &mut port, rx_virt, &rx_phys)
	}
}

// Receive the "IRQ" message carrying this device's Interrupt capability, which
// DeviceManager acquired and transferred to us. Exits if it does not arrive.
unsafe fn recv_irq(bootstrap: u64) -> u64 {
	unsafe {
		let mut buf: [u8; 16] = [0u8; 16];
		match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if handle != 0 && len >= 3 && &buf[..3] == b"IRQ" => handle,
			_ => exit(),
		}
	}
}

// One protocol session, which is all the state the guest keeps. A HELLO opens it and the
// idle deadline closes it. That pair exists because a virtio-serial port without MULTIPORT
// reports no open and no close: the guest is never told that a host connected or went away,
// so a session that only ever opened would outlive the host that opened it, and the next
// host would silently inherit its state instead of starting from a known one. Bounding the
// session by silence turns a disconnect - crash, kill, unplugged terminal - into the same
// deterministic outcome as an orderly one, on a deadline rather than on an event the
// transport cannot deliver.
struct Session {
	// Whether the handshake completed. Every other opcode fails until it has.
	handshake: bool,
	// The highest request ID accepted so far. IDs must be non-zero and strictly increasing,
	// which rejects both a duplicate and a replay with one word of state instead of a table
	// of in-flight IDs - and stays correct when the host does leave requests outstanding,
	// because it constrains the order they are issued in, not the order they are answered.
	high_request: u32,
}

// The transmit side of the port, and the reason it is not a simple synchronous write. The
// host end can stop reading at any moment - a killed tool, a full socket buffer, a
// terminal that went away - and QEMU responds by ceasing to consume the transmit queue
// rather than discarding what it cannot deliver. The device therefore keeps ownership of
// the buffer it was handed. A polled write that gave up on such a buffer would leave the
// device holding a descriptor that the next reply overwrites, which corrupts the ring
// permanently and takes the channel down for the rest of the guest's life. So completions
// are reaped explicitly and the buffer is never refilled until the device gives it back.
// Waiting for that is bounded; the port recovers by itself once the host reads again,
// precisely because the descriptor was never reused behind the device's back.
struct Port<'a> {
	device: &'a Virtio,
	irq: u64,
	tx: &'a mut Queue,
	virt: u64,
	phys: u64,
	// The device owns the transmit buffer and has not returned it yet.
	busy: bool,
}

impl Port<'_> {
	// Reap every transmit completion the device has posted, which is what releases the
	// buffer for the next frame.
	unsafe fn reclaim(&mut self) {
		unsafe {
			while self.tx.take_used().is_some() {
				self.busy = false;
			}
		}
	}

	// Write one frame to the port (virtio-serial buffers are raw bytes, no header), waiting
	// - bounded - for the previous frame to be taken first. Returns false when the host has
	// stopped consuming, which the caller treats as the session ending: there is no point
	// answering a peer that is not reading, and every reply after that would inherit the
	// same wait.
	unsafe fn send(&mut self, opcode: u8, request: u32, status: u16, payload: &[u8]) -> bool {
		unsafe {
			if payload.len() > MAX_PAYLOAD {
				return false;
			}
			self.reclaim();
			let limit: u64 = clock() + TX_DRAIN_TICKS;
			while self.busy {
				if clock() >= limit {
					return false;
				}
				wait(self.irq, limit);
				let _ = self.device.read_isr();
				interrupt_ack(self.irq);
				self.reclaim();
			}
			let mut header: [u8; HEADER_LEN] = [0u8; HEADER_LEN];
			header[..2].copy_from_slice(&MAGIC.to_le_bytes());
			header[2] = VERSION;
			header[3] = opcode;
			header[4..8].copy_from_slice(&request.to_le_bytes());
			header[12..14].copy_from_slice(&(payload.len() as u16).to_le_bytes());
			header[14..16].copy_from_slice(&status.to_le_bytes());
			core::ptr::copy_nonoverlapping(header.as_ptr(), self.virt as *mut u8, HEADER_LEN);
			core::ptr::copy_nonoverlapping(payload.as_ptr(), (self.virt + HEADER_LEN as u64) as *mut u8, payload.len());
			self.busy = self.tx.submit_async(&[(self.phys, (HEADER_LEN + payload.len()) as u32, false)]);
			self.busy
		}
	}
}

// Serve the channel: reap what the host wrote into the accumulator, parse whole frames out
// of it, and block on the device interrupt when there is nothing left to do. The
// accumulator holds at most one frame plus the receive slot that completed it, because
// every complete frame is consumed before the next wait and a fragment that never completes
// is discarded on its deadline.
//
// Receive buffers are reaped before every wait rather than only after one. Both queues
// share the device's single MSI-X vector, so the wait inside a transmit drain can consume
// the very interrupt that announced newly arrived bytes; reaping first means the loop never
// blocks while the used ring still holds work, whichever wait observed the interrupt.
unsafe fn serve(device: &Virtio, irq: u64, rx: &mut Queue, port: &mut Port, rx_virt: u64, rx_phys: &[u64]) -> ! {
	unsafe {
		let mut session: Session = Session { handshake: false, high_request: 0 };
		let mut pending: Vec<u8> = Vec::with_capacity(HEADER_LEN + MAX_PAYLOAD + RX_SLOT as usize);
		// The two absolute deadlines this loop carries: when the buffered fragment expires,
		// and when the open session does. Zero means the deadline does not apply.
		let mut fragment_at: u64 = 0;
		let mut session_at: u64 = 0;
		loop {
			let mut arrived: bool = false;
			while let Some((id, len)) = rx.take_used() {
				if id < RX_SLOTS && len > 0 {
					let n: usize = if len as u64 > RX_SLOT { RX_SLOT as usize } else { len as usize };
					let chunk: &[u8] = core::slice::from_raw_parts((rx_virt + id as u64 * RX_SLOT) as *const u8, n);
					pending.extend_from_slice(chunk);
					arrived = true;
				}
				rx.post_recv(id, rx_phys[id as usize], RX_SLOT as u32);
			}
			if arrived {
				rx.notify();
				port.reclaim();
				// A host that stopped reading its own replies is gone as far as this session is
				// concerned. Drop what it left buffered rather than answer into a channel nobody
				// is draining.
				if !consume(&mut session, &mut pending, port) {
					session = Session { handshake: false, high_request: 0 };
					pending.clear();
				}
				// Rearm both deadlines against what the parse left behind. The fragment deadline
				// dates from when the fragment first appeared, not from the last byte of it, so a
				// host trickling one byte at a time cannot hold a frame open indefinitely. The
				// session deadline is refreshed by any traffic at all, because a host that is
				// still writing has not gone away whatever it is writing.
				let now: u64 = clock();
				if pending.is_empty() {
					fragment_at = 0;
				} else if fragment_at == 0 {
					fragment_at = now + PARTIAL_FRAME_TICKS;
				}
				session_at = if session.handshake { now + SESSION_IDLE_TICKS } else { 0 };
				continue;
			}
			// Wait on whichever deadline comes first, and on none at all when the channel is
			// idle with no session open: nothing is then waiting to expire, so the driver must
			// not wake the scheduler even once.
			let mut deadline: u64 = fragment_at;
			if session_at != 0 && (deadline == 0 || session_at < deadline) {
				deadline = session_at;
			}
			let ready: i64 = wait(irq, deadline);
			// Read the ISR to deassert the device's level-triggered INTx line before acking
			// (a harmless zero read on MSI-X, which is edge-triggered).
			let _ = device.read_isr();
			interrupt_ack(irq);
			if ready == ERR_TIMED_OUT {
				let now: u64 = clock();
				if fragment_at != 0 && now >= fragment_at {
					fail_partial(&mut pending, port);
					fragment_at = 0;
				}
				if session_at != 0 && now >= session_at {
					session = Session { handshake: false, high_request: 0 };
					session_at = 0;
				}
			}
		}
	}
}

// Discard the fragment whose deadline expired. It is reported against its own request when
// a full header arrived, so a host that sent a length it never delivered is told which
// request died rather than being left to time out; a fragment too short to name a request
// is dropped silently, since there is nothing to answer.
unsafe fn fail_partial(pending: &mut Vec<u8>, port: &mut Port) {
	unsafe {
		if pending.len() >= HEADER_LEN && u16::from_le_bytes([pending[0], pending[1]]) == MAGIC {
			let request: u32 = u32::from_le_bytes([pending[4], pending[5], pending[6], pending[7]]);
			port.send(OP_ERROR, request, ST_TIMED_OUT, &[]);
		}
		pending.clear();
	}
}

// Parse and dispatch every complete frame the accumulator holds, leaving any trailing
// fragment for the next interrupt. Returns false when the port stopped accepting replies,
// which ends the session rather than being retried frame by frame.
unsafe fn consume(session: &mut Session, pending: &mut Vec<u8>, port: &mut Port) -> bool {
	unsafe {
		loop {
			resync(pending);
			if pending.len() < HEADER_LEN {
				return true;
			}
			let version: u8 = pending[2];
			let opcode: u8 = pending[3];
			let request: u32 = u32::from_le_bytes([pending[4], pending[5], pending[6], pending[7]]);
			let generation: u32 = u32::from_le_bytes([pending[8], pending[9], pending[10], pending[11]]);
			let length: usize = u16::from_le_bytes([pending[12], pending[13]]) as usize;
			// A version this guest does not speak makes the rest of the frame unreadable: the
			// length field only means what the version says it means, so there is no safe
			// number of bytes to skip. Report it and drop everything buffered, which puts the
			// stream back at a resynchronisation point instead of at a guess.
			if version != VERSION {
				let live: bool = port.send(OP_ERROR, request, ST_BAD_VERSION, &[]);
				*session = Session { handshake: false, high_request: 0 };
				pending.clear();
				return live;
			}
			// Wait for the whole frame. The length is a u16 and the accumulator is sized for
			// the largest one expressible, so even an oversized frame is buffered in full and
			// then discarded in full, which keeps the stream framed rather than forcing a
			// resynchronisation after every rejection.
			if pending.len() < HEADER_LEN + length {
				return true;
			}
			let live: bool = dispatch(session, opcode, request, generation, &pending[HEADER_LEN..HEADER_LEN + length], port);
			pending.drain(..HEADER_LEN + length);
			if !live {
				return false;
			}
		}
	}
}

// Drop everything before the next plausible frame start. Leading junk is expected once per
// boot on x86_64 (the UEFI console preamble) and possible at any time from a desynchronised
// host, so the parser hunts for the magic rather than trusting the first byte it is given.
// When no magic is present the last byte is kept, because a magic can straddle two receive
// buffers.
fn resync(pending: &mut Vec<u8>) {
	if pending.len() >= 2 && u16::from_le_bytes([pending[0], pending[1]]) == MAGIC {
		return;
	}
	let mut i: usize = 1;
	while i + 2 <= pending.len() {
		if u16::from_le_bytes([pending[i], pending[i + 1]]) == MAGIC {
			pending.drain(..i);
			return;
		}
		i += 1;
	}
	let keep: usize = if pending.is_empty() { 0 } else { 1 };
	pending.drain(..pending.len() - keep);
}

// Dispatch one complete, correctly versioned frame. Every path answers exactly once, with a
// reply on success and an OP_ERROR naming the status on rejection, so the host never has to
// distinguish a refusal from a loss. Returns whether the reply reached the port.
unsafe fn dispatch(session: &mut Session, opcode: u8, request: u32, generation: u32, payload: &[u8], port: &mut Port) -> bool {
	unsafe {
		// The generation field carries an artifact generation, which no operation in this
		// version has: publication and scenarios define it. Requiring zero now keeps the
		// field from acquiring an accidental second meaning that a later version would have
		// to preserve.
		if request == 0 || generation != 0 {
			return port.send(OP_ERROR, request, ST_MALFORMED, &[]);
		}
		if payload.len() > MAX_PAYLOAD {
			return port.send(OP_ERROR, request, ST_OVERSIZED, &[]);
		}
		// The handshake is the session's reset point, so it is the one opcode exempt from the
		// request-ID watermark it resets. Everything else is refused until it has run.
		if opcode == OP_HELLO {
			*session = Session { handshake: true, high_request: request };
			let mut reply: [u8; 12] = [0u8; 12];
			reply[..4].copy_from_slice(&(MAX_FRAME as u32).to_le_bytes());
			reply[4..8].copy_from_slice(&(MAX_PAYLOAD as u32).to_le_bytes());
			reply[8..10].copy_from_slice(&MAX_OUTSTANDING.to_le_bytes());
			return port.send(OP_HELLO_ACK, request, ST_OK, &reply);
		}
		if !session.handshake {
			return port.send(OP_ERROR, request, ST_HANDSHAKE_REQUIRED, &[]);
		}
		if request <= session.high_request {
			return port.send(OP_ERROR, request, ST_DUPLICATE_REQUEST, &[]);
		}
		session.high_request = request;
		match opcode {
			// Echo the payload, so a ping measures the round trip of a real payload rather
			// than of an empty frame.
			OP_PING => port.send(OP_PONG, request, ST_OK, payload),
			_ => port.send(OP_ERROR, request, ST_BAD_OPCODE, &[]),
		}
	}
}
