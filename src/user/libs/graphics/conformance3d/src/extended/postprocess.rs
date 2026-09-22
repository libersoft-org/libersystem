//! HDR and post-processing: the bloom, the fog and the tone map that is last.

use render_math::Vec3;
use scene3d::postprocess;

use crate::Outcome;
use crate::extended::{close, exact};

fn grey(light: f32) -> Vec3 {
	Vec3::new(light, light, light)
}

pub fn bloom_soft_knee() -> Outcome {
	// THE QUADRATIC JOINS SMOOTHLY AT BOTH ENDS: zero with zero slope at `T - K`, and `L - T` with
	// unit slope at `T + K`. A hard threshold makes a specular glint crossing it bloom at full
	// strength in one frame, which reads as a flash.
	require!(exact(postprocess::bloom_prefilter(grey(0.4)).x, 0.0), "below the knee nothing blooms");
	require!(exact(postprocess::bloom_prefilter(grey(0.5)).x, 0.0), "and the knee starts at zero");
	require!(close(postprocess::bloom_prefilter(grey(1.0)).x, 0.125), "at the threshold the quadratic gives 0.125, got {}", postprocess::bloom_prefilter(grey(1.0)).x);
	require!(close(postprocess::bloom_prefilter(grey(1.5)).x, 0.5), "at the far end it is the excess itself, got {}", postprocess::bloom_prefilter(grey(1.5)).x);
	require!(close(postprocess::bloom_prefilter(grey(10.0)).x, 9.0), "above the knee the whole excess blooms, got {}", postprocess::bloom_prefilter(grey(10.0)).x);
	// AND THE BRANCHES MEET: a thousandth more light either side of the join changes the answer by a
	// thousandth rather than by a step.
	let below = postprocess::bloom_prefilter(grey(1.499)).x;
	let above = postprocess::bloom_prefilter(grey(1.501)).x;
	require!((above - below).abs() < 3e-3, "the branches join: {below} then {above}");
	// THE HUE SURVIVES, because the excess is a fraction of the luminance rather than a per-channel
	// threshold.
	let coloured = Vec3::new(4.0, 1.0, 0.5);
	let bloomed = postprocess::bloom_prefilter(coloured);
	require!((bloomed.x / bloomed.y - coloured.x / coloured.y).abs() < 1e-3, "the bloom keeps the colour's hue");
	Ok(())
}

pub fn bloom_pyramid() -> Outcome {
	// BOTH KERNELS PARTITION UNITY, which catches a mistyped weight, a missing tap and a wrong
	// divisor at once: a constant field must survive both, or the bloom brightens or darkens every
	// frame it is added to.
	let constant = Vec3::new(0.25, 0.5, 0.75);
	let down = postprocess::downsample_13((0.5, 0.5), (0.01, 0.01), |_, _| constant);
	require!(down.sub(constant).length() < 1e-4, "the 13-tap downsample preserves a constant, got {down:?}");
	let up = postprocess::upsample_tent_9((0.5, 0.5), (0.01, 0.01), |_, _| constant);
	require!(up.sub(constant).length() < 1e-4, "and so does the 9-tap tent, got {up:?}");
	// THE TENT IS WEIGHTED TOWARD ITS CENTRE, which is what makes it a tent and not a box.
	let centre = postprocess::upsample_tent_9((0.5, 0.5), (0.1, 0.1), |x, y| if (x - 0.5).abs() < 0.01 && (y - 0.5).abs() < 0.01 { Vec3::new(16.0, 16.0, 16.0) } else { Vec3::new(0.0, 0.0, 0.0) });
	require!(close(centre.x, 4.0), "the centre carries four sixteenths, got {}", centre.x);
	require!(postprocess::BLOOM_LEVELS == 6, "the pyramid is six levels, got {}", postprocess::BLOOM_LEVELS);
	require!(exact(postprocess::BLOOM_WEIGHT, 0.04), "and is added back at 0.04, got {}", postprocess::BLOOM_WEIGHT);
	Ok(())
}

pub fn rec709_luminance() -> Outcome {
	// THE SAME TRIPLE THE IMAGE-COLOUR PROFILE USES, and its coefficients sum to exactly one -
	// which is what makes white have a luminance of one and is the cheapest check that none of the
	// three has been mistyped.
	let sum = postprocess::LUMINANCE_REC709.x + postprocess::LUMINANCE_REC709.y + postprocess::LUMINANCE_REC709.z;
	require!(exact(sum, 1.0), "the coefficients sum to one, got {sum}");
	require!(exact(postprocess::luminance(Vec3::new(1.0, 1.0, 1.0)), 1.0), "white has a luminance of one");
	require!(exact(postprocess::luminance(Vec3::new(0.0, 1.0, 0.0)), 0.7152), "green carries most of it");
	Ok(())
}

pub fn tone_map_extended_reinhard() -> Outcome {
	// THE OPERATOR SENDS THE PROFILE'S WHITE TO EXACTLY ONE AND IS THE IDENTITY BELOW ITS KNEE,
	// which are the two properties that let one curve serve a scene renderer and a compositor at
	// once. At `WHITE` = 4 and `KNEE` = 0.8 the shoulder's white point is `(4 - 0.8) / 0.2` = 16,
	// and `16 * (1 + 16/256) / 17` = 1, so `0.8 + 0.2 * 1` = 1.
	let white = postprocess::tone_map_white();
	require!(exact(white, 4.0), "the white point comes from the image-colour profile, got {white}");
	require!(close(postprocess::tone_map(grey(white)).x, 1.0), "the white point maps to one, got {}", postprocess::tone_map(grey(white)).x);
	// BELOW THE KNEE IT IS THE IDENTITY, which is what a compositor needs: a colour an author
	// already put inside the range comes back as itself.
	for value in [0.0f32, 0.125, 0.5, 0.799] {
		require!(exact(postprocess::tone_map(grey(value)).x, value), "the identity below the knee at {value}");
	}
	// AND ABOVE IT THE SHOULDER RUNS: at a luminance of one the shoulder's input is 1, and
	// `0.8 + 0.2 * (1 * (1 + 1/256) / 2)` = 0.900391.
	require!(close(postprocess::tone_map(grey(1.0)).x, 0.900_391), "the curve at one is 0.900391, got {}", postprocess::tone_map(grey(1.0)).x);
	// AND A HIGHLIGHT KEEPS ITS ORDER rather than flattening, which is the difference between a
	// bright window and a white rectangle.
	let two = postprocess::tone_map(grey(2.0)).x;
	let three = postprocess::tone_map(grey(3.0)).x;
	require!(two < three && three < 1.0, "two and three times white are ordered and inside the range, got {two} then {three}");
	// AND THERE IS NO STEP ANYWHERE, at one or at the knee: the shoulder meets the identity with the
	// same slope, because extended Reinhard has slope one at zero.
	let below = postprocess::tone_map(grey(0.9999)).x;
	let above = postprocess::tone_map(grey(1.0001)).x;
	require!((above - below).abs() < 1e-3, "the curve is continuous at one: {below} then {above}");
	let under = postprocess::tone_map(grey(0.7999)).x;
	let over = postprocess::tone_map(grey(0.8001)).x;
	require!((over - under).abs() < 1e-3, "and at the knee: {under} then {over}");
	// ON LUMINANCE AND NOT PER CHANNEL: the hue survives, which a per-channel curve shifts most on
	// exactly the saturated colours a wide-gamut path exists for.
	let saturated = Vec3::new(8.0, 2.0, 1.0);
	let mapped = postprocess::tone_map(saturated);
	require!((mapped.x / mapped.y - saturated.x / saturated.y).abs() < 1e-3, "the hue is unchanged, got {mapped:?}");
	Ok(())
}

pub fn fog_exponential_squared() -> Outcome {
	// `f = exp(-(density * distance)^2)`: at one unit of optical depth the surviving fraction is
	// `exp(-1)` = 0.367879, and at two it is `exp(-4)` = 0.018316.
	let surface = Vec3::new(1.0, 1.0, 1.0);
	let dark = Vec3::new(0.0, 0.0, 0.0);
	require!(exact(postprocess::fog(surface, dark, 0.1, 0.0).x, 1.0), "at the camera there is no fog");
	require!(close(postprocess::fog(surface, dark, 1.0, 1.0).x, 0.367_879), "one unit leaves exp(-1), got {}", postprocess::fog(surface, dark, 1.0, 1.0).x);
	require!(close(postprocess::fog(surface, dark, 1.0, 2.0).x, 0.018_316), "two units leave exp(-4), got {}", postprocess::fog(surface, dark, 1.0, 2.0).x);
	// THE SQUARE IS WHAT MAKES THE NEAR FIELD FLAT, which is why it has no visible start plane: a
	// linear fog would already have dimmed a tenth of the way by a tenth.
	require!(postprocess::fog(surface, dark, 1.0, 0.1).x > 0.99, "the near field is flat, got {}", postprocess::fog(surface, dark, 1.0, 0.1).x);
	// AND IT MIXES TOWARD THE FOG'S OWN COLOUR rather than toward black.
	require!(postprocess::fog(dark, surface, 1.0, 10.0).x > 0.99, "far away everything is the fog colour");
	Ok(())
}

pub fn fixed_postprocess_order() -> Outcome {
	// TONE MAPPING IS LAST, and the two orders are a different picture INSIDE the range, where a
	// person can see the difference:
	//   profile's order:  tone_map(0.5 + 0.04 * 8) = tone_map(0.82) = 0.818189
	//   mapped first:     tone_map(0.5) + 0.04 * tone_map(8)        = 0.540878
	let scene = grey(0.5);
	let bloom = grey(8.0);
	let right = postprocess::resolve(scene, bloom, postprocess::BLOOM_WEIGHT);
	let wrong = postprocess::combine(postprocess::tone_map(scene), postprocess::tone_map(bloom), postprocess::BLOOM_WEIGHT);
	require!(close(right.x, 0.818_189), "the profile's order gives 0.818189, got {}", right.x);
	require!(close(wrong.x, 0.540_878), "and the other gives 0.540878, got {}", wrong.x);
	require!(right.x - wrong.x > 0.1, "which is a different picture and not a rounding difference");
	require!(right.x < 1.0 && wrong.x < 1.0, "and both are inside the range, where that difference is visible");
	Ok(())
}

pub fn hdr_target_format() -> Outcome {
	// "A LINEAR HDR RENDER TARGET" ADMITS TWO ANSWERS AND THEY ARE NOT EQUIVALENT, which is why the
	// profile names one. `RGBA32F` doubles the bandwidth of every pass for precision a tone map
	// spends immediately; a normalised format is not HDR at all, because the bloom threshold sits at
	// luminance 1.0 and there would be nothing above it to spread.
	require!(scene3d::environment::HDR_FORMAT == "RGBA16F", "the HDR target is RGBA16F, got {}", scene3d::environment::HDR_FORMAT);
	let target = scene3d::environment::hdr_target_desc(320, 240)?;
	require!(target.format == "RGBA16F", "and so is the target this layer describes, got {}", target.format);
	require!(target.mip_levels == 1, "the target has no mips - the bloom pyramid is its own chain, got {}", target.mip_levels);
	require!(target.usage.sampled && target.usage.colour_attachment, "it is written by the scene pass and read by the post-process ones");

	// AND THE ENVIRONMENT CUBE IS THE SAME FORMAT, with the prefilter levels as its mips: level i is
	// the radiance at roughness i/(levels-1), and the chain stops at 8x8 because below that the
	// filter is wider than the face.
	let cube = scene3d::environment::cube_desc(256)?;
	require!(cube.format == "RGBA16F", "the environment cube is RGBA16F, got {}", cube.format);
	require!(cube.mip_levels == scene3d::environment::prefilter_levels(256), "its mips are the roughness axis, got {}", cube.mip_levels);
	require!(cube.layers == 6, "six faces, checked rather than assumed, got {}", cube.layers);
	Ok(())
}
