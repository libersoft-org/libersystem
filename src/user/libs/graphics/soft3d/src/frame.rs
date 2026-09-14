//! PREPARE AND EXECUTE, AND WHY THE BOUNDARY IS WHERE IT IS.
//!
//! EVERYTHING THAT CAN FAIL, VALIDATE OR ALLOCATE HAPPENS IN `prepare`. Resource admission, shader
//! validation, pipeline compatibility, command-list checking and the tile and bin scratch are all
//! settled before a frame starts; `execute` then walks a plan and allocates NOTHING in steady state.
//! Without that boundary, programmable shaders, pipeline creation, mip generation and the pass graph
//! each become a place a frame can start allocating - and the no-steady-state-allocation rule this
//! milestone holds the 2D side to becomes untestable on the 3D one.
//!
//! `execute` FULFILS THE `Submission` CONTRACT. It is the software backend's answer to a submitted
//! list: it runs to completion and reports, which is the "already-completed ticket" shape the
//! submission model was written to admit beside a GPU backend's pending one.
//!
//! THE DERIVATIVES A TEXTURE LOD NEEDS ARE TAKEN FROM THE TRIANGLE'S OWN PLANE at `(x+1, y)` and
//! `(x, y+1)`. That is the SAME quantity a 2x2 quad estimates: within one primitive a quad's lanes
//! interpolate the same plane, so the differences are identical. A quad exists on hardware because
//! neighbouring pixels may belong to different primitives and a lane must be filled somehow; a
//! software rasteriser that owns the triangle already has the exact answer for the primitive it is
//! shading, and helper lanes are what it would be approximating.

use alloc::vec::Vec;

use render_math::{Vec4, Viewport};
use render_shader::ir::{Interpolation, Sampling};
use render_shader::{Module, Stage};
use render3d::blend::AttachmentBlend;
use render3d::command::{Command, PipelineState, Topology};
use render3d::depth::StencilFace;
use render3d::{CompareOp, Error, Render3DLimits};

use crate::clip::{self, Clipped, Varyings};
use crate::geometry::{self, Indices, Primitive};
use crate::interp;
use crate::interpreter::{self, Resources};
use crate::pass::{Colour, DepthStencil, Fragment};
use crate::raster::{self, Bins, Facing, Setup};
use crate::value::Val;

/// A pipeline, already validated.
#[derive(Clone, PartialEq, Debug)]
pub struct Pipeline {
	pub state: PipelineState,
	pub vertex: Module,
	pub fragment: Module,
	/// One blend state per colour attachment.
	pub blend: Vec<AttachmentBlend>,
	pub stencil: Option<StencilFace>,
	pub depth_compare: CompareOp,
	pub depth_write: bool,
	/// `(constant_factor, slope_factor, clamp)` of the frozen depth-bias equation.
	pub bias: (f32, f32, f32),
	/// Whether a fragment's own alpha narrows its coverage. A PIPELINE STATE AND NOT A SHADER ONE:
	/// it is what makes alpha-tested foliage antialias without sorting, and the shader that produced
	/// the alpha has no way to know whether the pass wants it.
	pub alpha_to_coverage: bool,
	/// The pipeline's static sample mask, ANDed with the rasteriser's coverage.
	pub sample_mask: u32,
}

/// What a draw reads its vertices from, and what its shaders read their resources from.
pub trait Source {
	fn attribute(&self, location: u32, vertex: u32, instance: u32) -> Option<Val>;
	fn uniform(&self, block: u32, member: u32) -> Option<Val>;
	fn sample(&self, texture: u32, sampler: u32, coordinate: &Val) -> Option<Val>;
	/// The indices of the current draw, if it is indexed.
	fn indices(&self) -> Indices<'_>;
}

/// One draw, as the plan holds it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Draw {
	pub pipeline: u32,
	pub topology: Topology,
	pub count: u32,
	pub instances: u32,
	pub first_instance: u32,
	pub base_vertex: i32,
	pub restart: bool,
}

/// What a frame writes into.
pub struct Attachments<'a> {
	pub colour: &'a mut [Colour],
	pub depth_stencil: Option<&'a mut DepthStencil>,
	pub viewport: Viewport,
}

/// The scratch a frame reuses. CLEARED, NEVER REALLOCATED, which is what makes a steady-state frame
/// allocate nothing - and the clipper's workspace is here for the same reason: it is a few kilobytes
/// and belongs to the frame, not to the primitive.
#[derive(Default)]
struct Scratch {
	primitives: Vec<Primitive>,
	setups: Vec<Binned>,
	segments: Vec<Segment>,
	points: Vec<Dot>,
	/// The primitive assembler's run of indices since the last restart.
	run: Vec<u32>,
	/// One interpreter machine per stage, reused across every vertex and every fragment.
	vertex_machine: interpreter::Machine,
	fragment_machine: interpreter::Machine,
	// AN `Option<Box<_>>` AND NOT A `Box<_>`. `core::mem::take` on a `Box` builds a fresh default
	// one - an allocation per primitive, which is precisely what this boundary exists to prevent -
	// while taking an `Option` leaves `None` and allocates nothing. The first primitive of the first
	// frame builds it; nothing after that does.
	clip_work: Option<alloc::boxed::Box<clip::Workspace>>,
	clipped: Clipped,
}

/// One vertex after the vertex stage. `Copy`, because a per-vertex `Vec` is a per-primitive
/// allocation three times over.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Shaded {
	position: Vec4,
	point_size: f32,
	smooth: Varyings,
	noperspective: Varyings,
	flat: Varyings,
}

/// A triangle that survived setup, with what the fragment stage needs.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Binned {
	setup: Setup,
	smooth: [Varyings; 3],
	noperspective: [Varyings; 3],
	flat: Varyings,
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct Segment {
	from: (crate::Subpixel, crate::Subpixel),
	to: (crate::Subpixel, crate::Subpixel),
	depth: [f32; 2],
	inverse_w: [f32; 2],
	smooth: [Varyings; 2],
	noperspective: [Varyings; 2],
	flat: Varyings,
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct Dot {
	centre: (crate::Subpixel, crate::Subpixel),
	size: u32,
	depth: f32,
	values: Varyings,
	flat: Varyings,
}

/// A validated plan. NOTHING HERE CAN FAIL AT SUBMISSION.
pub struct Prepared {
	pipelines: Vec<Pipeline>,
	/// Where each fragment varying lives in the three interpolated arrays, per pipeline. COMPUTED
	/// ONCE: it is a property of the pipeline, and rebuilding it per draw would allocate per draw.
	maps: Vec<Vec<(u32, Interpolation, usize, usize)>>,
	draws: Vec<Draw>,
	bins: Bins,
	scratch: Scratch,
	limits: Render3DLimits,
}

/// What one execution did, which is what a report and a benchmark both want.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Stats {
	pub primitives: u32,
	pub clipped: u32,
	pub culled: u32,
	pub fragments: u32,
	pub samples_written: u32,
	pub discarded: u32,
}

/// Validate everything that can be validated, and reserve everything that has to be reserved.
///
/// THE SHADERS ARE VALIDATED HERE AND NOT AT THE DRAW. A module is checked ONCE with the whole
/// program in front of it, which is where the position dependency slice can be walked at all.
pub fn prepare(limits: Render3DLimits, pipelines: Vec<Pipeline>, draws: Vec<Draw>, width: u32, height: u32) -> Result<Prepared, Error> {
	if width == 0 || height == 0 || width > crate::MAX_RASTER_EXTENT || height > crate::MAX_RASTER_EXTENT {
		return Err(Error::LimitExceeded { limit: "raster extent", ceiling: crate::MAX_RASTER_EXTENT as u64, asked: width.max(height) as u64 });
	}
	let shader_limits = render_shader::validate::ShaderLimits { max_instructions: limits.max_shader_instructions, max_loop_depth: limits.max_shader_loop_depth, max_loop_trips: limits.max_shader_loop_trip };
	for pipeline in &pipelines {
		if pipeline.vertex.stage != Stage::Vertex || pipeline.fragment.stage != Stage::Fragment {
			return Err(Error::InvalidShader { reason: "a pipeline whose stages are the wrong way round" });
		}
		render_shader::validate(&pipeline.vertex, &shader_limits).map_err(|_| Error::InvalidShader { reason: "a vertex module the IR refuses" })?;
		render_shader::validate(&pipeline.fragment, &shader_limits).map_err(|_| Error::InvalidShader { reason: "a fragment module the IR refuses" })?;
		// THE TWO STAGES MUST AGREE ABOUT THEIR VARYINGS. A fragment stage that reads a location the
		// vertex stage does not write reads whatever the interpolator was left holding, which is a
		// surface that is right until the draw before it changes.
		for varying in &pipeline.fragment.varyings {
			let written = pipeline.vertex.varyings.iter().any(|other| other.location == varying.location && other.kind == varying.kind);
			if !written {
				return Err(Error::InvalidShader { reason: "a fragment stage reading a varying location the vertex stage does not write with the same type" });
			}
		}
		// THE VARYINGS MUST FIT THE BACKEND'S INLINE STORAGE, refused HERE where a caller can do
		// something about it rather than at the first draw. The bound is what keeps the clipper and
		// the interpolator allocation-free, and a shader that exceeded it silently would be shaded
		// from values the vertex stage did not write.
		let mut totals = [0_usize; 3];
		for varying in &pipeline.vertex.varyings {
			let slot = match varying.interpolation {
				Interpolation::Smooth => 0,
				Interpolation::NoPerspective => 1,
				Interpolation::Flat => 2,
			};
			totals[slot] += crate::value::Val::words_in(&varying.kind);
		}
		if let Some(over) = totals.iter().find(|total| **total > clip::MAX_VARYING_COMPONENTS) {
			return Err(Error::LimitExceeded { limit: "varying components", ceiling: clip::MAX_VARYING_COMPONENTS as u64, asked: *over as u64 });
		}
		// AND THE SHADING RATE IS THE SHADER'S. A `sample`-qualified input cannot be shaded once per
		// pixel, and a pipeline that says otherwise contradicts its own fragment stage.
		let per_sample = pipeline.fragment.varyings.iter().any(|varying| varying.sampling == Sampling::Sample);
		if per_sample != pipeline.state.per_sample_shading {
			return Err(Error::IncompatiblePipeline { reason: "a pipeline whose shading rate disagrees with its fragment stage's inputs" });
		}
	}
	for draw in &draws {
		if draw.pipeline as usize >= pipelines.len() {
			return Err(Error::InvalidRenderState { reason: "a draw naming a pipeline the plan does not hold" });
		}
		if draw.instances == 0 {
			return Err(Error::InvalidMesh { reason: render3d::error::MeshFault::TooFewVertices { topology: "an instanced draw", needs: 1, has: 0 } });
		}
	}
	let maps: Vec<Vec<(u32, Interpolation, usize, usize)>> = pipelines.iter().map(varying_map).collect();
	Ok(Prepared { pipelines, maps, draws, bins: Bins::new(width, height), scratch: Scratch::default(), limits })
}

impl Prepared {
	pub fn pipelines(&self) -> &[Pipeline] {
		&self.pipelines
	}

	pub fn draws(&self) -> &[Draw] {
		&self.draws
	}

	/// The commands this plan is, for a caller that wants to see what was recorded.
	pub fn command_count(&self) -> usize {
		self.draws.len()
	}

	/// Reuse this plan against a differently sized target.
	pub fn resize(&mut self, width: u32, height: u32) {
		self.bins.resize(width, height);
	}

	/// The bytes this plan OWNS, for the caller to charge against its process Domain.
	///
	/// THE LIBRARY REPORTS AND THE PROCESS CHARGES, the same division the 2D side uses. This crate is
	/// `no_std` and has no way to reach a Domain; a renderer that tried would be a renderer that
	/// could only run inside one process model. What it CAN do is say exactly what it reserved, so
	/// the number the process charges is a measurement rather than an estimate.
	///
	/// AND IT COUNTS ONLY WHAT THIS PLAN OWNS. The colour and depth attachments are the caller's -
	/// `execute` borrows them - so charging for them here would charge for them twice.
	pub fn reserved_bytes(&self) -> usize {
		let of = |capacity: usize, each: usize| capacity * each;
		of(self.pipelines.capacity(), core::mem::size_of::<Pipeline>()) + self.maps.iter().map(|map| of(map.capacity(), core::mem::size_of::<(u32, Interpolation, usize, usize)>())).sum::<usize>() + of(self.draws.capacity(), core::mem::size_of::<Draw>()) + self.bins.reserved_bytes() + of(self.scratch.primitives.capacity(), core::mem::size_of::<Primitive>()) + of(self.scratch.setups.capacity(), core::mem::size_of::<Binned>()) + of(self.scratch.segments.capacity(), core::mem::size_of::<Segment>()) + of(self.scratch.points.capacity(), core::mem::size_of::<Dot>()) + of(self.scratch.run.capacity(), core::mem::size_of::<u32>()) + self.scratch.clip_work.as_ref().map_or(0, |_| core::mem::size_of::<clip::Workspace>()) + core::mem::size_of::<Clipped>()
	}

	/// How much scratch this plan is holding, so a fixture can assert that a steady-state frame
	/// allocates nothing: the number is what grew on the first frame and must not grow again.
	pub fn scratch_capacity(&self) -> usize {
		self.scratch.primitives.capacity() + self.scratch.setups.capacity() + self.scratch.segments.capacity() + self.scratch.points.capacity()
	}
}

/// Run a prepared plan. ALLOCATES NOTHING IN STEADY STATE: every buffer it uses is cleared rather
/// than freed, and the first frame is what sizes them.
pub fn execute(prepared: &mut Prepared, attachments: &mut Attachments<'_>, source: &dyn Source) -> Result<Stats, Error> {
	let mut stats = Stats::default();
	let (width, height) = match attachments.colour.first() {
		Some(first) => (first.width, first.height),
		None => return Err(Error::TargetMismatch { reason: render3d::error::AttachmentFault::TooMany { count: 0, ceiling: prepared.limits.max_colour_attachments } }),
	};
	// EVERY ATTACHMENT SHARES ONE EXTENT AND ONE SAMPLE COUNT. A pass has one set of fragments.
	let samples = attachments.colour[0].samples;
	for attachment in attachments.colour.iter() {
		if attachment.width != width || attachment.height != height || attachment.samples != samples {
			return Err(Error::TargetMismatch { reason: render3d::error::AttachmentFault::ExtentMismatch { width: attachment.width, height: attachment.height, expected_width: width, expected_height: height } });
		}
	}
	if let Some(buffer) = attachments.depth_stencil.as_deref() {
		if buffer.width != width || buffer.height != height || buffer.samples != samples {
			return Err(Error::TargetMismatch { reason: render3d::error::AttachmentFault::SampleCountMismatch { samples: buffer.samples, expected: samples } });
		}
	}
	prepared.bins.resize(width, height);
	prepared.bins.reset_depth();

	// THE PLAN IS TAKEN OUT RATHER THAN CLONED. A `Pipeline` holds two shader modules; cloning one
	// per draw would allocate several vectors per draw, which is exactly what this boundary exists
	// to prevent.
	let draws = core::mem::take(&mut prepared.draws);
	let pipelines = core::mem::take(&mut prepared.pipelines);
	let maps = core::mem::take(&mut prepared.maps);
	let mut outcome = Ok(());
	for draw in &draws {
		let pipeline = &pipelines[draw.pipeline as usize];
		let map = &maps[draw.pipeline as usize];
		outcome = draw_one(prepared, attachments, source, draw, pipeline, map, width, height, samples, &mut stats);
		if outcome.is_err() {
			break;
		}
	}
	prepared.draws = draws;
	prepared.pipelines = pipelines;
	prepared.maps = maps;
	outcome.map(|()| stats)
}

#[allow(clippy::too_many_arguments)]
fn draw_one(prepared: &mut Prepared, attachments: &mut Attachments<'_>, source: &dyn Source, draw: &Draw, pipeline: &Pipeline, map: &[(u32, Interpolation, usize, usize)], width: u32, height: u32, samples: u32, stats: &mut Stats) -> Result<(), Error> {
	if pipeline.state.samples != samples {
		return Err(Error::IncompatiblePipeline { reason: "the pipeline's sample count disagrees with the pass's attachments" });
	}
	if pipeline.blend.len() < attachments.colour.len() {
		return Err(Error::IncompatiblePipeline { reason: "a pipeline with fewer blend states than the pass has colour attachments" });
	}
	for instance in 0..draw.instances {
		let instance_index = draw.first_instance + instance;
		let mut run = core::mem::take(&mut prepared.scratch.run);
		let assembled = geometry::assemble(draw.topology, source.indices(), draw.count, draw.base_vertex, draw.restart, &mut prepared.scratch.primitives, &mut run);
		prepared.scratch.run = run;
		assembled?;
		let primitives = core::mem::take(&mut prepared.scratch.primitives);
		stats.primitives += primitives.len() as u32;

		prepared.bins.clear();
		prepared.scratch.setups.clear();
		prepared.scratch.segments.clear();
		prepared.scratch.points.clear();

		for primitive in &primitives {
			match primitive {
				Primitive::Triangle { vertices, index } => {
					stage_triangle(prepared, pipeline, source, *vertices, *index, draw.topology, instance_index, &attachments.viewport, width, height, stats)?;
				}
				Primitive::Line { vertices, index } => {
					stage_line(prepared, pipeline, source, *vertices, *index, draw.topology, instance_index, &attachments.viewport, stats)?;
				}
				Primitive::Point { vertex, index } => {
					stage_point(prepared, pipeline, source, *vertex, *index, draw.topology, instance_index, &attachments.viewport, stats)?;
				}
			}
		}
		prepared.scratch.primitives = primitives;
		shade_bins(prepared, attachments, source, pipeline, map, width, height, samples, stats)?;
		shade_lines(prepared, attachments, source, pipeline, map, width, height, samples, stats)?;
		shade_points(prepared, attachments, source, pipeline, map, width, height, samples, stats)?;
	}
	Ok(())
}

/// Run the vertex stage for one vertex and split its varyings by declared qualifier.
fn run_vertex(pipeline: &Pipeline, source: &dyn Source, vertex: u32, instance: u32, machine: &mut interpreter::Machine) -> Result<Shaded, Error> {
	let bindings = VertexBindings { source, vertex, instance };
	interpreter::execute_into(&pipeline.vertex, &bindings, machine).map_err(fault)?;
	let outputs = machine.outputs();
	let position = outputs.position.as_ref().ok_or(Error::InvalidShader { reason: "a vertex stage that produced no position" })?;
	let mut shaded = Shaded { position: Vec4::new(position.f32_at(0), position.f32_at(1), position.f32_at(2), position.f32_at(3)), point_size: outputs.point_size.unwrap_or(1.0), smooth: Varyings::EMPTY, noperspective: Varyings::EMPTY, flat: Varyings::EMPTY };
	// THE ORDER IS THE DECLARATION ORDER, so the fragment stage reads back what the vertex stage
	// wrote rather than whatever the store happened to run first.
	for varying in &pipeline.vertex.varyings {
		let Some((_, value)) = outputs.varyings.iter().find(|(location, _)| *location == varying.location) else {
			continue;
		};
		let room = match varying.interpolation {
			Interpolation::Smooth => value.f32_components().all(|component| shaded.smooth.push(component)),
			Interpolation::NoPerspective => value.f32_components().all(|component| shaded.noperspective.push(component)),
			Interpolation::Flat => value.f32_components().all(|component| shaded.flat.push(component)),
		};
		if !room {
			// REFUSED AND NOT TRUNCATED, and `prepare` has already refused the pipeline that could
			// reach here - this is the arithmetic's own guard rather than a case a prepared plan has.
			return Err(Error::LimitExceeded { limit: "varying components", ceiling: clip::MAX_VARYING_COMPONENTS as u64, asked: clip::MAX_VARYING_COMPONENTS as u64 + 1 });
		}
	}
	Ok(shaded)
}

fn fault(fault: interpreter::Fault) -> Error {
	match fault {
		interpreter::Fault::IndexOutOfRange { .. } => Error::InvalidShader { reason: "a shader indexed outside an array at runtime" },
		interpreter::Fault::MissingBinding => Error::InvalidShader { reason: "a shader read a binding the frame does not supply" },
		interpreter::Fault::SampleFailed => Error::InvalidTexture { reason: "a texture read the sampler refused" },
		_ => Error::InvalidShader { reason: "a shader the interpreter refused" },
	}
}

struct VertexBindings<'a> {
	source: &'a dyn Source,
	vertex: u32,
	instance: u32,
}

impl Resources for VertexBindings<'_> {
	fn uniform(&self, block: u32, member: u32) -> Option<Val> {
		self.source.uniform(block, member)
	}

	fn attribute(&self, location: u32) -> Option<Val> {
		self.source.attribute(location, self.vertex, self.instance)
	}

	fn varying(&self, _location: u32) -> Option<Val> {
		// A VERTEX STAGE HAS NO VARYING INPUTS. It produces them.
		None
	}

	fn built_in(&self, which: render_shader::ir::BuiltIn) -> Option<Val> {
		use render_shader::ir::BuiltIn;
		match which {
			BuiltIn::VertexIndex => Some(Val::scalar_u32(self.vertex)),
			BuiltIn::InstanceIndex => Some(Val::scalar_u32(self.instance)),
			_ => None,
		}
	}

	fn sample(&self, texture: u32, sampler: u32, coordinate: &Val) -> Option<Val> {
		self.source.sample(texture, sampler, coordinate)
	}
}

struct FragmentBindings<'a> {
	source: &'a dyn Source,
	smooth: &'a [f32],
	noperspective: &'a [f32],
	flat: &'a [f32],
	locations: &'a [(u32, Interpolation, usize, usize)],
	front_facing: bool,
	coordinate: [f32; 4],
	sample_index: u32,
	sample_mask: u32,
}

impl Resources for FragmentBindings<'_> {
	fn uniform(&self, block: u32, member: u32) -> Option<Val> {
		self.source.uniform(block, member)
	}

	fn attribute(&self, _location: u32) -> Option<Val> {
		// A FRAGMENT STAGE HAS NO ATTRIBUTES. It has varyings, and the difference is what the
		// interpolator did between them.
		None
	}

	fn varying(&self, location: u32) -> Option<Val> {
		let (_, interpolation, offset, count) = *self.locations.iter().find(|(name, ..)| *name == location)?;
		let source = match interpolation {
			Interpolation::Smooth => self.smooth,
			Interpolation::NoPerspective => self.noperspective,
			Interpolation::Flat => self.flat,
		};
		let slice = source.get(offset..offset + count)?;
		Some(Val::vector_f32(slice))
	}

	fn built_in(&self, which: render_shader::ir::BuiltIn) -> Option<Val> {
		use render_shader::ir::BuiltIn;
		match which {
			BuiltIn::FragmentCoordinate => Some(Val::vector_f32(&self.coordinate)),
			BuiltIn::FrontFacing => Some(Val::scalar_bool(self.front_facing)),
			BuiltIn::SampleIndex => Some(Val::scalar_u32(self.sample_index)),
			BuiltIn::SampleMaskIn => Some(Val::scalar_u32(self.sample_mask)),
			_ => None,
		}
	}

	fn sample(&self, texture: u32, sampler: u32, coordinate: &Val) -> Option<Val> {
		self.source.sample(texture, sampler, coordinate)
	}
}

/// Where each fragment varying lives in the three interpolated arrays.
fn varying_map(pipeline: &Pipeline) -> Vec<(u32, Interpolation, usize, usize)> {
	let mut out = Vec::new();
	let (mut smooth, mut noperspective, mut flat) = (0, 0, 0);
	for varying in &pipeline.vertex.varyings {
		let count = crate::value::Val::words_in(&varying.kind);
		let offset = match varying.interpolation {
			Interpolation::Smooth => {
				let at = smooth;
				smooth += count;
				at
			}
			Interpolation::NoPerspective => {
				let at = noperspective;
				noperspective += count;
				at
			}
			Interpolation::Flat => {
				let at = flat;
				flat += count;
				at
			}
		};
		out.push((varying.location, varying.interpolation, offset, count));
	}
	out
}

#[allow(clippy::too_many_arguments)]
fn stage_triangle(prepared: &mut Prepared, pipeline: &Pipeline, source: &dyn Source, vertices: [u32; 3], index: u32, topology: Topology, instance: u32, viewport: &Viewport, width: u32, height: u32, stats: &mut Stats) -> Result<(), Error> {
	let mut machine = core::mem::take(&mut prepared.scratch.vertex_machine);
	let shaded = {
		let mut run = |vertex: u32| run_vertex(pipeline, source, vertex, instance, &mut machine);
		let outcome = (run(vertices[0]), run(vertices[1]), run(vertices[2]));
		match outcome {
			(Ok(a), Ok(b), Ok(c)) => [a, b, c],
			(first, second, third) => {
				prepared.scratch.vertex_machine = machine;
				first?;
				second?;
				third?;
				return Ok(());
			}
		}
	};
	prepared.scratch.vertex_machine = machine;
	// THE FLAT VALUE COMES FROM THE PROVOKING VERTEX OF THE PRIMITIVE AS ASSEMBLED, captured before
	// the clip - a clipped triangle is the same triangle, and taking the value from a vertex the
	// clipper invented would make an object id depend on where the camera is.
	let provoking = geometry::provoking(topology, index);
	let which = vertices.iter().position(|vertex| *vertex == provoking).unwrap_or(0);
	let flat = shaded[which].flat.clone();

	let corners = [
		clip::Vertex::new(shaded[0].position).with_smooth(shaded[0].smooth).with_noperspective(shaded[0].noperspective),
		clip::Vertex::new(shaded[1].position).with_smooth(shaded[1].smooth).with_noperspective(shaded[1].noperspective),
		clip::Vertex::new(shaded[2].position).with_smooth(shaded[2].smooth).with_noperspective(shaded[2].noperspective),
	];
	// The workspace and the result both belong to the FRAME, so a primitive allocates nothing.
	let mut work = prepared.scratch.clip_work.take().unwrap_or_default();
	let outcome = clip::clip_triangle_into(&corners, flat, &mut work, &mut prepared.scratch.clipped);
	prepared.scratch.clip_work = Some(work);
	outcome?;
	let clipped = prepared.scratch.clipped;
	if clipped.is_empty() {
		stats.culled += 1;
		return Ok(());
	}
	if clipped.vertices().len() > 3 {
		stats.clipped += 1;
	}
	for index in 0..clipped.triangle_count() {
		let Some(triangle) = clipped.triangle(index) else { continue };
		let mut window = [render_math::Vec3::ZERO; 3];
		let mut inverse_w = [0.0_f32; 3];
		for (slot, vertex) in triangle.iter().enumerate() {
			let (point, one_over_w) = raster::project(clipped.vertices()[*vertex].position, viewport)?;
			window[slot] = point;
			inverse_w[slot] = one_over_w;
		}
		let Some(setup) = raster::setup(window, inverse_w, width, height)? else { continue };
		if raster::culled(setup.facing, pipeline.state.cull) {
			stats.culled += 1;
			continue;
		}
		if setup.bounds.is_empty() {
			continue;
		}
		let pick = |slot: usize| clipped.vertices()[triangle[setup.source[slot]]];
		let binned = Binned { setup, smooth: [pick(0).smooth, pick(1).smooth, pick(2).smooth], noperspective: [pick(0).noperspective, pick(1).noperspective, pick(2).noperspective], flat: clipped.flat };
		prepared.bins.insert(prepared.scratch.setups.len() as u32, &binned.setup.bounds);
		prepared.scratch.setups.push(binned);
	}
	Ok(())
}

#[allow(clippy::too_many_arguments)]
fn shade_bins(prepared: &mut Prepared, attachments: &mut Attachments<'_>, source: &dyn Source, pipeline: &Pipeline, map: &[(u32, Interpolation, usize, usize)], width: u32, height: u32, samples: u32, stats: &mut Stats) -> Result<(), Error> {
	let setups = core::mem::take(&mut prepared.scratch.setups);
	let mut machine = core::mem::take(&mut prepared.scratch.fragment_machine);
	let mut outcome = Ok(());
	'outer: for tile_y in 0..prepared.bins.down() {
		for tile_x in 0..prepared.bins.across() {
			let area = prepared.bins.tile_box(tile_x, tile_y, width, height);
			let mut drew_anything = false;
			// HIERARCHICAL DEPTH: the nearest depth anything in this tile can have, kept per tile and
			// compared against the triangle's own NEAREST vertex. A triangle entirely behind what the
			// tile already holds cannot produce a surviving fragment under a `Less` or `LessOrEqual`
			// test, so it is rejected before a single sample is tested. THE TEST IS CONSERVATIVE: it
			// rejects only what the per-sample test would reject anyway, so turning it off changes
			// the frame's speed and not its picture.
			let tile_far = prepared.bins.tile_far(tile_x, tile_y);
			for triangle in prepared.bins.tile(tile_x, tile_y) {
				let binned = &setups[*triangle as usize];
				if matches!(pipeline.depth_compare, CompareOp::Less | CompareOp::LessOrEqual) {
					let nearest = binned.setup.depth.iter().fold(f32::INFINITY, |held: f32, depth| held.min(*depth));
					if nearest > tile_far {
						continue;
					}
				}
				let bounds = binned.setup.bounds;
				let x0 = bounds.x0.max(area.x0);
				let x1 = bounds.x1.min(area.x1);
				let y0 = bounds.y0.max(area.y0);
				let y1 = bounds.y1.min(area.y1);
				for y in y0..y1 {
					for x in x0..x1 {
						let coverage = raster::coverage(&binned.setup, x, y, samples)?;
						if coverage == 0 {
							continue;
						}
						drew_anything = true;
						outcome = shade_pixel(attachments, source, pipeline, map, binned, x, y, coverage, samples, stats, &mut machine);
						if outcome.is_err() {
							break 'outer;
						}
					}
				}
			}
			// THE BOUND IS READ BACK FROM THE DEPTH BUFFER, once per tile, after the tile is drawn.
			// A bound inferred from the triangles that covered it would only be valid when one of
			// them covered the WHOLE tile, which after clipping almost never happens: a clipped
			// polygon is fan-triangulated and each piece covers part of it.
			if pipeline.depth_write && drew_anything {
				if let Some(buffer) = attachments.depth_stencil.as_deref() {
					let furthest = buffer.furthest_in(area.x0.max(0) as u32, area.y0.max(0) as u32, area.x1.max(0) as u32, area.y1.max(0) as u32);
					prepared.bins.narrow_tile_far(tile_x, tile_y, furthest);
				}
			}
		}
	}
	prepared.scratch.setups = setups;
	prepared.scratch.fragment_machine = machine;
	outcome
}

#[allow(clippy::too_many_arguments)]
fn shade_pixel(attachments: &mut Attachments<'_>, source: &dyn Source, pipeline: &Pipeline, map: &[(u32, Interpolation, usize, usize)], binned: &Binned, x: i64, y: i64, coverage: u32, samples: u32, stats: &mut Stats, machine: &mut interpreter::Machine) -> Result<(), Error> {
	let centre = |dx: i64, dy: i64| (crate::Subpixel((x + dx) * crate::fixed::SUBPIXEL_ONE + crate::fixed::SUBPIXEL_HALF), crate::Subpixel((y + dy) * crate::fixed::SUBPIXEL_ONE + crate::fixed::SUBPIXEL_HALF));
	let weights = raster::barycentric(&binned.setup, centre(0, 0));
	// INTERPOLATED INTO FIXED ARRAYS, not into vectors: a `Vec` per fragment is an allocation per
	// fragment, which is several million of them in a frame.
	let interpolate = |weights: [f32; 3]| -> (Varyings, Varyings) {
		let mut smooth = Varyings::EMPTY;
		for index in 0..binned.smooth[0].len() {
			smooth.push(interp::smooth(weights, binned.setup.inverse_w, [binned.smooth[0].as_slice()[index], binned.smooth[1].as_slice()[index], binned.smooth[2].as_slice()[index]]));
		}
		let mut screen_linear = Varyings::EMPTY;
		for index in 0..binned.noperspective[0].len() {
			screen_linear.push(interp::noperspective(weights, [binned.noperspective[0].as_slice()[index], binned.noperspective[1].as_slice()[index], binned.noperspective[2].as_slice()[index]]));
		}
		(smooth, screen_linear)
	};
	let (smooth, noperspective) = interpolate(weights);
	let depth = interp::depth(weights, binned.setup.depth);
	// THE DEPTH BIAS IS APPLIED AFTER THE DEPTH IS COMPUTED AND BEFORE THE TEST AND THE WRITE, which
	// is the frozen equation's own ordering. The slope is the maximum of the two gradients over the
	// primitive, taken from the plane the same way the derivatives below are.
	let depth = if pipeline.bias.0 != 0.0 || pipeline.bias.1 != 0.0 {
		let (dx_weights, dy_weights) = (raster::barycentric(&binned.setup, centre(1, 0)), raster::barycentric(&binned.setup, centre(0, 1)));
		let slope = (interp::depth(dx_weights, binned.setup.depth) - depth).abs().max((interp::depth(dy_weights, binned.setup.depth) - depth).abs());
		let format = attachments.depth_stencil.as_deref().map(|buffer| buffer.format).unwrap_or(render3d::depth::DepthFormat::Depth32F);
		render3d::depth::bias(format, depth, slope, pipeline.bias.0, pipeline.bias.1, pipeline.bias.2)
	} else {
		depth
	};

	let bindings = FragmentBindings { source, smooth: smooth.as_slice(), noperspective: noperspective.as_slice(), flat: binned.flat.as_slice(), locations: map, front_facing: binned.setup.facing == Facing::Front, coordinate: [x as f32 + 0.5, y as f32 + 0.5, depth, interp::inverse_w_at(weights, binned.setup.inverse_w)], sample_index: 0, sample_mask: coverage };
	interpreter::execute_into(&pipeline.fragment, &bindings, machine).map_err(fault)?;
	stats.fragments += 1;
	if machine.outputs().discarded {
		stats.discarded += 1;
		return Ok(());
	}
	let depth = machine.outputs().depth.unwrap_or(depth);
	let written = write_outputs(attachments, pipeline, machine.outputs(), x as u32, y as u32, coverage, depth, binned.setup.facing, samples)?;
	stats.samples_written += written;
	Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_outputs(attachments: &mut Attachments<'_>, pipeline: &Pipeline, outputs: &interpreter::Outputs, x: u32, y: u32, coverage: u32, depth: f32, facing: Facing, _samples: u32) -> Result<u32, Error> {
	// A SHADER-WRITTEN DEPTH REPLACED THE INTERPOLATED ONE AT THE CALLER, which is what a
	// depth-writing shader is for; the bias was applied to the interpolated value and is not
	// reapplied.
	let stencil = pipeline.stencil.as_ref().map(|face| (face, facing == Facing::Front));
	let mut written = 0;
	// MULTIPLE COLOUR ATTACHMENTS: each with its OWN blend state and write mask, because a pass that
	// writes colour to one and object ids to another must blend the first and not the second.
	//
	// THE DEPTH AND STENCIL TEST RUNS ONCE, against the first attachment, and the rest follow its
	// answer. Running it per attachment would test the same fragment several times and write the
	// depth buffer more than once for one fragment.
	let mut depth_stencil = attachments.depth_stencil.take();
	for (index, colour) in attachments.colour.iter_mut().enumerate() {
		let value = if colour.integer {
			let identity = outputs.integer.iter().find(|(slot, _)| *slot as usize == index).map(|(_, value)| *value).unwrap_or(0);
			Vec4::new(identity as f32, 0.0, 0.0, 1.0)
		} else {
			match outputs.colour.iter().find(|(slot, _)| *slot as usize == index) {
				Some((_, value)) => Vec4::new(value.f32_at(0), value.f32_at(1), value.f32_at(2), value.f32_at(3)),
				// AN ATTACHMENT THE SHADER DID NOT WRITE IS LEFT ALONE, not cleared: a pass with two
				// attachments whose shader writes one is ordinary, and zeroing the other would make
				// it a pass that erases what it does not touch.
				None => continue,
			}
		};
		let fragment = Fragment {
			x,
			y,
			coverage,
			depth,
			colour: value,
			blend: &pipeline.blend[index],
			depth_compare: pipeline.depth_compare,
			depth_write: pipeline.depth_write,
			stencil,
			// THE SHADER'S MASK AND THE PIPELINE'S ARE BOTH APPLIED, and neither replaces the
			// other: the pipeline's is the pass's decision and the shader's is the fragment's.
			sample_mask: outputs.sample_mask.unwrap_or(u32::MAX) & pipeline.sample_mask,
			alpha_to_coverage: pipeline.alpha_to_coverage && !colour.integer,
		};
		written += crate::pass::write_fragment(colour, if index == 0 { depth_stencil.as_deref_mut() } else { None }, &fragment)?;
	}
	attachments.depth_stencil = depth_stencil;
	Ok(written)
}

// ---------------------------------------------------------------------------------------------
// Lines and points.
// ---------------------------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn stage_line(prepared: &mut Prepared, pipeline: &Pipeline, source: &dyn Source, vertices: [u32; 2], index: u32, topology: Topology, instance: u32, viewport: &Viewport, stats: &mut Stats) -> Result<(), Error> {
	let mut machine = core::mem::take(&mut prepared.scratch.vertex_machine);
	let shaded = {
		let first = run_vertex(pipeline, source, vertices[0], instance, &mut machine);
		let second = run_vertex(pipeline, source, vertices[1], instance, &mut machine);
		match (first, second) {
			(Ok(a), Ok(b)) => [a, b],
			(first, second) => {
				prepared.scratch.vertex_machine = machine;
				first?;
				second?;
				return Ok(());
			}
		}
	};
	prepared.scratch.vertex_machine = machine;
	let provoking = geometry::provoking(topology, index);
	let which = vertices.iter().position(|vertex| *vertex == provoking).unwrap_or(0);
	// A LINE IS CLIPPED AS A TWO-VERTEX POLYGON AND KEEPS TWO VERTICES OR NONE.
	let corners = [
		clip::Vertex::new(shaded[0].position).with_smooth(shaded[0].smooth).with_noperspective(shaded[0].noperspective),
		clip::Vertex::new(shaded[1].position).with_smooth(shaded[1].smooth).with_noperspective(shaded[1].noperspective),
		clip::Vertex::new(shaded[1].position).with_smooth(shaded[1].smooth).with_noperspective(shaded[1].noperspective),
	];
	let mut work = prepared.scratch.clip_work.take().unwrap_or_default();
	let outcome = clip::clip_triangle_into(&corners, shaded[which].flat, &mut work, &mut prepared.scratch.clipped);
	prepared.scratch.clip_work = Some(work);
	outcome?;
	let clipped = prepared.scratch.clipped;
	if clipped.vertices().len() < 2 {
		stats.culled += 1;
		return Ok(());
	}
	let (first, first_w) = raster::project(clipped.vertices()[0].position, viewport)?;
	let (second, second_w) = raster::project(clipped.vertices()[1].position, viewport)?;
	let from = (crate::fixed::quantise(first.x).map_err(|_| Error::InvalidTransform)?, crate::fixed::quantise(first.y).map_err(|_| Error::InvalidTransform)?);
	let to = (crate::fixed::quantise(second.x).map_err(|_| Error::InvalidTransform)?, crate::fixed::quantise(second.y).map_err(|_| Error::InvalidTransform)?);
	prepared.scratch.segments.push(Segment { from, to, depth: [first.z, second.z], inverse_w: [first_w, second_w], smooth: [clipped.vertices()[0].smooth, clipped.vertices()[1].smooth], noperspective: [clipped.vertices()[0].noperspective, clipped.vertices()[1].noperspective], flat: clipped.flat });
	Ok(())
}

#[allow(clippy::too_many_arguments)]
fn stage_point(prepared: &mut Prepared, pipeline: &Pipeline, source: &dyn Source, vertex: u32, index: u32, topology: Topology, instance: u32, viewport: &Viewport, stats: &mut Stats) -> Result<(), Error> {
	let mut machine = core::mem::take(&mut prepared.scratch.vertex_machine);
	let outcome = run_vertex(pipeline, source, vertex, instance, &mut machine);
	prepared.scratch.vertex_machine = machine;
	let shaded = outcome?;
	let _ = (index, topology);
	// A POINT IS IN OR OUT WITH NO PARTIAL CASE. One whose centre is outside the volume is removed
	// whole even when its square would have covered visible pixels - the alternative is a point
	// clipped to a rectangle, which is not a point.
	if !render3d::clip::ClipCoordQ::new(shaded.position).inside_volume() {
		stats.culled += 1;
		return Ok(());
	}
	let (window, _) = raster::project(shaded.position, viewport)?;
	let centre = (crate::fixed::quantise(window.x).map_err(|_| Error::InvalidTransform)?, crate::fixed::quantise(window.y).map_err(|_| Error::InvalidTransform)?);
	// EVERY VARYING TAKES THE POINT'S OWN VERTEX VALUE, whatever its qualifier: a point has one
	// vertex and there is nothing to interpolate between.
	let mut values = shaded.smooth;
	for component in shaded.noperspective.as_slice() {
		values.push(*component);
	}
	prepared.scratch.points.push(Dot { centre, size: geometry::point_size(shaded.point_size), depth: window.z, values, flat: shaded.flat });
	Ok(())
}

#[allow(clippy::too_many_arguments)]
fn shade_lines(prepared: &mut Prepared, attachments: &mut Attachments<'_>, source: &dyn Source, pipeline: &Pipeline, map: &[(u32, Interpolation, usize, usize)], width: u32, height: u32, samples: u32, stats: &mut Stats) -> Result<(), Error> {
	let segments = core::mem::take(&mut prepared.scratch.segments);
	let mut machine = core::mem::take(&mut prepared.scratch.fragment_machine);
	for segment in &segments {
		let x0 = (segment.from.0.pixel().min(segment.to.0.pixel()) - 1).max(0);
		let x1 = (segment.from.0.pixel().max(segment.to.0.pixel()) + 2).min(width as i64);
		let y0 = (segment.from.1.pixel().min(segment.to.1.pixel()) - 1).max(0);
		let y1 = (segment.from.1.pixel().max(segment.to.1.pixel()) + 2).min(height as i64);
		for y in y0..y1 {
			for x in x0..x1 {
				if !geometry::line_covers_pixel(segment.from, segment.to, x, y) {
					continue;
				}
				let at = geometry::line_parameter(segment.from, segment.to, x, y);
				let weights = [1.0 - at, at, 0.0];
				let mut smooth = Varyings::EMPTY;
				for index in 0..segment.smooth[0].len() {
					smooth.push(interp::smooth(weights, [segment.inverse_w[0], segment.inverse_w[1], 1.0], [segment.smooth[0].as_slice()[index], segment.smooth[1].as_slice()[index], 0.0]));
				}
				let mut noperspective = Varyings::EMPTY;
				for index in 0..segment.noperspective[0].len() {
					noperspective.push(interp::noperspective(weights, [segment.noperspective[0].as_slice()[index], segment.noperspective[1].as_slice()[index], 0.0]));
				}
				let depth = interp::depth(weights, [segment.depth[0], segment.depth[1], 0.0]);
				let bindings = FragmentBindings {
					source,
					smooth: smooth.as_slice(),
					noperspective: noperspective.as_slice(),
					flat: segment.flat.as_slice(),
					locations: map,
					// A LINE HAS NO WINDING, so it is always reported front-facing rather than being
					// given a side a cull mode could remove it by.
					front_facing: true,
					coordinate: [x as f32 + 0.5, y as f32 + 0.5, depth, 1.0],
					sample_index: 0,
					sample_mask: u32::MAX,
				};
				interpreter::execute_into(&pipeline.fragment, &bindings, &mut machine).map_err(fault)?;
				stats.fragments += 1;
				if machine.outputs().discarded {
					stats.discarded += 1;
					continue;
				}
				let coverage = (1_u32 << samples) - 1;
				stats.samples_written += write_outputs(attachments, pipeline, machine.outputs(), x as u32, y as u32, coverage, depth, Facing::Front, samples)?;
			}
		}
	}
	prepared.scratch.segments = segments;
	prepared.scratch.fragment_machine = machine;
	Ok(())
}

#[allow(clippy::too_many_arguments)]
fn shade_points(prepared: &mut Prepared, attachments: &mut Attachments<'_>, source: &dyn Source, pipeline: &Pipeline, map: &[(u32, Interpolation, usize, usize)], width: u32, height: u32, samples: u32, stats: &mut Stats) -> Result<(), Error> {
	let points = core::mem::take(&mut prepared.scratch.points);
	let mut machine = core::mem::take(&mut prepared.scratch.fragment_machine);
	let positions = render3d::msaa::sample_positions(samples)?;
	for dot in &points {
		if dot.size == 0 {
			continue;
		}
		let half = dot.size as i64;
		let centre_x = dot.centre.0.pixel();
		let centre_y = dot.centre.1.pixel();
		for y in (centre_y - half).max(0)..(centre_y + half + 1).min(height as i64) {
			for x in (centre_x - half).max(0)..(centre_x + half + 1).min(width as i64) {
				let mut coverage = 0;
				for position in positions {
					let sample = (crate::Subpixel(x * crate::fixed::SUBPIXEL_ONE + (position.x as f64 * crate::fixed::SUBPIXEL_ONE as f64) as i64), crate::Subpixel(y * crate::fixed::SUBPIXEL_ONE + (position.y as f64 * crate::fixed::SUBPIXEL_ONE as f64) as i64));
					if geometry::point_covers(dot.centre, dot.size, sample) {
						coverage |= 1 << position.index;
					}
				}
				if coverage == 0 {
					continue;
				}
				let bindings = FragmentBindings { source, smooth: dot.values.as_slice(), noperspective: dot.values.as_slice(), flat: dot.flat.as_slice(), locations: map, front_facing: true, coordinate: [x as f32 + 0.5, y as f32 + 0.5, dot.depth, 1.0], sample_index: 0, sample_mask: coverage };
				interpreter::execute_into(&pipeline.fragment, &bindings, &mut machine).map_err(fault)?;
				stats.fragments += 1;
				if machine.outputs().discarded {
					stats.discarded += 1;
					continue;
				}
				stats.samples_written += write_outputs(attachments, pipeline, machine.outputs(), x as u32, y as u32, coverage, dot.depth, Facing::Front, samples)?;
			}
		}
	}
	prepared.scratch.points = points;
	prepared.scratch.fragment_machine = machine;
	Ok(())
}

/// The commands a caller recorded, turned into the plan's draws.
///
/// A COMMAND LIST IS THE FRAME and this is where it becomes a plan: everything it can refuse, it
/// refuses HERE, so `execute` walks a list that has already been checked.
pub fn draws_from(commands: &[Command], topology: Topology) -> Vec<Draw> {
	let mut out = Vec::new();
	let mut pipeline = 0;
	for command in commands {
		match command {
			Command::BindPipeline(handle) => pipeline = handle.0,
			Command::Draw { vertices, instances, first_instance, .. } => {
				out.push(Draw { pipeline, topology, count: *vertices, instances: *instances, first_instance: *first_instance, base_vertex: 0, restart: false });
			}
			Command::DrawIndexed { indices, instances, base_vertex, first_instance, .. } => {
				out.push(Draw { pipeline, topology, count: *indices, instances: *instances, first_instance: *first_instance, base_vertex: *base_vertex, restart: false });
			}
			_ => {}
		}
	}
	out
}
