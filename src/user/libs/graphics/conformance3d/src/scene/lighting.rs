//! THE LIGHTING GROUP: what a light is, which ones reach a surface, in what ORDER, and how they fall
//! off.
//!
//! THE ORDER IS PART OF THE CONTRACT AND IS THE REASON THIS GROUP EXISTS IN A CONFORMANCE SUITE.
//! Floating-point addition is not associative, so two implementations that accumulate the same lights
//! in different orders produce different colours - not visibly different, but different enough that a
//! comparison against a reference fails for a reason nobody can find. So one scene below is about
//! nothing but the order.
//!
//! AND THE FALL-OFF EQUATIONS ARE CHECKED AT THEIR BOUNDARIES, because that is where two
//! implementations differ: a light that reaches ALMOST zero at its range leaves a visible edge, and
//! that edge is what the window term exists to remove.

use crate::Outcome;
use crate::harness::close;
use crate::scene::world;
use render_math::Vec3;
use scene3d::{Light, LightKind, Node, Sphere, light};

/// A white light of unit intensity on a stated node.
fn lamp(node: u32, kind: LightKind) -> Light {
	Light { node, kind, colour: Vec3::new(1.0, 1.0, 1.0), intensity: 1.0, visibility: u32::MAX }
}

pub fn light_ambient() -> Outcome {
	// AMBIENT IS A CONSTANT TERM AND NOT A LIGHT WITH A POSITION, and it is in the ENUMERATION rather
	// than being a scene-wide field because a scene has more than one lighting environment over its
	// life: a room, a corridor and the outdoors are three, and a single field would make changing
	// between them a different mechanism from changing a light.
	let mut scene = world();
	let node = scene.add_node(Node::identity())?;
	let ambient = scene.add_light(lamp(node, LightKind::Ambient))?;
	require!(scene.lights()[ambient as usize].range().is_none(), "ambient light reaches everywhere");
	require!(close(light::attenuation(&LightKind::Ambient, 1000.0), 1.0), "and does not fall off with distance: {}", light::attenuation(&LightKind::Ambient, 1000.0));

	// AND IT COMES FIRST IN THE ACCUMULATION, ahead of every light with a direction - which is the
	// profile's own order and what `select` answers with.
	let far = scene.add_node(Node::identity().with_translation(Vec3::new(0.0, 10.0, 0.0)))?;
	let sun = scene.add_light(lamp(far, LightKind::Directional { direction: Vec3::new(0.0, -1.0, 0.0) }))?;
	scene.update();
	let chosen = light::select(&scene, &Sphere::new(Vec3::ZERO, 1.0), u32::MAX);
	require!(chosen == alloc::vec![ambient, sun], "the ambient term is accumulated first: {chosen:?}");
	Ok(())
}

pub fn light_directional() -> Outcome {
	// A DIRECTIONAL LIGHT IS INFINITELY FAR AWAY: one direction, no position, no attenuation. The
	// direction is the one the light TRAVELS, so a sun overhead points DOWN - which is the convention
	// a caller gets backwards, and backwards means the shadows fall the wrong way.
	let mut scene = world();
	let node = scene.add_node(Node::identity())?;
	let sun = lamp(node, LightKind::Directional { direction: Vec3::new(0.0, -1.0, 0.0) });
	scene.add_light(sun)?;
	require!(sun.range().is_none(), "a directional light has no range");
	require!(light::reaches(&sun, Vec3::ZERO, &Sphere::new(Vec3::new(1.0e6, 0.0, 0.0), 1.0)), "and reaches everything, however far away");

	// A DIRECTION OF ZERO IS REFUSED, because a light with no direction has no `L` and every surface
	// it touches would be shaded by a NaN.
	require!(scene.add_light(lamp(node, LightKind::Directional { direction: Vec3::ZERO })).is_err(), "a directional light with no direction is refused");
	Ok(())
}

pub fn light_point() -> Outcome {
	// A POINT LIGHT HAS A POSITION, A SOURCE RADIUS AND A RANGE, AND THE LAST TWO ARE DIFFERENT
	// THINGS: the radius softens the inverse square NEAR the source, and the range is where the light
	// reaches exactly zero. A layer with one number for both cannot have a bright small lamp that
	// reaches across a room.
	let mut scene = world();
	let node = scene.add_node(Node::identity().with_translation(Vec3::new(0.0, 3.0, 0.0)))?;
	let kind = LightKind::Point { radius: 1.0, range: 10.0 };
	scene.add_light(lamp(node, kind))?;
	require!(lamp(node, kind).range() == Some(10.0), "a point light states its range");
	require!(light::attenuation(&kind, 0.0) > 0.99, "at the light itself it delivers all of itself: {}", light::attenuation(&kind, 0.0));
	require!(light::attenuation(&kind, 2.0) < light::attenuation(&kind, 1.0), "and falls off with distance");

	// A LIGHT IS A NODE, SO IT MOVES WITH ITS PARENT - which is what makes a lamp on a vehicle one
	// object rather than two things somebody has to keep in step.
	let vehicle = scene.add_node(Node::identity())?;
	scene.set_parent(node, Some(vehicle))?;
	scene.set_translation(vehicle, Vec3::new(5.0, 0.0, 0.0))?;
	scene.update();
	require!(scene.transforms()[node as usize].translation() == Vec3::new(5.0, 3.0, 0.0), "and moving its parent moves the light: {:?}", scene.transforms()[node as usize].translation());

	// AND A RADIUS OR RANGE AT OR BELOW ZERO IS REFUSED, because the fall-off divides by both.
	require!(scene.add_light(lamp(node, LightKind::Point { radius: 0.0, range: 10.0 })).is_err(), "a point light with no source radius is refused");
	require!(scene.add_light(lamp(node, LightKind::Point { radius: 1.0, range: 0.0 })).is_err(), "and one with no range");
	Ok(())
}

pub fn light_spot() -> Outcome {
	// A SPOT LIGHT IS A POINT LIGHT RESTRICTED TO A CONE, with the angles in RADIANS and both of them
	// on the LIGHT rather than in the application - so the softness of the edge is part of the light
	// and two implementations soften it the same way.
	let mut scene = world();
	let node = scene.add_node(Node::identity().with_translation(Vec3::new(0.0, 5.0, 0.0)))?;
	let kind = LightKind::Spot { direction: Vec3::new(0.0, -1.0, 0.0), radius: 1.0, range: 20.0, inner: 0.2, outer: 0.5 };
	scene.add_light(lamp(node, kind))?;
	// Straight down the axis: `to_light` points from the surface up to the light.
	require!(close(light::cone(&kind, Vec3::new(0.0, 1.0, 0.0)), 1.0), "on the axis the cone delivers everything: {}", light::cone(&kind, Vec3::new(0.0, 1.0, 0.0)));
	// Well outside the outer angle.
	require!(close(light::cone(&kind, Vec3::new(1.0, 0.2, 0.0)), 0.0), "and outside the outer angle, nothing: {}", light::cone(&kind, Vec3::new(1.0, 0.2, 0.0)));
	// Between the two, something in between - which is what makes the edge soft.
	let between = light::cone(&kind, Vec3::new(0.35, 1.0, 0.0));
	require!(between > 0.0 && between < 1.0, "and between the two angles it is partial, at {between}");

	// AN INNER ANGLE EQUAL TO THE OUTER IS A HARD EDGE AND NOT A DIVISION BY ZERO, which is the case a
	// plain `smoothstep` gets wrong - and getting it wrong is a NaN across the whole cone.
	let hard = LightKind::Spot { direction: Vec3::new(0.0, -1.0, 0.0), radius: 1.0, range: 20.0, inner: 0.4, outer: 0.4 };
	require!(close(light::cone(&hard, Vec3::new(0.0, 1.0, 0.0)), 1.0), "a cone with one angle is lit on the axis: {}", light::cone(&hard, Vec3::new(0.0, 1.0, 0.0)));
	require!(close(light::cone(&hard, Vec3::new(1.0, 0.5, 0.0)), 0.0), "and dark outside it, with no NaN in between: {}", light::cone(&hard, Vec3::new(1.0, 0.5, 0.0)));

	// AND AN INNER ANGLE PAST THE OUTER IS REFUSED, because it is a cone turned inside out.
	require!(scene.add_light(lamp(node, LightKind::Spot { direction: Vec3::new(0.0, -1.0, 0.0), radius: 1.0, range: 20.0, inner: 1.0, outer: 0.5 })).is_err(), "an inner angle past the outer is refused");
	Ok(())
}

pub fn light_selection_order() -> Outcome {
	// THE ORDER IS AMBIENT, THEN DIRECTIONAL, THEN POINT AND SPOT BY DESCENDING IRRADIANCE AT THE
	// DRAWABLE'S BOUNDING-SPHERE CENTRE, ties broken by the order they were added. THE DIRECTIONAL
	// TIER IS ABSOLUTE: a directional light ranks ahead of every point light whatever the numbers say,
	// because the profile fixes the ORDER and not the arithmetic.
	let mut scene = world();
	let origin = scene.add_node(Node::identity())?;
	let near = scene.add_node(Node::identity().with_translation(Vec3::new(0.0, 2.0, 0.0)))?;
	let far = scene.add_node(Node::identity().with_translation(Vec3::new(0.0, 8.0, 0.0)))?;
	// Added in the WRONG order, so a layer that returned submission order fails.
	let dim_point = scene.add_light(Light { node: far, kind: LightKind::Point { radius: 1.0, range: 50.0 }, colour: Vec3::new(1.0, 1.0, 1.0), intensity: 1.0, visibility: u32::MAX })?;
	let bright_point = scene.add_light(Light { node: near, kind: LightKind::Point { radius: 1.0, range: 50.0 }, colour: Vec3::new(1.0, 1.0, 1.0), intensity: 4.0, visibility: u32::MAX })?;
	let sun = scene.add_light(lamp(origin, LightKind::Directional { direction: Vec3::new(0.0, -1.0, 0.0) }))?;
	let ambient = scene.add_light(lamp(origin, LightKind::Ambient))?;
	scene.update();
	let chosen = light::select(&scene, &Sphere::new(Vec3::ZERO, 1.0), u32::MAX);
	require!(chosen == alloc::vec![ambient, sun, bright_point, dim_point], "ambient, then directional, then point by descending contribution: {chosen:?}");

	// A DIM RED LIGHT DOES NOT OUTRANK A BRIGHT WHITE ONE, because the ranking is by LUMINANCE in
	// linear light and not by the largest channel.
	let mut scene = world();
	let close_node = scene.add_node(Node::identity().with_translation(Vec3::new(0.0, 2.0, 0.0)))?;
	let red = scene.add_light(Light { node: close_node, kind: LightKind::Point { radius: 1.0, range: 50.0 }, colour: Vec3::new(1.0, 0.0, 0.0), intensity: 1.0, visibility: u32::MAX })?;
	let white = scene.add_light(Light { node: close_node, kind: LightKind::Point { radius: 1.0, range: 50.0 }, colour: Vec3::new(1.0, 1.0, 1.0), intensity: 1.0, visibility: u32::MAX })?;
	scene.update();
	let chosen = light::select(&scene, &Sphere::new(Vec3::ZERO, 1.0), u32::MAX);
	require!(chosen == alloc::vec![white, red], "a white light outranks a red one of the same intensity: {chosen:?}");
	Ok(())
}

pub fn lights_per_drawable_limit() -> Outcome {
	// AT MOST `max_lights_per_drawable` AFFECT ONE DRAWABLE, AND THE TRUNCATION IS AT THE END. The
	// order is by descending contribution, so what is dropped is what matters LEAST; a selection that
	// truncated first and sorted afterwards would drop the brightest light in a scene with nine, which
	// is a room that goes dark when somebody adds a candle.
	let mut limits = scene3d::Limits::PROFILE_MINIMUM;
	limits.max_lights_per_drawable = 3;
	let mut scene = scene3d::Scene::new(limits);
	let mut added = alloc::vec::Vec::new();
	// Six point lights, the DIMMEST added first, so truncating before sorting keeps the wrong three.
	for step in 1..=6 {
		let node = scene.add_node(Node::identity().with_translation(Vec3::new(0.0, 2.0, 0.0)))?;
		added.push(scene.add_light(Light { node, kind: LightKind::Point { radius: 1.0, range: 50.0 }, colour: Vec3::new(1.0, 1.0, 1.0), intensity: step as f32, visibility: u32::MAX })?);
	}
	scene.update();
	let chosen = light::select(&scene, &Sphere::new(Vec3::ZERO, 1.0), u32::MAX);
	require!(chosen.len() == 3, "the selection is truncated at the limit, and kept {}", chosen.len());
	require!(chosen == alloc::vec![added[5], added[4], added[3]], "and what it kept is the BRIGHTEST three: {chosen:?}");

	// AND THE SCENE'S OWN LIGHT COUNT IS BOUNDED TOO, which is a different limit: how many a scene may
	// hold, against how many may reach one surface.
	require!(scene.limits().max_lights >= 256, "a conforming scene holds at least the profile's minimum number of lights");
	require!(scene.limits().max_lights_per_drawable == 3, "while this one admits three per drawable");
	Ok(())
}

pub fn point_attenuation() -> Outcome {
	// `1 / (1 + d^2 / r^2)` WITH A MULTIPLICATIVE WINDOW `saturate(1 - (d/range)^4)^2`. The window is
	// the half that is easy to leave out and impossible to un-see: without it the light reaches ALMOST
	// zero at its range and leaves a visible circular edge where it is cut off.
	let kind = LightKind::Point { radius: 2.0, range: 10.0 };
	require!(close(light::attenuation(&kind, 0.0), 1.0), "at the light it is one: {}", light::attenuation(&kind, 0.0));

	// AT THE SOURCE RADIUS the inverse square is exactly a half, times the window at that distance.
	let window = |d: f32| {
		let reach = (d / 10.0_f32).clamp(0.0, 1.0);
		let w = (1.0 - reach * reach * reach * reach).clamp(0.0, 1.0);
		w * w
	};
	require!(close(light::attenuation(&kind, 2.0), 0.5 * window(2.0)), "at the source radius the inverse square is a half: {}", light::attenuation(&kind, 2.0));

	// AND IT IS EXACTLY ZERO AT THE RANGE AND PAST IT, which a plain inverse square never is.
	require!(light::attenuation(&kind, 10.0) == 0.0, "at the range it is exactly zero, and is {}", light::attenuation(&kind, 10.0));
	require!(light::attenuation(&kind, 25.0) == 0.0, "and past it, still zero: {}", light::attenuation(&kind, 25.0));
	require!(light::attenuation(&kind, 9.9) > 0.0, "while just inside it there is still light: {}", light::attenuation(&kind, 9.9));

	// AND IT IS MONOTONIC, which is what makes it a fall-off rather than a curve with a bump in it.
	let mut previous = f32::INFINITY;
	let mut step = 0;
	while step <= 40 {
		let value = light::attenuation(&kind, step as f32 * 0.25);
		require!(value <= previous + 1.0e-6, "the fall-off never increases, and rose at {}", step as f32 * 0.25);
		previous = value;
		step += 1;
	}
	Ok(())
}

pub fn spot_cone_attenuation() -> Outcome {
	// THE SPOT FALL-OFF IS THE POINT FALL-OFF TIMES THE CONE, which is what makes a spot light a point
	// light with a shade on it rather than a separate equation: move a spot light twice as far away
	// and the distance term changes exactly as a point light's would.
	let spot = LightKind::Spot { direction: Vec3::new(0.0, -1.0, 0.0), radius: 2.0, range: 10.0, inner: 0.3, outer: 0.6 };
	let point = LightKind::Point { radius: 2.0, range: 10.0 };
	for distance in [0.0_f32, 1.0, 3.0, 7.5, 10.0] {
		require!(close(light::attenuation(&spot, distance), light::attenuation(&point, distance)), "the distance term of a spot is a point light's, and differs at {distance}");
	}

	// AND THE CONE IS `smoothstep(cos(outer), cos(inner), dot(-L, axis))`, which is SMOOTH: at the
	// midpoint between the two cosines it is exactly a half, because that is what smoothstep is.
	let (_, cos_inner) = render_math::quaternion::sin_cos(0.3);
	let (_, cos_outer) = render_math::quaternion::sin_cos(0.6);
	let middle = (cos_inner + cos_outer) * 0.5;
	// A direction whose `dot(-L, axis)` is that midpoint: the axis is `-y`, so `to_light` is `+y`
	// tilted until its cosine against the axis is the value wanted.
	let sine = (1.0 - middle * middle).max(0.0);
	let sine = crate::scene::lighting::root(sine);
	let to_light = Vec3::new(sine, middle, 0.0);
	require!(close(light::cone(&spot, to_light), 0.5), "halfway between the cosines the cone is half lit: {}", light::cone(&spot, to_light));

	// AND IT IS ONE INSIDE THE INNER ANGLE AND ZERO OUTSIDE THE OUTER, so the cone has an inside and
	// an outside rather than only an edge.
	require!(close(light::cone(&spot, Vec3::new(0.0, 1.0, 0.0)), 1.0), "inside the inner angle it is fully lit");
	require!(close(light::cone(&spot, Vec3::new(1.0, 0.1, 0.0)), 0.0), "and outside the outer angle it is dark");
	Ok(())
}

/// A square root without `std`, which the cone scene needs to build a direction of a stated cosine.
pub(crate) fn root(value: f32) -> f32 {
	if !(value > 0.0) {
		return 0.0;
	}
	let mut estimate = value;
	let mut step = 0;
	while step < 24 {
		let next = 0.5 * (estimate + value / estimate);
		if next == estimate {
			break;
		}
		estimate = next;
		step += 1;
	}
	estimate
}

pub fn directional_no_attenuation() -> Outcome {
	// A DIRECTIONAL LIGHT DOES NOT ATTENUATE, because it is infinitely far away: every surface is at
	// the same distance from it. That is what makes it the light a sun is, and a layer that applied a
	// fall-off to one would make the far side of a scene darker for no physical reason.
	let sun = LightKind::Directional { direction: Vec3::new(0.0, -1.0, 0.0) };
	for distance in [0.0_f32, 1.0, 100.0, 1.0e6] {
		require!(close(light::attenuation(&sun, distance), 1.0), "a directional light delivers all of itself at {distance}, and delivered {}", light::attenuation(&sun, distance));
	}

	// AND ITS IRRADIANCE IS THE SAME WHEREVER THE SURFACE IS, which is what the light selection ranks
	// by - so a directional light does not change its rank as the camera moves.
	let light_here = Light { node: 0, kind: sun, colour: Vec3::new(1.0, 1.0, 1.0), intensity: 2.0, visibility: u32::MAX };
	let near = light::irradiance(&light_here, Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0));
	let far = light::irradiance(&light_here, Vec3::ZERO, Vec3::new(1.0e5, 0.0, 0.0));
	require!(close(near, far), "and its contribution does not change with distance: {near} against {far}");
	require!(close(near, 2.0), "and is its intensity times its luminance, which for white at 2 is 2: {near}");
	Ok(())
}
