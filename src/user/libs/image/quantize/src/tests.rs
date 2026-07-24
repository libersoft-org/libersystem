use super::*;
use alloc::vec;

fn image(width: u32, pixels: Vec<u8>) -> pix::RgbaImage {
	pix::RgbaImage::new(width, pixels.len() as u32 / width / 4, pixels).unwrap()
}

#[test]
fn exact_colors_and_transparency_are_preserved() {
	let source = image(3, vec![255, 0, 0, 255, 0, 255, 0, 255, 9, 8, 7, 0]);
	let palette = build_palette(&[source.as_rgba()], Options { quality: 100, dither: true, alpha_threshold: 128 }).unwrap();
	let indices = map_image(source.as_rgba(), &palette).unwrap();
	assert_eq!(palette.transparent_index, Some(0));
	assert_eq!(palette.colors.len(), 3);
	assert_eq!(indices[2], 0);
	assert_eq!(palette.colors[indices[0] as usize], [255, 0, 0, 255]);
	assert_eq!(palette.colors[indices[1] as usize], [0, 255, 0, 255]);
}

#[test]
fn quality_controls_palette_budget_and_mapping_is_deterministic() {
	let mut pixels = Vec::new();
	for value in 0..1024u32 {
		pixels.extend_from_slice(&[(value & 255) as u8, ((value >> 2) & 255) as u8, ((value * 91) & 255) as u8, 255]);
	}
	let source = image(32, pixels);
	let low = build_palette(&[source.as_rgba()], Options { quality: 0, dither: true, alpha_threshold: 128 }).unwrap();
	let high = build_palette(&[source.as_rgba()], Options { quality: 100, dither: true, alpha_threshold: 128 }).unwrap();
	assert_eq!(low.colors.len(), 16);
	assert!(high.colors.len() > low.colors.len());
	assert!(high.colors.len() <= 256);
	let low_indices = map_image(source.as_rgba(), &low).unwrap();
	let high_indices = map_image(source.as_rgba(), &high).unwrap();
	assert_eq!(high_indices, map_image(source.as_rgba(), &high).unwrap());
	let low_error = squared_error(&source, &low, &low_indices);
	let high_error = squared_error(&source, &high, &high_indices);
	assert!(high_error < low_error);
	assert!(high_error / (source.pixel_count() * 3) <= 256);
}

#[test]
fn rejects_invalid_quality_and_geometry() {
	let source = image(1, vec![0, 0, 0, 255]);
	assert_eq!(build_palette(&[source.as_rgba()], Options { quality: 101, dither: false, alpha_threshold: 128 }), Err(Error::Invalid));
	let invalid = Rgba { data: &[0; 3], width: 1, height: 1, pitch: 4 };
	assert_eq!(build_palette(&[invalid], Options { quality: 100, dither: false, alpha_threshold: 128 }), Err(Error::Invalid));
}

#[test]
fn late_transparency_reserves_an_index() {
	let mut pixels = Vec::new();
	for value in 0..256u16 {
		pixels.extend_from_slice(&[value as u8, 0, 0, 255]);
	}
	pixels.extend_from_slice(&[0, 0, 0, 0]);
	let source = image(257, pixels);
	let palette = build_palette(&[source.as_rgba()], Options { quality: 100, dither: false, alpha_threshold: 128 }).unwrap();
	assert!(palette.colors.len() <= 256);
	assert!(palette.colors.len() > 1);
	assert_eq!(palette.transparent_index, Some(0));
	assert_eq!(map_image(source.as_rgba(), &palette).unwrap()[256], 0);
}

fn squared_error(source: &pix::RgbaImage, palette: &Palette, indices: &[u8]) -> u64 {
	source
		.pixels
		.chunks_exact(4)
		.zip(indices)
		.map(|(pixel, index)| {
			let color = palette.colors[*index as usize];
			(0..3)
				.map(|channel| {
					let difference = pixel[channel] as i32 - color[channel] as i32;
					(difference * difference) as u64
				})
				.sum::<u64>()
		})
		.sum()
}
