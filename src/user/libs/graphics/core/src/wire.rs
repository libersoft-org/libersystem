//! THE ONE DOOR FROM THE WIRE INTO THIS LIBRARY.
//!
//! `liber:graphics@1` carries the value types across a channel; this module is where they stop being
//! somebody else's bytes and become this library's validated ones. Every field in a wire descriptor
//! is a number a hostile or merely wrong peer chose: a pitch that does not cover a row, an extent
//! whose product overflows, an alpha mode the format has no channel for. `ImageLayout::new` already
//! refuses each of those; what this adds is that there is NO OTHER WAY IN.
//!
//! WHY A CONVERSION AND NOT A SHARED TYPE. Treating the wire record as the program's type deletes
//! the boundary that proves a descriptor was checked - the checked and the unchecked value become
//! the same value, and every function that takes one has to re-establish what the last one already
//! proved. It also ties the in-process layout to a schema this project versions independently: the
//! wire is a compatibility surface with its own rules, and the in-process type is free to hold what
//! a renderer needs.
//!
//! WHICH DIRECTION IS CHECKED, AND WHICH IS NOT. Wire to core is a `TryFrom` and can refuse. Core to
//! wire is a `From` and cannot: a validated layout is by construction expressible, and a conversion
//! that could fail on the way OUT would be a validated value this library could not describe.

use crate::Error;
use crate::color::ColorSpace;
use crate::format::{AlphaMode, PixelFormat, PixelStorage};
use crate::geom::Extent2D;
use crate::layout::{ImageLayout, RowOrigin};
use crate::semantics::ImageSemantics;

use graphics_proto::generated::liber::graphics::v1 as wire;

/// The wire pixel format, as the frozen list this library holds.
///
/// TOTAL, WITH NO FALLBACK ARM. The wire enumeration and the profile's list are the same twelve in
/// the same order, and a `_ =>` here would be the place a thirteenth wire format silently became one
/// of the twelve. If the two ever diverge this stops compiling, which is the whole point.
fn format_of(format: wire::PixelFormat) -> PixelFormat {
	match format {
		wire::PixelFormat::A8Unorm => PixelFormat::A8Unorm,
		wire::PixelFormat::R8Unorm => PixelFormat::R8Unorm,
		wire::PixelFormat::R8g8Unorm => PixelFormat::R8G8Unorm,
		wire::PixelFormat::B8g8r8x8Unorm => PixelFormat::B8G8R8X8Unorm,
		wire::PixelFormat::R8g8b8x8Unorm => PixelFormat::R8G8B8X8Unorm,
		wire::PixelFormat::B8g8r8a8Unorm => PixelFormat::B8G8R8A8Unorm,
		wire::PixelFormat::R8g8b8a8Unorm => PixelFormat::R8G8B8A8Unorm,
		wire::PixelFormat::R10g10b10a2Unorm => PixelFormat::R10G10B10A2Unorm,
		wire::PixelFormat::R16g16b16a16Unorm => PixelFormat::R16G16B16A16Unorm,
		wire::PixelFormat::R16g16b16a16Float => PixelFormat::R16G16B16A16Float,
		wire::PixelFormat::R32Uint => PixelFormat::R32Uint,
		wire::PixelFormat::R32g32b32a32Float => PixelFormat::R32G32B32A32Float,
	}
}

fn wire_format_of(format: PixelFormat) -> wire::PixelFormat {
	match format {
		PixelFormat::A8Unorm => wire::PixelFormat::A8Unorm,
		PixelFormat::R8Unorm => wire::PixelFormat::R8Unorm,
		PixelFormat::R8G8Unorm => wire::PixelFormat::R8g8Unorm,
		PixelFormat::B8G8R8X8Unorm => wire::PixelFormat::B8g8r8x8Unorm,
		PixelFormat::R8G8B8X8Unorm => wire::PixelFormat::R8g8b8x8Unorm,
		PixelFormat::B8G8R8A8Unorm => wire::PixelFormat::B8g8r8a8Unorm,
		PixelFormat::R8G8B8A8Unorm => wire::PixelFormat::R8g8b8a8Unorm,
		PixelFormat::R10G10B10A2Unorm => wire::PixelFormat::R10g10b10a2Unorm,
		PixelFormat::R16G16B16A16Unorm => wire::PixelFormat::R16g16b16a16Unorm,
		PixelFormat::R16G16B16A16Float => wire::PixelFormat::R16g16b16a16Float,
		PixelFormat::R32Uint => wire::PixelFormat::R32Uint,
		PixelFormat::R32G32B32A32Float => wire::PixelFormat::R32g32b32a32Float,
	}
}

impl From<wire::PixelFormat> for PixelFormat {
	/// The wire format as this library's, for a consumer that holds one field rather than a whole
	/// descriptor - a surface description carries its format beside an extent and a pitch, and
	/// taking it apart by hand at each such place is the second table this module exists to prevent.
	fn from(format: wire::PixelFormat) -> Self {
		format_of(format)
	}
}

impl From<PixelFormat> for wire::PixelFormat {
	fn from(format: PixelFormat) -> Self {
		wire_format_of(format)
	}
}

fn alpha_of(alpha: wire::AlphaMode) -> AlphaMode {
	match alpha {
		wire::AlphaMode::Opaque => AlphaMode::Opaque,
		wire::AlphaMode::Straight => AlphaMode::Straight,
		wire::AlphaMode::Premultiplied => AlphaMode::Premultiplied,
	}
}

fn wire_alpha_of(alpha: AlphaMode) -> wire::AlphaMode {
	match alpha {
		AlphaMode::Opaque => wire::AlphaMode::Opaque,
		AlphaMode::Straight => wire::AlphaMode::Straight,
		AlphaMode::Premultiplied => wire::AlphaMode::Premultiplied,
	}
}

fn space_of(space: wire::ColorSpace) -> ColorSpace {
	match space {
		wire::ColorSpace::Srgb => ColorSpace::Srgb,
		wire::ColorSpace::SrgbLinear => ColorSpace::SrgbLinear,
		wire::ColorSpace::DisplayP3 => ColorSpace::DisplayP3,
		wire::ColorSpace::DisplayP3Linear => ColorSpace::DisplayP3Linear,
		wire::ColorSpace::Rec2020 => ColorSpace::Rec2020,
		wire::ColorSpace::Rec2020Linear => ColorSpace::Rec2020Linear,
		wire::ColorSpace::Rec2020Pq => ColorSpace::Rec2020Pq,
		wire::ColorSpace::Rec2020Hlg => ColorSpace::Rec2020Hlg,
	}
}

fn wire_space_of(space: ColorSpace) -> wire::ColorSpace {
	match space {
		ColorSpace::Srgb => wire::ColorSpace::Srgb,
		ColorSpace::SrgbLinear => wire::ColorSpace::SrgbLinear,
		ColorSpace::DisplayP3 => wire::ColorSpace::DisplayP3,
		ColorSpace::DisplayP3Linear => wire::ColorSpace::DisplayP3Linear,
		ColorSpace::Rec2020 => wire::ColorSpace::Rec2020,
		ColorSpace::Rec2020Linear => wire::ColorSpace::Rec2020Linear,
		ColorSpace::Rec2020Pq => wire::ColorSpace::Rec2020Pq,
		ColorSpace::Rec2020Hlg => wire::ColorSpace::Rec2020Hlg,
	}
}

fn origin_of(origin: wire::RowOrigin) -> RowOrigin {
	match origin {
		wire::RowOrigin::TopLeft => RowOrigin::TopLeft,
		wire::RowOrigin::BottomLeft => RowOrigin::BottomLeft,
	}
}

fn wire_origin_of(origin: RowOrigin) -> wire::RowOrigin {
	match origin {
		RowOrigin::TopLeft => wire::RowOrigin::TopLeft,
		RowOrigin::BottomLeft => wire::RowOrigin::BottomLeft,
	}
}

impl TryFrom<&wire::ImageLayout> for ImageLayout {
	type Error = Error;

	/// THE CHECK IS `ImageLayout::new`'S AND IS NOT REPEATED HERE.
	///
	/// A conversion that re-validated would be a second copy of the rule, and the copy nobody
	/// maintains is the one that admits what the other refuses. What this function does is decide
	/// what the wire's fields MEAN - a `pixel-format` is a known storage, an alpha mode and a colour
	/// space together are colour semantics - and hand them to the one constructor.
	fn try_from(layout: &wire::ImageLayout) -> Result<Self, Self::Error> {
		let extent = Extent2D { width: layout.size.width, height: layout.size.height };
		let storage = PixelStorage::Known(format_of(layout.format));
		// COLOUR, ALWAYS, AND THAT IS A LIMIT WORTH STATING. The wire record carries a colour space
		// and an alpha mode and has no way to say "this is depth" or "this is a mask" - so what
		// crosses this boundary is a colour image, and a caller with a depth or data image builds
		// its layout directly rather than describing it on a wire that cannot.
		let semantics = ImageSemantics::Color { color_space: space_of(layout.color_space), alpha_mode: alpha_of(layout.alpha) };
		ImageLayout::new(extent, layout.pitch, storage, origin_of(layout.origin), semantics)
	}
}

impl TryFrom<wire::ImageLayout> for ImageLayout {
	type Error = Error;

	fn try_from(layout: wire::ImageLayout) -> Result<Self, Self::Error> {
		ImageLayout::try_from(&layout)
	}
}

impl ImageLayout {
	/// The wire description of a validated layout.
	///
	/// INFALLIBLE FOR COLOUR AND `None` FOR EVERYTHING ELSE, because the wire record has no way to
	/// carry depth, data, a normal map or a mask: it holds a colour space and an alpha mode and
	/// nothing that could say the image is not colour. Answering `None` is the honest form - the
	/// alternative is inventing a colour space for a depth buffer, which is exactly the kind of
	/// quiet nonsense a boundary exists to stop.
	pub fn to_wire(&self) -> Option<wire::ImageLayout> {
		let PixelStorage::Known(format) = self.storage else { return None };
		let ImageSemantics::Color { color_space, alpha_mode } = self.semantics else { return None };
		Some(wire::ImageLayout { size: wire::Extent2d { width: self.extent.width, height: self.extent.height }, pitch: self.pitch, format: wire_format_of(format), alpha: wire_alpha_of(alpha_mode), color_space: wire_space_of(color_space), origin: wire_origin_of(self.origin) })
	}
}
