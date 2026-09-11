//! Every check that has to happen before the wall clock moves.

use super::*;

const SENT: u64 = 0x1234_5678_9abc_def0;

fn reply(originate: u64, transmit: u64) -> [u8; MESSAGE_LEN] {
	let mut message = [0u8; MESSAGE_LEN];
	// LI 0, VN 4, Mode 4 (server), stratum 2.
	message[0] = 0x24;
	message[1] = 2;
	message[24..32].copy_from_slice(&originate.to_be_bytes());
	message[40..48].copy_from_slice(&transmit.to_be_bytes());
	message
}

fn ntp_time(unix: u32) -> u64 {
	(u64::from(unix.wrapping_add(NTP_UNIX_OFFSET))) << 32
}

#[test]
fn a_request_carries_a_per_request_value_where_there_used_to_be_a_zero() {
	// THE ONLY THING THAT TIES A REPLY TO A REQUEST. A constant zero ties it to every request ever
	// made, which is what let one forged datagram move the clock.
	let request = build_request(SENT);
	assert_eq!(request[0], 0x23, "LI 0, VN 4, Mode 3");
	assert_eq!(u64::from_be_bytes(request[40..48].try_into().expect("eight bytes")), SENT);
}

#[test]
fn a_well_formed_reply_gives_the_unix_time_its_transmit_timestamp_names() {
	let message = reply(SENT, ntp_time(1_700_000_000));
	assert_eq!(parse_reply(&message, SENT, None), Ok(1_700_000_000));
}

#[test]
fn a_reply_answering_a_different_request_is_refused() {
	// THE SPOOF. Every other field is right; the originate timestamp is not the one this host sent.
	let message = reply(SENT.wrapping_add(1), ntp_time(1_700_000_000));
	assert_eq!(parse_reply(&message, SENT, None), Err(Refusal::NotOurs));
}

#[test]
fn a_byte_identical_replay_is_refused_by_the_transmit_timestamp_and_nothing_else() {
	// THE REPLAY. It answers the right request and is otherwise perfect, so the ONLY thing that can
	// reject it is that its transmit timestamp is the one this host already acted on: a second
	// reading of a clock is never the same reading.
	let transmit: u64 = ntp_time(1_700_000_000);
	let message = reply(SENT, transmit);
	assert_eq!(parse_reply(&message, SENT, None), Ok(1_700_000_000), "the first time");
	assert_eq!(parse_reply(&message, SENT, Some(transmit)), Err(Refusal::Replayed), "and not the second");
	// A later reading from the same server is accepted, which is what makes this a replay check
	// rather than a one-reply-per-server rule.
	let later = reply(SENT, ntp_time(1_700_000_001));
	assert_eq!(parse_reply(&later, SENT, Some(transmit)), Ok(1_700_000_001));
}

#[test]
fn a_zero_transmit_timestamp_names_a_server_that_never_set_its_clock() {
	let message = reply(SENT, 0);
	assert_eq!(parse_reply(&message, SENT, None), Err(Refusal::ZeroTransmit));
}

#[test]
fn stratum_is_one_to_fifteen_and_everything_else_is_refused() {
	// THE RULE THAT USED TO BE "NOT 0 AND NOT 16". 17 through 255 are RESERVED and are not valid
	// server strata, so a forged reply carrying stratum 200 passed a check written to catch it.
	for stratum in [0u8, 16, 17, 200, 255] {
		let mut message = reply(SENT, ntp_time(1_700_000_000));
		message[1] = stratum;
		assert_eq!(parse_reply(&message, SENT, None), Err(Refusal::Stratum), "stratum {stratum}");
	}
	for stratum in [1u8, 2, 15] {
		let mut message = reply(SENT, ntp_time(1_700_000_000));
		message[1] = stratum;
		assert!(parse_reply(&message, SENT, None).is_ok(), "stratum {stratum}");
	}
}

#[test]
fn the_leap_indicator_alarm_is_the_server_saying_not_to_use_its_time() {
	let mut message = reply(SENT, ntp_time(1_700_000_000));
	message[0] = 0xc0 | (4 << 3) | 4;
	assert_eq!(parse_reply(&message, SENT, None), Err(Refusal::Alarm));
}

#[test]
fn only_a_server_reply_of_a_version_this_client_speaks_is_read() {
	let mut client_mode = reply(SENT, ntp_time(1_700_000_000));
	client_mode[0] = (4 << 3) | 3;
	assert_eq!(parse_reply(&client_mode, SENT, None), Err(Refusal::Mode));

	let mut ancient = reply(SENT, ntp_time(1_700_000_000));
	ancient[0] = (1 << 3) | 4;
	assert_eq!(parse_reply(&ancient, SENT, None), Err(Refusal::Version));

	// Version 3 is still spoken; a great many servers answer with it.
	let mut three = reply(SENT, ntp_time(1_700_000_000));
	three[0] = (3 << 3) | 4;
	assert!(parse_reply(&three, SENT, None).is_ok());
}

#[test]
fn a_short_datagram_is_refused_before_any_field_is_read() {
	assert_eq!(parse_reply(&[0u8; 20], SENT, None), Err(Refusal::Short));
	assert_eq!(transmit_of(&[0u8; 20]), None);
}
