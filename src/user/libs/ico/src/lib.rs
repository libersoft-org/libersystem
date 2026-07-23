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

pub fn decode(data: &[u8]) -> Result<pix::RgbaImage, Error> {
	let images = decode_all(data)?;
	images.into_iter().max_by_key(|image| image.pixel_count()).ok_or(Error::Invalid)
}

pub fn decode_all(data: &[u8]) -> Result<Vec<pix::RgbaImage>, Error> {
	let header = data.get(..6).ok_or(Error::Truncated)?;
	if header[..4] != [0, 0, 1, 0] {
		return Err(Error::Invalid);
	}
	let count = u16::from_le_bytes([header[4], header[5]]) as usize;
	if count == 0 || count > 256 {
		return Err(Error::Invalid);
	}
	let table_end = 6usize.checked_add(count.checked_mul(16).ok_or(Error::TooLarge)?).ok_or(Error::TooLarge)?;
	let table = data.get(6..table_end).ok_or(Error::Truncated)?;
	let mut images = Vec::new();
	let mut ranges = Vec::new();
	images.try_reserve_exact(count).map_err(|_| Error::TooLarge)?;
	ranges.try_reserve_exact(count).map_err(|_| Error::TooLarge)?;
	for entry in table.chunks_exact(16) {
		let width = if entry[0] == 0 { 256 } else { entry[0] as u32 };
		let height = if entry[1] == 0 { 256 } else { entry[1] as u32 };
		let size = u32::from_le_bytes(entry[8..12].try_into().map_err(|_| Error::Truncated)?) as usize;
		let offset = u32::from_le_bytes(entry[12..16].try_into().map_err(|_| Error::Truncated)?) as usize;
		let end = offset.checked_add(size).ok_or(Error::TooLarge)?;
		if size == 0 || offset < table_end || ranges.iter().any(|(start, previous_end)| offset < *previous_end && *start < end) {
			return Err(Error::Invalid);
		}
		let payload = data.get(offset..end).ok_or(Error::Truncated)?;
		ranges.push((offset, end));
		let image = if payload.starts_with(b"\x89PNG\r\n\x1a\n") { png::decode_rgba(payload).map_err(map_png)? } else { decode_bmp_entry(payload, width, height)? };
		if image.width != width || image.height != height {
			return Err(Error::Invalid);
		}
		images.push(image);
	}
	Ok(images)
}

pub fn encode(images: &[pix::RgbaImage], compression: u8) -> Result<Vec<u8>, Error> {
	if images.is_empty() || images.len() > 256 {
		return Err(Error::Invalid);
	}
	let table_end = 6usize.checked_add(images.len().checked_mul(16).ok_or(Error::TooLarge)?).ok_or(Error::TooLarge)?;
	let mut payloads = Vec::new();
	payloads.try_reserve_exact(images.len()).map_err(|_| Error::TooLarge)?;
	for image in images {
		if image.width == 0 || image.height == 0 || image.width > 256 || image.height > 256 {
			return Err(Error::Unsupported);
		}
		payloads.push(png::encode_rgba(image, png::EncodeOptions { compression }).map_err(map_png)?);
	}
	let payload_len = payloads.iter().try_fold(0usize, |sum, payload| sum.checked_add(payload.len()).ok_or(Error::TooLarge))?;
	let mut output = Vec::new();
	output.try_reserve_exact(table_end.checked_add(payload_len).ok_or(Error::TooLarge)?).map_err(|_| Error::TooLarge)?;
	output.extend_from_slice(&[0, 0, 1, 0]);
	output.extend_from_slice(&(images.len() as u16).to_le_bytes());
	let mut offset = table_end;
	for (image, payload) in images.iter().zip(payloads.iter()) {
		output.push(if image.width == 256 { 0 } else { image.width as u8 });
		output.push(if image.height == 256 { 0 } else { image.height as u8 });
		output.extend_from_slice(&[0, 0]);
		output.extend_from_slice(&1u16.to_le_bytes());
		output.extend_from_slice(&32u16.to_le_bytes());
		output.extend_from_slice(&u32::try_from(payload.len()).map_err(|_| Error::TooLarge)?.to_le_bytes());
		output.extend_from_slice(&u32::try_from(offset).map_err(|_| Error::TooLarge)?.to_le_bytes());
		offset = offset.checked_add(payload.len()).ok_or(Error::TooLarge)?;
	}
	for payload in payloads {
		output.extend_from_slice(&payload);
	}
	Ok(output)
}

fn decode_bmp_entry(data: &[u8], expected_width: u32, expected_height: u32) -> Result<pix::RgbaImage, Error> {
	let header = data.get(..40).ok_or(Error::Truncated)?;
	let header_len = u32::from_le_bytes(header[..4].try_into().map_err(|_| Error::Truncated)?) as usize;
	if header_len < 40 || header_len > data.len() {
		return Err(Error::Unsupported);
	}
	let width = i32::from_le_bytes(header[4..8].try_into().map_err(|_| Error::Truncated)?);
	let stored_height = i32::from_le_bytes(header[8..12].try_into().map_err(|_| Error::Truncated)?);
	let planes = u16::from_le_bytes(header[12..14].try_into().map_err(|_| Error::Truncated)?);
	let depth = u16::from_le_bytes(header[14..16].try_into().map_err(|_| Error::Truncated)?);
	let compression = u32::from_le_bytes(header[16..20].try_into().map_err(|_| Error::Truncated)?);
	if width <= 0 || stored_height <= 0 || stored_height % 2 != 0 || planes != 1 || depth != 32 || compression != 0 {
		return Err(Error::Unsupported);
	}
	let width = width as u32;
	let height = stored_height as u32 / 2;
	if width != expected_width || height != expected_height {
		return Err(Error::Invalid);
	}
	let xor_len = width as usize * height as usize * 4;
	let xor_end = header_len.checked_add(xor_len).ok_or(Error::TooLarge)?;
	let xor = data.get(header_len..xor_end).ok_or(Error::Truncated)?;
	let mut pixels = alloc::vec![0u8; xor_len];
	for file_y in 0..height as usize {
		let y = height as usize - 1 - file_y;
		for x in 0..width as usize {
			let source = (file_y * width as usize + x) * 4;
			let target = (y * width as usize + x) * 4;
			pixels[target..target + 4].copy_from_slice(&[xor[source + 2], xor[source + 1], xor[source], xor[source + 3]]);
		}
	}
	pix::RgbaImage::new(width, height, pixels).map_err(map_pix)
}

fn map_png(error: png::Error) -> Error {
	match error {
		png::Error::Truncated => Error::Truncated,
		png::Error::Invalid => Error::Invalid,
		png::Error::Unsupported => Error::Unsupported,
		png::Error::TooLarge => Error::TooLarge,
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
mod tests;
