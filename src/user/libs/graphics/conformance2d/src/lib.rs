//! `Render2D Core Profile 1`, WALKED ENTRY BY ENTRY.
//!
//! WHAT A CONFORMANCE SUITE IS FOR, and it is not "does it draw". A profile is a closed list, and
//! "supports Profile 1" is a claim about EVERY entry in it - so the suite is driven by the
//! machine-readable registry rather than by a list of scenes somebody remembered to write. A feature
//! added to the profile with no scene here is a failure of this suite, which is what makes the two
//! stay in step.
//!
//! ONE SCENE PER FEATURE, AND EACH SCENE STATES ITS OWN PASS CONDITION. A single screenshot compared
//! against a stored one fails for every reason at once and says nothing about which feature broke;
//! these fail one at a time and name the feature. Nothing here compares against a recorded baseline:
//! a baseline captured from this backend agrees with this backend by construction, which makes it a
//! regression test rather than a conformance one.
//!
//! AND IT IS A LIBRARY AND NOT A PROGRAM, so that the same scenes run as host tests on the machine
//! that builds the tree and inside a booted guest on each architecture. The program is a few lines
//! around `run`; what it proves is that the arithmetic agrees on the target and not only on the host.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use graphics_profile::{RENDER2D_CORE_PROFILE_1, Render2DFeature};

#[macro_use]
mod harness;
mod antialiasing;
mod clipping;
mod compositing;
mod filters;
mod geometry;
mod images;
mod layers;
mod output;
mod paints;
mod queries;
mod strokes;
mod text;
mod transforms;

pub use harness::{Trouble, checked};

/// What a scene answers.
pub type Outcome = Result<(), Trouble>;

/// One feature's scene.
///
/// THE FEATURE IS THE PROFILE'S OWN ENUM and the name is read from the registry rather than written
/// here, so a scene cannot claim to cover a feature under a name the profile spells differently.
pub struct Case {
	pub feature: Render2DFeature,
	pub scene: fn() -> Outcome,
}

/// How one case came out.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Verdict {
	Pass,
	/// The scene ran and the picture was wrong.
	Fail(String),
	/// The backend REFUSED a drawing the profile requires. In Profile 1 this is not a gap, it is a
	/// backend that does not conform - `Unsupported` is reserved for extensions added after it.
	Unsupported(String),
}

/// What a whole run came to.
#[derive(Clone, Default, Debug)]
pub struct Summary {
	pub passed: usize,
	pub failed: usize,
	pub unsupported: usize,
	/// Profile entries with no scene at all. THE CHECK THAT KEEPS THE SUITE HONEST: without it, a
	/// feature added to the profile is a feature nobody tests and every run still says "all passed".
	pub untested: Vec<&'static str>,
}

impl Summary {
	/// A run passes only when every feature has a scene and every scene passes.
	pub fn complete(&self) -> bool {
		self.failed == 0 && self.unsupported == 0 && self.untested.is_empty()
	}
}

/// The profile entry a feature belongs to, which is where a scene's name and group come from.
pub fn entry(feature: Render2DFeature) -> Option<&'static graphics_profile::ProfileEntry<Render2DFeature>> {
	RENDER2D_CORE_PROFILE_1.iter().find(|entry| entry.feature == feature)
}

/// Run every scene, reporting each as it finishes.
///
/// REPORTED AS THEY FINISH rather than collected and printed at the end, because this runs inside a
/// guest whose output is a serial log: a suite that prints at the end prints nothing at all when one
/// scene hangs, and which scene it was is the whole of what a reader needs.
pub fn run(mut report: impl FnMut(&'static str, &'static str, &Verdict)) -> Summary {
	let mut summary = Summary::default();
	for case in CASES {
		let Some(entry) = entry(case.feature) else {
			// A CASE FOR A FEATURE THE PROFILE DOES NOT HAVE is a scene measuring an extension while
			// reporting Profile 1, which is the failure the coverage gate refuses in the other
			// direction too.
			summary.failed += 1;
			continue;
		};
		let verdict = match (case.scene)() {
			Ok(()) => Verdict::Pass,
			Err(Trouble::Failed(why)) => Verdict::Fail(why),
			Err(Trouble::Unsupported(why)) => Verdict::Unsupported(why),
		};
		match verdict {
			Verdict::Pass => summary.passed += 1,
			Verdict::Fail(_) => summary.failed += 1,
			Verdict::Unsupported(_) => summary.unsupported += 1,
		}
		report(entry.name, entry.group, &verdict);
	}
	for (name, scene) in OUTPUT_CASES {
		let verdict = match scene() {
			Ok(()) => Verdict::Pass,
			Err(Trouble::Failed(why)) => Verdict::Fail(why),
			Err(Trouble::Unsupported(why)) => Verdict::Unsupported(why),
		};
		match verdict {
			Verdict::Pass => summary.passed += 1,
			Verdict::Fail(_) => summary.failed += 1,
			Verdict::Unsupported(_) => summary.unsupported += 1,
		}
		report(name, "output", &verdict);
	}
	for entry in RENDER2D_CORE_PROFILE_1 {
		if !CASES.iter().any(|case| case.feature == entry.feature) {
			summary.untested.push(entry.name);
		}
	}
	summary
}

/// THE SCENES THAT ARE NOT A PROFILE ENTRY, and are run and reported all the same.
///
/// A profile entry is something a backend can fail to do. Wide-gamut composition and the
/// quantisation at the end of a frame are properties of the path EVERY entry takes, so folding them
/// into one of the features would make that feature fail for two reasons - and leaving them out
/// would mean the suite never drew a colour the target cannot hold.
pub const OUTPUT_CASES: &[(&str, fn() -> Outcome)] = &[("WideGamutComposition", output::wide_gamut_composition), ("HdrToSdrWithDithering", output::hdr_to_sdr_with_dithering)];

/// Every scene, in the profile's own order.
pub const CASES: &[Case] = &[
	Case { feature: Render2DFeature::PathConstruction, scene: geometry::path_construction },
	Case { feature: Render2DFeature::FillNonZero, scene: geometry::fill_non_zero },
	Case { feature: Render2DFeature::FillEvenOdd, scene: geometry::fill_even_odd },
	Case { feature: Render2DFeature::ShapeRect, scene: geometry::shape_rect },
	Case { feature: Render2DFeature::ShapeRoundedRect, scene: geometry::shape_rounded_rect },
	Case { feature: Render2DFeature::ShapeCircle, scene: geometry::shape_circle },
	Case { feature: Render2DFeature::ShapeEllipse, scene: geometry::shape_ellipse },
	Case { feature: Render2DFeature::ShapeArc, scene: geometry::shape_arc },
	Case { feature: Render2DFeature::ShapeLine, scene: geometry::shape_line },
	Case { feature: Render2DFeature::ShapePolyline, scene: geometry::shape_polyline },
	Case { feature: Render2DFeature::ShapePolygon, scene: geometry::shape_polygon },
	Case { feature: Render2DFeature::StrokeWidth, scene: strokes::stroke_width },
	Case { feature: Render2DFeature::CapButt, scene: strokes::cap_butt },
	Case { feature: Render2DFeature::CapRound, scene: strokes::cap_round },
	Case { feature: Render2DFeature::CapSquare, scene: strokes::cap_square },
	Case { feature: Render2DFeature::JoinMiter, scene: strokes::join_miter },
	Case { feature: Render2DFeature::JoinBevel, scene: strokes::join_bevel },
	Case { feature: Render2DFeature::JoinRound, scene: strokes::join_round },
	Case { feature: Render2DFeature::MiterLimit, scene: strokes::miter_limit },
	Case { feature: Render2DFeature::StrokeDash, scene: strokes::stroke_dash },
	Case { feature: Render2DFeature::AnalyticCoverageFill, scene: antialiasing::analytic_coverage_fill },
	Case { feature: Render2DFeature::AnalyticCoverageStroke, scene: antialiasing::analytic_coverage_stroke },
	Case { feature: Render2DFeature::AnalyticCoverageGlyph, scene: antialiasing::analytic_coverage_glyph },
	Case { feature: Render2DFeature::TransformAffine, scene: transforms::transform_affine },
	Case { feature: Render2DFeature::TransformProjective, scene: transforms::transform_projective },
	Case { feature: Render2DFeature::PaintSolid, scene: paints::paint_solid },
	Case { feature: Render2DFeature::PaintLinearGradient, scene: paints::paint_linear_gradient },
	Case { feature: Render2DFeature::PaintRadialGradient, scene: paints::paint_radial_gradient },
	Case { feature: Render2DFeature::PaintConicGradient, scene: paints::paint_conic_gradient },
	Case { feature: Render2DFeature::PaintImagePattern, scene: paints::paint_image_pattern },
	Case { feature: Render2DFeature::GradientMultiStop, scene: paints::gradient_multi_stop },
	Case { feature: Render2DFeature::SpreadClamp, scene: paints::spread_clamp },
	Case { feature: Render2DFeature::SpreadRepeat, scene: paints::spread_repeat },
	Case { feature: Render2DFeature::SpreadMirror, scene: paints::spread_mirror },
	Case { feature: Render2DFeature::PaintTransform, scene: paints::paint_transform },
	Case { feature: Render2DFeature::LinearLightStops, scene: paints::linear_light_stops },
	Case { feature: Render2DFeature::ClipRect, scene: clipping::clip_rect },
	Case { feature: Render2DFeature::ClipRoundedRect, scene: clipping::clip_rounded_rect },
	Case { feature: Render2DFeature::ClipPath, scene: clipping::clip_path },
	Case { feature: Render2DFeature::ClipNested, scene: clipping::clip_nested },
	Case { feature: Render2DFeature::ClipAlphaMask, scene: clipping::clip_alpha_mask },
	Case { feature: Render2DFeature::ClipInverse, scene: clipping::clip_inverse },
	Case { feature: Render2DFeature::LayerGroupOpacity, scene: layers::layer_group_opacity },
	Case { feature: Render2DFeature::LayerBlendMode, scene: layers::layer_blend_mode },
	Case { feature: Render2DFeature::LayerNesting, scene: layers::layer_nesting },
	Case { feature: Render2DFeature::CompositeClear, scene: compositing::composite_clear },
	Case { feature: Render2DFeature::CompositeSource, scene: compositing::composite_source },
	Case { feature: Render2DFeature::CompositeDestination, scene: compositing::composite_destination },
	Case { feature: Render2DFeature::CompositeSourceOver, scene: compositing::composite_source_over },
	Case { feature: Render2DFeature::CompositeDestinationOver, scene: compositing::composite_destination_over },
	Case { feature: Render2DFeature::CompositeSourceIn, scene: compositing::composite_source_in },
	Case { feature: Render2DFeature::CompositeDestinationIn, scene: compositing::composite_destination_in },
	Case { feature: Render2DFeature::CompositeSourceOut, scene: compositing::composite_source_out },
	Case { feature: Render2DFeature::CompositeDestinationOut, scene: compositing::composite_destination_out },
	Case { feature: Render2DFeature::CompositeSourceAtop, scene: compositing::composite_source_atop },
	Case { feature: Render2DFeature::CompositeDestinationAtop, scene: compositing::composite_destination_atop },
	Case { feature: Render2DFeature::CompositeXor, scene: compositing::composite_xor },
	Case { feature: Render2DFeature::CompositePlus, scene: compositing::composite_plus },
	Case { feature: Render2DFeature::BlendMultiply, scene: compositing::blend_multiply },
	Case { feature: Render2DFeature::BlendScreen, scene: compositing::blend_screen },
	Case { feature: Render2DFeature::BlendOverlay, scene: compositing::blend_overlay },
	Case { feature: Render2DFeature::BlendDarken, scene: compositing::blend_darken },
	Case { feature: Render2DFeature::BlendLighten, scene: compositing::blend_lighten },
	Case { feature: Render2DFeature::BlendColorDodge, scene: compositing::blend_color_dodge },
	Case { feature: Render2DFeature::BlendColorBurn, scene: compositing::blend_color_burn },
	Case { feature: Render2DFeature::BlendHardLight, scene: compositing::blend_hard_light },
	Case { feature: Render2DFeature::BlendSoftLight, scene: compositing::blend_soft_light },
	Case { feature: Render2DFeature::BlendDifference, scene: compositing::blend_difference },
	Case { feature: Render2DFeature::BlendExclusion, scene: compositing::blend_exclusion },
	Case { feature: Render2DFeature::BlendHue, scene: compositing::blend_hue },
	Case { feature: Render2DFeature::BlendSaturation, scene: compositing::blend_saturation },
	Case { feature: Render2DFeature::BlendColor, scene: compositing::blend_color },
	Case { feature: Render2DFeature::BlendLuminosity, scene: compositing::blend_luminosity },
	Case { feature: Render2DFeature::ImageSourceDestRect, scene: images::image_source_dest_rect },
	Case { feature: Render2DFeature::ImageProjective, scene: images::image_projective },
	Case { feature: Render2DFeature::ImageNearest, scene: images::image_nearest },
	Case { feature: Render2DFeature::ImageBilinear, scene: images::image_bilinear },
	Case { feature: Render2DFeature::ImageBicubic, scene: images::image_bicubic },
	Case { feature: Render2DFeature::ImageMipmappedMinification, scene: images::image_mipmapped_minification },
	Case { feature: Render2DFeature::ImageWrapClamp, scene: images::image_wrap_clamp },
	Case { feature: Render2DFeature::ImageWrapRepeat, scene: images::image_wrap_repeat },
	Case { feature: Render2DFeature::ImageWrapMirror, scene: images::image_wrap_mirror },
	Case { feature: Render2DFeature::ImageOpacity, scene: images::image_opacity },
	Case { feature: Render2DFeature::ImageColorSpaceConversion, scene: images::image_color_space_conversion },
	Case { feature: Render2DFeature::FilterGaussianBlur, scene: filters::filter_gaussian_blur },
	Case { feature: Render2DFeature::FilterDropShadow, scene: filters::filter_drop_shadow },
	Case { feature: Render2DFeature::FilterColorMatrix, scene: filters::filter_color_matrix },
	Case { feature: Render2DFeature::FilterComposite, scene: filters::filter_composite },
	Case { feature: Render2DFeature::FilterBlend, scene: filters::filter_blend },
	Case { feature: Render2DFeature::FilterConvolution, scene: filters::filter_convolution },
	Case { feature: Render2DFeature::FilterMorphologyDilate, scene: filters::filter_morphology_dilate },
	Case { feature: Render2DFeature::FilterMorphologyErode, scene: filters::filter_morphology_erode },
	Case { feature: Render2DFeature::FilterDisplacementMap, scene: filters::filter_displacement_map },
	Case { feature: Render2DFeature::FilterCrop, scene: filters::filter_crop },
	Case { feature: Render2DFeature::FilterTile, scene: filters::filter_tile },
	Case { feature: Render2DFeature::FilterBackdrop, scene: filters::filter_backdrop },
	Case { feature: Render2DFeature::GlyphOutlines, scene: text::glyph_outlines },
	Case { feature: Render2DFeature::GlyphGrayscaleMask, scene: text::glyph_grayscale_mask },
	Case { feature: Render2DFeature::GlyphBitmapStrike, scene: text::glyph_bitmap_strike },
	Case { feature: Render2DFeature::GlyphColorLayers, scene: text::glyph_color_layers },
	Case { feature: Render2DFeature::GlyphEmbeddedColorBitmap, scene: text::glyph_embedded_color_bitmap },
	Case { feature: Render2DFeature::GlyphTransform, scene: text::glyph_transform },
	Case { feature: Render2DFeature::GlyphSubpixelPositioning, scene: text::glyph_subpixel_positioning },
	Case { feature: Render2DFeature::QueryPathBoolean, scene: queries::query_path_boolean },
	Case { feature: Render2DFeature::QueryHitTest, scene: queries::query_hit_test },
	Case { feature: Render2DFeature::QueryTightBounds, scene: queries::query_tight_bounds },
	Case { feature: Render2DFeature::QueryStrokeBounds, scene: queries::query_stroke_bounds },
	Case { feature: Render2DFeature::QueryPathLength, scene: queries::query_path_length },
	Case { feature: Render2DFeature::QueryPointAtDistance, scene: queries::query_point_at_distance },
	Case { feature: Render2DFeature::QueryTangentAtDistance, scene: queries::query_tangent_at_distance },
];

#[cfg(test)]
mod tests;
