//! STROKES: the width, the three caps, the three joins, the miter limit and the dash pattern.
//!
//! WHAT MAKES A CAP OR A JOIN CHECKABLE is a probe OUTSIDE the segment's own rectangle. Every cap
//! draws the same pixels along the line; they differ only past its end, and every join draws the same
//! pixels along both arms and differs only in the wedge between them.

use crate::Outcome;
use crate::harness::{draw, white};
use graphics_core::geom::PointF;
use render2d::path::{Cap, Join, PathBuilder, StrokeStyle};

/// A horizontal segment from (8,16) to (24,16), stroked eight wide with the given cap.
fn capped(cap: Cap) -> Result<crate::harness::Frame, crate::Trouble> {
	let mut builder = PathBuilder::new();
	builder.add_line(PointF { x: 8.0, y: 16.0 }, PointF { x: 24.0, y: 16.0 })?;
	let path = builder.finish();
	let style = StrokeStyle { width: 8.0, cap, ..StrokeStyle::default() };
	draw(32, 32, |canvas| canvas.stroke_path(path, white(), style))
}

/// A right angle at (24,8), stroked eight wide with the given join and miter limit.
fn joined(join: Join, miter_limit: f32) -> Result<crate::harness::Frame, crate::Trouble> {
	let mut builder = PathBuilder::new();
	builder.add_polyline(&[PointF { x: 8.0, y: 8.0 }, PointF { x: 24.0, y: 8.0 }, PointF { x: 24.0, y: 24.0 }])?;
	let path = builder.finish();
	let style = StrokeStyle { width: 8.0, join, miter_limit, ..StrokeStyle::default() };
	draw(32, 32, |canvas| canvas.stroke_path(path, white(), style))
}

// @covers: StrokeWidth
/// A stroke is the width it says, centred on the path - half on each side.
pub fn stroke_width() -> Outcome {
	let frame = capped(Cap::Butt)?;
	require!(frame.covered(16, 13), "four pixels above the line is inside a width of eight: {:?}", frame.pixel(16, 13));
	require!(frame.covered(16, 19), "and four below: {:?}", frame.pixel(16, 19));
	require!(frame.empty(16, 21), "five is outside it, so the width is not doubled: {:?}", frame.pixel(16, 21));
	Ok(())
}

// @covers: CapButt
/// A butt cap stops AT the endpoint: nothing is drawn past it.
pub fn cap_butt() -> Outcome {
	let frame = capped(Cap::Butt)?;
	require!(frame.covered(23, 16), "the last pixel of the segment is drawn: {:?}", frame.pixel(23, 16));
	require!(frame.empty(25, 16), "and nothing past its end: {:?}", frame.pixel(25, 16));
	Ok(())
}

// @covers: CapRound
/// A round cap is a half-disc of the stroke's radius past the end - so it reaches along the axis and
/// not into the corner, which is the whole difference from a square one.
pub fn cap_round() -> Outcome {
	let frame = capped(Cap::Round)?;
	require!(frame.covered(26, 16), "the cap reaches its radius along the axis: {:?}", frame.pixel(26, 16));
	require!(frame.empty(27, 12), "and does not reach the corner of the square that would contain it: {:?}", frame.pixel(27, 12));
	Ok(())
}

// @covers: CapSquare
/// A square cap extends the segment by half the width, corners and all.
pub fn cap_square() -> Outcome {
	let frame = capped(Cap::Square)?;
	require!(frame.covered(26, 16), "the cap reaches half a width past the end: {:?}", frame.pixel(26, 16));
	require!(frame.covered(26, 13), "including the corner a round cap leaves out: {:?}", frame.pixel(26, 13));
	require!(frame.empty(29, 16), "and stops there: {:?}", frame.pixel(29, 16));
	Ok(())
}

// @covers: JoinMiter
/// A miter join fills the corner out to where the two outer edges meet.
pub fn join_miter() -> Outcome {
	let frame = joined(Join::Miter, 4.0)?;
	require!(frame.covered(27, 5), "the outer corner is filled out to the miter point: {:?}", frame.pixel(27, 5));
	Ok(())
}

// @covers: JoinBevel
/// A bevel join cuts that corner off with one straight segment.
pub fn join_bevel() -> Outcome {
	let frame = joined(Join::Bevel, 4.0)?;
	require!(frame.empty(27, 5), "the miter point is NOT filled by a bevel: {:?}", frame.pixel(27, 5));
	require!(frame.covered(26, 8), "and the join itself is: {:?}", frame.pixel(26, 8));
	Ok(())
}

// @covers: JoinRound
/// A round join is an arc of the stroke's radius about the corner: it fills further than a bevel and
/// not as far as a miter.
pub fn join_round() -> Outcome {
	let frame = joined(Join::Round, 4.0)?;
	// THE ARC IS AT THE STROKE'S RADIUS ABOUT THE CORNER, so the pixel at four and a quarter away is
	// outside it and the one at three and a half is in - which a bevel's straight cut is not.
	require!(frame.alpha(27, 5) < 64, "a round join does not reach the miter point: {:?}", frame.pixel(27, 5));
	require!(frame.alpha(26, 5) > 200, "but it is rounder than a bevel's straight cut: {:?}", frame.pixel(26, 5));
	Ok(())
}

// @covers: MiterLimit
/// PAST THE LIMIT A MITER BECOMES A BEVEL. A nearly parallel join produces a spike hundreds of pixels
/// long without it, which is the artefact that looks like a corrupt path.
pub fn miter_limit() -> Outcome {
	// A SHALLOW JOIN, whose miter runs nearly seven pixels past a vertex three pixels wide.
	let mut builder = PathBuilder::new();
	builder.add_polyline(&[PointF { x: 2.0, y: 30.0 }, PointF { x: 40.0, y: 16.0 }, PointF { x: 2.0, y: 2.0 }])?;
	let path = builder.finish();
	let spike = |miter_limit: f32| {
		let style = StrokeStyle { width: 6.0, join: Join::Miter, miter_limit, ..StrokeStyle::default() };
		draw(48, 32, |canvas| canvas.stroke_path(path.clone(), white(), style))
	};
	let long = spike(8.0)?;
	let cut = spike(1.0)?;
	require!(long.alpha(44, 16) > 0, "a generous limit lets the miter run out toward its point: {:?}", long.pixel(44, 16));
	require!(cut.empty(44, 16), "and a limit of one turns the same join into a bevel: {:?}", cut.pixel(44, 16));
	require!(cut.alpha(38, 16) > 0, "which is still a join and not a gap: {:?}", cut.pixel(38, 16));
	Ok(())
}

// @covers: StrokeDash
/// A dash pattern alternates drawn and undrawn lengths along the path, starting at the phase given.
pub fn stroke_dash() -> Outcome {
	let mut builder = PathBuilder::new();
	builder.add_line(PointF { x: 0.0, y: 16.0 }, PointF { x: 32.0, y: 16.0 })?;
	let path = builder.finish();
	let mut canvas = render2d::Canvas::new();
	let dashes = canvas.resources().add_dashes(alloc::vec![4.0, 4.0])?;
	let style = StrokeStyle { width: 8.0, dash: Some(render2d::path::DashHandleRange { pattern: dashes, phase: 0.0 }), ..StrokeStyle::default() };
	canvas.stroke_path(path, white(), style)?;
	let list = canvas.finish()?;
	let frame = crate::harness::render(32, 32, &list, &soft2d::target::NoImages, &soft2d::glyph::NoGlyphs)?;
	require!(frame.covered(2, 16), "the first four pixels are on: {:?}", frame.pixel(2, 16));
	require!(frame.empty(6, 16), "the next four are off: {:?}", frame.pixel(6, 16));
	require!(frame.covered(10, 16), "and the pattern repeats: {:?}", frame.pixel(10, 16));
	Ok(())
}
