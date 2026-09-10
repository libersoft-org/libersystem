//! Router solicitation, and the retransmission schedule that makes it survive ordinary loss.
//!
//! WHY THIS IS NOT ONE PACKET. A host that solicits once and loses that packet has no prefixes, no
//! default route and no recursive DNS server until the router happens to send an unsolicited
//! advertisement - which on a quiet link can be minutes, and on some links is never. That is
//! ordinary loss rather than an attack, and it leaves an appliance with no network at all.
//!
//! THE SCHEDULE IS ADOPTED, NOT INVENTED. RFC 8504 requires RFC 7559's algorithm, which is RFC 3315
//! section 14 with `IRT = 4 s`, `MRT = 3600 s` and no retry or duration bound. Every computation
//! draws a fresh uniform `RAND` in `[-0.1, +0.1)`:
//!
//! ```text
//! first:       RT = IRT + RAND * IRT
//! subsequent:  RT = 2 * RTprev + RAND * RTprev
//! if RT > MRT: RT = MRT + RAND * MRT
//! ```
//!
//! The jitter on a subsequent interval multiplies `RTprev`, NOT twice it, so the uncapped band is
//! `[1.9, 2.1) * RTprev`. Equality with `MRT` is not above it and does not trigger the replacement.
//! After the cap the band is `[3240, 3960)` seconds, so `MRT` is not a hard ceiling once jitter is
//! applied - which is the point of writing the formula down rather than describing it.
//!
//! NO FLOATING POINT. The randomness is drawn as a signed count of thousandths, which makes every
//! interval exactly reproducible in a test and removes any question about rounding on a target
//! without an FPU.

/// The initial retransmission time, in milliseconds.
pub const IRT_MS: u64 = 4_000;

/// The maximum retransmission time, in milliseconds.
pub const MRT_MS: u64 = 3_600_000;

/// The bounds of the jitter draw, in thousandths: `[-100, +100)` is `[-0.1, +0.1)`.
pub const RAND_MIN_MILLI: i32 = -100;
pub const RAND_MAX_MILLI: i32 = 100;

/// Apply `RAND * base` to `base`, in integer thousandths.
fn jitter(base: u64, rand_milli: i32) -> u64 {
	let rand = rand_milli.clamp(RAND_MIN_MILLI, RAND_MAX_MILLI - 1);
	let magnitude = base.saturating_mul(rand.unsigned_abs() as u64) / 1000;
	if rand < 0 { base.saturating_sub(magnitude) } else { base.saturating_add(magnitude) }
}

/// `RT = IRT + RAND * IRT`.
pub fn first_interval(rand_milli: i32) -> u64 {
	jitter(IRT_MS, rand_milli)
}

/// `RT = 2 * RTprev + RAND * RTprev`, replaced by `MRT + RAND * MRT` when it exceeds `MRT`.
pub fn next_interval(previous_ms: u64, rand_milli: i32) -> u64 {
	let doubled = previous_ms.saturating_mul(2);
	let rand = rand_milli.clamp(RAND_MIN_MILLI, RAND_MAX_MILLI - 1);
	let magnitude = previous_ms.saturating_mul(rand.unsigned_abs() as u64) / 1000;
	let candidate = if rand < 0 { doubled.saturating_sub(magnitude) } else { doubled.saturating_add(magnitude) };
	// STRICTLY ABOVE. An interval that lands exactly on MRT is not above it, and replacing it would
	// re-jitter a value that was already inside the band.
	if candidate > MRT_MS { jitter(MRT_MS, rand_milli) } else { candidate }
}

/// Why the schedule is running or not.
///
/// The state is what decides, not the history: a host with no default router solicits, whatever the
/// sequence of advertisements and withdrawals that emptied the list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SolicitState {
	/// Not soliciting: a default route is installed.
	Idle,
	/// Soliciting, with the interval last computed.
	Running { interval_ms: u64, deadline_ms: u64 },
}

/// The per-interface solicitation state.
#[derive(Clone, Copy, Debug)]
pub struct Solicitation {
	state: SolicitState,
	/// A diagnostic count. It NEVER decides termination: `MRC = 0` means there is no retry bound,
	/// and a counter that stopped the schedule would be a bound by another name.
	sent: u32,
}

impl Default for Solicitation {
	fn default() -> Solicitation {
		Solicitation { state: SolicitState::Idle, sent: 0 }
	}
}

impl Solicitation {
	pub fn new() -> Solicitation {
		Solicitation::default()
	}

	/// Begin soliciting: the interface came up, or the default-router list became empty.
	///
	/// Restarting from any state resets the interval to the first one. A host that has just lost its
	/// last router should ask again promptly rather than at the hour-long interval it had backed off
	/// to while it had one.
	pub fn start(&mut self, now_ms: u64, rand_milli: i32) -> u64 {
		let interval = first_interval(rand_milli);
		self.state = SolicitState::Running { interval_ms: interval, deadline_ms: now_ms.saturating_add(interval) };
		self.sent = self.sent.saturating_add(1);
		interval
	}

	/// The deadline expired: send another solicitation and compute the next interval.
	///
	/// Returns `None` when the schedule is not running, so a stray timer cannot restart it.
	pub fn on_timeout(&mut self, now_ms: u64, rand_milli: i32) -> Option<u64> {
		let SolicitState::Running { interval_ms, .. } = self.state else {
			return None;
		};
		let next = next_interval(interval_ms, rand_milli);
		self.state = SolicitState::Running { interval_ms: next, deadline_ms: now_ms.saturating_add(next) };
		self.sent = self.sent.saturating_add(1);
		Some(next)
	}

	/// A router advertisement INSTALLED A DEFAULT ROUTE. That, and only that, stops the schedule.
	///
	/// Not "an advertisement arrived": a valid advertisement with a nonzero lifetime can fail to
	/// reserve the route or neighbour state it needs, and one carrying Router Lifetime zero installs
	/// no route at all while its prefix, MTU and DNS options are still processed. In both cases the
	/// host still has no default router and must keep asking.
	pub fn default_route_installed(&mut self) {
		self.state = SolicitState::Idle;
	}

	/// The default-router list became empty, by expiry, by withdrawal, or by any other route.
	pub fn router_list_empty(&mut self, now_ms: u64, rand_milli: i32) -> u64 {
		self.start(now_ms, rand_milli)
	}

	pub fn state(&self) -> SolicitState {
		self.state
	}

	pub fn is_running(&self) -> bool {
		matches!(self.state, SolicitState::Running { .. })
	}

	pub fn deadline(&self) -> Option<u64> {
		match self.state {
			SolicitState::Running { deadline_ms, .. } => Some(deadline_ms),
			SolicitState::Idle => None,
		}
	}

	pub fn interval(&self) -> Option<u64> {
		match self.state {
			SolicitState::Running { interval_ms, .. } => Some(interval_ms),
			SolicitState::Idle => None,
		}
	}

	/// How many solicitations have been sent. Diagnostic only.
	pub fn sent(&self) -> u32 {
		self.sent
	}
}

#[cfg(test)]
mod tests;
