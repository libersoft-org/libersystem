// A codec is judged by what it refuses. The zero-length option is the one that matters most: a
// walker that trusted the field would never come back.

use super::*;
use crate::ipv6_icmp::{NEIGHBOUR_ADVERTISEMENT, NEIGHBOUR_SOLICITATION, ROUTER_ADVERTISEMENT, ROUTER_SOLICITATION};
use alloc::vec;

fn address(low: u16) -> Address {
	let mut bytes = [0u8; 16];
	bytes[0] = 0x20;
	bytes[1] = 0x01;
	bytes[2] = 0x0d;
	bytes[3] = 0xb8;
	bytes[14..16].copy_from_slice(&low.to_be_bytes());
	Address::new(bytes)
}

fn mac(last: u8) -> [u8; 6] {
	[0x52, 0x54, 0x00, 0x11, 0x22, last]
}

fn advertisement(options: &[u8]) -> Vec<u8> {
	let mut message = vec![ROUTER_ADVERTISEMENT, 0, 0, 0, 64, 0x08, 0x07, 0x08];
	message.extend_from_slice(&30_000u32.to_be_bytes());
	message.extend_from_slice(&1_000u32.to_be_bytes());
	message.extend_from_slice(options);
	message
}

#[test]
fn a_message_that_could_have_been_forwarded_is_refused() {
	let message = advertisement(&[]);
	assert!(decode_router_advertisement(&message, ND_HOP_LIMIT).is_ok());
	for hop_limit in [254u8, 64, 1, 0] {
		assert_eq!(decode_router_advertisement(&message, hop_limit), Err(NdRefusal::NotFromTheLink), "hop limit {hop_limit}");
	}
	// The same rule on every message type, not on some of them.
	let mut solicitation = vec![NEIGHBOUR_SOLICITATION, 0, 0, 0, 0, 0, 0, 0];
	solicitation.extend_from_slice(&address(1).octets());
	assert_eq!(decode_neighbour_solicitation(&solicitation, 64), Err(NdRefusal::NotFromTheLink));
	let mut reply = vec![NEIGHBOUR_ADVERTISEMENT, 0, 0, 0, 0x60, 0, 0, 0];
	reply.extend_from_slice(&address(1).octets());
	assert_eq!(decode_neighbour_advertisement(&reply, 64), Err(NdRefusal::NotFromTheLink));
}

#[test]
fn a_zero_length_option_is_refused_rather_than_walked() {
	let message = advertisement(&[OPTION_SOURCE_LINK_LAYER, 0, 0, 0, 0, 0, 0, 0]);
	assert_eq!(decode_router_advertisement(&message, ND_HOP_LIMIT), Err(NdRefusal::ZeroLengthOption), "the loop the standard forbids");
}

#[test]
fn an_option_that_runs_past_the_message_is_refused() {
	// Declares four units and supplies one.
	let message = advertisement(&[OPTION_PREFIX_INFORMATION, 4, 0, 0, 0, 0, 0, 0]);
	assert_eq!(decode_router_advertisement(&message, ND_HOP_LIMIT), Err(NdRefusal::OptionOverruns));
	// And a trailing byte that cannot even hold an option header.
	let stub = advertisement(&[OPTION_MTU]);
	assert_eq!(decode_router_advertisement(&stub, ND_HOP_LIMIT), Err(NdRefusal::OptionOverruns));
}

#[test]
fn more_options_than_the_walk_bound_is_refused() {
	let mut options = Vec::new();
	for index in 0..=MAX_OPTIONS {
		options.extend_from_slice(&[OPTION_SOURCE_LINK_LAYER, 1, 0, 0, 0, 0, 0, index as u8]);
	}
	let message = advertisement(&options);
	assert_eq!(decode_router_advertisement(&message, ND_HOP_LIMIT), Err(NdRefusal::TooManyOptions));
}

#[test]
fn an_unknown_option_is_skipped_so_a_future_extension_does_not_drop_the_message() {
	let mut options = vec![200u8, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
	options.extend_from_slice(&[OPTION_SOURCE_LINK_LAYER, 1, 0x52, 0x54, 0x00, 0x11, 0x22, 0x07]);
	let message = advertisement(&options);
	let decoded = decode_router_advertisement(&message, ND_HOP_LIMIT).expect("the unknown option is stepped over");
	assert_eq!(decoded.options, vec![NdOption::SourceLinkLayer(mac(7))], "and the known one after it is still read");
}

#[test]
fn the_router_advertisement_fields_decode() {
	let decoded = decode_router_advertisement(&advertisement(&[]), ND_HOP_LIMIT).expect("valid");
	assert_eq!(decoded.current_hop_limit, 64);
	assert!(!decoded.managed, "the M bit is clear in this fixture");
	assert!(!decoded.other_configuration);
	assert_eq!(decoded.preference_bits, 0b01, "high, from the 0x08 flags byte");
	assert_eq!(decoded.router_lifetime_seconds, 0x0708);
	assert_eq!(decoded.reachable_time_ms, 30_000);
	assert_eq!(decoded.retrans_timer_ms, 1_000);

	let mut managed = advertisement(&[]);
	managed[5] = 0xc0;
	let decoded = decode_router_advertisement(&managed, ND_HOP_LIMIT).expect("valid");
	assert!(decoded.managed && decoded.other_configuration);
	assert_eq!(decoded.preference_bits, 0b00, "medium");

	assert_eq!(decode_router_advertisement(&advertisement(&[])[..8], ND_HOP_LIMIT), Err(NdRefusal::TooShort));
	let mut wrong = advertisement(&[]);
	wrong[0] = ROUTER_SOLICITATION;
	assert_eq!(decode_router_advertisement(&wrong, ND_HOP_LIMIT), Err(NdRefusal::WrongType));
}

/// A Prefix Information option: type, four units, length, flags, lifetimes, reserved, prefix.
fn prefix_option(prefix_len: u8, flags: u8, valid: u32, preferred: u32) -> Vec<u8> {
	let mut option = vec![OPTION_PREFIX_INFORMATION, 4, prefix_len, flags];
	option.extend_from_slice(&valid.to_be_bytes());
	option.extend_from_slice(&preferred.to_be_bytes());
	option.extend_from_slice(&[0, 0, 0, 0]);
	let mut base = [0u8; 16];
	base[0] = 0x20;
	base[1] = 0x01;
	base[2] = 0x0d;
	base[3] = 0xb8;
	option.extend_from_slice(&base);
	option
}

#[test]
fn a_prefix_information_option_decodes_both_flags_and_both_lifetimes() {
	let message = advertisement(&prefix_option(64, 0xc0, 2592000, 604800));
	let decoded = decode_router_advertisement(&message, ND_HOP_LIMIT).expect("valid");
	match &decoded.options[..] {
		[NdOption::Prefix(information)] => {
			assert_eq!(information.prefix.len(), 64);
			assert!(information.on_link, "the L bit");
			assert!(information.autonomous, "the A bit");
			assert_eq!(information.valid_seconds, 2592000);
			assert_eq!(information.preferred_seconds, 604800);
		}
		other => panic!("expected one prefix option, got {other:?}"),
	}

	// The flags are read separately, so a router may set either.
	let on_link_only = advertisement(&prefix_option(56, 0x80, 100, 100));
	match &decode_router_advertisement(&on_link_only, ND_HOP_LIMIT).expect("valid").options[..] {
		[NdOption::Prefix(information)] => assert!(information.on_link && !information.autonomous),
		other => panic!("expected one prefix option, got {other:?}"),
	}

	// A prefix length above 128 is not a prefix.
	let impossible = advertisement(&prefix_option(129, 0xc0, 100, 100));
	assert_eq!(decode_router_advertisement(&impossible, ND_HOP_LIMIT), Err(NdRefusal::MalformedOption));

	// A prefix option of the wrong size is malformed rather than partially read.
	let mut short = prefix_option(64, 0xc0, 100, 100);
	short[1] = 3;
	short.truncate(24);
	assert_eq!(decode_router_advertisement(&advertisement(&short), ND_HOP_LIMIT), Err(NdRefusal::MalformedOption));
}

#[test]
fn the_mtu_and_rdnss_options_decode_and_a_truncated_server_list_is_refused() {
	let mut options = vec![OPTION_MTU, 1, 0, 0];
	options.extend_from_slice(&1500u32.to_be_bytes());
	let mut rdnss = vec![OPTION_RDNSS, 5, 0, 0];
	rdnss.extend_from_slice(&600u32.to_be_bytes());
	rdnss.extend_from_slice(&address(0x53).octets());
	rdnss.extend_from_slice(&address(0x54).octets());
	options.extend_from_slice(&rdnss);

	let decoded = decode_router_advertisement(&advertisement(&options), ND_HOP_LIMIT).expect("valid");
	assert_eq!(decoded.options[0], NdOption::Mtu(1500));
	assert_eq!(decoded.options[1], NdOption::Rdnss { lifetime_seconds: 600, servers: vec![address(0x53), address(0x54)] });

	// A list that is not a whole number of addresses is malformed.
	let mut ragged = vec![OPTION_RDNSS, 2, 0, 0];
	ragged.extend_from_slice(&600u32.to_be_bytes());
	ragged.extend_from_slice(&[0u8; 8]);
	assert_eq!(decode_router_advertisement(&advertisement(&ragged), ND_HOP_LIMIT), Err(NdRefusal::MalformedOption));

	// An MTU option of the wrong size is malformed.
	assert_eq!(decode_router_advertisement(&advertisement(&[OPTION_MTU, 2, 0, 0, 0, 0, 5, 220, 0, 0, 0, 0, 0, 0, 0, 0]), ND_HOP_LIMIT), Err(NdRefusal::MalformedOption));
}

#[test]
fn a_link_layer_option_of_the_wrong_size_is_malformed() {
	let too_long = vec![OPTION_SOURCE_LINK_LAYER, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(decode_router_advertisement(&advertisement(&too_long), ND_HOP_LIMIT), Err(NdRefusal::MalformedOption));
}

#[test]
fn a_solicitation_and_its_advertisement_round_trip() {
	let built = build_neighbour_solicitation(address(2), Some(mac(1)));
	let decoded = decode_neighbour_solicitation(&built, ND_HOP_LIMIT).expect("valid");
	assert_eq!(decoded.target, address(2));
	assert_eq!(decoded.options, vec![NdOption::SourceLinkLayer(mac(1))]);

	let reply = build_neighbour_advertisement(address(2), mac(2), false, true, true);
	let decoded = decode_neighbour_advertisement(&reply, ND_HOP_LIMIT).expect("valid");
	assert_eq!(decoded.target, address(2));
	assert!(decoded.solicited && decoded.override_flag && !decoded.router);
	assert_eq!(decoded.options, vec![NdOption::TargetLinkLayer(mac(2))]);

	let from_router = build_neighbour_advertisement(address(3), mac(3), true, false, false);
	let decoded = decode_neighbour_advertisement(&from_router, ND_HOP_LIMIT).expect("valid");
	assert!(decoded.router && !decoded.solicited && !decoded.override_flag);
}

#[test]
fn a_detection_probe_carries_no_source_link_layer_option() {
	// THE PROBE MUST NOT SAY WHERE TO ANSWER. It is sourced from `::`, and an option telling the
	// link where to send a unicast reply would be an address this host does not yet hold.
	let probe = build_neighbour_solicitation(address(2), None);
	let decoded = decode_neighbour_solicitation(&probe, ND_HOP_LIMIT).expect("valid");
	assert!(decoded.options.is_empty());
	assert_eq!(probe.len(), 24, "the fixed part and nothing else");

	let solicitation = build_router_solicitation(Some(mac(1)));
	assert_eq!(solicitation[0], ROUTER_SOLICITATION);
	assert_eq!(decode_options(&solicitation[8..]).expect("options"), vec![NdOption::SourceLinkLayer(mac(1))]);
	assert!(decode_options(&build_router_solicitation(None)[8..]).expect("options").is_empty());
}
