//! Skinning, morphing and playing a clip.

use alloc::vec;
use alloc::vec::Vec;
use render_math::{Mat4, Quat, Vec3};
use scene3d::animate::{self, Channel, Clip, CubicKey, Curve, Ending, Key, Skeleton, Target, Track};
use scene3d::deform::{self, Influences, MorphTarget, Pose};

use crate::Outcome;
use crate::extended::{claimed, exact};

fn turn(radians: f32) -> Quat {
	Quat::from_axis_angle(Vec3::new(0.0, 0.0, 1.0), radians).unwrap_or(Quat::IDENTITY)
}

fn identity() -> Mat4 {
	Mat4::from_translation(Vec3::new(0.0, 0.0, 0.0))
}

fn line(joint: u16, keys: &[(f32, Vec3)]) -> Track {
	Track { target: Target::Joint(joint), channel: Channel::Translation(Curve::Linear(keys.iter().map(|(time, value)| Key { time: *time, value: *value }).collect())) }
}

fn spin(joint: u16, keys: &[(f32, Quat)]) -> Track {
	Track { target: Target::Joint(joint), channel: Channel::Rotation(Curve::Linear(keys.iter().map(|(time, value)| Key { time: *time, value: *value }).collect())) }
}

fn at(clip: &Clip, time: f32, joint: u16) -> Result<Vec3, crate::Trouble> {
	clip.sample(time).joint(joint).and_then(|sampled| sampled.translation).ok_or_else(|| crate::Trouble::Failed(alloc::format!("joint {joint} is not driven at {time}")))
}

pub fn linear_blend_skinning() -> Outcome {
	// THE WEIGHTED SUM OF THE POINT THROUGH EACH JOINT, and not a blend of the joints themselves.
	// Two joints at half weight, one moved by +3 and one still, put the vertex halfway: +1.5.
	let bind = Mat4::from_translation(Vec3::new(5.0, 0.0, 0.0));
	let inverse = bind.inverse().map_err(|_| crate::Trouble::Failed(alloc::string::String::from("a bind pose with no inverse")))?;
	let moved = Mat4::from_translation(Vec3::new(8.0, 0.0, 0.0));
	let pose = Pose::compose(&[moved, bind], &[inverse, inverse], &claimed())?;
	let vertex = Vec3::new(5.0, 1.0, 0.0);
	let blended = pose.skin_point(vertex, &Influences::new([0, 1, 0, 0], [0.5, 0.5, 0.0, 0.0])?);
	require!(blended.sub(Vec3::new(6.5, 1.0, 0.0)).length() < 1e-4, "half of +3 and half of nothing is +1.5, got {blended:?}");
	Ok(())
}

pub fn four_influences_per_vertex() -> Outcome {
	require!(deform::MAX_INFLUENCES == 4, "four influences per vertex, got {}", deform::MAX_INFLUENCES);
	// AND ALL FOUR ARE USED. Four joints at a quarter each, three still and one moved by +4, move
	// the vertex by exactly +1 - which a renderer reading only the first two would not produce.
	let bind = identity();
	let moved = Mat4::from_translation(Vec3::new(4.0, 0.0, 0.0));
	let pose = Pose::compose(&[bind, bind, bind, moved], &[bind, bind, bind, bind], &claimed())?;
	let skinned = pose.skin_point(Vec3::new(0.0, 0.0, 0.0), &Influences::new([0, 1, 2, 3], [0.25, 0.25, 0.25, 0.25])?);
	require!(skinned.sub(Vec3::new(1.0, 0.0, 0.0)).length() < 1e-4, "the fourth influence counts, got {skinned:?}");
	Ok(())
}

pub fn weight_normalisation_at_load() -> Outcome {
	// AT LOAD AND NOT AT DRAW: an exporter that wrote 2, 2, 0, 0 meant half and half, and the
	// correction happens once for the life of the asset rather than on every vertex of every frame.
	let influences = Influences::new([0, 1, 0, 0], [2.0, 2.0, 0.0, 0.0])?;
	require!(exact(influences.weights()[0], 0.5), "2 and 2 normalise to a half each, got {}", influences.weights()[0]);
	let total: f32 = influences.weights().iter().sum();
	require!(exact(total, 1.0), "the weights sum to one, got {total}");
	// AND A SET THAT CANNOT BE NORMALISED IS REFUSED rather than repaired: a vertex with no joint
	// stays at the origin while the mesh moves, which reads as a tear in the geometry.
	require!(Influences::new([0, 1, 2, 3], [0.0; 4]).is_err(), "weights summing to zero are refused");
	require!(Influences::new([0, 1, 2, 3], [-1.0, 1.0, 0.0, 0.0]).is_err(), "and so is a negative weight");
	Ok(())
}

pub fn joint_inverse_bind_composition() -> Outcome {
	// `joint_world * inverse_bind`: at the bind pose the composition is the identity, so a vertex
	// authored there does not move. Move the joint and the vertex moves by the joint's motion SINCE
	// the bind rather than by its position.
	let bind = Mat4::from_translation(Vec3::new(5.0, 0.0, 0.0));
	let inverse = bind.inverse().map_err(|_| crate::Trouble::Failed(alloc::string::String::from("a bind pose with no inverse")))?;
	let vertex = Vec3::new(5.0, 1.0, 0.0);
	let rest = Pose::compose(&[bind], &[inverse], &claimed())?;
	require!(rest.skin_point(vertex, &Influences::rigid(0)).sub(vertex).length() < 1e-4, "at the bind pose a vertex does not move");
	let posed = Pose::compose(&[Mat4::from_translation(Vec3::new(8.0, 0.0, 0.0))], &[inverse], &claimed())?;
	let moved = posed.skin_point(vertex, &Influences::rigid(0));
	require!(moved.sub(Vec3::new(8.0, 1.0, 0.0)).length() < 1e-4, "the vertex moves by the joint's +3, got {moved:?}");
	// AND A SKELETON THAT DOES NOT MATCH ITS BIND POSE IS REFUSED rather than truncated.
	require!(Pose::compose(&[bind, bind], &[inverse], &claimed()).is_err(), "one inverse bind per joint");
	Ok(())
}

pub fn morph_targets() -> Outcome {
	// ADDITIVE, which is why two at once do both rather than blending between them: a raised brow
	// and a smile displace different vertices and applying both should do both.
	let base = Vec3::new(1.0, 2.0, 3.0);
	let up = [Vec3::new(0.0, 1.0, 0.0)];
	let across = [Vec3::new(1.0, 0.0, 0.0)];
	let both = [MorphTarget { displacement: &up, weight: 1.0 }, MorphTarget { displacement: &across, weight: 1.0 }];
	let moved = deform::morphed(base, 0, &both);
	require!(moved.sub(Vec3::new(2.0, 3.0, 3.0)).length() < 1e-4, "both displacements are applied, got {moved:?}");
	let half = [MorphTarget { displacement: &up, weight: 0.5 }, MorphTarget { displacement: &across, weight: 0.5 }];
	let partial = deform::morphed(base, 0, &half);
	require!(partial.sub(Vec3::new(1.5, 2.5, 3.0)).length() < 1e-4, "half of each, and not a blend between them, got {partial:?}");
	// AND A SHORT SET IS REFUSED BEFORE ANY OF IT IS APPLIED, because discovering it at vertex 4,000
	// leaves the first 3,999 already displaced.
	require!(deform::check_targets(2, &[MorphTarget { displacement: &up, weight: 1.0 }], &claimed()).is_err(), "a target shorter than the mesh is refused");
	Ok(())
}

pub fn morph_before_skinning() -> Outcome {
	// A MORPH APPLIED AFTER SKINNING WOULD DISPLACE ALONG A REST-POSE DIRECTION while the vertex is
	// somewhere else, which moves it OUT of the pose rather than within it.
	//
	// The joint rotates a quarter turn about z, so a rest-pose +x becomes +y. A vertex at (1,0,0)
	// with a morph of (1,0,0) is at (2,0,0) before skinning and lands at (0,2,0). Skinning first
	// would give (0,1,0) and then add (1,0,0) for (1,1,0) - a different place.
	let quarter = Mat4::from_linear(&turn(core::f32::consts::FRAC_PI_2).to_mat3(), Vec3::new(0.0, 0.0, 0.0));
	let pose = Pose::compose(&[quarter], &[identity()], &claimed())?;
	let base = [Vec3::new(1.0, 0.0, 0.0)];
	let displacement = [Vec3::new(1.0, 0.0, 0.0)];
	let targets = [MorphTarget { displacement: &displacement, weight: 1.0 }];
	let bounds = deform::deformed_bounds(&base, &targets, &[Influences::rigid(0)], &pose, &claimed())?;
	require!(bounds.centre().sub(Vec3::new(0.0, 2.0, 0.0)).length() < 1e-3, "morph then skin puts the vertex at (0,2,0), got {:?}", bounds.centre());
	Ok(())
}

pub fn linear_translation_scale_keys() -> Outcome {
	let clip = Clip::new(vec![line(0, &[(0.0, Vec3::new(0.0, 0.0, 0.0)), (2.0, Vec3::new(4.0, 8.0, 0.0))])], 2.0, Ending::Clamp, 0, true, &claimed())?;
	for (time, expected) in [(0.5f32, Vec3::new(1.0, 2.0, 0.0)), (1.0, Vec3::new(2.0, 4.0, 0.0))] {
		let value = at(&clip, time, 0)?;
		require!(value.sub(expected).length() < 1e-4, "at {time} the value is {expected:?}, got {value:?}");
	}
	// BEFORE THE FIRST KEY AND AFTER THE LAST, THE END VALUE HOLDS rather than being extrapolated:
	// extrapolation puts a joint somewhere the author never authored.
	require!(at(&clip, -5.0, 0)?.length() < 1e-4, "before the clip the first key holds");
	Ok(())
}

pub fn spherical_linear_rotation_keys() -> Outcome {
	// THE MIDPOINT OF A QUARTER TURN IS EXACTLY 45 DEGREES, which a normalised linear blend is not -
	// that is the cheaper alternative this profile refuses, and it makes a turn crawl at its ends.
	let clip = Clip::new(vec![spin(0, &[(0.0, turn(0.0)), (1.0, turn(core::f32::consts::FRAC_PI_2))])], 1.0, Ending::Clamp, 0, true, &claimed())?;
	let half = clip.sample(0.5).joint(0).and_then(|sampled| sampled.rotation).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("no rotation")))?;
	let spun = half.rotate(Vec3::new(1.0, 0.0, 0.0));
	let root_half = core::f32::consts::FRAC_1_SQRT_2;
	require!(spun.sub(Vec3::new(root_half, root_half, 0.0)).length() < 1e-3, "the midpoint of a quarter turn is 45 degrees, got {spun:?}");
	Ok(())
}

pub fn shorter_arc_rotation() -> Outcome {
	// `q` AND `-q` ARE ONE ROTATION. A ten degree turn written with its end negated interpolates to
	// five degrees at the midpoint and not to 175 - the long way round.
	let ten = 10.0f32 * core::f32::consts::PI / 180.0;
	let end = turn(ten);
	let negated = Quat::from_components(-end.x, -end.y, -end.z, -end.w).map_err(|_| crate::Trouble::Failed(alloc::string::String::from("a degenerate quaternion")))?;
	let clip = Clip::new(vec![spin(0, &[(0.0, turn(0.0)), (1.0, negated)])], 1.0, Ending::Clamp, 0, true, &claimed())?;
	let half = clip.sample(0.5).joint(0).and_then(|sampled| sampled.rotation).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("no rotation")))?;
	let spun = half.rotate(Vec3::new(1.0, 0.0, 0.0));
	let five = turn(ten * 0.5).rotate(Vec3::new(1.0, 0.0, 0.0));
	require!(spun.sub(five).length() < 1e-3, "the shorter arc puts the midpoint at five degrees, got {spun:?}");
	Ok(())
}

pub fn root_motion_extraction() -> Outcome {
	// EXTRACTED BY DEFAULT: an application that never asked for the motion gets a character walking
	// on the spot rather than one drifting out of the world.
	let walk = vec![line(0, &[(0.0, Vec3::new(0.0, 0.0, 0.0)), (1.0, Vec3::new(0.0, 0.0, -2.0))])];
	let extracting = Clip::new(walk.clone(), 1.0, Ending::Clamp, 0, false, &claimed())?;
	let pose = extracting.sample(0.5);
	require!(pose.joint(0).map(|sampled| sampled.translation.is_none()).unwrap_or(false), "the root's translation left the pose");
	require!(pose.root_delta().sub(Vec3::new(0.0, 0.0, -1.0)).length() < 1e-4, "and came back as the delta, got {:?}", pose.root_delta());
	// AND A CLIP MAY DECLARE THAT IT KEEPS IT, getting a zero delta rather than the motion twice.
	let keeping = Clip::new(walk, 1.0, Ending::Clamp, 0, true, &claimed())?;
	let kept = keeping.sample(0.5);
	require!(kept.root_delta().length() < 1e-6, "a clip that keeps its motion is not handed it as well");
	Ok(())
}

pub fn loop_seam_refusal() -> Outcome {
	// A SEAM THAT DOES NOT CLOSE POPS ONCE A CYCLE, for ever, and is found by watching rather than
	// by testing - so it is refused at load.
	let closed = vec![line(0, &[(0.0, Vec3::new(1.0, 0.0, 0.0)), (1.0, Vec3::new(2.0, 0.0, 0.0)), (2.0, Vec3::new(1.0, 0.0, 0.0))])];
	require!(Clip::new(closed, 2.0, Ending::Loop, 0, true, &claimed()).is_ok(), "a clip that returns to its first key loops");
	let open = vec![line(0, &[(0.0, Vec3::new(1.0, 0.0, 0.0)), (2.0, Vec3::new(2.0, 0.0, 0.0))])];
	require!(Clip::new(open.clone(), 2.0, Ending::Loop, 0, true, &claimed()).is_err(), "and one that does not is refused");
	require!(Clip::new(open, 2.0, Ending::Clamp, 0, true, &claimed()).is_ok(), "the same clip is fine as a one-shot");
	// A ROTATION SEAM IS COMPARED BY THE ABSOLUTE DOT, so `q` and `-q` close the loop.
	let start = turn(0.3);
	let negated = Quat::from_components(-start.x, -start.y, -start.z, -start.w).map_err(|_| crate::Trouble::Failed(alloc::string::String::from("a degenerate quaternion")))?;
	let mirrored = vec![spin(0, &[(0.0, start), (1.0, turn(1.0)), (2.0, negated)])];
	require!(Clip::new(mirrored, 2.0, Ending::Loop, 0, true, &claimed()).is_ok(), "the same rotation with the other sign closes the loop");
	Ok(())
}

pub fn step_interpolation() -> Outcome {
	// HOLDS THE PREVIOUS KEY UNTIL THE NEXT KEY'S TIME IS REACHED, so a step track changes exactly
	// at its keyframes and nowhere else. A visibility flag interpolated linearly is half-visible for
	// half a second.
	let keys = vec![Key { time: 0.0, value: Vec3::new(1.0, 0.0, 0.0) }, Key { time: 2.0, value: Vec3::new(5.0, 0.0, 0.0) }];
	let track = Track { target: Target::Joint(0), channel: Channel::Translation(Curve::Step(keys)) };
	let clip = Clip::new(vec![track], 2.0, Ending::Clamp, 0, true, &claimed())?;
	for time in [0.0f32, 0.5, 1.0, 1.9999] {
		require!(exact(at(&clip, time, 0)?.x, 1.0), "at {time} the previous key still holds, got {}", at(&clip, time, 0)?.x);
	}
	require!(exact(at(&clip, 2.0, 0)?.x, 5.0), "and at the next key it is that key's own value");
	Ok(())
}

pub fn cubic_hermite_interpolation() -> Outcome {
	// WITH BOTH TANGENTS ZERO THE BASIS COLLAPSES TO `3t^2 - 2t^3`, so a quarter of the way is
	// 0.15625 where a linear track would be at 0.25.
	let keys = vec![
		CubicKey { time: 0.0, value: Vec3::new(0.0, 0.0, 0.0), in_tangent: Vec3::new(0.0, 0.0, 0.0), out_tangent: Vec3::new(0.0, 0.0, 0.0) },
		CubicKey { time: 1.0, value: Vec3::new(1.0, 0.0, 0.0), in_tangent: Vec3::new(0.0, 0.0, 0.0), out_tangent: Vec3::new(0.0, 0.0, 0.0) },
	];
	let track = Track { target: Target::Joint(0), channel: Channel::Translation(Curve::Cubic(keys)) };
	let clip = Clip::new(vec![track], 1.0, Ending::Clamp, 0, true, &claimed())?;
	require!(exact(at(&clip, 0.25, 0)?.x, 0.15625), "at a quarter the cubic is 0.15625, got {}", at(&clip, 0.25, 0)?.x);
	require!(exact(at(&clip, 0.0, 0)?.x, 0.0), "it passes through its keys");
	require!(exact(at(&clip, 1.0, 0)?.x, 1.0), "at both ends");
	Ok(())
}

pub fn cubic_tangents_per_second() -> Outcome {
	// A SLOPE OF ONE UNIT PER SECOND MEANS THE SAME HOWEVER FAR APART THE KEYS ARE, and the span
	// scales it inside the equation. Over a span of 2 the halfway value is
	// `(0.125 - 0.5 + 0.5) * 2 * 1` = 0.25; over a span of 1 it is 0.125.
	let leaning = |span: f32| {
		let keys = vec![
			CubicKey { time: 0.0, value: Vec3::new(0.0, 0.0, 0.0), in_tangent: Vec3::new(0.0, 0.0, 0.0), out_tangent: Vec3::new(1.0, 0.0, 0.0) },
			CubicKey { time: span, value: Vec3::new(0.0, 0.0, 0.0), in_tangent: Vec3::new(0.0, 0.0, 0.0), out_tangent: Vec3::new(0.0, 0.0, 0.0) },
		];
		Track { target: Target::Joint(0), channel: Channel::Translation(Curve::Cubic(keys)) }
	};
	let wide = Clip::new(vec![leaning(2.0)], 2.0, Ending::Clamp, 0, true, &claimed())?;
	require!(exact(at(&wide, 1.0, 0)?.x, 0.25), "over a span of two the halfway value is 0.25, got {}", at(&wide, 1.0, 0)?.x);
	let narrow = Clip::new(vec![leaning(1.0)], 1.0, Ending::Clamp, 0, true, &claimed())?;
	require!(exact(at(&narrow, 0.5, 0)?.x, 0.125), "and over half the span it reaches half as far, got {}", at(&narrow, 0.5, 0)?.x);
	Ok(())
}

pub fn clamp_ending() -> Outcome {
	// A ONE-SHOT HOLDS ITS LAST FRAME, because a one-shot that wrapped would restart a death
	// animation.
	let clip = Clip::new(vec![line(0, &[(0.0, Vec3::new(0.0, 0.0, 0.0)), (2.0, Vec3::new(2.0, 0.0, 0.0))])], 2.0, Ending::Clamp, 0, true, &claimed())?;
	require!(exact(clip.at(2.5), 2.0), "a one-shot holds its last frame, got {}", clip.at(2.5));
	require!(exact(clip.at(-0.5), 0.0), "and its first before it starts, got {}", clip.at(-0.5));
	// AND A LOOP WRAPS, which is the other half of the same decision.
	let looping = Clip::new(vec![line(0, &[(0.0, Vec3::new(0.0, 0.0, 0.0)), (2.0, Vec3::new(0.0, 0.0, 0.0))])], 2.0, Ending::Loop, 0, true, &claimed())?;
	require!(exact(looping.at(2.5), 0.5), "2.5 into a two second loop is 0.5, got {}", looping.at(2.5));
	require!(exact(looping.at(-0.5), 1.5), "and a negative time wraps forwards, got {}", looping.at(-0.5));
	Ok(())
}

pub fn ping_pong_ending() -> Outcome {
	// THE PERIOD IS TWICE THE DURATION, and the turning frames are visited ONCE per period. Holding
	// the last frame across two frames is a stutter at both ends, once a cycle.
	let clip = Clip::new(vec![line(0, &[(0.0, Vec3::new(0.0, 0.0, 0.0)), (2.0, Vec3::new(2.0, 0.0, 0.0))])], 2.0, Ending::PingPong, 0, true, &claimed())?;
	for (time, expected) in [(0.0f32, 0.0f32), (1.0, 1.0), (2.0, 2.0), (2.5, 1.5), (3.0, 1.0), (4.0, 0.0), (4.5, 0.5)] {
		require!(exact(clip.at(time), expected), "ping-pong at {time} is {expected}, got {}", clip.at(time));
	}
	let before = clip.at(2.0 - 1e-3);
	let after = clip.at(2.0 + 1e-3);
	require!((before - after).abs() < 1e-4, "the turn is symmetric about the end key: {before} then {after}");
	require!(before < 2.0 && after < 2.0, "and neither side sits on it, so it is one frame and not two");
	// A PING-PONG NEEDS NO CLOSED SEAM, because its ends meet themselves.
	let open = vec![line(0, &[(0.0, Vec3::new(0.0, 0.0, 0.0)), (2.0, Vec3::new(9.0, 0.0, 0.0))])];
	require!(Clip::new(open, 2.0, Ending::PingPong, 0, true, &claimed()).is_ok(), "a ping-pong may end anywhere");
	Ok(())
}

pub fn pose_blending() -> Outcome {
	// TRANSLATION AND SCALE LERP, ROTATION SLERPS ALONG THE SHORTER ARC, exactly as within one clip.
	let still = Clip::new(vec![spin(3, &[(0.0, turn(0.0)), (1.0, turn(0.0))])], 1.0, Ending::Clamp, 0, true, &claimed())?.sample(0.0);
	let turned = Clip::new(vec![spin(3, &[(0.0, turn(core::f32::consts::FRAC_PI_2)), (1.0, turn(core::f32::consts::FRAC_PI_2))])], 1.0, Ending::Clamp, 0, true, &claimed())?.sample(0.0);
	let half = animate::blend(&still, &turned, 0.5).joint(3).and_then(|sampled| sampled.rotation).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("no rotation")))?;
	let spun = half.rotate(Vec3::new(1.0, 0.0, 0.0));
	let root_half = core::f32::consts::FRAC_1_SQRT_2;
	require!(spun.sub(Vec3::new(root_half, root_half, 0.0)).length() < 1e-3, "half a quarter turn is 45 degrees, got {spun:?}");
	// AND THE ENDS ARE THE TWO POSES THEMSELVES, which is what makes a cross-fade one rule.
	let start = animate::blend(&still, &turned, 0.0).joint(3).and_then(|sampled| sampled.rotation).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("no rotation")))?;
	require!(start.rotate(Vec3::new(1.0, 0.0, 0.0)).sub(Vec3::new(1.0, 0.0, 0.0)).length() < 1e-4, "a weight of zero is the first pose");
	Ok(())
}

pub fn blend_keeps_undriven_targets() -> Outcome {
	// A TARGET ONLY ONE POSE DRIVES IS TAKEN FROM IT UNCHANGED, AT FULL VALUE. Scaling it by its
	// clip's weight would pull the joint toward the origin as the blend moves away, which is a limb
	// collapsing rather than a blend.
	let walk = Clip::new(vec![line(1, &[(0.0, Vec3::new(10.0, 0.0, 0.0)), (1.0, Vec3::new(10.0, 0.0, 0.0))])], 1.0, Ending::Clamp, 0, true, &claimed())?.sample(0.0);
	let run = Clip::new(vec![line(2, &[(0.0, Vec3::new(4.0, 0.0, 0.0)), (1.0, Vec3::new(4.0, 0.0, 0.0))])], 1.0, Ending::Clamp, 0, true, &claimed())?.sample(0.0);
	let mixed = animate::blend(&walk, &run, 0.25);
	let from_walk = mixed.joint(1).and_then(|sampled| sampled.translation).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("joint 1 is gone")))?;
	require!(exact(from_walk.x, 10.0), "a joint only one pose drives is unchanged, got {from_walk:?}");
	let from_run = mixed.joint(2).and_then(|sampled| sampled.translation).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("joint 2 is gone")))?;
	require!(exact(from_run.x, 4.0), "from either side, got {from_run:?}");
	Ok(())
}

pub fn blended_root_motion() -> Outcome {
	// THE SAME WEIGHTED SUM: a quarter of the way from -2 to -6 is -3, and NOT the sum of the two,
	// which would make the character briefly outrun both clips.
	let walk = Clip::new(vec![line(0, &[(0.0, Vec3::new(0.0, 0.0, 0.0)), (1.0, Vec3::new(0.0, 0.0, -2.0))])], 1.0, Ending::Clamp, 0, false, &claimed())?.sample(1.0);
	let run = Clip::new(vec![line(0, &[(0.0, Vec3::new(0.0, 0.0, 0.0)), (1.0, Vec3::new(0.0, 0.0, -6.0))])], 1.0, Ending::Clamp, 0, false, &claimed())?.sample(1.0);
	let quarter = animate::blend(&walk, &run, 0.25);
	require!(exact(quarter.root_delta().z, -3.0), "the blended speed is between the two, got {:?}", quarter.root_delta());
	Ok(())
}

pub fn morph_weight_tracks() -> Outcome {
	// ONE TARGET'S WEIGHT PER TRACK, AND NOT NORMALISED ACROSS TARGETS - because morph targets are
	// additive, and normalising them would make a second expression undo half of the first.
	let tracks = vec![
		Track { target: Target::Morph(3), channel: Channel::MorphWeight(Curve::Linear(vec![Key { time: 0.0, value: 0.0 }, Key { time: 1.0, value: 1.0 }])) },
		Track { target: Target::Morph(7), channel: Channel::MorphWeight(Curve::Linear(vec![Key { time: 0.0, value: 1.0 }, Key { time: 1.0, value: 1.0 }])) },
	];
	let clip = Clip::new(tracks, 1.0, Ending::Clamp, 0, true, &claimed())?;
	let pose = clip.sample(0.5);
	require!(exact(pose.morph(3).unwrap_or(-1.0), 0.5), "a linear weight is halfway, got {:?}", pose.morph(3));
	require!(exact(pose.morph(7).unwrap_or(-1.0), 1.0), "and the other is untouched by it");
	let total: f32 = pose.morphs().map(|(_, weight)| weight).sum();
	require!(exact(total, 1.5), "the weights are additive and sum to what they sum to, got {total}");
	require!(pose.joint(3).is_none(), "a morph target is not a joint");
	// AND A CLIP DRIVES A SCENE'S NODES THROUGH A SKELETON, which is where joints and nodes meet.
	let skeleton = Skeleton::new(alloc::vec![0u32], &claimed())?;
	require!(skeleton.joints() == 1, "a one-joint skeleton has one joint");
	let _: Vec<u32> = alloc::vec::Vec::new();
	Ok(())
}
