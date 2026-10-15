// DRV-005's and DRV-006's negatives, each held against the decision that closes it.
use super::{BotFault, CSW_LEN, CSW_SIGNATURE, CSW_STATUS_FAIL, CSW_STATUS_PHASE_ERROR, Csw, FlushOutcome, csw_outcome, flush_outcome};

const TAG: u32 = 0x1234_5678;

// A whole wrapper with these fields.
fn csw(signature: u32, echoed_tag: u32, status: u8, residue: u32) -> Csw {
	Csw { signature, echoed_tag, residue, status, received: CSW_LEN }
}

#[test]
// A SHORT TRANSFER IS NOT A COMPLETE ONE. The residue is the device's own account of how many of the
// bytes it was asked for did not move, and it was never read - so a device that transferred half a
// sector and said "pass" was a complete success, with the rest of the buffer holding whatever the
// previous command left there.
fn a_short_transfer_is_refused_rather_than_read_as_a_success() {
	assert_eq!(csw_outcome(&csw(CSW_SIGNATURE, TAG, 0, 0), TAG, 512, 512, false), Ok(512));
	assert_eq!(csw_outcome(&csw(CSW_SIGNATURE, TAG, 0, 256), TAG, 512, 512 - 256, false), Err(BotFault::Short { moved: 256, wanted: 512 }));
	// A reader that can use a short answer says so - the sense read asks for eighteen bytes and is
	// content with what it gets - and that is a different call rather than a different rule.
	assert_eq!(csw_outcome(&csw(CSW_SIGNATURE, TAG, 0, 6), TAG, 18, 18 - 6, true), Ok(12));
}

#[test]
// A RESIDUE LARGER THAN THE TRANSFER DESCRIBES BYTES THAT WERE NEVER ASKED FOR, and subtracting it
// wraps into a "moved" of four billion - which then passes every length check downstream.
fn a_residue_past_the_transfer_is_refused_before_it_is_subtracted() {
	assert_eq!(csw_outcome(&csw(CSW_SIGNATURE, TAG, 0, 513), TAG, 512, 512, false), Err(BotFault::Residue));
	assert_eq!(csw_outcome(&csw(CSW_SIGNATURE, TAG, 0, u32::MAX), TAG, 512, 512, true), Err(BotFault::Residue));
}

#[test]
// THE FRAMING AND THE STATUS WERE ALREADY CHECKED, and they stay checked: a wrapper that is not ours
// means the transport lost sync, and a failure is a failure whichever of the two codes it is.
fn a_wrapper_that_is_not_ours_or_reports_failure_is_refused() {
	assert_eq!(csw_outcome(&csw(0, TAG, 0, 0), TAG, 512, 512, false), Err(BotFault::Framing));
	assert_eq!(csw_outcome(&csw(CSW_SIGNATURE, TAG + 1, 0, 0), TAG, 512, 512, false), Err(BotFault::Framing));
	assert_eq!(csw_outcome(&csw(CSW_SIGNATURE, TAG, CSW_STATUS_FAIL, 0), TAG, 512, 512, false), Err(BotFault::Status(CSW_STATUS_FAIL)));
	assert_eq!(csw_outcome(&csw(CSW_SIGNATURE, TAG, CSW_STATUS_PHASE_ERROR, 0), TAG, 512, 512, false), Err(BotFault::Status(CSW_STATUS_PHASE_ERROR)));
}

#[test]
// THE TRANSPORT'S OWN COUNT OUTRANKS THE DEVICE'S WORD. A device that sent a short data-IN stage and
// then answered residue zero, PASS, has called a buffer whose tail is the previous command's
// complete; the transfer event says how many bytes arrived, and that number is what moved.
fn a_data_stage_shorter_than_the_device_claims_is_short() {
	assert_eq!(csw_outcome(&csw(CSW_SIGNATURE, TAG, 0, 0), TAG, 512, 200, false), Err(BotFault::Short { moved: 200, wanted: 512 }));
	// A reader that can use a short answer gets what arrived, not what was claimed.
	assert_eq!(csw_outcome(&csw(CSW_SIGNATURE, TAG, 0, 0), TAG, 18, 12, true), Ok(12));
	// And the two accounts agreeing is the ordinary short answer.
	assert_eq!(csw_outcome(&csw(CSW_SIGNATURE, TAG, 0, 6), TAG, 18, 12, true), Ok(12));
}

#[test]
// A WRAPPER SHORTER THAN THIRTEEN BYTES IS NOT ONE: its status byte is the zero the buffer was
// cleared to, which reads as PASS. Twelve bytes - every field but the status - is the dangerous case.
fn a_wrapper_that_did_not_arrive_whole_is_a_framing_fault() {
	for received in [0, 12, CSW_LEN - 1] {
		let short = Csw { received, ..csw(CSW_SIGNATURE, TAG, 0, 0) };
		assert_eq!(csw_outcome(&short, TAG, 512, 512, false), Err(BotFault::Framing));
	}
	assert_eq!(csw_outcome(&csw(CSW_SIGNATURE, TAG, 0, 0), TAG, 512, 512, false), Ok(512));
}

#[test]
// A DURABILITY BARRIER THAT REPORTS SUCCESS ON FAILURE IS WORSE THAN NO BARRIER: the filesystem above
// it orders its commits against a promise nothing kept. "The unit has no cache" is ONE refusal with
// ONE meaning - an illegal request naming an invalid operation code - and every other failure is a
// failure.
fn a_flush_that_failed_is_a_failure_unless_the_unit_has_no_cache() {
	assert_eq!(flush_outcome(true, &[]), FlushOutcome::Committed);
	// The one case that is not a failure: ILLEGAL REQUEST / INVALID COMMAND OPERATION CODE.
	let mut sense = [0u8; 18];
	sense[2] = 0x05;
	sense[12] = 0x20;
	assert_eq!(flush_outcome(false, &sense), FlushOutcome::NoVolatileCache);
	// A medium error is not "no cache", and this is exactly the case the old path reported as
	// success: a unit that could not commit what it had been given.
	let mut medium = [0u8; 18];
	medium[2] = 0x03;
	medium[12] = 0x0c;
	assert_eq!(flush_outcome(false, &medium), FlushOutcome::Failed);
	// An illegal request for some OTHER reason is not it either.
	let mut other = [0u8; 18];
	other[2] = 0x05;
	other[12] = 0x24;
	assert_eq!(flush_outcome(false, &other), FlushOutcome::Failed);
	// AND NO SENSE DATA IS NOT EVIDENCE OF ANYTHING. The old path did not read the sense at all,
	// which is how every failure became the one case that is not one.
	assert_eq!(flush_outcome(false, &[]), FlushOutcome::Failed);
	assert_eq!(flush_outcome(false, &[0, 0, 0x05]), FlushOutcome::Failed, "a sense buffer too short to hold the code it is read for");
}
