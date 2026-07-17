use super::*;
use alloc::vec;

fn wave(bits: u16, channels: u16, rate: u32, samples: &[u8], extra: Option<(&[u8; 4], &[u8])>) -> Vec<u8> {
	let sample_bytes = bits as usize / 8;
	let block_align = channels as usize * sample_bytes;
	let mut body = Vec::new();
	body.extend_from_slice(b"WAVE");
	body.extend_from_slice(b"fmt ");
	body.extend_from_slice(&16u32.to_le_bytes());
	body.extend_from_slice(&PCM_FORMAT.to_le_bytes());
	body.extend_from_slice(&channels.to_le_bytes());
	body.extend_from_slice(&rate.to_le_bytes());
	body.extend_from_slice(&(rate * block_align as u32).to_le_bytes());
	body.extend_from_slice(&(block_align as u16).to_le_bytes());
	body.extend_from_slice(&bits.to_le_bytes());
	if let Some((kind, bytes)) = extra {
		body.extend_from_slice(kind);
		body.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
		body.extend_from_slice(bytes);
		if bytes.len() & 1 != 0 {
			body.push(0);
		}
	}
	body.extend_from_slice(b"data");
	body.extend_from_slice(&(samples.len() as u32).to_le_bytes());
	body.extend_from_slice(samples);
	if samples.len() & 1 != 0 {
		body.push(0);
	}
	let mut output = b"RIFF".to_vec();
	output.extend_from_slice(&(body.len() as u32).to_le_bytes());
	output.extend_from_slice(&body);
	output
}

#[test]
fn parses_metadata_and_decodes_in_bounded_chunks() {
	let bytes = wave(16, 2, 48_000, &[0x34, 0x12, 0xfe, 0xff, 0, 0, 0xff, 0x7f], None);
	let wav = Wav::parse(&bytes).unwrap();
	assert_eq!(wav.metadata(), Metadata { rate: 48_000, channels: 2, bits_per_sample: 16, frames: 2, duration_ms: 0 });
	let mut decoder = wav.decoder();
	let mut output = Vec::new();
	assert_eq!(decoder.read_i16_le(1, &mut output), Ok(1));
	assert_eq!(output, vec![0x34, 0x12, 0xfe, 0xff]);
	assert_eq!(decoder.read_i16_le(8, &mut output), Ok(1));
	assert_eq!(output, vec![0, 0, 0xff, 0x7f]);
	assert_eq!(decoder.read_i16_le(8, &mut output), Ok(0));
}

#[test]
fn converts_unsigned_8_and_signed_24_32_bit_pcm() {
	for (bits, source, expected) in [
		(8, vec![0, 128, 255], vec![0x00, 0x80, 0, 0, 0x00, 0x7f]),
		(24, vec![0, 0, 0x80, 0, 0, 0, 0xff, 0xff, 0x7f], vec![0x00, 0x80, 0, 0, 0xff, 0x7f]),
		(32, vec![0, 0, 0, 0x80, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0x7f], vec![0x00, 0x80, 0, 0, 0xff, 0x7f]),
	] {
		let bytes = wave(bits, 1, 8_000, &source, None);
		let wav = Wav::parse(&bytes).unwrap();
		let mut output = Vec::new();
		assert_eq!(wav.decoder().read_i16_le(16, &mut output), Ok(3));
		assert_eq!(output, expected);
	}
}

#[test]
fn skips_odd_unknown_chunks_and_rejects_bad_structure() {
	let bytes = wave(16, 1, 8_000, &[1, 0], Some((b"JUNK", &[1, 2, 3])));
	assert!(Wav::parse(&bytes).is_ok());
	assert!(matches!(Wav::parse(b"RIFF"), Err(Error::Truncated)));
	let mut truncated = bytes.clone();
	truncated.pop();
	assert!(matches!(Wav::parse(&truncated), Err(Error::Truncated)));
	let mut bad_align = bytes;
	bad_align[32..34].copy_from_slice(&4u16.to_le_bytes());
	assert!(matches!(Wav::parse(&bad_align), Err(Error::Invalid)));
}

#[test]
fn rejects_unsupported_codec_rate_channels_and_partial_frames() {
	let mut codec = wave(16, 1, 8_000, &[0, 0], None);
	codec[20..22].copy_from_slice(&3u16.to_le_bytes());
	assert!(matches!(Wav::parse(&codec), Err(Error::Unsupported)));
	assert!(matches!(Wav::parse(&wave(16, 3, 8_000, &[0; 6], None)), Err(Error::Unsupported)));
	assert!(matches!(Wav::parse(&wave(16, 1, 96_000, &[0, 0], None)), Err(Error::Unsupported)));
	assert!(matches!(Wav::parse(&wave(24, 1, 8_000, &[0, 0], None)), Err(Error::Invalid)));
}

#[test]
fn staged_test_is_long_non_silent_pcm() {
	let wav = Wav::parse(include_bytes!("../../../volume/test.wav")).unwrap();
	assert_eq!(wav.metadata(), Metadata { rate: 44_100, channels: 1, bits_per_sample: 16, frames: 328_104, duration_ms: 7_440 });
	let mut output = Vec::new();
	assert_eq!(wav.decoder().read_i16_le(1_024, &mut output), Ok(1_024));
	assert_eq!(output.len(), 2_048);
	assert!(output.iter().any(|byte| *byte != 0));
}

#[test]
fn staged_adpcm_tests_decode_through_the_container_boundary() {
	for bytes in [include_bytes!("../../../volume/test-ima.wav").as_slice(), include_bytes!("../../../volume/test-ms.wav").as_slice()] {
		let wav = Wav::parse(bytes).unwrap();
		assert_eq!(wav.metadata().rate, 44_100);
		assert_eq!(wav.metadata().channels, 1);
		assert_eq!(wav.metadata().bits_per_sample, 4);
		assert_eq!(wav.metadata().frames, 328_104);
		let mut output = Vec::new();
		assert_eq!(wav.decoder().read_i16_le(1_024, &mut output), Ok(1_024));
		assert_eq!(output.len(), 2_048);
		assert!(output.iter().any(|byte| *byte != 0));
	}
}
