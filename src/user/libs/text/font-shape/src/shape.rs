//! The driver: from a run of glyphs to a shaped run.
//!
//! LOOKUPS ARE APPLIED IN LOOKUP-LIST ORDER, NOT FEATURE ORDER, and that is the rule an
//! implementation written from the feature list gets wrong. A font's features name lookups by index,
//! several features often name the same one, and the format says the LOOKUP LIST decides the order
//! they run in. Applying them feature by feature gives a different result for any font whose
//! features overlap - which is most of them - and the difference is a ligature that forms before the
//! substitution that was supposed to prevent it.
//!
//! THE ADVANCES COME FIRST, from `hmtx`, before any positioning rule runs. A `GPOS` value record
//! ADJUSTS an advance rather than setting one, so a shaper that positioned before it had widths
//! would kern correctly around zero.

use alloc::vec::Vec;
use font_parse::{Error, Face};

use crate::buffer::Buffer;
use crate::layout::{LayoutTable, class_of};
use crate::{Table, context, gpos, gsub};

/// How many lookups one shaping pass will apply. A font that asks for more is refused rather than
/// run: the bound is what keeps a crafted font from turning one paragraph into unbounded work.
pub const MAX_LOOKUPS: usize = opentype_profile::limits::FEATURES as usize;

/// Shape a buffer with a face.
///
/// `features` are the tags to turn on beyond what the language system requires. A shaper above this
/// decides which those are per script; this applies what it is given and the required one always.
pub fn shape(face: &Face<'_>, buffer: &mut Buffer, script: [u8; 4], language: [u8; 4], features: &[[u8; 4]]) -> Result<(), Error> {
	let masked: Vec<([u8; 4], u32)> = features.iter().map(|tag| (*tag, crate::buffer::GLOBAL)).collect();
	shape_with_masks(face, buffer, script, language, &masked)
}

/// The same, with each feature carrying the mask bit that says WHERE it applies.
///
/// THIS IS WHAT MAKES A SCRIPT SHAPER POSSIBLE. `fina` is not a global feature: it selects the final
/// form of a letter and must apply to the last letter of a word and to no other. A shaper that could
/// only turn features on globally would render every Arabic letter in its final form - a word made
/// entirely of endings - which is why the per-glyph mask exists and why the script shapers set it
/// before this runs.
pub fn shape_with_masks(face: &Face<'_>, buffer: &mut Buffer, script: [u8; 4], language: [u8; 4], features: &[([u8; 4], u32)]) -> Result<(), Error> {
	// EACH SELECTED FEATURE CONTRIBUTES LOOKUPS AND EACH LOOKUP WALKS THE BUFFER, so the feature
	// count is work per run rather than a declaration.
	if features.len() > opentype_profile::limits::FEATURES as usize {
		return Err(Error::Unsupported(opentype_profile::Unsupported::Exceeded { limit: "features", ceiling: opentype_profile::limits::FEATURES, asked: features.len() as u64 }));
	}
	// THE INPUT CEILING IS ABSOLUTE AND THE EXPANSION RULE IS PROPORTIONAL, and both are checked:
	// a ratio against an unbounded input is not a bound, and an input cap alone still admits a run a
	// pathological face expands sixty-four fold.
	let input = buffer.len();
	if input > opentype_profile::limits::RUN_INPUT as usize {
		return Err(Error::Unsupported(opentype_profile::Unsupported::Exceeded { limit: "run input", ceiling: opentype_profile::limits::RUN_INPUT, asked: input as u64 }));
	}
	classify(face, buffer)?;
	substitute(face, buffer, script, language, features)?;
	// THE CLASSES AND THE ADVANCES ARE BOTH READ AFTER SUBSTITUTION, and both for the same reason:
	// the buffer now holds different glyphs. A ligature has its own advance rather than the sum of
	// its components', and its own `GDEF` class - positioning it as though it were still its first
	// component gives it that component's width and attaches every mark in the word to the wrong
	// place.
	// THE OUTPUT CEILINGS, READ WHERE THE OUTPUT IS FINISHED GROWING. Substitution is the only stage
	// that changes the glyph count; positioning moves what is there.
	let output = buffer.len();
	if output > opentype_profile::limits::RUN_OUTPUT as usize {
		return Err(Error::Unsupported(opentype_profile::Unsupported::Exceeded { limit: "run output", ceiling: opentype_profile::limits::RUN_OUTPUT, asked: output as u64 }));
	}
	if output > input.saturating_mul(opentype_profile::limits::OUTPUT_EXPANSION as usize) {
		return Err(Error::Unsupported(opentype_profile::Unsupported::Exceeded { limit: "output expansion", ceiling: opentype_profile::limits::OUTPUT_EXPANSION, asked: (output / input.max(1)) as u64 }));
	}
	classify(face, buffer)?;
	advances(face, buffer)?;
	position(face, buffer, script, language, features)
}

/// Give every glyph its `GDEF` class. A font with no `GDEF` leaves them all zero, which is not an
/// error - it is a font that says nothing about which of its glyphs are marks.
fn classify(face: &Face<'_>, buffer: &mut Buffer) -> Result<(), Error> {
	let Some(gdef) = face.table(b"GDEF")? else { return Ok(()) };
	let mut reader = gdef;
	let _major = reader.u16();
	let _minor = reader.u16();
	let Some(offset) = reader.u16().map(|offset| offset as usize) else { return Ok(()) };
	if offset == 0 {
		return Ok(());
	}
	let Some(classes) = gdef.slice(offset, gdef.len().saturating_sub(offset)) else { return Ok(()) };
	for info in buffer.infos.iter_mut() {
		info.class = class_of(classes, info.glyph)?;
	}
	Ok(())
}

/// The advance of every glyph, before anything adjusts it.
fn advances(face: &Face<'_>, buffer: &mut Buffer) -> Result<(), Error> {
	for (index, info) in buffer.infos.iter().enumerate() {
		let (advance, _bearing) = face.advance(info.glyph)?;
		buffer.positions[index].x_advance = advance as i32;
	}
	Ok(())
}

fn substitute(face: &Face<'_>, buffer: &mut Buffer, script: [u8; 4], language: [u8; 4], features: &[([u8; 4], u32)]) -> Result<(), Error> {
	let Some(table) = face.table(b"GSUB")? else { return Ok(()) };
	let table = LayoutTable::new(table, *b"GSUB")?;
	for (index, mask) in selected_lookups(&table, script, language, features)? {
		apply_lookup(Table::Substitution, &table, index, buffer, mask)?;
	}
	Ok(())
}

fn position(face: &Face<'_>, buffer: &mut Buffer, script: [u8; 4], language: [u8; 4], features: &[([u8; 4], u32)]) -> Result<(), Error> {
	let Some(table) = face.table(b"GPOS")? else { return Ok(()) };
	let table = LayoutTable::new(table, *b"GPOS")?;
	for (index, mask) in selected_lookups(&table, script, language, features)? {
		apply_lookup(Table::Positioning, &table, index, buffer, mask)?;
	}
	Ok(())
}

/// The lookups a script, a language and a set of feature tags select - IN LOOKUP-LIST ORDER, with
/// each applied once however many features name it.
fn selected_lookups(table: &LayoutTable<'_>, script: [u8; 4], language: [u8; 4], features: &[([u8; 4], u32)]) -> Result<Vec<(u16, u32)>, Error> {
	let indices = table.features_for(script, language)?;
	let mut lookups: Vec<(u16, u32)> = Vec::new();
	// A LOOKUP NAMED BY TWO FEATURES TAKES BOTH THEIR MASKS, rather than appearing twice: applying
	// it twice would substitute twice, and taking only the first mask would silently drop the second
	// feature's positions.
	let add = |lookup: u16, mask: u32, lookups: &mut Vec<(u16, u32)>| -> Result<(), Error> {
		if let Some(entry) = lookups.iter_mut().find(|(index, _)| *index == lookup) {
			entry.1 |= mask;
		} else if lookups.len() < MAX_LOOKUPS {
			lookups.push((lookup, mask));
		} else {
			// REFUSED RATHER THAN DROPPED. A lookup silently left out is a run shaped as though the
			// font had not asked for it, which is a document rendered wrong with nothing to say so.
			return Err(Error::Unsupported(opentype_profile::Unsupported::Exceeded { limit: "features", ceiling: opentype_profile::limits::FEATURES, asked: lookups.len() as u64 + 1 }));
		}
		Ok(())
	};
	if let Some(required) = indices.required {
		let feature = table.feature(required)?;
		for index in 0..feature.len() {
			let Some(lookup) = feature.lookup_index(index) else { continue };
			add(lookup, crate::buffer::GLOBAL, &mut lookups)?;
		}
	}
	for index in 0..indices.len() {
		let Some(feature_index) = indices.get(index) else { continue };
		let feature = table.feature(feature_index)?;
		let Some((_, mask)) = features.iter().find(|(tag, _)| *tag == feature.tag) else { continue };
		for lookup_at in 0..feature.len() {
			let Some(lookup) = feature.lookup_index(lookup_at) else { continue };
			add(lookup, *mask, &mut lookups)?;
		}
	}
	// THE ORDER IS THE LOOKUP LIST'S, which is what the format says and what a feature-ordered walk
	// gets wrong for any font whose features share a lookup.
	lookups.sort_unstable_by_key(|(index, _)| *index);
	Ok(lookups)
}

/// Apply one lookup across the whole buffer.
///
/// LEFT TO RIGHT, EXCEPT FOR THE ONE THAT IS NOT. Reverse chaining substitution is applied right to
/// left, and that is not a detail: the whole point of it is that a substitution made at one position
/// is visible to the rule applied at the position before it, which only holds walking backwards.
pub fn apply_lookup(table: Table, layout: &LayoutTable<'_>, index: u16, buffer: &mut Buffer, mask: u32) -> Result<bool, Error> {
	let lookup = layout.lookup(index)?;
	let reverse = table == Table::Substitution && lookup.kind == 8;
	let mut changed = false;
	if reverse {
		let mut at = buffer.len();
		while at > 0 {
			at -= 1;
			if buffer.infos[at].mask & mask != 0 && apply_lookup_at(table, layout, index, buffer, at, 0)? {
				changed = true;
			}
		}
		return Ok(changed);
	}
	let mut at = 0usize;
	while at < buffer.len() {
		if buffer.infos[at].mask & mask != 0 && apply_lookup_at(table, layout, index, buffer, at, 0)? {
			changed = true;
		}
		at += 1;
	}
	Ok(changed)
}

/// Apply one lookup at one position, resolving extensions and bounding the nesting.
pub fn apply_lookup_at(table: Table, layout: &LayoutTable<'_>, index: u16, buffer: &mut Buffer, at: usize, depth: u8) -> Result<bool, Error> {
	if at >= buffer.len() {
		return Ok(false);
	}
	let lookup = layout.lookup(index)?;
	// THE FLAGS DECIDE WHETHER THIS GLYPH PARTICIPATES AT ALL, and they are read here rather than in
	// each subtable: a lookup that says it ignores marks and is applied to one attaches an accent to
	// an accent.
	if lookup.skips(buffer.infos[at].class) {
		return Ok(false);
	}
	for subtable_index in 0..lookup.subtable_count() {
		let Some(subtable) = lookup.subtable(subtable_index) else { continue };
		let (kind, subtable) = unwrap_extension(table, lookup.kind, subtable)?;
		let contextual = match table {
			Table::Substitution => matches!(kind, 5 | 6),
			Table::Positioning => matches!(kind, 7 | 8),
		};
		let fired = if contextual {
			context::apply(table, kind, subtable, buffer, at, layout, depth)?
		} else {
			match table {
				Table::Substitution => gsub::apply_subtable(kind, subtable, buffer, at)?,
				Table::Positioning => gpos::apply_subtable(kind, subtable, buffer, at)?,
			}
		};
		if fired {
			return Ok(true);
		}
	}
	Ok(false)
}

/// An EXTENSION lookup is a wrapper whose only content is the real type and a 32-bit offset to it -
/// how a font larger than 64 kB addresses its own subtables. Unwrapped once, here, rather than in
/// every type that might be wrapped.
fn unwrap_extension<'a>(table: Table, kind: u16, subtable: font_parse::Reader<'a>) -> Result<(u16, font_parse::Reader<'a>), Error> {
	let extension = match table {
		Table::Substitution => 7,
		Table::Positioning => 9,
	};
	if kind != extension {
		return Ok((kind, subtable));
	}
	let bad = || Error::Malformed(font_parse::Malformed::InconsistentTable { table: table.tag() });
	let mut reader = subtable;
	let format = reader.u16().ok_or_else(bad)?;
	if format != 1 {
		return Err(Error::Unsupported(opentype_profile::Unsupported::SubtableFormat { table: table.tag(), format }));
	}
	let real = reader.u16().ok_or_else(bad)?;
	let offset = reader.u32().ok_or_else(bad)? as usize;
	// An extension that extends an extension is a font pointing at itself.
	if real == extension {
		return Err(bad());
	}
	let inner = subtable.slice(offset, subtable.len().checked_sub(offset).ok_or_else(bad)?).ok_or_else(bad)?;
	Ok((real, inner))
}

/// Shape a run of CHARACTERS: pick the shaper the script needs, run its pass, and apply its features.
///
/// THIS IS THE ENTRY POINT THAT MAKES THE REST TRUE. Everything above it takes a buffer of glyphs and
/// a list of feature tags, which is enough for Latin and enough for nothing else: the joining forms,
/// the syllable reordering and the jamo composition all have to happen BEFORE any lookup runs, and
/// they all need the characters rather than the glyphs. A caller that went straight to
/// `shape_with_masks` would be doing what "Latin alone" means.
///
/// The buffer must already hold one glyph per character, in the run's order - which is what the
/// caller's `cmap` mapping produced. What comes back is the characters AFTER the shaper's own pass,
/// because Hangul composition changes them.
pub fn shape_run(face: &Face<'_>, buffer: &mut Buffer, characters: &[char], language: [u8; 4]) -> Result<Vec<char>, Error> {
	use crate::scripts::{self, Shaper};

	// REFUSED BEFORE THE SHAPER'S OWN PASS RUNS, rather than after: the reordering below copies and
	// rewrites the whole run, and doing that work in order to discover it was too long is the shape
	// of exhaustion this ceiling exists to prevent.
	if characters.len() > opentype_profile::limits::RUN_INPUT as usize {
		return Err(Error::Unsupported(opentype_profile::Unsupported::Exceeded { limit: "run input", ceiling: opentype_profile::limits::RUN_INPUT, asked: characters.len() as u64 }));
	}
	let shaper = scripts::shaper_for(characters);
	let mut characters = characters.to_vec();
	// THE SHAPER'S OWN PASS, before any lookup: this is the part that is not a feature.
	match shaper {
		Shaper::Default => {}
		Shaper::Cursive => scripts::apply_cursive_masks(buffer, &characters),
		Shaper::Indic => scripts::reorder_indic(&characters, buffer),
		Shaper::Khmer => scripts::reorder_khmer(&characters, buffer),
		Shaper::Myanmar => scripts::reorder_myanmar(&characters, buffer),
		Shaper::Hangul => characters = scripts::compose_hangul(&characters, buffer),
		Shaper::Universal => {}
	}
	// The script tag the font's own tables are indexed by.
	let script = scripts::script_tag(&characters);
	// And the features that shaper turns on, each with the mask that says where.
	let features: Vec<([u8; 4], u32)> = match shaper {
		Shaper::Default | Shaper::Universal => {
			let mut features: Vec<([u8; 4], u32)> = scripts::universal_features().to_vec();
			// The two every run gets: standard ligatures and contextual alternates.
			features.push((*b"liga", crate::buffer::GLOBAL));
			features.push((*b"calt", crate::buffer::GLOBAL));
			features.push((*b"kern", crate::buffer::GLOBAL));
			features.push((*b"mark", crate::buffer::GLOBAL));
			features.push((*b"mkmk", crate::buffer::GLOBAL));
			features
		}
		Shaper::Cursive => {
			let mut features: Vec<([u8; 4], u32)> = scripts::cursive_features().to_vec();
			features.push((*b"curs", crate::buffer::GLOBAL));
			features.push((*b"mark", crate::buffer::GLOBAL));
			features.push((*b"mkmk", crate::buffer::GLOBAL));
			features
		}
		Shaper::Indic => with_marks(scripts::indic_features().to_vec()),
		Shaper::Khmer => with_marks(scripts::khmer_features().to_vec()),
		Shaper::Myanmar => with_marks(scripts::myanmar_features().to_vec()),
		Shaper::Hangul => with_marks(scripts::hangul_features().to_vec()),
	};
	shape_with_masks(face, buffer, script, language, &features)?;
	Ok(characters)
}

/// THE MARK FEATURES ARE ADDED TO EVERY SCRIPT'S SET, because every script that has marks needs them
/// and no script's own list carries them: `mark` and `mkmk` are what attach an accent to its base
/// and to another accent, and a shaper that left them out of one script's list renders that script
/// with its marks at the pen.
fn with_marks(mut features: Vec<([u8; 4], u32)>) -> Vec<([u8; 4], u32)> {
	features.push((*b"kern", crate::buffer::GLOBAL));
	features.push((*b"mark", crate::buffer::GLOBAL));
	features.push((*b"mkmk", crate::buffer::GLOBAL));
	features
}
