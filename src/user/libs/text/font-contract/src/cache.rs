//! THE GLYPH CACHE KEY. One normative definition, used by the shaping side and by the renderer.
//!
//! NEITHER SIDE RESTATES IT IN ITS OWN WORDS. The key used to be face, size, transform and subpixel
//! phase - which omits the glyph itself, and everything that distinguishes two instances of one
//! face - so a variable, colour, bitmap or replaced-face glyph collided with another and the cache
//! answered with stale pixels. Every field below is one that two entries can differ in and must not
//! share an entry for, and the fixtures prove exactly that, one field at a time.
//!
//! A CHANGE TO THIS TYPE IS A CHANGE TO BOTH SIDES OF THE SEAM, in the same edit.

use crate::face::{FaceIdentity, Generation};
use crate::fixed::Fixed266;
use crate::glyph::{GlyphKind, RasterisationMode, SubpixelPhase, VariationCoordinates};

/// Which bitmap strike a `BitmapStrike` glyph came from, and which palette a `COLR` glyph was
/// rendered with.
///
/// PRESENT ONLY WHERE THE KIND USES THEM, and `None` where it does not - so an outline glyph's key
/// does not carry a palette index that means nothing, and two colour glyphs differing only in
/// palette are two entries.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct KindSelection {
	/// The strike, in the face's own strike order.
	pub strike: Option<u16>,
	/// The `COLR` palette index.
	pub palette: Option<u16>,
}

/// One rasterised glyph, identified completely.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GlyphCacheKey {
	/// WHICH FACE, content-derived and including the face index: a collection is one file and its
	/// faces share the file identity.
	pub face: FaceIdentity,
	/// WHICH PUBLICATION of that face. A replaced file is a different identity; the generation is
	/// what makes everything derived from the old one stop matching.
	pub generation: Generation,
	/// The glyph index within the face - the field whose absence made this key wrong.
	pub glyph: u32,
	/// The em size the glyph was rasterised at, in 26.6.
	pub size: Fixed266,
	/// The instance.
	pub variation: VariationCoordinates,
	/// The transform it was rasterised under.
	pub transform: crate::glyph::TransformKey,
	/// Where inside the pixel.
	pub phase: SubpixelPhase,
	/// Which decoded form.
	pub kind: GlyphKind,
	/// The strike and palette, where the kind uses them.
	pub selection: KindSelection,
	/// What it was rasterised AS - grayscale, or a subpixel mask for one named geometry.
	pub mode: RasterisationMode,
}
