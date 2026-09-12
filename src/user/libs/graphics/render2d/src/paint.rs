//! WHAT A SHAPE IS FILLED WITH.
//!
//! A COLOUR IS NOT ENOUGH AND NEVER WAS. A chart is gradients, a map is image patterns, a button is a
//! gradient with a border, and an API whose only paint is a colour makes each of those a texture
//! somebody generated somewhere else - which is the private-rasteriser failure one level up.
//!
//! EVERY PAINT CARRIES ITS OWN TRANSFORM, because a gradient's geometry is not the shape's. A
//! gradient rotated with the shape and one rotated independently are both wanted, and an API that
//! only has the first makes the second a pre-rotated image.

use graphics_core::ColorSpace;
use graphics_core::geom::{PointF, RectF};

use crate::resource::{ImageHandle, PaintHandle};
use crate::transform::Transform;

/// A colour, in a stated space, UNPREMULTIPLIED.
///
/// UNPREMULTIPLIED AT THE API AND PREMULTIPLIED IN THE PIPELINE. A caller says "half-transparent red"
/// as `(1, 0, 0, 0.5)`, which is what they mean; the multiplication happens once, on the way in, so
/// that two callers cannot disagree about whether they had already done it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Color {
	pub red: f32,
	pub green: f32,
	pub blue: f32,
	pub alpha: f32,
	pub space: ColorSpace,
}

impl Color {
	pub const fn new(red: f32, green: f32, blue: f32, alpha: f32, space: ColorSpace) -> Self {
		Self { red, green, blue, alpha, space }
	}

	/// Opaque black in linear sRGB, which is what an unset paint is rather than transparent: a shape
	/// drawn with a paint nobody set should be VISIBLE and obviously wrong, not invisible and
	/// mysterious.
	pub const BLACK: Self = Self::new(0.0, 0.0, 0.0, 1.0, ColorSpace::SrgbLinear);
	pub const TRANSPARENT: Self = Self::new(0.0, 0.0, 0.0, 0.0, ColorSpace::SrgbLinear);
}

/// One stop of a gradient.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct GradientStop {
	/// Where along the gradient, in `0..=1`.
	pub offset: f32,
	pub color: Color,
}

/// How a gradient or a pattern continues past its ends, and how an image is sampled.
///
/// BOTH COME FROM THE SHARED SAMPLER. A spread mode is a rule about texel coordinates and a quality
/// is a filter over them; both are implemented once in `graphics-core` and used by this API, by the
/// display path and by every backend, so declaring either here would be a second list.
pub use graphics_core::sample::{Quality as ImageQuality, Spread as SpreadMode};

/// What a shape is filled with.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Paint {
	Solid(Color),
	/// A gradient between two points, with its stops in the resource table.
	Linear {
		from: PointF,
		to: PointF,
		stops: PaintHandle,
		spread: SpreadMode,
		transform: Transform,
	},
	/// A gradient between two circles.
	Radial {
		from: PointF,
		from_radius: f32,
		to: PointF,
		to_radius: f32,
		stops: PaintHandle,
		spread: SpreadMode,
		transform: Transform,
	},
	/// An ANGULAR gradient about a centre. In the profile, so it is implemented rather than typed and
	/// refused: a conic gradient is what a pie chart, a colour wheel and a loading spinner are.
	Conic {
		centre: PointF,
		start_angle: f32,
		end_angle: f32,
		stops: PaintHandle,
		spread: SpreadMode,
		transform: Transform,
	},
	/// An image, tiled by its spread mode.
	Image {
		image: ImageHandle,
		source: RectF,
		quality: ImageQuality,
		spread: SpreadMode,
		transform: Transform,
	},
}

impl Paint {
	/// The paint's own transform, which is not the shape's.
	pub fn transform(&self) -> Transform {
		match self {
			Paint::Solid(_) => Transform::IDENTITY,
			Paint::Linear { transform, .. } | Paint::Radial { transform, .. } | Paint::Conic { transform, .. } | Paint::Image { transform, .. } => *transform,
		}
	}

	/// Whether this paint can possibly be opaque. A backend uses it to skip reading the backdrop, and
	/// getting it wrong in the OTHER direction - claiming opacity a paint does not have - loses what
	/// was underneath, so the answer is conservative: only a solid colour with alpha one says yes.
	pub fn is_definitely_opaque(&self) -> bool {
		matches!(self, Paint::Solid(color) if color.alpha >= 1.0)
	}
}
