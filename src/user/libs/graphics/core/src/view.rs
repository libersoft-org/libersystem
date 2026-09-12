//! `ImageView` and `ImageViewMut`: a borrowed slice plus a layout, with a constructor that checks.
//!
//! THE CONSTRUCTOR IS THE POINT. What this replaces is an application computing a length and building
//! an aliasing mutable slice out of a raw pointer - `from_raw_parts_mut(surface.addr() as *mut u8,
//! target_len)` - which is unsound whenever the length is wrong and is exactly as easy to write when
//! it is. A view that cannot be constructed from a short buffer cannot be used over one.

use crate::Error;
use crate::layout::ImageLayout;

/// A borrowed, read-only image.
#[derive(Clone, Copy)]
pub struct ImageView<'a> {
	layout: ImageLayout,
	bytes: &'a [u8],
}

/// A borrowed, writable image.
pub struct ImageViewMut<'a> {
	layout: ImageLayout,
	bytes: &'a mut [u8],
}

impl<'a> ImageView<'a> {
	/// Borrow a slice as an image, or refuse.
	pub fn new(layout: ImageLayout, bytes: &'a [u8]) -> Result<Self, Error> {
		check(&layout, bytes.len())?;
		Ok(Self { layout, bytes })
	}

	pub fn layout(&self) -> &ImageLayout {
		&self.layout
	}

	pub fn bytes(&self) -> &'a [u8] {
		self.bytes
	}

	/// One row's VISIBLE bytes - the pixels, without the padding after them.
	///
	/// PADDING IS NOT PIXEL CONTENT, so a row accessor that returned the pitch would hand a caller
	/// bytes that mean nothing and that a readback must not return.
	pub fn row(&self, y: u32) -> Option<&'a [u8]> {
		let start = row_start(&self.layout, y)?;
		let length = self.layout.minimum_row_bytes()? as usize;
		self.bytes.get(start..start.checked_add(length)?)
	}
}

impl<'a> ImageViewMut<'a> {
	pub fn new(layout: ImageLayout, bytes: &'a mut [u8]) -> Result<Self, Error> {
		check(&layout, bytes.len())?;
		Ok(Self { layout, bytes })
	}

	pub fn layout(&self) -> &ImageLayout {
		&self.layout
	}

	pub fn row_mut(&mut self, y: u32) -> Option<&mut [u8]> {
		let start = row_start(&self.layout, y)?;
		let length = self.layout.minimum_row_bytes()? as usize;
		self.bytes.get_mut(start..start.checked_add(length)?)
	}

	/// Borrow the same image read-only, which is what a caller hands to something that only reads.
	pub fn as_view(&self) -> ImageView<'_> {
		ImageView { layout: self.layout, bytes: self.bytes }
	}

	pub fn bytes_mut(&mut self) -> &mut [u8] {
		self.bytes
	}
}

/// Where a row starts, taking the ORIGIN into account - which is the one place a bottom-left image
/// differs from a top-left one, so it is the one place that has to remember.
fn row_start(layout: &ImageLayout, y: u32) -> Option<usize> {
	if y >= layout.extent.height {
		return None;
	}
	let row = match layout.origin {
		crate::layout::RowOrigin::TopLeft => y,
		crate::layout::RowOrigin::BottomLeft => layout.extent.height.checked_sub(1)?.checked_sub(y)?,
	};
	usize::try_from((row as u64).checked_mul(layout.pitch as u64)?).ok()
}

/// The one check both constructors make.
fn check(layout: &ImageLayout, length: usize) -> Result<(), Error> {
	let needed = layout.minimum_visible_bytes().ok_or(Error::Overflow)?;
	if (length as u64) < needed {
		return Err(Error::BufferTooShort);
	}
	Ok(())
}
