//! THE GEOMETRY GROUP: what a draw is made of, and how the pieces reach the rasteriser.
//!
//! EVERY SCENE HERE IS ABOUT WHICH VERTICES A DRAW VISITS AND IN WHAT ORDER, which is why almost all
//! of them are checked by COVERAGE rather than by colour: a topology that assembled the wrong
//! triangles covers different pixels, and an index type read at the wrong width reads different
//! vertices. A colour would only say that something was drawn.

use crate::Outcome;
use crate::harness::{Indices16Or32, Plan, Scene, Vertex, close, instanced_vertex_stage, render};
use alloc::vec;
use alloc::vec::Vec;
use render3d::Topology;

/// A triangle covering the bottom-left quadrant, which is where every coverage expectation below is
/// stated against: a scene that drew the whole target could not tell a topology from a topology.
fn corner_triangle() -> Scene {
	Scene::new(vec![
		Vertex::at(-1.0, -1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
		Vertex::at(0.0, -1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
		Vertex::at(-1.0, 0.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
	])
}

/// Where the rasteriser puts a clip-space point, so a scene can name the pixel it expects.
///
/// THE VIEWPORT INVERTS Y, which is the one conversion a reader gets wrong: clip space has `+y` up
/// and a target has row zero at the top.
fn at(x: f32, y: f32) -> (u32, u32) {
	let width = crate::harness::WIDTH as f32;
	let height = crate::harness::HEIGHT as f32;
	((((x + 1.0) * 0.5) * width) as u32, (((1.0 - y) * 0.5) * height) as u32)
}

pub fn index_u16() -> Outcome {
	// A SIXTEEN-BIT INDEX IS NOT A THIRTY-TWO-BIT ONE READ NARROWLY. A backend that read `u16`
	// indices two bytes at a time out of a `u32` reading would draw a triangle from vertices 0, 0, 1
	// - degenerate, and covering nothing - which is exactly what this catches.
	let scene = corner_triangle().indexed16(vec![0, 1, 2]);
	let frame = render(&scene, &Plan::default())?;
	// WELL INSIDE AND NOT ON THE HYPOTENUSE. A right triangle's diagonal is exactly where the fill
	// rule decides, and a scene about index width has no business depending on that decision - the
	// rule has a scene of its own.
	let (x, y) = at(-0.7, -0.7);
	require!(frame.covered(x, y), "the indexed triangle covers the quadrant its vertices name");
	require!(!frame.covered(at(0.5, 0.5).0, at(0.5, 0.5).1), "and nothing else");
	Ok(())
}

pub fn index_u32() -> Outcome {
	let scene = corner_triangle().indexed32(vec![0, 1, 2]);
	let frame = render(&scene, &Plan::default())?;
	let (x, y) = at(-0.7, -0.7);
	require!(frame.covered(x, y), "a thirty-two-bit index buffer names the same three vertices");
	Ok(())
}

pub fn multiple_vertex_streams() -> Outcome {
	// TWO STREAMS IS WHAT A PER-INSTANCE ATTRIBUTE IS. The position comes from the per-vertex stream
	// and the offset from a second one indexed by INSTANCE rather than by vertex; a backend with one
	// stream would read the offset with the vertex index and move each vertex of one instance by a
	// different amount, which is a sheared triangle rather than a moved one.
	let scene = corner_triangle().instanced(vec![[0.0, 0.0, 0.0, 0.0], [1.0, 1.0, 0.0, 0.0]]);
	let plan = Plan { vertex: instanced_vertex_stage(), instances: 2, ..Plan::default() };
	let frame = render(&scene, &plan)?;
	let (x, y) = at(-0.7, -0.7);
	require!(frame.covered(x, y), "the first instance is where the vertices put it");
	let (x, y) = at(0.3, 0.3);
	require!(frame.covered(x, y), "and the second is one unit along both axes, which is the second stream's value");
	Ok(())
}

pub fn configurable_attributes() -> Outcome {
	// AN ATTRIBUTE IS READ BY LOCATION AND NOT BY POSITION IN A STRUCT. The colour is at location 1
	// and the identity at 3; a backend that handed them over in declaration order would paint the
	// triangle with the identity.
	let scene = Scene::new(vec![
		Vertex::at(-1.0, -1.0, 0.5).coloured(0.25, 0.5, 0.75, 1.0).with_ident(7.0),
		Vertex::at(1.0, -1.0, 0.5).coloured(0.25, 0.5, 0.75, 1.0).with_ident(7.0),
		Vertex::at(0.0, 1.0, 0.5).coloured(0.25, 0.5, 0.75, 1.0).with_ident(7.0),
	]);
	let frame = render(&scene, &Plan::default())?;
	let (x, y) = frame.centre();
	let pixel = frame.pixel(x, y);
	require!(close(pixel.x, 0.25) && close(pixel.y, 0.5) && close(pixel.z, 0.75), "the colour comes from location 1: {pixel:?}");
	require!(frame.identity(x, y) == 7, "and the identity from location 3, which is {} here", frame.identity(x, y));
	Ok(())
}

/// The four vertices every strip and fan scene shares, wound so the same four make a quad under both
/// rules. WHAT DIFFERS IS THE ASSEMBLY and not the data, which is what makes the two scenes compare.
fn quad_corners() -> Vec<Vertex> {
	vec![
		Vertex::at(-1.0, -1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
		Vertex::at(1.0, -1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
		Vertex::at(-1.0, 1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
		Vertex::at(1.0, 1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
	]
}

pub fn triangle_list() -> Outcome {
	let scene = Scene::new(quad_corners()).indexed16(vec![0, 1, 2, 1, 3, 2]);
	let plan = Plan { topology: Topology::TriangleList, count: 6, ..Plan::default() };
	let frame = render(&scene, &plan)?;
	require!(frame.covered_pixels() > (crate::harness::WIDTH * crate::harness::HEIGHT) * 9 / 10, "six indices as two triangles cover the target: {} pixel(s)", frame.covered_pixels());
	require!(frame.stats.primitives == 2, "and they are two primitives, not {}", frame.stats.primitives);
	Ok(())
}

pub fn triangle_strip() -> Outcome {
	// A STRIP OF FOUR IS TWO TRIANGLES AND NOT FOUR. Each vertex after the second closes one triangle
	// with the two before it, and the winding of every second one is flipped - a backend that did not
	// flip it would cull half a strip the moment culling was on.
	let scene = Scene::new(quad_corners()).indexed16(vec![0, 1, 2, 3]);
	let plan = Plan { topology: Topology::TriangleStrip, count: 4, ..Plan::default() };
	let frame = render(&scene, &plan)?;
	require!(frame.stats.primitives == 2, "four vertices in a strip are two triangles, not {}", frame.stats.primitives);
	require!(frame.covered_pixels() > (crate::harness::WIDTH * crate::harness::HEIGHT) * 9 / 10, "and they cover the quad: {} pixel(s)", frame.covered_pixels());
	Ok(())
}

pub fn triangle_fan() -> Outcome {
	// A FAN SHARES ITS FIRST VERTEX WITH EVERY TRIANGLE, which is what makes the same four vertices a
	// different shape from the strip above.
	let scene = Scene::new(vec![
		Vertex::at(0.0, 0.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
		Vertex::at(-1.0, -1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
		Vertex::at(1.0, -1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
		Vertex::at(1.0, 1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
	])
	.indexed16(vec![0, 1, 2, 3]);
	let plan = Plan { topology: Topology::TriangleFan, count: 4, ..Plan::default() };
	let frame = render(&scene, &plan)?;
	require!(frame.stats.primitives == 2, "four vertices in a fan are two triangles, not {}", frame.stats.primitives);
	let (x, y) = at(0.5, -0.5);
	require!(frame.covered(x, y), "the fan covers the wedge below and right of its hub");
	let (x, y) = at(-0.5, 0.5);
	require!(!frame.covered(x, y), "and not the one above and left of it, which no triangle of this fan reaches");
	Ok(())
}

pub fn line_list() -> Outcome {
	// A LINE IS NOT A THIN TRIANGLE. What the profile requires is that a line primitive rasterises at
	// all, and what tells a line from a filled shape is that the pixels BETWEEN two lines are not
	// covered.
	let scene = Scene::new(vec![
		Vertex::at(-0.9, -0.9, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
		Vertex::at(0.9, -0.9, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
		Vertex::at(-0.9, 0.9, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
		Vertex::at(0.9, 0.9, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
	]);
	let plan = Plan { topology: Topology::LineList, count: 4, ..Plan::default() };
	let frame = render(&scene, &plan)?;
	require!(frame.stats.primitives == 2, "four vertices in a line list are two lines, not {}", frame.stats.primitives);
	require!(frame.covered_pixels() > 0, "and the lines reach the target");
	let (x, y) = frame.centre();
	require!(!frame.covered(x, y), "and nothing is filled between them");
	Ok(())
}

pub fn line_strip() -> Outcome {
	let scene = Scene::new(vec![
		Vertex::at(-0.9, -0.9, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
		Vertex::at(0.9, -0.9, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
		Vertex::at(0.9, 0.9, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
	]);
	let plan = Plan { topology: Topology::LineStrip, count: 3, ..Plan::default() };
	let frame = render(&scene, &plan)?;
	require!(frame.stats.primitives == 2, "three vertices in a strip are two lines, not {}", frame.stats.primitives);
	Ok(())
}

pub fn point_list() -> Outcome {
	// A POINT IS ONE PIXEL AND NOT A DOT OF SOME SIZE THE BACKEND CHOSE. Three points are three
	// covered pixels, which is the only statement about a point list that a second backend must also
	// satisfy.
	let scene = Scene::new(vec![
		Vertex::at(-0.5, -0.5, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
		Vertex::at(0.0, 0.0, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
		Vertex::at(0.5, 0.5, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
	]);
	let plan = Plan { topology: Topology::PointList, count: 3, ..Plan::default() };
	let frame = render(&scene, &plan)?;
	require!(frame.stats.primitives == 3, "three vertices in a point list are three points, not {}", frame.stats.primitives);
	require!(frame.covered_pixels() == 3, "and three pixels, not {}", frame.covered_pixels());
	Ok(())
}

pub fn primitive_restart() -> Outcome {
	// THE RESTART VALUE ENDS A STRIP AND BEGINS THE NEXT, and it is the largest index the type can
	// hold rather than a number a caller chose. A backend that treated it as an ordinary index would
	// draw a triangle joining the two strips - across the middle of the target, which is where this
	// looks.
	let scene = Scene::new(vec![
		Vertex::at(-1.0, -1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
		Vertex::at(-0.4, -1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
		Vertex::at(-1.0, 1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
		Vertex::at(0.4, -1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
		Vertex::at(1.0, -1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
		Vertex::at(1.0, 1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
	])
	.indexed16(vec![0, 1, 2, u16::MAX, 3, 4, 5]);
	let plan = Plan { topology: Topology::TriangleStrip, count: 7, restart: true, ..Plan::default() };
	let frame = render(&scene, &plan)?;
	require!(frame.stats.primitives == 2, "the restart makes two strips of one triangle each, not {} primitive(s)", frame.stats.primitives);
	let (x, y) = at(0.0, -0.9);
	require!(!frame.covered(x, y), "and nothing is drawn across the gap the restart left");
	Ok(())
}

pub fn indexed_draw() -> Outcome {
	// AN INDEX BUFFER IS WHAT LETS ONE VERTEX BE USED TWICE, which is the whole of what indexing is
	// for: three vertices and six indices are two triangles.
	let scene = Scene::new(quad_corners()).indexed16(vec![0, 1, 2, 1, 3, 2]);
	let plan = Plan { count: 6, ..Plan::default() };
	let frame = render(&scene, &plan)?;
	require!(frame.stats.primitives == 2, "six indices over four vertices are two primitives");
	Ok(())
}

pub fn non_indexed_draw() -> Outcome {
	// WITHOUT AN INDEX BUFFER, VERTEX `i` IS INDEX `i`, and the count is a vertex count rather than
	// an index count.
	let scene = corner_triangle();
	let frame = render(&scene, &Plan::default())?;
	require!(matches!(scene.indices, Indices16Or32::None), "this scene really has no index buffer");
	require!(frame.stats.primitives == 1, "three vertices are one triangle");
	let (x, y) = at(-0.7, -0.7);
	require!(frame.covered(x, y), "drawn where the vertices are");
	Ok(())
}

pub fn instancing() -> Outcome {
	// EVERY INSTANCE DRAWS THE SAME VERTICES, and what tells them apart is the instance index. Two
	// instances of one triangle are two primitives and two places on the target.
	let scene = corner_triangle().instanced(vec![[0.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]]);
	let plan = Plan { vertex: instanced_vertex_stage(), instances: 2, ..Plan::default() };
	let frame = render(&scene, &plan)?;
	require!(frame.stats.primitives == 2, "two instances of one triangle are two primitives, not {}", frame.stats.primitives);
	let (x, y) = at(-0.7, -0.7);
	require!(frame.covered(x, y), "the first instance is at the vertices' own position");
	let (x, y) = at(0.3, -0.7);
	require!(frame.covered(x, y), "and the second is one unit along x");
	Ok(())
}

pub fn base_vertex() -> Outcome {
	// A BASE VERTEX IS ADDED TO EVERY INDEX and is not a first-index offset: the same index buffer
	// reads a different part of the vertex buffer. Here it moves the draw from the first triangle to
	// the second, which is in the opposite corner.
	let scene = Scene::new(vec![
		Vertex::at(-1.0, -1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
		Vertex::at(0.0, -1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
		Vertex::at(-1.0, 0.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
		Vertex::at(0.0, 0.0, 0.5).coloured(0.0, 1.0, 0.0, 1.0),
		Vertex::at(1.0, 0.0, 0.5).coloured(0.0, 1.0, 0.0, 1.0),
		Vertex::at(0.0, 1.0, 0.5).coloured(0.0, 1.0, 0.0, 1.0),
	])
	.indexed16(vec![0, 1, 2]);
	let plan = Plan { base_vertex: 3, ..Plan::default() };
	let frame = render(&scene, &plan)?;
	let (x, y) = at(0.25, 0.25);
	require!(frame.covered(x, y), "the base vertex moved the draw to the second triangle");
	let (x, y) = at(-0.7, -0.7);
	require!(!frame.covered(x, y), "and the first one is not drawn");
	Ok(())
}

pub fn base_instance() -> Outcome {
	// A FIRST INSTANCE IS ADDED TO THE INSTANCE INDEX the per-instance attribute is read with, so one
	// instance starting at one reads the SECOND offset. A backend that ignored it would draw the
	// first.
	let scene = corner_triangle().instanced(vec![[0.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]]);
	let plan = Plan { vertex: instanced_vertex_stage(), instances: 1, first_instance: 1, ..Plan::default() };
	let frame = render(&scene, &plan)?;
	let (x, y) = at(0.3, -0.7);
	require!(frame.covered(x, y), "the one instance read the offset at index one");
	let (x, y) = at(-0.7, -0.7);
	require!(!frame.covered(x, y), "and not the one at index zero");
	Ok(())
}
