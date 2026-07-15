#![no_std]

extern crate alloc;

use alloc::vec::Vec;

const FILE_HEADER_LEN: usize = 14;
const CORE_HEADER_LEN: usize = 12;
const INFO_HEADER_LEN: usize = 40;
const BI_RGB: u32 = 0;
const BI_RLE8: u32 = 1;
const BI_RLE4: u32 = 2;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHA_BITFIELDS: u32 = 6;
const MAX_DIMENSION: u32 = 16_384;
const MAX_PIXELS: u64 = 16_777_216;

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

impl Image {
	pub fn as_pix(&self) -> pix::Image<'_> {
		pix::Image { data: &self.pixels, width: self.width, height: self.height, pitch: self.pitch }
	}
}

#[derive(Clone, Copy)]
struct Header {
	width: u32,
	height: u32,
	top_down: bool,
	bits_per_pixel: u16,
	compression: u32,
	pixel_offset: usize,
	pixel_limit: usize,
	palette_offset: usize,
	palette_entries: usize,
	palette_entry_len: usize,
	masks: Option<[u32; 3]>,
}

pub fn decode(data: &[u8]) -> Result<Image, Error> {
	let header = parse_header(data)?;
	let pitch = header.width.checked_mul(4).ok_or(Error::TooLarge)?;
	let output_len = (pitch as usize).checked_mul(header.height as usize).ok_or(Error::TooLarge)?;
	let mut pixels = zeroed(output_len)?;
	let palette = read_palette(data, &header)?;
	match header.compression {
		BI_RGB | BI_BITFIELDS | BI_ALPHA_BITFIELDS => decode_rows(data, &header, &palette, &mut pixels)?,
		BI_RLE8 => decode_rle(data, &header, &palette, &mut pixels, false)?,
		BI_RLE4 => decode_rle(data, &header, &palette, &mut pixels, true)?,
		_ => return Err(Error::Unsupported),
	}
	Ok(Image { width: header.width, height: header.height, pitch, pixels })
}

pub fn decode_rgba(data: &[u8]) -> Result<pix::RgbaImage, Error> {
	let image = decode(data)?;
	let mut pixels = Vec::new();
	pixels.try_reserve_exact(image.pixels.len()).map_err(|_| Error::TooLarge)?;
	for pixel in image.pixels.chunks_exact(4) {
		pixels.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
	}
	pix::RgbaImage::new(image.width, image.height, pixels).map_err(|error| match error {
		pix::Error::Invalid => Error::Invalid,
		pix::Error::TooLarge => Error::TooLarge,
	})
}

pub fn encode_rgba(image: &pix::RgbaImage) -> Result<Vec<u8>, Error> {
	let source_row = usize::try_from(image.width).ok().and_then(|width| width.checked_mul(4)).ok_or(Error::TooLarge)?;
	let expected = source_row.checked_mul(image.height as usize).ok_or(Error::TooLarge)?;
	if image.width == 0 || image.height == 0 || image.width > MAX_DIMENSION || image.height > MAX_DIMENSION || image.width as u64 * image.height as u64 > MAX_PIXELS || image.pitch as usize != source_row || image.pixels.len() != expected {
		return Err(Error::Invalid);
	}
	if image.pixels.chunks_exact(4).any(|pixel| pixel[3] != 255) {
		return Err(Error::Unsupported);
	}
	let row_stride = usize::try_from(image.width).ok().and_then(|width| width.checked_mul(3)).and_then(|bytes| bytes.checked_add(3)).map(|bytes| bytes & !3).ok_or(Error::TooLarge)?;
	let pixel_len = row_stride.checked_mul(image.height as usize).ok_or(Error::TooLarge)?;
	let file_len = FILE_HEADER_LEN.checked_add(INFO_HEADER_LEN).and_then(|header| header.checked_add(pixel_len)).ok_or(Error::TooLarge)?;
	let mut output = Vec::new();
	output.try_reserve_exact(file_len).map_err(|_| Error::TooLarge)?;
	output.resize(file_len, 0);
	output[..2].copy_from_slice(b"BM");
	output[2..6].copy_from_slice(&u32::try_from(file_len).map_err(|_| Error::TooLarge)?.to_le_bytes());
	let pixel_offset = FILE_HEADER_LEN + INFO_HEADER_LEN;
	output[10..14].copy_from_slice(&(pixel_offset as u32).to_le_bytes());
	output[14..18].copy_from_slice(&(INFO_HEADER_LEN as u32).to_le_bytes());
	output[18..22].copy_from_slice(&(image.width as i32).to_le_bytes());
	output[22..26].copy_from_slice(&(image.height as i32).to_le_bytes());
	output[26..28].copy_from_slice(&1u16.to_le_bytes());
	output[28..30].copy_from_slice(&24u16.to_le_bytes());
	output[34..38].copy_from_slice(&u32::try_from(pixel_len).map_err(|_| Error::TooLarge)?.to_le_bytes());
	for file_y in 0..image.height as usize {
		let source_y = image.height as usize - 1 - file_y;
		let source = &image.pixels[source_y * source_row..(source_y + 1) * source_row];
		let target = pixel_offset + file_y * row_stride;
		for x in 0..image.width as usize {
			let source = &source[x * 4..x * 4 + 4];
			output[target + x * 3..target + x * 3 + 3].copy_from_slice(&[source[2], source[1], source[0]]);
		}
	}
	Ok(output)
}

fn zeroed(len: usize) -> Result<Vec<u8>, Error> {
	let mut output = Vec::new();
	output.try_reserve_exact(len).map_err(|_| Error::TooLarge)?;
	output.resize(len, 0);
	Ok(output)
}

fn parse_header(data: &[u8]) -> Result<Header, Error> {
	if data.get(..2) != Some(b"BM") {
		return Err(if data.len() < 2 { Error::Truncated } else { Error::Invalid });
	}
	let declared_size = read_u32(data, 2)? as usize;
	let pixel_offset = read_u32(data, 10)? as usize;
	let dib_size = read_u32(data, FILE_HEADER_LEN)? as usize;
	let header_end = FILE_HEADER_LEN.checked_add(dib_size).ok_or(Error::Invalid)?;
	if dib_size < CORE_HEADER_LEN || header_end > data.len() {
		return Err(Error::Truncated);
	}
	let pixel_limit = if declared_size == 0 {
		data.len()
	} else {
		if declared_size > data.len() {
			return Err(Error::Truncated);
		}
		declared_size
	};
	if pixel_offset < header_end || pixel_offset > pixel_limit {
		return Err(Error::Invalid);
	}

	let (width, height, top_down, bits_per_pixel, compression, colors_used, palette_entry_len) = if dib_size == CORE_HEADER_LEN {
		let width = read_u16(data, 18)? as u32;
		let height = read_u16(data, 20)? as u32;
		let planes = read_u16(data, 22)?;
		let bits_per_pixel = read_u16(data, 24)?;
		if planes != 1 {
			return Err(Error::Invalid);
		}
		(width, height, false, bits_per_pixel, BI_RGB, 0, 3)
	} else if dib_size >= INFO_HEADER_LEN {
		let width_raw = read_i32(data, 18)?;
		let height_raw = read_i32(data, 22)?;
		let planes = read_u16(data, 26)?;
		let bits_per_pixel = read_u16(data, 28)?;
		let compression = read_u32(data, 30)?;
		let colors_used = read_u32(data, 46)?;
		if width_raw <= 0 || height_raw == 0 || height_raw == i32::MIN || planes != 1 {
			return Err(Error::Invalid);
		}
		(width_raw as u32, height_raw.unsigned_abs(), height_raw < 0, bits_per_pixel, compression, colors_used, 4)
	} else {
		return Err(Error::Unsupported);
	};
	validate_geometry(width, height)?;
	validate_format(bits_per_pixel, compression, top_down)?;

	let external_mask_len = if dib_size >= 52 || !matches!(compression, BI_BITFIELDS | BI_ALPHA_BITFIELDS) {
		0
	} else if compression == BI_ALPHA_BITFIELDS {
		16
	} else {
		12
	};
	let palette_offset = header_end.checked_add(external_mask_len).ok_or(Error::Invalid)?;
	if palette_offset > pixel_offset {
		return Err(Error::Invalid);
	}
	let masks = if matches!(compression, BI_BITFIELDS | BI_ALPHA_BITFIELDS) {
		let mask_offset = if dib_size >= 52 { FILE_HEADER_LEN + 40 } else { header_end };
		let masks = [read_u32(data, mask_offset)?, read_u32(data, mask_offset + 4)?, read_u32(data, mask_offset + 8)?];
		validate_masks(masks)?;
		Some(masks)
	} else if bits_per_pixel == 16 {
		Some([0x7c00, 0x03e0, 0x001f])
	} else {
		None
	};
	let palette_entries = if bits_per_pixel <= 8 {
		let maximum = 1usize << bits_per_pixel;
		let entries = if colors_used == 0 { maximum } else { colors_used as usize };
		if entries == 0 || entries > maximum {
			return Err(Error::Invalid);
		}
		entries
	} else {
		0
	};
	let palette_len = palette_entries.checked_mul(palette_entry_len).ok_or(Error::Invalid)?;
	if palette_offset.checked_add(palette_len).ok_or(Error::Invalid)? > pixel_offset {
		return Err(Error::Truncated);
	}
	Ok(Header { width, height, top_down, bits_per_pixel, compression, pixel_offset, pixel_limit, palette_offset, palette_entries, palette_entry_len, masks })
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

fn validate_format(bits_per_pixel: u16, compression: u32, top_down: bool) -> Result<(), Error> {
	let valid = match compression {
		BI_RGB => matches!(bits_per_pixel, 1 | 4 | 8 | 16 | 24 | 32),
		BI_RLE8 => bits_per_pixel == 8 && !top_down,
		BI_RLE4 => bits_per_pixel == 4 && !top_down,
		BI_BITFIELDS | BI_ALPHA_BITFIELDS => matches!(bits_per_pixel, 16 | 32),
		_ => false,
	};
	if valid { Ok(()) } else { Err(Error::Unsupported) }
}

fn validate_masks(masks: [u32; 3]) -> Result<(), Error> {
	if masks.iter().any(|mask| *mask == 0) || masks[0] & masks[1] != 0 || masks[0] & masks[2] != 0 || masks[1] & masks[2] != 0 {
		return Err(Error::Invalid);
	}
	Ok(())
}

fn read_palette(data: &[u8], header: &Header) -> Result<Vec<u32>, Error> {
	let mut palette = Vec::with_capacity(header.palette_entries);
	for index in 0..header.palette_entries {
		let offset = header.palette_offset.checked_add(index.checked_mul(header.palette_entry_len).ok_or(Error::Invalid)?).ok_or(Error::Invalid)?;
		let entry = data.get(offset..offset + header.palette_entry_len).ok_or(Error::Truncated)?;
		palette.push((entry[2] as u32) << 16 | (entry[1] as u32) << 8 | entry[0] as u32);
	}
	Ok(palette)
}

fn decode_rows(data: &[u8], header: &Header, palette: &[u32], output: &mut [u8]) -> Result<(), Error> {
	let row_bits = (header.width as u64).checked_mul(header.bits_per_pixel as u64).ok_or(Error::TooLarge)?;
	let row_stride = row_bits.div_ceil(32).checked_mul(4).ok_or(Error::TooLarge)? as usize;
	let image_len = row_stride.checked_mul(header.height as usize).ok_or(Error::TooLarge)?;
	let end = header.pixel_offset.checked_add(image_len).ok_or(Error::TooLarge)?;
	let source = data.get(header.pixel_offset..end).filter(|_| end <= header.pixel_limit).ok_or(Error::Truncated)?;
	for file_y in 0..header.height {
		let row_start = file_y as usize * row_stride;
		let row = &source[row_start..row_start + row_stride];
		let output_y = if header.top_down { file_y } else { header.height - 1 - file_y };
		for x in 0..header.width {
			let color = match header.bits_per_pixel {
				1 => palette_color(palette, ((row[x as usize / 8] >> (7 - x % 8)) & 1) as usize)?,
				4 => {
					let packed = row[x as usize / 2];
					palette_color(palette, if x % 2 == 0 { (packed >> 4) as usize } else { (packed & 0x0f) as usize })?
				}
				8 => palette_color(palette, row[x as usize] as usize)?,
				16 => {
					let offset = x as usize * 2;
					masked_color(u16::from_le_bytes([row[offset], row[offset + 1]]) as u32, header.masks.ok_or(Error::Invalid)?)
				}
				24 => {
					let offset = x as usize * 3;
					(row[offset + 2] as u32) << 16 | (row[offset + 1] as u32) << 8 | row[offset] as u32
				}
				32 if header.masks.is_some() => {
					let offset = x as usize * 4;
					masked_color(u32::from_le_bytes(row[offset..offset + 4].try_into().map_err(|_| Error::Truncated)?), header.masks.ok_or(Error::Invalid)?)
				}
				32 => {
					let offset = x as usize * 4;
					(row[offset + 2] as u32) << 16 | (row[offset + 1] as u32) << 8 | row[offset] as u32
				}
				_ => return Err(Error::Unsupported),
			};
			write_pixel(output, header.width, x, output_y, color)?;
		}
	}
	Ok(())
}

fn decode_rle(data: &[u8], header: &Header, palette: &[u32], output: &mut [u8], four_bit: bool) -> Result<(), Error> {
	let source = data.get(header.pixel_offset..header.pixel_limit).ok_or(Error::Truncated)?;
	let mut cursor = 0usize;
	let mut x = 0u32;
	let mut file_y = 0u32;
	loop {
		let pair = source.get(cursor..cursor + 2).ok_or(Error::Truncated)?;
		cursor += 2;
		let count = pair[0] as u32;
		let value = pair[1];
		if count != 0 {
			ensure_run(header, x, file_y, count)?;
			for index in 0..count {
				let palette_index = if four_bit { if index % 2 == 0 { value >> 4 } else { value & 0x0f } } else { value };
				write_rle_pixel(output, header, palette, x + index, file_y, palette_index)?;
			}
			x += count;
			continue;
		}
		match value {
			0 => {
				if file_y >= header.height {
					return Err(Error::Invalid);
				}
				x = 0;
				file_y += 1;
			}
			1 => return Ok(()),
			2 => {
				let delta = source.get(cursor..cursor + 2).ok_or(Error::Truncated)?;
				cursor += 2;
				x = x.checked_add(delta[0] as u32).ok_or(Error::Invalid)?;
				file_y = file_y.checked_add(delta[1] as u32).ok_or(Error::Invalid)?;
				if x > header.width || file_y >= header.height {
					return Err(Error::Invalid);
				}
			}
			absolute => {
				let count = absolute as u32;
				ensure_run(header, x, file_y, count)?;
				let encoded_len = if four_bit { count.div_ceil(2) as usize } else { count as usize };
				let padded_len = encoded_len.checked_add(encoded_len & 1).ok_or(Error::Invalid)?;
				let encoded = source.get(cursor..cursor + encoded_len).ok_or(Error::Truncated)?;
				if source.get(cursor..cursor + padded_len).is_none() {
					return Err(Error::Truncated);
				}
				cursor += padded_len;
				for index in 0..count {
					let palette_index = if four_bit {
						let packed = encoded[index as usize / 2];
						if index % 2 == 0 { packed >> 4 } else { packed & 0x0f }
					} else {
						encoded[index as usize]
					};
					write_rle_pixel(output, header, palette, x + index, file_y, palette_index)?;
				}
				x += count;
			}
		}
	}
}

fn ensure_run(header: &Header, x: u32, file_y: u32, count: u32) -> Result<(), Error> {
	if file_y >= header.height || x.checked_add(count).filter(|end| *end <= header.width).is_none() {
		return Err(Error::Invalid);
	}
	Ok(())
}

fn write_rle_pixel(output: &mut [u8], header: &Header, palette: &[u32], x: u32, file_y: u32, index: u8) -> Result<(), Error> {
	let color = palette_color(palette, index as usize)?;
	write_pixel(output, header.width, x, header.height - 1 - file_y, color)
}

fn palette_color(palette: &[u32], index: usize) -> Result<u32, Error> {
	palette.get(index).copied().ok_or(Error::Invalid)
}

fn masked_color(value: u32, masks: [u32; 3]) -> u32 {
	let red = scale_mask(value, masks[0]);
	let green = scale_mask(value, masks[1]);
	let blue = scale_mask(value, masks[2]);
	red << 16 | green << 8 | blue
}

fn scale_mask(value: u32, mask: u32) -> u32 {
	let shift = mask.trailing_zeros();
	let maximum = mask >> shift;
	(((value & mask) >> shift) as u64 * 255 / maximum as u64) as u32
}

fn write_pixel(output: &mut [u8], width: u32, x: u32, y: u32, color: u32) -> Result<(), Error> {
	let offset = (y as usize).checked_mul(width as usize).and_then(|row| row.checked_add(x as usize)).and_then(|pixel| pixel.checked_mul(4)).ok_or(Error::Invalid)?;
	let pixel = output.get_mut(offset..offset + 4).ok_or(Error::Invalid)?;
	pixel.copy_from_slice(&color.to_le_bytes());
	Ok(())
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16, Error> {
	Ok(u16::from_le_bytes(data.get(offset..offset + 2).ok_or(Error::Truncated)?.try_into().map_err(|_| Error::Truncated)?))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, Error> {
	Ok(u32::from_le_bytes(data.get(offset..offset + 4).ok_or(Error::Truncated)?.try_into().map_err(|_| Error::Truncated)?))
}

fn read_i32(data: &[u8], offset: usize) -> Result<i32, Error> {
	Ok(i32::from_le_bytes(data.get(offset..offset + 4).ok_or(Error::Truncated)?.try_into().map_err(|_| Error::Truncated)?))
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
	use super::*;
	use alloc::vec;

	fn info_bmp(width: i32, height: i32, bits: u16, compression: u32, palette: &[[u8; 4]], pixels: &[u8]) -> Vec<u8> {
		let pixel_offset = FILE_HEADER_LEN + INFO_HEADER_LEN + palette.len() * 4;
		let file_len = pixel_offset + pixels.len();
		let mut bmp = vec![0; file_len];
		bmp[..2].copy_from_slice(b"BM");
		bmp[2..6].copy_from_slice(&(file_len as u32).to_le_bytes());
		bmp[10..14].copy_from_slice(&(pixel_offset as u32).to_le_bytes());
		bmp[14..18].copy_from_slice(&(INFO_HEADER_LEN as u32).to_le_bytes());
		bmp[18..22].copy_from_slice(&width.to_le_bytes());
		bmp[22..26].copy_from_slice(&height.to_le_bytes());
		bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
		bmp[28..30].copy_from_slice(&bits.to_le_bytes());
		bmp[30..34].copy_from_slice(&compression.to_le_bytes());
		bmp[34..38].copy_from_slice(&(pixels.len() as u32).to_le_bytes());
		bmp[46..50].copy_from_slice(&(palette.len() as u32).to_le_bytes());
		for (index, entry) in palette.iter().enumerate() {
			let start = FILE_HEADER_LEN + INFO_HEADER_LEN + index * 4;
			bmp[start..start + 4].copy_from_slice(entry);
		}
		bmp[pixel_offset..].copy_from_slice(pixels);
		bmp
	}

	fn colors(image: &Image) -> Vec<u32> {
		image.pixels.chunks_exact(4).map(|pixel| u32::from_le_bytes(pixel.try_into().unwrap())).collect()
	}

	#[test]
	fn decodes_bottom_up_and_top_down_24_bit_rows() {
		let bottom_up = info_bmp(2, 2, 24, BI_RGB, &[], &[0xff, 0, 0, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0xff, 0, 0xff, 0, 0, 0]);
		let top_down = info_bmp(2, -2, 24, BI_RGB, &[], &[0, 0, 0xff, 0, 0xff, 0, 0, 0, 0xff, 0, 0, 0xff, 0xff, 0xff, 0, 0]);
		let expected = vec![0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0x00ff_ffff];
		assert_eq!(colors(&decode(&bottom_up).unwrap()), expected);
		assert_eq!(colors(&decode(&top_down).unwrap()), expected);
		assert_eq!(decode_rgba(&bottom_up).unwrap().pixels, vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]);
	}

	#[test]
	fn decodes_indexed_rows_and_rejects_bad_palette_indices() {
		let palette = [[0, 0, 0, 0], [0xff, 0xff, 0xff, 0]];
		let bmp = info_bmp(4, 1, 1, BI_RGB, &palette, &[0b1010_0000, 0, 0, 0]);
		assert_eq!(colors(&decode(&bmp).unwrap()), vec![0x00ff_ffff, 0, 0x00ff_ffff, 0]);

		let invalid = info_bmp(1, 1, 8, BI_RGB, &palette, &[2, 0, 0, 0]);
		assert_eq!(decode(&invalid), Err(Error::Invalid));
	}

	#[test]
	fn decodes_rle8_encoded_and_absolute_runs() {
		let palette = [[0, 0, 0, 0], [0, 0, 0xff, 0]];
		let encoded = [4, 1, 0, 0, 0, 4, 0, 1, 0, 1, 0, 0, 0, 1];
		let image = decode(&info_bmp(4, 2, 8, BI_RLE8, &palette, &encoded)).unwrap();
		assert_eq!(colors(&image), vec![0, 0x00ff_0000, 0, 0x00ff_0000, 0x00ff_0000, 0x00ff_0000, 0x00ff_0000, 0x00ff_0000]);
	}

	#[test]
	fn decodes_rle4_nibble_runs() {
		let palette = [[0, 0, 0, 0], [0, 0, 0xff, 0], [0, 0xff, 0, 0]];
		let image = decode(&info_bmp(4, 1, 4, BI_RLE4, &palette, &[4, 0x12, 0, 1])).unwrap();
		assert_eq!(colors(&image), vec![0x00ff_0000, 0x0000_ff00, 0x00ff_0000, 0x0000_ff00]);
	}

	#[test]
	fn rejects_truncation_oversized_geometry_and_out_of_bounds_rle() {
		assert_eq!(decode(b"BM"), Err(Error::Truncated));
		let mut too_wide = info_bmp(1, 1, 24, BI_RGB, &[], &[0, 0, 0, 0]);
		too_wide[18..22].copy_from_slice(&20_000i32.to_le_bytes());
		assert_eq!(decode(&too_wide), Err(Error::TooLarge));
		let palette = [[0, 0, 0, 0], [0xff, 0xff, 0xff, 0]];
		let invalid_run = info_bmp(2, 1, 8, BI_RLE8, &palette, &[3, 1, 0, 1]);
		assert_eq!(decode(&invalid_run), Err(Error::Invalid));
	}

	#[test]
	fn staged_sample_image_is_a_valid_two_by_two_bmp() {
		let image = decode(include_bytes!("../../../volume/sample.bmp")).unwrap();
		assert_eq!((image.width, image.height, image.pitch), (2, 2, 8));
		assert_eq!(image.pixels.len(), 16);
	}

	#[test]
	fn encodes_opaque_rgba_and_refuses_alpha_loss() {
		let image = pix::RgbaImage::new(2, 2, vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 1, 2, 3, 255]).unwrap();
		assert_eq!(decode_rgba(&encode_rgba(&image).unwrap()).unwrap(), image);
		let transparent = pix::RgbaImage::new(1, 1, vec![1, 2, 3, 4]).unwrap();
		assert_eq!(encode_rgba(&transparent), Err(Error::Unsupported));
	}
}
