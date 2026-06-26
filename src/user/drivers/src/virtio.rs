// The shared virtio transport - modern virtio-pci over the device's MMIO BAR.
//
// A driver maps its device's MMIO window (a DeviceMemory capability from
// DeviceManager) and calls `negotiate`, which runs the device-initialization
// handshake up to FEATURES_OK over the common-configuration structure. The driver
// then sets up the virtqueue(s) it needs (`setup_queue`, allocating the split-queue
// rings in a DMA buffer the device is told the physical address of) and calls
// `driver_ok`. After that it drives each queue with `Queue::submit` (a descriptor
// chain, a notify, and a poll of the used ring).

#![allow(dead_code)]

use core::sync::atomic::{fence, Ordering};

use rt::*;

// virtio_pci_common_cfg field offsets, relative to the common-config structure.
const CFG_DEVICE_FEATURE_SELECT: u64 = 0x00;
const CFG_DEVICE_FEATURE: u64 = 0x04;
const CFG_DRIVER_FEATURE_SELECT: u64 = 0x08;
const CFG_DRIVER_FEATURE: u64 = 0x0c;
const CFG_CONFIG_MSIX_VECTOR: u64 = 0x10;
const CFG_NUM_QUEUES: u64 = 0x12;
const CFG_DEVICE_STATUS: u64 = 0x14;
const CFG_QUEUE_SELECT: u64 = 0x16;
const CFG_QUEUE_SIZE: u64 = 0x18;
const CFG_QUEUE_MSIX_VECTOR: u64 = 0x1a;
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

// Available-ring flag: ask the device not to raise an interrupt when it consumes a
// buffer. The polling drivers set this (they busy-poll the used ring and want no
// interrupts, so their PCI INTx line - which may be shared with another device -
// never asserts); an interrupt-driven driver clears it on its event queue.
const VIRTQ_AVAIL_F_NO_INTERRUPT: u16 = 1;

// The MSI-X vector fields' reset value: no vector mapped (the device raises legacy
// INTx instead). A driver that has not opted into MSI-X leaves this in place.
const VIRTIO_MSI_NO_VECTOR: u16 = 0xffff;

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

// A virtio device negotiated up to FEATURES_OK. The driver sets up the virtqueues
// it needs, then calls `driver_ok`; afterwards it drives those queues.
pub struct Virtio {
	// Base of the common-config structure in the mapped MMIO window.
	common: u64,
	// Base of the device-specific config structure (e.g. a NIC's MAC).
	device: u64,
	// Base of the notify structure and its per-queue multiplier.
	notify: u64,
	notify_multiplier: u32,
	// The ISR-status register: reading it returns the pending-interrupt reason and
	// deasserts the device's (level-triggered INTx) line.
	isr: u64,
	// The MSI-X table-entry index this device's interrupts route to (NO_VECTOR until a
	// driver opts into MSI-X with set_msix_vector). setup_queue programs each queue's
	// MSI-X vector field from this.
	msix_vector: u16,
}

// One set-up split virtqueue: its rings (in a DMA page) and the address/value used
// to notify the device of new work.
pub struct Queue {
	index: u16,
	notify_addr: u64,
	size: u16,
	virt: u64,
	avail_off: u64,
	used_off: u64,
	// The used-ring index consumed so far, so `take_used` knows what is new (the RX
	// flow; unused by the synchronous `submit` path).
	last_used: u16,
	// Keeps the ring DMA buffer alive (the handle stays open).
	handle: u64,
}

// The RX / event-queue flow: the device pushes to the driver. The driver posts a
// pool of device-writable buffers once, then on each interrupt drains the buffers
// the device filled and re-posts them. Unlike `submit` (one synchronous
// request/response, busy-polled) this never blocks - the driver waits on its IRQ.
impl Queue {
	// Add a device-writable buffer (descriptor `id`, at physical `phys`, `len` bytes)
	// to the available ring so the device can fill it. Used to seed the pool and to
	// re-post each buffer after it is drained. Call `notify` after a batch.
	pub unsafe fn post_recv(&self, id: u16, phys: u64, len: u32) {
		unsafe {
			let d = self.virt + id as u64 * 16;
			w64(d, phys);
			w32(d + 8, len);
			w16(d + 12, DESC_WRITE);
			w16(d + 14, 0);
			let avail = self.virt + self.avail_off;
			let idx = r16(avail + 2);
			w16(avail + 4 + (idx % self.size) as u64 * 2, id);
			fence(Ordering::SeqCst);
			w16(avail + 2, idx.wrapping_add(1));
		}
	}

	// Tell the device this queue has freshly posted buffers.
	pub unsafe fn notify(&self) {
		unsafe {
			w16(self.notify_addr, self.index);
		}
	}

	// Clear the available-ring NO_INTERRUPT flag, so the device interrupts when it
	// fills a buffer on this queue (the interrupt-driven RX flow).
	pub unsafe fn enable_interrupts(&self) {
		unsafe {
			w16(self.virt + self.avail_off, 0);
		}
	}

	// Take the next buffer the device filled, as (descriptor id, bytes written), or
	// None if nothing new since the last take. The driver reads buffer `id`, then
	// re-posts it with `post_recv`.
	pub unsafe fn take_used(&mut self) -> Option<(u16, u32)> {
		unsafe {
			let used = self.virt + self.used_off;
			fence(Ordering::SeqCst);
			if r16(used + 2) == self.last_used {
				return None;
			}
			let elem = used + 4 + (self.last_used % self.size) as u64 * 8;
			let id = r32(elem) as u16;
			let len = r32(elem + 4);
			self.last_used = self.last_used.wrapping_add(1);
			Some((id, len))
		}
	}
}

// Run the device-initialization handshake up to FEATURES_OK (reset -> acknowledge
// -> driver -> negotiate VERSION_1 -> features-ok). Returns None (marking the
// device FAILED) if the device rejects the features. The caller then sets up its
// queues and calls `driver_ok`.
pub unsafe fn negotiate(mmio_base: u64, info: &DeviceInfo) -> Option<Virtio> {
	unsafe {
		let common: u64 = mmio_base + info.common_offset as u64;

		// reset, and wait for the device to acknowledge by reading status 0.
		w8(common + CFG_DEVICE_STATUS, 0);
		let mut spins: u32 = 0;
		while r8(common + CFG_DEVICE_STATUS) != 0 {
			spins += 1;
			if spins > 100_000 {
				return None;
			}
		}
		// acknowledge the device and signal we have a driver for it.
		w8(common + CFG_DEVICE_STATUS, STATUS_ACKNOWLEDGE);
		w8(common + CFG_DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

		// negotiate features: accept only VERSION_1 (the modern transport).
		w32(common + CFG_DRIVER_FEATURE_SELECT, 0);
		w32(common + CFG_DRIVER_FEATURE, 0);
		w32(common + CFG_DRIVER_FEATURE_SELECT, 1);
		w32(common + CFG_DRIVER_FEATURE, FEATURE_VERSION_1);

		// lock the features in and confirm the device accepted them.
		w8(common + CFG_DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK);
		if r8(common + CFG_DEVICE_STATUS) & STATUS_FEATURES_OK == 0 {
			w8(common + CFG_DEVICE_STATUS, STATUS_FAILED);
			return None;
		}
		Some(Virtio { common, device: mmio_base + info.device_offset as u64, notify: mmio_base + info.notify_offset as u64, notify_multiplier: info.notify_multiplier, isr: mmio_base + info.isr_offset as u64, msix_vector: VIRTIO_MSI_NO_VECTOR })
	}
}

impl Virtio {
	// Route this device's config-change and queue interrupts to MSI-X table entry
	// `vector` (the index the kernel programmed via device_msix_acquire). Must be called
	// after the kernel has enabled MSI-X on the device and before setup_queue, so each
	// queue is told to use this vector. INTx / polling drivers never call this and keep
	// the reset NO_VECTOR.
	pub unsafe fn set_msix_vector(&mut self, vector: u16) {
		unsafe {
			self.msix_vector = vector;
			w16(self.common + CFG_CONFIG_MSIX_VECTOR, vector);
		}
	}

	// Select queue `index`, allocate its split-virtqueue rings in a DMA page, program
	// the ring physical addresses, and enable the queue. One DMA page holds all three
	// rings (contiguous, as the rings require).
	pub unsafe fn setup_queue(&self, index: u16) -> Option<Queue> {
		unsafe {
			w16(self.common + CFG_QUEUE_SELECT, index);
			let max_size: u16 = r16(self.common + CFG_QUEUE_SIZE);
			if max_size == 0 {
				return None;
			}
			let size: u16 = if max_size < QUEUE_SIZE { max_size } else { QUEUE_SIZE };
			w16(self.common + CFG_QUEUE_SIZE, size);
			// Route this queue's interrupts to the device's MSI-X vector (NO_VECTOR for INTx /
			// polling drivers, which is also the reset value, so this is a no-op for them).
			w16(self.common + CFG_QUEUE_MSIX_VECTOR, self.msix_vector);

			let (handle, virt, phys): (u64, u64, u64) = match dma_buffer(4096) {
				Some(t) => t,
				None => return None,
			};
			core::ptr::write_bytes(virt as *mut u8, 0, 4096);

			// layout within the page: descriptor table (16 bytes each), then the
			// available ring (2-byte aligned), then the used ring (4-byte aligned).
			let avail_off: u64 = 16 * size as u64;
			let used_off: u64 = align_up(avail_off + 6 + 2 * size as u64, 4);
			// suppress device interrupts by default: a polling driver wants none, and an
			// interrupt-driven one re-enables them on its queue with `enable_interrupts`.
			w16(virt + avail_off, VIRTQ_AVAIL_F_NO_INTERRUPT);
			w64(self.common + CFG_QUEUE_DESC, phys);
			w64(self.common + CFG_QUEUE_DRIVER, phys + avail_off);
			w64(self.common + CFG_QUEUE_DEVICE, phys + used_off);
			let notify_off: u16 = r16(self.common + CFG_QUEUE_NOTIFY_OFF);
			w16(self.common + CFG_QUEUE_ENABLE, 1);

			Some(Queue { index, notify_addr: self.notify + notify_off as u64 * self.notify_multiplier as u64, size, virt, avail_off, used_off, last_used: 0, handle })
		}
	}

	// Tell the device the driver is ready, after the queues are set up.
	pub unsafe fn driver_ok(&self) {
		unsafe {
			let status = r8(self.common + CFG_DEVICE_STATUS);
			w8(self.common + CFG_DEVICE_STATUS, status | STATUS_DRIVER_OK);
		}
	}

	// Whether the device reports DRIVER_OK.
	pub fn is_live(&self) -> bool {
		unsafe { r8(self.common + CFG_DEVICE_STATUS) & STATUS_DRIVER_OK != 0 }
	}

	// Read one byte of the device-specific config (e.g. a NIC's MAC bytes).
	pub unsafe fn config_read(&self, offset: u64) -> u8 {
		unsafe { r8(self.device + offset) }
	}

	// Write one byte of the device-specific config (e.g. a virtio-input select/subsel
	// pair that chooses which config block the next reads return).
	pub unsafe fn config_write(&self, offset: u64, value: u8) {
		unsafe { w8(self.device + offset, value) }
	}

	// Read (and so acknowledge) the ISR-status register, deasserting the device's
	// level-triggered INTx line. An interrupt-driven driver must do this each time it
	// services an interrupt, or the line stays asserted and the IRQ storms.
	pub unsafe fn isr_ack(&self) -> u8 {
		unsafe { r8(self.isr) }
	}
}

impl Queue {
	pub fn size(&self) -> u16 {
		self.size
	}

	// Submit a descriptor chain to this queue and wait (by polling the used ring) for
	// the device to complete it. Each buffer is (physical address, length, whether
	// the device writes it). Returns the bytes the device reported using, or None if
	// the queue is too small or the device never completes. Synchronous, with a
	// single request in flight (the same descriptors are reused each call).
	pub unsafe fn submit(&self, bufs: &[(u64, u32, bool)]) -> Option<u32> {
		unsafe {
			let n = bufs.len();
			if n == 0 || n > self.size as usize {
				return None;
			}
			for (i, &(phys, len, device_writes)) in bufs.iter().enumerate() {
				let d = self.virt + i as u64 * 16;
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
			let avail = self.virt + self.avail_off;
			let old_avail = r16(avail + 2);
			w16(avail + 4 + (old_avail % self.size) as u64 * 2, 0);
			fence(Ordering::SeqCst);
			w16(avail + 2, old_avail.wrapping_add(1));
			fence(Ordering::SeqCst);
			// Notify the device that this queue has work (the value is the queue index).
			w16(self.notify_addr, self.index);
			// Poll the used ring until the request completes.
			let used = self.virt + self.used_off;
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
				// The fast common case completes within the spin budget below and never
				// yields, keeping device control paths low-latency. A slow completion
				// (e.g. a virtio-gpu RESOURCE_FLUSH stalled behind a slow/remote display
				// client) instead yields the core cooperatively, so a co-scheduled thread
				// (the console pipeline) is not starved while the present drains.
				if spins % 4096 == 0 {
					yield_now();
				}
			}
			// Each used-ring element is { id u32, len u32 }; return the used length.
			Some(r32(used + 4 + (old_used % self.size) as u64 * 8 + 4))
		}
	}

	// Post a descriptor chain to this queue and notify the device, then return without
	// waiting - the interrupt-driven counterpart to `submit`. The caller blocks on its
	// device's MSI-X interrupt (the queue must have had `enable_interrupts` called) and
	// reaps the completion with `take_used`. One request in flight (descriptors 0..n
	// reused each call, head index 0). Returns false if the chain is empty or longer
	// than the queue.
	pub unsafe fn submit_async(&self, bufs: &[(u64, u32, bool)]) -> bool {
		unsafe {
			let n = bufs.len();
			if n == 0 || n > self.size as usize {
				return false;
			}
			for (i, &(phys, len, device_writes)) in bufs.iter().enumerate() {
				let d = self.virt + i as u64 * 16;
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
			// Publish the head descriptor (index 0) in the available ring, ordered before
			// the index bump so the device never sees a half-written entry.
			let avail = self.virt + self.avail_off;
			let old_avail = r16(avail + 2);
			w16(avail + 4 + (old_avail % self.size) as u64 * 2, 0);
			fence(Ordering::SeqCst);
			w16(avail + 2, old_avail.wrapping_add(1));
			fence(Ordering::SeqCst);
			// Notify the device that this queue has work (the value is the queue index).
			w16(self.notify_addr, self.index);
			true
		}
	}
}
