// driver.dev-channel - the development channel port.
//
// The host attaches a SECOND single-port virtio-serial device, pinned to a fixed PCI
// address, and DeviceManager binds this program to it instead of the console driver. That
// is the whole point of a second device rather than a second port: no MULTIPORT
// negotiation, no control-queue port discovery, and the console keeps its own device, so
// either channel can fail without taking the other down.
//
// This program owns the transport only. The framing above it lives in `dev_protocol` and
// knows nothing about how the port was discovered, so a later multiport driver can host the
// same protocol on a named port without changing anything above this layer - and the
// development agent that will own the artifact registry can host it on a channel instead.
//
// Both queues are interrupt-driven. The receive side because a control channel is idle
// almost all the time, and a driver that polled it would spend the guest's whole life
// spinning - under a cooperative scheduler that is not a waste of cycles but a correctness
// problem, because a runnable spinner starves the threads that still have boot work to do.
// The transmit side because its completions are the only proof the buffer came back.

#![no_std]
#![no_main]

extern crate alloc;

mod common;
mod dev_protocol;
mod virtio;

use alloc::vec::Vec;
use rt::*;

use crate::dev_protocol::{HEADER_LEN, MAGIC, MAX_PAYLOAD, PARTIAL_FRAME_TICKS, SESSION_IDLE_TICKS, Session, Sink, VERSION};
use crate::virtio::{Queue, Virtio};

// The receive pool: enough slots that a burst of host writes is absorbed without the device
// running out of buffers between two interrupts, each large enough that an ordinary control
// frame arrives in one.
const RX_SLOTS: u16 = 8;
const RX_SLOT: u64 = 4096;

// How long a reply may wait for the transmit buffer to come back, in scheduler ticks
// (100 Hz). The host end can stop reading at any moment, and QEMU then stops consuming the
// transmit queue rather than discarding what it cannot deliver, so the buffer stays with the
// device. This bounds how long the guest tolerates that before declaring the host gone.
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
		rx.enable_interrupts();
		tx.enable_interrupts();
		let (rxpool, rx_virt, _): (u64, u64, u64) = match dma_buffer(RX_SLOTS as u64 * RX_SLOT) {
			Some(t) => t,
			None => exit(),
		};
		// One transmit buffer sized to the frame bound, so the largest reply the protocol can
		// produce is sent in one descriptor.
		let (_txbuf, tx_virt, tx_phys): (u64, u64, u64) = match dma_buffer((HEADER_LEN + MAX_PAYLOAD) as u64) {
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

// The transmit side of the port, and the reason it is not a simple synchronous write. The
// host end can stop reading at any moment - a killed tool, a full socket buffer, a terminal
// that went away - and QEMU responds by ceasing to consume the transmit queue rather than
// discarding what it cannot deliver. The device therefore keeps ownership of the buffer it
// was handed. A polled write that gave up on such a buffer would leave the device holding a
// descriptor that the next reply overwrites, which corrupts the ring permanently and takes
// the channel down for the rest of the guest's life. So completions are reaped explicitly
// and the buffer is never refilled until the device gives it back. Waiting for that is
// bounded; the port recovers by itself once the host reads again, precisely because the
// descriptor was never reused behind the device's back.
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
}

impl Sink for Port<'_> {
	// Write one frame to the port (virtio-serial buffers are raw bytes, no header), waiting
	// - bounded - for the previous frame to be taken first. Returns false when the host has
	// stopped consuming.
	fn send(&mut self, opcode: u8, request: u32, generation: u32, status: u16, payload: &[u8]) -> bool {
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
			header[8..12].copy_from_slice(&generation.to_le_bytes());
			header[12..14].copy_from_slice(&(payload.len() as u16).to_le_bytes());
			header[14..16].copy_from_slice(&status.to_le_bytes());
			core::ptr::copy_nonoverlapping(header.as_ptr(), self.virt as *mut u8, HEADER_LEN);
			core::ptr::copy_nonoverlapping(payload.as_ptr(), (self.virt + HEADER_LEN as u64) as *mut u8, payload.len());
			self.busy = self.tx.submit_async(&[(self.phys, (HEADER_LEN + payload.len()) as u32, false)]);
			self.busy
		}
	}
}

// Serve the channel: reap what the host wrote into the accumulator, hand it to the protocol,
// and block on the device interrupt when there is nothing left to do.
//
// Receive buffers are reaped before every wait rather than only after one. Both queues share
// the device's single MSI-X vector, so the wait inside a transmit drain can consume the very
// interrupt that announced newly arrived bytes; reaping first means the loop never blocks
// while the used ring still holds work, whichever wait observed the interrupt.
unsafe fn serve(device: &Virtio, irq: u64, rx: &mut Queue, port: &mut Port, rx_virt: u64, rx_phys: &[u64]) -> ! {
	unsafe {
		let mut session: Session = Session::new();
		let mut pending: Vec<u8> = Vec::with_capacity(HEADER_LEN + MAX_PAYLOAD + RX_SLOT as usize);
		// Two of the three deadlines this loop carries; the third, the open publication's,
		// belongs to the session and is read back from it. Zero means the deadline does not
		// apply.
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
				if !session.consume(&mut pending, port) {
					session.close();
					pending.clear();
				}
				// Rearm the deadlines against what the parse left behind. The fragment deadline
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
				session_at = if session.is_open() { now + SESSION_IDLE_TICKS } else { 0 };
				continue;
			}
			// Wait on whichever deadline comes first, and on none at all when the channel is
			// idle with no session open: nothing is then waiting to expire, so the driver must
			// not wake the scheduler even once.
			let deadline: u64 = soonest(&[fragment_at, session_at, session.publication_deadline()]);
			let ready: i64 = wait(irq, deadline);
			// Read the ISR to deassert the device's level-triggered INTx line before acking
			// (a harmless zero read on MSI-X, which is edge-triggered).
			let _ = device.read_isr();
			interrupt_ack(irq);
			if ready == ERR_TIMED_OUT {
				let now: u64 = clock();
				if fragment_at != 0 && now >= fragment_at {
					session.fail_partial(&mut pending, port);
					fragment_at = 0;
				}
				let publication_at: u64 = session.publication_deadline();
				if publication_at != 0 && now >= publication_at {
					session.expire_publication();
				}
				if session_at != 0 && now >= session_at {
					session.close();
					session_at = 0;
				}
			}
		}
	}
}

// The earliest deadline that applies, or zero when none does.
fn soonest(deadlines: &[u64]) -> u64 {
	let mut earliest: u64 = 0;
	for &at in deadlines {
		if at != 0 && (earliest == 0 || at < earliest) {
			earliest = at;
		}
	}
	earliest
}
