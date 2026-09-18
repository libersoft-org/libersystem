//! THE COMMAND MODEL: what a frame is recorded as, and what the recording refuses.
//!
//! THIS IS A USERSPACE RENDERING MODEL AND NOT AN OS-LEVEL ADAPTER/DEVICE/QUEUE VOCABULARY. The
//! difference is not taste: this one has two planned implementations - a software backend and a
//! future GPU one - and the other has none, so one is designed against real consumers and the other
//! would be designed against a guess.
//!
//! A LIST IS RECORDED, FINISHED, AND THEN IMMUTABLE. Recording into a finished list, ending a pass
//! that was not begun, drawing outside a pass, and finishing with a pass still open are each a typed
//! refusal AT THE CALL - not at submission, where the report would name a list rather than the
//! command that broke it.
//!
//! THE VALIDATION IS THE POINT. A command list that records anything and validates at submission has
//! moved every error message away from the code that caused it; one that validates at the call can
//! say "this draw has no pipeline bound" while the caller is still in the function that bound one.
//!
//! NOTHING HERE OWNS A RESOURCE. A `Buffer`, a `Texture` and a `Sampler` are the caller's own
//! identifiers; this layer records which were used and in what order, and the backend resolves them.
//! That is what makes the model backend-neutral rather than a description of one backend's handles.

use alloc::vec::Vec;

use crate::error::{Error, MeshFault};
use crate::limits::Render3DLimits;
use crate::resource::RenderTargetSet;

/// A resource as a caller names it. This layer stores no resources.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Buffer(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Texture(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sampler(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ShaderProgram(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GraphicsPipeline(pub u32);

/// How a vertex is read out of its streams.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VertexAttribute {
	/// Which stream this attribute is read from.
	pub stream: u32,
	/// Where in the stream's element it begins.
	pub offset: u32,
	/// How many bytes it occupies. The format's, and checked against the stride.
	pub size: u32,
	/// The shader input it feeds.
	pub location: u32,
}

/// One vertex stream's shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VertexStream {
	pub stride: u32,
	/// Whether the stream advances per vertex or per instance. THE DISTINCTION IS THE WHOLE OF
	/// INSTANCING, and a layout that could not say it would need a second draw call per instance.
	pub per_instance: bool,
}

/// A vertex layout: the streams, and the attributes read out of them.
pub struct VertexLayout<'a> {
	pub streams: &'a [VertexStream],
	pub attributes: &'a [VertexAttribute],
}

impl VertexLayout<'_> {
	/// Refuse a layout that cannot read what it claims to.
	///
	/// AN ATTRIBUTE THAT LEAVES ITS STRIDE READS THE NEXT VERTEX, which is the defect that shows as
	/// geometry that is almost right - so it is refused with the numbers rather than clamped.
	pub fn validate(&self, limits: &Render3DLimits) -> Result<(), Error> {
		limits.admit("vertex streams", self.streams.len() as u64, limits.max_vertex_streams as u64)?;
		limits.admit("vertex attributes", self.attributes.len() as u64, limits.max_vertex_attributes as u64)?;
		for (index, attribute) in self.attributes.iter().enumerate() {
			// ONE LOCATION, ONE VALUE. A stage reads `Binding::Attribute { location }` and there has
			// to be a single answer to that; a layout offering two is refused with BOTH attributes'
			// indices, because the second one is only half of what is wrong.
			if let Some(first) = self.attributes[..index].iter().position(|earlier| earlier.location == attribute.location) {
				return Err(Error::InvalidMesh { reason: MeshFault::LocationDeclaredTwice { location: attribute.location, attribute: index as u32, first: first as u32 } });
			}
			let Some(stream) = self.streams.get(attribute.stream as usize) else {
				return Err(Error::InvalidMesh { reason: MeshFault::StreamTooShort { stream: attribute.stream, needs: 0, has: 0 } });
			};
			let end = attribute.offset.saturating_add(attribute.size);
			if attribute.size == 0 || end > stream.stride {
				return Err(Error::InvalidMesh { reason: MeshFault::AttributeOutsideStride { attribute: index as u32, offset: attribute.offset, size: attribute.size, stride: stream.stride } });
			}
		}
		Ok(())
	}
}

/// What a pipeline is, beyond its shaders and its layout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PipelineState {
	pub topology: Topology,
	pub cull: Cull,
	/// Counter-clockwise is the front, in NDC, before the viewport inverts Y. The winding is FIXED
	/// by `render-math` and this field says which side is CULLED, not which is the front - a state
	/// that could flip the winding would make the whole convention local.
	pub depth_test: Option<crate::depth::CompareOp>,
	pub depth_write: bool,
	pub samples: u32,
	/// Whether any fragment input is `sample`-qualified, which is what decides the shading rate.
	pub per_sample_shading: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Topology {
	TriangleList,
	TriangleStrip,
	TriangleFan,
	LineList,
	LineStrip,
	PointList,
}

impl Topology {
	pub const fn name(self) -> &'static str {
		match self {
			Self::TriangleList => "triangle list",
			Self::TriangleStrip => "triangle strip",
			Self::TriangleFan => "triangle fan",
			Self::LineList => "line list",
			Self::LineStrip => "line strip",
			Self::PointList => "point list",
		}
	}

	/// How many vertices the FIRST primitive needs. A draw with fewer produces nothing, which is a
	/// mistake rather than a no-op.
	pub const fn first_primitive(self) -> u32 {
		match self {
			Self::TriangleList | Self::TriangleStrip | Self::TriangleFan => 3,
			Self::LineList | Self::LineStrip => 2,
			Self::PointList => 1,
		}
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cull {
	None,
	Front,
	Back,
}

/// A rectangle in window pixels, for the viewport and the scissor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
	pub x: i32,
	pub y: i32,
	pub width: u32,
	pub height: u32,
}

/// One recorded command. THE LIST IS THE FRAME, and a backend replays it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Command {
	BeginRenderPass { targets: u32 },
	EndRenderPass,
	BindPipeline(GraphicsPipeline),
	BindVertexBuffer { slot: u32, buffer: Buffer, offset: u64 },
	BindIndexBuffer { buffer: Buffer, offset: u64, wide: bool },
	BindResources { set: u32 },
	SetViewport(Rect),
	SetScissor(Rect),
	Draw { vertices: u32, instances: u32, first_vertex: u32, first_instance: u32 },
	DrawIndexed { indices: u32, instances: u32, first_index: u32, base_vertex: i32, first_instance: u32 },
}

/// What the recorder knows about the list so far. The state machine the refusals are about.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Recording {
	in_pass: bool,
	finished: bool,
	pipeline: bool,
	index_buffer: bool,
	viewport: bool,
	draws_in_pass: u32,
}

/// A command list being recorded, or finished.
pub struct CommandList {
	commands: Vec<Command>,
	state: Recording,
	limits: Render3DLimits,
	/// The bytes the recorded commands occupy, against the device's own bound.
	bytes: u64,
}

impl CommandList {
	pub fn new(limits: Render3DLimits) -> Self {
		Self { commands: Vec::new(), state: Recording::default(), limits, bytes: 0 }
	}

	pub fn commands(&self) -> &[Command] {
		&self.commands
	}

	pub fn is_finished(&self) -> bool {
		self.state.finished
	}

	fn push(&mut self, command: Command) -> Result<(), Error> {
		if self.state.finished {
			return Err(Error::InvalidRenderState { reason: "a command recorded into a list that is already finished" });
		}
		let size = core::mem::size_of::<Command>() as u64;
		if self.bytes + size > self.limits.max_command_bytes {
			return Err(Error::LimitExceeded { limit: "command bytes", ceiling: self.limits.max_command_bytes, asked: self.bytes + size });
		}
		self.bytes += size;
		self.commands.push(command);
		Ok(())
	}

	/// Begin a pass over a validated target set.
	///
	/// A PASS IS NOT NESTED. There is one set of attachments and one viewport at a time, and a
	/// nested pass would need a rule for which attachments the inner one writes that no backend has.
	pub fn begin_render_pass(&mut self, targets: u32, set: &RenderTargetSet<'_>) -> Result<(), Error> {
		if self.state.in_pass {
			return Err(Error::InvalidRenderState { reason: "a render pass begun inside another one" });
		}
		set.validate(&self.limits)?;
		self.push(Command::BeginRenderPass { targets })?;
		self.state.in_pass = true;
		// EVERY BINDING IS PER PASS. A pipeline bound in one pass is not bound in the next: the
		// attachments changed, so the pipeline's compatibility with them has to be established
		// again, and carrying the binding across would make that check silent.
		self.state.pipeline = false;
		self.state.index_buffer = false;
		self.state.viewport = false;
		self.state.draws_in_pass = 0;
		Ok(())
	}

	pub fn end_render_pass(&mut self) -> Result<(), Error> {
		if !self.state.in_pass {
			return Err(Error::InvalidRenderState { reason: "a render pass ended that was never begun" });
		}
		self.push(Command::EndRenderPass)?;
		self.state.in_pass = false;
		Ok(())
	}

	pub fn bind_pipeline(&mut self, pipeline: GraphicsPipeline, state: &PipelineState, pass_samples: u32) -> Result<(), Error> {
		self.inside_pass()?;
		// A PIPELINE IS COMPATIBLE WITH THE PASS OR IT IS NOT BOUND. A sample count that disagrees
		// produces coverage the attachments cannot hold, and finding out at the draw would name the
		// draw rather than the pipeline.
		if state.samples != pass_samples {
			return Err(Error::IncompatiblePipeline { reason: "the pipeline's sample count disagrees with the pass's attachments" });
		}
		if state.per_sample_shading && state.samples == 1 {
			return Err(Error::InvalidRenderState { reason: "per-sample shading with one sample has no samples to shade" });
		}
		self.push(Command::BindPipeline(pipeline))?;
		self.state.pipeline = true;
		Ok(())
	}

	pub fn bind_vertex_buffer(&mut self, slot: u32, buffer: Buffer, offset: u64) -> Result<(), Error> {
		self.inside_pass()?;
		self.limits.admit("vertex stream slot", slot as u64, self.limits.max_vertex_streams.saturating_sub(1) as u64)?;
		self.push(Command::BindVertexBuffer { slot, buffer, offset })
	}

	pub fn bind_index_buffer(&mut self, buffer: Buffer, offset: u64, wide: bool) -> Result<(), Error> {
		self.inside_pass()?;
		self.push(Command::BindIndexBuffer { buffer, offset, wide })?;
		self.state.index_buffer = true;
		Ok(())
	}

	pub fn bind_resources(&mut self, set: u32, count: u32) -> Result<(), Error> {
		self.inside_pass()?;
		self.limits.admit("bound resources", count as u64, self.limits.max_bound_resources as u64)?;
		self.push(Command::BindResources { set })
	}

	pub fn set_viewport(&mut self, rect: Rect) -> Result<(), Error> {
		self.inside_pass()?;
		if rect.width == 0 || rect.height == 0 {
			return Err(Error::InvalidRenderState { reason: "a viewport with no area draws nothing, which is a mistake rather than a state" });
		}
		self.push(Command::SetViewport(rect))?;
		self.state.viewport = true;
		Ok(())
	}

	/// The scissor, which is bounded BY THE VIEWPORT.
	///
	/// A SCISSOR OUTSIDE THE VIEWPORT IS A STATE WITH NO MEANING: the viewport decides where
	/// fragments are, and a scissor that reaches outside it can only clip things that do not exist.
	/// Refusing it names the mistake; clamping it hides one.
	pub fn set_scissor(&mut self, rect: Rect, viewport: Rect) -> Result<(), Error> {
		self.inside_pass()?;
		let inside = rect.x >= viewport.x && rect.y >= viewport.y && (rect.x as i64 + rect.width as i64) <= (viewport.x as i64 + viewport.width as i64) && (rect.y as i64 + rect.height as i64) <= (viewport.y as i64 + viewport.height as i64);
		if !inside {
			return Err(Error::InvalidRenderState { reason: "a scissor rectangle outside the viewport can only clip fragments that do not exist" });
		}
		self.push(Command::SetScissor(rect))
	}

	/// A non-indexed draw. `instances` of `vertices`.
	///
	/// `draw_instanced` IS THIS WITH A COUNT, and there is no separate entry point: two functions
	/// whose only difference is a `1` are two places for the validation to diverge.
	pub fn draw(&mut self, topology: Topology, vertices: u32, instances: u32, first_vertex: u32, first_instance: u32) -> Result<(), Error> {
		self.ready_to_draw()?;
		if vertices < topology.first_primitive() {
			return Err(Error::InvalidMesh { reason: MeshFault::TooFewVertices { topology: topology.name(), needs: topology.first_primitive(), has: vertices } });
		}
		if instances == 0 {
			return Err(Error::InvalidMesh { reason: MeshFault::TooFewVertices { topology: topology.name(), needs: 1, has: 0 } });
		}
		self.push(Command::Draw { vertices, instances, first_vertex, first_instance })?;
		self.state.draws_in_pass += 1;
		self.bound_draws()
	}

	/// An indexed draw. REFUSES WITHOUT AN INDEX BUFFER, which is the mistake that otherwise reads
	/// whatever was bound last.
	pub fn draw_indexed(&mut self, topology: Topology, indices: u32, instances: u32, first_index: u32, base_vertex: i32, first_instance: u32, vertices_available: u32) -> Result<(), Error> {
		self.ready_to_draw()?;
		if !self.state.index_buffer {
			return Err(Error::InvalidRenderState { reason: "an indexed draw with no index buffer bound" });
		}
		if indices < topology.first_primitive() {
			return Err(Error::InvalidMesh { reason: MeshFault::TooFewVertices { topology: topology.name(), needs: topology.first_primitive(), has: indices } });
		}
		if instances == 0 {
			return Err(Error::InvalidMesh { reason: MeshFault::TooFewVertices { topology: topology.name(), needs: 1, has: 0 } });
		}
		// THE BASE VERTEX IS SIGNED AND THE SUM IS CHECKED. A negative base with a small index is a
		// read before the buffer, which is the case a per-index check at draw time cannot catch and
		// this one can: the LARGEST index the draw can reach is bounded here.
		let highest = (first_index as i64 + indices as i64 - 1) + base_vertex as i64;
		if highest < 0 || highest >= vertices_available as i64 {
			return Err(Error::InvalidMesh { reason: MeshFault::IndexOutOfRange { index: highest.max(0) as u32, vertices: vertices_available } });
		}
		self.push(Command::DrawIndexed { indices, instances, first_index, base_vertex, first_instance })?;
		self.state.draws_in_pass += 1;
		self.bound_draws()
	}

	/// Finish recording. THE LIST IS IMMUTABLE AFTERWARDS, and a list finished with a pass still
	/// open is refused: the attachments it wrote have no store operation applied, so what they hold
	/// is undefined and a backend would have to invent an answer.
	pub fn finish(&mut self) -> Result<(), Error> {
		if self.state.finished {
			return Err(Error::InvalidRenderState { reason: "a list finished twice" });
		}
		if self.state.in_pass {
			return Err(Error::InvalidRenderState { reason: "a list finished with a render pass still open" });
		}
		self.state.finished = true;
		Ok(())
	}

	fn inside_pass(&self) -> Result<(), Error> {
		if self.state.finished {
			return Err(Error::InvalidRenderState { reason: "a command recorded into a list that is already finished" });
		}
		if !self.state.in_pass {
			return Err(Error::InvalidRenderState { reason: "a command recorded outside a render pass" });
		}
		Ok(())
	}

	fn ready_to_draw(&self) -> Result<(), Error> {
		self.inside_pass()?;
		if !self.state.pipeline {
			return Err(Error::InvalidRenderState { reason: "a draw with no pipeline bound" });
		}
		if !self.state.viewport {
			return Err(Error::InvalidRenderState { reason: "a draw with no viewport set; there is no default, because a default would be a guess at the attachment's size" });
		}
		Ok(())
	}

	fn bound_draws(&self) -> Result<(), Error> {
		if self.state.draws_in_pass > self.limits.max_draws_per_pass {
			return Err(Error::LimitExceeded { limit: "draws per pass", ceiling: self.limits.max_draws_per_pass as u64, asked: self.state.draws_in_pass as u64 });
		}
		Ok(())
	}
}
