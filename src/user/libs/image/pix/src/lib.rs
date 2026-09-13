#![no_std]

extern crate alloc;

use alloc::vec::Vec;

use graphics_core::composite::{BlendMode, Operator, composite};
use graphics_core::format::{PackedChannel, PackedRgbLayout};
use graphics_core::geom::Extent2D;
use graphics_core::layout::{ImageLayout, RowOrigin};
use graphics_core::pixel::{Decoder, Rgba as Colour, Working, quantise, write_packed};
use graphics_core::semantics::ImageSemantics;
use graphics_core::view::{ImageView, ImageViewMut};
use graphics_core::{AlphaMode, ColorSpace, PixelFormat, PixelStorage};

#[cfg(test)]
extern crate std;

pub const MAX_DIMENSION: u32 = 16_384;
pub const MAX_PIXELS: u64 = 16_777_216;
pub const MAX_ANIMATION_FRAMES: usize = 4_096;
pub const MAX_ANIMATION_PIXELS: u64 = 67_108_864;
pub const MAX_ANIMATION_DURATION_MS: u64 = 7 * 24 * 60 * 60 * 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Invalid,
	TooLarge,
}

/// An RGBA8 image, WITH ITS MEANING.
///
/// AN IMAGE WITH NO COLOUR METADATA IS A BACK DOOR INTO THE IMAGE MODEL. Width, height, pitch and
/// bytes say nothing about what a byte MEANS: whether 128 is half the light or half the encoded
/// value, whether the colour has already been multiplied by its alpha, and which primaries it was
/// authored against. Every consumer that guessed guessed sRGB with straight alpha and was usually
/// right, which is what makes the door hard to notice.
///
/// SO THE SEMANTICS TRAVEL WITH THE PIXELS, and the default is what every decoder in this tree
/// actually produces - sRGB, straight alpha - stated rather than assumed. A decoder that knows
/// better says so; a consumer that hands these pixels to `graphics-core` hands over the semantics
/// with them, through `view`, and there is no other way in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
	pub width: u32,
	pub height: u32,
	pub pitch: u32,
	pub pixels: Vec<u8>,
	/// What the bytes mean. `Color { srgb, straight }` unless a decoder says otherwise.
	pub semantics: ImageSemantics,
}

/// What a decoder produces unless it says otherwise: sRGB, straight alpha.
pub const DEFAULT_SEMANTICS: ImageSemantics = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Straight };

impl RgbaImage {
	pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, Error> {
		Self::new_with_semantics(width, height, pixels, DEFAULT_SEMANTICS)
	}

	/// The same, for a decoder that knows what its bytes mean - an opaque format whose alpha lane is
	/// padding, or a source tagged with a wider colour space.
	pub fn new_with_semantics(width: u32, height: u32, pixels: Vec<u8>, semantics: ImageSemantics) -> Result<Self, Error> {
		validate_geometry(width, height)?;
		let pitch = width.checked_mul(4).ok_or(Error::TooLarge)?;
		let expected = usize::try_from(pitch).ok().and_then(|pitch| pitch.checked_mul(height as usize)).ok_or(Error::TooLarge)?;
		if pixels.len() != expected {
			return Err(Error::Invalid);
		}
		Ok(Self { width, height, pitch, pixels, semantics })
	}

	/// This image as the shared model sees it: a CHECKED view, which is the only way its pixels enter
	/// anything that draws.
	///
	/// THE CHECK IS NOT CEREMONY. The layout constructor is what proves the pitch holds a row, that
	/// the alpha mode is one the format admits and that the buffer is long enough - and a view built
	/// here rather than by each consumer is the check happening once instead of nowhere.
	pub fn view(&self) -> Result<ImageView<'_>, Error> {
		let layout = self.layout()?;
		ImageView::new(layout, &self.pixels).map_err(|_| Error::Invalid)
	}

	pub fn view_mut(&mut self) -> Result<ImageViewMut<'_>, Error> {
		let layout = self.layout()?;
		ImageViewMut::new(layout, &mut self.pixels).map_err(|_| Error::Invalid)
	}

	fn layout(&self) -> Result<ImageLayout, Error> {
		ImageLayout::new(Extent2D::new(self.width, self.height), self.pitch, PixelStorage::Known(PixelFormat::R8G8B8A8Unorm), RowOrigin::TopLeft, self.semantics).map_err(|_| Error::Invalid)
	}

	pub fn pixel_count(&self) -> u64 {
		self.width as u64 * self.height as u64
	}

	pub fn as_rgba(&self) -> Rgba<'_> {
		Rgba { data: &self.pixels, width: self.width, height: self.height, pitch: self.pitch }
	}

	pub fn to_bgrx(&self) -> Result<Vec<u8>, Error> {
		let mut output = Vec::new();
		output.try_reserve_exact(self.pixels.len()).map_err(|_| Error::TooLarge)?;
		for pixel in self.pixels.chunks_exact(4) {
			let alpha = pixel[3] as u16;
			output.push((pixel[2] as u16 * alpha / 255) as u8);
			output.push((pixel[1] as u16 * alpha / 255) as u8);
			output.push((pixel[0] as u16 * alpha / 255) as u8);
			output.push(0);
		}
		Ok(output)
	}
}

#[derive(Clone, Copy)]
pub struct Rgba<'a> {
	pub data: &'a [u8],
	pub width: u32,
	pub height: u32,
	pub pitch: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blend {
	Source,
	Over,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposal {
	Keep,
	Background,
	Previous,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
	pub image: RgbaImage,
	pub x: u32,
	pub y: u32,
	pub duration_ms: u32,
	pub blend: Blend,
	pub disposal: Disposal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Animation {
	pub width: u32,
	pub height: u32,
	pub background: [u8; 4],
	pub loop_count: u32,
	pub frames: Vec<Frame>,
}

impl Animation {
	pub fn new(width: u32, height: u32, loop_count: u32, frames: Vec<Frame>) -> Result<Self, Error> {
		Self::new_with_background(width, height, [0; 4], loop_count, frames)
	}

	pub fn new_with_background(width: u32, height: u32, background: [u8; 4], loop_count: u32, frames: Vec<Frame>) -> Result<Self, Error> {
		validate_geometry(width, height)?;
		if frames.is_empty() || frames.len() > MAX_ANIMATION_FRAMES {
			return Err(if frames.is_empty() { Error::Invalid } else { Error::TooLarge });
		}
		let mut cumulative_pixels = 0u64;
		let mut cumulative_duration = 0u64;
		for frame in &frames {
			let end_x = frame.x.checked_add(frame.image.width).ok_or(Error::TooLarge)?;
			let end_y = frame.y.checked_add(frame.image.height).ok_or(Error::TooLarge)?;
			if end_x > width || end_y > height {
				return Err(Error::Invalid);
			}
			cumulative_pixels = cumulative_pixels.checked_add(frame.image.pixel_count()).ok_or(Error::TooLarge)?;
			if cumulative_pixels > MAX_ANIMATION_PIXELS {
				return Err(Error::TooLarge);
			}
			cumulative_duration = cumulative_duration.checked_add(frame.duration_ms as u64).ok_or(Error::TooLarge)?;
			if cumulative_duration > MAX_ANIMATION_DURATION_MS {
				return Err(Error::TooLarge);
			}
		}
		Ok(Self { width, height, background, loop_count, frames })
	}

	pub fn still(image: RgbaImage) -> Self {
		Self { width: image.width, height: image.height, background: [0; 4], loop_count: 1, frames: alloc::vec![Frame { image, x: 0, y: 0, duration_ms: 1, blend: Blend::Source, disposal: Disposal::Keep }] }
	}
}

pub struct Compositor {
	canvas: RgbaImage,
	background: [u8; 4],
}

impl Compositor {
	pub fn new(width: u32, height: u32) -> Result<Self, Error> {
		Self::new_with_background(width, height, [0; 4])
	}

	/// A compositor over a canvas of stated meaning. THE BACKGROUND IS IN THE CANVAS'S OWN SEMANTICS,
	/// because a colour is not a colour until something says what its numbers mean.
	pub fn new_with_semantics(width: u32, height: u32, background: [u8; 4], semantics: ImageSemantics) -> Result<Self, Error> {
		let mut compositor = Self::new_with_background(width, height, background)?;
		compositor.canvas.semantics = semantics;
		Ok(compositor)
	}

	pub fn new_with_background(width: u32, height: u32, background: [u8; 4]) -> Result<Self, Error> {
		let length = usize::try_from(width).ok().and_then(|width| width.checked_mul(height as usize)).and_then(|pixels| pixels.checked_mul(4)).ok_or(Error::TooLarge)?;
		let mut pixels = alloc::vec![0; length];
		for pixel in pixels.chunks_exact_mut(4) {
			pixel.copy_from_slice(&background);
		}
		Ok(Self { canvas: RgbaImage::new(width, height, pixels)?, background })
	}

	pub fn render(&mut self, frame: &Frame) -> Result<RgbaImage, Error> {
		let end_x = frame.x.checked_add(frame.image.width).ok_or(Error::TooLarge)?;
		let end_y = frame.y.checked_add(frame.image.height).ok_or(Error::TooLarge)?;
		if end_x > self.canvas.width || end_y > self.canvas.height {
			return Err(Error::Invalid);
		}
		let previous = matches!(frame.disposal, Disposal::Previous).then(|| self.canvas.pixels.clone());
		for y in 0..frame.image.height {
			for x in 0..frame.image.width {
				let source = y as usize * frame.image.pitch as usize + x as usize * 4;
				let destination = (frame.y + y) as usize * self.canvas.pitch as usize + (frame.x + x) as usize * 4;
				let pixel: [u8; 4] = frame.image.pixels.get(source..source + 4).ok_or(Error::Invalid)?.try_into().map_err(|_| Error::Invalid)?;
				if frame.blend == Blend::Source {
					self.canvas.pixels[destination..destination + 4].copy_from_slice(&pixel);
				} else {
					// THE CANVAS'S OWN SEMANTICS AND NOT AN ASSUMED sRGB. A compositor that blended
					// every canvas as though it were sRGB would be the back door this crate just
					// closed, reopened one layer up.
					blend_over(&mut self.canvas.pixels[destination..destination + 4], pixel, self.canvas.semantics);
				}
			}
		}
		let displayed = self.canvas.clone();
		match frame.disposal {
			Disposal::Keep => {}
			Disposal::Background => {
				for y in 0..frame.image.height {
					let start = (frame.y + y) as usize * self.canvas.pitch as usize + frame.x as usize * 4;
					for pixel in self.canvas.pixels[start..start + frame.image.width as usize * 4].chunks_exact_mut(4) {
						pixel.copy_from_slice(&self.background);
					}
				}
			}
			Disposal::Previous => self.canvas.pixels = previous.ok_or(Error::Invalid)?,
		}
		Ok(displayed)
	}
}

/// Source-over, through the ONE compositing implementation.
///
/// THE WORKING SPACE IS STATED AND NOT ASSUMED. This blends ENCODED sRGB bytes with STRAIGHT alpha,
/// which is what an animated image's frames are and what this crate has always done - it is cheap,
/// it is wrong in the darks, and for a decoder handing frames to a viewer it is the answer the format
/// itself specifies. Saying so is what stops it being the silent default in a compositor that needs
/// light instead; the same function, told `LinearPremultiplied`, is what `soft2d` composites with.
fn blend_over(destination: &mut [u8], source: [u8; 4], semantics: ImageSemantics) {
	// THE WORKING SPACE IS THE CANVAS'S OWN, encoded and straight - which is what this crate's frames
	// are and what the animation formats themselves specify. A compositor that blended every canvas as
	// though it were sRGB would be the back door this crate just closed, reopened one layer up.
	let space = semantics.color_space().unwrap_or(ColorSpace::Srgb);
	let Ok(decoder) = Decoder::new(&semantics, Working::EncodedStraight(space)) else { return };
	// THE DECODER IS WHAT APPLIES THE WORKING SPACE. Told `EncodedStraight` it premultiplies and
	// leaves the transfer function alone, which is this crate's contract; told `LinearPremultiplied`
	// the same function decodes to light, which is what a compositor wants. Neither is written here.
	let bytes_to_colour = |bytes: [u8; 4]| decoder.decode(Colour::new(bytes[0] as f32 / 255.0, bytes[1] as f32 / 255.0, bytes[2] as f32 / 255.0, bytes[3] as f32 / 255.0));
	let backdrop = bytes_to_colour([destination[0], destination[1], destination[2], destination[3]]);
	let result = composite(Operator::SrcOver, BlendMode::Normal, bytes_to_colour(source), backdrop);
	if result.alpha <= 0.0 {
		destination.fill(0);
		return;
	}
	for (index, channel) in [result.red, result.green, result.blue].into_iter().enumerate() {
		destination[index] = quantise(channel / result.alpha, 255.0) as u8;
	}
	destination[3] = quantise(result.alpha, 255.0) as u8;
}

fn validate_geometry(width: u32, height: u32) -> Result<(), Error> {
	if width == 0 || height == 0 {
		return Err(Error::Invalid);
	}
	if width > MAX_DIMENSION || height > MAX_DIMENSION || width as u64 * height as u64 > MAX_PIXELS {
		return Err(Error::TooLarge);
	}
	Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
	pub x: u32,
	pub y: u32,
	pub width: u32,
	pub height: u32,
}

pub struct Image<'a> {
	pub data: &'a [u8],
	pub width: u32,
	pub height: u32,
	pub pitch: u32,
}

pub struct Target<'a> {
	pub data: &'a mut [u8],
	pub width: u32,
	pub height: u32,
	pub pitch: u32,
	pub bytes_per_pixel: u32,
	pub red_shift: u8,
	pub red_size: u8,
	pub green_shift: u8,
	pub green_size: u8,
	pub blue_shift: u8,
	pub blue_size: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlitResult {
	pub rect: Rect,
	pub pixels: u64,
	pub direct: bool,
}

// Image-internal dynamic-link smoke symbol. The explicit unmangled ABI is generated
// and consumed within one system image; it is not a cross-release public contract.
#[unsafe(no_mangle)]
pub extern "C" fn liber_pix_probe() -> u64 {
	0x4c49_4250_4958_4f4b
}

pub fn blit(source: Image<'_>, mut target: Target<'_>, damage: Rect, first: bool) -> Option<BlitResult> {
	validate(&source, &target, damage)?;
	let direct = source.width == target.width && source.height == target.height && target.bytes_per_pixel == 4 && target.red_shift == 16 && target.red_size == 8 && target.green_shift == 8 && target.green_size == 8 && target.blue_shift == 0 && target.blue_size == 8;
	if direct {
		let rect = if first { Rect { x: 0, y: 0, width: source.width, height: source.height } } else { damage };
		let bytes = rect.width as usize * 4;
		for row in rect.y..rect.y + rect.height {
			let src = row as usize * source.pitch as usize + rect.x as usize * 4;
			let dst = row as usize * target.pitch as usize + rect.x as usize * 4;
			target.data[dst..dst + bytes].copy_from_slice(&source.data[src..src + bytes]);
		}
		return Some(BlitResult { rect, pixels: rect.width as u64 * rect.height as u64, direct: true });
	}

	let sw = source.width as u64;
	let sh = source.height as u64;
	let dw = target.width as u64;
	let dh = target.height as u64;
	let width_limited = dw.saturating_mul(sh) <= dh.saturating_mul(sw);
	let (out_width, out_height) = if width_limited { (target.width, ((sh * dw) / sw).max(1) as u32) } else { (((sw * dh) / sh).max(1) as u32, target.height) };
	let offset_x = (target.width - out_width) / 2;
	let offset_y = (target.height - out_height) / 2;
	let (x0, y0, x1, y1) = if first {
		target.data.fill(0);
		(0, 0, out_width, out_height)
	} else {
		let end_x = (damage.x + damage.width) as u64 * out_width as u64;
		let end_y = (damage.y + damage.height) as u64 * out_height as u64;
		((damage.x as u64 * out_width as u64 / sw) as u32, (damage.y as u64 * out_height as u64 / sh) as u32, end_x.div_ceil(sw) as u32, end_y.div_ceil(sh) as u32)
	};
	for output_y in y0..y1 {
		let source_y = (output_y as u64 * source.height as u64 / out_height as u64) as u32;
		for output_x in x0..x1 {
			let source_x = (output_x as u64 * source.width as u64 / out_width as u64) as u32;
			let source_offset = source_y as usize * source.pitch as usize + source_x as usize * 4;
			let pixel = u32::from_le_bytes(source.data[source_offset..source_offset + 4].try_into().ok()?);
			write_pixel(&mut target, offset_x + output_x, offset_y + output_y, pixel);
		}
	}
	let width = x1 - x0;
	let height = y1 - y0;
	let written = width as u64 * height as u64 + if first { target.width as u64 * target.height as u64 } else { 0 };
	let rect = if first { Rect { x: 0, y: 0, width: target.width, height: target.height } } else { Rect { x: offset_x + x0, y: offset_y + y0, width, height } };
	Some(BlitResult { rect, pixels: written, direct: false })
}

pub fn blit_crop(source: Image<'_>, mut target: Target<'_>, source_x: u32, source_y: u32) -> Option<BlitResult> {
	validate(&source, &target, Rect { x: 0, y: 0, width: source.width, height: source.height })?;
	if source_x >= source.width || source_y >= source.height {
		return None;
	}
	let width = (source.width - source_x).min(target.width);
	let height = (source.height - source_y).min(target.height);
	let offset_x = (target.width - width) / 2;
	let offset_y = (target.height - height) / 2;
	target.data.fill(0);
	for y in 0..height {
		for x in 0..width {
			let source_offset = (source_y + y) as usize * source.pitch as usize + (source_x + x) as usize * 4;
			let pixel = u32::from_le_bytes(source.data[source_offset..source_offset + 4].try_into().ok()?);
			write_pixel(&mut target, offset_x + x, offset_y + y, pixel);
		}
	}
	Some(BlitResult { rect: Rect { x: 0, y: 0, width: target.width, height: target.height }, pixels: target.width as u64 * target.height as u64, direct: false })
}

pub fn blit_view(source: Image<'_>, mut target: Target<'_>, view_width: u32, view_height: u32, view_x: u32, view_y: u32) -> Option<BlitResult> {
	validate(&source, &target, Rect { x: 0, y: 0, width: source.width, height: source.height })?;
	if view_width == 0 || view_height == 0 {
		return None;
	}
	let max_x = view_width.saturating_sub(target.width);
	let max_y = view_height.saturating_sub(target.height);
	let view_x = view_x.min(max_x);
	let view_y = view_y.min(max_y);
	let offset_x = target.width.saturating_sub(view_width) / 2;
	let offset_y = target.height.saturating_sub(view_height) / 2;
	target.data.fill(0);
	for output_y in 0..target.height {
		let display_y = if view_height > target.height {
			view_y as u64 + output_y as u64
		} else if output_y < offset_y {
			continue;
		} else {
			(output_y - offset_y) as u64
		};
		if display_y >= view_height as u64 {
			continue;
		}
		let source_y = (display_y * source.height as u64 / view_height as u64).min(source.height as u64 - 1) as u32;
		for output_x in 0..target.width {
			let display_x = if view_width > target.width {
				view_x as u64 + output_x as u64
			} else if output_x < offset_x {
				continue;
			} else {
				(output_x - offset_x) as u64
			};
			if display_x >= view_width as u64 {
				continue;
			}
			let source_x = (display_x * source.width as u64 / view_width as u64).min(source.width as u64 - 1) as u32;
			let source_offset = source_y as usize * source.pitch as usize + source_x as usize * 4;
			let pixel = u32::from_le_bytes(source.data[source_offset..source_offset + 4].try_into().ok()?);
			write_pixel(&mut target, output_x, output_y, pixel);
		}
	}
	Some(BlitResult { rect: Rect { x: 0, y: 0, width: target.width, height: target.height }, pixels: target.width as u64 * target.height as u64, direct: false })
}

fn validate(source: &Image<'_>, target: &Target<'_>, damage: Rect) -> Option<()> {
	if source.width == 0 || source.height == 0 || target.width == 0 || target.height == 0 {
		return None;
	}
	if source.pitch < source.width.checked_mul(4)? || target.bytes_per_pixel == 0 || target.bytes_per_pixel > 4 || target.pitch < target.width.checked_mul(target.bytes_per_pixel)? {
		return None;
	}
	let source_len = source.pitch as usize * source.height as usize;
	let target_len = target.pitch as usize * target.height as usize;
	if source.data.len() < source_len || target.data.len() < target_len {
		return None;
	}
	let end_x = damage.x.checked_add(damage.width)?;
	let end_y = damage.y.checked_add(damage.height)?;
	if damage.width == 0 || damage.height == 0 || end_x > source.width || end_y > source.height {
		return None;
	}
	Some(())
}

/// One pixel into a firmware-described packed layout, through the ONE packer.
///
/// A LITERAL 32 HERE ONCE GAVE A DIAGONAL SMEAR, which is what a private packer in every consumer
/// eventually produces: the element size, the shifts and the widths have to come from the layout the
/// firmware described, and there is one implementation of that arithmetic in `graphics-core`.
fn write_pixel(target: &mut Target<'_>, x: u32, y: u32, bgrx: u32) {
	let layout = packed_layout(target);
	let offset = y as usize * target.pitch as usize + x as usize * target.bytes_per_pixel as usize;
	let width = target.bytes_per_pixel as usize;
	let Some(pixel) = target.data.get_mut(offset..offset + width) else { return };
	let value = Colour::new(((bgrx >> 16) & 0xff) as f32 / 255.0, ((bgrx >> 8) & 0xff) as f32 / 255.0, (bgrx & 0xff) as f32 / 255.0, 1.0);
	write_packed(layout, pixel, value);
}

/// This target's channel masks, as the shared model describes them.
fn packed_layout(target: &Target<'_>) -> PackedRgbLayout {
	PackedRgbLayout {
		bytes_per_pixel: target.bytes_per_pixel.min(8) as u8,
		red: PackedChannel { shift: target.red_shift, bits: target.red_size },
		green: PackedChannel { shift: target.green_shift, bits: target.green_size },
		blue: PackedChannel { shift: target.blue_shift, bits: target.blue_size },
		// THE RESERVED BITS ARE NOT THIS BLITTER'S TO SET. A scanout target's unused lane is written
		// by whoever owns the format; here the three channels are the whole contract.
		reserved: PackedChannel { shift: 0, bits: 0 },
	}
}

#[cfg(test)]
mod tests;
