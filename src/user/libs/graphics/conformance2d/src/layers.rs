//! LAYERS: a group's opacity, a group's blend mode, and the nesting the profile bounds.
//!
//! EACH SCENE IS DRAWN ACROSS SEVERAL TILES AND COMPARED WHOLE. Every edge is a whole pixel, so every pixel
//! of the target is covered wholly or not at all and the frozen edge policy compares every one of them
//! EXACTLY - which is what says a group is composited once, everywhere, and not once per tile it was cut
//! into.

use crate::Outcome;
use crate::harness::{WIDE, byte, draw, every_pixel, rect_path, solid, stored, white};
use graphics_core::geom::RectF;
use render2d::blend::BlendMode;
use render2d::path::FillRule;

/// Whether a pixel lies in a rectangle given as `(left, top, right, bottom)`.
fn within(x: u32, y: u32, (left, top, right, bottom): (u32, u32, u32, u32)) -> bool {
	(left..right).contains(&x) && (top..bottom).contains(&y)
}

fn rect((left, top, right, bottom): (u32, u32, u32, u32)) -> render2d::path::Path {
	rect_path(RectF::new(left as f32, top as f32, (right - left) as f32, (bottom - top) as f32))
}

/// Two overlapping squares, across the tile boundaries on both axes.
const ONE: (u32, u32, u32, u32) = (24, 24, 96, 96);
const OTHER: (u32, u32, u32, u32) = (56, 40, 136, 120);

/// A straight colour at an alpha, as a fully covered pixel stores it.
fn holding(x: u32, y: u32, colour: f32, alpha: f32) -> [u8; 4] {
	[stored(colour, x, y), stored(colour, x, y), stored(colour, x, y), byte(alpha)]
}

// @covers: LayerGroupOpacity
/// GROUP OPACITY APPLIES ONCE, TO THE GROUP. Two overlapping opaque children at half opacity are a
/// half-transparent SHAPE; applying the opacity to each child instead makes the overlap darker, which
/// is the difference between a fading dialogue and a fading dialogue with a seam down the middle.
pub fn layer_group_opacity() -> Outcome {
	let frame = draw(WIDE, WIDE, |canvas| {
		canvas.begin_layer(None, 0.5, BlendMode::Normal, None)?;
		canvas.fill_path(rect(ONE), white(), FillRule::NonZero)?;
		canvas.fill_path(rect(OTHER), white(), FillRule::NonZero)?;
		canvas.end_layer()
	})?;
	// THE OVERLAP IS AT THE SAME HALF AS EITHER CHILD, not at three quarters, and nothing is drawn outside.
	every_pixel(&frame, |x, y| if within(x, y, ONE) || within(x, y, OTHER) { holding(x, y, 1.0, 0.5) } else { [0; 4] })
}

// @covers: LayerBlendMode
/// A LAYER BLENDS AS A WHOLE with what is under it, which is not the same as each of its children
/// blending: two overlapping multiplied children would multiply twice where they overlap.
pub fn layer_blend_mode() -> Outcome {
	let frame = draw(WIDE, WIDE, |canvas| {
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, WIDE as f32, WIDE as f32)), solid(0.5, 0.5, 0.5, 1.0), FillRule::NonZero)?;
		canvas.begin_layer(None, 1.0, BlendMode::Multiply, None)?;
		canvas.fill_path(rect(ONE), solid(0.5, 0.5, 0.5, 1.0), FillRule::NonZero)?;
		canvas.fill_path(rect(OTHER), solid(0.5, 0.5, 0.5, 1.0), FillRule::NonZero)?;
		canvas.end_layer()
	})?;
	// HALF MULTIPLIED BY HALF IS A QUARTER, the overlap multiplied ONCE is still a quarter, and outside the
	// children the backdrop is untouched.
	every_pixel(&frame, |x, y| holding(x, y, if within(x, y, ONE) || within(x, y, OTHER) { 0.25 } else { 0.5 }, 1.0))
}

// @covers: LayerNesting
/// LAYERS NEST, and each level's opacity multiplies the ones inside it - which is what makes a fading
/// panel inside a fading dialogue fade twice rather than being clamped at one.
pub fn layer_nesting() -> Outcome {
	let frame = draw(WIDE, WIDE, |canvas| {
		canvas.begin_layer(None, 0.5, BlendMode::Normal, None)?;
		canvas.begin_layer(None, 0.5, BlendMode::Normal, None)?;
		canvas.fill_path(rect(OTHER), white(), FillRule::NonZero)?;
		canvas.end_layer()?;
		canvas.end_layer()
	})?;
	// A HALF INSIDE A HALF IS A QUARTER.
	every_pixel(&frame, |x, y| if within(x, y, OTHER) { holding(x, y, 1.0, 0.25) } else { [0; 4] })
}
