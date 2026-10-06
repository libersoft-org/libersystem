//! LC3, THE LOW COMPLEXITY COMMUNICATION CODEC OF LE AUDIO - what the earbuds and the broadcasts this stack plays
//! carry - written from the Bluetooth SIG specification (LC3 v1.0) in plain Rust: no dependencies, no unsafe code,
//! no heap.
//!
//! HELD TO THE SPECIFICATION'S OWN DATA. The suites code Appendix C's reference frames byte for byte and check every
//! intermediate step of the encoder and the decoder against the appendix's values; the in-guest earbuds and broadcast
//! source read and write LC3 with a frame reader and writer of their own, written apart.
//!
//! One Encoder or Decoder codes one audio channel (stereo is two of each). Every configuration of
//! the specification is supported: 8, 16, 24, 32, 44.1 and 48 kHz (44.1 kHz runs the 48 kHz
//! configuration with fscal = 48000/44100), 10 ms and 7.5 ms frames, 20 to 400 bytes per frame, and
//! the frame size may change from one frame to the next (section 3.6, external rate adaptation).
//!
//! The signal processing is in f64, like the specification's own reference data; the bitstream is
//! what has to match other implementations bit for bit, and it is written and read exactly as the
//! pseudocode of sections 3.3.13 and 3.4.2 does. A frame that fails the decoder's bit error checks
//! is reported as Error::Corrupt and concealed; a missing frame is concealed. The concealment is the
//! specification's example packet loss concealment of appendix B (decoder.rs).
//!
//! The encoder and decoder keep all state in fixed size arrays sized for the largest configuration;
//! see Encoder and Decoder for their sizes. Module map, in the order of the specification:
//!   math      - sqrt, sin/cos, exp/log for no_std
//!   tables    - the constant tables of section 3.7 (extracted from the text)
//!   mdct      - the low delay MDCT and its inverse (3.3.4.3, 3.4.8)
//!   bits      - side information bit I/O and the range coder (3.3.13.6, 3.4.2.7)
//!   mpvq      - MPVQ enumeration of the SNS stage 2 shapes (3.3.7.3.3.8, 3.4.7.2.2.1)
//!   sns       - spectral noise shaping: analysis, vector quantization, synthesis (3.3.7, 3.4.7)
//!   tns       - temporal noise shaping (3.3.8, 3.4.6)
//!   ltpf      - long term postfilter: encoder pitch analysis and decoder filter (3.3.9, 3.4.9)
//!   encoder   - the rest of the encoder: band energies, bandwidth and attack detection, spectral
//!               quantization, noise level, frame writing (3.3)
//!   decoder   - frame reading, noise filling, synthesis, concealment (3.4, appendix B)

mod bits;
mod decoder;
mod encoder;
mod ltpf;
mod math;
mod mdct;
mod mpvq;
mod sns;
mod tables;
mod tns;

#[cfg(test)]
mod tests;

pub use decoder::Decoder;
pub use encoder::Encoder;

/// Smallest frame in bytes.
pub const MIN_BYTES: usize = 20;
/// Largest frame in bytes.
pub const MAX_BYTES: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
	/// The frame size is outside MIN_BYTES..=MAX_BYTES.
	Bytes,
	/// The PCM buffer does not hold exactly one frame.
	Samples,
	/// The frame failed the decoder's checks (a bit error, or not an LC3 frame of this
	/// configuration).
	Corrupt,
}

/// The frame duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Duration {
	Ms7_5,
	Ms10,
}

/// One codec configuration: sampling rate and frame duration, with everything derived from them.
#[derive(Debug, Clone, Copy)]
pub struct Config {
	sample_rate: u32,
	pub(crate) dt: Duration,
	/// The sampling rate index fs_ind of equation 1 (4 for 44.1 and 48 kHz).
	pub(crate) fs_ind: usize,
	/// NF: samples per frame.
	pub(crate) nf: usize,
	/// NE: encoded spectral lines.
	pub(crate) ne: usize,
	/// NB: number of bands.
	pub(crate) nb: usize,
	/// Z: the leading zeros of the MDCT window (equation 3), the last Z coefficients of the
	/// analysis window as tabulated.
	pub(crate) z: usize,
	/// The band limits I_fs, NB + 1 entries.
	pub(crate) bands: &'static [u16],
	/// The MDCT window, 2 NF entries.
	pub(crate) window: &'static [f64],
}

impl PartialEq for Config {
	fn eq(&self, other: &Config) -> bool {
		// Everything else derives from these two.
		self.sample_rate == other.sample_rate && self.dt == other.dt
	}
}

impl Eq for Config {}

impl Config {
	/// None for a rate or a duration the codec does not have. `frame_us` is 7500 or 10000.
	pub fn new(sample_rate: u32, frame_us: u32) -> Option<Config> {
		let fs_ind = match sample_rate {
			8000 => 0,
			16000 => 1,
			24000 => 2,
			32000 => 3,
			44100 | 48000 => 4,
			_ => return None,
		};
		let dt = match frame_us {
			7500 => Duration::Ms7_5,
			10000 => Duration::Ms10,
			_ => return None,
		};
		// NF = fs fscal Nms / 1000: 44.1 kHz takes the 48 kHz frame size.
		let rate = if sample_rate == 44100 { 48000 } else { sample_rate as usize };
		let (nf, bands, window): (usize, &'static [u16], &'static [f64]) = match (dt, fs_ind) {
			(Duration::Ms10, 0) => (80, &tables::I_10_8000, &tables::W_10_80),
			(Duration::Ms10, 1) => (160, &tables::I_10_16000, &tables::W_10_160),
			(Duration::Ms10, 2) => (240, &tables::I_10_24000, &tables::W_10_240),
			(Duration::Ms10, 3) => (320, &tables::I_10_32000, &tables::W_10_320),
			(Duration::Ms10, _) => (480, &tables::I_10_48000, &tables::W_10_480),
			(Duration::Ms7_5, 0) => (60, &tables::I_7_5_8000, &tables::W_7_5_60),
			(Duration::Ms7_5, 1) => (120, &tables::I_7_5_16000, &tables::W_7_5_120),
			(Duration::Ms7_5, 2) => (180, &tables::I_7_5_24000, &tables::W_7_5_180),
			(Duration::Ms7_5, 3) => (240, &tables::I_7_5_32000, &tables::W_7_5_240),
			(Duration::Ms7_5, _) => (360, &tables::I_7_5_48000, &tables::W_7_5_360),
		};
		debug_assert_eq!(nf, rate * if dt == Duration::Ms10 { 10 } else { 15 } / if dt == Duration::Ms10 { 1000 } else { 2000 });
		// Equation 9: 20 kHz is the highest coded frequency.
		let ne = match nf {
			480 => 400,
			360 => 300,
			_ => nf,
		};
		// Equation 3.
		let z = match dt {
			Duration::Ms10 => 3 * nf / 8,
			Duration::Ms7_5 => 7 * nf / 30,
		};
		Some(Config { sample_rate, dt, fs_ind, nf, ne, nb: bands.len() - 1, z, bands, window })
	}

	/// NF: PCM samples per frame.
	pub fn samples(&self) -> usize {
		self.nf
	}

	pub fn sample_rate(&self) -> u32 {
		self.sample_rate
	}

	pub fn frame_us(&self) -> u32 {
		match self.dt {
			Duration::Ms10 => 10000,
			Duration::Ms7_5 => 7500,
		}
	}

	/// The algorithmic delay in samples (the encoder's lookahead the decoder output trails by).
	///
	/// Feeding frame after frame through an Encoder and a Decoder, output sample n reproduces input
	/// sample n - delay_samples(): NF - 2 Z, 2.5 ms of 10 ms frames and 4 ms of 7.5 ms frames
	/// (scaled by 48000/44100 at 44.1 kHz). The specification's total algorithmic delay D of
	/// section 3.2.4 adds the frame itself: D = NF + delay_samples().
	pub fn delay_samples(&self) -> usize {
		self.nf - 2 * self.z
	}

	/// Nms in units of 1/10 ms (100 or 75); a few formulas scale with it.
	pub(crate) fn nms_x10(&self) -> usize {
		match self.dt {
			Duration::Ms10 => 100,
			Duration::Ms7_5 => 75,
		}
	}

	/// The bandwidth detector's number of bandwidths N_bw and its bit count nbits_bw (table 3.6).
	pub(crate) fn nbits_bw(&self) -> u32 {
		[0, 1, 2, 2, 3][self.fs_ind]
	}

	/// ceil(log2(NE / 2)): the size of the last non-zero tuple field.
	pub(crate) fn nbits_lastnz(&self) -> u32 {
		let half = (self.ne / 2) as u32;
		32 - (half - 1).leading_zeros()
	}

	/// bw_stop of tables 3.16 and 3.18 for a bandwidth index P_bw.
	pub(crate) fn bw_stop(&self, pbw: usize) -> usize {
		let stop = [80, 160, 240, 320, 400][pbw];
		match self.dt {
			Duration::Ms10 => stop,
			Duration::Ms7_5 => stop * 3 / 4,
		}
	}

	/// NF_start and NF_width of tables 3.17 and 3.19.
	pub(crate) fn nf_start_width(&self) -> (usize, usize) {
		match self.dt {
			Duration::Ms10 => (24, 3),
			Duration::Ms7_5 => (18, 2),
		}
	}

	/// The rateFlag of sections 3.3.10.4 and 3.4.2.2.
	pub(crate) fn rate_flag(&self, nbits: usize) -> usize {
		if nbits > 160 + self.fs_ind * 160 { 512 } else { 0 }
	}

	/// The global gain offset gg_off of equations 110 and 121.
	pub(crate) fn gg_off(&self, nbits: usize) -> i32 {
		let f = self.fs_ind as i32 + 1;
		-((nbits as i32 / (10 * f)).min(115)) - 105 - 5 * f
	}

	/// tns_lpc_weighting of equation 71.
	pub(crate) fn tns_lpc_weighting(&self, nbits: usize) -> usize {
		if nbits * 10 < 48 * self.nms_x10() { 1 } else { 0 }
	}
}
