// The two exemptions, the rate limit, and the rule that a path-MTU record is written for a consumer
// that asked and never for a stranger who quoted.

use super::*;
use crate::ipv6::{ALL_NODES, UNSPECIFIED};
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

fn interface() -> Interface {
	Interface::new(0, 1)
}

fn origination(class: ErrorClass, source: Address, destination: Address) -> Origination {
	Origination { class, trigger_source: source, trigger_destination: destination, trigger_icmp_type: None, option_action: None }
}

#[test]
fn an_error_is_never_sent_about_another_error() {
	let mut limiter = RateLimiter::new(DEFAULT_ERROR_RATE);
	let mut about_an_error = origination(ErrorClass::TimeExceeded { code: 0 }, address(2), address(1));
	about_an_error.trigger_icmp_type = Some(DESTINATION_UNREACHABLE);
	assert_eq!(may_originate(&about_an_error, &mut limiter, 0), Err(Suppressed::TriggerWasError));

	// An echo request is informational, so an error about one is allowed.
	let mut about_an_echo = origination(ErrorClass::TimeExceeded { code: 0 }, address(2), address(1));
	about_an_echo.trigger_icmp_type = Some(ECHO_REQUEST);
	assert_eq!(may_originate(&about_an_echo, &mut limiter, 0), Ok(()));

	assert!(is_error(DESTINATION_UNREACHABLE) && is_error(PARAMETER_PROBLEM));
	assert!(!is_error(ECHO_REQUEST) && !is_error(NEIGHBOUR_SOLICITATION) && !is_error(MLD_REPORT_V2));
}

#[test]
fn an_error_is_never_sent_to_a_source_that_is_not_one_host() {
	let mut limiter = RateLimiter::new(DEFAULT_ERROR_RATE);
	for source in [ALL_NODES, UNSPECIFIED] {
		let attempt = origination(ErrorClass::DestinationUnreachable { code: 3 }, source, address(1));
		assert_eq!(may_originate(&attempt, &mut limiter, 0), Err(Suppressed::SourceNotUnique), "nobody to tell");
	}
}

#[test]
fn a_multicast_destination_suppresses_an_error_except_in_the_two_named_cases() {
	let mut limiter = RateLimiter::new(MAX_ERROR_RATE);
	let ordinary = origination(ErrorClass::DestinationUnreachable { code: 3 }, address(2), ALL_NODES);
	assert_eq!(may_originate(&ordinary, &mut limiter, 0), Err(Suppressed::MulticastDestination), "one bad frame must not make every host answer");

	// EXEMPTION ONE: Packet Too Big, because a PMTU report is how a sender learns the path.
	let too_big = origination(ErrorClass::PacketTooBig { mtu: 1300 }, address(2), ALL_NODES);
	assert_eq!(may_originate(&too_big, &mut limiter, 0), Ok(()));

	// EXEMPTION TWO: an unknown option whose action bits say to report regardless.
	let mut always = origination(ErrorClass::ParameterProblem { code: 2, pointer: 44 }, address(2), ALL_NODES);
	always.option_action = Some(OptionAction::DiscardAndReportAlways);
	assert_eq!(may_originate(&always, &mut limiter, 0), Ok(()), "action 10 reports even to a group");

	let mut unicast_only = origination(ErrorClass::ParameterProblem { code: 2, pointer: 44 }, address(2), ALL_NODES);
	unicast_only.option_action = Some(OptionAction::DiscardAndReportUnicast);
	assert_eq!(may_originate(&unicast_only, &mut limiter, 0), Err(Suppressed::MulticastDestination), "action 11 does not");

	// And to a unicast destination both report, which is what makes them distinguishable only here.
	let mut always_unicast = always;
	always_unicast.trigger_destination = address(1);
	let mut unicast_unicast = unicast_only;
	unicast_unicast.trigger_destination = address(1);
	assert_eq!(may_originate(&always_unicast, &mut limiter, 0), Ok(()));
	assert_eq!(may_originate(&unicast_unicast, &mut limiter, 0), Ok(()));
}

#[test]
fn the_bucket_holds_a_burst_refills_at_the_rate_and_never_exceeds_the_burst() {
	let mut limiter = RateLimiter::new(10);
	let attempt = origination(ErrorClass::DestinationUnreachable { code: 3 }, address(2), address(1));
	for _ in 0..ERROR_BURST {
		assert_eq!(may_originate(&attempt, &mut limiter, 0), Ok(()));
	}
	assert_eq!(may_originate(&attempt, &mut limiter, 0), Err(Suppressed::RateLimited), "the burst is spent");
	assert_eq!(limiter.limited(), 1);

	// A hundred milliseconds at ten per second is one token.
	assert_eq!(may_originate(&attempt, &mut limiter, 100), Ok(()));
	assert_eq!(may_originate(&attempt, &mut limiter, 100), Err(Suppressed::RateLimited));

	// AN HOUR IDLE DOES NOT BUY AN HOUR OF ERRORS. The refill saturates at the burst.
	let mut rested = RateLimiter::new(10);
	for _ in 0..ERROR_BURST {
		assert!(rested.allow(0));
	}
	assert!(!rested.allow(0));
	for _ in 0..ERROR_BURST {
		assert!(rested.allow(3_600_000), "the burst is back");
	}
	assert!(!rested.allow(3_600_000), "and no more than the burst");
}

#[test]
fn a_fraction_of_a_token_is_not_lost_to_repeated_polling() {
	// Ten per second is one token per hundred milliseconds. Asking every ten milliseconds must still
	// earn a token at the hundred-millisecond mark rather than truncating to zero nine times and
	// losing the elapsed time each round.
	let mut limiter = RateLimiter::new(10);
	for _ in 0..ERROR_BURST {
		assert!(limiter.allow(0));
	}
	for tick in 1..10u64 {
		assert!(!limiter.allow(tick * 10), "no whole token yet at {}ms", tick * 10);
	}
	assert!(limiter.allow(100), "the hundredth millisecond earns one");
}

#[test]
fn the_configured_rate_is_read_once_and_a_value_this_host_cannot_honour_gives_the_default() {
	assert_eq!(RateLimiter::from_config(Some("50")).rate(), 50);
	assert_eq!(RateLimiter::from_config(Some(" 7 ")).rate(), 7);
	assert_eq!(RateLimiter::from_config(None).rate(), DEFAULT_ERROR_RATE);
	assert_eq!(RateLimiter::from_config(Some("")).rate(), DEFAULT_ERROR_RATE);
	assert_eq!(RateLimiter::from_config(Some("nonsense")).rate(), DEFAULT_ERROR_RATE);
	assert_eq!(RateLimiter::from_config(Some("0")).rate(), DEFAULT_ERROR_RATE, "below the floor");
	assert_eq!(RateLimiter::from_config(Some("1001")).rate(), DEFAULT_ERROR_RATE, "above the ceiling");
	assert_eq!(RateLimiter::from_config(Some("-5")).rate(), DEFAULT_ERROR_RATE);
	assert_eq!(RateLimiter::from_config(Some("1")).rate(), MIN_ERROR_RATE);
	assert_eq!(RateLimiter::from_config(Some("1000")).rate(), MAX_ERROR_RATE);
}

#[test]
fn an_error_quotes_a_bounded_portion_and_fits_the_minimum_mtu() {
	let invoking = vec![0x5au8; 4000];
	let message = build_error(ErrorClass::PacketTooBig { mtu: 1400 }, address(1), address(2), &invoking);
	assert_eq!(message[0], PACKET_TOO_BIG);
	assert_eq!(message[1], 0);
	assert_eq!(u32::from_be_bytes([message[4], message[5], message[6], message[7]]), 1400);
	assert!(HEADER_LEN + message.len() <= MIN_MTU as usize, "the report about a narrow path must fit through one");
	assert_eq!(message.len(), MESSAGE_HEADER_LEN + 4 + MAX_QUOTE);

	// A short invoking packet is quoted whole.
	let short = vec![1u8, 2, 3, 4];
	let about_short = build_error(ErrorClass::TimeExceeded { code: 0 }, address(1), address(2), &short);
	assert_eq!(&about_short[8..], &short);

	// The parameter problem carries its pointer where a reader looks for it.
	let parameter = build_error(ErrorClass::ParameterProblem { code: 2, pointer: 42 }, address(1), address(2), &short);
	assert_eq!(parameter[0], PARAMETER_PROBLEM);
	assert_eq!(parameter[1], 2);
	assert_eq!(u32::from_be_bytes([parameter[4], parameter[5], parameter[6], parameter[7]]), 42);
}

#[test]
fn a_message_verifies_only_against_the_addresses_it_was_sent_between() {
	let message = build_error(ErrorClass::DestinationUnreachable { code: 3 }, address(1), address(2), &[9, 9, 9, 9]);
	assert_eq!(verify(address(1), address(2), &message), Ok(()));
	assert_eq!(verify(address(1), address(3), &message), Err(IcmpRefusal::BadChecksum), "delivered to the wrong host, and it shows");
	assert_eq!(verify(address(9), address(2), &message), Err(IcmpRefusal::BadChecksum));
	assert_eq!(verify(address(1), address(2), &message[..2]), Err(IcmpRefusal::TooShort));

	let mut damaged = message.clone();
	let last = damaged.len() - 1;
	damaged[last] ^= 0xff;
	assert_eq!(verify(address(1), address(2), &damaged), Err(IcmpRefusal::BadChecksum));
}

#[test]
fn an_echo_reply_carries_the_identifier_sequence_and_payload_of_its_request() {
	let mut request = vec![ECHO_REQUEST, 0, 0, 0, 0x12, 0x34, 0x00, 0x07];
	request.extend_from_slice(b"payload");
	let checksum = crate::ipv6_packet::pseudo_header_checksum(address(2), address(1), NEXT_ICMPV6, &request);
	request[2..4].copy_from_slice(&checksum.to_be_bytes());
	assert_eq!(verify(address(2), address(1), &request), Ok(()));

	let echo = parse_echo(&request).expect("an echo");
	assert_eq!(echo.identifier, 0x1234);
	assert_eq!(echo.sequence, 7);
	assert_eq!(&request[echo.payload_offset..], b"payload");

	let reply = build_echo_reply(address(1), address(2), &request).expect("a reply");
	assert_eq!(reply[0], ECHO_REPLY);
	assert_eq!(verify(address(1), address(2), &reply), Ok(()), "the reply verifies between the swapped pair");
	let reply_echo = parse_echo(&reply).expect("an echo");
	assert_eq!(reply_echo.identifier, 0x1234);
	assert_eq!(reply_echo.sequence, 7);
	assert_eq!(&reply[reply_echo.payload_offset..], b"payload");

	assert_eq!(parse_echo(&[ECHO_REQUEST, 0, 0, 0]), Err(IcmpRefusal::TooShort));
	assert_eq!(parse_echo(&[DESTINATION_UNREACHABLE, 0, 0, 0, 0, 0, 0, 0]), Err(IcmpRefusal::UnknownType));
}

#[test]
fn a_validated_reply_carries_what_the_consumer_needs_and_this_layer_keeps_no_probe_table() {
	let reply = EchoReply { interface: interface(), source: address(2), destination: address(1), identifier: 0x1234, sequence: 7, hop_limit: 63 };
	assert_eq!(reply.identifier, 0x1234);
	assert_eq!(reply.hop_limit, 63, "how a caller notices a path that changed length");
	// Two replies from the same peer with different sequences are different events, which is what
	// lets the consumer - not this layer - do the matching.
	let later = EchoReply { sequence: 8, ..reply };
	assert_ne!(reply, later);
}

#[test]
fn a_path_mtu_record_lowers_only_downward_and_never_below_the_floor() {
	let mut cache = PathMtuCache::new();
	assert_eq!(cache.get(interface(), address(2), 0), None);

	assert_eq!(cache.record(interface(), address(2), 1400, 1000), MtuOutcome::Lowered { mtu: 1400 });
	assert_eq!(cache.get(interface(), address(2), 1000), Some(1400));

	assert_eq!(cache.record(interface(), address(2), 1300, 2000), MtuOutcome::Lowered { mtu: 1300 });
	assert_eq!(cache.record(interface(), address(2), 1400, 3000), MtuOutcome::NotLower, "a larger report does not raise it");
	assert_eq!(cache.record(interface(), address(2), 1300, 4000), MtuOutcome::NotLower, "and an equal one is not a lowering");
	assert_eq!(cache.get(interface(), address(2), 4000), Some(1300));

	// THE FLOOR IS APPLIED, NOT BELIEVED. A router reporting 576 is reporting a path that cannot
	// carry IPv6 at all; the record holds the minimum instead.
	assert_eq!(cache.record(interface(), address(2), 576, 5000), MtuOutcome::Lowered { mtu: PATH_MTU_FLOOR });
	assert_eq!(cache.get(interface(), address(2), 5000), Some(1280));
}

#[test]
fn an_equal_report_does_not_refresh_the_expiry() {
	let mut cache = PathMtuCache::new();
	cache.record(interface(), address(2), 1400, 0);
	assert_eq!(cache.get(interface(), address(2), PATH_MTU_LIFETIME_MS - 1), Some(1400));

	// A router repeating the same value at the last moment must not be able to keep the record
	// alive forever.
	assert_eq!(cache.record(interface(), address(2), 1400, PATH_MTU_LIFETIME_MS - 1), MtuOutcome::NotLower);
	assert_eq!(cache.get(interface(), address(2), PATH_MTU_LIFETIME_MS + 1), None, "expired on its original schedule");

	// A genuine lowering does refresh it.
	let mut refreshed = PathMtuCache::new();
	refreshed.record(interface(), address(2), 1400, 0);
	refreshed.record(interface(), address(2), 1350, PATH_MTU_LIFETIME_MS - 1);
	assert_eq!(refreshed.get(interface(), address(2), PATH_MTU_LIFETIME_MS + 1), Some(1350));
}

#[test]
fn a_full_table_says_capacity_rather_than_claiming_the_lowering_was_recorded() {
	let mut cache = PathMtuCache::new();
	let limit = crate::ipv6_budget::Resource::PathMtu.limit();
	for index in 0..limit as u16 {
		assert_eq!(cache.record(interface(), address(index), 1400, 0), MtuOutcome::Lowered { mtu: 1400 });
	}
	assert_eq!(cache.len(), limit as usize);
	assert_eq!(cache.record(interface(), address(9999), 1300, 0), MtuOutcome::Capacity, "not a silent success");
	assert_eq!(cache.get(interface(), address(9999), 0), None);

	// Once a record expires, the retry succeeds - which is what "retry after capacity is released"
	// means.
	let later = PATH_MTU_LIFETIME_MS + 1;
	assert_eq!(cache.record(interface(), address(9999), 1300, later), MtuOutcome::Lowered { mtu: 1300 });
	assert_eq!(cache.len(), 1, "the dead records were reclaimed rather than evicted while live");
}

#[test]
fn a_record_belongs_to_one_interface_generation() {
	let mut cache = PathMtuCache::new();
	cache.record(interface(), address(2), 1400, 0);
	let replaced = Interface::new(0, 2);
	assert_eq!(cache.get(replaced, address(2), 0), None, "a replaced NIC knows nothing about the old path");
	assert_eq!(cache.record(replaced, address(2), 1350, 0), MtuOutcome::Lowered { mtu: 1350 });
	assert_eq!(cache.get(interface(), address(2), 0), Some(1400), "and the old one is untouched");
}

#[test]
fn under_a_flood_the_emission_in_elapsed_time_is_bounded_by_the_burst_plus_the_rate() {
	// THE BOUND WRITTEN AS A FORMULA rather than as "not too many". In elapsed time T at rate R,
	// what may leave is at most `20 + floor(R * T)`, and a limiter that refilled per call or that
	// let the bucket grow past the burst breaks exactly this.
	for rate in [1u32, 10, 100] {
		for elapsed_ms in [0u64, 250, 1_000, 5_000] {
			let mut limiter = RateLimiter::new(rate);
			let attempt = origination(ErrorClass::DestinationUnreachable { code: 3 }, address(2), address(1));
			let mut emitted = 0u64;
			// A flood: ask far more often than the bucket can answer, across the whole interval.
			for step in 0..2000u64 {
				let now = elapsed_ms * step / 2000;
				if may_originate(&attempt, &mut limiter, now).is_ok() {
					emitted += 1;
				}
			}
			let permitted = u64::from(ERROR_BURST) + u64::from(rate) * elapsed_ms / 1000;
			assert!(emitted <= permitted, "rate {rate} over {elapsed_ms}ms emitted {emitted}, past {permitted}");
			// And the counter saw every one it refused, with no per-packet log to grow.
			assert_eq!(u64::from(limiter.limited()), 2000 - emitted);
		}
	}
}

#[test]
fn a_full_path_mtu_table_refuses_the_next_key_and_the_flow_keeps_its_own_smaller_limit() {
	let mut cache = PathMtuCache::new();
	let limit = crate::ipv6_budget::Resource::PathMtu.limit();
	for index in 0..limit as u16 {
		assert_eq!(cache.record(interface(), address(index), 1400, 0), MtuOutcome::Lowered { mtu: 1400 });
	}
	assert_eq!(cache.len(), limit as usize);

	// THE SIXTY-FIFTH KEY IS REFUSED, and the answer says `Capacity` rather than claiming the write
	// happened. The consumer that asked keeps the smaller limit it validated for its own flow - the
	// cache is where the durable record lives, not where the decision was made - and can retry once
	// a slot is released.
	assert_eq!(cache.record(interface(), address(9999), 1300, 0), MtuOutcome::Capacity);
	assert_eq!(cache.get(interface(), address(9999), 0), None, "nothing was written for it");
	assert_eq!(cache.len(), limit as usize, "and no live record was evicted to make room");
	// Every other flow still reads what it had.
	assert_eq!(cache.get(interface(), address(0), 0), Some(1400));

	// A REFRESH AT CAPACITY still works, because it costs no slot.
	assert_eq!(cache.record(interface(), address(0), 1350, 0), MtuOutcome::Lowered { mtu: 1350 });
	assert_eq!(cache.len(), limit as usize);

	// AND A SLOT RELEASED BY EXPIRY ADMITS THE KEY THAT WAS REFUSED, which is what makes `Capacity`
	// a retryable answer rather than a permanent one.
	assert_eq!(cache.expire(PATH_MTU_LIFETIME_MS + 1), limit as usize, "every record aged out together");
	assert!(cache.is_empty());
	assert_eq!(cache.record(interface(), address(9999), 1300, PATH_MTU_LIFETIME_MS + 1), MtuOutcome::Lowered { mtu: 1300 });
	assert_eq!(cache.get(interface(), address(9999), PATH_MTU_LIFETIME_MS + 1), Some(1300));
}
