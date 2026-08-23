// A most-significant-bit-first bit writer, which is the order every field in this format is
// written in.
//
// Bounded on purpose: an encoder handed a hostile frame count should hit a ceiling rather than take
// the machine down with it, and the ceiling is the one place that decision is made.

use super::EncodeError;
use alloc::vec::Vec;

// Sixty-four mebibytes of MP3 is about an hour at 128 kbit/s. A stream longer than that is a caller
// that meant something else.
const CEILING: usize = 64 * 1024 * 1024;

pub(crate) struct BitWriter {
	bytes: Vec<u8>,
	// The byte being filled, and how many of its bits are used.
	partial: u8,
	used: u8,
}

impl BitWriter {
	pub(crate) fn new() -> BitWriter {
		BitWriter { bytes: Vec::new(), partial: 0, used: 0 }
	}

	pub(crate) fn put(&mut self, count: u8, value: u32) -> Result<(), EncodeError> {
		if count > 32 || (count < 32 && value >= (1u32 << count)) {
			return Err(EncodeError::Invalid);
		}
		for index in (0..count).rev() {
			let bit = ((value >> index) & 1) as u8;
			self.partial = (self.partial << 1) | bit;
			self.used += 1;
			if self.used == 8 {
				if self.bytes.len() >= CEILING {
					return Err(EncodeError::TooLarge);
				}
				self.bytes.push(self.partial);
				self.partial = 0;
				self.used = 0;
			}
		}
		Ok(())
	}

	// Pad to the next byte boundary with zeros. A frame is a whole number of bytes, and the bits
	// between the last codeword and the frame's end are what the format calls stuffing.
	pub(crate) fn align(&mut self) -> Result<(), EncodeError> {
		if self.used != 0 {
			let padding = 8 - self.used;
			self.put(padding, 0)?;
		}
		Ok(())
	}

	pub(crate) fn into_bytes(mut self) -> Result<Vec<u8>, EncodeError> {
		self.align()?;
		Ok(self.bytes)
	}
}
