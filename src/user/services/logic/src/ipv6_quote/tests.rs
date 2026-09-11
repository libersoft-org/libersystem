//! The quotation seam: what a consumer is handed, and what it must refuse to act on.

use super::*;
use crate::ipv6_events::{ErrorClass, QuotedError, QuotedTransport};
use crate::ipv6_icmp::{MtuOutcome, PATH_MTU_LIFETIME_MS, PathMtuCache};
use alloc::vec::Vec;

fn interface() -> Interface {
	Interface::new(0, 1)
}

fn address(last: u16) -> Address {
	let mut bytes = [0u8; 16];
	bytes[0] = 0x20;
	bytes[1] = 0x01;
	bytes[2] = 0x0d;
	bytes[3] = 0xb8;
	bytes[14..].copy_from_slice(&last.to_be_bytes());
	Address::new(bytes)
}

/// One ICMPv6 error message, quoting a packet of `next_header` carrying `body`.
fn error(message_type: u8, code: u8, extra: u32, source: Address, destination: Address, next_header: u8, body: &[u8]) -> Vec<u8> {
	let mut message = alloc::vec![0u8; ERROR_HEADER_LEN];
	message[0] = message_type;
	message[1] = code;
	message[4..8].copy_from_slice(&extra.to_be_bytes());
	let mut quoted = alloc::vec![0u8; ipv6_packet::HEADER_LEN];
	quoted[0] = 0x60;
	quoted[4..6].copy_from_slice(&(body.len() as u16).to_be_bytes());
	quoted[6] = next_header;
	quoted[7] = 64;
	quoted[8..24].copy_from_slice(&source.octets());
	quoted[24..40].copy_from_slice(&destination.octets());
	message.extend_from_slice(&quoted);
	message.extend_from_slice(body);
	message
}

fn tcp(source_port: u16, destination_port: u16, sequence: u32) -> Vec<u8> {
	let mut body = Vec::new();
	body.extend_from_slice(&source_port.to_be_bytes());
	body.extend_from_slice(&destination_port.to_be_bytes());
	body.extend_from_slice(&sequence.to_be_bytes());
	body
}

fn echo(identifier: u16, sequence: u16) -> Vec<u8> {
	let mut body = alloc::vec![0u8; 8];
	body[0] = crate::ipv6_icmp::ECHO_REQUEST;
	body[4..6].copy_from_slice(&identifier.to_be_bytes());
	body[6..8].copy_from_slice(&sequence.to_be_bytes());
	body
}

#[test]
fn seven_quoted_tcp_bytes_are_refused_and_eight_recover_the_ports_and_the_exact_sequence() {
	// THE EXACT-BOUND PAIR. Seven bytes hold both ports and three quarters of the sequence, which is
	// precisely the quotation an implementation is tempted to accept with the last byte defaulted.
	let full = tcp(1234, 80, 0x1122_3344);
	assert_eq!(transport(&full[..7], ipv6_packet::NEXT_TCP), Err(QuoteRefusal::Short));
	assert_eq!(transport(&full, ipv6_packet::NEXT_TCP), Ok(QuotedTransport::Tcp { source_port: 1234, destination_port: 80, sequence: 0x1122_3344 }));

	// AND ZERO IS A SEQUENCE LIKE ANY OTHER, which is why a defaulted one is indistinguishable from a
	// real one and why the short quote had to be refused rather than filled in.
	assert_eq!(transport(&tcp(1, 2, 0), ipv6_packet::NEXT_TCP), Ok(QuotedTransport::Tcp { source_port: 1, destination_port: 2, sequence: 0 }));

	// A quotation longer than the eight bytes is not a problem: the rest of the header is simply not
	// something this layer reads.
	let mut long = full.clone();
	long.extend_from_slice(&[0u8; 12]);
	assert_eq!(transport(&long, ipv6_packet::NEXT_TCP), Ok(QuotedTransport::Tcp { source_port: 1234, destination_port: 80, sequence: 0x1122_3344 }));
}

#[test]
fn a_short_udp_quotation_is_refused_and_four_bytes_recover_both_ports() {
	let body = [0x30u8, 0x39, 0x00, 0x35];
	assert_eq!(transport(&body[..3], ipv6_packet::NEXT_UDP), Err(QuoteRefusal::Short));
	assert_eq!(transport(&body, ipv6_packet::NEXT_UDP), Ok(QuotedTransport::Udp { source_port: 12345, destination_port: 53 }));
}

#[test]
fn an_echo_quotation_needs_the_whole_header_and_the_request_type_before_it_is_an_identity() {
	let body = echo(0xbeef, 7);
	assert_eq!(transport(&body[..7], ipv6_packet::NEXT_ICMPV6), Err(QuoteRefusal::Short));
	assert_eq!(transport(&body, ipv6_packet::NEXT_ICMPV6), Ok(QuotedTransport::Icmpv6Echo { identifier: 0xbeef, sequence: 7 }));

	// A QUOTED MESSAGE OF ANOTHER KIND CARRIES NO ECHO IDENTITY. The identifier and sequence offsets
	// are meaningful only in an echo, so reading them out of a quoted listener report would hand a
	// probe an error about a packet it never sent.
	let mut report = body.clone();
	report[0] = crate::ipv6_icmp::MLD_REPORT_V2;
	assert_eq!(transport(&report, ipv6_packet::NEXT_ICMPV6), Ok(QuotedTransport::Other { next_header: ipv6_packet::NEXT_ICMPV6 }));
	let mut wrong_code = body.clone();
	wrong_code[1] = 1;
	assert_eq!(transport(&wrong_code, ipv6_packet::NEXT_ICMPV6), Ok(QuotedTransport::Other { next_header: ipv6_packet::NEXT_ICMPV6 }));
	let mut reply = body.clone();
	reply[0] = crate::ipv6_icmp::ECHO_REPLY;
	assert_eq!(transport(&reply, ipv6_packet::NEXT_ICMPV6), Ok(QuotedTransport::Other { next_header: ipv6_packet::NEXT_ICMPV6 }), "a quoted reply is not a probe this host sent");
}

#[test]
fn a_protocol_this_host_does_not_demultiplex_is_an_event_at_any_length() {
	// NOTHING TO TRUNCATE. There is no identity to recover, so there is no short quotation either,
	// and refusing these would throw away a Time Exceeded about a packet a consumer might still care
	// to hear about.
	assert_eq!(transport(&[], 132), Ok(QuotedTransport::Other { next_header: 132 }));
	assert_eq!(transport(&[1, 2, 3], 132), Ok(QuotedTransport::Other { next_header: 132 }));
}

#[test]
fn a_message_is_refused_before_the_quotation_when_it_cannot_carry_one() {
	let ours = address(1);
	let held = [ours];
	let full = error(crate::ipv6_icmp::TIME_EXCEEDED, 0, 0, ours, address(2), ipv6_packet::NEXT_ICMPV6, &echo(1, 1));
	assert_eq!(validate(interface(), address(9), &full[..ERROR_HEADER_LEN + 20], &held), Err(QuoteRefusal::Truncated));

	// A message whose type is not one of the four errors is not an error event at all.
	let mut echo_request = full.clone();
	echo_request[0] = crate::ipv6_icmp::ECHO_REQUEST;
	assert_eq!(validate(interface(), address(9), &echo_request, &held), Err(QuoteRefusal::NotAnError));

	// And a quoted header that is not an IPv6 one stops there rather than being walked.
	let mut bad_version = full.clone();
	bad_version[ERROR_HEADER_LEN] = 0x40;
	assert_eq!(validate(interface(), address(9), &bad_version, &held), Err(QuoteRefusal::Truncated));
}

#[test]
fn an_error_quoting_an_address_this_interface_does_not_hold_reaches_no_consumer() {
	// SOMEBODY ELSE'S COMPLAINT. It is well formed, it names a real destination, and this host never
	// sent the packet it quotes - the cheapest of the checks and the one that removes most of the
	// forgeries.
	let message = error(crate::ipv6_icmp::PACKET_TOO_BIG, 0, 1400, address(77), address(2), ipv6_packet::NEXT_TCP, &tcp(1, 2, 3));
	assert_eq!(validate(interface(), address(9), &message, &[address(1)]), Err(QuoteRefusal::NotOurs));
	assert!(validate(interface(), address(9), &message, &[address(1), address(77)]).is_ok(), "and it is accepted once the address is one of ours");
}

#[test]
fn a_validated_error_names_the_responder_separately_from_the_destination_it_was_going_to() {
	// THE OUTER SENDER IS THE HOP THAT COMPLAINED, and it is neither the quoted destination nor
	// necessarily this host's selected first hop. A consumer that confused the two would report the
	// wrong router for every error that came back from deeper in the path.
	let responder = address(0xfe);
	let message = error(crate::ipv6_icmp::TIME_EXCEEDED, 0, 0, address(1), address(2), ipv6_packet::NEXT_ICMPV6, &echo(0x4242, 3));
	let event = validate(interface(), responder, &message, &[address(1)]).expect("valid");
	assert_eq!(event.reporter, responder);
	assert_eq!(event.quoted_destination, address(2));
	assert_eq!(event.quoted_source, address(1));
	assert_eq!(event.class, ErrorClass::TimeExceeded { code: 0 });
	assert_eq!(event.transport, QuotedTransport::Icmpv6Echo { identifier: 0x4242, sequence: 3 });
	assert_eq!(event.interface, interface());
}

// ---------------------------------------------------------------------------------------------
// The consumer this seam exists for, in miniature. It holds what M0175's TCP will hold - a tuple,
// a send interval and a transmit limit - and nothing else, which is the point: if these cases pass
// against a stand-in this small, the seam carries everything the real consumer needs.
// ---------------------------------------------------------------------------------------------

struct Flow {
	local: Address,
	remote: Address,
	source_port: u16,
	destination_port: u16,
	snd_una: u32,
	snd_nxt: u32,
	limit: u32,
	terminated: bool,
}

impl Flow {
	fn new(local: Address, remote: Address, source_port: u16, destination_port: u16) -> Flow {
		Flow { local, remote, source_port, destination_port, snd_una: 1_000, snd_nxt: 5_000, limit: 1500, terminated: false }
	}

	/// Everything the consumer does with an error, in the order the frozen contract puts it in.
	fn on_error(&mut self, event: &QuotedError, cache: &mut PathMtuCache, now_ms: u64) -> bool {
		let QuotedTransport::Tcp { source_port, destination_port, sequence } = event.transport else {
			return false;
		};
		if event.quoted_source != self.local || event.quoted_destination != self.remote || source_port != self.source_port || destination_port != self.destination_port {
			return false;
		}
		match event.class {
			ErrorClass::PacketTooBig { mtu } => {
				// THE FLIGHT CHECK COMES BEFORE THE WRITE, always. This is the whole reason the
				// layer below does not touch the cache itself.
				if !quotation_is_in_flight(sequence, self.snd_una, self.snd_nxt) {
					return true;
				}
				if let MtuOutcome::Lowered { mtu } = cache.record(event.interface, event.quoted_destination, mtu, now_ms) {
					self.limit = mtu;
				}
			}
			ErrorClass::DestinationUnreachable { .. } => self.terminated = true,
			_ => {}
		}
		true
	}
}

#[test]
fn an_error_quoting_one_flow_is_delivered_against_that_flow_and_leaves_the_other_alone() {
	let mut cache = PathMtuCache::new();
	let ours = address(1);
	let mut flow_a = Flow::new(ours, address(2), 1000, 80);
	let mut flow_b = Flow::new(ours, address(3), 1001, 443);

	let message = error(crate::ipv6_icmp::DESTINATION_UNREACHABLE, 1, 0, ours, address(2), ipv6_packet::NEXT_TCP, &tcp(1000, 80, 2_000));
	let event = validate(interface(), address(9), &message, &[ours]).expect("valid");
	assert!(flow_a.on_error(&event, &mut cache, 0), "flow A owns this quotation");
	assert!(!flow_b.on_error(&event, &mut cache, 0), "and flow B does not");
	assert!(flow_a.terminated);
	assert!(!flow_b.terminated, "an error about another flow neither terminates nor resizes this one");
	assert_eq!(flow_b.limit, 1500);
}

#[test]
fn a_forged_packet_too_big_matching_no_live_flow_does_not_lower_the_path_mtu() {
	// THE NEGATIVE THE OWNERSHIP SPLIT EXISTS FOR. The message is well formed, its quoted source is
	// genuinely one of this interface's addresses, and it quotes a flow that does not exist. Nothing
	// durable may follow from it.
	let mut cache = PathMtuCache::new();
	let ours = address(1);
	let mut flow = Flow::new(ours, address(2), 1000, 80);

	let forged = error(crate::ipv6_icmp::PACKET_TOO_BIG, 0, 1280, ours, address(2), ipv6_packet::NEXT_TCP, &tcp(6666, 7777, 2_000));
	let event = validate(interface(), address(9), &forged, &[ours]).expect("well formed, and that is all");
	assert!(!flow.on_error(&event, &mut cache, 0), "no live flow claims it");
	assert!(cache.is_empty(), "and the layer below wrote nothing on its own");
	assert_eq!(cache.get(interface(), address(2), 0), None);
	assert_eq!(flow.limit, 1500);

	// The same message against the flow it actually names does lower it, which is what proves the
	// negative above is about the correlation and not about the message being unusable.
	let real = error(crate::ipv6_icmp::PACKET_TOO_BIG, 0, 1300, ours, address(2), ipv6_packet::NEXT_TCP, &tcp(1000, 80, 2_000));
	let event = validate(interface(), address(9), &real, &[ours]).expect("valid");
	assert!(flow.on_error(&event, &mut cache, 0));
	assert_eq!(flow.limit, 1300);
	assert_eq!(cache.get(interface(), address(2), 0), Some(1300));
}

#[test]
fn the_send_bound_admits_only_the_sequence_space_that_is_actually_on_the_wire() {
	let mut cache = PathMtuCache::new();
	let ours = address(1);
	let lower = |flow: &mut Flow, cache: &mut PathMtuCache, sequence: u32, mtu: u32| {
		let message = error(crate::ipv6_icmp::PACKET_TOO_BIG, 0, mtu, ours, address(2), ipv6_packet::NEXT_TCP, &tcp(1000, 80, sequence));
		let event = validate(interface(), address(9), &message, &[ours]).expect("valid");
		flow.on_error(&event, cache, 0);
	};

	let mut flow = Flow::new(ours, address(2), 1000, 80);
	lower(&mut flow, &mut cache, 999, 1300);
	assert_eq!(flow.limit, 1500, "below SND.UNA is data the peer has already acknowledged");
	assert!(cache.is_empty());

	lower(&mut flow, &mut cache, 5_000, 1300);
	assert_eq!(flow.limit, 1500, "SND.NXT is the first byte NOT sent, so the interval is half open");
	assert!(cache.is_empty());

	lower(&mut flow, &mut cache, 1_000, 1300);
	assert_eq!(flow.limit, 1300, "the oldest unacknowledged byte is on the wire");
	assert_eq!(cache.get(interface(), address(2), 0), Some(1300));

	// AN EMPTY FLIGHT REJECTS EVERYTHING, including the sequence that would be its own edge.
	assert!(!quotation_is_in_flight(7, 7, 7));
	assert!(!quotation_is_in_flight(6, 7, 7));
	assert!(!quotation_is_in_flight(8, 7, 7));

	// AND A WRAPPED INTERVAL IS ONE INTERVAL. `SND.UNA` near the top of the space with `SND.NXT` past
	// zero: an implementation comparing with `<` and `>` splits this in two and rejects the half that
	// wrapped, which is a live connection's traffic.
	let una = u32::MAX - 100;
	let nxt = 100u32;
	assert!(quotation_is_in_flight(una, una, nxt));
	assert!(quotation_is_in_flight(u32::MAX, una, nxt));
	assert!(quotation_is_in_flight(0, una, nxt), "zero is inside a wrapped flight");
	assert!(quotation_is_in_flight(99, una, nxt));
	assert!(!quotation_is_in_flight(nxt, una, nxt));
	assert!(!quotation_is_in_flight(una - 1, una, nxt));
}

#[test]
fn a_quotation_for_acknowledged_data_replayed_after_the_cache_expiry_neither_reinserts_nor_lowers() {
	// THE REPLAY. A quotation this host once acted on is still well formed forever; what stops it
	// being acted on twice is that the data it quotes is no longer outstanding.
	let mut cache = PathMtuCache::new();
	let ours = address(1);
	let mut flow = Flow::new(ours, address(2), 1000, 80);
	let message = error(crate::ipv6_icmp::PACKET_TOO_BIG, 0, 1300, ours, address(2), ipv6_packet::NEXT_TCP, &tcp(1000, 80, 1_200));
	let event = validate(interface(), address(9), &message, &[ours]).expect("valid");

	flow.on_error(&event, &mut cache, 0);
	assert_eq!(flow.limit, 1300);
	assert_eq!(cache.get(interface(), address(2), 0), Some(1300));

	// An equal report does not push the expiry out - a router repeating itself cannot keep a record
	// alive forever.
	flow.on_error(&event, &mut cache, 1_000);
	assert_eq!(cache.get(interface(), address(2), PATH_MTU_LIFETIME_MS - 1), Some(1300));
	assert_eq!(cache.get(interface(), address(2), PATH_MTU_LIFETIME_MS), None, "and it expires on its original schedule");

	// The peer acknowledges the quoted data and the flight moves past it.
	flow.snd_una = 4_000;
	flow.limit = 1500;
	let replayed = PATH_MTU_LIFETIME_MS + 1;
	flow.on_error(&event, &mut cache, replayed);
	assert_eq!(flow.limit, 1500, "a replayed quotation for acknowledged data lowers nothing");
	assert_eq!(cache.get(interface(), address(2), replayed), None, "and reinserts no record");

	// A FRESH QUOTATION FOR OUTSTANDING DATA STILL DOES BOTH, which is what makes the refusal above a
	// bound on the attacker rather than on discovery.
	let fresh = error(crate::ipv6_icmp::PACKET_TOO_BIG, 0, 1280, ours, address(2), ipv6_packet::NEXT_TCP, &tcp(1000, 80, 4_500));
	let event = validate(interface(), address(9), &fresh, &[ours]).expect("valid");
	flow.on_error(&event, &mut cache, replayed);
	assert_eq!(flow.limit, 1280);
	assert_eq!(cache.get(interface(), address(2), replayed), Some(1280));
}

// ---------------------------------------------------------------------------------------------
// Two probes, two routers, and errors that arrive in the wrong order.
// ---------------------------------------------------------------------------------------------

struct Probe {
	identifier: u16,
	sequence: u16,
	live: bool,
	responder: Option<Address>,
	class: Option<ErrorClass>,
}

impl Probe {
	fn new(identifier: u16, sequence: u16) -> Probe {
		Probe { identifier, sequence, live: true, responder: None, class: None }
	}

	fn on_error(&mut self, event: &QuotedError) -> bool {
		let QuotedTransport::Icmpv6Echo { identifier, sequence } = event.transport else {
			return false;
		};
		if !self.live || identifier != self.identifier || sequence != self.sequence || event.interface != interface() {
			return false;
		}
		self.responder = Some(event.reporter);
		self.class = Some(event.class);
		true
	}
}

#[test]
fn two_routers_answering_successive_probes_are_attributed_by_sequence_and_keep_their_own_responder() {
	// SAME DESTINATION, SAME IDENTIFIER, DIFFERENT SEQUENCES, and the two errors come back out of
	// order from two different hops. Everything that distinguishes them is in the quotation.
	let ours = address(1);
	let held = [ours];
	let first_router = address(0xa1);
	let second_router = address(0xa2);
	let mut probes = [Probe::new(0x1234, 1), Probe::new(0x1234, 2)];

	let second = error(crate::ipv6_icmp::DESTINATION_UNREACHABLE, 3, 0, ours, address(2), ipv6_packet::NEXT_ICMPV6, &echo(0x1234, 2));
	let second = validate(interface(), second_router, &second, &held).expect("valid");
	let first = error(crate::ipv6_icmp::TIME_EXCEEDED, 0, 0, ours, address(2), ipv6_packet::NEXT_ICMPV6, &echo(0x1234, 1));
	let first = validate(interface(), first_router, &first, &held).expect("valid");

	for event in [&second, &first] {
		let claimed = probes.iter_mut().map(|probe| probe.on_error(event)).filter(|claimed| *claimed).count();
		assert_eq!(claimed, 1, "exactly one probe owns each error");
	}
	assert_eq!(probes[0].responder, Some(first_router));
	assert_eq!(probes[0].class, Some(ErrorClass::TimeExceeded { code: 0 }));
	assert_eq!(probes[1].responder, Some(second_router));
	assert_eq!(probes[1].class, Some(ErrorClass::DestinationUnreachable { code: 3 }));
}

#[test]
fn retiring_the_earlier_probe_leaves_the_later_one_untouched() {
	let ours = address(1);
	let held = [ours];
	let mut probes = [Probe::new(0x1234, 1), Probe::new(0x1234, 2)];
	probes[0].live = false;

	let message = error(crate::ipv6_icmp::TIME_EXCEEDED, 0, 0, ours, address(2), ipv6_packet::NEXT_ICMPV6, &echo(0x1234, 1));
	let event = validate(interface(), address(0xa1), &message, &held).expect("valid");
	assert_eq!(probes.iter_mut().map(|probe| probe.on_error(&event)).filter(|claimed| *claimed).count(), 0, "a retired probe's error is nobody's");
	assert_eq!(probes[1].responder, None, "and above all it is not the later probe's");
	assert!(probes[1].live);
}

#[test]
fn a_truncated_echo_quotation_and_a_foreign_generation_both_reach_no_probe() {
	let ours = address(1);
	let held = [ours];
	let mut probes = [Probe::new(0x1234, 1)];

	// SEVEN BYTES OF ECHO. The identifier is in them and the sequence is not, so an implementation
	// that defaulted the missing half would hand this to sequence zero.
	let short = echo(0x1234, 1);
	let message = error(crate::ipv6_icmp::TIME_EXCEEDED, 0, 0, ours, address(2), ipv6_packet::NEXT_ICMPV6, &short[..7]);
	assert_eq!(validate(interface(), address(0xa1), &message, &held), Err(QuoteRefusal::Short));

	// A REPLACED NIC IS A DIFFERENT INTERFACE. The error is valid; it just belongs to a generation
	// this probe does not.
	let message = error(crate::ipv6_icmp::TIME_EXCEEDED, 0, 0, ours, address(2), ipv6_packet::NEXT_ICMPV6, &short);
	let event = validate(Interface::new(0, 2), address(0xa1), &message, &held).expect("valid");
	assert!(!probes[0].on_error(&event));
	assert_eq!(probes[0].responder, None);
}
