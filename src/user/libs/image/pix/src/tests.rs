use super::*;
use std::vec;
use std::vec::Vec;

#[test]
fn rgba_preserves_straight_alpha_and_converts_only_for_display() {
	let image = RgbaImage::new(2, 1, vec![255, 128, 64, 128, 1, 2, 3, 0]).unwrap();
	assert_eq!(image.pixels, vec![255, 128, 64, 128, 1, 2, 3, 0]);
	assert_eq!(image.to_bgrx().unwrap(), vec![32, 64, 128, 0, 0, 0, 0, 0]);
}

#[test]
fn animation_bounds_frames_geometry_duration_and_cumulative_pixels() {
	let image = RgbaImage::new(2, 2, vec![0; 16]).unwrap();
	let animation = Animation::new(4, 4, 0, vec![Frame { image, x: 1, y: 1, duration_ms: 20, blend: Blend::Over, disposal: Disposal::Previous }]).unwrap();
	assert_eq!(animation.frames.len(), 1);
	let outside = RgbaImage::new(2, 2, vec![0; 16]).unwrap();
	assert_eq!(Animation::new(2, 2, 1, vec![Frame { image: outside, x: 1, y: 0, duration_ms: 20, blend: Blend::Source, disposal: Disposal::Keep }]), Err(Error::Invalid));
}

#[test]
fn compositor_applies_blend_and_disposal_between_frames() {
	let first = RgbaImage::new(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
	let overlay = RgbaImage::new(1, 1, vec![0, 255, 0, 128]).unwrap();
	let mut compositor = Compositor::new(2, 1).unwrap();
	let shown = compositor.render(&Frame { image: first, x: 0, y: 0, duration_ms: 10, blend: Blend::Source, disposal: Disposal::Keep }).unwrap();
	assert_eq!(shown.pixels, vec![255, 0, 0, 255, 0, 0, 255, 255]);
	let shown = compositor.render(&Frame { image: overlay, x: 1, y: 0, duration_ms: 10, blend: Blend::Over, disposal: Disposal::Background }).unwrap();
	assert_eq!(&shown.pixels[..4], &[255, 0, 0, 255]);
	assert_eq!(shown.pixels[7], 255);
	let shown = compositor.render(&Frame { image: RgbaImage::new(1, 1, vec![1, 2, 3, 255]).unwrap(), x: 0, y: 0, duration_ms: 10, blend: Blend::Source, disposal: Disposal::Keep }).unwrap();
	assert_eq!(&shown.pixels[4..8], &[0, 0, 0, 0]);
}

#[test]
fn animation_preserves_zero_duration_and_compositor_background() {
	let background = [9, 8, 7, 6];
	let image = RgbaImage::new(1, 1, vec![1, 2, 3, 255]).unwrap();
	let animation = Animation::new_with_background(2, 1, background, 0, vec![Frame { image, x: 0, y: 0, duration_ms: 0, blend: Blend::Source, disposal: Disposal::Background }]).unwrap();
	assert_eq!(animation.background, background);
	assert_eq!(animation.frames[0].duration_ms, 0);
	let mut compositor = Compositor::new_with_background(animation.width, animation.height, animation.background).unwrap();
	let shown = compositor.render(&animation.frames[0]).unwrap();
	assert_eq!(shown.pixels, vec![1, 2, 3, 255, 9, 8, 7, 6]);
	let shown = compositor.render(&Frame { image: RgbaImage::new(1, 1, vec![4, 5, 6, 255]).unwrap(), x: 1, y: 0, duration_ms: 1, blend: Blend::Source, disposal: Disposal::Keep }).unwrap();
	assert_eq!(shown.pixels, vec![9, 8, 7, 6, 4, 5, 6, 255]);
}

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

#[test]
fn native_crop_uses_the_requested_source_origin_and_clears_the_target() {
	let source = bytes(&[1, 2, 3, 4, 5, 6]);
	let mut output = vec![0xaau8; 16];
	let result = blit_crop(Image { data: &source, width: 3, height: 2, pitch: 12 }, target(&mut output, 2, 2), 1, 0).unwrap();
	assert_eq!(result.rect, Rect { x: 0, y: 0, width: 2, height: 2 });
	assert_eq!(output, bytes(&[2, 3, 5, 6]));

	let mut letterbox = vec![0xaau8; 24];
	blit_crop(Image { data: &source, width: 3, height: 2, pitch: 12 }, target(&mut letterbox, 3, 2), 2, 0).unwrap();
	assert_eq!(&letterbox[..4], &[0; 4]);
	assert_eq!(u32::from_le_bytes(letterbox[4..8].try_into().unwrap()), 3);
}
