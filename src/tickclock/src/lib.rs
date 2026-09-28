//! THE MONOTONIC TICK, COMPUTED WHEN IT IS READ.
//!
//! The scheduler's tick is the ABI's unit of time - `SYS_CLOCK_GET` answers it and every deadline a
//! syscall takes is an absolute tick - and it used to be ADVANCED: whichever core's periodic timer
//! interrupt first saw a whole period of the free-running counter elapsed moved it on. That ties the
//! clock to an interrupt every core takes a hundred times a second whether it has anything to do or not,
//! and an idle core that stops taking it would stop the clock with it.
//!
//! So the tick is computed from the counter itself - the TSC, CNTVCT, the `time` CSR - each time it is
//! read: the counter less the SLEEP OFFSET, less the anchor taken once the counter's frequency is known,
//! divided by the cycles in one tick. Nothing has to happen for time to pass.
//!
//! NON-DECREASING ACROSS CORES, by one atomic maximum over every tick answered: a core whose counter reads
//! a little behind another's answers the larger value rather than stepping time back.
//!
//! ONE SLEEP-OFFSET TERM is the whole of this crate's share of a sleep. It is subtracted from every reading
//! and is zero until the first sleep. Between the counter reading a sleep's entry takes and the REBASE, the
//! clock is SUSPENDED: every reading - of the tick and of the nanoseconds, on every core - answers the value
//! at suspend, so no reading counts the sleep, none reads a counter that restarted, and none raises the
//! maximum past the suspend value. The rebase makes the offset the resume reading less the monotonic value
//! at suspend, which is the same arithmetic for a counter that kept running (a suspend to idle) and one
//! that restarted (S3): wrapping, so a restarted counter's small readings land where the suspended clock
//! left off. What a sleep MEANS for the clocks - the monotonic clock excludes it, a boot-time clock
//! includes it - is the sleep's owner's; it calls `suspend` and `rebase`.

#![cfg_attr(not(test), no_std)]

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The tick rate: the ABI's `TICKS_PER_SECOND`.
pub const TICK_HZ: u64 = 100;

/// One computed clock. `const`-constructible, so a kernel keeps one in a static.
pub struct Clock {
	// The counter reading tick zero is measured from, published before `per_tick`.
	anchor: AtomicU64,
	// Counter cycles in one tick; zero until the clock is anchored.
	per_tick: AtomicU64,
	// The counter's frequency, for nanoseconds.
	hz: AtomicU64,
	// THE SLEEP OFFSET, in counter cycles, subtracted (wrapping) from every reading.
	offset: AtomicU64,
	// The largest tick answered.
	max: AtomicU64,
	// SUSPENDED: every reading answers the one at `suspended_at` until the rebase.
	suspended: AtomicBool,
	suspended_at: AtomicU64,
}

impl Default for Clock {
	fn default() -> Clock {
		Clock::new()
	}
}

impl Clock {
	pub const fn new() -> Clock {
		Clock { anchor: AtomicU64::new(0), per_tick: AtomicU64::new(0), hz: AtomicU64::new(0), offset: AtomicU64::new(0), max: AtomicU64::new(0), suspended: AtomicBool::new(false), suspended_at: AtomicU64::new(0) }
	}

	/// ANCHOR the clock at counter reading `now`, the counter running at `hz`: tick zero is `now`. Once:
	/// false when it is already anchored, or when `hz` is too low to count one tick.
	pub fn anchor(&self, now: u64, hz: u64) -> bool {
		let per = hz / TICK_HZ;
		if per == 0 || self.per_tick.load(Ordering::Acquire) != 0 {
			return false;
		}
		self.anchor.store(now, Ordering::Relaxed);
		self.hz.store(hz, Ordering::Relaxed);
		// THE PERIOD IS PUBLISHED LAST, so a reader that sees it sees the anchor and the frequency too.
		self.per_tick.compare_exchange(0, per, Ordering::AcqRel, Ordering::Acquire).is_ok()
	}

	pub fn anchored(&self) -> bool {
		self.per_tick.load(Ordering::Acquire) != 0
	}

	/// Counter cycles in one tick, zero before the clock is anchored.
	pub fn cycles_per_tick(&self) -> u64 {
		self.per_tick.load(Ordering::Acquire)
	}

	/// The counter's frequency, zero before the clock is anchored.
	pub fn hz(&self) -> u64 {
		self.hz.load(Ordering::Relaxed)
	}

	pub fn suspended(&self) -> bool {
		self.suspended.load(Ordering::Acquire)
	}

	// The counter reading as the clock sees it: during a suspension, the one the suspend took.
	fn reading(&self, now: u64) -> u64 {
		if self.suspended.load(Ordering::Acquire) { self.suspended_at.load(Ordering::Relaxed) } else { now }
	}

	/// Cycles since tick zero at counter reading `now`, every sleep excluded. A reading behind the anchor -
	/// a core whose counter is a little behind the one that anchored - is zero, never a wrap to the far end.
	pub fn elapsed(&self, now: u64) -> u64 {
		let elapsed = self.reading(now).wrapping_sub(self.offset.load(Ordering::Acquire)).wrapping_sub(self.anchor.load(Ordering::Relaxed)) as i64;
		elapsed.max(0) as u64
	}

	/// THE TICK at counter reading `now`, never below one this clock has already answered. Zero before the
	/// clock is anchored.
	pub fn ticks(&self, now: u64) -> u64 {
		let per = self.per_tick.load(Ordering::Acquire);
		if per == 0 {
			return 0;
		}
		let raw = self.elapsed(now) / per;
		self.max.fetch_max(raw, Ordering::AcqRel).max(raw)
	}

	/// NANOSECONDS on the monotonic clock: the counter less the sleep offset, as `SYS_CLOCK_MONO_NS` answers
	/// it. Zero before the clock is anchored.
	pub fn nanos(&self, now: u64) -> u64 {
		let hz = self.hz.load(Ordering::Relaxed);
		if self.per_tick.load(Ordering::Acquire) == 0 {
			return 0;
		}
		cycles_to_ns(self.reading(now).wrapping_sub(self.offset.load(Ordering::Acquire)), hz)
	}

	/// The counter reading at which tick `tick` begins - what a compare register is programmed with for a
	/// tick deadline, through the offset. `None` before the clock is anchored, during a suspension, and for
	/// a tick too far away to express in the counter.
	pub fn counter_at(&self, tick: u64) -> Option<u64> {
		let per = self.per_tick.load(Ordering::Acquire);
		if per == 0 || self.suspended.load(Ordering::Acquire) {
			return None;
		}
		let cycles = tick.checked_mul(per)?;
		if cycles > i64::MAX as u64 {
			return None;
		}
		Some(self.anchor.load(Ordering::Relaxed).wrapping_add(self.offset.load(Ordering::Acquire)).wrapping_add(cycles))
	}

	/// ENTER THE SUSPENDED STATE at counter reading `counter` - the reading the sleep's entry takes. Until
	/// `rebase`, every reading answers the monotonic value at `counter`.
	pub fn suspend(&self, counter: u64) {
		self.suspended_at.store(counter, Ordering::Relaxed);
		self.suspended.store(true, Ordering::Release);
	}

	/// THE REBASE, leaving the suspended state: `suspend_counter` is the reading `suspend` took and
	/// `resume_counter` the counter now - later, on a counter that kept running; anything, on one that
	/// restarted. The offset becomes the resume reading less the monotonic value at suspend, and the maximum
	/// is set from that value, in one operation.
	pub fn rebase(&self, suspend_counter: u64, resume_counter: u64) {
		let old = self.offset.load(Ordering::Acquire);
		let per = self.per_tick.load(Ordering::Acquire);
		// The monotonic value at suspend, on the offset the suspended clock was answering with.
		let at_suspend = suspend_counter.wrapping_sub(old).wrapping_sub(self.anchor.load(Ordering::Relaxed)) as i64;
		let at_suspend = at_suspend.max(0) as u64;
		self.offset.store(resume_counter.wrapping_sub(suspend_counter).wrapping_add(old), Ordering::Release);
		if per != 0 {
			self.max.fetch_max(at_suspend / per, Ordering::AcqRel);
		}
		self.suspended.store(false, Ordering::Release);
	}
}

/// `cycles` at `hz` in nanoseconds; zero for a clock with no frequency.
pub fn cycles_to_ns(cycles: u64, hz: u64) -> u64 {
	if hz == 0 {
		return 0;
	}
	(cycles as u128 * 1_000_000_000 / hz as u128) as u64
}

#[cfg(test)]
mod tests;
