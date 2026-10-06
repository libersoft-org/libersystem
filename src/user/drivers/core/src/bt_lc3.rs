// THE FIXTURE'S LC3: what an emulated earbud reads from an LE Audio stream and what an emulated source writes into one.
//
// A TEST FIXTURE, DEVELOPMENT-ONLY, AND WRITTEN APART FROM THE HOST'S CODEC: its own side-information reader, its own
// arithmetic decoder and encoder, from the LC3 specification - so a host frame this reads correctly is one two
// implementations agree on. It needs no transform: an earbud judges what it hears in the spectral domain - which
// spectral line carries the energy - and a source writes one spectral line straight into a frame, which the host's
// synthesis turns into audio at that line's frequency.
//
// What it covers is the specification's bitstream and nothing past it: Section 3.4.2's reading of the side information,
// the TNS data and the spectrum's 2-tuples through the arithmetic decoder, the residual and LSB-mode bits, and every bit
// error check that section names; and Section 3.3.13's writing of the same, byte for byte - Appendix C's encoded
// frames come out of `write_frame` identical. The SNS indices are read and written, never dequantized; no noise filling,
// TNS or SNS synthesis, no inverse MDCT and no long-term postfilter.

mod tables;

use tables::{AC_SPEC_CUMFREQ, AC_SPEC_FREQ, AC_SPEC_LOOKUP, AC_TNS_COEF_CUMFREQ, AC_TNS_COEF_FREQ, AC_TNS_ORDER_CUMFREQ, AC_TNS_ORDER_FREQ, BANDS_10MS, TONE_STAGE1};

/// The most spectral lines a frame codes: 400, at 48 kHz and 44.1 kHz in 10 ms.
pub const MAX_LINES: usize = 400;

// What a decoder that met any of 3.4.2's bit error conditions does: it stops parsing.
const CORRUPT: &str = "the LC3 frame is corrupt: a decoder detects a bit error in it";

/// What one frame says, as the fixture reads it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FrameInfo {
	/// The bandwidth index the frame codes (0 narrowband up to 4 full band).
	pub bandwidth: u8,
	/// The last non-zero 2-tuple's end: no line at or past it is coded.
	pub lastnz: u16,
	/// The global gain index.
	pub global_gain: u8,
	/// The quantized line with the largest magnitude, where any line is not zero.
	pub loudest: Option<u16>,
}

/// THE SNS VECTOR QUANTIZER'S INDICES as the side information carries them (3.3.7.3.4, 3.4.7.2) - read and written,
/// never turned back into scale factors.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Sns {
	/// The first stage's two codebook rows.
	pub ind_lf: u8,
	pub ind_hf: u8,
	/// shape_j: 0 regular, 1 regular_lf, 2 outlier_near, 3 outlier_far.
	pub shape: u8,
	/// gain_i, whole: for shapes 1 and 3 its least significant bit travels inside the joint index.
	pub gain: u8,
	/// Set A's leading sign (1 negative) and MPVQ index.
	pub ls_a: u8,
	pub idx_a: u32,
	/// Set B's, for shape 0 alone.
	pub ls_b: u8,
	pub idx_b: u8,
}

/// The long-term postfilter's data, where the frame says a pitch is present.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Pitch {
	pub ltpf_active: bool,
	pub index: u16,
}

/// ONE FRAME'S CONTENT, field for field as Sections 3.3.13 and 3.4.2 lay it out: what `read_frame` reads and
/// `write_frame` writes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
	/// P_bw.
	pub bandwidth: u8,
	/// The end of the last coded 2-tuple: even, 2 at least.
	pub lastnz: u16,
	pub lsb_mode: bool,
	/// gg_ind.
	pub global_gain: u8,
	/// Each TNS filter's order, 0 where it is off, and its reflection coefficients' quantizer indices - 8, the zero
	/// coefficient, past the order.
	pub tns_order: [u8; 2],
	pub tns_index: [[u8; 8]; 2],
	pub pitch: Option<Pitch>,
	pub sns: Sns,
	/// F_NF, the noise filling level: 7 is the quietest.
	pub noise: u8,
	/// THE QUANTIZED SPECTRUM X_q - in LSB mode with the least significant bits the residual data carried.
	pub spectrum: [i32; MAX_LINES],
	/// THE RESIDUAL BITS (outside LSB mode): one refinement for each non-zero line from the lowest. Read, as many as the
	/// frame carried; written, as many of these as the frame has room for.
	pub residual: [bool; MAX_LINES],
	pub residual_len: u16,
	/// The bits the frame left after its side information and its arithmetically coded data - a decoder's
	/// `nbits_residual`. Read only; `write_frame` works it out.
	pub room: u16,
}

impl Frame {
	/// A frame of nothing: every line zero, TNS and pitch off, the first codebook rows.
	pub const fn empty() -> Frame {
		Frame { bandwidth: 0, lastnz: 2, lsb_mode: false, global_gain: 0, tns_order: [0; 2], tns_index: [[8; 8]; 2], pitch: None, sns: Sns { ind_lf: 0, ind_hf: 0, shape: 0, gain: 0, ls_a: 0, idx_a: 0, ls_b: 0, idx_b: 0 }, noise: 0, spectrum: [0; MAX_LINES], residual: [false; MAX_LINES], residual_len: 0, room: 0 }
	}

	/// The line with the largest magnitude - the lowest of equals - where any line is not zero.
	pub fn loudest(&self) -> Option<u16> {
		let mut best: Option<(usize, u32)> = None;
		for (line, value) in self.spectrum.iter().enumerate() {
			let magnitude = value.unsigned_abs();
			if magnitude > 0 && best.is_none_or(|(_, top)| magnitude > top) {
				best = Some((line, magnitude));
			}
		}
		best.map(|(line, _)| line as u16)
	}
}

// ------------------------------------------------------------------ the frame's fixed parameters

// WHAT THE SAMPLING RATE, THE DURATION AND THE SIZE FIX (3.2, 3.3.4.3, 3.3.5.2, 3.3.8.2, 3.4.2.2).
#[derive(Clone, Copy)]
struct Geometry {
	fs_ind: usize,
	// N_E, the coded lines.
	ne: usize,
	nbits: usize,
	nbits_bw: u32,
	// ceil(log2(N_E / 2)).
	nbits_lastnz: u32,
	// 512 above the rate's low-bitrate threshold: the other half of the context map.
	rate_flag: usize,
	// 1 below 48 bits a millisecond: the TNS order's other model.
	tns_lpc_weighting: usize,
}

// 7.5 ms frames are here for Appendix C, whose verification frames include two; `read` and `write_tone` take 10 ms.
fn geometry(sample_rate: u32, frame_us: u32, nbytes: usize) -> Result<Geometry, &'static str> {
	let fs_ind = match sample_rate {
		8_000 => 0,
		16_000 => 1,
		24_000 => 2,
		32_000 => 3,
		// 44.1 kHz runs in every respect as 48 kHz does (3.2.2).
		44_100 | 48_000 => 4,
		_ => return Err("LC3 has no such sampling rate"),
	};
	let ne = match frame_us {
		10_000 => [80, 160, 240, 320, 400][fs_ind],
		7_500 => [60, 120, 180, 240, 300][fs_ind],
		_ => return Err("LC3 frames last 10 ms or 7.5 ms"),
	};
	if !(20..=400).contains(&nbytes) {
		return Err("an LC3 frame is 20 to 400 bytes");
	}
	let nbits = nbytes * 8;
	Ok(Geometry { fs_ind, ne, nbits, nbits_bw: [0, 1, 2, 2, 3][fs_ind], nbits_lastnz: usize::BITS - (ne / 2 - 1).leading_zeros(), rate_flag: if nbits > 160 + fs_ind * 160 { 512 } else { 0 }, tns_lpc_weighting: usize::from(nbits * 1_000 < 48 * frame_us as usize) })
}

// THE JOINT SNS INDEX'S SECTIONS (Table 3.14): set A's MPVQ sizes for the regular shapes, outlier_near and outlier_far,
// and the regular shapes' second factor - set B's index and sign plus the two gain-LSB sections.
const SZ_REGULAR: u32 = 4_780_008 >> 1;
const SZ_REGULAR_B: u32 = 14;
const SZ_NEAR: u32 = 30_316_544 >> 1;
const SZ_FAR: u32 = 1_549_824 >> 1;
const GAIN_LEVELS: [u8; 4] = [2, 4, 4, 8];
const GAIN_MSB_BITS: [u32; 4] = [1, 1, 2, 2];
const GAIN_LSB_BITS: [u32; 4] = [0, 1, 0, 1];

// ------------------------------------------------------------------ reading

// THE SIDE INFORMATION'S READER: from the last byte backwards, least significant bit first (read_bit, 3.4.2.7).
struct Side<'a> {
	bytes: &'a [u8],
	at: isize,
	mask: u8,
}

impl Side<'_> {
	fn bit(&mut self) -> Result<u32, &'static str> {
		let byte = usize::try_from(self.at).ok().and_then(|at| self.bytes.get(at)).ok_or(CORRUPT)?;
		let bit = u32::from(byte & self.mask != 0);
		if self.mask == 0x80 {
			self.mask = 1;
			self.at -= 1;
		} else {
			self.mask <<= 1;
		}
		Ok(bit)
	}

	fn uint(&mut self, bits: u32) -> Result<u32, &'static str> {
		let mut value = 0;
		for shift in 0..bits {
			value |= self.bit()? << shift;
		}
		Ok(value)
	}

	// 8 * bp_side + 8 - log2(mask_side): the bits at and before the reading point.
	fn before(&self) -> isize {
		8 * self.at + 8 - self.mask.trailing_zeros() as isize
	}
}

// THE ARITHMETIC DECODER (ac_dec_init and ac_decode, 3.4.2.7): forwards from the first byte, three bytes ahead.
struct Decoder<'a> {
	bytes: &'a [u8],
	bp: usize,
	low: u32,
	range: u32,
}

impl<'a> Decoder<'a> {
	fn new(bytes: &'a [u8]) -> Self {
		let low = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
		Decoder { bytes, bp: 3, low, range: 0x00ff_ffff }
	}

	fn decode(&mut self, cumfreq: &[u16], freq: &[u16]) -> Result<usize, &'static str> {
		let step = self.range >> 10;
		if self.low >= step << 10 {
			return Err(CORRUPT);
		}
		let mut symbol = cumfreq.len() - 1;
		while self.low < step * u32::from(cumfreq[symbol]) {
			symbol -= 1;
		}
		self.low -= step * u32::from(cumfreq[symbol]);
		self.range = step * u32::from(freq[symbol]);
		while self.range < 0x10000 {
			// A byte past the frame's end is past the side information too: a decoder flags that at the end of the
			// 2-tuple at the latest (bp - bp_side > 3), so it is the same refusal here.
			let byte = *self.bytes.get(self.bp).ok_or(CORRUPT)?;
			self.bp += 1;
			self.low = ((self.low << 8) & 0x00ff_ffff) + u32::from(byte);
			self.range <<= 8;
		}
		Ok(symbol)
	}
}

/// READ ONE FRAME of `bytes.len()` bytes at `sample_rate` and `frame_us` (10 000 or 7 500) as Section 3.4.2 reads it,
/// refused where a decoder would detect a bit error - the bandwidth past the rate's, `lastnz` past the coded lines, an
/// SNS joint index past its codebooks, the arithmetic decoder outside its interval, an escape past the fourteenth
/// level, the arithmetic data running into the side information, or more bits spent than the frame has.
pub fn read_frame(bytes: &[u8], sample_rate: u32, frame_us: u32) -> Result<Frame, &'static str> {
	let g = geometry(sample_rate, frame_us, bytes.len())?;
	let mut frame = Frame::empty();
	let mut side = Side { bytes, at: bytes.len() as isize - 1, mask: 1 };

	// SIDE INFORMATION (3.4.2.3).
	let bandwidth = if g.nbits_bw > 0 { side.uint(g.nbits_bw)? as usize } else { 0 };
	if bandwidth > g.fs_ind {
		return Err(CORRUPT);
	}
	let lastnz = ((side.uint(g.nbits_lastnz)? as usize) + 1) << 1;
	if lastnz > g.ne {
		return Err(CORRUPT);
	}
	frame.bandwidth = bandwidth as u8;
	frame.lastnz = lastnz as u16;
	frame.lsb_mode = side.bit()? == 1;
	frame.global_gain = side.uint(8)? as u8;
	let filters = if bandwidth < 3 { 1 } else { 2 };
	let mut tns_active = [false; 2];
	for active in tns_active.iter_mut().take(filters) {
		*active = side.bit()? == 1;
	}
	let pitch_present = side.bit()? == 1;
	frame.sns = read_sns(&mut side)?;
	if pitch_present {
		let ltpf_active = side.uint(1)? == 1;
		frame.pitch = Some(Pitch { ltpf_active, index: side.uint(9)? as u16 });
	}
	frame.noise = side.uint(3)? as u8;

	// ARITHMETIC DECODING (3.4.2.5): the TNS data, then the spectrum's 2-tuples up to lastnz.
	let mut ac = Decoder::new(bytes);
	for f in 0..filters {
		if tns_active[f] {
			let order = ac.decode(&AC_TNS_ORDER_CUMFREQ[g.tns_lpc_weighting], &AC_TNS_ORDER_FREQ[g.tns_lpc_weighting])? + 1;
			frame.tns_order[f] = order as u8;
			for k in 0..order {
				frame.tns_index[f][k] = ac.decode(&AC_TNS_COEF_CUMFREQ[k], &AC_TNS_COEF_FREQ[k])? as u8;
			}
		}
	}
	// Each 2-tuple's escape count, which the LSB-mode residual reads by.
	let mut levels = [0u8; MAX_LINES / 2];
	let mut context = 0usize;
	for k in (0..lastnz).step_by(2) {
		let mut t = context + g.rate_flag;
		if k > g.ne / 2 {
			t += 256;
		}
		let (mut a, mut b) = (0i32, 0i32);
		let mut level = 0usize;
		let symbol = loop {
			let model = usize::from(AC_SPEC_LOOKUP[t + level.min(3) * 1024]);
			let symbol = ac.decode(&AC_SPEC_CUMFREQ[model], &AC_SPEC_FREQ[model])?;
			if symbol < 16 {
				break symbol;
			}
			// An escape: one more bit plane, its two bits in the side stream - but for LSB mode's first, which the
			// residual data carries.
			if !frame.lsb_mode || level > 0 {
				a += (side.bit()? as i32) << level;
				b += (side.bit()? as i32) << level;
			}
			level += 1;
			if level == 14 {
				return Err(CORRUPT);
			}
		};
		levels[k / 2] = level as u8;
		let (msb_a, msb_b) = (symbol & 3, symbol >> 2);
		a += (msb_a as i32) << level;
		b += (msb_b as i32) << level;
		if a > 0 && side.bit()? == 1 {
			a = -a;
		}
		if b > 0 && side.bit()? == 1 {
			b = -b;
		}
		frame.spectrum[k] = a;
		frame.spectrum[k + 1] = b;
		let level = level.min(3);
		let t = if level <= 1 { 1 + (msb_a + msb_b) * (level + 1) } else { 12 + level };
		context = (context & 15) * 16 + t;
		if ac.bp as isize - side.at > 3 {
			return Err(CORRUPT);
		}
	}

	// RESIDUAL DATA (3.4.2.6): what the side information and the arithmetic data left.
	let nbits_side = g.nbits as isize - side.before();
	let nbits_ari = (ac.bp as isize - 3) * 8 + 25 - floor_log2(ac.range);
	let room = g.nbits as isize - (nbits_side + nbits_ari);
	if room < 0 {
		return Err(CORRUPT);
	}
	frame.room = room as u16;
	let mut left = room as usize;
	if !frame.lsb_mode {
		let mut count = 0;
		for line in 0..g.ne {
			if frame.spectrum[line] != 0 {
				if count == left {
					break;
				}
				frame.residual[count] = side.bit()? == 1;
				count += 1;
			}
		}
		frame.residual_len = count as u16;
	} else {
		// LSB MODE: the first bit plane of each escaped 2-tuple, and the sign of a line that bit plane alone made.
		'lsbs: for k in (0..lastnz).step_by(2) {
			if levels[k / 2] == 0 {
				continue;
			}
			for line in [k, k + 1] {
				if left == 0 {
					break 'lsbs;
				}
				let bit = side.bit()?;
				left -= 1;
				if bit == 1 {
					let value = &mut frame.spectrum[line];
					if *value > 0 {
						*value += 1;
					} else if *value < 0 {
						*value -= 1;
					} else {
						if left == 0 {
							break 'lsbs;
						}
						let sign = side.bit()?;
						left -= 1;
						*value = if sign == 0 { 1 } else { -1 };
					}
				}
			}
		}
	}
	Ok(frame)
}

// THE SNS INDICES (3.4.7.2.1, 3.4.7.2.2), and the joint index's two bit error checks. The specification's outlier
// demultiplexing subtracts outlier_near's size and asks whether the result is negative, which only a signed reading
// makes a question: shape 2's indices are exactly those below that size.
fn read_sns(side: &mut Side) -> Result<Sns, &'static str> {
	let mut sns = Sns { ind_lf: side.uint(5)? as u8, ind_hf: side.uint(5)? as u8, ..Sns::default() };
	let submode_msb = side.bit()?;
	let mut gain = side.uint(if submode_msb == 0 { 1 } else { 2 })?;
	sns.ls_a = side.bit()? as u8;
	if submode_msb == 0 {
		let joint = side.uint(13)? | (side.uint(12)? << 13);
		if joint >= SZ_REGULAR_B * SZ_REGULAR {
			return Err(CORRUPT);
		}
		let section = joint / SZ_REGULAR;
		sns.idx_a = joint % SZ_REGULAR;
		if section < 2 {
			// regular_lf: the section is the gain's least significant bit.
			sns.shape = 1;
			gain = (gain << 1) + section;
		} else {
			sns.shape = 0;
			sns.idx_b = ((section - 2) >> 1) as u8;
			sns.ls_b = ((section - 2) & 1) as u8;
		}
	} else {
		let joint = side.uint(12)? | (side.uint(12)? << 12);
		if joint >= SZ_NEAR + 2 * SZ_FAR {
			return Err(CORRUPT);
		}
		if joint >= SZ_NEAR {
			sns.shape = 3;
			gain = (gain << 1) + ((joint - SZ_NEAR) & 1);
			sns.idx_a = (joint - SZ_NEAR) >> 1;
		} else {
			sns.shape = 2;
			sns.idx_a = joint;
		}
	}
	sns.gain = gain as u8;
	Ok(sns)
}

fn floor_log2(value: u32) -> isize {
	31 - value.leading_zeros() as isize
}

/// READ ONE FRAME of `frame.len()` bytes at `sample_rate` and `frame_us` (10 000 only): its side information and its
/// quantized spectrum, refused where a decoder would detect a bit error in it.
pub fn read(frame: &[u8], sample_rate: u32, frame_us: u32) -> Result<FrameInfo, &'static str> {
	if frame_us != 10_000 {
		return Err("the fixture's LC3 reads 10 ms frames");
	}
	let frame = read_frame(frame, sample_rate, frame_us)?;
	Ok(FrameInfo { bandwidth: frame.bandwidth, lastnz: frame.lastnz, global_gain: frame.global_gain, loudest: frame.loudest() })
}

// ------------------------------------------------------------------ writing

// THE FRAME AS IT IS WRITTEN: the side information backwards from its end (write_bit_backward, 3.3.13.6), the
// arithmetic encoder's bytes forwards from its start (ac_shift, ac_encode, ac_enc_finish). A write that would leave
// the frame is not made and marks the frame as not fitting.
struct Writer<'a> {
	bytes: &'a mut [u8],
	bp: usize,
	side: isize,
	mask: u8,
	low: u32,
	range: u32,
	cache: i32,
	carry: u32,
	carry_count: u32,
	overflow: bool,
}

impl<'a> Writer<'a> {
	fn new(bytes: &'a mut [u8]) -> Self {
		let side = bytes.len() as isize - 1;
		Writer { bytes, bp: 0, side, mask: 1, low: 0, range: 0x00ff_ffff, cache: -1, carry: 0, carry_count: 0, overflow: false }
	}

	fn side_bit(&mut self, bit: bool) {
		match usize::try_from(self.side).ok().and_then(|at| self.bytes.get_mut(at)) {
			Some(byte) if bit => *byte |= self.mask,
			Some(byte) => *byte &= !self.mask,
			None => self.overflow = true,
		}
		if self.mask == 0x80 {
			self.mask = 1;
			self.side -= 1;
		} else {
			self.mask <<= 1;
		}
	}

	fn side_uint(&mut self, value: u32, bits: u32) {
		for shift in 0..bits {
			self.side_bit((value >> shift) & 1 == 1);
		}
	}

	fn byte(&mut self, value: u8) {
		match self.bytes.get_mut(self.bp) {
			Some(byte) => *byte = value,
			None => self.overflow = true,
		}
		self.bp += 1;
	}

	fn shift(&mut self) {
		if self.low < 0x00ff_0000 || self.carry == 1 {
			if self.cache >= 0 {
				self.byte((self.cache as u32 + self.carry) as u8);
			}
			while self.carry_count > 0 {
				self.byte(((self.carry + 0xff) & 0xff) as u8);
				self.carry_count -= 1;
			}
			self.cache = (self.low >> 16) as i32;
			self.carry = 0;
		} else {
			self.carry_count += 1;
		}
		self.low = (self.low << 8) & 0x00ff_ffff;
	}

	fn encode(&mut self, cumfreq: u16, freq: u16) {
		let step = self.range >> 10;
		self.low += step * u32::from(cumfreq);
		if self.low >> 24 != 0 {
			self.carry = 1;
		}
		self.low &= 0x00ff_ffff;
		self.range = step * u32::from(freq);
		while self.range < 0x10000 {
			self.range <<= 8;
			self.shift();
		}
	}

	// The bits the arithmetic data takes once finished (3.3.13.5's nbits_ari).
	fn arithmetic_bits(&self) -> isize {
		let mut bits = self.bp as isize * 8 + 25 - floor_log2(self.range);
		if self.cache >= 0 {
			bits += 8;
		}
		bits + self.carry_count as isize * 8
	}

	fn finish(&mut self) {
		let mut bits: i32 = 1;
		while (self.range >> (24 - bits)) == 0 {
			bits += 1;
		}
		let mut mask = 0x00ff_ffff >> bits;
		let mut value = self.low + mask;
		let over1 = value >> 24;
		value &= 0x00ff_ffff;
		let mut high = self.low + self.range;
		let over2 = high >> 24;
		high &= 0x00ff_ffff;
		value &= !mask;
		if over1 == over2 {
			if value + mask >= high {
				bits += 1;
				mask >>= 1;
				value = ((self.low + mask) & 0x00ff_ffff) & !mask;
			}
			if value < self.low {
				self.carry = 1;
			}
		}
		self.low = value;
		while bits > 0 {
			self.shift();
			bits -= 8;
		}
		bits += 8;
		if self.carry_count > 0 {
			self.byte(self.cache as u8);
			while self.carry_count > 1 {
				self.byte(0xff);
				self.carry_count -= 1;
			}
			self.forward(0xff >> (8 - bits), bits);
		} else {
			self.forward(self.cache as u32, bits);
		}
	}

	// write_uint_forward: the top `bits` bits of `value` into the byte at the arithmetic position, the rest of it kept.
	fn forward(&mut self, value: u32, bits: i32) {
		let mut mask = 0x80u32;
		for _ in 0..bits {
			match self.bytes.get_mut(self.bp) {
				Some(byte) if value & mask == 0 => *byte &= !(mask as u8),
				Some(byte) => *byte |= mask as u8,
				None => self.overflow = true,
			}
			mask >>= 1;
		}
	}
}

/// WRITE ONE FRAME into `out` - all of it - at `sample_rate` and `frame_us` (10 000 or 7 500) exactly as Section 3.3.13
/// writes it, the spectrum coded up to `frame.lastnz`. Refused where the frame's content is not something the
/// bitstream can say, or where it does not fit `out.len()` bytes.
pub fn write_frame(frame: &Frame, sample_rate: u32, frame_us: u32, out: &mut [u8]) -> Result<(), &'static str> {
	let g = geometry(sample_rate, frame_us, out.len())?;
	let lastnz = usize::from(frame.lastnz);
	if usize::from(frame.bandwidth) > g.fs_ind || lastnz < 2 || lastnz % 2 != 0 || lastnz > g.ne {
		return Err("the frame's bandwidth or lastnz is not one this rate codes");
	}
	if frame.spectrum[lastnz..].iter().any(|&value| value != 0) || frame.spectrum.iter().any(|value| value.unsigned_abs() > 0x7fff) {
		return Err("the frame's spectrum has a line past lastnz or past fourteen bit planes");
	}
	let filters = if frame.bandwidth < 3 { 1 } else { 2 };
	if frame.tns_order.iter().any(|&order| order > 8) || frame.tns_order[filters..].iter().any(|&order| order != 0) || frame.tns_index.iter().flatten().any(|&index| index > 16) {
		return Err("the frame's TNS data is not one the bitstream can say");
	}
	if frame.pitch.is_some_and(|pitch| pitch.index > 511) || frame.noise > 7 || usize::from(frame.residual_len) > MAX_LINES {
		return Err("the frame's pitch, noise level or residual is out of range");
	}
	let joint = sns_joint(&frame.sns)?;
	out.fill(0);
	let mut w = Writer::new(out);

	// SIDE INFORMATION (3.3.13.3).
	w.side_uint(u32::from(frame.bandwidth), g.nbits_bw);
	w.side_uint((lastnz >> 1) as u32 - 1, g.nbits_lastnz);
	w.side_bit(frame.lsb_mode);
	w.side_uint(u32::from(frame.global_gain), 8);
	for &order in frame.tns_order.iter().take(filters) {
		w.side_bit(order > 0);
	}
	w.side_bit(frame.pitch.is_some());
	let sns = &frame.sns;
	let shape = usize::from(sns.shape);
	w.side_uint(u32::from(sns.ind_lf), 5);
	w.side_uint(u32::from(sns.ind_hf), 5);
	w.side_bit(shape >> 1 == 1);
	w.side_uint(u32::from(sns.gain) >> GAIN_LSB_BITS[shape], GAIN_MSB_BITS[shape]);
	w.side_bit(sns.ls_a == 1);
	if shape < 2 {
		w.side_uint(joint, 13);
		w.side_uint(joint >> 13, 12);
	} else {
		w.side_uint(joint, 12);
		w.side_uint(joint >> 12, 12);
	}
	if let Some(pitch) = frame.pitch {
		w.side_uint(u32::from(pitch.ltpf_active), 1);
		w.side_uint(u32::from(pitch.index), 9);
	}
	w.side_uint(u32::from(frame.noise), 3);

	// ARITHMETIC ENCODING (3.3.13.4.2): the TNS data, then the spectrum's 2-tuples.
	for f in 0..filters {
		let order = usize::from(frame.tns_order[f]);
		if order > 0 {
			w.encode(AC_TNS_ORDER_CUMFREQ[g.tns_lpc_weighting][order - 1], AC_TNS_ORDER_FREQ[g.tns_lpc_weighting][order - 1]);
			for k in 0..order {
				let index = usize::from(frame.tns_index[f][k]);
				w.encode(AC_TNS_COEF_CUMFREQ[k][index], AC_TNS_COEF_FREQ[k][index]);
			}
		}
	}
	// LSB mode's first bit planes and signs, in the order the residual data carries them.
	let mut lsbs = [false; 2 * MAX_LINES];
	let mut lsb_count = 0;
	let mut context = 0usize;
	for k in (0..lastnz).step_by(2) {
		let mut t = context + g.rate_flag;
		if k > g.ne / 2 {
			t += 256;
		}
		let (x0, x1) = (frame.spectrum[k], frame.spectrum[k + 1]);
		let (mut a, mut b) = (x0.unsigned_abs(), x1.unsigned_abs());
		let mut level = 0usize;
		let (mut lsb0, mut lsb1) = (false, false);
		while a.max(b) >= 4 {
			let model = usize::from(AC_SPEC_LOOKUP[t + level.min(3) * 1024]);
			w.encode(AC_SPEC_CUMFREQ[model][16], AC_SPEC_FREQ[model][16]);
			if frame.lsb_mode && level == 0 {
				lsb0 = a & 1 == 1;
				lsb1 = b & 1 == 1;
			} else {
				w.side_bit(a & 1 == 1);
				w.side_bit(b & 1 == 1);
			}
			a >>= 1;
			b >>= 1;
			level += 1;
		}
		let model = usize::from(AC_SPEC_LOOKUP[t + level.min(3) * 1024]);
		let symbol = (a + 4 * b) as usize;
		w.encode(AC_SPEC_CUMFREQ[model][symbol], AC_SPEC_FREQ[model][symbol]);
		let (mut a_lsb, mut b_lsb) = (x0.unsigned_abs(), x1.unsigned_abs());
		if frame.lsb_mode && level > 0 {
			a_lsb >>= 1;
			b_lsb >>= 1;
			lsbs[lsb_count] = lsb0;
			lsb_count += 1;
			if a_lsb == 0 && x0 != 0 {
				lsbs[lsb_count] = x0 < 0;
				lsb_count += 1;
			}
			lsbs[lsb_count] = lsb1;
			lsb_count += 1;
			if b_lsb == 0 && x1 != 0 {
				lsbs[lsb_count] = x1 < 0;
				lsb_count += 1;
			}
		}
		if a_lsb > 0 {
			w.side_bit(x0 < 0);
		}
		if b_lsb > 0 {
			w.side_bit(x1 < 0);
		}
		let level = level.min(3);
		let t = if level <= 1 { 1 + (a + b) as usize * (level + 1) } else { 12 + level };
		context = (context & 15) * 16 + t;
	}

	// RESIDUAL DATA AND FINALIZATION (3.3.13.5).
	let nbits_side = g.nbits as isize - (8 * w.side + 8 - w.mask.trailing_zeros() as isize);
	let room = g.nbits as isize - (nbits_side + w.arithmetic_bits());
	if room < 0 || w.overflow {
		return Err("the frame's content does not fit its size");
	}
	if frame.lsb_mode {
		for &bit in lsbs.iter().take(lsb_count.min(room as usize)) {
			w.side_bit(bit);
		}
	} else {
		for &bit in frame.residual.iter().take(usize::from(frame.residual_len).min(room as usize)) {
			w.side_bit(bit);
		}
	}
	w.finish();
	if w.overflow {
		return Err("the frame's content does not fit its size");
	}
	Ok(())
}

// THE SNS SECOND STAGE'S JOINT INDEX (3.3.7.3.4.2, Equations 58 to 61), its parts checked against their sizes.
fn sns_joint(sns: &Sns) -> Result<u32, &'static str> {
	let shape = usize::from(sns.shape);
	let refused = Err("the frame's SNS indices are not ones the codebooks have");
	if shape > 3 || sns.ind_lf > 31 || sns.ind_hf > 31 || sns.gain >= GAIN_LEVELS[shape] || sns.ls_a > 1 {
		return refused;
	}
	let gain_lsb = u32::from(sns.gain & 1);
	Ok(match shape {
		0 if sns.idx_a < SZ_REGULAR && sns.idx_b < 6 && sns.ls_b <= 1 => (2 * u32::from(sns.idx_b) + u32::from(sns.ls_b) + 2) * SZ_REGULAR + sns.idx_a,
		1 if sns.idx_a < SZ_REGULAR => gain_lsb * SZ_REGULAR + sns.idx_a,
		2 if sns.idx_a < SZ_NEAR => sns.idx_a,
		3 if sns.idx_a < SZ_FAR => SZ_NEAR + gain_lsb + 2 * sns.idx_a,
		_ => return refused,
	})
}

// ------------------------------------------------------------------ the tone

// THE TONE'S SCALE FACTORS: the first stage's two flattest rows near zero, LFCB[8] and HFCB[19], and a second stage
// whose one shape vector moves them all together - outlier_far with its six pulses on the first coefficient (MPVQ
// index 0, positive) and its largest gain, 19 882 / 4 096. Column 0 of the DCT matrix D is 1/4 throughout, so the
// second stage adds 19 882 / 16 384 to every scale factor: the decoder's SNS gain is 2^1.07 to 2^2.00 across the bands.
const TONE_SNS: Sns = Sns { ind_lf: 8, ind_hf: 19, shape: 3, gain: 7, ls_a: 0, idx_a: 0, ls_b: 0, idx_b: 0 };
const TONE_STAGE2: f32 = 19_882.0 / 16_384.0;

// THE TONE'S LEVEL, as log2 of what a decoder's synthesis turns into a sine's amplitude in 16-bit sample units: -12 dBFS,
// 32 768 * 10^(-12/20). One MDCT coefficient V repeated frame after frame synthesises about a sine of amplitude
// V * sqrt(2 / N_F), so the coefficient to write is that amplitude times sqrt(N_F / 2) - half of log2(N_F / 2) below, N_F
// being 80 to 480 samples. Run through the specification's synthesis - noise filling, gain, SNS and the LD-MDCT - the
// tones this writes come out between -11.7 and -12.1 dBFS.
const TONE_LOG2_AMPLITUDE: f32 = 13.006_843;
const TONE_HALF_LOG2_HALF_NF: [f32; 5] = [2.660_964, 3.160_964, 3.453_445, 3.660_964, 3.953_445];
// log2(10) / 28: one global gain step.
const LOG2_GAIN_STEP: f32 = 0.118_640_29;
// THE QUANTIZED MAGNITUDES the gain is chosen for, as log2, the first that fits. About 181 first, which puts the noise
// filling - at its quietest, 1/16 of a step on each empty line - 41 dB and more below the tone at every rate; every line
// fits so from 26 bytes up at every rate, and from 20 bytes up at 24 kHz and below. At 20 bytes
// and 32 kHz or more the zero 2-tuples below a high line take most of the frame, and the writer gives up bit planes
// half a plane at a time, down to a magnitude of 1 for the highest lines at 44.1 and 48 kHz - there the noise filling's
// power matches the tone's, each noise line 24 dB below it, and the largest global gain leaves the tone at about -18 dBFS.
const TONE_LOG2_MAGNITUDES: [f32; 8] = [7.5, 6.5, 5.5, 4.5, 3.5, 2.5, 1.5, 0.0];

/// WRITE ONE FRAME into `out` - all of it - at `sample_rate` and `frame_us` (10 000 only) whose spectrum is one line,
/// `line`, at a moderate level: about -12 dBFS once a decoder synthesises it. The same arguments write the same frame.
///
/// A line held constant frame after frame is not a sine at its own bin's centre, `(line + 0.5) * 50` Hz: the MDCT's
/// basis advances `(line + 0.5) * pi` in phase from one frame to the next, so the synthesis runs a quarter cycle a frame
/// off that centre - a tone at `100 * ceil(line / 2)` Hz (line 0 synthesises a constant, lines 1 and 2 both 100 Hz), with
/// about a tenth of its power in sidebands 100 Hz apart. At 44.1 kHz each 100 Hz is 91.875 Hz.
///
/// The frame says: the rate's full bandwidth, `lastnz` the end of the line's 2-tuple, LSB mode off, TNS off, no pitch,
/// the scale factors of `TONE_SNS`, noise filling at its quietest (7), and the line positive, with a global gain and a
/// magnitude chosen together for the level. Its one residual bit, where the frame has room for it, picks whichever of
/// the decoder's two refinements of the magnitude (-3/16, +5/16) lands nearer the level.
pub fn write_tone(line: u16, sample_rate: u32, frame_us: u32, out: &mut [u8]) -> Result<(), &'static str> {
	if frame_us != 10_000 {
		return Err("the fixture's LC3 writes 10 ms frames");
	}
	let g = geometry(sample_rate, frame_us, out.len())?;
	let line = usize::from(line);
	if line >= g.ne {
		return Err("the line is past the lines this rate codes");
	}
	let mut frame = Frame::empty();
	frame.bandwidth = g.fs_ind as u8;
	frame.lastnz = (line / 2 * 2 + 2) as u16;
	frame.sns = TONE_SNS;
	frame.noise = 7;
	frame.residual_len = 1;
	// log2 of the product of the global gain and the magnitude the decoder must see on this line: the level, less the
	// line's SNS gain.
	let want = TONE_LOG2_AMPLITUDE + TONE_HALF_LOG2_HALF_NF[g.fs_ind] - tone_scale_factor(g.fs_ind, line);
	let gg_off = -((g.nbits / (10 * (g.fs_ind + 1))).min(115) as i32) - 105 - 5 * (g.fs_ind as i32 + 1);
	let mut last = Err("the tone does not fit the frame");
	for log2_magnitude in TONE_LOG2_MAGNITUDES {
		let gain_index = (nearest((want - log2_magnitude) / LOG2_GAIN_STEP) - gg_off).clamp(0, 255);
		let exact = exp2(want - (gain_index + gg_off) as f32 * LOG2_GAIN_STEP);
		// The last rung is a magnitude of 1 whatever the gain: where even the largest global gain cannot reach the level
		// with it, the tone is quieter rather than missing.
		let magnitude = if log2_magnitude == 0.0 { 1 } else { nearest(exact).clamp(1, 0x7fff) };
		frame.global_gain = gain_index as u8;
		frame.spectrum[line] = magnitude;
		frame.residual[0] = exact - (magnitude as f32 - 0.1875) > (magnitude as f32 + 0.3125) - exact;
		last = write_frame(&frame, sample_rate, frame_us, out);
		if last.is_ok() {
			break;
		}
	}
	last
}

// THE TONE'S SCALE FACTOR on the band `line` is in, log2 of the decoder's SNS gain there: the 16 quantized scale factors
// interpolated to 64 bands as Section 3.4.7.3 does (10 ms frames have 64 bands at every rate).
fn tone_scale_factor(fs_ind: usize, line: usize) -> f32 {
	let edges = &BANDS_10MS[fs_ind];
	let band = (0..64).rev().find(|&band| usize::from(edges[band]) <= line).unwrap_or(0);
	let q = |n: usize| TONE_STAGE1[n] + TONE_STAGE2;
	match band {
		0 | 1 => q(0),
		62 => q(15) + (q(15) - q(14)) / 8.0,
		63 => q(15) + 3.0 * (q(15) - q(14)) / 8.0,
		_ => {
			let n = (band - 2) / 4;
			q(n) + [1.0, 3.0, 5.0, 7.0][(band - 2) % 4] / 8.0 * (q(n + 1) - q(n))
		}
	}
}

fn nearest(value: f32) -> i32 {
	if value < 0.0 { -((-value + 0.5) as i32) } else { (value + 0.5) as i32 }
}

// 2^x for the tone's arithmetic, with nothing from libm: the whole part into the exponent, the fraction by e^(f ln 2)'s
// series - eight terms, well inside the 0.1 % the level needs.
fn exp2(x: f32) -> f32 {
	let mut whole = x as i32;
	if whole as f32 > x {
		whole -= 1;
	}
	let y = (x - whole as f32) * core::f32::consts::LN_2;
	let (mut term, mut sum) = (1.0f32, 1.0f32);
	for n in 1..8 {
		term *= y / n as f32;
		sum += term;
	}
	sum * f32::from_bits(((whole.clamp(-126, 127) + 127) as u32) << 23)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod vectors;
