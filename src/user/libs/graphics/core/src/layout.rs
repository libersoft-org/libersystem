//! `ImageLayout`: the canonical single-plane image model, and the THREE byte spans.
//!
//! CONFUSING THE SPANS IS HOW A BUFFER IS ACCEPTED THAT CANNOT HOLD THE IMAGE - or how one that is
//! exactly big enough is refused. They are three different questions and an implementation that
//! stores one number answers all three with it and is wrong about two.
//!
//! EVERY OFFSET AND SIZE USES CHECKED ARITHMETIC. A width times a height times a byte count is
//! exactly the product that overflows on a crafted layout, and the overflow is what turns a length
//! check into a length check that passes.

use crate::Error;
use crate::format::PixelStorage;
use crate::geom::Extent2D;
use crate::semantics::ImageSemantics;

/// Which edge row zero is at.
///
/// PRESENTABLE SURFACES ARE `TopLeft` IN VERSION ONE and the field exists for images that are not
/// presentable - a texture handed over by something that counts from the bottom. Making it a property
/// of the layout rather than a global is what stops one flipped image flipping everything.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RowOrigin {
	#[default]
	TopLeft,
	BottomLeft,
}

/// A single-plane image's layout. No sample count: multisample storage is `render3d`'s, and saying a
/// 3D attachment is one of these would make a backend's own duty to validate a sample count
/// unsatisfiable against a type that cannot express one.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ImageLayout {
	pub extent: Extent2D,
	pub pitch: u32,
	pub storage: PixelStorage,
	pub origin: RowOrigin,
	pub semantics: ImageSemantics,
}

impl ImageLayout {
	/// Build a layout, CHECKING what a consumer would otherwise have to trust.
	///
	/// WHAT IS CHECKED: a known storage whose packed channels do not overlap, a non-zero extent, a
	/// pitch at least the minimum row, an alpha mode the format admits, and a colour space only on
	/// colour. Each of those is a way an image has actually been wrong.
	pub fn new(extent: Extent2D, pitch: u32, storage: PixelStorage, origin: RowOrigin, semantics: ImageSemantics) -> Result<Self, Error> {
		storage.validate()?;
		// A ZERO EXTENT IS A REFUSAL AND NOT AN EMPTY IMAGE: every consumer that divides by an extent
		// would have to check, and one of them will not.
		if extent.is_empty() {
			return Err(Error::ZeroExtent);
		}
		let minimum = storage.minimum_row_bytes(extent.width).ok_or(Error::Overflow)?;
		if pitch < minimum {
			return Err(Error::PitchTooSmall);
		}
		// THE ALPHA MODE MUST BE ONE THE FORMAT ADMITS. `Straight` on a format with no alpha channel
		// is a combination that cannot mean anything, and refusing it here keeps it out of every
		// backend's match arms.
		if let (PixelStorage::Known(format), Some(alpha)) = (storage, semantics.alpha_mode())
			&& !format.admits(alpha)
		{
			return Err(Error::AlphaModeNotAdmitted);
		}
		Ok(Self { extent, pitch, storage, origin, semantics })
	}

	/// `width * bytes_per_pixel`, checked.
	pub fn minimum_row_bytes(&self) -> Option<u32> {
		self.storage.minimum_row_bytes(self.extent.width)
	}

	/// WHAT A BORROWED CPU VIEW NEEDS: `(height - 1) * pitch + minimum_row_bytes`.
	///
	/// The last row's PADDING is not part of it, so a legal final row with no padding after it is
	/// accepted - and a validator demanding `pitch * height` refuses buffers that are exactly big
	/// enough for the image they hold.
	pub fn minimum_visible_bytes(&self) -> Option<u64> {
		let rows = (self.extent.height as u64).checked_sub(1)?;
		let row = self.minimum_row_bytes()? as u64;
		rows.checked_mul(self.pitch as u64)?.checked_add(row)
	}

	/// WHAT THE SELECTED BACKEND MAY TOUCH.
	///
	/// A DISPLAY OR DMA BACKEND MAY READ THE FINAL ROW'S PITCH PADDING - a scanout engine fetching
	/// whole rows does - and a driver may never touch beyond this. `reads_final_padding` is the
	/// backend's own answer rather than an assumption, because assuming it always does would make
	/// every presentable image own bytes it does not need, and assuming it never does is the
	/// out-of-bounds read.
	pub fn backend_access_span(&self, reads_final_padding: bool) -> Option<u64> {
		if reads_final_padding { (self.extent.height as u64).checked_mul(self.pitch as u64) } else { self.minimum_visible_bytes() }
	}

	/// The pixel count, checked.
	pub fn pixels(&self) -> Option<u64> {
		self.extent.pixels()
	}
}
