use super::encode::{Effort, EncodeError, Encoder};
use super::*;
use alloc::vec::Vec;
use pcm::Format;
use pcm::encode::{ForwardOnly, SinkError, VecSink};

// Music-shaped rather than random: a random signal has no correlation for a predictor to find, so
// an encoder tested only on noise is one whose compression was never exercised at all.
fn signal(frames: usize, channels: usize) -> Vec<i16> {
	let mut samples = Vec::new();
	let mut phase = [0i64, 3_000];
	let mut velocity = [700i64, -500];
	let mut state = 0x51ed_270bu32;
	for frame in 0..frames {
		for channel in 0..channels {
			state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
			// A resonant wander with a little noise on it, and a step every so often so the block
			// boundaries are not all alike.
			velocity[channel] -= phase[channel] / 64;
			velocity[channel] += ((state >> 20) as i64 & 0x3f) - 32;
			phase[channel] = (phase[channel] + velocity[channel]).clamp(-28_000, 28_000);
			let value = if frame % 1_500 == 0 { phase[channel] / 4 } else { phase[channel] };
			samples.push(value as i16);
		}
	}
	samples
}

fn decode_all(bytes: &[u8]) -> (Metadata, Vec<i16>) {
	let flac = Flac::parse(bytes).expect("the encoder wrote a file its own parser rejects");
	let mut decoder = flac.decoder();
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
	(flac.metadata(), samples)
}

fn encode(input: &[i16], format: Format, effort: Effort, chunk: usize) -> Vec<u8> {
	let mut encoder = Encoder::new(VecSink::new(4 << 20), format, effort).unwrap();
	for piece in input.chunks(chunk * format.channels() as usize) {
		encoder.push(piece).unwrap();
	}
	let (sink, frames) = encoder.finish().unwrap();
	assert_eq!(frames as usize, input.len() / format.channels() as usize);
	sink.into_bytes()
}

#[test]
fn every_effort_and_channel_count_round_trips_bit_for_bit() {
	// Lossless means the assertion can be equality, and it is the only assertion worth making: a
	// FLAC encoder that is merely close is not a FLAC encoder.
	for channels in [1u8, 2] {
		for effort in [Effort::Fast, Effort::Balanced, Effort::Thorough] {
			let format = Format::new(44_100, channels).unwrap();
			// Over one block, so the frame numbering, the short final block and the block-size
			// escape in the frame header are all exercised rather than assumed.
			let input = signal(9_000, channels as usize);
			let bytes = encode(&input, format, effort, 700);

			let (metadata, decoded) = decode_all(&bytes);
			assert_eq!(metadata.rate, 44_100);
			assert_eq!(metadata.channels, channels);
			assert_eq!(metadata.bits_per_sample, 16);
			assert_eq!(metadata.frames, 9_000);
			assert_eq!(decoded, input, "{effort:?} at {channels} channels was not lossless");
		}
	}
}

#[test]
fn the_output_does_not_depend_on_how_the_input_was_handed_over() {
	// A file whose bytes depend on the caller's read sizes is one whose tests cannot pin it, and a
	// conversion that is not reproducible cannot be checked against a checksum.
	let format = Format::new(48_000, 2).unwrap();
	let input = signal(5_000, 2);
	let whole = encode(&input, format, Effort::Balanced, 5_000);
	for chunk in [1usize, 17, 4_096, 4_112] {
		assert_eq!(encode(&input, format, Effort::Balanced, chunk), whole, "chunk size {chunk} changed the file");
	}
}

#[test]
fn compression_actually_compresses_and_more_effort_never_costs_more() {
	let format = Format::new(44_100, 2).unwrap();
	let input = signal(20_000, 2);
	let raw = input.len() * 2;
	let fast = encode(&input, format, Effort::Fast, 4_096).len();
	let balanced = encode(&input, format, Effort::Balanced, 4_096).len();
	let thorough = encode(&input, format, Effort::Thorough, 4_096).len();

	// The point of the format. A predictable signal must come out well under what it took as PCM,
	// or the residual coding is not doing anything and the round-trip test above would still pass.
	// Measured at just over half; the bound is loose enough that a small regression in the search
	// does not fail the build, and tight enough that a broken one does.
	assert!(balanced * 5 < raw * 3, "balanced produced {balanced} bytes from {raw} of PCM");
	// Effort buys candidates, and a candidate is only chosen when it is measured to be cheaper, so
	// more effort can never produce a larger file.
	assert!(balanced <= fast, "balanced ({balanced}) was worse than fast ({fast})");
	assert!(thorough <= balanced, "thorough ({thorough}) was worse than balanced ({balanced})");
}

#[test]
fn silence_and_a_constant_tone_collapse_to_almost_nothing() {
	// The constant subframe: a block that never changes should cost its header and one sample. If
	// this regresses, a quiet passage costs as much as a loud one and nobody notices for years.
	let format = Format::new(8_000, 1).unwrap();
	let silence = alloc::vec![0i16; 40_000];
	let bytes = encode(&silence, format, Effort::Balanced, 4_096);
	assert!(bytes.len() < 1_000, "forty thousand frames of silence took {} bytes", bytes.len());
	let (metadata, decoded) = decode_all(&bytes);
	assert_eq!(metadata.frames, 40_000);
	assert_eq!(decoded, silence);
}

#[test]
fn full_scale_and_alternating_samples_survive_the_predictors() {
	// The residual of a fixed predictor grows with the order, and full-scale alternation is where
	// it grows fastest: order four turns +/-32767 into about half a million, which is where an
	// encoder that assumed sixteen-bit residuals writes a file it cannot read back.
	let format = Format::new(48_000, 2).unwrap();
	let mut input = Vec::new();
	for frame in 0..3_000 {
		// Opposite phase in the two channels, so the side channel is the one that goes furthest out
		// of range: at full scale its residual is where a seventeenth bit stops being enough.
		input.push(if frame % 2 == 0 { i16::MAX } else { i16::MIN });
		input.push(if frame % 2 == 0 { i16::MIN } else { i16::MAX });
	}
	let bytes = encode(&input, format, Effort::Thorough, 999);
	let (metadata, decoded) = decode_all(&bytes);
	assert_eq!(metadata.frames, 3_000);
	assert_eq!(decoded, input);
}

#[test]
fn a_track_too_short_for_a_block_and_a_destination_that_cannot_seek_are_refused() {
	let format = Format::new(44_100, 1).unwrap();
	let refused = Encoder::new(ForwardOnly::new(VecSink::new(1 << 16)), format, Effort::Balanced);
	assert!(matches!(refused, Err(EncodeError::Destination(SinkError::Unseekable))));

	// Fifteen frames cannot be a FLAC stream: the smallest block the format's own metadata can
	// describe is sixteen.
	let mut short = Encoder::new(VecSink::new(1 << 16), format, Effort::Balanced).unwrap();
	short.push(&signal(15, 1)).unwrap();
	assert_eq!(short.finish().err(), Some(EncodeError::Invalid));

	let empty = Encoder::new(VecSink::new(1 << 16), format, Effort::Balanced).unwrap();
	assert_eq!(empty.finish().err(), Some(EncodeError::Invalid));

	// Sixteen is the smallest that can.
	let mut smallest = Encoder::new(VecSink::new(1 << 16), format, Effort::Balanced).unwrap();
	smallest.push(&signal(16, 1)).unwrap();
	let (sink, frames) = smallest.finish().unwrap();
	assert_eq!(frames, 16);
	assert_eq!(decode_all(sink.bytes()).1.len(), 16);
}

#[test]
fn a_full_destination_is_reported_rather_than_written_short() {
	let format = Format::new(44_100, 2).unwrap();
	let mut encoder = Encoder::new(VecSink::new(64), format, Effort::Balanced).unwrap();
	let input = signal(9_000, 2);
	// The first block cannot fit, and the encoder says so instead of writing a truncated frame.
	let outcome = encoder.push(&input).and_then(|()| encoder.finish().map(|_| ()));
	assert_eq!(outcome.err(), Some(EncodeError::Destination(SinkError::Full)));

	let mut partial = Encoder::new(VecSink::new(1 << 20), format, Effort::Balanced).unwrap();
	assert_eq!(partial.push(&[1, 2, 3]), Err(EncodeError::Invalid));
}

#[test]
fn the_frame_number_encoding_is_minimal_and_reads_back_at_every_length() {
	// A track long enough to reach a four-byte frame number is a quarter of an hour, so the round
	// trip above never gets near these. Tested directly instead, because "the encoder is correct
	// for the lengths the tests happened to reach" is not the claim being made.
	let boundaries = [0u64, 1, 0x7f, 0x80, 0x7ff, 0x800, 0xffff, 0x1_0000, 0x1f_ffff, 0x20_0000, 0x3ff_ffff, 0x400_0000, 0x7fff_ffff, 0x8000_0000, 0xf_ffff_ffff];
	for value in boundaries {
		let mut bytes = Vec::new();
		super::encode::write_utf8_number(&mut bytes, value);
		let (read, length) = super::read_utf8_number(&bytes).unwrap_or_else(|error| panic!("{value:#x} encoded to {bytes:?} which does not read back: {error:?}"));
		assert_eq!(read, value, "{value:#x} came back as {read:#x}");
		assert_eq!(length, bytes.len(), "{value:#x} wrote {} bytes and consumed {length}", bytes.len());
		// Minimal: one byte shorter must not be able to hold it. The decoder enforces exactly this,
		// so an encoder that pads produces a file it will not read.
		if value >= 0x80 {
			let mut shorter = Vec::new();
			super::encode::write_utf8_number(&mut shorter, value >> 6);
			assert!(shorter.len() < bytes.len() || value >> 6 < 0x80, "{value:#x} is not using the shortest length");
		}
	}
}

#[test]
fn the_bit_writer_and_the_bit_reader_agree() {
	// The two halves of every FLAC frame. If they disagree by one bit the container still parses -
	// the CRC is over bytes - and the audio comes out as noise, which is the kind of defect a
	// round-trip test finds only by luck.
	let mut writer = super::encode::BitWriter::new();
	let pieces: [(u64, u32); 9] = [(1, 1), (0b101101, 6), (0, 1), (0xdead, 16), (7, 3), (0x1234_5678, 32), (0, 5), (0x3f, 6), (1, 2)];
	for (value, count) in pieces {
		writer.write(value, count);
	}
	writer.unary(37);
	writer.write(0x2a, 7);
	writer.align_zero();

	let mut reader = super::Bits::new(&writer.bytes, 0);
	for (value, count) in pieces {
		assert_eq!(reader.read(count as u8).unwrap(), value, "a {count}-bit field did not read back");
	}
	assert_eq!(reader.unary().unwrap(), 37, "the unary run did not read back");
	assert_eq!(reader.read(7).unwrap(), 0x2a);
	reader.align_zero().expect("the padding is zero");
}

#[test]
fn every_stereo_layout_is_lossless_on_the_material_that_selects_it() {
	// The encoder picks between four channel layouts by cost, so which one a test exercises depends
	// on its signal - and the round trip above uses one. These four are shaped to make a different
	// layout the cheapest: identical channels make the difference nearly free, opposite channels
	// make the SUM nearly free, and a quiet channel beside a loud one makes whichever of the two is
	// cheap the one to keep. If any layout is wired up wrongly, exactly one of these fails.
	let format = Format::new(44_100, 2).unwrap();
	let base = signal(6_000, 1);
	let shapes: [(&str, &dyn Fn(usize, i16) -> (i16, i16)); 4] = [
		("identical", &|_, value| (value, value)),
		("opposite", &|_, value| (value, value.wrapping_neg())),
		("quiet right", &|_, value| (value, (value as i32 / 64) as i16)),
		("quiet left", &|_, value| (((value as i32) / 64) as i16, value)),
	];
	for (name, shape) in shapes {
		let mut input = Vec::new();
		for (index, &value) in base.iter().enumerate() {
			let (left, right) = shape(index, value);
			input.push(left);
			input.push(right);
		}
		let bytes = encode(&input, format, Effort::Thorough, 1_500);
		let (metadata, decoded) = decode_all(&bytes);
		assert_eq!(metadata.frames, 6_000, "{name}: the frame count is wrong");
		assert_eq!(decoded, input, "{name} was not lossless");
	}
}
