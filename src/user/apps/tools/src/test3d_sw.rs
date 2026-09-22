// test3d-sw - the interactive 3D demo, on the software backend.
//
// WHAT THIS IS FOR, AND WHAT IT IS NOT. The conformance suite is the PROOF that a backend implements
// `Render3D Core Profile 1`; this is the DEMO, and neither substitutes for the other. What a demo
// answers that a suite cannot is whether the whole path works when it is driven the way an
// application drives it: a scene, a camera that moves, lighting that changes, and a frame that has
// to be finished in time to be presented.
//
// THE PATH ONE FRAME TRAVELS is the point of the program: `render3d` describes the draw, `soft3d`
// executes it into an attachment, and the result reaches a `Surface`. A game, an editor and a map
// application all take that path, and it is the one nothing else in this tree exercises end to end.
//
// TWO GRANTS AND NOTHING ELSE - `display` and `input-keys`. Every vertex is computed here and every
// texture is generated here, so the program needs no volume, no font catalogue and no storage. That
// is a property to keep rather than an accident: a demo that needed a file would be a demo that
// could not run on a machine with no disk.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use graphics_app::{FrameLoop, Step};
use graphics_core::geom::{Extent2D, PointF, RectF};
use graphics_core::layout::{ImageLayout, RowOrigin};
use graphics_core::semantics::ImageSemantics;
use graphics_core::{AlphaMode, ColorSpace, ImageView, ImageViewMut, OwnedImage, PixelFormat, PixelStorage};
use keys::usage;
use proto::system::{LaunchContext, input};
use render_math::camera::Viewport;
use render_math::{Mat4, Quat, Vec3, Vec4, camera, sqrt};
use render_shader::builder::Builder;
use render_shader::ir::{BinaryOp, Binding, Constant, Module, Op, Output, Stage, Transcendental, Type, UnaryOp};
use render2d::Canvas;
use render2d::backend::{Backend, TargetDescription};
use render2d::list::{DrawList, ImageRecord, RecordedGlyphRun};
use render2d::paint::{Color, ImageQuality, Paint};
use render2d::path::{FillRule, PathBuilder};
use render3d::{CompareOp, Cull as CullMode, DepthFormat, Render3DLimits, Topology};
use rt::*;
use soft2d::Soft2d;
use soft3d::frame::{Attachments, Draw, Pipeline, Prepared, Source};
use soft3d::pass::{Colour, DepthStencil};
use soft3d::texture::{Filter, Kind, Level, Sampler, Texture, Wrap};
use soft3d::{Indices, Val};

const USAGE: &[u8] = b"Usage: test3d-sw [--width N] [--height N] [--scene-width N] [--scene-height N] [--frames N] [--no-input] [--fixed] [--pose N] [--report] [--extended]\nA rotating lit cube over a textured ground, with a transparent panel and a 2D overlay.\n--extended draws the Scene3D Extended scene instead: a physically based sphere with a normal map and a cast shadow.\nEsc or q exits, Space pauses, R resets, P shows the picking buffer, arrows orbit, +/- changes distance.\nThe scene renders at the window's own size; --scene-width/--scene-height fix it, which a slow machine wants.\n";

// THE SCENE IS RENDERED AT THE SURFACE'S OWN SIZE unless a run says otherwise, because that is what
// an application does and what makes a measurement at a stated resolution mean anything: a demo that
// always rendered at one internal size and scaled would report the same 3D cost at every window size
// and call the difference a measurement.
//
// `--scene WxH` FIXES IT, for two callers that need it fixed: a pixel check that reads known
// positions, and a comparison between two runs at the same number of fragments. A software
// rasteriser's cost is per fragment, so this is also the control a person reaches for when they want
// the demo to move quickly on a machine that renders slowly.
const SCENE_MIN: u32 = 32;
const SCENE_MAX: u32 = 2048;

// How far the camera starts from the origin, and the bounds `+`/`-` move it between.
const DISTANCE_START: f32 = 4.2;
const DISTANCE_MIN: f32 = 2.0;
const DISTANCE_MAX: f32 = 12.0;

// The rotation a clock-driven frame advances by. A FIXED STEP PER FRAME rather than a rate times a
// measured interval, because the deterministic mode below has to produce the same picture on a
// machine that renders at one frame a second and on one that renders at sixty.
const SPIN_PER_FRAME: f32 = 0.031;

// The two textures and the two samplers they are read through. BOTH ADDRESSING MODES, on purpose:
// the cube face's coordinates stay inside `0..=1` and are CLAMPED, the ground's run to six and
// REPEAT - and a backend that implemented one of the two would render one of these two surfaces
// wrong in a way nothing else in this scene would show.
const TEX_CHECKER: u32 = 0;
const TEX_GROUND: u32 = 1;
const SAMP_CLAMP: u32 = 0;
const SAMP_REPEAT: u32 = 1;

// How many times the ground's texture repeats across it. The number matters only in that it is
// GREATER THAN ONE: a ground whose coordinates stayed inside the texture would never wrap.
const GROUND_REPEATS: f32 = 6.0;

// The object identities the integer attachment carries. ZERO IS "NOTHING", which is what the clear
// leaves behind - so a pixel the scene never covered answers with no object rather than with the
// first one.
const ID_NONE: f32 = 0.0;
const ID_CUBE: f32 = 1.0;
const ID_GROUND: f32 = 2.0;
const ID_PANEL: f32 = 3.0;

// Which attachment each output goes to: the colour a person sees, and the identity a pick reads.
const ATTACH_COLOUR: u32 = 0;
const ATTACH_IDENT: u32 = 1;

// The uniform block members the shaders read. Named rather than numbered at the call sites, because
// a member index is exactly the kind of number two halves of one program disagree about silently.
const U_MVP: u32 = 0;
const U_MODEL: u32 = 1;
const U_LIGHT_DIR: u32 = 2;
const U_LIGHT_POINT: u32 = 3;
const U_EYE: u32 = 4;
const U_AMBIENT: u32 = 5;
// THE LIGHT'S OWN VIEW-PROJECTION, which the Extended phase needs in BOTH stages: the shadow pass
// draws through it and the lighting pass looks the fragment up in what it wrote.
const U_LIGHT_VP: u32 = 6;

// -------------------------------------------------------------------------------------------------
// `Scene3D Extended Profile 1`'s own phase: a physically based sphere with a normal map, and a cast
// shadow. SEPARATE FROM THE CORE SCENE AND NOT ADDED TO IT, because the core demo is a core gate:
// `--extended` selects this scene instead, and nothing the core phase does changes.
// -------------------------------------------------------------------------------------------------

/// The tangent-space normal map, generated like the checkerboards are.
const TEX_NORMAL: u32 = 2;
/// The shadow map: what the light saw, as depth in a colour attachment.
const TEX_SHADOW: u32 = 3;
const SAMP_SHADOW: u32 = 2;
const ID_SPHERE: f32 = 4.0;
/// The shadow map's extent. SQUARE AND FIXED, because the light's projection is square: a map whose
/// aspect did not match the projection's would stretch every shadow along one axis.
const SHADOW_EXTENT: u32 = 512;
/// The constant depth bias, in light-space depth units.
///
/// WITHOUT ONE EVERY LIT SURFACE SHADOWS ITSELF: the depth the map holds for a texel and the depth
/// the fragment has are the same surface sampled at two different places, and half the comparisons
/// come out the wrong way. The value is a texel's worth of slope at this extent and this projection.
const SHADOW_BIAS: f32 = 0.0035;
/// How far across the scene the light's orthographic box reaches.
const SHADOW_HALF_EXTENT: f32 = 3.0;
/// The sphere's own roughness and reflectance, which this scene fixes rather than sampling: the maps
/// under test here are the NORMAL map and the shadow map, and a metallic-roughness map would put a
/// third thing in one picture.
const PBR_ROUGHNESS: f32 = 0.35;
const PBR_F0: f32 = 0.04;
/// The Extended light's intensity. THE DIFFUSE TERM CARRIES A `1 / pi`, which is the BRDF's own
/// normalisation and not a brightness choice - a scene lit by a light of intensity one through a
/// correctly normalised BRDF is a third as bright as the same scene through an unnormalised one, and
/// the answer is to state the light's intensity rather than to drop the divisor.
const PBR_LIGHT: f32 = 3.0;

// One vertex of the scene. Position and normal are in MODEL space; the shader transforms both.
#[derive(Clone, Copy)]
struct Vertex {
	position: [f32; 4],
	normal: [f32; 4],
	colour: [f32; 4],
	/// The texture coordinate, as a vec4 so every attribute in this program has one shape.
	uv: [f32; 4],
	/// WHICH TEXTURE THIS SURFACE CARRIES, as a weight per texture rather than an index.
	///
	/// A sample reads a binding, and a binding is part of the SHADER - so "which texture" cannot come
	/// out of a vertex attribute the way a colour can. What can is how much of each sampled result to
	/// use, which is what these two numbers are: `1, 0` is the checkered cube face, `0, 1` is the
	/// ground, and `0, 0` is every untextured surface, whose colour is its material's alone.
	weights: [f32; 4],
	/// WHICH OBJECT THIS SURFACE BELONGS TO, as a number the fragment stage writes into the integer
	/// attachment beside the colour one. That attachment is what picking IS: an application asks
	/// "what is under this pixel", and the only answer that survives lighting, texturing and
	/// transparency is one the rasteriser wrote at the same time as the colour.
	ident: [f32; 4],
	/// THE TANGENT, in model space, with the handedness in `w`.
	///
	/// A NORMAL MAP IS IN TANGENT SPACE AND NOTHING ELSE KNOWS WHERE THAT IS. The map's three numbers
	/// are a direction relative to the surface's own texture axes, so a surface that carries a normal
	/// map has to carry those axes too - the bitangent is the cross product, which is why one vector
	/// and a sign is enough. Zero for a surface with no map, which the core phase's every vertex is.
	tangent: [f32; 4],
}

// The scene's geometry, as one indexed mesh.
//
// ONE MESH AND NOT TWO DRAWS, for now: the cube and the ground share a pipeline and a transform, so
// what tells them apart is the vertex data. The panel and the textures are the next slice.
struct Mesh {
	vertices: Vec<Vertex>,
	indices: Vec<u32>,
}

impl Mesh {
	// A cube with FLAT per-face normals and visibly distinct per-face colours, which needs
	// twenty-four vertices rather than eight: a vertex carries one normal, so a corner shared by
	// three faces cannot carry all three of theirs. A cube drawn from eight vertices is a cube with
	// smoothed lighting, which is a different object.
	fn scene() -> Mesh {
		let faces: [([f32; 3], [f32; 3], [f32; 4]); 6] = [
			([0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [0.86, 0.22, 0.22, 1.0]),
			([0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [0.20, 0.62, 0.86, 1.0]),
			([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.26, 0.76, 0.34, 1.0]),
			([-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.92, 0.74, 0.20, 1.0]),
			([0.0, 1.0, 0.0], [0.0, 0.0, -1.0], [0.78, 0.36, 0.82, 1.0]),
			([0.0, -1.0, 0.0], [0.0, 0.0, 1.0], [0.30, 0.78, 0.78, 1.0]),
		];
		let mut vertices: Vec<Vertex> = Vec::new();
		let mut indices: Vec<u32> = Vec::new();
		for (face, (normal, up, colour)) in faces.into_iter().enumerate() {
			let n = Vec3::new(normal[0], normal[1], normal[2]);
			let u = Vec3::new(up[0], up[1], up[2]);
			// The face's own basis: the normal, an "up" that is not parallel to it, and their cross.
			let right = Vec3::new(u.y * n.z - u.z * n.y, u.z * n.x - u.x * n.z, u.x * n.y - u.y * n.x);
			let base = vertices.len() as u32;
			// COUNTER-CLOCKWISE SEEN FROM OUTSIDE, which is what makes back-face culling cull the
			// inside of the cube and not the outside of it.
			// ONE FACE CARRIES THE CHECKERBOARD and the other five do not, which is what makes the
			// texture visible AS a texture: a cube textured on every side and a cube with a
			// patterned material look the same from any single angle.
			let weights = if face == 0 { [1.0, 0.0, 0.0, 0.0] } else { [0.0; 4] };
			for (sx, sy) in [(-1.0_f32, -1.0_f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
				let position = [n.x + right.x * sx + u.x * sy, n.y + right.y * sx + u.y * sy, n.z + right.z * sx + u.z * sy, 1.0];
				// THE CORNERS ARE EXACTLY `0` AND `1`, so the clamped sampler's edge behaviour is
				// the one under test and not an accident of where the texture happened to end.
				let uv = [(sx + 1.0) * 0.5, (1.0 - sy) * 0.5, 0.0, 0.0];
				vertices.push(Vertex { position: [position[0] * 0.75, position[1] * 0.75, position[2] * 0.75, 1.0], normal: [n.x, n.y, n.z, 0.0], colour, uv, weights, ident: [ID_CUBE, 0.0, 0.0, 0.0], tangent: [0.0; 4] });
			}
			indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
		}
		// THE GROUND, so the cube has something to be ABOVE and something to occlude. A demo with
		// nothing behind the object cannot show depth at all.
		let ground = vertices.len() as u32;
		let grey = [0.18, 0.19, 0.24, 1.0];
		for (x, z) in [(-6.0_f32, -6.0_f32), (6.0, -6.0), (6.0, 6.0), (-6.0, 6.0)] {
			// COORDINATES WELL OUTSIDE `0..=1`, which is the whole of the repeat test: a ground whose
			// UVs stayed in range would sample the same texel pattern once and prove nothing.
			let uv = [(x + 6.0) / 12.0 * GROUND_REPEATS, (z + 6.0) / 12.0 * GROUND_REPEATS, 0.0, 0.0];
			vertices.push(Vertex { position: [x, -1.4, z, 1.0], normal: [0.0, 1.0, 0.0, 0.0], colour: grey, uv, weights: [0.0, 1.0, 0.0, 0.0], ident: [ID_GROUND, 0.0, 0.0, 0.0], tangent: [1.0, 0.0, 0.0, 1.0] });
		}
		indices.extend_from_slice(&[ground, ground + 2, ground + 1, ground, ground + 3, ground + 2]);
		Mesh { vertices, indices }
	}

	/// The Extended phase's scene: a sphere on the same ground the core scene uses.
	///
	/// A SPHERE AND NOT A CUBE, because what this scene is for is a normal map and a shadow, and both
	/// are invisible on a flat face: a normal map perturbs a direction, so a surface whose direction
	/// never changes shows one constant perturbation, and a shadow cast by a box is a box.
	///
	/// THE TANGENTS ARE DERIVED FROM THE PARAMETRISATION rather than solved from the triangles. The
	/// sphere's `u` runs along its longitude, so the tangent IS the derivative of the position with
	/// respect to that angle - exact, per vertex, and with no averaging over a triangle fan at the
	/// poles where an averaged tangent is undefined.
	fn extended() -> Mesh {
		let mut vertices: Vec<Vertex> = Vec::new();
		let mut indices: Vec<u32> = Vec::new();
		const RINGS: u32 = 48;
		const SEGMENTS: u32 = 96;
		const RADIUS: f32 = 0.9;
		// THE UV REPEATS, so the map's own texels are visible as a pattern rather than stretched once
		// over the whole sphere.
		const UV_REPEATS: f32 = 4.0;
		let base_colour = [0.82, 0.78, 0.72, 1.0];
		for ring in 0..=RINGS {
			let polar = ring as f32 / RINGS as f32 * core::f32::consts::PI;
			let (sin_polar, cos_polar) = (sin(polar), cos(polar));
			for segment in 0..=SEGMENTS {
				let azimuth = segment as f32 / SEGMENTS as f32 * core::f32::consts::PI * 2.0;
				let (sin_azimuth, cos_azimuth) = (sin(azimuth), cos(azimuth));
				let normal = [sin_polar * cos_azimuth, cos_polar, sin_polar * sin_azimuth];
				let position = [normal[0] * RADIUS, normal[1] * RADIUS, normal[2] * RADIUS, 1.0];
				// d(position)/d(azimuth), with the `sin_polar` factor divided out so it is a unit
				// vector at every latitude. AT A POLE that factor is zero and the derivative
				// vanishes; the expression without it is the limit, which keeps the pole's tangent
				// in the same family as its neighbours' rather than at an arbitrary angle to them.
				let tangent = [-sin_azimuth, 0.0, cos_azimuth];
				let uv = [segment as f32 / SEGMENTS as f32 * UV_REPEATS, ring as f32 / RINGS as f32 * UV_REPEATS, 0.0, 0.0];
				vertices.push(Vertex { position: [position[0], position[1] + 0.35, position[2], 1.0], normal: [normal[0], normal[1], normal[2], 0.0], colour: base_colour, uv, weights: [0.0; 4], ident: [ID_SPHERE, 0.0, 0.0, 0.0], tangent: [tangent[0], tangent[1], tangent[2], 1.0] });
			}
		}
		let stride = SEGMENTS + 1;
		for ring in 0..RINGS {
			for segment in 0..SEGMENTS {
				let a = ring * stride + segment;
				let b = a + stride;
				indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
			}
		}
		// THE GROUND THE SHADOW FALLS ON. A cast shadow needs something to be cast ONTO, and a scene
		// whose only surface is the caster shows nothing at all.
		let ground = vertices.len() as u32;
		let grey = [0.34, 0.35, 0.38, 1.0];
		for (x, z) in [(-6.0_f32, -6.0_f32), (6.0, -6.0), (6.0, 6.0), (-6.0, 6.0)] {
			let uv = [(x + 6.0) / 12.0 * GROUND_REPEATS, (z + 6.0) / 12.0 * GROUND_REPEATS, 0.0, 0.0];
			vertices.push(Vertex { position: [x, -1.4, z, 1.0], normal: [0.0, 1.0, 0.0, 0.0], colour: grey, uv, weights: [0.0; 4], ident: [ID_GROUND, 0.0, 0.0, 0.0], tangent: [1.0, 0.0, 0.0, 1.0] });
		}
		indices.extend_from_slice(&[ground, ground + 2, ground + 1, ground, ground + 3, ground + 2]);
		Mesh { vertices, indices }
	}

	/// The semi-transparent panel that stands BETWEEN the camera and the cube.
	///
	/// IN FRONT, AND NOT BESIDE. A translucent quad drawn where nothing is behind it blends against
	/// the background and shows only that the blend arithmetic runs; one that overlaps the cube shows
	/// that the depth test admitted a fragment it must not write, that the source and destination
	/// went into the equation the right way round, and that the ground behind it still occludes what
	/// is behind the ground. Those are three different faults and this is the one position that
	/// distinguishes them.
	///
	/// ITS OWN ALPHA AND NO TEXTURE: what this surface is for is the blend, and a texture on it would
	/// put two things under test in one place.
	fn panel() -> Mesh {
		let mut vertices: Vec<Vertex> = Vec::new();
		let colour = [0.35, 0.85, 0.95, 0.45];
		for (x, y) in [(-0.95_f32, -0.75_f32), (0.95, -0.75), (0.95, 0.75), (-0.95, 0.75)] {
			vertices.push(Vertex { position: [x, y, 1.45, 1.0], normal: [0.0, 0.0, 1.0, 0.0], colour, uv: [0.0; 4], weights: [0.0; 4], ident: [ID_PANEL, 0.0, 0.0, 0.0], tangent: [0.0; 4] });
		}
		Mesh { vertices, indices: vec![0, 1, 2, 0, 2, 3] }
	}
}

/// Which mesh the frame is drawing. The opaque pass and the transparent one read the same uniforms
/// and differ in their geometry and their pipeline, so one `Source` serves both and this says which.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pass {
	Scene,
	Panel,
	/// The Extended phase's depth pass, drawn from the light.
	Shadow,
	/// The Extended phase's lighting pass.
	Pbr,
}

/// A checkerboard, generated rather than loaded - which is what keeps this program's grants at two.
///
/// LINEAR TEXELS AND NOT sRGB ONES. The shading below multiplies this into a material colour that is
/// already linear, and a texture the sampler transfer-decoded would be a second decode over numbers
/// that were never encoded.
fn checkerboard(id: u32, extent: u32, squares: u32, light: [f32; 4], dark: [f32; 4]) -> Texture {
	let mut level = Level::new(extent, extent, 1);
	let cell = (extent / squares).max(1);
	for y in 0..extent {
		for x in 0..extent {
			let odd = ((x / cell) + (y / cell)) % 2 == 1;
			level.set(x, y, 0, if odd { dark } else { light });
		}
	}
	Texture { id, kind: Kind::Dim2, levels: vec![level], transfer: graphics_profile::image::Transfer::Linear, semantics: graphics_profile::image::Semantics::Color, premultiplied: false }
}

/// A tangent-space normal map, generated the way the checkerboards are.
///
/// A GRID OF ROUND BUMPS, from the analytic derivative of a height field rather than from a
/// difference of neighbouring texels: the field is `cos(u) * cos(v)`, so its two slopes are known
/// exactly and the map has no quantisation of its own to confuse with the sampler's.
///
/// ENCODED INTO `0..=1` LIKE EVERY NORMAL MAP THERE IS, and declared `Normal` so the sampler never
/// transfer-decodes it: these three numbers are a DIRECTION, and decoding one bends every normal
/// toward the surface - which looks like a lighting bug and is a colour-space one.
fn normal_map(id: u32, extent: u32, bumps: u32, amplitude: f32) -> Texture {
	let mut level = Level::new(extent, extent, 1);
	let turns = bumps as f32 * core::f32::consts::PI * 2.0;
	for y in 0..extent {
		for x in 0..extent {
			let u = x as f32 / extent as f32;
			let v = y as f32 / extent as f32;
			// The height field's own slopes: `d/du` and `d/dv` of `amplitude * cos(turns u) cos(turns v)`.
			let slope_u = -amplitude * turns * sin(turns * u) * cos(turns * v);
			let slope_v = -amplitude * turns * cos(turns * u) * sin(turns * v);
			let length = sqrt(slope_u * slope_u + slope_v * slope_v + 1.0);
			let normal = [-slope_u / length, -slope_v / length, 1.0 / length];
			level.set(x, y, 0, [normal[0] * 0.5 + 0.5, normal[1] * 0.5 + 0.5, normal[2] * 0.5 + 0.5, 1.0]);
		}
	}
	Texture { id, kind: Kind::Dim2, levels: vec![level], transfer: graphics_profile::image::Transfer::Linear, semantics: graphics_profile::image::Semantics::Normal, premultiplied: false }
}

/// A shadow map before anything has been rendered into it: FULLY LIT, because a map of a light that
/// has not drawn yet must not shadow the first frame.
fn empty_shadow_map(id: u32, extent: u32) -> Texture {
	let mut level = Level::new(extent, extent, 1);
	for y in 0..extent {
		for x in 0..extent {
			level.set(x, y, 0, [1.0, 1.0, 1.0, 1.0]);
		}
	}
	Texture { id, kind: Kind::Dim2, levels: vec![level], transfer: graphics_profile::image::Transfer::Linear, semantics: graphics_profile::image::Semantics::Depth, premultiplied: false }
}

/// Read the shadow pass's attachment back into the texture the lighting pass samples.
///
/// THE COPY IS THE WHOLE OF WHAT THIS STACK HAS NO COMPARISON SAMPLER FOR. `Render3D Core Profile 1`
/// carries no depth-texture binding, so the light's depth reaches the lighting pass as ordinary
/// texels - which is also why the shadow pass writes depth into a COLOUR attachment rather than
/// relying on the depth buffer it also has.
fn read_shadow_map(attachment: &Colour, into: &mut Texture) {
	let Some(level) = into.levels.first_mut() else { return };
	for y in 0..level.height.min(attachment.height) {
		for x in 0..level.width.min(attachment.width) {
			let texel = attachment.at(x, y, 0);
			level.set(x, y, 0, [texel.x, texel.y, texel.z, texel.w]);
		}
	}
}

/// How many of the shadow map's texels hold a caster rather than the far plane.
///
/// WHAT IT PROVES IS THAT THE LIGHT SAW SOMETHING. A map that is entirely far plane is what a light
/// pointed away from the scene writes, what a projection too small to contain the caster writes, and
/// what a pass that was never executed leaves behind - and all three of those produce a picture with
/// no shadow in it, which is also what a correct scene with nothing to cast one looks like.
fn shadow_coverage(map: &Texture) -> u32 {
	let Some(level) = map.levels.first() else { return 0 };
	let mut covered = 0;
	for y in 0..level.height {
		for x in 0..level.width {
			if level.at(x, y, 0)[0] < 1.0 {
				covered += 1;
			}
		}
	}
	covered
}

// What the shaders read: the mesh, and the frame's transforms and lights.
struct Frame3d {
	mesh: Mesh,
	panel: Mesh,
	/// The Extended phase's geometry: the sphere and its ground.
	extended: Mesh,
	pass: Pass,
	checker: Texture,
	ground: Texture,
	/// The tangent-space normal map the sphere carries.
	normal_map: Texture,
	/// WHAT THE LIGHT SAW, rebuilt from the shadow pass's attachment each frame.
	///
	/// A TEXTURE AND NOT A TARGET, because this stack has no comparison sampler and a backend's
	/// attachment is not a sampler binding: the shadow pass writes light-space depth into a colour
	/// attachment, the frame reads it back, and the lighting pass samples it like any other texture.
	/// On a software renderer the read back costs a copy of one small buffer; on a GPU it is the
	/// place a real backend would bind the attachment directly.
	shadow_map: Texture,
	clamped: Sampler,
	repeated: Sampler,
	/// The sampler the shadow map is read through. NEAREST AND CLAMPED: a filtered depth is the
	/// average of two surfaces and is a depth nothing is at, and a lookup outside the light's box
	/// must read the far plane rather than wrap round to the other side of the scene.
	shadowed: Sampler,
	mvp: Mat4,
	model: Mat4,
	/// The light's own view-projection, which both Extended passes need.
	light_vp: Mat4,
	light_dir: [f32; 4],
	light_point: [f32; 4],
	eye: [f32; 4],
	ambient: [f32; 4],
}

impl Frame3d {
	fn active(&self) -> &Mesh {
		match self.pass {
			Pass::Scene => &self.mesh,
			Pass::Panel => &self.panel,
			// ONE MESH FOR BOTH EXTENDED PASSES, which is what makes the shadow the shape of the
			// thing casting it: a caster drawn from different geometry than its surface is a shadow
			// whose silhouette does not match what stands in the light.
			Pass::Shadow | Pass::Pbr => &self.extended,
		}
	}
}

impl Source for Frame3d {
	fn attribute(&self, location: u32, vertex: u32, _instance: u32) -> Option<Val> {
		let vertex = self.active().vertices.get(vertex as usize)?;
		match location {
			0 => Some(Val::vector_f32(&vertex.position)),
			1 => Some(Val::vector_f32(&vertex.normal)),
			2 => Some(Val::vector_f32(&vertex.colour)),
			3 => Some(Val::vector_f32(&vertex.uv)),
			4 => Some(Val::vector_f32(&vertex.weights)),
			5 => Some(Val::vector_f32(&vertex.ident)),
			6 => Some(Val::vector_f32(&vertex.tangent)),
			_ => None,
		}
	}

	fn uniform(&self, block: u32, member: u32) -> Option<Val> {
		if block != 0 {
			return None;
		}
		match member {
			U_MVP => Some(Val::matrix(&columns(self.mvp), 4)),
			U_MODEL => Some(Val::matrix(&columns(self.model), 4)),
			U_LIGHT_DIR => Some(Val::vector_f32(&self.light_dir)),
			U_LIGHT_POINT => Some(Val::vector_f32(&self.light_point)),
			U_EYE => Some(Val::vector_f32(&self.eye)),
			U_AMBIENT => Some(Val::vector_f32(&self.ambient)),
			U_LIGHT_VP => Some(Val::matrix(&columns(self.light_vp), 4)),
			_ => None,
		}
	}

	/// THE BACKEND'S OWN SAMPLER AND NOT A SECOND ONE WRITTEN HERE. Wrapping, the half-texel centre
	/// and the filter are rules `soft3d` freezes; a demo that computed its own texel address would
	/// render a checkerboard while proving nothing about the code a real application reaches.
	fn sample(&self, texture: u32, sampler: u32, coordinate: &Val) -> Option<Val> {
		if coordinate.len() < 2 {
			return None;
		}
		let (u, v) = (coordinate.f32_at(0), coordinate.f32_at(1));
		let (texture, sampler) = match (texture, sampler) {
			(TEX_CHECKER, SAMP_CLAMP) => (&self.checker, &self.clamped),
			(TEX_GROUND, SAMP_REPEAT) => (&self.ground, &self.repeated),
			(TEX_NORMAL, SAMP_REPEAT) => (&self.normal_map, &self.repeated),
			(TEX_SHADOW, SAMP_SHADOW) => (&self.shadow_map, &self.shadowed),
			_ => return None,
		};
		// MAGNIFYING, WITH NO MIP CHAIN. These textures are read at close to one texel per pixel and
		// carry one level, so a minification path would select a level that does not exist.
		let texel = soft3d::texture::sample(texture, sampler, [u, v, 0.0], 0.0, true);
		Some(Val::vector_f32(&texel))
	}

	fn indices(&self) -> Indices<'_> {
		Indices::U32(&self.active().indices)
	}
}

// A matrix as the four columns the value form takes. COLUMN-MAJOR, like everything else here.
fn columns(m: Mat4) -> [[f32; 4]; 4] {
	let mut out = [[0.0_f32; 4]; 4];
	for (index, column) in out.iter_mut().enumerate() {
		let source = m.column(index);
		*column = [source.x, source.y, source.z, source.w];
	}
	out
}

// The model transform: a rotation about the vertical axis, which is what makes the cube turn.
fn rotation_y(radians: f32) -> Mat4 {
	match Quat::from_axis_angle(Vec3::new(0.0, 1.0, 0.0), radians) {
		Ok(rotation) => Mat4::from_linear(&rotation.to_mat3(), Vec3::new(0.0, 0.0, 0.0)),
		Err(_) => Mat4::IDENTITY,
	}
}

// The vertex stage: clip position from the MVP, and the world-space normal and position the
// fragment stage lights with.
//
// THE NORMAL IS TRANSFORMED BY THE MODEL MATRIX AND ITS W IS ZERO, which is what makes it a
// DIRECTION rather than a point: a normal that carried w = 1 would pick up the translation and
// point somewhere else the moment the object moved.
fn vertex_stage() -> Module {
	let mut builder = Builder::new(Stage::Vertex, "scene-vertex");
	builder.varying(0, Type::vec(4), render_shader::Interpolation::Smooth);
	builder.varying(1, Type::vec(4), render_shader::Interpolation::Smooth);
	builder.varying(2, Type::vec(4), render_shader::Interpolation::Smooth);
	builder.varying(3, Type::vec(4), render_shader::Interpolation::Smooth);
	// THE WEIGHTS INTERPOLATE LIKE EVERYTHING ELSE, and they are constant across each surface, so
	// what reaches a fragment is the value its whole primitive carries. A flat qualifier would say
	// the same thing here and would stop saying it the moment a surface blended between two.
	builder.varying(4, Type::vec(4), render_shader::Interpolation::Smooth);
	// AND THE IDENTITY IS FLAT, which is the one varying here that must be. An object id is a NAME
	// and not a quantity: interpolating it between two vertices of the same object is harmless only
	// as long as they agree, and the moment a triangle's corners carry different ids a smooth
	// qualifier invents objects that do not exist across the middle of it.
	builder.varying_at(5, Type::vec(4), render_shader::Interpolation::Flat, render_shader::ir::Sampling::Pixel);
	let position = builder.load(Type::vec(4), Binding::Attribute { location: 0 });
	let normal = builder.load(Type::vec(4), Binding::Attribute { location: 1 });
	let colour = builder.load(Type::vec(4), Binding::Attribute { location: 2 });
	let uv = builder.load(Type::vec(4), Binding::Attribute { location: 3 });
	let weights = builder.load(Type::vec(4), Binding::Attribute { location: 4 });
	let ident = builder.load(Type::vec(4), Binding::Attribute { location: 5 });
	let mvp = builder.load(Type::Matrix(4), Binding::Uniform { block: 0, member: U_MVP });
	let model = builder.load(Type::Matrix(4), Binding::Uniform { block: 0, member: U_MODEL });
	let clip = builder.assign(Type::vec(4), Op::Binary(BinaryOp::MatrixProduct, mvp, position));
	let world = builder.assign(Type::vec(4), Op::Binary(BinaryOp::MatrixProduct, model, position));
	let world_normal = builder.assign(Type::vec(4), Op::Binary(BinaryOp::MatrixProduct, model, normal));
	builder.store(Output::Position, clip);
	builder.store(Output::Varying(0), colour);
	builder.store(Output::Varying(1), world_normal);
	builder.store(Output::Varying(2), world);
	builder.store(Output::Varying(3), uv);
	builder.store(Output::Varying(4), weights);
	builder.store(Output::Varying(5), ident);
	builder.finish()
}

// The fragment stage: ambient, one directional light, one point light and a specular highlight.
//
// EVERY LIGHT IS CLAMPED AT ZERO BEFORE IT IS ADDED. A dot product that went negative would SUBTRACT
// light from the side of the object facing away, which is not darkness - it is a hole.
fn fragment_stage() -> Module {
	let mut builder = Builder::new(Stage::Fragment, "scene-fragment");
	builder.varying(0, Type::vec(4), render_shader::Interpolation::Smooth);
	builder.varying(1, Type::vec(4), render_shader::Interpolation::Smooth);
	builder.varying(2, Type::vec(4), render_shader::Interpolation::Smooth);
	builder.varying(3, Type::vec(4), render_shader::Interpolation::Smooth);
	builder.varying(4, Type::vec(4), render_shader::Interpolation::Smooth);
	builder.varying_at(5, Type::vec(4), render_shader::Interpolation::Flat, render_shader::ir::Sampling::Pixel);
	let material = builder.load(Type::vec(4), Binding::Varying { location: 0 });
	let normal_in = builder.load(Type::vec(4), Binding::Varying { location: 1 });
	let world = builder.load(Type::vec(4), Binding::Varying { location: 2 });
	let light_dir = builder.load(Type::vec(4), Binding::Uniform { block: 0, member: U_LIGHT_DIR });
	let light_point = builder.load(Type::vec(4), Binding::Uniform { block: 0, member: U_LIGHT_POINT });
	let eye = builder.load(Type::vec(4), Binding::Uniform { block: 0, member: U_EYE });
	let ambient = builder.load(Type::vec(4), Binding::Uniform { block: 0, member: U_AMBIENT });
	let zero = builder.constant(Constant::F32(0.0));
	let one = builder.constant(Constant::F32(1.0));
	let shininess = builder.constant(Constant::F32(24.0));
	let spec_strength = builder.constant(Constant::F32(0.35));
	let point_strength = builder.constant(Constant::F32(0.55));

	// BOTH TEXTURES ARE READ AND THE WEIGHTS DECIDE HOW MUCH OF EACH IS USED.
	//
	// A BRANCH WOULD HAVE BEEN THE OTHER WAY TO WRITE THIS and it would have been the wrong one: a
	// sample inside a conditional is a sample whose derivatives are undefined for the fragments that
	// did not take the branch, which is how a mip selection becomes a diverging quad. Reading both
	// and mixing costs one extra sample per fragment and has no such case.
	//
	// THE MIX IS AROUND ONE RATHER THAN AROUND ZERO, because what a texture does here is TINT the
	// material: an untextured surface must come out of this with the colour it went in with, and a
	// mix that faded to black would darken every one of them by the weight it did not carry.
	let uv = builder.load(Type::vec(4), Binding::Varying { location: 3 });
	let weights = builder.load(Type::vec(4), Binding::Varying { location: 4 });
	let checker = builder.assign(Type::vec(4), Op::Sample { binding: Binding::Texture { texture: TEX_CHECKER, sampler: SAMP_CLAMP }, coordinate: uv });
	let ground = builder.assign(Type::vec(4), Op::Sample { binding: Binding::Texture { texture: TEX_GROUND, sampler: SAMP_REPEAT }, coordinate: uv });
	let unit = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![one, one, one, one]));
	let w_checker = builder.assign(Type::f32(), Op::Extract(weights, 0));
	let w_ground = builder.assign(Type::f32(), Op::Extract(weights, 1));
	let w_checker = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![w_checker, w_checker, w_checker, w_checker]));
	let w_ground = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![w_ground, w_ground, w_ground, w_ground]));
	let checker_delta = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Subtract, checker, unit));
	let ground_delta = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Subtract, ground, unit));
	let checker_term = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Multiply, checker_delta, w_checker));
	let ground_term = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Multiply, ground_delta, w_ground));
	let tint = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Add, unit, checker_term));
	let tint = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Add, tint, ground_term));
	let base = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Multiply, material, tint));

	// INTERPOLATION DOES NOT PRESERVE LENGTH, so the normal is renormalised per fragment. A
	// renderer that skipped this lights a curved surface with a normal that is short in the middle
	// of every triangle, which reads as a dark band.
	let n = builder.assign(Type::vec(4), Op::Unary(UnaryOp::Normalize, normal_in));
	let l = builder.assign(Type::vec(4), Op::Unary(UnaryOp::Normalize, light_dir));
	let ndotl = builder.assign(Type::f32(), Op::Binary(BinaryOp::Dot, n, l));
	let diffuse = builder.assign(Type::f32(), Op::Binary(BinaryOp::Max, ndotl, zero));

	// The point light, whose direction is a difference of two POSITIONS, so its `w` is zero by
	// construction and the normalisation is over the direction alone.
	let to_point = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Subtract, light_point, world));
	let pl = builder.assign(Type::vec(4), Op::Unary(UnaryOp::Normalize, to_point));
	let ndotp = builder.assign(Type::f32(), Op::Binary(BinaryOp::Dot, n, pl));
	let point_raw = builder.assign(Type::f32(), Op::Binary(BinaryOp::Max, ndotp, zero));
	let point = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, point_raw, point_strength));

	// The specular highlight, on the half vector between the eye and the light - which is what makes
	// the highlight move when the camera does.
	let to_eye = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Subtract, eye, world));
	let v = builder.assign(Type::vec(4), Op::Unary(UnaryOp::Normalize, to_eye));
	let half_raw = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Add, v, l));
	let h = builder.assign(Type::vec(4), Op::Unary(UnaryOp::Normalize, half_raw));
	let ndoth = builder.assign(Type::f32(), Op::Binary(BinaryOp::Dot, n, h));
	let ndoth_clamped = builder.assign(Type::f32(), Op::Binary(BinaryOp::Max, ndoth, zero));
	let spec_raw = builder.assign(Type::f32(), Op::Transcendental(Transcendental::Pow, ndoth_clamped, Some(shininess)));
	let spec = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, spec_raw, spec_strength));

	let lit_scalar = builder.assign(Type::f32(), Op::Binary(BinaryOp::Add, diffuse, point));
	let lit = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![lit_scalar, lit_scalar, lit_scalar, zero]));
	let shade = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Add, ambient, lit));
	let shaded = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Multiply, base, shade));
	let highlight = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![spec, spec, spec, zero]));
	let rgb = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Add, shaded, highlight));
	// THE ALPHA IS THE MATERIAL'S AND NOT THE LIGHT'S. Lighting changes how bright a surface is and
	// never how transparent it is, and a shader that let it through would make the panel in front
	// fade as the light moved.
	let alpha = builder.assign(Type::f32(), Op::Extract(material, 3));
	let r = builder.assign(Type::f32(), Op::Extract(rgb, 0));
	let g = builder.assign(Type::f32(), Op::Extract(rgb, 1));
	let b = builder.assign(Type::f32(), Op::Extract(rgb, 2));
	let r = builder.assign(Type::f32(), Op::Clamp { value: r, low: zero, high: one });
	let g = builder.assign(Type::f32(), Op::Clamp { value: g, low: zero, high: one });
	let b = builder.assign(Type::f32(), Op::Clamp { value: b, low: zero, high: one });
	let out = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![r, g, b, alpha]));
	builder.store(Output::Colour(ATTACH_COLOUR), out);
	// THE IDENTITY IS WRITTEN BY THE SAME FRAGMENT THAT WROTE THE COLOUR, which is the whole reason
	// picking belongs in the pipeline rather than beside it: it passes the same depth test, is
	// discarded by the same rules, and therefore answers for the surface a person can actually see.
	let ident = builder.load(Type::vec(4), Binding::Varying { location: 5 });
	let ident_f = builder.assign(Type::f32(), Op::Extract(ident, 0));
	let ident_u = builder.assign(Type::Scalar(render_shader::ir::ScalarType::U32), Op::Unary(UnaryOp::Convert(render_shader::ir::ScalarType::U32), ident_f));
	builder.store(Output::Integer(ATTACH_IDENT), ident_u);
	builder.finish()
}

// -------------------------------------------------------------------------------------------------
// The Extended phase's four stages.
// -------------------------------------------------------------------------------------------------

/// A direction out of a point or a tangent: the same three numbers with `w` at zero.
///
/// WITHOUT IT A MODEL MATRIX TRANSLATES A DIRECTION. A tangent stored with `w = 1` multiplied by a
/// model matrix comes out displaced by the object's position, which is a vector that points at the
/// origin from wherever the object happens to be - and the error is invisible at the origin, which
/// is exactly where a demo is first looked at.
fn direction(builder: &mut Builder, value: render_shader::ir::Value, zero: render_shader::ir::Value) -> render_shader::ir::Value {
	let x = builder.assign(Type::f32(), Op::Extract(value, 0));
	let y = builder.assign(Type::f32(), Op::Extract(value, 1));
	let z = builder.assign(Type::f32(), Op::Extract(value, 2));
	builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![x, y, z, zero]))
}

/// One scalar as a vec4, for the component-wise multiplies the IR has no scalar form of.
fn splat(builder: &mut Builder, scalar: render_shader::ir::Value) -> render_shader::ir::Value {
	builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![scalar, scalar, scalar, scalar]))
}

/// A cross product between two directions carried as vec4s.
///
/// THE IR'S `Cross` IS THREE-COMPONENT AND REFUSES ANYTHING ELSE, which is right: a cross product of
/// four-vectors is not defined, and an operation that quietly ignored the fourth component would
/// make a shader that passes a homogeneous point look like it worked. So the two are narrowed, the
/// product is taken, and the result comes back as a direction with `w` at zero.
fn cross(builder: &mut Builder, left: render_shader::ir::Value, right: render_shader::ir::Value, zero: render_shader::ir::Value) -> render_shader::ir::Value {
	let narrow = |builder: &mut Builder, value: render_shader::ir::Value| {
		let x = builder.assign(Type::f32(), Op::Extract(value, 0));
		let y = builder.assign(Type::f32(), Op::Extract(value, 1));
		let z = builder.assign(Type::f32(), Op::Extract(value, 2));
		builder.assign(Type::vec(3), Op::Compose(Type::vec(3), vec![x, y, z]))
	};
	let left = narrow(builder, left);
	let right = narrow(builder, right);
	let product = builder.assign(Type::vec(3), Op::Binary(BinaryOp::Cross, left, right));
	let x = builder.assign(Type::f32(), Op::Extract(product, 0));
	let y = builder.assign(Type::f32(), Op::Extract(product, 1));
	let z = builder.assign(Type::f32(), Op::Extract(product, 2));
	builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![x, y, z, zero]))
}

/// The shadow pass's vertex stage: the scene through the LIGHT's view-projection.
fn shadow_vertex_stage() -> Module {
	let mut builder = Builder::new(Stage::Vertex, "shadow-vertex");
	builder.varying(0, Type::vec(4), render_shader::Interpolation::Smooth);
	let position = builder.load(Type::vec(4), Binding::Attribute { location: 0 });
	let model = builder.load(Type::Matrix(4), Binding::Uniform { block: 0, member: U_MODEL });
	let light_vp = builder.load(Type::Matrix(4), Binding::Uniform { block: 0, member: U_LIGHT_VP });
	let world = builder.assign(Type::vec(4), Op::Binary(BinaryOp::MatrixProduct, model, position));
	let clip = builder.assign(Type::vec(4), Op::Binary(BinaryOp::MatrixProduct, light_vp, world));
	builder.store(Output::Position, clip);
	// THE CLIP POSITION IS ALSO A VARYING, because what this pass stores is the DEPTH and the
	// fragment stage has no way to ask the rasteriser for it.
	builder.store(Output::Varying(0), clip);
	builder.finish()
}

/// The shadow pass's fragment stage: light-space depth, written as a colour.
///
/// INTO A COLOUR ATTACHMENT AND NOT LEFT IN THE DEPTH BUFFER, because this profile has no
/// depth-texture binding and no comparison sampler: the lighting pass reads this map as ordinary
/// texels, so the depth has to BE texels.
fn shadow_fragment_stage() -> Module {
	let mut builder = Builder::new(Stage::Fragment, "shadow-fragment");
	builder.varying(0, Type::vec(4), render_shader::Interpolation::Smooth);
	let clip = builder.load(Type::vec(4), Binding::Varying { location: 0 });
	let z = builder.assign(Type::f32(), Op::Extract(clip, 2));
	let w = builder.assign(Type::f32(), Op::Extract(clip, 3));
	let depth = builder.assign(Type::f32(), Op::Binary(BinaryOp::Divide, z, w));
	let one = builder.constant(Constant::F32(1.0));
	let out = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![depth, depth, depth, one]));
	builder.store(Output::Colour(0), out);
	builder.finish()
}

/// The Extended lighting pass's vertex stage.
fn pbr_vertex_stage() -> Module {
	let mut builder = Builder::new(Stage::Vertex, "pbr-vertex");
	for location in [0, 1, 2, 3, 4] {
		builder.varying(location, Type::vec(4), render_shader::Interpolation::Smooth);
	}
	builder.varying_at(5, Type::vec(4), render_shader::Interpolation::Flat, render_shader::ir::Sampling::Pixel);
	builder.varying(6, Type::vec(4), render_shader::Interpolation::Smooth);
	let position = builder.load(Type::vec(4), Binding::Attribute { location: 0 });
	let normal = builder.load(Type::vec(4), Binding::Attribute { location: 1 });
	let colour = builder.load(Type::vec(4), Binding::Attribute { location: 2 });
	let uv = builder.load(Type::vec(4), Binding::Attribute { location: 3 });
	let ident = builder.load(Type::vec(4), Binding::Attribute { location: 5 });
	let tangent = builder.load(Type::vec(4), Binding::Attribute { location: 6 });
	let mvp = builder.load(Type::Matrix(4), Binding::Uniform { block: 0, member: U_MVP });
	let model = builder.load(Type::Matrix(4), Binding::Uniform { block: 0, member: U_MODEL });
	let light_vp = builder.load(Type::Matrix(4), Binding::Uniform { block: 0, member: U_LIGHT_VP });
	let zero = builder.constant(Constant::F32(0.0));
	let clip = builder.assign(Type::vec(4), Op::Binary(BinaryOp::MatrixProduct, mvp, position));
	let world = builder.assign(Type::vec(4), Op::Binary(BinaryOp::MatrixProduct, model, position));
	let world_normal = builder.assign(Type::vec(4), Op::Binary(BinaryOp::MatrixProduct, model, normal));
	let tangent_direction = direction(&mut builder, tangent, zero);
	let world_tangent = builder.assign(Type::vec(4), Op::Binary(BinaryOp::MatrixProduct, model, tangent_direction));
	// THE SAME WORLD POSITION THROUGH THE LIGHT'S MATRIX, which is what makes the lookup agree with
	// what the shadow pass wrote: both are the light's view-projection times the world position, and
	// a lookup computed from anything else would be comparing two different points.
	let light_clip = builder.assign(Type::vec(4), Op::Binary(BinaryOp::MatrixProduct, light_vp, world));
	builder.store(Output::Position, clip);
	builder.store(Output::Varying(0), colour);
	builder.store(Output::Varying(1), world_normal);
	builder.store(Output::Varying(2), world);
	builder.store(Output::Varying(3), uv);
	builder.store(Output::Varying(4), world_tangent);
	builder.store(Output::Varying(5), ident);
	builder.store(Output::Varying(6), light_clip);
	builder.finish()
}

/// The Extended lighting pass's fragment stage: `PbrMetallicRoughness` over one directional light,
/// with a tangent-space normal map and a shadow lookup.
///
/// THE THREE TERMS ARE THE PROFILE'S, WRITTEN OUT. GGX / Trowbridge-Reitz for the distribution,
/// Smith height-correlated for the visibility - which CARRIES the `1 / (4 NoL NoV)` denominator, so
/// the specular is `D * V * F` and not `D * G * F / (4 ...)` - and Schlick for the Fresnel, its
/// fifth power by multiplication rather than by a transcendental. A demo that wrote a different
/// Smith term from the one `scene3d` fixtures would look right and prove nothing.
fn pbr_fragment_stage() -> Module {
	let mut builder = Builder::new(Stage::Fragment, "pbr-fragment");
	for location in [0, 1, 2, 3, 4] {
		builder.varying(location, Type::vec(4), render_shader::Interpolation::Smooth);
	}
	builder.varying_at(5, Type::vec(4), render_shader::Interpolation::Flat, render_shader::ir::Sampling::Pixel);
	builder.varying(6, Type::vec(4), render_shader::Interpolation::Smooth);

	let base = builder.load(Type::vec(4), Binding::Varying { location: 0 });
	let normal_in = builder.load(Type::vec(4), Binding::Varying { location: 1 });
	let world = builder.load(Type::vec(4), Binding::Varying { location: 2 });
	let uv = builder.load(Type::vec(4), Binding::Varying { location: 3 });
	let tangent_in = builder.load(Type::vec(4), Binding::Varying { location: 4 });
	let light_clip = builder.load(Type::vec(4), Binding::Varying { location: 6 });
	let light_dir = builder.load(Type::vec(4), Binding::Uniform { block: 0, member: U_LIGHT_DIR });
	let eye = builder.load(Type::vec(4), Binding::Uniform { block: 0, member: U_EYE });
	let ambient = builder.load(Type::vec(4), Binding::Uniform { block: 0, member: U_AMBIENT });

	let zero = builder.constant(Constant::F32(0.0));
	let one = builder.constant(Constant::F32(1.0));
	let half = builder.constant(Constant::F32(0.5));
	let two = builder.constant(Constant::F32(2.0));
	let minus_one = builder.constant(Constant::F32(-1.0));
	let pi = builder.constant(Constant::F32(core::f32::consts::PI));
	let epsilon = builder.constant(Constant::F32(1.0e-4));
	let roughness = builder.constant(Constant::F32(PBR_ROUGHNESS));
	let f0 = builder.constant(Constant::F32(PBR_F0));
	let bias = builder.constant(Constant::F32(SHADOW_BIAS));

	// THE TANGENT FRAME, RE-ORTHOGONALISED HERE. Interpolating two unit vectors across a triangle
	// gives two that are neither unit nor perpendicular, and a frame that is not orthonormal tilts
	// every mapped normal by an amount that varies across the surface - which reads as a wobble in
	// the lighting rather than as a broken frame.
	let n = builder.assign(Type::vec(4), Op::Unary(UnaryOp::Normalize, normal_in));
	let t_raw = builder.assign(Type::vec(4), Op::Unary(UnaryOp::Normalize, tangent_in));
	let t_dot_n = builder.assign(Type::f32(), Op::Binary(BinaryOp::Dot, t_raw, n));
	let t_dot_n_v = splat(&mut builder, t_dot_n);
	let projected = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Multiply, n, t_dot_n_v));
	let t_ortho = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Subtract, t_raw, projected));
	let t = builder.assign(Type::vec(4), Op::Unary(UnaryOp::Normalize, t_ortho));
	let b = cross(&mut builder, n, t, zero);

	// THE MAP, DECODED FROM `0..=1` BACK TO A DIRECTION.
	let texel = builder.assign(Type::vec(4), Op::Sample { binding: Binding::Texture { texture: TEX_NORMAL, sampler: SAMP_REPEAT }, coordinate: uv });
	let two_v = splat(&mut builder, two);
	let one_v = splat(&mut builder, one);
	let scaled = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Multiply, texel, two_v));
	let centred = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Subtract, scaled, one_v));
	let map_x = builder.assign(Type::f32(), Op::Extract(centred, 0));
	let map_y = builder.assign(Type::f32(), Op::Extract(centred, 1));
	let map_z = builder.assign(Type::f32(), Op::Extract(centred, 2));
	let map_x_v = splat(&mut builder, map_x);
	let map_y_v = splat(&mut builder, map_y);
	let map_z_v = splat(&mut builder, map_z);
	let along_t = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Multiply, t, map_x_v));
	let along_b = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Multiply, b, map_y_v));
	let along_n = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Multiply, n, map_z_v));
	let mapped_raw = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Add, along_t, along_b));
	let mapped_raw = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Add, mapped_raw, along_n));
	let nm = builder.assign(Type::vec(4), Op::Unary(UnaryOp::Normalize, mapped_raw));

	// THE THREE DIRECTIONS THE TERMS ARE FUNCTIONS OF.
	let l = builder.assign(Type::vec(4), Op::Unary(UnaryOp::Normalize, light_dir));
	let to_eye = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Subtract, eye, world));
	let v = builder.assign(Type::vec(4), Op::Unary(UnaryOp::Normalize, to_eye));
	let h_raw = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Add, l, v));
	let h = builder.assign(Type::vec(4), Op::Unary(UnaryOp::Normalize, h_raw));
	let n_dot_l_raw = builder.assign(Type::f32(), Op::Binary(BinaryOp::Dot, nm, l));
	let n_dot_l = builder.assign(Type::f32(), Op::Binary(BinaryOp::Max, n_dot_l_raw, zero));
	let n_dot_v_raw = builder.assign(Type::f32(), Op::Binary(BinaryOp::Dot, nm, v));
	let n_dot_v = builder.assign(Type::f32(), Op::Binary(BinaryOp::Max, n_dot_v_raw, epsilon));
	let n_dot_h_raw = builder.assign(Type::f32(), Op::Binary(BinaryOp::Dot, nm, h));
	let n_dot_h = builder.assign(Type::f32(), Op::Binary(BinaryOp::Max, n_dot_h_raw, zero));
	let v_dot_h_raw = builder.assign(Type::f32(), Op::Binary(BinaryOp::Dot, v, h));
	let v_dot_h = builder.assign(Type::f32(), Op::Clamp { value: v_dot_h_raw, low: zero, high: one });

	// `a = roughness^2`, and `a2` with it.
	let a = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, roughness, roughness));
	let a2 = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, a, a));

	// D: `a2 / (pi * (NoH^2 (a2 - 1) + 1)^2)`.
	let n_dot_h2 = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, n_dot_h, n_dot_h));
	let a2_minus_one = builder.assign(Type::f32(), Op::Binary(BinaryOp::Subtract, a2, one));
	let inner = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, n_dot_h2, a2_minus_one));
	let inner = builder.assign(Type::f32(), Op::Binary(BinaryOp::Add, inner, one));
	let inner2 = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, inner, inner));
	let denominator = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, pi, inner2));
	let denominator = builder.assign(Type::f32(), Op::Binary(BinaryOp::Max, denominator, epsilon));
	let d_term = builder.assign(Type::f32(), Op::Binary(BinaryOp::Divide, a2, denominator));

	// V: Smith, height-correlated, WITH the `1 / (4 NoL NoV)` denominator folded in.
	let one_minus_a2 = builder.assign(Type::f32(), Op::Binary(BinaryOp::Subtract, one, a2));
	let n_dot_v2 = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, n_dot_v, n_dot_v));
	let from_view_inner = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, n_dot_v2, one_minus_a2));
	let from_view_inner = builder.assign(Type::f32(), Op::Binary(BinaryOp::Add, from_view_inner, a2));
	let from_view_root = builder.assign(Type::f32(), Op::Transcendental(Transcendental::Sqrt, from_view_inner, None));
	let from_view = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, n_dot_l, from_view_root));
	let n_dot_l2 = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, n_dot_l, n_dot_l));
	let from_light_inner = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, n_dot_l2, one_minus_a2));
	let from_light_inner = builder.assign(Type::f32(), Op::Binary(BinaryOp::Add, from_light_inner, a2));
	let from_light_root = builder.assign(Type::f32(), Op::Transcendental(Transcendental::Sqrt, from_light_inner, None));
	let from_light = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, n_dot_v, from_light_root));
	let total = builder.assign(Type::f32(), Op::Binary(BinaryOp::Add, from_view, from_light));
	let total = builder.assign(Type::f32(), Op::Binary(BinaryOp::Max, total, epsilon));
	let v_term = builder.assign(Type::f32(), Op::Binary(BinaryOp::Divide, half, total));

	// F: Schlick, the fifth power BY MULTIPLICATION.
	let one_minus_voh = builder.assign(Type::f32(), Op::Binary(BinaryOp::Subtract, one, v_dot_h));
	let squared = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, one_minus_voh, one_minus_voh));
	let fourth = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, squared, squared));
	let fifth = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, fourth, one_minus_voh));
	let one_minus_f0 = builder.assign(Type::f32(), Op::Binary(BinaryOp::Subtract, one, f0));
	let f_rest = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, one_minus_f0, fifth));
	let f_term = builder.assign(Type::f32(), Op::Binary(BinaryOp::Add, f0, f_rest));

	// THE SHADOW LOOKUP. The light-space position divided through by `w`, mapped from `-1..=1` to
	// the texture's `0..=1`, and compared against what the light wrote there.
	let light_x = builder.assign(Type::f32(), Op::Extract(light_clip, 0));
	let light_y = builder.assign(Type::f32(), Op::Extract(light_clip, 1));
	let light_z = builder.assign(Type::f32(), Op::Extract(light_clip, 2));
	let light_w = builder.assign(Type::f32(), Op::Extract(light_clip, 3));
	let light_w = builder.assign(Type::f32(), Op::Binary(BinaryOp::Max, light_w, epsilon));
	let ndc_x = builder.assign(Type::f32(), Op::Binary(BinaryOp::Divide, light_x, light_w));
	let ndc_y = builder.assign(Type::f32(), Op::Binary(BinaryOp::Divide, light_y, light_w));
	let ndc_z = builder.assign(Type::f32(), Op::Binary(BinaryOp::Divide, light_z, light_w));
	let map_u = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, ndc_x, half));
	let map_u = builder.assign(Type::f32(), Op::Binary(BinaryOp::Add, map_u, half));
	// THE `v` AXIS IS FLIPPED, because clip space climbs upward and a texture's rows run downward.
	// A map sampled without the flip shadows the mirror image of the scene, which looks like an
	// offset rather than like an inversion and is chased in the bias for hours.
	let map_v = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, ndc_y, minus_one));
	let map_v = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, map_v, half));
	let map_v = builder.assign(Type::f32(), Op::Binary(BinaryOp::Add, map_v, half));
	let lookup = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![map_u, map_v, zero, zero]));
	let occluder = builder.assign(Type::vec(4), Op::Sample { binding: Binding::Texture { texture: TEX_SHADOW, sampler: SAMP_SHADOW }, coordinate: lookup });
	let occluder_depth = builder.assign(Type::f32(), Op::Extract(occluder, 0));
	let biased = builder.assign(Type::f32(), Op::Binary(BinaryOp::Subtract, ndc_z, bias));
	let occluded = builder.assign(Type::Scalar(render_shader::ir::ScalarType::Bool), Op::Compare(render_shader::ir::CompareKind::Greater, biased, occluder_depth));
	let visibility = builder.assign(Type::f32(), Op::Select { condition: occluded, on_true: zero, on_false: one });

	// THE SUM. Diffuse is `(1 - F) * base / pi` - the energy the specular lobe did not take - and
	// the whole direct term is scaled by `NoL` and by the visibility. AMBIENT IS NOT SHADOWED,
	// because a shadow is the absence of the DIRECT light and a surface in shadow is still lit by
	// its surroundings; shadowing the ambient term too makes every shadow a hole.
	let intensity = builder.constant(Constant::F32(PBR_LIGHT));
	let one_minus_f = builder.assign(Type::f32(), Op::Binary(BinaryOp::Subtract, one, f_term));
	let diffuse_scale = builder.assign(Type::f32(), Op::Binary(BinaryOp::Divide, one_minus_f, pi));
	let diffuse_scale = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, diffuse_scale, n_dot_l));
	let diffuse_scale = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, diffuse_scale, visibility));
	let diffuse_scale = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, diffuse_scale, intensity));
	let diffuse_scale_v = splat(&mut builder, diffuse_scale);
	let diffuse = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Multiply, base, diffuse_scale_v));
	let specular = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, d_term, v_term));
	let specular = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, specular, f_term));
	let specular = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, specular, n_dot_l));
	let specular = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, specular, visibility));
	let specular = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, specular, intensity));
	let specular_v = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![specular, specular, specular, zero]));
	let ambient_term = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Multiply, base, ambient));
	let lit = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Add, diffuse, specular_v));
	let lit = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Add, lit, ambient_term));

	// THE SCENE IS WRITTEN CLAMPED, because this attachment is what a person sees and there is no
	// tone map between here and the surface. The HDR path's own answer is the `postprocess` chain in
	// `scene3d`, which is a pass graph rather than a demo.
	let alpha = builder.assign(Type::f32(), Op::Extract(base, 3));
	let r = builder.assign(Type::f32(), Op::Extract(lit, 0));
	let g = builder.assign(Type::f32(), Op::Extract(lit, 1));
	let bb = builder.assign(Type::f32(), Op::Extract(lit, 2));
	let r = builder.assign(Type::f32(), Op::Clamp { value: r, low: zero, high: one });
	let g = builder.assign(Type::f32(), Op::Clamp { value: g, low: zero, high: one });
	let bb = builder.assign(Type::f32(), Op::Clamp { value: bb, low: zero, high: one });
	let out = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![r, g, bb, alpha]));
	builder.store(Output::Colour(ATTACH_COLOUR), out);
	let ident = builder.load(Type::vec(4), Binding::Varying { location: 5 });
	let ident_f = builder.assign(Type::f32(), Op::Extract(ident, 0));
	let ident_u = builder.assign(Type::Scalar(render_shader::ir::ScalarType::U32), Op::Unary(UnaryOp::Convert(render_shader::ir::ScalarType::U32), ident_f));
	builder.store(Output::Integer(ATTACH_IDENT), ident_u);
	builder.finish()
}

// What the run was asked for.
struct Controls {
	width: u32,
	height: u32,
	frames: u32,
	input: bool,
	fixed: bool,
	/// Whether to print the per-stage timing report when the run ends.
	report: bool,
	/// The scene's own extent, or zero for "follow the surface".
	scene_width: u32,
	scene_height: u32,
	/// A fixed rotation, in hundredths of a radian, or `u32::MAX` for the animated scene. THE
	/// DETERMINISTIC POSE: a check that knows which face is visible needs the scene to be in a stated
	/// position, and "capture at frame N" is not one - a slow machine and a fast one reach different
	/// rotations in the same wall time.
	pose: u32,
	/// Draw `Scene3D Extended Profile 1`'s scene INSTEAD OF the core one: a physically based sphere
	/// with a normal map, and a cast shadow.
	///
	/// INSTEAD OF AND NOT BESIDE, which is what keeps this an optional part. The core demo is a core
	/// gate and its scene is what that gate asserts about; a phase that added to it would make every
	/// core check depend on an optional profile being implemented.
	extended: bool,
}

impl Controls {
	fn parse(arguments: &[u8]) -> Controls {
		let mut controls = Controls { width: 800, height: 600, frames: 0, input: true, fixed: false, report: false, pose: u32::MAX, scene_width: 0, scene_height: 0, extended: false };
		let mut words = arguments.split(|byte| *byte == b' ').filter(|word| !word.is_empty());
		while let Some(word) = words.next() {
			match word {
				b"--no-input" => controls.input = false,
				// THE DETERMINISTIC MODE, WHICH IS NOT IN THE NORMAL UI. A fixed step per frame, a
				// fixed camera and a fixed light: what a test compares has to be the same picture on
				// a machine that renders slowly and on one that does not.
				b"--fixed" => controls.fixed = true,
				b"--width" => controls.width = words.next().and_then(number).unwrap_or(controls.width),
				b"--height" => controls.height = words.next().and_then(number).unwrap_or(controls.height),
				b"--frames" => controls.frames = words.next().and_then(number).unwrap_or(0),
				b"--report" => controls.report = true,
				b"--extended" => controls.extended = true,
				b"--scene-width" => controls.scene_width = words.next().and_then(number).unwrap_or(0),
				b"--scene-height" => controls.scene_height = words.next().and_then(number).unwrap_or(0),
				b"--pose" => controls.pose = words.next().and_then(number).unwrap_or(u32::MAX),
				_ => {}
			}
		}
		controls
	}

	/// What extent the scene renders at, given what the surface currently is.
	///
	/// TWO EXTENT TYPES MEET HERE and they are not the same type: the surface protocol's own and the
	/// image model's. This takes a pair of numbers so neither has to be converted for the other.
	fn scene_extent(&self, width: u32, height: u32) -> (u32, u32) {
		let width = if self.scene_width == 0 { width } else { self.scene_width };
		let height = if self.scene_height == 0 { height } else { self.scene_height };
		(width.clamp(SCENE_MIN, SCENE_MAX), height.clamp(SCENE_MIN, SCENE_MAX))
	}
}

fn number(word: &[u8]) -> Option<u32> {
	let mut value: u32 = 0;
	for byte in word {
		let digit = byte.checked_sub(b'0').filter(|digit| *digit < 10)?;
		value = value.checked_mul(10)?.checked_add(digit as u32)?;
	}
	Some(value)
}

// Where the camera is, and what the keys have done to it.
struct View {
	yaw: f32,
	pitch: f32,
	distance: f32,
	spin: f32,
	paused: bool,
	/// Whether the identity attachment is shown instead of the colour one.
	picking: bool,
}

impl View {
	fn reset() -> View {
		View { yaw: 0.6, pitch: 0.42, distance: DISTANCE_START, spin: 0.0, paused: false, picking: false }
	}

	fn eye(&self) -> Vec3 {
		let (sy, cy) = (sin(self.yaw), cos(self.yaw));
		let (sp, cp) = (sin(self.pitch), cos(self.pitch));
		Vec3::new(self.distance * cp * sy, self.distance * sp, self.distance * cp * cy)
	}
}

// Sine and cosine by their own series, because this program links no libm and needs only these two.
// SEVEN TERMS OVER A REDUCED ARGUMENT, which is accurate to about a part in ten million over the
// range a camera angle takes - far finer than a pixel.
fn sin(x: f32) -> f32 {
	let x = wrap(x);
	let mut term = x;
	let mut total = x;
	let square = x * x;
	let mut n = 1.0_f32;
	for _ in 0..7 {
		term *= -square / ((2.0 * n) * (2.0 * n + 1.0));
		total += term;
		n += 1.0;
	}
	total
}

fn cos(x: f32) -> f32 {
	sin(x + core::f32::consts::FRAC_PI_2)
}

// Reduce to `-PI ..= PI`, where the series converges quickly.
fn wrap(x: f32) -> f32 {
	let two_pi = core::f32::consts::PI * 2.0;
	let mut value = x;
	while value > core::f32::consts::PI {
		value -= two_pi;
	}
	while value < -core::f32::consts::PI {
		value += two_pi;
	}
	value
}

/// EVERYTHING THAT DEPENDS ON THE SCENE'S EXTENT, in one value that is built in one place and
/// rebuilt in one place.
///
/// A RESIZE RECREATES EXACTLY THIS. The two prepared plans reserve for an extent, the colour and
/// identity attachments are that extent, the depth buffer is that extent, and the image the 2D half
/// composites over is that extent. What a resize must NOT touch is the camera, the spin, the pause
/// and the overlay toggle - which is why those live in `View` and none of them is here.
struct Targets {
	extent: (u32, u32),
	scene: Prepared,
	panel: Prepared,
	/// The Extended phase's lighting plan, prepared beside the core ones. PREPARED WHETHER OR NOT
	/// THE PHASE RUNS, because what a resize has to rebuild is every plan that depends on the extent
	/// and a plan built lazily on the first Extended frame would be built at a size that has already
	/// changed.
	pbr: Prepared,
	/// The shadow plan, which is the one thing here that does NOT follow the window: its extent is
	/// the map's.
	shadow: Prepared,
	colour: [Colour; 2],
	depth: DepthStencil,
	/// What the light writes: one colour attachment holding light-space depth, and a depth buffer of
	/// its own so the nearest caster is the one recorded.
	shadow_colour: [Colour; 1],
	shadow_depth: DepthStencil,
	image: OwnedImage,
}

impl Targets {
	fn new(extent: (u32, u32), draw: &Draw, panel_draw: &Draw, extended_draw: &Draw) -> Option<Targets> {
		let (width, height) = extent;
		let scene = soft3d::frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![scene_pipeline()], vec![*draw], width, height).ok()?;
		let panel = soft3d::frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![panel_pipeline()], vec![*panel_draw], width, height).ok()?;
		let pbr = soft3d::frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pbr_pipeline()], vec![*extended_draw], width, height).ok()?;
		let shadow = soft3d::frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![shadow_pipeline()], vec![*extended_draw], SHADOW_EXTENT, SHADOW_EXTENT).ok()?;
		// STRAIGHT ALPHA AND sRGB, which is what the surface this ends up on holds. A scene image in
		// a different space would be converted twice - once here and once at the composite - and
		// neither conversion would be wrong on its own.
		let semantics = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Straight };
		// THE PITCH IS IN BYTES AND NOT IN BITS: four bytes a pixel, one row at a time, with no
		// padding - which is what makes a row's length a function of the extent rather than a
		// constant somebody copied from an eight-pixel fixture.
		let layout = ImageLayout::new(Extent2D::new(width, height), width * 4, PixelStorage::Known(PixelFormat::R8G8B8A8Unorm), RowOrigin::TopLeft, semantics).ok()?;
		Some(Targets {
			extent,
			scene,
			panel,
			// TWO COLOUR ATTACHMENTS: what a person sees, and what a pick reads. The second is
			// declared INTEGER, which is what stops it being resolved, filtered or blended.
			colour: [Colour::new(width, height, 1, false), Colour::new(width, height, 1, true)],
			depth: DepthStencil::new(width, height, 1, DepthFormat::Depth32F),
			pbr,
			shadow,
			shadow_colour: [Colour::new(SHADOW_EXTENT, SHADOW_EXTENT, 1, false)],
			shadow_depth: DepthStencil::new(SHADOW_EXTENT, SHADOW_EXTENT, 1, DepthFormat::Depth32F),
			image: OwnedImage::new(layout).ok()?,
		})
	}

	/// The viewport the whole scene is drawn into.
	fn viewport(&self) -> Viewport {
		Viewport { x: 0.0, y: 0.0, width: self.extent.0 as f32, height: self.extent.1 as f32, min_depth: 0.0, max_depth: 1.0 }
	}

	/// The light's own viewport, which is the map's extent and not the window's.
	fn shadow_viewport(&self) -> Viewport {
		Viewport { x: 0.0, y: 0.0, width: SHADOW_EXTENT as f32, height: SHADOW_EXTENT as f32, min_depth: 0.0, max_depth: 1.0 }
	}

	fn aspect(&self) -> f32 {
		self.extent.0 as f32 / self.extent.1 as f32
	}

	/// What the targets cost, in bytes, one number per thing that is allocated.
	///
	/// COMPUTED FROM THE EXTENT AND NOT ASKED OF AN ALLOCATOR, which is the only form a reader can
	/// check: a peak the allocator reports is this frame's answer on this machine, and a formula is
	/// what says what a different extent would cost before anybody runs it.
	fn bytes(&self) -> (u64, u64, u64) {
		let pixels = self.extent.0 as u64 * self.extent.1 as u64;
		// A colour attachment holds one `Vec4` per pixel per sample; there are two of them, and this
		// scene is single-sampled. A depth-stencil holds a four-byte depth and a one-byte stencil.
		// The shared image is four bytes a pixel, which is what the surface holds.
		(pixels * 16 * 2, pixels * 5, pixels * 4)
	}
}

/// The opaque pass's pipeline.
fn scene_pipeline() -> Pipeline {
	Pipeline {
		state: render3d::command::PipelineState { topology: Topology::TriangleList, cull: CullMode::Back, depth_test: Some(CompareOp::Less), depth_write: true, samples: 1, per_sample_shading: false },
		vertex: vertex_stage(),
		fragment: fragment_stage(),
		// ONE ENTRY PER ATTACHMENT, and the identity one NEVER blends: an object id is a name,
		// and half of one name plus half of another is a third object that is not in the scene.
		blend: vec![
			render3d::AttachmentBlend { enabled: false, colour: render3d::BlendEquation::REPLACE, alpha: render3d::BlendEquation::REPLACE, write_mask: render3d::ColorWriteMask::ALL },
			render3d::AttachmentBlend { enabled: false, colour: render3d::BlendEquation::REPLACE, alpha: render3d::BlendEquation::REPLACE, write_mask: render3d::ColorWriteMask::ALL },
		],
		stencil: None,
		depth_compare: CompareOp::Less,
		depth_write: true,
		bias: (0.0, 0.0, 0.0),
		alpha_to_coverage: false,
		sample_mask: u32::MAX,
	}
}

/// The Extended lighting pass's pipeline: the same attachments as the core scene's, its own stages.
fn pbr_pipeline() -> Pipeline {
	Pipeline {
		state: render3d::command::PipelineState { topology: Topology::TriangleList, cull: CullMode::Back, depth_test: Some(CompareOp::Less), depth_write: true, samples: 1, per_sample_shading: false },
		vertex: pbr_vertex_stage(),
		fragment: pbr_fragment_stage(),
		blend: vec![
			render3d::AttachmentBlend { enabled: false, colour: render3d::BlendEquation::REPLACE, alpha: render3d::BlendEquation::REPLACE, write_mask: render3d::ColorWriteMask::ALL },
			render3d::AttachmentBlend { enabled: false, colour: render3d::BlendEquation::REPLACE, alpha: render3d::BlendEquation::REPLACE, write_mask: render3d::ColorWriteMask::ALL },
		],
		stencil: None,
		depth_compare: CompareOp::Less,
		depth_write: true,
		bias: (0.0, 0.0, 0.0),
		alpha_to_coverage: false,
		sample_mask: u32::MAX,
	}
}

/// The shadow pass's pipeline.
///
/// THE FRONT FACES ARE CULLED AND NOT THE BACK ONES, which is the one state here that differs from
/// every other pass in this program. A shadow map records where the light STOPS, and recording the
/// far side of a closed object puts the recorded depth behind the surface that is lit - which moves
/// the self-shadowing error to the side facing away from the camera, where nobody is looking.
fn shadow_pipeline() -> Pipeline {
	Pipeline { state: render3d::command::PipelineState { topology: Topology::TriangleList, cull: CullMode::Front, depth_test: Some(CompareOp::Less), depth_write: true, samples: 1, per_sample_shading: false }, vertex: shadow_vertex_stage(), fragment: shadow_fragment_stage(), blend: vec![render3d::AttachmentBlend { enabled: false, colour: render3d::BlendEquation::REPLACE, alpha: render3d::BlendEquation::REPLACE, write_mask: render3d::ColorWriteMask::ALL }], stencil: None, depth_compare: CompareOp::Less, depth_write: true, bias: (0.0, 0.0, 0.0), alpha_to_coverage: false, sample_mask: u32::MAX }
}

/// THE TRANSPARENT PASS IS A SECOND PLAN OVER THE SAME ATTACHMENTS, and the three differences
/// between it and the first are the whole of what "transparency" means here:
///   - the blend is enabled, source-over on straight alpha, so what the panel writes is mixed
///     with what is already there instead of replacing it;
///   - the depth test still RUNS, so the ground and anything nearer still occlude the panel;
///   - and the depth WRITE does not, because a translucent surface that wrote depth would hide
///     everything behind it while still showing it through - which is the single most common
///     way a first transparency pass is wrong.
/// It is drawn after the opaque one and neither attachment is cleared between them.
fn panel_pipeline() -> Pipeline {
	Pipeline {
		state: render3d::command::PipelineState { topology: Topology::TriangleList, cull: CullMode::None, depth_test: Some(CompareOp::Less), depth_write: false, samples: 1, per_sample_shading: false },
		vertex: vertex_stage(),
		fragment: fragment_stage(),
		blend: vec![
			render3d::AttachmentBlend { enabled: true, colour: render3d::BlendEquation { source: render3d::BlendFactor::SrcAlpha, destination: render3d::BlendFactor::OneMinusSrcAlpha, operation: render3d::BlendOp::Add }, alpha: render3d::BlendEquation { source: render3d::BlendFactor::One, destination: render3d::BlendFactor::OneMinusSrcAlpha, operation: render3d::BlendOp::Add }, write_mask: render3d::ColorWriteMask::ALL },
			// AND THE TRANSPARENT SURFACE STILL NAMES ITSELF. A panel a person can see through is a
			// panel a person can click on, so it writes its identity like everything else - which is
			// also why this attachment's blend stays off while the colour's is on.
			render3d::AttachmentBlend { enabled: false, colour: render3d::BlendEquation::REPLACE, alpha: render3d::BlendEquation::REPLACE, write_mask: render3d::ColorWriteMask::ALL },
		],
		stencil: None,
		depth_compare: CompareOp::Less,
		depth_write: false,
		bias: (0.0, 0.0, 0.0),
		alpha_to_coverage: false,
		sample_mask: u32::MAX,
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	// THE STDIO ENDPOINTS COME OFF THE BOOTSTRAP CHANNEL FIRST, and they are not optional to
	// read. A launch sends stdout, any stdin and any stderr, then READY, then the launch
	// context, then one message per granted capability - so a program that skips this reads the
	// stdout endpoint as its launch context and looks for `DISPLAY` among messages that have
	// already gone past. It ran, printed `no display` on a console it had not inherited, and
	// exited, with the grant present and the display service willing.
	inherit_stdout(bootstrap);
	let launch = recv_launch_bytes(bootstrap).unwrap_or_default();
	let arguments = LaunchContext::decode(&launch).map(|context| context.arguments.clone().into_bytes()).unwrap_or_default();
	if arguments.windows(6).any(|window| window == b"--help") {
		print(USAGE);
		exit();
	}
	let controls = Controls::parse(&arguments);
	let display: u64 = recv_tagged(bootstrap, &mut buf, b"DISPLAY").unwrap_or(0);
	if display == 0 {
		print(b"test3d-sw: no display\n");
		exit();
	}
	let input_channel: u64 = if controls.input { recv_tagged(bootstrap, &mut buf, b"INPUT_KEYS").unwrap_or(0) } else { 0 };

	let client = surface::connect(display);
	let Some(Ok(mut frames)) = FrameLoop::open(&client, controls.width, controls.height, 2) else {
		print(b"test3d-sw: no surface\n");
		exit();
	};
	// CTRL+C LEAVES THROUGH THE TEARDOWN, for the reason every full-screen tool here arms it:
	// unarmed it terminates the process where it stands, and the surface and the tty go with it.
	catch_interrupt();
	let mut key_stream = 0u64;
	if input_channel != 0
		&& let Some(Ok(focus)) = frames.surface().input_focus()
	{
		key_stream = surface::subscribe_keys(input_channel, focus).unwrap_or(0);
	}
	print(b"test3d-sw: open\n");

	let mesh = Mesh::scene();
	let panel = Mesh::panel();
	let extended = Mesh::extended();
	let draw = Draw { pipeline: 0, topology: Topology::TriangleList, count: mesh.indices.len() as u32, instances: 1, first_instance: 0, base_vertex: 0, restart: false };
	let extended_draw = Draw { pipeline: 0, topology: Topology::TriangleList, count: extended.indices.len() as u32, instances: 1, first_instance: 0, base_vertex: 0, restart: false };

	let panel_draw = Draw { pipeline: 0, topology: Topology::TriangleList, count: panel.indices.len() as u32, instances: 1, first_instance: 0, base_vertex: 0, restart: false };

	// EVERYTHING THAT DEPENDS ON THE SCENE'S SIZE, BUILT IN ONE PLACE AND REBUILT IN ONE PLACE. The
	// two prepared plans reserve for an extent, the three attachments are that extent, and the image
	// the 2D half composites is that extent - so a resize recreates exactly these and nothing else.
	// The camera, the spin, the pause and the overlay toggle are in `View`, which this does not
	// touch: that is what "preserves camera and animation state" means.
	let configuration = frames.surface().configuration();
	let Some(mut targets) = Targets::new(controls.scene_extent(configuration.physical_extent.width, configuration.physical_extent.height), &draw, &panel_draw, &extended_draw) else {
		print(b"test3d-sw: the pipeline was refused\n");
		exit();
	};
	let Some(mut hud) = Hud::new() else {
		print(b"test3d-sw: no overlay\n");
		exit();
	};
	let mut view = View::reset();
	let mut state = Frame3d { mesh, panel, extended, normal_map: normal_map(TEX_NORMAL, 128, 6, 0.045), shadow_map: empty_shadow_map(TEX_SHADOW, SHADOW_EXTENT), shadowed: Sampler { wrap_u: Wrap::ClampToEdge, wrap_v: Wrap::ClampToEdge, magnify: Filter::Nearest, minify: Filter::Nearest, ..Sampler::NEAREST }, light_vp: Mat4::IDENTITY, pass: Pass::Scene, checker: checkerboard(TEX_CHECKER, 64, 8, [1.0, 1.0, 1.0, 1.0], [0.16, 0.16, 0.20, 1.0]), ground: checkerboard(TEX_GROUND, 32, 2, [1.28, 1.28, 1.36, 1.0], [0.62, 0.62, 0.70, 1.0]), clamped: Sampler { wrap_u: Wrap::ClampToEdge, wrap_v: Wrap::ClampToEdge, magnify: Filter::Linear, minify: Filter::Linear, ..Sampler::NEAREST }, repeated: Sampler { wrap_u: Wrap::Repeat, wrap_v: Wrap::Repeat, magnify: Filter::Linear, minify: Filter::Linear, ..Sampler::NEAREST }, mvp: Mat4::IDENTITY, model: Mat4::IDENTITY, light_dir: [0.45, 0.8, 0.35, 0.0], light_point: [0.0, 2.0, 0.0, 1.0], eye: [0.0, 0.0, DISTANCE_START, 1.0], ambient: [0.22, 0.22, 0.26, 0.0] };
	if controls.extended {
		// THE EXTENDED PHASE'S OWN LIGHT, and it points the other way across `z` on purpose: the
		// shadow falls AWAY from the light, so a light behind the object puts its shadow behind it
		// too - where the object itself hides it. This one stands in front and to the side, which is
		// where a shadow is a thing a person can see.
		state.light_dir = [0.55, 0.72, -0.42, 0.0];
		// AND A LOWER AMBIENT, because ambient is what a shadowed surface is lit by: a scene whose
		// ambient equals its direct light has shadows nobody can find.
		state.ambient = [0.10, 0.11, 0.14, 0.0];
	}
	// Whether the shadow map's coverage has been reported: once a run, so a run says whether the
	// light saw anything at all.
	let mut shadow_reported = false;
	let mut presented: u32 = 0;
	let mut rebuilt: u32 = 0;
	/// How many acquires in a row may answer nothing before the display is taken to have gone.
	///
	/// A NUMBER AND NOT A DEADLINE, because what this counts is TURNS OF A LOOP THAT PACED ITSELF:
	/// every pass through it either parks on an event with a deadline or draws, so a run of this many
	/// refusals with nothing else happening is a service that is not going to answer.
	const ACQUIRE_REFUSALS: u32 = 64;
	let mut refusals: u32 = 0;
	// THE STAGE TIMES, AS SUMS. Each is the whole run's nanoseconds in one stage, divided by the
	// frame count when it is reported - an average over a run rather than one frame's reading, which
	// on a machine with other work on it is the only figure that means anything.
	let mut timing = Timing::default();
	let mut counts = soft3d::frame::Stats::default();
	let run_began = clock_ns();
	let mut running = true;
	while running {
		if interrupted() {
			break;
		}
		if key_stream != 0 {
			running = drain_keys(key_stream, &mut view);
			if !running {
				break;
			}
		}
		// THE SERVICE'S OWN EVENTS, READ BEFORE THE STEP IS ASKED FOR. A loop that never polled would
		// take the same step forever: a configuration change, a visibility change and a close request
		// all arrive here, and `step()` is a function of what has been read.
		frames.poll_events();
		if frames.close_requested() {
			break;
		}
		// A STEP THAT IS NOT `Draw` IS NOT A FRAME, and each of the three says what to wait on.
		// `Rebuild` is the resize: the surface's extent moved, the queue's images are stale, and the
		// camera, the spin and the overlay toggle are NOT - which is the whole of "preserves
		// camera/animation state". Nothing in `View` is touched here.
		match frames.step() {
			Step::Draw => {}
			Step::AwaitCompletion => {
				frames.park(None);
				continue;
			}
			Step::Idle { until } => {
				frames.park(until);
				continue;
			}
			Step::Rebuild => {
				match frames.rebuild() {
					Some(Ok(rebuilt_to)) => {
						rebuilt += 1;
						// THE TARGETS FOLLOW THE SURFACE AND THE VIEW DOES NOT. Everything whose size
						// is the surface's is rebuilt here; the camera, the spin, the pause and the
						// overlay toggle are `View`'s and are untouched, which is what makes a resize
						// continue the animation rather than restart it.
						let wanted = controls.scene_extent(rebuilt_to.extent.width, rebuilt_to.extent.height);
						if wanted != targets.extent {
							let Some(rebuilt_targets) = Targets::new(wanted, &draw, &panel_draw, &extended_draw) else {
								print(b"test3d-sw: the scene could not be rebuilt at the new size\n");
								break;
							};
							targets = rebuilt_targets;
						}
					}
					_ => {
						print(b"test3d-sw: the surface could not be rebuilt\n");
						break;
					}
				}
				continue;
			}
		}
		if controls.pose != u32::MAX {
			view.spin = controls.pose as f32 * 0.01;
		} else if !view.paused {
			view.spin += SPIN_PER_FRAME;
		}
		// The point light circles the scene, so the shading changes even when nothing else does.
		let orbit = if controls.fixed { 0.0 } else { view.spin };
		state.light_point = [2.4 * cos(orbit), 2.2, 2.4 * sin(orbit), 1.0];
		let eye = if controls.fixed { Vec3::new(0.0, 1.6, DISTANCE_START) } else { view.eye() };
		state.eye = [eye.x, eye.y, eye.z, 1.0];
		state.model = rotation_y(view.spin);
		let aspect = targets.aspect();
		let Ok(projection) = camera::perspective_rh_zo(0.9, aspect, 0.1, 60.0) else { break };
		let Ok(look) = camera::look_at_rh(eye, Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0)) else { break };
		state.mvp = projection.mul(&look).mul(&state.model);

		// A DARK BUT NOT FLAT BACKGROUND: a demo on black cannot show that anything was drawn at
		// all, and one on a single colour cannot show that the clear happened.
		targets.colour[ATTACH_COLOUR as usize].fill(Vec4::new(0.05, 0.06, 0.10, 1.0));
		targets.colour[ATTACH_IDENT as usize].fill(Vec4::new(ID_NONE, 0.0, 0.0, 0.0));
		targets.depth.clear(1.0, 0);

		// THE EXTENDED PHASE'S SHADOW PASS, BEFORE ANYTHING IS LIT. It runs first because the
		// lighting pass SAMPLES what it writes, and a map read in the frame that produced it is the
		// only order in which a moving light's shadow is where the light is now.
		if controls.extended {
			let to_light = Vec3::new(state.light_dir[0], state.light_dir[1], state.light_dir[2]);
			// AN ORTHOGRAPHIC PROJECTION, because this is a DIRECTIONAL light: its rays are parallel,
			// and a perspective projection here would converge them on a point the light does not
			// have. The box is fitted to the scene by hand - this scene is a sphere and a ground
			// quad, and `scene3d`'s cascade fit is what a scene that does not know its own extent
			// uses.
			let Ok(light_projection) = camera::orthographic_rh_zo(-SHADOW_HALF_EXTENT, SHADOW_HALF_EXTENT, -SHADOW_HALF_EXTENT, SHADOW_HALF_EXTENT, 0.1, 16.0) else { break };
			let Ok(light_view) = camera::look_at_rh(to_light.scale(6.0), Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0)) else { break };
			state.light_vp = light_projection.mul(&light_view);
			state.pass = Pass::Shadow;
			state.model = Mat4::IDENTITY;
			// THE MAP IS CLEARED TO THE FAR PLANE, which is what "nothing occludes this texel"
			// means: a map cleared to zero would say every texel is occluded at the near plane and
			// the whole scene would be in shadow.
			targets.shadow_colour[0].fill(Vec4::new(1.0, 1.0, 1.0, 1.0));
			targets.shadow_depth.clear(1.0, 0);
			let shadow_viewport = targets.shadow_viewport();
			let mut shadow_attachments = Attachments { colour: &mut targets.shadow_colour, depth_stencil: Some(&mut targets.shadow_depth), viewport: shadow_viewport, scissor: None };
			let stage_began = clock_ns();
			match soft3d::frame::execute(&mut targets.shadow, &mut shadow_attachments, &state) {
				Ok(stats) => {
					counts.primitives += stats.primitives;
					counts.fragments += stats.fragments;
					counts.samples_written += stats.samples_written;
				}
				Err(_) => {
					print(b"test3d-sw: the shadow pass was refused\n");
					break;
				}
			}
			timing.shadow += clock_ns().saturating_sub(stage_began);
			// AND THE MAP BECOMES A TEXTURE. This profile has no comparison sampler and no depth
			// binding, so what the light wrote reaches the lighting pass as texels.
			read_shadow_map(&targets.shadow_colour[0], &mut state.shadow_map);
			if !shadow_reported {
				shadow_reported = true;
				let covered = shadow_coverage(&state.shadow_map);
				let mut line = common_line::Line::new();
				line.push(b"test3d-sw: shadow map holds ");
				line.decimal(covered as u64);
				line.push(b" of ");
				line.decimal((SHADOW_EXTENT * SHADOW_EXTENT) as u64);
				line.push(b" texel(s)\n");
				print(line.as_bytes());
			}
		}

		state.pass = if controls.extended { Pass::Pbr } else { Pass::Scene };
		state.model = if controls.extended { Mat4::IDENTITY } else { state.model };
		state.mvp = projection.mul(&look).mul(&state.model);
		let viewport = targets.viewport();
		let mut attachments = Attachments { colour: &mut targets.colour, depth_stencil: Some(&mut targets.depth), viewport, scissor: None };
		let stage_began = clock_ns();
		let plan = if controls.extended { &mut targets.pbr } else { &mut targets.scene };
		match soft3d::frame::execute(plan, &mut attachments, &state) {
			Ok(stats) => {
				counts.primitives += stats.primitives;
				counts.clipped += stats.clipped;
				counts.culled += stats.culled;
				counts.fragments += stats.fragments;
				counts.samples_written += stats.samples_written;
				counts.discarded += stats.discarded;
			}
			Err(_) => {
				print(b"test3d-sw: the frame was refused\n");
				break;
			}
		}
		timing.scene += clock_ns().saturating_sub(stage_began);
		// THE PANEL DOES NOT TURN WITH THE CUBE, and it is the only thing in the scene that does
		// not. What it is for is to STAND BETWEEN the camera and the object: one that shared the
		// model transform would swing edge-on twice a revolution and spend most of the animation
		// as a one-pixel sliver, which is a transparency test that is invisible for most of its
		// run. Its transform is therefore the camera's alone.
		// THE TRANSPARENT PANEL IS THE CORE SCENE'S, and the Extended phase does not draw it: what
		// stands in front of the sphere there is its own shadow, and a translucent quad over it
		// would hide the one surface this phase exists to show.
		if !controls.extended {
			state.pass = Pass::Panel;
			state.model = Mat4::IDENTITY;
			state.mvp = projection.mul(&look);
			let viewport = targets.viewport();
			let mut attachments = Attachments { colour: &mut targets.colour, depth_stencil: Some(&mut targets.depth), viewport, scissor: None };
			let stage_began = clock_ns();
			match soft3d::frame::execute(&mut targets.panel, &mut attachments, &state) {
				Ok(stats) => {
					counts.primitives += stats.primitives;
					counts.fragments += stats.fragments;
					counts.samples_written += stats.samples_written;
				}
				Err(_) => {
					print(b"test3d-sw: the transparent pass was refused\n");
					break;
				}
			}
			timing.transparent += clock_ns().saturating_sub(stage_began);
		}

		// AND THE FRAME CROSSES INTO THE 2D MODEL HERE. What was a direct write into the mapped
		// surface is now an image the 2D half composites a HUD over - which is the path an
		// application takes and the one nothing else in this tree exercises end to end.
		let stage_began = clock_ns();
		hud.absorb(&mut targets, view.picking);
		timing.handover += clock_ns().saturating_sub(stage_began);
		let Some(frame) = frames.acquire() else {
			// AN ACQUIRE THAT ANSWERS NOTHING IS NOT ALWAYS A REASON TO WAIT. Four of the five answers
			// are policy - `again`, `not visible` and `out of date` each move the pacing and the step
			// machine above deals with them on the next pass - and the fifth is the service being
			// GONE, which looks identical from here and never changes. A loop that treated them all
			// as "ask again" would spin at full speed against a dead display service for as long as
			// the machine is up, holding its images and its terminal.
			refusals += 1;
			if refusals > ACQUIRE_REFUSALS {
				print(b"test3d-sw: the display stopped answering\n");
				break;
			}
			continue;
		};
		refusals = 0;
		let stage_began = clock_ns();
		// THE DESTINATION IS THE SURFACE'S CURRENT EXTENT AND NOT A REMEMBERED ONE, which is what
		// makes a resize need nothing recreated on this side: the scene renders at a fixed size and
		// the composite scales it to whatever the surface is now.
		if !hud.record(&targets, frame.layout.extent, presented, &view) {
			print(b"test3d-sw: the overlay was refused\n");
			frames.abandon(frame);
			break;
		}
		if !hud.present_into(&targets, &frame) {
			print(b"test3d-sw: the composite was refused\n");
			frames.abandon(frame);
			break;
		}
		timing.overlay += clock_ns().saturating_sub(stage_began);
		let stage_began = clock_ns();
		if !frames.present_whole(frame) {
			break;
		}
		timing.present += clock_ns().saturating_sub(stage_began);
		presented += 1;
		if controls.frames != 0 && presented >= controls.frames {
			break;
		}
	}
	let mut line: common_line::Line = common_line::Line::new();
	line.push(b"test3d-sw: presented ");
	line.decimal(presented as u64);
	line.push(b" frame(s) rebuilt=");
	line.decimal(rebuilt as u64);
	line.push(b"\n");
	print(line.as_bytes());
	if controls.report {
		report(&timing, &counts, presented, clock_ns().saturating_sub(run_began), controls.width, controls.height, targets.extent, targets.bytes());
	}
	exit();
}

// Take every key waiting, answering whether the demo should keep running.
fn drain_keys(stream: u64, view: &mut View) -> bool {
	let mut frame = [0u8; 32];
	loop {
		match try_recv_caps(stream, &mut frame) {
			PolledCaps::Message { len, handles } => {
				let mut handles = handles;
				let keep = match input::subscribe_keys_read(&frame[..len], &mut handles) {
					Some(event) if event.pressed => apply(event.code, view),
					_ => true,
				};
				for handle in handles.as_slice() {
					close(*handle);
				}
				if !keep {
					return false;
				}
			}
			PolledCaps::Empty | PolledCaps::Closed => return true,
		}
	}
}

// What one key does. Returns false for the keys that end the demo.
fn apply(code: u16, view: &mut View) -> bool {
	match code {
		usage::ESCAPE | usage::Q => return false,
		usage::SPACE => view.paused = !view.paused,
		usage::R => *view = View::reset(),
		// THE PICKING OVERLAY, which is how a person SEES what an application would read. A pick
		// answers from a buffer nothing draws, so a wrong one is invisible until something acts on
		// it; showing the buffer is what turns that into something a reader can check at a glance.
		usage::P => view.picking = !view.picking,
		usage::LEFT => view.yaw -= 0.12,
		usage::RIGHT => view.yaw += 0.12,
		usage::UP => view.pitch = (view.pitch + 0.08).min(1.4),
		usage::DOWN => view.pitch = (view.pitch - 0.08).max(-1.4),
		usage::MINUS | usage::KEYPAD_MINUS => view.distance = (view.distance + 0.4).min(DISTANCE_MAX),
		usage::PLUS | usage::KEYPAD_PLUS => view.distance = (view.distance - 0.4).max(DISTANCE_MIN),
		_ => {}
	}
	true
}

// A bounded line for the one report this program prints.
mod common_line {
	pub struct Line {
		bytes: [u8; 128],
		len: usize,
	}

	impl Line {
		pub fn new() -> Line {
			Line { bytes: [0; 128], len: 0 }
		}

		pub fn push(&mut self, text: &[u8]) {
			for byte in text {
				if self.len < self.bytes.len() {
					self.bytes[self.len] = *byte;
					self.len += 1;
				}
			}
		}

		pub fn decimal(&mut self, value: u64) {
			let mut digits = [0u8; 20];
			let mut at = digits.len();
			let mut value = value;
			loop {
				at -= 1;
				digits[at] = b'0' + (value % 10) as u8;
				value /= 10;
				if value == 0 {
					break;
				}
			}
			self.push(&digits[at..]);
		}

		pub fn as_bytes(&self) -> &[u8] {
			&self.bytes[..self.len]
		}
	}
}

// THE 2D/3D INTEROP, WHICH IS WHAT AN ACTUAL APPLICATION LOOKS LIKE.
//
// A GAME, AN EDITOR AND A MAP APPLICATION ALL TAKE THIS PATH. None of them presents a rendered scene
// on its own: each draws a scene and then puts something over it - a heads-up display, a toolbar, a
// label on a road - and that second half is 2D. The shared image model is what lets one half hand
// its result to the other, and until one frame actually travels
// `render3d -> soft3d -> OwnedImage -> render2d -> soft2d -> Surface` that model is an untested
// claim about two libraries that have never met.
//
// THE 3D RESULT IS AN IMAGE AND NOT A SPECIAL CASE. It is drawn with `draw_image` like any other
// image a 2D scene composites, which is the whole point: `render2d` needs to know nothing about
// where those pixels came from.
struct Hud {
	canvas: Canvas,
	list: DrawList,
	backend: Soft2d<'static>,
	forms: Forms,
}

/// A colour in the space the surface holds, which is the only one this program names.
fn srgb(red: f32, green: f32, blue: f32, alpha: f32) -> Color {
	Color::new(red, green, blue, alpha, ColorSpace::Srgb)
}

/// The identity the scene image is recorded under in a draw list.
const SCENE_IMAGE: u64 = 1;

/// The glyph id the provider answers with colour layers - the one glyph in the label that carries
/// its own colours rather than the run's paint, which is what an emoji in a line of text is.
const COLOUR_GLYPH: u32 = 99;

impl Hud {
	fn new() -> Option<Hud> {
		Some(Hud { canvas: Canvas::new(), list: DrawList::default(), backend: Soft2d::new(), forms: Forms { height: 12.0 } })
	}

	/// Copy the resolved colour attachment into the shared image. THE ONE PLACE THE TWO MODELS MEET:
	/// after this the 3D result is an ordinary image and nothing downstream knows otherwise.
	fn absorb(&mut self, targets: &mut Targets, overlay: bool) {
		let (width, height) = targets.extent;
		let mut view = targets.image.view_mut();
		for y in 0..height {
			for x in 0..width {
				let texel = if overlay { identity_colour(targets.colour[ATTACH_IDENT as usize].at(x, y, 0).x) } else { targets.colour[ATTACH_COLOUR as usize].at(x, y, 0) };
				graphics_core::pixel::write(&mut view, x, y, graphics_core::pixel::Rgba::new(texel.x, texel.y, texel.z, 1.0));
			}
		}
	}

	/// Record the frame: the scene, then the HUD over it.
	fn record(&mut self, targets: &Targets, extent: Extent2D, presented: u32, view: &View) -> bool {
		let (width, height) = (extent.width as f32, extent.height as f32);
		self.canvas.restart();
		// NEAREST, because what the scale has to preserve is WHICH pixels the rasteriser covered, and
		// a filter would soften exactly the edges a pixel check reads.
		let source = RectF::new(0.0, 0.0, targets.extent.0 as f32, targets.extent.1 as f32);
		if self.canvas.draw_image(ImageRecord { identity: SCENE_IMAGE, layout_generation: 1, content_generation: presented as u64 + 1 }, source, RectF::new(0.0, 0.0, width, height), ImageQuality::Nearest).is_err() {
			return false;
		}
		// THE TRANSLUCENT PANEL THE HUD SITS ON, which is a 2D blend over a 3D result - the one
		// composite that cannot be done in either half alone.
		let panel = RectF::new(width * 0.03, height * 0.03, width * 0.42, height * 0.16);
		let mut rounded = PathBuilder::new();
		if rounded.add_rounded_rect(panel, height * 0.02, height * 0.02).is_err() {
			return false;
		}
		// LIGHTER THAN WHAT IS UNDER IT, which is what makes the translucency VISIBLE rather than
		// merely present. A dark panel over this scene's dark sky is a correct blend that looks
		// exactly like no panel at all, and a check reading those pixels cannot tell the two apart.
		if self.canvas.fill_path(rounded.finish(), Paint::Solid(srgb(0.72, 0.78, 0.94, 0.38)), FillRule::NonZero).is_err() {
			return false;
		}
		// AN ANTIALIASED SHAPE WITH CURVES IN IT, so the coverage the 2D rasteriser computes is
		// visible over the 3D image rather than only against a flat background.
		let mut dot = PathBuilder::new();
		if dot.add_circle(PointF { x: panel.x + height * 0.055, y: panel.y + height * 0.08 }, height * 0.035).is_err() {
			return false;
		}
		let lit = if view.paused { srgb(0.95, 0.45, 0.10, 0.95) } else { srgb(0.10, 0.65, 0.25, 0.95) };
		if self.canvas.fill_path(dot.finish(), Paint::Solid(lit), FillRule::NonZero).is_err() {
			return false;
		}
		// A LINE OF TEXT AS A GLYPH RUN, which is the only form this program can draw one in: it
		// holds no font capability, so it carries the forms itself and `render2d` draws them.
		// THE FORMS ARE SIZED FOR THIS FRAME BEFORE THE RUN IS RECORDED, because the provider is asked
		// for them while the list is replayed and by then the extent is gone.
		let glyph_height = (height * 0.045).max(6.0);
		self.forms.height = glyph_height;
		if self.canvas.draw_glyph_run(label(panel.x + height * 0.10, panel.y + height * 0.115, glyph_height), Paint::Solid(srgb(0.06, 0.08, 0.16, 1.0))).is_err() {
			return false;
		}
		self.canvas.finish_into(&mut self.list).is_ok()
	}

	/// Replay the recorded list into the acquired surface image.
	fn present_into(&mut self, targets: &Targets, frame: &graphics_app::Frame) -> bool {
		let Some(span) = frame.layout.backend_access_span(true) else { return false };
		// SAFETY: the mapping is live while the frame is held, and the span is the layout's own
		// answer for what a backend may touch.
		let bytes = unsafe { core::slice::from_raw_parts_mut(frame.addr as *mut u8, span as usize) };
		let Ok(mut target) = ImageViewMut::new(frame.layout.clone(), bytes) else { return false };
		let PixelStorage::Known(format) = frame.layout.storage else { return false };
		let description = TargetDescription { extent: frame.layout.extent, format, color_space: ColorSpace::Srgb, scale: 1.0, luminance: graphics_core::pixel::OutputLuminance::UNKNOWN };
		let images = Scene { image: targets.image.view() };
		let mut backend = core::mem::replace(&mut self.backend, Soft2d::new()).with_images(&images).with_glyphs(&self.forms);
		match backend.prepare(&self.list, &description) {
			Ok(prepared) => backend.render(&prepared, &mut target).is_ok(),
			Err(_) => false,
		}
	}
}

/// What one object identity looks like when the overlay is on.
///
/// FLAT AND DISTINCT, with no shading at all: what this view has to show is WHICH object owns a
/// pixel, and a lit rendering of an id is a picture in which two objects can share a colour.
fn identity_colour(ident: f32) -> Vec4 {
	match ident as u32 {
		1 => Vec4::new(0.95, 0.25, 0.20, 1.0),
		2 => Vec4::new(0.20, 0.45, 0.95, 1.0),
		3 => Vec4::new(0.95, 0.85, 0.20, 1.0),
		_ => Vec4::new(0.02, 0.02, 0.03, 1.0),
	}
}

/// The one image a HUD list refers to.
struct Scene<'a> {
	image: ImageView<'a>,
}

impl soft2d::target::ImageSource for Scene<'_> {
	fn image(&self, identity: u64) -> Option<ImageView<'_>> {
		(identity == SCENE_IMAGE).then(|| self.image.clone())
	}
}

/// The glyph forms this program carries, one of them a COLOUR one.
///
/// SHAPES AND NOT A FONT. The grants are `display` and `input-keys`: there is no font catalogue to
/// read a face from, and what is under test here is the DRAWING of a run - which is `render2d`'s
/// half of text and is exactly what a program with these two grants can prove.
struct Forms {
	/// How tall a glyph is, in surface pixels.
	///
	/// THE FORMS ARE IN PIXELS AND THE SIZE FIELD DOES NOT SCALE THEM. What `GlyphImage::Outline`
	/// carries is a path in the target's own pixel space, so a provider that ignored this drew the
	/// same nine-pixel letters at every window size - a line of text three pixels tall on a full
	/// screen, which is a glyph run nothing can read and no check can see.
	height: f32,
}

impl soft2d::glyph::GlyphProvider for Forms {
	fn glyph(&self, key: &font_contract::cache::GlyphCacheKey) -> soft2d::glyph::GlyphImage {
		let height = self.height;
		if key.glyph == COLOUR_GLYPH {
			let radius = height * 0.45;
			let centre = PointF { x: radius, y: -radius };
			let mut ring = PathBuilder::new();
			let _ = ring.add_circle(centre, radius);
			let mut pupil = PathBuilder::new();
			let _ = pupil.add_circle(centre, radius * 0.4);
			return soft2d::glyph::GlyphImage::Layers(alloc::vec![(ring.finish(), srgb(0.30, 0.80, 0.95, 1.0)), (pupil.finish(), srgb(0.05, 0.10, 0.20, 1.0))]);
		}
		// EVERY OTHER GLYPH IS AN OUTLINE, and its shape is a function of its id so that the line
		// reads as a line of different letters rather than a row of identical boxes.
		let mut builder = PathBuilder::new();
		let stem = height * (0.30 + ((key.glyph % 3) as f32) * 0.12);
		let _ = builder.add_rect(RectF::new(0.0, -height, stem, height));
		let _ = builder.add_rect(RectF::new(0.0, -height * (0.55 - ((key.glyph % 2) as f32) * 0.22), stem + height * 0.25, height * 0.2));
		soft2d::glyph::GlyphImage::Outline(builder.finish())
	}
}

/// The HUD's line of text: eight glyphs, the last of which carries its own colours.
fn label(origin_x: f32, origin_y: f32, size: f32) -> RecordedGlyphRun {
	let mut glyphs = Vec::new();
	for index in 0..8u32 {
		let glyph = if index == 7 { COLOUR_GLYPH } else { 20 + index };
		glyphs.push(font_contract::PositionedGlyph {
			glyph,
			x_offset: font_contract::Fixed266::ZERO,
			y_offset: font_contract::Fixed266::ZERO,
			// THE ADVANCE FOLLOWS THE FORMS. A fixed advance with forms that scale is a line that
			// runs into itself at one size and spreads out at another.
			x_advance: font_contract::Fixed266::from_raw((size * 0.9 * 64.0) as i32),
			y_advance: font_contract::Fixed266::ZERO,
			kind: font_contract::glyph::GlyphKind::Outline,
			selection: font_contract::cache::KindSelection { strike: None, palette: None },
		});
	}
	RecordedGlyphRun { face: font_contract::FaceRef { face: font_contract::FaceIdentity { file: font_contract::face::FileIdentity([7u8; 32]), index: 0 }, generation: font_contract::face::Generation(1) }, size: font_contract::Fixed266::from_raw((size * 64.0) as i32), variation: font_contract::VariationCoordinates::default(), script: font_contract::ScriptTag::from_bytes(*b"latn"), direction: font_contract::Direction::LeftToRight, mode: font_contract::glyph::RasterisationMode::Grayscale, origin_x: font_contract::Fixed266::from_raw((origin_x * 64.0) as i32), origin_y: font_contract::Fixed266::from_raw((origin_y * 64.0) as i32), glyphs, clusters: Vec::new() }
}

/// Where a run's nanoseconds went, one sum per stage.
///
/// STAGES A READER CAN ACT ON, which is why these five and not a single frame time: the 3D scene,
/// the transparent pass over it, the handover into the shared image model, the 2D composite, and the
/// present. A regression in any one of them is a different piece of work, and a single number says
/// only that something got slower.
#[derive(Default)]
struct Timing {
	scene: u64,
	/// The Extended phase's shadow pass, counted SEPARATELY from the lighting pass it feeds. Folding
	/// it into `scene` would report one number for two passes that scale with different things: the
	/// lighting pass with the window, and the shadow pass with the light's own map.
	shadow: u64,
	transparent: u64,
	handover: u64,
	overlay: u64,
	present: u64,
}

/// Print the timing report. NOT IN THE ORDINARY RUN: it is asked for with `--report`, because a demo
/// that printed measurements at every exit would be a demo whose output a person has to read past.
fn report(timing: &Timing, counts: &soft3d::frame::Stats, frames: u32, wall: u64, width: u32, height: u32, scene: (u32, u32), memory: (u64, u64, u64)) {
	let divisor = frames.max(1) as u64;
	let mut line = common_line::Line::new();
	line.push(b"test3d-sw: surface ");
	line.decimal(width as u64);
	line.push(b"x");
	line.decimal(height as u64);
	line.push(b" scene ");
	line.decimal(scene.0 as u64);
	line.push(b"x");
	line.decimal(scene.1 as u64);
	line.push(b"\n");
	print(line.as_bytes());
	// MILLI-FRAMES PER SECOND, so a rate below one frame a second is still a number rather than a
	// zero. The floor this is measured against is stated in whole frames and this is a thousandth of
	// one, which is finer than any run-to-run difference.
	let per_frame = wall / divisor;
	let mfps = if per_frame == 0 { 0 } else { 1_000_000_000_000 / per_frame };
	let mut line = common_line::Line::new();
	line.push(b"test3d-sw: frame ");
	line.decimal(per_frame / 1000);
	line.push(b"us rate ");
	line.decimal(mfps / 1000);
	line.push(b".");
	line.decimal((mfps % 1000) / 100);
	line.push(b" fps\n");
	print(line.as_bytes());
	for (name, total) in [
		(&b"shadow"[..], timing.shadow),
		(b"scene", timing.scene),
		(b"transparent", timing.transparent),
		(b"handover", timing.handover),
		(b"overlay", timing.overlay),
		(b"present", timing.present),
	] {
		let mut line = common_line::Line::new();
		line.push(b"test3d-sw: ");
		line.push(name);
		line.push(b" ");
		line.decimal(total / divisor / 1000);
		line.push(b"us/frame\n");
		print(line.as_bytes());
	}
	let mut line = common_line::Line::new();
	line.push(b"test3d-sw: colour ");
	line.decimal(memory.0 / 1024);
	line.push(b"kB depth ");
	line.decimal(memory.1 / 1024);
	line.push(b"kB image ");
	line.decimal(memory.2 / 1024);
	line.push(b"kB\n");
	print(line.as_bytes());
	let mut line = common_line::Line::new();
	line.push(b"test3d-sw: primitives ");
	line.decimal(counts.primitives as u64 / divisor);
	line.push(b" clipped ");
	line.decimal(counts.clipped as u64 / divisor);
	line.push(b" culled ");
	line.decimal(counts.culled as u64 / divisor);
	line.push(b" fragments ");
	line.decimal(counts.fragments as u64 / divisor);
	line.push(b"\n");
	print(line.as_bytes());
}
