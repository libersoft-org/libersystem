// Synchronization primitives.
//
// SpinLock is the kernel's first lock and is written SMP-correct from the start:
// a test-and-test-and-set acquire with proper acquire/release memory ordering so
// data published under the lock is visible to the next holder on another core.
//
// It is also interrupt-safe (preemption can fire mid-section): `lock` disables interrupts on
// the current core before acquiring and the guard restores the prior state on
// drop. A lock holder therefore can never be preempted by the timer, so an
// interrupt handler that needs the same lock can never deadlock against a holder
// it interrupted. Nested locks restore correctly (only the outermost re-enables).
#![allow(dead_code)]
use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch;

// How many read-only spins pass between TLB-shootdown services in a contended acquire. Far enough
// out that no ordinary acquire pays for it, close enough that a core which would otherwise deadlock
// answers within microseconds rather than never. See the comment at the spin itself.
const SERVICE_EVERY: u32 = 1024;

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
		let mut spins: u32 = 0;
		while self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
			// Spin read-only (cheap, cache-friendly) until the lock looks free,
			// then retry the atomic acquire above.
			while self.locked.load(Ordering::Relaxed) {
				core::hint::spin_loop();
				// AND ANSWER TLB SHOOTDOWNS WHILE WAITING, because this is the deadlock
				// `mem::tlb::shootdown` describes and could not fix from its own side. Interrupts
				// are masked two lines above, so a core spinning here answers no IPI - and its
				// comment names exactly this case: "a core that masks interrupts for a lock, or is
				// already inside this function, does not". Shootdown gained a `service_pending` in
				// its own wait; this is the other half.
				//
				// The deadlock it closes: core A holds some lock and allocates, the allocation grows
				// the heap, the mapper shoots down, and it waits for core B - which is here, waiting
				// for A's lock with interrupts off, and will not acknowledge until A finishes.
				// Neither moves. `a_process_load_whose_image_goes_away...` hangs on eight cores and
				// passes on one, which is the signature of exactly this and of nothing else that has
				// survived measurement.
				//
				// Only after real contention, and only every so often: `lock` is among the hottest
				// paths in the kernel, an uncontended acquire never reaches this line, and a brief
				// wait does not either. `service_pending` is lock-free - atomics and a local flush -
				// so it cannot recurse into another acquire.
				spins = spins.wrapping_add(1);
				if spins % SERVICE_EVERY == 0 {
					crate::mem::tlb::service_pending();
				}
			}
		}
		SpinLockGuard { lock: self, was_enabled, _not_send: PhantomData }
	}

	// Whether the lock LOOKED taken a moment ago. Only useful for spinning read-only before
	// attempting an acquire: the answer is stale the instant it is returned, so nothing may
	// conclude it holds the lock (or that it would get it) from a `false`.
	pub fn is_locked(&self) -> bool {
		self.locked.load(Ordering::Relaxed)
	}

	pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
		let was_enabled = arch::interrupts_enabled();
		arch::disable_interrupts();
		if self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
			Some(SpinLockGuard { lock: self, was_enabled, _not_send: PhantomData })
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
	// Pins the guard to the CPU that took the lock.
	//
	// It held only a reference and a bool, which made it automatically `Send` whenever the
	// lock is `Sync` - and its `Drop` restores the interrupt state of whichever CPU runs
	// the drop. Handing a guard to another core therefore re-enables interrupts on the
	// wrong one and leaves the original with them off, permanently, with nothing to point
	// at afterwards. A raw pointer is the standard way to say "this value does not cross
	// cores"; nothing is ever read through it.
	_not_send: PhantomData<*const ()>,
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
