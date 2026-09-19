// THE SD PROTOCOL DECISIONS, WITH NO CONTROLLER BEHIND THEM.
//
// The item that owns this driver asks for the SD protocol core to be kept INDEPENDENT OF HOW THE
// CONTROLLER IS ATTACHED, so that a board's clock, reset, regulator and pinmux glue can sit outside
// the universal driver. This module is that core taken to its conclusion: everything here is a
// function from numbers a card or a controller reported to an answer, none of it touches MMIO, and
// none of it knows whether the controller is on PCI, in an ACPI node or in a device tree.
//
// It is also where an SD driver is most likely to be wrong. The CSD encodes capacity two entirely
// different ways depending on its version, and which addressing a card wants is one bit of a
// register that is easy to read past - both of those are silent when wrong, and both are tested.

// Controller registers, at the mapped BAR base.
pub const REG_SDMA: u64 = 0x00;
pub const REG_BLOCK_SIZE: u64 = 0x04; // 16-bit
pub const REG_BLOCK_COUNT: u64 = 0x06; // 16-bit
pub const REG_ARGUMENT: u64 = 0x08;
pub const REG_TRANSFER_MODE: u64 = 0x0C; // 16-bit
pub const REG_COMMAND: u64 = 0x0E; // 16-bit
pub const REG_RESPONSE: u64 = 0x10; // four 32-bit words
pub const REG_BUFFER: u64 = 0x20;
pub const REG_PRESENT_STATE: u64 = 0x24;
pub const REG_HOST_CONTROL: u64 = 0x28; // 8-bit
pub const REG_POWER_CONTROL: u64 = 0x29; // 8-bit
pub const REG_CLOCK_CONTROL: u64 = 0x2C; // 16-bit
pub const REG_TIMEOUT_CONTROL: u64 = 0x2E; // 8-bit
pub const REG_SOFTWARE_RESET: u64 = 0x2F; // 8-bit
pub const REG_INT_STATUS: u64 = 0x30;
pub const REG_INT_ENABLE: u64 = 0x34;
pub const REG_SIGNAL_ENABLE: u64 = 0x38;
pub const REG_ADMA_ERROR: u64 = 0x54; // 8-bit
pub const REG_ADMA_ADDRESS: u64 = 0x58; // 64-bit
pub const REG_CAPABILITIES: u64 = 0x40;

// `TRANSFER_MODE` bits.
pub const TRANSFER_DMA_ENABLE: u16 = 1 << 0;
pub const TRANSFER_BLOCK_COUNT_ENABLE: u16 = 1 << 1;
// Auto CMD12 is bits 3:2 = 01. WITHOUT IT A MULTI-BLOCK TRANSFER NEVER ENDS: the card keeps
// streaming until it is told to stop, and a driver that forgot has a card that answers nothing
// afterwards because it is still sending.
pub const TRANSFER_AUTO_CMD12: u16 = 1 << 2;
pub const TRANSFER_READ: u16 = 1 << 4;
pub const TRANSFER_MULTI_BLOCK: u16 = 1 << 5;

// `HOST_CONTROL` DMA select, bits 4:3. Zero is SDMA, which is not this.
pub const HOST_CONTROL_DMA_MASK: u8 = 0b11 << 3;
pub const HOST_CONTROL_DMA_ADMA2: u8 = 0b10 << 3;

// `CAPABILITIES` bit 19: the controller supports ADMA2. A driver that programmed the descriptor
// table on one that does not gets a command that never completes.
pub const CAPABILITY_ADMA2: u32 = 1 << 19;

// `PRESENT_STATE` bits.
pub const PRESENT_CMD_INHIBIT: u32 = 1 << 0;
pub const PRESENT_DAT_INHIBIT: u32 = 1 << 1;
pub const PRESENT_CARD_INSERTED: u32 = 1 << 16;
pub const PRESENT_WRITE_PROTECT: u32 = 1 << 19;
pub const PRESENT_BUFFER_WRITE_ENABLE: u32 = 1 << 10;
pub const PRESENT_BUFFER_READ_ENABLE: u32 = 1 << 11;

// `INT_STATUS` bits.
pub const INT_COMMAND_COMPLETE: u32 = 1 << 0;
pub const INT_TRANSFER_COMPLETE: u32 = 1 << 1;
pub const INT_BUFFER_WRITE_READY: u32 = 1 << 4;
pub const INT_BUFFER_READ_READY: u32 = 1 << 5;
pub const INT_CARD_INSERT: u32 = 1 << 6;
pub const INT_CARD_REMOVE: u32 = 1 << 7;
// Bit 15 is the error summary; the specific errors are in the upper half.
pub const INT_ERROR: u32 = 1 << 15;

// `SOFTWARE_RESET` bits.
pub const RESET_ALL: u8 = 1 << 0;
pub const RESET_CMD: u8 = 1 << 1;
pub const RESET_DAT: u8 = 1 << 2;

// `CLOCK_CONTROL` bits.
pub const CLOCK_INTERNAL_ENABLE: u16 = 1 << 0;
pub const CLOCK_INTERNAL_STABLE: u16 = 1 << 1;
pub const CLOCK_SD_ENABLE: u16 = 1 << 2;

// SD commands this driver sends.
pub const CMD_GO_IDLE: u8 = 0;
pub const CMD_ALL_SEND_CID: u8 = 2;
pub const CMD_SEND_RELATIVE_ADDR: u8 = 3;
pub const CMD_SELECT_CARD: u8 = 7;
pub const CMD_SEND_IF_COND: u8 = 8;
pub const CMD_SEND_CSD: u8 = 9;
pub const CMD_SET_BLOCKLEN: u8 = 16;
pub const CMD_READ_SINGLE_BLOCK: u8 = 17;
pub const CMD_READ_MULTIPLE_BLOCK: u8 = 18;
pub const CMD_WRITE_BLOCK: u8 = 24;
pub const CMD_WRITE_MULTIPLE_BLOCK: u8 = 25;
pub const CMD_APP_CMD: u8 = 55;
pub const ACMD_SD_SEND_OP_COND: u8 = 41;

/// The block size every command here works in. SD cards are addressed in 512-byte units whatever
/// their internal page size, and the block contract above is in 512-byte sectors too.
pub const BLOCK_BYTES: u32 = 512;

/// What kind of answer a command expects, which the controller has to be told before it sends one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Response {
	None,
	/// 48 bits, the ordinary case.
	Short,
	/// 48 bits WITH NO VALID CRC, which is a shape and not an oversight: the R3 answer that carries
	/// the OCR has its CRC field filled with ones by the specification, so a controller asked to
	/// check it reports a CRC error on a card that answered correctly. The card-identification
	/// sequence sends exactly one of these and it is the one every bring-up depends on.
	ShortNoCrc,
	/// 48 bits, and the controller must keep the data line busy until the card releases it.
	ShortBusy,
	/// 136 bits: CID and CSD come back this way.
	Long,
}

/// The `COMMAND` register value for one command.
///
/// THE RESPONSE SHAPE IS PART OF THE COMMAND and not a property of what comes back: the controller
/// clocks in a fixed number of bits and a driver that asked for the wrong length reads a CID as a
/// status word, or waits for bits the card is not sending.
pub fn command_word(index: u8, response: Response, has_data: bool) -> u16 {
	let kind: u16 = match response {
		Response::None => 0,
		Response::Short | Response::ShortNoCrc => 2,
		Response::ShortBusy => 3,
		Response::Long => 1,
	};
	// Bit 4 asks the controller to check the response's CRC, bit 3 its command index; neither is
	// meaningful for a command with no response, and a long response carries no index to check.
	let checks: u16 = match response {
		// Nothing comes back, and an R3 carries neither a valid CRC nor a command index.
		Response::None | Response::ShortNoCrc => 0,
		Response::Long => 1 << 3,
		Response::Short | Response::ShortBusy => (1 << 3) | (1 << 4),
	};
	let data: u16 = if has_data { 1 << 5 } else { 0 };
	((index as u16) << 8) | data | checks | kind
}

/// Why a command may not be sent yet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Busy {
	/// The command line is still carrying the previous command.
	Command,
	/// The data line is still carrying the previous transfer.
	Data,
}

/// Whether the controller can take a command now.
///
/// BOTH INHIBITS, AND THE DATA ONE ONLY WHEN IT MATTERS. The command inhibit clears when the command
/// line is free; the data inhibit stays set while a transfer is still running, and a command that
/// uses the data line must wait for it while one that does not may go. A driver that waited for both
/// every time serialises itself behind transfers it does not touch; one that waits for neither sends
/// a command the controller silently drops.
pub fn may_send(present: u32, uses_data: bool) -> Result<(), Busy> {
	if present & PRESENT_CMD_INHIBIT != 0 {
		return Err(Busy::Command);
	}
	if uses_data && present & PRESENT_DAT_INHIBIT != 0 {
		return Err(Busy::Data);
	}
	Ok(())
}

/// Whether a card is in the slot.
pub fn card_present(present: u32) -> bool {
	present & PRESENT_CARD_INSERTED != 0
}

/// Whether the card's write-protect switch is on. A read-only card is served READ-ONLY rather than
/// refused: the item asks for read-only cards to be covered, and a medium nobody can write is still
/// a medium somebody can read.
pub fn write_protected(present: u32) -> bool {
	present & PRESENT_WRITE_PROTECT == 0
}

/// How a command ended, from the interrupt-status register.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Completion {
	/// Nothing yet.
	Waiting,
	/// The bits asked for are set and no error is.
	Done,
	/// The error summary is set; the upper half of the register says which.
	Failed { errors: u16 },
}

/// Read a command's outcome. `wanted` is the set of status bits that mean this command finished.
///
/// THE ERROR BIT IS CHECKED FIRST AND ALWAYS. A controller that raises an error also raises the
/// completion bits for some commands, so a driver that tested for completion first calls a failed
/// transfer a finished one - and on a read that is a buffer of whatever was in memory, returned as
/// data.
pub fn completion(status: u32, wanted: u32) -> Completion {
	if status & INT_ERROR != 0 {
		return Completion::Failed { errors: (status >> 16) as u16 };
	}
	if status & wanted == wanted {
		return Completion::Done;
	}
	Completion::Waiting
}

/// The card's operating conditions, from the `ACMD41` answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ocr {
	/// Whether the card has finished its power-up sequence.
	pub ready: bool,
	/// Whether it is addressed in BLOCKS rather than in bytes.
	pub high_capacity: bool,
}

/// Read an OCR register.
///
/// THE ADDRESSING BIT IS THE ONE THAT MATTERS LATER AND IS ONLY VALID ONCE THE CARD IS READY. Bit 31
/// says the power-up is done and bit 30 says the card is high capacity; reading bit 30 before bit 31
/// is set reads a field the card has not filled in, and getting it wrong sends a BLOCK number to a
/// card addressed in BYTES - which reads the first 512 bytes of the medium for every request in the
/// system, successfully, for ever.
pub fn ocr(value: u32) -> Ocr {
	let ready = value & (1 << 31) != 0;
	Ocr { ready, high_capacity: ready && value & (1 << 30) != 0 }
}

/// One ADMA2 descriptor: two attribute bytes, two length bytes, four address bytes.
pub const ADMA_DESCRIPTOR_LEN: usize = 8;

// The attribute bits. `Act` is bits 5:4 - 00 is a no-op, 10 transfers, 11 links to another table.
pub const ADMA_VALID: u16 = 1 << 0;
pub const ADMA_END: u16 = 1 << 1;
pub const ADMA_ACT_TRAN: u16 = 0b10 << 4;

/// The most bytes ONE descriptor carries.
///
/// THE LENGTH FIELD IS SIXTEEN BITS AND ZERO MEANS 65536, which is the rule this bound exists to
/// avoid having to use: a driver writing a plain 65536 writes zero, and one writing zero for an
/// EMPTY span asks for the maximum. Splitting at 65024 - the largest multiple of a 512-byte block
/// below the field's range - keeps every length a number the field can hold literally, and keeps
/// each descriptor a whole number of blocks.
pub const ADMA_MAX_BYTES: u32 = 65024;

/// Why a span cannot be described as ADMA2 descriptors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Undescribable {
	/// Zero bytes is not a transfer.
	Empty,
	/// More descriptors than the table this driver allocates can hold.
	TooManyEntries,
	/// The span does not fit the 32-bit address the descriptor carries.
	AboveFourGigabytes,
}

/// How many descriptors a contiguous span of `bytes` needs.
pub fn adma_entries(bytes: u64, most: usize) -> Result<usize, Undescribable> {
	if bytes == 0 {
		return Err(Undescribable::Empty);
	}
	let entries = bytes.div_ceil(ADMA_MAX_BYTES as u64) as usize;
	if entries > most {
		return Err(Undescribable::TooManyEntries);
	}
	Ok(entries)
}

/// How many bytes the descriptor at `index` carries, out of a contiguous span of `bytes`.
pub fn adma_span(bytes: u64, index: usize) -> u32 {
	let taken = index as u64 * ADMA_MAX_BYTES as u64;
	(bytes.saturating_sub(taken)).min(ADMA_MAX_BYTES as u64) as u32
}

/// Build one descriptor, for a 32-bit address.
///
/// THE LAST ONE IS MARKED AND THE OTHERS ARE NOT. A table whose end is not marked is a controller
/// reading whatever follows it as another descriptor - which is the memory after this driver's
/// table, interpreted as addresses to transfer into.
pub fn adma_descriptor(address: u32, bytes: u32, last: bool) -> [u8; ADMA_DESCRIPTOR_LEN] {
	let attributes = ADMA_VALID | ADMA_ACT_TRAN | if last { ADMA_END } else { 0 };
	let length = (bytes & 0xFFFF) as u16;
	let mut out = [0u8; ADMA_DESCRIPTOR_LEN];
	out[0..2].copy_from_slice(&attributes.to_le_bytes());
	out[2..4].copy_from_slice(&length.to_le_bytes());
	out[4..8].copy_from_slice(&address.to_le_bytes());
	out
}

/// The transfer-mode word for a data transfer of `blocks` blocks.
///
/// AUTO CMD12 IS PART OF A MULTI-BLOCK TRANSFER AND NOT AN OPTION. A multi-block read streams until
/// the card is told to stop, so a driver that omits the stop has a card still sending when the next
/// command arrives - which reads as a controller that has stopped answering rather than as a missing
/// stop. A SINGLE-block transfer must NOT carry it: CMD12 after a single block is a stop for a
/// transmission that already ended, and the card reports an illegal command.
pub fn transfer_mode(blocks: u32, write: bool, dma: bool) -> u16 {
	let mut mode = TRANSFER_BLOCK_COUNT_ENABLE;
	if dma {
		mode |= TRANSFER_DMA_ENABLE;
	}
	if !write {
		mode |= TRANSFER_READ;
	}
	if blocks > 1 {
		mode |= TRANSFER_MULTI_BLOCK | TRANSFER_AUTO_CMD12;
	}
	mode
}

/// The address to put in a read or write command, for a card of this kind.
pub fn address_for(block: u64, high_capacity: bool) -> u64 {
	if high_capacity { block } else { block * BLOCK_BYTES as u64 }
}

/// Why a card is not served.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unservable {
	/// The CSD reports a structure version this driver does not parse. Refused rather than guessed
	/// at, because guessing means reporting a capacity that is wrong by orders of magnitude.
	UnknownCsdVersion,
	/// It reports no blocks.
	Empty,
}

/// A card this driver will serve.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Card {
	pub blocks: u64,
	pub block_bytes: u32,
}

/// Read the capacity out of a 128-bit CSD, given as its four 32-bit response words, most significant
/// first.
///
/// TWO ENCODINGS, AND WHICH ONE IS IN THE TOP TWO BITS. A version 1 card's size is a product of three
/// fields - `C_SIZE`, `C_SIZE_MULT` and the read block length - and a version 2 card's is one field
/// plus a constant, in half-megabyte units. They do not resemble each other, and a driver that
/// applied the wrong one reports a 32 GB card as two gigabytes or a 2 GB card as something absurd,
/// with no error anywhere.
pub fn csd_capacity(words: [u32; 4]) -> Result<Card, Unservable> {
	match words[0] >> 30 {
		0 => {
			// Version 1: C_SIZE is 12 bits spanning words 1 and 2, C_SIZE_MULT is 3 bits in word 2,
			// READ_BL_LEN is 4 bits in word 1. Capacity is
			// `(C_SIZE + 1) * 2^(C_SIZE_MULT + 2) * 2^READ_BL_LEN` bytes.
			let c_size = ((words[1] & 0x3FF) << 2) | (words[2] >> 30);
			let c_size_mult = (words[2] >> 15) & 0x07;
			let read_bl_len = (words[1] >> 16) & 0x0F;
			let blocks_on_card = (c_size as u64 + 1) << (c_size_mult + 2);
			let bytes = blocks_on_card << read_bl_len;
			let blocks = bytes / BLOCK_BYTES as u64;
			if blocks == 0 {
				return Err(Unservable::Empty);
			}
			Ok(Card { blocks, block_bytes: BLOCK_BYTES })
		}
		1 => {
			// Version 2: C_SIZE is 22 bits spanning words 1 and 2, and capacity is
			// `(C_SIZE + 1) * 512 KB`.
			let c_size = ((words[1] & 0x3F) << 16) | (words[2] >> 16);
			let blocks = (c_size as u64 + 1) * 1024;
			if blocks == 0 {
				return Err(Unservable::Empty);
			}
			Ok(Card { blocks, block_bytes: BLOCK_BYTES })
		}
		_ => Err(Unservable::UnknownCsdVersion),
	}
}

/// The divider to write into `CLOCK_CONTROL` for a target frequency, from the controller's base
/// clock. The field is the divisor over two, so a divider of `n` gives `base / (2n)`, and zero is
/// the base clock undivided.
///
/// ROUNDED SO THE RESULT IS NEVER FASTER THAN ASKED FOR. A clock above what a card negotiated is not
/// a card that runs slightly fast; it is a card that stops answering.
pub fn clock_divider(base_hz: u32, target_hz: u32) -> u16 {
	if target_hz == 0 || base_hz <= target_hz {
		return 0;
	}
	let mut divider: u32 = 1;
	while base_hz / (2 * divider) > target_hz && divider < 0x80 {
		divider += 1;
	}
	divider.min(0x80) as u16
}

/// The `CLOCK_CONTROL` value for a divider, in the eight-bit form every controller supports.
pub fn clock_control(divider: u16) -> u16 {
	((divider & 0xFF) << 8) | CLOCK_INTERNAL_ENABLE
}

#[cfg(test)]
mod tests;
