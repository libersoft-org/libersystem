//! The `GlyphRun` the renderer consumes: already-shaped, already-positioned glyphs.
//!
//! A RUN IS FACE-, SCRIPT- AND DIRECTION-HOMOGENEOUS, unconditionally. That is not a tuning choice
//! deferred to a measurement: it is what fallback produces, it is what a per-run paint and cache key
//! need, and a per-glyph face reference would put a face switch inside the one structure whose whole
//! purpose is to be paintable in a single operation. The type carries the face ONCE and cannot
//! express a run of two.
//!
//! EVERY UNIT AND REPRESENTATION IS FIXED HERE, not listed as a decision for an implementer:
//!
//! ```text
//!   source-span unit   UTF-8 BYTE OFFSETS into the original string
//!   span order         LOGICAL - the order the text was written. Visual order is derived from the
//!                        cluster mapping, because storing it would put a bidi decision inside the
//!                        data every consumer reads
//!   numeric form       26.6 fixed point for offsets, advances and origins, rounded half-to-even
//!                        where a scaled font unit becomes a run value
//!   glyph origin       the BASELINE origin, +x right and +y DOWN, which is the renderer's own
//!                        device-space convention - so a run needs no flip to be drawn
//! ```

use crate::face::FaceRef;
use crate::fixed::Fixed266;
use crate::glyph::{GlyphKind, RasterisationMode, VariationCoordinates};

/// Which way the run is written.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Direction {
	LeftToRight,
	RightToLeft,
	TopToBottom,
	BottomToTop,
}

/// An ISO 15924 script code, as its four characters packed big-endian - `Latn`, `Arab`, `Deva`.
///
/// THE TAG AND NOT AN ENUM. The supported-script list belongs to the shaping profile, which can add
/// to it; a closed enum here would make every addition a change to the seam both sides link.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ScriptTag(pub u32);

impl ScriptTag {
	pub const fn from_bytes(tag: [u8; 4]) -> Self {
		Self(u32::from_be_bytes(tag))
	}

	pub const fn to_bytes(self) -> [u8; 4] {
		self.0.to_be_bytes()
	}
}

/// One positioned glyph.
///
/// THE OFFSET IS FROM THE PEN AND THE ADVANCE MOVES IT. Both are 26.6, both are in the run's
/// direction-independent device convention: +x right, +y down.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PositionedGlyph {
	/// The glyph index within the run's face.
	pub glyph: u32,
	pub x_offset: Fixed266,
	pub y_offset: Fixed266,
	pub x_advance: Fixed266,
	pub y_advance: Fixed266,
	/// Which decoded form this entry is - the flag that tells the rasteriser what to draw.
	pub kind: GlyphKind,
	/// The strike and palette where the kind uses them.
	pub selection: crate::cache::KindSelection,
}

/// A shaped run, borrowed rather than owned: the layout that produced it holds the storage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GlyphRun<'a> {
	/// The ONE face this run is in, and the generation it was shaped under.
	pub face: FaceRef,
	/// The em size, in 26.6.
	pub size: Fixed266,
	/// The instance the run was shaped for.
	pub variation: VariationCoordinates,
	/// The ONE script.
	pub script: ScriptTag,
	/// The ONE direction.
	pub direction: Direction,
	/// What the glyphs are to be rasterised as. Carried per RUN because it is a property of the
	/// surface and the transform rather than of a glyph, and because it is part of every entry's
	/// cache key.
	pub mode: RasterisationMode,
	/// The baseline origin of the run, in the consumer's device space.
	pub origin_x: Fixed266,
	pub origin_y: Fixed266,
	/// The glyphs, in VISUAL order - the order they are drawn in.
	pub glyphs: &'a [PositionedGlyph],
}

impl GlyphRun<'_> {
	/// The advance of the whole run, or the typed overflow of the sum.
	///
	/// REFUSED RATHER THAN SATURATED: a line whose advance does not fit 26.6 is one this layer will
	/// not draw, and a saturated total is a silently wrong position.
	pub fn total_advance(&self) -> Result<(Fixed266, Fixed266), crate::fixed::Overflow> {
		let mut x = Fixed266::ZERO;
		let mut y = Fixed266::ZERO;
		for glyph in self.glyphs {
			x = x.checked_add(glyph.x_advance)?;
			y = y.checked_add(glyph.y_advance)?;
		}
		Ok((x, y))
	}

	/// The cache key for one glyph of this run - built HERE, from the run, so a consumer cannot
	/// assemble a key that omits a field.
	pub fn cache_key(&self, at: usize) -> Option<crate::cache::GlyphCacheKey> {
		let glyph = self.glyphs.get(at)?;
		let x = self.origin_x.checked_add(glyph.x_offset).ok()?;
		let y = self.origin_y.checked_add(glyph.y_offset).ok()?;
		Some(crate::cache::GlyphCacheKey { face: self.face.face, generation: self.face.generation, glyph: glyph.glyph, size: self.size, variation: self.variation, transform: crate::glyph::TransformKey::IDENTITY, phase: crate::glyph::SubpixelPhase::of(x, y), kind: glyph.kind, selection: glyph.selection, mode: self.mode })
	}

	/// The cache key under a transform the consumer is drawing with.
	pub fn cache_key_under(&self, at: usize, transform: crate::glyph::TransformKey) -> Option<crate::cache::GlyphCacheKey> {
		let mut key = self.cache_key(at)?;
		key.transform = transform;
		Some(key)
	}
}

/// What a run's glyph kinds tell a consumer about what it must be able to draw.
pub fn kinds_in(run: &GlyphRun<'_>) -> [bool; 6] {
	let mut present = [false; 6];
	for glyph in run.glyphs {
		let at = match glyph.kind {
			GlyphKind::Outline => 0,
			GlyphKind::GrayscaleMask => 1,
			GlyphKind::SubpixelMask => 2,
			GlyphKind::BitmapStrike => 3,
			GlyphKind::ColrLayers => 4,
			GlyphKind::ColrPaintGraph => 5,
		};
		present[at] = true;
	}
	present
}
