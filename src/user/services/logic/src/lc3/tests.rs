// End to end tests: the specification's appendix C reference data, round trips through encoder and
// decoder in every configuration, robustness against arbitrary bytes, and determinism.
//
// The appendix C data (lc3/appendix_c.txt, extracted mechanically from the specification's
// text, one record per line: section, frame, name, count, values) is a 16 kHz sine coded at 32 kbit/s, two frames of 10 ms (40 bytes) and two of
// 7.5 ms (30 bytes), with the intermediate values of every encoder and decoder stage, plus extra
// data for the attack detector (48 kHz), a two filter TNS frame and the four LTPF transition cases.

extern crate std;
use std::string::String;
use std::vec::Vec;
use std::{format, println};

use crate::lc3::{Config, Decoder, Encoder, Error};

static APPENDIX_C: &str = include_str!("appendix_c.txt");

/// The values of one appendix C record, as written.
pub(crate) fn record(section: &str, frame: u32, name: &str) -> Vec<String> {
	for line in APPENDIX_C.lines() {
		let mut it = line.split(' ');
		if it.next() == Some(section) && it.next() == Some(&format!("{frame}")) && it.next() == Some(name) {
			it.next();
			return it.map(String::from).collect();
		}
	}
	panic!("no appendix C record {section} {frame} {name}");
}

pub(crate) fn ints(section: &str, frame: u32, name: &str) -> Vec<i64> {
	record(section, frame, name).iter().map(|v| v.parse().unwrap()).collect()
}

pub(crate) fn int(section: &str, frame: u32, name: &str) -> i64 {
	ints(section, frame, name)[0]
}

/// Doubles written as 16 hex digits.
pub(crate) fn doubles(section: &str, frame: u32, name: &str) -> Vec<f64> {
	record(section, frame, name).iter().map(|v| f64::from_bits(u64::from_str_radix(v, 16).unwrap())).collect()
}

/// Encodes the appendix C input and compares every byte with the reference frames.
#[test]
fn appendix_c_encoder_frames() {
	for (section, us, nbytes) in [("enc10", 10000, 40), ("enc7_5", 7500, 30)] {
		let mut enc = Encoder::new(Config::new(16000, us).unwrap());
		for frame in [3, 4] {
			let pcm: Vec<i16> = ints(section, frame, "x_s").iter().map(|&v| v as i16).collect();
			let mut out = std::vec![0u8; nbytes];
			enc.encode(&pcm, &mut out).unwrap();
			let want: Vec<u8> = ints(section, frame, "bytes_ari").iter().map(|&v| v as u8).collect();
			assert_eq!(out, want, "{section} frame {frame}");
		}
	}
}

/// Decodes the reference frames and compares the output with the reference x^_clip.
#[test]
fn appendix_c_decoder_output() {
	for (dsection, esection, us) in [("dec10", "enc10", 10000), ("dec7_5", "enc7_5", 7500)] {
		let cfg = Config::new(16000, us).unwrap();
		let mut dec = Decoder::new(cfg);
		for frame in [1, 2] {
			let bytes: Vec<u8> = ints(esection, frame + 2, "bytes_ari").iter().map(|&v| v as u8).collect();
			let mut pcm = std::vec![0i16; cfg.samples()];
			dec.decode(Some(&bytes), &mut pcm).unwrap();
			let want = doubles(dsection, frame, "x_hat_clip");
			let mut worst = 0.0f64;
			for (n, (&got, &w)) in pcm.iter().zip(&want).enumerate() {
				// x^_clip is before the final rounding: the output is its nearest integer.
				let d = (got as f64 - w).abs();
				worst = worst.max(d);
				assert!(d <= 0.5 + 1.0e-6, "{dsection} frame {frame} sample {n}: {got} vs {w}");
			}
			println!("{dsection} frame {frame}: largest deviation from x_hat_clip {worst}");
		}
	}
}

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

fn assert_close(what: &str, got: &[f64], want: &[f64], rel: f64) {
	assert_eq!(got.len(), want.len(), "{what}: length");
	let scale = want.iter().fold(1.0e-300f64, |m, v| m.max(v.abs()));
	for (i, (g, w)) in got.iter().zip(want).enumerate() {
		assert!((g - w).abs() <= rel * scale, "{what}[{i}]: {g} vs {w} (scale {scale})");
	}
}

/// Each encoder stage, fed with the reference input of that stage, reproduces the reference
/// output: the stages are checked one by one rather than only through the final bytes.
#[test]
fn appendix_c_encoder_stages() {
	use crate::lc3::encoder;
	use crate::lc3::ltpf::LtpfEncoder;
	use crate::lc3::mdct::Mdct;
	use crate::lc3::{sns, tns};
	for (section, us, nbytes) in [("enc10", 10000, 40usize), ("enc7_5", 7500, 30)] {
		let cfg = Config::new(16000, us).unwrap();
		let (nf, ne, z) = (cfg.nf, cfg.ne, cfg.z);
		let nbits = 8 * nbytes;
		let mut mdct = Mdct::new(nf, cfg.window);
		let mut xbuf = std::vec![0.0; 2 * nf];
		let mut ltpf = LtpfEncoder::new();
		let mut offset = 0.0;
		for frame in [3u32, 4] {
			let s = section;
			let pcm = ints(s, frame, "x_s");
			for n in 0..nf {
				xbuf[nf - z + n] = pcm[n] as f64;
			}
			// 3.3.4 MDCT and band energies.
			let mut t = std::vec![0.0; 2 * nf];
			t[..2 * nf - z].copy_from_slice(&xbuf[..2 * nf - z]);
			let mut x = std::vec![0.0; nf];
			mdct.forward(&t, &mut x);
			let x_ref = doubles(s, frame, "X");
			assert_close(&format!("{s} {frame} X"), &x, &x_ref, 1.0e-12);
			let mut eb = [0.0; 64];
			for (e, band) in eb.iter_mut().zip(cfg.bands.windows(2)) {
				let (lo, hi) = (band[0] as usize, band[1] as usize);
				*e = x_ref[lo..hi].iter().map(|v| v * v).sum::<f64>() / (hi - lo) as f64;
			}
			let eb_ref = doubles(s, frame, "E_B");
			assert_close(&format!("{s} {frame} E_B"), &eb[..cfg.nb], &eb_ref[..cfg.nb], 1.0e-12);
			let mut eb = [0.0; 64];
			eb[..cfg.nb].copy_from_slice(&eb_ref[..cfg.nb]);
			// 3.3.5 Bandwidth.
			let pbw = encoder::bandwidth(&cfg, &eb);
			assert_eq!(pbw as i64, int(s, frame, "P_bw"));
			assert!(!encoder::near_nyquist(&cfg, &eb));
			// 3.3.7 SNS.
			let scf = sns::analyze(&cfg, &eb, false);
			let scf_ref = doubles(s, frame, "scf");
			assert_close(&format!("{s} {frame} scf"), &scf, &scf_ref, 1.0e-12);
			let mut scf_in = [0.0; 16];
			scf_in.copy_from_slice(&scf_ref);
			let (q, scfq) = sns::quantize(&scf_in);
			assert_eq!(q.ind_lf as i64, int(s, frame, "ind_LF"), "{s} {frame}");
			assert_eq!(q.ind_hf as i64, int(s, frame, "ind_HF"), "{s} {frame}");
			assert_eq!((q.shape >> 1) as i64, int(s, frame, "submodeMSB"), "{s} {frame}");
			assert_eq!(q.gain as i64, int(s, frame, "Gind"), "{s} {frame}");
			assert_eq!(q.ls_a as i64, int(s, frame, "LS_indA"), "{s} {frame}");
			assert_eq!(q.idx_a as i64, int(s, frame, "idxA"), "{s} {frame}");
			if q.shape == 0 {
				assert_eq!(q.idx_b as i64, int(s, frame, "idxB"), "{s} {frame}");
			}
			let scfq_ref = doubles(s, frame, "scfQ");
			assert_close(&format!("{s} {frame} scfQ"), &scfq, &scfq_ref, 1.0e-12);
			let mut scfq_in = [0.0; 16];
			scfq_in.copy_from_slice(&scfq_ref);
			let g = sns::interpolate(&cfg, &scfq_in, true);
			let g_ref = doubles(s, frame, "g_sns");
			assert_close(&format!("{s} {frame} g_sns"), &g[..cfg.nb], &g_ref[..cfg.nb], 1.0e-12);
			let mut xs = x_ref.clone();
			for b in 0..cfg.nb {
				for v in &mut xs[cfg.bands[b] as usize..cfg.bands[b + 1] as usize] {
					*v *= g_ref[b];
				}
			}
			let xs_ref = doubles(s, frame, "X_S");
			assert_close(&format!("{s} {frame} X_S"), &xs, &xs_ref, 1.0e-12);
			// 3.3.8 TNS.
			let tns = tns::analyze(&cfg, &xs_ref, pbw, nbits, false);
			assert_eq!(tns.order[0] as i64, ints(s, frame, "rc_order")[0], "{s} {frame}");
			let rci: Vec<i64> = tns.rc_i[0].iter().map(|&v| v as i64).collect();
			assert_eq!(rci, ints(s, frame, "rc_i_1"), "{s} {frame}");
			assert_eq!(tns::nbits(&tns) as i64, int(s, frame, "nbits_TNS"), "{s} {frame}");
			let mut xf = xs_ref.clone();
			tns::filter(&cfg, &mut xf, pbw, &tns);
			let xf_ref = doubles(s, frame, "X_f");
			assert_close(&format!("{s} {frame} X_f"), &xf, &xf_ref, 1.0e-12);
			// 3.3.9 LTPF.
			let lt = ltpf.analyze(&cfg, &xbuf, nf - z, nbits, false);
			let xd_ref = doubles(s, frame, "x_tilde_12.8D");
			let xd: Vec<f64> = (0..xd_ref.len()).map(|n| ltpf.delayed(&cfg, n)).collect();
			assert_close(&format!("{s} {frame} x_tilde_12.8D"), &xd, &xd_ref, 1.0e-12);
			assert_eq!(lt.pitch_present as i64, int(s, frame, "pitch_present"), "{s} {frame}");
			assert_eq!(lt.pitch_index as i64, int(s, frame, "pitch_index"), "{s} {frame}");
			assert_eq!(lt.active as i64, int(s, frame, "ltpf_active"), "{s} {frame}");
			// 3.3.10 Spectral quantization.
			let nbits_ari = cfg.nbits_lastnz() as i64 + 3;
			let nbits_spec = nbits as i64 - cfg.nbits_bw() as i64 - tns::nbits(&tns) as i64 - if lt.pitch_present { 11 } else { 1 } - 38 - 8 - 3 - nbits_ari;
			assert_eq!(nbits_spec, int(s, frame, "nbits_spec"), "{s} {frame}");
			let off_ref: f64 = record(s, frame, "nbits_offset")[0].parse().unwrap();
			assert!((offset - off_ref).abs() < 1.0e-6, "{s} {frame} offset {offset} vs {off_ref}");
			let gg_off = cfg.gg_off(nbits);
			assert_eq!(gg_off as i64, int(s, frame, "gg_off"));
			let nbits_spec2 = crate::lc3::math::nint(nbits_spec as f64 + offset);
			let gg_ind = encoder::estimate_gain(&xf_ref, ne, nbits_spec2, gg_off);
			assert_eq!(gg_ind as i64, int(s, frame, "gg_ind"), "{s} {frame}");
			let xf_max = xf_ref.iter().fold(0.0f64, |m, v| m.max(v.abs()));
			let gg_min = crate::lc3::math::ceil(28.0 * crate::lc3::math::log10(1.0e-31 + xf_max / (32768.0 - 0.375))) as i32 - gg_off;
			assert_eq!(gg_min as i64, int(s, frame, "gg_min"), "{s} {frame}");
			let q = encoder::quantize(&cfg, &xf_ref, gg_ind, gg_off, nbits, nbits_spec as i32);
			let xq: Vec<i64> = q.xq[..ne].iter().map(|&v| v as i64).collect();
			// X_q is listed before truncation: compare up to the truncation point and the rest
			// through lastnz and nbits_est.
			let xq_ref = ints(s, frame, "X_q");
			assert_eq!(&xq[..q.lastnz_trunc], &xq_ref[..q.lastnz_trunc], "{s} {frame} X_q");
			assert_eq!(q.lastnz as i64, int(s, frame, "lastnz"), "{s} {frame}");
			assert_eq!(q.nbits_est as i64, int(s, frame, "nbits_est"), "{s} {frame}");
			assert_eq!(q.lsb_mode as i64, int(s, frame, "lsbMode"), "{s} {frame}");
			offset = 0.8 * offset + 0.2 * (offset + (nbits_spec - q.nbits_est as i64) as f64).clamp(-40.0, 40.0);
			let delta = encoder::gain_delta(cfg.fs_ind, q.nbits_est);
			let nbits_spec = nbits_spec as i32;
			let mut gg = gg_ind;
			let adjust = (gg < 255 && q.nbits_est > nbits_spec) || (gg > 0 && q.nbits_est < nbits_spec - (delta + 2));
			if adjust {
				if q.nbits_est < nbits_spec - (delta + 2) {
					gg -= 1;
				} else if gg == 254 || q.nbits_est < nbits_spec + delta {
					gg += 1;
				} else {
					gg += 2;
				}
				gg = gg.max(gg_min);
			}
			assert_eq!(gg as i64, int(s, frame, "gg_ind_adj"), "{s} {frame}");
			let q = if adjust { encoder::quantize(&cfg, &xf_ref, gg, gg_off, nbits, nbits_spec) } else { q };
			if adjust {
				let xq: Vec<i64> = q.xq[..ne].iter().map(|&v| v as i64).collect();
				let xq_ref = ints(s, frame, "X_q_req");
				assert_eq!(&xq[..q.lastnz_trunc], &xq_ref[..q.lastnz_trunc], "{s} {frame} X_q_req");
				assert_eq!(q.lastnz_trunc as i64, int(s, frame, "lastnz_req"), "{s} {frame}");
				assert_eq!(q.nbits_est as i64, int(s, frame, "nbits_est_req"), "{s} {frame}");
				assert_eq!(q.nbits_trunc as i64, int(s, frame, "nbits_trunc_req"), "{s} {frame}");
			}
			// 3.3.11 Residual bits and 3.3.12 noise level.
			let max = nbits_spec - q.nbits_trunc + 4;
			let mut res = Vec::new();
			for (&x, &v) in xf_ref.iter().zip(&q.xq[..ne]) {
				if res.len() as i32 >= max {
					break;
				}
				if v != 0 {
					res.push((x >= v as f64 * q.gg) as i64);
				}
			}
			assert_eq!(res, ints(s, frame, "res_bits"), "{s} {frame}");
			assert_eq!(encoder::noise_level(&cfg, &xf_ref, &q, pbw) as i64, int(s, frame, "F_NF"), "{s} {frame}");
			xbuf.copy_within(nf..2 * nf - z, 0);
		}
	}
}

/// The SNS quantizer on the reference scale factors that select the 'outlier_far' shape (C.3.3).
#[test]
fn appendix_c_sns_outlier_shape() {
	use crate::lc3::sns;
	let s = "enc10_shape3";
	let mut scf = [0.0; 16];
	scf.copy_from_slice(&doubles(s, 0, "scf"));
	let (q, scfq) = sns::quantize(&scf);
	assert_eq!(q.shape as i64, int(s, 0, "shape_j"));
	assert_eq!(q.ind_lf as i64, int(s, 0, "ind_LF"));
	assert_eq!(q.ind_hf as i64, int(s, 0, "ind_HF"));
	assert_eq!(q.gain as i64, int(s, 0, "Gind"));
	assert_eq!(q.ls_a as i64, int(s, 0, "LS_indA"));
	assert_eq!(q.idx_a as i64, int(s, 0, "idxA"));
	assert_close("scfQ", &scfq, &doubles(s, 0, "scfQ"), 1.0e-12);
}

/// The attack detector flags both 48 kHz reference frames (C.3.2 and C.3.4, 88 kbit/s).
#[test]
fn appendix_c_attack_detector() {
	for (s, us) in [("enc10", 10000), ("enc7_5", 7500)] {
		let cfg = Config::new(48000, us).unwrap();
		let nbytes = 88000 * us as usize / 8_000_000;
		let mut enc = Encoder::new(cfg);
		for frame in [1, 2] {
			let pcm: Vec<i16> = ints(s, frame, "x_s").iter().map(|&v| v as i16).collect();
			assert_eq!(enc.attack(&pcm, nbytes) as i64, int(s, frame, "F_att"), "{s} frame {frame}");
		}
	}
}

/// Each decoder stage against the reference: the side information and arithmetic decoding,
/// the shaped spectrum, and the time signal before rounding.
#[test]
fn appendix_c_decoder_stages() {
	use crate::lc3::decoder;
	for (s, es, us) in [("dec10", "enc10", 10000), ("dec7_5", "enc7_5", 7500)] {
		let cfg = Config::new(16000, us).unwrap();
		let mut dec = Decoder::new(cfg);
		for frame in [1u32, 2] {
			let bytes: Vec<u8> = ints(es, frame + 2, "bytes_ari").iter().map(|&v| v as u8).collect();
			assert_eq!(int(s, frame, "nbytes") as usize, bytes.len());
			let p = dec.parse(&bytes).expect("reference frame decodes");
			assert_eq!(p.pbw as i64, int(s, frame, "P_BW"));
			assert_eq!(p.lsb_mode as i64, int(s, frame, "lsbMode"));
			assert_eq!(p.gg_ind as i64, int(s, frame, "gg_ind"));
			assert_eq!(p.tns.num_filters as i64, int(s, frame, "num_tns_filters"));
			assert_eq!(p.tns.lpc_weighting as i64, int(s, frame, "tns_lpc_weighting"));
			let order: Vec<i64> = p.tns.order.iter().map(|&v| v as i64).collect();
			assert_eq!(order, ints(s, frame, "rc_order_ari"));
			let rci: Vec<i64> = p.tns.rc_i.iter().flatten().map(|&v| v as i64).collect();
			if s == "dec10" {
				assert_eq!(rci, ints(s, frame, "rc_i"));
			} else {
				assert_eq!(&rci[..8], &ints(s, frame, "rc_i_1")[..]);
			}
			assert_eq!(p.pitch_index as i64, int(s, frame, "pitch_index"));
			assert_eq!(p.ltpf_active as i64, int(s, frame, "ltpf_active"));
			assert_eq!(p.fnf as i64, int(s, frame, "F_NF"));
			assert_eq!(p.sns.ind_lf as i64, int(s, frame, "ind_LF"));
			assert_eq!(p.sns.ind_hf as i64, int(s, frame, "ind_HF"));
			assert_eq!((p.sns.shape >> 1) as i64, int(s, frame, "submodeMSB"));
			assert_eq!(p.sns.gain as i64, int(s, frame, "Gind"));
			assert_eq!(p.sns.ls_a as i64, int(s, frame, "LS_indA"));
			assert_eq!(p.sns.idx_a as i64, int(s, frame, "idxA"));
			if p.sns.shape == 0 {
				assert_eq!(p.sns.idx_b as i64, int(s, frame, "idxB"));
			}
			let res: Vec<i64> = p.res_bits[..p.nres].iter().map(|&v| v as i64).collect();
			assert_eq!(res, ints(s, frame, "resBits"));
			let xq: Vec<i64> = p.xq[..cfg.ne].iter().map(|&v| v as i64).collect();
			assert_eq!(xq, ints(s, frame, "X_hat_q_ari"));
			let seed_name = if s == "dec10" { "nf_seed" } else { "nfseed" };
			assert_eq!(p.nf_seed as i64, int(s, frame, seed_name));
			let x = decoder::spectrum(&cfg, &p, 8 * bytes.len());
			assert_close(&format!("{s} {frame} X_hat_ss"), &x[..cfg.nf], &doubles(s, frame, "X_hat_ss"), 1.0e-12);
			let y = dec.synthesize(&x, p.ltpf_active, p.pitch_index, 8 * bytes.len());
			assert_close(&format!("{s} {frame} x_hat_ltpf"), &y[..cfg.nf], &doubles(s, frame, "x_hat_ltpf"), 1.0e-12);
		}
	}
}

/// The TNS decoder with two filters of different orders (C.4.4).
#[test]
fn appendix_c_tns_two_filters() {
	use crate::lc3::tns::{self, Tns};
	let cfg = Config::new(48000, 10000).unwrap();
	let s = "dec_tns";
	let order = ints(s, 0, "rc_order");
	let mut t = Tns { num_filters: 2, ..Tns::default() };
	t.order = [order[0] as usize, order[1] as usize];
	for (f, name) in ["rc_i_tns_filter1", "rc_i_tns_filter2"].iter().enumerate() {
		for (k, v) in ints(s, 0, name).iter().enumerate() {
			t.rc_i[f][k] = *v as usize;
		}
	}
	let mut x = doubles(s, 0, "X_f_hat");
	tns::synthesize(&cfg, &mut x, 4, &t);
	assert_close("X_s_tns", &x, &doubles(s, 0, "X_s_tns"), 1.0e-12);
}

/// The LTPF decoder in the four transition cases of section 3.4.9.2 that filter (C.4.5).
#[test]
fn appendix_c_ltpf_transitions() {
	use crate::lc3::ltpf::LtpfDecoder;
	let cfg = Config::new(16000, 10000).unwrap();
	let s = "dec_ltpf";
	for case in 2..=5 {
		let n = |name: &str| format!("{name}_case{case}");
		let nbits = int(s, 0, &n("nbits")) as usize;
		let prev_pitch = int(s, 0, &n("pitch_index_prev")) as u32;
		let curr_pitch = int(s, 0, &n("pitch_index_curr")) as u32;
		let (prev_active, curr_active) = match case {
			2 => (false, true),
			3 => (true, false),
			_ => (true, true),
		};
		// The coefficients (the listing leaves out c_den(0), which is zero).
		if curr_active {
			let (num, den) = LtpfDecoder::coefficients(&cfg, curr_pitch, nbits);
			assert_close(&n("c_num"), &num[..3], &doubles(s, 0, &n("c_num")), 1.0e-12);
			assert_eq!(den[0], 0.0);
			assert_close(&n("c_den"), &den[1..5], &doubles(s, 0, &n("c_den")), 1.0e-12);
		}
		let input_prev = doubles(s, 0, &format!("mdct_synt_output_prev_frame_transition_case{case}"));
		let mut out_prev = doubles(s, 0, &format!("x_hat_ltpf_prev_prev_transition_case{case}"));
		out_prev.extend(doubles(s, 0, &format!("x_hat_ltpf_prev_transition_case{case}")));
		let mut d = LtpfDecoder::with_history(&cfg, &input_prev, &out_prev, prev_active, prev_pitch, nbits);
		let mut x = doubles(s, 0, &format!("input_ltpf_transition_case{case}"));
		d.run(&cfg, &mut x, curr_active, curr_pitch, nbits);
		assert_close(&format!("case {case}"), &x, &doubles(s, 0, &format!("x_hat_ltpf_transition_case{case}")), 1.0e-12);
	}
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
