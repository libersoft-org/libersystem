//! Writing RIFF/WAVE, incrementally, in the profiles this leaf can read.
//!
//! The encoder writes a header with two sizes it cannot yet know, streams the audio past, and
//! corrects them at the end. That is what `Sink::patch` exists for, and it is why an encoder built
//! over a destination that only goes forward is refused when it is BUILT rather than when it
//! finishes: a wrong length in a finished file is a file that decodes into something else.
//!
//! Nothing here keeps the track. The PCM profiles convert frame by frame straight into the sink; the
//! ADPCM profiles hold exactly one block, because a block is the unit the format is defined in.

use crate::{Error, IMA_ADPCM_FORMAT, MS_ADPCM_FORMAT, PCM_FORMAT};
use adpcm::encode::{MS_COEFFICIENTS, encode_ima_block, encode_ms_block};
use alloc::vec::Vec;
use pcm::Format;
use pcm::encode::{Sink, SinkError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
	// The profile asked for is not one this leaf writes.
	Unsupported,
	// The arguments cannot describe a file: no frames, a block size nothing fits in, a count that
	// does not fit the container's 32-bit sizes.
	Invalid,
	TooLarge,
	// The destination said no. Carried rather than flattened, because "the disk is full" and "this
	// destination cannot seek" lead a caller to do different things.
	Destination(SinkError),
}

impl From<SinkError> for EncodeError {
	fn from(error: SinkError) -> EncodeError {
		EncodeError::Destination(error)
	}
}

// The block codecs report in their own vocabulary; it arrives here through the same door.
impl From<adpcm::Error> for EncodeError {
	fn from(error: adpcm::Error) -> EncodeError {
		match error {
			adpcm::Error::TooLarge => EncodeError::TooLarge,
			_ => EncodeError::Invalid,
		}
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
	Pcm { bits: u16 },
	ImaAdpcm { block_align: usize },
	MsAdpcm { block_align: usize },
}

impl Output {
	// The block size both ADPCM profiles are conventionally written at: 505 samples per block for
	// IMA and 500 for Microsoft, at either channel count, which is what the decoders in the wild
	// have been reading for thirty years.
	pub const fn ima_default(channels: u8) -> Output {
		Output::ImaAdpcm { block_align: 256 * channels as usize }
	}

	pub const fn ms_default(channels: u8) -> Output {
		Output::MsAdpcm { block_align: 256 * channels as usize }
	}
}

// Where the sizes that are only known at the end live in the header.
struct Placeholders {
	riff: u64,
	fact: Option<u64>,
	data: u64,
}

pub struct Encoder<S: Sink> {
	sink: S,
	format: Format,
	output: Output,
	block_align: usize,
	samples_per_block: usize,
	placeholders: Placeholders,
	// One block's worth of interleaved input, for the ADPCM profiles only.
	pending: Vec<i16>,
	block: Vec<u8>,
	reconstructed: Vec<i16>,
	frames: u64,
	data_bytes: u64,
}

impl<S: Sink> Encoder<S> {
	pub fn new(mut sink: S, format: Format, output: Output) -> Result<Encoder<S>, EncodeError> {
		let channels = format.channels();
		let (block_align, samples_per_block, tag, bits) = match output {
			Output::Pcm { bits } => {
				if !matches!(bits, 8 | 16 | 24 | 32) {
					return Err(EncodeError::Unsupported);
				}
				(channels as usize * (bits as usize / 8), 1, PCM_FORMAT, bits)
			}
			Output::ImaAdpcm { block_align } => {
				let samples = adpcm::ima_samples_per_block(block_align, channels).ok_or(EncodeError::Invalid)?;
				// The payload must divide into the groups the format packs in, or the block cannot be
				// filled exactly and the encoder would be writing a file its own decoder rejects.
				if samples < 2 || block_align > u16::MAX as usize || (channels == 2 && (block_align - 8) % 8 != 0) {
					return Err(EncodeError::Invalid);
				}
				(block_align, samples, IMA_ADPCM_FORMAT, 4)
			}
			Output::MsAdpcm { block_align } => {
				let samples = adpcm::ms_samples_per_block(block_align, channels).ok_or(EncodeError::Invalid)?;
				if samples < 3 || block_align > u16::MAX as usize {
					return Err(EncodeError::Invalid);
				}
				(block_align, samples, MS_ADPCM_FORMAT, 4)
			}
		};

		// Refused here rather than at `finish`, so a caller learns it needs to stage the output
		// before it has spent an hour of decoding on a file that cannot be completed.
		sink.patch(0, &[])?;

		let byte_rate = match output {
			Output::Pcm { .. } => format.rate() as u64 * block_align as u64,
			// For the block codecs this is an average, and it is what players use to seek: whole
			// blocks per second, rounded down, times the block size.
			_ => (format.rate() as u64 * block_align as u64).div_ceil(samples_per_block as u64),
		};
		let fmt_body = fmt_body_len(output);

		sink.write(b"RIFF")?;
		let riff_at = sink.written();
		sink.write(&0u32.to_le_bytes())?;
		sink.write(b"WAVE")?;
		sink.write(b"fmt ")?;
		sink.write(&(fmt_body as u32).to_le_bytes())?;
		sink.write(&tag.to_le_bytes())?;
		sink.write(&(channels as u16).to_le_bytes())?;
		sink.write(&format.rate().to_le_bytes())?;
		sink.write(&u32::try_from(byte_rate).map_err(|_| EncodeError::TooLarge)?.to_le_bytes())?;
		sink.write(&(block_align as u16).to_le_bytes())?;
		sink.write(&bits.to_le_bytes())?;
		let mut fact_at = None;
		if !matches!(output, Output::Pcm { .. }) {
			let extension = fmt_body - 18;
			sink.write(&(extension as u16).to_le_bytes())?;
			sink.write(&(samples_per_block as u16).to_le_bytes())?;
			if matches!(output, Output::MsAdpcm { .. }) {
				sink.write(&(MS_COEFFICIENTS.len() as u16).to_le_bytes())?;
				for (first, second) in MS_COEFFICIENTS {
					sink.write(&first.to_le_bytes())?;
					sink.write(&second.to_le_bytes())?;
				}
			}
			// `fact` carries the true frame count, which is the only thing that tells a decoder how
			// much of the final padded block was audio.
			sink.write(b"fact")?;
			sink.write(&4u32.to_le_bytes())?;
			fact_at = Some(sink.written());
			sink.write(&0u32.to_le_bytes())?;
		}
		sink.write(b"data")?;
		let data_at = sink.written();
		sink.write(&0u32.to_le_bytes())?;

		Ok(Encoder { sink, format, output, block_align, samples_per_block, placeholders: Placeholders { riff: riff_at, fact: fact_at, data: data_at }, pending: Vec::new(), block: Vec::new(), reconstructed: Vec::new(), frames: 0, data_bytes: 0 })
	}

	// Append interleaved frames at the encoder's channel count. Any number of them; the encoder
	// holds at most one block.
	pub fn push(&mut self, interleaved: &[i16]) -> Result<(), EncodeError> {
		let channels = self.format.channels() as usize;
		if interleaved.len() % channels != 0 {
			return Err(EncodeError::Invalid);
		}
		self.frames = self.frames.checked_add((interleaved.len() / channels) as u64).ok_or(EncodeError::TooLarge)?;
		match self.output {
			Output::Pcm { bits } => self.push_pcm(interleaved, bits),
			_ => self.push_blocks(interleaved),
		}
	}

	fn push_pcm(&mut self, interleaved: &[i16], bits: u16) -> Result<(), EncodeError> {
		// A small fixed staging buffer rather than one write per sample: the sink may be a channel
		// to another process, and a syscall per sample is not a design.
		let mut staged = [0u8; 512];
		let width = bits as usize / 8;
		let mut filled = 0usize;
		for &sample in interleaved {
			if filled + width > staged.len() {
				self.sink.write(&staged[..filled])?;
				self.data_bytes += filled as u64;
				filled = 0;
			}
			match bits {
				// Eight-bit WAV is unsigned, and the conversion rounds rather than truncates: the
				// decoder's inverse is `(byte - 128) << 8`, so truncation would bias every sample
				// downward by up to a full step.
				8 => {
					let value = ((sample as i32 + 128) >> 8).clamp(-128, 127) + 128;
					staged[filled] = value as u8;
				}
				16 => staged[filled..filled + 2].copy_from_slice(&sample.to_le_bytes()),
				24 => {
					let value = (sample as i32) << 8;
					staged[filled..filled + 3].copy_from_slice(&value.to_le_bytes()[..3]);
				}
				32 => {
					let value = (sample as i32) << 16;
					staged[filled..filled + 4].copy_from_slice(&value.to_le_bytes());
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

	fn push_blocks(&mut self, interleaved: &[i16]) -> Result<(), EncodeError> {
		let channels = self.format.channels() as usize;
		let block_samples = self.samples_per_block * channels;
		let mut offset = 0usize;
		while offset < interleaved.len() {
			let wanted = block_samples - self.pending.len();
			let take = core::cmp::min(wanted, interleaved.len() - offset);
			self.pending.try_reserve(take).map_err(|_| EncodeError::TooLarge)?;
			self.pending.extend_from_slice(&interleaved[offset..offset + take]);
			offset += take;
			if self.pending.len() == block_samples {
				self.emit_block()?;
			}
		}
		Ok(())
	}

	fn emit_block(&mut self) -> Result<(), EncodeError> {
		let channels = self.format.channels();
		match self.output {
			Output::ImaAdpcm { .. } => encode_ima_block(&self.pending, channels, self.block_align, &mut self.block, &mut self.reconstructed)?,
			Output::MsAdpcm { .. } => encode_ms_block(&self.pending, channels, self.block_align, &mut self.block, &mut self.reconstructed)?,
			Output::Pcm { .. } => return Err(EncodeError::Invalid),
		};
		self.sink.write(&self.block)?;
		self.data_bytes += self.block.len() as u64;
		self.pending.clear();
		Ok(())
	}

	// What the decoder will produce for the block just written. Empty for the PCM profiles, where
	// the answer is the input.
	pub fn last_block_reconstruction(&self) -> &[i16] {
		&self.reconstructed
	}

	// Close the file: flush a partial block, correct the sizes, and hand the destination back.
	pub fn finish(mut self) -> Result<(S, u64), EncodeError> {
		if !self.pending.is_empty() {
			self.emit_block()?;
		}
		// A WAVE file with no audio is one this leaf's own parser rejects, so it is not written.
		if self.frames == 0 || self.data_bytes == 0 {
			return Err(EncodeError::Invalid);
		}
		// Chunks are padded to an even length and the RIFF size counts the padding, which is the
		// difference between a file that parses to the last byte and one that reports trailing junk.
		if self.data_bytes % 2 == 1 {
			self.sink.write(&[0])?;
		}
		let data_size = u32::try_from(self.data_bytes).map_err(|_| EncodeError::TooLarge)?;
		let riff_size = self.sink.written().checked_sub(8).ok_or(EncodeError::Invalid)?;
		let riff_size = u32::try_from(riff_size).map_err(|_| EncodeError::TooLarge)?;
		self.sink.patch(self.placeholders.data, &data_size.to_le_bytes())?;
		self.sink.patch(self.placeholders.riff, &riff_size.to_le_bytes())?;
		if let Some(at) = self.placeholders.fact {
			let frames = u32::try_from(self.frames).map_err(|_| EncodeError::TooLarge)?;
			self.sink.patch(at, &frames.to_le_bytes())?;
		}
		Ok((self.sink, self.frames))
	}
}

const fn fmt_body_len(output: Output) -> usize {
	match output {
		Output::Pcm { .. } => 16,
		Output::ImaAdpcm { .. } => 20,
		// 18 fixed, plus `wSamplesPerBlock`, `wNumCoef` and the seven pairs.
		Output::MsAdpcm { .. } => 22 + MS_COEFFICIENTS.len() * 4,
	}
}
