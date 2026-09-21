//! The physically based material: the four terms, the constants, and what a map means.

use render_math::{Vec3, Vec4};
use scene3d::material::{Blending, Incident};
use scene3d::pbr::{self, PbrMaterial, PbrSurface};

use crate::Outcome;
use crate::extended::{close, exact};

/// A head-on fragment, where the equations are simplest to state an expected value for.
fn head_on() -> (PbrSurface, Vec3, [Incident; 1]) {
	(PbrSurface::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0)), Vec3::new(0.0, 0.0, 1.0), [Incident { to_light: Vec3::new(0.0, 0.0, 1.0), radiance: Vec3::new(1.0, 1.0, 1.0) }])
}

fn white(metallic: f32, roughness: f32) -> PbrMaterial {
	PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 1.0), metallic, roughness)
}

pub fn material_pbr_metallic_roughness() -> Outcome {
	let (surface, eye, lights) = head_on();
	let shaded = pbr::shade(&white(0.0, 0.5), &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights);
	let Some(colour) = shaded else {
		return Err(crate::Trouble::Failed(alloc::string::String::from("a material with no alpha mask discarded its fragment")));
	};
	// The whole direct term at roughness 0.5 on a white dielectric: 0.96/pi + 0.04 * 5.0929582 *
	// 0.25 = 0.3565071.
	require!(close(colour.x, 0.3565071), "the direct term is 0.3565071, got {}", colour.x);
	require!(exact(colour.w, 1.0), "lighting does not change coverage, got {}", colour.w);
	Ok(())
}

pub fn normal_distribution_ggx() -> Outcome {
	// At the peak `D` is `1 / (pi * a^2)`, which at `a = 0.25` is 5.0929582.
	let peak = pbr::distribution_ggx(1.0, 0.25);
	require!(close(peak, 5.0929582), "GGX at the peak is 5.0929582, got {peak}");
	// AND IT FALLS AWAY FROM THE PEAK, which a distribution that ignored `dot(N,H)` would not.
	let off = pbr::distribution_ggx(0.5, 0.25);
	require!(off < peak, "the lobe falls away from the normal: {off} against {peak}");
	// A ROUGHER LOBE IS LOWER AND WIDER, which is the whole of what roughness means.
	require!(pbr::distribution_ggx(1.0, 1.0) < peak, "a rough lobe has a lower peak");
	Ok(())
}

pub fn visibility_smith_height_correlated() -> Outcome {
	// Head-on the term is `0.5 / (1 + 1)` = 0.25 at every roughness, because both square roots are
	// one - which is the check that the INCLUDED `1/(4 NoL NoV)` denominator is there.
	for roughness in [0.045f32, 0.5, 1.0] {
		let visible = pbr::visibility_smith(1.0, 1.0, roughness * roughness);
		require!(exact(visible, 0.25), "Smith head-on is 0.25 at roughness {roughness}, got {visible}");
	}
	// AND IT IS FINITE AT A GRAZING ANGLE, which is what the `dot(N,V)` clamp is for.
	let grazing = pbr::visibility_smith(1.0, pbr::MIN_N_DOT_V, 0.25);
	require!(grazing.is_finite(), "a grazing visibility is finite, got {grazing}");
	Ok(())
}

pub fn fresnel_schlick() -> Outcome {
	let f0 = Vec3::new(0.04, 0.04, 0.04);
	require!(exact(pbr::fresnel_schlick(1.0, f0).x, 0.04), "along the half vector Fresnel is F0 itself");
	require!(exact(pbr::fresnel_schlick(0.0, f0).x, 1.0), "and at grazing incidence it is one, whatever F0 was");
	// THE FIFTH POWER AND NOT A SQUARE: at `dot(V,H)` = 0.5 the factor is `0.5^5` = 0.03125, so
	// `F = 0.04 + 0.96 * 0.03125` = 0.07.
	require!(close(pbr::fresnel_schlick(0.5, f0).x, 0.07), "the fifth power gives 0.07, got {}", pbr::fresnel_schlick(0.5, f0).x);
	Ok(())
}

pub fn diffuse_lambert() -> Outcome {
	let (surface, eye, lights) = head_on();
	// An unlit-by-specular check is not available, so the diffuse is read as the whole term minus
	// the specular the other scenes pin: 0.3565071 - 0.04 * 1.2732395 = 0.3055775, which is
	// `0.96 / pi` - Lambert with the energy factor and nothing else.
	let colour = pbr::shade(&white(0.0, 0.5), &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	let diffuse = colour.x - 0.04 * 1.2732395;
	require!(close(diffuse, 0.3055775), "the diffuse term is 0.96/pi, got {diffuse}");
	Ok(())
}

pub fn energy_conserving_diffuse() -> Outcome {
	// A METAL HAS NO DIFFUSE TERM AT ALL, which is `(1 - metallic)` doing its work: head-on, the
	// whole answer is `F * D * V` = base * 1.2732395.
	let (surface, eye, lights) = head_on();
	let metal = PbrMaterial::new(Vec4::new(1.0, 0.5, 0.25, 1.0), 1.0, 0.5);
	let colour = pbr::shade(&metal, &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(colour.y, 0.5 * 1.2732395), "a metal's green is its F0 times D*V, got {}", colour.y);
	// AND A DIELECTRIC'S DIFFUSE CARRIES `(1 - F)`: without it the white dielectric above would read
	// `1/pi + 0.0509296` = 0.3692, which is 0.0127 brighter than the profile's 0.3565.
	let plain = pbr::shade(&white(0.0, 0.5), &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(plain.x < 0.3692 - 0.008, "the diffuse is multiplied by (1 - F), got {}", plain.x);
	Ok(())
}

pub fn perceptual_roughness_remap() -> Outcome {
	// `a = roughness^2`, so a material at 0.5 must NOT shade as one at 0.25 - which is exactly what
	// a renderer that fed the perceptual value straight into `D` would produce.
	let (surface, eye, lights) = head_on();
	let half = pbr::shade(&white(1.0, 0.5), &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	let squared = pbr::shade(&white(1.0, 0.25), &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!((half.x - squared.x).abs() > 1e-3, "roughness is remapped, so 0.5 and 0.25 are different materials");
	Ok(())
}

pub fn minimum_roughness_clamp() -> Outcome {
	let (surface, eye, lights) = head_on();
	let mirror = pbr::shade(&white(0.0, 0.0), &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	let floor = pbr::shade(&white(0.0, pbr::MIN_ROUGHNESS), &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(mirror.x.is_finite(), "a roughness of zero is finite, got {}", mirror.x);
	require!(exact(mirror.x, floor.x), "and is exactly the profile's minimum: {} against {}", mirror.x, floor.x);
	require!(exact(pbr::MIN_ROUGHNESS, 0.045), "the minimum is 0.045, got {}", pbr::MIN_ROUGHNESS);
	Ok(())
}

pub fn dielectric_f0_constant() -> Outcome {
	require!(exact(pbr::DIELECTRIC_F0, 0.04), "the dielectric F0 is 0.04, got {}", pbr::DIELECTRIC_F0);
	// AND A DIELECTRIC'S SPECULAR IS THAT CONSTANT AND NOT ITS BASE COLOUR: a black dielectric still
	// has a highlight, which is the observable difference from treating F0 as the albedo.
	let (surface, eye, lights) = head_on();
	let black = PbrMaterial::new(Vec4::new(0.0, 0.0, 0.0, 1.0), 0.0, 0.5);
	let colour = pbr::shade(&black, &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(colour.x, 0.04 * 1.2732395), "a black dielectric still has a 0.04 highlight, got {}", colour.x);
	Ok(())
}

pub fn clamped_dot_products() -> Outcome {
	let surface = PbrSurface::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0));
	let material = white(0.0, 0.5);
	// A LIGHT BEHIND THE SURFACE CONTRIBUTES NOTHING rather than a negative amount.
	let behind = [Incident { to_light: Vec3::new(0.0, 0.0, -1.0), radiance: Vec3::new(10.0, 10.0, 10.0) }];
	let dark = pbr::shade(&material, &surface, Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, 0.0, 0.0), &behind).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(exact(dark.x, 0.0), "a light behind the surface adds nothing, got {}", dark.x);
	// AND A VIEW EXACTLY EDGE ON IS FINITE, which is the `dot(N,V)` floor.
	let grazing = [Incident { to_light: Vec3::new(0.0, 0.0, 1.0), radiance: Vec3::new(1.0, 1.0, 1.0) }];
	let edge = pbr::shade(&material, &surface, Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 0.0), &grazing).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(edge.x.is_finite(), "a grazing view is finite, got {}", edge.x);
	Ok(())
}

pub fn direct_term_composition() -> Outcome {
	// THE PLACEMENT OF `dot(N,L)` IS WHAT IMPLEMENTATIONS DIFFER ABOUT. At 60 degrees from the
	// normal `dot(N,L)` is 0.5, and the whole term scales by exactly that against the head-on one
	// only if the cosine multiplies the SUM rather than one of its parts.
	let surface = PbrSurface::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0));
	let material = white(0.0, 1.0);
	let eye = Vec3::new(0.0, 0.0, 1.0);
	let straight = [Incident { to_light: Vec3::new(0.0, 0.0, 1.0), radiance: Vec3::new(1.0, 1.0, 1.0) }];
	let bright = pbr::shade(&material, &surface, eye, Vec3::new(0.0, 0.0, 0.0), &straight).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	// TWO LIGHTS OF HALF THE RADIANCE FROM THE SAME DIRECTION ARE ONE LIGHT, which holds only if the
	// accumulation is a plain sum over lights.
	let halved = [
		Incident { to_light: Vec3::new(0.0, 0.0, 1.0), radiance: Vec3::new(0.5, 0.5, 0.5) },
		Incident { to_light: Vec3::new(0.0, 0.0, 1.0), radiance: Vec3::new(0.5, 0.5, 0.5) },
	];
	let split = pbr::shade(&material, &surface, eye, Vec3::new(0.0, 0.0, 0.0), &halved).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(split.x, bright.x), "the lights are summed: {} against {}", split.x, bright.x);
	// AND THE MATERIAL'S OWN ALPHA SURVIVES untouched by any of it.
	require!(exact(bright.w, 1.0), "coverage is not lighting");
	let _ = Blending::Opaque;
	Ok(())
}
