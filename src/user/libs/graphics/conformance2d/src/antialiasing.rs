//! ANTIALIASING: ANALYTIC coverage, which is a number a scene can state rather than a look.
//!
//! WHAT IS ACTUALLY CHECKED IS THE FRACTION. A half-covered pixel is half covered - not "smooth", not
//! "no jaggies" - so each scene puts an edge at a known fraction of a pixel and reads the alpha back.
//! An implementation that supersampled at four by four would answer in quarters and fail these, which
//! is the point: the profile says ANALYTIC.

use crate::Outcome;
use crate::harness::{draw, draw_with, near, rect_path, white};
use graphics_core::geom::{PointF, RectF};
use render2d::path::{FillRule, PathBuilder, StrokeStyle};

// @covers: AnalyticCoverageFill
/// An edge a quarter of the way through a pixel covers a quarter of it.
pub fn analytic_coverage_fill() -> Outcome {
	let frame = draw(16, 16, |canvas| canvas.fill_path(rect_path(RectF::new(4.25, 4.0, 8.0, 8.0)), white(), FillRule::NonZero))?;
	require!(near(frame.alpha(4, 8), 0.75), "a pixel three quarters covered reads three quarters: {:?}", frame.pixel(4, 8));
	require!(frame.covered(5, 8), "the pixel fully inside is full: {:?}", frame.pixel(5, 8));
	require!(near(frame.alpha(12, 8), 0.25), "and the far edge lands a quarter into its pixel: {:?}", frame.pixel(12, 8));
	Ok(())
}

// @covers: AnalyticCoverageStroke
/// A stroke's edge is antialiased by the same arithmetic: a one-pixel line centred on a pixel
/// boundary is half in each of the two pixels it straddles.
pub fn analytic_coverage_stroke() -> Outcome {
	let mut builder = PathBuilder::new();
	builder.add_line(PointF { x: 0.0, y: 8.0 }, PointF { x: 16.0, y: 8.0 })?;
	let path = builder.finish();
	let style = StrokeStyle { width: 1.0, ..StrokeStyle::default() };
	let frame = draw(16, 16, |canvas| canvas.stroke_path(path, white(), style))?;
	require!(near(frame.alpha(8, 7), 0.5), "half the line is in the pixel above the boundary: {:?}", frame.pixel(8, 7));
	require!(near(frame.alpha(8, 8), 0.5), "and half in the one below: {:?}", frame.pixel(8, 8));
	Ok(())
}

/// A provider whose single glyph is a mask of a STATED coverage, so what the scene checks is that the
/// coverage arrives rather than that a glyph appeared.
struct HalfCovered;

impl soft2d::glyph::GlyphProvider for HalfCovered {
	fn glyph(&self, _key: &font_contract::cache::GlyphCacheKey) -> soft2d::glyph::GlyphImage {
		soft2d::glyph::GlyphImage::Mask { left: 0, top: -4, width: 4, height: 4, coverage: alloc::vec![128u8; 16], mode: font_contract::glyph::RasterisationMode::Grayscale }
	}
}

// @covers: AnalyticCoverageGlyph
/// A glyph's coverage composites as a FRACTION and not as a threshold - and through the profile's own
/// coverage gamma, which is a number rather than a preference: blending coverage linearly makes
/// light-on-dark text look bolder than dark-on-light at the same weight, and that is what gets
/// reported as "the font renders too thin".
pub fn analytic_coverage_glyph() -> Outcome {
	let frame = draw_with(16, 16, &soft2d::target::NoImages, &HalfCovered, |canvas| canvas.draw_glyph_run(crate::text::one_glyph(font_contract::glyph::GlyphKind::GrayscaleMask), white()))?;
	let gamma = graphics_profile::compositing::lcd::COVERAGE_GAMMA as f32;
	let expected = libm::powf(128.0 / 255.0, 1.0 / gamma);
	require!(near(frame.alpha(5, 9), expected), "a mask of 128 composites at {expected} of full: {:?}", frame.pixel(5, 9));
	// AND IT IS NOT A THRESHOLD: partial coverage is neither nothing nor everything.
	require!(frame.alpha(5, 9) > 0 && frame.alpha(5, 9) < 255, "which is between the two: {:?}", frame.pixel(5, 9));
	Ok(())
}
