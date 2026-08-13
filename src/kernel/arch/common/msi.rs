// Portable per-device MSI-X slot registry - the interrupt-binding bookkeeping shared
// by every interrupt-controller backend.
//
// A device MSI-X vector is tracked as a fixed slot: each slot records whether it is
// reserved, which driver `Interrupt` to wake when it fires (held weakly, so a gone
// driver clears its own binding on Drop), and which discovered device it was acquired
// for (retained so `lsirq` can show the vector-to-device map). This registry owns that
// per-slot state and the reserve / bind / dispatch / free operations, all in terms of
// SLOT indices.
//
// What stays in the arch backend is only what is genuinely arch-specific: the
// slot <-> hardware-vector mapping (x86 vector = MSI_BASE + slot; a GICv2m SPI =
// BASE_SPI + slot), programming the device's MSI-X table entry (an x86 LAPIC message
// vs a GICv2m frame write), and the delivery path (an IDT stub + LAPIC EOI vs the GIC
// INTID read). The backend converts a hardware vector to a slot at its boundary and
// calls the registry, so a new architecture reuses all of this bookkeeping.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use alloc::sync::{Arc, Weak};

use crate::object::interrupt::Interrupt;
use crate::sync::SpinLock;

// `N` MSI slots. `N` is the count of per-device vectors the backend tracks (x86's
// global MSI window, or the SPIs a GICv2m frame owns); fixed-size tables keep the
// bindings off the heap and safe to touch from the interrupt path.
pub struct MsiRegistry<const N: usize> {
	// The Interrupt to wake when each slot's vector fires, held weakly so a gone
	// driver's binding drops itself.
	bound: [SpinLock<Option<Weak<Interrupt>>>; N],
	// Reservation flag per slot, set when the slot is acquired and cleared on free.
	used: [AtomicBool; N],
	// Set when a slot's binding is gone but its DEVICE has never been confirmed stopped.
	//
	// Masking an MSI-X entry stops the NEXT message, not one already in flight - the teardown says
	// so itself - so a vector freed the instant its binding drops can be handed to another driver
	// while the last device's message is still on its way, and that driver is woken by hardware it
	// does not own. The honest answer is the one the DMA rule reached: masking is a request, and
	// only the device says it stopped. A pending slot is `used` (so nothing acquires it) with its
	// owner retained (so `release_for_device` can find it when the device is quiesced).
	pending: [AtomicBool; N],
	// The discovered-device index each slot was acquired for (u32::MAX = none),
	// retained for the `lsirq` inventory.
	owner: [AtomicU32; N],
}

impl<const N: usize> Default for MsiRegistry<N> {
	fn default() -> Self {
		Self::new()
	}
}

impl<const N: usize> MsiRegistry<N> {
	pub const fn new() -> Self {
		Self { bound: [const { SpinLock::new(None) }; N], used: [const { AtomicBool::new(false) }; N], pending: [const { AtomicBool::new(false) }; N], owner: [const { AtomicU32::new(u32::MAX) }; N] }
	}

	// Reserve a free slot for device `owner`, searching the first `limit` slots
	// (capped at `N`), returning its index (None if every candidate slot is taken).
	// `limit` lets a backend expose fewer live vectors than the table holds (a GICv2m
	// frame owns only the SPIs its TYPER reports); pass `N` to use them all. The caller
	// then programs the device's MSI-X table for the slot's hardware vector and binds
	// an Interrupt with `bind`.
	pub fn acquire(&self, owner: u32, limit: usize) -> Option<usize> {
		for slot in 0..limit.min(N) {
			if self.used[slot].compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
				continue;
			}
			self.owner[slot].store(owner, Ordering::Release);
			return Some(slot);
		}
		None
	}

	// Whether `owner` already holds a LIVE slot - `used` and not `pending`.
	//
	// ONE LIVE VECTOR PER DEVICE, and this is where the question is answered rather than where it is
	// enforced: the policy belongs at `sys_device_msix_acquire`, the userspace path whose one caller
	// is DeviceManager, and not in this mechanism, which the kernel's own bring-up test calls
	// directly to stand in for DeviceManager on a device the booted system has already claimed.
	//
	// Why the question matters at all: every backend programs the device's MSI-X table ENTRY 0 for
	// whatever slot it was given, so two live slots for one device would both be programmed into one
	// entry - the second overwriting the first, and the first's `unbind` later masking the entry the
	// second is live on. An old handle silently disabling a new one.
	//
	// A PENDING slot does not count. Its `Interrupt` is already gone; what the pending state protects
	// is that no new owner is given that VECTOR while a message for it may still be in flight, which
	// is a fact about the slot rather than about the device. A restarting driver reprogramming its
	// own device's entry 0 is exactly what should happen.
	pub fn has_live(&self, owner: u32) -> bool {
		if owner == u32::MAX {
			return false;
		}
		(0..N).any(|slot| self.used[slot].load(Ordering::Acquire) && !self.pending[slot].load(Ordering::Acquire) && self.owner[slot].load(Ordering::Acquire) == owner)
	}

	// Reserve one SPECIFIC slot for device `owner`, for a backend whose slot is fixed by
	// the hardware rather than freely chosen: on riscv a device's PLIC INTx source is
	// determined by its PCI slot + pin, so the source id IS the slot and the caller
	// cannot pick a different free one. Returns false if the slot is already reserved.
	#[allow(dead_code)] // only the riscv INTx-over-PLIC backend fixes its slot this way
	pub fn acquire_at(&self, slot: usize, owner: u32) -> bool {
		if slot >= N {
			return false;
		}
		if self.used[slot].compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
			return false;
		}
		self.owner[slot].store(owner, Ordering::Release);
		true
	}

	// Bind `intr` to `slot` so `dispatch` wakes it when the slot's vector fires.
	// Returns false if the slot is already bound to a live Interrupt.
	pub fn bind(&self, slot: usize, intr: &Arc<Interrupt>) -> bool {
		let mut bound = self.bound[slot].lock();
		if bound.as_ref().and_then(Weak::upgrade).is_some() {
			return false;
		}
		*bound = Some(Arc::downgrade(intr));
		intr.mark_bound();
		true
	}

	// Whether `slot` currently has a live driver binding. Used to confirm a crashed
	// driver's IRQ was detached during cleanup, and for the `lsirq` inventory.
	pub fn is_bound(&self, slot: usize) -> bool {
		self.bound[slot].lock().as_ref().and_then(Weak::upgrade).is_some()
	}

	// Wake the driver bound to `slot`, if any. MSI is edge-triggered and unshared, so
	// there is no level source to mask - just signal.
	pub fn dispatch(&self, slot: usize) {
		if let Some(intr) = self.bound[slot].lock().as_ref().and_then(Weak::upgrade) {
			intr.signal();
		}
	}

	// Drop `slot`'s binding and free it for re-use (called from an Interrupt's Drop).
	//
	// IMMEDIATE, so only for a slot no device can still raise: a backend whose source is masked at
	// the CONTROLLER rather than at the device, or one acquired and never programmed. Anything
	// behind a device's own MSI-X table wants `retire`.
	pub fn free(&self, slot: usize) {
		*self.bound[slot].lock() = None;
		self.pending[slot].store(false, Ordering::Release);
		self.owner[slot].store(u32::MAX, Ordering::Release);
		self.used[slot].store(false, Ordering::Release);
	}

	// Drop `slot`'s binding and record the slot as PENDING rather than free.
	//
	// The vector stays out of circulation until `release_for_device` is told the device stopped.
	// A slot with no owner has no device to wait for and is freed outright.
	pub fn retire(&self, slot: usize) {
		*self.bound[slot].lock() = None;
		if self.owner[slot].load(Ordering::Acquire) == u32::MAX {
			self.used[slot].store(false, Ordering::Release);
			return;
		}
		self.pending[slot].store(true, Ordering::Release);
	}

	// Free every slot pending for `device`, and answer how many. Reached from
	// `SYS_DEVICE_QUIESCED`, which is the holder of the device's own DeviceMemory saying the
	// hardware is stopped - the same claim, from the same capability, that releases its DMA frames.
	pub fn release_for_device(&self, device: u32) -> usize {
		let mut released = 0;
		for slot in 0..N {
			if self.pending[slot].load(Ordering::Acquire) && self.owner[slot].load(Ordering::Acquire) == device {
				self.pending[slot].store(false, Ordering::Release);
				self.owner[slot].store(u32::MAX, Ordering::Release);
				self.used[slot].store(false, Ordering::Release);
				released += 1;
			}
		}
		released
	}

	// Whether `slot` is masked and waiting for its device to be confirmed stopped.
	pub fn is_pending(&self, slot: usize) -> bool {
		self.pending[slot].load(Ordering::Acquire)
	}

	// The device index `slot` was acquired for (u32::MAX if free).
	pub fn owner(&self, slot: usize) -> u32 {
		self.owner[slot].load(Ordering::Acquire)
	}
}

#[cfg(test)]
mod tests {
	use super::MsiRegistry;

	crate::tagged_test!(a_masked_msi_vector_is_not_reused_until_its_device_is_confirmed_stopped, [Kernel, Drivers, Interrupt], id = "kernel.arch.common.msi.a_masked_msi_vector_is_not_reused_until_its_device_is_confirmed_stopped", covers = ["kernel"]);
	fn a_masked_msi_vector_is_not_reused_until_its_device_is_confirmed_stopped() {
		// Masking an MSI-X entry stops the NEXT message. A vector freed the instant its binding
		// dropped could therefore be handed to another driver while the last device's message was
		// still on its way, and that driver would be woken by hardware it does not own - with
		// nothing on either side able to tell. The vector waits for the device instead, which is
		// the same rule, from the same capability, that releases the driver's DMA frames.
		const DEVICE: u32 = 3;
		const OTHER: u32 = 4;
		let registry: MsiRegistry<2> = MsiRegistry::new();

		let first = registry.acquire(DEVICE, 2).expect("a fresh registry has slots");
		let second = registry.acquire(OTHER, 2).expect("and a second one");
		assert!(registry.acquire(DEVICE, 2).is_none(), "two slots is two slots");

		// The driver dies: its binding goes, the entry is masked, and the slot does NOT come back.
		registry.retire(first);
		assert!(registry.is_pending(first), "the vector is recorded as pending rather than free");
		assert_eq!(registry.owner(first), DEVICE, "and still names the device it is waiting on");
		assert!(registry.acquire(OTHER, 2).is_none(), "a pending vector is not handed to the next driver");

		// Quiescing a DIFFERENT device releases nothing - the claim is per device.
		assert_eq!(registry.release_for_device(OTHER), 0, "the other device's quiesce does not free this vector");
		assert!(registry.is_pending(first), "still pending");

		// And the device's own capability holder saying it stopped is what frees it.
		assert_eq!(registry.release_for_device(DEVICE), 1, "the quiesce releases exactly this device's pending vectors");
		assert!(!registry.is_pending(first), "no longer pending");
		// A THIRD device asks for it, because `OTHER` already holds one: a device may hold one live
		// slot, since every backend programs its MSI-X table entry 0 and two slots for one device
		// would alias there. `OTHER` was used here as a convenience and the rule made it wrong.
		const THIRD: u32 = 5;
		assert_eq!(registry.acquire(THIRD, 2), Some(first), "and the vector is available again");

		// A slot with no device behind it has nothing to wait for and is freed outright.
		registry.free(second);
		let third = registry.acquire(u32::MAX, 2).expect("the freed slot is available");
		registry.retire(third);
		assert!(!registry.is_pending(third), "an ownerless slot is freed rather than left pending");
		assert!(registry.acquire(DEVICE, 2).is_some(), "and can be acquired at once");
	}
}
