#![no_std]

extern crate alloc;

mod inflate;

use alloc::vec;
use alloc::vec::Vec;

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const MAX_DIMENSION: u32 = 16_384;
const MAX_PIXELS: u64 = 16_777_216;
const PASS_X: [u32; 7] = [0, 4, 0, 2, 0, 1, 0];
const PASS_Y: [u32; 7] = [0, 0, 4, 0, 2, 0, 1];
const PASS_DX: [u32; 7] = [8, 8, 4, 4, 2, 2, 1];
const PASS_DY: [u32; 7] = [8, 8, 8, 4, 4, 2, 2];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Truncated,
	Invalid,
	Unsupported,
	TooLarge,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Image {
	pub width: u32,
	pub height: u32,
	pub pitch: u32,
	pub pixels: Vec<u8>,
}

struct Parsed {
	width: u32,
	height: u32,
	bit_depth: u8,
	color_type: u8,
	interlace: u8,
	palette: Vec<[u8; 3]>,
	transparency: Transparency,
	compressed: Vec<u8>,
}

enum Transparency {
	None,
	Gray(u16),
	Rgb([u16; 3]),
	Palette(Vec<u8>),
}

pub fn decode(data: &[u8]) -> Result<Image, Error> {
	let png = parse(data)?;
	let expected = filtered_len(&png)?;
	let filtered = inflate::zlib(&png.compressed, expected)?;
	let pitch = png.width.checked_mul(4).ok_or(Error::TooLarge)?;
	let output_len = (pitch as usize).checked_mul(png.height as usize).ok_or(Error::TooLarge)?;
	let mut pixels = zeroed(output_len)?;
	decode_passes(&png, &filtered, &mut pixels)?;
	Ok(Image { width: png.width, height: png.height, pitch, pixels })
}

fn zeroed(len: usize) -> Result<Vec<u8>, Error> {
	let mut output = Vec::new();
	output.try_reserve_exact(len).map_err(|_| Error::TooLarge)?;
	output.resize(len, 0);
	Ok(output)
}

fn parse(data: &[u8]) -> Result<Parsed, Error> {
	if data.get(..SIGNATURE.len()) != Some(SIGNATURE) {
		return Err(if data.len() < SIGNATURE.len() { Error::Truncated } else { Error::Invalid });
	}
	let mut cursor = SIGNATURE.len();
	let mut header: Option<(u32, u32, u8, u8, u8)> = None;
	let mut palette = Vec::new();
	let mut transparency = Transparency::None;
	let mut compressed = Vec::new();
	let mut seen_idat = false;
	let mut idat_ended = false;
	let mut seen_iend = false;
	while cursor < data.len() {
		let length = read_u32(data, cursor)? as usize;
		let kind = data.get(cursor + 4..cursor + 8).ok_or(Error::Truncated)?;
		let body_start = cursor.checked_add(8).ok_or(Error::Invalid)?;
		let body_end = body_start.checked_add(length).ok_or(Error::Invalid)?;
		let crc_end = body_end.checked_add(4).ok_or(Error::Invalid)?;
		let body = data.get(body_start..body_end).ok_or(Error::Truncated)?;
		let stored_crc = read_u32(data, body_end)?;
		if crc32(kind.iter().chain(body.iter()).copied()) != stored_crc {
			return Err(Error::Invalid);
		}
		if seen_idat && kind != b"IDAT" {
			idat_ended = true;
		}
		match kind {
			b"IHDR" => {
				if header.is_some() || cursor != SIGNATURE.len() || body.len() != 13 {
					return Err(Error::Invalid);
				}
				let width = read_u32(body, 0)?;
				let height = read_u32(body, 4)?;
				let bit_depth = body[8];
				let color_type = body[9];
				let interlace = body[12];
				if body[10] != 0 || body[11] != 0 || interlace > 1 {
					return Err(Error::Unsupported);
				}
				validate_header(width, height, bit_depth, color_type)?;
				header = Some((width, height, bit_depth, color_type, interlace));
			}
			b"PLTE" => {
				if header.is_none() || seen_idat || !palette.is_empty() || body.is_empty() || body.len() % 3 != 0 || body.len() / 3 > 256 {
					return Err(Error::Invalid);
				}
				palette.extend(body.chunks_exact(3).map(|entry| [entry[0], entry[1], entry[2]]));
			}
			b"tRNS" => {
				let (_, _, bit_depth, color_type, _) = header.ok_or(Error::Invalid)?;
				if seen_idat || !matches!(transparency, Transparency::None) {
					return Err(Error::Invalid);
				}
				transparency = match color_type {
					0 if body.len() == 2 => Transparency::Gray(u16::from_be_bytes([body[0], body[1]])),
					2 if body.len() == 6 => Transparency::Rgb([u16::from_be_bytes([body[0], body[1]]), u16::from_be_bytes([body[2], body[3]]), u16::from_be_bytes([body[4], body[5]])]),
					3 if !palette.is_empty() && body.len() <= palette.len() => Transparency::Palette(body.to_vec()),
					_ => return Err(Error::Invalid),
				};
				if bit_depth < 16 {
					let maximum = (1u16 << bit_depth) - 1;
					match &transparency {
						Transparency::Gray(value) if *value > maximum => return Err(Error::Invalid),
						Transparency::Rgb(values) if values.iter().any(|value| *value > maximum) => return Err(Error::Invalid),
						_ => {}
					}
				}
			}
			b"IDAT" => {
				if header.is_none() || idat_ended {
					return Err(Error::Invalid);
				}
				let (width, height, bit_depth, color_type, interlace) = header.ok_or(Error::Invalid)?;
				let compressed_limit = filtered_len_for(width, height, bit_depth, color_type, interlace)?.checked_mul(2).and_then(|size| size.checked_add(1_048_576)).ok_or(Error::TooLarge)?;
				if compressed.len().checked_add(body.len()).filter(|size| *size <= compressed_limit).is_none() {
					return Err(Error::TooLarge);
				}
				compressed.try_reserve(body.len()).map_err(|_| Error::TooLarge)?;
				compressed.extend_from_slice(body);
				seen_idat = true;
			}
			b"IEND" => {
				if body.len() != 0 || !seen_idat {
					return Err(Error::Invalid);
				}
				seen_iend = true;
				cursor = crc_end;
				break;
			}
			_ if kind[0] & 0x20 == 0 => return Err(Error::Unsupported),
			_ => {}
		}
		cursor = crc_end;
	}
	if !seen_iend || cursor != data.len() {
		return Err(Error::Invalid);
	}
	let (width, height, bit_depth, color_type, interlace) = header.ok_or(Error::Invalid)?;
	if color_type == 3 && (palette.is_empty() || palette.len() > 1usize << bit_depth) {
		return Err(Error::Invalid);
	}
	Ok(Parsed { width, height, bit_depth, color_type, interlace, palette, transparency, compressed })
}

fn validate_header(width: u32, height: u32, bit_depth: u8, color_type: u8) -> Result<(), Error> {
	if width == 0 || height == 0 {
		return Err(Error::Invalid);
	}
	if width > MAX_DIMENSION || height > MAX_DIMENSION || width as u64 * height as u64 > MAX_PIXELS {
		return Err(Error::TooLarge);
	}
	let valid = match color_type {
		0 => matches!(bit_depth, 1 | 2 | 4 | 8 | 16),
		2 => matches!(bit_depth, 8 | 16),
		3 => matches!(bit_depth, 1 | 2 | 4 | 8),
		4 | 6 => matches!(bit_depth, 8 | 16),
		_ => false,
	};
	if valid { Ok(()) } else { Err(Error::Unsupported) }
}

fn channels(color_type: u8) -> Result<usize, Error> {
	match color_type {
		0 | 3 => Ok(1),
		2 => Ok(3),
		4 => Ok(2),
		6 => Ok(4),
		_ => Err(Error::Unsupported),
	}
}

fn filtered_len(png: &Parsed) -> Result<usize, Error> {
	filtered_len_for(png.width, png.height, png.bit_depth, png.color_type, png.interlace)
}

fn filtered_len_for(width: u32, height: u32, bit_depth: u8, color_type: u8, interlace: u8) -> Result<usize, Error> {
	let passes = if interlace == 0 { 1 } else { 7 };
	let bits_per_pixel = channels(color_type)?.checked_mul(bit_depth as usize).ok_or(Error::TooLarge)?;
	let mut total = 0usize;
	for pass in 0..passes {
		let (pass_width, pass_height) = pass_size(width, height, interlace, pass);
		if pass_width == 0 || pass_height == 0 {
			continue;
		}
		let row_bytes = (pass_width as usize).checked_mul(bits_per_pixel).ok_or(Error::TooLarge)?.div_ceil(8);
		total = total.checked_add(row_bytes.checked_add(1).and_then(|row| row.checked_mul(pass_height as usize)).ok_or(Error::TooLarge)?).ok_or(Error::TooLarge)?;
	}
	Ok(total)
}

fn pass_size(width: u32, height: u32, interlace: u8, pass: usize) -> (u32, u32) {
	if interlace == 0 {
		return (width, height);
	}
	let pass_width = if width <= PASS_X[pass] { 0 } else { (width - PASS_X[pass]).div_ceil(PASS_DX[pass]) };
	let pass_height = if height <= PASS_Y[pass] { 0 } else { (height - PASS_Y[pass]).div_ceil(PASS_DY[pass]) };
	(pass_width, pass_height)
}

fn decode_passes(png: &Parsed, filtered: &[u8], output: &mut [u8]) -> Result<(), Error> {
	let passes = if png.interlace == 0 { 1 } else { 7 };
	let channel_count = channels(png.color_type)?;
	let bits_per_pixel = channel_count.checked_mul(png.bit_depth as usize).ok_or(Error::TooLarge)?;
	let filter_bpp = bits_per_pixel.div_ceil(8).max(1);
	let mut cursor = 0usize;
	for pass in 0..passes {
		let (width, height) = pass_size(png.width, png.height, png.interlace, pass);
		if width == 0 || height == 0 {
			continue;
		}
		let row_bytes = (width as usize).checked_mul(bits_per_pixel).ok_or(Error::TooLarge)?.div_ceil(8);
		let mut previous = vec![0u8; row_bytes];
		let mut current = vec![0u8; row_bytes];
		for y in 0..height {
			let filter = *filtered.get(cursor).ok_or(Error::Truncated)?;
			cursor += 1;
			let end = cursor.checked_add(row_bytes).ok_or(Error::Invalid)?;
			current.copy_from_slice(filtered.get(cursor..end).ok_or(Error::Truncated)?);
			cursor = end;
			unfilter(filter, &mut current, &previous, filter_bpp)?;
			for x in 0..width {
				let color = pixel_color(png, &current, x as usize, channel_count)?;
				let output_x = if png.interlace == 0 { x } else { PASS_X[pass] + x * PASS_DX[pass] };
				let output_y = if png.interlace == 0 { y } else { PASS_Y[pass] + y * PASS_DY[pass] };
				write_pixel(output, png.width, output_x, output_y, color)?;
			}
			core::mem::swap(&mut current, &mut previous);
		}
	}
	if cursor != filtered.len() {
		return Err(Error::Invalid);
	}
	Ok(())
}

fn unfilter(filter: u8, current: &mut [u8], previous: &[u8], bpp: usize) -> Result<(), Error> {
	for index in 0..current.len() {
		let left = if index >= bpp { current[index - bpp] } else { 0 };
		let above = previous[index];
		let upper_left = if index >= bpp { previous[index - bpp] } else { 0 };
		current[index] = match filter {
			0 => current[index],
			1 => current[index].wrapping_add(left),
			2 => current[index].wrapping_add(above),
			3 => current[index].wrapping_add(((left as u16 + above as u16) / 2) as u8),
			4 => current[index].wrapping_add(paeth(left, above, upper_left)),
			_ => return Err(Error::Invalid),
		};
	}
	Ok(())
}

fn paeth(left: u8, above: u8, upper_left: u8) -> u8 {
	let prediction = left as i32 + above as i32 - upper_left as i32;
	let left_distance = (prediction - left as i32).abs();
	let above_distance = (prediction - above as i32).abs();
	let upper_left_distance = (prediction - upper_left as i32).abs();
	if left_distance <= above_distance && left_distance <= upper_left_distance {
		left
	} else if above_distance <= upper_left_distance {
		above
	} else {
		upper_left
	}
}

fn pixel_color(png: &Parsed, row: &[u8], x: usize, channel_count: usize) -> Result<u32, Error> {
	let base = x.checked_mul(channel_count).ok_or(Error::Invalid)?;
	let sample = |channel: usize| read_sample(row, png.bit_depth, base + channel);
	let (red, green, blue, alpha) = match png.color_type {
		0 => {
			let raw = sample(0)?;
			let gray = scale_sample(raw, png.bit_depth);
			let alpha = if matches!(png.transparency, Transparency::Gray(value) if value == raw) { 0 } else { 255 };
			(gray, gray, gray, alpha)
		}
		2 => {
			let raw = [sample(0)?, sample(1)?, sample(2)?];
			let alpha = if matches!(png.transparency, Transparency::Rgb(value) if value == raw) { 0 } else { 255 };
			(scale_sample(raw[0], png.bit_depth), scale_sample(raw[1], png.bit_depth), scale_sample(raw[2], png.bit_depth), alpha)
		}
		3 => {
			let index = sample(0)? as usize;
			let color = png.palette.get(index).ok_or(Error::Invalid)?;
			let alpha = match &png.transparency {
				Transparency::Palette(values) => values.get(index).copied().unwrap_or(255),
				_ => 255,
			};
			(color[0], color[1], color[2], alpha)
		}
		4 => {
			let gray = scale_sample(sample(0)?, png.bit_depth);
			(gray, gray, gray, scale_sample(sample(1)?, png.bit_depth))
		}
		6 => (scale_sample(sample(0)?, png.bit_depth), scale_sample(sample(1)?, png.bit_depth), scale_sample(sample(2)?, png.bit_depth), scale_sample(sample(3)?, png.bit_depth)),
		_ => return Err(Error::Unsupported),
	};
	let blend = |value: u8| (value as u16 * alpha as u16 / 255) as u32;
	Ok(blend(red) << 16 | blend(green) << 8 | blend(blue))
}

fn read_sample(row: &[u8], bit_depth: u8, index: usize) -> Result<u16, Error> {
	match bit_depth {
		1 | 2 | 4 => {
			let bit = index.checked_mul(bit_depth as usize).ok_or(Error::Invalid)?;
			let byte = *row.get(bit / 8).ok_or(Error::Truncated)?;
			let shift = 8 - bit_depth as usize - bit % 8;
			Ok(((byte >> shift) & ((1 << bit_depth) - 1)) as u16)
		}
		8 => row.get(index).copied().map(u16::from).ok_or(Error::Truncated),
		16 => {
			let offset = index.checked_mul(2).ok_or(Error::Invalid)?;
			let bytes = row.get(offset..offset + 2).ok_or(Error::Truncated)?;
			Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
		}
		_ => Err(Error::Unsupported),
	}
}

fn scale_sample(value: u16, bit_depth: u8) -> u8 {
	let maximum = if bit_depth == 16 { u16::MAX as u32 } else { (1u32 << bit_depth) - 1 };
	(value as u32 * 255 / maximum) as u8
}

fn write_pixel(output: &mut [u8], width: u32, x: u32, y: u32, color: u32) -> Result<(), Error> {
	let offset = (y as usize).checked_mul(width as usize).and_then(|row| row.checked_add(x as usize)).and_then(|pixel| pixel.checked_mul(4)).ok_or(Error::Invalid)?;
	output.get_mut(offset..offset + 4).ok_or(Error::Invalid)?.copy_from_slice(&color.to_le_bytes());
	Ok(())
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, Error> {
	Ok(u32::from_be_bytes(data.get(offset..offset + 4).ok_or(Error::Truncated)?.try_into().map_err(|_| Error::Truncated)?))
}

fn crc32(bytes: impl Iterator<Item = u8>) -> u32 {
	let mut crc = u32::MAX;
	for byte in bytes {
		crc ^= byte as u32;
		for _ in 0..8 {
			crc = if crc & 1 != 0 { crc >> 1 ^ 0xedb8_8320 } else { crc >> 1 };
		}
	}
	!crc
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
	use super::*;

	fn adler32(data: &[u8]) -> u32 {
		let mut a = 1u32;
		let mut b = 0u32;
		for &byte in data {
			a = (a + byte as u32) % 65_521;
			b = (b + a) % 65_521;
		}
		b << 16 | a
	}

	fn zlib_stored(data: &[u8]) -> Vec<u8> {
		assert!(data.len() <= u16::MAX as usize);
		let mut out = vec![0x78, 0x01, 0x01];
		let len = data.len() as u16;
		out.extend_from_slice(&len.to_le_bytes());
		out.extend_from_slice(&(!len).to_le_bytes());
		out.extend_from_slice(data);
		out.extend_from_slice(&adler32(data).to_be_bytes());
		out
	}

	fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
		out.extend_from_slice(&(body.len() as u32).to_be_bytes());
		out.extend_from_slice(kind);
		out.extend_from_slice(body);
		out.extend_from_slice(&crc32(kind.iter().chain(body.iter()).copied()).to_be_bytes());
	}

	fn png(width: u32, height: u32, depth: u8, color_type: u8, interlace: u8, palette: &[[u8; 3]], transparency: &[u8], filtered: &[u8]) -> Vec<u8> {
		let mut out = SIGNATURE.to_vec();
		let mut header = Vec::new();
		header.extend_from_slice(&width.to_be_bytes());
		header.extend_from_slice(&height.to_be_bytes());
		header.extend_from_slice(&[depth, color_type, 0, 0, interlace]);
		chunk(&mut out, b"IHDR", &header);
		if !palette.is_empty() {
			let entries: Vec<u8> = palette.iter().flatten().copied().collect();
			chunk(&mut out, b"PLTE", &entries);
		}
		if !transparency.is_empty() {
			chunk(&mut out, b"tRNS", transparency);
		}
		chunk(&mut out, b"IDAT", &zlib_stored(filtered));
		chunk(&mut out, b"IEND", &[]);
		out
	}

	fn colors(image: &Image) -> Vec<u32> {
		image.pixels.chunks_exact(4).map(|pixel| u32::from_le_bytes(pixel.try_into().unwrap())).collect()
	}

	fn adam7_rgb(width: u32, height: u32, pixels: &[[u8; 3]]) -> Vec<u8> {
		let mut out = Vec::new();
		for pass in 0..7 {
			let (pass_width, pass_height) = pass_size(width, height, 1, pass);
			if pass_width == 0 || pass_height == 0 {
				continue;
			}
			for y in 0..pass_height {
				out.push(0);
				for x in 0..pass_width {
					let source_x = PASS_X[pass] + x * PASS_DX[pass];
					let source_y = PASS_Y[pass] + y * PASS_DY[pass];
					out.extend_from_slice(&pixels[(source_y * width + source_x) as usize]);
				}
			}
		}
		out
	}

	#[test]
	fn decodes_rgb_and_adam7_images() {
		let raw = [0, 255, 0, 0, 0, 255, 0, 0, 0, 0, 255, 255, 255, 255];
		let image = decode(&png(2, 2, 8, 2, 0, &[], &[], &raw)).unwrap();
		assert_eq!(colors(&image), vec![0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0x00ff_ffff]);

		let source = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0], [0, 255, 255], [255, 0, 255], [32, 64, 96], [96, 64, 32], [255, 255, 255]];
		let adam7 = adam7_rgb(3, 3, &source);
		let encoded = png(3, 3, 8, 2, 1, &[], &[], &adam7);
		let parsed = parse(&encoded).unwrap();
		let filtered_size = filtered_len(&parsed).unwrap();
		assert_eq!(filtered_size, adam7.len());
		let filtered = inflate::zlib(&parsed.compressed, filtered_size).unwrap();
		let mut pass_output = vec![0; 3 * 3 * 4];
		decode_passes(&parsed, &filtered, &mut pass_output).unwrap();
		let interlaced = decode(&encoded).unwrap();
		let expected: Vec<u32> = source.iter().map(|rgb| (rgb[0] as u32) << 16 | (rgb[1] as u32) << 8 | rgb[2] as u32).collect();
		assert_eq!(colors(&interlaced), expected);
	}

	#[test]
	fn decodes_indexed_transparency_and_all_row_filters() {
		let indexed = png(2, 1, 1, 3, 0, &[[255, 0, 0], [0, 255, 0]], &[255, 0], &[0, 0b0100_0000]);
		assert_eq!(colors(&decode(&indexed).unwrap()), vec![0x00ff_0000, 0]);

		let previous = [10, 20, 30, 40, 50, 60];
		for filter in 0..=4 {
			let mut encoded = [1, 2, 3, 4, 5, 6];
			let mut decoded = encoded;
			unfilter(filter, &mut decoded, &previous, 3).unwrap();
			if filter == 0 {
				assert_eq!(decoded, encoded);
			} else {
				assert_ne!(decoded, encoded);
			}
			encoded.fill(0);
		}
	}

	#[test]
	fn rejects_crc_truncation_and_oversized_dimensions() {
		let mut valid = png(1, 1, 8, 6, 0, &[], &[], &[0, 1, 2, 3, 255]);
		let last = valid.len() - 1;
		valid[last] ^= 1;
		assert_eq!(decode(&valid), Err(Error::Invalid));
		assert_eq!(decode(&SIGNATURE[..4]), Err(Error::Truncated));
		let oversized = png(20_000, 1, 8, 2, 0, &[], &[], &[]);
		assert_eq!(decode(&oversized), Err(Error::TooLarge));
	}

	#[test]
	fn staged_sample_image_is_a_valid_two_by_two_png() {
		let image = decode(include_bytes!("../../../volume/sample.png")).unwrap();
		assert_eq!((image.width, image.height, image.pitch), (2, 2, 8));
		assert_eq!(image.pixels.len(), 16);
		assert!(image.pixels.iter().any(|byte| *byte != 0));
	}
}
