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

// HOW LONG ONE REQUEST MAY POLL FOR ITS COMPLETION.
//
// The default is the boot's: large enough that an emulated device answering in microseconds is never
// cut off, and small enough that a device answering never does not hang the boot. The release path
// lowers it around a detach - see `iommu::detach_for` - and puts it back.
static SPIN_BUDGET: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(10_000_000);

pub fn spin_budget() -> u64 {
	SPIN_BUDGET.load(core::sync::atomic::Ordering::Relaxed)
}

// Set it, returning what it was, so a caller restores rather than assumes the default.
pub fn set_spin_budget(spins: u64) -> u64 {
	SPIN_BUDGET.swap(spins, core::sync::atomic::Ordering::Relaxed)
}

// WHAT A TEARDOWN MAY SPEND. A tenth of the boot's, because a release runs inside DeviceManager's
// single event loop and every other device's supervision waits behind it. Milliseconds on any
// machine this boots, and an expiry is `Unconfirmed` - which quarantines the claim rather than
// freeing anything, so a short wait can only be more conservative.
pub const TEARDOWN_SPINS: u64 = 1_000_000;

// AND WHAT IT MAY SPEND IN TIME, WHICH IS THE BOUND THAT MEANS ANYTHING TO THE MANAGER.
//
// A spin count is not a duration. The same million iterations are a millisecond on one machine and
// far longer on an emulated target under load, so "a tenth of the boot's budget" stated a ratio and
// bounded nothing a caller could reason about - and the caller here is DeviceManager's SOLE event
// loop, where every other device's supervision waits behind this. A release that occupies it for an
// unstated length of time is the blocking the binding milestone forbids, whatever the iteration
// count.
//
// TICKS, because that is what the rest of this kernel bounds waits with and what the manager's own
// budgets are expressed in. The wait ends at whichever comes first, the spins or the deadline; an
// expiry is `Unconfirmed` either way, which quarantines the claim rather than freeing anything, so
// cutting it short can only be more conservative.
pub const TEARDOWN_TICKS: u64 = 20;

// The deadline a bounded wait must not pass, or zero for "spins only" - which is what the ordinary
// boot path uses, since no manager is waiting behind it there.
static DEADLINE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn set_deadline(deadline: u64) -> u64 {
	DEADLINE.swap(deadline, core::sync::atomic::Ordering::Relaxed)
}

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
	// poll until it completes. `Some(written)` is how many bytes the device says it put in the tail.
	//
	// BOUNDED, because the device on the other end is emulated by something this milestone's threat
	// model does not trust. A device that never completes a request must not be able to stop the
	// kernel; it gets a refusal, and the caller treats that as unconfirmed.
	//
	// THE LENGTH IS RETURNED RATHER THAN JUDGED HERE. This answered `bool`, so the only thing a
	// caller could learn was "the device completed the chain" - and the caller is the one that knows
	// where in the tail its status byte sits. A queue cannot decide whether four bytes are enough
	// without knowing what was asked for.
	pub fn request(&mut self, request_physical: u64, request_len: u32, tail_physical: u64, tail_len: u32) -> Option<u32> {
		if self.size < 2 {
			return None;
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
		// A bounded poll, and the bound is the CALLER'S SITUATION rather than one number.
		//
		// The boot can afford to wait for a device that answers slowly; a TEARDOWN cannot, because
		// it runs inside DeviceManager's single event loop - the syscall that releases a claim does
		// not return until this does, and every other device's supervision is behind it. A wedged
		// controller could hold that loop for the whole of the boot-time budget.
		//
		// A shorter budget is not a weaker check: an expiry is `Fault::Unconfirmed`, the detach is
		// not confirmed, and the claim is QUARANTINED - which keeps its frames and vectors out of
		// circulation. Cutting the wait short can only make the release more conservative.
		// CHECKED EVERY `CLOCK_EVERY` SPINS, not every spin: reading the timer is a device access on
		// two of the three ports and doing it per iteration would make the wait it bounds slower than
		// the wait itself.
		const CLOCK_EVERY: u64 = 1024;
		let deadline = DEADLINE.load(core::sync::atomic::Ordering::Relaxed);
		for spin in 0..spin_budget() {
			if deadline != 0 && spin % CLOCK_EVERY == 0 && crate::arch::apic::ticks() >= deadline {
				return None;
			}
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
					return None;
				}
				// The device may write less than the tail holds; it may never claim to have written
				// more, and a length past the buffer is a device describing memory it was not given.
				if written > tail_len {
					return None;
				}
				return Some(written);
			}
			core::hint::spin_loop();
		}
		None
	}

	// Take one device-written record off the used ring, if the device has queued one. Used for the
	// EVENT queue, where the device fills buffers the driver supplied. `(id, written)` is the
	// descriptor the device completed and how many bytes it put in the buffer.
	//
	// BOTH FIELDS OF THE USED ELEMENT, NOT THE SLOT INDEX. This returned `self.used_seen % self.size`
	// - a number this side computed - and threw away the id and the length the DEVICE wrote. The
	// caller then copied a full record's worth of bytes out of a buffer that may have been filled
	// with fewer, or with none, and the tail of the previous report was read as part of this one.
	pub fn poll_used(&mut self) -> Option<(u32, u32)> {
		// SAFETY: this queue's own used ring.
		let used = unsafe { read16(self.used + 2) };
		if used == self.used_seen {
			return None;
		}
		let slot = self.used_seen % self.size;
		self.used_seen = self.used_seen.wrapping_add(1);
		core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
		let element = self.used + 4 + slot as u64 * 8;
		// SAFETY: an element inside this queue's own used ring - `slot` is below the ring size.
		let (id, written) = unsafe { (read32(element), read32(element + 4)) };
		Some((id, written))
	}

	// Offer one buffer to the device on this queue - the shape the event queue needs, where the
	// device writes and the driver reads. Answers the descriptor id the buffer went out on, so the
	// caller can check that what comes back is what it offered.
	pub fn offer(&mut self, physical: u64, len: u32) -> Option<u16> {
		if self.size == 0 {
			return None;
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
		Some(head)
	}
}
