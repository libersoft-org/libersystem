#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use pcm::Format;

const HEADER_LEN: usize = 32;
const MIN_VERSION: u16 = 0x402;
const MAX_VERSION: u16 = 0x410;
const BYTES_STORED: u32 = 0x3;
const MONO_FLAG: u32 = 0x4;
const HYBRID_FLAG: u32 = 0x8;
const JOINT_STEREO: u32 = 0x10;
const FLOAT_DATA: u32 = 0x80;
const INT32_DATA: u32 = 0x100;
const INITIAL_BLOCK: u32 = 0x800;
const FINAL_BLOCK: u32 = 0x1000;
const SHIFT_LSB: u32 = 13;
const SHIFT_MASK: u32 = 0x1f << SHIFT_LSB;
const MAG_LSB: u32 = 18;
const MAG_MASK: u32 = 0x1f << MAG_LSB;
const SRATE_LSB: u32 = 23;
const SRATE_MASK: u32 = 0xf << SRATE_LSB;
const UNKNOWN_FLAGS: u32 = 0x8000_0000;
const FALSE_STEREO: u32 = 0x4000_0000;
const ID_OPTIONAL_DATA: u8 = 0x20;
const ID_ODD_SIZE: u8 = 0x40;
const ID_LARGE: u8 = 0x80;
const ID_WV_BITSTREAM: u8 = 0x0a;
const ID_DECORR_TERMS: u8 = 0x02;
const ID_DECORR_WEIGHTS: u8 = 0x03;
const ID_DECORR_SAMPLES: u8 = 0x04;
const ID_ENTROPY_VARS: u8 = 0x05;
const ID_SAMPLE_RATE: u8 = ID_OPTIONAL_DATA | 0x07;
const SAMPLE_RATES: [u32; 15] = [6_000, 8_000, 9_600, 11_025, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 64_000, 88_200, 96_000, 192_000];
const EXP2: [u8; 256] = [
	0x00,
	0x01,
	0x01,
	0x02,
	0x03,
	0x03,
	0x04,
	0x05,
	0x06,
	0x06,
	0x07,
	0x08,
	0x08,
	0x09,
	0x0a,
	0x0b,
	0x0b,
	0x0c,
	0x0d,
	0x0e,
	0x0e,
	0x0f,
	0x10,
	0x10,
	0x11,
	0x12,
	0x13,
	0x13,
	0x14,
	0x15,
	0x16,
	0x16,
	0x17,
	0x18,
	0x19,
	0x19,
	0x1a,
	0x1b,
	0x1c,
	0x1d,
	0x1d,
	0x1e,
	0x1f,
	0x20,
	0x20,
	0x21,
	0x22,
	0x23,
	0x24,
	0x24,
	0x25,
	0x26,
	0x27,
	0x28,
	0x28,
	0x29,
	0x2a,
	0x2b,
	0x2c,
	0x2c,
	0x2d,
	0x2e,
	0x2f,
	0x30,
	0x30,
	0x31,
	0x32,
	0x33,
	0x34,
	0x35,
	0x35,
	0x36,
	0x37,
	0x38,
	0x39,
	0x3a,
	0x3a,
	0x3b,
	0x3c,
	0x3d,
	0x3e,
	0x3f,
	0x40,
	0x41,
	0x41,
	0x42,
	0x43,
	0x44,
	0x45,
	0x46,
	0x47,
	0x48,
	0x48,
	0x49,
	0x4a,
	0x4b,
	0x4c,
	0x4d,
	0x4e,
	0x4f,
	0x50,
	0x51,
	0x51,
	0x52,
	0x53,
	0x54,
	0x55,
	0x56,
	0x57,
	0x58,
	0x59,
	0x5a,
	0x5b,
	0x5c,
	0x5d,
	0x5e,
	0x5e,
	0x5f,
	0x60,
	0x61,
	0x62,
	0x63,
	0x64,
	0x65,
	0x66,
	0x67,
	0x68,
	0x69,
	0x6a,
	0x6b,
	0x6c,
	0x6d,
	0x6e,
	0x6f,
	0x70,
	0x71,
	0x72,
	0x73,
	0x74,
	0x75,
	0x76,
	0x77,
	0x78,
	0x79,
	0x7a,
	0x7b,
	0x7c,
	0x7d,
	0x7e,
	0x7f,
	0x80,
	0x81,
	0x82,
	0x83,
	0x84,
	0x85,
	0x87,
	0x88,
	0x89,
	0x8a,
	0x8b,
	0x8c,
	0x8d,
	0x8e,
	0x8f,
	0x90,
	0x91,
	0x92,
	0x93,
	0x95,
	0x96,
	0x97,
	0x98,
	0x99,
	0x9a,
	0x9b,
	0x9c,
	0x9d,
	0x9f,
	0xa0,
	0xa1,
	0xa2,
	0xa3,
	0xa4,
	0xa5,
	0xa6,
	0xa8,
	0xa9,
	0xaa,
	0xab,
	0xac,
	0xad,
	0xaf,
	0xb0,
	0xb1,
	0xb2,
	0xb3,
	0xb4,
	0xb6,
	0xb7,
	0xb8,
	0xb9,
	0xba,
	0xbc,
	0xbd,
	0xbe,
	0xbf,
	0xc0,
	0xc2,
	0xc3,
	0xc4,
	0xc5,
	0xc6,
	0xc8,
	0xc9,
	0xca,
	0xcb,
	0xcd,
	0xce,
	0xcf,
	0xd0,
	0xd2,
	0xd3,
	0xd4,
	0xd6,
	0xd7,
	0xd8,
	0xd9,
	0xdb,
	0xdc,
	0xdd,
	0xde,
	0xe0,
	0xe1,
	0xe2,
	0xe4,
	0xe5,
	0xe6,
	0xe8,
	0xe9,
	0xea,
	0xec,
	0xed,
	0xee,
	0xf0,
	0xf1,
	0xf2,
	0xf4,
	0xf5,
	0xf6,
	0xf8,
	0xf9,
	0xfa,
	0xfc,
	0xfd,
	0xff,
];

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

pub struct WavPack<'a> {
	bytes: &'a [u8],
	format: Format,
	metadata: Metadata,
	blocks: Vec<EncodedBlock<'a>>,
}

struct EncodedBlock<'a> {
	flags: u32,
	header_crc: u32,
	bitstream: &'a [u8],
	passes: Vec<DecorrPass>,
	entropy: Entropy,
	block_index: u64,
	frames: u64,
}

impl<'a> WavPack<'a> {
	pub fn parse(bytes: &'a [u8]) -> Result<WavPack<'a>, Error> {
		let mut cursor = 0usize;
		let mut blocks = Vec::new();
		let mut stream_format = None;
		let mut declared_frames = None;
		let mut decoded_frames = 0u64;
		while bytes.get(cursor..cursor + 4) == Some(b"wvpk") {
			let (block, rate, channels, bits_per_sample, total_samples, block_len) = parse_encoded_block(&bytes[cursor..])?;
			if block.block_index != decoded_frames {
				return Err(Error::Invalid);
			}
			decoded_frames = decoded_frames.checked_add(block.frames).ok_or(Error::TooLarge)?;
			match stream_format {
				Some(expected) if expected != (rate, channels, bits_per_sample) => return Err(Error::Unsupported),
				None => stream_format = Some((rate, channels, bits_per_sample)),
				_ => {}
			}
			if total_samples != 0 && total_samples != u32::MAX as u64 {
				match declared_frames {
					Some(expected) if expected != total_samples => return Err(Error::Invalid),
					None => declared_frames = Some(total_samples),
					_ => {}
				}
			}
			blocks.try_reserve(1).map_err(|_| Error::TooLarge)?;
			blocks.push(block);
			cursor = cursor.checked_add(block_len).ok_or(Error::TooLarge)?;
		}
		if blocks.is_empty() || !valid_trailer(&bytes[cursor..]) {
			return Err(Error::Invalid);
		}
		let (rate, channels, bits_per_sample) = stream_format.ok_or(Error::Invalid)?;
		let frames = declared_frames.unwrap_or(decoded_frames);
		if frames != decoded_frames {
			return Err(Error::Invalid);
		}
		let format = Format::new(rate, channels).ok_or(Error::Unsupported)?;
		let duration_ms = frames.checked_mul(1_000).ok_or(Error::TooLarge)? / rate as u64;
		Ok(WavPack { bytes, format, metadata: Metadata { rate, channels, bits_per_sample, frames, duration_ms }, blocks })
	}

	pub const fn metadata(&self) -> Metadata {
		self.metadata
	}

	pub const fn format(&self) -> Format {
		self.format
	}

	pub fn encoded_len(&self) -> usize {
		self.bytes.len()
	}

	pub fn decoder(&self) -> Decoder<'_> {
		Decoder { wavpack: self, block: 0, current: None, frame: 0 }
	}
}

fn parse_encoded_block(bytes: &[u8]) -> Result<(EncodedBlock<'_>, u32, u8, u8, u64, usize), Error> {
	let block = Block::parse(bytes)?;
	if block.header.block_samples == 0 {
		return Err(Error::Invalid);
	}
	let flags = block.header.flags;
	if flags & (HYBRID_FLAG | FLOAT_DATA | INT32_DATA | UNKNOWN_FLAGS) != 0 || flags & (INITIAL_BLOCK | FINAL_BLOCK) != INITIAL_BLOCK | FINAL_BLOCK {
		return Err(Error::Unsupported);
	}
	let channels = if flags & MONO_FLAG != 0 { 1 } else { 2 };
	let coded_channels = if flags & (MONO_FLAG | FALSE_STEREO) != 0 { 1 } else { 2 };
	let bytes_per_sample = ((flags & BYTES_STORED) + 1) as u8;
	let shift = ((flags & SHIFT_MASK) >> SHIFT_LSB) as u8;
	let bits_per_sample = bytes_per_sample.checked_mul(8).and_then(|bits| bits.checked_sub(shift)).filter(|bits| (1..=32).contains(bits)).ok_or(Error::Unsupported)?;
	let mut rate = SAMPLE_RATES.get(((flags & SRATE_MASK) >> SRATE_LSB) as usize).copied();
	let mut bitstream = None;
	let mut passes = Vec::new();
	let mut entropy = None;
	let mut metadata = block.metadata();
	while let Some(item) = metadata.next()? {
		match item.id {
			ID_DECORR_TERMS => passes = parse_terms(item.data)?,
			ID_DECORR_WEIGHTS => parse_weights(item.data, &mut passes, coded_channels)?,
			ID_DECORR_SAMPLES => parse_samples(item.data, &mut passes, coded_channels)?,
			ID_ENTROPY_VARS => entropy = Some(parse_entropy(item.data, coded_channels)?),
			ID_WV_BITSTREAM => {
				if bitstream.is_some() || item.data.is_empty() {
					return Err(Error::Invalid);
				}
				bitstream = Some(item.data);
			}
			ID_SAMPLE_RATE => {
				if item.data.len() != 3 {
					return Err(Error::Invalid);
				}
				rate = Some(u32::from(item.data[0]) | u32::from(item.data[1]) << 8 | u32::from(item.data[2]) << 16);
			}
			_ if !item.optional && item.id > ID_WV_BITSTREAM => return Err(Error::Unsupported),
			_ => {}
		}
	}
	let bitstream = bitstream.ok_or(Error::Invalid)?;
	let entropy = entropy.ok_or(Error::Invalid)?;
	let rate = rate.filter(|rate| *rate != 0).ok_or(Error::Unsupported)?;
	let total_samples = block.header.total_samples as u64;
	let encoded = EncodedBlock { flags, header_crc: block.header.crc, bitstream, passes, entropy, block_index: block.header.block_index as u64, frames: block.header.block_samples as u64 };
	Ok((encoded, rate, channels, bits_per_sample, total_samples, block.end))
}

#[derive(Clone, Copy)]
struct Header {
	total_samples: u32,
	block_index: u32,
	block_samples: u32,
	flags: u32,
	crc: u32,
}

struct Block<'a> {
	bytes: &'a [u8],
	header: Header,
	end: usize,
}

impl<'a> Block<'a> {
	fn parse(bytes: &'a [u8]) -> Result<Block<'a>, Error> {
		let header = bytes.get(..HEADER_LEN).ok_or(Error::Truncated)?;
		if &header[..4] != b"wvpk" {
			return Err(Error::Invalid);
		}
		let block_len = read_u32(header, 4)?.checked_add(8).ok_or(Error::TooLarge)? as usize;
		if block_len < HEADER_LEN {
			return Err(Error::Invalid);
		}
		let bytes = bytes.get(..block_len).ok_or(Error::Truncated)?;
		let version = read_u16(header, 8)?;
		if !(MIN_VERSION..=MAX_VERSION).contains(&version) || header[10] != 0 || header[11] != 0 {
			return Err(Error::Unsupported);
		}
		Ok(Block { bytes, header: Header { total_samples: read_u32(header, 12)?, block_index: read_u32(header, 16)?, block_samples: read_u32(header, 20)?, flags: read_u32(header, 24)?, crc: read_u32(header, 28)? }, end: block_len })
	}

	fn metadata(&self) -> MetadataReader<'a> {
		MetadataReader { bytes: &self.bytes[HEADER_LEN..], cursor: 0 }
	}
}

#[derive(Clone)]
struct DecorrPass {
	term: i8,
	delta: i32,
	weight: i32,
	weight_b: i32,
	samples: [i32; 8],
	samples_b: [i32; 8],
}

#[derive(Clone, Copy)]
struct Entropy {
	median: [[u32; 3]; 2],
}

fn parse_terms(bytes: &[u8]) -> Result<Vec<DecorrPass>, Error> {
	if bytes.len() > 16 {
		return Err(Error::TooLarge);
	}
	let mut passes = alloc::vec![DecorrPass { term: 0, delta: 0, weight: 0, weight_b: 0, samples: [0; 8], samples_b: [0; 8] }; bytes.len()];
	for (index, byte) in bytes.iter().copied().enumerate() {
		let term = (byte & 0x1f) as i8 - 5;
		if !matches!(term, 1..=8 | 17 | 18) {
			return Err(Error::Unsupported);
		}
		let target = bytes.len() - index - 1;
		passes[target].term = term;
		passes[target].delta = i32::from(byte >> 5);
	}
	Ok(passes)
}

fn parse_weights(bytes: &[u8], passes: &mut [DecorrPass], channels: u8) -> Result<(), Error> {
	if bytes.len() % channels as usize != 0 || bytes.len() / channels as usize > passes.len() {
		return Err(Error::Invalid);
	}
	for (pass, encoded) in passes.iter_mut().rev().zip(bytes.chunks_exact(channels as usize)) {
		pass.weight = restore_weight(encoded[0]);
		if channels == 2 {
			pass.weight_b = restore_weight(encoded[1]);
		}
	}
	Ok(())
}

fn restore_weight(byte: u8) -> i32 {
	let mut weight = i32::from(byte as i8) << 3;
	if weight > 0 {
		weight += (weight + 64) >> 7;
	}
	weight
}

fn parse_samples(mut bytes: &[u8], passes: &mut [DecorrPass], channels: u8) -> Result<(), Error> {
	for pass in passes.iter_mut().rev() {
		let count = if pass.term > 8 { 2 } else { pass.term as usize };
		if bytes.is_empty() {
			continue;
		}
		let needed = count.checked_mul(channels as usize).and_then(|samples| samples.checked_mul(2)).ok_or(Error::TooLarge)?;
		let encoded = bytes.get(..needed).ok_or(Error::Truncated)?;
		if channels == 2 && pass.term > 8 {
			for index in 0..count {
				pass.samples[index] = exp2s(i16::from_le_bytes([encoded[index * 2], encoded[index * 2 + 1]]) as i32)?;
				let offset_b = (count + index) * 2;
				pass.samples_b[index] = exp2s(i16::from_le_bytes([encoded[offset_b], encoded[offset_b + 1]]) as i32)?;
			}
		} else {
			for index in 0..count {
				let offset = index * channels as usize * 2;
				pass.samples[index] = exp2s(i16::from_le_bytes([encoded[offset], encoded[offset + 1]]) as i32)?;
				if channels == 2 {
					pass.samples_b[index] = exp2s(i16::from_le_bytes([encoded[offset + 2], encoded[offset + 3]]) as i32)?;
				}
			}
		}
		bytes = &bytes[needed..];
	}
	if bytes.is_empty() {
		Ok(())
	} else {
		Err(Error::Invalid)
	}
}

fn parse_entropy(bytes: &[u8], channels: u8) -> Result<Entropy, Error> {
	if bytes.len() != 6 * channels as usize {
		return Err(Error::Invalid);
	}
	let mut median = [[0; 3]; 2];
	for (index, value) in bytes.chunks_exact(2).enumerate() {
		median[index / 3][index % 3] = u32::try_from(exp2s(i16::from_le_bytes([value[0], value[1]]) as i32)?).map_err(|_| Error::Invalid)?;
	}
	Ok(Entropy { median })
}

fn exp2s(log: i32) -> Result<i32, Error> {
	if !(-8_192..=8_447).contains(&log) {
		return Err(Error::Invalid);
	}
	if log < 0 {
		return exp2s(-log)?.checked_neg().ok_or(Error::TooLarge);
	}
	let value = u32::from(EXP2[(log & 0xff) as usize]) | 0x100;
	let exponent = log >> 8;
	let expanded = if exponent <= 9 { value >> (9 - exponent) } else { value.checked_shl((exponent - 9) as u32).ok_or(Error::TooLarge)? };
	i32::try_from(expanded).map_err(|_| Error::TooLarge)
}

fn valid_trailer(bytes: &[u8]) -> bool {
	if bytes.is_empty() {
		return true;
	}
	if bytes.len() < 64 || bytes.get(..8) != Some(b"APETAGEX") || bytes.get(bytes.len() - 32..bytes.len() - 24) != Some(b"APETAGEX") {
		return false;
	}
	let Some(size) = read_u32(bytes, 12).ok().and_then(|size| usize::try_from(size).ok()) else { return false };
	let Some(footer_size) = read_u32(bytes, bytes.len() - 20).ok().and_then(|size| usize::try_from(size).ok()) else { return false };
	size == footer_size && size.checked_add(32) == Some(bytes.len()) && read_u32(bytes, 8) == Ok(2_000) && read_u32(bytes, bytes.len() - 24) == Ok(2_000)
}

pub struct Decoder<'a> {
	wavpack: &'a WavPack<'a>,
	block: usize,
	current: Option<BlockDecoder<'a>>,
	frame: u64,
}

struct BlockDecoder<'a> {
	flags: u32,
	header_crc: u32,
	bits: BitReader<'a>,
	passes: Vec<DecorrPass>,
	words: Words,
	frame: u64,
	frames: u64,
	position: usize,
	crc: u32,
}

impl Decoder<'_> {
	pub fn remaining_frames(&self) -> u64 {
		self.wavpack.metadata.frames - self.frame
	}

	pub fn read_i16_le(&mut self, max_frames: usize, output: &mut Vec<u8>) -> Result<usize, Error> {
		if max_frames == 0 {
			return Err(Error::Invalid);
		}
		output.clear();
		let channels = self.wavpack.format.channels() as usize;
		let frames = usize::try_from(self.remaining_frames().min(max_frames as u64)).map_err(|_| Error::TooLarge)?;
		output.try_reserve_exact(frames.checked_mul(channels).and_then(|samples| samples.checked_mul(2)).ok_or(Error::TooLarge)?).map_err(|_| Error::TooLarge)?;
		let mut decoded = 0usize;
		while decoded < frames {
			if self.current.is_none() {
				let block = self.wavpack.blocks.get(self.block).ok_or(Error::Truncated)?;
				self.current = Some(BlockDecoder::new(block));
			}
			let current = self.current.as_mut().ok_or(Error::Invalid)?;
			let take = (frames - decoded).min(usize::try_from(current.remaining_frames()).map_err(|_| Error::TooLarge)?);
			if let Err(error) = current.append_i16_le(take, channels, output) {
				output.clear();
				return Err(error);
			}
			decoded += take;
			if current.remaining_frames() == 0 {
				self.current = None;
				self.block += 1;
			}
		}
		self.frame += decoded as u64;
		Ok(decoded)
	}
}

impl<'a> BlockDecoder<'a> {
	fn new(block: &'a EncodedBlock<'a>) -> BlockDecoder<'a> {
		BlockDecoder { flags: block.flags, header_crc: block.header_crc, bits: BitReader::new(block.bitstream), passes: block.passes.clone(), words: Words::new(block.entropy), frame: 0, frames: block.frames, position: 0, crc: u32::MAX }
	}

	fn remaining_frames(&self) -> u64 {
		self.frames - self.frame
	}

	fn append_i16_le(&mut self, frames: usize, channels: usize, output: &mut Vec<u8>) -> Result<(), Error> {
		let coded_channels = if self.flags & (MONO_FLAG | FALSE_STEREO) != 0 { 1 } else { 2 };
		let magnitude = ((self.flags & MAG_MASK) >> MAG_LSB).min(30);
		let limit = (1i64 << magnitude) + 2;
		let stored_bits = ((self.flags & BYTES_STORED) + 1) * 8;
		let shift = (self.flags & SHIFT_MASK) >> SHIFT_LSB;
		for _ in 0..frames {
			let mut left = self.words.read(&mut self.bits, 0, coded_channels)?;
			let mut right = if coded_channels == 2 { self.words.read(&mut self.bits, 1, coded_channels)? } else { left };
			for pass in &mut self.passes {
				if coded_channels == 2 {
					(left, right) = pass.apply_stereo(left, right, self.position)?;
				} else {
					left = pass.apply_mono(left, self.position)?;
					right = left;
				}
			}
			self.position = (self.position + 1) & 7;
			if coded_channels == 2 && self.flags & JOINT_STEREO != 0 {
				right = right.wrapping_sub(left >> 1);
				left = left.wrapping_add(right);
			}
			if i64::from(left).abs() > limit || i64::from(right).abs() > limit {
				return Err(Error::Invalid);
			}
			self.crc = if coded_channels == 2 { self.crc.wrapping_mul(3).wrapping_add(left as u32).wrapping_mul(3).wrapping_add(right as u32) } else { self.crc.wrapping_mul(3).wrapping_add(left as u32) };
			output.extend_from_slice(&convert_sample(left, shift, stored_bits)?.to_le_bytes());
			if channels == 2 {
				output.extend_from_slice(&convert_sample(right, shift, stored_bits)?.to_le_bytes());
			}
		}
		self.frame += frames as u64;
		if self.remaining_frames() == 0 && self.crc != self.header_crc {
			return Err(Error::Checksum);
		}
		Ok(())
	}
}

fn convert_sample(sample: i32, shift: u32, stored_bits: u32) -> Result<i16, Error> {
	let sample = sample.checked_shl(shift).ok_or(Error::TooLarge)?;
	let converted = if stored_bits < 16 { sample.checked_shl(16 - stored_bits).ok_or(Error::TooLarge)? } else { sample >> (stored_bits - 16) };
	i16::try_from(converted).map_err(|_| Error::Invalid)
}

impl DecorrPass {
	fn prediction(term: i8, samples: &[i32; 8], position: usize) -> Result<(i32, usize), Error> {
		let prediction = match term {
			17 => (samples[0] as u32).wrapping_mul(2).wrapping_sub(samples[1] as u32) as i32,
			18 => ((samples[0] as u32).wrapping_mul(3).wrapping_sub(samples[1] as u32) as i32) >> 1,
			1..=8 => samples[position],
			_ => return Err(Error::Unsupported),
		};
		Ok((prediction, if term > 8 { 0 } else { (position + term as usize) & 7 }))
	}

	fn apply_mono(&mut self, residual: i32, position: usize) -> Result<i32, Error> {
		let (prediction, target) = Self::prediction(self.term, &self.samples, position)?;
		let weighted = apply_weight(self.weight, prediction)?;
		let sample = residual.wrapping_add(weighted as u32 as i32);
		if prediction != 0 && residual != 0 {
			self.weight += if prediction ^ residual < 0 { -self.delta } else { self.delta };
		}
		if self.term > 8 {
			self.samples[1] = self.samples[0];
		}
		self.samples[target] = sample;
		Ok(sample)
	}

	fn apply_stereo(&mut self, left: i32, right: i32, position: usize) -> Result<(i32, i32), Error> {
		let (prediction_a, target) = Self::prediction(self.term, &self.samples, position)?;
		let (prediction_b, _) = Self::prediction(self.term, &self.samples_b, position)?;
		let decoded_a = left.wrapping_add(apply_weight(self.weight, prediction_a)? as u32 as i32);
		let decoded_b = right.wrapping_add(apply_weight(self.weight_b, prediction_b)? as u32 as i32);
		if prediction_a != 0 && left != 0 {
			self.weight += if prediction_a ^ left < 0 { -self.delta } else { self.delta };
		}
		if prediction_b != 0 && right != 0 {
			self.weight_b += if prediction_b ^ right < 0 { -self.delta } else { self.delta };
		}
		if self.term > 8 {
			self.samples[1] = self.samples[0];
			self.samples_b[1] = self.samples_b[0];
		}
		self.samples[target] = decoded_a;
		self.samples_b[target] = decoded_b;
		Ok((decoded_a, decoded_b))
	}
}

fn apply_weight(weight: i32, sample: i32) -> Result<i32, Error> {
	Ok((weight as u32).wrapping_mul(sample as u32).wrapping_add(512) as i32 >> 10)
}

struct Words {
	median: [[u32; 3]; 2],
	holding_zero: bool,
	holding_one: bool,
	zeros_acc: u32,
}

impl Words {
	fn new(entropy: Entropy) -> Words {
		Words { median: entropy.median, holding_zero: false, holding_one: false, zeros_acc: 0 }
	}

	fn read(&mut self, bits: &mut BitReader<'_>, channel: usize, channels: u8) -> Result<i32, Error> {
		if self.median[0][0] & !1 == 0 && self.median[1][0] & !1 == 0 && !self.holding_zero && !self.holding_one {
			if self.zeros_acc != 0 {
				self.zeros_acc -= 1;
				if self.zeros_acc != 0 {
					return Ok(0);
				}
			} else {
				let count = bits.unary(33)?;
				self.zeros_acc = if count < 2 { count } else { bits.read((count - 1) as u8)? | (1 << (count - 1)) };
				if self.zeros_acc != 0 {
					self.median = [[0; 3]; 2];
					return Ok(0);
				}
			}
		}
		let ones = if self.holding_zero {
			self.holding_zero = false;
			0
		} else {
			let mut count = bits.unary(17)?;
			if count == 16 {
				let extra = bits.unary(33)?;
				count = 16 + if extra < 2 { extra } else { bits.read((extra - 1) as u8)? | (1 << (extra - 1)) };
			}
			let low = u32::from(self.holding_one);
			self.holding_one = count & 1 != 0;
			self.holding_zero = count & 1 == 0;
			(count >> 1) + low
		};
		let (low, high) = self.range(channel.min(channels as usize - 1), ones)?;
		let magnitude = bits.read_code(high - low)?.checked_add(low).ok_or(Error::TooLarge)?;
		let negative = bits.bit()?;
		Ok(if negative { !(magnitude as i32) } else { magnitude as i32 })
	}

	fn range(&mut self, channel: usize, ones: u32) -> Result<(u32, u32), Error> {
		let median = &mut self.median[channel];
		let get = |median: u32| (median >> 4) + 1;
		if ones == 0 {
			let high = get(median[0]) - 1;
			median[0] = median[0].wrapping_sub(((median[0] + 126) / 128) * 2);
			return Ok((0, high));
		}
		let mut low = get(median[0]);
		median[0] = median[0].wrapping_add(((median[0] + 128) / 128) * 5);
		if ones == 1 {
			let high = low.checked_add(get(median[1]) - 1).ok_or(Error::TooLarge)?;
			median[1] = median[1].wrapping_sub(((median[1] + 62) / 64) * 2);
			return Ok((low, high));
		}
		low = low.checked_add(get(median[1])).ok_or(Error::TooLarge)?;
		median[1] = median[1].wrapping_add(((median[1] + 64) / 64) * 5);
		if ones == 2 {
			let high = low.checked_add(get(median[2]) - 1).ok_or(Error::TooLarge)?;
			median[2] = median[2].wrapping_sub(((median[2] + 30) / 32) * 2);
			return Ok((low, high));
		}
		low = low.checked_add((ones - 2).checked_mul(get(median[2])).ok_or(Error::TooLarge)?).ok_or(Error::TooLarge)?;
		let high = low.checked_add(get(median[2]) - 1).ok_or(Error::TooLarge)?;
		median[2] = median[2].wrapping_add(((median[2] + 32) / 32) * 5);
		Ok((low, high))
	}
}

struct BitReader<'a> {
	bytes: &'a [u8],
	byte: usize,
	bit: u8,
}

impl<'a> BitReader<'a> {
	const fn new(bytes: &'a [u8]) -> BitReader<'a> {
		BitReader { bytes, byte: 0, bit: 0 }
	}

	fn bit(&mut self) -> Result<bool, Error> {
		let value = *self.bytes.get(self.byte).ok_or(Error::Truncated)?;
		let bit = value & (1 << self.bit) != 0;
		self.bit += 1;
		if self.bit == 8 {
			self.bit = 0;
			self.byte += 1;
		}
		Ok(bit)
	}

	fn read(&mut self, count: u8) -> Result<u32, Error> {
		if count > 32 {
			return Err(Error::TooLarge);
		}
		let mut value = 0u32;
		for shift in 0..count {
			if self.bit()? {
				value |= 1 << shift;
			}
		}
		Ok(value)
	}

	fn unary(&mut self, limit: u32) -> Result<u32, Error> {
		for count in 0..limit {
			if !self.bit()? {
				return Ok(count);
			}
		}
		Err(Error::Invalid)
	}

	fn read_code(&mut self, max: u32) -> Result<u32, Error> {
		let bit_count = 32 - max.leading_zeros();
		if bit_count == 0 {
			return Ok(0);
		}
		let extras = (1u64 << bit_count) - u64::from(max) - 1;
		let mut code = u64::from(self.read((bit_count - 1) as u8)?);
		if code >= extras {
			code = (code << 1) - extras + u64::from(self.bit()?);
		}
		u32::try_from(code).map_err(|_| Error::TooLarge)
	}
}

struct MetadataItem<'a> {
	id: u8,
	optional: bool,
	data: &'a [u8],
}

struct MetadataReader<'a> {
	bytes: &'a [u8],
	cursor: usize,
}

impl<'a> MetadataReader<'a> {
	fn next(&mut self) -> Result<Option<MetadataItem<'a>>, Error> {
		if self.cursor == self.bytes.len() {
			return Ok(None);
		}
		let basic = self.bytes.get(self.cursor..self.cursor + 2).ok_or(Error::Truncated)?;
		let raw_id = basic[0];
		let mut padded_len = usize::from(basic[1]) << 1;
		let header_len = if raw_id & ID_LARGE != 0 {
			let large = self.bytes.get(self.cursor + 2..self.cursor + 4).ok_or(Error::Truncated)?;
			padded_len = padded_len.checked_add(usize::from(large[0]) << 9).and_then(|len| len.checked_add(usize::from(large[1]) << 17)).ok_or(Error::TooLarge)?;
			4
		} else {
			2
		};
		let actual_len = padded_len.checked_sub(usize::from(raw_id & ID_ODD_SIZE != 0)).ok_or(Error::Invalid)?;
		let data_start = self.cursor.checked_add(header_len).ok_or(Error::TooLarge)?;
		let data_end = data_start.checked_add(padded_len).ok_or(Error::TooLarge)?;
		let padded = self.bytes.get(data_start..data_end).ok_or(Error::Truncated)?;
		self.cursor = data_end;
		Ok(Some(MetadataItem { id: raw_id & !(ID_ODD_SIZE | ID_LARGE), optional: raw_id & ID_OPTIONAL_DATA != 0, data: &padded[..actual_len] }))
	}
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, Error> {
	let value = bytes.get(offset..offset + 2).ok_or(Error::Truncated)?;
	Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, Error> {
	let value = bytes.get(offset..offset + 4).ok_or(Error::Truncated)?;
	Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

#[cfg(test)]
mod tests {
	extern crate alloc;

	use super::*;
	use alloc::vec::Vec;

	fn item(id: u8, data: &[u8], large: bool) -> Vec<u8> {
		let padded = data.len() + (data.len() & 1);
		let words = padded / 2;
		let mut out = Vec::new();
		out.push(id | if data.len() & 1 != 0 { ID_ODD_SIZE } else { 0 } | if large { ID_LARGE } else { 0 });
		out.push(words as u8);
		if large {
			out.push((words >> 8) as u8);
			out.push((words >> 16) as u8);
		}
		out.extend_from_slice(data);
		if data.len() & 1 != 0 {
			out.push(0);
		}
		out
	}

	fn block(flags: u32, metadata: &[u8]) -> Vec<u8> {
		let mut out = b"wvpk".to_vec();
		out.extend_from_slice(&((HEADER_LEN + metadata.len() - 8) as u32).to_le_bytes());
		out.extend_from_slice(&0x410u16.to_le_bytes());
		out.extend_from_slice(&[0, 0]);
		out.extend_from_slice(&16u32.to_le_bytes());
		out.extend_from_slice(&0u32.to_le_bytes());
		out.extend_from_slice(&16u32.to_le_bytes());
		out.extend_from_slice(&flags.to_le_bytes());
		out.extend_from_slice(&0u32.to_le_bytes());
		out.extend_from_slice(metadata);
		out
	}

	fn base_flags() -> u32 {
		INITIAL_BLOCK | FINAL_BLOCK | MONO_FLAG | (1 << SRATE_LSB) | 1
	}

	fn lossless_metadata(bitstream: &[u8]) -> Vec<u8> {
		let mut metadata = item(ID_ENTROPY_VARS, &[0; 6], false);
		metadata.extend_from_slice(&item(ID_WV_BITSTREAM, bitstream, false));
		metadata
	}

	#[test]
	fn parses_a_bounded_lossless_block() {
		let bytes = block(base_flags(), &lossless_metadata(&[1, 2]));
		let wavpack = WavPack::parse(&bytes).unwrap();
		assert_eq!(wavpack.metadata(), Metadata { rate: 8_000, channels: 1, bits_per_sample: 16, frames: 16, duration_ms: 2 });
		assert_eq!(wavpack.encoded_len(), bytes.len());
	}

	#[test]
	fn parses_large_odd_metadata_and_custom_rate() {
		let mut metadata = item(ID_SAMPLE_RATE, &[0x80, 0xbb, 0x00], false);
		metadata.extend_from_slice(&item(ID_OPTIONAL_DATA | 0x1f, &alloc::vec![7; 513], true));
		metadata.extend_from_slice(&lossless_metadata(&[1, 2]));
		let bytes = block(base_flags() | (14 << SRATE_LSB), &metadata);
		let wavpack = WavPack::parse(&bytes).unwrap();
		assert_eq!(wavpack.metadata().rate, 48_000);
	}

	#[test]
	fn rejects_truncation_missing_bitstream_and_unsupported_profiles() {
		let valid = block(base_flags(), &lossless_metadata(&[1, 2]));
		for len in [0, 4, 31, valid.len() - 1] {
			assert!(WavPack::parse(&valid[..len]).is_err());
		}
		assert!(matches!(WavPack::parse(&block(base_flags(), &[])), Err(Error::Invalid)));
		for flag in [HYBRID_FLAG, FLOAT_DATA, INT32_DATA, UNKNOWN_FLAGS] {
			assert!(matches!(WavPack::parse(&block(base_flags() | flag, &lossless_metadata(&[1, 2]))), Err(Error::Unsupported)));
		}
		assert!(matches!(WavPack::parse(&block(base_flags() & !FINAL_BLOCK, &lossless_metadata(&[1, 2]))), Err(Error::Unsupported)));
	}

	#[test]
	fn decodes_staged_lossless_stream_bit_exactly_in_bounded_chunks() {
		let wavpack = WavPack::parse(include_bytes!("../../../volume/test.wv")).unwrap();
		assert_eq!(wavpack.metadata(), Metadata { rate: 44_100, channels: 1, bits_per_sample: 16, frames: 328_104, duration_ms: 7_440 });
		let mut decoder = wavpack.decoder();
		let mut chunk = Vec::new();
		let mut decoded = Vec::new();
		let wav = wav::Wav::parse(include_bytes!("../../../volume/test.wav")).unwrap();
		let mut wav_decoder = wav.decoder();
		let mut expected = Vec::new();
		loop {
			let frames = wav_decoder.read_i16_le(1_024, &mut chunk).unwrap();
			if frames == 0 {
				break;
			}
			expected.extend_from_slice(&chunk);
		}
		while decoder.remaining_frames() != 0 {
			let frames = decoder.read_i16_le(127, &mut chunk).unwrap();
			assert!((1..=127).contains(&frames));
			decoded.extend_from_slice(&chunk);
		}
		assert_eq!(decoded, expected);
	}

	#[test]
	fn decodes_true_stereo_with_independent_channel_state() {
		let wavpack = WavPack::parse(include_bytes!("../../../volume/test-stereo.wv")).unwrap();
		assert_eq!(wavpack.metadata(), Metadata { rate: 44_100, channels: 2, bits_per_sample: 16, frames: 328_104, duration_ms: 7_440 });
		let wav = wav::Wav::parse(include_bytes!("../../../volume/test.wav")).unwrap();
		let mut wav_decoder = wav.decoder();
		let mut mono = Vec::new();
		let mut mono_chunk = Vec::new();
		loop {
			let frames = wav_decoder.read_i16_le(1_024, &mut mono_chunk).unwrap();
			if frames == 0 {
				break;
			}
			mono.extend_from_slice(&mono_chunk);
		}
		let mut expected = Vec::new();
		for bytes in mono.chunks_exact(2) {
			let sample = i16::from_le_bytes([bytes[0], bytes[1]]);
			expected.extend_from_slice(&sample.to_le_bytes());
			expected.extend_from_slice(&sample.wrapping_neg().to_le_bytes());
		}
		let mut decoder = wavpack.decoder();
		let mut chunk = Vec::new();
		let mut decoded = Vec::new();
		while decoder.remaining_frames() != 0 {
			let frames = decoder.read_i16_le(127, &mut chunk).unwrap();
			assert!((1..=127).contains(&frames));
			decoded.extend_from_slice(&chunk);
		}
		assert_eq!(decoded, expected);
	}

	#[test]
	fn streams_multiple_blocks_and_checks_each_crc() {
		let source = include_bytes!("../../../volume/test.wv");
		let wavpack = WavPack::parse(source).unwrap();
		assert_eq!(wavpack.metadata().frames, 328_104);
		assert!(wavpack.blocks.len() > 2);
		let mut decoder = wavpack.decoder();
		let mut chunk = Vec::new();
		let mut bytes = 0usize;
		let mut hash = 0xcbf2_9ce4_8422_2325u64;
		while decoder.remaining_frames() != 0 {
			let frames = decoder.read_i16_le(777, &mut chunk).unwrap();
			assert!((1..=777).contains(&frames));
			bytes += chunk.len();
			for byte in &chunk {
				hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3);
			}
		}
		assert_eq!(bytes, 656_208);
		assert_eq!(hash, 0x3a09_ed82_4ddc_9e5d);

		let mut corrupt = source.to_vec();
		let second = corrupt.windows(4).enumerate().filter(|(_, bytes)| *bytes == b"wvpk").nth(1).unwrap().0;
		corrupt[second + 100] ^= 1;
		let wavpack = WavPack::parse(&corrupt).unwrap();
		let mut decoder = wavpack.decoder();
		loop {
			match decoder.read_i16_le(1_024, &mut chunk) {
				Ok(0) => panic!("corrupt second block passed its CRC"),
				Ok(_) => {}
				Err(_) => break,
			}
		}
	}

	#[test]
	fn malformed_streams_fail_without_panicking_or_stalling() {
		let source = include_bytes!("../tests/test-stereo-short.wv");
		for len in (0..source.len()).step_by(source.len().div_ceil(256)) {
			if let Ok(wavpack) = WavPack::parse(&source[..len]) {
				let mut decoder = wavpack.decoder();
				let mut chunk = Vec::new();
				for _ in 0..wavpack.blocks.len() * 2 + 2 {
					if decoder.remaining_frames() == 0 || decoder.read_i16_le(4_096, &mut chunk).is_err() {
						break;
					}
				}
				assert_eq!(decoder.remaining_frames(), 0, "truncated stream stalled the decoder");
			}
		}
		let mut state = 0x72d6_8a31u32;
		for _ in 0..256 {
			state ^= state << 13;
			state ^= state >> 17;
			state ^= state << 5;
			let mut corrupt = source.to_vec();
			let index = state as usize % corrupt.len();
			corrupt[index] ^= 1 << (state >> 29);
			if let Ok(wavpack) = WavPack::parse(&corrupt) {
				let mut decoder = wavpack.decoder();
				let mut chunk = Vec::new();
				let mut steps = 0usize;
				let mut rejected = false;
				while decoder.remaining_frames() != 0 && steps < 128 {
					if decoder.read_i16_le(4_096, &mut chunk).is_err() {
						rejected = true;
						break;
					}
					steps += 1;
				}
				assert!(rejected || decoder.remaining_frames() == 0, "mutated stream stalled the decoder");
			}
		}
	}
}
