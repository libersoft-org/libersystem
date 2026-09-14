//! WHAT A SCENE TURNS INTO: `render3d` commands, and nothing else.
//!
//! THERE IS NO PATH FROM THIS LAYER TO A BACKEND. Every feature the scene has - hierarchy, culling,
//! queues, instancing, materials, lights, offscreen targets, the pass graph, picking - ends here, in
//! a `render3d::CommandList`. The moment one feature had its own route to a backend, a second
//! backend would have to implement that route as well as the command model, and the layer would be
//! two implementations pretending to be one. A fixture asserts it by recording a scene that uses
//! every feature and finding only `render3d::Command` values in the result.
//!
//! THE DEPTH WRITE IS THE QUEUE'S AND NOT THE GEOMETRY'S. `Scene3D Core Profile 1` fixes it per
//! queue - the two opaque queues write and the transparent one does not - so this layer overrides
//! whatever the pipeline state arrived with. A transparent surface that wrote depth would hide the
//! one behind it, which is the commonest transparency bug and one no caller should be able to cause.
//!
//! REDUNDANT BINDS ARE ELIDED. The queues sort by distance and not by material, so consecutive draws
//! often share a pipeline; rebinding it is a command a backend has to look at to learn it changes
//! nothing. The elision is per pass, because `render3d` drops every binding at a pass boundary.

use render3d::command::{Buffer, CommandList, GraphicsPipeline, PipelineState, Rect, Topology};
use render3d::error::Error;
use render3d::resource::RenderTargetSet;

use crate::queue::{Queue, Queued};
use crate::scene::Scene;

/// Where the indices of an indexed draw come from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Indices {
	pub buffer: Buffer,
	pub count: u32,
	/// 32-bit indices rather than 16-bit.
	pub wide: bool,
}

/// How one mesh is drawn.
///
/// THE SCENE STORES A MESH IDENTIFIER AND NOT THIS, because the geometry belongs to whatever loaded
/// it and a retained scene that owned buffers would be a resource manager as well as a hierarchy.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct MeshDraw {
	pub topology: Topology,
	pub state: PipelineState,
	pub vertex_buffer: Buffer,
	/// The PER-INSTANCE stream: one world transform and one colour per instance, compacted by the
	/// caller in the order `Queued::instances` gives. `None` for geometry that is never instanced.
	pub instance_buffer: Option<Buffer>,
	/// How many vertices the stream holds, which bounds an indexed draw's reach.
	pub vertices: u32,
	pub indices: Option<Indices>,
}

/// Where the recorder looks a mesh identifier up.
pub trait Geometry {
	fn draw_of(&self, mesh: u32) -> Option<MeshDraw>;
}

/// What a pass writes into, and which resource slot carries a drawable's picking identity.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PassTargets {
	/// The caller's own identifier for the target set.
	pub targets: u32,
	pub viewport: Rect,
	pub samples: u32,
	/// The resource-set index a drawable's identity is bound at, for a pass that writes one. `None`
	/// for a pass with no picking attachment.
	pub id_set: Option<u32>,
}

/// Record one view's queues into a pass.
///
/// THE ORDER IS OPAQUE, ALPHA-MASK, TRANSPARENT, fixed here rather than being the caller's choice.
pub fn record(scene: &Scene, queue: &Queue, geometry: &dyn Geometry, list: &mut CommandList, set: &RenderTargetSet<'_>, pass: &PassTargets) -> Result<(), Error> {
	list.begin_render_pass(pass.targets, set)?;
	list.set_viewport(pass.viewport)?;
	let mut bound = Bound::default();
	for entries in queue.in_order() {
		for entry in entries {
			record_one(scene, entry, geometry, list, pass, &mut bound)?;
		}
	}
	list.end_render_pass()
}

/// What is already bound in this pass, so a redundant bind is not recorded.
#[derive(Default)]
struct Bound {
	pipeline: Option<GraphicsPipeline>,
	vertex: Option<Buffer>,
	instance: Option<Buffer>,
	index: Option<Buffer>,
	resources: Option<u32>,
}

fn record_one(scene: &Scene, entry: &Queued, geometry: &dyn Geometry, list: &mut CommandList, pass: &PassTargets, bound: &mut Bound) -> Result<(), Error> {
	let Some(drawable) = scene.drawables().get(entry.drawable as usize) else {
		return Err(Error::InvalidRenderState { reason: "a queue entry naming a drawable the scene does not hold" });
	};
	let Ok(material) = scene.material_of(drawable) else {
		return Err(Error::InvalidRenderState { reason: "a drawable naming a material the scene does not hold" });
	};
	let Some(draw) = geometry.draw_of(drawable.mesh) else {
		// NOT SKIPPED. A drawable whose geometry the source does not know is a scene that names
		// something never loaded, and drawing the rest of the frame without it is the defect that
		// shows up as an object which is sometimes missing.
		return Err(Error::InvalidRenderState { reason: "a scene drawable naming geometry the source does not know" });
	};
	if bound.pipeline != Some(material.pipeline) {
		// THE QUEUE'S DEPTH WRITE, not the geometry's.
		let state = PipelineState { depth_write: material.writes_depth(), ..draw.state };
		list.bind_pipeline(material.pipeline, &state, pass.samples)?;
		bound.pipeline = Some(material.pipeline);
	}
	if bound.vertex != Some(draw.vertex_buffer) {
		list.bind_vertex_buffer(0, draw.vertex_buffer, 0)?;
		bound.vertex = Some(draw.vertex_buffer);
	}
	if !entry.instances.is_empty() {
		let Some(buffer) = draw.instance_buffer else {
			return Err(Error::InvalidRenderState { reason: "an instanced drawable whose geometry declares no per-instance stream" });
		};
		if bound.instance != Some(buffer) {
			list.bind_vertex_buffer(1, buffer, 0)?;
			bound.instance = Some(buffer);
		}
	}
	if bound.resources != Some(material.uniforms) {
		list.bind_resources(material.uniforms, 1)?;
		bound.resources = Some(material.uniforms);
	}
	// THE IDENTITY IS BOUND PER DRAWABLE AND ONLY FOR THE QUEUES THAT WRITE IT. A transparent
	// drawable binds none, which is what makes a pick through glass answer what is behind it.
	if let Some(set) = pass.id_set {
		if material.writes_id() {
			list.bind_resources(set, 1)?;
			// The binding is per drawable, so the next one must record its own.
			bound.resources = None;
		}
	}
	let instances = entry.instance_count();
	match draw.indices {
		Some(indices) => {
			if bound.index != Some(indices.buffer) {
				list.bind_index_buffer(indices.buffer, 0, indices.wide)?;
				bound.index = Some(indices.buffer);
			}
			// INSTANCING IS THE COUNT AND NOT A SECOND PATH: a count of one records the same
			// command, so a scene does not have two ways to draw the same thing.
			list.draw_indexed(draw.topology, indices.count, instances, 0, 0, 0, draw.vertices)
		}
		None => list.draw(draw.topology, draw.vertices, instances, 0, 0),
	}
}
