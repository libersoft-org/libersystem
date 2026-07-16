#![no_std]

extern crate alloc;

use alloc::vec::Vec;

const HEADER_LEN: usize = 128;
const PALETTE_LEN: usize = 769;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Truncated,
	Invalid,
	Unsupported,
	TooLarge,
}

pub fn decode(data: &[u8]) -> Result<pix::RgbaImage, Error> {
	let header = data.get(..HEADER_LEN).ok_or(Error::Truncated)?;
	if header[0] != 0x0a || header[2] != 1 {
		return Err(Error::Invalid);
	}
	if header[3] != 8 {
		return Err(Error::Unsupported);
	}
	let x_min = u16::from_le_bytes([header[4], header[5]]);
	let y_min = u16::from_le_bytes([header[6], header[7]]);
	let x_max = u16::from_le_bytes([header[8], header[9]]);
	let y_max = u16::from_le_bytes([header[10], header[11]]);
	if x_max < x_min || y_max < y_min {
		return Err(Error::Invalid);
	}
	let width = (x_max - x_min) as u32 + 1;
	let height = (y_max - y_min) as u32 + 1;
	if width > pix::MAX_DIMENSION || height > pix::MAX_DIMENSION || width as u64 * height as u64 > pix::MAX_PIXELS {
		return Err(Error::TooLarge);
	}
	let planes = header[65] as usize;
	if !matches!(planes, 1 | 3) {
		return Err(Error::Unsupported);
	}
	let bytes_per_line = u16::from_le_bytes([header[66], header[67]]) as usize;
	if bytes_per_line < width as usize || bytes_per_line == 0 {
		return Err(Error::Invalid);
	}
	let expected = (height as usize).checked_mul(planes).and_then(|rows| rows.checked_mul(bytes_per_line)).ok_or(Error::TooLarge)?;
	let compressed_end = if planes == 1 {
		let palette_start = data.len().checked_sub(PALETTE_LEN).ok_or(Error::Truncated)?;
		if data[palette_start] != 0x0c {
			return Err(Error::Invalid);
		}
		palette_start
	} else {
		data.len()
	};
	let decoded = decode_rle(data.get(HEADER_LEN..compressed_end).ok_or(Error::Truncated)?, expected)?;
	let mut pixels = Vec::new();
	pixels.try_reserve_exact(width as usize * height as usize * 4).map_err(|_| Error::TooLarge)?;
	for y in 0..height as usize {
		for x in 0..width as usize {
			let (red, green, blue) = if planes == 3 {
				let row = y * planes * bytes_per_line;
				(decoded[row + x], decoded[row + bytes_per_line + x], decoded[row + bytes_per_line * 2 + x])
			} else {
				let index = decoded[y * bytes_per_line + x] as usize;
				let palette = data.len() - PALETTE_LEN + 1 + index * 3;
				(data[palette], data[palette + 1], data[palette + 2])
			};
			pixels.extend_from_slice(&[red, green, blue, 255]);
		}
	}
	pix::RgbaImage::new(width, height, pixels).map_err(map_pix)
}

pub fn encode(image: &pix::RgbaImage) -> Result<Vec<u8>, Error> {
	validate_opaque(image)?;
	if image.width > u16::MAX as u32 || image.height > u16::MAX as u32 {
		return Err(Error::TooLarge);
	}
	let bytes_per_line = (image.width as usize + 1) & !1;
	let mut output = header(image, bytes_per_line)?;
	output[65] = 3;
	for y in 0..image.height as usize {
		for channel in 0..3 {
			let mut row = Vec::new();
			row.try_reserve_exact(bytes_per_line).map_err(|_| Error::TooLarge)?;
			for x in 0..image.width as usize {
				row.push(image.pixels[(y * image.width as usize + x) * 4 + channel]);
			}
			row.resize(bytes_per_line, 0);
			encode_rle(&row, &mut output);
		}
	}
	Ok(output)
}

pub fn encode_indexed(image: &pix::RgbaImage, quality: u8) -> Result<Vec<u8>, Error> {
	validate_opaque(image)?;
	if image.width > u16::MAX as u32 || image.height > u16::MAX as u32 {
		return Err(Error::TooLarge);
	}
	if quality > 100 {
		return Err(Error::Invalid);
	}
	let palette = quantize::build_palette(&[image.as_rgba()], quantize::Options { quality, dither: true, alpha_threshold: 1 }).map_err(map_quantize)?;
	let indices = quantize::map_image(image.as_rgba(), &palette).map_err(map_quantize)?;
	let bytes_per_line = (image.width as usize + 1) & !1;
	let mut output = header(image, bytes_per_line)?;
	output[65] = 1;
	for y in 0..image.height as usize {
		let source = y.checked_mul(image.width as usize).ok_or(Error::TooLarge)?;
		let mut row = Vec::new();
		row.try_reserve_exact(bytes_per_line).map_err(|_| Error::TooLarge)?;
		row.extend_from_slice(&indices[source..source + image.width as usize]);
		row.resize(bytes_per_line, 0);
		encode_rle(&row, &mut output);
	}
	output.try_reserve_exact(PALETTE_LEN).map_err(|_| Error::TooLarge)?;
	output.push(0x0c);
	for index in 0..256 {
		let color = palette.colors.get(index).copied().unwrap_or([0, 0, 0, 255]);
		output.extend_from_slice(&color[..3]);
	}
	Ok(output)
}

fn header(image: &pix::RgbaImage, bytes_per_line: usize) -> Result<Vec<u8>, Error> {
	let bytes_per_line = u16::try_from(bytes_per_line).map_err(|_| Error::TooLarge)?;
	let mut output = alloc::vec![0u8; HEADER_LEN];
	output[0] = 0x0a;
	output[1] = 5;
	output[2] = 1;
	output[3] = 8;
	output[8..10].copy_from_slice(&(image.width as u16 - 1).to_le_bytes());
	output[10..12].copy_from_slice(&(image.height as u16 - 1).to_le_bytes());
	output[12..14].copy_from_slice(&72u16.to_le_bytes());
	output[14..16].copy_from_slice(&72u16.to_le_bytes());
	output[66..68].copy_from_slice(&bytes_per_line.to_le_bytes());
	output[68..70].copy_from_slice(&1u16.to_le_bytes());
	Ok(output)
}

fn validate_opaque(image: &pix::RgbaImage) -> Result<(), Error> {
	let row_bytes = image.width.checked_mul(4).ok_or(Error::TooLarge)?;
	let expected = (image.pitch as usize).checked_mul(image.height as usize).ok_or(Error::TooLarge)?;
	if image.width == 0 || image.height == 0 || image.pitch != row_bytes || image.pixels.len() != expected {
		return Err(Error::Invalid);
	}
	if image.pixels.chunks_exact(4).any(|pixel| pixel[3] != 255) {
		return Err(Error::Unsupported);
	}
	Ok(())
}

fn decode_rle(input: &[u8], expected: usize) -> Result<Vec<u8>, Error> {
	let mut output = Vec::new();
	output.try_reserve_exact(expected).map_err(|_| Error::TooLarge)?;
	let mut cursor = 0usize;
	while output.len() < expected {
		let byte = *input.get(cursor).ok_or(Error::Truncated)?;
		cursor += 1;
		let (length, value) = if byte & 0xc0 == 0xc0 {
			let length = (byte & 0x3f) as usize;
			if length == 0 {
				return Err(Error::Invalid);
			}
			let value = *input.get(cursor).ok_or(Error::Truncated)?;
			cursor += 1;
			(length, value)
		} else {
			(1, byte)
		};
		if output.len().checked_add(length).filter(|length| *length <= expected).is_none() {
			return Err(Error::Invalid);
		}
		output.extend(core::iter::repeat_n(value, length));
	}
	if cursor != input.len() {
		return Err(Error::Invalid);
	}
	Ok(output)
}

fn encode_rle(input: &[u8], output: &mut Vec<u8>) {
	let mut index = 0usize;
	while index < input.len() {
		let mut length = 1usize;
		while length < 63 && index + length < input.len() && input[index + length] == input[index] {
			length += 1;
		}
		let value = input[index];
		if length > 1 || value & 0xc0 == 0xc0 {
			output.extend_from_slice(&[0xc0 | length as u8, value]);
		} else {
			output.push(value);
		}
		index += length;
	}
}

fn map_pix(error: pix::Error) -> Error {
	match error {
		pix::Error::Invalid => Error::Invalid,
		pix::Error::TooLarge => Error::TooLarge,
	}
}

fn map_quantize(error: quantize::Error) -> Error {
	match error {
		quantize::Error::Invalid => Error::Invalid,
		quantize::Error::TooLarge => Error::TooLarge,
	}
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
	use super::*;
	use alloc::vec;

	#[test]
	fn rgb_round_trips_odd_width_and_escaped_bytes() {
		let image = pix::RgbaImage::new(3, 2, vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 192, 193, 194, 255, 1, 2, 3, 255, 4, 5, 6, 255]).unwrap();
		assert_eq!(decode(&encode(&image).unwrap()).unwrap(), image);
	}

	#[test]
	fn indexed_round_trips_exact_colors_and_honors_quality_budget() {
		let exact = pix::RgbaImage::new(3, 1, vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255]).unwrap();
		let encoded = encode_indexed(&exact, 100).unwrap();
		assert_eq!(encoded[65], 1);
		assert_eq!(encoded[encoded.len() - PALETTE_LEN], 0x0c);
		assert_eq!(decode(&encoded).unwrap(), exact);

		let mut pixels = Vec::new();
		for value in 0..512u32 {
			pixels.extend_from_slice(&[value as u8, (value >> 1) as u8, value.wrapping_mul(47) as u8, 255]);
		}
		let true_color = pix::RgbaImage::new(512, 1, pixels).unwrap();
		let low = encode_indexed(&true_color, 0).unwrap();
		let high = encode_indexed(&true_color, 100).unwrap();
		assert_ne!(&low[low.len() - 768..], &high[high.len() - 768..]);
		assert_eq!(decode(&low).unwrap().width, true_color.width);
	}

	#[test]
	fn rejects_truncation_and_unsupported_depth() {
		assert_eq!(decode(&[]), Err(Error::Truncated));
		let mut header = [0u8; HEADER_LEN];
		header[0] = 0x0a;
		header[2] = 1;
		header[3] = 4;
		assert_eq!(decode(&header), Err(Error::Unsupported));
	}
}
