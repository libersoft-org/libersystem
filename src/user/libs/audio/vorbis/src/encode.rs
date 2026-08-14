//! Writing Ogg Vorbis, in a deliberately small corner of the format.
//!
//! Vorbis is not a codec you implement a little of and get a little of: a stream is decodable or it
//! is noise, and the line between them runs through the SETUP HEADER - the codebooks, the floor and
//! the residue configuration that tell a decoder how to read everything after it. So this file
//! starts there, and everything in it is checked against this tree's own decoder rather than
//! against the specification as I read it: the headers are written, parsed back by `header::read_*`,
//! and the values compared. A specification can be misread twice in the same direction; a decoder
//! that was written by somebody else, from the same specification, cannot be.
//!
//! WHAT THIS ENCODER CHOOSES, and it chooses the simple end of every fork the format offers:
//!
//! - ONE BLOCK SIZE. `blocksize_0 == blocksize_1`, so there is no window switching and no long/short
//!   decision to make. Vorbis uses two sizes to keep a transient from smearing across a long window;
//!   an encoder that always uses one is legal, and the cost is pre-echo on sharp attacks rather than
//!   an undecodable stream. Window switching fits behind this interface later.
//! - FLOOR 1, the piecewise-linear one, with a single partition class. Floor 0 is the LSP curve and
//!   is deprecated in practice.
//! - RESIDUE 2, the interleaved form, with one classification. One classification means the
//!   classbook says the same thing about every partition, which costs a bit per partition group and
//!   removes the whole classification search.
//! - NO CHANNEL COUPLING. Stereo is two independent channels. Coupling is where Vorbis gets much of
//!   its stereo efficiency, and it is a ratio improvement rather than a different format.
//!
//! Every one of those is a ratio decision. None of them changes whether the stream decodes, which is
//! what makes them safe to improve later behind the same interface.

use crate::header;
use alloc::vec::Vec;

/// A Vorbis packet, built bit by bit.
///
/// LSB FIRST, which is the one thing about Vorbis' bit packing that catches everybody: a value's
/// low bit goes into the lowest free bit of the current byte. Huffman CODEWORDS are the exception
/// that proves it - their bits are emitted most-significant first, one bit at a time, into the same
/// LSB-first stream - because a codeword is a path through a tree rather than a number.
pub struct BitWriter {
	bytes: Vec<u8>,
	// How many bits of the last byte are used, 0..8. Zero means the last byte is full (or there is
	// none), so the next write starts a new one.
	used: u8,
}

impl Default for BitWriter {
	fn default() -> BitWriter {
		BitWriter::new()
	}
}

impl BitWriter {
	pub fn new() -> BitWriter {
		BitWriter { bytes: Vec::new(), used: 0 }
	}

	/// Write the low `bits` bits of `value`, low bit first.
	pub fn write(&mut self, value: u32, bits: u8) -> Option<()> {
		if bits > 32 {
			return None;
		}
		let mut left: u8 = bits;
		let mut value: u32 = if bits == 32 { value } else { value & ((1u32 << bits) - 1) };
		while left > 0 {
			if self.used == 0 {
				self.bytes.try_reserve(1).ok()?;
				self.bytes.push(0);
			}
			let room: u8 = 8 - self.used;
			let take: u8 = core::cmp::min(room, left);
			let piece: u8 = (value & ((1u32 << take) - 1)) as u8;
			let at: usize = self.bytes.len() - 1;
			self.bytes[at] |= piece << self.used;
			self.used = (self.used + take) % 8;
			value >>= take;
			left -= take;
		}
		Some(())
	}

	/// Write a Huffman codeword: `bits` bits of `code`, most significant first.
	pub fn write_codeword(&mut self, code: u32, bits: u8) -> Option<()> {
		if bits == 0 || bits > 32 {
			return None;
		}
		for index in (0..bits).rev() {
			self.write((code >> index) & 1, 1)?;
		}
		Some(())
	}

	/// The packet so far. A partial last byte is included with its unused bits zero, which is what
	/// the format expects: a packet's length is in bytes and the reader stops at the framing bit.
	pub fn finish(self) -> Vec<u8> {
		self.bytes
	}

	pub fn len(&self) -> usize {
		self.bytes.len()
	}

	pub fn is_empty(&self) -> bool {
		self.bytes.is_empty()
	}
}

/// One codebook, as this encoder builds them: a length per entry, and optionally a scalar lookup
/// that turns an entry number into a value.
///
/// Only the shapes this encoder writes are here - unordered lengths, and lookup type 1 with
/// one-dimensional entries. The decoder accepts far more than that; an encoder does not have to
/// offer every shape it could, and each one it does offer is one more thing to be right about.
pub struct Codebook {
	pub lengths: Vec<u8>,
	// `Some((minimum, delta, value_bits))` for a lookup-1 book whose entry `i` decodes to
	// `minimum + i * delta`.
	pub lookup: Option<(f32, f32, u8)>,
	// The canonical codeword per entry, filled by `assign_codes`.
	codes: Vec<u32>,
}

impl Codebook {
	/// A book of `entries` entries, all the same length, which is complete exactly when `entries`
	/// is a power of two. Completeness is what the decoder's tree builder demands, and a book that
	/// is one entry short of complete is refused as underpopulated rather than tolerated.
	pub fn flat(entries: usize, lookup: Option<(f32, f32, u8)>) -> Option<Codebook> {
		if entries < 2 || !entries.is_power_of_two() || entries > 1 << 24 {
			return None;
		}
		let bits: u8 = entries.trailing_zeros() as u8;
		let mut book = Codebook { lengths: alloc::vec![bits; entries], lookup, codes: Vec::new() };
		book.assign_codes()?;
		Some(book)
	}

	/// Canonical Huffman codes from the lengths: entries are taken in index order and each gets the
	/// next free code of its length, which is exactly how the decoder's tree fills up.
	fn assign_codes(&mut self) -> Option<()> {
		let mut codes: Vec<u32> = Vec::new();
		codes.try_reserve_exact(self.lengths.len()).ok()?;
		let mut next: [u32; 33] = [0; 33];
		let mut counts: [u32; 33] = [0; 33];
		for &length in &self.lengths {
			if length == 0 || length > 32 {
				return None;
			}
			counts[length as usize] += 1;
		}
		let mut code: u32 = 0;
		for length in 1..=32usize {
			code = (code + counts[length - 1]) << 1;
			next[length] = code;
		}
		for &length in &self.lengths {
			codes.push(next[length as usize]);
			next[length as usize] += 1;
		}
		self.codes = codes;
		Some(())
	}

	pub fn code(&self, entry: usize) -> Option<(u32, u8)> {
		Some((*self.codes.get(entry)?, *self.lengths.get(entry)?))
	}

	pub fn entries(&self) -> usize {
		self.lengths.len()
	}

	/// Serialise the book into a setup header.
	fn write(&self, out: &mut BitWriter) -> Option<()> {
		out.write(0x564342, 24)?;
		// One dimension per entry: this encoder codes single values, never vectors.
		out.write(1, 16)?;
		out.write(self.lengths.len() as u32, 24)?;
		// Unordered and not sparse: every entry is used and its length is written as it is. The
		// ordered form is a compression of the length table and buys nothing at these sizes.
		out.write(0, 1)?;
		out.write(0, 1)?;
		for &length in &self.lengths {
			out.write((length - 1) as u32, 5)?;
		}
		match self.lookup {
			None => out.write(0, 4)?,
			Some((minimum, delta, value_bits)) => {
				out.write(1, 4)?;
				out.write(f32_to_vorbis(minimum), 32)?;
				out.write(f32_to_vorbis(delta), 32)?;
				out.write((value_bits - 1) as u32, 4)?;
				// `sequence_p` off: the entries are independent values rather than a running sum.
				out.write(0, 1)?;
				// One dimension, so `lookup1_values(entries, 1)` is `entries` - one multiplicand
				// per entry, and entry `i` decodes to `minimum + i * delta`.
				for index in 0..self.lengths.len() {
					out.write(index as u32, value_bits)?;
				}
			}
		}
		Some(())
	}
}

/// Vorbis' own float format, which is NOT IEEE 754: a sign, a nine-bit exponent biased by 788 and a
/// twenty-one bit mantissa, decoded as `mantissa * 2^(exponent - 788)`. It exists because the format
/// predates a promise that every decoder has an FPU with the same rounding.
///
/// Only the values this encoder writes need to survive it, and they are small integers over powers
/// of two, so the mantissa is exact.
fn f32_to_vorbis(value: f32) -> u32 {
	if value == 0.0 {
		return 0;
	}
	let negative: bool = value < 0.0;
	let mut magnitude: f32 = if negative { -value } else { value };
	let mut exponent: i32 = 0;
	// Bring the value into [2^20, 2^21) so the mantissa uses its whole width.
	while magnitude < 1_048_576.0 {
		magnitude *= 2.0;
		exponent -= 1;
	}
	while magnitude >= 2_097_152.0 {
		magnitude /= 2.0;
		exponent += 1;
	}
	let mantissa: u32 = magnitude as u32 & 0x001f_ffff;
	// TEN bits of exponent, not nine. The field is `0x7fe0_0000`, and a nine-bit mask writes a
	// number the decoder reads as a different power of two - which for these values meant one
	// coming back as 10^-70.
	let biased: u32 = ((exponent + 788) as u32) & 0x3ff;
	(u32::from(negative) << 31) | (biased << 21) | mantissa
}

/// The identification header: what the stream is.
pub fn write_ident(channels: u8, rate: u32, blocksize_log2: u8) -> Option<Vec<u8>> {
	let mut out = BitWriter::new();
	write_header_begin(&mut out, 1)?;
	out.write(0, 32)?;
	out.write(channels as u32, 8)?;
	out.write(rate, 32)?;
	// The three bitrate hints, all "no information". A decoder does nothing with them; a container
	// that carried a number nothing measured would be worse than one that says nothing.
	out.write(0, 32)?;
	out.write(0, 32)?;
	out.write(0, 32)?;
	out.write(blocksize_log2 as u32, 4)?;
	out.write(blocksize_log2 as u32, 4)?;
	out.write(1, 1)?;
	Some(out.finish())
}

/// The comment header. Version one of this encoder writes a vendor string and no comments, which is
/// the honest shape for a tool that strips metadata deliberately.
pub fn write_comment(vendor: &str) -> Option<Vec<u8>> {
	let mut out = BitWriter::new();
	write_header_begin(&mut out, 3)?;
	out.write(vendor.len() as u32, 32)?;
	for &byte in vendor.as_bytes() {
		out.write(byte as u32, 8)?;
	}
	out.write(0, 32)?;
	out.write(1, 1)?;
	Some(out.finish())
}

// The header's type byte and the "vorbis" that follows every one of them.
fn write_header_begin(out: &mut BitWriter, kind: u8) -> Option<()> {
	out.write(kind as u32, 8)?;
	for &byte in b"vorbis" {
		out.write(byte as u32, 8)?;
	}
	Some(())
}

/// How the floor divides the spectrum. The X positions are the posts the curve bends at; everything
/// between two posts is a straight line in the dB domain.
///
/// The list is deliberately dense at the bottom and sparse at the top, because that is where
/// hearing is: the distance between 100 Hz and 200 Hz matters and the distance between 10 kHz and
/// 10.1 kHz does not.
const FLOOR_POSTS: [u32; 14] = [4, 8, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768];
// Y values are coded in `ilog(range - 1)` bits, where the range comes from the multiplier: 256/m.
// Multiplier 2 gives a range of 128 and a step of 2 dB, which is as fine as this encoder's floor
// fit can honestly use.
const FLOOR_MULTIPLIER: u8 = 2;
const FLOOR_RANGE: u32 = 128;
// The residue codebook: sixty-four entries covering -1 .. +0.96875 in steps of 1/32. Residue values
// are the spectrum divided by the floor, so they sit around one by construction; the range is what
// bounds how far a single bin may exceed its own envelope.
const RESIDUE_ENTRIES: usize = 64;
const RESIDUE_DELTA: f32 = 0.031_25;
const RESIDUE_MINIMUM: f32 = -1.0;
// One partition per 32 spectral values, which is the size every residue book in the wild uses.
const RESIDUE_PARTITION: u32 = 32;

/// The setup header: the codebooks, the floor, the residue, the mapping and the mode.
///
/// The three books are the least a legal stream needs: one that codes a floor Y value, one that
/// codes a residue value, and one that says which class a residue partition is - which with a
/// single classification says the same thing every time and costs one bit.
pub fn write_setup(channels: u8, blocksize_log2: u8) -> Option<Vec<u8>> {
	let floor_book = Codebook::flat(FLOOR_RANGE as usize, None)?;
	let class_book = Codebook::flat(2, None)?;
	let value_book = Codebook::flat(RESIDUE_ENTRIES, Some((RESIDUE_MINIMUM, RESIDUE_DELTA, 8)))?;
	let mut out = BitWriter::new();
	write_header_begin(&mut out, 5)?;

	// 1. the codebooks, in the order their numbers refer to them
	out.write(2, 8)?;
	floor_book.write(&mut out)?;
	class_book.write(&mut out)?;
	value_book.write(&mut out)?;

	// 2. the time domain transforms: one, and it is the only value the format allows
	out.write(0, 6)?;
	out.write(0, 16)?;

	// 3. one floor, type 1
	out.write(0, 6)?;
	out.write(1, 16)?;
	// TWO PARTITIONS OF THE SAME CLASS, because a class carries at most eight posts: the dimension
	// field is three bits, and fourteen posts in one class writes a thirteen that reads back as
	// five. The posts are split evenly and both partitions name class 0, so the configuration is
	// still one class.
	out.write(2, 5)?;
	out.write(0, 4)?;
	out.write(0, 4)?;
	// Class 0: half the posts, no subclasses, coded with the floor book.
	out.write((FLOOR_POSTS.len() / 2 - 1) as u32, 3)?;
	out.write(0, 2)?;
	// One book number per `1 << subclasses` entries, biased by one so that zero means "no book".
	out.write(1, 8)?;
	out.write((FLOOR_MULTIPLIER - 1) as u32, 2)?;
	let rangebits: u8 = blocksize_log2 - 1;
	out.write(rangebits as u32, 4)?;
	for &post in &FLOOR_POSTS {
		out.write(post, rangebits as u32 as u8)?;
	}

	// 4. one residue, type 2
	out.write(0, 6)?;
	out.write(2, 16)?;
	// Residue 2 interleaves the channels, so its length is the whole spectrum of every channel.
	let spectrum: u32 = 1 << (blocksize_log2 - 1);
	out.write(0, 24)?;
	out.write(spectrum * channels as u32, 24)?;
	out.write(RESIDUE_PARTITION - 1, 24)?;
	out.write(0, 6)?;
	// The classbook is book 1.
	out.write(1, 8)?;
	// One classification, one pass: the cascade names book 2 in pass zero.
	out.write(1, 3)?;
	out.write(0, 1)?;
	out.write(2, 8)?;

	// 5. one mapping, type 0
	out.write(0, 6)?;
	out.write(0, 16)?;
	// One submap, no coupling, and the two reserved bits.
	out.write(0, 1)?;
	out.write(0, 1)?;
	out.write(0, 2)?;
	// THREE bytes per submap, and the first of them is the one the format reserved and nobody
	// reads. Writing two is a setup header that parses as far as the modes and then reads a mode
	// out of the bytes that were meant to be the mapping's - which is how this first failed.
	out.write(0, 8)?;
	out.write(0, 8)?;
	out.write(0, 8)?;

	// 6. one mode: long blocks off (there is only one size), and the two transform selectors the
	// format fixes at zero
	out.write(0, 6)?;
	out.write(0, 1)?;
	out.write(0, 16)?;
	out.write(0, 16)?;
	out.write(0, 8)?;

	out.write(1, 1)?;
	Some(out.finish())
}

/// The forward MDCT: `2n` windowed samples in, `n` spectral coefficients out.
///
/// DIRECT FROM THE DEFINITION, and O(n²). The decoder's inverse is a fast one because it runs on
/// every packet of every file somebody plays; an encoder runs once over a file somebody is
/// converting, and a transform whose loop IS its own definition is one that can be read against the
/// formula rather than against a paper with known bugs in its pseudocode - which the decoder's own
/// comments say theirs had.
///
/// `X[k] = sum over i of x[i] * cos(pi/n * (i + 1/2 + n/2) * (k + 1/2))`
///
/// THE SCALE COMES FROM THE DECODER, not from the specification. Vorbis' IMDCT carries a
/// normalisation that implementations place differently, and this tree has one specific inverse -
/// so the constant below is the one that makes `forward_mdct` followed by that inverse reproduce the
/// input, measured by the round-trip test rather than derived. A specification can be misread twice
/// in the same direction; a round trip through somebody else's transform cannot agree by accident.
pub fn forward_mdct(input: &[f32]) -> Option<Vec<f32>> {
	let two_n = input.len();
	if two_n < 4 || !two_n.is_power_of_two() {
		return None;
	}
	let n = two_n / 2;
	let mut out: Vec<f32> = Vec::new();
	out.try_reserve_exact(n).ok()?;
	let scale = 2.0 / n as f32;
	for k in 0..n {
		let mut sum = 0.0f32;
		for (i, &sample) in input.iter().enumerate() {
			let angle = core::f32::consts::PI / n as f32 * (i as f32 + 0.5 + n as f32 / 2.0) * (k as f32 + 0.5);
			sum += sample * libm::cosf(angle);
		}
		out.push(sum * scale);
	}
	Some(out)
}

/// The window a Vorbis block is multiplied by, on the way in and on the way out.
///
/// THE DECODER'S OWN, through `CachedBlocksizeDerived`: the slope is the `sin(pi/2 * sin²(...))`
/// curve, and the specification's own text about the right half has a note in this tree's decoder
/// saying it may be wrong. Taking the array the decoder uses means the encoder cannot disagree with
/// it about the half of the window that overlaps.
pub fn window_for(blocksize_log2: u8) -> Vec<f32> {
	let cached = crate::header_cached::CachedBlocksizeDerived::from_blocksize(blocksize_log2);
	let half = cached.window_slope.len();
	let mut window: Vec<f32> = Vec::with_capacity(half * 2);
	window.extend_from_slice(&cached.window_slope);
	window.extend(cached.window_slope.iter().rev());
	window
}

#[cfg(test)]
#[path = "encode_tests.rs"]
mod tests;
