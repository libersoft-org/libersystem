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

use crate::pbr::PbrMaterial;
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

impl Blending {
	/// THE QUEUE IS THE MATERIAL'S BLENDING AND NOT A FLAG ON THE NODE, so a node's queue changes
	/// when its material does and the two can never disagree.
	///
	/// ON `Blending` RATHER THAN ON EACH MATERIAL TYPE, because the core four and the Extended
	/// physically based material answer it identically. A second copy of this rule is how the two
	/// families come to disagree about what a transparent surface does, and a PBR surface that hid
	/// what was behind it while a `BlinnPhong` one did not would read as a defect in the shading.
	pub fn queue(self) -> QueueKind {
		match self {
			Self::Opaque => QueueKind::Opaque,
			Self::AlphaMask { .. } => QueueKind::AlphaMask,
			Self::Blended => QueueKind::Transparent,
		}
	}

	/// Whether a drawable of this blending writes the depth buffer. THE TWO OPAQUE QUEUES DO AND THE
	/// TRANSPARENT ONE DOES NOT: writing it would make a transparent surface hide the one behind it,
	/// which is the commonest transparency bug.
	pub fn writes_depth(self) -> bool {
		!matches!(self, Self::Blended)
	}

	/// Whether a drawable of this blending writes the picking attachment. A TRANSPARENT ONE DOES
	/// NOT, so a pick through glass answers what is behind it.
	pub fn writes_id(self) -> bool {
		!matches!(self, Self::Blended)
	}

	/// The blend state this blending implies, which is the state the material's pipeline must have
	/// been built with.
	pub fn blend_state(self) -> AttachmentBlend {
		match self {
			Self::Blended => AttachmentBlend { enabled: true, colour: BlendEquation::PREMULTIPLIED_OVER, alpha: BlendEquation::PREMULTIPLIED_OVER, write_mask: ColorWriteMask::ALL },
			_ => AttachmentBlend { enabled: false, colour: BlendEquation::REPLACE, alpha: BlendEquation::REPLACE, write_mask: ColorWriteMask::ALL },
		}
	}

	/// The one thing a blending can be wrong about on its own.
	pub fn validate(self) -> Result<(), Error> {
		if let Self::AlphaMask { threshold } = self {
			if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
				return Err(Error::Degenerate { reason: "an alpha threshold outside zero to one, which discards everything or nothing whatever the surface holds" });
			}
		}
		Ok(())
	}
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
		Self { blending, blend: blending.blend_state(), ..self }
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
		self.blending.queue()
	}

	/// Whether a drawable of this material writes the depth buffer. THE TWO OPAQUE QUEUES DO AND THE
	/// TRANSPARENT ONE DOES NOT: writing it would make a transparent surface hide the one behind it,
	/// which is the commonest transparency bug.
	pub fn writes_depth(&self) -> bool {
		self.blending.writes_depth()
	}

	/// Whether a drawable of this material writes the picking attachment. A TRANSPARENT ONE DOES
	/// NOT, so a pick through glass answers what is behind it.
	pub fn writes_id(&self) -> bool {
		self.blending.writes_id()
	}

	pub fn validate(&self) -> Result<(), Error> {
		if !self.base_colour.is_finite() || !self.specular.is_finite() {
			return Err(Error::Degenerate { reason: "a material with a non-finite colour" });
		}
		self.blending.validate()?;
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

/// Which of the five maps `PbrMetallicRoughness` names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PbrMap {
	BaseColour,
	MetallicRoughness,
	Normal,
	Occlusion,
	Emissive,
}

impl PbrMap {
	/// What this map's numbers MEAN, which is what decides whether a transfer function is applied
	/// to it.
	///
	/// A FUNCTION AND NOT A COMMENT, because this is the rule that costs nothing to get wrong and
	/// changes every pixel: decoding a normal map bends every normal toward the surface and
	/// decoding a roughness map makes a whole material glossier, and neither reports itself.
	pub const fn semantics(self) -> graphics_profile::image::Semantics {
		match self {
			Self::BaseColour | Self::Emissive => graphics_profile::image::Semantics::Color,
			Self::MetallicRoughness | Self::Normal | Self::Occlusion => graphics_profile::image::Semantics::Data,
		}
	}

	/// The five, in the order the profile's rules name them.
	pub const ALL: [Self; 5] = [Self::BaseColour, Self::MetallicRoughness, Self::Normal, Self::Occlusion, Self::Emissive];
}

/// The maps a physically based material samples, by the caller's own identifiers.
///
/// `None` IS A MAP THE MATERIAL DOES NOT HAVE, and its absence is the identity - which is what makes
/// a material with no maps the same path as one with five rather than a second one.
///
/// THE IDENTIFIERS ARE HERE AND THE SAMPLED VALUES ARE IN `PbrSurface`, which is the same split the
/// core material has: a material names a texture, a fragment carries what was read out of it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PbrMaps {
	pub base_colour: Option<u32>,
	pub metallic_roughness: Option<u32>,
	pub normal: Option<u32>,
	pub occlusion: Option<u32>,
	pub emissive: Option<u32>,
}

impl PbrMaps {
	pub const fn of(&self, map: PbrMap) -> Option<u32> {
		match map {
			PbrMap::BaseColour => self.base_colour,
			PbrMap::MetallicRoughness => self.metallic_roughness,
			PbrMap::Normal => self.normal,
			PbrMap::Occlusion => self.occlusion,
			PbrMap::Emissive => self.emissive,
		}
	}

	pub fn with(self, map: PbrMap, texture: u32) -> Self {
		let texture = Some(texture);
		match map {
			PbrMap::BaseColour => Self { base_colour: texture, ..self },
			PbrMap::MetallicRoughness => Self { metallic_roughness: texture, ..self },
			PbrMap::Normal => Self { normal: texture, ..self },
			PbrMap::Occlusion => Self { occlusion: texture, ..self },
			PbrMap::Emissive => Self { emissive: texture, ..self },
		}
	}

	/// How many of the five this material samples.
	pub fn count(&self) -> usize {
		PbrMap::ALL.iter().filter(|map| self.of(**map).is_some()).count()
	}
}

/// A PHYSICALLY BASED MATERIAL AND THE BINDING A BACKEND DRAWS IT WITH, which is what makes it a
/// scene's material rather than a set of factors.
///
/// A SECOND TABLE AND NOT A FIFTH `MaterialKind`. `Scene3D Core Profile 1`'s material list is
/// exactly four entries; a physically based material held in the core table would either claim one
/// of those four kinds, which is a lie in the inventory, or add a fifth, which changes what the core
/// profile means. So the Extended materials live in a table of their own, and only a scene whose
/// limits claim Extended has one at all.
///
/// `PbrMaterial` STAYS THE PURE FACTORS. The equations and every input they take are `pbr`'s, with
/// fixtures that compute their expected values by hand; this type adds the three things the RECORDER
/// needs - a pipeline, a uniform block and a blend state - and nothing the arithmetic can see.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ExtendedMaterial {
	/// The factors `pbr::shade` evaluates.
	pub shading: PbrMaterial,
	/// The five maps the glTF data model names, by the caller's own identifiers.
	pub maps: PbrMaps,
	/// The pipeline a backend draws this material with.
	pub pipeline: render3d::GraphicsPipeline,
	/// The uniform block this material's parameters live in.
	pub uniforms: u32,
	/// The PER-DRAW blend state, held against the shading's blending by `validate` for the same
	/// reason the core material's is: a material that says it is transparent and carries a pipeline
	/// that does not blend draws opaque, which reads as a missing texture.
	pub blend: AttachmentBlend,
}

impl ExtendedMaterial {
	pub fn new(shading: PbrMaterial, pipeline: render3d::GraphicsPipeline, uniforms: u32) -> Self {
		Self { shading, maps: PbrMaps::default(), pipeline, uniforms, blend: shading.blending.blend_state() }
	}

	pub fn with_blend_state(self, blend: AttachmentBlend) -> Self {
		Self { blend, ..self }
	}

	/// Name one of the five maps.
	pub fn with_map(self, map: PbrMap, texture: u32) -> Self {
		Self { maps: self.maps.with(map, texture), ..self }
	}

	pub fn queue(&self) -> QueueKind {
		self.shading.blending.queue()
	}

	pub fn writes_depth(&self) -> bool {
		self.shading.blending.writes_depth()
	}

	pub fn writes_id(&self) -> bool {
		self.shading.blending.writes_id()
	}

	pub fn validate(&self) -> Result<(), Error> {
		if !self.shading.base_colour.is_finite() || !self.shading.emissive.is_finite() {
			return Err(Error::Degenerate { reason: "a material with a non-finite colour" });
		}
		// BOTH FACTORS ARE MIXES, AND A MIX OUTSIDE ZERO TO ONE IS NOT ONE. A metallic above one
		// drives the diffuse term negative, and a roughness above one takes `a = roughness^2` past
		// the range every fit in these equations was made over - neither of which the shading
		// reports, because both produce a colour.
		if !(0.0..=1.0).contains(&self.shading.metallic) || !(0.0..=1.0).contains(&self.shading.roughness) {
			return Err(Error::Degenerate { reason: "a metallic or roughness outside zero to one, which is a mix factor that does not mix" });
		}
		if !(0.0..=1.0).contains(&self.shading.occlusion_strength) || !self.shading.normal_scale.is_finite() {
			return Err(Error::Degenerate { reason: "an occlusion strength outside zero to one or a non-finite normal scale" });
		}
		self.shading.blending.validate()?;
		if matches!(self.shading.blending, Blending::Blended) != self.blend.enabled {
			return Err(Error::Degenerate { reason: "a material whose blend state disagrees with its blending: a transparent material must blend and an opaque one must not" });
		}
		Ok(())
	}
}

/// A drawable's material, resolved out of whichever table holds it.
///
/// THE RECORDER ASKS THIS AND NOT A TABLE. `emit`, `queue` and `pick` each need the same answers
/// from a material - its pipeline, its uniforms, its queue, whether it writes depth and whether it
/// writes an identity - and a branch per material family in each of the three is three places a
/// family added later is forgotten in one of them.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Shading<'a> {
	Core(&'a Material),
	Extended(&'a ExtendedMaterial),
}

impl Shading<'_> {
	pub fn blending(&self) -> Blending {
		match self {
			Self::Core(material) => material.blending,
			Self::Extended(material) => material.shading.blending,
		}
	}

	pub fn queue(&self) -> QueueKind {
		self.blending().queue()
	}

	pub fn writes_depth(&self) -> bool {
		self.blending().writes_depth()
	}

	pub fn writes_id(&self) -> bool {
		self.blending().writes_id()
	}

	pub fn pipeline(&self) -> render3d::GraphicsPipeline {
		match self {
			Self::Core(material) => material.pipeline,
			Self::Extended(material) => material.pipeline,
		}
	}

	pub fn uniforms(&self) -> u32 {
		match self {
			Self::Core(material) => material.uniforms,
			Self::Extended(material) => material.uniforms,
		}
	}

	/// Whether this is a material from the Extended part, for a caller that has to know - the
	/// backend that picks a shader family, and nothing in this layer.
	pub fn is_extended(&self) -> bool {
		matches!(self, Self::Extended(_))
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
