//! THE PHYSICALLY BASED MATERIAL: `Scene3D Extended Profile 1`'s `material` group.
//!
//! "PBR METALLIC-ROUGHNESS" IS A FAMILY OF IMPLEMENTATIONS THAT DISAGREE. A physically based
//! material is four or five functions multiplied together, and every published renderer chooses
//! slightly different ones - a different normal distribution, a different visibility term, a
//! different roughness remapping. The difference is not a quality setting; it is a different
//! material, and two renderers that chose differently produce visibly different metal from one
//! asset. The profile picks, and this module computes what it picked and nothing else.
//!
//! THE DATA MODEL IS glTF's, WHICH IS THE REALITY CHECK. Base colour, metallic-roughness, normal,
//! occlusion and emissive; `Opaque`, `Mask` and `Blend`; a double-sided flag. A material model that
//! cannot consume the assets the world already has is a material model for one demo.
//!
//! THE TEXELS ARRIVE ALREADY SAMPLED, AND THEIR COLOUR SPACE IS THE CALLER'S TO HAVE GOT RIGHT.
//! Base colour and emissive are `Color` and reach here decoded to linear; metallic-roughness, normal
//! and occlusion are `Data` and reach here raw. The profile states it, `PbrSurface`'s fields name it
//! again, and it is the one rule that costs nothing to get wrong and changes every pixel.
//!
//! AND THE RESULT IS NOT CLAMPED. The post-processing rules put tone mapping LAST, after bloom and
//! fog, precisely because both are defined on linear radiance; a material that clamped its own
//! output to 1 would have thrown away everything bloom exists to spread before bloom ever saw it.

use render_math::{Vec3, Vec4, sqrt};

use crate::material::Blending;

/// The smallest roughness the equations admit, clamped BEFORE `a = roughness^2` is taken.
///
/// A ROUGHNESS OF ZERO MAKES `D` A DELTA FUNCTION: an infinite highlight at one pixel, and a NaN in
/// a filtered environment lookup. The clamp is on the perceptual value and not on `a`, because that
/// is where the profile puts it and the two are not the same bound.
pub const MIN_ROUGHNESS: f32 = 0.045;

/// The reflectance of an ordinary dielectric at normal incidence.
pub const DIELECTRIC_F0: f32 = 0.04;

/// `dot(N,V)` never goes below this, so the visibility term cannot divide by zero at a grazing
/// angle.
pub const MIN_N_DOT_V: f32 = 1e-4;

/// A physically based material's own factors, before any map is applied.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PbrMaterial {
	/// LINEAR. At `metallic` 0 this is the diffuse albedo; at 1 it IS the metal's F0.
	pub base_colour: Vec4,
	pub metallic: f32,
	/// PERCEPTUAL, and remapped by `a = roughness^2` inside the equations.
	pub roughness: f32,
	/// LINEAR RADIANCE, added after everything else.
	pub emissive: Vec3,
	/// How much of the occlusion map is applied: `mix(1, sampled, strength)`.
	pub occlusion_strength: f32,
	/// How much of the normal map is applied, scaling its tangent-space x and y.
	pub normal_scale: f32,
	pub blending: Blending,
	pub two_sided: bool,
}

impl PbrMaterial {
	/// A plain dielectric, which is what a material with no maps at all looks like.
	pub fn new(base_colour: Vec4, metallic: f32, roughness: f32) -> Self {
		Self { base_colour, metallic, roughness, emissive: Vec3::new(0.0, 0.0, 0.0), occlusion_strength: 1.0, normal_scale: 1.0, blending: Blending::Opaque, two_sided: false }
	}

	pub fn with_emissive(self, emissive: Vec3) -> Self {
		Self { emissive, ..self }
	}

	pub fn with_blending(self, blending: Blending) -> Self {
		Self { blending, ..self }
	}

	pub fn two_sided(self) -> Self {
		Self { two_sided: true, ..self }
	}
}

/// One fragment's sampled inputs.
///
/// EVERY MAP IS OPTIONAL AND ITS ABSENCE IS THE IDENTITY, which is what makes a material with no
/// maps the same code path as one with five.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PbrSurface {
	pub position: Vec3,
	/// The interpolated geometric normal, before the normal map.
	pub normal: Vec3,
	/// The tangent and its handedness, as glTF stores it: `w` is +1 or -1. `None` means the mesh has
	/// no tangents, and then the normal map is NOT applied.
	pub tangent: Option<(Vec3, f32)>,
	pub front_facing: bool,
	/// `Color`, DECODED TO LINEAR by the sampler.
	pub base_colour_texel: Vec4,
	/// `Data`, NEVER decoded. Green is roughness and blue is metallic, which is glTF's packing.
	pub metallic_roughness_texel: Vec3,
	/// `Data`, NEVER decoded, already mapped from [0,1] to [-1,1] by the sampler.
	pub normal_texel: Option<Vec3>,
	/// `Data`, NEVER decoded.
	pub occlusion_texel: f32,
	/// `Color`, DECODED TO LINEAR.
	pub emissive_texel: Vec3,
}

impl PbrSurface {
	/// A fragment with no maps: every texel the identity for its channel.
	pub fn new(position: Vec3, normal: Vec3) -> Self {
		Self { position, normal, tangent: None, front_facing: true, base_colour_texel: Vec4::new(1.0, 1.0, 1.0, 1.0), metallic_roughness_texel: Vec3::new(1.0, 1.0, 1.0), normal_texel: None, occlusion_texel: 1.0, emissive_texel: Vec3::new(1.0, 1.0, 1.0) }
	}

	pub fn back_facing(self) -> Self {
		Self { front_facing: false, ..self }
	}

	pub fn with_tangent(self, tangent: Vec3, handedness: f32) -> Self {
		Self { tangent: Some((tangent, handedness)), ..self }
	}

	pub fn with_normal_map(self, texel: Vec3) -> Self {
		Self { normal_texel: Some(texel), ..self }
	}

	pub fn with_metallic_roughness(self, texel: Vec3) -> Self {
		Self { metallic_roughness_texel: texel, ..self }
	}

	pub fn with_occlusion(self, texel: f32) -> Self {
		Self { occlusion_texel: texel, ..self }
	}
}

/// The GGX / Trowbridge-Reitz normal distribution.
///
/// `D(h) = a^2 / (pi * (dot(N,H)^2 * (a^2 - 1) + 1)^2)`, with `a` ALREADY REMAPPED. GGX rather than
/// Beckmann because its tail is longer, which is what makes a rough metal look rough rather than
/// plastic.
pub fn distribution_ggx(n_dot_h: f32, a: f32) -> f32 {
	let a2 = a * a;
	let denominator = n_dot_h * n_dot_h * (a2 - 1.0) + 1.0;
	a2 / (core::f32::consts::PI * denominator * denominator)
}

/// The Smith height-correlated visibility term.
///
/// IT INCLUDES THE `1 / (4 dot(N,L) dot(N,V))` DENOMINATOR of the specular BRDF, which is why the
/// whole specular term is `D * V * F` and not `D * G * F / (4 ...)`. Stating which convention the
/// term carries is the difference between a correct highlight and one four times too bright.
pub fn visibility_smith(n_dot_l: f32, n_dot_v: f32, a: f32) -> f32 {
	let a2 = a * a;
	let from_view = n_dot_l * sqrt(n_dot_v * n_dot_v * (1.0 - a2) + a2);
	let from_light = n_dot_v * sqrt(n_dot_l * n_dot_l * (1.0 - a2) + a2);
	let total = from_view + from_light;
	if total <= 0.0 { 0.0 } else { 0.5 / total }
}

/// Schlick's Fresnel: `F0 + (1 - F0) * (1 - dot(V,H))^5`.
pub fn fresnel_schlick(v_dot_h: f32, f0: Vec3) -> Vec3 {
	let one_minus = (1.0 - v_dot_h).clamp(0.0, 1.0);
	// FIFTH POWER BY MULTIPLICATION, not by a transcendental: exact, deterministic, and the same on
	// every target this stack builds for.
	let squared = one_minus * one_minus;
	let fifth = squared * squared * one_minus;
	f0.add(Vec3::new(1.0 - f0.x, 1.0 - f0.y, 1.0 - f0.z).scale(fifth))
}

/// One light as it arrives, in the core layer's own terms.
pub use crate::material::Incident;

/// Evaluate the material at one fragment.
///
/// `None` MEANS THE FRAGMENT WAS DISCARDED by the alpha threshold, which is a different answer from
/// a transparent colour: a discarded fragment writes no depth and no picking identity.
///
/// THE LIGHTS ARE ACCUMULATED IN THE ORDER GIVEN, which the caller must not shuffle, because
/// floating-point addition is not associative and a conformance comparison is over the sum.
pub fn shade(material: &PbrMaterial, surface: &PbrSurface, eye: Vec3, ambient: Vec3, lights: &[Incident]) -> Option<Vec4> {
	let albedo = material.base_colour.mul(surface.base_colour_texel);
	if let Blending::AlphaMask { threshold } = material.blending {
		// STRICTLY BELOW, so a threshold of zero discards nothing. The core profile's rule, under
		// glTF's name for it.
		if albedo.w < threshold {
			return None;
		}
	}
	let base = Vec3::new(albedo.x, albedo.y, albedo.z);

	// THE NORMAL, FLIPPED FIRST. A double-sided surface flips its interpolated normal BEFORE
	// anything else reads it, so the normal map, the lighting and the environment lookup all see the
	// flipped one; flipping after shading would light a leaf's underside as though it were its top.
	let Ok(mut normal) = surface.normal.normalise() else { return Some(albedo) };
	if material.two_sided && !surface.front_facing {
		normal = normal.negate();
	}
	normal = apply_normal_map(normal, surface, material.normal_scale);

	// glTF's PACKING: roughness in green, metallic in blue, each multiplied by the material's own
	// factor. Clamped on the PERCEPTUAL value, which is where the profile puts the bound.
	let roughness = (material.roughness * surface.metallic_roughness_texel.y).clamp(MIN_ROUGHNESS, 1.0);
	let metallic = (material.metallic * surface.metallic_roughness_texel.z).clamp(0.0, 1.0);
	let a = roughness * roughness;
	let f0 = Vec3::new(DIELECTRIC_F0, DIELECTRIC_F0, DIELECTRIC_F0).lerp(base, metallic);

	let view = eye.sub(surface.position).normalise().unwrap_or(Vec3::new(0.0, 0.0, 1.0));
	// CLAMPED TO AT LEAST `MIN_N_DOT_V`, so the visibility term cannot divide by zero at a grazing
	// angle - which is a whole silhouette of NaN pixels on a sphere.
	let n_dot_v = normal.dot(view).clamp(MIN_N_DOT_V, 1.0);

	let mut direct = Vec3::new(0.0, 0.0, 0.0);
	for light in lights {
		let n_dot_l = normal.dot(light.to_light).clamp(0.0, 1.0);
		if n_dot_l <= 0.0 {
			continue;
		}
		let Ok(half) = light.to_light.add(view).normalise() else { continue };
		let n_dot_h = normal.dot(half).clamp(0.0, 1.0);
		let v_dot_h = view.dot(half).clamp(0.0, 1.0);
		let d = distribution_ggx(n_dot_h, a);
		let v = visibility_smith(n_dot_l, n_dot_v, a);
		let f = fresnel_schlick(v_dot_h, f0);
		// THE DIFFUSE IS MULTIPLIED BY `(1 - F)` SO ENERGY IS NOT CREATED, and by `(1 - metallic)`
		// because a metal has no diffuse term at all.
		let diffuse = base.scale((1.0 - metallic) / core::f32::consts::PI).mul(Vec3::new(1.0 - f.x, 1.0 - f.y, 1.0 - f.z));
		let specular = f.scale(d * v);
		// THE ORDER AND THE PLACEMENT OF `dot(N,L)` ARE WHAT IMPLEMENTATIONS DIFFER ABOUT, so the
		// whole term is written the way the profile writes it. `radiance` already carries the
		// light's colour, its intensity and its attenuation.
		direct = direct.add(diffuse.add(specular).mul(light.radiance).scale(n_dot_l));
	}

	// OCCLUSION REACHES THE AMBIENT TERM AND NOTHING ELSE. It is a statement about how much of the
	// sky a point can see; applying it to a light the scene placed would darken a surface that light
	// demonstrably reaches.
	let occlusion = 1.0 + material.occlusion_strength * (surface.occlusion_texel - 1.0);
	let ambient_term = base.mul(ambient).scale(occlusion);

	// EMISSIVE IS A SOURCE AND NOT A RECEIVER: added last, unattenuated, and untouched by occlusion.
	let emissive = material.emissive.mul(surface.emissive_texel);
	let lit = direct.add(ambient_term).add(emissive);
	// NOT CLAMPED TO 1 - see the module note. A NaN is still refused, because one NaN pixel spreads
	// through a bloom pyramid into the whole frame.
	Some(Vec4::new(finite(lit.x), finite(lit.y), finite(lit.z), albedo.w))
}

/// The normal after its map, in the tangent frame the mesh supplied.
///
/// A MISSING TANGENT MEANS NO NORMAL MAP, and that is a decision rather than an omission: deriving
/// one from screen-space derivatives makes the frame depend on the rasteriser's derivative rule, so
/// a mesh whose author simply did not export tangents would look different on each backend.
fn apply_normal_map(normal: Vec3, surface: &PbrSurface, scale: f32) -> Vec3 {
	let (Some(texel), Some((tangent, handedness))) = (surface.normal_texel, surface.tangent) else {
		return normal;
	};
	let Ok(tangent) = tangent.sub(normal.scale(normal.dot(tangent))).normalise() else {
		return normal;
	};
	// +Y UP, THE OpenGL CONVENTION, and the bitangent carries glTF's handedness. The other
	// convention inverts every crevice into a bump: the surface still looks lit, and it looks lit
	// from the wrong side.
	let bitangent = normal.cross(tangent).scale(handedness);
	let mapped = tangent.scale(texel.x * scale).add(bitangent.scale(texel.y * scale)).add(normal.scale(texel.z));
	mapped.normalise().unwrap_or(normal)
}

fn finite(value: f32) -> f32 {
	if value.is_finite() { value.max(0.0) } else { 0.0 }
}
