//! `OwnedImage`: an image an application can create and draw into without a surface.
//!
//! WITHOUT IT EVERY ONE OF THESE NEEDS A SPECIAL CASE: component caches, thumbnails, chart and map
//! tiles, an image editor, shadow maps, render-to-texture, a 3D frame a 2D pass composites, a 2D
//! texture a 3D pass samples, screenshots, and every host test of every drawing routine. An image
//! model that only exists attached to a display is a model most drawing cannot use.
//!
//! ITS STORAGE IS ZEROED, AND THAT IS A SECURITY PROPERTY RATHER THAN A COURTESY. A buffer recycled
//! between Domains holds whatever the last one put in it; an image whose bytes were never written
//! shows that, and the bug reads as a flicker rather than as a disclosure - so it is not reported.
//!
//! IT IS SINGLE-SAMPLE AND SINGLE-PLANE, with any valid semantics. Multisample storage, depth and
//! stencil attachments, cube and array and 3D textures and mip chains are `render3d`'s resources, and
//! saying a 3D attachment is one of these would make a backend's own duty to validate a SAMPLE COUNT
//! unsatisfiable against a type that cannot express one.

use alloc::vec::Vec;

use crate::Error;
use crate::layout::ImageLayout;
use crate::semantics::ImageSemantics;
use crate::view::{ImageView, ImageViewMut};

/// An image and the bytes it owns.
pub struct OwnedImage {
	layout: ImageLayout,
	bytes: Vec<u8>,
}

impl OwnedImage {
	/// Allocate an image, ZEROED.
	///
	/// THE ALLOCATION IS FALLIBLE. Infallible allocation in userspace ends the process when it fails,
	/// which would take a caller's whole program down over one thumbnail - and an image is exactly the
	/// allocation an application is most likely to ask for too much of.
	pub fn new(layout: ImageLayout) -> Result<Self, Error> {
		// THE ALLOCATION IS THE BACKEND SPAN AND NOT THE VISIBLE ONE. An image that may be presented
		// or read by DMA must OWN the final row's padding, because a scanout engine fetching whole
		// rows reads it - and an allocation one row-padding short is an out-of-bounds read by
		// hardware, which no bounds check in this process can catch.
		let length = layout.backend_access_span(true).ok_or(Error::Overflow)?;
		let length = usize::try_from(length).map_err(|_| Error::Overflow)?;
		let mut bytes = Vec::new();
		bytes.try_reserve_exact(length).map_err(|_| Error::Allocation)?;
		bytes.resize(length, 0);
		Ok(Self { layout, bytes })
	}

	/// Allocate a COLOUR image, refusing any other semantics.
	///
	/// A COLOUR-ONLY ENTRY POINT VALIDATES ITS SEMANTICS rather than documenting them: a caller that
	/// passes a mask to something that will sRGB-decode it has made the exact mistake the semantics
	/// exist to prevent.
	pub fn new_color(layout: ImageLayout) -> Result<Self, Error> {
		if !layout.semantics.is_colour() {
			return Err(Error::NotColour);
		}
		Self::new(layout)
	}

	pub fn layout(&self) -> &ImageLayout {
		&self.layout
	}

	pub fn semantics(&self) -> ImageSemantics {
		self.layout.semantics
	}

	/// How many bytes this image actually owns.
	pub fn allocation_len(&self) -> usize {
		self.bytes.len()
	}

	/// The bytes, for a consumer that knows this image's layout and addresses it itself.
	///
	/// A VIEW IS CHECKED WHEN IT IS BUILT, which is right once per image and ruinous once per pixel: a
	/// renderer's own scratch surface knows its layout is the one it allocated, and building a
	/// validated view to read one texel is forty nanoseconds of checking the same four fields again.
	pub fn bytes(&self) -> &[u8] {
		&self.bytes
	}

	/// The bytes, mutably. Same contract: the caller owns the arithmetic.
	pub fn bytes_mut(&mut self) -> &mut [u8] {
		&mut self.bytes
	}

	pub fn view(&self) -> ImageView<'_> {
		// The constructor cannot fail here: the allocation is at least the span it checks. It is
		// still called rather than bypassed, because a constructor that is only correct when nobody
		// bypasses it is a constructor somebody bypasses.
		ImageView::new(self.layout, &self.bytes).unwrap_or_else(|_| unreachable_view())
	}

	pub fn view_mut(&mut self) -> ImageViewMut<'_> {
		let layout = self.layout;
		ImageViewMut::new(layout, &mut self.bytes).unwrap_or_else(|_| unreachable_view())
	}

	/// Hand the bytes back, for a caller exporting the image.
	///
	/// PADDING IS ZEROED ON EXPORT, which is what makes two exports of one image the same bytes - and
	/// therefore what makes an image hashable and a golden comparison meaningful. It is zeroed HERE
	/// rather than trusted to have stayed zero, because a drawing routine that wrote a whole pitch is
	/// not a bug and its padding is not content.
	pub fn export(&mut self) -> &[u8] {
		let Some(row) = self.layout.minimum_row_bytes() else { return &self.bytes };
		let pitch = self.layout.pitch as usize;
		let row = row as usize;
		if pitch > row {
			let length = self.bytes.len();
			for y in 0..self.layout.extent.height as usize {
				let start = y * pitch + row;
				let end = ((y + 1) * pitch).min(length);
				if let Some(padding) = self.bytes.get_mut(start..end) {
					padding.fill(0);
				}
			}
		}
		&self.bytes
	}
}

/// The two views above cannot fail; this says so in one place rather than panicking in two.
fn unreachable_view<T>() -> T {
	// An owned image whose own allocation does not satisfy its own layout is a defect in this file,
	// and there is no value to return - so this is the one place the crate cannot continue from.
	panic!("an owned image's allocation must satisfy its own layout")
}
