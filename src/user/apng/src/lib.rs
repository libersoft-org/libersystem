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

pub fn decode(data: &[u8]) -> Result<Animation, Error> {
	if data.get(..8) != Some(SIGNATURE) {
		return Err(if data.len() < 8 { Error::Truncated } else { Error::Invalid });
	}
	let mut cursor = 8usize;
	let mut canvas = None;
	let mut declared_frames = None;
	let mut loop_count = 0u32;
	let mut sequence = 0u32;
	let mut control = None;
	let mut compressed = Vec::new();
	let mut frames = Vec::new();
	let mut saw_idat = false;
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
		match kind {
			b"IHDR" => {
				if canvas.is_some() || body.len() != 13 || body[8..] != [8, 6, 0, 0, 0] {
					return Err(Error::Unsupported);
				}
				let width = read_u32(body, 0)?;
				let height = read_u32(body, 4)?;
				if width == 0 || height == 0 || width > pix::MAX_DIMENSION || height > pix::MAX_DIMENSION || width as u64 * height as u64 > pix::MAX_PIXELS {
					return Err(Error::TooLarge);
				}
				canvas = Some((width, height));
			}
			b"acTL" => {
				if declared_frames.is_some() || body.len() != 8 || saw_idat {
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
				finish_frame(&mut frames, control.take(), &mut compressed)?;
				if body.len() != 26 || read_u32(body, 0)? != sequence {
					return Err(Error::Invalid);
				}
				sequence = sequence.checked_add(1).ok_or(Error::TooLarge)?;
				control = Some(parse_control(body)?);
			}
			b"IDAT" => {
				if saw_idat || control.is_none() {
					return Err(Error::Invalid);
				}
				saw_idat = true;
				compressed.extend_from_slice(body);
			}
			b"fdAT" => {
				if body.len() < 4 || read_u32(body, 0)? != sequence || control.is_none() {
					return Err(Error::Invalid);
				}
				sequence = sequence.checked_add(1).ok_or(Error::TooLarge)?;
				compressed.extend_from_slice(&body[4..]);
			}
			b"IEND" => {
				finish_frame(&mut frames, control.take(), &mut compressed)?;
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
	Ok(Control { width: read_u32(body, 4)?, height: read_u32(body, 8)?, x: read_u32(body, 12)?, y: read_u32(body, 16)?, duration_ms: duration_ms.max(1), blend, disposal })
}

fn finish_frame(frames: &mut Vec<Frame>, control: Option<Control>, compressed: &mut Vec<u8>) -> Result<(), Error> {
	let Some(control) = control else {
		if compressed.is_empty() {
			return Ok(());
		}
		return Err(Error::Invalid);
	};
	if compressed.is_empty() {
		return Err(Error::Invalid);
	}
	let image = png::decode_rgba_payload(control.width, control.height, compressed).map_err(map_png)?;
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

	#[test]
	fn frame_rect_timing_blend_disposal_and_loop_round_trip() {
		let first = pix::RgbaImage::new(2, 2, vec![1; 16]).unwrap();
		let second = pix::RgbaImage::new(1, 2, vec![2; 8]).unwrap();
		let animation = Animation::new(
			2,
			2,
			3,
			vec![
				Frame { image: first, x: 0, y: 0, duration_ms: 20, blend: Blend::Source, disposal: Disposal::Keep },
				Frame { image: second, x: 1, y: 0, duration_ms: 35, blend: Blend::Over, disposal: Disposal::Previous },
			],
		)
		.unwrap();
		assert_eq!(decode(&encode(&animation, 100).unwrap()).unwrap(), animation);
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
}
