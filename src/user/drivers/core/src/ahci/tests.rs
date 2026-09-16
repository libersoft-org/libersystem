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
