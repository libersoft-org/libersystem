//! The stages, as types. Each exists only as the output of the one before it.
//!
//! THIS IS THE ORDER, EXPRESSED SO IT CANNOT BE RUN WRONGLY. A pipeline written as a list of
//! functions over one mutable buffer is a pipeline whose order is a convention; here the input of
//! each stage is a type nothing else produces, so "reorder before wrapping" is not a bug that can be
//! written - `Visual` is reachable from `Reshaped`, `Reshaped` from `Lines`, and from nowhere else.

use alloc::vec::Vec;
use font_contract::{Direction, Fixed266};
use font_parse::Face;
use font_shape::Buffer;
use unicode_bidi::{Levels, ParagraphDirection};
use unicode_segmentation::{LineBreakOpportunity, grapheme_boundaries, line_break_opportunities};
use unicode_tables::{GeneralCategory, Script, general_category, script};

use crate::canonical::{CanonicalView, Coverage};
use crate::fallback::{Policy, face_for_cluster};
use crate::{Error, MAX_PARAGRAPH, room, storage};

/// The language a stretch of text is in when the caller states none: OpenType's `dflt`, the
/// script's own forms with no language-specific substitution.
pub const DEFAULT_LANGUAGE: [u8; 4] = *b"dflt";

/// A stretch of the paragraph in one stated language.
///
/// THE LANGUAGE IS AN INPUT AND NEVER AMBIENT STATE. Turkish dotless i, Romanian comma-below, the
/// Serbian italic forms and a Japanese rather than a Chinese ideograph are all decided by it, and a
/// pipeline that read it from a process-wide locale would shape the same document differently on two
/// machines. The tag is the OpenType `LangSys` tag the profile names its languages by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LanguageSpan {
	/// Where the stretch begins: a byte offset into the text. It runs to the next span's start, or to
	/// the end.
	pub start: usize,
	pub language: [u8; 4],
}

/// STAGE 1: the caller's text, validated and never rewritten again, with the languages its
/// stretches are in.
///
/// THE BYTES ARE THE CALLER'S FOR THE WHOLE PIPELINE. Every offset any later stage reports is an
/// offset into THIS string - which is why it is carried through rather than copied out of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Source<'a> {
	text: &'a str,
	characters: usize,
	languages: &'a [LanguageSpan],
}

impl<'a> Source<'a> {
	/// The caller's text, all of it in `dflt` - which is what a caller states when it states no
	/// language.
	pub fn new(text: &'a str) -> Result<Self, Error> {
		Self::with_languages(text, &[])
	}

	/// The caller's text and the languages its stretches are in. Text before the first span is in
	/// `dflt`. A span that does not start on a character boundary, or does not start after the one
	/// before it, is refused rather than rounded: it is a statement about the text that is not true
	/// of it.
	pub fn with_languages(text: &'a str, languages: &'a [LanguageSpan]) -> Result<Self, Error> {
		let characters = text.chars().count();
		if characters > MAX_PARAGRAPH {
			return Err(Error::TooLong { characters, limit: MAX_PARAGRAPH });
		}
		let mut previous: Option<usize> = None;
		for span in languages {
			if span.start > text.len() || !text.is_char_boundary(span.start) || previous.is_some_and(|previous| span.start <= previous) {
				return Err(Error::Language { at: span.start });
			}
			previous = Some(span.start);
		}
		Ok(Self { text, characters, languages })
	}

	pub fn text(&self) -> &'a str {
		self.text
	}

	/// The language the character at a byte offset is in.
	pub fn language_at(&self, at: usize) -> [u8; 4] {
		let spans = self.languages.partition_point(|span| span.start <= at);
		if spans == 0 { DEFAULT_LANGUAGE } else { self.languages[spans - 1].language }
	}
}

/// One itemised run: a stretch of one script and one language, beginning and ending at cluster
/// boundaries.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Item {
	/// Byte offsets into the source.
	pub start: usize,
	pub end: usize,
	pub script: Script,
	/// The OpenType `LangSys` tag the caller stated for this stretch.
	pub language: [u8; 4],
}

/// STAGE 2: itemisation. Grapheme cluster boundaries, and the script and language runs between them.
///
/// THE CLUSTER BOUNDARIES COME FIRST AND NOTHING LATER MAY CROSS THEM. A script run that split a
/// cluster would hand half of a letter to one face and half to another, which is what
/// "cluster-atomically" means three stages further down.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Items<'a> {
	pub source: Source<'a>,
	/// The grapheme cluster boundaries, as byte offsets, including both ends.
	pub clusters: Vec<usize>,
	pub items: Vec<Item>,
	/// The storage this stage could not have, carried to the first stage that returns a result.
	refused: Option<Error>,
}

impl Items<'_> {
	/// The refusal this stage carried, if it ran out of storage.
	///
	/// CARRIED RATHER THAN RAISED, because stages 2 and 3 answer their output directly and cannot
	/// refuse: everything their INPUT can do wrong `Source` has already refused. The one thing left is
	/// storage, and the first stage that returns a result reports it - an empty item list is never
	/// mistaken for an empty paragraph.
	pub fn refused(&self) -> Result<(), Error> {
		match self.refused {
			Some(error) => Err(error),
			None => Ok(()),
		}
	}
}

/// THE SCRIPT RULE, WHICH IS THE `Script` PROPERTY AND NOT `Script_Extensions`.
///
/// A cluster's script is the script of its first character that DECIDES one. `Common`, `Inherited`
/// and `Unknown` decide nothing: such a cluster - a space, a digit, punctuation, a mark with nothing
/// before it, a private-use icon - joins the run it is in, and a stretch of them at the start of a
/// language run joins the first script after it, so `«שלום»` is one Hebrew run and not a quotation
/// mark shaped on its own.
///
/// `Script_Extensions` IS NOT READ, and the rule above is why it need not be. What the extensions
/// refine is which scripts a SHARED character belongs to - the danda in Devanagari and Bengali, the
/// prolonged sound mark in both kana, tatweel and the Arabic comma in every Arabic-script language -
/// and every such character is `Common` or `Inherited`, so it already takes the script of the text
/// around it, which is the answer its extensions give whenever that text is in one of them. The
/// remaining difference is a character with a script of its own used inside another script's text -
/// Arabic-Indic digits inside Thaana - which becomes a run of its own script here; among the scripts
/// the profile names that case is also a bidi level change, so the run would be split there anyway.
fn deciding(cluster: &str) -> Option<Script> {
	cluster.chars().map(script).find(|candidate| !matches!(candidate, Script::Common | Script::Inherited | Script::Unknown))
}

pub fn itemise<'a>(source: Source<'a>) -> Items<'a> {
	match itemised(source) {
		Ok(items) => items,
		Err(error) => Items { source, clusters: Vec::new(), items: Vec::new(), refused: Some(error) },
	}
}

fn itemised(source: Source<'_>) -> Result<Items<'_>, Error> {
	let text = source.text();
	// One boundary per character at most, and both ends.
	let mut clusters: Vec<usize> = storage(source.characters + 2)?;
	clusters.resize(source.characters + 2, 0);
	let count = grapheme_boundaries(text, &mut clusters);
	clusters.truncate(count);

	let mut items: Vec<Item> = Vec::new();
	for window in clusters.windows(2) {
		let (start, end) = (window[0], window[1]);
		let decided = deciding(&text[start..end]);
		let language = source.language_at(start);
		match items.last_mut() {
			// A cluster joins the run before it when the two are in one language and the cluster
			// decides nothing, decides the same script, or meets a run that has decided nothing yet.
			Some(last) if last.language == language && (decided.is_none() || decided == Some(last.script) || last.script == Script::Common) => {
				if let Some(found) = decided {
					last.script = found;
				}
				last.end = end;
			}
			_ => {
				room(&mut items)?;
				items.push(Item { start, end, script: decided.unwrap_or(Script::Common), language });
			}
		}
	}
	Ok(Items { source, clusters, items, refused: None })
}

/// STAGE 3: the bidi levels of the paragraph.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Levelled<'a> {
	pub items: Items<'a>,
	pub levels: Levels,
}

pub fn resolve_levels<'a>(items: Items<'a>, direction: ParagraphDirection) -> Levelled<'a> {
	let text = items.source.text();
	let levels = match items.refused {
		// The paragraph level is still answered - it needs no storage - so a caller that reads only
		// that is not told something false; the refusal itself waits for the first stage that can
		// report it.
		Some(_) => Levels { paragraph_level: unicode_bidi::paragraph_level(text, direction), levels: Vec::new(), classes: Vec::new() },
		None => unicode_bidi::levels(text, direction),
	};
	Levelled { items, levels }
}

/// STAGE 4: the face each cluster is drawn with, with its level and the mirror its level asks for.
///
/// FALLBACK IS CLUSTER-ATOMIC, which is the whole reason the cluster boundaries were computed two
/// stages ago: a face is chosen for a CLUSTER, never for a character, or an `e` and its accent are
/// drawn from two different fonts.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Faced<'a> {
	pub levelled: Levelled<'a>,
	/// One entry per cluster: the face the policy chose for it.
	pub faces: Vec<u16>,
	/// One entry per cluster: its bidi level, which is the level of its first character.
	pub levels: Vec<u8>,
	/// One entry per cluster: the character its first is drawn as, when its level is odd and that
	/// character mirrors. The shaping stage is where it is applied, because only that stage knows
	/// whether the face has the mirror.
	pub mirrored: Vec<Option<char>>,
}

/// Choose every cluster's face IN THE POLICY'S ORDER FOR ITS RUN'S SCRIPT AND LANGUAGE.
///
/// THE ORDER IS THE POLICY'S AND NEVER THE COVERAGE PROVIDER'S. The provider answers whether a face
/// covers a cluster; which faces are asked, and in which order, is the stated per-script and
/// per-language policy - an order taken from however the provider enumerated its faces would be the
/// accident of enumeration fallback exists to replace, and the same for every script in a paragraph.
pub fn choose_faces<'a>(levelled: Levelled<'a>, policy: &Policy, coverage: &impl Coverage) -> Result<Faced<'a>, Error> {
	levelled.items.refused()?;
	let text = levelled.items.source.text();
	let count = levelled.items.clusters.len().saturating_sub(1);
	let mut faces: Vec<u16> = storage(count)?;
	let mut levels: Vec<u8> = storage(count)?;
	let mut mirrored: Vec<Option<char>> = storage(count)?;
	let mut preferred: Vec<u16> = Vec::new();
	let mut cluster: Vec<char> = Vec::new();
	let mut item: Option<usize> = None;
	let mut character = 0usize;
	for window in levelled.items.clusters.windows(2) {
		let (start, end) = (window[0], window[1]);
		// THE ORDER IS ASKED FOR ONCE PER RUN, not once per cluster: it depends on the run's script
		// and language and on nothing in the cluster.
		let at = levelled.items.items.partition_point(|candidate| candidate.end <= start);
		if item != Some(at) {
			let run = levelled.items.items.get(at).ok_or(Error::NoFace { cluster: start as u32 })?;
			policy.preferred_into(run.script, run.language, &mut preferred)?;
			item = Some(at);
		}
		cluster.clear();
		for found in text[start..end].chars() {
			room(&mut cluster)?;
			cluster.push(found);
		}
		let face = face_for_cluster(&preferred, policy.last_resort(), coverage, &cluster).ok_or(Error::NoFace { cluster: start as u32 })?;
		faces.push(face);
		// MIRRORING IS A PROPERTY OF THE LEVEL, not of the character: a parenthesis in a
		// right-to-left run is drawn as its pair, and in a left-to-right run it is not.
		let level = levelled.levels.levels.get(character).copied().unwrap_or(levelled.levels.paragraph_level);
		levels.push(level);
		mirrored.push(if level % 2 == 1 { cluster.first().copied().and_then(unicode_bidi::mirrored) } else { None });
		character += cluster.len();
	}
	Ok(Faced { levelled, faces, levels, mirrored })
}

/// The faces a paragraph is shaped with, by the face ids the fallback policy chose.
///
/// A TRAIT RATHER THAN A TYPE for the reason `Coverage` is one: the faces are the catalogue's and the
/// caller's, and the pipeline only asks for the one a run was given.
pub trait Faces {
	fn face(&self, face: u16) -> Option<&Face<'_>>;
}

/// One shaped run: face-, script-, language- and direction-homogeneous, which is the shared
/// contract's own requirement.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ShapedRun {
	/// Byte offsets into the source.
	pub start: usize,
	pub end: usize,
	pub face: u16,
	pub script: Script,
	pub language: [u8; 4],
	/// The bidi level every cluster of the run is at.
	pub level: u8,
	pub direction: Direction,
	/// The shaped glyphs, in LOGICAL order, with their positions in font units and each glyph's
	/// cluster a byte offset into the ORIGINAL text - which is what `font-run`'s producer takes.
	pub buffer: Buffer,
	/// One advance per glyph of `buffer`, in 26.6 at the paragraph's size: what line breaking
	/// measures. Converted by the seam's one rounding function, so it is the advance the produced
	/// run will carry.
	pub advances: Vec<Fixed266>,
}

/// STAGES 5 AND 6: the paragraph's runs, mirrored and shaped.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Shaped<'a> {
	pub faced: Faced<'a>,
	/// The em size the advances were measured at, in 26.6.
	pub size: Fixed266,
	pub runs: Vec<ShapedRun>,
}

/// What one run is before it is shaped: the clusters it covers, and what all of them share.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Span {
	/// Cluster indices, the end exclusive.
	first: usize,
	end: usize,
	face: u16,
	script: Script,
	language: [u8; 4],
	level: u8,
}

/// MIRROR AND SHAPE: split the paragraph into runs and put every run through the shaper.
///
/// A RUN ENDS WHERE ANYTHING IT IS HOMOGENEOUS IN CHANGES - the item, so the script or the language;
/// the face fallback chose; the bidi level - and where the shaper's input ceiling would be passed,
/// at the last space before it when there is one. Each run is shaped with the language its item was
/// given, which is what selects the face's language system and the `locl` forms it carries.
pub fn shape<'a>(faced: Faced<'a>, faces: &impl Faces, size: Fixed266) -> Result<Shaped<'a>, Error> {
	let spans = spans_of(&faced)?;
	let mut runs: Vec<ShapedRun> = storage(spans.len())?;
	let mut glyphs = 0usize;
	for span in &spans {
		let run = shape_span(&faced, span, faces, size)?;
		// THE PARAGRAPH'S OUTPUT CEILING, held as the runs arrive rather than after all of them have
		// been shaped and stored.
		glyphs = glyphs.saturating_add(run.buffer.len());
		if glyphs > crate::MAX_PARAGRAPH_GLYPHS {
			return Err(Error::Exceeded { limit: "paragraph output", ceiling: opentype_profile::limits::PARAGRAPH_OUTPUT, asked: glyphs as u64 });
		}
		runs.push(run);
	}
	Ok(Shaped { faced, size, runs })
}

/// The runs a paragraph is split into.
fn spans_of(faced: &Faced<'_>) -> Result<Vec<Span>, Error> {
	let items = &faced.levelled.items;
	let text = items.source.text();
	let ceiling = opentype_profile::limits::RUN_INPUT as usize;
	let mut spans: Vec<Span> = Vec::new();
	// How many characters the open span holds, and the last cluster boundary in it that follows a
	// space - where it is split if it grows past the shaper's ceiling.
	let mut held = 0usize;
	let mut after_space: Option<usize> = None;
	for index in 0..faced.faces.len() {
		let (start, end) = (items.clusters[index], items.clusters[index + 1]);
		let item = items.items.get(items.items.partition_point(|candidate| candidate.end <= start)).ok_or(Error::NoFace { cluster: start as u32 })?;
		let span = Span { first: index, end: index + 1, face: faced.faces[index], script: item.script, language: item.language, level: faced.levels[index] };
		let length = text[start..end].chars().count();
		match spans.last_mut() {
			Some(open) if open.face == span.face && open.script == span.script && open.language == span.language && open.level == span.level => {
				if held + length > ceiling {
					// SPLIT AT THE LAST SPACE when there is one, because a space breaks no joining and
					// almost no font's context; mid-word only when the span has no space at all.
					match after_space.filter(|at| *at > open.first) {
						Some(at) => {
							let carried = Span { first: at, end: index + 1, ..*open };
							open.end = at;
							held = text[items.clusters[at]..end].chars().count();
							room(&mut spans)?;
							spans.push(carried);
						}
						None => {
							held = length;
							room(&mut spans)?;
							spans.push(span);
						}
					}
					after_space = None;
				} else {
					open.end = index + 1;
					held += length;
				}
			}
			_ => {
				held = length;
				after_space = None;
				room(&mut spans)?;
				spans.push(span);
			}
		}
		if text[start..end].chars().next().is_some_and(|first| general_category(first) == GeneralCategory::Zs) {
			after_space = Some(index + 1);
		}
	}
	Ok(spans)
}

/// Shape one run: map its characters through its face, mirrored where its level asks, and put them
/// through the shaper with its language.
fn shape_span(faced: &Faced<'_>, span: &Span, faces: &impl Faces, size: Fixed266) -> Result<ShapedRun, Error> {
	let items = &faced.levelled.items;
	let text = items.source.text();
	let (start, end) = (items.clusters[span.first], items.clusters[span.end]);
	let face = faces.face(span.face).ok_or(Error::NoFace { cluster: start as u32 })?;
	let right_to_left = span.level % 2 == 1;
	let mut characters: Vec<char> = storage(text[start..end].chars().count())?;
	let mut buffer = Buffer::new();
	let mut cluster: Vec<char> = Vec::new();
	for index in span.first..span.end {
		let at = items.clusters[index];
		let mirror = if right_to_left { faced.mirrored[index] } else { None };
		spell(face, &text[at..items.clusters[index + 1]], at, mirror, right_to_left, &mut cluster, &mut characters, &mut buffer)?;
	}
	font_shape::shape_run(face, &mut buffer, &characters, span.language)?;
	let units = face.header.units_per_em;
	let mut advances: Vec<Fixed266> = storage(buffer.len())?;
	for position in &buffer.positions {
		advances.push(Fixed266::from_font_units(position.x_advance, units, size).map_err(Error::Overflow)?);
	}
	Ok(ShapedRun { start, end, face: span.face, script: span.script, language: span.language, level: span.level, direction: if right_to_left { Direction::RightToLeft } else { Direction::LeftToRight }, buffer, advances })
}

/// The glyph a face maps a character to.
fn glyph_of(face: &Face<'_>, character: char) -> Result<Option<u16>, Error> {
	Ok(face.glyph_for(character)?)
}

/// Append one cluster to a run: its characters, and the glyphs the face maps them to.
///
/// THE CALLER'S OWN SPELLING WHEN THE FACE HAS IT. Each glyph then reports the byte offset of the
/// character it came from, and a right-to-left cluster whose first character mirrors is drawn from
/// the MIRROR when the face has a glyph for it - rule L4, applied here because this is the one stage
/// that knows the face. Every other glyph of a right-to-left run is marked for the face's own `rtlm`,
/// which is how a face draws a mirrored form there is no mirror character for.
///
/// OTHERWISE THE CANONICAL SPELLING FALLBACK ASKED ABOUT. A face that covers only the decomposition
/// of `é` was chosen for it because the canonical view said so, and it is shaped with the
/// decomposition - every glyph reporting the CLUSTER's start, because the characters it came from
/// are not bytes the caller wrote. The caller's buffer is never rewritten; only what the shaper is
/// handed is. A cluster no spelling of which the face maps is a coverage answer the face does not
/// bear out, and it is refused rather than drawn as nothing.
#[allow(clippy::too_many_arguments)]
fn spell(face: &Face<'_>, original: &str, at: usize, mirror: Option<char>, right_to_left: bool, cluster: &mut Vec<char>, characters: &mut Vec<char>, buffer: &mut Buffer) -> Result<(), Error> {
	let mirror = match mirror {
		Some(mirror) if glyph_of(face, mirror)?.is_some() => Some(mirror),
		_ => None,
	};
	let mark = if right_to_left { font_shape::buffer::RTLM } else { 0 };
	let mut whole = true;
	for (offset, found) in original.char_indices() {
		let drawn = if offset == 0 { mirror.unwrap_or(found) } else { found };
		if glyph_of(face, drawn)?.is_none() {
			whole = false;
			break;
		}
	}
	if whole {
		for (offset, found) in original.char_indices() {
			let mirrored = offset == 0 && mirror.is_some();
			let drawn = if mirrored { mirror.unwrap_or(found) } else { found };
			let glyph = glyph_of(face, drawn)?.ok_or(Error::NoFace { cluster: at as u32 })?;
			append(characters, buffer, drawn, glyph, (at + offset) as u32, if mirrored { 0 } else { mark })?;
		}
		return Ok(());
	}
	cluster.clear();
	for found in original.chars() {
		room(cluster)?;
		cluster.push(found);
	}
	let view = CanonicalView::of(cluster).ok_or(Error::NoFace { cluster: at as u32 })?;
	for spelling in [view.composed(), view.decomposed()] {
		let mut mapped = true;
		for found in spelling {
			if glyph_of(face, *found)?.is_none() {
				mapped = false;
				break;
			}
		}
		if mapped {
			for found in spelling {
				let glyph = glyph_of(face, *found)?.ok_or(Error::NoFace { cluster: at as u32 })?;
				append(characters, buffer, *found, glyph, at as u32, mark)?;
			}
			return Ok(());
		}
	}
	Err(Error::NoFace { cluster: at as u32 })
}

fn append(characters: &mut Vec<char>, buffer: &mut Buffer, character: char, glyph: u16, cluster: u32, mask: u32) -> Result<(), Error> {
	room(characters)?;
	characters.push(character);
	buffer.push(glyph, cluster)?;
	if let Some(info) = buffer.infos.last_mut() {
		info.mask |= mask;
	}
	Ok(())
}

/// STAGE 7: the width of every run, which line breaking needs and nothing before it does.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Measured<'a> {
	pub shaped: Shaped<'a>,
	pub widths: Vec<Fixed266>,
}

pub fn measure<'a>(shaped: Shaped<'a>) -> Result<Measured<'a>, Error> {
	// THE PARAGRAPH'S OUTPUT CEILING, counted where every run's glyphs are finally in hand. The input
	// ceiling above does not imply it: a paragraph within its code-point limit that a pathological
	// face expands sixty-four fold is millions of glyphs with every offset in range.
	let mut glyphs = 0usize;
	for run in &shaped.runs {
		glyphs = glyphs.saturating_add(run.buffer.len());
	}
	if glyphs > crate::MAX_PARAGRAPH_GLYPHS {
		return Err(Error::Exceeded { limit: "paragraph output", ceiling: opentype_profile::limits::PARAGRAPH_OUTPUT, asked: glyphs as u64 });
	}
	let mut widths: Vec<Fixed266> = storage(shaped.runs.len())?;
	for run in &shaped.runs {
		let mut total = Fixed266::ZERO;
		for advance in &run.advances {
			total = total.checked_add(*advance).map_err(|_| Error::TooManyGlyphs)?;
		}
		widths.push(total);
	}
	Ok(Measured { shaped, widths })
}

/// STAGE 8: the lines, from the break opportunities and a width.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Line {
	/// Byte offsets into the source.
	pub start: usize,
	pub end: usize,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Lines<'a> {
	pub measured: Measured<'a>,
	pub lines: Vec<Line>,
}

/// Break into lines at a width, in 26.6 pixels at the paragraph's size. A width of zero or less
/// breaks only where a break is mandatory.
///
/// GREEDY OVER THE UAX #14 OPPORTUNITIES: each line takes as much as fits and ends at the last
/// opportunity that did, and a line too long for any opportunity ends at the first one after it -
/// a line that cannot be broken is still a line. A MANDATORY BREAK ENDS A LINE WHATEVER THE WIDTH IS,
/// which is what "mandatory" means and what a wrapper that only measured would miss - but the text
/// before it is still wrapped to the width first, the last line of a paragraph included.
///
/// A LINE NEVER ENDS INSIDE A GRAPHEME CLUSTER. UAX #14 permits a few breaks UAX #29 does not - a
/// space followed by a combining mark is one cluster and two break classes - and a line ending there
/// would split a cluster between two lines, which no later stage can draw.
pub fn break_lines<'a>(measured: Measured<'a>, width: Fixed266) -> Result<Lines<'a>, Error> {
	let items = &measured.shaped.faced.levelled.items;
	let text = items.source.text();
	let clusters = &items.clusters;
	let mut opportunities: Vec<(usize, LineBreakOpportunity)> = storage(items.source.characters + 2)?;
	opportunities.resize(items.source.characters + 2, (0usize, LineBreakOpportunity::Prohibited));
	let count = line_break_opportunities(text, &mut opportunities).min(opportunities.len());

	// THE ADVANCE BEFORE EVERY CLUSTER BOUNDARY, summed once: the width of any stretch of whole
	// clusters is then one subtraction rather than a walk over every glyph of the paragraph.
	let mut before: Vec<i64> = storage(clusters.len().max(1))?;
	before.resize(clusters.len().max(1), 0);
	for run in &measured.shaped.runs {
		for (info, advance) in run.buffer.infos.iter().zip(&run.advances) {
			let at = clusters.partition_point(|boundary| *boundary <= info.cluster as usize);
			if let Some(slot) = before.get_mut(at) {
				*slot += i64::from(advance.raw());
			}
		}
	}
	for at in 1..before.len() {
		before[at] += before[at - 1];
	}
	let over = |from: usize, to: usize| width.raw() > 0 && before[to] - before[from] > i64::from(width.raw());

	let mut lines: Vec<Line> = Vec::new();
	let push = |from: usize, to: usize, lines: &mut Vec<Line>| -> Result<(), Error> {
		room(lines)?;
		lines.push(Line { start: clusters[from], end: clusters[to] });
		Ok(())
	};
	let mut start = 0usize;
	let mut last_allowed: Option<usize> = None;
	for (offset, opportunity) in &opportunities[..count] {
		let Ok(at) = clusters.binary_search(offset) else { continue };
		if at <= start {
			continue;
		}
		// The text since the line began no longer fits: end the line at the last opportunity that
		// did. THIS opportunity is then still one - for the new line - and is kept as such below,
		// rather than dropped because it was the one that showed the old line was full.
		if over(start, at)
			&& let Some(previous) = last_allowed.take()
		{
			push(start, previous, &mut lines)?;
			start = previous;
		}
		if *opportunity == LineBreakOpportunity::Mandatory {
			push(start, at, &mut lines)?;
			start = at;
			last_allowed = None;
		} else {
			last_allowed = Some(at);
		}
	}
	if start + 1 < clusters.len() {
		push(start, clusters.len() - 1, &mut lines)?;
	}
	Ok(Lines { measured, lines })
}

/// STAGE 9: the runs of every line, with each run a break split shaped again piece by piece.
///
/// A BREAK CAN CHANGE WHAT A RUN WAS SHAPED AGAINST. The paragraph's runs were shaped whole, and a
/// break inside one leaves the glyphs on either side shaped with a context that is now on another
/// line: a zero-width space or a soft hyphen is TRANSPARENT to Arabic joining, so a break at one
/// leaves a letter at the end of a line in a medial form joining towards nothing; a kerning pair
/// across a hyphen leaves the hyphen at the end of a line narrowed for a letter that is not there.
/// So every piece of a split run is shaped AGAIN, on its own text - once, because each piece is new
/// text and is never reshaped a second time. A run a line holds whole is not touched.
///
/// THE BREAKS ARE NOT CHOSEN AGAIN. A reshaped line may be a few units wider or narrower than the
/// width it was broken at; choosing the breaks again from the new widths is an iterative fit, and
/// that is what the profile's line-pass ceilings are reserved for when one is written.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Reshaped<'a> {
	/// The lines - with the paragraph's runs moved out of them, into `runs`.
	pub lines: Lines<'a>,
	/// For each line, its runs in LOGICAL order.
	pub runs: Vec<Vec<ShapedRun>>,
}

pub fn reshape<'a>(mut lines: Lines<'a>, faces: &impl Faces) -> Result<Reshaped<'a>, Error> {
	let shaped = core::mem::take(&mut lines.measured.shaped.runs);
	let size = lines.measured.shaped.size;
	let faced = &lines.measured.shaped.faced;
	let clusters = &faced.levelled.items.clusters;
	let mut runs: Vec<Vec<ShapedRun>> = storage(lines.lines.len())?;
	for _ in &lines.lines {
		runs.push(Vec::new());
	}
	for run in shaped {
		let first = lines.lines.partition_point(|line| line.end <= run.start);
		let Some(line) = lines.lines.get(first) else { continue };
		if run.end <= line.end {
			room(&mut runs[first])?;
			runs[first].push(run);
			continue;
		}
		for (index, line) in lines.lines.iter().enumerate().skip(first) {
			if line.start >= run.end {
				break;
			}
			let (start, end) = (run.start.max(line.start), run.end.min(line.end));
			// Both ends are cluster boundaries: the run's because runs are made of clusters, the
			// line's because a line never ends inside one.
			let (Ok(from), Ok(to)) = (clusters.binary_search(&start), clusters.binary_search(&end)) else { return Err(Error::NoFace { cluster: start as u32 }) };
			let span = Span { first: from, end: to, face: run.face, script: run.script, language: run.language, level: run.level };
			let piece = shape_span(faced, &span, faces, size)?;
			room(&mut runs[index])?;
			runs[index].push(piece);
		}
	}
	Ok(Reshaped { lines, runs })
}

/// STAGE 10: the visual order of each line.
///
/// THIS IS WHY THE TYPES ARE SHAPED THIS WAY. UAX #9's reordering is applied PER LINE, after the
/// breaks are known - a paragraph reordered once and then wrapped is wrong wherever it wraps, and
/// the error is invisible until a line happens to break inside a right-to-left run. `Visual` is
/// reachable from `Reshaped` and from nothing else, so that mistake cannot be written here.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Visual<'a> {
	pub reshaped: Reshaped<'a>,
	/// For each line, its characters' indices within the line in the order they are drawn.
	pub order: Vec<Vec<usize>>,
}

pub fn reorder<'a>(reshaped: Reshaped<'a>) -> Result<Visual<'a>, Error> {
	let levelled = &reshaped.lines.measured.shaped.faced.levelled;
	let text = levelled.items.source.text();
	let mut order: Vec<Vec<usize>> = storage(reshaped.lines.lines.len())?;
	let mut start = 0usize;
	let mut counted = 0usize;
	for line in &reshaped.lines.lines {
		// The character index the line starts at, counted on from the last line's rather than from
		// the start of the paragraph every time.
		start += text[counted..line.start].chars().count();
		counted = line.start;
		let length = text[line.start..line.end].chars().count();
		let available = levelled.levels.levels.len();
		let (from, to) = (start.min(available), (start + length).min(available));
		// The levels of THIS line, and rule L1 applied to it - which is also per line, because the
		// whitespace that resets is the whitespace at the end of a LINE.
		let mut classes: Vec<unicode_tables::BidiClass> = storage(length)?;
		classes.extend(text[line.start..line.end].chars().map(unicode_tables::bidi_class));
		let mut slice: Vec<u8> = storage(to - from)?;
		slice.extend_from_slice(&levelled.levels.levels[from..to]);
		let mut levels = Levels { paragraph_level: levelled.levels.paragraph_level, levels: slice, classes: Vec::new() };
		levels.reset_whitespace(&classes);
		let mut removed: Vec<bool> = storage(levels.levels.len())?;
		removed.resize(levels.levels.len(), false);
		order.push(unicode_bidi::reorder_visual(&levels.levels, &removed));
	}
	Ok(Visual { reshaped, order })
}
