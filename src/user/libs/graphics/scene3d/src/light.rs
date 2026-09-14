//! LIGHTING: which lights reach a drawable, in what order, and how they fall off.
//!
//! THE ORDER IS PART OF THE CONTRACT. Floating-point addition is not associative, so two
//! implementations that accumulate the same lights in different orders produce different colours -
//! not visibly different, but different enough that a conformance comparison against a reference
//! image fails for a reason nobody can find. So: the ambient term first, then directional lights,
//! then point and spot lights by DESCENDING irradiance at the drawable's bounding-sphere centre,
//! ties broken by the order they were added.
//!
//! THE FALL-OFF EQUATIONS ARE THE PROFILE'S AND ARE WRITTEN OUT HERE. `1 / (1 + d^2 / r^2)` is the
//! physical inverse-square softened near the source so a light at a surface is not infinite; the
//! multiplicative `saturate(1 - (d/range)^4)^2` is what takes it to EXACTLY zero at the range rather
//! than leaving the visible edge a plain clamp produces.

use alloc::vec::Vec;

use render_math::Vec3;

use crate::bounds::Sphere;
use crate::scene::{Error, Scene, VisibilityMask};

/// What kind of light, and the numbers that kind needs.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LightKind {
	/// A constant term. NOT A LIGHT WITH A POSITION, and it is in the enumeration rather than being
	/// a scene-wide field because a scene has more than one lighting environment over its life.
	Ambient,
	/// No position, one direction, no attenuation: infinitely far away. `direction` is the direction
	/// the light TRAVELS, so a sun overhead points down.
	Directional { direction: Vec3 },
	/// A position, a source radius and a range. THE TWO ARE DIFFERENT: the radius softens the
	/// inverse square near the source, and the range is where the light reaches exactly zero.
	Point { radius: f32, range: f32 },
	/// A point light restricted to a cone, with the cone angles in RADIANS - inner and outer, so the
	/// falloff between them belongs to the light and not to the application.
	Spot { direction: Vec3, radius: f32, range: f32, inner: f32, outer: f32 },
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Light {
	/// A LIGHT IS A NODE, so it moves with its parent.
	pub node: u32,
	pub kind: LightKind,
	/// In LINEAR space, like everything else the equations touch.
	pub colour: Vec3,
	pub intensity: f32,
	pub visibility: VisibilityMask,
}

impl Light {
	pub fn validate(&self) -> Result<(), Error> {
		if !self.colour.is_finite() || !self.intensity.is_finite() || self.intensity < 0.0 {
			return Err(Error::Degenerate { reason: "a light with a non-finite or negative intensity" });
		}
		match self.kind {
			LightKind::Ambient => {}
			LightKind::Directional { direction } => {
				if direction.normalise().is_err() {
					return Err(Error::Degenerate { reason: "a directional light with no direction" });
				}
			}
			LightKind::Point { radius, range } => bounds_of(radius, range)?,
			LightKind::Spot { direction, radius, range, inner, outer } => {
				bounds_of(radius, range)?;
				if direction.normalise().is_err() {
					return Err(Error::Degenerate { reason: "a spot light with no direction" });
				}
				if !inner.is_finite() || !outer.is_finite() || inner < 0.0 || outer < inner || outer > core::f32::consts::PI {
					return Err(Error::Degenerate { reason: "a spot cone whose inner angle is past its outer, or whose outer is past a half turn" });
				}
			}
		}
		Ok(())
	}

	/// How far the light reaches, or `None` for one that reaches everywhere.
	pub fn range(&self) -> Option<f32> {
		match self.kind {
			LightKind::Ambient | LightKind::Directional { .. } => None,
			LightKind::Point { range, .. } | LightKind::Spot { range, .. } => Some(range),
		}
	}
}

fn bounds_of(radius: f32, range: f32) -> Result<(), Error> {
	if !radius.is_finite() || !range.is_finite() || !(radius > 0.0) || !(range > 0.0) {
		return Err(Error::Degenerate { reason: "a light with a source radius or a range at or below zero, which divides by zero in the fall-off" });
	}
	Ok(())
}

/// The distance fall-off. ONE at the light's position, EXACTLY ZERO at and past its range.
pub fn attenuation(kind: &LightKind, distance: f32) -> f32 {
	match *kind {
		LightKind::Ambient => 1.0,
		// A DIRECTIONAL LIGHT DOES NOT ATTENUATE: it is infinitely far away, so every surface is at
		// the same distance from it.
		LightKind::Directional { .. } => 1.0,
		LightKind::Point { radius, range } | LightKind::Spot { radius, range, .. } => {
			let ratio = distance / radius;
			let inverse_square = 1.0 / (1.0 + ratio * ratio);
			let reach = saturate(distance / range);
			let window = saturate(1.0 - reach * reach * reach * reach);
			inverse_square * window * window
		}
	}
}

/// The cone fall-off of a spot light, given `to_light` pointing FROM the surface TO the light.
///
/// AN INNER ANGLE EQUAL TO THE OUTER IS A HARD EDGE and not a division by zero, which is the case a
/// plain `smoothstep` gets wrong.
pub fn cone(kind: &LightKind, to_light: Vec3) -> f32 {
	let LightKind::Spot { direction, inner, outer, .. } = *kind else { return 1.0 };
	let Ok(axis) = direction.normalise() else { return 0.0 };
	let Ok(to_light) = to_light.normalise() else { return 0.0 };
	// `-L` points from the light towards the surface, which is the direction the cone opens along.
	let along = to_light.negate().dot(axis);
	let (_, cos_inner) = render_math::quaternion::sin_cos(inner);
	let (_, cos_outer) = render_math::quaternion::sin_cos(outer);
	smoothstep(cos_outer, cos_inner, along)
}

/// The scalar contribution a light makes at a point, which is what the ordering is by.
///
/// A SCALAR IS NEEDED AND A COLOUR IS NOT ONE. The weighting is Rec.709 luminance in linear light,
/// the same one the image profile uses, so a dim red light does not outrank a bright white one.
pub fn irradiance(light: &Light, light_position: Vec3, at: Vec3) -> f32 {
	let luminance = 0.2126 * light.colour.x + 0.7152 * light.colour.y + 0.0722 * light.colour.z;
	let base = light.intensity * luminance;
	match light.kind {
		LightKind::Ambient | LightKind::Directional { .. } => base,
		LightKind::Point { .. } => base * attenuation(&light.kind, at.sub(light_position).length()),
		LightKind::Spot { .. } => {
			let to_light = light_position.sub(at);
			base * attenuation(&light.kind, to_light.length()) * cone(&light.kind, to_light)
		}
	}
}

/// Whether a light's volume reaches a sphere at all.
///
/// CONSERVATIVE: a spot light is tested against its RANGE and not its cone, because a cone test that
/// is slightly wrong drops a light from a surface it lit, and an unnecessary light costs arithmetic
/// while a missing one is a dark patch.
pub fn reaches(light: &Light, light_position: Vec3, sphere: &Sphere) -> bool {
	match light.range() {
		None => true,
		Some(range) => sphere.centre.sub(light_position).length() <= range + sphere.radius,
	}
}

/// The lights that affect a drawable, in the accumulation order, truncated at the per-drawable
/// limit.
///
/// TRUNCATED AT THE END AND NOT AT THE START: the order is by descending contribution, so what is
/// dropped is what matters least. A selection that truncated first and sorted afterwards would drop
/// the brightest light in a scene with nine.
pub fn select(scene: &Scene, sphere: &Sphere, mask: VisibilityMask) -> Vec<u32> {
	let transforms = scene.transforms();
	let limit = scene.limits().max_lights_per_drawable as usize;
	let mut ambient: Vec<u32> = Vec::new();
	let mut ranked: Vec<(u32, f32, u32)> = Vec::new();
	for (index, light) in scene.lights().iter().enumerate() {
		if !scene.effectively_enabled(light.node) {
			continue;
		}
		if scene.effective_visibility(light.node) & light.visibility & mask == 0 {
			continue;
		}
		let Some(world) = transforms.get(light.node as usize) else { continue };
		let position = world.translation();
		if !reaches(light, position, sphere) {
			continue;
		}
		match light.kind {
			LightKind::Ambient => ambient.push(index as u32),
			// The tier: directional lights rank ahead of every point and spot light, whatever the
			// numbers say, because the profile fixes the order and not the arithmetic.
			LightKind::Directional { .. } => ranked.push((0, irradiance(light, position, sphere.centre), index as u32)),
			_ => ranked.push((1, irradiance(light, position, sphere.centre), index as u32)),
		}
	}
	ranked.sort_by(|left, right| {
		left.0
			.cmp(&right.0)
			.then(right.1.partial_cmp(&left.1).unwrap_or(core::cmp::Ordering::Equal))
			// Ties by the order they were added, so a scene with two identical lights draws the same
			// way every frame.
			.then(left.2.cmp(&right.2))
	});
	let mut out = ambient;
	out.extend(ranked.into_iter().map(|(_, _, index)| index));
	out.truncate(limit);
	out
}

fn saturate(value: f32) -> f32 {
	if value.is_nan() {
		return 0.0;
	}
	value.clamp(0.0, 1.0)
}

/// `smoothstep`, with an edge pair that coincides giving a step rather than a division by zero.
fn smoothstep(from: f32, to: f32, at: f32) -> f32 {
	if to <= from {
		return if at >= to { 1.0 } else { 0.0 };
	}
	let t = saturate((at - from) / (to - from));
	t * t * (3.0 - 2.0 * t)
}
