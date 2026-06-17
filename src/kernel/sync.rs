// Synchronization primitives.
//
// SpinLock is the kernel's first lock and is written SMP-correct from the start:
// a test-and-test-and-set acquire with proper acquire/release memory ordering so
// data published under the lock is visible to the next holder on another core.
//
// Note: this lock is NOT interrupt-safe yet. Once we enable interrupts (M2), any
// lock that can also be taken from an interrupt handler must additionally disable
// interrupts while held, or a handler interrupting a holder on the same core will
// deadlock. Locks introduced in M1 are only taken with interrupts disabled.
#![allow(dead_code)]
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

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
		while self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
			// Spin read-only (cheap, cache-friendly) until the lock looks free,
			// then retry the atomic acquire above.
			while self.locked.load(Ordering::Relaxed) {
				core::hint::spin_loop();
			}
		}
		SpinLockGuard { lock: self }
	}

	pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
		if self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
			Some(SpinLockGuard { lock: self })
		} else {
			None
		}
	}
}

pub struct SpinLockGuard<'a, T> {
	lock: &'a SpinLock<T>,
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
		self.lock.locked.store(false, Ordering::Release);
	}
}
