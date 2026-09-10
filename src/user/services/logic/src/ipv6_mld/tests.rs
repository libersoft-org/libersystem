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

	// A LOST FIRST REPORT IS RECOVERED BY THE RETRANSMISSION. Without it, one lost packet defeats
	// membership until the next query, which on a quiet link can be minutes.
	assert!(listener.tick(UNSOLICITED_REPORT_INTERVAL_MS - 1).is_empty());
	assert_eq!(listener.tick(UNSOLICITED_REPORT_INTERVAL_MS), alloc::vec![Emission::ReportV2 { group: group(1) }]);
	assert_eq!(listener.tick(2 * UNSOLICITED_REPORT_INTERVAL_MS), alloc::vec![Emission::ReportV2 { group: group(1) }]);
	assert!(listener.tick(3 * UNSOLICITED_REPORT_INTERVAL_MS).is_empty(), "the robustness count is spent");
	assert_eq!(listener.next_deadline(), None);
}

#[test]
fn leaving_keeps_the_record_until_its_retransmissions_finish_and_a_rejoin_reuses_it() {
	let mut listener = Listener::new();
	listener.join(group(1), 0);
	listener.tick(UNSOLICITED_REPORT_INTERVAL_MS);
	listener.tick(2 * UNSOLICITED_REPORT_INTERVAL_MS);

	assert_eq!(listener.leave(group(1), 10_000), Some(Emission::LeaveV2 { group: group(1) }));
	assert!(listener.joined().is_empty(), "no longer a member");
	assert_eq!(listener.len(), 1, "but the record stays for the retransmissions");

	assert_eq!(listener.tick(11_000), alloc::vec![Emission::LeaveV2 { group: group(1) }]);
	assert_eq!(listener.len(), 1);
	assert_eq!(listener.tick(12_000), alloc::vec![Emission::LeaveV2 { group: group(1) }]);
	assert!(listener.is_empty(), "and then it is gone");

	// A REJOIN BEFORE THE RECORD IS GONE reuses it rather than taking a second slot.
	let mut rejoining = Listener::new();
	rejoining.join(group(2), 0);
	rejoining.leave(group(2), 100);
	assert_eq!(rejoining.len(), 1);
	assert_eq!(rejoining.join(group(2), 200), Some(Emission::ReportV2 { group: group(2) }));
	assert_eq!(rejoining.len(), 1);
	assert_eq!(rejoining.joined(), alloc::vec![group(2)]);

	assert_eq!(rejoining.leave(group(99), 0), None, "leaving a group nothing joined does nothing");
}

#[test]
fn the_first_report_precedes_detection_and_every_group_is_reported_again_after_it() {
	let mut listener = Listener::new();
	// Joining the solicited-node groups happens while the link-local address is still tentative.
	listener.join(group(1), 0);
	listener.join(group(2), 0);
	assert!(!listener.reported_after_dad());

	let again = listener.report_after_dad(5_000);
	assert_eq!(again.len(), 2, "every group already joined is reported again from the real address");
	assert!(again.contains(&Emission::ReportV2 { group: group(1) }));
	assert!(again.contains(&Emission::ReportV2 { group: group(2) }));
	assert!(listener.reported_after_dad());
	// The re-report supersedes the outstanding retransmissions rather than doubling them.
	assert!(listener.tick(6_000).is_empty());
}

#[test]
fn a_query_schedules_one_delayed_answer_and_a_second_query_does_not_add_another() {
	let mut listener = Listener::new();
	listener.join(group(1), 0);
	listener.report_after_dad(0);
	assert_eq!(listener.next_deadline(), None, "nothing pending");

	assert_eq!(listener.on_query(None, &[], 10_000, 1_000), 1, "a general query asks about every joined group");
	assert_eq!(listener.next_deadline(), Some(6_000), "answered inside the maximum response delay");

	// A SECOND QUERY WHILE ONE IS PENDING: the earlier deadline wins, so the host answers once.
	assert_eq!(listener.on_query(None, &[], 20_000, 2_000), 1);
	assert_eq!(listener.next_deadline(), Some(6_000), "not replaced by the later one, and not a second answer");
	assert_eq!(listener.tick(6_000), alloc::vec![Emission::ReportV2 { group: group(1) }]);
	assert!(listener.tick(20_000).is_empty(), "one answer, not two");

	// A group-specific query only asks about that group.
	listener.join(group(2), 30_000);
	listener.report_after_dad(30_000);
	assert_eq!(listener.on_query(Some(group(2)), &[], 1_000, 30_000), 1);
	assert_eq!(listener.on_query(Some(group(99)), &[], 1_000, 30_000), 0, "a group this host has not joined");
}

#[test]
fn a_source_specific_query_records_its_sources_and_degrades_when_there_are_too_many() {
	let mut listener = Listener::new();
	listener.join(group(1), 0);
	listener.report_after_dad(0);

	let sources: alloc::vec::Vec<Address> = (0..4u16).map(link_local).collect();
	listener.on_query(Some(group(1)), &sources, 1_000, 0);
	assert_eq!(listener.get(group(1)).expect("a record").sources.len(), 4);

	// PAST THE CAP THE ANSWER DEGRADES TO ADDRESS-SPECIFIC rather than being refused: an answer
	// about the whole group is correct, just less precise.
	let many: alloc::vec::Vec<Address> = (0..Resource::MldSourcesPerRecord.limit() as u16 + 10).map(link_local).collect();
	listener.on_query(Some(group(1)), &many, 1_000, 0);
	assert!(listener.get(group(1)).expect("a record").sources.is_empty(), "degraded, not refused");
}

#[test]
fn a_version_one_querier_changes_every_message_this_host_sends_until_the_timer_runs_out() {
	let mut listener = Listener::new();
	listener.join(group(1), 0);
	assert_eq!(listener.version(), Version::V2);

	listener.saw_v1_querier(1_000);
	assert_eq!(listener.version(), Version::V1 { until_ms: 1_000 + V1_QUERIER_PRESENT_MS });
	assert_eq!(listener.join(group(2), 1_000), Some(Emission::ReportV1 { group: group(2) }), "reports are v1 Reports");
	assert_eq!(listener.leave(group(2), 1_100), Some(Emission::DoneV1 { group: group(2) }), "and a leave is a Done");

	// The retransmissions are v1 too.
	assert!(listener.tick(2_100).contains(&Emission::DoneV1 { group: group(2) }));

	// AND IT EXPIRES. A querier that goes away leaves this host in v2 again.
	let mut expiring = Listener::new();
	expiring.saw_v1_querier(0);
	expiring.tick(V1_QUERIER_PRESENT_MS - 1);
	assert!(matches!(expiring.version(), Version::V1 { .. }));
	expiring.tick(V1_QUERIER_PRESENT_MS);
	assert_eq!(expiring.version(), Version::V2);

	// A source-specific query while in v1 records nothing: the version cannot express it.
	let mut compat = Listener::new();
	compat.saw_v1_querier(0);
	compat.join(group(1), 0);
	compat.report_after_dad(0);
	compat.on_query(Some(group(1)), &[link_local(1)], 1_000, 0);
	assert!(compat.get(group(1)).expect("a record").sources.is_empty());
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
}
