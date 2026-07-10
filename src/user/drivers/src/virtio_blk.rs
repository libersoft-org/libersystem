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
const BLK_T_FLUSH: u32 = 4; // flush the device's volatile write cache
const BLK_S_OK: u8 = 0;

// virtio-blk feature bits: size_max / seg_max declare the device's own transfer
// limits (config fields at offsets 8 / 12); FLUSH (bit 9) means the device has a
// volatile write cache and honours BLK_T_FLUSH (without it the cache is
// write-through and a flush is a no-op).
const FEATURE_SIZE_MAX: u32 = 1 << 1;
const FEATURE_SEG_MAX: u32 = 1 << 2;
const FEATURE_FLUSH: u32 = 1 << 9;

// Block-service request opcodes (the leading u32 of each request).
const OP_READ: u32 = 0;
const OP_WRITE: u32 = 1;
const OP_CAPACITY: u32 = 2;
const OP_FLUSH: u32 = 3;

// Block-service reply status codes.
const STATUS_OK: u32 = 0;
const STATUS_ERR: u32 = 1;

// The fixed control page: a 16-byte request header and the 1-byte status. The
// data rides its own contiguous DMA span, grown to the largest request seen, so
// one request moves the whole span - no per-sector unit stands anywhere.
const HDR_OFF: u64 = 0;
const STATUS_OFF: u64 = 512;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// negotiate the flush feature (a real write barrier) plus the device's own
		// transfer limits (size_max / seg_max), which bound a request's span - the
		// device's numbers, not a driver constant.
		let device = common::bringup_features(bootstrap, FEATURE_FLUSH | FEATURE_SIZE_MAX | FEATURE_SEG_MAX);
		let has_flush: bool = device.features_word0() & FEATURE_FLUSH != 0;
		// One data descriptor per request (the span is physically contiguous), so
		// seg_max >= 1 always holds; size_max caps the one segment's bytes.
		let size_max: u64 = if device.features_word0() & FEATURE_SIZE_MAX != 0 {
			let mut v: u32 = 0;
			for i in 0..4u64 {
				v |= (device.config_read(8 + i) as u32) << (i * 8);
			}
			if v == 0 {
				u64::MAX
			} else {
				v as u64
			}
		} else {
			u64::MAX
		};
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
		// the disk's capacity in 512-byte sectors, from the virtio-blk device config
		// (bytes 0..8), answered to OP_CAPACITY requests.
		let mut capacity_sectors: u64 = 0;
		for i in 0..8u64 {
			capacity_sectors |= (device.config_read(i) as u64) << (i * 8);
		}
		serve_blocks(&queue, blk_server, capacity_sectors, has_flush, size_max)
	}
}

// Read sector 0 from the disk and verify it carries a PKGARCH1 volume archive,
// exercising the block read path end to end over the virtqueue. Returns true if the
// magic matches (the archive was laid down on the device).
unsafe fn verify_archive(queue: &Queue) -> bool {
	unsafe {
		// a DMA buffer for the control page and one data sector.
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

		// READ sector 0 (the device writes the data buffer, placed past the control page).
		if !request(queue, virt, phys, 0, BLK_T_IN, true, phys + 1024, SECTOR) {
			return false;
		}
		// the sector must start with the PKGARCH1 archive magic.
		for (i, &b) in PKG_MAGIC.iter().enumerate() {
			if ((virt + 1024 + i as u64) as *const u8).read_volatile() != b {
				return false;
			}
		}
		true
	}
}

// Issue one block request covering `data_len` bytes at `data_phys` (one
// physically contiguous span - the whole request moves in one device round-trip):
// fill the header on the control page, submit the three-buffer descriptor chain
// (header, data span, status), and check the device reported success.
// `data_is_written` is whether the device writes the data buffer (a read).
#[allow(clippy::too_many_arguments)]
unsafe fn request(queue: &Queue, virt: u64, phys: u64, lba: u64, kind: u32, data_is_written: bool, data_phys: u64, data_len: u32) -> bool {
	unsafe {
		(virt as *mut u32).write_volatile(kind); // type
		((virt + 4) as *mut u32).write_volatile(0); // reserved
		((virt + 8) as *mut u64).write_volatile(lba); // sector
		((virt + STATUS_OFF) as *mut u8).write_volatile(0xff); // status sentinel
		let bufs: [(u64, u32, bool); 3] = [(phys + HDR_OFF, 16, false), (data_phys, data_len, data_is_written), (phys + STATUS_OFF, 1, true)];
		queue.submit(&bufs).is_some() && ((virt + STATUS_OFF) as *const u8).read_volatile() == BLK_S_OK
	}
}

// Issue one flush request: the device must write out its volatile cache before
// completing it, so every earlier write is durable when this returns. The chain is
// just the header and the status byte (a flush carries no data).
unsafe fn flush_request(queue: &Queue, virt: u64, phys: u64) -> bool {
	unsafe {
		(virt as *mut u32).write_volatile(BLK_T_FLUSH); // type
		((virt + 4) as *mut u32).write_volatile(0); // reserved
		((virt + 8) as *mut u64).write_volatile(0); // sector (unused)
		((virt + STATUS_OFF) as *mut u8).write_volatile(0xff); // status sentinel
		let bufs: [(u64, u32, bool); 2] = [(phys + HDR_OFF, 16, false), (phys + STATUS_OFF, 1, true)];
		queue.submit(&bufs).is_some() && ((virt + STATUS_OFF) as *const u8).read_volatile() == BLK_S_OK
	}
}

// A reusable contiguous DMA span the request data rides, grown to the largest
// request seen (the old buffer is released when replaced).
struct Span {
	handle: u64,
	virt: u64,
	phys: u64,
	bytes: u64,
}

impl Span {
	// Ensure the span holds `bytes`, reallocating a larger contiguous buffer when
	// the requests outgrow it. False when the allocation fails.
	unsafe fn fit(&mut self, bytes: u64) -> bool {
		unsafe {
			if bytes <= self.bytes {
				return true;
			}
			let handle: i64 = dma_buffer_create(bytes);
			if handle < 0 {
				return false;
			}
			let virt: i64 = dma_buffer_map(handle as u64);
			if sys_is_err(virt as u64) {
				close(handle as u64);
				return false;
			}
			if self.handle != 0 {
				close(self.handle);
			}
			self.handle = handle as u64;
			self.virt = virt as u64;
			self.phys = dma_buffer_phys(self.handle);
			self.bytes = bytes;
			true
		}
	}
}

// Serve block requests on `blk_server` until the client closes its end. Each request
// is [op u32][lba u64][count u32], the count bounded only by the device's own
// reported transfer limit (size_max), never a driver constant. A read (op 0) replies
// [status u32] carrying, on success, a MemoryObject capability to a freshly filled
// buffer of count*512 bytes - the whole span read in ONE device round-trip. A write
// (op 1) carries a transferred MemoryObject of count*512 bytes the device writes to
// disk in one round-trip, and replies [status u32] with no buffer. A capacity query
// (op 2) replies [status u32][capacity bytes u64][max sectors u32] - the disk's size
// plus the most sectors one request moves here (the device's own size_max), so the
// StorageService sizes its requests to the driver; the size is asked once of the
// device config at startup. A flush (op 3) is the write barrier: it completes
// only once every earlier write is durable, as BLK_T_FLUSH when the device
// negotiated the flush feature and trivially (write-through cache) when it did not;
// the reply is [status u32]. The data crosses as a shared buffer handle, never
// copied through the channel.
unsafe fn serve_blocks(queue: &Queue, blk_server: u64, capacity_sectors: u64, has_flush: bool, size_max: u64) -> ! {
	unsafe {
		// the fixed control page (header + status) and the growable data span.
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
		let mut span: Span = Span { handle: 0, virt: 0, phys: 0, bytes: 0 };
		let max_sectors: u64 = (size_max / SECTOR as u64).max(1);
		let mut req: [u8; 16] = [0u8; 16];
		loop {
			match recv_blocking(blk_server, &mut req) {
				Received::Message { len, handle } if len >= 16 => {
					let op: u32 = u32::from_le_bytes([req[0], req[1], req[2], req[3]]);
					let lba: u64 = u64::from_le_bytes([req[4], req[5], req[6], req[7], req[8], req[9], req[10], req[11]]);
					let count: u32 = (u32::from_le_bytes([req[12], req[13], req[14], req[15]]) as u64).clamp(1, max_sectors) as u32;
					match op {
						OP_READ => serve_read(queue, blk_server, virt, phys, &mut span, lba, count),
						OP_WRITE => serve_write(queue, blk_server, virt, phys, &mut span, lba, count, handle),
						OP_CAPACITY => reply_capacity(blk_server, capacity_sectors * SECTOR as u64, max_sectors),
						OP_FLUSH => {
							let ok: bool = !has_flush || flush_request(queue, virt, phys);
							reply_block(blk_server, if ok { STATUS_OK } else { STATUS_ERR }, 0);
						}
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

// Read `count` sectors starting at `lba` - one whole-span device request - into a
// fresh shared buffer and hand it to the client, or reply with an error status and
// no buffer on any failure.
unsafe fn serve_read(queue: &Queue, blk_server: u64, virt: u64, phys: u64, span: &mut Span, lba: u64, count: u32) {
	unsafe {
		let bytes: u64 = count as u64 * SECTOR as u64;
		if !span.fit(bytes) {
			reply_block(blk_server, STATUS_ERR, 0);
			return;
		}
		if !request(queue, virt, phys, lba, BLK_T_IN, true, span.phys, bytes as u32) {
			reply_block(blk_server, STATUS_ERR, 0);
			return;
		}
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
		core::ptr::copy_nonoverlapping(span.virt as *const u8, dst as *mut u8, bytes as usize);
		unmap_object(obj);
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
// (a MemoryObject of count*512 bytes the client filled): copy it into the DMA span
// and push the whole span to the device in one request, then unmap and close the
// handle. Replies with the status and no buffer.
#[allow(clippy::too_many_arguments)]
unsafe fn serve_write(queue: &Queue, blk_server: u64, virt: u64, phys: u64, span: &mut Span, lba: u64, count: u32, src_handle: u64) {
	unsafe {
		if src_handle == 0 {
			reply_block(blk_server, STATUS_ERR, 0);
			return;
		}
		let bytes: u64 = count as u64 * SECTOR as u64;
		if !span.fit(bytes) {
			close(src_handle);
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
		core::ptr::copy_nonoverlapping(src as *const u8, span.virt as *mut u8, bytes as usize);
		let ok: bool = request(queue, virt, phys, lba, BLK_T_OUT, false, span.phys, bytes as u32);
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

// Send a capacity reply: [status u32 LE][capacity bytes u64 LE][max sectors u32 LE],
// no handle - the size of the disk plus the most sectors one request moves here, so
// the StorageService sizes its requests to the driver instead of a shared constant.
unsafe fn reply_capacity(blk_server: u64, bytes: u64, max_sectors: u64) {
	unsafe {
		let mut reply: [u8; 16] = [0u8; 16];
		reply[..4].copy_from_slice(&STATUS_OK.to_le_bytes());
		reply[4..12].copy_from_slice(&bytes.to_le_bytes());
		reply[12..16].copy_from_slice(&(max_sectors.min(u32::MAX as u64) as u32).to_le_bytes());
		send_blocking(blk_server, &reply, 0);
	}
}
