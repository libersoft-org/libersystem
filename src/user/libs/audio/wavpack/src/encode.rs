//! Writing WavPack, block at a time, in the lossless profile this leaf can read.
//!
//! Lossless, so the test beside it asserts equality and nothing weaker. The compression is two
//! decorrelation passes over adaptive-median entropy coding - WavPack's own arithmetic, run
//! forwards - and every step is the exact inverse of the decoder in the file next door. That is not
//! a style choice: the coder is adaptive on both sides, so an encoder that computes a median or a
//! weight even slightly differently produces a stream that decodes into noise from that point on.
//!
//! What is deliberately left out of version one, and why:
//!
//! - **The declared total.** Every block header says "unknown" rather than the track length, which
//!   the format defines as zero and the decoder answers by counting. Writing the real number means
//!   going back over every block at the end, and the offsets to go back to are a list that grows
//!   with the track - the one unbounded thing in an encoder whose whole point is that it is not.
//! - **Tuned initial medians.** The entropy state starts at zero in each block and adapts upward
//!   over roughly the first three hundred samples, which is a few hundred wasted bits per block.
//!   Priming it means writing the block's own statistics into ENTROPY_VARS, which is a second pass
//!   over the block; worth doing, not worth guessing at.
//! - **Hybrid, float, extended-integer and multichannel.** The decoder refuses them and so does
//!   this: a format this tree cannot read back is one it has no business writing.

use crate::Error;
use alloc::vec::Vec;
use pcm::Format;
use pcm::encode::{Sink, SinkError};

// Frames per block. Self-contained - each block carries its own decorrelation terms and entropy
// state - so this bounds what the encoder holds, and the cost of it is the adaptation ramp above.
const BLOCK_FRAMES: usize = 4_096;
// The stream version this writes. Within the decoder's accepted range and the one real encoders use.
const VERSION: u16 = 0x410;
// Sixteen-bit samples: two bytes stored, no shift, and a magnitude that admits `i16::MIN`.
const BYTES_STORED_16: u32 = 1;
const MAGNITUDE_16: u32 = 15;

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
			_ => EncodeError::Invalid,
		}
	}
}

pub struct Encoder<S: Sink> {
	sink: S,
	format: Format,
	joint_stereo: bool,
	pending: Vec<i16>,
	frames: u64,
	// Scratch, reused between blocks.
	residuals: Vec<i32>,
	block: Vec<u8>,
}

impl<S: Sink> Encoder<S> {
	// `joint_stereo` codes the difference of the two channels instead of one of them, which is most
	// of the gain on real stereo material and costs nothing on material where it is not.
	pub fn new(sink: S, format: Format, joint_stereo: bool) -> Result<Encoder<S>, EncodeError> {
		Ok(Encoder { sink, format, joint_stereo: joint_stereo && format.channels() == 2, pending: Vec::new(), frames: 0, residuals: Vec::new(), block: Vec::new() })
	}

	pub fn push(&mut self, interleaved: &[i16]) -> Result<(), EncodeError> {
		let channels = self.format.channels() as usize;
		if interleaved.len() % channels != 0 {
			return Err(EncodeError::Invalid);
		}
		self.frames = self.frames.checked_add((interleaved.len() / channels) as u64).ok_or(EncodeError::TooLarge)?;
		self.pending.try_reserve(interleaved.len()).map_err(|_| EncodeError::TooLarge)?;
		self.pending.extend_from_slice(interleaved);
		while self.pending.len() >= BLOCK_FRAMES * channels {
			self.emit(BLOCK_FRAMES)?;
		}
		Ok(())
	}

	pub fn finish(mut self) -> Result<(S, u64), EncodeError> {
		let channels = self.format.channels() as usize;
		// A block of no samples is one the decoder rejects, so an empty track is refused rather than
		// written as a header with nothing behind it.
		if self.frames == 0 {
			return Err(EncodeError::Invalid);
		}
		while !self.pending.is_empty() {
			let held = self.pending.len() / channels;
			self.emit(held.min(BLOCK_FRAMES))?;
		}
		Ok((self.sink, self.frames))
	}

	fn emit(&mut self, block_frames: usize) -> Result<(), EncodeError> {
		let channels = self.format.channels() as usize;
		let taken = block_frames.checked_mul(channels).ok_or(EncodeError::TooLarge)?;
		if block_frames == 0 || taken > self.pending.len() {
			return Err(EncodeError::Invalid);
		}
		let block_index = self.frames - (self.pending.len() / channels) as u64;
		let coded_channels = channels;

		// The CRC the decoder recomputes as it goes, over the values it will hold after undoing
		// every transform below - which for sixteen-bit, unshifted samples are the samples.
		let mut crc = u32::MAX;
		for frame in 0..block_frames {
			let left = self.pending[frame * channels] as i32;
			crc = crc.wrapping_mul(3).wrapping_add(left as u32);
			if channels == 2 {
				let right = self.pending[frame * channels + 1] as i32;
				crc = crc.wrapping_mul(3).wrapping_add(right as u32);
			}
		}

		// Decorrelation, then entropy coding, in that order - the decoder undoes them the other way
		// round, which is why the passes are walked in reverse here.
		self.residuals.clear();
		self.residuals.try_reserve(taken).map_err(|_| EncodeError::TooLarge)?;
		let mut passes = decorrelation_passes();
		let mut position = 0usize;
		for frame in 0..block_frames {
			let mut a = self.pending[frame * channels] as i32;
			let mut b = if channels == 2 { self.pending[frame * channels + 1] as i32 } else { a };
			if channels == 2 && self.joint_stereo {
				// The decoder finishes with `right -= left >> 1; left += right`, so this is that read
				// backwards: the difference goes in the first channel and what is left of the second
				// keeps the pair reconstructible exactly.
				let difference = a.wrapping_sub(b);
				b = b.wrapping_add(difference >> 1);
				a = difference;
			}
			for pass in passes.iter_mut().rev() {
				if channels == 2 {
					(a, b) = pass.strip_stereo(a, b, position);
				} else {
					a = pass.strip_mono(a, position);
					b = a;
				}
			}
			position = (position + 1) & 7;
			self.residuals.push(a);
			if channels == 2 {
				self.residuals.push(b);
			}
		}

		let bitstream = encode_words(&self.residuals, coded_channels as u8)?;

		// The block: a fixed header, then the metadata items the decoder needs, then the bits.
		self.block.clear();
		let flags = self.flags();
		self.block.try_reserve(64 + bitstream.len()).map_err(|_| EncodeError::TooLarge)?;
		self.block.extend_from_slice(b"wvpk");
		self.block.extend_from_slice(&0u32.to_le_bytes());
		self.block.extend_from_slice(&VERSION.to_le_bytes());
		self.block.push(0);
		self.block.push(0);
		// Zero is the format's "not known", and the decoder answers with what it counted.
		self.block.extend_from_slice(&0u32.to_le_bytes());
		self.block.extend_from_slice(&u32::try_from(block_index).map_err(|_| EncodeError::TooLarge)?.to_le_bytes());
		self.block.extend_from_slice(&u32::try_from(block_frames).map_err(|_| EncodeError::TooLarge)?.to_le_bytes());
		self.block.extend_from_slice(&flags.to_le_bytes());
		self.block.extend_from_slice(&crc.to_le_bytes());

		push_item(&mut self.block, crate::ID_DECORR_TERMS, &decorrelation_bytes())?;
		push_item(&mut self.block, crate::ID_ENTROPY_VARS, &[0u8; 12][..6 * coded_channels])?;
		if rate_index(self.format.rate()).is_none() {
			let rate = self.format.rate();
			push_item(&mut self.block, crate::ID_SAMPLE_RATE, &[rate as u8, (rate >> 8) as u8, (rate >> 16) as u8])?;
		}
		push_item(&mut self.block, crate::ID_WV_BITSTREAM, &bitstream)?;

		let length = u32::try_from(self.block.len() - 8).map_err(|_| EncodeError::TooLarge)?;
		self.block[4..8].copy_from_slice(&length.to_le_bytes());
		self.sink.write(&self.block)?;
		self.pending.drain(..taken);
		Ok(())
	}

	fn flags(&self) -> u32 {
		let mut flags = BYTES_STORED_16 | (MAGNITUDE_16 << 18) | crate::INITIAL_BLOCK | crate::FINAL_BLOCK;
		if self.format.channels() == 1 {
			flags |= crate::MONO_FLAG;
		}
		if self.joint_stereo {
			flags |= crate::JOINT_STEREO;
		}
		// Fifteen is the format's "not one of the fifteen standard rates"; the rate then travels in
		// its own metadata item, which is why every rate `Format` can name is writable.
		flags |= (rate_index(self.format.rate()).unwrap_or(15) as u32) << 23;
		flags
	}
}

fn rate_index(rate: u32) -> Option<usize> {
	crate::SAMPLE_RATES.iter().position(|known| *known == rate)
}

// Two passes, the pair every WavPack encoder starts with: a second-difference predictor and a
// first-difference one. The decoder applies them in the order they come out of `parse_terms`, which
// REVERSES the bytes - so the byte order below is the pass order backwards, and getting that wrong
// produces a file that decodes without complaint into something that is not the input.
fn decorrelation_bytes() -> [u8; 2] {
	[term_byte(17, 2), term_byte(18, 2)]
}

const fn term_byte(term: i8, delta: u8) -> u8 {
	(((term + 5) as u8) & 0x1f) | (delta << 5)
}

fn decorrelation_passes() -> [Pass; 2] {
	[Pass::new(18, 2), Pass::new(17, 2)]
}

// One decorrelation pass, run forwards. Every line of it is the decoder's `DecorrPass` with the
// prediction subtracted instead of added; the weight adaptation and the history update are the
// same, because both sides must arrive at the same state after every sample.
struct Pass {
	term: i8,
	delta: i32,
	weight: i32,
	weight_b: i32,
	samples: [i32; 8],
	samples_b: [i32; 8],
}

impl Pass {
	const fn new(term: i8, delta: i32) -> Pass {
		Pass { term, delta, weight: 0, weight_b: 0, samples: [0; 8], samples_b: [0; 8] }
	}

	fn prediction(term: i8, samples: &[i32; 8], position: usize) -> (i32, usize) {
		let prediction = match term {
			17 => (samples[0] as u32).wrapping_mul(2).wrapping_sub(samples[1] as u32) as i32,
			18 => ((samples[0] as u32).wrapping_mul(3).wrapping_sub(samples[1] as u32) as i32) >> 1,
			_ => samples[position],
		};
		(prediction, if term > 8 { 0 } else { (position + term as usize) & 7 })
	}

	fn strip_mono(&mut self, sample: i32, position: usize) -> i32 {
		let (prediction, target) = Self::prediction(self.term, &self.samples, position);
		let weighted = apply_weight(self.weight, prediction);
		let residual = sample.wrapping_sub(weighted);
		if prediction != 0 && residual != 0 {
			self.weight += if prediction ^ residual < 0 { -self.delta } else { self.delta };
		}
		if self.term > 8 {
			self.samples[1] = self.samples[0];
		}
		self.samples[target] = sample;
		residual
	}

	fn strip_stereo(&mut self, left: i32, right: i32, position: usize) -> (i32, i32) {
		let (prediction_a, target) = Self::prediction(self.term, &self.samples, position);
		let (prediction_b, _) = Self::prediction(self.term, &self.samples_b, position);
		let residual_a = left.wrapping_sub(apply_weight(self.weight, prediction_a));
		let residual_b = right.wrapping_sub(apply_weight(self.weight_b, prediction_b));
		if prediction_a != 0 && residual_a != 0 {
			self.weight += if prediction_a ^ residual_a < 0 { -self.delta } else { self.delta };
		}
		if prediction_b != 0 && residual_b != 0 {
			self.weight_b += if prediction_b ^ residual_b < 0 { -self.delta } else { self.delta };
		}
		if self.term > 8 {
			self.samples[1] = self.samples[0];
			self.samples_b[1] = self.samples_b[0];
		}
		self.samples[target] = left;
		self.samples_b[target] = right;
		(residual_a, residual_b)
	}
}

fn apply_weight(weight: i32, sample: i32) -> i32 {
	(weight as u32).wrapping_mul(sample as u32).wrapping_add(512) as i32 >> 10
}

// A metadata item: an identifier, a length in sixteen-bit words, and the data padded to that.
fn push_item(block: &mut Vec<u8>, id: u8, data: &[u8]) -> Result<(), EncodeError> {
	let words = data.len().div_ceil(2);
	if words > 0x00ff_ffff {
		return Err(EncodeError::TooLarge);
	}
	let mut raw = id;
	if data.len() % 2 == 1 {
		raw |= crate::ID_ODD_SIZE;
	}
	// The word count is nine bits wide in the short form; anything longer says so and carries the
	// rest in two more bytes. The bitstream is always longer.
	if words > 0xff {
		raw |= crate::ID_LARGE;
		block.extend_from_slice(&[raw, (words & 0xff) as u8, ((words >> 8) & 0xff) as u8, ((words >> 16) & 0xff) as u8]);
	} else {
		block.extend_from_slice(&[raw, words as u8]);
	}
	block.try_reserve(words * 2).map_err(|_| EncodeError::TooLarge)?;
	block.extend_from_slice(data);
	if data.len() % 2 == 1 {
		block.push(0);
	}
	Ok(())
}

// The entropy coder, forwards.
//
// The awkward part is the ones-count, which the format packs two samples at a time: the parity of
// each count says whether the NEXT sample codes its own count or is forced to zero. So the encoder
// has to know the next sample's bucket before it can write this one's, and the next sample's bucket
// depends on the medians this one is about to move. It is resolved by advancing a COPY of the
// medians one step - cheap, and the only alternative is buffering the whole block's buckets.
fn encode_words(residuals: &[i32], coded_channels: u8) -> Result<Vec<u8>, EncodeError> {
	let mut out = BitWriter::new();
	out.bytes.try_reserve(residuals.len()).map_err(|_| EncodeError::TooLarge)?;
	let mut median = [[0u32; 3]; 2];
	let mut holding_zero = false;
	let mut holding_one = false;
	let channel_of = |index: usize| if coded_channels == 2 { index & 1 } else { 0 };

	for index in 0..residuals.len() {
		let channel = channel_of(index);
		// The decoder looks for a zero-run whenever both first medians have fallen to nothing. It is
		// answered with an empty run rather than used: runs pay only in digital silence, and a wrong
		// guess about when the decoder will read this field desynchronises the whole stream.
		if median[0][0] & !1 == 0 && median[1][0] & !1 == 0 && !holding_zero && !holding_one {
			out.unary(0);
		}
		let magnitude = magnitude_of(residuals[index]);
		let ones = bucket(magnitude, &median[channel]);

		if holding_zero {
			// The previous sample's parity already said this one codes nothing, and it could only
			// say that because this bucket is zero.
			debug_assert_eq!(ones, 0);
			holding_zero = false;
		} else {
			let low = u32::from(holding_one);
			// One step of lookahead, on a copy, so the parity below can promise what comes next.
			let next_ones = if index + 1 < residuals.len() {
				let mut ahead = median;
				advance(&mut ahead[channel], ones);
				bucket(magnitude_of(residuals[index + 1]), &ahead[channel_of(index + 1)])
			} else {
				0
			};
			let parity = u32::from(next_ones != 0);
			let count = (ones - low) * 2 + parity;
			write_count(&mut out, count);
			holding_one = count & 1 != 0;
			holding_zero = count & 1 == 0;
		}

		let (low, high) = span(&mut median[channel], ones);
		out.code(magnitude - low, high - low);
		out.bit(residuals[index] < 0);
	}
	Ok(out.finish())
}

// Signed to the format's magnitude-and-sign: the decoder returns `!magnitude` for a negative, so a
// negative residual's magnitude is one less than its absolute value.
const fn magnitude_of(residual: i32) -> u32 {
	if residual < 0 { !residual as u32 } else { residual as u32 }
}

const fn get(median: u32) -> u32 {
	(median >> 4) + 1
}

// Which bucket a magnitude falls in, given the medians. The inverse of the decoder's `range`.
fn bucket(magnitude: u32, median: &[u32; 3]) -> u32 {
	let first = get(median[0]);
	if magnitude < first {
		return 0;
	}
	let second = first + get(median[1]);
	if magnitude < second {
		return 1;
	}
	2 + (magnitude - second) / get(median[2])
}

// The bucket's magnitude span, moving the medians exactly as the decoder's `range` does.
fn span(median: &mut [u32; 3], ones: u32) -> (u32, u32) {
	if ones == 0 {
		let high = get(median[0]) - 1;
		median[0] = median[0].wrapping_sub(((median[0] + 126) / 128) * 2);
		return (0, high);
	}
	let mut low = get(median[0]);
	median[0] = median[0].wrapping_add(((median[0] + 128) / 128) * 5);
	if ones == 1 {
		let high = low + get(median[1]) - 1;
		median[1] = median[1].wrapping_sub(((median[1] + 62) / 64) * 2);
		return (low, high);
	}
	low += get(median[1]);
	median[1] = median[1].wrapping_add(((median[1] + 64) / 64) * 5);
	if ones == 2 {
		let high = low + get(median[2]) - 1;
		median[2] = median[2].wrapping_sub(((median[2] + 30) / 32) * 2);
		return (low, high);
	}
	low += (ones - 2) * get(median[2]);
	let high = low + get(median[2]) - 1;
	median[2] = median[2].wrapping_add(((median[2] + 32) / 32) * 5);
	(low, high)
}

// The same movement without committing, for the one-step lookahead.
fn advance(median: &mut [u32; 3], ones: u32) {
	let _ = span(median, ones);
}

// The ones-count: up to fifteen in unary, and an escape above that which is itself a unary length
// followed by the value's low bits.
fn write_count(out: &mut BitWriter, count: u32) {
	if count < 16 {
		out.unary(count);
		return;
	}
	out.unary(16);
	let escape = count - 16;
	if escape < 2 {
		out.unary(escape);
		return;
	}
	let bits = 32 - escape.leading_zeros();
	out.unary(bits);
	out.write(escape, (bits - 1) as u8);
}

// Bits go out least-significant first within each byte, which is the order the reader takes them in.
struct BitWriter {
	bytes: Vec<u8>,
	current: u8,
	bit: u8,
}

impl BitWriter {
	fn new() -> BitWriter {
		BitWriter { bytes: Vec::new(), current: 0, bit: 0 }
	}

	fn bit(&mut self, set: bool) {
		if set {
			self.current |= 1 << self.bit;
		}
		self.bit += 1;
		if self.bit == 8 {
			self.bytes.push(self.current);
			self.current = 0;
			self.bit = 0;
		}
	}

	fn write(&mut self, value: u32, count: u8) {
		for shift in 0..count {
			self.bit(value & (1 << shift) != 0);
		}
	}

	// `count` ones, then a zero - which is what the reader counts up to its first zero.
	fn unary(&mut self, count: u32) {
		for _ in 0..count {
			self.bit(true);
		}
		self.bit(false);
	}

	// The format's bounded code: values below the number of short slots take one bit fewer.
	fn code(&mut self, value: u32, max: u32) {
		let bits = 32 - max.leading_zeros();
		if bits == 0 {
			return;
		}
		let extras = (1u64 << bits) - u64::from(max) - 1;
		if u64::from(value) < extras {
			self.write(value, (bits - 1) as u8);
		} else {
			let combined = u64::from(value) + extras;
			self.write((combined >> 1) as u32, (bits - 1) as u8);
			self.bit(combined & 1 != 0);
		}
	}

	fn finish(mut self) -> Vec<u8> {
		if self.bit != 0 {
			self.bytes.push(self.current);
		}
		// A block whose bitstream is empty is one the decoder rejects, and the reader is allowed to
		// run past the last sample's bits, so a byte of slack costs nothing and prevents both.
		self.bytes.push(0);
		self.bytes
	}
}
