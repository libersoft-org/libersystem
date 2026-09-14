//! CLIPPING: the four shapes a clip can be, how two of them combine, and what an inverse one is.

use crate::Outcome;
use crate::harness::{draw, draw_with, near, rect_path, white};
use graphics_core::geom::{PointF, RectF};
use render2d::path::{FillRule, PathBuilder};

/// A clip of the caller's shape, with a rectangle covering the whole surface drawn through it - so
/// every pixel of the answer is "did the clip let this through".
fn clipped(clip: render2d::path::Path, rule: FillRule, inverse: bool) -> Result<crate::harness::Frame, crate::Trouble> {
	draw(32, 32, |canvas| {
		// A CLIP IS PUSHED AND POPPED, which is what `save` and `restore` are: a recording that left
		// one open would be refused, and that refusal is what stops a component from clipping its
		// siblings by forgetting to undo its own.
		canvas.save()?;
		if inverse {
			canvas.set_clip_inverse(clip, rule)?;
		} else {
			canvas.set_clip(clip, rule)?;
		}
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 32.0, 32.0)), white(), FillRule::NonZero)?;
		canvas.restore()
	})
}

// @covers: ClipRect
/// A rectangular clip passes what is inside it and nothing outside.
pub fn clip_rect() -> Outcome {
	let frame = clipped(rect_path(RectF::new(8.0, 8.0, 16.0, 16.0)), FillRule::NonZero, false)?;
	require!(frame.covered(16, 16), "inside the clip the drawing is there: {:?}", frame.pixel(16, 16));
	require!(frame.empty(4, 16), "and outside it is not: {:?}", frame.pixel(4, 16));
	require!(frame.empty(28, 28), "on every side: {:?}", frame.pixel(28, 28));
	Ok(())
}

// @covers: ClipRoundedRect
/// A rounded-rectangle clip cuts its corners, which is the clip a card, a panel and a thumbnail are.
pub fn clip_rounded_rect() -> Outcome {
	let mut builder = PathBuilder::new();
	builder.add_rounded_rect(RectF::new(4.0, 4.0, 24.0, 24.0), 10.0, 10.0)?;
	let frame = clipped(builder.finish(), FillRule::NonZero, false)?;
	require!(frame.covered(16, 16), "the middle passes: {:?}", frame.pixel(16, 16));
	require!(frame.covered(16, 5), "the middle of an edge passes: {:?}", frame.pixel(16, 5));
	require!(frame.empty(5, 5), "and the corner is clipped away: {:?}", frame.pixel(5, 5));
	Ok(())
}

// @covers: ClipPath
/// AN ARBITRARY PATH IS A CLIP, under its own fill rule - so a clip is as expressive as a fill and
/// not a rectangle with rounded corners.
pub fn clip_path() -> Outcome {
	let mut builder = PathBuilder::new();
	builder.add_polygon(&[PointF { x: 16.0, y: 2.0 }, PointF { x: 30.0, y: 30.0 }, PointF { x: 2.0, y: 30.0 }])?;
	let frame = clipped(builder.finish(), FillRule::NonZero, false)?;
	require!(frame.covered(16, 24), "the triangle passes what is inside it: {:?}", frame.pixel(16, 24));
	require!(frame.empty(4, 6), "and clips what is outside: {:?}", frame.pixel(4, 6));
	Ok(())
}

// @covers: ClipNested
/// TWO CLIPS INTERSECT. A nested clip that REPLACED its parent would let a child draw outside the
/// region its parent had bounded, which is how a scrolled list paints over its own header.
pub fn clip_nested() -> Outcome {
	let frame = draw(32, 32, |canvas| {
		canvas.save()?;
		canvas.set_clip(rect_path(RectF::new(0.0, 0.0, 20.0, 32.0)), FillRule::NonZero)?;
		canvas.save()?;
		canvas.set_clip(rect_path(RectF::new(12.0, 0.0, 20.0, 32.0)), FillRule::NonZero)?;
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 32.0, 32.0)), white(), FillRule::NonZero)?;
		canvas.restore()?;
		canvas.restore()
	})?;
	require!(frame.covered(16, 16), "the overlap of the two clips passes: {:?}", frame.pixel(16, 16));
	require!(frame.empty(4, 16), "what only the first allowed does not: {:?}", frame.pixel(4, 16));
	require!(frame.empty(28, 16), "and neither does what only the second allowed: {:?}", frame.pixel(28, 16));
	Ok(())
}

// @covers: ClipAlphaMask
/// A CLIP CAN BE AN IMAGE'S ALPHA, which is what a soft-edged mask, a gradient fade and a
/// photographic cut-out are - none of which is a path.
pub fn clip_alpha_mask() -> Outcome {
	// A MASK IN DEVICE PIXELS: opaque on the left, half in the middle, transparent on the right - so
	// what the scene checks is that a clip is a FRACTION and not a threshold.
	let mask = crate::images::image_of(32, 8, graphics_core::ColorSpace::SrgbLinear, |x, _| match x {
		0..=9 => graphics_core::pixel::Rgba::new(1.0, 1.0, 1.0, 1.0),
		10..=19 => graphics_core::pixel::Rgba::new(0.5, 0.5, 0.5, 0.5),
		_ => graphics_core::pixel::Rgba::TRANSPARENT,
	});
	let images = crate::images::OneImage(mask);
	let frame = draw_with(32, 8, &images, &soft2d::glyph::NoGlyphs, |canvas| {
		canvas.save()?;
		canvas.set_clip_mask(crate::images::record(), false)?;
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 32.0, 8.0)), white(), FillRule::NonZero)?;
		canvas.restore()
	})?;
	require!(frame.covered(2, 4), "where the mask is opaque the drawing passes in full: {:?}", frame.pixel(2, 4));
	require!(near(frame.alpha(15, 4), 0.5), "where it is half the drawing is half: {:?}", frame.pixel(15, 4));
	require!(frame.empty(30, 4), "and where it is transparent nothing passes: {:?}", frame.pixel(30, 4));
	Ok(())
}

// @covers: ClipInverse
/// AN INVERSE CLIP IS THE COMPLEMENT and not a second shape. A hole punched in a panel, a spotlight's
/// surround and "everything except the selection" are all this, and building them out of ordinary
/// clips needs the complement of an arbitrary path, which is not a path.
pub fn clip_inverse() -> Outcome {
	let frame = clipped(rect_path(RectF::new(8.0, 8.0, 16.0, 16.0)), FillRule::NonZero, true)?;
	require!(frame.empty(16, 16), "inside the shape nothing passes: {:?}", frame.pixel(16, 16));
	require!(frame.covered(2, 2), "and everything outside it does: {:?}", frame.pixel(2, 2));
	require!(frame.covered(30, 30), "on every side: {:?}", frame.pixel(30, 30));
	Ok(())
}
