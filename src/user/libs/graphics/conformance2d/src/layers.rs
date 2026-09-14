//! LAYERS: a group's opacity, a group's blend mode, and the nesting the profile bounds.

use crate::Outcome;
use crate::harness::{draw, near, rect_path, solid, white};
use graphics_core::geom::RectF;
use render2d::blend::BlendMode;
use render2d::path::FillRule;

// @covers: LayerGroupOpacity
/// GROUP OPACITY APPLIES ONCE, TO THE GROUP. Two overlapping opaque children at half opacity are a
/// half-transparent SHAPE; applying the opacity to each child instead makes the overlap darker, which
/// is the difference between a fading dialogue and a fading dialogue with a seam down the middle.
pub fn layer_group_opacity() -> Outcome {
	let frame = draw(32, 32, |canvas| {
		canvas.begin_layer(None, 0.5, BlendMode::Normal, None)?;
		canvas.fill_path(rect_path(RectF::new(4.0, 4.0, 16.0, 16.0)), white(), FillRule::NonZero)?;
		canvas.fill_path(rect_path(RectF::new(12.0, 4.0, 16.0, 16.0)), white(), FillRule::NonZero)?;
		canvas.end_layer()
	})?;
	require!(near(frame.alpha(8, 8), 0.5), "a child is at the group's opacity: {:?}", frame.pixel(8, 8));
	require!(near(frame.alpha(16, 8), 0.5), "and the OVERLAP is at the same opacity, not at three quarters: {:?}", frame.pixel(16, 8));
	Ok(())
}

// @covers: LayerBlendMode
/// A LAYER BLENDS AS A WHOLE with what is under it, which is not the same as each of its children
/// blending: two overlapping multiplied children would multiply twice where they overlap.
pub fn layer_blend_mode() -> Outcome {
	let frame = draw(32, 32, |canvas| {
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 32.0, 32.0)), solid(0.5, 0.5, 0.5, 1.0), FillRule::NonZero)?;
		canvas.begin_layer(None, 1.0, BlendMode::Multiply, None)?;
		canvas.fill_path(rect_path(RectF::new(4.0, 4.0, 16.0, 16.0)), solid(0.5, 0.5, 0.5, 1.0), FillRule::NonZero)?;
		canvas.fill_path(rect_path(RectF::new(12.0, 4.0, 16.0, 16.0)), solid(0.5, 0.5, 0.5, 1.0), FillRule::NonZero)?;
		canvas.end_layer()
	})?;
	require!(near(frame.pixel(8, 8)[0], 0.25), "half multiplied by half is a quarter: {:?}", frame.pixel(8, 8));
	require!(near(frame.pixel(16, 8)[0], 0.25), "and the overlap multiplied ONCE is still a quarter: {:?}", frame.pixel(16, 8));
	require!(near(frame.pixel(28, 28)[0], 0.5), "outside the layer the backdrop is untouched: {:?}", frame.pixel(28, 28));
	Ok(())
}

// @covers: LayerNesting
/// LAYERS NEST, and each level's opacity multiplies the ones inside it - which is what makes a fading
/// panel inside a fading dialogue fade twice rather than being clamped at one.
pub fn layer_nesting() -> Outcome {
	let frame = draw(32, 32, |canvas| {
		canvas.begin_layer(None, 0.5, BlendMode::Normal, None)?;
		canvas.begin_layer(None, 0.5, BlendMode::Normal, None)?;
		canvas.fill_path(rect_path(RectF::new(4.0, 4.0, 24.0, 24.0)), white(), FillRule::NonZero)?;
		canvas.end_layer()?;
		canvas.end_layer()
	})?;
	require!(near(frame.alpha(16, 16), 0.25), "a half inside a half is a quarter: {:?}", frame.pixel(16, 16));
	Ok(())
}
