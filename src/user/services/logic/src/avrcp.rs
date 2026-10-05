//! AVRCP OVER AVCTP: the remote control a headset and a phone speak - pass-through operations (play, pause, next,
//! previous) and the absolute volume - as pure functions of bytes.
//!
//! AVCTP is a header on an L2CAP channel on PSM 0x17: a transaction label, the packet type, whether it is a command or
//! a response, whether the profile is understood, and the profile's 16-bit identifier - AV Remote Control's. Inside it
//! an AV/C frame: a command or response type, the panel subunit, and an opcode - PASS THROUGH for the buttons, VENDOR
//! DEPENDENT carrying the Bluetooth SIG's company identifier and an AVRCP PDU for the volume.
//!
//! THE TARGET'S ANSWER TO WHAT IT DOES NOT DO is NOT IMPLEMENTED: this system has no media session yet, so a headset's
//! play or pause is answered that way rather than acknowledged as done.

use alloc::vec::Vec;

/// The PSM AVCTP's control channel uses.
pub const PSM: u16 = 0x0017;
/// AV Remote Control's profile identifier in the AVCTP header.
pub const PROFILE: u16 = 0x110e;
/// The Bluetooth SIG's company identifier, before every AVRCP vendor-dependent PDU.
pub const BLUETOOTH_SIG: [u8; 3] = [0x00, 0x19, 0x58];

/// AV/C command and response types.
pub mod ctype {
	pub const CONTROL: u8 = 0x00;
	pub const STATUS: u8 = 0x01;
	pub const NOTIFY: u8 = 0x03;
	pub const NOT_IMPLEMENTED: u8 = 0x08;
	pub const ACCEPTED: u8 = 0x09;
	pub const REJECTED: u8 = 0x0a;
	pub const STABLE: u8 = 0x0c;
	pub const CHANGED: u8 = 0x0d;
	pub const INTERIM: u8 = 0x0f;
}

/// The panel subunit, type 9 id 0, as the subunit byte carries it.
pub const PANEL: u8 = 0x48;

pub mod opcode {
	pub const VENDOR_DEPENDENT: u8 = 0x00;
	pub const PASS_THROUGH: u8 = 0x7c;
}

/// The pass-through operations this system sends and recognises.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Operation {
	Play,
	Stop,
	Pause,
	Forward,
	Backward,
	VolumeUp,
	VolumeDown,
	Other(u8),
}

impl Operation {
	pub const fn id(self) -> u8 {
		match self {
			Operation::Play => 0x44,
			Operation::Stop => 0x45,
			Operation::Pause => 0x46,
			Operation::Forward => 0x4b,
			Operation::Backward => 0x4c,
			Operation::VolumeUp => 0x41,
			Operation::VolumeDown => 0x42,
			Operation::Other(id) => id & 0x7f,
		}
	}

	pub const fn of(id: u8) -> Operation {
		match id & 0x7f {
			0x44 => Operation::Play,
			0x45 => Operation::Stop,
			0x46 => Operation::Pause,
			0x4b => Operation::Forward,
			0x4c => Operation::Backward,
			0x41 => Operation::VolumeUp,
			0x42 => Operation::VolumeDown,
			other => Operation::Other(other),
		}
	}
}

/// AVRCP's vendor-dependent PDUs this system uses.
pub mod pdu {
	pub const GET_CAPABILITIES: u8 = 0x10;
	pub const REGISTER_NOTIFICATION: u8 = 0x31;
	pub const SET_ABSOLUTE_VOLUME: u8 = 0x50;
}

/// The volume-changed event a controller registers for.
pub const EVENT_VOLUME_CHANGED: u8 = 0x0d;

/// The highest absolute volume: 0x7f is the loudest.
pub const MAX_VOLUME: u8 = 0x7f;

/// ONE AVCTP MESSAGE carrying one AV/C frame.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
	pub label: u8,
	pub response: bool,
	/// The profile was not understood: the IPID bit, set on a response only.
	pub invalid_profile: bool,
	pub ctype: u8,
	pub subunit: u8,
	pub opcode: u8,
	pub operands: Vec<u8>,
}

impl Frame {
	pub fn encode(&self) -> Vec<u8> {
		let mut out = Vec::with_capacity(6 + self.operands.len());
		out.push((self.label << 4) | (u8::from(self.response) << 1) | u8::from(self.invalid_profile));
		out.extend_from_slice(&PROFILE.to_be_bytes());
		out.extend_from_slice(&[self.ctype & 0x0f, self.subunit, self.opcode]);
		out.extend_from_slice(&self.operands);
		out
	}

	/// One frame, or `None`: shorter than its headers, fragmented, or for another profile.
	pub fn decode(bytes: &[u8]) -> Option<Frame> {
		if bytes.len() < 6 || (bytes[0] >> 2) & 3 != 0 {
			return None;
		}
		if u16::from_be_bytes([bytes[1], bytes[2]]) != PROFILE {
			return None;
		}
		Some(Frame { label: bytes[0] >> 4, response: bytes[0] & 2 != 0, invalid_profile: bytes[0] & 1 != 0, ctype: bytes[3] & 0x0f, subunit: bytes[4], opcode: bytes[5], operands: bytes[6..].to_vec() })
	}

	/// A PASS-THROUGH command: the operation, pressed or released.
	pub fn pass_through(label: u8, operation: Operation, pressed: bool) -> Frame {
		Frame { label: label & 0x0f, response: false, invalid_profile: false, ctype: ctype::CONTROL, subunit: PANEL, opcode: opcode::PASS_THROUGH, operands: alloc::vec![operation.id() | if pressed { 0 } else { 0x80 }, 0] }
	}

	/// An AVRCP PDU in a vendor-dependent frame.
	pub fn vendor(label: u8, ctype: u8, pdu: u8, parameters: &[u8]) -> Frame {
		let mut operands = Vec::with_capacity(7 + parameters.len());
		operands.extend_from_slice(&BLUETOOTH_SIG);
		operands.push(pdu);
		operands.push(0);
		operands.extend_from_slice(&(parameters.len() as u16).to_be_bytes());
		operands.extend_from_slice(parameters);
		Frame { label: label & 0x0f, response: false, invalid_profile: false, ctype, subunit: PANEL, opcode: opcode::VENDOR_DEPENDENT, operands }
	}

	/// The answer to this command, with its own label and operands and the response type given.
	pub fn answer(&self, ctype: u8, operands: &[u8]) -> Frame {
		Frame { label: self.label, response: true, invalid_profile: false, ctype, subunit: self.subunit, opcode: self.opcode, operands: operands.to_vec() }
	}

	/// A pass-through's operation and whether it is the press.
	pub fn operation(&self) -> Option<(Operation, bool)> {
		if self.opcode != opcode::PASS_THROUGH || self.operands.is_empty() {
			return None;
		}
		Some((Operation::of(self.operands[0]), self.operands[0] & 0x80 == 0))
	}

	/// A vendor-dependent frame's AVRCP PDU and its parameters.
	pub fn pdu(&self) -> Option<(u8, &[u8])> {
		if self.opcode != opcode::VENDOR_DEPENDENT || self.operands.len() < 7 || self.operands[..3] != BLUETOOTH_SIG {
			return None;
		}
		let length = usize::from(u16::from_be_bytes([self.operands[5], self.operands[6]]));
		Some((self.operands[3], self.operands.get(7..7 + length)?))
	}
}

/// A LEVEL AS AVRCP CARRIES IT, from the 0 to 100 AudioService keeps, and back - rounded, so a level survives the trip.
pub fn to_absolute(level: u8) -> u8 {
	((u32::from(level.min(100)) * u32::from(MAX_VOLUME) + 50) / 100) as u8
}

pub fn from_absolute(volume: u8) -> u8 {
	((u32::from(volume & MAX_VOLUME) * 100 + u32::from(MAX_VOLUME) / 2) / u32::from(MAX_VOLUME)) as u8
}

#[cfg(test)]
mod tests;
