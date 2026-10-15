//! THE STAGED FACE'S RUN: one Latin string through the last-resort face, at two sizes.
//!
//! TWO SIZES, AND THE SECOND IS THE INTERESTING ONE. At 50 pixels to the em every edge of this face
//! lands on a pixel boundary, so every pixel is fully covered or not at all and the frozen edge
//! policy compares it EXACTLY. At 37 the edges fall mid-pixel, so most of the outline's boundary is
//! partially covered and the frozen antialiasing tolerance applies - 2/255 per pixel and 1/255 mean.
//! A run at the aligned size alone would never exercise a tolerance; a run at the fractional size
//! alone would never exercise the exactness.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use font_parse::Face;
use font_run::{Request, produce};

use crate::raster::{self, Placed, Target, fixed};

/// The family this run is about. NAMED rather than "the first face": a catalogue with two faces
/// would otherwise make the answer depend on the order it happened to enumerate them.
pub const FAMILY: &str = "LiberSystem Last Resort";

/// The string this run shapes, lays out and draws.
///
/// EVERY CHARACTER OF IT IS COVERED BY THE STAGED FACE and one of them is the space, which the face
/// draws as NOTHING. A corpus of letters alone would never show that the blank glyph advances the
/// pen without marking the target - which is the one case where drawing what the face says and
/// drawing something are different.
const CORPUS: &str = "Ab Cd";

/// The two sizes, in pixels to the em. See the note at the top of this file for why there are two.
pub const ALIGNED_SIZE: f64 = 50.0;
pub const FRACTIONAL_SIZE: f64 = 37.0;

/// The target, and where the baseline sits in it.
const TARGET: Target = Target { width: 256, height: 72, baseline: 56.0, origin_x: 4.0 };

/// One size: shape, lay out, draw, and compare against the analytic oracle.
pub fn run(face: &Face<'_>, bytes: &[u8], size: f64, exact: bool, note: &mut dyn FnMut(&str)) -> Result<String, String> {
	let (recorded, placed) = shape_and_place(face, bytes, size)?;
	let report = raster::check(face, &[recorded], &placed, size, exact, TARGET, note)?;
	Ok(format!("{size} px: {report}"))
}

/// Shape the corpus, produce the shared run, and lay it out.
///
/// THROUGH THE ORDERED PIPELINE AND THROUGH THE SEAM, not around either. The stages exist so that a
/// caller cannot reorder before it wraps; `font-run` exists so that font units become 26.6 in ONE
/// place; `text-layout` exists so that where a run sits on a line is decided once. A conformance run
/// that did any of those itself would be exercising a path no application takes - and would agree
/// with itself about the arithmetic it was supposed to be checking.
fn shape_and_place(face: &Face<'_>, bytes: &[u8], size: f64) -> Result<(render2d::list::RecordedGlyphRun, Vec<Placed>), String> {
	use font_shape::Buffer;
	use font_shape::buffer::GlyphInfo;

	let source = text_pipeline::Source::new(CORPUS).map_err(|error| format!("the corpus is refused: {error:?}"))?;
	let items = text_pipeline::stages::itemise(source);
	let levelled = text_pipeline::stages::resolve_levels(items, unicode_bidi::ParagraphDirection::Auto);
	if levelled.levels.paragraph_level != 0 {
		return Err(format!("the corpus is left to right and the pipeline answered level {}", levelled.levels.paragraph_level));
	}
	if levelled.items.items.len() != 1 {
		return Err(format!("the corpus is one item and the pipeline found {}", levelled.items.items.len()));
	}

	let characters: Vec<char> = CORPUS.chars().collect();
	let mut buffer = Buffer::default();
	let mut at = 0u32;
	for character in &characters {
		let glyph = face.glyph_for(*character).map_err(|error| format!("the face refused a character: {error:?}"))?.ok_or_else(|| format!("the staged face has no glyph for U+{:04X}", *character as u32))?;
		buffer.infos.push(GlyphInfo::new(glyph, at));
		buffer.positions.push(font_shape::Position::default());
		at += character.len_utf8() as u32;
	}
	font_shape::shape_run(face, &mut buffer, &characters, *b"dflt").map_err(|error| format!("shaping refused the corpus: {error:?}"))?;

	let identity = font_contract::FaceRef { face: font_contract::FaceIdentity { file: font_contract::face::FileIdentity(raster::digest_of(bytes)), index: 0 }, generation: font_contract::face::Generation(1) };
	let request = Request { face, identity, size: fixed(size), variation: font_contract::VariationCoordinates::default(), script: font_contract::ScriptTag::from_bytes(*b"latn"), direction: font_contract::Direction::LeftToRight, mode: font_contract::glyph::RasterisationMode::Grayscale, origin: (fixed(TARGET.origin_x), fixed(TARGET.baseline)), text: CORPUS, start: 0, end: CORPUS.len() };
	let run = produce(&request, &buffer).map_err(|error| format!("the run could not be produced: {error:?}"))?;
	let map = run.cluster_map().ok_or_else(|| String::from("the produced run has no cluster map"))?;

	// THE LINE, COMPOSED. One run makes this a short composition and not a pointless one: it is where
	// the run's LEFT edge on the line is decided, and a renderer is handed that rather than the pen
	// the shaper finished at.
	// TEXT-LAYOUT HAS ITS OWN DIRECTION TYPE and reads no text at all, which is what keeps its
	// decisions checkable on numbers alone. The pipeline's answer above is what chooses which one.
	let line = text_layout::compose::compose(&[(run.run(), map)], fixed(TARGET.width as f64 - TARGET.origin_x * 2.0), text_layout::ParagraphDirection::LeftToRight).map_err(|error| format!("the line could not be composed: {error:?}"))?;
	let first = line.runs.first().ok_or_else(|| String::from("the composed line has no runs"))?;

	let shared = first.run;
	let mut placed = Vec::new();
	let mut pen = shared.origin_x;
	for glyph in shared.glyphs {
		let index = u16::try_from(glyph.glyph).map_err(|_| format!("glyph {} is not one this face has", glyph.glyph))?;
		placed.push(Placed { glyph: index, x: pen, y: fixed(TARGET.baseline) });
		pen = pen.checked_add(glyph.x_advance).map_err(|error| format!("the pen left the coordinate space: {error:?}"))?;
	}

	let recorded = render2d::list::RecordedGlyphRun { face: shared.face, size: shared.size, variation: shared.variation, script: shared.script, direction: shared.direction, mode: shared.mode, origin_x: shared.origin_x, origin_y: shared.origin_y, glyphs: shared.glyphs.to_vec(), clusters: run.clusters().to_vec() };
	Ok((recorded, placed))
}
