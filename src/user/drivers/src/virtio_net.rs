// driver.virtio-net - the userspace virtio network-device driver (the driver only;
// the network stack is a later phase).
//
// After bringing the device up it reads the NIC's MAC from device config and
// transmits one minimal broadcast Ethernet frame on the transmit virtqueue,
// proving the network driver drives its device over the queue. Receiving and a real
// network stack are phase 2.

#![no_std]
#![no_main]

mod common;
mod virtio;

use rt::*;

use crate::virtio::Queue;

// virtio_net_hdr length (VERSION_1: includes num_buffers) and a minimal Ethernet
// frame (no FCS), padded to the 60-byte minimum.
const NET_HDR_LEN: u64 = 12;
const FRAME_LEN: u64 = 60;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let device = common::bringup(bootstrap);
		// read the NIC's MAC from device-specific config (used as the frame source).
		let mut mac: [u8; 6] = [0u8; 6];
		for (i, b) in mac.iter_mut().enumerate() {
			*b = device.config_read(i as u64);
		}
		// virtio-net queues: receiveq = 0, transmitq = 1.
		let _rx = device.setup_queue(0);
		let tx = device.setup_queue(1);
		device.driver_ok();
		let ok = match tx {
			Some(q) => transmit_frame(&q, &mac),
			None => false,
		};
		let report: &[u8] = if ok { b"driver.virtio-net: online (frame tx ok)" } else { b"driver.virtio-net: online" };
		common::online_and_stand(bootstrap, report)
	}
}

// Transmit one minimal broadcast Ethernet frame on the transmit queue.
unsafe fn transmit_frame(tx: &Queue, mac: &[u8; 6]) -> bool {
	unsafe {
		let handle: i64 = dma_buffer_create(4096);
		if handle < 0 {
			return false;
		}
		let virt: i64 = dma_buffer_map(handle as u64);
		if sys_is_err(virt as u64) {
			return false;
		}
		let virt: u64 = virt as u64;
		let phys: u64 = dma_buffer_phys(handle as u64);
		let total: u64 = NET_HDR_LEN + FRAME_LEN;
		core::ptr::write_bytes(virt as *mut u8, 0, total as usize);

		// the virtio_net_hdr stays all zeros (no checksum/GSO offloads). The frame
		// follows: destination = broadcast, source = our MAC, ethertype = 0x0800.
		let frame: u64 = virt + NET_HDR_LEN;
		for i in 0..6u64 {
			((frame + i) as *mut u8).write_volatile(0xff);
			((frame + 6 + i) as *mut u8).write_volatile(mac[i as usize]);
		}
		((frame + 12) as *mut u8).write_volatile(0x08);
		((frame + 13) as *mut u8).write_volatile(0x00);
		// the rest of the frame is zero padding to the minimum size.

		tx.submit(&[(phys, total as u32, false)]).is_some()
	}
}
