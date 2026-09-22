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
	// THE QUEUE'S MESH AND NOT THE DRAWABLE'S, because a level-of-detail ladder resolves per view and
	// the queue is the per-view list. For a drawable with no ladder the two are the same value.
	let Some(draw) = geometry.draw_of(entry.mesh) else {
		// NOT SKIPPED. A drawable whose geometry the source does not know is a scene that names
		// something never loaded, and drawing the rest of the frame without it is the defect that
		// shows up as an object which is sometimes missing.
		return Err(Error::InvalidRenderState { reason: "a scene drawable naming geometry the source does not know" });
	};
	if bound.pipeline != Some(material.pipeline()) {
		// THE QUEUE'S DEPTH WRITE, not the geometry's.
		let state = PipelineState { depth_write: material.writes_depth(), ..draw.state };
		list.bind_pipeline(material.pipeline(), &state, pass.samples)?;
		bound.pipeline = Some(material.pipeline());
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
	if bound.resources != Some(material.uniforms()) {
		list.bind_resources(material.uniforms(), 1)?;
		bound.resources = Some(material.uniforms());
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

/// What a shadow pass writes into and what it draws with.
///
/// ONE PIPELINE FOR EVERY CASTER, which is the whole difference from the lighting pass. A shadow map
/// holds DEPTH and nothing else, so what a caster's material would have contributed - its colour,
/// its texture, its lighting model - contributes nothing to the result; binding it per draw would be
/// state changes for a value never read. The one exception the profile keeps is the ALPHA MASK,
/// which decides whether a fragment exists at all, and that is why the mask queue is drawn with a
/// pipeline of its own rather than skipped or folded in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CasterPass {
	/// The caller's own identifier for the depth-only target set.
	pub targets: u32,
	pub viewport: Rect,
	/// The depth-only pipeline every opaque caster is drawn with.
	pub pipeline: GraphicsPipeline,
	/// The pipeline the ALPHA-MASKED casters are drawn with, which is the one that samples the
	/// material's cutout. `None` leaves masked casters out of the map entirely, which is the honest
	/// answer for a layer that has no such pipeline - a leaf shadow drawn as a solid quad is worse
	/// than no leaf shadow.
	pub masked_pipeline: Option<GraphicsPipeline>,
	/// The resource set carrying the light's own view-projection, bound once for the pass.
	pub light_set: u32,
}

/// Record one light's shadow pass: every caster in the view, depth only.
///
/// THE TRANSPARENT QUEUE IS NOT DRAWN, and that is the profile's shape rather than a simplification.
/// A surface that lets light through does not stop it, and a shadow map has one depth per texel with
/// no way to say "some of the light got past" - so a transparent caster written into it casts a
/// SOLID shadow, which is the opposite of what the surface does.
///
/// THE DEPTH WRITE IS FORCED ON AND THE COMPARISON IS THE PIPELINE'S. A caster pass whose depth write
/// was the queue's would inherit the lighting pass's rule, and the transparent queue's rule there is
/// "do not write" - which would produce an empty map on a layer that reused one state for both.
pub fn record_casters(scene: &Scene, queue: &Queue, geometry: &dyn Geometry, list: &mut CommandList, set: &RenderTargetSet<'_>, pass: &CasterPass) -> Result<(), Error> {
	list.begin_render_pass(pass.targets, set)?;
	list.set_viewport(pass.viewport)?;
	list.bind_resources(pass.light_set, 1)?;
	let mut bound = Bound::default();
	// OPAQUE THEN MASKED, which is the order the lighting pass uses and the order a depth pass wants
	// for the same reason: the cheap pipeline first fills the map, and the masked draws that follow
	// are the ones that can be rejected by it.
	for (entries, pipeline) in [(&queue.opaque, Some(pass.pipeline)), (&queue.alpha_mask, pass.masked_pipeline)] {
		let Some(pipeline) = pipeline else { continue };
		for entry in entries {
			record_caster(scene, entry, geometry, list, pipeline, &mut bound)?;
		}
	}
	list.end_render_pass()
}

/// The pipeline state a caster is drawn with, whatever state its geometry arrived carrying.
///
/// DEPTH IS WRITTEN, ALWAYS, and this is the one pass where that is not a caller's choice. A shadow
/// map that wrote no depth is a shadow map of nothing - and a layer that reused the lighting pass's
/// per-queue rule here would get exactly that for its mask queue, because the rule there is "the
/// transparent queue does not write". An empty shadow map reads as a scene with no shadows in it,
/// which is not a failure anything reports.
///
/// A SEPARATE FUNCTION BECAUSE IT IS THE DECISION. `Command::BindPipeline` carries the pipeline and
/// not the state it was validated against, so a fixture reading the recorded list cannot see this
/// rule at all; reading it here is what makes it testable.
pub fn caster_state(state: PipelineState) -> PipelineState {
	PipelineState { depth_write: true, ..state }
}

fn record_caster(scene: &Scene, entry: &Queued, geometry: &dyn Geometry, list: &mut CommandList, pipeline: GraphicsPipeline, bound: &mut Bound) -> Result<(), Error> {
	if scene.drawables().get(entry.drawable as usize).is_none() {
		return Err(Error::InvalidRenderState { reason: "a queue entry naming a drawable the scene does not hold" });
	}
	// THE SAME MESH THE LIGHTING PASS DRAWS. A caster at a different level than its own surface is a
	// shadow whose silhouette does not match the thing casting it.
	let Some(draw) = geometry.draw_of(entry.mesh) else {
		return Err(Error::InvalidRenderState { reason: "a scene drawable naming geometry the source does not know" });
	};
	if bound.pipeline != Some(pipeline) {
		list.bind_pipeline(pipeline, &caster_state(draw.state), 1)?;
		bound.pipeline = Some(pipeline);
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
	let instances = entry.instance_count();
	match draw.indices {
		Some(indices) => {
			if bound.index != Some(indices.buffer) {
				list.bind_index_buffer(indices.buffer, 0, indices.wide)?;
				bound.index = Some(indices.buffer);
			}
			list.draw_indexed(draw.topology, indices.count, instances, 0, 0, 0, draw.vertices)
		}
		None => list.draw(draw.topology, draw.vertices, instances, 0, 0),
	}
}
