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

use crate::buffer::{Buffer, GLOBAL, RTLM};
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
	let mut masked: Vec<([u8; 4], u32)> = Vec::new();
	masked.try_reserve_exact(features.len()).map_err(|_| Error::Allocation)?;
	masked.extend(features.iter().map(|tag| (*tag, GLOBAL)));
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
	shape_in_stages(face, buffer, script, language, &[features], &mut |_| Ok(()))
}

/// The same, with the substitution features applied in STAGES and a pass between them.
///
/// THE INDIC SHAPER IS WHY THERE ARE STAGES. A reph is formed by the basic features and then MOVED,
/// and the presentation features are written against the syllable after the move - so the two
/// groups cannot be one pass however their lookups are ordered. Within a stage the rule above holds:
/// lookup-list order, each lookup once. Positioning runs once, over every stage's features, after all
/// of them.
fn shape_in_stages(face: &Face<'_>, buffer: &mut Buffer, script: [u8; 4], language: [u8; 4], stages: &[&[([u8; 4], u32)]], between: &mut dyn FnMut(&mut Buffer) -> Result<(), Error>) -> Result<(), Error> {
	// EACH SELECTED FEATURE CONTRIBUTES LOOKUPS AND EACH LOOKUP WALKS THE BUFFER, so the feature
	// count is work per run rather than a declaration.
	let count: usize = stages.iter().map(|stage| stage.len()).sum();
	if count > opentype_profile::limits::FEATURES as usize {
		return Err(Error::Unsupported(opentype_profile::Unsupported::Exceeded { limit: "features", ceiling: opentype_profile::limits::FEATURES, asked: count as u64 }));
	}
	// THE INPUT CEILING IS ABSOLUTE AND THE EXPANSION RULE IS PROPORTIONAL, and both are checked:
	// a ratio against an unbounded input is not a bound, and an input cap alone still admits a run a
	// pathological face expands sixty-four fold.
	let input = buffer.len();
	if input > opentype_profile::limits::RUN_INPUT as usize {
		return Err(Error::Unsupported(opentype_profile::Unsupported::Exceeded { limit: "run input", ceiling: opentype_profile::limits::RUN_INPUT, asked: input as u64 }));
	}
	let substitution = match face.table(b"GSUB")? {
		Some(table) => Some(LayoutTable::new(table, *b"GSUB")?),
		None => None,
	};
	for (index, stage) in stages.iter().enumerate() {
		if index > 0 {
			between(buffer)?;
		}
		// THE CLASSES ARE READ BEFORE EVERY STAGE, because the stage before it changed the glyphs: a
		// reph formed from a consonant and a virama is a mark, and a lookup that skips marks has to
		// know that before it runs.
		classify(face, buffer)?;
		if let Some(table) = &substitution {
			// THE REQUIRED FEATURE BELONGS TO THE FIRST STAGE, which is where a face expects it.
			for (lookup, mask) in selected_lookups(table, script, language, stage, index == 0)? {
				apply_lookup(Table::Substitution, table, lookup, buffer, mask)?;
			}
		}
	}
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
	let mut features: Vec<([u8; 4], u32)> = Vec::new();
	features.try_reserve_exact(count).map_err(|_| Error::Allocation)?;
	for stage in stages {
		features.extend_from_slice(stage);
	}
	position(face, buffer, script, language, &features)
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

fn position(face: &Face<'_>, buffer: &mut Buffer, script: [u8; 4], language: [u8; 4], features: &[([u8; 4], u32)]) -> Result<(), Error> {
	let Some(table) = face.table(b"GPOS")? else { return Ok(()) };
	let table = LayoutTable::new(table, *b"GPOS")?;
	for (index, mask) in selected_lookups(&table, script, language, features, true)? {
		apply_lookup(Table::Positioning, &table, index, buffer, mask)?;
	}
	Ok(())
}

/// The lookups a script, a language and a set of feature tags select - IN LOOKUP-LIST ORDER, with
/// each applied once however many features name it. `required` says whether the language system's
/// required feature is among them, which it is everywhere but a stage after the first and a probe.
fn selected_lookups(table: &LayoutTable<'_>, script: [u8; 4], language: [u8; 4], features: &[([u8; 4], u32)], required: bool) -> Result<Vec<(u16, u32)>, Error> {
	let indices = table.features_for(script, language)?;
	let mut lookups: Vec<(u16, u32)> = Vec::new();
	// A LOOKUP NAMED BY TWO FEATURES TAKES BOTH THEIR MASKS, rather than appearing twice: applying
	// it twice would substitute twice, and taking only the first mask would silently drop the second
	// feature's positions.
	let add = |lookup: u16, mask: u32, lookups: &mut Vec<(u16, u32)>| -> Result<(), Error> {
		if let Some(entry) = lookups.iter_mut().find(|(index, _)| *index == lookup) {
			entry.1 |= mask;
		} else if lookups.len() < MAX_LOOKUPS {
			lookups.try_reserve(1).map_err(|_| Error::Allocation)?;
			lookups.push((lookup, mask));
		} else {
			// REFUSED RATHER THAN DROPPED. A lookup silently left out is a run shaped as though the
			// font had not asked for it, which is a document rendered wrong with nothing to say so.
			return Err(Error::Unsupported(opentype_profile::Unsupported::Exceeded { limit: "features", ceiling: opentype_profile::limits::FEATURES, asked: lookups.len() as u64 + 1 }));
		}
		Ok(())
	};
	if let Some(feature_index) = indices.required.filter(|_| required) {
		let feature = table.feature(feature_index)?;
		for index in 0..feature.len() {
			let Some(lookup) = feature.lookup_index(index) else { continue };
			add(lookup, GLOBAL, &mut lookups)?;
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
///
/// THE LANGUAGE SELECTS THE FACE'S LANGUAGE SYSTEM, and `locl` is what applies what that system says:
/// Turkish dotless i, Romanian comma-below, the Serbian italic forms. A run in the face's `dflt`
/// language and one in a language it names are shaped differently by design.
///
/// A RIGHT-TO-LEFT RUN'S MIRRORED FORMS are the caller's half and this one's together: the caller maps
/// a mirrored character to its mirror when the face has it, and marks every other glyph of the run
/// with `RTLM` - so the face's own `rtlm` forms apply exactly where no mirror character did.
pub fn shape_run(face: &Face<'_>, buffer: &mut Buffer, characters: &[char], language: [u8; 4]) -> Result<Vec<char>, Error> {
	use crate::scripts::{self, Shaper};

	// REFUSED BEFORE THE SHAPER'S OWN PASS RUNS, rather than after: the reordering below copies and
	// rewrites the whole run, and doing that work in order to discover it was too long is the shape
	// of exhaustion this ceiling exists to prevent.
	if characters.len() > opentype_profile::limits::RUN_INPUT as usize {
		return Err(Error::Unsupported(opentype_profile::Unsupported::Exceeded { limit: "run input", ceiling: opentype_profile::limits::RUN_INPUT, asked: characters.len() as u64 }));
	}
	// THE SCRIPT TAG THE FONT'S OWN TABLES ARE INDEXED BY - and the refusal of a script the profile
	// does not name, before any of its text is shaped by an engine written for another.
	let script = scripts::profile_script(characters)?;
	let shaper = scripts::shaper_for(characters);
	let mut copied: Vec<char> = Vec::new();
	copied.try_reserve_exact(characters.len()).map_err(|_| Error::Allocation)?;
	copied.extend_from_slice(characters);
	let mut characters = copied;
	// THE INDIC SYLLABLE PLAN, which the reordering below already reads: where each reph is, which
	// consonant is each syllable's base, and the masks that keep each positional feature inside the
	// part of the syllable it belongs to.
	let indic = if shaper == Shaper::Indic { crate::indic::plan(face, buffer, &characters, script, language)? } else { None };
	// THE SHAPER'S OWN PASS, before any lookup: this is the part that is not a feature.
	match shaper {
		Shaper::Default | Shaper::Universal => Ok(()),
		Shaper::Cursive => scripts::apply_cursive_masks(buffer, &characters),
		Shaper::Indic => scripts::reorder_indic(&characters, buffer),
		Shaper::Khmer => scripts::reorder_khmer(&characters, buffer),
		Shaper::Myanmar => scripts::reorder_myanmar(&characters, buffer),
		Shaper::Hangul => {
			characters = scripts::compose_hangul(&characters, buffer)?;
			Ok(())
		}
	}?;
	// And the features that shaper turns on, each with the mask that says where.
	let mut features: Vec<([u8; 4], u32)> = Vec::new();
	features.try_reserve_exact(32).map_err(|_| Error::Allocation)?;
	match shaper {
		Shaper::Default | Shaper::Universal => {
			features.extend_from_slice(&scripts::universal_features());
			// The two every run gets: standard ligatures and contextual alternates.
			features.extend_from_slice(&[(*b"liga", GLOBAL), (*b"calt", GLOBAL), (*b"kern", GLOBAL), (*b"mark", GLOBAL), (*b"mkmk", GLOBAL)]);
		}
		Shaper::Cursive => {
			features.extend_from_slice(&scripts::cursive_features());
			// KERNING IS NOT A LATIN FEATURE. An Arabic face kerns its joined forms like any other
			// face kerns its letters, and a cursive set without `kern` draws every pair the face
			// adjusts at its default distance.
			features.extend_from_slice(&[(*b"curs", GLOBAL), (*b"kern", GLOBAL), (*b"mark", GLOBAL), (*b"mkmk", GLOBAL)]);
		}
		Shaper::Indic => with_marks(&mut features, &crate::indic::basic_features()),
		Shaper::Khmer => with_marks(&mut features, &scripts::khmer_features()),
		Shaper::Myanmar => with_marks(&mut features, &scripts::myanmar_features()),
		Shaper::Hangul => with_marks(&mut features, &scripts::hangul_features()),
	}
	// `ccmp` BELONGS TO EVERY SCRIPT AND TO EVERY SHAPER, which is why it is added here rather than
	// in any one of the lists above. It is how a face says that a precomposed character is DRAWN as
	// two glyphs, or that two characters are drawn as one, and it is the feature a font expects to
	// have applied before anything else looks at the run: a shaper that left it out renders Thai's
	// sara am as a glyph the face never meant to be used alone, and every face that composes a
	// letter with its accent as one glyph loses the composition.
	//
	// FIRST IN THE LIST, which is not what decides the order it runs in - the LOOKUP LIST decides
	// that - but is what a reader of this function expects to find, and costs nothing to state.
	features.insert(0, (*b"ccmp", crate::buffer::GLOBAL));
	// `locl` TOO, and for the same reason: the profile's languages are defined by what they change -
	// Turkish dotless i, Romanian comma-below, the Serbian italic forms - and every one of those is a
	// `locl` lookup under the language system the caller selected. A shaper that selected the system
	// and never applied `locl` would render all of them as the default language.
	features.insert(1, (*b"locl", GLOBAL));
	// And `rtlm`, which reaches only the glyphs the caller marked: the right-to-left ones it did not
	// map to a mirror character.
	features.push((*b"rtlm", RTLM));
	match indic {
		// THE INDIC SHAPER IN TWO STAGES, with the final reordering between them: the reph the basic
		// features formed is moved before the presentation features see the syllable.
		Some(config) => shape_in_stages(face, buffer, script, language, &[&features, &crate::indic::presentation_features()], &mut |buffer| crate::indic::final_reorder(buffer, &config))?,
		None => shape_with_masks(face, buffer, script, language, &features)?,
	}
	Ok(characters)
}

/// THE MARK FEATURES ARE ADDED TO EVERY SCRIPT'S SET, because every script that has marks needs them
/// and no script's own list carries them: `mark` and `mkmk` are what attach an accent to its base
/// and to another accent, and a shaper that left them out of one script's list renders that script
/// with its marks at the pen. The syllable-based shapers take the Indic positioning features too -
/// `abvm` and `blwm` place what is drawn above and below a syllable's base, `dist` its distances -
/// because their faces position a reph, a below-base form and a vowel sign through them rather than
/// through `mark`.
fn with_marks(features: &mut Vec<([u8; 4], u32)>, own: &[([u8; 4], u32)]) {
	features.extend_from_slice(own);
	features.extend_from_slice(&[(*b"kern", GLOBAL), (*b"mark", GLOBAL), (*b"mkmk", GLOBAL), (*b"abvm", GLOBAL), (*b"blwm", GLOBAL), (*b"dist", GLOBAL)]);
}

/// What a face's substitution lookups would do to a short sequence, asked BEFORE shaping.
///
/// HOW THE INDIC PLAN LEARNS WHAT THE FACE CAN FORM. Whether `र` after a virama is a consonant of its
/// own or a form below the base, and whether `Ra, virama` becomes a reph at all, is the face's answer
/// and not the script's - so the plan asks the face's own lookups about the exact pair, the way the
/// specification describes, rather than keeping a list of its own.
pub(crate) struct Probe<'a> {
	table: Option<LayoutTable<'a>>,
	script: [u8; 4],
	language: [u8; 4],
	scratch: Buffer,
	/// The lookups each feature asked about selects, found once: a plan asks the same few features
	/// about every consonant of a run.
	selected: Vec<([u8; 4], Vec<(u16, u32)>)>,
}

impl<'a> Probe<'a> {
	pub(crate) fn new(face: &Face<'a>, script: [u8; 4], language: [u8; 4]) -> Result<Self, Error> {
		let table = match face.table(b"GSUB")? {
			Some(table) => Some(LayoutTable::new(table, *b"GSUB")?),
			None => None,
		};
		let mut scratch = Buffer::new();
		scratch.reserve(4)?;
		Ok(Self { table, script, language, scratch, selected: Vec::new() })
	}

	/// Would the lookups `feature` selects substitute ALL of `glyphs`, applied where they begin?
	///
	/// THE WHOLE SEQUENCE, not its first glyph: a lookup that rewrote the virama alone has not formed
	/// anything from the pair, so it is a ligature that consumed them or a rule that rewrote a glyph
	/// past the first that counts.
	pub(crate) fn would_substitute(&mut self, feature: [u8; 4], glyphs: &[u16]) -> Result<bool, Error> {
		let Some(table) = &self.table else { return Ok(false) };
		let found = match self.selected.iter().position(|(tag, _)| *tag == feature) {
			Some(found) => found,
			None => {
				let lookups = selected_lookups(table, self.script, self.language, &[(feature, GLOBAL)], false)?;
				self.selected.try_reserve(1).map_err(|_| Error::Allocation)?;
				self.selected.push((feature, lookups));
				self.selected.len() - 1
			}
		};
		for (lookup, _) in &self.selected[found].1 {
			let lookup = *lookup;
			self.scratch.infos.clear();
			self.scratch.positions.clear();
			for glyph in glyphs {
				self.scratch.push(*glyph, 0)?;
			}
			if apply_lookup_at(Table::Substitution, table, lookup, &mut self.scratch, 0, 0)? {
				let consumed = self.scratch.len() < glyphs.len();
				let rewritten = self.scratch.infos.iter().zip(glyphs).skip(1).any(|(info, glyph)| info.glyph != *glyph);
				if consumed || rewritten {
					return Ok(true);
				}
			}
		}
		Ok(false)
	}
}
