use super::encode::{EncodeError, Encoder};
use super::*;
use alloc::vec::Vec;
use pcm::Format;
use pcm::encode::{ForwardOnly, VecSink};

// Music-shaped: a resonant wander with a little noise on it. Noise would leave the decorrelation
// passes nothing to find, and a ramp would flatter them.
fn signal(frames: usize, channels: usize) -> Vec<i16> {
	let mut samples = Vec::new();
	let mut phase = [0i64, 4_000];
	let mut velocity = [800i64, -600];
	let mut state = 0x6d3f_20a1u32;
	for frame in 0..frames {
		for channel in 0..channels {
			state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
			velocity[channel] -= phase[channel] / 72;
			velocity[channel] += ((state >> 19) as i64 & 0x7f) - 64;
			phase[channel] = (phase[channel] + velocity[channel]).clamp(-29_000, 29_000);
			samples.push(if frame % 2_000 == 0 { (phase[channel] / 3) as i16 } else { phase[channel] as i16 });
		}
	}
	samples
}

fn decode_all(bytes: &[u8]) -> (Metadata, Vec<i16>) {
	let file = WavPack::parse(bytes).expect("the encoder wrote a file its own parser rejects");
	let mut decoder = file.decoder();
	let mut samples = Vec::new();
	let mut chunk = Vec::new();
	loop {
		let frames = decoder.read_i16_le(512, &mut chunk).expect("decode");
		if frames == 0 {
			break;
		}
		for pair in chunk.chunks_exact(2) {
			samples.push(i16::from_le_bytes([pair[0], pair[1]]));
		}
	}
	(file.metadata(), samples)
}

fn encode(input: &[i16], format: Format, joint: bool, chunk: usize) -> Vec<u8> {
	let mut encoder = Encoder::new(VecSink::new(8 << 20), format, joint).unwrap();
	for piece in input.chunks(chunk * format.channels() as usize) {
		encoder.push(piece).unwrap();
	}
	let (sink, frames) = encoder.finish().unwrap();
	assert_eq!(frames as usize, input.len() / format.channels() as usize);
	sink.into_bytes()
}

#[test]
fn mono_and_both_stereo_modes_round_trip_bit_for_bit() {
	// Lossless, so equality is the assertion. Ten thousand frames crosses the block boundary at
	// 4096, which is where the per-block entropy state and the block index have to line up.
	for (channels, joint) in [(1u8, false), (2, false), (2, true)] {
		let format = Format::new(44_100, channels).unwrap();
		let input = signal(10_000, channels as usize);
		let bytes = encode(&input, format, joint, 777);

		let (metadata, decoded) = decode_all(&bytes);
		assert_eq!(metadata.rate, 44_100);
		assert_eq!(metadata.channels, channels);
		assert_eq!(metadata.bits_per_sample, 16);
		assert_eq!(metadata.frames, 10_000);
		assert_eq!(decoded, input, "{channels} channels, joint={joint} was not lossless");
	}
}

#[test]
fn the_output_does_not_depend_on_how_the_input_was_handed_over() {
	let format = Format::new(48_000, 2).unwrap();
	let input = signal(9_000, 2);
	let whole = encode(&input, format, true, 9_000);
	for chunk in [1usize, 13, 4_096, 4_097] {
		assert_eq!(encode(&input, format, true, chunk), whole, "chunk size {chunk} changed the file");
	}
}

#[test]
fn every_rate_format_can_name_survives_including_the_ones_the_flags_cannot_hold() {
	// Nine of these are in the format's table of standard rates and get an index in the flags; the
	// rest have to travel in a metadata item, which is the branch that would otherwise never run.
	for rate in [8_000u32, 11_025, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 37_800, 44_101, 47_999] {
		let format = Format::new(rate, 1).unwrap();
		let input = signal(600, 1);
		let bytes = encode(&input, format, false, 600);
		let (metadata, decoded) = decode_all(&bytes);
		assert_eq!(metadata.rate, rate, "the rate did not come back");
		assert_eq!(decoded, input, "{rate} Hz was not lossless");
	}
}

#[test]
fn it_compresses_and_joint_stereo_helps_where_the_channels_are_alike() {
	let format = Format::new(44_100, 2).unwrap();
	let input = signal(20_000, 2);
	let raw = input.len() * 2;
	let plain = encode(&input, format, false, 4_096).len();
	let joint = encode(&input, format, true, 4_096).len();
	assert!(plain * 4 < raw * 3, "plain stereo produced {plain} bytes from {raw} of PCM");

	// Two channels that differ only slightly: the difference is nearly free, so coding it must beat
	// coding the second channel outright. If this ever fails, joint stereo is wired up backwards.
	let mut alike = Vec::new();
	for pair in input.chunks_exact(2) {
		alike.push(pair[0]);
		alike.push(pair[0].saturating_add(pair[1] / 64));
	}
	let alike_plain = encode(&alike, format, false, 4_096).len();
	let alike_joint = encode(&alike, format, true, 4_096).len();
	assert!(alike_joint < alike_plain, "joint stereo ({alike_joint}) did not beat plain ({alike_plain}) on near-identical channels");
	let _ = joint;
}

#[test]
fn full_scale_and_alternating_samples_survive_the_predictors() {
	// The residual of a second-difference predictor at full-scale alternation is four times the
	// sample range, and the entropy coder's buckets are unbounded - so this is where an encoder
	// that assumed a magnitude writes something its own decoder refuses.
	let format = Format::new(48_000, 2).unwrap();
	let mut input = Vec::new();
	for frame in 0..5_000 {
		input.push(if frame % 2 == 0 { i16::MAX } else { i16::MIN });
		input.push(if frame % 3 == 0 { i16::MIN } else { i16::MAX });
	}
	for joint in [false, true] {
		let bytes = encode(&input, format, joint, 1_111);
		let (metadata, decoded) = decode_all(&bytes);
		assert_eq!(metadata.frames, 5_000);
		assert_eq!(decoded, input, "joint={joint} lost full-scale alternation");
	}
}

#[test]
fn silence_stays_lossless_where_the_entropy_state_falls_to_nothing() {
	// Digital silence drives both medians to zero, which is the one condition under which the
	// decoder goes looking for a zero-run. The encoder has to answer that field even though it
	// never uses runs; if it does not, the stream desynchronises at the first quiet passage.
	let format = Format::new(8_000, 1).unwrap();
	let mut input = alloc::vec![0i16; 3_000];
	input.extend_from_slice(&signal(3_000, 1));
	input.extend_from_slice(&alloc::vec![0i16; 3_000]);
	let bytes = encode(&input, format, false, 500);
	let (metadata, decoded) = decode_all(&bytes);
	assert_eq!(metadata.frames, 9_000);
	assert_eq!(decoded, input, "silence broke the stream");
}

#[test]
fn a_forward_only_destination_is_enough_because_nothing_is_patched() {
	// The one encoder in this tree that never goes back: WavPack's block header carries no size
	// that is only known at the end, so this can write to something that cannot seek. Worth an
	// assertion, because it is a property a caller can rely on and a refactor could quietly remove.
	let format = Format::new(44_100, 1).unwrap();
	let mut encoder = Encoder::new(ForwardOnly::new(VecSink::new(1 << 20)), format, false).unwrap();
	let input = signal(5_000, 1);
	encoder.push(&input).unwrap();
	let (sink, frames) = encoder.finish().unwrap();
	assert_eq!(frames, 5_000);
	assert_eq!(decode_all(sink.into_inner().bytes()).1, input);
}

#[test]
fn an_empty_track_and_a_full_destination_are_reported() {
	let format = Format::new(44_100, 2).unwrap();
	let empty = Encoder::new(VecSink::new(1 << 20), format, true).unwrap();
	assert_eq!(empty.finish().err(), Some(EncodeError::Invalid));

	let mut full = Encoder::new(VecSink::new(64), format, true).unwrap();
	let outcome = full.push(&signal(9_000, 2)).and_then(|()| full.finish().map(|_| ()));
	assert!(matches!(outcome, Err(EncodeError::Destination(_))), "a full destination was not reported: {outcome:?}");

	let mut partial = Encoder::new(VecSink::new(1 << 20), format, true).unwrap();
	assert_eq!(partial.push(&[1, 2, 3]), Err(EncodeError::Invalid));
}
