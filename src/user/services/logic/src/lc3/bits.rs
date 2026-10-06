// Bit level frame access: the backward side information stream and the forward arithmetic coder.
//
// An LC3 frame is filled from both ends (specification section 3.5). Side information, then the
// signs and low bit planes of the spectrum, then the residual bits are written bit by bit from the
// last byte backwards (least significant bit of a byte first); the range coded data grows from the
// first byte forwards. The functions here transcribe the pseudocode of sections 3.3.13.6 (encoder)
// and 3.4.2.7 (decoder) - write_bit_backward, read_bit, ac_enc_init, ac_shift, ac_encode,
// ac_enc_finish, ac_dec_init, ac_decode - with the state kept in small structs.
//
// THE FRAME IS UNTRUSTED ON THE DECODER SIDE. Every read past either end of the frame returns zero
// and marks the reader as overrun, so a malformed frame can never index out of bounds; the decoder
// turns an overrun into a bit error, as it does with the specification's own BEC_detect checks.
// The encoder never writes outside the frame either: a write past an end is dropped (the encoder's
// bit budget keeps that from happening, the check only keeps a budget error from becoming a panic).

/// Backward bit writer (write_bit_backward / write_uint_backward).
pub(crate) struct SideWriter {
	/// Byte being written; counts down from nbytes - 1. Negative once the frame is exhausted.
	pub(crate) bp: isize,
	pub(crate) mask: u8,
}

impl SideWriter {
	pub(crate) fn new(nbytes: usize) -> SideWriter {
		SideWriter { bp: nbytes as isize - 1, mask: 1 }
	}

	pub(crate) fn bit(&mut self, bytes: &mut [u8], bit: u32) {
		if self.bp >= 0 && (self.bp as usize) < bytes.len() {
			let b = &mut bytes[self.bp as usize];
			if bit == 0 {
				*b &= !self.mask;
			} else {
				*b |= self.mask;
			}
		}
		if self.mask == 0x80 {
			self.mask = 1;
			self.bp -= 1;
		} else {
			self.mask <<= 1;
		}
	}

	pub(crate) fn uint(&mut self, bytes: &mut [u8], mut val: u32, numbits: u32) {
		for _ in 0..numbits {
			self.bit(bytes, val & 1);
			val >>= 1;
		}
	}

	/// Bits written so far: 8 bp_side + 8 - log2(mask_side) subtracted from nbits, as the
	/// specification computes nbits_side.
	pub(crate) fn used(&self, nbits: i32) -> i32 {
		nbits - (8 * self.bp as i32 + 8 - self.mask.trailing_zeros() as i32)
	}
}

/// Backward bit reader (read_bit / read_uint).
pub(crate) struct SideReader {
	pub(crate) bp: isize,
	pub(crate) mask: u8,
	pub(crate) overrun: bool,
}

impl SideReader {
	pub(crate) fn new(nbytes: usize) -> SideReader {
		SideReader { bp: nbytes as isize - 1, mask: 1, overrun: false }
	}

	pub(crate) fn bit(&mut self, bytes: &[u8]) -> u32 {
		let bit = if self.bp >= 0 && (self.bp as usize) < bytes.len() {
			(bytes[self.bp as usize] & self.mask != 0) as u32
		} else {
			self.overrun = true;
			0
		};
		if self.mask == 0x80 {
			self.mask = 1;
			self.bp -= 1;
		} else {
			self.mask <<= 1;
		}
		bit
	}

	pub(crate) fn uint(&mut self, bytes: &[u8], numbits: u32) -> u32 {
		let mut value = 0;
		for i in 0..numbits {
			value |= self.bit(bytes) << i;
		}
		value
	}

	pub(crate) fn used(&self, nbits: i32) -> i32 {
		nbits - (8 * self.bp as i32 + 8 - self.mask.trailing_zeros() as i32)
	}
}

/// Range encoder state (ac_enc_init and friends).
pub(crate) struct AcEncoder {
	pub(crate) low: u32,
	pub(crate) range: u32,
	pub(crate) cache: i32,
	pub(crate) carry: u32,
	pub(crate) carry_count: u32,
	/// Next byte to write, counting up from 0.
	pub(crate) bp: usize,
}

impl AcEncoder {
	pub(crate) fn new() -> AcEncoder {
		AcEncoder { low: 0, range: 0x00ff_ffff, cache: -1, carry: 0, carry_count: 0, bp: 0 }
	}

	fn put(&mut self, bytes: &mut [u8], v: u32) {
		if self.bp < bytes.len() {
			bytes[self.bp] = v as u8;
		}
		self.bp += 1;
	}

	fn shift(&mut self, bytes: &mut [u8]) {
		if self.low < 0x00ff_0000 || self.carry == 1 {
			if self.cache >= 0 {
				// The byte store truncates like the specification's unsigned char store.
				self.put(bytes, (self.cache as u32 + self.carry) & 0xff);
			}
			while self.carry_count > 0 {
				self.put(bytes, (self.carry + 0xff) & 0xff);
				self.carry_count -= 1;
			}
			self.cache = (self.low >> 16) as i32;
			self.carry = 0;
		} else {
			self.carry_count += 1;
		}
		self.low <<= 8;
		self.low &= 0x00ff_ffff;
	}

	pub(crate) fn encode(&mut self, bytes: &mut [u8], cum_freq: u32, sym_freq: u32) {
		let r = self.range >> 10;
		self.low += r * cum_freq;
		if (self.low >> 24) != 0 {
			self.carry = 1;
		}
		self.low &= 0x00ff_ffff;
		self.range = r * sym_freq;
		while self.range < 0x10000 {
			self.range <<= 8;
			self.shift(bytes);
		}
	}

	/// nbits_ari of section 3.3.13.5: the bits the range coder has used, its termination included.
	pub(crate) fn used(&self) -> i32 {
		let mut n = self.bp as i32 * 8 + 25 - floor_log2(self.range);
		if self.cache >= 0 {
			n += 8;
		}
		if self.carry_count > 0 {
			n += self.carry_count as i32 * 8;
		}
		n
	}

	/// ac_enc_finish.
	pub(crate) fn finish(&mut self, bytes: &mut [u8]) {
		let mut bits: i32 = 1;
		while (self.range >> (24 - bits)) == 0 {
			bits += 1;
		}
		let mut mask: u32 = 0x00ff_ffff >> bits;
		let mut val = self.low + mask;
		let over1 = val >> 24;
		val &= 0x00ff_ffff;
		let high = self.low + self.range;
		let over2 = high >> 24;
		let high = high & 0x00ff_ffff;
		val &= !mask;
		if over1 == over2 {
			if val + mask >= high {
				bits += 1;
				mask >>= 1;
				val = ((self.low + mask) & 0x00ff_ffff) & !mask;
			}
			if val < self.low {
				self.carry = 1;
			}
		}
		self.low = val;
		while bits > 0 {
			self.shift(bytes);
			bits -= 8;
		}
		bits += 8;
		if self.carry_count > 0 {
			let c = self.cache as u32;
			self.put(bytes, c & 0xff);
			while self.carry_count > 1 {
				self.put(bytes, 0xff);
				self.carry_count -= 1;
			}
			self.put_forward(bytes, 0xff >> (8 - bits), bits as u32);
		} else {
			let c = self.cache as u32;
			self.put_forward(bytes, c, bits as u32);
		}
	}

	/// write_uint_forward: the top `numbits` bits of val into the top bits of bytes[bp], the other
	/// bits of that byte (side information, residual bits) left alone. bp does not advance.
	fn put_forward(&mut self, bytes: &mut [u8], val: u32, numbits: u32) {
		if self.bp >= bytes.len() {
			return;
		}
		let mut mask = 0x80u32;
		for _ in 0..numbits {
			if val & mask == 0 {
				bytes[self.bp] &= !(mask as u8);
			} else {
				bytes[self.bp] |= mask as u8;
			}
			mask >>= 1;
		}
	}
}

/// floor(log2(x)) for x > 0.
pub(crate) fn floor_log2(x: u32) -> i32 {
	31 - x.leading_zeros() as i32
}

/// Range decoder state (ac_dec_init, ac_decode).
pub(crate) struct AcDecoder {
	pub(crate) low: u32,
	pub(crate) range: u32,
	pub(crate) bp: usize,
	/// A byte past the end of the frame was needed.
	pub(crate) overrun: bool,
	/// The BEC_detect condition of ac_decode (low outside the coding interval).
	pub(crate) error: bool,
}

impl AcDecoder {
	pub(crate) fn new(bytes: &[u8]) -> AcDecoder {
		let mut d = AcDecoder { low: 0, range: 0x00ff_ffff, bp: 0, overrun: false, error: false };
		for _ in 0..3 {
			let b = d.byte(bytes);
			d.low = (d.low << 8) + b;
		}
		d
	}

	fn byte(&mut self, bytes: &[u8]) -> u32 {
		let b = if self.bp < bytes.len() {
			bytes[self.bp] as u32
		} else {
			self.overrun = true;
			0
		};
		self.bp += 1;
		b
	}

	/// Decodes one symbol of a model with `numsym` symbols.
	pub(crate) fn decode(&mut self, bytes: &[u8], cum_freq: &[u16], sym_freq: &[u16], numsym: usize) -> usize {
		let tmp = self.range >> 10;
		if self.low >= (tmp << 10) {
			self.error = true;
		}
		let mut val = numsym - 1;
		while val > 0 && self.low < tmp * cum_freq[val] as u32 {
			val -= 1;
		}
		// With low beyond the interval (the error above) the subtraction below would wrap; the
		// frame is rejected anyway, so keep the state sane instead.
		self.low = self.low.wrapping_sub(tmp * cum_freq[val] as u32) & 0x00ff_ffff;
		self.range = tmp * sym_freq[val] as u32;
		while self.range < 0x10000 {
			let b = self.byte(bytes);
			self.low = ((self.low << 8) & 0x00ff_ffff) + b;
			self.range <<= 8;
		}
		val
	}

	/// nbits_ari of section 3.4.2.6.
	pub(crate) fn used(&self) -> i32 {
		(self.bp as i32 - 3) * 8 + 25 - floor_log2(self.range)
	}
}

#[cfg(test)]
mod tests {
	extern crate std;
	use super::*;
	use crate::lc3::tables;

	#[test]
	fn side_bits_round_trip() {
		let mut buf = [0u8; 40];
		let mut w = SideWriter::new(40);
		let values = [(5u32, 3u32), (0, 1), (1, 1), (255, 8), (0x1abcdef, 25), (3, 2)];
		for &(v, n) in &values {
			w.uint(&mut buf, v, n);
		}
		assert_eq!(w.used(320), 40);
		let mut r = SideReader::new(40);
		for &(v, n) in &values {
			assert_eq!(r.uint(&buf, n), v);
		}
		assert_eq!(r.used(320), 40);
		assert!(!r.overrun);
		// The first bit lands in the least significant bit of the last byte.
		let mut buf = [0u8; 4];
		let mut w = SideWriter::new(4);
		w.bit(&mut buf, 1);
		w.uint(&mut buf, 0b1000_0000, 8);
		assert_eq!(buf, [0, 0, 1, 1]);
	}

	#[test]
	fn reader_overrun_is_reported_not_a_panic() {
		let buf = [0xffu8; 2];
		let mut r = SideReader::new(2);
		assert_eq!(r.uint(&buf, 16), 0xffff);
		assert!(!r.overrun);
		assert_eq!(r.uint(&buf, 8), 0);
		assert!(r.overrun);
		let mut d = AcDecoder::new(&buf);
		assert!(d.overrun);
		for _ in 0..100 {
			d.decode(&buf, &tables::AC_SPEC_CUMFREQ[0], &tables::AC_SPEC_FREQ[0], 17);
		}
	}

	/// Encodes symbol sequences with the specification's models and decodes them back.
	#[test]
	fn arithmetic_coder_round_trip() {
		let mut seed = 12345u32;
		let mut rand = move || {
			seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
			seed >> 8
		};
		for trial in 0..300 {
			let mut buf = [0u8; 400];
			let mut ac = AcEncoder::new();
			// A mix of the three kinds of models the bitstream uses.
			let mut syms = std::vec::Vec::new();
			let count = 1 + rand() as usize % 300;
			for _ in 0..count {
				let kind = rand() % 3;
				let s = match kind {
					0 => {
						let m = (rand() % 64) as usize;
						// Bias towards likely symbols as real spectra are.
						let mut sym = (rand() % 17) as usize;
						if trial % 2 == 0 && tables::AC_SPEC_FREQ[m][sym] < 16 {
							sym = 0;
						}
						ac.encode(&mut buf, tables::AC_SPEC_CUMFREQ[m][sym] as u32, tables::AC_SPEC_FREQ[m][sym] as u32);
						(0, m, sym)
					}
					1 => {
						let m = (rand() % 2) as usize;
						let sym = (rand() % 8) as usize;
						ac.encode(&mut buf, tables::AC_TNS_ORDER_CUMFREQ[m][sym] as u32, tables::AC_TNS_ORDER_FREQ[m][sym] as u32);
						(1, m, sym)
					}
					_ => {
						let m = (rand() % 8) as usize;
						let sym = (rand() % 17) as usize;
						ac.encode(&mut buf, tables::AC_TNS_COEF_CUMFREQ[m][sym] as u32, tables::AC_TNS_COEF_FREQ[m][sym] as u32);
						(2, m, sym)
					}
				};
				syms.push(s);
			}
			let used = ac.used();
			ac.finish(&mut buf);
			assert!(used <= 8 * 400, "trial {trial}");
			// The bits after the used ones belong to the side information and residual data in a
			// real frame, and the decoder must not depend on them: fill them with garbage.
			let used = used as usize;
			if !used.is_multiple_of(8) {
				let keep = 0xffu8 << (8 - used % 8);
				buf[used / 8] = (buf[used / 8] & keep) | (rand() as u8 & !keep);
			}
			for b in buf.iter_mut().skip(used.div_ceil(8)) {
				*b = rand() as u8;
			}
			let used = used as i32;
			let mut d = AcDecoder::new(&buf);
			for (i, &(kind, m, sym)) in syms.iter().enumerate() {
				let got = match kind {
					0 => d.decode(&buf, &tables::AC_SPEC_CUMFREQ[m], &tables::AC_SPEC_FREQ[m], 17),
					1 => d.decode(&buf, &tables::AC_TNS_ORDER_CUMFREQ[m], &tables::AC_TNS_ORDER_FREQ[m], 8),
					_ => d.decode(&buf, &tables::AC_TNS_COEF_CUMFREQ[m], &tables::AC_TNS_COEF_FREQ[m], 17),
				};
				assert_eq!(got, sym, "trial {trial} symbol {i}");
				assert!(!d.error);
			}
			// Both sides agree on how many bits the coded data took.
			assert_eq!(d.used(), used, "trial {trial}");
		}
	}
}
