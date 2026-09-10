//! The neighbour cache and the five states RFC 4861 gives an entry.
//!
//! WHY ALL FIVE. A cache that stops at `STALE` has one of two bugs and cannot avoid both: either it
//! keeps using a neighbour that has gone away, because nothing ever revalidates it, or it throws one
//! away the moment it goes stale, because nothing distinguishes "unconfirmed" from "dead". `DELAY`
//! and `PROBE` are the revalidation: a stale entry is used, and a unicast probe is sent a moment
//! later to find out whether it should have been. RFC 8504 requires this of a host, and an appliance
//! whose default router silently changes its link-layer address needs it to recover at all.
//!
//! WHAT MOVES AN ENTRY, IN ONE SENTENCE EACH:
//!
//!   `INCOMPLETE`  a packet needs an address nothing has answered for. Multicast solicitations go to
//!                   the solicited-node group; after three, the entry is retired and every packet
//!                   waiting on it fails.
//!   `REACHABLE`   an advertisement with the Solicited flag confirmed it, or a consumer above said
//!                   its traffic is getting through. It lasts a reachable time and then goes stale.
//!   `STALE`       usable, unconfirmed. Nothing is sent about it until something is sent TO it.
//!   `DELAY`       something was sent to a stale neighbour. Wait a moment for the upper layer to
//!                   confirm reachability by itself before spending a probe.
//!   `PROBE`       unicast solicitations, three of them, and then the entry is gone.
//!
//! AN UNSOLICITED ADVERTISEMENT NEVER MAKES AN ENTRY REACHABLE. Only a solicited one, or a
//! confirmation from the layer above, proves the path works in both directions; an unsolicited
//! advertisement proves only that somebody sent a packet claiming an address.

use crate::ipv6::{Address, Interface};
use crate::ipv6_budget::{Refusals, Resource};
use alloc::vec::Vec;

/// How long to wait between solicitations, in milliseconds.
pub const RETRANS_TIMER_MS: u64 = 1_000;

/// How long a confirmed entry stays reachable, in milliseconds.
pub const REACHABLE_TIME_MS: u64 = 30_000;

/// How long a `DELAY` entry waits for the upper layer before probing, in milliseconds.
pub const DELAY_FIRST_PROBE_MS: u64 = 5_000;

/// How many multicast solicitations resolve an address before the entry is retired.
pub const MAX_MULTICAST_SOLICIT: u8 = 3;

/// How many unicast probes revalidate an entry before it is removed.
pub const MAX_UNICAST_SOLICIT: u8 = 3;

/// The five states of RFC 4861 section 7.3.2.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NeighbourState {
	Incomplete,
	Reachable,
	Stale,
	Delay,
	Probe,
}

/// One cache entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Neighbour {
	pub interface: Interface,
	pub address: Address,
	pub state: NeighbourState,
	/// None only in `INCOMPLETE`: every other state has an address to send to.
	pub link_layer: Option<[u8; 6]>,
	/// Whether this neighbour advertised itself as a router. A router that stops being one is
	/// removed from the default-router list, which is the caller's table and not this one.
	pub is_router: bool,
	/// Solicitations or probes sent in the current state.
	pub attempts: u8,
	/// When this state ends, in milliseconds. `STALE` has no deadline and holds `None`.
	pub deadline_ms: Option<u64>,
}

/// What the caller must do about a state change.
///
/// The cache decides; the caller puts packets on the wire. Keeping the two apart is what lets the
/// whole machine be driven by a test with no network at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
	/// Send a neighbour solicitation to the target's solicited-node group.
	SolicitMulticast { target: Address },
	/// Send a neighbour solicitation to the neighbour's own link-layer address.
	SolicitUnicast { target: Address, link_layer: [u8; 6] },
	/// The entry is gone. Every packet waiting on it fails with `ResolutionFailed`.
	Retire { target: Address },
	/// The entry became usable: send whatever was waiting.
	Resolved { target: Address, link_layer: [u8; 6] },
	/// Nothing to do.
	Nothing,
}

/// What a lookup found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lookup {
	/// Send to this link-layer address.
	Ready { link_layer: [u8; 6] },
	/// Resolution has begun or is under way: retain the packet.
	Pending,
	/// The cache is full of live entries. The packet cannot be retained against a neighbour that
	/// does not exist, and no live entry was evicted to make room.
	Capacity,
}

/// The bounded neighbour cache.
#[derive(Debug, Default)]
pub struct NeighbourCache {
	entries: Vec<Neighbour>,
	refusals: Refusals,
}

impl NeighbourCache {
	pub fn new() -> NeighbourCache {
		NeighbourCache::default()
	}

	fn find(&mut self, interface: Interface, address: Address) -> Option<&mut Neighbour> {
		self.entries.iter_mut().find(|entry| entry.interface == interface && entry.address == address)
	}

	/// Look `address` up for something about to be sent to it.
	///
	/// A `STALE` entry is USED and moved to `DELAY`: the packet goes out now, and the probe that
	/// finds out whether it should have gone out happens a moment later. Refusing to send while
	/// revalidating would stall every flow on a link whose neighbours have simply been quiet.
	pub fn resolve(&mut self, interface: Interface, address: Address, now_ms: u64) -> (Lookup, Action) {
		if let Some(entry) = self.find(interface, address) {
			return match entry.state {
				NeighbourState::Incomplete => (Lookup::Pending, Action::Nothing),
				NeighbourState::Stale => {
					entry.state = NeighbourState::Delay;
					entry.attempts = 0;
					entry.deadline_ms = Some(now_ms + DELAY_FIRST_PROBE_MS);
					let link_layer = entry.link_layer.expect("a stale entry has a link-layer address");
					(Lookup::Ready { link_layer }, Action::Nothing)
				}
				NeighbourState::Reachable | NeighbourState::Delay | NeighbourState::Probe => {
					let link_layer = entry.link_layer.expect("only INCOMPLETE has none");
					(Lookup::Ready { link_layer }, Action::Nothing)
				}
			};
		}
		// A NEW ENTRY IS AN ADMISSION, and a full table refuses rather than evicting. Evicting a
		// live neighbour to make room for an unsolicited one is how a hostile link empties a cache.
		if self.entries.len() as u32 >= Resource::Neighbours.limit() {
			self.refusals.record(Resource::Neighbours);
			return (Lookup::Capacity, Action::Nothing);
		}
		self.entries.push(Neighbour { interface, address, state: NeighbourState::Incomplete, link_layer: None, is_router: false, attempts: 1, deadline_ms: Some(now_ms + RETRANS_TIMER_MS) });
		(Lookup::Pending, Action::SolicitMulticast { target: address })
	}

	/// A neighbour advertisement arrived.
	///
	/// `solicited` is the flag that proves two-way reachability; `override_flag` is what lets a
	/// neighbour that changed its link-layer address say so. An advertisement with neither, carrying
	/// a different address, leaves the entry stale rather than believing it - the entry stays usable
	/// and gets probed, which is the conservative direction.
	pub fn on_advertisement(&mut self, interface: Interface, address: Address, link_layer: [u8; 6], solicited: bool, override_flag: bool, is_router: bool, now_ms: u64) -> Action {
		let Some(entry) = self.find(interface, address) else {
			// UNSOLICITED ADVERTISEMENTS DO NOT CREATE ENTRIES. A host that made one per
			// advertisement would have its cache filled by anybody on the link.
			return Action::Nothing;
		};
		entry.is_router = is_router;
		let known = entry.link_layer;
		let differs = known.is_some_and(|held| held != link_layer);
		match entry.state {
			NeighbourState::Incomplete => {
				entry.link_layer = Some(link_layer);
				entry.attempts = 0;
				if solicited {
					entry.state = NeighbourState::Reachable;
					entry.deadline_ms = Some(now_ms + REACHABLE_TIME_MS);
				} else {
					entry.state = NeighbourState::Stale;
					entry.deadline_ms = None;
				}
				Action::Resolved { target: address, link_layer }
			}
			_ => {
				if differs && !override_flag {
					// Somebody claims this address with a different link-layer address and did not
					// set Override. Do not believe it, but do not keep trusting the old one either:
					// go stale so the next send probes.
					if entry.state == NeighbourState::Reachable {
						entry.state = NeighbourState::Stale;
						entry.deadline_ms = None;
					}
					return Action::Nothing;
				}
				if differs {
					entry.link_layer = Some(link_layer);
				}
				if solicited {
					entry.state = NeighbourState::Reachable;
					entry.attempts = 0;
					entry.deadline_ms = Some(now_ms + REACHABLE_TIME_MS);
				} else if differs {
					entry.state = NeighbourState::Stale;
					entry.deadline_ms = None;
				}
				Action::Nothing
			}
		}
	}

	/// A solicitation arrived from `address` carrying its link-layer address.
	///
	/// This creates or refreshes a STALE entry: the sender is on the link and this host is about to
	/// answer it, so knowing where to send the answer saves a resolution. It is never REACHABLE -
	/// nothing here proves this host's packets reach that neighbour.
	pub fn on_solicitation(&mut self, interface: Interface, address: Address, link_layer: [u8; 6]) -> Action {
		if let Some(entry) = self.find(interface, address) {
			// READ THE STATE BEFORE CHANGING IT. An INCOMPLETE entry has no link-layer address, so
			// the "differs" branch below is exactly the one it takes - and a check made afterwards
			// would never see that it had been resolving.
			let was_incomplete = entry.state == NeighbourState::Incomplete;
			let differs = entry.link_layer != Some(link_layer);
			if differs || was_incomplete {
				entry.link_layer = Some(link_layer);
				entry.state = NeighbourState::Stale;
				entry.deadline_ms = None;
				entry.attempts = 0;
			}
			if was_incomplete {
				return Action::Resolved { target: address, link_layer };
			}
			return Action::Nothing;
		}
		if self.entries.len() as u32 >= Resource::Neighbours.limit() {
			self.refusals.record(Resource::Neighbours);
			return Action::Nothing;
		}
		self.entries.push(Neighbour { interface, address, state: NeighbourState::Stale, link_layer: Some(link_layer), is_router: false, attempts: 0, deadline_ms: None });
		Action::Nothing
	}

	/// The layer above says its traffic to this neighbour is getting through.
	///
	/// RFC 4861 section 7.3.1: a transport that sees its data acknowledged has proved two-way
	/// reachability, and that is worth more than a probe because it costs nothing. It moves an entry
	/// to `REACHABLE` from any state that has a link-layer address.
	pub fn confirm_reachable(&mut self, interface: Interface, address: Address, now_ms: u64) -> bool {
		let Some(entry) = self.find(interface, address) else {
			return false;
		};
		if entry.link_layer.is_none() {
			return false;
		}
		entry.state = NeighbourState::Reachable;
		entry.attempts = 0;
		entry.deadline_ms = Some(now_ms + REACHABLE_TIME_MS);
		true
	}

	/// A timer for this entry expired.
	pub fn on_timeout(&mut self, interface: Interface, address: Address, now_ms: u64) -> Action {
		let Some(entry) = self.find(interface, address) else {
			return Action::Nothing;
		};
		match entry.state {
			NeighbourState::Incomplete => {
				if entry.attempts >= MAX_MULTICAST_SOLICIT {
					self.remove(interface, address);
					return Action::Retire { target: address };
				}
				entry.attempts += 1;
				entry.deadline_ms = Some(now_ms + RETRANS_TIMER_MS);
				Action::SolicitMulticast { target: address }
			}
			NeighbourState::Reachable => {
				// Confirmation ran out. The entry stays usable; it is simply no longer proven.
				entry.state = NeighbourState::Stale;
				entry.deadline_ms = None;
				Action::Nothing
			}
			NeighbourState::Delay => {
				let link_layer = entry.link_layer.expect("DELAY has a link-layer address");
				entry.state = NeighbourState::Probe;
				entry.attempts = 1;
				entry.deadline_ms = Some(now_ms + RETRANS_TIMER_MS);
				Action::SolicitUnicast { target: address, link_layer }
			}
			NeighbourState::Probe => {
				if entry.attempts >= MAX_UNICAST_SOLICIT {
					self.remove(interface, address);
					return Action::Retire { target: address };
				}
				let link_layer = entry.link_layer.expect("PROBE has a link-layer address");
				entry.attempts += 1;
				entry.deadline_ms = Some(now_ms + RETRANS_TIMER_MS);
				Action::SolicitUnicast { target: address, link_layer }
			}
			NeighbourState::Stale => Action::Nothing,
		}
	}

	/// Remove one entry, whatever its state. Used by teardown and by the retirement paths.
	pub fn remove(&mut self, interface: Interface, address: Address) -> bool {
		let before = self.entries.len();
		self.entries.retain(|entry| !(entry.interface == interface && entry.address == address));
		self.entries.len() != before
	}

	/// Remove every entry on an interface: it went away, or came back as a new generation.
	pub fn clear_interface(&mut self, interface: Interface) -> usize {
		let before = self.entries.len();
		self.entries.retain(|entry| entry.interface != interface);
		before - self.entries.len()
	}

	pub fn get(&self, interface: Interface, address: Address) -> Option<Neighbour> {
		self.entries.iter().copied().find(|entry| entry.interface == interface && entry.address == address)
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

	/// Every entry that has a deadline at or before `now_ms`, so the caller can drive them.
	pub fn due(&self, now_ms: u64) -> Vec<(Interface, Address)> {
		self.entries.iter().filter(|entry| entry.deadline_ms.is_some_and(|deadline| deadline <= now_ms)).map(|entry| (entry.interface, entry.address)).collect()
	}
}

#[cfg(test)]
mod tests;
