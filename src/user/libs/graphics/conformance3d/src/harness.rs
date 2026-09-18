//! WHAT EVERY SCENE IS BUILT OUT OF: a mesh, a pipeline, a target, and a way to ask about a pixel.
//!
//! THE TARGET IS THE BACKEND'S OWN ATTACHMENT AND NOT AN IMAGE. `soft3d` composites into `f32`
//! attachments, which is where every decision a scene is about is made: coverage, the depth test, the
//! blend equation and the identity write all happen there, and a scene that read an encoded image
//! would be reading the conversion afterwards as well. The one scene about the conversion asks for it
//! by name.
//!
//! AND A REFUSAL IS NOT A FAILURE, it is `Unsupported`. The distinction matters because the profile
//! is a CLOSED list: a backend that refuses a Profile 1 frame is not a backend with a gap, it is a
//! backend that does not conform, and the suite reports the two differently so that a reader can tell
//! "this is wrong" from "this is missing".

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use render_math::Vec4;
use render_math::camera::Viewport;
use render_shader::builder::Builder;
use render_shader::ir::{Binding, Module, Op, Output, Stage, Type};
use render3d::{CompareOp, Cull, DepthFormat, Render3DLimits, Topology};
use soft3d::frame::{Attachments, Draw, Pipeline, Prepared, Source, Stats};
use soft3d::pass::{Colour, DepthStencil};
use soft3d::{Indices, Val};

/// Why a scene did not pass.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Trouble {
	Failed(String),
	Unsupported(String),
}

/// A backend refusal is a refusal of something the profile requires.
impl From<render3d::Error> for Trouble {
	fn from(error: render3d::Error) -> Self {
		Trouble::Unsupported(format!("the frame was refused with {error:?}"))
	}
}

/// State a pass condition, and say what was seen when it does not hold.
macro_rules! require {
	($condition:expr, $($detail:tt)*) => {
		if !($condition) {
			return Err($crate::Trouble::Failed(alloc::format!($($detail)*)));
		}
	};
}

/// The default target. SMALL ON PURPOSE: every scene states a pass condition about NAMED pixels, so
/// what an extra hundred rows would add is time and not evidence.
pub const WIDTH: u32 = 32;
pub const HEIGHT: u32 = 32;

/// One vertex, in the one layout every scene in this suite uses.
///
/// FOUR ATTRIBUTES AND ALWAYS THE SAME FOUR, because a scene is about one feature: a suite in which
/// each scene invented its own layout would be a suite in which a layout mistake reads as the
/// feature being wrong. The scenes that are ABOUT the layout say so and use their own.
#[derive(Clone, Copy, Default)]
pub struct Vertex {
	pub position: [f32; 4],
	pub colour: [f32; 4],
	pub uv: [f32; 4],
	/// What the fragment stage writes into the integer attachment, when there is one.
	pub ident: [f32; 4],
}

impl Vertex {
	/// A vertex in CLIP SPACE, which is where a scene about coverage wants to work: no camera, no
	/// projection, and `w = 1`, so what a position says is where it lands.
	pub fn at(x: f32, y: f32, z: f32) -> Vertex {
		Vertex { position: [x, y, z, 1.0], colour: [1.0, 1.0, 1.0, 1.0], uv: [0.0; 4], ident: [1.0, 0.0, 0.0, 0.0] }
	}

	pub fn coloured(mut self, red: f32, green: f32, blue: f32, alpha: f32) -> Vertex {
		self.colour = [red, green, blue, alpha];
		self
	}

	pub fn with_uv(mut self, u: f32, v: f32) -> Vertex {
		self.uv = [u, v, 0.0, 0.0];
		self
	}

	/// A three-component coordinate, which a cube direction and a volume read both need.
	pub fn with_uvw(mut self, u: f32, v: f32, w: f32) -> Vertex {
		self.uv = [u, v, w, 0.0];
		self
	}

	pub fn with_ident(mut self, ident: f32) -> Vertex {
		self.ident = [ident, 0.0, 0.0, 0.0];
		self
	}
}

/// What a scene hands the backend: its vertices, its indices, and whatever a texture read answers.
pub struct Scene {
	pub vertices: Vec<Vertex>,
	pub indices: Indices16Or32,
	/// The uniform block, member by member. A scene that needs one puts values here rather than
	/// inventing a second binding kind.
	pub uniforms: Vec<Val>,
	/// What `sample` answers, per texture. `None` for a scene with no textures, which is most of them.
	pub textures: Vec<(soft3d::texture::Texture, soft3d::texture::Sampler)>,
	/// Where a sample is taken from, for the scenes about array layers and cube faces.
	pub sampling: Sampling,
	/// The instance offset a scene about instancing reads, one entry per instance.
	pub instance_offsets: Vec<[f32; 4]>,
}

/// Which of `soft3d::texture`'s entry points a `sample` goes through.
///
/// A CUBE AND AN ARRAY ARE NOT A 2D READ WITH EXTRA COORDINATES. Each has its own rule - a face is
/// selected by the major axis of a direction, a layer by an explicit index - and a suite that read
/// all three through one entry point would be testing one of them three times.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sampling {
	Plain,
	Cube,
	Array(u32),
	Compare(u32),
	Anisotropic,
	Level(u32),
}

/// The index buffer a scene draws with.
pub enum Indices16Or32 {
	None,
	U16(Vec<u16>),
	U32(Vec<u32>),
}

impl Scene {
	pub fn new(vertices: Vec<Vertex>) -> Scene {
		Scene { vertices, indices: Indices16Or32::None, uniforms: Vec::new(), textures: Vec::new(), sampling: Sampling::Plain, instance_offsets: Vec::new() }
	}

	pub fn indexed16(mut self, indices: Vec<u16>) -> Scene {
		self.indices = Indices16Or32::U16(indices);
		self
	}

	pub fn indexed32(mut self, indices: Vec<u32>) -> Scene {
		self.indices = Indices16Or32::U32(indices);
		self
	}

	pub fn with_texture(mut self, texture: soft3d::texture::Texture, sampler: soft3d::texture::Sampler) -> Scene {
		self.textures.push((texture, sampler));
		self
	}

	pub fn sampled_as(mut self, sampling: Sampling) -> Scene {
		self.sampling = sampling;
		self
	}

	pub fn instanced(mut self, offsets: Vec<[f32; 4]>) -> Scene {
		self.instance_offsets = offsets;
		self
	}
}

impl Source for Scene {
	fn attribute(&self, location: u32, vertex: u32, instance: u32) -> Option<Val> {
		let vertex = self.vertices.get(vertex as usize)?;
		match location {
			0 => Some(Val::vector_f32(&vertex.position)),
			1 => Some(Val::vector_f32(&vertex.colour)),
			2 => Some(Val::vector_f32(&vertex.uv)),
			3 => Some(Val::vector_f32(&vertex.ident)),
			// THE PER-INSTANCE ATTRIBUTE, which is what makes instancing visible at all: every
			// instance of one draw reads the same vertices, so what tells them apart has to come
			// from somewhere else.
			4 => Some(Val::vector_f32(self.instance_offsets.get(instance as usize).unwrap_or(&[0.0; 4]))),
			_ => None,
		}
	}

	fn uniform(&self, block: u32, member: u32) -> Option<Val> {
		if block != 0 {
			return None;
		}
		self.uniforms.get(member as usize).cloned()
	}

	fn sample(&self, texture: u32, _sampler: u32, coordinate: &Val) -> Option<Val> {
		let (texture, sampler) = self.textures.get(texture as usize)?;
		if coordinate.len() < 2 {
			return None;
		}
		let (u, v, w) = (coordinate.f32_at(0), coordinate.f32_at(1), if coordinate.len() > 2 { coordinate.f32_at(2) } else { 0.0 });
		let texel = match self.sampling {
			Sampling::Plain => soft3d::texture::sample(texture, sampler, [u, v, w], 0.0, true),
			Sampling::Level(level) => soft3d::texture::sample(texture, sampler, [u, v, w], level as f32, false),
			Sampling::Cube => soft3d::texture::sample_cube(texture, sampler, [u, v, w], 0.0),
			Sampling::Array(layer) => soft3d::texture::sample_array(texture, sampler, [u, v], layer, 0.0).ok()?,
			// A DEPTH-COMPARE SAMPLE ANSWERS A COVERAGE AND NOT A COLOUR: the reference is compared
			// against the stored depth and what comes back is how much of the read passed.
			Sampling::Compare(_) => {
				let passed = soft3d::texture::sample_compare(texture, sampler, [u, v], 0, w).ok()?;
				[passed, passed, passed, 1.0]
			}
			// The derivatives a scene declares are one texel across in `u` and none in `v`, which is
			// the grazing case anisotropy exists for.
			Sampling::Anisotropic => {
				let level = texture.level(0).map(|level| (level.width, level.height)).unwrap_or((1, 1));
				soft3d::texture::sample_anisotropic(texture, sampler, [u, v, w], [4.0 / level.0 as f32, 0.0], [0.0, 1.0 / level.1 as f32], level.0, level.1)
			}
		};
		Some(Val::vector_f32(&texel))
	}

	fn indices(&self) -> Indices<'_> {
		match &self.indices {
			Indices16Or32::None => Indices::None,
			Indices16Or32::U16(values) => Indices::U16(values),
			Indices16Or32::U32(values) => Indices::U32(values),
		}
	}
}

/// The vertex stage every scene shares: the position through, and the three varyings a fragment
/// stage reads.
///
/// NO TRANSFORM AT ALL, deliberately. A scene about coverage or depth states its positions in clip
/// space, so a matrix between them and the rasteriser is a second thing that can be wrong. The scenes
/// that are ABOUT a transform build their own.
pub fn vertex_stage() -> Module {
	let mut builder = Builder::new(Stage::Vertex, "conformance-vertex");
	for location in 0..3 {
		builder.varying(location, Type::vec(4), render_shader::Interpolation::Smooth);
	}
	builder.varying_at(3, Type::vec(4), render_shader::Interpolation::Flat, render_shader::ir::Sampling::Pixel);
	let position = builder.load(Type::vec(4), Binding::Attribute { location: 0 });
	let colour = builder.load(Type::vec(4), Binding::Attribute { location: 1 });
	let uv = builder.load(Type::vec(4), Binding::Attribute { location: 2 });
	let ident = builder.load(Type::vec(4), Binding::Attribute { location: 3 });
	builder.store(Output::Position, position);
	builder.store(Output::Varying(0), colour);
	builder.store(Output::Varying(1), uv);
	builder.store(Output::Varying(2), position);
	builder.store(Output::Varying(3), ident);
	builder.finish()
}

/// The same, with a per-instance offset added to the position.
pub fn instanced_vertex_stage() -> Module {
	let mut builder = Builder::new(Stage::Vertex, "conformance-instanced-vertex");
	for location in 0..3 {
		builder.varying(location, Type::vec(4), render_shader::Interpolation::Smooth);
	}
	builder.varying_at(3, Type::vec(4), render_shader::Interpolation::Flat, render_shader::ir::Sampling::Pixel);
	let position = builder.load(Type::vec(4), Binding::Attribute { location: 0 });
	let colour = builder.load(Type::vec(4), Binding::Attribute { location: 1 });
	let uv = builder.load(Type::vec(4), Binding::Attribute { location: 2 });
	let ident = builder.load(Type::vec(4), Binding::Attribute { location: 3 });
	let offset = builder.load(Type::vec(4), Binding::Attribute { location: 4 });
	let moved = builder.assign(Type::vec(4), Op::Binary(render_shader::ir::BinaryOp::Add, position, offset));
	builder.store(Output::Position, moved);
	builder.store(Output::Varying(0), colour);
	builder.store(Output::Varying(1), uv);
	builder.store(Output::Varying(2), moved);
	builder.store(Output::Varying(3), ident);
	builder.finish()
}

/// The fragment stage every scene shares: the interpolated colour out, and the identity beside it.
pub fn fragment_stage() -> Module {
	let mut builder = Builder::new(Stage::Fragment, "conformance-fragment");
	for location in 0..3 {
		builder.varying(location, Type::vec(4), render_shader::Interpolation::Smooth);
	}
	builder.varying_at(3, Type::vec(4), render_shader::Interpolation::Flat, render_shader::ir::Sampling::Pixel);
	let colour = builder.load(Type::vec(4), Binding::Varying { location: 0 });
	builder.store(Output::Colour(0), colour);
	let ident = builder.load(Type::vec(4), Binding::Varying { location: 3 });
	let scalar = builder.assign(Type::f32(), Op::Extract(ident, 0));
	let integer = builder.assign(Type::Scalar(render_shader::ir::ScalarType::U32), Op::Unary(render_shader::ir::UnaryOp::Convert(render_shader::ir::ScalarType::U32), scalar));
	builder.store(Output::Integer(1), integer);
	builder.finish()
}

/// A fragment stage that samples one texture at the interpolated coordinate.
pub fn textured_fragment_stage(texture: u32, sampler: u32) -> Module {
	let mut builder = Builder::new(Stage::Fragment, "conformance-textured-fragment");
	for location in 0..3 {
		builder.varying(location, Type::vec(4), render_shader::Interpolation::Smooth);
	}
	builder.varying_at(3, Type::vec(4), render_shader::Interpolation::Flat, render_shader::ir::Sampling::Pixel);
	let uv = builder.load(Type::vec(4), Binding::Varying { location: 1 });
	let texel = builder.assign(Type::vec(4), Op::Sample { binding: Binding::Texture { texture, sampler }, coordinate: uv });
	builder.store(Output::Colour(0), texel);
	builder.finish()
}

/// What a scene asks the backend to do, beyond the mesh.
pub struct Plan {
	pub topology: Topology,
	pub cull: Cull,
	pub depth_test: Option<CompareOp>,
	pub depth_write: bool,
	pub samples: u32,
	pub blend: Vec<render3d::AttachmentBlend>,
	pub stencil: Option<render3d::StencilFace>,
	pub bias: (f32, f32, f32),
	pub alpha_to_coverage: bool,
	pub sample_mask: u32,
	pub vertex: Module,
	pub fragment: Module,
	/// How many indices or vertices the draw covers, and how many instances of it.
	pub count: u32,
	pub instances: u32,
	pub base_vertex: i32,
	pub first_instance: u32,
	pub restart: bool,
	/// The extent, the sample count of the attachments, and what the colour attachment starts as.
	pub width: u32,
	pub height: u32,
	pub clear: Vec4,
	pub depth_format: DepthFormat,
	/// Whether a second, INTEGER attachment is present for the identity the fragment stage writes.
	pub identity_attachment: bool,
	pub viewport: Option<Viewport>,
	/// The rectangle outside which the pass writes nothing. `None` is the whole target.
	pub scissor: Option<soft3d::Scissor>,
}

impl Default for Plan {
	fn default() -> Plan {
		Plan { topology: Topology::TriangleList, cull: Cull::None, depth_test: None, depth_write: false, samples: 1, blend: vec![render3d::AttachmentBlend { enabled: false, colour: render3d::BlendEquation::REPLACE, alpha: render3d::BlendEquation::REPLACE, write_mask: render3d::ColorWriteMask::ALL }], stencil: None, bias: (0.0, 0.0, 0.0), alpha_to_coverage: false, sample_mask: u32::MAX, vertex: vertex_stage(), fragment: fragment_stage(), count: 3, instances: 1, base_vertex: 0, first_instance: 0, restart: false, width: WIDTH, height: HEIGHT, clear: Vec4::new(0.0, 0.0, 0.0, 1.0), depth_format: DepthFormat::Depth32F, identity_attachment: true, viewport: None, scissor: None }
	}
}

/// What a scene renders into, and the questions it asks of it.
pub struct Frame {
	pub colour: Colour,
	pub identity: Option<Colour>,
	pub depth: DepthStencil,
	pub stats: Stats,
	width: u32,
	height: u32,
}

impl Frame {
	/// One pixel of the colour attachment, resolved. A MULTISAMPLED ATTACHMENT IS RESOLVED FIRST,
	/// because what a scene about coverage states is what a PIXEL comes to and not what one sample
	/// did.
	pub fn pixel(&self, x: u32, y: u32) -> Vec4 {
		if self.colour.samples == 1 {
			return self.colour.at(x, y, 0);
		}
		let mut total = Vec4::ZERO;
		for sample in 0..self.colour.samples {
			let value = self.colour.at(x, y, sample);
			total = Vec4::new(total.x + value.x, total.y + value.y, total.z + value.z, total.w + value.w);
		}
		let count = self.colour.samples as f32;
		Vec4::new(total.x / count, total.y / count, total.z / count, total.w / count)
	}

	/// One SAMPLE of the colour attachment, for the scenes that are about coverage per sample.
	pub fn sample(&self, x: u32, y: u32, sample: u32) -> Vec4 {
		self.colour.at(x, y, sample)
	}

	/// The identity written at a pixel, or zero where the attachment is absent.
	pub fn identity(&self, x: u32, y: u32) -> u32 {
		self.identity.as_ref().map(|attachment| attachment.at(x, y, 0).x as u32).unwrap_or(0)
	}

	/// The stored depth at a pixel, as the number a scene states its expectation in.
	///
	/// WHAT A FORMAT STORES IS NOT WHAT A FRAGMENT CARRIED, and that is the point of the depth
	/// formats being separate features: a `Depth16` buffer holds `round(d * 65535)` and a `Depth32F`
	/// holds the float. This answers both in `0..=1`, which is where a scene's expectation is
	/// written, and the scenes that are about the QUANTISATION ask for the stored form instead.
	pub fn depth_at(&self, x: u32, y: u32) -> f32 {
		render3d::depth::readback(self.depth.format, self.depth.depth_at(x, y, 0))
	}

	/// The stored depth exactly as the format holds it.
	pub fn depth_stored(&self, x: u32, y: u32) -> render3d::depth::Stored {
		self.depth.depth_at(x, y, 0)
	}

	pub fn stencil_at(&self, x: u32, y: u32) -> u8 {
		self.depth.stencil_at(x, y, 0)
	}

	/// Whether anything was drawn at a pixel, which is the commonest question a coverage scene asks.
	pub fn covered(&self, x: u32, y: u32) -> bool {
		self.pixel(x, y) != self.clear_value()
	}

	fn clear_value(&self) -> Vec4 {
		Vec4::new(0.0, 0.0, 0.0, 1.0)
	}

	/// How many pixels of the whole attachment differ from the clear.
	pub fn covered_pixels(&self) -> u32 {
		let mut count = 0;
		for y in 0..self.height {
			for x in 0..self.width {
				if self.covered(x, y) {
					count += 1;
				}
			}
		}
		count
	}

	pub fn centre(&self) -> (u32, u32) {
		(self.width / 2, self.height / 2)
	}
}

/// Render one scene under one plan.
pub fn render(scene: &Scene, plan: &Plan) -> Result<Frame, Trouble> {
	let mut colour = vec![Colour::new(plan.width, plan.height, plan.samples, false)];
	colour[0].fill(plan.clear);
	if plan.identity_attachment {
		colour.push(Colour::new(plan.width, plan.height, plan.samples, true));
	}
	let mut depth = DepthStencil::new(plan.width, plan.height, plan.samples, plan.depth_format);
	depth.clear(1.0, 0);
	let viewport = plan.viewport.unwrap_or(Viewport { x: 0.0, y: 0.0, width: plan.width as f32, height: plan.height as f32, min_depth: 0.0, max_depth: 1.0 });
	let mut prepared = prepare_for(plan, colour.len())?;
	let stats = {
		let mut attachments = Attachments { colour: &mut colour, depth_stencil: Some(&mut depth), viewport, scissor: plan.scissor };
		soft3d::frame::execute(&mut prepared, &mut attachments, scene)?
	};
	let identity = if plan.identity_attachment { colour.pop() } else { None };
	Ok(Frame { colour: colour.remove(0), identity, depth, stats, width: plan.width, height: plan.height })
}

/// Render two scenes into ONE set of attachments, each under its own plan.
///
/// A DEPTH TEST NEEDS SOMETHING TO TEST AGAINST, and a second `render` would clear the buffer: the
/// first draw is what leaves a depth and a stencil behind, and the second is the one the scene is
/// about. The extent, the sample count and the format come from the SECOND plan, because that is the
/// one the scene states its expectation under.
pub fn render_pair(first: &Scene, first_plan: &Plan, second: &Scene, second_plan: &Plan) -> Result<Frame, Trouble> {
	let mut colour = vec![Colour::new(second_plan.width, second_plan.height, second_plan.samples, false)];
	colour[0].fill(second_plan.clear);
	if second_plan.identity_attachment {
		colour.push(Colour::new(second_plan.width, second_plan.height, second_plan.samples, true));
	}
	let mut depth = DepthStencil::new(second_plan.width, second_plan.height, second_plan.samples, second_plan.depth_format);
	depth.clear(1.0, 0);
	let viewport = second_plan.viewport.unwrap_or(Viewport { x: 0.0, y: 0.0, width: second_plan.width as f32, height: second_plan.height as f32, min_depth: 0.0, max_depth: 1.0 });
	let mut stats = Stats::default();
	for (scene, plan) in [(first, first_plan), (second, second_plan)] {
		let mut prepared = prepare_for(plan, colour.len())?;
		let mut attachments = Attachments { colour: &mut colour, depth_stencil: Some(&mut depth), viewport, scissor: plan.scissor };
		stats = soft3d::frame::execute(&mut prepared, &mut attachments, scene)?;
	}
	let identity = if second_plan.identity_attachment { colour.pop() } else { None };
	Ok(Frame { colour: colour.remove(0), identity, depth, stats, width: second_plan.width, height: second_plan.height })
}

/// The prepared plan one draw needs, with a blend state for every attachment the pass has.
fn prepare_for(plan: &Plan, attachments: usize) -> Result<Prepared, Trouble> {
	let mut blend = plan.blend.clone();
	while blend.len() < attachments {
		blend.push(render3d::AttachmentBlend { enabled: false, colour: render3d::BlendEquation::REPLACE, alpha: render3d::BlendEquation::REPLACE, write_mask: render3d::ColorWriteMask::ALL });
	}
	let pipeline = Pipeline { state: render3d::command::PipelineState { topology: plan.topology, cull: plan.cull, depth_test: plan.depth_test, depth_write: plan.depth_write, samples: plan.samples, per_sample_shading: false }, vertex: plan.vertex.clone(), fragment: plan.fragment.clone(), blend, stencil: plan.stencil, depth_compare: plan.depth_test.unwrap_or(CompareOp::Always), depth_write: plan.depth_write, bias: plan.bias, alpha_to_coverage: plan.alpha_to_coverage, sample_mask: plan.sample_mask };
	let draw = Draw { pipeline: 0, topology: plan.topology, count: plan.count, instances: plan.instances, first_instance: plan.first_instance, base_vertex: plan.base_vertex, restart: plan.restart };
	Ok(soft3d::frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline], vec![draw], plan.width, plan.height)?)
}

/// A full-target triangle pair, which is what a scene that wants every pixel covered draws.
pub fn full_quad(red: f32, green: f32, blue: f32) -> Scene {
	Scene::new(vec![
		Vertex::at(-1.0, -1.0, 0.5).coloured(red, green, blue, 1.0).with_uv(0.0, 1.0),
		Vertex::at(1.0, -1.0, 0.5).coloured(red, green, blue, 1.0).with_uv(1.0, 1.0),
		Vertex::at(1.0, 1.0, 0.5).coloured(red, green, blue, 1.0).with_uv(1.0, 0.0),
		Vertex::at(-1.0, 1.0, 0.5).coloured(red, green, blue, 1.0).with_uv(0.0, 0.0),
	])
	.indexed16(vec![0, 1, 2, 0, 2, 3])
}

/// A quad at a stated depth, for the scenes about the depth test.
pub fn quad_at_depth(depth: f32, red: f32, green: f32, blue: f32) -> Scene {
	Scene::new(vec![
		Vertex::at(-1.0, -1.0, depth).coloured(red, green, blue, 1.0),
		Vertex::at(1.0, -1.0, depth).coloured(red, green, blue, 1.0),
		Vertex::at(1.0, 1.0, depth).coloured(red, green, blue, 1.0),
		Vertex::at(-1.0, 1.0, depth).coloured(red, green, blue, 1.0),
	])
	.indexed16(vec![0, 1, 2, 0, 2, 3])
}

/// Two values are equal within the tolerance a shaded comparison is made at.
pub fn close(left: f32, right: f32) -> bool {
	(left - right).abs() < 1.0 / 255.0
}

/// A whole colour within that tolerance.
pub fn colour_close(left: Vec4, red: f32, green: f32, blue: f32) -> bool {
	close(left.x, red) && close(left.y, green) && close(left.z, blue)
}

/// Read one texel THROUGH A DRAW, at the centre of a full quad whose every vertex carries the same
/// coordinate.
///
/// THE SAMPLING SCENES READ THE SAMPLER DIRECTLY AND THIS IS THE OTHER HALF. What a filter or a wrap
/// decides is one texel's value, so checking it through a rasteriser would let a failure be the
/// interpolator's - but a backend whose DRAW PATH never reached the sampler at all would pass every
/// one of those scenes while drawing an untextured frame. So each entry point is also read once from
/// inside a pass, which is where an application reads it.
pub fn drawn_texel(texture: soft3d::texture::Texture, sampler: soft3d::texture::Sampler, sampling: Sampling, coordinate: [f32; 3]) -> Result<Vec4, Trouble> {
	let corner = |x: f32, y: f32| Vertex::at(x, y, 0.5).with_uvw(coordinate[0], coordinate[1], coordinate[2]);
	let quad = Scene::new(vec![corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0)]).indexed16(vec![0, 1, 2, 0, 2, 3]).with_texture(texture, sampler).sampled_as(sampling);
	let plan = Plan { count: 6, fragment: textured_fragment_stage(0, 0), identity_attachment: false, ..Plan::default() };
	let frame = render(&quad, &plan)?;
	let (x, y) = frame.centre();
	Ok(frame.pixel(x, y))
}
