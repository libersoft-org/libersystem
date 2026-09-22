use super::{ASSEMBLY_TICKS, Fed, HEADER, MAX_SDU, Reassembly, Refusal};

// One L2CAP PDU's bytes: a declared length, a channel id, and the payload.
fn pdu(cid: u16, payload: &[u8]) -> alloc::vec::Vec<u8> {
	let mut out = alloc::vec::Vec::new();
	out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
	out.extend_from_slice(&cid.to_le_bytes());
	out.extend_from_slice(payload);
	out
}

const START: u8 = 0b10;
const CONT: u8 = 0b01;

#[test]
// THE ORDINARY CASE, AND THE ONE THE BOOT MOUSE ACTUALLY USES: a whole PDU in one fragment, on the
// attribute channel, at the default ATT MTU.
fn a_whole_pdu_in_one_fragment_completes_on_that_fragment() {
	let mut link = Reassembly::new();
	let report = pdu(0x0004, &[0x1b, 0x2a, 0x00, 0x00, 0x05, 0xfb]);
	assert_eq!(link.feed(START, &report, 0), Ok(Fed::Complete { cid: 0x0004, len: 6 }));
	assert_eq!(link.payload(), &[0x1b, 0x2a, 0x00, 0x00, 0x05, 0xfb]);
	assert!(!link.in_progress(), "a completed PDU leaves nothing in progress");
	// And the next one is a start again.
	let second = pdu(0x0004, &[0x1b, 0x2a, 0x00, 0x01, 0x00, 0x00]);
	assert_eq!(link.feed(START, &second, 0), Ok(Fed::Complete { cid: 0x0004, len: 6 }));
	assert_eq!(link.payload()[3], 0x01);
}

#[test]
// A PDU LARGER THAN ONE ACL BUFFER ARRIVES IN PIECES, which is why reassembly exists at all.
fn a_pdu_split_across_fragments_completes_on_the_last_one() {
	let mut link = Reassembly::new();
	let payload: alloc::vec::Vec<u8> = (0..200u32).map(|n| n as u8).collect();
	let whole = pdu(0x0004, &payload);
	assert_eq!(link.feed(START, &whole[..64], 0), Ok(Fed::More { owed: whole.len() - 64 }));
	assert!(link.in_progress());
	assert_eq!(link.feed(CONT, &whole[64..150], 0), Ok(Fed::More { owed: whole.len() - 150 }));
	assert_eq!(link.feed(CONT, &whole[150..], 0), Ok(Fed::Complete { cid: 0x0004, len: 200 }));
	assert_eq!(link.payload(), &payload[..]);
	assert!(!link.in_progress());
}

#[test]
// BYTES APPENDED TO A PDU THAT WAS NEVER STARTED are bytes appended to whatever the buffer held,
// which on a reused buffer is the previous PDU - so the layer above would be handed a message made
// of two.
fn a_continuation_with_nothing_to_continue_is_refused() {
	let mut link = Reassembly::new();
	assert_eq!(link.feed(CONT, &[1, 2, 3, 4], 0), Err(Refusal::NoPduInProgress));
	assert!(!link.in_progress());
	// And a second start while one is in progress is a controller that abandoned the first without
	// saying so. ONE INCOMPLETE PDU PER LINK.
	let whole = pdu(0x0004, &[0; 100]);
	assert_eq!(link.feed(START, &whole[..10], 0), Ok(Fed::More { owed: whole.len() - 10 }));
	assert_eq!(link.feed(START, &whole[..10], 0), Err(Refusal::AlreadyInProgress));
	assert!(link.in_progress(), "and the one in progress is not disturbed by the refusal");
	assert_eq!(link.feed(CONT, &whole[10..], 0), Ok(Fed::Complete { cid: 0x0004, len: 100 }));
}

#[test]
// THE DECLARED LENGTH IS THE DEVICE'S CLAIM AND THE BUFFER IS THIS PROCESS'S. A host that allocated
// to the claim would let a controller choose how much memory this service uses.
fn a_declared_length_past_what_this_service_holds_is_refused_before_a_byte_is_copied() {
	let mut link = Reassembly::new();
	let mut hostile = alloc::vec::Vec::new();
	hostile.extend_from_slice(&((MAX_SDU + 1) as u16).to_le_bytes());
	hostile.extend_from_slice(&0x0004u16.to_le_bytes());
	hostile.push(0);
	assert_eq!(link.feed(START, &hostile, 0), Err(Refusal::TooLong { declared: MAX_SDU + 1, bound: MAX_SDU }));
	assert!(!link.in_progress(), "a refused start begins nothing");
	// The ceiling itself is admitted, in pieces.
	let big = pdu(0x0004, &[7u8; MAX_SDU]);
	assert_eq!(big.len(), MAX_SDU + HEADER);
	assert_eq!(link.feed(START, &big[..100], 0), Ok(Fed::More { owed: big.len() - 100 }));
	assert_eq!(link.feed(CONT, &big[100..], 0), Ok(Fed::Complete { cid: 0x0004, len: MAX_SDU }));
	// A start too short to hold its own header is not a start.
	assert_eq!(link.feed(START, &[0, 0, 0], 0), Err(Refusal::ShortHeader { len: 3 }));
	// And a fragment with no bytes advances nothing, which would otherwise let a controller hold a
	// deadline open by sending them.
	assert_eq!(link.feed(START, &[], 0), Err(Refusal::Empty));
}

#[test]
// A FRAGMENT THAT OVERRUNS ITS OWN PDU IS A FRAMING DISAGREEMENT AND NOT A LONG PDU. Keeping what
// arrived before it would hand the layer above a message assembled out of two framings.
fn a_fragment_past_the_declared_length_gives_the_pdu_up_rather_than_truncating_it() {
	let mut link = Reassembly::new();
	// A start carrying more than its own declared length.
	let mut long_start = pdu(0x0004, &[1, 2, 3, 4]);
	long_start.extend_from_slice(&[5, 6]);
	assert_eq!(link.feed(START, &long_start, 0), Err(Refusal::Overrun { have: 10, want: 8 }));
	assert!(!link.in_progress());
	// A continuation carrying more than is owed.
	let whole = pdu(0x0004, &[9u8; 40]);
	assert_eq!(link.feed(START, &whole[..20], 0), Ok(Fed::More { owed: 24 }));
	assert_eq!(link.feed(CONT, &[0u8; 25], 0), Err(Refusal::Overrun { have: 25, want: 24 }));
	assert!(!link.in_progress(), "the PDU is given up rather than kept half-framed");
}

#[test]
// A FIRST FRAGMENT WHOSE CONTINUATIONS NEVER ARRIVE HOLDS A BUFFER FOR EVER, and a controller that
// sends one per connection holds all of them. The deadline costs one PDU and not the link.
fn an_incomplete_pdu_is_given_up_at_its_deadline() {
	let mut link = Reassembly::new();
	let whole = pdu(0x0004, &[0u8; 300]);
	assert_eq!(link.feed(START, &whole[..10], 1_000), Ok(Fed::More { owed: whole.len() - 10 }));
	assert!(!link.expire(1_000 + ASSEMBLY_TICKS - 1), "before the deadline it is still owed");
	assert!(link.in_progress());
	// AT THE DEADLINE AND NOT PAST IT.
	assert!(link.expire(1_000 + ASSEMBLY_TICKS));
	assert!(!link.in_progress());
	assert!(!link.expire(u64::MAX), "nothing in progress is nothing to expire");
	// And the link is usable immediately afterwards: the cost was one PDU.
	let next = pdu(0x0004, &[1, 2]);
	assert_eq!(link.feed(START, &next, 2_000), Ok(Fed::Complete { cid: 0x0004, len: 2 }));
}

#[test]
// A BOUNDARY FLAG THIS PROFILE DOES NOT ACCEPT IS REFUSED RATHER THAN GUESSED AT. `11` is a
// complete automatically-flushable PDU, which nothing in this milestone's scope sends; read as a
// start it would begin a PDU under a framing rule this code does not implement.
fn a_boundary_flag_this_profile_does_not_accept_is_refused() {
	let mut link = Reassembly::new();
	let whole = pdu(0x0004, &[1, 2]);
	assert_eq!(link.feed(0b11, &whole, 0), Err(Refusal::UnknownBoundary(0b11)));
	// `00` and `10` are both starts: a controller may use either and neither means "continue".
	assert_eq!(link.feed(0b00, &whole, 0), Ok(Fed::Complete { cid: 0x0004, len: 2 }));
	assert_eq!(link.feed(0b10, &whole, 0), Ok(Fed::Complete { cid: 0x0004, len: 2 }));
	// The upper bits are the broadcast flag and are not this decision's.
	assert_eq!(link.feed(0b1110, &whole, 0), Ok(Fed::Complete { cid: 0x0004, len: 2 }));
}
