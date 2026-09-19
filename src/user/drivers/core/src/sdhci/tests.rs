// EACH OF THESE WATCHES ONE WAY AN SD DRIVER IS WRONG, and the two that matter most are silent when
// they happen: a capacity read with the wrong CSD encoding, and a block number sent to a card that
// is addressed in bytes. Neither produces an error anywhere; the first reports a size off by orders
// of magnitude and the second reads the first sector of the medium for every request, for ever.

use super::*;

#[test]
fn a_version_one_card_and_a_version_two_card_are_sized_by_different_arithmetic() {
	// Version 2: C_SIZE 15271 means (15271 + 1) * 512 KB, which is a 8 GB card in 512-byte blocks.
	let v2 = [1u32 << 30, 15271 >> 16, (15271 & 0xFFFF) << 16, 0];
	assert_eq!(csd_capacity(v2), Ok(Card { blocks: 15272 * 1024, block_bytes: 512 }));

	// Version 1: C_SIZE 3751, C_SIZE_MULT 7, READ_BL_LEN 10 is the classic 2 GB encoding -
	// (3751 + 1) * 2^9 * 2^10 bytes.
	let c_size: u32 = 3751;
	let mut v1 = [0u32; 4];
	v1[1] = (10 << 16) | (c_size >> 2);
	v1[2] = ((c_size & 0x03) << 30) | (7 << 15);
	let card = csd_capacity(v1).expect("a version 1 card");
	assert_eq!(card.blocks, ((c_size as u64 + 1) << 9 << 10) / 512);
	assert_eq!(card.block_bytes, 512);
}

#[test]
fn a_csd_version_this_driver_does_not_parse_is_refused_rather_than_guessed_at() {
	// Guessing means reporting a capacity that is wrong by orders of magnitude, with no error.
	assert_eq!(csd_capacity([2 << 30, 0, 0, 0]), Err(Unservable::UnknownCsdVersion));
	assert_eq!(csd_capacity([3 << 30, 0, 0, 0]), Err(Unservable::UnknownCsdVersion));
}

#[test]
fn a_card_reporting_no_blocks_is_refused() {
	// A version 1 CSD of all zeroes is one block of one byte, which rounds to no blocks at all.
	assert_eq!(csd_capacity([0, 0, 0, 0]), Err(Unservable::Empty));
}

#[test]
fn the_addressing_bit_decides_whether_a_block_number_or_a_byte_offset_is_sent() {
	// A block number sent to a byte-addressed card names byte 7 - inside the first sector - so every
	// request in the system reads sector zero and reports success.
	assert_eq!(address_for(7, true), 7);
	assert_eq!(address_for(7, false), 7 * 512);
	assert_eq!(address_for(0, false), 0, "and the first block is the same either way");
}

#[test]
fn the_capacity_bit_is_not_read_before_the_card_says_it_is_ready() {
	// Bit 30 is a field the card has not filled in until bit 31 is set, and reading it early is how
	// a high-capacity card is driven as a standard one.
	assert_eq!(ocr(0), Ocr { ready: false, high_capacity: false });
	assert_eq!(ocr(1 << 30), Ocr { ready: false, high_capacity: false }, "not ready, so not read");
	assert_eq!(ocr(1 << 31), Ocr { ready: true, high_capacity: false });
	assert_eq!(ocr((1 << 31) | (1 << 30)), Ocr { ready: true, high_capacity: true });
}

#[test]
fn a_command_waits_for_the_data_line_only_when_it_uses_it() {
	// Waiting for both every time serialises a driver behind transfers it does not touch; waiting
	// for neither sends a command the controller drops.
	assert_eq!(may_send(0, false), Ok(()));
	assert_eq!(may_send(PRESENT_DAT_INHIBIT, false), Ok(()), "a command with no data may go");
	assert_eq!(may_send(PRESENT_DAT_INHIBIT, true), Err(Busy::Data));
	assert_eq!(may_send(PRESENT_CMD_INHIBIT, false), Err(Busy::Command));
	// The command inhibit stops everything, data or not.
	assert_eq!(may_send(PRESENT_CMD_INHIBIT | PRESENT_DAT_INHIBIT, true), Err(Busy::Command));
}

#[test]
fn an_error_is_read_before_a_completion_and_not_after_it() {
	// A controller raising an error also raises the completion bits for some commands, so testing
	// for completion first calls a failed read finished - and returns whatever was in the buffer.
	assert_eq!(completion(0, INT_COMMAND_COMPLETE), Completion::Waiting);
	assert_eq!(completion(INT_COMMAND_COMPLETE, INT_COMMAND_COMPLETE), Completion::Done);
	assert_eq!(completion(INT_ERROR | (0x0008 << 16), INT_COMMAND_COMPLETE), Completion::Failed { errors: 0x0008 });
	assert_eq!(completion(INT_ERROR | INT_COMMAND_COMPLETE | (0x0001 << 16), INT_COMMAND_COMPLETE), Completion::Failed { errors: 0x0001 }, "set together, the error wins");
	// Every wanted bit, not any of them.
	let both = INT_COMMAND_COMPLETE | INT_TRANSFER_COMPLETE;
	assert_eq!(completion(INT_COMMAND_COMPLETE, both), Completion::Waiting);
	assert_eq!(completion(both, both), Completion::Done);
}

#[test]
fn a_commands_word_carries_its_index_its_response_shape_and_whether_it_moves_data() {
	// The controller clocks in a fixed number of bits, so asking for the wrong shape reads a CID as
	// a status word or waits for bits the card is not sending.
	let read = command_word(CMD_READ_SINGLE_BLOCK, Response::Short, true);
	assert_eq!(read >> 8, CMD_READ_SINGLE_BLOCK as u16, "the index is in the top byte");
	assert_eq!(read & 0x03, 2, "a short response");
	assert_ne!(read & (1 << 5), 0, "and it moves data");

	let idle = command_word(CMD_GO_IDLE, Response::None, false);
	assert_eq!(idle & 0x03, 0);
	assert_eq!(idle & ((1 << 3) | (1 << 4)), 0, "nothing to check in a response that does not come");
	assert_eq!(idle & (1 << 5), 0);

	// A long response carries no command index, so only its CRC is checked.
	let csd = command_word(CMD_SEND_CSD, Response::Long, false);
	assert_eq!(csd & 0x03, 1);
	assert_ne!(csd & (1 << 3), 0);
	assert_eq!(csd & (1 << 4), 0, "no index to check in a 136-bit answer");

	// A busy response is its own shape, not a short one with a flag.
	assert_eq!(command_word(CMD_SELECT_CARD, Response::ShortBusy, false) & 0x03, 3);
}

#[test]
fn the_clock_divider_never_rounds_upwards() {
	// A clock above what the card negotiated is not a card running slightly fast; it is a card that
	// stops answering.
	assert_eq!(clock_divider(50_000_000, 400_000), 63, "400 kHz identification from a 50 MHz base");
	assert!(50_000_000 / (2 * 63) <= 400_000, "and the result is at or below what was asked for");
	assert_eq!(clock_divider(50_000_000, 50_000_000), 0, "the base clock undivided");
	assert_eq!(clock_divider(50_000_000, 60_000_000), 0, "and never faster than the base");
	assert_eq!(clock_divider(50_000_000, 0), 0, "a target of zero is not a division by zero");
}

#[test]
fn the_clock_control_word_puts_the_divider_where_the_register_wants_it() {
	let value = clock_control(63);
	assert_eq!(value >> 8, 63);
	assert_ne!(value & CLOCK_INTERNAL_ENABLE, 0, "and asks for the internal clock");
}

#[test]
fn a_card_and_its_write_protect_switch_are_read_from_the_present_state() {
	assert!(!card_present(0));
	assert!(card_present(PRESENT_CARD_INSERTED));
	// The write-protect line is ACTIVE LOW: the bit set means writable, so a driver reading it the
	// obvious way round makes every ordinary card read-only.
	assert!(write_protected(0), "the line low is a protected card");
	assert!(!write_protected(PRESENT_WRITE_PROTECT), "and the line high is a writable one");
}

#[test]
fn the_ocr_answer_is_asked_for_without_a_crc_check() {
	// R3 has its CRC field filled with ones by the specification, so a controller asked to check it
	// reports a CRC error on a card that answered correctly - and the identification sequence sends
	// exactly one of these, which every bring-up depends on.
	let opcond = command_word(ACMD_SD_SEND_OP_COND, Response::ShortNoCrc, false);
	assert_eq!(opcond & 0x03, 2, "still a 48-bit answer");
	assert_eq!(opcond & ((1 << 3) | (1 << 4)), 0, "and nothing about it is checked");
	// The ordinary short response is unchanged and still checked both ways.
	assert_eq!(command_word(CMD_SEND_IF_COND, Response::Short, false) & ((1 << 3) | (1 << 4)), (1 << 3) | (1 << 4));
}

// ------------------------------------------------------------------ a fake controller's sequence
//
// AN SD COMMAND IS TWO EVENTS AND NOT ONE. The command completes, and then - for a command that moves
// data - the transfer does, with the buffer becoming ready somewhere in between. A driver that waited
// for the wrong one of those either reads a buffer the controller has not filled or reports success
// while the card is still writing, and neither says anything at the time.
struct FakeSlot {
	status: u32,
	// Whether the controller is still carrying a command or a transfer, which is what the inhibits
	// report.
	present: u32,
}

impl FakeSlot {
	fn new() -> FakeSlot {
		FakeSlot { status: 0, present: PRESENT_CARD_INSERTED | PRESENT_WRITE_PROTECT }
	}

	fn issue(&mut self, uses_data: bool) {
		self.status = 0;
		self.present |= PRESENT_CMD_INHIBIT;
		if uses_data {
			self.present |= PRESENT_DAT_INHIBIT;
		}
	}

	fn command_done(&mut self) {
		self.present &= !PRESENT_CMD_INHIBIT;
		self.status |= INT_COMMAND_COMPLETE;
	}

	fn buffer_ready(&mut self, write: bool) {
		self.status |= if write { INT_BUFFER_WRITE_READY } else { INT_BUFFER_READ_READY };
	}

	fn transfer_done(&mut self) {
		self.present &= !PRESENT_DAT_INHIBIT;
		self.status |= INT_TRANSFER_COMPLETE;
	}

	fn fail(&mut self, errors: u16) {
		self.present &= !(PRESENT_CMD_INHIBIT | PRESENT_DAT_INHIBIT);
		self.status |= INT_ERROR | ((errors as u32) << 16);
	}
}

#[test]
fn a_read_is_not_finished_when_its_buffer_becomes_ready() {
	// The buffer being readable and the transfer being over are different moments, and reporting
	// success at the first lets the next command start while the card is still working.
	let mut slot = FakeSlot::new();
	slot.issue(true);
	let wanted = INT_COMMAND_COMPLETE | INT_BUFFER_READ_READY;
	assert_eq!(completion(slot.status, wanted), Completion::Waiting);
	slot.command_done();
	assert_eq!(completion(slot.status, wanted), Completion::Waiting, "the command is done and the buffer is not");
	slot.buffer_ready(false);
	assert_eq!(completion(slot.status, wanted), Completion::Done, "now the driver may drain the port");
	assert_eq!(completion(slot.status, INT_TRANSFER_COMPLETE), Completion::Waiting, "but the transfer is still running");
	slot.transfer_done();
	assert_eq!(completion(slot.status, INT_TRANSFER_COMPLETE), Completion::Done);
}

#[test]
fn a_second_command_waits_for_the_line_it_actually_uses() {
	// A command with no data may go while a transfer is still running; one that moves data may not.
	// Waiting for both every time serialises the driver behind work it does not touch.
	let mut slot = FakeSlot::new();
	slot.issue(true);
	slot.command_done();
	assert_eq!(may_send(slot.present, false), Ok(()), "a command with no data may follow");
	assert_eq!(may_send(slot.present, true), Err(Busy::Data), "one that moves data may not");
	slot.transfer_done();
	assert_eq!(may_send(slot.present, true), Ok(()));
}

#[test]
fn an_error_ends_the_wait_even_though_the_completion_bits_never_arrive() {
	// A driver that only tested for its wanted bits would spin out its whole timeout on a command
	// the controller has already refused, and then report a timeout rather than the error.
	let mut slot = FakeSlot::new();
	slot.issue(true);
	slot.fail(0x0010); // a command timeout error
	assert_eq!(completion(slot.status, INT_COMMAND_COMPLETE | INT_BUFFER_READ_READY), Completion::Failed { errors: 0x0010 });
	assert_eq!(may_send(slot.present, true), Ok(()), "and the controller has released both lines");
}

#[test]
fn an_error_arriving_together_with_a_completion_is_still_an_error() {
	// Some controllers raise both. Testing for completion first calls a failed transfer finished and
	// returns whatever was in the buffer.
	let mut slot = FakeSlot::new();
	slot.issue(true);
	slot.command_done();
	slot.buffer_ready(false);
	slot.fail(0x0002);
	assert_eq!(completion(slot.status, INT_COMMAND_COMPLETE | INT_BUFFER_READ_READY), Completion::Failed { errors: 0x0002 });
}

#[test]
fn a_write_to_a_protected_card_is_refused_before_the_command_is_built() {
	// The card is present and readable; only the write is refused. A driver that treated the switch
	// as a broken medium would stop serving reads it can perfectly well serve.
	let mut slot = FakeSlot::new();
	assert!(card_present(slot.present));
	assert!(!write_protected(slot.present), "the switch is off in this fixture");
	slot.present &= !PRESENT_WRITE_PROTECT;
	assert!(card_present(slot.present), "still a card");
	assert!(write_protected(slot.present), "and now a protected one");
}

#[test]
fn a_descriptor_carries_its_length_literally_and_marks_the_end() {
	let one = adma_descriptor(0x1234_5678, 512, false);
	assert_eq!(u16::from_le_bytes([one[0], one[1]]), ADMA_VALID | ADMA_ACT_TRAN, "valid and a transfer, and not the end");
	assert_eq!(u16::from_le_bytes([one[2], one[3]]), 512, "the length is the length");
	assert_eq!(u32::from_le_bytes([one[4], one[5], one[6], one[7]]), 0x1234_5678);

	let last = adma_descriptor(0, 512, true);
	assert_eq!(u16::from_le_bytes([last[0], last[1]]) & ADMA_END, ADMA_END, "the last one says so, or the controller reads what follows the table as another descriptor");

	// AND THE BOUND KEEPS EVERY LENGTH LITERAL. The field is sixteen bits and ZERO means 65536, so
	// a descriptor carrying the full range would have to be written as zero - which is also how an
	// empty one is written. The bound is below the range for exactly that reason.
	assert!(ADMA_MAX_BYTES < 65536);
	assert_eq!(ADMA_MAX_BYTES % BLOCK_BYTES, 0, "and it is a whole number of blocks");
	let full = adma_descriptor(0, ADMA_MAX_BYTES, true);
	assert_ne!(u16::from_le_bytes([full[2], full[3]]), 0, "the largest span this driver asks for is not written as zero");
}

#[test]
fn a_span_is_split_into_as_many_descriptors_as_it_needs() {
	assert_eq!(adma_entries(512, 8), Ok(1));
	assert_eq!(adma_entries(ADMA_MAX_BYTES as u64, 8), Ok(1), "exactly one descriptor's worth is one descriptor");
	assert_eq!(adma_entries(ADMA_MAX_BYTES as u64 + 512, 8), Ok(2), "and one block more is two");
	assert_eq!(adma_entries(0, 8), Err(Undescribable::Empty));
	assert_eq!(adma_entries(ADMA_MAX_BYTES as u64 * 9, 8), Err(Undescribable::TooManyEntries), "a span past the table is refused rather than truncated");

	// THE SPANS SUM TO THE WHOLE, which is what says nothing is dropped at the split.
	let bytes = ADMA_MAX_BYTES as u64 + 1024;
	let entries = adma_entries(bytes, 8).unwrap();
	let total: u64 = (0..entries).map(|index| adma_span(bytes, index) as u64).sum();
	assert_eq!(total, bytes);
	assert_eq!(adma_span(bytes, 0), ADMA_MAX_BYTES);
	assert_eq!(adma_span(bytes, 1), 1024);
	assert_eq!(adma_span(bytes, 2), 0, "past the end is nothing, not a wrap");
}

#[test]
fn a_single_block_transfer_does_not_carry_the_stop_a_multi_block_one_needs() {
	// AUTO CMD12 AFTER ONE BLOCK IS A STOP FOR A TRANSMISSION THAT ALREADY ENDED, and the card
	// reports an illegal command - so this is not "set it always and be safe".
	let single = transfer_mode(1, false, true);
	assert_eq!(single & TRANSFER_AUTO_CMD12, 0);
	assert_eq!(single & TRANSFER_MULTI_BLOCK, 0);
	assert_eq!(single & TRANSFER_READ, TRANSFER_READ);
	assert_eq!(single & TRANSFER_DMA_ENABLE, TRANSFER_DMA_ENABLE);
	assert_eq!(single & TRANSFER_BLOCK_COUNT_ENABLE, TRANSFER_BLOCK_COUNT_ENABLE);

	// AND WITHOUT IT A MULTI-BLOCK READ NEVER ENDS.
	let many = transfer_mode(8, false, true);
	assert_eq!(many & TRANSFER_AUTO_CMD12, TRANSFER_AUTO_CMD12);
	assert_eq!(many & TRANSFER_MULTI_BLOCK, TRANSFER_MULTI_BLOCK);

	// A write is the same word without the direction bit.
	assert_eq!(transfer_mode(8, true, true) & TRANSFER_READ, 0);
	// And the PIO path asks for no DMA.
	assert_eq!(transfer_mode(1, false, false) & TRANSFER_DMA_ENABLE, 0);
}

// A UHS-I MODE IS OFFERED OR IT IS NOT, AND EACH BIT SAYS SO ON ITS OWN.
//
// The reason this is a decision rather than a comparison at the call site: the three bits are three
// DIFFERENT modes and any one of them means a faster mode exists to negotiate. A driver that
// checked only SDR50 would report "no UHS" on a controller offering SDR104, which is the faster of
// the two - and the report is the only thing distinguishing a controller with nothing to offer from
// a driver that never asks.
#[test]
fn uhs_is_offered_when_any_mode_is_advertised_and_not_when_none_is() {
	assert!(!uhs_offered(0));
	// Each mode alone is enough.
	assert!(uhs_offered(UHS_SDR50));
	assert!(uhs_offered(UHS_SDR104));
	assert!(uhs_offered(UHS_DDR50));
	// And bits that are not these three are not UHS: a word full of other capabilities offers none.
	assert!(!uhs_offered(!(UHS_SDR50 | UHS_SDR104 | UHS_DDR50)));
}
