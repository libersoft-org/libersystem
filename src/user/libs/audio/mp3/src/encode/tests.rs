// The round trip: encode a signal, decode it with this crate's own decoder, and compare.
//
// This is what the tables and the window are ultimately checked by. A mistranscribed codeword, a
// window fitted wrongly, an alias butterfly applied in the wrong direction - each of them produces
// a stream that either fails to parse or decodes into something that is not the signal, and none of
// them can pass this.

use super::*;
use crate::Mp3;
use alloc::vec::Vec;
use pcm::encode::VecSink;

// Two tones and a sweep, which is a signal the transform has to work for rather than a constant it
// would be right about by accident.
fn signal(frames: usize, rate: u32, channels: u8) -> Vec<i16> {
	let mut out = Vec::with_capacity(frames * channels as usize);
	for frame in 0..frames {
		let t = frame as f32 / rate as f32;
		let value = 0.35 * (core::f64::consts::TAU * 440.0 * t as f64).sin() as f32 + 0.25 * (core::f64::consts::TAU * 1_330.0 * t as f64).sin() as f32;
		out.push((value * 24_000.0) as i16);
		if channels == 2 {
			out.push((value * 18_000.0) as i16 - 900);
		}
	}
	out
}

fn encode(samples: &[i16], rate: u32, channels: u8, bitrate: u32) -> Vec<u8> {
	let format = Format::new(rate, channels).expect("the test format is one `Format` names");
	let mut encoder = Encoder::new(VecSink::new(1 << 24), format, bitrate).expect("the encoder starts");
	encoder.push(samples).expect("the samples encode");
	encoder.finish().expect("the stream closes").0.into_bytes()
}

// The correlation of what came back with what went in, at the best alignment. A lossy codec shifts
// its output by the filterbank's delay, so comparing sample for sample from zero would measure the
// delay rather than the audio.
fn best_match(original: &[i16], decoded: &[i16], channels: usize) -> f64 {
	let take = 4_096.min(original.len() / channels - 1);
	let mut best = 0.0f64;
	for shift in 0..2_000 {
		let start = shift * channels;
		if start + take * channels > decoded.len() {
			break;
		}
		let (mut dot, mut a2, mut b2) = (0.0f64, 0.0f64, 0.0f64);
		for i in 0..take {
			let a = decoded[start + i * channels] as f64;
			let b = original[i * channels] as f64;
			dot += a * b;
			a2 += a * a;
			b2 += b * b;
		}
		if a2 > 0.0 && b2 > 0.0 {
			let correlation = dot / (a2.sqrt() * b2.sqrt());
			if correlation > best {
				best = correlation;
			}
		}
	}
	best
}

fn decode(bytes: &[u8]) -> (Vec<i16>, u32, u8) {
	let mp3 = Mp3::parse(bytes).expect("the stream this encoder wrote is one this decoder reads");
	let metadata = mp3.metadata();
	let mut decoder = mp3.decoder();
	let mut out = Vec::new();
	let mut raw = Vec::new();
	loop {
		let frames = decoder.read_i16_le(1_024, &mut raw).expect("the stream decodes");
		if frames == 0 {
			break;
		}
		for pair in raw.chunks_exact(2) {
			out.push(i16::from_le_bytes([pair[0], pair[1]]));
		}
	}
	(out, metadata.rate, metadata.channels)
}

#[test]
fn a_mono_stream_decodes_back_as_the_signal_that_went_in() {
	let original = signal(9_000, 44_100, 1);
	let bytes = encode(&original, 44_100, 1, 128);
	let (decoded, rate, channels) = decode(&bytes);
	assert_eq!((rate, channels), (44_100, 1));
	assert!(decoded.len() > 7_000, "the stream decoded {} samples of a nine-thousand-sample track", decoded.len());
	let correlation = best_match(&original, &decoded, 1);
	assert!(correlation > 0.95, "the decoded audio correlates {correlation:.4} with what was encoded");
}

#[test]
fn a_stereo_stream_keeps_its_two_channels_apart() {
	let original = signal(9_000, 44_100, 2);
	let bytes = encode(&original, 44_100, 2, 192);
	let (decoded, rate, channels) = decode(&bytes);
	assert_eq!((rate, channels), (44_100, 2));
	// Both channels carry the same shape at different levels, so a stream that duplicated one over
	// the other would still correlate - what it would not do is keep the level difference.
	let left: Vec<i16> = decoded.iter().step_by(2).copied().collect();
	let right: Vec<i16> = decoded.iter().skip(1).step_by(2).copied().collect();
	let energy = |v: &[i16]| v.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>().sqrt();
	let ratio = energy(&right) / energy(&left).max(1.0);
	assert!((0.6..0.9).contains(&ratio), "the channels came back at a level ratio of {ratio:.3}");
	assert!(best_match(&original, &decoded, 2) > 0.9, "the stereo stream does not match what was encoded");
}

#[test]
fn every_rate_this_encoder_names_round_trips_and_every_other_is_refused() {
	for rate in [32_000u32, 44_100, 48_000] {
		let original = signal(5_000, rate, 1);
		let (decoded, decoded_rate, _) = decode(&encode(&original, rate, 1, 128));
		assert_eq!(decoded_rate, rate);
		assert!(best_match(&original, &decoded, 1) > 0.9, "{rate} Hz does not round trip");
	}
	// MPEG-2 halves these rates and changes the side info; refused by name rather than mislabelled.
	assert!(Config::new(22_050, 1).is_none());
	assert!(Config::new(16_000, 2).is_none());
	assert!(Config::new(44_100, 3).is_none());
	assert!(Config::with_bitrate(44_100, 2, 1).is_none(), "a bitrate the format does not name");
}

#[test]
fn silence_encodes_to_silence_rather_than_to_noise() {
	let original = alloc::vec![0i16; 8_000];
	let bytes = encode(&original, 44_100, 1, 128);
	let (decoded, _, _) = decode(&bytes);
	let peak = decoded.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
	assert!(peak < 64, "silence came back with a peak of {peak}");
}

#[test]
fn a_higher_bitrate_is_a_closer_reconstruction() {
	// The rate loop's whole purpose: more room per frame is a finer quantiser.
	//
	// THE SIGNAL HAS TO BE DENSE for this to measure anything. Two sinusoids are a handful of
	// non-zero lines out of 576, and 32 kbit/s codes them almost perfectly - the first version of
	// this test compared 0.9977 against 0.9977 and would have gone on passing with the rate loop
	// removed. A broadband signal spends its bits everywhere and cannot be coded well in a hundred
	// bytes a frame.
	let mut state = 0x1234_5678u32;
	let mut original = Vec::with_capacity(9_000);
	for _ in 0..9_000 {
		state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
		original.push(((state >> 16) as i16 as i32 * 20_000 / 32_768) as i16);
	}
	let low = best_match(&original, &decode(&encode(&original, 44_100, 1, 32)).0, 1);
	let high = best_match(&original, &decode(&encode(&original, 44_100, 1, 320)).0, 1);
	assert!(high > low + 0.05, "320 kbit/s ({high:.4}) is no closer than 32 ({low:.4})");
}
