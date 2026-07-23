use super::*;
use alloc::vec;

fn extended(rate: u32) -> [u8; 10] {
	let leading = 31 - rate.leading_zeros();
	let exponent = 16_383 + leading;
	let mantissa = (rate as u64) << (63 - leading);
	let mut bytes = [0u8; 10];
	bytes[..2].copy_from_slice(&(exponent as u16).to_be_bytes());
	bytes[2..].copy_from_slice(&mantissa.to_be_bytes());
	bytes
}

fn aiff(aifc: bool, little: bool, bits: u16, channels: u16, rate: u32, samples: &[u8]) -> Vec<u8> {
	let frame_bytes = channels as usize * bits as usize / 8;
	let frames = samples.len() / frame_bytes;
	let mut body = Vec::new();
	body.extend_from_slice(if aifc { b"AIFC" } else { b"AIFF" });
	body.extend_from_slice(b"COMM");
	body.extend_from_slice(&(if aifc { 22u32 } else { 18u32 }).to_be_bytes());
	body.extend_from_slice(&channels.to_be_bytes());
	body.extend_from_slice(&(frames as u32).to_be_bytes());
	body.extend_from_slice(&bits.to_be_bytes());
	body.extend_from_slice(&extended(rate));
	if aifc {
		body.extend_from_slice(if little { b"sowt" } else { b"NONE" });
	}
	body.extend_from_slice(b"SSND");
	body.extend_from_slice(&(samples.len() as u32 + 8).to_be_bytes());
	body.extend_from_slice(&0u32.to_be_bytes());
	body.extend_from_slice(&0u32.to_be_bytes());
	body.extend_from_slice(samples);
	if samples.len() & 1 != 0 {
		body.push(0);
	}
	let mut output = b"FORM".to_vec();
	output.extend_from_slice(&(body.len() as u32).to_be_bytes());
	output.extend_from_slice(&body);
	output
}

#[test]
fn decodes_big_endian_aiff_and_little_endian_aifc_in_chunks() {
	for (bytes, expected) in [
		(aiff(false, false, 16, 1, 8_000, &[0x12, 0x34, 0xff, 0xfe]), vec![0x34, 0x12, 0xfe, 0xff]),
		(aiff(true, true, 16, 1, 8_000, &[0x34, 0x12, 0xfe, 0xff]), vec![0x34, 0x12, 0xfe, 0xff]),
	] {
		let parsed = Aiff::parse(&bytes).unwrap();
		assert_eq!(parsed.metadata().frames, 2);
		let mut decoder = parsed.decoder();
		let mut output = Vec::new();
		assert_eq!(decoder.read_i16_le(1, &mut output), Ok(1));
		assert_eq!(&output, &expected[..2]);
		assert_eq!(decoder.read_i16_le(8, &mut output), Ok(1));
		assert_eq!(&output, &expected[2..]);
	}
}

#[test]
fn converts_signed_8_24_and_32_bit_samples() {
	for (bits, source, expected) in [
		(8, vec![0x80, 0, 0x7f], vec![0, 0x80, 0, 0, 0, 0x7f]),
		(24, vec![0x80, 0, 0, 0, 0, 0, 0x7f, 0xff, 0xff], vec![0, 0x80, 0, 0, 0xff, 0x7f]),
		(32, vec![0x80, 0, 0, 0, 0, 0, 0, 0, 0x7f, 0xff, 0xff, 0xff], vec![0, 0x80, 0, 0, 0xff, 0x7f]),
	] {
		let bytes = aiff(false, false, bits, 1, 8_000, &source);
		let parsed = Aiff::parse(&bytes).unwrap();
		let mut output = Vec::new();
		assert_eq!(parsed.decoder().read_i16_le(16, &mut output), Ok(3));
		assert_eq!(output, expected);
	}
}

#[test]
fn rejects_fractional_or_unsupported_rates_codecs_and_lengths() {
	let mut fractional = extended(8_000);
	fractional[9] = 1;
	assert_eq!(extended_rate(&fractional), Err(Error::Unsupported));
	assert!(matches!(Aiff::parse(b"FORM"), Err(Error::Truncated)));
	let mut unsupported = aiff(true, false, 16, 1, 8_000, &[0, 0]);
	unsupported[38..42].copy_from_slice(b"fl32");
	assert!(matches!(Aiff::parse(&unsupported), Err(Error::Unsupported)));
	let mut truncated = aiff(false, false, 16, 1, 8_000, &[0, 0]);
	truncated.pop();
	assert!(matches!(Aiff::parse(&truncated), Err(Error::Truncated)));
}

#[test]
fn decodes_staged_ffmpeg_aiff_and_aifc() {
	for bytes in [include_bytes!("../../../../volume/test.aiff").as_slice(), include_bytes!("../../../../volume/test.aifc").as_slice()] {
		let parsed = Aiff::parse(bytes).unwrap();
		assert_eq!(parsed.metadata().rate, 44_100);
		assert_eq!(parsed.metadata().channels, 1);
		assert_eq!(parsed.metadata().frames, 328_104);
		let mut output = Vec::new();
		assert_eq!(parsed.decoder().read_i16_le(1_024, &mut output), Ok(1_024));
		assert_eq!(output.len(), 2_048);
		assert!(output.iter().any(|byte| *byte != 0));
	}
}
