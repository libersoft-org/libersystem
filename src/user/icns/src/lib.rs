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
		if expected_size(&kind).is_some() {
			if payload.starts_with(b"\x89PNG\r\n\x1a\n") {
				let image = png::decode_rgba(payload).map_err(map_png)?;
				let size = expected_size(&kind).ok_or(Error::Invalid)?;
				if image.width != size || image.height != size {
					return Err(Error::Invalid);
				}
				images.push(image);
			} else {
				saw_unsupported = true;
			}
		}
		cursor = end;
	}
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
		let kind = kind_for_size(image.width).filter(|_| image.width == image.height).ok_or(Error::Unsupported)?;
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

fn expected_size(kind: &[u8; 4]) -> Option<u32> {
	match kind {
		b"ic07" => Some(128),
		b"ic08" => Some(256),
		b"ic09" => Some(512),
		b"ic10" => Some(1024),
		_ => None,
	}
}

fn kind_for_size(size: u32) -> Option<[u8; 4]> {
	match size {
		128 => Some(*b"ic07"),
		256 => Some(*b"ic08"),
		512 => Some(*b"ic09"),
		1024 => Some(*b"ic10"),
		_ => None,
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

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
	use super::*;
	use alloc::vec;

	fn solid(size: u32, color: [u8; 4]) -> pix::RgbaImage {
		let mut pixels = Vec::new();
		pixels.resize(size as usize * size as usize * 4, 0);
		for pixel in pixels.chunks_exact_mut(4) {
			pixel.copy_from_slice(&color);
		}
		pix::RgbaImage::new(size, size, pixels).unwrap()
	}

	#[test]
	fn modern_png_entries_round_trip_and_largest_wins() {
		let small = solid(128, [1, 2, 3, 4]);
		let large = solid(256, [5, 6, 7, 8]);
		let encoded = encode(&[large.clone(), small.clone()], 100).unwrap();
		assert_eq!(decode_all(&encoded).unwrap(), vec![small, large.clone()]);
		assert_eq!(decode(&encoded).unwrap(), large);
	}

	#[test]
	fn rejects_nonstandard_output_and_legacy_only_container() {
		assert_eq!(encode(&[solid(64, [0; 4])], 50), Err(Error::Unsupported));
		let legacy = b"icns\x00\x00\x00\x10is32\x00\x00\x00\x08";
		assert_eq!(decode(legacy), Err(Error::Unsupported));
	}
}
