// THE USB ATTACHED SCSI INFORMATION UNITS, WITH NO CONTROLLER BEHIND THEM.
//
// UAS is the SCSI command set again - `drivers::scsi` is unchanged and shared - carried differently.
// Bulk-Only Transport is one command, one data stage and one status, strictly in order on two
// endpoints. UAS is FOUR pipes with a TAG joining them, and the device may answer in any order. That
// difference is the whole of this module: everything here is about telling one answer from another
// and matching it to the command that asked.
//
// WHERE A UAS DRIVER IS WRONG is the tag. It is BIG-ENDIAN in a protocol whose transport is little-
// endian everywhere else, so a byte-swapped tag matches nothing - and an answer that matches nothing
// is indistinguishable from a device fault unless the decode is right.

/// Information unit identifiers.
pub const IU_COMMAND: u8 = 0x01;
pub const IU_SENSE: u8 = 0x03;
pub const IU_RESPONSE: u8 = 0x04;
pub const IU_TASK_MANAGEMENT: u8 = 0x05;
pub const IU_READ_READY: u8 = 0x06;
pub const IU_WRITE_READY: u8 = 0x07;

/// A command information unit is sixteen bytes of header and then the command descriptor block.
pub const COMMAND_IU_HEADER: usize = 16;
/// A sense information unit's fixed part, before the sense data it carries.
pub const SENSE_IU_HEADER: usize = 16;

/// THE TAG THIS DRIVER NEVER ISSUES. Zero is reserved by the specification, and reserving it here as
/// well is what makes an unmatched answer distinguishable from a zeroed buffer nobody wrote - the
/// same rule, for the same reason, as the NVMe driver's command ids.
pub const NEVER_ISSUED: u16 = 0;

/// Build a command information unit's header.
///
/// THE TAG IS BIG-ENDIAN AND THE LENGTH FIELD IS THE CDB'S, and both are easy to get wrong in
/// opposite directions: a swapped tag matches no answer, and a length that counted the header makes
/// the device read the command block from past its end.
pub fn command_iu(tag: u16, lun: &[u8; 8], cdb_len: u8) -> [u8; COMMAND_IU_HEADER] {
	let mut iu = [0u8; COMMAND_IU_HEADER];
	iu[0] = IU_COMMAND;
	iu[2] = (tag >> 8) as u8;
	iu[3] = tag as u8;
	// Task attribute: simple, which is the only one this driver issues.
	iu[4] = 0;
	// The additional CDB length, in DWORDS beyond the sixteen a command block carries by default. A
	// ten-byte command needs none, which is why this is zero for everything this driver sends.
	iu[6] = if cdb_len > 16 { (cdb_len - 16).div_ceil(4) } else { 0 };
	iu[8..16].copy_from_slice(lun);
	iu
}

/// Task-management functions, from the UAS specification's own table.
///
/// ABORT TASK NAMES A TAG AND LOGICAL UNIT RESET NAMES A UNIT, which is why the two are not
/// interchangeable escalation steps: the first asks the device to forget ONE command and leaves
/// everything else outstanding, the second throws away every task on that unit including ones this
/// driver is still waiting for.
pub const TMF_ABORT_TASK: u8 = 0x01;
pub const TMF_LOGICAL_UNIT_RESET: u8 = 0x0E;

/// A task-management information unit is sixteen bytes.
pub const TASK_MANAGEMENT_IU_LEN: usize = 16;

/// Response codes a device answers a task-management request with.
pub const RESPONSE_COMPLETE: u8 = 0x00;
pub const RESPONSE_INVALID_IU: u8 = 0x02;
pub const RESPONSE_NOT_SUPPORTED: u8 = 0x04;
pub const RESPONSE_FAILED: u8 = 0x05;
pub const RESPONSE_SUCCEEDED: u8 = 0x08;
pub const RESPONSE_INCORRECT_LUN: u8 = 0x09;

/// Whether a response code says the function was carried out.
///
/// TWO CODES MEAN YES AND THEY ARE NOT ADJACENT. `0x00` is "function complete" and `0x08` is
/// "function succeeded"; a driver checking for zero alone treats a successful abort as a failure
/// and escalates to a unit reset that throws away commands that were fine.
pub fn task_done(code: u8) -> bool {
	code == RESPONSE_COMPLETE || code == RESPONSE_SUCCEEDED
}

/// Build a task-management information unit.
///
/// THE TAG IS THIS REQUEST'S OWN AND THE MANAGED TAG IS THE ONE BEING ABORTED, in two different
/// fields, both big-endian. A driver that put the doomed command's tag in the header would be
/// asking the device to answer under a tag that is already outstanding - and then matching the
/// answer against the wrong one.
pub fn task_management_iu(tag: u16, function: u8, managed_tag: u16, lun: &[u8; 8]) -> [u8; TASK_MANAGEMENT_IU_LEN] {
	let mut iu = [0u8; TASK_MANAGEMENT_IU_LEN];
	iu[0] = IU_TASK_MANAGEMENT;
	iu[2] = (tag >> 8) as u8;
	iu[3] = tag as u8;
	iu[4] = function;
	iu[6] = (managed_tag >> 8) as u8;
	iu[7] = managed_tag as u8;
	iu[8..16].copy_from_slice(lun);
	iu
}

/// What arrived on the status pipe.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer {
	/// A status and its sense length, for the command carrying this tag.
	Sense { tag: u16, status: u8, sense_len: u16 },
	/// The device is ready to send or receive the data for this tag. On a controller without
	/// streams these are informational, and a driver that treated one as a completion would report a
	/// transfer finished before a byte had moved.
	Ready { tag: u16, write: bool },
	/// The answer to a task-management request.
	Response { tag: u16, code: u8 },
	/// Too short to be an information unit, or a kind this driver does not model.
	Unreadable,
}

/// Decode one information unit from the status pipe.
///
/// A SENSE UNIT AND A RESPONSE UNIT ARRIVE ON THE SAME PIPE and mean different things - one is a
/// command's status, the other the answer to a task-management request - so the kind is read before
/// anything else is believed about the bytes.
pub fn answer(iu: &[u8]) -> Answer {
	if iu.len() < 4 {
		return Answer::Unreadable;
	}
	let tag = u16::from_be_bytes([iu[2], iu[3]]);
	match iu[0] {
		IU_SENSE => {
			if iu.len() < SENSE_IU_HEADER {
				return Answer::Unreadable;
			}
			// The status is at 11 and the sense length at 12, big-endian like the tag.
			Answer::Sense { tag, status: iu[11], sense_len: u16::from_be_bytes([iu[12], iu[13]]) }
		}
		IU_RESPONSE => {
			if iu.len() < 8 {
				return Answer::Unreadable;
			}
			Answer::Response { tag, code: iu[3 + 1] }
		}
		IU_READ_READY => Answer::Ready { tag, write: false },
		IU_WRITE_READY => Answer::Ready { tag, write: true },
		_ => Answer::Unreadable,
	}
}

/// Why an answer cannot be acted on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mismatch {
	/// It carries the reserved tag, so it belongs to no command this driver issued.
	Reserved,
	/// It carries a tag that is not the one outstanding.
	Unknown { tag: u16 },
}

/// Match an answer's tag against the command waiting for it.
///
/// AN UNKNOWN TAG IS NOT THE OUTSTANDING COMMAND'S ANSWER, and matching it to the nearest one is how
/// a driver reports somebody else's status as this command's. With one command outstanding that is a
/// stale answer from a command already abandoned; with several it is worse.
pub fn matches(tag: u16, waiting_for: u16) -> Result<(), Mismatch> {
	if tag == NEVER_ISSUED {
		return Err(Mismatch::Reserved);
	}
	if tag != waiting_for {
		return Err(Mismatch::Unknown { tag });
	}
	Ok(())
}

/// The next tag after this one, skipping the reserved value on the wrap.
pub fn next_tag(tag: u16) -> u16 {
	let next = tag.wrapping_add(1);
	if next == NEVER_ISSUED { NEVER_ISSUED.wrapping_add(1) } else { next }
}

/// Whether the residue a device reported is consistent with what was asked for.
///
/// A RESIDUE LARGER THAN THE REQUEST IS NOT A SHORT ANSWER, it is a device describing a transfer that
/// did not happen - and a driver that subtracted it would compute a negative length as an enormous
/// positive one. The item asks for inconsistent residue to be rejected deterministically, and this is
/// where that is decided.
pub fn moved(requested: u32, residue: u32) -> Option<u32> {
	if residue > requested {
		return None;
	}
	Some(requested - residue)
}

#[cfg(test)]
mod tests;
