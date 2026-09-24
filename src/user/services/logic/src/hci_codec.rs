//! THE HCI COMMANDS THIS HOST SENDS AND THE EVENTS IT READS, as bytes.
//!
//! Every number on this wire is LITTLE-ENDIAN, including a device address and a public key - and
//! every Security Manager function in `smp` takes its values MOST-SIGNIFICANT FIRST, because that is
//! the order the specification's own sample data is printed in. The two meet at exactly two places,
//! a peer address and a public key's coordinates, and both conversions are here, named, and tested
//! against a known value. A byte order silently reversed at that boundary produces a pairing that
//! fails at the confirm value with nothing saying why - which is the defect this module's shape is
//! written against.
//!
//! THE EVENT PARSER IS INPUT VALIDATION. The controller chooses every length here, so an event whose
//! declared parameter length disagrees with the bytes that arrived is refused rather than read, and
//! a report list whose entries run past their event is refused at the first entry that does.

/// The four packet kinds, as `hci::Kind` numbers them.
pub use crate::hci::Kind;

/// Opcodes, as `(OGF << 10) | OCF`.
pub mod opcode {
	pub const SET_EVENT_MASK: u16 = 0x0c01;
	pub const RESET: u16 = 0x0c03;
	pub const READ_LOCAL_SUPPORTED_COMMANDS: u16 = 0x1002;
	pub const READ_BD_ADDR: u16 = 0x1009;
	pub const DISCONNECT: u16 = 0x0406;
	pub const LE_SET_EVENT_MASK: u16 = 0x2001;
	pub const LE_READ_BUFFER_SIZE: u16 = 0x2002;
	pub const LE_SET_SCAN_PARAMETERS: u16 = 0x200b;
	pub const LE_SET_SCAN_ENABLE: u16 = 0x200c;
	pub const LE_CREATE_CONNECTION: u16 = 0x200d;
	pub const LE_CREATE_CONNECTION_CANCEL: u16 = 0x200e;
	pub const LE_ENABLE_ENCRYPTION: u16 = 0x2019;
	pub const LE_READ_LOCAL_P256_PUBLIC_KEY: u16 = 0x2025;
	pub const LE_GENERATE_DHKEY: u16 = 0x2026;
}

/// Event codes.
pub mod event {
	pub const DISCONNECTION_COMPLETE: u8 = 0x05;
	pub const ENCRYPTION_CHANGE: u8 = 0x08;
	pub const COMMAND_COMPLETE: u8 = 0x0e;
	pub const COMMAND_STATUS: u8 = 0x0f;
	pub const NUMBER_OF_COMPLETED_PACKETS: u8 = 0x13;
	pub const LE_META: u8 = 0x3e;
}

/// LE meta subevent codes.
pub mod subevent {
	pub const CONNECTION_COMPLETE: u8 = 0x01;
	pub const ADVERTISING_REPORT: u8 = 0x02;
	pub const READ_LOCAL_P256_PUBLIC_KEY_COMPLETE: u8 = 0x08;
	pub const GENERATE_DHKEY_COMPLETE: u8 = 0x09;
}

/// The Supported Commands bitmap positions of the two commands LE Secure Connections needs from the
/// controller: octet 34 bits 1 and 2. A controller without both cannot pair under this service, and
/// that is reported as a fact about the machine rather than found as a pairing that fails halfway.
pub const P256_OCTET: usize = 34;
pub const P256_BITS: u8 = 0b0000_0110;

/// The longest command parameter list, which is the command ceiling less its three-byte header.
pub const MAX_COMMAND_PARAMS: usize = 255;

/// One command, as bytes. The header is the opcode and the parameter length, both as the wire wants.
pub fn command(op: u16, params: &[u8]) -> Option<alloc::vec::Vec<u8>> {
	if params.len() > MAX_COMMAND_PARAMS {
		return None;
	}
	let mut out = alloc::vec::Vec::with_capacity(3 + params.len());
	out.extend_from_slice(&op.to_le_bytes());
	out.push(params.len() as u8);
	out.extend_from_slice(params);
	Some(out)
}

/// A device address, as the wire carries it (least significant byte first) and as this service and
/// every SMP function hold it (most significant first).
pub fn address_from_wire(wire: &[u8; 6]) -> [u8; 6] {
	let mut out = *wire;
	out.reverse();
	out
}

pub fn address_to_wire(address: &[u8; 6]) -> [u8; 6] {
	address_from_wire(address)
}

/// A public key's X coordinate from the wire's 64-byte X-then-Y, each little-endian, into the
/// most-significant-first order `smp` takes.
pub fn public_key_x(wire: &[u8; 64]) -> [u8; 32] {
	let mut out = [0u8; 32];
	out.copy_from_slice(&wire[..32]);
	out.reverse();
	out
}

/// A 128-bit value between the two orders. Nonces, confirm values and keys all cross the SMP wire
/// little-endian and are computed most-significant first.
pub fn reverse16(value: &[u8; 16]) -> [u8; 16] {
	let mut out = *value;
	out.reverse();
	out
}

/// The 32-byte Diffie-Hellman key the controller answers with, into `smp`'s order.
pub fn dhkey(wire: &[u8; 32]) -> [u8; 32] {
	let mut out = *wire;
	out.reverse();
	out
}

/// `LE Set Scan Parameters`: active scanning, a 60 ms window every 60 ms, from the public address,
/// accepting every advertiser.
pub fn scan_parameters() -> [u8; 7] {
	let interval: u16 = 0x0060;
	let window: u16 = 0x0060;
	let mut out = [0u8; 7];
	out[0] = 0x01;
	out[1..3].copy_from_slice(&interval.to_le_bytes());
	out[3..5].copy_from_slice(&window.to_le_bytes());
	out[5] = 0x00;
	out[6] = 0x00;
	out
}

/// `LE Set Scan Enable`, with duplicate filtering left to this host: the controller's own filter is
/// a table of unknown size, and deduplication is a bound this service states itself.
pub fn scan_enable(on: bool) -> [u8; 2] {
	[u8::from(on), 0x00]
}

/// `LE Create Connection` to one peer, with conservative connection parameters. `kind` is the
/// peer's address type: 0 public, 1 random.
pub fn create_connection(kind: u8, address: &[u8; 6]) -> [u8; 25] {
	let mut out = [0u8; 25];
	out[0..2].copy_from_slice(&0x0060u16.to_le_bytes());
	out[2..4].copy_from_slice(&0x0060u16.to_le_bytes());
	out[4] = 0x00;
	out[5] = kind;
	out[6..12].copy_from_slice(&address_to_wire(address));
	out[12] = 0x00;
	// Connection interval 30-50 ms, no latency, a 4.2 s supervision timeout.
	out[13..15].copy_from_slice(&0x0018u16.to_le_bytes());
	out[15..17].copy_from_slice(&0x0028u16.to_le_bytes());
	out[17..19].copy_from_slice(&0x0000u16.to_le_bytes());
	out[19..21].copy_from_slice(&0x01a4u16.to_le_bytes());
	out[21..23].copy_from_slice(&0x0000u16.to_le_bytes());
	out[23..25].copy_from_slice(&0x0000u16.to_le_bytes());
	out
}

/// `Disconnect` a link, with the reason the host gives.
pub fn disconnect(handle: u16, reason: u8) -> [u8; 3] {
	let mut out = [0u8; 3];
	out[..2].copy_from_slice(&handle.to_le_bytes());
	out[2] = reason;
	out
}

/// `LE Enable Encryption` with a Secure Connections key: random number and diversifier are both zero,
/// which is what an LE Secure Connections LTK is used with. `ltk` is in `smp`'s order and is reversed
/// onto the wire here.
pub fn enable_encryption(handle: u16, ltk: &[u8; 16]) -> [u8; 28] {
	let mut out = [0u8; 28];
	out[..2].copy_from_slice(&handle.to_le_bytes());
	out[12..28].copy_from_slice(&reverse16(ltk));
	out
}

/// `LE Generate DHKey` from the peer's 64-byte public key, which is passed through as the wire
/// carried it - the controller and the SMP wire agree about this key's byte order.
pub fn generate_dhkey(remote: &[u8; 64]) -> [u8; 64] {
	*remote
}

/// Why an event could not be read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Shorter than the two-byte header every event has.
	Short { len: usize },
	/// The declared parameter length is not the length of what arrived.
	Length { declared: usize, got: usize },
	/// Too short for the fields its own code requires.
	Truncated { code: u8, needed: usize },
	/// An advertising report whose entries run past the event.
	RaggedReports,
	/// An event this host does not read.
	Unhandled { code: u8 },
}

/// One advertising report, bounded: the address and at most the 31 bytes of data an LE advertisement
/// may carry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Advertisement {
	pub kind: u8,
	/// Most significant first.
	pub address: [u8; 6],
	pub rssi: i8,
	pub data: [u8; 31],
	pub data_len: u8,
}

impl Advertisement {
	pub fn data(&self) -> &[u8] {
		&self.data[..self.data_len as usize]
	}
}

/// The events this host reads, decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event<'a> {
	CommandComplete {
		credits: u8,
		opcode: u16,
		params: &'a [u8],
	},
	CommandStatus {
		status: u8,
		credits: u8,
		opcode: u16,
	},
	Disconnected {
		status: u8,
		handle: u16,
		reason: u8,
	},
	EncryptionChange {
		status: u8,
		handle: u16,
		enabled: bool,
	},
	/// One handle's completed count, the first of the list; `rest` is the remainder for a caller that
	/// runs more than one link.
	CompletedPackets {
		handle: u16,
		count: u16,
		rest: &'a [u8],
	},
	Connected {
		status: u8,
		handle: u16,
		peer_kind: u8,
		peer: [u8; 6],
	},
	/// The raw report list, walked by `reports` - one event may carry several.
	Advertising {
		count: u8,
		reports: &'a [u8],
	},
	LocalPublicKey {
		status: u8,
		key: &'a [u8],
	},
	DhKey {
		status: u8,
		key: &'a [u8],
	},
}

/// Decode one event packet, header included.
pub fn event(bytes: &[u8]) -> Result<Event<'_>, Refusal> {
	if bytes.len() < 2 {
		return Err(Refusal::Short { len: bytes.len() });
	}
	let code = bytes[0];
	let declared = bytes[1] as usize;
	let params = &bytes[2..];
	if params.len() != declared {
		return Err(Refusal::Length { declared, got: params.len() });
	}
	let need = |n: usize| if params.len() < n { Err(Refusal::Truncated { code, needed: n }) } else { Ok(()) };
	match code {
		event::COMMAND_COMPLETE => {
			need(3)?;
			Ok(Event::CommandComplete { credits: params[0], opcode: u16::from_le_bytes([params[1], params[2]]), params: &params[3..] })
		}
		event::COMMAND_STATUS => {
			need(4)?;
			Ok(Event::CommandStatus { status: params[0], credits: params[1], opcode: u16::from_le_bytes([params[2], params[3]]) })
		}
		event::DISCONNECTION_COMPLETE => {
			need(4)?;
			Ok(Event::Disconnected { status: params[0], handle: u16::from_le_bytes([params[1], params[2]]) & 0x0fff, reason: params[3] })
		}
		event::ENCRYPTION_CHANGE => {
			need(4)?;
			Ok(Event::EncryptionChange { status: params[0], handle: u16::from_le_bytes([params[1], params[2]]) & 0x0fff, enabled: params[3] != 0 })
		}
		event::NUMBER_OF_COMPLETED_PACKETS => {
			need(5)?;
			let handles = params[0] as usize;
			if handles == 0 || params.len() != 1 + handles * 4 {
				return Err(Refusal::Truncated { code, needed: 1 + handles.max(1) * 4 });
			}
			Ok(Event::CompletedPackets { handle: u16::from_le_bytes([params[1], params[2]]) & 0x0fff, count: u16::from_le_bytes([params[3], params[4]]), rest: &params[5..] })
		}
		event::LE_META => {
			need(1)?;
			let body = &params[1..];
			match params[0] {
				subevent::CONNECTION_COMPLETE => {
					if body.len() < 18 {
						return Err(Refusal::Truncated { code, needed: 19 });
					}
					let mut wire = [0u8; 6];
					wire.copy_from_slice(&body[5..11]);
					Ok(Event::Connected { status: body[0], handle: u16::from_le_bytes([body[1], body[2]]) & 0x0fff, peer_kind: body[4], peer: address_from_wire(&wire) })
				}
				subevent::ADVERTISING_REPORT => {
					if body.is_empty() {
						return Err(Refusal::Truncated { code, needed: 2 });
					}
					Ok(Event::Advertising { count: body[0], reports: &body[1..] })
				}
				subevent::READ_LOCAL_P256_PUBLIC_KEY_COMPLETE => {
					if body.len() < 65 {
						return Err(Refusal::Truncated { code, needed: 66 });
					}
					Ok(Event::LocalPublicKey { status: body[0], key: &body[1..65] })
				}
				subevent::GENERATE_DHKEY_COMPLETE => {
					if body.len() < 33 {
						return Err(Refusal::Truncated { code, needed: 34 });
					}
					Ok(Event::DhKey { status: body[0], key: &body[1..33] })
				}
				_ => Err(Refusal::Unhandled { code }),
			}
		}
		_ => Err(Refusal::Unhandled { code }),
	}
}

/// Walk an advertising report list. The list's layout is one report after another, each an event
/// type, an address type, six address bytes, a data length, the data and a signal strength.
///
/// THE WALK STOPS AT THE FIRST REPORT THAT RUNS PAST THE EVENT and reports it, because every report
/// after a ragged one is read at the wrong offset - an advertiser's data read as the next one's
/// address.
pub fn reports(count: u8, bytes: &[u8]) -> Result<alloc::vec::Vec<Advertisement>, Refusal> {
	let mut out = alloc::vec::Vec::new();
	let mut at = 0usize;
	for _ in 0..count {
		if at + 9 > bytes.len() {
			return Err(Refusal::RaggedReports);
		}
		let kind = bytes[at + 1];
		let mut wire = [0u8; 6];
		wire.copy_from_slice(&bytes[at + 2..at + 8]);
		let len = bytes[at + 8] as usize;
		if len > 31 || at + 9 + len + 1 > bytes.len() {
			return Err(Refusal::RaggedReports);
		}
		let mut data = [0u8; 31];
		data[..len].copy_from_slice(&bytes[at + 9..at + 9 + len]);
		let rssi = bytes[at + 9 + len] as i8;
		out.push(Advertisement { kind, address: address_from_wire(&wire), rssi, data, data_len: len as u8 });
		at += 9 + len + 1;
	}
	if at != bytes.len() {
		return Err(Refusal::RaggedReports);
	}
	Ok(out)
}

/// What an advertisement says about itself: its local name, and whether it lists the human-interface
/// service. Neither is an identity, and nothing is decided from either - they are what an operator
/// reads when choosing from a list.
pub fn advertised(data: &[u8]) -> (Option<&[u8]>, bool) {
	const COMPLETE_NAME: u8 = 0x09;
	const SHORT_NAME: u8 = 0x08;
	const UUID16_INCOMPLETE: u8 = 0x02;
	const UUID16_COMPLETE: u8 = 0x03;
	const APPEARANCE: u8 = 0x19;
	const HID_SERVICE: u16 = 0x1812;
	let mut name = None;
	let mut hid = false;
	let mut at = 0usize;
	while at < data.len() {
		let len = data[at] as usize;
		if len == 0 || at + 1 + len > data.len() {
			break;
		}
		let kind = data[at + 1];
		let field = &data[at + 2..at + 1 + len];
		match kind {
			COMPLETE_NAME | SHORT_NAME => name = Some(field),
			UUID16_INCOMPLETE | UUID16_COMPLETE => hid |= field.chunks_exact(2).any(|uuid| u16::from_le_bytes([uuid[0], uuid[1]]) == HID_SERVICE),
			// Appearance 0x03C2 is a mouse; the category 0x03C0 is human-interface generally.
			APPEARANCE if field.len() == 2 => hid |= u16::from_le_bytes([field[0], field[1]]) >> 6 == 0x03c0 >> 6,
			_ => {}
		}
		at += 1 + len;
	}
	(name, hid)
}

/// An ACL data packet, as the transport carries it: the handle and boundary flag, the length, the data.
pub fn acl(handle: u16, boundary: u8, data: &[u8]) -> alloc::vec::Vec<u8> {
	let mut out = alloc::vec::Vec::with_capacity(4 + data.len());
	let word = (handle & 0x0fff) | ((boundary as u16 & 0b11) << 12);
	out.extend_from_slice(&word.to_le_bytes());
	out.extend_from_slice(&(data.len() as u16).to_le_bytes());
	out.extend_from_slice(data);
	out
}

/// One ACL packet's header, read: the handle, the boundary flag and the data. The declared length
/// must be the length that arrived.
pub fn acl_header(bytes: &[u8]) -> Option<(u16, u8, &[u8])> {
	if bytes.len() < 4 {
		return None;
	}
	let word = u16::from_le_bytes([bytes[0], bytes[1]]);
	let len = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
	if bytes.len() != 4 + len {
		return None;
	}
	Some((word & 0x0fff, ((word >> 12) & 0b11) as u8, &bytes[4..]))
}

#[cfg(test)]
mod tests;
