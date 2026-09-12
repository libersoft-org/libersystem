//! ONE CALL FOR BOTH OUTLINE FORMATS, so a caller does not have to know which a face is.
//!
//! A FACE HAS `glyf` OR CHARSTRINGS AND NEVER BOTH. Which one it is decides whether its curves are
//! quadratic or cubic, whether a glyph is data or a program, and whether it varies through `gvar`
//! deltas or through `blend` inside the charstring - and none of that is the business of a layer that
//! wants a glyph drawn. A caller asking each face which format it is would ask once per glyph and get
//! it wrong for the faces nobody tested against.
//!
//! THE SCRATCH IS ONLY THE `glyf` PATH'S, and it is still taken for both: a charstring needs no point
//! buffer because it is walked as it is run, while a `glyf` glyph's variation deltas are interpolated
//! from its own points and cannot be. Taking the buffer either way keeps one signature rather than
//! two, and a caller that switches faces does not switch call sites.

use crate::cff::Cff;
use crate::glyf::Outline;
use crate::gvar::Scratch;
use crate::tables::Face;
use crate::variations::Normalised;
use crate::{Error, Malformed};

/// Walk one glyph's outline at a variation coordinate, whichever format the face is in.
pub fn walk(face: &Face<'_>, glyph: u16, coordinates: &[Normalised], scratch: Scratch<'_>, outline: &mut impl Outline) -> Result<(), Error> {
	if let Some(cff) = Cff::of(face)? {
		return cff.walk(glyph, coordinates, outline);
	}
	if face.table(b"glyf")?.is_some() {
		return crate::gvar::walk_varied(face, glyph, coordinates, scratch, outline);
	}
	// A FACE WITH NEITHER IS NOT A FACE THIS SYSTEM DRAWS, and saying which table is missing is what a
	// staging report needs: "no outlines" is not something anybody can act on.
	Err(Error::Malformed(Malformed::MissingTable { table: *b"glyf" }))
}

/// Which format a face's outlines are in, for a caller that has to know - a rasteriser choosing
/// between a quadratic and a cubic path builder, and nothing else.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
	/// Quadratic outlines in `glyf`, varying through `gvar`.
	Quadratic,
	/// Cubic charstrings in `CFF` or `CFF2`, varying through `blend`.
	Cubic,
}

/// The format, or `None` for a face with no outlines at all.
pub fn format(face: &Face<'_>) -> Result<Option<Format>, Error> {
	if face.table(b"CFF2")?.is_some() || face.table(b"CFF ")?.is_some() {
		return Ok(Some(Format::Cubic));
	}
	if face.table(b"glyf")?.is_some() {
		return Ok(Some(Format::Quadratic));
	}
	Ok(None)
}
