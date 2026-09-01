// Interrupt kernel object.
//
// An Interrupt is a capability to a device IRQ, bound to a vector. When the IRQ
// fires the kernel marks the Interrupt pending and wakes any thread blocked on it
// (via wait), so a userspace driver sleeps until its device needs attention rather
// than polling. The interrupt-dispatch table holds the binding weakly, so closing
// the handle (or the driver dying) drops the Interrupt, which unbinds its vector -
// the kernel stops delivering to a driver that is gone.

use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicBool, Ordering};

use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::sched;

pub struct Interrupt {
	header: ObjectHeader,
	// THE ARCHITECTURAL INTERRUPT ID, at the width the architecture uses (KERN-ARCH-017).
	//
	// An x86 IDT vector fits a byte; a GICv2m SPI is ten bits and an IMSIC EID is eleven, so on two
	// of three ports a `u8` was a truncation - the hardware stayed armed under one identifier while
	// the kernel's registry, bind, cleanup and reporting paths all named another.
	vector: u32,
	// Set when the IRQ has fired and not yet been cleared; the wait readiness.
	pending: AtomicBool,
	// Set once this Interrupt actually owns its vector's binding, so only the owner
	// unbinds on drop (a refused bind's Interrupt leaves the live binding alone).
	bound: AtomicBool,
	// REVOKED: the claim this was derived from has been released, and this object's authority ended
	// with it. Set by `revoke`, never cleared - a revoked interrupt does not come back.
	revoked: AtomicBool,
}

impl Interrupt {
	// FALLIBLY: `SYS_IRQ_BIND` and `SYS_DEVICE_MSIX_ACQUIRE` reach this.
	pub fn new(vector: u32) -> Option<Arc<Self>> {
		crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), vector, pending: AtomicBool::new(false), bound: AtomicBool::new(false), revoked: AtomicBool::new(false) })
	}

	pub fn vector(&self) -> u32 {
		self.vector
	}

	// Mark this Interrupt as the owner of its vector binding (called by bind()).
	pub fn mark_bound(&self) {
		self.bound.store(true, Ordering::Release);
	}

	// Mark the interrupt pending and wake any thread blocked waiting on it. Called
	// from the interrupt-dispatch path when the bound vector fires.
	pub fn signal(&self) {
		// A REVOKED INTERRUPT IS NOT SIGNALLED. The dispatch table holds this weakly and a driver
		// that is still running - or a wait that already resolved the object - holds it strongly, so
		// revoking the capability alone left a live path from a released device's vector into the
		// old holder. The vector is unbound by `revoke` as well; this is the second half, for a
		// message already in flight when that happened.
		if self.revoked.load(Ordering::Acquire) {
			return;
		}
		self.pending.store(true, Ordering::Release);
		sched::wake_object(self.header.koid());
	}

	// Clear the pending flag, re-arming for the next IRQ.
	pub fn clear(&self) {
		self.pending.store(false, Ordering::Release);
	}

	// Whether the IRQ has fired and not yet been cleared (the wait readiness).
	//
	// A REVOKED INTERRUPT IS NEVER PENDING, so a wait that resolved this object before the release
	// and is testing readiness afterwards does not act on authority that has ended.
	pub fn is_pending(&self) -> bool {
		!self.revoked.load(Ordering::Acquire) && self.pending.load(Ordering::Acquire)
	}

	// Whether this interrupt's authority has been revoked with its claim.
	//
	// THE CFG IS ITS CALLER'S. The production paths do not ask - `signal` and `is_pending` consult
	// the flag themselves, which is where it has to be enforced - so the only reader is the test
	// that proves a forced release takes a live vector away. This tree denies dead code rather than
	// suppressing the warning, so the cfg names the build where a caller exists.
	#[cfg(test)]
	pub fn is_revoked(&self) -> bool {
		self.revoked.load(Ordering::Acquire)
	}

	// GIVE UP OWNERSHIP OF THE SLOT WITHOUT TOUCHING IT, for a caller that is about to free the slot
	// itself.
	//
	// `sys_device_msix_acquire` binds the vector and can then still fail - the derived-capability
	// registry may refuse the registration - and its rollback calls `release_unused_msi`, which frees
	// the registry slot outright. That left this object still believing it owned the binding, so its
	// `Drop` called the architectural `unbind` afterwards: mask the entry, unmap the table page and
	// RETIRE the slot. By then another core may have acquired the freed slot and bound its own
	// interrupt, and the stale rollback tore down the replacement's binding instead of its own.
	//
	// `swap`, so the disarm happens exactly once and a caller cannot disarm a slot twice. Call it
	// BEFORE making the slot reusable: the order is what closes the window rather than narrowing it.
	//
	// AND IT ANSWERS WHETHER THERE WAS ANYTHING TO DISARM, which is what a rollback has to know
	// (2026-09-01). The comment above already said `swap`; the code was a `store`, so the previous
	// value - the one fact that says whether this object still owned the binding - was thrown away,
	// and every caller went on to free the slot unconditionally. That is the same cross-generation
	// teardown this method exists to prevent, reached from the other side: a forced release
	// `revoke`s the interrupt (which swaps `bound` to false and unbinds), publishes the claim
	// `Free`, and `release_msi_for_device` returns the slot to circulation; a replacement claim
	// takes it, binds its own interrupt and programs the entry; and only then does the old syscall
	// resume, disarm nothing, and free a slot that has belonged to somebody else since.
	//
	// So the answer is the caller's authority to free: true means this object still held the
	// binding and nothing else can have taken the slot, false means the release already took it and
	// there is nothing here to give back.
	#[must_use]
	pub fn disown(&self) -> bool {
		self.bound.swap(false, Ordering::AcqRel)
	}

	// TAKE THE VECTOR AWAY NOW, rather than when the last reference happens to go.
	//
	// Unbinding lived in `Drop`, which is the wrong moment for a FORCED release: the holder is still
	// running, and a wait in progress or any transient `Arc` defers that drop for as long as it
	// likes. So a released device kept a live, bound, deliverable vector - and the next claimant
	// could not have it either, because the slot was still owned.
	//
	// Returns whether the architecture CONFIRMED the teardown. On riscv64 a hart that does not
	// answer leaves the slot armed and quarantined, and the claim's terminal state has to say so
	// rather than reading `Free` from the IOMMU alone.
	pub fn revoke(&self) -> bool {
		self.revoked.store(true, Ordering::Release);
		self.pending.store(false, Ordering::Release);
		// `swap`, so the unbind happens exactly once however many callers race here - and so `Drop`
		// does not repeat it against a slot another device may by then own.
		if self.bound.swap(false, Ordering::AcqRel) { crate::arch::interrupts::unbind(self.vector) } else { true }
	}
}

impl_kernel_object!(Interrupt, Interrupt);

impl Drop for Interrupt {
	fn drop(&mut self) {
		// The driver let go of this interrupt (closed the handle, or died): stop
		// delivering its vector. Only the binding's owner unbinds, so a refused bind's
		// Interrupt does not clear the live binding.
		// `swap` for the same reason `revoke` uses one: a forced release may already have unbound
		// this vector, and repeating it would tear down whatever owns the slot by now.
		if self.bound.swap(false, Ordering::AcqRel) {
			crate::arch::interrupts::unbind(self.vector);
		}
	}
}
