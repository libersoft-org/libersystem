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

use crate::workers::Access;

/// Where a list's referenced images actually are.
///
/// A `DrawList` CARRIES AN IDENTITY AND NOT A POINTER, which is what makes it cacheable and hashable.
/// The pixels are supplied here, by whoever owns them, at the moment they are needed - so a list that
/// outlives an image is a lookup that answers `None` rather than a dangling reference.
///
/// `Sync`, BECAUSE A LANE READS IT. A frame replayed on several workers consults the source from each of
/// them at once, and every source in the tree is plain data already - a supertrait says so where the
/// compiler can check it, rather than in a comment it cannot.
pub trait ImageSource: Sync {
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

	/// CLEAR WHAT `bounds` DOES NOT COVER - the columns right of it and the rows below it, to the
	/// surface's extent - for a writer that is about to set every pixel of `bounds` itself. Clearing
	/// the whole scratch before a node that overwrites it was a third of what a filter graph's
	/// trivial nodes cost; what is left beyond `bounds` must still read transparent, because a
	/// bilinear tap or a convolution reaches one pixel past the edge.
	pub fn clear_outside(&mut self, bounds: PixelRect) {
		let extent = self.image.layout().extent;
		let pitch = self.image.layout().pitch as usize;
		let row_bytes = extent.width as usize * BYTES_PER_PIXEL;
		// THE KEPT RECTANGLE IN THIS SURFACE'S OWN PIXELS, clamped to it: the columns `left..right` of the
		// rows `top..bottom`.
		let left = bounds.x.saturating_sub(self.origin.0).min(extent.width);
		let right = bounds.x.saturating_add(bounds.width).saturating_sub(self.origin.0).min(extent.width).max(left);
		let top = bounds.y.saturating_sub(self.origin.1).min(extent.height);
		let bottom = bounds.y.saturating_add(bounds.height).saturating_sub(self.origin.1).min(extent.height).max(top);
		let bytes = self.image.bytes_mut();
		for row in 0..extent.height {
			let start = row as usize * pitch;
			let Some(line) = bytes.get_mut(start..start + row_bytes) else { break };
			if row < top || row >= bottom {
				line.fill(0);
			} else {
				line[..left as usize * BYTES_PER_PIXEL].fill(0);
				line[right as usize * BYTES_PER_PIXEL..].fill(0);
			}
		}
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
	/// ONE BOUNDS CHECK FOR THE RUN AND NOT ONE PER PIXEL, which is the difference between a loop
	/// the compiler can widen and a loop it cannot.
	///
	/// This asked `bytes.get(start..start + BYTES_PER_PIXEL)` for every pixel and matched on the
	/// `Option`. That is a branch and a panic path inside the hot loop of the whole backend, it
	/// recomputes the offset per pixel, and it stops the four channel loads being anything but four
	/// scalar loads. Taking the whole run once and walking it with `chunks_exact` gives the compiler
	/// a fixed-size window with no failure case in it - MEASURED, on the probe that isolates this:
	/// one opaque full-screen fill.
	pub fn read_span(&self, x: u32, y: u32, out: &mut [Rgba]) {
		let Some(offset) = self.offset(x, y) else {
			out.fill(Rgba::TRANSPARENT);
			return;
		};
		let bytes = self.image.bytes();
		let Some(run) = bytes.get(offset..offset + out.len() * BYTES_PER_PIXEL) else {
			out.fill(Rgba::TRANSPARENT);
			return;
		};
		for (slot, pixel) in out.iter_mut().zip(run.chunks_exact(BYTES_PER_PIXEL)) {
			*slot = decode_half(pixel);
		}
	}

	/// Read a VERTICAL run of pixels: the same idea as `read_span`, down a column.
	///
	/// WHY A COLUMN NEEDS ITS OWN: a separable blur's second pass walks columns, and it walked them
	/// with `get(x, y)` per pixel - which recomputes the local coordinates, the bounds check and the
	/// offset for every one of them. This surface's own documentation says what that costs: "a view
	/// is CHECKED when it is built ... and is forty nanoseconds per PIXEL when a get builds one".
	/// The first offset is computed once and the rest is the pitch, which is what a column IS.
	pub fn read_column(&self, x: u32, y: u32, out: &mut [Rgba]) {
		let Some(offset) = self.offset(x, y) else {
			out.fill(Rgba::TRANSPARENT);
			return;
		};
		let pitch = self.image.layout().pitch as usize;
		let bytes = self.image.bytes();
		for (index, slot) in out.iter_mut().enumerate() {
			let start = offset + index * pitch;
			*slot = match bytes.get(start..start + BYTES_PER_PIXEL) {
				Some(pixel) => decode_half(pixel),
				None => Rgba::TRANSPARENT,
			};
		}
	}

	/// Write a vertical run of pixels - see `read_column`.
	pub fn write_column(&mut self, x: u32, y: u32, values: &[Rgba]) {
		let Some(offset) = self.offset(x, y) else { return };
		let pitch = self.image.layout().pitch as usize;
		let bytes = self.image.bytes_mut();
		for (index, value) in values.iter().enumerate() {
			let start = offset + index * pitch;
			if let Some(pixel) = bytes.get_mut(start..start + BYTES_PER_PIXEL) {
				encode_half(pixel, *value);
			}
		}
	}

	/// Write a horizontal run of pixels. One bounds check for the run - see `read_span`.
	pub fn write_span(&mut self, x: u32, y: u32, values: &[Rgba]) {
		let Some(offset) = self.offset(x, y) else { return };
		let bytes = self.image.bytes_mut();
		let Some(run) = bytes.get_mut(offset..offset + values.len() * BYTES_PER_PIXEL) else { return };
		for (value, pixel) in values.iter().zip(run.chunks_exact_mut(BYTES_PER_PIXEL)) {
			encode_half(pixel, *value);
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
			decoder.decode_row(table, &mut scratch[..width]);
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
			encoder.encode_row(table, &mut scratch[..width], bounds.x, y);
			write_row(target, bounds.x, y, &scratch[..width]);
		}
		Ok(())
	}

	pub fn scratch_bytes(&self) -> u64 {
		self.image.allocation_len() as u64
	}
}

/// THE CANONICAL INTERMEDIATE IS FOUR SINGLES, and it was four halves (changed 2026-09-15).
///
/// WHY IT CHANGED: this machine has no hardware half conversion in reach of a `no_std` build, so
/// every read and every write of a working pixel went through a branchy software routine - twice per
/// channel, four channels, on the hot loop of the whole backend. Doubling the scratch to remove
/// eight conversions per pixel per access is the trade, and the scratch it doubles is a tile plus a
/// surface per open layer, measured in hundreds of kilobytes against a sixty-four megabyte ceiling.
///
/// AND IT IS MORE ACCURATE, not less. A half carries eleven bits of mantissa and the arithmetic
/// above it is `f32` throughout, so the old intermediate ROUNDED between every pair of composites -
/// which is a loss the conformance suite tolerated rather than wanted.
const BYTES_PER_PIXEL: usize = 16;

fn decode_half(pixel: &[u8]) -> Rgba {
	let channel = |index: usize| f32::from_le_bytes([pixel[index * 4], pixel[index * 4 + 1], pixel[index * 4 + 2], pixel[index * 4 + 3]]);
	Rgba::new(channel(0), channel(1), channel(2), channel(3))
}

fn encode_half(pixel: &mut [u8], value: Rgba) {
	for (index, channel) in [value.red, value.green, value.blue, value.alpha].into_iter().enumerate() {
		pixel[index * 4..index * 4 + 4].copy_from_slice(&channel.to_le_bytes());
	}
}

/// The layout of every intermediate this backend makes.
pub fn canonical_layout(extent: Extent2D, space: ColorSpace) -> Result<ImageLayout, CoreError> {
	let storage = PixelStorage::Known(PixelFormat::R32G32B32A32Float);
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
	/// Decode `bounds` of the target into the tile, through the unit's part of the target.
	pub(crate) fn load(&mut self, access: &Access<'_>, bounds: PixelRect, working: Working, table: Option<&TransferTable>) -> Result<(), Error> {
		match access {
			Access::Band { view, top } => self.load_from(&view.as_view(), *top, bounds, working, table),
			// THE WHOLE TARGET, READ ONLY: nothing writes it while the units run, so every tile reads the
			// pixels the frame started with - which is what the serial walk reads, since no tile writes
			// another's pixels.
			Access::Slot { source, .. } => self.load_from(source, 0, bounds, working, table),
			Access::Rect { rows, tile, layout } => {
				let decoder = Decoder::new(&layout.semantics, working).map_err(from_core)?;
				let part = part_layout(layout, tile)?;
				let width = bounds.width as usize;
				for y in bounds.y..bounds.y.saturating_add(bounds.height) {
					let Some(start) = self.index(bounds.x, y) else { continue };
					let Some(row) = self.pixels.get_mut(start..start + width) else { continue };
					let Some(bytes) = rows.get((y - tile.y) as usize) else { continue };
					read_row(&ImageView::new(part, bytes).map_err(from_core)?, bounds.x - tile.x, 0, row);
					decoder.decode_row(table, row);
				}
				Ok(())
			}
		}
	}

	fn load_from(&mut self, source: &ImageView<'_>, top: u32, bounds: PixelRect, working: Working, table: Option<&TransferTable>) -> Result<(), Error> {
		let decoder = Decoder::new(&source.layout().semantics, working).map_err(from_core)?;
		let width = bounds.width as usize;
		for y in bounds.y..bounds.y.saturating_add(bounds.height) {
			let Some(start) = self.index(bounds.x, y) else { continue };
			let Some(row) = self.pixels.get_mut(start..start + width) else { continue };
			read_row(source, bounds.x, y.saturating_sub(top), row);
			decoder.decode_row(table, row);
		}
		Ok(())
	}

	/// Encode `bounds` of the tile into the unit's part of the target: its band's rows, or its slot.
	///
	/// THE ENCODER IS TOLD THE ABSOLUTE POSITION either way, because its dither is a function of where a
	/// pixel is on the target - a slot's own coordinates would dither every tile alike.
	pub(crate) fn store(&self, access: &mut Access<'_>, bounds: PixelRect, working: Working, output: OutputLuminance, table: Option<&TransferTable>, scratch: &mut [Rgba]) -> Result<(), Error> {
		let layout = match access {
			Access::Band { view, .. } => *view.layout(),
			Access::Slot { source, .. } => *source.layout(),
			Access::Rect { layout, .. } => *layout,
		};
		let encoder = Encoder::new_for_output(&layout.semantics, layout.storage, working, output).map_err(from_core)?;
		let width = (bounds.width as usize).min(scratch.len());
		match access {
			Access::Band { view, top } => {
				for y in bounds.y..bounds.y.saturating_add(bounds.height) {
					let Some(start) = self.index(bounds.x, y) else { continue };
					let Some(row) = self.pixels.get(start..start + width) else { continue };
					scratch[..width].copy_from_slice(row);
					encoder.encode_row(table, &mut scratch[..width], bounds.x, y);
					write_row(view, bounds.x, y.saturating_sub(*top), &scratch[..width]);
				}
			}
			Access::Slot { slot, tile, pitch, .. } => {
				// THE SLOT IS AN IMAGE OF ITS OWN, one tile of the target's format with no padding past
				// its rows, so the one row writer the target uses writes it too.
				let slot_layout = ImageLayout::new(Extent2D::new(tile.width.max(1), tile.height.max(1)), *pitch as u32, layout.storage, RowOrigin::TopLeft, layout.semantics).map_err(from_core)?;
				let mut view = ImageViewMut::new(slot_layout, slot).map_err(from_core)?;
				for y in bounds.y..bounds.y.saturating_add(bounds.height) {
					let Some(start) = self.index(bounds.x, y) else { continue };
					let Some(row) = self.pixels.get(start..start + width) else { continue };
					scratch[..width].copy_from_slice(row);
					encoder.encode_row(table, &mut scratch[..width], bounds.x, y);
					write_row(&mut view, bounds.x - tile.x, y - tile.y, &scratch[..width]);
				}
			}
			Access::Rect { rows, tile, .. } => {
				let part = part_layout(&layout, tile)?;
				for y in bounds.y..bounds.y.saturating_add(bounds.height) {
					let Some(start) = self.index(bounds.x, y) else { continue };
					let Some(row) = self.pixels.get(start..start + width) else { continue };
					let Some(bytes) = rows.get_mut((y - tile.y) as usize) else { continue };
					scratch[..width].copy_from_slice(row);
					encoder.encode_row(table, &mut scratch[..width], bounds.x, y);
					write_row(&mut ImageViewMut::new(part, bytes).map_err(from_core)?, bounds.x - tile.x, 0, &scratch[..width]);
				}
			}
		}
		Ok(())
	}
}

/// ONE ROW PART OF A TILE AS AN IMAGE OF ITS OWN - a single row of the target's format, as wide as the
/// tile - so the row reader and writer the target uses read and write it too.
fn part_layout(layout: &ImageLayout, tile: &PixelRect) -> Result<ImageLayout, Error> {
	let width = tile.width.max(1);
	ImageLayout::new(Extent2D::new(width, 1), layout.storage.minimum_row_bytes(width).ok_or(Error::Allocation)?, layout.storage, RowOrigin::TopLeft, layout.semantics).map_err(from_core)
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
