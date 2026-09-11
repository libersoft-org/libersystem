// Every rule here is written against a router advertisement that is lying, so these tests are mostly
// about what the host REFUSES to do with one.

use super::*;
use crate::ipv6::Address;

fn interface() -> Interface {
	Interface::new(0, 1)
}

fn prefix(third: u16, len: u8) -> Prefix {
	let mut bytes = [0u8; 16];
	bytes[0] = 0x20;
	bytes[1] = 0x01;
	bytes[2] = 0x0d;
	bytes[3] = 0xb8;
	bytes[4..6].copy_from_slice(&third.to_be_bytes());
	Prefix::new(Address::new(bytes), len).expect("a prefix")
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

fn option(len: u8, on_link: bool, autonomous: bool, valid: u32, preferred: u32) -> PrefixInformation {
	PrefixInformation { prefix: prefix(0, len), on_link, autonomous, valid_seconds: valid, preferred_seconds: preferred }
}

#[test]
fn infinity_has_one_representation_and_is_selected_by_the_wire_value() {
	assert_eq!(Lifetime::from_wire(INFINITE_LIFETIME, 1000), Lifetime::Infinite);
	assert_eq!(Lifetime::from_wire(INFINITE_LIFETIME - 1, 0), Lifetime::Finite(4_294_967_294_000), "a very large finite value is still finite");
	assert_eq!(Lifetime::from_wire(30, 1000), Lifetime::Finite(31_000));
	assert!(!Lifetime::Infinite.expired(u64::MAX));
	assert_eq!(Lifetime::Infinite.remaining(0), None, "infinity is not a large number");
	assert!(Lifetime::Finite(1000).expired(1000));
	assert!(!Lifetime::Finite(1001).expired(1000));
	assert_eq!(Lifetime::Finite(1500).remaining(1000), Some(500));
	assert_eq!(Lifetime::Finite(500).remaining(1000), Some(0), "saturating, not negative");
}

#[test]
fn a_preferred_lifetime_longer_than_the_valid_one_discards_the_whole_option() {
	let bad = option(64, true, true, 100, 200);
	assert_eq!(evaluate_prefix(&bad, 0), Err(PrefixRefusal::PreferredExceedsValid), "not clamped into something plausible");
	// Equal is fine.
	assert!(evaluate_prefix(&option(64, true, true, 100, 100), 0).is_ok());
	// And infinity compares correctly on the wire values.
	assert!(evaluate_prefix(&option(64, true, true, INFINITE_LIFETIME, 100), 0).is_ok());
	assert_eq!(evaluate_prefix(&option(64, true, true, 100, INFINITE_LIFETIME), 0), Err(PrefixRefusal::PreferredExceedsValid));
}

#[test]
fn the_autonomous_flag_forms_an_address_only_from_a_slash_sixty_four() {
	assert_eq!(evaluate_prefix(&option(56, false, true, 100, 100), 0), Err(PrefixRefusal::NotSlashSixtyFour), "not truncated into an address");
	assert_eq!(evaluate_prefix(&option(80, false, true, 100, 100), 0), Err(PrefixRefusal::NotSlashSixtyFour), "and not padded either");

	// WITHOUT `A`, ANY LENGTH IS A LEGITIMATE ON-LINK PREFIX. The two flags are separate decisions.
	let on_link_only = evaluate_prefix(&option(56, true, false, 100, 100), 0).expect("a /56 is a fine on-link prefix");
	assert!(on_link_only.on_link_route.is_some());
	assert!(on_link_only.address.is_none());
}

#[test]
fn the_two_flags_are_evaluated_independently() {
	let both = evaluate_prefix(&option(64, true, true, 200, 100), 1000).expect("valid");
	assert_eq!(both.on_link_route, Some(Lifetime::Finite(201_000)));
	assert_eq!(both.address, Some((Lifetime::Finite(101_000), Lifetime::Finite(201_000))));

	let address_only = evaluate_prefix(&option(64, false, true, 200, 100), 0).expect("valid");
	assert!(address_only.on_link_route.is_none(), "L=0 installs no route even though A=1 forms an address");
	assert!(address_only.address.is_some());

	assert_eq!(evaluate_prefix(&option(64, false, false, 200, 100), 0), Err(PrefixRefusal::NothingToDo));
}

#[test]
fn the_link_local_prefix_may_not_be_advertised() {
	let mut bytes = [0u8; 16];
	bytes[0] = 0xfe;
	bytes[1] = 0x80;
	let link_local = PrefixInformation { prefix: Prefix::new(Address::new(bytes), 64).expect("prefix"), on_link: true, autonomous: true, valid_seconds: 100, preferred_seconds: 100 };
	assert_eq!(evaluate_prefix(&link_local, 0), Err(PrefixRefusal::LinkLocalPrefix), "a host's own detection proved that address, not a router");
}

#[test]
fn the_two_hour_rule_refuses_an_instant_invalidation_but_honours_a_genuine_renumbering() {
	let now = 1_000_000u64;
	let day = 24 * 60 * 60 * 1000u64;
	let stored = Lifetime::Finite(now + day);

	// A FORGED ONE-SECOND LIFETIME cannot cut off an address in use: two hours is left instead.
	let forged = Lifetime::Finite(now + 1000);
	assert_eq!(apply_two_hour_rule(stored, forged, now), Lifetime::Finite(now + TWO_HOURS_MS));

	// A lifetime over two hours is honoured whatever it is, including a reduction.
	let three_hours = Lifetime::Finite(now + 3 * 60 * 60 * 1000);
	assert_eq!(apply_two_hour_rule(stored, three_hours, now), three_hours);

	// A lifetime longer than what remains is honoured.
	let two_days = Lifetime::Finite(now + 2 * day);
	assert_eq!(apply_two_hour_rule(stored, two_days, now), two_days);

	// ALREADY INSIDE THE WINDOW: the advertisement changes nothing, so a repeated forgery cannot
	// ratchet the address down a second at a time.
	let nearly_over = Lifetime::Finite(now + 60_000);
	assert_eq!(apply_two_hour_rule(nearly_over, forged, now), nearly_over);
	assert_eq!(apply_two_hour_rule(nearly_over, Lifetime::Finite(now + 30_000), now), nearly_over);

	// An advertised infinity is longer than anything stored.
	assert_eq!(apply_two_hour_rule(stored, Lifetime::Infinite, now), Lifetime::Infinite);
	// A finite advertisement against a stored infinity: under two hours leaves two hours, over two
	// hours is honoured. That is the transition between the two representations, and it is defined.
	assert_eq!(apply_two_hour_rule(Lifetime::Infinite, forged, now), Lifetime::Finite(now + TWO_HOURS_MS));
	assert_eq!(apply_two_hour_rule(Lifetime::Infinite, three_hours, now), three_hours);
}

#[test]
fn an_address_is_tentative_until_detection_finishes() {
	let mut set = AddressSet::new();
	let outcome = set.add_tentative(interface(), address(1), Lifetime::Finite(10_000), Lifetime::Finite(20_000)).expect("room");
	assert_eq!(outcome, DadOutcome::Probe { address: address(1) });
	let entry = set.get(interface(), address(1)).expect("an entry");
	assert_eq!(entry.state, AddressState::Tentative { probes_sent: 1 });
	assert!(!entry.assigned(), "a tentative address does not answer for itself");
	assert!(!entry.usable_for_new());
	assert!(set.assigned(interface()).is_empty());

	assert_eq!(set.on_dad_timeout(interface(), address(1)), DadOutcome::Assigned { address: address(1) });
	let entry = set.get(interface(), address(1)).expect("an entry");
	assert_eq!(entry.state, AddressState::Preferred);
	assert!(entry.assigned() && entry.usable_for_new());
	assert_eq!(set.assigned(interface()), alloc::vec![address(1)]);
}

#[test]
fn a_duplicate_is_never_used_and_a_conflict_after_assignment_is_ignored() {
	let mut set = AddressSet::new();
	set.add_tentative(interface(), address(1), Lifetime::Infinite, Lifetime::Infinite).expect("room");
	assert_eq!(set.on_dad_conflict(interface(), address(1)), DadOutcome::Duplicate { address: address(1) });
	let entry = set.get(interface(), address(1)).expect("an entry");
	assert_eq!(entry.state, AddressState::Duplicate);
	assert!(!entry.assigned(), "this host does not answer for an address somebody else holds");
	assert_eq!(set.on_dad_timeout(interface(), address(1)), DadOutcome::Nothing, "a duplicate does not become assigned by a timer");

	// A conflict reported about an address that finished detection is not a reason to give it up
	// here: that is a different problem, and this path is only about the tentative window.
	let mut assigned = AddressSet::new();
	assigned.add_tentative(interface(), address(2), Lifetime::Infinite, Lifetime::Infinite).expect("room");
	assigned.on_dad_timeout(interface(), address(2));
	assert_eq!(assigned.on_dad_conflict(interface(), address(2)), DadOutcome::Nothing);
	assert_eq!(assigned.get(interface(), address(2)).expect("entry").state, AddressState::Preferred);
}

#[test]
fn an_address_deprecates_before_it_expires() {
	let mut set = AddressSet::new();
	set.add_tentative(interface(), address(1), Lifetime::Finite(10_000), Lifetime::Finite(20_000)).expect("room");
	set.on_dad_timeout(interface(), address(1));

	assert!(set.tick(9_999).is_empty());
	assert_eq!(set.get(interface(), address(1)).expect("entry").state, AddressState::Preferred);

	assert!(set.tick(10_000).is_empty());
	let entry = set.get(interface(), address(1)).expect("entry");
	assert_eq!(entry.state, AddressState::Deprecated);
	assert!(entry.assigned(), "existing work keeps using it");
	assert!(!entry.usable_for_new(), "but nothing new chooses it");

	assert_eq!(set.tick(20_000), alloc::vec![address(1)]);
	assert!(set.get(interface(), address(1)).is_none());
}

#[test]
fn a_refresh_runs_the_two_hour_rule_and_can_undeprecate_an_address() {
	let now = 1_000_000u64;
	let mut set = AddressSet::new();
	let day = 24 * 60 * 60 * 1000u64;
	set.add_tentative(interface(), address(1), Lifetime::Finite(now + 1000), Lifetime::Finite(now + day)).expect("room");
	set.on_dad_timeout(interface(), address(1));
	set.tick(now + 1000);
	assert_eq!(set.get(interface(), address(1)).expect("entry").state, AddressState::Deprecated);

	// A genuine advertisement with a fresh preferred lifetime brings it back.
	assert!(set.refresh(interface(), address(1), Lifetime::Finite(now + day), Lifetime::Finite(now + 2 * day), now));
	let entry = set.get(interface(), address(1)).expect("entry");
	assert_eq!(entry.state, AddressState::Preferred);
	assert_eq!(entry.valid, Lifetime::Finite(now + 2 * day));

	// A forged short valid lifetime is held off by the rule.
	assert!(set.refresh(interface(), address(1), Lifetime::Finite(now + 10), Lifetime::Finite(now + 10), now));
	assert_eq!(set.get(interface(), address(1)).expect("entry").valid, Lifetime::Finite(now + TWO_HOURS_MS));

	assert!(!set.refresh(interface(), address(99), Lifetime::Infinite, Lifetime::Infinite, now), "an address nothing configured is not refreshed into existence");
}

#[test]
fn the_address_set_is_bounded_and_refuses_rather_than_evicting() {
	let mut set = AddressSet::new();
	for index in 0..Resource::UnicastAddresses.limit() as u16 {
		set.add_tentative(interface(), address(index), Lifetime::Infinite, Lifetime::Infinite).expect("room");
	}
	assert_eq!(set.len(), 16, "fifteen SLAAC slots and one link-local");
	assert_eq!(set.add_tentative(interface(), address(999), Lifetime::Infinite, Lifetime::Infinite), Err(Resource::UnicastAddresses));
	assert_eq!(set.len(), 16, "no live address was thrown away");
	assert_eq!(set.refusals().get(Resource::UnicastAddresses), 1);

	// Adding one that is already there is not an admission and costs no slot.
	assert_eq!(set.add_tentative(interface(), address(0), Lifetime::Infinite, Lifetime::Infinite), Ok(DadOutcome::Nothing));

	// A REFRESH AT CAPACITY COSTS NO SLOT, and a slot released by removal admits a new address. The
	// pair matters together: a table that refused for ever once full would leave a renumbered host
	// unable to take the prefix that replaced the one it just let go.
	assert!(set.refresh(interface(), address(0), Lifetime::Finite(1_000), Lifetime::Finite(2_000), 0));
	assert_eq!(set.len(), 16);
	assert_eq!(set.add_tentative(interface(), address(999), Lifetime::Infinite, Lifetime::Infinite), Err(Resource::UnicastAddresses));

	assert!(set.remove(interface(), address(3)));
	assert_eq!(set.len(), 15);
	assert_eq!(set.add_tentative(interface(), address(999), Lifetime::Infinite, Lifetime::Infinite), Ok(DadOutcome::Probe { address: address(999) }), "the reclaimed slot admits it");
	assert_eq!(set.len(), 16);

	// And expiry is the other way a slot comes back: it releases exactly the one that ran out. The
	// deadline is the two-hour rule's and not the advertised one - the refresh above tried to cut an
	// infinite lifetime to two seconds, which is the shortening that rule refuses.
	assert!(set.tick(TWO_HOURS_MS - 1).is_empty(), "still inside the floor the rule imposed");
	assert_eq!(set.tick(TWO_HOURS_MS), alloc::vec![address(0)]);
	assert_eq!(set.len(), 15);
	assert_eq!(set.add_tentative(interface(), address(998), Lifetime::Infinite, Lifetime::Infinite), Ok(DadOutcome::Probe { address: address(998) }));
}

#[test]
fn addresses_belong_to_one_interface_generation() {
	let mut set = AddressSet::new();
	let replaced = Interface::new(0, 2);
	set.add_tentative(interface(), address(1), Lifetime::Infinite, Lifetime::Infinite).expect("room");
	set.add_tentative(replaced, address(1), Lifetime::Infinite, Lifetime::Infinite).expect("room");
	assert_eq!(set.len(), 2, "a replaced NIC re-runs detection rather than inheriting");
	assert_eq!(set.clear_interface(interface()), 1);
	assert_eq!(set.len(), 1);
	assert!(set.remove(replaced, address(1)));
	assert!(!set.remove(replaced, address(1)));
}

fn router(low: u16) -> Address {
	let mut bytes = [0u8; 16];
	bytes[0] = 0xfe;
	bytes[1] = 0x80;
	bytes[14..16].copy_from_slice(&low.to_be_bytes());
	Address::new(bytes)
}

#[test]
fn a_recursive_server_is_keyed_on_the_pair_so_one_router_cannot_withdraw_anothers_offer() {
	let mut set = RdnssSet::new();
	assert!(set.advertise(interface(), router(1), address(0x53), 600, 0));
	assert!(set.advertise(interface(), router(2), address(0x53), 600, 0));
	assert_eq!(set.len(), 2, "two records");
	assert_eq!(set.servers(), alloc::vec![address(0x53)], "one server: a resolver list, not a record list");

	// ONE ROUTER WITHDRAWS. The server is still offered by the other, so nothing is taken away.
	assert!(set.advertise(interface(), router(1), address(0x53), 0, 1_000));
	assert_eq!(set.len(), 1);
	assert_eq!(set.servers(), alloc::vec![address(0x53)]);

	// The second withdrawal is the one that removes it.
	assert!(set.advertise(interface(), router(2), address(0x53), 0, 2_000));
	assert!(set.is_empty());
	assert!(set.servers().is_empty());
	assert!(!set.advertise(interface(), router(2), address(0x53), 0, 3_000), "withdrawing what is not there changes nothing");
}

#[test]
fn expiry_reports_only_the_servers_that_are_no_longer_offered_at_all() {
	let mut set = RdnssSet::new();
	set.advertise(interface(), router(1), address(0x53), 10, 0);
	set.advertise(interface(), router(2), address(0x53), 60, 0);
	set.advertise(interface(), router(1), address(0x54), 10, 0);

	// The first router's records run out. `0x53` is still offered by the second, so only `0x54` is
	// reported gone - a consumer told to stop using `0x53` would stop for no reason.
	assert_eq!(set.expire(10_000), alloc::vec![address(0x54)]);
	assert_eq!(set.servers(), alloc::vec![address(0x53)]);
	assert_eq!(set.expire(60_000), alloc::vec![address(0x53)]);
	assert!(set.is_empty());
}

#[test]
fn a_refreshed_record_outlives_its_original_lifetime_and_an_infinite_one_never_expires() {
	let mut set = RdnssSet::new();
	set.advertise(interface(), router(1), address(0x53), 10, 0);
	set.advertise(interface(), router(1), address(0x53), 60, 5_000);
	assert!(set.expire(10_000).is_empty(), "the refresh moved its deadline");
	assert_eq!(set.expire(65_001), alloc::vec![address(0x53)]);

	let mut forever = RdnssSet::new();
	forever.advertise(interface(), router(1), address(0x53), INFINITE_LIFETIME, 0);
	assert!(forever.expire(u64::MAX - 1).is_empty(), "an infinite lifetime is not a large number");
}

#[test]
fn the_recursive_server_set_is_bounded_and_refuses_rather_than_evicting() {
	let mut set = RdnssSet::new();
	for index in 0..Resource::Rdnss.limit() as u16 {
		assert!(set.advertise(interface(), router(index), address(0x53 + index), 600, 0), "record {index}");
	}
	assert_eq!(set.len(), 4);
	assert!(!set.advertise(interface(), router(99), address(0x99), 600, 0), "the fifth is refused");
	assert_eq!(set.len(), 4, "and no live record was thrown away");
	assert_eq!(set.refusals().get(Resource::Rdnss), 1);

	// A refresh at capacity costs no slot; a released slot admits a new record.
	assert!(set.advertise(interface(), router(0), address(0x53), 1200, 0));
	set.advertise(interface(), router(0), address(0x53), 0, 0);
	assert!(set.advertise(interface(), router(99), address(0x99), 600, 0), "the reclaimed slot admits it");

	// AND THE BOUND IS THE WHOLE SET'S, not one interface's: a replaced NIC cannot use a slot the
	// full table does not have.
	let replaced = Interface::new(0, 2);
	assert!(!set.advertise(replaced, router(1), address(0x53), 600, 0), "the table is full again");
	assert_eq!(set.clear_interface(interface()), 4, "teardown takes its own link's records");
	assert!(set.is_empty());
	assert!(set.advertise(replaced, router(1), address(0x53), 600, 0), "and the new generation can then learn");
	assert_eq!(set.clear_interface(interface()), 0, "which the old link's teardown does not touch");
}
