//! THE BOUNDS AND CULLING GROUP: what a volume is, where it lives, and what rejects it.
//!
//! CULLING IS OBSERVABLE ONLY IN PERFORMANCE, which is what makes it hard to test and easy to get
//! wrong: a cull that removes something visible looks like a rendering bug, and a cull that removes
//! nothing looks like a slow backend. So every scene below asserts one of the two halves - what must
//! be rejected, and what must NEVER be.
//!
//! AND THE BOX IS LOCAL WHILE THE SPHERE IS WORLD. A box re-fitted to a rotated mesh every frame
//! GROWS: the axis bound of a rotated box is larger than the original, and re-fitting that gives a
//! larger one again, until after a few hundred frames everything is visible. A sphere has no
//! orientation, so transforming it is idempotent - and one scene below measures exactly that.

use crate::Outcome;
use crate::harness::close;
use crate::scene::{camera_at, material, world};
use render_math::{Mat4, Vec3};
use scene3d::{Aabb, Blending, Camera, Drawable, Frustum, MaterialKind, Node, Side, Sphere, Visibility, queue};

/// The frustum of a camera at the origin looking down `-z`: a right angle across, from 1 to 10.
fn frustum() -> Result<Frustum, crate::Trouble> {
	let projection = Camera::perspective(0, core::f32::consts::FRAC_PI_2, 1.0, 1.0, 10.0, u32::MAX)?;
	Ok(Frustum::from_view_projection(projection.projection()))
}

fn unit() -> Aabb {
	Aabb::new(Vec3::new(-0.5, -0.5, -0.5), Vec3::new(0.5, 0.5, 0.5))
}

pub fn local_bounding_box() -> Outcome {
	// THE BOX IS WHAT A MESH CAN STATE EXACTLY, in the mesh's OWN space - so one mesh drawn at ten
	// places carries one box and not ten. And a box turned inside out is refused where it enters,
	// because the slab test reads a negative extent as a miss and the p/n-vertex test reads the same
	// box as visible: it is a caller's mistake, and two tests disagreeing about it is not.
	let box_ = unit();
	require!(box_.centre() == Vec3::ZERO, "a box states its own centre: {:?}", box_.centre());
	require!(box_.half_extent() == Vec3::new(0.5, 0.5, 0.5), "and its own half extent: {:?}", box_.half_extent());
	require!(box_.is_well_formed(), "and a box with every minimum below its maximum is a box");
	require!(!Aabb::new(Vec3::new(1.0, 0.0, 0.0), Vec3::new(-1.0, 1.0, 1.0)).is_well_formed(), "while an inverted one is not");

	let mut scene = world();
	let opaque = scene.add_material(material(MaterialKind::Lambert, Blending::Opaque))?;
	let node = scene.add_node(Node::identity())?;
	require!(scene.add_drawable(Drawable::new(node, 0, opaque).with_bounds(Aabb::new(Vec3::new(1.0, 1.0, 1.0), Vec3::ZERO))).is_err(), "and an inverted box is refused where the drawable is added");

	// THE SAME LOCAL BOX UNDER TWO TRANSFORMS IS TWO WORLD VOLUMES, which is the whole of what "local"
	// buys: the mesh states its extent once.
	let here = box_.world_sphere(&Mat4::from_translation(Vec3::new(3.0, 0.0, 0.0)));
	let there = box_.world_sphere(&Mat4::from_translation(Vec3::new(-3.0, 0.0, 0.0)));
	require!(here.centre.x == 3.0 && there.centre.x == -3.0, "one box at two places is two volumes: {:?} and {:?}", here.centre, there.centre);
	require!(close(here.radius, there.radius), "with one radius between them");
	Ok(())
}

pub fn world_bounding_sphere() -> Outcome {
	// THE WORLD SPHERE IS THE LOCAL BOX'S CENTRE TRANSFORMED, WITH THE RADIUS SCALED BY THE LARGEST OF
	// THE THREE AXIS SCALES. The average is the plausible alternative and is WRONG: it produces a
	// sphere smaller than the geometry it claims to contain, which is a cull that removes something
	// visible - the one failure mode culling must not have.
	let squashed = Mat4::from_translation(Vec3::new(2.0, 0.0, 0.0)).mul(&Mat4::from_scale(Vec3::new(1.0, 3.0, 1.0)));
	let sphere = unit().world_sphere(&squashed);
	require!(sphere.centre == Vec3::new(2.0, 0.0, 0.0), "the centre is the local centre transformed: {:?}", sphere.centre);
	let local_radius = unit().bounding_sphere().radius;
	require!(close(sphere.radius, local_radius * 3.0), "and the radius takes the LARGEST axis scale, so {} and not {}", sphere.radius, local_radius * (1.0 + 3.0 + 1.0) / 3.0);

	// AND IT CONTAINS THE GEOMETRY, which is what the largest scale is for: every corner of the
	// transformed box is inside the sphere, and under the average radius the top and bottom are not.
	for corner in [Vec3::new(0.5, 0.5, 0.5), Vec3::new(-0.5, 0.5, -0.5), Vec3::new(0.5, -0.5, -0.5)] {
		let placed = squashed.transform_point(corner).truncate();
		require!(placed.sub(sphere.centre).length() <= sphere.radius + 1.0 / 255.0, "every corner is inside the world sphere, and {placed:?} is not");
	}
	let average = local_radius * (1.0 + 3.0 + 1.0) / 3.0;
	require!(squashed.transform_point(Vec3::new(0.5, 0.5, 0.5)).truncate().sub(sphere.centre).length() > average, "while an averaged radius would not have contained it");

	// A SPHERE HAS NO ORIENTATION, SO TRANSFORMING ONE IS IDEMPOTENT. This is the reason the per-frame
	// volume is a sphere and not a box: two half turns of the sphere give the same radius, and two
	// half turns of a RE-FITTED box give a bigger box than one full turn - which is the bound that
	// inflates until everything is visible.
	let turn = Mat4::from_columns(render_math::Vec4::new(0.7071068, 0.7071068, 0.0, 0.0), render_math::Vec4::new(-0.7071068, 0.7071068, 0.0, 0.0), render_math::Vec4::new(0.0, 0.0, 1.0, 0.0), render_math::Vec4::new(0.0, 0.0, 0.0, 1.0));
	let once = unit().bounding_sphere().transformed(&turn);
	let twice = once.transformed(&turn);
	require!(close(twice.radius, unit().bounding_sphere().radius), "a sphere turned twice has the radius it started with: {}", twice.radius);
	let box_once = unit().transformed(&turn);
	let box_twice = box_once.transformed(&turn);
	require!(box_twice.half_extent().x > unit().transformed(&turn.mul(&turn)).half_extent().x + 0.01, "while a re-fitted box grows: {:?} against {:?}", box_twice.half_extent(), unit().transformed(&turn.mul(&turn)).half_extent());
	Ok(())
}

pub fn frustum_plane_extraction() -> Outcome {
	// THE PLANES COME FROM THE MATRIX AND NOT FROM THE CAMERA'S NUMBERS, which is the only way they
	// stay correct for a projection the fields cannot describe - an oblique near plane, an infinite
	// far plane, a matrix built by hand. A culler that rebuilt them from a field of view would cull
	// against a different volume than the vertices are clipped against, and the difference is geometry
	// that disappears near the edge of the screen.
	let frustum = frustum()?;
	for (index, plane) in frustum.planes().iter().enumerate() {
		let length = Vec3::new(plane.x, plane.y, plane.z).length();
		require!(close(length, 1.0), "plane {index} is normalised, because the sphere test compares its distance against a radius in world units: {length}");
	}

	// AND THEY POINT INWARD, so a point inside the volume is at a positive distance from every one.
	let inside = Vec3::new(0.0, 0.0, -5.0);
	for (index, plane) in frustum.planes().iter().enumerate() {
		let distance = plane.x * inside.x + plane.y * inside.y + plane.z * inside.z + plane.w;
		require!(distance > 0.0, "a point in the middle of the frustum is inside plane {index}, and is at {distance}");
	}

	// THE INFINITE FAR PLANE IS THE CASE THE FIELDS CANNOT DESCRIBE, and extracting from the matrix
	// handles it without a special case: the far plane becomes one nothing is ever outside of.
	let unbounded = Camera::perspective(0, core::f32::consts::FRAC_PI_2, 1.0, 1.0, f32::INFINITY, u32::MAX)?;
	let unbounded = Frustum::from_view_projection(unbounded.projection());
	let distant = Sphere::new(Vec3::new(0.0, 0.0, -1.0e6), 1.0);
	require!(unbounded.test_sphere(&distant) != Visibility::Outside, "nothing is beyond an infinite far plane");
	require!(frustum.test_sphere(&distant) == Visibility::Outside, "while a finite one rejects the same sphere");
	Ok(())
}

pub fn sphere_frustum_test() -> Outcome {
	// THE REJECTION IS `dot(n, centre) + d < -radius`, AND IT IS CONSERVATIVE. A sphere PARTLY inside
	// is kept, because the one thing a culler may not do is drop something visible - so the three
	// answers are wholly in, wholly out, and might be.
	let frustum = frustum()?;
	require!(frustum.test_sphere(&Sphere::new(Vec3::new(0.0, 0.0, -5.0), 0.5)) == Visibility::Inside, "a small sphere in the middle is wholly inside");
	require!(frustum.test_sphere(&Sphere::new(Vec3::new(0.0, 0.0, 5.0), 0.5)) == Visibility::Outside, "one behind the camera is wholly outside");
	require!(frustum.test_sphere(&Sphere::new(Vec3::new(0.0, 0.0, -50.0), 0.5)) == Visibility::Outside, "and one past the far plane too");

	// THE CONSERVATIVE CASE: a sphere whose centre is outside the left plane but whose body crosses
	// it. A test that compared the centre alone would reject it, and the object would vanish as it
	// left the screen instead of sliding off it.
	let straddling = Sphere::new(Vec3::new(-5.2, 0.0, -5.0), 1.0);
	require!(frustum.test_sphere(&straddling) == Visibility::Intersecting, "a sphere crossing a plane is kept: {:?}", frustum.test_sphere(&straddling));
	require!(frustum.test_sphere(&Sphere::new(Vec3::new(-8.0, 0.0, -5.0), 1.0)) == Visibility::Outside, "while one wholly past it is rejected");

	// AND THE RADIUS IS WHAT DECIDES, which is what makes the normalised planes necessary: the same
	// centre with a larger radius crosses back in.
	require!(frustum.test_sphere(&Sphere::new(Vec3::new(-8.0, 0.0, -5.0), 4.0)) != Visibility::Outside, "a bigger sphere at the same centre reaches the frustum again");
	Ok(())
}

pub fn box_frustum_test() -> Outcome {
	// THE p/n-VERTEX TEST: for each plane the corner FURTHEST ALONG its normal decides "outside", and
	// the corner furthest against it decides "inside". Testing the box's bounding sphere instead is
	// correct and much LOOSER, and the difference is a long thin box near a corner of the screen that
	// is drawn for no reason.
	let frustum = frustum()?;
	require!(frustum.test_aabb(&Aabb::new(Vec3::new(-1.0, -1.0, -6.0), Vec3::new(1.0, 1.0, -4.0))) == Visibility::Inside, "a box in the middle is inside");
	require!(frustum.test_aabb(&Aabb::new(Vec3::new(-1.0, -1.0, 4.0), Vec3::new(1.0, 1.0, 6.0))) == Visibility::Outside, "one behind the camera is outside");

	// THE CASE THE SPHERE TEST GETS WRONG: a long flat box lying just outside the left plane. Its
	// bounding sphere reaches into the frustum and its corners do not, so a sphere-only test draws it.
	let thin = Aabb::new(Vec3::new(-12.0, -0.05, -5.05), Vec3::new(-6.0, 0.05, -4.95));
	require!(frustum.test_aabb(&thin) == Visibility::Outside, "a long thin box outside a plane is rejected: {:?}", frustum.test_aabb(&thin));
	require!(frustum.test_sphere(&thin.bounding_sphere()) != Visibility::Outside, "while its bounding sphere is not, which is why the box test exists");
	require!(frustum.test_aabb(&Aabb::new(Vec3::new(-6.0, -1.0, -6.0), Vec3::new(0.0, 1.0, -4.0))) == Visibility::Intersecting, "and a box crossing a plane is kept");
	Ok(())
}

pub fn fixed_plane_test_order() -> Outcome {
	// THE ORDER IS NEAR, FAR, LEFT, RIGHT, BOTTOM, TOP, AND IT IS FIXED. A volume outside TWO planes
	// is rejected by whichever is tested first, so two implementations that disagree about the order
	// name different planes in their reports - and a report that names a different plane on each
	// machine is a report nobody can act on. Near first, because it rejects the most in an ordinary
	// scene.
	require!(scene3d::cull::SIDES == [Side::Near, Side::Far, Side::Left, Side::Right, Side::Bottom, Side::Top], "the published order is the profile's: {:?}", scene3d::cull::SIDES);
	let frustum = frustum()?;
	// Behind the camera AND far to the left: outside both, and Near is tested first.
	let both = Sphere::new(Vec3::new(-100.0, 0.0, 10.0), 1.0);
	require!(frustum.rejected_by(&both) == Err(Side::Near), "a sphere outside the near and the left plane is rejected by NEAR: {:?}", frustum.rejected_by(&both));
	// Past the far plane AND below: Far is tested before Bottom.
	let below = Sphere::new(Vec3::new(0.0, -100.0, -50.0), 1.0);
	require!(frustum.rejected_by(&below) == Err(Side::Far), "and one past the far plane and below is rejected by FAR: {:?}", frustum.rejected_by(&below));
	// Each of the four sides on its own, so the order above is a boundary and not a plane that
	// answers for everything.
	require!(frustum.rejected_by(&Sphere::new(Vec3::new(-20.0, 0.0, -5.0), 1.0)) == Err(Side::Left), "a sphere to the left is rejected by LEFT");
	require!(frustum.rejected_by(&Sphere::new(Vec3::new(20.0, 0.0, -5.0), 1.0)) == Err(Side::Right), "one to the right by RIGHT");
	require!(frustum.rejected_by(&Sphere::new(Vec3::new(0.0, -20.0, -5.0), 1.0)) == Err(Side::Bottom), "one below by BOTTOM");
	require!(frustum.rejected_by(&Sphere::new(Vec3::new(0.0, 20.0, -5.0), 1.0)) == Err(Side::Top), "and one above by TOP");
	Ok(())
}

pub fn unbounded_never_culled() -> Outcome {
	// A DRAWABLE WITH NO BOUNDS IS NEVER CULLED. An unbounded drawable is one the scene cannot reason
	// about - a sky, a full-screen effect, geometry a shader displaces - and dropping it would make it
	// disappear for a reason nobody can see. So the scene draws it and says so.
	let mut scene = world();
	let opaque = scene.add_material(material(MaterialKind::Lambert, Blending::Opaque))?;
	let behind = scene.add_node(Node::identity().with_translation(Vec3::new(0.0, 0.0, 500.0)))?;
	let unbounded = scene.add_drawable(Drawable::new(behind, 0, opaque))?;
	let bounded = scene.add_drawable(Drawable::new(behind, 0, opaque).with_bounds(unit()))?;
	let camera = camera_at(&mut scene, Vec3::new(0.0, 0.0, 5.0))?;
	let built = queue::build(&mut scene, &camera)?;
	let drawn: alloc::vec::Vec<u32> = built.opaque.iter().map(|entry| entry.drawable).collect();
	require!(drawn == alloc::vec![unbounded], "the unbounded drawable survives a position far behind the camera, and the queue held {drawn:?}");
	require!(built.culled == 1, "while the bounded one at the same place is culled, and {} were", built.culled);
	let _ = bounded;

	// AND IT STILL SORTS, by its node's position - which is the only thing about it the scene knows.
	let queued = &built.opaque[0];
	require!(close(queued.depth, 495.0), "an unbounded drawable sorts by its node's position, at {}", queued.depth);
	require!(queued.bounds.radius == 0.0, "and carries no volume: {:?}", queued.bounds);
	Ok(())
}
