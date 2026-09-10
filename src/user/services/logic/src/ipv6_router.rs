//! The default-router list, and the total order this layer exports over it.
//!
//! ORDERING, NOT SELECTION. Choosing which router a particular flow uses is a policy question that
//! belongs to the layer above, which knows the source address, the destination and the transport.
//! What this layer owes is a TOTAL, STABLE ORDER over the candidates, so that the layer above has
//! something to select from and two implementations cannot disagree about what "first" means.
//!
//! THE THREE KEYS, IN THIS ORDER AND NO OTHER:
//!
//!   1. REACHABILITY CLASS. `Reachable`, `Stale`, `Delay` and `Probe` are ONE class - usable, or
//!      probably usable - and there is no ranking inside it. `Incomplete` and known-unreachable
//!      candidates come last. Recent confirmation does not outrank advertised preference, so a
//!      high-preference `Stale` router precedes a low-preference `Reachable` one; a known
//!      unreachable high-preference router still follows a usable low-preference one.
//!   2. ADVERTISED PREFERENCE: high, then medium, then low. A reserved value reads as medium.
//!   3. THE TIE-BREAK: ascending by the router's link-local address, compared as sixteen octets.
//!      It is arbitrary, and that is what makes it right: it is total, stable across boots, needs no
//!      extra state, and nobody has to agree about anything except the bytes.
//!
//! A LIST AND NOT A SCALAR. One default router in a field means an unrelated advertisement
//! overwrites a working one; the cap is eight, which meets the two RFC 4861 requires and matches the
//! bound the transport layer above expects.

use crate::ipv6::{Address, Interface};
use crate::ipv6_budget::{Refusals, Resource};
use crate::ipv6_neighbour::NeighbourState;
use alloc::vec::Vec;

/// The advertised preference of a router, RFC 4191 section 2.1.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Preference {
	High,
	Medium,
	Low,
}

impl Preference {
	/// Read the two-bit field. `10` is reserved and reads as medium, which is what RFC 4191
	/// requires - not "unknown", and not a fourth rank.
	pub fn from_bits(bits: u8) -> Preference {
		match bits & 0b11 {
			0b01 => Preference::High,
			0b11 => Preference::Low,
			_ => Preference::Medium,
		}
	}

	/// Ordering rank, lower is better.
	fn rank(&self) -> u8 {
		match self {
			Preference::High => 0,
			Preference::Medium => 1,
			Preference::Low => 2,
		}
	}
}

/// Where a router sits in the reachability classes of key 1.
///
/// TWO CLASSES, NOT FIVE. The four usable NUD states are one class with no internal ranking, which
/// is the whole content of the 2026-09-08 correction: an implementation that ranked `Reachable`
/// above `Stale` would put a low-preference confirmed router ahead of a high-preference unconfirmed
/// one, which is the outcome key 2 exists to produce.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reachability {
	/// `Reachable`, `Stale`, `Delay` or `Probe`.
	Usable,
	/// `Incomplete`, or a candidate probing has retired.
	Unusable,
}

impl Reachability {
	/// The class an NUD state falls in.
	pub fn of(state: NeighbourState) -> Reachability {
		match state {
			NeighbourState::Reachable | NeighbourState::Stale | NeighbourState::Delay | NeighbourState::Probe => Reachability::Usable,
			NeighbourState::Incomplete => Reachability::Unusable,
		}
	}

	fn rank(&self) -> u8 {
		match self {
			Reachability::Usable => 0,
			Reachability::Unusable => 1,
		}
	}
}

/// A router's lifetime, which is how it leaves the list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RouterLifetime {
	/// Expires at this monotonic millisecond.
	Until(u64),
}

/// One default router.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Router {
	pub interface: Interface,
	/// The router's link-local address, which is its identity here and the tie-break key.
	pub address: Address,
	pub preference: Preference,
	pub lifetime: RouterLifetime,
	/// The reachability class of the neighbour entry for this router, refreshed by the caller from
	/// the neighbour cache. Kept here rather than looked up so the order is a pure function of the
	/// list, which is what makes it testable and stable within one pass.
	pub reachability: Reachability,
}

/// What an advertisement did to the list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RouterOutcome {
	/// A new router took a slot.
	Added,
	/// An existing router's lifetime or preference was refreshed.
	Refreshed,
	/// Router Lifetime zero: the router withdrew itself. It is gone.
	Withdrawn,
	/// A zero lifetime for a router that was not in the list. Nothing changed.
	NotPresent,
	/// The list is full of live routers, and none was evicted to make room.
	Capacity,
}

/// The bounded default-router list.
#[derive(Debug, Default)]
pub struct RouterList {
	entries: Vec<Router>,
	refusals: Refusals,
}

impl RouterList {
	pub fn new() -> RouterList {
		RouterList::default()
	}

	/// Process a router advertisement's Router Lifetime field.
	///
	/// A lifetime of zero WITHDRAWS the router, which is one of the two ways the list empties. The
	/// caller checks whether the list became empty and restarts solicitation if it did; that rule is
	/// about the state of the list, not about which of the two causes emptied it.
	pub fn advertise(&mut self, interface: Interface, address: Address, preference: Preference, lifetime_seconds: u32, now_ms: u64) -> RouterOutcome {
		if lifetime_seconds == 0 {
			return if self.remove(interface, address) { RouterOutcome::Withdrawn } else { RouterOutcome::NotPresent };
		}
		let expires = now_ms.saturating_add(u64::from(lifetime_seconds).saturating_mul(1000));
		if let Some(entry) = self.entries.iter_mut().find(|held| held.interface == interface && held.address == address) {
			entry.preference = preference;
			entry.lifetime = RouterLifetime::Until(expires);
			return RouterOutcome::Refreshed;
		}
		if self.entries.len() as u32 >= Resource::DefaultRouters.limit() {
			self.refusals.record(Resource::DefaultRouters);
			return RouterOutcome::Capacity;
		}
		self.entries.push(Router { interface, address, preference, lifetime: RouterLifetime::Until(expires), reachability: Reachability::Unusable });
		RouterOutcome::Added
	}

	/// Tell the list what the neighbour cache currently says about a router.
	pub fn set_reachability(&mut self, interface: Interface, address: Address, reachability: Reachability) -> bool {
		let Some(entry) = self.entries.iter_mut().find(|held| held.interface == interface && held.address == address) else {
			return false;
		};
		entry.reachability = reachability;
		true
	}

	/// Drop routers whose lifetime has run out. The other way the list empties.
	pub fn expire(&mut self, now_ms: u64) -> Vec<Address> {
		let mut gone = Vec::new();
		self.entries.retain(|entry| {
			let RouterLifetime::Until(deadline) = entry.lifetime;
			if deadline <= now_ms {
				gone.push(entry.address);
				return false;
			}
			true
		});
		gone
	}

	pub fn remove(&mut self, interface: Interface, address: Address) -> bool {
		let before = self.entries.len();
		self.entries.retain(|entry| !(entry.interface == interface && entry.address == address));
		self.entries.len() != before
	}

	pub fn clear_interface(&mut self, interface: Interface) -> usize {
		let before = self.entries.len();
		self.entries.retain(|entry| entry.interface != interface);
		before - self.entries.len()
	}

	/// The candidates, in the exported order.
	///
	/// EXPORTED WHOLE. This is the router key for EVERY equal-prefix route decision above, not only
	/// for a source-aware subset: a consumer that froze an order of its own for ordinary routes
	/// would be able to choose an unreachable router over a usable one, which is exactly what key 1
	/// exists to prevent.
	pub fn ordered(&self) -> Vec<Router> {
		let mut out = self.entries.clone();
		out.sort_by(|left, right| left.reachability.rank().cmp(&right.reachability.rank()).then(left.preference.rank().cmp(&right.preference.rank())).then(left.address.octets().cmp(&right.address.octets())));
		out
	}

	pub fn len(&self) -> usize {
		self.entries.len()
	}

	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}

	pub fn refusals(&self) -> Refusals {
		self.refusals
	}
}

#[cfg(test)]
mod tests;
