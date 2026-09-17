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
