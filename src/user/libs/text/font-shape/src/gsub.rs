//! `GSUB`: what the font substitutes.
//!
//! FIVE OF THE EIGHT TYPES ARE HERE and the other three are named where they are refused, because a
//! lookup type quietly skipped renders text the font asked to change - which is the failure mode
//! this milestone is shaped against, and worse than a refusal because nobody sees it.
//!
//! EVERY SUBSTITUTION KEEPS THE CLUSTER. A ligature is one glyph for three characters and a
//! decomposition is three for one, and what a caret, a hit test and a selection are built from is
//! which characters a glyph came from - so the buffer carries that and each rule below preserves it.

use font_parse::{Error, Malformed, Reader};
use opentype_profile::Unsupported;

use crate::buffer::Buffer;
use crate::layout::coverage_index;

fn bad() -> Error {
	Error::Malformed(Malformed::InconsistentTable { table: *b"GSUB" })
}

/// Apply one substitution subtable at one position.
///
/// THE CONTEXTUAL TYPES ARE NOT HERE. They apply OTHER lookups rather than substituting anything
/// themselves, so they need the table they came from and the driver that bounds the nesting - see
/// `context`. What is left here is every type that acts on the buffer directly.
pub fn apply_subtable(kind: u16, subtable: Reader<'_>, buffer: &mut Buffer, at: usize) -> Result<bool, Error> {
	match kind {
		1 => single(subtable, buffer, at),
		2 => multiple(subtable, buffer, at),
		3 => alternate(subtable, buffer, at),
		4 => ligature(subtable, buffer, at),
		8 => reverse_chain(subtable, buffer, at),
		other => Err(Error::Unsupported(Unsupported::GsubLookup(other))),
	}
}

/// Type 1: one glyph for another.
fn single(subtable: Reader<'_>, buffer: &mut Buffer, at: usize) -> Result<bool, Error> {
	let mut reader = subtable;
	let format = reader.u16().ok_or_else(bad)?;
	let coverage_offset = reader.u16().ok_or_else(bad)? as usize;
	let coverage = subtable.slice(coverage_offset, subtable.len().checked_sub(coverage_offset).ok_or_else(bad)?).ok_or_else(bad)?;
	let glyph = buffer.infos[at].glyph;
	let Some(index) = coverage_index(coverage, glyph)? else { return Ok(false) };
	match format {
		1 => {
			// A delta applied to the glyph id itself, which is how a font maps a whole alphabet to
			// its small-capital forms in four bytes.
			let delta = reader.i16().ok_or_else(bad)?;
			buffer.substitute(at, glyph.wrapping_add(delta as u16));
			Ok(true)
		}
		2 => {
			let count = reader.u16().ok_or_else(bad)? as usize;
			if index as usize >= count {
				return Err(bad());
			}
			let glyphs = subtable.slice(6, count.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
			buffer.substitute(at, glyphs.u16_at(index as usize).ok_or_else(bad)?);
			Ok(true)
		}
		other => Err(Error::Unsupported(Unsupported::SubtableFormat { table: *b"GSUB", format: other })),
	}
}

/// Type 2: one glyph becomes several - a decomposition.
fn multiple(subtable: Reader<'_>, buffer: &mut Buffer, at: usize) -> Result<bool, Error> {
	let mut reader = subtable;
	let format = reader.u16().ok_or_else(bad)?;
	if format != 1 {
		return Err(Error::Unsupported(Unsupported::SubtableFormat { table: *b"GSUB", format }));
	}
	let coverage_offset = reader.u16().ok_or_else(bad)? as usize;
	let count = reader.u16().ok_or_else(bad)? as usize;
	let coverage = subtable.slice(coverage_offset, subtable.len().checked_sub(coverage_offset).ok_or_else(bad)?).ok_or_else(bad)?;
	let Some(index) = coverage_index(coverage, buffer.infos[at].glyph)? else { return Ok(false) };
	if index as usize >= count {
		return Err(bad());
	}
	let sequences = subtable.slice(6, count.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
	let offset = sequences.u16_at(index as usize).ok_or_else(bad)? as usize;
	let sequence = subtable.slice(offset, subtable.len().checked_sub(offset).ok_or_else(bad)?).ok_or_else(bad)?;
	let mut reader = sequence;
	let glyph_count = reader.u16().ok_or_else(bad)? as usize;
	// A sequence of zero glyphs would DELETE the glyph, which the format forbids in as many words -
	// and a shaper that allowed it would lose a cluster nothing could put a caret in.
	if glyph_count == 0 {
		return Err(bad());
	}
	let glyphs = sequence.slice(2, glyph_count.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
	let mut replacement = alloc::vec::Vec::with_capacity(glyph_count);
	for index in 0..glyph_count {
		replacement.push(glyphs.u16_at(index).ok_or_else(bad)?);
	}
	buffer.decompose(at, &replacement);
	Ok(true)
}

/// Type 3: one glyph for one of several, chosen by the caller.
///
/// THE FIRST ALTERNATE IS TAKEN, and that is a decision rather than an oversight: choosing among
/// alternates is a user-facing feature - a stylistic set, a swash - and until something above this
/// layer can express which one was asked for, taking the first is the answer the format itself gives
/// for `aalt` applied with no index.
fn alternate(subtable: Reader<'_>, buffer: &mut Buffer, at: usize) -> Result<bool, Error> {
	let mut reader = subtable;
	let format = reader.u16().ok_or_else(bad)?;
	if format != 1 {
		return Err(Error::Unsupported(Unsupported::SubtableFormat { table: *b"GSUB", format }));
	}
	let coverage_offset = reader.u16().ok_or_else(bad)? as usize;
	let count = reader.u16().ok_or_else(bad)? as usize;
	let coverage = subtable.slice(coverage_offset, subtable.len().checked_sub(coverage_offset).ok_or_else(bad)?).ok_or_else(bad)?;
	let Some(index) = coverage_index(coverage, buffer.infos[at].glyph)? else { return Ok(false) };
	if index as usize >= count {
		return Err(bad());
	}
	let sets = subtable.slice(6, count.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
	let offset = sets.u16_at(index as usize).ok_or_else(bad)? as usize;
	let set = subtable.slice(offset, subtable.len().checked_sub(offset).ok_or_else(bad)?).ok_or_else(bad)?;
	let mut reader = set;
	let alternates = reader.u16().ok_or_else(bad)? as usize;
	if alternates == 0 {
		return Ok(false);
	}
	let glyphs = set.slice(2, alternates.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
	buffer.substitute(at, glyphs.u16_at(0).ok_or_else(bad)?);
	Ok(true)
}

/// Type 4: several glyphs become one - a ligature.
fn ligature(subtable: Reader<'_>, buffer: &mut Buffer, at: usize) -> Result<bool, Error> {
	let mut reader = subtable;
	let format = reader.u16().ok_or_else(bad)?;
	if format != 1 {
		return Err(Error::Unsupported(Unsupported::SubtableFormat { table: *b"GSUB", format }));
	}
	let coverage_offset = reader.u16().ok_or_else(bad)? as usize;
	let count = reader.u16().ok_or_else(bad)? as usize;
	let coverage = subtable.slice(coverage_offset, subtable.len().checked_sub(coverage_offset).ok_or_else(bad)?).ok_or_else(bad)?;
	let Some(index) = coverage_index(coverage, buffer.infos[at].glyph)? else { return Ok(false) };
	if index as usize >= count {
		return Err(bad());
	}
	let sets = subtable.slice(6, count.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
	let set_offset = sets.u16_at(index as usize).ok_or_else(bad)? as usize;
	let set = subtable.slice(set_offset, subtable.len().checked_sub(set_offset).ok_or_else(bad)?).ok_or_else(bad)?;
	let mut reader = set;
	let ligatures = reader.u16().ok_or_else(bad)? as usize;
	for index in 0..ligatures {
		let offset = set.u16_at(1 + index).ok_or_else(bad)? as usize;
		let table = set.slice(offset, set.len().checked_sub(offset).ok_or_else(bad)?).ok_or_else(bad)?;
		let mut reader = table;
		let glyph = reader.u16().ok_or_else(bad)?;
		let components = reader.u16().ok_or_else(bad)? as usize;
		// The count INCLUDES the first glyph, which is already matched by the coverage - so a
		// two-glyph ligature lists one component, and reading it as two matches one glyph too many.
		if components == 0 {
			return Err(bad());
		}
		let tail = components - 1;
		if at + tail >= buffer.len() + 1 && tail > 0 && at + tail > buffer.len() - 1 {
			continue;
		}
		let mut matched = true;
		for component in 0..tail {
			let expected = table.u16_at(2 + component).ok_or_else(bad)?;
			match buffer.infos.get(at + 1 + component) {
				Some(info) if info.glyph == expected => {}
				_ => {
					matched = false;
					break;
				}
			}
		}
		if matched {
			buffer.ligate(at, components, glyph);
			return Ok(true);
		}
	}
	Ok(false)
}

/// Type 8: reverse chaining single substitution.
///
/// THE ONE LOOKUP APPLIED RIGHT TO LEFT, which is what "reverse" means and why it cannot share the
/// contextual machinery: the whole point is that a substitution made at one position is visible to
/// the rule applied at the position BEFORE it, which only holds if the buffer is walked backwards.
/// It is what an Arabic font uses to pick a final form after everything else has settled.
pub fn reverse_chain(subtable: Reader<'_>, buffer: &mut Buffer, at: usize) -> Result<bool, Error> {
	let mut reader = subtable;
	let format = reader.u16().ok_or_else(bad)?;
	if format != 1 {
		return Err(Error::Unsupported(Unsupported::SubtableFormat { table: *b"GSUB", format }));
	}
	let coverage_at = reader.u16().ok_or_else(bad)? as usize;
	let coverage = subtable.slice(coverage_at, subtable.len().checked_sub(coverage_at).ok_or_else(bad)?).ok_or_else(bad)?;
	let Some(index) = coverage_index(coverage, buffer.infos[at].glyph)? else { return Ok(false) };
	let backtrack_count = reader.u16().ok_or_else(bad)? as usize;
	let backtrack_at = reader.position();
	reader.skip(backtrack_count.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
	let lookahead_count = reader.u16().ok_or_else(bad)? as usize;
	let lookahead_at = reader.position();
	reader.skip(lookahead_count.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
	let glyph_count = reader.u16().ok_or_else(bad)? as usize;
	let glyphs_at = reader.position();
	if index as usize >= glyph_count {
		return Err(bad());
	}
	// The backtrack is stored NEAREST-FIRST here as everywhere else.
	for offset in 0..backtrack_count {
		let Some(position) = at.checked_sub(offset + 1) else { return Ok(false) };
		let at_coverage = subtable.u16_at(backtrack_at / 2 + offset).ok_or_else(bad)? as usize;
		let table = subtable.slice(at_coverage, subtable.len().checked_sub(at_coverage).ok_or_else(bad)?).ok_or_else(bad)?;
		let Some(info) = buffer.infos.get(position) else { return Ok(false) };
		if coverage_index(table, info.glyph)?.is_none() {
			return Ok(false);
		}
	}
	for offset in 0..lookahead_count {
		let at_coverage = subtable.u16_at(lookahead_at / 2 + offset).ok_or_else(bad)? as usize;
		let table = subtable.slice(at_coverage, subtable.len().checked_sub(at_coverage).ok_or_else(bad)?).ok_or_else(bad)?;
		let Some(info) = buffer.infos.get(at + offset + 1) else { return Ok(false) };
		if coverage_index(table, info.glyph)?.is_none() {
			return Ok(false);
		}
	}
	let replacement = subtable.u16_at(glyphs_at / 2 + index as usize).ok_or_else(bad)?;
	buffer.substitute(at, replacement);
	Ok(true)
}
