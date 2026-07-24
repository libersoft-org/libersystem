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

fn main() {
	println!("| codec/container | rate (Hz) | fixture frames | iterations | wall (s) | realtime |");
	println!("| --- | ---: | ---: | ---: | ---: | ---: |");
	let results = [bench("MP3", || {
		let audio = mp3::Mp3::parse(MP3).unwrap();
		(drain(audio.decoder()), audio.metadata().rate)
	})];
	let slowest = results.into_iter().fold(f64::INFINITY, f64::min);
	println!("slowest decoder: {slowest:.1}x realtime");
}
