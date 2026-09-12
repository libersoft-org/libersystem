//! `GPOS`: where the font puts what it substituted.
//!
//! POSITIONING IS WHERE A SHAPER STOPS LOOKING FINISHED AND STARTS BEING CORRECT. Kerning is the
//! visible half and the marks are the half that decides whether a script renders at all: an accent
//! placed at the pen rather than attached to its base is Arabic, Hebrew and Devanagari rendered as a
//! row of disconnected pieces.
//!
//! THE VALUE RECORD IS A BITFIELD OF OPTIONAL FIELDS, which is the one shape in this table that
//! cannot be read without reading its format first - and reading it wrong shifts every field after
//! it, which looks like a font with wild kerning rather than like a parser bug.

use font_parse::{Error, Malformed, Reader};
use opentype_profile::Unsupported;

use crate::buffer::Buffer;
use crate::layout::{class_of, coverage_index};

fn bad() -> Error {
	Error::Malformed(Malformed::InconsistentTable { table: *b"GPOS" })
}

/// How many bytes a value record takes, from its format bits.
fn value_size(format: u16) -> usize {
	(format & 0x00FF).count_ones() as usize * 2
}

/// One value record, applied to a position.
///
/// THE DEVICE AND VARIATION FIELDS ARE READ AND NOT APPLIED, deliberately: they adjust a value per
/// pixel size or per variation coordinate, and both need a size and an instance this layer is not
/// given. They are SKIPPED by their size rather than ignored, because skipping them by the wrong
/// number shifts everything after them.
fn apply_value(format: u16, reader: &mut Reader<'_>, position: &mut crate::buffer::Position) -> Result<(), Error> {
	if format & 0x0001 != 0 {
		position.x_offset += reader.i16().ok_or_else(bad)? as i32;
	}
	if format & 0x0002 != 0 {
		position.y_offset += reader.i16().ok_or_else(bad)? as i32;
	}
	if format & 0x0004 != 0 {
		position.x_advance += reader.i16().ok_or_else(bad)? as i32;
	}
	if format & 0x0008 != 0 {
		position.y_advance += reader.i16().ok_or_else(bad)? as i32;
	}
	for bit in [0x0010u16, 0x0020, 0x0040, 0x0080] {
		if format & bit != 0 {
			reader.skip(2).ok_or_else(bad)?;
		}
	}
	Ok(())
}

/// Apply one positioning subtable at one position. The contextual types are in `context`, for the
/// same reason they are there for substitution: they apply other lookups rather than positioning
/// anything themselves.
pub fn apply_subtable(kind: u16, subtable: Reader<'_>, buffer: &mut Buffer, at: usize) -> Result<bool, Error> {
	match kind {
		1 => single(subtable, buffer, at),
		2 => pair(subtable, buffer, at),
		4 | 6 => mark(kind, subtable, buffer, at),
		3 => cursive(subtable, buffer, at),
		5 => mark_to_ligature(subtable, buffer, at),
		other => Err(Error::Unsupported(Unsupported::GposLookup(other))),
	}
}

/// Type 1: a single adjustment.
fn single(subtable: Reader<'_>, buffer: &mut Buffer, at: usize) -> Result<bool, Error> {
	let mut reader = subtable;
	let format = reader.u16().ok_or_else(bad)?;
	let coverage_offset = reader.u16().ok_or_else(bad)? as usize;
	let value_format = reader.u16().ok_or_else(bad)?;
	let coverage = subtable.slice(coverage_offset, subtable.len().checked_sub(coverage_offset).ok_or_else(bad)?).ok_or_else(bad)?;
	let Some(index) = coverage_index(coverage, buffer.infos[at].glyph)? else { return Ok(false) };
	match format {
		1 => {
			// One value for every covered glyph.
			let mut values = subtable;
			values.seek(6).ok_or_else(bad)?;
			apply_value(value_format, &mut values, &mut buffer.positions[at])?;
			Ok(true)
		}
		2 => {
			let count = reader.u16().ok_or_else(bad)? as usize;
			if index as usize >= count {
				return Err(bad());
			}
			let size = value_size(value_format);
			let offset = 8usize.checked_add((index as usize).checked_mul(size).ok_or_else(bad)?).ok_or_else(bad)?;
			let mut values = subtable;
			values.seek(offset).ok_or_else(bad)?;
			apply_value(value_format, &mut values, &mut buffer.positions[at])?;
			Ok(true)
		}
		other => Err(Error::Unsupported(Unsupported::SubtableFormat { table: *b"GPOS", format: other })),
	}
}

/// Type 2: a pair adjustment - kerning, in both the glyph-pair and the class-pair forms.
fn pair(subtable: Reader<'_>, buffer: &mut Buffer, at: usize) -> Result<bool, Error> {
	if at + 1 >= buffer.len() {
		return Ok(false);
	}
	let mut reader = subtable;
	let format = reader.u16().ok_or_else(bad)?;
	let coverage_offset = reader.u16().ok_or_else(bad)? as usize;
	let first_format = reader.u16().ok_or_else(bad)?;
	let second_format = reader.u16().ok_or_else(bad)?;
	let coverage = subtable.slice(coverage_offset, subtable.len().checked_sub(coverage_offset).ok_or_else(bad)?).ok_or_else(bad)?;
	let first = buffer.infos[at].glyph;
	let second = buffer.infos[at + 1].glyph;
	let Some(index) = coverage_index(coverage, first)? else { return Ok(false) };
	match format {
		1 => {
			let count = reader.u16().ok_or_else(bad)? as usize;
			if index as usize >= count {
				return Err(bad());
			}
			let sets = subtable.slice(10, count.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
			let offset = sets.u16_at(index as usize).ok_or_else(bad)? as usize;
			let set = subtable.slice(offset, subtable.len().checked_sub(offset).ok_or_else(bad)?).ok_or_else(bad)?;
			let mut reader = set;
			let pairs = reader.u16().ok_or_else(bad)? as usize;
			let record = 2 + value_size(first_format) + value_size(second_format);
			for index in 0..pairs {
				let at_record = 2usize.checked_add(index.checked_mul(record).ok_or_else(bad)?).ok_or_else(bad)?;
				let mut entry = set;
				entry.seek(at_record).ok_or_else(bad)?;
				let candidate = entry.u16().ok_or_else(bad)?;
				if candidate != second {
					continue;
				}
				apply_value(first_format, &mut entry, &mut buffer.positions[at])?;
				apply_value(second_format, &mut entry, &mut buffer.positions[at + 1])?;
				return Ok(true);
			}
			Ok(false)
		}
		2 => {
			// The class form: a matrix of classes, which is how a font kerns a whole alphabet
			// without listing every pair.
			let first_classes_at = reader.u16().ok_or_else(bad)? as usize;
			let second_classes_at = reader.u16().ok_or_else(bad)? as usize;
			let first_count = reader.u16().ok_or_else(bad)? as usize;
			let second_count = reader.u16().ok_or_else(bad)? as usize;
			let first_classes = subtable.slice(first_classes_at, subtable.len().checked_sub(first_classes_at).ok_or_else(bad)?).ok_or_else(bad)?;
			let second_classes = subtable.slice(second_classes_at, subtable.len().checked_sub(second_classes_at).ok_or_else(bad)?).ok_or_else(bad)?;
			let first_class = class_of(first_classes, first)? as usize;
			let second_class = class_of(second_classes, second)? as usize;
			if first_class >= first_count || second_class >= second_count {
				return Ok(false);
			}
			let record = value_size(first_format) + value_size(second_format);
			let row = first_class.checked_mul(second_count).ok_or_else(bad)?;
			let cell = row.checked_add(second_class).ok_or_else(bad)?;
			let offset = 16usize.checked_add(cell.checked_mul(record).ok_or_else(bad)?).ok_or_else(bad)?;
			let mut entry = subtable;
			entry.seek(offset).ok_or_else(bad)?;
			apply_value(first_format, &mut entry, &mut buffer.positions[at])?;
			apply_value(second_format, &mut entry, &mut buffer.positions[at + 1])?;
			Ok(true)
		}
		other => Err(Error::Unsupported(Unsupported::SubtableFormat { table: *b"GPOS", format: other })),
	}
}

/// Types 4 and 6: a mark attached to a base, and a mark attached to another mark.
///
/// THIS IS THE RULE THAT MAKES A SCRIPT RENDER AT ALL. An accent placed at the pen instead of at its
/// anchor is not a small error: Arabic, Hebrew and Devanagari become a row of disconnected pieces,
/// and a reader who does not know the script cannot tell that anything is wrong.
fn mark(kind: u16, subtable: Reader<'_>, buffer: &mut Buffer, at: usize) -> Result<bool, Error> {
	// A mark attaches to what PRECEDES it, so there is nothing to do at the start of a run.
	if at == 0 {
		return Ok(false);
	}
	let mut reader = subtable;
	let format = reader.u16().ok_or_else(bad)?;
	if format != 1 {
		return Err(Error::Unsupported(Unsupported::SubtableFormat { table: *b"GPOS", format }));
	}
	let mark_coverage_at = reader.u16().ok_or_else(bad)? as usize;
	let base_coverage_at = reader.u16().ok_or_else(bad)? as usize;
	let class_count = reader.u16().ok_or_else(bad)? as usize;
	let mark_array_at = reader.u16().ok_or_else(bad)? as usize;
	let base_array_at = reader.u16().ok_or_else(bad)? as usize;

	let mark_coverage = subtable.slice(mark_coverage_at, subtable.len().checked_sub(mark_coverage_at).ok_or_else(bad)?).ok_or_else(bad)?;
	let Some(mark_index) = coverage_index(mark_coverage, buffer.infos[at].glyph)? else { return Ok(false) };

	// The base is the nearest preceding glyph of the right kind: for type 4 a base, for type 6
	// another mark. Walking back rather than taking `at - 1` is what makes two stacked accents
	// attach to each other and to the letter rather than all to the letter.
	let mut base_at = None;
	for candidate in (0..at).rev() {
		let class = buffer.infos[candidate].class;
		let wanted = if kind == 4 { class != 3 } else { class == 3 };
		if wanted {
			base_at = Some(candidate);
			break;
		}
	}
	let Some(base_at) = base_at else { return Ok(false) };
	let base_coverage = subtable.slice(base_coverage_at, subtable.len().checked_sub(base_coverage_at).ok_or_else(bad)?).ok_or_else(bad)?;
	let Some(base_index) = coverage_index(base_coverage, buffer.infos[base_at].glyph)? else { return Ok(false) };

	// The mark's own class and anchor.
	let mark_array = subtable.slice(mark_array_at, subtable.len().checked_sub(mark_array_at).ok_or_else(bad)?).ok_or_else(bad)?;
	let mut reader = mark_array;
	let mark_count = reader.u16().ok_or_else(bad)? as usize;
	if mark_index as usize >= mark_count {
		return Err(bad());
	}
	let record_at = 2usize.checked_add((mark_index as usize).checked_mul(4).ok_or_else(bad)?).ok_or_else(bad)?;
	let mut record = mark_array;
	record.seek(record_at).ok_or_else(bad)?;
	let mark_class = record.u16().ok_or_else(bad)? as usize;
	let mark_anchor_at = record.u16().ok_or_else(bad)? as usize;
	if mark_class >= class_count {
		return Err(bad());
	}
	let (mark_x, mark_y) = anchor(mark_array, mark_anchor_at)?;

	// The base's anchor for that class.
	let base_array = subtable.slice(base_array_at, subtable.len().checked_sub(base_array_at).ok_or_else(bad)?).ok_or_else(bad)?;
	let mut reader = base_array;
	let base_count = reader.u16().ok_or_else(bad)? as usize;
	if base_index as usize >= base_count {
		return Err(bad());
	}
	let row = (base_index as usize).checked_mul(class_count).ok_or_else(bad)?;
	let cell = row.checked_add(mark_class).ok_or_else(bad)?;
	let anchor_at = 2usize.checked_add(cell.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
	let offset = base_array.u16_at(anchor_at / 2).ok_or_else(bad)? as usize;
	// An anchor offset of zero means the base has NO anchor for this mark class, which is an
	// ordinary answer: the mark simply does not attach here.
	if offset == 0 {
		return Ok(false);
	}
	let (base_x, base_y) = anchor(base_array, offset)?;

	// The mark is placed so its anchor meets the base's, and it takes no advance of its own - which
	// is what makes it sit ON the letter rather than after it.
	// EVERYTHING FROM THE BASE UP TO THE MARK, the base INCLUDED: the pen has already advanced past
	// the base by the time the mark is drawn, so the mark is moved back by that much to sit on it.
	// Starting after the base leaves the mark one glyph's width to the right, which on a letter with
	// a wide base is an accent floating over the NEXT letter.
	let advance_between: i32 = buffer.positions[base_at..at].iter().map(|position| position.x_advance).sum();
	buffer.positions[at].x_offset = base_x - mark_x - advance_between;
	buffer.positions[at].y_offset = base_y - mark_y;
	buffer.infos[at].attached_to = u16::try_from(base_at).ok();
	Ok(true)
}

/// One anchor: a point in font units. Formats 2 and 3 carry a contour point or a device table on top
/// of the same two coordinates, and both are read AS the two coordinates - which is what they are.
fn anchor(table: Reader<'_>, offset: usize) -> Result<(i32, i32), Error> {
	let anchor = table.slice(offset, table.len().checked_sub(offset).ok_or_else(bad)?).ok_or_else(bad)?;
	let mut reader = anchor;
	let format = reader.u16().ok_or_else(bad)?;
	if !matches!(format, 1 | 2 | 3) {
		return Err(Error::Unsupported(Unsupported::SubtableFormat { table: *b"GPOS", format }));
	}
	let x = reader.i16().ok_or_else(bad)? as i32;
	let y = reader.i16().ok_or_else(bad)? as i32;
	Ok((x, y))
}

/// Type 3: cursive attachment - what joins Arabic letters into a written line.
///
/// EACH GLYPH HAS AN ENTRY AND AN EXIT ANCHOR, and a pair joins when the first has an exit and the
/// second an entry: the second is moved so its entry meets the first's exit. Without it a joining
/// script is rendered as letters that touch but do not connect, which is the difference between
/// handwriting and a row of shapes.
pub fn cursive(subtable: Reader<'_>, buffer: &mut Buffer, at: usize) -> Result<bool, Error> {
	if at + 1 >= buffer.len() {
		return Ok(false);
	}
	let mut reader = subtable;
	let format = reader.u16().ok_or_else(bad)?;
	if format != 1 {
		return Err(Error::Unsupported(Unsupported::SubtableFormat { table: *b"GPOS", format }));
	}
	let coverage_at = reader.u16().ok_or_else(bad)? as usize;
	let count = reader.u16().ok_or_else(bad)? as usize;
	let coverage = subtable.slice(coverage_at, subtable.len().checked_sub(coverage_at).ok_or_else(bad)?).ok_or_else(bad)?;
	let Some(first) = coverage_index(coverage, buffer.infos[at].glyph)? else { return Ok(false) };
	let Some(second) = coverage_index(coverage, buffer.infos[at + 1].glyph)? else { return Ok(false) };
	if first as usize >= count || second as usize >= count {
		return Err(bad());
	}
	// Each entry is an entry anchor offset and an exit anchor offset, both from the subtable start.
	let exit_offset = subtable.u16_at(3 + (first as usize) * 2 + 1).ok_or_else(bad)? as usize;
	let entry_offset = subtable.u16_at(3 + (second as usize) * 2).ok_or_else(bad)? as usize;
	// A zero offset is no anchor, which is an ordinary answer: this pair does not join.
	if exit_offset == 0 || entry_offset == 0 {
		return Ok(false);
	}
	let (exit_x, exit_y) = anchor(subtable, exit_offset)?;
	let (entry_x, entry_y) = anchor(subtable, entry_offset)?;
	// The first glyph's advance is shortened to its exit, and the second is moved so its entry lands
	// there - which is what makes the two share a join rather than merely abut.
	buffer.positions[at].x_advance = exit_x;
	buffer.positions[at + 1].x_offset += -entry_x;
	buffer.positions[at + 1].y_offset += exit_y - entry_y;
	Ok(true)
}

/// Type 5: a mark attached to one COMPONENT of a ligature.
///
/// THE COMPONENT IS WHY THIS IS NOT TYPE 4. A ligature is one glyph standing for several characters,
/// and a mark belonging to the second of them must attach to the second component's anchor - putting
/// it on the first is an accent under the wrong half of a ligature, which is what a naive shaper
/// does with Arabic and Devanagari.
pub fn mark_to_ligature(subtable: Reader<'_>, buffer: &mut Buffer, at: usize) -> Result<bool, Error> {
	if at == 0 {
		return Ok(false);
	}
	let mut reader = subtable;
	let format = reader.u16().ok_or_else(bad)?;
	if format != 1 {
		return Err(Error::Unsupported(Unsupported::SubtableFormat { table: *b"GPOS", format }));
	}
	let mark_coverage_at = reader.u16().ok_or_else(bad)? as usize;
	let ligature_coverage_at = reader.u16().ok_or_else(bad)? as usize;
	let class_count = reader.u16().ok_or_else(bad)? as usize;
	let mark_array_at = reader.u16().ok_or_else(bad)? as usize;
	let ligature_array_at = reader.u16().ok_or_else(bad)? as usize;

	let mark_coverage = subtable.slice(mark_coverage_at, subtable.len().checked_sub(mark_coverage_at).ok_or_else(bad)?).ok_or_else(bad)?;
	let Some(mark_index) = coverage_index(mark_coverage, buffer.infos[at].glyph)? else { return Ok(false) };
	// The ligature is the nearest preceding glyph that is not a mark.
	let mut base_at = None;
	for candidate in (0..at).rev() {
		if buffer.infos[candidate].class != 3 {
			base_at = Some(candidate);
			break;
		}
	}
	let Some(base_at) = base_at else { return Ok(false) };
	let ligature_coverage = subtable.slice(ligature_coverage_at, subtable.len().checked_sub(ligature_coverage_at).ok_or_else(bad)?).ok_or_else(bad)?;
	let Some(ligature_index) = coverage_index(ligature_coverage, buffer.infos[base_at].glyph)? else { return Ok(false) };

	let mark_array = subtable.slice(mark_array_at, subtable.len().checked_sub(mark_array_at).ok_or_else(bad)?).ok_or_else(bad)?;
	let mut reader = mark_array;
	let mark_count = reader.u16().ok_or_else(bad)? as usize;
	if mark_index as usize >= mark_count {
		return Err(bad());
	}
	let record_at = 2usize.checked_add((mark_index as usize).checked_mul(4).ok_or_else(bad)?).ok_or_else(bad)?;
	let mut record = mark_array;
	record.seek(record_at).ok_or_else(bad)?;
	let mark_class = record.u16().ok_or_else(bad)? as usize;
	let mark_anchor_at = record.u16().ok_or_else(bad)? as usize;
	if mark_class >= class_count {
		return Err(bad());
	}
	let (mark_x, mark_y) = anchor(mark_array, mark_anchor_at)?;

	let ligature_array = subtable.slice(ligature_array_at, subtable.len().checked_sub(ligature_array_at).ok_or_else(bad)?).ok_or_else(bad)?;
	let mut reader = ligature_array;
	let ligature_count = reader.u16().ok_or_else(bad)? as usize;
	if ligature_index as usize >= ligature_count {
		return Err(bad());
	}
	let attach_offset = ligature_array.u16_at(1 + ligature_index as usize).ok_or_else(bad)? as usize;
	let attach = ligature_array.slice(attach_offset, ligature_array.len().checked_sub(attach_offset).ok_or_else(bad)?).ok_or_else(bad)?;
	let mut reader = attach;
	let components = reader.u16().ok_or_else(bad)? as usize;
	if components == 0 {
		return Err(bad());
	}
	// WHICH COMPONENT: the mark's cluster against the ligature's tells how far into it the mark
	// belongs. A mark whose cluster matches the ligature's own attaches to the first component.
	let component = (buffer.infos[at].cluster.saturating_sub(buffer.infos[base_at].cluster) as usize).min(components - 1);
	let cell = component.checked_mul(class_count).ok_or_else(bad)?.checked_add(mark_class).ok_or_else(bad)?;
	let anchor_at = 2usize.checked_add(cell.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
	let offset = attach.u16_at(anchor_at / 2).ok_or_else(bad)? as usize;
	if offset == 0 {
		return Ok(false);
	}
	let (base_x, base_y) = anchor(attach, offset)?;
	let advance_between: i32 = buffer.positions[base_at..at].iter().map(|position| position.x_advance).sum();
	buffer.positions[at].x_offset = base_x - mark_x - advance_between;
	buffer.positions[at].y_offset = base_y - mark_y;
	buffer.infos[at].attached_to = u16::try_from(base_at).ok();
	Ok(true)
}
