// driver.virtio-blk - the userspace virtio block-device driver.
//
// After bringing the device up it proves the block data path over the virtqueue by
// reading sector 0 and checking it holds a `PKGARCH1` volume archive (laid down on
// the disk at boot), then reports in. StorageService (M26) reads its `vol://system`
// volume from this device over the block-read channel the driver serves.

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
			Some(q) => verify_archive(&q),
			None => false,
		};
		let report: &[u8] = if ok { b"driver.virtio-blk: online (volume archive on disk)" } else { b"driver.virtio-blk: online" };
		common::online_and_stand(bootstrap, report)
	}
}

// Read sector 0 from the disk and verify it carries a PKGARCH1 volume archive,
// exercising the block read path end to end over the virtqueue. Returns true if the
// magic matches (the archive was laid down on the device).
unsafe fn verify_archive(queue: &Queue) -> bool {
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

		// READ sector 0 (the device writes the data buffer).
		if !request(queue, virt, phys, 0, BLK_T_IN, true) {
			return false;
		}
		// the sector must start with the PKGARCH1 archive magic.
		for (i, &b) in PKG_MAGIC.iter().enumerate() {
			if ((virt + DATA_OFF + i as u64) as *const u8).read_volatile() != b {
				return false;
			}
		}
		true
	}
}

// Issue one block request for sector `lba`: fill the header, submit the
// three-buffer descriptor chain (header, data, status), and check the device
// reported success. `data_is_written` is whether the device writes the data buffer
// (a read).
unsafe fn request(queue: &Queue, virt: u64, phys: u64, lba: u64, kind: u32, data_is_written: bool) -> bool {
	unsafe {
		(virt as *mut u32).write_volatile(kind); // type
		((virt + 4) as *mut u32).write_volatile(0); // reserved
		((virt + 8) as *mut u64).write_volatile(lba); // sector
		((virt + STATUS_OFF) as *mut u8).write_volatile(0xff); // status sentinel
		let bufs: [(u64, u32, bool); 3] = [(phys + HDR_OFF, 16, false), (phys + DATA_OFF, SECTOR, data_is_written), (phys + STATUS_OFF, 1, true)];
		queue.submit(&bufs).is_some() && ((virt + STATUS_OFF) as *const u8).read_volatile() == BLK_S_OK
	}
}
