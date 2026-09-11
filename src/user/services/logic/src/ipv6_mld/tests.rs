// The ordering rule this exists for - report before detection, re-report after - and the timer cases
// named individually, because a generic "it retransmits" test passes with any of them missing.

use super::*;

fn group(low: u16) -> Address {
	let mut bytes = [0u8; 16];
	bytes[0] = 0xff;
	bytes[1] = 0x02;
	bytes[11] = 1;
	bytes[12] = 0xff;
	bytes[14..16].copy_from_slice(&low.to_be_bytes());
	Address::new(bytes)
}

fn link_local(low: u16) -> Address {
	let mut bytes = [0u8; 16];
	bytes[0] = 0xfe;
	bytes[1] = 0x80;
	bytes[14..16].copy_from_slice(&low.to_be_bytes());
	Address::new(bytes)
}

#[test]
fn all_nodes_is_never_reported() {
	let mut listener = Listener::new();
	assert_eq!(listener.join(ALL_NODES, 0), None, "membership that cannot be left is not announced");
	assert!(listener.is_empty());
	// And a unicast address is not a group at all.
	assert_eq!(listener.join(link_local(1), 0), None);
	assert!(listener.is_empty());
}

#[test]
fn a_query_is_validated_on_mlds_terms_and_not_on_neighbour_discoverys() {
	// HOP LIMIT ONE, not 255. Applying the neighbour-discovery rule here would discard every
	// legitimate query.
	assert_eq!(validate_query(link_local(1), 1, true), Ok(()));
	assert_eq!(validate_query(link_local(1), 255, true), Err(QueryRefusal::HopLimitNotOne));
	assert_eq!(validate_query(link_local(1), 64, true), Err(QueryRefusal::HopLimitNotOne));
	assert_eq!(validate_query(link_local(1), 1, false), Err(QueryRefusal::NoRouterAlert));

	let mut global = [0u8; 16];
	global[0] = 0x20;
	global[1] = 0x01;
	assert_eq!(validate_query(Address::new(global), 1, true), Err(QueryRefusal::SourceNotLinkLocal));
	assert_eq!(validate_query(UNSPECIFIED, 1, true), Err(QueryRefusal::SourceNotLinkLocal));
}

#[test]
fn an_invalid_query_is_counted_and_not_logged_per_packet() {
	let mut listener = Listener::new();
	for _ in 0..1000 {
		listener.record_invalid_query();
	}
	assert_eq!(listener.invalid_queries(), 1000, "an aggregate, not a line each");
}

#[test]
fn the_envelope_is_the_same_on_every_message_and_the_source_rule_has_exactly_one_exception() {
	// BEFORE DETECTION: the unspecified source, which is the only case that uses it.
	let pre = envelope(None, ALL_MLDV2_ROUTERS);
	assert_eq!(pre.hop_limit, 1);
	assert!(pre.router_alert);
	assert_eq!(pre.source, UNSPECIFIED);
	assert_eq!(pre.destination, ALL_MLDV2_ROUTERS);

	// AFTER: every later message, whatever its kind, uses the real address.
	let post = envelope(Some(link_local(1)), ALL_MLDV2_ROUTERS);
	assert_eq!(post.source, link_local(1));
	assert_eq!(post.hop_limit, 1);
	assert!(post.router_alert);

	// The destinations the message kinds carry.
	assert_eq!(Emission::ReportV2 { group: group(1) }.destination(), ALL_MLDV2_ROUTERS);
	assert_eq!(Emission::LeaveV2 { group: group(1) }.destination(), ALL_MLDV2_ROUTERS);
	assert_eq!(Emission::ReportV1 { group: group(1) }.destination(), group(1));
	assert_eq!(Emission::DoneV1 { group: group(1) }.destination(), crate::ipv6::ALL_ROUTERS);
	assert_eq!(Emission::ReportV2 { group: group(7) }.group(), group(7));
}

#[test]
fn joining_reports_immediately_and_the_report_is_retransmitted() {
	let mut listener = Listener::new();
	assert_eq!(listener.join(group(1), 0), Some(Emission::ReportV2 { group: group(1) }));
	assert_eq!(listener.joined(), alloc::vec![group(1)]);

	// A LOST FIRST REPORT IS RECOVERED BY THE RETRANSMISSION. The default robustness is TWO
	// TRANSMISSIONS in total, so what it buys is exactly one loss - and a test that asked for more
	// would be asking the default configuration to survive something it does not claim to.
	assert!(listener.tick(UNSOLICITED_REPORT_INTERVAL_MS - 1).is_empty());
	assert_eq!(listener.tick(UNSOLICITED_REPORT_INTERVAL_MS), alloc::vec![Emission::ReportV2 { group: group(1) }]);
	assert!(listener.tick(2 * UNSOLICITED_REPORT_INTERVAL_MS).is_empty(), "two transmissions, and no more");
	assert_eq!(listener.next_deadline(), None);
}

#[test]
fn leaving_keeps_the_record_until_its_transmissions_finish_and_a_rejoin_reuses_it() {
	let mut listener = Listener::new();
	listener.join(group(1), 0);
	listener.tick(UNSOLICITED_REPORT_INTERVAL_MS);

	assert_eq!(listener.leave(group(1), 10_000), Some(Emission::LeaveV2 { group: group(1) }));
	assert!(listener.joined().is_empty(), "no longer a member");
	assert_eq!(listener.len(), 1, "but the record stays for the retransmission");
	assert_eq!(listener.tick(11_000), alloc::vec![Emission::LeaveV2 { group: group(1) }]);
	assert!(listener.is_empty(), "and then it is gone");

	let mut rejoining = Listener::new();
	rejoining.join(group(2), 0);
	rejoining.leave(group(2), 100);
	assert_eq!(rejoining.len(), 1);
	assert_eq!(rejoining.join(group(2), 200), Some(Emission::ReportV2 { group: group(2) }));
	assert_eq!(rejoining.len(), 1, "the record is reused rather than taking a second slot");
	assert_eq!(rejoining.joined(), alloc::vec![group(2)]);
	assert_eq!(rejoining.leave(group(99), 0), None, "leaving a group nothing joined does nothing");
}

#[test]
fn a_leave_while_a_join_report_is_pending_merges_and_resets_rather_than_transmitting_out_of_order() {
	// THE ORDER MATTERS MORE THAN THE COUNT. A router that saw the Done before the Report would
	// believe this host is still a member of a group it has left.
	let mut listener = Listener::new();
	listener.join(group(1), 0);
	assert_eq!(listener.get(group(1)).expect("a record").retransmits_left, 1, "one transmission still owed");

	assert_eq!(listener.leave(group(1), 500), Some(Emission::LeaveV2 { group: group(1) }));
	let record = listener.get(group(1)).expect("a record");
	assert_eq!(record.retransmits_left, 1, "the counter is reset for the leave, not carried over");
	assert_eq!(record.retransmit_at_ms, Some(500 + UNSOLICITED_REPORT_INTERVAL_MS), "and rescheduled from the leave");
	assert!(record.response.is_none(), "a pending query response about a group we have left is dropped");

	// What comes out is the leave, twice, and never the superseded report.
	let due = listener.tick(500 + UNSOLICITED_REPORT_INTERVAL_MS);
	assert_eq!(due, alloc::vec![Emission::LeaveV2 { group: group(1) }]);
	assert!(listener.is_empty());
}

#[test]
fn the_first_report_precedes_detection_and_every_group_is_reported_again_after_it() {
	let mut listener = Listener::new();
	listener.join(group(1), 0);
	listener.join(group(2), 0);
	assert!(!listener.reported_after_dad());

	let again = listener.report_after_dad(5_000);
	assert_eq!(again.len(), 2, "every group already joined is reported again from the real address");
	assert!(again.contains(&Emission::ReportV2 { group: group(1) }));
	assert!(again.contains(&Emission::ReportV2 { group: group(2) }));
	assert!(listener.reported_after_dad());
	assert!(listener.tick(6_000).is_empty(), "the re-report supersedes the outstanding retransmissions");
}

#[test]
fn a_general_query_does_not_supersede_a_per_address_response_that_is_due_earlier() {
	// THE FIRST OF THE FOUR TIMER CASES. An implementation that replaced the pending deadline would
	// answer late, and one that queued a second answer would talk twice.
	let mut listener = Listener::new();
	listener.join(group(1), 0);
	listener.report_after_dad(0);

	listener.on_query(Some(group(1)), &[], 2_000, 1_000);
	assert_eq!(listener.get(group(1)).expect("a record").response.as_ref().expect("owed").due_ms, 2_000);

	listener.on_query(None, &[], 60_000, 1_500);
	let response = listener.get(group(1)).expect("a record").response.as_ref().expect("still owed");
	assert_eq!(response.due_ms, 2_000, "the earlier deadline wins");
	assert_eq!(listener.tick(2_000), alloc::vec![Emission::ReportV2 { group: group(1) }]);
	assert!(listener.tick(40_000).is_empty(), "one answer, not two");
}

#[test]
fn a_general_query_while_a_general_response_is_pending_cancels_the_older_rather_than_duplicating_it() {
	let mut listener = Listener::new();
	listener.join(group(1), 0);
	listener.join(group(2), 0);
	listener.report_after_dad(0);

	listener.on_query(None, &[], 10_000, 0);
	listener.on_query(None, &[], 4_000, 0);
	for index in [1u16, 2] {
		assert_eq!(listener.get(group(index)).expect("a record").response.as_ref().expect("owed").due_ms, 2_000, "the earlier of the two");
	}
	assert_eq!(listener.tick(2_000).len(), 2, "one answer per group");
	assert!(listener.tick(10_000).is_empty(), "and the older one was merged, not queued");
}

#[test]
fn an_address_specific_query_merging_into_a_source_specific_response_clears_the_recorded_sources() {
	let mut listener = Listener::new();
	listener.join(group(1), 0);
	listener.report_after_dad(0);

	listener.on_query(Some(group(1)), &[link_local(1), link_local(2)], 10_000, 0);
	assert_eq!(listener.get(group(1)).expect("a record").response.as_ref().expect("owed").sources.len(), 2);

	// THE BROADER REPORT IS NOW OWED, so the narrowing is dropped and the earlier deadline kept.
	listener.on_query(Some(group(1)), &[], 2_000, 0);
	let response = listener.get(group(1)).expect("a record").response.as_ref().expect("owed");
	assert!(response.address_specific(), "an address-specific query clears the recorded sources");
	assert_eq!(response.due_ms, 1_000, "and keeps the earlier of the two deadlines");
}

#[test]
fn two_source_specific_queries_union_their_lists() {
	let mut listener = Listener::new();
	listener.join(group(1), 0);
	listener.report_after_dad(0);

	listener.on_query(Some(group(1)), &[link_local(1), link_local(2)], 10_000, 0);
	listener.on_query(Some(group(1)), &[link_local(2), link_local(3)], 10_000, 0);
	let response = listener.get(group(1)).expect("a record").response.as_ref().expect("owed");
	assert_eq!(response.sources, alloc::vec![link_local(1), link_local(2), link_local(3)], "unioned, and a repeat costs no slot");
}

#[test]
fn a_source_specific_query_arriving_while_an_address_specific_response_is_pending_stays_broad() {
	// THE REVERSE ORDERING, which the other four cases miss. The recorded list is ALREADY EMPTY, so
	// an implementation that unions into it answers the new sources and silently drops the broader
	// report that was already owed - and every one of the other cases still passes.
	let mut listener = Listener::new();
	listener.join(group(1), 0);
	listener.report_after_dad(0);

	listener.on_query(Some(group(1)), &[], 10_000, 0);
	assert!(listener.get(group(1)).expect("a record").response.as_ref().expect("owed").address_specific());

	listener.on_query(Some(group(1)), &[link_local(1)], 2_000, 0);
	let response = listener.get(group(1)).expect("a record").response.as_ref().expect("owed");
	assert!(response.address_specific(), "the list stays EMPTY and the answer stays the broader one");
	assert_eq!(response.due_ms, 1_000, "only the timer merged");
}

#[test]
fn the_source_cap_keeps_exactly_its_limit_and_degrades_past_it_without_dropping_the_answer() {
	let cap = Resource::MldSourcesPerRecord.limit() as usize;
	let mut listener = Listener::new();
	listener.join(group(1), 0);
	listener.report_after_dad(0);

	// EXACTLY THE CAP is kept and answered source-specifically.
	let exactly: alloc::vec::Vec<Address> = (0..cap as u16).map(link_local).collect();
	listener.on_query(Some(group(1)), &exactly, 10_000, 0);
	let response = listener.get(group(1)).expect("a record").response.as_ref().expect("owed");
	assert_eq!(response.sources.len(), cap);
	assert!(!response.address_specific());
	assert_eq!(response.due_ms, 5_000);

	// ONE MORE DISJOINT SOURCE degrades the record, clears the list, and keeps the EARLIEST of the
	// deadlines already chosen.
	listener.on_query(Some(group(1)), &[link_local(9999)], 60_000, 0);
	let response = listener.get(group(1)).expect("a record").response.as_ref().expect("still owed");
	assert!(response.address_specific(), "degraded to address-specific");
	assert!(response.sources.is_empty(), "and the list is cleared");
	assert_eq!(response.due_ms, 5_000, "the earliest deadline already chosen is kept");
}

#[test]
fn a_flood_of_disjoint_source_lists_stays_within_the_cap_and_is_still_answered() {
	// THE HOSTILE CASE THE CAP EXISTS FOR. Under an unbounded union these queries grow one record
	// without limit; the answer must degrade rather than be dropped, because a querier that floods
	// must not be able to suppress a report.
	let cap = Resource::MldSourcesPerRecord.limit() as usize;
	let mut listener = Listener::new();
	listener.join(group(1), 0);
	listener.report_after_dad(0);

	for round in 0..64u16 {
		let disjoint: alloc::vec::Vec<Address> = (0..8u16).map(|index| link_local(round * 8 + index)).collect();
		listener.on_query(Some(group(1)), &disjoint, 10_000, 0);
		let response = listener.get(group(1)).expect("a record").response.as_ref().expect("owed");
		assert!(response.sources.len() <= cap, "round {round} grew the record past the cap");
	}
	let response = listener.get(group(1)).expect("a record").response.as_ref().expect("owed");
	assert!(response.address_specific(), "degraded");
	assert_eq!(listener.tick(5_000), alloc::vec![Emission::ReportV2 { group: group(1) }], "and still answered");
}

#[test]
fn a_version_one_querier_changes_every_message_and_cannot_express_sources() {
	let mut listener = Listener::new();
	listener.join(group(1), 0);
	assert_eq!(listener.version(), Version::V2);

	listener.saw_v1_querier(1_000);
	assert_eq!(listener.version(), Version::V1 { until_ms: 1_000 + V1_QUERIER_PRESENT_MS });
	assert_eq!(listener.join(group(2), 1_000), Some(Emission::ReportV1 { group: group(2) }));
	assert_eq!(listener.leave(group(2), 1_100), Some(Emission::DoneV1 { group: group(2) }));
	assert!(listener.tick(2_100).contains(&Emission::DoneV1 { group: group(2) }));

	// A source-specific query in compatibility mode records nothing: the version cannot say it.
	listener.report_after_dad(2_000);
	listener.on_query(Some(group(1)), &[link_local(1)], 1_000, 2_000);
	assert!(listener.get(group(1)).expect("a record").response.as_ref().expect("owed").address_specific());

	let mut expiring = Listener::new();
	expiring.saw_v1_querier(0);
	expiring.tick(V1_QUERIER_PRESENT_MS - 1);
	assert!(matches!(expiring.version(), Version::V1 { .. }));
	expiring.tick(V1_QUERIER_PRESENT_MS);
	assert_eq!(expiring.version(), Version::V2);
}

#[test]
fn the_listener_is_bounded_and_refuses_rather_than_evicting() {
	let mut listener = Listener::new();
	for index in 0..Resource::MldGroups.limit() as u16 {
		assert!(listener.join(group(index), 0).is_some(), "group {index}");
	}
	assert_eq!(listener.len(), 32);
	assert_eq!(listener.join(group(999), 0), None, "no live record was thrown away");
	assert_eq!(listener.len(), 32);
	assert_eq!(listener.refusals().get(Resource::MldGroups), 1);

	// A REJOIN AT CAPACITY REUSES THE RECORD, so it costs no slot and is not refused. What it does
	// cost is a fresh report, which is right: a host that says it is a member again says so on the
	// wire, and the alternative is a router whose state depends on when it last heard.
	assert_eq!(listener.join(group(0), 0), Some(Emission::ReportV2 { group: group(0) }));
	assert_eq!(listener.len(), 32, "and no slot with it");
	assert_eq!(listener.refusals().get(Resource::MldGroups), 1, "nothing was refused");

	// A SLOT COMES BACK ONLY WHEN THE LEAVE HAS FINISHED TRANSMITTING, not when membership ends: the
	// record is what carries the remaining Done messages, and dropping it early would lose them.
	listener.leave(group(7), 1_000);
	assert_eq!(listener.len(), 32, "the record stays for its retransmission");
	assert_eq!(listener.join(group(999), 1_000), None, "so the slot is not free yet");
	listener.tick(1_000 + UNSOLICITED_REPORT_INTERVAL_MS);
	assert_eq!(listener.len(), 31);
	assert!(listener.join(group(999), 2_000).is_some(), "and now the reclaimed slot admits a new group");
	assert_eq!(listener.len(), 32);
}

#[test]
fn the_aggregated_deadline_covers_both_obligations() {
	// A listener that reported only its retransmission deadline would let a query response fire
	// late, and one that reported only the response would stop retransmitting.
	let mut listener = Listener::new();
	listener.join(group(1), 0);
	assert_eq!(listener.next_deadline(), Some(UNSOLICITED_REPORT_INTERVAL_MS), "the retransmission");
	listener.report_after_dad(0);
	assert_eq!(listener.next_deadline(), None);
	listener.on_query(Some(group(1)), &[], 500, 0);
	assert_eq!(listener.next_deadline(), Some(250), "the response");

	let mut both = Listener::new();
	both.join(group(2), 0);
	both.on_query(Some(group(2)), &[], 100, 0);
	assert_eq!(both.next_deadline(), Some(50), "the earlier of the two");
}
