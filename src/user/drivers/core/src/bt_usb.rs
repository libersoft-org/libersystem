// THE USB BLUETOOTH HCI TRANSPORT AS PURE DECISIONS (Bluetooth Core, Volume 4, Part B): which interface carries
// the controller's primary pipes, and where one HCI packet ends in a stream of USB transfers that do not say.
//
// THE PIPES. Interface 0 of a wireless controller (class 0xE0, subclass 1, protocol 1) has an interrupt IN for
// events and a bulk pair for ACL data; commands go on the control pipe. Interface 1 carries SCO voice on isochronous
// alternates - zero the zero-bandwidth one, and the one a voice setting needs from the specification's table
// (`alternate_for`).
//
// A PACKET IS AS LONG AS ITS OWN HEADER SAYS. An event is its two-byte header and the length in its second byte;
// an ACL packet its four-byte header and the length in its last two. A transfer that ends on a packet boundary is
// not a packet boundary - interrupt and bulk pipes send no zero-length packet to mark one - so the transport
// reads transfers of one packet size and cuts packets out of what accumulated by their headers. A length past the
// transport's ceiling is not waited for: the stream is out of step and is dropped.

use crate::usb_function::{Configuration, Endpoint, Refused};
use alloc::vec::Vec;

pub const CLASS_WIRELESS: u8 = 0xe0;
pub const SUBCLASS_RF: u8 = 0x01;
pub const PROTOCOL_BLUETOOTH: u8 = 0x01;
/// A command goes to the device on the control pipe: class, host to device, device recipient, request 0.
pub const RT_CLASS_DEVICE_OUT: u8 = 0x20;
pub const EVENT_HEADER: usize = 2;
pub const ACL_HEADER: usize = 4;
pub const MAX_COMMAND: usize = 258;
pub const MAX_EVENT: usize = 257;
pub const MAX_ACL: usize = 1028;
/// A SCO packet: a two-byte connection handle with its status bits, a length byte, and that many bytes.
pub const SCO_HEADER: usize = 3;
pub const MAX_SCO: usize = SCO_HEADER + 255;
/// The voice interface's alternates this transport reads: zero and the six the table names.
pub const MAX_VOICE: usize = 7;
const TRANSFER_ISOCHRONOUS: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotBindable {
	NoController,
	/// A controller interface without its event pipe and its bulk pair.
	NoPipes,
	Malformed(Refused),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
	pub config_value: u8,
	pub interface: u8,
	pub events: Endpoint,
	pub acl_in: Endpoint,
	pub acl_out: Endpoint,
	/// The voice interface, and each of its alternates that carries an isochronous pair - by alternate number.
	pub voice_interface: Option<u8>,
	pub voice: [Option<Voice>; MAX_VOICE],
}

/// One voice alternate: its number and its isochronous pair, each carrying one service interval's piece of SCO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Voice {
	pub alternate: u8,
	pub iso_in: Endpoint,
	pub iso_out: Endpoint,
}

impl Binding {
	/// The voice alternate `alternate`, when the controller declares it with both pipes.
	pub fn voice_at(&self, alternate: u8) -> Option<Voice> {
		self.voice.get(alternate as usize).copied().flatten()
	}

	/// Whether this controller carries voice at all.
	pub fn has_voice(&self) -> bool {
		self.voice.iter().any(Option::is_some)
	}
}

/// The controller's primary interface - alternate zero of the first Bluetooth interface - and its voice interface, the
/// next one of the same class: every alternate of it whose isochronous pair has room for a piece.
pub fn bind(config: &[u8]) -> Result<Binding, NotBindable> {
	let parsed = Configuration::parse(config).map_err(NotBindable::Malformed)?;
	let bluetooth = |setting: &&crate::usb_function::Setting| setting.is(CLASS_WIRELESS, SUBCLASS_RF) && setting.protocol == PROTOCOL_BLUETOOTH;
	let setting = parsed.settings.iter().filter(bluetooth).find(|setting| setting.alternate == 0).ok_or(NotBindable::NoController)?;
	let (Some(events), Some(acl_in), Some(acl_out)) = (setting.first(Endpoint::is_interrupt_in), setting.first(Endpoint::is_bulk_in), setting.first(Endpoint::is_bulk_out)) else {
		return Err(NotBindable::NoPipes);
	};
	let voice_interface = parsed.settings.iter().filter(bluetooth).map(|other| other.interface).find(|&interface| interface != setting.interface);
	let mut voice = [None; MAX_VOICE];
	for other in parsed.settings.iter().filter(bluetooth).filter(|other| Some(other.interface) == voice_interface) {
		let iso = |input: bool| other.first(|endpoint| endpoint.transfer() == TRANSFER_ISOCHRONOUS && endpoint.is_in() == input && endpoint.max_packet() > 0);
		if let (Some(slot), Some(iso_in), Some(iso_out)) = (voice.get_mut(other.alternate as usize), iso(true), iso(false))
			&& other.alternate != 0
		{
			*slot = Some(Voice { alternate: other.alternate, iso_in, iso_out });
		}
	}
	Ok(Binding { config_value: parsed.value, interface: setting.interface, events, acl_in, acl_out, voice_interface, voice })
}

/// THE ALTERNATE A VOICE SETTING NEEDS (Core, Volume 4, Part B, 2.1.1): one, two or three channels of 8-bit samples are
/// alternates 1 to 3; of 16-bit samples 2, 4 and 5; the wideband setting, one channel, is alternate 6. `None` for a
/// setting the table has no row for.
pub fn alternate_for(channels: u8, bits: u8, wideband: bool) -> Option<u8> {
	match (channels, bits, wideband) {
		(1, _, true) => Some(6),
		(1..=3, 8, false) => Some(channels),
		(1, 16, false) => Some(2),
		(2, 16, false) => Some(4),
		(3, 16, false) => Some(5),
		_ => None,
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
	Event,
	Acl,
}

/// Packets cut out of a pipe's transfers by their own headers.
pub struct Reassembly {
	kind: Kind,
	held: Vec<u8>,
}

impl Reassembly {
	pub fn new(kind: Kind) -> Reassembly {
		Reassembly { kind, held: Vec::new() }
	}

	pub fn clear(&mut self) {
		self.held.clear();
	}

	fn length(&self) -> Option<usize> {
		match self.kind {
			Kind::Event if self.held.len() >= EVENT_HEADER => Some(EVENT_HEADER + self.held[1] as usize),
			Kind::Acl if self.held.len() >= ACL_HEADER => Some(ACL_HEADER + u16::from_le_bytes([self.held[2], self.held[3]]) as usize),
			_ => None,
		}
	}

	/// Take one transfer's bytes, and every whole packet they complete. A header claiming more than the kind's
	/// ceiling empties what is held: the stream is out of step, and waiting for that many bytes would swallow the
	/// packets after it.
	pub fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
		self.held.extend_from_slice(bytes);
		let ceiling = match self.kind {
			Kind::Event => MAX_EVENT,
			Kind::Acl => MAX_ACL,
		};
		let mut out = Vec::new();
		while let Some(length) = self.length() {
			if length > ceiling {
				self.held.clear();
				break;
			}
			if self.held.len() < length {
				break;
			}
			out.push(self.held.drain(..length).collect());
		}
		out
	}
}

/// Whether a command packet is one: its three-byte header, and a parameter length that is the rest of it.
pub fn command_is_whole(packet: &[u8]) -> bool {
	packet.len() >= 3 && packet.len() <= MAX_COMMAND && packet[2] as usize == packet.len() - 3
}

/// Whether a SCO packet is one: its three-byte header, and a length that is the rest of it.
pub fn sco_is_whole(packet: &[u8]) -> bool {
	packet.len() >= SCO_HEADER && packet.len() <= MAX_SCO && packet[2] as usize == packet.len() - SCO_HEADER
}

/// SCO PACKETS CUT OUT OF ISOCHRONOUS PIECES, AND A PACKET THAT LOST ONE REFUSED. A packet starts a piece - a controller
/// cuts them that way, and so does this transport going out - so a lost piece leaves a packet whose pieces do not add
/// up, and it is never delivered with a hole in it: when its header arrived, it said how long the packet is and the
/// pieces still to come of it are skipped; when the lost piece WAS its header, pieces are skipped until one opens a
/// packet on the connection the last whole packet came on. An empty piece - an interval with nothing to carry - is no
/// piece of anything.
pub struct ScoPieces {
	held: Vec<u8>,
	capacity: usize,
	skip: Skip,
	handle: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Skip {
	Nothing,
	Pieces(usize),
	ToHandle,
}

fn handle_of(bytes: &[u8]) -> u16 {
	u16::from_le_bytes([bytes[0], bytes[1]]) & 0x0fff
}

impl ScoPieces {
	/// Pieces of at most `capacity` bytes each - one service interval of the voice alternate.
	pub fn new(capacity: usize) -> ScoPieces {
		ScoPieces { held: Vec::new(), capacity: capacity.max(1), skip: Skip::Nothing, handle: None }
	}

	/// One piece, and every whole packet it completes.
	pub fn piece(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
		if bytes.is_empty() {
			return Vec::new();
		}
		match self.skip {
			Skip::Pieces(left) => {
				self.skip = if left > 1 { Skip::Pieces(left - 1) } else { Skip::Nothing };
				return Vec::new();
			}
			Skip::ToHandle => {
				if bytes.len() < SCO_HEADER || Some(handle_of(bytes)) != self.handle {
					return Vec::new();
				}
				self.skip = Skip::Nothing;
			}
			Skip::Nothing => {}
		}
		self.held.extend_from_slice(bytes);
		let mut out = Vec::new();
		while self.held.len() >= SCO_HEADER {
			let length = SCO_HEADER + self.held[2] as usize;
			if self.held.len() < length {
				break;
			}
			let packet: Vec<u8> = self.held.drain(..length).collect();
			self.handle = Some(handle_of(&packet));
			out.push(packet);
		}
		out
	}

	/// A piece the transport lost: the packet it belonged to is dropped, and what is still to come of it skipped.
	pub fn lost(&mut self) {
		self.skip = if self.held.len() >= SCO_HEADER {
			// The pieces the packet occupies, the ones it had - whole, as a packet's are until its last - and this one.
			let pieces = (SCO_HEADER + self.held[2] as usize).div_ceil(self.capacity);
			match pieces.saturating_sub(self.held.len().div_ceil(self.capacity) + 1) {
				0 => Skip::Nothing,
				left => Skip::Pieces(left),
			}
		} else if self.handle.is_some() {
			Skip::ToHandle
		} else {
			Skip::Nothing
		};
		self.held.clear();
	}

	/// Everything held and skipped forgotten, as a new voice setting starts.
	pub fn clear(&mut self) {
		self.held.clear();
		self.skip = Skip::Nothing;
	}
}

/// Whether an ACL packet is one: its four-byte header, and a data length that is the rest of it.
pub fn acl_is_whole(packet: &[u8]) -> bool {
	packet.len() >= ACL_HEADER && packet.len() <= MAX_ACL && u16::from_le_bytes([packet[2], packet[3]]) as usize == packet.len() - ACL_HEADER
}

#[cfg(test)]
mod tests;
