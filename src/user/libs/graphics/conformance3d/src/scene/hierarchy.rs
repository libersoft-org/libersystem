//! THE HIERARCHY GROUP: what a node is, how a transform composes, and what a parent does to its
//! subtree.
//!
//! THE COMPOSITION ORDER IS THE WHOLE OF THIS GROUP. `world = parent * local` and
//! `local = T * R * S` are not conventions a layer may choose: the three do not commute, so a scene
//! authored under one order looks wrong under the other, and the difference is a shear nobody can
//! name. Each scene below picks numbers under which the WRONG order gives a different answer, because
//! a check with a uniform scale and no rotation passes under every ordering there is.

use crate::Outcome;
use crate::harness::close;
use crate::scene::world;
use render_math::{Mat4, Quat, Vec3};
use scene3d::{Node, normal_matrix};

/// The world translation of a node, which is what most of these scenes compare.
fn at(scene: &mut scene3d::Scene, node: u32) -> Vec3 {
	scene.update();
	scene.transforms()[node as usize].translation()
}

pub fn node() -> Outcome {
	// A NODE IS A TRANSFORM, and a node with no drawable is DRAWN AS NOTHING rather than being a
	// different kind of object. That is what makes a pure transform - a pivot, a group, an attachment
	// point - expressible at all: a layer with a separate "group" type would need every operation
	// twice.
	let mut scene = world();
	let node = scene.add_node(Node::identity().with_translation(Vec3::new(1.0, 2.0, 3.0)))?;
	require!(scene.nodes().len() == 1, "a scene holds the node it was given, and holds {}", scene.nodes().len());
	require!(scene.nodes()[node as usize].parent().is_none(), "a node with no parent stated has none");
	require!(at(&mut scene, node) == Vec3::new(1.0, 2.0, 3.0), "and its world transform is its own: {:?}", at(&mut scene, node));
	require!(scene.drawables().is_empty(), "a node is not a drawable, so a scene of one node draws nothing");
	Ok(())
}

pub fn parent_child_transform() -> Outcome {
	// `world = parent_world * local`, WHICH IS WHY MOVING A PARENT MOVES ITS CHILD. The scene is the
	// one every hierarchy exists for, and the check is that the child moved WITHOUT being touched.
	let mut scene = world();
	let parent = scene.add_node(Node::identity().with_translation(Vec3::new(1.0, 0.0, 0.0)))?;
	let child = scene.add_node(Node::identity().with_parent(parent).with_translation(Vec3::new(0.0, 2.0, 0.0)))?;
	require!(at(&mut scene, child) == Vec3::new(1.0, 2.0, 0.0), "a child is placed in its parent's frame: {:?}", at(&mut scene, child));
	scene.set_translation(parent, Vec3::new(-4.0, 0.0, 0.0))?;
	require!(at(&mut scene, child) == Vec3::new(-4.0, 2.0, 0.0), "and moving the parent moves it: {:?}", at(&mut scene, child));

	// AND THE PARENT'S ROTATION AND SCALE REACH IT TOO, which is what separates a hierarchy from a
	// list of offsets. A quarter turn about `z` sends the child's `+y` offset to `-x`.
	scene.set_translation(parent, Vec3::ZERO)?;
	scene.set_rotation(parent, Quat::from_axis_angle(Vec3::new(0.0, 0.0, 1.0), core::f32::consts::FRAC_PI_2).unwrap_or(Quat::IDENTITY))?;
	let placed = at(&mut scene, child);
	require!(close(placed.x, -2.0) && close(placed.y, 0.0), "a parent's rotation turns its child about the parent: {placed:?}");
	Ok(())
}

pub fn local_transform_order() -> Outcome {
	// `local = TRANSLATION * ROTATION * SCALE`, STATED BECAUSE THE THREE DO NOT COMMUTE. The numbers
	// are chosen so that every other order gives a different answer: a non-uniform scale on `x` with
	// a quarter turn about `z` and a translation that is not zero.
	//
	// UNDER T*R*S the local `+x` axis is scaled to twice its length and THEN turned onto `+y`, so it
	// arrives at `(0, 2, 0)` before the translation. Under T*S*R it is turned first and the scale then
	// applies to `y`, which leaves it at `(0, 1, 0)` - and under S*R*T the translation itself would be
	// scaled, which is the mistake that makes a child drift away from its parent as the parent grows.
	let node = Node::identity().with_translation(Vec3::new(10.0, 0.0, 0.0)).with_rotation(Quat::from_axis_angle(Vec3::new(0.0, 0.0, 1.0), core::f32::consts::FRAC_PI_2).unwrap_or(Quat::IDENTITY)).with_scale(Vec3::new(2.0, 1.0, 1.0));
	let local = node.local();
	let mapped = local.transform_point(Vec3::new(1.0, 0.0, 0.0)).truncate();
	require!(close(mapped.x, 10.0) && close(mapped.y, 2.0), "the local x axis is scaled and then turned, and lands at {mapped:?}");
	let origin = local.transform_point(Vec3::ZERO).truncate();
	require!(close(origin.x, 10.0) && close(origin.y, 0.0), "and the translation is NOT scaled, so the origin stays at the translation: {origin:?}");

	// AND IT IS THE PRODUCT ITSELF, so a caller composing the three by hand gets the same matrix.
	let expected = Mat4::from_translation(node.translation()).mul(&node.rotation().to_mat4()).mul(&Mat4::from_scale(node.scale()));
	for column in 0..4 {
		for row in 0..4 {
			require!(close(local.at(row, column), expected.at(row, column)), "the local transform is T*R*S at ({row}, {column}): {} against {}", local.at(row, column), expected.at(row, column));
		}
	}
	Ok(())
}

pub fn quaternion_rotation() -> Outcome {
	// THE ROTATION IS A UNIT QUATERNION AND IS NORMALISED ON COMPOSITION. A matrix drifts away from
	// orthonormal as it is composed, which shows up as a child that slowly shears; a quaternion that
	// is not renormalised does the same thing more slowly. So a rotation given at three times its
	// length must produce the SAME transform as the unit one - not one three times the size.
	let mut scene = world();
	let axis = Vec3::new(0.0, 0.0, 1.0);
	let unit = Quat::from_axis_angle(axis, core::f32::consts::FRAC_PI_2).unwrap_or(Quat::IDENTITY);
	let node = scene.add_node(Node::identity().with_rotation(unit))?;
	let stretched = scene.add_node(Node::identity().with_rotation(Quat { x: unit.x * 3.0, y: unit.y * 3.0, z: unit.z * 3.0, w: unit.w * 3.0 }))?;
	scene.update();
	let point = Vec3::new(1.0, 0.0, 0.0);
	let turned = scene.transforms()[node as usize].transform_point(point).truncate();
	let also = scene.transforms()[stretched as usize].transform_point(point).truncate();
	require!(close(turned.x, 0.0) && close(turned.y, 1.0), "a quarter turn about z sends +x to +y: {turned:?}");
	require!(close(also.x, turned.x) && close(also.y, turned.y) && close(also.z, turned.z), "and a quaternion at three times its length is the same rotation and not a scale: {also:?}");

	// A QUATERNION THAT IS NOT ONE IS REFUSED where it is set, because it cannot be normalised and
	// every transform after it would be a NaN with nothing left to say where it came from.
	require!(scene.set_rotation(node, Quat { x: 0.0, y: 0.0, z: 0.0, w: 0.0 }).is_err(), "a zero quaternion is refused rather than normalised into a NaN");
	Ok(())
}

pub fn normal_matrix_feature() -> Outcome {
	// A NORMAL IS TRANSFORMED BY THE INVERSE TRANSPOSE OF THE UPPER 3x3 AND NOT BY THE MATRIX. Under a
	// uniform scale the two agree, which is why this scene uses a SQUASH: the property is that a
	// normal stays perpendicular to the surface, so a tangent transformed by the matrix and a normal
	// transformed by the normal matrix must still meet at a right angle.
	let squash = Mat4::from_scale(Vec3::new(1.0, 0.25, 1.0));
	let normal = Vec3::new(1.0, 1.0, 0.0).normalise().unwrap_or(Vec3::new(1.0, 0.0, 0.0));
	// A tangent of the plane the normal belongs to: perpendicular to it, in the same plane.
	let tangent = Vec3::new(-1.0, 1.0, 0.0).normalise().unwrap_or(Vec3::new(0.0, 1.0, 0.0));
	require!(close(normal.dot(tangent), 0.0), "the fixture starts with a normal perpendicular to its tangent");

	let moved_tangent = squash.linear().mul_vector(tangent);
	let moved_normal = normal_matrix(&squash).mul_vector(normal);
	require!(close(moved_normal.dot(moved_tangent), 0.0), "after a squash the normal is still perpendicular to the surface: {}", moved_normal.dot(moved_tangent));

	// AND THE MATRIX ITSELF IS NOT THAT ANSWER, which is what makes this a feature rather than a
	// restatement: transforming the normal the way the position is transformed TILTS IT, and the one
	// squashed object in a scene is then lit wrongly in a way nothing else shows.
	let naive = squash.linear().mul_vector(normal);
	require!(!close(naive.dot(moved_tangent), 0.0), "and the matrix itself would have tilted it off the surface, by {}", naive.dot(moved_tangent));
	Ok(())
}

pub fn lazy_world_transforms() -> Outcome {
	// WORLD TRANSFORMS ARE COMPUTED LAZILY, ONCE PER FRAME PER DIRTY SUBTREE. What makes that
	// observable rather than an implementation note is the DIRTY flag: a scene says whether it is up
	// to date, a change makes it not, and an update makes it so again - which is what lets two views
	// of one scene cost one traversal rather than two.
	let mut scene = world();
	let parent = scene.add_node(Node::identity())?;
	let child = scene.add_node(Node::identity().with_parent(parent).with_translation(Vec3::new(0.0, 1.0, 0.0)))?;
	scene.update();
	require!(scene.is_up_to_date(), "after an update nothing is dirty");
	scene.set_translation(parent, Vec3::new(3.0, 0.0, 0.0))?;
	require!(!scene.is_up_to_date(), "a change to a node marks the scene out of date");
	require!(scene.transforms()[child as usize].translation() == Vec3::new(0.0, 1.0, 0.0), "and the cached transform is still the old one until an update: {:?}", scene.transforms()[child as usize].translation());

	// THE PARENT'S CHANGE DIRTIES ITS WHOLE SUBTREE, which is the half a per-node dirty flag gets
	// wrong: the child was never touched, and its world transform still has to move.
	scene.update();
	require!(scene.transforms()[child as usize].translation() == Vec3::new(3.0, 1.0, 0.0), "one update moves the whole subtree: {:?}", scene.transforms()[child as usize].translation());
	require!(scene.is_up_to_date(), "and leaves nothing dirty");
	let after = scene.transforms()[child as usize];
	scene.update();
	require!(scene.transforms()[child as usize] == after, "a second update with nothing changed is idempotent");
	Ok(())
}

pub fn hierarchy_cycle_refusal() -> Outcome {
	// A CYCLE IS REFUSED WHERE THE PARENT IS SET AND NOT DISCOVERED DURING A TRAVERSAL, because a
	// traversal that discovered it would already be inside an infinite loop. The refusal names both
	// ends, and the scene is left exactly as it was.
	let mut scene = world();
	let grandparent = scene.add_node(Node::identity())?;
	let parent = scene.add_node(Node::identity().with_parent(grandparent))?;
	let child = scene.add_node(Node::identity().with_parent(parent))?;
	require!(scene.set_parent(grandparent, Some(child)).is_err(), "making a node the child of its own descendant is refused");
	require!(scene.set_parent(grandparent, Some(grandparent)).is_err(), "and so is making one its own parent");
	require!(scene.nodes()[grandparent as usize].parent().is_none(), "and the refused edge was not applied");
	require!(scene.nodes()[child as usize].parent() == Some(parent), "and nothing else moved either");
	Ok(())
}

pub fn hierarchy_depth_limit() -> Outcome {
	// THE DEPTH IS BOUNDED AND THE BOUND IS CHECKED WHERE THE PARENT IS SET, so a traversal needs no
	// depth counter and cannot overflow a stack. A limit checked during the traversal would be a
	// limit checked after the stack was already deep.
	let mut limits = scene3d::Limits::PROFILE_MINIMUM;
	limits.max_hierarchy_depth = 4;
	let mut scene = scene3d::Scene::new(limits);
	let mut chain = alloc::vec![scene.add_node(Node::identity())?];
	for _ in 1..4 {
		let parent = *chain.last().unwrap_or(&0);
		chain.push(scene.add_node(Node::identity().with_parent(parent))?);
	}
	require!(scene.nodes()[chain[3] as usize].depth() == 3, "a chain of four is three deep, and is {}", scene.nodes()[chain[3] as usize].depth());
	let past = scene.add_node(Node::identity().with_parent(chain[3]));
	require!(matches!(past, Err(scene3d::Error::TooDeep { .. })), "a fifth node in the chain is refused by DEPTH and not by anything else: {past:?}");
	require!(scene.add_node(Node::identity()).is_ok(), "while a fifth ROOT is admitted, because the limit is on depth and not on count");

	// AND MOVING A SUBTREE IS CHECKED THE SAME WAY, which is the case a check at `add_node` alone
	// misses: reparenting deepens every node below the one that moved.
	let mut scene = scene3d::Scene::new(limits);
	let a = scene.add_node(Node::identity())?;
	let b = scene.add_node(Node::identity().with_parent(a))?;
	let c = scene.add_node(Node::identity())?;
	let d = scene.add_node(Node::identity().with_parent(c))?;
	let e = scene.add_node(Node::identity().with_parent(d))?;
	require!(scene.set_parent(c, Some(b)).is_err(), "moving a three-deep subtree under a two-deep one is refused");
	require!(scene.nodes()[c as usize].parent().is_none(), "and the move was undone rather than half applied");
	require!(scene.nodes()[e as usize].depth() == 2, "and the subtree kept the depth it had, which is {}", scene.nodes()[e as usize].depth());
	Ok(())
}

pub fn visibility_mask() -> Outcome {
	// A MASK SAYS WHICH VIEWS SEE A SUBTREE, AND IT IS ANDed DOWN THE HIERARCHY. A child that could be
	// seen by a view its parent is hidden from would make a mask on a group mean nothing - which is
	// the whole reason to put one on a group.
	let mut scene = world();
	let parent = scene.add_node(Node::identity().with_visibility(0b0011))?;
	let child = scene.add_node(Node::identity().with_parent(parent).with_visibility(0b0110))?;
	require!(scene.effective_visibility(child) == 0b0010, "a child's mask is its own ANDed with its parent's, and is {:#b}", scene.effective_visibility(child));
	require!(scene.effective_visibility(parent) == 0b0011, "and the parent keeps its own");
	scene.set_visibility(parent, 0b1001)?;
	require!(scene.effective_visibility(child) == 0, "a parent that shares no view with its child hides the child entirely, and left {:#b}", scene.effective_visibility(child));
	Ok(())
}

pub fn node_enable() -> Outcome {
	// AN ANCESTOR'S `enabled` HIDES ITS WHOLE SUBTREE, which is what makes hiding a group one edit
	// rather than one per descendant. It is SEPARATE from the visibility mask because the two answer
	// different questions: a mask is "which views", and this is "at all".
	let mut scene = world();
	let parent = scene.add_node(Node::identity())?;
	let child = scene.add_node(Node::identity().with_parent(parent))?;
	require!(scene.effectively_enabled(child), "a node under an enabled parent is enabled");
	scene.set_enabled(parent, false)?;
	require!(!scene.effectively_enabled(child), "and disabling the parent disables it");
	require!(!scene.effectively_enabled(parent), "as well as the parent itself");
	require!(scene.nodes()[child as usize].is_enabled(), "while the child's OWN flag is untouched, so re-enabling the parent restores it");
	scene.set_enabled(parent, true)?;
	require!(scene.effectively_enabled(child), "which it does");
	Ok(())
}
