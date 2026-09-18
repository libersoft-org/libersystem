//! THE PICKING AND READBACK GROUP: which drawable is at a pixel, and how the answer arrives.
//!
//! THE ANSWER IS AN IDENTITY THE FRAME WROTE AND NOT A DEPTH UNPROJECTED. Reading the depth and
//! unprojecting it answers WHERE and not WHAT, and every application that tries it ends up matching a
//! world position against its own objects - which is this module, written badly, in the application.
//!
//! AND THE RESULT ARRIVES WITH THE FRAME'S COMPLETION. A synchronous pick stalls the whole pipeline
//! for a cursor, so the API shape is a TICKET: a software backend may report the submission complete
//! immediately and a GPU one later, and neither changes the shape. That is a conformance feature
//! because an application written against a synchronous answer does not work on the other one.

use crate::Outcome;
use crate::harness::close;
use crate::scene::{material, world};
use render_math::{Vec3, Viewport};
use scene3d::{Aabb, Blending, Drawable, MaterialKind, Node, Pending, Ray, Readback, pick};

fn unit() -> Aabb {
	Aabb::new(Vec3::new(-0.5, -0.5, -0.5), Vec3::new(0.5, 0.5, 0.5))
}

/// A drawable of a stated material at a stated distance in front of the camera.
fn placed(scene: &mut scene3d::Scene, material_index: u32, z: f32) -> Result<u32, crate::Trouble> {
	let node = scene.add_node(Node::identity().with_translation(Vec3::new(0.0, 0.0, z)))?;
	Ok(scene.add_drawable(Drawable::new(node, 0, material_index).with_bounds(unit()))?)
}

/// A ray down `-z` from in front of the scene, which is what a pick at the centre of the view is.
fn down_the_axis() -> Ray {
	Ray { origin: Vec3::new(0.0, 0.0, 10.0), direction: Vec3::new(0.0, 0.0, -1.0) }
}

pub fn object_id_attachment() -> Outcome {
	// AN INTEGER ATTACHMENT WRITTEN BY THE SAME PASS THAT WROTE THE COLOUR, holding a per-drawable id.
	// The same pass is what makes the answer agree with the picture: an identity written by a second
	// pass is depth-tested against its OWN buffer, so an object the colour pass hid can win it.
	let mut scene = world();
	let opaque = scene.add_material(material(MaterialKind::Lambert, Blending::Opaque))?;
	let near = placed(&mut scene, opaque, 2.0)?;
	let far = placed(&mut scene, opaque, -2.0)?;
	scene.update();
	let hit = pick::resolve(&scene, &down_the_axis(), u32::MAX).ok_or(crate::Trouble::Failed(alloc::string::String::from("a ray through two drawables hit nothing")))?;
	require!(hit.drawable == near, "a pick answers the NEAREST drawable that passed the depth test, and answered {}", hit.drawable);
	require!(hit.id == scene.drawables()[near as usize].id(), "and names it by its identity: {}", hit.id);
	require!(hit.distance > 0.0, "at a distance along the ray: {}", hit.distance);
	let _ = far;

	// AND THE PASS THAT WRITES IT WRITES THE COLOUR AND THE DEPTH TOO, which is what `selection_pass`
	// is: ONE pass and three targets, not two passes.
	let pass = pick::selection_pass(7, 1, 2, 3);
	require!(pass.writes.len() == 3, "the selection pass writes three targets: {:?}", pass.writes);
	require!(pass.writes.contains(&2), "one of which is the identity attachment");
	Ok(())
}

pub fn reserved_zero_identity() -> Outcome {
	// ZERO IS RESERVED FOR NOTHING, so a pick on the background is UNAMBIGUOUS rather than being
	// drawable zero. A layer that handed out identities from zero would make "nothing is here" and
	// "the first object is here" the same answer, and the first object is the one most likely to be
	// the background.
	let mut scene = world();
	let opaque = scene.add_material(material(MaterialKind::Lambert, Blending::Opaque))?;
	let first = placed(&mut scene, opaque, 0.0)?;
	require!(scene.drawables()[first as usize].id() != 0, "the first identity a scene hands out is not zero, and is {}", scene.drawables()[first as usize].id());
	require!(scene.drawable_of_id(0).is_none(), "and zero names nothing");
	let pending = pick::request(Readback::Identity, 4, 4, 32, 32, 1, 9)?;
	require!(pick::answer(&scene, &pending, 0)?.is_none(), "so a readback of zero answers that nothing was picked");
	require!(pick::answer(&scene, &pending, scene.drawables()[first as usize].id())?.is_some(), "while a real identity answers a drawable");
	Ok(())
}

pub fn application_assigned_identity() -> Outcome {
	// THE IDENTITY IS A 32-BIT VALUE THE APPLICATION MAY SET, which is what lets it be the
	// application's OWN key - a row in its model, a handle in its document - rather than a number the
	// scene invented that has to be mapped back.
	let mut scene = world();
	let opaque = scene.add_material(material(MaterialKind::Lambert, Blending::Opaque))?;
	let node = scene.add_node(Node::identity())?;
	let mine = scene.add_drawable(Drawable::new(node, 0, opaque).with_bounds(unit()).with_id(40_000))?;
	require!(scene.drawables()[mine as usize].id() == 40_000, "a stated identity is kept: {}", scene.drawables()[mine as usize].id());
	require!(scene.drawable_of_id(40_000) == Some(mine), "and names its drawable");

	// AND TWO DRAWABLES MAY NOT SHARE ONE, because the ambiguity would only ever show as a click that
	// selects the wrong object - which is the hardest kind of defect to trace back to its cause.
	let other = scene.add_node(Node::identity())?;
	require!(scene.add_drawable(Drawable::new(other, 0, opaque).with_id(40_000)).is_err(), "a second drawable with the same identity is refused");

	// AND THE ONES THE SCENE ASSIGNS DO NOT COLLIDE WITH IT EITHER, which is what makes mixing the two
	// safe: a caller may name some drawables and let the scene name the rest.
	let automatic = scene.add_drawable(Drawable::new(other, 0, opaque).with_bounds(unit()))?;
	require!(scene.drawables()[automatic as usize].id() != 40_000, "an assigned identity does not collide with a stated one");
	Ok(())
}

pub fn selection_pass() -> Outcome {
	// THE SELECTION PASS IS THE SAME PASS AS THE COLOUR AND NOT A SECOND ONE. A separate selection
	// pass draws the scene twice, and the second drawing is depth-tested against its own buffer - so
	// an object the colour pass hid can win the identity pass, and the pick answers something the
	// person cannot see.
	let pass = pick::selection_pass(1, 10, 11, 12);
	require!(pass.id == 1, "the pass carries the caller's identifier");
	require!(pass.writes == alloc::vec![10, 11, 12], "and writes the colour, the identity and the depth together: {:?}", pass.writes);
	require!(pass.reads.is_empty(), "and reads nothing, because it is the pass that DRAWS the scene");

	// AND IT IS A PASS IN THE GRAPH LIKE ANY OTHER, so a later pass that reads what it wrote is
	// ordered after it rather than being a special case.
	let mut graph = scene3d::PassGraph::new();
	graph.add(pass);
	graph.add(scene3d::Pass { id: 2, writes: alloc::vec![20], reads: alloc::vec![10] });
	let order = graph.order()?;
	require!(order == alloc::vec![1, 2], "a pass reading the selection pass's colour runs after it: {order:?}");
	Ok(())
}

pub fn transparent_writes_no_identity() -> Outcome {
	// A TRANSPARENT DRAWABLE WRITES NO IDENTITY, SO A PICK THROUGH GLASS ANSWERS WHAT IS BEHIND IT.
	// That is what a person clicking expects, and it is a thing a depth-based pick cannot express at
	// all: the depth buffer has the glass in it or it does not, and either way the answer is wrong.
	let mut scene = world();
	let opaque = scene.add_material(material(MaterialKind::Lambert, Blending::Opaque))?;
	let glass = scene.add_material(material(MaterialKind::Unlit, Blending::Blended))?;
	let window = placed(&mut scene, glass, 3.0)?;
	let behind = placed(&mut scene, opaque, 0.0)?;
	scene.update();
	let hit = pick::resolve(&scene, &down_the_axis(), u32::MAX).ok_or(crate::Trouble::Failed(alloc::string::String::from("a pick through glass hit nothing")))?;
	require!(hit.drawable == behind, "a pick through glass answers what is behind it, and answered {}", hit.drawable);
	require!(!scene.materials()[glass as usize].writes_id(), "because a blended material writes no identity");
	require!(scene.materials()[opaque as usize].writes_id(), "while an opaque one does");
	let _ = window;

	// AND AN ALPHA-MASKED DRAWABLE DOES WRITE ONE, which is the case between the two: a fragment that
	// survived the threshold is opaque, so it is pickable and the ones discarded are not.
	let leaf = scene.add_material(material(MaterialKind::Unlit, Blending::AlphaMask { threshold: 0.5 }))?;
	require!(scene.materials()[leaf as usize].writes_id(), "an alpha-masked material writes an identity, because a surviving fragment is opaque");
	Ok(())
}

pub fn identity_readback() -> Outcome {
	// AN IDENTITY READBACK ANSWERS A DRAWABLE, and it is the ONLY readback that does. A depth value
	// reinterpreted as an identity names whichever drawable happens to hold that number - an answer
	// that is wrong and looks right, which is the worst kind there is.
	let mut scene = world();
	let opaque = scene.add_material(material(MaterialKind::Lambert, Blending::Opaque))?;
	let thing = placed(&mut scene, opaque, 0.0)?;
	let id = scene.drawables()[thing as usize].id();
	let pending = pick::request(Readback::Identity, 3, 4, 32, 32, 5, 11)?;
	require!(pending.kind == Readback::Identity, "an identity readback asks for the identity attachment");
	require!(pick::answer(&scene, &pending, id)? == Some(thing), "and the value it reads back names the drawable: {:?}", pick::answer(&scene, &pending, id));

	// AND A DEPTH OR A COLOUR READBACK IS REFUSED THE QUESTION, rather than being answered by
	// reinterpretation.
	let depth = pick::request(Readback::Depth, 3, 4, 32, 32, 5, 11)?;
	require!(pick::answer(&scene, &depth, id).is_err(), "a depth readback does not name a drawable");
	let colour = pick::request(Readback::Colour, 3, 4, 32, 32, 5, 11)?;
	require!(pick::answer(&scene, &colour, id).is_err(), "and neither does a colour one");
	Ok(())
}

pub fn depth_readback() -> Outcome {
	// A DEPTH READBACK ANSWERS WHERE, which a CAD viewport placing a cursor in the world needs and an
	// editor selecting an object does not. It is a separate feature from the identity because it reads
	// a DIFFERENT ATTACHMENT of the same pass - and one request type with a kind is what keeps the
	// three on one completion contract rather than three.
	let pending = pick::request(Readback::Depth, 8, 9, 64, 64, 2, 21)?;
	require!(pending.kind == Readback::Depth, "a depth readback asks for the depth attachment");
	require!(pending.request.x == 8 && pending.request.y == 9, "at the pixel it was asked about: {:?}", pending.request);
	require!(pending.readback == 2, "into the buffer it was given");

	// AND THE RAY A DEPTH ANSWER IS TURNED INTO IS BUILT FROM TWO UNPROJECTED POINTS AND NOT FROM AN
	// EYE AND A DIRECTION, because under an ORTHOGRAPHIC projection every ray is parallel and the
	// camera position is on none of them - the projection where the one-point construction is wrong is
	// exactly the one where it is hardest to notice.
	let mut scene = world();
	let node = scene.add_node(Node::identity().with_translation(Vec3::new(0.0, 0.0, 5.0)))?;
	let camera = scene3d::Camera::orthographic(node, 2.0, 2.0, 1.0, 100.0, u32::MAX)?;
	scene.update();
	let inverse = scene.view_projection_of(&camera)?.inverse().map_err(|_| crate::Trouble::Failed(alloc::string::String::from("the view-projection has no inverse")))?;
	let viewport = Viewport::new(0.0, 0.0, 64.0, 64.0);
	let centre = pick::ray_from_window(32.0, 32.0, &viewport, &inverse)?;
	let corner = pick::ray_from_window(8.0, 8.0, &viewport, &inverse)?;
	let centre_direction = centre.direction.normalise().unwrap_or(Vec3::ZERO);
	let corner_direction = corner.direction.normalise().unwrap_or(Vec3::ZERO);
	require!(close(centre_direction.dot(corner_direction), 1.0), "under an orthographic projection every pick ray is PARALLEL: {}", centre_direction.dot(corner_direction));
	require!(!close(centre.origin.x, corner.origin.x) || !close(centre.origin.y, corner.origin.y), "and they start at different places, which a one-point construction cannot express");
	Ok(())
}

pub fn colour_readback() -> Outcome {
	// A COLOUR READBACK ANSWERS THE PIXEL, which a colour picker and a screenshot want. Three kinds
	// and not one, because they read three different attachments - and a caller that had to use three
	// mechanisms would have three completion rules to get right.
	let pending = pick::request(Readback::Colour, 0, 0, 16, 16, 4, 3)?;
	require!(pending.kind == Readback::Colour, "a colour readback asks for the colour attachment");
	require!(pending.request.width == 16 && pending.request.height == 16, "and knows the attachment it is inside: {:?}", pending.request);

	// AND ALL THREE SHARE ONE SHAPE, which is the point: the same pixel, the same frame, three
	// attachments, one completion rule.
	let identity = pick::request(Readback::Identity, 0, 0, 16, 16, 5, 3)?;
	let depth = pick::request(Readback::Depth, 0, 0, 16, 16, 6, 3)?;
	require!(identity.serial == pending.serial && depth.serial == pending.serial, "three readbacks of one frame complete together");
	require!(identity.request == pending.request && depth.request == pending.request, "at one pixel");
	require!(identity.readback != pending.readback && depth.readback != pending.readback, "into three different buffers");
	Ok(())
}

pub fn asynchronous_readback() -> Outcome {
	// THE RESULT ARRIVES WITH THE FRAME'S COMPLETION AND NOT BEFORE. A synchronous pick stalls the
	// whole pipeline for a cursor, so what a caller gets is a TICKET carrying the submission serial it
	// will be answered by - and a software backend that could answer at once still hands one back,
	// because the API shape must not change between backends.
	let pending: Pending = pick::request(Readback::Identity, 1, 1, 8, 8, 0, 42)?;
	require!(pending.serial == 42, "the ticket carries the submission it completes with: {}", pending.serial);
	require!(!pending.is_answered_by(41), "a queue that has completed an earlier frame has not answered it");
	require!(pending.is_answered_by(42), "the frame it was submitted with does");
	require!(pending.is_answered_by(43), "and so does a later one, because serials increase and a later completion implies every earlier one");
	require!(!pending.is_answered_by(0), "while nothing completed answers nothing");
	Ok(())
}

pub fn pick_outside_attachment_refusal() -> Outcome {
	// A PICK OUTSIDE THE ATTACHMENT IS REFUSED RATHER THAN CLAMPED, because a clamped pick answers
	// about a pixel the caller did not ask about AND THE CALLER HAS NO WAY TO TELL. A cursor that
	// leaves the window would then keep selecting whatever is at the edge.
	require!(pick::request(Readback::Identity, 32, 4, 32, 32, 0, 1).is_err(), "a pick at the width is refused, because the last column is width minus one");
	require!(pick::request(Readback::Identity, 4, 32, 32, 32, 0, 1).is_err(), "and one at the height");
	require!(pick::request(Readback::Identity, 0, 0, 0, 32, 0, 1).is_err(), "and a pick inside an attachment with no width");
	require!(pick::request(Readback::Identity, 31, 31, 32, 32, 0, 1).is_ok(), "while the last pixel is a pixel");

	// AND THE SAME RULE APPLIES TO A RAY THROUGH A WINDOW, which is the other way a pick is expressed:
	// outside the viewport is refused there too, on the same grounds.
	let viewport = Viewport::new(0.0, 0.0, 64.0, 64.0);
	let inverse = render_math::Mat4::from_scale(Vec3::new(1.0, 1.0, 1.0));
	require!(pick::ray_from_window(-1.0, 32.0, &viewport, &inverse).is_err(), "a ray through a point left of the viewport is refused");
	require!(pick::ray_from_window(32.0, 100.0, &viewport, &inverse).is_err(), "and one below it");
	require!(pick::ray_from_window(32.0, 32.0, &Viewport::new(0.0, 0.0, 0.0, 64.0), &inverse).is_err(), "and a viewport with no area has no pixels to pick through");
	Ok(())
}
