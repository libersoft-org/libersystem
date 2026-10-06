// The long term postfilter (specification sections 3.3.9 and 3.4.9).
//
// LTPF is a pitch based comb filter the decoder runs on its output to attenuate quantization noise
// between the harmonics of voiced signals. The encoder finds the pitch: it resamples the input to
// 12.8 kHz (11.76 kHz from 44.1 kHz), high-pass filters and delays it, estimates the lag at 6.4 kHz
// from the autocorrelation, refines it to a quarter sample at 12.8 kHz, and decides whether the
// filter should be on from the normalized correlation of this and the previous frames. It sends a
// pitch present bit and, when present, a 9 bit pitch index and the activation bit; the pitch is
// sent even at bit rates where the decoder will not filter, for the benefit of concealment.
//
// The decoder converts the pitch index back to a lag at the output rate and filters with
// coefficient tables chosen by bit rate (the gain is zero, so no filtering, at high rates). When the
// filter switches on, off or changes lag, the first 2.5 ms of the frame cross-fade between the old
// and the new filter (the five cases of section 3.4.9.2).
//
// THE FILTER INPUT HISTORY AND OUTPUT HISTORY ARE DIFFERENT SIGNALS. The numerator taps read the
// unfiltered decoder signal x^ (also into the previous frame), the recursive taps read the filtered
// output; only the fifth transition case feeds a filtered signal into the numerator, as equation
// 135 says.

use crate::lc3::math;
use crate::lc3::tables::{TAB_LTPF_DEN_8000, TAB_LTPF_DEN_16000, TAB_LTPF_DEN_24000, TAB_LTPF_DEN_32000, TAB_LTPF_DEN_48000, TAB_LTPF_INTERP_R, TAB_LTPF_INTERP_X12K8, TAB_LTPF_NUM_8000, TAB_LTPF_NUM_16000, TAB_LTPF_NUM_24000, TAB_LTPF_NUM_32000, TAB_LTPF_NUM_48000, TAB_RESAMP_FILTER};
use crate::lc3::{Config, Duration};

/// History of the high-pass filtered 12.8 kHz signal: the 232 lags of R_12.8, the two samples of
/// the x_i interpolator and the delay D_LTPF (at most 44).
const HP_HIST: usize = 288;
/// Samples of the 12.8 kHz signal per frame, at most (10 ms).
const MAX_LEN12: usize = 128;
/// History of the 6.4 kHz signal: k_max.
const X6_HIST: usize = 114;
const K_MIN: usize = 17;
const K_MAX: usize = 114;

/// gain_ltpf and gain_ind of section 3.4.9.4; None where the gain is zero.
pub(crate) fn gain(cfg: &Config, nbits: usize) -> Option<(f64, usize)> {
	// t_nbits = round(nbits * 10 / 7.5) for 7.5 ms frames.
	let t_nbits = match cfg.dt {
		Duration::Ms10 => nbits as i32,
		Duration::Ms7_5 => math::nint(nbits as f64 * 10.0 / 7.5),
	};
	let f = cfg.fs_ind as i32 * 80;
	if t_nbits < 320 + f {
		Some((0.4, 0))
	} else if t_nbits < 400 + f {
		Some((0.35, 1))
	} else if t_nbits < 480 + f {
		Some((0.3, 2))
	} else if t_nbits < 560 + f {
		Some((0.25, 3))
	} else {
		None
	}
}

/// The LTPF side information of one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LtpfParams {
	pub(crate) pitch_present: bool,
	pub(crate) pitch_index: u32,
	pub(crate) active: bool,
}

/// Encoder state: filter memories and the decisions of past frames.
#[derive(Clone)]
pub(crate) struct LtpfEncoder {
	/// High-pass filter state: x(n-1), x(n-2), y(n-1), y(n-2).
	hp: [f64; 4],
	/// hp_buf[HP_HIST + n] is the high-pass filtered 12.8 kHz signal at n, n < len12.8.
	hp_buf: [f64; HP_HIST + MAX_LEN12],
	/// x6_buf[X6_HIST + n] is x_6.4(n).
	x6_buf: [f64; X6_HIST + MAX_LEN12 / 2],
	t_prev: usize,
	mem_active: bool,
	mem_nc: f64,
	mem_mem_nc: f64,
	mem_pitch: f64,
}

/// normcorr of equation 92 for the signal x (x[off + n] is x(n)), length l, lag t.
fn normcorr(x: &[f64], off: usize, l: usize, t: usize) -> f64 {
	let mut c = 0.0;
	let mut e0 = 0.0;
	let mut e1 = 0.0;
	for n in 0..l {
		let a = x[off + n];
		let b = x[off + n - t];
		c += a * b;
		e0 += a * a;
		e1 += b * b;
	}
	let d = math::sqrt(e0 * e1);
	if d > 0.0 { math::max(0.0, c / d) } else { 0.0 }
}

impl LtpfEncoder {
	pub(crate) fn new() -> LtpfEncoder {
		LtpfEncoder { hp: [0.0; 4], hp_buf: [0.0; HP_HIST + MAX_LEN12], x6_buf: [0.0; X6_HIST + MAX_LEN12 / 2], t_prev: K_MIN, mem_active: false, mem_nc: 0.0, mem_mem_nc: 0.0, mem_pitch: 0.0 }
	}

	/// x~_12.8,D(n) of the last analyzed frame, n = 0 .. len12.8 (equation 84).
	#[cfg(test)]
	pub(crate) fn delayed(&self, cfg: &Config, n: usize) -> f64 {
		let d = if cfg.dt == Duration::Ms10 { 24 } else { 44 };
		self.hp_buf[HP_HIST - d + n]
	}

	/// Sections 3.3.9.3 to 3.3.9.8 for one frame. `x` holds the scaled input with history:
	/// x[off + n] is x_s(n) for -240/P <= n < NF.
	pub(crate) fn analyze(&mut self, cfg: &Config, x: &[f64], off: usize, nbits: usize, near_nyquist: bool) -> LtpfParams {
		let (len12, d_ltpf, corrlen) = match cfg.dt {
			Duration::Ms10 => (128, 24, 64),
			Duration::Ms7_5 => (96, 44, 48),
		};
		let len6 = len12 / 2;
		// Resampling (equation 78): P = 192 kHz / fs (4 at 44.1 kHz), 15 samples at 192 kHz per
		// output sample.
		let p = [24, 12, 8, 6, 4][cfg.fs_ind];
		let resfac = if cfg.fs_ind == 0 { 0.5 } else { 1.0 };
		let half = (120 / p) as isize;
		let mut x12 = [0.0; MAX_LEN12];
		for (n, out) in x12.iter_mut().enumerate().take(len12) {
			let base = (15 * n / p) as isize - half;
			let phase = ((15 * n) % p) as isize;
			let mut s = 0.0;
			for k in -half..=half {
				let m = p as isize * k - phase;
				if m > -120 && m < 120 {
					s += x[(off as isize + base + k) as usize] * TAB_RESAMP_FILTER[(m + 119) as usize];
				}
			}
			*out = resfac * p as f64 * s;
		}
		// High-pass filtering (equation 83), appended to the history buffer.
		self.hp_buf.copy_within(len12..len12 + HP_HIST, 0);
		let [mut x1, mut x2, mut y1, mut y2] = self.hp;
		for (n, &x0) in x12.iter().enumerate().take(len12) {
			let y0 = 0.9827947082978771 * x0 - 1.965589416595754 * x1 + 0.9827947082978771 * x2 + 1.9652933726226904 * y1 - 0.9658854605688177 * y2;
			x2 = x1;
			x1 = x0;
			y2 = y1;
			y1 = y0;
			self.hp_buf[HP_HIST + n] = y0;
		}
		self.hp = [x1, x2, y1, y2];
		// The delayed signal x~_12.8,D(n) = hp(n - D_LTPF) is xd[XD + n].
		let xd = &self.hp_buf;
		let xd_off = HP_HIST - d_ltpf;
		// Downsampling to 6.4 kHz (equation 85).
		const H2: [f64; 5] = [0.1236796411180537, 0.2353512128364889, 0.2819382920909148, 0.2353512128364889, 0.1236796411180537];
		self.x6_buf.copy_within(len6..len6 + X6_HIST, 0);
		for n in 0..len6 {
			let mut s = 0.0;
			for (k, h) in H2.iter().enumerate() {
				s += xd[xd_off + 2 * n + k - 3] * h;
			}
			self.x6_buf[X6_HIST + n] = s;
		}
		let x6 = &self.x6_buf;
		// Pitch detection (equations 86 to 93).
		let mut r6 = [0.0; K_MAX + 1];
		for (k, rk) in r6.iter_mut().enumerate().skip(K_MIN) {
			let mut s = 0.0;
			for n in 0..len6 {
				s += x6[X6_HIST + n] * x6[X6_HIST + n - k];
			}
			*rk = s;
		}
		let mut t1 = K_MIN;
		let mut best = f64::NEG_INFINITY;
		for (k, rk) in r6.iter().enumerate().skip(K_MIN) {
			let w = *rk * (1.0 - 0.5 * (k - K_MIN) as f64 / (K_MAX - K_MIN) as f64);
			if w > best {
				best = w;
				t1 = k;
			}
		}
		let kmin = K_MIN.max(self.t_prev.saturating_sub(4));
		let kmax = K_MAX.min(self.t_prev + 4);
		let mut t2 = kmin;
		let mut best = f64::NEG_INFINITY;
		for (k, rk) in r6.iter().enumerate().take(kmax + 1).skip(kmin) {
			if *rk > best {
				best = *rk;
				t2 = k;
			}
		}
		let nc1 = normcorr(x6, X6_HIST, corrlen, t1);
		let nc2 = normcorr(x6, X6_HIST, corrlen, t2);
		let t_curr = if nc2 <= 0.85 * nc1 { t1 } else { t2 };
		let nc_curr = if t_curr == t1 { nc1 } else { nc2 };
		self.t_prev = t_curr;
		// Equation 94.
		if nc_curr <= 0.6 {
			self.mem_mem_nc = self.mem_nc;
			self.mem_nc = 0.0;
			self.mem_active = false;
			self.mem_pitch = 0.0;
			return LtpfParams::default();
		}
		// Pitch lag at 12.8 kHz (equations 96 to 102).
		let kmin2 = 32.max(2 * t_curr as isize - 4) as usize;
		let kmax2 = 228.min(2 * t_curr + 4);
		let mut r12 = [0.0; 240];
		for (k, rk) in r12.iter_mut().enumerate().take(kmax2 + 5).skip(kmin2 - 4) {
			let mut s = 0.0;
			for n in 0..len12 {
				s += xd[xd_off + n] * xd[xd_off + n - k];
			}
			*rk = s;
		}
		let mut pitch_int = kmin2;
		let mut best = f64::NEG_INFINITY;
		for (k, rk) in r12.iter().enumerate().take(kmax2 + 1).skip(kmin2) {
			if *rk > best {
				best = *rk;
				pitch_int = k;
			}
		}
		let interp = |d: isize| -> f64 {
			let mut s = 0.0;
			for m in -4isize..=4 {
				let i = 4 * m - d;
				if i > -16 && i < 16 {
					s += r12[(pitch_int as isize + m) as usize] * TAB_LTPF_INTERP_R[(i + 15) as usize];
				}
			}
			s
		};
		let (dlo, dhi, dstep) = if pitch_int >= 157 {
			(0, 0, 1)
		} else if pitch_int >= 127 {
			(-2, 2, 2)
		} else if pitch_int > 32 {
			(-3, 3, 1)
		} else {
			(0, 3, 1)
		};
		let mut pitch_fr = dlo;
		let mut best = f64::NEG_INFINITY;
		let mut d = dlo;
		while d <= dhi {
			let v = interp(d);
			if v > best {
				best = v;
				pitch_fr = d;
			}
			d += dstep;
		}
		let mut pitch_int = pitch_int as isize;
		if pitch_fr < 0 {
			pitch_int -= 1;
			pitch_fr += 4;
		}
		let pitch_index = if pitch_int >= 157 {
			pitch_int + 283
		} else if pitch_int >= 127 {
			2 * pitch_int + pitch_fr / 2 + 126
		} else {
			4 * pitch_int + pitch_fr - 128
		} as u32;
		// Normalized correlation for the activation (equations 103 to 105).
		let xi = |n: isize, d: isize| -> f64 {
			let mut s = 0.0;
			for k in -2isize..=2 {
				let i = 4 * k - d;
				if i > -8 && i < 8 {
					s += xd[(xd_off as isize + n - k) as usize] * TAB_LTPF_INTERP_X12K8[(i + 7) as usize];
				}
			}
			s
		};
		let mut c = 0.0;
		let mut e0 = 0.0;
		let mut e1 = 0.0;
		for n in 0..len12 as isize {
			let a = xi(n, 0);
			let b = xi(n - pitch_int, pitch_fr);
			c += a * b;
			e0 += a * a;
			e1 += b * b;
		}
		let den = math::sqrt(e0 * e1);
		let nc = if den > 0.0 { c / den } else { 0.0 };
		let pitch = pitch_int as f64 + pitch_fr as f64 / 4.0;
		let mut active = false;
		if gain(cfg, nbits).is_some() {
			active = (!self.mem_active && (cfg.dt == Duration::Ms10 || self.mem_mem_nc > 0.94) && self.mem_nc > 0.94 && nc > 0.94) || (self.mem_active && nc > 0.9) || (self.mem_active && math::abs(pitch - self.mem_pitch) < 2.0 && (nc - self.mem_nc) > -0.1 && nc > 0.84);
		}
		if near_nyquist {
			active = false;
		}
		self.mem_mem_nc = self.mem_nc;
		self.mem_nc = nc;
		self.mem_active = active;
		self.mem_pitch = pitch;
		LtpfParams { pitch_present: true, pitch_index, active }
	}
}

/// History of the decoder's filtered output: the largest lag plus half the denominator, at 48 kHz
/// 855 + 6 samples.
const OUT_HIST: usize = 1024;
const MAX_NF: usize = 480;
/// The longest transition (norm), 2.5 ms: NF/4 of 480 or NF/3 of 360.
const MAX_NORM: usize = 120;
/// Previous frame input samples kept for the numerator taps (L_num <= 10).
const IN_HIST: usize = 16;

/// Decoder state.
#[derive(Clone)]
pub(crate) struct LtpfDecoder {
	/// input[IN_HIST + n] is the unfiltered x^(n) of the current frame, the IN_HIST entries
	/// before it are the end of the previous frame.
	input: [f64; IN_HIST + MAX_NF],
	/// y[OUT_HIST + n] is the filtered output x^_ltpf(n) of the current frame.
	y: [f64; OUT_HIST + MAX_NF],
	/// Previous frame: ltpf_active, p_int, p_fr, c_num, c_den.
	active: bool,
	p_int: usize,
	p_fr: usize,
	c_num: [f64; 11],
	c_den: [f64; 13],
}

/// The filter of one frame: lag at the output rate and coefficients (section 3.4.9.4).
struct Filter {
	p_int: usize,
	p_fr: usize,
	c_num: [f64; 11],
	c_den: [f64; 13],
}

fn filter_params(cfg: &Config, pitch_index: u32, nbits: usize) -> Filter {
	let pitch_index = pitch_index as i32;
	// Equations 139 to 141.
	let (pitch_int, pitch_fr) = if pitch_index >= 440 {
		(pitch_index - 283, 0)
	} else if pitch_index >= 380 {
		let pi = pitch_index / 2 - 63;
		(pi, 2 * pitch_index - 4 * pi - 252)
	} else {
		let pi = pitch_index / 4 + 32;
		(pi, pitch_index - 4 * pi + 128)
	};
	let pitch = pitch_int as f64 + pitch_fr as f64 / 4.0;
	// Equations 142 to 145: 8000 ceil(fs / 8000) is 48000 at 44.1 kHz.
	let fs_ceil = [8000.0, 16000.0, 24000.0, 32000.0, 48000.0][cfg.fs_ind];
	let p_up = math::nint(pitch * fs_ceil / 12800.0 * 4.0);
	let p_int = (p_up / 4) as usize;
	let p_fr = (p_up - 4 * (p_up / 4)) as usize;
	let mut f = Filter { p_int, p_fr, c_num: [0.0; 11], c_den: [0.0; 13] };
	if let Some((g, gi)) = gain(cfg, nbits) {
		let (num, den): (&[f64], &[f64]) = match cfg.fs_ind {
			0 => (&TAB_LTPF_NUM_8000[gi], &TAB_LTPF_DEN_8000[p_fr]),
			1 => (&TAB_LTPF_NUM_16000[gi], &TAB_LTPF_DEN_16000[p_fr]),
			2 => (&TAB_LTPF_NUM_24000[gi], &TAB_LTPF_DEN_24000[p_fr]),
			3 => (&TAB_LTPF_NUM_32000[gi], &TAB_LTPF_DEN_32000[p_fr]),
			_ => (&TAB_LTPF_NUM_48000[gi], &TAB_LTPF_DEN_48000[p_fr]),
		};
		for (c, t) in f.c_num.iter_mut().zip(num) {
			*c = 0.85 * g * t;
		}
		for (c, t) in f.c_den.iter_mut().zip(den) {
			*c = g * t;
		}
	}
	f
}

/// L_den of equation 148 (at 44.1 kHz ceil(44100 / 4000) = 12, as at 48 kHz).
fn l_den(cfg: &Config) -> usize {
	[4, 4, 6, 8, 12][cfg.fs_ind]
}

impl LtpfDecoder {
	pub(crate) fn new() -> LtpfDecoder {
		LtpfDecoder { input: [0.0; IN_HIST + MAX_NF], y: [0.0; OUT_HIST + MAX_NF], active: false, p_int: 0, p_fr: 0, c_num: [0.0; 11], c_den: [0.0; 13] }
	}

	/// A decoder whose previous frames are given: `prev_input` ends with the unfiltered signal of
	/// the previous frame, `prev_output` with the filtered output, and the previous frame's filter
	/// was `prev_active` with `prev_pitch_index` at `prev_nbits`.
	#[cfg(test)]
	pub(crate) fn with_history(cfg: &Config, prev_input: &[f64], prev_output: &[f64], prev_active: bool, prev_pitch_index: u32, prev_nbits: usize) -> LtpfDecoder {
		let mut d = LtpfDecoder::new();
		d.input[..IN_HIST].copy_from_slice(&prev_input[prev_input.len() - IN_HIST..]);
		d.y[OUT_HIST - prev_output.len()..OUT_HIST].copy_from_slice(prev_output);
		d.active = prev_active;
		if prev_active {
			let f = filter_params(cfg, prev_pitch_index, prev_nbits);
			d.p_int = f.p_int;
			d.p_fr = f.p_fr;
			d.c_num = f.c_num;
			d.c_den = f.c_den;
		}
		d
	}

	/// The filter coefficients c_num, c_den of a pitch index at a bit rate.
	#[cfg(test)]
	pub(crate) fn coefficients(cfg: &Config, pitch_index: u32, nbits: usize) -> ([f64; 11], [f64; 13]) {
		let f = filter_params(cfg, pitch_index, nbits);
		(f.c_num, f.c_den)
	}

	/// Filters one frame in place: x holds x^(n), n < NF, and receives x^_ltpf(n).
	pub(crate) fn run(&mut self, cfg: &Config, x: &mut [f64], active: bool, pitch_index: u32, nbits: usize) {
		let nf = cfg.nf;
		let lden = l_den(cfg);
		let lnum = lden - 2;
		let half = lden / 2;
		// norm = NF/4 * 10/Nms: the 2.5 ms transition.
		let norm = match cfg.dt {
			Duration::Ms10 => nf / 4,
			Duration::Ms7_5 => nf / 3,
		};
		let cur = if active { Some(filter_params(cfg, pitch_index, nbits)) } else { None };
		let input = &mut self.input;
		input[IN_HIST..IN_HIST + nf].copy_from_slice(&x[..nf]);
		let input = &*input;
		let y = &mut self.y;
		// One output sample: sig(n - k) (sig[IN_HIST + n - k]) through the numerator, the output
		// through the denominator at lag p_int, the bracket of equations 131 to 138.
		let bracket = |y: &[f64], sig: &[f64], n: usize, c_num: &[f64; 11], c_den: &[f64; 13], p_int: usize| -> f64 {
			let mut s = 0.0;
			for k in 0..=lnum {
				s += c_num[k] * sig[IN_HIST + n - k];
			}
			for k in 0..=lden {
				s -= c_den[k] * y[OUT_HIST + n + half - p_int - k];
			}
			s
		};
		let mem = if self.active { Some((self.p_int, self.c_num, self.c_den)) } else { None };
		let ramp = |n: usize| n as f64 / norm as f64;
		match (mem, &cur) {
			(None, None) => {
				y[OUT_HIST..OUT_HIST + norm].copy_from_slice(&input[IN_HIST..IN_HIST + norm]);
			}
			(None, Some(c)) => {
				// Case 2: fade in.
				for n in 0..norm {
					y[OUT_HIST + n] = input[IN_HIST + n] - ramp(n) * bracket(y, input, n, &c.c_num, &c.c_den, c.p_int);
				}
			}
			(Some((mp, mnum, mden)), None) => {
				// Case 3: fade out.
				for n in 0..norm {
					y[OUT_HIST + n] = input[IN_HIST + n] - (1.0 - ramp(n)) * bracket(y, input, n, &mnum, &mden, mp);
				}
			}
			(Some((mp, mnum, mden)), Some(c)) => {
				if mp == c.p_int && self.p_fr == c.p_fr {
					// Case 4: the same filter goes on.
					for n in 0..norm {
						y[OUT_HIST + n] = input[IN_HIST + n] - bracket(y, input, n, &c.c_num, &c.c_den, c.p_int);
					}
				} else {
					// Case 5: fade the old filter out, then the new one in over its output.
					for n in 0..norm {
						y[OUT_HIST + n] = input[IN_HIST + n] - (1.0 - ramp(n)) * bracket(y, input, n, &mnum, &mden, mp);
					}
					let mut x2 = [0.0; IN_HIST + MAX_NORM];
					x2[..IN_HIST + norm].copy_from_slice(&y[OUT_HIST - IN_HIST..OUT_HIST + norm]);
					for n in 0..norm {
						y[OUT_HIST + n] = x2[IN_HIST + n] - ramp(n) * bracket(y, &x2, n, &c.c_num, &c.c_den, c.p_int);
					}
				}
			}
		}
		// Remainder of the frame (equations 137 and 138).
		for n in norm..nf {
			y[OUT_HIST + n] = match &cur {
				Some(c) => input[IN_HIST + n] - bracket(y, input, n, &c.c_num, &c.c_den, c.p_int),
				None => input[IN_HIST + n],
			};
		}
		x[..nf].copy_from_slice(&y[OUT_HIST..OUT_HIST + nf]);
		// Keep the histories and this frame's filter.
		self.input.copy_within(nf..nf + IN_HIST, 0);
		self.y.copy_within(nf..nf + OUT_HIST, 0);
		self.active = active;
		if let Some(c) = cur {
			self.p_int = c.p_int;
			self.p_fr = c.p_fr;
			self.c_num = c.c_num;
			self.c_den = c.c_den;
		}
	}
}
