use super::*;
use alloc::vec;
use render_shader::builder::Builder;
use render_shader::ir::{Binding, Output, Type};
use render3d::{Cull, StencilOp};

struct Input {
	positions: Vec<[f32; 4]>,
	depth: f32,
}
impl Source for Input {
	fn attribute(&self, location: u32, vertex: u32, _: u32) -> Option<Val> {
		(location == 0).then(|| self.positions.get(vertex as usize).map(|value| Val::vector_f32(value))).flatten()
	}
	fn uniform(&self, _: u32, member: u32) -> Option<Val> {
		Some(if member == 0 { Val::scalar_f32(self.depth) } else { Val::vector_f32(&[1.0; 4]) })
	}
	fn sample(&self, _: u32, _: u32, _: &Val) -> Option<Val> {
		None
	}
	fn indices(&self) -> Indices<'_> {
		Indices::None
	}
}

fn pipeline(topology: Topology, samples: u32, explicit_depth: bool) -> Pipeline {
	let mut vertex = Builder::new(Stage::Vertex, "depth-only-vertex");
	let position = vertex.load(Type::vec(4), Binding::Attribute { location: 0 });
	vertex.store(Output::Position, position);
	let mut fragment = Builder::new(Stage::Fragment, "depth-only-fragment");
	if explicit_depth {
		let depth = fragment.load(Type::f32(), Binding::Uniform { block: 0, member: 0 });
		fragment.store(Output::Depth, depth);
	} else {
		let colour = fragment.load(Type::vec(4), Binding::Uniform { block: 0, member: 1 });
		fragment.store(Output::Colour(0), colour);
	}
	Pipeline { state: PipelineState { topology, cull: Cull::None, depth_test: Some(CompareOp::Less), depth_write: true, samples, per_sample_shading: false }, vertex: vertex.finish(), fragment: fragment.finish(), blend: vec![], stencil: None, stencil_back: None, depth_compare: CompareOp::Less, depth_write: true, bias: (0.0, 0.0, 0.0), alpha_to_coverage: false, sample_mask: u32::MAX }
}

fn plan(pipeline: Pipeline, count: u32) -> Prepared {
	let draw = Draw { pipeline: 0, topology: pipeline.state.topology, first: 0, count, instances: 1, first_instance: 0, base_vertex: 0, restart: false };
	prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline], vec![draw], 64, 64).unwrap()
}

fn quad(depth: f32) -> Input {
	Input { positions: vec![[-1.0, -1.0, 0.6, 1.0], [1.0, -1.0, 0.6, 1.0], [1.0, 1.0, 0.6, 1.0], [-1.0, -1.0, 0.6, 1.0], [1.0, 1.0, 0.6, 1.0], [-1.0, 1.0, 0.6, 1.0]], depth }
}

struct Reverse;
impl Workers for Reverse {
	fn lanes(&self) -> usize {
		4
	}
	fn run<'t, 'a>(&self, lanes: &mut [Lane], tiles: &mut [Tile<'t, 'a>], work: &(dyn Fn(&mut Lane, &mut Tile<'t, 'a>) + Sync)) {
		for (index, tile) in tiles.iter_mut().rev().enumerate() {
			work(&mut lanes[index % 4], tile);
		}
	}
}

#[test]
fn depth_only_tiles_preserve_masks_stencil_occlusion_and_scheduling_order() {
	for workers in [&Serial as &dyn Workers, &Reverse] {
		let mut state = pipeline(Topology::TriangleList, 4, true);
		state.sample_mask = 0b0101;
		state.stencil = Some(StencilFace { reference: 7, on_pass: StencilOp::Replace, on_depth_fail: StencilOp::IncrementClamp, ..StencilFace::default() });
		let mut prepared = plan(state, 6);
		let mut depth = DepthStencil::new(64, 64, 4, DepthFormat::Depth32FStencil8);
		depth.clear(1.0, 0);
		let mut empty = [];
		let mut attachments = Attachments { colour: &mut empty, depth_stencil: Some(&mut depth), viewport: Viewport::new(0.0, 0.0, 64.0, 64.0), scissor: Some(Scissor { x: 8, y: 8, width: 48, height: 48 }) };
		let first = execute_with(&mut prepared, &mut attachments, &quad(0.25), workers).unwrap();
		assert_eq!(first.samples_written, 48 * 48 * 2);
		let farther = execute_with(&mut prepared, &mut attachments, &quad(0.75), workers).unwrap();
		assert_eq!(farther.samples_written, 0);
		for y in 0..64 {
			for x in 0..64 {
				for sample in 0..4 {
					let covered = (8..56).contains(&x) && (8..56).contains(&y) && sample % 2 == 0;
					assert_eq!(depth.depth_at(x, y, sample), render3d::depth::Stored::Float(if covered { 0.25 } else { 1.0 }));
					assert_eq!(depth.stencil_at(x, y, sample), if covered { 8 } else { 0 });
				}
			}
		}
	}
}

#[test]
fn depth_only_lines_and_points_use_shader_depth_without_a_colour_target() {
	// Pass through pixel centres; y=0 is between rows, where the frozen open-diamond rule
	// correctly covers none of the line's boundary-only contacts.
	for (topology, positions) in [(Topology::LineList, vec![[-0.8, 0.015625, 0.6, 1.0], [0.8, 0.015625, 0.6, 1.0]]), (Topology::PointList, vec![[0.0, 0.0, 0.6, 1.0]])] {
		let mut prepared = plan(pipeline(topology, 1, true), positions.len() as u32);
		let source = Input { positions, depth: 0.125 };
		let mut depth = DepthStencil::new(64, 64, 1, DepthFormat::Depth32F);
		let mut empty = [];
		let mut attachments = Attachments { colour: &mut empty, depth_stencil: Some(&mut depth), viewport: Viewport::new(0.0, 0.0, 64.0, 64.0), scissor: None };
		let stats = execute(&mut prepared, &mut attachments, &source).unwrap();
		assert!(stats.samples_written > 0, "{topology:?} must reach the depth test");
		let mut changed = 0;
		for y in 0..64 {
			for x in 0..64 {
				let value = render3d::depth::readback(DepthFormat::Depth32F, depth.depth_at(x, y, 0));
				assert!(value == 1.0 || value == 0.125, "shader depth, never the interpolated 0.6: {value}");
				changed += u32::from(value == 0.125);
			}
		}
		assert_eq!(stats.samples_written, changed);
	}
}

#[test]
fn depth_only_rejects_undefined_reads_before_early_culling() {
	let mut prepared = plan(pipeline(Topology::TriangleList, 1, false), 6);
	let mut depth = DepthStencil::new(64, 64, 1, DepthFormat::Depth32F);
	depth.clear(0.0, 0);
	depth.discard_depth();
	let mut empty = [];
	let mut attachments = Attachments { colour: &mut empty, depth_stencil: Some(&mut depth), viewport: Viewport::new(0.0, 0.0, 64.0, 64.0), scissor: None };
	assert!(matches!(execute(&mut prepared, &mut attachments, &quad(0.25)), Err(Error::InvalidRenderState { .. })), "the unreadable nearer depth must not silently cull the draw");
}

#[test]
fn missing_attachments_and_sample_mismatch_are_typed_refusals() {
	let mut prepared = plan(pipeline(Topology::TriangleList, 4, true), 6);
	let mut empty = [];
	let mut attachments = Attachments { colour: &mut empty, depth_stencil: None, viewport: Viewport::new(0.0, 0.0, 64.0, 64.0), scissor: None };
	assert!(matches!(execute(&mut prepared, &mut attachments, &quad(0.25)), Err(Error::TargetMismatch { .. })));
	let mut depth = DepthStencil::new(64, 64, 1, DepthFormat::Depth32F);
	attachments.depth_stencil = Some(&mut depth);
	assert!(matches!(execute(&mut prepared, &mut attachments, &quad(0.25)), Err(Error::IncompatiblePipeline { .. })));
	assert_eq!(depth.depth_at(32, 32, 0), render3d::depth::Stored::Float(1.0));
}
