#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use libpcm::Format;

const MAX_ID3_SIZE: usize = 16 * 1024 * 1024;
const MAX_SYNC_SCAN: usize = 64 * 1024;

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
		Ok(Mp3 { bytes, start, format, metadata: Metadata { rate: header.rate, channels: header.channels, frames: 0, duration_ms: 0 } })
	}

	pub const fn metadata(&self) -> Metadata {
		self.metadata
	}

	pub const fn format(&self) -> Format {
		self.format
	}

	pub fn decoder(&self) -> Decoder<'_> {
		Decoder { mp3: self, engine: nanomp3::Decoder::new(), cursor: self.start, pcm: [0.0; nanomp3::MAX_SAMPLES_PER_FRAME], pending_samples: 0, pending_sample: 0, emitted: 0, done: false }
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

struct FrameHeader {
	rate: u32,
	channels: u8,
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
		Ok(FrameHeader { rate, channels })
	}
}

pub struct Decoder<'a> {
	mp3: &'a Mp3<'a>,
	engine: nanomp3::Decoder,
	cursor: usize,
	pcm: [f32; nanomp3::MAX_SAMPLES_PER_FRAME],
	pending_samples: usize,
	pending_sample: usize,
	emitted: u64,
	done: bool,
}

impl Decoder<'_> {
	pub fn remaining_frames(&self) -> u64 {
		if self.done && self.pending_sample == self.pending_samples { 0 } else { u64::MAX - self.emitted }
	}

	pub fn read_i16_le(&mut self, max_frames: usize, output: &mut Vec<u8>) -> Result<usize, Error> {
		if max_frames == 0 {
			return Err(Error::Invalid);
		}
		output.clear();
		let channels = self.mp3.format.channels() as usize;
		while output.len() / (channels * 2) < max_frames {
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
			}
			let available_frames = (self.pending_samples - self.pending_sample) / channels;
			let wanted = max_frames - output.len() / (channels * 2);
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
mod tests {
	use super::*;
	use alloc::vec;

	#[test]
	fn skips_bounded_id3v2_metadata() {
		let mut bytes = b"ID3\x04\0\0\0\0\0\x03abc".to_vec();
		bytes.extend_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
		assert_eq!(id3v2_end(&bytes), Ok(13));
		bytes[9] = 0x80;
		assert_eq!(id3v2_end(&bytes), Err(Error::Unsupported));
	}

	#[test]
	fn validates_mpeg_layer_and_profile() {
		let header = FrameHeader::parse(&[0xff, 0xfb, 0x90, 0x00]).unwrap();
		assert_eq!((header.rate, header.channels), (44_100, 2));
		assert_eq!(FrameHeader::parse(&[0xff, 0xf3, 0x90, 0xc0]).unwrap().rate, 22_050);
		assert!(matches!(FrameHeader::parse(&[0xff, 0xe3, 0x90, 0xc0]), Err(Error::Unsupported)));
		assert!(matches!(FrameHeader::parse(&[0xff, 0xfd, 0x90, 0x00]), Err(Error::Unsupported)));
		assert_eq!(id3v2_end(&vec![b'I', b'D', b'3']), Err(Error::Truncated));
	}

	#[test]
	fn decodes_staged_mpeg2_stream_in_bounded_chunks() {
		let mp3 = Mp3::parse(include_bytes!("../../../volume/sample.mp3")).unwrap();
		assert_eq!(mp3.metadata().rate, 16_000);
		assert_eq!(mp3.metadata().channels, 1);
		let mut decoder = mp3.decoder();
		let mut chunk = Vec::new();
		let mut decoded = Vec::new();
		loop {
			let frames = decoder.read_i16_le(127, &mut chunk).unwrap();
			if frames == 0 {
				break;
			}
			assert!(frames <= 127);
			decoded.extend_from_slice(&chunk);
		}
		let hash = decoded.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ *byte as u64).wrapping_mul(0x100_0000_01b3));
		assert_eq!(decoded.len() / 2, 1_728);
		assert_eq!(hash, 0xdc70_57b9_334b_1113);
		assert!(decoded.iter().any(|byte| *byte != 0));
	}
}
