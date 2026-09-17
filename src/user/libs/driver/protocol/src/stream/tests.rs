use super::*;

#[test]
fn a_connect_carries_its_port_and_no_payload() {
	let bytes = Request { op: OP_CONNECT, arg: 4096 }.encode();
	assert_eq!(Request::decode(&bytes), Some((Request { op: OP_CONNECT, arg: 4096 }, &[][..])));
}

#[test]
fn a_send_declares_exactly_what_it_carries() {
	let mut frame = Request { op: OP_SEND, arg: 4 }.encode().to_vec();
	frame.extend_from_slice(b"pkt!");
	assert_eq!(Request::decode(&frame), Some((Request { op: OP_SEND, arg: 4 }, &b"pkt!"[..])));
}

#[test]
fn a_send_claiming_more_than_it_carries_is_refused() {
	// The header is a claim by another address space, so it is checked against the message and not
	// only against a bound. THE CLAIM HERE IS INSIDE THE BOUND deliberately: a length over the
	// wire's ceiling is refused by the ceiling and proves nothing about this check, which is
	// exactly the hole the mutation gate found in an earlier version of this test.
	let mut frame = Request { op: OP_SEND, arg: 64 }.encode().to_vec();
	frame.extend_from_slice(b"pkt!");
	assert!(64 < MAX_PAYLOAD, "the claim must be inside the bound or this tests the bound instead");
	assert_eq!(Request::decode(&frame), None);
}

#[test]
fn a_send_claiming_less_than_it_carries_is_refused() {
	let mut frame = Request { op: OP_SEND, arg: 2 }.encode().to_vec();
	frame.extend_from_slice(b"pkt!");
	assert_eq!(Request::decode(&frame), None);
}

#[test]
fn a_send_over_the_wire_bound_is_refused() {
	let mut frame = Request { op: OP_SEND, arg: MAX_PAYLOAD + 1 }.encode().to_vec();
	frame.resize(REQUEST_LEN + MAX_PAYLOAD as usize + 1, 0);
	assert_eq!(Request::decode(&frame), None);
}

#[test]
fn a_request_that_is_not_a_send_carries_nothing() {
	let mut frame = Request { op: OP_RECEIVE, arg: 64 }.encode().to_vec();
	frame.extend_from_slice(b"stowaway");
	assert_eq!(Request::decode(&frame), None);
}

#[test]
fn a_truncated_header_is_not_a_request() {
	assert_eq!(Request::decode(&[0u8; REQUEST_LEN - 1]), None);
}

#[test]
fn a_reply_round_trips_with_its_payload() {
	let mut frame = reply(STATUS_OK, 3).to_vec();
	frame.extend_from_slice(b"abc");
	assert_eq!(decode_reply(&frame), Some((STATUS_OK, 3, &b"abc"[..])));
}

#[test]
fn a_reply_whose_length_disagrees_with_its_payload_is_refused() {
	let mut frame = reply(STATUS_OK, 8).to_vec();
	frame.extend_from_slice(b"abc");
	assert_eq!(decode_reply(&frame), None);
}

#[test]
fn a_send_answers_with_a_count_and_no_bytes() {
	// A short write is what a finite window produces, and the count is how the caller learns to
	// send the rest. It carries no payload, so nothing here contradicts it.
	assert_eq!(decode_reply(&reply(STATUS_OK, 64)), Some((STATUS_OK, 64, &[][..])));
}

#[test]
fn a_reply_over_the_wire_bound_is_refused() {
	assert_eq!(decode_reply(&reply(STATUS_OK, MAX_PAYLOAD + 1)), None);
}

#[test]
fn nothing_yet_and_end_of_stream_are_different_answers() {
	// A receive that found no bytes is OK with a length of zero; a stream that is over is CLOSED.
	// One is retried and the other is not, so a wire that spelled both the same would make a closed
	// connection poll for ever.
	assert_eq!(decode_reply(&reply(STATUS_OK, 0)), Some((STATUS_OK, 0, &[][..])));
	assert_eq!(decode_reply(&reply(STATUS_CLOSED, 0)), Some((STATUS_CLOSED, 0, &[][..])));
	assert_ne!(STATUS_OK, STATUS_CLOSED);
	assert_ne!(STATUS_ERR, STATUS_CLOSED);
}

#[test]
fn an_identity_request_carries_nothing_and_its_answer_carries_the_context_id() {
	let ask = Request { op: OP_IDENTITY, arg: 0 }.encode();
	assert_eq!(Request::decode(&ask), Some((Request { op: OP_IDENTITY, arg: 0 }, &[][..])));
	let mut frame = reply(STATUS_OK, IDENTITY_LEN as u32).to_vec();
	frame.extend_from_slice(&7u64.to_le_bytes());
	let (status, len, payload) = decode_reply(&frame).unwrap();
	assert_eq!((status, len), (STATUS_OK, IDENTITY_LEN as u32));
	assert_eq!(u64::from_le_bytes(payload.try_into().unwrap()), 7);
}
