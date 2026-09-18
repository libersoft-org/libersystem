//! THE INSTANCING GROUP: one mesh, many places, one draw.
//!
//! AN INSTANCE IS NOT A NODE. A node has a hierarchy, an identity and a dirty flag, and a million of
//! them is a million traversals; an instance is one world transform and one colour in a per-instance
//! vertex stream, which is the whole of why instancing exists. Every scene below is about a decision
//! that follows from that: what instances share, what is culled, what is sorted, and what the layer
//! will not promise.

use crate::Outcome;
use crate::harness::close;
use crate::scene::{camera_at, material, world};
use render_math::{Mat4, Vec3, Vec4};
use scene3d::{Aabb, Blending, Drawable, Instance, MaterialKind, Node, queue};

fn unit() -> Aabb {
	Aabb::new(Vec3::new(-0.5, -0.5, -0.5), Vec3::new(0.5, 0.5, 0.5))
}

/// One instance at a stated place.
fn at(x: f32, y: f32, z: f32) -> Instance {
	Instance { transform: Mat4::from_translation(Vec3::new(x, y, z)), colour: Vec4::new(1.0, 1.0, 1.0, 1.0) }
}

pub fn instance_stream() -> Outcome {
	// AN INSTANCE IS ONE WORLD TRANSFORM AND ONE COLOUR, in a PER-INSTANCE stream - so a hundred
	// copies of a mesh are one drawable and one draw, and the count is what tells the backend how many
	// to make. A count of one records the same command as a count of a hundred, which is what keeps
	// the scene from having two ways to draw the same thing.
	let mut scene = world();
	let opaque = scene.add_material(material(MaterialKind::Lambert, Blending::Opaque))?;
	let node = scene.add_node(Node::identity())?;
	let plain = scene.add_drawable(Drawable::new(node, 0, opaque).with_bounds(unit()))?;
	let many = scene.add_drawable(Drawable::new(node, 1, opaque).with_bounds(unit()).with_instances(alloc::vec![at(-1.0, 0.0, 0.0), at(0.0, 0.0, 0.0), at(1.0, 0.0, 0.0)]))?;
	require!(scene.drawables()[plain as usize].instance_count() == 1, "a drawable with no stream draws once");
	require!(scene.drawables()[many as usize].instance_count() == 3, "and one with three instances draws three: {}", scene.drawables()[many as usize].instance_count());

	let camera = camera_at(&mut scene, Vec3::new(0.0, 0.0, 5.0))?;
	let built = queue::build(&mut scene, &camera)?;
	let instanced = built.opaque.iter().find(|entry| entry.drawable == many).ok_or(crate::Trouble::Failed(alloc::string::String::from("the instanced drawable was not queued")))?;
	require!(instanced.instance_count() == 3, "and all three reach the queue: {}", instanced.instance_count());

	// AND THE COUNT IS BOUNDED. A layer without a bound is one whose worst case is a caller's loop.
	let past = scene.add_drawable(Drawable::new(node, 1, opaque).with_bounds(unit()).with_instances(alloc::vec![at(0.0, 0.0, 0.0); scene.limits().max_instances_per_drawable as usize + 1]));
	require!(past.is_err(), "more instances than the profile admits is refused");
	Ok(())
}

pub fn per_instance_culling() -> Outcome {
	// CULLING IS PER INSTANCE AND NOT PER SET. Culling the whole set because one building is visible
	// would draw a city; keeping the whole set because one is visible would draw it too. So each
	// instance is tested on its own, and the count says how many that removed.
	let mut scene = world();
	let opaque = scene.add_material(material(MaterialKind::Lambert, Blending::Opaque))?;
	let node = scene.add_node(Node::identity())?;
	let many = scene.add_drawable(Drawable::new(node, 0, opaque).with_bounds(unit()).with_instances(alloc::vec![at(0.0, 0.0, 0.0), at(0.0, 0.0, 400.0), at(1.0, 0.0, 0.0), at(0.0, 0.0, -400.0)]))?;
	let camera = camera_at(&mut scene, Vec3::new(0.0, 0.0, 5.0))?;
	let built = queue::build(&mut scene, &camera)?;
	let entry = built.opaque.iter().find(|entry| entry.drawable == many).ok_or(crate::Trouble::Failed(alloc::string::String::from("a set with two visible instances was culled whole")))?;
	require!(entry.instance_count() == 2, "two of four instances survive, and {} did", entry.instance_count());
	require!(built.culled_instances == 2, "and the two that did not are counted: {}", built.culled_instances);
	require!(built.culled == 0, "while the set itself is not culled");

	// AND A SET WHOSE EVERY INSTANCE IS OUTSIDE IS CULLED WHOLE, which is the other half: an empty
	// draw is a draw nobody should record.
	let gone = scene.add_drawable(Drawable::new(node, 0, opaque).with_bounds(unit()).with_instances(alloc::vec![at(0.0, 0.0, 400.0), at(0.0, 0.0, 600.0)]))?;
	let built = queue::build(&mut scene, &camera)?;
	require!(!built.opaque.iter().any(|entry| entry.drawable == gone), "a set with nothing visible is not queued");
	require!(built.culled == 1, "and is counted as a culled drawable: {}", built.culled);
	Ok(())
}

pub fn instance_compaction() -> Outcome {
	// THE SURVIVORS ARE COMPACTED INTO THE STREAM IN THEIR ORIGINAL ORDER. Compaction is what makes
	// per-instance culling worth doing at all - the draw is over a contiguous run - and keeping the
	// ORDER is what makes the result reproducible: a compaction that emitted survivors in whatever
	// order a test happened to finish in would shuffle a crowd every frame.
	let mut scene = world();
	let opaque = scene.add_material(material(MaterialKind::Lambert, Blending::Opaque))?;
	let node = scene.add_node(Node::identity())?;
	// Slots 0, 2 and 5 are visible; 1, 3 and 4 are not.
	let instances = alloc::vec![at(-1.0, 0.0, 0.0), at(0.0, 0.0, 400.0), at(0.0, 0.0, 0.0), at(0.0, 0.0, 500.0), at(0.0, 0.0, 600.0), at(1.0, 0.0, 0.0)];
	let many = scene.add_drawable(Drawable::new(node, 0, opaque).with_bounds(unit()).with_instances(instances))?;
	let camera = camera_at(&mut scene, Vec3::new(0.0, 0.0, 5.0))?;
	let built = queue::build(&mut scene, &camera)?;
	let entry = built.opaque.iter().find(|entry| entry.drawable == many).ok_or(crate::Trouble::Failed(alloc::string::String::from("the set was culled whole")))?;
	require!(entry.instances == alloc::vec![0, 2, 5], "the survivors are named by their original slots, in order: {:?}", entry.instances);
	Ok(())
}

pub fn single_sort_per_instance_set() -> Outcome {
	// AN INSTANCED DRAWABLE SORTS ONCE, BY THE BOUNDING SPHERE OF THE WHOLE SET. Sorting instances
	// against each other would break the single draw that instancing exists to produce - and the
	// sphere is over EVERY instance rather than over the survivors, so the sort key does not change as
	// instances leave the frustum and the set does not jump past its neighbours as the camera moves.
	let mut scene = world();
	let opaque = scene.add_material(material(MaterialKind::Lambert, Blending::Opaque))?;
	let node = scene.add_node(Node::identity())?;
	let spread = scene.add_drawable(Drawable::new(node, 0, opaque).with_bounds(unit()).with_instances(alloc::vec![at(0.0, 0.0, 2.0), at(0.0, 0.0, -2.0)]))?;
	let camera = camera_at(&mut scene, Vec3::new(0.0, 0.0, 5.0))?;
	let built = queue::build(&mut scene, &camera)?;
	let entry = built.opaque.iter().find(|entry| entry.drawable == spread).ok_or(crate::Trouble::Failed(alloc::string::String::from("the set was not queued")))?;
	require!(entry.instance_count() == 2, "both instances are drawn");
	require!(close(entry.bounds.centre.z, 0.0), "the set's sphere is centred between them: {:?}", entry.bounds.centre);
	require!(close(entry.depth, 5.0), "and there is ONE distance for the set, which is 5 and was {}", entry.depth);

	// AND IT DOES NOT MOVE WHEN AN INSTANCE LEAVES THE FRUSTUM. The far instance is pushed behind the
	// camera; the remaining one is nearer, and the set's key stays the whole set's.
	let held = entry.depth;
	let all_in = scene.add_drawable(Drawable::new(node, 0, opaque).with_bounds(unit()).with_instances(alloc::vec![at(0.0, 0.0, 2.0), at(0.0, 0.0, -2.0), at(0.0, 0.0, 400.0)]))?;
	let built = queue::build(&mut scene, &camera)?;
	let entry = built.opaque.iter().find(|entry| entry.drawable == all_in).ok_or(crate::Trouble::Failed(alloc::string::String::from("the set was not queued")))?;
	require!(entry.instance_count() == 2, "with one instance culled, two are drawn");
	require!(entry.depth > held, "and the key is still the WHOLE set's, which the culled instance widened: {} against {held}", entry.depth);
	Ok(())
}

pub fn transparent_instancing_approximate() -> Outcome {
	// TRANSPARENT INSTANCING IS PERMITTED AND IS DOCUMENTED AS APPROXIMATE. Instances within one set
	// are not sorted against each other, so overlapping transparent instances may composite in the
	// wrong order. REFUSING IT WOULD MAKE GRASS AND PARTICLES IMPOSSIBLE; hiding the limitation would
	// make the wrong picture a mystery. So the layer admits the set, sorts it as ONE, and this scene
	// is where the limitation is written down in a form that fails if it ever changes quietly.
	let mut scene = world();
	let glass = scene.add_material(material(MaterialKind::Unlit, Blending::Blended))?;
	let node = scene.add_node(Node::identity())?;
	let many = scene.add_drawable(Drawable::new(node, 0, glass).with_bounds(unit()).with_instances(alloc::vec![at(0.0, 0.0, 2.0), at(0.0, 0.0, -2.0)]))?;
	let camera = camera_at(&mut scene, Vec3::new(0.0, 0.0, 5.0))?;
	let built = queue::build(&mut scene, &camera)?;
	require!(built.transparent.len() == 1, "a transparent instanced set is admitted, as ONE entry: {}", built.transparent.len());
	let entry = &built.transparent[0];
	require!(entry.drawable == many && entry.instance_count() == 2, "with both instances in it");
	require!(entry.instances == alloc::vec![0, 1], "in SUBMISSION order and not in depth order, which is the approximation: {:?}", entry.instances);
	require!(close(entry.depth, 5.0), "and one distance for the set, which is what it sorts against other drawables by: {}", entry.depth);
	Ok(())
}
