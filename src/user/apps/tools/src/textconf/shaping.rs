//! THE SCRIPT CORPUS ON THE TARGET: the authored conformance cases, shaped, laid out and drawn inside a
//! booted guest.
//!
//! WHY THE STAGED FACE ALONE DOES NOT MAKE THE CLAIM. Every code point the last-resort face covers is
//! the same box and the face carries no layout tables, so a string of it exercises the catalogue, the
//! `cmap`, one run and the rasteriser - and not the shaping the three-architecture claim is about. The
//! conformance faces carry real `GSUB`, `GPOS` and `GDEF`, and the cases named at the end of
//! `corpus.rs` are shaped here by the shaper and the pipeline stages the host gate drives, and
//! compared with the same authored expectations, EXACTLY: every glyph, every cluster, every advance
//! and every offset.
//!
//! AND THEN DRAWN, against coverage computed from the outlines and NOT from the shaper's answer. The
//! oracle places each glyph where the corpus SAYS it goes - the authored advances and offsets, turned
//! into 26.6 by the seam's one rounding rule - while the picture is drawn from the run the shaper and
//! `font-run` actually produced. Every glyph of these faces is an axis-aligned rectangle, so the two
//! meet in the frozen comparison with nothing approximated between them.
//!
//! PURE, like the drawing it uses: the faces arrive already read, and what is printed is the caller's.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use font_contract::{Direction, FaceRef, Fixed266};
use font_parse::Face;
use font_run::{Request, Run, produce};
use font_shape::Buffer;
use font_shape::buffer::GlyphInfo;
use render2d::list::RecordedGlyphRun;
use text_pipeline::Visual;

use crate::corpus::{Case, Expect, PARAGRAPH_SIZE, Paragraph};
use crate::raster::{self, Placed, Target, fixed, pixels_of, whole};

/// THE SIZE AT WHICH EVERY EDGE OF THE CORPUS FACES LANDS ON A PIXEL BOUNDARY. Their outlines,
/// advances and anchors are all multiples of five units on a grid of a thousand, and five units is
/// one pixel at 200 to the em - so every pixel is wholly covered or wholly clear, and the frozen edge
/// policy compares each one EXACTLY.
pub const ALIGNED_SIZE: f64 = 200.0;

/// AND A SIZE AT WHICH THEY DO NOT. At 50 to the em a vertical edge fifty units in from its advance
/// falls half way through a pixel, so the antialiasing tolerance applies along every one of them.
/// Every HORIZONTAL edge of these faces is a multiple of twenty units, which is a whole pixel here -
/// and that keeps a mark's lower edge on the same pixel boundary as its base's upper edge.
pub const FRACTIONAL_SIZE: f64 = 50.0;

/// AND A SIZE AT WHICH TWO GLYPHS SHARE PIXELS. At 37 to the em a mark's lower edge and its base's
/// upper edge fall inside one pixel row, so a pixel there is covered in part by each - and what it
/// holds is the compositing rule (`raster::analytic`), drawn and compared here rather than avoided.
pub const SHARED_SIZE: f64 = 37.0;

/// The space left around a line inside its target, in whole pixels.
const MARGIN: f64 = 4.0;

/// One face of the pin file: its digest, and what its glyphs are called.
pub struct Pinned<'p> {
	pub file: &'p str,
	pub digest: [u8; 32],
	pub glyphs: Vec<(&'p str, u16)>,
}

/// The pin file, read: `face <file> <sha-256>` and `glyph <file> <name> <index>`, comments and blank
/// lines between them. The grammar `text-corpus` reads, and nothing else.
pub fn pins(text: &str) -> Result<Vec<Pinned<'_>>, String> {
	let mut pins: Vec<Pinned<'_>> = Vec::new();
	for (number, line) in text.lines().enumerate() {
		let line = line.trim();
		if line.is_empty() || line.starts_with('#') {
			continue;
		}
		let mut fields = line.split_whitespace();
		match (fields.next(), fields.next(), fields.next(), fields.next(), fields.next()) {
			(Some("face"), Some(file), Some(digest), None, None) => {
				let digest = decode_digest(digest).ok_or_else(|| format!("pin line {}: {digest} is not a SHA-256", number + 1))?;
				pins.push(Pinned { file, digest, glyphs: Vec::new() });
			}
			(Some("glyph"), Some(file), Some(name), Some(index), None) => {
				let index: u16 = index.parse().map_err(|_| format!("pin line {}: {index} is not a glyph index", number + 1))?;
				let face = pins.iter_mut().find(|face| face.file == file).ok_or_else(|| format!("pin line {}: {file} has no `face` line before its glyphs", number + 1))?;
				face.glyphs.push((name, index));
			}
			_ => return Err(format!("pin line {}: `{line}` is not a pin", number + 1)),
		}
	}
	if pins.is_empty() {
		return Err(String::from("the pin file pins no faces at all"));
	}
	Ok(pins)
}

fn decode_digest(text: &str) -> Option<[u8; 32]> {
	if text.len() != 64 {
		return None;
	}
	let mut out = [0u8; 32];
	for (index, pair) in text.as_bytes().chunks(2).enumerate() {
		out[index] = u8::from_str_radix(core::str::from_utf8(pair).ok()?, 16).ok()?;
	}
	Some(out)
}

/// One conformance face as the guest reached it: the pin that names its glyphs, and the face itself.
pub struct Installed<'a> {
	pub file: &'a str,
	pub glyphs: &'a [(&'a str, u16)],
	pub face: Face<'a>,
	/// Who the face is for the glyph cache - a function of its bytes, as the seam requires.
	pub identity: FaceRef,
}

impl Installed<'_> {
	/// The index of a glyph the corpus names. THE CORPUS NAMES GLYPHS AND NEVER NUMBERS THEM; the pins
	/// are what turn a name into the index this face gives it.
	pub fn id(&self, name: &str) -> Result<u16, String> {
		self.glyphs.iter().find(|(known, _)| *known == name).map(|(_, index)| *index).ok_or_else(|| format!("the corpus names a glyph the pins of {} do not: {name}", self.file))
	}

	/// The name of a glyph, for a report.
	pub fn name(&self, glyph: u16) -> String {
		self.glyphs.iter().find(|(_, index)| *index == glyph).map(|(name, _)| String::from(*name)).unwrap_or_else(|| format!("glyph {glyph}"))
	}
}

/// THE ONE FACE A PARAGRAPH MAY USE, installed the way the host gate installs it.
impl text_pipeline::Coverage for Installed<'_> {
	fn covers(&self, face: u16, spelling: &[char]) -> bool {
		face == 0 && !spelling.is_empty() && spelling.iter().all(|character| matches!(self.face.glyph_for(*character), Ok(Some(_))))
	}
}

impl text_pipeline::Faces for Installed<'_> {
	fn face(&self, face: u16) -> Option<&Face<'_>> {
		(face == 0).then_some(&self.face)
	}
}

/// Shape one case the way the host gate does: the buffer from the `cmap` and from nothing else, then
/// the shaper, in the default language.
pub fn shape(installed: &Installed<'_>, case: &Case<'_>) -> Result<Buffer, String> {
	let face = &installed.face;
	let characters: Vec<char> = case.text.chars().collect();
	let mut buffer = Buffer::default();
	let mut at = 0u32;
	for character in &characters {
		let glyph = face.glyph_for(*character).map_err(|error| format!("the face refused a character: {error:?}"))?.ok_or_else(|| format!("{} has no glyph for U+{:04X}, which the corpus says it covers", installed.file, *character as u32))?;
		buffer.infos.push(GlyphInfo::new(glyph, at));
		buffer.positions.push(font_shape::Position::default());
		at += character.len_utf8() as u32;
	}
	font_shape::shape_run(face, &mut buffer, &characters, *b"dflt").map_err(|error| format!("shaping refused the run: {error:?}"))?;
	Ok(buffer)
}

/// The OpenType script a case is shaped under, which is what its report names.
pub fn script_of(text: &str) -> Result<[u8; 4], String> {
	let characters: Vec<char> = text.chars().collect();
	font_shape::scripts::profile_script(&characters).map_err(|error| format!("the run's script is refused: {error:?}"))
}

/// The comparison: every glyph, cluster, advance and offset, EXACTLY.
pub fn compare(installed: &Installed<'_>, buffer: &Buffer, expect: &[Expect]) -> Result<(), String> {
	if buffer.len() != expect.len() {
		return Err(format!("expected {} glyph(s), got {}: {}", expect.len(), buffer.len(), describe(installed, buffer)));
	}
	for (index, expect) in expect.iter().enumerate() {
		let wanted = installed.id(expect.glyph)?;
		let info = buffer.infos[index];
		let position = buffer.positions[index];
		if info.glyph != wanted {
			return Err(format!("glyph {index} is {} where the corpus declares {}: {}", installed.name(info.glyph), expect.glyph, describe(installed, buffer)));
		}
		if info.cluster != expect.cluster {
			return Err(format!("glyph {index} ({}) came from cluster {} where the corpus declares {}", expect.glyph, info.cluster, expect.cluster));
		}
		if position.x_advance != expect.x_advance {
			return Err(format!("glyph {index} ({}) advances {} where the corpus declares {}", expect.glyph, position.x_advance, expect.x_advance));
		}
		if position.x_offset != expect.x_offset || position.y_offset != expect.y_offset {
			return Err(format!("glyph {index} ({}) sits at ({}, {}) where the corpus declares ({}, {})", expect.glyph, position.x_offset, position.y_offset, expect.x_offset, expect.y_offset));
		}
		// WHAT A MARK HANGS FROM, which is what a right-to-left run is placed from once it is reversed.
		if info.attached_to != expect.base {
			return Err(format!("glyph {index} ({}) is attached to {:?} where the corpus declares {:?}", expect.glyph, info.attached_to, expect.base));
		}
	}
	Ok(())
}

/// AND THE COMPARISON PROVES IT REFUSES, against this very case: the authored expectations with ONE
/// thing wrong at a time, each of which must be refused. A comparison that stopped comparing a field
/// would pass every one of these but its own - and in a guest nothing else would notice.
pub fn refuses_the_wrong_answer(installed: &Installed<'_>, case: &Case<'_>, buffer: &Buffer) -> Result<(), String> {
	let copy = |expect: &Expect| Expect { glyph: expect.glyph, cluster: expect.cluster, x_advance: expect.x_advance, x_offset: expect.x_offset, y_offset: expect.y_offset, base: expect.base };
	let mutations: [(&str, fn(&mut Vec<Expect>)); 7] = [
		("a wrong glyph", |wrong| wrong[0].glyph = ".notdef"),
		("a wrong cluster", |wrong| wrong[0].cluster += 1),
		("an advance one unit out", |wrong| wrong[0].x_advance += 1),
		("an offset one unit out across", |wrong| {
			let last = wrong.len() - 1;
			wrong[last].x_offset += 1;
		}),
		("an offset one unit out up", |wrong| {
			let last = wrong.len() - 1;
			wrong[last].y_offset += 1;
		}),
		("a glyph too few", |wrong| {
			wrong.pop();
		}),
		("a mark on the wrong glyph", |wrong| {
			let last = wrong.len() - 1;
			wrong[last].base = Some(last as u16);
		}),
	];
	for (what, mutate) in mutations {
		let mut wrong: Vec<Expect> = case.expect.iter().map(copy).collect();
		if wrong.is_empty() {
			return Err(String::from("a case with no expectations cannot be made wrong"));
		}
		mutate(&mut wrong);
		if compare(installed, buffer, &wrong).is_ok() {
			return Err(format!("the comparison accepted {what}"));
		}
	}
	Ok(())
}

fn describe(installed: &Installed<'_>, buffer: &Buffer) -> String {
	let mut out = String::from("got [");
	for (index, info) in buffer.infos.iter().enumerate() {
		if index > 0 {
			out.push_str(", ");
		}
		let position = buffer.positions[index];
		out.push_str(&format!("{}@{}+{}", installed.name(info.glyph), info.cluster, position.x_advance));
		if position.x_offset != 0 || position.y_offset != 0 {
			out.push_str(&format!("({},{})", position.x_offset, position.y_offset));
		}
	}
	out.push(']');
	out
}

/// Draw one shaped case at one size and compare it with the analytic oracle.
///
/// THE RUN IS THE SHAPER'S AND THE ORACLE IS THE CORPUS'S. The picture comes from the buffer the
/// shaper produced, through `font-run` - its one rounding, its visual order, its turn of the y axis -
/// and through `render2d` into `soft2d`. The oracle places each glyph where the authored expectations
/// put it, in visual order: the logical order reversed for a right-to-left run, which is what visual
/// order IS for a run of one level - and a mark where its anchor meets its base's (`drawn_offset`).
pub fn draw_case(installed: &Installed<'_>, case: &Case<'_>, buffer: &Buffer, size: f64, exact: bool, note: &mut dyn FnMut(&str)) -> Result<String, String> {
	let face = &installed.face;
	let units = face.header.units_per_em;
	let em = fixed(size);
	let right_to_left = paragraph_level(case.text)? % 2 == 1;

	let count = case.expect.len();
	let mut placed = Vec::new();
	let mut pen = Fixed266::ZERO;
	for visual in 0..count {
		let logical = if right_to_left { count - 1 - visual } else { visual };
		let expect = &case.expect[logical];
		let glyph = installed.id(expect.glyph)?;
		let x = sum(pen, units_to(drawn_offset(case, logical, right_to_left)?, units, em)?)?;
		// UP IN THE FONT IS DOWN THE TARGET.
		let y = units_to(-expect.y_offset, units, em)?;
		placed.push(Placed { glyph, x, y });
		pen = sum(pen, units_to(expect.x_advance, units, em)?)?;
	}
	let target = target_for(face, &placed, size)?;
	let placed = place(placed, target)?;

	let request = Request { face, identity: installed.identity, size: em, variation: font_contract::VariationCoordinates::default(), script: font_contract::ScriptTag::from_bytes(script_of(case.text)?), direction: if right_to_left { Direction::RightToLeft } else { Direction::LeftToRight }, mode: font_contract::glyph::RasterisationMode::Grayscale, origin: (fixed(target.origin_x), fixed(target.baseline)), text: case.text, start: 0, end: case.text.len() };
	let run = produce(&request, buffer).map_err(|error| format!("the run could not be produced: {error:?}"))?;
	compare_run(installed, case, &run, right_to_left, em)?;
	let recorded = record(&run, run.run().origin_x);
	raster::check(face, &[recorded], &placed, size, exact, target, note)
}

/// WHERE AN AUTHORED GLYPH IS DRAWN FROM, as an offset from the pen of the order it is drawn in, in
/// font units.
///
/// A MARK IS PLACED BY ITS ANCHORS, NOT BY THE PEN IT HAPPENS TO MEET. The corpus states a mark's offset
/// as the format defines it, in logical order: the base's anchor less the mark's, less everything the
/// pen advanced from the base up to the mark, the base included. So the anchors' difference is the
/// offset plus those advances, and the mark is drawn at its base's pen plus that difference - wherever
/// the order puts the two pens. For a left-to-right run that is the authored offset again; for a
/// right-to-left one, drawn with the mark before its base, it is not, and a reversal that kept the
/// authored offset would draw the mark a whole base away from its anchor. This is computed from the
/// authored expectations alone, so it judges `font-run`'s correction rather than repeating it.
fn drawn_offset(case: &Case<'_>, logical: usize, right_to_left: bool) -> Result<i32, String> {
	let expect = &case.expect[logical];
	// The pen a glyph is drawn at: everything drawn before it, which is what precedes it in the text
	// for a left-to-right run and what follows it for a right-to-left one. Summed rather than stored,
	// so nothing here allocates.
	let pen = |at: usize| -> i64 {
		let before = if right_to_left { &case.expect[at + 1..] } else { &case.expect[..at] };
		before.iter().map(|glyph| i64::from(glyph.x_advance)).sum()
	};
	let offset = match expect.base.map(usize::from) {
		Some(base) if base < logical => {
			let anchors = i64::from(expect.x_offset) + case.expect[base..logical].iter().map(|glyph| i64::from(glyph.x_advance)).sum::<i64>();
			pen(base) + anchors - pen(logical)
		}
		Some(base) => return Err(format!("the corpus attaches {} to glyph {base}, which does not precede it", expect.glyph)),
		None => i64::from(expect.x_offset),
	};
	i32::try_from(offset).map_err(|_| format!("{}'s drawn offset does not fit", expect.glyph))
}

/// THE RUN ITSELF, before its picture: the `GlyphRun` the seam hands a renderer, compared with the
/// corpus EXACTLY - every glyph in visual order, its advance and both offsets in 26.6 by the seam's one
/// rounding, no vertical advance, and every glyph inside the cluster of the character it came from. A
/// run whose last advance or whose cluster map were wrong would draw the same picture, and a caret or a
/// hit test built on it would still be wrong.
fn compare_run(installed: &Installed<'_>, case: &Case<'_>, run: &Run, right_to_left: bool, em: Fixed266) -> Result<(), String> {
	let units = installed.face.header.units_per_em;
	let produced = run.run();
	let count = case.expect.len();
	if produced.glyphs.len() != count {
		return Err(format!("the produced run has {} glyph(s) where the corpus declares {count}", produced.glyphs.len()));
	}
	for (logical, expect) in case.expect.iter().enumerate() {
		let visual = if right_to_left { count - 1 - logical } else { logical };
		let glyph = &produced.glyphs[visual];
		let wanted = (u32::from(installed.id(expect.glyph)?), units_to(expect.x_advance, units, em)?, units_to(drawn_offset(case, logical, right_to_left)?, units, em)?, units_to(-expect.y_offset, units, em)?, Fixed266::ZERO);
		let got = (glyph.glyph, glyph.x_advance, glyph.x_offset, glyph.y_offset, glyph.y_advance);
		if got != wanted {
			return Err(format!("the produced run's glyph {visual} is (glyph, x advance, x offset, y offset, y advance) {got:?} where the corpus declares {} as {wanted:?}", expect.glyph));
		}
		let cluster = run.clusters().iter().find(|cluster| cluster.source.start <= expect.cluster && expect.cluster < cluster.source.end).ok_or_else(|| format!("the produced run has no cluster for byte {} of the text", expect.cluster))?;
		let (first, last) = (cluster.first_glyph as usize, cluster.first_glyph as usize + cluster.glyph_count as usize);
		if !(first..last).contains(&visual) {
			return Err(format!("the produced run puts glyph {visual} ({}) outside the cluster of byte {}, which holds glyphs {first}..{last}", expect.glyph, expect.cluster));
		}
	}
	Ok(())
}

/// A paragraph through every stage of the pipeline, as the host gate drives it: its levels, its
/// lines, the order each line is drawn in, and the glyphs each line's runs were shaped to.
pub fn lay_out(installed: &Installed<'_>, paragraph: &Paragraph<'_>) -> Result<Visual<'static>, String> {
	use text_pipeline::stages;

	let policy = text_pipeline::Policy::new(&[text_pipeline::FaceEntry { face: 0, script: None, language: None, rank: 0 }], None);
	let source = text_pipeline::Source::new(paragraph.text).map_err(|error| format!("the paragraph is refused: {error:?}"))?;
	let levelled = stages::resolve_levels(stages::itemise(source), paragraph.direction);
	let faced = stages::choose_faces(levelled, &policy, installed).map_err(|error| format!("fallback refused: {error:?}"))?;
	let shaped = stages::shape(faced, installed, Fixed266::from_pixels(PARAGRAPH_SIZE)).map_err(|error| format!("shaping refused: {error:?}"))?;
	let measured = stages::measure(shaped).map_err(|error| format!("measuring refused: {error:?}"))?;
	let lines = stages::break_lines(measured, Fixed266::from_pixels(paragraph.width)).map_err(|error| format!("line breaking refused: {error:?}"))?;
	let reshaped = stages::reshape(lines, installed).map_err(|error| format!("reshaping refused: {error:?}"))?;
	stages::reorder(reshaped).map_err(|error| format!("reordering refused: {error:?}"))
}

/// The paragraph's comparison: its level, how many lines, each line's visual order and each line's
/// glyphs, EXACTLY.
pub fn compare_paragraph(installed: &Installed<'_>, paragraph: &Paragraph<'_>, visual: &Visual<'_>) -> Result<(), String> {
	let level = levels_of(visual).paragraph_level;
	if level != paragraph.paragraph_level {
		return Err(format!("the paragraph level is {level} where the corpus declares {}", paragraph.paragraph_level));
	}
	if visual.order.len() != paragraph.lines.len() {
		return Err(format!("the paragraph broke into {} line(s) where the corpus declares {}", visual.order.len(), paragraph.lines.len()));
	}
	for (index, (line, expect)) in visual.order.iter().zip(paragraph.lines).enumerate() {
		if line.as_slice() != expect.order {
			return Err(format!("line {index}'s visual order is {line:?} where the corpus declares {:?}", expect.order));
		}
		let glyphs: Vec<String> = visual.reshaped.runs[index].iter().flat_map(|run| run.buffer.infos.iter().map(|info| installed.name(info.glyph))).collect();
		if glyphs.len() != expect.glyphs.len() || glyphs.iter().zip(expect.glyphs).any(|(got, wanted)| got != wanted) {
			return Err(format!("line {index} was shaped to {glyphs:?} where the corpus declares {:?}", expect.glyphs));
		}
	}
	Ok(())
}

/// And it refuses a reversed order and a line's swapped glyphs - the two halves of what a paragraph
/// case compares.
pub fn paragraph_refuses_the_wrong_answer(installed: &Installed<'_>, paragraph: &Paragraph<'_>, visual: &Visual<'_>) -> Result<(), String> {
	let line = paragraph.lines.first().ok_or_else(|| String::from("a paragraph with no lines"))?;
	let mut order = line.order.to_vec();
	order.reverse();
	let reversed = [crate::corpus::Line { order: &order, glyphs: line.glyphs }];
	let broken = Paragraph { name: paragraph.name, face: paragraph.face, text: paragraph.text, direction: paragraph.direction, paragraph_level: paragraph.paragraph_level, width: paragraph.width, lines: &reversed };
	if compare_paragraph(installed, &broken, visual).is_ok() {
		return Err(String::from("the comparison accepted a reversed visual order"));
	}
	let mut glyphs = line.glyphs.to_vec();
	glyphs.reverse();
	let swapped = [crate::corpus::Line { order: line.order, glyphs: &glyphs }];
	let broken = Paragraph { name: paragraph.name, face: paragraph.face, text: paragraph.text, direction: paragraph.direction, paragraph_level: paragraph.paragraph_level, width: paragraph.width, lines: &swapped };
	if compare_paragraph(installed, &broken, visual).is_ok() {
		return Err(String::from("the comparison accepted a line declared with glyphs it was not shaped to"));
	}
	Ok(())
}

/// Draw a laid-out paragraph's one line at one size and compare it with the analytic oracle.
///
/// THE LINE IS THE PIPELINE'S AND THE ORACLE IS THE CORPUS'S. The picture is every run the pipeline
/// produced for the line, produced again by `font-run` at this size in its own direction, put side by
/// side by `text-layout` in the order the line draws them, and drawn. The oracle places the authored
/// glyphs in the authored visual order, each at the advance the face's own `hmtx` states for it -
/// which is exact for a paragraph in which every character is one glyph and no pair the face kerns or
/// mark it attaches meets inside a run, and it says so when that is not the case.
pub fn draw_paragraph(installed: &Installed<'_>, paragraph: &Paragraph<'_>, visual: &Visual<'_>, size: f64, exact: bool, note: &mut dyn FnMut(&str)) -> Result<String, String> {
	let face = &installed.face;
	let units = face.header.units_per_em;
	let em = fixed(size);
	let (Some(expect), Some(order), Some(runs), Some(line)) = (paragraph.lines.first(), visual.order.first(), visual.reshaped.runs.first(), visual.reshaped.lines.lines.first()) else {
		return Err(String::from("a paragraph with no line"));
	};
	if paragraph.lines.len() != 1 || visual.order.len() != 1 {
		return Err(String::from("the drawn comparison lays out a paragraph of one line, and this one has more"));
	}
	let text = paragraph.text;
	let characters = text[line.start..line.end].chars().count();
	if expect.order.len() != characters || expect.glyphs.len() != characters {
		return Err(format!("the oracle draws one authored glyph per character, and this line has {characters} character(s), {} in its order and {} glyph(s)", expect.order.len(), expect.glyphs.len()));
	}
	for run in runs {
		if run.buffer.positions.iter().any(|position| position.x_offset != 0 || position.y_offset != 0) {
			return Err(String::from("a run of this line carries an offset, and the oracle places a paragraph's glyphs at their own advances"));
		}
	}

	let mut placed = Vec::new();
	let mut pen = Fixed266::ZERO;
	for character in expect.order {
		let name = expect.glyphs.get(*character).ok_or_else(|| format!("the order names character {character}, past the line's glyphs"))?;
		let glyph = installed.id(name)?;
		let (advance, _) = face.advance(glyph).map_err(|error| format!("the face refused the advance of {name}: {error:?}"))?;
		placed.push(Placed { glyph, x: pen, y: Fixed266::ZERO });
		pen = sum(pen, units_to(advance as i32, units, em)?)?;
	}
	let target = target_for(face, &placed, size)?;
	let placed = place(placed, target)?;

	// THE RUNS IN THE ORDER THE LINE DRAWS THEM. The pipeline keeps a line's runs in LOGICAL order and
	// its characters' VISUAL order beside them; a run is drawn where its first-drawn character is.
	let visual_rank = |start: usize, end: usize| -> usize {
		let first = text[line.start..start].chars().count();
		let last = text[line.start..end].chars().count();
		order.iter().position(|character| (first..last).contains(character)).unwrap_or(usize::MAX)
	};
	let mut ranked: Vec<(usize, &text_pipeline::ShapedRun)> = runs.iter().map(|run| (visual_rank(run.start, run.end), run)).collect();
	ranked.sort_by_key(|(rank, _)| *rank);
	let mut produced: Vec<Run> = Vec::new();
	for (_, run) in &ranked {
		let request = Request { face, identity: installed.identity, size: em, variation: font_contract::VariationCoordinates::default(), script: font_contract::ScriptTag::from_bytes(script_of(&text[run.start..run.end])?), direction: run.direction, mode: font_contract::glyph::RasterisationMode::Grayscale, origin: (Fixed266::ZERO, fixed(target.baseline)), text, start: run.start, end: run.end };
		produced.push(produce(&request, &run.buffer).map_err(|error| format!("a run could not be produced: {error:?}"))?);
	}
	let mut pairs = Vec::new();
	for run in &produced {
		pairs.push((run.run(), run.cluster_map().ok_or_else(|| String::from("a produced run has no cluster map"))?));
	}
	let direction = if levels_of(visual).paragraph_level % 2 == 1 { text_layout::ParagraphDirection::RightToLeft } else { text_layout::ParagraphDirection::LeftToRight };
	let composed = text_layout::compose::compose(&pairs, fixed(target.width as f64 - MARGIN * 2.0), direction).map_err(|error| format!("the line could not be composed: {error:?}"))?;
	let mut recorded = Vec::new();
	for (laid, run) in composed.runs.iter().zip(&produced) {
		recorded.push(record(run, sum(fixed(target.origin_x), laid.x)?));
	}
	raster::check(face, &recorded, &placed, size, exact, target, note)
}

fn levels_of<'v>(visual: &'v Visual<'_>) -> &'v unicode_bidi::Levels {
	&visual.reshaped.lines.measured.shaped.faced.levelled.levels
}

/// The paragraph level of a string, which is what decides whether a case's one run reads right to
/// left - the pipeline's answer, the one the Latin run asks too.
fn paragraph_level(text: &str) -> Result<u8, String> {
	let source = text_pipeline::Source::new(text).map_err(|error| format!("the text is refused: {error:?}"))?;
	Ok(text_pipeline::stages::resolve_levels(text_pipeline::stages::itemise(source), unicode_bidi::ParagraphDirection::Auto).levels.paragraph_level)
}

/// A produced run, recorded for drawing with its left edge at `origin_x`.
fn record(run: &Run, origin_x: Fixed266) -> RecordedGlyphRun {
	let shared = run.run();
	RecordedGlyphRun { face: shared.face, size: shared.size, variation: shared.variation, script: shared.script, direction: shared.direction, mode: shared.mode, origin_x, origin_y: shared.origin_y, glyphs: shared.glyphs.to_vec(), clusters: run.clusters().to_vec() }
}

/// Font units in 26.6 at this size, by the seam's one rounding rule - the rounding the run carries.
fn units_to(value: i32, units: u16, em: Fixed266) -> Result<Fixed266, String> {
	Fixed266::from_font_units(value, units, em).map_err(|error| format!("{value} units do not fit 26.6: {error:?}"))
}

fn sum(left: Fixed266, right: Fixed266) -> Result<Fixed266, String> {
	left.checked_add(right).map_err(|error| format!("the pen left the coordinate space: {error:?}"))
}

/// The target a line needs: its ink, from the outlines the oracle will place, with a margin on every
/// side - and the line's origin and baseline in WHOLE pixels, so that a size at which every edge lands
/// on a pixel boundary keeps that property once the line is placed in it.
fn target_for(face: &Face<'_>, placed: &[Placed], size: f64) -> Result<Target, String> {
	let scale = size / face.header.units_per_em as f64;
	let (mut left, mut right, mut up, mut down) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
	let mut inked = false;
	for entry in placed {
		for rectangle in raster::contours_of(face, entry.glyph)? {
			let (x, y) = (pixels_of(entry.x), pixels_of(entry.y));
			left = left.min(x + rectangle.0 as f64 * scale);
			right = right.max(x + rectangle.2 as f64 * scale);
			up = up.max(rectangle.3 as f64 * scale - y);
			down = down.max(y - rectangle.1 as f64 * scale);
			inked = true;
		}
	}
	if !inked {
		return Err(String::from("nothing in this line has an outline to draw"));
	}
	let ceiling = |value: f64| -whole(-value);
	let origin_x = MARGIN + ceiling(-left);
	let baseline = MARGIN + ceiling(up);
	let width = ceiling(origin_x + right) + MARGIN;
	let height = baseline + ceiling(down) + MARGIN;
	Ok(Target { width: width as u32, height: height as u32, baseline, origin_x })
}

/// Placements relative to the line's origin and baseline, moved to where the target puts those.
fn place(placed: Vec<Placed>, target: Target) -> Result<Vec<Placed>, String> {
	let (x, y) = (fixed(target.origin_x), fixed(target.baseline));
	placed.into_iter().map(|entry| Ok(Placed { glyph: entry.glyph, x: sum(entry.x, x)?, y: sum(entry.y, y)? })).collect()
}
