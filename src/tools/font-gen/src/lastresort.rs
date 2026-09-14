//! THE LAST-RESORT FACE: a visible box for a code point nothing else covers.
//!
//! WHAT IT IS NOT. It is not a text face and it does not make this system able to display Arabic,
//! Devanagari, Thai or Khmer: every code point it covers draws the same box. What it gives the tree
//! is a REAL face - real `glyf` outlines, a real `cmap`, real metrics - staged at the canonical
//! destination, so the catalogue has something to enumerate, the truth oracle has something to
//! check, and the guest gate has something to shape, lay out and rasterise.

use crate::write::{Cmap, FaceSpec, Glyph, Names};

// The design grid. A thousand units to the em is what a `glyf` face conventionally uses, and every
// number below is in it.
const UNITS_PER_EM: u16 = 1000;
const ASCENDER: i16 = 800;
const DESCENDER: i16 = -200;
const LINE_GAP: i16 = 0;

// The box, and the space it sits in. The advance is the same for every covered code point: a face
// whose glyphs are all one box is monospaced by construction, and saying so is more honest than
// inventing per-character widths nothing measured.
const BOX_ADVANCE: u16 = 800;
const SPACE_ADVANCE: u16 = 500;
const BOX_LEFT: i16 = 100;
const BOX_RIGHT: i16 = 700;
const BOX_BOTTOM: i16 = 0;
const BOX_TOP: i16 = 800;
const STROKE: i16 = 80;

// Three glyphs, and each is there for a reason.
//
//   0  `.notdef`, which every face must have at index zero. A `cmap` may not map to it: zero is how
//      a lookup says NOT COVERED, so a face that mapped a code point there would be claiming coverage
//      and delivering the absence of it.
//   1  the space, which must be BLANK. A last-resort face draws a box for everything it covers, and
//      a box where a space belongs would break every line break and every advance in the layout
//      above it - the one character whose correct rendering is nothing at all.
//   2  the box, which is what every other covered code point maps to.
const GLYPH_NOTDEF: u16 = 0;
const GLYPH_SPACE: u16 = 1;
const GLYPH_BOX: u16 = 2;

// The family and style the declaration beside the face has to agree with, letter for letter.
pub const FAMILY: &str = "LiberSystem Last Resort";
pub const STYLE: &str = "Regular";
const VERSION_STRING: &str = "Version 1.000";
const POSTSCRIPT_NAME: &str = "LiberSystemLastResort-Regular";
const VENDOR: [u8; 4] = *b"LIBR";
pub const WEIGHT_CLASS: u16 = 400;
pub const WIDTH_CLASS: u16 = 5;

/// The box: an outer contour and an inner one wound the other way, so a non-zero fill leaves a ring.
///
/// The winding is the whole of it. Both contours wound the same way would fill solid under a
/// non-zero rule, and a solid rectangle is not what a reader recognises as "this system has no face
/// for this character".
fn box_glyph() -> Glyph {
	let outer = vec![(BOX_LEFT, BOX_BOTTOM), (BOX_LEFT, BOX_TOP), (BOX_RIGHT, BOX_TOP), (BOX_RIGHT, BOX_BOTTOM)];
	let inner = vec![
		(BOX_LEFT + STROKE, BOX_BOTTOM + STROKE),
		(BOX_RIGHT - STROKE, BOX_BOTTOM + STROKE),
		(BOX_RIGHT - STROKE, BOX_TOP - STROKE),
		(BOX_LEFT + STROKE, BOX_TOP - STROKE),
	];
	Glyph { contours: vec![outer, inner], advance: BOX_ADVANCE, left_bearing: BOX_LEFT }
}

/// `cmap`: the whole basic plane, mapped to the box.
///
/// FORMAT 4 WITH A GLYPH ARRAY, and the size is the price of the design. Format 4's `idDelta` maps a
/// segment to CONSECUTIVE glyph ids, so a segment cannot send a range to one glyph; the array is how
/// a many-to-one mapping is expressed in the formats this profile admits. Format 13 exists in the
/// specification for exactly this and is NOT in the admitted set, so adding it would be a profile
/// change rather than a face.
fn coverage() -> Vec<u16> {
	(0..0xFFFFu32)
		.map(|code| match code {
			// U+0000 is not a character anybody renders, and the control range is not coverage
			// either: a face that claimed them would be answering for code points a text stack is
			// supposed to handle before it ever asks a face.
			0x0000..=0x001F | 0x007F..=0x009F => GLYPH_NOTDEF,
			0x0020 | 0x00A0 => GLYPH_SPACE,
			_ => GLYPH_BOX,
		})
		.collect()
}

pub fn spec() -> FaceSpec {
	FaceSpec { names: Names { family: FAMILY.into(), style: STYLE.into(), postscript: POSTSCRIPT_NAME.into(), version: VERSION_STRING.into() }, units_per_em: UNITS_PER_EM, ascender: ASCENDER, descender: DESCENDER, line_gap: LINE_GAP, weight_class: WEIGHT_CLASS, width_class: WIDTH_CLASS, vendor: VENDOR, average_advance: BOX_ADVANCE as i16, fixed_pitch: true, glyphs: vec![box_glyph(), Glyph::blank(SPACE_ADVANCE), box_glyph()], cmap: Cmap::BasicPlane(coverage()), extra: Vec::new() }
}
