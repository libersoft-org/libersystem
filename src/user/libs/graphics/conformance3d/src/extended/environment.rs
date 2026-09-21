//! Environment lighting: the split sum's two halves, the prefilter's shape and the irradiance term.

use render_math::Vec3;
use scene3d::environment::{self, Irradiance};

use crate::Outcome;
use crate::extended::{claimed, close, exact};

/// A sphere of directions with equal solid angles, for integrating an environment by hand.
///
/// THE GOLDEN-ANGLE SPIRAL, because a latitude-longitude grid concentrates points at the poles and
/// would weight the top of the sky more heavily than the sides - which is the error the solid angle
/// exists to prevent, so a fixture built on one could not detect it.
fn sphere(count: u32) -> alloc::vec::Vec<Vec3> {
	let golden = core::f32::consts::PI * (3.0 - render_math::sqrt(5.0));
	(0..count)
		.map(|index| {
			let z = 1.0 - 2.0 * (index as f32 + 0.5) / count as f32;
			let radius = render_math::sqrt((1.0 - z * z).max(0.0));
			let (sine, cosine) = render_math::quaternion::sin_cos(golden * index as f32);
			Vec3::new(radius * cosine, radius * sine, z)
		})
		.collect()
}

pub fn split_sum_approximation() -> Outcome {
	// `prefiltered * (F0 * A + B)`: with the table at `(1, 0)` the term is the prefiltered radiance
	// times F0 and nothing else, which is what makes one prefilter serve every F0.
	let f0 = Vec3::new(0.2, 0.4, 0.6);
	let term = environment::environment_term(f0, Vec3::new(2.0, 2.0, 2.0), (1.0, 0.0));
	require!(term.sub(f0.scale(2.0)).length() < 1e-4, "the split sum is prefiltered * (F0 * A + B), got {term:?}");
	// AND `B` IS ADDED RATHER THAN SCALED, which is the whole reason the table has two channels.
	let with_bias = environment::environment_term(Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 1.0), (0.0, 0.25));
	require!(exact(with_bias.x, 0.25), "the bias reaches a surface with no F0 at all, got {}", with_bias.x);
	Ok(())
}

pub fn ggx_prefiltered_environment() -> Outcome {
	// PREFILTERING A UNIFORM ENVIRONMENT RETURNS IT UNCHANGED, at every roughness and in every
	// direction. It is the prefilter's own white furnace: a wrong weight, a missing normalisation or
	// a sample left out of the divisor all show up here and in no single rendered image.
	let sky = Vec3::new(2.0, 3.0, 4.0);
	for roughness in [0.0f32, 0.25, 0.5, 1.0] {
		for direction in [Vec3::new(0.0, 0.0, 1.0), Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.577, 0.577, 0.577)] {
			let filtered = environment::prefilter_direction(direction, roughness, 256, |_, _| sky);
			require!(filtered.sub(sky).length() < 1e-3, "a uniform sky prefilters to itself at roughness {roughness}, got {filtered:?}");
		}
	}
	// AND THE SAMPLE'S OWN SOLID ANGLE REACHES THE SOURCE, which is what lets a caller choose the
	// mip that removes fireflies. A rougher lobe spreads its samples, so each covers more sky.
	let mut widest_mirror = 0.0f32;
	let _ = environment::prefilter_direction(Vec3::new(0.0, 0.0, 1.0), 0.05, 256, |_, solid| {
		if solid > widest_mirror {
			widest_mirror = solid;
		}
		sky
	});
	let mut widest_rough = 0.0f32;
	let _ = environment::prefilter_direction(Vec3::new(0.0, 0.0, 1.0), 1.0, 256, |_, solid| {
		if solid > widest_rough {
			widest_rough = solid;
		}
		sky
	});
	require!(widest_rough > widest_mirror, "a rough lobe's samples cover more sky: {widest_rough} against {widest_mirror}");
	Ok(())
}

pub fn prefilter_level_rule() -> Outcome {
	require!(environment::prefilter_levels(256) == 6, "256 down to 8 is six levels, got {}", environment::prefilter_levels(256));
	require!(environment::prefilter_levels(8) == 1, "a face already at the floor has one");
	require!(environment::prefilter_levels(4) == 0, "and one below it has none");
	require!(environment::prefilter_levels(256) == claimed().environment_prefilter_levels, "the code and the profile's limit agree: {} against {}", environment::prefilter_levels(256), claimed().environment_prefilter_levels);
	// LEVEL `i` HOLDS `roughness = i / (levels - 1)`: the first is a mirror and the last fully rough.
	require!(exact(environment::prefilter_roughness(0, 6), 0.0), "the first level is a mirror");
	require!(exact(environment::prefilter_roughness(5, 6), 1.0), "the last is fully rough");
	require!(exact(environment::prefilter_roughness(3, 6), 0.6), "and the steps are even in the perceptual value");
	Ok(())
}

pub fn brdf_integration_table() -> Outcome {
	let samples = environment::PREFILTER_SAMPLES;
	// A SMOOTH SURFACE HEAD-ON IS EXACTLY `(1, 0)`: the lobe is a delta along the normal, so
	// `dot(V,H)` is one, Schlick's fifth power is zero, and the integral is the visibility term at
	// normal incidence times four, which is one.
	let (scale, bias) = environment::brdf_integration(1.0, 0.045, samples);
	require!(close(scale, 1.0), "a smooth surface head-on scales F0 by one, got {scale}");
	require!(close(bias, 0.0), "and adds nothing, got {bias}");
	// ROUGHNESS TAKES ENERGY OUT AND GRAZING PUTS IT INTO THE BIAS, which is the shape the two
	// channels exist to carry. Measured from the profile's own terms at 1024 samples.
	let rough = environment::brdf_integration(1.0, 1.0, samples);
	require!(rough.0 < scale, "a rougher surface reflects less of F0: {} against {scale}", rough.0);
	require!((rough.0 - 0.307).abs() < 0.03, "and by the amount the profile's terms give, got {}", rough.0);
	let grazing = environment::brdf_integration(0.1, 0.5, samples);
	let facing = environment::brdf_integration(0.5, 0.5, samples);
	require!(grazing.1 > facing.1, "Fresnel rises at grazing, so the bias does: {} against {}", grazing.1, facing.1);
	// ENERGY IS NOT CREATED ANYWHERE IN THE TABLE.
	for step in 0..8u32 {
		let n_dot_v = (step as f32 + 0.5) / 8.0;
		for rough_step in 0..8u32 {
			let roughness = (rough_step as f32 + 0.5) / 8.0;
			let (a, b) = environment::brdf_integration(n_dot_v, roughness, 256);
			require!(a >= 0.0 && b >= 0.0, "neither channel goes negative at ({n_dot_v}, {roughness})");
			require!(a + b <= 1.0 + 1e-3, "a surface reflects no more than it receives at ({n_dot_v}, {roughness}): {a} + {b}");
		}
	}
	// AND THE TABLE IS THE SIZE THE PROFILE STATES.
	require!(environment::BRDF_TABLE_SIZE == 256, "the table is 256 by 256, got {}", environment::BRDF_TABLE_SIZE);
	Ok(())
}

pub fn irradiance_term() -> Outcome {
	// THE WHITE FURNACE. A sky of uniform radiance 1 delivers an irradiance of exactly `pi` to a
	// surface of ANY orientation - the integral of `cos(theta)` over a hemisphere - and it catches a
	// wrong band factor, a wrong basis normalisation and a missing solid angle at once.
	//
	// EITHER FORM CONFORMS. The profile admits a cosine-convolved cube map or nine spherical-
	// harmonic coefficients and requires only that they agree within its threshold, so what is
	// checked here is the ANSWER rather than which of the two produced it.
	let count = 4096u32;
	let solid = 4.0 * core::f32::consts::PI / count as f32;
	let mut sky = Irradiance::new();
	for direction in sphere(count) {
		sky.add(direction, Vec3::new(1.0, 1.0, 1.0), solid);
	}
	for normal in [Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, 0.0, -1.0), Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.577, 0.577, 0.577)] {
		let irradiance = sky.evaluate(normal).x;
		require!((irradiance - core::f32::consts::PI).abs() < 8.0 / 255.0, "a uniform sky gives pi at {normal:?}, got {irradiance}");
	}
	// AND A DIRECTIONAL SKY IS BRIGHTEST WHERE IT POINTS, which the furnace alone cannot tell: a
	// constant has no linear band at all.
	let mut half = Irradiance::new();
	for direction in sphere(count) {
		let radiance = if direction.z > 0.0 { Vec3::new(1.0, 1.0, 1.0) } else { Vec3::new(0.0, 0.0, 0.0) };
		half.add(direction, radiance, solid);
	}
	let up = half.evaluate(Vec3::new(0.0, 0.0, 1.0)).x;
	let side = half.evaluate(Vec3::new(1.0, 0.0, 0.0)).x;
	let down = half.evaluate(Vec3::new(0.0, 0.0, -1.0)).x;
	require!(up > side && side > down, "a surface facing the bright half is brightest: {up}, {side}, {down}");
	require!(down >= -1e-3, "and nothing goes negative, got {down}");
	Ok(())
}
