#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use pcm::Format;

const MAX_ID3_SIZE: usize = 16 * 1024 * 1024;
const MAX_SYNC_SCAN: usize = 64 * 1024;
// nanomp3's synthesis filter emits the conventional Layer III decoder delay.
const DECODER_DELAY: u64 = 529;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Truncated,
	Invalid,
	Unsupported,
	FormatChanged,
	TooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata {
	pub rate: u32,
	pub channels: u8,
	pub frames: u64,
	pub duration_ms: u64,
}

pub struct Mp3<'a> {
	bytes: &'a [u8],
	start: usize,
	format: Format,
	metadata: Metadata,
	skip_frames: u64,
}

impl<'a> Mp3<'a> {
	pub fn parse(bytes: &'a [u8]) -> Result<Mp3<'a>, Error> {
		let audio_start = id3v2_end(bytes)?;
		let scan_end = bytes.len().min(audio_start.checked_add(MAX_SYNC_SCAN).ok_or(Error::TooLarge)?);
		let mut start = audio_start;
		let header = loop {
			let candidate = bytes.get(start..start + 4).ok_or(Error::Truncated)?;
			match FrameHeader::parse(candidate) {
				Ok(header) => break header,
				Err(Error::Unsupported) if start == audio_start && candidate[0] == 0xff && candidate[1] & 0xe0 == 0xe0 => return Err(Error::Unsupported),
				_ => {}
			}
			start += 1;
			if start >= scan_end {
				return Err(Error::Invalid);
			}
		};
		let format = Format::new(header.rate, header.channels).ok_or(Error::Unsupported)?;
		let gapless = gapless_info(bytes, start, header);
		let frames = gapless.map_or(0, |info| info.frames);
		let duration_ms = frames.saturating_mul(1_000) / u64::from(header.rate);
		Ok(Mp3 { bytes, start, format, metadata: Metadata { rate: header.rate, channels: header.channels, frames, duration_ms }, skip_frames: gapless.map_or(0, |info| info.skip_frames) })
	}

	pub const fn metadata(&self) -> Metadata {
		self.metadata
	}

	pub const fn format(&self) -> Format {
		self.format
	}

	pub fn decoder(&self) -> Decoder<'_> {
		Decoder { mp3: self, engine: nanomp3::Decoder::new(), cursor: self.start, pcm: [0.0; nanomp3::MAX_SAMPLES_PER_FRAME], pending_samples: 0, pending_sample: 0, skip_frames: self.skip_frames, emitted: 0, done: false }
	}
}

fn id3v2_end(bytes: &[u8]) -> Result<usize, Error> {
	if bytes.get(..3) != Some(b"ID3") {
		return Ok(0);
	}
	let header = bytes.get(..10).ok_or(Error::Truncated)?;
	if !(2..=4).contains(&header[3]) || header[4] != 0 || header[5] & 0x0f != 0 || header[6..10].iter().any(|byte| byte & 0x80 != 0) {
		return Err(Error::Unsupported);
	}
	let size = header[6..10].iter().fold(0usize, |value, byte| (value << 7) | *byte as usize);
	if size > MAX_ID3_SIZE {
		return Err(Error::TooLarge);
	}
	let footer = if header[3] == 4 && header[5] & 0x10 != 0 { 10 } else { 0 };
	let end = 10usize.checked_add(size).and_then(|end| end.checked_add(footer)).ok_or(Error::TooLarge)?;
	if end > bytes.len() {
		return Err(Error::Truncated);
	}
	Ok(end)
}

#[derive(Clone, Copy)]
struct FrameHeader {
	rate: u32,
	channels: u8,
	version: u8,
	has_crc: bool,
}

impl FrameHeader {
	fn parse(bytes: &[u8]) -> Result<FrameHeader, Error> {
		if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] & 0xe0 != 0xe0 {
			return Err(Error::Invalid);
		}
		let version = (bytes[1] >> 3) & 0x03;
		if !matches!(version, 2 | 3) || (bytes[1] >> 1) & 0x03 != 1 {
			return Err(Error::Unsupported);
		}
		let bitrate_index = bytes[2] >> 4;
		let rate_index = (bytes[2] >> 2) & 0x03;
		if bitrate_index == 0 || bitrate_index == 15 || rate_index == 3 {
			return Err(Error::Invalid);
		}
		let base_rates = [44_100, 48_000, 32_000];
		let rate = if version == 3 { base_rates[rate_index as usize] } else { base_rates[rate_index as usize] / 2 };
		let channels = if bytes[3] >> 6 == 3 { 1 } else { 2 };
		Ok(FrameHeader { rate, channels, version, has_crc: bytes[1] & 1 == 0 })
	}

	const fn samples(self) -> u64 {
		if self.version == 3 { 1_152 } else { 576 }
	}

	const fn side_info_bytes(self) -> usize {
		match (self.version, self.channels) {
			(3, 1) => 17,
			(3, _) => 32,
			(_, 1) => 9,
			(_, _) => 17,
		}
	}
}

#[derive(Clone, Copy)]
struct GaplessInfo {
	frames: u64,
	skip_frames: u64,
}

fn gapless_info(bytes: &[u8], frame_start: usize, header: FrameHeader) -> Option<GaplessInfo> {
	let xing = frame_start.checked_add(4 + usize::from(header.has_crc) * 2 + header.side_info_bytes())?;
	let marker_end = xing.checked_add(4)?;
	if !matches!(bytes.get(xing..marker_end), Some(b"Xing") | Some(b"Info")) {
		return None;
	}
	let flags = read_be_u32(bytes, marker_end)?;
	let mut cursor = xing.checked_add(8)?;
	let frame_count = if flags & 1 != 0 {
		let count = read_be_u32(bytes, cursor)?;
		cursor = cursor.checked_add(4)?;
		Some(count)
	} else {
		None
	};
	if flags & 2 != 0 {
		cursor = cursor.checked_add(4)?;
	}
	if flags & 4 != 0 {
		cursor = cursor.checked_add(100)?;
	}
	if flags & 8 != 0 {
		cursor = cursor.checked_add(4)?;
	}
	if !matches!(bytes.get(cursor..cursor.checked_add(4)?), Some(b"LAME") | Some(b"Lavf") | Some(b"Lavc") | Some(b"GOGO")) {
		return None;
	}
	let delay_start = cursor.checked_add(21)?;
	let delay = bytes.get(delay_start..delay_start.checked_add(3)?)?;
	let encoder_delay = u64::from(delay[0]) << 4 | u64::from(delay[1] >> 4);
	let padding = u64::from(delay[1] & 0x0f) << 8 | u64::from(delay[2]);
	let encoded_frames = u64::from(frame_count?).checked_mul(header.samples())?;
	let frames = encoded_frames.checked_sub(encoder_delay.checked_add(padding)?)?;
	let skip_frames = header.samples().checked_add(encoder_delay)?.checked_add(DECODER_DELAY)?;
	Some(GaplessInfo { frames, skip_frames })
}

fn read_be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
	Some(u32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?))
}

pub struct Decoder<'a> {
	mp3: &'a Mp3<'a>,
	engine: nanomp3::Decoder,
	cursor: usize,
	pcm: [f32; nanomp3::MAX_SAMPLES_PER_FRAME],
	pending_samples: usize,
	pending_sample: usize,
	skip_frames: u64,
	emitted: u64,
	done: bool,
}

impl Decoder<'_> {
	pub fn remaining_frames(&self) -> u64 {
		if self.mp3.metadata.frames != 0 {
			return self.mp3.metadata.frames.saturating_sub(self.emitted);
		}
		if self.done && self.pending_sample == self.pending_samples { 0 } else { u64::MAX - self.emitted }
	}

	pub fn read_i16_le(&mut self, max_frames: usize, output: &mut Vec<u8>) -> Result<usize, Error> {
		if max_frames == 0 {
			return Err(Error::Invalid);
		}
		output.clear();
		let channels = self.mp3.format.channels() as usize;
		while output.len() / (channels * 2) < max_frames {
			if self.mp3.metadata.frames != 0 && self.emitted == self.mp3.metadata.frames {
				self.done = true;
				break;
			}
			if self.pending_sample == self.pending_samples {
				self.pending_sample = 0;
				self.pending_samples = 0;
				if self.done || self.cursor >= self.mp3.bytes.len() || self.mp3.bytes.get(self.cursor..self.cursor + 3) == Some(b"TAG") {
					self.done = true;
					break;
				}
				let (consumed, info) = self.engine.decode(&self.mp3.bytes[self.cursor..], &mut self.pcm);
				if consumed == 0 {
					return Err(Error::Invalid);
				}
				self.cursor = self.cursor.checked_add(consumed).ok_or(Error::TooLarge)?;
				let Some(info) = info else {
					continue;
				};
				if info.sample_rate != self.mp3.format.rate() || info.channels.num() != self.mp3.format.channels() {
					return Err(Error::FormatChanged);
				}
				self.pending_samples = info.samples_produced.checked_mul(channels).ok_or(Error::TooLarge)?;
				if self.pending_samples > self.pcm.len() {
					return Err(Error::Invalid);
				}
				let available_frames = self.pending_samples / channels;
				let skipped = usize::try_from(self.skip_frames.min(available_frames as u64)).map_err(|_| Error::TooLarge)?;
				self.pending_sample = skipped.checked_mul(channels).ok_or(Error::TooLarge)?;
				self.skip_frames -= skipped as u64;
			}
			let available_frames = (self.pending_samples - self.pending_sample) / channels;
			let remaining = if self.mp3.metadata.frames == 0 { u64::MAX } else { self.mp3.metadata.frames - self.emitted };
			let wanted = usize::try_from(remaining.min((max_frames - output.len() / (channels * 2)) as u64)).map_err(|_| Error::TooLarge)?;
			let frames = available_frames.min(wanted);
			let samples = frames.checked_mul(channels).ok_or(Error::TooLarge)?;
			output.try_reserve(samples.checked_mul(2).ok_or(Error::TooLarge)?).map_err(|_| Error::TooLarge)?;
			for &sample in &self.pcm[self.pending_sample..self.pending_sample + samples] {
				let sample = if sample.is_nan() { 0.0 } else { sample.clamp(-1.0, 1.0) };
				let sample = if sample <= -1.0 {
					i16::MIN
				} else {
					let scaled = sample * i16::MAX as f32;
					(scaled + if scaled < 0.0 { -0.5 } else { 0.5 }) as i16
				};
				output.extend_from_slice(&sample.to_le_bytes());
			}
			self.pending_sample += samples;
			self.emitted = self.emitted.checked_add(frames as u64).ok_or(Error::TooLarge)?;
		}
		Ok(output.len() / (channels * 2))
	}
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;
