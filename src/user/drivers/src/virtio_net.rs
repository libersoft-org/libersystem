// driver.virtio-net - the userspace virtio network-device frame-mover.
//
// It brings the NIC up and drives both virtqueues: the receive queue (0) is
// interrupt-driven (DeviceManager hands it the device's Interrupt, like the
// virtio-input driver), and the transmit queue (1) is polled synchronously. Since
// M33 the driver carries no network stack - it is a pure frame-mover. Over a single
// channel to NetworkService it forwards every frame the device receives and
// transmits every frame the service hands back, standing on its IRQ and that
// channel at once with `wait_any`. The L2/L3 protocol lives in NetworkService.

#![no_std]
#![no_main]

mod common;
mod virtio;

use rt::*;

use crate::virtio::{Queue, Virtio};

// The virtio_net_hdr prepended to every frame on both queues (VERSION_1: 12 bytes).
const NET_HDR_LEN: u64 = 12;
// The receive buffer pool: a handful of slots, each holding one full frame (the
// 12-byte header + an up-to-1514-byte Ethernet frame fits comfortably in 2 kB).
const RX_SLOTS: u16 = 8;
const RX_SLOT: u64 = 2048;
// The transmit scratch buffer (one frame at a time, copied in then submitted).
const TX_BUF: u64 = 4096;
// The largest L2 frame we move to/from NetworkService (1514 bytes with slack), the
// size of the channel receive buffer for outbound frames.
const FRAME_MAX: usize = 2048;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// 1. bring the device up and receive our device's MSI-X Interrupt capability, then
		//    route this device's interrupts to MSI-X table entry 0 (DeviceManager acquired it
		//    with device_msix_acquire and the kernel programmed the table + enabled MSI-X).
		let mut device: Virtio = common::bringup(bootstrap);
		let irq: u64 = recv_irq(bootstrap);
		device.set_msix_vector(0);
		// read the NIC's MAC from device-specific config (our Ethernet source).
		let mut mac: [u8; 6] = [0u8; 6];
		for (i, b) in mac.iter_mut().enumerate() {
			*b = device.config_read(i as u64);
		}
		// 2. set up the queues: receiveq 0 (interrupt-driven), transmitq 1 (polled).
		let mut rx: Queue = match device.setup_queue(0) {
			Some(q) => q,
			None => exit(),
		};
		let tx: Queue = match device.setup_queue(1) {
			Some(q) => q,
			None => exit(),
		};
		rx.enable_interrupts();
		let (rxpool, rx_virt, _): (u64, u64, u64) = match dma_buffer(RX_SLOTS as u64 * RX_SLOT) {
			Some(t) => t,
			None => exit(),
		};
		let (_txbuf, tx_virt, tx_phys): (u64, u64, u64) = match dma_buffer(TX_BUF) {
			Some(t) => t,
			None => exit(),
		};
		// 3. post the receive pool and go live. The pool spans several physical frames
		//    that are not contiguous, so each slot is posted at its own true physical
		//    address (looked up by offset) while the contiguous virtual mapping is read
		//    back with `rx_virt + id * RX_SLOT`.
		let mut rx_phys: [u64; RX_SLOTS as usize] = [0u64; RX_SLOTS as usize];
		let mut id: u16 = 0;
		while id < RX_SLOTS {
			rx_phys[id as usize] = dma_buffer_phys_at(rxpool, id as u64 * RX_SLOT);
			rx.post_recv(id, rx_phys[id as usize], RX_SLOT as u32);
			id += 1;
		}
		rx.notify();
		device.driver_ok();
		// 4. create the frame channel to NetworkService, hand its far end up with the
		//    online report (DeviceManager routes it to NetworkService), lead with the
		//    NIC's MAC (the service owns the stack and needs our hardware address), then
		//    move frames between the device and the service.
		let (frames, frames_far): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => exit(),
		};
		send_blocking(bootstrap, b"driver.virtio-net: online", frames_far);
		let mut macmsg: [u8; 9] = [0u8; 9];
		macmsg[..3].copy_from_slice(b"MAC");
		macmsg[3..9].copy_from_slice(&mac);
		send_blocking(frames, &macmsg, 0);
		move_frames(irq, frames, &mut rx, &tx, rx_virt, &rx_phys, tx_virt, tx_phys)
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

// Transmit one L2 frame NetworkService handed back: copy it after the
// virtio_net_hdr in the transmit buffer, zero the header, and submit it on the
// transmit queue. A frame that does not fit the buffer is dropped.
unsafe fn transmit_frame(tx: &Queue, tx_virt: u64, tx_phys: u64, frame: &[u8]) {
	unsafe {
		if frame.is_empty() || frame.len() > (TX_BUF - NET_HDR_LEN) as usize {
			return;
		}
		core::ptr::write_bytes(tx_virt as *mut u8, 0, NET_HDR_LEN as usize);
		core::ptr::copy_nonoverlapping(frame.as_ptr(), (tx_virt + NET_HDR_LEN) as *mut u8, frame.len());
		tx.submit(&[(tx_phys, NET_HDR_LEN as u32 + frame.len() as u32, false)]);
	}
}

// Move frames between the device and NetworkService, standing on the device IRQ and
// the service's frame channel at once (wait_any). On an interrupt: drain the receive
// ring (forwarding each frame to the service), then re-post the buffers and re-arm
// (MSI-X is edge-triggered, so there is no ISR line to deassert). On a channel
// message: transmit the frame the service handed back. The channel closing
// (NetworkService gone) leaves us draining the device alone.
#[allow(clippy::too_many_arguments)]
unsafe fn move_frames(irq: u64, frames: u64, rx: &mut Queue, tx: &Queue, rx_virt: u64, rx_phys: &[u64], tx_virt: u64, tx_phys: u64) -> ! {
	unsafe {
		let mut frame: [u8; FRAME_MAX] = [0u8; FRAME_MAX];
		let mut service_open: bool = true;
		loop {
			let ready: i64 = if service_open {
				wait_any(&[irq, frames], 0)
			} else {
				wait(irq, 0);
				0
			};
			if ready == 0 {
				while let Some((id, len)) = rx.take_used() {
					if id < RX_SLOTS && len as u64 > NET_HDR_LEN {
						let f: &[u8] = core::slice::from_raw_parts((rx_virt + id as u64 * RX_SLOT + NET_HDR_LEN) as *const u8, (len as u64 - NET_HDR_LEN) as usize);
						send_blocking(frames, f, 0);
					}
					rx.post_recv(id, rx_phys[id as usize], RX_SLOT as u32);
				}
				rx.notify();
				interrupt_ack(irq);
			} else if ready == 1 {
				match recv_blocking(frames, &mut frame) {
					Received::Message { len, .. } => transmit_frame(tx, tx_virt, tx_phys, &frame[..len]),
					Received::Closed => service_open = false,
				}
			}
		}
	}
}
