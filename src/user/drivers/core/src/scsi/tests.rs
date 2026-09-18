// EACH OF THESE WATCHES ONE WAY A SCSI DRIVER IS WRONG, and the three that matter most are silent:
// an address written in the wrong byte order, a capacity read as a count rather than as the last
// block, and a sense key acted on without the code that qualifies it.

use super::*;
use alloc::vec::Vec;

#[test]
fn a_block_address_is_big_endian_which_is_the_opposite_of_every_other_wire_here() {
	// A driver writing it the way the block wire carries it reads from a plausible address on the
	// same medium, and nothing anywhere refuses it.
	let cdb = read_write10(false, 0x0102_0304, 8);
	assert_eq!(cdb[0], READ10);
	assert_eq!(&cdb[2..6], &[0x01, 0x02, 0x03, 0x04], "network byte order, most significant first");
	assert_eq!(&cdb[7..9], &[0x00, 0x08], "and so is the block count");
	assert_eq!(read_write10(true, 0, 1)[0], WRITE10, "and the opcode is the only difference");
}

#[test]
fn the_capacity_answer_is_the_last_block_and_not_the_count() {
	// The off-by-one that makes a driver read one block past the end of every medium it serves, and
	// the block past the end is the one the medium refuses - so it surfaces as an I/O error on the
	// last sector of every disk rather than as anything naming the arithmetic.
	let mut answer = [0u8; 8];
	answer[..4].copy_from_slice(&1023u32.to_be_bytes());
	answer[4..].copy_from_slice(&512u32.to_be_bytes());
	assert_eq!(capacity(&answer), Ok(Capacity { blocks: 1024, block_bytes: 512 }));

	// A unit of exactly one block reports a last block of zero, so zero is not an empty medium.
	let mut one = [0u8; 8];
	one[4..].copy_from_slice(&512u32.to_be_bytes());
	assert_eq!(capacity(&one), Ok(Capacity { blocks: 1, block_bytes: 512 }));
}

#[test]
fn a_medium_whose_block_size_the_contract_cannot_carry_is_refused() {
	let mut answer = [0u8; 8];
	answer[..4].copy_from_slice(&1023u32.to_be_bytes());
	answer[4..].copy_from_slice(&4096u32.to_be_bytes());
	assert_eq!(capacity(&answer), Err(Unservable::BlockSize));
	assert_eq!(capacity(&[0u8; 4]), Err(Unservable::Short), "an answer too short to hold one");
}

#[test]
fn the_value_that_means_ask_with_the_longer_command_is_not_read_as_a_capacity() {
	// `0xFFFFFFFF` says the medium is larger than this command can express and the sixteen-byte form
	// must be used. Read as a last block it is a four-terabyte medium the driver cannot address, and
	// every request past two terabytes would then be built with a truncated address.
	let mut answer = [0u8; 8];
	answer[..4].copy_from_slice(&u32::MAX.to_be_bytes());
	answer[4..].copy_from_slice(&512u32.to_be_bytes());
	assert_eq!(capacity(&answer), Err(Unservable::Short));
}

// Fixed-format sense data with the fields this core reads.
fn sense_bytes(key: u8, asc: u8, ascq: u8) -> [u8; SENSE_LEN] {
	let mut data = [0u8; SENSE_LEN];
	data[0] = 0x70;
	data[2] = key;
	data[7] = 10; // additional length
	data[12] = asc;
	data[13] = ascq;
	data
}

#[test]
fn not_ready_covers_a_unit_spinning_up_and_one_with_no_medium_and_the_key_alone_cannot_tell() {
	// THE LINE A SCSI DRIVER MOST OFTEN GETS WRONG. Reading the key alone either retries a permanent
	// failure for ever or gives up on a unit that was a second from ready.
	assert_eq!(sense(&sense_bytes(0x02, 0x04, 0x01)), Sense::NotReadyYet, "becoming ready");
	assert_eq!(sense(&sense_bytes(0x02, 0x3A, 0x00)), Sense::NotReady, "no medium present");
	assert!(sense(&sense_bytes(0x02, 0x04, 0x01)).retryable());
	assert!(!sense(&sense_bytes(0x02, 0x3A, 0x00)).retryable());
}

#[test]
fn a_power_on_attention_is_refused_once_and_clears_by_being_read() {
	// Which is why a bring-up retries: the unit's first command after power-on or a medium change is
	// refused, and reading the sense is what clears it.
	assert_eq!(sense(&sense_bytes(0x06, 0x28, 0x00)), Sense::AttentionCleared);
	assert!(sense(&sense_bytes(0x06, 0x28, 0x00)).retryable());
}

#[test]
fn a_medium_failure_and_a_refused_command_are_told_apart_and_neither_is_retried() {
	assert_eq!(sense(&sense_bytes(0x03, 0x11, 0x00)), Sense::Failed, "unrecovered read error");
	assert_eq!(sense(&sense_bytes(0x04, 0x44, 0x00)), Sense::Failed, "hardware error");
	assert_eq!(sense(&sense_bytes(0x05, 0x21, 0x00)), Sense::Refused, "address out of range");
	assert!(!sense(&sense_bytes(0x03, 0x11, 0x00)).retryable());
	assert!(!sense(&sense_bytes(0x05, 0x21, 0x00)).retryable());
}

#[test]
fn no_sense_is_not_a_failure_and_an_unmodelled_key_says_so_rather_than_guessing() {
	assert_eq!(sense(&sense_bytes(0x00, 0, 0)), Sense::None);
	assert_eq!(sense(&sense_bytes(0x01, 0, 0)), Sense::Other { key: 0x01 }, "recovered error");
	assert_eq!(sense(&sense_bytes(0x0B, 0, 0)), Sense::Other { key: 0x0B }, "aborted command");
}

#[test]
fn bytes_that_are_not_sense_data_are_refused_rather_than_read_as_a_key() {
	// A short answer, or one whose response code is not a fixed format, is not sense data - and
	// reading byte two of it as a key is how a driver decides a working unit has failed.
	assert_eq!(sense(&[0u8; 4]), Sense::Other { key: 0xFF }, "too short");
	let mut wrong = sense_bytes(0x02, 0x04, 0x01);
	wrong[0] = 0x72; // descriptor format, which this core does not read
	assert_eq!(sense(&wrong), Sense::Other { key: 0xFF });
}

#[test]
fn the_virtio_addressing_field_puts_the_target_in_the_second_byte_and_not_the_first() {
	// A driver that wrote the target into byte zero addresses nothing, and the device answers with a
	// bad target that looks exactly like an empty bus.
	let lun = virtio_lun(3, 0);
	assert_eq!(lun[0], 1, "the fixed first byte");
	assert_eq!(lun[1], 3, "the target");
	assert_eq!(lun[2] & 0xC0, 0x40, "and the logical unit's own encoding");
	let higher = virtio_lun(0, 5);
	assert_eq!(higher[3], 5);
}

#[test]
fn a_request_the_device_never_delivered_is_not_a_success_because_the_target_set_no_status() {
	// Two fields and not one: the response code says whether the DEVICE carried it, the status says
	// what the TARGET thought. Reading only the status calls an undelivered request a success.
	assert_eq!(virtio_outcome(0, 0x00), Ok(()));
	assert_eq!(virtio_outcome(1, 0x00), Err(Sense::Failed), "the device did not deliver it");
	assert_eq!(virtio_outcome(0, 0x02), Err(Sense::Other { key: 0xFE }), "delivered, and the target refused it");
	assert_eq!(virtio_outcome(0, 0x08), Err(Sense::Failed), "busy");
}

#[test]
fn the_commands_that_carry_nothing_carry_nothing() {
	assert_eq!(test_unit_ready(), [TEST_UNIT_READY, 0, 0, 0, 0, 0]);
	assert_eq!(read_capacity10(), [READ_CAPACITY10, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
	// SYNCHRONIZE CACHE with a count of zero means the WHOLE medium rather than nothing, which is
	// the one place a zero length is not an empty request.
	assert_eq!(synchronize_cache10()[0], SYNCHRONIZE_CACHE10);
	assert_eq!(&synchronize_cache10()[7..9], &[0, 0]);
	// And the sense request's allocation length is one byte, so what a caller asks for has to fit.
	assert_eq!(request_sense(SENSE_LEN as u8)[4], SENSE_LEN as u8);
}

// A `REPORT LUNS` answer: the header, then one eight-byte addressing field per unit. Built here the
// way a target builds it, so a test states the wire rather than a decoded shape.
fn report_luns_answer(entries: &[[u8; 8]], claimed_bytes: u32) -> Vec<u8> {
	let mut out: Vec<u8> = Vec::new();
	out.extend_from_slice(&claimed_bytes.to_be_bytes());
	out.extend_from_slice(&[0u8; 4]);
	for entry in entries {
		out.extend_from_slice(entry);
	}
	out
}

#[test]
fn the_lun_list_length_counts_bytes_and_not_units() {
	// Read as a count, a target with four units reports four BYTES of list: the driver takes zero
	// whole entries out of it and serves NONE of the four. The number is right there in the answer,
	// which is why nothing downstream ever contradicts it.
	let entries = [[0u8, 0, 0, 0, 0, 0, 0, 0], [0u8, 1, 0, 0, 0, 0, 0, 0], [0u8, 2, 0, 0, 0, 0, 0, 0], [0u8, 3, 0, 0, 0, 0, 0, 0]];
	let answer = report_luns_answer(&entries, 32);
	let mut out = [0u16; 8];
	assert_eq!(luns(&answer, &mut out), 4, "four units, from a list thirty-two bytes long");
	assert_eq!(&out[..4], &[0, 1, 2, 3]);

	// AND THE MISREAD IS NOT MERELY FEWER UNITS: a claim of 4 is half an entry, which rounds down to
	// none at all. A test asserting only "some units" would pass on the defect.
	let misread = report_luns_answer(&entries, 4);
	assert_eq!(luns(&misread, &mut out), 0, "a claim of four BYTES holds no whole entry");
}

#[test]
fn a_list_longer_than_the_answer_is_clamped_to_what_arrived() {
	// A target says "ask again with more" by claiming a list longer than the allocation it was
	// given. The claim is the DEVICE'S, and a driver that indexed by it would read past the answer -
	// which here is a buffer inside the driver's own DMA page, so what it would read is whatever the
	// last command left behind and would publish as a medium.
	let entries = [[0u8, 7, 0, 0, 0, 0, 0, 0]];
	let answer = report_luns_answer(&entries, 8 * 64);
	let mut out = [0u16; 8];
	assert_eq!(luns(&answer, &mut out), 1, "one entry arrived, however many the target claims exist");
	assert_eq!(out[0], 7);
}

#[test]
fn the_addressing_method_decides_which_number_the_same_bytes_name() {
	// 0x40, 0x01 is FLAT unit 1 and 0x00, 0x01 is PERIPHERAL unit 1 - but 0x41, 0x01 is flat unit
	// 0x0101, and a driver reading the second byte alone calls it unit 1. Unit 1 usually exists and
	// answers, so the wrong medium is served under the right name.
	assert_eq!(lun_number(&[0x00, 0x01, 0, 0, 0, 0, 0, 0]), Some(1), "peripheral addressing, bus zero");
	assert_eq!(lun_number(&[0x40, 0x01, 0, 0, 0, 0, 0, 0]), Some(1), "flat space, low byte only");
	assert_eq!(lun_number(&[0x41, 0x01, 0, 0, 0, 0, 0, 0]), Some(0x0101), "flat space across both bytes");
	// A peripheral field naming a bus other than zero is a unit behind ANOTHER bus, reached through
	// that bus's own nexus rather than by dropping the bus number.
	assert_eq!(lun_number(&[0x01, 0x05, 0, 0, 0, 0, 0, 0]), None, "peripheral addressing on bus one");
	// Logical-unit addressing and the extended form, which is where the well-known units live.
	assert_eq!(lun_number(&[0x80, 0x00, 0, 0, 0, 0, 0, 0]), None);
	assert_eq!(lun_number(&[0xC1, 0x00, 0, 0, 0, 0, 0, 0]), None, "a well-known unit is not a medium");
	// A second level names a unit BEHIND the one the first two bytes name.
	assert_eq!(lun_number(&[0x00, 0x01, 0x40, 0x02, 0, 0, 0, 0]), None, "a second level is refused, not dropped");
	assert_eq!(lun_number(&[0x00, 0x01, 0, 0, 0, 0, 0]), None, "and a field too short to be one");
}

#[test]
fn a_unit_listed_twice_is_published_once() {
	// Two providers over one medium, each able to write under the other. The duplicate is the
	// target's to send and the driver's to drop.
	let entries = [[0u8, 0, 0, 0, 0, 0, 0, 0], [0u8, 2, 0, 0, 0, 0, 0, 0], [0u8, 0, 0, 0, 0, 0, 0, 0], [0x40u8, 0x02, 0, 0, 0, 0, 0, 0]];
	let answer = report_luns_answer(&entries, 32);
	let mut out = [0u16; 8];
	assert_eq!(luns(&answer, &mut out), 2, "two distinct units out of four entries");
	assert_eq!(&out[..2], &[0, 2]);
}

#[test]
fn the_report_luns_allocation_length_is_the_four_bytes_in_the_middle() {
	// Every other command here carries its allocation in one or two bytes near the end; this one
	// puts thirty-two bits at offset six, and a driver writing it where the ten-byte forms carry
	// theirs asks for zero bytes and gets an empty list from a target with units on it.
	let cdb = report_luns(264);
	assert_eq!(cdb[0], REPORT_LUNS);
	assert_eq!(cdb.len(), CDB12_LEN, "REPORT LUNS is defined at twelve bytes");
	assert_eq!(&cdb[6..10], &264u32.to_be_bytes(), "big-endian, at offset six");
	assert_eq!(cdb[2], 0, "select report zero: the units this nexus addresses, not the well-known ones");
}

#[test]
fn an_answer_too_short_to_hold_a_header_names_no_units() {
	let mut out = [0u16; 8];
	assert_eq!(luns(&[0u8; 7], &mut out), 0);
	assert_eq!(luns(&report_luns_answer(&[], 0), &mut out), 0, "a target with nothing on it");
	// And a caller with nowhere to put them asks for none.
	assert_eq!(luns(&report_luns_answer(&[[0u8; 8]], 8), &mut []), 0);
}

#[test]
fn more_units_than_the_caller_can_hold_are_taken_up_to_its_bound() {
	let entries = [[0u8, 0, 0, 0, 0, 0, 0, 0], [0u8, 1, 0, 0, 0, 0, 0, 0], [0u8, 2, 0, 0, 0, 0, 0, 0]];
	let answer = report_luns_answer(&entries, 24);
	let mut out = [0u16; 2];
	assert_eq!(luns(&answer, &mut out), 2, "bounded by the caller's own array");
	assert_eq!(out, [0, 1]);
}

// One event buffer, built the way the device writes it: the event word, the addressing field, the
// reason word.
fn event_buffer(word: u32, lun: [u8; 8], reason: u32) -> Vec<u8> {
	let mut out: Vec<u8> = Vec::new();
	out.extend_from_slice(&word.to_le_bytes());
	out.extend_from_slice(&lun);
	out.extend_from_slice(&reason.to_le_bytes());
	out
}

#[test]
fn the_missed_flag_is_not_part_of_the_event_number() {
	// The device ORs it in when it ran out of buffers and threw events away. A driver comparing the
	// whole word stops recognising events exactly when its picture of the bus is known to be stale -
	// so it goes quiet on a busy bus, which is the shape of a driver that is working.
	let lun = virtio_lun(1, 0);
	let plain = event_buffer(1, lun, 1);
	assert_eq!(virtio_event(&plain), Some((Event::Rescan, lun, false)));

	let after_overflow = event_buffer(0x8000_0001, lun, 1);
	assert_eq!(virtio_event(&after_overflow), Some((Event::Rescan, lun, true)), "the same event, and the device says it dropped some");
}

#[test]
fn a_transport_reset_is_three_different_things_and_the_reason_says_which() {
	// One event number covers a unit arriving, a unit leaving and a unit resetting itself. Acting on
	// the number alone treats a removal as an arrival: the driver re-enumerates a unit that is gone
	// and publishes a provider over nothing.
	let lun = virtio_lun(0, 3);
	assert_eq!(virtio_event(&event_buffer(1, lun, 1)).map(|event| event.0), Some(Event::Rescan));
	assert_eq!(virtio_event(&event_buffer(1, lun, 2)).map(|event| event.0), Some(Event::Removed));
	assert_eq!(virtio_event(&event_buffer(1, lun, 3)).map(|event| event.0), Some(Event::HardReset));
	assert_eq!(virtio_event(&event_buffer(1, lun, 99)).map(|event| event.0), Some(Event::Other), "a reason this core does not model");
	// And the unit it is about travels with it, so a driver acts on one unit rather than all of them.
	assert_eq!(virtio_event(&event_buffer(1, lun, 2)).map(|event| event.1), Some(lun));
}

#[test]
fn an_empty_or_short_event_buffer_says_so() {
	let lun = virtio_lun(0, 0);
	assert_eq!(virtio_event(&event_buffer(0, lun, 0)).map(|event| event.0), Some(Event::None), "a buffer the device handed back untouched");
	assert_eq!(virtio_event(&event_buffer(3, lun, 0)).map(|event| event.0), Some(Event::ParamChange));
	assert_eq!(virtio_event(&[0u8; EVENT_LEN - 1]), None, "too short to hold one");
}
