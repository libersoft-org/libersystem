// BIG-ENDIAN, BOUNDED, BOTH WAYS: a writer that builds a command and patches its size, and a reader that
// refuses rather than reads past the end of what arrived.

use crate::{MAX_MESSAGE, Refused};
use alloc::vec::Vec;

/// A command under construction.
pub struct Writer {
	bytes: Vec<u8>,
}

impl Writer {
	/// A command with `tag` and `code`; the size is filled in by `finish`.
	pub fn command(tag: u16, code: u32) -> Writer {
		let mut writer = Writer { bytes: Vec::with_capacity(64) };
		writer.u16(tag);
		writer.u32(0);
		writer.u32(code);
		writer
	}

	/// A bare buffer, for a structure built on its own before it is placed in a TPM2B.
	pub fn empty() -> Writer {
		Writer { bytes: Vec::new() }
	}

	pub fn u8(&mut self, value: u8) -> &mut Writer {
		self.bytes.push(value);
		self
	}

	pub fn u16(&mut self, value: u16) -> &mut Writer {
		self.bytes.extend_from_slice(&value.to_be_bytes());
		self
	}

	pub fn u32(&mut self, value: u32) -> &mut Writer {
		self.bytes.extend_from_slice(&value.to_be_bytes());
		self
	}

	pub fn bytes(&mut self, value: &[u8]) -> &mut Writer {
		self.bytes.extend_from_slice(value);
		self
	}

	/// A sized buffer: its length as a `u16`, then its bytes.
	pub fn sized(&mut self, value: &[u8]) -> &mut Writer {
		self.u16(value.len() as u16);
		self.bytes(value)
	}

	pub fn len(&self) -> usize {
		self.bytes.len()
	}

	pub fn is_empty(&self) -> bool {
		self.bytes.is_empty()
	}

	/// The bytes written, for a structure that is not a command.
	pub fn into_bytes(self) -> Vec<u8> {
		self.bytes
	}

	/// The command, its size patched in - or `None` past the bound, which is never sent.
	pub fn finish(mut self) -> Option<Vec<u8>> {
		if self.bytes.len() > MAX_MESSAGE || self.bytes.len() < 10 {
			return None;
		}
		let size = (self.bytes.len() as u32).to_be_bytes();
		self.bytes[2..6].copy_from_slice(&size);
		Some(self.bytes)
	}
}

/// A cursor over bytes a TPM sent.
pub struct Reader<'a> {
	bytes: &'a [u8],
	at: usize,
}

impl<'a> Reader<'a> {
	pub fn new(bytes: &'a [u8]) -> Reader<'a> {
		Reader { bytes, at: 0 }
	}

	pub fn take(&mut self, count: usize) -> Result<&'a [u8], Refused> {
		let end = self.at.checked_add(count).ok_or(Refused::Short)?;
		let taken = self.bytes.get(self.at..end).ok_or(Refused::Short)?;
		self.at = end;
		Ok(taken)
	}

	pub fn u8(&mut self) -> Result<u8, Refused> {
		Ok(self.take(1)?[0])
	}

	pub fn u16(&mut self) -> Result<u16, Refused> {
		let bytes = self.take(2)?;
		Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
	}

	pub fn u32(&mut self) -> Result<u32, Refused> {
		let bytes = self.take(4)?;
		Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
	}

	pub fn u64(&mut self) -> Result<u64, Refused> {
		let bytes = self.take(8)?;
		let mut out = [0u8; 8];
		out.copy_from_slice(bytes);
		Ok(u64::from_be_bytes(out))
	}

	/// A sized buffer: a `u16` length and that many bytes, none of them past the end.
	pub fn sized(&mut self) -> Result<&'a [u8], Refused> {
		let length = self.u16()? as usize;
		self.take(length)
	}

	pub fn remaining(&self) -> usize {
		self.bytes.len() - self.at
	}

	pub fn rest(&mut self) -> &'a [u8] {
		let rest = &self.bytes[self.at..];
		self.at = self.bytes.len();
		rest
	}
}
