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
	decode_all(data)?.into_iter().max_by_key(|image| image.pixel_count()).ok_or(Error::Unsupported)
}

pub fn decode_all(data: &[u8]) -> Result<Vec<pix::RgbaImage>, Error> {
	let header = data.get(..8).ok_or(Error::Truncated)?;
	if &header[..4] != b"icns" {
		return Err(Error::Invalid);
	}
	let declared = u32::from_be_bytes(header[4..8].try_into().map_err(|_| Error::Truncated)?) as usize;
	if declared != data.len() {
		return Err(if declared > data.len() { Error::Truncated } else { Error::Invalid });
	}
	let mut cursor = 8usize;
	let mut images = Vec::new();
	let mut legacy_colors: Vec<(u32, Vec<u8>)> = Vec::new();
	let mut legacy_masks: Vec<(u32, Vec<u8>)> = Vec::new();
	let mut saw_unsupported = false;
	while cursor < data.len() {
		let entry = data.get(cursor..cursor + 8).ok_or(Error::Truncated)?;
		let kind: [u8; 4] = entry[..4].try_into().map_err(|_| Error::Truncated)?;
		let length = u32::from_be_bytes(entry[4..8].try_into().map_err(|_| Error::Truncated)?) as usize;
		if length < 8 {
			return Err(Error::Invalid);
		}
		let end = cursor.checked_add(length).ok_or(Error::TooLarge)?;
		let payload = data.get(cursor + 8..end).ok_or(Error::Truncated)?;
		if let Some(size) = modern_size(&kind) {
			if payload.starts_with(b"\x89PNG\r\n\x1a\n") {
				let image = png::decode_rgba(payload).map_err(map_png)?;
				if image.width != size || image.height != size {
					return Err(Error::Invalid);
				}
				images.push(image);
			} else {
				saw_unsupported = true;
			}
		} else if let Some(size) = legacy_color_size(&kind) {
			if legacy_colors.iter().any(|(existing, _)| *existing == size) {
				return Err(Error::Invalid);
			}
			legacy_colors.push((size, decode_legacy_rgb(payload, size)?));
		} else if let Some(size) = legacy_mask_size(&kind) {
			let expected = pixel_count(size)?;
			if payload.len() != expected || legacy_masks.iter().any(|(existing, _)| *existing == size) {
				return Err(if payload.len() < expected { Error::Truncated } else { Error::Invalid });
			}
			legacy_masks.push((size, payload.to_vec()));
		}
		cursor = end;
	}
	for (size, colors) in legacy_colors {
		let count = pixel_count(size)?;
		let mask = legacy_masks.iter().find(|(mask_size, _)| *mask_size == size).map(|(_, mask)| mask.as_slice());
		let mut pixels = Vec::new();
		pixels.try_reserve_exact(count.checked_mul(4).ok_or(Error::TooLarge)?).map_err(|_| Error::TooLarge)?;
		for index in 0..count {
			pixels.extend_from_slice(&[colors[index], colors[count + index], colors[count * 2 + index], mask.map_or(255, |alpha| alpha[index])]);
		}
		images.push(pix::RgbaImage::new(size, size, pixels).map_err(map_pix)?);
	}
	images.sort_by_key(|image| image.pixel_count());
	if images.is_empty() && saw_unsupported {
		return Err(Error::Unsupported);
	}
	Ok(images)
}

pub fn encode(images: &[pix::RgbaImage], compression: u8) -> Result<Vec<u8>, Error> {
	if images.is_empty() || images.len() > 16 {
		return Err(Error::Invalid);
	}
	let mut entries: Vec<([u8; 4], Vec<u8>)> = Vec::new();
	entries.try_reserve_exact(images.len()).map_err(|_| Error::TooLarge)?;
	for image in images {
		if image.width != image.height {
			return Err(Error::Unsupported);
		}
		if let Some((color_kind, mask_kind)) = legacy_kinds_for_size(image.width) {
			if entries.iter().any(|(existing, _)| *existing == color_kind || *existing == mask_kind) {
				return Err(Error::Invalid);
			}
			entries.push((color_kind, encode_legacy_rgb(image)?));
			entries.push((mask_kind, image.pixels.chunks_exact(4).map(|pixel| pixel[3]).collect()));
			continue;
		}
		let kind = modern_kind_for_size(image.width).ok_or(Error::Unsupported)?;
		if entries.iter().any(|(existing, _)| *existing == kind) {
			return Err(Error::Invalid);
		}
		entries.push((kind, png::encode_rgba(image, png::EncodeOptions { compression }).map_err(map_png)?));
	}
	entries.sort_by_key(|(kind, _)| *kind);
	let total = entries.iter().try_fold(8usize, |sum, (_, payload)| sum.checked_add(payload.len().checked_add(8).ok_or(Error::TooLarge)?).ok_or(Error::TooLarge))?;
	let mut output = Vec::new();
	output.try_reserve_exact(total).map_err(|_| Error::TooLarge)?;
	output.extend_from_slice(b"icns");
	output.extend_from_slice(&u32::try_from(total).map_err(|_| Error::TooLarge)?.to_be_bytes());
	for (kind, payload) in entries {
		output.extend_from_slice(&kind);
		output.extend_from_slice(&u32::try_from(payload.len() + 8).map_err(|_| Error::TooLarge)?.to_be_bytes());
		output.extend_from_slice(&payload);
	}
	Ok(output)
}

fn modern_size(kind: &[u8; 4]) -> Option<u32> {
	match kind {
		b"ic07" => Some(128),
		b"ic08" => Some(256),
		b"ic09" => Some(512),
		b"ic10" => Some(1024),
		_ => None,
	}
}

fn modern_kind_for_size(size: u32) -> Option<[u8; 4]> {
	match size {
		128 => Some(*b"ic07"),
		256 => Some(*b"ic08"),
		512 => Some(*b"ic09"),
		1024 => Some(*b"ic10"),
		_ => None,
	}
}

fn legacy_color_size(kind: &[u8; 4]) -> Option<u32> {
	match kind {
		b"is32" => Some(16),
		b"il32" => Some(32),
		b"ih32" => Some(48),
		b"it32" => Some(128),
		_ => None,
	}
}

fn legacy_mask_size(kind: &[u8; 4]) -> Option<u32> {
	match kind {
		b"s8mk" => Some(16),
		b"l8mk" => Some(32),
		b"h8mk" => Some(48),
		b"t8mk" => Some(128),
		_ => None,
	}
}

fn legacy_kinds_for_size(size: u32) -> Option<([u8; 4], [u8; 4])> {
	match size {
		16 => Some((*b"is32", *b"s8mk")),
		32 => Some((*b"il32", *b"l8mk")),
		48 => Some((*b"ih32", *b"h8mk")),
		_ => None,
	}
}

fn pixel_count(size: u32) -> Result<usize, Error> {
	usize::try_from(size).ok().and_then(|size| size.checked_mul(size)).ok_or(Error::TooLarge)
}

fn decode_legacy_rgb(payload: &[u8], size: u32) -> Result<Vec<u8>, Error> {
	let count = pixel_count(size)?;
	let payload = if size == 128 && payload.starts_with(&[0, 0, 0, 0]) { &payload[4..] } else { payload };
	let mut cursor = 0usize;
	let mut colors = Vec::new();
	colors.try_reserve_exact(count.checked_mul(3).ok_or(Error::TooLarge)?).map_err(|_| Error::TooLarge)?;
	for _ in 0..3 {
		decode_packbits_component(payload, &mut cursor, count, &mut colors)?;
	}
	if cursor != payload.len() {
		return Err(Error::Invalid);
	}
	Ok(colors)
}

fn decode_packbits_component(input: &[u8], cursor: &mut usize, expected: usize, output: &mut Vec<u8>) -> Result<(), Error> {
	let target = output.len().checked_add(expected).ok_or(Error::TooLarge)?;
	while output.len() < target {
		let control = *input.get(*cursor).ok_or(Error::Truncated)?;
		*cursor += 1;
		if control & 0x80 != 0 {
			let length = (control as usize & 0x7f) + 3;
			let value = *input.get(*cursor).ok_or(Error::Truncated)?;
			*cursor += 1;
			if output.len().checked_add(length).filter(|end| *end <= target).is_none() {
				return Err(Error::Invalid);
			}
			output.extend(core::iter::repeat_n(value, length));
		} else {
			let length = control as usize + 1;
			let end = cursor.checked_add(length).ok_or(Error::TooLarge)?;
			if output.len().checked_add(length).filter(|output_end| *output_end <= target).is_none() {
				return Err(Error::Invalid);
			}
			output.extend_from_slice(input.get(*cursor..end).ok_or(Error::Truncated)?);
			*cursor = end;
		}
	}
	Ok(())
}

fn encode_legacy_rgb(image: &pix::RgbaImage) -> Result<Vec<u8>, Error> {
	let count = pixel_count(image.width)?;
	if image.pixels.len() != count.checked_mul(4).ok_or(Error::TooLarge)? || image.pitch != image.width.checked_mul(4).ok_or(Error::TooLarge)? {
		return Err(Error::Invalid);
	}
	let mut output = Vec::new();
	for channel in 0..3 {
		let values: Vec<u8> = image.pixels.chunks_exact(4).map(|pixel| pixel[channel]).collect();
		encode_packbits_component(&values, &mut output);
	}
	Ok(output)
}

fn encode_packbits_component(input: &[u8], output: &mut Vec<u8>) {
	let mut index = 0usize;
	while index < input.len() {
		let mut repeat = 1usize;
		while repeat < 130 && index + repeat < input.len() && input[index + repeat] == input[index] {
			repeat += 1;
		}
		if repeat >= 3 {
			output.extend_from_slice(&[0x80 | (repeat - 3) as u8, input[index]]);
			index += repeat;
			continue;
		}
		let start = index;
		index += repeat;
		while index < input.len() && index - start < 128 {
			let mut next_repeat = 1usize;
			while next_repeat < 3 && index + next_repeat < input.len() && input[index + next_repeat] == input[index] {
				next_repeat += 1;
			}
			if next_repeat >= 3 {
				break;
			}
			index += next_repeat.min(128 - (index - start));
		}
		let literal = &input[start..index];
		output.push((literal.len() - 1) as u8);
		output.extend_from_slice(literal);
	}
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
