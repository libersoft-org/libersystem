// THE CODEC, HELD TO THE SPECIFICATION'S DEFINITIONS: its tables against the formulas they tabulate, the filter bank's
// near-perfect reconstruction with no quantization at all, frame lengths and bit rates the A2DP specification's own
// recommended configurations give, the allocation spending exactly its bitpool, and audio through the whole codec at
// the quality its bitpool buys. A differential run against an independent encoder and decoder is the oracle part i
// names; this is what the codec must do before that.
use super::*;
extern crate std;
use std::vec::Vec;

// Noise with no period, so the delay that aligns input and output is the one delay there is.
fn noise(frames: usize, amplitude: i32) -> Vec<i16> {
	let mut state: u32 = 0x1234_5678;
	(0..frames)
		.map(|_| {
			state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
			((state >> 16) as i32 % amplitude) as i16
		})
		.collect()
}

fn sine(frequency: f64, rate: u32, frames: usize, channels: usize, amplitude: f64) -> Vec<i16> {
	let mut out = Vec::with_capacity(frames * channels);
	for n in 0..frames {
		let phase = 2.0 * std::f64::consts::PI * frequency * n as f64 / f64::from(rate);
		for ch in 0..channels {
			// The right channel a little out of phase, so joint stereo has a difference to code.
			let value = amplitude * (phase + ch as f64 * 0.3).sin();
			out.push(value.round() as i16);
		}
	}
	out
}

// The signal-to-noise ratio of `output` against `input` for one channel, at the delay that aligns them best.
fn snr(input: &[i16], output: &[i16], channels: usize, ch: usize, skip: usize) -> (usize, f64) {
	let frames = input.len() / channels;
	let mut best = (0usize, f64::MIN);
	for delay in 0..200 {
		let mut signal = 0.0;
		let mut noise = 0.0;
		for n in skip..frames - delay - 1 {
			let x = f64::from(input[n * channels + ch]);
			let y = f64::from(output[(n + delay) * channels + ch]);
			signal += x * x;
			noise += (x - y) * (x - y);
		}
		let ratio = 10.0 * (signal / noise.max(1e-9)).log10();
		if ratio > best.1 {
			best = (delay, ratio);
		}
	}
	best
}

#[test]
// THE COSINES the matrices take are the formulas', to the last bit of Q30.
fn the_matrices_are_the_specifications_cosines() {
	use std::f64::consts::PI;
	for k in 0..8 {
		for i in 0..16 {
			let expected = ((k as f64 + 0.5) * (i as f64 - 4.0) * PI / 8.0).cos();
			assert!((M8[k][i] as f64 / 1_073_741_824.0 - expected).abs() < 2e-9, "M8[{k}][{i}]");
		}
	}
	for k in 0..4 {
		for i in 0..8 {
			let expected = ((k as f64 + 0.5) * (i as f64 - 2.0) * PI / 4.0).cos();
			assert!((M4[k][i] as f64 / 1_073_741_824.0 - expected).abs() < 2e-9, "M4[{k}][{i}]");
		}
	}
	for k in 0..16 {
		for i in 0..8 {
			let expected = ((i as f64 + 0.5) * (k as f64 + 4.0) * PI / 8.0).cos();
			assert!((N8[k][i] as f64 / 1_073_741_824.0 - expected).abs() < 2e-9, "N8[{k}][{i}]");
		}
	}
	for k in 0..8 {
		for i in 0..4 {
			let expected = ((i as f64 + 0.5) * (k as f64 + 2.0) * PI / 4.0).cos();
			assert!((N4[k][i] as f64 / 1_073_741_824.0 - expected).abs() < 2e-9, "N4[{k}][{i}]");
		}
	}
}

#[test]
// THE FILTER BANK ALONE, with no quantization: what the analysis splits, the synthesis puts back - near-perfect
// reconstruction, delayed - at both subband counts. This is what holds the prototype tables and the windowing order
// to the specification: a wrong coefficient, a wrong sign or a wrong gain shows here as noise or a scaled signal.
fn the_filter_bank_reconstructs_what_it_analyses() {
	for subbands in [4usize, 8] {
		let config = Config { frequency: 48_000, blocks: 16, mode: Mode::Mono, allocation: Allocation::Loudness, subbands: subbands as u8, bitpool: 32 };
		let mut encoder = Encoder::new(config).unwrap();
		let mut decoder = Decoder::new();
		let input = noise(4_800, 12_000);
		let mut output = std::vec![0i16; input.len()];
		for (block, chunk) in input.chunks(subbands).enumerate() {
			let mut samples = [0i32; 8];
			for (i, &sample) in chunk.iter().enumerate() {
				samples[i] = i32::from(sample);
			}
			let mut subband = [0i64; 8];
			encoder.analyse(0, &samples, &mut subband);
			let mut pcm = [0i16; 8];
			decoder.synthesise(0, subbands, &subband, &mut pcm);
			output[block * subbands..(block + 1) * subbands].copy_from_slice(&pcm[..subbands]);
		}
		let (delay, ratio) = snr(&input, &output, 1, 0, 200);
		assert!(ratio > 30.0, "{subbands} subbands reconstruct at {ratio:.1} dB (delay {delay})");
		assert_eq!(delay, 10 * subbands - subbands + 1, "the filter bank's delay is the prototype's length less one block, plus one");
		// AND NOT INVERTED: the one delay that aligns the two is positively correlated.
		let correlation: i64 = (200..input.len() - delay).map(|n| i64::from(input[n]) * i64::from(output[n + delay])).sum();
		assert!(correlation > 0, "the synthesis window's sign inverts the signal");
	}
}

#[test]
// THE A2DP SPECIFICATION'S RECOMMENDED CONFIGURATIONS: the frame lengths and bit rates its tables give - the
// high-quality joint stereo at 44.1 kHz is the 119-byte, 328 kbit/s frame - and mSBC's fixed 57 bytes.
fn frame_lengths_and_bit_rates_are_the_specifications() {
	let joint = |frequency, bitpool| Config { frequency, blocks: 16, mode: Mode::JointStereo, allocation: Allocation::Loudness, subbands: 8, bitpool };
	assert_eq!(joint(44_100, 53).frame_length(), 119);
	assert_eq!(joint(44_100, 53).bit_rate(), 327_993, "the 328 kbit/s the tables round to");
	assert_eq!(joint(48_000, 51).frame_length(), 115);
	assert_eq!(joint(48_000, 51).bit_rate(), 345_000);
	assert_eq!(joint(44_100, 35).frame_length(), 83);
	let mono = Config { frequency: 48_000, blocks: 16, mode: Mode::Mono, allocation: Allocation::Loudness, subbands: 8, bitpool: 31 };
	assert_eq!(mono.frame_length(), 4 + 4 + 62);
	assert_eq!(Config::MSBC.frame_length(), 57);
	assert_eq!(Config::MSBC.samples(), 120);
	assert!(Config::MSBC.valid());
	assert!(!Config { blocks: 15, ..mono }.valid(), "fifteen blocks is mSBC's alone");
	assert!(!Config { bitpool: 1, ..mono }.valid());
	assert!(!Config { bitpool: 129, ..mono }.valid(), "mono's bitpool is at most 16 per subband");
}

#[test]
// THE ALLOCATION SPENDS ITS BITPOOL: exactly, where the subbands can take it, in every mode and for both methods.
fn the_allocation_spends_exactly_its_bitpool() {
	for mode in [Mode::Mono, Mode::DualChannel, Mode::Stereo, Mode::JointStereo] {
		for allocation in [Allocation::Loudness, Allocation::Snr] {
			for bitpool in [8u8, 19, 32, 53] {
				let config = Config { frequency: 44_100, blocks: 16, mode, allocation, subbands: 8, bitpool };
				let factors = [[9, 8, 7, 6, 5, 4, 3, 2], [10, 9, 8, 6, 4, 2, 1, 0]];
				let mut bits = [[0u8; 8]; 2];
				allocate(&config, &factors, &mut bits);
				let per_channel = |ch: usize| bits[ch].iter().map(|&b| u32::from(b)).sum::<u32>();
				match mode {
					Mode::Mono => assert_eq!(per_channel(0), u32::from(bitpool), "{mode:?} {allocation:?} {bitpool}"),
					Mode::DualChannel => {
						assert_eq!(per_channel(0), u32::from(bitpool));
						assert_eq!(per_channel(1), u32::from(bitpool));
					}
					_ => assert_eq!(per_channel(0) + per_channel(1), u32::from(bitpool), "{mode:?} {allocation:?} {bitpool}"),
				}
				assert!(bits.iter().flatten().all(|&b| b <= 16 && b != 1), "a subband takes none, or two to sixteen bits");
			}
		}
	}
}

// Encode then decode `input` whole frames at `config`.
fn through(config: Config, input: &[i16]) -> Vec<i16> {
	let channels = config.mode.channels();
	let mut encoder = Encoder::new(config).unwrap();
	let mut decoder = Decoder::new();
	let per_frame = config.samples() * channels;
	let mut output = std::vec![0i16; input.len()];
	let mut frame = [0u8; MAX_FRAME_BYTES];
	for (at, chunk) in input.chunks_exact(per_frame).enumerate() {
		let length = encoder.encode(chunk, &mut frame);
		assert_eq!(length, config.frame_length());
		let (read, frames) = decoder.decode(&frame[..length], &mut output[at * per_frame..]).expect("a frame this encoder made decodes");
		assert_eq!(read, config);
		assert_eq!(frames, config.samples());
	}
	output
}

#[test]
// AUDIO THROUGH THE WHOLE CODEC at the quality its bitpool buys: the high-quality configurations well above 30 dB on a
// tone, the low ones still the tone, in every channel mode and at both subband counts.
fn audio_survives_the_codec_at_the_quality_its_bitpool_buys() {
	let cases = [
		(Config { frequency: 44_100, blocks: 16, mode: Mode::JointStereo, allocation: Allocation::Loudness, subbands: 8, bitpool: 53 }, 30.0),
		(Config { frequency: 48_000, blocks: 16, mode: Mode::Stereo, allocation: Allocation::Snr, subbands: 8, bitpool: 51 }, 30.0),
		(Config { frequency: 32_000, blocks: 8, mode: Mode::DualChannel, allocation: Allocation::Loudness, subbands: 4, bitpool: 32 }, 25.0),
		(Config { frequency: 16_000, blocks: 4, mode: Mode::Mono, allocation: Allocation::Loudness, subbands: 4, bitpool: 18 }, 15.0),
		(Config::MSBC, 20.0),
	];
	for (config, floor) in cases {
		let channels = config.mode.channels();
		let frames = config.samples() * 60;
		let input = sine(1_000.0, config.frequency, frames, channels, 10_000.0);
		let output = through(config, &input);
		for ch in 0..channels {
			let (_, ratio) = snr(&input, &output, channels, ch, 300);
			assert!(ratio > floor, "{config:?} channel {ch}: {ratio:.1} dB, below {floor}");
		}
	}
}

#[test]
// A FRAME THAT WAS DAMAGED IS REFUSED: a flipped scale-factor bit fails the CRC, a short one is short, a strange sync
// word is not a frame.
fn a_damaged_frame_is_refused() {
	let config = Config { frequency: 44_100, blocks: 16, mode: Mode::JointStereo, allocation: Allocation::Loudness, subbands: 8, bitpool: 53 };
	let mut encoder = Encoder::new(config).unwrap();
	let input = sine(440.0, 44_100, config.samples(), 2, 8_000.0);
	let mut frame = [0u8; MAX_FRAME_BYTES];
	let length = encoder.encode(&input, &mut frame);
	let mut pcm = [0i16; 256];
	assert!(Decoder::new().decode(&frame[..length], &mut pcm).is_ok());
	let mut damaged = frame;
	damaged[6] ^= 0x10;
	assert_eq!(Decoder::new().decode(&damaged[..length], &mut pcm), Err(Refusal::Crc));
	assert_eq!(Decoder::new().decode(&frame[..length - 1], &mut pcm), Err(Refusal::Short));
	let mut odd = frame;
	odd[0] = 0x9d;
	assert_eq!(Decoder::new().decode(&odd[..length], &mut pcm), Err(Refusal::Sync));
	assert_eq!(parse_header(&frame[..length]), Ok(config));
}

#[test]
// THE CRC'S OWN DEFINITION: x^8 + x^4 + x^3 + x^2 + 1 from 0x0F, most significant bit first - checked bit by bit against
// a direct polynomial division.
fn the_crc_is_the_specified_polynomial() {
	let data = [0x31u8, 0x35, 0xa5, 0x5a, 0xff];
	for bits in [8, 16, 20, 33, 40] {
		let mut register: u32 = 0x0f;
		for at in 0..bits {
			let bit = u32::from((data[at / 8] >> (7 - at % 8)) & 1);
			let feedback = ((register >> 7) & 1) ^ bit;
			register = (register << 1) & 0xff;
			if feedback != 0 {
				register ^= 0x1d;
			}
		}
		assert_eq!(crc8(&data, bits), register as u8);
	}
}
