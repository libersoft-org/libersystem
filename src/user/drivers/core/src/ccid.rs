// THE USB CCID CLASS AS PURE DECISIONS (USB Integrated Circuit(s) Cards Interface Devices 1.1): which interface a
// reader is, what its class descriptor says it can do, and what a message on its bulk pipes may say before a
// byte of it is believed.
//
// A MESSAGE IS A TEN-BYTE HEADER AND ITS DATA: the type, the data length, the slot, the sequence number and three
// bytes that depend on the type. The length is the device's claim about the rest of the transfer, so it is held
// against what arrived and against the reader's own declared maximum before the data is sliced; a reply whose
// sequence is not the outstanding request's, or whose slot is not the one asked about, answers something else.
//
// WHAT IS REFUSED AT BIND: a reader advertising more than four slots (the provider contract's bound - refused,
// not truncated), and one whose largest message cannot carry a short APDU and its header.

use crate::usb_function::{Configuration, Endpoint, Refused};
use alloc::vec::Vec;

pub const CLASS_SMART_CARD: u8 = 0x0b;
/// The class descriptor's type and its fixed length.
pub const DT_CCID: u8 = 0x21;
pub const CCID_DESCRIPTOR_LENGTH: usize = 54;
pub const MAX_SLOTS: u8 = 4;
pub const HEADER: usize = 10;
/// A short APDU command: header and 255 bytes of data, plus the expected length - and a response: 256 and SW.
pub const MAX_APDU: usize = 261;
pub const MAX_RESPONSE: usize = 258;
pub const MAX_ATR: usize = 33;

// Host to reader.
pub const PC_TO_RDR_SET_PARAMETERS: u8 = 0x61;
pub const PC_TO_RDR_ICC_POWER_ON: u8 = 0x62;
pub const PC_TO_RDR_ICC_POWER_OFF: u8 = 0x63;
pub const PC_TO_RDR_GET_SLOT_STATUS: u8 = 0x65;
pub const PC_TO_RDR_SECURE: u8 = 0x69;
pub const PC_TO_RDR_XFR_BLOCK: u8 = 0x6f;
pub const PC_TO_RDR_ABORT: u8 = 0x72;
// Reader to host.
pub const RDR_TO_PC_DATA_BLOCK: u8 = 0x80;
pub const RDR_TO_PC_SLOT_STATUS: u8 = 0x81;
pub const RDR_TO_PC_PARAMETERS: u8 = 0x82;
pub const RDR_TO_PC_NOTIFY_SLOT_CHANGE: u8 = 0x50;
pub const RDR_TO_PC_HARDWARE_ERROR: u8 = 0x51;
/// The class request that starts an abort on the control pipe, beside the bulk one.
pub const REQ_ABORT: u8 = 0x01;
pub const RT_CLASS_INTERFACE_OUT: u8 = 0x21;

/// The feature bits that say how the reader exchanges with a card.
pub const FEATURE_TPDU: u32 = 0x0001_0000;
pub const FEATURE_SHORT_APDU: u32 = 0x0002_0000;
pub const FEATURE_EXTENDED_APDU: u32 = 0x0004_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exchange {
	Character,
	Tpdu,
	ShortApdu,
	ExtendedApdu,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotBindable {
	NoReaderInterface,
	/// A reader setting without its bulk pair, or without the class descriptor.
	Incomplete,
	/// More slots than the contract carries.
	TooManySlots,
	/// A largest message too small for a short APDU.
	MessageTooSmall,
	Malformed(Refused),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
	pub config_value: u8,
	pub interface: u8,
	pub alternate: u8,
	pub bulk_in: Endpoint,
	pub bulk_out: Endpoint,
	/// The slot-change pipe, which a reader with a fixed card may leave out.
	pub interrupt_in: Option<Endpoint>,
	pub slots: u8,
	/// Bit 0 T=0, bit 1 T=1.
	pub protocols: u8,
	pub exchange: Exchange,
	pub max_message: u32,
	/// Bit 0: the reader verifies a PIN on its own keypad.
	pub pin_support: u8,
}

fn u32_at(record: &crate::descriptor::Record, offset: usize) -> Result<u32, NotBindable> {
	let low = record.field16(offset).map_err(|_| NotBindable::Incomplete)? as u32;
	let high = record.field16(offset + 2).map_err(|_| NotBindable::Incomplete)? as u32;
	Ok(low | high << 16)
}

/// The reader in one configuration.
pub fn bind(config: &[u8]) -> Result<Binding, NotBindable> {
	let parsed = Configuration::parse(config).map_err(NotBindable::Malformed)?;
	let mut readers = parsed.settings.iter().filter(|setting| setting.class == CLASS_SMART_CARD).peekable();
	if readers.peek().is_none() {
		return Err(NotBindable::NoReaderInterface);
	}
	for setting in readers {
		let (Some(bulk_in), Some(bulk_out)) = (setting.first(Endpoint::is_bulk_in), setting.first(Endpoint::is_bulk_out)) else { continue };
		let Some(class) = parsed.functional(setting).find(|record| record.kind == DT_CCID && record.len() >= CCID_DESCRIPTOR_LENGTH) else { continue };
		let slots = class.field(4).map_err(|_| NotBindable::Incomplete)?.saturating_add(1);
		if slots > MAX_SLOTS {
			return Err(NotBindable::TooManySlots);
		}
		let protocols = (u32_at(&class, 6)? & 0b11) as u8;
		let features = u32_at(&class, 40)?;
		let exchange = if features & FEATURE_EXTENDED_APDU != 0 {
			Exchange::ExtendedApdu
		} else if features & FEATURE_SHORT_APDU != 0 {
			Exchange::ShortApdu
		} else if features & FEATURE_TPDU != 0 {
			Exchange::Tpdu
		} else {
			Exchange::Character
		};
		let max_message = u32_at(&class, 44)?;
		if (max_message as usize) < HEADER + MAX_APDU {
			return Err(NotBindable::MessageTooSmall);
		}
		let pin_support = class.field(52).map_err(|_| NotBindable::Incomplete)?;
		return Ok(Binding { config_value: parsed.value, interface: setting.interface, alternate: setting.alternate, bulk_in, bulk_out, interrupt_in: setting.first(Endpoint::is_interrupt_in), slots, protocols, exchange, max_message, pin_support });
	}
	Err(NotBindable::Incomplete)
}

/// One message for the bulk OUT pipe.
pub fn encode(kind: u8, slot: u8, seq: u8, specific: [u8; 3], data: &[u8]) -> Vec<u8> {
	let mut out = Vec::with_capacity(HEADER + data.len());
	out.push(kind);
	out.extend_from_slice(&(data.len() as u32).to_le_bytes());
	out.push(slot);
	out.push(seq);
	out.extend_from_slice(&specific);
	out.extend_from_slice(data);
	out
}

/// SetParameters' protocol data, the defaults ISO 7816-3 gives each protocol.
pub fn default_parameters(t1: bool) -> ([u8; 3], Vec<u8>) {
	if t1 { ([1, 0, 0], alloc::vec![0x11, 0x10, 0x00, 0x4d, 0x00, 0xfe, 0x00]) } else { ([0, 0, 0], alloc::vec![0x11, 0x00, 0x00, 0x0a, 0x00]) }
}

/// What a reader's reply says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
	pub kind: u8,
	pub slot: u8,
	pub seq: u8,
	/// The card's state: 0 present and active, 1 present and inactive, 2 absent.
	pub icc: u8,
	/// 0 processed, 1 failed (with `error`), 2 the reader asks for more time.
	pub command: u8,
	pub error: u8,
	pub chain: u8,
	pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyRefused {
	/// Shorter than a header.
	Short,
	/// A data length past what arrived or past the reader's own maximum.
	Length,
	/// A type no bulk IN reply has.
	Kind,
}

/// A reply from the bulk IN pipe, its length held against what arrived and against `max_message`.
pub fn decode(bytes: &[u8], max_message: u32) -> Result<Reply, ReplyRefused> {
	if bytes.len() < HEADER {
		return Err(ReplyRefused::Short);
	}
	let length = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
	if length > bytes.len() - HEADER || HEADER + length > max_message as usize {
		return Err(ReplyRefused::Length);
	}
	if !matches!(bytes[0], RDR_TO_PC_DATA_BLOCK | RDR_TO_PC_SLOT_STATUS | RDR_TO_PC_PARAMETERS) {
		return Err(ReplyRefused::Kind);
	}
	Ok(Reply { kind: bytes[0], slot: bytes[5], seq: bytes[6], icc: bytes[7] & 0x03, command: bytes[7] >> 6, error: bytes[8], chain: bytes[9], data: bytes[HEADER..HEADER + length].to_vec() })
}

/// The reply type a request is answered with (the class specification's table of message pairs), for the
/// requests this transport sends.
pub fn answer_kind(request: u8) -> Option<u8> {
	match request {
		PC_TO_RDR_ICC_POWER_ON | PC_TO_RDR_XFR_BLOCK | PC_TO_RDR_SECURE => Some(RDR_TO_PC_DATA_BLOCK),
		PC_TO_RDR_ICC_POWER_OFF | PC_TO_RDR_GET_SLOT_STATUS | PC_TO_RDR_ABORT => Some(RDR_TO_PC_SLOT_STATUS),
		PC_TO_RDR_SET_PARAMETERS => Some(RDR_TO_PC_PARAMETERS),
		_ => None,
	}
}

/// What one reply is to the request waiting on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
	/// Another sequence, or another slot: an answer to something else - an aborted command's, arriving late. It
	/// is dropped and the wait goes on.
	Stray,
	/// The reader asks for more time; the wait goes on, up to its bound.
	MoreTime,
	/// The request's own sequence answered with a type that request is never answered with.
	WrongKind,
	/// The card is not in the slot.
	CardAbsent,
	/// The reader says the command failed.
	Failed,
	/// The answer.
	Answer,
}

impl Reply {
	/// This reply, held against the request of type `request` that is waiting on sequence `seq` in `slot`. The
	/// ORDER IS THE DECISION: a reply that is not this request's says nothing about it, however it is marked;
	/// a request for more time is neither an answer nor a failure; a card the reader says is gone is gone,
	/// whichever type the reader said it with; and only then is the reply's type held against the request's.
	pub fn verdict(&self, request: u8, seq: u8, slot: u8) -> Verdict {
		if self.seq != seq || self.slot != slot {
			return Verdict::Stray;
		}
		if self.command == 2 {
			return Verdict::MoreTime;
		}
		if self.icc == 2 {
			return Verdict::CardAbsent;
		}
		if answer_kind(request) != Some(self.kind) {
			return Verdict::WrongKind;
		}
		if self.command == 1 {
			return Verdict::Failed;
		}
		Verdict::Answer
	}
}

/// The sequence number after `seq`. It wraps, and a reply is matched by equality, so a wrapped number is the
/// same number again only after 256 requests - far past the one outstanding request this transport allows.
pub fn next_seq(seq: u8) -> u8 {
	seq.wrapping_add(1)
}

/// What a slot-change notification says of each slot it covers: whether a card is present, and whether that
/// changed since the last notification. Two bits a slot, from bit 0 of the byte after the type.
pub fn slot_changes(bytes: &[u8], slots: u8) -> Option<Vec<(u8, bool, bool)>> {
	if bytes.first() != Some(&RDR_TO_PC_NOTIFY_SLOT_CHANGE) {
		return None;
	}
	let needed = 1 + (slots as usize * 2).div_ceil(8);
	if bytes.len() < needed {
		return None;
	}
	Some(
		(0..slots)
			.map(|slot| {
				let bits = bytes[1 + slot as usize / 4] >> ((slot % 4) * 2);
				(slot, bits & 1 != 0, bits & 2 != 0)
			})
			.collect(),
	)
}

#[cfg(test)]
mod tests;
