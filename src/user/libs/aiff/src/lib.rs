#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use pcm::Format;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Truncated,
	Invalid,
	Unsupported,
	TooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata {
	pub rate: u32,
	pub channels: u8,
	pub bits_per_sample: u16,
	pub frames: u64,
	pub duration_ms: u64,
}

#[derive(Clone, Copy)]
enum Endian {
	Big,
	Little,
}

pub struct Aiff<'a> {
	format: Format,
	bits_per_sample: u16,
	endian: Endian,
	frame_bytes: usize,
	data: &'a [u8],
	metadata: Metadata,
}

impl<'a> Aiff<'a> {
	pub fn parse(bytes: &'a [u8]) -> Result<Aiff<'a>, Error> {
		if bytes.len() < 12 {
			return Err(Error::Truncated);
		}
		if &bytes[..4] != b"FORM" {
			return Err(Error::Invalid);
		}
		let form_len = read_u32(bytes, 4)? as usize;
		let form_end = form_len.checked_add(8).ok_or(Error::TooLarge)?;
		if form_end > bytes.len() {
			return Err(Error::Truncated);
		}
		let aifc = match &bytes[8..12] {
			b"AIFF" => false,
			b"AIFC" => true,
			_ => return Err(Error::Invalid),
		};
		let mut cursor = 12usize;
		let mut common = None;
		let mut sound = None;
		while cursor < form_end {
			let header = bytes.get(cursor..cursor + 8).ok_or(Error::Truncated)?;
			let len = u32::from_be_bytes(header[4..8].try_into().map_err(|_| Error::Truncated)?) as usize;
			let start = cursor.checked_add(8).ok_or(Error::TooLarge)?;
			let end = start.checked_add(len).ok_or(Error::TooLarge)?;
			let body = bytes.get(start..end).filter(|_| end <= form_end).ok_or(Error::Truncated)?;
			match &header[..4] {
				b"COMM" => {
					if common.is_some() {
						return Err(Error::Invalid);
					}
					common = Some(parse_common(body, aifc)?);
				}
				b"SSND" => {
					if sound.is_some() || body.len() < 8 {
						return Err(Error::Invalid);
					}
					let offset = read_u32(body, 0)? as usize;
					let data_start = 8usize.checked_add(offset).ok_or(Error::TooLarge)?;
					sound = Some(body.get(data_start..).ok_or(Error::Truncated)?);
				}
				_ => {}
			}
			cursor = end.checked_add(len & 1).ok_or(Error::TooLarge)?;
			if cursor > form_end {
				return Err(Error::Truncated);
			}
		}
		let common = common.ok_or(Error::Invalid)?;
		let data = sound.ok_or(Error::Invalid)?;
		let expected = usize::try_from(common.frames).map_err(|_| Error::TooLarge)?.checked_mul(common.frame_bytes).ok_or(Error::TooLarge)?;
		if data.len() != expected || data.is_empty() {
			return Err(Error::Invalid);
		}
		let duration_ms = common.frames.checked_mul(1_000).ok_or(Error::TooLarge)? / common.format.rate() as u64;
		let metadata = Metadata { rate: common.format.rate(), channels: common.format.channels(), bits_per_sample: common.bits_per_sample, frames: common.frames, duration_ms };
		Ok(Aiff { format: common.format, bits_per_sample: common.bits_per_sample, endian: common.endian, frame_bytes: common.frame_bytes, data, metadata })
	}

	pub const fn metadata(&self) -> Metadata {
		self.metadata
	}

	pub const fn format(&self) -> Format {
		self.format
	}

	pub fn decoder(&self) -> Decoder<'_> {
		Decoder { aiff: self, frame: 0 }
	}
}

struct Common {
	format: Format,
	bits_per_sample: u16,
	frames: u64,
	frame_bytes: usize,
	endian: Endian,
}

fn parse_common(bytes: &[u8], aifc: bool) -> Result<Common, Error> {
	if bytes.len() < 18 || aifc && bytes.len() < 22 {
		return Err(Error::Truncated);
	}
	let channels_raw = read_u16(bytes, 0)?;
	let channels = u8::try_from(channels_raw).map_err(|_| Error::Unsupported)?;
	let frames = read_u32(bytes, 2)? as u64;
	let bits_per_sample = read_u16(bytes, 6)?;
	if frames == 0 || !matches!(bits_per_sample, 8 | 16 | 24 | 32) {
		return Err(Error::Unsupported);
	}
	let rate = extended_rate(bytes.get(8..18).ok_or(Error::Truncated)?)?;
	let format = Format::new(rate, channels).ok_or(Error::Unsupported)?;
	let endian = if aifc {
		match bytes.get(18..22) {
			Some(b"NONE") => Endian::Big,
			Some(b"sowt") => Endian::Little,
			_ => return Err(Error::Unsupported),
		}
	} else {
		Endian::Big
	};
	let frame_bytes = (channels as usize).checked_mul(bits_per_sample as usize / 8).ok_or(Error::TooLarge)?;
	Ok(Common { format, bits_per_sample, frames, frame_bytes, endian })
}

fn extended_rate(bytes: &[u8]) -> Result<u32, Error> {
	if bytes.len() != 10 {
		return Err(Error::Truncated);
	}
	let exponent = u16::from_be_bytes([bytes[0], bytes[1]]);
	if exponent & 0x8000 != 0 || exponent == 0 || exponent == 0x7fff {
		return Err(Error::Unsupported);
	}
	let mantissa = u64::from_be_bytes(bytes[2..10].try_into().map_err(|_| Error::Truncated)?);
	if mantissa & (1u64 << 63) == 0 {
		return Err(Error::Invalid);
	}
	let shift = exponent as i32 - 16_383 - 63;
	let value = if shift >= 0 {
		(mantissa as u128).checked_shl(shift as u32).ok_or(Error::TooLarge)?
	} else {
		let divisor_shift = (-shift) as u32;
		if divisor_shift >= 128 {
			return Err(Error::Unsupported);
		}
		let divisor = 1u128 << divisor_shift;
		if mantissa as u128 % divisor != 0 {
			return Err(Error::Unsupported);
		}
		mantissa as u128 / divisor
	};
	u32::try_from(value).map_err(|_| Error::TooLarge)
}

pub struct Decoder<'a> {
	aiff: &'a Aiff<'a>,
	frame: u64,
}

impl Decoder<'_> {
	pub const fn remaining_frames(&self) -> u64 {
		self.aiff.metadata.frames - self.frame
	}

	pub fn read_i16_le(&mut self, max_frames: usize, output: &mut Vec<u8>) -> Result<usize, Error> {
		if max_frames == 0 {
			return Err(Error::Invalid);
		}
		let frames = usize::try_from(self.remaining_frames().min(max_frames as u64)).map_err(|_| Error::TooLarge)?;
		if frames == 0 {
			output.clear();
			return Ok(0);
		}
		let channels = self.aiff.format.channels() as usize;
		let output_len = frames.checked_mul(channels).and_then(|samples| samples.checked_mul(2)).ok_or(Error::TooLarge)?;
		output.clear();
		output.try_reserve_exact(output_len).map_err(|_| Error::TooLarge)?;
		let start = usize::try_from(self.frame).map_err(|_| Error::TooLarge)?.checked_mul(self.aiff.frame_bytes).ok_or(Error::TooLarge)?;
		let len = frames.checked_mul(self.aiff.frame_bytes).ok_or(Error::TooLarge)?;
		let source = self.aiff.data.get(start..start + len).ok_or(Error::Truncated)?;
		let sample_bytes = self.aiff.bits_per_sample as usize / 8;
		for sample in source.chunks_exact(sample_bytes) {
			let value = match (self.aiff.bits_per_sample, self.aiff.endian) {
				(8, _) => (sample[0] as i8 as i16) << 8,
				(16, Endian::Big) => i16::from_be_bytes([sample[0], sample[1]]),
				(16, Endian::Little) => i16::from_le_bytes([sample[0], sample[1]]),
				(24, Endian::Big) => (i32::from_be_bytes([if sample[0] & 0x80 != 0 { 0xff } else { 0 }, sample[0], sample[1], sample[2]]) >> 8) as i16,
				(24, Endian::Little) => (i32::from_le_bytes([sample[0], sample[1], sample[2], if sample[2] & 0x80 != 0 { 0xff } else { 0 }]) >> 8) as i16,
				(32, Endian::Big) => (i32::from_be_bytes([sample[0], sample[1], sample[2], sample[3]]) >> 16) as i16,
				(32, Endian::Little) => (i32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]) >> 16) as i16,
				_ => return Err(Error::Unsupported),
			};
			output.extend_from_slice(&value.to_le_bytes());
		}
		self.frame += frames as u64;
		Ok(frames)
	}
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, Error> {
	Ok(u16::from_be_bytes(bytes.get(offset..offset + 2).ok_or(Error::Truncated)?.try_into().map_err(|_| Error::Truncated)?))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, Error> {
	Ok(u32::from_be_bytes(bytes.get(offset..offset + 4).ok_or(Error::Truncated)?.try_into().map_err(|_| Error::Truncated)?))
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;
