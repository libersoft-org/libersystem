// The order is three keys and nothing else, and the case the dimension order exists for is the one
// where an unreachable high-preference router must NOT come first.

use super::*;
use crate::ipv6_neighbour::NeighbourState;

fn interface() -> Interface {
	Interface::new(0, 1)
}

fn router(low: u16) -> Address {
	let mut bytes = [0u8; 16];
	bytes[0] = 0xfe;
	bytes[1] = 0x80;
	bytes[14..16].copy_from_slice(&low.to_be_bytes());
	Address::new(bytes)
}

#[test]
fn the_reserved_preference_value_reads_as_medium() {
	assert_eq!(Preference::from_bits(0b01), Preference::High);
	assert_eq!(Preference::from_bits(0b00), Preference::Medium);
	assert_eq!(Preference::from_bits(0b11), Preference::Low);
	assert_eq!(Preference::from_bits(0b10), Preference::Medium, "reserved is medium, not a fourth rank");
}

#[test]
fn the_four_usable_nud_states_are_one_class() {
	for state in [NeighbourState::Reachable, NeighbourState::Stale, NeighbourState::Delay, NeighbourState::Probe] {
		assert_eq!(Reachability::of(state), Reachability::Usable, "{state:?}");
	}
	assert_eq!(Reachability::of(NeighbourState::Incomplete), Reachability::Unusable);
}

#[test]
fn a_usable_low_preference_router_precedes_an_unreachable_high_preference_one() {
	let mut list = RouterList::new();
	list.advertise(interface(), router(1), Preference::High, 1800, 0);
	list.advertise(interface(), router(2), Preference::Low, 1800, 0);
	list.set_reachability(interface(), router(1), Reachability::Unusable);
	list.set_reachability(interface(), router(2), Reachability::Usable);

	let order = list.ordered();
	assert_eq!(order[0].address, router(2), "key 1 comes first: a router nothing can reach is not a candidate to prefer");
	assert_eq!(order[1].address, router(1));
}

#[test]
fn a_high_preference_stale_router_precedes_a_low_preference_reachable_one() {
	// THE 2026-09-08 CORRECTION, checked directly: there is no ranking inside the usable class, so
	// recent confirmation does not outrank advertised preference.
	let mut list = RouterList::new();
	list.advertise(interface(), router(1), Preference::High, 1800, 0);
	list.advertise(interface(), router(2), Preference::Low, 1800, 0);
	list.set_reachability(interface(), router(1), Reachability::of(NeighbourState::Stale));
	list.set_reachability(interface(), router(2), Reachability::of(NeighbourState::Reachable));

	let order = list.ordered();
	assert_eq!(order[0].address, router(1), "preference decides among usable routers");
}

#[test]
fn differing_usable_states_do_not_reorder_equal_preferences() {
	// If an extra rank had crept inside key 1, these four would come out in state order rather than
	// in address order.
	let mut list = RouterList::new();
	let states = [NeighbourState::Probe, NeighbourState::Delay, NeighbourState::Stale, NeighbourState::Reachable];
	for (index, state) in states.iter().enumerate() {
		let address = router(index as u16 + 1);
		list.advertise(interface(), address, Preference::Medium, 1800, 0);
		list.set_reachability(interface(), address, Reachability::of(*state));
	}
	let order = list.ordered();
	for (index, entry) in order.iter().enumerate() {
		assert_eq!(entry.address, router(index as u16 + 1), "position {index} is the address order, not the state order");
	}
}

#[test]
fn two_routers_equal_in_both_keys_are_ordered_by_address_and_the_same_way_twice() {
	let mut list = RouterList::new();
	list.advertise(interface(), router(9), Preference::Medium, 1800, 0);
	list.advertise(interface(), router(3), Preference::Medium, 1800, 0);
	list.set_reachability(interface(), router(9), Reachability::Usable);
	list.set_reachability(interface(), router(3), Reachability::Usable);

	let first = list.ordered();
	assert_eq!(first[0].address, router(3), "ascending by the sixteen octets");
	let second = list.ordered();
	assert_eq!(first.iter().map(|entry| entry.address).collect::<Vec<_>>(), second.iter().map(|entry| entry.address).collect::<Vec<_>>(), "stable across calls");
}

#[test]
fn the_order_is_total_over_all_three_keys_at_once() {
	let mut list = RouterList::new();
	// Two unusable, two usable; within each, one high and one medium; the medium pair shares a
	// preference so the address decides.
	let plan = [
		(1u16, Preference::Medium, Reachability::Usable),
		(2, Preference::High, Reachability::Usable),
		(3, Preference::High, Reachability::Unusable),
		(4, Preference::Medium, Reachability::Unusable),
	];
	for (low, preference, reachability) in plan {
		list.advertise(interface(), router(low), preference, 1800, 0);
		list.set_reachability(interface(), router(low), reachability);
	}
	let order: Vec<u16> = list.ordered().iter().map(|entry| u16::from_be_bytes([entry.address.octets()[14], entry.address.octets()[15]])).collect();
	assert_eq!(order, alloc::vec![2, 1, 3, 4], "usable high, usable medium, then unusable high, unusable medium");
}

#[test]
fn a_zero_lifetime_withdraws_a_router_and_expiry_removes_one() {
	let mut list = RouterList::new();
	assert_eq!(list.advertise(interface(), router(1), Preference::Medium, 1800, 0), RouterOutcome::Added);
	assert_eq!(list.advertise(interface(), router(1), Preference::High, 3600, 1000), RouterOutcome::Refreshed);
	assert_eq!(list.ordered()[0].preference, Preference::High);

	assert_eq!(list.advertise(interface(), router(1), Preference::High, 0, 2000), RouterOutcome::Withdrawn);
	assert!(list.is_empty(), "a router may withdraw itself");
	assert_eq!(list.advertise(interface(), router(1), Preference::High, 0, 2000), RouterOutcome::NotPresent);

	// The other way the list empties.
	list.advertise(interface(), router(2), Preference::Medium, 10, 0);
	assert!(list.expire(9_999).is_empty(), "not yet");
	assert_eq!(list.expire(10_000), alloc::vec![router(2)]);
	assert!(list.is_empty());
}

#[test]
fn the_list_holds_eight_and_refuses_the_ninth_without_evicting_a_live_router() {
	let mut list = RouterList::new();
	for index in 0..Resource::DefaultRouters.limit() as u16 {
		assert_eq!(list.advertise(interface(), router(index), Preference::Medium, 1800, 0), RouterOutcome::Added);
	}
	assert_eq!(list.len(), 8, "RFC 4861 asks for at least two; this holds eight");
	assert_eq!(list.advertise(interface(), router(99), Preference::High, 1800, 0), RouterOutcome::Capacity, "a high preference does not buy a slot");
	assert_eq!(list.len(), 8);
	assert_eq!(list.refusals().get(Resource::DefaultRouters), 1);

	// AN EXISTING ROUTER STILL REFRESHES while the list is full: it costs no slot.
	assert_eq!(list.advertise(interface(), router(0), Preference::Low, 3600, 0), RouterOutcome::Refreshed);
}

#[test]
fn a_router_belongs_to_one_interface_generation() {
	let mut list = RouterList::new();
	let replaced = Interface::new(0, 2);
	list.advertise(interface(), router(1), Preference::Medium, 1800, 0);
	list.advertise(replaced, router(1), Preference::Medium, 1800, 0);
	assert_eq!(list.len(), 2);
	assert_eq!(list.clear_interface(interface()), 1);
	assert_eq!(list.len(), 1);
}
