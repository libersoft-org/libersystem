// The LC3 decoder (specification section 3.4) and its packet loss concealment (appendix B).
//
// A frame is read in the order of section 3.4.2: side information from the end of the frame
// backwards, then the range coded TNS parameters and spectrum from the start forwards (with the
// signs and low bit planes from the side stream), then the residual bits. The spectrum is then
// rebuilt: residual refinement (3.4.3), noise filling (3.4.4), global gain (3.4.5), TNS synthesis
// (3.4.6), SNS shaping (3.4.7), the inverse LD-MDCT with overlap-add (3.4.8), the long term
// postfilter (3.4.9) and rounding to 16 bits (3.4.10).
//
// THE FRAME IS UNTRUSTED. Every bit error condition of section 3.4.2 (BEC_detect) is checked, plus
// reads past either end of the frame; any of them rejects the frame before any state changes, and
// the frame is concealed instead. No input can make the decoder index out of bounds or loop
// without end: every loop is bounded by the frame or the spectrum size.
//
// Concealment (lost frames, and frames rejected as corrupt) is the specification's example of
// appendix B.3: the last good frame's shaped spectrum with randomly flipped signs (a 16 bit LCG
// seeded with 24607), at full level for the first three lost frames, then attenuated by 0.9 per
// frame up to the seventh and by 0.85 per frame after that, through the normal inverse MDCT and
// overlap-add; the postfilter is only allowed to fade out (cases 1 and 3 of section 3.4.9.2). Before
// any good frame the concealment is silence.

use crate::lc3::bits::{AcDecoder, SideReader};
use crate::lc3::encoder::next_context;
use crate::lc3::ltpf::LtpfDecoder;
use crate::lc3::math;
use crate::lc3::mdct::{MAX_NF, Mdct};
use crate::lc3::sns::{self, SnsIndices};
use crate::lc3::tables::{AC_SPEC_CUMFREQ, AC_SPEC_FREQ, AC_SPEC_LOOKUP, AC_TNS_COEF_CUMFREQ, AC_TNS_COEF_FREQ, AC_TNS_ORDER_CUMFREQ, AC_TNS_ORDER_FREQ};
use crate::lc3::tns::{self, Tns};
use crate::lc3::{Config, Error, MAX_BYTES, MIN_BYTES};

const MAX_NE: usize = 400;

/// An LC3 decoder for one channel.
///
/// The struct holds its state and its large work buffers in fixed arrays sized for 48 kHz, about
/// 55 KB; box it where stack space is scarce. A decode() call itself needs about 32 KB of stack.
#[derive(Clone)]
pub struct Decoder {
	cfg: Config,
	mdct: Mdct,
	/// mem_ola_add: NF - Z samples.
	ola: [f64; MAX_NF],
	/// Work space: the inverse MDCT output t^(n), 2 NF samples.
	t: [f64; 2 * MAX_NF],
	ltpf: LtpfDecoder,
	/// X^_lastGood for the concealment.
	last_good: [f64; MAX_NF],
	plc_seed: u32,
	/// Consecutive lost frames, and the attenuation of the last one.
	lost: u32,
	alpha: f64,
}

/// Everything read from a frame.
pub(crate) struct Parsed {
	pub(crate) pbw: usize,
	pub(crate) gg_ind: i32,
	pub(crate) tns: Tns,
	pub(crate) sns: SnsIndices,
	pub(crate) ltpf_active: bool,
	pub(crate) pitch_index: u32,
	pub(crate) fnf: u32,
	pub(crate) lsb_mode: bool,
	pub(crate) xq: [i32; MAX_NE],
	pub(crate) res_bits: [u8; MAX_NE],
	pub(crate) nres: usize,
	pub(crate) nf_seed: u32,
	pub(crate) zero_frame: bool,
}

impl Decoder {
	pub fn new(config: Config) -> Decoder {
		Decoder { cfg: config, mdct: Mdct::new(config.nf, config.window), ola: [0.0; MAX_NF], t: [0.0; 2 * MAX_NF], ltpf: LtpfDecoder::new(), last_good: [0.0; MAX_NF], plc_seed: 24607, lost: 0, alpha: 1.0 }
	}

	/// One frame of `frame.len()` bytes (20..=400), or None for a lost frame; `pcm` is exactly NF
	/// samples.
	///
	/// A lost frame is concealed. A frame that fails the decoder's checks is concealed the same
	/// way, `pcm` holds the concealment, and the result is Err(Corrupt). On Err(Bytes) and
	/// Err(Samples) nothing is decoded and the decoder state is unchanged.
	pub fn decode(&mut self, frame: Option<&[u8]>, pcm: &mut [i16]) -> Result<(), Error> {
		if pcm.len() != self.cfg.nf {
			return Err(Error::Samples);
		}
		let bytes = match frame {
			None => {
				self.conceal(pcm);
				return Ok(());
			}
			Some(b) => b,
		};
		if !(MIN_BYTES..=MAX_BYTES).contains(&bytes.len()) {
			return Err(Error::Bytes);
		}
		match self.parse(bytes) {
			Some(p) => {
				self.reconstruct(&p, 8 * bytes.len(), pcm);
				Ok(())
			}
			None => {
				self.conceal(pcm);
				Err(Error::Corrupt)
			}
		}
	}

	/// Section 3.4.2; None on any bit error condition.
	pub(crate) fn parse(&self, bytes: &[u8]) -> Option<Parsed> {
		let cfg = &self.cfg;
		let ne = cfg.ne;
		let nbits = 8 * bytes.len();
		let mut r = SideReader::new(bytes.len());

		// 3.4.2.3 Side information.
		let pbw = if cfg.nbits_bw() > 0 { r.uint(bytes, cfg.nbits_bw()) as usize } else { 0 };
		if pbw > cfg.fs_ind {
			return None;
		}
		let lastnz = ((r.uint(bytes, cfg.nbits_lastnz()) + 1) << 1) as usize;
		if lastnz > ne {
			return None;
		}
		let lsb_mode = r.bit(bytes) != 0;
		let gg_ind = r.uint(bytes, 8) as i32;
		let mut tns = Tns { num_filters: tns::num_filters(pbw), lpc_weighting: cfg.tns_lpc_weighting(nbits), ..Tns::default() };
		for f in 0..tns.num_filters {
			tns.order[f] = r.bit(bytes) as usize;
		}
		let pitch_present = r.bit(bytes) != 0;
		let sns = sns::read(&mut r, bytes)?;
		let (ltpf_active, pitch_index) = if pitch_present {
			let a = r.uint(bytes, 1) != 0;
			(a, r.uint(bytes, 9))
		} else {
			(false, 0)
		};
		let fnf = r.uint(bytes, 3);

		// 3.4.2.5 Arithmetic decoding.
		let mut ac = AcDecoder::new(bytes);
		for f in 0..tns.num_filters {
			if tns.order[f] > 0 {
				let w = tns.lpc_weighting;
				tns.order[f] = ac.decode(bytes, &AC_TNS_ORDER_CUMFREQ[w], &AC_TNS_ORDER_FREQ[w], 8) + 1;
				for k in 0..tns.order[f] {
					tns.rc_i[f][k] = ac.decode(bytes, &AC_TNS_COEF_CUMFREQ[k], &AC_TNS_COEF_FREQ[k], 17);
				}
			}
		}
		let rate_flag = cfg.rate_flag(nbits);
		let mut xq = [0i32; MAX_NE];
		let mut save_lev = [0u8; MAX_NE / 2];
		let mut c = 0;
		let mut k = 0;
		while k < lastnz {
			let mut t = c + rate_flag;
			if k > ne / 2 {
				t += 256;
			}
			let mut lev = 0;
			let mut sym = 16;
			while lev < 14 {
				let pki = AC_SPEC_LOOKUP[t + lev.min(3) * 1024] as usize;
				sym = ac.decode(bytes, &AC_SPEC_CUMFREQ[pki], &AC_SPEC_FREQ[pki], 17);
				if sym < 16 {
					break;
				}
				if !lsb_mode || lev > 0 {
					xq[k] += (r.bit(bytes) << lev) as i32;
					xq[k + 1] += (r.bit(bytes) << lev) as i32;
				}
				lev += 1;
			}
			if lev == 14 {
				return None;
			}
			if lsb_mode {
				save_lev[k / 2] = lev as u8;
			}
			let a = (sym & 3) as u32;
			let b = (sym >> 2) as u32;
			xq[k] += (a << lev) as i32;
			xq[k + 1] += (b << lev) as i32;
			if xq[k] > 0 && r.bit(bytes) == 1 {
				xq[k] = -xq[k];
			}
			if xq[k + 1] > 0 && r.bit(bytes) == 1 {
				xq[k + 1] = -xq[k + 1];
			}
			c = next_context(c, a, b, lev);
			if ac.bp as isize - r.bp > 3 {
				return None;
			}
			k += 2;
		}
		if ac.error || ac.overrun || r.overrun {
			return None;
		}

		// 3.4.2.6 Residual data.
		let nbits_residual = nbits as i32 - (r.used(nbits as i32) + ac.used());
		if nbits_residual < 0 {
			return None;
		}
		let mut nbits_residual = nbits_residual as usize;
		let mut res_bits = [0u8; MAX_NE];
		let mut nres = 0;
		if !lsb_mode {
			for &v in &xq[..ne] {
				if v != 0 {
					if nres == nbits_residual {
						break;
					}
					res_bits[nres] = r.bit(bytes) as u8;
					nres += 1;
				}
			}
		} else {
			// The first bit plane, sent last (section 3.3.13.4.2, lsbMode).
			let mut k = 0;
			'outer: while k < lastnz {
				if save_lev[k / 2] > 0 {
					for v in &mut xq[k..k + 2] {
						if nbits_residual == 0 {
							break 'outer;
						}
						let bit = r.bit(bytes);
						nbits_residual -= 1;
						if bit == 1 {
							if *v > 0 {
								*v += 1;
							} else if *v < 0 {
								*v -= 1;
							} else {
								if nbits_residual == 0 {
									break 'outer;
								}
								let sign = r.bit(bytes);
								nbits_residual -= 1;
								*v = if sign == 0 { 1 } else { -1 };
							}
						}
					}
				}
				k += 2;
			}
		}
		if r.overrun {
			return None;
		}
		let mut tmp: u32 = 0;
		for (k, &v) in xq.iter().enumerate().take(ne) {
			tmp = tmp.wrapping_add(v.unsigned_abs().wrapping_mul(k as u32));
		}
		let zero_frame = lastnz == 2 && xq[0] == 0 && xq[1] == 0 && gg_ind == 0 && fnf == 7;
		Some(Parsed { pbw, gg_ind, tns, sns, ltpf_active, pitch_index, fnf, lsb_mode, xq, res_bits, nres, nf_seed: tmp & 0xffff, zero_frame })
	}

	/// Sections 3.4.3 to 3.4.10 for a good frame.
	fn reconstruct(&mut self, p: &Parsed, nbits: usize, pcm: &mut [i16]) {
		let x = spectrum(&self.cfg, p, nbits);
		self.last_good = x;
		self.lost = 0;
		let y = self.synthesize(&x, p.ltpf_active, p.pitch_index, nbits);
		output(&y, pcm);
	}

	/// Appendix B.3.
	fn conceal(&mut self, pcm: &mut [i16]) {
		self.lost += 1;
		let prev = if self.lost == 1 { 1.0 } else { self.alpha };
		self.alpha = if self.lost < 4 {
			prev
		} else if self.lost < 8 {
			0.9 * prev
		} else {
			0.85 * prev
		};
		let mut x = [0.0; MAX_NF];
		for (k, v) in x.iter_mut().enumerate().take(self.cfg.nf) {
			self.plc_seed = (16831 + self.plc_seed * 12821) & 0xffff;
			let s = self.last_good[k] * self.alpha;
			*v = if self.plc_seed < 0x8000 { s } else { -s };
		}
		let y = self.synthesize(&x, false, 0, 0);
		output(&y, pcm);
	}

	/// Sections 3.4.8 and 3.4.9: inverse MDCT, overlap-add and postfilter of one spectrum.
	pub(crate) fn synthesize(&mut self, spec: &[f64; MAX_NF], ltpf_active: bool, pitch_index: u32, nbits: usize) -> [f64; MAX_NF] {
		let cfg = self.cfg;
		let (nf, z) = (cfg.nf, cfg.z);
		let t = &mut self.t;
		self.mdct.inverse(spec, t);
		let mut x = [0.0; MAX_NF];
		for n in 0..nf - z {
			x[n] = self.ola[n] + t[z + n];
		}
		x[nf - z..nf].copy_from_slice(&t[nf..nf + z]);
		self.ola[..nf - z].copy_from_slice(&t[nf + z..2 * nf]);
		self.ltpf.run(&cfg, &mut x, ltpf_active, pitch_index, nbits);
		x
	}
}

/// Section 3.4.10: clipping to 16 bits, then nint() (the signal is already in 16-bit scale).
fn output(x: &[f64; MAX_NF], pcm: &mut [i16]) {
	for (o, &v) in pcm.iter_mut().zip(x) {
		*o = math::round(math::max(-32768.0, math::min(32767.0, v))) as i16;
	}
}

/// Sections 3.4.3 to 3.4.7: the shaped spectrum X^ of a parsed frame (zero above NE).
pub(crate) fn spectrum(cfg: &Config, p: &Parsed, nbits: usize) -> [f64; MAX_NF] {
	let ne = cfg.ne;
	let mut x = [0.0; MAX_NF];
	for (v, &q) in x.iter_mut().zip(&p.xq[..ne]) {
		*v = q as f64;
	}
	// 3.4.3 Residual decoding.
	if !p.lsb_mode {
		let mut n = 0;
		let mut k = 0;
		while k < ne && n < p.nres {
			if p.xq[k] != 0 {
				let up = p.res_bits[n] == 1;
				n += 1;
				x[k] += match (p.xq[k] > 0, up) {
					(true, false) => -0.1875,
					(false, false) => -0.3125,
					(true, true) => 0.3125,
					(false, true) => 0.1875,
				};
			}
			k += 1;
		}
	}
	// 3.4.4 Noise filling, on the lines that are zero with all their neighbours.
	if !p.zero_frame {
		let bw_stop = cfg.bw_stop(p.pbw);
		let (start, width) = cfg.nf_start_width();
		let lnf = (8.0 - p.fnf as f64) / 16.0;
		let mut seed = p.nf_seed;
		for (k, v) in x.iter_mut().enumerate().take(bw_stop).skip(start) {
			let hi = (bw_stop - 1).min(k + width);
			if p.xq[k - width..=hi].iter().all(|&q| q == 0) {
				seed = (13849 + seed * 31821) & 0xffff;
				*v = if seed < 0x8000 { lnf } else { -lnf };
			}
		}
	}
	// 3.4.5 Global gain.
	let g = math::pow10((p.gg_ind + cfg.gg_off(nbits)) as f64 / 28.0);
	for v in &mut x[..ne] {
		*v *= g;
	}
	// 3.4.6 TNS.
	tns::synthesize(cfg, &mut x, p.pbw, &p.tns);
	// 3.4.7 SNS.
	let scfq = sns::dequantize(&p.sns);
	let gs = sns::interpolate(cfg, &scfq, false);
	for b in 0..cfg.nb {
		for v in &mut x[cfg.bands[b] as usize..cfg.bands[b + 1] as usize] {
			*v *= gs[b];
		}
	}
	x
}
