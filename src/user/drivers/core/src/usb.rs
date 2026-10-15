// The USB mass-storage decisions, as pure functions: what a Bulk-Only Transport status wrapper means
// and what a failed flush means, tested on the host through the crate's seam.
//
// TWO DEBTS LIVE HERE.
//
// THE WRAPPER WAS READ FOR THREE OF ITS FIVE FIELDS (DRV-006). A Command Status Wrapper carries a
// signature, the tag it echoes, the RESIDUE - how many of the bytes the command asked for were not
// transferred - and a status. The driver checked the first two and the last, and never looked at the
// residue or at how many bytes the data stage actually moved: a device that transferred half a sector
// and said "pass" was read as a complete success, and the rest of the buffer is whatever the previous
// command left there.
//
// AND A FLUSH THAT FAILED TWICE WAS CALLED A SUCCESS (DRV-005). `SYNCHRONIZE CACHE` is optional in
// SBC, so a unit with no volatile cache refuses it - and refusing it means the barrier already holds.
// But that is ONE refusal with ONE meaning, and the driver treated EVERY repeated failure as that
// case, including a unit that could not commit its cache. A durability barrier that reports success
// on failure is worse than no barrier: the filesystem above it orders its commits against a promise
// nothing kept.

// The Bulk-Only Transport status wrapper's own constants.
pub const CSW_SIGNATURE: u32 = 0x5342_5355;
pub const CSW_STATUS_PASS: u8 = 0;
pub const CSW_STATUS_FAIL: u8 = 1;
pub const CSW_STATUS_PHASE_ERROR: u8 = 2;

// Why a status wrapper is not a completed command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotFault {
	// The signature or the echoed tag is not ours: the transport has lost sync and needs a reset.
	Framing,
	// The device reported a failure, with the status byte it gave.
	Status(u8),
	// The residue is larger than the command asked for, which describes a transfer that did not
	// happen and cannot be subtracted from anything.
	Residue,
	// The device transferred fewer bytes than the command asked for. A SHORT TRANSFER IS NOT A
	// COMPLETE ONE, and the bytes past it are the previous command's.
	Short { moved: u32, wanted: u32 },
}

// The status wrapper is thirteen bytes, and a shorter one is not a wrapper.
pub const CSW_LEN: u32 = 13;

// A status wrapper as it arrived: its four fields, and how many of its thirteen bytes the transport
// actually delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Csw {
	pub signature: u32,
	pub echoed_tag: u32,
	pub residue: u32,
	pub status: u8,
	pub received: u32,
}

// What a status wrapper says about the command that produced it, in bytes actually transferred.
//
// `wanted` is the data stage's length as the command block asked for it; `observed` is what the
// transport itself delivered in that stage - the transfer event's length - and it is checked
// against the device's own residue: two independent accounts of the same number, and the smaller is
// what moved. A device that answers residue zero after a data stage that came up short has called
// a buffer whose tail is the previous command's complete.
//
// A WRAPPER SHORTER THAN THIRTEEN BYTES IS NOT ONE. Its missing bytes read as whatever the buffer
// held - the zeroes the driver cleared it to - and a status byte of zero is PASS.
pub fn csw_outcome(csw: &Csw, expected_tag: u32, wanted: u32, observed: u32, allow_short: bool) -> Result<u32, BotFault> {
	if csw.received != CSW_LEN || csw.signature != CSW_SIGNATURE || csw.echoed_tag != expected_tag {
		return Err(BotFault::Framing);
	}
	if csw.status != CSW_STATUS_PASS {
		return Err(BotFault::Status(csw.status));
	}
	// THE RESIDUE IS CHECKED BEFORE IT IS SUBTRACTED. A residue larger than the transfer describes
	// bytes that were never asked for, and subtracting it wraps into a very large "moved".
	if csw.residue > wanted {
		return Err(BotFault::Residue);
	}
	let moved = (wanted - csw.residue).min(observed);
	if !allow_short && moved < wanted {
		return Err(BotFault::Short { moved, wanted });
	}
	Ok(moved)
}

// What a failed `SYNCHRONIZE CACHE` means, from the sense data the failure left behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlushOutcome {
	// The unit committed its cache.
	Committed,
	// The unit does not implement the command, which per SBC means it has no volatile cache - so the
	// barrier holds without it. THIS IS ONE REFUSAL WITH ONE MEANING and not every failure.
	NoVolatileCache,
	// Anything else: the barrier did NOT hold, and saying otherwise is the defect.
	Failed,
}

// The sense key and additional sense code that mean "this unit does not implement that command".
const SENSE_ILLEGAL_REQUEST: u8 = 0x05;
const ASC_INVALID_COMMAND: u8 = 0x20;

// Decide a flush from whether the command succeeded and, when it did not, from the sense data read
// after it. `sense` is the fixed-format sense buffer as returned; a short one is not evidence of
// anything and is therefore a failure rather than an assumption.
pub fn flush_outcome(succeeded: bool, sense: &[u8]) -> FlushOutcome {
	if succeeded {
		return FlushOutcome::Committed;
	}
	// Fixed-format sense: the key is the low nibble of byte 2, the additional sense code is byte 12.
	let Some(&key) = sense.get(2) else {
		return FlushOutcome::Failed;
	};
	let Some(&asc) = sense.get(12) else {
		return FlushOutcome::Failed;
	};
	if key & 0x0f == SENSE_ILLEGAL_REQUEST && asc == ASC_INVALID_COMMAND { FlushOutcome::NoVolatileCache } else { FlushOutcome::Failed }
}

#[cfg(test)]
mod tests;
