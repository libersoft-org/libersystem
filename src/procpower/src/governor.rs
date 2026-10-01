//! THE IDLE GOVERNOR: which of a core's idle states its next idle period enters.
//!
//! THE PREDICTION is the shorter of two: the time to the core's own next one-shot - the one wake the core KNOWS of -
//! and what its recent idle periods have lasted, an average of the last few weighted to the newest, because most wakes
//! are interrupts nobody scheduled. A state pays only when the core stays in it for its target residency, so the
//! governor picks the DEEPEST state whose target residency fits the prediction and whose exit latency fits the bound
//! the live latency requests set - and never one this core cannot enter. The first state - the halt, which needs no
//! table - is always there to fall back on.

/// How much a new idle period moves the average: it weighs one part in this many.
const WEIGHT: u64 = 8;

/// A CORE'S RECENT IDLE PERIODS, as one weighted average in microseconds.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Predictor {
	average_us: u64,
	seen: bool,
}

impl Predictor {
	pub const fn new() -> Predictor {
		Predictor { average_us: 0, seen: false }
	}

	/// An idle period that lasted `us`.
	pub fn observe(&mut self, us: u64) {
		if !self.seen {
			self.average_us = us;
			self.seen = true;
			return;
		}
		self.average_us = (self.average_us * (WEIGHT - 1) + us) / WEIGHT;
	}

	/// What the next period is predicted to last, given the time to the core's own next timed wake (None: none
	/// armed): the shorter of the two; the history alone where nothing is armed; the timer alone before any history.
	pub fn predict(&self, until_timer_us: Option<u64>) -> u64 {
		match (until_timer_us, self.seen) {
			(Some(timer), true) => timer.min(self.average_us),
			(Some(timer), false) => timer,
			(None, true) => self.average_us,
			(None, false) => 0,
		}
	}
}

/// One state as the governor weighs it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Candidate {
	pub exit_latency_us: u32,
	pub target_residency_us: u32,
	/// This core may enter it (`crate::enterable`).
	pub enterable: bool,
}

/// THE STATE TO ENTER: the deepest one this core may enter whose target residency fits `predicted_us` and whose exit
/// latency fits `latency_bound_us` (None: no live request). The first state answers when nothing deeper fits, and
/// whatever it is - it is the halt.
pub fn choose(states: &[Candidate], predicted_us: u64, latency_bound_us: Option<u32>) -> usize {
	let mut chosen = 0;
	for (index, state) in states.iter().enumerate().skip(1) {
		if !state.enterable {
			continue;
		}
		if u64::from(state.target_residency_us) > predicted_us {
			continue;
		}
		if latency_bound_us.is_some_and(|bound| state.exit_latency_us > bound) {
			continue;
		}
		chosen = index;
	}
	chosen
}

#[cfg(test)]
mod tests;
