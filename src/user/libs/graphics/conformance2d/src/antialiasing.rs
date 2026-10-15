//! ANTIALIASING: ANALYTIC coverage, which is a number a scene can state rather than a look.
//!
//! WHAT IS ACTUALLY CHECKED IS THE FRACTION. A half-covered pixel is half covered - not "smooth", not
//! "no jaggies" - so each scene puts an edge at a known fraction of a pixel and reads the alpha back.
//! An implementation that supersampled at four by four would answer in quarters and fail these, which
//! is the point: the profile says ANALYTIC.

use crate::Outcome;
use crate::harness::{WIDE, byte, draw, draw_with, every_pixel, near, rect_path, stored, threshold, white};
use graphics_core::geom::{PointF, RectF};
use render2d::path::{FillRule, PathBuilder, StrokeStyle};

// @covers: AnalyticCoverageFill
/// An edge a quarter of the way through a pixel covers a quarter of it - at EVERY pixel, against the frozen
/// coverage threshold.
///
/// A RECTANGLE IS THE SHAPE WHOSE ANALYTIC AREA A SCENE CAN STATE: its overlap with a pixel is the product of
/// two one-dimensional overlaps, computed here from the rectangle and not from the backend. So every pixel of
/// a target several tiles wide is compared - exactly where the area is nothing or everything, within the
/// frozen 2/255 where it is a fraction, and the mean over those within the frozen 1/255, which is the bound
/// that catches a bias every pixel is one step inside. The edges are at four phases against the grid, and
/// the far ones never at the near ones'.
pub fn analytic_coverage_fill() -> Outcome {
	let overlap = |pixel: u32, from: f64, to: f64| (to.min(pixel as f64 + 1.0) - from.max(pixel as f64)).max(0.0);
	let (mut partial, mut total) = (0usize, 0.0f64);
	for phase in [0.0f64, 0.25, 0.375, 0.875] {
		let (left, top, width, height) = (40.0 + phase, 36.5 + phase, 60.5, 50.75);
		let frame = draw(WIDE, WIDE, |canvas| canvas.fill_path(rect_path(RectF::new(left as f32, top as f32, width as f32, height as f32)), white(), FillRule::NonZero))?;
		let area = |x: u32, y: u32| overlap(x, left, left + width) * overlap(y, top, top + height);
		// NOTHING AND EVERYTHING ARE COMPARED EXACTLY, the colour of a partly covered pixel too: a white
		// shape is white wherever it is at all.
		every_pixel(&frame, |x, y| match area(x, y) {
			value if value <= 0.0 => [0; 4],
			value if value >= 1.0 => [stored(1.0, x, y), stored(1.0, x, y), stored(1.0, x, y), byte(1.0)],
			_ => {
				let mut pixel = frame.pixel(x, y);
				pixel[..3].copy_from_slice(&[stored(1.0, x, y); 3]);
				pixel
			}
		})?;
		for y in 0..WIDE {
			for x in 0..WIDE {
				let wanted = area(x, y);
				if wanted <= 0.0 || wanted >= 1.0 {
					continue;
				}
				let difference = (frame.alpha(x, y) as f64 / 255.0 - wanted).abs();
				require!(difference <= threshold::COVERAGE_PER_PIXEL, "pixel ({x}, {y}) at phase {phase} is {} covered by area and drawn at {:?}, which is {difference} out against the frozen 2/255", wanted, frame.pixel(x, y));
				partial += 1;
				total += difference;
			}
		}
	}
	require!(partial > 0, "the edges' phases leave pixels partly covered to compare");
	let mean = total / partial as f64;
	require!(mean <= threshold::COVERAGE_MEAN, "the mean difference over {partial} partly covered pixels is {mean}, against the frozen 1/255");
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
