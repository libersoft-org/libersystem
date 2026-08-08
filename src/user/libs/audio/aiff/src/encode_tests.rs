use super::encode::{EncodeError, Encoder, Output};
use super::*;
use alloc::vec::Vec;
use pcm::Format;
use pcm::encode::{ForwardOnly, SinkError, VecSink};

fn signal(frames: usize, channels: usize) -> Vec<i16> {
	let mut samples = Vec::new();
	let mut state = 0x0bad_c0deu32;
	let mut value = [0i32; 2];
	for _ in 0..frames {
		for channel in 0..channels {
			state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
			let step = ((state >> 16) as i32 & 0x3ff) - 512;
			value[channel] = (value[channel] + step).clamp(-30_000, 30_000);
			samples.push(value[channel] as i16);
		}
	}
	samples
}

fn decode_all(bytes: &[u8]) -> (Metadata, Vec<i16>) {
	let aiff = Aiff::parse(bytes).expect("the encoder wrote a file its own parser rejects");
	let mut decoder = aiff.decoder();
	let mut samples = Vec::new();
	let mut chunk = Vec::new();
	loop {
		let frames = decoder.read_i16_le(48, &mut chunk).expect("decode");
		if frames == 0 {
			break;
		}
		for pair in chunk.chunks_exact(2) {
			samples.push(i16::from_le_bytes([pair[0], pair[1]]));
		}
	}
	(aiff.metadata(), samples)
}

#[test]
fn every_container_and_width_round_trips_to_what_the_decoder_will_make_of_them() {
	let widths = [
		(Output::Aiff { bits: 8 }, 8u16),
		(Output::Aiff { bits: 16 }, 16),
		(Output::Aiff { bits: 24 }, 24),
		(Output::Aiff { bits: 32 }, 32),
		(Output::Aifc { bits: 16, little_endian: false }, 16),
		(Output::Aifc { bits: 16, little_endian: true }, 16),
		(Output::Aifc { bits: 24, little_endian: true }, 24),
		(Output::Aifc { bits: 32, little_endian: true }, 32),
	];
	for channels in [1u8, 2] {
		for (output, bits) in widths {
			let format = Format::new(44_100, channels).unwrap();
			let input = signal(500, channels as usize);
			let mut encoder = Encoder::new(VecSink::new(1 << 20), format, output).unwrap();
			for piece in input.chunks(channels as usize * 41) {
				encoder.push(piece).unwrap();
			}
			let (sink, frames) = encoder.finish().unwrap();
			assert_eq!(frames, 500);

			let (metadata, decoded) = decode_all(sink.bytes());
			assert_eq!(metadata.rate, 44_100);
			assert_eq!(metadata.channels, channels);
			assert_eq!(metadata.bits_per_sample, bits);
			assert_eq!(metadata.frames, 500);
			// Eight-bit AIFF is signed, unlike its RIFF counterpart, so the expected reconstruction
			// differs from the WAV one by exactly that.
			let expected: Vec<i16> = match bits {
				8 => input.iter().map(|&s| ((((s as i32 + 128) >> 8).clamp(-128, 127)) << 8) as i16).collect(),
				_ => input.clone(),
			};
			assert_eq!(decoded, expected, "{output:?} at {channels} channels did not survive");
		}
	}
}

#[test]
fn the_rate_is_written_as_an_extended_float_the_parser_reads_back_exactly() {
	// Ten bytes of eighty-bit float, constructed from an integer without a floating-point unit.
	// Every rate `Format` will name has to survive it, or a converted file plays at the wrong speed.
	for rate in [8_000u32, 11_025, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000] {
		let format = Format::new(rate, 1).unwrap();
		let mut encoder = Encoder::new(VecSink::new(1 << 16), format, Output::Aiff { bits: 16 }).unwrap();
		encoder.push(&signal(16, 1)).unwrap();
		let (sink, _) = encoder.finish().unwrap();
		let (metadata, _) = decode_all(sink.bytes());
		assert_eq!(metadata.rate, rate, "the extended-float rate did not come back");
	}
}

#[test]
fn an_odd_length_body_is_padded_and_the_sizes_still_add_up() {
	// One channel of eight-bit audio at an odd frame count: the only combination that leaves the
	// sound chunk on an odd boundary, where a missing pad byte makes the FORM size disagree with
	// the file.
	let format = Format::new(8_000, 1).unwrap();
	let mut encoder = Encoder::new(VecSink::new(1 << 16), format, Output::Aiff { bits: 8 }).unwrap();
	encoder.push(&signal(101, 1)).unwrap();
	let (sink, frames) = encoder.finish().unwrap();
	assert_eq!(frames, 101);
	let bytes = sink.into_bytes();
	assert_eq!(bytes.len() % 2, 0);
	// The FORM size must name the whole file, padding included.
	assert_eq!(u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize + 8, bytes.len());
	let (metadata, decoded) = decode_all(&bytes);
	assert_eq!(metadata.frames, 101);
	assert_eq!(decoded.len(), 101);
}

#[test]
fn a_destination_that_cannot_seek_or_cannot_hold_it_is_reported() {
	let format = Format::new(48_000, 2).unwrap();
	let refused = Encoder::new(ForwardOnly::new(VecSink::new(1 << 16)), format, Output::Aiff { bits: 16 });
	assert!(matches!(refused, Err(EncodeError::Destination(SinkError::Unseekable))));

	let mut full = Encoder::new(VecSink::new(64), format, Output::Aiff { bits: 16 }).unwrap();
	assert_eq!(full.push(&signal(64, 2)), Err(EncodeError::Destination(SinkError::Full)));

	let empty = Encoder::new(VecSink::new(1 << 16), format, Output::Aiff { bits: 16 }).unwrap();
	assert_eq!(empty.finish().err(), Some(EncodeError::Invalid));

	let unsupported = Encoder::new(VecSink::new(1 << 16), format, Output::Aiff { bits: 20 });
	assert_eq!(unsupported.err(), Some(EncodeError::Unsupported));

	let mut partial = Encoder::new(VecSink::new(1 << 16), format, Output::Aiff { bits: 16 }).unwrap();
	assert_eq!(partial.push(&[1, 2, 3]), Err(EncodeError::Invalid));
}
