use super::*;

#[test]
fn independent_blur_accumulators_match_scalar_taps_bit_for_bit() {
	let bits = |pixel: Rgba| [pixel.red.to_bits(), pixel.green.to_bits(), pixel.blue.to_bits(), pixel.alpha.to_bits()];
	for size in [1, 2, 3, 7, 8, 16, 31, 64, 65, 129, 576] {
		let input: Vec<_> = (0..size)
			.map(|at| {
				let value = (at * 1_664_525 + 1_013_904_223) as u32;
				Rgba::new((value & 255) as f32 / 127.0 - 1.0, ((value >> 8) & 255) as f32 / 255.0, ((value >> 16) & 255) as f32 / 63.0, ((value >> 24) & 255) as f32 / 255.0)
			})
			.collect();
		for sigma in [0.0, 0.333, 1.0, 4.0, 6.0, 85.0] {
			let weights = kernel(sigma);
			let radius = (weights.len() as i64 - 1) / 2;
			for wanted in [0..size, size / 3..size * 2 / 3] {
				let sentinel = Rgba::new(-3.0, 7.0, 9.0, 2.0);
				let mut output = alloc::vec![sentinel; size];
				blur_run(&input, &mut output, wanted.clone(), &weights);
				for (at, actual) in output.iter().enumerate() {
					let mut expected = sentinel;
					if wanted.contains(&at) {
						expected = Rgba::TRANSPARENT;
						// The scalar reference includes one bounds decision per original tap.
						for (index, weight) in weights.iter().enumerate() {
							let tap = at as i64 + index as i64 - radius;
							if tap >= 0 && (tap as usize) < input.len() {
								expected = expected.plus(input[tap as usize].scaled(*weight));
							}
						}
					}
					assert_eq!(bits(*actual), bits(expected), "size={size}, sigma={sigma}, wanted={wanted:?}, at={at}");
				}
			}
		}
	}
}

// THE KERNEL IS THE GAUSSIAN THE PROFILE NAMES, TO THE FROZEN TOLERANCE. `graphics-profile` freezes a filter
// kernel's output at 1e-3 relative against the kernel evaluated at f64; the test above holds the two passes to
// these f32 weights bit for bit, so the weights are where the comparison with f64 belongs. Every weight, from
// a deviation under a pixel to one whose reach is the profile's largest radius, against `exp(-d^2 / 2 sigma^2)`
// at f64 over the same reach - three deviations in whole pixels, at least one - normalised the same way.
#[test]
fn the_blur_kernel_is_the_gaussian_at_f64_within_the_frozen_tolerance() {
	let frozen = graphics_profile::thresholds::THRESHOLDS.iter().find(|threshold| threshold.profile == "render2d" && threshold.what == "a filter kernel's output").expect("the frozen kernel threshold");
	assert!(frozen.tolerance.starts_with("<= 1e-3 relative against the kernel evaluated at f64"), "the tolerance applied here is the frozen one: {}", frozen.tolerance);
	const RELATIVE: f64 = 1e-3;
	for sigma in [0.2f32, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 10.0, 25.5, 85.0] {
		let weights = kernel(sigma);
		let radius = (weights.len() - 1) / 2;
		let sigma = f64::from(sigma);
		assert_eq!(radius, (sigma * 3.0).ceil().max(1.0) as usize, "sigma {sigma}: the reach is three deviations in whole pixels");
		let exact: Vec<f64> = (0..weights.len()).map(|index| (-(index as f64 - radius as f64).powi(2) / (2.0 * sigma * sigma)).exp()).collect();
		let total: f64 = exact.iter().sum();
		for (index, (weight, exact)) in weights.iter().zip(&exact).enumerate() {
			let exact = exact / total;
			let difference = (f64::from(*weight) - exact).abs();
			assert!(difference <= RELATIVE * exact, "sigma {sigma}, tap {index}: {weight} against {exact} at f64 is {} relative", difference / exact);
		}
	}
}

// A RUN READS AND WRITES WHAT `get` AND `set` WOULD, PIXEL FOR PIXEL. The pointwise nodes walk their regions
// through `read_run` and `write_run` now, so a run that starts before a raster, ends past it, lies on a row it
// does not have, or reaches a coordinate `get` cannot be asked for must still answer transparent exactly where
// `get` did - and write exactly the pixels `set` would have, and no others.
#[test]
fn a_run_reads_and_writes_what_get_and_set_would_across_every_edge() {
	use crate::target::{Raster, Tile};
	let bits = |pixel: Rgba| [pixel.red.to_bits(), pixel.green.to_bits(), pixel.blue.to_bits(), pixel.alpha.to_bits()];
	let colour = |x: u32, y: u32| Rgba::new(x as f32 + 0.25, y as f32 - 0.5, -(x as f32), 0.5 + (x % 3) as f32);
	let sentinel = Rgba::new(-3.0, 7.0, 9.0, 2.0);
	let lengths = [0usize, 1, 2, 5, 7, 9, 20];
	// A SURFACE WELL INSIDE THE COORDINATE SPACE, a tile beside it, and a surface whose last columns lie past
	// `u32::MAX`, where `get` cannot reach.
	let near_the_end = u32::MAX - 3;
	let mut tile = Tile::new(6);
	tile.rebase((4, 2));
	for y in 2..8 {
		for x in 4..10 {
			tile.set(x, y, colour(x, y));
		}
	}
	for bounds in [PixelRect::new(5, 3, 7, 4), PixelRect::new(near_the_end, 1, 8, 2)] {
		let mut surface = Surface::new(bounds, graphics_core::ColorSpace::SrgbLinear).expect("a surface");
		for y in bounds.y..bounds.y + bounds.height {
			for x in bounds.x..=bounds.x.saturating_add(bounds.width - 1) {
				surface.set(x, y, colour(x, y));
			}
		}
		let (left, top) = (i64::from(bounds.x), i64::from(bounds.y));
		let expected_at = |raster: &dyn Raster, x: i64, y: i64| if x < 0 || y < 0 || x > i64::from(u32::MAX) || y > i64::from(u32::MAX) { Rgba::TRANSPARENT } else { raster.get(x as u32, y as u32) };
		for y in top - 2..top + i64::from(bounds.height) + 2 {
			for x in left - 3..left + i64::from(bounds.width) + 4 {
				for length in lengths {
					for raster in [&surface as &dyn Raster, &tile as &dyn Raster] {
						let mut run = alloc::vec![sentinel; length];
						raster.read_run(x, y, &mut run);
						for (index, pixel) in run.iter().enumerate() {
							assert_eq!(bits(*pixel), bits(expected_at(raster, x + index as i64, y)), "read at x={x} y={y} length={length} index={index}");
						}
					}
				}
			}
		}
		// THE WRITES: one surface written by runs, one by `set`, both from the same starting picture.
		for y in bounds.y.saturating_sub(1)..bounds.y + bounds.height + 1 {
			for x in bounds.x.saturating_sub(3)..=bounds.x.saturating_add(bounds.width + 3) {
				for length in lengths {
					let mut by_run = Surface::new(bounds, graphics_core::ColorSpace::SrgbLinear).expect("a surface");
					let mut by_pixel = Surface::new(bounds, graphics_core::ColorSpace::SrgbLinear).expect("a surface");
					let values: Vec<Rgba> = (0..length).map(|index| Rgba::new(index as f32, 1.0, 2.0, 0.75)).collect();
					by_run.write_run(x, y, &values);
					for (index, value) in values.iter().enumerate() {
						if let Some(at) = x.checked_add(index as u32) {
							by_pixel.set(at, y, *value);
						}
					}
					for row in bounds.y..bounds.y + bounds.height {
						for column in bounds.x..=bounds.x.saturating_add(bounds.width - 1) {
							assert_eq!(bits(by_run.get(column, row)), bits(by_pixel.get(column, row)), "write at x={x} y={y} length={length}, pixel {column},{row}");
						}
					}
				}
			}
		}
	}
}
