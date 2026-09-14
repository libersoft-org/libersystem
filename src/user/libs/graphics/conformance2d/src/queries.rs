//! QUERIES: the geometry answers a rasteriser never sees.
//!
//! THESE ARE `Render2D`-OWNED FEATURES and they are in the profile for the same reason the drawing
//! ones are: an application that can draw a path and cannot ask where it is, how long it is or
//! whether a click landed on it has to carry a second geometry library to find out.

use crate::Outcome;
use crate::harness::rect_path;
use graphics_core::geom::{PointF, RectF};
use render2d::boolean::{Operation, combine};
use render2d::path::{Cap, FillRule, PathBuilder, StrokeStyle};
use render2d::query;

/// Two overlapping squares, which is enough to tell every boolean operation apart.
fn overlapping() -> (render2d::path::Path, render2d::path::Path) {
	(rect_path(RectF::new(0.0, 0.0, 10.0, 10.0)), rect_path(RectF::new(5.0, 5.0, 10.0, 10.0)))
}

// @covers: QueryPathBoolean
/// Union, intersection and difference of two squares, each checked where the three answers differ.
pub fn query_path_boolean() -> Outcome {
	let (left, right) = overlapping();
	let of = |operation: Operation| combine(&left, FillRule::NonZero, &right, FillRule::NonZero, operation);
	let union = of(Operation::Union).map_err(|error| crate::Trouble::Unsupported(alloc::format!("a union was refused: {error:?}")))?;
	let intersection = of(Operation::Intersection).map_err(|error| crate::Trouble::Unsupported(alloc::format!("an intersection was refused: {error:?}")))?;
	let difference = of(Operation::Difference).map_err(|error| crate::Trouble::Unsupported(alloc::format!("a difference was refused: {error:?}")))?;

	let inside = PointF { x: 2.0, y: 2.0 };
	let shared = PointF { x: 7.0, y: 7.0 };
	let other = PointF { x: 12.0, y: 12.0 };
	require!(union.contains(inside, FillRule::NonZero) && union.contains(other, FillRule::NonZero), "a union holds both operands");
	require!(intersection.contains(shared, FillRule::NonZero), "an intersection holds the overlap");
	require!(!intersection.contains(inside, FillRule::NonZero), "and nothing else");
	require!(difference.contains(inside, FillRule::NonZero), "a difference holds the first operand");
	require!(!difference.contains(shared, FillRule::NonZero), "with the second taken out of it");
	Ok(())
}

// @covers: QueryHitTest
/// A hit test answers for a FILL under a stated rule and for a STROKE at a stated width - the second
/// being the one a naive implementation leaves out, so that a click on a line never lands.
pub fn query_hit_test() -> Outcome {
	let mut builder = PathBuilder::new();
	builder.add_rect(RectF::new(0.0, 0.0, 10.0, 10.0))?;
	builder.add_rect(RectF::new(3.0, 3.0, 4.0, 4.0))?;
	let path = builder.finish();
	require!(query::fill_contains(&path, FillRule::NonZero, PointF { x: 5.0, y: 5.0 }, None), "the inner square is inside under the non-zero rule");
	require!(!query::fill_contains(&path, FillRule::EvenOdd, PointF { x: 5.0, y: 5.0 }, None), "and outside under the even-odd one");
	require!(!query::fill_contains(&path, FillRule::NonZero, PointF { x: 12.0, y: 5.0 }, None), "a point outside the whole shape is outside");

	let mut line = PathBuilder::new();
	line.add_line(PointF { x: 0.0, y: 0.0 }, PointF { x: 10.0, y: 0.0 })?;
	let line = line.finish();
	let style = StrokeStyle { width: 4.0, cap: Cap::Butt, ..StrokeStyle::default() };
	require!(query::stroke_contains(&line, &style, PointF { x: 5.0, y: 1.5 }, None), "a point within half the width of a line is on it");
	require!(!query::stroke_contains(&line, &style, PointF { x: 5.0, y: 3.0 }, None), "and one beyond it is not");
	Ok(())
}

// @covers: QueryTightBounds
/// TIGHT BOUNDS ARE THE CURVE'S OWN and not its control points'. A control point is often well
/// outside the curve, so the loose bound of a rounded rectangle is bigger than the shape - which is a
/// scroll area that is too large and a damage rectangle that redraws too much.
pub fn query_tight_bounds() -> Outcome {
	let mut builder = PathBuilder::new();
	builder.move_to(PointF { x: 0.0, y: 0.0 })?;
	builder.cubic_to(PointF { x: 0.0, y: 100.0 }, PointF { x: 10.0, y: 100.0 }, PointF { x: 10.0, y: 0.0 })?;
	let path = builder.finish();
	let loose = path.bounds().ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("a path with points has bounds")))?;
	let tight = path.tight_bounds().ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("and tight ones")))?;
	require!(loose.bottom() > 99.0, "the loose bound reaches the control points: {loose:?}");
	// THE CURVE'S OWN MAXIMUM. `y(t) = 300t(1-t)` for these control points, which peaks at `t = 1/2`
	// and reaches 75 - a quarter less than the control polygon's hundred.
	require!((tight.bottom() - 75.0).abs() < 0.5, "the tight bound is the curve's own maximum of 75: {tight:?}");
	Ok(())
}

// @covers: QueryStrokeBounds
/// A stroke's bounds are the path's grown by what the stroke adds - which is what a damage rectangle
/// for a stroked shape needs, and is not the path's bounds.
pub fn query_stroke_bounds() -> Outcome {
	let mut builder = PathBuilder::new();
	builder.add_line(PointF { x: 10.0, y: 10.0 }, PointF { x: 20.0, y: 10.0 })?;
	let path = builder.finish();
	let style = StrokeStyle { width: 6.0, cap: Cap::Butt, ..StrokeStyle::default() };
	let bounds = query::stroke_bounds(&path, &style, None).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("a stroked line has bounds")))?;
	require!(bounds.y <= 7.0 && bounds.bottom() >= 13.0, "half the width on each side of the line: {bounds:?}");
	require!(bounds.x <= 10.0 && bounds.right() >= 20.0, "and the length of it: {bounds:?}");
	Ok(())
}

// @covers: QueryPathLength
/// The length of a path is the length of the segments in it, which for a three-four-five triangle's
/// two legs is seven.
pub fn query_path_length() -> Outcome {
	let mut builder = PathBuilder::new();
	builder.move_to(PointF { x: 0.0, y: 0.0 })?;
	builder.line_to(PointF { x: 3.0, y: 0.0 })?;
	builder.line_to(PointF { x: 3.0, y: 4.0 })?;
	let path = builder.finish();
	let length = query::length(&path, None);
	require!((length - 7.0).abs() < 0.01, "three across and four down is seven, not {length}");
	Ok(())
}

// @covers: QueryPointAtDistance
/// The point a stated distance along the path - which is what places a label on a curve, an arrow
/// head on a route and a dash on a dashed outline.
pub fn query_point_at_distance() -> Outcome {
	let mut builder = PathBuilder::new();
	builder.move_to(PointF { x: 0.0, y: 0.0 })?;
	builder.line_to(PointF { x: 3.0, y: 0.0 })?;
	builder.line_to(PointF { x: 3.0, y: 4.0 })?;
	let path = builder.finish();
	let (point, _) = query::at_distance(&path, 5.0, None).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("five is within a path of seven")))?;
	require!((point.x - 3.0).abs() < 0.01 && (point.y - 2.0).abs() < 0.01, "five along is two down the second leg, not {point:?}");
	// PAST THE END IS THE END, with the last segment's direction. Answering nothing would make a dash
	// or a label that ran one rounding unit past the path disappear instead of finishing at its tip.
	let (tip, _) = query::at_distance(&path, 9.0, None).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("past the end is the end and not nothing")))?;
	require!((tip.x - 3.0).abs() < 0.01 && (tip.y - 4.0).abs() < 0.01, "past the end is the last point, not {tip:?}");
	Ok(())
}

// @covers: QueryTangentAtDistance
/// The DIRECTION at that distance, which is the other half of placing something on a path: a label
/// without it is upright on a curve that is not.
pub fn query_tangent_at_distance() -> Outcome {
	let mut builder = PathBuilder::new();
	builder.move_to(PointF { x: 0.0, y: 0.0 })?;
	builder.line_to(PointF { x: 3.0, y: 0.0 })?;
	builder.line_to(PointF { x: 3.0, y: 4.0 })?;
	let path = builder.finish();
	let (_, along_first) = query::at_distance(&path, 1.0, None).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("one is on the first leg")))?;
	let (_, along_second) = query::at_distance(&path, 5.0, None).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("five is on the second")))?;
	require!((along_first.x - 1.0).abs() < 0.01 && along_first.y.abs() < 0.01, "the first leg runs along x: {along_first:?}");
	require!(along_second.x.abs() < 0.01 && (along_second.y - 1.0).abs() < 0.01, "and the second down y: {along_second:?}");
	Ok(())
}
