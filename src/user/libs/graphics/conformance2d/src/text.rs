//! TEXT: the five forms a glyph can arrive in, the transform it is drawn under, and the subpixel
//! position it is placed at.
//!
//! THE GLYPHS COME FROM A PROVIDER THIS SUITE WRITES, and that is deliberate. What Profile 1 requires
//! of `render2d` is that it DRAWS each form - an outline filled with the run's paint, a coverage mask
//! composited as coverage, a bitmap strike composited as colour, a layered glyph in its palette
//! colours - and a suite that reached for a real face would be testing the face parser and the shaper
//! on the way. Those have their own conformance run; this one is about the drawing.

use crate::Outcome;
use crate::harness::{draw_with, near, white};
use font_contract::glyph::{GlyphKind, RasterisationMode};
use graphics_core::geom::{Extent2D, RectF};
use graphics_core::layout::{ImageLayout, RowOrigin};
use graphics_core::semantics::ImageSemantics;
use graphics_core::{AlphaMode, ColorSpace, OwnedImage, PixelFormat, PixelStorage};
use render2d::list::RecordedGlyphRun;
use render2d::paint::Color;
use render2d::path::PathBuilder;
use soft2d::glyph::{GlyphImage, GlyphProvider};

/// One glyph at a known pen position, in the form the scene is about.
pub fn one_glyph(kind: GlyphKind) -> RecordedGlyphRun {
	run_at(kind, 4.0, RasterisationMode::Grayscale)
}

/// The same, with the pen where the scene wants it - which is what a subpixel position is.
pub fn run_at(kind: GlyphKind, origin_x: f32, mode: RasterisationMode) -> RecordedGlyphRun {
	RecordedGlyphRun { face: font_contract::FaceRef { face: font_contract::FaceIdentity { file: font_contract::face::FileIdentity([3u8; 32]), index: 0 }, generation: font_contract::face::Generation(1) }, size: font_contract::Fixed266::from_pixels(8), variation: font_contract::VariationCoordinates::default(), script: font_contract::ScriptTag::from_bytes(*b"latn"), direction: font_contract::Direction::LeftToRight, mode, origin_x: font_contract::Fixed266::from_raw((origin_x * 64.0) as i32), origin_y: font_contract::Fixed266::from_pixels(12), glyphs: alloc::vec![font_contract::PositionedGlyph { glyph: 17, x_offset: font_contract::Fixed266::ZERO, y_offset: font_contract::Fixed266::ZERO, x_advance: font_contract::Fixed266::from_pixels(8), y_advance: font_contract::Fixed266::ZERO, kind, selection: font_contract::cache::KindSelection { strike: None, palette: None } }], clusters: alloc::vec![] }
}

/// A provider with one form per kind, each of a STATED shape - a four-pixel box above the baseline -
/// so every scene's expectation is arithmetic rather than a look.
pub struct Forms;

impl GlyphProvider for Forms {
	fn glyph(&self, key: &font_contract::cache::GlyphCacheKey) -> GlyphImage {
		match key.kind {
			// THE FORM IS PRODUCED FOR THE TRANSFORM IN THE KEY, which is what a real provider does and
			// is the half of `GlyphTransform` that belongs to whoever owns the face: a glyph drawn at
			// twice the scale is RASTERISED at twice the scale rather than scaled up afterwards, or
			// the text in a zoomed drawing is a blurred picture of text.
			GlyphKind::Outline => {
				let mut builder = PathBuilder::new();
				let _ = builder.add_rect(RectF::new(0.0, -4.0, 4.0, 4.0));
				let matrix = key.transform.matrix();
				let scaled = render2d::transform::Transform { m: [[matrix[0], matrix[2], 0.0], [matrix[1], matrix[3], 0.0], [0.0, 0.0, 1.0]] };
				let path = builder.finish();
				GlyphImage::Outline(path.transformed(&scaled).unwrap_or(path))
			}
			GlyphKind::GrayscaleMask => GlyphImage::Mask { left: 0, top: -4, width: 4, height: 4, coverage: alloc::vec![255u8; 16], mode: RasterisationMode::Grayscale },
			GlyphKind::SubpixelMask => GlyphImage::Mask { left: 0, top: -4, width: 4, height: 4, coverage: alloc::vec![255u8; 48], mode: key.mode },
			// THE STRIKE CARRIES TWO DIFFERENT COLOURS, which is what makes "the strike's own colour"
			// checkable: a tinted one would be one colour whatever the bitmap held.
			GlyphKind::BitmapStrike => GlyphImage::Bitmap { left: 0, top: -4, image: two_colour_strike() },
			GlyphKind::ColrLayers | GlyphKind::ColrPaintGraph => {
				let mut builder = PathBuilder::new();
				let _ = builder.add_rect(RectF::new(0.0, -4.0, 4.0, 4.0));
				GlyphImage::Layers(alloc::vec![(builder.finish(), Color::new(0.0, 1.0, 0.0, 1.0, ColorSpace::SrgbLinear))])
			}
		}
	}
}

/// A four by four strike whose left half is blue and whose right half is red.
fn two_colour_strike() -> OwnedImage {
	let semantics = ImageSemantics::Color { color_space: ColorSpace::SrgbLinear, alpha_mode: AlphaMode::Straight };
	let storage = PixelStorage::Known(PixelFormat::R8G8B8A8Unorm);
	let layout = ImageLayout::new(Extent2D::new(4, 4), 16, storage, RowOrigin::TopLeft, semantics).expect("a four by four layout is legal");
	let mut image = OwnedImage::new(layout).expect("sixty-four bytes");
	{
		let mut view = image.view_mut();
		for y in 0..4 {
			for x in 0..4 {
				let colour = if x < 2 { graphics_core::pixel::Rgba::new(0.0, 0.0, 1.0, 1.0) } else { graphics_core::pixel::Rgba::new(1.0, 0.0, 0.0, 1.0) };
				graphics_core::pixel::write(&mut view, x, y, colour);
			}
		}
	}
	image
}

fn drawn(kind: GlyphKind) -> Result<crate::harness::Frame, crate::Trouble> {
	draw_with(24, 24, &soft2d::target::NoImages, &Forms, |canvas| canvas.draw_glyph_run(one_glyph(kind), white()))
}

// @covers: GlyphOutlines
/// An outline glyph is FILLED WITH THE RUN'S PAINT, which is what makes text a colour the application
/// chose rather than a colour the face has.
pub fn glyph_outlines() -> Outcome {
	let frame = draw_with(24, 24, &soft2d::target::NoImages, &Forms, |canvas| canvas.draw_glyph_run(one_glyph(GlyphKind::Outline), crate::harness::solid(0.0, 1.0, 0.0, 1.0)))?;
	let pixel = frame.pixel(5, 9);
	require!(pixel[3] == 255, "the outline is filled: {pixel:?}");
	require!(pixel[1] > 200 && pixel[0] < 40, "in the run's own paint and not the face's: {pixel:?}");
	Ok(())
}

// @covers: GlyphGrayscaleMask
/// A grayscale mask is composited as COVERAGE: full coverage puts the paint down in full.
pub fn glyph_grayscale_mask() -> Outcome {
	let frame = drawn(GlyphKind::GrayscaleMask)?;
	require!(frame.covered(5, 9), "a mask of full coverage is opaque: {:?}", frame.pixel(5, 9));
	require!(frame.empty(15, 9), "and covers only where the mask is: {:?}", frame.pixel(15, 9));
	Ok(())
}

// @covers: GlyphBitmapStrike
/// A bitmap strike brings its OWN pixels: it is composited rather than filled, so the paint does not
/// reach it.
pub fn glyph_bitmap_strike() -> Outcome {
	let frame = draw_with(24, 24, &soft2d::target::NoImages, &Forms, |canvas| canvas.draw_glyph_run(one_glyph(GlyphKind::BitmapStrike), crate::harness::solid(0.0, 1.0, 0.0, 1.0)))?;
	let pixel = frame.pixel(5, 9);
	require!(pixel[3] > 200, "the strike is drawn: {pixel:?}");
	require!(pixel[1] < 40, "and the run's green paint did not tint it: {pixel:?}");
	Ok(())
}

// @covers: GlyphEmbeddedColorBitmap
/// AND ITS COLOURS ARE ITS OWN, more than one of them - which is the difference between a strike and
/// a mask with a colour attached: an emoji is not one colour.
pub fn glyph_embedded_color_bitmap() -> Outcome {
	let frame = drawn(GlyphKind::BitmapStrike)?;
	let (left, right) = (frame.pixel(5, 9), frame.pixel(7, 9));
	require!(left[2] > 200 && left[0] < 40, "the strike's left half is its own blue: {left:?}");
	require!(right[0] > 200 && right[2] < 40, "and its right half its own red: {right:?}");
	Ok(())
}

// @covers: GlyphColorLayers
/// A layered glyph is drawn in the layer's PALETTE colour, again not the run's paint.
pub fn glyph_color_layers() -> Outcome {
	let frame = draw_with(24, 24, &soft2d::target::NoImages, &Forms, |canvas| canvas.draw_glyph_run(one_glyph(GlyphKind::ColrLayers), crate::harness::solid(1.0, 0.0, 0.0, 1.0)))?;
	let pixel = frame.pixel(5, 9);
	require!(pixel[1] > 200 && pixel[0] < 40, "the layer's green is drawn and not the run's red: {pixel:?}");
	Ok(())
}

// @covers: GlyphTransform
/// A glyph run is TRANSFORMED WITH THE CANVAS, which is what makes text in a scaled or rotated
/// drawing text rather than a sticker: the form is produced for the transform it is drawn under.
pub fn glyph_transform() -> Outcome {
	let plain = drawn(GlyphKind::Outline)?;
	let scaled = draw_with(24, 24, &soft2d::target::NoImages, &Forms, |canvas| {
		canvas.concat_transform(&render2d::transform::Transform::scale(2.0, 2.0));
		canvas.draw_glyph_run(one_glyph(GlyphKind::Outline), white())
	})?;
	// THE PEN IS MAPPED BY THE TRANSFORM. The run's origin is in USER space, so a run recorded at
	// (4,12) under a twofold scale is drawn at (8,24) - a backend that placed the form at the
	// recorded coordinates would draw text that stays put while the drawing around it moves.
	require!(plain.covered(5, 9), "the untransformed run is at its recorded pen: {:?}", plain.pixel(5, 9));
	require!(scaled.covered(10, 18), "and the scaled one is at the mapped pen: {:?}", scaled.pixel(10, 18));
	require!(scaled.empty(5, 9), "and not at the recorded one: {:?}", scaled.pixel(5, 9));
	// AND THE FORM IS THE ONE THE KEY ASKED FOR, which is twice as wide here.
	let width_of = |frame: &crate::harness::Frame, y: u32| (0..24).filter(|x| frame.alpha(*x, y) > 128).count();
	let (small, large) = (width_of(&plain, 9), width_of(&scaled, 18));
	require!(large >= small * 2, "a twofold scale draws the glyph twice as wide: {small} against {large}");
	Ok(())
}

// @covers: GlyphSubpixelPositioning
/// A pen at a FRACTION of a pixel draws the glyph at that fraction. A renderer that rounded the
/// origin to a whole pixel would draw both of these identically, which is what makes text at small
/// sizes drift out of step with its own layout.
pub fn glyph_subpixel_positioning() -> Outcome {
	let at = |origin: f32| draw_with(24, 24, &soft2d::target::NoImages, &Forms, move |canvas| canvas.draw_glyph_run(run_at(GlyphKind::Outline, origin, RasterisationMode::Grayscale), white()));
	let whole = at(4.0)?;
	let half = at(4.5)?;
	require!(whole.covered(4, 9), "at a whole pixel the glyph's left edge is sharp: {:?}", whole.pixel(4, 9));
	require!(near(half.alpha(4, 9), 0.5), "and at half a pixel it is half covered: {:?}", half.pixel(4, 9));
	Ok(())
}
