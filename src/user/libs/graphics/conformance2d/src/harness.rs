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
}

/// A value and the byte it is stored as in a linear target.
pub fn byte(value: f32) -> u8 {
	(value * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

/// Is a stored byte the value the scene expects, within a rounding?
///
/// THE TOLERANCE IS TWO PARTS IN 255 AND NOT ZERO. The working intermediate is half-float, the
/// coverage is a fraction and the final quantisation rounds, so an exact comparison would be a
/// comparison against this backend's rounding rather than against the profile's arithmetic. Two parts
/// is smaller than any difference between two definitions the profile distinguishes.
pub fn near(seen: u8, expected: f32) -> bool {
	let expected = byte(expected) as i32;
	(seen as i32 - expected).abs() <= 2
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
