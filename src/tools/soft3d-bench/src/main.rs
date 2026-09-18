//! THE SOFT3D PERFORMANCE FLOOR, measured headlessly and stage by stage.
//!
//! IT NEEDS NO SURFACE, NO DisplayService, NO GUEST AND NO APPLICATION. One frozen scene is prepared
//! once and executed into attachments the program owns, which is what makes this a number a person
//! gets in a second on a host rather than a boot away - and what stops the live demo becoming a
//! prerequisite of the backend's own floor.
//!
//! THE STAGES ARE MEASURED BY DIFFERENCE AND NOT BY A CLOCK INSIDE THE LIBRARY. `soft3d` is `no_std`
//! and has no clock; giving it one so a benchmark could read it would put a timer on every frame the
//! system ever renders. What this program does instead is run the SAME geometry through five
//! pipelines that differ by exactly one stage each, and report the differences:
//!
//!     geometry      every primitive back-face culled: transform, clip and cull, and no fragment
//!     raster        the same geometry front-facing, with a fragment stage that writes a constant
//!     shading       the lit fragment stage, with no texture read
//!     texturing     the lit stage with its two texture samples
//!     blending      the same again with the blend equation enabled
//!
//! Each difference is one stage's cost, and each figure says which two runs it came from. A reader
//! who doubts a number can run the two variants and subtract them by hand.
//!
//! THE WORKLOAD IS FROZEN AND THIS PROGRAM CHECKS THAT IT IS. The triangle count, the vertex count
//! and the extent are asserted against the numbers recorded here, so a later simplification cannot
//! quietly lower the workload and report the same milliseconds against an easier scene. A benchmark
//! whose workload can drift measures the workload and not the renderer.

use std::time::Instant;

use render_math::{Mat4, Quat, Vec3, Vec4, camera};
use render_shader::builder::Builder;
use render_shader::ir::{BinaryOp, Binding, Constant, Interpolation, Module, Op, Output, Sampling, Stage, Transcendental, Type, UnaryOp};
use render3d::{CompareOp, Cull, DepthFormat, Render3DLimits, Topology};
use soft3d::frame::{Attachments, Draw, Pipeline, Prepared, Source, Stats};
use soft3d::pass::{Colour, DepthStencil};
use soft3d::texture::{Filter, Kind, Level, Sampler, Texture, Wrap};
use soft3d::{Indices, Val};

/// THE MEASURED EXTENT. The floor the milestone states is at this size, so the default is this size
/// and every other one is a comparison rather than the claim.
const WIDTH: u32 = 640;
const HEIGHT: u32 = 480;

/// The frozen workload: how many cubes, and what that makes the scene.
///
/// TWENTY-FOUR TRIANGLES PER CUBE AND TWENTY-FOUR VERTICES, because a cube with flat per-face
/// normals cannot share corners. The count below is asserted, not computed from whatever the mesh
/// builder happened to produce.
const CUBES: usize = 16;
const TRIANGLES_PER_CUBE: usize = 12;
const VERTICES_PER_CUBE: usize = 24;
const EXPECTED_TRIANGLES: usize = CUBES * TRIANGLES_PER_CUBE;
const EXPECTED_VERTICES: usize = CUBES * VERTICES_PER_CUBE;

/// How many frames are thrown away before measuring, and how many are kept.
///
/// THE WARMUP IS NOT A COURTESY. The first execution touches every page of the attachments and pulls
/// the value table into cache; including it would measure the allocator's first-touch cost once and
/// divide it into every sample.
const WARMUP: usize = 2;
const SAMPLES: usize = 8;

/// The uniform block members, named at both ends so an index cannot drift between them.
const U_MVP: u32 = 0;
const U_MODEL: u32 = 1;
const U_LIGHT_DIR: u32 = 2;
const U_EYE: u32 = 3;
const U_AMBIENT: u32 = 4;

const TEX_A: u32 = 0;
const TEX_B: u32 = 1;
const SAMP_CLAMP: u32 = 0;
const SAMP_REPEAT: u32 = 1;

/// Which fragment stage a variant runs, which is the only thing that differs between them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Variant {
	Geometry,
	Raster,
	Shading,
	Texturing,
	Blending,
}

impl Variant {
	const ALL: [Variant; 5] = [Variant::Geometry, Variant::Raster, Variant::Shading, Variant::Texturing, Variant::Blending];

	fn name(self) -> &'static str {
		match self {
			Variant::Geometry => "geometry",
			Variant::Raster => "raster",
			Variant::Shading => "shading",
			Variant::Texturing => "texturing",
			Variant::Blending => "blending",
		}
	}

	/// What this variant's difference from the one before it MEASURES.
	fn attributed_to(self) -> &'static str {
		match self {
			Variant::Geometry => "transform, clip and cull",
			Variant::Raster => "rasterisation, depth test and attachment write",
			Variant::Shading => "the lit fragment stage",
			Variant::Texturing => "two texture samples per fragment",
			Variant::Blending => "the blend equation",
		}
	}

	/// THE GEOMETRY VARIANT DRAWS INTO ONE PIXEL, which is how transform, clipping and culling are
	/// measured with nothing after them.
	///
	/// The first way this was written culled the FRONT faces instead, on the reasoning that a culled
	/// primitive produces no fragment. It produced the back ones instead - the same count - and the
	/// variant measured a whole shaded frame. What removes the fragments without touching the
	/// geometry is the ATTACHMENT: every primitive is still transformed, still clipped against the
	/// same planes and still culled by the same rule, and what is left to rasterise is one pixel.
	fn extent(self, width: u32, height: u32) -> (u32, u32) {
		if self == Variant::Geometry { (1, 1) } else { (width, height) }
	}

	fn blending(self) -> bool {
		self == Variant::Blending
	}
}

/// One vertex of the scene.
#[derive(Clone, Copy)]
struct Vertex {
	position: [f32; 4],
	normal: [f32; 4],
	colour: [f32; 4],
	uv: [f32; 4],
}

/// The scene: a grid of cubes, deliberately overlapping, with the nearest ones crossing the near
/// plane so the clipper has work to do.
struct Scene {
	vertices: Vec<Vertex>,
	indices: Vec<u32>,
}

impl Scene {
	fn build() -> Scene {
		let faces: [([f32; 3], [f32; 3], [f32; 4]); 6] = [
			([0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [0.86, 0.22, 0.22, 0.65]),
			([0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [0.20, 0.62, 0.86, 0.65]),
			([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.26, 0.76, 0.34, 0.65]),
			([-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.92, 0.74, 0.20, 0.65]),
			([0.0, 1.0, 0.0], [0.0, 0.0, -1.0], [0.78, 0.36, 0.82, 0.65]),
			([0.0, -1.0, 0.0], [0.0, 0.0, 1.0], [0.30, 0.78, 0.78, 0.65]),
		];
		let mut vertices: Vec<Vertex> = Vec::new();
		let mut indices: Vec<u32> = Vec::new();
		for cube in 0..CUBES {
			// A FOUR BY FOUR GRID WALKED TOWARDS THE CAMERA. The near column crosses `z = -0.1` in
			// view space, which is what makes the clipper run on real primitives rather than on a
			// fixture built to look like one; the columns overlap on screen, which is the overdraw.
			let column = (cube % 4) as f32;
			let row = (cube / 4) as f32;
			let centre = Vec3::new((column - 1.5) * 0.9, (row - 1.5) * 0.55, 1.6 - row * 1.6);
			for (normal, up, colour) in faces {
				let n = Vec3::new(normal[0], normal[1], normal[2]);
				let u = Vec3::new(up[0], up[1], up[2]);
				let right = Vec3::new(u.y * n.z - u.z * n.y, u.z * n.x - u.x * n.z, u.x * n.y - u.y * n.x);
				let base = vertices.len() as u32;
				for (sx, sy) in [(-1.0_f32, -1.0_f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
					let local = Vec3::new(n.x + right.x * sx + u.x * sy, n.y + right.y * sx + u.y * sy, n.z + right.z * sx + u.z * sy);
					vertices.push(Vertex {
						position: [centre.x + local.x * 0.55, centre.y + local.y * 0.55, centre.z + local.z * 0.55, 1.0],
						normal: [n.x, n.y, n.z, 0.0],
						colour,
						// COORDINATES OUTSIDE `0..=1` ON ONE AXIS, so the repeating sampler wraps and
						// the clamping one does not - both addressing rules in one scene.
						uv: [(sx + 1.0) * 1.5, (1.0 - sy) * 0.5, 0.0, 0.0],
					});
				}
				indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
			}
		}
		Scene { vertices, indices }
	}
}

/// What the shaders read.
struct Frame {
	scene: Scene,
	textures: [Texture; 2],
	samplers: [Sampler; 2],
	mvp: Mat4,
	model: Mat4,
	light_dir: [f32; 4],
	eye: [f32; 4],
	ambient: [f32; 4],
}

impl Source for Frame {
	fn attribute(&self, location: u32, vertex: u32, _instance: u32) -> Option<Val> {
		let vertex = self.scene.vertices.get(vertex as usize)?;
		match location {
			0 => Some(Val::vector_f32(&vertex.position)),
			1 => Some(Val::vector_f32(&vertex.normal)),
			2 => Some(Val::vector_f32(&vertex.colour)),
			3 => Some(Val::vector_f32(&vertex.uv)),
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
			U_EYE => Some(Val::vector_f32(&self.eye)),
			U_AMBIENT => Some(Val::vector_f32(&self.ambient)),
			_ => None,
		}
	}

	fn sample(&self, texture: u32, sampler: u32, coordinate: &Val) -> Option<Val> {
		if coordinate.len() < 2 {
			return None;
		}
		let texture = self.textures.get(texture as usize)?;
		let sampler = self.samplers.get(sampler as usize)?;
		let texel = soft3d::texture::sample(texture, sampler, [coordinate.f32_at(0), coordinate.f32_at(1), 0.0], 0.0, true);
		Some(Val::vector_f32(&texel))
	}

	fn indices(&self) -> Indices<'_> {
		Indices::U32(&self.scene.indices)
	}
}

fn columns(m: Mat4) -> [[f32; 4]; 4] {
	let mut out = [[0.0_f32; 4]; 4];
	for (index, column) in out.iter_mut().enumerate() {
		let source = m.column(index);
		*column = [source.x, source.y, source.z, source.w];
	}
	out
}

fn checkerboard(id: u32, extent: u32, squares: u32) -> Texture {
	let mut level = Level::new(extent, extent, 1);
	let cell = (extent / squares).max(1);
	for y in 0..extent {
		for x in 0..extent {
			let odd = ((x / cell) + (y / cell)) % 2 == 1;
			let value = if odd { 0.22 } else { 0.94 };
			level.set(x, y, 0, [value, value, value * 1.05, 1.0]);
		}
	}
	Texture { id, kind: Kind::Dim2, levels: vec![level], transfer: graphics_profile::image::Transfer::Linear, semantics: graphics_profile::image::Semantics::Color, premultiplied: false }
}

fn vertex_stage() -> Module {
	let mut builder = Builder::new(Stage::Vertex, "bench-vertex");
	for location in 0..4 {
		builder.varying_at(location, Type::vec(4), Interpolation::Smooth, Sampling::Pixel);
	}
	let position = builder.load(Type::vec(4), Binding::Attribute { location: 0 });
	let normal = builder.load(Type::vec(4), Binding::Attribute { location: 1 });
	let colour = builder.load(Type::vec(4), Binding::Attribute { location: 2 });
	let uv = builder.load(Type::vec(4), Binding::Attribute { location: 3 });
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
	builder.finish()
}

/// The fragment stage each variant runs. THE ONE PLACE THE VARIANTS DIFFER, so the difference
/// between two measurements is the difference between two of these and nothing else.
fn fragment_stage(variant: Variant) -> Module {
	let mut builder = Builder::new(Stage::Fragment, "bench-fragment");
	for location in 0..4 {
		builder.varying_at(location, Type::vec(4), Interpolation::Smooth, Sampling::Pixel);
	}
	let base = builder.load(Type::vec(4), Binding::Varying { location: 0 });
	let zero = builder.constant(Constant::F32(0.0));
	let one = builder.constant(Constant::F32(1.0));
	if matches!(variant, Variant::Geometry | Variant::Raster) {
		// A CONSTANT OUT OF THE VARYING IT ALREADY READ, so the interpolator still runs and the
		// shading does not. A stage that read nothing would also measure away the interpolation,
		// which belongs to rasterisation rather than to shading.
		builder.store(Output::Colour(0), base);
		return builder.finish();
	}

	let normal_in = builder.load(Type::vec(4), Binding::Varying { location: 1 });
	let world = builder.load(Type::vec(4), Binding::Varying { location: 2 });
	let light_dir = builder.load(Type::vec(4), Binding::Uniform { block: 0, member: U_LIGHT_DIR });
	let eye = builder.load(Type::vec(4), Binding::Uniform { block: 0, member: U_EYE });
	let ambient = builder.load(Type::vec(4), Binding::Uniform { block: 0, member: U_AMBIENT });
	let shininess = builder.constant(Constant::F32(24.0));
	let spec_strength = builder.constant(Constant::F32(0.35));

	let base = if matches!(variant, Variant::Texturing | Variant::Blending) {
		let uv = builder.load(Type::vec(4), Binding::Varying { location: 3 });
		let first = builder.assign(Type::vec(4), Op::Sample { binding: Binding::Texture { texture: TEX_A, sampler: SAMP_CLAMP }, coordinate: uv });
		let second = builder.assign(Type::vec(4), Op::Sample { binding: Binding::Texture { texture: TEX_B, sampler: SAMP_REPEAT }, coordinate: uv });
		let both = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Multiply, first, second));
		builder.assign(Type::vec(4), Op::Binary(BinaryOp::Multiply, base, both))
	} else {
		base
	};

	let n = builder.assign(Type::vec(4), Op::Unary(UnaryOp::Normalize, normal_in));
	let l = builder.assign(Type::vec(4), Op::Unary(UnaryOp::Normalize, light_dir));
	let ndotl = builder.assign(Type::f32(), Op::Binary(BinaryOp::Dot, n, l));
	let diffuse = builder.assign(Type::f32(), Op::Binary(BinaryOp::Max, ndotl, zero));
	let to_eye = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Subtract, eye, world));
	let v = builder.assign(Type::vec(4), Op::Unary(UnaryOp::Normalize, to_eye));
	let half_raw = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Add, v, l));
	let h = builder.assign(Type::vec(4), Op::Unary(UnaryOp::Normalize, half_raw));
	let ndoth = builder.assign(Type::f32(), Op::Binary(BinaryOp::Dot, n, h));
	let ndoth_clamped = builder.assign(Type::f32(), Op::Binary(BinaryOp::Max, ndoth, zero));
	let spec_raw = builder.assign(Type::f32(), Op::Transcendental(Transcendental::Pow, ndoth_clamped, Some(shininess)));
	let spec = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, spec_raw, spec_strength));
	let lit = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![diffuse, diffuse, diffuse, zero]));
	let shade = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Add, ambient, lit));
	let shaded = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Multiply, base, shade));
	let highlight = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![spec, spec, spec, zero]));
	let rgb = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Add, shaded, highlight));
	let alpha = builder.assign(Type::f32(), Op::Extract(base, 3));
	let r = builder.assign(Type::f32(), Op::Extract(rgb, 0));
	let g = builder.assign(Type::f32(), Op::Extract(rgb, 1));
	let b = builder.assign(Type::f32(), Op::Extract(rgb, 2));
	let r = builder.assign(Type::f32(), Op::Clamp { value: r, low: zero, high: one });
	let g = builder.assign(Type::f32(), Op::Clamp { value: g, low: zero, high: one });
	let b = builder.assign(Type::f32(), Op::Clamp { value: b, low: zero, high: one });
	let out = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![r, g, b, alpha]));
	builder.store(Output::Colour(0), out);
	builder.finish()
}

fn pipeline_for(variant: Variant) -> Pipeline {
	let blend = if variant.blending() { render3d::AttachmentBlend { enabled: true, colour: render3d::BlendEquation { source: render3d::BlendFactor::SrcAlpha, destination: render3d::BlendFactor::OneMinusSrcAlpha, operation: render3d::BlendOp::Add }, alpha: render3d::BlendEquation { source: render3d::BlendFactor::One, destination: render3d::BlendFactor::OneMinusSrcAlpha, operation: render3d::BlendOp::Add }, write_mask: render3d::ColorWriteMask::ALL } } else { render3d::AttachmentBlend { enabled: false, colour: render3d::BlendEquation::REPLACE, alpha: render3d::BlendEquation::REPLACE, write_mask: render3d::ColorWriteMask::ALL } };
	Pipeline { state: render3d::command::PipelineState { topology: Topology::TriangleList, cull: Cull::Back, depth_test: Some(CompareOp::Less), depth_write: true, samples: 1, per_sample_shading: false }, vertex: vertex_stage(), fragment: fragment_stage(variant), blend: vec![blend], stencil: None, depth_compare: CompareOp::Less, depth_write: true, bias: (0.0, 0.0, 0.0), alpha_to_coverage: false, sample_mask: u32::MAX }
}

/// One variant's measurement.
struct Measured {
	variant: Variant,
	nanoseconds: f64,
	stats: Stats,
}

fn measure(variant: Variant, frame: &mut Frame, requested_width: u32, requested_height: u32) -> Measured {
	let (width, height) = variant.extent(requested_width, requested_height);
	let draw = Draw { pipeline: 0, topology: Topology::TriangleList, count: frame.scene.indices.len() as u32, instances: 1, first_instance: 0, base_vertex: 0, restart: false };
	let mut prepared: Prepared = soft3d::frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline_for(variant)], vec![draw], width, height).expect("the benchmark pipeline is one the profile admits");
	let mut colour = Colour::new(width, height, 1, false);
	let mut depth = DepthStencil::new(width, height, 1, DepthFormat::Depth32F);
	let viewport = render_math::camera::Viewport { x: 0.0, y: 0.0, width: width as f32, height: height as f32, min_depth: 0.0, max_depth: 1.0 };

	let mut run = |frame: &Frame, colour: &mut Colour, depth: &mut DepthStencil| -> Stats {
		colour.fill(Vec4::new(0.05, 0.06, 0.10, 1.0));
		depth.clear(1.0, 0);
		let mut attachments = Attachments { colour: std::slice::from_mut(colour), depth_stencil: Some(depth), viewport, scissor: None };
		soft3d::frame::execute(&mut prepared, &mut attachments, frame).expect("the benchmark frame is one the backend accepts")
	};

	for _ in 0..WARMUP {
		run(frame, &mut colour, &mut depth);
	}
	let began = Instant::now();
	let mut stats = Stats::default();
	for _ in 0..SAMPLES {
		stats = run(frame, &mut colour, &mut depth);
	}
	let elapsed = began.elapsed();
	Measured { variant, nanoseconds: elapsed.as_nanos() as f64 / SAMPLES as f64, stats }
}

/// A stub the interpreter can be driven against with no rasteriser in front of it.
///
/// WHY A MODE LIKE THIS EXISTS. The five variants above measure a FRAME, and a frame's cost is the
/// rasteriser, the interpolator, the attachment write and the shader together. When the shader is
/// the part that is slow, a frame measurement moves by a few percent for a change that halved the
/// interpreter - so the interpreter gets a measurement of its own, with every other stage removed.
struct Stub {
	values: [Val; 6],
	texel: Val,
}

impl soft3d::interpreter::Resources for Stub {
	fn uniform(&self, _block: u32, member: u32) -> Option<Val> {
		self.values.get(member as usize % self.values.len()).cloned()
	}

	fn attribute(&self, location: u32) -> Option<Val> {
		self.values.get(location as usize % self.values.len()).cloned()
	}

	fn varying(&self, location: u32) -> Option<Val> {
		self.values.get(location as usize % self.values.len()).cloned()
	}

	fn built_in(&self, _which: render_shader::ir::BuiltIn) -> Option<Val> {
		Some(self.values[0].clone())
	}

	fn sample(&self, _texture: u32, _sampler: u32, _coordinate: &Val) -> Option<Val> {
		Some(self.texel.clone())
	}
}

/// Run one module a great many times and report what one instruction costs.
fn interpreter_only(runs: usize) {
	let stub = Stub {
		values: [
			Val::vector_f32(&[0.4, 0.5, 0.6, 1.0]),
			Val::vector_f32(&[0.3, 0.8, 0.5, 0.0]),
			Val::vector_f32(&[1.2, 0.4, 2.0, 1.0]),
			Val::vector_f32(&[0.5, 0.5, 0.5, 1.0]),
			Val::vector_f32(&[0.2, 0.2, 0.25, 0.0]),
			Val::vector_f32(&[0.0, 0.0, 3.0, 1.0]),
		],
		texel: Val::vector_f32(&[0.7, 0.7, 0.75, 1.0]),
	};
	// A SYNTHETIC MODULE OF PURE CONSTANTS, which reads nothing and computes nothing: it isolates
	// what an instruction costs BEFORE any operand is read or any arithmetic is done - the dispatch,
	// the value the operation returns and the store into the value table.
	{
		let mut builder = Builder::new(Stage::Fragment, "bench-constants");
		builder.varying_at(0, Type::vec(4), Interpolation::Smooth, Sampling::Pixel);
		let scalar = builder.constant(Constant::F32(0.5));
		for _ in 0..35 {
			let _ = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![scalar, scalar, scalar, scalar]));
		}
		let module = builder.finish();
		let statements = module.body.len();
		let mut machine = soft3d::interpreter::Machine::default();
		for _ in 0..1000 {
			soft3d::interpreter::execute_into(&module, &stub, &mut machine).expect("the stub answers every binding");
		}
		let began = Instant::now();
		for _ in 0..runs {
			soft3d::interpreter::execute_into(&module, &stub, &mut machine).expect("the stub answers every binding");
		}
		let elapsed = began.elapsed().as_nanos() as f64 / runs as f64;
		println!("soft3d-bench: interpreter {:<10} {statements:>3} statements {elapsed:>8.1} ns/run {:>6.1} ns/statement", "compose", elapsed / statements as f64);
	}
	for variant in [Variant::Raster, Variant::Shading, Variant::Texturing] {
		let module = fragment_stage(variant);
		let statements = module.body.len();
		let mut machine = soft3d::interpreter::Machine::default();
		for _ in 0..1000 {
			soft3d::interpreter::execute_into(&module, &stub, &mut machine).expect("the stub answers every binding");
		}
		let began = Instant::now();
		for _ in 0..runs {
			soft3d::interpreter::execute_into(&module, &stub, &mut machine).expect("the stub answers every binding");
		}
		let elapsed = began.elapsed().as_nanos() as f64 / runs as f64;
		println!("soft3d-bench: interpreter {:<10} {statements:>3} statements {elapsed:>8.1} ns/run {:>6.1} ns/statement", variant.name(), elapsed / statements as f64);
	}
}

fn main() {
	let mut arguments = std::env::args().skip(1);
	let mut width = WIDTH;
	let mut height = HEIGHT;
	while let Some(argument) = arguments.next() {
		match argument.as_str() {
			"--width" => width = arguments.next().and_then(|value| value.parse().ok()).unwrap_or(WIDTH),
			"--height" => height = arguments.next().and_then(|value| value.parse().ok()).unwrap_or(HEIGHT),
			"--interpreter" => {
				interpreter_only(200_000);
				return;
			}
			"--help" | "-h" => {
				println!("usage: soft3d-bench [--width N] [--height N]");
				println!("One frozen scene through five pipelines that differ by one stage each.");
				return;
			}
			other => {
				eprintln!("soft3d-bench: unknown argument '{other}'");
				std::process::exit(2);
			}
		}
	}

	let scene = Scene::build();
	// THE FROZEN WORKLOAD, ASSERTED. A benchmark whose scene can change is a benchmark whose numbers
	// cannot be compared with yesterday's.
	assert_eq!(scene.vertices.len(), EXPECTED_VERTICES, "the benchmark scene's vertex count is frozen");
	assert_eq!(scene.indices.len(), EXPECTED_TRIANGLES * 3, "the benchmark scene's triangle count is frozen");

	let eye = Vec3::new(0.0, 0.7, 3.4);
	let projection = camera::perspective_rh_zo(0.9, width as f32 / height as f32, 0.1, 60.0).expect("a perspective the camera admits");
	let look = camera::look_at_rh(eye, Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0)).expect("a view the camera admits");
	let model = Mat4::from_linear(&Quat::from_axis_angle(Vec3::new(0.0, 1.0, 0.0), 0.6).expect("a rotation the quaternion admits").to_mat3(), Vec3::new(0.0, 0.0, 0.0));
	let mut frame = Frame {
		scene,
		textures: [checkerboard(TEX_A, 64, 8), checkerboard(TEX_B, 32, 4)],
		samplers: [
			Sampler { wrap_u: Wrap::ClampToEdge, wrap_v: Wrap::ClampToEdge, magnify: Filter::Linear, minify: Filter::Linear, ..Sampler::NEAREST },
			Sampler { wrap_u: Wrap::Repeat, wrap_v: Wrap::Repeat, magnify: Filter::Linear, minify: Filter::Linear, ..Sampler::NEAREST },
		],
		mvp: projection.mul(&look).mul(&model),
		model,
		light_dir: [0.45, 0.8, 0.35, 0.0],
		eye: [eye.x, eye.y, eye.z, 1.0],
		ambient: [0.22, 0.22, 0.26, 0.0],
	};

	println!("soft3d-bench: {width}x{height}, {EXPECTED_TRIANGLES} triangles, {EXPECTED_VERTICES} vertices, {SAMPLES} samples after {WARMUP} warmup frames");
	let mut measurements: Vec<Measured> = Vec::new();
	for variant in Variant::ALL {
		measurements.push(measure(variant, &mut frame, width, height));
	}

	let mut previous = 0.0_f64;
	for measured in &measurements {
		let milliseconds = measured.nanoseconds / 1_000_000.0;
		let attributable = measured.nanoseconds - previous;
		println!("soft3d-bench: {:<10} {:>9.3} ms/frame   +{:>9.3} ms for {}", measured.variant.name(), milliseconds, attributable / 1_000_000.0, measured.variant.attributed_to());
		previous = measured.nanoseconds;
	}

	// THE TWO RATES THE MILESTONE ASKS FOR, from the LAST variant - the one that does every stage,
	// which is the only one that corresponds to a frame an application renders.
	let full = measurements.last().expect("five variants were measured");
	let seconds = full.nanoseconds / 1_000_000_000.0;
	let triangles_per_second = EXPECTED_TRIANGLES as f64 / seconds;
	let fragments_per_second = full.stats.fragments as f64 / seconds;
	println!("soft3d-bench: {:.0} triangles/s, {:.0} shaded fragments/s", triangles_per_second, fragments_per_second);
	println!("soft3d-bench: {} primitives, {} clipped, {} culled, {} fragments, {} samples written per frame", full.stats.primitives, full.stats.clipped, full.stats.culled, full.stats.fragments, full.stats.samples_written);
	println!("soft3d-bench: {:.2} frames/s at {width}x{height}", 1.0 / seconds);

	// THE CLIPPER MUST HAVE RUN. A scene arranged to cross the near plane that reports no clipped
	// primitive is a scene that drifted away from what it was built to measure, and the numbers above
	// would then be a different workload's.
	assert!(full.stats.clipped > 0, "the benchmark scene crosses the near plane and must clip");
	assert!(full.stats.fragments > 0, "the benchmark scene must cover fragments");
}
