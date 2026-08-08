use super::encode::{EncodeError, Encoder, Output};
use super::*;
use alloc::vec::Vec;
use pcm::Format;
use pcm::encode::{ForwardOnly, SinkError, VecSink};

// A deterministic signal that moves the way music does - smooth, with the occasional step - so the
// differential codecs are exercised on something other than a ramp, and the same every run.
fn signal(frames: usize, channels: usize) -> Vec<i16> {
	let mut samples = Vec::new();
	let mut state = 0x1234_5678u32;
	let mut value = [0i32; 2];
	for frame in 0..frames {
		for channel in 0..channels {
			state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
			let step = ((state >> 16) as i32 & 0x1ff) - 256;
			value[channel] = (value[channel] + step).clamp(-30_000, 30_000);
			let shaped = if frame % 97 == 0 { value[channel] / 2 } else { value[channel] };
			samples.push(shaped as i16);
		}
	}
	samples
}

fn decode_all(bytes: &[u8]) -> (Metadata, Vec<i16>) {
	let wav = Wav::parse(bytes).expect("the encoder wrote a file its own parser rejects");
	let mut decoder = wav.decoder();
	let mut samples = Vec::new();
	let mut chunk = Vec::new();
	loop {
		let frames = decoder.read_i16_le(64, &mut chunk).expect("decode");
		if frames == 0 {
			break;
		}
		for pair in chunk.chunks_exact(2) {
			samples.push(i16::from_le_bytes([pair[0], pair[1]]));
		}
	}
	(wav.metadata(), samples)
}

#[test]
fn sixteen_bit_pcm_round_trips_sample_exactly() {
	for channels in [1u8, 2] {
		let format = Format::new(44_100, channels).unwrap();
		let input = signal(1_000, channels as usize);
		let mut encoder = Encoder::new(VecSink::new(1 << 20), format, Output::Pcm { bits: 16 }).unwrap();
		// Pushed in uneven pieces, because a caller streams whatever a reader gave it.
		for piece in input.chunks(channels as usize * 37) {
			encoder.push(piece).unwrap();
		}
		let (sink, frames) = encoder.finish().unwrap();
		assert_eq!(frames, 1_000);

		let (metadata, decoded) = decode_all(sink.bytes());
		assert_eq!(metadata.rate, 44_100);
		assert_eq!(metadata.channels, channels);
		assert_eq!(metadata.bits_per_sample, 16);
		assert_eq!(metadata.frames, 1_000);
		assert_eq!(decoded, input);
	}
}

#[test]
fn the_other_pcm_widths_round_trip_to_what_the_decoder_will_make_of_them() {
	// Eight-bit throws away a byte per sample, so the assertion is against what the decoder is
	// going to produce rather than against the input: exact, and about the conversion rather than
	// about the loss.
	let format = Format::new(22_050, 1).unwrap();
	let input = signal(300, 1);
	for bits in [8u16, 24, 32] {
		let mut encoder = Encoder::new(VecSink::new(1 << 20), format, Output::Pcm { bits }).unwrap();
		encoder.push(&input).unwrap();
		let (sink, _) = encoder.finish().unwrap();
		let (metadata, decoded) = decode_all(sink.bytes());
		assert_eq!(metadata.bits_per_sample, bits);
		assert_eq!(decoded.len(), input.len());
		let expected: Vec<i16> = match bits {
			8 => input.iter().map(|&s| ((((s as i32 + 128) >> 8).clamp(-128, 127)) << 8) as i16).collect(),
			_ => input.clone(),
		};
		assert_eq!(decoded, expected, "{bits}-bit did not survive the round trip");
	}
}

#[test]
fn adpcm_round_trips_to_exactly_what_the_encoder_predicted() {
	// The strong form of a lossy-codec test. The encoder runs the decoder's own state update, so it
	// knows precisely which samples will come back; anything else means the two halves disagree
	// about the format, which is the failure that shows up as noise in somebody else's player.
	for channels in [1u8, 2] {
		for output in [Output::ima_default(channels), Output::ms_default(channels)] {
			let format = Format::new(16_000, channels).unwrap();
			let samples_per_block = match output {
				Output::ImaAdpcm { block_align } => adpcm::ima_samples_per_block(block_align, channels).unwrap(),
				Output::MsAdpcm { block_align } => adpcm::ms_samples_per_block(block_align, channels).unwrap(),
				Output::Pcm { .. } => unreachable!(),
			};
			let frames = samples_per_block * 3;
			let input = signal(frames, channels as usize);

			let mut encoder = Encoder::new(VecSink::new(1 << 20), format, output).unwrap();
			let mut predicted = Vec::new();
			// One whole block per push, so the reconstruction of each can be collected as it is
			// written rather than kept for the length of the track.
			for block in input.chunks(samples_per_block * channels as usize) {
				encoder.push(block).unwrap();
				predicted.extend_from_slice(encoder.last_block_reconstruction());
			}
			let (sink, written) = encoder.finish().unwrap();
			assert_eq!(written, frames as u64);

			let (metadata, decoded) = decode_all(sink.bytes());
			assert_eq!(metadata.frames, frames as u64);
			assert_eq!(metadata.bits_per_sample, 4);
			assert_eq!(decoded, predicted, "{output:?} at {channels} channels drifted from the encoder");

			// And it must still be audio: a codec that agrees with itself about silence would pass
			// the assertion above.
			let error: i64 = decoded.iter().zip(&input).map(|(&a, &b)| (a as i64 - b as i64).abs()).sum();
			let mean = error / decoded.len() as i64;
			assert!(mean < 900, "{output:?} at {channels} channels averaged {mean} off the input");
		}
	}
}

#[test]
fn a_short_final_block_is_padded_and_the_frame_count_trims_it() {
	let channels = 1u8;
	let format = Format::new(8_000, channels).unwrap();
	let output = Output::ima_default(channels);
	let Output::ImaAdpcm { block_align } = output else { unreachable!() };
	let samples_per_block = adpcm::ima_samples_per_block(block_align, channels).unwrap();
	// One block and a fifth: the last block is padded out and `fact` says where the audio stopped.
	let frames = samples_per_block + samples_per_block / 5;
	let input = signal(frames, 1);

	let mut encoder = Encoder::new(VecSink::new(1 << 20), format, output).unwrap();
	encoder.push(&input).unwrap();
	let (sink, written) = encoder.finish().unwrap();
	assert_eq!(written, frames as u64);
	let bytes = sink.into_bytes();
	assert_eq!(bytes.len() % 2, 0, "the file is not even-aligned");

	let (metadata, decoded) = decode_all(&bytes);
	assert_eq!(metadata.frames, frames as u64);
	assert_eq!(decoded.len(), frames, "the padding of the last block was not trimmed");
}

#[test]
fn a_destination_that_cannot_seek_is_refused_before_any_audio_is_written() {
	let format = Format::new(48_000, 2).unwrap();
	let result = Encoder::new(ForwardOnly::new(VecSink::new(1 << 20)), format, Output::Pcm { bits: 16 });
	assert!(matches!(result, Err(EncodeError::Destination(SinkError::Unseekable))));
}

#[test]
fn a_full_destination_and_an_empty_track_are_reported_rather_than_written() {
	let format = Format::new(48_000, 1).unwrap();

	// Room for the header and nothing else.
	let mut encoder = Encoder::new(VecSink::new(48), format, Output::Pcm { bits: 16 }).unwrap();
	assert_eq!(encoder.push(&signal(64, 1)), Err(EncodeError::Destination(SinkError::Full)));

	// A WAVE file with no audio is one this leaf's own parser rejects, so it is not produced.
	let empty = Encoder::new(VecSink::new(1 << 20), format, Output::Pcm { bits: 16 }).unwrap();
	assert_eq!(empty.finish().err(), Some(EncodeError::Invalid));
}

#[test]
fn profiles_the_encoder_cannot_write_are_refused_by_name() {
	let format = Format::new(48_000, 2).unwrap();
	let sink = || VecSink::new(1 << 20);
	assert_eq!(Encoder::new(sink(), format, Output::Pcm { bits: 12 }).err(), Some(EncodeError::Unsupported));
	// A block too small to hold its own header, and a stereo block that does not divide into the
	// eight-byte groups the format packs in.
	assert_eq!(Encoder::new(sink(), format, Output::ImaAdpcm { block_align: 4 }).err(), Some(EncodeError::Invalid));
	assert_eq!(Encoder::new(sink(), format, Output::ImaAdpcm { block_align: 20 }).err(), Some(EncodeError::Invalid));
	assert_eq!(Encoder::new(sink(), format, Output::MsAdpcm { block_align: 8 }).err(), Some(EncodeError::Invalid));
	// Frames must be whole.
	let mut encoder = Encoder::new(sink(), format, Output::Pcm { bits: 16 }).unwrap();
	assert_eq!(encoder.push(&[1, 2, 3]), Err(EncodeError::Invalid));
}
