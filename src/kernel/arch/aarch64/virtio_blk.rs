// aarch64 minimal virtio-blk driver (M116 bring-up).
//
// Enough of the modern virtio-pci block device to read one sector by polling: it
// resets the device, negotiates VIRTIO_F_VERSION_1, sets up a small split
// virtqueue (descriptor / available / used rings in DMA frames), submits a single
// read request as a 3-descriptor chain, kicks the device through its notify
// region, and polls the used ring for completion. Caches are off during bring-up,
// so DMA memory is plain non-cacheable RAM and only ordering barriers are needed;
// the BAR MMIO is Device memory the boot map already covers (identity, hhdm 0).

use core::arch::asm;

use super::pci::VirtioDevice;

// device_status bits.
const S_ACK: u8 = 1;
const S_DRIVER: u8 = 2;
const S_DRIVER_OK: u8 = 4;
const S_FEATURES_OK: u8 = 8;

// virtq descriptor flags.
const VRING_DESC_F_NEXT: u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;

// virtio_pci_common_cfg field offsets.
const CFG_DEVICE_FEATURE_SELECT: u64 = 0x00;
const CFG_DEVICE_FEATURE: u64 = 0x04;
const CFG_DRIVER_FEATURE_SELECT: u64 = 0x08;
const CFG_DRIVER_FEATURE: u64 = 0x0c;
const CFG_DEVICE_STATUS: u64 = 0x14;
const CFG_QUEUE_SELECT: u64 = 0x16;
const CFG_QUEUE_SIZE: u64 = 0x18;
const CFG_QUEUE_ENABLE: u64 = 0x1c;
const CFG_QUEUE_NOTIFY_OFF: u64 = 0x1e;
const CFG_QUEUE_DESC: u64 = 0x20;
const CFG_QUEUE_DRIVER: u64 = 0x28;
const CFG_QUEUE_DEVICE: u64 = 0x30;

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

// Write one descriptor-table entry (16 bytes: addr, len, flags, next).
unsafe fn put_desc(desc: u64, i: u64, addr: u64, len: u32, flags: u16, next: u16) {
	let d = desc + i * 16;
	unsafe {
		w64(d, addr);
		w32(d + 8, len);
		w16(d + 12, flags);
		w16(d + 14, next);
	}
}

// Read `sector` from `dev` into a freshly allocated frame, returning (frame_phys,
// blk_status) on completion, or None on setup failure / timeout. blk_status 0 = OK.
pub fn read_sector(dev: &VirtioDevice, sector: u64) -> Option<(u64, u8)> {
	let cfg = dev.bar_phys + dev.common.offset as u64;
	unsafe {
		// Reset, then acknowledge and claim the device.
		w8(cfg + CFG_DEVICE_STATUS, 0);
		let mut spins = 0u64;
		while r8(cfg + CFG_DEVICE_STATUS) != 0 && spins < 1_000_000 {
			spins += 1;
		}
		w8(cfg + CFG_DEVICE_STATUS, S_ACK);
		w8(cfg + CFG_DEVICE_STATUS, S_ACK | S_DRIVER);

		// Negotiate features: require VIRTIO_F_VERSION_1 (feature bit 32 = bit 0 of
		// the high dword).
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

		// Set up request queue 0 with a small ring in three zeroed DMA frames.
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
		w64(cfg + CFG_QUEUE_DESC, desc);
		w64(cfg + CFG_QUEUE_DRIVER, avail);
		w64(cfg + CFG_QUEUE_DEVICE, used);
		let notify_off = r16(cfg + CFG_QUEUE_NOTIFY_OFF);
		w16(cfg + CFG_QUEUE_ENABLE, 1);
		w8(cfg + CFG_DEVICE_STATUS, S_ACK | S_DRIVER | S_FEATURES_OK | S_DRIVER_OK);

		// Build the request: 16-byte header, 512-byte data, 1-byte status, packed
		// into one frame. type 0 = read (VIRTIO_BLK_T_IN).
		let req = super::paging::alloc_frame()?;
		let hdr = req;
		let data = req + 512;
		let status = req + 1024;
		w32(hdr, 0); // type = read
		w32(hdr + 4, 0); // reserved
		w64(hdr + 8, sector);
		w8(status, 0xff); // sentinel the device overwrites

		// 3-descriptor chain: header (device reads), data (device writes), status.
		put_desc(desc, 0, hdr, 16, VRING_DESC_F_NEXT, 1);
		put_desc(desc, 1, data, SECTOR_SIZE as u32, VRING_DESC_F_NEXT | VRING_DESC_F_WRITE, 2);
		put_desc(desc, 2, status, 1, VRING_DESC_F_WRITE, 0);

		// Publish descriptor 0 as head in the available ring.
		w16(avail, 0); // flags
		w16(avail + 4, 0); // ring[0] = head index 0
		barrier();
		w16(avail + 2, 1); // avail.idx = 1
		barrier();

		// Kick the device: write the queue index to its notify address.
		let notify = dev.bar_phys + dev.notify.offset as u64 + notify_off as u64 * dev.notify.notify_multiplier as u64;
		w16(notify, 0);

		// Poll the used ring for completion (used.idx goes 0 -> 1).
		let mut spins = 0u64;
		while r16(used + 2) == 0 && spins < 200_000_000 {
			spins += 1;
			core::hint::spin_loop();
		}
		barrier();
		if r16(used + 2) == 0 {
			return None; // timed out
		}
		Some((data, r8(status)))
	}
}
