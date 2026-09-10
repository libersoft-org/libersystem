//! One aggregated deadline over every IPv6 timer, and a bounded amount of due work per iteration.
//!
//! WHY ONE DEADLINE AND NOT A TIMER PER MACHINE. The service loop blocks on something: a frame, an
//! RPC, a lease. Whatever it blocks on, it must not block PAST the next thing that has to happen -
//! and "the next thing" is spread across duplicate-address detection, router solicitation, neighbour
//! retries, unreachability probes, four kinds of lifetime expiry, path-MTU expiry and two multicast
//! listener timers. A layer that let each machine own its own timer would let a blocking `resolve`
//! starve all of them, which is the shape the previous one-event-per-frame model had.
//!
//! WHY DUE WORK IS BOUNDED. If a thousand timers come due at once, running all of them before the
//! next frame is read is a stall an attacker can arrange. Sixteen run, the cursor remembers where it
//! stopped, and the rest stay due - so the aggregated deadline is still immediate and the loop comes
//! straight back to them, interleaved with ingress.

use crate::ipv6_budget::Resource;
use crate::ipv6_events::Identity;
use alloc::vec::Vec;

/// What a timer is for.
///
/// The list is exhaustive on purpose: an earlier version of this contract called its list exhaustive
/// while omitting the multicast listener response delay and the state-change retransmission, and a
/// timer that is not in the aggregation is a timer that fires late or not at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TimerKind {
	/// A duplicate-address detection probe, or the wait after the last one.
	DuplicateAddress,
	/// A router solicitation retransmission.
	RouterSolicitation,
	/// A neighbour solicitation retransmission while resolving.
	NeighbourRetry,
	/// A neighbour unreachability probe, or the reachable-time expiry that starts one.
	Unreachability,
	/// A learned prefix's valid or preferred lifetime.
	PrefixLifetime,
	/// One of this host's own addresses: preferred to deprecated, or valid to gone.
	AddressLifetime,
	/// A default router's lifetime.
	RouterLifetime,
	/// A recursive DNS server's lifetime.
	RdnssLifetime,
	/// A path-MTU record's expiry, 600 seconds after the last accepted lowering.
	PathMtuExpiry,
	/// The delay before answering a multicast listener query.
	MldResponseDelay,
	/// A multicast listener state-change report retransmission, which is what recovers a lost first
	/// report.
	MldStateChangeRetransmit,
}

/// Every kind, so the aggregation can be checked against the enum rather than against a comment.
pub const TIMER_KINDS: [TimerKind; 11] = [
	TimerKind::DuplicateAddress,
	TimerKind::RouterSolicitation,
	TimerKind::NeighbourRetry,
	TimerKind::Unreachability,
	TimerKind::PrefixLifetime,
	TimerKind::AddressLifetime,
	TimerKind::RouterLifetime,
	TimerKind::RdnssLifetime,
	TimerKind::PathMtuExpiry,
	TimerKind::MldResponseDelay,
	TimerKind::MldStateChangeRetransmit,
];

/// One armed timer: what it is for, what it is about, and when.
///
/// The identity is the same one the invalidation queue uses, so a timer that fires and a table that
/// changes name the same thing and a consumer does not have to translate between two vocabularies.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Timer {
	pub kind: TimerKind,
	pub identity: Identity,
	/// Monotonic, in the same unit the caller measures `now` in.
	pub deadline: u64,
}

/// The armed timers, and the cursor that keeps due work fair.
#[derive(Debug, Default)]
pub struct Timers {
	entries: Vec<Timer>,
	/// Where the last bounded pass stopped, so the next one resumes rather than restarts. Without
	/// it, a set of due timers larger than the per-iteration bound would run its first sixteen
	/// forever and never reach the rest.
	cursor: usize,
}

impl Timers {
	pub fn new() -> Timers {
		Timers::default()
	}

	/// Arm a timer, replacing any timer of the same kind for the same identity.
	///
	/// ONE TIMER PER (KIND, IDENTITY), which is what makes re-arming idempotent: a router
	/// advertisement that refreshes a lifetime must move the deadline, not add a second one that
	/// fires at the old time.
	pub fn set(&mut self, timer: Timer) {
		if let Some(existing) = self.entries.iter_mut().find(|held| held.kind == timer.kind && held.identity == timer.identity) {
			existing.deadline = timer.deadline;
			return;
		}
		self.entries.push(timer);
	}

	/// Disarm one timer. Returns whether there was one.
	pub fn clear(&mut self, kind: TimerKind, identity: Identity) -> bool {
		let before = self.entries.len();
		self.entries.retain(|held| !(held.kind == kind && held.identity == identity));
		self.entries.len() != before
	}

	/// Disarm every timer about `identity`, whatever its kind.
	///
	/// An address that goes away takes its detection, lifetime and probe timers with it, and a
	/// caller that had to remember which kinds existed would eventually forget one.
	pub fn clear_identity(&mut self, identity: Identity) -> usize {
		let before = self.entries.len();
		self.entries.retain(|held| held.identity != identity);
		before - self.entries.len()
	}

	/// Disarm every timer belonging to an interface, for teardown or a generation change.
	pub fn clear_interface(&mut self, interface: crate::ipv6::Interface) -> usize {
		let before = self.entries.len();
		self.entries.retain(|held| held.identity.interface() != interface);
		before - self.entries.len()
	}

	/// The earliest deadline armed, or `None` when nothing is.
	///
	/// THIS IS WHAT THE SERVICE LOOP BOUNDS ITS WAIT BY. A caller that has other reasons to wake
	/// takes the minimum of this and its own deadline; a caller with none waits for exactly this.
	pub fn next_deadline(&self) -> Option<u64> {
		self.entries.iter().map(|timer| timer.deadline).min()
	}

	/// How long to wait from `now`, saturating at zero when something is already due.
	pub fn wait_from(&self, now: u64) -> Option<u64> {
		self.next_deadline().map(|deadline| deadline.saturating_sub(now))
	}

	/// Take up to `Resource::DueActionsPerIteration` timers that are due at `now`, resuming where
	/// the last pass stopped.
	///
	/// A timer that comes back is DISARMED: the caller re-arms it if the machine it belongs to wants
	/// another round. That is what stops a retransmission from firing forever because nobody
	/// remembered to clear it.
	pub fn due(&mut self, now: u64) -> Vec<Timer> {
		let limit = Resource::DueActionsPerIteration.limit() as usize;
		let count = self.entries.len();
		if count == 0 {
			self.cursor = 0;
			return Vec::new();
		}
		let start = self.cursor % count;
		let mut fired = Vec::new();
		let mut fires = alloc::vec![false; count];
		for step in 0..count {
			if fired.len() >= limit {
				break;
			}
			let index = (start + step) % count;
			if self.entries[index].deadline <= now {
				fired.push(self.entries[index]);
				fires[index] = true;
			}
		}
		let mut kept = Vec::with_capacity(count - fired.len());
		for (index, timer) in self.entries.iter().enumerate() {
			if !fires[index] {
				kept.push(*timer);
			}
		}
		self.entries = kept;
		// KEEP THE POSITION, NOT THE INDEX. What was fired is gone, so the remaining due timers have
		// moved down; resuming near where the pass stopped is what stops a large due set from being
		// walked from the front every iteration while its tail waits.
		self.cursor = if self.entries.is_empty() { 0 } else { start % self.entries.len() };
		fired
	}

	/// Is anything due at `now`? A loop that wants to know whether to come straight back asks this
	/// rather than running the pass.
	pub fn any_due(&self, now: u64) -> bool {
		self.entries.iter().any(|timer| timer.deadline <= now)
	}

	pub fn len(&self) -> usize {
		self.entries.len()
	}

	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}

	/// How many timers of one kind are armed. For the capacity snapshot, and for a test that wants
	/// to know a machine armed what it said it would.
	pub fn count_of(&self, kind: TimerKind) -> usize {
		self.entries.iter().filter(|timer| timer.kind == kind).count()
	}
}

#[cfg(test)]
mod tests;
