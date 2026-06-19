// driver.virtio-blk - the userspace virtio block-device driver.
//
// After bringing the device up it proves the block data path over the virtqueue:
// it writes a known pattern to sector 0 and reads it back, then reports in with the
// result. Real filesystem-level use is for StorageService over this driver (M26).

#![no_std]
#![no_main]

mod common;
mod virtio;

use rt::*;

use crate::virtio::Queue;

// One disk sector.
const SECTOR: u32 = 512;

// virtio-blk request types and the success status.
const BLK_T_IN: u32 = 0; // read (device writes the data buffer)
const BLK_T_OUT: u32 = 1; // write (device reads the data buffer)
const BLK_S_OK: u8 = 0;

// Request-buffer layout within one DMA page: a 16-byte header, the 512-byte data
// sector, then the 1-byte status.
const HDR_OFF: u64 = 0;
const DATA_OFF: u64 = 512;
const STATUS_OFF: u64 = 1024;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let device = common::bringup(bootstrap);
		// virtio-blk has a single request queue (queue 0).
		let queue = device.setup_queue(0);
		device.driver_ok();
		let ok = match queue {
			Some(q) => self_test(&q),
			None => false,
		};
		let report: &[u8] = if ok { b"driver.virtio-blk: online (sector r/w ok)" } else { b"driver.virtio-blk: online" };
		common::online_and_stand(bootstrap, report)
	}
}

// Write a recognizable pattern to sector 0 and read it back, exercising the block
// data path end to end over the virtqueue. Returns true if the round-trip matches.
unsafe fn self_test(queue: &Queue) -> bool {
	unsafe {
		// a DMA buffer for the request header, data sector, and status byte.
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

		// fill the data buffer with a recognizable pattern.
		for i in 0..SECTOR as u64 {
			((virt + DATA_OFF + i) as *mut u8).write_volatile((i as u8) ^ 0x5a);
		}
		// WRITE sector 0 (device reads the data buffer).
		if !request(queue, virt, phys, BLK_T_OUT, false) {
			return false;
		}
		// clobber the buffer, then READ sector 0 back (device writes the data buffer).
		for i in 0..SECTOR as u64 {
			((virt + DATA_OFF + i) as *mut u8).write_volatile(0);
		}
		if !request(queue, virt, phys, BLK_T_IN, true) {
			return false;
		}
		// the pattern must have survived the disk round-trip.
		for i in 0..SECTOR as u64 {
			if ((virt + DATA_OFF + i) as *const u8).read_volatile() != (i as u8) ^ 0x5a {
				return false;
			}
		}
		true
	}
}

// Issue one block request for sector 0: fill the header, submit the three-buffer
// descriptor chain (header, data, status), and check the device reported success.
// `data_is_written` is whether the device writes the data buffer (a read).
unsafe fn request(queue: &Queue, virt: u64, phys: u64, kind: u32, data_is_written: bool) -> bool {
	unsafe {
		(virt as *mut u32).write_volatile(kind); // type
		((virt + 4) as *mut u32).write_volatile(0); // reserved
		((virt + 8) as *mut u64).write_volatile(0); // sector 0
		((virt + STATUS_OFF) as *mut u8).write_volatile(0xff); // status sentinel
		let bufs: [(u64, u32, bool); 3] = [(phys + HDR_OFF, 16, false), (phys + DATA_OFF, SECTOR, data_is_written), (phys + STATUS_OFF, 1, true)];
		queue.submit(&bufs).is_some() && ((virt + STATUS_OFF) as *const u8).read_volatile() == BLK_S_OK
	}
}
