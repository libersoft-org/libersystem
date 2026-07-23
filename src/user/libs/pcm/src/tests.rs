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
