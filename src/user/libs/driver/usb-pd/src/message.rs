//! THE MESSAGE CODEC: a Power Delivery message on SOP - its 16-bit header and up to seven 32-bit data objects - as a port
//! controller's receive buffer holds it and its transmit buffer takes it.
//!
//! A MESSAGE IS BELIEVED ONLY WHEN IT CHECKS: the header's object count agrees with the bytes that came, the message type
//! is one the specification defines for its class, and an extended message - which a sink in the Standard Power Range
//! takes none of - is refused as extended. What fails is not believed and nothing is inferred from it.

use alloc::vec::Vec;

/// The specification revision a header carries in bits 7-6.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum Revision {
	R1 = 0,
	R2 = 1,
	R3 = 2,
}

impl Revision {
	fn of(bits: u16) -> Option<Revision> {
		match bits {
			0 => Some(Revision::R1),
			1 => Some(Revision::R2),
			2 => Some(Revision::R3),
			_ => None,
		}
	}
}

/// The control messages: a header and no data object.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Control {
	GoodCrc = 1,
	GotoMin = 2,
	Accept = 3,
	Reject = 4,
	Ping = 5,
	PsRdy = 6,
	GetSourceCap = 7,
	GetSinkCap = 8,
	DrSwap = 9,
	PrSwap = 10,
	VconnSwap = 11,
	Wait = 12,
	SoftReset = 13,
	DataReset = 14,
	DataResetComplete = 15,
	NotSupported = 16,
	GetSourceCapExtended = 17,
	GetStatus = 18,
	FrSwap = 19,
	GetPpsStatus = 20,
	GetCountryCodes = 21,
	GetSinkCapExtended = 22,
	GetSourceInfo = 23,
	GetRevision = 24,
}

impl Control {
	fn of(kind: u16) -> Option<Control> {
		use Control::*;
		Some(match kind {
			1 => GoodCrc,
			2 => GotoMin,
			3 => Accept,
			4 => Reject,
			5 => Ping,
			6 => PsRdy,
			7 => GetSourceCap,
			8 => GetSinkCap,
			9 => DrSwap,
			10 => PrSwap,
			11 => VconnSwap,
			12 => Wait,
			13 => SoftReset,
			14 => DataReset,
			15 => DataResetComplete,
			16 => NotSupported,
			17 => GetSourceCapExtended,
			18 => GetStatus,
			19 => FrSwap,
			20 => GetPpsStatus,
			21 => GetCountryCodes,
			22 => GetSinkCapExtended,
			23 => GetSourceInfo,
			24 => GetRevision,
			_ => return None,
		})
	}
}

/// The data messages: a header and one to seven data objects.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Data {
	SourceCapabilities = 1,
	Request = 2,
	Bist = 3,
	SinkCapabilities = 4,
	BatteryStatus = 5,
	Alert = 6,
	GetCountryInfo = 7,
	EnterUsb = 8,
	EprRequest = 9,
	EprMode = 10,
	SourceInfo = 11,
	Revision = 12,
	VendorDefined = 15,
}

impl Data {
	fn of(kind: u16) -> Option<Data> {
		use Data::*;
		Some(match kind {
			1 => SourceCapabilities,
			2 => Request,
			3 => Bist,
			4 => SinkCapabilities,
			5 => BatteryStatus,
			6 => Alert,
			7 => GetCountryInfo,
			8 => EnterUsb,
			9 => EprRequest,
			10 => EprMode,
			11 => SourceInfo,
			12 => Revision,
			15 => VendorDefined,
			_ => return None,
		})
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
	Control(Control),
	Data(Data),
}

/// One message as it crossed the wire.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Message {
	pub kind: Kind,
	pub revision: Revision,
	pub id: u8,
	/// Bit 8 of the header on SOP: the sender is the source.
	pub from_source: bool,
	/// Bit 5: the sender is the downstream-facing port.
	pub from_dfp: bool,
	pub objects: Vec<u32>,
}

/// Why a message was not believed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Fewer than two bytes, or a byte count the header's object count does not match.
	Length,
	/// A message type the specification does not define for its class, or a reserved revision.
	Reserved,
	/// An extended message.
	Extended,
}

/// The most data objects a message carries.
pub const MAX_OBJECTS: usize = 7;

pub fn decode(bytes: &[u8]) -> Result<Message, Refusal> {
	let header = u16::from_le_bytes([*bytes.first().ok_or(Refusal::Length)?, *bytes.get(1).ok_or(Refusal::Length)?]);
	let count = usize::from((header >> 12) & 0x7);
	if header & 0x8000 != 0 {
		return Err(Refusal::Extended);
	}
	if bytes.len() != 2 + 4 * count {
		return Err(Refusal::Length);
	}
	let revision = Revision::of((header >> 6) & 0x3).ok_or(Refusal::Reserved)?;
	let kind_bits = header & 0x1F;
	let kind = if count == 0 { Kind::Control(Control::of(kind_bits).ok_or(Refusal::Reserved)?) } else { Kind::Data(Data::of(kind_bits).ok_or(Refusal::Reserved)?) };
	let objects = bytes[2..].chunks_exact(4).map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])).collect();
	Ok(Message { kind, revision, id: ((header >> 9) & 0x7) as u8, from_source: header & 0x100 != 0, from_dfp: header & 0x20 != 0, objects })
}

/// A message this sink sends, its header built for it: the sink's roles (power sink; data UFP unless swapped - which
/// nothing here does), the revision spoken, and the MessageID.
pub fn encode(kind: Kind, revision: Revision, id: u8, objects: &[u32]) -> Vec<u8> {
	let (class_bits, count) = match kind {
		Kind::Control(control) => (control as u16, 0),
		Kind::Data(data) => (data as u16, objects.len().min(MAX_OBJECTS) as u16),
	};
	let header = class_bits | (revision as u16) << 6 | u16::from(id & 0x7) << 9 | count << 12;
	let mut out = Vec::with_capacity(2 + 4 * usize::from(count));
	out.extend_from_slice(&header.to_le_bytes());
	for object in objects.iter().take(usize::from(count)) {
		out.extend_from_slice(&object.to_le_bytes());
	}
	out
}
