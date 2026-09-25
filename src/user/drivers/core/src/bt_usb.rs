// THE USB BLUETOOTH HCI TRANSPORT AS PURE DECISIONS (Bluetooth Core, Volume 4, Part B): which interface carries
// the controller's primary pipes, and where one HCI packet ends in a stream of USB transfers that do not say.
//
// THE PIPES. Interface 0 of a wireless controller (class 0xE0, subclass 1, protocol 1) has an interrupt IN for
// events and a bulk pair for ACL data; commands go on the control pipe. Interface 1 carries SCO and ISO on
// isochronous alternates, which this transport does not drive.
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
}

/// The controller's primary interface: alternate zero of the first Bluetooth interface.
pub fn bind(config: &[u8]) -> Result<Binding, NotBindable> {
	let parsed = Configuration::parse(config).map_err(NotBindable::Malformed)?;
	let setting = parsed.settings.iter().find(|setting| setting.is(CLASS_WIRELESS, SUBCLASS_RF) && setting.protocol == PROTOCOL_BLUETOOTH && setting.alternate == 0).ok_or(NotBindable::NoController)?;
	match (setting.first(Endpoint::is_interrupt_in), setting.first(Endpoint::is_bulk_in), setting.first(Endpoint::is_bulk_out)) {
		(Some(events), Some(acl_in), Some(acl_out)) => Ok(Binding { config_value: parsed.value, interface: setting.interface, events, acl_in, acl_out }),
		_ => Err(NotBindable::NoPipes),
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

/// Whether an ACL packet is one: its four-byte header, and a data length that is the rest of it.
pub fn acl_is_whole(packet: &[u8]) -> bool {
	packet.len() >= ACL_HEADER && packet.len() <= MAX_ACL && u16::from_le_bytes([packet[2], packet[3]]) as usize == packet.len() - ACL_HEADER
}

#[cfg(test)]
mod tests;
