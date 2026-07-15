use std::time::Instant;

const LOGICAL_SECONDS: u64 = 60;
const CHUNK_FRAMES: usize = 1_024;

const WAV: &[u8] = include_bytes!("../../../volume/sample.wav");
const WAV_IMA: &[u8] = include_bytes!("../../../volume/sample-ima.wav");
const WAV_MS: &[u8] = include_bytes!("../../../volume/sample-ms.wav");
const AIFF: &[u8] = include_bytes!("../../../volume/sample.aiff");
const AIFC: &[u8] = include_bytes!("../../../volume/sample.aifc");
const FLAC: &[u8] = include_bytes!("../../../volume/sample.flac");
const MP3: &[u8] = include_bytes!("../../../volume/sample.mp3");
const OGG: &[u8] = include_bytes!("../../../volume/sample.ogg");
const WAVPACK: &[u8] = include_bytes!("../../../volume/sample.wv");
const WAVPACK_STEREO: &[u8] = include_bytes!("../../../volume/sample-stereo.wv");

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

impl_decoder!(aiff::Decoder<'_>);
impl_decoder!(flac::Decoder<'_>);
impl_decoder!(mp3::Decoder<'_>);
impl_decoder!(vorbis::Decoder<'_>);
impl_decoder!(wav::Decoder<'_>);
impl_decoder!(wavpack::Decoder<'_>);

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

fn main() {
	println!("| codec/container | rate (Hz) | fixture frames | iterations | wall (s) | realtime |");
	println!("| --- | ---: | ---: | ---: | ---: | ---: |");
	let results = [
		bench("WAV PCM", || {
			let audio = wav::Wav::parse(WAV).unwrap();
			(drain(audio.decoder()), audio.metadata().rate)
		}),
		bench("WAV IMA ADPCM", || {
			let audio = wav::Wav::parse(WAV_IMA).unwrap();
			(drain(audio.decoder()), audio.metadata().rate)
		}),
		bench("WAV MS ADPCM", || {
			let audio = wav::Wav::parse(WAV_MS).unwrap();
			(drain(audio.decoder()), audio.metadata().rate)
		}),
		bench("AIFF PCM", || {
			let audio = aiff::Aiff::parse(AIFF).unwrap();
			(drain(audio.decoder()), audio.metadata().rate)
		}),
		bench("AIFC PCM", || {
			let audio = aiff::Aiff::parse(AIFC).unwrap();
			(drain(audio.decoder()), audio.metadata().rate)
		}),
		bench("FLAC", || {
			let audio = flac::Flac::parse(FLAC).unwrap();
			(drain(audio.decoder()), audio.metadata().rate)
		}),
		bench("MP3", || {
			let audio = mp3::Mp3::parse(MP3).unwrap();
			(drain(audio.decoder()), audio.metadata().rate)
		}),
		bench("Ogg Vorbis", || {
			let audio = vorbis::Vorbis::parse(OGG).unwrap();
			(drain(audio.decoder()), audio.metadata().rate)
		}),
		bench("WavPack mono", || {
			let audio = wavpack::WavPack::parse(WAVPACK).unwrap();
			(drain(audio.decoder()), audio.metadata().rate)
		}),
		bench("WavPack stereo", || {
			let audio = wavpack::WavPack::parse(WAVPACK_STEREO).unwrap();
			(drain(audio.decoder()), audio.metadata().rate)
		}),
	];
	let slowest = results.into_iter().fold(f64::INFINITY, f64::min);
	println!("slowest decoder: {slowest:.1}x realtime");
}
