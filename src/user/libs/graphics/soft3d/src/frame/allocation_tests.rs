//! Animation and scheduling can change without changing the prepared scene's resources.

use super::*;

struct Animated {
	scale: f32,
	x: f32,
}

impl Source for Animated {
	fn attribute(&self, location: u32, vertex: u32, _instance: u32) -> Option<Val> {
		match location {
			0 => Some(Val::vector_f32(&[[-0.8, -0.8, 0.5, 1.0], [0.8, -0.8, 0.5, 1.0], [0.0, 0.8, 0.5, 1.0]][vertex as usize % 3])),
			1 => Some(Val::vector_f32(&white())),
			_ => None,
		}
	}

	fn uniform(&self, block: u32, member: u32) -> Option<Val> {
		match (block, member) {
			(0, 0) => Some(Val::vector_f32(&[self.scale, self.scale, 1.0, 1.0])),
			(0, 1) => Some(Val::vector_f32(&[self.x, 0.0, 0.0, 0.0])),
			_ => None,
		}
	}

	fn sample(&self, _texture: u32, _sampler: u32, _coordinate: &Val) -> Option<Val> {
		None
	}

	fn indices(&self) -> Indices<'_> {
		Indices::None
	}
}

fn animated_plan(width: u32, height: u32, triangles: u32) -> Prepared {
	animated_topology(width, height, Topology::TriangleList, triangles * 3)
}

fn animated_topology(width: u32, height: u32, topology: Topology, count: u32) -> Prepared {
	let mut vertex = Builder::new(Stage::Vertex, "animated-capacity");
	vertex.varying(0, Type::vec(4), render_shader::Interpolation::Smooth);
	let position = vertex.load(Type::vec(4), Binding::Attribute { location: 0 });
	let scale = vertex.load(Type::vec(4), Binding::Uniform { block: 0, member: 0 });
	let shift = vertex.load(Type::vec(4), Binding::Uniform { block: 0, member: 1 });
	let scaled = vertex.assign(Type::vec(4), Op::Binary(BinaryOp::Multiply, position, scale));
	let position = vertex.assign(Type::vec(4), Op::Binary(BinaryOp::Add, scaled, shift));
	let colour = vertex.load(Type::vec(4), Binding::Attribute { location: 1 });
	vertex.store(Output::Position, position);
	vertex.store(Output::Varying(0), colour);
	let mut state = pipeline(topology);
	state.vertex = vertex.finish();
	frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![state], vec![Draw { first: 0, pipeline: 0, topology, count, instances: 1, first_instance: 0, base_vertex: 0, restart: false }], width, height).unwrap()
}

fn render_pose(plan: &mut Prepared, colour: &mut Colour, pose: &Animated, workers: &dyn frame::Workers) -> frame::Stats {
	colour.fill(Vec4::ZERO);
	let viewport = viewport(colour.width as f32, colour.height as f32);
	let mut attachments = Attachments { colour: core::slice::from_mut(colour), depth_stencil: None, viewport, scissor: None };
	frame::execute_with(plan, &mut attachments, pose, workers).unwrap()
}

#[test]
fn animated_uniforms_visit_unwarmed_tiles_without_allocating() {
	let mut plan = animated_plan(320, 64, 1);
	let mut colour = Colour::new(320, 64, 1, false);
	let mut pose = Animated { scale: 0.2, x: -0.7 };
	for _ in 0..5 {
		render_pose(&mut plan, &mut colour, &pose, &frame::Serial);
	}
	let bytes_before = plan.reserved_bytes();
	pose.x = 0.7;
	let before = crate::counted::count();
	let moved = render_pose(&mut plan, &mut colour, &pose, &frame::Serial);
	let allocated = crate::counted::count() - before;
	let bytes_after = plan.reserved_bytes();
	assert!(moved.fragments > 100);
	assert_eq!(colour.at(48, 32, 0), Vec4::ZERO, "the old position is empty");
	assert!(colour.at(272, 32, 0).x > 0.5, "the same mesh moved to new tiles");
	assert_eq!(allocated, 0, "uniform movement allocated {allocated} times; prepared bytes {bytes_before}->{bytes_after}");
}

#[test]
fn animated_clipping_growth_stays_inside_prepared_capacity() {
	let mut plan = animated_plan(32, 32, 3);
	let mut colour = Colour::new(32, 32, 1, false);
	let mut pose = Animated { scale: 0.25, x: 0.0 };
	for _ in 0..5 {
		render_pose(&mut plan, &mut colour, &pose, &frame::Serial);
	}
	let scratch_before = plan.scratch_capacity();
	pose.scale = 4.0;
	let before = crate::counted::count();
	let clipped = render_pose(&mut plan, &mut colour, &pose, &frame::Serial);
	let allocated = crate::counted::count() - before;
	let scratch_after = plan.scratch_capacity();
	assert_eq!(clipped.clipped, 3, "each original triangle becomes a larger clipped fan");
	assert!(clipped.fragments > 1000, "clipped triangles still cover the target");
	assert_eq!(allocated, 0, "clipping allocated {allocated} times; scratch slots {scratch_before}->{scratch_after}");
}

struct SelectedLane(core::cell::Cell<usize>);

impl frame::Workers for SelectedLane {
	fn lanes(&self) -> usize {
		2
	}

	fn run<'t, 'a>(&self, lanes: &mut [frame::Lane], tiles: &mut [frame::Tile<'t, 'a>], work: &(dyn Fn(&mut frame::Lane, &mut frame::Tile<'t, 'a>) + Sync)) {
		for tile in tiles {
			work(&mut lanes[self.0.get()], tile);
		}
	}
}

#[test]
fn a_previously_idle_lane_shades_without_allocating() {
	let mut plan = animated_plan(64, 64, 1);
	let mut colour = Colour::new(64, 64, 1, false);
	let pose = Animated { scale: 1.0, x: 0.0 };
	let workers = SelectedLane(core::cell::Cell::new(0));
	let mut previous = frame::Stats::default();
	for _ in 0..5 {
		previous = render_pose(&mut plan, &mut colour, &pose, &workers);
	}
	workers.0.set(1);
	let before = crate::counted::count();
	let current = render_pose(&mut plan, &mut colour, &pose, &workers);
	let allocated = crate::counted::count() - before;
	assert_eq!(current, previous, "scheduling cannot change the work");
	assert!(current.fragments > 100);
	assert_eq!(allocated, 0, "a different existing lane allocated {allocated} times");
}

fn entering_view(topology: Topology, count: u32) {
	let mut plan = animated_topology(64, 64, topology, count);
	let mut colour = Colour::new(64, 64, 1, false);
	let mut pose = Animated { scale: 1.0, x: 4.0 };
	let workers = SelectedLane(core::cell::Cell::new(0));
	for _ in 0..5 {
		let hidden = render_pose(&mut plan, &mut colour, &pose, &workers);
		assert_eq!(hidden.fragments, 0);
	}
	pose.x = 0.0;
	workers.0.set(1);
	let before = crate::counted::count();
	let visible = render_pose(&mut plan, &mut colour, &pose, &workers);
	let allocated = crate::counted::count() - before;
	assert!(visible.fragments > 0, "the same resources now draw visible {topology:?}");
	assert_eq!(allocated, 0, "previously clipped-out {topology:?} allocated {allocated} times when entering view");
}

#[test]
fn a_previously_clipped_triangle_enters_view_without_allocating() {
	entering_view(Topology::TriangleList, 3);
}

#[test]
fn a_previously_clipped_line_enters_view_without_allocating() {
	entering_view(Topology::LineList, 2);
}

#[test]
fn a_previously_clipped_point_enters_view_without_allocating() {
	entering_view(Topology::PointList, 1);
}

#[test]
fn a_refused_frame_reservation_preserves_the_target_and_can_retry() {
	let mut plan = animated_plan(64, 64, 1);
	let mut colour = Colour::new(64, 64, 1, false);
	colour.fill(crate::pass::POISON);
	let pose = Animated { scale: 1.0, x: 0.0 };
	let mut attachments = Attachments { colour: core::slice::from_mut(&mut colour), depth_stencil: None, viewport: viewport(64.0, 64.0), scissor: None };
	let refused = crate::counted::fail_after(0, || frame::execute(&mut plan, &mut attachments, &pose));
	assert!(matches!(refused, Err(render3d::Error::OutOfMemory { .. })), "{refused:?}");
	assert!((0..64).all(|y| (0..64).all(|x| colour.at(x, y, 0) == crate::pass::POISON)), "reservation failure happens before attachment writes");
	let retried = render_pose(&mut plan, &mut colour, &pose, &frame::Serial);
	assert!(retried.fragments > 100, "the same prepared frame recovers after memory becomes available");
}

#[test]
fn invalid_geometry_after_a_large_valid_prefix_writes_no_pixels() {
	let mut positions = Vec::new();
	for _ in 0..256 {
		positions.extend_from_slice(&[[-0.8, -0.8, 0.5, 1.0], [0.8, -0.8, 0.5, 1.0], [0.0, 0.8, 0.5, 1.0]]);
	}
	let mesh = Mesh { colours: vec![white(); positions.len()], positions, instance_offset: [0.0; 4] };
	let draw = Draw { first: 0, pipeline: 0, topology: Topology::TriangleList, count: 257 * 3, instances: 1, first_instance: 0, base_vertex: 0, restart: false };
	let mut plan = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline(Topology::TriangleList)], vec![draw], 64, 32).unwrap();
	let mut colour = Colour::new(64, 32, 1, false);
	colour.fill(crate::pass::POISON);
	let mut attachments = Attachments { colour: core::slice::from_mut(&mut colour), depth_stencil: None, viewport: viewport(64.0, 32.0), scissor: None };
	assert!(matches!(frame::execute(&mut plan, &mut attachments, &mesh), Err(render3d::Error::InvalidShader { .. })), "the final triangle's vertex binding is missing");
	assert!((0..32).all(|y| (0..64).all(|x| colour.at(x, y, 0) == crate::pass::POISON)), "geometry must be validated before any part of the draw shades");
}

#[test]
fn a_lower_tile_error_in_later_geometry_keeps_global_error_priority() {
	let mut positions = Vec::new();
	for _ in 0..256 {
		positions.extend_from_slice(&[[0.1, -0.8, 0.5, 1.0], [0.9, -0.8, 0.5, 1.0], [0.5, 0.8, 0.5, 1.0]]);
	}
	positions.extend_from_slice(&[[-0.9, -0.8, 0.5, 1.0], [-0.1, -0.8, 0.5, 1.0], [-0.5, 0.8, 0.5, 1.0]]);
	let mesh = Mesh { colours: vec![white(); positions.len()], positions, instance_offset: [0.0; 4] };
	let mut fragment = Builder::new(Stage::Fragment, "tile-priority-across-geometry");
	let coordinate = fragment.load(Type::vec(4), Binding::BuiltIn(BuiltIn::FragmentCoordinate));
	let x = fragment.assign(Type::f32(), Op::Extract(coordinate, 0));
	let middle = fragment.constant(Constant::F32(32.0));
	let right = fragment.assign(Type::Scalar(ScalarType::Bool), Op::Compare(CompareKind::Greater, x, middle));
	let branch = fragment.if_then(right);
	let _ = fragment.assign(Type::vec(4), Op::Sample { binding: Binding::Texture { texture: 0, sampler: 0 }, coordinate });
	let branch = fragment.else_branch(branch);
	let _ = fragment.load(Type::vec(4), Binding::Uniform { block: 99, member: 0 });
	fragment.end_if(branch);
	fragment.store(Output::Colour(0), coordinate);
	let mut state = pipeline(Topology::TriangleList);
	state.fragment = fragment.finish();
	let draw = Draw { first: 0, pipeline: 0, topology: Topology::TriangleList, count: 257 * 3, instances: 1, first_instance: 0, base_vertex: 0, restart: false };
	let mut plan = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![state], vec![draw], 64, 32).unwrap();
	let mut colour = Colour::new(64, 32, 1, false);
	for workers in [&frame::Serial as &dyn frame::Workers, &Backwards] {
		let mut attachments = Attachments { colour: core::slice::from_mut(&mut colour), depth_stencil: None, viewport: viewport(64.0, 32.0), scissor: None };
		assert_eq!(frame::execute_with(&mut plan, &mut attachments, &mesh, workers), Err(render3d::Error::InvalidShader { reason: "a shader read a binding the frame does not supply" }), "the later left tile's binding failure precedes the earlier right tile's texture failure");
	}
}

#[test]
fn overflowing_tile_indices_preserve_blend_depth_order_and_do_not_allocate_on_motion() {
	const TRIANGLES: u32 = 257;
	let mut positions = Vec::new();
	let mut colours = Vec::new();
	for triangle in 0..TRIANGLES {
		let z = 0.9 - triangle as f32 * 0.001;
		positions.extend_from_slice(&[[-0.9, -0.7, z, 1.0], [-0.6, -0.7, z, 1.0], [-0.75, 0.7, z, 1.0]]);
		let colour = if triangle % 2 == 0 { [0.8, 0.1, 0.3, 0.4] } else { [0.1, 0.7, 0.2, 0.3] };
		colours.extend_from_slice(&[colour; 3]);
	}
	let mut mesh = Mesh { positions, colours, instance_offset: [0.0; 4] };
	let draw = Draw { first: 0, pipeline: 0, topology: Topology::TriangleList, count: TRIANGLES * 3, instances: 1, first_instance: 1, base_vertex: 0, restart: false };
	let mut state = pipeline(Topology::TriangleList);
	state.blend[0] = AttachmentBlend { enabled: true, colour: BlendEquation { source: render3d::BlendFactor::SrcAlpha, destination: render3d::BlendFactor::OneMinusSrcAlpha, operation: render3d::BlendOp::Add }, alpha: BlendEquation { source: render3d::BlendFactor::One, destination: render3d::BlendFactor::OneMinusSrcAlpha, operation: render3d::BlendOp::Add }, write_mask: ColorWriteMask::ALL };
	let mut overflow = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![state.clone()], vec![draw], 128, 32).unwrap();
	// One primitive per draw never overflows a tile's index cache. It executes the same ordered
	// blending/depth work through the ordinary fast path, giving an independent storage oracle.
	let draws = (0..TRIANGLES).map(|triangle| Draw { first: triangle * 3, count: 3, ..draw }).collect();
	let mut reference = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![state], draws, 128, 32).unwrap();
	let mut colour = Colour::new(128, 32, 1, false);
	let mut depth = DepthStencil::new(128, 32, 1, DepthFormat::Depth32F);
	let mut expected_colour = Colour::new(128, 32, 1, false);
	let mut expected_depth = DepthStencil::new(128, 32, 1, DepthFormat::Depth32F);
	let once = |plan: &mut Prepared, colour: &mut Colour, depth: &mut DepthStencil, mesh: &Mesh| {
		colour.fill(Vec4::ZERO);
		depth.clear(1.0, 0);
		let mut attachments = Attachments { colour: core::slice::from_mut(colour), depth_stencil: Some(depth), viewport: viewport(128.0, 32.0), scissor: None };
		frame::execute(plan, &mut attachments, mesh).unwrap()
	};
	for _ in 0..5 {
		once(&mut overflow, &mut colour, &mut depth, &mesh);
	}
	for x in [0.0, 1.4, 0.0, 1.4] {
		mesh.instance_offset[0] = x;
		let before = crate::counted::count();
		let actual = once(&mut overflow, &mut colour, &mut depth, &mesh);
		let allocated = crate::counted::count() - before;
		let expected = once(&mut reference, &mut expected_colour, &mut expected_depth, &mesh);
		assert_eq!(actual, expected, "overflow visits each primitive exactly once in order");
		assert_eq!(actual.primitives, TRIANGLES);
		assert!(actual.samples_written > TRIANGLES);
		for y in 0..32 {
			for x in 0..128 {
				assert_eq!(colour.at(x, y, 0), expected_colour.at(x, y, 0), "blend at {x},{y}");
				assert_eq!(depth.depth_at(x, y, 0), expected_depth.depth_at(x, y, 0), "depth at {x},{y}");
			}
		}
		assert_eq!(allocated, 0, "moving overflow geometry allocated {allocated} times");
	}
}

#[test]
fn extended_sized_active_plans_report_bounded_geometry_reservation() {
	// Match the demo's 48*96*2 sphere triangles plus two ground triangles and its largest
	// measured lighting/shadow extents. This deliberately uses the small valid shader above,
	// not the demo's PBR/shadow modules; the result forecasts geometry capacity, not guest heap.
	const TRIANGLES: u32 = 9_218;
	let mut lighting = animated_plan(800, 600, TRIANGLES);
	let mut shadow = animated_plan(512, 512, TRIANGLES);
	let inactive_lighting = lighting.reserved_bytes();
	let inactive_shadow = shadow.reserved_bytes();
	assert_eq!(lighting.scratch_capacity(), 0, "inactive lighting does not reserve large geometry scratch");
	assert_eq!(shadow.scratch_capacity(), 0, "inactive shadow does not reserve large geometry scratch");
	let mut colour = [Colour::new(800, 600, 1, false), Colour::new(800, 600, 1, true)];
	let mut depth = DepthStencil::new(800, 600, 1, DepthFormat::Depth32F);
	let mut shadow_colour = [Colour::new(512, 512, 1, false)];
	let mut shadow_depth = DepthStencil::new(512, 512, 1, DepthFormat::Depth32F);
	let mut pose = Animated { scale: 0.2, x: 4.0 };
	let workers = Rotating(32);
	let once = |plan: &mut Prepared, colour: &mut [Colour], depth: &mut DepthStencil, source: &Animated| {
		let viewport = viewport(depth.width as f32, depth.height as f32);
		let mut attachments = Attachments { colour, depth_stencil: Some(depth), viewport, scissor: None };
		frame::execute_with(plan, &mut attachments, source, &workers).unwrap()
	};
	let a = once(&mut lighting, &mut colour, &mut depth, &pose);
	let b = once(&mut shadow, &mut shadow_colour, &mut shadow_depth, &pose);
	for stats in [a, b] {
		assert_eq!(stats.primitives, TRIANGLES, "every vertex binding is valid and assembled");
		assert_eq!(stats.culled, TRIANGLES);
		assert_eq!(stats.fragments, 0, "hidden geometry avoids a heavy shading benchmark");
	}
	let active_lighting = lighting.reserved_bytes();
	let active_shadow = shadow.reserved_bytes();
	pose.x = -4.0;
	let before = crate::counted::count();
	let a = once(&mut lighting, &mut colour, &mut depth, &pose);
	let b = once(&mut shadow, &mut shadow_colour, &mut shadow_depth, &pose);
	let allocated = crate::counted::count() - before;
	assert_eq!(a.culled, TRIANGLES);
	assert_eq!(b.culled, TRIANGLES);
	assert_eq!(allocated, 0, "reusing the two active plans reserves nothing more");
	assert_eq!(lighting.reserved_bytes(), active_lighting);
	assert_eq!(shadow.reserved_bytes(), active_shadow);
	std::println!("representative 9218-triangle, 32-lane reservation: inactive lighting800x600={inactive_lighting} shadow512x512={inactive_shadow}; active lighting={active_lighting} shadow={active_shadow} total={}; warmed allocations={allocated}; simple shaders, borrowed attachments excluded", active_lighting + active_shadow);
}
