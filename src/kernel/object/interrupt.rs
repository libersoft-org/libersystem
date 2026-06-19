// Interrupt kernel object.
//
// An Interrupt is a capability to a device IRQ, bound to a vector. When the IRQ
// fires the kernel marks the Interrupt pending and wakes any thread blocked on it
// (via wait), so a userspace driver sleeps until its device needs attention rather
// than polling. The interrupt-dispatch table holds the binding weakly, so closing
// the handle (or the driver dying) drops the Interrupt, which unbinds its vector -
// the kernel stops delivering to a driver that is gone.

#![allow(dead_code)]

use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicBool, Ordering};

use super::{KernelObject, ObjectHeader, ObjectType};
use crate::sched;

pub struct Interrupt {
	header: ObjectHeader,
	vector: u8,
	// Set when the IRQ has fired and not yet been cleared; the wait readiness.
	pending: AtomicBool,
	// Set once this Interrupt actually owns its vector's binding, so only the owner
	// unbinds on drop (a refused bind's Interrupt leaves the live binding alone).
	bound: AtomicBool,
}

impl Interrupt {
	pub fn new(vector: u8) -> Arc<Self> {
		Arc::new(Self { header: ObjectHeader::new(), vector, pending: AtomicBool::new(false), bound: AtomicBool::new(false) })
	}

	pub fn vector(&self) -> u8 {
		self.vector
	}

	// Mark this Interrupt as the owner of its vector binding (called by bind()).
	pub fn mark_bound(&self) {
		self.bound.store(true, Ordering::Release);
	}

	// Mark the interrupt pending and wake any thread blocked waiting on it. Called
	// from the interrupt-dispatch path when the bound vector fires.
	pub fn signal(&self) {
		self.pending.store(true, Ordering::Release);
		sched::wake_object(self.header.koid());
	}

	// Clear the pending flag, re-arming for the next IRQ.
	pub fn clear(&self) {
		self.pending.store(false, Ordering::Release);
	}

	// Whether the IRQ has fired and not yet been cleared (the wait readiness).
	pub fn is_pending(&self) -> bool {
		self.pending.load(Ordering::Acquire)
	}
}

impl KernelObject for Interrupt {
	fn header(&self) -> &ObjectHeader {
		&self.header
	}

	fn object_type(&self) -> ObjectType {
		ObjectType::Interrupt
	}

	fn as_any(&self) -> &dyn Any {
		self
	}
}

impl Drop for Interrupt {
	fn drop(&mut self) {
		// The driver let go of this interrupt (closed the handle, or died): stop
		// delivering its vector. Only the binding's owner unbinds, so a refused bind's
		// Interrupt does not clear the live binding.
		if self.bound.load(Ordering::Acquire) {
			crate::arch::interrupts::unbind(self.vector);
		}
	}
}
