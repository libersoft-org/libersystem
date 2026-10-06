// The low delay MDCT of specification sections 3.3.4.3 (analysis) and 3.4.8 (synthesis).
//
// The transform is the ordinary MDCT kernel cos[pi/NF (n + 1/2 + NF/2)(k + 1/2)] over 2 NF windowed
// samples, scaled by sqrt(2/NF); what makes it "low delay" is only the asymmetric window with its Z
// trailing zeros, which lives in the tables. Computing equation 8 directly costs 2 NF^2 products
// (460,800 at 48 kHz), so it is computed the usual fast way instead: the 2 NF inputs fold into NF
// values whose DCT-IV is the MDCT, and the DCT-IV of NF points is one complex FFT of NF/2 points
// between two twiddle multiplications. The synthesis is the transpose of the same steps.
//
// NF/2 is 30, 40, 60, 80, 90, 120, 160, 180 or 240 here, all products of 2, 3 and 5, so the FFT is a
// small mixed radix decimation in time FFT (radix 4, 2, 3 and 5 stages with a generic butterfly);
// its twiddles are computed once when the transform is set up. Everything is f64 and in fixed size
// arrays: the largest configuration (NF = 480) needs 240 point buffers. The work space lives in the
// struct, not on the stack, so a transform call needs little stack.

use crate::lc3::math;

/// Largest FFT: NF/2 for NF = 480.
const MAX_FFT: usize = 240;
/// Largest MDCT: NF.
pub(crate) const MAX_NF: usize = 480;

#[derive(Clone, Copy, Debug, PartialEq)]
struct C {
	re: f64,
	im: f64,
}

const ZERO: C = C { re: 0.0, im: 0.0 };

impl C {
	fn mul(self, o: C) -> C {
		C { re: self.re * o.re - self.im * o.im, im: self.re * o.im + self.im * o.re }
	}
	fn add(self, o: C) -> C {
		C { re: self.re + o.re, im: self.im + o.im }
	}
	/// e^(-i a).
	fn expi_neg(a: f64) -> C {
		let (s, c) = math::sin_cos(a);
		C { re: c, im: -s }
	}
}

/// Mixed radix FFT of a fixed size n <= MAX_FFT whose prime factors are 2, 3 and 5.
#[derive(Clone)]
struct Fft {
	n: usize,
	/// Stages as (radix p, remaining length m), outermost first; p * m of a stage is the length
	/// of the sub-transform at that depth.
	stages: [(usize, usize); 8],
	/// twiddle[k] = e^(-2 pi i k / n).
	twiddle: [C; MAX_FFT],
}

impl Fft {
	fn new(n: usize) -> Fft {
		let mut stages = [(1, 1); 8];
		let mut nstages = 0;
		let mut m = n;
		// Radix 4 first (it is still the generic butterfly, it just means fewer stages).
		for p in [4, 2, 3, 5] {
			while m.is_multiple_of(p) && m > 1 {
				m /= p;
				stages[nstages] = (p, m);
				nstages += 1;
			}
		}
		assert!(m == 1, "FFT size with a prime factor other than 2, 3 and 5");
		let mut twiddle = [ZERO; MAX_FFT];
		for (k, t) in twiddle.iter_mut().enumerate().take(n) {
			*t = C::expi_neg(2.0 * math::PI * k as f64 / n as f64);
		}
		Fft { n, stages, twiddle }
	}

	/// out = DFT(input), both of length n.
	fn run(&self, input: &[C], out: &mut [C]) {
		self.work(&mut out[..self.n], input, 0, 1, 0);
	}

	/// Decimation in time: `out` (length p m) receives the DFT of input[start + j stride],
	/// j = 0 .. p m - 1.
	fn work(&self, out: &mut [C], input: &[C], start: usize, stride: usize, stage: usize) {
		let (p, m) = self.stages[stage];
		if m == 1 {
			for (j, o) in out.iter_mut().enumerate().take(p) {
				*o = input[start + j * stride];
			}
		} else {
			for q in 0..p {
				self.work(&mut out[q * m..(q + 1) * m], input, start + q * stride, stride * p, stage + 1);
			}
		}
		// Combine the p interleaved sub-transforms of length m. The twiddle for output index k of
		// this stage and sub-transform q is e^(-2 pi i q k / (p m)) = twiddle[q k stride mod n].
		let mut scratch = [ZERO; 5];
		for u in 0..m {
			for q in 0..p {
				scratch[q] = out[u + q * m];
			}
			for q1 in 0..p {
				let k = u + q1 * m;
				let mut acc = scratch[0];
				let mut tw = 0;
				for s in scratch.iter().take(p).skip(1) {
					tw += stride * k;
					tw %= self.n;
					acc = acc.add(s.mul(self.twiddle[tw]));
				}
				out[k] = acc;
			}
		}
	}
}

/// The MDCT of one configuration: NF and its window.
#[derive(Clone)]
pub(crate) struct Mdct {
	nf: usize,
	window: &'static [f64],
	dct: Dct4,
	/// sqrt(2 / NF).
	scale: f64,
	/// Work space of inverse().
	u: [f64; MAX_NF],
}

/// The DCT-IV of NF points through an FFT of NF/2, with its twiddles and work space.
#[derive(Clone)]
struct Dct4 {
	nf: usize,
	fft: Fft,
	/// e^(-i pi n / NF), n < NF/2.
	pre: [C; MAX_FFT],
	/// e^(-i pi (n + 1/4) / NF), n < NF/2.
	post: [C; MAX_FFT],
	/// FFT input and output.
	z: [C; MAX_FFT],
	y: [C; MAX_FFT],
}

impl Dct4 {
	fn new(nf: usize) -> Dct4 {
		let m = nf / 2;
		let mut pre = [ZERO; MAX_FFT];
		let mut post = [ZERO; MAX_FFT];
		for n in 0..m {
			pre[n] = C::expi_neg(math::PI * n as f64 / nf as f64);
			post[n] = C::expi_neg(math::PI * (n as f64 + 0.25) / nf as f64);
		}
		Dct4 { nf, fft: Fft::new(m), pre, post, z: [ZERO; MAX_FFT], y: [ZERO; MAX_FFT] }
	}

	/// In place DCT-IV without scaling: v(k) <- sum_n v(n) cos(pi/NF (n + 1/2)(k + 1/2)).
	fn run(&mut self, v: &mut [f64]) {
		let nf = self.nf;
		let m = nf / 2;
		for n in 0..m {
			self.z[n] = C { re: v[2 * n], im: v[nf - 1 - 2 * n] }.mul(self.pre[n]);
		}
		self.fft.run(&self.z[..m], &mut self.y[..m]);
		for k in 0..m {
			let t = self.y[k].mul(self.post[k]);
			v[2 * k] = t.re;
			v[nf - 1 - 2 * k] = -t.im;
		}
	}
}

impl Mdct {
	pub(crate) fn new(nf: usize, window: &'static [f64]) -> Mdct {
		Mdct { nf, window, dct: Dct4::new(nf), scale: math::sqrt(2.0 / nf as f64), u: [0.0; MAX_NF] }
	}

	/// Equation 8: t holds the 2 NF samples t(n) of the time buffer; out receives X(k), k < NF.
	pub(crate) fn forward(&mut self, t: &[f64], out: &mut [f64]) {
		let nf = self.nf;
		let h = nf / 2;
		let w = self.window;
		// Fold the windowed input: with the four quarters a, b, c, d of the 2 NF samples, the MDCT
		// is the DCT-IV of (-c_reversed - d, a - b_reversed).
		for n in 0..h {
			out[n] = -w[3 * h - 1 - n] * t[3 * h - 1 - n] - w[3 * h + n] * t[3 * h + n];
			out[h + n] = w[n] * t[n] - w[nf - 1 - n] * t[nf - 1 - n];
		}
		self.dct.run(&mut out[..nf]);
		for x in out.iter_mut().take(nf) {
			*x *= self.scale;
		}
	}

	/// Equations 125 and 126: spec holds X(k), k < NF; out receives the windowed 2 NF samples t(n).
	pub(crate) fn inverse(&mut self, spec: &[f64], out: &mut [f64]) {
		let nf = self.nf;
		let h = nf / 2;
		let u = &mut self.u;
		u[..nf].copy_from_slice(&spec[..nf]);
		self.dct.run(&mut u[..nf]);
		// The transpose of the folding in forward().
		let w = self.window;
		for j in 0..2 * nf {
			let v = if j < h {
				u[j + h]
			} else if j < 3 * h {
				-u[3 * h - 1 - j]
			} else {
				-u[j - 3 * h]
			};
			out[j] = w[2 * nf - 1 - j] * v * self.scale;
		}
	}
}

#[cfg(test)]
mod tests {
	extern crate std;
	use super::*;
	use crate::lc3::tables;

	fn direct_forward(nf: usize, w: &[f64], t: &[f64]) -> std::vec::Vec<f64> {
		(0..nf)
			.map(|k| {
				let mut s = 0.0;
				for n in 0..2 * nf {
					s += w[n] * t[n] * std::primitive::f64::cos(std::f64::consts::PI / nf as f64 * (n as f64 + 0.5 + nf as f64 / 2.0) * (k as f64 + 0.5));
				}
				s * (2.0 / nf as f64).sqrt()
			})
			.collect()
	}

	fn direct_inverse(nf: usize, w: &[f64], x: &[f64]) -> std::vec::Vec<f64> {
		(0..2 * nf)
			.map(|n| {
				let mut s = 0.0;
				for (k, xk) in x.iter().enumerate().take(nf) {
					s += xk * std::primitive::f64::cos(std::f64::consts::PI / nf as f64 * (n as f64 + 0.5 + nf as f64 / 2.0) * (k as f64 + 0.5));
				}
				s * (2.0 / nf as f64).sqrt() * w[2 * nf - 1 - n]
			})
			.collect()
	}

	#[test]
	fn fft_matches_dft() {
		for n in [30, 40, 60, 80, 90, 120, 160, 180, 240] {
			let fft = Fft::new(n);
			let mut x = [ZERO; MAX_FFT];
			let mut seed = 1u32;
			for v in x.iter_mut().take(n) {
				seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
				v.re = (seed >> 8) as f64 / (1u32 << 24) as f64 - 0.5;
				seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
				v.im = (seed >> 8) as f64 / (1u32 << 24) as f64 - 0.5;
			}
			let mut y = [ZERO; MAX_FFT];
			fft.run(&x[..n], &mut y[..n]);
			for (k, yk) in y.iter().enumerate().take(n) {
				let mut re = 0.0;
				let mut im = 0.0;
				for (j, xj) in x.iter().enumerate().take(n) {
					let a = -2.0 * std::f64::consts::PI * (j * k % n) as f64 / n as f64;
					re += xj.re * a.cos() - xj.im * a.sin();
					im += xj.re * a.sin() + xj.im * a.cos();
				}
				assert!((re - yk.re).abs() < 1.0e-12 && (im - yk.im).abs() < 1.0e-12, "n {n} k {k}");
			}
		}
	}

	#[test]
	fn fast_transforms_match_the_equations() {
		let windows: [&'static [f64]; 10] = [
			&tables::W_10_80,
			&tables::W_10_160,
			&tables::W_10_240,
			&tables::W_10_320,
			&tables::W_10_480,
			&tables::W_7_5_60,
			&tables::W_7_5_120,
			&tables::W_7_5_180,
			&tables::W_7_5_240,
			&tables::W_7_5_360,
		];
		for w in windows {
			let nf = w.len() / 2;
			let mut mdct = Mdct::new(nf, w);
			let mut t = [0.0; 2 * MAX_NF];
			let mut seed = 7u32;
			for v in t.iter_mut().take(2 * nf) {
				seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
				*v = ((seed >> 8) as f64 / (1u32 << 24) as f64 - 0.5) * 65536.0;
			}
			let mut x = [0.0; MAX_NF];
			mdct.forward(&t, &mut x);
			let d = direct_forward(nf, w, &t[..2 * nf]);
			for k in 0..nf {
				assert!((x[k] - d[k]).abs() < 1.0e-8 * 65536.0, "nf {nf} k {k}: {} vs {}", x[k], d[k]);
			}
			let mut y = [0.0; 2 * MAX_NF];
			mdct.inverse(&x, &mut y);
			let d = direct_inverse(nf, w, &x[..nf]);
			for n in 0..2 * nf {
				assert!((y[n] - d[n]).abs() < 1.0e-8 * 65536.0, "nf {nf} n {n}");
			}
		}
	}
}
