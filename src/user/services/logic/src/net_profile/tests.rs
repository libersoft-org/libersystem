//! The three profiles, the four readiness states, and the partition that refuses before it sends.

use super::*;

#[test]
fn the_key_has_three_values_and_anything_else_is_dual() {
	assert_eq!(Families::parse(Some("ipv4")), Families::Ipv4);
	assert_eq!(Families::parse(Some("ipv6")), Families::Ipv6);
	assert_eq!(Families::parse(Some("dual")), Families::Dual);
	// AN ABSENT OR UNPARSEABLE KEY IS `dual`, which is what the machine does today extended to the
	// second family - not an error and not a silent single-family boot.
	assert_eq!(Families::parse(None), Families::Dual);
	assert_eq!(Families::parse(Some("")), Families::Dual);
	assert_eq!(Families::parse(Some("both")), Families::Dual);
	assert_eq!(Families::parse(Some("IPV4")), Families::Dual, "the value is read exactly, not case-folded");

	assert!(Families::Ipv4.includes_v4() && !Families::Ipv4.includes_v6());
	assert!(!Families::Ipv6.includes_v4() && Families::Ipv6.includes_v6());
	assert!(Families::Dual.includes_v4() && Families::Dual.includes_v6());
}

#[test]
fn a_family_outside_the_profile_is_disabled_and_nothing_else() {
	assert_eq!(readiness(false, FamilyState::default()), Readiness::Disabled);
	// Even one that would otherwise be ready: the profile is the veto.
	assert_eq!(readiness(false, FamilyState { has_address: true, has_route: true, ..Default::default() }), Readiness::Disabled);
}

#[test]
fn an_on_link_route_is_enough_to_be_ready() {
	// A HOST THAT CAN REACH ITS OWN LINK IS WORKING. Demanding a default route would report a
	// router-less link as broken, and this appliance runs on links like that.
	let on_link = FamilyState { has_address: true, has_route: true, ..Default::default() };
	assert_eq!(readiness(true, on_link), Readiness::Ready);

	// One half is not enough either way.
	assert_eq!(readiness(true, FamilyState { has_address: true, ..Default::default() }), Readiness::Configuring);
	assert_eq!(readiness(true, FamilyState { has_route: true, ..Default::default() }), Readiness::Configuring);
}

#[test]
fn waiting_is_never_failure_and_failure_needs_nothing_left_to_try() {
	// SOLICITATION REACHING ITS MAXIMUM INTERVAL IS NOT FAILURE: P02M0174 asks indefinitely, so a
	// link with no router is a link this host keeps asking.
	let waiting = FamilyState { recovering: true, ..Default::default() };
	assert_eq!(readiness(true, waiting), Readiness::Configuring);
	// A reported failure with something still retrying is still `Configuring`.
	assert_eq!(readiness(true, FamilyState { failed: true, recovering: true, ..Default::default() }), Readiness::Configuring);
	// Only a reported failure with nothing left to try is `Failed` - DHCPv4 failing and then its
	// static configuration failing too.
	assert_eq!(readiness(true, FamilyState { failed: true, ..Default::default() }), Readiness::Failed);
	// AND IT IS A REPORT, not a terminal state: an address and a route arriving afterwards make it
	// ready again.
	assert_eq!(readiness(true, FamilyState { failed: true, has_address: true, has_route: true, ..Default::default() }), Readiness::Ready);
}

#[test]
fn each_kind_refuses_at_its_own_cap_with_the_partition_two_thirds_empty() {
	// HEADROOM IS NOT PERMISSION. The caps sum to 42 of 128 slots, and the 86 left are unavailable to
	// any kind: a seventeenth DNS operation is refused while most of the partition is free.
	assert_eq!(DNS_CAP + SNTP_CAP + DHCP_RESERVED + DIAGNOSTIC_CAP, 42);
	assert!(42 < PENDING_SLOTS);

	let mut pending = Pending::new();
	for index in 0..DNS_CAP {
		// Spread across clients so the per-client cap is not what refuses.
		assert_eq!(pending.admit(PendingKind::Dns, u64::from(index) / 4 + 1), Ok(()), "slot {index}");
	}
	assert_eq!(pending.admit(PendingKind::Dns, 99), Err(PendingRefusal::Kind));
	assert_eq!(pending.refusals(PendingKind::Dns), 1);
	assert!(pending.total() < PENDING_SLOTS, "and the partition itself has room");

	// The other kinds are untouched by DNS being full.
	assert_eq!(pending.admit(PendingKind::Sntp, 1), Ok(()));
	assert_eq!(pending.admit(PendingKind::Diagnostic, 1), Ok(()));
}

#[test]
fn one_client_cannot_take_more_than_its_share_of_a_kind() {
	let mut pending = Pending::new();
	for index in 0..DNS_PER_CLIENT {
		assert_eq!(pending.admit(PendingKind::Dns, 7), Ok(()), "slot {index}");
	}
	assert_eq!(pending.admit(PendingKind::Dns, 7), Err(PendingRefusal::PerClient));
	// ANOTHER CLIENT IS UNAFFECTED, which is the whole point of a per-client cap rather than only a
	// service-wide one.
	assert_eq!(pending.admit(PendingKind::Dns, 8), Ok(()));

	// Ping and probe share the diagnostic cap, because they are one mechanism.
	for _ in 0..DIAGNOSTIC_PER_CLIENT {
		pending.admit(PendingKind::Diagnostic, 7).expect("room");
	}
	assert_eq!(pending.admit(PendingKind::Diagnostic, 7), Err(PendingRefusal::PerClient));
}

#[test]
fn releasing_returns_the_slot_to_both_counts() {
	let mut pending = Pending::new();
	pending.admit(PendingKind::Dns, 7).expect("room");
	assert_eq!(pending.used(PendingKind::Dns), 1);
	pending.release(PendingKind::Dns, 7);
	assert_eq!(pending.used(PendingKind::Dns), 0);
	// And the client's own count went with it, so it can ask again.
	for _ in 0..DNS_PER_CLIENT {
		pending.admit(PendingKind::Dns, 7).expect("room");
	}
	assert_eq!(pending.admit(PendingKind::Dns, 7), Err(PendingRefusal::PerClient));
}

#[test]
fn a_client_that_goes_away_releases_everything_it_was_holding() {
	let mut pending = Pending::new();
	pending.admit(PendingKind::Dns, 7).expect("room");
	pending.admit(PendingKind::Dns, 7).expect("room");
	pending.admit(PendingKind::Diagnostic, 7).expect("room");
	pending.admit(PendingKind::Dns, 8).expect("room");
	pending.release_client(7);
	assert_eq!(pending.used(PendingKind::Dns), 1, "only the other client's");
	assert_eq!(pending.used(PendingKind::Diagnostic), 0);
}

#[test]
fn the_services_own_lease_work_has_capacity_client_work_cannot_take() {
	// AUTONOMOUS DHCP ALWAYS HAS ITS RESERVED CAPACITY when T1, T2 or expiry starts an exchange -
	// which is why it is a kind of its own rather than a share of the general pool.
	let mut pending = Pending::new();
	for index in 0..DNS_CAP {
		pending.admit(PendingKind::Dns, u64::from(index) / 4 + 1).expect("room");
	}
	for _ in 0..DIAGNOSTIC_CAP {
		pending.admit(PendingKind::Diagnostic, 50).ok();
	}
	assert_eq!(pending.admit(PendingKind::Dhcp, 0), Ok(()), "one active exchange");
	assert_eq!(pending.admit(PendingKind::Dhcp, 0), Ok(()), "and one replacing phase");
	assert_eq!(pending.admit(PendingKind::Dhcp, 0), Err(PendingRefusal::Kind), "and no more than that");
}

#[test]
fn the_echo_pair_comes_from_one_counter_and_does_not_repeat() {
	// TWO PROBES A MILLISECOND APART WOULD OTHERWISE CARRY THE SAME PAIR, and an error quoting one
	// would be attributed to both - which is exactly the correlation the ICMP rules exist to make
	// possible.
	let mut counter = EchoCounter::new(0x4242);
	assert_eq!(counter.next(), (0x4242, 0));
	assert_eq!(counter.next(), (0x4242, 1));
	assert_eq!(counter.next(), (0x4242, 2));

	// The identifier advances when the sequence wraps, so the FULL pair is not reused.
	let mut wrapping = EchoCounter { identifier: 1, sequence: u16::MAX };
	assert_eq!(wrapping.next(), (1, u16::MAX));
	assert_eq!(wrapping.next(), (2, 0), "a new identifier rather than the same pair again");
}

/// M7's diagnostic matrix, run for both families.
///
/// ONE PARTITION AND NOT TWO, which is the property worth asserting rather than assuming: a
/// per-family diagnostic budget would let one caller hold twice its share simply by alternating a
/// `ping` of a v4 address with a `ping` of a v6 one, and neither half would ever look overspent.
mod both_families {
	use super::*;
	use crate::invalidation::{OperationState, Outcome, on_invalidation};

	/// Which family a diagnostic named. It changes no budget; that is the point.
	#[derive(Clone, Copy, PartialEq, Eq, Debug)]
	enum Family {
		V4,
		V6,
	}

	#[test]
	fn admission_is_one_budget_that_neither_family_can_double() {
		let mut pending = Pending::new();
		let client: u64 = 7;
		// A caller's four diagnostic slots, taken as two of each family.
		for family in [Family::V4, Family::V6, Family::V4, Family::V6] {
			assert_eq!(pending.admit(PendingKind::Diagnostic, client), Ok(()), "{family:?}");
		}
		// AND THE FIFTH IS REFUSED WHATEVER FAMILY IT NAMES.
		for family in [Family::V4, Family::V6] {
			assert_eq!(pending.admit(PendingKind::Diagnostic, client), Err(PendingRefusal::PerClient), "{family:?}");
		}
		assert_eq!(pending.used(PendingKind::Diagnostic), 4);
	}

	#[test]
	fn a_released_diagnostic_frees_the_slot_for_either_family() {
		let mut pending = Pending::new();
		let client: u64 = 7;
		for _ in 0..DIAGNOSTIC_PER_CLIENT {
			pending.admit(PendingKind::Diagnostic, client).expect("its share");
		}
		// A TIMEOUT AND A CANCELLATION RELEASE THE SAME WAY. The caller's four are its four whether
		// each ended by answering, by timing out or by the caller going away.
		pending.release(PendingKind::Diagnostic, client);
		assert_eq!(pending.used(PendingKind::Diagnostic), DIAGNOSTIC_PER_CLIENT - 1);
		assert_eq!(pending.admit(PendingKind::Diagnostic, client), Ok(()), "and the slot is usable again");
	}

	#[test]
	fn a_caller_going_away_releases_every_diagnostic_it_held_in_either_family() {
		let mut pending = Pending::new();
		let (gone, other): (u64, u64) = (7, 8);
		for _ in 0..DIAGNOSTIC_PER_CLIENT {
			pending.admit(PendingKind::Diagnostic, gone).expect("its share");
		}
		pending.admit(PendingKind::Diagnostic, other).expect("another caller's");
		pending.release_client(gone);
		assert_eq!(pending.used(PendingKind::Diagnostic), 1, "exactly the other caller's is left");
		// EXACTLY ONCE. A second release of a caller that is already gone must not credit the budget
		// with slots nobody was holding.
		pending.release_client(gone);
		assert_eq!(pending.used(PendingKind::Diagnostic), 1);
	}

	#[test]
	fn a_diagnostic_that_has_transmitted_fails_on_invalidation_in_either_family() {
		// NO SENT REQUEST SILENTLY CHANGES ITS SOURCE AND RETRIES AS THE OLD TUPLE, whichever family
		// it is in: the peer will answer to the tuple it was asked from.
		for family in [Family::V4, Family::V6] {
			let outcome = on_invalidation(OperationState::AwaitingReply);
			assert_eq!(outcome, Outcome::Fail, "{family:?}");
			assert!(!outcome.keeps_deadline());
		}
		// And one that has not transmitted reselects, still within its original deadline.
		let unsent = on_invalidation(OperationState::UnsentAutomaticSource);
		assert_eq!(unsent, Outcome::Reselect);
		assert!(unsent.keeps_deadline(), "a reselected unsent operation keeps its deadline");
	}

	#[test]
	fn a_silent_ping_and_probe_hold_two_slots_while_a_dns_query_completes_beside_them() {
		// THE CONCURRENCY CASE. The diagnostics and the resolver draw on DIFFERENT partitions, so a
		// DNS answer arriving while two diagnostics sit silent neither frees nor consumes a
		// diagnostic slot - and the diagnostics still own theirs when it does.
		let mut pending = Pending::new();
		let client: u64 = 7;
		pending.admit(PendingKind::Diagnostic, client).expect("the ping");
		pending.admit(PendingKind::Diagnostic, client).expect("the probe");
		pending.admit(PendingKind::Dns, client).expect("the query");
		assert_eq!(pending.used(PendingKind::Diagnostic), 2);
		assert_eq!(pending.used(PendingKind::Dns), 1);

		// The query answers first.
		pending.release(PendingKind::Dns, client);
		assert_eq!(pending.used(PendingKind::Dns), 0);
		assert_eq!(pending.used(PendingKind::Diagnostic), 2, "the silent pair is untouched");

		// Then both diagnostics time out, and each releases its own.
		pending.release(PendingKind::Diagnostic, client);
		pending.release(PendingKind::Diagnostic, client);
		assert_eq!(pending.used(PendingKind::Diagnostic), 0);
		assert_eq!(pending.total(), 0, "and nothing is left charged to anyone");
	}

	#[test]
	fn the_echo_identity_never_repeats_a_sequence_within_one_identifier() {
		// THE CORRELATION KEY IS DRAWN HERE, and it is what tells one row of a trace from the next.
		// A counter that repeated a sequence would let an error about an earlier hop complete a
		// later one.
		let mut counter = EchoCounter::new(0x4321);
		let first = counter.next();
		let second = counter.next();
		assert_eq!(first.0, second.0, "one identifier for the run");
		assert_ne!(first.1, second.1, "and a fresh sequence for each probe");
	}
}
