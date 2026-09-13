//! THE SURFACES THIS BACKEND WORKS ON, and where the pixels of a referenced image come from.
//!
//! EVERY INTERMEDIATE IS THE CANONICAL PREMULTIPLIED LINEAR `R16G16B16A16_FLOAT`. Layers, clip
//! masks' colour when one is needed, filter intermediates and the working copy of a tile are all that
//! one format, which is what makes a layer composited by one path and a filter applied by another
//! agree about what a colour is. A backend that used the target's own format for its intermediates
//! would quantise twice, band its gradients, and clip every value a filter briefly takes above one.
//!
//! AND THE TILE IS DECODED ONCE AND ENCODED ONCE. Compositing straight into the target would decode
//! and re-encode the destination for every command that touched it, which is the cost that makes a
//! correct pipeline look expensive - the conversion belongs at the edges of a tile and not inside its
//! command loop.

use alloc::vec::Vec;

use graphics_core::geom::{Extent2D, PixelRect};
use graphics_core::layout::{ImageLayout, RowOrigin};
use graphics_core::pixel::{Decoder, Encoder, OutputLuminance, Rgba, TransferTable, Working, read_row, write_row};
use graphics_core::semantics::ImageSemantics;
use graphics_core::view::{ImageView, ImageViewMut};
use graphics_core::{AlphaMode, ColorSpace, Error as CoreError, OwnedImage, PixelFormat, PixelStorage};
use render2d::Error;

/// Where a list's referenced images actually are.
///
/// A `DrawList` CARRIES AN IDENTITY AND NOT A POINTER, which is what makes it cacheable and hashable.
/// The pixels are supplied here, by whoever owns them, at the moment they are needed - so a list that
/// outlives an image is a lookup that answers `None` rather than a dangling reference.
pub trait ImageSource {
	fn image(&self, identity: u64) -> Option<ImageView<'_>>;

	/// The same image as PLANES, for a decoder or a camera that hands over `NV12`, `I420` or `P010`.
	///
	/// A DEFAULT OF `None` AND NOT A SECOND TRAIT. Almost every source is single-plane, and a source
	/// that has planes answers here instead of at `image` - which is what lets one lookup serve both
	/// and keeps the video path out of every consumer that has no video.
	fn planes(&self, identity: u64) -> Option<graphics_core::planar::MultiPlaneView<'_>> {
		let _ = identity;
		None
	}
}

/// The source for a drawing that references no images.
pub struct NoImages;

impl ImageSource for NoImages {
	fn image(&self, _identity: u64) -> Option<ImageView<'_>> {
		None
	}
}

/// A rectangle of the canonical intermediate, with its place in the target.
pub struct Surface {
	image: OwnedImage,
	origin: (u32, u32),
}

impl Surface {
	/// Allocate. CALLED IN `prepare` AND NEVER DURING A DRAW.
	pub fn new(bounds: PixelRect, space: ColorSpace) -> Result<Self, Error> {
		let extent = Extent2D::new(bounds.width.max(1), bounds.height.max(1));
		let layout = canonical_layout(extent, space).map_err(from_core)?;
		let image = OwnedImage::new(layout).map_err(from_core)?;
		Ok(Self { image, origin: (bounds.x, bounds.y) })
	}

	pub fn bounds(&self) -> PixelRect {
		let extent = self.image.layout().extent;
		PixelRect::new(self.origin.0, self.origin.1, extent.width, extent.height)
	}

	/// Move this surface to another place in the target, KEEPING its storage.
	///
	/// A TILE LOOP THAT ALLOCATED A SURFACE PER TILE WOULD ALLOCATE PER FRAME, which is the rule the
	/// two phases exist to keep: the scratch is reserved once and re-aimed.
	pub fn rebase(&mut self, origin: (u32, u32)) {
		self.origin = origin;
	}

	pub fn clear(&mut self) {
		self.image.view_mut().bytes_mut().fill(0);
	}

	/// One pixel, in the TARGET's coordinates.
	///
	/// THE BYTES ARE ADDRESSED DIRECTLY AND NOT THROUGH A VIEW. A view is CHECKED when it is built -
	/// the storage, the pitch, the alpha mode and the span - which is exactly right once per image and
	/// is forty nanoseconds per PIXEL when a get builds one. This surface owns its storage, knows its
	/// layout is the canonical one, and computes the offset itself.
	pub fn get(&self, x: u32, y: u32) -> Rgba {
		let Some(offset) = self.offset(x, y) else { return Rgba::TRANSPARENT };
		let bytes = self.image.bytes();
		let Some(pixel) = bytes.get(offset..offset + BYTES_PER_PIXEL) else { return Rgba::TRANSPARENT };
		decode_half(pixel)
	}

	pub fn set(&mut self, x: u32, y: u32, value: Rgba) {
		let Some(offset) = self.offset(x, y) else { return };
		let bytes = self.image.bytes_mut();
		let Some(pixel) = bytes.get_mut(offset..offset + BYTES_PER_PIXEL) else { return };
		encode_half(pixel, value);
	}

	/// Read a horizontal run of pixels into a buffer.
	///
	/// A RUN AND NOT A PIXEL AT A TIME, because the loop that composites a span is the hot loop of the
	/// whole backend: fetching one pixel through its own offset computation per sample is most of the
	/// cost of a fill, and it is the shape that stops the arithmetic being vectorised.
	pub fn read_span(&self, x: u32, y: u32, out: &mut [Rgba]) {
		let Some(offset) = self.offset(x, y) else {
			out.fill(Rgba::TRANSPARENT);
			return;
		};
		let bytes = self.image.bytes();
		for (index, slot) in out.iter_mut().enumerate() {
			let start = offset + index * BYTES_PER_PIXEL;
			*slot = match bytes.get(start..start + BYTES_PER_PIXEL) {
				Some(pixel) => decode_half(pixel),
				None => Rgba::TRANSPARENT,
			};
		}
	}

	/// Write a horizontal run of pixels.
	pub fn write_span(&mut self, x: u32, y: u32, values: &[Rgba]) {
		let Some(offset) = self.offset(x, y) else { return };
		let bytes = self.image.bytes_mut();
		for (index, value) in values.iter().enumerate() {
			let start = offset + index * BYTES_PER_PIXEL;
			if let Some(pixel) = bytes.get_mut(start..start + BYTES_PER_PIXEL) {
				encode_half(pixel, *value);
			}
		}
	}

	/// The byte offset of a pixel, or `None` when it is outside this surface.
	fn offset(&self, x: u32, y: u32) -> Option<usize> {
		let (local_x, local_y) = self.local(x, y)?;
		let pitch = self.image.layout().pitch as usize;
		Some(local_y as usize * pitch + local_x as usize * BYTES_PER_PIXEL)
	}

	pub fn view(&self) -> ImageView<'_> {
		self.image.view()
	}

	fn local(&self, x: u32, y: u32) -> Option<(u32, u32)> {
		let extent = self.image.layout().extent;
		let local_x = x.checked_sub(self.origin.0)?;
		let local_y = y.checked_sub(self.origin.1)?;
		(local_x < extent.width && local_y < extent.height).then_some((local_x, local_y))
	}

	/// Decode a rectangle of a target image into this surface.
	///
	/// ROW BY ROW, THROUGH A SCRATCH RUN. The target's row is found once, its pixels are read in one
	/// pass and decoded in another, and the result is written into this surface in a third - three
	/// tight loops instead of one loop doing three lookups per pixel.
	pub fn load(&mut self, target: &ImageViewMut<'_>, bounds: PixelRect, working: Working, table: Option<&TransferTable>, scratch: &mut [Rgba]) -> Result<(), Error> {
		let source = target.as_view();
		let decoder = Decoder::new(&source.layout().semantics, working).map_err(from_core)?;
		let width = (bounds.width as usize).min(scratch.len());
		if width == 0 {
			return Ok(());
		}
		for y in bounds.y..bounds.y.saturating_add(bounds.height) {
			read_row(&source, bounds.x, y, &mut scratch[..width]);
			for value in scratch[..width].iter_mut() {
				*value = match table {
					Some(table) => decoder.decode_tabled(table, *value),
					None => decoder.decode(*value),
				};
			}
			self.write_span(bounds.x, y, &scratch[..width]);
		}
		Ok(())
	}

	/// Encode this surface back into a rectangle of a target image.
	///
	/// THE DITHER PHASE IS THE TARGET'S x AND y, which is why they are passed through rather than the
	/// surface's own: a tile-relative phase makes the ordered pattern restart at every tile boundary,
	/// and that is the artefact that looks like a seam.
	pub fn store(&self, target: &mut ImageViewMut<'_>, bounds: PixelRect, working: Working, output: OutputLuminance, table: Option<&TransferTable>, scratch: &mut [Rgba]) -> Result<(), Error> {
		let (semantics, storage) = (target.layout().semantics, target.layout().storage);
		let encoder = Encoder::new_for_output(&semantics, storage, working, output).map_err(from_core)?;
		let width = (bounds.width as usize).min(scratch.len());
		if width == 0 {
			return Ok(());
		}
		for y in bounds.y..bounds.y.saturating_add(bounds.height) {
			self.read_span(bounds.x, y, &mut scratch[..width]);
			for (index, value) in scratch[..width].iter_mut().enumerate() {
				let x = bounds.x + index as u32;
				*value = match table {
					Some(table) => encoder.encode_tabled(table, *value, x, y),
					None => encoder.encode(*value, x, y),
				};
			}
			write_row(target, bounds.x, y, &scratch[..width]);
		}
		Ok(())
	}

	pub fn scratch_bytes(&self) -> u64 {
		self.image.allocation_len() as u64
	}
}

/// The canonical intermediate is four halves.
const BYTES_PER_PIXEL: usize = 8;

fn decode_half(pixel: &[u8]) -> Rgba {
	let channel = |index: usize| graphics_core::pixel::half_to_f32(u16::from_le_bytes([pixel[index * 2], pixel[index * 2 + 1]]));
	Rgba::new(channel(0), channel(1), channel(2), channel(3))
}

fn encode_half(pixel: &mut [u8], value: Rgba) {
	for (index, channel) in [value.red, value.green, value.blue, value.alpha].into_iter().enumerate() {
		pixel[index * 2..index * 2 + 2].copy_from_slice(&graphics_core::pixel::f32_to_half(channel).to_le_bytes());
	}
}

/// The layout of every intermediate this backend makes.
pub fn canonical_layout(extent: Extent2D, space: ColorSpace) -> Result<ImageLayout, CoreError> {
	let storage = PixelStorage::Known(PixelFormat::R16G16B16A16Float);
	let pitch = storage.minimum_row_bytes(extent.width).ok_or(CoreError::Overflow)?;
	let semantics = ImageSemantics::Color { color_space: space.linear_counterpart(), alpha_mode: AlphaMode::Premultiplied };
	ImageLayout::new(extent, pitch, storage, RowOrigin::TopLeft, semantics)
}

/// Translate the image model's refusals into this API's.
///
/// AN ALLOCATION FAILURE IS A RESOURCE DECISION AND A BAD LAYOUT IS A DEFECT, and the two must not
/// arrive at a caller as one word.
pub fn from_core(error: CoreError) -> Error {
	match error {
		CoreError::Allocation => Error::Allocation,
		CoreError::Overflow | CoreError::ZeroExtent | CoreError::PitchTooSmall | CoreError::BufferTooShort => Error::LimitExceeded { limit: "target extent", ceiling: graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS.max_image_extent as u64 },
		_ => Error::UnknownResource { kind: render2d::resource::ResourceKind::Image, index: 0 },
	}
}

/// WHAT A DRAW COMPOSITES INTO: the target's working copy, or a layer.
///
/// ONE TRAIT AND TWO STORAGES, because the two have different contracts. A LAYER is an intermediate
/// and the profile fixes its format - premultiplied linear `R16G16B16A16_FLOAT` - so that a layer
/// composited by one path and a filter applied by another agree about what a colour is. THE TARGET'S
/// WORKING COPY IS NOT AN INTERMEDIATE: it is this backend's own scratch for one tile, it never
/// leaves the backend, and holding it as halves costs eight conversions per pixel per composite -
/// which on a full-screen fill is most of what the fill costs.
pub trait Raster {
	fn get(&self, x: u32, y: u32) -> Rgba;
	fn set(&mut self, x: u32, y: u32, value: Rgba);
	fn read_span(&self, x: u32, y: u32, out: &mut [Rgba]);
	fn write_span(&mut self, x: u32, y: u32, values: &[Rgba]);
}

/// The target's working copy of one tile, in `f32`.
pub struct Tile {
	pixels: Vec<Rgba>,
	origin: (u32, u32),
	width: u32,
	height: u32,
}

impl Tile {
	pub fn new(size: u32) -> Self {
		Self { pixels: alloc::vec![Rgba::TRANSPARENT; (size as usize).saturating_mul(size as usize)], origin: (0, 0), width: size, height: size }
	}

	pub fn rebase(&mut self, origin: (u32, u32)) {
		self.origin = origin;
	}

	pub fn clear(&mut self) {
		self.pixels.fill(Rgba::TRANSPARENT);
	}

	pub fn scratch_bytes(&self) -> u64 {
		(self.pixels.capacity() * core::mem::size_of::<Rgba>()) as u64
	}

	fn index(&self, x: u32, y: u32) -> Option<usize> {
		let local_x = x.checked_sub(self.origin.0)?;
		let local_y = y.checked_sub(self.origin.1)?;
		(local_x < self.width && local_y < self.height).then(|| local_y as usize * self.width as usize + local_x as usize)
	}

	/// Decode a rectangle of a target image into this tile.
	pub fn load(&mut self, target: &ImageViewMut<'_>, bounds: PixelRect, working: Working, table: Option<&TransferTable>) -> Result<(), Error> {
		let source = target.as_view();
		let decoder = Decoder::new(&source.layout().semantics, working).map_err(from_core)?;
		let width = bounds.width as usize;
		for y in bounds.y..bounds.y.saturating_add(bounds.height) {
			let Some(start) = self.index(bounds.x, y) else { continue };
			let Some(row) = self.pixels.get_mut(start..start + width) else { continue };
			read_row(&source, bounds.x, y, row);
			for value in row.iter_mut() {
				*value = match table {
					Some(table) => decoder.decode_tabled(table, *value),
					None => decoder.decode(*value),
				};
			}
		}
		Ok(())
	}

	/// Encode this tile back into a rectangle of a target image.
	///
	/// THE DITHER PHASE IS THE TARGET'S x AND y, which is why they are passed through rather than the
	/// tile's own: a tile-relative phase makes the ordered pattern restart at every tile boundary, and
	/// that is the artefact that looks like a seam.
	pub fn store(&self, target: &mut ImageViewMut<'_>, bounds: PixelRect, working: Working, output: OutputLuminance, table: Option<&TransferTable>, scratch: &mut [Rgba]) -> Result<(), Error> {
		let (semantics, storage) = (target.layout().semantics, target.layout().storage);
		let encoder = Encoder::new_for_output(&semantics, storage, working, output).map_err(from_core)?;
		let width = (bounds.width as usize).min(scratch.len());
		for y in bounds.y..bounds.y.saturating_add(bounds.height) {
			let Some(start) = self.index(bounds.x, y) else { continue };
			let Some(row) = self.pixels.get(start..start + width) else { continue };
			for (index, value) in row.iter().enumerate() {
				scratch[index] = match table {
					Some(table) => encoder.encode_tabled(table, *value, bounds.x + index as u32, y),
					None => encoder.encode(*value, bounds.x + index as u32, y),
				};
			}
			write_row(target, bounds.x, y, &scratch[..width]);
		}
		Ok(())
	}
}

impl Raster for Tile {
	fn get(&self, x: u32, y: u32) -> Rgba {
		self.index(x, y).and_then(|index| self.pixels.get(index).copied()).unwrap_or(Rgba::TRANSPARENT)
	}

	fn set(&mut self, x: u32, y: u32, value: Rgba) {
		if let Some(index) = self.index(x, y)
			&& let Some(slot) = self.pixels.get_mut(index)
		{
			*slot = value;
		}
	}

	fn read_span(&self, x: u32, y: u32, out: &mut [Rgba]) {
		match self.index(x, y).and_then(|start| self.pixels.get(start..start + out.len())) {
			Some(row) => out.copy_from_slice(row),
			None => out.fill(Rgba::TRANSPARENT),
		}
	}

	fn write_span(&mut self, x: u32, y: u32, values: &[Rgba]) {
		if let Some(start) = self.index(x, y)
			&& let Some(row) = self.pixels.get_mut(start..start + values.len())
		{
			row.copy_from_slice(values);
		}
	}
}

impl Raster for Surface {
	fn get(&self, x: u32, y: u32) -> Rgba {
		Surface::get(self, x, y)
	}

	fn set(&mut self, x: u32, y: u32, value: Rgba) {
		Surface::set(self, x, y, value)
	}

	fn read_span(&self, x: u32, y: u32, out: &mut [Rgba]) {
		Surface::read_span(self, x, y, out)
	}

	fn write_span(&mut self, x: u32, y: u32, values: &[Rgba]) {
		Surface::write_span(self, x, y, values)
	}
}
