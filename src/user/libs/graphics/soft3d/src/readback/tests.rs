use super::*;
use crate::{Colour, Indices, Val};
use render_shader::{
	Stage, Type,
	builder::Builder,
	ir::{Binding, Constant, Output},
};
use render3d::command::{Buffer, Cull, GraphicsPipeline, PipelineState, Topology};
use render3d::resource::{Aspect, RenderTargetSet, TextureDimension, TextureViewDesc};

struct Mesh(bool);
impl Source for Mesh {
	fn attribute(&self, location: u32, vertex: u32, _: u32) -> Option<Val> {
		if !self.0 || location != 0 {
			return None;
		}
		[[-1.0, -1.0, 0.5, 1.0], [1.0, -1.0, 0.5, 1.0], [0.0, 1.0, 0.5, 1.0]].get(vertex as usize).map(|value| Val::vector_f32(value))
	}
	fn uniform(&self, _: u32, _: u32) -> Option<Val> {
		None
	}
	fn sample(&self, _: u32, _: u32, _: &Val) -> Option<Val> {
		None
	}
	fn indices(&self) -> Indices<'_> {
		Indices::None
	}
}
fn pipeline() -> Pipeline {
	let mut vertex = Builder::new(Stage::Vertex, "readback-test-position");
	let position = vertex.load(Type::vec(4), Binding::Attribute { location: 0 });
	vertex.store(Output::Position, position);
	let mut fragment = Builder::new(Stage::Fragment, "readback-test-id");
	let id = fragment.constant(Constant::U32(0xffff_fffe));
	fragment.store(Output::Integer(0), id);
	Pipeline { state: PipelineState { topology: Topology::TriangleList, cull: Cull::None, depth_test: None, depth_write: false, samples: 1, per_sample_shading: false }, vertex: vertex.finish(), fragment: fragment.finish(), blend: vec![render3d::AttachmentBlend { enabled: false, colour: render3d::BlendEquation::REPLACE, alpha: render3d::BlendEquation::REPLACE, write_mask: render3d::ColorWriteMask::ALL }], stencil: None, stencil_back: None, depth_compare: render3d::CompareOp::Always, depth_write: false, bias: (0.0, 0.0, 0.0), alpha_to_coverage: false, sample_mask: u32::MAX }
}
fn view(load: LoadOp, store: StoreOp) -> RenderTargetView {
	RenderTargetView { texture: 1, view: TextureViewDesc { dimension: TextureDimension::D2, aspect: Aspect::Colour, base_mip: 0, mip_count: 1, base_layer: 0, layer_count: 1 }, format: "R32_UINT", samples: 1, width: 8, height: 8, load, store }
}
fn record(load: LoadOp, draw: bool, x: u32, y: u32) -> CommandList {
	let views = [view(load, StoreOp::Store)];
	let set = RenderTargetSet { colour: &views, depth_stencil: None, resolve: &[] };
	let mut list = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	list.begin_render_pass(1, &set).unwrap();
	if draw {
		list.bind_pipeline(GraphicsPipeline(0), &pipeline().state, 1).unwrap();
		list.set_viewport(Rect { x: 0, y: 0, width: 8, height: 8 }).unwrap();
		list.draw(Topology::TriangleList, 3, 1, 0, 0).unwrap();
	}
	list.end_render_pass().unwrap();
	list.readback(ReadbackCommand { targets: 1, attachment: ReadbackAttachment::Identity(0), x, y, destination: Buffer(10) }, &set).unwrap();
	list.finish().unwrap();
	list
}
fn prepare_one(list: &CommandList) -> Prepared {
	prepare(list, Render3DLimits::PROFILE_MINIMUM, &[pipeline()], &[TargetLayout { id: 1, width: 8, height: 8 }], 1, &[ClearValues { colour: &[Some(ClearColour::Identity(17))], depth: None, stencil: None }]).unwrap()
}
fn target(storage: &mut [Colour]) -> [TargetBinding<'_>; 1] {
	[TargetBinding { id: 1, attachments: Attachments { colour: storage, depth_stencil: None, viewport: Viewport::new(0.0, 0.0, 8.0, 8.0), scissor: None } }]
}

#[test]
fn actual_draw_readback_has_unique_completion_and_no_steady_allocation() {
	let list = record(LoadOp::Clear, true, 4, 4);
	let mut prepared = prepare_one(&list);
	let mut other = prepare_one(&list);
	let mesh = Mesh(true);
	let mut storage = [Colour::new(8, 8, 1, true)];
	let mut targets = target(&mut storage);
	let first = prepared.submit(&mut targets, &[&mesh]).unwrap().submission.completion.serial;
	let other_serial = other.submit(&mut targets, &[&mesh]).unwrap().submission.completion.serial;
	assert_ne!(first, other_serial, "independent prepared lists cannot alias completion origin");
	let before = crate::counted::count();
	for _ in 0..20 {
		let mut done = prepared.submit(&mut targets, &[&mesh]).unwrap();
		assert_eq!(done.submission.status, Status::Complete);
		assert!(done.stats.samples_written > 0);
		assert_eq!(done.take(10).unwrap().finish(), Ok(ReadbackValue::Identity(0xffff_fffe)));
		assert!(done.take(10).is_err());
		assert!(done.take(99).is_err());
	}
	assert_eq!(crate::counted::count(), before);
}

#[test]
fn actual_clear_and_discard_distinguish_untouched_and_drawn_pixels() {
	for (load, draw, x, y, expected) in [
		(LoadOp::Clear, false, 0, 0, Some(17)),
		(LoadOp::Discard, false, 0, 0, None),
		(LoadOp::Discard, true, 4, 4, Some(0xffff_fffe)),
		(LoadOp::Discard, true, 0, 0, None),
	] {
		let mut prepared = prepare_one(&record(load, draw, x, y));
		let mut storage = [Colour::new(8, 8, 1, true)];
		storage[0].fill_identity(777).unwrap();
		let mesh = Mesh(true);
		let sources: &[&dyn Source] = if draw { &[&mesh] } else { &[] };
		let mut targets = target(&mut storage);
		let mut done = prepared.submit(&mut targets, sources).unwrap();
		let read = done.take(10).unwrap().finish();
		match expected {
			Some(value) => assert_eq!(read, Ok(ReadbackValue::Identity(value))),
			None => assert!(read.is_err()),
		}
	}
}

#[test]
fn failed_execution_never_publishes_readback_value() {
	let mut prepared = prepare_one(&record(LoadOp::Clear, true, 4, 4));
	let mut storage = [Colour::new(8, 8, 1, true)];
	let mut targets = target(&mut storage);
	let mut done = prepared.submit(&mut targets, &[&Mesh(false)]).unwrap();
	assert!(matches!(done.submission.status, Status::Failed(_)));
	assert!(done.take(10).unwrap().finish().is_err());
}

#[test]
fn capacity_missing_clear_wrong_storage_and_forged_store_descriptor_are_refused() {
	let list = record(LoadOp::Clear, false, 0, 0);
	let layouts = [TargetLayout { id: 1, width: 8, height: 8 }];
	assert!(prepare(&list, Render3DLimits::PROFILE_MINIMUM, &[], &layouts, 0, &[ClearValues::NONE]).is_err());
	assert!(prepare(&list, Render3DLimits::PROFILE_MINIMUM, &[], &layouts, 1, &[ClearValues::NONE]).is_err());
	assert!(prepare(&list, Render3DLimits::PROFILE_MINIMUM, &[], &layouts, 0, &[ClearValues { colour: &[Some(ClearColour::Identity(0))], depth: None, stencil: None }]).is_err());
	let mut prepared = prepare_one(&list);
	let mut wrong = [Colour::new(8, 8, 1, false)];
	assert!(prepared.submit(&mut target(&mut wrong), &[]).is_err());
	let discarded = [view(LoadOp::Clear, StoreOp::Discard)];
	let kept = [view(LoadOp::Clear, StoreOp::Store)];
	let mut forged = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	forged.begin_render_pass(1, &RenderTargetSet { colour: &discarded, depth_stencil: None, resolve: &[] }).unwrap();
	forged.end_render_pass().unwrap();
	forged.readback(ReadbackCommand { targets: 1, attachment: ReadbackAttachment::Identity(0), x: 0, y: 0, destination: Buffer(10) }, &RenderTargetSet { colour: &kept, depth_stencil: None, resolve: &[] }).unwrap();
	forged.finish().unwrap();
	assert!(prepare(&forged, Render3DLimits::PROFILE_MINIMUM, &[], &layouts, 1, &[ClearValues { colour: &[Some(ClearColour::Identity(0))], depth: None, stencil: None }]).is_err());
}

#[test]
fn later_execution_failure_withholds_an_earlier_successful_read_in_the_same_list() {
	let views = [view(LoadOp::Clear, StoreOp::Store)];
	let set = RenderTargetSet { colour: &views, depth_stencil: None, resolve: &[] };
	let mut list = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	list.begin_render_pass(1, &set).unwrap();
	list.end_render_pass().unwrap();
	list.readback(ReadbackCommand { targets: 1, attachment: ReadbackAttachment::Identity(0), x: 4, y: 4, destination: Buffer(10) }, &set).unwrap();
	list.begin_render_pass(1, &set).unwrap();
	list.bind_pipeline(GraphicsPipeline(0), &pipeline().state, 1).unwrap();
	list.set_viewport(Rect { x: 0, y: 0, width: 8, height: 8 }).unwrap();
	list.draw(Topology::TriangleList, 3, 1, 0, 0).unwrap();
	list.end_render_pass().unwrap();
	list.finish().unwrap();
	let clear = || ClearValues { colour: &[Some(ClearColour::Identity(17))], depth: None, stencil: None };
	let mut prepared = prepare(&list, Render3DLimits::PROFILE_MINIMUM, &[pipeline()], &[TargetLayout { id: 1, width: 8, height: 8 }], 1, &[clear(), clear()]).unwrap();
	let mut storage = [Colour::new(8, 8, 1, true)];
	let mut targets = target(&mut storage);
	let mut done = prepared.submit(&mut targets, &[&Mesh(false)]).unwrap();
	assert!(matches!(done.submission.status, Status::Failed(_)));
	let earlier = done.take(10).unwrap();
	assert!(earlier.value.is_none());
	assert!(earlier.finish().is_err());
}

#[test]
fn recorded_depth_only_pass_draws_and_reads_normalised_depth() {
	use render3d::resource::DepthStencilView;
	let colour_view = view(LoadOp::Clear, StoreOp::Store);
	let depth = DepthStencilView { texture: 2, view: TextureViewDesc { aspect: Aspect::Depth, ..colour_view.view }, format: render3d::DepthFormat::Depth16, samples: 1, width: 8, height: 8, depth_load: LoadOp::Clear, depth_store: StoreOp::Store, stencil_load: LoadOp::Clear, stencil_store: StoreOp::Store };
	let set = RenderTargetSet { colour: &[], depth_stencil: Some(depth), resolve: &[] };
	let mut state = pipeline();
	state.blend.clear();
	state.depth_write = true;
	state.state.depth_write = true;
	let mut fragment = Builder::new(Stage::Fragment, "depth-only-readback");
	let value = fragment.constant(Constant::F32(0.3));
	fragment.store(Output::Depth, value);
	state.fragment = fragment.finish();
	let mut list = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	list.begin_render_pass(1, &set).unwrap();
	list.bind_pipeline(GraphicsPipeline(0), &state.state, 1).unwrap();
	list.set_viewport(Rect { x: 0, y: 0, width: 8, height: 8 }).unwrap();
	list.draw(Topology::TriangleList, 3, 1, 0, 0).unwrap();
	list.end_render_pass().unwrap();
	list.readback(ReadbackCommand { targets: 1, attachment: ReadbackAttachment::Depth, x: 4, y: 4, destination: Buffer(10) }, &set).unwrap();
	list.finish().unwrap();
	let mut prepared = prepare(&list, Render3DLimits::PROFILE_MINIMUM, &[state], &[TargetLayout { id: 1, width: 8, height: 8 }], 1, &[ClearValues { colour: &[], depth: Some(0.9), stencil: Some(0) }]).unwrap();
	let mut depth = crate::DepthStencil::new(8, 8, 1, render3d::DepthFormat::Depth16);
	let mut targets = [TargetBinding { id: 1, attachments: Attachments { colour: &mut [], depth_stencil: Some(&mut depth), viewport: Viewport::new(0.0, 0.0, 8.0, 8.0), scissor: None } }];
	let mut done = prepared.submit(&mut targets, &[&Mesh(true)]).unwrap();
	assert_eq!(done.submission.status, Status::Complete);
	assert!(done.stats.samples_written > 0);
	let ReadbackValue::Depth(value) = done.take(10).unwrap().finish().unwrap() else { panic!("depth result changed type") };
	assert!((value - 0.3).abs() < 1.0 / 65535.0);
}

#[test]
fn explicit_resolve_precedes_source_discard_and_keeps_typed_results_without_allocation() {
	let source_views = [
		RenderTargetView { format: "RGBA8", samples: 4, load: LoadOp::Load, store: StoreOp::Discard, ..view(LoadOp::Load, StoreOp::Store) },
		RenderTargetView { texture: 2, samples: 4, load: LoadOp::Load, store: StoreOp::Discard, ..view(LoadOp::Load, StoreOp::Store) },
	];
	let resolve_views = [
		Some(RenderTargetView { texture: 11, samples: 1, store: StoreOp::Store, ..source_views[0] }),
		Some(RenderTargetView { texture: 12, samples: 1, store: StoreOp::Store, ..source_views[1] }),
	];
	let set = RenderTargetSet { colour: &source_views, depth_stencil: None, resolve: &resolve_views };
	let mut list = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	list.begin_render_pass(1, &set).unwrap();
	list.end_render_pass().unwrap();
	for (attachment, destination) in [(ReadbackAttachment::ResolvedColour(0), 10), (ReadbackAttachment::ResolvedIdentity(1), 20)] {
		list.readback(ReadbackCommand { targets: 1, attachment, x: 4, y: 4, destination: Buffer(destination) }, &set).unwrap();
	}
	list.finish().unwrap();
	let mut prepared = prepare(&list, Render3DLimits::PROFILE_MINIMUM, &[], &[TargetLayout { id: 1, width: 8, height: 8 }], 2, &[ClearValues::NONE]).unwrap();
	let mut source = [Colour::new(8, 8, 4, false), Colour::new(8, 8, 4, true)];
	let mut resolved_colour = Colour::new(8, 8, 1, false);
	let mut resolved_identity = Colour::new(8, 8, 1, true);
	let mut destinations = [
		ResolveBinding { texture: 11, view: source_views[0].view, colour: &mut resolved_colour },
		ResolveBinding { texture: 12, view: source_views[1].view, colour: &mut resolved_identity },
	];
	// Missing or wrong storage is refused before the source can be discarded.
	assert!(prepared.submit(&mut target(&mut source), &[]).is_err());
	assert!(source[0].readable_at(4, 4, 0).is_ok());
	let before = crate::counted::count();
	for _ in 0..3 {
		for (sample, value) in [0.0, 0.2, 0.6, 1.0].into_iter().enumerate() {
			source[0].set(4, 4, sample as u32, Vec4::new(value, value, value, 1.0));
			source[1].set_identity(4, 4, sample as u32, 0xffff_fffe - sample as u32);
		}
		let mut done = prepared.submit_with_resolves(&mut target(&mut source), &[], &mut destinations, &frame::Serial).unwrap();
		assert_eq!(done.submission.status, Status::Complete);
		let ReadbackValue::Colour(value) = done.take(10).unwrap().finish().unwrap() else { panic!("wrong resolve kind") };
		assert!((value.x - 0.45).abs() < 0.0001);
		assert_eq!(done.take(20).unwrap().finish(), Ok(ReadbackValue::Identity(0xffff_fffe)));
		assert!(source[0].readable_at(4, 4, 0).is_err());
		assert!(source[1].readable_at(4, 4, 0).is_err());
		assert_eq!(destinations[1].colour.identity_at(4, 4, 0), Some(0xffff_fffe));
	}
	assert_eq!(crate::counted::count(), before);
}
