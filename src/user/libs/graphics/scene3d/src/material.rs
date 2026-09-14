//! THE FOUR CORE MATERIALS, AND THE EQUATIONS THEY COMPUTE.
//!
//! WRITTEN OUT AND EVALUABLE, not described. A material library whose equations live only in a
//! document is one where two implementations disagree and neither is wrong; one whose equations are
//! a function has an answer a fixture can hold against a value worked out by hand.
//!
//! EVERYTHING IS IN LINEAR LIGHT. A texture declared as sRGB is decoded before it is multiplied and
//! the result is encoded once at the end by the image profile's rule. Lighting in an encoded space
//! is the classic too-dark shadow, and it is wrong in a way that looks like an artistic choice.
//!
//! THE NORMAL IS RENORMALISED HERE. Interpolating unit vectors does not produce unit vectors, and
//! the error is largest in the middle of a large triangle - which is where a hand-checked fixture
//! would not look.
//!
//! THE SHININESS IS AN INTEGER IN `1..=MAX_SHININESS`. A fractional exponent needs `exp` and `ln`,
//! which this stack does not have outside a shader; the difference between 31 and 31.5 is not
//! visible, and exponentiation by squaring is exact and needs no transcendental at all. The upper
//! bound is there because the exponent is a loop count: an unbounded one is a frame that never ends.
//!
//! THE RESULT IS CLAMPED TO `[0, 1]` BEFORE THE TRANSFER FUNCTION. `Scene3D Core Profile 1` has no
//! high dynamic range - that is Extended - so a value above one has nowhere to go, and clamping in
//! linear light before the encode is the only place the clamp gives the colour a person expects.
//! AND LIGHTING NEVER CHANGES ALPHA: coverage is not brightness, and a lit surface that became more
//! opaque where the light fell would composite differently from the same surface in shadow.

use render_math::{Vec3, Vec4};

use render3d::blend::{AttachmentBlend, BlendEquation, ColorWriteMask};

use crate::queue::QueueKind;
use crate::scene::Error;

/// The largest specular exponent a core material may declare. The exponent is a LOOP COUNT in the
/// exact evaluation below, and an unbounded loop count is a frame that never ends.
pub const MAX_SHININESS: u32 = 1024;

/// Which of the four core equations a material computes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MaterialKind {
	/// `colour = base_colour * texture(uv)`. No light is applied and no normal is needed.
	Unlit,
	/// `colour = vertex_colour * texture(uv)`. Unlit, with the vertex colour interpolated smoothly.
	VertexColor,
	/// Diffuse only.
	Lambert,
	/// Diffuse and a specular highlight about the HALF VECTOR, which is what makes it Blinn-Phong
	/// rather than Phong and what keeps the highlight from vanishing at grazing angles.
	BlinnPhong,
}

/// How a material composites, which is what decides its queue.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Blending {
	Opaque,
	/// A fragment with alpha STRICTLY BELOW the threshold is discarded, so a threshold of zero
	/// discards nothing.
	AlphaMask {
		threshold: f32,
	},
	Blended,
}

/// A surface's appearance.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Material {
	pub kind: MaterialKind,
	pub blending: Blending,
	/// In LINEAR space.
	pub base_colour: Vec4,
	/// The caller's own identifier for a base-colour texture, or `None`.
	pub base_colour_texture: Option<u32>,
	pub specular: Vec3,
	/// The specular exponent, in `1..=MAX_SHININESS`. SPECULAR CARRIES ITS OWN COLOUR rather than
	/// multiplying the base, so a plastic with a coloured body keeps a white highlight.
	pub shininess: u32,
	/// Whether the normal is flipped for a back-facing fragment. Without it the lit side of a leaf
	/// is the side away from the light.
	pub two_sided: bool,
	/// The pipeline a backend draws this material with.
	pub pipeline: render3d::GraphicsPipeline,
	/// The uniform block this material's parameters live in.
	pub uniforms: u32,
	/// The PER-DRAW blend state and colour write mask. `render3d` carries blending in the pipeline
	/// rather than in a command, so this is the state the material's pipeline must have been built
	/// with - and `validate` refuses a material whose blend state disagrees with its blending, which
	/// is the mistake that draws a transparent surface opaque.
	pub blend: AttachmentBlend,
}

impl Material {
	pub fn new(kind: MaterialKind, pipeline: render3d::GraphicsPipeline, uniforms: u32) -> Self {
		Self { kind, blending: Blending::Opaque, base_colour: Vec4::new(1.0, 1.0, 1.0, 1.0), base_colour_texture: None, specular: Vec3::ZERO, shininess: 32, two_sided: false, pipeline, uniforms, blend: AttachmentBlend { enabled: false, colour: BlendEquation::REPLACE, alpha: BlendEquation::REPLACE, write_mask: ColorWriteMask::ALL } }
	}

	/// Set the blending, and with it the blend state that blending implies. A caller that wants a
	/// different equation says so with `with_blend_state` afterwards.
	pub fn with_blending(self, blending: Blending) -> Self {
		let blend = match blending {
			Blending::Blended => AttachmentBlend { enabled: true, colour: BlendEquation::PREMULTIPLIED_OVER, alpha: BlendEquation::PREMULTIPLIED_OVER, write_mask: ColorWriteMask::ALL },
			_ => AttachmentBlend { enabled: false, colour: BlendEquation::REPLACE, alpha: BlendEquation::REPLACE, write_mask: ColorWriteMask::ALL },
		};
		Self { blending, blend, ..self }
	}

	pub fn with_blend_state(self, blend: AttachmentBlend) -> Self {
		Self { blend, ..self }
	}

	pub fn with_base_colour(self, base_colour: Vec4) -> Self {
		Self { base_colour, ..self }
	}

	pub fn with_texture(self, texture: u32) -> Self {
		Self { base_colour_texture: Some(texture), ..self }
	}

	pub fn with_specular(self, specular: Vec3, shininess: u32) -> Self {
		Self { specular, shininess, ..self }
	}

	pub fn two_sided(self) -> Self {
		Self { two_sided: true, ..self }
	}

	/// THE QUEUE IS THE MATERIAL'S BLENDING AND NOT A FLAG ON THE NODE, so a node's queue changes
	/// when its material does and the two can never disagree.
	pub fn queue(&self) -> QueueKind {
		match self.blending {
			Blending::Opaque => QueueKind::Opaque,
			Blending::AlphaMask { .. } => QueueKind::AlphaMask,
			Blending::Blended => QueueKind::Transparent,
		}
	}

	/// Whether a drawable of this material writes the depth buffer. THE TWO OPAQUE QUEUES DO AND THE
	/// TRANSPARENT ONE DOES NOT: writing it would make a transparent surface hide the one behind it,
	/// which is the commonest transparency bug.
	pub fn writes_depth(&self) -> bool {
		!matches!(self.blending, Blending::Blended)
	}

	/// Whether a drawable of this material writes the picking attachment. A TRANSPARENT ONE DOES
	/// NOT, so a pick through glass answers what is behind it.
	pub fn writes_id(&self) -> bool {
		!matches!(self.blending, Blending::Blended)
	}

	pub fn validate(&self) -> Result<(), Error> {
		if !self.base_colour.is_finite() || !self.specular.is_finite() {
			return Err(Error::Degenerate { reason: "a material with a non-finite colour" });
		}
		if let Blending::AlphaMask { threshold } = self.blending {
			if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
				return Err(Error::Degenerate { reason: "an alpha threshold outside zero to one, which discards everything or nothing whatever the surface holds" });
			}
		}
		if self.shininess == 0 || self.shininess > MAX_SHININESS {
			return Err(Error::Degenerate { reason: "a specular exponent of zero or past the profile's ceiling; the exponent is a loop count and an unbounded one is a frame that never ends" });
		}
		// THE BLEND STATE AND THE BLENDING MUST AGREE. A material that says it is transparent and
		// carries a pipeline that does not blend draws opaque, which reads as a missing texture.
		if matches!(self.blending, Blending::Blended) != self.blend.enabled {
			return Err(Error::Degenerate { reason: "a material whose blend state disagrees with its blending: a transparent material must blend and an opaque one must not" });
		}
		Ok(())
	}
}

/// What a fragment knows about itself.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Surface {
	pub position: Vec3,
	/// The INTERPOLATED normal, not yet renormalised - `shade` does that, because a caller that did
	/// it would be doing it in the one place the error is not.
	pub normal: Vec3,
	pub vertex_colour: Vec4,
	/// The sampled base-colour texture, DECODED TO LINEAR. `(1,1,1,1)` when there is none.
	pub texture: Vec4,
	pub front_facing: bool,
}

impl Surface {
	pub fn new(position: Vec3, normal: Vec3) -> Self {
		Self { position, normal, vertex_colour: Vec4::new(1.0, 1.0, 1.0, 1.0), texture: Vec4::new(1.0, 1.0, 1.0, 1.0), front_facing: true }
	}

	pub fn with_vertex_colour(self, vertex_colour: Vec4) -> Self {
		Self { vertex_colour, ..self }
	}

	pub fn with_texture(self, texture: Vec4) -> Self {
		Self { texture, ..self }
	}

	pub fn back_facing(self) -> Self {
		Self { front_facing: false, ..self }
	}
}

/// One light as it arrives at a surface: the direction to it and what it delivers.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Incident {
	/// `L`, pointing FROM the surface TO the light. Unit length.
	pub to_light: Vec3,
	/// The light's colour times its intensity times its attenuation, in linear light.
	pub radiance: Vec3,
}

/// Evaluate a material.
///
/// `None` MEANS THE FRAGMENT WAS DISCARDED by the alpha threshold, which is a different answer from
/// a transparent colour: a discarded fragment writes no depth and no picking identity.
///
/// THE LIGHTS ARE ACCUMULATED IN THE ORDER GIVEN, which is `light::select`'s order and which the
/// caller must not shuffle: floating-point addition is not associative.
pub fn shade(material: &Material, surface: &Surface, eye: Vec3, ambient: Vec3, lights: &[Incident]) -> Option<Vec4> {
	let albedo = match material.kind {
		MaterialKind::VertexColor => surface.vertex_colour.mul(surface.texture),
		_ => material.base_colour.mul(surface.texture),
	};
	if let Blending::AlphaMask { threshold } = material.blending {
		// STRICTLY BELOW, so a threshold of zero discards nothing.
		if albedo.w < threshold {
			return None;
		}
	}
	match material.kind {
		MaterialKind::Unlit | MaterialKind::VertexColor => Some(albedo),
		MaterialKind::Lambert | MaterialKind::BlinnPhong => {
			let Ok(mut normal) = surface.normal.normalise() else { return Some(albedo) };
			if material.two_sided && !surface.front_facing {
				normal = normal.negate();
			}
			let base = Vec3::new(albedo.x, albedo.y, albedo.z);
			let mut diffuse = ambient;
			let mut specular = Vec3::ZERO;
			let view = eye.sub(surface.position).normalise().unwrap_or(Vec3::new(0.0, 0.0, 1.0));
			for light in lights {
				let lambert = normal.dot(light.to_light).max(0.0);
				diffuse = diffuse.add(light.radiance.scale(lambert));
				if material.kind == MaterialKind::BlinnPhong && lambert > 0.0 {
					// THE HALF VECTOR, not the reflection vector.
					let Ok(half) = light.to_light.add(view).normalise() else { continue };
					let highlight = power(normal.dot(half).max(0.0), material.shininess);
					specular = specular.add(light.radiance.scale(highlight));
				}
			}
			let lit = base.mul(diffuse).add(material.specular.mul(specular));
			// CLAMPED IN LINEAR LIGHT, BEFORE THE TRANSFER FUNCTION, and the alpha is the albedo's:
			// lighting never changes coverage.
			Some(Vec4::new(saturate(lit.x), saturate(lit.y), saturate(lit.z), albedo.w))
		}
	}
}

fn saturate(value: f32) -> f32 {
	if value.is_nan() {
		return 0.0;
	}
	value.clamp(0.0, 1.0)
}

/// Exponentiation by squaring. EXACT, DETERMINISTIC AND WITHOUT A TRANSCENDENTAL, which is what lets
/// the highlight be the same on every target this stack builds for.
fn power(base: f32, exponent: u32) -> f32 {
	let mut result = 1.0_f32;
	let mut base = base;
	let mut exponent = exponent;
	while exponent > 0 {
		if exponent & 1 == 1 {
			result *= base;
		}
		base *= base;
		exponent >>= 1;
	}
	result
}
