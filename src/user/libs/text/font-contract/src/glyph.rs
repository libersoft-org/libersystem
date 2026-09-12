//! What a glyph IS to a rasteriser, and the values that distinguish two rasterisations of one glyph.
//!
//! THE DECODED FORMS ARE ENUMERATED RATHER THAN CALLED A CATEGORY. "Access to decoded resources" is
//! not a contract: an outline, a coverage mask, a bitmap strike, a `COLR` v0 layer list and a `COLR`
//! v1 paint graph are five different things to draw, and a consumer that could only ask for "the
//! glyph" would have to guess which it got.

use crate::fixed::Fixed266;

/// How many variation axes a run or a cache key carries.
///
/// EIGHT, CHOSEN HERE. The contract needs a bound because the cache key is a fixed-size value that
/// is hashed and compared, and a growable list in a key is an allocation per lookup. Five axes are
/// registered by OpenType and a shipping variable face rarely declares more; a face beyond this
/// bound is a typed refusal by the layer that reads it rather than a silently truncated instance.
pub const MAX_VARIATION_AXES: usize = 8;

/// Which decoded form of a glyph is being asked for or was rasterised.
///
/// `GrayscaleMask` and `SubpixelMask` are separate kinds because they are different rasterisations
/// of one outline and must not share a cache entry; the LAYOUT a subpixel mask was rasterised for is
/// in `RasterisationMode`, not here, because a mask for RGB-horizontal and one for BGR-vertical are
/// the same KIND and different pixels.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum GlyphKind {
	/// The outline itself, for a consumer that fills it.
	Outline,
	/// An 8-bit coverage mask.
	GrayscaleMask,
	/// A three-coverage mask for an LCD surface.
	SubpixelMask,
	/// A monochrome or colour bitmap from a strike (`sbix`, `CBDT`/`CBLC`).
	BitmapStrike,
	/// A `COLR` v0 layer list: layers of glyphs, each with a palette colour.
	ColrLayers,
	/// A `COLR` v1 paint graph: solids, gradients, transforms, clips and composites.
	ColrPaintGraph,
}

/// The subpixel geometry of a surface, which decides whether an LCD mask can be rasterised at all.
///
/// `None` is a real answer and not a missing one: it is what a surface whose geometry is unknown
/// says, and the rule everywhere is that an unknown layout means grayscale rather than a refusal.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum SubpixelLayout {
	None,
	RgbHorizontal,
	BgrHorizontal,
	RgbVertical,
	BgrVertical,
}

/// What a glyph was actually rasterised AS.
///
/// IT IS PART OF THE CACHE KEY, and that is the whole reason it is a type. An LCD mask differs by
/// RGB/BGR and by horizontal/vertical, and grayscale is what a rotated transform, an unknown layout
/// and a transparent offscreen layer all fall back to - so without this a mask rasterised for one
/// mode is returned for another, which is stale pixels that look like a rasteriser bug.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum RasterisationMode {
	/// One coverage value per pixel.
	Grayscale,
	/// Three coverage values per pixel, for the named geometry.
	Subpixel(SubpixelLayout),
}

/// Where inside a pixel a glyph was rasterised.
///
/// QUANTISED, because a cache with a continuous key caches nothing. The quarter-pixel grid is the
/// granularity positioning is stored at for cache purposes; the POSITION itself stays 26.6 in the
/// run, so quantising here loses nothing a run carries.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SubpixelPhase {
	/// Quarters of a pixel, `0..4`.
	pub x: u8,
	/// Quarters of a pixel, `0..4`.
	pub y: u8,
}

impl SubpixelPhase {
	/// The phase a position falls in. The fractional part of a 26.6 value is `0..64`, so the quarter
	/// is the top two bits of it.
	pub fn of(x: Fixed266, y: Fixed266) -> Self {
		Self { x: (x.fraction() >> 4) as u8, y: (y.fraction() >> 4) as u8 }
	}
}

/// The normalised variation coordinates of an instance.
///
/// NORMALISED 2.14, as OpenType defines them - `-1.0` to `1.0` in a signed 16-bit value - so the
/// same instance is the same coordinates however it was named. A named instance and an arbitrary one
/// that resolve to the same coordinates are the same instance and share a cache entry, which is the
/// property a user-space axis list would not have.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct VariationCoordinates {
	axes: [i16; MAX_VARIATION_AXES],
	count: u8,
}

impl VariationCoordinates {
	/// The default instance: no axes.
	pub const NONE: Self = Self { axes: [0; MAX_VARIATION_AXES], count: 0 };

	/// The coordinates of an instance, or `None` when the face declares more axes than the contract
	/// carries - which is a typed refusal at the layer that reads the face, not a truncation.
	pub fn new(coordinates: &[i16]) -> Option<Self> {
		if coordinates.len() > MAX_VARIATION_AXES {
			return None;
		}
		let mut axes = [0i16; MAX_VARIATION_AXES];
		axes[..coordinates.len()].copy_from_slice(coordinates);
		Some(Self { axes, count: coordinates.len() as u8 })
	}

	pub fn as_slice(&self) -> &[i16] {
		&self.axes[..self.count as usize]
	}
}

/// The transform a glyph was rasterised under, as a cache key compares it.
///
/// STORED AS BITS, because a key is hashed and compared and `f32` is neither `Eq` nor `Hash`. The
/// constructor refuses a non-finite matrix, which is what makes that substitution safe: the only
/// `f32` values that compare unequal to themselves cannot get in, and `-0.0` is normalised to `0.0`
/// so two transforms that ARE equal cannot hash apart.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct TransformKey {
	bits: [u32; 6],
}

impl TransformKey {
	/// The identity transform.
	pub const IDENTITY: Self = Self { bits: [1f32.to_bits(), 0, 0, 1f32.to_bits(), 0, 0] };

	/// A 2x3 affine matrix in column-major order - `a b c d e f` as `x' = a*x + c*y + e`.
	/// `None` for a matrix carrying a NaN or an infinity, which is a refusal rather than a key that
	/// never matches itself.
	pub fn new(matrix: [f32; 6]) -> Option<Self> {
		let mut bits = [0u32; 6];
		for (slot, value) in bits.iter_mut().zip(matrix) {
			if !value.is_finite() {
				return None;
			}
			// NORMALISE NEGATIVE ZERO. `-0.0 == 0.0` and their bit patterns differ, so without this
			// two equal transforms would be two cache entries.
			*slot = if value == 0.0 { 0f32.to_bits() } else { value.to_bits() };
		}
		Some(Self { bits })
	}

	/// The matrix back, for a consumer that has to apply it.
	pub fn matrix(&self) -> [f32; 6] {
		let mut matrix = [0f32; 6];
		for (value, bits) in matrix.iter_mut().zip(self.bits) {
			*value = f32::from_bits(bits);
		}
		matrix
	}
}
