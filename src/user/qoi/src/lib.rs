#![no_std]

extern crate alloc;

use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Invalid,
	Unsupported,
	TooLarge,
}

pub fn decode(data: &[u8]) -> Result<pix::RgbaImage, Error> {
	let header = qoi_codec::decode_header(data).map_err(|_| Error::Invalid)?;
	if header.width > pix::MAX_DIMENSION || header.height > pix::MAX_DIMENSION || header.width as u64 * header.height as u64 > pix::MAX_PIXELS {
		return Err(Error::TooLarge);
	}
	let (header, decoded) = qoi_codec::decode_to_vec(data).map_err(|_| Error::Invalid)?;
	let pixels = match header.channels {
		qoi_codec::Channels::Rgba => decoded,
		qoi_codec::Channels::Rgb => {
			let mut rgba = Vec::new();
			rgba.try_reserve_exact(decoded.len() / 3 * 4).map_err(|_| Error::TooLarge)?;
			for pixel in decoded.chunks_exact(3) {
				rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
			}
			rgba
		}
	};
	pix::RgbaImage::new(header.width, header.height, pixels).map_err(map_pix)
}

pub fn encode(image: &pix::RgbaImage) -> Result<Vec<u8>, Error> {
	if image.pixels.chunks_exact(4).any(|pixel| pixel[3] != 255) {
		return qoi_codec::encode_to_vec(&image.pixels, image.width, image.height).map_err(|_| Error::Invalid);
	}
	let mut rgb = Vec::new();
	let length = usize::try_from(image.pixel_count()).ok().and_then(|pixels| pixels.checked_mul(3)).ok_or(Error::TooLarge)?;
	rgb.try_reserve_exact(length).map_err(|_| Error::TooLarge)?;
	for pixel in image.pixels.chunks_exact(4) {
		rgb.extend_from_slice(&pixel[..3]);
	}
	qoi_codec::encode_to_vec(rgb, image.width, image.height).map_err(|_| Error::Invalid)
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

	fn fnv1a(bytes: &[u8]) -> u64 {
		bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
	}

	fn opcode_counts(data: &[u8]) -> [usize; 6] {
		let pixels = u32::from_be_bytes(data[4..8].try_into().unwrap()) as usize * u32::from_be_bytes(data[8..12].try_into().unwrap()) as usize;
		let mut counts = [0usize; 6];
		let mut cursor = 14usize;
		let mut decoded = 0usize;
		while decoded < pixels {
			let byte = data[cursor];
			cursor += 1;
			match byte {
				0xfe => {
					counts[4] += 1;
					cursor += 3;
					decoded += 1;
				}
				0xff => {
					counts[5] += 1;
					cursor += 4;
					decoded += 1;
				}
				_ => match byte >> 6 {
					0 => {
						counts[0] += 1;
						decoded += 1;
					}
					1 => {
						counts[1] += 1;
						decoded += 1;
					}
					2 => {
						counts[2] += 1;
						cursor += 1;
						decoded += 1;
					}
					_ => {
						counts[3] += 1;
						decoded += usize::from(byte & 0x3f) + 1;
					}
				},
			}
		}
		counts
	}

	#[test]
	fn rgba_round_trips_exactly() {
		let image = pix::RgbaImage::new(2, 2, vec![255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 1, 2, 3, 4]).unwrap();
		let encoded = encode(&image).unwrap();
		assert_eq!(encoded[12], 4);
		assert_eq!(decode(&encoded).unwrap(), image);

		let opaque = pix::RgbaImage::new(2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255]).unwrap();
		let encoded = encode(&opaque).unwrap();
		assert_eq!(encoded[12], 3);
		assert_eq!(decode(&encoded).unwrap(), opaque);
	}

	#[test]
	fn rejects_bad_stream_and_oversized_geometry() {
		assert_eq!(decode(b"qoif"), Err(Error::Invalid));
		let mut encoded = encode(&pix::RgbaImage::new(1, 1, vec![0, 0, 0, 255]).unwrap()).unwrap();
		encoded[4..8].copy_from_slice(&20_000u32.to_be_bytes());
		assert!(decode(&encoded).is_err());
	}

	#[test]
	fn decodes_external_netpbm_rgb_rgba_and_every_opcode_family() {
		let rgb = include_bytes!("../tests/data/external-rgb.qoi");
		assert_eq!(&rgb[..14], b"qoif\0\0\x01\x01\0\0\0\x03\x03\0");
		assert_eq!(opcode_counts(rgb), [1, 256, 64, 10, 2, 0]);
		assert_eq!(fnv1a(&decode(rgb).unwrap().pixels), 0x8493_67b2_3303_6f72);

		let rgba = include_bytes!("../tests/data/external-rgba.qoi");
		assert_eq!(&rgba[..14], b"qoif\0\0\0\x11\0\0\0\x09\x04\0");
		assert_eq!(opcode_counts(rgba), [2, 0, 0, 13, 0, 13]);
		assert_eq!(fnv1a(&decode(rgba).unwrap().pixels), 0xe8e4_8624_24b7_12f2);
	}
}
