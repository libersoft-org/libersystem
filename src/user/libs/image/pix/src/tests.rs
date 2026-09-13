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
	// The scanout shape every one of these cases uses: four bytes with blue first in memory, which
	// as a little-endian `u32` puts red at 16 and blue at 0.
	Target::packed(data, width, height, width * 4, 4, (16, 8), (8, 8), (0, 8)).expect("a target this test sized itself")
}

#[test]
fn first_direct_present_copies_the_whole_surface() {
	let source = bytes(&[1, 2, 3, 4]);
	let mut output = vec![0xaau8; 16];
	let result = blit(Image::rgba(&source, 2, 2, 8).expect("an image this test sized itself"), target(&mut output, 2, 2), Rect { x: 0, y: 0, width: 1, height: 1 }, true).unwrap();
	assert_eq!(result.rect, Rect { x: 0, y: 0, width: 2, height: 2 });
	assert_eq!(output, source);
}

#[test]
fn scaled_damage_updates_only_its_output_rectangle() {
	let mut source = bytes(&[0x0011_2233, 0x0044_5566, 0x0077_8899, 0x00aa_bbcc]);
	let mut output = vec![0xaau8; 64];
	let image = Image::rgba(&source, 2, 2, 8).expect("an image this test sized itself");
	let first = blit(image, target(&mut output, 4, 4), Rect { x: 0, y: 0, width: 1, height: 1 }, true).unwrap();
	assert_eq!(first.pixels, 32);
	source[..4].copy_from_slice(&0x00dd_eeffu32.to_le_bytes());
	let damage = blit(Image::rgba(&source, 2, 2, 8).expect("an image this test sized itself"), target(&mut output, 4, 4), Rect { x: 0, y: 0, width: 1, height: 1 }, false).unwrap();
	assert_eq!(damage.rect, Rect { x: 0, y: 0, width: 2, height: 2 });
	assert_eq!(damage.pixels, 4);
	assert_eq!(u32::from_le_bytes(output[0..4].try_into().unwrap()), 0x00dd_eeff);
	assert_eq!(u32::from_le_bytes(output[60..64].try_into().unwrap()), 0x00aa_bbcc);
}

#[test]
fn first_scaled_present_clears_letterbox_rows() {
	let source = bytes(&[0x0011_2233, 0x0044_5566]);
	let mut output = vec![0xaau8; 64];
	blit(Image::rgba(&source, 2, 1, 8).expect("an image this test sized itself"), target(&mut output, 4, 4), Rect { x: 0, y: 0, width: 2, height: 1 }, true).unwrap();
	assert_eq!(&output[..16], &[0; 16]);
	assert_eq!(&output[48..], &[0; 16]);
}

#[test]
fn native_crop_uses_the_requested_source_origin_and_clears_the_target() {
	let source = bytes(&[1, 2, 3, 4, 5, 6]);
	let mut output = vec![0xaau8; 16];
	let result = blit_crop(Image::rgba(&source, 3, 2, 12).expect("an image this test sized itself"), target(&mut output, 2, 2), 1, 0).unwrap();
	assert_eq!(result.rect, Rect { x: 0, y: 0, width: 2, height: 2 });
	assert_eq!(output, bytes(&[2, 3, 5, 6]));

	let mut letterbox = vec![0xaau8; 24];
	blit_crop(Image::rgba(&source, 3, 2, 12).expect("an image this test sized itself"), target(&mut letterbox, 3, 2), 2, 0).unwrap();
	assert_eq!(&letterbox[..4], &[0; 4]);
	assert_eq!(u32::from_le_bytes(letterbox[4..8].try_into().unwrap()), 3);
}

#[test]
fn viewport_blit_scales_and_clamps_a_centered_crop() {
	let source = bytes(&[1, 2, 3, 4]);
	let mut output = vec![0xaau8; 16];
	let result = blit_view(Image::rgba(&source, 2, 2, 8).expect("an image this test sized itself"), target(&mut output, 2, 2), 4, 4, 1, 1).unwrap();
	assert_eq!(result.rect, Rect { x: 0, y: 0, width: 2, height: 2 });
	assert_eq!(output, bytes(&[1, 2, 3, 4]));

	let mut letterbox = vec![0xaau8; 64];
	blit_view(Image::rgba(&source, 2, 2, 8).expect("an image this test sized itself"), target(&mut letterbox, 4, 4), 2, 2, 99, 99).unwrap();
	assert_eq!(&letterbox[..16], &[0; 16]);
	assert_eq!(&letterbox[48..], &[0; 16]);
	assert_eq!(u32::from_le_bytes(letterbox[20..24].try_into().unwrap()), 1);
}

#[test]
// AN IMAGE WITH NO COLOUR METADATA IS A BACK DOOR INTO THE IMAGE MODEL: width, height, pitch and
// bytes say nothing about whether 128 is half the light or half the encoded value, or whether the
// colour has already been multiplied by its alpha. The semantics travel with the pixels, and the only
// way these pixels enter anything that draws is through a CHECKED view that carries them.
fn an_image_carries_its_meaning_and_enters_the_model_checked() {
	let image = RgbaImage::new(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 128]).unwrap();
	assert_eq!(image.semantics, DEFAULT_SEMANTICS, "what every decoder in this tree produces, stated rather than assumed");

	let view = image.view().expect("a checked view");
	assert_eq!(view.layout().extent.width, 2);
	assert_eq!(view.layout().semantics, DEFAULT_SEMANTICS);
	// THE VIEW IS THE SEAM, so what a renderer sees is the bytes AND what they mean.
	let raw = graphics_core::pixel::read(&view, 0, 0).expect("a pixel");
	assert_eq!((raw.red, raw.alpha), (1.0, 1.0));

	// A DECODER THAT KNOWS BETTER SAYS SO, and the view carries that instead.
	let opaque = graphics_core::semantics::ImageSemantics::Color { color_space: graphics_core::ColorSpace::DisplayP3, alpha_mode: graphics_core::AlphaMode::Opaque };
	let wide = RgbaImage::new_with_semantics(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 128], opaque).unwrap();
	assert_eq!(wide.view().expect("a view").layout().semantics, opaque);
	// And the two are different images, because their bytes mean different things.
	assert!(wide != image);

	// AN ALPHA MODE THE FORMAT DOES NOT ADMIT IS REFUSED at the seam rather than drawn: the check is
	// the layout constructor's, which is why it happens once here instead of nowhere.
	let mask = graphics_core::semantics::ImageSemantics::Mask { interpretation: graphics_core::semantics::MaskInterpretation::Coverage };
	let as_mask = RgbaImage::new_with_semantics(2, 1, vec![0; 8], mask).unwrap();
	assert!(as_mask.view().is_ok(), "a mask is a legal meaning for these bytes; what it is not is a colour");
}
