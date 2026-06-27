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
const BLK_T_OUT: u32 = 1; // write (device reads the data buffer)
const BLK_S_OK: u8 = 0;

// The most sectors served per block request (one DMA page).
const MAX_SECTORS: u32 = 8;

// Block-service request opcodes (the leading u32 of each request).
const OP_READ: u32 = 0;
const OP_WRITE: u32 = 1;

// Block-service reply status codes.
const STATUS_OK: u32 = 0;
const STATUS_ERR: u32 = 1;

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
		let queue: Queue = match queue {
			Some(q) => q,
			None => common::online_and_stand(bootstrap, b"driver.virtio-blk: online"),
		};
		let ok: bool = verify_archive(&queue);
		// create the block-read service channel; hand the client end up to
		// DeviceManager with our report (it routes it on to StorageService), then
		// serve block reads on the server end until that client closes.
		let (blk_server, blk_client): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => common::online_and_stand(bootstrap, b"driver.virtio-blk: online"),
		};
		let report: &[u8] = if ok { b"driver.virtio-blk: online (volume archive on disk)" } else { b"driver.virtio-blk: online" };
		send_blocking(bootstrap, report, blk_client);
		serve_blocks(&queue, blk_server)
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

// Serve block requests on `blk_server` until the client closes its end. Each request
// is [op u32][lba u64][count u32] (count clamped to one DMA page = 8 sectors). A read
// (op 0) replies [status u32] carrying, on success, a MemoryObject capability to a
// freshly filled buffer of count*512 bytes - the sectors read off the disk. A write
// (op 1) carries a transferred MemoryObject of count*512 bytes the device reads onto
// the disk, and replies [status u32] with no buffer. The data crosses as a shared
// buffer handle, never copied through the channel.
unsafe fn serve_blocks(queue: &Queue, blk_server: u64) -> ! {
	unsafe {
		// one reusable DMA buffer the device reads/writes each sector through.
		let dma: i64 = dma_buffer_create(4096);
		if dma < 0 {
			exit();
		}
		let virt: i64 = dma_buffer_map(dma as u64);
		if sys_is_err(virt as u64) {
			exit();
		}
		let virt: u64 = virt as u64;
		let phys: u64 = dma_buffer_phys(dma as u64);
		let mut req: [u8; 16] = [0u8; 16];
		loop {
			match recv_blocking(blk_server, &mut req) {
				Received::Message { len, handle } if len >= 16 => {
					let op: u32 = u32::from_le_bytes([req[0], req[1], req[2], req[3]]);
					let lba: u64 = u64::from_le_bytes([req[4], req[5], req[6], req[7], req[8], req[9], req[10], req[11]]);
					let count: u32 = u32::from_le_bytes([req[12], req[13], req[14], req[15]]).clamp(1, MAX_SECTORS);
					match op {
						OP_READ => serve_read(queue, blk_server, virt, phys, lba, count),
						OP_WRITE => serve_write(queue, blk_server, virt, phys, lba, count, handle),
						_ => {
							if handle != 0 {
								close(handle);
							}
							reply_block(blk_server, STATUS_ERR, 0);
						}
					}
				}
				_ => exit(),
			}
		}
	}
}

// Read `count` sectors starting at `lba` into a fresh shared buffer and hand it to
// the client, or reply with an error status and no buffer on any failure.
unsafe fn serve_read(queue: &Queue, blk_server: u64, virt: u64, phys: u64, lba: u64, count: u32) {
	unsafe {
		let bytes: u64 = count as u64 * SECTOR as u64;
		let obj: u64 = syscall(SYS_MEMORY_OBJECT_CREATE, bytes, 0, 0, 0);
		if sys_is_err(obj) {
			reply_block(blk_server, STATUS_ERR, 0);
			return;
		}
		let dst: u64 = match map_object(obj) {
			Some(base) => base,
			None => {
				close(obj);
				reply_block(blk_server, STATUS_ERR, 0);
				return;
			}
		};
		// read each sector into the DMA buffer, then copy it into the shared buffer.
		let mut s: u32 = 0;
		let mut ok: bool = true;
		while s < count {
			if !request(queue, virt, phys, lba + s as u64, BLK_T_IN, true) {
				ok = false;
				break;
			}
			core::ptr::copy_nonoverlapping((virt + DATA_OFF) as *const u8, (dst + s as u64 * SECTOR as u64) as *mut u8, SECTOR as usize);
			s += 1;
		}
		unmap_object(obj);
		if !ok {
			close(obj);
			reply_block(blk_server, STATUS_ERR, 0);
			return;
		}
		// attenuate to read+map plus the transfer right, then hand the buffer over.
		let granted: i64 = duplicate(obj, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		close(obj);
		if granted < 0 {
			reply_block(blk_server, STATUS_ERR, 0);
			return;
		}
		reply_block(blk_server, STATUS_OK, granted as u64);
	}
}

// Write `count` sectors starting at `lba` from the transferred buffer `src_handle`
// (a MemoryObject of count*512 bytes the client filled). We map it, push each sector
// through the DMA buffer to the device, then unmap and close the handle. Replies with
// the status and no buffer.
unsafe fn serve_write(queue: &Queue, blk_server: u64, virt: u64, phys: u64, lba: u64, count: u32, src_handle: u64) {
	unsafe {
		if src_handle == 0 {
			reply_block(blk_server, STATUS_ERR, 0);
			return;
		}
		let src: u64 = match map_object(src_handle) {
			Some(base) => base,
			None => {
				close(src_handle);
				reply_block(blk_server, STATUS_ERR, 0);
				return;
			}
		};
		// copy each sector from the client buffer into the DMA buffer, then write it.
		let mut s: u32 = 0;
		let mut ok: bool = true;
		while s < count {
			core::ptr::copy_nonoverlapping((src + s as u64 * SECTOR as u64) as *const u8, (virt + DATA_OFF) as *mut u8, SECTOR as usize);
			if !request(queue, virt, phys, lba + s as u64, BLK_T_OUT, false) {
				ok = false;
				break;
			}
			s += 1;
		}
		unmap_object(src_handle);
		close(src_handle);
		reply_block(blk_server, if ok { STATUS_OK } else { STATUS_ERR }, 0);
	}
}

// Send a block reply: [status u32 LE] carrying the handle `xfer` (0 = none).
unsafe fn reply_block(blk_server: u64, status: u32, xfer: u64) {
	unsafe {
		send_blocking(blk_server, &status.to_le_bytes(), xfer);
	}
}
