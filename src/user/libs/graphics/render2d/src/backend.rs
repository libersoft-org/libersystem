//! THE BACKEND INTERFACE, in TWO phases.
//!
//! "NO STEADY-STATE ALLOCATION" AND A PROFILE CONTAINING LAYERS, CLIP MASKS, BLUR, BACKDROP FILTERS,
//! PATH FLATTENING, GLYPH CACHES AND IMAGE PYRAMIDS CANNOT BOTH BE TRUE without saying WHEN the
//! scratch is worked out. `prepare` works it out; `render` replays. A frame that repeats with the
//! same list and the same target then genuinely allocates nothing, and a frame that cannot fit says
//! so BEFORE it starts drawing rather than failing halfway through a filter chain - which leaves a
//! half-drawn frame on the screen.

use graphics_core::pixel::OutputLuminance;
use graphics_core::{ColorSpace, Extent2D, ImageViewMut, PixelFormat};

use crate::Error;
use crate::list::DrawList;
use crate::prepared::PreparedKey;

/// What a backend is preparing for.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TargetDescription {
	pub extent: Extent2D,
	pub format: PixelFormat,
	pub color_space: ColorSpace,
	/// Physical pixels per logical pixel. Curves are flattened in DEVICE space, so this is part of
	/// what a prepared list is bound to.
	pub scale: f32,
	/// WHAT THE DESTINATION CAN ACTUALLY SHOW, which the tone map needs and a colour space name does
	/// not say. A target that reports nothing gets the profile's stated reference white point.
	///
	/// IT IS NOT PART OF THE PREPARED KEY, deliberately: the tone curve is applied when a tile is
	/// ENCODED into the target, and preparation flattens geometry and builds shaders. A display that
	/// changes what it can show changes the pixels, not the preparation.
	pub luminance: OutputLuminance,
}

/// What a backend hands back from `prepare`.
///
/// ITS CONTENTS ARE THE BACKEND'S OWN and this trait says nothing about them. What it does say is
/// that the thing carries the KEY it was prepared against, because that is what makes
/// `is_compatible` answerable by anybody rather than only by the backend that built it.
pub trait Prepared {
	fn key(&self) -> &PreparedKey;

	/// The scratch this preparation reserved, which a caller compares against a budget.
	fn scratch_bytes(&self) -> u64;
}

/// A 2D backend.
pub trait Backend {
	type Prepared: Prepared;

	/// A name and a version. A PREPARED LIST IS BOUND TO THEM, because the prepared form is this
	/// backend's own - another backend's bytes are not a prepared list, they are bytes.
	fn identity(&self) -> (&'static str, u32);

	/// Validate, flatten, bin, resolve bounds, reserve scratch - and either answer with the
	/// preparation or refuse with the reason.
	fn prepare(&mut self, list: &DrawList, target: &TargetDescription) -> Result<Self::Prepared, Error>;

	/// Replay. ALLOCATES NOTHING.
	fn render(&mut self, prepared: &Self::Prepared, target: &mut ImageViewMut<'_>) -> Result<(), Error>;
}
