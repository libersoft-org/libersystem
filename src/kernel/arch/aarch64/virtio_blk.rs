// aarch64 minimal virtio-blk driver (M116 bring-up).
//
// Enough of the modern virtio-pci block device to read and write sectors by
// polling. `BlkDevice::init` resets the device, negotiates VIRTIO_F_VERSION_1,
// and sets up a small split virtqueue (descriptor / available / used rings in DMA
// frames); `read`/`write` submit a single request as a 3-descriptor chain, kick
// the device through its notify region, and poll the used ring for completion.
// Caches are off during bring-up, so DMA memory is plain non-cacheable RAM and
// only ordering barriers are needed. The kernel runs in the higher half, so both
// the BAR MMIO and the DMA rings are reached through the physical direct map
// (`phys_to_virt`); the addresses programmed into the device (queue rings, buffer
// pointers) stay physical, since the device sees physical memory.

use core::arch::asm;

use super::paging::phys_to_virt;
use super::pci::VirtioDevice;

// The virtio-pci wire format (device_status bits, descriptor flags, common-config
// register offsets) is the shared `abi` source of truth, aliased here to this driver's
// short names; only the block-request types below are device-specific.
use abi::{VIRTIO_CFG_DEVICE_FEATURE as CFG_DEVICE_FEATURE, VIRTIO_CFG_DEVICE_FEATURE_SELECT as CFG_DEVICE_FEATURE_SELECT, VIRTIO_CFG_DEVICE_STATUS as CFG_DEVICE_STATUS, VIRTIO_CFG_DRIVER_FEATURE as CFG_DRIVER_FEATURE, VIRTIO_CFG_DRIVER_FEATURE_SELECT as CFG_DRIVER_FEATURE_SELECT, VIRTIO_CFG_QUEUE_DESC as CFG_QUEUE_DESC, VIRTIO_CFG_QUEUE_DEVICE as CFG_QUEUE_DEVICE, VIRTIO_CFG_QUEUE_DRIVER as CFG_QUEUE_DRIVER, VIRTIO_CFG_QUEUE_ENABLE as CFG_QUEUE_ENABLE, VIRTIO_CFG_QUEUE_NOTIFY_OFF as CFG_QUEUE_NOTIFY_OFF, VIRTIO_CFG_QUEUE_SELECT as CFG_QUEUE_SELECT, VIRTIO_CFG_QUEUE_SIZE as CFG_QUEUE_SIZE, VIRTIO_DESC_F_NEXT as VRING_DESC_F_NEXT, VIRTIO_DESC_F_WRITE as VRING_DESC_F_WRITE, VIRTIO_STATUS_ACKNOWLEDGE as S_ACK, VIRTIO_STATUS_DRIVER as S_DRIVER, VIRTIO_STATUS_DRIVER_OK as S_DRIVER_OK, VIRTIO_STATUS_FEATURES_OK as S_FEATURES_OK};

// virtio_blk request types (device-specific).
const VIRTIO_BLK_T_IN: u32 = 0; // read (device -> memory)
const VIRTIO_BLK_T_OUT: u32 = 1; // write (memory -> device)

const SECTOR_SIZE: usize = 512;
unsafe fn r8(a: u64) -> u8 {
	unsafe { core::ptr::read_volatile(a as *const u8) }
}
unsafe fn r16(a: u64) -> u16 {
	unsafe { core::ptr::read_volatile(a as *const u16) }
}
unsafe fn r32(a: u64) -> u32 {
	unsafe { core::ptr::read_volatile(a as *const u32) }
}
unsafe fn w8(a: u64, v: u8) {
	unsafe { core::ptr::write_volatile(a as *mut u8, v) }
}
unsafe fn w16(a: u64, v: u16) {
	unsafe { core::ptr::write_volatile(a as *mut u16, v) }
}
unsafe fn w32(a: u64, v: u32) {
	unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}
unsafe fn w64(a: u64, v: u64) {
	unsafe { core::ptr::write_volatile(a as *mut u64, v) }
}
fn barrier() {
	unsafe { asm!("dmb ish", options(nostack, preserves_flags)) }
}

// Write one descriptor-table entry (16 bytes: addr, len, flags, next). `desc` is
// the ring's physical address; the kernel reaches it through the direct map,
// while `addr` (the buffer pointer stored in the entry) stays physical.
unsafe fn put_desc(desc: u64, i: u64, addr: u64, len: u32, flags: u16, next: u16) {
	let d = phys_to_virt(desc) + i * 16;
	unsafe {
		w64(d, addr);
		w32(d + 8, len);
		w16(d + 12, flags);
		w16(d + 14, next);
	}
}

// A brought-up virtio-blk device: the common-config MMIO base, the queue-0 notify
// address, the split-ring physical addresses, and a reusable request frame.
pub struct BlkDevice {
	notify: u64,
	desc: u64,
	avail: u64,
	used: u64,
	qsz: u16,
	req: u64, // header@+0 (16), data@+512 (512), status@+1024 (1)
	avail_idx: u16,
	used_seen: u16,
}

impl BlkDevice {
	// Reset, negotiate features, and set up request queue 0. Returns None on
	// failure or if memory is exhausted.
	pub fn init(dev: &VirtioDevice) -> Option<BlkDevice> {
		let cfg = phys_to_virt(dev.bar_phys + dev.common.offset as u64);
		unsafe {
			// Reset, acknowledge, claim.
			w8(cfg + CFG_DEVICE_STATUS, 0);
			let mut spins = 0u64;
			while r8(cfg + CFG_DEVICE_STATUS) != 0 && spins < 1_000_000 {
				spins += 1;
			}
			w8(cfg + CFG_DEVICE_STATUS, S_ACK);
			w8(cfg + CFG_DEVICE_STATUS, S_ACK | S_DRIVER);

			// Require VIRTIO_F_VERSION_1 (feature bit 32 = bit 0 of the high dword).
			w32(cfg + CFG_DEVICE_FEATURE_SELECT, 1);
			let hi = r32(cfg + CFG_DEVICE_FEATURE);
			w32(cfg + CFG_DRIVER_FEATURE_SELECT, 1);
			w32(cfg + CFG_DRIVER_FEATURE, hi & 1);
			w32(cfg + CFG_DRIVER_FEATURE_SELECT, 0);
			w32(cfg + CFG_DRIVER_FEATURE, 0);
			w8(cfg + CFG_DEVICE_STATUS, S_ACK | S_DRIVER | S_FEATURES_OK);
			if r8(cfg + CFG_DEVICE_STATUS) & S_FEATURES_OK == 0 {
				return None;
			}

			// Queue 0: small ring in zeroed DMA frames plus a reusable request frame.
			w16(cfg + CFG_QUEUE_SELECT, 0);
			let qsz_max = r16(cfg + CFG_QUEUE_SIZE);
			if qsz_max == 0 {
				return None;
			}
			let qsz: u16 = 8.min(qsz_max);
			w16(cfg + CFG_QUEUE_SIZE, qsz);
			let desc = super::paging::alloc_frame()?;
			let avail = super::paging::alloc_frame()?;
			let used = super::paging::alloc_frame()?;
			let req = super::paging::alloc_frame()?;
			w64(cfg + CFG_QUEUE_DESC, desc);
			w64(cfg + CFG_QUEUE_DRIVER, avail);
			w64(cfg + CFG_QUEUE_DEVICE, used);
			let notify_off = r16(cfg + CFG_QUEUE_NOTIFY_OFF);
			w16(cfg + CFG_QUEUE_ENABLE, 1);
			w8(cfg + CFG_DEVICE_STATUS, S_ACK | S_DRIVER | S_FEATURES_OK | S_DRIVER_OK);

			let notify = phys_to_virt(dev.bar_phys + dev.notify.offset as u64 + notify_off as u64 * dev.notify.notify_multiplier as u64);
			Some(BlkDevice { notify, desc, avail, used, qsz, req, avail_idx: 0, used_seen: 0 })
		}
	}

	// Submit one request (header + data + status chain) and poll for completion.
	// Returns the blk status byte (0 = OK) or None on timeout. `data_write` marks
	// the data buffer device-writable (a read); a write leaves the device reading it.
	fn submit(&mut self, blk_type: u32, sector: u64, data_write: bool) -> Option<u8> {
		// Physical buffer pointers (programmed into descriptors, seen by the device)
		// and their direct-map virtual aliases (used for kernel reads/writes).
		let hdr = self.req;
		let data = self.req + 512;
		let status = self.req + 1024;
		let hdr_v = phys_to_virt(hdr);
		let status_v = phys_to_virt(status);
		let avail_v = phys_to_virt(self.avail);
		let used_v = phys_to_virt(self.used);
		unsafe {
			w32(hdr_v, blk_type);
			w32(hdr_v + 4, 0);
			w64(hdr_v + 8, sector);
			w8(status_v, 0xff);

			let data_flags = if data_write { VRING_DESC_F_NEXT | VRING_DESC_F_WRITE } else { VRING_DESC_F_NEXT };
			put_desc(self.desc, 0, hdr, 16, VRING_DESC_F_NEXT, 1);
			put_desc(self.desc, 1, data, SECTOR_SIZE as u32, data_flags, 2);
			put_desc(self.desc, 2, status, 1, VRING_DESC_F_WRITE, 0);

			// Publish descriptor 0 as head in the available ring.
			w16(avail_v + 4 + (self.avail_idx % self.qsz) as u64 * 2, 0);
			barrier();
			self.avail_idx = self.avail_idx.wrapping_add(1);
			w16(avail_v + 2, self.avail_idx);
			barrier();

			// Kick, then poll the used ring for a new completion.
			w16(self.notify, 0);
			let mut spins = 0u64;
			while r16(used_v + 2) == self.used_seen && spins < 200_000_000 {
				spins += 1;
				core::hint::spin_loop();
			}
			barrier();
			if r16(used_v + 2) == self.used_seen {
				return None;
			}
			self.used_seen = self.used_seen.wrapping_add(1);
			Some(r8(status_v))
		}
	}

	// Read one 512-byte sector into `buf`.
	pub fn read(&mut self, sector: u64, buf: &mut [u8; SECTOR_SIZE]) -> bool {
		match self.submit(VIRTIO_BLK_T_IN, sector, true) {
			Some(0) => {
				unsafe { core::ptr::copy_nonoverlapping(phys_to_virt(self.req + 512) as *const u8, buf.as_mut_ptr(), SECTOR_SIZE) };
				true
			}
			_ => false,
		}
	}

	// Write one 512-byte sector from `buf`.
	pub fn write(&mut self, sector: u64, buf: &[u8; SECTOR_SIZE]) -> bool {
		unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr(), phys_to_virt(self.req + 512) as *mut u8, SECTOR_SIZE) };
		matches!(self.submit(VIRTIO_BLK_T_OUT, sector, false), Some(0))
	}
}
