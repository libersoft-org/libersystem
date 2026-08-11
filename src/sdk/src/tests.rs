// The SDK's own tests: the half of it that can be decided without a host.
//
// `src/sdk` had no tests at all, and its whole coverage was one kernel end-to-end happy path - the
// one that could not fail on the write. These need no guest, no interpreter and no boot, and they
// pin the two things the wrappers actually do: map a status, and refuse to believe a count.

use crate::{Error, STATUS_DENIED, STATUS_FAULT, STATUS_IO, STATUS_UNSUPPORTED, clamp_count};

#[test]
fn a_negative_answer_is_a_status_and_a_non_negative_one_is_a_count() {
	assert_eq!(clamp_count(0, 16), Ok(0), "an empty read is not an error - which is the whole reason the model exists");
	assert_eq!(clamp_count(7, 16), Ok(7));
	assert_eq!(clamp_count(16, 16), Ok(16), "exactly the buffer is a legitimate answer");

	assert_eq!(clamp_count(STATUS_DENIED, 16), Err(Error::Denied));
	assert_eq!(clamp_count(STATUS_FAULT, 16), Err(Error::Fault));
	assert_eq!(clamp_count(STATUS_IO, 16), Err(Error::Io));
	assert_eq!(clamp_count(STATUS_UNSUPPORTED, 16), Err(Error::Unsupported));
	// A code this build has never heard of is reported, not invented: a newer host may define one,
	// and an older guest guessing at its meaning is worse than saying it does not know.
	assert_eq!(clamp_count(-99, 16), Err(Error::Unknown(-99)));
	assert_eq!(clamp_count(i32::MIN, 16), Err(Error::Unknown(i32::MIN)));
}

#[test]
fn a_count_larger_than_the_buffer_is_not_a_count() {
	// `write_output` returned whatever the host said. A host regression answering a million for a
	// hundred-byte write handed a million straight to the application - through the wrapper whose
	// whole job is to be the safe side of that boundary.
	assert_eq!(clamp_count(1_000_000, 100), Ok(100));
	assert_eq!(clamp_count(i32::MAX, 100), Ok(100));
	// And the degenerate buffer: nothing offered, nothing can have moved.
	assert_eq!(clamp_count(5, 0), Ok(0));
}
