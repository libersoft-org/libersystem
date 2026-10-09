//! Recorded draws and typed readbacks on the immediate software backend.
//!
//! Sources are resolved by the caller, in recorded draw order, just as `frame::execute` borrows a
//! caller-resolved Source. Every source and attachment remains borrowed until execution finishes.
//! Only preparation allocates command/result storage. Completion slots are reused, and returning
//! them by borrow prevents a second submission from overwriting an unread result.

use crate::frame::{self, Attachments, Pipeline, Source, Stats, Workers};
use alloc::{vec, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};
use render_math::{Vec4, Viewport};
use render3d::command::{Command, CommandList, ReadbackAttachment, ReadbackCommand, Rect};
use render3d::resource::{DepthStencilView, LoadOp, RenderTargetView, StoreOp, TextureViewDesc};
use render3d::{Completion, Error, ReadbackResult, ReadbackTicket, ReadbackValue, Render3DLimits, Status, Submission};

static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy)]
pub struct TargetLayout {
	pub id: u32,
	pub width: u32,
	pub height: u32,
}

/// A single-sample resolve destination, explicitly bound by its recorded texture and view.
pub struct ResolveBinding<'a> {
	pub texture: u32,
	pub view: TextureViewDesc,
	pub colour: &'a mut crate::Colour,
}

pub struct TargetBinding<'a> {
	pub id: u32,
	pub attachments: Attachments<'a>,
}

#[derive(Clone, Copy)]
pub enum ClearColour {
	Colour(Vec4),
	Identity(u32),
}

/// Explicit per-pass clear values. Entries are required exactly where that pass says LoadClear.
pub struct ClearValues<'a> {
	pub colour: &'a [Option<ClearColour>],
	pub depth: Option<f32>,
	pub stencil: Option<u8>,
}
impl ClearValues<'_> {
	pub const NONE: Self = Self { colour: &[], depth: None, stencil: None };
}

struct Pass {
	target: usize,
	colour: Vec<RenderTargetView>,
	depth: Option<DepthStencilView>,
	resolve: Vec<Option<RenderTargetView>>,
	clear_colour: Vec<Option<ClearColour>>,
	clear_depth: Option<f32>,
	clear_stencil: Option<u8>,
}

enum Step {
	Begin(usize),
	End(usize),
	Draw { target: usize, source: usize, frame: frame::Prepared, viewport: Rect, scissor: Option<Rect> },
	Read { target: usize, slot: usize },
}
struct Slot {
	request: ReadbackCommand,
	pass: usize,
	value: Option<ReadbackValue>,
	consumed: bool,
}

pub struct Prepared {
	targets: Vec<TargetLayout>,
	steps: Vec<Step>,
	slots: Vec<Slot>,
	draws: usize,
	passes: Vec<Pass>,
}

fn invalid(reason: &'static str) -> Error {
	Error::InvalidRenderState { reason }
}

/// Prepare an immutable finished list. Each draw is a separate prepared segment so viewport,
/// scissor, pipeline changes and readbacks between passes retain their exact recorded order.
pub fn prepare(list: &CommandList, limits: Render3DLimits, pipelines: &[Pipeline], targets: &[TargetLayout], readback_capacity: usize, clears: &[ClearValues<'_>]) -> Result<Prepared, Error> {
	if !list.is_finished() {
		return Err(invalid("an unfinished command list cannot be submitted"));
	}
	for (index, target) in targets.iter().enumerate() {
		if target.width == 0 || target.height == 0 || targets[..index].iter().any(|old| old.id == target.id) {
			return Err(invalid("target layouts must have unique names and nonempty extents"));
		}
	}
	let mut prepared = Prepared { targets: targets.to_vec(), steps: Vec::new(), slots: Vec::new(), draws: 0, passes: Vec::new() };
	if clears.len() != list.passes().len() {
		return Err(invalid("one explicit clear-value binding is required per recorded pass"));
	}
	let mut current = None;
	let mut current_pass = None;
	let mut pipeline = None;
	let mut viewport = None;
	let mut scissor = None;
	let mut written = vec![false; targets.len()];
	for command in list.commands() {
		match *command {
			Command::BeginRenderPass { targets: id } => {
				current = Some(targets.iter().position(|target| target.id == id).ok_or(invalid("a pass names an unbound target"))?);
				pipeline = None;
				viewport = None;
				scissor = None;
				let index = prepared.passes.len();
				let recorded = &list.passes()[index];
				let clear = &clears[index];
				if clear.depth.is_some_and(|value| !value.is_finite()) {
					return Err(Error::NonFinite { what: "depth clear" });
				}
				for (slot, view) in recorded.colour.iter().enumerate() {
					if view.load == LoadOp::Clear {
						let value = clear.colour.get(slot).copied().flatten().ok_or(invalid("LoadClear requires an explicit colour or integer clear value"))?;
						if matches!(value, ClearColour::Colour(value) if !value.is_finite()) {
							return Err(Error::NonFinite { what: "colour clear" });
						}
						let integer = matches!(view.format, "R32_UINT" | "R32Uint");
						if integer != matches!(value, ClearColour::Identity(_)) {
							return Err(invalid("clear value and attachment types disagree"));
						}
					}
				}
				if recorded.depth_stencil.is_some_and(|view| (view.depth_load == LoadOp::Clear && clear.depth.is_none()) || (view.stencil_load == LoadOp::Clear && clear.stencil.is_none())) {
					return Err(invalid("LoadClear requires explicit depth/stencil clear values"));
				}
				prepared.passes.push(Pass { target: current.unwrap(), colour: recorded.colour.clone(), depth: recorded.depth_stencil, resolve: recorded.resolve.clone(), clear_colour: clear.colour.to_vec(), clear_depth: clear.depth, clear_stencil: clear.stencil });
				prepared.steps.push(Step::Begin(index));
				current_pass = Some(index);
			}
			Command::EndRenderPass => {
				written[current.take().ok_or(invalid("a pass ends without a target"))?] = true;
				prepared.steps.push(Step::End(current_pass.take().ok_or(invalid("a pass ends without operations"))?));
			}
			Command::BindPipeline(handle) => pipeline = Some(handle.0 as usize),
			Command::SetViewport(rect) => viewport = Some(rect),
			Command::SetScissor(rect) => scissor = Some(rect),
			Command::Draw { .. } | Command::DrawIndexed { .. } => {
				let target = current.ok_or(invalid("a draw has no target"))?;
				let pipeline = pipelines.get(pipeline.ok_or(invalid("a draw has no pipeline"))?).ok_or(invalid("a draw names an unbound pipeline"))?;
				let mut draws = frame::draws_from(core::slice::from_ref(command), pipeline.state.topology);
				draws[0].pipeline = 0;
				let frame = frame::prepare(limits, vec![pipeline.clone()], draws, targets[target].width, targets[target].height)?;
				prepared.steps.push(Step::Draw { target, source: prepared.draws, frame, viewport: viewport.ok_or(invalid("a draw has no viewport"))?, scissor });
				prepared.draws += 1;
			}
			Command::Readback(request) => {
				let target = targets.iter().position(|target| target.id == request.targets).ok_or(invalid("a readback names an unbound target"))?;
				if current.is_some() || !written[target] {
					return Err(invalid("readback requires an earlier completed pass on its target"));
				}
				let (pass_index, pass) = prepared.passes.iter().enumerate().rev().find(|(_, pass)| pass.target == target).ok_or(invalid("readback has no recorded attachment provenance"))?;
				let stored = match request.attachment {
					ReadbackAttachment::Colour(slot) | ReadbackAttachment::Identity(slot) => pass.colour.get(slot as usize).is_some_and(|view| view.store == StoreOp::Store && matches!(view.format, "R32_UINT" | "R32Uint") == matches!(request.attachment, ReadbackAttachment::Identity(_))),
					ReadbackAttachment::ResolvedColour(slot) | ReadbackAttachment::ResolvedIdentity(slot) => pass.resolve.get(slot as usize).and_then(Option::as_ref).is_some_and(|view| view.store == StoreOp::Store && matches!(view.format, "R32_UINT" | "R32Uint") == matches!(request.attachment, ReadbackAttachment::ResolvedIdentity(_))),
					ReadbackAttachment::Depth => pass.depth.is_some_and(|view| view.depth_store == StoreOp::Store),
				};
				if !stored {
					return Err(invalid("readback disagrees with the actual preceding pass type or StoreDiscard"));
				}
				if request.x >= targets[target].width || request.y >= targets[target].height {
					return Err(invalid("readback is outside its prepared target"));
				}
				if prepared.slots.len() >= readback_capacity {
					return Err(Error::LimitExceeded { limit: "readbacks per submission", ceiling: readback_capacity as u64, asked: prepared.slots.len() as u64 + 1 });
				}
				if prepared.slots.iter().any(|slot| slot.request.destination == request.destination) {
					return Err(invalid("two readbacks name the same destination in one submission"));
				}
				prepared.steps.push(Step::Read { target, slot: prepared.slots.len() });
				prepared.slots.push(Slot { request, pass: pass_index, value: None, consumed: false });
			}
			// These names have already been resolved into the borrowed Sources supplied for each
			// draw. They are not a second resource registry inside the software rasteriser.
			Command::BindVertexBuffer { .. } | Command::BindIndexBuffer { .. } | Command::BindResources { .. } => {}
		}
	}
	Ok(prepared)
}

/// A terminal software submission and the readbacks produced by that same execution.
pub struct Completed<'a> {
	pub submission: Submission,
	pub stats: Stats,
	slots: &'a mut [Slot],
}

impl Completed<'_> {
	/// Each destination is consumed once. A failed list never publishes an earlier partial value.
	pub fn take(&mut self, destination: u32) -> Result<ReadbackResult, Error> {
		let slot = self.slots.iter_mut().find(|slot| slot.request.destination.0 == destination).ok_or(invalid("no readback was recorded for this destination"))?;
		if slot.consumed {
			return Err(invalid("a readback result was consumed twice"));
		}
		slot.consumed = true;
		Ok(ReadbackResult { ticket: ReadbackTicket { completion: Completion { serial: self.submission.completion.serial, source: self.submission.completion.source }, status: self.submission.status, destination }, value: slot.value.take() })
	}
}

impl Prepared {
	pub fn draw_count(&self) -> usize {
		self.draws
	}

	pub fn submit<'a>(&'a mut self, targets: &mut [TargetBinding<'_>], sources: &[&dyn Source]) -> Result<Completed<'a>, Error> {
		self.submit_with(targets, sources, &frame::Serial)
	}

	// @handles: AsynchronousReadback
	pub fn submit_with<'a>(&'a mut self, targets: &mut [TargetBinding<'_>], sources: &[&dyn Source], workers: &dyn Workers) -> Result<Completed<'a>, Error> {
		self.submit_with_resolves(targets, sources, &mut [], workers)
	}

	/// Submit with caller-owned resolve storage; all aliases and destination shapes are validated
	/// before execution. Resolves happen before multisample source StoreDiscard at pass end.
	pub fn submit_with_resolves<'a>(&'a mut self, targets: &mut [TargetBinding<'_>], sources: &[&dyn Source], resolves: &mut [ResolveBinding<'_>], workers: &dyn Workers) -> Result<Completed<'a>, Error> {
		if sources.len() != self.draws || targets.len() != self.targets.len() {
			return Err(invalid("submission bindings differ from the prepared draws or targets"));
		}
		// Validate all actual storage before the first draw, not after a partially executed list.
		for (layout, target) in self.targets.iter().zip(targets.iter()) {
			if layout.id != target.id || (target.attachments.colour.is_empty() && target.attachments.depth_stencil.is_none()) {
				return Err(invalid("a submission target does not match its prepared binding"));
			}
			let samples = target.attachments.colour.first().map(|colour| colour.samples).or_else(|| target.attachments.depth_stencil.as_ref().map(|depth| depth.samples)).ok_or(invalid("a submission target has no attachments"))?;
			if samples == 0 || target.attachments.colour.iter().any(|colour| colour.width != layout.width || colour.height != layout.height || colour.samples != samples) {
				return Err(invalid("a submission attachment has the wrong extent or sample count"));
			}
			if target.attachments.depth_stencil.as_ref().is_some_and(|depth| depth.width != layout.width || depth.height != layout.height || depth.samples != samples) {
				return Err(invalid("a submission depth attachment has the wrong extent or sample count"));
			}
		}
		for (index, binding) in resolves.iter().enumerate() {
			if resolves[..index].iter().any(|old| old.texture == binding.texture && old.view.overlaps(&binding.view)) {
				return Err(invalid("resolve destination bound twice"));
			}
		}
		for pass in &self.passes {
			let attachment = &targets[pass.target].attachments;
			if pass.colour.len() != attachment.colour.len() || pass.depth.is_some() != attachment.depth_stencil.is_some() {
				return Err(invalid("actual pass attachment count differs from recorded descriptors"));
			}
			for (view, colour) in pass.colour.iter().zip(attachment.colour.iter()) {
				if view.width != colour.width || view.height != colour.height || view.samples != colour.samples || matches!(view.format, "R32_UINT" | "R32Uint") != colour.integer {
					return Err(invalid("recorded colour descriptor differs from actual storage"));
				}
			}
			for view in pass.resolve.iter().flatten() {
				let destination = resolve_binding(resolves, view)?;
				if destination.width != view.width || destination.height != view.height || destination.samples != 1 || destination.integer != matches!(view.format, "R32_UINT" | "R32Uint") {
					return Err(invalid("resolve storage differs from its recorded single-sample destination"));
				}
			}
			if let (Some(view), Some(depth)) = (pass.depth, attachment.depth_stencil.as_deref()) {
				if view.width != depth.width || view.height != depth.height || view.samples != depth.samples || view.format != depth.format {
					return Err(invalid("recorded depth descriptor differs from actual storage"));
				}
			}
		}
		for step in &self.steps {
			match step {
				Step::Read { target, slot } => {
					read(&targets[*target].attachments, &self.slots[*slot], &self.passes, resolves, false)?;
				}
				Step::Draw { target, frame, .. } if Some(frame.pipelines()[0].state.samples) != targets[*target].attachments.colour.first().map(|colour| colour.samples).or_else(|| targets[*target].attachments.depth_stencil.as_ref().map(|depth| depth.samples)) => return Err(invalid("pipeline and bound attachment sample counts differ")),
				_ => {}
			}
		}
		let serial = NEXT_SERIAL.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |serial| serial.checked_add(1)).map_err(|_| invalid("submission serials exhausted"))?;
		for slot in &mut self.slots {
			slot.value = None;
			slot.consumed = false;
		}
		let mut status = Status::Complete;
		let mut stats = Stats::default();
		for step in &mut self.steps {
			let result = match step {
				Step::Begin(pass) => begin(&self.passes[*pass], &mut targets[self.passes[*pass].target].attachments),
				Step::End(pass) => end(&self.passes[*pass], &mut targets[self.passes[*pass].target].attachments, resolves),
				Step::Draw { target, source, frame, viewport, scissor } => {
					let attachments = &mut targets[*target].attachments;
					attachments.viewport = Viewport::new(viewport.x as f32, viewport.y as f32, viewport.width as f32, viewport.height as f32);
					attachments.scissor = scissor.map(|rect| frame::Scissor { x: rect.x, y: rect.y, width: rect.width, height: rect.height });
					frame::execute_with(frame, attachments, sources[*source], workers).map(|draw| {
						stats.primitives += draw.primitives;
						stats.clipped += draw.clipped;
						stats.culled += draw.culled;
						stats.fragments += draw.fragments;
						stats.samples_written += draw.samples_written;
						stats.discarded += draw.discarded;
					})
				}
				Step::Read { target, slot } => read(&targets[*target].attachments, &self.slots[*slot], &self.passes, resolves, true).map(|value| self.slots[*slot].value = Some(value)),
			};
			if let Err(error) = result {
				status = Status::Failed(error);
				break;
			}
		}
		if status != Status::Complete {
			for slot in &mut self.slots {
				slot.value = None;
			}
		}
		Ok(Completed { submission: Submission::terminal(serial, 0, status)?, stats, slots: &mut self.slots })
	}
}

fn begin(pass: &Pass, attachments: &mut Attachments<'_>) -> Result<(), Error> {
	for (index, (view, colour)) in pass.colour.iter().zip(attachments.colour.iter_mut()).enumerate() {
		match view.load {
			LoadOp::Load => {}
			LoadOp::Discard => colour.discard(),
			LoadOp::Clear => match pass.clear_colour[index].ok_or(invalid("missing prepared colour clear"))? {
				ClearColour::Colour(value) => colour.fill(value),
				ClearColour::Identity(value) => colour.fill_identity(value)?,
			},
		}
	}
	if let Some(view) = pass.depth {
		let depth = attachments.depth_stencil.as_deref_mut().ok_or(invalid("the recorded pass has no bound depth storage"))?;
		match view.depth_load {
			LoadOp::Load => {}
			LoadOp::Discard => depth.discard_depth(),
			LoadOp::Clear => depth.clear_depth(pass.clear_depth.ok_or(invalid("missing prepared depth clear"))?),
		}
		match view.stencil_load {
			LoadOp::Load => {}
			LoadOp::Discard => depth.discard_stencil(),
			LoadOp::Clear => depth.clear_stencil(pass.clear_stencil.ok_or(invalid("missing prepared stencil clear"))?),
		}
	}
	Ok(())
}

fn end(pass: &Pass, attachments: &mut Attachments<'_>, resolves: &mut [ResolveBinding<'_>]) -> Result<(), Error> {
	// Resolve before discarding the multisample source. Undefined samples propagate to undefined
	// destination pixels, so a later read refuses only the pixels nobody defined.
	for (slot, view) in pass.resolve.iter().enumerate() {
		let Some(view) = view else { continue };
		let source = &attachments.colour[slot];
		let destination = resolves.iter_mut().find(|binding| binding.texture == view.texture && binding.view == view.view).ok_or(invalid("missing prepared resolve binding"))?;
		destination.colour.discard();
		for y in 0..source.height {
			for x in 0..source.width {
				if let Ok(value) = colour_pixel(source, x, y, source.integer, true) {
					match value {
						ReadbackValue::Identity(id) => {
							destination.colour.set_identity(x, y, 0, id);
						}
						ReadbackValue::Colour(value) => destination.colour.set(x, y, 0, value),
						_ => unreachable!(),
					}
				}
			}
		}
		if view.store == StoreOp::Discard {
			destination.colour.discard();
		}
	}

	for (view, colour) in pass.colour.iter().zip(attachments.colour.iter_mut()) {
		if view.store == StoreOp::Discard {
			colour.discard();
		}
	}
	if pass.depth.is_some_and(|view| view.depth_store == StoreOp::Discard) {
		attachments.depth_stencil.as_deref_mut().ok_or(invalid("the recorded pass has no bound depth storage"))?.discard_depth();
	}
	if pass.depth.is_some_and(|view| view.stencil_store == StoreOp::Discard) {
		attachments.depth_stencil.as_deref_mut().ok_or(invalid("the recorded pass has no bound stencil storage"))?.discard_stencil();
	}
	Ok(())
}

fn resolve_binding<'a>(resolves: &'a [ResolveBinding<'_>], view: &RenderTargetView) -> Result<&'a crate::Colour, Error> {
	resolves.iter().find(|binding| binding.texture == view.texture && binding.view == view.view).map(|binding| &*binding.colour).ok_or(invalid("a recorded resolve destination has no actual storage binding"))
}

fn read(attachments: &Attachments<'_>, slot: &Slot, passes: &[Pass], resolves: &[ResolveBinding<'_>], check_contents: bool) -> Result<ReadbackValue, Error> {
	let request = slot.request;
	match request.attachment {
		ReadbackAttachment::Identity(index) | ReadbackAttachment::Colour(index) => {
			let colour = attachments.colour.get(index as usize).ok_or(invalid("readback has no attachment"))?;
			colour_pixel(colour, request.x, request.y, matches!(request.attachment, ReadbackAttachment::Identity(_)), check_contents)
		}
		ReadbackAttachment::ResolvedIdentity(index) | ReadbackAttachment::ResolvedColour(index) => {
			let view = passes[slot.pass].resolve.get(index as usize).and_then(Option::as_ref).ok_or(invalid("readback has no recorded resolve destination"))?;
			colour_pixel(resolve_binding(resolves, view)?, request.x, request.y, matches!(request.attachment, ReadbackAttachment::ResolvedIdentity(_)), check_contents)
		}
		// @handles: ReadbackDepth, DepthReadback
		ReadbackAttachment::Depth => {
			let depth = attachments.depth_stencil.as_deref().filter(|depth| request.x < depth.width && request.y < depth.height && depth.samples != 0).ok_or(invalid("depth readback requires a depth attachment"))?;
			if check_contents {
				depth.readable_depth_at(request.x, request.y, 0)?;
			}
			Ok(ReadbackValue::Depth(render3d::depth::readback(depth.format, depth.depth_at(request.x, request.y, 0))))
		}
	}
}

// @handles: ReadbackColor, ColourReadback
fn colour_pixel(colour: &crate::Colour, x: u32, y: u32, integer: bool, check_contents: bool) -> Result<ReadbackValue, Error> {
	if colour.integer != integer || x >= colour.width || y >= colour.height || colour.samples == 0 {
		return Err(invalid("colour readback type or bounds differ from actual storage"));
	}
	if integer {
		if check_contents {
			colour.readable_at(x, y, 0)?;
		}
		return colour.identity_at(x, y, 0).map(ReadbackValue::Identity).ok_or(invalid("identity readback requires an integer attachment"));
	}
	let mut sum = Vec4::ZERO;
	for sample in 0..colour.samples {
		if check_contents {
			colour.readable_at(x, y, sample)?;
		}
		let value = colour.at(x, y, sample);
		sum = Vec4::new(sum.x + value.x, sum.y + value.y, sum.z + value.z, sum.w + value.w);
	}
	let count = colour.samples as f32;
	Ok(ReadbackValue::Colour(Vec4::new(sum.x / count, sum.y / count, sum.z / count, sum.w / count)))
}

#[cfg(test)]
mod tests;
