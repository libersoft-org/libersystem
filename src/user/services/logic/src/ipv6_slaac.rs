//! Address autoconfiguration: forming addresses from advertised prefixes, proving they are free,
//! and holding lifetimes in a way an unauthenticated advertisement cannot abuse.
//!
//! A ROUTER ADVERTISEMENT IS NOT AUTHENTICATED. Anything on the link can send one, so every rule
//! here is written against a sender that is lying: a prefix that is not a /64 does not silently
//! become an address, a preferred lifetime longer than the valid lifetime discards the option rather
//! than being clamped into something plausible, and a short valid lifetime cannot immediately
//! invalidate an address this host is already using. That last one is RFC 4862's two-hour rule, and
//! it is worth being precise about what it buys: it stops a forged PIO from cutting a working
//! address off in one packet. It does not authenticate anything, and it does not prevent every
//! denial of service.
//!
//! TWO FLAGS, TWO INDEPENDENT DECISIONS. `A` says an address may be formed from this prefix; `L`
//! says the prefix is on-link. They are evaluated separately because they mean different things, and
//! a router may legitimately set either without the other.
//!
//! AN ADDRESS IS NOT USABLE UNTIL IT IS PROVEN FREE. Every newly formed unicast address, the
//! link-local one included, is tentative until duplicate-address detection finishes. A host that
//! skipped that step would answer for an address another host already holds.

use crate::ipv6::{Address, Interface, Prefix};
use crate::ipv6_budget::{Refusals, Resource};
use alloc::vec::Vec;

/// The wire value that means "for ever".
pub const INFINITE_LIFETIME: u32 = 0xffff_ffff;

/// The two hours of RFC 4862 section 5.5.3, in milliseconds.
pub const TWO_HOURS_MS: u64 = 2 * 60 * 60 * 1000;

/// How many solicitations duplicate-address detection sends before declaring an address free.
pub const DUP_ADDR_DETECT_TRANSMITS: u8 = 1;

/// How long to wait for an answer to each detection probe, in milliseconds.
pub const DAD_RETRANS_MS: u64 = 1_000;

/// A lifetime, with ONE representation for infinity.
///
/// `Infinite` is selected explicitly by the wire value `0xffffffff` and by nothing else - not by a
/// very large finite number, and not by arithmetic that happened to saturate. That matters because
/// the transitions between the two are real: a prefix may be advertised as infinite and later as
/// finite, and a host that represented infinity as "a deadline far away" could not tell the
/// difference between that and a router announcing sixty-eight years.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lifetime {
	Infinite,
	/// Ends at this monotonic millisecond.
	Finite(u64),
}

impl Lifetime {
	/// Read a wire lifetime in seconds, relative to `now_ms`.
	pub fn from_wire(seconds: u32, now_ms: u64) -> Lifetime {
		if seconds == INFINITE_LIFETIME {
			return Lifetime::Infinite;
		}
		Lifetime::Finite(now_ms.saturating_add(u64::from(seconds).saturating_mul(1000)))
	}

	/// Has it run out at `now_ms`?
	pub fn expired(&self, now_ms: u64) -> bool {
		match self {
			Lifetime::Infinite => false,
			Lifetime::Finite(deadline) => *deadline <= now_ms,
		}
	}

	/// What is left, in milliseconds. `None` is infinity, not zero.
	pub fn remaining(&self, now_ms: u64) -> Option<u64> {
		match self {
			Lifetime::Infinite => None,
			Lifetime::Finite(deadline) => Some(deadline.saturating_sub(now_ms)),
		}
	}
}

/// A Prefix Information option, already read off the wire.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PrefixInformation {
	pub prefix: Prefix,
	/// `L`: the prefix is on-link.
	pub on_link: bool,
	/// `A`: an address may be formed from it.
	pub autonomous: bool,
	pub valid_seconds: u32,
	pub preferred_seconds: u32,
}

/// Why a Prefix Information option was not acted on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrefixRefusal {
	/// `PreferredLifetime > ValidLifetime`. The option is discarded WHOLE rather than clamped: a
	/// sender that got this wrong is a sender whose other fields are not worth trusting either.
	PreferredExceedsValid,
	/// The link-local prefix may not be advertised, and a host that took one would replace the
	/// address its own detection proved.
	LinkLocalPrefix,
	/// `A` was set on a prefix that is not a /64. Not truncated into an address, not padded: the
	/// autonomous flag is defined for that length alone.
	NotSlashSixtyFour,
	/// Neither flag was set, so there is nothing to do.
	NothingToDo,
}

/// What this host decided to do with a Prefix Information option.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PrefixOutcome {
	/// Install or refresh an on-link route for the prefix, with this lifetime.
	pub on_link_route: Option<Lifetime>,
	/// Form or refresh an address in the prefix, with these lifetimes.
	pub address: Option<(Lifetime, Lifetime)>,
}

/// Read a Prefix Information option into the two independent decisions it carries.
///
/// The refusals are checked before either decision, so a malformed option does neither half.
pub fn evaluate_prefix(option: &PrefixInformation, now_ms: u64) -> Result<PrefixOutcome, PrefixRefusal> {
	// The comparison is on the WIRE values, because infinity compares correctly there: 0xffffffff is
	// the largest, which is exactly the ordering the rule wants.
	if option.preferred_seconds > option.valid_seconds {
		return Err(PrefixRefusal::PreferredExceedsValid);
	}
	if Prefix::link_local().contains(option.prefix.base()) {
		return Err(PrefixRefusal::LinkLocalPrefix);
	}
	if option.autonomous && option.prefix.len() != 64 {
		return Err(PrefixRefusal::NotSlashSixtyFour);
	}
	if !option.autonomous && !option.on_link {
		return Err(PrefixRefusal::NothingToDo);
	}
	let valid = Lifetime::from_wire(option.valid_seconds, now_ms);
	let preferred = Lifetime::from_wire(option.preferred_seconds, now_ms);
	Ok(PrefixOutcome { on_link_route: option.on_link.then_some(valid), address: option.autonomous.then_some((preferred, valid)) })
}

/// The two-hour rule of RFC 4862 section 5.5.3.
///
/// A forged advertisement carrying a one-second valid lifetime must not be able to cut off an
/// address this host is using. The rule is not "ignore short lifetimes": a lifetime longer than two
/// hours is honoured whatever it is, and so is one longer than what remains. What it refuses is the
/// case where both are short - and there it leaves two hours rather than the advertised value, so a
/// genuine renumbering still completes, just not instantly.
///
/// Returns the valid lifetime to store.
pub fn apply_two_hour_rule(stored: Lifetime, advertised: Lifetime, now_ms: u64) -> Lifetime {
	let Some(advertised_remaining) = advertised.remaining(now_ms) else {
		// An advertised infinity is longer than anything stored.
		return Lifetime::Infinite;
	};
	let Some(stored_remaining) = stored.remaining(now_ms) else {
		// The stored lifetime is infinite. A finite advertisement is shorter than it by definition,
		// so the rule's two conditions reduce to the first: honour it only if it is over two hours.
		return if advertised_remaining > TWO_HOURS_MS { advertised } else { Lifetime::Finite(now_ms + TWO_HOURS_MS) };
	};
	if advertised_remaining > TWO_HOURS_MS || advertised_remaining > stored_remaining {
		return advertised;
	}
	if stored_remaining <= TWO_HOURS_MS {
		// Already inside the window: the advertisement changes nothing, which is what stops a
		// repeated forgery from ratcheting an address down.
		return stored;
	}
	Lifetime::Finite(now_ms + TWO_HOURS_MS)
}

/// Where an address is in its life.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AddressState {
	/// Detection is running. The address may not be used as a source and does not answer for itself.
	Tentative { probes_sent: u8 },
	/// Proven free and inside its preferred lifetime.
	Preferred,
	/// Past its preferred lifetime: usable for existing work, not chosen for new.
	Deprecated,
	/// Detection found a duplicate. The address is unusable and this host does not answer for it.
	Duplicate,
}

/// One of this host's own addresses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ConfiguredAddress {
	pub interface: Interface,
	pub address: Address,
	pub state: AddressState,
	pub preferred: Lifetime,
	pub valid: Lifetime,
}

impl ConfiguredAddress {
	/// May this address be used as a source for new work?
	pub fn usable_for_new(&self) -> bool {
		self.state == AddressState::Preferred
	}

	/// May this host answer a solicitation for this address, or send from it at all?
	pub fn assigned(&self) -> bool {
		matches!(self.state, AddressState::Preferred | AddressState::Deprecated)
	}
}

/// What happened when detection was driven.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DadOutcome {
	/// Send another solicitation for the tentative address, from the unspecified source.
	Probe { address: Address },
	/// Detection finished with no answer: the address is this host's.
	Assigned { address: Address },
	/// Somebody else holds it. It is marked duplicate and never used.
	Duplicate { address: Address },
	/// Nothing to do.
	Nothing,
}

/// This host's addresses on its interfaces.
#[derive(Debug, Default)]
pub struct AddressSet {
	entries: Vec<ConfiguredAddress>,
	refusals: Refusals,
}

impl AddressSet {
	pub fn new() -> AddressSet {
		AddressSet::default()
	}

	fn find(&mut self, interface: Interface, address: Address) -> Option<&mut ConfiguredAddress> {
		self.entries.iter_mut().find(|entry| entry.interface == interface && entry.address == address)
	}

	/// Form a new address and begin detection. Returns the first probe, or a refusal.
	pub fn add_tentative(&mut self, interface: Interface, address: Address, preferred: Lifetime, valid: Lifetime) -> Result<DadOutcome, Resource> {
		if self.find(interface, address).is_some() {
			return Ok(DadOutcome::Nothing);
		}
		if self.entries.len() as u32 >= Resource::UnicastAddresses.limit() {
			self.refusals.record(Resource::UnicastAddresses);
			return Err(Resource::UnicastAddresses);
		}
		self.entries.push(ConfiguredAddress { interface, address, state: AddressState::Tentative { probes_sent: 1 }, preferred, valid });
		Ok(DadOutcome::Probe { address })
	}

	/// A detection probe's timer expired.
	pub fn on_dad_timeout(&mut self, interface: Interface, address: Address) -> DadOutcome {
		let Some(entry) = self.find(interface, address) else {
			return DadOutcome::Nothing;
		};
		let AddressState::Tentative { probes_sent } = entry.state else {
			return DadOutcome::Nothing;
		};
		if probes_sent >= DUP_ADDR_DETECT_TRANSMITS {
			entry.state = AddressState::Preferred;
			return DadOutcome::Assigned { address };
		}
		entry.state = AddressState::Tentative { probes_sent: probes_sent + 1 };
		DadOutcome::Probe { address }
	}

	/// Somebody answered for a tentative address, or solicited it themselves.
	///
	/// EITHER IS A CONFLICT. An advertisement means another host holds it; a solicitation from a
	/// source of `::` means another host is testing the same address at the same moment, and both of
	/// them must give it up rather than both take it.
	pub fn on_dad_conflict(&mut self, interface: Interface, address: Address) -> DadOutcome {
		let Some(entry) = self.find(interface, address) else {
			return DadOutcome::Nothing;
		};
		if !matches!(entry.state, AddressState::Tentative { .. }) {
			return DadOutcome::Nothing;
		}
		entry.state = AddressState::Duplicate;
		DadOutcome::Duplicate { address }
	}

	/// Refresh an existing autoconfigured address's lifetimes from an advertisement, under the
	/// two-hour rule.
	pub fn refresh(&mut self, interface: Interface, address: Address, preferred: Lifetime, valid: Lifetime, now_ms: u64) -> bool {
		let Some(entry) = self.find(interface, address) else {
			return false;
		};
		entry.valid = apply_two_hour_rule(entry.valid, valid, now_ms);
		entry.preferred = preferred;
		// A refreshed address that had been deprecated becomes preferred again, which is how a
		// renumbering that changes its mind is followed.
		if entry.state == AddressState::Deprecated && !preferred.expired(now_ms) {
			entry.state = AddressState::Preferred;
		}
		true
	}

	/// Move addresses through deprecation and expiry. Returns the addresses that went away.
	pub fn tick(&mut self, now_ms: u64) -> Vec<Address> {
		for entry in self.entries.iter_mut() {
			if entry.state == AddressState::Preferred && entry.preferred.expired(now_ms) {
				entry.state = AddressState::Deprecated;
			}
		}
		let mut gone = Vec::new();
		self.entries.retain(|entry| {
			if entry.valid.expired(now_ms) {
				gone.push(entry.address);
				return false;
			}
			true
		});
		gone
	}

	/// Every address this host answers for: the source-selection candidates and the solicited-node
	/// groups both come from here.
	pub fn assigned(&self, interface: Interface) -> Vec<Address> {
		self.entries.iter().filter(|entry| entry.interface == interface && entry.assigned()).map(|entry| entry.address).collect()
	}

	/// Every record this interface holds, tentative and deprecated included.
	pub fn configured(&self, interface: Interface) -> Vec<ConfiguredAddress> {
		self.entries.iter().filter(|entry| entry.interface == interface).copied().collect()
	}

	pub fn get(&self, interface: Interface, address: Address) -> Option<ConfiguredAddress> {
		self.entries.iter().copied().find(|entry| entry.interface == interface && entry.address == address)
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

/// One recursive DNS server learned from a router advertisement, and who advertised it.
///
/// THE PAIR IS THE IDENTITY, not the server. Two routers may advertise the same server with
/// different lifetimes, and a table keyed on the server alone would let one router's withdrawal take
/// away a server the other is still offering.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RdnssRecord {
	pub interface: Interface,
	pub router: Address,
	pub server: Address,
	pub lifetime: Lifetime,
}

/// The bounded set of recursive DNS servers.
#[derive(Debug, Default)]
pub struct RdnssSet {
	entries: Vec<RdnssRecord>,
	refusals: Refusals,
}

impl RdnssSet {
	pub fn new() -> RdnssSet {
		RdnssSet::default()
	}

	/// Learn or refresh a server from `router`.
	///
	/// A LIFETIME OF ZERO IS A WITHDRAWAL, and it withdraws only that router's offer. Returns
	/// whether anything changed, so a caller knows when to tell its consumers.
	pub fn advertise(&mut self, interface: Interface, router: Address, server: Address, lifetime_seconds: u32, now_ms: u64) -> bool {
		let existing = self.entries.iter().position(|held| held.interface == interface && held.router == router && held.server == server);
		if lifetime_seconds == 0 {
			return match existing {
				Some(index) => {
					self.entries.remove(index);
					true
				}
				None => false,
			};
		}
		let lifetime = Lifetime::from_wire(lifetime_seconds, now_ms);
		if let Some(index) = existing {
			self.entries[index].lifetime = lifetime;
			return true;
		}
		if self.entries.len() as u32 >= Resource::Rdnss.limit() {
			self.refusals.record(Resource::Rdnss);
			return false;
		}
		self.entries.push(RdnssRecord { interface, router, server, lifetime });
		true
	}

	/// Drop records whose lifetime ran out, returning the servers that are no longer offered AT ALL.
	///
	/// A record expiring is not the same as a server going away: another router may still be
	/// offering it, and a consumer told to stop using it would stop for no reason.
	pub fn expire(&mut self, now_ms: u64) -> Vec<Address> {
		let before: Vec<Address> = self.servers();
		self.entries.retain(|record| !record.lifetime.expired(now_ms));
		let after = self.servers();
		before.into_iter().filter(|server| !after.contains(server)).collect()
	}

	/// Every server offered, once each, in the order they were learned.
	///
	/// EXPORTED UNIQUE, because a consumer wants a resolver list rather than a record list: two
	/// routers offering the same server is one server to ask.
	pub fn servers(&self) -> Vec<Address> {
		let mut out: Vec<Address> = Vec::new();
		for record in &self.entries {
			if !out.contains(&record.server) {
				out.push(record.server);
			}
		}
		out
	}

	/// Remove everything an interface learned, for teardown or a generation change.
	pub fn clear_interface(&mut self, interface: Interface) -> usize {
		let before = self.entries.len();
		self.entries.retain(|record| record.interface != interface);
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
