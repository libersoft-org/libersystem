// driver.virtio-console - the userspace virtio serial/console driver.
//
// We do not negotiate MULTIPORT, so the device is a single console port: queue 0 is
// the receive queue, queue 1 the transmit queue, and the port is always open. After
// bringing the device up the driver writes a banner to the console over the
// transmit virtqueue (it lands on QEMU's console chardev).

#![no_std]
#![no_main]

use rt::*;

use crate::virtio::Queue;
use drivers::{common, virtio};

// The line the driver writes over the console transmit queue.
const BANNER: &[u8] = b"virtio-console driver online: console output over the virtqueue\n";

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let (bind, device) = common::bringup(bootstrap);
		// single-port virtio-console: receiveq = 0, transmitq = 1.
		let _rx = device.setup_queue(0);
		let tx = device.setup_queue(1);
		device.driver_ok();
		// KEPT, NOT DROPPED: the queue's capability is what `finish_stop` hands to `device_quiesced`
		// when the manager asks this driver to stop, so the kernel may reclaim the frames and masked
		// vectors this binding was holding. It used to go out of scope with the `match` and the stop
		// path had nothing to name.
		// AND THE CAPABILITY IS THE DEVICE'S, NOT THE QUEUE'S (2026-09-03). `Queue::capability` IS
		// this device's capability - every queue of a device carries the same one - so taking it
		// from the transmit queue and falling back to zero when that queue could not be set up threw
		// away a capability the driver still holds. The receive queue may already own DMA on that
		// path, and `finish_stop` skips `device_quiesced` for zero and sends `STOPPED` regardless:
		// a clean stop with nothing behind it.
		let ok: bool = match &tx {
			Some(q) => write_console(q, BANNER),
			None => false,
		};
		let queue_capability: u64 = device.capability;
		let mut line = [0u8; 64];
		let n = common::describe(&mut line, b"virtio-console", &device, if ok { b"tx ok" } else { b"tx failed" });
		let report: &[u8] = &line[..n];
		common::online_and_stand(bootstrap, &bind, report, 0, 0, queue_capability)
	}
}

// Write `bytes` to the console over the transmit queue (virtio-console transmit
// buffers are raw bytes, no header).
unsafe fn write_console(tx: &Queue, bytes: &[u8]) -> bool {
	unsafe {
		let (_handle, virt, phys): (u64, u64, u64) = match dma_buffer_for(tx.capability, 4096) {
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
