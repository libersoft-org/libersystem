//! ACPI VALUES ON A WIRE: a data object - an integer, a string, a buffer, or a package of them to any depth up to a
//! bound - as bytes, for the node channel a driver evaluates methods over. The IDL carries the bytes; this module is
//! the one encoder and the one decoder, bounded, so a hostile peer cannot make either end allocate without limit.
//!
//! THE ENCODING, per value: one tag byte, then
//!   0x01 integer   eight bytes little-endian
//!   0x02 string    a u32 length, then that many bytes
//!   0x03 buffer    a u32 length, then that many bytes
//!   0x04 package   a u32 count, then that many values
//!   0x05 reference a u32 length, then the path's text
//!   0x00 none      (an uninitialized element)

use alloc::string::String;
use alloc::vec::Vec;

use crate::object::{Object, obj};

/// The deepest nesting either end accepts.
pub const MAX_DEPTH: usize = 8;
/// The most bytes a value may encode to.
pub const MAX_BYTES: usize = 64 * 1024;

/// A value as it travels: a reference is sent as the path it names.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Value {
	None,
	Integer(u64),
	String(String),
	Buffer(Vec<u8>),
	Package(Vec<Value>),
	Reference(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WireError {
	TooDeep,
	TooLarge,
	Truncated,
	UnknownTag(u8),
	/// Bytes left over after the value.
	Trailing,
}

pub fn encode(value: &Value) -> Result<Vec<u8>, WireError> {
	let mut out = Vec::new();
	put(value, &mut out, 0)?;
	Ok(out)
}

fn put(value: &Value, out: &mut Vec<u8>, depth: usize) -> Result<(), WireError> {
	if depth > MAX_DEPTH {
		return Err(WireError::TooDeep);
	}
	match value {
		Value::None => out.push(0x00),
		Value::Integer(value) => {
			out.push(0x01);
			out.extend_from_slice(&value.to_le_bytes());
		}
		Value::String(text) => bytes(0x02, text.as_bytes(), out),
		Value::Buffer(data) => bytes(0x03, data, out),
		Value::Reference(path) => bytes(0x05, path.as_bytes(), out),
		Value::Package(elements) => {
			out.push(0x04);
			out.extend_from_slice(&(elements.len() as u32).to_le_bytes());
			for element in elements {
				put(element, out, depth + 1)?;
			}
		}
	}
	if out.len() > MAX_BYTES {
		return Err(WireError::TooLarge);
	}
	Ok(())
}

fn bytes(tag: u8, data: &[u8], out: &mut Vec<u8>) {
	out.push(tag);
	out.extend_from_slice(&(data.len() as u32).to_le_bytes());
	out.extend_from_slice(data);
}

pub fn decode(bytes: &[u8]) -> Result<Value, WireError> {
	if bytes.len() > MAX_BYTES {
		return Err(WireError::TooLarge);
	}
	let mut at = 0usize;
	let value = take(bytes, &mut at, 0)?;
	if at != bytes.len() {
		return Err(WireError::Trailing);
	}
	Ok(value)
}

fn u32_at(bytes: &[u8], at: &mut usize) -> Result<u32, WireError> {
	let raw = bytes.get(*at..*at + 4).ok_or(WireError::Truncated)?;
	*at += 4;
	Ok(u32::from_le_bytes(raw.try_into().map_err(|_| WireError::Truncated)?))
}

fn take(bytes: &[u8], at: &mut usize, depth: usize) -> Result<Value, WireError> {
	if depth > MAX_DEPTH {
		return Err(WireError::TooDeep);
	}
	let tag = *bytes.get(*at).ok_or(WireError::Truncated)?;
	*at += 1;
	Ok(match tag {
		0x00 => Value::None,
		0x01 => {
			let raw = bytes.get(*at..*at + 8).ok_or(WireError::Truncated)?;
			*at += 8;
			Value::Integer(u64::from_le_bytes(raw.try_into().map_err(|_| WireError::Truncated)?))
		}
		0x02 | 0x03 | 0x05 => {
			let length = u32_at(bytes, at)? as usize;
			let data = bytes.get(*at..at.checked_add(length).ok_or(WireError::Truncated)?).ok_or(WireError::Truncated)?.to_vec();
			*at += length;
			match tag {
				0x02 => Value::String(data.iter().map(|&byte| byte as char).collect()),
				0x03 => Value::Buffer(data),
				_ => Value::Reference(data.iter().map(|&byte| byte as char).collect()),
			}
		}
		0x04 => {
			let count = u32_at(bytes, at)? as usize;
			// A COUNT IS BOUNDED BY THE BYTES LEFT - each element takes one at least - before anything is allocated.
			if count > bytes.len() - *at {
				return Err(WireError::Truncated);
			}
			let mut elements = Vec::with_capacity(count);
			for _ in 0..count {
				elements.push(take(bytes, at, depth + 1)?);
			}
			Value::Package(elements)
		}
		other => return Err(WireError::UnknownTag(other)),
	})
}

/// An interpreter object as a wire value; `path` names a node a reference or a name element leads to.
pub fn from_object(object: &Object, path: &mut dyn FnMut(&Object) -> Option<String>) -> Value {
	match object {
		Object::Integer(value) => Value::Integer(*value),
		Object::String(text) => Value::String(text.clone()),
		Object::Buffer(data) => Value::Buffer(data.clone()),
		Object::Package(elements) => Value::Package(elements.iter().map(|element| from_object(&element.borrow(), path)).collect()),
		Object::Uninitialized => Value::None,
		other => match path(other) {
			Some(text) => Value::Reference(text),
			None => Value::None,
		},
	}
}

/// A wire value as an argument: a reference becomes its path as a string.
pub fn to_object(value: &Value) -> Object {
	match value {
		Value::None => Object::Uninitialized,
		Value::Integer(value) => Object::Integer(*value),
		Value::String(text) | Value::Reference(text) => Object::String(text.clone()),
		Value::Buffer(data) => Object::Buffer(data.clone()),
		Value::Package(elements) => Object::Package(elements.iter().map(|element| obj(to_object(element))).collect()),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::vec;

	#[test]
	fn a_value_round_trips_and_hostile_bytes_are_refused() {
		let value = Value::Package(vec![
			Value::Integer(7),
			Value::String(String::from("BAT0")),
			Value::Buffer(vec![1, 2]),
			Value::Package(vec![Value::None, Value::Reference(String::from("\\_SB_.PCI0"))]),
		]);
		let bytes = encode(&value).unwrap();
		assert_eq!(decode(&bytes), Ok(value));
		// Nesting past the bound, both ways.
		let mut deep = Value::Integer(1);
		for _ in 0..=MAX_DEPTH + 1 {
			deep = Value::Package(vec![deep]);
		}
		assert_eq!(encode(&deep), Err(WireError::TooDeep));
		let mut raw = Vec::new();
		for _ in 0..=MAX_DEPTH + 1 {
			raw.extend_from_slice(&[0x04, 1, 0, 0, 0]);
		}
		raw.extend_from_slice(&[0x00]);
		assert_eq!(decode(&raw), Err(WireError::TooDeep));
		// A count that promises more than the bytes hold, a length past the end, a tag nobody defined, trailing bytes.
		assert_eq!(decode(&[0x04, 0xFF, 0xFF, 0xFF, 0x7F]), Err(WireError::Truncated));
		assert_eq!(decode(&[0x03, 10, 0, 0, 0, 1]), Err(WireError::Truncated));
		assert_eq!(decode(&[0x09]), Err(WireError::UnknownTag(9)));
		assert_eq!(decode(&[0x00, 0x00]), Err(WireError::Trailing));
	}
}
