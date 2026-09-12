//! TYPED HANDLES INTO A PROCESS-LOCAL TABLE, which is what makes a list cacheable and serialisable
//! later without a redesign.
//!
//! A HANDLE IS NOT A RUST REFERENCE, and that is the whole point. A list holding references is a list
//! that cannot outlive what it points at, cannot be hashed into a cache key, and cannot be encoded
//! into bytes - so the transportable form later becomes an extension of this schema rather than a
//! replacement for it.
//!
//! AND EVERY HANDLE IS TYPED. A single integer index shared by paths, images, fonts and filters is an
//! index a validator cannot check: index seven is a valid path and a valid image, and the drawing that
//! confused them still replays.

/// One kind of thing a list can reference.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum ResourceKind {
	Path,
	Image,
	/// The stop list of a gradient.
	Stops,
	/// The on-off lengths of a dash pattern. A SEPARATE KIND FROM A GRADIENT'S STOPS, because a dash
	/// length is a distance and a stop is a position with a colour: sharing one table would make
	/// index seven a valid dash pattern and a valid gradient, which is the confusion typed handles
	/// exist to prevent.
	Dashes,
	GlyphRun,
	Filter,
}

impl ResourceKind {
	pub const fn name(self) -> &'static str {
		match self {
			ResourceKind::Path => "path",
			ResourceKind::Image => "image",
			ResourceKind::Stops => "stops",
			ResourceKind::Dashes => "dashes",
			ResourceKind::GlyphRun => "glyph-run",
			ResourceKind::Filter => "filter",
		}
	}
}

macro_rules! handle {
	($name:ident, $kind:expr, $what:literal) => {
		#[doc = $what]
		#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
		pub struct $name(pub u32);

		impl $name {
			pub const KIND: ResourceKind = $kind;

			pub const fn index(self) -> u32 {
				self.0
			}
		}
	};
}

handle!(PathHandle, ResourceKind::Path, "A path in the list's own table.");
handle!(ImageHandle, ResourceKind::Image, "An image in the list's own table.");
handle!(PaintHandle, ResourceKind::Stops, "A gradient's stop list in the list's own table.");
handle!(DashHandle, ResourceKind::Dashes, "A dash pattern in the list's own table.");
handle!(GlyphRunHandle, ResourceKind::GlyphRun, "A glyph run in the list's own table.");
handle!(FilterHandle, ResourceKind::Filter, "A filter graph in the list's own table.");
