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
	qoi_codec::encode_to_vec(&image.pixels, image.width, image.height).map_err(|_| Error::Invalid)
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
	fn rgba_round_trips_exactly() {
		let image = pix::RgbaImage::new(2, 2, vec![255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 1, 2, 3, 4]).unwrap();
		assert_eq!(decode(&encode(&image).unwrap()).unwrap(), image);
	}

	#[test]
	fn rejects_bad_stream_and_oversized_geometry() {
		assert_eq!(decode(b"qoif"), Err(Error::Invalid));
		let mut encoded = encode(&pix::RgbaImage::new(1, 1, vec![0, 0, 0, 255]).unwrap()).unwrap();
		encoded[4..8].copy_from_slice(&20_000u32.to_be_bytes());
		assert!(decode(&encoded).is_err());
	}
}
