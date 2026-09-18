//! THE CAMERA GROUP: where a view comes from, what it projects with, and what it refuses.
//!
//! A CAMERA IS A NODE, which is the decision the whole group follows from: its view transform is the
//! INVERSE of its node's world transform, so parenting one to a vehicle needs no second mechanism and
//! a camera cannot be somewhere its hierarchy does not put it.
//!
//! AND A DEGENERATE PROJECTION IS REFUSED AT THE CALL THAT SETS IT. A zero near plane or a far at or
//! below near produces a matrix of infinities, and every vertex after it is a NaN with nothing left to
//! say where it came from - so the refusal has to be here, where the numbers are still the caller's.

use crate::Outcome;
use crate::harness::close;
use crate::scene::world;
use render_math::{Mat4, Quat, Vec3};
use scene3d::{Camera, Node};

/// Where a world-space point lands in NDC under a camera, or `None` if it is behind the eye.
fn ndc(scene: &mut scene3d::Scene, camera: &Camera, point: Vec3) -> Option<Vec3> {
	scene.update();
	let view_projection = scene.view_projection_of(camera).ok()?;
	view_projection.transform_point(point).perspective_divide().ok()
}

pub fn perspective_camera() -> Outcome {
	// A PERSPECTIVE CAMERA IS GIVEN A VERTICAL FIELD OF VIEW, an aspect, a near and a far, and what
	// makes the vertical one the profile's choice is that a window which changes WIDTH then keeps the
	// same amount of vertical content - which is what a person expects when they drag a corner.
	let mut scene = world();
	let node = scene.add_node(Node::identity().with_translation(Vec3::new(0.0, 0.0, 5.0)))?;
	let camera = Camera::perspective(node, core::f32::consts::FRAC_PI_2, 1.0, 1.0, 100.0, u32::MAX)?;
	let centre = ndc(&mut scene, &camera, Vec3::ZERO).ok_or(crate::Trouble::Failed(alloc::string::String::from("a point in front of the camera has no projection")))?;
	require!(close(centre.x, 0.0) && close(centre.y, 0.0), "a point on the view axis lands in the middle: {centre:?}");

	// A RIGHT-ANGLE FIELD OF VIEW PUTS THE EDGE OF THE FRUSTUM AT 45 DEGREES, so a point one unit up
	// and one unit in front of the eye lands exactly on the top edge - which is NDC `y = +1`, because
	// this stack's Y INVERSION IS IN THE VIEWPORT TRANSFORM AND NOWHERE ELSE. A projection that
	// flipped it here would give a picture upside down and a winding that culls the wrong faces.
	let edge = ndc(&mut scene, &camera, Vec3::new(0.0, 1.0, 4.0)).ok_or(crate::Trouble::Failed(alloc::string::String::from("the frustum edge has no projection")))?;
	require!(close(edge.y, 1.0), "and a point on the top of the frustum lands on the top edge of NDC: {edge:?}");

	// AND THINGS FURTHER AWAY ARE SMALLER, which is the whole of what a perspective projection is and
	// the one thing an orthographic one does not do.
	let near = ndc(&mut scene, &camera, Vec3::new(1.0, 0.0, 4.0)).ok_or(crate::Trouble::Failed(alloc::string::String::from("no projection")))?;
	let far = ndc(&mut scene, &camera, Vec3::new(1.0, 0.0, -5.0)).ok_or(crate::Trouble::Failed(alloc::string::String::from("no projection")))?;
	require!(far.x.abs() < near.x.abs(), "the same offset is smaller further away: {} against {}", far.x, near.x);
	Ok(())
}

pub fn orthographic_camera() -> Outcome {
	// AN ORTHOGRAPHIC CAMERA IS GIVEN HALF-EXTENTS CENTRED ON THE VIEW AXIS, so it can be swapped for
	// a perspective one without the content moving sideways. What makes it its own feature is the
	// property a perspective camera does not have: distance changes nothing.
	let mut scene = world();
	let node = scene.add_node(Node::identity().with_translation(Vec3::new(0.0, 0.0, 5.0)))?;
	let camera = Camera::orthographic(node, 2.0, 2.0, 1.0, 100.0, u32::MAX)?;
	let near = ndc(&mut scene, &camera, Vec3::new(1.0, 0.0, 0.0)).ok_or(crate::Trouble::Failed(alloc::string::String::from("no projection")))?;
	let far = ndc(&mut scene, &camera, Vec3::new(1.0, 0.0, -50.0)).ok_or(crate::Trouble::Failed(alloc::string::String::from("no projection")))?;
	require!(close(near.x, 0.5), "a point at half the half-width lands at half of NDC: {near:?}");
	require!(close(far.x, near.x), "and the same offset fifty units further away lands in the same place: {far:?}");
	require!(far.z > near.z, "while the depth still increases with distance: {} against {}", far.z, near.z);
	Ok(())
}

pub fn infinite_far_plane() -> Outcome {
	// AN INFINITE FAR PLANE IS PERMITTED AND IS SPELLED AS A FAR OF INFINITY. With a `[0, 1]` depth
	// range it is WELL CONDITIONED - the precision is spent near the camera either way - so a scene
	// that does not know how far it reaches does not have to guess a number and hope.
	let mut scene = world();
	let node = scene.add_node(Node::identity().with_translation(Vec3::new(0.0, 0.0, 5.0)))?;
	let camera = Camera::perspective(node, 1.0, 1.0, 1.0, f32::INFINITY, u32::MAX)?;
	require!(camera.projection().is_finite(), "an infinite far plane still gives a finite matrix");
	let near = ndc(&mut scene, &camera, Vec3::new(0.0, 0.0, 4.0)).ok_or(crate::Trouble::Failed(alloc::string::String::from("no projection")))?;
	let far = ndc(&mut scene, &camera, Vec3::new(0.0, 0.0, -1.0e6)).ok_or(crate::Trouble::Failed(alloc::string::String::from("no projection")))?;
	require!(close(near.z, 0.0), "the near plane is depth zero: {}", near.z);
	require!(far.z < 1.0 && far.z > 0.99, "and nothing ever reaches one, which is what makes the far plane infinite: {}", far.z);
	require!(far.z > near.z, "while depth still increases with distance");
	Ok(())
}

pub fn view_from_node_transform() -> Outcome {
	// THE VIEW IS THE INVERSE OF THE CAMERA NODE'S WORLD TRANSFORM, so a camera parented to something
	// moves with it. This is the scene that says a camera is a node like any other: NOTHING is set on
	// the camera at all, and the view changes because its PARENT moved.
	let mut scene = world();
	let vehicle = scene.add_node(Node::identity())?;
	let mount = scene.add_node(Node::identity().with_parent(vehicle).with_translation(Vec3::new(0.0, 0.0, 5.0)))?;
	let camera = Camera::perspective(mount, 1.0, 1.0, 0.1, 100.0, u32::MAX)?;
	scene.update();
	let view = scene.view_of(&camera)?;
	let world_transform = scene.transforms()[mount as usize];
	let product = view.mul(&world_transform);
	for column in 0..4 {
		for row in 0..4 {
			let expected = if row == column { 1.0 } else { 0.0 };
			require!(close(product.at(row, column), expected), "the view times the world transform is the identity at ({row}, {column}): {}", product.at(row, column));
		}
	}

	// AND MOVING THE PARENT MOVES THE VIEW, which is the point.
	scene.set_translation(vehicle, Vec3::new(10.0, 0.0, 0.0))?;
	scene.update();
	let moved = scene.view_of(&camera)?;
	require!(!close(moved.at(0, 3), view.at(0, 3)), "moving the camera's parent changed the view: {} against {}", moved.at(0, 3), view.at(0, 3));
	let origin = moved.transform_point(Vec3::new(10.0, 0.0, 5.0)).truncate();
	require!(close(origin.x, 0.0) && close(origin.y, 0.0) && close(origin.z, 0.0), "and the camera's own world position is the origin of view space: {origin:?}");

	// A ROTATION ON THE PARENT REACHES IT TOO, and it reaches BOTH halves of the view: a half turn
	// about `y` swings the camera to the other side of its parent AND turns it to face back the way it
	// came - so the pivot is still in front of it, five units away, which a layer that applied the
	// rotation without the translation would not manage.
	scene.set_translation(vehicle, Vec3::ZERO)?;
	scene.set_rotation(vehicle, Quat::from_axis_angle(Vec3::new(0.0, 1.0, 0.0), core::f32::consts::PI).unwrap_or(Quat::IDENTITY))?;
	scene.update();
	let position = scene.transforms()[mount as usize].translation();
	require!(close(position.z, -5.0), "the camera swung to the other side of its parent: {position:?}");
	let turned = scene.view_of(&camera)?.transform_point(Vec3::ZERO).truncate();
	require!(close(turned.z, -5.0) && close(turned.x, 0.0), "and still looks at the pivot, five units in front of it: {turned:?}");
	Ok(())
}

pub fn custom_projection() -> Outcome {
	// A CALLER MAY BUILD ITS OWN PROJECTION - an oblique near plane, a reversed range, anything the
	// two constructors cannot describe - and the layer carries it unchanged. A layer that rebuilt the
	// matrix from fields would silently replace it, and the frustum would then be extracted from a
	// different volume than the vertices are clipped against.
	let mut scene = world();
	let node = scene.add_node(Node::identity())?;
	let mine = Mat4::from_array([2.0, 0.0, 0.0, 0.0, 0.0, 3.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.0, 0.25, 0.0, 0.5, 1.0]);
	let camera = Camera::from_projection(node, mine, u32::MAX)?;
	for index in 0..16 {
		require!(camera.projection().to_array()[index] == mine.to_array()[index], "the matrix is carried unchanged, and differs at {index}");
	}
	let _ = scene.add_camera(camera);

	// AND A NON-FINITE MATRIX IS REFUSED, which is the one property no projection can have.
	let mut broken = mine.to_array();
	broken[5] = f32::NAN;
	require!(Camera::from_projection(node, Mat4::from_array(broken), u32::MAX).is_err(), "a projection with a NaN in it is refused");
	Ok(())
}

pub fn degenerate_projection_refusal() -> Outcome {
	// EVERY DEGENERATE PROJECTION IS REFUSED WHERE IT IS SET. Each of these produces infinities, and
	// an infinity in a projection is a frame of NaNs with nothing left to say where they came from -
	// so the refusal names the numbers while they are still the caller's.
	let node = 0;
	require!(Camera::perspective(node, 1.0, 1.0, 0.0, 100.0, u32::MAX).is_err(), "a zero near plane is refused");
	require!(Camera::perspective(node, 1.0, 1.0, -1.0, 100.0, u32::MAX).is_err(), "and a negative one");
	require!(Camera::perspective(node, 1.0, 1.0, 10.0, 10.0, u32::MAX).is_err(), "and a far plane at near");
	require!(Camera::perspective(node, 1.0, 1.0, 10.0, 1.0, u32::MAX).is_err(), "and one in front of it");
	require!(Camera::perspective(node, 1.0, 0.0, 1.0, 100.0, u32::MAX).is_err(), "and an aspect of zero");
	require!(Camera::perspective(node, f32::NAN, 1.0, 1.0, 100.0, u32::MAX).is_err(), "and a field of view that is not a number");
	require!(Camera::perspective(node, core::f32::consts::PI, 1.0, 1.0, 100.0, u32::MAX).is_err(), "and one at a half turn, where the frustum has no volume");
	require!(Camera::orthographic(node, 0.0, 1.0, 1.0, 100.0, u32::MAX).is_err(), "an orthographic camera with no width is refused");
	require!(Camera::orthographic(node, 1.0, 1.0, 10.0, 10.0, u32::MAX).is_err(), "and one whose far plane is at its near plane");

	// AND THE ONES THAT ARE NOT DEGENERATE ARE ADMITTED, so the refusals above are a boundary rather
	// than a layer that refuses everything.
	require!(Camera::perspective(node, 1.0, 1.0, 0.01, f32::INFINITY, u32::MAX).is_ok(), "a near plane close to zero and an infinite far plane is a projection");
	require!(Camera::orthographic(node, 1.0, 1.0, -5.0, 5.0, u32::MAX).is_ok(), "and an orthographic near plane behind the camera is one too, which a perspective one cannot be");
	Ok(())
}
