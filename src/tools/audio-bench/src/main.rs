use std::time::Instant;

const LOGICAL_SECONDS: u64 = 60;
const CHUNK_FRAMES: usize = 1_024;

const MP3: &[u8] = include_bytes!("../../../volume/audio/test.mp3");

trait Decoder {
	fn read_i16_le(&mut self, max_frames: usize, output: &mut Vec<u8>) -> Result<usize, ()>;
}

macro_rules! impl_decoder {
	($type:ty) => {
		impl Decoder for $type {
			fn read_i16_le(&mut self, max_frames: usize, output: &mut Vec<u8>) -> Result<usize, ()> {
				self.read_i16_le(max_frames, output).map_err(|_| ())
			}
		}
	};
}

impl_decoder!(mp3::Decoder<'_>);

fn drain(mut decoder: impl Decoder) -> u64 {
	let mut output = Vec::new();
	let mut frames = 0u64;
	loop {
		let decoded = decoder.read_i16_le(CHUNK_FRAMES, &mut output).expect("decoder rejected staged fixture");
		if decoded == 0 {
			break;
		}
		frames += decoded as u64;
	}
	frames
}

fn bench(name: &str, mut decode: impl FnMut() -> (u64, u32)) -> f64 {
	let (fixture_frames, rate) = decode();
	assert!(fixture_frames != 0 && rate != 0, "{name} decoded no audio");
	let target_frames = LOGICAL_SECONDS * rate as u64;
	let iterations = target_frames.div_ceil(fixture_frames).clamp(1, 10_000);
	let start = Instant::now();
	let mut decoded_frames = 0u64;
	for _ in 0..iterations {
		let (frames, observed_rate) = decode();
		assert_eq!(observed_rate, rate, "{name} rate changed between iterations");
		assert_eq!(frames, fixture_frames, "{name} frame count changed between iterations");
		decoded_frames += frames;
	}
	std::hint::black_box(decoded_frames);
	let elapsed = start.elapsed();
	let logical_seconds = decoded_frames as f64 / rate as f64;
	let realtime = logical_seconds / elapsed.as_secs_f64();
	println!("| {name} | {rate} | {fixture_frames} | {iterations} | {:.3} | {:.1}x |", elapsed.as_secs_f64(), realtime);
	assert!(realtime > 1.0, "{name} decoder is slower than real time: {realtime:.2}x");
	realtime
}

// The signal the encoders are measured on, and it is deliberately not silence.
//
// Silence is the best case for every one of them - FLAC codes a constant subframe, WavPack's
// medians collapse to a zero run, Vorbis's floor sits flat - so a bench over silence measures the
// container's overhead and calls it throughput. This is two tones an octave apart plus a slow
// sweep, which gives the predictors something to be wrong about and keeps the output size
// meaningful.
fn fixture(frames: usize, channels: u8, rate: u32) -> Vec<i16> {
	let mut samples = Vec::with_capacity(frames * channels as usize);
	for frame in 0..frames {
		let t = frame as f64 / rate as f64;
		let sweep = 200.0 + 1_800.0 * (t * 0.25).fract();
		let value = 0.35 * (core::f64::consts::TAU * 440.0 * t).sin() + 0.25 * (core::f64::consts::TAU * 880.0 * t).sin() + 0.20 * (core::f64::consts::TAU * sweep * t).sin();
		let left = (value * 30_000.0) as i16;
		samples.push(left);
		if channels == 2 {
			// The right channel is not a copy: a joint-stereo encoder that assumed it was would be
			// measured on material that flatters it.
			samples.push((value * 24_000.0) as i16 - 1_500);
		}
	}
	samples
}

// One encode pass over `samples`, reported the way the decoder rows are: wall clock, the realtime
// factor, and the size of what came out. The size is a row of its own because it is the number a
// caller actually chooses a profile by, and it is not derivable from the throughput.
fn bench_encode(name: &str, rate: u32, channels: u8, frames: usize, mut encode: impl FnMut(&[i16]) -> usize) -> f64 {
	let samples = fixture(frames, channels, rate);
	let start = Instant::now();
	let bytes = encode(&samples);
	let elapsed = start.elapsed();
	std::hint::black_box(bytes);
	let logical_seconds = frames as f64 / rate as f64;
	let realtime = logical_seconds / elapsed.as_secs_f64();
	let kbps = (bytes as f64 * 8.0 / 1_000.0) / logical_seconds;
	println!("| {name} | {rate} | {channels} | {frames} | {:.3} | {realtime:.1}x | {bytes} | {kbps:.0} |", elapsed.as_secs_f64());
	assert!(realtime > 1.0, "{name} encoder is slower than real time: {realtime:.2}x");
	realtime
}

fn main() {
	println!("| codec/container | rate (Hz) | fixture frames | iterations | wall (s) | realtime |");
	println!("| --- | ---: | ---: | ---: | ---: | ---: |");
	let results = [bench("MP3", || {
		let audio = mp3::Mp3::parse(MP3).unwrap();
		(drain(audio.decoder()), audio.metadata().rate)
	})];
	let slowest = results.into_iter().fold(f64::INFINITY, f64::min);
	println!("slowest decoder: {slowest:.1}x realtime");

	// THE ENCODERS, over the same logical minute the decoder rows use. Vorbis gets a shorter fixture
	// and says so: it is the one encoder that holds the whole decoded track (its floor scale is one
	// number for the stream), so measuring it over a minute would measure the host's memory rather
	// than the encoder.
	const RATE: u32 = 44_100;
	const CHANNELS: u8 = 2;
	let frames = (LOGICAL_SECONDS * RATE as u64) as usize;
	println!();
	println!("| encoder | rate (Hz) | channels | frames | wall (s) | realtime | bytes | kbit/s |");
	println!("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
	let format = pcm::Format::new(RATE, CHANNELS).expect("the bench format is one `Format` names");
	let ceiling: u64 = 1 << 30;
	let encoded = [
		bench_encode("WAV PCM 16", RATE, CHANNELS, frames, |samples| {
			let mut encoder = wav::encode::Encoder::new(pcm::encode::VecSink::new(ceiling), format, wav::encode::Output::Pcm { bits: 16 }).expect("WAV encoder");
			encoder.push(samples).expect("WAV push");
			encoder.finish().expect("WAV finish").0.into_bytes().len()
		}),
		bench_encode("WAV IMA ADPCM", RATE, CHANNELS, frames, |samples| {
			let mut encoder = wav::encode::Encoder::new(pcm::encode::VecSink::new(ceiling), format, wav::encode::Output::ima_default(CHANNELS)).expect("IMA encoder");
			encoder.push(samples).expect("IMA push");
			encoder.finish().expect("IMA finish").0.into_bytes().len()
		}),
		bench_encode("AIFF", RATE, CHANNELS, frames, |samples| {
			let mut encoder = aiff::encode::Encoder::new(pcm::encode::VecSink::new(ceiling), format, aiff::encode::Output::Aiff { bits: 16 }).expect("AIFF encoder");
			encoder.push(samples).expect("AIFF push");
			encoder.finish().expect("AIFF finish").0.into_bytes().len()
		}),
		bench_encode("FLAC", RATE, CHANNELS, frames, |samples| {
			let mut encoder = flac::encode::Encoder::new(pcm::encode::VecSink::new(ceiling), format, flac::encode::Effort::from_percent(50)).expect("FLAC encoder");
			encoder.push(samples).expect("FLAC push");
			encoder.finish().expect("FLAC finish").0.into_bytes().len()
		}),
		bench_encode("WavPack", RATE, CHANNELS, frames, |samples| {
			let mut encoder = wavpack::encode::Encoder::new(pcm::encode::VecSink::new(ceiling), format, true).expect("WavPack encoder");
			encoder.push(samples).expect("WavPack push");
			encoder.finish().expect("WavPack finish").0.into_bytes().len()
		}),
	];
	// Ten seconds rather than sixty, for the reason above.
	let vorbis_frames = (10 * RATE) as usize;
	let vorbis = bench_encode("Ogg Vorbis (10 s)", RATE, CHANNELS, vorbis_frames, |samples| {
		let mut channels: Vec<Vec<f32>> = vec![Vec::with_capacity(samples.len() / 2); CHANNELS as usize];
		for frame in samples.chunks_exact(CHANNELS as usize) {
			for (index, channel) in channels.iter_mut().enumerate() {
				channel.push(frame[index] as f32 / 32_768.0);
			}
		}
		vorbis::encode::encode(&channels, RATE, 11, 0x4c69_6265).expect("Vorbis encode").len()
	});
	let slowest = encoded.into_iter().chain(core::iter::once(vorbis)).fold(f64::INFINITY, f64::min);
	println!("slowest encoder: {slowest:.1}x realtime");
}
