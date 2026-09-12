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

use alloc::vec::Vec;
use font_parse::{Error, Malformed, Reader};
use opentype_profile::Unsupported;

use crate::Table;
use crate::buffer::Buffer;
use crate::layout::{LayoutTable, class_of, coverage_index};

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
pub fn apply(table: Table, kind: u16, subtable: Reader<'_>, buffer: &mut Buffer, at: usize, layout: &LayoutTable<'_>, depth: u8) -> Result<bool, Error> {
	if depth >= MAX_NESTING {
		// A REFUSAL AND NOT A SILENT STOP. Declining to apply the lookup would leave the run shaped
		// as though the font had not asked, which is a document rendered wrong with nothing to say so.
		return Err(Error::Unsupported(opentype_profile::Unsupported::Exceeded { limit: "context depth", ceiling: opentype_profile::limits::CONTEXT_DEPTH, asked: depth as u64 + 1 }));
	}
	let chaining = matches!(kind, 6 | 8);
	let mut reader = subtable;
	let format = reader.u16().ok_or_else(|| bad(table))?;
	let matched = match (chaining, format) {
		(false, 1) => simple_glyph_rules(table, subtable, buffer, at)?,
		(false, 2) => simple_class_rules(table, subtable, buffer, at)?,
		(false, 3) => simple_coverage_rule(table, subtable, buffer, at)?,
		(true, 1) => chain_glyph_rules(table, subtable, buffer, at)?,
		(true, 2) => chain_class_rules(table, subtable, buffer, at)?,
		(true, 3) => chain_coverage_rule(table, subtable, buffer, at)?,
		(_, other) => return Err(Error::Unsupported(Unsupported::SubtableFormat { table: table.tag(), format: other })),
	};
	let Some((length, nested)) = matched else { return Ok(false) };
	let _ = length;
	let mut changed = false;
	for entry in nested {
		let position = at.checked_add(entry.sequence_index as usize).ok_or_else(|| bad(table))?;
		if position >= buffer.len() {
			continue;
		}
		if crate::shape::apply_lookup_at(table, layout, entry.lookup_index, buffer, position, depth + 1)? {
			changed = true;
		}
	}
	Ok(changed)
}

/// The nested applications a rule carries, read from a rule table positioned after its input.
fn nested_of(table: Table, rule: Reader<'_>, at: usize, count: usize) -> Result<Vec<Nested>, Error> {
	let mut out = Vec::new();
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

/// Format 1 of the contextual types: rules written out as glyph sequences.
fn simple_glyph_rules(table: Table, subtable: Reader<'_>, buffer: &Buffer, at: usize) -> Result<Option<(usize, Vec<Nested>)>, Error> {
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
		let mut matched = true;
		for offset in 1..glyph_count {
			let expected = rule.u16_at(2 + offset - 1).ok_or_else(|| bad(table))?;
			match buffer.infos.get(at + offset) {
				Some(info) if info.glyph == expected => {}
				_ => {
					matched = false;
					break;
				}
			}
		}
		if matched {
			let nested_at = 4usize.checked_add((glyph_count - 1).checked_mul(2).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
			return Ok(Some((glyph_count, nested_of(table, rule, nested_at, nested_count)?)));
		}
	}
	Ok(None)
}

/// Format 2: rules written as class sequences, which is how a font writes one rule for a whole
/// category of glyphs.
fn simple_class_rules(table: Table, subtable: Reader<'_>, buffer: &Buffer, at: usize) -> Result<Option<(usize, Vec<Nested>)>, Error> {
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
		let mut matched = true;
		for offset in 1..glyph_count {
			let expected = rule.u16_at(2 + offset - 1).ok_or_else(|| bad(table))?;
			match buffer.infos.get(at + offset) {
				Some(info) if class_of(classes, info.glyph)? == expected => {}
				_ => {
					matched = false;
					break;
				}
			}
		}
		if matched {
			let nested_at = 4usize.checked_add((glyph_count - 1).checked_mul(2).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
			return Ok(Some((glyph_count, nested_of(table, rule, nested_at, nested_count)?)));
		}
	}
	Ok(None)
}

/// Format 3: one rule, written as a run of coverage tables.
fn simple_coverage_rule(table: Table, subtable: Reader<'_>, buffer: &Buffer, at: usize) -> Result<Option<(usize, Vec<Nested>)>, Error> {
	let mut reader = subtable;
	let _format = reader.u16().ok_or_else(|| bad(table))?;
	let glyph_count = reader.u16().ok_or_else(|| bad(table))? as usize;
	let nested_count = reader.u16().ok_or_else(|| bad(table))? as usize;
	if glyph_count == 0 {
		return Err(bad(table));
	}
	for offset in 0..glyph_count {
		let coverage_at = subtable.u16_at(3 + offset).ok_or_else(|| bad(table))? as usize;
		let coverage = subtable.slice(coverage_at, subtable.len().checked_sub(coverage_at).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
		let Some(info) = buffer.infos.get(at + offset) else { return Ok(None) };
		if coverage_index(coverage, info.glyph)?.is_none() {
			return Ok(None);
		}
	}
	let nested_at = 6usize.checked_add(glyph_count.checked_mul(2).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
	Ok(Some((glyph_count, nested_of(table, subtable, nested_at, nested_count)?)))
}

/// Does a run of coverage tables match, reading the buffer backwards from `before`?
///
/// THE BACKTRACK IS STORED NEAREST-FIRST. Entry 0 is the glyph immediately before the match, entry 1
/// the one before that. Reading it forwards matches the mirror of what the font asked for.
fn backtrack_matches(table: Table, subtable: Reader<'_>, offsets_at: usize, count: usize, buffer: &Buffer, before: usize) -> Result<bool, Error> {
	for index in 0..count {
		let Some(position) = before.checked_sub(index + 1) else { return Ok(false) };
		let coverage_at = subtable.u16_at(offsets_at / 2 + index).ok_or_else(|| bad(table))? as usize;
		let coverage = subtable.slice(coverage_at, subtable.len().checked_sub(coverage_at).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
		let Some(info) = buffer.infos.get(position) else { return Ok(false) };
		if coverage_index(coverage, info.glyph)?.is_none() {
			return Ok(false);
		}
	}
	Ok(true)
}

/// Chaining format 3: backtrack, input and lookahead, each a run of coverage tables.
fn chain_coverage_rule(table: Table, subtable: Reader<'_>, buffer: &Buffer, at: usize) -> Result<Option<(usize, Vec<Nested>)>, Error> {
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
	if !backtrack_matches(table, subtable, backtrack_at, backtrack_count, buffer, at)? {
		return Ok(None);
	}
	for index in 0..input_count {
		let coverage_at = subtable.u16_at(input_at / 2 + index).ok_or_else(|| bad(table))? as usize;
		let coverage = subtable.slice(coverage_at, subtable.len().checked_sub(coverage_at).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
		let Some(info) = buffer.infos.get(at + index) else { return Ok(None) };
		if coverage_index(coverage, info.glyph)?.is_none() {
			return Ok(None);
		}
	}
	for index in 0..lookahead_count {
		let coverage_at = subtable.u16_at(lookahead_at / 2 + index).ok_or_else(|| bad(table))? as usize;
		let coverage = subtable.slice(coverage_at, subtable.len().checked_sub(coverage_at).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
		let Some(info) = buffer.infos.get(at + input_count + index) else { return Ok(None) };
		if coverage_index(coverage, info.glyph)?.is_none() {
			return Ok(None);
		}
	}
	Ok(Some((input_count, nested_of(table, subtable, nested_at, nested_count)?)))
}

/// Chaining formats 1 and 2: rule sets selected by the first glyph or its class, each rule carrying
/// its own backtrack, input and lookahead as glyph or class sequences.
fn chain_rules(table: Table, subtable: Reader<'_>, buffer: &Buffer, at: usize, by_class: bool) -> Result<Option<(usize, Vec<Nested>)>, Error> {
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
		let mut matched = true;
		for index in 0..backtrack_count {
			let Some(position) = at.checked_sub(index + 1) else {
				matched = false;
				break;
			};
			let expected = rule.u16_at(backtrack_at / 2 + index).ok_or_else(|| bad(table))?;
			let Some(info) = buffer.infos.get(position) else {
				matched = false;
				break;
			};
			let actual = match &backtrack_classes {
				Some(classes) => class_of(*classes, info.glyph)?,
				None => info.glyph,
			};
			if actual != expected {
				matched = false;
				break;
			}
		}
		if matched {
			for index in 1..input_count {
				let expected = rule.u16_at(input_at / 2 + index - 1).ok_or_else(|| bad(table))?;
				match value_of(at + index)? {
					Some(actual) if actual == expected => {}
					_ => {
						matched = false;
						break;
					}
				}
			}
		}
		if matched {
			for index in 0..lookahead_count {
				let expected = rule.u16_at(lookahead_at / 2 + index).ok_or_else(|| bad(table))?;
				let Some(info) = buffer.infos.get(at + input_count + index) else {
					matched = false;
					break;
				};
				let actual = match &lookahead_classes {
					Some(classes) => class_of(*classes, info.glyph)?,
					None => info.glyph,
				};
				if actual != expected {
					matched = false;
					break;
				}
			}
		}
		if matched {
			return Ok(Some((input_count, nested_of(table, rule, nested_at, nested_count)?)));
		}
	}
	Ok(None)
}

fn chain_glyph_rules(table: Table, subtable: Reader<'_>, buffer: &Buffer, at: usize) -> Result<Option<(usize, Vec<Nested>)>, Error> {
	chain_rules(table, subtable, buffer, at, false)
}

fn chain_class_rules(table: Table, subtable: Reader<'_>, buffer: &Buffer, at: usize) -> Result<Option<(usize, Vec<Nested>)>, Error> {
	chain_rules(table, subtable, buffer, at, true)
}
