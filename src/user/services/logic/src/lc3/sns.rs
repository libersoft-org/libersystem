// Spectral noise shaping (specification sections 3.3.7 and 3.4.7).
//
// SNS shapes the quantization noise along frequency with 16 scale factors per frame. The encoder
// derives them from the band energies (smoothing, pre-emphasis, a noise floor, log2, grouping by
// four, mean removal; attack frames are smoothed once more), quantizes them with a 38 bit two stage
// vector quantizer, and divides the spectrum by the interpolated quantized factors. The decoder
// rebuilds the same quantized factors from the 38 bits and multiplies the spectrum back.
//
// The vector quantizer: stage 1 is a split VQ, two 5 bit codebooks of 8 dimensions (LFCB, HFCB).
// Stage 2 rotates the stage 1 residual with the 16 point DCT matrix D and approximates it by a gain
// times a unit energy pyramid VQ shape, one of four shapes (table 3.8): 'regular' (10 pulses on the
// first 10 coefficients plus 1 pulse on the last 6), 'regular_lf' (the 10 pulses only), and the
// 'outlier' shapes with 8 or 6 pulses over all 16. The pulse search follows the strategy of table
// 3.9 (project onto the K = 6 pyramid, then add unit pulses up to each shape's K), the shape and
// gain are chosen by the smallest error in the rotated domain, and the shape is enumerated as MPVQ
// indices (mpvq.rs) that are packed with the shape and gain bits into the 38 bits of table 3.13.
//
// THE QUANTIZED SCALE FACTORS ARE DEFINED BY THE BITS. The encoder computes its scfQ with the same
// synthesis function the decoder uses on the decoded indices, so both shape the spectrum with
// identical factors; the search itself only affects quality, not interoperability.

use crate::lc3::bits::{SideReader, SideWriter};
use crate::lc3::math;
use crate::lc3::mpvq;
use crate::lc3::tables::{D, HFCB, LFCB, SNS_GAIN_LSB_BITS, SNS_GAIN_MSB_BITS, SNS_VQ_FAR_ADJ_GAINS, SNS_VQ_NEAR_ADJ_GAINS, SNS_VQ_REG_ADJ_GAINS, SNS_VQ_REG_LF_ADJ_GAINS};
use crate::lc3::{Config, Duration};

/// SZ_shapeA,0 (= SZ_shapeA,1): MPVQ indices of PVQ(10, 10).
const SZ_A0: u32 = 2_390_004;
/// SZ_shapeA,2: MPVQ indices of PVQ(16, 8).
const SZ_A2: u32 = 15_158_272;
/// SZ_shapeA,3: MPVQ indices of PVQ(16, 6).
const SZ_A3: u32 = 774_912;

/// The pulse configurations (N_A, K_A) of the four shapes; shape 0 also has PVQ(6, 1) in set B.
const SHAPE_PVQ: [(usize, usize); 4] = [(10, 10), (10, 10), (16, 8), (16, 6)];

fn gains(shape: usize) -> &'static [f64] {
	match shape {
		0 => &SNS_VQ_REG_ADJ_GAINS,
		1 => &SNS_VQ_REG_LF_ADJ_GAINS,
		2 => &SNS_VQ_NEAR_ADJ_GAINS,
		_ => &SNS_VQ_FAR_ADJ_GAINS,
	}
}

/// The transmitted SNS parameters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SnsIndices {
	pub(crate) ind_lf: u32,
	pub(crate) ind_hf: u32,
	/// shape_j, 0 ..= 3.
	pub(crate) shape: usize,
	/// gain_i.
	pub(crate) gain: usize,
	pub(crate) ls_a: u32,
	pub(crate) idx_a: u32,
	/// Set B of the 'regular' shape only.
	pub(crate) ls_b: u32,
	pub(crate) idx_b: u32,
}

/// Section 3.3.7.2: the 16 scale factors of a frame from its band energies E_B (cfg.nb of them).
pub(crate) fn analyze(cfg: &Config, eb_in: &[f64], attack: bool) -> [f64; 16] {
	// 3.3.7.2.1 Padding to 64 bands (only 7.5 ms at 8 kHz has fewer).
	let nb = cfg.nb;
	let mut eb = [0.0; 64];
	if nb < 64 {
		let n2 = 64 - nb;
		for i in 0..n2 {
			eb[2 * i] = eb_in[i];
			eb[2 * i + 1] = eb_in[i];
		}
		for i in 0..nb - n2 {
			eb[2 * n2 + i] = eb_in[n2 + i];
		}
	} else {
		eb.copy_from_slice(&eb_in[..64]);
	}
	// 3.3.7.2.2 Smoothing.
	let mut es = [0.0; 64];
	es[0] = 0.75 * eb[0] + 0.25 * eb[1];
	es[63] = 0.25 * eb[62] + 0.75 * eb[63];
	for b in 1..63 {
		es[b] = 0.25 * eb[b - 1] + 0.5 * eb[b] + 0.25 * eb[b + 1];
	}
	// 3.3.7.2.3 Pre-emphasis, g_tilt of table 3.7.
	let gtilt = [14.0, 18.0, 22.0, 26.0, 30.0][cfg.fs_ind];
	let mut ep = [0.0; 64];
	let mut sum = 0.0;
	for b in 0..64 {
		ep[b] = es[b] * math::pow10(b as f64 * gtilt / 630.0);
		sum += ep[b];
	}
	// 3.3.7.2.4 Noise floor at -40 dB, 3.3.7.2.5 logarithm.
	let floor = math::max(sum / 64.0 * 1.0e-4, math::exp2(-32.0));
	let mut el = [0.0; 64];
	for b in 0..64 {
		el[b] = math::log2(1.0e-31 + math::max(ep[b], floor)) / 2.0;
	}
	// 3.3.7.2.6 Grouping by four with the weights {1, 2, 3, 3, 2, 1} / 12.
	let w = [1.0 / 12.0, 2.0 / 12.0, 3.0 / 12.0, 3.0 / 12.0, 2.0 / 12.0, 1.0 / 12.0];
	let mut e4 = [0.0; 16];
	for (b2, e) in e4.iter_mut().enumerate() {
		let mut s = 0.0;
		for (k, wk) in w.iter().enumerate() {
			// Indices 4 b2 + k - 1 that fall outside 0 .. 63 repeat the edge bands.
			let i = (4 * b2 + k) as isize - 1;
			let i = i.clamp(0, 63) as usize;
			s += wk * el[i];
		}
		*e = s;
	}
	// 3.3.7.2.7 Mean removal and scaling, attack handling.
	let mean = e4.iter().sum::<f64>() / 16.0;
	let mut scf = [0.0; 16];
	for b in 0..16 {
		scf[b] = 0.85 * (e4[b] - mean);
	}
	if attack {
		let s0 = scf;
		let mut s1 = [0.0; 16];
		s1[0] = (s0[0] + s0[1] + s0[2]) / 3.0;
		s1[1] = (s0[0] + s0[1] + s0[2] + s0[3]) / 4.0;
		for n in 2..14 {
			s1[n] = (s0[n - 2] + s0[n - 1] + s0[n] + s0[n + 1] + s0[n + 2]) / 5.0;
		}
		s1[14] = (s0[12] + s0[13] + s0[14] + s0[15]) / 4.0;
		s1[15] = (s0[13] + s0[14] + s0[15]) / 3.0;
		let mean = s1.iter().sum::<f64>() / 16.0;
		let fatt = match cfg.dt {
			Duration::Ms10 => 0.5,
			Duration::Ms7_5 => 0.3,
		};
		for n in 0..16 {
			scf[n] = fatt * (s1[n] - mean);
		}
	}
	scf
}

/// Section 3.3.7.3: quantizes the scale factors; returns the indices and the quantized factors.
pub(crate) fn quantize(scf: &[f64; 16]) -> (SnsIndices, [f64; 16]) {
	let mut q = SnsIndices::default();
	// Stage 1 (3.3.7.3.2): the nearest codebook vector of each half, first one on ties.
	let nearest = |cb: &[[f64; 8]; 32], x: &[f64]| -> usize {
		let mut best = 0;
		let mut best_d = f64::INFINITY;
		for (i, row) in cb.iter().enumerate() {
			let mut d = 0.0;
			for n in 0..8 {
				let e = x[n] - row[n];
				d += e * e;
			}
			if d < best_d {
				best_d = d;
				best = i;
			}
		}
		best
	};
	let lf = nearest(&LFCB, &scf[..8]);
	let hf = nearest(&HFCB, &scf[8..]);
	q.ind_lf = lf as u32;
	q.ind_hf = hf as u32;
	let mut r1 = [0.0; 16];
	for n in 0..8 {
		r1[n] = scf[n] - LFCB[lf][n];
		r1[n + 8] = scf[n + 8] - HFCB[hf][n];
	}
	// Stage 2 target (equation 43): t2rot = D^T r1.
	let mut t = [0.0; 16];
	for n in 0..16 {
		let mut s = 0.0;
		for row in 0..16 {
			s += r1[row] * D[row][n];
		}
		t[n] = s;
	}
	let ys = search_shapes(&t);
	// Shape and gain with the smallest error (equations 55 and 56).
	let mut best = (0, 0);
	let mut best_d = f64::INFINITY;
	for (j, y) in ys.iter().enumerate() {
		let xq = normalize(y);
		for (i, &g) in gains(j).iter().enumerate() {
			let mut d = 0.0;
			for n in 0..16 {
				let e = t[n] - g * xq[n];
				d += e * e;
			}
			if d < best_d {
				best_d = d;
				best = (j, i);
			}
		}
	}
	let (shape, gain) = best;
	q.shape = shape;
	q.gain = gain;
	// Enumeration (table 3.12).
	let y = &ys[shape];
	let (na, _) = SHAPE_PVQ[shape];
	let (idx_a, ls_a) = mpvq::enumerate(na, &y[..na]);
	q.idx_a = idx_a;
	q.ls_a = ls_a;
	if shape == 0 {
		let (idx_b, ls_b) = mpvq::enumerate(6, &y[10..16]);
		q.idx_b = idx_b;
		q.ls_b = ls_b;
	}
	let scfq = synthesize(&q, y);
	(q, scfq)
}

/// The four candidate pulse vectors y0 .. y3 for the target t2rot, signs included (table 3.9).
fn search_shapes(t: &[f64; 16]) -> [[i32; 16]; 4] {
	let mut xm = [0.0; 16];
	let mut sum = 0.0;
	for n in 0..16 {
		xm[n] = math::abs(t[n]);
		sum += xm[n];
	}
	// Step 1: project onto or below the K = 6 pyramid (equations 53 and 54).
	let mut y = [0i32; 16];
	if sum > 0.0 {
		let proj = (6.0 - 1.0) / sum;
		for n in 0..16 {
			y[n] = math::floor(xm[n] * proj) as i32;
		}
	}
	let mut ys = [[0i32; 16]; 4];
	// Step 2: up to K = 6 over all 16 positions: y3.
	add_pulses(&xm, &mut y, 0, 16, 6);
	ys[3] = y;
	// Step 3: up to K = 8: y2.
	add_pulses(&xm, &mut y, 0, 16, 8);
	ys[2] = y;
	// Steps 4 to 6: drop the pulses outside set A, then up to K = 10 over set A: y1.
	for v in y.iter_mut().skip(10) {
		*v = 0;
	}
	add_pulses(&xm, &mut y, 0, 10, 10);
	ys[1] = y;
	// Step 7: one more pulse in set B: y0.
	add_pulses(&xm, &mut y, 10, 6, 11);
	ys[0] = y;
	// Step 8: the signs of the target.
	for yj in ys.iter_mut() {
		for n in 0..16 {
			if t[n] < 0.0 {
				yj[n] = -yj[n];
			}
		}
	}
	ys
}

/// Adds unit pulses to the non-negative vector y, at positions start .. start + len, until its
/// L1 norm is k, each one where it maximizes corr^2 / energy (equations 48 to 52).
fn add_pulses(xm: &[f64; 16], y: &mut [i32; 16], start: usize, len: usize, k: i32) {
	let mut corr = 0.0;
	let mut energy = 0.0;
	let mut pulses = 0;
	for n in 0..16 {
		corr += y[n] as f64 * xm[n];
		energy += (y[n] * y[n]) as f64;
		pulses += y[n];
	}
	while pulses < k {
		let mut best = start;
		let mut best_corr_sq = 0.0;
		let mut best_en = 1.0;
		for nc in start..start + len {
			let c = corr + xm[nc];
			let e = energy + 2.0 * y[nc] as f64 + 1.0;
			// Cross-multiplied comparison of c^2 / e with the best so far; the first position
			// is taken unconditionally (nbest starts at the first position).
			if nc == start || c * c * best_en > best_corr_sq * e {
				best = nc;
				best_corr_sq = c * c;
				best_en = e;
			}
		}
		corr += xm[best];
		energy += 2.0 * y[best] as f64 + 1.0;
		y[best] += 1;
		pulses += 1;
	}
}

/// Equation 44 over 16 dimensions: y / sqrt(y^T y).
fn normalize(y: &[i32; 16]) -> [f64; 16] {
	let e: i32 = y.iter().map(|v| v * v).sum();
	let mut x = [0.0; 16];
	if e > 0 {
		let s = 1.0 / math::sqrt(e as f64);
		for n in 0..16 {
			x[n] = y[n] as f64 * s;
		}
	}
	x
}

/// Equation 62: scfQ = st1 + G D xq, for the shape vector y of the indices.
fn synthesize(q: &SnsIndices, y: &[i32; 16]) -> [f64; 16] {
	let xq = normalize(y);
	let g = gains(q.shape)[q.gain];
	let mut scfq = [0.0; 16];
	for n in 0..16 {
		let mut s = 0.0;
		for col in 0..16 {
			s += xq[col] * D[n][col];
		}
		let st1 = if n < 8 { LFCB[q.ind_lf as usize][n] } else { HFCB[q.ind_hf as usize][n - 8] };
		scfq[n] = st1 + g * s;
	}
	scfq
}

/// Section 3.4.7.2: the quantized scale factors of decoded indices.
pub(crate) fn dequantize(q: &SnsIndices) -> [f64; 16] {
	let mut y = [0i32; 16];
	let (na, ka) = SHAPE_PVQ[q.shape];
	mpvq::deenumerate(na, ka, q.ls_a, q.idx_a, &mut y);
	if q.shape == 0 {
		let mut z = [0i32; 6];
		mpvq::deenumerate(6, 1, q.ls_b, q.idx_b, &mut z);
		y[10..16].copy_from_slice(&z);
	}
	synthesize(q, &y)
}

/// Writes the 38 SNS bits (the SCF VQ parts of section 3.3.13.3).
pub(crate) fn write(w: &mut SideWriter, bytes: &mut [u8], q: &SnsIndices) {
	w.uint(bytes, q.ind_lf, 5);
	w.uint(bytes, q.ind_hf, 5);
	let submode_msb = (q.shape >> 1) as u32;
	let submode_lsb = (q.shape & 1) as u32;
	w.bit(bytes, submode_msb);
	let gain = q.gain as u32;
	w.uint(bytes, gain >> SNS_GAIN_LSB_BITS[q.shape], SNS_GAIN_MSB_BITS[q.shape]);
	w.bit(bytes, q.ls_a);
	if submode_msb == 0 {
		let tmp = if submode_lsb == 0 {
			// Equation 58.
			(2 * q.idx_b + q.ls_b + 2) * SZ_A0 + q.idx_a
		} else {
			// Equation 59.
			(gain & 1) * SZ_A0 + q.idx_a
		};
		w.uint(bytes, tmp, 13);
		w.uint(bytes, tmp >> 13, 12);
	} else {
		let tmp = if submode_lsb == 0 {
			// Equation 60.
			q.idx_a
		} else {
			// Equation 61.
			SZ_A2 + (gain & 1) + 2 * q.idx_a
		};
		w.uint(bytes, tmp, 12);
		w.uint(bytes, tmp >> 12, 12);
	}
}

/// Reads the 38 SNS bits (sections 3.4.7.2.1 and 3.4.7.2.2); None is a bit error (BEC_detect).
pub(crate) fn read(r: &mut SideReader, bytes: &[u8]) -> Option<SnsIndices> {
	let mut q = SnsIndices { ind_lf: r.uint(bytes, 5), ind_hf: r.uint(bytes, 5), ..SnsIndices::default() };
	let submode_msb = r.bit(bytes);
	let mut gind = if submode_msb == 0 { r.uint(bytes, 1) } else { r.uint(bytes, 2) };
	q.ls_a = r.bit(bytes);
	let submode_lsb;
	if submode_msb == 0 {
		let mut tmp = r.uint(bytes, 13);
		tmp |= r.uint(bytes, 12) << 13;
		// dec_split_st2VQ_CW(tmp, 4780008 >> 1, 14).
		if tmp >= 14 * SZ_A0 {
			return None;
		}
		let mut idx_b_or_gain_lsb = (tmp / SZ_A0) as i32;
		q.idx_a = tmp - idx_b_or_gain_lsb as u32 * SZ_A0;
		idx_b_or_gain_lsb -= 2;
		submode_lsb = if idx_b_or_gain_lsb < 0 { 1 } else { 0 };
		idx_b_or_gain_lsb += 2 * submode_lsb;
		if submode_lsb != 0 {
			gind = (gind << 1) + idx_b_or_gain_lsb as u32;
		} else {
			q.idx_b = (idx_b_or_gain_lsb >> 1) as u32;
			q.ls_b = (idx_b_or_gain_lsb & 1) as u32;
		}
	} else {
		let mut tmp = r.uint(bytes, 12);
		tmp |= r.uint(bytes, 12) << 12;
		q.idx_a = tmp;
		if tmp >= SZ_A2 + 2 * SZ_A3 {
			return None;
		}
		if tmp >= SZ_A2 {
			let t = tmp - SZ_A2;
			submode_lsb = 1;
			gind = (gind << 1) + (t & 1);
			q.idx_a = t >> 1;
		} else {
			submode_lsb = 0;
		}
	}
	q.shape = ((submode_msb << 1) + submode_lsb as u32) as usize;
	q.gain = gind as usize;
	Some(q)
}

/// Sections 3.3.7.4 and 3.4.7.3: interpolation of the 16 factors to one per band, then the linear
/// gains, 2^-scfQint for the encoder (`inverse`) and 2^scfQint for the decoder.
pub(crate) fn interpolate(cfg: &Config, scfq: &[f64; 16], inverse: bool) -> [f64; 64] {
	let mut s = [0.0; 64];
	s[0] = scfq[0];
	s[1] = scfq[0];
	for n in 0..15 {
		let d = scfq[n + 1] - scfq[n];
		s[4 * n + 2] = scfq[n] + d / 8.0;
		s[4 * n + 3] = scfq[n] + 3.0 * d / 8.0;
		s[4 * n + 4] = scfq[n] + 5.0 * d / 8.0;
		s[4 * n + 5] = scfq[n] + 7.0 * d / 8.0;
	}
	s[62] = scfq[15] + (scfq[15] - scfq[14]) / 8.0;
	s[63] = scfq[15] + 3.0 * (scfq[15] - scfq[14]) / 8.0;
	let nb = cfg.nb;
	if nb < 64 {
		let n2 = 64 - nb;
		let mut tmp = [0.0; 64];
		for i in 0..n2 {
			tmp[i] = 0.5 * s[2 * i] + 0.5 * s[2 * i + 1];
		}
		tmp[n2..nb].copy_from_slice(&s[2 * n2..n2 + nb]);
		s[..nb].copy_from_slice(&tmp[..nb]);
	}
	let mut g = [0.0; 64];
	for b in 0..nb {
		g[b] = math::exp2(if inverse { -s[b] } else { s[b] });
	}
	g
}

#[cfg(test)]
mod tests {
	extern crate std;
	use super::*;

	fn random_scf(seed: &mut u32, spread: f64) -> [f64; 16] {
		let mut scf = [0.0; 16];
		for v in scf.iter_mut() {
			*seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
			*v = ((*seed >> 8) as f64 / (1u32 << 24) as f64 - 0.5) * spread;
		}
		scf
	}

	/// The decoder's dequantized scale factors are the encoder's quantized ones, through the
	/// actual 38 bit multiplexing, for every shape.
	#[test]
	fn quantized_factors_survive_the_bitstream() {
		let mut seed = 5u32;
		let mut shapes = [0; 4];
		for i in 0..20000 {
			let scf = random_scf(&mut seed, [1.0, 4.0, 8.0, 16.0][i % 4]);
			let (q, scfq) = quantize(&scf);
			shapes[q.shape] += 1;
			let mut bytes = [0u8; 8];
			let mut w = SideWriter::new(8);
			write(&mut w, &mut bytes, &q);
			assert_eq!(w.used(64), 38);
			let mut r = SideReader::new(8);
			let back = read(&mut r, &bytes).expect("valid SNS bits");
			assert_eq!(back, q);
			assert_eq!(dequantize(&back), scfq);
		}
		// All four shapes occur.
		assert!(shapes.iter().all(|&n| n > 100), "{shapes:?}");
	}

	#[test]
	fn invalid_joint_indices_are_bit_errors() {
		// 'regular' family: tmp >= 14 SZ_A0.
		let mut bytes = [0u8; 8];
		let mut w = SideWriter::new(8);
		w.uint(&mut bytes, 0, 10);
		w.bit(&mut bytes, 0);
		w.uint(&mut bytes, 0, 1);
		w.bit(&mut bytes, 0);
		w.uint(&mut bytes, 14 * SZ_A0, 13);
		w.uint(&mut bytes, (14 * SZ_A0) >> 13, 12);
		assert!(read(&mut SideReader::new(8), &bytes).is_none());
		// 'outlier' family: tmp >= SZ_A2 + 2 SZ_A3.
		let mut bytes = [0u8; 8];
		let mut w = SideWriter::new(8);
		w.uint(&mut bytes, 0, 10);
		w.bit(&mut bytes, 1);
		w.uint(&mut bytes, 0, 2);
		w.bit(&mut bytes, 0);
		w.uint(&mut bytes, SZ_A2 + 2 * SZ_A3, 12);
		w.uint(&mut bytes, (SZ_A2 + 2 * SZ_A3) >> 12, 12);
		assert!(read(&mut SideReader::new(8), &bytes).is_none());
	}
}
