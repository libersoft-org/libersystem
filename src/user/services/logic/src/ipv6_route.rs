//! What this interface believes is on-link, and where everything else goes.
//!
//! TWO TABLES AND NOT ONE, because they answer different questions and are refilled by different
//! events. A LEARNED PREFIX says "these addresses are neighbours" and is a fact several routers can
//! assert independently - which is why each one carries the set of routers currently saying it, and
//! why one router going away removes only its own membership. A ROUTE says "send this there" and is
//! keyed by destination and next hop, so the same destination reached through two routers is two
//! routes and not one flapping record.
//!
//! WHY THE ADVERTISER SET IS PER PREFIX AND BOUNDED. A prefix advertised by two routers must survive
//! one of them withdrawing it: a host that dropped the prefix when the first advertiser left would
//! stop treating its own link as its own link, and would then resolve every neighbour through a
//! router that is not there. Holding the set is what makes the withdrawal a membership change. It is
//! bounded for the reason every other table here is: the set's size is chosen by whoever is on the
//! link.
//!
//! WHAT IS DELIBERATELY NOT HERE. No forwarding, no metric arithmetic and no route redistribution -
//! this is a host's table. Ordering among default routers belongs to `ipv6_router`, which owns the
//! preference and reachability keys; this table stores the routes those routers gave and answers
//! "which route covers this address", longest match first.

use crate::ipv6::{Address, Interface, Prefix};
use crate::ipv6_budget::{Refusals, Resource};
use crate::ipv6_slaac::Lifetime;
use alloc::vec::Vec;

/// One learned on-link prefix and the routers currently asserting it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LearnedPrefix {
	pub interface: Interface,
	pub prefix: Prefix,
	pub valid: Lifetime,
	advertisers: Vec<Address>,
}

impl LearnedPrefix {
	/// The routers currently saying this prefix is on-link.
	pub fn advertisers(&self) -> &[Address] {
		&self.advertisers
	}
}

/// What an advertisement did to the prefix table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrefixOutcome {
	/// A prefix nothing had advertised took a slot.
	Admitted,
	/// A prefix already held had its lifetime refreshed. No slot was taken.
	Refreshed,
	/// A prefix already held gained another router asserting it.
	AdvertiserAdded,
	/// The prefix stays, and this router is no longer one of the ones asserting it.
	Withdrawn,
	/// The prefix is gone: the router that withdrew it was the last one asserting it.
	Removed,
}

/// The bounded table of on-link prefixes.
#[derive(Debug, Default)]
pub struct PrefixTable {
	entries: Vec<LearnedPrefix>,
	refusals: Refusals,
}

impl PrefixTable {
	pub fn new() -> PrefixTable {
		PrefixTable::default()
	}

	/// A router says this prefix is on-link, for this long.
	///
	/// UNIQUE BY PREFIX AND LENGTH: a second advertisement of the same prefix updates the record it
	/// already has rather than taking another slot, which is what stops a router refreshing its own
	/// advertisement from filling the table.
	pub fn advertise(&mut self, interface: Interface, prefix: Prefix, router: Address, valid: Lifetime) -> Result<PrefixOutcome, Resource> {
		if let Some(index) = self.entries.iter().position(|held| held.interface == interface && held.prefix == prefix) {
			let entry = &mut self.entries[index];
			entry.valid = valid;
			if entry.advertisers.contains(&router) {
				return Ok(PrefixOutcome::Refreshed);
			}
			if entry.advertisers.len() as u32 >= Resource::AdvertisersPerPrefix.limit() {
				self.refusals.record(Resource::AdvertisersPerPrefix);
				return Err(Resource::AdvertisersPerPrefix);
			}
			entry.advertisers.push(router);
			return Ok(PrefixOutcome::AdvertiserAdded);
		}
		if self.entries.len() as u32 >= Resource::Prefixes.limit() {
			self.refusals.record(Resource::Prefixes);
			return Err(Resource::Prefixes);
		}
		self.entries.push(LearnedPrefix { interface, prefix, valid, advertisers: alloc::vec![router] });
		Ok(PrefixOutcome::Admitted)
	}

	/// A router stopped asserting a prefix. Only its own membership goes.
	pub fn withdraw(&mut self, interface: Interface, prefix: Prefix, router: Address) -> Option<PrefixOutcome> {
		let index = self.entries.iter().position(|held| held.interface == interface && held.prefix == prefix)?;
		let entry = &mut self.entries[index];
		let position = entry.advertisers.iter().position(|held| *held == router)?;
		entry.advertisers.remove(position);
		if entry.advertisers.is_empty() {
			self.entries.remove(index);
			return Some(PrefixOutcome::Removed);
		}
		Some(PrefixOutcome::Withdrawn)
	}

	/// A router went away entirely. Every prefix loses that membership, and the ones left with none
	/// are gone.
	pub fn withdraw_router(&mut self, interface: Interface, router: Address) -> Vec<Prefix> {
		let mut removed = Vec::new();
		for entry in self.entries.iter_mut() {
			if entry.interface != interface {
				continue;
			}
			if let Some(position) = entry.advertisers.iter().position(|held| *held == router) {
				entry.advertisers.remove(position);
			}
		}
		self.entries.retain(|entry| {
			if entry.advertisers.is_empty() {
				removed.push(entry.prefix);
				return false;
			}
			true
		});
		removed
	}

	/// Drop prefixes whose valid lifetime has run out.
	pub fn expire(&mut self, now_ms: u64) -> Vec<Prefix> {
		let mut removed = Vec::new();
		self.entries.retain(|entry| {
			if entry.valid.expired(now_ms) {
				removed.push(entry.prefix);
				return false;
			}
			true
		});
		removed
	}

	/// Is this address on this link, by some prefix currently held?
	pub fn on_link(&self, interface: Interface, address: Address, now_ms: u64) -> bool {
		self.entries.iter().any(|entry| entry.interface == interface && !entry.valid.expired(now_ms) && entry.prefix.contains(address))
	}

	pub fn get(&self, interface: Interface, prefix: Prefix) -> Option<&LearnedPrefix> {
		self.entries.iter().find(|held| held.interface == interface && held.prefix == prefix)
	}

	/// The prefix an address was formed from, longest match first.
	///
	/// RFC 8028 RESTRICTS THE DEFAULT ROUTER TO THE ADVERTISERS OF THE SOURCE'S PREFIX, and this is
	/// how a source address is turned back into that set. The longest match decides, for the same
	/// reason it decides a route: a more specific prefix is a more specific statement about who is
	/// on this link, and its advertisers are the ones that actually said so.
	pub fn covering(&self, interface: Interface, address: Address, now_ms: u64) -> Option<&LearnedPrefix> {
		self.entries.iter().filter(|entry| entry.interface == interface && !entry.valid.expired(now_ms) && entry.prefix.contains(address)).max_by_key(|entry| entry.prefix.len())
	}

	/// The size of the fullest advertiser set currently held.
	///
	/// A PER-PREFIX CEILING HAS NO SINGLE "USED" VALUE, and this is the one a consumer watching for
	/// saturation wants: the set that will refuse first.
	pub fn widest_advertiser_set(&self) -> usize {
		self.entries.iter().map(|entry| entry.advertisers.len()).max().unwrap_or(0)
	}

	pub fn clear_interface(&mut self, interface: Interface) -> usize {
		let before = self.entries.len();
		self.entries.retain(|entry| entry.interface != interface);
		before - self.entries.len()
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

/// What a route is for, which is what decides how it is matched.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RouteKind {
	/// `fe80::/10`, installed once with the interface and never advertised.
	LinkLocal,
	/// A prefix a router said is on-link. Its destinations are neighbours, so it has no next hop.
	OnLink,
	/// `::/0` through one default router.
	Default,
}

/// One route.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Route {
	pub interface: Interface,
	pub destination: Prefix,
	/// The router to send through, or `None` when the destination is directly attached.
	pub next_hop: Option<Address>,
	pub kind: RouteKind,
	pub expires: Lifetime,
}

/// What an install did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RouteOutcome {
	/// A route nothing held took a slot.
	Installed,
	/// A route already held had its lifetime refreshed. No slot was taken.
	Refreshed,
}

/// The bounded route table.
#[derive(Debug, Default)]
pub struct RouteTable {
	entries: Vec<Route>,
	refusals: Refusals,
}

impl RouteTable {
	pub fn new() -> RouteTable {
		RouteTable::default()
	}

	/// Install a route, or refresh the one already there.
	///
	/// THE KEY IS DESTINATION PLUS NEXT HOP. The same destination through two routers is two routes,
	/// because that is what lets one of them be withdrawn without taking the other's path with it.
	pub fn install(&mut self, route: Route) -> Result<RouteOutcome, Resource> {
		if let Some(existing) = self.entries.iter_mut().find(|held| held.interface == route.interface && held.destination == route.destination && held.next_hop == route.next_hop) {
			existing.expires = route.expires;
			existing.kind = route.kind;
			return Ok(RouteOutcome::Refreshed);
		}
		if self.entries.len() as u32 >= Resource::Routes.limit() {
			self.refusals.record(Resource::Routes);
			return Err(Resource::Routes);
		}
		self.entries.push(route);
		Ok(RouteOutcome::Installed)
	}

	pub fn remove(&mut self, interface: Interface, destination: Prefix, next_hop: Option<Address>) -> bool {
		let before = self.entries.len();
		self.entries.retain(|held| !(held.interface == interface && held.destination == destination && held.next_hop == next_hop));
		before != self.entries.len()
	}

	/// Every route through this next hop goes.
	pub fn remove_next_hop(&mut self, interface: Interface, next_hop: Address) -> usize {
		let before = self.entries.len();
		self.entries.retain(|held| !(held.interface == interface && held.next_hop == Some(next_hop)));
		before - self.entries.len()
	}

	/// Drop routes whose lifetime has run out.
	pub fn expire(&mut self, now_ms: u64) -> Vec<Route> {
		let mut removed = Vec::new();
		self.entries.retain(|entry| {
			if entry.expires.expired(now_ms) {
				removed.push(*entry);
				return false;
			}
			true
		});
		removed
	}

	/// The route that covers this address: longest prefix first, so an on-link prefix beats the
	/// default route and a more specific prefix beats a less specific one.
	///
	/// AMONG EQUAL LENGTHS THIS TABLE DOES NOT CHOOSE. Two default routes are two routers, and which
	/// router to prefer is `ipv6_router`'s decision over preference and reachability - a second
	/// opinion here would be a second answer to the same question.
	pub fn lookup(&self, interface: Interface, destination: Address, now_ms: u64) -> Option<&Route> {
		self.entries.iter().filter(|entry| entry.interface == interface && !entry.expires.expired(now_ms) && entry.destination.contains(destination)).max_by_key(|entry| entry.destination.len())
	}

	/// Every route currently held, for a consumer taking a snapshot.
	pub fn routes(&self) -> &[Route] {
		&self.entries
	}

	pub fn clear_interface(&mut self, interface: Interface) -> usize {
		let before = self.entries.len();
		self.entries.retain(|entry| entry.interface != interface);
		before - self.entries.len()
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
