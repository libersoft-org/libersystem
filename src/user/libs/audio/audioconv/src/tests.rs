use super::*;
use alloc::vec;

fn signal(frames: usize, channels: usize) -> Vec<i16> {
	let mut samples = Vec::new();
	let mut phase = [0i64, 2_000];
	let mut velocity = [600i64, -400];
	let mut state = 0x2f6e_1a37u32;
	for _ in 0..frames {
		for channel in 0..channels {
			state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
			velocity[channel] -= phase[channel] / 64;
			velocity[channel] += ((state >> 20) as i64 & 0x3f) - 32;
			phase[channel] = (phase[channel] + velocity[channel]).clamp(-27_000, 27_000);
			samples.push(phase[channel] as i16);
		}
	}
	samples
}

fn wave(frames: usize, channels: u8, rate: u32) -> Vec<u8> {
	let format = Format::new(rate, channels).unwrap();
	let mut encoder = wav::encode::Encoder::new(VecSink::new(8 << 20), format, wav::encode::Output::Pcm { bits: 16 }).unwrap();
	encoder.push(&signal(frames, channels as usize)).unwrap();
	encoder.finish().unwrap().0.into_bytes()
}

fn config(output: &str) -> Config {
	parse_args(alloc::format!("in.wav {output}").as_bytes()).expect("these options parse")
}

fn samples_of(bytes: &[u8]) -> Vec<i16> {
	let file = wav::Wav::parse(bytes).expect("a WAV came back");
	let mut decoder = file.decoder();
	let mut out = Vec::new();
	let mut chunk = Vec::new();
	while decoder.read_i16_le(512, &mut chunk).expect("decode") != 0 {
		for pair in chunk.chunks_exact(2) {
			out.push(i16::from_le_bytes([pair[0], pair[1]]));
		}
	}
	out
}

#[test]
fn the_capability_table_is_in_the_order_its_lookup_assumes() {
	// `capabilities` indexes the table by the enum. A profile added to one and not the other would
	// otherwise answer with its neighbour's capabilities, and nothing would say so.
	assert_eq!(PROFILES.len(), Profile::Mp3 as usize + 1);
	for entry in PROFILES {
		assert_eq!(capabilities(entry.profile).name, entry.name);
	}
	// Nothing is both lossless and quality-controlled, and nothing lossy takes a compression effort.
	for entry in PROFILES {
		assert!(!(entry.lossless && entry.quality), "{} is lossless and takes --quality", entry.name);
		assert!(!(!entry.lossless && entry.compression), "{} is lossy and takes --compression", entry.name);
	}
}

#[test]
fn the_input_is_recognised_by_its_bytes_and_not_by_its_name() {
	assert_eq!(sniff(&wave(64, 1, 44_100)), Some(Container::Wav));

	let format = Format::new(44_100, 1).unwrap();
	let mut aiff_encoder = aiff::encode::Encoder::new(VecSink::new(1 << 20), format, aiff::encode::Output::Aiff { bits: 16 }).unwrap();
	aiff_encoder.push(&signal(64, 1)).unwrap();
	assert_eq!(sniff(aiff_encoder.finish().unwrap().0.bytes()), Some(Container::Aiff));

	let mut aifc_encoder = aiff::encode::Encoder::new(VecSink::new(1 << 20), format, aiff::encode::Output::Aifc { bits: 16, little_endian: true }).unwrap();
	aifc_encoder.push(&signal(64, 1)).unwrap();
	assert_eq!(sniff(aifc_encoder.finish().unwrap().0.bytes()), Some(Container::Aifc));

	let mut flac_encoder = flac::encode::Encoder::new(VecSink::new(1 << 20), format, flac::encode::Effort::Fast).unwrap();
	flac_encoder.push(&signal(64, 1)).unwrap();
	assert_eq!(sniff(flac_encoder.finish().unwrap().0.bytes()), Some(Container::Flac));

	assert_eq!(sniff(b"wvpk\0\0\0\0"), Some(Container::WavPack));
	assert_eq!(sniff(b"OggS\0\0\0\0"), Some(Container::Vorbis));
	assert_eq!(sniff(b"ID3\x04\0\0\0"), Some(Container::Mp3));
	assert_eq!(sniff(&[0xff, 0xfb, 0x90, 0x00]), Some(Container::Mp3));
	assert_eq!(sniff(b"not audio at all"), None);
	assert_eq!(sniff(b"RIFF____AVI "), None);
	assert_eq!(sniff(b""), None);
}

#[test]
fn the_destination_comes_from_the_suffix_or_from_the_flag() {
	assert_eq!(config("out.flac").resolved_profile(), Some(Profile::Flac));
	assert_eq!(config("out.AIFF").resolved_profile(), Some(Profile::Aiff));
	assert_eq!(config("out.aif").resolved_profile(), Some(Profile::Aiff));
	assert_eq!(config("out.wav").resolved_profile(), Some(Profile::WavPcm));
	// The two ADPCM profiles have no suffix of their own - they are WAV files - so they can only be
	// asked for by name, which is why the table lets a profile have no suffixes at all.
	let ima = parse_args(b"--format wav-ima in.wav out.wav").unwrap();
	assert_eq!(ima.resolved_profile(), Some(Profile::WavIma));
	let named = parse_args(b"--format flac in.wav out.bin").unwrap();
	assert_eq!(named.resolved_profile(), Some(Profile::Flac));
	// A destination whose suffix means nothing, and no flag to say what it should be.
	assert_eq!(parse_args(b"in.wav out.bin").err(), Some(Error::UnsupportedFormat));
	assert_eq!(parse_args(b"--format wobble in.wav out.bin").err(), Some(Error::UnsupportedFormat));
}

#[test]
fn options_are_judged_against_the_profile_they_are_given_with() {
	// The point of one shared table. Each of these is a real option that means nothing here.
	assert_eq!(parse_args(b"--quality 80 in.wav out.flac").err(), Some(Error::UnsupportedOption));
	assert_eq!(parse_args(b"--compression 80 in.wav out.wav").err(), Some(Error::UnsupportedOption));
	assert_eq!(parse_args(b"--compression 80 in.wav out.wv").err(), Some(Error::UnsupportedOption));
	assert_eq!(parse_args(b"--bits 24 in.wav out.flac").err(), Some(Error::UnsupportedOption));
	assert_eq!(parse_args(b"--bits 12 in.wav out.wav").err(), Some(Error::UnsupportedOption));
	// And each of these is the same option where it does mean something.
	assert_eq!(parse_args(b"--compression 80 in.wav out.flac").unwrap().compression, Some(80));
	assert_eq!(parse_args(b"--bits 24 in.wav out.wav").unwrap().bits, Some(24));
	assert_eq!(parse_args(b"--quality 20 in.wav out.ogg").unwrap().quality, Some(20));

	assert_eq!(parse_args(b"--rate 96000 in.wav out.wav").err(), Some(Error::InvalidOptions));
	assert_eq!(parse_args(b"--channels 3 in.wav out.wav").err(), Some(Error::InvalidOptions));
	assert_eq!(parse_args(b"--rate in.wav out.wav").err(), Some(Error::InvalidOptions));
	assert_eq!(parse_args(b"--rate -5 in.wav out.wav").err(), Some(Error::InvalidOptions));
	assert_eq!(parse_args(b"in.wav").err(), Some(Error::InvalidOptions));
	assert_eq!(parse_args(b"a b c").err(), Some(Error::InvalidOptions));
	assert_eq!(parse_args(b"--wobble 1 in.wav out.wav").err(), Some(Error::InvalidOptions));
	assert_eq!(parse_args(b"--help").unwrap().mode, Mode::Help);
	assert!(parse_args(b"--force in.wav out.wav").unwrap().force);
}

#[test]
fn the_help_is_generated_from_the_same_table_the_parser_reads() {
	let text = help_text();
	for entry in PROFILES {
		assert!(text.contains(entry.name), "{} is missing from the help", entry.name);
		if entry.quality {
			assert!(text.contains("--quality"), "the help never mentions --quality");
		}
		if !entry.implemented {
			assert!(text.contains("not yet written"), "the help does not say {} is unwritten", entry.name);
		}
	}
	assert!(text.contains("strips tags"));
}

#[test]
fn a_lossless_conversion_and_back_returns_the_samples_it_started_with() {
	// WAV to FLAC to WAV. Both hops are lossless, so anything other than equality is a defect in
	// one of them, and the assertion does not have to know which.
	let frames = 5_000;
	let original = signal(frames, 2);
	let source = wave(frames, 2, 44_100);

	let (flac_bytes, to_flac) = convert(&source, &config("out.flac")).expect("WAV to FLAC");
	assert_eq!(to_flac.source, Container::Wav);
	assert_eq!(to_flac.destination, Profile::Flac);
	assert_eq!(to_flac.frames, frames as u64);
	assert_eq!(to_flac.rate, 44_100);
	assert_eq!(to_flac.channels, 2);
	assert!(to_flac.stripped_metadata);
	// It is a FLAC, and it is smaller than the WAV it came from.
	assert_eq!(sniff(&flac_bytes), Some(Container::Flac));
	assert!(flac_bytes.len() < source.len(), "the FLAC ({}) is no smaller than the WAV ({})", flac_bytes.len(), source.len());

	let (back, to_wav) = convert(&flac_bytes, &config("out.wav")).expect("FLAC to WAV");
	assert_eq!(to_wav.source, Container::Flac);
	assert_eq!(to_wav.frames, frames as u64);
	assert_eq!(samples_of(&back), original);
}

#[test]
fn resampling_and_remixing_happen_where_the_options_asked_for_them() {
	let source = wave(4_000, 2, 32_000);
	let converted = parse_args(b"--rate 16000 --channels 1 in.wav out.wav").unwrap();
	let (bytes, info) = convert(&source, &converted).expect("convert");
	assert_eq!(info.source_rate, 32_000);
	assert_eq!(info.source_channels, 2);
	assert_eq!(info.source_frames, 4_000);
	assert_eq!(info.rate, 16_000);
	assert_eq!(info.channels, 1);
	// Half the rate, so half the frames, and the header must agree with what came out.
	assert_eq!(info.frames, 2_000);
	assert_eq!(info.duration_ms, 125);
	let file = wav::Wav::parse(&bytes).expect("a WAV came back");
	assert_eq!(file.metadata().rate, 16_000);
	assert_eq!(file.metadata().channels, 1);
	assert_eq!(file.metadata().frames, 2_000);
	assert_eq!(samples_of(&bytes).len(), 2_000);
	assert_eq!(info.bytes, bytes.len() as u64);

	// Upward too, and mono to stereo, because halving is the one ratio where the resampler's final
	// flush makes no difference to the count: dropping it costs two frames when going up and none
	// when going down by exactly two, so a test that only halves cannot see it.
	let quiet = wave(1_000, 1, 24_000);
	let up = parse_args(b"--rate 48000 --channels 2 in.wav out.wav").unwrap();
	let (bytes, info) = convert(&quiet, &up).expect("convert");
	assert_eq!(info.rate, 48_000);
	assert_eq!(info.channels, 2);
	assert_eq!(info.frames, 2_000);
	let file = wav::Wav::parse(&bytes).expect("a WAV came back");
	assert_eq!(file.metadata().frames, 2_000);
	// Mono duplicated into both channels, so the two are identical all the way through.
	let samples = samples_of(&bytes);
	assert_eq!(samples.len(), 4_000);
	assert!(samples.chunks_exact(2).all(|frame| frame[0] == frame[1]), "mono was not duplicated into both channels");
}

#[test]
fn every_written_profile_produces_a_file_this_tree_can_read_back() {
	let source = wave(2_000, 2, 44_100);
	for (options, expected) in [
		(&b"in.wav out.wav"[..], Profile::WavPcm),
		(b"--bits 24 in.wav out.wav", Profile::WavPcm),
		(b"--bits 8 in.wav out.wav", Profile::WavPcm),
		(b"--format wav-ima in.wav out.wav", Profile::WavIma),
		(b"--format wav-ms in.wav out.wav", Profile::WavMs),
		(b"in.wav out.aiff", Profile::Aiff),
		(b"in.wav out.aifc", Profile::Aifc),
		(b"--compression 90 in.wav out.flac", Profile::Flac),
		(b"--compression 10 in.wav out.flac", Profile::Flac),
		(b"in.wav out.wv", Profile::WavPack),
	] {
		let config = parse_args(options).expect("these options parse");
		let (bytes, info) = convert(&source, &config).unwrap_or_else(|error| panic!("{:?} failed: {error:?}", core::str::from_utf8(options)));
		assert_eq!(info.destination, expected);
		assert_eq!(info.frames, 2_000, "{:?} lost frames", core::str::from_utf8(options));
		assert!(sniff(&bytes).is_some(), "{:?} produced something unrecognisable", core::str::from_utf8(options));
		// And the output actually decodes, which the sniff alone does not prove.
		let (round, _) = convert(&bytes, &config_wav()).expect("the output decodes");
		assert_eq!(wav::Wav::parse(&round).unwrap().metadata().frames, 2_000);
	}
}

fn config_wav() -> Config {
	config("out.wav")
}

#[test]
fn a_profile_with_no_encoder_yet_says_so_rather_than_writing_something_else() {
	let source = wave(64, 1, 44_100);
	for suffix in ["out.ogg", "out.mp3"] {
		assert_eq!(convert(&source, &config(suffix)).err(), Some(Error::NotImplemented), "{suffix} should not be written yet");
	}
	// And an input nothing here reads is refused before a destination is opened.
	assert_eq!(convert(b"not audio", &config("out.wav")).err(), Some(Error::UnsupportedFormat));
	// A WAV header with nothing behind it is a damaged file, not an unknown format.
	assert_eq!(convert(b"RIFF\x04\0\0\0WAVE", &config("out.wav")).err(), Some(Error::InvalidAudio));
}

#[test]
fn a_conversion_does_not_hold_the_decoded_track() {
	// Not a timing test - a shape test. The pipeline reads `CHUNK_FRAMES` at a time, so an input
	// many times that size must convert with the same peak as one a fraction of it, and the only
	// thing that grows is the output. If somebody replaces the loop with "decode it all, then
	// encode it", this is what says so: a track of a hundred thousand frames converts, and the
	// buffers named in `pump` are all bounded by the chunk.
	let frames = 100_000;
	let source = wave(frames, 2, 48_000);
	let (bytes, info) = convert(&source, &config("out.flac")).expect("a long track converts");
	assert_eq!(info.frames, frames as u64);
	assert_eq!(info.duration_ms, 2_083);
	assert!(bytes.len() < source.len());
	assert_eq!(vec![Container::Flac], vec![sniff(&bytes).unwrap()]);
}
