#![no_std]

extern crate alloc;

use ai_image_webp::{ColorType, EncoderParams, WebPDecoder, WebPEncoder};
use alloc::vec::Vec;
use no_std_io::io::Cursor;

mod vp8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Invalid,
	Unsupported,
	TooLarge,
}

pub fn decode(data: &[u8]) -> Result<pix::RgbaImage, Error> {
	let mut decoder = WebPDecoder::new(Cursor::new(data)).map_err(|_| Error::Invalid)?;
	if decoder.is_animated() {
		let animation = decode_animation(data)?;
		let mut compositor = pix::Compositor::new_with_background(animation.width, animation.height, animation.background).map_err(map_pix)?;
		return compositor.render(animation.frames.first().ok_or(Error::Invalid)?).map_err(map_pix);
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
	if data.len() < 12 || data.get(..4) != Some(b"RIFF") || data.get(8..12) != Some(b"WEBP") {
		return Err(Error::Invalid);
	}
	let declared = u32::from_le_bytes(data[4..8].try_into().map_err(|_| Error::Invalid)?) as usize;
	if declared.checked_add(8) != Some(data.len()) {
		return Err(Error::Invalid);
	}
	let mut cursor = 12usize;
	let mut canvas = None;
	let mut background = None;
	let mut loop_count = None;
	let mut frames = Vec::new();
	let mut trailing = false;
	while cursor < data.len() {
		let header = data.get(cursor..cursor + 8).ok_or(Error::Invalid)?;
		let kind: [u8; 4] = header[..4].try_into().map_err(|_| Error::Invalid)?;
		let length = u32::from_le_bytes(header[4..8].try_into().map_err(|_| Error::Invalid)?) as usize;
		let padded = length.checked_add(length & 1).ok_or(Error::TooLarge)?;
		let payload_start = cursor.checked_add(8).ok_or(Error::TooLarge)?;
		let end = payload_start.checked_add(padded).ok_or(Error::TooLarge)?;
		if end > data.len() {
			return Err(Error::Invalid);
		}
		if length & 1 != 0 && data[payload_start + length] != 0 {
			return Err(Error::Invalid);
		}
		let payload = &data[payload_start..payload_start + length];
		match &kind {
			b"VP8 " | b"VP8L" if cursor == 12 => return Err(Error::Unsupported),
			b"VP8X" => {
				if cursor != 12 || payload.len() != 10 || payload[0] & 0xc1 != 0 || payload[1..4] != [0; 3] || canvas.is_some() {
					return Err(Error::Invalid);
				}
				if payload[0] & 0x02 == 0 {
					return Err(Error::Unsupported);
				}
				let width = read_u24(&payload[4..7])?.checked_add(1).ok_or(Error::TooLarge)?;
				let height = read_u24(&payload[7..10])?.checked_add(1).ok_or(Error::TooLarge)?;
				if width > pix::MAX_DIMENSION || height > pix::MAX_DIMENSION || width as u64 * height as u64 > pix::MAX_PIXELS {
					return Err(Error::TooLarge);
				}
				canvas = Some((width, height));
			}
			b"ANIM" => {
				if canvas.is_none() || payload.len() != 6 || loop_count.is_some() || trailing {
					return Err(Error::Invalid);
				}
				background = Some([payload[2], payload[1], payload[0], payload[3]]);
				loop_count = Some(u16::from_le_bytes([payload[4], payload[5]]) as u32);
			}
			b"ANMF" => {
				let (canvas_width, canvas_height) = canvas.ok_or(Error::Invalid)?;
				if loop_count.is_none() || trailing || payload.len() < 24 || frames.len() >= pix::MAX_ANIMATION_FRAMES {
					return Err(if frames.len() >= pix::MAX_ANIMATION_FRAMES { Error::TooLarge } else { Error::Invalid });
				}
				let x = read_u24(&payload[0..3])?.checked_mul(2).ok_or(Error::TooLarge)?;
				let y = read_u24(&payload[3..6])?.checked_mul(2).ok_or(Error::TooLarge)?;
				let width = read_u24(&payload[6..9])?.checked_add(1).ok_or(Error::TooLarge)?;
				let height = read_u24(&payload[9..12])?.checked_add(1).ok_or(Error::TooLarge)?;
				let duration_ms = read_u24(&payload[12..15])?;
				if x.checked_add(width).filter(|end| *end <= canvas_width).is_none() || y.checked_add(height).filter(|end| *end <= canvas_height).is_none() {
					return Err(Error::Invalid);
				}
				let image = decode_frame_chunks(&payload[16..], width, height)?;
				let flags = payload[15];
				if flags & 0xfc != 0 {
					return Err(Error::Invalid);
				}
				frames.push(pix::Frame { image, x, y, duration_ms, blend: if flags & 0x02 == 0 { pix::Blend::Over } else { pix::Blend::Source }, disposal: if flags & 0x01 == 0 { pix::Disposal::Keep } else { pix::Disposal::Background } });
			}
			b"ICCP" if loop_count.is_none() && !trailing => {}
			b"EXIF" | b"XMP " if !frames.is_empty() => trailing = true,
			_ if !frames.is_empty() => trailing = true,
			_ => return Err(Error::Invalid),
		}
		cursor = end;
	}
	let (width, height) = canvas.ok_or(Error::Unsupported)?;
	pix::Animation::new_with_background(width, height, background.ok_or(Error::Invalid)?, loop_count.ok_or(Error::Invalid)?, frames).map_err(map_pix)
}

fn decode_frame_chunks(chunks: &[u8], width: u32, height: u32) -> Result<pix::RgbaImage, Error> {
	let has_alpha = validate_image_chunks(chunks)?;
	let mut still = Vec::new();
	let extra = if has_alpha { 18 } else { 0 };
	still.try_reserve_exact(12usize.checked_add(extra).and_then(|length| length.checked_add(chunks.len())).ok_or(Error::TooLarge)?).map_err(|_| Error::TooLarge)?;
	still.extend_from_slice(b"RIFF");
	still.extend_from_slice(&0u32.to_le_bytes());
	still.extend_from_slice(b"WEBP");
	if has_alpha {
		let mut vp8x = Vec::new();
		vp8x.extend_from_slice(&[0x10, 0, 0, 0]);
		put_u24(&mut vp8x, width - 1)?;
		put_u24(&mut vp8x, height - 1)?;
		append_chunk(&mut still, b"VP8X", &vp8x)?;
	}
	still.extend_from_slice(chunks);
	let riff_size = u32::try_from(still.len().checked_sub(8).ok_or(Error::Invalid)?).map_err(|_| Error::TooLarge)?;
	still[4..8].copy_from_slice(&riff_size.to_le_bytes());
	let image = decode(&still)?;
	if image.width != width || image.height != height {
		return Err(Error::Invalid);
	}
	Ok(image)
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
	if compression > 100 {
		return Err(Error::Unsupported);
	}
	if compression == 100 {
		let plain = encode_lossless_profile(image, false)?;
		let predicted = encode_lossless_profile(image, true)?;
		return Ok(if predicted.len() < plain.len() { predicted } else { plain });
	}
	let predictor = compression != 0 && predictor_is_promising(image, compression);
	encode_lossless_profile(image, predictor)
}

fn encode_lossless_profile(image: &pix::RgbaImage, predictor: bool) -> Result<Vec<u8>, Error> {
	let mut output = Vec::new();
	let mut encoder = WebPEncoder::new(&mut output);
	let mut params = EncoderParams::default();
	params.use_predictor_transform = predictor;
	encoder.set_params(params);
	encoder.encode(&image.pixels, image.width, image.height, ColorType::Rgba8).map_err(|_| Error::Invalid)?;
	Ok(output)
}

fn predictor_is_promising(image: &pix::RgbaImage, effort: u8) -> bool {
	let width = image.width as usize;
	let height = image.height as usize;
	let rows = height.saturating_mul(usize::from(effort)).div_ceil(100).max(1);
	let mut raw_variation = 0u64;
	let mut predicted_variation = 0u64;
	for y in 0..rows.min(height) {
		for x in 0..width {
			let pixel = (y * width + x) * 4;
			for channel in 0..4usize {
				let value = image.pixels[pixel + channel];
				let prediction = if x != 0 {
					image.pixels[pixel + channel - 4]
				} else if y != 0 {
					image.pixels[pixel + channel - width * 4]
				} else if channel == 3 {
					255
				} else {
					0
				};
				raw_variation += u64::from(value.min(255 - value));
				let residual = value.wrapping_sub(prediction);
				predicted_variation += u64::from(residual.min(255 - residual));
			}
		}
	}
	predicted_variation < raw_variation
}

pub fn encode_lossy(image: &pix::RgbaImage, quality: u8, effort: u8) -> Result<Vec<u8>, Error> {
	if quality > 100 || effort > 100 {
		return Err(Error::Unsupported);
	}
	let frame = vp8::encode_keyframe(image, quality, effort)?;
	let has_alpha = image.pixels.chunks_exact(4).any(|pixel| pixel[3] != 255);
	let mut output = Vec::new();
	output.try_reserve_exact(frame.len().checked_add(if has_alpha { image.pixels.len() / 4 + 48 } else { 20 }).ok_or(Error::TooLarge)?).map_err(|_| Error::TooLarge)?;
	output.extend_from_slice(b"RIFF");
	output.extend_from_slice(&0u32.to_le_bytes());
	output.extend_from_slice(b"WEBP");
	if has_alpha {
		let mut vp8x = Vec::new();
		vp8x.extend_from_slice(&[0x10, 0, 0, 0]);
		put_u24(&mut vp8x, image.width - 1)?;
		put_u24(&mut vp8x, image.height - 1)?;
		append_chunk(&mut output, b"VP8X", &vp8x)?;
		append_alpha(&mut output, &image.pixels)?;
	}
	append_chunk(&mut output, b"VP8 ", &frame)?;
	let riff_size = u32::try_from(output.len().checked_sub(8).ok_or(Error::Invalid)?).map_err(|_| Error::TooLarge)?;
	output[4..8].copy_from_slice(&riff_size.to_le_bytes());
	Ok(output)
}

pub fn encode_animation(animation: &pix::Animation, compression: u8) -> Result<Vec<u8>, Error> {
	let loop_count = u16::try_from(animation.loop_count).map_err(|_| Error::Unsupported)?;
	let mut output = Vec::new();
	output.try_reserve_exact(64).map_err(|_| Error::TooLarge)?;
	output.extend_from_slice(b"RIFF");
	output.extend_from_slice(&0u32.to_le_bytes());
	output.extend_from_slice(b"WEBP");

	let mut vp8x = Vec::new();
	vp8x.try_reserve_exact(10).map_err(|_| Error::TooLarge)?;
	vp8x.extend_from_slice(&[0x12, 0, 0, 0]);
	put_u24(&mut vp8x, animation.width.checked_sub(1).ok_or(Error::Invalid)?)?;
	put_u24(&mut vp8x, animation.height.checked_sub(1).ok_or(Error::Invalid)?)?;
	append_chunk(&mut output, b"VP8X", &vp8x)?;
	let mut anim = Vec::new();
	anim.try_reserve_exact(6).map_err(|_| Error::TooLarge)?;
	anim.extend_from_slice(&[animation.background[2], animation.background[1], animation.background[0], animation.background[3]]);
	anim.extend_from_slice(&loop_count.to_le_bytes());
	append_chunk(&mut output, b"ANIM", &anim)?;

	let mut compositor = pix::Compositor::new_with_background(animation.width, animation.height, animation.background).map_err(map_pix)?;
	for frame in &animation.frames {
		if frame.duration_ms > 0x00ff_ffff {
			return Err(Error::TooLarge);
		}
		let displayed = compositor.render(frame).map_err(map_pix)?;
		let still = encode_lossless(&displayed, compression)?;
		let chunks = static_image_chunks(&still)?;
		let payload_len = 16usize.checked_add(chunks.len()).ok_or(Error::TooLarge)?;
		let mut payload = Vec::new();
		payload.try_reserve_exact(payload_len).map_err(|_| Error::TooLarge)?;
		put_u24(&mut payload, 0)?;
		put_u24(&mut payload, 0)?;
		put_u24(&mut payload, animation.width - 1)?;
		put_u24(&mut payload, animation.height - 1)?;
		put_u24(&mut payload, frame.duration_ms)?;
		payload.push(0x02);
		payload.extend_from_slice(chunks);
		append_chunk(&mut output, b"ANMF", &payload)?;
	}
	let riff_size = u32::try_from(output.len().checked_sub(8).ok_or(Error::Invalid)?).map_err(|_| Error::TooLarge)?;
	output[4..8].copy_from_slice(&riff_size.to_le_bytes());
	Ok(output)
}

fn static_image_chunks(data: &[u8]) -> Result<&[u8], Error> {
	if data.len() < 20 || data.get(..4) != Some(b"RIFF") || data.get(8..12) != Some(b"WEBP") {
		return Err(Error::Invalid);
	}
	let declared = u32::from_le_bytes(data[4..8].try_into().map_err(|_| Error::Invalid)?) as usize;
	if declared.checked_add(8) != Some(data.len()) {
		return Err(Error::Invalid);
	}
	let chunks = &data[12..];
	validate_image_chunks(chunks)?;
	Ok(chunks)
}

fn validate_image_chunks(chunks: &[u8]) -> Result<bool, Error> {
	let mut cursor = 0usize;
	let mut image_chunks = 0usize;
	let mut has_alpha = false;
	while cursor < chunks.len() {
		let header = chunks.get(cursor..cursor + 8).ok_or(Error::Invalid)?;
		let kind = &header[..4];
		if !matches!(kind, b"VP8L" | b"VP8 " | b"ALPH") {
			return Err(Error::Unsupported);
		}
		if matches!(kind, b"VP8L" | b"VP8 ") {
			image_chunks += 1;
		}
		if kind == b"ALPH" {
			has_alpha = true;
		}
		let length = u32::from_le_bytes(header[4..8].try_into().map_err(|_| Error::Invalid)?) as usize;
		let padded = length.checked_add(length & 1).ok_or(Error::TooLarge)?;
		cursor = cursor.checked_add(8).and_then(|cursor| cursor.checked_add(padded)).ok_or(Error::TooLarge)?;
		if cursor > chunks.len() {
			return Err(Error::Invalid);
		}
	}
	if image_chunks != 1 {
		return Err(Error::Invalid);
	}
	Ok(has_alpha)
}

fn append_chunk(output: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) -> Result<(), Error> {
	let length = u32::try_from(payload.len()).map_err(|_| Error::TooLarge)?;
	let appended = 8usize.checked_add(payload.len()).and_then(|length| length.checked_add(payload.len() & 1)).ok_or(Error::TooLarge)?;
	output.try_reserve(appended).map_err(|_| Error::TooLarge)?;
	output.extend_from_slice(kind);
	output.extend_from_slice(&length.to_le_bytes());
	output.extend_from_slice(payload);
	if payload.len() & 1 != 0 {
		output.push(0);
	}
	Ok(())
}

fn append_alpha(output: &mut Vec<u8>, rgba: &[u8]) -> Result<(), Error> {
	let count = rgba.len() / 4;
	let length = u32::try_from(count.checked_add(1).ok_or(Error::TooLarge)?).map_err(|_| Error::TooLarge)?;
	let padding = usize::from(length & 1 != 0);
	output.try_reserve(9usize.checked_add(count).and_then(|size| size.checked_add(padding)).ok_or(Error::TooLarge)?).map_err(|_| Error::TooLarge)?;
	output.extend_from_slice(b"ALPH");
	output.extend_from_slice(&length.to_le_bytes());
	output.push(0);
	output.extend(rgba.chunks_exact(4).map(|pixel| pixel[3]));
	if padding != 0 {
		output.push(0);
	}
	Ok(())
}

fn put_u24(output: &mut Vec<u8>, value: u32) -> Result<(), Error> {
	if value > 0x00ff_ffff {
		return Err(Error::TooLarge);
	}
	let bytes = value.to_le_bytes();
	output.extend_from_slice(&bytes[..3]);
	Ok(())
}

fn read_u24(input: &[u8]) -> Result<u32, Error> {
	let bytes: [u8; 3] = input.try_into().map_err(|_| Error::Invalid)?;
	Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0]))
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
