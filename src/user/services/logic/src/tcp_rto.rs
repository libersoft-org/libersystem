//! The retransmission timer: how long to wait before deciding a segment was lost.
//!
//! RFC 6298, AND THE ONE PLACE THIS PROFILE DEPARTS FROM IT IS WRITTEN DOWN. A retransmission timer
//! is the difference between a sender and a retransmit loop: a fixed timeout with no backoff
//! collapses under a changed path and congests one it was already struggling on. The estimator here
//! is the standard's - a smoothed round-trip time, a smoothed variance, and a timeout four variances
//! above the mean - with three numbers this profile fixes:
//!
//!   INITIAL     1 second, until a measurement exists. RFC 6298 section 2.1.
//!   FLOOR       200 ms, WHICH IS A DECLARED DEVIATION. Section 2, rule 2.4 says a computed RTO
//!                 below one second SHOULD be rounded up to one second, and its own note says that
//!                 minimum is a conservative choice for coarse-grained clocks which a finer clock
//!                 may justify shortening. This is a bounded appliance on an emulated link with a
//!                 millisecond clock, where a one-second floor turns every ordinary loss into a
//!                 one-second stall. The deviation is deliberate and is stated here rather than
//!                 being left to be discovered in the arithmetic.
//!   CEILING     60 seconds. Backoff doubles into it and stops.
//!
//! KARN'S RULE IS THE CALLER'S TO OBSERVE AND THIS TYPE MAKES IT HARD TO MISS: `sample` takes the
//! measurement only for a segment that was NEVER retransmitted, because a round-trip time measured
//! against a retransmission cannot say which transmission the acknowledgement answered - and the two
//! readings differ by exactly the amount that would make the estimator wrong in the direction of
//! being too fast.

/// The RTO before any round-trip time has been measured.
pub const INITIAL_RTO_MS: u32 = 1000;

/// The smallest retransmission timeout this profile will use. See the module note: a deviation.
pub const MIN_RTO_MS: u32 = 200;

/// The largest. Backoff doubles into this and stops.
pub const MAX_RTO_MS: u32 = 60_000;

/// The clock granularity, `G` in RFC 6298's arithmetic.
pub const CLOCK_GRANULARITY_MS: u32 = 1;

/// How many times one segment of an ESTABLISHED connection is retransmitted before the connection is
/// closed with a typed error. With the backoff and ceiling above this is about ten minutes, which is
/// the order RFC 1122's R2 asks for.
pub const MAX_DATA_RETRANSMISSIONS: u8 = 8;

/// The least time a SYN schedule must span before an open may fail, from RFC 9293 section 3.8.3.
pub const MIN_SYN_SCHEDULE_MS: u32 = 180_000;

/// The round-trip estimator for one connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rto {
	/// The smoothed round-trip time, scaled by 8 so the 1/8 weight is integer arithmetic.
	srtt_scaled: u32,
	/// The smoothed variance, scaled by 4 for the same reason. `4 * RTTVAR` is exactly this value,
	/// which is what the timeout formula wants.
	rttvar_scaled: u32,
	measured: bool,
	rto_ms: u32,
	/// How many times the current segment has been retransmitted, which is what backs off and what
	/// the retry limit counts.
	backoffs: u8,
}

impl Default for Rto {
	fn default() -> Rto {
		Rto::new()
	}
}

impl Rto {
	pub fn new() -> Rto {
		Rto { srtt_scaled: 0, rttvar_scaled: 0, measured: false, rto_ms: INITIAL_RTO_MS, backoffs: 0 }
	}

	/// The timeout to arm now.
	pub fn rto_ms(&self) -> u32 {
		self.rto_ms
	}

	/// Has a round-trip time been measured on this connection yet?
	pub fn measured(&self) -> bool {
		self.measured
	}

	/// The smoothed round-trip time in milliseconds, or `None` before the first measurement.
	pub fn srtt_ms(&self) -> Option<u32> {
		self.measured.then_some(self.srtt_scaled / 8)
	}

	/// The smoothed variance in milliseconds.
	pub fn rttvar_ms(&self) -> Option<u32> {
		self.measured.then_some(self.rttvar_scaled / 4)
	}

	/// How many consecutive retransmissions the current segment has taken.
	pub fn backoffs(&self) -> u8 {
		self.backoffs
	}

	/// Take a round-trip measurement.
	///
	/// KARN'S RULE IS HERE AND NOT IN THE CALLER'S HEAD: a segment that was retransmitted supplies no
	/// measurement, because an acknowledgement covering it cannot say WHICH transmission it answers.
	/// Passing the flag rather than expecting the caller to check it is what keeps the rule from
	/// being the one line somebody forgets.
	pub fn sample(&mut self, rtt_ms: u32, segment_was_retransmitted: bool) -> bool {
		if segment_was_retransmitted {
			return false;
		}
		// A measurement of zero is a measurement: on a loopback or an emulated link the reply can
		// arrive inside one tick, and treating that as "no sample" would leave the estimator on its
		// initial value for the life of a fast connection.
		let rtt: u32 = rtt_ms;
		if !self.measured {
			// RFC 6298 section 2.2: the first measurement seeds both directly.
			self.srtt_scaled = rtt.saturating_mul(8);
			self.rttvar_scaled = rtt.saturating_mul(2);
			self.measured = true;
		} else {
			// Section 2.3, in the scaled integer form: RTTVAR takes a quarter of the error, SRTT an
			// eighth of it, and both are updated with the OLD SRTT - which is why the difference is
			// computed before SRTT moves.
			let srtt: u32 = self.srtt_scaled / 8;
			let difference: u32 = srtt.abs_diff(rtt);
			self.rttvar_scaled = self.rttvar_scaled - self.rttvar_scaled / 4 + difference;
			self.srtt_scaled = self.srtt_scaled - self.srtt_scaled / 8 + rtt;
		}
		// RTO = SRTT + max(G, K*RTTVAR), with K = 4 - and `rttvar_scaled` IS `4 * RTTVAR`.
		let computed: u32 = (self.srtt_scaled / 8).saturating_add(self.rttvar_scaled.max(CLOCK_GRANULARITY_MS));
		self.rto_ms = clamp_rto(computed);
		// A NEW MEASUREMENT RESETS THE BACKOFF. The path answered, so whatever the doubling was
		// compensating for is over; keeping the doubled timeout would leave the connection slow for
		// as long as it stayed idle.
		self.backoffs = 0;
		true
	}

	/// The segment timed out: double the timeout and count the attempt.
	pub fn back_off(&mut self) -> u8 {
		self.rto_ms = clamp_rto(self.rto_ms.saturating_mul(2));
		self.backoffs = self.backoffs.saturating_add(1);
		self.backoffs
	}

	/// A fresh segment is being timed: the backoff count starts again, and the timeout stays where
	/// the estimator put it.
	pub fn restart(&mut self) {
		self.backoffs = 0;
	}

	/// Has this segment been retransmitted more times than an established connection permits?
	pub fn exhausted(&self) -> bool {
		self.backoffs >= MAX_DATA_RETRANSMISSIONS
	}
}

fn clamp_rto(value: u32) -> u32 {
	value.clamp(MIN_RTO_MS, MAX_RTO_MS)
}

/// The interval before the retransmission at `index`, counting from zero, or `None` past the end of
/// the schedule.
///
/// DERIVED FROM THE RTO PARAMETERS RATHER THAN WRITTEN BESIDE THEM. A count written next to an
/// initial RTO and a ceiling is two statements that can drift apart, and they did: an earlier
/// derivation read 1, 2, 4, 8, 16, 32, 64, 128 seconds, which doubles straight past this profile's
/// own 60-second ceiling twice and specified a schedule the ceiling forbids. Under the cap the
/// intervals are 1, 2, 4, 8, 16, 32, 60 - and an implementation that changes the initial RTO or the
/// ceiling gets a new schedule rather than an inherited number.
///
/// COMPUTED RATHER THAN COLLECTED, because this is a `no_std` module a bounded system links whole:
/// a handful of doublings costs nothing and a heap allocation for seven numbers would be a new
/// allocation shape for every consumer of this library to carry.
pub fn syn_interval(index: usize) -> Option<u32> {
	let attempts: usize = syn_attempts();
	if index >= attempts {
		return None;
	}
	let mut interval: u32 = INITIAL_RTO_MS;
	for _ in 0..index {
		interval = clamp_rto(interval.saturating_mul(2));
	}
	Some(interval)
}

/// How many times a SYN is retransmitted before the open fails.
pub fn syn_attempts() -> usize {
	let mut interval: u32 = INITIAL_RTO_MS;
	let mut spanned: u32 = 0;
	let mut attempts: usize = 0;
	loop {
		attempts += 1;
		spanned = spanned.saturating_add(interval);
		// The open fails when the LAST interval has ALSO elapsed with no answer, so the schedule is
		// long enough as soon as the retries plus that final wait clear the minimum.
		if spanned.saturating_add(interval) >= MIN_SYN_SCHEDULE_MS {
			return attempts;
		}
		interval = clamp_rto(interval.saturating_mul(2));
	}
}

/// How long the SYN schedule spans in total: every retry interval, plus the wait on the last.
pub fn syn_schedule_ms() -> u32 {
	let attempts: usize = syn_attempts();
	let mut total: u32 = 0;
	let mut last: u32 = INITIAL_RTO_MS;
	for index in 0..attempts {
		last = syn_interval(index).unwrap_or(last);
		total = total.saturating_add(last);
	}
	total.saturating_add(last)
}

#[cfg(test)]
mod tests;
