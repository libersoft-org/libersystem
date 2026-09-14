//! GEOMETRY: what a path is, what a fill rule means, and the shapes the profile names.

use crate::Outcome;
use crate::harness::{draw, rect_path, solid, white};
use graphics_core::geom::{PointF, RectF};
use render2d::path::{FillRule, PathBuilder, StrokeStyle};

// @covers: PathConstruction
/// A path of every verb the profile has - move, line, quad, cubic, close - fills the region it
/// encloses and nothing else.
pub fn path_construction() -> Outcome {
	let mut builder = PathBuilder::new();
	builder.move_to(PointF { x: 4.0, y: 4.0 })?;
	builder.line_to(PointF { x: 28.0, y: 4.0 })?;
	builder.quad_to(PointF { x: 30.0, y: 16.0 }, PointF { x: 28.0, y: 28.0 })?;
	builder.cubic_to(PointF { x: 20.0, y: 30.0 }, PointF { x: 10.0, y: 30.0 }, PointF { x: 4.0, y: 28.0 })?;
	builder.close()?;
	let path = builder.finish();
	let frame = draw(32, 32, |canvas| canvas.fill_path(path, white(), FillRule::NonZero))?;
	require!(frame.covered(16, 16), "the inside of a closed path of every verb is filled: {:?}", frame.pixel(16, 16));
	require!(frame.empty(1, 1), "and the outside is not: {:?}", frame.pixel(1, 1));
	// THE CLOSE IS A SEGMENT AND NOT A HINT: without it the last point would join the first anyway
	// for a fill, so what is checked is that the curved side is where the curve put it.
	// THE CURVE REACHES x = 29 AT ITS MIDDLE while the straight chord between its endpoints is at
	// x = 28, so the pixel whose centre is 28.5 is inside the bulge and outside the chord.
	require!(frame.alpha(28, 16) > 200, "the quadratic bulges outward past the chord: {:?}", frame.pixel(28, 16));
	require!(frame.empty(30, 16), "and no further than the curve itself: {:?}", frame.pixel(30, 16));
	Ok(())
}

/// TWO SQUARES WOUND THE SAME WAY, which is the one drawing that tells the two fill rules apart: the
/// inner one is inside two contours of the same direction, so its winding number is two.
fn two_squares(rule: FillRule) -> Result<crate::harness::Frame, crate::Trouble> {
	let mut builder = PathBuilder::new();
	builder.add_rect(RectF::new(4.0, 4.0, 24.0, 24.0))?;
	builder.add_rect(RectF::new(10.0, 10.0, 12.0, 12.0))?;
	let path = builder.finish();
	draw(32, 32, |canvas| canvas.fill_path(path, white(), rule))
}

// @covers: FillNonZero
/// Under the non-zero rule a winding number of two is inside, so the inner square is filled.
pub fn fill_non_zero() -> Outcome {
	let frame = two_squares(FillRule::NonZero)?;
	require!(frame.covered(16, 16), "a winding number of two is inside under the non-zero rule: {:?}", frame.pixel(16, 16));
	require!(frame.covered(6, 6), "and so is the ring between the squares: {:?}", frame.pixel(6, 6));
	Ok(())
}

// @covers: FillEvenOdd
/// Under the even-odd rule the same drawing is a ring: two crossings is an even number.
pub fn fill_even_odd() -> Outcome {
	let frame = two_squares(FillRule::EvenOdd)?;
	require!(frame.empty(16, 16), "two crossings is outside under the even-odd rule: {:?}", frame.pixel(16, 16));
	require!(frame.covered(6, 6), "and one crossing is inside: {:?}", frame.pixel(6, 6));
	Ok(())
}

// @covers: ShapeRect
/// A rectangle is HALF-OPEN: it covers the pixel at its origin and not the one at its far edge.
pub fn shape_rect() -> Outcome {
	let frame = draw(16, 16, |canvas| canvas.fill_path(rect_path(RectF::new(4.0, 4.0, 8.0, 8.0)), white(), FillRule::NonZero))?;
	require!(frame.covered(4, 4), "the pixel at the origin is covered: {:?}", frame.pixel(4, 4));
	require!(frame.covered(11, 11), "and the last pixel inside it is: {:?}", frame.pixel(11, 11));
	require!(frame.empty(12, 12), "the pixel at the far edge is NOT: {:?}", frame.pixel(12, 12));
	require!(frame.empty(3, 3), "nor the one before the origin: {:?}", frame.pixel(3, 3));
	Ok(())
}

// @covers: ShapeRoundedRect
/// A rounded rectangle has its corners cut to the radius and its edges where the rectangle's are.
pub fn shape_rounded_rect() -> Outcome {
	let mut builder = PathBuilder::new();
	builder.add_rounded_rect(RectF::new(2.0, 2.0, 28.0, 28.0), 8.0, 8.0)?;
	let path = builder.finish();
	let frame = draw(32, 32, |canvas| canvas.fill_path(path, white(), FillRule::NonZero))?;
	require!(frame.covered(16, 3), "the middle of the top edge is at the rectangle's own edge: {:?}", frame.pixel(16, 3));
	require!(frame.empty(3, 3), "the corner is cut away: {:?}", frame.pixel(3, 3));
	require!(frame.covered(16, 16), "and the middle is filled: {:?}", frame.pixel(16, 16));
	Ok(())
}

// @covers: ShapeCircle
/// A circle covers what is inside its radius and nothing outside it.
pub fn shape_circle() -> Outcome {
	let mut builder = PathBuilder::new();
	builder.add_circle(PointF { x: 16.0, y: 16.0 }, 12.0)?;
	let path = builder.finish();
	let frame = draw(32, 32, |canvas| canvas.fill_path(path, white(), FillRule::NonZero))?;
	require!(frame.covered(26, 16), "ten pixels out along an axis is inside a radius of twelve: {:?}", frame.pixel(26, 16));
	require!(frame.covered(16, 6), "on every axis: {:?}", frame.pixel(16, 6));
	// A DIAGONAL AT THE RADIUS IS OUTSIDE, which is the check a square would fail: a shape that
	// covered its corners is not a circle.
	require!(frame.empty(25, 25), "and the diagonal at the radius is outside it: {:?}", frame.pixel(25, 25));
	Ok(())
}

// @covers: ShapeEllipse
/// An ellipse has two different radii, which is what tells it from a circle: it reaches further on
/// one axis than the other.
pub fn shape_ellipse() -> Outcome {
	let mut builder = PathBuilder::new();
	builder.add_ellipse(PointF { x: 16.0, y: 16.0 }, 14.0, 6.0)?;
	let path = builder.finish();
	let frame = draw(32, 32, |canvas| canvas.fill_path(path, white(), FillRule::NonZero))?;
	require!(frame.covered(28, 16), "it reaches fourteen along x: {:?}", frame.pixel(28, 16));
	require!(frame.empty(16, 8), "and only six along y: {:?}", frame.pixel(16, 8));
	require!(frame.covered(16, 12), "which is where it does reach: {:?}", frame.pixel(16, 12));
	Ok(())
}

// @covers: ShapeArc
/// An arc is a PART of an ellipse, so the wedge it closes covers the quadrant it swept and not the
/// three it did not.
pub fn shape_arc() -> Outcome {
	let mut builder = PathBuilder::new();
	builder.move_to(PointF { x: 16.0, y: 16.0 })?;
	// A QUARTER TURN FROM THE +X AXIS, which in a y-down space sweeps toward the BOTTOM of the image.
	builder.add_arc(PointF { x: 16.0, y: 16.0 }, 12.0, 12.0, 0.0, core::f32::consts::FRAC_PI_2)?;
	builder.close()?;
	let path = builder.finish();
	let frame = draw(32, 32, |canvas| canvas.fill_path(path, white(), FillRule::NonZero))?;
	require!(frame.covered(22, 22), "the swept quadrant is filled: {:?}", frame.pixel(22, 22));
	require!(frame.empty(22, 10), "the quadrant above it is not: {:?}", frame.pixel(22, 10));
	require!(frame.empty(10, 22), "nor the one beside it: {:?}", frame.pixel(10, 22));
	Ok(())
}

// @covers: ShapeLine
/// A line has no area, so what is checked is its STROKE: the segment is drawn where the two points
/// are and nowhere else.
pub fn shape_line() -> Outcome {
	let mut builder = PathBuilder::new();
	builder.add_line(PointF { x: 4.0, y: 16.0 }, PointF { x: 28.0, y: 16.0 })?;
	let path = builder.finish();
	let style = StrokeStyle { width: 4.0, ..StrokeStyle::default() };
	let frame = draw(32, 32, |canvas| canvas.stroke_path(path, white(), style))?;
	require!(frame.covered(16, 16), "the line is drawn along its length: {:?}", frame.pixel(16, 16));
	require!(frame.empty(16, 22), "and is four wide rather than filling the surface: {:?}", frame.pixel(16, 22));
	require!(frame.empty(2, 16), "and it starts where it was told to: {:?}", frame.pixel(2, 16));
	Ok(())
}

// @covers: ShapePolyline
/// A polyline is OPEN: every segment between consecutive points is drawn and the closing one is not.
pub fn shape_polyline() -> Outcome {
	let corners = [PointF { x: 6.0, y: 6.0 }, PointF { x: 26.0, y: 6.0 }, PointF { x: 26.0, y: 26.0 }];
	let mut builder = PathBuilder::new();
	builder.add_polyline(&corners)?;
	let path = builder.finish();
	let style = StrokeStyle { width: 4.0, ..StrokeStyle::default() };
	let frame = draw(32, 32, |canvas| canvas.stroke_path(path, white(), style))?;
	require!(frame.covered(16, 6), "the first segment is drawn: {:?}", frame.pixel(16, 6));
	require!(frame.covered(26, 16), "and the second: {:?}", frame.pixel(26, 16));
	require!(frame.empty(16, 16), "and the segment that would CLOSE it is not - that is what open means: {:?}", frame.pixel(16, 16));
	Ok(())
}

// @covers: ShapePolygon
/// A polygon is CLOSED, so it encloses a region that a fill covers.
pub fn shape_polygon() -> Outcome {
	let corners = [PointF { x: 16.0, y: 4.0 }, PointF { x: 28.0, y: 28.0 }, PointF { x: 4.0, y: 28.0 }];
	let mut builder = PathBuilder::new();
	builder.add_polygon(&corners)?;
	let path = builder.finish();
	let frame = draw(32, 32, |canvas| canvas.fill_path(path, solid(1.0, 1.0, 1.0, 1.0), FillRule::NonZero))?;
	require!(frame.covered(16, 20), "the triangle's inside is filled: {:?}", frame.pixel(16, 20));
	require!(frame.empty(5, 6), "and the corner outside it is not: {:?}", frame.pixel(5, 6));
	Ok(())
}
