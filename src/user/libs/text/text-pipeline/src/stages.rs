//! The stages, as types. Each exists only as the output of the one before it.
//!
//! THIS IS THE ORDER, EXPRESSED SO IT CANNOT BE RUN WRONGLY. A pipeline written as a list of
//! functions over one mutable buffer is a pipeline whose order is a convention; here the input of
//! each stage is a type nothing else produces, so "reorder before wrapping" is not a bug that can be
//! written - `Visual` is reachable from `Lines` and from nowhere else.

use alloc::vec::Vec;
use font_contract::{Direction, Fixed266};
use unicode_bidi::{Levels, ParagraphDirection};
use unicode_segmentation::{LineBreakOpportunity, grapheme_boundaries, line_break_opportunities};
use unicode_tables::{Script, script};

use crate::canonical::{Coverage, face_for};
use crate::{Error, MAX_PARAGRAPH};

/// STAGE 1: the caller's text, validated and never rewritten again.
///
/// THE BYTES ARE THE CALLER'S FOR THE WHOLE PIPELINE. Every offset any later stage reports is an
/// offset into THIS string - which is why it is carried through rather than copied out of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Source<'a> {
	text: &'a str,
}

impl<'a> Source<'a> {
	pub fn new(text: &'a str) -> Result<Self, Error> {
		let characters = text.chars().count();
		if characters > MAX_PARAGRAPH {
			return Err(Error::TooLong { characters, limit: MAX_PARAGRAPH });
		}
		Ok(Self { text })
	}

	pub fn text(&self) -> &'a str {
		self.text
	}
}

/// One itemised run: a stretch of one script, one language and one grapheme-cluster boundary set.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Item {
	/// Byte offsets into the source.
	pub start: usize,
	pub end: usize,
	pub script: Script,
}

/// STAGE 2: itemisation. Grapheme cluster boundaries, and the script runs between them.
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
}

pub fn itemise<'a>(source: Source<'a>) -> Items<'a> {
	let text = source.text();
	let mut clusters = alloc::vec![0usize; text.len() + 2];
	let count = grapheme_boundaries(text, &mut clusters);
	clusters.truncate(count);

	// The script of a run is the first script that DECIDES: a common character - a space, a digit -
	// belongs to the run it is in rather than starting one of its own, which is what keeps `a b` one
	// item and not three.
	let mut items: Vec<Item> = Vec::new();
	for window in clusters.windows(2) {
		let (start, end) = (window[0], window[1]);
		let deciding = text[start..end].chars().map(script).find(|candidate| !matches!(candidate, Script::Common | Script::Inherited | Script::Unknown));
		match (items.last_mut(), deciding) {
			(Some(last), Some(found)) if last.script == found => last.end = end,
			(Some(last), None) => last.end = end,
			(_, Some(found)) => items.push(Item { start, end, script: found }),
			(None, None) => items.push(Item { start, end, script: Script::Common }),
		}
	}
	Items { source, clusters, items }
}

/// STAGE 3: the bidi levels of the paragraph.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Levelled<'a> {
	pub items: Items<'a>,
	pub levels: Levels,
}

pub fn resolve_levels<'a>(items: Items<'a>, direction: ParagraphDirection) -> Levelled<'a> {
	let levels = unicode_bidi::levels(items.source.text(), direction);
	Levelled { items, levels }
}

/// STAGE 4 and 5: the face each cluster is drawn with, and the mirroring that follows from its level.
///
/// FALLBACK IS CLUSTER-ATOMIC, which is the whole reason the cluster boundaries were computed two
/// stages ago: a face is chosen for a CLUSTER, never for a character, or an `e` and its accent are
/// drawn from two different fonts.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Faced<'a> {
	pub levelled: Levelled<'a>,
	/// One entry per cluster: the face, and the character to draw if mirroring changed it.
	pub faces: Vec<u16>,
	pub mirrored: Vec<Option<char>>,
}

pub fn choose_faces<'a>(levelled: Levelled<'a>, coverage: &impl Coverage) -> Result<Faced<'a>, Error> {
	let text = levelled.items.source.text();
	let mut faces = Vec::new();
	let mut mirrored = Vec::new();
	for window in levelled.items.clusters.windows(2) {
		let (start, end) = (window[0], window[1]);
		let cluster: Vec<char> = text[start..end].chars().collect();
		let face = face_for(coverage, &cluster).ok_or(Error::NoFace { cluster: start as u32 })?;
		faces.push(face);
		// MIRRORING IS A PROPERTY OF THE LEVEL, not of the character: a parenthesis in a
		// right-to-left run is drawn as its pair, and in a left-to-right run it is not.
		let level = text[..start].chars().count();
		let odd = levelled.levels.levels.get(level).is_some_and(|level| level % 2 == 1);
		mirrored.push(if odd { cluster.first().copied().and_then(unicode_bidi::mirrored) } else { None });
	}
	Ok(Faced { levelled, faces, mirrored })
}

/// STAGE 6: the shaped runs. What a run holds is the caller's - this layer says only that a run is
/// face-, script- and direction-homogeneous, which is the shared contract's own requirement.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ShapedRun {
	pub start: usize,
	pub end: usize,
	pub face: u16,
	pub script: Script,
	pub direction: Direction,
	/// The glyphs, as the caller's shaper produced them.
	pub glyphs: Vec<u16>,
	/// One cluster start per glyph, as a byte offset into the ORIGINAL text.
	pub clusters: Vec<u32>,
	pub advances: Vec<Fixed266>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Shaped<'a> {
	pub faced: Faced<'a>,
	pub runs: Vec<ShapedRun>,
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
		glyphs = glyphs.saturating_add(run.glyphs.len());
	}
	if glyphs > crate::MAX_PARAGRAPH_GLYPHS {
		return Err(Error::Exceeded { limit: "paragraph output", ceiling: opentype_profile::limits::PARAGRAPH_OUTPUT, asked: glyphs as u64 });
	}
	let mut widths = Vec::with_capacity(shaped.runs.len());
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

/// Break into lines at a width, in font units.
///
/// A MANDATORY BREAK ENDS A LINE WHATEVER THE WIDTH IS, which is what "mandatory" means and what a
/// wrapper that only measured would miss.
pub fn break_lines<'a>(measured: Measured<'a>, width: Fixed266) -> Lines<'a> {
	let text = measured.shaped.faced.levelled.items.source.text();
	let mut opportunities = alloc::vec![(0usize, LineBreakOpportunity::Prohibited); text.len() + 2];
	let count = line_break_opportunities(text, &mut opportunities);
	let mut lines = Vec::new();
	let mut start = 0usize;
	let mut last_allowed: Option<usize> = None;
	for (offset, opportunity) in opportunities[..count].iter() {
		let so_far = advance_between(&measured, start, *offset);
		let over = so_far.raw() > width.raw() && width.raw() > 0;
		if *opportunity == LineBreakOpportunity::Mandatory {
			lines.push(Line { start, end: *offset });
			start = *offset;
			last_allowed = None;
			continue;
		}
		if over {
			// The last opportunity that FITS is where the line ends; when none fits, this one does,
			// because a line that cannot be broken is still a line.
			let end = last_allowed.unwrap_or(*offset);
			if end > start {
				lines.push(Line { start, end });
				start = end;
			}
			last_allowed = None;
			continue;
		}
		last_allowed = Some(*offset);
	}
	if start < text.len() {
		lines.push(Line { start, end: text.len() });
	}
	Lines { measured, lines }
}

/// The advance of the text between two byte offsets, from the runs that cover it.
fn advance_between(measured: &Measured<'_>, from: usize, to: usize) -> Fixed266 {
	let mut total = Fixed266::ZERO;
	for run in &measured.shaped.runs {
		for (index, cluster) in run.clusters.iter().enumerate() {
			let at = *cluster as usize;
			if at < from || at >= to {
				continue;
			}
			if let Some(advance) = run.advances.get(index)
				&& let Ok(sum) = total.checked_add(*advance)
			{
				total = sum;
			}
		}
	}
	total
}

/// STAGE 9 and 10: the visual order of each line.
///
/// THIS IS WHY THE TYPES ARE SHAPED THIS WAY. UAX #9's reordering is applied PER LINE, after the
/// breaks are known - a paragraph reordered once and then wrapped is wrong wherever it wraps, and
/// the error is invisible until a line happens to break inside a right-to-left run. `Visual` is
/// reachable from `Lines` and from nothing else, so that mistake cannot be written here.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Visual<'a> {
	pub lines: Lines<'a>,
	/// For each line, the cluster indices in the order they are drawn.
	pub order: Vec<Vec<usize>>,
}

pub fn reorder<'a>(lines: Lines<'a>) -> Visual<'a> {
	let levelled = &lines.measured.shaped.faced.levelled;
	let text = levelled.items.source.text();
	let mut order = Vec::with_capacity(lines.lines.len());
	for line in &lines.lines {
		// The levels of THIS line, and rule L1 applied to it - which is also per line, because the
		// whitespace that resets is the whitespace at the end of a LINE.
		let start = text[..line.start].chars().count();
		let end = text[..line.end].chars().count();
		let classes: Vec<unicode_tables::BidiClass> = text[line.start..line.end].chars().map(unicode_tables::bidi_class).collect();
		let mut levels = Levels { paragraph_level: levelled.levels.paragraph_level, levels: levelled.levels.levels[start.min(levelled.levels.levels.len())..end.min(levelled.levels.levels.len())].to_vec(), classes: Vec::new() };
		levels.reset_whitespace(&classes);
		let removed = alloc::vec![false; levels.levels.len()];
		order.push(unicode_bidi::reorder_visual(&levels.levels, &removed));
	}
	Visual { lines, order }
}
