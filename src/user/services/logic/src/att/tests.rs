use super::{ATTRIBUTE_NOT_FOUND, Answer, DEFAULT_MTU, Entries, MAX_DISCOVERED, MAX_MTU, Refusal, ServerError, agreed_mtu, answer, next_range, op};

// A length-prefixed response body: the entry length, then the entries.
fn body(entry: usize, handles: &[u16], value: &[u8]) -> alloc::vec::Vec<u8> {
	let mut out = alloc::vec::Vec::new();
	out.push(entry as u8);
	for handle in handles {
		out.extend_from_slice(&handle.to_le_bytes());
		out.extend_from_slice(&value[..entry - 2]);
	}
	out
}

#[test]
// AN ERROR RESPONSE IS AN ANSWER TO ANY REQUEST and is data rather than a fault: "attribute not
// found" is how a discovery loop ENDS, and a client that treated it as a protocol violation would
// never finish one.
fn an_error_response_answers_any_request_and_ending_a_discovery_is_what_it_is_for() {
	let pdu = [op::ERROR_RESPONSE, op::READ_BY_GROUP_TYPE_REQUEST, 0x10, 0x00, ATTRIBUTE_NOT_FOUND];
	assert_eq!(answer(&pdu, op::READ_BY_GROUP_TYPE_RESPONSE, DEFAULT_MTU), Ok(Answer::Error(ServerError { request: op::READ_BY_GROUP_TYPE_REQUEST, handle: 0x0010, code: ATTRIBUTE_NOT_FOUND })));
	// The same PDU answers a different request, because it names the one it is about.
	assert!(matches!(answer(&pdu, op::READ_RESPONSE, DEFAULT_MTU), Ok(Answer::Error(_))));
	// Too short to hold the fields it must have.
	assert_eq!(answer(&pdu[..4], op::READ_RESPONSE, DEFAULT_MTU), Err(Refusal::Truncated { len: 4, needed: 5 }));
}

#[test]
// AN OPCODE THIS SUBSET DOES NOT SPEAK IS NAMED RATHER THAN IGNORED. A client that ignored unknown
// opcodes would sit through a server's answers waiting for data that had already been refused.
fn an_unknown_opcode_and_the_wrong_expected_one_are_different_refusals() {
	assert_eq!(answer(&[], op::READ_RESPONSE, DEFAULT_MTU), Err(Refusal::Empty));
	assert_eq!(answer(&[0x99], op::READ_RESPONSE, DEFAULT_MTU), Err(Refusal::UnknownOpcode(0x99)));
	// A well-known opcode that is not the one awaited: a notification arriving where a read response
	// was expected is ordinary on a live link and is not a fault the caller handles the same way.
	assert_eq!(answer(&[op::HANDLE_VALUE_NOTIFICATION, 1, 2], op::READ_RESPONSE, DEFAULT_MTU), Err(Refusal::Unexpected { got: op::HANDLE_VALUE_NOTIFICATION, want: op::READ_RESPONSE }));
	assert_eq!(answer(&[op::READ_RESPONSE, 1, 2], op::READ_RESPONSE, DEFAULT_MTU), Ok(Answer::Response(&[1, 2][..])));
	// A server exceeding the agreed MTU is a server whose bytes do not fit the buffer that agreement
	// sized.
	let long = [op::READ_RESPONSE; DEFAULT_MTU + 1];
	assert_eq!(answer(&long, op::READ_RESPONSE, DEFAULT_MTU), Err(Refusal::OverMtu { len: DEFAULT_MTU + 1, mtu: DEFAULT_MTU }));
}

#[test]
// THE SMALLER OF THE TWO AND NEVER BELOW THE DEFAULT. A server answering with less than 23 answers
// with a number the protocol does not allow, and a client that believed it would size a buffer
// smaller than the one PDU every server may send.
fn the_agreed_mtu_is_bounded_at_both_ends() {
	assert_eq!(agreed_mtu(64, 64), 64);
	assert_eq!(agreed_mtu(64, 27), 27, "the server's smaller answer");
	assert_eq!(agreed_mtu(23, 512), DEFAULT_MTU, "and this client's own");
	assert_eq!(agreed_mtu(64, 0), DEFAULT_MTU, "below the floor the protocol allows");
	assert_eq!(agreed_mtu(64, 7), DEFAULT_MTU);
	assert_eq!(agreed_mtu(65_000, 65_000), MAX_MTU, "a server proposing sixty thousand is refused, not sized for");
}

#[test]
// THE ORDINARY WALK: a response of well-formed entries, all inside the range and each past the last.
fn a_well_formed_response_yields_its_entries_and_says_where_the_next_request_begins() {
	let bytes = body(6, &[0x0010, 0x0020, 0x0030], &[1, 2, 3, 4]);
	let mut entries = Entries::new(&bytes, 0x0001, 0xffff).expect("a well-formed list");
	let seen: alloc::vec::Vec<u16> = entries.by_ref().map(|entry| entry.handle).collect();
	assert_eq!(seen, alloc::vec![0x0010, 0x0020, 0x0030]);
	assert_eq!(entries.fault(), None);
	assert_eq!(entries.last_handle(), Some(0x0030));
	assert_eq!(next_range(0x0030, 0xffff), Some((0x0031, 0xffff)));
}

#[test]
// A HANDLE THAT DOES NOT MOVE FORWARD MAKES THE CLIENT ASK THE IDENTICAL QUESTION FOR EVER. This is
// the defect that hangs a service rather than corrupting it, and it needs a server that is merely
// broken rather than hostile.
fn a_handle_that_does_not_advance_ends_the_walk_rather_than_repeating_the_request() {
	let bytes = body(4, &[0x0020, 0x0020], &[0, 0]);
	let mut entries = Entries::new(&bytes, 0x0001, 0xffff).expect("a well-formed list");
	assert_eq!(entries.next().map(|entry| entry.handle), Some(0x0020));
	assert_eq!(entries.next(), None);
	assert_eq!(entries.fault(), Some(Refusal::HandleDidNotAdvance { got: 0x0020, from: 0x0020 }));
	// Backwards is the same fault.
	let back = body(4, &[0x0020, 0x0010], &[0, 0]);
	let mut walk = Entries::new(&back, 0x0001, 0xffff).expect("a well-formed list");
	assert_eq!(walk.by_ref().count(), 1);
	assert_eq!(walk.fault(), Some(Refusal::HandleDidNotAdvance { got: 0x0010, from: 0x0020 }));
	// AND THE CLIENT'S OWN HALF: at the top of the range the addition wraps to zero and starts the
	// whole walk again.
	assert_eq!(next_range(0xffff, 0xffff), None);
	assert_eq!(next_range(0x0030, 0x0030), None, "the range is exhausted when the last handle is its end");
	assert_eq!(next_range(0x0030, 0x0040), Some((0x0031, 0x0040)));
}

#[test]
// AN ANSWER ABOUT ATTRIBUTES NOBODY ASKED FOR IS AN ANSWER ABOUT SOMEBODY ELSE'S, and a client that
// recorded it would bind a characteristic it never discovered.
fn a_handle_outside_the_range_that_was_asked_about_ends_the_walk() {
	let bytes = body(4, &[0x0050], &[0, 0]);
	let mut below = Entries::new(&bytes, 0x0100, 0x0200).expect("a well-formed list");
	assert_eq!(below.next(), None);
	assert_eq!(below.fault(), Some(Refusal::HandleOutOfRange { got: 0x0050, from: 0x0100, to: 0x0200 }));
	let above = body(4, &[0x0150, 0x0300], &[0, 0]);
	let mut past = Entries::new(&above, 0x0100, 0x0200).expect("a well-formed list");
	assert_eq!(past.by_ref().count(), 1);
	assert_eq!(past.fault(), Some(Refusal::HandleOutOfRange { got: 0x0300, from: 0x0100, to: 0x0200 }));
}

#[test]
// A LENGTH THAT DOES NOT DIVIDE WHAT FOLLOWS MEANS THE TWO ENDS DISAGREE ABOUT WHERE EACH ENTRY
// BEGINS, and every field read after the first is offset. A zero length would never advance the walk
// at all, which is the same shape of defect as a handle that does not move.
fn a_list_whose_entries_do_not_divide_it_is_refused_before_a_field_is_read() {
	let mut ragged = body(6, &[0x0010], &[1, 2, 3, 4]);
	ragged.push(0xff);
	assert_eq!(Entries::new(&ragged, 0x0001, 0xffff).err(), Some(Refusal::Ragged { entry: 6, bytes: 7 }));
	assert_eq!(Entries::new(&[0], 0x0001, 0xffff).err(), Some(Refusal::Ragged { entry: 0, bytes: 0 }));
	assert_eq!(Entries::new(&[1, 2, 3], 0x0001, 0xffff).err(), Some(Refusal::Ragged { entry: 1, bytes: 2 }), "an entry shorter than its own handle is not an entry");
	assert_eq!(Entries::new(&[4], 0x0001, 0xffff).err(), Some(Refusal::Ragged { entry: 4, bytes: 0 }), "a list with no entries is not a response");
	assert_eq!(Entries::new(&[], 0x0001, 0xffff).err(), Some(Refusal::Truncated { len: 0, needed: 1 }));
}

#[test]
// A SERVER WITH SIXTY-FIVE THOUSAND WELL-FORMED ATTRIBUTES IS NOT MALFORMED AND IS NOT A MOUSE. The
// cap is what makes a peer's attribute table a thing this service walks rather than one it is given.
fn discovery_is_bounded_by_a_count_as_well_as_by_the_range() {
	let handles: alloc::vec::Vec<u16> = (1..=(MAX_DISCOVERED as u16 + 4)).collect();
	let bytes = body(4, &handles, &[0, 0]);
	let mut entries = Entries::new(&bytes, 0x0001, 0xffff).expect("a well-formed list");
	assert_eq!(entries.by_ref().count(), MAX_DISCOVERED);
	assert_eq!(entries.fault(), Some(Refusal::TooMany));
	assert_eq!(entries.last_handle(), Some(MAX_DISCOVERED as u16), "and what was recorded is still usable");
}
