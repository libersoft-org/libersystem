// The shared virtio transport - modern virtio-pci over the device's MMIO BAR.
//
// A driver maps its device's MMIO window (a DeviceMemory capability from
// DeviceManager) and calls `init`, which runs the virtio device-initialization
// handshake (reset -> acknowledge -> driver -> feature negotiation -> features-ok
// -> set up a virtqueue -> driver-ok) over the common-configuration structure, and
// allocates the split virtqueue rings in a DMA buffer the device is told the
// physical address of. After `init` the device is live; submitting buffers and
// driving the queue's data path is the per-driver work in the next milestone.

#![allow(dead_code)]

use core::sync::atomic::{Ordering, fence};

use rt::*;

// virtio_pci_common_cfg field offsets, relative to the common-config structure.
const CFG_DEVICE_FEATURE_SELECT: u64 = 0x00;
const CFG_DEVICE_FEATURE: u64 = 0x04;
const CFG_DRIVER_FEATURE_SELECT: u64 = 0x08;
const CFG_DRIVER_FEATURE: u64 = 0x0c;
const CFG_NUM_QUEUES: u64 = 0x12;
const CFG_DEVICE_STATUS: u64 = 0x14;
const CFG_QUEUE_SELECT: u64 = 0x16;
const CFG_QUEUE_SIZE: u64 = 0x18;
const CFG_QUEUE_ENABLE: u64 = 0x1c;
const CFG_QUEUE_NOTIFY_OFF: u64 = 0x1e;
const CFG_QUEUE_DESC: u64 = 0x20;
const CFG_QUEUE_DRIVER: u64 = 0x28;
const CFG_QUEUE_DEVICE: u64 = 0x30;

// device_status register bits.
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;
const STATUS_FAILED: u8 = 128;

// VIRTIO_F_VERSION_1 (feature bit 32) is bit 0 of the second feature word; every
// modern virtio device offers it and a modern driver must accept it.
const FEATURE_VERSION_1: u32 = 1 << 0;

// The queue size we request. Small enough that the split-virtqueue rings
// (descriptor table + available ring + used ring) fit in one DMA page, which is
// therefore physically contiguous as the rings require.
const QUEUE_SIZE: u16 = 16;

// Descriptor flags.
const DESC_NEXT: u16 = 1; // the buffer continues in the `next` descriptor
const DESC_WRITE: u16 = 2; // the device writes this buffer (it is device-writable)

unsafe fn r8(addr: u64) -> u8 {
	unsafe { (addr as *const u8).read_volatile() }
}
unsafe fn w8(addr: u64, v: u8) {
	unsafe { (addr as *mut u8).write_volatile(v) }
}
unsafe fn r16(addr: u64) -> u16 {
	unsafe { (addr as *const u16).read_volatile() }
}
unsafe fn r32(addr: u64) -> u32 {
	unsafe { (addr as *const u32).read_volatile() }
}
unsafe fn w16(addr: u64, v: u16) {
	unsafe { (addr as *mut u16).write_volatile(v) }
}
unsafe fn w32(addr: u64, v: u32) {
	unsafe { (addr as *mut u32).write_volatile(v) }
}
// A 64-bit common-config field is written as two 32-bit halves (low then high),
// the portable form the spec allows.
unsafe fn w64(addr: u64, v: u64) {
	unsafe {
		w32(addr, v as u32);
		w32(addr + 4, (v >> 32) as u32);
	}
}

fn align_up(x: u64, a: u64) -> u64 {
	(x + a - 1) & !(a - 1)
}

// A virtio device brought up to DRIVER_OK with one ready virtqueue.
pub struct Virtio {
	// Base of the common-config structure in the mapped MMIO window.
	common: u64,
	// Base of the notify structure and its per-queue multiplier (used by the data
	// path in the next milestone).
	notify: u64,
	notify_multiplier: u32,
	queue_notify_off: u16,
	// The negotiated queue size and the DMA-backed split-virtqueue rings.
	queue_size: u16,
	queue_handle: i64,
	queue_virt: u64,
	queue_phys: u64,
	// Byte offsets of the available and used rings within the ring page.
	avail_off: u64,
	used_off: u64,
}

// Bring the device up: run the init handshake and set up virtqueue 0. Returns None
// (and marks the device FAILED) if negotiation does not stick or no queue exists.
pub unsafe fn init(mmio_base: u64, info: &DeviceInfo) -> Option<Virtio> {
	unsafe {
		let common: u64 = mmio_base + info.common_offset as u64;

		// 1. reset, and wait for the device to acknowledge by reading status 0.
		w8(common + CFG_DEVICE_STATUS, 0);
		let mut spins: u32 = 0;
		while r8(common + CFG_DEVICE_STATUS) != 0 {
			spins += 1;
			if spins > 100_000 {
				return None;
			}
		}
		// 2. acknowledge the device and signal we have a driver for it.
		w8(common + CFG_DEVICE_STATUS, STATUS_ACKNOWLEDGE);
		w8(common + CFG_DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

		// 3. negotiate features: accept only VERSION_1 (the modern transport).
		w32(common + CFG_DRIVER_FEATURE_SELECT, 0);
		w32(common + CFG_DRIVER_FEATURE, 0);
		w32(common + CFG_DRIVER_FEATURE_SELECT, 1);
		w32(common + CFG_DRIVER_FEATURE, FEATURE_VERSION_1);

		// 4. lock the features in and confirm the device accepted them.
		w8(common + CFG_DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK);
		if r8(common + CFG_DEVICE_STATUS) & STATUS_FEATURES_OK == 0 {
			w8(common + CFG_DEVICE_STATUS, STATUS_FAILED);
			return None;
		}

		// 5. set up virtqueue 0.
		let virtio = match setup_queue(common, info, mmio_base) {
			Some(v) => v,
			None => {
				w8(common + CFG_DEVICE_STATUS, STATUS_FAILED);
				return None;
			}
		};

		// 6. tell the device the driver is ready.
		w8(common + CFG_DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK);
		Some(virtio)
	}
}

// Select queue 0, allocate its split-virtqueue rings in a DMA page, program the
// ring physical addresses, and enable the queue.
unsafe fn setup_queue(common: u64, info: &DeviceInfo, mmio_base: u64) -> Option<Virtio> {
	unsafe {
		w16(common + CFG_QUEUE_SELECT, 0);
		let max_size: u16 = r16(common + CFG_QUEUE_SIZE);
		if max_size == 0 {
			return None;
		}
		let size: u16 = if max_size < QUEUE_SIZE { max_size } else { QUEUE_SIZE };
		w16(common + CFG_QUEUE_SIZE, size);

		// one DMA page holds all three rings (contiguous, as the rings require).
		let handle: i64 = dma_buffer_create(4096);
		if handle < 0 {
			return None;
		}
		let virt: i64 = dma_buffer_map(handle as u64);
		if sys_is_err(virt as u64) {
			return None;
		}
		let phys: u64 = dma_buffer_phys(handle as u64);
		core::ptr::write_bytes(virt as *mut u8, 0, 4096);

		// split-virtqueue layout within the page: descriptor table (16 bytes each),
		// then the available ring (2-byte aligned), then the used ring (4-byte
		// aligned). With QUEUE_SIZE = 16 all three fit well inside one page.
		let desc_off: u64 = 0;
		let avail_off: u64 = 16 * size as u64;
		let used_off: u64 = align_up(avail_off + 6 + 2 * size as u64, 4);

		w64(common + CFG_QUEUE_DESC, phys + desc_off);
		w64(common + CFG_QUEUE_DRIVER, phys + avail_off);
		w64(common + CFG_QUEUE_DEVICE, phys + used_off);
		let queue_notify_off: u16 = r16(common + CFG_QUEUE_NOTIFY_OFF);
		w16(common + CFG_QUEUE_ENABLE, 1);

		Some(Virtio { common, notify: mmio_base + info.notify_offset as u64, notify_multiplier: info.notify_multiplier, queue_notify_off, queue_size: size, queue_handle: handle, queue_virt: virt as u64, queue_phys: phys, avail_off, used_off })
	}
}

impl Virtio {
	pub fn queue_size(&self) -> u16 {
		self.queue_size
	}

	// Whether the device reports DRIVER_OK (negotiation completed, queue live).
	pub fn is_live(&self) -> bool {
		unsafe { r8(self.common + CFG_DEVICE_STATUS) & STATUS_DRIVER_OK != 0 }
	}

	// Submit a descriptor chain to queue 0 and wait (by polling the used ring) for
	// the device to complete it. Each buffer is (physical address, length, whether
	// the device writes it). Returns the number of bytes the device reported using,
	// or None if the queue is too small or the device never completes. This is the
	// synchronous, single-request-in-flight path the headless drivers use.
	pub unsafe fn submit(&self, bufs: &[(u64, u32, bool)]) -> Option<u32> {
		unsafe {
			let n = bufs.len();
			if n == 0 || n > self.queue_size as usize {
				return None;
			}
			// Fill descriptors 0..n as one chain (we keep a single request in flight,
			// so the same descriptors are reused each call).
			for (i, &(phys, len, device_writes)) in bufs.iter().enumerate() {
				let d = self.queue_virt + i as u64 * 16;
				w64(d, phys);
				w32(d + 8, len);
				let mut flags: u16 = 0;
				if device_writes {
					flags |= DESC_WRITE;
				}
				if i + 1 < n {
					flags |= DESC_NEXT;
				}
				w16(d + 12, flags);
				w16(d + 14, (i + 1) as u16);
			}
			// Publish the head descriptor (index 0) in the available ring, ordered
			// before the index bump so the device never sees a half-written entry.
			let avail = self.queue_virt + self.avail_off;
			let old_avail = r16(avail + 2);
			w16(avail + 4 + (old_avail % self.queue_size) as u64 * 2, 0);
			fence(Ordering::SeqCst);
			w16(avail + 2, old_avail.wrapping_add(1));
			fence(Ordering::SeqCst);
			// Notify the device that queue 0 has work.
			w16(self.notify + self.queue_notify_off as u64 * self.notify_multiplier as u64, 0);
			// Poll the used ring until the request completes.
			let used = self.queue_virt + self.used_off;
			let old_used = r16(used + 2);
			let mut spins: u32 = 0;
			loop {
				fence(Ordering::SeqCst);
				if r16(used + 2) != old_used {
					break;
				}
				spins += 1;
				if spins > 10_000_000 {
					return None;
				}
			}
			// Each used-ring element is { id u32, len u32 }; return the used length.
			Some(r32(used + 4 + (old_used % self.queue_size) as u64 * 8 + 4))
		}
	}
}
