// Event: a simple signalable latch.
//
// An Event is the minimal "wait without polling" primitive: it carries a boolean
// signaled state that one party raises and another observes. Until the scheduler
// can block a thread on an object, callers observe the state with is_signaled()
// (cooperatively yielding between checks); a true blocking wait is layered on top
// later without changing this object.

use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicBool, Ordering};

use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::sched;

pub struct Event {
	header: ObjectHeader,
	signaled: AtomicBool,
}

impl Event {
	// FALLIBLY: `SYS_EVENT_CREATE` reaches this, so a short heap must be a refusal and not a halt.
	pub fn create() -> Option<Arc<Self>> {
		crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), signaled: AtomicBool::new(false) })
	}

	// Raise the signal.
	pub fn signal(&self) {
		self.signaled.store(true, Ordering::Release);
		// Wake any thread blocked waiting on this event.
		sched::wake_object(self.header.koid());
	}

	// Lower the signal.
	#[cfg(test)]
	pub fn clear(&self) {
		self.signaled.store(false, Ordering::Release);
	}

	pub fn is_signaled(&self) -> bool {
		self.signaled.load(Ordering::Acquire)
	}
}

impl_kernel_object!(Event, Event);

#[cfg(test)]
mod tests;
