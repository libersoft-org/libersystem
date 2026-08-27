// driver.dev-channel - the development channel port.
//
// The host attaches a SECOND single-port virtio-serial device, pinned to a fixed PCI
// address, and DeviceManager binds this program to it instead of the console driver. That
// is the whole point of a second device rather than a second port: no MULTIPORT
// negotiation, no control-queue port discovery, and the console keeps its own device, so
// either channel can fail without taking the other down.
//
// This program is a transport and nothing else. It moves raw bytes between the port and one
// channel it hands up to DeviceManager, which routes it to the development agent; the
// framing, the session and the artifact registry all live there. A driver holds a device
// capability and an MMIO mapping, and that is not where megabytes of unverified bytes a host
// streamed in belong. Nothing here knows what a frame is, so a later multiport driver can
// carry the same protocol on a named port without anything above it changing.
//
// Both queues are interrupt-driven. The receive side because a control channel is idle
// almost all the time, and a driver that polled it would spend the guest's whole life
// spinning - under a cooperative scheduler that is not a waste of cycles but a correctness
// problem, because a runnable spinner starves the threads that still have boot work to do.
// The transmit side because its completions are the only proof the buffer came back.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use rt::*;

use crate::virtio::{Queue, Virtio};
use drivers::{common, virtio};

// The receive pool: enough slots that a burst of host writes is absorbed without the device
// running out of buffers between two interrupts, each large enough that an ordinary control
// frame arrives in one.
const RX_SLOTS: u16 = 8;
const RX_SLOT: u64 = 4096;

// The largest frame the protocol above defines, which is the largest message the agent can
// hand down and therefore the transmit buffer this driver needs. The driver never inspects a
// frame; it only has to be able to carry one.
const MAX_FRAME: usize = 65536;

// How long a write may wait for the transmit buffer to come back, in scheduler ticks
// (100 Hz). The host end can stop reading at any moment, and QEMU then stops consuming the
// transmit queue rather than discarding what it cannot deliver, so the buffer stays with the
// device.
//
// This matches the session's own idle deadline rather than being an independent, shorter
// guess. A shorter one drops replies from a host that is merely slow to read - a host
// pipelining up to the advertised outstanding bound can easily have more reply bytes in
// flight than a socket buffer holds, and losing its answers for reading a moment late is not
// a bound, it is data loss. A host that has genuinely gone is caught by the same window the
// session uses to decide the same thing, which is the only question this deadline is really
// asking.
const TX_DRAIN_TICKS: u64 = 3000;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// Bring the device up and take its MSI-X Interrupt, then route this device's
		// interrupts to table entry 0 (DeviceManager acquired it and the kernel programmed
		// the table), before the queues are set up so each queue is told the vector.
		let (bind, resources) = common::handshake(bootstrap);
		let mut device: Virtio = common::bringup_bound(bootstrap, &bind, &resources, 0);
		let irq: u64 = resources.irq;
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
		let (rxpool, rx_virt, _): (u64, u64, u64) = match dma_buffer_for(device.capability, RX_SLOTS as u64 * RX_SLOT) {
			Some(t) => t,
			None => exit(),
		};
		let (_txbuf, tx_virt, tx_phys): (u64, u64, u64) = match dma_buffer_for(device.capability, MAX_FRAME as u64) {
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
		// Create the byte channel and hand its far end up with the online report, the way
		// every driver with a service above it does. DeviceManager routes it to the agent.
		let (bytes, bytes_far): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => exit(),
		};
		common::online(bootstrap, &bind, b"driver.dev-channel: online (transport)", &[(driver_protocol::provider::CONSOLE_BYTES, bytes_far)]);
		let mut port: Port = Port { device: &device, irq, tx: &mut tx, virt: tx_virt, phys: tx_phys, busy: false };
		pump(&device, &bind, irq, bootstrap, bytes, &mut rx, &mut port, rx_virt, &rx_phys)
	}
}

// The transmit side of the port, and the reason it is not a simple synchronous write. The
// host end can stop reading at any moment - a killed tool, a full socket buffer, a terminal
// that went away - and QEMU responds by ceasing to consume the transmit queue rather than
// discarding what it cannot deliver. The device therefore keeps ownership of the buffer it
// was handed. A polled write that gave up on such a buffer would leave the device holding a
// descriptor that the next write overwrites, which corrupts the ring permanently and takes
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
	// buffer for the next write.
	unsafe fn reclaim(&mut self) {
		unsafe {
			while self.tx.take_used().is_some() {
				self.busy = false;
			}
		}
	}

	// Write bytes to the port, waiting - bounded - for the previous write to be taken first.
	// Returns false when the host has stopped consuming.
	unsafe fn write(&mut self, payload: &[u8]) -> bool {
		unsafe {
			if payload.is_empty() || payload.len() > MAX_FRAME {
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
			core::ptr::copy_nonoverlapping(payload.as_ptr(), self.virt as *mut u8, payload.len());
			self.busy = self.tx.submit_async(&[(self.phys, payload.len() as u32, false)]);
			self.busy
		}
	}
}

// Move bytes between the port and the agent's channel, in both directions, forever.
//
// Receive buffers and channel messages are both drained before every wait rather than only
// after one. Both queues share the device's single MSI-X vector, so the wait inside a
// transmit drain can consume the very interrupt that announced newly arrived bytes; draining
// first means the loop never blocks while work is already queued, whichever wait observed it.
unsafe fn pump(device: &Virtio, bind: &common::Bind, irq: u64, bootstrap: u64, bytes: u64, rx: &mut Queue, port: &mut Port, rx_virt: u64, rx_phys: &[u64]) -> ! {
	unsafe {
		let mut outbound: Vec<u8> = alloc::vec![0u8; MAX_FRAME];
		let mut bytes: u64 = bytes;
		loop {
			let mut worked: bool = false;
			// Port to agent.
			while let Some((id, len)) = rx.take_used() {
				if id < RX_SLOTS && len > 0 {
					let n: usize = if len as u64 > RX_SLOT { RX_SLOT as usize } else { len as usize };
					let chunk: &[u8] = core::slice::from_raw_parts((rx_virt + id as u64 * RX_SLOT) as *const u8, n);
					// The agent going away leaves the port with nobody above it. The port itself
					// is fine, so this driver waits for the replacement rather than taking the
					// device down with the process that was using it - the bytes in flight are
					// lost with the session they belonged to, which is what a disconnect means.
					if !send_blocking(bytes, chunk, 0) {
						bytes = adopt(device, irq, bootstrap, bytes, rx, rx_phys);
					}
					worked = true;
				}
				rx.post_recv(id, rx_phys[id as usize], RX_SLOT as u32);
			}
			if worked {
				rx.notify();
			}
			// Agent to port. A frame the port would not take within its deadline is not
			// silently dropped: the agent is told, with an empty message, that the write
			// failed. Empty is unambiguous because every frame is at least a header long, and
			// telling the agent is what turns a host that stopped reading into a session that
			// ends deterministically rather than into replies that quietly disappear.
			// Polled rather than peeked, because a channel whose peer is gone reports nothing to
			// read and stays ready forever: peeking would find no message, the wait below would
			// return immediately every time, and this loop would spin at the expense of every
			// other thread. The closure has to be observed where it is, not inferred from the
			// absence of a message.
			loop {
				match try_recv(bytes, &mut outbound) {
					Polled::Message { len, .. } => {
						if !port.write(&outbound[..len]) && !send_blocking(bytes, &[], 0) {
							bytes = adopt(device, irq, bootstrap, bytes, rx, rx_phys);
						}
						worked = true;
					}
					Polled::Empty => break,
					Polled::Closed => {
						bytes = adopt(device, irq, bootstrap, bytes, rx, rx_phys);
						break;
					}
				}
			}
			if worked {
				continue;
			}
			// Nothing left either way: block on the device interrupt and the agent's channel
			// at once, waking on whichever speaks first.
			let ready: i64 = wait_any(&[irq, bytes], 0);
			if ready == 0 {
				// Read the ISR to deassert the device's level-triggered INTx line before
				// acking (a harmless zero read on MSI-X, which is edge-triggered).
				let _ = device.read_isr();
				interrupt_ack(irq);
			}
			port.reclaim();
		}
	}
}

// The agent above this driver is gone. Drop the dead channel and wait for DeviceManager,
// which supervises the agent, to hand down the channel its replacement is listening on.
//
// The receive ring keeps turning while waiting, and what arrives on it is discarded. Both
// halves of that matter. Discarding is right because the bytes belong to a session that died
// with the agent, and a fragment of it delivered to a fresh agent would be read as the
// beginning of a frame; the protocol's fragment deadline is what clears the stream, and it is
// the host that reconnects. Turning the ring is right because eight buffers is all the device
// has: stopping here would leave it with nowhere to put what a host is still writing, and the
// port would still be stalled once the new agent arrived.
unsafe fn adopt(device: &Virtio, irq: u64, bootstrap: u64, dead: u64, rx: &mut Queue, rx_phys: &[u64]) -> u64 {
	unsafe {
		close(dead);
		let mut buf: [u8; 16] = [0u8; 16];
		loop {
			let mut recycled: bool = false;
			while let Some((id, _)) = rx.take_used() {
				if id < RX_SLOTS {
					rx.post_recv(id, rx_phys[id as usize], RX_SLOT as u32);
					recycled = true;
				}
			}
			if recycled {
				rx.notify();
			}
			loop {
				match try_recv(bootstrap, &mut buf) {
					Polled::Message { len, handle } if handle != 0 && len >= 5 && &buf[..5] == b"BYTES" => return handle,
					// THE MANAGER'S PING LANDS HERE, in the branch that used to close whatever
					// arrived and say nothing. This driver already reads its bootstrap without
					// blocking, so the ping is answered by the loop that was already looking.
					Polled::Message { len, handle } => {
						if handle != 0 {
							close(handle);
						}
						if let Ok(header) = driver_protocol::Header::decode(&buf[..len])
							&& header.generation == bind.generation
							&& header.opcode == driver_protocol::Opcode::Ping
							&& let Ok(sequence) = driver_protocol::decode_sequence(header.payload(&buf))
							&& !common::pong(bootstrap, bind, sequence)
						{
							exit();
						}
					}
					Polled::Empty => break,
					// The supervisor dropped this driver's bootstrap, which is how it is told to
					// shut down - and with it goes any prospect of a replacement.
					Polled::Closed => exit(),
				}
			}
			let ready: i64 = wait_any(&[irq, bootstrap], 0);
			if ready == 0 {
				let _ = device.read_isr();
				interrupt_ack(irq);
			}
		}
	}
}
