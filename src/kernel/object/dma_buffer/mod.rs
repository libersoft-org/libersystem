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

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
	// THE TRANSLATION, WHERE THERE IS ONE. Present exactly when the device this buffer was created
	// for is behind an enforcing IOMMU: the address handed to the driver is then the IOVA rather
	// than the physical address, and the frames are not reusable until the unmap and the
	// invalidation have both completed.
	//
	// `None` is the untranslated case, which is every device in this tree today - and the comment
	// above `device` says what that costs.
	translation: SpinLock<Option<(dma::MappingId, dma::DmaAddress)>>,
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

// Bounded, because the entries come from processes dying and nothing else prunes them.
//
// The `Vec` behind a `try_reserve` was the wrong shape for the same reason the frame quarantine's
// was: metadata for dead drivers must not be able to fail for want of a heap, and a driver dying is
// exactly when the heap is likely to be short. A fixed table allocates nothing.
const MAX_HELD: usize = 64;
static HELD: SpinLock<[Option<Held>; MAX_HELD]> = SpinLock::new([const { None }; MAX_HELD]);

// How many frames the table could not hold and this kernel has therefore leaked ON PURPOSE, rather
// than hand back to an allocator while a device may still be writing into them.
static LEAKED: AtomicUsize = AtomicUsize::new(0);

// Hold `frames` until `device` is stopped.
//
// LEAKS RATHER THAN RETIRES WHEN IT CANNOT. Past the bound this used to return the frames, and the
// caller retired them - printing, to its credit, "frames of a terminated driver retired while
// device may still write". That is the kernel saying out loud that it is handing physical memory a
// device may be writing into to whoever allocates next: a use-after-free with somebody else's page
// on the receiving end, and no bound on what it damages.
//
// A pinned frame is a bounded loss the machine survives. The count is reported beside the frame
// allocator's other losses so a machine accumulating them is diagnosable, which is the whole
// difference between a leak and a disappearance.
fn hold(device: u32, frames: Vec<u64>) {
	let mut held = HELD.lock();
	if let Some(slot) = held.iter_mut().find(|slot| slot.is_none()) {
		*slot = Some(Held { device, frames });
		return;
	}
	LEAKED.fetch_add(frames.len(), Ordering::Relaxed);
	// Counted in the frame allocator's LOST total as well, which is the number a machine losing
	// memory is diagnosed from. A private counter here only would mean that total says "some of
	// the losses".
	frame::note_lost_pages(frames.len() as u64);
	crate::serial_println!("dma: the hold table is full - {} frame(s) of a terminated driver are LEAKED rather than returned, because device {} was never confirmed stopped ({} leaked so far, {} pages lost in all)", frames.len(), device, LEAKED.load(Ordering::Relaxed), frame::lost_pages());
	// Dropped here: the `Vec` goes, the frames stay out of circulation. Deliberately NOT retired.
}

// A driver has reset `device`, so nothing it was pointed at is in flight any more: retire every
// frame held for it. Returns how many frames were released.
//
// RETIRED RATHER THAN FREED, for the same reason the ordinary drop path retires: the frames were
// mapped into an address space that other cores may still hold translations for.
pub fn release_for(device: u32) -> usize {
	// Taken out of the fixed table one slot at a time, so this allocates nothing either - the
	// vector it used to build was one more thing that could fail on the path that exists to keep
	// memory from being lost.
	let mut released = 0usize;
	loop {
		let entry = {
			let mut held = HELD.lock();
			let found = held.iter_mut().find(|slot| slot.as_ref().is_some_and(|held| held.device == device));
			match found {
				Some(slot) => slot.take(),
				None => None,
			}
		};
		let Some(entry) = entry else { break };
		released += entry.frames.len();
		// SAFETY: these frames belonged to a DmaBuffer that has been dropped, so nothing owns them,
		// and the device that could have been writing into them has been reset by the caller.
		unsafe { frame::retire(&entry.frames) };
	}
	released
}

// How many frames the hold table has leaked, and a way to empty it WITHOUT retiring - both for the
// overflow test, which fills the table with frame numbers that were never allocated. Retiring those
// would hand the frame allocator addresses it does not own, so the test cannot use `release_for`.
#[cfg(test)]
pub fn leaked_frames_for_test() -> usize {
	LEAKED.load(Ordering::Relaxed)
}

#[cfg(test)]
pub fn forget_for_test(device: u32) {
	for slot in HELD.lock().iter_mut() {
		if slot.as_ref().is_some_and(|held| held.device == device) {
			*slot = None;
		}
	}
}

#[cfg(test)]
pub fn hold_for_test(device: u32, frames: Vec<u64>) {
	hold(device, frames);
}

// How many frames are being held for `device`, for the test that is about the holding itself.
#[cfg(test)]
pub fn held_frames_for_test(device: u32) -> usize {
	HELD.lock().iter().flatten().filter(|entry| entry.device == device).map(|entry| entry.frames.len()).sum()
}

impl DmaBuffer {
	// Allocate `size` bytes (rounded up to whole pages, at least one) of pinned,
	// physically CONTIGUOUS DMA memory charged to `domain`'s DMA quota - one run,
	// so a device sees a single span (a virtqueue ring, a block data stage, a
	// jumbo frame all ride it whole). The quota is charged before any frame is
	// taken, so an over-cap request fails cleanly (QuotaExceeded) with nothing
	// allocated or charged, and an out-of-memory rolls the charge back.
	#[cfg(test)]
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
			for i in 0..pages as u64 {
				// SAFETY: the span was allocated by this call and has never been mapped, so it
				// goes straight back rather than through `retire`.
				// NEVER-MAPPED: allocated a few lines above and refused before any mapping was
				// made - the metadata vector this rollback is for is what failed.
				unsafe { frame::deallocate(base + i * PAGE_SIZE) };
			}
			domain.uncharge_dma(bytes);
			return Err(MemoryError::OutOfMemory);
		}
		frames.extend((0..pages as u64).map(|i| base + i * PAGE_SIZE));

		// THE TRANSLATION IS INSTALLED AND CONFIRMED BEFORE THE HANDLE EXISTS, which is the ordering
		// the whole milestone turns on: a driver that has a handle has an address, and an address
		// that is not yet translated is one the device would put on the bus untranslated. A map that
		// does not confirm is a refusal here, with the frames and the quota given back - not a
		// buffer that works for a while.
		//
		// BIDIRECTIONAL, because a `DmaBuffer` is a region a driver may use either way and this
		// interface does not yet carry the direction of the intended use. That is a WEAKER claim
		// than the contract can express - `Direction` is there and the model checks it - and it is
		// stated rather than dressed up: narrowing it needs the create syscall to say which way the
		// buffer is used, which the protected endpoints will need when they are migrated.
		let translation = match device.filter(|_| crate::iommu::present()) {
			Some(index) => match crate::iommu::map_device_buffer(index, base, (pages * PAGE_SIZE as usize) as u64) {
				Ok(Some(mapped)) => Some(mapped),
				Ok(None) => None,
				Err(_) => {
					for i in 0..pages as u64 {
						// NEVER-MAPPED: allocated in this call and refused before any mapping was
						// made or any address published.
						// SAFETY: the span was allocated by this call and has never been mapped.
						unsafe { frame::deallocate(base + i * PAGE_SIZE) };
					}
					domain.uncharge_dma(bytes);
					return Err(MemoryError::OutOfMemory);
				}
			},
			None => None,
		};
		crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), frames, size: pages * PAGE_SIZE as usize, mappings: SpinLock::new(Vec::new()), domain: domain.clone(), device, translation: SpinLock::new(translation), orphaned: AtomicBool::new(false) }).ok_or(MemoryError::OutOfMemory)
	}

	pub fn size(&self) -> usize {
		self.size
	}

	// "This buffer's owner did not say it was done with it." Called by process teardown, for every
	// DmaBuffer the dying process holds, BEFORE its handles are closed - so the drop that follows
	// knows which of the two cases it is in.
	pub fn mark_orphaned(&self) {
		self.orphaned.store(true, Ordering::Release);
	}

	#[cfg(test)]
	pub fn is_orphaned_for_test(&self) -> bool {
		self.orphaned.load(Ordering::Acquire)
	}

	// The device-table entry this buffer was created for, or None for one bound to nothing.
	//
	// Asked by `sys_dma_buffer_phys`: a physical address is what lets a device write into a page,
	// so it is answerable only for a buffer some device was named for.
	pub fn device(&self) -> Option<u32> {
		self.device
	}

	pub fn frames(&self) -> &[u64] {
		&self.frames
	}

	// THE ADDRESS THE DEVICE USES, which is not always a physical one.
	//
	// Where the device is translated this is an IOVA the kernel can revoke; where it is not, it is
	// the raw physical address `SYS_DMA_BUFFER_PHYS` has always handed out - an integer nothing
	// constrains the device to, which is the exposure translated DMA exists to replace and which remains
	// only under the explicit `trusted-untranslated` policy.
	pub fn device_address(&self) -> u64 {
		if let Some((_, address)) = *self.translation.lock() {
			return address.get();
		}
		self.frames.first().copied().unwrap_or(0)
	}

	// Whether this buffer's address is one the kernel can take back.
	pub fn is_translated(&self) -> bool {
		self.translation.lock().is_some()
	}

	// Claim the right to map this buffer into `cr3`, under one lock - see the note on
	// `MemoryObject::reserve_mapping`. Asking and then acting is two operations, and two
	// threads of one process fit between them.
	pub fn reserve_mapping(&self, cr3: u64) -> bool {
		let mut mappings = self.mappings.lock();
		if mappings.iter().any(|(mapped_cr3, _)| *mapped_cr3 == cr3) {
			return false;
		}
		// FALLIBLE, because the refusal channel is already here and this was the one allocation
		// using it for nothing. Reachable from `SYS_MEMORY_MAP` / `SYS_DMA_BUFFER_MAP`: a caller
		// that maps objects until the heap is short turned a `false` this function already knows
		// how to say into a kernel abort.
		if mappings.try_reserve(1).is_err() {
			return false;
		}
		mappings.push((cr3, 0));
		true
	}

	// The reservation slot only - see `MemoryObject::commit_mapping`, which this mirrors, and which
	// carries the reasoning for both.
	pub fn commit_mapping(&self, cr3: u64, base: u64) -> bool {
		let mut mappings = self.mappings.lock();
		match mappings.iter_mut().find(|(mapped_cr3, mapped_base)| *mapped_cr3 == cr3 && *mapped_base == 0) {
			Some(entry) => {
				entry.1 = base;
				true
			}
			None => false,
		}
	}

	pub fn abandon_reservation(&self, cr3: u64) {
		self.mappings.lock().retain(|(mapped_cr3, base)| !(*mapped_cr3 == cr3 && *base == 0));
	}

	// Take this buffer's mapping out of `space`. The address space, not a bare cr3, because
	// the virtual range goes back to a pool that lives inside it.
	pub fn remove_mapping(&self, space: &crate::object::address_space::AddressSpace) -> bool {
		let cr3 = space.cr3();
		let base = {
			let mut mappings = self.mappings.lock();
			// A COMMITTED MAPPING ONLY - the same hazard `MemoryObject::remove_mapping` describes at
			// length: matching on `cr3` alone also matched an in-flight RESERVATION, so a sibling
			// thread's unmap stole it, unmapped the first pages of the address space, and left the
			// real mapping in the page tables with nothing recording it.
			let Some(index) = mappings.iter().position(|(mapped_cr3, base)| *mapped_cr3 == cr3 && *base != 0) else { return false };
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
		// THE TRANSLATION COMES DOWN BEFORE THE FRAMES GO ANYWHERE. Until the unmap and the
		// invalidation have both completed the device may still resolve this address, so a frame
		// released here would be one handed to its next owner while a descriptor can still reach it.
		// An unconfirmed release quarantines: the pages are lost rather than reused.
		let released = match self.translation.lock().take() {
			Some((id, _)) => match crate::iommu::unmap_for_device(id) {
				Ok(dma::Release::FramesReusable) => true,
				_ => {
					crate::serial_println!("dma: an unmap did not complete - {} frame(s) are QUARANTINED rather than returned", self.frames.len());
					false
				}
			},
			None => true,
		};
		if !released {
			// Deliberately leaked: see above. Nothing here is recoverable by a later pass, because
			// nothing can establish that the device stopped being able to reach them.
			let _ = core::mem::take(&mut self.frames);
			return;
		}
		let frames = core::mem::take(&mut self.frames);
		let frames = match (self.device, self.orphaned.load(Ordering::Acquire)) {
			// Held, or - if the table is full - leaked inside `hold`. Either way nothing to retire
			// here; `release_for` retires the held ones when the device is reset, and the leaked
			// ones are gone on purpose rather than handed to the next allocator under a live DMA
			// descriptor. That is why `hold` has nothing to give back.
			(Some(device), true) => {
				hold(device, frames);
				Vec::new()
			}
			_ => frames,
		};
		unsafe { frame::retire(&frames) };
		// Refund the pinned DMA memory to the owning Domain.
		self.domain.uncharge_dma(self.size as u64);
	}
}

#[cfg(test)]
mod tests;
