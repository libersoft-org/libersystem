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

use alloc::vec::Vec;
use font_parse::{Error, Malformed, Reader};
use opentype_profile::Unsupported;

use crate::buffer::{Buffer, GlyphInfo};
use crate::layout::{Lookup, RIGHT_TO_LEFT, class_of, coverage_index};

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
///
/// `lookup` is the lookup the subtable belongs to, whose flags say which glyphs are passed over on the
/// way to a pair's second glyph, a cursive join's other half and a stacked mark's parent.
pub fn apply_subtable(kind: u16, subtable: Reader<'_>, buffer: &mut Buffer, at: usize, lookup: &Lookup<'_>) -> Result<bool, Error> {
	match kind {
		1 => single(subtable, buffer, at),
		2 => pair(subtable, buffer, at, lookup),
		4 | 6 => mark(kind, subtable, buffer, at, lookup),
		3 => cursive(subtable, buffer, at, lookup),
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
///
/// THE SECOND GLYPH IS THE NEXT ONE THE LOOKUP SEES, so a kerning rule that ignores marks kerns a pair
/// with an accent between them - which is every pointed or vocalised word.
fn pair(subtable: Reader<'_>, buffer: &mut Buffer, at: usize, lookup: &Lookup<'_>) -> Result<bool, Error> {
	let Some(second_at) = lookup.next(buffer, at)? else { return Ok(false) };
	let mut reader = subtable;
	let format = reader.u16().ok_or_else(bad)?;
	let coverage_offset = reader.u16().ok_or_else(bad)? as usize;
	let first_format = reader.u16().ok_or_else(bad)?;
	let second_format = reader.u16().ok_or_else(bad)?;
	let coverage = subtable.slice(coverage_offset, subtable.len().checked_sub(coverage_offset).ok_or_else(bad)?).ok_or_else(bad)?;
	let first = buffer.infos[at].glyph;
	let second = buffer.infos[second_at].glyph;
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
				apply_value(second_format, &mut entry, &mut buffer.positions[second_at])?;
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
			apply_value(second_format, &mut entry, &mut buffer.positions[second_at])?;
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
fn mark(kind: u16, subtable: Reader<'_>, buffer: &mut Buffer, at: usize, lookup: &Lookup<'_>) -> Result<bool, Error> {
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

	// FOR TYPE 4 THE BASE IS THE NEAREST GLYPH BEFORE THE MARK THAT IS NOT A MARK, whatever else the
	// lookup's flags say: every mark between the two hangs from that base as well.
	//
	// FOR TYPE 6 IT IS THE ONE GLYPH BEFORE - the nearest the lookup sees once only the marks it does
	// not name are passed over - and that glyph has to be a mark, of the same base or of the same
	// component of one ligature. A base there ends the search: walking on past it attached an accent
	// on one letter to the previous letter's accent.
	let base_at = if kind == 4 {
		(0..at).rev().find(|candidate| buffer.infos[*candidate].class != 3)
	} else {
		let mut found = None;
		let mut candidate = at;
		while candidate > 0 {
			candidate -= 1;
			if !lookup.skips_marks_named(&buffer.infos[candidate])? {
				found = Some(candidate);
				break;
			}
		}
		found.filter(|previous| buffer.infos[*previous].class == 3 && same_owner(&buffer.infos[at], &buffer.infos[*previous]))
	};
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

	attach(buffer, at, base_at, (base_x - mark_x, base_y - mark_y))
}

/// Attach the mark at `at` to the glyph at `base_at`, its anchors `offset` apart.
///
/// THE ANCHORS AND NOTHING ELSE, here: how far the pen has moved from what the mark hangs from, and
/// where that is drawn, are not final until every positioning rule has run - see `propagate`.
fn attach(buffer: &mut Buffer, at: usize, base_at: usize, offset: (i32, i32)) -> Result<bool, Error> {
	let Ok(base) = u16::try_from(base_at) else { return Ok(false) };
	buffer.positions[at].x_offset = offset.0;
	buffer.positions[at].y_offset = offset.1;
	buffer.infos[at].attached_to = Some(base);
	buffer.infos[at].cursive = None;
	Ok(true)
}

/// Whether a mark may hang from the mark before it: both belong to the same base, or to the same
/// component of one ligature - or one of them is itself a ligature of marks.
fn same_owner(mark: &GlyphInfo, previous: &GlyphInfo) -> bool {
	if mark.ligature == previous.ligature { mark.ligature == 0 || mark.component == previous.component } else { (mark.ligature != 0 && mark.component == 0) || (previous.ligature != 0 && previous.component == 0) }
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
/// EACH GLYPH HAS AN ENTRY AND AN EXIT ANCHOR, and a pair joins when the glyph before has an exit and
/// this one an entry: the two are placed so the anchors meet. Without it a joining script is rendered
/// as letters that touch but do not connect, which is the difference between handwriting and a row
/// of shapes. The glyph before is the nearest the lookup sees, so a mark between two letters does not
/// break their join.
///
/// ALONG THE LINE THE JOIN IS MADE BY THE ADVANCES, and which glyph's advance is the run's direction:
/// drawn left to right the glyph before ends at its exit and this one starts at its entry; drawn right
/// to left the glyph before - drawn to the RIGHT of this one - starts at its exit and this one ends at
/// its entry. Every cursive script the profile names is written right to left, so the arithmetic
/// written only one way round was wrong for all of them.
///
/// ACROSS THE LINE ONE OF THE TWO IS MOVED TO MEET THE OTHER, and the lookup's `rightToLeft` flag says
/// which: set, the LAST glyph of a joined sequence stays on the baseline and each earlier one hangs
/// from the one after it; clear, the first stays and each later one hangs from the one before. A
/// glyph already hanging from another is re-rooted onto its new parent, its old chain reversed - which
/// is what keeps a whole word on one line of joins rather than every pair on its own.
pub fn cursive(subtable: Reader<'_>, buffer: &mut Buffer, at: usize, lookup: &Lookup<'_>) -> Result<bool, Error> {
	let mut reader = subtable;
	let format = reader.u16().ok_or_else(bad)?;
	if format != 1 {
		return Err(Error::Unsupported(Unsupported::SubtableFormat { table: *b"GPOS", format }));
	}
	let coverage_at = reader.u16().ok_or_else(bad)? as usize;
	let count = reader.u16().ok_or_else(bad)? as usize;
	let coverage = subtable.slice(coverage_at, subtable.len().checked_sub(coverage_at).ok_or_else(bad)?).ok_or_else(bad)?;
	// Each entry is an entry anchor offset and an exit anchor offset, both from the subtable start; a
	// zero offset is no anchor, which is an ordinary answer: the pair does not join.
	let record = |glyph: u16, exit: bool| -> Result<usize, Error> {
		let Some(index) = coverage_index(coverage, glyph)? else { return Ok(0) };
		if index as usize >= count {
			return Err(bad());
		}
		Ok(subtable.u16_at(3 + index as usize * 2 + usize::from(exit)).ok_or_else(bad)? as usize)
	};
	let entry_offset = record(buffer.infos[at].glyph, false)?;
	if entry_offset == 0 {
		return Ok(false);
	}
	let Some(before) = lookup.previous(buffer, at)? else { return Ok(false) };
	let exit_offset = record(buffer.infos[before].glyph, true)?;
	if exit_offset == 0 {
		return Ok(false);
	}
	let (exit_x, exit_y) = anchor(subtable, exit_offset)?;
	let (entry_x, entry_y) = anchor(subtable, entry_offset)?;
	let (first, second) = (before, at);
	if buffer.right_to_left {
		let moved = exit_x + buffer.positions[first].x_offset;
		buffer.positions[first].x_advance -= moved;
		buffer.positions[first].x_offset -= moved;
		buffer.positions[second].x_advance = entry_x + buffer.positions[second].x_offset;
	} else {
		buffer.positions[first].x_advance = exit_x + buffer.positions[first].x_offset;
		let moved = entry_x + buffer.positions[second].x_offset;
		buffer.positions[second].x_advance -= moved;
		buffer.positions[second].x_offset -= moved;
	}
	let (mut child, mut parent, mut across) = (first, second, entry_y - exit_y);
	if lookup.flags & RIGHT_TO_LEFT == 0 {
		core::mem::swap(&mut child, &mut parent);
		across = -across;
	}
	let Ok(to) = u16::try_from(parent) else { return Ok(false) };
	reroot(buffer, child, parent)?;
	buffer.infos[child].cursive = Some(to);
	buffer.infos[child].attached_to = None;
	buffer.positions[child].y_offset = across;
	// A PARENT THAT HUNG FROM THIS CHILD is separated from it, or the two would hang from each other.
	if buffer.infos[parent].cursive.map(usize::from) == Some(child) {
		buffer.infos[parent].cursive = None;
		buffer.positions[parent].y_offset = 0;
	}
	Ok(true)
}

/// THE CHAIN A GLYPH HUNG FROM, REVERSED, before it is hung from `parent`: what it hung from now hangs
/// from it, and so on up the old chain - stopping where the chain reaches the new parent - with each
/// offset across the line turned round with its link.
fn reroot(buffer: &mut Buffer, child: usize, parent: usize) -> Result<(), Error> {
	let mut links: Vec<(usize, usize)> = Vec::new();
	let mut node = child;
	while let Some(old) = buffer.infos[node].cursive.map(usize::from) {
		buffer.infos[node].cursive = None;
		if old == parent || old >= buffer.len() || links.len() >= buffer.len() {
			break;
		}
		links.try_reserve(1).map_err(|_| Error::Allocation)?;
		links.push((node, old));
		node = old;
	}
	// FROM THE FAR END BACK, so each offset is turned round before the link below it changes it.
	for (from, old) in links.iter().rev() {
		buffer.positions[*old].y_offset = -buffer.positions[*from].y_offset;
		buffer.infos[*old].cursive = u16::try_from(*from).ok();
	}
	Ok(())
}

/// WHERE EVERY ATTACHED GLYPH ENDS UP, once every positioning rule has run.
///
/// ONCE, AT THE END, AND NOT WHERE EACH RULE ATTACHES, because what an attachment is measured from is
/// not final until positioning is: a kerning rule after the mark rule changes the advance a mark is
/// measured back over, a stacked mark hangs from a mark that moved after it was attached, and a
/// cursive chain is re-rooted by every join. Measured where it attached, a mark on a mark kept none
/// of its parent's movement - a second accent drawn a letter off, toward the next one.
///
/// A MARK takes what it hangs from's offsets and is moved back over everything the pen crossed from
/// there up to it, that glyph included - in logical order; `font-run` turns that round, link by link,
/// for a run drawn right to left. A GLYPH A CURSIVE JOIN HANGS takes its parent's offset across the
/// line; along it, the join was made by the advances. Parents first, so each takes a final answer.
pub fn propagate(buffer: &mut Buffer) -> Result<(), Error> {
	let count = buffer.len();
	if !buffer.infos.iter().any(|info| info.attached_to.is_some() || info.cursive.is_some()) {
		return Ok(());
	}
	let mut done: Vec<bool> = Vec::new();
	done.try_reserve_exact(count).map_err(|_| Error::Allocation)?;
	done.resize(count, false);
	let mut path: Vec<usize> = Vec::new();
	path.try_reserve_exact(count).map_err(|_| Error::Allocation)?;
	for start in 0..count {
		path.clear();
		let mut node = start;
		// UP TO A GLYPH ALREADY PLACED OR ONE THAT HANGS FROM NOTHING - bounded by the run, so a chain
		// a font managed to close on itself ends rather than loops.
		while !done[node] && path.len() < count {
			path.push(node);
			match parent_of(&buffer.infos[node]) {
				Some((parent, _)) if parent < count && parent != node => node = parent,
				_ => break,
			}
		}
		for child in path.iter().rev() {
			let child = *child;
			done[child] = true;
			let Some((parent, mark)) = parent_of(&buffer.infos[child]) else { continue };
			if parent >= count || parent == child {
				continue;
			}
			let above = buffer.positions[parent];
			if mark {
				buffer.positions[child].x_offset = buffer.positions[child].x_offset.saturating_add(above.x_offset);
				buffer.positions[child].y_offset = buffer.positions[child].y_offset.saturating_add(above.y_offset);
				// EVERYTHING FROM WHAT THE MARK HANGS FROM UP TO THE MARK, that glyph INCLUDED: the pen has
				// already advanced past it by the time the mark is drawn, so the mark is moved back by
				// that much to sit on it. Starting after it leaves the mark one glyph's width to the
				// right, which on a letter with a wide base is an accent floating over the NEXT letter.
				let crossed = buffer.positions[parent.min(child)..child].iter().fold((0i32, 0i32), |sum, position| (sum.0.saturating_add(position.x_advance), sum.1.saturating_add(position.y_advance)));
				buffer.positions[child].x_offset = buffer.positions[child].x_offset.saturating_sub(crossed.0);
				buffer.positions[child].y_offset = buffer.positions[child].y_offset.saturating_sub(crossed.1);
			} else {
				buffer.positions[child].y_offset = buffer.positions[child].y_offset.saturating_add(above.y_offset);
			}
		}
	}
	Ok(())
}

/// What a glyph hangs from, and whether as a mark.
fn parent_of(info: &GlyphInfo) -> Option<(usize, bool)> {
	match (info.attached_to, info.cursive) {
		(Some(base), _) => Some((usize::from(base), true)),
		(None, Some(parent)) => Some((usize::from(parent), false)),
		(None, None) => None,
	}
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
	// WHICH COMPONENT: the one the mark followed when the ligature was formed, which the substitution
	// recorded on the mark - a fatha written after the lam of a lam-alef goes over the lam half. A
	// mark that sat in no part of this ligature followed all of it, and takes the last component.
	let mark = buffer.infos[at];
	let ligature = buffer.infos[base_at];
	let component = if ligature.ligature != 0 && mark.ligature == ligature.ligature && mark.component > 0 { usize::from(mark.component).min(components) - 1 } else { components - 1 };
	let cell = component.checked_mul(class_count).ok_or_else(bad)?.checked_add(mark_class).ok_or_else(bad)?;
	let anchor_at = 2usize.checked_add(cell.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
	let offset = attach.u16_at(anchor_at / 2).ok_or_else(bad)? as usize;
	if offset == 0 {
		return Ok(false);
	}
	let (base_x, base_y) = anchor(attach, offset)?;
	self::attach(buffer, at, base_at, (base_x - mark_x, base_y - mark_y))
}
