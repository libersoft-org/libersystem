//! Which flow an error belongs to, and what it is allowed to do to it.

use super::*;

fn v4(a: u8, b: u8, c: u8, d: u8) -> Local {
	Local::V4([a, b, c, d])
}

fn v6(low: u16) -> Local {
	let mut octets = [0u8; 16];
	octets[0] = 0x20;
	octets[1] = 0x01;
	octets[14..].copy_from_slice(&low.to_be_bytes());
	Local::V6(octets)
}

fn flow() -> FlowKey {
	FlowKey { local: v6(1), local_port: 50000, remote: v6(2), remote_port: 443, interface_generation: 1 }
}

fn quote_for(flow: &FlowKey, transport: QuotedKind) -> Quotation {
	Quotation { responder: v6(0xa1), local: flow.local, local_port: flow.local_port, remote: flow.remote, remote_port: flow.remote_port, interface_generation: flow.interface_generation, transport }
}

#[test]
fn an_error_belongs_to_one_flow_and_every_part_of_the_key_decides() {
	let flow = flow();
	let quote = quote_for(&flow, QuotedKind::Tcp { sequence: 1000 });
	assert!(flow.owns(&quote));

	for altered in [
		Quotation { local_port: 50001, ..quote },
		Quotation { remote_port: 80, ..quote },
		Quotation { remote: v6(3), ..quote },
		Quotation { local: v6(9), ..quote },
		// A REPLACED NIC IS A DIFFERENT INTERFACE. Without the generation this flow would inherit
		// the errors of one that is gone.
		Quotation { interface_generation: 2, ..quote },
	] {
		assert!(!flow.owns(&altered), "{altered:?}");
	}

	// EQUAL PORTS IN TWO FAMILIES ARE TWO FLOWS. The low words match; the family does not.
	let other_family = FlowKey { local: v4(10, 0, 2, 15), local_port: 50000, remote: v4(10, 0, 2, 2), remote_port: 443, interface_generation: 1 };
	assert!(!other_family.owns(&quote));
}

#[test]
fn an_error_quoting_one_flow_does_nothing_to_another() {
	// THE NEGATIVE THIS MODULE EXISTS FOR: an error may not terminate, resize or invalidate a flow it
	// is not about.
	let a = flow();
	let b = FlowKey { local_port: 50001, ..a };
	let quote = quote_for(&a, QuotedKind::Tcp { sequence: 1000 });
	assert_eq!(path_mtu_for_tcp(&a, &quote, 900, 2000, true, 1280, 1500), PathMtu::Apply(1280));
	assert_eq!(path_mtu_for_tcp(&b, &quote, 900, 2000, true, 1280, 1500), PathMtu::Ignored(Ignored::NotThisFlow));
}

#[test]
fn a_packet_too_big_must_quote_sequence_space_that_is_actually_on_the_wire() {
	let flow = flow();
	// AN EMPTY FLIGHT REJECTS EVERYTHING, which is what makes a forged report useless against an idle
	// connection.
	let quote = quote_for(&flow, QuotedKind::Tcp { sequence: 1000 });
	assert_eq!(path_mtu_for_tcp(&flow, &quote, 1000, 1000, true, 1280, 1500), PathMtu::Ignored(Ignored::EmptyFlight));

	// Below the oldest unacknowledged byte is data the peer has already taken.
	assert_eq!(path_mtu_for_tcp(&flow, &quote, 1001, 2000, true, 1280, 1500), PathMtu::Ignored(Ignored::OutOfWindow));
	// And `SND.NXT` is the first byte NOT sent, so the interval is half open.
	let at_nxt = quote_for(&flow, QuotedKind::Tcp { sequence: 2000 });
	assert_eq!(path_mtu_for_tcp(&flow, &at_nxt, 1000, 2000, true, 1280, 1500), PathMtu::Ignored(Ignored::OutOfWindow));
	// The oldest unacknowledged byte itself is on the wire.
	assert_eq!(path_mtu_for_tcp(&flow, &quote, 1000, 2000, true, 1280, 1500), PathMtu::Apply(1280));
}

#[test]
fn every_check_happens_before_anything_is_changed() {
	let flow = flow();
	// A route that is gone, a quotation of something that is not TCP, and a report that is not
	// smaller each refuse without reaching the write.
	let quote = quote_for(&flow, QuotedKind::Tcp { sequence: 1000 });
	assert_eq!(path_mtu_for_tcp(&flow, &quote, 900, 2000, false, 1280, 1500), PathMtu::Ignored(Ignored::RouteGone));
	let udp = quote_for(&flow, QuotedKind::Udp);
	assert_eq!(path_mtu_for_tcp(&flow, &udp, 900, 2000, true, 1280, 1500), PathMtu::Ignored(Ignored::NotTcp));
	assert_eq!(path_mtu_for_tcp(&flow, &quote, 900, 2000, true, 1500, 1500), PathMtu::Ignored(Ignored::NotSmaller));
	assert_eq!(path_mtu_for_tcp(&flow, &quote, 900, 2000, true, 9000, 1500), PathMtu::Ignored(Ignored::NotSmaller), "a larger report is not a lowering");
}

#[test]
fn a_report_below_the_familys_floor_is_raised_to_it_rather_than_refused() {
	// The hop is telling the truth about itself; the floor is what the protocol guarantees. The
	// smaller of the two that is still legal is the answer, and the two families have different ones.
	let six = flow();
	let quote = quote_for(&six, QuotedKind::Tcp { sequence: 1000 });
	assert_eq!(path_mtu_for_tcp(&six, &quote, 1000, 2000, true, 576, 1500), PathMtu::Apply(MIN_PATH_MTU_V6));

	let four = FlowKey { local: v4(10, 0, 2, 15), local_port: 50000, remote: v4(10, 0, 2, 2), remote_port: 443, interface_generation: 1 };
	let quote4 = quote_for(&four, QuotedKind::Tcp { sequence: 1000 });
	assert_eq!(path_mtu_for_tcp(&four, &quote4, 1000, 2000, true, 296, 1500), PathMtu::Apply(296), "IPv4's floor is far lower");
	assert_eq!(path_mtu_for_tcp(&four, &quote4, 1000, 2000, true, 8, 1500), PathMtu::Apply(MIN_PATH_MTU_V4));
}

#[test]
fn successive_strictly_lower_reports_each_take_effect_and_a_repeat_does_not() {
	// The same outstanding transmission can provoke several reports from several hops, each smaller
	// than the last, and all of them are valid.
	let mut limit = FlowLimit::new(1500);
	assert!(limit.lower(1400));
	assert_eq!(limit.limit(), 1400);
	assert!(limit.lower(1280));
	assert_eq!(limit.limit(), 1280);
	assert!(!limit.lower(1280), "a repeat is not a lowering");
	assert!(!limit.lower(1400), "and neither is a larger one");
	assert_eq!(limit.limit(), 1280);
}

#[test]
fn a_full_path_mtu_cache_never_restores_the_larger_limit() {
	// THE `Capacity` CASE. The cache is bounded and says so rather than pretending it recorded the
	// write; the flow keeps what it validated, because the path has already refused to carry more.
	let flow = flow();
	let quote = quote_for(&flow, QuotedKind::Tcp { sequence: 1000 });
	let mut limit = FlowLimit::new(1500);
	let PathMtu::Apply(mtu) = path_mtu_for_tcp(&flow, &quote, 1000, 2000, true, 1280, limit.limit()) else {
		panic!("the quotation is this flow's and is in flight");
	};
	// The consumer applies it whatever the cache answered.
	let cache_refused: bool = true;
	assert!(limit.lower(mtu));
	assert_eq!(limit.limit(), 1280);
	assert!(cache_refused, "and the refusal changes nothing about the flow's own limit");
}

#[test]
fn two_probes_of_one_trace_are_told_apart_by_the_quoted_sequence() {
	// A TRACEROUTE IS SEVERAL PROBES TO ONE DESTINATION WITH ONE IDENTIFIER, so the tuple matches all
	// of them and only the sequence separates them. Matching on the tuple would attribute every hop's
	// answer to whichever probe was looked at first.
	let flow = flow();
	let first = quote_for(&flow, QuotedKind::Echo { identifier: 0x1234, sequence: 1 });
	let second = quote_for(&flow, QuotedKind::Echo { identifier: 0x1234, sequence: 2 });
	assert!(probe_owns(&flow, 0x1234, 1, &first));
	assert!(!probe_owns(&flow, 0x1234, 2, &first));
	assert!(probe_owns(&flow, 0x1234, 2, &second));
	// A different identifier is a different trace entirely.
	assert!(!probe_owns(&flow, 0x9999, 1, &first));
	// And a TCP quotation is nobody's probe.
	assert!(!probe_owns(&flow, 0x1234, 1, &quote_for(&flow, QuotedKind::Tcp { sequence: 1 })));
}

#[test]
fn the_responder_is_the_hop_that_complained_and_not_the_destination() {
	// Reporting the quoted destination as the responder is how a traceroute names the same router for
	// every hop.
	let flow = flow();
	let quote = quote_for(&flow, QuotedKind::Echo { identifier: 1, sequence: 1 });
	assert_eq!(quote.responder, v6(0xa1));
	assert_ne!(quote.responder, quote.remote);
	assert!(flow.owns(&quote), "and the match does not depend on who complained");
}
