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
