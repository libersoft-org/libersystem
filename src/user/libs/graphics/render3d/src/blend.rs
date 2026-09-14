//! THE BLEND STATE, ENUMERATED - because "blend state per attachment" cannot be conformance-tested
//! without knowing what a state may contain.
//!
//! THIS IS NOT 2D COMPOSITING AND THE TWO ARE DELIBERATELY DIFFERENT TYPES. `graphics-core` carries
//! the Porter-Duff operators and the separable and non-separable blend modes that `render2d` draws
//! with; those are a painter's model, where a source is composited ONTO a backdrop by a named
//! operation. This is the fixed-function blend a 3D pipeline has: two factors and an operation, per
//! channel group, per attachment. Re-using one for the other would make `SrcAlphaSaturate` a
//! compositing operator and `Multiply` a blend factor, and neither means anything in the other model.
//!
//! SEPARATE COLOUR AND ALPHA STATE, because the commonest correct blend needs it: premultiplied
//! source-over is `(One, OneMinusSrcAlpha)` for colour and `(One, OneMinusSrcAlpha)` for alpha, and
//! the commonest MISTAKE is a straight-alpha blend that gets alpha right and colour wrong. A state
//! that could not say the two separately would make that mistake unfixable.

use crate::error::Error;
use render_math::Vec4;

/// A blend factor: what a source or destination colour is multiplied by before the operation.
///
/// THE CLOSED SET. A factor outside it is not "unsupported"; there is no such thing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlendFactor {
	Zero,
	One,
	SrcColor,
	OneMinusSrcColor,
	DstColor,
	OneMinusDstColor,
	SrcAlpha,
	OneMinusSrcAlpha,
	DstAlpha,
	OneMinusDstAlpha,
	ConstantColor,
	OneMinusConstantColor,
	ConstantAlpha,
	OneMinusConstantAlpha,
	/// `min(src_alpha, 1 - dst_alpha)` on the colour channels and `1` on alpha. The asymmetry is the
	/// definition and not an implementation detail: it is what makes accumulating coverage saturate
	/// rather than overshoot, and a factor that applied the minimum to alpha as well would never
	/// reach an opaque result.
	SrcAlphaSaturate,
}

/// Every factor, for a conformance suite that has to range over them.
pub const ALL_BLEND_FACTORS: &[BlendFactor] = &[
	BlendFactor::Zero,
	BlendFactor::One,
	BlendFactor::SrcColor,
	BlendFactor::OneMinusSrcColor,
	BlendFactor::DstColor,
	BlendFactor::OneMinusDstColor,
	BlendFactor::SrcAlpha,
	BlendFactor::OneMinusSrcAlpha,
	BlendFactor::DstAlpha,
	BlendFactor::OneMinusDstAlpha,
	BlendFactor::ConstantColor,
	BlendFactor::OneMinusConstantColor,
	BlendFactor::ConstantAlpha,
	BlendFactor::OneMinusConstantAlpha,
	BlendFactor::SrcAlphaSaturate,
];

/// How the two factored terms are combined.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlendOp {
	Add,
	/// `source - destination`.
	Subtract,
	/// `destination - source`. Named for which way round it is rather than left to the reader.
	ReverseSubtract,
	/// The component-wise minimum. THE FACTORS ARE IGNORED, which is what the operation means in
	/// every fixed-function pipeline that has it, and is stated here because a state that set them
	/// and expected them to apply would be silently wrong.
	Min,
	/// The component-wise maximum, with the factors ignored for the same reason.
	Max,
}

pub const ALL_BLEND_OPS: &[BlendOp] = &[BlendOp::Add, BlendOp::Subtract, BlendOp::ReverseSubtract, BlendOp::Min, BlendOp::Max];

/// Which channels a fragment may write. A mask is applied AFTER blending, so a masked channel keeps
/// the destination's value rather than blending into it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ColorWriteMask {
	pub red: bool,
	pub green: bool,
	pub blue: bool,
	pub alpha: bool,
}

impl ColorWriteMask {
	pub const ALL: Self = Self { red: true, green: true, blue: true, alpha: true };
	pub const NONE: Self = Self { red: false, green: false, blue: false, alpha: false };

	pub const fn writes_nothing(self) -> bool {
		!(self.red || self.green || self.blue || self.alpha)
	}
}

impl Default for ColorWriteMask {
	fn default() -> Self {
		Self::ALL
	}
}

/// One channel group's blend equation: two factors and the operation that combines them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BlendEquation {
	pub source: BlendFactor,
	pub destination: BlendFactor,
	pub operation: BlendOp,
}

impl BlendEquation {
	/// The identity: the source replaces the destination.
	pub const REPLACE: Self = Self { source: BlendFactor::One, destination: BlendFactor::Zero, operation: BlendOp::Add };
	/// PREMULTIPLIED source-over, which is the blend a correctly authored texture wants.
	pub const PREMULTIPLIED_OVER: Self = Self { source: BlendFactor::One, destination: BlendFactor::OneMinusSrcAlpha, operation: BlendOp::Add };
}

/// One attachment's blend state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AttachmentBlend {
	/// PER-ATTACHMENT ENABLE, because a pass that writes colour to one target and object ids to
	/// another must blend the first and not the second - and an integer target cannot blend at all.
	pub enabled: bool,
	pub colour: BlendEquation,
	pub alpha: BlendEquation,
	pub write_mask: ColorWriteMask,
}

impl Default for AttachmentBlend {
	fn default() -> Self {
		Self { enabled: false, colour: BlendEquation::REPLACE, alpha: BlendEquation::REPLACE, write_mask: ColorWriteMask::ALL }
	}
}

/// The blend state of a whole pipeline: one per attachment, and the constant they share.
///
/// ONE CONSTANT FOR THE PIPELINE AND NOT ONE PER ATTACHMENT, which is what every pipeline that has
/// this state does, and is stated because the alternative is a reasonable thing to assume.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct BlendState<const ATTACHMENTS: usize> {
	pub attachments: [AttachmentBlend; ATTACHMENTS],
	/// The value `ConstantColor` and `ConstantAlpha` refer to, in the attachment's own numeric space.
	pub constant: Vec4,
}

impl<const ATTACHMENTS: usize> BlendState<ATTACHMENTS> {
	pub fn new() -> Self {
		Self { attachments: [AttachmentBlend::default(); ATTACHMENTS], constant: Vec4::ZERO }
	}

	/// Check this state against what each attachment's format admits.
	///
	/// AN INTEGER FORMAT CANNOT BLEND, which the profile states as a rule rather than a capability
	/// bit to be discovered: there is no correct answer to what the average of two object ids is.
	/// Enabling blending on one is a pipeline that contradicts itself, and the refusal names the
	/// format and what it was used as rather than saying "invalid".
	pub fn validate(&self, formats: &[&'static str]) -> Result<(), Error> {
		if formats.len() != ATTACHMENTS {
			return Err(Error::IncompatiblePipeline { reason: "the blend state has one entry per attachment and the pass has a different number" });
		}
		for (index, format) in formats.iter().enumerate() {
			if !self.attachments[index].enabled {
				continue;
			}
			let capability = graphics_profile::render3d_spec::COLOUR_FORMATS.iter().find(|entry| entry.name == *format);
			match capability {
				Some(entry) if entry.blendable => {}
				Some(_) => return Err(Error::UnsupportedFormat { format, used_as: "a blend target" }),
				None => return Err(Error::UnsupportedFormat { format, used_as: "a colour attachment" }),
			}
		}
		Ok(())
	}
}

impl<const ATTACHMENTS: usize> Default for BlendState<ATTACHMENTS> {
	fn default() -> Self {
		Self::new()
	}
}

/// One channel group's factor, evaluated.
///
/// THE ARITHMETIC IS HERE SO THERE IS ONE OF IT. A backend that wrote its own would be a second
/// answer to "what does `OneMinusDstAlpha` mean", and the conformance suite compares the two.
/// Everything is in the attachment's own numeric space, PREMULTIPLIED, which is the space the
/// profile's resolve rule is also stated in.
fn factor(which: BlendFactor, source: Vec4, destination: Vec4, constant: Vec4, is_alpha: bool) -> Vec4 {
	let saturate = {
		let value = source.w.min(1.0 - destination.w);
		// THE ASYMMETRY IS THE DEFINITION: the minimum on colour, one on alpha.
		Vec4::new(value, value, value, 1.0)
	};
	match which {
		BlendFactor::Zero => Vec4::ZERO,
		BlendFactor::One => Vec4::new(1.0, 1.0, 1.0, 1.0),
		BlendFactor::SrcColor => source,
		BlendFactor::OneMinusSrcColor => Vec4::new(1.0, 1.0, 1.0, 1.0).sub(source),
		BlendFactor::DstColor => destination,
		BlendFactor::OneMinusDstColor => Vec4::new(1.0, 1.0, 1.0, 1.0).sub(destination),
		BlendFactor::SrcAlpha => Vec4::new(source.w, source.w, source.w, source.w),
		BlendFactor::OneMinusSrcAlpha => Vec4::new(1.0 - source.w, 1.0 - source.w, 1.0 - source.w, 1.0 - source.w),
		BlendFactor::DstAlpha => Vec4::new(destination.w, destination.w, destination.w, destination.w),
		BlendFactor::OneMinusDstAlpha => Vec4::new(1.0 - destination.w, 1.0 - destination.w, 1.0 - destination.w, 1.0 - destination.w),
		BlendFactor::ConstantColor => constant,
		BlendFactor::OneMinusConstantColor => Vec4::new(1.0, 1.0, 1.0, 1.0).sub(constant),
		BlendFactor::ConstantAlpha => Vec4::new(constant.w, constant.w, constant.w, constant.w),
		BlendFactor::OneMinusConstantAlpha => Vec4::new(1.0 - constant.w, 1.0 - constant.w, 1.0 - constant.w, 1.0 - constant.w),
		BlendFactor::SrcAlphaSaturate => {
			let _ = is_alpha;
			saturate
		}
	}
}

/// Apply one attachment's blend state to one fragment.
///
/// THE ORDER IS BLEND, THEN MASK. A masked channel keeps the destination unchanged rather than
/// keeping a blended value that was not written - which is the same answer for most states and a
/// different one for `Min`, `Max` and anything reading `DstColor`.
pub fn blend(state: &AttachmentBlend, source: Vec4, destination: Vec4, constant: Vec4) -> Vec4 {
	if !state.enabled {
		return mask(state.write_mask, source, destination);
	}
	let colour = combine(state.colour, source, destination, constant, false);
	let alpha = combine(state.alpha, source, destination, constant, true);
	mask(state.write_mask, Vec4::new(colour.x, colour.y, colour.z, alpha.w), destination)
}

fn combine(equation: BlendEquation, source: Vec4, destination: Vec4, constant: Vec4, is_alpha: bool) -> Vec4 {
	match equation.operation {
		// MIN AND MAX IGNORE THE FACTORS, which is what the operation means and is why they are not
		// written as `Add` with different factors.
		BlendOp::Min => Vec4::new(source.x.min(destination.x), source.y.min(destination.y), source.z.min(destination.z), source.w.min(destination.w)),
		BlendOp::Max => Vec4::new(source.x.max(destination.x), source.y.max(destination.y), source.z.max(destination.z), source.w.max(destination.w)),
		operation => {
			let left = source.mul(factor(equation.source, source, destination, constant, is_alpha));
			let right = destination.mul(factor(equation.destination, source, destination, constant, is_alpha));
			match operation {
				BlendOp::Add => left.add(right),
				BlendOp::Subtract => left.sub(right),
				BlendOp::ReverseSubtract => right.sub(left),
				BlendOp::Min | BlendOp::Max => unreachable!("handled above"),
			}
		}
	}
}

fn mask(write: ColorWriteMask, value: Vec4, destination: Vec4) -> Vec4 {
	Vec4::new(if write.red { value.x } else { destination.x }, if write.green { value.y } else { destination.y }, if write.blue { value.z } else { destination.z }, if write.alpha { value.w } else { destination.w })
}
