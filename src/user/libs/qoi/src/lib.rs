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
mod tests;
