#![no_std]

extern crate alloc;

use alloc::vec::Vec;

pub const OUTPUT_RATE: u32 = 48_000;
pub const MIN_RATE: u32 = 8_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
	rate: u32,
	channels: u8,
}

impl Format {
	pub fn new(rate: u32, channels: u8) -> Option<Format> {
		if !(MIN_RATE..=OUTPUT_RATE).contains(&rate) || !(1..=2).contains(&channels) {
			return None;
		}
		Some(Format { rate, channels })
	}

	pub const fn rate(self) -> u32 {
		self.rate
	}

	pub const fn channels(self) -> u8 {
		self.channels
	}

	pub const fn frame_bytes(self) -> u64 {
		self.channels as u64 * 2
	}

	pub fn frames_in(self, byte_len: u64) -> Option<usize> {
		if byte_len == 0 || byte_len % self.frame_bytes() != 0 {
			return None;
		}
		usize::try_from(byte_len / self.frame_bytes()).ok()
	}

	pub fn append_i16_le(self, bytes: &[u8], frames: usize, output: &mut Vec<i16>) -> Option<()> {
		let sample_count = frames.checked_mul(self.channels as usize)?;
		let byte_count = sample_count.checked_mul(2)?;
		if bytes.len() < byte_count {
			return None;
		}
		output.reserve(sample_count);
		for sample in bytes[..byte_count].chunks_exact(2) {
			output.push(i16::from_le_bytes([sample[0], sample[1]]));
		}
		Some(())
	}

	pub fn stereo_frame(self, samples: &[i16], frame: usize) -> Option<(i16, i16)> {
		let offset = frame.checked_mul(self.channels as usize)?;
		let left = *samples.get(offset)?;
		let right = if self.channels == 2 { *samples.get(offset + 1)? } else { left };
		Some((left, right))
	}

	pub fn advance(self, phase: &mut u32, frame: &mut usize) {
		*phase += self.rate;
		while *phase >= OUTPUT_RATE {
			*phase -= OUTPUT_RATE;
			*frame += 1;
		}
	}
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
	use super::*;
	use alloc::vec;

	#[test]
	fn validates_rates_channels_and_whole_frames() {
		assert!(Format::new(7_999, 1).is_none());
		assert!(Format::new(48_001, 2).is_none());
		assert!(Format::new(48_000, 0).is_none());
		let stereo = Format::new(48_000, 2).unwrap();
		assert_eq!(stereo.frames_in(8), Some(2));
		assert_eq!(stereo.frames_in(6), None);
	}

	#[test]
	fn decodes_little_endian_and_expands_mono() {
		let mono = Format::new(24_000, 1).unwrap();
		let mut samples = Vec::new();
		mono.append_i16_le(&[0x34, 0x12, 0xfe, 0xff], 2, &mut samples).unwrap();
		assert_eq!(samples, vec![0x1234, -2]);
		assert_eq!(mono.stereo_frame(&samples, 0), Some((0x1234, 0x1234)));
	}

	#[test]
	fn phase_accumulator_converts_24_to_48_khz() {
		let mono = Format::new(24_000, 1).unwrap();
		let (mut phase, mut frame) = (0, 0);
		mono.advance(&mut phase, &mut frame);
		assert_eq!((phase, frame), (24_000, 0));
		mono.advance(&mut phase, &mut frame);
		assert_eq!((phase, frame), (0, 1));
	}
}
