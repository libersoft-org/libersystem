//! Host tests over a built EDID and over the ways a real one is wrong.
//!
//! The fixture is assembled field by field rather than pasted in as a hex blob, for the reason every
//! fixture in this tree is: a blob is a snapshot nobody can change one field of, and every test below
//! is about changing exactly one field.

use super::*;

/// A base block under construction: the header and the checksum are written by `finish`, so a fixture
/// states only what it is about.
struct Builder {
	bytes: [u8; BLOCK_LEN],
	extensions: Vec<[u8; BLOCK_LEN]>,
	/// A count OTHER than the number of blocks actually appended, which is how a monitor that lies
	/// about its extensions is built.
	declared: Option<u8>,
}

impl Builder {
	fn new() -> Self {
		let mut bytes = [0u8; BLOCK_LEN];
		bytes[..8].copy_from_slice(&MAGIC);
		// A plausible 1.4 panel: the manufacturer code, a product and serial, week 20 of 2021.
		bytes[8..10].copy_from_slice(&packed_manufacturer(b"LIB").to_be_bytes());
		bytes[10..12].copy_from_slice(&0x2a3bu16.to_le_bytes());
		bytes[12..16].copy_from_slice(&0x0102_0304u32.to_le_bytes());
		bytes[16] = 20;
		bytes[17] = 31;
		bytes[18] = 1;
		bytes[19] = 4;
		// A digital input, ten bits per channel, DisplayPort.
		bytes[20] = 0x80 | (3 << 4) | 5;
		bytes[21] = 60;
		bytes[22] = 34;
		// The feature byte: preferred timing and sRGB.
		bytes[24] = 0x02 | 0x04;
		// Established: 640x480 at 60 and 1024x768 at 60.
		bytes[35] = 0x20;
		bytes[36] = 0x08;
		// The eight standard timings start unused.
		for index in 0..8 {
			bytes[38 + index * 2] = 0x01;
			bytes[39 + index * 2] = 0x01;
		}
		Self { bytes, extensions: Vec::new(), declared: None }
	}

	fn declare_extensions(mut self, count: u8) -> Self {
		self.declared = Some(count);
		self
	}

	fn byte(mut self, offset: usize, value: u8) -> Self {
		self.bytes[offset] = value;
		self
	}

	fn standard_timing(mut self, index: usize, first: u8, second: u8) -> Self {
		self.bytes[38 + index * 2] = first;
		self.bytes[39 + index * 2] = second;
		self
	}

	fn descriptor(mut self, index: usize, bytes: [u8; DESCRIPTOR_LEN]) -> Self {
		let at = DESCRIPTOR_BASE + index * DESCRIPTOR_LEN;
		self.bytes[at..at + DESCRIPTOR_LEN].copy_from_slice(&bytes);
		self
	}

	fn extension(mut self, tag: u8) -> Self {
		let mut block = [0u8; BLOCK_LEN];
		block[0] = tag;
		block[1] = 3;
		block[BLOCK_LEN - 1] = block.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)).wrapping_neg();
		self.extensions.push(block);
		self
	}

	/// An extension block whose checksum is wrong, which is what a short or noisy read produces.
	fn broken_extension(mut self) -> Self {
		let mut block = [0u8; BLOCK_LEN];
		block[0] = 0x02;
		block[BLOCK_LEN - 1] = 0x01;
		self.extensions.push(block);
		self
	}

	fn finish(mut self) -> Vec<u8> {
		self.bytes[126] = self.declared.unwrap_or(self.extensions.len() as u8);
		self.bytes[127] = 0;
		self.bytes[127] = self.bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)).wrapping_neg();
		let mut out = self.bytes.to_vec();
		for block in &self.extensions {
			out.extend_from_slice(block);
		}
		out
	}

	/// The same bytes with the base checksum left wrong.
	fn finish_broken(self) -> Vec<u8> {
		let mut bytes = self.finish();
		bytes[127] = bytes[127].wrapping_add(1);
		bytes
	}
}

fn packed_manufacturer(letters: &[u8; 3]) -> u16 {
	let value = |letter: u8| ((letter - b'A' + 1) & 0x1f) as u16;
	(value(letters[0]) << 10) | (value(letters[1]) << 5) | value(letters[2])
}

/// A 1920x1080 at 60 Hz detailed timing, which is the mode the fixture panel prefers.
fn full_hd() -> [u8; DESCRIPTOR_LEN] {
	let mut bytes = [0u8; DESCRIPTOR_LEN];
	// 148.5 MHz in units of 10 kHz.
	bytes[0..2].copy_from_slice(&14850u16.to_le_bytes());
	bytes[2] = (1920 & 0xff) as u8;
	bytes[3] = (280 & 0xff) as u8;
	bytes[4] = ((1920 >> 8) << 4) as u8 | (280 >> 8) as u8;
	bytes[5] = (1080 & 0xff) as u8;
	bytes[6] = 45;
	bytes[7] = ((1080 >> 8) << 4) as u8;
	bytes[8] = 88;
	bytes[9] = 44;
	bytes[10] = (4 << 4) | 5;
	bytes[11] = 0;
	bytes[12] = (598 & 0xff) as u8;
	bytes[13] = (336 & 0xff) as u8;
	bytes[14] = ((598 >> 8) << 4) as u8 | (336 >> 8) as u8;
	bytes[17] = 0x1e;
	bytes
}

/// A display descriptor carrying a string.
fn string_descriptor(tag: u8, text: &[u8]) -> [u8; DESCRIPTOR_LEN] {
	let mut bytes = [0u8; DESCRIPTOR_LEN];
	bytes[3] = tag;
	let mut at = 5;
	for byte in text.iter().take(13) {
		bytes[at] = *byte;
		at += 1;
	}
	if at < DESCRIPTOR_LEN {
		bytes[at] = 0x0a;
		at += 1;
	}
	while at < DESCRIPTOR_LEN {
		bytes[at] = 0x20;
		at += 1;
	}
	bytes
}

fn panel() -> Vec<u8> {
	Builder::new()
		.standard_timing(0, 0x81, 0x40)
		.descriptor(0, full_hd())
		.descriptor(1, string_descriptor(0xfc, b"LiberPanel"))
		.descriptor(2, string_descriptor(0xff, b"SN-0001"))
		.descriptor(3, {
			let mut bytes = [0u8; DESCRIPTOR_LEN];
			bytes[3] = 0xfd;
			bytes[5] = 50;
			bytes[6] = 75;
			bytes[7] = 30;
			bytes[8] = 83;
			bytes[9] = 17;
			bytes
		})
		.finish()
}

#[test]
// The easy half: every field the fixture states comes back in the form the standard says it is in,
// not as the raw bytes.
fn a_well_formed_edid_reads_as_what_the_panel_said() {
	let bytes = panel();
	let edid = Edid::parse(&bytes).expect("a well-formed EDID");
	assert_eq!(&edid.manufacturer(), b"LIB");
	assert_eq!(edid.product_code(), 0x2a3b);
	assert_eq!(edid.serial_number(), 0x0102_0304);
	assert_eq!(edid.manufactured(), Manufactured::Week { week: 20, year: 2021 });
	assert_eq!(edid.version(), (1, 4));
	assert_eq!(edid.video_input(), VideoInput::Digital { bits_per_channel: Some(10), interface: DigitalInterface::DisplayPort });
	assert_eq!(edid.physical_size(), PhysicalSize::Centimetres { width: 60, height: 34 });
	assert!(edid.prefers_first_timing() && edid.srgb_default());
	assert_eq!(edid.name(), Some(&b"LiberPanel"[..]));
	assert_eq!(edid.extension_count(), 0);

	let timing = edid.preferred_timing().expect("the preferred mode");
	assert_eq!((timing.horizontal_active, timing.vertical_active), (1920, 1080));
	assert_eq!(timing.pixel_clock_khz, 148_500);
	assert_eq!(timing.image_size_mm, Some((598, 336)));
	assert!(!timing.interlaced);
	// 148.5 MHz over 2200 x 1125 is 60 Hz, and the millihertz form is what a driver compares.
	assert_eq!(timing.refresh_millihertz(), Some(60_000));

	assert_eq!(edid.descriptor(2), Some(Descriptor::Serial(&b"SN-0001"[..])));
	assert_eq!(edid.descriptor(3), Some(Descriptor::RangeLimits { min_vertical_hz: 50, max_vertical_hz: 75, min_horizontal_khz: 30, max_horizontal_khz: 83, max_pixel_clock_mhz: Some(170) }));
	assert_eq!(edid.descriptor(4), None, "there are four descriptors");
}

#[test]
// Four refusals before any field is read, and each says which one it was.
fn bytes_that_are_not_an_edid_are_refused_and_say_why() {
	assert_eq!(Edid::parse(&[0u8; 64]).err(), Some(Error::Truncated));

	let mut wrong_header = panel();
	wrong_header[1] = 0xfe;
	assert_eq!(Edid::parse(&wrong_header).err(), Some(Error::Header));

	assert_eq!(Edid::parse(&Builder::new().finish_broken()).err(), Some(Error::Checksum), "a block that does not sum did not arrive intact");

	assert_eq!(Edid::parse(&Builder::new().byte(18, 2).finish()).err(), Some(Error::Version), "version 2 was a different structure and no panel ships it");
}

#[test]
// The extension count is a byte a monitor wrote, and both of its failure modes are real: more blocks
// than this reader will walk, and fewer blocks than it declared.
fn the_extension_count_must_agree_with_the_bytes() {
	let one = Builder::new().descriptor(0, full_hd()).extension(0x02).finish();
	let edid = Edid::parse(&one).expect("one extension");
	assert_eq!(edid.extension_count(), 1);
	assert_eq!(edid.extension(0).map(|block| block[0]), Some(0x02));
	assert_eq!(edid.extension(1), None);

	// A count that says one and bytes that stop at the base block: the read ended early, and reading
	// the base block's own tail as an extension is how a driver believes in capabilities that are
	// really padding.
	let mut short = one.clone();
	short.truncate(BLOCK_LEN);
	assert_eq!(Edid::parse(&short).err(), Some(Error::ExtensionMissing));

	// More than this reader will walk.
	let greedy = Builder::new().declare_extensions((MAX_EXTENSIONS + 1) as u8).finish();
	assert_eq!(Edid::parse(&greedy).err(), Some(Error::TooManyExtensions));

	// AND AN EXTENSION THAT DOES NOT SUM IS NOT HANDED ON. It is read for capabilities, and a corrupt
	// one grants capabilities the panel does not have.
	let broken = Builder::new().descriptor(0, full_hd()).broken_extension().finish();
	let edid = Edid::parse(&broken).expect("the base block is fine");
	assert_eq!(edid.extension_count(), 1, "the block is there");
	assert_eq!(edid.extension(0), None, "and it is not believed");
}

#[test]
// Every one of these is a descriptor that would become a division or an impossible signal in the
// driver that took it.
fn a_timing_that_cannot_be_driven_is_not_a_timing() {
	let refused = |edit: &dyn Fn(&mut [u8; DESCRIPTOR_LEN])| {
		let mut timing = full_hd();
		edit(&mut timing);
		let bytes = Builder::new().descriptor(0, timing).finish();
		let edid = Edid::parse(&bytes).expect("the block itself is well formed");
		edid.preferred_timing()
	};

	assert!(refused(&|_| {}).is_some(), "the unedited fixture must be accepted, or nothing below means anything");
	assert_eq!(
		refused(&|timing| {
			timing[2] = 0;
			timing[4] &= 0x0f;
		}),
		None,
		"no horizontal pixels"
	);
	assert_eq!(
		refused(&|timing| {
			timing[5] = 0;
			timing[7] &= 0x0f;
		}),
		None,
		"no vertical pixels"
	);
	assert_eq!(
		refused(&|timing| {
			timing[3] = 0;
			timing[4] &= 0xf0;
		}),
		None,
		"no horizontal blanking is a division by zero in every refresh calculation"
	);
	assert_eq!(refused(&|timing| timing[6] = 0), None, "no vertical blanking, the same");
	assert_eq!(refused(&|timing| timing[9] = 255), None, "a sync pulse wider than the blanking it lives in");
	assert_eq!(
		refused(&|timing| {
			timing[10] = 0xff;
			timing[11] |= 0x0c;
		}),
		None,
		"a vertical sync past its own blanking"
	);
}

#[test]
// A slot that fails its own checks is not a descriptor of another kind, and the four display
// descriptor tags each read as themselves.
fn a_descriptor_is_what_its_tag_says_or_nothing() {
	let mut impossible = full_hd();
	impossible[3] = 0;
	impossible[4] = 0;
	let bytes = Builder::new()
		.descriptor(0, impossible)
		.descriptor(1, string_descriptor(0xfe, b"a note"))
		.descriptor(2, [0u8; DESCRIPTOR_LEN])
		.descriptor(3, {
			let mut unknown = [0u8; DESCRIPTOR_LEN];
			unknown[3] = 0xf7;
			unknown[5] = 1;
			unknown
		})
		.finish();
	let edid = Edid::parse(&bytes).expect("a well-formed block");
	assert_eq!(edid.descriptor(0), Some(Descriptor::Other(0)), "a timing this reader cannot believe is not a display descriptor either");
	assert_eq!(edid.descriptor(1), Some(Descriptor::Text(&b"a note"[..])));
	// Eighteen zero bytes: the standard's own unused slot.
	assert_eq!(edid.descriptor(2), Some(Descriptor::Other(0)));
	assert_eq!(edid.descriptor(3), Some(Descriptor::Other(0xf7)), "a tag this reader does not interpret is kept as its tag");
}

#[test]
// A string descriptor is terminated and padded by the standard, and a reader that handed both on
// gives every consumer the same trimming to do.
fn a_string_descriptor_is_trimmed_where_it_is_read() {
	let bytes = Builder::new().descriptor(0, full_hd()).descriptor(1, string_descriptor(0xfc, b"LP")).descriptor(2, string_descriptor(0xfc, b"ABCDEFGHIJKLM")).finish();
	let edid = Edid::parse(&bytes).expect("a well-formed block");
	assert_eq!(edid.name(), Some(&b"LP"[..]), "the terminator and the padding are not part of the name");
	// Thirteen characters fill the field, so there is no terminator to find.
	assert_eq!(edid.descriptor(2), Some(Descriptor::Name(&b"ABCDEFGHIJKLM"[..])));
}

#[test]
// Physical size has three answers and a consumer that computed a DPI from the wrong one divides by
// zero or reports a panel the size of a wall.
fn physical_size_says_which_of_the_three_things_it_is() {
	let centimetres = Builder::new().finish();
	assert_eq!(Edid::parse(&centimetres).unwrap().physical_size(), PhysicalSize::Centimetres { width: 60, height: 34 });

	let undefined = Builder::new().byte(21, 0).byte(22, 0).finish();
	assert_eq!(Edid::parse(&undefined).unwrap().physical_size(), PhysicalSize::Undefined);

	// A zero height makes the width byte a landscape aspect ratio: 79 + 99 is 178, which with 100 is
	// the 16:9 EDID 1.4 encodes.
	let landscape = Builder::new().byte(21, 79).byte(22, 0).finish();
	assert_eq!(Edid::parse(&landscape).unwrap().physical_size(), PhysicalSize::AspectRatio { width: 178, height: 100 });

	let portrait = Builder::new().byte(21, 0).byte(22, 79).finish();
	assert_eq!(Edid::parse(&portrait).unwrap().physical_size(), PhysicalSize::AspectRatio { width: 100, height: 178 });
}

#[test]
// The established and standard timing tables, including the two entries that are not modes.
fn the_timing_tables_answer_only_what_the_panel_declared() {
	let bytes = panel();
	let edid = Edid::parse(&bytes).expect("a well-formed block");
	let mut modes = [Mode { width: 0, height: 0, refresh_hz: 0 }; 20];

	let established = edid.established_timings(&mut modes);
	assert_eq!(established, 2, "the fixture set exactly two bits");
	assert_eq!(modes[0], Mode { width: 640, height: 480, refresh_hz: 60 });
	assert_eq!(modes[1], Mode { width: 1024, height: 768, refresh_hz: 60 });

	let standard = edid.standard_timings(&mut modes);
	assert_eq!(standard, 1, "seven slots are the unused 0x0101 and one is a mode");
	// 0x81 is (129 + 31) * 8 = 1280, aspect 1 is 4:3, and refresh 0 is 60.
	assert_eq!(modes[0], Mode { width: 1280, height: 960, refresh_hz: 60 });

	// A SLOT OF `0x01 0x01` IS UNUSED AND NOT A 256-PIXEL MODE, and a reader that did the arithmetic
	// anyway reports eight modes from every monitor in the world.
	let all_unused = Builder::new().finish();
	assert_eq!(Edid::parse(&all_unused).unwrap().standard_timings(&mut modes), 0);

	// And a caller's buffer bounds the answer rather than the panel's declaration.
	let mut one = [Mode { width: 0, height: 0, refresh_hz: 0 }; 1];
	assert_eq!(edid.established_timings(&mut one), 1);
}

#[test]
// The three ways EDID states when a panel was made are three different facts, and week 255 is not a
// week.
fn a_manufacture_date_says_which_kind_of_date_it_is() {
	assert_eq!(Edid::parse(&Builder::new().byte(16, 20).byte(17, 31).finish()).unwrap().manufactured(), Manufactured::Week { week: 20, year: 2021 });
	assert_eq!(Edid::parse(&Builder::new().byte(16, 0).byte(17, 31).finish()).unwrap().manufactured(), Manufactured::Year(2021));
	assert_eq!(Edid::parse(&Builder::new().byte(16, 0xff).byte(17, 35).finish()).unwrap().manufactured(), Manufactured::ModelYear(2025));
}

#[test]
// The input byte changed meaning at EDID 1.4, so reading a 1.3 panel's analogue flags as a bit depth
// reports a colour depth the panel never claimed.
fn the_input_description_is_read_by_the_version_that_defines_it() {
	let modern = Builder::new().byte(19, 4).byte(20, 0x80 | (2 << 4) | 1).finish();
	assert_eq!(Edid::parse(&modern).unwrap().video_input(), VideoInput::Digital { bits_per_channel: Some(8), interface: DigitalInterface::Dvi });

	let old = Builder::new().byte(19, 3).byte(20, 0x80 | (2 << 4) | 1).finish();
	assert_eq!(Edid::parse(&old).unwrap().video_input(), VideoInput::Digital { bits_per_channel: None, interface: DigitalInterface::Undefined }, "EDID 1.3 said neither");

	let analog = Builder::new().byte(20, 0x0f).finish();
	assert_eq!(Edid::parse(&analog).unwrap().video_input(), VideoInput::Analog);

	// A depth code of seven is reserved and a reader that added it to the table reports 18 bits.
	let reserved = Builder::new().byte(19, 4).byte(20, 0x80 | (7 << 4) | 9).finish();
	assert_eq!(Edid::parse(&reserved).unwrap().video_input(), VideoInput::Digital { bits_per_channel: None, interface: DigitalInterface::Reserved(9) });
}

#[test]
// EDID 1.4 made the preferred-timing bit mandatory, and a driver that took the first timing
// regardless chooses a mode the panel merely admits.
fn the_preferred_timing_is_the_one_the_panel_calls_preferred() {
	let preferred = Builder::new().byte(24, 0x02).descriptor(0, full_hd()).finish();
	assert!(Edid::parse(&preferred).unwrap().preferred_timing().is_some());

	let not_preferred = Builder::new().byte(19, 4).byte(24, 0).descriptor(0, full_hd()).finish();
	assert_eq!(Edid::parse(&not_preferred).unwrap().preferred_timing(), None);

	// On EDID 1.3 the first detailed timing WAS the preferred one by convention, and the bit was not
	// required - so refusing there would reject the panels the convention was written for.
	let legacy = Builder::new().byte(19, 3).byte(24, 0).descriptor(0, full_hd()).finish();
	assert!(Edid::parse(&legacy).unwrap().preferred_timing().is_some());

	// A first descriptor that is a NAME is not a timing, whatever the bit says.
	let named_first = Builder::new().byte(24, 0x02).descriptor(0, string_descriptor(0xfc, b"LP")).finish();
	assert_eq!(Edid::parse(&named_first).unwrap().preferred_timing(), None);
}

#[test]
// The refresh rate is the one number every consumer computes from a timing, and it divides by two
// sums of monitor-chosen bytes.
fn a_refresh_rate_is_checked_arithmetic_and_not_a_division() {
	let timing = Timing { pixel_clock_khz: 148_500, horizontal_active: 1920, horizontal_blanking: 280, vertical_active: 1080, vertical_blanking: 45, horizontal_sync_offset: 88, horizontal_sync_width: 44, vertical_sync_offset: 4, vertical_sync_width: 5, image_size_mm: None, interlaced: false };
	assert_eq!(timing.refresh_millihertz(), Some(60_000));

	// A totals of zero, which `detailed_timing` refuses but a caller building a `Timing` itself can
	// still reach: the answer is `None` rather than a panic.
	let zero = Timing { horizontal_active: 0, horizontal_blanking: 0, vertical_active: 0, vertical_blanking: 0, ..timing };
	assert_eq!(zero.refresh_millihertz(), None);

	// And a total whose product would overflow the arithmetic.
	let huge = Timing { horizontal_active: u32::MAX, horizontal_blanking: u32::MAX, ..timing };
	assert_eq!(huge.refresh_millihertz(), None);
}

#[test]
// The manufacturer code is the one big-endian field in EDID, and a zero letter is not a letter.
fn the_manufacturer_code_is_three_letters_and_not_a_number() {
	assert_eq!(&Edid::parse(&panel()).unwrap().manufacturer(), b"LIB");

	let bytes = Builder::new().byte(8, 0).byte(9, 0).finish();
	assert_eq!(&Edid::parse(&bytes).unwrap().manufacturer(), b"???", "a field nobody filled in is not a manufacturer called '@@@'");
}
