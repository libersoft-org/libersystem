// Timer: a one-shot deadline against the monotonic tick counter.
//
// The timer is armed with an absolute deadline (in LAPIC ticks) and reports
// is_expired() once the counter reaches it. It is exposed as a pollable object;
// a blocking wait that sleeps the caller until expiry is layered on top once the
// scheduler can block a thread, without changing this object.

#![allow(dead_code)]

use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::arch;

pub struct Timer {
	header: ObjectHeader,
	armed: AtomicBool,
	deadline: AtomicU64,
}

impl Timer {
	pub fn create() -> Arc<Self> {
		Arc::new(Self { header: ObjectHeader::new(), armed: AtomicBool::new(false), deadline: AtomicU64::new(0) })
	}

	// Arm the timer to fire when the tick counter reaches `deadline_ticks`.
	pub fn set(&self, deadline_ticks: u64) {
		self.deadline.store(deadline_ticks, Ordering::Release);
		self.armed.store(true, Ordering::Release);
	}

	// Disarm the timer.
	pub fn cancel(&self) {
		self.armed.store(false, Ordering::Release);
	}

	// True if the timer is armed and its deadline has been reached.
	pub fn is_expired(&self) -> bool {
		self.armed.load(Ordering::Acquire) && arch::apic::ticks() >= self.deadline.load(Ordering::Acquire)
	}

	// The armed deadline in ticks, or None if the timer is not armed. Used by `wait`
	// to wake the caller when the timer fires.
	pub fn deadline(&self) -> Option<u64> {
		if self.armed.load(Ordering::Acquire) { Some(self.deadline.load(Ordering::Acquire)) } else { None }
	}
}

impl_kernel_object!(Timer, Timer);
