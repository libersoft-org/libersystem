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

/// The floor's X positions, in the order floor 1 codes them.
///
/// The first two are not in `FLOOR_POSTS` and are not optional: floor 1 always begins with position
/// 0 and position `1 << rangebits`, and their Y values are written literally rather than as deltas.
/// Every other post is coded against a prediction from its neighbours, which is why the order here
/// is the CODING order and not the sorted one.
pub fn floor_x_list(blocksize_log2: u8) -> Vec<u32> {
	let range: u32 = 1 << (blocksize_log2 - 1);
	let mut list: Vec<u32> = Vec::with_capacity(FLOOR_POSTS.len() + 2);
	list.push(0);
	list.push(range);
	list.extend_from_slice(&FLOOR_POSTS);
	list
}

/// Turn the Y values this encoder WANTS into the values it must write.
///
/// THE PACKET DOES NOT CARRY THE CURVE, it carries corrections to a prediction. For every post
/// after the first two the decoder draws a line between that post's already-fixed neighbours, reads
/// the point at this X, and adds the coded value to it under a rule with two branches: a small
/// correction is a zig-zag around the prediction (even up, odd down), and a large one is measured
/// from whichever side has more room. This inverts that, branch for branch.
///
/// A post whose wanted value IS the prediction codes as zero, which the decoder reads as "no
/// correction" and marks as not needing its own line segment. That costs nothing and changes no
/// sample: the prediction is the line, so skipping the post draws the same curve.
///
/// Returns `None` when a wanted value cannot be reached at all - the correction would not fit in
/// the range the codebook covers. The caller's fit is what keeps that from happening; answering
/// `None` rather than clamping means a fit that drifts is a failure here and not a quiet
/// half-octave of error in the output.
pub fn code_floor(final_y: &[u32], blocksize_log2: u8) -> Option<Vec<u32>> {
	let x_list = floor_x_list(blocksize_log2);
	if final_y.len() != x_list.len() {
		return None;
	}
	let range: i32 = FLOOR_RANGE as i32;
	let mut coded: Vec<u32> = Vec::with_capacity(final_y.len());
	// The decoder's own view, rebuilt as we go: a prediction is made against the values the decoder
	// will HAVE, which for the first two posts is what was written literally.
	let mut fixed: Vec<i32> = Vec::with_capacity(final_y.len());
	for index in 0..2 {
		let value = final_y[index] as i32;
		if value >= range {
			return None;
		}
		coded.push(value as u32);
		fixed.push(value);
	}
	for index in 2..x_list.len() {
		let low = crate::audio::low_neighbor(&x_list, index);
		let high = crate::audio::high_neighbor(&x_list, index);
		let predicted: i32 = crate::audio::render_point(low.1, fixed[low.0] as u32, high.1, fixed[high.0] as u32, x_list[index]) as i32;
		let wanted: i32 = final_y[index] as i32;
		let error: i32 = wanted - predicted;
		if error == 0 {
			coded.push(0);
			fixed.push(predicted);
			continue;
		}
		let highroom: i32 = range - predicted;
		let lowroom: i32 = predicted;
		let room: i32 = core::cmp::min(highroom, lowroom) * 2;
		// The zig-zag branch, which is the cheap one: an even value steps up by half of it and an
		// odd value steps down by half of one more. Only usable while the result stays under `room`,
		// because that is the boundary the decoder itself tests.
		let small: i32 = if error > 0 { 2 * error } else { -2 * error - 1 };
		let value: i32 = if small < room {
			small
		} else if highroom > lowroom {
			error + lowroom
		} else {
			highroom - 1 - error
		};
		if value <= 0 || value >= range {
			// Zero would mean "no correction" and this post wanted one; past the range there is no
			// codeword. Either way the caller asked for a curve this floor cannot draw.
			return None;
		}
		coded.push(value as u32);
		fixed.push(wanted);
	}
	Some(coded)
}

/// The curve the decoder will draw from `coded`, in linear amplitude, one value per spectral bin.
///
/// Runs the decoder's own two stages - the amplitude fixup and the line synthesis - so this is what
/// the decoder WILL do rather than what the encoder hopes it does. The fit below uses it to check
/// its own work, which is the only way to be sure a straight line between two posts did not dip
/// under the spectrum somewhere in the middle.
pub fn render_floor(coded: &[u32], blocksize_log2: u8) -> Option<Vec<f32>> {
	let x_list = floor_x_list(blocksize_log2);
	if coded.len() != x_list.len() {
		return None;
	}
	let n: usize = 1 << (blocksize_log2 - 1);
	let range: i32 = FLOOR_RANGE as i32;
	// Stage one: the fixup, exactly as `floor_one_curve_compute_amplitude` runs it.
	let mut final_y: Vec<i32> = alloc::vec![coded[0] as i32, coded[1] as i32];
	let mut step2: Vec<bool> = alloc::vec![true, true];
	for index in 2..x_list.len() {
		let low = crate::audio::low_neighbor(&x_list, index);
		let high = crate::audio::high_neighbor(&x_list, index);
		let predicted: i32 = crate::audio::render_point(low.1, final_y[low.0] as u32, high.1, final_y[high.0] as u32, x_list[index]) as i32;
		let value: i32 = coded[index] as i32;
		let highroom: i32 = range - predicted;
		let lowroom: i32 = predicted;
		let room: i32 = core::cmp::min(highroom, lowroom) * 2;
		if value > 0 {
			step2[low.0] = true;
			step2[high.0] = true;
			step2.push(true);
			final_y.push(if value >= room { if highroom > lowroom { predicted + value - lowroom } else { predicted - value + highroom - 1 } } else { predicted + (if value % 2 == 1 { -value - 1 } else { value } >> 1) });
		} else {
			final_y.push(predicted);
			step2.push(false);
		}
	}
	for value in &mut final_y {
		*value = core::cmp::min(range - 1, *value);
	}
	// Stage two: the synthesis, over the posts in X order.
	let mut order: Vec<usize> = (0..x_list.len()).collect();
	order.sort_by_key(|&index| x_list[index]);
	let mut indices: Vec<u32> = Vec::with_capacity(n);
	let (mut hx, mut hy, mut lx, mut ly): (u32, u32, u32, u32) = (0, 0, 0, final_y[order[0]] as u32 * FLOOR_MULTIPLIER as u32);
	for &index in order.iter().skip(1) {
		if !step2[index] {
			continue;
		}
		hy = final_y[index] as u32 * FLOOR_MULTIPLIER as u32;
		hx = x_list[index];
		crate::audio::render_line(lx, ly, hx, hy, &mut indices);
		lx = hx;
		ly = hy;
	}
	if hx < n as u32 {
		crate::audio::render_line(hx, hy, n as u32, hy, &mut indices);
	} else if hx > n as u32 {
		indices.truncate(n);
	}
	if indices.len() != n {
		return None;
	}
	Some(indices.into_iter().map(|index| crate::audio::FLOOR1_INVERSE_DB_TABLE[index as usize]).collect())
}

/// Choose the floor for one channel's spectrum: the coded Y values whose curve sits AT OR ABOVE
/// every magnitude in it.
///
/// ABOVE, NOT THROUGH. The residue this floor divides out is coded by a book covering -1 .. +0.97,
/// so a bin whose magnitude exceeds its floor cannot be represented and would come back clipped.
/// Fitting the envelope over the spectrum rather than through it is what keeps every residue in
/// range by construction, and it is why the loop below only ever raises.
///
/// The first pass takes, for each post, the loudest bin in the half-segments either side of it -
/// so a peak between two posts lifts both. That is not enough on its own: the curve between two
/// posts is a straight line in the dB domain, and a peak in the middle of a long segment can still
/// stand above it. So the fit is checked against the rendered curve and any post whose segment is
/// short gets raised, up to a bounded number of passes. It converges because every pass strictly
/// raises at least one post and the range is finite; the bound is there so that a spectrum it
/// cannot fit ends as a refusal rather than a loop.
pub fn fit_floor(magnitude: &[f32], blocksize_log2: u8) -> Option<Vec<u32>> {
	let n: usize = 1 << (blocksize_log2 - 1);
	if magnitude.len() != n {
		return None;
	}
	// A CEILING THIS CONFIGURATION CANNOT REACH IS A REFUSAL, not a quiet attenuation.
	//
	// The multiplier is 2, so a coded Y of `y` lands on table entry `2y` and the largest one this
	// floor can name is entry 254 - just under the table's top of 1.0. A spectrum with a bin above
	// that has no floor to sit under, and the alternatives are both worse than saying so: fitting
	// as high as possible clips that bin's residue, and there is no gain field in the format to
	// record the scaling with, so an encoder that scaled here would change the output level and
	// nothing in the stream would say it had. Normalising is the caller's decision to make and to
	// be seen making.
	let ceiling: f32 = crate::audio::FLOOR1_INVERSE_DB_TABLE[((FLOOR_RANGE - 1) * FLOOR_MULTIPLIER as u32) as usize];
	if magnitude.iter().any(|value| (if *value < 0.0 { -*value } else { *value }) > ceiling) {
		return None;
	}
	let x_list = floor_x_list(blocksize_log2);
	let mut order: Vec<usize> = (0..x_list.len()).collect();
	order.sort_by_key(|&index| x_list[index]);
	// The first pass: each post covers the bins halfway to its neighbours on either side.
	let mut final_y: Vec<u32> = alloc::vec![0; x_list.len()];
	for (position, &index) in order.iter().enumerate() {
		let x = x_list[index] as usize;
		let previous = if position == 0 { 0 } else { x_list[order[position - 1]] as usize };
		let next = if position + 1 == order.len() { n } else { x_list[order[position + 1]] as usize };
		let from = (previous + x) / 2;
		let to = core::cmp::min((x + next) / 2 + 1, n);
		let mut peak = 0.0f32;
		for &value in magnitude.iter().take(to).skip(from) {
			let magnitude = if value < 0.0 { -value } else { value };
			if magnitude > peak {
				peak = magnitude;
			}
		}
		final_y[index] = y_for(peak);
	}
	// The repair passes.
	for _ in 0..8 {
		let coded = code_floor(&final_y, blocksize_log2)?;
		let curve = render_floor(&coded, blocksize_log2)?;
		let mut raised = false;
		for (bin, &value) in magnitude.iter().enumerate() {
			let magnitude = if value < 0.0 { -value } else { value };
			if magnitude <= curve[bin] {
				continue;
			}
			// The bin stands above its floor. Raise the two posts the segment it falls in is drawn
			// between - both, because raising only the nearer one tilts the line and can leave the
			// far half of the segment as short as it was.
			let wanted = y_for(magnitude);
			let mut low = 0usize;
			let mut high = order.len() - 1;
			for (position, &index) in order.iter().enumerate() {
				if (x_list[index] as usize) <= bin {
					low = position;
				}
				if x_list[index] as usize >= bin && position < high {
					high = position;
				}
			}
			for &position in &[low, high] {
				let index = order[position];
				if final_y[index] < wanted {
					final_y[index] = wanted;
					raised = true;
				}
			}
		}
		if !raised {
			return Some(coded);
		}
	}
	None
}

/// The smallest floor Y value whose curve is at least `magnitude`.
///
/// The table the decoder indexes runs from a value too small to hear up to exactly 1.0, and the
/// multiplier means a coded Y of `y` lands on table entry `y * 2`. So this is a search over the
/// entries this configuration can actually reach; `fit_floor` has already refused anything above
/// the top of that range, so the saturation here is only ever the exact top.

/// The largest magnitude this floor configuration can sit above. A caller scales its spectrum to
/// this before fitting, and does so where the decision is visible.
pub fn floor_ceiling() -> f32 {
	crate::audio::FLOOR1_INVERSE_DB_TABLE[((FLOOR_RANGE - 1) * FLOOR_MULTIPLIER as u32) as usize]
}

fn y_for(magnitude: f32) -> u32 {
	let mut low: u32 = 0;
	let mut high: u32 = FLOOR_RANGE - 1;
	while low < high {
		let middle = (low + high) / 2;
		if crate::audio::FLOOR1_INVERSE_DB_TABLE[(middle * FLOOR_MULTIPLIER as u32) as usize] >= magnitude {
			high = middle;
		} else {
			low = middle + 1;
		}
	}
	low
}

/// The codeword for one entry: `(code, bits)`, most significant bit first.
impl Codebook {
	pub fn codeword(&self, entry: usize) -> Option<(u32, u8)> {
		Some((*self.codes.get(entry)?, *self.lengths.get(entry)?))
	}
}

/// One audio packet: the floor and the residue for every channel.
///
/// `spectra` is one MDCT spectrum per channel, each `1 << (blocksize_log2 - 1)` long and already
/// scaled into the range `floor_ceiling` allows - see `fit_floor` for why that scaling is the
/// caller's to make and to be seen making.
///
/// THE PACKET IS THE FLOOR AND WHAT IS LEFT AFTER IT. A floor is fitted over each channel's
/// spectrum, the spectrum is divided by the curve the decoder will draw - not by the curve that was
/// fitted, which is the same thing only because `render_floor` runs the decoder's own two stages -
/// and the quotient is quantized against the residue book. That quotient sits around one by
/// construction, which is the whole reason a floor exists: coding the spectrum directly would spend
/// the same bits saying how loud each band is over and over.
pub fn encode_audio_packet(spectra: &[Vec<f32>], blocksize_log2: u8) -> Option<Vec<u8>> {
	let channels = spectra.len();
	if channels == 0 || channels > 255 {
		return None;
	}
	let n: usize = 1 << (blocksize_log2 - 1);
	if spectra.iter().any(|spectrum| spectrum.len() != n) {
		return None;
	}
	let floor_book = Codebook::flat(FLOOR_RANGE as usize, None)?;
	let class_book = Codebook::flat(2, None)?;
	let residue_book = Codebook::flat(RESIDUE_ENTRIES, Some((RESIDUE_MINIMUM, RESIDUE_DELTA, 6)))?;
	let mut out = BitWriter::new();
	// The packet type: 0 for audio. Then the mode number, in `ilog(modes - 1)` bits - which for the
	// one mode this encoder configures is ZERO bits, so nothing is written. Writing a zero here
	// instead would shift every field after it by one bit, which is a packet that decodes into
	// noise with nothing in it wrong.
	out.write(0, 1)?;

	// THE FLOORS, ALL CHANNELS FIRST, because that is the order the decoder reads them: every
	// channel's floor, then one residue vector covering all of them.
	let mut curves: Vec<Vec<f32>> = Vec::new();
	curves.try_reserve_exact(channels).ok()?;
	for spectrum in spectra {
		let magnitude: Vec<f32> = spectrum.iter().map(|value| if *value < 0.0 { -*value } else { *value }).collect();
		let coded = fit_floor(&magnitude, blocksize_log2)?;
		curves.push(render_floor(&coded, blocksize_log2)?);
		// Non-zero: this channel has a floor. A zero here says the whole channel is silent and the
		// decoder skips its residue, which is a different packet shape.
		out.write(1, 1)?;
		// The two literal posts, in `ilog(range - 1)` bits.
		let bits: u8 = ilog(FLOOR_RANGE - 1);
		out.write(coded[0], bits)?;
		out.write(coded[1], bits)?;
		// Then the rest, coded with the floor book. Two partitions of one class with no subclasses,
		// so there is no class codeword at all and the posts follow in order.
		for &value in coded.iter().skip(2) {
			let (code, length) = floor_book.codeword(value as usize)?;
			out.write_codeword(code, length)?;
		}
	}

	// THE RESIDUE, INTERLEAVED. Residue format 2 codes one vector of every channel's values taken
	// in turn - `c0[0], c1[0], c0[1], c1[1], ...` - and the decoder de-interleaves it afterwards.
	// Interleaving is not an implementation detail here: it is what makes a stereo partition cover
	// the same band in both channels, so one classification decides for both.
	let mut interleaved: Vec<f32> = Vec::new();
	interleaved.try_reserve_exact(n * channels).ok()?;
	for bin in 0..n {
		for channel in 0..channels {
			// The floor is never zero - the table's smallest entry is a value too small to hear but
			// not zero - so this division is always defined.
			interleaved.push(spectra[channel][bin] / curves[channel][bin]);
		}
	}
	let partitions: usize = interleaved.len() / RESIDUE_PARTITION as usize;
	// The class codeword for one partition. One classification means every partition is class 0,
	// and the classbook has two entries so it costs one bit - the smallest legal book.
	let (class_code, class_bits) = class_book.codeword(0)?;
	for partition in 0..partitions {
		// PASS ZERO ONLY, and the class codeword comes first because the decoder reads all the
		// classifications for a group of `classwords_per_codeword` partitions before the values.
		// Every book here has one dimension, so that group is one partition and the two interleave
		// perfectly.
		out.write_codeword(class_code, class_bits)?;
		for index in 0..RESIDUE_PARTITION as usize {
			let value = interleaved[partition * RESIDUE_PARTITION as usize + index];
			let (code, length) = residue_book.codeword(quantize_residue(value))?;
			out.write_codeword(code, length)?;
		}
	}
	Some(out.finish())
}

/// The residue book entry closest to `value`.
///
/// SATURATING RATHER THAN WRAPPING, and the saturation should never be reached: `fit_floor` puts
/// the curve at or above every magnitude, so the quotient is within -1 .. +1 by construction. It is
/// clamped anyway because the alternative to a clamp here is an index out of the book - a panic in
/// an encoder, on input that is merely unusual.
fn quantize_residue(value: f32) -> usize {
	let scaled = (value - RESIDUE_MINIMUM) / RESIDUE_DELTA;
	let rounded = libm::floorf(scaled + 0.5);
	if rounded < 0.0 {
		0
	} else if rounded >= (RESIDUE_ENTRIES - 1) as f32 {
		RESIDUE_ENTRIES - 1
	} else {
		rounded as usize
	}
}

/// The number of bits needed to hold `value` - the format's own `ilog`, which is zero for zero.
fn ilog(value: u32) -> u8 {
	(32 - value.leading_zeros()) as u8
}

#[cfg(test)]
#[path = "encode_tests.rs"]
mod tests;

/// Encode interleaved-by-channel PCM into a complete Ogg Vorbis stream.
///
/// `pcm` is one vector of samples per channel, each the same length, in -1.0 .. 1.0. The result is
/// a stream this tree's own player reads.
///
/// THE BLOCKS OVERLAP BY HALF, which is not a choice - it is what the MDCT is. Each packet
/// transforms `2n` windowed samples and the decoder recovers `n` of them by adding the second half
/// of one block's inverse to the first half of the next. So a run of `m` samples needs `m/n + 1`
/// packets, the first of which decodes to nothing (it has no predecessor to overlap with) and the
/// last of which is the tail.
///
/// THE SPECTRUM IS SCALED INTO THE FLOOR'S REACH, here, where the decision is visible. `fit_floor`
/// refuses a spectrum it cannot sit above rather than clipping it or scaling it silently, because
/// there is no gain field in the format to record a scaling with - so the scaling has to be one the
/// caller can see, and it is applied uniformly across the whole stream so it cannot be heard as
/// anything but level.
pub fn encode(pcm: &[Vec<f32>], rate: u32, blocksize_log2: u8, serial: u32) -> Option<Vec<u8>> {
	let channels = pcm.len();
	if channels == 0 || channels > 255 || rate == 0 || !(6..=13).contains(&blocksize_log2) {
		return None;
	}
	let frames = pcm[0].len();
	if pcm.iter().any(|channel| channel.len() != frames) {
		return None;
	}
	let n: usize = 1 << (blocksize_log2 - 1);
	let window = window_for(blocksize_log2);
	let mut writer = ogg::PageWriter::new(serial);
	// The identification header, ALONE ON THE FIRST PAGE, which the format requires: a reader that
	// finds anything beside it is entitled to refuse the stream.
	writer.write_packet(&write_ident(channels as u8, rate, blocksize_log2)?, None).ok()?;
	writer.flush().ok()?;
	writer.write_packet(&write_comment("LiberSystem")?, None).ok()?;
	writer.write_packet(&write_setup(channels as u8, blocksize_log2)?, None).ok()?;
	writer.flush().ok()?;

	// TWO PASSES OVER THE SIGNAL, and the second is not avoidable by being clever.
	//
	// The floor must sit above every spectral magnitude and the highest floor this configuration
	// can name is just under 1.0, so the spectrum has to be scaled into that reach - and the scale
	// has to be ONE NUMBER FOR THE WHOLE STREAM, because a scale that changed between packets would
	// be a gain nothing in the stream records, which is audible as pumping.
	//
	// So the peak has to be known before the first packet is written, and the peak of a SPECTRUM is
	// not a function of the peak of the samples: the transform's gain depends on how the signal
	// lines up with the window and the bins. A bound computed from the samples alone is correct and
	// costs about twelve decibels of headroom on every file to cover a case most files do not hit.
	// Measuring is one more pass of a transform that runs once per file.
	//
	// The spectra are NOT kept between the passes. Holding them would make the encoder's memory a
	// function of the file's length, which is the property the block structure exists to avoid.
	let mut block: Vec<f32> = Vec::new();
	block.try_reserve_exact(n * 2).ok()?;
	// `+ 2`, and both are boundaries rather than slack. The FIRST packet decodes to nothing - it is
	// the left half of the first overlap, with no predecessor to add to - and the LAST block of
	// signal needs a packet after it to complete its own overlap, or its second half never comes
	// out. A stream one packet short ends a full block early, which is a tail nobody hears going
	// missing until they compare lengths.
	let packets = frames.div_ceil(n) + 2;
	let mut spectrum_of = |index: usize, channel: &Vec<f32>, block: &mut Vec<f32>| -> Option<Vec<f32>> {
		let start: isize = index as isize * n as isize - n as isize;
		block.clear();
		for offset in 0..n * 2 {
			let at: isize = start + offset as isize;
			// Outside the signal is silence, which is what the overlap of the first and last blocks
			// is made of - and it is why the first packet decodes to nothing.
			let sample: f32 = if at < 0 || at as usize >= frames { 0.0 } else { channel[at as usize] };
			block.push(sample * window[offset]);
		}
		forward_mdct(block)
	};
	let mut peak: f32 = 0.0;
	for index in 0..packets {
		for channel in pcm {
			for value in spectrum_of(index, channel, &mut block)? {
				let magnitude = if value < 0.0 { -value } else { value };
				if magnitude > peak {
					peak = magnitude;
				}
			}
		}
	}
	// A little under the reach rather than exactly at it: `fit_floor` refuses a magnitude above the
	// highest floor it can name, and equality with a float that has been through a transform is not
	// something to rely on.
	let scale: f32 = if peak > 0.0 { floor_ceiling() * 0.98 / peak } else { 1.0 };

	let mut granule: u64 = 0;
	for index in 0..packets {
		let mut spectra: Vec<Vec<f32>> = Vec::new();
		spectra.try_reserve_exact(channels).ok()?;
		for channel in pcm {
			let mut spectrum = spectrum_of(index, channel, &mut block)?;
			for value in &mut spectrum {
				*value *= scale;
			}
			spectra.push(spectrum);
		}
		let packet = encode_audio_packet(&spectra, blocksize_log2)?;
		// The granule of a packet is how many samples the stream can produce once it has been
		// decoded. The first produces none; every one after it completes one block's overlap.
		if index > 0 {
			granule = granule.saturating_add(n as u64);
		}
		let position = core::cmp::min(granule, frames as u64);
		writer.write_packet(&packet, Some(position)).ok()?;
	}
	writer.finish().ok()
}
