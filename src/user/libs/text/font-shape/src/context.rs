//! Contextual and chaining rules: `GSUB` 5 and 6, `GPOS` 7 and 8.
//!
//! ONE IMPLEMENTATION FOR FOUR LOOKUP TYPES, because they are one structure with two payloads. A
//! contextual rule matches a sequence and then applies OTHER lookups at positions inside it; a
//! chaining one matches a backtrack and a lookahead around that sequence as well. What differs
//! between `GSUB` and `GPOS` is only which table the nested lookups come from, and writing it twice
//! is writing the backtrack-is-stored-backwards rule twice.
//!
//! THE BACKTRACK IS STORED IN REVERSE ORDER, which is the single thing implementations of this get
//! wrong: the first entry of the backtrack array is the glyph IMMEDIATELY BEFORE the match, not the
//! one furthest from it. Reading it forwards matches text that is the mirror of what the font asked
//! for, which fires a rule almost at random.
//!
//! AND A NESTED LOOKUP IS APPLIED AT A POSITION, NOT AT THE CURSOR. A rule says "at sequence index
//! 2, apply lookup 7"; applying it where the match started is how a substitution lands on the wrong
//! glyph of the run it matched.
//!
//! EVERY GLYPH OF A MATCH IS ONE THE LOOKUP SEES. The input after its first glyph, the backtrack and
//! the lookahead are each the next glyph the lookup's flags do not skip - so a rule written to ignore
//! marks matches across them - and a sequence index names the glyph the match found there, not the
//! buffer slot that many places on.

use alloc::vec::Vec;
use font_parse::{Error, Malformed, Reader};
use opentype_profile::Unsupported;

use crate::Table;
use crate::buffer::Buffer;
use crate::layout::{LayoutTable, Lookup, class_of, coverage_index};

fn bad(table: Table) -> Error {
	Error::Malformed(Malformed::InconsistentTable { table: table.tag() })
}

/// How deep a nested lookup may go.
///
/// TAKEN FROM THE PROFILE RATHER THAN CHOSEN HERE. A contextual rule applying a contextual rule is
/// ordinary; one doing so sixty-four deep is a font arranging for a shaper to recurse until the stack
/// ends. A ceiling this file picked for itself would be a ceiling nobody froze, and the gate that
/// asks whether every numeric limit is tested would have no number to test at.
pub const MAX_NESTING: u8 = opentype_profile::limits::CONTEXT_DEPTH as u8;

/// One nested application a matched rule asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Nested {
	sequence_index: u16,
	lookup_index: u16,
}

/// Apply a contextual or chaining subtable at one position.
///
/// THE NESTED LOOKUP GOES BACK THROUGH THE DRIVER, which is what bounds the recursion: `depth` rises
/// on every nesting and the driver refuses past `MAX_NESTING`. A contextual rule applying a
/// contextual rule is ordinary; one doing so eight deep is a font arranging for a shaper to recurse
/// until the stack ends.
#[allow(clippy::too_many_arguments)]
pub fn apply(table: Table, kind: u16, subtable: Reader<'_>, buffer: &mut Buffer, at: usize, layout: &LayoutTable<'_>, depth: u8, lookup: &Lookup<'_>) -> Result<bool, Error> {
	if depth >= MAX_NESTING {
		// A REFUSAL AND NOT A SILENT STOP. Declining to apply the lookup would leave the run shaped
		// as though the font had not asked, which is a document rendered wrong with nothing to say so.
		return Err(Error::Unsupported(opentype_profile::Unsupported::Exceeded { limit: "context depth", ceiling: opentype_profile::limits::CONTEXT_DEPTH, asked: depth as u64 + 1 }));
	}
	let chaining = matches!(kind, 6 | 8);
	let mut reader = subtable;
	let format = reader.u16().ok_or_else(|| bad(table))?;
	let matched = match (chaining, format) {
		(false, 1) => simple_glyph_rules(table, subtable, buffer, at, lookup)?,
		(false, 2) => simple_class_rules(table, subtable, buffer, at, lookup)?,
		(false, 3) => simple_coverage_rule(table, subtable, buffer, at, lookup)?,
		(true, 1) => chain_glyph_rules(table, subtable, buffer, at, lookup)?,
		(true, 2) => chain_class_rules(table, subtable, buffer, at, lookup)?,
		(true, 3) => chain_coverage_rule(table, subtable, buffer, at, lookup)?,
		(_, other) => return Err(Error::Unsupported(Unsupported::SubtableFormat { table: table.tag(), format: other })),
	};
	let Some((mut positions, nested)) = matched else { return Ok(false) };
	let mut changed = false;
	// WHERE THE MATCH ENDS, which a nested substitution that removes glyphs can move back but never
	// past the glyph it was applied at.
	let mut end = positions.last().map_or(at, |last| *last) as isize + 1;
	for entry in nested {
		let index = entry.sequence_index as usize;
		let Some(&position) = positions.get(index) else { continue };
		let before = buffer.len();
		// A POSITION AN EARLIER NESTED SUBSTITUTION REMOVED is not applied at: it is no longer there.
		if position >= before {
			continue;
		}
		if crate::shape::apply_lookup_at(table, layout, entry.lookup_index, buffer, position, depth + 1)? {
			changed = true;
		}
		let delta = buffer.len() as isize - before as isize;
		if delta != 0 && !reposition(&mut positions, index, delta, &mut end)? {
			break;
		}
	}
	Ok(changed)
}

/// THE MATCH'S POSITIONS AFTER A NESTED SUBSTITUTION CHANGED THE RUN'S LENGTH by `delta` at the one
/// at `index` - which is OpenType's own rule, as HarfBuzz applies it: glyphs a substitution added
/// come right after where it was applied and take the next places in the match, and glyphs it
/// removed are the places after it, never more of them than there are. Every later position moves
/// by what is left of the change. False when the match cannot be followed any further.
fn reposition(positions: &mut Vec<usize>, index: usize, delta: isize, end: &mut isize) -> Result<bool, Error> {
	let mut delta = delta;
	*end += delta;
	let applied = positions[index] as isize;
	if *end < applied {
		delta += applied - *end;
		*end = applied;
	}
	let next = index + 1;
	if delta > 0 {
		let grown = delta as usize;
		positions.try_reserve(grown).map_err(|_| Error::Allocation)?;
		for _ in 0..grown {
			positions.insert(next, 0);
		}
		for at in next..next + grown {
			positions[at] = positions[at - 1] + 1;
		}
		for at in next + grown..positions.len() {
			positions[at] += grown;
		}
	} else if delta < 0 {
		let after = positions.len() - next;
		let removed = ((-delta) as usize).min(after);
		positions.drain(next..next + removed);
		for at in next..positions.len() {
			let Some(moved) = positions[at].checked_sub(removed) else { return Ok(false) };
			positions[at] = moved;
		}
	}
	Ok(true)
}

/// Room for the positions a match of `count` glyphs finds, asked for rather than assumed: the count
/// is the font's. More than the run has glyphs left cannot match, and is answered with no room.
fn positions_for(buffer: &Buffer, at: usize, count: usize) -> Result<Option<Vec<usize>>, Error> {
	if count > buffer.len().saturating_sub(at) {
		return Ok(None);
	}
	let mut positions = Vec::new();
	positions.try_reserve_exact(count).map_err(|_| Error::Allocation)?;
	positions.push(at);
	Ok(Some(positions))
}

/// The nested applications a rule carries, read from a rule table positioned after its input.
fn nested_of(table: Table, rule: Reader<'_>, at: usize, count: usize) -> Result<Vec<Nested>, Error> {
	// THE COUNT IS THE FONT'S, so the storage for it is asked for rather than assumed.
	let mut out = Vec::new();
	out.try_reserve_exact(count).map_err(|_| Error::Allocation)?;
	for index in 0..count {
		let offset = at.checked_add(index.checked_mul(4).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
		let mut reader = rule;
		reader.seek(offset).ok_or_else(|| bad(table))?;
		let sequence_index = reader.u16().ok_or_else(|| bad(table))?;
		let lookup_index = reader.u16().ok_or_else(|| bad(table))?;
		out.push(Nested { sequence_index, lookup_index });
	}
	Ok(out)
}

/// What a matched rule found: where each glyph of its input sequence is, and what it asks for there.
type Matched = Option<(Vec<usize>, Vec<Nested>)>;

/// Format 1 of the contextual types: rules written out as glyph sequences.
fn simple_glyph_rules(table: Table, subtable: Reader<'_>, buffer: &Buffer, at: usize, lookup: &Lookup<'_>) -> Result<Matched, Error> {
	let mut reader = subtable;
	let _format = reader.u16().ok_or_else(|| bad(table))?;
	let coverage_at = reader.u16().ok_or_else(|| bad(table))? as usize;
	let set_count = reader.u16().ok_or_else(|| bad(table))? as usize;
	let coverage = subtable.slice(coverage_at, subtable.len().checked_sub(coverage_at).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
	let Some(index) = coverage_index(coverage, buffer.infos[at].glyph)? else { return Ok(None) };
	if index as usize >= set_count {
		return Err(bad(table));
	}
	let set_offset = subtable.u16_at(3 + index as usize).ok_or_else(|| bad(table))? as usize;
	if set_offset == 0 {
		return Ok(None);
	}
	let set = subtable.slice(set_offset, subtable.len().checked_sub(set_offset).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
	let mut reader = set;
	let rules = reader.u16().ok_or_else(|| bad(table))? as usize;
	for rule_index in 0..rules {
		let rule_offset = set.u16_at(1 + rule_index).ok_or_else(|| bad(table))? as usize;
		let rule = set.slice(rule_offset, set.len().checked_sub(rule_offset).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
		let mut reader = rule;
		let glyph_count = reader.u16().ok_or_else(|| bad(table))? as usize;
		let nested_count = reader.u16().ok_or_else(|| bad(table))? as usize;
		if glyph_count == 0 {
			return Err(bad(table));
		}
		// The first glyph of the sequence is the covered one, so the rule lists the rest.
		let Some(mut positions) = positions_for(buffer, at, glyph_count)? else { continue };
		for offset in 1..glyph_count {
			let expected = rule.u16_at(2 + offset - 1).ok_or_else(|| bad(table))?;
			match lookup.next(buffer, positions[offset - 1])? {
				Some(next) if buffer.infos[next].glyph == expected => positions.push(next),
				_ => break,
			}
		}
		if positions.len() == glyph_count {
			let nested_at = 4usize.checked_add((glyph_count - 1).checked_mul(2).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
			return Ok(Some((positions, nested_of(table, rule, nested_at, nested_count)?)));
		}
	}
	Ok(None)
}

/// Format 2: rules written as class sequences, which is how a font writes one rule for a whole
/// category of glyphs.
fn simple_class_rules(table: Table, subtable: Reader<'_>, buffer: &Buffer, at: usize, lookup: &Lookup<'_>) -> Result<Matched, Error> {
	let mut reader = subtable;
	let _format = reader.u16().ok_or_else(|| bad(table))?;
	let coverage_at = reader.u16().ok_or_else(|| bad(table))? as usize;
	let class_at = reader.u16().ok_or_else(|| bad(table))? as usize;
	let set_count = reader.u16().ok_or_else(|| bad(table))? as usize;
	let coverage = subtable.slice(coverage_at, subtable.len().checked_sub(coverage_at).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
	if coverage_index(coverage, buffer.infos[at].glyph)?.is_none() {
		return Ok(None);
	}
	let classes = subtable.slice(class_at, subtable.len().checked_sub(class_at).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
	let class = class_of(classes, buffer.infos[at].glyph)? as usize;
	if class >= set_count {
		return Ok(None);
	}
	let set_offset = subtable.u16_at(4 + class).ok_or_else(|| bad(table))? as usize;
	if set_offset == 0 {
		return Ok(None);
	}
	let set = subtable.slice(set_offset, subtable.len().checked_sub(set_offset).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
	let mut reader = set;
	let rules = reader.u16().ok_or_else(|| bad(table))? as usize;
	for rule_index in 0..rules {
		let rule_offset = set.u16_at(1 + rule_index).ok_or_else(|| bad(table))? as usize;
		let rule = set.slice(rule_offset, set.len().checked_sub(rule_offset).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
		let mut reader = rule;
		let glyph_count = reader.u16().ok_or_else(|| bad(table))? as usize;
		let nested_count = reader.u16().ok_or_else(|| bad(table))? as usize;
		if glyph_count == 0 {
			return Err(bad(table));
		}
		let Some(mut positions) = positions_for(buffer, at, glyph_count)? else { continue };
		for offset in 1..glyph_count {
			let expected = rule.u16_at(2 + offset - 1).ok_or_else(|| bad(table))?;
			match lookup.next(buffer, positions[offset - 1])? {
				Some(next) if class_of(classes, buffer.infos[next].glyph)? == expected => positions.push(next),
				_ => break,
			}
		}
		if positions.len() == glyph_count {
			let nested_at = 4usize.checked_add((glyph_count - 1).checked_mul(2).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
			return Ok(Some((positions, nested_of(table, rule, nested_at, nested_count)?)));
		}
	}
	Ok(None)
}

/// Format 3: one rule, written as a run of coverage tables.
fn simple_coverage_rule(table: Table, subtable: Reader<'_>, buffer: &Buffer, at: usize, lookup: &Lookup<'_>) -> Result<Matched, Error> {
	let mut reader = subtable;
	let _format = reader.u16().ok_or_else(|| bad(table))?;
	let glyph_count = reader.u16().ok_or_else(|| bad(table))? as usize;
	let nested_count = reader.u16().ok_or_else(|| bad(table))? as usize;
	if glyph_count == 0 {
		return Err(bad(table));
	}
	let Some(positions) = input_by_coverage(table, subtable, 6, glyph_count, buffer, at, lookup)? else { return Ok(None) };
	let nested_at = 6usize.checked_add(glyph_count.checked_mul(2).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
	Ok(Some((positions, nested_of(table, subtable, nested_at, nested_count)?)))
}

/// An input sequence written as a run of coverage tables, from `at`: where each of its glyphs is, or
/// `None` when it does not match there.
fn input_by_coverage(table: Table, subtable: Reader<'_>, offsets_at: usize, count: usize, buffer: &Buffer, at: usize, lookup: &Lookup<'_>) -> Result<Option<Vec<usize>>, Error> {
	let Some(mut positions) = positions_for(buffer, at, count)? else { return Ok(None) };
	for index in 0..count {
		let coverage_at = subtable.u16_at(offsets_at / 2 + index).ok_or_else(|| bad(table))? as usize;
		let coverage = subtable.slice(coverage_at, subtable.len().checked_sub(coverage_at).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
		let position = if index == 0 {
			at
		} else {
			match lookup.next(buffer, positions[index - 1])? {
				Some(next) => {
					positions.push(next);
					next
				}
				None => return Ok(None),
			}
		};
		if coverage_index(coverage, buffer.infos[position].glyph)?.is_none() {
			return Ok(None);
		}
	}
	Ok(Some(positions))
}

/// Does a run of coverage tables match, reading the buffer backwards from `before`?
///
/// THE BACKTRACK IS STORED NEAREST-FIRST. Entry 0 is the glyph immediately before the match, entry 1
/// the one before that. Reading it forwards matches the mirror of what the font asked for.
fn backtrack_matches(table: Table, subtable: Reader<'_>, offsets_at: usize, count: usize, buffer: &Buffer, before: usize, lookup: &Lookup<'_>) -> Result<bool, Error> {
	let mut position = before;
	for index in 0..count {
		let Some(found) = lookup.previous(buffer, position)? else { return Ok(false) };
		let coverage_at = subtable.u16_at(offsets_at / 2 + index).ok_or_else(|| bad(table))? as usize;
		let coverage = subtable.slice(coverage_at, subtable.len().checked_sub(coverage_at).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
		if coverage_index(coverage, buffer.infos[found].glyph)?.is_none() {
			return Ok(false);
		}
		position = found;
	}
	Ok(true)
}

/// The same for a lookahead, read FORWARDS from the glyph after `after`.
fn lookahead_matches(table: Table, subtable: Reader<'_>, offsets_at: usize, count: usize, buffer: &Buffer, after: usize, lookup: &Lookup<'_>) -> Result<bool, Error> {
	let mut position = after;
	for index in 0..count {
		let Some(found) = lookup.next(buffer, position)? else { return Ok(false) };
		let coverage_at = subtable.u16_at(offsets_at / 2 + index).ok_or_else(|| bad(table))? as usize;
		let coverage = subtable.slice(coverage_at, subtable.len().checked_sub(coverage_at).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
		if coverage_index(coverage, buffer.infos[found].glyph)?.is_none() {
			return Ok(false);
		}
		position = found;
	}
	Ok(true)
}

/// Chaining format 3: backtrack, input and lookahead, each a run of coverage tables.
fn chain_coverage_rule(table: Table, subtable: Reader<'_>, buffer: &Buffer, at: usize, lookup: &Lookup<'_>) -> Result<Matched, Error> {
	let mut reader = subtable;
	let _format = reader.u16().ok_or_else(|| bad(table))?;
	let backtrack_count = reader.u16().ok_or_else(|| bad(table))? as usize;
	let backtrack_at = reader.position();
	reader.skip(backtrack_count.checked_mul(2).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
	let input_count = reader.u16().ok_or_else(|| bad(table))? as usize;
	let input_at = reader.position();
	reader.skip(input_count.checked_mul(2).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
	let lookahead_count = reader.u16().ok_or_else(|| bad(table))? as usize;
	let lookahead_at = reader.position();
	reader.skip(lookahead_count.checked_mul(2).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
	let nested_count = reader.u16().ok_or_else(|| bad(table))? as usize;
	let nested_at = reader.position();

	if input_count == 0 {
		return Err(bad(table));
	}
	if !backtrack_matches(table, subtable, backtrack_at, backtrack_count, buffer, at, lookup)? {
		return Ok(None);
	}
	let Some(positions) = input_by_coverage(table, subtable, input_at, input_count, buffer, at, lookup)? else { return Ok(None) };
	let last = positions[positions.len() - 1];
	if !lookahead_matches(table, subtable, lookahead_at, lookahead_count, buffer, last, lookup)? {
		return Ok(None);
	}
	Ok(Some((positions, nested_of(table, subtable, nested_at, nested_count)?)))
}

/// Chaining formats 1 and 2: rule sets selected by the first glyph or its class, each rule carrying
/// its own backtrack, input and lookahead as glyph or class sequences.
fn chain_rules(table: Table, subtable: Reader<'_>, buffer: &Buffer, at: usize, by_class: bool, lookup: &Lookup<'_>) -> Result<Matched, Error> {
	let mut reader = subtable;
	let _format = reader.u16().ok_or_else(|| bad(table))?;
	let coverage_at = reader.u16().ok_or_else(|| bad(table))? as usize;
	let coverage = subtable.slice(coverage_at, subtable.len().checked_sub(coverage_at).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
	let Some(covered) = coverage_index(coverage, buffer.infos[at].glyph)? else { return Ok(None) };

	// Format 2 selects its rule set by the input CLASS and carries three class definitions.
	let (set_index, backtrack_classes, input_classes, lookahead_classes, sets_at, set_count) = if by_class {
		let backtrack_at = reader.u16().ok_or_else(|| bad(table))? as usize;
		let input_at = reader.u16().ok_or_else(|| bad(table))? as usize;
		let lookahead_at = reader.u16().ok_or_else(|| bad(table))? as usize;
		let count = reader.u16().ok_or_else(|| bad(table))? as usize;
		let resolve = |offset: usize| -> Result<Reader<'_>, Error> { subtable.slice(offset, subtable.len().checked_sub(offset).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table)) };
		let input = resolve(input_at)?;
		let class = class_of(input, buffer.infos[at].glyph)? as usize;
		(class, Some(resolve(backtrack_at)?), Some(input), Some(resolve(lookahead_at)?), 12usize, count)
	} else {
		let count = reader.u16().ok_or_else(|| bad(table))? as usize;
		(covered as usize, None, None, None, 6usize, count)
	};
	if set_index >= set_count {
		return Ok(None);
	}
	let set_offset = subtable.u16_at(sets_at / 2 + set_index).ok_or_else(|| bad(table))? as usize;
	if set_offset == 0 {
		return Ok(None);
	}
	let set = subtable.slice(set_offset, subtable.len().checked_sub(set_offset).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
	let mut reader = set;
	let rules = reader.u16().ok_or_else(|| bad(table))? as usize;
	for rule_index in 0..rules {
		let rule_offset = set.u16_at(1 + rule_index).ok_or_else(|| bad(table))? as usize;
		let rule = set.slice(rule_offset, set.len().checked_sub(rule_offset).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
		let mut reader = rule;
		let backtrack_count = reader.u16().ok_or_else(|| bad(table))? as usize;
		let backtrack_at = reader.position();
		reader.skip(backtrack_count.checked_mul(2).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
		let input_count = reader.u16().ok_or_else(|| bad(table))? as usize;
		let input_at = reader.position();
		if input_count == 0 {
			return Err(bad(table));
		}
		reader.skip((input_count - 1).checked_mul(2).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
		let lookahead_count = reader.u16().ok_or_else(|| bad(table))? as usize;
		let lookahead_at = reader.position();
		reader.skip(lookahead_count.checked_mul(2).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
		let nested_count = reader.u16().ok_or_else(|| bad(table))? as usize;
		let nested_at = reader.position();

		// The value each position is matched against: a glyph id in format 1, a class in format 2.
		let value_of = |position: usize| -> Result<Option<u16>, Error> {
			let Some(info) = buffer.infos.get(position) else { return Ok(None) };
			Ok(Some(match &input_classes {
				Some(classes) => class_of(*classes, info.glyph)?,
				None => info.glyph,
			}))
		};
		// THE BACKTRACK, read backwards from the match through the glyphs the lookup sees.
		let mut matched = true;
		let mut position = at;
		for index in 0..backtrack_count {
			let Some(found) = lookup.previous(buffer, position)? else {
				matched = false;
				break;
			};
			let expected = rule.u16_at(backtrack_at / 2 + index).ok_or_else(|| bad(table))?;
			let glyph = buffer.infos[found].glyph;
			let actual = match &backtrack_classes {
				Some(classes) => class_of(*classes, glyph)?,
				None => glyph,
			};
			if actual != expected {
				matched = false;
				break;
			}
			position = found;
		}
		if !matched {
			continue;
		}
		let Some(mut positions) = positions_for(buffer, at, input_count)? else { continue };
		for index in 1..input_count {
			let expected = rule.u16_at(input_at / 2 + index - 1).ok_or_else(|| bad(table))?;
			match lookup.next(buffer, positions[index - 1])? {
				Some(next) if value_of(next)? == Some(expected) => positions.push(next),
				_ => break,
			}
		}
		if positions.len() != input_count {
			continue;
		}
		let mut position = positions[input_count - 1];
		for index in 0..lookahead_count {
			let Some(found) = lookup.next(buffer, position)? else {
				matched = false;
				break;
			};
			let expected = rule.u16_at(lookahead_at / 2 + index).ok_or_else(|| bad(table))?;
			let glyph = buffer.infos[found].glyph;
			let actual = match &lookahead_classes {
				Some(classes) => class_of(*classes, glyph)?,
				None => glyph,
			};
			if actual != expected {
				matched = false;
				break;
			}
			position = found;
		}
		if matched {
			return Ok(Some((positions, nested_of(table, rule, nested_at, nested_count)?)));
		}
	}
	Ok(None)
}

fn chain_glyph_rules(table: Table, subtable: Reader<'_>, buffer: &Buffer, at: usize, lookup: &Lookup<'_>) -> Result<Matched, Error> {
	chain_rules(table, subtable, buffer, at, false, lookup)
}

fn chain_class_rules(table: Table, subtable: Reader<'_>, buffer: &Buffer, at: usize, lookup: &Lookup<'_>) -> Result<Matched, Error> {
	chain_rules(table, subtable, buffer, at, true, lookup)
}
