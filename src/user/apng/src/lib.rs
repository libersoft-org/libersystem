#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use pix::{Animation, Blend, Disposal, Frame};

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Truncated,
	Invalid,
	Unsupported,
	TooLarge,
}

#[derive(Clone, Copy)]
struct Control {
	width: u32,
	height: u32,
	x: u32,
	y: u32,
	duration_ms: u32,
	blend: Blend,
	disposal: Disposal,
}

struct PngProfile {
	ihdr_tail: [u8; 5],
	palette: Option<Vec<u8>>,
	transparency: Option<Vec<u8>>,
}

pub fn decode(data: &[u8]) -> Result<Animation, Error> {
	if data.get(..8) != Some(SIGNATURE) {
		return Err(if data.len() < 8 { Error::Truncated } else { Error::Invalid });
	}
	let mut cursor = 8usize;
	let mut canvas = None;
	let mut profile = None;
	let mut declared_frames = None;
	let mut loop_count = 0u32;
	let mut sequence = 0u32;
	let mut control = None;
	let mut compressed = Vec::new();
	let mut frames = Vec::new();
	let mut idat_frame = None;
	let mut idat_closed = false;
	while cursor < data.len() {
		let length = read_u32(data, cursor)? as usize;
		let kind: &[u8; 4] = data.get(cursor + 4..cursor + 8).ok_or(Error::Truncated)?.try_into().map_err(|_| Error::Truncated)?;
		let start = cursor.checked_add(8).ok_or(Error::TooLarge)?;
		let end = start.checked_add(length).ok_or(Error::TooLarge)?;
		let crc_end = end.checked_add(4).ok_or(Error::TooLarge)?;
		let body = data.get(start..end).ok_or(Error::Truncated)?;
		if crc32(kind.iter().chain(body.iter()).copied()) != read_u32(data, end)? {
			return Err(Error::Invalid);
		}
		if idat_frame.is_some() && kind != b"IDAT" {
			idat_closed = true;
		}
		match kind {
			b"IHDR" => {
				if canvas.is_some() || body.len() != 13 {
					return Err(Error::Invalid);
				}
				let width = read_u32(body, 0)?;
				let height = read_u32(body, 4)?;
				if width == 0 || height == 0 || width > pix::MAX_DIMENSION || height > pix::MAX_DIMENSION || width as u64 * height as u64 > pix::MAX_PIXELS {
					return Err(Error::TooLarge);
				}
				canvas = Some((width, height));
				profile = Some(PngProfile { ihdr_tail: body[8..13].try_into().map_err(|_| Error::Invalid)?, palette: None, transparency: None });
			}
			b"PLTE" => {
				let profile = profile.as_mut().ok_or(Error::Invalid)?;
				if profile.palette.is_some() || idat_frame.is_some() {
					return Err(Error::Invalid);
				}
				profile.palette = Some(body.to_vec());
			}
			b"tRNS" => {
				let profile = profile.as_mut().ok_or(Error::Invalid)?;
				if profile.transparency.is_some() || idat_frame.is_some() {
					return Err(Error::Invalid);
				}
				profile.transparency = Some(body.to_vec());
			}
			b"acTL" => {
				if profile.is_none() || declared_frames.is_some() || body.len() != 8 || idat_frame.is_some() {
					return Err(Error::Invalid);
				}
				let count = read_u32(body, 0)?;
				if count == 0 || count as usize > pix::MAX_ANIMATION_FRAMES {
					return Err(Error::TooLarge);
				}
				declared_frames = Some(count);
				loop_count = read_u32(body, 4)?;
			}
			b"fcTL" => {
				finish_frame(&mut frames, control.take(), &mut compressed, profile.as_ref().ok_or(Error::Invalid)?)?;
				if body.len() != 26 || read_u32(body, 0)? != sequence {
					return Err(Error::Invalid);
				}
				sequence = sequence.checked_add(1).ok_or(Error::TooLarge)?;
				let next = parse_control(body)?;
				if idat_frame.is_none() {
					let (width, height) = canvas.ok_or(Error::Invalid)?;
					if next.x != 0 || next.y != 0 || next.width != width || next.height != height {
						return Err(Error::Invalid);
					}
				}
				control = Some(next);
			}
			b"IDAT" => {
				let belongs_to_animation = control.is_some();
				if idat_closed || idat_frame.is_some_and(|expected| expected != belongs_to_animation) {
					return Err(Error::Invalid);
				}
				idat_frame = Some(belongs_to_animation);
				if belongs_to_animation {
					compressed.extend_from_slice(body);
				}
			}
			b"fdAT" => {
				if body.len() < 4 || read_u32(body, 0)? != sequence || control.is_none() || idat_frame.is_none() {
					return Err(Error::Invalid);
				}
				sequence = sequence.checked_add(1).ok_or(Error::TooLarge)?;
				compressed.extend_from_slice(&body[4..]);
			}
			b"IEND" => {
				if !body.is_empty() {
					return Err(Error::Invalid);
				}
				finish_frame(&mut frames, control.take(), &mut compressed, profile.as_ref().ok_or(Error::Invalid)?)?;
				cursor = crc_end;
				break;
			}
			_ if kind[0] & 0x20 == 0 => return Err(Error::Unsupported),
			_ => {}
		}
		cursor = crc_end;
	}
	if cursor != data.len() || declared_frames != Some(frames.len() as u32) {
		return Err(Error::Invalid);
	}
	let (width, height) = canvas.ok_or(Error::Invalid)?;
	Animation::new(width, height, loop_count, frames).map_err(map_pix)
}

pub fn encode(animation: &Animation, compression: u8) -> Result<Vec<u8>, Error> {
	if animation.background != [0; 4] {
		return Err(Error::Unsupported);
	}
	let validated = Animation::new(animation.width, animation.height, animation.loop_count, animation.frames.clone()).map_err(map_pix)?;
	let mut output = SIGNATURE.to_vec();
	let mut header = Vec::new();
	header.extend_from_slice(&validated.width.to_be_bytes());
	header.extend_from_slice(&validated.height.to_be_bytes());
	header.extend_from_slice(&[8, 6, 0, 0, 0]);
	chunk(&mut output, b"IHDR", &header)?;
	let mut animation_header = Vec::new();
	animation_header.extend_from_slice(&(validated.frames.len() as u32).to_be_bytes());
	animation_header.extend_from_slice(&validated.loop_count.to_be_bytes());
	chunk(&mut output, b"acTL", &animation_header)?;
	let mut sequence = 0u32;
	for (index, frame) in validated.frames.iter().enumerate() {
		let duration = u16::try_from(frame.duration_ms).map_err(|_| Error::Unsupported)?;
		let mut control = Vec::new();
		control.extend_from_slice(&sequence.to_be_bytes());
		sequence += 1;
		control.extend_from_slice(&frame.image.width.to_be_bytes());
		control.extend_from_slice(&frame.image.height.to_be_bytes());
		control.extend_from_slice(&frame.x.to_be_bytes());
		control.extend_from_slice(&frame.y.to_be_bytes());
		control.extend_from_slice(&duration.to_be_bytes());
		control.extend_from_slice(&1_000u16.to_be_bytes());
		control.push(match frame.disposal {
			Disposal::Keep => 0,
			Disposal::Background => 1,
			Disposal::Previous => 2,
		});
		control.push(match frame.blend {
			Blend::Source => 0,
			Blend::Over => 1,
		});
		chunk(&mut output, b"fcTL", &control)?;
		let payload = png::encode_rgba_payload(&frame.image, compression).map_err(map_png)?;
		if index == 0 {
			chunk(&mut output, b"IDAT", &payload)?;
		} else {
			let mut body = Vec::new();
			body.extend_from_slice(&sequence.to_be_bytes());
			sequence += 1;
			body.extend_from_slice(&payload);
			chunk(&mut output, b"fdAT", &body)?;
		}
	}
	chunk(&mut output, b"IEND", &[])?;
	Ok(output)
}

fn parse_control(body: &[u8]) -> Result<Control, Error> {
	let numerator = u16::from_be_bytes([body[20], body[21]]) as u32;
	let denominator = match u16::from_be_bytes([body[22], body[23]]) as u32 {
		0 => 100,
		value => value,
	};
	let duration_ms = (numerator as u64).checked_mul(1_000).ok_or(Error::TooLarge)?.div_ceil(denominator as u64) as u32;
	let disposal = match body[24] {
		0 => Disposal::Keep,
		1 => Disposal::Background,
		2 => Disposal::Previous,
		_ => return Err(Error::Invalid),
	};
	let blend = match body[25] {
		0 => Blend::Source,
		1 => Blend::Over,
		_ => return Err(Error::Invalid),
	};
	Ok(Control { width: read_u32(body, 4)?, height: read_u32(body, 8)?, x: read_u32(body, 12)?, y: read_u32(body, 16)?, duration_ms, blend, disposal })
}

fn finish_frame(frames: &mut Vec<Frame>, control: Option<Control>, compressed: &mut Vec<u8>, profile: &PngProfile) -> Result<(), Error> {
	let Some(control) = control else {
		if compressed.is_empty() {
			return Ok(());
		}
		return Err(Error::Invalid);
	};
	if compressed.is_empty() {
		return Err(Error::Invalid);
	}
	let mut encoded = SIGNATURE.to_vec();
	let mut header = Vec::new();
	header.extend_from_slice(&control.width.to_be_bytes());
	header.extend_from_slice(&control.height.to_be_bytes());
	header.extend_from_slice(&profile.ihdr_tail);
	chunk(&mut encoded, b"IHDR", &header)?;
	if let Some(palette) = &profile.palette {
		chunk(&mut encoded, b"PLTE", palette)?;
	}
	if let Some(transparency) = &profile.transparency {
		chunk(&mut encoded, b"tRNS", transparency)?;
	}
	chunk(&mut encoded, b"IDAT", compressed)?;
	chunk(&mut encoded, b"IEND", &[])?;
	let image = png::decode_rgba(&encoded).map_err(map_png)?;
	frames.push(Frame { image, x: control.x, y: control.y, duration_ms: control.duration_ms, blend: control.blend, disposal: control.disposal });
	compressed.clear();
	Ok(())
}

fn chunk(output: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) -> Result<(), Error> {
	output.extend_from_slice(&u32::try_from(body.len()).map_err(|_| Error::TooLarge)?.to_be_bytes());
	output.extend_from_slice(kind);
	output.extend_from_slice(body);
	output.extend_from_slice(&crc32(kind.iter().chain(body.iter()).copied()).to_be_bytes());
	Ok(())
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, Error> {
	Ok(u32::from_be_bytes(data.get(offset..offset + 4).ok_or(Error::Truncated)?.try_into().map_err(|_| Error::Truncated)?))
}

fn crc32(bytes: impl Iterator<Item = u8>) -> u32 {
	let mut crc = u32::MAX;
	for byte in bytes {
		crc ^= byte as u32;
		for _ in 0..8 {
			crc = if crc & 1 != 0 { crc >> 1 ^ 0xedb8_8320 } else { crc >> 1 };
		}
	}
	!crc
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
mod tests {
	use super::*;
	use alloc::vec;

	fn fnv1a(bytes: &[u8]) -> u64 {
		bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
	}

	fn displayed_hashes(animation: &Animation) -> Vec<u64> {
		let mut compositor = pix::Compositor::new_with_background(animation.width, animation.height, animation.background).unwrap();
		animation.frames.iter().map(|frame| fnv1a(&compositor.render(frame).unwrap().pixels)).collect()
	}

	fn png_chunks(data: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
		let mut chunks = Vec::new();
		let mut cursor = 8usize;
		while cursor < data.len() {
			let length = read_u32(data, cursor).unwrap() as usize;
			let kind = data[cursor + 4..cursor + 8].try_into().unwrap();
			let start = cursor + 8;
			chunks.push((kind, data[start..start + length].to_vec()));
			cursor = start + length + 4;
		}
		chunks
	}

	fn control(sequence: u32, width: u32, height: u32) -> Vec<u8> {
		let mut body = Vec::new();
		body.extend_from_slice(&sequence.to_be_bytes());
		body.extend_from_slice(&width.to_be_bytes());
		body.extend_from_slice(&height.to_be_bytes());
		body.extend_from_slice(&[0; 8]);
		body.extend_from_slice(&1u16.to_be_bytes());
		body.extend_from_slice(&1_000u16.to_be_bytes());
		body.extend_from_slice(&[0, 0]);
		body
	}

	#[test]
	fn frame_rect_timing_blend_disposal_and_loop_round_trip() {
		let first = pix::RgbaImage::new(2, 2, vec![1; 16]).unwrap();
		let second = pix::RgbaImage::new(1, 2, vec![2; 8]).unwrap();
		let animation = Animation::new(
			2,
			2,
			3,
			vec![
				Frame { image: first, x: 0, y: 0, duration_ms: 0, blend: Blend::Source, disposal: Disposal::Keep },
				Frame { image: second, x: 1, y: 0, duration_ms: 35, blend: Blend::Over, disposal: Disposal::Previous },
			],
		)
		.unwrap();
		assert_eq!(decode(&encode(&animation, 100).unwrap()).unwrap(), animation);
		let mut unsupported_background = animation;
		unsupported_background.background = [1, 2, 3, 255];
		assert_eq!(encode(&unsupported_background, 100), Err(Error::Unsupported));
	}

	#[test]
	fn compression_endpoints_preserve_frames_and_exercise_distinct_streams() {
		let image = |seed: u32| {
			let mut pixels = Vec::new();
			for y in 0..19u32 {
				for x in 0..31u32 {
					pixels.extend_from_slice(&[
						((x * 17 + y * 3 + seed) & 255) as u8,
						((x * 5 + y * 23 + seed * 2) & 255) as u8,
						((x * 11 + y * 7 + seed * 3) & 255) as u8,
						((x * 9 + y * 13 + seed * 5) & 255) as u8,
					]);
				}
			}
			pix::RgbaImage::new(31, 19, pixels).unwrap()
		};
		let animation = Animation::new(
			31,
			19,
			3,
			vec![
				Frame { image: image(1), x: 0, y: 0, duration_ms: 40, blend: Blend::Source, disposal: Disposal::Keep },
				Frame { image: image(7), x: 0, y: 0, duration_ms: 75, blend: Blend::Source, disposal: Disposal::Previous },
			],
		)
		.unwrap();
		let fast = encode(&animation, 0).unwrap();
		let compact = encode(&animation, 100).unwrap();
		assert_ne!(fast, compact, "APNG compression endpoints must exercise distinct deflate streams");
		assert_eq!(decode(&fast).unwrap(), animation);
		assert_eq!(decode(&compact).unwrap(), animation);
	}

	#[test]
	fn decodes_indexed_frame_split_across_multiple_idat_chunks() {
		let image = pix::RgbaImage::new(2, 1, vec![255, 0, 0, 0, 0, 255, 0, 255]).unwrap();
		let source = png::encode_indexed(&image, 0, 100).unwrap();
		let expected = png::decode_rgba(&source).unwrap();
		let chunks = png_chunks(&source);
		let mut encoded = SIGNATURE.to_vec();
		for kind in [b"IHDR", b"PLTE", b"tRNS"] {
			if let Some((_, body)) = chunks.iter().find(|(candidate, _)| candidate == kind) {
				chunk(&mut encoded, kind, body).unwrap();
			}
		}
		let mut animation_header = Vec::new();
		animation_header.extend_from_slice(&1u32.to_be_bytes());
		animation_header.extend_from_slice(&0u32.to_be_bytes());
		chunk(&mut encoded, b"acTL", &animation_header).unwrap();
		chunk(&mut encoded, b"fcTL", &control(0, 2, 1)).unwrap();
		let (_, payload) = chunks.iter().find(|(kind, _)| kind == b"IDAT").unwrap();
		let split = payload.len() / 2;
		chunk(&mut encoded, b"IDAT", &payload[..split]).unwrap();
		let second_idat = encoded.len();
		chunk(&mut encoded, b"IDAT", &payload[split..]).unwrap();
		chunk(&mut encoded, b"IEND", &[]).unwrap();
		let mut nonconsecutive = encoded.clone();
		let mut ancillary = Vec::new();
		chunk(&mut ancillary, b"tEXt", b"key\0value").unwrap();
		nonconsecutive.splice(second_idat..second_idat, ancillary);
		assert_eq!(decode(&nonconsecutive), Err(Error::Invalid));

		let animation = decode(&encoded).unwrap();
		assert_eq!(animation.frames.len(), 1);
		assert_eq!(animation.frames[0].image, expected);
	}

	#[test]
	fn decodes_animation_whose_static_image_is_not_a_frame() {
		let static_image = pix::RgbaImage::new(1, 1, vec![1, 2, 3, 255]).unwrap();
		let frame = pix::RgbaImage::new(1, 1, vec![4, 5, 6, 128]).unwrap();
		let source = png::encode_rgba(&static_image, png::EncodeOptions::default()).unwrap();
		let chunks = png_chunks(&source);
		let mut encoded = SIGNATURE.to_vec();
		let (_, header) = chunks.iter().find(|(kind, _)| kind == b"IHDR").unwrap();
		chunk(&mut encoded, b"IHDR", header).unwrap();
		let mut animation_header = Vec::new();
		animation_header.extend_from_slice(&1u32.to_be_bytes());
		animation_header.extend_from_slice(&0u32.to_be_bytes());
		chunk(&mut encoded, b"acTL", &animation_header).unwrap();
		let (_, default_payload) = chunks.iter().find(|(kind, _)| kind == b"IDAT").unwrap();
		chunk(&mut encoded, b"IDAT", default_payload).unwrap();
		chunk(&mut encoded, b"fcTL", &control(0, 1, 1)).unwrap();
		let mut frame_data = 1u32.to_be_bytes().to_vec();
		frame_data.extend_from_slice(&png::encode_rgba_payload(&frame, 50).unwrap());
		chunk(&mut encoded, b"fdAT", &frame_data).unwrap();
		chunk(&mut encoded, b"IEND", &[]).unwrap();

		let animation = decode(&encoded).unwrap();
		assert_eq!(animation.frames.len(), 1);
		assert_eq!(animation.frames[0].image, frame);
	}

	#[test]
	fn rejects_static_png_and_corrupt_sequence() {
		let static_png = png::encode_rgba(&pix::RgbaImage::new(1, 1, vec![0; 4]).unwrap(), png::EncodeOptions::default()).unwrap();
		assert!(decode(&static_png).is_err());
		let animation = Animation::still(pix::RgbaImage::new(1, 1, vec![0; 4]).unwrap());
		let mut encoded = encode(&animation, 50).unwrap();
		let fctl = encoded.windows(4).position(|window| window == b"fcTL").unwrap();
		encoded[fctl + 4] ^= 1;
		assert_eq!(decode(&encoded), Err(Error::Invalid));
	}

	#[test]
	fn decodes_external_apng_and_separate_default_image() {
		let animation = decode(include_bytes!("../tests/data/external-animation.png")).unwrap();
		assert_eq!((animation.width, animation.height, animation.loop_count, animation.frames.len()), (31, 19, 2, 3));
		assert!(animation.frames.iter().all(|frame| frame.duration_ms == 60));
		assert_eq!(displayed_hashes(&animation), vec![0x5fad_a2ef_c37e_917f, 0xfa2f_ff14_7b88_5f15, 0x566d_1dac_d369_b6ab]);

		let separate = decode(include_bytes!("../tests/data/external-separate-default.png")).unwrap();
		assert_eq!((separate.width, separate.height, separate.loop_count, separate.frames.len()), (31, 19, 2, 2));
		assert!(separate.frames.iter().all(|frame| frame.duration_ms == 60));
		assert_eq!(displayed_hashes(&separate), vec![0xfa2f_ff14_7b88_5f15, 0x566d_1dac_d369_b6ab]);
	}
}
