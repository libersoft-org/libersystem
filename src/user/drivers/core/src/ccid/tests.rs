use super::*;
use crate::descriptor;
use alloc::vec::Vec;

/// THE HARNESS'S READER: the class descriptor `usb_ffs.py`'s CCID emulator presents, byte for byte - one slot,
/// T=0 and T=1, short-APDU exchange with automatic parameters, a 271-byte largest message, no PIN pad.
pub const READER_CLASS_DESCRIPTOR: [u8; 54] = [
	54,
	0x21,
	0x10,
	0x01,
	0x00,
	0x07,
	0x03,
	0x00,
	0x00,
	0x00,
	0xa0,
	0x0f,
	0x00,
	0x00,
	0xa0,
	0x0f,
	0x00,
	0x00,
	0x00,
	0x80,
	0x25,
	0x00,
	0x00,
	0x80,
	0x25,
	0x00,
	0x00,
	0x00,
	0xfe,
	0x00,
	0x00,
	0x00,
	0x00,
	0x00,
	0x00,
	0x00,
	0x00,
	0x00,
	0x00,
	0x00,
	0xba,
	0x04,
	0x02,
	0x00,
	0x0f,
	0x01,
	0x00,
	0x00,
	0xff,
	0xff,
	0x00,
	0x00,
	0x00,
	0x01,
];

fn reader(class: &[u8], with_interrupt: bool) -> Vec<u8> {
	let mut out: Vec<u8> = alloc::vec![9, descriptor::DT_CONFIG, 0, 0, 1, 1, 0, 0x80, 50];
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 0, 0, 2 + u8::from(with_interrupt), CLASS_SMART_CARD, 0, 0, 0]);
	out.extend_from_slice(class);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x01, 0x02, 0x40, 0x00, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x82, 0x02, 0x40, 0x00, 0]);
	if with_interrupt {
		out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x83, 0x03, 0x08, 0x00, 16]);
	}
	let total = out.len() as u16;
	out[2..4].copy_from_slice(&total.to_le_bytes());
	out
}

#[test]
fn the_harness_reader_binds_with_what_its_class_descriptor_says() {
	let bound = bind(&reader(&READER_CLASS_DESCRIPTOR, true)).expect("a CCID reader binds");
	assert_eq!((bound.slots, bound.protocols, bound.exchange, bound.max_message, bound.pin_support), (1, 0b11, Exchange::ShortApdu, 271, 0));
	assert_eq!(bound.interrupt_in.map(|e| e.address), Some(0x83));
	assert_eq!((bound.bulk_in.address, bound.bulk_out.address), (0x82, 0x01));
}

#[test]
fn a_reader_with_more_slots_than_the_contract_carries_is_refused() {
	let mut class = READER_CLASS_DESCRIPTOR;
	class[4] = 4;
	assert_eq!(bind(&reader(&class, true)), Err(NotBindable::TooManySlots));
}

#[test]
fn a_largest_message_that_cannot_carry_a_short_apdu_is_refused() {
	let mut class = READER_CLASS_DESCRIPTOR;
	class[44..48].copy_from_slice(&64u32.to_le_bytes());
	assert_eq!(bind(&reader(&class, false)), Err(NotBindable::MessageTooSmall));
}

#[test]
fn a_reader_with_no_class_descriptor_is_incomplete() {
	assert_eq!(bind(&reader(&[], true)), Err(NotBindable::Incomplete));
}

#[test]
fn a_message_carries_its_header_and_its_data() {
	let apdu = [0x00, 0xa4, 0x04, 0x00, 0x05, 0xa0, 0x00, 0x00, 0x03, 0x08];
	let message = encode(PC_TO_RDR_XFR_BLOCK, 0, 7, [0, 0, 0], &apdu);
	assert_eq!(&message[..HEADER], &[0x6f, 10, 0, 0, 0, 0, 7, 0, 0, 0]);
	assert_eq!(&message[HEADER..], &apdu);
}

#[test]
fn a_reply_is_believed_only_as_far_as_its_length() {
	let mut reply = alloc::vec![RDR_TO_PC_DATA_BLOCK, 2, 0, 0, 0, 0, 9, 0x00, 0, 0, 0x90, 0x00];
	let decoded = decode(&reply, 271).expect("a data block decodes");
	assert_eq!((decoded.seq, decoded.icc, decoded.command, decoded.data.as_slice()), (9, 0, 0, &[0x90, 0x00][..]));
	// A length past what arrived.
	reply[1] = 3;
	assert_eq!(decode(&reply, 271), Err(ReplyRefused::Length));
	// A length past the reader's own maximum.
	reply[1] = 2;
	assert_eq!(decode(&reply, 11), Err(ReplyRefused::Length));
	// A type no reply has, and a header cut short.
	reply[0] = PC_TO_RDR_XFR_BLOCK;
	assert_eq!(decode(&reply, 271), Err(ReplyRefused::Kind));
	assert_eq!(decode(&reply[..9], 271), Err(ReplyRefused::Short));
	// A reader asking for more time, and a failure with its error.
	let wait = decode(&[RDR_TO_PC_DATA_BLOCK, 0, 0, 0, 0, 0, 9, 0x80, 1, 0], 271).unwrap();
	assert_eq!((wait.command, wait.error), (2, 1));
	let failed = decode(&[RDR_TO_PC_SLOT_STATUS, 0, 0, 0, 0, 0, 9, 0x42, 0xfe, 0], 271).unwrap();
	assert_eq!((failed.command, failed.icc, failed.error), (1, 2, 0xfe));
}

// A reply header with no data: type, slot, sequence, and the status byte (icc in bits 0-1, command in 6-7).
fn reply(kind: u8, slot: u8, seq: u8, status: u8) -> Reply {
	decode(&[kind, 0, 0, 0, 0, slot, seq, status, 0, 0], 271).expect("a header decodes")
}

#[test]
fn a_reply_answers_only_the_request_whose_sequence_and_slot_it_names() {
	let answer = reply(RDR_TO_PC_DATA_BLOCK, 0, 9, 0x00);
	assert_eq!(answer.verdict(PC_TO_RDR_XFR_BLOCK, 9, 0), Verdict::Answer);
	// The same reply to a request on another sequence - an aborted command's answer arriving late - or to one
	// on another slot answers something else, and is dropped however it is marked.
	assert_eq!(answer.verdict(PC_TO_RDR_XFR_BLOCK, 10, 0), Verdict::Stray);
	assert_eq!(answer.verdict(PC_TO_RDR_XFR_BLOCK, 9, 1), Verdict::Stray);
	assert_eq!(reply(RDR_TO_PC_SLOT_STATUS, 0, 8, 0x42).verdict(PC_TO_RDR_XFR_BLOCK, 9, 0), Verdict::Stray, "another sequence's failure is not this request's");
	assert_eq!(reply(RDR_TO_PC_DATA_BLOCK, 0, 8, 0x02).verdict(PC_TO_RDR_XFR_BLOCK, 9, 0), Verdict::Stray, "nor is its word that the card is gone");
}

#[test]
fn more_time_is_neither_an_answer_nor_a_failure() {
	// Time extension: command status 2, whatever the icc bits and the type say.
	assert_eq!(reply(RDR_TO_PC_DATA_BLOCK, 0, 3, 0x80).verdict(PC_TO_RDR_XFR_BLOCK, 3, 0), Verdict::MoreTime);
	assert_eq!(reply(RDR_TO_PC_SLOT_STATUS, 0, 3, 0x82).verdict(PC_TO_RDR_XFR_BLOCK, 3, 0), Verdict::MoreTime);
}

#[test]
fn a_reply_of_a_type_its_request_is_never_answered_with_is_refused() {
	assert_eq!(reply(RDR_TO_PC_SLOT_STATUS, 0, 4, 0x00).verdict(PC_TO_RDR_ICC_POWER_ON, 4, 0), Verdict::WrongKind, "a power-on is answered with a data block");
	assert_eq!(reply(RDR_TO_PC_DATA_BLOCK, 0, 4, 0x00).verdict(PC_TO_RDR_SET_PARAMETERS, 4, 0), Verdict::WrongKind, "and parameters with parameters");
	assert_eq!(reply(RDR_TO_PC_PARAMETERS, 0, 4, 0x00).verdict(PC_TO_RDR_SET_PARAMETERS, 4, 0), Verdict::Answer);
	assert_eq!(reply(RDR_TO_PC_SLOT_STATUS, 0, 4, 0x01).verdict(PC_TO_RDR_ICC_POWER_OFF, 4, 0), Verdict::Answer, "a card left inactive by a power-off is the answer");
	assert_eq!(reply(RDR_TO_PC_SLOT_STATUS, 0, 4, 0x00).verdict(PC_TO_RDR_ABORT, 4, 0), Verdict::Answer);
	assert_eq!(answer_kind(0x6b), None, "a request this transport never sends has no answer it expects");
}

#[test]
fn a_gone_card_and_a_failure_are_read_only_from_the_requests_own_reply() {
	// The card is gone, said on a type the request is not answered with - still gone.
	assert_eq!(reply(RDR_TO_PC_SLOT_STATUS, 0, 5, 0x42).verdict(PC_TO_RDR_ICC_POWER_ON, 5, 0), Verdict::CardAbsent);
	assert_eq!(reply(RDR_TO_PC_DATA_BLOCK, 0, 5, 0x02).verdict(PC_TO_RDR_XFR_BLOCK, 5, 0), Verdict::CardAbsent);
	assert_eq!(reply(RDR_TO_PC_DATA_BLOCK, 0, 5, 0x40).verdict(PC_TO_RDR_XFR_BLOCK, 5, 0), Verdict::Failed);
}

#[test]
fn a_sequence_wraps_and_stays_distinct_from_its_neighbours() {
	assert_eq!(next_seq(0), 1);
	assert_eq!(next_seq(255), 0);
	let mut seq = 250u8;
	let issued: Vec<u8> = (0..10)
		.map(|_| {
			seq = next_seq(seq);
			seq
		})
		.collect();
	assert!(issued.windows(2).all(|pair| pair[0] != pair[1]), "no two consecutive requests share a sequence: {issued:?}");
	assert_eq!(reply(RDR_TO_PC_DATA_BLOCK, 0, 255, 0).verdict(PC_TO_RDR_XFR_BLOCK, 0, 0), Verdict::Stray, "the request before the wrap answers nothing after it");
}

#[test]
fn a_slot_change_says_presence_and_change_per_slot() {
	assert_eq!(slot_changes(&[RDR_TO_PC_NOTIFY_SLOT_CHANGE, 0b0000_0011], 1), Some(alloc::vec![(0, true, true)]));
	assert_eq!(slot_changes(&[RDR_TO_PC_NOTIFY_SLOT_CHANGE, 0b0010_0110], 3), Some(alloc::vec![(0, false, true), (1, true, false), (2, false, true)]));
	assert_eq!(slot_changes(&[RDR_TO_PC_NOTIFY_SLOT_CHANGE], 1), None, "a notification cut short says nothing");
	assert_eq!(slot_changes(&[RDR_TO_PC_HARDWARE_ERROR, 0, 0, 0], 1), None);
}
