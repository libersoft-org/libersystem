//! THE MATERIAL GROUP: the four core equations, and the four rules that make two implementations of
//! them agree.
//!
//! THE EQUATIONS ARE EVALUABLE AND ARE EVALUATED HERE. A material library whose equations live only
//! in a document is one where two implementations disagree and neither is wrong; these scenes hold
//! the layer's own `shade` against values worked out by hand, which is what makes "Blinn-Phong" a
//! claim rather than a name.
//!
//! AND EVERY VALUE IS IN LINEAR LIGHT. Lighting in an encoded space is the classic too-dark shadow,
//! and it is wrong in a way that looks like an artistic choice - which is why one scene below checks
//! the property no encoded space has.

use crate::Outcome;
use crate::harness::close;
use crate::scene::{material, world};
use graphics_core::color::decode;
use graphics_profile::image::Transfer;
use render_math::{Vec3, Vec4};
use render3d::blend::{AttachmentBlend, BlendEquation, ColorWriteMask};
use scene3d::{Blending, Incident, MaterialKind, QueueKind, Surface, shade};

/// A surface facing `+z` at the origin, which every shading scene below lights.
fn facing_camera() -> Surface {
	Surface::new(Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0))
}

/// A white light arriving straight along the surface's normal.
fn head_on(strength: f32) -> Incident {
	Incident { to_light: Vec3::new(0.0, 0.0, 1.0), radiance: Vec3::new(strength, strength, strength) }
}

pub fn material_unlit() -> Outcome {
	// `colour = base_colour * texture(uv)`, WITH NO LIGHT APPLIED AND NO NORMAL NEEDED. That is what
	// makes it a material rather than a special case of the lit ones: a sky, a gizmo, a debug overlay
	// and a video frame all want the colour they were given, and a Lambert material with no lights is
	// not the same thing - it is black.
	let unlit = material(MaterialKind::Unlit, Blending::Opaque).with_base_colour(Vec4::new(0.5, 0.25, 0.75, 1.0));
	let shaded = shade(&unlit, &facing_camera(), Vec3::new(0.0, 0.0, 5.0), Vec3::new(0.1, 0.1, 0.1), &[head_on(1.0)]).ok_or(crate::Trouble::Failed(alloc::string::String::from("an unlit material discarded")))?;
	require!(close(shaded.x, 0.5) && close(shaded.y, 0.25) && close(shaded.z, 0.75), "an unlit material answers its base colour whatever the lighting is: {shaded:?}");

	// AND THE TEXTURE MULTIPLIES IT, which is the other half of the equation.
	let textured = shade(&unlit, &facing_camera().with_texture(Vec4::new(0.5, 1.0, 1.0, 1.0)), Vec3::ZERO, Vec3::ZERO, &[]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(textured.x, 0.25) && close(textured.y, 0.25), "the texture multiplies the base colour: {textured:?}");
	Ok(())
}

pub fn material_vertex_color() -> Outcome {
	// `colour = vertex_colour * texture(uv)`, AND THE BASE COLOUR IS NOT IN IT. That is the whole
	// difference from `Unlit`, and it is the one a caller gets wrong: a material that multiplied both
	// would tint every vertex colour in the mesh by whatever the base happened to be.
	let vertexed = material(MaterialKind::VertexColor, Blending::Opaque).with_base_colour(Vec4::new(0.0, 0.0, 0.0, 1.0));
	let surface = facing_camera().with_vertex_colour(Vec4::new(0.25, 0.5, 1.0, 1.0));
	let shaded = shade(&vertexed, &surface, Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0), &[head_on(1.0)]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(shaded.x, 0.25) && close(shaded.y, 0.5) && close(shaded.z, 1.0), "a vertex-colour material answers the VERTEX colour and not the base: {shaded:?}");
	let textured = shade(&vertexed, &surface.with_texture(Vec4::new(1.0, 0.5, 0.5, 1.0)), Vec3::ZERO, Vec3::ZERO, &[]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(textured.y, 0.25), "and the texture multiplies that: {textured:?}");
	Ok(())
}

pub fn material_lambert() -> Outcome {
	// DIFFUSE ONLY: `base * (ambient + sum of light * max(dot(N, L), 0))`. The cosine is what makes it
	// Lambert, so the scene checks it at three angles - head on, at sixty degrees, and from BEHIND,
	// where the clamp at zero is what stops a light shining through a wall.
	let lambert = material(MaterialKind::Lambert, Blending::Opaque).with_base_colour(Vec4::new(1.0, 1.0, 1.0, 1.0));
	let head = shade(&lambert, &facing_camera(), Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, &[head_on(0.5)]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(head.x, 0.5), "a light straight on delivers all of itself: {head:?}");

	let oblique = Incident { to_light: Vec3::new(0.8660254, 0.0, 0.5), radiance: Vec3::new(0.5, 0.5, 0.5) };
	let angled = shade(&lambert, &facing_camera(), Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, &[oblique]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(angled.x, 0.25), "and one at sixty degrees delivers half of it, which is the cosine: {angled:?}");

	let behind = Incident { to_light: Vec3::new(0.0, 0.0, -1.0), radiance: Vec3::new(1.0, 1.0, 1.0) };
	let dark = shade(&lambert, &facing_camera(), Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, &[behind]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(dark.x, 0.0), "and one behind the surface delivers nothing rather than a negative amount: {dark:?}");

	// AND IT HAS NO SPECULAR AT ALL, which is what separates it from Blinn-Phong: the same surface,
	// the same light and a specular colour changes nothing.
	let with_specular = lambert.with_specular(Vec3::new(1.0, 1.0, 1.0), 32);
	let same = shade(&with_specular, &facing_camera(), Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, &[head_on(0.5)]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(same.x, head.x), "a Lambert material ignores a specular colour: {same:?}");
	Ok(())
}

pub fn material_blinn_phong() -> Outcome {
	// THE HALF VECTOR AND NOT THE REFLECTION VECTOR, which is what makes it Blinn-Phong and what keeps
	// the highlight from vanishing at grazing angles. With the eye and the light both on the normal,
	// `H` is the normal, `dot(N, H)` is one, and the highlight is the whole specular colour whatever
	// the exponent.
	let shiny = material(MaterialKind::BlinnPhong, Blending::Opaque).with_base_colour(Vec4::new(0.0, 0.0, 0.0, 1.0)).with_specular(Vec3::new(1.0, 1.0, 1.0), 64);
	let eye = Vec3::new(0.0, 0.0, 5.0);
	let hot = shade(&shiny, &facing_camera(), eye, Vec3::ZERO, &[head_on(1.0)]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(hot.x, 1.0), "with eye and light on the normal the highlight is the whole specular: {hot:?}");

	// OFF THE HIGHLIGHT IT FALLS AWAY WITH THE EXPONENT, and a larger exponent is a tighter highlight:
	// the same geometry under 4 and under 256 must differ, and in that direction.
	let oblique = Incident { to_light: Vec3::new(0.7071068, 0.0, 0.7071068), radiance: Vec3::new(1.0, 1.0, 1.0) };
	let broad = shade(&shiny.with_specular(Vec3::new(1.0, 1.0, 1.0), 4), &facing_camera(), eye, Vec3::ZERO, &[oblique]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	let tight = shade(&shiny.with_specular(Vec3::new(1.0, 1.0, 1.0), 256), &facing_camera(), eye, Vec3::ZERO, &[oblique]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(broad.x > tight.x, "a larger exponent is a tighter highlight: {} against {}", broad.x, tight.x);
	require!(tight.x >= 0.0 && broad.x < 1.0, "and off the centre it is less than the whole: {}", broad.x);

	// AND THE SPECULAR CARRIES ITS OWN COLOUR rather than multiplying the base, which is what lets a
	// red plastic keep a white highlight - a material that multiplied them would make every highlight
	// the colour of the thing it is on, which is what METAL does and plastic does not.
	let red = material(MaterialKind::BlinnPhong, Blending::Opaque).with_base_colour(Vec4::new(1.0, 0.0, 0.0, 1.0)).with_specular(Vec3::new(1.0, 1.0, 1.0), 64);
	let white = shade(&red, &facing_camera(), eye, Vec3::ZERO, &[head_on(0.5)]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(white.y, 0.5) && close(white.z, 0.5), "a red body keeps a white highlight: {white:?}");

	// AND A LIGHT BEHIND THE SURFACE ADDS NO HIGHLIGHT, which the half vector alone does not prevent:
	// `dot(N, H)` can be positive with the light behind, and the diffuse term is what gates it.
	let behind = Incident { to_light: Vec3::new(0.0, 0.0, -1.0), radiance: Vec3::new(1.0, 1.0, 1.0) };
	let none = shade(&shiny, &facing_camera(), eye, Vec3::ZERO, &[behind]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(none.x, 0.0), "a light behind the surface makes no highlight: {none:?}");
	Ok(())
}

pub fn linear_colour_space() -> Outcome {
	// THE EQUATION IS EVALUATED IN LINEAR LIGHT, and the property no encoded space has is LINEARITY:
	// twice the radiance is twice the answer. In an sRGB-encoded space doubling the light would
	// brighten by about 1.55, which is the too-dark shadow everybody recognises and nobody can name.
	let lambert = material(MaterialKind::Lambert, Blending::Opaque).with_base_colour(Vec4::new(1.0, 1.0, 1.0, 1.0));
	let dim = shade(&lambert, &facing_camera(), Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, &[head_on(0.2)]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	let bright = shade(&lambert, &facing_camera(), Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, &[head_on(0.4)]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(bright.x, dim.x * 2.0), "twice the light is twice the answer: {} against {}", bright.x, dim.x * 2.0);

	// TWO LIGHTS ARE THEIR SUM, which is the other half of linearity and the reason accumulation order
	// is a stated rule rather than an implementation detail.
	let pair = shade(&lambert, &facing_camera(), Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, &[head_on(0.2), head_on(0.2)]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(pair.x, bright.x), "and two lights are their sum: {} against {}", pair.x, bright.x);

	// AND THE TEXTURE ARRIVES ALREADY DECODED, which is what "decoded before it is multiplied" means:
	// an sRGB texel of 0.5 is 0.2140 of linear light, and it is that number the equation multiplies.
	let decoded = decode(Transfer::Srgb, 0.5) as f32;
	require!(close(decoded, 0.2140), "an sRGB half decodes to about 0.214 of linear light, and decodes to {decoded}");
	let unlit = material(MaterialKind::Unlit, Blending::Opaque);
	let sampled = shade(&unlit, &facing_camera().with_texture(Vec4::new(decoded, decoded, decoded, 1.0)), Vec3::ZERO, Vec3::ZERO, &[]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(sampled.x, decoded), "and the equation multiplies the decoded value and not the stored one: {sampled:?}");

	// AND THE RESULT IS CLAMPED IN LINEAR LIGHT, BEFORE ANY ENCODE. `Scene3D Core Profile 1` has no
	// high dynamic range - that is Extended - so a value above one has nowhere to go.
	let over = shade(&lambert, &facing_camera(), Vec3::new(0.0, 0.0, 5.0), Vec3::new(4.0, 4.0, 4.0), &[head_on(4.0)]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(over.x, 1.0), "an over-bright result is clamped to one: {over:?}");
	require!(close(over.w, 1.0), "and lighting never changed the alpha: {over:?}");
	Ok(())
}

pub fn normal_renormalisation() -> Outcome {
	// THE NORMAL IS RENORMALISED IN THE FRAGMENT STAGE. Interpolating unit vectors does not produce
	// unit vectors, and the error is largest IN THE MIDDLE of a large triangle - which is where a
	// hand-checked fixture would not look. So the check is that a normal at half length shades the
	// same as the unit one, and not at half the brightness.
	let lambert = material(MaterialKind::Lambert, Blending::Opaque).with_base_colour(Vec4::new(1.0, 1.0, 1.0, 1.0));
	let unit = shade(&lambert, &facing_camera(), Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, &[head_on(0.5)]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	let short = Surface::new(Vec3::ZERO, Vec3::new(0.0, 0.0, 0.5));
	let shaded = shade(&lambert, &short, Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, &[head_on(0.5)]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(shaded.x, unit.x), "a normal at half length shades as the unit one and not at half the brightness: {shaded:?}");
	let long = Surface::new(Vec3::ZERO, Vec3::new(0.0, 0.0, 4.0));
	let shaded = shade(&lambert, &long, Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, &[head_on(0.5)]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(shaded.x, unit.x), "and one at four times its length shades the same rather than four times as bright: {shaded:?}");

	// AND THE MIDPOINT OF TWO UNIT NORMALS IS THE CASE IT IS FOR: interpolating them gives something
	// SHORTER than one, and unrenormalised it would darken the middle of every large triangle.
	let interpolated = Vec3::new(0.7071068, 0.0, 0.7071068).add(Vec3::new(-0.7071068, 0.0, 0.7071068)).scale(0.5);
	require!(interpolated.length() < 0.95, "the midpoint of two unit normals is short, at {}", interpolated.length());
	let middle = shade(&lambert, &Surface::new(Vec3::ZERO, interpolated), Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, &[head_on(0.5)]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(middle.x, unit.x), "and it shades as a unit normal pointing the same way: {middle:?}");
	Ok(())
}

pub fn two_sided_normal_flip() -> Outcome {
	// THE NORMAL IS FLIPPED FOR A BACK-FACING FRAGMENT OF A TWO-SIDED MATERIAL. Without it the lit
	// side of a leaf is the side AWAY from the light - which is a thing people see and cannot explain,
	// because the geometry is right and the light is right.
	let leaf = material(MaterialKind::Lambert, Blending::Opaque).with_base_colour(Vec4::new(1.0, 1.0, 1.0, 1.0)).two_sided();
	let back = facing_camera().back_facing();
	let from_behind = Incident { to_light: Vec3::new(0.0, 0.0, -1.0), radiance: Vec3::new(0.5, 0.5, 0.5) };
	let lit = shade(&leaf, &back, Vec3::new(0.0, 0.0, -5.0), Vec3::ZERO, &[from_behind]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(lit.x, 0.5), "a two-sided surface seen from behind is lit by a light behind it: {lit:?}");

	// AND A ONE-SIDED MATERIAL IS NOT, which is what makes this a material property rather than a
	// rasteriser one: the same geometry and the same light, and the answer differs by the flag.
	let solid = material(MaterialKind::Lambert, Blending::Opaque).with_base_colour(Vec4::new(1.0, 1.0, 1.0, 1.0));
	let dark = shade(&solid, &back, Vec3::new(0.0, 0.0, -5.0), Vec3::ZERO, &[from_behind]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(dark.x, 0.0), "a one-sided one is dark from behind: {dark:?}");

	// AND A FRONT-FACING FRAGMENT IS UNTOUCHED BY THE FLAG, so a two-sided material is not simply one
	// whose normal points the other way.
	let front = shade(&leaf, &facing_camera(), Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, &[head_on(0.5)]).ok_or(crate::Trouble::Failed(alloc::string::String::from("discarded")))?;
	require!(close(front.x, 0.5), "and the front of a two-sided surface is lit the ordinary way: {front:?}");
	Ok(())
}

pub fn alpha_threshold_discard() -> Outcome {
	// A FRAGMENT WITH ALPHA STRICTLY BELOW THE THRESHOLD IS DISCARDED, so a threshold of ZERO discards
	// nothing. The strictness is the whole of the rule: "at or below" would make a threshold of zero
	// discard a fully transparent fragment, and a threshold of one discard everything including the
	// opaque ones.
	let masked = material(MaterialKind::Unlit, Blending::AlphaMask { threshold: 0.5 }).with_base_colour(Vec4::new(1.0, 1.0, 1.0, 0.4));
	require!(shade(&masked, &facing_camera(), Vec3::ZERO, Vec3::ZERO, &[]).is_none(), "alpha below the threshold is discarded");
	let kept = masked.with_base_colour(Vec4::new(1.0, 1.0, 1.0, 0.5));
	require!(shade(&kept, &facing_camera(), Vec3::ZERO, Vec3::ZERO, &[]).is_some(), "and alpha AT the threshold is kept, because the comparison is strict");

	let nothing = material(MaterialKind::Unlit, Blending::AlphaMask { threshold: 0.0 }).with_base_colour(Vec4::new(1.0, 1.0, 1.0, 0.0));
	require!(shade(&nothing, &facing_camera(), Vec3::ZERO, Vec3::ZERO, &[]).is_some(), "a threshold of zero discards nothing, not even a fully transparent fragment");

	// AND IT IS THE ALBEDO'S ALPHA - base times texture - RATHER THAN THE BASE'S, so a cut-out in a
	// texture cuts out.
	let textured = material(MaterialKind::Unlit, Blending::AlphaMask { threshold: 0.5 }).with_base_colour(Vec4::new(1.0, 1.0, 1.0, 1.0));
	require!(shade(&textured, &facing_camera().with_texture(Vec4::new(1.0, 1.0, 1.0, 0.1)), Vec3::ZERO, Vec3::ZERO, &[]).is_none(), "a texture's alpha cuts the fragment out");

	// AND A THRESHOLD OUTSIDE ZERO TO ONE IS REFUSED, because it discards everything or nothing
	// whatever the surface holds - which is a material that does not do what it says.
	let mut scene = world();
	require!(scene.add_material(material(MaterialKind::Unlit, Blending::AlphaMask { threshold: 1.5 })).is_err(), "a threshold past one is refused");
	require!(scene.add_material(material(MaterialKind::Unlit, Blending::AlphaMask { threshold: -0.5 })).is_err(), "and one below zero");
	Ok(())
}

pub fn queue_from_blending() -> Outcome {
	// THE QUEUE COMES FROM THE MATERIAL'S BLENDING AND NOT FROM A FLAG ON THE NODE. So a node's queue
	// changes when its material does, and THE TWO CAN NEVER DISAGREE - which is the defect a flag has:
	// somebody sets a material to blend and forgets the flag, and the object draws opaque.
	require!(material(MaterialKind::Lambert, Blending::Opaque).queue() == QueueKind::Opaque, "an opaque material is in the opaque queue");
	require!(material(MaterialKind::Unlit, Blending::AlphaMask { threshold: 0.5 }).queue() == QueueKind::AlphaMask, "an alpha-masked one is in the alpha-mask queue");
	require!(material(MaterialKind::Unlit, Blending::Blended).queue() == QueueKind::Transparent, "and a blended one is in the transparent queue");

	// AND THE BLEND STATE FOLLOWS THE BLENDING, so the two cannot be set to contradict each other by
	// accident: `with_blending` sets both, and a material whose pipeline disagrees is REFUSED.
	let mut scene = world();
	let contradictory = material(MaterialKind::Unlit, Blending::Blended).with_blend_state(AttachmentBlend { enabled: false, colour: BlendEquation::REPLACE, alpha: BlendEquation::REPLACE, write_mask: ColorWriteMask::ALL });
	require!(scene.add_material(contradictory).is_err(), "a transparent material that does not blend is refused, because it would draw opaque and read as a missing texture");
	let also = material(MaterialKind::Lambert, Blending::Opaque).with_blend_state(AttachmentBlend { enabled: true, colour: BlendEquation::PREMULTIPLIED_OVER, alpha: BlendEquation::PREMULTIPLIED_OVER, write_mask: ColorWriteMask::ALL });
	require!(scene.add_material(also).is_err(), "and an opaque material that blends is refused too");
	Ok(())
}

pub fn per_draw_blend_state() -> Outcome {
	// THE BLEND STATE IS PER DRAW, which is what lets two materials in one pass composite differently:
	// a window that blends `over` and a flame that ADDS are one frame, and a pass-wide blend state
	// would make them two.
	let over = material(MaterialKind::Unlit, Blending::Blended);
	require!(over.blend.enabled, "a blended material carries an enabled blend state");
	require!(over.blend.colour == BlendEquation::PREMULTIPLIED_OVER, "and the profile's default is premultiplied over: {:?}", over.blend.colour);

	// AND A CALLER MAY STATE ANOTHER EQUATION, which is the feature: the same blending, a different
	// composite. An additive blend is what fire and light shafts are, and it is not `over`.
	let additive = BlendEquation { source: render3d::blend::BlendFactor::One, destination: render3d::blend::BlendFactor::One, operation: render3d::blend::BlendOp::Add };
	let flame = material(MaterialKind::Unlit, Blending::Blended).with_blend_state(AttachmentBlend { enabled: true, colour: additive, alpha: additive, write_mask: ColorWriteMask::ALL });
	let mut scene = world();
	let window = scene.add_material(over)?;
	let fire = scene.add_material(flame)?;
	require!(scene.materials()[window as usize].blend.colour != scene.materials()[fire as usize].blend.colour, "two materials in one scene carry two different blend equations");
	require!(scene.materials()[fire as usize].blend.colour == additive, "and the one that asked to add, adds: {:?}", scene.materials()[fire as usize].blend.colour);
	require!(scene.materials()[window as usize].queue() == scene.materials()[fire as usize].queue(), "while both are still in the transparent queue, because the QUEUE is the blending and not the equation");
	Ok(())
}

pub fn per_draw_color_write_mask() -> Outcome {
	// THE COLOUR WRITE MASK IS PER DRAW TOO, and it is separate from blending: a depth-only prepass
	// writes NO colour and blends nothing, which a blend state alone cannot express.
	let mut scene = world();
	let depth_only = material(MaterialKind::Lambert, Blending::Opaque).with_blend_state(AttachmentBlend { enabled: false, colour: BlendEquation::REPLACE, alpha: BlendEquation::REPLACE, write_mask: ColorWriteMask::NONE });
	let ordinary = material(MaterialKind::Lambert, Blending::Opaque);
	let prepass = scene.add_material(depth_only)?;
	let shaded = scene.add_material(ordinary)?;
	require!(scene.materials()[prepass as usize].blend.write_mask == ColorWriteMask::NONE, "a depth-only material writes no colour channel");
	require!(scene.materials()[shaded as usize].blend.write_mask == ColorWriteMask::ALL, "while an ordinary one writes them all");
	require!(scene.materials()[prepass as usize].writes_depth(), "and the depth-only material still writes DEPTH, which is the whole of what it is for");

	// AND A MASK MAY NAME SOME CHANNELS, which is what an alpha-only or a colour-only pass is.
	let alpha_only = ColorWriteMask { red: false, green: false, blue: false, alpha: true };
	let mask = scene.add_material(material(MaterialKind::Unlit, Blending::Opaque).with_blend_state(AttachmentBlend { enabled: false, colour: BlendEquation::REPLACE, alpha: BlendEquation::REPLACE, write_mask: alpha_only }))?;
	require!(scene.materials()[mask as usize].blend.write_mask == alpha_only, "a mask may name one channel: {:?}", scene.materials()[mask as usize].blend.write_mask);
	require!(alpha_only != ColorWriteMask::ALL && alpha_only != ColorWriteMask::NONE, "and it is neither of the two named ones");
	Ok(())
}
