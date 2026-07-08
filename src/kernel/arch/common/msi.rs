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
		Self { bound: [const { SpinLock::new(None) }; N], used: [const { AtomicBool::new(false) }; N], owner: [const { AtomicU32::new(u32::MAX) }; N] }
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
	pub fn free(&self, slot: usize) {
		*self.bound[slot].lock() = None;
		self.owner[slot].store(u32::MAX, Ordering::Release);
		self.used[slot].store(false, Ordering::Release);
	}

	// The device index `slot` was acquired for (u32::MAX if free).
	pub fn owner(&self, slot: usize) -> u32 {
		self.owner[slot].load(Ordering::Acquire)
	}
}
