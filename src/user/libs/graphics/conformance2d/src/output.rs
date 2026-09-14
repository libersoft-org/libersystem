//! THE OUTPUT PATH EVERY FEATURE ENDS AT: a colour from a wider gamut than the target's, and an
//! eight-bit destination that has to represent more values than it has codes.
//!
//! THESE ARE NOT PROFILE ENTRIES, and they are here because the suite's own coverage list names
//! them. A profile entry is something a backend can fail to do; wide-gamut composition and the
//! quantisation at the end are properties of the path every one of those features takes, so they are
//! run and reported alongside the enumeration rather than folded into a feature that would then fail
//! for two reasons.

use crate::Outcome;
use crate::harness::{draw, rect_path, render_in, solid};
use graphics_core::ColorSpace;
use graphics_core::geom::RectF;
use render2d::paint::{Color, Paint};
use render2d::path::FillRule;

/// WIDE-GAMUT COMPOSITION: a colour from a wider gamut is CONVERTED and not copied.
///
/// WHITE IS THE PROBE THAT CATCHES A WRONG MATRIX. White is common to every gamut with the same
/// white point, so a Display P3 white must arrive as the target's white EXACTLY - a conversion built
/// from the wrong primaries lands beside it. And a saturated P3 green is OUTSIDE sRGB, so it must
/// come back at least as far as sRGB's own green rather than as the same numbers.
pub fn wide_gamut_composition() -> Outcome {
	let drawn = |colour: Color| draw(8, 8, move |canvas| canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 8.0, 8.0)), Paint::Solid(colour), FillRule::NonZero));
	let white = drawn(Color::new(1.0, 1.0, 1.0, 1.0, ColorSpace::DisplayP3))?;
	require!(white.pixel(4, 4)[..3] == [255, 255, 255], "a Display P3 white is the target's white: {:?}", white.pixel(4, 4));

	// THE SAME NUMBERS IN TWO GAMUTS ARE TWO COLOURS. Both spaces carry the sRGB transfer, so what is
	// left between them is the PRIMARIES - and a colour inside both gamuts is where that shows,
	// because a colour outside one of them clamps and two clamped colours are equal for the wrong
	// reason.
	let wide = drawn(Color::new(0.8, 0.3, 0.2, 1.0, ColorSpace::DisplayP3))?;
	let narrow = drawn(Color::new(0.8, 0.3, 0.2, 1.0, ColorSpace::Srgb))?;
	let (wide_pixel, narrow_pixel) = (wide.pixel(4, 4), narrow.pixel(4, 4));
	require!(wide_pixel != narrow_pixel, "a P3 colour is not an sRGB colour with the same numbers: both are {wide_pixel:?}");
	// P3'S PRIMARIES ARE FURTHER OUT, so the same coordinates inside them describe a MORE saturated
	// colour: converting into sRGB's smaller gamut spreads the channels apart rather than together.
	let spread = |pixel: [u8; 4]| pixel[0].max(pixel[1]).max(pixel[2]) as i32 - pixel[0].min(pixel[1]).min(pixel[2]) as i32;
	require!(spread(wide_pixel) > spread(narrow_pixel), "and it is the more saturated of the two: {wide_pixel:?} against {narrow_pixel:?}");
	Ok(())
}

/// HDR TO SDR, AND THE DITHER AT THE END OF IT.
///
/// AN EIGHT-BIT TARGET HAS 256 CODES AND A GRADIENT HAS MORE VALUES THAN THAT. Rounding each pixel
/// to its nearest code turns a shallow ramp into visible bands; the profile's ORDERED dither spreads
/// the error over a neighbourhood instead, so a region whose exact value lies between two codes is
/// drawn as BOTH of them. That is the property here: not "it looks smooth", but that two codes
/// appear where an undithered round would have produced one.
pub fn hdr_to_sdr_with_dithering() -> Outcome {
	// A value a third of a step above a code, over a region wide enough for the ordered matrix to
	// repeat twice on each axis.
	let value = 0.5 + 1.0 / (3.0 * 255.0);
	let mut canvas = render2d::Canvas::new();
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 16.0, 16.0)), solid(value, value, value, 1.0), FillRule::NonZero)?;
	let list = canvas.finish()?;
	let frame = render_in(16, 16, ColorSpace::SrgbLinear, &list)?;
	// THE TWO CODES THE VALUE LIES BETWEEN, computed from the value rather than written down: the
	// target is linear, so a stored code is the value times 255 and the pair either side of it is
	// what a dither is allowed to use.
	let exact = value * 255.0;
	let low = exact as u8;
	let high = low + 1;
	let mut lower = 0usize;
	let mut higher = 0usize;
	for y in 0..16 {
		for x in 0..16 {
			let code = frame.pixel(x, y)[0];
			if code == low {
				lower += 1;
			} else if code == high {
				higher += 1;
			} else {
				return Err(crate::Trouble::Failed(alloc::format!("a value between {low} and {high} is drawn as one of the two and not as {code}")));
			}
		}
	}
	require!(lower > 0 && higher > 0, "both codes appear: {lower} of {low} and {higher} of {high}");
	// AND IT AVERAGES TO THE VALUE, which is what makes it a dither rather than a brightening.
	let average = (lower as f32 * low as f32 + higher as f32 * high as f32) / 256.0;
	require!((average - exact).abs() < 0.5, "the dithered region averages to the value it was given: {average} against {exact}");
	Ok(())
}
