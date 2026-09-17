// EACH OF THESE WATCHES ONE WAY A UAS DRIVER IS WRONG, and the first is the one that makes every
// other symptom unreadable: a byte-swapped tag matches no answer, and an answer matching nothing
// looks exactly like a device fault.

use super::*;

#[test]
fn the_tag_is_big_endian_in_a_transport_that_is_little_endian_everywhere_else() {
	let iu = command_iu(0x0102, &[0u8; 8], 10);
	assert_eq!(iu[0], IU_COMMAND);
	assert_eq!(&iu[2..4], &[0x01, 0x02], "most significant byte first");
	// And the decode reads it back the same way, which is what makes an answer matchable at all.
	let mut sense = [0u8; SENSE_IU_HEADER];
	sense[0] = IU_SENSE;
	sense[2] = 0x01;
	sense[3] = 0x02;
	assert_eq!(answer(&sense), Answer::Sense { tag: 0x0102, status: 0, sense_len: 0 });
}

#[test]
fn a_ten_byte_command_adds_no_extra_command_block_dwords() {
	// The length field counts DWORDS BEYOND the sixteen a command block carries by default. Writing
	// the command's own length there makes the device read from past the end of the unit.
	assert_eq!(command_iu(1, &[0u8; 8], 10)[6], 0);
	assert_eq!(command_iu(1, &[0u8; 8], 16)[6], 0, "sixteen is still none beyond the default");
	assert_eq!(command_iu(1, &[0u8; 8], 20)[6], 1, "and twenty is one dword more");
}

#[test]
fn the_addressing_field_is_carried_whole() {
	let lun = [1u8, 3, 0x40, 0, 0, 0, 0, 0];
	assert_eq!(&command_iu(7, &lun, 10)[8..16], &lun);
}

#[test]
fn a_sense_unit_and_a_response_unit_arrive_on_one_pipe_and_are_told_apart() {
	// One is a command's status, the other the answer to a task-management request. Reading the
	// bytes without the kind gets a status out of a field that is not one.
	let mut sense = [0u8; SENSE_IU_HEADER];
	sense[0] = IU_SENSE;
	sense[2] = 0;
	sense[3] = 5;
	sense[11] = 0x02; // CHECK CONDITION
	sense[12] = 0;
	sense[13] = 18;
	assert_eq!(answer(&sense), Answer::Sense { tag: 5, status: 0x02, sense_len: 18 });

	let mut response = [0u8; 8];
	response[0] = IU_RESPONSE;
	response[3] = 5;
	response[4] = 0x09;
	assert_eq!(answer(&response), Answer::Response { tag: 5, code: 0x09 });
}

#[test]
fn a_ready_unit_is_not_a_completion() {
	// It says the device is willing to move the data. A driver treating one as a status reports a
	// transfer finished before a byte has moved.
	let mut ready = [0u8; 8];
	ready[0] = IU_READ_READY;
	ready[3] = 9;
	assert_eq!(answer(&ready), Answer::Ready { tag: 9, write: false });
	ready[0] = IU_WRITE_READY;
	assert_eq!(answer(&ready), Answer::Ready { tag: 9, write: true });
}

#[test]
fn bytes_too_short_or_of_a_kind_this_driver_does_not_model_are_refused() {
	assert_eq!(answer(&[]), Answer::Unreadable);
	assert_eq!(answer(&[IU_SENSE, 0, 0, 1]), Answer::Unreadable, "a sense unit needs its whole header");
	assert_eq!(answer(&[0xFF, 0, 0, 1]), Answer::Unreadable, "and an unknown kind is not guessed at");
}

#[test]
fn an_answer_carrying_the_reserved_tag_belongs_to_no_command() {
	// Zero is reserved, and reserving it here as well is what makes an unmatched answer
	// distinguishable from a zeroed buffer nobody wrote.
	assert_eq!(matches(NEVER_ISSUED, 5), Err(Mismatch::Reserved));
	assert_eq!(matches(7, 5), Err(Mismatch::Unknown { tag: 7 }));
	assert_eq!(matches(5, 5), Ok(()));
}

#[test]
fn the_tag_counter_skips_the_reserved_value_on_the_wrap() {
	assert_eq!(next_tag(1), 2);
	assert_eq!(next_tag(u16::MAX), 1, "and never lands on zero");
	assert_ne!(next_tag(u16::MAX), NEVER_ISSUED);
}

#[test]
fn a_residue_larger_than_the_request_is_refused_rather_than_subtracted() {
	// Subtracting it computes a negative length as an enormous positive one, and the caller is then
	// handed a buffer described as gigabytes.
	assert_eq!(moved(512, 0), Some(512));
	assert_eq!(moved(512, 512), Some(0), "nothing moved is still consistent");
	assert_eq!(moved(512, 100), Some(412));
	assert_eq!(moved(512, 513), None, "and more left over than was asked for is not a transfer");
}
