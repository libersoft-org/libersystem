//! On-link prefixes, their advertisers, and the routes that come out of them.

use super::*;
use alloc::vec;

fn interface() -> Interface {
	Interface::new(0, 1)
}

fn address(low: u16) -> Address {
	let mut bytes = [0u8; 16];
	bytes[0] = 0x20;
	bytes[1] = 0x01;
	bytes[2] = 0x0d;
	bytes[3] = 0xb8;
	bytes[14..16].copy_from_slice(&low.to_be_bytes());
	Address::new(bytes)
}

fn router(low: u16) -> Address {
	let mut bytes = [0u8; 16];
	bytes[0] = 0xfe;
	bytes[1] = 0x80;
	bytes[14..16].copy_from_slice(&low.to_be_bytes());
	Address::new(bytes)
}

/// An address inside the prefix of the same index.
fn inside(index: u16, low: u16) -> Address {
	let mut bytes = [0u8; 16];
	bytes[0] = 0x20;
	bytes[1] = 0x01;
	bytes[2] = 0x0d;
	bytes[3] = 0xb8;
	bytes[4..6].copy_from_slice(&index.to_be_bytes());
	bytes[14..16].copy_from_slice(&low.to_be_bytes());
	Address::new(bytes)
}

/// A distinct /64 per index, so a table can be filled with prefixes that are genuinely different.
fn prefix(index: u16) -> Prefix {
	let mut bytes = [0u8; 16];
	bytes[0] = 0x20;
	bytes[1] = 0x01;
	bytes[2] = 0x0d;
	bytes[3] = 0xb8;
	bytes[4..6].copy_from_slice(&index.to_be_bytes());
	Prefix::new(Address::new(bytes), 64).expect("a /64")
}

fn default_route(next_hop: Address, expires: Lifetime) -> Route {
	Route { interface: interface(), destination: Prefix::new(Address::new([0u8; 16]), 0).expect("::/0"), next_hop: Some(next_hop), kind: RouteKind::Default, expires }
}

#[test]
fn a_prefix_is_unique_by_prefix_and_length_and_a_second_advertisement_updates_it() {
	let mut table = PrefixTable::new();
	assert_eq!(table.advertise(interface(), prefix(1), router(1), Lifetime::Finite(10_000)), Ok(PrefixOutcome::Admitted));
	assert_eq!(table.len(), 1);

	// The SAME router advertising it again is a refresh and costs no slot.
	assert_eq!(table.advertise(interface(), prefix(1), router(1), Lifetime::Finite(60_000)), Ok(PrefixOutcome::Refreshed));
	assert_eq!(table.len(), 1);
	assert_eq!(table.get(interface(), prefix(1)).expect("held").valid, Lifetime::Finite(60_000));
	assert_eq!(table.get(interface(), prefix(1)).expect("held").advertisers(), &[router(1)]);

	// A DIFFERENT router advertising it is a second assertion of the same fact, not a second record.
	assert_eq!(table.advertise(interface(), prefix(1), router(2), Lifetime::Finite(90_000)), Ok(PrefixOutcome::AdvertiserAdded));
	assert_eq!(table.len(), 1);
	assert_eq!(table.get(interface(), prefix(1)).expect("held").advertisers(), &[router(1), router(2)]);
}

#[test]
fn a_prefix_advertised_by_two_routers_survives_one_of_them_withdrawing_it() {
	// THE CASE THE ADVERTISER SET EXISTS FOR. A host that dropped the prefix here would stop
	// treating its own link as its own, and would send every neighbour's traffic to a router.
	let mut table = PrefixTable::new();
	table.advertise(interface(), prefix(1), router(1), Lifetime::Finite(10_000)).expect("room");
	table.advertise(interface(), prefix(1), router(2), Lifetime::Finite(10_000)).expect("room");

	assert_eq!(table.withdraw(interface(), prefix(1), router(1)), Some(PrefixOutcome::Withdrawn));
	assert_eq!(table.len(), 1, "the other router still says it is on-link");
	assert_eq!(table.get(interface(), prefix(1)).expect("held").advertisers(), &[router(2)]);
	assert!(table.on_link(interface(), inside(1, 5), 0), "and it is still on-link");

	assert_eq!(table.withdraw(interface(), prefix(1), router(2)), Some(PrefixOutcome::Removed));
	assert!(table.is_empty(), "the last one leaving takes the prefix with it");
	assert!(!table.on_link(interface(), inside(1, 5), 0));

	// Withdrawing something nothing advertised changes nothing and says so.
	assert_eq!(table.withdraw(interface(), prefix(9), router(1)), None);
}

#[test]
fn a_router_going_away_takes_only_its_own_memberships() {
	let mut table = PrefixTable::new();
	table.advertise(interface(), prefix(1), router(1), Lifetime::Infinite).expect("room");
	table.advertise(interface(), prefix(1), router(2), Lifetime::Infinite).expect("room");
	table.advertise(interface(), prefix(2), router(1), Lifetime::Infinite).expect("room");
	table.advertise(interface(), prefix(3), router(2), Lifetime::Infinite).expect("room");

	let removed = table.withdraw_router(interface(), router(1));
	assert_eq!(removed, vec![prefix(2)], "only the prefix nothing else asserted");
	assert_eq!(table.len(), 2);
	assert_eq!(table.get(interface(), prefix(1)).expect("held").advertisers(), &[router(2)]);
	assert!(table.get(interface(), prefix(3)).is_some());
}

#[test]
fn the_prefix_table_holds_fifteen_and_refuses_the_sixteenth_without_evicting_one() {
	let mut table = PrefixTable::new();
	let limit = Resource::Prefixes.limit();
	for index in 0..limit as u16 {
		assert_eq!(table.advertise(interface(), prefix(index), router(1), Lifetime::Finite(10_000)), Ok(PrefixOutcome::Admitted), "prefix {index}");
	}
	assert_eq!(table.len(), limit as usize);
	assert_eq!(table.advertise(interface(), prefix(999), router(1), Lifetime::Infinite), Err(Resource::Prefixes));
	assert_eq!(table.len(), limit as usize, "no live prefix was thrown away");
	assert_eq!(table.refusals().get(Resource::Prefixes), 1);
	assert!(table.get(interface(), prefix(0)).is_some());

	// A REFRESH AT CAPACITY COSTS NO SLOT, and a slot released by expiry admits the prefix that was
	// refused. Both halves matter: a table that refused for ever once full would leave a renumbered
	// host unable to take the prefix replacing the one it just let go.
	assert_eq!(table.advertise(interface(), prefix(0), router(1), Lifetime::Finite(20_000)), Ok(PrefixOutcome::Refreshed));
	assert_eq!(table.len(), limit as usize);
	assert_eq!(table.expire(10_000), (1..limit as u16).map(prefix).collect::<Vec<_>>(), "every prefix but the refreshed one");
	assert_eq!(table.len(), 1);
	assert_eq!(table.advertise(interface(), prefix(999), router(1), Lifetime::Infinite), Ok(PrefixOutcome::Admitted));
}

#[test]
fn one_prefix_holds_eight_advertisers_and_refuses_the_ninth() {
	let mut table = PrefixTable::new();
	let limit = Resource::AdvertisersPerPrefix.limit();
	table.advertise(interface(), prefix(1), router(0), Lifetime::Infinite).expect("the first");
	for index in 1..limit as u16 {
		assert_eq!(table.advertise(interface(), prefix(1), router(index), Lifetime::Infinite), Ok(PrefixOutcome::AdvertiserAdded), "router {index}");
	}
	assert_eq!(table.get(interface(), prefix(1)).expect("held").advertisers().len(), limit as usize);

	assert_eq!(table.advertise(interface(), prefix(1), router(999), Lifetime::Infinite), Err(Resource::AdvertisersPerPrefix));
	assert_eq!(table.get(interface(), prefix(1)).expect("held").advertisers().len(), limit as usize, "no advertiser was evicted");
	assert_eq!(table.refusals().get(Resource::AdvertisersPerPrefix), 1);
	assert_eq!(table.len(), 1, "and the prefix itself is untouched");

	// A membership released admits the one that was refused.
	assert_eq!(table.withdraw(interface(), prefix(1), router(3)), Some(PrefixOutcome::Withdrawn));
	assert_eq!(table.advertise(interface(), prefix(1), router(999), Lifetime::Infinite), Ok(PrefixOutcome::AdvertiserAdded));
}

#[test]
fn a_prefix_expires_and_stops_being_on_link() {
	let mut table = PrefixTable::new();
	table.advertise(interface(), prefix(0), router(1), Lifetime::Finite(10_000)).expect("room");
	assert!(table.on_link(interface(), address(1), 9_999));
	assert!(!table.on_link(interface(), address(1), 10_000), "expired is not on-link, even before it is swept");
	assert_eq!(table.expire(10_000), vec![prefix(0)]);
	assert!(table.is_empty());

	// An infinite lifetime is not a large number.
	table.advertise(interface(), prefix(0), router(1), Lifetime::Infinite).expect("room");
	assert!(table.expire(u64::MAX - 1).is_empty());
	assert!(table.on_link(interface(), address(1), u64::MAX - 1));

	// A prefix belongs to one interface generation: a replaced NIC holds none of it.
	assert!(!table.on_link(Interface::new(0, 2), address(1), 0));
	assert_eq!(table.clear_interface(interface()), 1);
	assert!(table.is_empty());
}

#[test]
fn a_route_is_keyed_by_destination_and_next_hop_so_two_routers_are_two_routes() {
	let mut table = RouteTable::new();
	assert_eq!(table.install(default_route(router(1), Lifetime::Finite(10_000))), Ok(RouteOutcome::Installed));
	assert_eq!(table.install(default_route(router(2), Lifetime::Finite(10_000))), Ok(RouteOutcome::Installed));
	assert_eq!(table.len(), 2, "the same destination through two routers is two routes");

	assert_eq!(table.install(default_route(router(1), Lifetime::Finite(60_000))), Ok(RouteOutcome::Refreshed));
	assert_eq!(table.len(), 2, "and a refresh costs no slot");

	// Withdrawing one leaves the other's path alone, which is the whole reason the next hop is in
	// the key.
	assert_eq!(table.remove_next_hop(interface(), router(1)), 1);
	assert_eq!(table.len(), 1);
	assert_eq!(table.lookup(interface(), address(9), 0).expect("a route").next_hop, Some(router(2)));
}

#[test]
fn the_longest_prefix_wins_and_an_on_link_destination_needs_no_router() {
	let mut table = RouteTable::new();
	table.install(default_route(router(1), Lifetime::Infinite)).expect("room");
	table.install(Route { interface: interface(), destination: prefix(0), next_hop: None, kind: RouteKind::OnLink, expires: Lifetime::Infinite }).expect("room");
	table.install(Route { interface: interface(), destination: Prefix::link_local(), next_hop: None, kind: RouteKind::LinkLocal, expires: Lifetime::Infinite }).expect("room");

	// An address inside the on-link prefix is a neighbour, and the route that covers it says so by
	// having no next hop.
	let route = table.lookup(interface(), address(1), 0).expect("a route");
	assert_eq!(route.kind, RouteKind::OnLink);
	assert_eq!(route.next_hop, None);

	// Anything else goes to the router.
	let mut elsewhere = [0u8; 16];
	elsewhere[0] = 0x30;
	let route = table.lookup(interface(), Address::new(elsewhere), 0).expect("a route");
	assert_eq!(route.kind, RouteKind::Default);
	assert_eq!(route.next_hop, Some(router(1)));

	// And a link-local destination matches the link-local route rather than the default one.
	assert_eq!(table.lookup(interface(), router(7), 0).expect("a route").kind, RouteKind::LinkLocal);

	// An expired route covers nothing, even before it is swept.
	let mut expiring = RouteTable::new();
	expiring.install(default_route(router(1), Lifetime::Finite(5_000))).expect("room");
	assert!(expiring.lookup(interface(), address(9), 4_999).is_some());
	assert!(expiring.lookup(interface(), address(9), 5_000).is_none());
	assert_eq!(expiring.expire(5_000).len(), 1);
	assert!(expiring.is_empty());
}

#[test]
fn the_route_table_holds_thirty_two_and_refuses_the_thirty_third_without_evicting_one() {
	let mut table = RouteTable::new();
	let limit = Resource::Routes.limit();
	// The eight default routes the router list can hold, and on-link prefixes for the rest.
	for index in 0..8u16 {
		table.install(default_route(router(index), Lifetime::Finite(10_000))).expect("room");
	}
	for index in 0..(limit as u16 - 8) {
		assert_eq!(table.install(Route { interface: interface(), destination: prefix(index), next_hop: None, kind: RouteKind::OnLink, expires: Lifetime::Finite(10_000) }), Ok(RouteOutcome::Installed), "route {index}");
	}
	assert_eq!(table.len(), limit as usize);

	let refused = Route { interface: interface(), destination: prefix(999), next_hop: None, kind: RouteKind::OnLink, expires: Lifetime::Infinite };
	assert_eq!(table.install(refused), Err(Resource::Routes));
	assert_eq!(table.len(), limit as usize, "no live route was thrown away");
	assert_eq!(table.refusals().get(Resource::Routes), 1);
	assert!(table.lookup(interface(), address(1), 0).is_some(), "and every route still resolves");

	// A refresh at capacity costs no slot; a slot released by expiry admits the route that was
	// refused.
	assert_eq!(table.install(default_route(router(0), Lifetime::Infinite)), Ok(RouteOutcome::Refreshed));
	assert_eq!(table.len(), limit as usize);
	assert_eq!(table.expire(10_000).len(), limit as usize - 1, "everything but the refreshed default route");
	assert_eq!(table.install(refused), Ok(RouteOutcome::Installed));
	assert_eq!(table.len(), 2);
}

#[test]
fn routes_belong_to_one_interface_generation() {
	let mut table = RouteTable::new();
	table.install(default_route(router(1), Lifetime::Infinite)).expect("room");
	table.install(Route { interface: Interface::new(0, 2), destination: prefix(0), next_hop: None, kind: RouteKind::OnLink, expires: Lifetime::Infinite }).expect("room");

	let mut elsewhere = [0u8; 16];
	elsewhere[0] = 0x30;
	assert!(table.lookup(Interface::new(0, 2), Address::new(elsewhere), 0).is_none(), "the default route is the older generation's");
	assert_eq!(table.clear_interface(interface()), 1);
	assert_eq!(table.len(), 1, "teardown took that generation's routes and left the other's");
	assert!(table.lookup(Interface::new(0, 2), address(1), 0).is_some());
}
