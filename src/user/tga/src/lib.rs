#![no_std]

extern crate alloc;

use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Truncated,
	Invalid,
	Unsupported,
	TooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct EncodeOptions {
	pub rle: bool,
}

pub fn decode(data: &[u8]) -> Result<pix::RgbaImage, Error> {
	let header = data.get(..18).ok_or(Error::Truncated)?;
	let id_len = header[0] as usize;
	if header[1] != 0 {
		return Err(Error::Unsupported);
	}
	let rle = match header[2] {
		2 => false,
		10 => true,
		_ => return Err(Error::Unsupported),
	};
	let width = u16::from_le_bytes([header[12], header[13]]) as u32;
	let height = u16::from_le_bytes([header[14], header[15]]) as u32;
	if width == 0 || height == 0 {
		return Err(Error::Invalid);
	}
	if width > pix::MAX_DIMENSION || height > pix::MAX_DIMENSION || width as u64 * height as u64 > pix::MAX_PIXELS {
		return Err(Error::TooLarge);
	}
	let bytes_per_pixel = match header[16] {
		24 => 3,
		32 => 4,
		_ => return Err(Error::Unsupported),
	};
	let right_to_left = header[17] & 0x10 != 0;
	let top_down = header[17] & 0x20 != 0;
	let count = usize::try_from(width).ok().and_then(|width| width.checked_mul(height as usize)).ok_or(Error::TooLarge)?;
	let start = 18usize.checked_add(id_len).ok_or(Error::TooLarge)?;
	let source = data.get(start..).ok_or(Error::Truncated)?;
	let mut file_pixels: Vec<[u8; 4]> = Vec::new();
	file_pixels.try_reserve_exact(count).map_err(|_| Error::TooLarge)?;
	let mut cursor = 0usize;
	while file_pixels.len() < count {
		if !rle {
			file_pixels.push(read_pixel(source, &mut cursor, bytes_per_pixel)?);
			continue;
		}
		let packet = *source.get(cursor).ok_or(Error::Truncated)?;
		cursor += 1;
		let length = (packet as usize & 0x7f) + 1;
		if file_pixels.len().checked_add(length).filter(|end| *end <= count).is_none() {
			return Err(Error::Invalid);
		}
		if packet & 0x80 != 0 {
			let pixel = read_pixel(source, &mut cursor, bytes_per_pixel)?;
			file_pixels.extend(core::iter::repeat_n(pixel, length));
		} else {
			for _ in 0..length {
				file_pixels.push(read_pixel(source, &mut cursor, bytes_per_pixel)?);
			}
		}
	}
	let mut pixels = Vec::new();
	pixels.try_reserve_exact(count * 4).map_err(|_| Error::TooLarge)?;
	pixels.resize(count * 4, 0);
	for file_y in 0..height {
		for file_x in 0..width {
			let x = if right_to_left { width - 1 - file_x } else { file_x };
			let y = if top_down { file_y } else { height - 1 - file_y };
			let source = file_pixels[(file_y * width + file_x) as usize];
			let target = (y as usize * width as usize + x as usize) * 4;
			pixels[target..target + 4].copy_from_slice(&source);
		}
	}
	pix::RgbaImage::new(width, height, pixels).map_err(map_pix)
}

pub fn encode(image: &pix::RgbaImage, options: EncodeOptions) -> Result<Vec<u8>, Error> {
	if image.width > u16::MAX as u32 || image.height > u16::MAX as u32 {
		return Err(Error::TooLarge);
	}
	let alpha = image.pixels.chunks_exact(4).any(|pixel| pixel[3] != 255);
	let bytes_per_pixel = if alpha { 4 } else { 3 };
	let mut output = Vec::new();
	output.try_reserve(18 + image.pixel_count() as usize * bytes_per_pixel).map_err(|_| Error::TooLarge)?;
	output.resize(18, 0);
	output[2] = if options.rle { 10 } else { 2 };
	output[12..14].copy_from_slice(&(image.width as u16).to_le_bytes());
	output[14..16].copy_from_slice(&(image.height as u16).to_le_bytes());
	output[16] = (bytes_per_pixel * 8) as u8;
	output[17] = 0x20 | if alpha { 8 } else { 0 };
	let pixels: Vec<[u8; 4]> = image.pixels.chunks_exact(4).map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]]).collect();
	if options.rle {
		encode_rle(&pixels, bytes_per_pixel, &mut output);
	} else {
		for pixel in &pixels {
			write_pixel(&mut output, *pixel, bytes_per_pixel);
		}
	}
	Ok(output)
}

fn encode_rle(pixels: &[[u8; 4]], bytes_per_pixel: usize, output: &mut Vec<u8>) {
	let mut index = 0usize;
	while index < pixels.len() {
		let run = run_len(pixels, index);
		if run >= 2 {
			output.push(0x80 | (run - 1) as u8);
			write_pixel(output, pixels[index], bytes_per_pixel);
			index += run;
			continue;
		}
		let start = index;
		index += 1;
		while index - start < 128 && index < pixels.len() && run_len(pixels, index) < 2 {
			index += 1;
		}
		output.push((index - start - 1) as u8);
		for pixel in &pixels[start..index] {
			write_pixel(output, *pixel, bytes_per_pixel);
		}
	}
}

fn run_len(pixels: &[[u8; 4]], start: usize) -> usize {
	let mut length = 1usize;
	while length < 128 && start + length < pixels.len() && pixels[start + length] == pixels[start] {
		length += 1;
	}
	length
}

fn read_pixel(source: &[u8], cursor: &mut usize, bytes_per_pixel: usize) -> Result<[u8; 4], Error> {
	let end = cursor.checked_add(bytes_per_pixel).ok_or(Error::TooLarge)?;
	let bytes = source.get(*cursor..end).ok_or(Error::Truncated)?;
	*cursor = end;
	Ok([bytes[2], bytes[1], bytes[0], if bytes_per_pixel == 4 { bytes[3] } else { 255 }])
}

fn write_pixel(output: &mut Vec<u8>, pixel: [u8; 4], bytes_per_pixel: usize) {
	output.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
	if bytes_per_pixel == 4 {
		output.push(pixel[3]);
	}
}

fn map_pix(error: pix::Error) -> Error {
	match error {
		pix::Error::Invalid => Error::Invalid,
		pix::Error::TooLarge => Error::TooLarge,
	}
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
	use super::*;
	use alloc::vec;

	#[test]
	fn raw_and_rle_round_trip_rgba() {
		let image = pix::RgbaImage::new(3, 2, vec![255, 0, 0, 255, 255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 255, 1, 2, 3, 4, 1, 2, 3, 4]).unwrap();
		for rle in [false, true] {
			assert_eq!(decode(&encode(&image, EncodeOptions { rle }).unwrap()).unwrap(), image);
		}
	}

	#[test]
	fn rejects_truncation_and_unsupported_colormap() {
		assert_eq!(decode(&[]), Err(Error::Truncated));
		let mut header = [0u8; 18];
		header[1] = 1;
		header[2] = 2;
		assert_eq!(decode(&header), Err(Error::Unsupported));
	}
}
