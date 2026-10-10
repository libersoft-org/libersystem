use super::*;
use graphics_core::geom::Extent2D;
use graphics_core::layout::ImageLayout;
use graphics_core::semantics::ImageSemantics;
use graphics_core::{AlphaMode, ColorSpace, PixelFormat, PixelStorage};
use std::sync::atomic::{AtomicUsize, Ordering};

fn attachments(width: u32, height: u32) -> (Colour, Colour) {
	let mut colour = Colour::try_new(width, height, 1, false).unwrap();
	let mut ids = Colour::try_new(width, height, 1, true).unwrap();
	for y in 0..height {
		for x in 0..width {
			colour.set(x, y, 0, Vec4::new((x as f32 - 2.0) / width as f32, y as f32 / height as f32, ((x + y) % 17) as f32 / 13.0, 0.37));
			ids.set_identity(x, y, 0, [0, 1, 2, 3, 0x01000001, u32::MAX][(x + y) as usize % 6]);
		}
	}
	(colour, ids)
}

fn image(width: u32, height: u32, origin: RowOrigin) -> OwnedImage {
	let layout = ImageLayout::new(Extent2D::new(width, height), width * 4 + 12, PixelStorage::Known(PixelFormat::R8G8B8A8Unorm), origin, ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Straight }).unwrap();
	let mut image = OwnedImage::new(layout).unwrap();
	image.bytes_mut().fill(0xa5);
	image
}

// The original whole-image walk, independent of the new chunk origin and row arithmetic.
fn reference(image: &mut OwnedImage, colour: &Colour, ids: &Colour, overlay: bool) {
	let mut view = image.view_mut();
	for y in 0..colour.height {
		for x in 0..colour.width {
			let texel = if overlay {
				match ids.identity_at(x, y, 0).unwrap() {
					1 => Vec4::new(0.95, 0.25, 0.20, 1.0),
					2 => Vec4::new(0.20, 0.45, 0.95, 1.0),
					3 => Vec4::new(0.95, 0.85, 0.20, 1.0),
					_ => Vec4::new(0.02, 0.02, 0.03, 1.0),
				}
			} else {
				colour.at(x, y, 0)
			};
			write(&mut view, x, y, Rgba::new(texel.x, texel.y, texel.z, 1.0));
		}
	}
}

#[test]
fn handover_bands_match_original_walk_on_real_threads_without_worker_allocations() {
	for (width, height) in [(1, 1), (7, 3), (33, 31), (65, 67), (320, 240)] {
		let (colour, ids) = attachments(width, height);
		for origin in [RowOrigin::TopLeft, RowOrigin::BottomLeft] {
			for overlay in [false, true] {
				let mut expected = image(width, height, origin);
				let mut actual = image(width, height, origin);
				reference(&mut expected, &colour, &ids, overlay);
				let allocations = AtomicUsize::new(0);
				let mut work = bands(&mut actual);
				assert!(work.iter().flatten().count() <= MAX_BANDS);
				std::thread::scope(|scope| {
					for band in work.iter_mut().rev().flatten() {
						let (colour, ids, allocations) = (&colour, &ids, &allocations);
						scope.spawn(move || {
							allocations.fetch_add(crate::allocation_test::count(|| paint(band, colour, ids, overlay)), Ordering::Relaxed);
						});
					}
				});
				assert_eq!(allocations.load(Ordering::Relaxed), 0, "per-band execution allocates nothing on every worker");
				assert_eq!(actual.bytes(), expected.bytes(), "{width}x{height}, {origin:?}, IDs={overlay}; padding is preserved too");
			}
		}
	}
}

#[test]
fn handover_repeated_animation_and_fixed_dispatch_storage_allocate_nothing() {
	let (mut colour, mut ids) = attachments(67, 69);
	let mut actual = image(67, 69, RowOrigin::TopLeft);
	let mut expected = image(67, 69, RowOrigin::TopLeft);
	for frame in 0..12 {
		colour.set(frame, frame * 3, 0, Vec4::new(0.15, 0.77, frame as f32 / 10.0, 0.0));
		ids.set_identity(frame, frame * 3, 0, frame);
		let overlay = frame % 2 != 0;
		assert_eq!(
			crate::allocation_test::count(|| {
				let mut work = bands(&mut actual);
				for band in work.iter_mut().rev().flatten() {
					paint(band, &colour, &ids, overlay);
				}
			}),
			0,
			"band construction and changing image execution retain no heap work"
		);
		reference(&mut expected, &colour, &ids, overlay);
		assert_eq!(actual.bytes(), expected.bytes());
	}
}
