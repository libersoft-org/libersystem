//! WHAT EVERY SCENE IS BUILT OUT OF: a target, a drawing, and a way to ask about a pixel.
//!
//! THE TARGET IS LINEAR AND NOT sRGB, deliberately. A scene's pass condition is arithmetic - "the
//! multiply of six tenths and four tenths is twenty-four hundredths" - and in an sRGB target every
//! one of those numbers would go through a transfer function before it could be compared, which
//! turns a stated expectation into a stated expectation plus a second implementation of the encoding.
//! In a linear target a stored byte is the value times 255, so the arithmetic in a scene is the
//! arithmetic the profile states. The sRGB path is not untested by this: `ImageColorSpaceConversion`
//! is its own feature with its own scene.
//!
//! AND A REFUSAL IS NOT A FAILURE, it is `Unsupported`. The distinction matters because the profile
//! is a CLOSED list: a backend that refuses a Profile 1 drawing is not a backend with a gap, it is a
//! backend that does not conform, and the suite reports the two differently so that a reader can tell
//! "this is wrong" from "this is missing".

use alloc::format;
use alloc::string::String;
use graphics_core::geom::Extent2D;
use graphics_core::layout::{ImageLayout, RowOrigin};
use graphics_core::pixel::OutputLuminance;
use graphics_core::semantics::ImageSemantics;
use graphics_core::{AlphaMode, ColorSpace, OwnedImage, PixelFormat, PixelStorage};
use render2d::backend::{Backend, TargetDescription};
use render2d::{Canvas, DrawList};
use soft2d::Soft2d;
use soft2d::glyph::GlyphProvider;
use soft2d::target::ImageSource;
use soft2d::target::NoImages;

/// Why a scene did not pass.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Trouble {
	Failed(String),
	Unsupported(String),
}

/// A drawing API refusal is a refusal of something the profile requires.
impl From<render2d::Error> for Trouble {
	fn from(error: render2d::Error) -> Self {
		Trouble::Unsupported(format!("the drawing was refused with {error:?}"))
	}
}

/// State a pass condition, and say what was seen when it does not hold.
macro_rules! require {
	($condition:expr, $($detail:tt)*) => {
		if !($condition) {
			return Err($crate::Trouble::Failed(alloc::format!($($detail)*)));
		}
	};
}

/// The rendered target, and the questions a scene asks of it.
pub struct Frame {
	image: OwnedImage,
}

impl Frame {
	/// The stored bytes of one pixel: red, green, blue, alpha, each `value * 255` because the target
	/// is linear and its alpha is straight.
	pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
		let view = self.image.view();
		let Some(row) = view.row(y) else { return [0; 4] };
		let start = x as usize * 4;
		match row.get(start..start + 4) {
			Some(pixel) => [pixel[0], pixel[1], pixel[2], pixel[3]],
			None => [0; 4],
		}
	}

	pub fn alpha(&self, x: u32, y: u32) -> u8 {
		self.pixel(x, y)[3]
	}

	/// Whether a pixel is fully covered, which is the commonest question a geometry scene asks.
	pub fn covered(&self, x: u32, y: u32) -> bool {
		self.alpha(x, y) == 255
	}

	/// Whether nothing was drawn here at all.
	pub fn empty(&self, x: u32, y: u32) -> bool {
		self.alpha(x, y) == 0
	}

	/// Whether a pixel the drawing covers WHOLLY holds exactly a straight colour at an alpha - the frozen edge
	/// policy, which compares such a pixel exactly. See `stored`.
	pub fn holds(&self, x: u32, y: u32, colour: [f32; 3], alpha: f32) -> bool {
		self.pixel(x, y) == [stored(colour[0], x, y), stored(colour[1], x, y), stored(colour[2], x, y), byte(alpha)]
	}
}

/// THE FROZEN THRESHOLDS, as the numbers a comparison applies. Each is the `render2d` entry of
/// `graphics_profile::thresholds::THRESHOLDS` it is named after, and a host test holds that entry's text to
/// the number here - so the suite cannot drift from the freeze without a test saying so.
pub mod threshold {
	/// "antialiasing coverage": per pixel, against the analytic area.
	pub const COVERAGE_PER_PIXEL: f64 = 2.0 / 255.0;
	/// "antialiasing coverage": the mean over the partially covered pixels.
	pub const COVERAGE_MEAN: f64 = 1.0 / 255.0;
	/// "a filter kernel's output": relative, against the kernel evaluated at f64, per channel in linear space.
	pub const FILTER_RELATIVE: f64 = 1e-3;
}

/// A TARGET OF SEVERAL TILES on a side: the reference backend replays a frame sixty-four pixels at a time,
/// so a drawing on this one is cut by tile boundaries at 64 and 128 on both axes - which is where an effect
/// that reads past the pixel it writes, or a group composited once per tile, shows a seam.
pub const WIDE: u32 = 160;

/// A value and the byte it is stored as in a linear target.
pub fn byte(value: f32) -> u8 {
	(value * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

/// Is a stored byte a PARTIALLY COVERED pixel's expected value, within the frozen coverage tolerance?
///
/// TWO PARTS IN 255 AND NOT ZERO, because that is the frozen per-pixel bound for a pixel whose analytic
/// coverage is a fraction - and ONLY for such a pixel. A fully covered or an empty one is compared
/// exactly, by `stored` and `Frame::holds`: a fully covered pixel that differs at all is a colour error
/// wearing an antialiasing tolerance.
pub fn near(seen: u8, expected: f32) -> bool {
	let expected = byte(expected) as i32;
	(seen as i32 - expected).abs() <= 2
}

/// THE BYTE A FULLY COVERED PIXEL STORES A COLOUR CHANNEL AS, EXACTLY.
///
/// WHAT LIES BETWEEN THE ARITHMETIC AND THE BYTE is the target's own quantisation: the value in code units
/// moved by the profile's ORDERED DITHER at that pixel and rounded. A value that is a whole code value comes
/// back as itself whatever the dither, and one that is not lands on the code below it at one pixel and the
/// one above at the next - so the exact comparison the edge policy asks for is against this and not against
/// the value rounded. Alpha is not dithered: its byte is `byte`.
pub fn stored(value: f32, x: u32, y: u32) -> u8 {
	graphics_core::pixel::quantise_u8(value + graphics_core::pixel::dither_offset(x, y) * (1.0 / 255.0))
}

/// Compare EVERY PIXEL of a frame with what it should hold, which is the frozen comparison mask: no border
/// excluded and no pixel skipped. The first that differs is the answer.
pub fn every_pixel(frame: &Frame, expected: impl Fn(u32, u32) -> [u8; 4]) -> Result<(), crate::Trouble> {
	let extent = frame.image.layout().extent;
	for y in 0..extent.height {
		for x in 0..extent.width {
			let (seen, wanted) = (frame.pixel(x, y), expected(x, y));
			require!(seen == wanted, "pixel ({x}, {y}) holds {seen:?} and should hold exactly {wanted:?}");
		}
	}
	Ok(())
}

/// A frame in the comparison space the thresholds NAME - linear Rec. 709, premultiplied, at f32 - for a scene
/// whose expectation is a kernel evaluated at f64, which an eight-bit target would quantise before it could
/// be compared.
pub struct Linear {
	image: OwnedImage,
}

impl Linear {
	/// One pixel, premultiplied: red, green, blue, alpha.
	pub fn value(&self, x: u32, y: u32) -> [f32; 4] {
		let view = self.image.view();
		let mut out = [0.0f32; 4];
		if let Some(pixel) = view.row(y).and_then(|row| row.get(x as usize * 16..x as usize * 16 + 16)) {
			for (channel, bytes) in out.iter_mut().zip(pixel.chunks_exact(4)) {
				*channel = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
			}
		}
		out
	}

	pub fn extent(&self) -> Extent2D {
		self.image.layout().extent
	}

	/// Compare EVERY PIXEL with what it should be, at f64. A pixel the expectation marks exact must be it to
	/// the bit in every channel, which is the edge policy for a pixel the drawing covers wholly or not at all;
	/// any other is a filter's output, and each of its channels must be within the frozen kernel tolerance.
	pub fn compare(&self, expected: impl Fn(u32, u32) -> ([f64; 4], bool)) -> Result<(), crate::Trouble> {
		let extent = self.extent();
		for y in 0..extent.height {
			for x in 0..extent.width {
				let (wanted, exact) = expected(x, y);
				let seen = self.value(x, y);
				for (channel, (seen, wanted)) in seen.iter().zip(wanted.iter()).enumerate() {
					let difference = (*seen as f64 - wanted).abs();
					let within = if exact { *seen as f64 == *wanted } else { difference <= threshold::FILTER_RELATIVE * wanted.abs() };
					require!(within, "pixel ({x}, {y}) channel {channel} is {seen} and should be {wanted}{}", if exact { " exactly" } else { ", within the frozen 1e-3 relative" });
				}
			}
		}
		Ok(())
	}
}

/// Draw into a target in the named comparison space and hand back its values.
pub fn draw_linear(width: u32, height: u32, build: impl FnOnce(&mut Canvas) -> Result<(), render2d::Error>) -> Result<Linear, Trouble> {
	let mut canvas = Canvas::new();
	build(&mut canvas).map_err(|error| Trouble::Failed(format!("the scene could not be recorded: {error:?}")))?;
	let list = canvas.finish().map_err(|error| Trouble::Failed(format!("the scene's recording is unbalanced: {error:?}")))?;
	let semantics = ImageSemantics::Color { color_space: ColorSpace::SrgbLinear, alpha_mode: AlphaMode::Premultiplied };
	let storage = PixelStorage::Known(PixelFormat::R32G32B32A32Float);
	let layout = ImageLayout::new(Extent2D::new(width, height), width * 16, storage, RowOrigin::TopLeft, semantics).map_err(|error| Trouble::Failed(format!("the target layout was refused: {error:?}")))?;
	let mut image = OwnedImage::new(layout).map_err(|error| Trouble::Failed(format!("the target could not be allocated: {error:?}")))?;
	let description = TargetDescription { extent: image.layout().extent, format: PixelFormat::R32G32B32A32Float, color_space: ColorSpace::SrgbLinear, scale: 1.0, luminance: OutputLuminance::UNKNOWN };
	let mut backend = Soft2d::new();
	let prepared = backend.prepare(&list, &description).map_err(|error| Trouble::Unsupported(format!("the backend refused to prepare this drawing: {error:?}")))?;
	{
		let mut view = image.view_mut();
		backend.render(&prepared, &mut view).map_err(|error| Trouble::Unsupported(format!("the backend refused to draw this: {error:?}")))?;
	}
	Ok(Linear { image })
}

/// THE GAUSSIAN THE PROFILE NAMES, EVALUATED AT f64: `exp(-d^2 / 2 sigma^2)` at every whole-pixel offset out
/// to three standard deviations rounded up - the reach the profile's bounds map gives a blur, and never less
/// than one pixel or more than the profile's largest filter radius - normalised to sum to one. The first entry
/// is the farthest tap on the negative side.
pub fn gaussian(sigma: f64) -> alloc::vec::Vec<f64> {
	let ceiling = graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS.max_filter_radius as f64;
	let radius = libm::ceil(sigma * 3.0).clamp(1.0, ceiling) as i64;
	let weights: alloc::vec::Vec<f64> = (-radius..=radius).map(|distance| libm::exp(-((distance * distance) as f64) / (2.0 * sigma * sigma))).collect();
	let total: f64 = weights.iter().sum();
	weights.into_iter().map(|weight| weight / total).collect()
}

/// One pass of a separable blur at f64: the weighted sum of `value` over the kernel's taps around `at`.
pub fn blurred(kernel: &[f64], at: i64, value: impl Fn(i64) -> f64) -> f64 {
	let radius = (kernel.len() as i64 - 1) / 2;
	kernel.iter().enumerate().map(|(index, weight)| weight * value(at + index as i64 - radius)).sum()
}

/// Draw into a target of this size and hand back the pixels.
pub fn draw(width: u32, height: u32, build: impl FnOnce(&mut Canvas) -> Result<(), render2d::Error>) -> Result<Frame, Trouble> {
	draw_with(width, height, &NoImages, &soft2d::glyph::NoGlyphs, build)
}

/// The same, for a scene that references images or glyphs.
pub fn draw_with(width: u32, height: u32, images: &dyn ImageSource, glyphs: &dyn GlyphProvider, build: impl FnOnce(&mut Canvas) -> Result<(), render2d::Error>) -> Result<Frame, Trouble> {
	let mut canvas = Canvas::new();
	build(&mut canvas).map_err(|error| Trouble::Failed(format!("the scene could not be recorded: {error:?}")))?;
	let list = canvas.finish().map_err(|error| Trouble::Failed(format!("the scene's recording is unbalanced: {error:?}")))?;
	render(width, height, &list, images, glyphs)
}

/// Replay a list into a fresh target.
pub fn render(width: u32, height: u32, list: &DrawList, images: &dyn ImageSource, glyphs: &dyn GlyphProvider) -> Result<Frame, Trouble> {
	let mut image = linear_target(width, height)?;
	let description = TargetDescription { extent: image.layout().extent, format: PixelFormat::R8G8B8A8Unorm, color_space: ColorSpace::SrgbLinear, scale: 1.0, luminance: OutputLuminance::UNKNOWN };
	let mut backend = Soft2d::new().with_images(images).with_glyphs(glyphs);
	let prepared = backend.prepare(list, &description).map_err(|error| Trouble::Unsupported(format!("the backend refused to prepare this drawing: {error:?}")))?;
	{
		let mut view = image.view_mut();
		backend.render(&prepared, &mut view).map_err(|error| Trouble::Unsupported(format!("the backend refused to draw this: {error:?}")))?;
	}
	Ok(Frame { image })
}

/// A target in an ARBITRARY colour space, for the one scene that is about the conversion itself.
pub fn render_in(width: u32, height: u32, space: ColorSpace, list: &DrawList) -> Result<Frame, Trouble> {
	let semantics = ImageSemantics::Color { color_space: space, alpha_mode: AlphaMode::Straight };
	let storage = PixelStorage::Known(PixelFormat::R8G8B8A8Unorm);
	let layout = ImageLayout::new(Extent2D::new(width, height), width * 4, storage, RowOrigin::TopLeft, semantics).map_err(|error| Trouble::Failed(format!("the target layout was refused: {error:?}")))?;
	let mut image = OwnedImage::new(layout).map_err(|error| Trouble::Failed(format!("the target could not be allocated: {error:?}")))?;
	let description = TargetDescription { extent: image.layout().extent, format: PixelFormat::R8G8B8A8Unorm, color_space: space, scale: 1.0, luminance: OutputLuminance::UNKNOWN };
	let mut backend = Soft2d::new();
	let prepared = backend.prepare(list, &description).map_err(|error| Trouble::Unsupported(format!("the backend refused to prepare this drawing: {error:?}")))?;
	{
		let mut view = image.view_mut();
		backend.render(&prepared, &mut view).map_err(|error| Trouble::Unsupported(format!("the backend refused to draw this: {error:?}")))?;
	}
	Ok(Frame { image })
}

fn linear_target(width: u32, height: u32) -> Result<OwnedImage, Trouble> {
	let semantics = ImageSemantics::Color { color_space: ColorSpace::SrgbLinear, alpha_mode: AlphaMode::Straight };
	let storage = PixelStorage::Known(PixelFormat::R8G8B8A8Unorm);
	let layout = ImageLayout::new(Extent2D::new(width, height), width * 4, storage, RowOrigin::TopLeft, semantics).map_err(|error| Trouble::Failed(format!("the target layout was refused: {error:?}")))?;
	OwnedImage::new(layout).map_err(|error| Trouble::Failed(format!("the target could not be allocated: {error:?}")))
}

/// A scene that only has to not be refused, for the few features whose whole content is that the
/// drawing is accepted and produces the shape it names.
pub fn checked(condition: bool, detail: &str) -> Result<(), Trouble> {
	if condition { Ok(()) } else { Err(Trouble::Failed(String::from(detail))) }
}

/// A solid paint in LINEAR light, which is the space every scene states its expectations in.
pub fn solid(red: f32, green: f32, blue: f32, alpha: f32) -> render2d::paint::Paint {
	render2d::paint::Paint::Solid(render2d::paint::Color::new(red, green, blue, alpha, ColorSpace::SrgbLinear))
}

/// Opaque white, the commonest paint in a scene whose question is about coverage rather than colour.
pub fn white() -> render2d::paint::Paint {
	solid(1.0, 1.0, 1.0, 1.0)
}

/// A rectangle as a path.
pub fn rect_path(rect: graphics_core::geom::RectF) -> render2d::path::Path {
	let mut builder = render2d::path::PathBuilder::new();
	let _ = builder.add_rect(rect);
	builder.finish()
}
