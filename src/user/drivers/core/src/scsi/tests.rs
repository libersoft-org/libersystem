// EACH OF THESE WATCHES ONE WAY A SCSI DRIVER IS WRONG, and the three that matter most are silent:
// an address written in the wrong byte order, a capacity read as a count rather than as the last
// block, and a sense key acted on without the code that qualifies it.

use super::*;

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
