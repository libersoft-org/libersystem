#![no_std]

#[cfg(test)]
extern crate std;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
	pub x: u32,
	pub y: u32,
	pub width: u32,
	pub height: u32,
}

pub struct Image<'a> {
	pub data: &'a [u8],
	pub width: u32,
	pub height: u32,
	pub pitch: u32,
}

pub struct Target<'a> {
	pub data: &'a mut [u8],
	pub width: u32,
	pub height: u32,
	pub pitch: u32,
	pub bytes_per_pixel: u32,
	pub red_shift: u8,
	pub red_size: u8,
	pub green_shift: u8,
	pub green_size: u8,
	pub blue_shift: u8,
	pub blue_size: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlitResult {
	pub rect: Rect,
	pub pixels: u64,
	pub direct: bool,
}

pub fn blit(source: Image<'_>, mut target: Target<'_>, damage: Rect, first: bool) -> Option<BlitResult> {
	validate(&source, &target, damage)?;
	let direct = source.width == target.width && source.height == target.height && target.bytes_per_pixel == 4 && target.red_shift == 16 && target.red_size == 8 && target.green_shift == 8 && target.green_size == 8 && target.blue_shift == 0 && target.blue_size == 8;
	if direct {
		let rect = if first { Rect { x: 0, y: 0, width: source.width, height: source.height } } else { damage };
		let bytes = rect.width as usize * 4;
		for row in rect.y..rect.y + rect.height {
			let src = row as usize * source.pitch as usize + rect.x as usize * 4;
			let dst = row as usize * target.pitch as usize + rect.x as usize * 4;
			target.data[dst..dst + bytes].copy_from_slice(&source.data[src..src + bytes]);
		}
		return Some(BlitResult { rect, pixels: rect.width as u64 * rect.height as u64, direct: true });
	}

	let sw = source.width as u64;
	let sh = source.height as u64;
	let dw = target.width as u64;
	let dh = target.height as u64;
	let width_limited = dw.saturating_mul(sh) <= dh.saturating_mul(sw);
	let (out_width, out_height) = if width_limited { (target.width, ((sh * dw) / sw).max(1) as u32) } else { (((sw * dh) / sh).max(1) as u32, target.height) };
	let offset_x = (target.width - out_width) / 2;
	let offset_y = (target.height - out_height) / 2;
	let (x0, y0, x1, y1) = if first {
		target.data.fill(0);
		(0, 0, out_width, out_height)
	} else {
		let end_x = (damage.x + damage.width) as u64 * out_width as u64;
		let end_y = (damage.y + damage.height) as u64 * out_height as u64;
		((damage.x as u64 * out_width as u64 / sw) as u32, (damage.y as u64 * out_height as u64 / sh) as u32, end_x.div_ceil(sw) as u32, end_y.div_ceil(sh) as u32)
	};
	for output_y in y0..y1 {
		let source_y = (output_y as u64 * source.height as u64 / out_height as u64) as u32;
		for output_x in x0..x1 {
			let source_x = (output_x as u64 * source.width as u64 / out_width as u64) as u32;
			let source_offset = source_y as usize * source.pitch as usize + source_x as usize * 4;
			let pixel = u32::from_le_bytes(source.data[source_offset..source_offset + 4].try_into().ok()?);
			write_pixel(&mut target, offset_x + output_x, offset_y + output_y, pixel);
		}
	}
	let width = x1 - x0;
	let height = y1 - y0;
	let written = width as u64 * height as u64 + if first { target.width as u64 * target.height as u64 } else { 0 };
	let rect = if first { Rect { x: 0, y: 0, width: target.width, height: target.height } } else { Rect { x: offset_x + x0, y: offset_y + y0, width, height } };
	Some(BlitResult { rect, pixels: written, direct: false })
}

fn validate(source: &Image<'_>, target: &Target<'_>, damage: Rect) -> Option<()> {
	if source.width == 0 || source.height == 0 || target.width == 0 || target.height == 0 {
		return None;
	}
	if source.pitch < source.width.checked_mul(4)? || target.bytes_per_pixel == 0 || target.bytes_per_pixel > 4 || target.pitch < target.width.checked_mul(target.bytes_per_pixel)? {
		return None;
	}
	let source_len = source.pitch as usize * source.height as usize;
	let target_len = target.pitch as usize * target.height as usize;
	if source.data.len() < source_len || target.data.len() < target_len {
		return None;
	}
	let end_x = damage.x.checked_add(damage.width)?;
	let end_y = damage.y.checked_add(damage.height)?;
	if damage.width == 0 || damage.height == 0 || end_x > source.width || end_y > source.height {
		return None;
	}
	Some(())
}

fn write_pixel(target: &mut Target<'_>, x: u32, y: u32, bgrx: u32) {
	let red = (bgrx >> 16) & 0xff;
	let green = (bgrx >> 8) & 0xff;
	let blue = bgrx & 0xff;
	let packed = scale_channel(red, target.red_size) << target.red_shift | scale_channel(green, target.green_size) << target.green_shift | scale_channel(blue, target.blue_size) << target.blue_shift;
	let offset = y as usize * target.pitch as usize + x as usize * target.bytes_per_pixel as usize;
	for byte in 0..target.bytes_per_pixel as usize {
		target.data[offset + byte] = (packed >> (byte * 8)) as u8;
	}
}

fn scale_channel(value: u32, bits: u8) -> u32 {
	if bits == 0 {
		0
	} else if bits >= 8 {
		value
	} else {
		value >> (8 - bits)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::vec;
	use std::vec::Vec;

	fn bytes(pixels: &[u32]) -> Vec<u8> {
		pixels.iter().flat_map(|pixel| pixel.to_le_bytes()).collect()
	}

	fn target(data: &mut [u8], width: u32, height: u32) -> Target<'_> {
		Target { data, width, height, pitch: width * 4, bytes_per_pixel: 4, red_shift: 16, red_size: 8, green_shift: 8, green_size: 8, blue_shift: 0, blue_size: 8 }
	}

	#[test]
	fn first_direct_present_copies_the_whole_surface() {
		let source = bytes(&[1, 2, 3, 4]);
		let mut output = vec![0xaau8; 16];
		let result = blit(Image { data: &source, width: 2, height: 2, pitch: 8 }, target(&mut output, 2, 2), Rect { x: 0, y: 0, width: 1, height: 1 }, true).unwrap();
		assert_eq!(result.rect, Rect { x: 0, y: 0, width: 2, height: 2 });
		assert_eq!(output, source);
	}

	#[test]
	fn scaled_damage_updates_only_its_output_rectangle() {
		let mut source = bytes(&[0x0011_2233, 0x0044_5566, 0x0077_8899, 0x00aa_bbcc]);
		let mut output = vec![0xaau8; 64];
		let image = Image { data: &source, width: 2, height: 2, pitch: 8 };
		let first = blit(image, target(&mut output, 4, 4), Rect { x: 0, y: 0, width: 1, height: 1 }, true).unwrap();
		assert_eq!(first.pixels, 32);
		source[..4].copy_from_slice(&0x00dd_eeffu32.to_le_bytes());
		let damage = blit(Image { data: &source, width: 2, height: 2, pitch: 8 }, target(&mut output, 4, 4), Rect { x: 0, y: 0, width: 1, height: 1 }, false).unwrap();
		assert_eq!(damage.rect, Rect { x: 0, y: 0, width: 2, height: 2 });
		assert_eq!(damage.pixels, 4);
		assert_eq!(u32::from_le_bytes(output[0..4].try_into().unwrap()), 0x00dd_eeff);
		assert_eq!(u32::from_le_bytes(output[60..64].try_into().unwrap()), 0x00aa_bbcc);
	}

	#[test]
	fn first_scaled_present_clears_letterbox_rows() {
		let source = bytes(&[0x0011_2233, 0x0044_5566]);
		let mut output = vec![0xaau8; 64];
		blit(Image { data: &source, width: 2, height: 1, pitch: 8 }, target(&mut output, 4, 4), Rect { x: 0, y: 0, width: 2, height: 1 }, true).unwrap();
		assert_eq!(&output[..16], &[0; 16]);
		assert_eq!(&output[48..], &[0; 16]);
	}
}
