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
		crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), claim: None, phys_base, len, mapped_at: AtomicU64::new(0), mapped_in: SpinLock::new(None) })
	}

	// The real one: minted under a claim, and stamped with it - what `SYS_DEVICE_CLAIM` hands out.
	pub fn for_claim(key: abi::ClaimKey, phys_base: u64, len: usize) -> Option<Arc<Self>> {
		crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), claim: Some(key), phys_base, len, mapped_at: AtomicU64::new(0), mapped_in: SpinLock::new(None) })
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

	// The sentinel a claim holds between `claim_mapping` and `commit_mapping`.
	const RESERVED: u64 = 1;

	// THE TOMBSTONE A TEARDOWN LEAVES, and it is what makes the commit below claim-current.
	//
	// The sweep and the map syscall could both run and neither could see the other. `claim_mapping`
	// takes `RESERVED`, the syscall then allocates a range and installs PTEs, and only afterwards
	// records where it mapped. A release landing in that window found `RESERVED`, read it as "not
	// mapped yet", and returned having unmapped nothing - and the syscall then published a live
	// mapping of device registers AFTER the only sweep the claim will ever run. The holder kept raw
	// BAR access with the claim already `Free`, which is the property this milestone exists to
	// deny.
	//
	// A value no mapping can have, like `RESERVED`, and one that is TERMINAL: once a teardown has
	// swapped it in, `claim_mapping` can never take the object again and the in-flight commit
	// cannot publish. The mapping is one-shot by design, so there is nothing to reset it for.
	const REVOKED: u64 = 2;

	// Claim the right to map this region, once.
	//
	// `mapped_at() != 0` was tested at the top of the map syscall and the record was written
	// twenty-five lines later, so two threads could both find it unmapped and both build an MMIO
	// mapping - and the second write overwrote the record of the first. `Drop` then
	// removed one of them, and the other outlived the capability that authorised it: a mapping of
	// device registers left in a process with no handle to the device.
	//
	// The atomic claim is the same reserve-then-commit `MemoryObject` and `DmaBuffer` were given.
	// `RESERVED` is a value no mapping can have - the windows this maps into never start at 1 - so
	// it marks the claim without pretending to be an address.
	pub fn claim_mapping(&self) -> bool {
		self.mapped_at.compare_exchange(0, Self::RESERVED, Ordering::AcqRel, Ordering::Acquire).is_ok()
	}

	// COMMIT THE MAPPING, OR REFUSE IT BECAUSE THE CLAIM ENDED WHILE IT WAS BEING BUILT.
	//
	// The record used to be stored unconditionally, so a teardown that ran between `claim_mapping`
	// and the store was overwritten by it. This is the same reserve-then-commit the claim uses, closed at
	// the other end: the commit is a CAS off `RESERVED`, so a `REVOKED` tombstone left by a sweep
	// makes it fail and the caller unmaps what it had installed.
	//
	// `false` means the mapping must be torn down by its builder, because nothing else will: the
	// sweep that set the tombstone had nothing to find.
	pub fn commit_mapping(&self, virt: u64, space: Arc<AddressSpace>) -> bool {
		*self.mapped_in.lock() = Some(space);
		if self.mapped_at.compare_exchange(Self::RESERVED, virt, Ordering::AcqRel, Ordering::Acquire).is_err() {
			// The space record is dropped with it, so a later teardown cannot reach into an address
			// space for a mapping this call is about to remove itself.
			*self.mapped_in.lock() = None;
			return false;
		}
		// AND THE DEVICE IS TOLD IT HAS A LIVE MAPPING, which is what a release waits for.
		//
		// A sweep reaches this object through a WEAK reference, and `Weak::upgrade` fails as soon as
		// the last strong count reaches zero - which is BEFORE `Drop` has run the teardown. So a row
		// whose destructor was running was counted quiet, the claim went `Free`, and the unmap and
		// the cross-core flush happened afterwards with nobody waiting for them. The count is taken
		// HERE, where the mapping becomes real, so it is already standing whichever way the object
		// later dies: the sweep upgrades it and tears it down, or a concurrent drop does, and either
		// way the release sees an outstanding mapping until the teardown has finished.
		//
		// Exactly the shape `settled_vectors` uses for interrupts, for exactly the same race.
		if let Some(key) = self.claim {
			crate::device::mmio_mapping_installed(key);
		}
		true
	}

	// Give the claim back, for a map that could not be completed.
	pub fn release_claim(&self) {
		let _ = self.mapped_at.compare_exchange(Self::RESERVED, 0, Ordering::AcqRel, Ordering::Acquire);
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
	// Answers whether the teardown was CONFIRMED. See the shootdown below: a cross-core flush that
	// could not be confirmed leaves another core able to translate the BAR, and a claim whose
	// terminal state was decided on the strength of `true` would publish `Free` over exactly that.
	pub fn teardown_mapping(&self) -> bool {
		// SWAPPED TO THE TOMBSTONE, NOT TO ZERO. Zero is the state a fresh object is in, so a sweep
		// that wrote it left the object mappable again - and, worse, indistinguishable from one that
		// had never been mapped, which is how an in-flight commit could publish past the sweep. The
		// tombstone is terminal and says which of the two happened.
		let base = self.mapped_at.swap(Self::REVOKED, Ordering::AcqRel);
		if base == 0 || base == Self::REVOKED {
			// Nothing was ever installed through this object, or a previous teardown already took it
			// down and answered for it. Either way this call has nothing to confirm.
			return true;
		}
		if base == Self::RESERVED {
			// A MAP SYSCALL IS BETWEEN ITS CLAIM AND ITS COMMIT, AND THAT IS NOT A CONFIRMATION
			// (corrected 2026-09-03).
			//
			// This answered `true`, on the reasoning that the tombstone makes the commit fail and
			// the syscall unmaps its own work. Both halves are true and the conclusion was not: the
			// syscall installs every page table entry BEFORE it attempts the commit, so at this
			// instant a live mapping of the device's registers exists and the builder has not yet
			// been told to take it down. Answering `true` let `finish_release` publish `Free` over
			// exactly that interval, and the next claimant could be given a device whose previous
			// holder still had its BAR mapped.
			//
			// The honest answer is the one this whole file gives everywhere else: a teardown that
			// could not confirm leaves the claim `Quarantined`. The builder still removes its own
			// work - nothing else can, because this sweep found nothing to unmap - and the device is
			// not handed to anybody while that is outstanding.
			crate::serial_println!("device: a claim ended while a map of its registers was being built - the mapping is the builder's to remove and this teardown cannot confirm it");
			return false;
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
		// AND EVERY OTHER CORE IS TOLD, before the range is given back.
		//
		// `AddressSpace::unmap` clears the PTE and invalidates THIS core's translation buffer, and
		// `mem::tlb` says in its own first paragraph that nothing else is told. A driver is a process
		// with threads, and one of them on another core keeps a cached translation for the BAR - so
		// revoking the capability and tearing the mapping out still left that thread reaching the
		// device's registers, which is the exact half of M2's revocation this method exists to
		// perform. The freed virtual range has the same problem one step later: whatever is mapped
		// there next is reachable through the stale entry.
		//
		// Blunt and synchronous, like every other caller: it flushes each online core and waits.
		// This runs once per binding teardown, which is where that cost belongs.
		//
		// AND THE ANSWER IS READ (2026-08-31). `shootdown` returns `false` for a core it could not
		// reach and for a wait that went on too long, and says so in its own contract - and this
		// discarded it and freed the range anyway. Two consequences, and the second is the one this
		// milestone is about: whatever is mapped at that virtual range NEXT is reachable through the
		// stale entry, and `revoke_effects_of` reported the revocation quiet, so the claim could
		// reach `Free` while another core still held a translation for the BAR.
		//
		// A RANGE THAT COULD NOT BE FLUSHED IS NOT GIVEN BACK. That is the same choice
		// `frame::retire` makes for physical pages and for the same reason: losing an address range
		// is a cost, and handing back one a live core can still translate is a correctness failure.
		// The mapping itself is gone either way - the page-table entries were cleared above - so what
		// is retained is the RANGE, not the access.
		if !crate::mem::tlb::shootdown() {
			crate::serial_println!("device: a revoked MMIO window could not confirm its cross-core flush - the virtual range is retained rather than reused, and the claim is not free");
			// CHARGED TO THE DEVICE, NOT ONLY RETURNED TO THIS CALLER. A `Drop` has no caller to
			// refuse, so an unconfirmed flush inside a destructor used to be lost entirely; the
			// count is where a release reads it whichever path ran the teardown.
			self.teardown_finished(false);
			return false;
		}
		crate::syscall::free_vrange(space.as_deref(), base, self.pages() as u64 * PAGE_SIZE);
		self.teardown_finished(true);
		true
	}

	// The mapping this object had is gone, and whether its cross-core flush confirmed.
	//
	// One place, because both exits of the teardown owe it and a release is waiting on the count:
	// giving it back before the flush would let the wait pass while another core could still
	// translate the BAR.
	fn teardown_finished(&self, confirmed: bool) {
		if let Some(key) = self.claim {
			crate::device::mmio_mapping_torn_down(key, confirmed);
		}
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
		// The answer has nowhere to go from a `Drop` - there is no caller to refuse - and it is not
		// lost either: `teardown_mapping` says so on the console, and the range it could not confirm
		// is retained rather than handed back. A forced release reads the same answer through
		// `revoke_effects_of`, where there IS a claim to keep out of circulation.
		let _ = self.teardown_mapping();
	}
}
