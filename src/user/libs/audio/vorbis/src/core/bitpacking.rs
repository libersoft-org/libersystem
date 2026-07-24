// Vorbis decoder written in Rust
//
// Copyright (c) 2016 est31 <MTest31@outlook.com>
// and contributors. All rights reserved.
// Licensed under MIT license, or Apache 2 license,
// at your option. Please see the LICENSE file
// attached to this source distribution for details.

/*!
Vorbis bitpacking layer

Functionality to read content from the bitpacking layer.

Implements vorbis spec, section 2.

The most important struct of this mod is the `BitpackCursor` struct.
It can be instantiated using `BitpackCursor::new()`.

Note that this implementation doesn't fully align with the spec in the regard that it assumes a byte is an octet.
This is no problem on most architectures.
This non-alignment to the spec is due to the fact that the rust language is highly leaned towards byte == u8,
and doesn't even have a builtin single byte type.
*/

use crate::huffman_tree::{PeekedDataLookupResult, VorbisHuffmanTree};

/// A Cursor on slices to read numbers and bitflags, bit aligned.
pub struct BitpackCursor<'a> {
	bit_cursor: u8,
	byte_cursor: usize,
	inner: &'a [u8],
}

macro_rules! sign_extend {
	( $num:expr, $desttype:ident, $bit_cnt_large:expr, $bit_cnt_small:expr) => {{
		let n = $num;
		let res: $desttype = n as $desttype;
		let k: u8 = $bit_cnt_large - $bit_cnt_small;
		res << k >> k
	}};
}

/// Returns `num` bits of 1 (but never more than 8).
fn mask_bits(num: u8) -> u8 {
	!((!0u8).wrapping_shl(num as u32)) | if num >= 8 { 0xff } else { 0 }
}

// Same as mask_bits but different in a special case: for num % 8 == 0
// Make sure that 0 <= num <= 8.
fn bmask_bits(num: u8) -> u8 {
	(!0u8).wrapping_shr(8 - num as u32)
}

// The main macro to read bit aligned
// Note that `$octetnum` is the number of octets in $bitnum ($bitnum / 8 rounded down)
macro_rules! bpc_read_body {
	( $rettype:ident, $bitnum:expr, $octetnum:expr, $selfarg:expr ) => {{
		let last_octet_partial: usize = ($bitnum as i8 - $octetnum as i8 * 8 > 0) as usize;
		let octetnum_rounded_up: usize = last_octet_partial + $octetnum;
		let bit_cursor_after = ($selfarg.bit_cursor + $bitnum) % 8;

		if ($selfarg.bit_cursor + $bitnum) as usize > 8 * octetnum_rounded_up {
			/*println!("Reading {} bits (octetnum={}, last_partial={}, total_touched={}+1)",
				$bitnum, $octetnum, last_octet_partial, $octetnum + last_octet_partial);
			println!("    byte_c={}; bit_c={}", $selfarg.byte_cursor, $selfarg.bit_cursor);// */
			/*print!("Reading {} bits (byte_c={}; bit_c={}) [] = {:?}", $bitnum,
			$selfarg.byte_cursor, $selfarg.bit_cursor,
			&$selfarg.inner[$selfarg.byte_cursor .. $selfarg.byte_cursor +
			1 + octetnum_rounded_up]);// */
			if $selfarg.byte_cursor + 1 + octetnum_rounded_up > $selfarg.inner.len() {
				//println!(" => Out of bounds :\\");
				return Err(());
			}
			let buf = &$selfarg.inner[$selfarg.byte_cursor..$selfarg.byte_cursor + 1 + octetnum_rounded_up];
			let mut res: $rettype = buf[0] as $rettype;
			res >>= $selfarg.bit_cursor;
			let mut cur_bit_cursor = 8 - $selfarg.bit_cursor;
			for i in 1..octetnum_rounded_up {
				res |= (buf[i] as $rettype) << cur_bit_cursor;
				cur_bit_cursor += 8;
			}
			let last_bits = buf[octetnum_rounded_up] & mask_bits(bit_cursor_after);
			res |= (last_bits as $rettype) << cur_bit_cursor;
			$selfarg.byte_cursor += octetnum_rounded_up;
			$selfarg.bit_cursor = bit_cursor_after;
			//println!(" => {:?}", res);
			Ok(res)
		} else {
			/*println!("Reading {} bits (octetnum={}, last_partial={}, total_touched={})",
				$bitnum, $octetnum, last_octet_partial, $octetnum + last_octet_partial);
			println!("    byte_c={}; bit_c={}", $selfarg.byte_cursor, $selfarg.bit_cursor);// */
			/*print!("Reading {} bits (byte_c={}; bit_c={}) [] = {:?}", $bitnum,
			$selfarg.byte_cursor, $selfarg.bit_cursor,
			&$selfarg.inner[$selfarg.byte_cursor .. $selfarg.byte_cursor +
			octetnum_rounded_up]);// */
			if $selfarg.byte_cursor + octetnum_rounded_up > $selfarg.inner.len() {
				//println!(" => Out of bounds :\\");
				return Err(());
			}
			let buf = &$selfarg.inner[$selfarg.byte_cursor..$selfarg.byte_cursor + octetnum_rounded_up];
			let mut res: $rettype = buf[0] as $rettype;
			res >>= $selfarg.bit_cursor;
			if $bitnum <= 8 {
				res &= mask_bits($bitnum) as $rettype;
			}
			let mut cur_bit_cursor = 8 - $selfarg.bit_cursor;
			for i in 1..octetnum_rounded_up - 1 {
				res |= (buf[i] as $rettype) << cur_bit_cursor;
				cur_bit_cursor += 8;
			}
			if $bitnum > 8 {
				let last_bits = buf[octetnum_rounded_up - 1] & bmask_bits(bit_cursor_after);
				res |= (last_bits as $rettype) << cur_bit_cursor;
			}
			$selfarg.byte_cursor += $octetnum;
			$selfarg.byte_cursor += ($selfarg.bit_cursor == 8 - ($bitnum % 8)) as usize;
			$selfarg.bit_cursor = bit_cursor_after;
			//println!(" => {:?}", res);
			Ok(res)
		}
	}};
}

// The main macro to peek bit aligned
// Note that `$octetnum` is the number of octets in $bitnum ($bitnum / 8 rounded down)
macro_rules! bpc_peek_body {
	( $rettype:ident, $bitnum:expr, $octetnum:expr, $selfarg:expr ) => {{
		let last_octet_partial: usize = ($bitnum as i8 - $octetnum as i8 * 8 > 0) as usize;
		let octetnum_rounded_up: usize = last_octet_partial + $octetnum;
		let bit_cursor_after = ($selfarg.bit_cursor + $bitnum) % 8;

		if ($selfarg.bit_cursor + $bitnum) as usize > 8 * octetnum_rounded_up {
			/*println!("Reading {} bits (octetnum={}, last_partial={}, total_touched={}+1)",
				$bitnum, $octetnum, last_octet_partial, $octetnum + last_octet_partial);
			println!("    byte_c={}; bit_c={}", $selfarg.byte_cursor, $selfarg.bit_cursor);// */
			/*print!("Reading {} bits (byte_c={}; bit_c={}) [] = {:?}", $bitnum,
			$selfarg.byte_cursor, $selfarg.bit_cursor,
			&$selfarg.inner[$selfarg.byte_cursor .. $selfarg.byte_cursor +
			1 + octetnum_rounded_up]);// */
			if $selfarg.byte_cursor + 1 + octetnum_rounded_up > $selfarg.inner.len() {
				//println!(" => Out of bounds :\\");
				return Err(());
			}
			let buf = &$selfarg.inner[$selfarg.byte_cursor..$selfarg.byte_cursor + 1 + octetnum_rounded_up];
			let mut res: $rettype = buf[0] as $rettype;
			res >>= $selfarg.bit_cursor;
			let mut cur_bit_cursor = 8 - $selfarg.bit_cursor;
			for i in 1..octetnum_rounded_up {
				res |= (buf[i] as $rettype) << cur_bit_cursor;
				cur_bit_cursor += 8;
			}
			let last_bits = buf[octetnum_rounded_up] & mask_bits(bit_cursor_after);
			res |= (last_bits as $rettype) << cur_bit_cursor;
			//println!(" => {:?}", res);
			Ok(res)
		} else {
			/*println!("Reading {} bits (octetnum={}, last_partial={}, total_touched={})",
				$bitnum, $octetnum, last_octet_partial, $octetnum + last_octet_partial);
			println!("    byte_c={}; bit_c={}", $selfarg.byte_cursor, $selfarg.bit_cursor);// */
			/*print!("Reading {} bits (byte_c={}; bit_c={}) [] = {:?}", $bitnum,
			$selfarg.byte_cursor, $selfarg.bit_cursor,
			&$selfarg.inner[$selfarg.byte_cursor .. $selfarg.byte_cursor +
			octetnum_rounded_up]);// */
			if $selfarg.byte_cursor + octetnum_rounded_up > $selfarg.inner.len() {
				//println!(" => Out of bounds :\\");
				return Err(());
			}
			let buf = &$selfarg.inner[$selfarg.byte_cursor..$selfarg.byte_cursor + octetnum_rounded_up];
			let mut res: $rettype = buf[0] as $rettype;
			res >>= $selfarg.bit_cursor;
			if $bitnum <= 8 {
				res &= mask_bits($bitnum) as $rettype;
			}
			let mut cur_bit_cursor = 8 - $selfarg.bit_cursor;
			for i in 1..octetnum_rounded_up - 1 {
				res |= (buf[i] as $rettype) << cur_bit_cursor;
				cur_bit_cursor += 8;
			}
			if $bitnum > 8 {
				let last_bits = buf[octetnum_rounded_up - 1] & bmask_bits(bit_cursor_after);
				res |= (last_bits as $rettype) << cur_bit_cursor;
			}
			//println!(" => {:?}", res);
			Ok(res)
		}
	}};
}

// The main macro to advance bit aligned
// Note that `$octetnum` is the number of octets in $bitnum ($bitnum / 8 rounded down)
macro_rules! bpc_advance_body {
	( $bitnum:expr, $octetnum:expr, $selfarg:expr ) => {{
		let last_octet_partial: usize = ($bitnum as i8 - $octetnum as i8 * 8 > 0) as usize;
		let octetnum_rounded_up: usize = last_octet_partial + $octetnum;
		let bit_cursor_after = ($selfarg.bit_cursor + $bitnum) % 8;

		if ($selfarg.bit_cursor + $bitnum) as usize > 8 * octetnum_rounded_up {
			$selfarg.byte_cursor += octetnum_rounded_up;
			$selfarg.bit_cursor = bit_cursor_after;
			//println!(" => {:?}", res);
			Ok(())
		} else {
			$selfarg.byte_cursor += $octetnum;
			$selfarg.byte_cursor += ($selfarg.bit_cursor == 8 - ($bitnum % 8)) as usize;
			$selfarg.bit_cursor = bit_cursor_after;
			//println!(" => {:?}", res);
			Ok(())
		}
	}};
}

macro_rules! uk_reader {
	( $fnname:ident, $rettype:ident, $bitnum:expr, $octetnum:expr) => {
		#[inline]
		pub fn $fnname(&mut self) -> Result<$rettype, ()> {
			bpc_read_body!($rettype, $bitnum, $octetnum, self)
		}
	};
}

macro_rules! ik_reader {
	( $fnname:ident, $rettype:ident, $bitnum_of_rettype:expr, $bitnum:expr, $octetnum:expr) => {
		#[inline]
		pub fn $fnname(&mut self) -> Result<$rettype, ()> {
			Ok(sign_extend!(try_old!(bpc_read_body!($rettype, $bitnum, $octetnum, self)), $rettype, $bitnum_of_rettype, $bitnum))
		}
	};
}

macro_rules! ik_dynamic_reader {
	( $fnname:ident, $rettype:ident, $bitnum_of_rettype:expr) => {
		#[inline]
		pub fn $fnname(&mut self, bit_num: u8) -> Result<$rettype, ()> {
			let octet_num: usize = (bit_num / 8) as usize;
			assert!(bit_num <= $bitnum_of_rettype);
			Ok(sign_extend!(try_old!(bpc_read_body!($rettype, bit_num, octet_num, self)), $rettype, $bitnum_of_rettype, bit_num))
		}
	};
}

macro_rules! uk_dynamic_reader {
	( $fnname:ident, $rettype:ident, $bit_num_max:expr) => {
		#[inline]
		pub fn $fnname(&mut self, bit_num: u8) -> Result<$rettype, ()> {
			let octet_num: usize = (bit_num / 8) as usize;
			if bit_num == 0 {
				// TODO: one day let bpc_read_body handle this,
				// if its smartly doable in there.
				// For why it is required, see comment in the
				// test_bitpacking_reader_empty function.
				return Ok(0);
			}
			assert!(bit_num <= $bit_num_max);
			bpc_read_body!($rettype, bit_num, octet_num, self)
		}
	};
}

fn float32_unpack(val: u32) -> f32 {
	let sgn = val & 0x80000000;
	let exp = (val & 0x7fe00000) >> 21;
	let mantissa = (val & 0x1fffff) as f64;
	let signed_mantissa = if sgn != 0 { -mantissa } else { mantissa };
	return signed_mantissa as f32 * libm::exp2f(exp as f32 - 788.0);
}

// allow some code that is only used in the tests
#[allow(dead_code)]
impl<'a> BitpackCursor<'a> {
	/// Creates a new `BitpackCursor` for the given data array
	pub fn new(arr: &'a [u8]) -> BitpackCursor<'a> {
		return BitpackCursor::<'a> { bit_cursor: 0, byte_cursor: 0, inner: arr };
	}

	// Unsigned, non-dynamic reader methods

	// u32 based

	// TODO add here if needed
	uk_reader!(read_u32, u32, 32, 4);
	// TODO add here if needed
	uk_reader!(read_u24, u32, 24, 3);
	// TODO add here if needed

	// u16 based

	uk_reader!(read_u16, u16, 16, 2);

	// TODO add here if needed
	uk_reader!(read_u13, u16, 13, 1);
	// TODO add here if needed

	// u8 based
	uk_reader!(read_u8, u8, 8, 1);
	uk_reader!(read_u7, u8, 7, 0);
	uk_reader!(read_u6, u8, 6, 0);
	uk_reader!(read_u5, u8, 5, 0);
	uk_reader!(read_u4, u8, 4, 0);
	uk_reader!(read_u3, u8, 3, 0);
	uk_reader!(read_u2, u8, 2, 0);
	uk_reader!(read_u1, u8, 1, 0);

	// Returning bool:
	#[inline]
	pub fn read_bit_flag(&mut self) -> Result<bool, ()> {
		return Ok(try_old!(self.read_u1()) == 1);
	}

	// Unsigned dynamic reader methods
	// They panic if you give them invalid params
	// (bit_num larger than maximum allowed bit number for the type)
	uk_dynamic_reader!(read_dyn_u8, u8, 8);
	uk_dynamic_reader!(read_dyn_u16, u16, 16);
	uk_dynamic_reader!(read_dyn_u32, u32, 32);
	uk_dynamic_reader!(read_dyn_u64, u64, 64);

	// Signed non-dynamic reader methods

	ik_reader!(read_i32, i32, 32, 32, 4);
	// TODO add here if needed

	ik_reader!(read_i8, i8, 8, 8, 1);
	ik_reader!(read_i7, i8, 8, 7, 0);
	// TODO add here if needed

	// Signed dynamic reader methods
	// They panic if you give them invalid params
	// (bit_num larger than maximum allowed bit number for the type)
	ik_dynamic_reader!(read_dyn_i8, i8, 8);
	ik_dynamic_reader!(read_dyn_i16, i16, 16);
	ik_dynamic_reader!(read_dyn_i32, i32, 32);

	// Float reading methods

	/// Reads a single floating point number in the vorbis-float32 format
	pub fn read_f32(&mut self) -> Result<f32, ()> {
		let val = try_old!(self.read_u32());
		Ok(float32_unpack(val))
	}

	/// Peeks 8 bits of non read yet content without advancing the reader
	#[inline]
	pub fn peek_u8(&self) -> Result<u8, ()> {
		bpc_peek_body!(u8, 8, 1, self)
	}

	// Advances the reader by the given number of bits (up to 8).
	pub fn advance_dyn_u8(&mut self, bit_num: u8) -> Result<(), ()> {
		let octet_num: usize = (bit_num / 8) as usize;
		if bit_num == 0 {
			// TODO: one day let bpc_advance_body handle this,
			// if its smartly doable in there.
			// For why it is required, see comment in the
			// test_bitpacking_reader_empty function.
			return Ok(());
		}
		assert!(bit_num <= 8);
		bpc_advance_body!(bit_num, octet_num, self)
	}

	/// Reads a huffman word using the codebook abstraction
	pub fn read_huffman(&mut self, tree: &VorbisHuffmanTree) -> Result<u32, ()> {
		//let mut c :usize = 0;
		//let mut w :usize = 0;
		let mut iter = match self.peek_u8() {
			Ok(data) => match tree.lookup_peeked_data(8, data as u32) {
				PeekedDataLookupResult::Iter(advance, iter) => {
					try_old!(self.advance_dyn_u8(advance));
					iter
				}
				PeekedDataLookupResult::PayloadFound(advance, payload) => {
					try_old!(self.advance_dyn_u8(advance));
					return Ok(payload);
				}
			},
			Err(_) => tree.iter(),
		};

		loop {
			let b = try_old!(self.read_bit_flag());
			/*
			c +=1;
			w >>= 1;
			w |= (b as usize) << 63;
			// Put this into the Some arm of the match below in order to debug:
			{print!("({}:{}:{}) ", w >> (64 - c), v, c); }
			// */
			match iter.next(b) {
				Some(v) => return Ok(v),
				None => (),
			}
		}
	}
}

#[cfg(test)]
#[path = "bitpacking/tests.rs"]
mod tests;
