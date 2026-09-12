//! `CFF` and `CFF2`: the OTHER outline format, and the one that is a PROGRAM.
//!
//! `glyf` IS DATA AND A CHARSTRING IS CODE. A `glyf` glyph is a list of points; a charstring is a
//! stack machine with subroutine calls, and drawing it means RUNNING it. That is the whole of why
//! this file is bounded the way it is: the operand stack, the call depth and the operator set are all
//! the profile's frozen numbers rather than whatever the font asks for, because "run what the file
//! says" over untrusted input with no bound is the shape of every font-parser exploit there has ever
//! been.
//!
//! THE CURVES ARE CUBIC AND `glyf`'S ARE QUADRATIC, which is why [`crate::glyf::PointKind`] is an
//! enum rather than a boolean: two quadratic controls in a row imply an on-curve point half way
//! between them, and the two controls of a cubic do not. A consumer that read one as the other draws
//! a different shape and nothing in the data says so.
//!
//! THE DEPRECATED HALF OF TYPE 2 IS REFUSED BY NUMBER. Arithmetic, storage, a random number and the
//! Type 1 callother bridge are a stack machine inside the stack machine; no font written this century
//! uses them, and implementing an interpreter for untrusted input in exchange for nothing is a trade
//! this profile does not make. The refusal names the operator, which a report and a conformance suite
//! can act on.
//!
//! CFF2 IS THE SAME MACHINE WITH THE WIDTH REMOVED AND `blend` ADDED. A width in a charstring was
//! always a duplicate of `hmtx`, and CFF2 drops it; `blend` takes a value and the deltas for every
//! region of the current variation subtable and leaves the value AT THIS INSTANCE. Reading a CFF2
//! charstring as a Type 2 one silently takes the first blend delta for the value itself.

use crate::glyf::{Outline, Point, PointKind};
use crate::reader::Reader;
use crate::tables::Face;
use crate::variations::Normalised;
use crate::{Error, Malformed, Unsupported};

/// How many operands a charstring's stack holds, and how deep its calls may go: the profile's.
const MAX_STACK: usize = opentype_profile::limits::CHARSTRING_STACK as usize;
const MAX_DEPTH: usize = opentype_profile::limits::CHARSTRING_DEPTH as usize;
/// How many regions one `blend` may mix, which is also how many scalars are held while it runs.
const MAX_BLEND_REGIONS: usize = 64;

fn bad(table: &[u8; 4]) -> Error {
	Error::Malformed(Malformed::InconsistentTable { table: *table })
}

/// A `CFF` or `CFF2` INDEX: a count, an offset array and the data those offsets are into.
#[derive(Clone, Copy)]
struct Index<'a> {
	data: Reader<'a>,
	count: usize,
	offset_size: usize,
	offsets_at: usize,
	/// One PAST the offset array, minus one - the base every offset is measured from, because the
	/// format's offsets start at 1.
	base: usize,
	/// Where this INDEX ends in its table, which is how the next one is found.
	end: usize,
}

impl<'a> Index<'a> {
	/// Read an INDEX at a position. `wide` is CFF2's 32-bit count, which CFF's 16-bit one is not.
	fn read(data: Reader<'a>, at: usize, wide: bool, table: &[u8; 4]) -> Result<Self, Error> {
		let mut reader = data;
		reader.seek(at).ok_or_else(|| bad(table))?;
		let count = if wide { reader.u32().ok_or_else(|| bad(table))? as usize } else { reader.u16().ok_or_else(|| bad(table))? as usize };
		if count == 0 {
			// AN EMPTY INDEX IS ITS COUNT AND NOTHING ELSE - not a count, an offset size and an empty
			// array - and a reader that expected the rest would walk into whatever follows it.
			return Ok(Self { data, count: 0, offset_size: 0, offsets_at: reader.position(), base: reader.position(), end: reader.position() });
		}
		let offset_size = reader.u8().ok_or_else(|| bad(table))? as usize;
		if !(1..=4).contains(&offset_size) {
			return Err(bad(table));
		}
		let offsets_at = reader.position();
		let offsets_length = (count + 1).checked_mul(offset_size).ok_or_else(|| bad(table))?;
		let base = offsets_at.checked_add(offsets_length).ok_or_else(|| bad(table))?.checked_sub(1).ok_or_else(|| bad(table))?;
		let mut index = Self { data, count, offset_size, offsets_at, base, end: 0 };
		let last = index.offset(count)?;
		index.end = base.checked_add(last).ok_or_else(|| bad(table))?;
		Ok(index)
	}

	/// One entry of the offset array.
	fn offset(&self, which: usize) -> Result<usize, Error> {
		let at = self.offsets_at.checked_add(which.checked_mul(self.offset_size).ok_or_else(|| bad(b"CFF "))?).ok_or_else(|| bad(b"CFF "))?;
		let mut reader = self.data;
		reader.seek(at).ok_or_else(|| bad(b"CFF "))?;
		Ok(match self.offset_size {
			1 => reader.u8().ok_or_else(|| bad(b"CFF "))? as usize,
			2 => reader.u16().ok_or_else(|| bad(b"CFF "))? as usize,
			3 => reader.u24().ok_or_else(|| bad(b"CFF "))? as usize,
			_ => reader.u32().ok_or_else(|| bad(b"CFF "))? as usize,
		})
	}

	/// One entry's bytes.
	fn get(&self, which: usize) -> Result<Option<Reader<'a>>, Error> {
		if which >= self.count {
			return Ok(None);
		}
		let start = self.base.checked_add(self.offset(which)?).ok_or_else(|| bad(b"CFF "))?;
		let end = self.base.checked_add(self.offset(which + 1)?).ok_or_else(|| bad(b"CFF "))?;
		if end < start {
			return Err(bad(b"CFF "));
		}
		Ok(Some(self.data.slice(start, end - start).ok_or_else(|| bad(b"CFF "))?))
	}

	fn len(&self) -> usize {
		self.count
	}
}

/// THE SUBROUTINE BIAS, which is not an optimisation but part of the encoding.
///
/// A subroutine is named by a number that may be NEGATIVE, and the bias is what turns it into an
/// index - so the commonest subroutines get the shortest numbers. Getting the bias wrong calls a
/// different subroutine, which draws a different glyph and reports nothing.
fn bias(count: usize) -> i32 {
	if count < 1240 {
		107
	} else if count < 33900 {
		1131
	} else {
		32768
	}
}

/// One DICT operator's operands, read out of a DICT by scanning it.
///
/// SCANNED RATHER THAN PARSED INTO A STRUCT. A DICT has a handful of keys out of a hundred possible
/// ones, and building a structure for all of them would be a hundred fields nothing reads; scanning
/// for the four this system needs is one pass over a few dozen bytes.
fn dict_operands(dict: Reader<'_>, wanted: u16, into: &mut [i32; 8]) -> Result<Option<usize>, Error> {
	let table = b"CFF ";
	let mut reader = dict;
	let mut operands = 0usize;
	loop {
		let Some(byte) = reader.u8() else { return Ok(None) };
		match byte {
			// THE OPERATORS RUN TO 27 AND NOT TO 21, which is the trap CFF2 sets for a reader written
			// against CFF alone: CFF's own operators stop at 21 and everything to 27 is reserved, but
			// CFF2 uses 24 for its variation store and 22 and 23 inside a private DICT. A reader that
			// stopped at 21 reads 24 as a malformed byte and refuses every CFF2 font. 12 is an escape
			// into the second hundred, in both formats.
			0..=27 => {
				let operator = if byte == 12 { 1200 + reader.u8().ok_or_else(|| bad(table))? as u16 } else { byte as u16 };
				if operator == wanted {
					return Ok(Some(operands));
				}
				operands = 0;
			}
			28 => push_operand(reader.i16().ok_or_else(|| bad(table))? as i32, into, &mut operands),
			29 => push_operand(reader.i32().ok_or_else(|| bad(table))?, into, &mut operands),
			30 => {
				// A REAL, IN BINARY-CODED DECIMAL. Nothing this system reads out of a DICT is real -
				// the font matrix is the only one and this profile does not transform by it - so it
				// is stepped over rather than decoded, which is the honest way to skip a value.
				loop {
					let pair = reader.u8().ok_or_else(|| bad(table))?;
					if pair & 0x0F == 0x0F || pair >> 4 == 0x0F {
						break;
					}
				}
				push_operand(0, into, &mut operands);
			}
			32..=246 => push_operand(byte as i32 - 139, into, &mut operands),
			247..=250 => {
				let low = reader.u8().ok_or_else(|| bad(table))? as i32;
				push_operand((byte as i32 - 247) * 256 + low + 108, into, &mut operands);
			}
			251..=254 => {
				let low = reader.u8().ok_or_else(|| bad(table))? as i32;
				push_operand(-(byte as i32 - 251) * 256 - low - 108, into, &mut operands);
			}
			_ => return Err(bad(table)),
		}
	}
}

fn push_operand(value: i32, into: &mut [i32; 8], operands: &mut usize) {
	if *operands < into.len() {
		into[*operands] = value;
	}
	// A DICT KEY WITH MORE OPERANDS THAN THIS READS IS NOT AN ERROR - `FontMatrix` has six and
	// `XUID` has up to sixteen - so the count keeps rising and the extra values are dropped. What
	// must not happen is writing past the array, which is why the store is guarded and the count is
	// not.
	*operands += 1;
}

/// A font's PRIVATE DICT: its local subroutines and the widths a charstring may leave out.
#[derive(Clone, Copy, Default)]
struct Private<'a> {
	local_subrs: Option<Index<'a>>,
}

/// A `CFF` or `CFF2` table, ready to draw from.
pub struct Cff<'a> {
	table: Reader<'a>,
	tag: [u8; 4],
	charstrings: Index<'a>,
	global_subrs: Index<'a>,
	/// One private DICT per font DICT. A plain font has one; a CID-keyed font has an array, and
	/// `FDSelect` says which glyph uses which.
	privates: [Private<'a>; MAX_FONT_DICTS],
	private_count: usize,
	fd_select: Option<usize>,
	/// CFF2's item variation store, which `blend` reads through.
	var_store: Option<Reader<'a>>,
}

/// How many font DICTs a CID-keyed font may have.
///
/// A BOUND BECAUSE IT IS INPUT. Real CID fonts have a handful; the format allows sixty-five thousand,
/// each with a private DICT and a subroutine index, and reading them all is work a font should not be
/// able to ask for.
const MAX_FONT_DICTS: usize = 64;

impl<'a> Cff<'a> {
	/// Read a face's `CFF` or `CFF2` table, or answer `None` for a face with neither.
	pub fn of(face: &Face<'a>) -> Result<Option<Self>, Error> {
		if let Some(table) = face.table(b"CFF2")? {
			return Self::read(table, *b"CFF2").map(Some);
		}
		if let Some(table) = face.table(b"CFF ")? {
			return Self::read(table, *b"CFF ").map(Some);
		}
		Ok(None)
	}

	fn read(table: Reader<'a>, tag: [u8; 4]) -> Result<Self, Error> {
		let cff2 = tag == *b"CFF2";
		let mut reader = table;
		let major = reader.u8().ok_or_else(|| bad(&tag))?;
		let _minor = reader.u8().ok_or_else(|| bad(&tag))?;
		let header_size = reader.u8().ok_or_else(|| bad(&tag))? as usize;
		if major as u16 != if cff2 { 2 } else { 1 } {
			return Err(Error::Unsupported(Unsupported::TableVersion { tag, major: major as u16, minor: 0 }));
		}

		// THE TOP DICT IS FOUND DIFFERENTLY IN THE TWO FORMATS, and that is the first place a reader
		// that treated CFF2 as CFF goes wrong: CFF2 states the top DICT's LENGTH in its header and
		// has no Name INDEX at all, while CFF has a Name INDEX and then an INDEX of top DICTs.
		let top = if cff2 {
			let length = reader.u16().ok_or_else(|| bad(&tag))? as usize;
			table.slice(header_size, length).ok_or_else(|| bad(&tag))?
		} else {
			let names = Index::read(table, header_size, false, &tag)?;
			let tops = Index::read(table, names.end, false, &tag)?;
			tops.get(0)?.ok_or_else(|| bad(&tag))?
		};

		let mut operands = [0i32; 8];
		// 17: CharStrings.
		let charstrings_at = match dict_operands(top, 17, &mut operands)? {
			Some(count) if count >= 1 => operands[0] as usize,
			_ => return Err(bad(&tag)),
		};
		let charstrings = Index::read(table, charstrings_at, cff2, &tag)?;

		// The global subroutines follow the String INDEX in CFF and the top DICT in CFF2.
		let global_subrs = if cff2 {
			Index::read(table, header_size + top.len(), true, &tag)?
		} else {
			let names = Index::read(table, header_size, false, &tag)?;
			let tops = Index::read(table, names.end, false, &tag)?;
			let strings = Index::read(table, tops.end, false, &tag)?;
			Index::read(table, strings.end, false, &tag)?
		};

		// 1236: FDArray, and 1237: FDSelect. A CID-keyed font states both.
		let mut privates = [Private::default(); MAX_FONT_DICTS];
		let private_count;
		let fd_array = match dict_operands(top, 1236, &mut operands)? {
			Some(count) if count >= 1 => Some(operands[0] as usize),
			_ => None,
		};
		let fd_select = match dict_operands(top, 1237, &mut operands)? {
			Some(count) if count >= 1 => Some(operands[0] as usize),
			_ => None,
		};
		match fd_array {
			Some(at) => {
				let fonts = Index::read(table, at, cff2, &tag)?;
				if fonts.len() > MAX_FONT_DICTS {
					return Err(Error::Unsupported(Unsupported::Exceeded { limit: "features", ceiling: MAX_FONT_DICTS as u32, asked: fonts.len() as u64 }));
				}
				for (index, private) in privates.iter_mut().enumerate().take(fonts.len()) {
					let font = fonts.get(index)?.ok_or_else(|| bad(&tag))?;
					*private = read_private(table, font, &tag)?;
				}
				private_count = fonts.len();
			}
			None => {
				privates[0] = read_private(table, top, &tag)?;
				private_count = 1;
			}
		}

		// 24: vstore, CFF2's item variation store. Its first field is a length the store follows.
		let var_store = match dict_operands(top, 24, &mut operands)? {
			Some(count) if count >= 1 && cff2 => {
				let at = operands[0] as usize;
				let mut reader = table;
				reader.seek(at).ok_or_else(|| bad(&tag))?;
				let length = reader.u16().ok_or_else(|| bad(&tag))? as usize;
				Some(table.slice(reader.position(), length).ok_or_else(|| bad(&tag))?)
			}
			_ => None,
		};

		Ok(Self { table, tag, charstrings, global_subrs, privates, private_count, fd_select, var_store })
	}

	/// How many glyphs the charstring INDEX holds.
	pub fn glyph_count(&self) -> usize {
		self.charstrings.len()
	}

	/// Walk one glyph's outline at a variation coordinate.
	///
	/// `coordinates` are ignored by `CFF`, which does not vary, and are what `blend` reads in `CFF2`.
	pub fn walk(&self, glyph: u16, coordinates: &[Normalised], outline: &mut impl Outline) -> Result<(), Error> {
		let Some(charstring) = self.charstrings.get(glyph as usize)? else {
			return Err(Error::Malformed(Malformed::BadGlyph { glyph }));
		};
		let private = self.private_for(glyph)?;
		let mut machine = Machine { cff: self, coordinates, stack: [0; MAX_STACK], depth: 0, x: 0, y: 0, stems: 0, width_parsed: self.tag == *b"CFF2", pending: None, var_index: 0, local: private.local_subrs };
		machine.run(charstring, 0, outline)?;
		machine.close(outline);
		Ok(())
	}

	/// Which private DICT a glyph draws with.
	fn private_for(&self, glyph: u16) -> Result<Private<'a>, Error> {
		let Some(at) = self.fd_select else {
			return Ok(self.privates[0]);
		};
		let mut reader = self.table;
		reader.seek(at).ok_or_else(|| bad(&self.tag))?;
		let format = reader.u8().ok_or_else(|| bad(&self.tag))?;
		let which = match format {
			0 => {
				let mut entry = self.table;
				entry.seek(at.checked_add(1).ok_or_else(|| bad(&self.tag))?.checked_add(glyph as usize).ok_or_else(|| bad(&self.tag))?).ok_or_else(|| bad(&self.tag))?;
				entry.u8().ok_or_else(|| bad(&self.tag))? as usize
			}
			3 => {
				// FORMAT 3 IS A RANGE LIST, and the ranges are sorted - so the answer is the LAST
				// range whose first glyph is at or below this one. Taking the first that contains it
				// would need an end, and a range's end is the next range's start.
				let ranges = reader.u16().ok_or_else(|| bad(&self.tag))? as usize;
				let base = reader.position();
				let mut chosen = 0usize;
				for index in 0..ranges {
					let mut entry = self.table;
					entry.seek(base.checked_add(index.checked_mul(3).ok_or_else(|| bad(&self.tag))?).ok_or_else(|| bad(&self.tag))?).ok_or_else(|| bad(&self.tag))?;
					let first = entry.u16().ok_or_else(|| bad(&self.tag))?;
					let which = entry.u8().ok_or_else(|| bad(&self.tag))? as usize;
					if first > glyph {
						break;
					}
					chosen = which;
				}
				chosen
			}
			_ => return Err(Error::Unsupported(Unsupported::SubtableFormat { table: self.tag, format: format as u16 })),
		};
		if which >= self.private_count {
			return Err(bad(&self.tag));
		}
		Ok(self.privates[which])
	}
}

/// A font DICT's private DICT, and the local subroutines inside it.
fn read_private<'a>(table: Reader<'a>, dict: Reader<'a>, tag: &[u8; 4]) -> Result<Private<'a>, Error> {
	let mut operands = [0i32; 8];
	// 18: Private, whose two operands are a SIZE and then an OFFSET - in that order, which is the
	// one DICT key whose operands are not what a reader expects.
	let Some(count) = dict_operands(dict, 18, &mut operands)? else { return Ok(Private::default()) };
	if count < 2 {
		return Ok(Private::default());
	}
	let (size, at) = (operands[0] as usize, operands[1] as usize);
	let private = table.slice(at, size).ok_or_else(|| bad(tag))?;
	// 19: Subrs, whose offset is from the start of the PRIVATE DICT rather than of the table - the
	// one offset in the format that is relative, and reading it as absolute lands anywhere.
	let local_subrs = match dict_operands(private, 19, &mut operands)? {
		Some(count) if count >= 1 => {
			let subrs_at = at.checked_add(operands[0] as usize).ok_or_else(|| bad(tag))?;
			Some(Index::read(table, subrs_at, tag == b"CFF2", tag)?)
		}
		_ => None,
	};
	Ok(Private { local_subrs })
}

/// The charstring machine: the stack, the pen, and the bounds on both.
struct Machine<'a, 'c> {
	cff: &'c Cff<'a>,
	coordinates: &'c [Normalised],
	stack: [i32; MAX_STACK],
	depth: usize,
	x: i32,
	y: i32,
	stems: usize,
	width_parsed: bool,
	/// The last point emitted, held back so that the one that CLOSES a contour can be marked.
	pending: Option<Point>,
	var_index: u16,
	local: Option<Index<'a>>,
}

impl Machine<'_, '_> {
	fn tag(&self) -> [u8; 4] {
		self.cff.tag
	}

	fn run(&mut self, charstring: Reader<'_>, count: usize, outline: &mut impl Outline) -> Result<usize, Error> {
		let mut count = count;
		let mut reader = charstring;
		loop {
			let Some(byte) = reader.u8() else { return Ok(count) };
			match byte {
				// The operands, in the charstring's own encoding - which is NOT the DICT's.
				28 => self.push(reader.i16().ok_or_else(|| bad(&self.tag()))? as i32, &mut count)?,
				32..=246 => self.push(byte as i32 - 139, &mut count)?,
				247..=250 => {
					let low = reader.u8().ok_or_else(|| bad(&self.tag()))? as i32;
					self.push((byte as i32 - 247) * 256 + low + 108, &mut count)?;
				}
				251..=254 => {
					let low = reader.u8().ok_or_else(|| bad(&self.tag()))? as i32;
					self.push(-(byte as i32 - 251) * 256 - low - 108, &mut count)?;
				}
				// 255 IS A FIXED-POINT NUMBER AND NOT AN INTEGER, which is the encoding difference
				// between a DICT and a charstring: the same byte means a 32-bit integer in one and
				// 16.16 in the other, and a reader using the DICT rule here scales every value by
				// sixty-five thousand.
				255 => {
					let value = reader.i32().ok_or_else(|| bad(&self.tag()))?;
					self.push(value >> 16, &mut count)?;
				}
				_ => {
					let operator = if byte == 12 { 1200 + reader.u8().ok_or_else(|| bad(&self.tag()))? as u16 } else { byte as u16 };
					if self.operator(operator, &mut reader, &mut count, outline)? {
						return Ok(count);
					}
				}
			}
		}
	}

	fn push(&mut self, value: i32, count: &mut usize) -> Result<(), Error> {
		if *count >= MAX_STACK {
			// THE STACK IS THE PROFILE'S SIZE AND NOT THE FONT'S. A charstring that pushes without
			// popping is the cheapest way to ask a parser for unbounded memory.
			return Err(Error::Unsupported(Unsupported::Exceeded { limit: "charstring stack", ceiling: MAX_STACK as u32, asked: *count as u64 + 1 }));
		}
		self.stack[*count] = value;
		*count += 1;
		Ok(())
	}

	/// One operator. Answers whether the charstring ENDED.
	fn operator(&mut self, operator: u16, reader: &mut Reader<'_>, count: &mut usize, outline: &mut impl Outline) -> Result<bool, Error> {
		let tag = self.tag();
		match operator {
			// The stem hints. This profile does not hint, so what they are read for is their COUNT,
			// which is what `hintmask` is sized by - a reader that skipped them would then skip the
			// wrong number of mask bytes and interpret a mask as operators.
			1 | 3 | 18 | 23 => {
				self.take_width(count, *count % 2 == 1);
				self.stems += *count / 2;
				*count = 0;
			}
			19 | 20 => {
				// AN IMPLICIT `vstem`: operands still on the stack before a mask are stem hints the
				// font did not bother to name, and not counting them mis-sizes every later mask.
				self.take_width(count, *count % 2 == 1);
				self.stems += *count / 2;
				*count = 0;
				let bytes = self.stems.div_ceil(8);
				reader.skip(bytes).ok_or_else(|| bad(&tag))?;
			}
			// The moves, each of which begins a new contour.
			21 => {
				self.take_width(count, *count > 2);
				let (dx, dy) = (self.arg(*count, 2, 0), self.arg(*count, 2, 1));
				self.move_to(dx, dy, outline);
				*count = 0;
			}
			22 => {
				self.take_width(count, *count > 1);
				let dx = self.arg(*count, 1, 0);
				self.move_to(dx, 0, outline);
				*count = 0;
			}
			4 => {
				self.take_width(count, *count > 1);
				let dy = self.arg(*count, 1, 0);
				self.move_to(0, dy, outline);
				*count = 0;
			}
			// The lines.
			5 => {
				let mut index = 0;
				while index + 1 < *count + 1 && index + 2 <= *count {
					let (dx, dy) = (self.stack[index], self.stack[index + 1]);
					self.line_to(dx, dy, outline);
					index += 2;
				}
				*count = 0;
			}
			6 | 7 => {
				// ALTERNATING, starting horizontal for 6 and vertical for 7. A reader that took them
				// as all-horizontal draws a staircase as a straight line.
				let mut horizontal = operator == 6;
				for index in 0..*count {
					let value = self.stack[index];
					if horizontal {
						self.line_to(value, 0, outline);
					} else {
						self.line_to(0, value, outline);
					}
					horizontal = !horizontal;
				}
				*count = 0;
			}
			// The curves.
			8 => {
				let mut index = 0;
				while index + 6 <= *count {
					self.curve_to(self.stack[index], self.stack[index + 1], self.stack[index + 2], self.stack[index + 3], self.stack[index + 4], self.stack[index + 5], outline);
					index += 6;
				}
				*count = 0;
			}
			24 => {
				// `rcurveline`: curves, then ONE line with what is left.
				let mut index = 0;
				while *count >= index + 8 {
					self.curve_to(self.stack[index], self.stack[index + 1], self.stack[index + 2], self.stack[index + 3], self.stack[index + 4], self.stack[index + 5], outline);
					index += 6;
				}
				if index + 2 <= *count {
					self.line_to(self.stack[index], self.stack[index + 1], outline);
				}
				*count = 0;
			}
			25 => {
				// `rlinecurve`: lines, then ONE curve with the last six.
				let mut index = 0;
				while *count >= index + 8 {
					self.line_to(self.stack[index], self.stack[index + 1], outline);
					index += 2;
				}
				if index + 6 <= *count {
					self.curve_to(self.stack[index], self.stack[index + 1], self.stack[index + 2], self.stack[index + 3], self.stack[index + 4], self.stack[index + 5], outline);
				}
				*count = 0;
			}
			26 | 27 => {
				// `vvcurveto` and `hhcurveto`: an ODD first operand is a one-off displacement on the
				// other axis, and only for the FIRST curve. Applying it to every curve bends the
				// whole stroke.
				let mut index = 0;
				let mut odd = 0;
				if *count % 4 == 1 {
					odd = self.stack[0];
					index = 1;
				}
				while index + 4 <= *count {
					let (a, b, c, d) = (self.stack[index], self.stack[index + 1], self.stack[index + 2], self.stack[index + 3]);
					if operator == 26 {
						self.curve_to(odd, a, b, c, 0, d, outline);
					} else {
						self.curve_to(a, odd, b, c, d, 0, outline);
					}
					odd = 0;
					index += 4;
				}
				*count = 0;
			}
			30 | 31 => {
				// `vhcurveto` and `hvcurveto`: alternating, with a fifth operand on the LAST curve
				// giving the axis the alternation would otherwise leave at zero.
				let mut horizontal = operator == 31;
				let mut index = 0;
				while index + 4 <= *count {
					let last = index + 8 > *count;
					let extra = if last && index + 5 == *count { self.stack[index + 4] } else { 0 };
					let (a, b, c, d) = (self.stack[index], self.stack[index + 1], self.stack[index + 2], self.stack[index + 3]);
					if horizontal {
						self.curve_to(a, 0, b, c, extra, d, outline);
					} else {
						self.curve_to(0, a, b, c, d, extra, outline);
					}
					horizontal = !horizontal;
					index += 4;
				}
				*count = 0;
			}
			// The flex operators: two curves the font says are flat enough to be one, which this
			// profile draws as the two curves they are. The flex DEPTH is a hinting hint and is not
			// applied, for the same reason no other hint is.
			1235 => {
				if *count >= 13 {
					self.curve_to(self.stack[0], self.stack[1], self.stack[2], self.stack[3], self.stack[4], self.stack[5], outline);
					self.curve_to(self.stack[6], self.stack[7], self.stack[8], self.stack[9], self.stack[10], self.stack[11], outline);
				}
				*count = 0;
			}
			1234 => {
				if *count >= 7 {
					let (dx1, dx2, dy2, dx3, dx4, dx5, dx6) = (self.stack[0], self.stack[1], self.stack[2], self.stack[3], self.stack[4], self.stack[5], self.stack[6]);
					self.curve_to(dx1, 0, dx2, dy2, dx3, 0, outline);
					self.curve_to(dx4, 0, dx5, -dy2, dx6, 0, outline);
				}
				*count = 0;
			}
			1236 => {
				if *count >= 9 {
					let (dx1, dy1, dx2, dy2, dx3, dx4, dx5, dy5, dx6) = (self.stack[0], self.stack[1], self.stack[2], self.stack[3], self.stack[4], self.stack[5], self.stack[6], self.stack[7], self.stack[8]);
					self.curve_to(dx1, dy1, dx2, dy2, dx3, 0, outline);
					self.curve_to(dx4, 0, dx5, dy5, dx6, -(dy1 + dy2 + dy5), outline);
				}
				*count = 0;
			}
			1237 => {
				if *count >= 11 {
					let values: [i32; 11] = core::array::from_fn(|index| self.stack[index]);
					let dx = values[0] + values[2] + values[4] + values[6] + values[8];
					let dy = values[1] + values[3] + values[5] + values[7] + values[9];
					self.curve_to(values[0], values[1], values[2], values[3], values[4], values[5], outline);
					// THE LAST POINT RETURNS TO THE STARTING Y (or X), which is what makes it a flex:
					// the sixth delta is whichever of the two the font did not state.
					if dx.abs() > dy.abs() {
						self.curve_to(values[6], values[7], values[8], values[9], values[10], -dy, outline);
					} else {
						self.curve_to(values[6], values[7], values[8], values[9], -dx, values[10], outline);
					}
				}
				*count = 0;
			}
			// The calls.
			10 | 29 => {
				if *count == 0 {
					return Err(bad(&tag));
				}
				*count -= 1;
				let number = self.stack[*count];
				let subrs = if operator == 10 { self.local } else { Some(self.cff.global_subrs) };
				let Some(subrs) = subrs else { return Err(bad(&tag)) };
				let index = number.checked_add(bias(subrs.len())).ok_or_else(|| bad(&tag))?;
				if index < 0 {
					return Err(bad(&tag));
				}
				let Some(body) = subrs.get(index as usize)? else { return Err(bad(&tag)) };
				if self.depth >= MAX_DEPTH {
					// A SUBROUTINE THAT CALLS ITSELF IS A CHARSTRING THAT NEVER RETURNS, and the
					// ceiling is the profile's rather than this file's.
					return Err(Error::Unsupported(Unsupported::Exceeded { limit: "charstring depth", ceiling: MAX_DEPTH as u32, asked: self.depth as u64 + 1 }));
				}
				self.depth += 1;
				*count = self.run(body, *count, outline)?;
				self.depth -= 1;
			}
			11 => return Ok(true),
			14 => {
				// `endchar`. Its four-operand form is `seac`, an accented character built out of two
				// others by their STANDARD ENCODING codes - a Type 1 mechanism that needs the
				// standard encoding table and draws a composite. It is refused by name rather than
				// drawn as a bare letter without its accent.
				self.take_width(count, *count == 1 || *count == 5);
				match *count {
					0 => {}
					// FOUR OPERANDS IS `seac`, and nothing else is: an accented character built out of
					// two others by their standard encoding codes. It is a Type 1 mechanism that needs
					// that encoding table and draws a composite, so it is refused by name rather than
					// drawn as a bare letter without its accent.
					4 => return Err(Error::Unsupported(Unsupported::CharstringOperator(operator))),
					// Anything else is a charstring that contradicts the operator it ends with.
					_ => return Err(bad(&tag)),
				}
				*count = 0;
				return Ok(true);
			}
			// CFF2's two additions.
			15 => {
				if self.cff.tag != *b"CFF2" {
					return Err(Error::Unsupported(Unsupported::CharstringOperator(operator)));
				}
				if *count >= 1 {
					self.var_index = self.stack[*count - 1].clamp(0, u16::MAX as i32) as u16;
				}
				*count = 0;
			}
			16 => {
				if self.cff.tag != *b"CFF2" {
					return Err(Error::Unsupported(Unsupported::CharstringOperator(operator)));
				}
				*count = self.blend(*count)?;
			}
			// EVERYTHING ELSE IS THE DEPRECATED HALF and is refused by number.
			_ => return Err(Error::Unsupported(Unsupported::CharstringOperator(operator))),
		}
		Ok(false)
	}

	/// CFF2's `blend`: `n` values followed by `n * regions` deltas, leaving the `n` values at THIS
	/// instance.
	///
	/// WITHOUT IT A CFF2 CHARSTRING IS READ AS A TYPE 2 ONE and the deltas are taken as coordinates,
	/// which draws a glyph out of the difference between two masters rather than the glyph itself.
	fn blend(&mut self, count: usize) -> Result<usize, Error> {
		let tag = self.tag();
		if count == 0 {
			return Err(bad(&tag));
		}
		let values = self.stack[count - 1].max(0) as usize;
		let Some(store) = self.cff.var_store else { return Err(bad(&tag)) };
		let mut scalars = [0i32; MAX_BLEND_REGIONS];
		let regions = crate::variations::blend_regions(store, self.var_index, self.coordinates, &mut scalars)?;
		let total = values.checked_mul(regions.checked_add(1).ok_or_else(|| bad(&tag))?).ok_or_else(|| bad(&tag))?;
		if total + 1 > count {
			return Err(bad(&tag));
		}
		let base = count - 1 - total;
		for value in 0..values {
			let mut sum = self.stack[base + value] as i64;
			for (region, scalar) in scalars.iter().enumerate().take(regions) {
				let delta = self.stack[base + values + value * regions + region] as i64;
				sum += (delta * *scalar as i64) / 16384;
			}
			self.stack[base + value] = sum.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
		}
		Ok(base + values)
	}

	/// THE WIDTH, WHICH IS THE FIRST OPERAND OF THE FIRST STACK-CLEARING OPERATOR AND ONLY THEN.
	///
	/// It is a duplicate of `hmtx` and this profile takes its advances from there, so it is dropped -
	/// but it must be RECOGNISED, because leaving it on the stack shifts every operand of that
	/// operator by one and draws the glyph somewhere else.
	fn take_width(&mut self, count: &mut usize, present: bool) {
		if self.width_parsed {
			return;
		}
		self.width_parsed = true;
		if present && *count > 0 {
			self.stack.copy_within(1..*count, 0);
			*count -= 1;
		}
	}

	/// One operand of an operator that takes a fixed number, counted from the END of the stack.
	///
	/// FROM THE END, because a stack-clearing operator may have a width in front of its operands that
	/// a preceding hint operator already removed - and because an operand list longer than the
	/// operator takes is the font's business rather than a fault.
	fn arg(&self, count: usize, wanted: usize, which: usize) -> i32 {
		match count.checked_sub(wanted) {
			Some(base) => self.stack.get(base + which).copied().unwrap_or(0),
			None => 0,
		}
	}

	fn move_to(&mut self, dx: i32, dy: i32, outline: &mut impl Outline) {
		self.close(outline);
		self.x = self.x.saturating_add(dx);
		self.y = self.y.saturating_add(dy);
		self.emit(PointKind::OnCurve, outline);
	}

	fn line_to(&mut self, dx: i32, dy: i32, outline: &mut impl Outline) {
		self.x = self.x.saturating_add(dx);
		self.y = self.y.saturating_add(dy);
		self.emit(PointKind::OnCurve, outline);
	}

	#[allow(clippy::too_many_arguments)]
	fn curve_to(&mut self, dx1: i32, dy1: i32, dx2: i32, dy2: i32, dx3: i32, dy3: i32, outline: &mut impl Outline) {
		self.x = self.x.saturating_add(dx1);
		self.y = self.y.saturating_add(dy1);
		self.emit(PointKind::Cubic, outline);
		self.x = self.x.saturating_add(dx2);
		self.y = self.y.saturating_add(dy2);
		self.emit(PointKind::Cubic, outline);
		self.x = self.x.saturating_add(dx3);
		self.y = self.y.saturating_add(dy3);
		self.emit(PointKind::OnCurve, outline);
	}

	/// Hold a point back by one, so that the one which CLOSES a contour can be marked as doing so.
	///
	/// A CHARSTRING DOES NOT SAY WHERE A CONTOUR ENDS - the next `moveto` or the end of the glyph
	/// says it, after the fact - so the only way to mark the last point of a contour is to be one
	/// point behind.
	fn emit(&mut self, kind: PointKind, outline: &mut impl Outline) {
		let point = Point { x: self.x.clamp(i16::MIN as i32, i16::MAX as i32) as i16, y: self.y.clamp(i16::MIN as i32, i16::MAX as i32) as i16, kind, ends_contour: false };
		if let Some(previous) = self.pending.replace(point) {
			outline.point(previous);
		}
	}

	/// Close the contour in progress, marking its last point.
	fn close(&mut self, outline: &mut impl Outline) {
		if let Some(mut last) = self.pending.take() {
			last.ends_contour = true;
			outline.point(last);
		}
	}
}
