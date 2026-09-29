//! FRU INVENTORY (Platform Management FRU Information Storage Definition 1.0): the common header, and the chassis,
//! board and product areas' type/length fields - binary, BCD plus, six-bit ASCII packed, eight-bit - with every area's
//! checksum and every field's length checked against its area before a byte of it is believed. The multi-record area
//! is listed as present and not decoded.
//!
//! READ IN BOUNDED PIECES: Get FRU Inventory Area Info (Storage 0x10) for the size, then Read FRU Data (Storage 0x11)
//! `READ_CHUNK` bytes at a time, never past `MAX_BYTES`.

use crate::Request;
use alloc::string::String;
use alloc::vec::Vec;

pub const GET_FRU_INVENTORY_AREA_INFO: u8 = 0x10;
pub const READ_FRU_DATA: u8 = 0x11;
/// One Read FRU Data asks for at most this many bytes.
pub const READ_CHUNK: u8 = 32;
/// The most of one FRU device this layer reads.
pub const MAX_BYTES: usize = 2048;
/// The FRU devices the locator records may name that are read.
pub const MAX_DEVICES: usize = 8;

/// Why FRU data was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	Header(&'static str),
	Area(&'static str),
	Field(&'static str),
}

/// Get FRU Inventory Area Info's answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AreaInfo {
	pub size: u16,
	/// Whether the device is accessed by words rather than bytes.
	pub by_words: bool,
}

pub fn area_info(data: &[u8]) -> Option<AreaInfo> {
	Some(AreaInfo { size: crate::le16(data, 0)?, by_words: data.get(2)? & 1 != 0 })
}

pub fn info_request(device: u8) -> Request {
	Request::new(crate::netfn::STORAGE, GET_FRU_INVENTORY_AREA_INFO, &[device])
}

pub fn read_request(device: u8, offset: u16, count: u8) -> Request {
	let [low, high] = offset.to_le_bytes();
	Request::new(crate::netfn::STORAGE, READ_FRU_DATA, &[device, low, high, count])
}

/// Read FRU Data's answer: the count it says it returned, and exactly that many bytes.
pub fn read_response(data: &[u8]) -> Option<&[u8]> {
	let (&count, bytes) = data.split_first()?;
	(bytes.len() == count as usize).then_some(bytes)
}

/// A decoded type/length field.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Field {
	Text(String),
	Binary(Vec<u8>),
}

impl Field {
	pub fn text(&self) -> String {
		match self {
			Field::Text(text) => text.clone(),
			Field::Binary(bytes) => {
				let mut out = String::new();
				for byte in bytes {
					out.push_str(&alloc::format!("{byte:02x}"));
				}
				out
			}
		}
	}
}

/// The type/length byte's type: its top two bits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
	Binary,
	BcdPlus,
	SixBit,
	EightBit,
}

pub const fn kind(type_length: u8) -> Kind {
	match type_length >> 6 {
		0 => Kind::Binary,
		1 => Kind::BcdPlus,
		2 => Kind::SixBit,
		_ => Kind::EightBit,
	}
}

/// Decode `bytes` as `kind` says.
pub fn decode(kind: Kind, bytes: &[u8]) -> Field {
	match kind {
		Kind::Binary => Field::Binary(bytes.to_vec()),
		Kind::BcdPlus => {
			const DIGITS: &[u8; 16] = b"0123456789 -.???";
			let mut out = String::new();
			for byte in bytes {
				out.push(DIGITS[(byte >> 4) as usize] as char);
				out.push(DIGITS[(byte & 0x0F) as usize] as char);
			}
			Field::Text(out)
		}
		// FOUR CHARACTERS IN EVERY THREE BYTES, least significant bits first, each 0x20 above its six bits.
		Kind::SixBit => {
			let mut out = String::new();
			for chunk in bytes.chunks(3) {
				let word = chunk.iter().rev().fold(0u32, |word, byte| (word << 8) | u32::from(*byte));
				for index in 0..(chunk.len() * 8 / 6) {
					out.push(char::from(((word >> (6 * index)) & 0x3F) as u8 + 0x20));
				}
			}
			Field::Text(out)
		}
		Kind::EightBit => Field::Text(bytes.iter().map(|byte| char::from(*byte)).collect()),
	}
}

/// The end-of-fields marker: type 3, length 1.
pub const END: u8 = 0xC1;

/// Read one type/length field at `at` of `area`, bounded by `end`: the field and where the next begins. `None` at the
/// end marker.
fn field(area: &[u8], at: usize, end: usize) -> Result<Option<(Field, usize)>, Refusal> {
	let type_length = *area.get(at).filter(|_| at < end).ok_or(Refusal::Field("a field runs past its area without an end marker"))?;
	if type_length == END {
		return Ok(None);
	}
	let length = (type_length & 0x3F) as usize;
	let start = at + 1;
	if start + length > end {
		return Err(Refusal::Field("a field's length runs past its area"));
	}
	Ok(Some((decode(kind(type_length), &area[start..start + length]), start + length)))
}

/// A zero-sum checksum over `bytes`.
pub fn sums_to_zero(bytes: &[u8]) -> bool {
	bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) == 0
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Chassis {
	pub chassis_type: u8,
	pub part: Option<Field>,
	pub serial: Option<Field>,
	pub custom: Vec<Field>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Board {
	/// Minutes since 1996-01-01 00:00.
	pub manufactured: u32,
	pub manufacturer: Option<Field>,
	pub product: Option<Field>,
	pub serial: Option<Field>,
	pub part: Option<Field>,
	pub file_id: Option<Field>,
	pub custom: Vec<Field>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Product {
	pub manufacturer: Option<Field>,
	pub name: Option<Field>,
	pub part: Option<Field>,
	pub version: Option<Field>,
	pub serial: Option<Field>,
	pub asset: Option<Field>,
	pub file_id: Option<Field>,
	pub custom: Vec<Field>,
}

/// One FRU device's inventory.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Inventory {
	pub chassis: Option<Chassis>,
	pub board: Option<Board>,
	pub product: Option<Product>,
	pub multirecord: bool,
	/// An area refused while the rest was still read.
	pub refused: Vec<Refusal>,
}

/// The fixed fields and then the custom ones, up to the end marker: `fixed` fields, each `None` when the area ends
/// early.
fn fields(area: &[u8], mut at: usize, fixed: usize) -> Result<(Vec<Option<Field>>, Vec<Field>), Refusal> {
	let end = area.len() - 1;
	let mut named = Vec::new();
	let mut ended = false;
	for _ in 0..fixed {
		if ended {
			named.push(None);
			continue;
		}
		match field(area, at, end)? {
			Some((found, next)) => {
				named.push(Some(found));
				at = next;
			}
			None => {
				named.push(None);
				ended = true;
			}
		}
	}
	let mut custom = Vec::new();
	while !ended {
		match field(area, at, end)? {
			Some((found, next)) => {
				custom.push(found);
				at = next;
			}
			None => ended = true,
		}
	}
	Ok((named, custom))
}

// An area at `offset` (in multiples of eight): its version, its length, its checksum.
fn area(bytes: &[u8], offset: u8) -> Result<&[u8], Refusal> {
	let start = offset as usize * 8;
	let header = bytes.get(start..start + 2).ok_or(Refusal::Area("an area begins past the data"))?;
	if header[0] & 0x0F != 1 {
		return Err(Refusal::Area("an area of a format version other than 1"));
	}
	let length = header[1] as usize * 8;
	if length < 3 {
		return Err(Refusal::Area("an area too short to hold its own header"));
	}
	let area = bytes.get(start..start + length).ok_or(Refusal::Area("an area's length runs past the data"))?;
	if !sums_to_zero(area) {
		return Err(Refusal::Area("an area's checksum"));
	}
	Ok(area)
}

/// THE INVENTORY OF ONE FRU DEVICE: the common header checked, then each area it names - an area refused is recorded
/// and the others are still read.
pub fn inventory(bytes: &[u8]) -> Result<Inventory, Refusal> {
	let header = bytes.get(..8).ok_or(Refusal::Header("shorter than the common header"))?;
	if header[0] & 0x0F != 1 {
		return Err(Refusal::Header("a common header of a format version other than 1"));
	}
	if !sums_to_zero(header) {
		return Err(Refusal::Header("the common header's checksum"));
	}
	let mut found = Inventory { multirecord: header[5] != 0, ..Inventory::default() };
	if header[2] != 0 {
		match area(bytes, header[2]).and_then(|area| fields(area, 3, 2).map(|(named, custom)| (area[2], named, custom))) {
			Ok((chassis_type, mut named, custom)) => found.chassis = Some(Chassis { chassis_type, serial: named.pop().flatten(), part: named.pop().flatten(), custom }),
			Err(refusal) => found.refused.push(refusal),
		}
	}
	if header[3] != 0 {
		match area(bytes, header[3]).and_then(|area| {
			let manufactured = u32::from_le_bytes([*area.get(3).unwrap_or(&0), *area.get(4).unwrap_or(&0), *area.get(5).unwrap_or(&0), 0]);
			if area.len() < 7 {
				return Err(Refusal::Area("a board area too short for its date"));
			}
			fields(area, 6, 5).map(|(named, custom)| (manufactured, named, custom))
		}) {
			Ok((manufactured, named, custom)) => {
				let mut named = named.into_iter();
				found.board = Some(Board { manufactured, manufacturer: named.next().flatten(), product: named.next().flatten(), serial: named.next().flatten(), part: named.next().flatten(), file_id: named.next().flatten(), custom });
			}
			Err(refusal) => found.refused.push(refusal),
		}
	}
	if header[4] != 0 {
		match area(bytes, header[4]).and_then(|area| fields(area, 3, 7)) {
			Ok((named, custom)) => {
				let mut named = named.into_iter();
				found.product = Some(Product { manufacturer: named.next().flatten(), name: named.next().flatten(), part: named.next().flatten(), version: named.next().flatten(), serial: named.next().flatten(), asset: named.next().flatten(), file_id: named.next().flatten(), custom });
			}
			Err(refusal) => found.refused.push(refusal),
		}
	}
	Ok(found)
}
