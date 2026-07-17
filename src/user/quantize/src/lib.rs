#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use pix::Rgba;

const HISTOGRAM_BITS: usize = 5;
const HISTOGRAM_SIDE: usize = 1 << HISTOGRAM_BITS;
const HISTOGRAM_SIZE: usize = HISTOGRAM_SIDE * HISTOGRAM_SIDE * HISTOGRAM_SIDE;
const EXACT_SLOTS: usize = 512;
const EMPTY_COLOR: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Invalid,
	TooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
	pub quality: u8,
	pub dither: bool,
	pub alpha_threshold: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
	pub colors: Vec<[u8; 4]>,
	pub transparent_index: Option<u8>,
	pub dither: bool,
	pub alpha_threshold: u8,
}

#[derive(Clone, Copy, Default)]
struct Bucket {
	count: u64,
	red: u64,
	green: u64,
	blue: u64,
}

#[derive(Clone, Copy)]
struct Sample {
	red: u8,
	green: u8,
	blue: u8,
	bucket: Bucket,
}

#[derive(Clone, Copy)]
struct ColorBox {
	start: usize,
	end: usize,
	count: u64,
	red_min: u8,
	red_max: u8,
	green_min: u8,
	green_max: u8,
	blue_min: u8,
	blue_max: u8,
}

struct ExactColors {
	slots: Vec<u32>,
	len: usize,
}

impl ExactColors {
	fn new() -> Result<Self, Error> {
		let mut slots = Vec::new();
		slots.try_reserve_exact(EXACT_SLOTS).map_err(|_| Error::TooLarge)?;
		slots.resize(EXACT_SLOTS, EMPTY_COLOR);
		Ok(Self { slots, len: 0 })
	}

	fn insert(&mut self, color: u32) {
		let mut index = color.wrapping_mul(0x9e37_79b1) as usize & (EXACT_SLOTS - 1);
		loop {
			match self.slots[index] {
				EMPTY_COLOR => {
					self.slots[index] = color;
					self.len += 1;
					return;
				}
				stored if stored == color => return,
				_ => index = (index + 1) & (EXACT_SLOTS - 1),
			}
		}
	}
}

pub fn build_palette(images: &[Rgba<'_>], options: Options) -> Result<Palette, Error> {
	if images.is_empty() || options.quality > 100 {
		return Err(Error::Invalid);
	}
	let color_limit = 16usize + options.quality as usize * 240 / 100;
	let mut histogram = Vec::new();
	histogram.try_reserve_exact(HISTOGRAM_SIZE).map_err(|_| Error::TooLarge)?;
	histogram.resize(HISTOGRAM_SIZE, Bucket::default());
	let mut exact = Some(ExactColors::new()?);
	let mut has_transparency = false;
	for image in images {
		validate_image(image)?;
		for row in 0..image.height as usize {
			let start = row.checked_mul(image.pitch as usize).ok_or(Error::TooLarge)?;
			for pixel in image.data[start..start + image.width as usize * 4].chunks_exact(4) {
				if pixel[3] < options.alpha_threshold {
					has_transparency = true;
					if exact.as_ref().is_some_and(|colors| colors.len >= color_limit) {
						exact = None;
					}
					continue;
				}
				let key = u32::from_be_bytes([0, pixel[0], pixel[1], pixel[2]]);
				if let Some(colors) = &mut exact {
					colors.insert(key);
					let available = color_limit.saturating_sub(usize::from(has_transparency));
					if colors.len > available {
						exact = None;
					}
				}
				let index = histogram_index(pixel[0], pixel[1], pixel[2]);
				let bucket = &mut histogram[index];
				bucket.count = bucket.count.checked_add(1).ok_or(Error::TooLarge)?;
				bucket.red = bucket.red.checked_add(pixel[0] as u64).ok_or(Error::TooLarge)?;
				bucket.green = bucket.green.checked_add(pixel[1] as u64).ok_or(Error::TooLarge)?;
				bucket.blue = bucket.blue.checked_add(pixel[2] as u64).ok_or(Error::TooLarge)?;
			}
		}
	}
	let available = color_limit.saturating_sub(usize::from(has_transparency));
	let mut colors = Vec::new();
	colors.try_reserve_exact(color_limit).map_err(|_| Error::TooLarge)?;
	let transparent_index = has_transparency.then_some(0);
	if has_transparency {
		colors.push([0, 0, 0, 0]);
	}
	if let Some(exact) = exact {
		for color in exact.slots.into_iter().filter(|color| *color != EMPTY_COLOR) {
			let bytes = color.to_be_bytes();
			colors.push([bytes[1], bytes[2], bytes[3], 255]);
		}
	} else {
		colors.extend(quantized_colors(&histogram, available)?);
	}
	if colors.is_empty() {
		colors.push([0, 0, 0, 0]);
	}
	Ok(Palette { colors, transparent_index, dither: options.dither, alpha_threshold: options.alpha_threshold })
}

pub fn map_image(image: Rgba<'_>, palette: &Palette) -> Result<Vec<u8>, Error> {
	validate_image(&image)?;
	if palette.colors.is_empty() || palette.colors.len() > 256 {
		return Err(Error::Invalid);
	}
	let pixel_count = (image.width as usize).checked_mul(image.height as usize).ok_or(Error::TooLarge)?;
	let mut indices = Vec::new();
	indices.try_reserve_exact(pixel_count).map_err(|_| Error::TooLarge)?;
	if !palette.dither {
		for row in 0..image.height as usize {
			let start = row.checked_mul(image.pitch as usize).ok_or(Error::TooLarge)?;
			for pixel in image.data[start..start + image.width as usize * 4].chunks_exact(4) {
				indices.push(map_pixel(pixel, palette, [0; 3]));
			}
		}
		return Ok(indices);
	}
	let error_len = (image.width as usize + 2).checked_mul(3).ok_or(Error::TooLarge)?;
	let mut current_error = Vec::new();
	current_error.try_reserve_exact(error_len).map_err(|_| Error::TooLarge)?;
	current_error.resize(error_len, 0i32);
	let mut next_error = current_error.clone();
	for row in 0..image.height as usize {
		let start = row.checked_mul(image.pitch as usize).ok_or(Error::TooLarge)?;
		for (column, pixel) in image.data[start..start + image.width as usize * 4].chunks_exact(4).enumerate() {
			let error_offset = (column + 1) * 3;
			let correction = [current_error[error_offset] / 16, current_error[error_offset + 1] / 16, current_error[error_offset + 2] / 16];
			let index = map_pixel(pixel, palette, correction);
			indices.push(index);
			if pixel[3] < palette.alpha_threshold {
				continue;
			}
			let color = palette.colors[index as usize];
			for channel in 0..3 {
				let adjusted = (pixel[channel] as i32 + correction[channel]).clamp(0, 255);
				let error = adjusted - color[channel] as i32;
				current_error[error_offset + 3 + channel] += error * 7;
				next_error[error_offset - 3 + channel] += error * 3;
				next_error[error_offset + channel] += error * 5;
				next_error[error_offset + 3 + channel] += error;
			}
		}
		core::mem::swap(&mut current_error, &mut next_error);
		next_error.fill(0);
	}
	Ok(indices)
}

fn validate_image(image: &Rgba<'_>) -> Result<(), Error> {
	if image.width == 0 || image.height == 0 || image.width > pix::MAX_DIMENSION || image.height > pix::MAX_DIMENSION || image.width as u64 * image.height as u64 > pix::MAX_PIXELS {
		return Err(Error::Invalid);
	}
	let row_bytes = image.width.checked_mul(4).ok_or(Error::TooLarge)?;
	let length = (image.pitch as usize).checked_mul(image.height as usize).ok_or(Error::TooLarge)?;
	if image.pitch < row_bytes || image.data.len() < length {
		return Err(Error::Invalid);
	}
	Ok(())
}

fn histogram_index(red: u8, green: u8, blue: u8) -> usize {
	((red as usize >> (8 - HISTOGRAM_BITS)) * HISTOGRAM_SIDE + (green as usize >> (8 - HISTOGRAM_BITS))) * HISTOGRAM_SIDE + (blue as usize >> (8 - HISTOGRAM_BITS))
}

fn quantized_colors(histogram: &[Bucket], limit: usize) -> Result<Vec<[u8; 4]>, Error> {
	let mut samples = Vec::new();
	for (index, bucket) in histogram.iter().copied().enumerate() {
		if bucket.count == 0 {
			continue;
		}
		let red = (index / (HISTOGRAM_SIDE * HISTOGRAM_SIDE)) as u8;
		let green = (index / HISTOGRAM_SIDE % HISTOGRAM_SIDE) as u8;
		let blue = (index % HISTOGRAM_SIDE) as u8;
		samples.push(Sample { red, green, blue, bucket });
	}
	if samples.is_empty() || limit == 0 {
		return Ok(Vec::new());
	}
	let mut boxes = alloc::vec![make_box(&samples, 0, samples.len())];
	while boxes.len() < limit {
		let Some((box_index, color_box)) = boxes.iter().copied().enumerate().filter(|(_, color_box)| color_box.end - color_box.start > 1).max_by_key(|(_, color_box)| box_score(*color_box)) else {
			break;
		};
		let red_range = color_box.red_max - color_box.red_min;
		let green_range = color_box.green_max - color_box.green_min;
		let blue_range = color_box.blue_max - color_box.blue_min;
		let channel = if red_range >= green_range && red_range >= blue_range {
			0
		} else if green_range >= blue_range {
			1
		} else {
			2
		};
		heap_sort_samples(&mut samples[color_box.start..color_box.end], channel);
		let midpoint_count = color_box.count.div_ceil(2);
		let mut cumulative = 0u64;
		let mut split = color_box.start + 1;
		for (offset, sample) in samples[color_box.start..color_box.end - 1].iter().enumerate() {
			cumulative += sample.bucket.count;
			if cumulative >= midpoint_count {
				split = color_box.start + offset + 1;
				break;
			}
		}
		boxes[box_index] = make_box(&samples, color_box.start, split);
		boxes.push(make_box(&samples, split, color_box.end));
	}
	let mut colors = Vec::new();
	colors.try_reserve_exact(boxes.len()).map_err(|_| Error::TooLarge)?;
	for color_box in boxes {
		let mut count = 0u64;
		let mut red = 0u64;
		let mut green = 0u64;
		let mut blue = 0u64;
		for sample in &samples[color_box.start..color_box.end] {
			count += sample.bucket.count;
			red += sample.bucket.red;
			green += sample.bucket.green;
			blue += sample.bucket.blue;
		}
		colors.push([(red / count) as u8, (green / count) as u8, (blue / count) as u8, 255]);
	}
	Ok(colors)
}

fn make_box(samples: &[Sample], start: usize, end: usize) -> ColorBox {
	let mut color_box = ColorBox { start, end, count: 0, red_min: u8::MAX, red_max: 0, green_min: u8::MAX, green_max: 0, blue_min: u8::MAX, blue_max: 0 };
	for sample in &samples[start..end] {
		color_box.count += sample.bucket.count;
		color_box.red_min = color_box.red_min.min(sample.red);
		color_box.red_max = color_box.red_max.max(sample.red);
		color_box.green_min = color_box.green_min.min(sample.green);
		color_box.green_max = color_box.green_max.max(sample.green);
		color_box.blue_min = color_box.blue_min.min(sample.blue);
		color_box.blue_max = color_box.blue_max.max(sample.blue);
	}
	color_box
}

fn box_score(color_box: ColorBox) -> u64 {
	let range = (color_box.red_max - color_box.red_min).max(color_box.green_max - color_box.green_min).max(color_box.blue_max - color_box.blue_min) as u64;
	range * color_box.count
}

fn heap_sort_samples(samples: &mut [Sample], channel: usize) {
	for root in (0..samples.len() / 2).rev() {
		sift_down(samples, root, samples.len(), channel);
	}
	for end in (1..samples.len()).rev() {
		samples.swap(0, end);
		sift_down(samples, 0, end, channel);
	}
}

fn sift_down(samples: &mut [Sample], mut root: usize, end: usize, channel: usize) {
	loop {
		let child = root * 2 + 1;
		if child >= end {
			return;
		}
		let right = child + 1;
		let largest = if right < end && sample_key(samples[right], channel) > sample_key(samples[child], channel) { right } else { child };
		if sample_key(samples[root], channel) >= sample_key(samples[largest], channel) {
			return;
		}
		samples.swap(root, largest);
		root = largest;
	}
}

fn sample_key(sample: Sample, channel: usize) -> u32 {
	match channel {
		0 => (sample.red as u32) << 16 | (sample.green as u32) << 8 | sample.blue as u32,
		1 => (sample.green as u32) << 16 | (sample.red as u32) << 8 | sample.blue as u32,
		_ => (sample.blue as u32) << 16 | (sample.red as u32) << 8 | sample.green as u32,
	}
}

fn map_pixel(pixel: &[u8], palette: &Palette, correction: [i32; 3]) -> u8 {
	if pixel[3] < palette.alpha_threshold {
		return palette.transparent_index.unwrap_or(0);
	}
	let first = usize::from(palette.transparent_index.is_some());
	let red = (pixel[0] as i32 + correction[0]).clamp(0, 255);
	let green = (pixel[1] as i32 + correction[1]).clamp(0, 255);
	let blue = (pixel[2] as i32 + correction[2]).clamp(0, 255);
	palette.colors[first..]
		.iter()
		.enumerate()
		.min_by_key(|(_, color)| {
			let red_delta = red - color[0] as i32;
			let green_delta = green - color[1] as i32;
			let blue_delta = blue - color[2] as i32;
			red_delta * red_delta + green_delta * green_delta + blue_delta * blue_delta
		})
		.map(|(index, _)| (first + index) as u8)
		.unwrap_or_else(|| palette.transparent_index.unwrap_or(0))
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;
