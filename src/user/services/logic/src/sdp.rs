//! SDP ON BOTH SIDES: the data elements every record is made of, the request and response PDUs with their
//! continuation, a CLIENT that finds a classic profile's channel through a peer's records, and a SERVER answering
//! from the records of the roles this system offers while they are enabled.
//!
//! SDP is the one service the specification lets run without security - a peer reads it before it pairs - so every
//! number here is a stranger's: element sizes, nesting, counts and continuation states are bounded, and a malformed
//! request is answered with an error response rather than a guess. Every multi-byte value in SDP is big-endian.

use alloc::vec::Vec;

#[cfg(test)]
mod tests;

/// How deep data elements may nest.
pub const MAX_DEPTH: usize = 8;
/// How many elements one sequence may hold.
pub const MAX_ELEMENTS: usize = 256;
/// The UUIDs one search pattern may name, as the specification bounds it.
pub const MAX_PATTERN: usize = 12;
/// A continuation state's bytes, as the specification bounds them.
pub const MAX_CONTINUATION: usize = 16;
/// The largest response this client assembles across continuations.
pub const MAX_ASSEMBLED: usize = 16 * 1024;
/// Records this host's server holds: one for each role it offers, well inside it.
pub const MAX_RECORDS: usize = 16;

/// The Bluetooth base UUID, into which a 16- or 32-bit UUID is placed.
pub const BASE_UUID: [u8; 16] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0x80, 0x5F, 0x9B, 0x34, 0xFB];

/// The UUIDs this stack names.
pub mod uuid {
	pub const L2CAP: u16 = 0x0100;
	pub const RFCOMM: u16 = 0x0003;
	pub const OBEX: u16 = 0x0008;
	pub const BNEP: u16 = 0x000F;
	pub const HIDP: u16 = 0x0011;
	pub const AVCTP: u16 = 0x0017;
	pub const AVDTP: u16 = 0x0019;
	pub const SERIAL_PORT: u16 = 0x1101;
	pub const OBEX_OBJECT_PUSH: u16 = 0x1105;
	pub const HEADSET: u16 = 0x1108;
	pub const AUDIO_SOURCE: u16 = 0x110A;
	pub const AUDIO_SINK: u16 = 0x110B;
	pub const AV_REMOTE_CONTROL_TARGET: u16 = 0x110C;
	pub const ADVANCED_AUDIO_DISTRIBUTION: u16 = 0x110D;
	pub const AV_REMOTE_CONTROL: u16 = 0x110E;
	pub const AV_REMOTE_CONTROL_CONTROLLER: u16 = 0x110F;
	pub const HEADSET_AUDIO_GATEWAY: u16 = 0x1112;
	pub const PANU: u16 = 0x1115;
	pub const NAP: u16 = 0x1116;
	pub const GN: u16 = 0x1117;
	pub const HANDSFREE: u16 = 0x111E;
	pub const HANDSFREE_AUDIO_GATEWAY: u16 = 0x111F;
	pub const HUMAN_INTERFACE_DEVICE: u16 = 0x1124;
	pub const GENERIC_AUDIO: u16 = 0x1203;
	pub const PUBLIC_BROWSE_ROOT: u16 = 0x1002;
}

/// The attribute ids this stack reads and writes.
pub mod attribute {
	pub const SERVICE_RECORD_HANDLE: u16 = 0x0000;
	pub const SERVICE_CLASS_ID_LIST: u16 = 0x0001;
	pub const PROTOCOL_DESCRIPTOR_LIST: u16 = 0x0004;
	pub const BROWSE_GROUP_LIST: u16 = 0x0005;
	pub const PROFILE_DESCRIPTOR_LIST: u16 = 0x0009;
	pub const ADDITIONAL_PROTOCOL_DESCRIPTOR_LISTS: u16 = 0x000D;
	pub const SERVICE_NAME: u16 = 0x0100;
	pub const GOEP_L2CAP_PSM: u16 = 0x0200;
	pub const SUPPORTED_FEATURES: u16 = 0x0311;
	pub const SUPPORTED_FORMATS: u16 = 0x0303;
	pub const NETWORK: u16 = 0x0301;
}

/// A UUID as SDP carries it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Uuid {
	U16(u16),
	U32(u32),
	U128([u8; 16]),
}

impl Uuid {
	/// The full 128-bit form, a short UUID placed in the base UUID - which is how two UUIDs are compared.
	pub fn full(&self) -> [u8; 16] {
		match *self {
			Uuid::U16(short) => {
				let mut out = BASE_UUID;
				out[2..4].copy_from_slice(&short.to_be_bytes());
				out
			}
			Uuid::U32(short) => {
				let mut out = BASE_UUID;
				out[0..4].copy_from_slice(&short.to_be_bytes());
				out
			}
			Uuid::U128(bytes) => bytes,
		}
	}

	pub fn same(&self, other: &Uuid) -> bool {
		self.full() == other.full()
	}
}

/// One data element.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Element {
	Nil,
	/// An unsigned integer and its width in bytes: 1, 2, 4, 8 or 16.
	Uint(u128, u8),
	Int(i128, u8),
	Uuid(Uuid),
	Text(Vec<u8>),
	Bool(bool),
	Sequence(Vec<Element>),
	Alternative(Vec<Element>),
	Url(Vec<u8>),
}

impl Element {
	pub const fn u8(value: u8) -> Element {
		Element::Uint(value as u128, 1)
	}

	pub const fn u16(value: u16) -> Element {
		Element::Uint(value as u128, 2)
	}

	pub const fn u32(value: u32) -> Element {
		Element::Uint(value as u128, 4)
	}

	pub const fn uuid16(value: u16) -> Element {
		Element::Uuid(Uuid::U16(value))
	}

	pub fn text(value: &str) -> Element {
		Element::Text(value.as_bytes().to_vec())
	}

	pub fn uint(&self) -> Option<u128> {
		if let Element::Uint(value, _) = self { Some(*value) } else { None }
	}

	pub fn elements(&self) -> Option<&[Element]> {
		match self {
			Element::Sequence(elements) | Element::Alternative(elements) => Some(elements),
			_ => None,
		}
	}

	/// Every UUID anywhere inside this element.
	pub fn uuids(&self, out: &mut Vec<Uuid>) {
		match self {
			Element::Uuid(uuid) => out.push(*uuid),
			Element::Sequence(elements) | Element::Alternative(elements) => {
				for element in elements {
					element.uuids(out);
				}
			}
			_ => {}
		}
	}
}

/// Why bytes are not an element, or a PDU.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	Truncated,
	/// A type and size combination the specification does not define.
	BadDescriptor(u8),
	TooDeep,
	TooMany,
	/// Bytes left over after what a PDU says it carries.
	Trailing,
}

fn put_length(out: &mut Vec<u8>, kind: u8, len: usize) {
	if len <= 0xFF {
		out.push((kind << 3) | 5);
		out.push(len as u8);
	} else if len <= 0xFFFF {
		out.push((kind << 3) | 6);
		out.extend_from_slice(&(len as u16).to_be_bytes());
	} else {
		out.push((kind << 3) | 7);
		out.extend_from_slice(&(len as u32).to_be_bytes());
	}
}

fn width_index(width: u8) -> u8 {
	match width {
		1 => 0,
		2 => 1,
		4 => 2,
		8 => 3,
		_ => 4,
	}
}

pub fn encode_element(element: &Element, out: &mut Vec<u8>) {
	match element {
		Element::Nil => out.push(0),
		Element::Uint(value, width) => {
			out.push((1 << 3) | width_index(*width));
			out.extend_from_slice(&value.to_be_bytes()[16 - usize::from(*width)..]);
		}
		Element::Int(value, width) => {
			out.push((2 << 3) | width_index(*width));
			out.extend_from_slice(&value.to_be_bytes()[16 - usize::from(*width)..]);
		}
		Element::Uuid(Uuid::U16(value)) => {
			out.push((3 << 3) | 1);
			out.extend_from_slice(&value.to_be_bytes());
		}
		Element::Uuid(Uuid::U32(value)) => {
			out.push((3 << 3) | 2);
			out.extend_from_slice(&value.to_be_bytes());
		}
		Element::Uuid(Uuid::U128(bytes)) => {
			out.push((3 << 3) | 4);
			out.extend_from_slice(bytes);
		}
		Element::Text(bytes) => {
			put_length(out, 4, bytes.len());
			out.extend_from_slice(bytes);
		}
		Element::Bool(value) => {
			out.push(5 << 3);
			out.push(u8::from(*value));
		}
		Element::Sequence(elements) | Element::Alternative(elements) => {
			let mut body = Vec::new();
			for element in elements {
				encode_element(element, &mut body);
			}
			put_length(out, if matches!(element, Element::Sequence(_)) { 6 } else { 7 }, body.len());
			out.extend_from_slice(&body);
		}
		Element::Url(bytes) => {
			put_length(out, 8, bytes.len());
			out.extend_from_slice(bytes);
		}
	}
}

/// ONE ELEMENT from the front of `bytes`: it and what follows it.
pub fn decode_element(bytes: &[u8]) -> Result<(Element, &[u8]), Refusal> {
	decode_at(bytes, 0)
}

fn take(bytes: &[u8], n: usize) -> Result<(&[u8], &[u8]), Refusal> {
	if bytes.len() < n {
		return Err(Refusal::Truncated);
	}
	Ok(bytes.split_at(n))
}

fn decode_at(bytes: &[u8], depth: usize) -> Result<(Element, &[u8]), Refusal> {
	if depth > MAX_DEPTH {
		return Err(Refusal::TooDeep);
	}
	let (&descriptor, rest) = bytes.split_first().ok_or(Refusal::Truncated)?;
	let (kind, size) = (descriptor >> 3, descriptor & 7);
	let fixed = |size: u8| -> Option<usize> { [1usize, 2, 4, 8, 16].get(usize::from(size)).copied() };
	let variable = |rest: &[u8]| -> Result<(usize, usize), Refusal> {
		match size {
			5 => Ok((usize::from(*rest.first().ok_or(Refusal::Truncated)?), 1)),
			6 => Ok((usize::from(u16::from_be_bytes([*rest.first().ok_or(Refusal::Truncated)?, *rest.get(1).ok_or(Refusal::Truncated)?])), 2)),
			7 => {
				let (len, _) = take(rest, 4)?;
				Ok((u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize, 4))
			}
			_ => Err(Refusal::BadDescriptor(descriptor)),
		}
	};
	match kind {
		0 if size == 0 => Ok((Element::Nil, rest)),
		1 | 2 => {
			let width = fixed(size).ok_or(Refusal::BadDescriptor(descriptor))?;
			let (value, rest) = take(rest, width)?;
			let mut full = [0u8; 16];
			full[16 - width..].copy_from_slice(value);
			if kind == 1 {
				Ok((Element::Uint(u128::from_be_bytes(full), width as u8), rest))
			} else {
				// SIGN-EXTENDED from its own width.
				let shift = (16 - width) * 8;
				Ok((Element::Int((i128::from_be_bytes(full) << shift) >> shift, width as u8), rest))
			}
		}
		3 => match size {
			1 => {
				let (value, rest) = take(rest, 2)?;
				Ok((Element::Uuid(Uuid::U16(u16::from_be_bytes([value[0], value[1]]))), rest))
			}
			2 => {
				let (value, rest) = take(rest, 4)?;
				Ok((Element::Uuid(Uuid::U32(u32::from_be_bytes([value[0], value[1], value[2], value[3]]))), rest))
			}
			4 => {
				let (value, rest) = take(rest, 16)?;
				let mut full = [0u8; 16];
				full.copy_from_slice(value);
				Ok((Element::Uuid(Uuid::U128(full)), rest))
			}
			_ => Err(Refusal::BadDescriptor(descriptor)),
		},
		5 if size == 0 => {
			let (value, rest) = take(rest, 1)?;
			Ok((Element::Bool(value[0] != 0), rest))
		}
		4 | 6 | 7 | 8 => {
			let (len, header) = variable(rest)?;
			let (body, rest) = take(&rest[header..], len)?;
			match kind {
				4 => Ok((Element::Text(body.to_vec()), rest)),
				8 => Ok((Element::Url(body.to_vec()), rest)),
				_ => {
					let mut elements = Vec::new();
					let mut inner = body;
					while !inner.is_empty() {
						if elements.len() >= MAX_ELEMENTS {
							return Err(Refusal::TooMany);
						}
						let (element, after) = decode_at(inner, depth + 1)?;
						elements.push(element);
						inner = after;
					}
					Ok((if kind == 6 { Element::Sequence(elements) } else { Element::Alternative(elements) }, rest))
				}
			}
		}
		_ => Err(Refusal::BadDescriptor(descriptor)),
	}
}

// ------------------------------------------------------------------------------------------------------------- PDUs

/// The PDU ids.
pub mod pdu {
	pub const ERROR_RESPONSE: u8 = 0x01;
	pub const SERVICE_SEARCH_REQUEST: u8 = 0x02;
	pub const SERVICE_SEARCH_RESPONSE: u8 = 0x03;
	pub const SERVICE_ATTRIBUTE_REQUEST: u8 = 0x04;
	pub const SERVICE_ATTRIBUTE_RESPONSE: u8 = 0x05;
	pub const SERVICE_SEARCH_ATTRIBUTE_REQUEST: u8 = 0x06;
	pub const SERVICE_SEARCH_ATTRIBUTE_RESPONSE: u8 = 0x07;
}

/// The error codes an error response carries.
pub mod error {
	pub const INVALID_VERSION: u16 = 0x0001;
	pub const INVALID_HANDLE: u16 = 0x0002;
	pub const INVALID_SYNTAX: u16 = 0x0003;
	pub const INVALID_PDU_SIZE: u16 = 0x0004;
	pub const INVALID_CONTINUATION: u16 = 0x0005;
	pub const INSUFFICIENT_RESOURCES: u16 = 0x0006;
}

/// An attribute id or an inclusive range of them, as an attribute id list names them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wanted {
	Id(u16),
	Range(u16, u16),
}

impl Wanted {
	fn contains(&self, id: u16) -> bool {
		match *self {
			Wanted::Id(wanted) => wanted == id,
			Wanted::Range(low, high) => (low..=high).contains(&id),
		}
	}
}

/// One PDU.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Pdu {
	Error { code: u16 },
	ServiceSearchRequest { pattern: Vec<Uuid>, max_records: u16, continuation: Vec<u8> },
	ServiceSearchResponse { total: u16, handles: Vec<u32>, continuation: Vec<u8> },
	ServiceAttributeRequest { handle: u32, max_bytes: u16, wanted: Vec<Wanted>, continuation: Vec<u8> },
	ServiceAttributeResponse { bytes: Vec<u8>, continuation: Vec<u8> },
	ServiceSearchAttributeRequest { pattern: Vec<Uuid>, max_bytes: u16, wanted: Vec<Wanted>, continuation: Vec<u8> },
	ServiceSearchAttributeResponse { bytes: Vec<u8>, continuation: Vec<u8> },
}

fn pattern_element(pattern: &[Uuid]) -> Element {
	Element::Sequence(pattern.iter().map(|uuid| Element::Uuid(*uuid)).collect())
}

fn wanted_element(wanted: &[Wanted]) -> Element {
	Element::Sequence(
		wanted
			.iter()
			.map(|wanted| match *wanted {
				Wanted::Id(id) => Element::u16(id),
				Wanted::Range(low, high) => Element::u32(u32::from(low) << 16 | u32::from(high)),
			})
			.collect(),
	)
}

fn put_continuation(out: &mut Vec<u8>, continuation: &[u8]) {
	out.push(continuation.len() as u8);
	out.extend_from_slice(continuation);
}

/// One PDU with its transaction id, as the bytes of an L2CAP SDU.
pub fn encode(transaction: u16, pdu: &Pdu) -> Vec<u8> {
	let mut body = Vec::new();
	let id = match pdu {
		Pdu::Error { code } => {
			body.extend_from_slice(&code.to_be_bytes());
			pdu::ERROR_RESPONSE
		}
		Pdu::ServiceSearchRequest { pattern, max_records, continuation } => {
			encode_element(&pattern_element(pattern), &mut body);
			body.extend_from_slice(&max_records.to_be_bytes());
			put_continuation(&mut body, continuation);
			pdu::SERVICE_SEARCH_REQUEST
		}
		Pdu::ServiceSearchResponse { total, handles, continuation } => {
			body.extend_from_slice(&total.to_be_bytes());
			body.extend_from_slice(&(handles.len() as u16).to_be_bytes());
			for handle in handles {
				body.extend_from_slice(&handle.to_be_bytes());
			}
			put_continuation(&mut body, continuation);
			pdu::SERVICE_SEARCH_RESPONSE
		}
		Pdu::ServiceAttributeRequest { handle, max_bytes, wanted, continuation } => {
			body.extend_from_slice(&handle.to_be_bytes());
			body.extend_from_slice(&max_bytes.to_be_bytes());
			encode_element(&wanted_element(wanted), &mut body);
			put_continuation(&mut body, continuation);
			pdu::SERVICE_ATTRIBUTE_REQUEST
		}
		Pdu::ServiceAttributeResponse { bytes, continuation } => {
			body.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
			body.extend_from_slice(bytes);
			put_continuation(&mut body, continuation);
			pdu::SERVICE_ATTRIBUTE_RESPONSE
		}
		Pdu::ServiceSearchAttributeRequest { pattern, max_bytes, wanted, continuation } => {
			encode_element(&pattern_element(pattern), &mut body);
			body.extend_from_slice(&max_bytes.to_be_bytes());
			encode_element(&wanted_element(wanted), &mut body);
			put_continuation(&mut body, continuation);
			pdu::SERVICE_SEARCH_ATTRIBUTE_REQUEST
		}
		Pdu::ServiceSearchAttributeResponse { bytes, continuation } => {
			body.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
			body.extend_from_slice(bytes);
			put_continuation(&mut body, continuation);
			pdu::SERVICE_SEARCH_ATTRIBUTE_RESPONSE
		}
	};
	let mut out = Vec::with_capacity(5 + body.len());
	out.push(id);
	out.extend_from_slice(&transaction.to_be_bytes());
	out.extend_from_slice(&(body.len() as u16).to_be_bytes());
	out.extend_from_slice(&body);
	out
}

fn decode_pattern(element: Element) -> Result<Vec<Uuid>, Refusal> {
	let Element::Sequence(elements) = element else { return Err(Refusal::BadDescriptor(0)) };
	if elements.is_empty() || elements.len() > MAX_PATTERN {
		return Err(Refusal::TooMany);
	}
	elements.into_iter().map(|element| if let Element::Uuid(uuid) = element { Ok(uuid) } else { Err(Refusal::BadDescriptor(0)) }).collect()
}

fn decode_wanted(element: Element) -> Result<Vec<Wanted>, Refusal> {
	let Element::Sequence(elements) = element else { return Err(Refusal::BadDescriptor(0)) };
	if elements.is_empty() {
		return Err(Refusal::TooMany);
	}
	elements
		.into_iter()
		.map(|element| match element {
			Element::Uint(id, 2) => Ok(Wanted::Id(id as u16)),
			Element::Uint(range, 4) => Ok(Wanted::Range((range >> 16) as u16, range as u16)),
			_ => Err(Refusal::BadDescriptor(0)),
		})
		.collect()
}

fn take_continuation(bytes: &[u8]) -> Result<(Vec<u8>, &[u8]), Refusal> {
	let (&len, rest) = bytes.split_first().ok_or(Refusal::Truncated)?;
	if usize::from(len) > MAX_CONTINUATION {
		return Err(Refusal::TooMany);
	}
	let (state, rest) = take(rest, usize::from(len))?;
	Ok((state.to_vec(), rest))
}

fn u16_of(bytes: &[u8]) -> Result<(u16, &[u8]), Refusal> {
	let (value, rest) = take(bytes, 2)?;
	Ok((u16::from_be_bytes([value[0], value[1]]), rest))
}

/// ONE PDU and its transaction id, from an L2CAP SDU.
pub fn decode(bytes: &[u8]) -> Result<(u16, Pdu), Refusal> {
	let (header, body) = take(bytes, 5)?;
	let transaction = u16::from_be_bytes([header[1], header[2]]);
	let len = usize::from(u16::from_be_bytes([header[3], header[4]]));
	if body.len() != len {
		return Err(if body.len() < len { Refusal::Truncated } else { Refusal::Trailing });
	}
	let done = |rest: &[u8]| if rest.is_empty() { Ok(()) } else { Err(Refusal::Trailing) };
	let pdu = match header[0] {
		pdu::ERROR_RESPONSE => {
			let (code, rest) = u16_of(body)?;
			let _ = rest;
			Pdu::Error { code }
		}
		pdu::SERVICE_SEARCH_REQUEST => {
			let (pattern, rest) = decode_element(body)?;
			let (max_records, rest) = u16_of(rest)?;
			let (continuation, rest) = take_continuation(rest)?;
			done(rest)?;
			Pdu::ServiceSearchRequest { pattern: decode_pattern(pattern)?, max_records, continuation }
		}
		pdu::SERVICE_SEARCH_RESPONSE => {
			let (total, rest) = u16_of(body)?;
			let (current, rest) = u16_of(rest)?;
			let (list, rest) = take(rest, usize::from(current) * 4)?;
			// COLLECTED, NEVER PUSHED: a `Vec<u32>` that grows by `push` imports its growth routine from whichever
			// loaded crate shares that instance, which differs per target. An exact-size collect allocates once.
			let handles: Vec<u32> = list.chunks_exact(4).map(|handle| u32::from_be_bytes([handle[0], handle[1], handle[2], handle[3]])).collect();
			let (continuation, rest) = take_continuation(rest)?;
			done(rest)?;
			Pdu::ServiceSearchResponse { total, handles, continuation }
		}
		pdu::SERVICE_ATTRIBUTE_REQUEST => {
			let (handle, rest) = take(body, 4)?;
			let (max_bytes, rest) = u16_of(rest)?;
			let (wanted, rest) = decode_element(rest)?;
			let (continuation, rest) = take_continuation(rest)?;
			done(rest)?;
			Pdu::ServiceAttributeRequest { handle: u32::from_be_bytes([handle[0], handle[1], handle[2], handle[3]]), max_bytes, wanted: decode_wanted(wanted)?, continuation }
		}
		pdu::SERVICE_ATTRIBUTE_RESPONSE | pdu::SERVICE_SEARCH_ATTRIBUTE_RESPONSE => {
			let (count, rest) = u16_of(body)?;
			let (bytes, rest) = take(rest, usize::from(count))?;
			let (continuation, rest) = take_continuation(rest)?;
			done(rest)?;
			if header[0] == pdu::SERVICE_ATTRIBUTE_RESPONSE { Pdu::ServiceAttributeResponse { bytes: bytes.to_vec(), continuation } } else { Pdu::ServiceSearchAttributeResponse { bytes: bytes.to_vec(), continuation } }
		}
		pdu::SERVICE_SEARCH_ATTRIBUTE_REQUEST => {
			let (pattern, rest) = decode_element(body)?;
			let (max_bytes, rest) = u16_of(rest)?;
			let (wanted, rest) = decode_element(rest)?;
			let (continuation, rest) = take_continuation(rest)?;
			done(rest)?;
			Pdu::ServiceSearchAttributeRequest { pattern: decode_pattern(pattern)?, max_bytes, wanted: decode_wanted(wanted)?, continuation }
		}
		other => return Err(Refusal::BadDescriptor(other)),
	};
	Ok((transaction, pdu))
}

// ----------------------------------------------------------------------------------------------------------- records

/// One service record: its handle and its attributes, ascending by id.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Record {
	pub handle: u32,
	pub attributes: Vec<(u16, Element)>,
}

impl Record {
	pub fn get(&self, id: u16) -> Option<&Element> {
		self.attributes.iter().find(|(at, _)| *at == id).map(|(_, value)| value)
	}

	/// Whether every UUID of a search pattern is somewhere in this record.
	pub fn matches(&self, pattern: &[Uuid]) -> bool {
		let mut uuids = Vec::new();
		for (_, value) in &self.attributes {
			value.uuids(&mut uuids);
		}
		pattern.iter().all(|wanted| uuids.iter().any(|uuid| uuid.same(wanted)))
	}

	/// The attributes a list asks for, as the sequence of id and value pairs a response carries.
	pub fn list(&self, wanted: &[Wanted]) -> Element {
		let mut out = Vec::new();
		for (id, value) in &self.attributes {
			if wanted.iter().any(|wanted| wanted.contains(*id)) {
				out.push(Element::u16(*id));
				out.push(value.clone());
			}
		}
		Element::Sequence(out)
	}

	/// The record a response's attribute list describes.
	pub fn from_list(element: &Element) -> Option<Record> {
		let elements = element.elements()?;
		let mut attributes = Vec::new();
		for pair in elements.chunks(2) {
			let [Element::Uint(id, 2), value] = pair else { return None };
			attributes.push((*id as u16, value.clone()));
		}
		let handle = attributes.iter().find(|(id, _)| *id == attribute::SERVICE_RECORD_HANDLE).and_then(|(_, value)| value.uint()).unwrap_or(0) as u32;
		Some(Record { handle, attributes })
	}

	/// THE RFCOMM SERVER CHANNEL this record's protocol descriptor list names.
	pub fn rfcomm_channel(&self) -> Option<u8> {
		protocol_parameter(self.get(attribute::PROTOCOL_DESCRIPTOR_LIST)?, uuid::RFCOMM).map(|value| value as u8)
	}

	/// THE L2CAP PSM this record's protocol descriptor list names.
	pub fn l2cap_psm(&self) -> Option<u16> {
		protocol_parameter(self.get(attribute::PROTOCOL_DESCRIPTOR_LIST)?, uuid::L2CAP).map(|value| value as u16)
	}

	/// The PSM of an additional protocol descriptor list - AVRCP's browsing channel - where the record has one.
	pub fn additional_psm(&self) -> Option<u16> {
		let lists = self.get(attribute::ADDITIONAL_PROTOCOL_DESCRIPTOR_LISTS)?.elements()?;
		lists.iter().find_map(|list| protocol_parameter(list, uuid::L2CAP)).map(|value| value as u16)
	}

	/// The version this record names for a profile, as its profile descriptor list says.
	pub fn profile_version(&self, profile: u16) -> Option<u16> {
		let profiles = self.get(attribute::PROFILE_DESCRIPTOR_LIST)?.elements()?;
		profiles.iter().find_map(|entry| {
			let [Element::Uuid(id), Element::Uint(version, _)] = entry.elements()? else { return None };
			id.same(&Uuid::U16(profile)).then_some(*version as u16)
		})
	}

	pub fn supported_features(&self) -> Option<u16> {
		self.get(attribute::SUPPORTED_FEATURES)?.uint().map(|value| value as u16)
	}
}

// The parameter after a protocol's UUID in a protocol descriptor list: `{ { L2CAP, psm }, { RFCOMM, channel } }`.
fn protocol_parameter(list: &Element, protocol: u16) -> Option<u128> {
	list.elements()?.iter().find_map(|entry| {
		let elements = entry.elements()?;
		let Element::Uuid(id) = elements.first()? else { return None };
		if !id.same(&Uuid::U16(protocol)) {
			return None;
		}
		elements.get(1)?.uint()
	})
}

// -------------------------------------------------------------------------------------------------------------- client

/// What a client's response gave.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Answer {
	/// Ask again with this request: the server continues.
	More(Vec<u8>),
	/// The records the search found.
	Records(Vec<Record>),
	/// The server answered with an error, or something that is not an answer to this request.
	Failed(u16),
}

/// ONE SERVICE SEARCH ATTRIBUTE TRANSACTION as a client: the request, then each response, continuing until the server
/// says it is done, the bytes bounded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Search {
	transaction: u16,
	pattern: Vec<Uuid>,
	wanted: Vec<Wanted>,
	assembled: Vec<u8>,
}

impl Search {
	/// A search for one service class, every attribute asked for: the first request's bytes.
	pub fn new(transaction: u16, service: Uuid) -> (Search, Vec<u8>) {
		let search = Search { transaction, pattern: alloc::vec![service], wanted: alloc::vec![Wanted::Range(0x0000, 0xFFFF)], assembled: Vec::new() };
		let request = search.request(Vec::new());
		(search, request)
	}

	fn request(&self, continuation: Vec<u8>) -> Vec<u8> {
		encode(self.transaction, &Pdu::ServiceSearchAttributeRequest { pattern: self.pattern.clone(), max_bytes: 0x0400, wanted: self.wanted.clone(), continuation })
	}

	pub fn answer(&mut self, bytes: &[u8]) -> Answer {
		let Ok((transaction, pdu)) = decode(bytes) else { return Answer::Failed(error::INVALID_SYNTAX) };
		if transaction != self.transaction {
			return Answer::Failed(error::INVALID_SYNTAX);
		}
		match pdu {
			Pdu::ServiceSearchAttributeResponse { bytes, continuation } => {
				if self.assembled.len() + bytes.len() > MAX_ASSEMBLED {
					return Answer::Failed(error::INSUFFICIENT_RESOURCES);
				}
				self.assembled.extend_from_slice(&bytes);
				if !continuation.is_empty() {
					self.transaction = self.transaction.wrapping_add(1);
					return Answer::More(self.request(continuation));
				}
				let Ok((Element::Sequence(lists), rest)) = decode_element(&self.assembled) else { return Answer::Failed(error::INVALID_SYNTAX) };
				if !rest.is_empty() {
					return Answer::Failed(error::INVALID_SYNTAX);
				}
				Answer::Records(lists.iter().filter_map(Record::from_list).collect())
			}
			Pdu::Error { code } => Answer::Failed(code),
			_ => Answer::Failed(error::INVALID_SYNTAX),
		}
	}
}

// -------------------------------------------------------------------------------------------------------------- server

/// THE SERVER: the records of the roles this system offers while they are enabled, and the continuation of a response
/// longer than a peer's byte count - kept per transaction, an offset and the request it continues.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Server {
	records: Vec<Record>,
	// The response being continued: the request's bytes without its continuation, and the whole answer.
	pending: Option<(Vec<u8>, Vec<u8>)>,
}

impl Server {
	pub fn new() -> Server {
		Server::default()
	}

	/// Offer a record, or replace one with the same handle.
	pub fn offer(&mut self, record: Record) {
		self.records.retain(|held| held.handle != record.handle);
		self.records.push(record);
		self.records.sort_by_key(|record| record.handle);
	}

	pub fn withdraw(&mut self, handle: u32) {
		self.records.retain(|held| held.handle != handle);
	}

	pub fn records(&self) -> &[Record] {
		&self.records
	}

	/// ONE REQUEST, ANSWERED: the response's bytes.
	pub fn answer(&mut self, bytes: &[u8]) -> Vec<u8> {
		let transaction = if bytes.len() >= 3 { u16::from_be_bytes([bytes[1], bytes[2]]) } else { 0 };
		let Ok((transaction, pdu)) = decode(bytes) else { return encode(transaction, &Pdu::Error { code: error::INVALID_SYNTAX }) };
		match pdu {
			Pdu::ServiceSearchRequest { pattern, max_records, .. } => {
				// Into a fixed array and copied out, for the reason the decoder collects: no `Vec<u32>` grows here.
				let mut matched = [0u32; MAX_RECORDS];
				let mut count = 0;
				for record in self.records.iter().filter(|record| record.matches(&pattern)).take(usize::from(max_records).min(MAX_RECORDS)) {
					matched[count] = record.handle;
					count += 1;
				}
				let handles = matched[..count].to_vec();
				encode(transaction, &Pdu::ServiceSearchResponse { total: handles.len() as u16, handles, continuation: Vec::new() })
			}
			Pdu::ServiceAttributeRequest { handle, max_bytes, wanted, continuation } => {
				let Some(record) = self.records.iter().find(|record| record.handle == handle) else { return encode(transaction, &Pdu::Error { code: error::INVALID_HANDLE }) };
				let mut whole = Vec::new();
				encode_element(&record.list(&wanted), &mut whole);
				self.continue_response(transaction, bytes, whole, max_bytes, &continuation, false)
			}
			Pdu::ServiceSearchAttributeRequest { pattern, max_bytes, wanted, continuation } => {
				let lists: Vec<Element> = self.records.iter().filter(|record| record.matches(&pattern)).map(|record| record.list(&wanted)).collect();
				let mut whole = Vec::new();
				encode_element(&Element::Sequence(lists), &mut whole);
				self.continue_response(transaction, bytes, whole, max_bytes, &continuation, true)
			}
			_ => encode(transaction, &Pdu::Error { code: error::INVALID_SYNTAX }),
		}
	}

	// THE PART OF `whole` THE CONTINUATION ASKS FOR, at most `max_bytes` of it, and a continuation for the rest. The
	// continuation is a four-byte offset into the same answer to the same request; anything else is refused.
	fn continue_response(&mut self, transaction: u16, request: &[u8], whole: Vec<u8>, max_bytes: u16, continuation: &[u8], search: bool) -> Vec<u8> {
		let key = request[5..request.len() - 1 - continuation.len()].to_vec();
		let offset = if continuation.is_empty() {
			0
		} else {
			let valid = continuation.len() == 4 && self.pending.as_ref().is_some_and(|(held, answer)| *held == key && *answer == whole);
			if !valid {
				return encode(transaction, &Pdu::Error { code: error::INVALID_CONTINUATION });
			}
			u32::from_be_bytes([continuation[0], continuation[1], continuation[2], continuation[3]]) as usize
		};
		if offset > whole.len() || max_bytes < 7 {
			return encode(transaction, &Pdu::Error { code: if max_bytes < 7 { error::INVALID_SYNTAX } else { error::INVALID_CONTINUATION } });
		}
		let end = whole.len().min(offset + usize::from(max_bytes));
		let next = if end < whole.len() {
			self.pending = Some((key, whole.clone()));
			(end as u32).to_be_bytes().to_vec()
		} else {
			self.pending = None;
			Vec::new()
		};
		let part = whole[offset..end].to_vec();
		encode(transaction, &if search { Pdu::ServiceSearchAttributeResponse { bytes: part, continuation: next } } else { Pdu::ServiceAttributeResponse { bytes: part, continuation: next } })
	}
}

// ------------------------------------------------------------------------------------------- the records offered here

fn protocols(list: &[(u16, Option<Element>)]) -> Element {
	Element::Sequence(list.iter().map(|(protocol, parameter)| Element::Sequence(core::iter::once(Element::uuid16(*protocol)).chain(parameter.clone()).collect())).collect())
}

fn profile(id: u16, version: u16) -> Element {
	Element::Sequence(alloc::vec![Element::Sequence(alloc::vec![Element::uuid16(id), Element::u16(version)])])
}

fn browse() -> Element {
	Element::Sequence(alloc::vec![Element::uuid16(uuid::PUBLIC_BROWSE_ROOT)])
}

fn record(handle: u32, attributes: Vec<(u16, Element)>) -> Record {
	let mut all = alloc::vec![(attribute::SERVICE_RECORD_HANDLE, Element::u32(handle))];
	all.extend(attributes);
	all.sort_by_key(|(id, _)| *id);
	Record { handle, attributes: all }
}

/// HFP's Audio Gateway on an RFCOMM channel: version 1.8, the network attribute (able to reject a call) and the
/// supported features - three-way calling 0, the gateway's wideband speech bit 5.
pub fn hfp_audio_gateway(handle: u32, channel: u8, features: u16) -> Record {
	record(
		handle,
		alloc::vec![
			(attribute::SERVICE_CLASS_ID_LIST, Element::Sequence(alloc::vec![Element::uuid16(uuid::HANDSFREE_AUDIO_GATEWAY), Element::uuid16(uuid::GENERIC_AUDIO)])),
			(attribute::PROTOCOL_DESCRIPTOR_LIST, protocols(&[(uuid::L2CAP, None), (uuid::RFCOMM, Some(Element::u8(channel)))])),
			(attribute::BROWSE_GROUP_LIST, browse()),
			(attribute::PROFILE_DESCRIPTOR_LIST, profile(uuid::HANDSFREE, 0x0108)),
			(attribute::SERVICE_NAME, Element::text("Voice Gateway")),
			(attribute::NETWORK, Element::u8(1)),
			(attribute::SUPPORTED_FEATURES, Element::u16(features)),
		],
	)
}

/// HSP's Audio Gateway on an RFCOMM channel, version 1.2.
pub fn hsp_audio_gateway(handle: u32, channel: u8) -> Record {
	record(
		handle,
		alloc::vec![
			(attribute::SERVICE_CLASS_ID_LIST, Element::Sequence(alloc::vec![Element::uuid16(uuid::HEADSET_AUDIO_GATEWAY), Element::uuid16(uuid::GENERIC_AUDIO)])),
			(attribute::PROTOCOL_DESCRIPTOR_LIST, protocols(&[(uuid::L2CAP, None), (uuid::RFCOMM, Some(Element::u8(channel)))])),
			(attribute::BROWSE_GROUP_LIST, browse()),
			(attribute::PROFILE_DESCRIPTOR_LIST, profile(uuid::HEADSET, 0x0102)),
			(attribute::SERVICE_NAME, Element::text("Headset Gateway")),
		],
	)
}

/// A2DP's source or sink over AVDTP, version 1.3: the supported features (player or headphones / speaker).
pub fn a2dp(handle: u32, sink: bool, features: u16) -> Record {
	record(
		handle,
		alloc::vec![
			(attribute::SERVICE_CLASS_ID_LIST, Element::Sequence(alloc::vec![Element::uuid16(if sink { uuid::AUDIO_SINK } else { uuid::AUDIO_SOURCE })])),
			(attribute::PROTOCOL_DESCRIPTOR_LIST, protocols(&[(uuid::L2CAP, Some(Element::u16(crate::l2cap_bredr::psm::AVDTP))), (uuid::AVDTP, Some(Element::u16(0x0103)))])),
			(attribute::BROWSE_GROUP_LIST, browse()),
			(attribute::PROFILE_DESCRIPTOR_LIST, profile(uuid::ADVANCED_AUDIO_DISTRIBUTION, 0x0103)),
			(attribute::SUPPORTED_FEATURES, Element::u16(features)),
		],
	)
}

/// AVRCP's controller or target over AVCTP, version 1.6, the target with its browsing channel. A target claims no
/// player category: there is no media session (features bit 0 clear).
pub fn avrcp(handle: u32, target: bool, features: u16) -> Record {
	let class = if target { alloc::vec![Element::uuid16(uuid::AV_REMOTE_CONTROL_TARGET)] } else { alloc::vec![Element::uuid16(uuid::AV_REMOTE_CONTROL), Element::uuid16(uuid::AV_REMOTE_CONTROL_CONTROLLER)] };
	let mut attributes = alloc::vec![
		(attribute::SERVICE_CLASS_ID_LIST, Element::Sequence(class)),
		(attribute::PROTOCOL_DESCRIPTOR_LIST, protocols(&[(uuid::L2CAP, Some(Element::u16(crate::l2cap_bredr::psm::AVCTP))), (uuid::AVCTP, Some(Element::u16(0x0104)))])),
		(attribute::BROWSE_GROUP_LIST, browse()),
		(attribute::PROFILE_DESCRIPTOR_LIST, profile(uuid::AV_REMOTE_CONTROL, 0x0106)),
		(attribute::SUPPORTED_FEATURES, Element::u16(features)),
	];
	if target {
		attributes.push((attribute::ADDITIONAL_PROTOCOL_DESCRIPTOR_LISTS, Element::Sequence(alloc::vec![protocols(&[(uuid::L2CAP, Some(Element::u16(crate::l2cap_bredr::psm::AVCTP_BROWSING))), (uuid::AVCTP, Some(Element::u16(0x0104)))])])));
	}
	record(handle, attributes)
}

/// OPP's server over RFCOMM, and over L2CAP where `psm` names one, version 1.2, any format.
pub fn opp_server(handle: u32, channel: u8, psm: Option<u16>) -> Record {
	let mut attributes = alloc::vec![
		(attribute::SERVICE_CLASS_ID_LIST, Element::Sequence(alloc::vec![Element::uuid16(uuid::OBEX_OBJECT_PUSH)])),
		(attribute::PROTOCOL_DESCRIPTOR_LIST, protocols(&[(uuid::L2CAP, None), (uuid::RFCOMM, Some(Element::u8(channel))), (uuid::OBEX, None)])),
		(attribute::BROWSE_GROUP_LIST, browse()),
		(attribute::PROFILE_DESCRIPTOR_LIST, profile(uuid::OBEX_OBJECT_PUSH, 0x0102)),
		(attribute::SERVICE_NAME, Element::text("Object Push")),
		(attribute::SUPPORTED_FORMATS, Element::Sequence(alloc::vec![Element::u8(0xFF)])),
	];
	if let Some(psm) = psm {
		attributes.push((attribute::GOEP_L2CAP_PSM, Element::u16(psm)));
	}
	record(handle, attributes)
}
