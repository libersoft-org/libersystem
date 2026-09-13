//! EDID: what a monitor says about itself, read as bytes and checked before any of it is arithmetic.
//!
//! WHAT THIS IS AND IS NOT. It is a PARSER AND A VOCABULARY and it fetches nothing. EDID arrives over
//! DDC, which is I2C on the display connector, and this tree has no I2C controller and no display
//! driver that can reach one - so the transport half belongs to whichever display controller item
//! implements it, and this half is a library that half will call. That is the same split the HID
//! over I2C item takes and for the same stated reason: a candidate whose only deliverable today is a
//! parser is classified as a shared library and closed as one, rather than held against a bind gate
//! it cannot reach.
//!
//! WHY IT IS DEFENSIVE ABOUT A DESCRIPTION AND NOT ONLY ABOUT A LENGTH. An EDID is 128 bytes a
//! monitor's manufacturer wrote years ago, read over a two-wire bus that drops bits, from panels that
//! are routinely cloned and reflashed. The failures are not exotic: a checksum that does not sum
//! because the read was short, an extension count that says three when one block arrived, a detailed
//! timing whose blanking is zero so a refresh rate is a division by zero, a physical size of zero
//! that a compositor turns into a DPI of infinity. Every one of those is a number this module refuses
//! rather than passes on.
//!
//! THE VOCABULARY IS THE POINT AS MUCH AS THE PARSE. A simple framebuffer, virtio-gpu and a real
//! display driver all need to say "this is the mode the panel prefers", "this is how big the panel
//! is" and "these are the modes it admits". Three drivers inventing three descriptions of that is
//! how the compositor above them ends up with three special cases, so the types are here, beside the
//! parser, and no driver defines its own.
//!
//! WHAT IT DOES NOT DO. It does not choose a mode, does not talk to a bus, does not know what a
//! scanout is, and does not parse the CEA-861 extension's data blocks: an extension is CARRIED and
//! CHECKED here, and what is inside it belongs to the item that needs an audio or HDMI capability.

#![cfg_attr(not(test), no_std)]

/// One EDID block is 128 bytes, in every version of the standard.
pub const BLOCK_LEN: usize = 128;

/// The eight bytes every base block starts with.
const MAGIC: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];

/// The most extension blocks this reader will walk.
///
/// THE FIELD IS A BYTE AND THE BOUND IS NOT ITS RANGE. A monitor declaring 255 extensions is
/// declaring 32 kB of blocks over a bus that delivers a byte at a time; a display that really has
/// more than four is not one this system needs to read past. The cap is what stops a wrong byte
/// becoming a long wait rather than an answer.
pub const MAX_EXTENSIONS: usize = 4;

/// The four 18-byte descriptors of the base block.
const DESCRIPTORS: usize = 4;
const DESCRIPTOR_LEN: usize = 18;
const DESCRIPTOR_BASE: usize = 54;

/// Why an EDID was refused. Each variant is a way a real monitor is wrong, and they are separate
/// because a driver that logs one of them wants to know which.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
	/// Fewer than 128 bytes: the read was short.
	Truncated,
	/// The eight-byte header is not EDID's.
	Header,
	/// The block's bytes do not sum to zero.
	Checksum,
	/// A version this reader does not implement.
	Version,
	/// The extension count is beyond what this reader will walk.
	TooManyExtensions,
	/// The declared extensions are not all there.
	ExtensionMissing,
}

/// How the panel is driven.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VideoInput {
	/// An analogue input, with its signal levels left to the driver that cares.
	Analog,
	/// A digital input. The bit depth and interface are `None` before EDID 1.4, where the byte
	/// carried neither - and saying so is the point: a compositor that read a zero there would be
	/// told the panel has no colour depth.
	Digital { bits_per_channel: Option<u8>, interface: DigitalInterface },
}

/// Which digital interface the panel declares. `Undefined` is EDID 1.3 and earlier, or a monitor
/// that declines to say.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DigitalInterface {
	Undefined,
	Dvi,
	HdmiA,
	HdmiB,
	Mddi,
	DisplayPort,
	/// A value the standard has not defined, kept rather than folded into `Undefined`.
	Reserved(u8),
}

/// How big the panel is, in the terms EDID can express.
///
/// THREE ANSWERS AND NOT ONE NUMBER. A size in centimetres, a bare aspect ratio (which is what a
/// projector or a television reports), or nothing at all - and a consumer that computed a DPI from
/// the third would divide by zero, which is why the difference is in the type rather than in a
/// comment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PhysicalSize {
	Centimetres {
		width: u8,
		height: u8,
	},
	/// The stored landscape or portrait aspect ratio, as EDID 1.4 encodes it.
	AspectRatio {
		width: u16,
		height: u16,
	},
	Undefined,
}

/// A timing, in the terms a display controller programs one in.
///
/// EVERY FIELD IS A NUMBER THE MONITOR CHOSE, and the constructor is where they stop being that: a
/// total of zero, an active larger than its total, or a pixel clock of zero are each refused, because
/// each of them is a division or an overflow in the driver that would otherwise take them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Timing {
	/// Pixel clock in kHz. EDID stores it in units of 10 kHz.
	pub pixel_clock_khz: u32,
	pub horizontal_active: u32,
	pub horizontal_blanking: u32,
	pub vertical_active: u32,
	pub vertical_blanking: u32,
	pub horizontal_sync_offset: u32,
	pub horizontal_sync_width: u32,
	pub vertical_sync_offset: u32,
	pub vertical_sync_width: u32,
	/// Image size in millimetres, when the descriptor carries one.
	pub image_size_mm: Option<(u32, u32)>,
	pub interlaced: bool,
}

impl Timing {
	/// The refresh rate in millihertz, CHECKED.
	///
	/// `clock / (h_total * v_total)` is the arithmetic, and both totals are sums of two
	/// monitor-chosen numbers. A total of zero is a division by zero and a product that overflows is
	/// a refresh rate of something else entirely, so this answers `None` rather than either.
	pub fn refresh_millihertz(&self) -> Option<u32> {
		let horizontal = self.horizontal_active.checked_add(self.horizontal_blanking)?;
		let vertical = self.vertical_active.checked_add(self.vertical_blanking)?;
		let total = (horizontal as u64).checked_mul(vertical as u64)?;
		if total == 0 {
			return None;
		}
		// Clock is in kHz, so 1000 * 1000 takes it to millihertz over a pixel count.
		let millihertz = (self.pixel_clock_khz as u64).checked_mul(1_000_000)? / total;
		u32::try_from(millihertz).ok()
	}
}

/// A mode from the established or standard timing tables: a size and a refresh, with no pixel clock,
/// because those tables carry neither blanking nor sync.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mode {
	pub width: u32,
	pub height: u32,
	pub refresh_hz: u32,
}

/// What the four 18-byte descriptors can be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Descriptor<'a> {
	/// A detailed timing, which is the only kind that carries a pixel clock.
	Timing(Timing),
	/// The monitor's name, trimmed of the standard's own terminator and padding.
	Name(&'a [u8]),
	/// The serial number as ASCII, which is a different field from the numeric one in the header.
	Serial(&'a [u8]),
	/// An unstructured ASCII string the manufacturer chose.
	Text(&'a [u8]),
	/// The ranges the panel will accept, which is what a driver checks a computed mode against.
	RangeLimits { min_vertical_hz: u8, max_vertical_hz: u8, min_horizontal_khz: u8, max_horizontal_khz: u8, max_pixel_clock_mhz: Option<u32> },
	/// A descriptor tag this reader does not interpret, kept by tag so a caller can say what it saw.
	Other(u8),
	/// Eighteen zero bytes, which is how a monitor says the slot is unused.
	Unused,
}

/// The base block of an EDID, validated.
pub struct Edid<'a> {
	bytes: &'a [u8],
	extensions: &'a [u8],
}

impl<'a> Edid<'a> {
	/// Validate the base block and adopt whatever extension blocks came with it.
	///
	/// FOUR REFUSALS BEFORE ANY FIELD IS READ. The length, the header, the checksum and the version:
	/// the first two say these bytes are an EDID at all, the third says they arrived intact, and the
	/// fourth says this reader understands them. A parser that checked only the header would read a
	/// corrupted block as a monitor with strange opinions.
	pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
		if bytes.len() < BLOCK_LEN {
			return Err(Error::Truncated);
		}
		let base = &bytes[..BLOCK_LEN];
		if base[..8] != MAGIC {
			return Err(Error::Header);
		}
		if !sums_to_zero(base) {
			return Err(Error::Checksum);
		}
		// VERSION 1 ONLY, AND THE REVISION IS NOT CHECKED. Every revision of version 1 keeps the
		// layout this reader uses and adds meaning to bytes it either reads conditionally or ignores;
		// version 2 was a different structure entirely and no panel ships it.
		if base[18] != 1 {
			return Err(Error::Version);
		}
		let declared = base[126] as usize;
		if declared > MAX_EXTENSIONS {
			return Err(Error::TooManyExtensions);
		}
		let wanted = BLOCK_LEN.checked_mul(declared + 1).ok_or(Error::TooManyExtensions)?;
		// THE COUNT AND THE BYTES MUST AGREE. A monitor that declares two extensions and hands over
		// one is a read that stopped early, and reading the tail of the base block as an extension is
		// how a driver comes to believe a display supports what the padding happens to spell.
		if bytes.len() < wanted {
			return Err(Error::ExtensionMissing);
		}
		Ok(Self { bytes: base, extensions: &bytes[BLOCK_LEN..wanted] })
	}

	/// The three-letter manufacturer code, uppercase ASCII.
	///
	/// It is three FIVE-BIT letters packed into a big-endian `u16`, which is the one field in EDID
	/// that is not little-endian - and a reader that took it the other way round reports every
	/// manufacturer as a different one.
	pub fn manufacturer(&self) -> [u8; 3] {
		let packed = u16::from_be_bytes([self.bytes[8], self.bytes[9]]);
		let letter = |shift: u32| {
			let value = ((packed >> shift) & 0x1f) as u8;
			// Zero is not a letter. The standard numbers A as 1, so a zero is a field nobody filled
			// in, and answering '@' - which is what adding 64 gives - would be a manufacturer.
			if value == 0 { b'?' } else { value + b'A' - 1 }
		};
		[letter(10), letter(5), letter(0)]
	}

	pub fn product_code(&self) -> u16 {
		u16::from_le_bytes([self.bytes[10], self.bytes[11]])
	}

	pub fn serial_number(&self) -> u32 {
		u32::from_le_bytes([self.bytes[12], self.bytes[13], self.bytes[14], self.bytes[15]])
	}

	/// The week and year of manufacture, when the monitor states them.
	///
	/// THE YEAR IS AN OFFSET FROM 1990 AND THE WEEK HAS TWO SPECIAL VALUES: zero means the monitor
	/// did not say, and 255 means the year field is a MODEL year rather than a manufacture date. A
	/// reader that returned week 255 would be reporting the fifth week of the following year.
	pub fn manufactured(&self) -> Manufactured {
		let week = self.bytes[16];
		let year = 1990u32 + self.bytes[17] as u32;
		match week {
			0 => Manufactured::Year(year),
			0xff => Manufactured::ModelYear(year),
			week => Manufactured::Week { week, year },
		}
	}

	pub fn version(&self) -> (u8, u8) {
		(self.bytes[18], self.bytes[19])
	}

	pub fn video_input(&self) -> VideoInput {
		let byte = self.bytes[20];
		if byte & 0x80 == 0 {
			return VideoInput::Analog;
		}
		// The depth and interface nibbles arrived in EDID 1.4; before that the whole low seven bits
		// were analogue-style flags that mean nothing on a digital input.
		let modern = self.version().1 >= 4;
		let bits = match (modern, (byte >> 4) & 0x07) {
			(false, _) | (_, 0) => None,
			// 1 through 6 are 6, 8, 10, 12, 14 and 16 bits; 7 is reserved.
			(_, 7) => None,
			(_, code) => Some(4 + code * 2),
		};
		let interface = match (modern, byte & 0x0f) {
			(false, _) | (_, 0) => DigitalInterface::Undefined,
			(_, 1) => DigitalInterface::Dvi,
			(_, 2) => DigitalInterface::HdmiA,
			(_, 3) => DigitalInterface::HdmiB,
			(_, 4) => DigitalInterface::Mddi,
			(_, 5) => DigitalInterface::DisplayPort,
			(_, other) => DigitalInterface::Reserved(other),
		};
		VideoInput::Digital { bits_per_channel: bits, interface }
	}

	/// How big the panel is, or what shape it is, or nothing.
	pub fn physical_size(&self) -> PhysicalSize {
		let (width, height) = (self.bytes[21], self.bytes[22]);
		match (width, height) {
			(0, 0) => PhysicalSize::Undefined,
			// EDID 1.4 uses a zero in one field to mean the other holds an aspect ratio: a zero
			// HEIGHT makes the width byte a landscape ratio, and a zero width a portrait one.
			(0, portrait) => PhysicalSize::AspectRatio { width: 100, height: portrait as u16 + 99 },
			(landscape, 0) => PhysicalSize::AspectRatio { width: landscape as u16 + 99, height: 100 },
			(width, height) => PhysicalSize::Centimetres { width, height },
		}
	}

	/// Whether the first detailed timing is the panel's PREFERRED mode, which is the bit a driver
	/// actually acts on.
	pub fn prefers_first_timing(&self) -> bool {
		self.bytes[24] & 0x02 != 0
	}

	/// Whether the panel declares sRGB as its default colour space.
	pub fn srgb_default(&self) -> bool {
		self.bytes[24] & 0x04 != 0
	}

	/// The modes the established-timing bitmap names.
	///
	/// A FIXED TABLE AND NOT A COMPUTATION: these seventeen bits name seventeen specific modes from
	/// the VESA and IBM history, and a reader that derived them from the bit positions would be
	/// inventing an ordering the standard does not have.
	pub fn established_timings(&self, out: &mut [Mode]) -> usize {
		const TABLE: [(u8, u8, Mode); 17] = [
			(0, 0x80, Mode { width: 720, height: 400, refresh_hz: 70 }),
			(0, 0x40, Mode { width: 720, height: 400, refresh_hz: 88 }),
			(0, 0x20, Mode { width: 640, height: 480, refresh_hz: 60 }),
			(0, 0x10, Mode { width: 640, height: 480, refresh_hz: 67 }),
			(0, 0x08, Mode { width: 640, height: 480, refresh_hz: 72 }),
			(0, 0x04, Mode { width: 640, height: 480, refresh_hz: 75 }),
			(0, 0x02, Mode { width: 800, height: 600, refresh_hz: 56 }),
			(0, 0x01, Mode { width: 800, height: 600, refresh_hz: 60 }),
			(1, 0x80, Mode { width: 800, height: 600, refresh_hz: 72 }),
			(1, 0x40, Mode { width: 800, height: 600, refresh_hz: 75 }),
			(1, 0x20, Mode { width: 832, height: 624, refresh_hz: 75 }),
			(1, 0x10, Mode { width: 1024, height: 768, refresh_hz: 87 }),
			(1, 0x08, Mode { width: 1024, height: 768, refresh_hz: 60 }),
			(1, 0x04, Mode { width: 1024, height: 768, refresh_hz: 70 }),
			(1, 0x02, Mode { width: 1024, height: 768, refresh_hz: 75 }),
			(1, 0x01, Mode { width: 1280, height: 1024, refresh_hz: 75 }),
			(2, 0x80, Mode { width: 1152, height: 870, refresh_hz: 75 }),
		];
		let mut written = 0;
		for (byte, mask, mode) in TABLE {
			if self.bytes[35 + byte as usize] & mask != 0
				&& let Some(slot) = out.get_mut(written)
			{
				*slot = mode;
				written += 1;
			}
		}
		written
	}

	/// The eight standard timings, which are a width, an aspect ratio and a refresh.
	pub fn standard_timings(&self, out: &mut [Mode]) -> usize {
		let mut written = 0;
		for index in 0..8 {
			let at = 38 + index * 2;
			let (first, second) = (self.bytes[at], self.bytes[at + 1]);
			// `0x01 0x01` IS THE UNUSED SLOT, and it is not a 257-pixel mode. The standard says so
			// and a reader that did the arithmetic anyway reports eight modes from every monitor.
			if first == 0x01 && second == 0x01 {
				continue;
			}
			// A byte of zero would give a width of 248, which is not a mode either; the standard
			// reserves it.
			if first == 0 {
				continue;
			}
			let width = (first as u32 + 31) * 8;
			let height = match (second >> 6) & 0x03 {
				// 16:10 is the EDID 1.3 meaning of 0; before that it was 1:1, and this reader takes
				// the modern one because every panel that ships today is 1.3 or later.
				0 => width * 10 / 16,
				1 => width * 3 / 4,
				2 => width * 4 / 5,
				_ => width * 9 / 16,
			};
			let refresh = (second & 0x3f) as u32 + 60;
			if let Some(slot) = out.get_mut(written) {
				*slot = Mode { width, height, refresh_hz: refresh };
				written += 1;
			}
		}
		written
	}

	/// One of the four 18-byte descriptors.
	pub fn descriptor(&self, index: usize) -> Option<Descriptor<'a>> {
		if index >= DESCRIPTORS {
			return None;
		}
		let at = DESCRIPTOR_BASE + index * DESCRIPTOR_LEN;
		let bytes = self.bytes.get(at..at + DESCRIPTOR_LEN)?;
		Some(parse_descriptor(bytes))
	}

	/// The panel's preferred timing: the first descriptor, when it is a timing and the feature byte
	/// says it is preferred.
	///
	/// THE BIT IS PART OF THE ANSWER. EDID 1.3 made the first detailed timing the preferred one by
	/// convention and 1.4 made the bit mandatory; a driver that took the first timing regardless
	/// would be choosing a mode the panel merely admits on the monitors that clear it.
	pub fn preferred_timing(&self) -> Option<Timing> {
		if !self.prefers_first_timing() && self.version().1 >= 4 {
			return None;
		}
		match self.descriptor(0)? {
			Descriptor::Timing(timing) => Some(timing),
			_ => None,
		}
	}

	/// The monitor's name, from whichever descriptor carries it.
	pub fn name(&self) -> Option<&'a [u8]> {
		(0..DESCRIPTORS).find_map(|index| match self.descriptor(index) {
			Some(Descriptor::Name(name)) => Some(name),
			_ => None,
		})
	}

	/// How many extension blocks came with this EDID.
	pub fn extension_count(&self) -> usize {
		self.extensions.len() / BLOCK_LEN
	}

	/// One extension block, CHECKSUMMED. A block that does not sum is not handed on: an extension is
	/// read for capabilities, and a corrupt one grants capabilities the panel does not have.
	pub fn extension(&self, index: usize) -> Option<&'a [u8]> {
		let at = index.checked_mul(BLOCK_LEN)?;
		let block = self.extensions.get(at..at + BLOCK_LEN)?;
		sums_to_zero(block).then_some(block)
	}

	/// The base block as it was, for a caller that keeps the bytes beside what was read from them.
	pub fn bytes(&self) -> &'a [u8] {
		self.bytes
	}
}

/// When the panel was made, in the three forms EDID can say it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Manufactured {
	Week {
		week: u8,
		year: u32,
	},
	/// The week byte was zero: the year is all the monitor said.
	Year(u32),
	/// The week byte was 255: the year is a MODEL year, not a manufacture date.
	ModelYear(u32),
}

// An EDID block sums to zero in eight bits, the same rule for the base block and every extension.
fn sums_to_zero(block: &[u8]) -> bool {
	block.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) == 0
}

// A display descriptor's string, trimmed where it is READ so no consumer has to know the standard's
// own terminator and padding.
//
// The standard terminates with `0x0A` and pads with `0x20`, and a string that fills all thirteen bytes
// has neither - so a reader that handed both on would give every consumer the same trimming to do,
// and one of them would do it differently.
fn descriptor_text(bytes: &[u8]) -> &[u8] {
	let mut end = bytes.iter().position(|byte| *byte == 0x0a).unwrap_or(bytes.len());
	while end > 0 && bytes[end - 1] == 0x20 {
		end -= 1;
	}
	&bytes[..end]
}

// One 18-byte descriptor. The first two bytes decide what it is: a non-zero pixel clock makes it a
// detailed timing, and zero makes it a display descriptor whose kind is byte three.
fn parse_descriptor(bytes: &[u8]) -> Descriptor<'_> {
	let clock = u16::from_le_bytes([bytes[0], bytes[1]]);
	if clock != 0 {
		return match detailed_timing(bytes, clock) {
			Some(timing) => Descriptor::Timing(timing),
			// A TIMING THIS READER CANNOT BELIEVE IS NOT A DESCRIPTOR OF ANOTHER KIND. Answering
			// `Other` with the tag byte of a timing would be inventing a descriptor; the honest
			// answer is that the slot holds something that failed its own checks.
			None => Descriptor::Other(bytes[3]),
		};
	}
	match bytes[3] {
		0xff => Descriptor::Serial(descriptor_text(&bytes[5..DESCRIPTOR_LEN])),
		0xfe => Descriptor::Text(descriptor_text(&bytes[5..DESCRIPTOR_LEN])),
		0xfc => Descriptor::Name(descriptor_text(&bytes[5..DESCRIPTOR_LEN])),
		0xfd => Descriptor::RangeLimits {
			min_vertical_hz: bytes[5],
			max_vertical_hz: bytes[6],
			min_horizontal_khz: bytes[7],
			max_horizontal_khz: bytes[8],
			// Byte 9 is the maximum pixel clock in units of 10 MHz, and ZERO MEANS IT WAS NOT
			// STATED rather than a maximum of nothing - which a driver comparing a computed clock
			// against would read as "every mode is too fast".
			max_pixel_clock_mhz: (bytes[9] != 0).then(|| bytes[9] as u32 * 10),
		},
		0x10 if bytes[4] == 0 && bytes[5..].iter().all(|byte| *byte == 0) => Descriptor::Unused,
		tag if tag == 0x10 => Descriptor::Other(tag),
		tag => Descriptor::Other(tag),
	}
}

// The detailed timing descriptor, with every field that a driver would divide or add checked here.
fn detailed_timing(bytes: &[u8], clock: u16) -> Option<Timing> {
	let horizontal_active = bytes[2] as u32 | ((bytes[4] as u32 & 0xf0) << 4);
	let horizontal_blanking = bytes[3] as u32 | ((bytes[4] as u32 & 0x0f) << 8);
	let vertical_active = bytes[5] as u32 | ((bytes[7] as u32 & 0xf0) << 4);
	let vertical_blanking = bytes[6] as u32 | ((bytes[7] as u32 & 0x0f) << 8);
	// A MODE WITH NO PIXELS IS NOT A MODE, and it is what an all-zero descriptor with a stray clock
	// byte decodes to. Every consumer of this would divide by one of these two.
	if horizontal_active == 0 || vertical_active == 0 {
		return None;
	}
	// AND A MODE WITH NO BLANKING IS NOT ONE EITHER. The total is active plus blanking and the
	// refresh rate divides by it; a zero blanking is a panel description nothing can drive.
	if horizontal_blanking == 0 || vertical_blanking == 0 {
		return None;
	}
	let horizontal_sync_offset = bytes[8] as u32 | ((bytes[11] as u32 & 0xc0) << 2);
	let horizontal_sync_width = bytes[9] as u32 | ((bytes[11] as u32 & 0x30) << 4);
	let vertical_sync_offset = ((bytes[10] as u32 & 0xf0) >> 4) | ((bytes[11] as u32 & 0x0c) << 2);
	let vertical_sync_width = (bytes[10] as u32 & 0x0f) | ((bytes[11] as u32 & 0x03) << 4);
	// THE SYNC PULSE HAS TO FIT INSIDE THE BLANKING IT LIVES IN. A descriptor whose sync starts and
	// runs past the blanking interval describes a signal no controller can generate, and a driver
	// that programmed it would produce a picture that rolls.
	if horizontal_sync_offset.checked_add(horizontal_sync_width)? > horizontal_blanking {
		return None;
	}
	if vertical_sync_offset.checked_add(vertical_sync_width)? > vertical_blanking {
		return None;
	}
	let width_mm = bytes[12] as u32 | ((bytes[14] as u32 & 0xf0) << 4);
	let height_mm = bytes[13] as u32 | ((bytes[14] as u32 & 0x0f) << 8);
	Some(Timing {
		pixel_clock_khz: clock as u32 * 10,
		horizontal_active,
		horizontal_blanking,
		vertical_active,
		vertical_blanking,
		horizontal_sync_offset,
		horizontal_sync_width,
		vertical_sync_offset,
		vertical_sync_width,
		// Zero and zero is "not stated", which is not a panel of no size.
		image_size_mm: (width_mm != 0 && height_mm != 0).then_some((width_mm, height_mm)),
		interlaced: bytes[17] & 0x80 != 0,
	})
}

#[cfg(test)]
mod tests;
