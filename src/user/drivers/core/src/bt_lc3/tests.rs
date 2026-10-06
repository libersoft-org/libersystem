use super::vectors::{DECODED, ENCODED};
use super::*;

const RATES: [u32; 6] = [8_000, 16_000, 24_000, 32_000, 44_100, 48_000];
const SIZES: [usize; 9] = [20, 26, 30, 40, 60, 100, 120, 155, 400];

fn lines_of(sample_rate: u32) -> usize {
	geometry(sample_rate, 10_000, 40).unwrap().ne
}

// The spectrum as long as the frame codes it, from a vector's own length.
fn spectrum_of(frame: &Frame, lines: usize) -> &[i32] {
	&frame.spectrum[..lines]
}

// ------------------------------------------------------------------ Appendix C

// THE SPECIFICATION'S DECODER, FRAME BY FRAME: every value Appendix C prints after reading its four bitstreams - the
// side information, the TNS orders and coefficient indices the arithmetic decoder found, the residual bits, the zero
// frame flag, the whole quantized spectrum and the noise filling seed taken from it.
#[test]
fn the_appendix_bitstreams_read_as_its_decoder_read_them() {
	for (encoded, decoded) in ENCODED.iter().zip(DECODED.iter()) {
		assert_eq!(encoded.frame_us, decoded.frame_us);
		let frame = read_frame(encoded.bytes, 16_000, decoded.frame_us).expect("the appendix's frame reads");
		assert_eq!(frame.bandwidth, decoded.bandwidth);
		assert_eq!(frame.lastnz, decoded.lastnz);
		assert_eq!(frame.lsb_mode, decoded.lsb_mode);
		assert_eq!(frame.global_gain, decoded.global_gain);
		assert_eq!(if frame.bandwidth < 3 { 1 } else { 2 }, decoded.num_tns_filters);
		assert_eq!(frame.tns_order.map(|order| u8::from(order > 0)), decoded.tns_active);
		assert_eq!(frame.tns_order, decoded.tns_order);
		assert_eq!(frame.tns_index, decoded.tns_index);
		assert_eq!(geometry(16_000, decoded.frame_us, encoded.bytes.len()).unwrap().tns_lpc_weighting, usize::from(decoded.tns_lpc_weighting));
		assert_eq!(frame.pitch.is_some(), decoded.pitch_present);
		assert_eq!(frame.pitch.map_or(0, |pitch| pitch.index), decoded.pitch_index);
		assert_eq!(frame.pitch.is_some_and(|pitch| pitch.ltpf_active), decoded.ltpf_active);
		assert_eq!(frame.noise, decoded.noise);
		assert_eq!(frame.sns.ind_lf, decoded.ind_lf);
		assert_eq!(frame.sns.ind_hf, decoded.ind_hf);
		assert_eq!(frame.sns.shape >> 1, decoded.submode_msb);
		assert_eq!(frame.sns.gain, decoded.gind);
		assert_eq!(frame.sns.ls_a, decoded.ls_a);
		assert_eq!(frame.sns.idx_a, decoded.idx_a);
		if let Some(lsb) = decoded.submode_lsb {
			assert_eq!(frame.sns.shape & 1, lsb);
		}
		if let Some(shape) = decoded.shape {
			assert_eq!(frame.sns.shape, shape);
		}
		if let Some(ls_b) = decoded.ls_b {
			assert_eq!(frame.sns.ls_b, ls_b);
		}
		if let Some(idx_b) = decoded.idx_b {
			assert_eq!(frame.sns.idx_b, idx_b);
		}
		assert_eq!(frame.room, decoded.nbits_residual);
		let residual: Vec<u8> = frame.residual[..usize::from(frame.residual_len)].iter().map(|&bit| u8::from(bit)).collect();
		assert_eq!(residual, decoded.residual);
		let zero_frame = frame.lastnz == 2 && frame.spectrum[0] == 0 && frame.spectrum[1] == 0 && frame.global_gain == 0 && frame.noise == 7;
		assert_eq!(zero_frame, decoded.zero_frame);
		assert_eq!(spectrum_of(&frame, decoded.spectrum.len()), decoded.spectrum);
		assert!(frame.spectrum[decoded.spectrum.len()..].iter().all(|&value| value == 0));
		let seed = frame.spectrum.iter().enumerate().map(|(k, value)| value.unsigned_abs() * k as u32).sum::<u32>() & 0xffff;
		assert_eq!(seed, decoded.nf_seed);
	}
}

// THE SPECIFICATION'S ENCODER, FRAME BY FRAME: what it put into each bitstream - after its global gain adjustment and
// requantization where it made them - is what the reader finds there, the whole quantized spectrum included.
#[test]
fn the_appendix_bitstreams_carry_what_its_encoder_put_in() {
	for encoded in ENCODED.iter() {
		let frame = read_frame(encoded.bytes, 16_000, encoded.frame_us).expect("the appendix's frame reads");
		assert_eq!(frame.bandwidth, encoded.bandwidth);
		assert_eq!(frame.global_gain, encoded.global_gain);
		assert_eq!(frame.lastnz, encoded.lastnz);
		assert_eq!(frame.lsb_mode, encoded.lsb_mode);
		assert_eq!(frame.tns_order, encoded.tns_order);
		for f in 0..2 {
			let order = usize::from(encoded.tns_order[f]);
			assert_eq!(frame.tns_index[f][..order], encoded.tns_index[f][..order]);
		}
		assert_eq!(frame.pitch.is_some(), encoded.pitch_present);
		if let Some(pitch) = frame.pitch {
			assert_eq!(pitch.ltpf_active, encoded.ltpf_active);
			assert_eq!(pitch.index, encoded.pitch_index);
		}
		assert_eq!((frame.sns.ind_lf, frame.sns.ind_hf), (encoded.ind_lf, encoded.ind_hf));
		assert_eq!(frame.sns.shape >> 1, encoded.submode_msb);
		assert_eq!(frame.sns.gain, encoded.gind);
		assert_eq!((frame.sns.ls_a, frame.sns.idx_a), (encoded.ls_a, encoded.idx_a));
		assert!(encoded.ls_b.is_none_or(|ls_b| ls_b == frame.sns.ls_b));
		assert!(encoded.idx_b.is_none_or(|idx_b| idx_b == frame.sns.idx_b));
		assert!(encoded.shape.is_none_or(|shape| shape == frame.sns.shape));
		assert_eq!(frame.noise, encoded.noise);
		assert_eq!(spectrum_of(&frame, encoded.spectrum.len()), encoded.spectrum);
		// The encoder had more residual bits than the frame had room for: the frame carries the first of them.
		let carried: Vec<u8> = frame.residual[..usize::from(frame.residual_len)].iter().map(|&bit| u8::from(bit)).collect();
		assert_eq!(carried[..], encoded.residual[..carried.len()]);
		assert_eq!(usize::from(frame.room).min(encoded.residual.len()), carried.len());
		if encoded.frame_us == 10_000 {
			let loudest = (0..encoded.spectrum.len()).fold(None::<usize>, |best, k| match best {
				Some(top) if encoded.spectrum[top].unsigned_abs() >= encoded.spectrum[k].unsigned_abs() => Some(top),
				_ if encoded.spectrum[k] != 0 => Some(k),
				other => other,
			});
			let info = read(encoded.bytes, 16_000, 10_000).unwrap();
			assert_eq!(info, FrameInfo { bandwidth: encoded.bandwidth, lastnz: encoded.lastnz, global_gain: encoded.global_gain, loudest: loudest.map(|k| k as u16) });
		}
	}
}

// AND WRITTEN BACK, BYTE FOR BYTE: the encoder's values laid out by `write_frame` are the appendix's bitstreams. The
// appendix prints no LS_indB for its 10 ms frames, nor their shape (regular: the submode bit is 0 and there is an idxB);
// the sign alone is taken from the frame as read.
#[test]
fn the_appendix_bitstreams_are_written_byte_for_byte() {
	for encoded in ENCODED.iter() {
		let read_back = read_frame(encoded.bytes, 16_000, encoded.frame_us).unwrap();
		let mut frame = Frame::empty();
		frame.bandwidth = encoded.bandwidth;
		frame.lastnz = encoded.lastnz;
		frame.lsb_mode = encoded.lsb_mode;
		frame.global_gain = encoded.global_gain;
		frame.tns_order = encoded.tns_order;
		frame.tns_index = encoded.tns_index;
		frame.pitch = encoded.pitch_present.then_some(Pitch { ltpf_active: encoded.ltpf_active, index: encoded.pitch_index });
		assert!(encoded.shape.is_some() || (encoded.submode_msb == 0 && encoded.idx_b.is_some()));
		frame.sns = Sns { ind_lf: encoded.ind_lf, ind_hf: encoded.ind_hf, shape: encoded.shape.unwrap_or(0), gain: encoded.gind, ls_a: encoded.ls_a, idx_a: encoded.idx_a, ls_b: encoded.ls_b.unwrap_or(read_back.sns.ls_b), idx_b: encoded.idx_b.unwrap_or(0) };
		frame.noise = encoded.noise;
		frame.spectrum[..encoded.spectrum.len()].copy_from_slice(encoded.spectrum);
		for (bit, &value) in frame.residual.iter_mut().zip(encoded.residual) {
			*bit = value == 1;
		}
		frame.residual_len = encoded.residual.len() as u16;
		let mut out = vec![0xa5u8; encoded.bytes.len()];
		write_frame(&frame, 16_000, encoded.frame_us, &mut out).expect("the appendix's frame writes");
		assert_eq!(out, encoded.bytes);
	}
}

// ------------------------------------------------------------------ the tone

fn tone(line: usize, sample_rate: u32, size: usize) -> Vec<u8> {
	let mut out = vec![0u8; size];
	write_tone(line as u16, sample_rate, 10_000, &mut out).unwrap_or_else(|error| panic!("{sample_rate} Hz, {size} bytes, line {line}: {error}"));
	out
}

fn tone_lines(sample_rate: u32) -> Vec<usize> {
	let ne = lines_of(sample_rate);
	vec![0, 1, 2, 3, 20, 23, 24, 25, ne / 2 - 1, ne / 2, ne / 2 + 1, ne - 2, ne - 1]
}

// WHAT THE SOURCE WRITES THE EARBUD READS: at every rate, at sizes from the smallest to the largest, for low, middle
// and the highest lines - one line, where it was written, and the frame's 2-tuples ending just past it.
#[test]
fn a_written_tone_reads_back_as_its_line() {
	for rate in RATES {
		let fs_ind = geometry(rate, 10_000, 40).unwrap().fs_ind as u8;
		for size in SIZES {
			for line in tone_lines(rate) {
				let bytes = tone(line, rate, size);
				let info = read(&bytes, rate, 10_000).unwrap_or_else(|error| panic!("{rate} Hz, {size} bytes, line {line}: {error}"));
				assert_eq!(info.loudest, Some(line as u16), "{rate} Hz, {size} bytes, line {line}");
				assert_eq!(info.lastnz, (line / 2 * 2 + 2) as u16);
				assert_eq!(info.bandwidth, fs_ind);
				let frame = read_frame(&bytes, rate, 10_000).unwrap();
				assert_eq!(frame.spectrum.iter().filter(|&&value| value != 0).count(), 1);
				assert!(frame.spectrum[line] > 0);
				assert_eq!(frame.sns, TONE_SNS);
				assert_eq!((frame.noise, frame.tns_order, frame.pitch, frame.lsb_mode), (7, [0, 0], None, false));
				assert_eq!(frame.residual_len, frame.room.min(1));
				// The same arguments, the same frame.
				assert_eq!(tone(line, rate, size), bytes);
			}
		}
	}
}

// THE LEVEL A DECODER WILL SYNTHESISE: the line's value after its residual refinement, the global gain and the SNS gain,
// as a sine's amplitude (V * sqrt(2 / N_F)), is -12 dBFS to within a quarter of a decibel, and the noise filling lies 40
// dB and more below it - wherever the frame has the bits for the full magnitude, which is everywhere but 20 bytes at 32
// kHz and above. There the highest lines take fewer bit planes, and the level stays between -20 and -10 dBFS.
#[test]
fn a_written_tone_is_at_minus_twelve_dbfs() {
	for rate in RATES {
		let g = geometry(rate, 10_000, 40).unwrap();
		let nf: f64 = [80.0, 160.0, 240.0, 320.0, 480.0][g.fs_ind];
		for size in SIZES {
			for line in tone_lines(rate) {
				let frame = read_frame(&tone(line, rate, size), rate, 10_000).unwrap();
				let magnitude = frame.spectrum[line];
				let refinement = match (frame.residual_len, frame.residual[0]) {
					(0, _) => 0.0,
					(_, false) => -0.1875,
					(_, true) => 0.3125,
				};
				let gg_off = -((size * 8 / (10 * (g.fs_ind + 1))).min(115) as f64) - 105.0 - 5.0 * (g.fs_ind as f64 + 1.0);
				let gain = 10f64.powf((f64::from(frame.global_gain) + gg_off) / 28.0);
				let value = (f64::from(magnitude) + refinement) * gain * 2f64.powf(f64::from(tone_scale_factor(g.fs_ind, line)));
				let dbfs = 20.0 * (value * (2.0 / nf).sqrt() / 32_768.0).log10();
				let full = !(size == 20 && g.fs_ind >= 3);
				if magnitude >= 10 {
					assert!((dbfs + 12.0).abs() < 0.25, "{rate} Hz, {size} bytes, line {line}: {dbfs} dBFS from magnitude {magnitude}");
				} else {
					assert!(!full && (-20.0..-10.0).contains(&dbfs), "{rate} Hz, {size} bytes, line {line}: {dbfs} dBFS from magnitude {magnitude}");
				}
				if full {
					// Noise filling: 1/16 on every empty line from 24 up, against the line's magnitude.
					let filled = (g.ne - 24) as f64 / 256.0;
					let below = 10.0 * (f64::from(magnitude).powi(2) / filled).log10();
					assert!(below > 40.0, "{rate} Hz, {size} bytes, line {line}: noise {below} dB below");
				}
			}
		}
	}
}

#[test]
fn the_tone_arithmetic_is_accurate() {
	for x in [-3.7f32, -1.0, 0.0, 0.25, 1.0, 7.5, 10.123, 15.9] {
		assert!(((exp2(x) as f64) / 2f64.powf(f64::from(x)) - 1.0).abs() < 1e-5, "exp2({x})");
	}
	assert_eq!((nearest(2.5), nearest(2.49), nearest(-2.5), nearest(-0.4)), (3, 2, -3, 0));
	for (half, nf) in TONE_HALF_LOG2_HALF_NF.iter().zip([80.0f64, 160.0, 240.0, 320.0, 480.0]) {
		assert!((f64::from(*half) - 0.5 * (nf / 2.0).log2()).abs() < 1e-6);
	}
	assert!((f64::from(TONE_LOG2_AMPLITUDE) - (32_768.0 * 10f64.powf(-12.0 / 20.0)).log2()).abs() < 1e-6);
	assert!((f64::from(LOG2_GAIN_STEP) - 10f64.log2() / 28.0).abs() < 1e-7);
}

// ------------------------------------------------------------------ refusals and errors

#[test]
fn what_is_not_a_frame_is_refused() {
	let mut out = [0u8; 40];
	assert!(write_tone(10, 22_050, 10_000, &mut out).is_err());
	assert!(write_tone(10, 16_000, 7_500, &mut out).is_err());
	assert!(write_tone(10, 16_000, 5_000, &mut out).is_err());
	assert!(write_tone(160, 16_000, 10_000, &mut out).is_err());
	assert!(write_tone(80, 8_000, 10_000, &mut out).is_err());
	assert!(write_tone(399, 48_000, 10_000, &mut [0u8; 19]).is_err());
	assert!(write_tone(399, 48_000, 10_000, &mut [0u8; 401]).is_err());
	assert!(write_tone(399, 48_000, 10_000, &mut [0u8; 20]).is_ok());
	assert!(write_tone(399, 48_000, 10_000, &mut [0u8; 400]).is_ok());
	let good = tone(10, 16_000, 40);
	assert!(read(&good, 16_000, 10_000).is_ok());
	assert!(read(&good, 22_050, 10_000).is_err());
	assert!(read(&good, 16_000, 7_500).is_err());
	assert!(read(&good, 16_000, 2_500).is_err());
	assert!(read(&good[..19], 16_000, 10_000).is_err());
	assert!(read(&[0u8; 401], 16_000, 10_000).is_err());
	assert!(read(&[], 16_000, 10_000).is_err());
}

// THE SIDE INFORMATION'S OWN CHECKS: a bandwidth past the rate's, and lastnz past the coded lines.
#[test]
fn a_decoder_refuses_a_bandwidth_or_lastnz_past_the_rate() {
	// 24 kHz: two bandwidth bits, the last byte's lowest - 3 is past its three bandwidths.
	let mut bytes = tone(30, 24_000, 40);
	assert!(read(&bytes, 24_000, 10_000).is_ok());
	bytes[39] |= 0b11;
	assert!(read(&bytes, 24_000, 10_000).is_err());
	// 48 kHz: three bandwidth bits, then eight of lastnz - all ones is 512, past the 400 lines.
	let mut bytes = tone(30, 48_000, 40);
	bytes[39] |= 0xf8;
	bytes[38] |= 0x07;
	assert!(read(&bytes, 48_000, 10_000).is_err());
}

// THE SNS JOINT INDEX'S CHECKS: the largest index each demultiplexing accepts, and the next one refused.
#[test]
fn a_decoder_refuses_an_sns_index_past_the_codebooks() {
	let sides = |msb: bool, joint: u32| {
		let mut bytes = [0u8; 8];
		let mut w = Writer::new(&mut bytes);
		w.side_uint(3, 5);
		w.side_uint(17, 5);
		w.side_bit(msb);
		w.side_uint(1, if msb { 2 } else { 1 });
		w.side_bit(true);
		if msb {
			w.side_uint(joint, 12);
			w.side_uint(joint >> 12, 12);
		} else {
			w.side_uint(joint, 13);
			w.side_uint(joint >> 13, 12);
		}
		read_sns(&mut Side { bytes: &bytes, at: 7, mask: 1 })
	};
	let last = sides(false, 14 * SZ_REGULAR - 1).unwrap();
	assert_eq!((last.shape, last.idx_b, last.ls_b, last.idx_a, last.gain), (0, 5, 1, SZ_REGULAR - 1, 1));
	assert!(sides(false, 14 * SZ_REGULAR).is_err());
	assert!(sides(false, (1 << 25) - 1).is_err());
	let lf = sides(false, SZ_REGULAR + 7).unwrap();
	assert_eq!((lf.shape, lf.gain, lf.idx_a), (1, 3, 7));
	let near = sides(true, SZ_NEAR - 1).unwrap();
	assert_eq!((near.shape, near.gain, near.idx_a), (2, 1, SZ_NEAR - 1));
	let far = sides(true, SZ_NEAR + 2 * SZ_FAR - 1).unwrap();
	assert_eq!((far.shape, far.gain, far.idx_a), (3, 3, SZ_FAR - 1));
	assert!(sides(true, SZ_NEAR + 2 * SZ_FAR).is_err());
	assert!(sides(true, (1 << 24) - 1).is_err());
	// What the writer refuses to say.
	assert!(sns_joint(&Sns { shape: 4, ..Sns::default() }).is_err());
	assert!(sns_joint(&Sns { shape: 0, gain: 2, ..Sns::default() }).is_err());
	assert!(sns_joint(&Sns { shape: 0, idx_b: 6, ..Sns::default() }).is_err());
	assert!(sns_joint(&Sns { shape: 2, idx_a: SZ_NEAR, ..Sns::default() }).is_err());
	assert!(sns_joint(&Sns { shape: 3, idx_a: SZ_FAR, gain: 7, ..Sns::default() }).is_err());
	assert_eq!(sns_joint(&TONE_SNS), Ok(SZ_NEAR + 1));
}

// A DAMAGED FRAME IS NOTICED: a bit flipped in the arithmetic data's first bytes or in the side information's last ones
// makes the frame refused or read differently - nearly always. (The gap between the two carries nothing, and a flip
// there is rightly invisible.)
#[test]
fn a_flipped_bit_is_usually_noticed() {
	let mut frames: Vec<(Vec<u8>, u32, u32)> = ENCODED.iter().map(|encoded| (encoded.bytes.to_vec(), 16_000, encoded.frame_us)).collect();
	for (rate, size, line) in [(8_000, 20, 5), (16_000, 40, 37), (24_000, 60, 200), (32_000, 100, 1), (48_000, 155, 399), (48_000, 400, 120)] {
		frames.push((tone(line, rate, size), rate, 10_000));
	}
	let (mut flips, mut unnoticed) = (0, 0);
	for (bytes, rate, frame_us) in frames {
		let original = read_frame(&bytes, rate, frame_us).unwrap();
		let n = bytes.len();
		for at in (0..3).chain(n - 6..n) {
			for bit in 0..8 {
				let mut damaged = bytes.clone();
				damaged[at] ^= 1 << bit;
				flips += 1;
				if read_frame(&damaged, rate, frame_us).is_ok_and(|frame| frame == original) {
					unnoticed += 1;
				}
			}
		}
	}
	assert!(unnoticed * 20 <= flips, "{unnoticed} of {flips} flips unnoticed");
}

// NOTHING PANICS ON WHAT A STREAM MIGHT CARRY: arbitrary bytes at every rate and size are read or refused, and most -
// the decoder's checks are many - are refused.
#[test]
fn arbitrary_bytes_are_read_or_refused() {
	let mut seed = 0x1234_5678u32;
	let mut next = move || {
		seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
		seed >> 8
	};
	let (mut tried, mut refused) = (0, 0);
	for _ in 0..4_000 {
		let rate = RATES[next() as usize % RATES.len()];
		let frame_us = if next() % 4 == 0 { 7_500 } else { 10_000 };
		let size = 20 + next() as usize % 381;
		let bytes: Vec<u8> = (0..size).map(|_| next() as u8).collect();
		tried += 1;
		if read_frame(&bytes, rate, frame_us).is_err() {
			refused += 1;
		}
	}
	assert!(refused * 2 > tried, "{refused} of {tried} refused");
}

// ------------------------------------------------------------------ round trips

// THE WRITER AND THE READER AGREE ON EVERYTHING THE BITSTREAM CAN SAY: pseudo-random frames - large and small spectra,
// LSB mode, both TNS filters, pitch, every SNS shape, both durations - read back as they were written, residual bits up
// to the room the frame had.
#[test]
fn random_frames_round_trip() {
	let mut seed = 0x0bad_cafeu32;
	let mut next = move || {
		seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
		seed >> 8
	};
	let mut written = 0;
	for round in 0..600 {
		let rate = RATES[round % RATES.len()];
		let frame_us = if round % 5 == 4 { 7_500 } else { 10_000 };
		let size = [40, 100, 200, 400][round / RATES.len() % 4];
		let g = geometry(rate, frame_us, size).unwrap();
		let mut frame = Frame::empty();
		frame.bandwidth = (next() as usize % (g.fs_ind + 1)) as u8;
		frame.lastnz = (2 + 2 * (next() as usize % (g.ne / 2).min(24))) as u16;
		frame.lsb_mode = round % 3 == 0;
		frame.global_gain = next() as u8;
		for f in 0..if frame.bandwidth < 3 { 1 } else { 2 } {
			frame.tns_order[f] = (next() % 9) as u8;
			for k in 0..usize::from(frame.tns_order[f]) {
				frame.tns_index[f][k] = (4 + next() % 9) as u8;
			}
		}
		frame.pitch = (round % 2 == 0).then(|| Pitch { ltpf_active: next() % 2 == 0, index: (next() % 512) as u16 });
		let shape = (next() % 4) as u8;
		frame.sns = Sns { ind_lf: (next() % 32) as u8, ind_hf: (next() % 32) as u8, shape, gain: (next() % u32::from(GAIN_LEVELS[usize::from(shape)])) as u8, ls_a: (next() % 2) as u8, idx_a: next() % [SZ_REGULAR, SZ_REGULAR, SZ_NEAR, SZ_FAR][usize::from(shape)], ls_b: if shape == 0 { (next() % 2) as u8 } else { 0 }, idx_b: if shape == 0 { (next() % 6) as u8 } else { 0 } };
		frame.noise = (next() % 8) as u8;
		let lastnz = usize::from(frame.lastnz);
		for k in 0..lastnz {
			let magnitude = match next() % 8 {
				0..=3 => 0,
				4 | 5 => next() % 4,
				6 => next() % 64,
				_ => next() % 0x8000,
			} as i32;
			frame.spectrum[k] = if next() % 2 == 0 { magnitude } else { -magnitude };
		}
		// The last 2-tuple is the last non-zero one.
		if frame.spectrum[lastnz - 2] == 0 && frame.spectrum[lastnz - 1] == 0 {
			frame.spectrum[lastnz - 1] = 1;
		}
		let nonzero = frame.spectrum.iter().filter(|&&value| value != 0).count();
		frame.residual_len = nonzero as u16;
		for bit in frame.residual.iter_mut().take(nonzero) {
			*bit = next() % 2 == 1;
		}
		let mut out = vec![0u8; size];
		if write_frame(&frame, rate, frame_us, &mut out).is_err() {
			continue;
		}
		written += 1;
		let back = read_frame(&out, rate, frame_us).unwrap_or_else(|error| panic!("round {round}: {error}"));
		let mut expected = frame.clone();
		expected.room = back.room;
		if frame.lsb_mode {
			// The first bit planes of the escaped 2-tuples travel as residual data: those the frame had no room for are
			// not there to read.
			expected.residual = [false; MAX_LINES];
			expected.residual_len = 0;
			if back.spectrum != frame.spectrum {
				let lsb_count: usize = (0..lastnz).step_by(2).filter(|&k| frame.spectrum[k].unsigned_abs().max(frame.spectrum[k + 1].unsigned_abs()) >= 4).map(|k| 2 + [k, k + 1].iter().filter(|&&line| frame.spectrum[line].unsigned_abs() == 1).count()).sum();
				assert!(usize::from(back.room) < lsb_count, "round {round}: the LSBs fitted and yet differ");
				continue;
			}
		} else {
			expected.residual_len = frame.residual_len.min(back.room);
			for bit in expected.residual.iter_mut().skip(usize::from(expected.residual_len)) {
				*bit = false;
			}
		}
		assert_eq!(back, expected, "round {round}");
	}
	assert!(written > 400, "only {written} of 600 frames fitted");
}

// WHAT THE WRITER REFUSES: a spectrum past lastnz or past fourteen bit planes, a spectrum too large for the frame, an
// odd lastnz, a TNS order past 8.
#[test]
fn what_the_bitstream_cannot_say_is_not_written() {
	let mut out = [0u8; 40];
	let mut frame = Frame::empty();
	frame.spectrum[2] = 1;
	assert!(write_frame(&frame, 16_000, 10_000, &mut out).is_err());
	frame.lastnz = 4;
	assert!(write_frame(&frame, 16_000, 10_000, &mut out).is_ok());
	frame.lastnz = 5;
	assert!(write_frame(&frame, 16_000, 10_000, &mut out).is_err());
	frame.lastnz = 4;
	frame.spectrum[2] = 0x8000;
	assert!(write_frame(&frame, 16_000, 10_000, &mut out).is_err());
	frame.spectrum[2] = 1;
	frame.tns_order[0] = 9;
	assert!(write_frame(&frame, 16_000, 10_000, &mut out).is_err());
	frame.tns_order = [0, 1];
	assert!(write_frame(&frame, 16_000, 10_000, &mut out).is_err());
	frame.tns_order = [0, 0];
	frame.lastnz = 160;
	for value in frame.spectrum.iter_mut().take(160) {
		*value = 20_000;
	}
	assert_eq!(write_frame(&frame, 16_000, 10_000, &mut out), Err("the frame's content does not fit its size"));
}
