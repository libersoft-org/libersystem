//! THE SYSTEM EVENT LOG (IPMI 2.0, 31 and 32): its count and state from Get SEL Info, its entries one per Get SEL
//! Entry, and the reserve-check-clear sequence that erases only what was counted.
//!
//! CLEAR ERASES ONLY WHAT WAS COUNTED: Reserve SEL takes a reservation, which the BMC cancels when an event is added,
//! and Clear SEL names it - so an event that arrives between the count and the clear cancels the clear, and nothing is
//! erased. The erasure is then polled to completion through Get SEL Info, which answers 0x81 while it is in progress.

use crate::Request;
use alloc::vec::Vec;

pub const GET_SEL_INFO: u8 = 0x40;
pub const RESERVE_SEL: u8 = 0x42;
pub const GET_SEL_ENTRY: u8 = 0x43;
pub const CLEAR_SEL: u8 = 0x47;

/// Entries in one page of the tool's reading.
pub const PAGE: usize = 16;
/// The first entry, and the ID that ends the log.
pub const FIRST: u16 = 0x0000;
pub const LAST: u16 = 0xFFFF;
/// Every SEL record is this long.
pub const RECORD: usize = 16;

/// Get SEL Info's answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Info {
	pub version: u8,
	pub entries: u16,
	pub free: u16,
	pub last_addition: u32,
	pub last_erase: u32,
	pub overflow: bool,
	pub reserve: bool,
	pub delete: bool,
}

pub fn info(data: &[u8]) -> Option<Info> {
	let support = *data.get(13)?;
	Some(Info { version: data[0], entries: crate::le16(data, 1)?, free: crate::le16(data, 3)?, last_addition: crate::le32(data, 5)?, last_erase: crate::le32(data, 9)?, overflow: support & 0x80 != 0, reserve: support & 0x02 != 0, delete: support & 0x08 != 0 })
}

pub fn info_request() -> Request {
	Request::new(crate::netfn::STORAGE, GET_SEL_INFO, &[])
}

pub fn reserve_request() -> Request {
	Request::new(crate::netfn::STORAGE, RESERVE_SEL, &[])
}

pub fn reservation(data: &[u8]) -> Option<u16> {
	crate::le16(data, 0)
}

/// Get SEL Entry for the whole record `id`: offset 0, which needs no reservation.
pub fn entry_request(id: u16) -> Request {
	let [i0, i1] = id.to_le_bytes();
	Request::new(crate::netfn::STORAGE, GET_SEL_ENTRY, &[0, 0, i0, i1, 0, 0xFF])
}

/// Get SEL Entry's answer: the next ID and the sixteen bytes.
pub fn entry_response(data: &[u8]) -> Option<(u16, [u8; RECORD])> {
	Some((crate::le16(data, 0)?, data.get(2..2 + RECORD)?.try_into().ok()?))
}

/// Clear SEL under `reservation`: to begin the erasure, or to ask its status.
pub fn clear_request(reservation: u16, begin: bool) -> Request {
	let [r0, r1] = reservation.to_le_bytes();
	Request::new(crate::netfn::STORAGE, CLEAR_SEL, &[r0, r1, b'C', b'L', b'R', if begin { 0xAA } else { 0x00 }])
}

/// Whether Clear SEL's answer says the erasure is complete.
pub fn clear_complete(data: &[u8]) -> Option<bool> {
	Some(data.first()? & 0x0F == 1)
}

/// One SEL record, decoded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Record {
	/// A system event (type 0x02).
	System { id: u16, timestamp: u32, generator: u16, evm: u8, sensor_type: u8, sensor: u8, event_type: u8, deassertion: bool, data: [u8; 3] },
	/// An OEM record with a timestamp (types 0xC0 to 0xDF).
	OemTimestamped { id: u16, record_type: u8, timestamp: u32, manufacturer: u32, data: Vec<u8> },
	/// An OEM record without one (0xE0 to 0xFF).
	Oem { id: u16, record_type: u8, data: Vec<u8> },
}

/// Why a record was refused: its type is none the specification defines.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Refused {
	pub id: u16,
	pub record_type: u8,
}

pub fn record(bytes: &[u8; RECORD]) -> Result<Record, Refused> {
	let id = u16::from_le_bytes([bytes[0], bytes[1]]);
	let record_type = bytes[2];
	let timestamp = u32::from_le_bytes([bytes[3], bytes[4], bytes[5], bytes[6]]);
	match record_type {
		0x02 => Ok(Record::System { id, timestamp, generator: u16::from_le_bytes([bytes[7], bytes[8]]), evm: bytes[9], sensor_type: bytes[10], sensor: bytes[11], event_type: bytes[12] & 0x7F, deassertion: bytes[12] & 0x80 != 0, data: [bytes[13], bytes[14], bytes[15]] }),
		0xC0..=0xDF => Ok(Record::OemTimestamped { id, record_type, timestamp, manufacturer: u32::from_le_bytes([bytes[7], bytes[8], bytes[9], 0]), data: bytes[10..].to_vec() }),
		0xE0..=0xFF => Ok(Record::Oem { id, record_type, data: bytes[3..].to_vec() }),
		_ => Err(Refused { id, record_type }),
	}
}
