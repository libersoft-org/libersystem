// EVERY ONE OF THESE WATCHES A DECISION, because the whole reason this module exists is that three
// programs made these decisions separately and one of them made a different one. A round trip that
// only proves `encode` and `decode` agree with each other would pass just as well against a wire
// nobody else speaks, so the byte layout is asserted against LITERAL BYTES: those are the bytes
// `driver.virtio-blk` and `driver.xhci` put on the channel today.

use super::*;

#[test]
fn a_request_is_the_sixteen_bytes_the_servers_already_take_apart() {
	// op 1 (write), lba 0x0102030405060708, count 0x0A0B0C0D - little endian, in that order.
	let bytes: [u8; 16] = [1, 0, 0, 0, 8, 7, 6, 5, 4, 3, 2, 1, 0x0D, 0x0C, 0x0B, 0x0A];
	let request = Request::decode(&bytes).expect("sixteen bytes is a request");
	assert_eq!(request, Request { op: OP_WRITE, lba: 0x0102_0304_0506_0708, count: 0x0A0B_0C0D });
	assert_eq!(request.encode(), bytes, "the encoding is the same bytes the decoder was given");
}

#[test]
fn a_frame_too_short_to_be_a_request_is_refused_rather_than_indexed() {
	// The servers receive into a reusable buffer, so the bytes past a short message are the PREVIOUS
	// message's. Fifteen of them is not a request with a zero on the end.
	for short in 0..REQUEST_LEN {
		assert_eq!(Request::decode(&[0u8; REQUEST_LEN][..short]), None, "{short} bytes is not a request");
	}
	assert!(Request::decode(&[0u8; REQUEST_LEN]).is_some(), "sixteen is");
}

#[test]
fn a_longer_frame_is_accepted_because_the_servers_accept_one() {
	// Holding this decoder to equality would refuse frames the running servers take, which would be
	// this module changing the protocol while claiming to record it.
	let mut long = [0u8; 64];
	long[0..4].copy_from_slice(&OP_FLUSH.to_le_bytes());
	let request = Request::decode(&long).expect("a longer frame still carries a request");
	assert_eq!(request.op, OP_FLUSH);
}

#[test]
fn an_unknown_op_decodes_rather_than_becoming_an_error_the_caller_cannot_tell_apart() {
	// A server refuses an op it does not serve. It can only do that if the decoder hands it one:
	// folding an unknown op into `None` would make it indistinguishable from a malformed frame, and
	// the two want different replies.
	let request = Request { op: 9, lba: 0, count: 1 };
	assert_eq!(Request::decode(&request.encode()), Some(request));
}

#[test]
fn only_a_write_carries_a_source_and_only_a_read_answers_with_data() {
	// The two ways to get this wrong are opposite and both silent: a leaked client object, or a
	// close of something nobody sent.
	assert!(Request::carries_source(OP_WRITE));
	assert!(Request::answers_with_data(OP_READ));
	for op in [OP_READ, OP_CAPACITY, OP_FLUSH] {
		assert!(!Request::carries_source(op), "op {op} carries no source");
	}
	for op in [OP_WRITE, OP_CAPACITY, OP_FLUSH] {
		assert!(!Request::answers_with_data(op), "op {op} answers with no data");
	}
}

#[test]
fn the_three_statuses_stay_distinct_and_a_refusal_is_not_a_device_failure() {
	// `STATUS_INVALID` exists so a caller can tell a request it got wrong from a device that failed
	// one it got right. If these ever collapse, that distinction is gone from every server at once.
	assert_ne!(STATUS_OK, STATUS_ERR);
	assert_ne!(STATUS_ERR, STATUS_INVALID);
	assert_ne!(STATUS_OK, STATUS_INVALID);
	assert_eq!(decode_status(&reply(STATUS_INVALID)), Some(STATUS_INVALID));
}

#[test]
fn a_reply_too_short_to_hold_a_status_is_refused() {
	for short in 0..REPLY_LEN {
		assert_eq!(decode_status(&[0u8; REPLY_LEN][..short]), None);
	}
}

#[test]
fn a_capacity_reply_round_trips_through_the_bytes_the_servers_send() {
	let encoded = capacity_reply(4096 * 512, 128);
	assert_eq!(&encoded[0..4], &STATUS_OK.to_le_bytes(), "the status leads");
	assert_eq!(decode_capacity(&encoded), Some(Capacity { bytes: 4096 * 512, max_sectors: 128 }));
}

#[test]
fn a_transfer_bound_above_what_the_wire_carries_saturates_rather_than_truncating() {
	// A truncated bound publishes a SMALL number: the client then sends requests the server refuses
	// for a reason it never stated. Saturation publishes the largest number the wire can say.
	let encoded = capacity_reply(1 << 40, (u32::MAX as u64) + 7);
	assert_eq!(decode_capacity(&encoded).expect("still a capacity").max_sectors, u32::MAX);
}

#[test]
fn a_failed_capacity_query_carries_no_size_and_is_not_read_as_one() {
	// The eight bytes where a size would be are whatever the server's reply buffer held. A client
	// that read them anyway would mount a medium whose size it invented.
	let mut failed = capacity_reply(1234 * 512, 64);
	failed[0..4].copy_from_slice(&STATUS_ERR.to_le_bytes());
	assert_eq!(decode_capacity(&failed), None, "a failed query answers with no capacity at all");

	let mut refused = capacity_reply(1234 * 512, 64);
	refused[0..4].copy_from_slice(&STATUS_INVALID.to_le_bytes());
	assert_eq!(decode_capacity(&refused), None);
}

#[test]
fn a_capacity_reply_too_short_is_refused_even_when_its_status_says_ok() {
	// An ordinary four-byte OK reply is not a capacity reply, and a client waiting for one would
	// otherwise read twelve bytes past the message.
	assert_eq!(decode_capacity(&reply(STATUS_OK)), None);
	for short in 0..CAPACITY_REPLY_LEN {
		let whole = capacity_reply(512, 1);
		assert_eq!(decode_capacity(&whole[..short]), None, "{short} bytes is not a capacity reply");
	}
}

#[test]
fn the_total_decoder_and_the_checked_one_are_the_same_parser() {
	// Two parsers for one layout is how the wire drifted in the first place, so the array entry
	// point is the checked one's body rather than a second copy of it.
	let bytes: [u8; 16] = [2, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(Request::decode(&bytes), Some(Request::from_bytes(&bytes)));
	assert_eq!(Request::from_bytes(&bytes), Request { op: OP_CAPACITY, lba: 9, count: 7 });
}

#[test]
fn a_server_predating_the_per_request_bound_still_reports_a_size() {
	// Twelve bytes is what a server that only ever sent status-and-size answers. StorageService
	// reads the size and falls back to its own bound; it must not be left with no size at all.
	let whole = capacity_reply(2048 * 512, 64);
	let old: &[u8] = &whole[..CAPACITY_SIZE_LEN];
	assert_eq!(decode_capacity_bytes(old), Some(2048 * 512));
	assert_eq!(decode_capacity(old), None, "but the bound is genuinely absent rather than zero");
}

#[test]
fn a_short_or_failed_reply_yields_no_size_either() {
	for short in 0..CAPACITY_SIZE_LEN {
		let whole = capacity_reply(512, 1);
		assert_eq!(decode_capacity_bytes(&whole[..short]), None, "{short} bytes carries no size");
	}
	let mut failed = capacity_reply(512, 1);
	failed[0..4].copy_from_slice(&STATUS_ERR.to_le_bytes());
	assert_eq!(decode_capacity_bytes(&failed), None);
}
