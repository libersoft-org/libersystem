#![no_std]

extern crate alloc;

use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Truncated,
	Invalid,
	Unsupported,
	TooLarge,
}

const IMA_INDEX: [i8; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];
const IMA_STEP: [i16; 89] = [
	7,
	8,
	9,
	10,
	11,
	12,
	13,
	14,
	16,
	17,
	19,
	21,
	23,
	25,
	28,
	31,
	34,
	37,
	41,
	45,
	50,
	55,
	60,
	66,
	73,
	80,
	88,
	97,
	107,
	118,
	130,
	143,
	157,
	173,
	190,
	209,
	230,
	253,
	279,
	307,
	337,
	371,
	408,
	449,
	494,
	544,
	598,
	658,
	724,
	796,
	876,
	963,
	1060,
	1166,
	1282,
	1411,
	1552,
	1707,
	1878,
	2066,
	2272,
	2499,
	2749,
	3024,
	3327,
	3660,
	4026,
	4428,
	4871,
	5358,
	5894,
	6484,
	7132,
	7845,
	8630,
	9493,
	10442,
	11487,
	12635,
	13899,
	15289,
	16818,
	18500,
	20350,
	22385,
	24623,
	27086,
	29794,
	32767,
];

#[derive(Clone, Copy)]
struct ImaState {
	predictor: i32,
	index: i32,
}

pub fn ima_samples_per_block(block_align: usize, channels: u8) -> Option<usize> {
	if !(1..=2).contains(&channels) {
		return None;
	}
	let header = 4usize.checked_mul(channels as usize)?;
	let payload = block_align.checked_sub(header)?;
	payload.checked_mul(2)?.checked_div(channels as usize)?.checked_add(1)
}

pub fn decode_ima_block(block: &[u8], channels: u8, samples_per_block: usize, output: &mut Vec<u8>) -> Result<usize, Error> {
	let expected = ima_samples_per_block(block.len(), channels).ok_or(Error::Invalid)?;
	if samples_per_block != expected || samples_per_block == 0 {
		return Err(Error::Invalid);
	}
	let channels = channels as usize;
	let header_len = channels.checked_mul(4).ok_or(Error::TooLarge)?;
	let headers = block.get(..header_len).ok_or(Error::Truncated)?;
	let mut states = [ImaState { predictor: 0, index: 0 }; 2];
	output.clear();
	reserve_pcm(output, samples_per_block, channels)?;
	for channel in 0..channels {
		let offset = channel * 4;
		let predictor = i16::from_le_bytes([headers[offset], headers[offset + 1]]);
		let index = headers[offset + 2];
		if index > 88 || headers[offset + 3] != 0 {
			return Err(Error::Invalid);
		}
		states[channel] = ImaState { predictor: predictor as i32, index: index as i32 };
		push_i16(output, predictor);
	}
	let payload = &block[header_len..];
	let mut produced = 1usize;
	if channels == 1 {
		for &byte in payload {
			for nibble in [byte & 0x0f, byte >> 4] {
				if produced == samples_per_block {
					return Err(Error::Invalid);
				}
				push_i16(output, ima_sample(&mut states[0], nibble));
				produced += 1;
			}
		}
	} else {
		if payload.len() % 8 != 0 {
			return Err(Error::Invalid);
		}
		for group in payload.chunks_exact(8) {
			let mut decoded = [[0i16; 8]; 2];
			for channel in 0..2 {
				for (index, &byte) in group[channel * 4..channel * 4 + 4].iter().enumerate() {
					decoded[channel][index * 2] = ima_sample(&mut states[channel], byte & 0x0f);
					decoded[channel][index * 2 + 1] = ima_sample(&mut states[channel], byte >> 4);
				}
			}
			for index in 0..8 {
				if produced == samples_per_block {
					return Err(Error::Invalid);
				}
				push_i16(output, decoded[0][index]);
				push_i16(output, decoded[1][index]);
				produced += 1;
			}
		}
	}
	if produced != samples_per_block {
		return Err(Error::Truncated);
	}
	Ok(produced)
}

fn ima_sample(state: &mut ImaState, nibble: u8) -> i16 {
	let step = IMA_STEP[state.index as usize] as i32;
	let mut difference = step >> 3;
	if nibble & 1 != 0 {
		difference += step >> 2;
	}
	if nibble & 2 != 0 {
		difference += step >> 1;
	}
	if nibble & 4 != 0 {
		difference += step;
	}
	if nibble & 8 != 0 {
		state.predictor -= difference;
	} else {
		state.predictor += difference;
	}
	state.predictor = state.predictor.clamp(i16::MIN as i32, i16::MAX as i32);
	state.index = (state.index + IMA_INDEX[nibble as usize] as i32).clamp(0, 88);
	state.predictor as i16
}

const MS_ADAPTATION: [i32; 16] = [230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230];

#[derive(Clone, Copy)]
struct MsState {
	coefficient: (i16, i16),
	delta: i32,
	sample1: i32,
	sample2: i32,
}

pub fn ms_samples_per_block(block_align: usize, channels: u8) -> Option<usize> {
	if !(1..=2).contains(&channels) {
		return None;
	}
	let header = 7usize.checked_mul(channels as usize)?;
	block_align.checked_sub(header)?.checked_mul(2)?.checked_div(channels as usize)?.checked_add(2)
}

pub fn decode_ms_block(block: &[u8], channels: u8, samples_per_block: usize, coefficients: &[(i16, i16)], output: &mut Vec<u8>) -> Result<usize, Error> {
	let expected = ms_samples_per_block(block.len(), channels).ok_or(Error::Invalid)?;
	if samples_per_block != expected || coefficients.is_empty() || coefficients.len() > 32 {
		return Err(Error::Invalid);
	}
	let channels = channels as usize;
	let header_len = channels.checked_mul(7).ok_or(Error::TooLarge)?;
	let header = block.get(..header_len).ok_or(Error::Truncated)?;
	let mut states = [MsState { coefficient: (0, 0), delta: 0, sample1: 0, sample2: 0 }; 2];
	for channel in 0..channels {
		let predictor = header[channel] as usize;
		let coefficient = *coefficients.get(predictor).ok_or(Error::Invalid)?;
		let delta_offset = channels + channel * 2;
		let sample1_offset = channels * 3 + channel * 2;
		let sample2_offset = channels * 5 + channel * 2;
		let delta = i16::from_le_bytes([header[delta_offset], header[delta_offset + 1]]) as i32;
		if delta < 16 {
			return Err(Error::Invalid);
		}
		states[channel] = MsState { coefficient, delta, sample1: i16::from_le_bytes([header[sample1_offset], header[sample1_offset + 1]]) as i32, sample2: i16::from_le_bytes([header[sample2_offset], header[sample2_offset + 1]]) as i32 };
	}
	output.clear();
	reserve_pcm(output, samples_per_block, channels)?;
	for state in states.iter().take(channels) {
		push_i16(output, state.sample2 as i16);
	}
	for state in states.iter().take(channels) {
		push_i16(output, state.sample1 as i16);
	}
	let mut produced = 2usize;
	if channels == 1 {
		for &byte in &block[header_len..] {
			for nibble in [byte >> 4, byte & 0x0f] {
				if produced == samples_per_block {
					return Err(Error::Invalid);
				}
				push_i16(output, ms_sample(&mut states[0], nibble));
				produced += 1;
			}
		}
	} else {
		for &byte in &block[header_len..] {
			if produced == samples_per_block {
				return Err(Error::Invalid);
			}
			push_i16(output, ms_sample(&mut states[0], byte >> 4));
			push_i16(output, ms_sample(&mut states[1], byte & 0x0f));
			produced += 1;
		}
	}
	if produced != samples_per_block {
		return Err(Error::Truncated);
	}
	Ok(produced)
}

fn ms_sample(state: &mut MsState, nibble: u8) -> i16 {
	let signed = if nibble & 8 != 0 { nibble as i32 - 16 } else { nibble as i32 };
	let predicted = (state.sample1 * state.coefficient.0 as i32 + state.sample2 * state.coefficient.1 as i32) / 256 + signed * state.delta;
	let sample = predicted.clamp(i16::MIN as i32, i16::MAX as i32);
	state.sample2 = state.sample1;
	state.sample1 = sample;
	state.delta = (state.delta * MS_ADAPTATION[nibble as usize] / 256).max(16);
	sample as i16
}

fn reserve_pcm(output: &mut Vec<u8>, frames: usize, channels: usize) -> Result<(), Error> {
	let bytes = frames.checked_mul(channels).and_then(|samples| samples.checked_mul(2)).ok_or(Error::TooLarge)?;
	output.try_reserve_exact(bytes).map_err(|_| Error::TooLarge)
}

fn push_i16(output: &mut Vec<u8>, sample: i16) {
	output.extend_from_slice(&sample.to_le_bytes());
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
	use super::*;
	use alloc::vec;

	#[test]
	fn ima_mono_decodes_header_and_low_then_high_nibbles() {
		let block = [0, 0, 0, 0, 0x11, 0x88];
		let mut output = Vec::new();
		assert_eq!(ima_samples_per_block(block.len(), 1), Some(5));
		assert_eq!(decode_ima_block(&block, 1, 5, &mut output), Ok(5));
		let samples: Vec<i16> = output.chunks_exact(2).map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]])).collect();
		assert_eq!(samples, vec![0, 1, 2, 2, 2]);
	}

	#[test]
	fn ima_stereo_keeps_channel_groups_interleaved_by_frame() {
		let mut block = vec![0u8; 16];
		block[4..6].copy_from_slice(&100i16.to_le_bytes());
		block[8..12].fill(0x11);
		block[12..16].fill(0x88);
		let mut output = Vec::new();
		assert_eq!(ima_samples_per_block(block.len(), 2), Some(9));
		assert_eq!(decode_ima_block(&block, 2, 9, &mut output), Ok(9));
		let samples: Vec<i16> = output.chunks_exact(2).map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]])).collect();
		assert_eq!(&samples[..4], &[0, 100, 1, 100]);
	}

	#[test]
	fn microsoft_mono_uses_predictor_delta_and_high_nibble_first() {
		let mut block = vec![0u8; 8];
		block[1..3].copy_from_slice(&16i16.to_le_bytes());
		block[3..5].copy_from_slice(&100i16.to_le_bytes());
		block[5..7].copy_from_slice(&50i16.to_le_bytes());
		block[7] = 0x10;
		let mut output = Vec::new();
		assert_eq!(ms_samples_per_block(block.len(), 1), Some(4));
		assert_eq!(decode_ms_block(&block, 1, 4, &[(256, 0)], &mut output), Ok(4));
		let samples: Vec<i16> = output.chunks_exact(2).map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]])).collect();
		assert_eq!(samples, vec![50, 100, 116, 116]);
	}

	#[test]
	fn malformed_headers_and_block_geometry_fail_cleanly() {
		let mut output = Vec::new();
		assert_eq!(decode_ima_block(&[0, 0, 89, 0, 0], 1, 3, &mut output), Err(Error::Invalid));
		assert_eq!(decode_ima_block(&[0; 8], 3, 1, &mut output), Err(Error::Invalid));
		assert_eq!(decode_ms_block(&[0; 8], 1, 4, &[], &mut output), Err(Error::Invalid));
		assert_eq!(decode_ms_block(&[0; 7], 1, 3, &[(256, 0)], &mut output), Err(Error::Invalid));
	}
}
