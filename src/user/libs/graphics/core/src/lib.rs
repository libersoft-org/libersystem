//! `graphics-core`: the canonical image model every layer above shares.
//!
//! WHAT IT IS FOR. `render2d` and `render3d` both need images, colour and sampling; a compositor needs
//! them; a decoder produces them; a screenshot is one. Without a shared model each of those grows its
//! own, and the tree already had four - which is the reason `framebuffer` came to mean five things.
//!
//! WHAT IT IS NOT. It is not a drawing API, not a backend, and not a display connection. It carries
//! the LAYOUT and the MEANING of pixels and the arithmetic between two colour spaces; everything that
//! draws sits above it.
//!
//! THE PROFILE IS THE LIST AND THIS IS THE CODE. Every format, colour constant, byte-span rule and
//! operation table comes from `graphics-profile`'s frozen registry rather than being restated here,
//! and a fixture holds the two to describing the same things. A second copy of a list is a second
//! answer.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod color;
pub mod composite;
pub mod format;
pub mod geom;
pub mod layout;
pub mod owned;
pub mod pixel;
pub mod planar;
pub mod sample;
pub mod semantics;
pub mod view;
// THE ONE DOOR FROM THE WIRE. `liber:graphics@1`'s descriptors become this library's validated
// types here and nowhere else - see the module.
pub mod wire;

pub use color::ColorSpace;
pub use composite::{BlendMode, Operator, composite};
pub use format::{AlphaMode, PackedChannel, PackedRgbLayout, PixelFormat, PixelStorage};
pub use geom::{Extent2D, PixelOffset, PixelRect, PointF, RectF};
pub use layout::{ImageLayout, RowOrigin};
pub use owned::OwnedImage;
pub use pixel::{Rgba, Working};
pub use planar::{MultiPlaneLayout, MultiPlaneView, PlanarFormat, YuvMatrix, YuvRange};
pub use sample::{Pyramid, Quality, Sampler, Spread};
pub use semantics::{ImageSemantics, MaskInterpretation, NormalMapConvention};
pub use view::{ImageView, ImageViewMut};

/// Why an image could not be described, borrowed or allocated.
///
/// NAMED RATHER THAN COUNTED, because each is acted on differently: a short buffer is a caller's
/// arithmetic, an overlapping channel mask is firmware this system does not understand, and an
/// allocation failure is a resource decision.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
	/// A storage the profile does not carry, or a packed layout that is not one.
	UnknownFormat,
	/// A packed layout whose channels share bits. Two channels sharing a bit means one changes when
	/// the other is written, which is a mode nobody tested.
	OverlappingChannels,
	/// A zero width or height. A refusal rather than an empty image: every consumer that divides by an
	/// extent would have to check, and one of them will not.
	ZeroExtent,
	/// A pitch below the minimum row bytes, which is a layout that cannot hold its own rows.
	PitchTooSmall,
	/// An alpha mode the format does not admit - a type error rather than an unsupported case.
	AlphaModeNotAdmitted,
	/// A colour operation asked of an image that is not colour.
	NotColour,
	/// A colour space whose primaries do not describe a colour space.
	UnknownColorSpace,
	/// A buffer shorter than the image's minimum visible bytes.
	BufferTooShort,
	/// An offset or a size that does not fit its type. A width times a height times a byte count is
	/// exactly the product a crafted layout overflows.
	Overflow,
	/// Storage this image needed and could not have. A refusal and never an exit.
	Allocation,
}

#[cfg(test)]
mod tests;
