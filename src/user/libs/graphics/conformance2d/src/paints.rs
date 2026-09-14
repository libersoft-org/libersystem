//! PAINTS: what a shape is filled WITH - solid, three kinds of gradient, an image pattern, and the
//! stops, spreads and transforms that apply to them.
//!
//! A GRADIENT'S PASS CONDITION IS ITS VALUE AT A POINT, computed from the stops and the geometry
//! rather than looked at. "It goes from red to blue" is true of a gradient with the wrong shape, the
//! wrong spread and the wrong interpolation space.

use crate::Outcome;
use crate::harness::{draw, draw_with, near, rect_path, solid};
use graphics_core::geom::{PointF, RectF};
use render2d::paint::{Color, GradientStop, ImageQuality, Paint, SpreadMode};
use render2d::path::FillRule;
use render2d::transform::Transform;

/// Black at one end, white at the other, in LINEAR light - so the value at a point is the fraction
/// along, with nothing in between to explain.
fn black_to_white() -> alloc::vec::Vec<GradientStop> {
	alloc::vec![
		GradientStop { offset: 0.0, color: Color::new(0.0, 0.0, 0.0, 1.0, graphics_core::ColorSpace::SrgbLinear) },
		GradientStop { offset: 1.0, color: Color::new(1.0, 1.0, 1.0, 1.0, graphics_core::ColorSpace::SrgbLinear) },
	]
}

// @covers: PaintSolid
/// A solid paint is its colour, everywhere inside the shape.
pub fn paint_solid() -> Outcome {
	let frame = draw(16, 16, |canvas| canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 16.0, 16.0)), solid(0.2, 0.6, 0.8, 1.0), FillRule::NonZero))?;
	let pixel = frame.pixel(8, 8);
	require!(near(pixel[0], 0.2) && near(pixel[1], 0.6) && near(pixel[2], 0.8), "a solid paint is the colour it was given: {pixel:?}");
	require!(near(frame.pixel(1, 14)[1], 0.6), "in every pixel of the shape and not only the middle: {:?}", frame.pixel(1, 14));
	Ok(())
}

// @covers: PaintLinearGradient
/// A linear gradient's value at a point is the fraction of the way along its AXIS - projected onto
/// it, so a point off the axis has the value of its projection.
pub fn paint_linear_gradient() -> Outcome {
	let frame = draw(32, 32, |canvas| {
		let stops = canvas.resources().add_stops(black_to_white())?;
		let paint = Paint::Linear { from: PointF { x: 0.0, y: 0.0 }, to: PointF { x: 32.0, y: 0.0 }, stops, spread: SpreadMode::Clamp, transform: Transform::IDENTITY };
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 32.0, 32.0)), paint, FillRule::NonZero)
	})?;
	require!(near(frame.pixel(8, 16)[0], 8.5 / 32.0), "a quarter along the axis is a quarter of the way: {:?}", frame.pixel(8, 16));
	require!(near(frame.pixel(24, 16)[0], 24.5 / 32.0), "and three quarters along is three quarters: {:?}", frame.pixel(24, 16));
	require!(frame.pixel(8, 2)[0] == frame.pixel(8, 30)[0], "a point off the axis takes its projection's value: {:?} against {:?}", frame.pixel(8, 2), frame.pixel(8, 30));
	Ok(())
}

// @covers: PaintRadialGradient
/// A radial gradient's value depends on the DISTANCE from its centre, so two points the same distance
/// away in different directions are the same colour and a nearer one is not.
pub fn paint_radial_gradient() -> Outcome {
	let frame = draw(32, 32, |canvas| {
		let stops = canvas.resources().add_stops(black_to_white())?;
		let paint = Paint::Radial { from: PointF { x: 16.0, y: 16.0 }, from_radius: 0.0, to: PointF { x: 16.0, y: 16.0 }, to_radius: 16.0, stops, spread: SpreadMode::Clamp, transform: Transform::IDENTITY };
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 32.0, 32.0)), paint, FillRule::NonZero)
	})?;
	let (right, below) = (frame.pixel(24, 16)[0], frame.pixel(16, 24)[0]);
	require!((right as i32 - below as i32).abs() <= 2, "the same distance in two directions is the same colour: {right} against {below}");
	require!(frame.pixel(18, 16)[0] < right, "and nearer the centre is nearer the first stop: {:?}", frame.pixel(18, 16));
	require!(near(right, 8.5 / 16.0), "half the radius out is half way along the stops: {right}");
	Ok(())
}

// @covers: PaintConicGradient
/// A conic gradient's value depends on the ANGLE about its centre, which is what makes it the paint a
/// colour wheel and a loading spinner are: the same distance out is a different colour in a different
/// direction.
pub fn paint_conic_gradient() -> Outcome {
	let frame = draw(32, 32, |canvas| {
		let stops = canvas.resources().add_stops(black_to_white())?;
		let paint = Paint::Conic { centre: PointF { x: 16.0, y: 16.0 }, start_angle: 0.0, end_angle: core::f32::consts::TAU, stops, spread: SpreadMode::Clamp, transform: Transform::IDENTITY };
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 32.0, 32.0)), paint, FillRule::NonZero)
	})?;
	// THE EXPECTATION IS COMPUTED FROM THE PROBE'S OWN GEOMETRY, because a pixel CENTRE is at a half
	// and the angle there is not the angle at the corner: an expectation written as "a quarter" would
	// be a tolerance in disguise.
	let fraction_at = |x: u32, y: u32| {
		let (dx, dy) = (x as f32 + 0.5 - 16.0, y as f32 + 0.5 - 16.0);
		let angle = libm::atan2f(dy, dx);
		let turns = angle / core::f32::consts::TAU;
		if turns < 0.0 { turns + 1.0 } else { turns }
	};
	for (x, y) in [(16u32, 28u32), (4, 16), (16, 4), (28, 16)] {
		let expected = fraction_at(x, y);
		require!(near(frame.pixel(x, y)[0], expected), "at ({x},{y}) the angle is {expected} of a turn round and the pixel is {:?}", frame.pixel(x, y));
	}
	// AND IT IS AN ANGLE AND NOT A RADIUS: two points the same distance from the centre in different
	// directions are different colours, which a radial gradient's are not.
	require!(frame.pixel(28, 16)[0] != frame.pixel(16, 28)[0], "two points the same distance out differ");
	Ok(())
}

// @covers: PaintImagePattern
/// An image pattern fills a shape with an image rather than a colour, tiled by its spread.
pub fn paint_image_pattern() -> Outcome {
	let source = crate::images::image_of(2, 1, graphics_core::ColorSpace::SrgbLinear, |x, _| if x == 0 { graphics_core::pixel::Rgba::new(1.0, 0.0, 0.0, 1.0) } else { graphics_core::pixel::Rgba::new(0.0, 0.0, 1.0, 1.0) });
	let images = crate::images::OneImage(source);
	let frame = draw_with(32, 8, &images, &soft2d::glyph::NoGlyphs, |canvas| {
		let handle = canvas.resources().add_image(crate::images::record())?;
		let paint = Paint::Image { image: handle, source: RectF::new(0.0, 0.0, 2.0, 1.0), quality: ImageQuality::Nearest, spread: SpreadMode::Repeat, transform: Transform::scale(8.0, 8.0) };
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 32.0, 8.0)), paint, FillRule::NonZero)
	})?;
	require!(frame.pixel(4, 4)[0] > 200, "the pattern's first texel fills the first tile: {:?}", frame.pixel(4, 4));
	require!(frame.pixel(12, 4)[2] > 200, "and its second the next: {:?}", frame.pixel(12, 4));
	Ok(())
}

// @covers: GradientMultiStop
/// A gradient has as many stops as it says, and each one is EXACTLY its colour at its own offset -
/// which is what a two-stop implementation that interpolates between the ends gets wrong in the
/// middle.
pub fn gradient_multi_stop() -> Outcome {
	let frame = draw(32, 8, |canvas| {
		let stops = canvas.resources().add_stops(alloc::vec![
			GradientStop { offset: 0.0, color: Color::new(1.0, 0.0, 0.0, 1.0, graphics_core::ColorSpace::SrgbLinear) },
			GradientStop { offset: 0.5, color: Color::new(0.0, 1.0, 0.0, 1.0, graphics_core::ColorSpace::SrgbLinear) },
			GradientStop { offset: 1.0, color: Color::new(0.0, 0.0, 1.0, 1.0, graphics_core::ColorSpace::SrgbLinear) },
		])?;
		let paint = Paint::Linear { from: PointF { x: 0.0, y: 0.0 }, to: PointF { x: 32.0, y: 0.0 }, stops, spread: SpreadMode::Clamp, transform: Transform::IDENTITY };
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 32.0, 8.0)), paint, FillRule::NonZero)
	})?;
	let middle = frame.pixel(16, 4);
	require!(middle[1] > 200 && middle[0] < 60 && middle[2] < 60, "the middle stop's own colour appears at its offset: {middle:?}");
	require!(frame.pixel(8, 4)[0] > 80 && frame.pixel(8, 4)[1] > 80, "and between two stops the two colours mix: {:?}", frame.pixel(8, 4));
	Ok(())
}

/// A gradient over the middle third of a wide rectangle, so most of what is drawn is what the spread
/// mode decides.
fn spread(mode: SpreadMode) -> Result<crate::harness::Frame, crate::Trouble> {
	draw(48, 8, |canvas| {
		let stops = canvas.resources().add_stops(black_to_white())?;
		let paint = Paint::Linear { from: PointF { x: 16.0, y: 0.0 }, to: PointF { x: 32.0, y: 0.0 }, stops, spread: mode, transform: Transform::IDENTITY };
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 48.0, 8.0)), paint, FillRule::NonZero)
	})
}

// @covers: SpreadClamp
/// Clamped, the end stops continue for ever.
pub fn spread_clamp() -> Outcome {
	let frame = spread(SpreadMode::Clamp)?;
	require!(frame.pixel(4, 4)[0] == 0, "before the start the first stop is held: {:?}", frame.pixel(4, 4));
	require!(frame.pixel(44, 4)[0] == 255, "and past the end the last: {:?}", frame.pixel(44, 4));
	Ok(())
}

// @covers: SpreadRepeat
/// Repeated, the gradient starts again at the same end it started at - so just past the end it is
/// dark again, which is the discontinuity that tells repeat from mirror.
pub fn spread_repeat() -> Outcome {
	let frame = spread(SpreadMode::Repeat)?;
	require!(frame.pixel(34, 4)[0] < 64, "just past the end the gradient restarts from its first stop: {:?}", frame.pixel(34, 4));
	require!(frame.pixel(46, 4)[0] > 190, "and runs up to its last again: {:?}", frame.pixel(46, 4));
	Ok(())
}

// @covers: SpreadMirror
/// Mirrored, it runs BACKWARDS past the end, so there is no discontinuity at all.
pub fn spread_mirror() -> Outcome {
	let frame = spread(SpreadMode::Mirror)?;
	require!(frame.pixel(34, 4)[0] > 190, "just past the end the gradient continues from its last stop: {:?}", frame.pixel(34, 4));
	require!(frame.pixel(46, 4)[0] < 64, "and runs back down to its first: {:?}", frame.pixel(46, 4));
	Ok(())
}

// @covers: PaintTransform
/// A PAINT HAS ITS OWN TRANSFORM, separate from the shape's - which is what lets a pattern stay still
/// while the thing it fills moves, and a gradient rotate inside a shape that does not.
pub fn paint_transform() -> Outcome {
	let frame = draw(32, 32, |canvas| {
		let stops = canvas.resources().add_stops(black_to_white())?;
		// A GRADIENT ALONG X, ROTATED A QUARTER TURN BY ITS OWN TRANSFORM, so it runs along y.
		let paint = Paint::Linear { from: PointF { x: 0.0, y: 0.0 }, to: PointF { x: 32.0, y: 0.0 }, stops, spread: SpreadMode::Clamp, transform: Transform { m: [[0.0, -1.0, 32.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]] } };
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 32.0, 32.0)), paint, FillRule::NonZero)
	})?;
	let along_y = (frame.pixel(16, 4)[0] as i32 - frame.pixel(16, 28)[0] as i32).abs();
	let along_x = (frame.pixel(4, 16)[0] as i32 - frame.pixel(28, 16)[0] as i32).abs();
	require!(along_y > 100, "the paint's transform turned the gradient to run along y: {along_y}");
	require!(along_x < 8, "and it no longer runs along x: {along_x}");
	Ok(())
}

// @covers: LinearLightStops
/// STOPS ARE INTERPOLATED IN LINEAR LIGHT and not in the encoding. Half way between black and white
/// is half the LIGHT, which is why a gradient interpolated in sRGB looks dark in the middle - and it
/// is a difference of seventy parts in 255, not a subtlety.
pub fn linear_light_stops() -> Outcome {
	let frame = draw(32, 8, |canvas| {
		let stops = canvas.resources().add_stops(black_to_white())?;
		let paint = Paint::Linear { from: PointF { x: 0.0, y: 0.0 }, to: PointF { x: 32.0, y: 0.0 }, stops, spread: SpreadMode::Clamp, transform: Transform::IDENTITY };
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 32.0, 8.0)), paint, FillRule::NonZero)
	})?;
	// The target is linear, so half the light is stored as half of 255 - and an implementation that
	// interpolated the sRGB encodings would store about 188 here.
	let middle = frame.pixel(16, 4)[0];
	require!(near(middle, 16.5 / 32.0), "the midpoint of a black-to-white gradient is half the LIGHT: {middle}");
	Ok(())
}
