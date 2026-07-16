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
mod tests {
	use super::*;
	use alloc::vec;

	fn insert_chunk(data: &mut Vec<u8>, offset: usize, kind: &[u8; 4], payload: &[u8]) {
		let mut chunk = Vec::new();
		append_chunk(&mut chunk, kind, payload).unwrap();
		data.splice(offset..offset, chunk);
		let size = u32::try_from(data.len() - 8).unwrap();
		data[4..8].copy_from_slice(&size.to_le_bytes());
	}

	fn top_level_offsets(data: &[u8], target: &[u8; 4]) -> Vec<usize> {
		let mut offsets = Vec::new();
		let mut cursor = 12usize;
		while cursor < data.len() {
			let length = u32::from_le_bytes(data[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
			if &data[cursor..cursor + 4] == target {
				offsets.push(cursor);
			}
			cursor += 8 + length + (length & 1);
		}
		offsets
	}

	#[test]
	fn lossless_endpoints_round_trip_rgba() {
		let image = pix::RgbaImage::new(3, 2, vec![255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4]).unwrap();
		let plain = encode_lossless_profile(&image, false).unwrap();
		let predicted = encode_lossless_profile(&image, true).unwrap();
		for compression in [0, 1, 24, 25, 49, 50, 74, 75, 99, 100] {
			let encoded = encode_lossless(&image, compression).unwrap();
			assert_eq!(encoded, encode_lossless(&image, compression).unwrap());
			assert_eq!(decode(&encoded).unwrap(), image);
		}
		let compact = encode_lossless(&image, 100).unwrap();
		assert!(compact.len() <= plain.len() && compact.len() <= predicted.len());

		let mut pixels = Vec::new();
		for x in 0..16usize {
			let value = if x & 1 == 0 { 0 } else { 255 };
			pixels.extend_from_slice(&[value, value, value, 255]);
		}
		for _ in 0..48 {
			pixels.extend_from_slice(&[128, 128, 128, 255]);
		}
		let search_fixture = pix::RgbaImage::new(16, 4, pixels).unwrap();
		assert!(!predictor_is_promising(&search_fixture, 25));
		assert!(predictor_is_promising(&search_fixture, 50));
		assert_ne!(encode_lossless(&search_fixture, 25).unwrap(), encode_lossless(&search_fixture, 50).unwrap());
	}

	#[test]
	fn rejects_out_of_range_effort_and_bad_input() {
		let image = pix::RgbaImage::new(1, 1, vec![0, 0, 0, 255]).unwrap();
		assert_eq!(encode_lossless(&image, 101), Err(Error::Unsupported));
		assert_eq!(decode(b"RIFF"), Err(Error::Invalid));
	}

	#[test]
	fn lossy_quality_improves_rgb_and_preserves_alpha() {
		let mut pixels = Vec::new();
		for y in 0..17u8 {
			for x in 0..19u8 {
				pixels.extend_from_slice(&[x.wrapping_mul(11), y.wrapping_mul(13), x.wrapping_mul(5).wrapping_add(y.wrapping_mul(7)), x.wrapping_mul(17).wrapping_add(y.wrapping_mul(3))]);
			}
		}
		let source = pix::RgbaImage::new(19, 17, pixels).unwrap();
		let low_bytes = encode_lossy(&source, 0, 100).unwrap();
		let high_bytes = encode_lossy(&source, 100, 100).unwrap();
		assert_eq!(high_bytes, encode_lossy(&source, 100, 100).unwrap());
		assert_ne!(encode_lossy(&source, 80, 0).unwrap(), encode_lossy(&source, 80, 100).unwrap());
		assert_eq!(&high_bytes[12..16], b"VP8X");
		assert!(high_bytes.windows(4).any(|window| window == b"ALPH"));
		for effort in [0, 24, 25, 49, 50, 74, 75, 100] {
			let decoded = decode(&encode_lossy(&source, 80, effort).unwrap()).unwrap();
			assert_eq!((decoded.width, decoded.height), (19, 17));
		}
		let low = decode(&low_bytes).unwrap();
		let high = decode(&high_bytes).unwrap();
		let error = |actual: &pix::RgbaImage| -> u64 { actual.pixels.chunks_exact(4).zip(source.pixels.chunks_exact(4)).map(|(actual, expected)| (0..3).map(|channel| u64::from(actual[channel].abs_diff(expected[channel]))).sum::<u64>()).sum() };
		assert!(error(&high) < error(&low));
		for actual in [low, high] {
			assert_eq!((actual.width, actual.height), (source.width, source.height));
			assert_eq!(actual.pixels.iter().skip(3).step_by(4).copied().collect::<Vec<_>>(), source.pixels.iter().skip(3).step_by(4).copied().collect::<Vec<_>>());
		}
		assert_eq!(encode_lossy(&source, 101, 100), Err(Error::Unsupported));
		assert_eq!(encode_lossy(&source, 100, 101), Err(Error::Unsupported));

		let opaque = pix::RgbaImage::new(1, 1, vec![31, 127, 223, 255]).unwrap();
		let opaque = encode_lossy(&opaque, 100, 100).unwrap();
		assert_eq!(&opaque[12..16], b"VP8 ");
		for end in [0, 4, 12, 20, opaque.len() / 2] {
			assert_eq!(decode(&opaque[..end]), Err(Error::Invalid));
		}
		let mut corrupt = opaque;
		corrupt[23] ^= 0xff;
		assert_eq!(decode(&corrupt), Err(Error::Invalid));
	}

	#[test]
	fn decodes_animation_with_exact_anmf_metadata_and_composited_preview() {
		let animation = decode_animation(include_bytes!("../tests/animated.webp")).unwrap();
		assert_eq!((animation.width, animation.height), (2, 2));
		assert_eq!(animation.frames.len(), 2);
		assert_eq!(animation.loop_count, 0);
		assert!(animation.frames.iter().all(|frame| frame.duration_ms == 500 && frame.x == 0 && frame.y == 0 && frame.image.width == 2 && frame.image.height == 2 && frame.disposal == pix::Disposal::Keep));
		assert_eq!(animation.frames[0].blend, pix::Blend::Source);
		assert_eq!(animation.frames[1].blend, pix::Blend::Over);
		assert_ne!(animation.frames[0].image.pixels, animation.frames[1].image.pixels);
		let mut compositor = pix::Compositor::new(2, 2).unwrap();
		assert_eq!(decode(include_bytes!("../tests/animated.webp")).unwrap(), compositor.render(&animation.frames[0]).unwrap());
	}

	#[test]
	fn lossless_animation_round_trips_visual_frames_timing_and_loop() {
		let first = pix::RgbaImage::new(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
		let second = pix::RgbaImage::new(1, 1, vec![0, 255, 0, 128]).unwrap();
		let source = pix::Animation::new(
			2,
			1,
			7,
			vec![
				pix::Frame { image: first, x: 0, y: 0, duration_ms: 20, blend: pix::Blend::Source, disposal: pix::Disposal::Keep },
				pix::Frame { image: second, x: 1, y: 0, duration_ms: 30, blend: pix::Blend::Over, disposal: pix::Disposal::Previous },
			],
		)
		.unwrap();
		let mut compositor = pix::Compositor::new(2, 1).unwrap();
		let expected: Vec<pix::RgbaImage> = source.frames.iter().map(|frame| compositor.render(frame).unwrap()).collect();
		for effort in [50, 100] {
			let encoded = encode_animation(&source, effort).unwrap();
			let decoded = decode_animation(&encoded).unwrap();
			assert_eq!((decoded.width, decoded.height, decoded.loop_count), (2, 1, 7));
			assert_eq!(decoded.frames.iter().map(|frame| frame.duration_ms).collect::<Vec<_>>(), vec![20, 30]);
			assert_eq!(decoded.frames.into_iter().map(|frame| frame.image).collect::<Vec<_>>(), expected);
		}
	}

	#[test]
	fn animation_preserves_background_zero_duration_and_disposal() {
		let background = [9, 19, 29, 200];
		let source = pix::Animation::new_with_background(
			2,
			1,
			background,
			3,
			vec![
				pix::Frame { image: pix::RgbaImage::new(1, 1, vec![255, 0, 0, 255]).unwrap(), x: 0, y: 0, duration_ms: 0, blend: pix::Blend::Source, disposal: pix::Disposal::Background },
				pix::Frame { image: pix::RgbaImage::new(1, 1, vec![0, 255, 0, 255]).unwrap(), x: 1, y: 0, duration_ms: 30, blend: pix::Blend::Source, disposal: pix::Disposal::Keep },
			],
		)
		.unwrap();
		let mut compositor = pix::Compositor::new_with_background(2, 1, background).unwrap();
		let expected: Vec<pix::RgbaImage> = source.frames.iter().map(|frame| compositor.render(frame).unwrap()).collect();
		let encoded = encode_animation(&source, 100).unwrap();
		let anim = encoded.windows(4).position(|window| window == b"ANIM").unwrap();
		assert_eq!(&encoded[anim + 8..anim + 12], &[background[2], background[1], background[0], background[3]]);
		let decoded = decode_animation(&encoded).unwrap();
		assert_eq!(decoded.background, background);
		assert_eq!(decoded.frames.iter().map(|frame| frame.duration_ms).collect::<Vec<_>>(), vec![0, 30]);
		assert_eq!(decoded.frames.into_iter().map(|frame| frame.image).collect::<Vec<_>>(), expected);
		assert_eq!(decode(&encoded).unwrap(), expected[0]);
		let mut before_header = encoded.clone();
		insert_chunk(&mut before_header, 12, b"JUNK", &[]);
		assert_eq!(decode_animation(&before_header), Err(Error::Invalid));
		let mut between_frames = encoded.clone();
		let second_frame = top_level_offsets(&between_frames, b"ANMF")[1];
		insert_chunk(&mut between_frames, second_frame, b"EXIF", &[]);
		assert_eq!(decode_animation(&between_frames), Err(Error::Invalid));
	}

	#[test]
	fn animation_refuses_unrepresentable_loop_and_duration() {
		let image = pix::RgbaImage::new(1, 1, vec![0, 0, 0, 255]).unwrap();
		let loop_overflow = pix::Animation::new(1, 1, 65_536, vec![pix::Frame { image: image.clone(), x: 0, y: 0, duration_ms: 1, blend: pix::Blend::Source, disposal: pix::Disposal::Keep }]).unwrap();
		assert_eq!(encode_animation(&loop_overflow, 100), Err(Error::Unsupported));
		let duration_overflow = pix::Animation::new(1, 1, 1, vec![pix::Frame { image, x: 0, y: 0, duration_ms: 0x0100_0000, blend: pix::Blend::Source, disposal: pix::Disposal::Keep }]).unwrap();
		assert_eq!(encode_animation(&duration_overflow, 100), Err(Error::TooLarge));
	}

	#[test]
	fn animation_parser_rejects_truncation_and_out_of_canvas_frames() {
		let source = include_bytes!("../tests/animated.webp");
		assert_eq!(decode_animation(&source[..source.len() - 1]), Err(Error::Invalid));
		let mut outside = source.to_vec();
		outside[0x32..0x35].copy_from_slice(&[1, 0, 0]);
		assert_eq!(decode_animation(&outside), Err(Error::Invalid));
		let mut bad_size = source.to_vec();
		bad_size[0x30..0x34].copy_from_slice(&u32::MAX.to_le_bytes());
		assert_eq!(decode_animation(&bad_size), Err(Error::Invalid));
		let mut reserved_vp8x = source.to_vec();
		let vp8x = reserved_vp8x.windows(4).position(|window| window == b"VP8X").unwrap();
		reserved_vp8x[vp8x + 8] |= 1;
		assert_eq!(decode_animation(&reserved_vp8x), Err(Error::Invalid));
		let mut reserved_anmf = source.to_vec();
		let anmf = reserved_anmf.windows(4).position(|window| window == b"ANMF").unwrap();
		reserved_anmf[anmf + 8 + 15] |= 0x80;
		assert_eq!(decode_animation(&reserved_anmf), Err(Error::Invalid));
	}
}
