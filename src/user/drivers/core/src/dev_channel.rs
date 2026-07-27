// driver.dev-channel - the development channel port.
//
// The host attaches a SECOND single-port virtio-serial device, pinned to a fixed PCI
// address, and DeviceManager binds this program to it instead of the console driver. That
// is the whole point of a second device rather than a second port: no MULTIPORT
// negotiation, no control-queue port discovery, and the console keeps its own device, so
// either channel can fail without taking the other down.
//
// This owns the transport only. The framing above it is carried in the port's byte stream
// and knows nothing about how the port was discovered, so a later multiport driver can host
// the same protocol on a named port without changing anything above this layer.

#![no_std]
#![no_main]

mod common;
mod virtio;

use rt::*;

use crate::virtio::Queue;

// Written once to the host end so an attached runner can tell the channel is bound and
// live. It is the only thing this program puts on the wire until the protocol lands.
const HELLO: &[u8] = b"liber-dev-channel: port bound\n";

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let device = common::bringup(bootstrap);
		// Single port, exactly like the console device: receiveq = 0, transmitq = 1.
		let _rx = device.setup_queue(0);
		let tx = device.setup_queue(1);
		device.driver_ok();
		let ok = match tx {
			Some(q) => write_port(&q, HELLO),
			None => false,
		};
		let report: &[u8] = if ok { b"driver.dev-channel: online (port tx ok)" } else { b"driver.dev-channel: online" };
		common::online_and_stand(bootstrap, report)
	}
}

// Write `bytes` to the port over the transmit queue (virtio-serial transmit buffers are
// raw bytes, no header).
unsafe fn write_port(tx: &Queue, bytes: &[u8]) -> bool {
	unsafe {
		let (_handle, virt, phys): (u64, u64, u64) = match dma_buffer(4096) {
			Some(t) => t,
			None => return false,
		};
		let n: usize = if bytes.len() < 4096 { bytes.len() } else { 4096 };
		for (i, &b) in bytes[..n].iter().enumerate() {
			((virt + i as u64) as *mut u8).write_volatile(b);
		}
		tx.submit(&[(phys, n as u32, false)]).is_some()
	}
}
