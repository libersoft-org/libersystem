// Synchronization primitives.
//
// SpinLock is the kernel's first lock and is written SMP-correct from the start:
// a test-and-test-and-set acquire with proper acquire/release memory ordering so
// data published under the lock is visible to the next holder on another core.
//
// It is also interrupt-safe (since M19, preemption): `lock` disables interrupts on
// the current core before acquiring and the guard restores the prior state on
// drop. A lock holder therefore can never be preempted by the timer, so an
// interrupt handler that needs the same lock can never deadlock against a holder
// it interrupted. Nested locks restore correctly (only the outermost re-enables).
#![allow(dead_code)]
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch;

pub struct SpinLock<T> {
	locked: AtomicBool,
	data: UnsafeCell<T>,
}

// Safe to share across cores: access to the inner data is serialized by the lock.
unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
	pub const fn new(value: T) -> Self {
		Self { locked: AtomicBool::new(false), data: UnsafeCell::new(value) }
	}

	pub fn lock(&self) -> SpinLockGuard<'_, T> {
		// Disable interrupts BEFORE acquiring, so a holder can never be preempted on
		// this core (which would deadlock an interrupt handler needing the same
		// lock). The prior interrupt state is restored when the guard drops.
		let was_enabled = arch::interrupts_enabled();
		arch::disable_interrupts();
		while self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
			// Spin read-only (cheap, cache-friendly) until the lock looks free,
			// then retry the atomic acquire above.
			while self.locked.load(Ordering::Relaxed) {
				core::hint::spin_loop();
			}
		}
		SpinLockGuard { lock: self, was_enabled }
	}

	pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
		let was_enabled = arch::interrupts_enabled();
		arch::disable_interrupts();
		if self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
			Some(SpinLockGuard { lock: self, was_enabled })
		} else {
			// Acquisition failed: restore the interrupt state we just disabled.
			if was_enabled {
				arch::enable_interrupts();
			}
			None
		}
	}
}

pub struct SpinLockGuard<'a, T> {
	lock: &'a SpinLock<T>,
	// Whether interrupts were enabled when this lock was taken; restored on drop.
	was_enabled: bool,
}

impl<T> Deref for SpinLockGuard<'_, T> {
	type Target = T;
	fn deref(&self) -> &T {
		unsafe { &*self.lock.data.get() }
	}
}

impl<T> DerefMut for SpinLockGuard<'_, T> {
	fn deref_mut(&mut self) -> &mut T {
		unsafe { &mut *self.lock.data.get() }
	}
}

impl<T> Drop for SpinLockGuard<'_, T> {
	fn drop(&mut self) {
		// Release the lock first, then restore interrupts: an interrupt handler that
		// fires the instant interrupts come back must see the lock already free.
		self.lock.locked.store(false, Ordering::Release);
		if self.was_enabled {
			arch::enable_interrupts();
		}
	}
}
