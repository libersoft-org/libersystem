//! Colour glyphs and bitmap strikes: the `COLR` v0 layer list, the v1 paint graph, and the strikes.
//!
//! THE PAINT GRAPH IS ENUMERATED BY PAINT, not called "COLR v1". A v1 font is a small drawing
//! program, and which paints a renderer can draw decides whether an emoji arrives as intended, as a
//! flat colour, or not at all. The renderer this profile draws through already has the gradients,
//! transforms, clips and composites the graph asks for - which is why the graph is IN the profile
//! rather than deferred - and a paint outside this list is a typed refusal with the glyph's
//! fallback, never a silently different picture.
//!
//! EVERY PAINT HAS A `Var` COUNTERPART in the format, reading its values through the item variation
//! store. They are not listed twice: admitting a paint admits its variable form, and a `Var` paint
//! in a font with no variation store is a malformed font rather than an unsupported one.

/// One paint the v1 graph may carry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PaintKind {
	/// The base format number.
	pub format: u8,
	pub name: &'static str,
	pub what: &'static str,
	/// Whether the format gives this paint a VARIABLE counterpart at `format + 1`.
	///
	/// NOT EVERY PAINT HAS ONE, and assuming they all did admitted a format the format does not
	/// have: `PaintColrLayers`, `PaintGlyph`, `PaintColrGlyph` and `PaintComposite` carry no numbers
	/// of their own to vary, so there is no 33 above `PaintComposite` at 32.
	pub varies: bool,
}

/// How a gradient continues outside its defined range.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExtendMode {
	Pad,
	Repeat,
	Reflect,
}

/// The composite modes `PaintComposite` may name.
///
/// THE SAME SET THE 2D PROFILE CARRIES, deliberately: a colour glyph composites with the operators
/// the renderer already implements, and a profile that named a mode the renderer does not have would
/// be a promise nothing could keep.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CompositeMode {
	pub value: u8,
	pub name: &'static str,
}

/// A bitmap strike format.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BitmapFormat {
	pub table: &'static str,
	pub name: &'static str,
	pub what: &'static str,
}

/// The paints of `COLR` v1.
pub const PAINTS: &[PaintKind] = &[
	PaintKind { format: 1, name: "PaintColrLayers", what: "a run of layers in the layer list", varies: false },
	PaintKind { format: 2, name: "PaintSolid", what: "a palette entry with an alpha", varies: true },
	PaintKind { format: 4, name: "PaintLinearGradient", what: "a two-point gradient with a rotation point", varies: true },
	PaintKind { format: 6, name: "PaintRadialGradient", what: "a two-circle gradient", varies: true },
	PaintKind { format: 8, name: "PaintSweepGradient", what: "an angular gradient", varies: true },
	PaintKind { format: 10, name: "PaintGlyph", what: "a glyph outline used as a clip for the paint below it", varies: false },
	PaintKind { format: 11, name: "PaintColrGlyph", what: "another colour glyph, by glyph id", varies: false },
	PaintKind { format: 12, name: "PaintTransform", what: "an affine transform of the paint below", varies: true },
	PaintKind { format: 14, name: "PaintTranslate", what: "a translation", varies: true },
	PaintKind { format: 16, name: "PaintScale", what: "a scale", varies: true },
	PaintKind { format: 18, name: "PaintScaleAroundCenter", what: "a scale about a point", varies: true },
	PaintKind { format: 20, name: "PaintScaleUniform", what: "a uniform scale", varies: true },
	PaintKind { format: 22, name: "PaintScaleUniformAroundCenter", what: "a uniform scale about a point", varies: true },
	PaintKind { format: 24, name: "PaintRotate", what: "a rotation", varies: true },
	PaintKind { format: 26, name: "PaintRotateAroundCenter", what: "a rotation about a point", varies: true },
	PaintKind { format: 28, name: "PaintSkew", what: "a skew", varies: true },
	PaintKind { format: 30, name: "PaintSkewAroundCenter", what: "a skew about a point", varies: true },
	PaintKind { format: 32, name: "PaintComposite", what: "two paints under one of the composite modes", varies: false },
];

/// The three extend modes, all of them.
pub const EXTEND_MODES: &[ExtendMode] = &[ExtendMode::Pad, ExtendMode::Repeat, ExtendMode::Reflect];

/// The composite modes `PaintComposite` may name: the Porter-Duff set, the separable blend modes and
/// the non-separable four - the same list the 2D profile's compositing group carries.
pub const COMPOSITE_MODES: &[CompositeMode] = &[
	CompositeMode { value: 0, name: "Clear" },
	CompositeMode { value: 1, name: "Source" },
	CompositeMode { value: 2, name: "Destination" },
	CompositeMode { value: 3, name: "SourceOver" },
	CompositeMode { value: 4, name: "DestinationOver" },
	CompositeMode { value: 5, name: "SourceIn" },
	CompositeMode { value: 6, name: "DestinationIn" },
	CompositeMode { value: 7, name: "SourceOut" },
	CompositeMode { value: 8, name: "DestinationOut" },
	CompositeMode { value: 9, name: "SourceAtop" },
	CompositeMode { value: 10, name: "DestinationAtop" },
	CompositeMode { value: 11, name: "Xor" },
	CompositeMode { value: 12, name: "Plus" },
	CompositeMode { value: 13, name: "Screen" },
	CompositeMode { value: 14, name: "Overlay" },
	CompositeMode { value: 15, name: "Darken" },
	CompositeMode { value: 16, name: "Lighten" },
	CompositeMode { value: 17, name: "ColorDodge" },
	CompositeMode { value: 18, name: "ColorBurn" },
	CompositeMode { value: 19, name: "HardLight" },
	CompositeMode { value: 20, name: "SoftLight" },
	CompositeMode { value: 21, name: "Difference" },
	CompositeMode { value: 22, name: "Exclusion" },
	CompositeMode { value: 23, name: "Multiply" },
	CompositeMode { value: 24, name: "HslHue" },
	CompositeMode { value: 25, name: "HslSaturation" },
	CompositeMode { value: 26, name: "HslColor" },
	CompositeMode { value: 27, name: "HslLuminosity" },
];

/// The bitmap strike formats.
pub const BITMAP_FORMATS: &[BitmapFormat] = &[
	BitmapFormat { table: "sbix", name: "png", what: "a PNG strike, which is what Apple colour emoji carry" },
	BitmapFormat { table: "sbix", name: "tiff", what: "a TIFF strike" },
	BitmapFormat { table: "sbix", name: "jpg", what: "a JPEG strike" },
	BitmapFormat { table: "CBDT", name: "format 17", what: "small metrics, PNG data" },
	BitmapFormat { table: "CBDT", name: "format 18", what: "big metrics, PNG data" },
	BitmapFormat { table: "CBDT", name: "format 19", what: "metrics in `CBLC`, PNG data" },
];

/// Does the profile draw this paint format?
///
/// The variable counterpart of a paint is admitted WITH it - the same drawing operation reading its
/// numbers through the variation store - but only where the format has one.
pub fn paint(format: u8) -> Option<&'static PaintKind> {
	PAINTS.iter().find(|paint| paint.format == format || (paint.varies && paint.format + 1 == format))
}

/// Does the profile composite under this mode value?
pub fn composite_mode(value: u8) -> Option<&'static CompositeMode> {
	COMPOSITE_MODES.iter().find(|mode| mode.value == value)
}
