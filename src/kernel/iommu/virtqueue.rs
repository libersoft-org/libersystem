// One split virtqueue, owned by the kernel and reachable by nobody else.
//
// WHY THE KERNEL OWNS THIS ONE. An ordinary driver never receives the IOMMU control queue: a
// userspace service that could send `MAP` requests would be able to map any physical page to any
// device, which is the same memory-safety TCB under another name. So this queue is set up here, its
// rings are kernel frames that are never published, and the only thing that reaches it is the
// backend above.
//
// POLLED, NOT INTERRUPT-DRIVEN, and deliberately: the operations this carries are all
// configuration - attach, map, unmap - and every one of them is a point where the caller must WAIT
// for a completion before it may do anything else. There is nothing to overlap.

use abi::{VIRTIO_DESC_F_NEXT, VIRTIO_DESC_F_WRITE};

use crate::mem::frame::PAGE_SIZE;

// The rings, at the offsets the specification lays them out at within their own frames. Each ring
// gets a frame of its own rather than being packed: a frame is the unit the allocator hands out, and
// three of them is a negligible cost next to arithmetic that has to be right.
pub struct VirtQueue {
	// Physical addresses, which is what the DEVICE is programmed with.
	desc: u64,
	avail: u64,
	used: u64,
	// A frame the requests are written into, so nothing on the wire ever points at a kernel stack.
	scratch: u64,
	size: u16,
	// THE AVAILABLE RING'S INDEX COUNTS CHAINS, NOT DESCRIPTORS - and getting that wrong is not a
	// slow queue, it is a device processing entries the driver never wrote. Advancing it by two for
	// a two-descriptor chain made the device read the next ring slot as well, find the zero left
	// there, and run descriptor 0 a second time against whatever the scratch page held by then. It
	// answered `INVAL`, and the status the driver read was that second request's.
	avail_index: u16,
	// Where the next chain's descriptors go. Separate from the ring index above for the same reason.
	next_descriptor: u16,
	used_seen: u16,
	notify: u64,
}

fn direct(physical: u64) -> u64 {
	crate::mem::hhdm_offset() + physical
}

unsafe fn write16(at: u64, value: u16) {
	unsafe { core::ptr::write_volatile(at as *mut u16, value) }
}

unsafe fn read16(at: u64) -> u16 {
	unsafe { core::ptr::read_volatile(at as *const u16) }
}

unsafe fn read32(at: u64) -> u32 {
	unsafe { core::ptr::read_volatile(at as *const u32) }
}

unsafe fn write32(at: u64, value: u32) {
	unsafe { core::ptr::write_volatile(at as *mut u32, value) }
}

unsafe fn write64(at: u64, value: u64) {
	unsafe { core::ptr::write_volatile(at as *mut u64, value) }
}

// One descriptor, written into the descriptor table at `index`.
unsafe fn descriptor(table: u64, index: u16, address: u64, len: u32, flags: u16, next: u16) {
	let at = table + index as u64 * 16;
	unsafe {
		write64(at, address);
		write32(at + 8, len);
		write16(at + 12, flags);
		write16(at + 14, next);
	}
}

impl VirtQueue {
	// Allocate the rings and program the device's queue registers. `common` is the direct-map
	// address of the device's common configuration structure.
	pub fn create(common: u64, index: u16, notify_base: u64, notify_multiplier: u32, size: u16) -> Option<VirtQueue> {
		let desc = crate::mem::frame::allocate()?;
		let avail = crate::mem::frame::allocate()?;
		let used = crate::mem::frame::allocate()?;
		let scratch = crate::mem::frame::allocate()?;
		for frame in [desc, avail, used, scratch] {
			// SAFETY: freshly allocated frames, reachable through the direct map, owned by nobody
			// else until this queue publishes their addresses to the device below.
			unsafe { core::ptr::write_bytes(direct(frame) as *mut u8, 0, PAGE_SIZE as usize) };
		}
		// SAFETY: `common` is the device's common configuration structure, resolved from its PCI
		// capabilities, and these are the registers the specification defines there.
		let notify_offset = unsafe {
			write16(common + abi::VIRTIO_CFG_QUEUE_SELECT, index);
			let maximum = read16(common + abi::VIRTIO_CFG_QUEUE_SIZE);
			if maximum == 0 {
				return None;
			}
			let size = size.min(maximum);
			write16(common + abi::VIRTIO_CFG_QUEUE_SIZE, size);
			write64(common + abi::VIRTIO_CFG_QUEUE_DESC, desc);
			write64(common + abi::VIRTIO_CFG_QUEUE_DRIVER, avail);
			write64(common + abi::VIRTIO_CFG_QUEUE_DEVICE, used);
			let notify_offset = read16(common + abi::VIRTIO_CFG_QUEUE_NOTIFY_OFF);
			write16(common + abi::VIRTIO_CFG_QUEUE_ENABLE, 1);
			notify_offset
		};
		let size = size.min(unsafe {
			write16(common + abi::VIRTIO_CFG_QUEUE_SELECT, index);
			read16(common + abi::VIRTIO_CFG_QUEUE_SIZE)
		});
		Some(VirtQueue { desc: direct(desc), avail: direct(avail), used: direct(used), scratch, size, avail_index: 0, next_descriptor: 0, used_seen: 0, notify: notify_base + notify_offset as u64 * notify_multiplier as u64 })
	}

	// The physical address of the scratch frame, and its direct-map alias.
	pub fn scratch_physical(&self) -> u64 {
		self.scratch
	}

	pub fn scratch_virtual(&self) -> u64 {
		direct(self.scratch)
	}

	// Submit a two-descriptor chain - the request the device READS, and the tail it WRITES - and
	// poll until it completes.
	//
	// BOUNDED, because the device on the other end is emulated by something this milestone's threat
	// model does not trust. A device that never completes a request must not be able to stop the
	// kernel; it gets a refusal, and the caller treats that as unconfirmed.
	pub fn request(&mut self, request_physical: u64, request_len: u32, tail_physical: u64, tail_len: u32) -> bool {
		if self.size < 2 {
			return false;
		}
		let head = self.next_descriptor % self.size;
		let second = (head + 1) % self.size;
		self.next_descriptor = self.next_descriptor.wrapping_add(2);
		// SAFETY: both indices are below the ring size, and the rings are this queue's own frames.
		unsafe {
			descriptor(self.desc, head, request_physical, request_len, VIRTIO_DESC_F_NEXT, second);
			descriptor(self.desc, second, tail_physical, tail_len, VIRTIO_DESC_F_WRITE, 0);
			// The available ring: flags, index, then the ring itself. ONE ENTRY PER CHAIN.
			let ring = self.avail + 4 + (self.avail_index % self.size) as u64 * 2;
			write16(ring, head);
			core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
			self.avail_index = self.avail_index.wrapping_add(1);
			write16(self.avail + 2, self.avail_index);
			core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
			write16(self.notify, 0);
		}
		// A bounded poll. The number is large enough that an emulated device answering in
		// microseconds is never cut off, and small enough that a device answering never does not
		// hang the boot.
		for _ in 0..10_000_000u64 {
			// SAFETY: the used ring's index field, in this queue's own frame.
			let used = unsafe { read16(self.used + 2) };
			if used != self.used_seen {
				// WHICH ELEMENT THE DEVICE JUST WROTE. One chain is in flight per call, so the
				// completion is the element at the index this driver had not consumed yet.
				let slot = self.used_seen % self.size;
				self.used_seen = used;
				core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
				// A COMPLETION IS NOT A CHANGED INDEX. This used to return true on the index
				// moving, which is the device saying "something finished" and nothing more - so a
				// device that completed a DIFFERENT chain, or claimed to have written more bytes
				// than the buffer holds, was read as this request succeeding. The threat model for
				// this milestone includes a device that is wrong, and this is the one place its
				// answer is turned into a fact.
				//
				// Each used-ring element is `{ id: u32, len: u32 }` after the 4-byte header.
				let element = self.used + 4 + slot as u64 * 8;
				// SAFETY: an element inside this queue's own used ring - `slot` is below the ring size.
				let (id, written) = unsafe { (read32(element), read32(element + 4)) };
				if id != head as u32 {
					return false;
				}
				// The device may write less than the tail holds; it may never claim to have written
				// more, and a length past the buffer is a device describing memory it was not given.
				if written > tail_len {
					return false;
				}
				return true;
			}
			core::hint::spin_loop();
		}
		false
	}

	// Take one device-written record off the used ring, if the device has queued one. Used for the
	// EVENT queue, where the device fills buffers the driver supplied.
	pub fn poll_used(&mut self) -> Option<u16> {
		// SAFETY: this queue's own used ring.
		let used = unsafe { read16(self.used + 2) };
		if used == self.used_seen {
			return None;
		}
		let slot = self.used_seen % self.size;
		self.used_seen = self.used_seen.wrapping_add(1);
		Some(slot)
	}

	// Offer one buffer to the device on this queue - the shape the event queue needs, where the
	// device writes and the driver reads.
	pub fn offer(&mut self, physical: u64, len: u32) {
		if self.size == 0 {
			return;
		}
		let head = self.next_descriptor % self.size;
		self.next_descriptor = self.next_descriptor.wrapping_add(1);
		// SAFETY: an index below the ring size, in this queue's own frames.
		unsafe {
			descriptor(self.desc, head, physical, len, VIRTIO_DESC_F_WRITE, 0);
			let ring = self.avail + 4 + (self.avail_index % self.size) as u64 * 2;
			write16(ring, head);
			core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
			self.avail_index = self.avail_index.wrapping_add(1);
			write16(self.avail + 2, self.avail_index);
			core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
			write16(self.notify, 1);
		}
	}
}
