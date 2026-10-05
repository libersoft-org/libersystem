//! SBC, THE CODEC EVERY A2DP DEVICE HAS - its encoder and decoder, written from the A2DP specification's own definition
//! (its Appendix B: the frame, the bit allocation, the quantization, and the analysis and synthesis filter banks with
//! their prototype filters), in fixed point.
//!
//! THE FRAME. A sync word, the configuration (sampling frequency, blocks, channel mode, allocation method, subbands),
//! the bitpool, an 8-bit CRC over the configuration, the joint-stereo flags and the scale factors, then the quantized
//! subband samples block by block and channel by channel, padded to a byte.
//!
//! THE ARITHMETIC. Subband samples are carried in Q12 of the 16-bit sample domain the specification's scale factors are
//! defined in: a scale factor `n` bounds a subband sample below 2^(n+1). The prototype filters are the specification's
//! tables in Q31 and the cosine matrices are `cos(m * pi / 16)` in Q30, both converted when this crate is compiled;
//! every product is accumulated in 64 bits. The allocation is the specification's integer algorithm exactly - it must
//! be, because the decoder repeats it from the scale factors to know how many bits each sample took.
//!
//! mSBC, the wideband speech variant HFP carries, is the same codec at one fixed configuration - 16 kHz mono, 15
//! blocks, 8 subbands, loudness, bitpool 26 - behind its own sync word (`MSBC_SYNC`), and is reached through
//! `Config::MSBC`.

/// The sync word of an A2DP frame, and of an mSBC frame.
pub const SYNC: u8 = 0x9c;
pub const MSBC_SYNC: u8 = 0xad;

/// The most a frame can be: 16 blocks of 8 subbands in two channels at the highest bitpool, with its header.
pub const MAX_FRAME_BYTES: usize = 4 + 8 + 2 + (16usize * 2 * 250).div_ceil(8);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
	Mono,
	DualChannel,
	Stereo,
	JointStereo,
}

impl Mode {
	pub const fn channels(self) -> usize {
		match self {
			Mode::Mono => 1,
			_ => 2,
		}
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Allocation {
	Loudness,
	Snr,
}

/// ONE STREAM'S CONFIGURATION: everything a frame header carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
	/// 16000, 32000, 44100 or 48000.
	pub frequency: u32,
	/// 4, 8, 12 or 16; an mSBC frame's 15.
	pub blocks: u8,
	pub mode: Mode,
	pub allocation: Allocation,
	/// 4 or 8.
	pub subbands: u8,
	pub bitpool: u8,
}

impl Config {
	/// THE WIDEBAND SPEECH CONFIGURATION HFP's mSBC fixes.
	pub const MSBC: Config = Config { frequency: 16_000, blocks: 15, mode: Mode::Mono, allocation: Allocation::Loudness, subbands: 8, bitpool: 26 };

	/// Whether every field is one the specification defines, and the bitpool within its bounds for the mode.
	pub fn valid(&self) -> bool {
		let msbc = *self == Config::MSBC;
		let frequency = matches!(self.frequency, 16_000 | 32_000 | 44_100 | 48_000);
		let blocks = matches!(self.blocks, 4 | 8 | 12 | 16) || msbc;
		let subbands = matches!(self.subbands, 4 | 8);
		let most = match self.mode {
			Mode::Mono | Mode::DualChannel => 16 * u32::from(self.subbands),
			Mode::Stereo | Mode::JointStereo => 32 * u32::from(self.subbands),
		};
		frequency && blocks && subbands && self.bitpool >= 2 && u32::from(self.bitpool) <= most.min(250)
	}

	/// PCM frames - samples per channel - one SBC frame carries.
	pub const fn samples(&self) -> usize {
		self.blocks as usize * self.subbands as usize
	}

	/// HOW LONG A FRAME IS, in bytes, from its configuration alone.
	pub fn frame_length(&self) -> usize {
		let (subbands, blocks, channels, bitpool) = (self.subbands as usize, self.blocks as usize, self.mode.channels(), self.bitpool as usize);
		let header = 4 + (4 * subbands * channels) / 8;
		header
			+ match self.mode {
				Mode::Mono | Mode::DualChannel => (blocks * channels * bitpool).div_ceil(8),
				Mode::Stereo => (blocks * bitpool).div_ceil(8),
				Mode::JointStereo => (subbands + blocks * bitpool).div_ceil(8),
			}
	}

	/// The bit rate the configuration makes, in bits a second.
	pub fn bit_rate(&self) -> u32 {
		(8 * self.frame_length() as u64 * u64::from(self.frequency) / self.samples() as u64) as u32
	}

	fn frequency_index(&self) -> usize {
		match self.frequency {
			16_000 => 0,
			32_000 => 1,
			44_100 => 2,
			_ => 3,
		}
	}
}

// ------------------------------------------------------------------ the specification's tables

/// The loudness allocation's offsets, by sampling frequency and subband: Appendix B's tables.
const OFFSET4: [[i32; 4]; 4] = [[-1, 0, 0, 0], [-2, 0, 0, 1], [-2, 0, 0, 1], [-2, 0, 0, 1]];
const OFFSET8: [[i32; 8]; 4] = [[-2, 0, 0, 0, 0, 0, 0, 1], [-3, 0, 0, 0, 0, 0, 1, 2], [-4, 0, 0, 0, 0, 0, 1, 2], [-4, 0, 0, 0, 0, 0, 1, 2]];

/// THE PROTOTYPE FILTERS, Proto_4_40 and Proto_8_80, as the specification tabulates them.
#[rustfmt::skip]
const PROTO_4_40: [f64; 40] = [
	0.00000000E+00, 5.36548976E-04, 1.49188357E-03, 2.73370904E-03,
	3.83720193E-03, 3.89205149E-03, 1.86581691E-03, -3.06012286E-03,
	1.09137620E-02, 2.04385087E-02, 2.88757392E-02, 3.21939290E-02,
	2.58767811E-02, 6.13245186E-03, -2.88217274E-02, -7.76463494E-02,
	1.35593274E-01, 1.94987841E-01, 2.46636662E-01, 2.81828203E-01,
	2.94315332E-01, 2.81828203E-01, 2.46636662E-01, 1.94987841E-01,
	-1.35593274E-01, -7.76463494E-02, -2.88217274E-02, 6.13245186E-03,
	2.58767811E-02, 3.21939290E-02, 2.88757392E-02, 2.04385087E-02,
	-1.09137620E-02, -3.06012286E-03, 1.86581691E-03, 3.89205149E-03,
	3.83720193E-03, 2.73370904E-03, 1.49188357E-03, 5.36548976E-04,
];

#[rustfmt::skip]
const PROTO_8_80: [f64; 80] = [
	0.00000000E+00, 1.56575398E-04, 3.43256425E-04, 5.54620202E-04,
	8.23919506E-04, 1.13992507E-03, 1.47640169E-03, 1.78371725E-03,
	2.01182542E-03, 2.10371989E-03, 1.99454554E-03, 1.61656283E-03,
	9.02154502E-04, -1.78805361E-04, -1.64973098E-03, -3.49717454E-03,
	5.65949473E-03, 8.02941163E-03, 1.04584443E-02, 1.27472335E-02,
	1.46525263E-02, 1.59045603E-02, 1.62208471E-02, 1.53184106E-02,
	1.29371806E-02, 8.85757540E-03, 2.92408442E-03, -4.91578024E-03,
	-1.46404076E-02, -2.61098752E-02, -3.90751381E-02, -5.31873032E-02,
	6.79989431E-02, 8.29847578E-02, 9.75753918E-02, 1.11196689E-01,
	1.23264548E-01, 1.33264415E-01, 1.40753505E-01, 1.45389847E-01,
	1.46955068E-01, 1.45389847E-01, 1.40753505E-01, 1.33264415E-01,
	1.23264548E-01, 1.11196689E-01, 9.75753918E-02, 8.29847578E-02,
	-6.79989431E-02, -5.31873032E-02, -3.90751381E-02, -2.61098752E-02,
	-1.46404076E-02, -4.91578024E-03, 2.92408442E-03, 8.85757540E-03,
	1.29371806E-02, 1.53184106E-02, 1.62208471E-02, 1.59045603E-02,
	1.46525263E-02, 1.27472335E-02, 1.04584443E-02, 8.02941163E-03,
	-5.65949473E-03, -3.49717454E-03, -1.64973098E-03, -1.78805361E-04,
	9.02154502E-04, 1.61656283E-03, 1.99454554E-03, 2.10371989E-03,
	2.01182542E-03, 1.78371725E-03, 1.47640169E-03, 1.13992507E-03,
	8.23919506E-04, 5.54620202E-04, 3.43256425E-04, 1.56575398E-04,
];

/// `cos(m * pi / 16)` for m from 0 to 8; every cosine the matrices take is one of these, by symmetry.
#[rustfmt::skip]
const COS16: [f64; 9] = [1.0, 0.980_785_280_403_230_4, 0.923_879_532_511_286_7, 0.831_469_612_302_545_2, 0.707_106_781_186_547_6, 0.555_570_233_019_602_2, 0.382_683_432_365_089_8, 0.195_090_322_016_128_3, 0.0];

const fn q31(value: f64) -> i64 {
	(value * 2_147_483_648.0) as i64
}

const fn q30(value: f64) -> i64 {
	(value * 1_073_741_824.0) as i64
}

const fn proto4() -> [i64; 40] {
	let mut out = [0i64; 40];
	let mut at = 0;
	while at < 40 {
		out[at] = q31(PROTO_4_40[at]);
		at += 1;
	}
	out
}

const fn proto8() -> [i64; 80] {
	let mut out = [0i64; 80];
	let mut at = 0;
	while at < 80 {
		out[at] = q31(PROTO_8_80[at]);
		at += 1;
	}
	out
}

/// `cos(m * pi / 16)` for any integer `m`, in Q30.
const fn cos16(m: i64) -> i64 {
	let m = m.rem_euclid(32);
	let m = if m > 16 { 32 - m } else { m };
	if m <= 8 { q30(COS16[m as usize]) } else { -q30(COS16[(16 - m) as usize]) }
}

/// The analysis matrices: `M[k][i] = cos((k + 0.5)(i - 4) pi / 8)` for eight subbands, `cos((k + 0.5)(i - 2) pi / 4)` for
/// four - both `cos(m pi / 16)` with `m` the products below.
const fn analysis8() -> [[i64; 16]; 8] {
	let mut out = [[0i64; 16]; 8];
	let mut k = 0;
	while k < 8 {
		let mut i = 0;
		while i < 16 {
			out[k][i] = cos16((2 * k as i64 + 1) * (i as i64 - 4));
			i += 1;
		}
		k += 1;
	}
	out
}

const fn analysis4() -> [[i64; 8]; 4] {
	let mut out = [[0i64; 8]; 4];
	let mut k = 0;
	while k < 4 {
		let mut i = 0;
		while i < 8 {
			out[k][i] = cos16(2 * (2 * k as i64 + 1) * (i as i64 - 2));
			i += 1;
		}
		k += 1;
	}
	out
}

/// The synthesis matrices: `N[k][i] = cos((i + 0.5)(k + 4) pi / 8)` for eight subbands, `cos((i + 0.5)(k + 2) pi / 4)`
/// for four.
const fn synthesis8() -> [[i64; 8]; 16] {
	let mut out = [[0i64; 8]; 16];
	let mut k = 0;
	while k < 16 {
		let mut i = 0;
		while i < 8 {
			out[k][i] = cos16((2 * i as i64 + 1) * (k as i64 + 4));
			i += 1;
		}
		k += 1;
	}
	out
}

const fn synthesis4() -> [[i64; 4]; 8] {
	let mut out = [[0i64; 4]; 8];
	let mut k = 0;
	while k < 8 {
		let mut i = 0;
		while i < 4 {
			out[k][i] = cos16(2 * (2 * i as i64 + 1) * (k as i64 + 2));
			i += 1;
		}
		k += 1;
	}
	out
}

const C4: [i64; 40] = proto4();
const C8: [i64; 80] = proto8();
const M4: [[i64; 8]; 4] = analysis4();
const M8: [[i64; 16]; 8] = analysis8();
const N4: [[i64; 4]; 8] = synthesis4();
const N8: [[i64; 8]; 16] = synthesis8();

/// Subband samples carry this many fraction bits beyond the 16-bit sample domain.
const FRACTION: u32 = 12;

// ------------------------------------------------------------------ the bit allocation

/// THE SPECIFICATION'S ALLOCATION: how many bits each subband of each channel takes, from the scale factors and the
/// bitpool - run identically by the encoder and the decoder.
fn allocate(config: &Config, scale_factors: &[[u8; 8]; 2], bits: &mut [[u8; 8]; 2]) {
	let subbands = config.subbands as usize;
	let offset = |sb: usize| -> i32 { if subbands == 4 { OFFSET4[config.frequency_index()][sb] } else { OFFSET8[config.frequency_index()][sb] } };
	let need = |ch: usize, sb: usize| -> i32 {
		let factor = i32::from(scale_factors[ch][sb]);
		match config.allocation {
			Allocation::Snr => factor,
			Allocation::Loudness if factor == 0 => -5,
			Allocation::Loudness => {
				let loudness = factor - offset(sb);
				if loudness > 0 { loudness / 2 } else { loudness }
			}
		}
	};
	let bitpool = i32::from(config.bitpool);
	*bits = [[0; 8]; 2];
	match config.mode {
		Mode::Mono | Mode::DualChannel => {
			for ch in 0..config.mode.channels() {
				let mut bitneed = [0i32; 8];
				for (sb, slot) in bitneed.iter_mut().enumerate().take(subbands) {
					*slot = need(ch, sb);
				}
				let mut channel = [[0u8; 8]; 1];
				distribute(&[bitneed], subbands, bitpool, &mut channel, false);
				bits[ch] = channel[0];
			}
		}
		Mode::Stereo | Mode::JointStereo => {
			let mut bitneed = [[0i32; 8]; 2];
			for (ch, row) in bitneed.iter_mut().enumerate() {
				for (sb, slot) in row.iter_mut().enumerate().take(subbands) {
					*slot = need(ch, sb);
				}
			}
			distribute(&bitneed, subbands, bitpool, bits, true);
		}
	}
}

// The slicing and the two distributions of what is left, over one channel or - in the two stereo modes - both,
// alternating channels as the specification does.
fn distribute<const C: usize>(bitneed: &[[i32; 8]; C], subbands: usize, bitpool: i32, bits: &mut [[u8; 8]; C], joint: bool) {
	let channels = if joint { 2 } else { 1 };
	let max_bitneed = (0..channels).flat_map(|ch| bitneed[ch][..subbands].iter().copied()).max().unwrap_or(0);
	let mut bitcount = 0i32;
	let mut slicecount = 0i32;
	let mut bitslice = max_bitneed + 1;
	loop {
		bitslice -= 1;
		bitcount += slicecount;
		slicecount = 0;
		for row in bitneed.iter().take(channels) {
			for &need in &row[..subbands] {
				if need > bitslice + 1 && need < bitslice + 16 {
					slicecount += 1;
				} else if need == bitslice + 1 {
					slicecount += 2;
				}
			}
		}
		if bitcount + slicecount >= bitpool {
			break;
		}
	}
	if bitcount + slicecount == bitpool {
		bitcount += slicecount;
		bitslice -= 1;
	}
	for ch in 0..channels {
		for sb in 0..subbands {
			let need = bitneed[ch][sb];
			bits[ch][sb] = if need < bitslice + 2 { 0 } else { (need - bitslice).min(16) as u8 };
		}
	}
	// WHAT IS LEFT, first to subbands already allocated and to those one short of a slice, then one bit at a time.
	let (mut ch, mut sb) = (0usize, 0usize);
	while bitcount < bitpool && sb < subbands {
		if bits[ch][sb] >= 2 && bits[ch][sb] < 16 {
			bits[ch][sb] += 1;
			bitcount += 1;
		} else if bitneed[ch][sb] == bitslice + 1 && bitpool > bitcount + 1 {
			bits[ch][sb] = 2;
			bitcount += 2;
		}
		if channels == 2 && ch == 0 {
			ch = 1;
		} else {
			ch = 0;
			sb += 1;
		}
	}
	let (mut ch, mut sb) = (0usize, 0usize);
	while bitcount < bitpool && sb < subbands {
		if bits[ch][sb] < 16 {
			bits[ch][sb] += 1;
			bitcount += 1;
		}
		if channels == 2 && ch == 0 {
			ch = 1;
		} else {
			ch = 0;
			sb += 1;
		}
	}
}

// ------------------------------------------------------------------ the CRC

/// THE FRAME'S CHECK: CRC-8 with the polynomial x^8 + x^4 + x^3 + x^2 + 1 from 0x0F, over `bits` bits of `data`, most
/// significant first.
pub fn crc8(data: &[u8], bits: usize) -> u8 {
	let mut crc: u8 = 0x0f;
	for at in 0..bits {
		let bit = (data[at / 8] >> (7 - at % 8)) & 1;
		let top = crc >> 7;
		crc <<= 1;
		if top ^ bit != 0 {
			crc ^= 0x1d;
		}
	}
	crc
}

// ------------------------------------------------------------------ bits

struct Writer<'a> {
	out: &'a mut [u8],
	at: usize,
}

impl Writer<'_> {
	fn put(&mut self, value: u32, bits: u32) {
		for shift in (0..bits).rev() {
			let bit = ((value >> shift) & 1) as u8;
			let byte = self.at / 8;
			if bit != 0 {
				self.out[byte] |= 0x80 >> (self.at % 8);
			} else {
				self.out[byte] &= !(0x80 >> (self.at % 8));
			}
			self.at += 1;
		}
	}
}

struct Reader<'a> {
	data: &'a [u8],
	at: usize,
}

impl Reader<'_> {
	fn get(&mut self, bits: u32) -> Option<u32> {
		let mut value = 0u32;
		for _ in 0..bits {
			let byte = *self.data.get(self.at / 8)?;
			value = (value << 1) | u32::from((byte >> (7 - self.at % 8)) & 1);
			self.at += 1;
		}
		Some(value)
	}
}

// ------------------------------------------------------------------ the header

/// Why a frame was not decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Not a sync word this decoder knows.
	Sync,
	/// A configuration the specification does not define.
	Config,
	/// Shorter than its own header says.
	Short,
	/// Its CRC does not match its header and scale factors.
	Crc,
}

/// THE CONFIGURATION A FRAME CARRIES, read from its first bytes.
pub fn parse_header(frame: &[u8]) -> Result<Config, Refusal> {
	if frame.len() < 4 {
		return Err(Refusal::Short);
	}
	if frame[0] == MSBC_SYNC {
		return Ok(Config::MSBC);
	}
	if frame[0] != SYNC {
		return Err(Refusal::Sync);
	}
	let frequency = [16_000, 32_000, 44_100, 48_000][usize::from(frame[1] >> 6)];
	let blocks = [4, 8, 12, 16][usize::from((frame[1] >> 4) & 3)];
	let mode = [Mode::Mono, Mode::DualChannel, Mode::Stereo, Mode::JointStereo][usize::from((frame[1] >> 2) & 3)];
	let allocation = if frame[1] & 2 != 0 { Allocation::Snr } else { Allocation::Loudness };
	let subbands = if frame[1] & 1 != 0 { 8 } else { 4 };
	let config = Config { frequency, blocks, mode, allocation, subbands, bitpool: frame[2] };
	if !config.valid() {
		return Err(Refusal::Config);
	}
	Ok(config)
}

// ------------------------------------------------------------------ the encoder

/// AN SBC ENCODER for one configuration: the analysis filter's history per channel.
pub struct Encoder {
	config: Config,
	x: [[i64; 80]; 2],
}

impl Encoder {
	pub fn new(config: Config) -> Option<Encoder> {
		config.valid().then_some(Encoder { config, x: [[0; 80]; 2] })
	}

	pub fn config(&self) -> Config {
		self.config
	}

	// THE ANALYSIS FILTER: one block of `subbands` new samples of one channel into as many subband samples, Q12.
	fn analyse(&mut self, ch: usize, input: &[i32], out: &mut [i64; 8]) {
		let subbands = self.config.subbands as usize;
		let taps = 10 * subbands;
		let x = &mut self.x[ch];
		x.copy_within(0..taps - subbands, subbands);
		for (i, &sample) in input.iter().enumerate().take(subbands) {
			x[subbands - 1 - i] = i64::from(sample);
		}
		// Y[i] = sum over five of Z = C * X: Q31 products, kept to Q15 of the sample domain.
		let mut y = [0i64; 16];
		for (i, slot) in y.iter_mut().enumerate().take(2 * subbands) {
			let mut sum = 0i64;
			for j in 0..5 {
				let at = i + 2 * subbands * j;
				let c = if subbands == 8 { C8[at] } else { C4[at] };
				sum += x[at] * c;
			}
			*slot = sum >> 16;
		}
		for (k, slot) in out.iter_mut().enumerate().take(subbands) {
			let mut sum = 0i64;
			for (i, &value) in y.iter().enumerate().take(2 * subbands) {
				let m = if subbands == 8 { M8[k][i] } else { M4[k][i] };
				sum += value * m;
			}
			// Q15 times Q30, down to Q12.
			*slot = sum >> (15 + 30 - FRACTION);
		}
	}

	/// ONE FRAME from `pcm`: the configuration's `samples()` frames, interleaved by channel. Returns the frame's length;
	/// `out` must hold `frame_length()`.
	pub fn encode(&mut self, pcm: &[i16], out: &mut [u8]) -> usize {
		let config = self.config;
		let (subbands, blocks, channels) = (config.subbands as usize, config.blocks as usize, config.mode.channels());
		let length = config.frame_length();
		if out.len() < length || pcm.len() < config.samples() * channels {
			return 0;
		}
		// THE SUBBAND SAMPLES, every block of every channel.
		let mut sb_samples = [[[0i64; 8]; 2]; 16];
		for (block, row) in sb_samples.iter_mut().enumerate().take(blocks) {
			for (ch, samples) in row.iter_mut().enumerate().take(channels) {
				let mut input = [0i32; 8];
				for (i, slot) in input.iter_mut().enumerate().take(subbands) {
					*slot = i32::from(pcm[(block * subbands + i) * channels + ch]);
				}
				self.analyse(ch, &input, samples);
			}
		}
		// JOINT STEREO, subband by subband but the last: the sum and difference where they need fewer bits.
		let mut join = [false; 8];
		if config.mode == Mode::JointStereo {
			for (sb, joined) in join.iter_mut().enumerate().take(subbands - 1) {
				let separate = factor_of(&sb_samples, blocks, 0, sb) + factor_of(&sb_samples, blocks, 1, sb);
				let mut mid_side = [[[0i64; 8]; 2]; 16];
				for block in 0..blocks {
					let (l, r) = (sb_samples[block][0][sb], sb_samples[block][1][sb]);
					mid_side[block][0][sb] = (l + r) >> 1;
					mid_side[block][1][sb] = (l - r) >> 1;
				}
				if factor_of(&mid_side, blocks, 0, sb) + factor_of(&mid_side, blocks, 1, sb) < separate {
					*joined = true;
					for block in 0..blocks {
						sb_samples[block][0][sb] = mid_side[block][0][sb];
						sb_samples[block][1][sb] = mid_side[block][1][sb];
					}
				}
			}
		}
		let mut scale_factors = [[0u8; 8]; 2];
		for (ch, row) in scale_factors.iter_mut().enumerate().take(channels) {
			for (sb, factor) in row.iter_mut().enumerate().take(subbands) {
				*factor = factor_of(&sb_samples, blocks, ch, sb);
			}
		}
		let mut bits = [[0u8; 8]; 2];
		allocate(&config, &scale_factors, &mut bits);
		// THE FRAME.
		out[..length].fill(0);
		let mut w = Writer { out, at: 0 };
		let msbc = config == Config::MSBC;
		if msbc {
			w.put(u32::from(MSBC_SYNC), 8);
			w.put(0, 16);
		} else {
			w.put(u32::from(SYNC), 8);
			let frequency = config.frequency_index() as u32;
			let blocks_code = (config.blocks / 4 - 1) as u32;
			let mode = match config.mode {
				Mode::Mono => 0,
				Mode::DualChannel => 1,
				Mode::Stereo => 2,
				Mode::JointStereo => 3,
			};
			let allocation = u32::from(config.allocation == Allocation::Snr);
			let subbands_code = u32::from(config.subbands == 8);
			w.put((frequency << 6) | (blocks_code << 4) | (mode << 2) | (allocation << 1) | subbands_code, 8);
			w.put(u32::from(config.bitpool), 8);
		}
		w.put(0, 8);
		if config.mode == Mode::JointStereo {
			for &joined in join.iter().take(subbands) {
				w.put(u32::from(joined), 1);
			}
		}
		for row in scale_factors.iter().take(channels) {
			for &factor in row.iter().take(subbands) {
				w.put(u32::from(factor), 4);
			}
		}
		let checked = w.at;
		for row in sb_samples.iter().take(blocks) {
			for ch in 0..channels {
				for sb in 0..subbands {
					let width = u32::from(bits[ch][sb]);
					if width == 0 {
						continue;
					}
					let levels = (1i64 << width) - 1;
					let shift = u32::from(scale_factors[ch][sb]) + 1 + FRACTION;
					// (sample / 2^(sf + 1) + 1) * levels / 2, floored: in [0, levels - 1].
					let quantized = ((row[ch][sb] + (1i64 << shift)) * levels) >> (shift + 1);
					w.put(quantized.clamp(0, levels - 1) as u32, width);
				}
			}
		}
		// THE CHECK, over the configuration and bitpool bytes - mSBC's two reserved ones - and everything after the CRC
		// up to the end of the scale factors.
		let crc = frame_crc(out, checked);
		out[3] = crc;
		length
	}
}

// The scale factor of one subband of one channel over the frame's blocks: the smallest `n` with every sample below
// 2^(n+1).
fn factor_of(samples: &[[[i64; 8]; 2]; 16], blocks: usize, ch: usize, sb: usize) -> u8 {
	let peak = samples.iter().take(blocks).map(|row| row[ch][sb].unsigned_abs()).max().unwrap_or(0);
	let mut factor = 0u8;
	while factor < 15 && peak >= 1u64 << (u32::from(factor) + 1 + FRACTION) {
		factor += 1;
	}
	factor
}

// The CRC over bytes 1 and 2 and the bits from byte 4 up to `end`.
fn frame_crc(frame: &[u8], end: usize) -> u8 {
	let mut data = [0u8; 2 + 1 + 8 + 2];
	data[0] = frame[1];
	data[1] = frame[2];
	let tail_bits = end - 32;
	let tail_bytes = tail_bits.div_ceil(8);
	data[2..2 + tail_bytes].copy_from_slice(&frame[4..4 + tail_bytes]);
	crc8(&data, 16 + tail_bits)
}

// ------------------------------------------------------------------ the decoder

/// AN SBC DECODER: the synthesis filter's history per channel, kept across frames of one stream.
pub struct Decoder {
	v: [[i64; 160]; 2],
}

impl Default for Decoder {
	fn default() -> Decoder {
		Decoder::new()
	}
}

impl Decoder {
	pub const fn new() -> Decoder {
		Decoder { v: [[0; 160]; 2] }
	}

	// THE SYNTHESIS FILTER: one block of subband samples of one channel into as many PCM samples.
	fn synthesise(&mut self, ch: usize, subbands: usize, input: &[i64; 8], out: &mut [i16]) {
		let v = &mut self.v[ch];
		let length = 20 * subbands;
		v.copy_within(0..length - 2 * subbands, 2 * subbands);
		for k in 0..2 * subbands {
			let mut sum = 0i64;
			for (i, &sample) in input.iter().enumerate().take(subbands) {
				let n = if subbands == 8 { N8[k][i] } else { N4[k][i] };
				sum += sample * n;
			}
			// Q12 times Q30, kept at Q12.
			v[k] = sum >> 30;
		}
		// U from V, windowed by D = -C times the subband count, and summed over ten.
		for (j, slot) in out.iter_mut().enumerate().take(subbands) {
			let mut sum = 0i64;
			for i in 0..10 {
				let at = j + subbands * i;
				// THE U ORDERING: of each 2 * subbands of V the first half, then of the next 2 * subbands the second.
				let u = if i % 2 == 0 { v[(i / 2) * 4 * subbands + j] } else { v[(i / 2) * 4 * subbands + 3 * subbands + j] };
				let c = if subbands == 8 { C8[at] } else { C4[at] };
				sum += u * c;
			}
			// D = -C * subbands: Q12 times Q31, scaled by the subband count with the window's sign, down to the sample
			// domain.
			let value = -(sum * subbands as i64) >> (31 + FRACTION);
			*slot = value.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16;
		}
	}

	/// ONE FRAME into `pcm`, interleaved by channel: its configuration and the PCM frames written.
	pub fn decode(&mut self, frame: &[u8], pcm: &mut [i16]) -> Result<(Config, usize), Refusal> {
		let config = parse_header(frame)?;
		let (subbands, blocks, channels) = (config.subbands as usize, config.blocks as usize, config.mode.channels());
		let length = config.frame_length();
		if frame.len() < length {
			return Err(Refusal::Short);
		}
		if pcm.len() < config.samples() * channels {
			return Err(Refusal::Short);
		}
		let mut r = Reader { data: frame, at: 32 };
		let mut join = [false; 8];
		if config.mode == Mode::JointStereo {
			for joined in join.iter_mut().take(subbands) {
				*joined = r.get(1).ok_or(Refusal::Short)? != 0;
			}
		}
		let mut scale_factors = [[0u8; 8]; 2];
		for row in scale_factors.iter_mut().take(channels) {
			for factor in row.iter_mut().take(subbands) {
				*factor = r.get(4).ok_or(Refusal::Short)? as u8;
			}
		}
		if frame_crc(frame, r.at) != frame[3] {
			return Err(Refusal::Crc);
		}
		let mut bits = [[0u8; 8]; 2];
		allocate(&config, &scale_factors, &mut bits);
		for block in 0..blocks {
			let mut samples = [[0i64; 8]; 2];
			for ch in 0..channels {
				for sb in 0..subbands {
					let width = u32::from(bits[ch][sb]);
					if width == 0 {
						continue;
					}
					let quantized = i64::from(r.get(width).ok_or(Refusal::Short)?);
					let levels = (1i64 << width) - 1;
					let scale = 1i64 << (u32::from(scale_factors[ch][sb]) + 1 + FRACTION);
					// scale * ((2q + 1) / levels - 1)
					samples[ch][sb] = ((2 * quantized + 1) * scale) / levels - scale;
				}
			}
			if config.mode == Mode::JointStereo {
				for sb in 0..subbands {
					if join[sb] {
						let (mid, side) = (samples[0][sb], samples[1][sb]);
						samples[0][sb] = mid + side;
						samples[1][sb] = mid - side;
					}
				}
			}
			for (ch, channel) in samples.iter().enumerate().take(channels) {
				let mut block_out = [0i16; 8];
				self.synthesise(ch, subbands, channel, &mut block_out);
				for (i, &sample) in block_out.iter().enumerate().take(subbands) {
					pcm[(block * subbands + i) * channels + ch] = sample;
				}
			}
		}
		Ok((config, config.samples()))
	}
}

#[cfg(test)]
mod tests;
