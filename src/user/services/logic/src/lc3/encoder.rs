// The LC3 encoder (specification section 3.3).
//
// One call codes one frame of NF samples into nbytes bytes, in the order of the specification:
// LD-MDCT and band energies (3.3.4), near Nyquist and bandwidth detection (3.3.4.5, 3.3.5), attack
// detection (3.3.6), spectral noise shaping (3.3.7, sns.rs), temporal noise shaping (3.3.8,
// tns.rs), the long term postfilter's pitch analysis (3.3.9, ltpf.rs), spectral quantization with
// its global gain search, bit estimate and truncation (3.3.10), residual bits (3.3.11), the noise
// level (3.3.12) and the frame itself (3.3.13).
//
// The encoder carries state from frame to frame: the input history the MDCT window and the
// resampler reach back into, the LTPF filter memories and decisions, the attack detector's
// envelope, and the bit budget offset of the global gain estimate. nbytes may change from frame to
// frame (section 3.6); everything that depends on the bit rate is recomputed per frame.
//
// THE BIT BUDGET IS THE SPECIFICATION'S. The global gain search, truncation and the residual bit
// count use exactly the formulas of 3.3.10 and 3.3.13.5, so the side information, the arithmetic
// coded spectrum and the residual bits meet without a gap or an overlap, as a decoder expects.

use crate::lc3::bits::{AcEncoder, SideWriter};
use crate::lc3::ltpf::{LtpfEncoder, LtpfParams};
use crate::lc3::math;
use crate::lc3::mdct::{MAX_NF, Mdct};
use crate::lc3::sns::{self, SnsIndices};
use crate::lc3::tables::{AC_SPEC_BITS, AC_SPEC_CUMFREQ, AC_SPEC_FREQ, AC_SPEC_LOOKUP, AC_TNS_COEF_CUMFREQ, AC_TNS_COEF_FREQ, AC_TNS_ORDER_CUMFREQ, AC_TNS_ORDER_FREQ};
use crate::lc3::tns::{self, Tns};
use crate::lc3::{Config, Duration, Error, MAX_BYTES, MIN_BYTES};

/// Largest NE.
const MAX_NE: usize = 400;

/// The quantized spectrum and what the bit estimate of section 3.3.10.4 found out about it.
pub(crate) struct Quantized {
	pub(crate) xq: [i32; MAX_NE],
	pub(crate) gg: f64,
	pub(crate) lastnz: usize,
	pub(crate) lastnz_trunc: usize,
	pub(crate) nbits_est: i32,
	pub(crate) nbits_trunc: i32,
	pub(crate) lsb_mode: bool,
}

/// An LC3 encoder for one channel.
///
/// The struct holds its state and its large work buffers in fixed arrays sized for 48 kHz, about
/// 36 KB; box it where stack space is scarce. An encode() call itself needs about 16 KB of stack.
#[derive(Clone)]
pub struct Encoder {
	cfg: Config,
	mdct: Mdct,
	/// x_s: the last NF - Z samples of the previous frames, then the current frame.
	x: [f64; 2 * MAX_NF],
	ltpf: LtpfEncoder,
	/// Attack detector: x_att(-2), x_att(-1), E_att(-1), A_att(-1), P_att(k - 1).
	att_x: [f64; 2],
	att_e: f64,
	att_a: f64,
	att_p: i32,
	/// Global gain estimate memory: nbits_offset, nbits_spec, nbits_est and reset_offset of the
	/// previous frame.
	old_offset: f64,
	old_spec: i32,
	old_est: i32,
	old_reset: bool,
}

impl Encoder {
	pub fn new(config: Config) -> Encoder {
		Encoder { cfg: config, mdct: Mdct::new(config.nf, config.window), x: [0.0; 2 * MAX_NF], ltpf: LtpfEncoder::new(), att_x: [0.0; 2], att_e: 0.0, att_a: 0.0, att_p: -1, old_offset: 0.0, old_spec: 0, old_est: 0, old_reset: false }
	}

	/// One frame: `pcm` is exactly NF signed 16-bit samples; `out.len()` is nbytes, 20..=400; every
	/// byte of `out` is written.
	pub fn encode(&mut self, pcm: &[i16], out: &mut [u8]) -> Result<(), Error> {
		let cfg = self.cfg;
		let nf = cfg.nf;
		let ne = cfg.ne;
		if pcm.len() != nf {
			return Err(Error::Samples);
		}
		let nbytes = out.len();
		if !(MIN_BYTES..=MAX_BYTES).contains(&nbytes) {
			return Err(Error::Bytes);
		}
		let nbits = 8 * nbytes;

		// 3.3.3 Input: 16-bit PCM is already in the native range.
		let hist = nf - cfg.z;
		for (d, &s) in self.x[hist..hist + nf].iter_mut().zip(pcm) {
			*d = s as f64;
		}

		// 3.3.4 LD-MDCT: t(n) = x_s(Z - NF + n) is self.x itself; its last Z samples (equation 7)
		// are entries of self.x that are never written and stay zero.
		let mut spec = [0.0; MAX_NF];
		self.mdct.forward(&self.x, &mut spec);
		let mut eb = [0.0; 64];
		for (e, band) in eb.iter_mut().zip(cfg.bands.windows(2)) {
			let (lo, hi) = (band[0] as usize, band[1] as usize);
			let mut s = 0.0;
			for v in &spec[lo..hi] {
				s += v * v;
			}
			*e = s / (hi - lo) as f64;
		}
		let near_nyquist = near_nyquist(&cfg, &eb);
		let pbw = bandwidth(&cfg, &eb);
		let attack = self.attack(pcm, nbytes);

		// 3.3.7 SNS.
		let scf = sns::analyze(&cfg, &eb, attack);
		let (sns_q, scfq) = sns::quantize(&scf);
		let g = sns::interpolate(&cfg, &scfq, true);
		for b in 0..cfg.nb {
			for v in &mut spec[cfg.bands[b] as usize..cfg.bands[b + 1] as usize] {
				*v *= g[b];
			}
		}

		// 3.3.8 TNS (on X_s, giving X_f in place).
		let tns = tns::analyze(&cfg, &spec, pbw, nbits, near_nyquist);
		tns::filter(&cfg, &mut spec, pbw, &tns);
		let xf = &spec;

		// 3.3.9 LTPF.
		let ltpf = self.ltpf.analyze(&cfg, &self.x, hist, nbits, near_nyquist);

		// 3.3.10.1 Bit budget.
		let nbits_tns = tns::nbits(&tns);
		let nbits_ltpf = if ltpf.pitch_present { 11 } else { 1 };
		let nbits_ari = cfg.nbits_lastnz() as usize
			+ if nbits <= 1280 {
				3
			} else if nbits <= 2560 {
				4
			} else {
				5
			};
		let nbits_spec = nbits as i32 - cfg.nbits_bw() as i32 - nbits_tns as i32 - nbits_ltpf - 38 - 8 - 3 - nbits_ari as i32;

		// 3.3.10.2 First global gain estimation.
		let offset = if self.old_reset { 0.0 } else { 0.8 * self.old_offset + 0.2 * math::min(40.0, math::max(-40.0, self.old_offset + (self.old_spec - self.old_est) as f64)) };
		let nbits_spec2 = math::nint(nbits_spec as f64 + offset);
		let gg_off = cfg.gg_off(nbits);
		let mut gg_ind = estimate_gain(xf, ne, nbits_spec2, gg_off);
		let mut xf_max = 0.0;
		for v in &xf[..ne] {
			xf_max = math::max(xf_max, math::abs(*v));
		}
		let gg_min = if xf_max > 0.0 { math::ceil(28.0 * math::log10(1.0e-31 + xf_max / (32768.0 - 0.375))) as i32 - gg_off } else { 0 };
		// The 8 bit field cannot hold more; 16-bit input never gets close.
		let gg_min = gg_min.min(255);
		let reset = gg_ind < gg_min || xf_max == 0.0;
		if reset {
			gg_ind = gg_min;
		}

		// 3.3.10.3 to 3.3.10.6 Quantization, bit estimate, truncation, gain adjustment.
		let mut q = quantize(&cfg, xf, gg_ind, gg_off, nbits, nbits_spec);
		self.old_offset = offset;
		self.old_spec = nbits_spec;
		self.old_est = q.nbits_est;
		self.old_reset = reset;
		let delta = gain_delta(cfg.fs_ind, q.nbits_est);
		if (gg_ind < 255 && q.nbits_est > nbits_spec) || (gg_ind > 0 && q.nbits_est < nbits_spec - (delta + 2)) {
			if q.nbits_est < nbits_spec - (delta + 2) {
				gg_ind -= 1;
			} else if gg_ind == 254 || q.nbits_est < nbits_spec + delta {
				gg_ind += 1;
			} else {
				gg_ind += 2;
			}
			gg_ind = gg_ind.max(gg_min);
			q = quantize(&cfg, xf, gg_ind, gg_off, nbits, nbits_spec);
		}

		// 3.3.11 Residual bits.
		let mut res_bits = [0u8; MAX_NE];
		let mut nres = 0;
		if !q.lsb_mode {
			let max = nbits_spec - q.nbits_trunc + 4;
			let mut k = 0;
			while k < ne && (nres as i32) < max {
				if q.xq[k] != 0 {
					res_bits[nres] = (xf[k] >= q.xq[k] as f64 * q.gg) as u8;
					nres += 1;
				}
				k += 1;
			}
		}

		// 3.3.12 Noise level.
		let fnf = noise_level(&cfg, xf, &q, pbw);

		// 3.3.13 The frame.
		let frame = Frame { pbw, gg_ind: gg_ind as u32, tns, ltpf, sns: sns_q, fnf, nbits };
		write_frame(&cfg, out, &frame, &q, &res_bits[..nres]);

		self.x.copy_within(nf..nf + hist, 0);
		Ok(())
	}

	/// Section 3.3.6: the attack flag F_att of this frame. The detector state follows the signal
	/// in every frame of a 32 kHz and higher configuration, the flag counts only where the bit rate
	/// enables the detector.
	pub(crate) fn attack(&mut self, pcm: &[i16], nbytes: usize) -> bool {
		let cfg = &self.cfg;
		if cfg.fs_ind < 3 {
			return false;
		}
		let (mf, nblocks) = match cfg.dt {
			Duration::Ms10 => (160, 4),
			Duration::Ms7_5 => (120, 3),
		};
		let ratio = cfg.nf / mf;
		let mut e = [0.0; 4];
		let [mut x2, mut x1] = self.att_x;
		for n in 0..mf {
			let mut xa = 0.0;
			for m in 0..ratio {
				xa += pcm[ratio * n + m] as f64;
			}
			let hp = 0.375 * xa - 0.5 * x1 + 0.125 * x2;
			x2 = x1;
			x1 = xa;
			e[n / 40] += hp * hp;
		}
		self.att_x = [x2, x1];
		let mut p_att = -1;
		for (n, &en) in e.iter().enumerate().take(nblocks) {
			let a = math::max(0.25 * self.att_a, self.att_e);
			if en > 8.5 * a {
				p_att = n as i32;
			}
			self.att_a = a;
			self.att_e = en;
		}
		let flag = p_att >= 0 || self.att_p >= nblocks as i32 / 2;
		self.att_p = p_att;
		let enabled = match (cfg.dt, cfg.fs_ind) {
			(Duration::Ms10, 3) => nbytes > 80,
			(Duration::Ms10, _) => nbytes >= 100,
			(Duration::Ms7_5, 3) => (61..150).contains(&nbytes),
			(Duration::Ms7_5, _) => (75..150).contains(&nbytes),
		};
		enabled && flag
	}
}

/// Section 3.3.4.5 (only up to 32 kHz).
pub(crate) fn near_nyquist(cfg: &Config, eb: &[f64; 64]) -> bool {
	if cfg.fs_ind > 3 {
		return false;
	}
	let nn_idx = match cfg.dt {
		Duration::Ms10 => cfg.nb - 2,
		Duration::Ms7_5 => cfg.nb - 4,
	};
	let high: f64 = eb[nn_idx..cfg.nb].iter().sum();
	let low: f64 = eb[..nn_idx].iter().sum();
	high > 30.0 * low
}

/// Section 3.3.5: the bandwidth index P_bw.
pub(crate) fn bandwidth(cfg: &Config, eb: &[f64; 64]) -> usize {
	let nbw = cfg.fs_ind;
	if nbw == 0 {
		return 0;
	}
	// Table 3.6.
	let (start, stop): (&[usize], &[usize]) = match (cfg.dt, nbw) {
		(Duration::Ms10, 1) => (&[53], &[63]),
		(Duration::Ms10, 2) => (&[47, 59], &[56, 63]),
		(Duration::Ms10, 3) => (&[44, 54, 60], &[52, 59, 63]),
		(Duration::Ms10, _) => (&[41, 51, 57, 61], &[49, 55, 60, 63]),
		(Duration::Ms7_5, 1) => (&[51], &[63]),
		(Duration::Ms7_5, 2) => (&[45, 58], &[55, 63]),
		(Duration::Ms7_5, 3) => (&[42, 53, 60], &[51, 58, 63]),
		(Duration::Ms7_5, _) => (&[40, 51, 57, 61], &[48, 55, 60, 63]),
	};
	const TQ: [f64; 4] = [20.0, 10.0, 10.0, 10.0];
	const TC: [f64; 4] = [15.0, 23.0, 20.0, 20.0];
	let l: [usize; 4] = match cfg.dt {
		Duration::Ms10 => [4, 4, 3, 1],
		Duration::Ms7_5 => [4, 4, 3, 2],
	};
	// First stage (equation 12): bw0 is one above the highest band region that is not quiet.
	let quiet = |k: usize| -> bool {
		let s: f64 = eb[start[k]..=stop[k]].iter().sum();
		s / ((stop[k] - start[k] + 1) as f64) < TQ[k]
	};
	let mut bw0 = nbw;
	while bw0 > 0 && quiet(bw0 - 1) {
		bw0 -= 1;
	}
	if bw0 == nbw {
		return nbw;
	}
	// Second stage (equation 13): a steep enough energy drop at the cut-off confirms it.
	let lb = l[bw0];
	let mut drop = f64::NEG_INFINITY;
	for n in start[bw0] + 1 - lb..=start[bw0] + 1 {
		let (num, den) = (eb[n - lb], eb[n]);
		let v = if den > 0.0 {
			10.0 * math::log10(1.0e-31 + num / den)
		} else if num > 0.0 {
			f64::INFINITY
		} else {
			// 0 / 0: not a drop.
			f64::NEG_INFINITY
		};
		drop = math::max(drop, v);
	}
	if drop > TC[bw0] { bw0 } else { nbw }
}

/// Section 3.3.10.2: the bisection search of the global gain index.
pub(crate) fn estimate_gain(xf: &[f64], ne: usize, nbits_spec: i32, gg_off: i32) -> i32 {
	let mut e = [0.0; MAX_NE / 4];
	for (k, ek) in e.iter_mut().enumerate().take(ne / 4) {
		let s = xf[4 * k] * xf[4 * k] + xf[4 * k + 1] * xf[4 * k + 1] + xf[4 * k + 2] * xf[4 * k + 2] + xf[4 * k + 3] * xf[4 * k + 3];
		*ek = 10.0 * math::log10(math::exp2(-31.0) + s);
	}
	let mut fac = 256;
	let mut gg_ind = 255;
	for _ in 0..8 {
		fac >>= 1;
		gg_ind -= fac;
		let g = (gg_ind + gg_off) as f64;
		let mut tmp = 0.0;
		let mut iszero = true;
		for i in (0..ne / 4).rev() {
			let ei = e[i] * 28.0 / 20.0;
			if ei < g {
				if !iszero {
					tmp += 2.7 * 28.0 / 20.0;
				}
			} else {
				if g < ei - 43.0 * 28.0 / 20.0 {
					tmp += 2.0 * ei - 2.0 * g - 36.0 * 28.0 / 20.0;
				} else {
					tmp += ei - g + 7.0 * 28.0 / 20.0;
				}
				iszero = false;
			}
		}
		if tmp > nbits_spec as f64 * 1.4 * 28.0 / 20.0 && !iszero {
			gg_ind += fac;
		}
	}
	gg_ind
}

/// delta of section 3.3.10.6 (the result is nint()ed there).
pub(crate) fn gain_delta(fs_ind: usize, nbits_est: i32) -> i32 {
	const T1: [f64; 5] = [80.0, 230.0, 380.0, 530.0, 680.0];
	const T2: [f64; 5] = [500.0, 1025.0, 1550.0, 2075.0, 2600.0];
	const T3: [f64; 5] = [850.0, 1700.0, 2550.0, 3400.0, 4250.0];
	let n = nbits_est as f64;
	let (t1, t2, t3) = (T1[fs_ind], T2[fs_ind], T3[fs_ind]);
	let delta = if n < t1 {
		(n + 48.0) / 16.0
	} else if n < t2 {
		let tmp1 = t1 / 16.0 + 3.0;
		let tmp2 = t2 / 48.0;
		(n - t1) * (tmp2 - tmp1) / (t2 - t1) + tmp1
	} else if n < t3 {
		n / 48.0
	} else {
		t3 / 48.0
	};
	math::nint(delta)
}

/// The context state t of a 2-tuple from the previous one (shared by the bit estimate, the
/// encoder and the decoder): t = 1 + (a + b)(lev + 1) for lev <= 1, else 12 + lev.
pub(crate) fn next_context(c: usize, a: u32, b: u32, lev: usize) -> usize {
	let lev = lev.min(3);
	let t = if lev <= 1 { 1 + (a + b) as usize * (lev + 1) } else { 12 + lev };
	(c & 15) * 16 + t
}

/// Sections 3.3.10.3 to 3.3.10.5 for one global gain index.
pub(crate) fn quantize(cfg: &Config, xf: &[f64], gg_ind: i32, gg_off: i32, nbits: usize, nbits_spec: i32) -> Quantized {
	let ne = cfg.ne;
	let gg = math::pow10((gg_ind + gg_off) as f64 / 28.0);
	let mut q = Quantized { xq: [0; MAX_NE], gg, lastnz: 0, lastnz_trunc: 2, nbits_est: 0, nbits_trunc: 0, lsb_mode: false };
	for (xq, &x) in q.xq.iter_mut().zip(&xf[..ne]) {
		let v = x / gg;
		let v = if x >= 0.0 { math::floor(v + 0.375) } else { math::ceil(v - 0.375) };
		// The global gain floor keeps |v| within 16 bits; the clamp only guards the 14 bit planes
		// the decoder accepts against a pathological input.
		*xq = math::max(-32767.0, math::min(32767.0, v)) as i32;
	}
	let rate_flag = cfg.rate_flag(nbits);
	let mode_flag = nbits >= 480 + cfg.fs_ind * 160;
	let mut lastnz = ne;
	while lastnz > 2 && q.xq[lastnz - 1] == 0 && q.xq[lastnz - 2] == 0 {
		lastnz -= 2;
	}
	let mut est: i64 = 0;
	let mut trunc: i64 = 0;
	let mut lsb: i64 = 0;
	let mut c = 0;
	let budget = nbits_spec as i64 * 2048;
	let mut n = 0;
	while n < lastnz {
		let mut t = c + rate_flag;
		if n > ne / 2 {
			t += 256;
		}
		let mut a = q.xq[n].unsigned_abs();
		let mut b = q.xq[n + 1].unsigned_abs();
		let mut lev = 0;
		while a.max(b) >= 4 {
			let pki = AC_SPEC_LOOKUP[t + lev * 1024] as usize;
			est += AC_SPEC_BITS[pki][16] as i64;
			if lev == 0 && mode_flag {
				lsb += 2;
			} else {
				est += 2 * 2048;
			}
			a >>= 1;
			b >>= 1;
			lev = (lev + 1).min(3);
		}
		let pki = AC_SPEC_LOOKUP[t + lev * 1024] as usize;
		est += AC_SPEC_BITS[pki][(a + 4 * b) as usize] as i64;
		let a_lsb = q.xq[n].unsigned_abs();
		let b_lsb = q.xq[n + 1].unsigned_abs();
		est += (a_lsb.min(1) + b_lsb.min(1)) as i64 * 2048;
		if lev > 0 && mode_flag {
			if a_lsb >> 1 == 0 && q.xq[n] != 0 {
				lsb += 1;
			}
			if b_lsb >> 1 == 0 && q.xq[n + 1] != 0 {
				lsb += 1;
			}
		}
		if (q.xq[n] != 0 || q.xq[n + 1] != 0) && est <= budget {
			q.lastnz_trunc = n + 2;
			trunc = est;
		}
		c = next_context(c, a, b, lev);
		n += 2;
	}
	q.nbits_est = ((est + 2047) / 2048 + lsb) as i32;
	q.nbits_trunc = ((trunc + 2047) / 2048) as i32;
	q.lastnz = lastnz;
	for v in &mut q.xq[q.lastnz_trunc..lastnz.max(q.lastnz_trunc)] {
		*v = 0;
	}
	q.lsb_mode = mode_flag && q.nbits_est > nbits_spec;
	q
}

/// Section 3.3.12: the noise level index F_NF.
pub(crate) fn noise_level(cfg: &Config, xf: &[f64], q: &Quantized, pbw: usize) -> u32 {
	let bw_stop = cfg.bw_stop(pbw);
	let (start, width) = cfg.nf_start_width();
	let mut sum = 0.0;
	let mut count = 0;
	for (k, &x) in xf.iter().enumerate().take(bw_stop).skip(start) {
		let hi = (bw_stop - 1).min(k + width);
		if q.xq[k - width..=hi].iter().all(|&v| v == 0) {
			sum += math::abs(x) / q.gg;
			count += 1;
		}
	}
	// No line to estimate from: the level of an empty spectrum, the lowest.
	let lnf = if count > 0 { sum / count as f64 } else { 0.0 };
	math::nint(8.0 - 16.0 * lnf).clamp(0, 7) as u32
}

/// What the frame carries besides the spectrum.
struct Frame {
	pbw: usize,
	gg_ind: u32,
	tns: Tns,
	ltpf: LtpfParams,
	sns: SnsIndices,
	fnf: u32,
	nbits: usize,
}

/// Section 3.3.13: side information backwards, arithmetic coded data forwards, residual bits.
fn write_frame(cfg: &Config, bytes: &mut [u8], f: &Frame, q: &Quantized, res_bits: &[u8]) {
	for b in bytes.iter_mut() {
		*b = 0;
	}
	let ne = cfg.ne;
	let mut w = SideWriter::new(bytes.len());
	// 3.3.13.3 Side information.
	if cfg.nbits_bw() > 0 {
		w.uint(bytes, f.pbw as u32, cfg.nbits_bw());
	}
	w.uint(bytes, (q.lastnz_trunc as u32 >> 1) - 1, cfg.nbits_lastnz());
	w.bit(bytes, q.lsb_mode as u32);
	w.uint(bytes, f.gg_ind, 8);
	for fi in 0..f.tns.num_filters {
		w.bit(bytes, f.tns.order[fi].min(1) as u32);
	}
	w.bit(bytes, f.ltpf.pitch_present as u32);
	sns::write(&mut w, bytes, &f.sns);
	if f.ltpf.pitch_present {
		w.uint(bytes, f.ltpf.active as u32, 1);
		w.uint(bytes, f.ltpf.pitch_index, 9);
	}
	w.uint(bytes, f.fnf, 3);

	// 3.3.13.4 Arithmetic coding of the TNS data and the spectrum; signs and low bit planes go
	// to the side stream as they come.
	let mut ac = AcEncoder::new();
	for fi in 0..f.tns.num_filters {
		let order = f.tns.order[fi];
		if order > 0 {
			let wgt = f.tns.lpc_weighting;
			ac.encode(bytes, AC_TNS_ORDER_CUMFREQ[wgt][order - 1] as u32, AC_TNS_ORDER_FREQ[wgt][order - 1] as u32);
			for k in 0..order {
				let i = f.tns.rc_i[fi][k];
				ac.encode(bytes, AC_TNS_COEF_CUMFREQ[k][i] as u32, AC_TNS_COEF_FREQ[k][i] as u32);
			}
		}
	}
	let rate_flag = cfg.rate_flag(f.nbits);
	let mut lsbs = [0u8; 2 * MAX_NE];
	let mut nlsbs = 0;
	let mut c = 0;
	let mut k = 0;
	while k < q.lastnz_trunc {
		let mut t = c + rate_flag;
		if k > ne / 2 {
			t += 256;
		}
		let xa = q.xq[k];
		let xb = q.xq[k + 1];
		let mut a = xa.unsigned_abs();
		let mut b = xb.unsigned_abs();
		let mut lev = 0;
		let mut lsb0 = 0;
		let mut lsb1 = 0;
		while a.max(b) >= 4 {
			let pki = AC_SPEC_LOOKUP[t + lev.min(3) * 1024] as usize;
			ac.encode(bytes, AC_SPEC_CUMFREQ[pki][16] as u32, AC_SPEC_FREQ[pki][16] as u32);
			if q.lsb_mode && lev == 0 {
				lsb0 = (a & 1) as u8;
				lsb1 = (b & 1) as u8;
			} else {
				w.bit(bytes, a & 1);
				w.bit(bytes, b & 1);
			}
			a >>= 1;
			b >>= 1;
			lev += 1;
		}
		let pki = AC_SPEC_LOOKUP[t + lev.min(3) * 1024] as usize;
		let sym = (a + 4 * b) as usize;
		ac.encode(bytes, AC_SPEC_CUMFREQ[pki][sym] as u32, AC_SPEC_FREQ[pki][sym] as u32);
		let mut a_lsb = xa.unsigned_abs();
		let mut b_lsb = xb.unsigned_abs();
		if q.lsb_mode && lev > 0 {
			a_lsb >>= 1;
			b_lsb >>= 1;
			lsbs[nlsbs] = lsb0;
			nlsbs += 1;
			if a_lsb == 0 && xa != 0 {
				lsbs[nlsbs] = (xa < 0) as u8;
				nlsbs += 1;
			}
			lsbs[nlsbs] = lsb1;
			nlsbs += 1;
			if b_lsb == 0 && xb != 0 {
				lsbs[nlsbs] = (xb < 0) as u8;
				nlsbs += 1;
			}
		}
		if a_lsb > 0 {
			w.bit(bytes, (xa < 0) as u32);
		}
		if b_lsb > 0 {
			w.bit(bytes, (xb < 0) as u32);
		}
		c = next_context(c, a, b, lev);
		k += 2;
	}

	// 3.3.13.5 Residual data and finalization.
	let nbits = f.nbits as i32;
	let nbits_side = w.used(nbits);
	let nbits_ari = ac.used();
	let avail = nbits - (nbits_side + nbits_ari);
	let extra: &[u8] = if q.lsb_mode { &lsbs[..nlsbs] } else { res_bits };
	let n = avail.clamp(0, extra.len() as i32) as usize;
	for &bit in &extra[..n] {
		w.bit(bytes, bit as u32);
	}
	ac.finish(bytes);
}
