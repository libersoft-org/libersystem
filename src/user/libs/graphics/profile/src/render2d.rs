//! `Render2D Core Profile 1`: the closed list, and the functions that answer questions about it.
//!
//! WHAT "COMPLETE" MEANS, AND IT MEANS THIS LIST. A conforming backend implements every entry.
//! `Unsupported` is reserved for extensions added after Profile 1 and may never be returned for
//! anything in it: a backend that answers `Unsupported` for a profile feature is not a backend with
//! a gap, it is a backend that does not conform.

use crate::{FeatureOwner, ProfileEntry};

/// One feature of `Render2D Core Profile 1`.
///
/// THE GRANULARITY IS "SOMETHING A BACKEND CAN FAIL TO DO". `CapButt` and `CapRound` are separate
/// because a rasteriser can get one right and the other wrong, and a conformance matrix that could
/// only say "strokes" would report a half-working backend as conforming.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Render2DFeature {
	// Geometry.
	PathConstruction,
	FillNonZero,
	FillEvenOdd,
	ShapeRect,
	ShapeRoundedRect,
	ShapeCircle,
	ShapeEllipse,
	ShapeArc,
	ShapeLine,
	ShapePolyline,
	ShapePolygon,
	// Strokes.
	StrokeWidth,
	CapButt,
	CapRound,
	CapSquare,
	JoinMiter,
	JoinBevel,
	JoinRound,
	MiterLimit,
	StrokeDash,
	// Antialiasing.
	AnalyticCoverageFill,
	AnalyticCoverageStroke,
	AnalyticCoverageGlyph,
	// Transforms.
	TransformAffine,
	TransformProjective,
	// Paints.
	PaintSolid,
	PaintLinearGradient,
	PaintRadialGradient,
	PaintConicGradient,
	PaintImagePattern,
	GradientMultiStop,
	SpreadClamp,
	SpreadRepeat,
	SpreadMirror,
	PaintTransform,
	LinearLightStops,
	// Clipping.
	ClipRect,
	ClipRoundedRect,
	ClipPath,
	ClipNested,
	ClipAlphaMask,
	ClipInverse,
	// Layers.
	LayerGroupOpacity,
	LayerBlendMode,
	LayerNesting,
	// Compositing: the twelve Porter-Duff operators, and additive `Plus`, which is not one of them.
	CompositeClear,
	CompositeSource,
	CompositeDestination,
	CompositeSourceOver,
	CompositeDestinationOver,
	CompositeSourceIn,
	CompositeDestinationIn,
	CompositeSourceOut,
	CompositeDestinationOut,
	CompositeSourceAtop,
	CompositeDestinationAtop,
	CompositeXor,
	CompositePlus,
	// Separable blend modes.
	BlendMultiply,
	BlendScreen,
	BlendOverlay,
	BlendDarken,
	BlendLighten,
	BlendColorDodge,
	BlendColorBurn,
	BlendHardLight,
	BlendSoftLight,
	BlendDifference,
	BlendExclusion,
	// Non-separable blend modes. NOT deferred: they are what a theme tint, a highlight and a duotone
	// are, they need the whole pixel rather than a per-channel function, and adding them later
	// changes the shape of the compositing loop.
	BlendHue,
	BlendSaturation,
	BlendColor,
	BlendLuminosity,
	// Images.
	ImageSourceDestRect,
	ImageProjective,
	ImageNearest,
	ImageBilinear,
	ImageBicubic,
	ImageMipmappedMinification,
	ImageWrapClamp,
	ImageWrapRepeat,
	ImageWrapMirror,
	ImageOpacity,
	ImageColorSpaceConversion,
	// Filters: a bounded graph, not three named effects.
	FilterGaussianBlur,
	FilterDropShadow,
	FilterColorMatrix,
	FilterComposite,
	FilterBlend,
	FilterConvolution,
	FilterMorphologyDilate,
	FilterMorphologyErode,
	FilterDisplacementMap,
	FilterCrop,
	FilterTile,
	FilterBackdrop,
	// Text. `render2d` must render everything a shaping stack can produce, not only outlines.
	GlyphOutlines,
	GlyphGrayscaleMask,
	GlyphBitmapStrike,
	GlyphColorLayers,
	GlyphEmbeddedColorBitmap,
	GlyphTransform,
	GlyphSubpixelPositioning,
	// Queries. Geometry-core functions a rasteriser never sees - see `FeatureOwner`.
	QueryPathBoolean,
	QueryHitTest,
	QueryTightBounds,
	QueryStrokeBounds,
	QueryPathLength,
	QueryPointAtDistance,
	QueryTangentAtDistance,
}

profile! {
	/// `Render2D Core Profile 1`, closed and enumerated.
	RENDER2D_CORE_PROFILE_1: Render2DFeature;
	"geometry", Render2D, PathConstruction;
	"geometry", Backend, FillNonZero;
	"geometry", Backend, FillEvenOdd;
	"geometry", Render2D, ShapeRect;
	"geometry", Render2D, ShapeRoundedRect;
	"geometry", Render2D, ShapeCircle;
	"geometry", Render2D, ShapeEllipse;
	"geometry", Render2D, ShapeArc;
	"geometry", Render2D, ShapeLine;
	"geometry", Render2D, ShapePolyline;
	"geometry", Render2D, ShapePolygon;
	"strokes", Backend, StrokeWidth;
	"strokes", Backend, CapButt;
	"strokes", Backend, CapRound;
	"strokes", Backend, CapSquare;
	"strokes", Backend, JoinMiter;
	"strokes", Backend, JoinBevel;
	"strokes", Backend, JoinRound;
	"strokes", Backend, MiterLimit;
	"strokes", Backend, StrokeDash;
	"antialiasing", Backend, AnalyticCoverageFill;
	"antialiasing", Backend, AnalyticCoverageStroke;
	"antialiasing", Backend, AnalyticCoverageGlyph;
	"transforms", Render2D, TransformAffine;
	"transforms", Render2D, TransformProjective;
	"paints", Backend, PaintSolid;
	"paints", Backend, PaintLinearGradient;
	"paints", Backend, PaintRadialGradient;
	"paints", Backend, PaintConicGradient;
	"paints", Backend, PaintImagePattern;
	"paints", Backend, GradientMultiStop;
	"paints", Backend, SpreadClamp;
	"paints", Backend, SpreadRepeat;
	"paints", Backend, SpreadMirror;
	"paints", Backend, PaintTransform;
	"paints", GraphicsCore, LinearLightStops;
	"clipping", Backend, ClipRect;
	"clipping", Backend, ClipRoundedRect;
	"clipping", Backend, ClipPath;
	"clipping", Backend, ClipNested;
	"clipping", Backend, ClipAlphaMask;
	"clipping", Backend, ClipInverse;
	"layers", Backend, LayerGroupOpacity;
	"layers", Backend, LayerBlendMode;
	"layers", Render2D, LayerNesting;
	"compositing", Backend, CompositeClear;
	"compositing", Backend, CompositeSource;
	"compositing", Backend, CompositeDestination;
	"compositing", Backend, CompositeSourceOver;
	"compositing", Backend, CompositeDestinationOver;
	"compositing", Backend, CompositeSourceIn;
	"compositing", Backend, CompositeDestinationIn;
	"compositing", Backend, CompositeSourceOut;
	"compositing", Backend, CompositeDestinationOut;
	"compositing", Backend, CompositeSourceAtop;
	"compositing", Backend, CompositeDestinationAtop;
	"compositing", Backend, CompositeXor;
	"compositing", Backend, CompositePlus;
	"compositing", Backend, BlendMultiply;
	"compositing", Backend, BlendScreen;
	"compositing", Backend, BlendOverlay;
	"compositing", Backend, BlendDarken;
	"compositing", Backend, BlendLighten;
	"compositing", Backend, BlendColorDodge;
	"compositing", Backend, BlendColorBurn;
	"compositing", Backend, BlendHardLight;
	"compositing", Backend, BlendSoftLight;
	"compositing", Backend, BlendDifference;
	"compositing", Backend, BlendExclusion;
	"compositing", Backend, BlendHue;
	"compositing", Backend, BlendSaturation;
	"compositing", Backend, BlendColor;
	"compositing", Backend, BlendLuminosity;
	"images", Backend, ImageSourceDestRect;
	"images", Backend, ImageProjective;
	"images", Backend, ImageNearest;
	"images", Backend, ImageBilinear;
	"images", Backend, ImageBicubic;
	"images", Backend, ImageMipmappedMinification;
	"images", Backend, ImageWrapClamp;
	"images", Backend, ImageWrapRepeat;
	"images", Backend, ImageWrapMirror;
	"images", Backend, ImageOpacity;
	"images", GraphicsCore, ImageColorSpaceConversion;
	"filters", Backend, FilterGaussianBlur;
	"filters", Backend, FilterDropShadow;
	"filters", Backend, FilterColorMatrix;
	"filters", Backend, FilterComposite;
	"filters", Backend, FilterBlend;
	"filters", Backend, FilterConvolution;
	"filters", Backend, FilterMorphologyDilate;
	"filters", Backend, FilterMorphologyErode;
	"filters", Backend, FilterDisplacementMap;
	"filters", Backend, FilterCrop;
	"filters", Backend, FilterTile;
	"filters", Backend, FilterBackdrop;
	"text", Backend, GlyphOutlines;
	"text", Backend, GlyphGrayscaleMask;
	"text", Backend, GlyphBitmapStrike;
	"text", Backend, GlyphColorLayers;
	"text", Backend, GlyphEmbeddedColorBitmap;
	"text", Backend, GlyphTransform;
	"text", Backend, GlyphSubpixelPositioning;
	"queries", Render2D, QueryPathBoolean;
	"queries", Render2D, QueryHitTest;
	"queries", Render2D, QueryTightBounds;
	"queries", Render2D, QueryStrokeBounds;
	"queries", Render2D, QueryPathLength;
	"queries", Render2D, QueryPointAtDistance;
	"queries", Render2D, QueryTangentAtDistance;
}

/// The groups, in the order the profile documents them.
pub const RENDER2D_GROUPS: &[&str] = &["geometry", "strokes", "antialiasing", "transforms", "paints", "clipping", "layers", "compositing", "images", "filters", "text", "queries"];

/// Is this feature in Profile 1?
///
/// EVERY FEATURE IS, TODAY, and the function exists for the day one is not: `Unsupported` is
/// reserved for extensions added AFTER Profile 1, and the thing that decides whether a name is an
/// extension is this list rather than a reader's memory.
pub fn in_profile_1(feature: Render2DFeature) -> bool {
	RENDER2D_CORE_PROFILE_1.iter().any(|entry| entry.feature == feature)
}

/// The entry for a feature named as the profile spells it.
pub fn entry_by_name(name: &str) -> Option<&'static ProfileEntry<Render2DFeature>> {
	RENDER2D_CORE_PROFILE_1.iter().find(|entry| entry.name == name)
}
