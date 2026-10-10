use super::*;
use graphics_core::format::PackedChannel;
use std::vec;

// Frozen pre-optimization blitter. Comparing the whole buffer includes pitch padding,
// letterbox pixels, and pixels outside damage; result equality also covers device damage.
fn generic_blit(source: Image<'_>, mut target: Target<'_>, damage: Rect, first: bool) -> Option<BlitResult> {
	validate(&source, &target, damage)?;
	// THE FAST PATH IS A QUESTION ABOUT THE FORMAT, and it used to be asked as nine comparisons on
	// loose fields - four bytes per pixel with red at 16, green at 8 and blue at 0, each eight bits
	// wide. That IS `B8G8R8A8` in memory order, and the shared model can say so by name.
	let direct = source.width() == target.width() && source.height() == target.height() && target.is_bgra8();
	if direct {
		let rect = if first { Rect { x: 0, y: 0, width: source.width(), height: source.height() } } else { damage };
		let bytes = rect.width as usize * 4;
		for row in rect.y..rect.y + rect.height {
			let src = row as usize * source.pitch() as usize + rect.x as usize * 4;
			let dst = row as usize * target.pitch() as usize + rect.x as usize * 4;
			target.bytes_mut()[dst..dst + bytes].copy_from_slice(&source.bytes()[src..src + bytes]);
		}
		return Some(BlitResult { rect, pixels: rect.width as u64 * rect.height as u64, direct: true });
	}

	let sw = source.width() as u64;
	let sh = source.height() as u64;
	let dw = target.width() as u64;
	let dh = target.height() as u64;
	let width_limited = dw.saturating_mul(sh) <= dh.saturating_mul(sw);
	let (out_width, out_height) = if width_limited { (target.width(), ((sh * dw) / sw).max(1) as u32) } else { (((sw * dh) / sh).max(1) as u32, target.height()) };
	let offset_x = (target.width() - out_width) / 2;
	let offset_y = (target.height() - out_height) / 2;
	let (x0, y0, x1, y1) = if first {
		target.bytes_mut().fill(0);
		(0, 0, out_width, out_height)
	} else {
		let end_x = (damage.x + damage.width) as u64 * out_width as u64;
		let end_y = (damage.y + damage.height) as u64 * out_height as u64;
		((damage.x as u64 * out_width as u64 / sw) as u32, (damage.y as u64 * out_height as u64 / sh) as u32, end_x.div_ceil(sw) as u32, end_y.div_ceil(sh) as u32)
	};
	for output_y in y0..y1 {
		let source_y = (output_y as u64 * source.height() as u64 / out_height as u64) as u32;
		for output_x in x0..x1 {
			let source_x = (output_x as u64 * source.width() as u64 / out_width as u64) as u32;
			let source_offset = source_y as usize * source.pitch() as usize + source_x as usize * 4;
			let pixel = u32::from_le_bytes(source.bytes()[source_offset..source_offset + 4].try_into().ok()?);
			write_pixel(&mut target, offset_x + output_x, offset_y + output_y, pixel);
		}
	}
	let width = x1 - x0;
	let height = y1 - y0;
	let written = width as u64 * height as u64 + if first { target.width() as u64 * target.height() as u64 } else { 0 };
	let rect = if first { Rect { x: 0, y: 0, width: target.width(), height: target.height() } } else { Rect { x: offset_x + x0, y: offset_y + y0, width, height } };
	Some(BlitResult { rect, pixels: written, direct: false })
}

fn run_case(sw: u32, sh: u32, dw: u32, dh: u32, packed: PackedRgbLayout, damage: Rect, first: bool) {
	let source_pitch = sw * 4 + 12;
	let target_pitch = dw * packed.bytes_per_pixel as u32 + 16;
	let mut source = vec![0xabu8; (source_pitch * sh) as usize];
	for y in 0..sh {
		for x in 0..sw {
			let p = (y * source_pitch + x * 4) as usize;
			source[p..p + 4].copy_from_slice(&[(x + y * 17) as u8, (255 - (x + y * 31) % 256) as u8, (x * 97 + y * 71) as u8, (x * 53 + y * 11) as u8]);
		}
	}
	let mut actual = vec![0xa5u8; (target_pitch * dh) as usize];
	let mut expected = actual.clone();
	let layout = ImageLayout::scanout(Extent2D::new(dw, dh), target_pitch, PixelStorage::PackedRgbUnorm(packed)).unwrap();
	let got = blit(Image::rgba(&source, sw, sh, source_pitch).unwrap(), Target::from_layout(&mut actual, layout).unwrap(), damage, first);
	let want = generic_blit(Image::rgba(&source, sw, sh, source_pitch).unwrap(), Target::from_layout(&mut expected, layout).unwrap(), damage, first);
	assert_eq!(got, want, "geometry={sw}x{sh}->{dw}x{dh}, format={packed:?}, damage={damage:?}, first={first}");
	assert_eq!(actual, expected, "geometry={sw}x{sh}->{dw}x{dh}, format={packed:?}, damage={damage:?}, first={first}");
}

#[test]
fn nearest_copy_matches_generic_packing_and_damage_for_all_layout_classes() {
	let canonical = PixelFormat::B8G8R8A8Unorm.packed_masks().unwrap();
	let mut reserved = canonical;
	reserved.reserved = PackedChannel { shift: 24, bits: 8 };
	let layouts = [
		canonical,
		reserved,
		PackedRgbLayout::from_masks(4, (0, 8), (8, 8), (16, 8)).unwrap(),
		PackedRgbLayout::from_masks(4, (20, 10), (10, 10), (0, 10)).unwrap(),
		PackedRgbLayout::from_masks(3, (16, 8), (8, 8), (0, 8)).unwrap(),
		PackedRgbLayout::from_masks(2, (11, 5), (5, 6), (0, 5)).unwrap(),
		PackedRgbLayout::from_masks(1, (5, 3), (2, 3), (0, 2)).unwrap(),
	];
	for (sw, sh) in [(1, 1), (2, 3), (7, 5), (17, 13), (256, 1)] {
		for (dw, dh) in [(1, 1), (3, 2), (5, 9), (31, 19), (sw, sh), (sw * 2, sh * 2)] {
			let damages = [
				Rect { x: 0, y: 0, width: sw, height: sh },
				Rect { x: 0, y: 0, width: 1, height: 1 },
				Rect { x: sw - 1, y: sh - 1, width: 1, height: 1 },
				Rect { x: sw / 2, y: sh / 2, width: sw - sw / 2, height: sh - sh / 2 },
			];
			for packed in layouts {
				for damage in damages {
					for first in [false, true] {
						run_case(sw, sh, dw, dh, packed, damage, first);
					}
				}
			}
		}
	}
}

#[test]
fn nearest_copy_preserves_real_scanout_letterboxes_and_partial_damage() {
	let packed = PixelFormat::B8G8R8A8Unorm.packed_masks().unwrap();
	for (sw, sh, dw, dh) in [(640, 480, 1280, 800), (800, 600, 1280, 800), (640, 480, 320, 240), (480, 640, 1280, 800)] {
		for first in [false, true] {
			run_case(sw, sh, dw, dh, packed, Rect { x: 0, y: 0, width: sw, height: sh }, first);
			run_case(sw, sh, dw, dh, packed, Rect { x: 23, y: 19, width: 71, height: 53 }, first);
		}
	}
}

#[test]
fn nearest_copy_keeps_invalid_damage_a_refusal_without_writes() {
	let packed = PixelFormat::B8G8R8A8Unorm.packed_masks().unwrap();
	for damage in [
		Rect { x: 0, y: 0, width: 0, height: 1 },
		Rect { x: 6, y: 4, width: 2, height: 1 },
		Rect { x: 0, y: 0, width: 1, height: 6 },
		Rect { x: u32::MAX, y: 0, width: 1, height: 1 },
	] {
		for first in [false, true] {
			run_case(7, 5, 31, 19, packed, damage, first);
		}
	}
}
