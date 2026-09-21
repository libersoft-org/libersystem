//! Shadows: the format, the bias, the filter and the cascades.

use render_math::Vec3;
use scene3d::shadow::{self, CubeFace};

use crate::Outcome;
use crate::extended::{claimed, exact};

pub fn shadow_map_depth32_f() -> Outcome {
	// `Depth32F`, AND THE COMPARISON RULE COMES WITH IT: the stored floats are compared and the
	// incoming depth is not quantised. A normalised format quantises because its storage cannot hold
	// anything else, and imposing that on a float format throws away the precision a shadow map is
	// exactly where you spend.
	require!(shadow::MAP_FORMAT == render3d::DepthFormat::Depth32F, "the map is Depth32F, got {:?}", shadow::MAP_FORMAT);
	// AND THE FILTER USES `LessOrEqual`: a surface at exactly its own depth in the map is LIT, which
	// is what keeps a caster from shadowing itself.
	let lit = shadow::percentage_closer(0.5, (0.1, 0.1), (0.5, 0.5), |_, _| 0.5);
	require!(exact(lit, 1.0), "equal depth is lit, got {lit}");
	Ok(())
}

pub fn shadow_depth_bias() -> Outcome {
	// THE CONSTANT TERM IS RELATIVE TO THE DEPTH'S OWN PRECISION, which is what `2^(exponent(z) -
	// 23)` in the 3D profile's equation means. A bias in absolute units is far too small near the
	// camera and far too large at the far plane.
	let near = shadow::representable_step(1.0);
	let far = shadow::representable_step(1024.0);
	require!((far / near - 1024.0).abs() < 1.0, "the step grows with the depth, got a ratio of {}", far / near);
	// THE NAMED VALUES, because a scene tuned against an unstated bias acnes on the next
	// implementation.
	require!(exact(shadow::BIAS_CONSTANT, 0.0015), "the constant factor is 0.0015, got {}", shadow::BIAS_CONSTANT);
	require!(exact(shadow::BIAS_SLOPE, 2.0), "the slope factor is 2.0, got {}", shadow::BIAS_SLOPE);
	require!(exact(shadow::BIAS_CLAMP, 0.01), "the clamp is 0.01, got {}", shadow::BIAS_CLAMP);
	let sloped = shadow::depth_bias(1.0, 0.001);
	require!(exact(sloped, shadow::BIAS_CONSTANT * near + shadow::BIAS_SLOPE * 0.001), "the equation is constant * r + slope * m, got {sloped}");
	// AND IT IS BOUNDED, because a polygon almost parallel to the light has an unbounded derivative
	// and an unbounded bias detaches the shadow from what casts it.
	require!(exact(shadow::depth_bias(1.0, 1000.0), shadow::BIAS_CLAMP), "the bias is bounded whatever the slope");
	Ok(())
}

pub fn percentage_closer_filter_3x3() -> Outcome {
	// COMPARE THEN FILTER, NOT FILTER THEN COMPARE. Three of nine taps behind the receiver is three
	// ninths lit; averaging the depths first gives 0.367, compares once, and calls the whole
	// footprint shadowed - which is a halo around every silhouette.
	let lit = shadow::percentage_closer(0.5, (0.1, 0.1), (0.5, 0.5), |u, _| if u > 0.5 { 0.9 } else { 0.1 });
	require!(exact(lit, 3.0 / 9.0), "three of nine taps are behind the receiver, got {lit}");
	// EQUAL WEIGHTS: a corner tap counts as much as the centre one, so the answer is one of ten
	// values.
	let corner = shadow::percentage_closer(0.5, (0.1, 0.1), (0.5, 0.5), |u, v| if u < 0.45 && v < 0.45 { 0.9 } else { 0.1 });
	require!(exact(corner, 1.0 / 9.0), "one corner tap is one ninth, got {corner}");
	require!(shadow::PCF_TAPS == 9, "the kernel is 3 by 3, got {} taps", shadow::PCF_TAPS);
	require!(exact(shadow::percentage_closer(0.0, (0.1, 0.1), (0.5, 0.5), |_, _| 1.0), 1.0), "a wholly lit footprint is one");
	require!(exact(shadow::percentage_closer(1.0, (0.1, 0.1), (0.5, 0.5), |_, _| 0.0), 0.0), "and a wholly shadowed one is zero");
	Ok(())
}

pub fn cascaded_shadow_maps() -> Outcome {
	let splits = shadow::split_distances(1.0, 100.0, 4, shadow::CASCADE_SPLIT_LAMBDA, &claimed())?;
	require!(splits.len() == 4, "four cascades give four splits, got {}", splits.len());
	// THE SPLITS ASCEND, which is what makes the first-match walk correct.
	for pair in splits.windows(2) {
		require!(pair[1] > pair[0], "the splits ascend: {} then {}", pair[0], pair[1]);
	}
	// AND THE LAST IS THE FAR PLANE EXACTLY, so a fragment at the far plane does not fall off the
	// end of the last cascade by a few bits.
	require!(exact(splits[3], 100.0), "the last split is the far plane, got {}", splits[3]);
	Ok(())
}

pub fn cascade_split_blend() -> Outcome {
	// THE PRACTICAL SCHEME AT LAMBDA 0.5, worked out from the profile for near 1 and far 100:
	//   logarithmic: 3.1623, 10, 31.623, 100
	//   uniform:     25.75, 50.5, 75.25, 100
	//   blended:     14.456, 30.25, 53.436, 100
	let splits = shadow::split_distances(1.0, 100.0, 4, 0.5, &claimed())?;
	for (index, expected) in [(0usize, 14.456f32), (1, 30.25), (2, 53.436)] {
		require!((splits[index] - expected).abs() < 0.05, "split {index} is {expected}, got {}", splits[index]);
	}
	// AND THE ENDS ARE THE TWO SCHEMES THEMSELVES, so a wrong blend cannot hide in the middle.
	let uniform = shadow::split_distances(1.0, 100.0, 4, 0.0, &claimed())?;
	require!((uniform[0] - 25.75).abs() < 0.01, "lambda 0 is the uniform scheme, got {}", uniform[0]);
	let logarithmic = shadow::split_distances(1.0, 100.0, 4, 1.0, &claimed())?;
	require!((logarithmic[0] - 3.1623).abs() < 0.01, "lambda 1 is the logarithmic one, got {}", logarithmic[0]);
	require!(exact(shadow::CASCADE_SPLIT_LAMBDA, 0.5), "the profile's lambda is 0.5, got {}", shadow::CASCADE_SPLIT_LAMBDA);
	Ok(())
}

pub fn per_fragment_cascade_selection() -> Outcome {
	// BY THE FRAGMENT'S OWN DEPTH AND NOT THE DRAWABLE'S, which the profile states because a large
	// object spans cascades: choosing once per drawable puts the far end of a long wall in the near
	// cascade's map, where it is outside the map's extent entirely.
	let splits = shadow::split_distances(1.0, 100.0, 4, 0.5, &claimed())?;
	let near_end = shadow::cascade_for(2.0, &splits);
	let far_end = shadow::cascade_for(90.0, &splits);
	require!(near_end == Some(0), "the near end of a long wall is in the first cascade, got {near_end:?}");
	require!(far_end == Some(3), "and its far end is in the last, got {far_end:?}");
	require!(near_end != far_end, "one drawable spans cascades, which is why the choice is per fragment");
	Ok(())
}

pub fn cascade_transition_blend() -> Outcome {
	// A BLEND OVER THE LAST TENTH OF EACH RANGE. Cascade 0 runs from the near plane at 1 to 14.456,
	// a range of 13.456; its last tenth begins at 13.110, so 13.0 is wholly inside and 13.783 is
	// halfway through.
	let near = 1.0f32;
	let splits = shadow::split_distances(near, 100.0, 4, 0.5, &claimed())?;
	require!(exact(shadow::cascade_blend(13.0, near, &splits, 0), 0.0), "before the last tenth there is no blend");
	let halfway = shadow::cascade_blend(13.783, near, &splits, 0);
	require!((halfway - 0.5).abs() < 0.03, "halfway through the transition is 0.5, got {halfway}");
	require!((shadow::cascade_blend(14.456, near, &splits, 0) - 1.0).abs() < 1e-2, "at the split it is wholly the next cascade");
	require!(exact(shadow::CASCADE_BLEND_FRACTION, 0.1), "the transition is a tenth, got {}", shadow::CASCADE_BLEND_FRACTION);
	Ok(())
}

pub fn unshadowed_beyond_last_cascade() -> Outcome {
	// EXTENDING THE LAST CASCADE TO INFINITY MAKES ITS TEXELS USELESS EVERYWHERE, so the profile
	// says where the shadows stop and lets a scene choose its far distance.
	let splits = shadow::split_distances(1.0, 100.0, 4, 0.5, &claimed())?;
	require!(shadow::cascade_for(100.0, &splits) == Some(3), "the far plane itself is still in the last cascade");
	require!(shadow::cascade_for(100.1, &splits).is_none(), "and past it there is no cascade, which means unshadowed");
	// AND THE LAST CASCADE BLENDS INTO NOTHING, because fading into no shadow is what unshadowed
	// already looks like.
	require!(exact(shadow::cascade_blend(99.9, 1.0, &splits, 3), 0.0), "the last cascade has nothing to blend into");
	Ok(())
}

pub fn cascade_count_refusal() -> Outcome {
	// REFUSED RATHER THAN SILENTLY GIVEN FEWER, which is the profile's own word: a scene that asked
	// for six and got four would render with a far distance it did not choose.
	let too_many = shadow::split_distances(1.0, 100.0, claimed().max_shadow_cascades + 1, 0.5, &claimed());
	require!(too_many.is_err(), "a cascade count past the limit is refused");
	require!(shadow::split_distances(1.0, 100.0, 0, 0.5, &claimed()).is_err(), "and so is none at all");
	require!(shadow::split_distances(0.0, 100.0, 4, 0.5, &claimed()).is_err(), "a near plane at zero is refused");
	require!(shadow::split_distances(100.0, 100.0, 4, 0.5, &claimed()).is_err(), "and a far plane at near");
	require!(shadow::split_distances(1.0, 100.0, 4, 1.5, &claimed()).is_err(), "and a lambda outside [0, 1]");
	Ok(())
}

pub fn point_light_cube_shadow() -> Outcome {
	// THE FACE IS CHOSEN BY THE MAJOR AXIS, in the 3D profile's index order - so a cube built for
	// any other system loads without a flip.
	for (direction, face) in [
		(Vec3::new(2.0, 1.0, 1.0), CubeFace::PositiveX),
		(Vec3::new(-2.0, 1.0, 1.0), CubeFace::NegativeX),
		(Vec3::new(1.0, 2.0, 1.0), CubeFace::PositiveY),
		(Vec3::new(1.0, -2.0, 1.0), CubeFace::NegativeY),
		(Vec3::new(1.0, 1.0, 2.0), CubeFace::PositiveZ),
		(Vec3::new(1.0, 1.0, -2.0), CubeFace::NegativeZ),
	] {
		require!(shadow::cube_face(direction) == Some(face), "{direction:?} falls on {face:?}, got {:?}", shadow::cube_face(direction));
	}
	// A TIE GOES TO THE EARLIER AXIS, so a direction exactly on a face edge picks one face rather
	// than depending on which comparison was evaluated first.
	require!(shadow::cube_face(Vec3::new(1.0, 1.0, 0.0)) == Some(CubeFace::PositiveX), "a tie goes to the earlier axis");
	require!(shadow::cube_face(Vec3::new(0.0, 0.0, 0.0)).is_none(), "a direction that is not one is answered rather than guessed at");
	Ok(())
}
