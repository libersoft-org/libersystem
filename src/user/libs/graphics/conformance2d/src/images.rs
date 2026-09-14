//! IMAGES: where they are put, how they are sampled, how they continue past their edge, and what
//! happens to their colour on the way into a target that is not in their own space.

use crate::Outcome;
use crate::harness::{Frame, draw_with, near, render_in};
use graphics_core::geom::{Extent2D, RectF};
use graphics_core::layout::{ImageLayout, RowOrigin};
use graphics_core::pixel::Rgba;
use graphics_core::semantics::ImageSemantics;
use graphics_core::{AlphaMode, ColorSpace, ImageView, OwnedImage, PixelFormat, PixelStorage};
use render2d::paint::{ImageQuality, Paint, SpreadMode};
use render2d::path::FillRule;
use soft2d::target::ImageSource;

/// The identity every scene's image is registered under.
const IMAGE: u64 = 1;

/// One image, handed to the backend by identity.
pub struct OneImage(pub OwnedImage);

impl ImageSource for OneImage {
	fn image(&self, identity: u64) -> Option<ImageView<'_>> {
		(identity == IMAGE).then(|| self.0.view())
	}
}

/// Build an image of `width` by `height` in a stated colour space, from a function of the texel.
pub fn image_of(width: u32, height: u32, space: ColorSpace, texel: impl Fn(u32, u32) -> Rgba) -> OwnedImage {
	let semantics = ImageSemantics::Color { color_space: space, alpha_mode: AlphaMode::Straight };
	let storage = PixelStorage::Known(PixelFormat::R8G8B8A8Unorm);
	let layout = ImageLayout::new(Extent2D::new(width, height), width * 4, storage, RowOrigin::TopLeft, semantics).expect("a legal layout");
	let mut image = OwnedImage::new(layout).expect("an image");
	{
		let mut view = image.view_mut();
		for y in 0..height {
			for x in 0..width {
				graphics_core::pixel::write(&mut view, x, y, texel(x, y));
			}
		}
	}
	image
}

/// TWO TEXELS, BLACK AND WHITE, side by side - the source every sampling scene is about, because the
/// answers a filter gives between two known values are arithmetic a scene can state.
pub fn black_and_white() -> OwnedImage {
	image_of(2, 1, ColorSpace::SrgbLinear, |x, _| if x == 0 { Rgba::new(0.0, 0.0, 0.0, 1.0) } else { Rgba::new(1.0, 1.0, 1.0, 1.0) })
}

/// The record a list refers to an image by.
pub fn record() -> render2d::list::ImageRecord {
	render2d::list::ImageRecord { identity: IMAGE, layout_generation: 1, content_generation: 1 }
}

/// Draw one image into a destination rectangle at a stated quality.
fn drawn(source: &OwnedImage, quality: ImageQuality, from: RectF, to: RectF) -> Result<Frame, crate::Trouble> {
	let images = OneImage(clone_of(source, ColorSpace::SrgbLinear));
	draw_with(32, 32, &images, &soft2d::glyph::NoGlyphs, |canvas| canvas.draw_image(record(), from, to, quality))
}

/// An image is not `Clone`, and a scene needs its own copy to hand to the source.
fn clone_of(image: &OwnedImage, space: ColorSpace) -> OwnedImage {
	let view = image.view();
	let extent = image.layout().extent;
	image_of(extent.width, extent.height, space, |x, y| graphics_core::pixel::read(&view, x, y).unwrap_or(Rgba::TRANSPARENT))
}

// @covers: ImageSourceDestRect
/// A source rectangle chooses WHAT is drawn and a destination rectangle WHERE - and the two are
/// separate, which is what makes a sprite sheet possible at all.
pub fn image_source_dest_rect() -> Outcome {
	let source = image_of(4, 1, ColorSpace::SrgbLinear, |x, _| match x {
		0 => Rgba::new(1.0, 0.0, 0.0, 1.0),
		1 => Rgba::new(0.0, 1.0, 0.0, 1.0),
		2 => Rgba::new(0.0, 0.0, 1.0, 1.0),
		_ => Rgba::new(1.0, 1.0, 1.0, 1.0),
	});
	// THE THIRD TEXEL ALONE, into the middle of the target.
	let frame = drawn(&source, ImageQuality::Nearest, RectF::new(2.0, 0.0, 1.0, 1.0), RectF::new(8.0, 8.0, 16.0, 16.0))?;
	let pixel = frame.pixel(16, 16);
	require!(pixel[2] > 200 && pixel[0] < 40, "the chosen texel is the one drawn: {pixel:?}");
	require!(frame.empty(4, 4), "and it is drawn only where the destination says: {:?}", frame.pixel(4, 4));
	Ok(())
}

// @covers: ImageProjective
/// An image under a PROJECTIVE transform is sampled through the divide, so a square image becomes a
/// trapezium. An implementation that mapped the corners and interpolated linearly between them draws
/// the same outline with the texture sliding through it.
pub fn image_projective() -> Outcome {
	let source = image_of(2, 2, ColorSpace::SrgbLinear, |x, y| if (x + y) % 2 == 0 { Rgba::new(1.0, 1.0, 1.0, 1.0) } else { Rgba::new(0.0, 0.0, 0.0, 1.0) });
	let images = OneImage(source);
	let mut transform = render2d::transform::Transform::IDENTITY;
	transform.m[2][0] = 1.0 / 48.0;
	let frame = draw_with(48, 48, &images, &soft2d::glyph::NoGlyphs, |canvas| {
		canvas.concat_transform(&transform);
		canvas.draw_image(record(), RectF::new(0.0, 0.0, 2.0, 2.0), RectF::new(2.0, 2.0, 44.0, 44.0), ImageQuality::Nearest)
	})?;
	let height_at = |x: u32| (0..48).filter(|y| frame.alpha(x, *y) > 128).count();
	let (near_edge, far_edge) = (height_at(4), height_at(30));
	require!(near_edge > far_edge + 3, "the image narrows with the perspective divide: {near_edge} against {far_edge}");
	Ok(())
}

// @covers: ImageNearest
/// Nearest sampling is a STEP: every destination pixel is one whole texel, so the boundary between
/// two texels is the only place the value changes.
pub fn image_nearest() -> Outcome {
	let frame = drawn(&black_and_white(), ImageQuality::Nearest, RectF::new(0.0, 0.0, 2.0, 1.0), RectF::new(0.0, 0.0, 32.0, 32.0))?;
	require!(frame.pixel(4, 16)[0] == 0, "the left half is the black texel and nothing between: {:?}", frame.pixel(4, 16));
	require!(frame.pixel(12, 16)[0] == 0, "right up to the boundary: {:?}", frame.pixel(12, 16));
	require!(frame.pixel(20, 16)[0] == 255, "and the right half is the white one: {:?}", frame.pixel(20, 16));
	Ok(())
}

// @covers: ImageBilinear
/// Bilinear sampling is a straight RAMP between the two texel centres - and the value at a quarter of
/// the way is a quarter, which is the number a scene can state.
pub fn image_bilinear() -> Outcome {
	let frame = drawn(&black_and_white(), ImageQuality::Bilinear, RectF::new(0.0, 0.0, 2.0, 1.0), RectF::new(0.0, 0.0, 32.0, 32.0))?;
	// THE TEXEL CENTRES LAND AT 8 AND 24, so the pixel whose centre is 12.5 is `(12.5 - 8) / 16` of
	// the way along - the half-pixel is the part an expectation written as "a quarter" gets wrong.
	require!(near(frame.pixel(12, 16)[0], (12.5 - 8.0) / 16.0), "the ramp is the straight line between the texel centres: {:?}", frame.pixel(12, 16));
	require!(near(frame.pixel(20, 16)[0], (20.5 - 8.0) / 16.0), "at every point along it: {:?}", frame.pixel(20, 16));
	// AND IT IS FLAT OUTSIDE THEM: a two-tap kernel reaches one texel and no further.
	require!(frame.pixel(6, 16)[0] == 0, "before the first texel centre there is nothing to interpolate with: {:?}", frame.pixel(6, 16));
	Ok(())
}

// @covers: ImageBicubic
/// Bicubic sampling is NOT the straight ramp: its curve is steeper in the middle and flatter at the
/// ends, which is what makes an enlarged photograph look sharp rather than soft. A backend that
/// answered bilinear for both would pass every "is it smooth" check.
pub fn image_bicubic() -> Outcome {
	let bicubic = drawn(&black_and_white(), ImageQuality::Bicubic, RectF::new(0.0, 0.0, 2.0, 1.0), RectF::new(0.0, 0.0, 32.0, 32.0))?;
	let bilinear = drawn(&black_and_white(), ImageQuality::Bilinear, RectF::new(0.0, 0.0, 2.0, 1.0), RectF::new(0.0, 0.0, 32.0, 32.0))?;
	// A FOUR-TAP KERNEL REACHES A TEXEL FURTHER ON EACH SIDE, so the transition is WIDER than the
	// two-tap ramp and begins before the first texel centre - which is the structural difference
	// between the two and not a matter of how smooth the result looks.
	let transition = |frame: &Frame| {
		(0..32)
			.filter(|x| {
				let value = frame.pixel(*x, 16)[0];
				value > 0 && value < 255
			})
			.count()
	};
	require!(transition(&bicubic) >= transition(&bilinear) + 3, "the cubic transition is wider: {} against {}", transition(&bicubic), transition(&bilinear));
	require!(bicubic.pixel(6, 16)[0] > 0, "and it has begun before the first texel centre: {:?}", bicubic.pixel(6, 16));
	// AND IT IS STILL MONOTONE. A kernel with the wrong sign overshoots into a ringing edge, which is
	// visible as a bright line beside every boundary in an enlarged photograph.
	let mut previous = 0u8;
	for x in 0..32 {
		let value = bicubic.pixel(x, 16)[0];
		require!(value >= previous, "the ramp does not go backwards at {x}: {value} after {previous}");
		previous = value;
	}
	Ok(())
}

// @covers: ImageMipmappedMinification
/// A large MINIFICATION without a pyramid is aliasing: the destination pixel takes whichever texel it
/// landed on. With one it is the average of what it covers, which is what stops a scaled-down
/// checkerboard from shimmering as it moves.
pub fn image_mipmapped_minification() -> Outcome {
	let checker = image_of(16, 16, ColorSpace::SrgbLinear, |x, y| if (x + y) % 2 == 0 { Rgba::new(1.0, 1.0, 1.0, 1.0) } else { Rgba::new(0.0, 0.0, 0.0, 1.0) });
	let frame = drawn(&checker, ImageQuality::Mipmapped, RectF::new(0.0, 0.0, 16.0, 16.0), RectF::new(0.0, 0.0, 2.0, 2.0))?;
	let value = frame.pixel(0, 0)[0];
	require!((value as i32 - 128).abs() < 40, "a minified checkerboard averages to the middle rather than picking a side: {value}");
	Ok(())
}

/// A pattern paint over a rectangle wider than the image, so what is checked is what happens PAST the
/// image's own edge.
fn spread(mode: SpreadMode) -> Result<Frame, crate::Trouble> {
	let source = image_of(2, 1, ColorSpace::SrgbLinear, |x, _| if x == 0 { Rgba::new(1.0, 0.0, 0.0, 1.0) } else { Rgba::new(0.0, 0.0, 1.0, 1.0) });
	let images = OneImage(source);
	draw_with(32, 8, &images, &soft2d::glyph::NoGlyphs, |canvas| {
		let handle = canvas.resources().add_image(record())?;
		// EIGHT TIMES ACTUAL SIZE, so each texel is eight pixels wide and the pattern repeats at 16.
		let paint = Paint::Image { image: handle, source: RectF::new(0.0, 0.0, 2.0, 1.0), quality: ImageQuality::Nearest, spread: mode, transform: render2d::transform::Transform::scale(8.0, 8.0) };
		canvas.fill_path(crate::harness::rect_path(RectF::new(0.0, 0.0, 32.0, 8.0)), paint, FillRule::NonZero)
	})
}

// @covers: ImageWrapClamp
/// Clamped, the edge texel continues for ever - so everything past the image is its last colour.
pub fn image_wrap_clamp() -> Outcome {
	let frame = spread(SpreadMode::Clamp)?;
	require!(frame.pixel(4, 4)[0] > 200, "inside the image the first texel is itself: {:?}", frame.pixel(4, 4));
	require!(frame.pixel(30, 4)[2] > 200, "and past its end the last texel is held: {:?}", frame.pixel(30, 4));
	Ok(())
}

// @covers: ImageWrapRepeat
/// Repeated, the pattern starts again - so one period along is the first texel again.
pub fn image_wrap_repeat() -> Outcome {
	let frame = spread(SpreadMode::Repeat)?;
	require!(frame.pixel(20, 4)[0] > 200, "one period along is the first texel again: {:?}", frame.pixel(20, 4));
	require!(frame.pixel(28, 4)[2] > 200, "and the second follows it: {:?}", frame.pixel(28, 4));
	Ok(())
}

// @covers: ImageWrapMirror
/// Mirrored, the pattern reverses at each boundary - which is what makes a tiled texture seamless
/// without a seam-free source.
pub fn image_wrap_mirror() -> Outcome {
	let frame = spread(SpreadMode::Mirror)?;
	require!(frame.pixel(20, 4)[2] > 200, "past the edge the pattern runs backwards, so the LAST texel comes first: {:?}", frame.pixel(20, 4));
	require!(frame.pixel(28, 4)[0] > 200, "and the first comes second: {:?}", frame.pixel(28, 4));
	Ok(())
}

// @covers: ImageOpacity
/// An image is drawn at the canvas's opacity, which is what a fade of a photograph is.
pub fn image_opacity() -> Outcome {
	let source = image_of(1, 1, ColorSpace::SrgbLinear, |_, _| Rgba::new(1.0, 1.0, 1.0, 1.0));
	let images = OneImage(source);
	let frame = draw_with(16, 16, &images, &soft2d::glyph::NoGlyphs, |canvas| {
		canvas.set_opacity(0.5);
		canvas.draw_image(record(), RectF::new(0.0, 0.0, 1.0, 1.0), RectF::new(0.0, 0.0, 16.0, 16.0), ImageQuality::Nearest)
	})?;
	require!(near(frame.alpha(8, 8), 0.5), "half opacity is half alpha: {:?}", frame.pixel(8, 8));
	Ok(())
}

// @covers: ImageColorSpaceConversion
/// AN IMAGE IS CONVERTED INTO THE TARGET'S SPACE and not copied into it. Half-way up the sRGB
/// encoding is a fifth of the way up in linear light, and a backend that treated the bytes as linear
/// would draw every photograph too bright.
pub fn image_color_space_conversion() -> Outcome {
	// An sRGB image whose stored value is 128, which the encoding says is 0.2158 of linear light.
	let source = image_of(1, 1, ColorSpace::Srgb, |_, _| Rgba::new(0.5, 0.5, 0.5, 1.0));
	let images = OneImage(source);
	let frame = draw_with(16, 16, &images, &soft2d::glyph::NoGlyphs, |canvas| canvas.draw_image(record(), RectF::new(0.0, 0.0, 1.0, 1.0), RectF::new(0.0, 0.0, 16.0, 16.0), ImageQuality::Nearest))?;
	let value = frame.pixel(8, 8)[0];
	// `((0.5 + 0.055) / 1.055)^2.4`, which is the profile's own sRGB transfer, times 255.
	require!((value as i32 - 55).abs() <= 3, "sRGB 0.5 is linear 0.2158 and is stored as about 55, not as {value}");

	// AND THE OTHER DIRECTION TOO: a linear paint into an sRGB target comes back encoded.
	let mut canvas = render2d::Canvas::new();
	canvas.fill_path(crate::harness::rect_path(RectF::new(0.0, 0.0, 8.0, 8.0)), crate::harness::solid(0.2158, 0.2158, 0.2158, 1.0), FillRule::NonZero)?;
	let list = canvas.finish()?;
	let encoded = render_in(8, 8, ColorSpace::Srgb, &list)?;
	require!((encoded.pixel(4, 4)[0] as i32 - 128).abs() <= 3, "linear 0.2158 encodes back to about 128, not to {:?}", encoded.pixel(4, 4));
	Ok(())
}
