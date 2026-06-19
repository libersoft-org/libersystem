// Event: a simple signalable latch.
//
// An Event is the minimal "wait without polling" primitive: it carries a boolean
// signaled state that one party raises and another observes. Until the scheduler
// can block a thread on an object, callers observe the state with is_signaled()
// (cooperatively yielding between checks); a true blocking wait is layered on top
// later without changing this object.

#![allow(dead_code)]

use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicBool, Ordering};

use super::{KernelObject, ObjectHeader, ObjectType};
use crate::sched;

pub struct Event {
	header: ObjectHeader,
	signaled: AtomicBool,
}

impl Event {
	pub fn create() -> Arc<Self> {
		Arc::new(Self { header: ObjectHeader::new(), signaled: AtomicBool::new(false) })
	}

	// Raise the signal.
	pub fn signal(&self) {
		self.signaled.store(true, Ordering::Release);
		// Wake any thread blocked waiting on this event.
		sched::wake_object(self.header.koid());
	}

	// Lower the signal.
	pub fn clear(&self) {
		self.signaled.store(false, Ordering::Release);
	}

	pub fn is_signaled(&self) -> bool {
		self.signaled.load(Ordering::Acquire)
	}
}

impl KernelObject for Event {
	fn header(&self) -> &ObjectHeader {
		&self.header
	}

	fn object_type(&self) -> ObjectType {
		ObjectType::Event
	}

	fn as_any(&self) -> &dyn Any {
		self
	}

	fn into_any_arc(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
		self
	}
}
