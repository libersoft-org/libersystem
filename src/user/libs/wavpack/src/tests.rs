extern crate alloc;
extern crate std;

use super::*;
use alloc::vec::Vec;
use std::path::PathBuf;
use std::process::Command;

fn ffmpeg_fixture() -> Vec<u8> {
	let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../..");
	let output = root.join(".build/testdata/wavpack/test-stereo-short.wv");
	std::fs::create_dir_all(output.parent().unwrap()).expect("failed to create WavPack testdata directory");
	let status = Command::new("ffmpeg").args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=997:sample_rate=44100:duration=0.1"]).args(["-filter_complex", "[0:a]pan=stereo|c0=c0|c1=-1*c0[out]", "-map", "[out]", "-c:a", "wavpack"]).arg(&output).status().expect("ffmpeg is required to generate the WavPack mutation fixture");
	assert!(status.success(), "ffmpeg failed to generate the WavPack mutation fixture");
	std::fs::read(output).expect("failed to read the generated WavPack mutation fixture")
}

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
fn malformed_streams_fail_without_panicking_or_stalling() {
	let source = ffmpeg_fixture();
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
