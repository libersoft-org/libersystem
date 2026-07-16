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
mod tests {
	use super::*;
	use alloc::vec;

	fn extended(rate: u32) -> [u8; 10] {
		let leading = 31 - rate.leading_zeros();
		let exponent = 16_383 + leading;
		let mantissa = (rate as u64) << (63 - leading);
		let mut bytes = [0u8; 10];
		bytes[..2].copy_from_slice(&(exponent as u16).to_be_bytes());
		bytes[2..].copy_from_slice(&mantissa.to_be_bytes());
		bytes
	}

	fn aiff(aifc: bool, little: bool, bits: u16, channels: u16, rate: u32, samples: &[u8]) -> Vec<u8> {
		let frame_bytes = channels as usize * bits as usize / 8;
		let frames = samples.len() / frame_bytes;
		let mut body = Vec::new();
		body.extend_from_slice(if aifc { b"AIFC" } else { b"AIFF" });
		body.extend_from_slice(b"COMM");
		body.extend_from_slice(&(if aifc { 22u32 } else { 18u32 }).to_be_bytes());
		body.extend_from_slice(&channels.to_be_bytes());
		body.extend_from_slice(&(frames as u32).to_be_bytes());
		body.extend_from_slice(&bits.to_be_bytes());
		body.extend_from_slice(&extended(rate));
		if aifc {
			body.extend_from_slice(if little { b"sowt" } else { b"NONE" });
		}
		body.extend_from_slice(b"SSND");
		body.extend_from_slice(&(samples.len() as u32 + 8).to_be_bytes());
		body.extend_from_slice(&0u32.to_be_bytes());
		body.extend_from_slice(&0u32.to_be_bytes());
		body.extend_from_slice(samples);
		if samples.len() & 1 != 0 {
			body.push(0);
		}
		let mut output = b"FORM".to_vec();
		output.extend_from_slice(&(body.len() as u32).to_be_bytes());
		output.extend_from_slice(&body);
		output
	}

	#[test]
	fn decodes_big_endian_aiff_and_little_endian_aifc_in_chunks() {
		for (bytes, expected) in [
			(aiff(false, false, 16, 1, 8_000, &[0x12, 0x34, 0xff, 0xfe]), vec![0x34, 0x12, 0xfe, 0xff]),
			(aiff(true, true, 16, 1, 8_000, &[0x34, 0x12, 0xfe, 0xff]), vec![0x34, 0x12, 0xfe, 0xff]),
		] {
			let parsed = Aiff::parse(&bytes).unwrap();
			assert_eq!(parsed.metadata().frames, 2);
			let mut decoder = parsed.decoder();
			let mut output = Vec::new();
			assert_eq!(decoder.read_i16_le(1, &mut output), Ok(1));
			assert_eq!(&output, &expected[..2]);
			assert_eq!(decoder.read_i16_le(8, &mut output), Ok(1));
			assert_eq!(&output, &expected[2..]);
		}
	}

	#[test]
	fn converts_signed_8_24_and_32_bit_samples() {
		for (bits, source, expected) in [
			(8, vec![0x80, 0, 0x7f], vec![0, 0x80, 0, 0, 0, 0x7f]),
			(24, vec![0x80, 0, 0, 0, 0, 0, 0x7f, 0xff, 0xff], vec![0, 0x80, 0, 0, 0xff, 0x7f]),
			(32, vec![0x80, 0, 0, 0, 0, 0, 0, 0, 0x7f, 0xff, 0xff, 0xff], vec![0, 0x80, 0, 0, 0xff, 0x7f]),
		] {
			let bytes = aiff(false, false, bits, 1, 8_000, &source);
			let parsed = Aiff::parse(&bytes).unwrap();
			let mut output = Vec::new();
			assert_eq!(parsed.decoder().read_i16_le(16, &mut output), Ok(3));
			assert_eq!(output, expected);
		}
	}

	#[test]
	fn rejects_fractional_or_unsupported_rates_codecs_and_lengths() {
		let mut fractional = extended(8_000);
		fractional[9] = 1;
		assert_eq!(extended_rate(&fractional), Err(Error::Unsupported));
		assert!(matches!(Aiff::parse(b"FORM"), Err(Error::Truncated)));
		let mut unsupported = aiff(true, false, 16, 1, 8_000, &[0, 0]);
		unsupported[38..42].copy_from_slice(b"fl32");
		assert!(matches!(Aiff::parse(&unsupported), Err(Error::Unsupported)));
		let mut truncated = aiff(false, false, 16, 1, 8_000, &[0, 0]);
		truncated.pop();
		assert!(matches!(Aiff::parse(&truncated), Err(Error::Truncated)));
	}

	#[test]
	fn decodes_staged_ffmpeg_aiff_and_aifc() {
		for bytes in [include_bytes!("../../../volume/test.aiff").as_slice(), include_bytes!("../../../volume/test.aifc").as_slice()] {
			let parsed = Aiff::parse(bytes).unwrap();
			assert_eq!(parsed.metadata().rate, 44_100);
			assert_eq!(parsed.metadata().channels, 1);
			assert_eq!(parsed.metadata().frames, 328_104);
			let mut output = Vec::new();
			assert_eq!(parsed.decoder().read_i16_le(1_024, &mut output), Ok(1_024));
			assert_eq!(output.len(), 2_048);
			assert!(output.iter().any(|byte| *byte != 0));
		}
	}
}
