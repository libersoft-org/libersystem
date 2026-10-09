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
