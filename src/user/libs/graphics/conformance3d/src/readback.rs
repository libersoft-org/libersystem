//! Real recorded readbacks: typed storage, command order and the submission completion are joined.

use crate::harness::{HEIGHT, Plan, WIDTH, close, pipeline_for, quad_at_depth};
use crate::{Outcome, Trouble};
use render_math::{Vec4, Viewport};
use render_shader::{
	Stage, Type,
	builder::Builder,
	ir::{Binding, Output},
};
use render3d::command::{Buffer, CommandList, GraphicsPipeline, ReadbackAttachment, ReadbackCommand, Rect};
use render3d::resource::{Aspect, DepthStencilView, LoadOp, RenderTargetSet, RenderTargetView, StoreOp, TextureDimension, TextureViewDesc};
use render3d::{CompareOp, DepthFormat, ReadbackValue, Render3DLimits, Status};
use soft3d::readback::{ClearColour, ClearValues, ResolveBinding, TargetBinding, TargetLayout};

fn view(aspect: Aspect) -> TextureViewDesc {
	TextureViewDesc { dimension: TextureDimension::D2, aspect, base_mip: 0, mip_count: 1, base_layer: 0, layer_count: 1 }
}

fn fragment(identity: u32) -> render_shader::Module {
	let mut shader = Builder::new(Stage::Fragment, "exact-readback-identity");
	shader.varying(0, Type::vec(4), render_shader::Interpolation::Smooth);
	let colour = shader.load(Type::vec(4), Binding::Varying { location: 0 });
	shader.store(Output::Colour(0), colour);
	let identity = shader.constant(render_shader::ir::Constant::U32(identity));
	shader.store(Output::Integer(1), identity);
	shader.finish()
}

struct Pixels {
	colour: Vec4,
	depth: f32,
	identity: u32,
	background: u32,
	picked: Option<u32>,
	expected_pick: Option<u32>,
	serial: u64,
}

fn drawn(identity: u32, format: DepthFormat, samples: u32) -> Result<Pixels, Trouble> {
	let limits = Render3DLimits::PROFILE_MINIMUM;
	let colours = [
		RenderTargetView { texture: 1, view: view(Aspect::Colour), format: "RGBA8", samples, width: WIDTH, height: HEIGHT, load: LoadOp::Load, store: StoreOp::Store },
		RenderTargetView { texture: 2, view: view(Aspect::Colour), format: "R32_UINT", samples, width: WIDTH, height: HEIGHT, load: LoadOp::Load, store: StoreOp::Store },
	];
	let depth = DepthStencilView { texture: 3, view: view(Aspect::Depth), format, samples, width: WIDTH, height: HEIGHT, depth_load: LoadOp::Load, depth_store: StoreOp::Store, stencil_load: LoadOp::Load, stencil_store: StoreOp::Store };
	let resolve_views = [Some(RenderTargetView { texture: 4, samples: 1, ..colours[0] }), Some(RenderTargetView { texture: 5, samples: 1, ..colours[1] })];
	let set = RenderTargetSet { colour: &colours, depth_stencil: Some(depth), resolve: if samples > 1 { &resolve_views } else { &[] } };
	let plan = Plan { fragment: fragment(identity), count: 6, samples, depth_test: Some(CompareOp::Less), depth_write: true, ..Plan::default() };
	let near = pipeline_for(&plan, 2);
	let far = pipeline_for(&Plan { fragment: fragment(identity ^ u32::MAX), ..plan }, 2);
	let mut commands = CommandList::new(limits);
	// The first result must stay the clear value even though the same pixel is written later.
	let clear_colours = [RenderTargetView { load: LoadOp::Clear, ..colours[0] }, RenderTargetView { load: LoadOp::Clear, ..colours[1] }];
	let clear_set = RenderTargetSet { colour: &clear_colours, depth_stencil: Some(DepthStencilView { depth_load: LoadOp::Clear, stencil_load: LoadOp::Clear, ..depth }), resolve: &[] };
	commands.begin_render_pass(7, &clear_set)?;
	commands.end_render_pass()?;
	let pixel = scene3d::pick::PickRequest::new(WIDTH / 2, HEIGHT / 2, WIDTH, HEIGHT)?;
	let background = scene3d::pick::record(&mut commands, 7, &set, pixel, scene3d::Readback::Identity, 1, Buffer(10))?;
	commands.begin_render_pass(7, &set)?;
	commands.set_viewport(Rect { x: 0, y: 0, width: WIDTH, height: HEIGHT })?;
	commands.bind_index_buffer(Buffer(100), 0, false)?;
	for (index, pipeline) in [(0, &near), (1, &far)] {
		commands.bind_pipeline(GraphicsPipeline(index), &pipeline.state, samples)?;
		commands.draw_indexed(pipeline.state.topology, 6, 1, 0, 0, 0, 4)?;
	}
	commands.end_render_pass()?;
	let colour = scene3d::pick::record(&mut commands, 7, &set, pixel, scene3d::Readback::Colour, 0, Buffer(11))?;
	let depth_read = scene3d::pick::record(&mut commands, 7, &set, pixel, scene3d::Readback::Depth, 0, Buffer(12))?;
	let id_read = scene3d::pick::record(&mut commands, 7, &set, pixel, scene3d::Readback::Identity, 1, Buffer(13))?;
	let pick_read = scene3d::pick::record(&mut commands, 7, &set, pixel, scene3d::Readback::Identity, 1, Buffer(14))?;
	if samples > 1 {
		for (attachment, destination) in [(ReadbackAttachment::ResolvedColour(0), 15), (ReadbackAttachment::ResolvedIdentity(1), 16)] {
			commands.readback(ReadbackCommand { targets: 7, attachment, x: pixel.x, y: pixel.y, destination: Buffer(destination) }, &set)?;
		}
	}
	commands.finish()?;
	let pipelines = [near, far];
	let mut prepared = soft3d::readback::prepare(&commands, limits, &pipelines, &[TargetLayout { id: 7, width: WIDTH, height: HEIGHT }], 7, &[ClearValues { colour: &[Some(ClearColour::Colour(Vec4::ZERO)), Some(ClearColour::Identity(0))], depth: Some(1.0), stencil: Some(0) }, ClearValues::NONE])?;
	let near_source = quad_at_depth(0.375, 0.25, 0.5, 0.75);
	let far_source = quad_at_depth(0.75, 1.0, 0.0, 0.0);
	let mut storage = [soft3d::Colour::new(WIDTH, HEIGHT, samples, false), soft3d::Colour::new(WIDTH, HEIGHT, samples, true)];
	storage[1].fill_identity(777)?; // The recorded Clear, not the constructor, must establish the background.
	let mut depth_storage = soft3d::DepthStencil::new(WIDTH, HEIGHT, samples, format);
	let mut targets = [TargetBinding { id: 7, attachments: soft3d::Attachments { colour: &mut storage, depth_stencil: Some(&mut depth_storage), viewport: Viewport::new(0.0, 0.0, WIDTH as f32, HEIGHT as f32), scissor: None } }];
	let mut resolved_colour = soft3d::Colour::new(WIDTH, HEIGHT, 1, false);
	let mut resolved_identity = soft3d::Colour::new(WIDTH, HEIGHT, 1, true);
	let mut resolved = [
		ResolveBinding { texture: 4, view: colours[0].view, colour: &mut resolved_colour },
		ResolveBinding { texture: 5, view: colours[1].view, colour: &mut resolved_identity },
	];
	let mut done = prepared.submit_with_resolves(&mut targets, &[&near_source, &far_source], &mut resolved, &soft3d::frame::Serial)?;
	require!(done.submission.status == Status::Complete && done.stats.samples_written > 0, "actual software execution must complete and write pixels");
	let serial = done.submission.completion.serial;
	let background = scene3d::pick::completed(&background.submitted(&done.submission), done.take(10)?)?;
	let colour = scene3d::pick::completed(&colour.submitted(&done.submission), done.take(11)?)?;
	let depth = scene3d::pick::completed(&depth_read.submitted(&done.submission), done.take(12)?)?;
	let id = scene3d::pick::completed(&id_read.submitted(&done.submission), done.take(13)?)?;
	if samples > 1 {
		require!(done.take(15)?.finish()? == colour && done.take(16)?.finish()? == id, "explicit pass resolve destinations and direct typed readbacks agree on the actual MRT result");
	}
	let mut scene = crate::scene::world();
	let expected_pick = if identity == 0 {
		None
	} else {
		let material = scene.add_material(crate::scene::material(scene3d::MaterialKind::Lambert, scene3d::Blending::Opaque))?;
		let node = scene.add_node(scene3d::Node::identity())?;
		Some(scene.add_drawable(scene3d::Drawable::new(node, 0, material).with_id(identity))?)
	};
	let pending_pick = pick_read.submitted(&done.submission);
	// A second genuinely executed list with identical destinations must not satisfy this pick.
	let mut other = soft3d::readback::prepare(&commands, limits, &pipelines, &[TargetLayout { id: 7, width: WIDTH, height: HEIGHT }], 7, &[ClearValues { colour: &[Some(ClearColour::Colour(Vec4::ZERO)), Some(ClearColour::Identity(0))], depth: Some(1.0), stencil: Some(0) }, ClearValues::NONE])?;
	let mut unrelated = other.submit_with_resolves(&mut targets, &[&near_source, &far_source], &mut resolved, &soft3d::frame::Serial)?;
	require!(unrelated.submission.status == Status::Complete, "the unrelated list really executed");
	require!(scene3d::pick::answer_completed(&scene, &pending_pick, unrelated.take(14)?).is_err(), "a different prepared list's actual completion must not satisfy this pick");
	let picked = scene3d::pick::answer_completed(&scene, &pending_pick, done.take(14)?)?;
	require!(done.take(14).is_err(), "a real readback completion is consumed only once");
	let (ReadbackValue::Colour(colour), ReadbackValue::Depth(depth), ReadbackValue::Identity(identity), ReadbackValue::Identity(background)) = (colour, depth, id, background) else {
		return Err(Trouble::Failed(alloc::string::String::from("typed readbacks changed kind")));
	};
	Ok(Pixels { colour, depth, identity, background, picked, expected_pick, serial })
}

// @covers: ReadbackColor
pub fn readback_color() -> Outcome {
	for samples in [1, 4] {
		let pixels = drawn(11, DepthFormat::Depth32F, samples)?;
		require!(close(pixels.colour.x, 0.25) && close(pixels.colour.y, 0.5) && close(pixels.colour.z, 0.75), "recorded colour readback resolves the nearer colour at {samples} samples: {:?}", pixels.colour);
	}
	Ok(())
}

// @covers: ReadbackDepth
pub fn readback_depth() -> Outcome {
	for format in [DepthFormat::Depth16, DepthFormat::Depth32F] {
		let pixels = drawn(11, format, 1)?;
		require!(close(pixels.depth, 0.375), "recorded depth readback normalises {format:?}: {}", pixels.depth);
	}
	Ok(())
}

// @covers: ReadbackObjectId
pub fn readback_object_id() -> Outcome {
	for identity in [0, 0x0100_0001, 0x8000_0001, 0xffff_fffe, u32::MAX] {
		let pixels = drawn(identity, DepthFormat::Depth32F, 4)?;
		require!(pixels.identity == identity, "the nearer exact 32-bit identity survives a farther competing MRT draw: {:#x} != {identity:#x}", pixels.identity);
		require!(pixels.background == 0, "an earlier read in the same list observes the clear before later writes");
		require!(pixels.picked == pixels.expected_pick, "Scene3D resolves the identity from the real completed submission");
	}
	Ok(())
}

pub(crate) fn completed_scene_pick() -> Outcome {
	let pixels = drawn(0xffff_fffe, DepthFormat::Depth16, 1)?;
	require!(pixels.serial != 0 && pixels.picked == pixels.expected_pick && pixels.picked.is_some(), "Scene3D joins its recorded pick to an actual completed software submission");
	Ok(())
}
