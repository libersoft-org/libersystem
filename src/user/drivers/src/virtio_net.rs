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

extern crate alloc;

mod common;
mod virtio;

use alloc::vec::Vec;
use rt::*;

use crate::virtio::{Queue, Virtio};

// The virtio_net_hdr prepended to every frame on both queues (VERSION_1: 12 bytes).
const NET_HDR_LEN: u64 = 12;
// The device-specific VIRTIO_NET_F_MTU feature: the device reports the link's MTU
// in config space (u16 at offset 10) - what the host side of the link carries.
const FEATURE_MTU: u32 = 1 << 3;
// The default MTU when the device does not report one: standard Ethernet.
const DEFAULT_MTU: u64 = 1500;
// The receive buffer pool: a handful of slots, each holding one full frame (the
// 12-byte header + an Ethernet frame of the link's MTU); the slot size follows
// the reported MTU, so jumbo links get jumbo slots.
const RX_SLOTS: u16 = 8;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// 1. bring the device up (asking for the MTU report) and receive our device's
		//    MSI-X Interrupt capability, then route this device's interrupts to MSI-X
		//    table entry 0 (DeviceManager acquired it with device_msix_acquire and the
		//    kernel programmed the table + enabled MSI-X).
		let mut device: Virtio = common::bringup_features(bootstrap, FEATURE_MTU);
		let irq: u64 = recv_irq(bootstrap);
		device.set_msix_vector(0);
		// read the NIC's MAC from device-specific config (our Ethernet source), and
		// the link's MTU when the device reports one - the buffers below follow it.
		let mut mac: [u8; 6] = [0u8; 6];
		for (i, b) in mac.iter_mut().enumerate() {
			*b = device.config_read(i as u64);
		}
		let mtu: u64 = if device.features_word0() & FEATURE_MTU != 0 {
			let v: u64 = device.config_read(10) as u64 | (device.config_read(11) as u64) << 8;
			if v == 0 { DEFAULT_MTU } else { v }
		} else {
			DEFAULT_MTU
		};
		// the largest L2 frame the link carries (MTU + the 14-byte Ethernet header),
		// and the receive slot / transmit buffer that holds it behind the virtio header.
		let frame_max: u64 = mtu + 14;
		let slot: u64 = NET_HDR_LEN + frame_max;
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
		let (rxpool, rx_virt, _): (u64, u64, u64) = match dma_buffer(RX_SLOTS as u64 * slot) {
			Some(t) => t,
			None => exit(),
		};
		let (_txbuf, tx_virt, tx_phys): (u64, u64, u64) = match dma_buffer(slot) {
			Some(t) => t,
			None => exit(),
		};
		// 3. post the receive pool and go live, each slot at its own physical address
		//    (looked up by offset) with the contiguous virtual mapping read back as
		//    `rx_virt + id * slot`.
		let mut rx_phys: [u64; RX_SLOTS as usize] = [0u64; RX_SLOTS as usize];
		let mut id: u16 = 0;
		while id < RX_SLOTS {
			rx_phys[id as usize] = dma_buffer_phys_at(rxpool, id as u64 * slot);
			rx.post_recv(id, rx_phys[id as usize], slot as u32);
			id += 1;
		}
		rx.notify();
		device.driver_ok();
		// 4. create the frame channel to NetworkService, hand its far end up with the
		//    online report (DeviceManager routes it to NetworkService), lead with the
		//    NIC's MAC and the link MTU (the service owns the stack and sizes its own
		//    frame buffers by the link), then move frames between the device and the
		//    service.
		let (frames, frames_far): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => exit(),
		};
		send_blocking(bootstrap, b"driver.virtio-net: online", frames_far);
		let mut macmsg: [u8; 11] = [0u8; 11];
		macmsg[..3].copy_from_slice(b"MAC");
		macmsg[3..9].copy_from_slice(&mac);
		macmsg[9..11].copy_from_slice(&(mtu as u16).to_le_bytes());
		send_blocking(frames, &macmsg, 0);
		move_frames(irq, frames, &mut rx, &tx, rx_virt, &rx_phys, tx_virt, tx_phys, slot)
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
unsafe fn transmit_frame(tx: &Queue, tx_virt: u64, tx_phys: u64, slot: u64, frame: &[u8]) {
	unsafe {
		if frame.is_empty() || frame.len() > (slot - NET_HDR_LEN) as usize {
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
unsafe fn move_frames(irq: u64, frames: u64, rx: &mut Queue, tx: &Queue, rx_virt: u64, rx_phys: &[u64], tx_virt: u64, tx_phys: u64, slot: u64) -> ! {
	unsafe {
		let mut frame: Vec<u8> = alloc::vec![0u8; (slot - NET_HDR_LEN) as usize];
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
						let f: &[u8] = core::slice::from_raw_parts((rx_virt + id as u64 * slot + NET_HDR_LEN) as *const u8, (len as u64 - NET_HDR_LEN) as usize);
						send_blocking(frames, f, 0);
					}
					rx.post_recv(id, rx_phys[id as usize], slot as u32);
				}
				rx.notify();
				interrupt_ack(irq);
			} else if ready == 1 {
				match recv_blocking(frames, &mut frame) {
					Received::Message { len, .. } => transmit_frame(tx, tx_virt, tx_phys, slot, &frame[..len]),
					Received::Closed => service_open = false,
				}
			}
		}
	}
}
