// THE SCSI COMMAND AND SENSE CORE, WITH NO TRANSPORT BEHIND IT.
//
// SCSI is one command set carried over several transports: this tree reaches it over USB Bulk-Only
// Transport today and over virtio-scsi as of this module, and UAS would be a third. The roadmap's
// rule is that whichever of them is implemented first OWNS this core and the others consume it - and
// that the existing one MOVES ONTO IT IN THE SAME CHANGE, because an extraction that leaves the
// original behind has produced a second copy rather than a shared one.
//
// THAT RULE IS NOT A STYLE PREFERENCE HERE. The block wire had three hand-written copies, and
// extracting it found that two of its servers had drifted into answering a refused request
// differently - so on one of them a caller could not tell a request it got wrong from a device that
// failed one it got right. Nobody had noticed, because nothing compared them.
//
// Everything below is a function from bytes to an answer. None of it touches a transport, which is
// what lets a host test watch each refusal happen.

/// The block size this tree's block contract carries. A medium with any other is refused rather than
/// served at the wrong scale.
pub const BLOCK_BYTES: u32 = 512;

/// A ten-byte command descriptor block: the length every command here uses.
pub const CDB10_LEN: usize = 10;
/// A six-byte one, for the commands that predate the longer form.
pub const CDB6_LEN: usize = 6;
/// A twelve-byte one, which is the length `REPORT LUNS` is defined at.
pub const CDB12_LEN: usize = 12;
/// Fixed-format sense data is eighteen bytes, and the descriptor format is longer; a driver asks for
/// this many and is content with fewer.
pub const SENSE_LEN: usize = 18;

/// Operation codes.
pub const TEST_UNIT_READY: u8 = 0x00;
pub const REQUEST_SENSE: u8 = 0x03;
pub const INQUIRY: u8 = 0x12;
pub const READ_CAPACITY10: u8 = 0x25;
pub const READ10: u8 = 0x28;
pub const WRITE10: u8 = 0x2A;
pub const SYNCHRONIZE_CACHE10: u8 = 0x35;
pub const REPORT_LUNS: u8 = 0xA0;

/// A `TEST UNIT READY`, which carries nothing.
pub fn test_unit_ready() -> [u8; CDB6_LEN] {
	[TEST_UNIT_READY, 0, 0, 0, 0, 0]
}

/// A `REQUEST SENSE` for `len` bytes. The allocation length is one byte, so a caller asking for more
/// than it holds would ask for a number the field cannot express.
pub fn request_sense(len: u8) -> [u8; CDB6_LEN] {
	[REQUEST_SENSE, 0, 0, 0, len, 0]
}

/// A `READ CAPACITY (10)`, which carries nothing this driver sets.
pub fn read_capacity10() -> [u8; CDB10_LEN] {
	[READ_CAPACITY10, 0, 0, 0, 0, 0, 0, 0, 0, 0]
}

/// A `SYNCHRONIZE CACHE (10)` over the whole medium: block zero, count zero, which in this command
/// means everything rather than nothing.
pub fn synchronize_cache10() -> [u8; CDB10_LEN] {
	[SYNCHRONIZE_CACHE10, 0, 0, 0, 0, 0, 0, 0, 0, 0]
}

/// A `READ (10)` or `WRITE (10)`.
///
/// BIG-ENDIAN, WHICH IS THE OPPOSITE OF EVERY OTHER WIRE IN THIS TREE. SCSI is network byte order
/// throughout, and a driver that wrote the address the way the block wire carries it would read from
/// a block number with its bytes reversed - a plausible address, on the same medium, that nothing
/// would refuse.
///
/// The caller is expected to have admitted the request first: `drivers::blk::request_range` bounds
/// the count against the medium and `command_lba32` refuses an address this ten-byte form cannot
/// carry. This builds what it is given.
pub fn read_write10(write: bool, lba: u32, blocks: u16) -> [u8; CDB10_LEN] {
	let mut cdb = [0u8; CDB10_LEN];
	cdb[0] = if write { WRITE10 } else { READ10 };
	cdb[2] = (lba >> 24) as u8;
	cdb[3] = (lba >> 16) as u8;
	cdb[4] = (lba >> 8) as u8;
	cdb[5] = lba as u8;
	cdb[7] = (blocks >> 8) as u8;
	cdb[8] = blocks as u8;
	cdb
}

/// A `REPORT LUNS` asking for `len` bytes of list.
///
/// SELECT REPORT 0x00, which is the units addressable through this nexus and not the well-known ones.
/// The allocation length is a THIRTY-TWO-BIT field in the middle of the command rather than the one
/// or two bytes every other command here uses, and it counts BYTES of answer, not units.
pub fn report_luns(len: u32) -> [u8; CDB12_LEN] {
	let mut cdb = [0u8; CDB12_LEN];
	cdb[0] = REPORT_LUNS;
	cdb[6] = (len >> 24) as u8;
	cdb[7] = (len >> 16) as u8;
	cdb[8] = (len >> 8) as u8;
	cdb[9] = len as u8;
	cdb
}

/// The logical unit number one eight-byte addressing field names, or `None` when this core will not
/// address it.
///
/// THE TOP TWO BITS CHOOSE HOW THE REST IS READ, and a driver that skips them reads a different
/// number off the same eight bytes. Peripheral addressing puts the unit in the SECOND byte; flat
/// space puts it across six bits of the first and all of the second - so a flat unit 0x0101 read as
/// peripheral is unit 1, which exists on most targets and answers. The two forms are told apart
/// only here.
///
/// LEVELS BEYOND THE FIRST ARE REFUSED RATHER THAN IGNORED. The field addresses a hierarchy four
/// levels deep, and a device that answers with a second level is naming a unit BEHIND the one the
/// first two bytes name. Reading the first level and dropping the rest addresses the wrong unit with
/// a request that succeeds.
pub fn lun_number(field: &[u8]) -> Option<u16> {
	if field.len() < 8 || field[2..8].iter().any(|byte| *byte != 0) {
		return None;
	}
	match field[0] >> 6 {
		// Peripheral device addressing. The low six bits are the BUS, and this core addresses bus
		// zero only: a unit behind another bus is reached through that bus's own nexus.
		0b00 if field[0] & 0x3F == 0 => Some(field[1] as u16),
		// Flat space addressing.
		0b01 => Some((((field[0] & 0x3F) as u16) << 8) | field[1] as u16),
		// Logical-unit addressing and the extended form, which includes the WELL-KNOWN units. None
		// of them is a medium to serve blocks from.
		_ => None,
	}
}

/// Decode a `REPORT LUNS` answer into logical unit numbers, writing at most `out.len()` of them and
/// answering how many were written.
///
/// THE FIRST FIELD COUNTS BYTES AND NOT UNITS, which is the mistake this function exists to make
/// impossible: read as a count, a target with four units reports four BYTES of list and the driver
/// serves none of them.
///
/// AND THE FIELD IS THE DEVICE'S CLAIM, NOT THE BUFFER'S LENGTH. A target may answer with a list
/// longer than the allocation it was given - that is how it says "ask again with more" - so the
/// claim is clamped to what actually arrived. Trusting it reads past the answer.
pub fn luns(answer: &[u8], out: &mut [u16]) -> usize {
	if answer.len() < 8 || out.is_empty() {
		return 0;
	}
	let claimed = u32::from_be_bytes([answer[0], answer[1], answer[2], answer[3]]) as usize;
	let arrived = answer.len() - 8;
	// Rounded DOWN to whole entries: a truncated last entry is half an address.
	let usable = claimed.min(arrived) / 8;
	let mut written = 0usize;
	for index in 0..usable {
		if written == out.len() {
			break;
		}
		let at = 8 + index * 8;
		let Some(lun) = lun_number(&answer[at..at + 8]) else { continue };
		// A TARGET MAY LIST THE SAME UNIT TWICE, and a driver that published a provider per entry
		// would publish two for one medium - two block devices over one disk, each able to write
		// under the other.
		if out[..written].contains(&lun) {
			continue;
		}
		out[written] = lun;
		written += 1;
	}
	written
}

/// What a unit answered about its size.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capacity {
	pub blocks: u64,
	pub block_bytes: u32,
}

/// Why a unit is not served.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unservable {
	/// The answer is too short to hold a capacity.
	Short,
	/// Its block size is not the one the block contract carries.
	BlockSize,
	/// It reports no blocks.
	Empty,
}

/// Read a `READ CAPACITY (10)` answer: eight bytes, both fields big-endian.
///
/// THE FIRST FIELD IS THE LAST BLOCK AND NOT THE COUNT, which is the off-by-one that makes a driver
/// read one block past the end of every medium it serves - and the block past the end is the one the
/// medium refuses, so it surfaces as an I/O error on the last sector of every disk rather than as
/// anything that names this line.
pub fn capacity(answer: &[u8]) -> Result<Capacity, Unservable> {
	if answer.len() < 8 {
		return Err(Unservable::Short);
	}
	let last = u32::from_be_bytes([answer[0], answer[1], answer[2], answer[3]]);
	let block_bytes = u32::from_be_bytes([answer[4], answer[5], answer[6], answer[7]]);
	if block_bytes != BLOCK_BYTES {
		return Err(Unservable::BlockSize);
	}
	// A unit with one block reports a last block of zero, so an answer of zero is a medium of one
	// block rather than an empty one. `0xFFFFFFFF` is the value that says "ask with the sixteen-byte
	// form", which this driver does not send.
	if last == u32::MAX {
		return Err(Unservable::Short);
	}
	Ok(Capacity { blocks: last as u64 + 1, block_bytes })
}

/// What sense data says happened, reduced to what a caller can act on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sense {
	/// Nothing is wrong: the command that asked was not failing.
	None,
	/// The unit has just powered on, or the medium changed. Its first command after either is
	/// refused ONCE and the refusal clears by being read, which is why a bring-up retries.
	AttentionCleared,
	/// The unit is not ready YET - spinning up, or becoming ready. Worth waiting for.
	NotReadyYet,
	/// The unit is not ready and says nothing about becoming ready. Not worth waiting for.
	NotReady,
	/// The medium or the hardware failed this command.
	Failed,
	/// The command or its parameters were refused. Retrying sends the same thing again.
	Refused,
	/// A key this core does not model.
	Other { key: u8 },
}

impl Sense {
	/// Whether sending the same command again could reasonably answer differently.
	pub fn retryable(self) -> bool {
		matches!(self, Sense::AttentionCleared | Sense::NotReadyYet)
	}
}

/// Decode fixed-format sense data.
///
/// THE KEY ALONE IS NOT THE ANSWER, and this is the line a driver most often gets wrong: `NOT READY`
/// covers both a unit that is spinning up and one that has no medium at all, and the two are told
/// apart only by the additional code and its qualifier. A driver reading the key alone either
/// retries a permanent failure for ever or gives up on a unit that was a second from ready.
pub fn sense(data: &[u8]) -> Sense {
	// Byte 0's low seven bits are the response code; 0x70 and 0x71 are the fixed formats. Anything
	// else is not sense data this core reads.
	if data.len() < 14 || (data[0] & 0x7F) != 0x70 && (data[0] & 0x7F) != 0x71 {
		return Sense::Other { key: 0xFF };
	}
	let key = data[2] & 0x0F;
	let asc = data[12];
	let ascq = data[13];
	match key {
		0x00 => Sense::None,
		0x02 => match (asc, ascq) {
			// "Logical unit is in process of becoming ready" and "in process of becoming ready" -
			// both worth waiting for.
			(0x04, 0x01) | (0x04, 0x02) | (0x04, 0x03) => Sense::NotReadyYet,
			_ => Sense::NotReady,
		},
		0x03 | 0x04 => Sense::Failed,
		0x05 => Sense::Refused,
		0x06 => Sense::AttentionCleared,
		other => Sense::Other { key: other },
	}
}

/// The virtio-scsi request header, as the device reads it.
///
/// `lun` is the eight-byte addressing field, and its shape is the trap: the FIRST byte is a fixed
/// one, the SECOND is the target, and the LUN itself sits in the two after that with its own
/// encoding. A driver that wrote the target into byte zero addresses nothing, and the device answers
/// with a "bad target" that looks exactly like an empty bus.
pub fn virtio_lun(target: u8, lun: u16) -> [u8; 8] {
	let mut out = [0u8; 8];
	out[0] = 1;
	out[1] = target;
	out[2] = 0x40 | ((lun >> 8) as u8 & 0x3F);
	out[3] = lun as u8;
	out
}

/// What the device reported on the virtio-scsi EVENT queue.
///
/// The event carries the addressing field of the unit it is about, so a driver acts on ONE unit
/// rather than rebuilding everything it holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
	/// The buffer came back with nothing in it.
	None,
	/// A unit appeared, or one that was there should be looked at again.
	Rescan,
	/// A unit went away. What was published for it no longer addresses anything.
	Removed,
	/// The unit reset itself: everything a driver had established with it is gone.
	HardReset,
	/// Something about the unit changed - its capacity, its medium - without it going away.
	ParamChange,
	/// An event this core does not model.
	Other,
}

/// The virtio-scsi event buffer: an event word, the addressing field, and a reason word.
pub const EVENT_LEN: usize = 16;

/// Decode one event buffer into what happened, the unit it happened to, and whether the device had
/// to DROP events before this one.
///
/// THE TOP BIT OF THE EVENT WORD IS NOT PART OF THE EVENT. `VIRTIO_SCSI_T_EVENTS_MISSED` is OR'd in
/// when the device ran out of buffers and threw events away, so a driver comparing the whole word
/// against the event numbers stops recognising events exactly when it has already missed some - the
/// one moment its picture of the bus is known to be stale. It reads as a quiet driver on a busy bus.
///
/// AND "MISSED" IS RETURNED RATHER THAN SWALLOWED, because the only correct answer to it is a full
/// re-enumeration: the events that say what changed are the ones that were dropped.
pub fn virtio_event(buffer: &[u8]) -> Option<(Event, [u8; 8], bool)> {
	if buffer.len() < EVENT_LEN {
		return None;
	}
	let word = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]);
	let missed = word & 0x8000_0000 != 0;
	let reason = u32::from_le_bytes([buffer[12], buffer[13], buffer[14], buffer[15]]);
	let mut lun = [0u8; 8];
	lun.copy_from_slice(&buffer[4..12]);
	let event = match word & 0x7FFF_FFFF {
		0 => Event::None,
		// A TRANSPORT RESET, and the REASON is what it is about: the same event number covers a unit
		// arriving, a unit leaving and a unit resetting itself, and acting on the number alone
		// treats a removal as an arrival.
		1 => match reason {
			1 => Event::Rescan,
			2 => Event::Removed,
			3 => Event::HardReset,
			_ => Event::Other,
		},
		3 => Event::ParamChange,
		_ => Event::Other,
	};
	Some((event, lun, missed))
}

/// What a virtio-scsi response's status byte and response code mean together.
///
/// TWO FIELDS AND NOT ONE. The response code says whether the DEVICE carried the request, and the
/// status says what the TARGET thought of it; a driver reading only the status calls a request the
/// device never delivered a success, because the target never set anything.
pub fn virtio_outcome(response: u8, status: u8) -> Result<(), Sense> {
	if response != 0 {
		return Err(Sense::Failed);
	}
	match status {
		0x00 => Ok(()),
		// CHECK CONDITION: the sense buffer alongside says what happened.
		0x02 => Err(Sense::Other { key: 0xFE }),
		_ => Err(Sense::Failed),
	}
}

#[cfg(test)]
mod tests;
