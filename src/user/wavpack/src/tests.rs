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
