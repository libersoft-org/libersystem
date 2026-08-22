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
		crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), index: None, phys_base, len, mapped_at: AtomicU64::new(0), mapped_in: SpinLock::new(None) })
	}

	// The same, for entry `index` of the device table - what `sys_device_acquire` mints.
	pub fn for_device(index: u32, phys_base: u64, len: usize) -> Option<Arc<Self>> {
		crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), index: Some(index), phys_base, len, mapped_at: AtomicU64::new(0), mapped_in: SpinLock::new(None) })
	}

	// The device-table entry this capability is for, if it is for one.
	pub fn device_index(&self) -> Option<u32> {
		self.index
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
}

impl_kernel_object!(DeviceMemory, DeviceMemory);

impl Drop for DeviceMemory {
	fn drop(&mut self) {
		// Tear down the mapping so the VA window is not left pointing at the device
		// after the capability is gone, and return its address range to the window's
		// pool. The physical range is hardware, not owned RAM, so nothing is freed.
		let base = self.mapped_at.load(Ordering::Acquire);
		let space = self.mapped_in.lock().take();
		if base != 0 {
			for i in 0..self.pages() {
				match &space {
					// the address space it was mapped in, which is the only one this
					// mapping was ever in.
					Some(space) => {
						space.unmap(base + i as u64 * PAGE_SIZE);
					}
					// nothing recorded a space: a mapping made before this object learned
					// to remember one, or none at all. Leave the tables alone rather than
					// unmap a page out of whichever space happens to be active.
					None => {}
				}
			}
			crate::syscall::free_vrange(space.as_deref(), base, self.pages() as u64 * PAGE_SIZE);
		}
	}
}
