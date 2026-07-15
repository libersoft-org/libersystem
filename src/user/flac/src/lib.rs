#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use pcm::Format;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Truncated,
	Invalid,
	Unsupported,
	Checksum,
	TooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata {
	pub rate: u32,
	pub channels: u8,
	pub bits_per_sample: u8,
	pub frames: u64,
	pub duration_ms: u64,
}

pub struct Flac<'a> {
	bytes: &'a [u8],
	frames_start: usize,
	format: Format,
	bits_per_sample: u8,
	min_block_size: usize,
	max_block_size: usize,
	metadata: Metadata,
}

impl<'a> Flac<'a> {
	pub fn parse(bytes: &'a [u8]) -> Result<Flac<'a>, Error> {
		if bytes.get(..4) != Some(b"fLaC") {
			return Err(if bytes.len() < 4 { Error::Truncated } else { Error::Invalid });
		}
		let mut cursor = 4usize;
		let mut stream_info = None;
		let mut last = false;
		let mut first = true;
		while !last {
			let header = *bytes.get(cursor).ok_or(Error::Truncated)?;
			last = header & 0x80 != 0;
			let kind = header & 0x7f;
			let len_bytes = bytes.get(cursor + 1..cursor + 4).ok_or(Error::Truncated)?;
			let len = ((len_bytes[0] as usize) << 16) | ((len_bytes[1] as usize) << 8) | len_bytes[2] as usize;
			let start = cursor.checked_add(4).ok_or(Error::TooLarge)?;
			let end = start.checked_add(len).ok_or(Error::TooLarge)?;
			let body = bytes.get(start..end).ok_or(Error::Truncated)?;
			if first && kind != 0 {
				return Err(Error::Invalid);
			}
			if kind == 0 {
				if stream_info.is_some() || len != 34 {
					return Err(Error::Invalid);
				}
				stream_info = Some(parse_stream_info(body)?);
			}
			cursor = end;
			first = false;
		}
		let info = stream_info.ok_or(Error::Invalid)?;
		if cursor >= bytes.len() {
			return Err(Error::Truncated);
		}
		let duration_ms = if info.frames == 0 { 0 } else { info.frames.checked_mul(1_000).ok_or(Error::TooLarge)? / info.format.rate() as u64 };
		Ok(Flac { bytes, frames_start: cursor, format: info.format, bits_per_sample: info.bits_per_sample, min_block_size: info.min_block_size, max_block_size: info.max_block_size, metadata: Metadata { rate: info.format.rate(), channels: info.format.channels(), bits_per_sample: info.bits_per_sample, frames: info.frames, duration_ms } })
	}

	pub const fn metadata(&self) -> Metadata {
		self.metadata
	}

	pub const fn format(&self) -> Format {
		self.format
	}

	pub fn decoder(&self) -> Decoder<'_> {
		Decoder { flac: self, cursor: self.frames_start, emitted: 0, pending: Vec::new(), pending_frame: 0, done: false }
	}
}

struct StreamInfo {
	format: Format,
	bits_per_sample: u8,
	min_block_size: usize,
	max_block_size: usize,
	frames: u64,
}

fn parse_stream_info(bytes: &[u8]) -> Result<StreamInfo, Error> {
	let min_block_size = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
	let max_block_size = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
	if min_block_size < 16 || max_block_size < min_block_size {
		return Err(Error::Invalid);
	}
	let packed = u64::from_be_bytes(bytes[10..18].try_into().map_err(|_| Error::Truncated)?);
	let rate = (packed >> 44) as u32;
	let channels = ((packed >> 41) as u8 & 0x07) + 1;
	let bits_per_sample = ((packed >> 36) as u8 & 0x1f) + 1;
	let frames = packed & 0x0f_ffff_ffff;
	let format = Format::new(rate, channels).ok_or(Error::Unsupported)?;
	if !matches!(bits_per_sample, 8..=24) {
		return Err(Error::Unsupported);
	}
	Ok(StreamInfo { format, bits_per_sample, min_block_size, max_block_size, frames })
}

pub struct Decoder<'a> {
	flac: &'a Flac<'a>,
	cursor: usize,
	emitted: u64,
	pending: Vec<i32>,
	pending_frame: usize,
	done: bool,
}

impl Decoder<'_> {
	pub fn remaining_frames(&self) -> u64 {
		if self.flac.metadata.frames != 0 {
			self.flac.metadata.frames.saturating_sub(self.emitted)
		} else if self.done {
			0
		} else {
			u64::MAX
		}
	}

	pub fn read_i16_le(&mut self, max_frames: usize, output: &mut Vec<u8>) -> Result<usize, Error> {
		if max_frames == 0 {
			return Err(Error::Invalid);
		}
		output.clear();
		let channels = self.flac.format.channels() as usize;
		while output.len() / (channels * 2) < max_frames {
			let pending_frames = self.pending.len() / channels;
			if self.pending_frame == pending_frames {
				self.pending.clear();
				self.pending_frame = 0;
				if self.done || self.flac.metadata.frames != 0 && self.emitted >= self.flac.metadata.frames {
					self.done = true;
					break;
				}
				self.pending = decode_frame(self.flac, &mut self.cursor)?;
				if self.pending.is_empty() {
					self.done = true;
					break;
				}
			}
			let available = self.pending.len() / channels - self.pending_frame;
			let wanted = max_frames - output.len() / (channels * 2);
			let frames = available.min(wanted);
			let start = self.pending_frame.checked_mul(channels).ok_or(Error::TooLarge)?;
			let end = start.checked_add(frames.checked_mul(channels).ok_or(Error::TooLarge)?).ok_or(Error::TooLarge)?;
			output.try_reserve(frames.checked_mul(channels * 2).ok_or(Error::TooLarge)?).map_err(|_| Error::TooLarge)?;
			for &sample in &self.pending[start..end] {
				let converted = if self.flac.bits_per_sample > 16 { sample >> (self.flac.bits_per_sample - 16) } else { sample.checked_shl((16 - self.flac.bits_per_sample) as u32).ok_or(Error::TooLarge)? };
				let converted = i16::try_from(converted).map_err(|_| Error::Invalid)?;
				output.extend_from_slice(&converted.to_le_bytes());
			}
			self.pending_frame += frames;
			self.emitted = self.emitted.checked_add(frames as u64).ok_or(Error::TooLarge)?;
			if self.flac.metadata.frames != 0 && self.emitted > self.flac.metadata.frames {
				return Err(Error::Invalid);
			}
		}
		Ok(output.len() / (channels * 2))
	}
}

struct FrameHeader {
	block_size: usize,
	channel_assignment: u8,
	bits_per_sample: u8,
	header_end: usize,
}

fn decode_frame(flac: &Flac<'_>, cursor: &mut usize) -> Result<Vec<i32>, Error> {
	if *cursor >= flac.bytes.len() {
		return Ok(Vec::new());
	}
	let start = *cursor;
	let header = parse_frame_header(flac, start)?;
	if header.block_size < flac.min_block_size && flac.metadata.frames == 0 || header.block_size > flac.max_block_size {
		return Err(Error::Invalid);
	}
	let channels = flac.format.channels() as usize;
	let sample_count = header.block_size.checked_mul(channels).ok_or(Error::TooLarge)?;
	let mut reader = Bits::new(flac.bytes, header.header_end);
	let mut decoded = Vec::new();
	decoded.try_reserve_exact(sample_count).map_err(|_| Error::TooLarge)?;
	let mut channel_data = Vec::new();
	channel_data.try_reserve_exact(channels).map_err(|_| Error::TooLarge)?;
	for channel in 0..channels {
		let bits = match (header.channel_assignment, channel) {
			(8, 1) | (9, 0) | (10, 1) => header.bits_per_sample.checked_add(1).ok_or(Error::TooLarge)?,
			_ => header.bits_per_sample,
		};
		channel_data.push(decode_subframe(&mut reader, header.block_size, bits)?);
	}
	reader.align_zero()?;
	let footer = reader.byte_position();
	let expected_crc = u16::from_be_bytes(flac.bytes.get(footer..footer + 2).ok_or(Error::Truncated)?.try_into().map_err(|_| Error::Truncated)?);
	if crc16(flac.bytes.get(start..footer).ok_or(Error::Truncated)?) != expected_crc {
		return Err(Error::Checksum);
	}
	*cursor = footer.checked_add(2).ok_or(Error::TooLarge)?;
	for frame in 0..header.block_size {
		let first = channel_data[0][frame] as i64;
		if channels == 1 {
			decoded.push(i32::try_from(first).map_err(|_| Error::Invalid)?);
			continue;
		}
		let second = channel_data[1][frame] as i64;
		let (left, right) = match header.channel_assignment {
			0..=7 => (first, second),
			8 => (first, first.checked_sub(second).ok_or(Error::TooLarge)?),
			9 => (first.checked_add(second).ok_or(Error::TooLarge)?, second),
			10 => {
				let mid = first.checked_mul(2).and_then(|value| value.checked_add(second & 1)).ok_or(Error::TooLarge)?;
				(mid.checked_add(second).ok_or(Error::TooLarge)? >> 1, mid.checked_sub(second).ok_or(Error::TooLarge)? >> 1)
			}
			_ => return Err(Error::Invalid),
		};
		decoded.push(i32::try_from(left).map_err(|_| Error::Invalid)?);
		decoded.push(i32::try_from(right).map_err(|_| Error::Invalid)?);
	}
	Ok(decoded)
}

fn parse_frame_header(flac: &Flac<'_>, start: usize) -> Result<FrameHeader, Error> {
	let bytes = flac.bytes;
	let first = *bytes.get(start).ok_or(Error::Truncated)?;
	let second = *bytes.get(start + 1).ok_or(Error::Truncated)?;
	if first != 0xff || second & 0xfe != 0xf8 {
		return Err(Error::Invalid);
	}
	let blocking = second & 1;
	let fields = *bytes.get(start + 2).ok_or(Error::Truncated)?;
	let block_code = fields >> 4;
	let rate_code = fields & 0x0f;
	if block_code == 0 || rate_code == 15 {
		return Err(Error::Invalid);
	}
	let format = *bytes.get(start + 3).ok_or(Error::Truncated)?;
	let channel_assignment = format >> 4;
	let bits_code = (format >> 1) & 0x07;
	if format & 1 != 0 || channel_assignment > 10 || bits_code == 3 || bits_code == 7 {
		return Err(Error::Invalid);
	}
	let channels = if channel_assignment <= 7 { channel_assignment + 1 } else { 2 };
	if channels != flac.format.channels() || channels > 2 {
		return Err(Error::Unsupported);
	}
	let mut cursor = start.checked_add(4).ok_or(Error::TooLarge)?;
	let (number, number_len) = read_utf8_number(bytes.get(cursor..).ok_or(Error::Truncated)?)?;
	cursor = cursor.checked_add(number_len).ok_or(Error::TooLarge)?;
	if blocking == 0 && number > 0x7fff_ffff || blocking != 0 && number > 0x0f_ffff_ffff {
		return Err(Error::Invalid);
	}
	let block_size = match block_code {
		1 => 192,
		2..=5 => 576usize << (block_code - 2),
		6 => {
			let value = *bytes.get(cursor).ok_or(Error::Truncated)? as usize + 1;
			cursor += 1;
			value
		}
		7 => {
			let raw = bytes.get(cursor..cursor + 2).ok_or(Error::Truncated)?;
			cursor += 2;
			u16::from_be_bytes([raw[0], raw[1]]) as usize + 1
		}
		8..=15 => 256usize << (block_code - 8),
		_ => return Err(Error::Invalid),
	};
	let rate = match rate_code {
		0 => flac.format.rate(),
		1 => 88_200,
		2 => 176_400,
		3 => 192_000,
		4 => 8_000,
		5 => 16_000,
		6 => 22_050,
		7 => 24_000,
		8 => 32_000,
		9 => 44_100,
		10 => 48_000,
		11 => 96_000,
		12 => {
			let value = *bytes.get(cursor).ok_or(Error::Truncated)? as u32 * 1_000;
			cursor += 1;
			value
		}
		13 | 14 => {
			let raw = bytes.get(cursor..cursor + 2).ok_or(Error::Truncated)?;
			cursor += 2;
			let value = u16::from_be_bytes([raw[0], raw[1]]) as u32;
			if rate_code == 14 { value * 10 } else { value }
		}
		_ => return Err(Error::Invalid),
	};
	if rate != flac.format.rate() {
		return Err(Error::Unsupported);
	}
	let bits_per_sample = match bits_code {
		0 => flac.bits_per_sample,
		1 => 8,
		2 => 12,
		4 => 16,
		5 => 20,
		6 => 24,
		_ => return Err(Error::Invalid),
	};
	if bits_per_sample != flac.bits_per_sample {
		return Err(Error::Unsupported);
	}
	let expected_crc = *bytes.get(cursor).ok_or(Error::Truncated)?;
	if crc8(bytes.get(start..cursor).ok_or(Error::Truncated)?) != expected_crc {
		return Err(Error::Checksum);
	}
	Ok(FrameHeader { block_size, channel_assignment, bits_per_sample, header_end: cursor + 1 })
}

fn read_utf8_number(bytes: &[u8]) -> Result<(u64, usize), Error> {
	let first = *bytes.first().ok_or(Error::Truncated)?;
	if first & 0x80 == 0 {
		return Ok((first as u64, 1));
	}
	let leading = first.leading_ones() as usize;
	if !(2..=7).contains(&leading) {
		return Err(Error::Invalid);
	}
	let mask = if leading == 7 { 0 } else { (1u8 << (7 - leading)) - 1 };
	let mut value = (first & mask) as u64;
	for &byte in bytes.get(1..leading).ok_or(Error::Truncated)? {
		if byte & 0xc0 != 0x80 {
			return Err(Error::Invalid);
		}
		value = value.checked_shl(6).and_then(|value| value.checked_add((byte & 0x3f) as u64)).ok_or(Error::TooLarge)?;
	}
	let minimum = if leading == 2 { 1 << 7 } else { 1u64 << (5 * leading - 4) };
	if value < minimum {
		return Err(Error::Invalid);
	}
	Ok((value, leading))
}

fn decode_subframe(reader: &mut Bits<'_>, block_size: usize, bits_per_sample: u8) -> Result<Vec<i32>, Error> {
	if reader.read(1)? != 0 {
		return Err(Error::Invalid);
	}
	let kind = reader.read(6)? as u8;
	let wasted = if reader.read(1)? == 0 { 0 } else { reader.unary()?.checked_add(1).ok_or(Error::TooLarge)? };
	let effective_bits = bits_per_sample.checked_sub(u8::try_from(wasted).map_err(|_| Error::TooLarge)?).filter(|bits| *bits != 0).ok_or(Error::Invalid)?;
	let mut samples = Vec::new();
	samples.try_reserve_exact(block_size).map_err(|_| Error::TooLarge)?;
	match kind {
		0 => {
			let sample = reader.read_signed(effective_bits)?;
			samples.resize(block_size, sample);
		}
		1 => {
			for _ in 0..block_size {
				samples.push(reader.read_signed(effective_bits)?);
			}
		}
		8..=12 => {
			let order = (kind - 8) as usize;
			if order > block_size {
				return Err(Error::Invalid);
			}
			for _ in 0..order {
				samples.push(reader.read_signed(effective_bits)?);
			}
			let residual = decode_residual(reader, block_size, order)?;
			for value in residual {
				let len = samples.len();
				let prediction = match order {
					0 => 0,
					1 => samples[len - 1] as i64,
					2 => 2 * samples[len - 1] as i64 - samples[len - 2] as i64,
					3 => 3 * samples[len - 1] as i64 - 3 * samples[len - 2] as i64 + samples[len - 3] as i64,
					4 => 4 * samples[len - 1] as i64 - 6 * samples[len - 2] as i64 + 4 * samples[len - 3] as i64 - samples[len - 4] as i64,
					_ => return Err(Error::Invalid),
				};
				samples.push(i32::try_from(prediction.checked_add(value as i64).ok_or(Error::TooLarge)?).map_err(|_| Error::Invalid)?);
			}
		}
		32..=63 => {
			let order = (kind - 31) as usize;
			if order > block_size {
				return Err(Error::Invalid);
			}
			for _ in 0..order {
				samples.push(reader.read_signed(effective_bits)?);
			}
			let precision_code = reader.read(4)? as u8;
			if precision_code == 15 {
				return Err(Error::Invalid);
			}
			let precision = precision_code + 1;
			let shift = reader.read_signed(5)?;
			let mut coefficients = Vec::new();
			coefficients.try_reserve_exact(order).map_err(|_| Error::TooLarge)?;
			for _ in 0..order {
				coefficients.push(reader.read_signed(precision)?);
			}
			let residual = decode_residual(reader, block_size, order)?;
			for value in residual {
				let mut sum = 0i64;
				for (index, &coefficient) in coefficients.iter().enumerate() {
					sum = sum.checked_add((coefficient as i64).checked_mul(samples[samples.len() - index - 1] as i64).ok_or(Error::TooLarge)?).ok_or(Error::TooLarge)?;
				}
				let prediction = if shift >= 0 { sum >> shift } else { sum.checked_shl((-shift) as u32).ok_or(Error::TooLarge)? };
				samples.push(i32::try_from(prediction.checked_add(value as i64).ok_or(Error::TooLarge)?).map_err(|_| Error::Invalid)?);
			}
		}
		_ => return Err(Error::Unsupported),
	}
	if wasted != 0 {
		for sample in &mut samples {
			*sample = sample.checked_shl(u32::try_from(wasted).map_err(|_| Error::TooLarge)?).ok_or(Error::TooLarge)?;
		}
	}
	Ok(samples)
}

fn decode_residual(reader: &mut Bits<'_>, block_size: usize, predictor_order: usize) -> Result<Vec<i32>, Error> {
	let method = reader.read(2)?;
	let parameter_bits = match method {
		0 => 4,
		1 => 5,
		_ => return Err(Error::Unsupported),
	};
	let partition_order = reader.read(4)? as u32;
	let partitions = 1usize.checked_shl(partition_order).ok_or(Error::TooLarge)?;
	if block_size % partitions != 0 {
		return Err(Error::Invalid);
	}
	let partition_size = block_size / partitions;
	if partition_size < predictor_order {
		return Err(Error::Invalid);
	}
	let total = block_size.checked_sub(predictor_order).ok_or(Error::Invalid)?;
	let mut output = Vec::new();
	output.try_reserve_exact(total).map_err(|_| Error::TooLarge)?;
	for partition in 0..partitions {
		let count = if partition == 0 { partition_size - predictor_order } else { partition_size };
		let parameter = reader.read(parameter_bits)? as u8;
		let escape = (1u8 << parameter_bits) - 1;
		if parameter == escape {
			let width = reader.read(5)? as u8;
			for _ in 0..count {
				output.push(if width == 0 { 0 } else { reader.read_signed(width)? });
			}
		} else {
			for _ in 0..count {
				let quotient = reader.unary()?;
				let remainder = reader.read(parameter)?;
				let unsigned = quotient.checked_shl(parameter as u32).and_then(|value| value.checked_add(remainder)).ok_or(Error::TooLarge)?;
				let signed = if unsigned & 1 == 0 { (unsigned >> 1) as i64 } else { -((unsigned >> 1) as i64) - 1 };
				output.push(i32::try_from(signed).map_err(|_| Error::TooLarge)?);
			}
		}
	}
	Ok(output)
}

struct Bits<'a> {
	bytes: &'a [u8],
	bit: usize,
}

impl<'a> Bits<'a> {
	fn new(bytes: &'a [u8], byte: usize) -> Bits<'a> {
		Bits { bytes, bit: byte.saturating_mul(8) }
	}

	fn read(&mut self, count: u8) -> Result<u64, Error> {
		if count > 63 || self.bit.checked_add(count as usize).ok_or(Error::TooLarge)? > self.bytes.len().checked_mul(8).ok_or(Error::TooLarge)? {
			return Err(Error::Truncated);
		}
		let mut value = 0u64;
		for _ in 0..count {
			value = (value << 1) | ((self.bytes[self.bit / 8] >> (7 - self.bit % 8)) & 1) as u64;
			self.bit += 1;
		}
		Ok(value)
	}

	fn read_signed(&mut self, count: u8) -> Result<i32, Error> {
		if count == 0 || count > 32 {
			return Err(Error::Invalid);
		}
		let raw = self.read(count)?;
		let signed = if count == 32 {
			raw as u32 as i32 as i64
		} else if raw & (1u64 << (count - 1)) != 0 {
			raw as i64 - (1i64 << count)
		} else {
			raw as i64
		};
		i32::try_from(signed).map_err(|_| Error::TooLarge)
	}

	fn unary(&mut self) -> Result<u64, Error> {
		let mut zeros = 0u64;
		loop {
			if self.read(1)? != 0 {
				return Ok(zeros);
			}
			zeros = zeros.checked_add(1).ok_or(Error::TooLarge)?;
			if zeros > 0x00ff_ffff {
				return Err(Error::TooLarge);
			}
		}
	}

	fn align_zero(&mut self) -> Result<(), Error> {
		let remainder = self.bit % 8;
		if remainder != 0 && self.read((8 - remainder) as u8)? != 0 {
			return Err(Error::Invalid);
		}
		Ok(())
	}

	fn byte_position(&self) -> usize {
		self.bit / 8
	}
}

fn crc8(bytes: &[u8]) -> u8 {
	let mut crc = 0u8;
	for &byte in bytes {
		crc ^= byte;
		for _ in 0..8 {
			crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 };
		}
	}
	crc
}

fn crc16(bytes: &[u8]) -> u16 {
	let mut crc = 0u16;
	for &byte in bytes {
		crc ^= (byte as u16) << 8;
		for _ in 0..8 {
			crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x8005 } else { crc << 1 };
		}
	}
	crc
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn validates_checksums_and_utf8_numbers() {
		assert_eq!(crc8(b"123456789"), 0xf4);
		assert_eq!(crc16(b"123456789"), 0xfee8);
		assert_eq!(read_utf8_number(&[0x7f]), Ok((127, 1)));
		assert_eq!(read_utf8_number(&[0xc2, 0x80]), Ok((128, 2)));
		assert_eq!(read_utf8_number(&[0xc0, 0x80]), Err(Error::Invalid));
	}

	#[test]
	fn rejects_bad_stream_info_bounds() {
		assert!(matches!(Flac::parse(b"fLaC"), Err(Error::Truncated)));
		let mut bytes = alloc::vec![b'f', b'L', b'a', b'C', 0x80, 0, 0, 34];
		bytes.resize(42, 0);
		assert!(matches!(Flac::parse(&bytes), Err(Error::Invalid)));
	}

	#[test]
	fn decodes_staged_flac_bit_exactly_in_bounded_chunks() {
		let flac = Flac::parse(include_bytes!("../../../volume/sample.flac")).unwrap();
		assert_eq!(flac.metadata().rate, 8_000);
		assert_eq!(flac.metadata().channels, 2);
		assert_eq!(flac.metadata().bits_per_sample, 24);
		assert_eq!(flac.metadata().frames, 512);
		let mut decoder = flac.decoder();
		let mut chunk = Vec::new();
		let mut decoded = Vec::new();
		while decoder.remaining_frames() != 0 {
			let frames = decoder.read_i16_le(127, &mut chunk).unwrap();
			assert!((1..=127).contains(&frames));
			decoded.extend_from_slice(&chunk);
		}
		assert_eq!(decoded, include_bytes!("../tests/data/sample-s16le.pcm"));
	}

	#[test]
	fn rejects_truncated_and_corrupt_frames() {
		let source = include_bytes!("../../../volume/sample.flac");
		for len in [0, 1, 4, 8, 41, source.len() - 1] {
			let result = Flac::parse(&source[..len]);
			if let Ok(flac) = result {
				assert!(flac.decoder().read_i16_le(1_024, &mut Vec::new()).is_err());
			}
		}
		let mut corrupt = source.to_vec();
		*corrupt.last_mut().unwrap() ^= 1;
		let flac = Flac::parse(&corrupt).unwrap();
		assert_eq!(flac.decoder().read_i16_le(1_024, &mut Vec::new()), Err(Error::Checksum));
	}
}
