//! Writing FORM/AIFF and FORM/AIFC, incrementally.
//!
//! Same shape as the RIFF encoder beside it - a header with sizes that are corrected at the end -
//! with two differences the format insists on. Everything is big-endian, including the sizes; and
//! the sample rate is an eighty-bit extended float, which is written here from the integer rate by
//! construction rather than converted through a floating-point type that does not exist on a
//! freestanding target.

use crate::Error;
use pcm::Format;
use pcm::encode::{Sink, SinkError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
	Unsupported,
	Invalid,
	TooLarge,
	Destination(SinkError),
}

impl From<SinkError> for EncodeError {
	fn from(error: SinkError) -> EncodeError {
		EncodeError::Destination(error)
	}
}

impl From<Error> for EncodeError {
	fn from(error: Error) -> EncodeError {
		match error {
			Error::Unsupported => EncodeError::Unsupported,
			Error::TooLarge => EncodeError::TooLarge,
			Error::Truncated | Error::Invalid => EncodeError::Invalid,
		}
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
	// FORM/AIFF: big-endian samples, no compression field. What every reader understands.
	Aiff { bits: u16 },
	// FORM/AIFC with `NONE` (big-endian) or `sowt` (little-endian). `sowt` is uncompressed audio in
	// the byte order the machine already has, which is why it exists and why it is worth writing.
	Aifc { bits: u16, little_endian: bool },
}

impl Output {
	const fn bits(self) -> u16 {
		match self {
			Output::Aiff { bits } | Output::Aifc { bits, .. } => bits,
		}
	}

	const fn compressed(self) -> bool {
		matches!(self, Output::Aifc { .. })
	}

	const fn little_endian(self) -> bool {
		matches!(self, Output::Aifc { little_endian: true, .. })
	}
}

pub struct Encoder<S: Sink> {
	sink: S,
	format: Format,
	output: Output,
	frame_bytes: usize,
	form_at: u64,
	frames_at: u64,
	ssnd_at: u64,
	frames: u64,
	data_bytes: u64,
}

impl<S: Sink> Encoder<S> {
	pub fn new(mut sink: S, format: Format, output: Output) -> Result<Encoder<S>, EncodeError> {
		let bits = output.bits();
		if !matches!(bits, 8 | 16 | 24 | 32) {
			return Err(EncodeError::Unsupported);
		}
		// The destination must be able to go back for the frame count and the two sizes, and it says
		// so now rather than after the audio has been written.
		sink.patch(0, &[])?;

		let channels = format.channels();
		let frame_bytes = channels as usize * (bits as usize / 8);
		// The compression name is a Pascal string, padded so the chunk stays even-aligned.
		let comm_body = if output.compressed() { 18 + 4 + 6 } else { 18 };

		sink.write(b"FORM")?;
		let form_at = sink.written();
		sink.write(&0u32.to_be_bytes())?;
		sink.write(if output.compressed() { b"AIFC" } else { b"AIFF" })?;
		if output.compressed() {
			// FVER, whose one field is the format's publication date. Readers check it; writing it
			// is four bytes and the alternative is a file some of them refuse.
			sink.write(b"FVER")?;
			sink.write(&4u32.to_be_bytes())?;
			sink.write(&0xA2805140u32.to_be_bytes())?;
		}
		sink.write(b"COMM")?;
		sink.write(&(comm_body as u32).to_be_bytes())?;
		sink.write(&(channels as u16).to_be_bytes())?;
		let frames_at = sink.written();
		sink.write(&0u32.to_be_bytes())?;
		sink.write(&bits.to_be_bytes())?;
		sink.write(&extended(format.rate()))?;
		if output.compressed() {
			// The four-character identifier, then its human name as a Pascal string padded to an
			// even length: six bytes either way, which is why `comm_body` can be a constant.
			sink.write(if output.little_endian() { b"sowt" } else { b"NONE" })?;
			sink.write(if output.little_endian() { b"\x04sowt\x00" } else { b"\x04none\x00" })?;
		}
		sink.write(b"SSND")?;
		let ssnd_at = sink.written();
		sink.write(&0u32.to_be_bytes())?;
		// Offset and block size, both zero: the audio starts immediately and is not block-aligned.
		sink.write(&0u32.to_be_bytes())?;
		sink.write(&0u32.to_be_bytes())?;

		Ok(Encoder { sink, format, output, frame_bytes, form_at, frames_at, ssnd_at, frames: 0, data_bytes: 0 })
	}

	pub fn push(&mut self, interleaved: &[i16]) -> Result<(), EncodeError> {
		let channels = self.format.channels() as usize;
		if interleaved.len() % channels != 0 {
			return Err(EncodeError::Invalid);
		}
		self.frames = self.frames.checked_add((interleaved.len() / channels) as u64).ok_or(EncodeError::TooLarge)?;

		let bits = self.output.bits();
		let width = bits as usize / 8;
		let little = self.output.little_endian();
		let mut staged = [0u8; 512];
		let mut filled = 0usize;
		for &sample in interleaved {
			if filled + width > staged.len() {
				self.sink.write(&staged[..filled])?;
				self.data_bytes += filled as u64;
				filled = 0;
			}
			match bits {
				// Eight-bit AIFF is SIGNED, where eight-bit RIFF is not. The two formats disagree and
				// the decoders beside these encoders already say so; this is the writing half.
				8 => staged[filled] = (((sample as i32 + 128) >> 8).clamp(-128, 127) as i8) as u8,
				16 => {
					let bytes = if little { sample.to_le_bytes() } else { sample.to_be_bytes() };
					staged[filled..filled + 2].copy_from_slice(&bytes);
				}
				24 => {
					let value = (sample as i32) << 8;
					let bytes = value.to_be_bytes();
					if little {
						staged[filled..filled + 3].copy_from_slice(&[bytes[3], bytes[2], bytes[1]]);
					} else {
						staged[filled..filled + 3].copy_from_slice(&bytes[1..4]);
					}
				}
				32 => {
					let value = (sample as i32) << 16;
					let bytes = if little { value.to_le_bytes() } else { value.to_be_bytes() };
					staged[filled..filled + 4].copy_from_slice(&bytes);
				}
				_ => return Err(EncodeError::Unsupported),
			}
			filled += width;
		}
		if filled != 0 {
			self.sink.write(&staged[..filled])?;
			self.data_bytes += filled as u64;
		}
		Ok(())
	}

	pub fn finish(mut self) -> Result<(S, u64), EncodeError> {
		if self.frames == 0 || self.data_bytes == 0 {
			return Err(EncodeError::Invalid);
		}
		if self.data_bytes != self.frames * self.frame_bytes as u64 {
			return Err(EncodeError::Invalid);
		}
		if self.data_bytes % 2 == 1 {
			self.sink.write(&[0])?;
		}
		// The SSND size counts its own two leading words, which is the one place this container
		// invites an off-by-eight.
		let ssnd_size = u32::try_from(self.data_bytes + 8).map_err(|_| EncodeError::TooLarge)?;
		let form_size = u32::try_from(self.sink.written().checked_sub(8).ok_or(EncodeError::Invalid)?).map_err(|_| EncodeError::TooLarge)?;
		let frames = u32::try_from(self.frames).map_err(|_| EncodeError::TooLarge)?;
		self.sink.patch(self.ssnd_at, &ssnd_size.to_be_bytes())?;
		self.sink.patch(self.frames_at, &frames.to_be_bytes())?;
		self.sink.patch(self.form_at, &form_size.to_be_bytes())?;
		Ok((self.sink, self.frames))
	}
}

// The sample rate as an eighty-bit extended float, built from the integer.
//
// The value is `mantissa * 2^(exponent - 16383 - 63)` with the mantissa normalised so its top bit is
// set - which for an integer means shifting it up until it is, and subtracting that shift from the
// exponent. No floating-point arithmetic is involved, so this produces the same ten bytes on every
// architecture and on a target with no floating-point unit at all.
fn extended(rate: u32) -> [u8; 10] {
	let mut bytes = [0u8; 10];
	if rate == 0 {
		return bytes;
	}
	let leading = 63 - (rate as u64).leading_zeros();
	let mantissa = (rate as u64) << (63 - leading);
	let exponent = 16_383u16 + leading as u16;
	bytes[..2].copy_from_slice(&exponent.to_be_bytes());
	bytes[2..].copy_from_slice(&mantissa.to_be_bytes());
	bytes
}
