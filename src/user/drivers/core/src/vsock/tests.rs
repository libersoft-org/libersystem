use super::*;

fn header(op: u16, len: u32) -> Header {
	Header { src_cid: CID_HOST, dst_cid: 3, src_port: 1024, dst_port: 4096, len, kind: TYPE_STREAM, op, flags: 0, buf_alloc: 8192, fwd_cnt: 0 }
}

#[test]
fn a_header_survives_the_round_trip() {
	let sent = Header { src_cid: 0x1122_3344_5566_7788, dst_cid: 2, src_port: 0xDEAD_BEEF, dst_port: 7, len: 512, kind: TYPE_STREAM, op: OP_RW, flags: SHUTDOWN_SEND, buf_alloc: 0x0001_0000, fwd_cnt: 0xFFFF_FFF0 };
	assert_eq!(Header::decode(&sent.encode()), Some(sent));
}

#[test]
fn a_header_shorter_than_the_layout_is_not_a_header() {
	let bytes = [0u8; HEADER_LEN - 1];
	assert_eq!(Header::decode(&bytes), None);
}

#[test]
fn every_field_sits_where_the_device_puts_it() {
	// The offsets are the contract with the device, so they are asserted against the bytes and not
	// only against a round trip, which would pass with every field in the wrong place.
	let bytes = header(OP_REQUEST, 0).encode();
	assert_eq!(u64::from_le_bytes(bytes[0..8].try_into().unwrap()), CID_HOST);
	assert_eq!(u64::from_le_bytes(bytes[8..16].try_into().unwrap()), 3);
	assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 1024);
	assert_eq!(u32::from_le_bytes(bytes[20..24].try_into().unwrap()), 4096);
	assert_eq!(u16::from_le_bytes(bytes[28..30].try_into().unwrap()), TYPE_STREAM);
	assert_eq!(u16::from_le_bytes(bytes[30..32].try_into().unwrap()), OP_REQUEST);
	assert_eq!(u32::from_le_bytes(bytes[36..40].try_into().unwrap()), 8192);
}

#[test]
fn the_window_is_the_buffer_less_what_is_in_flight() {
	assert_eq!(may_send(8192, 0, 0), Some(8192));
	assert_eq!(may_send(8192, 0, 1000), Some(7192));
	assert_eq!(may_send(8192, 1000, 1000), Some(8192));
	assert_eq!(may_send(8192, 0, 8192), Some(0));
}

#[test]
fn the_window_survives_the_counters_wrapping() {
	// Both counters are free-running u32s. A subtraction that is not modular gives four gibibytes of
	// window here, and the connection sends into a buffer that does not exist.
	let fwd_cnt = u32::MAX - 100;
	let sent = 200u32; // wrapped past the end
	assert_eq!(sent.wrapping_sub(fwd_cnt), 301);
	assert_eq!(may_send(8192, fwd_cnt, sent), Some(8192 - 301));
}

#[test]
fn a_peer_claiming_more_than_it_was_sent_is_refused() {
	// Hostile credit: fwd_cnt ahead of what this side ever wrote is a window wider than the peer's
	// own buffer. Trusting it is the overrun; refusing it is a reset.
	assert_eq!(may_send(8192, 5000, 1000), None);
}

#[test]
fn a_peer_shrinking_its_buffer_below_what_is_in_flight_is_refused() {
	assert_eq!(may_send(64, 0, 1000), None);
}

#[test]
fn a_packet_for_another_port_is_not_ours() {
	let head = header(OP_RW, 0);
	assert_eq!(admit(&head, HEADER_LEN, 3, 9999, 4096), Err(Refusal::NotOurs));
	assert_eq!(admit(&head, HEADER_LEN, 77, 4096, 4096), Err(Refusal::NotOurs));
	assert_eq!(admit(&head, HEADER_LEN, 3, 4096, 4096), Ok(()));
}

#[test]
fn a_packet_type_this_driver_does_not_speak_is_refused() {
	let mut head = header(OP_RW, 0);
	head.kind = 9;
	assert_eq!(admit(&head, HEADER_LEN, 3, 4096, 4096), Err(Refusal::Type));
}

#[test]
fn a_length_larger_than_what_arrived_is_refused() {
	// The lie that reads past the buffer: a header claiming 4096 bytes in a packet that carried 64.
	let head = header(OP_RW, 4096);
	assert_eq!(admit(&head, HEADER_LEN + 64, 3, 4096, 65536), Err(Refusal::Length));
	assert_eq!(admit(&head, HEADER_LEN + 4096, 3, 4096, 65536), Ok(()));
}

#[test]
fn a_length_over_our_own_bound_is_refused() {
	let head = header(OP_RW, 4096);
	assert_eq!(admit(&head, HEADER_LEN + 4096, 3, 4096, 2048), Err(Refusal::Length));
}

#[test]
fn a_response_opens_a_connecting_socket_and_nothing_else_does() {
	assert_eq!(advance(State::Connecting, OP_RESPONSE, 0), State::Open);
	assert_eq!(advance(State::Connecting, OP_RW, 0), State::Connecting);
	assert_eq!(advance(State::Connecting, OP_CREDIT_UPDATE, 0), State::Connecting);
}

#[test]
fn a_reset_closes_from_every_state() {
	// Including from Connecting: that is how a REFUSED connection is told from a slow one.
	for state in [State::Closed, State::Connecting, State::Open, State::HalfClosed { peer_done: true, we_done: false }] {
		assert_eq!(advance(state, OP_RST, 0), State::Closed);
	}
}

#[test]
fn shutdown_is_directional_and_one_direction_is_not_a_close() {
	let peer_stopped_sending = advance(State::Open, OP_SHUTDOWN, SHUTDOWN_SEND);
	assert_eq!(peer_stopped_sending, State::HalfClosed { peer_done: true, we_done: false });
	// This side may still write out everything it had.
	assert!(may_write(peer_stopped_sending));
	assert!(!may_read(peer_stopped_sending));
}

#[test]
fn both_directions_shut_is_a_close() {
	let half = advance(State::Open, OP_SHUTDOWN, SHUTDOWN_SEND);
	assert_eq!(advance(half, OP_SHUTDOWN, SHUTDOWN_RECEIVE), State::Closed);
	assert_eq!(advance(State::Open, OP_SHUTDOWN, SHUTDOWN_SEND | SHUTDOWN_RECEIVE), State::Closed);
}

#[test]
fn a_closed_connection_carries_nothing() {
	assert!(!may_write(State::Closed));
	assert!(!may_read(State::Closed));
	assert!(!may_write(State::Connecting));
	assert!(!may_read(State::Connecting));
}

#[test]
fn the_host_disappearing_is_a_reset_on_every_open_connection() {
	// The item names "host disappearance" as a case. It arrives as a reset per connection or as no
	// packets at all; the state machine has to reach Closed from Open without a shutdown first, and
	// a driver that only closes through HalfClosed leaks every connection when the host goes away.
	assert_eq!(advance(State::Open, OP_RST, 0), State::Closed);
	assert_eq!(advance(State::HalfClosed { peer_done: true, we_done: false }, OP_RST, 0), State::Closed);
}

#[test]
fn a_local_port_names_its_slot_and_a_port_outside_the_range_names_nobody() {
	assert_eq!(slot_of(1024, 1024, 4), Some(0));
	assert_eq!(slot_of(1027, 1024, 4), Some(3));
	// THE TWO ENDS OF THE RANGE, because both are where an off-by-one lives: one below the base is
	// not slot zero and one past the last is not the last.
	assert_eq!(slot_of(1023, 1024, 4), None);
	assert_eq!(slot_of(1028, 1024, 4), None);
	// AND A PORT A REMAINDER WOULD ADMIT. `1032 % 4` is 0, so a driver indexing by the remainder
	// would hand this stranger's packet to the first stream.
	assert_eq!(slot_of(1032, 1024, 4), None);
	// A port below the base does not wrap into the range either: the subtraction is checked, so a
	// port of zero against a base of 1024 is nobody rather than a slot near four billion.
	assert_eq!(slot_of(0, 1024, 4), None);
	assert_eq!(slot_of(u32::MAX, 1024, 4), None);
}

#[test]
fn an_event_is_a_number_and_a_short_one_is_not_an_event() {
	assert_eq!(event(&0u32.to_le_bytes()), Some(EVENT_TRANSPORT_RESET));
	assert_eq!(event(&7u32.to_le_bytes()), Some(7), "an event this driver does not act on is still read, not guessed at");
	// The device writes four bytes; a shorter used length is a completion that did not carry one.
	assert_eq!(event(&[0u8; EVENT_LEN - 1]), None);
	assert_eq!(event(&[]), None);
	// A LONGER BUFFER IS THE FIRST FOUR BYTES and not a refusal: the slot is whatever was posted,
	// and the device reports how much of it it wrote.
	assert_eq!(event(&[1, 0, 0, 0, 0xFF, 0xFF]), Some(1));
}
