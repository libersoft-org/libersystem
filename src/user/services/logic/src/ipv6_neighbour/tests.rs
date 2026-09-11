// All five states and the transitions between them, driven with no network at all.
//
// The two failures a four-state cache cannot avoid are checked directly: a neighbour that goes away
// is eventually retired, and one that is merely quiet is revalidated rather than discarded.

use super::*;

fn interface() -> Interface {
	Interface::new(0, 1)
}

fn address(low: u16) -> Address {
	let mut bytes = [0u8; 16];
	bytes[0] = 0xfe;
	bytes[1] = 0x80;
	bytes[14..16].copy_from_slice(&low.to_be_bytes());
	Address::new(bytes)
}

fn mac(last: u8) -> [u8; 6] {
	[0x52, 0x54, 0x00, 0x11, 0x22, last]
}

fn state_of(cache: &NeighbourCache, low: u16) -> NeighbourState {
	cache.get(interface(), address(low)).expect("an entry").state
}

#[test]
fn a_first_send_creates_an_incomplete_entry_and_solicits_the_group() {
	let mut cache = NeighbourCache::new();
	let (lookup, action) = cache.resolve(interface(), address(2), 0);
	assert_eq!(lookup, Lookup::Pending, "the packet waits");
	assert_eq!(action, Action::SolicitMulticast { target: address(2) });
	assert_eq!(state_of(&cache, 2), NeighbourState::Incomplete);

	// A second send while resolving does not solicit again: the retransmission timer owns that.
	let (lookup, action) = cache.resolve(interface(), address(2), 10);
	assert_eq!(lookup, Lookup::Pending);
	assert_eq!(action, Action::Nothing);
}

#[test]
fn a_solicited_advertisement_resolves_it_and_an_unsolicited_one_leaves_it_stale() {
	let mut cache = NeighbourCache::new();
	cache.resolve(interface(), address(2), 0);
	let action = cache.on_advertisement(interface(), address(2), mac(1), true, true, false, 100);
	assert_eq!(action, Action::Resolved { target: address(2), link_layer: mac(1) });
	assert_eq!(state_of(&cache, 2), NeighbourState::Reachable);
	assert_eq!(cache.get(interface(), address(2)).expect("entry").deadline_ms, Some(100 + REACHABLE_TIME_MS));

	// The unsolicited case: usable, but nothing proved this host's packets get through.
	let mut other = NeighbourCache::new();
	other.resolve(interface(), address(3), 0);
	let action = other.on_advertisement(interface(), address(3), mac(2), false, true, false, 100);
	assert_eq!(action, Action::Resolved { target: address(3), link_layer: mac(2) });
	assert_eq!(state_of(&other, 3), NeighbourState::Stale, "an unsolicited advertisement is not proof");
}

#[test]
fn an_address_nobody_answers_for_is_retired_after_three_solicitations() {
	let mut cache = NeighbourCache::new();
	cache.resolve(interface(), address(2), 0);
	let mut now = RETRANS_TIMER_MS;
	for attempt in 2..=u64::from(MAX_MULTICAST_SOLICIT) {
		assert_eq!(cache.on_timeout(interface(), address(2), now), Action::SolicitMulticast { target: address(2) }, "solicitation {attempt}");
		now += RETRANS_TIMER_MS;
	}
	assert_eq!(cache.on_timeout(interface(), address(2), now), Action::Retire { target: address(2) });
	assert!(cache.get(interface(), address(2)).is_none(), "and the entry is gone");
	assert!(cache.is_empty());
}

#[test]
fn a_reachable_entry_goes_stale_and_the_next_send_delays_rather_than_stalling() {
	let mut cache = NeighbourCache::new();
	cache.resolve(interface(), address(2), 0);
	cache.on_advertisement(interface(), address(2), mac(1), true, true, false, 0);
	assert_eq!(state_of(&cache, 2), NeighbourState::Reachable);

	// Confirmation runs out.
	assert_eq!(cache.on_timeout(interface(), address(2), REACHABLE_TIME_MS), Action::Nothing);
	assert_eq!(state_of(&cache, 2), NeighbourState::Stale);
	assert_eq!(cache.get(interface(), address(2)).expect("entry").deadline_ms, None, "a stale entry costs no timer");

	// THE PACKET STILL GOES OUT. Refusing to send while revalidating would stall every flow on a
	// link whose neighbours have been quiet.
	let (lookup, action) = cache.resolve(interface(), address(2), REACHABLE_TIME_MS + 1);
	assert_eq!(lookup, Lookup::Ready { link_layer: mac(1) });
	assert_eq!(action, Action::Nothing);
	assert_eq!(state_of(&cache, 2), NeighbourState::Delay);
	assert_eq!(cache.get(interface(), address(2)).expect("entry").deadline_ms, Some(REACHABLE_TIME_MS + 1 + DELAY_FIRST_PROBE_MS));
}

#[test]
fn delay_becomes_probe_and_a_dead_neighbour_is_removed_after_three_probes() {
	let mut cache = NeighbourCache::new();
	cache.resolve(interface(), address(2), 0);
	cache.on_advertisement(interface(), address(2), mac(1), true, true, false, 0);
	cache.on_timeout(interface(), address(2), REACHABLE_TIME_MS);
	cache.resolve(interface(), address(2), REACHABLE_TIME_MS);
	assert_eq!(state_of(&cache, 2), NeighbourState::Delay);

	let mut now = REACHABLE_TIME_MS + DELAY_FIRST_PROBE_MS;
	assert_eq!(cache.on_timeout(interface(), address(2), now), Action::SolicitUnicast { target: address(2), link_layer: mac(1) }, "the first probe is unicast, not a group");
	assert_eq!(state_of(&cache, 2), NeighbourState::Probe);

	for _ in 2..=u64::from(MAX_UNICAST_SOLICIT) {
		now += RETRANS_TIMER_MS;
		assert_eq!(cache.on_timeout(interface(), address(2), now), Action::SolicitUnicast { target: address(2), link_layer: mac(1) });
	}
	now += RETRANS_TIMER_MS;
	assert_eq!(cache.on_timeout(interface(), address(2), now), Action::Retire { target: address(2) });
	assert!(cache.is_empty(), "a neighbour that has gone away is given up");
}

#[test]
fn a_probe_that_is_answered_brings_the_entry_back_without_losing_it() {
	let mut cache = NeighbourCache::new();
	cache.resolve(interface(), address(2), 0);
	cache.on_advertisement(interface(), address(2), mac(1), true, true, false, 0);
	cache.on_timeout(interface(), address(2), REACHABLE_TIME_MS);
	cache.resolve(interface(), address(2), REACHABLE_TIME_MS);
	cache.on_timeout(interface(), address(2), REACHABLE_TIME_MS + DELAY_FIRST_PROBE_MS);
	assert_eq!(state_of(&cache, 2), NeighbourState::Probe);

	let at = REACHABLE_TIME_MS + DELAY_FIRST_PROBE_MS + 100;
	cache.on_advertisement(interface(), address(2), mac(1), true, true, false, at);
	assert_eq!(state_of(&cache, 2), NeighbourState::Reachable);
	assert_eq!(cache.get(interface(), address(2)).expect("entry").attempts, 0, "the probe count resets");
	assert_eq!(cache.get(interface(), address(2)).expect("entry").deadline_ms, Some(at + REACHABLE_TIME_MS));
}

#[test]
fn a_neighbour_that_changed_its_link_layer_address_is_believed_only_with_override() {
	let mut cache = NeighbourCache::new();
	cache.resolve(interface(), address(2), 0);
	cache.on_advertisement(interface(), address(2), mac(1), true, true, false, 0);

	// WITHOUT OVERRIDE: not believed, and not still trusted either. The entry goes stale so the
	// next send probes rather than committing to either address.
	cache.on_advertisement(interface(), address(2), mac(9), false, false, false, 100);
	assert_eq!(cache.get(interface(), address(2)).expect("entry").link_layer, Some(mac(1)), "the claim is not taken");
	assert_eq!(state_of(&cache, 2), NeighbourState::Stale);

	// WITH OVERRIDE: the neighbour is entitled to say it moved.
	cache.on_advertisement(interface(), address(2), mac(9), true, true, false, 200);
	assert_eq!(cache.get(interface(), address(2)).expect("entry").link_layer, Some(mac(9)));
	assert_eq!(state_of(&cache, 2), NeighbourState::Reachable);
}

#[test]
fn an_advertisement_for_an_address_nothing_asked_about_creates_nothing() {
	let mut cache = NeighbourCache::new();
	assert_eq!(cache.on_advertisement(interface(), address(50), mac(5), true, true, false, 0), Action::Nothing);
	assert!(cache.is_empty(), "otherwise anybody on the link fills the cache");
}

#[test]
fn a_solicitation_teaches_the_sender_s_address_as_stale_and_never_as_reachable() {
	let mut cache = NeighbourCache::new();
	assert_eq!(cache.on_solicitation(interface(), address(7), mac(7)), Action::Nothing);
	assert_eq!(state_of(&cache, 7), NeighbourState::Stale, "nothing here proves our packets reach them");
	assert_eq!(cache.get(interface(), address(7)).expect("entry").link_layer, Some(mac(7)));

	// It also completes an entry that was still resolving, which saves the round trip.
	let mut resolving = NeighbourCache::new();
	resolving.resolve(interface(), address(8), 0);
	assert_eq!(resolving.on_solicitation(interface(), address(8), mac(8)), Action::Resolved { target: address(8), link_layer: mac(8) });
	assert_eq!(state_of(&resolving, 8), NeighbourState::Stale);
}

#[test]
fn the_layer_above_can_confirm_reachability_without_a_probe() {
	let mut cache = NeighbourCache::new();
	cache.resolve(interface(), address(2), 0);
	assert!(!cache.confirm_reachable(interface(), address(2), 0), "an INCOMPLETE entry has nothing to confirm");

	cache.on_advertisement(interface(), address(2), mac(1), false, true, false, 0);
	assert_eq!(state_of(&cache, 2), NeighbourState::Stale);
	assert!(cache.confirm_reachable(interface(), address(2), 500), "a transport whose data was acknowledged has proved it");
	assert_eq!(state_of(&cache, 2), NeighbourState::Reachable);
	assert_eq!(cache.get(interface(), address(2)).expect("entry").deadline_ms, Some(500 + REACHABLE_TIME_MS));

	assert!(!cache.confirm_reachable(interface(), address(99), 500), "and an address nothing knows about is not confirmed into existence");
}

#[test]
fn a_full_cache_refuses_rather_than_evicting_a_live_neighbour() {
	let mut cache = NeighbourCache::new();
	let limit = Resource::Neighbours.limit();
	for index in 0..limit as u16 {
		let (lookup, _) = cache.resolve(interface(), address(index), 0);
		assert_eq!(lookup, Lookup::Pending);
	}
	assert_eq!(cache.len(), limit as usize);
	let (lookup, action) = cache.resolve(interface(), address(9999), 0);
	assert_eq!(lookup, Lookup::Capacity, "no live entry was thrown away to make room");
	assert_eq!(action, Action::Nothing);
	assert_eq!(cache.len(), limit as usize);
	assert_eq!(cache.refusals().get(Resource::Neighbours), 1);

	// A CACHE HIT AT CAPACITY IS NOT AN ADMISSION, so it neither costs a slot nor refuses.
	let (lookup, _) = cache.resolve(interface(), address(0), 0);
	assert_eq!(lookup, Lookup::Pending, "the entry it already holds");
	assert_eq!(cache.len(), limit as usize);
	assert_eq!(cache.refusals().get(Resource::Neighbours), 1, "and nothing was refused");

	// A RECLAIMED SLOT ADMITS A NEW NEIGHBOUR. Retirement is the normal way one comes back: three
	// unanswered solicitations remove the entry, and the address behind it can then be resolved.
	assert!(cache.remove(interface(), address(5)));
	assert_eq!(cache.len(), limit as usize - 1);
	let (lookup, action) = cache.resolve(interface(), address(9999), 0);
	assert_eq!(lookup, Lookup::Pending, "the reclaimed slot admits it");
	assert_eq!(action, Action::SolicitMulticast { target: address(9999) });
	assert_eq!(cache.len(), limit as usize);
}

#[test]
fn an_entry_belongs_to_one_interface_generation_and_teardown_takes_them_all() {
	let mut cache = NeighbourCache::new();
	let replaced = Interface::new(0, 2);
	cache.resolve(interface(), address(2), 0);
	cache.resolve(replaced, address(2), 0);
	assert_eq!(cache.len(), 2, "the same address on a new NIC is a new neighbour");

	assert_eq!(cache.clear_interface(interface()), 1);
	assert_eq!(cache.len(), 1);
	assert!(cache.get(replaced, address(2)).is_some());
	assert!(cache.remove(replaced, address(2)));
	assert!(!cache.remove(replaced, address(2)));
}

#[test]
fn due_reports_exactly_the_entries_whose_timer_has_expired() {
	let mut cache = NeighbourCache::new();
	cache.resolve(interface(), address(1), 0);
	cache.resolve(interface(), address(2), 500);
	cache.on_advertisement(interface(), address(2), mac(2), false, true, false, 500);
	assert_eq!(state_of(&cache, 2), NeighbourState::Stale);

	let due = cache.due(RETRANS_TIMER_MS);
	assert_eq!(due, alloc::vec![(interface(), address(1))], "a stale entry has no timer to be due");
	assert!(cache.due(0).is_empty());
}

#[test]
fn a_full_neighbour_table_refuses_a_router_this_host_has_not_yet_resolved() {
	// THE ADMISSION-REFUSAL CASE A HOST WITH NO ROUTER MEETS. Solicitation keeps running while the
	// list is empty, and each answer needs a neighbour entry for the router that sent it. With the
	// table full of live entries there is no slot, and the refusal must be a refusal rather than an
	// eviction: throwing a live neighbour away to make room for an unsolicited advertisement is how
	// a hostile link empties a cache.
	let mut cache = NeighbourCache::new();
	for index in 0..Resource::Neighbours.limit() as u16 {
		cache.resolve(interface(), address(index), 0);
	}
	assert_eq!(cache.len(), Resource::Neighbours.limit() as usize);

	let router = address(0xffff);
	let (lookup, action) = cache.resolve(interface(), router, 0);
	assert_eq!(lookup, Lookup::Capacity);
	assert_eq!(action, Action::Nothing, "and nothing is put on the wire about it");
	assert_eq!(cache.len(), Resource::Neighbours.limit() as usize);
	assert_eq!(cache.refusals().get(Resource::Neighbours), 1);

	// A solicitation FROM that router is refused the same way rather than growing the table.
	assert_eq!(cache.on_solicitation(interface(), router, mac(9)), Action::Nothing);
	assert_eq!(cache.len(), Resource::Neighbours.limit() as usize);
	assert_eq!(cache.refusals().get(Resource::Neighbours), 2);

	// RECLAIM, THEN ADMIT: an entry that probing retires releases its slot, and the router enters.
	let mut now = RETRANS_TIMER_MS;
	for _ in 0..=u64::from(MAX_MULTICAST_SOLICIT) {
		cache.on_timeout(interface(), address(0), now);
		now += RETRANS_TIMER_MS;
	}
	assert_eq!(cache.len(), Resource::Neighbours.limit() as usize - 1);
	let (lookup, action) = cache.resolve(interface(), router, now);
	assert_eq!(lookup, Lookup::Pending);
	assert_eq!(action, Action::SolicitMulticast { target: router });
}
