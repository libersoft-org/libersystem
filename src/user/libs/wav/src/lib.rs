#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use pcm::Format;

const RIFF_HEADER_LEN: usize = 12;
const CHUNK_HEADER_LEN: usize = 8;
const PCM_FORMAT: u16 = 1;
const MS_ADPCM_FORMAT: u16 = 2;
const IMA_ADPCM_FORMAT: u16 = 0x11;

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

pub struct Wav<'a> {
	format: Format,
	codec: Codec,
	block_align: usize,
	data: &'a [u8],
	metadata: Metadata,
}

enum Codec {
	Pcm { bits_per_sample: u16 },
	Ima { samples_per_block: usize },
	Microsoft { samples_per_block: usize, coefficients: Vec<(i16, i16)> },
}

impl<'a> Wav<'a> {
	pub fn parse(bytes: &'a [u8]) -> Result<Wav<'a>, Error> {
		if bytes.len() < RIFF_HEADER_LEN {
			return Err(Error::Truncated);
		}
		if &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
			return Err(Error::Invalid);
		}
		let riff_len = read_u32(bytes, 4)? as usize;
		let riff_end = riff_len.checked_add(8).ok_or(Error::TooLarge)?;
		if riff_end > bytes.len() {
			return Err(Error::Truncated);
		}
		if riff_end < RIFF_HEADER_LEN {
			return Err(Error::Invalid);
		}

		let mut cursor = RIFF_HEADER_LEN;
		let mut parsed_format = None;
		let mut data = None;
		let mut fact_frames = None;
		while cursor < riff_end {
			let header_end = cursor.checked_add(CHUNK_HEADER_LEN).ok_or(Error::TooLarge)?;
			let header = bytes.get(cursor..header_end).ok_or(Error::Truncated)?;
			let chunk_len = u32::from_le_bytes(header[4..8].try_into().map_err(|_| Error::Truncated)?) as usize;
			let body_start = header_end;
			let body_end = body_start.checked_add(chunk_len).ok_or(Error::TooLarge)?;
			let body = bytes.get(body_start..body_end).filter(|_| body_end <= riff_end).ok_or(Error::Truncated)?;
			match &header[..4] {
				b"fmt " => {
					if parsed_format.is_some() {
						return Err(Error::Invalid);
					}
					parsed_format = Some(parse_format(body)?);
				}
				b"data" => {
					if data.is_some() {
						return Err(Error::Invalid);
					}
					data = Some(body);
				}
				b"fact" => {
					if fact_frames.is_some() || body.len() < 4 {
						return Err(Error::Invalid);
					}
					fact_frames = Some(read_u32(body, 0)? as u64);
				}
				_ => {}
			}
			cursor = body_end.checked_add(chunk_len & 1).ok_or(Error::TooLarge)?;
			if cursor > riff_end {
				return Err(Error::Truncated);
			}
		}
		if cursor != riff_end {
			return Err(Error::Invalid);
		}
		let parsed = parsed_format.ok_or(Error::Invalid)?;
		let data = data.ok_or(Error::Invalid)?;
		if data.is_empty() || data.len() % parsed.block_align != 0 {
			return Err(Error::Invalid);
		}
		let block_count = data.len() / parsed.block_align;
		let capacity = match &parsed.codec {
			Codec::Pcm { .. } => block_count as u64,
			Codec::Ima { samples_per_block } | Codec::Microsoft { samples_per_block, .. } => (block_count as u64).checked_mul(*samples_per_block as u64).ok_or(Error::TooLarge)?,
		};
		let frames = match fact_frames {
			Some(frames) if frames != 0 && frames <= capacity => frames,
			Some(_) => return Err(Error::Invalid),
			None => capacity,
		};
		let duration_ms = frames.checked_mul(1_000).ok_or(Error::TooLarge)? / parsed.format.rate() as u64;
		let metadata = Metadata { rate: parsed.format.rate(), channels: parsed.format.channels(), bits_per_sample: parsed.bits_per_sample, frames, duration_ms };
		Ok(Wav { format: parsed.format, codec: parsed.codec, block_align: parsed.block_align, data, metadata })
	}

	pub const fn metadata(&self) -> Metadata {
		self.metadata
	}

	pub const fn format(&self) -> Format {
		self.format
	}

	pub fn decoder(&self) -> Decoder<'_> {
		Decoder { wav: self, frame: 0, block: 0, pending: Vec::new(), pending_frame: 0 }
	}
}

struct ParsedFormat {
	format: Format,
	bits_per_sample: u16,
	block_align: usize,
	codec: Codec,
}

fn parse_format(bytes: &[u8]) -> Result<ParsedFormat, Error> {
	if bytes.len() < 16 {
		return Err(Error::Truncated);
	}
	let codec_tag = read_u16(bytes, 0)?;
	let channels_raw = read_u16(bytes, 2)?;
	let channels = u8::try_from(channels_raw).map_err(|_| Error::Unsupported)?;
	let rate = read_u32(bytes, 4)?;
	let byte_rate = read_u32(bytes, 8)? as u64;
	let block_align = read_u16(bytes, 12)? as usize;
	let bits_per_sample = read_u16(bytes, 14)?;
	let format = Format::new(rate, channels).ok_or(Error::Unsupported)?;
	let codec = match codec_tag {
		PCM_FORMAT => {
			if !matches!(bits_per_sample, 8 | 16 | 24 | 32) {
				return Err(Error::Unsupported);
			}
			let expected_align = (channels as usize).checked_mul(bits_per_sample as usize / 8).ok_or(Error::TooLarge)?;
			let expected_rate = (rate as u64).checked_mul(expected_align as u64).ok_or(Error::TooLarge)?;
			if block_align != expected_align || byte_rate != expected_rate {
				return Err(Error::Invalid);
			}
			Codec::Pcm { bits_per_sample }
		}
		IMA_ADPCM_FORMAT => {
			if bits_per_sample != 4 || bytes.len() < 20 || read_u16(bytes, 16)? < 2 {
				return Err(Error::Invalid);
			}
			let samples_per_block = read_u16(bytes, 18)? as usize;
			if adpcm::ima_samples_per_block(block_align, channels) != Some(samples_per_block) || byte_rate == 0 {
				return Err(Error::Invalid);
			}
			Codec::Ima { samples_per_block }
		}
		MS_ADPCM_FORMAT => {
			if bits_per_sample != 4 || bytes.len() < 22 {
				return Err(Error::Invalid);
			}
			let extension_len = read_u16(bytes, 16)? as usize;
			let samples_per_block = read_u16(bytes, 18)? as usize;
			let coefficient_count = read_u16(bytes, 20)? as usize;
			let coefficient_bytes = coefficient_count.checked_mul(4).ok_or(Error::TooLarge)?;
			if coefficient_count == 0 || coefficient_count > 32 || extension_len < 4 + coefficient_bytes || bytes.len() < 22 + coefficient_bytes || adpcm::ms_samples_per_block(block_align, channels) != Some(samples_per_block) || byte_rate == 0 {
				return Err(Error::Invalid);
			}
			let mut coefficients = Vec::new();
			coefficients.try_reserve_exact(coefficient_count).map_err(|_| Error::TooLarge)?;
			for index in 0..coefficient_count {
				let offset = 22 + index * 4;
				coefficients.push((read_i16(bytes, offset)?, read_i16(bytes, offset + 2)?));
			}
			Codec::Microsoft { samples_per_block, coefficients }
		}
		_ => return Err(Error::Unsupported),
	};
	Ok(ParsedFormat { format, bits_per_sample, block_align, codec })
}

pub struct Decoder<'a> {
	wav: &'a Wav<'a>,
	frame: u64,
	block: usize,
	pending: Vec<u8>,
	pending_frame: usize,
}

impl Decoder<'_> {
	pub const fn remaining_frames(&self) -> u64 {
		self.wav.metadata.frames - self.frame
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
		if !matches!(self.wav.codec, Codec::Pcm { .. }) {
			return self.read_adpcm(frames, output);
		}
		let channels = self.wav.format.channels() as usize;
		let output_len = frames.checked_mul(channels).and_then(|samples| samples.checked_mul(2)).ok_or(Error::TooLarge)?;
		output.clear();
		output.try_reserve_exact(output_len).map_err(|_| Error::TooLarge)?;
		let source_start = usize::try_from(self.frame).map_err(|_| Error::TooLarge)?.checked_mul(self.wav.block_align).ok_or(Error::TooLarge)?;
		let source_len = frames.checked_mul(self.wav.block_align).ok_or(Error::TooLarge)?;
		let source = self.wav.data.get(source_start..source_start + source_len).ok_or(Error::Truncated)?;
		let bits_per_sample = match self.wav.codec {
			Codec::Pcm { bits_per_sample } => bits_per_sample,
			_ => return Err(Error::Invalid),
		};
		let sample_bytes = bits_per_sample as usize / 8;
		for sample in source.chunks_exact(sample_bytes) {
			let value = match bits_per_sample {
				8 => ((sample[0] as i16) - 128) << 8,
				16 => i16::from_le_bytes([sample[0], sample[1]]),
				24 => {
					let value = i32::from_le_bytes([sample[0], sample[1], sample[2], if sample[2] & 0x80 != 0 { 0xff } else { 0 }]);
					(value >> 8) as i16
				}
				32 => (i32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]) >> 16) as i16,
				_ => return Err(Error::Unsupported),
			};
			output.extend_from_slice(&value.to_le_bytes());
		}
		self.frame += frames as u64;
		Ok(frames)
	}

	fn read_adpcm(&mut self, frames: usize, output: &mut Vec<u8>) -> Result<usize, Error> {
		let channels = self.wav.format.channels() as usize;
		let frame_bytes = channels * 2;
		let output_len = frames.checked_mul(frame_bytes).ok_or(Error::TooLarge)?;
		output.clear();
		output.try_reserve_exact(output_len).map_err(|_| Error::TooLarge)?;
		while output.len() < output_len {
			let available = self.pending.len() / frame_bytes - self.pending_frame;
			if available == 0 {
				let start = self.block.checked_mul(self.wav.block_align).ok_or(Error::TooLarge)?;
				let block = self.wav.data.get(start..start + self.wav.block_align).ok_or(Error::Truncated)?;
				match &self.wav.codec {
					Codec::Ima { samples_per_block } => adpcm::decode_ima_block(block, self.wav.format.channels(), *samples_per_block, &mut self.pending).map_err(map_adpcm)?,
					Codec::Microsoft { samples_per_block, coefficients } => adpcm::decode_ms_block(block, self.wav.format.channels(), *samples_per_block, coefficients, &mut self.pending).map_err(map_adpcm)?,
					Codec::Pcm { .. } => return Err(Error::Invalid),
				};
				self.pending_frame = 0;
				self.block += 1;
			}
			let available = self.pending.len() / frame_bytes - self.pending_frame;
			let wanted = (output_len - output.len()) / frame_bytes;
			let take = available.min(wanted);
			let start = self.pending_frame * frame_bytes;
			output.extend_from_slice(&self.pending[start..start + take * frame_bytes]);
			self.pending_frame += take;
		}
		self.frame += frames as u64;
		Ok(frames)
	}
}

fn map_adpcm(error: adpcm::Error) -> Error {
	match error {
		adpcm::Error::Truncated => Error::Truncated,
		adpcm::Error::Invalid => Error::Invalid,
		adpcm::Error::Unsupported => Error::Unsupported,
		adpcm::Error::TooLarge => Error::TooLarge,
	}
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, Error> {
	Ok(u16::from_le_bytes(bytes.get(offset..offset + 2).ok_or(Error::Truncated)?.try_into().map_err(|_| Error::Truncated)?))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, Error> {
	Ok(u32::from_le_bytes(bytes.get(offset..offset + 4).ok_or(Error::Truncated)?.try_into().map_err(|_| Error::Truncated)?))
}

fn read_i16(bytes: &[u8], offset: usize) -> Result<i16, Error> {
	Ok(i16::from_le_bytes(bytes.get(offset..offset + 2).ok_or(Error::Truncated)?.try_into().map_err(|_| Error::Truncated)?))
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;
