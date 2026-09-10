// The parser's job is to be strict about what it accepts and specific about what it refuses.
//
// Two cases decide the shape of everything above: a padded Ethernet frame must not turn its padding
// into payload, and an extension chain must be bounded whatever the sender writes into it.

use super::*;
use crate::ipv6::{ALL_NODES, Address, UNSPECIFIED};
use alloc::vec;
use alloc::vec::Vec;

fn address(groups: [u16; 8]) -> Address {
	let mut bytes = [0u8; 16];
	for (index, group) in groups.iter().enumerate() {
		bytes[index * 2..index * 2 + 2].copy_from_slice(&group.to_be_bytes());
	}
	Address::new(bytes)
}

fn host() -> Address {
	address([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1])
}

fn peer() -> Address {
	address([0x2001, 0xdb8, 0, 0, 0, 0, 0, 2])
}

/// An L3 packet with a hand-written body, so a test can lie about Payload Length.
fn packet_with(next_header: u8, body: &[u8], declared: u16) -> Vec<u8> {
	let mut out = vec![0x60, 0, 0, 0];
	out.extend_from_slice(&declared.to_be_bytes());
	out.push(next_header);
	out.push(64);
	out.extend_from_slice(&peer().octets());
	out.extend_from_slice(&host().octets());
	out.extend_from_slice(body);
	out
}

fn packet(next_header: u8, body: &[u8]) -> Vec<u8> {
	packet_with(next_header, body, body.len() as u16)
}

#[test]
fn the_fixed_header_decodes_every_field() {
	let mut raw = packet(NEXT_UDP, &[1, 2, 3, 4]);
	raw[0] = 0x6a;
	raw[1] = 0x51;
	raw[2] = 0x23;
	raw[3] = 0x45;
	let header = header(&raw).expect("a header");
	assert_eq!(header.traffic_class, 0xa5);
	assert_eq!(header.flow_label, 0x12345);
	assert_eq!(header.payload_len, 4);
	assert_eq!(header.next_header, NEXT_UDP);
	assert_eq!(header.hop_limit, 64);
	assert_eq!(header.source, peer());
	assert_eq!(header.destination, host());
}

#[test]
fn a_version_that_is_not_six_is_refused_and_a_short_packet_says_so() {
	let mut raw = packet(NEXT_UDP, &[]);
	raw[0] = 0x40;
	assert_eq!(header(&raw), Err(Refusal::BadVersion));
	assert_eq!(header(&raw[..20]), Err(Refusal::TooShort));
}

#[test]
fn ethernet_padding_is_padding_and_not_payload() {
	// A four-byte UDP payload in a frame padded to the sixty-byte Ethernet minimum.
	let l3 = packet(NEXT_UDP, &[9, 9, 9, 9]);
	let mut frame = Vec::new();
	frame.extend_from_slice(&[0x33, 0x33, 0, 0, 0, 1]);
	frame.extend_from_slice(&[0x52, 0x54, 0, 1, 2, 3]);
	frame.extend_from_slice(&ETHERTYPE_IPV6.to_be_bytes());
	frame.extend_from_slice(&l3);
	while frame.len() < 60 {
		frame.push(0);
	}
	let parsed = parse_frame(&frame).expect("accepted");
	assert_eq!(parsed.payload, &[9, 9, 9, 9], "the padding is not payload");
	assert_eq!(parsed.upper, NEXT_UDP);

	// AND AN EMPTY PAYLOAD IS STILL VALID, which a "no trailing bytes" rule would have refused.
	let empty = packet(NEXT_NONE, &[]);
	let mut short_frame = Vec::new();
	short_frame.extend_from_slice(&[0x33, 0x33, 0, 0, 0, 1]);
	short_frame.extend_from_slice(&[0x52, 0x54, 0, 1, 2, 3]);
	short_frame.extend_from_slice(&ETHERTYPE_IPV6.to_be_bytes());
	short_frame.extend_from_slice(&empty);
	while short_frame.len() < 60 {
		short_frame.push(0);
	}
	let parsed = parse_frame(&short_frame).expect("an empty payload is a packet");
	assert!(parsed.payload.is_empty());
}

#[test]
fn a_payload_length_longer_than_the_frame_is_refused() {
	let raw = packet_with(NEXT_UDP, &[1, 2, 3, 4], 40);
	assert_eq!(parse_packet(&raw), Err(Refusal::PayloadTruncated));
}

#[test]
fn a_frame_that_is_not_ipv6_is_refused_by_ethertype() {
	let mut frame = vec![0u8; 60];
	frame[12] = 0x08;
	frame[13] = 0x00;
	assert_eq!(parse_frame(&frame).unwrap_err(), Refusal::NotIpv6);
	assert_eq!(parse_frame(&frame[..10]).unwrap_err(), Refusal::TooShort);
}

#[test]
fn a_multicast_source_is_refused_and_the_unspecified_source_is_allowed_for_dad() {
	let mut raw = packet(NEXT_ICMPV6, &[135, 0, 0, 0]);
	raw[8..24].copy_from_slice(&ALL_NODES.octets());
	assert_eq!(parse_packet(&raw), Err(Refusal::BadSource));

	let mut dad = packet(NEXT_ICMPV6, &[135, 0, 0, 0]);
	dad[8..24].copy_from_slice(&UNSPECIFIED.octets());
	assert!(parse_packet(&dad).is_ok(), "a DAD solicitation is sourced from ::");

	let mut bad_destination = packet(NEXT_UDP, &[]);
	bad_destination[24..40].copy_from_slice(&UNSPECIFIED.octets());
	assert_eq!(parse_packet(&bad_destination), Err(Refusal::BadDestination));
}

/// A Hop-by-Hop or Destination Options header: next header, length in 8-byte units minus one, then
/// options padded to that length.
fn options_header(next: u8, options: &[u8]) -> Vec<u8> {
	let mut out = vec![next, 0];
	out.extend_from_slice(options);
	while (out.len() % 8) != 0 {
		out.push(0);
	}
	out[1] = (out.len() / 8 - 1) as u8;
	out
}

#[test]
fn the_chain_is_walked_and_the_payload_starts_after_it() {
	let mut body = options_header(NEXT_UDP, &[1, 2, 0, 0]);
	let header_len = body.len();
	body.extend_from_slice(&[7, 7, 7, 7]);
	let raw = packet(NEXT_DESTINATION, &body);
	let parsed = parse_packet(&raw).expect("accepted");
	assert_eq!(parsed.upper, NEXT_UDP);
	assert_eq!(parsed.payload, &[7, 7, 7, 7]);
	assert_eq!(parsed.extension_bytes, header_len);
}

#[test]
fn a_chain_longer_than_the_bound_is_refused_rather_than_walked() {
	// Nine Destination Options headers, each the minimum eight bytes.
	let mut body = Vec::new();
	for _ in 0..9 {
		body.extend_from_slice(&options_header(NEXT_DESTINATION, &[]));
	}
	let last = body.len() - 8;
	body[last] = NEXT_UDP;
	assert_eq!(parse_packet(&packet(NEXT_DESTINATION, &body)), Err(Refusal::ChainTooLong));

	// And a chain WITHIN the header count but past the byte bound: five headers of sixty-four bytes
	// is 320, which no legitimate packet on this link needs and which the walk must not follow.
	let mut fat = Vec::new();
	for index in 0..5 {
		let next = if index == 4 { NEXT_UDP } else { NEXT_DESTINATION };
		// Next Header, Hdr Ext Len in 8-byte units minus one, then a PadN filling the rest.
		let mut header = vec![next, 7, 1, 60];
		header.resize(64, 0);
		fat.extend_from_slice(&header);
	}
	let raw = packet(NEXT_DESTINATION, &fat);
	assert_eq!(parse_packet(&raw), Err(Refusal::ChainTooLarge));
}

#[test]
fn a_header_that_runs_past_the_packet_is_malformed() {
	// Declares four 8-byte units and supplies one.
	let body = vec![NEXT_UDP, 3, 0, 0, 0, 0, 0, 0];
	assert_eq!(parse_packet(&packet(NEXT_DESTINATION, &body)), Err(Refusal::MalformedChain));
	assert_eq!(parse_packet(&packet(NEXT_DESTINATION, &[NEXT_UDP])), Err(Refusal::MalformedChain));
}

#[test]
fn the_unknown_option_action_bits_decide_the_answer_and_multicast_only_changes_one_of_them() {
	for (option_type, action) in [(0x00u8, OptionAction::Skip), (0x40, OptionAction::Discard), (0x80, OptionAction::DiscardAndReportAlways), (0xc0, OptionAction::DiscardAndReportUnicast)] {
		let mut body = options_header(NEXT_UDP, &[option_type, 2, 0, 0]);
		body.extend_from_slice(&[1, 2, 3, 4]);
		let raw = packet(NEXT_DESTINATION, &body);
		let outcome = parse_packet(&raw);
		if action == OptionAction::Skip {
			assert!(outcome.is_ok(), "action 00 steps over the option");
			continue;
		}
		match outcome {
			Err(Refusal::UnknownOption { action: reported, offset }) => {
				assert_eq!(reported, action);
				assert_eq!(offset, (HEADER_LEN + 2) as u32, "the pointer is the option's own offset");
			}
			other => panic!("expected an unknown-option refusal, got {other:?}"),
		}
	}

	// THE TWO REPORTING ACTIONS DIFFER ONLY HERE, which is why a blanket multicast rule is wrong.
	assert!(OptionAction::DiscardAndReportAlways.reports(ALL_NODES), "action 10 reports even to a group");
	assert!(!OptionAction::DiscardAndReportUnicast.reports(ALL_NODES), "action 11 does not");
	assert!(OptionAction::DiscardAndReportAlways.reports(host()));
	assert!(OptionAction::DiscardAndReportUnicast.reports(host()));
	assert!(!OptionAction::Skip.reports(host()));
	assert!(!OptionAction::Discard.reports(host()));
}

#[test]
fn a_jumbo_payload_is_refused_by_name() {
	let body = options_header(NEXT_UDP, &[OPTION_JUMBO_PAYLOAD_TEST, 4, 0, 1, 0, 0]);
	assert_eq!(parse_packet(&packet_with(NEXT_HOP_BY_HOP, &body, body.len() as u16)), Err(Refusal::Jumbogram));
}

/// The Jumbo Payload option type, spelled here so the test does not depend on a private constant.
const OPTION_JUMBO_PAYLOAD_TEST: u8 = 0xc2;

#[test]
fn a_deprecated_routing_header_is_refused_and_a_segmented_one_too() {
	let type_zero = vec![NEXT_UDP, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(parse_packet(&packet(NEXT_ROUTING, &type_zero)), Err(Refusal::DeprecatedRouting));
	let segments_left = vec![NEXT_UDP, 0, 4, 1, 0, 0, 0, 0];
	assert_eq!(parse_packet(&packet(NEXT_ROUTING, &segments_left)), Err(Refusal::DeprecatedRouting));
	// Type 4 with nothing left to do is a header this host may step over.
	let mut spent = vec![NEXT_UDP, 0, 4, 0, 0, 0, 0, 0];
	spent.extend_from_slice(&[5, 5]);
	let raw = packet(NEXT_ROUTING, &spent);
	let parsed = parse_packet(&raw).expect("accepted");
	assert_eq!(parsed.payload, &[5, 5]);
}

/// A fragment header: next header, reserved, offset and flags, identification.
fn fragment_header(next: u8, offset_units: u16, more: bool) -> Vec<u8> {
	let field = (offset_units << 3) | u16::from(more);
	let mut out = vec![next, 0];
	out.extend_from_slice(&field.to_be_bytes());
	out.extend_from_slice(&[0, 0, 0, 1]);
	out
}

#[test]
fn an_atomic_fragment_is_the_whole_packet_and_any_other_fragment_is_refused() {
	let mut atomic = fragment_header(NEXT_UDP, 0, false);
	atomic.extend_from_slice(&[4, 4, 4, 4]);
	let raw = packet(NEXT_FRAGMENT, &atomic);
	let parsed = parse_packet(&raw).expect("an atomic fragment is complete");
	assert!(parsed.atomic_fragment);
	assert_eq!(parsed.upper, NEXT_UDP);
	assert_eq!(parsed.payload, &[4, 4, 4, 4]);

	let mut more = fragment_header(NEXT_UDP, 0, true);
	more.extend_from_slice(&[4, 4, 4, 4]);
	assert_eq!(parse_packet(&packet(NEXT_FRAGMENT, &more)), Err(Refusal::NonAtomicFragment), "the named gap, counted rather than dropped");

	let mut later = fragment_header(NEXT_UDP, 1, false);
	later.extend_from_slice(&[4, 4, 4, 4]);
	assert_eq!(parse_packet(&packet(NEXT_FRAGMENT, &later)), Err(Refusal::NonAtomicFragment));

	let truncated = vec![NEXT_UDP, 0, 0];
	assert_eq!(parse_packet(&packet(NEXT_FRAGMENT, &truncated)), Err(Refusal::MalformedChain));
}

#[test]
fn neighbour_discovery_behind_a_fragment_header_is_refused_even_when_atomic() {
	for message_type in [133u8, 134, 135, 136, 137] {
		let mut body = fragment_header(NEXT_ICMPV6, 0, false);
		body.extend_from_slice(&[message_type, 0, 0, 0]);
		assert_eq!(parse_packet(&packet(NEXT_FRAGMENT, &body)), Err(Refusal::FragmentedNeighbourDiscovery), "RFC 6980, type {message_type}");
	}
	// An echo request behind an atomic fragment is not Neighbour Discovery and is accepted.
	let mut echo = fragment_header(NEXT_ICMPV6, 0, false);
	echo.extend_from_slice(&[128, 0, 0, 0]);
	assert!(parse_packet(&packet(NEXT_FRAGMENT, &echo)).is_ok());
}

#[test]
fn a_destination_is_ours_only_when_it_is_ours() {
	let groups = [host().solicited_node()];
	let unicast = [host()];
	assert!(destination_is_ours(host(), &unicast, &groups));
	assert!(destination_is_ours(host().solicited_node(), &unicast, &groups));
	assert!(destination_is_ours(ALL_NODES, &unicast, &groups), "every host answers all-nodes");
	assert!(!destination_is_ours(peer(), &unicast, &groups));
	assert!(!destination_is_ours(peer().solicited_node(), &unicast, &groups));
}

#[test]
fn building_a_packet_refuses_a_payload_the_path_cannot_carry() {
	let payload = [7u8; 100];
	let built = build_packet(host(), peer(), NEXT_UDP, 64, &payload, 1500).expect("built");
	assert_eq!(built.len(), HEADER_LEN + payload.len());
	let parsed = parse_packet(&built).expect("what we build, we parse");
	assert_eq!(parsed.header.source, host());
	assert_eq!(parsed.header.destination, peer());
	assert_eq!(parsed.header.hop_limit, 64);
	assert_eq!(parsed.payload, &payload);

	let big = [0u8; 1300];
	match build_packet(host(), peer(), NEXT_UDP, 64, &big, 1280) {
		Err(EgressRefusal::TooLarge { limit }) => assert_eq!(limit, 1280),
		other => panic!("expected a too-large refusal, got {other:?}"),
	}
	// AN MTU BELOW THE IPv6 MINIMUM IS NOT A SMALLER MTU. A link that cannot carry 1280 cannot
	// carry IPv6, and the floor is applied rather than believed.
	match build_packet(host(), peer(), NEXT_UDP, 64, &big, 576) {
		Err(EgressRefusal::TooLarge { limit }) => assert_eq!(limit, MIN_MTU),
		other => panic!("expected the floor, got {other:?}"),
	}
	assert_eq!(build_packet(ALL_NODES, peer(), NEXT_UDP, 64, &[], 1500), Err(EgressRefusal::BadAddress));
	assert_eq!(build_packet(host(), UNSPECIFIED, NEXT_UDP, 64, &[], 1500), Err(EgressRefusal::BadAddress));
}

#[test]
fn a_built_frame_carries_the_ethertype_and_round_trips() {
	let frame = build_frame([0x33, 0x33, 0, 0, 0, 1], [0x52, 0x54, 0, 1, 2, 3], host(), peer(), NEXT_UDP, 64, &[1, 2], 1500).expect("built");
	assert_eq!(u16::from_be_bytes([frame[12], frame[13]]), ETHERTYPE_IPV6);
	let parsed = parse_frame(&frame).expect("accepted");
	assert_eq!(parsed.payload, &[1, 2]);
}

#[test]
fn the_pseudo_header_checksum_covers_the_addresses() {
	let message = [0x80u8, 0, 0, 0, 1, 2, 3, 4];
	let ours = pseudo_header_checksum(host(), peer(), NEXT_ICMPV6, &message);
	let elsewhere = pseudo_header_checksum(host(), address([0x2001, 0xdb8, 0, 0, 0, 0, 0, 3]), NEXT_ICMPV6, &message);
	assert_ne!(ours, elsewhere, "a message delivered to the wrong host does not verify");
	assert_ne!(ours, pseudo_header_checksum(host(), peer(), NEXT_UDP, &message), "the protocol is covered too");

	// Verification: with the computed value written into the message, the sum over the whole thing
	// folds to zero - which is how a receiver checks it.
	let mut checked = message;
	let sum = pseudo_header_checksum(host(), peer(), NEXT_ICMPV6, &checked);
	checked[2..4].copy_from_slice(&sum.to_be_bytes());
	assert_eq!(pseudo_header_checksum(host(), peer(), NEXT_ICMPV6, &checked), 0xffff, "a verified message sums to the all-ones form of zero");

	// An odd-length message pads with a zero byte rather than reading past the end.
	let odd = [1u8, 2, 3];
	assert_ne!(pseudo_header_checksum(host(), peer(), NEXT_UDP, &odd), 0);
}
