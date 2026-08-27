// DeviceMemory kernel object.
//
// A DeviceMemory is a capability to a physical MMIO region (a device's registers
// or BARs). A driver maps it into its address space - uncacheable, since it is
// device registers and not RAM - to talk to its device. Unlike a MemoryObject the
// kernel does not own or free the physical range (it is hardware, not allocated
// RAM) and it is not charged to a memory quota; the capability simply gates which
// driver may reach which device. DeviceManager (later) mints these for the devices
// it discovers and hands each driver only its own.

use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

use super::address_space::AddressSpace;
use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::mem::frame::PAGE_SIZE;
use crate::sync::SpinLock;

pub struct DeviceMemory {
	header: ObjectHeader,
	// Which entry of the device table this capability is for, or None for a region that is not
	// one of them (the tests mint bare MMIO windows).
	//
	// Carried because DMA reclamation is keyed on the DEVICE: a buffer names the device it was
	// created for, and the frames of a driver that died holding one are not recycled until somebody
	// proves that device has been stopped. Both ends of that are capabilities to this object, so
	// the index it names belongs on it rather than in a number passed alongside.
	index: Option<u32>,
	// WHICH BINDING OF THAT DEVICE THIS CAPABILITY BELONGS TO, when it was minted under a claim.
	//
	// The index alone cannot answer it. A device is claimed, released and claimed again, and the
	// index is the same every time - so "everything derived from the PREVIOUS claim" is not a set
	// the index can name. The key carries the generation, which makes it one: ending a claim revokes
	// exactly the capabilities stamped with that generation and leaves the next binding's alone.
	//
	// `None` for the bare MMIO windows the tests mint, which belong to no binding.
	claim: Option<abi::ClaimKey>,
	// Physical base of the MMIO region.
	phys_base: u64,
	// Length of the region in bytes.
	len: usize,
	// Virtual base this region is currently mapped at (0 = unmapped), and the address
	// space it is mapped IN.
	//
	// The address space was not recorded, and `Drop` called the active-address-space
	// `unmap_page` - so the last reference going away in a supervisor, on another thread,
	// or during some other process's teardown unmapped that virtual address from whichever
	// space happened to be current. The unrelated process lost a page, the driver's real
	// mapping survived the capability that justified it, and the virtual range went back
	// to the pool while it was still mapped.
	mapped_at: AtomicU64,
	mapped_in: SpinLock<Option<Arc<AddressSpace>>>,
}

impl DeviceMemory {
	// A capability to the physical MMIO region [phys_base, phys_base + len), naming no device.
	// FALLIBLY, here and in `for_device`: `sys_device_acquire` mints them.
	#[cfg(test)]
	pub fn new(phys_base: u64, len: usize) -> Option<Arc<Self>> {
		crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), index: None, claim: None, phys_base, len, mapped_at: AtomicU64::new(0), mapped_in: SpinLock::new(None) })
	}

	#[cfg(test)]
	// The same, for entry `index` of the device table but under no claim - what the kernel's own
	// bring-up suites mint, standing in for a DeviceManager on a device the booted system has
	// already claimed. Nothing minted this way is revocable, because there is no binding to revoke.
	pub fn for_device(index: u32, phys_base: u64, len: usize) -> Option<Arc<Self>> {
		crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), index: Some(index), claim: None, phys_base, len, mapped_at: AtomicU64::new(0), mapped_in: SpinLock::new(None) })
	}

	// The real one: minted under a claim, and stamped with it - what `SYS_DEVICE_CLAIM` hands out.
	pub fn for_claim(key: abi::ClaimKey, phys_base: u64, len: usize) -> Option<Arc<Self>> {
		crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), index: Some(key.device_index), claim: Some(key), phys_base, len, mapped_at: AtomicU64::new(0), mapped_in: SpinLock::new(None) })
	}

	// The device-table entry this capability is for, if it is for one.
	pub fn device_index(&self) -> Option<u32> {
		self.index
	}

	// The binding this capability was derived from, if it was derived from one.
	pub fn claim(&self) -> Option<abi::ClaimKey> {
		self.claim
	}

	// Number of pages the region spans (at least one), counted from the aligned base so an
	// unaligned region still covers its own tail.
	pub fn pages(&self) -> usize {
		(self.page_offset() as usize + self.len).div_ceil(PAGE_SIZE as usize).max(1)
	}

	// The sentinel a claim holds between `claim_mapping` and `set_mapped_in`.
	const RESERVED: u64 = 1;

	// Claim the right to map this region, once.
	//
	// `mapped_at() != 0` was tested at the top of the map syscall and `set_mapped_in` ran
	// twenty-five lines later, so two threads could both find it unmapped and both build an MMIO
	// mapping - and the second `set_mapped_in` overwrote the record of the first. `Drop` then
	// removed one of them, and the other outlived the capability that authorised it: a mapping of
	// device registers left in a process with no handle to the device.
	//
	// The atomic claim is the same reserve-then-commit `MemoryObject` and `DmaBuffer` were given.
	// `RESERVED` is a value no mapping can have - the windows this maps into never start at 1 - so
	// it marks the claim without pretending to be an address.
	pub fn claim_mapping(&self) -> bool {
		self.mapped_at.compare_exchange(0, Self::RESERVED, Ordering::AcqRel, Ordering::Acquire).is_ok()
	}

	// Give the claim back, for a map that could not be completed.
	pub fn release_claim(&self) {
		let _ = self.mapped_at.compare_exchange(Self::RESERVED, 0, Ordering::AcqRel, Ordering::Acquire);
	}

	// Record where this region is mapped AND in which address space, so the teardown can
	// reach the same one rather than whichever is active when the last reference dies.
	pub fn set_mapped_in(&self, virt: u64, space: Arc<AddressSpace>) {
		*self.mapped_in.lock() = Some(space);
		self.mapped_at.store(virt, Ordering::Release);
	}

	// Where this region is mapped, for the test that asks whether a revocation reached the address
	// space rather than only the handle. The two are different claims and the second alone would
	// pass a revocation that left a live mapping of device registers behind.
	#[cfg(test)]
	pub fn mapped_at_for_test(&self) -> u64 {
		self.mapped_at.load(Ordering::Acquire)
	}

	// The physical offset within the first mapped page.
	//
	// A region whose physical base is not page-aligned is mapped from the page BELOW it,
	// so the address a caller wants is that many bytes into the mapping. Nothing recorded
	// this, and the low bits of a BAR are simply not representable in a PTE - so a device
	// whose registers do not begin on a page boundary handed back a virtual address
	// pointing at the wrong place.
	pub fn page_offset(&self) -> u64 {
		self.phys_base % PAGE_SIZE
	}

	// The page-aligned physical base the mapping actually starts from.
	pub fn aligned_phys_base(&self) -> u64 {
		self.phys_base - self.page_offset()
	}

	// Tear this region's mapping out of the address space it was mapped in, and give its virtual range
	// back. Idempotent: the address is taken with a swap, so a revocation and the drop that follows it
	// do not both unmap - and the second caller finds nothing to do rather than unmapping a range
	// something else has since been given.
	//
	// A METHOD RATHER THAN A `Drop` BODY, because ending a device claim has to reach it while the
	// object is still alive and still held by whoever it was sent to. A handle that refuses is not
	// evidence that a MAPPING is gone: the driver has a raw virtual address it has been using for as
	// long as it has been running, and revoking the capability does not touch it. This does.
	pub fn teardown_mapping(&self) {
		let base = self.mapped_at.swap(0, Ordering::AcqRel);
		if base == 0 || base == Self::RESERVED {
			return;
		}
		let space = self.mapped_in.lock().take();
		for i in 0..self.pages() {
			match &space {
				// the address space it was mapped in, which is the only one this mapping was ever in.
				Some(space) => {
					space.unmap(base + i as u64 * PAGE_SIZE);
				}
				// nothing recorded a space: a mapping made before this object learned to remember
				// one, or none at all. Leave the tables alone rather than unmap a page out of
				// whichever space happens to be active.
				None => {}
			}
		}
		crate::syscall::free_vrange(space.as_deref(), base, self.pages() as u64 * PAGE_SIZE);
	}
}

impl_kernel_object!(DeviceMemory, DeviceMemory);

impl Drop for DeviceMemory {
	fn drop(&mut self) {
		// NOTHING IS DRIVING THIS DEVICE ANY MORE, so it does not master the bus.
		//
		// This used to decrement an owner COUNT and turn bus mastering off at the 1 -> 0 transition.
		// The count is gone - it was the way two owners could be represented, which is the state the
		// claim exists to make impossible - and the property it delivered is kept: a driver that
		// CRASHED disables its own device without knowing the rule exists, because its handle dies
		// with its process and this is the one MMIO capability its binding derived.
		//
		// The CLAIM does not end here. It belongs to whoever took it, which is not the driver, and
		// only a release ends it: a device whose driver died is not free for the next claimant until
		// the teardown has been run and confirmed. What ends here is the device's ability to reach
		// memory, which is the part that must not wait for anybody.
		if let Some(key) = self.claim {
			crate::device::mmio_capability_dropped(key);
		}
		self.teardown_mapping();
	}
}
