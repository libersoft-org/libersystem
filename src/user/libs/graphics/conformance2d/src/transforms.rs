//! TRANSFORMS: affine, and PROJECTIVE - which is the one an implementation quietly leaves out.

use crate::Outcome;
use crate::harness::{draw, rect_path, white};
use graphics_core::geom::RectF;
use render2d::path::FillRule;
use render2d::transform::Transform;

// @covers: TransformAffine
/// A transform moves the shape to where the arithmetic says, and the drawing is what is transformed
/// rather than the coordinates being ignored.
pub fn transform_affine() -> Outcome {
	let frame = draw(32, 32, |canvas| {
		canvas.concat_transform(&Transform::translate(12.0, 8.0));
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 8.0, 8.0)), white(), FillRule::NonZero)
	})?;
	require!(frame.covered(14, 10), "the shape is where the translation put it: {:?}", frame.pixel(14, 10));
	require!(frame.empty(2, 2), "and not where it was recorded: {:?}", frame.pixel(2, 2));

	// AND A SCALE SCALES, which is what tells a transform that is applied from one that is rounded to
	// a translation.
	let scaled = draw(32, 32, |canvas| {
		canvas.concat_transform(&Transform::scale(3.0, 1.0));
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 8.0, 8.0)), white(), FillRule::NonZero)
	})?;
	require!(scaled.covered(22, 4), "a threefold scale reaches three times as far: {:?}", scaled.pixel(22, 4));
	require!(scaled.empty(25, 4), "and no further: {:?}", scaled.pixel(25, 4));
	Ok(())
}

// @covers: TransformProjective
/// A PROJECTIVE transform is not an affine one with extra zeroes: the perspective terms make a square
/// a trapezium, wider at one end than the other. An implementation that dropped the third row would
/// draw a parallelogram and pass every affine check.
pub fn transform_projective() -> Outcome {
	// A perspective that shrinks with x: `w = 1 + x/64`, so the far side of a wide square is narrower.
	let mut transform = Transform::IDENTITY;
	transform.m[2][0] = 1.0 / 64.0;
	let frame = draw(64, 64, |canvas| {
		canvas.concat_transform(&transform);
		canvas.fill_path(rect_path(RectF::new(2.0, 2.0, 60.0, 60.0)), white(), FillRule::NonZero)
	})?;
	// THE NEAR EDGE IS TALLER THAN THE FAR ONE, measured as the count of covered pixels in a column.
	let height_at = |x: u32| (0..64).filter(|y| frame.alpha(x, *y) > 128).count();
	let (near_edge, far_edge) = (height_at(4), height_at(40));
	require!(near_edge > far_edge + 4, "the perspective divide makes the far side narrower: {near_edge} against {far_edge}");
	Ok(())
}
