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
mod tests;
