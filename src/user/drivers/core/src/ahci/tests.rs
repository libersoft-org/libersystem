// EACH OF THESE WATCHES ONE WAY AN AHCI DRIVER IS WRONG, and every one is a mistake with a
// plausible-looking driver on the other side: a port walked because the controller said it has six,
// a command believed because its slot cleared, an ATAPI device driven with ATA reads, a disk sized
// modulo 128 gibibytes, and a scatter-gather entry one byte too long.

use super::*;

#[test]
fn the_port_bitmap_is_not_a_port_count() {
	// `CAP.NP` says how many ports the controller has; `PI` says WHICH exist, and they disagree on
	// real hardware. Walking `0..NP` reads the registers of ports that are not there.
	let ports: alloc::vec::Vec<u32> = implemented_ports(0b0001_0011, 6).collect();
	assert_eq!(ports, alloc::vec![0, 1, 4]);
	assert_eq!(implemented_ports(0, 6).count(), 0, "no bits set is no ports");
	// A bitmap claiming ports past what the controller reports is not followed past the bound.
	let bounded: alloc::vec::Vec<u32> = implemented_ports(0xFFFF_FFFF, 3).collect();
	assert_eq!(bounded, alloc::vec![0, 1, 2]);
}

#[test]
fn a_ports_registers_are_where_the_specification_puts_them() {
	assert_eq!(port_offset(0), 0x100);
	assert_eq!(port_offset(1), 0x180);
	assert_eq!(port_offset(4), 0x300);
}

#[test]
fn the_slot_and_port_counts_are_one_more_than_the_register_says() {
	// Both are zero-based on the wire. A driver using the raw fields builds one slot too few and
	// walks one port too few.
	let caps = Capabilities::decode((0x1F << 8) | 0x05 | (1 << 31));
	assert_eq!(caps.slots, 32);
	assert_eq!(caps.ports, 6);
	assert!(caps.sixty_four_bit);
	let small = Capabilities::decode(0);
	assert_eq!(small.slots, 1, "zero slots would be a command list with nothing in it");
	assert_eq!(small.ports, 1);
	assert!(!small.sixty_four_bit);
}

#[test]
fn a_controller_with_no_ports_or_no_64_bit_addressing_is_refused() {
	let caps = Capabilities::decode(0x1F << 8);
	assert_eq!(caps.usable(0, false), Err(Unusable::NoPorts));
	// A 32-bit controller handed the low half of a 64-bit address reads somebody else's memory, and
	// writes into it on a read. It is refused rather than truncated.
	assert_eq!(caps.usable(1, true), Err(Unusable::NotSixtyFourBit));
	assert_eq!(caps.usable(1, false), Ok(()), "and is perfectly usable with its structures below 4 GiB");
	let wide = Capabilities::decode((0x1F << 8) | (1 << 31));
	assert_eq!(wide.usable(1, true), Ok(()));
}

#[test]
fn an_atapi_device_is_refused_by_its_signature_rather_than_driven() {
	// Its command set is packet-based and nothing here speaks it. A driver that issued ATA reads
	// would get errors it would report as a failing disk.
	assert_eq!(attached(0x0000_0101), Attached::Disk);
	assert_eq!(attached(0xEB14_0101), Attached::Atapi);
	assert_eq!(attached(0x9669_0101), Attached::Unsupported, "a port multiplier");
	assert_eq!(attached(0xC33C_0101), Attached::Unsupported, "enclosure services");
	assert_eq!(attached(0xFFFF_FFFF), Attached::None);
	assert_eq!(attached(0), Attached::None);
}

#[test]
fn a_port_needs_both_a_device_and_an_active_link() {
	// DET 3 is a device detected with the physical link up; IPM 1 is the interface in an active
	// power state. A port in partial or slumber answers nothing until it is woken, and a driver that
	// issued a command would wait out its whole timeout.
	assert!(link_up(0x0000_0103), "device present, interface active");
	assert!(!link_up(0x0000_0203), "device present, interface in partial power");
	assert!(!link_up(0x0000_0101), "no device detected");
	assert!(!link_up(0), "nothing at all");
}

#[test]
fn a_cleared_slot_is_not_the_same_as_a_successful_command() {
	// AHCI clears the command-issue bit when the command COMPLETES, not when it succeeds. A driver
	// watching only `PxCI` reports somebody's bad sector as good data.
	assert_eq!(outcome(1 << 0, 0, 0), Outcome::Pending, "the slot is still set");
	assert_eq!(outcome(0, 0, 0x50), Outcome::Done, "cleared, and the task file is clean");
	assert_eq!(outcome(0, 0, 0x51 | (0x40 << 8)), Outcome::Failed { status: 0x51, error: 0x40 }, "cleared WITH an error");
	// Another slot's bit being set says nothing about this one.
	assert_eq!(outcome(1 << 3, 0, 0x50), Outcome::Done);
}

#[test]
fn a_transfer_that_is_empty_odd_or_too_long_is_refused() {
	assert_eq!(prdt_entries(0, 8), Err(Untransferable::Empty));
	// A SATA transfer moves whole words and the byte count's low bit is reserved, so an odd count
	// cannot be expressed at all.
	assert_eq!(prdt_entries(513, 8), Err(Untransferable::OddLength));
	assert_eq!(prdt_entries(512, 8), Ok(1));
	// One entry carries four mebibytes; five of them need two.
	assert_eq!(prdt_entries(PRDT_MAX_BYTES as u64, 8), Ok(1), "exactly one entry's worth");
	assert_eq!(prdt_entries(PRDT_MAX_BYTES as u64 + 2, 8), Ok(2), "two bytes more needs a second");
	assert_eq!(prdt_entries(PRDT_MAX_BYTES as u64 * 9, 8), Err(Untransferable::TooManyEntries));
}

#[test]
fn the_byte_count_field_is_zero_based_and_the_spans_add_up() {
	// A value of zero in that field is ONE byte, not none, so writing the length straight in
	// transfers one byte too many on every entry.
	assert_eq!(prdt_count(512), 511);
	assert_eq!(prdt_count(1), 0);
	assert_eq!(prdt_count(0), 0, "saturating, so a zero length cannot wrap to four gigabytes");

	let len = PRDT_MAX_BYTES as u64 + 1024;
	let entries = prdt_entries(len, 8).expect("two entries");
	let total: u64 = (0..entries).map(|i| prdt_span(len, i) as u64).sum();
	assert_eq!(total, len, "the spans cover the transfer exactly once");
	assert_eq!(prdt_span(len, 0), PRDT_MAX_BYTES);
	assert_eq!(prdt_span(len, 1), 1024);
}

// A 256-word IDENTIFY answer with the fields this driver reads.
fn identify_words(sectors: u64, lba_ext: bool) -> alloc::vec::Vec<u16> {
	let mut words = alloc::vec![0u16; 256];
	if lba_ext {
		words[83] = 1 << 10;
	}
	words[100] = sectors as u16;
	words[101] = (sectors >> 16) as u16;
	words[102] = (sectors >> 32) as u16;
	words[103] = (sectors >> 48) as u16;
	words
}

#[test]
fn a_disk_is_sized_from_the_48_bit_field_and_not_the_28_bit_one() {
	// The 28-bit field gives a large disk its size modulo 128 GiB, and a driver using it writes past
	// the end believing it is inside.
	let big = 1u64 << 34;
	assert_eq!(identify(&identify_words(big, true)), Ok(Disk { sectors: big, sector_bytes: 512 }));
}

#[test]
fn a_disk_without_48_bit_addressing_is_refused_rather_than_capped_silently() {
	assert_eq!(identify(&identify_words(1024, false)), Err(Unservable::NoLbaExt));
	assert_eq!(identify(&identify_words(0, true)), Err(Unservable::Empty));
	assert_eq!(identify(&[0u16; 16]), Err(Unservable::Empty), "an answer too short to hold the field");
}

#[test]
fn a_longer_logical_sector_is_read_and_a_missing_declaration_means_512() {
	let mut words = identify_words(2048, true);
	// Word 106 is meaningful only with bit 14 set and bit 15 clear; bit 12 says the sector is not
	// 512 bytes, and words 117/118 carry its size IN WORDS.
	words[106] = 0x4000 | (1 << 12);
	words[117] = 2048;
	words[118] = 0;
	assert_eq!(identify(&words), Ok(Disk { sectors: 2048, sector_bytes: 4096 }));

	// The same word without bit 12 declares nothing about the size, and 512 stands.
	words[106] = 0x4000;
	assert_eq!(identify(&words).expect("still a disk").sector_bytes, 512);
	// And a word 106 that is not marked valid at all is ignored whatever else it holds.
	words[106] = 0xFFFF;
	assert_eq!(identify(&words).expect("still a disk").sector_bytes, 512);
}

// --------------------------------------------------------------------- a fake controller's port
//
// A PORT IS TWO REGISTERS THAT DISAGREE ON PURPOSE. `PxCI` says whether a slot is still issued and
// `PxTFD` says what the device thought of it, and the whole trap of this interface is that the first
// clears on COMPLETION rather than on success. A model of the port is the only way to ask what
// happens over a sequence: a command issued, another issued behind it, one failing, and the error
// latched until somebody reads it.
struct FakePort {
	// One bit per slot, as the register carries them.
	issued: u32,
	// The task file: status in the low byte, error in the next.
	tfd: u32,
}

impl FakePort {
	fn new() -> FakePort {
		// A ready, idle port: DRDY set, nothing busy, no error.
		FakePort { issued: 0, tfd: 0x50 }
	}

	fn issue(&mut self, slot: u32) {
		self.issued |= 1 << slot;
		// The device takes it: busy, and the previous error is gone.
		self.tfd = 0x80;
	}

	// The device finishes a slot. `error` is the ATA error byte, or zero for success.
	fn finish(&mut self, slot: u32, error: u8) {
		self.issued &= !(1 << slot);
		self.tfd = if error == 0 { 0x50 } else { 0x51 | ((error as u32) << 8) };
	}
}

#[test]
fn a_port_reports_pending_until_the_device_clears_the_slot() {
	let mut port = FakePort::new();
	port.issue(0);
	assert_eq!(outcome(port.issued, 0, port.tfd), Outcome::Pending);
	port.finish(0, 0);
	assert_eq!(outcome(port.issued, 0, port.tfd), Outcome::Done);
}

#[test]
fn a_failure_is_read_off_the_task_file_and_not_off_the_cleared_slot() {
	// THE TRAP THIS INTERFACE SETS. The slot clears either way, so a driver watching only `PxCI`
	// reports a bad sector as good data - and the error byte is the only place the difference is.
	let mut port = FakePort::new();
	port.issue(0);
	port.finish(0, 0x40); // uncorrectable data error
	assert_eq!(outcome(port.issued, 0, port.tfd), Outcome::Failed { status: 0x51, error: 0x40 });
}

#[test]
fn one_slot_finishing_says_nothing_about_another_still_issued() {
	// Two commands outstanding on one port: clearing one must not read as clearing the other, which
	// is what a driver testing `PxCI != 0` rather than its own bit would do.
	let mut port = FakePort::new();
	port.issue(0);
	port.issue(3);
	port.finish(0, 0);
	assert_eq!(outcome(port.issued, 0, port.tfd), Outcome::Done);
	assert_eq!(outcome(port.issued, 3, port.tfd), Outcome::Pending, "the other slot is still issued");
	port.finish(3, 0);
	assert_eq!(outcome(port.issued, 3, port.tfd), Outcome::Done);
}

#[test]
fn a_command_issued_after_a_failure_is_not_reported_as_the_failure_again() {
	// The latched error is cleared when the next command is taken, so a driver that read the task
	// file without reissuing would report every later command as the first one's failure.
	let mut port = FakePort::new();
	port.issue(0);
	port.finish(0, 0x40);
	assert!(matches!(outcome(port.issued, 0, port.tfd), Outcome::Failed { .. }));
	port.issue(0);
	assert_eq!(outcome(port.issued, 0, port.tfd), Outcome::Pending, "and the new command is simply outstanding");
	port.finish(0, 0);
	assert_eq!(outcome(port.issued, 0, port.tfd), Outcome::Done);
}

#[test]
fn a_port_with_no_device_is_walked_past_rather_than_waited_on() {
	// The enumeration half of the same sequence: a port that reports no device never gets a command,
	// so `link_up` is what stops a driver issuing one and waiting out its whole timeout.
	assert!(!link_up(0), "an empty port");
	assert!(!link_up(0x0000_0201), "a detected device whose link is in partial power");
	assert!(link_up(0x0000_0103), "and one that is actually there");
}

#[test]
fn a_queued_command_puts_the_count_in_features_and_the_tag_where_the_count_was() {
	// THE CLASSIC NCQ DEFECT, held against the field layout rather than against a round trip. Eight
	// sectors under tag 3.
	let fis = queued_fis(8, 3, false);
	assert_eq!(fis.features, 8, "the low byte of the COUNT is the features field");
	assert_eq!(fis.features_exp, 0, "and its high byte is features_exp");
	assert_eq!(fis.count, 3 << 3, "the sector-count field carries the TAG, shifted left by three");
	assert_eq!(fis.device, 1 << 6, "LBA mode, and no FUA unless asked");

	// A COUNT THAT NEEDS BOTH BYTES, because a driver that wrote only the low one asks for a
	// different transfer and the disk agrees to it.
	let wide = queued_fis(0x0140, 0, false);
	assert_eq!((wide.features, wide.features_exp), (0x40, 0x01), "a count over 255 uses both bytes");

	// THE TAG'S THREE LOW BITS ARE RESERVED AND STAY ZERO. A driver writing the tag unshifted asks
	// for tag 0 with reserved bits set.
	assert_eq!(queued_fis(1, 31, false).count, 31 << 3);
	assert_eq!(queued_fis(1, 31, false).count & 0x07, 0, "the low three bits are reserved");

	assert_eq!(queued_fis(1, 0, true).device, (1 << 6) | (1 << 7), "FUA is the device register's bit 7");
}

#[test]
fn a_queued_command_is_outstanding_in_sact_and_not_in_ci() {
	// WHAT `PxCI` SAYS ABOUT A QUEUED COMMAND IS "IT WAS SENT", which is true the moment it is
	// issued. A driver reading completion out of it hands back a buffer the disk has not written.
	assert_eq!(queued_outcome(1 << 2, 2, 0), Outcome::Pending, "the tag's bit is set in SACT, so it is still running");
	assert_eq!(queued_outcome(0, 2, 0), Outcome::Done, "the device cleared it through a Set Device Bits FIS");
	// Another tag's bit says nothing about this one.
	assert_eq!(queued_outcome(1 << 5, 2, 0), Outcome::Done);

	// AN ERROR IS READ BEFORE THE BIT, because a failed queued command stops the whole queue: the
	// port halts and the remaining tags are abandoned rather than completing. A driver that checked
	// SACT first would call this one pending for ever.
	let tfd = TFD_ERR | 0x51 | (0x40 << 8);
	assert_eq!(queued_outcome(1 << 2, 2, tfd), Outcome::Failed { status: 0x51, error: 0x40 });
	assert_eq!(queued_outcome(0, 2, tfd), Outcome::Failed { status: 0x51, error: 0x40 }, "and a cleared bit does not turn a failure into a success");
}

#[test]
fn the_capability_register_says_whether_the_controller_queues() {
	// CAP.SNCQ is bit 30, beside S64A at 31 - the two are adjacent, so a driver that read the wrong
	// one refuses a 64-bit controller or queues on one that cannot.
	assert!(Capabilities::decode(1 << 30).queued);
	assert!(!Capabilities::decode(1 << 30).sixty_four_bit);
	assert!(!Capabilities::decode(1 << 31).queued);
	assert!(Capabilities::decode(1 << 31).sixty_four_bit);
	assert!(!Capabilities::decode(0).queued);
}
