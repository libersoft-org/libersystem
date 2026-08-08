//! Encoding, block at a time, as the exact inverse of the decoders beside it.
//!
//! Both codecs here are adaptive differential: the decoder's output depends on every nibble before
//! it, so an encoder that guesses at the decoder's state produces a file that drifts. The way that
//! is avoided is not care - it is that each `encode_*_block` runs the decoder's own update, sample
//! by sample, and encodes the next difference against the value the decoder will actually hold.
//! The reconstruction that falls out of doing it that way is handed back to the caller, which is
//! what lets a test assert exact equality against a lossy codec: not "close enough", but the very
//! samples the decoder is going to produce.
//!
//! Blocks are fixed size, so the last one is padded by holding the final sample. The container is
//! responsible for recording the true frame count - `fact` in RIFF - so that the padding is trimmed
//! on the way back rather than heard.

use crate::{Error, IMA_INDEX, IMA_STEP, ImaState, MS_ADAPTATION, MsState, ima_samples_per_block, ms_samples_per_block};
use alloc::vec::Vec;

// The seven coefficient pairs every Microsoft ADPCM decoder is expected to know, written into the
// `fmt ` chunk by the container so the file stays self-describing.
pub const MS_COEFFICIENTS: [(i16, i16); 7] = [(256, 0), (512, -256), (0, 0), (192, 64), (240, 0), (460, -208), (392, -232)];

// Encode one IMA ADPCM block.
//
// `samples` is interleaved at `channels` and is padded here if it is short of a whole block, so a
// caller streams whatever it has and the container records how much of the last block was real.
// Both output vectors are cleared. The return is the block's sample count per channel.
pub fn encode_ima_block(samples: &[i16], channels: u8, block_align: usize, block: &mut Vec<u8>, reconstructed: &mut Vec<i16>) -> Result<usize, Error> {
	let samples_per_block = ima_samples_per_block(block_align, channels).ok_or(Error::Invalid)?;
	let channels = channels as usize;
	if samples_per_block == 0 || samples.is_empty() || samples.len() % channels != 0 {
		return Err(Error::Invalid);
	}
	let frames = samples.len() / channels;
	if frames > samples_per_block {
		return Err(Error::Invalid);
	}
	block.try_reserve_exact(block_align).map_err(|_| Error::TooLarge)?;
	reconstructed.try_reserve_exact(samples_per_block * channels).map_err(|_| Error::TooLarge)?;
	block.clear();
	reconstructed.clear();

	// Held, so a short final block repeats its last frame rather than falling to silence: a click
	// at the end of every converted file is the kind of defect that survives a passing test.
	let at = |frame: usize, channel: usize| samples[frame.min(frames - 1) * channels + channel];

	let mut states = [ImaState { predictor: 0, index: 0 }; 2];
	for channel in 0..channels {
		// The first sample of a block is carried exactly in the header, which is also the decoder's
		// starting predictor. Choosing the step index is the encoder's only free choice here; zero
		// is the smallest step and adaptation climbs to the signal within a few samples.
		let first = at(0, channel);
		states[channel] = ImaState { predictor: first as i32, index: 0 };
		block.extend_from_slice(&first.to_le_bytes());
		block.push(0);
		block.push(0);
	}
	for channel in 0..channels {
		reconstructed.push(states[channel].predictor as i16);
	}

	// Mono packs two samples per byte, low nibble first. Stereo interleaves in groups of four bytes
	// per channel - eight samples each - which is the layout the decoder unpacks.
	if channels == 1 {
		// A pending half-byte, tracked rather than inferred from the sample count's parity. The
		// first version worked that out from `samples_per_block` and got the Microsoft case exactly
		// wrong, because the two codecs carry a different number of samples in their headers - one
		// against two - so the same parity means opposite things. Held explicitly, it cannot.
		let mut half = None;
		for index in 1..samples_per_block {
			let nibble = ima_nibble(&mut states[0], at(index, 0));
			reconstructed.push(states[0].predictor as i16);
			match half.take() {
				None => half = Some(nibble),
				Some(low) => block.push(low | (nibble << 4)),
			}
		}
		// An unpaired nibble is padded with a zero difference, which the container's frame count
		// makes inaudible.
		if let Some(low) = half {
			block.push(low);
		}
	} else {
		let mut index = 1usize;
		let mut pending = [[0i16; 8]; 2];
		while index < samples_per_block {
			let run = core::cmp::min(8, samples_per_block - index);
			for channel in 0..2 {
				let mut nibbles = [0u8; 8];
				for step in 0..run {
					nibbles[step] = ima_nibble(&mut states[channel], at(index + step, channel));
					pending[channel][step] = states[channel].predictor as i16;
				}
				for pair in 0..4 {
					block.push(nibbles[pair * 2] | (nibbles[pair * 2 + 1] << 4));
				}
			}
			for step in 0..run {
				reconstructed.push(pending[0][step]);
				reconstructed.push(pending[1][step]);
			}
			index += 8;
		}
	}

	// The block is fixed size and the arithmetic above should have filled it exactly. Padding a
	// short one would hide a packing bug behind a file that still decodes, so it is an error.
	if block.len() != block_align {
		return Err(Error::Invalid);
	}
	Ok(samples_per_block)
}

// Quantise one difference against the decoder's current state, and advance that state exactly as
// `ima_sample` will on the way back.
fn ima_nibble(state: &mut ImaState, sample: i16) -> u8 {
	let step = IMA_STEP[state.index as usize] as i32;
	let mut difference = sample as i32 - state.predictor;
	let mut nibble = 0u8;
	if difference < 0 {
		nibble = 8;
		difference = -difference;
	}
	let mut delta = step >> 3;
	if difference >= step {
		nibble |= 4;
		difference -= step;
		delta += step;
	}
	if difference >= step >> 1 {
		nibble |= 2;
		difference -= step >> 1;
		delta += step >> 1;
	}
	if difference >= step >> 2 {
		nibble |= 1;
		delta += step >> 2;
	}
	if nibble & 8 != 0 {
		state.predictor -= delta;
	} else {
		state.predictor += delta;
	}
	state.predictor = state.predictor.clamp(i16::MIN as i32, i16::MAX as i32);
	state.index = (state.index + IMA_INDEX[nibble as usize] as i32).clamp(0, 88);
	nibble
}

// Encode one Microsoft ADPCM block, choosing the predictor pair by trying all seven.
//
// Seven trial encodes of a block is not free, and it is the difference between a file that tracks
// the signal and one that does not: the pairs are second-order predictors and which of them fits
// depends entirely on the material. The choice is by least total absolute error with ties to the
// lowest index, so the same input gives the same file every time.
pub fn encode_ms_block(samples: &[i16], channels: u8, block_align: usize, block: &mut Vec<u8>, reconstructed: &mut Vec<i16>) -> Result<usize, Error> {
	let samples_per_block = ms_samples_per_block(block_align, channels).ok_or(Error::Invalid)?;
	let channel_count = channels as usize;
	if samples_per_block < 2 || samples.is_empty() || samples.len() % channel_count != 0 {
		return Err(Error::Invalid);
	}
	let frames = samples.len() / channel_count;
	if frames > samples_per_block {
		return Err(Error::Invalid);
	}
	block.try_reserve_exact(block_align).map_err(|_| Error::TooLarge)?;
	reconstructed.try_reserve_exact(samples_per_block * channel_count).map_err(|_| Error::TooLarge)?;
	block.clear();
	reconstructed.clear();

	let at = |frame: usize, channel: usize| samples[frame.min(frames - 1) * channel_count + channel];

	// The first two frames go in the header exactly, in the order the decoder emits them: sample2 is
	// frame zero and sample1 is frame one.
	let mut chosen = [0usize; 2];
	let mut deltas = [0i32; 2];
	for channel in 0..channel_count {
		deltas[channel] = initial_delta(samples, channel_count, channel, frames);
		let mut best = (0usize, i64::MAX);
		for (index, &coefficient) in MS_COEFFICIENTS.iter().enumerate() {
			let mut state = MsState { coefficient, delta: deltas[channel], sample1: at(1, channel) as i32, sample2: at(0, channel) as i32 };
			let mut error = 0i64;
			for frame in 2..samples_per_block {
				let wanted = at(frame, channel);
				let nibble = ms_nibble(&mut state, wanted);
				let _ = nibble;
				error += (wanted as i64 - state.sample1 as i64).abs();
			}
			if error < best.1 {
				best = (index, error);
			}
		}
		chosen[channel] = best.0;
	}

	for channel in 0..channel_count {
		block.push(chosen[channel] as u8);
	}
	for channel in 0..channel_count {
		block.extend_from_slice(&(deltas[channel] as i16).to_le_bytes());
	}
	for channel in 0..channel_count {
		block.extend_from_slice(&at(1, channel).to_le_bytes());
	}
	for channel in 0..channel_count {
		block.extend_from_slice(&at(0, channel).to_le_bytes());
	}
	for channel in 0..channel_count {
		reconstructed.push(at(0, channel));
	}
	for channel in 0..channel_count {
		reconstructed.push(at(1, channel));
	}

	let mut states = [MsState { coefficient: (0, 0), delta: 0, sample1: 0, sample2: 0 }; 2];
	for channel in 0..channel_count {
		states[channel] = MsState { coefficient: MS_COEFFICIENTS[chosen[channel]], delta: deltas[channel], sample1: at(1, channel) as i32, sample2: at(0, channel) as i32 };
	}

	// Mono is two samples per byte, high nibble first; stereo is one byte per frame, channel zero in
	// the high nibble. Both are the decoder's order read backwards.
	if channel_count == 1 {
		// High nibble first here, where IMA puts the low one first. Same explicit pending half-byte.
		let mut half = None;
		for frame in 2..samples_per_block {
			let nibble = ms_nibble(&mut states[0], at(frame, 0));
			reconstructed.push(states[0].sample1 as i16);
			match half.take() {
				None => half = Some(nibble << 4),
				Some(high) => block.push(high | nibble),
			}
		}
		if let Some(high) = half {
			block.push(high);
		}
	} else {
		for frame in 2..samples_per_block {
			let high = ms_nibble(&mut states[0], at(frame, 0));
			let low = ms_nibble(&mut states[1], at(frame, 1));
			block.push((high << 4) | low);
			reconstructed.push(states[0].sample1 as i16);
			reconstructed.push(states[1].sample1 as i16);
		}
	}

	if block.len() != block_align {
		return Err(Error::Invalid);
	}
	Ok(samples_per_block)
}

// A starting step size, from the average frame-to-frame movement in this block.
//
// The floor of 16 is the decoder's: a header delta below it is rejected outright. Starting from a
// measurement rather than from that floor matters because the adaptation table climbs by at most
// 3x per sample, so a loud block that starts at 16 spends its opening on a ramp that is audible.
fn initial_delta(samples: &[i16], channels: usize, channel: usize, frames: usize) -> i32 {
	if frames < 2 {
		return 16;
	}
	let mut total = 0i64;
	for frame in 1..frames {
		let previous = samples[(frame - 1) * channels + channel] as i64;
		let current = samples[frame * channels + channel] as i64;
		total += (current - previous).abs();
	}
	let mean = total / (frames as i64 - 1);
	(mean / 4).clamp(16, 16_384) as i32
}

// Quantise one sample against the decoder's current state, and advance that state exactly as
// `ms_sample` will on the way back.
fn ms_nibble(state: &mut MsState, sample: i16) -> u8 {
	let predicted = (state.sample1 * state.coefficient.0 as i32 + state.sample2 * state.coefficient.1 as i32) / 256;
	let error = sample as i32 - predicted;
	let delta = state.delta;
	let rounded = if error >= 0 { (error + delta / 2) / delta } else { -((-error + delta / 2) / delta) };
	let signed = rounded.clamp(-8, 7);
	let nibble = (signed & 0x0f) as u8;
	let reconstructed = (predicted + signed * delta).clamp(i16::MIN as i32, i16::MAX as i32);
	state.sample2 = state.sample1;
	state.sample1 = reconstructed;
	state.delta = (state.delta * MS_ADAPTATION[nibble as usize] / 256).max(16);
	nibble
}
