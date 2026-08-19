// Portable timekeeping policy + arithmetic shared by every arch backend.
//
// The periodic scheduler-tick RATE is a policy the whole kernel shares; only how each
// backend programs its timer to fire at that rate is arch-specific (the LAPIC on x86,
// CNTP on aarch64, the CLINT on riscv64). Likewise the cycles->ns conversion each
// backend's fine cycle clock reports latency through is pure arithmetic - the only
// arch-specific part is where the frequency comes from (a calibrated TSC, CNTFRQ_EL0,
// the device tree's timebase-frequency).

// The periodic scheduler-tick rate, in Hz. Each backend programs its timer to fire at
// this rate; the portable scheduler counts these ticks as its monotonic coarse clock.
pub const TICK_HZ: u32 = 100;

// Convert a raw cycle count to nanoseconds at frequency `hz` (cycles per second). The
// u128 intermediate keeps `cycles * 1e9` from overflowing; an uncalibrated clock
// (hz == 0) reports 0 rather than dividing by zero.
pub fn cycles_to_ns(cycles: u64, hz: u64) -> u64 {
	if hz == 0 {
		return 0;
	}
	(cycles as u128 * 1_000_000_000 / hz as u128) as u64
}

// THE GLOBAL TICK COUNTER, ADVANCED AT `TICK_HZ` WHATEVER THE CORE COUNT (KERN-ARCH-007).
//
// The scheduler tick is a PER-CORE timer on every architecture here - the LAPIC on x86, CNTP on
// aarch64, the CLINT on riscv64 - and each core takes its own interrupt. aarch64 and riscv64 both
// answered that with `TICKS.fetch_add(1)` in the handler, so the monotonic clock advanced once per
// core per period: on an eight-core machine time ran eight times fast, and every deadline in the
// system - a sleep, a timeout, the caret blink - was wrong by the core count.
//
// x86_64 had already solved it and its comment records what does NOT work: counting only the boot
// core's tick froze the clock whenever that core stalled, and hung every tick-based wait with it.
// So the rate is gated on the machine's own cycle counter instead. Any core may drive it; a tick is
// only claimed once a full period has elapsed since the last one, and the core that wins the
// compare-exchange advances the clock by the WHOLE backlog in one step rather than one tick per
// interrupt - which would replay a stall at the interrupt rate times the core count.
//
// This is that mechanism, in the portable file, because three backends needed the same one.
pub struct TickClock {
	ticks: core::sync::atomic::AtomicU64,
	// The cycle count the last tick was claimed at, and the period in cycles. A period of 0 means
	// the clock has not been anchored yet.
	anchor: core::sync::atomic::AtomicU64,
	cycles_per_tick: core::sync::atomic::AtomicU64,
}

impl TickClock {
	pub const fn new() -> TickClock {
		TickClock { ticks: core::sync::atomic::AtomicU64::new(0), anchor: core::sync::atomic::AtomicU64::new(0), cycles_per_tick: core::sync::atomic::AtomicU64::new(0) }
	}

	pub fn ticks(&self) -> u64 {
		self.ticks.load(core::sync::atomic::Ordering::Relaxed)
	}

	// Called from every core's timer interrupt. `now` is the machine's shared cycle counter and
	// `hz` its frequency; a frequency of 0 is a clock that cannot be gated yet and is answered by
	// counting nothing rather than by guessing a period.
	pub fn advance(&self, now: u64, hz: u64) {
		use core::sync::atomic::Ordering;
		let mut period = self.cycles_per_tick.load(Ordering::Acquire);
		if period == 0 {
			if hz == 0 {
				return;
			}
			let per = hz / TICK_HZ as u64;
			if per == 0 {
				return;
			}
			// ANCHOR BEFORE PUBLISHING THE PERIOD, so a core that observes a non-zero period also
			// observes the anchor and never measures an interval against zero. The exchange is what
			// makes two cores arriving together seed once.
			self.anchor.store(now, Ordering::Relaxed);
			if self.cycles_per_tick.compare_exchange(0, per, Ordering::AcqRel, Ordering::Acquire).is_err() {
				return;
			}
			// The tick that did the seeding still counts: time starts here.
			self.ticks.fetch_add(1, Ordering::Relaxed);
			period = per;
			let _ = period;
			return;
		}
		let anchor = self.anchor.load(Ordering::Relaxed);
		// SIGNED distance: a racing core may move the anchor past this core's reading, and the
		// unsigned difference would wrap to an enormous backlog. "The anchor is ahead" means
		// nothing is due.
		let elapsed = now.wrapping_sub(anchor) as i64;
		if elapsed < period as i64 {
			return;
		}
		let periods = elapsed as u64 / period;
		if self.anchor.compare_exchange(anchor, anchor.wrapping_add(periods * period), Ordering::AcqRel, Ordering::Relaxed).is_ok() {
			self.ticks.fetch_add(periods, Ordering::Relaxed);
		}
	}

	// The harness reads the clock through the arch `ticks()`, which adds a test skew; this is the
	// raw counter, for a test that wants to measure the RATE.
	#[cfg(test)]
	pub fn raw_for_test(&self) -> u64 {
		self.ticks()
	}
}
