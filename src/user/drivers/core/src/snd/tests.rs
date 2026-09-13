// DRV-009's negatives, each held against the decision that closes it.
use super::{PeriodFault, S_BAD_MSG, S_IO_ERR, S_OK, capture_outcome, period_outcome, playback_reply};

#[test]
// AN INTERRUPT IS NOT A COMPLETION. The vector is shared with queues this driver does not drive, and
// the used ring is the only thing that says a submission finished - so nothing having completed has
// to be an answer the caller can act on rather than a period reported as played.
fn a_spurious_interrupt_is_not_a_played_period() {
	assert_eq!(period_outcome(None, 0, S_OK), Err(PeriodFault::NoCompletion));
	// AND THE REPLY SAYS SO. The old path answered "OK" here, which is how the other three defects
	// stayed invisible from outside the driver.
	assert_eq!(playback_reply(period_outcome(None, 0, S_OK)), b"");
}

#[test]
// A COMPLETION FOR SOMEBODY ELSE'S DESCRIPTOR IS NOT THIS PERIOD'S. The queue is the driver's own,
// but a device that invents an id is exactly what the used-element check exists for, and believing
// one here would report a period played that was never submitted.
fn a_completion_for_another_descriptor_is_refused() {
	assert_eq!(period_outcome(Some((3, 8)), 0, S_OK), Err(PeriodFault::WrongDescriptor));
}

#[test]
// THE STATUS WORD IS WHERE A DEVICE SAYS IT COULD NOT PLAY WHAT IT WAS GIVEN, and it was never read.
// Every code that is not `S_OK` is a failure, including ones this driver has no special handling for.
fn a_device_refusal_is_a_failure_and_not_a_success() {
	assert_eq!(period_outcome(Some((0, 8)), 0, S_OK), Ok(8));
	assert_eq!(period_outcome(Some((0, 8)), 0, S_BAD_MSG), Err(PeriodFault::Device(S_BAD_MSG)));
	assert_eq!(period_outcome(Some((0, 8)), 0, S_IO_ERR), Err(PeriodFault::Device(S_IO_ERR)));
	// An unknown code is still not `S_OK`, which is the point of comparing against the one value that
	// means success rather than against the three that do not.
	assert_eq!(period_outcome(Some((0, 8)), 0, 0x8fff), Err(PeriodFault::Device(0x8fff)));
	assert_eq!(playback_reply(period_outcome(Some((0, 8)), 0, S_IO_ERR)), b"");
	assert_eq!(playback_reply(period_outcome(Some((0, 8)), 0, S_OK)), b"OK");
}

#[test]
// A DEVICE THAT WROTE FOUR BYTES DID NOT WRITE A STATUS STRUCTURE, and the word beyond what it wrote
// is whatever was in the page - which on a reused DMA page is the PREVIOUS period's status, and
// therefore an `S_OK` that belongs to a period that succeeded a moment ago.
fn a_short_status_write_is_refused_before_the_status_is_believed() {
	assert_eq!(period_outcome(Some((0, 0)), 0, S_OK), Err(PeriodFault::ShortStatus));
	assert_eq!(period_outcome(Some((0, 7)), 0, S_OK), Err(PeriodFault::ShortStatus));
	assert_eq!(period_outcome(Some((0, 8)), 0, S_OK), Ok(8));
}

#[test]
// A SHORT CAPTURE IS NOT A SHORT SUCCESS. The bytes past what the device wrote are the previous
// period's, and handing them to a recorder is handing it audio from a moment that has passed.
fn a_capture_that_filled_less_than_a_period_is_refused() {
	let period = 2048;
	assert_eq!(capture_outcome(Some((0, period + 8)), 0, S_OK, period), Ok(period + 8));
	assert_eq!(capture_outcome(Some((0, period)), 0, S_OK, period), Err(PeriodFault::ShortStatus));
	assert_eq!(capture_outcome(Some((0, 8)), 0, S_OK, period), Err(PeriodFault::ShortStatus));
	// And the playback checks still apply underneath it.
	assert_eq!(capture_outcome(None, 0, S_OK, period), Err(PeriodFault::NoCompletion));
	assert_eq!(capture_outcome(Some((0, period + 8)), 0, S_IO_ERR, period), Err(PeriodFault::Device(S_IO_ERR)));
}
