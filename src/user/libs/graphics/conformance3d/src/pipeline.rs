//! THE PIPELINE GROUP: the two programmable stages, and the fixed state around them.
//!
//! WHAT A PIPELINE FEATURE MEANS IS THAT A STATE CHANGES THE PICTURE. Every scene here renders the
//! same geometry twice - once with the state and once without - and states the difference, because a
//! state a backend accepts and ignores passes every check that only looks at one frame.

use crate::Outcome;
use crate::harness::{Plan, Scene, Vertex, close, full_quad, render};
use alloc::vec;
use render_math::Vec4;
use render_math::camera::Viewport;
use render_shader::builder::Builder;
use render_shader::ir::{BinaryOp, Binding, Constant, Op, Output, Stage, Type};
use render3d::{CompareOp, Cull, Topology};

pub fn programmable_vertex_stage() -> Outcome {
	// A VERTEX STAGE COMPUTES THE POSITION, which is what makes it programmable rather than a fixed
	// transform with parameters: this one halves every coordinate, and the triangle it draws is half
	// the size of the one its vertices describe.
	let mut builder = Builder::new(Stage::Vertex, "halving");
	for location in 0..4 {
		builder.varying(location, Type::vec(4), render_shader::Interpolation::Smooth);
	}
	let position = builder.load(Type::vec(4), Binding::Attribute { location: 0 });
	let colour = builder.load(Type::vec(4), Binding::Attribute { location: 1 });
	// X AND Y ONLY, AND THAT IS THE POINT OF THE SCENE. Scaling all FOUR components scales `w` too,
	// and `x/w` is then what it was: a homogeneous scale is the identity after the perspective divide.
	// A stage that halves the picture has to leave `w` alone, and a suite that scaled all four would
	// pass against a backend that ignored the vertex stage entirely.
	let half = builder.constant(Constant::F32(0.5));
	let one = builder.constant(Constant::F32(1.0));
	let halved = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![half, half, one, one]));
	let scaled = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Multiply, position, halved));
	builder.store(Output::Position, scaled);
	builder.store(Output::Varying(0), colour);
	builder.store(Output::Varying(1), colour);
	builder.store(Output::Varying(2), colour);
	builder.store(Output::Varying(3), colour);
	let vertex = builder.finish();

	let scene = full_quad(1.0, 1.0, 1.0);
	let plain = render(&scene, &Plan { count: 6, ..Plan::default() })?;
	let halved_frame = render(&scene, &Plan { count: 6, vertex, identity_attachment: false, ..Plan::default() })?;
	require!(plain.covered_pixels() > halved_frame.covered_pixels() * 3, "a stage that halves every position draws a quarter of the area: {} against {}", plain.covered_pixels(), halved_frame.covered_pixels());
	require!(halved_frame.covered_pixels() > 0, "and it still draws something");

	// AND WHAT THE STAGE PRODUCES SURVIVES CLIPPING. A vertex stage may put a primitive anywhere,
	// including across the edge of the clip volume, and what comes out of the clipper is a FAN of
	// triangles over vertices the stage never wrote - so every varying it did write has to arrive at
	// the fragments of those new triangles under its own qualifier. This is the half of the stage that
	// a scene drawing wholly inside the volume never reaches.
	clipping_preserves_every_qualifier()?;
	// AND THE POSITIONS MAY COME THROUGH A TRANSCENDENTAL, which is where the strict-float rule bites.
	strict_transcendental_vertices_straddle_a_clip_boundary()
}

/// A CLIPPED, FORESHORTENED TRIANGLE WITH ONE VARYING OF EACH QUALIFIER.
///
/// THE THREE QUALIFIERS DISAGREE ONLY WHEN `w` DIFFERS ACROSS THE PRIMITIVE, so the triangle has two
/// vertices four times as far away as the third: `smooth` divides by the interpolated `1/w` and
/// `noperspective` does not, and a backend that implemented one as the other would be right on every
/// primitive parallel to the screen. And `flat` takes the PROVOKING VERTEX'S ORIGINAL VALUE - not a
/// value interpolated to a clipped vertex, which is what a clipper that treats flat like smooth
/// produces and what makes a fan-triangulated polygon change colour across its own interior.
fn clipping_preserves_every_qualifier() -> Outcome {
	let mut vertex = Builder::new(Stage::Vertex, "qualifiers");
	vertex.varying(0, Type::vec(4), render_shader::Interpolation::Smooth);
	vertex.varying_at(1, Type::vec(4), render_shader::Interpolation::NoPerspective, render_shader::ir::Sampling::Pixel);
	vertex.varying_at(2, Type::vec(4), render_shader::Interpolation::Flat, render_shader::ir::Sampling::Pixel);
	let position = vertex.load(Type::vec(4), Binding::Attribute { location: 0 });
	let colour = vertex.load(Type::vec(4), Binding::Attribute { location: 1 });
	vertex.store(Output::Position, position);
	vertex.store(Output::Varying(0), colour);
	vertex.store(Output::Varying(1), colour);
	vertex.store(Output::Varying(2), colour);
	let vertex = vertex.finish();

	// The fragment stage answers all three at once: smooth in red, screen-linear in green, flat in
	// blue, so one pixel carries the whole comparison.
	let mut fragment = Builder::new(Stage::Fragment, "qualifiers");
	fragment.varying(0, Type::vec(4), render_shader::Interpolation::Smooth);
	fragment.varying_at(1, Type::vec(4), render_shader::Interpolation::NoPerspective, render_shader::ir::Sampling::Pixel);
	fragment.varying_at(2, Type::vec(4), render_shader::Interpolation::Flat, render_shader::ir::Sampling::Pixel);
	let smooth = fragment.load(Type::vec(4), Binding::Varying { location: 0 });
	let screen = fragment.load(Type::vec(4), Binding::Varying { location: 1 });
	let flat = fragment.load(Type::vec(4), Binding::Varying { location: 2 });
	let red = fragment.assign(Type::f32(), Op::Extract(smooth, 0));
	let green = fragment.assign(Type::f32(), Op::Extract(screen, 0));
	let blue = fragment.assign(Type::f32(), Op::Extract(flat, 0));
	let one = fragment.constant(Constant::F32(1.0));
	let composed = fragment.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![red, green, blue, one]));
	fragment.store(Output::Colour(0), composed);
	let fragment = fragment.finish();

	// THE TRIANGLE REACHES WELL OUTSIDE THE VOLUME, so it is clipped and fan-triangulated rather than
	// drawn as it was given. The attribute is ZERO at the near vertex and ONE at both far ones, and
	// the first vertex is the provoking one.
	let scene = Scene::new(vec![
		Vertex { position: [-1.0, -1.0, 0.5, 1.0], colour: [0.0, 0.0, 0.0, 1.0], uv: [0.0; 4], ident: [0.0; 4] },
		Vertex { position: [12.0, -12.0, 2.0, 4.0], colour: [1.0, 1.0, 1.0, 1.0], uv: [0.0; 4], ident: [0.0; 4] },
		Vertex { position: [-12.0, 12.0, 2.0, 4.0], colour: [1.0, 1.0, 1.0, 1.0], uv: [0.0; 4], ident: [0.0; 4] },
	]);
	let frame = render(&scene, &Plan { count: 3, vertex, fragment, identity_attachment: false, ..Plan::default() })?;
	// THE CLIPPER REALLY RAN. Without this the scene would still pass against a triangle that fitted
	// inside the volume, and what it is about - what survives being cut up - would not have happened.
	require!(frame.stats.clipped > 0, "the triangle left the clip volume and was clipped, and {} were", frame.stats.clipped);
	require!(frame.stats.primitives == 1, "from one primitive: {}", frame.stats.primitives);

	// A pixel well inside the drawn area, away from every edge, so the comparison is about the
	// interpolation and not about coverage.
	let mut lit = alloc::vec::Vec::new();
	for y in 4..crate::harness::HEIGHT - 4 {
		for x in 4..crate::harness::WIDTH - 4 {
			if frame.covered(x, y) {
				lit.push((x, y));
			}
		}
	}
	require!(!lit.is_empty(), "the clipped triangle covered something");
	let (x, y) = lit[lit.len() / 2];
	let pixel = frame.pixel(x, y);
	// THE PERSPECTIVE-CORRECT VALUE IS NEARER THE NEAR VERTEX'S than the screen-linear one, because
	// the far vertices' contribution is weighted down by their `1/w`.
	require!(pixel.x < pixel.y - 0.02, "smooth is below screen-linear on a foreshortened triangle at ({x}, {y}): {pixel:?}");
	require!(pixel.y > 0.0 && pixel.y < 1.0, "and the screen-linear one is between the two vertex values: {pixel:?}");
	// AND THE FLAT ONE IS THE PROVOKING VERTEX'S ORIGINAL VALUE, EXACTLY - not a value interpolated to
	// a vertex the clipper invented, which is what makes a clipped polygon keep ONE flat colour.
	require!(close(pixel.z, 0.0), "flat is the first vertex's own value after clipping and fan triangulation, which is zero: {pixel:?}");

	// AND IT IS THAT VALUE EVERYWHERE, across every triangle of the fan: a clipper that interpolated
	// flat varyings would give the far side of the polygon a different one.
	for (x, y) in &lit {
		require!(close(frame.pixel(*x, *y).z, 0.0), "flat is the same value over the whole clipped polygon, and is {} at ({x}, {y})", frame.pixel(*x, *y).z);
	}
	Ok(())
}

/// STRICT TRANSCENDENTAL-DERIVED VERTICES ON BOTH SIDES OF A CLIP BOUNDARY.
///
/// THIS IS THE CASE THE WHOLE STRICT-FLOAT RULE EXISTS FOR, and it is only a claim when the same
/// scene runs on more than one architecture: a vertex whose position came out of a transcendental
/// lands a fraction of a unit either side of a clip plane, and two backends whose answer differs by
/// one ULP CLASSIFY IT DIFFERENTLY - one clips the triangle and the other does not, which is a
/// different picture and not a different rounding.
///
/// SO THE PROFILE ALLOWS ONLY THE ZERO-ULP ONES ON A POSITION'S DEPENDENCY SLICE. `sqrt` is correctly
/// rounded and is allowed; `normalize` is a FROZEN COMPOSITION - `v / length(v)`, a division and not
/// a multiply by `inversesqrt` - and is allowed; `sin` carries four ULP and a position that depends
/// on it is refused at module load. This scene puts a vertex on the plane through each of the two
/// that are allowed, and requires the one that is not to be refused.
///
/// WHAT IT CAN CHECK IN ONE RUN is that the boundary was actually straddled, that the answer is
/// reproducible bit for bit, and that the REFUSAL is on the path a backend takes rather than only in
/// a validator a backend might not call. What makes it a CROSS-ARCHITECTURE case is that the suite
/// runs on all three ports and the pass condition is the same numbers.
fn strict_transcendental_vertices_straddle_a_clip_boundary() -> Outcome {
	// `x = sqrt(a) * sx`, `y = sqrt(b) * sy`, so both coordinates are on the transcendental's own
	// dependency slice and nothing else is.
	let rooted = |which: render_shader::Transcendental| {
		let mut builder = Builder::new(Stage::Vertex, "strict-transcendental");
		for location in 0..3 {
			builder.varying(location, Type::vec(4), render_shader::Interpolation::Smooth);
		}
		builder.varying_at(3, Type::vec(4), render_shader::Interpolation::Flat, render_shader::ir::Sampling::Pixel);
		let attribute = builder.load(Type::vec(4), Binding::Attribute { location: 0 });
		let colour = builder.load(Type::vec(4), Binding::Attribute { location: 1 });
		let uv = builder.load(Type::vec(4), Binding::Attribute { location: 2 });
		let ident = builder.load(Type::vec(4), Binding::Attribute { location: 3 });
		let take = |builder: &mut Builder, index: u8| builder.assign(Type::f32(), Op::Extract(attribute, index));
		let a = take(&mut builder, 0);
		let b = take(&mut builder, 1);
		let scale_x = take(&mut builder, 2);
		let scale_y = take(&mut builder, 3);
		let root_a = builder.assign(Type::f32(), Op::Transcendental(which, a, None));
		let root_b = builder.assign(Type::f32(), Op::Transcendental(which, b, None));
		let x = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, root_a, scale_x));
		let y = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, root_b, scale_y));
		let depth = builder.constant(Constant::F32(0.5));
		let one = builder.constant(Constant::F32(1.0));
		let position = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![x, y, depth, one]));
		builder.store(Output::Position, position);
		builder.store(Output::Varying(0), colour);
		builder.store(Output::Varying(1), uv);
		builder.store(Output::Varying(2), position);
		builder.store(Output::Varying(3), ident);
		builder.finish()
	};

	// ONE VERTEX OUTSIDE AND TWO INSIDE. `sqrt(2) * 0.98994949` is 1.39999998, which is past the right
	// plane; `sqrt(0.5) * 1.4` is 0.98994946, which is not - so the triangle is cut, and the cut is
	// where the root decided it was.
	let vertex_at = |a: f32, b: f32, sx: f32, sy: f32| Vertex { position: [a, b, sx, sy], colour: [1.0, 1.0, 1.0, 1.0], uv: [0.0; 4], ident: [0.0; 4] };
	let scene = Scene::new(vec![vertex_at(2.0, 0.25, 0.98994949, 1.0), vertex_at(0.5, 0.25, 1.4, -1.0), vertex_at(0.25, 1.0, -2.0, -0.5)]);
	let plan = || Plan { count: 3, vertex: rooted(render_shader::Transcendental::Sqrt), identity_attachment: false, ..Plan::default() };
	let frame = render(&scene, &plan())?;
	require!(frame.stats.clipped > 0, "a triangle whose vertices came out of sqrt straddles the clip plane and is cut, and {} were", frame.stats.clipped);
	require!(frame.covered_pixels() > 0, "and what survives the cut is drawn");
	// THE CUT IS AT THE PLANE AND NOT SHORT OF IT: the surviving geometry reaches the last column of
	// the target, which is where `x = 1` lands.
	require!((0..crate::harness::HEIGHT).any(|y| frame.covered(crate::harness::WIDTH - 1, y)), "the clipped edge sits on the boundary rather than short of it");

	// AND THE ANSWER IS REPRODUCIBLE, which is the property the rule is about: the same scene twice is
	// the same frame, pixel for pixel and BIT for bit - not within a tolerance, because a coverage
	// decision has no tolerance.
	let again = render(&scene, &plan())?;
	for y in 0..crate::harness::HEIGHT {
		for x in 0..crate::harness::WIDTH {
			require!(frame.pixel(x, y) == again.pixel(x, y), "the same scene renders the same frame at ({x}, {y}): {:?} against {:?}", frame.pixel(x, y), again.pixel(x, y));
		}
	}

	// `normalize` IS ON THE PATH TOO, and it is the composition rather than a single operation: a
	// backend that spelled it as a multiply by `inversesqrt` carries two ULP and would put this vertex
	// on the other side of the plane. It is frozen at zero ULP, so it is allowed - and a scene that
	// only used `sqrt` would not have reached it.
	let mut builder = Builder::new(Stage::Vertex, "strict-normalize");
	for location in 0..3 {
		builder.varying(location, Type::vec(4), render_shader::Interpolation::Smooth);
	}
	builder.varying_at(3, Type::vec(4), render_shader::Interpolation::Flat, render_shader::ir::Sampling::Pixel);
	let attribute = builder.load(Type::vec(4), Binding::Attribute { location: 0 });
	let colour = builder.load(Type::vec(4), Binding::Attribute { location: 1 });
	let uv = builder.load(Type::vec(4), Binding::Attribute { location: 2 });
	let ident = builder.load(Type::vec(4), Binding::Attribute { location: 3 });
	let unit = builder.assign(Type::vec(4), Op::Unary(render_shader::ir::UnaryOp::Normalize, attribute));
	builder.store(Output::Position, unit);
	builder.store(Output::Varying(0), colour);
	builder.store(Output::Varying(1), uv);
	builder.store(Output::Varying(2), unit);
	builder.store(Output::Varying(3), ident);
	let normalised = builder.finish();
	let corner = |x: f32, y: f32| Vertex { position: [x, y, 0.5, 1.0], colour: [1.0, 1.0, 1.0, 1.0], uv: [0.0; 4], ident: [0.0; 4] };
	let unit_scene = Scene::new(vec![corner(-4.0, -4.0), corner(4.0, -4.0), corner(0.0, 4.0)]);
	let unit_frame = render(&unit_scene, &Plan { count: 3, vertex: normalised, identity_attachment: false, ..Plan::default() })?;
	require!(unit_frame.covered_pixels() > 0, "a position normalised on the strict path is drawn rather than refused");

	// AND A TRANSCENDENTAL WITH NO ZERO-ULP BOUND IS REFUSED ON THE PATH A BACKEND TAKES, not only by
	// a validator a backend might not call. `sin` carries four ULP, which is exactly enough for two
	// backends to put the vertex above on different sides of the plane.
	let relaxed = Plan { count: 3, vertex: rooted(render_shader::Transcendental::Sin), identity_attachment: false, ..Plan::default() };
	require!(render(&scene, &relaxed).is_err(), "a position computed through a transcendental with no strict definition is refused before a frame runs");
	Ok(())
}

pub fn programmable_fragment_stage() -> Outcome {
	// A FRAGMENT STAGE COMPUTES THE COLOUR, and this one computes a value no vertex carries: the
	// interpolated colour times itself. A backend that passed the varying straight through would
	// answer the colour rather than its square.
	let mut builder = Builder::new(Stage::Fragment, "squaring");
	for location in 0..4 {
		builder.varying(location, Type::vec(4), render_shader::Interpolation::Smooth);
	}
	let colour = builder.load(Type::vec(4), Binding::Varying { location: 0 });
	let squared = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Multiply, colour, colour));
	builder.store(Output::Colour(0), squared);
	let fragment = builder.finish();

	let scene = full_quad(0.5, 0.5, 0.5);
	let frame = render(&scene, &Plan { count: 6, fragment, identity_attachment: false, ..Plan::default() })?;
	let (x, y) = frame.centre();
	require!(close(frame.pixel(x, y).x, 0.25), "a stage that squares a half answers a quarter: {:?}", frame.pixel(x, y));
	Ok(())
}

pub fn vertex_layout() -> Outcome {
	// A LAYOUT SAYS WHICH LOCATION A STAGE READS AND WHAT SHAPE IT IS, and `render3d` refuses one
	// that is not a layout: two attributes at one location is a description with two answers to one
	// question, and a stream a backend has no slot for is a read with nowhere to come from. The
	// duplicate case is a refusal this suite ASKED FOR: `validate` checked the stride and the stream
	// and let two attributes share a location, which is a value a stage reads and two backends answer
	// differently.
	use render3d::command::{VertexAttribute, VertexLayout, VertexStream};
	let limits = render3d::Render3DLimits::PROFILE_MINIMUM;
	let attributes = [VertexAttribute { location: 0, stream: 0, offset: 0, size: 16 }, VertexAttribute { location: 1, stream: 0, offset: 16, size: 16 }];
	let streams = [VertexStream { stride: 32, per_instance: false }];
	let layout = VertexLayout { attributes: &attributes, streams: &streams };
	layout.validate(&limits).map_err(|error| crate::Trouble::Unsupported(alloc::format!("an ordinary two-attribute layout was refused: {error:?}")))?;

	let duplicated = [attributes[0], VertexAttribute { location: 0, ..attributes[1] }];
	let layout = VertexLayout { attributes: &duplicated, streams: &streams };
	require!(layout.validate(&limits).is_err(), "two attributes at one location is refused");

	let dangling = [VertexAttribute { stream: 3, ..attributes[0] }];
	let layout = VertexLayout { attributes: &dangling, streams: &streams };
	require!(layout.validate(&limits).is_err(), "and an attribute naming a stream that is not there is refused");
	Ok(())
}

pub fn primitive_topology() -> Outcome {
	// THE TOPOLOGY IS PIPELINE STATE and not a property of the data: the SAME vertices assembled two
	// ways are two different pictures, which is the whole reason it is a feature of its own beside
	// the six topologies in the geometry group.
	let vertices = vec![
		Vertex::at(-0.9, -0.9, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
		Vertex::at(0.9, -0.9, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
		Vertex::at(0.0, 0.9, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
	];
	let filled = render(&Scene::new(vertices.clone()), &Plan { topology: Topology::TriangleList, count: 3, ..Plan::default() })?;
	let outlined = render(&Scene::new(vertices), &Plan { topology: Topology::LineStrip, count: 3, ..Plan::default() })?;
	require!(filled.covered_pixels() > outlined.covered_pixels() * 3, "the same three vertices as a triangle cover far more than as a line strip: {} against {}", filled.covered_pixels(), outlined.covered_pixels());
	let (x, y) = filled.centre();
	require!(filled.covered(x, y) && !outlined.covered(x, y), "and the middle is filled by one and not by the other");
	Ok(())
}

pub fn rasteriser_state() -> Outcome {
	// THE RASTERISER STATE THIS PROFILE HAS IS THE CULL RULE, and what it decides is which SIDE of a
	// surface is drawn. One triangle, two states: front-facing culling removes it and back-facing
	// culling does not, which is a statement about the winding as well as about the state.
	let scene = Scene::new(vec![
		Vertex::at(-0.9, -0.9, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
		Vertex::at(0.9, -0.9, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
		Vertex::at(0.0, 0.9, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
	]);
	let none = render(&scene, &Plan { cull: Cull::None, count: 3, ..Plan::default() })?;
	require!(none.covered_pixels() > 0, "with nothing culled the triangle is drawn");
	let back = render(&scene, &Plan { cull: Cull::Back, count: 3, ..Plan::default() })?;
	let front = render(&scene, &Plan { cull: Cull::Front, count: 3, ..Plan::default() })?;
	require!(back.covered_pixels() != front.covered_pixels(), "and the two cull rules do not agree about it: {} against {}", back.covered_pixels(), front.covered_pixels());
	require!(back.covered_pixels() == 0 || front.covered_pixels() == 0, "one of them removes it entirely");
	require!(back.stats.culled + front.stats.culled == 1, "and exactly one of the two culled it, not {}", back.stats.culled + front.stats.culled);
	Ok(())
}

pub fn depth_stencil_state() -> Outcome {
	// THE STATE IS PART OF THE PIPELINE AND NOT OF THE ATTACHMENT, which is what lets two draws into
	// one buffer test differently. The same geometry and the same buffer under two states: one is
	// drawn and the other is not.
	let first = crate::harness::quad_at_depth(0.5, 1.0, 0.0, 0.0);
	let second = crate::harness::quad_at_depth(0.75, 0.0, 1.0, 0.0);
	let write = Plan { depth_test: Some(CompareOp::Always), depth_write: true, count: 6, ..Plan::default() };
	let nearer_only = Plan { depth_test: Some(CompareOp::Less), depth_write: true, count: 6, ..Plan::default() };
	let frame = crate::harness::render_pair(&first, &write, &second, &nearer_only)?;
	let (x, y) = frame.centre();
	require!(close(frame.pixel(x, y).x, 1.0), "under Less the farther draw is refused and the first is what is visible");

	let anything = Plan { depth_test: Some(CompareOp::Always), ..nearer_only };
	let frame = crate::harness::render_pair(&first, &write, &second, &anything)?;
	require!(close(frame.pixel(x, y).y, 1.0), "and under Always the same draw into the same buffer is visible: {:?}", frame.pixel(x, y));
	Ok(())
}

pub fn blend_state_per_attachment() -> Outcome {
	// PER ATTACHMENT IS THE WHOLE POINT. A pass that writes colour to one target and object ids to
	// another must blend the first and not the second, and a backend with one blend state for the
	// pass could not do both. This draws a half-alpha quad over a cleared frame: the colour
	// attachment blends and the identity attachment does not.
	let scene = Scene::new(vec![
		Vertex::at(-1.0, -1.0, 0.5).coloured(1.0, 0.0, 0.0, 0.5).with_ident(4.0),
		Vertex::at(1.0, -1.0, 0.5).coloured(1.0, 0.0, 0.0, 0.5).with_ident(4.0),
		Vertex::at(1.0, 1.0, 0.5).coloured(1.0, 0.0, 0.0, 0.5).with_ident(4.0),
		Vertex::at(-1.0, 1.0, 0.5).coloured(1.0, 0.0, 0.0, 0.5).with_ident(4.0),
	])
	.indexed16(vec![0, 1, 2, 0, 2, 3]);
	let blending = render3d::AttachmentBlend { enabled: true, colour: render3d::BlendEquation { source: render3d::BlendFactor::SrcAlpha, destination: render3d::BlendFactor::OneMinusSrcAlpha, operation: render3d::BlendOp::Add }, alpha: render3d::BlendEquation::REPLACE, write_mask: render3d::ColorWriteMask::ALL };
	let plan = Plan { blend: vec![blending], count: 6, clear: Vec4::new(0.0, 0.0, 1.0, 1.0), ..Plan::default() };
	let frame = render(&scene, &plan)?;
	let (x, y) = frame.centre();
	let pixel = frame.pixel(x, y);
	require!(close(pixel.x, 0.5) && close(pixel.z, 0.5), "half a red over a blue clear is half of each: {pixel:?}");
	require!(frame.identity(x, y) == 4, "and the identity beside it is the id itself, not a blend of one: {}", frame.identity(x, y));
	Ok(())
}

pub fn color_write_mask() -> Outcome {
	// A MASK IS APPLIED AFTER BLENDING, so a masked channel keeps the DESTINATION's value rather than
	// blending into it. A red quad over a blue clear with the red channel masked off leaves the blue
	// exactly as it was.
	let mask = render3d::ColorWriteMask { red: false, green: true, blue: true, alpha: true };
	let masked = render3d::AttachmentBlend { enabled: false, colour: render3d::BlendEquation::REPLACE, alpha: render3d::BlendEquation::REPLACE, write_mask: mask };
	let plan = Plan { blend: vec![masked], count: 6, clear: Vec4::new(0.25, 0.0, 1.0, 1.0), ..Plan::default() };
	let frame = render(&full_quad(1.0, 0.0, 0.0), &plan)?;
	let (x, y) = frame.centre();
	let pixel = frame.pixel(x, y);
	require!(close(pixel.x, 0.25), "the masked channel keeps what was there: {pixel:?}");
	require!(close(pixel.z, 0.0), "and the unmasked ones take the fragment's value");
	Ok(())
}

pub fn viewport() -> Outcome {
	// THE VIEWPORT IS WHERE CLIP SPACE LANDS, and a smaller one is the same geometry in fewer pixels
	// rather than a smaller part of it: the whole quad is still drawn, in the corner it names.
	let scene = full_quad(1.0, 1.0, 1.0);
	let whole = render(&scene, &Plan { count: 6, ..Plan::default() })?;
	let corner = Viewport { x: 0.0, y: 0.0, width: (crate::harness::WIDTH / 2) as f32, height: (crate::harness::HEIGHT / 2) as f32, min_depth: 0.0, max_depth: 1.0 };
	let quarter = render(&scene, &Plan { count: 6, viewport: Some(corner), ..Plan::default() })?;
	require!(whole.covered_pixels() > quarter.covered_pixels() * 3, "a half-size viewport draws a quarter of the pixels: {} against {}", whole.covered_pixels(), quarter.covered_pixels());
	require!(quarter.covered(1, 1), "and what it draws is in the corner the viewport names");
	require!(!quarter.covered(crate::harness::WIDTH - 2, crate::harness::HEIGHT - 2), "and not outside it");
	Ok(())
}

pub fn scissor() -> Outcome {
	// A SCISSOR REMOVES FRAGMENTS OUTSIDE A RECTANGLE AND CHANGES NOTHING ELSE, which is what makes
	// it different from a viewport: the geometry lands where it always did and part of it is
	// discarded. A pass that draws the whole target under a scissor covering one quadrant leaves the
	// other three at the clear.
	let scene = full_quad(1.0, 1.0, 1.0);
	let half = soft3d::Scissor { x: 0, y: 0, width: crate::harness::WIDTH / 2, height: crate::harness::HEIGHT / 2 };
	let frame = render(&scene, &Plan { count: 6, scissor: Some(half), ..Plan::default() })?;
	require!(frame.covered(1, 1), "inside the scissor the quad is drawn");
	require!(!frame.covered(crate::harness::WIDTH - 2, crate::harness::HEIGHT - 2), "and outside it nothing is");
	let quarter = (crate::harness::WIDTH / 2) * (crate::harness::HEIGHT / 2);
	require!(frame.covered_pixels() == quarter, "and what is drawn is exactly the scissor's own area: {} against {quarter}", frame.covered_pixels());

	// AND THE GEOMETRY DID NOT MOVE, which is the half a viewport would fail: the pixel at the very
	// corner of the scissor is the one the quad covers there, not a scaled-down copy of the whole.
	let unscissored = render(&scene, &Plan { count: 6, ..Plan::default() })?;
	require!(unscissored.pixel(1, 1) == frame.pixel(1, 1), "a scissored draw is the same drawing with part of it removed");
	Ok(())
}
