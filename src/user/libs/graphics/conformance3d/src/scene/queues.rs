//! THE QUEUE GROUP: what is drawn in what order, and who decides.
//!
//! TRANSPARENCY IS THE LAYER'S JOB AND NOT THE APPLICATION'S, which is why the ORDER is a conformance
//! feature rather than a performance one: blending is not commutative, so two implementations that
//! sort a transparent queue differently produce different pictures from one scene. Opaque front to
//! back is a performance decision and is still stated, because a suite that only checked the
//! transparent one would let a backend reverse the other and call itself conforming.
//!
//! EACH SCENE BELOW PUTS ITS DRAWABLES AT DISTINCT DISTANCES AND SUBMITS THEM IN THE WRONG ORDER, so
//! a layer that returned submission order would fail every one of them.

use crate::Outcome;
use crate::harness::close;
use crate::scene::{camera_at, material, world};
use render_math::Vec3;
use scene3d::{Aabb, Blending, Drawable, MaterialKind, Node, QueueKind, queue};

/// A unit cube's local bound, which every drawable in this group carries.
fn unit() -> Aabb {
	Aabb::new(Vec3::new(-0.5, -0.5, -0.5), Vec3::new(0.5, 0.5, 0.5))
}

/// A drawable of a stated material at a stated distance in front of the camera.
fn placed(scene: &mut scene3d::Scene, material_index: u32, z: f32) -> Result<u32, crate::Trouble> {
	let node = scene.add_node(Node::identity().with_translation(Vec3::new(0.0, 0.0, z)))?;
	Ok(scene.add_drawable(Drawable::new(node, 0, material_index).with_bounds(unit()))?)
}

/// The three queues of a scene seen from `(0, 0, 5)`.
fn queues(scene: &mut scene3d::Scene) -> Result<scene3d::Queue, crate::Trouble> {
	let camera = camera_at(scene, Vec3::new(0.0, 0.0, 5.0))?;
	Ok(queue::build(scene, &camera)?)
}

pub fn opaque_queue() -> Outcome {
	// THE OPAQUE QUEUE IS FRONT TO BACK, so the depth test rejects the most fragments. The drawables
	// are submitted BACK TO FRONT, so a layer that returned submission order gets it exactly wrong.
	let mut scene = world();
	let opaque = scene.add_material(material(MaterialKind::Lambert, Blending::Opaque))?;
	let far = placed(&mut scene, opaque, -10.0)?;
	let middle = placed(&mut scene, opaque, 0.0)?;
	let near = placed(&mut scene, opaque, 3.0)?;
	let queue = queues(&mut scene)?;
	require!(queue.opaque.len() == 3, "three opaque drawables make three opaque entries, and made {}", queue.opaque.len());
	require!(queue.alpha_mask.is_empty() && queue.transparent.is_empty(), "and nothing else");
	let order: alloc::vec::Vec<u32> = queue.opaque.iter().map(|entry| entry.drawable).collect();
	require!(order == alloc::vec![near, middle, far], "the opaque queue runs front to back, and ran {order:?}");
	Ok(())
}

pub fn alpha_mask_queue() -> Outcome {
	// THE ALPHA-MASK QUEUE IS ITS OWN QUEUE AND RUNS AFTER THE OPAQUE ONE, front to back like it. A
	// discarding fragment stage cannot take the early depth path, so keeping it OUT of the opaque
	// queue is what lets that queue stay fast - and running it second means fewer of its fragments
	// survive to be discarded at all.
	let mut scene = world();
	let opaque = scene.add_material(material(MaterialKind::Lambert, Blending::Opaque))?;
	let masked = scene.add_material(material(MaterialKind::Unlit, Blending::AlphaMask { threshold: 0.5 }))?;
	let _ = placed(&mut scene, opaque, 0.0)?;
	let far_leaf = placed(&mut scene, masked, -8.0)?;
	let near_leaf = placed(&mut scene, masked, 2.0)?;
	let queue = queues(&mut scene)?;
	require!(queue.opaque.len() == 1, "an alpha-masked material does not go in the opaque queue");
	let order: alloc::vec::Vec<u32> = queue.alpha_mask.iter().map(|entry| entry.drawable).collect();
	require!(order == alloc::vec![near_leaf, far_leaf], "and the alpha-mask queue is front to back too, and ran {order:?}");
	require!(QueueKind::AlphaMask.writes_depth(), "and it writes depth, because a surviving fragment is opaque");
	Ok(())
}

pub fn transparent_queue() -> Outcome {
	// THE TRANSPARENT QUEUE IS BACK TO FRONT, because blending is not commutative: `over` applied in
	// the wrong order gives a different colour, and it is the one queue whose order is visible in the
	// picture rather than only in the frame time.
	let mut scene = world();
	let glass = scene.add_material(material(MaterialKind::Unlit, Blending::Blended))?;
	let near = placed(&mut scene, glass, 3.0)?;
	let far = placed(&mut scene, glass, -10.0)?;
	let middle = placed(&mut scene, glass, 0.0)?;
	let queue = queues(&mut scene)?;
	let order: alloc::vec::Vec<u32> = queue.transparent.iter().map(|entry| entry.drawable).collect();
	require!(order == alloc::vec![far, middle, near], "the transparent queue runs back to front, and ran {order:?}");
	require!(queue.opaque.is_empty(), "and a blended material is in no other queue");
	Ok(())
}

pub fn distance_sort_key() -> Outcome {
	// THE KEY IS THE DISTANCE FROM THE CAMERA POSITION TO THE BOUNDING SPHERE'S CENTRE. NOT THE
	// NEAREST POINT OF THE BOUNDS, which is the plausible alternative and is wrong: a large object
	// sorts ahead of a small one it CONTAINS, and the two then swap as the camera moves.
	let mut scene = world();
	let opaque = scene.add_material(material(MaterialKind::Lambert, Blending::Opaque))?;
	// A big shell centred on the origin, and a small object inside it slightly nearer the camera.
	let shell_node = scene.add_node(Node::identity())?;
	let shell = scene.add_drawable(Drawable::new(shell_node, 0, opaque).with_bounds(Aabb::new(Vec3::new(-4.0, -4.0, -4.0), Vec3::new(4.0, 4.0, 4.0))))?;
	let inner = placed(&mut scene, opaque, 1.0)?;
	let queue = queues(&mut scene)?;
	let order: alloc::vec::Vec<u32> = queue.opaque.iter().map(|entry| entry.drawable).collect();
	require!(order == alloc::vec![inner, shell], "the smaller object inside the shell sorts first because its CENTRE is nearer, and the order was {order:?}");
	require!(close(queue.opaque[0].depth, 4.0), "and the key is the distance to the centre, which is 4 and was {}", queue.opaque[0].depth);
	require!(close(queue.opaque[1].depth, 5.0), "and 5 for the shell, which was {}", queue.opaque[1].depth);
	Ok(())
}

pub fn stable_tiebreak() -> Outcome {
	// TWO DRAWABLES AT ONE DISTANCE KEEP THEIR SUBMISSION ORDER, in BOTH directions of sort. A
	// tiebreak that reversed with the sort would make two coincident objects swap between the opaque
	// and the transparent queue, which a person reads as flicker and a comparison reads as a frame
	// that differs from itself.
	let mut scene = world();
	let opaque = scene.add_material(material(MaterialKind::Lambert, Blending::Opaque))?;
	let glass = scene.add_material(material(MaterialKind::Unlit, Blending::Blended))?;
	let first = placed(&mut scene, opaque, 0.0)?;
	let second = placed(&mut scene, opaque, 0.0)?;
	let third = placed(&mut scene, glass, 0.0)?;
	let fourth = placed(&mut scene, glass, 0.0)?;
	let queue = queues(&mut scene)?;
	let opaque_order: alloc::vec::Vec<u32> = queue.opaque.iter().map(|entry| entry.drawable).collect();
	let blended_order: alloc::vec::Vec<u32> = queue.transparent.iter().map(|entry| entry.drawable).collect();
	require!(opaque_order == alloc::vec![first, second], "coincident opaque drawables keep submission order: {opaque_order:?}");
	require!(blended_order == alloc::vec![third, fourth], "and so do coincident transparent ones, though the queue runs the other way: {blended_order:?}");
	Ok(())
}

pub fn fixed_queue_order() -> Outcome {
	// THE QUEUES THEMSELVES RUN OPAQUE, ALPHA-MASK, TRANSPARENT, AND A SCENE MAY NOT REORDER THEM.
	// Each queue's own sort means something only in this sequence: a caller free to reorder them would
	// be free to blend before the depth buffer was filled, which is the one ordering that cannot be
	// recovered from afterwards.
	require!(QueueKind::ORDER == [QueueKind::Opaque, QueueKind::AlphaMask, QueueKind::Transparent], "the profile's order is the one the layer publishes: {:?}", QueueKind::ORDER);
	let mut scene = world();
	let opaque = scene.add_material(material(MaterialKind::Lambert, Blending::Opaque))?;
	let masked = scene.add_material(material(MaterialKind::Unlit, Blending::AlphaMask { threshold: 0.25 }))?;
	let glass = scene.add_material(material(MaterialKind::Unlit, Blending::Blended))?;
	// Submitted in exactly the WRONG order, and at distances that would not fix it either.
	let blended = placed(&mut scene, glass, 4.0)?;
	let leaf = placed(&mut scene, masked, 2.0)?;
	let solid = placed(&mut scene, opaque, 0.0)?;
	let queue = queues(&mut scene)?;
	let walked: alloc::vec::Vec<u32> = queue.in_order().iter().flat_map(|entries| entries.iter().map(|entry| entry.drawable)).collect();
	require!(walked == alloc::vec![solid, leaf, blended], "the three queues are walked in the fixed order whatever the distances say: {walked:?}");
	Ok(())
}

pub fn transparent_depth_no_write() -> Outcome {
	// A TRANSPARENT DRAWABLE IS DEPTH-TESTED AND DOES NOT WRITE DEPTH. Writing it would make a
	// transparent surface HIDE the one behind it, which is the commonest transparency bug there is -
	// and it is invisible until two transparent things overlap.
	require!(QueueKind::Opaque.writes_depth(), "the opaque queue writes depth");
	require!(QueueKind::AlphaMask.writes_depth(), "and so does the alpha-mask queue, because a surviving fragment is opaque");
	require!(!QueueKind::Transparent.writes_depth(), "and the transparent queue does not");
	let glass = material(MaterialKind::Unlit, Blending::Blended);
	let solid = material(MaterialKind::Lambert, Blending::Opaque);
	require!(!glass.writes_depth(), "a blended material writes no depth");
	require!(solid.writes_depth(), "and an opaque one does");
	// AND THE TEST IS STILL APPLIED, which is the half "no depth write" is often mistaken for: a
	// transparent surface behind an opaque one is still hidden by it.
	require!(glass.blend.enabled, "a blended material blends, so what is already in the target still shows through");
	require!(glass.base_colour.w <= 1.0 && glass.base_colour.is_finite(), "and it carries a coverage: {:?}", glass.base_colour);
	Ok(())
}
