//! Which of this host's addresses to send from, and which of a name's addresses to send to.
//!
//! "IPv6 FIRST" IS NOT AN ALGORITHM. It has no answer for a host with two global prefixes, for a
//! deprecated address that is still usable for existing work, for a destination with no route, or
//! for the case where the two families would reach different machines. RFC 6724 is the algorithm,
//! and this is it: section 5 for the source, section 6 for the destination, with the DEFAULT policy
//! table verbatim.
//!
//! THE TABLE IS NOT CONFIGURABLE HERE, deliberately. A table an operator can edit is a way to make
//! address selection differ between two machines running one image, and nothing in this appliance
//! needs that.
//!
//! AND THE FINAL TIE-BREAKS ARE MECHANICAL AND TOTAL. Ties that survive every rule used to be
//! decided by insertion order, which is not a rule anybody can rely on and is not the same on two
//! runs. Source ties go to the lower address bytes and then the lower interface identity; route ties
//! go to the longer prefix, then DIRECT before VIA, then the earlier router in P02M0174's frozen
//! order, then the lower prefix bytes, then the lower interface identity. Both are total on any set
//! of distinct candidates, which is the property that makes two runs - and two implementations -
//! choose identically.

use alloc::vec::Vec;

/// One row of RFC 6724 section 2.1's default policy table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Policy {
	pub prefix: [u8; 16],
	pub len: u8,
	pub precedence: u8,
	pub label: u8,
}

const fn prefix(bytes: [u8; 16], len: u8, precedence: u8, label: u8) -> Policy {
	Policy { prefix: bytes, len, precedence, label }
}

/// The default table, verbatim and with no appliance rows.
pub const POLICY_TABLE: [Policy; 9] = [
	// ::1/128
	prefix([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], 128, 50, 0),
	// ::/0
	prefix([0; 16], 0, 40, 1),
	// ::ffff:0:0/96 - every IPv4 destination lands here, which is how one table covers both families
	prefix([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0, 0, 0, 0], 96, 35, 4),
	// 2002::/16
	prefix([0x20, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 16, 30, 2),
	// 2001::/32
	prefix([0x20, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 32, 5, 5),
	// fc00::/7
	prefix([0xfc, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 7, 3, 13),
	// ::/96
	prefix([0; 16], 96, 1, 3),
	// fec0::/10
	prefix([0xfe, 0xc0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 10, 1, 11),
	// 3ffe::/16
	prefix([0x3f, 0xfe, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 16, 1, 12),
];

/// The row that covers an address: the LONGEST matching prefix, which is what makes `::/0` the
/// fallback rather than the answer.
pub fn policy_for(address: [u8; 16]) -> Policy {
	let mut best: Policy = POLICY_TABLE[1];
	for row in POLICY_TABLE {
		if matches_prefix(address, row.prefix, row.len) && row.len >= best.len {
			best = row;
		}
	}
	best
}

fn matches_prefix(address: [u8; 16], prefix: [u8; 16], len: u8) -> bool {
	let whole: usize = (len / 8) as usize;
	if address[..whole] != prefix[..whole] {
		return false;
	}
	let bits: u8 = len % 8;
	if bits == 0 {
		return true;
	}
	let mask: u8 = 0xffu8 << (8 - bits);
	address[whole] & mask == prefix[whole] & mask
}

/// An address's scope, as RFC 6724 section 3 defines it - the multicast scope values, with unicast
/// mapped onto them so one comparison covers both.
pub fn scope_of(address: [u8; 16]) -> u8 {
	if address[0] == 0xff {
		return address[1] & 0x0f;
	}
	if address[0] == 0xfe && address[1] & 0xc0 == 0x80 {
		// Link-local unicast.
		return 2;
	}
	if address[0] == 0xfe && address[1] & 0xc0 == 0xc0 {
		// Site-local unicast, deprecated but still scoped.
		return 5;
	}
	if address == [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1] {
		// The loopback address is interface-local, which is what stops it being chosen for anything
		// that leaves this machine.
		return 1;
	}
	if is_ipv4_mapped(address) {
		// AN IPv4 ADDRESS TAKES ITS SCOPE FROM ITS OWN RULES: 169.254/16 and 127/8 are link-local,
		// everything else is global. Without this every IPv4 address would read as global and a
		// link-local one would be chosen to reach the internet.
		let octets: [u8; 4] = [address[12], address[13], address[14], address[15]];
		if octets[0] == 169 && octets[1] == 254 {
			return 2;
		}
		if octets[0] == 127 {
			return 2;
		}
		return 14;
	}
	14
}

pub fn is_ipv4_mapped(address: [u8; 16]) -> bool {
	address[..10] == [0; 10] && address[10] == 0xff && address[11] == 0xff
}

/// Wrap an IPv4 address in the mapped form, which is how the one policy table covers both families.
pub fn mapped_v4(octets: [u8; 4]) -> [u8; 16] {
	let mut address = [0u8; 16];
	address[10] = 0xff;
	address[11] = 0xff;
	address[12..].copy_from_slice(&octets);
	address
}

/// How many leading bits two addresses share.
pub fn common_prefix_len(a: [u8; 16], b: [u8; 16]) -> u8 {
	let mut bits: u8 = 0;
	for index in 0..16 {
		let differing: u8 = a[index] ^ b[index];
		if differing == 0 {
			bits += 8;
			continue;
		}
		bits += differing.leading_zeros() as u8;
		break;
	}
	bits
}

/// One of this host's addresses, as a source candidate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Source {
	pub address: [u8; 16],
	pub interface_index: u32,
	pub interface_generation: u64,
	/// Past its preferred lifetime: usable for existing work, not chosen for new.
	pub deprecated: bool,
	/// The prefix it was formed under, for the longest-matching-prefix rule.
	pub prefix_len: u8,
}

/// Choose a source for `destination` from this host's candidates.
///
/// RFC 6724 SECTION 5, IN ORDER. Rules 1 to 8, with the ones this appliance has no state for - home
/// addresses, temporary addresses, mobility - simply absent rather than stubbed: a rule with nothing
/// to compare is a rule that changes no outcome, and pretending to apply it would be the pretence.
pub fn select_source<'a>(destination: [u8; 16], candidates: &'a [Source]) -> Option<&'a Source> {
	candidates.iter().reduce(|best, next| if prefers_source(next, best, destination) { next } else { best })
}

fn prefers_source(a: &Source, b: &Source, destination: [u8; 16]) -> bool {
	// Rule 1: prefer the same address.
	let same = (a.address == destination, b.address == destination);
	if same.0 != same.1 {
		return same.0;
	}
	// Rule 2: prefer appropriate scope - the smallest scope at least as large as the destination's,
	// and failing that the largest available.
	let wanted: u8 = scope_of(destination);
	let (scope_a, scope_b) = (scope_of(a.address), scope_of(b.address));
	if scope_a != scope_b {
		if scope_a < wanted || scope_b < wanted {
			return scope_a > scope_b;
		}
		return scope_a < scope_b;
	}
	// Rule 3: avoid deprecated addresses.
	if a.deprecated != b.deprecated {
		return !a.deprecated;
	}
	// Rules 4 and 5 are home and outgoing-interface preferences this appliance has no state for: one
	// interface, no mobility.
	// Rule 6: prefer a matching label.
	let wanted_label: u8 = policy_for(destination).label;
	let matching = (policy_for(a.address).label == wanted_label, policy_for(b.address).label == wanted_label);
	if matching.0 != matching.1 {
		return matching.0;
	}
	// Rule 7 is temporary addresses, which this appliance does not form.
	// Rule 8: use the longest matching prefix.
	let (common_a, common_b) = (common_prefix_len(a.address, destination), common_prefix_len(b.address, destination));
	if common_a != common_b {
		return common_a > common_b;
	}
	// THE FINAL TIE-BREAK, mechanical and total: the lower address bytes, then the lower interface
	// identity. Two candidates equal through all of it are the same candidate.
	if a.address != b.address {
		return a.address < b.address;
	}
	(a.interface_index, a.interface_generation) < (b.interface_index, b.interface_generation)
}

/// A destination with the source this host would use for it, ready to be ordered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Destination {
	pub address: [u8; 16],
	/// The source selected for it, or `None` when this host has no usable path.
	pub source: Option<Source>,
	/// The order the caller supplied it in, which is RFC 6724 section 6's final rule.
	pub supplied: usize,
}

/// Order destinations by RFC 6724 section 6.
///
/// THE SUPPLIED ORDER IS THE LAST RULE AND IT IS PART OF THE ALGORITHM, not a convenience: a caller
/// that listed a preferred address first has said something, and rule 10 is where that survives.
pub fn order_destinations(destinations: &mut [Destination]) {
	destinations.sort_by(|a, b| {
		if prefers_destination(a, b) {
			return core::cmp::Ordering::Less;
		}
		if prefers_destination(b, a) {
			return core::cmp::Ordering::Greater;
		}
		core::cmp::Ordering::Equal
	});
}

fn prefers_destination(a: &Destination, b: &Destination) -> bool {
	// Rule 1: avoid unusable destinations - one with no source has no path at all.
	let usable = (a.source.is_some(), b.source.is_some());
	if usable.0 != usable.1 {
		return usable.0;
	}
	let (Some(source_a), Some(source_b)) = (a.source, b.source) else {
		return a.supplied < b.supplied;
	};
	// Rule 2: prefer matching scope.
	let matching = (scope_of(source_a.address) == scope_of(a.address), scope_of(source_b.address) == scope_of(b.address));
	if matching.0 != matching.1 {
		return matching.0;
	}
	// Rule 3: avoid deprecated addresses.
	if source_a.deprecated != source_b.deprecated {
		return !source_a.deprecated;
	}
	// Rules 4 and 5 are home addresses and outgoing interfaces, which this appliance has none of.
	// Rule 6: prefer matching label.
	let labelled = (policy_for(source_a.address).label == policy_for(a.address).label, policy_for(source_b.address).label == policy_for(b.address).label);
	if labelled.0 != labelled.1 {
		return labelled.0;
	}
	// Rule 7: prefer higher precedence.
	let (precedence_a, precedence_b) = (policy_for(a.address).precedence, policy_for(b.address).precedence);
	if precedence_a != precedence_b {
		return precedence_a > precedence_b;
	}
	// Rule 8: prefer smaller scope.
	let (scope_a, scope_b) = (scope_of(a.address), scope_of(b.address));
	if scope_a != scope_b {
		return scope_a < scope_b;
	}
	// Rule 9: use the longest matching prefix.
	let (common_a, common_b) = (common_prefix_len(source_a.address, a.address), common_prefix_len(source_b.address, b.address));
	if common_a != common_b {
		return common_a > common_b;
	}
	// Rule 10: leave the order unchanged.
	a.supplied < b.supplied
}

/// Where a route sends a packet, for the ordering below.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NextHop {
	/// On-link: the destination is its own next hop.
	Direct,
	/// Through a router, at this position in P02M0174's frozen order.
	Via { router_rank: u32 },
}

/// One route candidate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Route {
	pub prefix: [u8; 16],
	pub prefix_len: u8,
	pub next_hop: NextHop,
	pub interface_index: u32,
	pub interface_generation: u64,
}

/// Choose a route from the candidates that cover a destination.
///
/// THE KEYS ARE FROZEN AND TOTAL: the longer prefix first; then DIRECT before VIA, because an on-link
/// destination needs no router at all; then the earlier router in P02M0174's order - which puts
/// REACHABILITY before advertised preference and is consumed whole rather than rebuilt here, because
/// one seam cannot have two orders; then the lower prefix bytes; then the lower interface identity.
pub fn select_route<'a>(candidates: &'a [Route]) -> Option<&'a Route> {
	candidates.iter().reduce(|best, next| if prefers_route(next, best) { next } else { best })
}

fn prefers_route(a: &Route, b: &Route) -> bool {
	if a.prefix_len != b.prefix_len {
		return a.prefix_len > b.prefix_len;
	}
	let direct = (matches!(a.next_hop, NextHop::Direct), matches!(b.next_hop, NextHop::Direct));
	if direct.0 != direct.1 {
		return direct.0;
	}
	if let (NextHop::Via { router_rank: rank_a }, NextHop::Via { router_rank: rank_b }) = (a.next_hop, b.next_hop)
		&& rank_a != rank_b
	{
		return rank_a < rank_b;
	}
	if a.prefix != b.prefix {
		return a.prefix < b.prefix;
	}
	(a.interface_index, a.interface_generation) < (b.interface_index, b.interface_generation)
}

/// RFC 8028's default-router restriction.
///
/// A SET AND NOT "THE ROUTER". Sections 3.1 to 3.3 are written for the case where several routers
/// advertise one prefix: the choice is restricted to those routers, and P02M0174's order then decides
/// among them. A singular input made the result depend on which advertisement happened to be retained
/// and lost a still-live alternative when that one expired.
///
/// THE ORDER IS CONSUMED WHOLE, NOT REBUILT. P02M0174 orders by reachability first and advertised
/// preference second, deliberately, so an unreachable high-preference router stays behind a reachable
/// lower-preference one. Applying preference here would let an unreachable advertiser win and would
/// give the two sides of one seam different policies.
///
/// `None` means the general default-router rules apply, and the caller RECORDS the fallback rather
/// than taking it silently: every advertiser of the chosen source's prefix expired while the address
/// is still valid on its own lifetime, which is an ordinary state and worth saying.
pub fn default_router(advertisers_in_order: &[u32]) -> Option<u32> {
	advertisers_in_order.first().copied()
}

/// RFC 8028's restriction and P02M0174's order, applied together.
///
/// THE FILTER IS RFC 8028'S AND THE ORDER IS P02M0174'S, and neither is rebuilt here: `ordered` comes
/// from the router list whole, and `advertisers` is the set that actually said the source's prefix is
/// on this link. The first router that is in both is the answer.
///
/// `None` IS THE RECORDED FALLBACK, not a failure: every advertiser of that prefix has expired while
/// the address remains valid on the prefix's own lifetime, so the general default-router rules apply
/// and the caller says so rather than taking them silently.
pub fn default_router_for_source(ordered: &[crate::ipv6_router::Router], advertisers: &[crate::ipv6::Address]) -> Option<crate::ipv6::Address> {
	ordered.iter().find(|router| advertisers.contains(&router.address)).map(|router| router.address)
}

/// Whether a selected route needs a default router chosen for it at all.
///
/// A DIRECT ROUTE NEVER DOES, and must not be filtered through the advertiser set or replaced by an
/// advertising router: RFC 4861 section 5.2 determines the on-link next hop BEFORE default-router
/// selection, and RFC 8028 extends only that later choice.
pub fn needs_default_router(route: &Route) -> bool {
	!matches!(route.next_hop, NextHop::Direct)
}

/// The candidates a caller supplied, in the order this host will try them.
pub fn attempt_order(destinations: &[Destination]) -> Vec<usize> {
	let mut ordered: Vec<Destination> = destinations.to_vec();
	order_destinations(&mut ordered);
	ordered.iter().map(|held| held.supplied).collect()
}

#[cfg(test)]
mod tests;
