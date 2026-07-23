// driver.virtio-console - the userspace virtio serial/console driver.
//
// We do not negotiate MULTIPORT, so the device is a single console port: queue 0 is
// the receive queue, queue 1 the transmit queue, and the port is always open. After
// bringing the device up the driver writes a banner to the console over the
// transmit virtqueue (it lands on QEMU's console chardev).

#![no_std]
#![no_main]

mod common;
mod virtio;

use rt::*;

use crate::virtio::Queue;

// The line the driver writes over the console transmit queue.
const BANNER: &[u8] = b"virtio-console driver online: console output over the virtqueue\n";

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let device = common::bringup(bootstrap);
		// single-port virtio-console: receiveq = 0, transmitq = 1.
		let _rx = device.setup_queue(0);
		let tx = device.setup_queue(1);
		device.driver_ok();
		let ok = match tx {
			Some(q) => write_console(&q, BANNER),
			None => false,
		};
		let report: &[u8] = if ok { b"driver.virtio-console: online (console tx ok)" } else { b"driver.virtio-console: online" };
		common::online_and_stand(bootstrap, report)
	}
}

// Write `bytes` to the console over the transmit queue (virtio-console transmit
// buffers are raw bytes, no header).
unsafe fn write_console(tx: &Queue, bytes: &[u8]) -> bool {
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
