//! SHADOWS: the bias, the filter, and which cascade a fragment belongs to.
//!
//! EVERY NUMBER HERE IS THE PROFILE'S. A shadow map's picture is decided almost entirely by
//! parameters that look like tuning and are not: a scene authored against an unstated bias acnes on
//! the next implementation, a 3x3 kernel with Gaussian weights softens differently from one with
//! equal weights, and two renderers splitting their cascades by different rules put the resolution
//! change in different places. The profile states all of them, and this module is where they live
//! once rather than at each call site.
//!
//! THE BIAS IS APPLIED WHERE THE 3D PROFILE PUTS IT: in the SHADOW PASS, through the depth-bias
//! equation, added after the depth is computed and before the test and the write. It is not a
//! subtraction in the lighting pass - that is the other common arrangement, and having both would
//! bias twice.
//!
//! AND WHAT IS OUTSIDE THE LAST CASCADE IS UNSHADOWED. Extending the last cascade to infinity makes
//! its texels useless everywhere, so the profile says where the shadows stop and lets a scene choose
//! its own far distance.

use alloc::vec::Vec;

use render_math::{Vec3, powf};

use crate::scene::{Error, Limits};

/// The format a shadow map is stored in.
///
/// `Depth32F`, AND THE COMPARISON RULE COMES WITH IT: the stored floats are compared and the
/// incoming depth is NOT quantised, which is what the 3D profile fixes for this format. A normalised
/// format quantises because its storage cannot hold anything else; imposing that on a float format
/// would throw away the precision the format exists for - and a shadow map is exactly where that
/// precision is spent.
pub const MAP_FORMAT: render3d::DepthFormat = render3d::DepthFormat::Depth32F;

/// The constant factor of the depth bias.
pub const BIAS_CONSTANT: f32 = 0.0015;

/// The slope-scaled factor.
pub const BIAS_SLOPE: f32 = 2.0;

/// The bound on the bias, whatever the slope.
pub const BIAS_CLAMP: f32 = 0.01;

/// How far into a cascade's far end the blend to the next one runs.
pub const CASCADE_BLEND_FRACTION: f32 = 0.1;

/// The blend between the uniform and logarithmic split schemes.
pub const CASCADE_SPLIT_LAMBDA: f32 = 0.5;

/// The percentage-closer kernel is 3 by 3 with equal weights, so nine taps.
pub const PCF_TAPS: u32 = 9;

/// The smallest representable difference at a float depth's own precision.
///
/// `2^(exponent(z) - 23)`, WHICH IS WHAT THE PROFILE'S EQUATION MEANS BY `r`. A constant bias in
/// absolute units is wrong at both ends of the range: far too small near the camera, where the
/// float's step is tiny, and far too large at the far plane, where it is not.
pub fn representable_step(depth: f32) -> f32 {
	if !depth.is_finite() {
		return 0.0;
	}
	let bits = depth.abs().to_bits();
	let exponent = ((bits >> 23) & 0xff) as i32;
	if exponent == 0 {
		// A subnormal depth: the step is the smallest subnormal itself.
		return f32::from_bits(1);
	}
	let stepped = exponent - 23;
	if stepped <= 0 {
		return f32::from_bits(1);
	}
	f32::from_bits((stepped as u32) << 23)
}

/// The offset the shadow pass adds to a fragment's depth.
///
/// `offset = constant * r + slope * m`, where `m` is the largest of the primitive's depth
/// derivatives - which is what makes the bias grow on a surface seen nearly edge-on to the light,
/// where one texel of the map spans a long way along the surface and acne appears first.
///
/// CLAMPED, BECAUSE A SLOPE CAN BE ENORMOUS. A polygon almost parallel to the light direction has an
/// unbounded derivative, and an unbounded bias detaches the shadow from whatever casts it.
pub fn depth_bias(depth: f32, maximum_slope: f32) -> f32 {
	let offset = BIAS_CONSTANT * representable_step(depth) + BIAS_SLOPE * maximum_slope.abs();
	offset.min(BIAS_CLAMP)
}

/// The 3x3 percentage-closer filter: compare first, then average, with EQUAL weights.
///
/// COMPARE-THEN-FILTER AND NOT THE OTHER WAY ROUND. Averaging nine depths and comparing once gives a
/// wrong answer at every depth discontinuity - the mean of a near and a far occluder is a depth
/// neither of them has - and the difference is a halo around every silhouette.
///
/// EQUAL WEIGHTS RATHER THAN A GAUSSIAN, because the sample positions are already a box: weighting a
/// box by a bell approximates neither, and the profile picks the one that is what it looks like.
///
/// The answer is the LIT fraction, so one of ten values from zero to one.
pub fn percentage_closer(receiver: f32, texel: (f32, f32), at: (f32, f32), mut stored: impl FnMut(f32, f32) -> f32) -> f32 {
	let mut lit = 0u32;
	for row in -1i32..=1 {
		for column in -1i32..=1 {
			let u = at.0 + column as f32 * texel.0;
			let v = at.1 + row as f32 * texel.1;
			// `incoming OP stored`, with `LessOrEqual` - the 3D profile's own order and the
			// operation that keeps a surface lit against its own depth in the map.
			if receiver <= stored(u, v) {
				lit += 1;
			}
		}
	}
	lit as f32 / PCF_TAPS as f32
}

/// The cascade far distances, in VIEW-SPACE DEPTH.
///
/// THE PRACTICAL SPLIT SCHEME, blended between uniform and logarithmic at the profile's lambda of
/// 0.5. The logarithmic scheme alone puts almost every texel near the camera and leaves the distance
/// unresolved; the uniform one alone wastes the near cascades on geometry that occupies few pixels.
/// Neither is right and the blend is what every published implementation uses, so the profile fixes
/// the blend rather than the scheme.
pub fn split_distances(near: f32, far: f32, cascades: u32, lambda: f32, limits: &Limits) -> Result<Vec<f32>, Error> {
	if cascades == 0 {
		return Err(Error::Degenerate { reason: "a cascaded shadow needs at least one cascade" });
	}
	if cascades > limits.max_shadow_cascades {
		return Err(Error::LimitExceeded { limit: "max_shadow_cascades", ceiling: limits.max_shadow_cascades, asked: cascades });
	}
	if !near.is_finite() || !far.is_finite() || near <= 0.0 || far <= near {
		return Err(Error::Degenerate { reason: "a cascade split needs a positive near and a far beyond it" });
	}
	if !lambda.is_finite() || !(0.0..=1.0).contains(&lambda) {
		return Err(Error::Degenerate { reason: "a cascade split's lambda lies in [0, 1]" });
	}
	let mut splits: Vec<f32> = Vec::with_capacity(cascades as usize);
	let ratio = far / near;
	for index in 1..=cascades {
		let fraction = index as f32 / cascades as f32;
		let logarithmic = near * powf(ratio, fraction);
		let uniform = near + (far - near) * fraction;
		splits.push(lambda * logarithmic + (1.0 - lambda) * uniform);
	}
	// THE LAST SPLIT IS THE FAR PLANE EXACTLY. The blend of two schemes that both end at `far` ends
	// at `far` in real arithmetic and a few bits away from it in this one, and a fragment at exactly
	// the far plane must not fall off the end of the last cascade.
	if let Some(last) = splits.last_mut() {
		*last = far;
	}
	Ok(splits)
}

/// Which cascade a fragment's own view-space depth belongs to, or `None` beyond the last.
///
/// BY THE FRAGMENT'S DEPTH AND NOT THE DRAWABLE'S, which the profile states because a large object
/// spans cascades: choosing once per drawable puts the far end of a long wall in the near cascade's
/// map, where it is outside the map's extent entirely.
pub fn cascade_for(view_depth: f32, splits: &[f32]) -> Option<u32> {
	if !view_depth.is_finite() {
		return None;
	}
	splits.iter().position(|split| view_depth <= *split).map(|index| index as u32)
}

/// How far into the transition to the next cascade a fragment is: 0 wholly in this one, 1 wholly in
/// the next.
///
/// A BLEND OVER THE LAST TENTH OF EACH CASCADE'S RANGE, so the seam is a gradient rather than a line.
/// Without it the resolution change is a visible edge across the ground at a fixed distance from the
/// camera, which moves with the camera and reads as a fault in the world.
pub fn cascade_blend(view_depth: f32, near: f32, splits: &[f32], cascade: u32) -> f32 {
	let index = cascade as usize;
	let Some(end) = splits.get(index) else { return 0.0 };
	if index + 1 >= splits.len() {
		// THE LAST CASCADE BLENDS INTO NOTHING, because what is beyond it is unshadowed rather than
		// another map: fading into no shadow at all is what "unshadowed" already looks like.
		return 0.0;
	}
	let start = if index == 0 { near } else { splits[index - 1] };
	let range = end - start;
	if range <= 0.0 {
		return 0.0;
	}
	let transition = range * CASCADE_BLEND_FRACTION;
	if transition <= 0.0 {
		return 0.0;
	}
	((view_depth - (end - transition)) / transition).clamp(0.0, 1.0)
}

/// The six cube faces in the 3D profile's index order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CubeFace {
	PositiveX,
	NegativeX,
	PositiveY,
	NegativeY,
	PositiveZ,
	NegativeZ,
}

/// Which face of a point light's shadow cube a direction falls on.
///
/// BY THE MAJOR AXIS, which is the standard cube-map selection the 3D profile adopts so a cube built
/// for any other system loads without a flip. A TIE GOES TO THE EARLIER AXIS in that index order, so
/// a direction exactly on a face edge picks one face rather than depending on comparison order.
pub fn cube_face(direction: Vec3) -> Option<CubeFace> {
	if !direction.is_finite() {
		return None;
	}
	let (x, y, z) = (direction.x.abs(), direction.y.abs(), direction.z.abs());
	if x == 0.0 && y == 0.0 && z == 0.0 {
		return None;
	}
	if x >= y && x >= z {
		return Some(if direction.x >= 0.0 { CubeFace::PositiveX } else { CubeFace::NegativeX });
	}
	if y >= z {
		return Some(if direction.y >= 0.0 { CubeFace::PositiveY } else { CubeFace::NegativeY });
	}
	Some(if direction.z >= 0.0 { CubeFace::PositiveZ } else { CubeFace::NegativeZ })
}
