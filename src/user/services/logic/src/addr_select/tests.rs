//! The pinned table, the two orderings, and the three cases the tie-break keys exist for.

use super::*;

fn v6(text: &str) -> [u8; 16] {
	// A tiny parser for the fixtures' own literals: groups separated by colons, with `::` once.
	let mut head: Vec<u16> = Vec::new();
	let mut tail: Vec<u16> = Vec::new();
	let mut compressed = false;
	let mut current = &mut head;
	let mut index = 0usize;
	let bytes = text.as_bytes();
	while index < bytes.len() {
		if bytes[index] == b':' {
			if index + 1 < bytes.len() && bytes[index + 1] == b':' {
				compressed = true;
				current = &mut tail;
				index += 2;
				continue;
			}
			index += 1;
			continue;
		}
		let start = index;
		while index < bytes.len() && bytes[index] != b':' {
			index += 1;
		}
		let group = u16::from_str_radix(core::str::from_utf8(&bytes[start..index]).expect("ascii"), 16).expect("hex");
		current.push(group);
	}
	assert!(compressed || head.len() == 8, "{text}");
	let mut groups = [0u16; 8];
	groups[..head.len()].copy_from_slice(&head);
	groups[8 - tail.len()..].copy_from_slice(&tail);
	let mut octets = [0u8; 16];
	for (index, group) in groups.iter().enumerate() {
		octets[2 * index..2 * index + 2].copy_from_slice(&group.to_be_bytes());
	}
	octets
}

fn source(address: [u8; 16]) -> Source {
	Source { address, interface_index: 0, interface_generation: 1, deprecated: false, prefix_len: 64 }
}

#[test]
fn the_policy_table_is_the_default_one_and_the_longest_row_covers_an_address() {
	assert_eq!(POLICY_TABLE.len(), 9, "nine rows, and no appliance rows");
	// The rows an address actually lands on, which is what the precedence and label comparisons read.
	assert_eq!(policy_for(v6("::1")), Policy { prefix: v6("::1"), len: 128, precedence: 50, label: 0 });
	// `2001::/32` IS TEREDO, NOT THE DOCUMENTATION PREFIX. `2001:db8::` is outside it - the first
	// thirty-two bits differ - so it falls to `::/0`, which is the row an ordinary global address
	// lands on and the reason a global IPv6 destination outranks an IPv4 one at all.
	assert_eq!(policy_for(v6("2001::1")).label, 5, "Teredo itself");
	assert_eq!(policy_for(v6("2001::1")).precedence, 5);
	assert_eq!(policy_for(v6("2001:db8::1")).label, 1);
	assert_eq!(policy_for(v6("2001:db8::1")).precedence, 40);
	assert_eq!(policy_for(v6("2002::1")).label, 2);
	assert_eq!(policy_for(v6("fc00::1")).label, 13);
	assert_eq!(policy_for(v6("fe80::1")).label, 1, "link-local falls to ::/0, which the table gives no row of its own");
	assert_eq!(policy_for(mapped_v4([10, 0, 2, 15])).label, 4, "every IPv4 address lands on ::ffff:0:0/96");
	assert_eq!(policy_for(mapped_v4([10, 0, 2, 15])).precedence, 35);
	// A GLOBAL IPv6 ADDRESS OUTRANKS AN IPv4 ONE, which is where "IPv6 first" comes from - as an
	// OUTCOME of the table rather than as the rule.
	assert!(policy_for(v6("2600::1")).precedence > policy_for(mapped_v4([10, 0, 2, 15])).precedence);
}

#[test]
fn scope_is_read_from_the_address_and_ipv4_keeps_its_own_rules() {
	assert_eq!(scope_of(v6("fe80::1")), 2, "link-local");
	assert_eq!(scope_of(v6("2001:db8::1")), 14, "global");
	assert_eq!(scope_of(v6("ff02::1")), 2, "a link-local multicast group");
	assert_eq!(scope_of(v6("ff0e::1")), 14, "a global one");
	assert_eq!(scope_of(v6("::1")), 1, "loopback is interface-local, which keeps it off the wire");
	// WITHOUT THE IPv4 RULES every IPv4 address would read as global and a link-local one would be
	// chosen to reach the internet.
	assert_eq!(scope_of(mapped_v4([169, 254, 1, 1])), 2);
	assert_eq!(scope_of(mapped_v4([127, 0, 0, 1])), 2);
	assert_eq!(scope_of(mapped_v4([10, 0, 2, 15])), 14);
}

#[test]
fn a_source_is_chosen_by_the_rules_in_order() {
	let global = source(v6("2001:db8::1"));
	let link = source(v6("fe80::1"));

	// Rule 2: a link-local destination takes the link-local source, and a global one takes the
	// global source - the smallest scope that is at least the destination's.
	assert_eq!(select_source(v6("fe80::2"), &[global, link]), Some(&link));
	assert_eq!(select_source(v6("2001:db8::2"), &[global, link]), Some(&global));

	// Rule 1: the destination itself, when this host holds it.
	assert_eq!(select_source(v6("2001:db8::1"), &[global, link]), Some(&global));

	// Rule 3: a deprecated address loses to a preferred one of the same scope.
	let mut deprecated = source(v6("2001:db8::2"));
	deprecated.deprecated = true;
	assert_eq!(select_source(v6("2001:db8::9"), &[deprecated, global]), Some(&global));
	// AND IS STILL CHOSEN WHEN IT IS ALL THERE IS, because it is usable for existing work.
	assert_eq!(select_source(v6("2001:db8::9"), &[deprecated]), Some(&deprecated));
}

#[test]
fn rule_six_prefers_a_matching_label_and_rule_eight_the_longest_matching_prefix() {
	// A 2002:: destination and two global sources, one of which is itself 2002:: - the labels decide,
	// and without rule 6 the longest-prefix rule would pick the wrong one.
	let sixtofour = source(v6("2002:1::1"));
	let ordinary = source(v6("2001:db8::1"));
	assert_eq!(select_source(v6("2002:2::1"), &[ordinary, sixtofour]), Some(&sixtofour));

	// Two sources under one label: the one sharing more bits with the destination wins.
	let near = source(v6("2001:db8:0:1::1"));
	let far = source(v6("2001:db8:ffff::1"));
	assert_eq!(select_source(v6("2001:db8:0:1::99"), &[far, near]), Some(&near));
}

#[test]
fn a_source_tie_is_broken_by_the_address_bytes_and_then_the_interface_identity() {
	// TWO RUNS ON ONE MACHINE CHOOSE IDENTICALLY, which insertion order does not give.
	let low = source(v6("2001:db8::1"));
	let high = source(v6("2001:db8::2"));
	assert_eq!(select_source(v6("2001:db8::99"), &[high, low]), Some(&low));
	assert_eq!(select_source(v6("2001:db8::99"), &[low, high]), Some(&low), "and the other way round");

	// Equal as bytes, different interfaces: the lower identity wins, and the same one twice.
	let first = Source { interface_index: 0, interface_generation: 1, ..low };
	let second = Source { interface_index: 0, interface_generation: 2, ..low };
	assert_eq!(select_source(v6("2001:db8::99"), &[second, first]), Some(&first));
	assert_eq!(select_source(v6("2001:db8::99"), &[first, second]), Some(&first));
}

#[test]
fn destinations_are_ordered_by_the_rules_and_ties_keep_the_supplied_order() {
	let global = source(v6("2001:db8::1"));
	let v4 = source(mapped_v4([10, 0, 2, 15]));
	// Rule 1: a destination with no usable source goes last however attractive it looks.
	let mut list = alloc::vec![Destination { address: v6("2600::1"), source: None, supplied: 0 }, Destination { address: mapped_v4([93, 184, 216, 34]), source: Some(v4), supplied: 1 },];
	order_destinations(&mut list);
	assert_eq!(list[0].supplied, 1, "the one this host can actually reach");

	// Rule 7: higher precedence first - which is where "IPv6 first" actually comes from.
	let mut mixed = alloc::vec![Destination { address: mapped_v4([93, 184, 216, 34]), source: Some(v4), supplied: 0 }, Destination { address: v6("2600::1"), source: Some(global), supplied: 1 },];
	order_destinations(&mut mixed);
	assert_eq!(mixed[0].address, v6("2600::1"), "a global IPv6 destination outranks an IPv4 one");

	// Rule 10: everything else equal, the caller's order survives. A caller that listed a preferred
	// address first has said something.
	let mut equal = alloc::vec![Destination { address: v6("2001:db8::a"), source: Some(global), supplied: 0 }, Destination { address: v6("2001:db8::b"), source: Some(global), supplied: 1 },];
	order_destinations(&mut equal);
	assert_eq!(equal.iter().map(|held| held.supplied).collect::<Vec<_>>(), alloc::vec![0, 1]);
}

#[test]
fn a_deprecated_source_pushes_its_destination_down_the_order() {
	// Rule 3 applies to the DESTINATION list too: a destination this host can only reach from a
	// deprecated address is a worse destination than one it can reach from a preferred address.
	let preferred = source(v6("2001:db8::1"));
	let mut deprecated = source(v6("2001:db8::2"));
	deprecated.deprecated = true;
	let mut list = alloc::vec![
		Destination { address: v6("2001:db8::a"), source: Some(deprecated), supplied: 0 },
		Destination { address: v6("2001:db8::b"), source: Some(preferred), supplied: 1 },
	];
	order_destinations(&mut list);
	assert_eq!(list[0].supplied, 1);
}

fn route(prefix_len: u8, next_hop: NextHop) -> Route {
	Route { prefix: v6("2001:db8::"), prefix_len, next_hop, interface_index: 0, interface_generation: 1 }
}

#[test]
fn a_longer_prefix_wins_before_anything_else_is_considered() {
	let specific = route(64, NextHop::Via { router_rank: 5 });
	let general = route(0, NextHop::Direct);
	assert_eq!(select_route(&[general, specific]), Some(&specific));
	assert_eq!(select_route(&[specific, general]), Some(&specific));
}

#[test]
fn a_direct_route_beats_a_via_route_to_the_same_prefix() {
	// THE FIRST OF THE THREE CASES THE KEYS EXIST FOR. An on-link destination needs no router at all,
	// and a key that started at the router would be undefined for the route that names none.
	let direct = route(64, NextHop::Direct);
	let via = route(64, NextHop::Via { router_rank: 0 });
	assert_eq!(select_route(&[via, direct]), Some(&direct));
	assert_eq!(select_route(&[direct, via]), Some(&direct));
	assert!(!needs_default_router(&direct), "and it is not filtered through the advertiser set");
	assert!(needs_default_router(&via));
}

#[test]
fn two_equal_prefix_routes_are_decided_by_the_order_p02m0174_froze() {
	// THE SECOND CASE. That order puts REACHABILITY before advertised preference, so the router this
	// host can actually reach comes first - and consuming it whole is what keeps one seam from having
	// two policies. An address comparison here would let an unreachable router win.
	let reachable = route(64, NextHop::Via { router_rank: 0 });
	let unreachable = route(64, NextHop::Via { router_rank: 1 });
	assert_eq!(select_route(&[unreachable, reachable]), Some(&reachable));
	assert_eq!(select_route(&[reachable, unreachable]), Some(&reachable));
}

#[test]
fn two_routes_differing_only_in_interface_identity_choose_the_same_one_twice() {
	// THE THIRD CASE, and the one that makes the order TOTAL: without this key two distinct records
	// have identical keys and insertion order decides after all.
	let first = Route { interface_index: 0, interface_generation: 1, ..route(64, NextHop::Via { router_rank: 0 }) };
	let second = Route { interface_index: 0, interface_generation: 2, ..first };
	assert_eq!(select_route(&[second, first]), Some(&first));
	assert_eq!(select_route(&[first, second]), Some(&first));
}

#[test]
fn the_default_router_is_the_first_advertiser_of_the_chosen_sources_prefix() {
	// RFC 8028: restricted to the advertisers, and P02M0174's order decides among them. Restricting
	// is this milestone's decision; ordering is not.
	assert_eq!(default_router(&[7, 3, 9]), Some(7));
	// AN EMPTY SET IS AN ORDINARY STATE, not an impossible one: every advertiser can expire while the
	// address is still valid on the prefix's own lifetime. The general rules then apply and the
	// caller records the fallback rather than taking it silently.
	assert_eq!(default_router(&[]), None);
}

#[test]
fn the_attempt_order_is_the_ordered_supplied_indices() {
	let global = source(v6("2001:db8::1"));
	let v4 = source(mapped_v4([10, 0, 2, 15]));
	let list = alloc::vec![
		Destination { address: mapped_v4([93, 184, 216, 34]), source: Some(v4), supplied: 0 },
		Destination { address: v6("2600::1"), source: Some(global), supplied: 1 },
		Destination { address: v6("2600::2"), source: None, supplied: 2 },
	];
	// The reachable IPv6 destination first, the IPv4 one second, and the one with no path last.
	assert_eq!(attempt_order(&list), alloc::vec![1, 0, 2]);
}

/// RFC 8028's default-router restriction, over P02M0174's bounded advertiser set.
///
/// EVERY CASE HERE HAS AN OFF-LINK DESTINATION, so a default router is genuinely required and the
/// restriction is genuinely load-bearing: an on-link destination is its own next hop and never
/// reaches this choice at all. The source is formed from a prefix two routers advertise, which is
/// the situation sections 3.1 to 3.3 are written for.
mod rfc8028 {
	use crate::addr_select::{NextHop, Route, default_router_for_source, needs_default_router, select_route};
	use crate::ipv6::{Address, Interface, Prefix};
	use crate::ipv6_route::{PrefixOutcome, PrefixTable};
	use crate::ipv6_router::{Preference, Reachability, RouterList, RouterOutcome};
	use crate::ipv6_slaac::Lifetime;

	const IFACE: Interface = Interface::new(0, 1);
	/// Long enough that the prefix outlives every router in these cases.
	const PREFIX_SECONDS: u32 = 86_400;

	fn router(low: u16) -> Address {
		let mut bytes = [0u8; 16];
		bytes[0] = 0xfe;
		bytes[1] = 0x80;
		bytes[14..16].copy_from_slice(&low.to_be_bytes());
		Address::new(bytes)
	}

	/// The one /64 both routers advertise.
	fn shared_prefix() -> Prefix {
		let mut bytes = [0u8; 16];
		bytes[..4].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8]);
		Prefix::new(Address::new(bytes), 64).expect("a /64")
	}

	/// A SLAAC source formed from that prefix.
	fn source() -> Address {
		let mut bytes = [0u8; 16];
		bytes[..4].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8]);
		bytes[15] = 0x11;
		Address::new(bytes)
	}

	/// The off-link destination every case is trying to reach.
	fn off_link_route() -> Route {
		Route { prefix: [0u8; 16], prefix_len: 0, next_hop: NextHop::Via { router_rank: 0 }, interface_index: 0, interface_generation: 1 }
	}

	/// A table in which `routers` all advertise the shared prefix.
	fn advertised_by(routers: &[Address], now_ms: u64) -> PrefixTable {
		let mut table = PrefixTable::new();
		for (index, address) in routers.iter().enumerate() {
			let outcome = table.advertise(IFACE, shared_prefix(), *address, Lifetime::from_wire(PREFIX_SECONDS, now_ms)).expect("room for the advertiser");
			let expected = match index {
				0 => PrefixOutcome::Admitted,
				_ => PrefixOutcome::AdvertiserAdded,
			};
			assert_eq!(outcome, expected);
		}
		table
	}

	fn chosen(prefixes: &PrefixTable, routers: &RouterList, now_ms: u64) -> Option<Address> {
		let covering = prefixes.covering(IFACE, source(), now_ms)?;
		default_router_for_source(&routers.ordered(), covering.advertisers())
	}

	#[test]
	fn the_destination_really_does_need_a_default_router() {
		// The premise of every case below. A direct route is decided by RFC 4861 section 5.2 before
		// this choice exists, so filtering it through an advertiser set would be a bug of its own.
		assert!(needs_default_router(&off_link_route()));
		let direct = Route { next_hop: NextHop::Direct, ..off_link_route() };
		assert!(!needs_default_router(&direct));
		assert_eq!(select_route(&[off_link_route(), direct]), Some(&direct), "and direct wins the route decision outright");
	}

	#[test]
	fn two_advertisers_of_one_prefix_differing_only_in_preference_choose_the_higher() {
		let now: u64 = 1_000;
		let (high, low) = (router(0x0002), router(0x0001));
		let prefixes = advertised_by(&[high, low], now);
		let mut routers = RouterList::new();
		assert_eq!(routers.advertise(IFACE, low, Preference::Low, 1800, now), RouterOutcome::Added);
		assert_eq!(routers.advertise(IFACE, high, Preference::High, 1800, now), RouterOutcome::Added);
		// Both usable, so key 1 is a tie and key 2 decides.
		assert!(routers.set_reachability(IFACE, low, Reachability::Usable));
		assert!(routers.set_reachability(IFACE, high, Reachability::Usable));

		// NOTE THE ADDRESSES: the high-preference router has the LARGER link-local address, so a
		// selection that fell through to the tie-break would pick the other one.
		assert_eq!(chosen(&prefixes, &routers, now), Some(high));
	}

	#[test]
	fn an_unreachable_high_preference_advertiser_loses_to_a_reachable_low_preference_one() {
		// THE CASE THAT TELLS THIS RULE APART FROM THE ONE IT REPLACED. P02M0174 orders by
		// reachability FIRST, and this seam consumes that order whole; re-applying preference here
		// would send every packet to a router this host knows it cannot reach.
		let now: u64 = 1_000;
		let (high, low) = (router(0x0001), router(0x0002));
		let prefixes = advertised_by(&[high, low], now);
		let mut routers = RouterList::new();
		routers.advertise(IFACE, high, Preference::High, 1800, now);
		routers.advertise(IFACE, low, Preference::Low, 1800, now);
		assert!(routers.set_reachability(IFACE, high, Reachability::Unusable));
		assert!(routers.set_reachability(IFACE, low, Reachability::Usable));

		assert_eq!(chosen(&prefixes, &routers, now), Some(low), "reachable beats preferred");
		// And when the preferred one becomes reachable again it takes the choice back.
		assert!(routers.set_reachability(IFACE, high, Reachability::Usable));
		assert_eq!(chosen(&prefixes, &routers, now), Some(high));
	}

	#[test]
	fn two_advertisers_with_equal_keys_choose_the_same_one_whatever_order_they_arrived_in() {
		// STABLE RATHER THAN ARRIVAL-DEPENDENT. Two routers with the same reachability and the same
		// preference are separated by their addresses and by nothing else; an implementation that
		// kept "the last advertisement seen" would answer differently on two identical links.
		let now: u64 = 1_000;
		let (first, second) = (router(0x0001), router(0x0002));
		let expected = first;

		let prefixes = advertised_by(&[first, second], now);
		let mut forwards = RouterList::new();
		forwards.advertise(IFACE, first, Preference::Medium, 1800, now);
		forwards.advertise(IFACE, second, Preference::Medium, 1800, now);
		forwards.set_reachability(IFACE, first, Reachability::Usable);
		forwards.set_reachability(IFACE, second, Reachability::Usable);
		assert_eq!(chosen(&prefixes, &forwards, now), Some(expected));

		// The same two links, advertised in the other order.
		let reversed_prefixes = advertised_by(&[second, first], now);
		let mut backwards = RouterList::new();
		backwards.advertise(IFACE, second, Preference::Medium, 1800, now);
		backwards.advertise(IFACE, first, Preference::Medium, 1800, now);
		backwards.set_reachability(IFACE, second, Reachability::Usable);
		backwards.set_reachability(IFACE, first, Reachability::Usable);
		assert_eq!(chosen(&reversed_prefixes, &backwards, now), Some(expected), "the same answer, not the last one heard");
	}

	#[test]
	fn the_selected_advertiser_expiring_hands_the_choice_to_the_survivor() {
		// NOT A FALLBACK TO THE GENERAL RULES. The other advertiser of this prefix is still live, so
		// the restriction still has something to restrict to - which is the whole reason the set is
		// a set and not a single retained router.
		let now: u64 = 1_000;
		let (chosen_router, survivor, unrelated) = (router(0x0001), router(0x0002), router(0x00ff));
		let prefixes = advertised_by(&[chosen_router, survivor], now);
		let mut routers = RouterList::new();
		routers.advertise(IFACE, chosen_router, Preference::Medium, 60, now);
		routers.advertise(IFACE, survivor, Preference::Medium, 1800, now);
		// A router that advertises NOTHING about this prefix, to prove the restriction is applied.
		routers.advertise(IFACE, unrelated, Preference::High, 1800, now);
		for address in [chosen_router, survivor, unrelated] {
			routers.set_reachability(IFACE, address, Reachability::Usable);
		}
		assert_eq!(chosen(&prefixes, &routers, now), Some(chosen_router), "not the high-preference outsider");

		// Its Router Lifetime runs out while the other keeps advertising.
		let later: u64 = now + 61_000;
		assert_eq!(routers.expire(later), alloc::vec![chosen_router]);
		assert_eq!(chosen(&prefixes, &routers, later), Some(survivor));
	}

	#[test]
	fn every_advertiser_going_away_falls_back_to_the_general_rules_and_says_so() {
		// THE PREFIX OUTLIVES ITS ADVERTISERS. The address stays valid on the prefix's own lifetime,
		// so this is an ordinary state rather than an error - and `None` is what makes the caller
		// record the fallback instead of taking it silently.
		let now: u64 = 1_000;
		let (first, second, outsider) = (router(0x0001), router(0x0002), router(0x00ff));
		let prefixes = advertised_by(&[first, second], now);
		let mut routers = RouterList::new();
		routers.advertise(IFACE, first, Preference::Medium, 60, now);
		routers.advertise(IFACE, second, Preference::Medium, 60, now);
		routers.advertise(IFACE, outsider, Preference::High, 1800, now);
		for address in [first, second, outsider] {
			routers.set_reachability(IFACE, address, Reachability::Usable);
		}

		let later: u64 = now + 61_000;
		assert_eq!(routers.expire(later).len(), 2);
		assert_eq!(routers.len(), 1, "the outsider is still a default router");
		// The prefix is still here and the source is still formed from it.
		let covering = prefixes.covering(IFACE, source(), later).expect("the prefix outlives its advertisers");
		assert_eq!(covering.advertisers().len(), 2, "the prefix still records who advertised it");
		assert_eq!(chosen(&prefixes, &routers, later), None, "none of them is a default router any more");
	}

	#[test]
	fn a_destination_on_the_advertised_prefix_is_direct_while_the_advertiser_set_stays_live() {
		// THE DIRECT CASE WITH A LIVE ADVERTISER. A router with a nonzero Router Lifetime advertises
		// an A=1,L=1 /64; a second host sits on that prefix. Its neighbour is resolved and frames go
		// to ITS MAC - the router is not in the path at all, and forwarding being disabled does not
		// stop the exchange. This is a different case from the zero-Router-Lifetime readiness one,
		// which has no advertiser or default router to compete in the first place.
		let now: u64 = 1_000;
		let advertiser = router(0x0001);
		let prefixes = advertised_by(&[advertiser], now);
		let mut routers = RouterList::new();
		assert_eq!(routers.advertise(IFACE, advertiser, Preference::Medium, 1800, now), RouterOutcome::Added);
		routers.set_reachability(IFACE, advertiser, Reachability::Usable);

		// The neighbour on the same /64.
		let mut peer = [0u8; 16];
		peer[..4].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8]);
		peer[15] = 0x22;
		assert!(prefixes.on_link(IFACE, Address::new(peer), now), "L=1 makes it on-link");

		// Both a transport and an internal UDP operation take the same decision, because it is one
		// decision: the on-link route wins outright and needs no router.
		let direct = Route { prefix: peer, prefix_len: 64, next_hop: NextHop::Direct, interface_index: 0, interface_generation: 1 };
		assert_eq!(select_route(&[off_link_route(), direct]), Some(&direct));
		assert!(!needs_default_router(&direct), "the destination is its own next hop");

		// AND THE DEFAULT ROUTER AND ADVERTISER SET REMAIN LIVE. The off-link control still selects
		// an advertiser in P02M0174's order.
		assert_eq!(routers.len(), 1);
		assert!(needs_default_router(&off_link_route()));
		assert_eq!(chosen(&prefixes, &routers, now), Some(advertiser));
	}

	#[test]
	fn a_source_from_no_learned_prefix_has_no_set_to_restrict_to() {
		// A statically configured or link-local source was never delegated by anybody, so RFC 8028
		// has nothing to say about it and the general rules are the only rules.
		let now: u64 = 1_000;
		let prefixes = advertised_by(&[router(0x0001)], now);
		let mut elsewhere = [0u8; 16];
		elsewhere[..4].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb9]);
		assert!(prefixes.covering(IFACE, Address::new(elsewhere), now).is_none());
	}
}
