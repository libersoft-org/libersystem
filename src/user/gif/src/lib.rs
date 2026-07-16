#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use pix::{Animation, Blend, Disposal, Frame};
use weezl::{BitOrder, decode::Decoder, encode::Encoder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Truncated,
	Invalid,
	Unsupported,
	TooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodeOptions {
	pub quality: u8,
	pub dither: bool,
	pub alpha_threshold: u8,
}

#[derive(Clone, Copy)]
struct GraphicsControl {
	delay: u16,
	disposal: Disposal,
	transparent: Option<u8>,
}

pub fn decode(data: &[u8]) -> Result<Animation, Error> {
	if !matches!(data.get(..6), Some(b"GIF87a") | Some(b"GIF89a")) {
		return Err(if data.len() < 6 { Error::Truncated } else { Error::Invalid });
	}
	let screen = data.get(6..13).ok_or(Error::Truncated)?;
	let width = u16::from_le_bytes([screen[0], screen[1]]) as u32;
	let height = u16::from_le_bytes([screen[2], screen[3]]) as u32;
	if width == 0 || height == 0 {
		return Err(Error::Invalid);
	}
	if width > pix::MAX_DIMENSION || height > pix::MAX_DIMENSION || width as u64 * height as u64 > pix::MAX_PIXELS {
		return Err(Error::TooLarge);
	}
	let mut cursor = 13usize;
	let global = if screen[4] & 0x80 != 0 {
		let count = 1usize << ((screen[4] & 7) + 1);
		Some(read_palette(data, &mut cursor, count)?)
	} else {
		None
	};
	let mut control = GraphicsControl { delay: 1, disposal: Disposal::Keep, transparent: None };
	let mut loop_count = 1u32;
	let mut frames = Vec::new();
	loop {
		match *data.get(cursor).ok_or(Error::Truncated)? {
			0x21 => {
				cursor += 1;
				let label = *data.get(cursor).ok_or(Error::Truncated)?;
				cursor += 1;
				if label == 0xf9 {
					let body = data.get(cursor..cursor + 6).ok_or(Error::Truncated)?;
					if body[0] != 4 || body[5] != 0 {
						return Err(Error::Invalid);
					}
					control.delay = u16::from_le_bytes([body[2], body[3]]).max(1);
					control.disposal = match (body[1] >> 2) & 7 {
						0 | 1 => Disposal::Keep,
						2 => Disposal::Background,
						3 => Disposal::Previous,
						_ => return Err(Error::Unsupported),
					};
					control.transparent = (body[1] & 1 != 0).then_some(body[4]);
					cursor += 6;
				} else if label == 0xff {
					let length = *data.get(cursor).ok_or(Error::Truncated)? as usize;
					cursor += 1;
					let application = data.get(cursor..cursor + length).ok_or(Error::Truncated)?;
					cursor += length;
					let blocks = read_subblocks(data, &mut cursor)?;
					if application == b"NETSCAPE2.0" && blocks.len() >= 3 && blocks[0] == 1 {
						loop_count = u16::from_le_bytes([blocks[1], blocks[2]]) as u32;
					}
				} else {
					let _ = read_subblocks(data, &mut cursor)?;
				}
			}
			0x2c => {
				let descriptor = data.get(cursor + 1..cursor + 10).ok_or(Error::Truncated)?;
				cursor += 10;
				let x = u16::from_le_bytes([descriptor[0], descriptor[1]]) as u32;
				let y = u16::from_le_bytes([descriptor[2], descriptor[3]]) as u32;
				let frame_width = u16::from_le_bytes([descriptor[4], descriptor[5]]) as u32;
				let frame_height = u16::from_le_bytes([descriptor[6], descriptor[7]]) as u32;
				if frame_width == 0 || frame_height == 0 || frame_width as u64 * frame_height as u64 > pix::MAX_PIXELS {
					return Err(Error::TooLarge);
				}
				let packed = descriptor[8];
				let palette = if packed & 0x80 != 0 {
					let count = 1usize << ((packed & 7) + 1);
					read_palette(data, &mut cursor, count)?
				} else {
					global.clone().ok_or(Error::Invalid)?
				};
				let minimum = *data.get(cursor).ok_or(Error::Truncated)?;
				cursor += 1;
				if !(2..=8).contains(&minimum) {
					return Err(Error::Invalid);
				}
				let compressed = read_subblocks(data, &mut cursor)?;
				let indices = Decoder::new(BitOrder::Lsb, minimum).decode(&compressed).map_err(|_| Error::Invalid)?;
				let count = frame_width as usize * frame_height as usize;
				if indices.len() != count {
					return Err(Error::Invalid);
				}
				let indices = if packed & 0x40 != 0 { deinterlace(&indices, frame_width as usize, frame_height as usize) } else { indices };
				let mut pixels = Vec::new();
				pixels.try_reserve_exact(count * 4).map_err(|_| Error::TooLarge)?;
				for index in indices {
					let color = *palette.get(index as usize).ok_or(Error::Invalid)?;
					pixels.extend_from_slice(&[color[0], color[1], color[2], if control.transparent == Some(index) { 0 } else { 255 }]);
				}
				let image = pix::RgbaImage::new(frame_width, frame_height, pixels).map_err(map_pix)?;
				frames.push(Frame { image, x, y, duration_ms: control.delay as u32 * 10, blend: Blend::Over, disposal: control.disposal });
				control = GraphicsControl { delay: 1, disposal: Disposal::Keep, transparent: None };
			}
			0x3b => {
				cursor += 1;
				break;
			}
			_ => return Err(Error::Invalid),
		}
	}
	if cursor != data.len() {
		return Err(Error::Invalid);
	}
	Animation::new(width, height, loop_count, frames).map_err(map_pix)
}

pub fn encode(animation: &Animation) -> Result<Vec<u8>, Error> {
	encode_with_options(animation, EncodeOptions { quality: 100, dither: true, alpha_threshold: 128 })
}

pub fn encode_with_options(animation: &Animation, options: EncodeOptions) -> Result<Vec<u8>, Error> {
	let validated = Animation::new(animation.width, animation.height, animation.loop_count, animation.frames.clone()).map_err(map_pix)?;
	if validated.width > u16::MAX as u32 || validated.height > u16::MAX as u32 || validated.loop_count > u16::MAX as u32 {
		return Err(Error::TooLarge);
	}
	let images: Vec<_> = validated.frames.iter().map(|frame| frame.image.as_rgba()).collect();
	let palette = quantize::build_palette(&images, quantize::Options { quality: options.quality, dither: options.dither, alpha_threshold: options.alpha_threshold }).map_err(map_quantize)?;
	let table_size = palette.colors.len().max(2).next_power_of_two();
	let size_bits = table_size.trailing_zeros() as u8 - 1;
	let minimum = (size_bits + 1).max(2);
	let mut output = b"GIF89a".to_vec();
	output.extend_from_slice(&(validated.width as u16).to_le_bytes());
	output.extend_from_slice(&(validated.height as u16).to_le_bytes());
	output.push(0x80 | 0x70 | size_bits);
	output.extend_from_slice(&[0, 0]);
	for index in 0..table_size {
		let color = palette.colors.get(index).copied().unwrap_or([0; 4]);
		output.extend_from_slice(&color[..3]);
	}
	output.extend_from_slice(b"\x21\xff\x0bNETSCAPE2.0\x03\x01");
	output.extend_from_slice(&(validated.loop_count as u16).to_le_bytes());
	output.push(0);
	let transparent = palette.transparent_index;
	for frame in &validated.frames {
		let delay = u16::try_from(frame.duration_ms.div_ceil(10)).map_err(|_| Error::TooLarge)?.max(1);
		let disposal = match frame.disposal {
			Disposal::Keep => 1,
			Disposal::Background => 2,
			Disposal::Previous => 3,
		};
		output.extend_from_slice(&[0x21, 0xf9, 4, disposal << 2 | u8::from(transparent.is_some())]);
		output.extend_from_slice(&delay.to_le_bytes());
		output.extend_from_slice(&[transparent.unwrap_or(0), 0]);
		output.push(0x2c);
		output.extend_from_slice(&(frame.x as u16).to_le_bytes());
		output.extend_from_slice(&(frame.y as u16).to_le_bytes());
		output.extend_from_slice(&(frame.image.width as u16).to_le_bytes());
		output.extend_from_slice(&(frame.image.height as u16).to_le_bytes());
		output.push(0);
		let indices = quantize::map_image(frame.image.as_rgba(), &palette).map_err(map_quantize)?;
		let compressed = Encoder::new(BitOrder::Lsb, minimum).encode(&indices).map_err(|_| Error::Invalid)?;
		output.push(minimum);
		write_subblocks(&mut output, &compressed);
	}
	output.push(0x3b);
	Ok(output)
}

fn read_palette(data: &[u8], cursor: &mut usize, count: usize) -> Result<Vec<[u8; 3]>, Error> {
	let length = count.checked_mul(3).ok_or(Error::TooLarge)?;
	let bytes = data.get(*cursor..*cursor + length).ok_or(Error::Truncated)?;
	*cursor += length;
	Ok(bytes.chunks_exact(3).map(|color| [color[0], color[1], color[2]]).collect())
}

fn read_subblocks(data: &[u8], cursor: &mut usize) -> Result<Vec<u8>, Error> {
	let mut output = Vec::new();
	loop {
		let length = *data.get(*cursor).ok_or(Error::Truncated)? as usize;
		*cursor += 1;
		if length == 0 {
			break;
		}
		let end = cursor.checked_add(length).ok_or(Error::TooLarge)?;
		output.extend_from_slice(data.get(*cursor..end).ok_or(Error::Truncated)?);
		*cursor = end;
	}
	Ok(output)
}

fn write_subblocks(output: &mut Vec<u8>, data: &[u8]) {
	for block in data.chunks(255) {
		output.push(block.len() as u8);
		output.extend_from_slice(block);
	}
	output.push(0);
}

fn deinterlace(input: &[u8], width: usize, height: usize) -> Vec<u8> {
	let mut output = alloc::vec![0; input.len()];
	let mut source_row = 0usize;
	for (start, step) in [(0, 8), (4, 8), (2, 4), (1, 2)] {
		for y in (start..height).step_by(step) {
			output[y * width..(y + 1) * width].copy_from_slice(&input[source_row * width..(source_row + 1) * width]);
			source_row += 1;
		}
	}
	output
}

fn map_pix(error: pix::Error) -> Error {
	match error {
		pix::Error::Invalid => Error::Invalid,
		pix::Error::TooLarge => Error::TooLarge,
	}
}

fn map_quantize(error: quantize::Error) -> Error {
	match error {
		quantize::Error::Invalid => Error::Invalid,
		quantize::Error::TooLarge => Error::TooLarge,
	}
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
	use super::*;
	use alloc::vec;

	#[test]
	fn timing_loop_disposal_and_binary_alpha_round_trip() {
		let first = pix::RgbaImage::new(2, 1, vec![255, 0, 0, 255, 0, 0, 0, 0]).unwrap();
		let second = pix::RgbaImage::new(1, 1, vec![0, 255, 0, 255]).unwrap();
		let animation = Animation::new(
			2,
			1,
			5,
			vec![
				Frame { image: first, x: 0, y: 0, duration_ms: 20, blend: Blend::Over, disposal: Disposal::Background },
				Frame { image: second, x: 1, y: 0, duration_ms: 30, blend: Blend::Over, disposal: Disposal::Previous },
			],
		)
		.unwrap();
		assert_eq!(decode(&encode(&animation).unwrap()).unwrap(), animation);
	}

	#[test]
	fn quantizes_partial_alpha_and_more_than_256_exact_colors() {
		let partial = Animation::still(pix::RgbaImage::new(1, 1, vec![1, 2, 3, 4]).unwrap());
		let partial = decode(&encode(&partial).unwrap()).unwrap();
		assert_eq!(partial.frames[0].image.pixels, vec![0, 0, 0, 0]);
		let mut pixels = Vec::new();
		for value in 0..257u16 {
			pixels.extend_from_slice(&[(value & 255) as u8, (value >> 8) as u8, 0, 255]);
		}
		let many = Animation::still(pix::RgbaImage::new(257, 1, pixels).unwrap());
		let decoded = decode(&encode(&many).unwrap()).unwrap();
		assert_eq!(decoded.frames[0].image.width, 257);
		assert!(decoded.frames[0].image.pixels.chunks_exact(4).all(|pixel| pixel[3] == 255));
	}

	#[test]
	fn quality_changes_palette_budget() {
		let mut pixels = Vec::new();
		for value in 0..1024u32 {
			pixels.extend_from_slice(&[(value & 255) as u8, ((value * 37) & 255) as u8, ((value * 91) & 255) as u8, 255]);
		}
		let animation = Animation::still(pix::RgbaImage::new(32, 32, pixels).unwrap());
		let low = encode_with_options(&animation, EncodeOptions { quality: 0, dither: true, alpha_threshold: 128 }).unwrap();
		let high = encode_with_options(&animation, EncodeOptions { quality: 100, dither: true, alpha_threshold: 128 }).unwrap();
		assert!(low.len() < high.len());
		assert_eq!(decode(&low).unwrap().frames[0].image.width, 32);
		assert_eq!(decode(&high).unwrap().frames[0].image.width, 32);
	}
}
