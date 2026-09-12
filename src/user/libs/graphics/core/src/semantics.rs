//! WHAT AN IMAGE MEANS, not only how it is stored.
//!
//! A COLOUR-MANAGED PIPELINE THAT CANNOT TELL A COLOUR FROM A MEASUREMENT WILL TRANSFORM THE
//! MEASUREMENT. A normal map, a roughness map, a coverage mask and a depth buffer are all just bytes,
//! and running any of them through an sRGB decode corrupts them silently - the artefact looks like a
//! lighting bug, which is where the days go.
//!
//! ALPHA COMPOSITING EXISTS ONLY IN `Color`, and the type says so: a non-colour image cannot carry an
//! ignored alpha mode or an ignored colour space, because a field that is ignored is a field somebody
//! eventually sets and expects to matter.

use graphics_profile::image::{Operation, Semantics as ProfileSemantics, operations};

use crate::color::ColorSpace;
use crate::format::AlphaMode;

/// What a mask's numbers mean, which is not obvious from the fact that it is one channel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MaskInterpretation {
	/// Coverage: how much of the pixel the shape covers, already linear.
	Coverage,
	/// A stencil: zero or not, with no intermediate meaning. Filtering one produces a value that
	/// means neither.
	Stencil,
}

/// Which way a normal map's green axis points.
///
/// THE TWO CONVENTIONS LOOK IDENTICAL UNTIL THE LIGHT MOVES. A map authored for one and read as the
/// other lights every bump as a dent, which reads as an art bug rather than a convention mismatch -
/// so it is carried rather than assumed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NormalMapConvention {
	/// Green points UP in texture space: OpenGL's.
	GreenUp,
	/// Green points DOWN: Direct3D's.
	GreenDown,
}

/// What an image's numbers mean.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImageSemantics {
	/// Colour, in a stated space and alpha mode. THE ONLY kind a transfer function applies to, and
	/// the only kind that composites.
	Color {
		color_space: ColorSpace,
		alpha_mode: AlphaMode,
	},
	/// A measurement carried as a number.
	Data,
	NormalMap {
		convention: NormalMapConvention,
	},
	Depth,
	Mask {
		interpretation: MaskInterpretation,
	},
}

impl ImageSemantics {
	/// The profile's own kind, which is what the operation table is keyed by.
	pub const fn kind(&self) -> ProfileSemantics {
		match self {
			ImageSemantics::Color { .. } => ProfileSemantics::Color,
			ImageSemantics::Data => ProfileSemantics::Data,
			ImageSemantics::NormalMap { .. } => ProfileSemantics::Normal,
			ImageSemantics::Depth => ProfileSemantics::Depth,
			ImageSemantics::Mask { .. } => ProfileSemantics::Mask,
		}
	}

	/// Whether an operation may be applied to an image of this kind, straight from the frozen table.
	pub fn admits(&self, operation: Operation) -> bool {
		operations(self.kind()).contains(&operation)
	}

	/// Whether a transfer function may be applied. The question the whole type exists to answer.
	pub const fn is_colour(&self) -> bool {
		matches!(self, ImageSemantics::Color { .. })
	}

	/// The colour space, for the one kind that has one.
	pub const fn color_space(&self) -> Option<ColorSpace> {
		match self {
			ImageSemantics::Color { color_space, .. } => Some(*color_space),
			_ => None,
		}
	}

	/// The alpha mode, for the one kind that has one.
	pub const fn alpha_mode(&self) -> Option<AlphaMode> {
		match self {
			ImageSemantics::Color { alpha_mode, .. } => Some(*alpha_mode),
			_ => None,
		}
	}
}
