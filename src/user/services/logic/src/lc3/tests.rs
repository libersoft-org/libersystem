// End to end tests: round trips through encoder and decoder in every configuration, robustness against
// arbitrary bytes, and determinism.
//
// THE SPECIFICATION'S APPENDIX C TEST DATA IS NOT CARRIED HERE (removed 2026-10-14, the owner's decision): it was
// extracted from the specification's text, which is the Bluetooth SIG's work, and this repository is public. The
// codec's normative tables stay in `tables.rs` - an implementation cannot work without them. Conformance to another
// implementation is held by the `bluetooth-codecs` gate, which crosses this encoder and decoder with Google's liblc3
// against the published decoder thresholds.

extern crate std;
use std::string::String;
use std::vec::Vec;
use std::{format, println};

use crate::lc3::{Config, Decoder, Encoder, Error};

#[test]
fn bad_lengths_are_rejected() {
	let cfg = Config::new(48000, 10000).unwrap();
	let mut enc = Encoder::new(cfg);
	let mut dec = Decoder::new(cfg);
	let pcm = [0i16; 480];
	let mut out = [0u8; 401];
	assert_eq!(enc.encode(&pcm, &mut out), Err(Error::Bytes));
	assert_eq!(enc.encode(&pcm, &mut out[..19]), Err(Error::Bytes));
	assert_eq!(enc.encode(&pcm[..479], &mut out[..100]), Err(Error::Samples));
	let mut pcm = [0i16; 480];
	assert_eq!(dec.decode(Some(&out[..19]), &mut pcm), Err(Error::Bytes));
	assert_eq!(dec.decode(Some(&out), &mut pcm), Err(Error::Bytes));
	assert_eq!(dec.decode(None, &mut pcm[..100]), Err(Error::Samples));
	assert!(Config::new(22050, 10000).is_none());
	assert!(Config::new(48000, 5000).is_none());
}

/// The test signals of the round trip tests.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Signal {
	/// A sine of the given frequency at half of full scale.
	Sine(f64),
	/// Uniform white noise at a quarter of full scale.
	Noise,
	Silence,
	/// A full scale square wave of the given frequency.
	Square(f64),
}

pub(crate) fn signal(sig: Signal, rate: u32, len: usize) -> Vec<i16> {
	let mut seed = 0x1234_5678u32;
	(0..len)
		.map(|n| {
			let t = n as f64 / rate as f64;
			match sig {
				Signal::Sine(f) => (16384.0 * (2.0 * std::f64::consts::PI * f * t).sin()).round() as i16,
				Signal::Noise => {
					seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
					((seed >> 16) as i32 - 32768) as i16 / 4
				}
				Signal::Silence => 0,
				Signal::Square(f) => {
					if (f * t).fract() < 0.5 {
						32767
					} else {
						-32767
					}
				}
			}
		})
		.collect()
}

/// Encodes and decodes `frames` frames of a signal; returns input and output.
pub(crate) fn round_trip(cfg: Config, nbytes: usize, sig: Signal, frames: usize) -> (Vec<i16>, Vec<i16>) {
	let nf = cfg.samples();
	let input = signal(sig, cfg.sample_rate(), nf * frames);
	let mut enc = Encoder::new(cfg);
	let mut dec = Decoder::new(cfg);
	let mut output = std::vec![0i16; nf * frames];
	let mut buf = std::vec![0u8; nbytes];
	for f in 0..frames {
		enc.encode(&input[f * nf..(f + 1) * nf], &mut buf).unwrap();
		dec.decode(Some(&buf), &mut output[f * nf..(f + 1) * nf]).unwrap();
	}
	(input, output)
}

/// SNR in dB of the output against the input, compensating the codec delay and skipping the
/// first `skip` samples of the output (the start-up).
pub(crate) fn snr(cfg: &Config, input: &[i16], output: &[i16], skip: usize) -> f64 {
	let d = cfg.delay_samples();
	let mut sig = 0.0;
	let mut err = 0.0;
	for n in skip.max(d)..output.len() {
		let x = input[n - d] as f64;
		let e = output[n] as f64 - x;
		sig += x * x;
		err += e * e;
	}
	if err == 0.0 { f64::INFINITY } else { 10.0 * (sig / err).log10() }
}

/// The frequency of the largest DFT magnitude of x, on a grid of `step` Hz.
pub(crate) fn peak_frequency(x: &[i16], rate: u32, step: f64) -> f64 {
	let mut best = (0.0, 0.0);
	let mut f = step;
	while f < rate as f64 / 2.0 {
		let w = 2.0 * std::f64::consts::PI * f / rate as f64;
		let (mut re, mut im) = (0.0, 0.0);
		for (n, &v) in x.iter().enumerate() {
			// Hann window against leakage.
			let h = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / x.len() as f64).cos();
			re += h * v as f64 * (w * n as f64).cos();
			im += h * v as f64 * (w * n as f64).sin();
		}
		let m = re * re + im * im;
		if m > best.1 {
			best = (f, m);
		}
		f += step;
	}
	best.0
}

pub(crate) const RATES: [u32; 6] = [8000, 16000, 24000, 32000, 44100, 48000];
pub(crate) const DURATIONS: [u32; 2] = [10000, 7500];
pub(crate) const NBYTES: [usize; 6] = [20, 40, 60, 100, 155, 400];

/// The signals of the SNR table: three sines (1 kHz, 0.15 fs, 0.35 fs), white noise, a full
/// scale 500 Hz square wave.
fn snr_signals(rate: u32) -> [Signal; 5] {
	[Signal::Sine(1000.0), Signal::Sine(rate as f64 * 0.15), Signal::Sine(rate as f64 * 0.35), Signal::Noise, Signal::Square(500.0)]
}

/// SNR of 30 frames (the first 4 skipped) of each SNR signal for one configuration and size.
fn snr_row(cfg: Config, nbytes: usize) -> [f64; 5] {
	let mut row = [0.0; 5];
	for (i, sig) in snr_signals(cfg.sample_rate()).into_iter().enumerate() {
		let (inp, out) = round_trip(cfg, nbytes, sig, 30);
		row[i] = snr(&cfg, &inp, &out, 4 * cfg.samples());
	}
	row
}

/// Prints the SNR table in the form of SNR_TABLE (cargo test --release measure_snr --
/// --ignored --nocapture).
#[test]
#[ignore]
fn measure_snr() {
	for &rate in &RATES {
		for &us in &DURATIONS {
			let cfg = Config::new(rate, us).unwrap();
			for &nbytes in &NBYTES {
				let row = snr_row(cfg, nbytes);
				let cells: Vec<String> = row.iter().map(|v| if v.is_infinite() { std::string::String::from("INF") } else { format!("{v:.1}") }).collect();
				println!("\t({rate}, {us}, {nbytes}, [{}]),", cells.join(", "));
			}
		}
	}
}

/// The SNR in dB measured by measure_snr (release and debug builds give the same numbers): per
/// sampling rate, frame duration and frame size, for the five signals of snr_signals(). INFINITY
/// is an exact reconstruction (16-bit rounding hides the coding error).
const SNR_TABLE: [(u32, u32, usize, [f64; 5]); 72] = [
	(8000, 10000, 20, [16.1, 18.6, 24.3, 2.0, 19.9]),
	(8000, 10000, 40, [17.3, 18.7, 42.8, 14.1, 27.3]),
	(8000, 10000, 60, [21.2, 22.4, 58.4, 25.0, 30.4]),
	(8000, 10000, 100, [78.0, 77.3, 75.8, 44.5, 59.8]),
	(8000, 10000, 155, [f64::INFINITY, f64::INFINITY, f64::INFINITY, 62.0, 78.4]),
	(8000, 10000, 400, [f64::INFINITY, f64::INFINITY, f64::INFINITY, f64::INFINITY, 96.6]),
	(8000, 7500, 20, [16.2, 15.5, 35.2, 3.2, 20.9]),
	(8000, 7500, 40, [19.2, 20.5, 59.3, 19.5, 28.8]),
	(8000, 7500, 60, [74.4, 68.3, 73.3, 35.8, 52.6]),
	(8000, 7500, 100, [f64::INFINITY, 88.5, f64::INFINITY, 53.8, 76.8]),
	(8000, 7500, 155, [f64::INFINITY, f64::INFINITY, f64::INFINITY, 76.7, 92.9]),
	(8000, 7500, 400, [f64::INFINITY, f64::INFINITY, f64::INFINITY, f64::INFINITY, 92.9]),
	(16000, 10000, 20, [23.1, 14.6, 16.4, -0.8, 15.3]),
	(16000, 10000, 40, [21.4, 15.2, 17.0, 4.5, 24.9]),
	(16000, 10000, 60, [24.6, 18.5, 19.9, 10.8, 28.5]),
	(16000, 10000, 100, [74.8, 74.4, 73.9, 21.7, 43.2]),
	(16000, 10000, 155, [85.6, 79.4, 85.1, 37.0, 56.8]),
	(16000, 10000, 400, [f64::INFINITY, 103.3, 91.0, 76.4, 94.8]),
	(16000, 7500, 20, [20.9, 15.5, 16.8, -0.5, 18.5]),
	(16000, 7500, 40, [22.7, 16.8, 18.4, 6.6, 26.6]),
	(16000, 7500, 60, [72.2, 66.2, 69.9, 14.9, 34.0]),
	(16000, 7500, 100, [77.6, 77.5, 82.4, 29.7, 48.2]),
	(16000, 7500, 155, [f64::INFINITY, f64::INFINITY, 97.3, 45.6, 66.3]),
	(16000, 7500, 400, [f64::INFINITY, f64::INFINITY, 97.3, 92.9, 90.9]),
	(24000, 10000, 20, [20.1, 13.0, 17.9, -1.8, 12.9]),
	(24000, 10000, 40, [19.9, 12.7, 19.3, 1.4, 20.6]),
	(24000, 10000, 60, [21.3, 14.1, 20.6, 5.2, 25.1]),
	(24000, 10000, 100, [71.8, 65.6, 69.0, 13.3, 34.3]),
	(24000, 10000, 155, [84.7, 77.1, 78.7, 23.0, 44.5]),
	(24000, 10000, 400, [f64::INFINITY, f64::INFINITY, 93.9, 56.1, 79.4]),
	(24000, 7500, 20, [19.2, 11.9, 19.1, -1.6, 12.9]),
	(24000, 7500, 40, [19.6, 12.7, 19.8, 2.6, 21.2]),
	(24000, 7500, 60, [25.3, 17.6, 24.3, 7.6, 27.8]),
	(24000, 7500, 100, [77.6, 69.9, 72.5, 18.1, 36.8]),
	(24000, 7500, 155, [100.8, 86.0, 83.5, 31.6, 51.9]),
	(24000, 7500, 400, [f64::INFINITY, f64::INFINITY, 88.3, 69.5, 86.5]),
	(32000, 10000, 20, [19.6, 4.8, -1.3, -1.7, 11.2]),
	(32000, 10000, 40, [19.3, 12.3, 24.1, -0.2, 19.1]),
	(32000, 10000, 60, [18.9, 12.3, 24.2, 2.4, 24.6]),
	(32000, 10000, 100, [68.0, 68.6, 68.1, 8.7, 32.0]),
	(32000, 10000, 155, [78.3, 79.7, 78.7, 16.3, 38.7]),
	(32000, 10000, 400, [91.6, 90.9, 85.0, 45.9, 67.7]),
	(32000, 7500, 20, [21.0, 11.8, 6.2, -2.0, 11.8]),
	(32000, 7500, 40, [18.9, 12.4, 24.2, 0.6, 23.3]),
	(32000, 7500, 60, [22.4, 15.3, 26.9, 4.1, 27.9]),
	(32000, 7500, 100, [80.9, 77.2, 72.9, 12.5, 30.2]),
	(32000, 7500, 155, [89.6, 86.3, 84.2, 22.5, 44.2]),
	(32000, 7500, 400, [f64::INFINITY, 99.1, 87.7, 56.1, 79.8]),
	(44100, 10000, 20, [13.4, 8.7, -4.1, -2.4, 7.5]),
	(44100, 10000, 40, [16.7, 14.6, 27.8, -0.9, 14.5]),
	(44100, 10000, 60, [16.7, 14.6, 28.6, 0.8, 17.6]),
	(44100, 10000, 100, [22.2, 19.1, 33.1, 4.0, 21.4]),
	(44100, 10000, 155, [76.1, 77.3, 71.0, 6.7, 24.3]),
	(44100, 10000, 400, [92.6, 84.6, 74.8, 7.8, 26.0]),
	(44100, 7500, 20, [15.3, 10.4, -2.5, -2.1, 7.9]),
	(44100, 7500, 40, [16.7, 14.6, 29.0, -0.4, 16.4]),
	(44100, 7500, 60, [18.3, 15.9, 30.3, 1.4, 18.7]),
	(44100, 7500, 100, [71.2, 72.5, 73.0, 5.3, 22.7]),
	(44100, 7500, 155, [80.5, 79.7, 77.7, 7.3, 25.3]),
	(44100, 7500, 400, [87.9, 84.1, 78.1, 7.8, 26.0]),
	(48000, 10000, 20, [20.9, 8.7, -4.1, -2.4, 7.7]),
	(48000, 10000, 40, [19.1, 14.6, 27.8, -0.9, 15.9]),
	(48000, 10000, 60, [19.1, 14.6, 28.6, 0.8, 21.4]),
	(48000, 10000, 100, [24.8, 19.1, 33.1, 4.0, 23.7]),
	(48000, 10000, 155, [85.0, 77.3, 71.0, 6.7, 26.4]),
	(48000, 10000, 400, [103.3, 84.6, 74.8, 7.8, 26.8]),
	(48000, 7500, 20, [18.0, 10.4, -2.5, -2.1, 8.1]),
	(48000, 7500, 40, [19.4, 14.6, 29.0, -0.4, 18.5]),
	(48000, 7500, 60, [20.9, 15.9, 30.3, 1.4, 21.8]),
	(48000, 7500, 100, [77.6, 72.5, 73.0, 5.3, 24.6]),
	(48000, 7500, 155, [89.6, 79.7, 77.7, 7.3, 26.3]),
	(48000, 7500, 400, [92.9, 84.1, 78.1, 7.8, 26.8]),
];

/// Every configuration at 20, 40, 60, 100, 155 and 400 bytes: the decoded signal, delay
/// compensated, has at least the measured SNR minus 2 dB (an exact reconstruction must stay
/// above 88 dB). The numbers explain themselves once two things are known: the long term
/// postfilter reshapes pure tones at the bit rates where it is on (up to about 70 to 110 bytes
/// depending on the rate), which caps a tone's waveform SNR there at 10 to 25 dB, and noise
/// filling replaces the uncoded part of a noise signal by other noise; above 32 kHz the codec
/// stops at 20 kHz (18.4 kHz at 44.1 kHz), so white noise loses a sixth of its energy and can
/// not exceed about 7.8 dB.
#[test]
fn round_trip_snr() {
	let mut checked = 0;
	for &(rate, us, nbytes, want) in &SNR_TABLE {
		let cfg = Config::new(rate, us).unwrap();
		let got = snr_row(cfg, nbytes);
		for i in 0..5 {
			let floor = want[i].min(90.0) - 2.0;
			assert!(got[i] >= floor, "{rate} Hz {us} us {nbytes} bytes, {:?}: {:.1} dB, expected at least {floor:.1}", snr_signals(rate)[i], got[i]);
			checked += 1;
		}
	}
	assert_eq!(checked, RATES.len() * DURATIONS.len() * NBYTES.len() * 5);
}

/// Silence codes to exact digital silence in every configuration and frame size.
#[test]
fn silence_stays_silent() {
	for &rate in &RATES {
		for &us in &DURATIONS {
			let cfg = Config::new(rate, us).unwrap();
			for &nbytes in &NBYTES {
				let (_, o) = round_trip(cfg, nbytes, Signal::Silence, 4);
				assert!(o.iter().all(|&v| v == 0), "{rate} {us} {nbytes}");
			}
		}
	}
}

/// The decoded tone has the input's frequency as its loudest component.
#[test]
fn tone_frequency_survives() {
	for &rate in &RATES {
		for &us in &DURATIONS {
			let cfg = Config::new(rate, us).unwrap();
			for &nbytes in &[20, 60, 400] {
				for f in [1000.0, rate as f64 * 0.15] {
					let (_, o) = round_trip(cfg, nbytes, Signal::Sine(f), 12);
					let tail = &o[o.len() - 4 * cfg.samples()..];
					let p = peak_frequency(tail, rate, 25.0);
					assert!((p - f).abs() <= 25.0, "{rate} {us} {nbytes}: {f} Hz came out as {p} Hz");
				}
			}
		}
	}
}

/// A number as the specification prints it.
fn spec(text: &str) -> f64 {
	text.parse().unwrap()
}

/// Table sizes, spot values copied from the specification text, and the per configuration
/// band and window tables.
#[test]
fn tables_match_the_specification() {
	use crate::lc3::tables::*;
	// Spot values, as printed in section 3.7.
	assert_eq!(W_10_80[0], spec("-7.078546706512391e-04"));
	assert_eq!(W_10_160[0], spec("-4.619898752628163e-04"));
	assert_eq!(W_10_240[0], spec("-3.613496418928369e-04"));
	assert_eq!(W_10_320[0], spec("-3.021153494057143e-04"));
	assert_eq!(W_10_480[0], spec("-2.353032150516754e-04"));
	assert_eq!(W_7_5_60[0], spec("2.950608593187313e-03"));
	assert_eq!(W_7_5_120[0], spec("2.208248743046650e-03"));
	assert_eq!(W_7_5_180[0], spec("1.970849076512990e-03"));
	assert_eq!(W_7_5_240[0], spec("1.848330370601890e-03"));
	assert_eq!(W_7_5_360[0], spec("1.721526681611966e-03"));
	assert_eq!(LFCB[0][0], spec("2.262833655926780e+00"));
	assert_eq!(LFCB[31][7], spec("1.272326725547010e+00"));
	assert_eq!(HFCB[0][0], spec("2.320284191244650e-01"));
	assert_eq!(D[0][1], spec("3.518509343815957e-01"));
	assert_eq!(D[15][15], spec("-3.465429229977293e-02"));
	assert_eq!(SNS_VQ_FAR_ADJ_GAINS[7], 19882.0 / 4096.0);
	assert_eq!(SNS_GAIN_MSB_BITS, [1, 1, 2, 2]);
	assert_eq!(SNS_GAIN_LSB_BITS, [0, 1, 0, 1]);
	assert_eq!(MPVQ_OFFSETS[15][10], 89129247);
	assert_eq!(MPVQ_OFFSETS[2][3], 13);
	assert_eq!(AC_SPEC_LOOKUP[..8], [0x01, 0x27, 0x07, 0x19, 0x16, 0x16, 0x1C, 0x16]);
	assert_eq!(AC_TNS_COEF_BITS[7], [20480, 20480, 20480, 20480, 20480, 20480, 15725, 3658, 20480, 1201, 10854, 18432, 20480, 20480, 20480, 20480, 20480]);
	assert_eq!(AC_TNS_ORDER_CUMFREQ[1], [0, 14, 56, 156, 313, 494, 672, 839]);
	assert_eq!(TAB_RESAMP_FILTER[0], spec("-2.043055832879108e-05"));
	assert_eq!(TAB_RESAMP_FILTER[238], spec("-2.043055832879108e-05"));
	assert_eq!(TAB_LTPF_INTERP_R[2], spec("2.745471654059321e-03"));
	assert_eq!(TAB_LTPF_INTERP_R[3], spec("1.535727698935322e-02"));
	assert_eq!(TAB_LTPF_INTERP_X12K8[5], spec("4.592209296082350e-01"));
	assert_eq!(TAB_LTPF_INTERP_X12K8[7], spec("5.835275754221211e-01"));
	assert_eq!(TAB_LTPF_NUM_8000[0][0], spec("6.023618207009578e-01"));
	assert_eq!(TAB_LTPF_DEN_48000[1][1], spec("7.041404930459358e-03"));
	// The FIR tables are symmetric.
	for i in 0..239 {
		assert_eq!(TAB_RESAMP_FILTER[i], TAB_RESAMP_FILTER[238 - i]);
	}
	for i in 0..31 {
		assert_eq!(TAB_LTPF_INTERP_R[i], TAB_LTPF_INTERP_R[30 - i]);
	}
	// The arithmetic coding models: cumulative frequencies are the prefix sums of the
	// frequencies and every model spans 1024, and the bit costs are -log2(p) in 1/2048 bits.
	for m in 0..64 {
		for s in 0..16 {
			assert_eq!(AC_SPEC_CUMFREQ[m][s + 1], AC_SPEC_CUMFREQ[m][s] + AC_SPEC_FREQ[m][s]);
		}
		assert_eq!(AC_SPEC_CUMFREQ[m][16] + AC_SPEC_FREQ[m][16], 1024);
		for s in 0..17 {
			let est = -(AC_SPEC_FREQ[m][s] as f64 / 1024.0).log2() * 2048.0;
			assert!((AC_SPEC_BITS[m][s] as f64 - est).abs() <= 1.0, "model {m} symbol {s}");
		}
	}
	for m in 0..8 {
		assert_eq!(AC_TNS_COEF_CUMFREQ[m][16] + AC_TNS_COEF_FREQ[m][16], 1024);
	}
	assert!(AC_SPEC_LOOKUP.iter().all(|&v| v < 64));
	// Per configuration: band limits rise from 0 to NE, the window has 2 NF values ending in Z
	// zeros, and the derived sizes are those of the specification.
	let expect = [
		(8000, 10000, 80, 80, 64, 30),
		(16000, 10000, 160, 160, 64, 60),
		(24000, 10000, 240, 240, 64, 90),
		(32000, 10000, 320, 320, 64, 120),
		(44100, 10000, 480, 400, 64, 180),
		(48000, 10000, 480, 400, 64, 180),
		(8000, 7500, 60, 60, 60, 14),
		(16000, 7500, 120, 120, 64, 28),
		(24000, 7500, 180, 180, 64, 42),
		(32000, 7500, 240, 240, 64, 56),
		(44100, 7500, 360, 300, 64, 84),
		(48000, 7500, 360, 300, 64, 84),
	];
	for (rate, us, nf, ne, nb, z) in expect {
		let cfg = Config::new(rate, us).unwrap();
		assert_eq!((cfg.nf, cfg.ne, cfg.nb, cfg.z), (nf, ne, nb, z), "{rate} {us}");
		assert_eq!(cfg.samples(), nf);
		assert_eq!(cfg.bands.len(), nb + 1);
		assert_eq!(cfg.bands[0], 0);
		assert_eq!(cfg.bands[nb] as usize, ne);
		assert!(cfg.bands.windows(2).all(|w| w[0] < w[1]));
		assert_eq!(cfg.window.len(), 2 * nf);
		assert!(cfg.window[2 * nf - z..].iter().all(|&v| v == 0.0));
		assert!(cfg.window[2 * nf - z - 1] != 0.0);
		// Section 3.2.4: 2.5 ms lookahead at 10 ms, 4 ms at 7.5 ms (both stretched at 44.1 kHz).
		let ms = cfg.delay_samples() as f64 * 1000.0 / if rate == 44100 { 48000.0 } else { rate as f64 };
		assert_eq!(ms, if us == 10000 { 2.5 } else { 4.0 });
	}
}

/// MDCT analysis, synthesis and overlap-add without quantization give back the input, delayed
/// by delay_samples(): the windows and the transforms are consistent (perfect reconstruction).
#[test]
fn mdct_perfect_reconstruction() {
	use crate::lc3::mdct::Mdct;
	for &rate in &[8000, 16000, 24000, 32000, 48000] {
		for &us in &DURATIONS {
			let cfg = Config::new(rate, us).unwrap();
			let (nf, z) = (cfg.nf, cfg.z);
			let mut mdct = Mdct::new(nf, cfg.window);
			let input = signal(Signal::Noise, rate, 8 * nf);
			let mut hist = std::vec![0.0; nf - z];
			let mut ola = std::vec![0.0; nf - z];
			let mut out = Vec::new();
			for f in 0..8 {
				let mut t = std::vec![0.0; 2 * nf];
				t[..nf - z].copy_from_slice(&hist);
				for n in 0..nf {
					t[nf - z + n] = input[f * nf + n] as f64;
				}
				hist.copy_from_slice(&t[nf..2 * nf - z]);
				for v in &mut t[2 * nf - z..] {
					*v = 0.0;
				}
				let mut x = std::vec![0.0; nf];
				mdct.forward(&t, &mut x);
				let mut y = std::vec![0.0; 2 * nf];
				mdct.inverse(&x, &mut y);
				for n in 0..nf - z {
					out.push(ola[n] + y[z + n]);
				}
				out.extend_from_slice(&y[nf..nf + z]);
				ola.copy_from_slice(&y[nf + z..]);
			}
			let d = cfg.delay_samples();
			for n in 2 * nf..out.len() {
				assert!((out[n] - input[n - d] as f64).abs() < 1.0e-6, "{rate} {us} sample {n}: {} vs {}", out[n], input[n - d]);
			}
		}
	}
}

/// A simple LCG for the robustness tests.
struct Lcg(u32);

impl Lcg {
	fn next(&mut self) -> u32 {
		self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
		self.0 >> 8
	}
	fn below(&mut self, n: u32) -> u32 {
		self.next() % n
	}
}

/// Signals that push the bit budget: full scale white noise, isolated full scale clicks, an
/// alternating Nyquist tone, a fast chirp.
fn hard_signal(kind: usize, rate: u32, len: usize, rng: &mut Lcg) -> Vec<i16> {
	(0..len)
		.map(|n| match kind {
			0 => (rng.next() >> 8) as u16 as i16,
			1 => {
				if n % 97 == 0 {
					32767
				} else if n % 89 == 0 {
					-32768
				} else {
					0
				}
			}
			2 => {
				if n % 2 == 0 {
					32767
				} else {
					-32767
				}
			}
			_ => {
				let t = n as f64 / rate as f64;
				(30000.0 * (2.0 * std::f64::consts::PI * (100.0 * t + rate as f64 * t * t)).sin()) as i16
			}
		})
		.collect()
}

/// Whatever the signal and the frame size, even when the size changes every frame, the encoder
/// writes frames the decoder accepts: the side information, the spectrum and the residual bits
/// never collide.
#[test]
fn encoder_output_always_decodes() {
	let mut rng = Lcg(77);
	for &rate in &RATES {
		for &us in &DURATIONS {
			let cfg = Config::new(rate, us).unwrap();
			let nf = cfg.samples();
			for kind in 0..4 {
				let frames = 12;
				let input = hard_signal(kind, rate, nf * frames, &mut rng);
				for sizes in [[20usize, 21, 23, 27], [33, 47, 61, 80], [101, 133, 155, 200], [271, 333, 399, 400], [0, 0, 0, 0]] {
					let mut enc = Encoder::new(cfg);
					let mut dec = Decoder::new(cfg);
					let mut pcm = std::vec![0i16; nf];
					for f in 0..frames {
						// The last set picks a random size for every frame.
						let nbytes = if sizes[0] == 0 { 20 + rng.below(381) as usize } else { sizes[f % 4] };
						let mut buf = std::vec![0u8; nbytes];
						enc.encode(&input[f * nf..(f + 1) * nf], &mut buf).unwrap();
						assert_eq!(dec.decode(Some(&buf), &mut pcm), Ok(()), "{rate} {us} signal {kind} frame {f} {nbytes} bytes");
					}
				}
			}
		}
	}
}

/// Arbitrary bytes never make the decoder panic: random frames of random sizes, and valid
/// frames with a few bits flipped, a few thousand per configuration through one decoder (so
/// the state after bad frames is exercised too). Every result is Ok or Corrupt.
#[test]
fn decoder_survives_arbitrary_bytes() {
	let mut rng = Lcg(2024);
	let mut corrupt = 0usize;
	let mut total = 0usize;
	for &rate in &RATES {
		for &us in &DURATIONS {
			let cfg = Config::new(rate, us).unwrap();
			let nf = cfg.samples();
			let mut dec = Decoder::new(cfg);
			let mut pcm = std::vec![0i16; nf];
			for _ in 0..2000 {
				let nbytes = 20 + rng.below(381) as usize;
				let buf: Vec<u8> = (0..nbytes).map(|_| rng.next() as u8).collect();
				let r = dec.decode(Some(&buf), &mut pcm);
				assert!(r == Ok(()) || r == Err(Error::Corrupt));
				corrupt += (r == Err(Error::Corrupt)) as usize;
				total += 1;
			}
			// Valid frames of a noisy signal, then damaged.
			let input = hard_signal(0, rate, nf * 4, &mut rng);
			let mut enc = Encoder::new(cfg);
			for i in 0..1000 {
				let nbytes = [20, 40, 100, 400][i % 4];
				let mut buf = std::vec![0u8; nbytes];
				enc.encode(&input[(i % 4) * nf..(i % 4 + 1) * nf], &mut buf).unwrap();
				for _ in 0..1 + rng.below(8) {
					let bit = rng.below(8 * nbytes as u32) as usize;
					buf[bit / 8] ^= 1 << (bit % 8);
				}
				let r = dec.decode(Some(&buf), &mut pcm);
				assert!(r == Ok(()) || r == Err(Error::Corrupt));
				corrupt += (r == Err(Error::Corrupt)) as usize;
				total += 1;
			}
		}
	}
	println!("{corrupt} of {total} damaged or random frames were rejected as corrupt");
	assert!(corrupt > total / 4);
}

/// The same input gives the same bytes, and the same bytes the same samples.
#[test]
fn bitstream_is_deterministic() {
	for &rate in &RATES {
		for &us in &DURATIONS {
			let cfg = Config::new(rate, us).unwrap();
			let nf = cfg.samples();
			let input = signal(Signal::Sine(rate as f64 * 0.11), rate, nf * 6);
			let mut a = (Encoder::new(cfg), Decoder::new(cfg));
			let mut b = (Encoder::new(cfg), Decoder::new(cfg));
			for f in 0..6 {
				let (mut ba, mut bb) = ([0u8; 77], [0u8; 77]);
				a.0.encode(&input[f * nf..(f + 1) * nf], &mut ba).unwrap();
				b.0.encode(&input[f * nf..(f + 1) * nf], &mut bb).unwrap();
				assert_eq!(ba, bb);
				let (mut pa, mut pb) = (std::vec![0i16; nf], std::vec![0i16; nf]);
				a.1.decode(Some(&ba), &mut pa).unwrap();
				b.1.decode(Some(&bb), &mut pb).unwrap();
				assert_eq!(pa, pb);
			}
		}
	}
}

/// Concealment: lost frames continue the signal at first and fade out over a long loss, a
/// corrupt frame is concealed exactly like a lost one, and decoding recovers afterwards.
#[test]
fn concealment() {
	let energy = |x: &[i16]| x.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / x.len() as f64;
	for &rate in &[8000, 16000, 48000] {
		for &us in &DURATIONS {
			let cfg = Config::new(rate, us).unwrap();
			let nf = cfg.samples();
			let input = signal(Signal::Sine(440.0), rate, nf * 60);
			let mut enc = Encoder::new(cfg);
			let mut dec = Decoder::new(cfg);
			let mut frames = Vec::new();
			for f in 0..60 {
				let mut buf = std::vec![0u8; 60];
				enc.encode(&input[f * nf..(f + 1) * nf], &mut buf).unwrap();
				frames.push(buf);
			}
			let mut pcm = std::vec![0i16; nf];
			// Before any good frame the concealment is silence.
			let mut fresh = Decoder::new(cfg);
			fresh.decode(None, &mut pcm).unwrap();
			assert!(pcm.iter().all(|&v| v == 0));
			for frame in &frames[..20] {
				dec.decode(Some(frame), &mut pcm).unwrap();
			}
			let good = energy(&pcm);
			// A corrupt frame and a lost frame give the same concealment.
			let mut twin = dec.clone();
			let mut pcm2 = std::vec![0i16; nf];
			let garbage = [0xffu8; 60];
			assert_eq!(dec.decode(Some(&garbage), &mut pcm), Err(Error::Corrupt));
			twin.decode(None, &mut pcm2).unwrap();
			assert_eq!(pcm, pcm2);
			assert!(energy(&pcm) > 0.1 * good, "{rate} {us}: first concealed frame too quiet");
			// A long loss fades out.
			let mut last = 0.0;
			for _ in 1..40 {
				dec.decode(None, &mut pcm).unwrap();
				last = energy(&pcm);
			}
			assert!(last < 0.01 * good, "{rate} {us}: no fade out ({last} vs {good})");
			// Recovery: a few frames later the output follows the input again.
			let mut out = std::vec![0i16; nf * 20];
			for f in 40..60 {
				dec.decode(Some(&frames[f]), &mut out[(f - 40) * nf..(f - 39) * nf]).unwrap();
			}
			let d = cfg.delay_samples();
			let tail = 10 * nf;
			let mut err = 0.0;
			let mut sig = 0.0;
			for n in tail..20 * nf {
				let x = input[40 * nf + n - d] as f64;
				err += (out[n] as f64 - x) * (out[n] as f64 - x);
				sig += x * x;
			}
			assert!(10.0 * (sig / err).log10() > 10.0, "{rate} {us}: no recovery");
		}
	}
}
