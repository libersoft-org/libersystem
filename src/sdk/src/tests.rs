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
	// AND IS THEREFORE NOT REPORTED AS ONE. This test used to assert `Ok(100)` - it was named after
	// a property and then required its opposite, which is how the defect survived being written
	// down twice: the function's own comment said "the host cannot have moved more bytes than it
	// was given room for" and then returned `min(answer, capacity)`.
	//
	// Clamping is not a safe middle. On a read `Ok(100)` says a hundred fresh bytes are in the
	// buffer when the host may have written none, so the component consumes whatever was there
	// before; on a write it says the payload persisted. Both convert a detectable broken host into
	// data the caller cannot question.
	assert_eq!(clamp_count(1_000_000, 100), Err(Error::HostContract(1_000_000)));
	assert_eq!(clamp_count(i32::MAX, 100), Err(Error::HostContract(i32::MAX)));
	// The degenerate buffer, where it is clearest: nothing was offered, so nothing can have moved,
	// and a host claiming five is not reporting an empty transfer.
	assert_eq!(clamp_count(5, 0), Err(Error::HostContract(5)));
	assert_eq!(clamp_count(0, 0), Ok(0), "and zero of zero is still an honest answer");
	// One below and exactly the bound stay successful, so the refusal is a boundary and not a mood.
	assert_eq!(clamp_count(99, 100), Ok(99));
	assert_eq!(clamp_count(100, 100), Ok(100));
}

#[test]
fn an_impossible_answer_is_told_apart_from_one_this_build_has_not_heard_of() {
	// `Unknown` is forward compatibility: a newer host may define a status this guest predates, and
	// the honest answer is to carry the number rather than guess. `HostContract` is the opposite -
	// there is no future version of this world in which a host moved more bytes than it was given
	// room for, so a caller must not treat the two the same way. One may become meaningful; the
	// other is the boundary being broken.
	assert_ne!(clamp_count(-99, 16), clamp_count(99, 16));
	assert_eq!(clamp_count(-99, 16), Err(Error::Unknown(-99)));
	assert_eq!(clamp_count(99, 16), Err(Error::HostContract(99)));
}
