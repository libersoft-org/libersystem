//! HDR AND POST-PROCESSING: the bloom threshold and pyramid, the fog, and the tone map that is last.
//!
//! TONE MAPPING IS LAST, AFTER BLOOM AND FOG, and the profile says why: both are defined on LINEAR
//! RADIANCE, and a tone-mapped input would already have compressed the highlights they exist to
//! spread. That ordering is the reason `pbr::shade` does not clamp its own output - a material that
//! saturated at 1 would have thrown the highlights away before bloom ever saw them.
//!
//! THE OPERATOR IS THE 2D IMAGE-COLOUR PROFILE'S, NOT A SECOND ONE. Extended Reinhard on LUMINANCE
//! at that profile's own `WHITE`, which is read from the profile crate rather than written again
//! here. Two operators in one system is two systems, and the one thing that guarantees the 3D frame
//! and a 2D image of it look alike is that the curve is literally the same.
//!
//! ON LUMINANCE AND NOT PER CHANNEL, for the same reason the image profile gives: a per-channel
//! curve shifts hue, and it shifts it most on exactly the saturated colours the wide-gamut path
//! exists for.

use render_math::{Vec3, exp};

/// Rec. 709 luminance in linear light, which is the same triple the image-colour profile uses.
pub const LUMINANCE_REC709: Vec3 = Vec3::new(0.2126, 0.7152, 0.0722);

/// The luminance the bloom knee is centred on.
pub const BLOOM_THRESHOLD: f32 = 1.0;

/// Half the width of the knee: nothing below `THRESHOLD - KNEE`, the whole excess above
/// `THRESHOLD + KNEE`.
pub const BLOOM_KNEE: f32 = 0.5;

/// How many levels the bloom pyramid has.
pub const BLOOM_LEVELS: u32 = 6;

/// How much of the pyramid is added back by default.
pub const BLOOM_WEIGHT: f32 = 0.04;

/// The luminance mapped to 1.0, taken from the image-colour profile so there is one of it.
pub fn tone_map_white() -> f32 {
	graphics_profile::image::tone_map::WHITE as f32
}

/// Linear Rec. 709 luminance.
pub fn luminance(colour: Vec3) -> f32 {
	colour.dot(LUMINANCE_REC709)
}

/// What a fragment contributes to the bloom pyramid.
///
/// A SOFT KNEE AND NOT A HARD THRESHOLD, because a hard one makes a moving highlight pop in and out:
/// a specular glint drifting across a surface crosses the threshold in one frame and the bloom
/// appears at full strength, which reads as a flash.
///
/// THE QUADRATIC IS THE ONE THAT JOINS SMOOTHLY AT BOTH ENDS. `(L - T + K)^2 / (4K)` is zero with
/// zero slope at `T - K` and equals `L - T` with unit slope at `T + K`, so the curve and its
/// derivative are continuous across the whole knee - which is what "and a quadratic between" has to
/// mean for two implementations to agree.
pub fn bloom_prefilter(colour: Vec3) -> Vec3 {
	let light = luminance(colour);
	if !(light > BLOOM_THRESHOLD - BLOOM_KNEE) {
		// WRITTEN AS A NEGATED COMPARISON because every comparison with NaN is false: a NaN
		// luminance contributes nothing rather than spreading through the whole pyramid.
		return Vec3::new(0.0, 0.0, 0.0);
	}
	let excess = if light >= BLOOM_THRESHOLD + BLOOM_KNEE {
		light - BLOOM_THRESHOLD
	} else {
		let over = light - BLOOM_THRESHOLD + BLOOM_KNEE;
		over * over / (4.0 * BLOOM_KNEE)
	};
	if light <= 0.0 {
		return Vec3::new(0.0, 0.0, 0.0);
	}
	// THE COLOUR KEEPS ITS HUE: the excess is a fraction of the luminance and scales all three
	// channels, rather than each channel being thresholded on its own.
	colour.scale(excess / light)
}

/// The 13-tap downsample, whose weights partition unity.
///
/// THIRTEEN TAPS AND NOT A BOX, because a box downsample of a bright point produces a pyramid whose
/// shape depends on where the point fell in the texel grid - the bloom of a moving highlight then
/// pulses as it crosses texel boundaries. The inner four taps at half a texel are what remove it.
pub fn downsample_13(at: (f32, f32), texel: (f32, f32), mut sample: impl FnMut(f32, f32) -> Vec3) -> Vec3 {
	let mut fetch = |x: f32, y: f32| sample(at.0 + x * texel.0, at.1 + y * texel.1);
	let outer = fetch(-2.0, -2.0).add(fetch(2.0, -2.0)).add(fetch(-2.0, 2.0)).add(fetch(2.0, 2.0));
	let edges = fetch(0.0, -2.0).add(fetch(-2.0, 0.0)).add(fetch(2.0, 0.0)).add(fetch(0.0, 2.0));
	let inner = fetch(-1.0, -1.0).add(fetch(1.0, -1.0)).add(fetch(-1.0, 1.0)).add(fetch(1.0, 1.0));
	let centre = fetch(0.0, 0.0);
	// 0.125 + 4 x 0.03125 + 4 x 0.0625 + 4 x 0.125 = 1.
	centre.scale(0.125).add(outer.scale(0.031_25)).add(edges.scale(0.0625)).add(inner.scale(0.125))
}

/// The 9-tap tent upsample, whose weights also partition unity.
pub fn upsample_tent_9(at: (f32, f32), radius: (f32, f32), mut sample: impl FnMut(f32, f32) -> Vec3) -> Vec3 {
	let mut fetch = |x: f32, y: f32| sample(at.0 + x * radius.0, at.1 + y * radius.1);
	let corners = fetch(-1.0, -1.0).add(fetch(1.0, -1.0)).add(fetch(-1.0, 1.0)).add(fetch(1.0, 1.0));
	let edges = fetch(0.0, -1.0).add(fetch(-1.0, 0.0)).add(fetch(1.0, 0.0)).add(fetch(0.0, 1.0));
	let centre = fetch(0.0, 0.0);
	// (1 + 2 + 1 + 2 + 4 + 2 + 1 + 2 + 1) / 16 = 1.
	corners.add(edges.scale(2.0)).add(centre.scale(4.0)).scale(1.0 / 16.0)
}

/// Exponential-squared fog.
///
/// `f = exp(-(density * distance)^2)`, SQUARED AND NOT LINEAR, because the squared form has no
/// visible start plane: a linear fog begins abruptly at a distance the author picked, and that edge
/// sweeps across the world as the camera moves.
///
/// `f` IS HOW MUCH OF THE SURFACE SURVIVES: 1 at the camera and 0 far away, so the mix goes toward
/// the fog colour with distance.
pub fn fog(colour: Vec3, fog_colour: Vec3, density: f32, distance: f32) -> Vec3 {
	if !density.is_finite() || !distance.is_finite() || density <= 0.0 {
		return colour;
	}
	let depth = density * distance.max(0.0);
	let surviving = exp(-(depth * depth)).clamp(0.0, 1.0);
	fog_colour.lerp(colour, surviving)
}

/// Add the pyramid back to the frame.
pub fn combine(scene: Vec3, bloom: Vec3, weight: f32) -> Vec3 {
	scene.add(bloom.scale(weight))
}

/// Extended Reinhard on luminance, at the image-colour profile's white point.
///
/// `Ld = L * (1 + L / WHITE^2) / (1 + L)`, APPLIED TO THE WHOLE RANGE. It is a global operator and
/// compressing everything is what it is for: `WHITE` is "the luminance mapped to 1.0", so a scene
/// whose diffuse white sits at 1 maps to 0.53 and something four times brighter maps to 1. That is
/// an exposure decision the scene makes by choosing its radiances, not something the curve should
/// take back by leaving part of its range alone.
///
/// AND THAT IS WHY THERE IS NO `if L > 1` GUARD HERE, although `graphics-core`'s 2D path has one.
/// The curve is not the identity at 1 - it is 0.53125 there - so a guard that passes values at or
/// below 1 through unchanged and maps everything above produces a STEP: measured, 1.000000 at
/// `L = 1.0` and 0.531280 at `L = 1.0001`, a 47 per cent drop across a boundary that runs through
/// the middle of every lit surface. A guard like that belongs with a curve that IS the identity at
/// its knee, and this one is not. The divergence is recorded where the finding is rather than
/// silently copied here; nothing pins the 2D behaviour, and changing it changes every image that
/// path produces, so it is not this part's to change.
pub fn tone_map(colour: Vec3) -> Vec3 {
	let light = luminance(colour);
	// EVERY COMPARISON WITH NaN IS FALSE, and this is written so a NaN falls through unmapped rather
	// than being scaled by a NaN ratio - which would spread one bad pixel over the whole frame at
	// the next downsample. A luminance of zero falls through for the same reason: the ratio below is
	// a division by it.
	if !matches!(light.partial_cmp(&0.0), Some(core::cmp::Ordering::Greater)) {
		return colour;
	}
	let white = tone_map_white();
	let mapped = light * (1.0 + light / (white * white)) / (1.0 + light);
	colour.scale(mapped / light)
}

/// The whole chain in the profile's order.
///
/// THE SCENE COLOUR ARRIVES ALREADY FOGGED, because fog needs the fragment's own view-space depth
/// and that is known where the fragment is shaded rather than here. The consequence is the order the
/// profile asks for: the bloom pyramid is built over the fogged frame, and tone mapping is last.
pub fn resolve(fogged_scene: Vec3, bloom: Vec3, weight: f32) -> Vec3 {
	tone_map(combine(fogged_scene, bloom, weight))
}
