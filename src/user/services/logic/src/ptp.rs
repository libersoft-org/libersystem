//! THE PICTURE TRANSFER PROTOCOL, READ ONCE, AT THE SERVICE BOUNDARY: containers, one transaction's bulk-IN
//! stream, and the datasets of the read-only subset MediaImportService uses. Everything here reads bytes a
//! device chose, so everything is bounded before it is allocated and checked before it is believed.
//!
//! A CONTAINER is a 12-byte header - length (including the header), type, code and transaction ID, all
//! little-endian - and a payload. A command or a response carries up to five 32-bit parameters, so neither is
//! longer than 32 bytes. A transaction is one command out, then on bulk-IN an optional data container for
//! that operation and one response, both under the command's transaction ID.
//!
//! THE BULK-IN STREAM IS PARSED AS IT ARRIVES. `Inbound` takes whatever a pull returned - a header split in
//! two, a data container's tail and the response in one piece - and says what each byte was. A data container
//! longer than the transaction admits is refused from its header, before a byte of its payload is kept, and
//! so is the length that means "more than four gigabytes". Nothing after the final response is accepted.
//!
//! THE DATASETS are read fallibly: a string that is not UCS-2, or a time that is not one, leaves its typed
//! field absent and the record marked partial, and the original bytes are kept by the caller untouched.

use alloc::string::String;
use alloc::vec::Vec;

pub const HEADER: usize = 12;
/// A command or a response: the header and five parameters.
pub const MAX_SHORT: usize = HEADER + 5 * 4;

pub const COMMAND: u16 = 1;
pub const DATA: u16 = 2;
pub const RESPONSE: u16 = 3;
pub const EVENT: u16 = 4;

pub const GET_DEVICE_INFO: u16 = 0x1001;
pub const OPEN_SESSION: u16 = 0x1002;
pub const CLOSE_SESSION: u16 = 0x1003;
pub const GET_STORAGE_IDS: u16 = 0x1004;
pub const GET_STORAGE_INFO: u16 = 0x1005;
pub const GET_OBJECT_HANDLES: u16 = 0x1007;
pub const GET_OBJECT_INFO: u16 = 0x1008;
pub const GET_OBJECT: u16 = 0x1009;

/// THE READ-ONLY SUBSET, and the only operations this service ever sends. A device that does not offer all
/// of them is not usable.
pub const REQUIRED: [u16; 8] = [GET_DEVICE_INFO, OPEN_SESSION, CLOSE_SESSION, GET_STORAGE_IDS, GET_STORAGE_INFO, GET_OBJECT_HANDLES, GET_OBJECT_INFO, GET_OBJECT];

pub const OK: u16 = 0x2001;
pub const SESSION_NOT_OPEN: u16 = 0x2003;
pub const INVALID_STORAGE_ID: u16 = 0x2008;
pub const INVALID_OBJECT_HANDLE: u16 = 0x2009;
pub const DEVICE_BUSY: u16 = 0x2019;
pub const SESSION_ALREADY_OPEN: u16 = 0x201e;

pub const CANCEL_TRANSACTION: u16 = 0x4001;
pub const OBJECT_ADDED: u16 = 0x4002;
pub const OBJECT_REMOVED: u16 = 0x4003;
pub const STORE_ADDED: u16 = 0x4004;
pub const STORE_REMOVED: u16 = 0x4005;
pub const OBJECT_INFO_CHANGED: u16 = 0x4007;
pub const DEVICE_INFO_CHANGED: u16 = 0x4008;
pub const STORE_FULL: u16 = 0x400a;
pub const DEVICE_RESET: u16 = 0x400b;
pub const STORAGE_INFO_CHANGED: u16 = 0x400c;
pub const UNREPORTED_STATUS: u16 = 0x400e;

/// Every storage, in a GetObjectHandles or GetNumObjects selector.
pub const ALL_STORAGES: u32 = 0xffff_ffff;
/// The ObjectCompressedSize that means "four gigabytes or more": no size at all.
pub const SIZE_UNKNOWN: u32 = 0xffff_ffff;
/// The container length that means the same.
pub const LENGTH_UNKNOWN: u32 = 0xffff_ffff;

/// Whether a format code is one the standard defines: the ancillary formats and the image formats, less the
/// codes the image range leaves reserved.
pub fn format_known(code: u16) -> bool {
	matches!(code, 0x3000..=0x300c) || (matches!(code, 0x3800..=0x3810) && code != 0x3806 && code != 0x380c)
}

/// One command container: the header and the parameters, at most five.
pub fn command(operation: u16, transaction: u32, params: &[u32]) -> Option<Vec<u8>> {
	if params.len() > 5 {
		return None;
	}
	let length = HEADER + 4 * params.len();
	let mut bytes = Vec::with_capacity(length);
	bytes.extend_from_slice(&(length as u32).to_le_bytes());
	bytes.extend_from_slice(&COMMAND.to_le_bytes());
	bytes.extend_from_slice(&operation.to_le_bytes());
	bytes.extend_from_slice(&transaction.to_le_bytes());
	for param in params {
		bytes.extend_from_slice(&param.to_le_bytes());
	}
	Some(bytes)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
	pub length: u32,
	pub kind: u16,
	pub code: u16,
	pub transaction: u32,
}

fn header(bytes: &[u8]) -> Header {
	Header { length: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]), kind: u16::from_le_bytes([bytes[4], bytes[5]]), code: u16::from_le_bytes([bytes[6], bytes[7]]), transaction: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Response {
	pub code: u16,
	pub params: [u32; 5],
	pub count: u8,
}

/// An event container: its code, transaction and up to three parameters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Event {
	pub code: u16,
	pub transaction: u32,
	pub params: [u32; 3],
	pub count: u8,
}

/// An event container, whole, as the transport delivered it.
pub fn event(bytes: &[u8]) -> Option<Event> {
	if bytes.len() < HEADER || bytes.len() > HEADER + 3 * 4 || (bytes.len() - HEADER) % 4 != 0 {
		return None;
	}
	let head = header(bytes);
	if head.kind != EVENT || head.length as usize != bytes.len() {
		return None;
	}
	let mut params = [0u32; 3];
	let count = (bytes.len() - HEADER) / 4;
	for (at, param) in params.iter_mut().enumerate().take(count) {
		let offset = HEADER + 4 * at;
		*param = u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]]);
	}
	Some(Event { code: head.code, transaction: head.transaction, params, count: count as u8 })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fault {
	/// A container that is not one: a type bulk-IN never carries, a length shorter than a header, a response
	/// that is not whole parameters.
	Malformed,
	/// Another transaction's ID, or a data container for another operation.
	Mismatch,
	/// A data phase this operation does not have, a second one, or bytes after the final response.
	Sequence,
	/// A data container longer than this transaction admits, refused from its header.
	TooLarge(u32),
	/// The length that means "four gigabytes or more".
	Unrepresentable,
}

/// What the bulk-IN stream said, piece by piece.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Piece<'a> {
	/// The data container's header, and how many payload bytes follow it.
	Data(u32),
	/// Payload bytes, in order.
	Payload(&'a [u8]),
	/// The final response.
	Response(Response),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
	Head,
	Payload(u32),
	Body(usize),
	Done,
}

/// ONE TRANSACTION'S BULK-IN STREAM, parsed as it arrives.
pub struct Inbound {
	operation: u16,
	transaction: u32,
	// The data phase this operation has, and the most payload it admits.
	data: bool,
	limit: u32,
	seen_data: bool,
	stage: Stage,
	held: [u8; MAX_SHORT],
	have: usize,
	// The response's code, from its header.
	code: u16,
}

impl Inbound {
	pub fn new(operation: u16, transaction: u32, data: bool, limit: u32) -> Inbound {
		Inbound { operation, transaction, data, limit, seen_data: false, stage: Stage::Head, held: [0; MAX_SHORT], have: 0, code: 0 }
	}

	pub fn done(&self) -> bool {
		self.stage == Stage::Done
	}

	/// Whether the data container's header has been read.
	pub fn seen_data(&self) -> bool {
		self.seen_data
	}

	/// Feed bytes as they arrived; `piece` hears what each was, in order. A fault stops the stream: nothing
	/// after it is looked at.
	pub fn feed(&mut self, mut input: &[u8], piece: &mut impl FnMut(Piece<'_>)) -> Result<(), Fault> {
		while !input.is_empty() {
			match self.stage {
				Stage::Done => return Err(Fault::Sequence),
				Stage::Payload(remaining) => {
					let take = input.len().min(remaining as usize);
					piece(Piece::Payload(&input[..take]));
					input = &input[take..];
					let left = remaining - take as u32;
					self.stage = if left == 0 { Stage::Head } else { Stage::Payload(left) };
				}
				Stage::Head => {
					let take = input.len().min(HEADER - self.have);
					self.held[self.have..self.have + take].copy_from_slice(&input[..take]);
					self.have += take;
					input = &input[take..];
					if self.have < HEADER {
						continue;
					}
					self.have = 0;
					let head = header(&self.held[..HEADER]);
					if head.transaction != self.transaction {
						return Err(Fault::Mismatch);
					}
					match head.kind {
						DATA => {
							if !self.data || self.seen_data {
								return Err(Fault::Sequence);
							}
							if head.code != self.operation {
								return Err(Fault::Mismatch);
							}
							if head.length == LENGTH_UNKNOWN {
								return Err(Fault::Unrepresentable);
							}
							if (head.length as usize) < HEADER {
								return Err(Fault::Malformed);
							}
							let payload = head.length - HEADER as u32;
							// REFUSED FROM THE HEADER, before a byte of the payload is kept.
							if payload > self.limit {
								return Err(Fault::TooLarge(payload));
							}
							self.seen_data = true;
							piece(Piece::Data(payload));
							self.stage = if payload == 0 { Stage::Head } else { Stage::Payload(payload) };
						}
						RESPONSE => {
							let length = head.length as usize;
							if !(HEADER..=MAX_SHORT).contains(&length) || (length - HEADER) % 4 != 0 {
								return Err(Fault::Malformed);
							}
							self.code = head.code;
							self.stage = Stage::Body(length);
							self.have = HEADER;
							if length == HEADER {
								self.respond(piece);
							}
						}
						_ => return Err(Fault::Malformed),
					}
				}
				Stage::Body(length) => {
					let take = input.len().min(length - self.have);
					self.held[self.have..self.have + take].copy_from_slice(&input[..take]);
					self.have += take;
					input = &input[take..];
					if self.have == length {
						self.respond(piece);
					}
				}
			}
		}
		Ok(())
	}

	fn respond(&mut self, piece: &mut impl FnMut(Piece<'_>)) {
		let Stage::Body(length) = self.stage else { return };
		let count = (length - HEADER) / 4;
		let mut params = [0u32; 5];
		for (at, param) in params.iter_mut().enumerate().take(count) {
			let offset = HEADER + 4 * at;
			*param = u32::from_le_bytes([self.held[offset], self.held[offset + 1], self.held[offset + 2], self.held[offset + 3]]);
		}
		self.stage = Stage::Done;
		self.have = 0;
		piece(Piece::Response(Response { code: self.code, params, count: count as u8 }));
	}
}

// ------------------------------------------------------------------ datasets

/// A cursor over a dataset. Every read is checked; a short dataset is `None`, never a zero.
pub struct Reader<'a> {
	bytes: &'a [u8],
	at: usize,
}

impl<'a> Reader<'a> {
	pub fn new(bytes: &'a [u8]) -> Reader<'a> {
		Reader { bytes, at: 0 }
	}

	pub fn remaining(&self) -> usize {
		self.bytes.len() - self.at
	}

	fn take(&mut self, count: usize) -> Option<&'a [u8]> {
		let end = self.at.checked_add(count)?;
		let taken = self.bytes.get(self.at..end)?;
		self.at = end;
		Some(taken)
	}

	pub fn u8(&mut self) -> Option<u8> {
		Some(self.take(1)?[0])
	}

	pub fn u16(&mut self) -> Option<u16> {
		let bytes = self.take(2)?;
		Some(u16::from_le_bytes([bytes[0], bytes[1]]))
	}

	pub fn u32(&mut self) -> Option<u32> {
		let bytes = self.take(4)?;
		Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
	}

	pub fn u64(&mut self) -> Option<u64> {
		let bytes = self.take(8)?;
		let mut value = [0u8; 8];
		value.copy_from_slice(bytes);
		Some(u64::from_le_bytes(value))
	}

	/// An array of 16-bit values: a 32-bit count, checked against what remains before anything is kept.
	pub fn u16s(&mut self) -> Option<Vec<u16>> {
		let count = self.u32()? as usize;
		if count.checked_mul(2)? > self.remaining() {
			return None;
		}
		(0..count).map(|_| self.u16()).collect()
	}

	/// A string: a character count including its terminator, then that many UCS-2 code units. The outer
	/// `None` is a dataset that ends inside it; the inner one is a string that is not text.
	pub fn string(&mut self) -> Option<Option<String>> {
		let count = usize::from(self.u8()?);
		let units = self.take(count * 2)?;
		if count == 0 {
			return Some(Some(String::new()));
		}
		let units: Vec<u16> = units.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
		// THE TERMINATOR IS PART OF THE COUNT, and the one place a NUL may be.
		let (last, text) = units.split_last()?;
		if *last != 0 || text.contains(&0) {
			return Some(None);
		}
		Some(char::decode_utf16(text.iter().copied()).collect::<Result<String, _>>().ok())
	}
}

/// A time as a PTP DateTime string has it: `YYYYMMDDThhmmss`, optionally tenths, then `Z`, an offset, or
/// nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Time {
	pub year: u16,
	pub month: u8,
	pub day: u8,
	pub hour: u8,
	pub minute: u8,
	pub second: u8,
	pub offset_minutes: Option<i32>,
}

fn digits(text: &[u8]) -> Option<u32> {
	if text.is_empty() || !text.iter().all(u8::is_ascii_digit) {
		return None;
	}
	Some(text.iter().fold(0u32, |value, digit| value * 10 + u32::from(digit - b'0')))
}

pub fn time(text: &str) -> Option<Time> {
	let bytes = text.as_bytes();
	if bytes.len() < 15 || bytes[8] != b'T' {
		return None;
	}
	let (year, month, day) = (digits(&bytes[0..4])?, digits(&bytes[4..6])?, digits(&bytes[6..8])?);
	let (hour, minute, second) = (digits(&bytes[9..11])?, digits(&bytes[11..13])?, digits(&bytes[13..15])?);
	if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 59 {
		return None;
	}
	let mut rest = &bytes[15..];
	if let Some(fraction) = rest.strip_prefix(b".") {
		let tenths = fraction.iter().take_while(|byte| byte.is_ascii_digit()).count();
		if tenths == 0 {
			return None;
		}
		rest = &fraction[tenths..];
	}
	let offset_minutes = match rest {
		[] => None,
		[b'Z'] => Some(0),
		[sign @ (b'+' | b'-'), zone @ ..] if zone.len() == 4 => {
			let (hours, minutes) = (digits(&zone[0..2])?, digits(&zone[2..4])?);
			if hours > 23 || minutes > 59 {
				return None;
			}
			let magnitude = (hours * 60 + minutes) as i32;
			Some(if *sign == b'-' { -magnitude } else { magnitude })
		}
		_ => return None,
	};
	Some(Time { year: year as u16, month: month as u8, day: day as u8, hour: hour as u8, minute: minute as u8, second: second as u8, offset_minutes })
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DeviceInfo {
	pub operations: Vec<u16>,
	pub manufacturer: String,
	pub model: String,
	pub version: String,
	pub serial: String,
}

impl DeviceInfo {
	/// Every operation of the read-only subset is offered.
	pub fn usable(&self) -> bool {
		REQUIRED.iter().all(|operation| self.operations.contains(operation))
	}
}

fn bounded(text: Option<String>, most: usize) -> String {
	let mut text = text.unwrap_or_default();
	if text.len() > most {
		let mut cut = most;
		while !text.is_char_boundary(cut) {
			cut -= 1;
		}
		text.truncate(cut);
	}
	text
}

/// A DeviceInfo dataset. The operations list is what decides whether a device is usable; the strings are
/// for a person, bounded to 64 bytes, and an unreadable one is empty.
pub fn device_info(bytes: &[u8]) -> Option<DeviceInfo> {
	let mut reader = Reader::new(bytes);
	let _standard = reader.u16()?;
	let _vendor = reader.u32()?;
	let _vendor_version = reader.u16()?;
	let _vendor_description = reader.string()?;
	let _mode = reader.u16()?;
	let operations = reader.u16s()?;
	let _events = reader.u16s()?;
	let _properties = reader.u16s()?;
	let _capture = reader.u16s()?;
	let _playback = reader.u16s()?;
	let manufacturer = bounded(reader.string()?, 64);
	let model = bounded(reader.string()?, 64);
	let version = bounded(reader.string()?, 64);
	let serial = bounded(reader.string()?, 64);
	Some(DeviceInfo { operations, manufacturer, model, version, serial })
}

/// A storage ID array: a count, then the IDs, at most `most` of them.
pub fn ids(bytes: &[u8], most: usize) -> Option<Vec<u32>> {
	let mut reader = Reader::new(bytes);
	let count = reader.u32()? as usize;
	if count > most || count * 4 != reader.remaining() {
		return None;
	}
	(0..count).map(|_| reader.u32()).collect()
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct StorageInfo {
	pub storage_type: u16,
	pub filesystem_type: u16,
	pub access: u16,
	pub capacity: u64,
	pub free: u64,
	pub description: String,
	pub label: String,
}

pub fn storage_info(bytes: &[u8]) -> Option<StorageInfo> {
	let mut reader = Reader::new(bytes);
	let storage_type = reader.u16()?;
	let filesystem_type = reader.u16()?;
	let access = reader.u16()?;
	let capacity = reader.u64()?;
	let free = reader.u64()?;
	let _free_images = reader.u32()?;
	let description = bounded(reader.string()?, 64);
	let label = bounded(reader.string()?, 64);
	Some(StorageInfo { storage_type, filesystem_type, access, capacity, free, description, label })
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ObjectInfo {
	pub storage: u32,
	pub format: u16,
	/// Absent for the "four gigabytes or more" sentinel.
	pub size: Option<u32>,
	/// Absent for no parent.
	pub parent: Option<u32>,
	pub filename: String,
	pub captured: Option<Time>,
	/// Every typed field was read. False when a string or a time could not be, or the dataset ended early.
	pub complete: bool,
}

/// The fixed part of an ObjectInfo dataset: everything before the filename.
pub const OBJECT_INFO_FIXED: usize = 52;

/// An ObjectInfo dataset, read as far as it can be. The fixed part must be there - without it nothing
/// about the object is known - and whatever follows is taken field by field: a string that is not text, a
/// time that is not a time, or a dataset that ends early leaves that field absent and the result partial.
pub fn object_info(bytes: &[u8]) -> Option<ObjectInfo> {
	let mut reader = Reader::new(bytes);
	let storage = reader.u32()?;
	let format = reader.u16()?;
	let _protection = reader.u16()?;
	let size = reader.u32()?;
	let _thumb_format = reader.u16()?;
	let _thumb_size = reader.u32()?;
	let _thumb_width = reader.u32()?;
	let _thumb_height = reader.u32()?;
	let _width = reader.u32()?;
	let _height = reader.u32()?;
	let _depth = reader.u32()?;
	let parent = reader.u32()?;
	let _association_type = reader.u16()?;
	let _association = reader.u32()?;
	let _sequence = reader.u32()?;
	let mut info = ObjectInfo { storage, format, size: (size != SIZE_UNKNOWN).then_some(size), parent: (parent != 0).then_some(parent), filename: String::new(), captured: None, complete: true };
	match reader.string() {
		Some(Some(name)) => info.filename = bounded(Some(name), 255),
		_ => {
			info.complete = false;
			return Some(info);
		}
	}
	match reader.string() {
		Some(Some(text)) if text.is_empty() => {}
		Some(Some(text)) => match time(&text) {
			Some(time) => info.captured = Some(time),
			None => info.complete = false,
		},
		_ => {
			info.complete = false;
			return Some(info);
		}
	}
	// The modification date and the keywords are not typed fields, but a dataset must still carry them.
	if !matches!(reader.string(), Some(Some(_))) || !matches!(reader.string(), Some(Some(_))) {
		info.complete = false;
	}
	Some(info)
}

/// The revision an object identity carries: FNV-1a over its ObjectInfo dataset as fetched, so a later fetch
/// that differs in any byte is a different revision.
pub fn revision(dataset: &[u8]) -> u32 {
	dataset.iter().fold(0x811c_9dc5u32, |hash, byte| (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193))
}

#[cfg(test)]
mod tests;
