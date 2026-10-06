// Temporal noise shaping (specification sections 3.3.8 and 3.4.6).
//
// TNS shapes the quantization noise in time inside a frame by filtering the MDCT spectrum along
// frequency: the encoder runs an LPC analysis over the spectrum of each TNS filter's range (one
// range up to SSWB, two from SWB on, table 3.15), turns the filter on when its prediction gain is
// worth it, quantizes its reflection coefficients in the arcsine domain to 17 levels and filters the
// spectrum with the quantized lattice; the decoder runs the inverse lattice. The coefficients are
// sent arithmetic coded (order, then one index per coefficient).
//
// THE DECODER FILTER IS ALWAYS STABLE. The reflection coefficients are sin(k pi/17) for
// k = -8 ..= 8, all of magnitude below one, so even a garbage frame cannot make it diverge.

use crate::lc3::math;
use crate::lc3::tables::{AC_TNS_COEF_BITS, AC_TNS_ORDER_BITS};
use crate::lc3::{Config, Duration};

/// The quantization step of the reflection coefficients in the arcsine domain: pi / 17.
const DELTA: f64 = math::PI / 17.0;

/// TNS side information of one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Tns {
	pub(crate) num_filters: usize,
	pub(crate) lpc_weighting: usize,
	/// rc_order(f): 0 for a filter that is off.
	pub(crate) order: [usize; 2],
	/// rc_i(k, f), 0 ..= 16 (8 is a zero coefficient).
	pub(crate) rc_i: [[usize; 8]; 2],
}

impl Default for Tns {
	/// No filter: every coefficient index at 8, the zero coefficient.
	fn default() -> Tns {
		Tns { num_filters: 0, lpc_weighting: 0, order: [0; 2], rc_i: [[8; 8]; 2] }
	}
}

/// The filter ranges of tables 3.15 and 3.20 for a bandwidth P_bw: (start_freq, stop_freq,
/// sub_start, sub_stop) per filter, and the number of filters.
struct Ranges {
	num: usize,
	start: [usize; 2],
	stop: [usize; 2],
	sub_start: [[usize; 3]; 2],
	sub_stop: [[usize; 3]; 2],
}

fn ranges(dt: Duration, pbw: usize) -> Ranges {
	match dt {
		Duration::Ms10 => match pbw {
			0 => Ranges { num: 1, start: [12, 0], stop: [80, 0], sub_start: [[12, 34, 57], [0; 3]], sub_stop: [[34, 57, 80], [0; 3]] },
			1 => Ranges { num: 1, start: [12, 0], stop: [160, 0], sub_start: [[12, 61, 110], [0; 3]], sub_stop: [[61, 110, 160], [0; 3]] },
			2 => Ranges { num: 1, start: [12, 0], stop: [240, 0], sub_start: [[12, 88, 164], [0; 3]], sub_stop: [[88, 164, 240], [0; 3]] },
			3 => Ranges { num: 2, start: [12, 160], stop: [160, 320], sub_start: [[12, 61, 110], [160, 213, 266]], sub_stop: [[61, 110, 160], [213, 266, 320]] },
			_ => Ranges { num: 2, start: [12, 200], stop: [200, 400], sub_start: [[12, 74, 137], [200, 266, 333]], sub_stop: [[74, 137, 200], [266, 333, 400]] },
		},
		Duration::Ms7_5 => match pbw {
			0 => Ranges { num: 1, start: [9, 0], stop: [60, 0], sub_start: [[9, 26, 43], [0; 3]], sub_stop: [[26, 43, 60], [0; 3]] },
			1 => Ranges { num: 1, start: [9, 0], stop: [120, 0], sub_start: [[9, 46, 83], [0; 3]], sub_stop: [[46, 83, 120], [0; 3]] },
			2 => Ranges { num: 1, start: [9, 0], stop: [180, 0], sub_start: [[9, 66, 123], [0; 3]], sub_stop: [[66, 123, 180], [0; 3]] },
			3 => Ranges { num: 2, start: [9, 120], stop: [120, 240], sub_start: [[9, 46, 82], [120, 159, 200]], sub_stop: [[46, 82, 120], [159, 200, 240]] },
			_ => Ranges { num: 2, start: [9, 150], stop: [150, 300], sub_start: [[9, 56, 103], [150, 200, 250]], sub_stop: [[56, 103, 150], [200, 250, 300]] },
		},
	}
}

/// The number of TNS filters for a bandwidth (section 3.4.2.3: one below SWB, two from SWB on).
pub(crate) fn num_filters(pbw: usize) -> usize {
	if pbw < 3 { 1 } else { 2 }
}

/// rc_q of equations 74 and 122.
fn rcq(i: usize) -> f64 {
	math::sin(DELTA * (i as f64 - 8.0))
}

/// Sections 3.3.8.2 and 3.3.8.3: analysis and quantization. `xs` is the shaped spectrum X_s.
pub(crate) fn analyze(cfg: &Config, xs: &[f64], pbw: usize, nbits: usize, near_nyquist: bool) -> Tns {
	let r = ranges(cfg.dt, pbw);
	let mut tns = Tns { num_filters: r.num, lpc_weighting: cfg.tns_lpc_weighting(nbits), ..Tns::default() };
	for f in 0..r.num {
		// Normalized autocorrelation (equations 65 to 67).
		let mut rr = [0.0; 9];
		let mut e = [0.0; 3];
		for (s, es) in e.iter_mut().enumerate() {
			for v in &xs[r.sub_start[f][s]..r.sub_stop[f][s]] {
				*es += v * v;
			}
		}
		if e[0] * e[1] * e[2] == 0.0 {
			rr[0] = 3.0;
		} else {
			for (k, rk) in rr.iter_mut().enumerate() {
				let mut sum = 0.0;
				for (s, es) in e.iter().enumerate() {
					let mut c = 0.0;
					let stop = r.sub_stop[f][s] - k;
					for n in r.sub_start[f][s]..stop {
						c += xs[n] * xs[n + k];
					}
					sum += c / es;
				}
				*rk = sum;
			}
		}
		// Lag window (equation 68).
		for (k, rk) in rr.iter_mut().enumerate() {
			let a = 0.02 * math::PI * k as f64;
			*rk *= math::exp(-0.5 * a * a);
		}
		// Levinson-Durbin.
		let mut a = [0.0; 9];
		a[0] = 1.0;
		let mut err = rr[0];
		for k in 1..9 {
			let mut s = 0.0;
			for n in 0..k {
				s += a[n] * rr[k - n];
			}
			let rc = if err > 0.0 { -s / err } else { 0.0 };
			let mut tmp = [0.0; 9];
			tmp[0] = 1.0;
			for n in 1..k {
				tmp[n] = a[n] + rc * a[k - n];
			}
			tmp[k] = rc;
			a[..=k].copy_from_slice(&tmp[..=k]);
			err *= 1.0 - rc * rc;
		}
		let pred_gain = if err > 0.0 { rr[0] / err } else { f64::INFINITY };
		let mut rc = [0.0; 8];
		if pred_gain > 1.5 && !near_nyquist {
			// Weighting (equations 70 and 72).
			let gamma = if tns.lpc_weighting == 1 && pred_gain < 2.0 { 1.0 - (1.0 - 0.85) * (2.0 - pred_gain) / (2.0 - 1.5) } else { 1.0 };
			let mut t1 = [0.0; 9];
			let mut g = 1.0;
			for k in 0..9 {
				t1[k] = g * a[k];
				g *= gamma;
			}
			// LPC to reflection coefficients.
			for k in (1..9).rev() {
				rc[k - 1] = t1[k];
				let e = 1.0 - rc[k - 1] * rc[k - 1];
				let mut t2 = [0.0; 9];
				for n in 1..k {
					t2[n] = if e > 0.0 { (t1[n] - rc[k - 1] * t1[k - n]) / e } else { 0.0 };
				}
				t1[1..k].copy_from_slice(&t2[1..k]);
			}
		}
		// Quantization (equation 73) and order.
		let mut order = 0;
		for (k, (&c, ri)) in rc.iter().zip(tns.rc_i[f].iter_mut()).enumerate() {
			let i = math::nint(math::asin(c) / DELTA) + 8;
			let i = i.clamp(0, 16) as usize;
			*ri = i;
			if i != 8 {
				order = k + 1;
			}
		}
		tns.order[f] = order;
	}
	tns
}

/// nbits_TNS of equation 75.
pub(crate) fn nbits(tns: &Tns) -> usize {
	let mut total = 0;
	for f in 0..tns.num_filters {
		let order = tns.order[f];
		let mut n = 2048;
		if order > 0 {
			n += AC_TNS_ORDER_BITS[tns.lpc_weighting][order - 1] as usize;
			for k in 0..order {
				n += AC_TNS_COEF_BITS[k][tns.rc_i[f][k]] as usize;
			}
		}
		total += n.div_ceil(2048);
	}
	total
}

/// Section 3.3.8.4: the analysis lattice filter, in place over the spectrum.
pub(crate) fn filter(cfg: &Config, x: &mut [f64], pbw: usize, tns: &Tns) {
	let r = ranges(cfg.dt, pbw);
	let mut st = [0.0; 8];
	for f in 0..tns.num_filters {
		let order = tns.order[f];
		if order == 0 {
			continue;
		}
		let mut q = [0.0; 8];
		for (qk, &i) in q.iter_mut().zip(&tns.rc_i[f][..order]) {
			*qk = rcq(i);
		}
		for xn in &mut x[r.start[f]..r.stop[f]] {
			let mut t = *xn;
			let mut st_save = t;
			for k in 0..order - 1 {
				let st_tmp = q[k] * t + st[k];
				t += q[k] * st[k];
				st[k] = st_save;
				st_save = st_tmp;
			}
			t += q[order - 1] * st[order - 1];
			st[order - 1] = st_save;
			*xn = t;
		}
	}
}

/// Section 3.4.6: the synthesis lattice filter, in place over the spectrum.
pub(crate) fn synthesize(cfg: &Config, x: &mut [f64], pbw: usize, tns: &Tns) {
	let r = ranges(cfg.dt, pbw);
	let mut s = [0.0; 8];
	for f in 0..tns.num_filters {
		let order = tns.order[f];
		if order == 0 {
			continue;
		}
		let mut q = [0.0; 8];
		for (qk, &i) in q.iter_mut().zip(&tns.rc_i[f][..order]) {
			*qk = rcq(i);
		}
		for xn in &mut x[r.start[f]..r.stop[f]] {
			let mut t = *xn - q[order - 1] * s[order - 1];
			for k in (0..order - 1).rev() {
				t -= q[k] * s[k];
				s[k + 1] = q[k] * t + s[k];
			}
			*xn = t;
			s[0] = t;
		}
	}
}

#[cfg(test)]
mod tests {
	extern crate std;
	use super::*;

	/// The decoder's lattice inverts the encoder's for random coefficients, two filters too.
	#[test]
	fn synthesis_inverts_analysis() {
		let mut seed = 3u32;
		let mut rand = move |m: u32| {
			seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
			(seed >> 8) % m
		};
		for dt in [Duration::Ms10, Duration::Ms7_5] {
			let cfg = Config::new(48000, if dt == Duration::Ms10 { 10000 } else { 7500 }).unwrap();
			for pbw in 0..5 {
				let mut tns = Tns { num_filters: num_filters(pbw), ..Tns::default() };
				for f in 0..tns.num_filters {
					tns.order[f] = rand(9) as usize;
					for k in 0..8 {
						tns.rc_i[f][k] = if k < tns.order[f] { rand(17) as usize } else { 8 };
					}
				}
				let mut x = [0.0; 400];
				for v in x.iter_mut() {
					*v = rand(20001) as f64 - 10000.0;
				}
				let orig = x;
				filter(&cfg, &mut x, pbw, &tns);
				synthesize(&cfg, &mut x, pbw, &tns);
				for n in 0..400 {
					assert!((x[n] - orig[n]).abs() < 1.0e-6, "pbw {pbw} n {n}");
				}
			}
		}
	}
}
