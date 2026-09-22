//! THE BOOT MOUSE SUBSET OF HID-OVER-GATT, which is one report format and one mode selection.
//!
//! A HID device describes itself with a REPORT MAP - a small language for laying out fields in a
//! packet - and a host that reads one is running a parser over bytes a peer chose. The boot protocol
//! exists so that a host need not: a device in boot mode sends one fixed layout that predates the
//! report map, which is how a keyboard works in a firmware setup screen.
//!
//! THIS MILESTONE TAKES THE FIXED LAYOUT AND NOTHING ELSE. Arbitrary report maps, keyboards and the
//! rest of HOGP are excluded by the milestone in as many words, and what is here refuses them by
//! name rather than half-implementing them - a device whose reports do not fit the boot layout is
//! reported as unsupported, not decoded on a guess.
//!
//! THE DISPLACEMENTS ARE SIGNED, and this is the defect the whole module is written against. Read
//! unsigned, a move of one pixel to the LEFT is `0xff`, which is two hundred and fifty-five pixels
//! to the right - so a mouse pushed left walks to the right edge and stays there, and nothing
//! anywhere reports an error.

/// The protocol mode a HOGP device is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
	/// The fixed boot layout. This is the one this milestone speaks.
	Boot = 0,
	/// The device's own report map. Out of scope here, and named so a device already in it can be
	/// asked to change rather than being decoded as though it were not.
	Report = 1,
}

impl Mode {
	/// What a mode byte names, or `None` for a value this characteristic does not define. Two values
	/// are defined and every other is a device answering with something that is not a mode.
	pub const fn from_byte(value: u8) -> Option<Mode> {
		match value {
			0 => Some(Mode::Boot),
			1 => Some(Mode::Report),
			_ => None,
		}
	}

	pub const fn byte(self) -> u8 {
		self as u8
	}
}

/// The buttons the boot layout carries. THREE BITS AND NOT A BYTE: the bits above them are reserved,
/// and a host that took the whole byte as a button mask would report buttons four through eight
/// whenever a device set one.
pub const BUTTON_LEFT: u8 = 1 << 0;
pub const BUTTON_RIGHT: u8 = 1 << 1;
pub const BUTTON_MIDDLE: u8 = 1 << 2;
pub const BUTTON_MASK: u8 = BUTTON_LEFT | BUTTON_RIGHT | BUTTON_MIDDLE;

/// The boot mouse report is three bytes, or four with a wheel.
pub const MIN_REPORT: usize = 3;
pub const MAX_REPORT: usize = 4;

/// Why a report was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Too short to hold the fixed layout.
	Short { len: usize },
	/// Longer than the boot layout is. A device sending five bytes is not in boot mode, whatever its
	/// protocol-mode characteristic says, and decoding the first four would be reading a report map
	/// this milestone does not parse as though it were the layout that predates one.
	NotBootLayout { len: usize },
	/// A mode byte this characteristic does not define.
	UnknownMode(u8),
	/// The device is in report mode, which this subset does not speak. A typed answer rather than a
	/// decode, so a caller can ask it to change instead of receiving nonsense.
	ReportMode,
}

/// One decoded boot mouse report.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Report {
	/// The three defined buttons, as the bits above.
	pub buttons: u8,
	/// Displacement since the last report, SIGNED.
	pub dx: i32,
	pub dy: i32,
	/// Wheel ticks, signed, and zero for the three-byte form.
	pub wheel: i32,
}

/// Decode one boot mouse input report.
pub fn report(bytes: &[u8]) -> Result<Report, Refusal> {
	if bytes.len() < MIN_REPORT {
		return Err(Refusal::Short { len: bytes.len() });
	}
	if bytes.len() > MAX_REPORT {
		return Err(Refusal::NotBootLayout { len: bytes.len() });
	}
	Ok(Report {
		buttons: bytes[0] & BUTTON_MASK,
		// `as i8 as i32` and not `as i32`: the cast through the signed byte is what carries the sign.
		dx: bytes[1] as i8 as i32,
		dy: bytes[2] as i8 as i32,
		wheel: if bytes.len() > 3 { bytes[3] as i8 as i32 } else { 0 },
	})
}

/// What this client does about the mode a device reports it is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModeAction {
	/// Already in boot mode: start the reports.
	Ready,
	/// In report mode: write boot mode and read it back.
	SelectBoot,
}

/// Read a protocol-mode characteristic's value and say what to do about it.
///
/// A MODE IS ONE BYTE AND A LONGER VALUE IS NOT A LONGER MODE. A device answering with two bytes has
/// answered with something else, and taking the first would be reading a field whose meaning this
/// code has no basis for.
pub fn mode(value: &[u8]) -> Result<ModeAction, Refusal> {
	let [byte] = value else {
		return Err(Refusal::Short { len: value.len() });
	};
	match Mode::from_byte(*byte) {
		Some(Mode::Boot) => Ok(ModeAction::Ready),
		Some(Mode::Report) => Ok(ModeAction::SelectBoot),
		None => Err(Refusal::UnknownMode(*byte)),
	}
}

/// What a device's mode says about whether its reports may be decoded at all.
///
/// SEPARATE FROM `mode` ON PURPOSE: that answers what to DO about a mode, and this answers whether a
/// report arriving right now is one this subset understands. A device that changed mode without
/// being asked is the case, and it arrives as a report rather than as a mode read.
pub fn decodable(current: Mode) -> Result<(), Refusal> {
	match current {
		Mode::Boot => Ok(()),
		Mode::Report => Err(Refusal::ReportMode),
	}
}

/// The Client Characteristic Configuration value that turns notifications on, and the one that turns
/// them off. TWO BYTES, LITTLE-ENDIAN, and bit zero is notifications: bit one is indications, which
/// this profile does not use and must not set - an indication is acknowledged, and a device waiting
/// for an acknowledgement this client never sends stops sending reports.
pub const NOTIFICATIONS_ON: [u8; 2] = [0x01, 0x00];
pub const NOTIFICATIONS_OFF: [u8; 2] = [0x00, 0x00];

#[cfg(test)]
mod tests;
