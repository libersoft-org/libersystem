#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use jpeg_encoder::{ColorType, Encoder};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Invalid,
	Unsupported,
	TooLarge,
}

pub fn decode(data: &[u8]) -> Result<pix::RgbaImage, Error> {
	if is_progressive(data)? {
		return Err(Error::Unsupported);
	}
	let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
	let mut decoder = JpegDecoder::new_with_options(ZCursor::new(data), options);
	decoder.decode_headers().map_err(|_| Error::Invalid)?;
	let info = decoder.info().ok_or(Error::Invalid)?;
	if info.width as u32 > pix::MAX_DIMENSION || info.height as u32 > pix::MAX_DIMENSION || info.width as u64 * info.height as u64 > pix::MAX_PIXELS {
		return Err(Error::TooLarge);
	}
	let pixels = decoder.decode().map_err(|_| Error::Invalid)?;
	pix::RgbaImage::new(info.width as u32, info.height as u32, pixels).map_err(map_pix)
}

pub fn encode(image: &pix::RgbaImage, quality: u8) -> Result<Vec<u8>, Error> {
	if quality > 100 || image.width > u16::MAX as u32 || image.height > u16::MAX as u32 {
		return Err(if quality > 100 { Error::Invalid } else { Error::TooLarge });
	}
	if image.pixels.chunks_exact(4).any(|pixel| pixel[3] != 255) {
		return Err(Error::Unsupported);
	}
	let quality = quality.max(1);
	let mut rgb = Vec::new();
	let length = usize::try_from(image.pixel_count()).ok().and_then(|pixels| pixels.checked_mul(3)).ok_or(Error::TooLarge)?;
	rgb.try_reserve_exact(length).map_err(|_| Error::TooLarge)?;
	for pixel in image.pixels.chunks_exact(4) {
		rgb.extend_from_slice(&pixel[..3]);
	}
	let mut output = Vec::new();
	Encoder::new(&mut output, quality).encode(&rgb, image.width as u16, image.height as u16, ColorType::Rgb).map_err(|_| Error::Invalid)?;
	Ok(output)
}

fn is_progressive(data: &[u8]) -> Result<bool, Error> {
	if data.get(..2) != Some(&[0xff, 0xd8]) {
		return Err(Error::Invalid);
	}
	let mut cursor = 2usize;
	while cursor < data.len() {
		while data.get(cursor) == Some(&0xff) {
			cursor += 1;
		}
		let marker = *data.get(cursor).ok_or(Error::Invalid)?;
		cursor += 1;
		if marker == 0xda || marker == 0xd9 {
			return Ok(false);
		}
		if marker == 0xc2 {
			return Ok(true);
		}
		if marker == 0xc0 || marker == 0xc1 {
			return Ok(false);
		}
		if matches!(marker, 0x01 | 0xd0..=0xd7) {
			continue;
		}
		let length = u16::from_be_bytes(data.get(cursor..cursor + 2).ok_or(Error::Invalid)?.try_into().map_err(|_| Error::Invalid)?) as usize;
		if length < 2 {
			return Err(Error::Invalid);
		}
		cursor = cursor.checked_add(length).ok_or(Error::TooLarge)?;
	}
	Err(Error::Invalid)
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

	fn mean_error(left: &pix::RgbaImage, right: &pix::RgbaImage) -> f64 {
		left.pixels.chunks_exact(4).zip(right.pixels.chunks_exact(4)).flat_map(|(left, right)| (0..3).map(move |channel| (left[channel] as i16 - right[channel] as i16).unsigned_abs() as u64)).sum::<u64>() as f64 / (left.pixel_count() * 3) as f64
	}

	#[test]
	fn quality_endpoints_decode_with_expected_fidelity() {
		let mut pixels = Vec::new();
		for y in 0..16u8 {
			for x in 0..16u8 {
				pixels.extend_from_slice(&[x * 16, y * 16, x.wrapping_add(y) * 8, 255]);
			}
		}
		let image = pix::RgbaImage::new(16, 16, pixels).unwrap();
		let low = decode(&encode(&image, 10).unwrap()).unwrap();
		let high = decode(&encode(&image, 100).unwrap()).unwrap();
		assert!(mean_error(&high, &image) < mean_error(&low, &image));
		assert!(mean_error(&high, &image) < 4.0);
	}

	#[test]
	fn rejects_alpha_invalid_quality_and_progressive_marker() {
		assert_eq!(encode(&pix::RgbaImage::new(1, 1, vec![1, 2, 3, 4]).unwrap(), 90), Err(Error::Unsupported));
		assert_eq!(encode(&pix::RgbaImage::new(1, 1, vec![1, 2, 3, 255]).unwrap(), 101), Err(Error::Invalid));
		assert_eq!(is_progressive(&[0xff, 0xd8, 0xff, 0xc2]), Ok(true));
	}

	#[test]
	fn decodes_external_baseline_profiles_and_rejects_progressive() {
		let gray = include_bytes!("../tests/data/external-gray-baseline.jpg");
		let decoded = decode(gray).unwrap();
		assert_eq!((decoded.width, decoded.height, fnv1a(&decoded.pixels)), (23, 11, 0x114e_cee6_0ce1_ccc3));

		let ycbcr = include_bytes!("../tests/data/external-ycbcr-baseline.jpg");
		let decoded = decode(ycbcr).unwrap();
		let expected = include_bytes!("../tests/data/external-ycbcr-baseline.rgba");
		let maximum = decoded.pixels.iter().zip(expected).map(|(actual, expected)| actual.abs_diff(*expected)).max().unwrap();
		let total = decoded.pixels.iter().zip(expected).map(|(actual, expected)| u64::from(actual.abs_diff(*expected))).sum::<u64>();
		assert_eq!((decoded.width, decoded.height, fnv1a(&decoded.pixels)), (19, 13, 0xaa27_fe0a_c440_e9e4));
		assert!(maximum <= 2);
		assert!(total as f64 / decoded.pixels.len() as f64 <= 0.25);

		let progressive = include_bytes!("../tests/data/external-progressive.jpg");
		assert_eq!(is_progressive(progressive), Ok(true));
		assert_eq!(decode(progressive), Err(Error::Unsupported));
	}
}
