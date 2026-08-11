// DmaBuffer kernel object.
//
// A DmaBuffer owns physical frames pinned for device DMA: a driver maps it to
// fill or drain it and hands its physical address to its device. Unlike a plain
// MemoryObject the memory is charged to the owning Domain's DMA quota - pinned DMA
// is a distinct, separately capped resource (the anti-DoS rule for drivers) - and
// the frames are freed and the quota refunded when the last reference drops.
//
// Every buffer uses one physically contiguous frame run, so a device receives a
// single physical span for a virtqueue ring, block-data stage or jumbo frame.

#![allow(dead_code)]

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, Ordering};

use super::domain::Domain;
use super::memory_object::MemoryError;
use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::arch::paging;
use crate::mem::frame::{self, PAGE_SIZE};
use crate::sync::SpinLock;

pub struct DmaBuffer {
	header: ObjectHeader,
	// Physical addresses of the pinned frames backing this buffer.
	frames: Vec<u64>,
	// Size in bytes (rounded up to whole pages).
	size: usize,
	// The driver and display server map the same backing in different address spaces.
	mappings: SpinLock<Vec<(u64, u64)>>,
	// Domain charged for this buffer's pinned DMA memory; refunded on drop.
	domain: Arc<Domain>,
	// The device-table entry this buffer was created for, if the creator named one.
	//
	// This is what makes the frames reclaimable at the right MOMENT rather than at the convenient
	// one. A driver hands its device a real physical address and there is no IOMMU, so the frames
	// stop being the device's business when the DEVICE stops - which no shootdown, no quota and no
	// handle count can observe.
	device: Option<u32>,
	// Set when the owning process was TERMINATED rather than when it closed this buffer.
	//
	// The difference is the whole rule. A driver that closes its buffer is saying the device is
	// done with it; a driver that faulted said nothing, and its descriptors may still be live in a
	// device that is still running. So a deliberate close retires as it always did, and only the
	// second case holds the frames back - which is also the case a `submit`/`complete` pair cannot
	// help with, because the process that would have called `complete` is the one that died.
	orphaned: AtomicBool,
}

// Frames of terminated drivers, held per device until that device is known to be stopped.
//
// A fixed table rather than a map: the device table is small and fixed at boot, this is touched
// only on a driver's death and on a device's reset, and an allocation on a teardown path is a
// failure mode of its own (see the fallible-allocation sweep in this milestone). `frames` is the
// one Vec, moved in whole from the buffer that owned it.
struct Held {
	device: u32,
	frames: Vec<u64>,
}

// Bounded, because the entries come from processes dying and nothing else prunes them. Past the
// bound the frames are retired as they were before this rule existed: a bounded lifetime bug is
// better than an unbounded list, and the number is logged rather than silently absorbed.
const MAX_HELD: usize = 64;
static HELD: SpinLock<Vec<Held>> = SpinLock::new(Vec::new());

// Hold `frames` until `device` is stopped. Returns them if they cannot be held.
fn hold(device: u32, frames: Vec<u64>) -> Option<Vec<u64>> {
	let mut held = HELD.lock();
	if held.len() >= MAX_HELD || held.try_reserve(1).is_err() {
		return Some(frames);
	}
	held.push(Held { device, frames });
	None
}

// A driver has reset `device`, so nothing it was pointed at is in flight any more: retire every
// frame held for it. Returns how many frames were released.
//
// RETIRED RATHER THAN FREED, for the same reason the ordinary drop path retires: the frames were
// mapped into an address space that other cores may still hold translations for.
pub fn release_for(device: u32) -> usize {
	let taken: Vec<Held> = {
		let mut held = HELD.lock();
		let mut taken: Vec<Held> = Vec::new();
		let mut index = 0;
		while index < held.len() {
			if held[index].device == device {
				if taken.try_reserve(1).is_err() {
					break;
				}
				taken.push(held.swap_remove(index));
			} else {
				index += 1;
			}
		}
		taken
	};
	let mut released = 0;
	for entry in &taken {
		released += entry.frames.len();
		// SAFETY: these frames belonged to a DmaBuffer that has been dropped, so nothing owns them,
		// and the device that could have been writing into them has been reset by the caller.
		unsafe { frame::retire(&entry.frames) };
	}
	released
}

// How many frames are being held for `device`, for the test that is about the holding itself.
#[cfg(test)]
pub fn held_frames_for_test(device: u32) -> usize {
	HELD.lock().iter().filter(|entry| entry.device == device).map(|entry| entry.frames.len()).sum()
}

impl DmaBuffer {
	// Allocate `size` bytes (rounded up to whole pages, at least one) of pinned,
	// physically CONTIGUOUS DMA memory charged to `domain`'s DMA quota - one run,
	// so a device sees a single span (a virtqueue ring, a block data stage, a
	// jumbo frame all ride it whole). The quota is charged before any frame is
	// taken, so an over-cap request fails cleanly (QuotaExceeded) with nothing
	// allocated or charged, and an out-of-memory rolls the charge back.
	pub fn create_in(domain: &Arc<Domain>, size: usize) -> Result<Arc<Self>, MemoryError> {
		Self::create_for(domain, size, None)
	}

	// The same, for a buffer whose physical address is about to be handed to device `device`.
	pub fn create_for(domain: &Arc<Domain>, size: usize, device: Option<u32>) -> Result<Arc<Self>, MemoryError> {
		// A ceiling and checked arithmetic, for the reason `MemoryObject::create_in` has them: the
		// size is a caller's number and the product below is what the quota is then checked against.
		if size as u64 > abi::MAX_OBJECT_BYTES {
			return Err(MemoryError::OutOfMemory);
		}
		let pages = frame::pages_for(size);
		let Some(bytes) = (pages as u64).checked_mul(PAGE_SIZE) else {
			return Err(MemoryError::OutOfMemory);
		};
		if !domain.try_charge_dma(bytes) {
			return Err(MemoryError::QuotaExceeded);
		}
		let base = match frame::allocate_contiguous(pages) {
			Some(b) => b,
			None => {
				domain.uncharge_dma(bytes);
				return Err(MemoryError::OutOfMemory);
			}
		};
		// fallible, like every other metadata allocation sized from a caller's number.
		let mut frames: Vec<u64> = Vec::new();
		if frames.try_reserve_exact(pages).is_err() {
			// SAFETY: the span was allocated by this call and has never been mapped, so it goes
			// straight back rather than through `retire`.
			for i in 0..pages as u64 {
				unsafe { frame::deallocate(base + i * PAGE_SIZE) };
			}
			domain.uncharge_dma(bytes);
			return Err(MemoryError::OutOfMemory);
		}
		frames.extend((0..pages as u64).map(|i| base + i * PAGE_SIZE));
		Ok(Arc::new(Self { header: ObjectHeader::new(), frames, size: pages * PAGE_SIZE as usize, mappings: SpinLock::new(Vec::new()), domain: domain.clone(), device, orphaned: AtomicBool::new(false) }))
	}

	pub fn size(&self) -> usize {
		self.size
	}

	// The device this buffer was created for, if any.
	pub fn device(&self) -> Option<u32> {
		self.device
	}

	// "This buffer's owner did not say it was done with it." Called by process teardown, for every
	// DmaBuffer the dying process holds, BEFORE its handles are closed - so the drop that follows
	// knows which of the two cases it is in.
	pub fn mark_orphaned(&self) {
		self.orphaned.store(true, Ordering::Release);
	}

	pub fn frames(&self) -> &[u64] {
		&self.frames
	}

	// The physical address a driver hands its device for DMA (the first frame).
	pub fn phys_base(&self) -> u64 {
		self.frames.first().copied().unwrap_or(0)
	}

	pub fn is_mapped_in(&self, cr3: u64) -> bool {
		self.mappings.lock().iter().any(|(mapped_cr3, _)| *mapped_cr3 == cr3)
	}

	// Claim the right to map this buffer into `cr3`, under one lock - see the note on
	// `MemoryObject::reserve_mapping`. Asking and then acting is two operations, and two
	// threads of one process fit between them.
	pub fn reserve_mapping(&self, cr3: u64) -> bool {
		let mut mappings = self.mappings.lock();
		if mappings.iter().any(|(mapped_cr3, _)| *mapped_cr3 == cr3) {
			return false;
		}
		mappings.push((cr3, 0));
		true
	}

	pub fn commit_mapping(&self, cr3: u64, base: u64) {
		let mut mappings = self.mappings.lock();
		if let Some(entry) = mappings.iter_mut().find(|(mapped_cr3, _)| *mapped_cr3 == cr3) {
			entry.1 = base;
		}
	}

	pub fn abandon_reservation(&self, cr3: u64) {
		self.mappings.lock().retain(|(mapped_cr3, base)| !(*mapped_cr3 == cr3 && *base == 0));
	}

	pub fn add_mapping(&self, cr3: u64, base: u64) {
		self.mappings.lock().push((cr3, base));
	}

	// Take this buffer's mapping out of `space`. The address space, not a bare cr3, because
	// the virtual range goes back to a pool that lives inside it.
	pub fn remove_mapping(&self, space: &crate::object::address_space::AddressSpace) -> bool {
		let cr3 = space.cr3();
		let base = {
			let mut mappings = self.mappings.lock();
			let Some(index) = mappings.iter().position(|(mapped_cr3, _)| *mapped_cr3 == cr3) else { return false };
			mappings.swap_remove(index).1
		};
		for page in 0..self.frames.len() {
			paging::unmap_page_in(cr3, base + page as u64 * PAGE_SIZE);
		}
		crate::syscall::free_vrange(Some(space), base, self.size as u64);
		true
	}
}

impl_kernel_object!(DmaBuffer, DmaBuffer);

impl Drop for DmaBuffer {
	fn drop(&mut self) {
		debug_assert!(self.mappings.lock().is_empty(), "process cleanup must remove every DmaBuffer mapping");
		// SAFETY: this buffer owns its frames, and the debug assert above has established
		// that nothing is mapping them any more.
		// Retired rather than freed, for the reason `MemoryObject` is: DMA memory is mapped into a
		// driver's address space AND handed to a device, so a stale translation here is the worst
		// version of the case.
		//
		// And a buffer orphaned by a TERMINATION is not retired at all yet: its owner never said
		// the device was finished with it, and retiring puts the frames back in circulation while a
		// descriptor may still name them. They wait for the device to be stopped instead.
		let frames = core::mem::take(&mut self.frames);
		let frames = match (self.device, self.orphaned.load(Ordering::Acquire)) {
			(Some(device), true) => match hold(device, frames) {
				// Held. Nothing to retire here; `release_for` does it when the device is reset.
				None => Vec::new(),
				// The hold table is full. Retiring is what this did before the rule existed, and
				// saying so is better than an unbounded list.
				Some(frames) => {
					crate::serial_println!("dma: hold table full - {} frame(s) of a terminated driver retired while device {} may still write", frames.len(), device);
					frames
				}
			},
			_ => frames,
		};
		unsafe { frame::retire(&frames) };
		// Refund the pinned DMA memory to the owning Domain.
		self.domain.uncharge_dma(self.size as u64);
	}
}

#[cfg(test)]
mod tests;
