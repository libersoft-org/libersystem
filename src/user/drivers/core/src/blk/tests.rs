use super::{Refusal, command_lba32, request_range, write_source};
use rt::{OBJECT_TYPE_CHANNEL, OBJECT_TYPE_MEMORY_OBJECT, ObjectInfo, RIGHT_MAP, RIGHT_READ, RIGHT_TRANSFER};

// DRV-002: the count is refused, never clamped, and the LBA is checked against the capacity.
#[test]
fn a_zero_count_and_a_count_above_the_request_limit_are_refused_not_clamped() {
	assert_eq!(request_range(0, 0, 1024, 64), Err(Refusal::Count));
	assert_eq!(request_range(0, 65, 1024, 64), Err(Refusal::Count), "65 sectors against a 64-sector request limit is refused - the clamp that used to make it 64 is gone");
	assert_eq!(request_range(0, 64, 1024, 64), Ok(64));
	assert_eq!(request_range(0, 1, 1024, 64), Ok(1));
}

#[test]
fn a_request_past_the_last_sector_is_refused_by_range() {
	// The last sector is 1023; a request ending exactly at the capacity is the last admitted one.
	assert_eq!(request_range(1023, 1, 1024, 64), Ok(1));
	assert_eq!(request_range(1020, 4, 1024, 64), Ok(4));
	assert_eq!(request_range(1021, 4, 1024, 64), Err(Refusal::Range));
	assert_eq!(request_range(1024, 1, 1024, 64), Err(Refusal::Range), "one past the end");
	assert_eq!(request_range(u64::MAX, 1, 1024, 64), Err(Refusal::Range), "an address that overflows is a range refusal, not a wrap");
	assert_eq!(request_range(u64::MAX - 1, 2, u64::MAX, 64), Err(Refusal::Range));
}

fn memory_object(rights: u32, size: u64) -> ObjectInfo {
	ObjectInfo { koid: 7, object_type: OBJECT_TYPE_MEMORY_OBJECT, rights, generation: 1, size }
}

// DRV-003 / WIRE-002: type, rights and size are three separate refusals, in that order.
#[test]
fn a_write_source_must_be_a_readable_memory_object_at_least_as_long_as_the_request() {
	let bytes = 4 * 512;
	assert_eq!(write_source(&memory_object(RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER, bytes), bytes), Ok(()));
	assert_eq!(write_source(&memory_object(RIGHT_READ | RIGHT_MAP, bytes + 512), bytes), Ok(()), "a longer object is fine; the request reads its first `bytes`");
	let channel = ObjectInfo { koid: 9, object_type: OBJECT_TYPE_CHANNEL, rights: RIGHT_READ | RIGHT_MAP, generation: 1, size: 0 };
	assert_eq!(write_source(&channel, bytes), Err(Refusal::ObjectType));
	assert_eq!(write_source(&memory_object(RIGHT_MAP | RIGHT_TRANSFER, bytes), bytes), Err(Refusal::Rights), "a handle that maps but cannot be read is the half of the finding that survived two narrowings");
	assert_eq!(write_source(&memory_object(RIGHT_READ | RIGHT_MAP, 512), bytes), Err(Refusal::Size), "count = 4 against a one-sector object is the whole of the copy defect");
	assert_eq!(write_source(&memory_object(RIGHT_READ | RIGHT_MAP, bytes - 1), bytes), Err(Refusal::Size));
}

#[test]
// DRV-002's USB HALF: a ten-byte SCSI command carries a THIRTY-TWO-BIT block address, and the driver
// wrote `lba` into four bytes without asking whether it fitted. Truncation is the failure mode and it
// is silent: a request past two terabytes names a block near the start of the medium, so a write
// lands on somebody else's data and reports success.
fn an_address_that_does_not_fit_the_command_is_refused_rather_than_truncated() {
	assert_eq!(command_lba32(0, 1), Ok(0));
	assert_eq!(command_lba32(1_000_000, 8), Ok(1_000_000));
	// The last block a ten-byte command can name, and the first one it cannot.
	assert_eq!(command_lba32(u32::MAX as u64, 1), Ok(u32::MAX));
	assert_eq!(command_lba32(u32::MAX as u64 + 1, 1), Err(Refusal::Addressing));
	// A request that STARTS inside the range and runs past it is not a short write: it is the same
	// truncation one block later.
	assert_eq!(command_lba32(u32::MAX as u64, 2), Err(Refusal::Addressing));
	// And an address that overflows on its own arithmetic is a range refusal, not an addressing one -
	// the two are different mistakes and a caller that sees them as one cannot fix either.
	assert_eq!(command_lba32(u64::MAX, 2), Err(Refusal::Range));
}
