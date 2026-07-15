#![no_std]

extern crate alloc;

use ai_image_webp::{ColorType, EncoderParams, LoopCount, WebPDecoder, WebPEncoder};
use alloc::vec::Vec;
use no_std_io::io::Cursor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Invalid,
	Unsupported,
	TooLarge,
}

pub fn decode(data: &[u8]) -> Result<pix::RgbaImage, Error> {
	let mut decoder = WebPDecoder::new(Cursor::new(data)).map_err(|_| Error::Invalid)?;
	if decoder.is_animated() {
		return decode_animation_with(decoder)?.frames.into_iter().next().map(|frame| frame.image).ok_or(Error::Invalid);
	}
	let (width, height) = decoder.dimensions();
	if width > pix::MAX_DIMENSION || height > pix::MAX_DIMENSION || width as u64 * height as u64 > pix::MAX_PIXELS {
		return Err(Error::TooLarge);
	}
	decoder.set_memory_limit(pix::MAX_PIXELS as usize * 16);
	let mut decoded = alloc::vec![0u8; decoder.output_buffer_size().ok_or(Error::TooLarge)?];
	decoder.read_image(&mut decoded).map_err(|_| Error::Invalid)?;
	let pixels = expand_rgba(decoded, decoder.has_alpha())?;
	pix::RgbaImage::new(width, height, pixels).map_err(map_pix)
}

pub fn decode_animation(data: &[u8]) -> Result<pix::Animation, Error> {
	let decoder = WebPDecoder::new(Cursor::new(data)).map_err(|_| Error::Invalid)?;
	if !decoder.is_animated() {
		return Err(Error::Unsupported);
	}
	decode_animation_with(decoder)
}

fn decode_animation_with(mut decoder: WebPDecoder<Cursor<&[u8]>>) -> Result<pix::Animation, Error> {
	let (width, height) = decoder.dimensions();
	if width > pix::MAX_DIMENSION || height > pix::MAX_DIMENSION || width as u64 * height as u64 > pix::MAX_PIXELS || decoder.num_frames() as usize > pix::MAX_ANIMATION_FRAMES {
		return Err(Error::TooLarge);
	}
	decoder.set_memory_limit(pix::MAX_ANIMATION_PIXELS as usize * 4);
	let has_alpha = decoder.has_alpha();
	let size = decoder.output_buffer_size().ok_or(Error::TooLarge)?;
	let count = decoder.num_frames();
	let mut frames = Vec::new();
	frames.try_reserve_exact(count as usize).map_err(|_| Error::TooLarge)?;
	for _ in 0..count {
		let mut decoded = alloc::vec![0u8; size];
		let duration_ms = decoder.read_frame(&mut decoded).map_err(|_| Error::Invalid)?.max(1);
		let image = pix::RgbaImage::new(width, height, expand_rgba(decoded, has_alpha)?).map_err(map_pix)?;
		frames.push(pix::Frame { image, x: 0, y: 0, duration_ms, blend: pix::Blend::Source, disposal: pix::Disposal::Keep });
	}
	let loop_count = match decoder.loop_count() {
		LoopCount::Forever => 0,
		LoopCount::Times(count) => count.get() as u32,
	};
	pix::Animation::new(width, height, loop_count, frames).map_err(map_pix)
}

fn expand_rgba(decoded: Vec<u8>, has_alpha: bool) -> Result<Vec<u8>, Error> {
	if has_alpha {
		return Ok(decoded);
	}
	let mut rgba = Vec::new();
	rgba.try_reserve_exact(decoded.len() / 3 * 4).map_err(|_| Error::TooLarge)?;
	for pixel in decoded.chunks_exact(3) {
		rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
	}
	Ok(rgba)
}

pub fn encode_lossless(image: &pix::RgbaImage, compression: u8) -> Result<Vec<u8>, Error> {
	let predictor = match compression {
		0 => false,
		100 => true,
		_ => return Err(Error::Unsupported),
	};
	let mut output = Vec::new();
	let mut encoder = WebPEncoder::new(&mut output);
	let mut params = EncoderParams::default();
	params.use_predictor_transform = predictor;
	encoder.set_params(params);
	encoder.encode(&image.pixels, image.width, image.height, ColorType::Rgba8).map_err(|_| Error::Invalid)?;
	Ok(output)
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
	fn lossless_endpoints_round_trip_rgba() {
		let image = pix::RgbaImage::new(3, 2, vec![255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4]).unwrap();
		for compression in [0, 100] {
			assert_eq!(decode(&encode_lossless(&image, compression).unwrap()).unwrap(), image);
		}
	}

	#[test]
	fn rejects_unimplemented_intermediate_effort_and_bad_input() {
		let image = pix::RgbaImage::new(1, 1, vec![0, 0, 0, 255]).unwrap();
		assert_eq!(encode_lossless(&image, 50), Err(Error::Unsupported));
		assert_eq!(decode(b"RIFF"), Err(Error::Invalid));
	}

	#[test]
	fn decodes_independent_animation_as_full_canvas_frames() {
		let animation = decode_animation(include_bytes!("../tests/animated.webp")).unwrap();
		assert_eq!((animation.width, animation.height), (2, 2));
		assert_eq!(animation.frames.len(), 2);
		assert_eq!(animation.loop_count, 0);
		assert!(animation.frames.iter().all(|frame| frame.duration_ms != 0 && frame.x == 0 && frame.y == 0 && frame.blend == pix::Blend::Source && frame.disposal == pix::Disposal::Keep));
		assert_ne!(animation.frames[0].image.pixels, animation.frames[1].image.pixels);
		assert_eq!(decode(include_bytes!("../tests/animated.webp")).unwrap(), animation.frames[0].image);
	}
}
