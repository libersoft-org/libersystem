//! The one bounded reader every byte of a font goes through.
//!
//! ONE PLACE, SO THERE IS ONE THING TO GET RIGHT. A parser that reads `u16` here and slices there
//! has as many bounds checks as it has reads, and the one that is missing is the one that matters.
//! Everything below answers `None` rather than panicking, so a caller cannot forget to check: the
//! `?` is the check.
//!
//! NO ARITHMETIC ON AN OFFSET THAT IS NOT CHECKED. Every addition here is `checked_add`; an offset
//! plus a length that wraps is precisely the shape of a font crafted to read somebody else's memory,
//! and on a 32-bit target it wraps with values a `u32` table can hold.

/// A bounded view over a font's bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reader<'a> {
	bytes: &'a [u8],
	offset: usize,
}

impl<'a> Reader<'a> {
	pub const fn new(bytes: &'a [u8]) -> Self {
		Self { bytes, offset: 0 }
	}

	/// A reader over a sub-range, or `None` when the range is not inside this one.
	pub fn slice(&self, offset: usize, length: usize) -> Option<Reader<'a>> {
		let end = offset.checked_add(length)?;
		let bytes = self.bytes.get(offset..end)?;
		Some(Reader { bytes, offset: 0 })
	}

	/// The bytes of a sub-range.
	pub fn bytes(&self, offset: usize, length: usize) -> Option<&'a [u8]> {
		let end = offset.checked_add(length)?;
		self.bytes.get(offset..end)
	}

	pub const fn len(&self) -> usize {
		self.bytes.len()
	}

	pub const fn is_empty(&self) -> bool {
		self.bytes.is_empty()
	}

	pub const fn position(&self) -> usize {
		self.offset
	}

	/// Move to an absolute position. Past the end is a refusal rather than a position nothing can
	/// read from - which is the same thing one step later, at a call site that has forgotten why.
	pub fn seek(&mut self, offset: usize) -> Option<()> {
		if offset > self.bytes.len() {
			return None;
		}
		self.offset = offset;
		Some(())
	}

	pub fn skip(&mut self, length: usize) -> Option<()> {
		let next = self.offset.checked_add(length)?;
		self.seek(next)
	}

	pub fn u8(&mut self) -> Option<u8> {
		let value = *self.bytes.get(self.offset)?;
		self.offset += 1;
		Some(value)
	}

	pub fn i8(&mut self) -> Option<i8> {
		self.u8().map(|value| value as i8)
	}

	pub fn u16(&mut self) -> Option<u16> {
		let end = self.offset.checked_add(2)?;
		let bytes = self.bytes.get(self.offset..end)?;
		self.offset = end;
		Some(u16::from_be_bytes([bytes[0], bytes[1]]))
	}

	pub fn i16(&mut self) -> Option<i16> {
		self.u16().map(|value| value as i16)
	}

	pub fn u24(&mut self) -> Option<u32> {
		let end = self.offset.checked_add(3)?;
		let bytes = self.bytes.get(self.offset..end)?;
		self.offset = end;
		Some(u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]))
	}

	pub fn u32(&mut self) -> Option<u32> {
		let end = self.offset.checked_add(4)?;
		let bytes = self.bytes.get(self.offset..end)?;
		self.offset = end;
		Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
	}

	pub fn i32(&mut self) -> Option<i32> {
		self.u32().map(|value| value as i32)
	}

	/// A four-character tag, as the format stores it.
	pub fn tag(&mut self) -> Option<[u8; 4]> {
		let end = self.offset.checked_add(4)?;
		let bytes = self.bytes.get(self.offset..end)?;
		self.offset = end;
		Some([bytes[0], bytes[1], bytes[2], bytes[3]])
	}

	/// The `u16` at an index of an array starting here, WITHOUT moving - which is what a table of
	/// offsets is read through.
	pub fn u16_at(&self, index: usize) -> Option<u16> {
		let offset = index.checked_mul(2)?;
		let start = self.offset.checked_add(offset)?;
		let end = start.checked_add(2)?;
		let bytes = self.bytes.get(start..end)?;
		Some(u16::from_be_bytes([bytes[0], bytes[1]]))
	}

	pub fn u32_at(&self, index: usize) -> Option<u32> {
		let offset = index.checked_mul(4)?;
		let start = self.offset.checked_add(offset)?;
		let end = start.checked_add(4)?;
		let bytes = self.bytes.get(start..end)?;
		Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
	}
}
