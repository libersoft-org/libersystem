//! CONVERSIONS between the data types, as the specification states them: implicit ones for operands and stores,
//! and the explicit `ToInteger`, `ToBuffer`, `ToHexString`, `ToDecimalString` and `ToString`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::Error;
use crate::object::Object;

/// An integer from a data object, as an operand is converted: a string read as hexadecimal up to its first
/// character that is not a hex digit, a buffer's first bytes little-endian.
pub fn to_integer(object: &Object, int64: bool) -> Result<u64, Error> {
	let width = if int64 { 8 } else { 4 };
	let value = match object {
		Object::Integer(value) => *value,
		Object::String(text) => {
			let text = text.trim_start();
			let digits = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")).unwrap_or(text);
			let mut value: u64 = 0;
			for byte in digits.bytes().take(if int64 { 16 } else { 8 }) {
				let Some(digit) = (byte as char).to_digit(16) else { break };
				value = (value << 4) | digit as u64;
			}
			value
		}
		Object::Buffer(bytes) => {
			let mut value: u64 = 0;
			for (at, byte) in bytes.iter().take(width).enumerate() {
				value |= (*byte as u64) << (8 * at);
			}
			value
		}
		other => return Err(Error::Type(format!("an integer was needed, not a {}", other.type_name()))),
	};
	Ok(mask(value, int64))
}

/// `ToInteger`: a string read as decimal, or as hexadecimal after `0x`.
pub fn explicit_integer(object: &Object, int64: bool) -> Result<u64, Error> {
	match object {
		Object::String(text) => {
			let text = text.trim();
			let (digits, radix) = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
				Some(hex) => (hex, 16),
				None => (text, 10),
			};
			let mut value: u64 = 0;
			for byte in digits.bytes() {
				let Some(digit) = (byte as char).to_digit(radix) else { break };
				value = value.wrapping_mul(radix as u64).wrapping_add(digit as u64);
			}
			Ok(mask(value, int64))
		}
		other => to_integer(other, int64),
	}
}

pub fn mask(value: u64, int64: bool) -> u64 {
	if int64 { value } else { value & 0xFFFF_FFFF }
}

/// A buffer from a data object: an integer's bytes little-endian at the integer width, a string's bytes with its
/// null terminator.
pub fn to_buffer(object: &Object, int64: bool) -> Result<Vec<u8>, Error> {
	match object {
		Object::Integer(value) => Ok(if int64 { value.to_le_bytes().to_vec() } else { (*value as u32).to_le_bytes().to_vec() }),
		Object::String(text) => {
			let mut bytes = text.as_bytes().to_vec();
			if !bytes.is_empty() {
				bytes.push(0);
			}
			Ok(bytes)
		}
		Object::Buffer(bytes) => Ok(bytes.clone()),
		other => Err(Error::Type(format!("a buffer was needed, not a {}", other.type_name()))),
	}
}

/// A string from a data object, as an operand is converted: an integer in hexadecimal at the integer width, a
/// buffer as its bytes in hexadecimal separated by spaces.
pub fn to_string(object: &Object, int64: bool) -> Result<String, Error> {
	match object {
		Object::Integer(value) => Ok(if int64 { format!("{value:016X}") } else { format!("{:08X}", *value as u32) }),
		Object::String(text) => Ok(text.clone()),
		Object::Buffer(bytes) => Ok(bytes.iter().map(|byte| format!("{byte:02X}")).collect::<Vec<_>>().join(" ")),
		other => Err(Error::Type(format!("a string was needed, not a {}", other.type_name()))),
	}
}

/// `ToHexString`.
pub fn hex_string(object: &Object, int64: bool) -> Result<String, Error> {
	match object {
		Object::Integer(value) => Ok(format!("0x{:X}", mask(*value, int64))),
		Object::String(text) => Ok(text.clone()),
		Object::Buffer(bytes) => Ok(bytes.iter().map(|byte| format!("0x{byte:02X}")).collect::<Vec<_>>().join(",")),
		other => Err(Error::Type(format!("a hex string cannot be made of a {}", other.type_name()))),
	}
}

/// `ToDecimalString`.
pub fn decimal_string(object: &Object, int64: bool) -> Result<String, Error> {
	match object {
		Object::Integer(value) => Ok(format!("{}", mask(*value, int64))),
		Object::String(text) => Ok(text.clone()),
		Object::Buffer(bytes) => Ok(bytes.iter().map(|byte| format!("{byte}")).collect::<Vec<_>>().join(",")),
		other => Err(Error::Type(format!("a decimal string cannot be made of a {}", other.type_name()))),
	}
}

/// `ToString`: a buffer's bytes as characters up to a null or `length`.
pub fn buffer_to_string(bytes: &[u8], length: Option<usize>) -> String {
	let limit = length.unwrap_or(bytes.len()).min(bytes.len());
	bytes[..limit].iter().take_while(|&&byte| byte != 0).map(|&byte| byte as char).collect()
}

/// A value converted to the type of a TARGET it is stored into: an integer, a string or a buffer keep their type.
/// A buffer target keeps its length - a shorter source zero-filled, a longer one cut.
pub fn to_target_type(target: &Object, source: &Object, int64: bool) -> Result<Object, Error> {
	Ok(match target {
		Object::Integer(_) => Object::Integer(to_integer(source, int64)?),
		Object::String(_) => Object::String(match source {
			Object::Buffer(bytes) => buffer_to_string(bytes, None),
			other => to_string(other, int64)?,
		}),
		Object::Buffer(existing) => {
			let mut bytes = match source {
				Object::String(text) => text.as_bytes().to_vec(),
				other => to_buffer(other, int64)?,
			};
			bytes.resize(existing.len(), 0);
			Object::Buffer(bytes)
		}
		_ => crate::object::copy(source),
	})
}

pub fn bcd_to_integer(value: u64) -> u64 {
	let mut out: u64 = 0;
	let mut scale: u64 = 1;
	let mut rest = value;
	while rest != 0 {
		out = out.wrapping_add((rest & 0xF).wrapping_mul(scale));
		scale = scale.wrapping_mul(10);
		rest >>= 4;
	}
	out
}

pub fn integer_to_bcd(value: u64) -> u64 {
	let mut out: u64 = 0;
	let mut shift = 0;
	let mut rest = value;
	while rest != 0 && shift < 64 {
		out |= (rest % 10) << shift;
		shift += 4;
		rest /= 10;
	}
	out
}
