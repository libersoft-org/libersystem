#![no_std]

extern crate alloc;

use alloc::vec::Vec;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
	pub width: u32,
	pub height: u32,
	pub pitch: u32,
	pub pixels: Vec<u8>,
}

impl RgbaImage {
	pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, Error> {
		validate_geometry(width, height)?;
		let pitch = width.checked_mul(4).ok_or(Error::TooLarge)?;
		let expected = usize::try_from(pitch).ok().and_then(|pitch| pitch.checked_mul(height as usize)).ok_or(Error::TooLarge)?;
		if pixels.len() != expected {
			return Err(Error::Invalid);
		}
		Ok(Self { width, height, pitch, pixels })
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
					blend_over(&mut self.canvas.pixels[destination..destination + 4], pixel);
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

fn blend_over(destination: &mut [u8], source: [u8; 4]) {
	let source_alpha = source[3] as u32;
	let destination_alpha = destination[3] as u32;
	let out_alpha = source_alpha + (destination_alpha * (255 - source_alpha) + 127) / 255;
	if out_alpha == 0 {
		destination.fill(0);
		return;
	}
	for channel in 0..3 {
		let numerator = source[channel] as u32 * source_alpha * 255 + destination[channel] as u32 * destination_alpha * (255 - source_alpha);
		destination[channel] = ((numerator + out_alpha * 127) / (out_alpha * 255)) as u8;
	}
	destination[3] = out_alpha as u8;
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

fn write_pixel(target: &mut Target<'_>, x: u32, y: u32, bgrx: u32) {
	let red = (bgrx >> 16) & 0xff;
	let green = (bgrx >> 8) & 0xff;
	let blue = bgrx & 0xff;
	let packed = scale_channel(red, target.red_size) << target.red_shift | scale_channel(green, target.green_size) << target.green_shift | scale_channel(blue, target.blue_size) << target.blue_shift;
	let offset = y as usize * target.pitch as usize + x as usize * target.bytes_per_pixel as usize;
	for byte in 0..target.bytes_per_pixel as usize {
		target.data[offset + byte] = (packed >> (byte * 8)) as u8;
	}
}

fn scale_channel(value: u32, bits: u8) -> u32 {
	if bits == 0 {
		0
	} else if bits >= 8 {
		value
	} else {
		value >> (8 - bits)
	}
}

#[cfg(test)]
mod tests;
