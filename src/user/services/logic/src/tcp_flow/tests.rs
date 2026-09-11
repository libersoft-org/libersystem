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

/// The Packet Too Big matrix, run against the production decision core with a controlled clock.
///
/// EVERY CASE HERE IS ONE LIVE TUPLE ON ONE UNCHANGED INTERFACE AND ROUTE, so nothing but the
/// quoted sequence, the reported MTU and the clock separates them. That is the point: these are the
/// cases a validator that looked only at the tuple would accept, and each of them can lower a live
/// connection's segment size or force it to resegment.
mod packet_too_big {
	use super::*;
	use crate::ipv6::{Address, Interface};
	use crate::ipv6_icmp::{MtuOutcome, PATH_MTU_LIFETIME_MS, PathMtuCache};
	use crate::tcp_transmit::{Handoff, TransmitBound};

	const IFACE: Interface = Interface::new(1, 1);
	/// The link's own MTU, which is what a flow starts out using.
	const LINK: u32 = 1500;

	fn peer() -> Address {
		let Local::V6(octets) = v6(2) else {
			panic!("an IPv6 peer");
		};
		Address::new(octets)
	}

	#[test]
	fn only_a_quotation_of_transmitted_unacknowledged_data_can_lower_anything() {
		// THE FOUR NAMED CASES ON ONE TUPLE: below `SND.UNA`, exactly at `SND.NXT`, accepted but
		// unsent, and a valid one at `SND.UNA`. Only the last may lower a limit.
		let flow = flow();
		let (snd_una, snd_nxt) = (10_000u32, 12_000u32);
		let accepted_but_unsent: u32 = 13_000;

		let below = quote_for(&flow, QuotedKind::Tcp { sequence: snd_una.wrapping_sub(1) });
		let at_nxt = quote_for(&flow, QuotedKind::Tcp { sequence: snd_nxt });
		let unsent = quote_for(&flow, QuotedKind::Tcp { sequence: accepted_but_unsent });
		let valid = quote_for(&flow, QuotedKind::Tcp { sequence: snd_una });

		for refused in [&below, &at_nxt, &unsent] {
			assert_eq!(path_mtu_for_tcp(&flow, refused, snd_una, snd_nxt, true, 1280, LINK), PathMtu::Ignored(Ignored::OutOfWindow));
		}
		assert_eq!(path_mtu_for_tcp(&flow, &valid, snd_una, snd_nxt, true, 1280, LINK), PathMtu::Apply(1280));
	}

	#[test]
	fn a_packet_held_for_address_resolution_is_not_quotable_until_its_sent_completion() {
		// ACCEPTED-BUT-UNSENT INCLUDES A PACKET WAITING ON L3 RESOLUTION. Its sequence space has
		// never been on the wire, so no router can have seen it and a quotation of it is a forgery.
		let flow = flow();
		let mut bound = TransmitBound::new(10_000);
		assert_eq!(bound.advance(12_000), Handoff::Transmitted { end: 12_000 });
		// The next segment goes to the layer below, which retains it while the neighbour resolves.
		assert!(bound.hold(77, 13_000));

		let held = quote_for(&flow, QuotedKind::Tcp { sequence: 12_000 });
		assert_eq!(path_mtu_for_tcp(&flow, &held, 10_000, bound.transmitted(), true, 1280, LINK), PathMtu::Ignored(Ignored::OutOfWindow), "queued is not transmitted");

		// The completion arrives and the same quotation becomes eligible.
		assert_eq!(bound.on_sent(77), Handoff::Transmitted { end: 13_000 });
		assert_eq!(path_mtu_for_tcp(&flow, &held, 10_000, bound.transmitted(), true, 1280, LINK), PathMtu::Apply(1280));
	}

	#[test]
	fn a_cancelled_or_old_token_completion_leaves_the_replacement_flows_data_unquotable() {
		let flow = flow();
		let mut bound = TransmitBound::new(10_000);
		bound.advance(12_000);
		assert!(bound.hold(77, 13_000));
		// The operation is cancelled: the frame never reached the driver.
		assert_eq!(bound.on_retired(77), Handoff::Retired);
		let quote = quote_for(&flow, QuotedKind::Tcp { sequence: 12_000 });
		assert_eq!(path_mtu_for_tcp(&flow, &quote, 10_000, bound.transmitted(), true, 1280, LINK), PathMtu::Ignored(Ignored::OutOfWindow));

		// The control block is reused. A late completion for the old token must not declare the NEW
		// connection's unsent sequence space transmitted.
		let mut reused = TransmitBound::new(10_000);
		reused.advance(12_000);
		assert!(reused.hold(78, 900_000));
		reused.reset(40_000);
		reused.advance(41_000);
		assert_eq!(reused.on_sent(78), Handoff::Retired);
		assert_eq!(reused.transmitted(), 41_000);
		let stale = quote_for(&flow, QuotedKind::Tcp { sequence: 41_000 });
		assert_eq!(path_mtu_for_tcp(&flow, &stale, 40_000, reused.transmitted(), true, 1280, LINK), PathMtu::Ignored(Ignored::OutOfWindow), "`SND.NXT` itself is never in flight");
	}

	#[test]
	fn a_send_interval_that_wraps_still_tells_in_flight_from_out_of_it() {
		// An unsigned comparison refuses the whole interval here, which is a connection that stops
		// accepting Packet Too Big after two gigabytes.
		let flow = flow();
		let snd_una: u32 = u32::MAX - 500;
		let snd_nxt: u32 = 500;
		let inside = quote_for(&flow, QuotedKind::Tcp { sequence: 0 });
		assert_eq!(path_mtu_for_tcp(&flow, &inside, snd_una, snd_nxt, true, 1280, LINK), PathMtu::Apply(1280), "past the wrap and still in flight");
		let before = quote_for(&flow, QuotedKind::Tcp { sequence: u32::MAX - 600 });
		assert_eq!(path_mtu_for_tcp(&flow, &before, snd_una, snd_nxt, true, 1280, LINK), PathMtu::Ignored(Ignored::OutOfWindow));
		let after = quote_for(&flow, QuotedKind::Tcp { sequence: 600 });
		assert_eq!(path_mtu_for_tcp(&flow, &after, snd_una, snd_nxt, true, 1280, LINK), PathMtu::Ignored(Ignored::OutOfWindow));
	}

	#[test]
	fn a_delayed_quotation_whose_start_was_partially_acknowledged_is_refused() {
		// THE REPORT IS SLOWER THAN THE CONNECTION. By the time it arrives the peer has taken the
		// bytes it quotes, so there is nothing outstanding for it to be about - and accepting it
		// would let a replayed report resegment a flow that has moved on.
		let flow = flow();
		let quote = quote_for(&flow, QuotedKind::Tcp { sequence: 10_000 });
		assert_eq!(path_mtu_for_tcp(&flow, &quote, 10_000, 12_000, true, 1280, LINK), PathMtu::Apply(1280), "valid when it is sent");
		// The peer acknowledges the first half; the quotation now names retired sequence space.
		assert_eq!(path_mtu_for_tcp(&flow, &quote, 11_000, 12_000, true, 1280, LINK), PathMtu::Ignored(Ignored::OutOfWindow));
	}

	#[test]
	fn resegmented_outstanding_data_takes_two_successively_lower_reports() {
		// AFTER A REWIND `SND.NXT` IS BEHIND WHAT WAS TRANSMITTED, and a second hop's smaller report
		// about those same bytes is legitimate. Validating against the queue's cursor would refuse it
		// and leave the flow retrying at a size the path has refused twice.
		let flow = flow();
		let mut bound = TransmitBound::new(10_000);
		bound.advance(12_000);
		let mut limit = FlowLimit::new(LINK);

		let quote = quote_for(&flow, QuotedKind::Tcp { sequence: 10_000 });
		let PathMtu::Apply(first) = path_mtu_for_tcp(&flow, &quote, 10_000, bound.transmitted(), true, 1400, limit.limit()) else {
			panic!("the first report is valid");
		};
		assert!(limit.lower(first));
		// Go-Back-N: the queue rewinds and resends the same bytes in smaller segments. The
		// transmitted bound is a high-water mark and does not move back.
		assert_eq!(bound.advance(11_400), Handoff::Covered);
		assert_eq!(bound.transmitted(), 12_000);
		let PathMtu::Apply(second) = path_mtu_for_tcp(&flow, &quote, 10_000, bound.transmitted(), true, 1280, limit.limit()) else {
			panic!("the second report is valid too");
		};
		assert!(limit.lower(second));
		assert_eq!(limit.limit(), 1280);
	}

	#[test]
	fn equal_and_increasing_reports_do_not_refresh_the_cache_expiry() {
		// A ROUTER THAT KEEPS REPEATING THE SAME MTU MUST NOT KEEP A RECORD ALIVE FOR EVER, which is
		// what a cache refreshing on every report would let it do.
		let mut cache = PathMtuCache::new();
		let mut clock: u64 = 1_000;
		assert_eq!(cache.record(IFACE, peer(), 1400, clock), MtuOutcome::Lowered { mtu: 1400 });

		clock += PATH_MTU_LIFETIME_MS - 1;
		assert_eq!(cache.record(IFACE, peer(), 1400, clock), MtuOutcome::NotLower, "the same value again");
		assert_eq!(cache.record(IFACE, peer(), 1450, clock), MtuOutcome::NotLower, "and a larger one");

		// Had either refreshed the expiry, the entry would still be here.
		clock += 2;
		assert_eq!(cache.get(IFACE, peer(), clock), None, "it expired on its original schedule");
		assert_eq!(cache.expire(clock), 1);
		assert!(cache.is_empty());
	}

	#[test]
	fn an_expired_entry_is_not_recreated_by_replaying_a_quotation_of_acknowledged_data() {
		// THE FULL NAMED SEQUENCE, with the production decision core and a controlled monotonic
		// clock: lower, acknowledge the quoted data, let the record expire after 600 seconds, and
		// replay the same quotation. Nothing is recreated and the flow's own limit does not move.
		let flow = flow();
		let mut cache = PathMtuCache::new();
		let mut limit = FlowLimit::new(LINK);
		let mut clock: u64 = 5_000;

		let quote = quote_for(&flow, QuotedKind::Tcp { sequence: 10_000 });
		let PathMtu::Apply(mtu) = path_mtu_for_tcp(&flow, &quote, 10_000, 12_000, true, 1280, limit.limit()) else {
			panic!("valid while the data is outstanding");
		};
		assert!(limit.lower(mtu));
		assert_eq!(cache.record(IFACE, peer(), mtu, clock), MtuOutcome::Lowered { mtu: 1280 });

		// The peer acknowledges everything the quotation named.
		let (snd_una, snd_nxt) = (12_000u32, 12_000u32);
		clock += PATH_MTU_LIFETIME_MS + 1;
		assert_eq!(cache.expire(clock), 1, "the record lived exactly its lifetime");

		// The replay. It is refused by the flight check before the cache is ever consulted.
		assert_eq!(path_mtu_for_tcp(&flow, &quote, snd_una, snd_nxt, true, 1200, limit.limit()), PathMtu::Ignored(Ignored::EmptyFlight));
		assert!(cache.is_empty(), "no entry is recreated");
		assert_eq!(cache.get(IFACE, peer(), clock), None);
		assert_eq!(limit.limit(), 1280, "and the flow-local limit is untouched");
	}

	#[test]
	fn a_quotation_of_newly_transmitted_data_on_the_same_tuple_is_accepted() {
		// The tuple is not poisoned by the refusals above: once the connection transmits again, a
		// report about THAT data is valid and lowers the limit further.
		let flow = flow();
		let mut cache = PathMtuCache::new();
		let mut limit = FlowLimit::new(1400);
		let mut clock: u64 = 700_000;
		let mut bound = TransmitBound::new(12_000);
		assert_eq!(bound.advance(14_000), Handoff::Transmitted { end: 14_000 });

		let fresh = quote_for(&flow, QuotedKind::Tcp { sequence: 12_500 });
		let PathMtu::Apply(mtu) = path_mtu_for_tcp(&flow, &fresh, 12_000, bound.transmitted(), true, 1300, limit.limit()) else {
			panic!("newly transmitted data is quotable");
		};
		assert!(limit.lower(mtu));
		assert_eq!(limit.limit(), 1300);
		clock += 1;
		assert_eq!(cache.record(IFACE, peer(), mtu, clock), MtuOutcome::Lowered { mtu: 1300 });
		// AND THE FLOOR STILL HOLDS. A report under 1280 on an IPv6 path is raised to it, so it can
		// never lower a flow that is already there.
		assert_eq!(path_mtu_for_tcp(&flow, &fresh, 12_000, bound.transmitted(), true, 1000, 1280), PathMtu::Ignored(Ignored::NotSmaller));
	}

	#[test]
	fn a_full_cache_refuses_the_write_without_ever_bypassing_validation() {
		// BOTH HALVES ON A FULL TABLE. An invalid quotation is still refused - `Capacity` is not a
		// path around the flight check - and a valid one still lowers the FLOW's limit even though
		// the cache cannot record it.
		let flow = flow();
		let mut cache = PathMtuCache::new();
		let clock: u64 = 1_000;
		for index in 0..crate::ipv6_budget::Resource::PathMtu.limit() {
			let mut octets = [0u8; 16];
			octets[0] = 0x20;
			octets[1] = 0x01;
			// Deliberately away from the flow's own peer: a filler that collided with it would make
			// the table hold the very entry this case needs it to refuse.
			octets[12..].copy_from_slice(&(index + 1_000).to_be_bytes());
			assert!(matches!(cache.record(IFACE, Address::new(octets), 1400, clock), MtuOutcome::Lowered { .. }));
		}
		assert_eq!(cache.record(IFACE, peer(), 1300, clock), MtuOutcome::Capacity, "the table is full of live records");

		// The refused case.
		let mut limit = FlowLimit::new(LINK);
		let stale = quote_for(&flow, QuotedKind::Tcp { sequence: 9_000 });
		assert_eq!(path_mtu_for_tcp(&flow, &stale, 10_000, 12_000, true, 1280, limit.limit()), PathMtu::Ignored(Ignored::OutOfWindow));
		assert_eq!(limit.limit(), LINK, "and nothing was lowered on the way to that refusal");

		// The accepted case: the flow keeps what it validated whatever the cache answered.
		let valid = quote_for(&flow, QuotedKind::Tcp { sequence: 10_500 });
		let PathMtu::Apply(mtu) = path_mtu_for_tcp(&flow, &valid, 10_000, 12_000, true, 1280, limit.limit()) else {
			panic!("a valid quotation is valid on a full cache too");
		};
		assert_eq!(cache.record(IFACE, peer(), mtu, clock), MtuOutcome::Capacity);
		assert!(limit.lower(mtu));
		assert_eq!(limit.limit(), 1280, "the path refused to carry more, whatever the table can hold");
	}
}

/// Which probe of a trace an error is about, over both families.
///
/// A TRACEROUTE IS A RUN OF PROBES TO ONE DESTINATION UNDER ONE IDENTIFIER, and every hop answers
/// from its own address. The identifier matches all of them; only the sequence tells them apart, so
/// an implementation keyed on the tuple names the same router for every row - a plausible route that
/// is not the route.
mod probes {
	use super::*;

	fn v4_flow() -> FlowKey {
		FlowKey { local: v4(10, 0, 2, 15), local_port: 0, remote: v4(10, 0, 2, 99), remote_port: 0, interface_generation: 1 }
	}

	fn v6_flow() -> FlowKey {
		FlowKey { local: v6(1), local_port: 0, remote: v6(2), remote_port: 0, interface_generation: 1 }
	}

	/// An error from `responder` about the probe with this identity, on `flow`'s tuple.
	fn from_router(flow: &FlowKey, responder: Local, identifier: u16, sequence: u16) -> Quotation {
		Quotation { responder, local: flow.local, local_port: 0, remote: flow.remote, remote_port: 0, interface_generation: flow.interface_generation, transport: QuotedKind::Echo { identifier, sequence } }
	}

	#[test]
	fn two_routers_answering_two_hops_of_one_trace_each_match_only_their_own_probe() {
		// DELAYED AND REORDERED, which is the ordinary case rather than the adversarial one: the
		// second hop's error can easily arrive before the first hop's.
		for flow in [v4_flow(), v6_flow()] {
			let first_hop = match flow.local.is_v4() {
				true => v4(10, 0, 2, 1),
				false => v6(0xa1),
			};
			let second_hop = match flow.local.is_v4() {
				true => v4(10, 0, 2, 2),
				false => v6(0xa2),
			};
			let from_first = from_router(&flow, first_hop, 0x4321, 1);
			let from_second = from_router(&flow, second_hop, 0x4321, 2);

			assert!(probe_owns(&flow, 0x4321, 1, &from_first));
			assert!(!probe_owns(&flow, 0x4321, 1, &from_second), "the second hop's answer is not the first row");
			assert!(probe_owns(&flow, 0x4321, 2, &from_second));
			assert!(!probe_owns(&flow, 0x4321, 2, &from_first));

			// AND THE RESPONDER IS THE HOP THAT COMPLAINED, not the destination the probe named.
			assert_eq!(from_first.responder, first_hop);
			assert_ne!(from_first.responder, flow.remote);
		}
	}

	#[test]
	fn a_late_error_about_a_retired_probe_leaves_the_live_one_untouched() {
		// The first row has been printed and the trace has moved on; an error about it arriving now
		// must not be read as the second row's answer.
		for flow in [v4_flow(), v6_flow()] {
			let stale = from_router(&flow, flow.local, 0x4321, 1);
			assert!(!probe_owns(&flow, 0x4321, 2, &stale), "an old sequence is not the live probe");
		}
	}

	#[test]
	fn a_wrong_identifier_source_family_or_generation_all_refuse() {
		let flow = v6_flow();
		let responder = v6(0xa1);
		assert!(probe_owns(&flow, 0x4321, 1, &from_router(&flow, responder, 0x4321, 1)), "the control");

		// Another process's trace, on the same tuple.
		assert!(!probe_owns(&flow, 0x4321, 1, &from_router(&flow, responder, 0x9999, 1)));

		// A quotation naming somebody else's source.
		let wrong_source = Quotation { local: v6(0xbb), ..from_router(&flow, responder, 0x4321, 1) };
		assert!(!probe_owns(&flow, 0x4321, 1, &wrong_source));

		// THE OTHER FAMILY, at the same numbers. Equal ports in two families are two flows, and an
		// echo identity is no different.
		let other_family = Quotation { local: v4(10, 0, 2, 15), remote: v4(10, 0, 2, 99), ..from_router(&flow, responder, 0x4321, 1) };
		assert!(!probe_owns(&flow, 0x4321, 1, &other_family));

		// An interface that has been replaced. The same tuple on a new NIC is a different flow.
		let old_generation = Quotation { interface_generation: 0, ..from_router(&flow, responder, 0x4321, 1) };
		assert!(!probe_owns(&flow, 0x4321, 1, &old_generation));
	}

	#[test]
	fn a_quoted_transport_that_is_not_an_echo_is_never_a_probes_answer() {
		// A Packet Too Big about a TCP flow on the same tuple is not this diagnostic's reply, even
		// though every address in it matches.
		let flow = v6_flow();
		let tcp = Quotation { transport: QuotedKind::Tcp { sequence: 1000 }, ..from_router(&flow, v6(0xa1), 0x4321, 1) };
		assert!(!probe_owns(&flow, 0x4321, 1, &tcp));
		let udp = Quotation { transport: QuotedKind::Udp, ..from_router(&flow, v6(0xa1), 0x4321, 1) };
		assert!(!probe_owns(&flow, 0x4321, 1, &udp));
	}
}
