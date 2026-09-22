//! The fixtures. Each holds a RULE - `Scene3D Core Profile 1`'s, or one this crate decides - against
//! a value worked out by hand from the definition, so an implementation that changes the arithmetic
//! and keeps the rule passes, and one that changes the rule fails.

use super::*;

use alloc::vec;
use alloc::vec::Vec;

use render_math::{Mat4, Quat, Vec3, Vec4, Viewport, look_at_rh, perspective_rh_zo};
use render3d::command::{Buffer, CommandList, GraphicsPipeline, PipelineState, Rect, Topology};
use render3d::resource::{Aspect, LoadOp, RenderTargetSet, RenderTargetView, StoreOp, TextureDimension, TextureViewDesc};
use render3d::{Command, Cull, Render3DLimits};

use crate::light::{Light, LightKind};
use crate::material::{Blending, Incident, Material, MaterialKind, Surface};
use crate::scene::{Camera, Drawable, Error, Instance, Limits, Node, Scene};

fn near(left: f32, right: f32) -> bool {
	(left - right).abs() <= 1e-4
}

fn at(translation: Vec3) -> Node {
	Node::identity().with_translation(translation)
}

/// A unit cube centred on its node's origin.
fn cube() -> Aabb {
	Aabb::new(Vec3::new(-0.5, -0.5, -0.5), Vec3::new(0.5, 0.5, 0.5))
}

fn opaque(pipeline: u32) -> Material {
	Material::new(MaterialKind::Unlit, GraphicsPipeline(pipeline), pipeline)
}

fn blended(pipeline: u32) -> Material {
	opaque(pipeline).with_blending(Blending::Blended)
}

// ---------------------------------------------------------------------------------------------
// The frozen profile.
// ---------------------------------------------------------------------------------------------

#[test]
// THE LIMITS ARE THE PROFILE'S, BY NAME. A list checked by position drifts the first time one is
// added; a list checked by name cannot claim conformance while missing one.
fn every_limit_the_profile_names_is_one_this_layer_enforces() {
	let limits = Limits::PROFILE_MINIMUM;
	for entry in graphics_profile::scene3d::SCENE3D_PROFILE_1_MIN_LIMITS {
		let held = limits.by_name(entry.name).unwrap_or_else(|| panic!("the profile names `{}` and this layer has no such limit", entry.name));
		assert_eq!(held, entry.minimum, "`{}` must be the profile's floor", entry.name);
	}
	// And nothing is enforced that the profile does not name, which would be a limit an application
	// could hit without the document saying so.
	for name in ["max_nodes", "max_hierarchy_depth", "max_drawables", "max_instances_per_drawable", "max_lights", "max_lights_per_drawable", "max_materials", "max_cameras"] {
		assert!(graphics_profile::scene3d::SCENE3D_PROFILE_1_MIN_LIMITS.iter().any(|entry| entry.name == name), "`{name}` is enforced but the profile does not name it");
	}
}

#[test]
// THE QUEUES ARE THE PROFILE'S, IN ITS ORDER, with its depth-write answer.
fn the_queues_match_the_frozen_list_in_name_order_and_depth_write() {
	let frozen = graphics_profile::scene3d::QUEUES;
	assert_eq!(frozen.len(), QueueKind::ORDER.len());
	for (entry, kind) in frozen.iter().zip(QueueKind::ORDER) {
		let name = match kind {
			QueueKind::Opaque => "Opaque",
			QueueKind::AlphaMask => "AlphaMask",
			QueueKind::Transparent => "Transparent",
		};
		assert_eq!(entry.name, name, "the queues run in the profile's order");
		assert_eq!(entry.depth_write, kind.writes_depth(), "`{name}` writes depth as the profile says");
	}
}

// ---------------------------------------------------------------------------------------------
// The hierarchy.
// ---------------------------------------------------------------------------------------------

#[test]
// A CHILD'S WORLD TRANSFORM IS ITS PARENT'S TIMES ITS OWN, composed TRANSLATION * ROTATION * SCALE.
// The order matters: scaling after rotating shears, and a pure translation gives the same answer
// either way - which is why this fixture uses a scale AND a rotation.
fn a_world_transform_is_the_parent_chain_applied_outermost_last() {
	// @covers: Node, ParentChildTransform, LocalTransformOrder
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let parent = scene.add_node(at(Vec3::new(10.0, 0.0, 0.0)).with_scale(Vec3::new(2.0, 2.0, 2.0))).unwrap();
	let child = scene.add_node(at(Vec3::new(1.0, 0.0, 0.0)).with_parent(parent)).unwrap();
	let placed = scene.update()[child as usize].translation();
	assert!(near(placed.x, 12.0), "the child sits at the parent's translation plus its own, scaled: {}", placed.x);

	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let quarter = Quat::from_axis_angle(Vec3::new(0.0, 0.0, 1.0), core::f32::consts::FRAC_PI_2).unwrap();
	let parent = scene.add_node(Node::identity().with_rotation(quarter)).unwrap();
	let child = scene.add_node(at(Vec3::new(1.0, 0.0, 0.0)).with_parent(parent)).unwrap();
	let placed = scene.update()[child as usize].translation();
	assert!(near(placed.x, 0.0) && near(placed.y, 1.0), "a quarter turn about Z sends +X to +Y: ({}, {})", placed.x, placed.y);

	// TRANSLATION * ROTATION * SCALE, not the other orders: a non-uniform scale under a rotation
	// distinguishes them. Scale (2,1,1) then a quarter turn about Z puts the local +X axis, twice as
	// long, along world +Y.
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let node = scene.add_node(Node::identity().with_rotation(quarter).with_scale(Vec3::new(2.0, 1.0, 1.0))).unwrap();
	let world = scene.update()[node as usize];
	let tip = world.transform_point(Vec3::new(1.0, 0.0, 0.0)).truncate();
	assert!(near(tip.x, 0.0) && near(tip.y, 2.0), "scale applied before the rotation: ({}, {})", tip.x, tip.y);
}

#[test]
// THE ROTATION IS NORMALISED ON COMPOSITION, because a quaternion composed a thousand times drifts
// away from unit length and a matrix built from a drifted one SCALES - which reads as a child that
// slowly grows.
fn a_rotation_that_is_not_unit_length_is_normalised_rather_than_scaling_the_node() {
	// @covers: QuaternionRotation
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let drifted = Quat { x: 0.0, y: 0.0, z: 0.0, w: 4.0 };
	let node = scene.add_node(Node::identity().with_rotation(drifted)).unwrap();
	let world = scene.update()[node as usize];
	let tip = world.transform_point(Vec3::new(1.0, 0.0, 0.0)).truncate();
	assert!(near(tip.x, 1.0), "a quadrupled identity quaternion is still the identity: {}", tip.x);
	// A quaternion that cannot be normalised at all is refused where it enters.
	assert!(matches!(scene.add_node(Node::identity().with_rotation(Quat { x: 0.0, y: 0.0, z: 0.0, w: 0.0 })), Err(Error::Degenerate { .. })));
}

#[test]
// A PARENT'S CHANGE REACHES ITS WHOLE SUBTREE. A propagation that can be missed is an object that
// stays where it was, which is correct until something moves - the commonest scene-graph defect and
// the hardest to see.
fn moving_a_parent_moves_everything_under_it() {
	// @covers: LazyWorldTransforms
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let root = scene.add_node(Node::identity()).unwrap();
	let middle = scene.add_node(at(Vec3::new(1.0, 0.0, 0.0)).with_parent(root)).unwrap();
	let leaf = scene.add_node(at(Vec3::new(1.0, 0.0, 0.0)).with_parent(middle)).unwrap();
	assert!(near(scene.update()[leaf as usize].translation().x, 2.0));
	assert!(scene.is_up_to_date(), "one update leaves nothing stale");

	scene.set_translation(root, Vec3::new(10.0, 0.0, 0.0)).unwrap();
	assert!(!scene.is_up_to_date(), "a move marks the scene stale rather than leaving a silent cache");
	assert!(near(scene.update()[leaf as usize].translation().x, 12.0), "the grandchild moved with the root");
	// AND THE UPDATE IS IDEMPOTENT: a second one with nothing dirty changes nothing.
	let again = scene.update()[leaf as usize].translation().x;
	assert!(near(again, 12.0));
}

#[test]
// A RE-PARENTING THAT MAKES A PARENT'S INDEX HIGHER THAN ITS CHILD'S still composes correctly,
// because the traversal order is topological and rebuilt depth-first rather than being the order the
// nodes were added in. An implementation that walked the nodes by index would leave this child a
// frame behind its parent, for ever.
fn a_child_added_before_its_parent_still_follows_it() {
	// @covers: LazyWorldTransforms
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let first = scene.add_node(at(Vec3::new(1.0, 0.0, 0.0))).unwrap();
	let second = scene.add_node(at(Vec3::new(10.0, 0.0, 0.0))).unwrap();
	scene.set_parent(first, Some(second)).unwrap();
	assert!(near(scene.update()[first as usize].translation().x, 11.0), "the lower-indexed node is now the child");
	scene.set_translation(second, Vec3::new(100.0, 0.0, 0.0)).unwrap();
	assert!(near(scene.update()[first as usize].translation().x, 101.0));
}

#[test]
// A CYCLE IS REFUSED AT THE EDGE THAT WOULD CREATE IT. A traversal that discovered it would already
// be in an infinite loop.
fn a_parenting_that_would_close_a_loop_is_refused() {
	// @covers: HierarchyCycleRefusal
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let root = scene.add_node(Node::identity()).unwrap();
	let middle = scene.add_node(Node::identity().with_parent(root)).unwrap();
	let leaf = scene.add_node(Node::identity().with_parent(middle)).unwrap();
	assert_eq!(scene.set_parent(root, Some(leaf)), Err(Error::WouldCycle { node: root, parent: leaf }));
	assert_eq!(scene.set_parent(middle, Some(middle)), Err(Error::WouldCycle { node: middle, parent: middle }));
	assert_eq!(scene.nodes()[root as usize].parent(), None, "the refusal left the hierarchy alone");
	assert_eq!(scene.set_parent(leaf, Some(root)), Ok(()));
}

#[test]
// THE DEPTH IS CHECKED WHEN THE PARENT IS SET, so a traversal needs no depth counter and cannot
// overflow a stack. A re-parenting that deepens a whole subtree is refused for the same reason and
// LEAVES THE SCENE AS IT WAS.
fn a_hierarchy_deeper_than_the_limit_is_refused_at_the_edge() {
	// @covers: HierarchyDepthLimit
	let limits = Limits { max_hierarchy_depth: 4, ..Limits::PROFILE_MINIMUM };
	let mut scene = Scene::new(limits);
	let mut parent = scene.add_node(Node::identity()).unwrap();
	for _ in 0..3 {
		parent = scene.add_node(Node::identity().with_parent(parent)).unwrap();
	}
	assert_eq!(scene.nodes()[parent as usize].depth(), 3);
	assert!(matches!(scene.add_node(Node::identity().with_parent(parent)), Err(Error::TooDeep { ceiling: 4, asked: 5, .. })));

	// A subtree moved under a node that is already deep is refused, and the move is undone.
	let mut scene = Scene::new(limits);
	let deep = {
		let mut node = scene.add_node(Node::identity()).unwrap();
		for _ in 0..3 {
			node = scene.add_node(Node::identity().with_parent(node)).unwrap();
		}
		node
	};
	let group = scene.add_node(Node::identity()).unwrap();
	let under = scene.add_node(Node::identity().with_parent(group)).unwrap();
	assert!(matches!(scene.set_parent(group, Some(deep)), Err(Error::TooDeep { .. })));
	assert_eq!(scene.nodes()[group as usize].parent(), None, "the refused move was undone");
	assert_eq!(scene.nodes()[under as usize].depth(), 1, "and the subtree kept its depths");
}

#[test]
// DISABLING A NODE HIDES ITS WHOLE SUBTREE, and a mask composes by AND down the chain. If either did
// not, hiding a group would mean one edit per descendant.
fn an_ancestors_enablement_and_mask_reach_every_descendant() {
	// @covers: VisibilityMask, NodeEnable
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let root = scene.add_node(Node::identity().with_visibility(0b0011)).unwrap();
	let leaf = scene.add_node(Node::identity().with_visibility(0b0110).with_parent(root)).unwrap();
	assert_eq!(scene.effective_visibility(leaf), 0b0010, "the masks compose by AND, not by replacement");
	assert!(scene.effectively_enabled(leaf));

	scene.set_parent(leaf, None).unwrap();
	assert_eq!(scene.effective_visibility(leaf), 0b0110, "detached, it keeps only its own");

	scene.set_parent(leaf, Some(root)).unwrap();
	scene.set_enabled(root, false).unwrap();
	assert!(!scene.effectively_enabled(leaf), "an enabled child of a disabled parent is still hidden");
}

#[test]
// A LAYER WITHOUT A BOUND IS ONE WHOSE WORST CASE IS A CALLER'S LOOP, and every bound the profile
// names is a refusal rather than guidance.
fn the_limits_are_refusals_and_not_guidance() {
	// @covers: SceneLimits, LimitRefusal
	let limits = Limits { max_nodes: 2, max_lights: 1, max_drawables: 1, max_materials: 1, max_cameras: 1, max_instances_per_drawable: 2, ..Limits::PROFILE_MINIMUM };
	let mut scene = Scene::new(limits);
	let first = scene.add_node(Node::identity()).unwrap();
	scene.add_node(Node::identity()).unwrap();
	assert_eq!(scene.add_node(Node::identity()), Err(Error::LimitExceeded { limit: "max_nodes", ceiling: 2, asked: 3 }));

	let light = Light { node: first, kind: LightKind::Ambient, colour: Vec3::new(1.0, 1.0, 1.0), intensity: 1.0, visibility: u32::MAX };
	scene.add_light(light).unwrap();
	assert_eq!(scene.add_light(light), Err(Error::LimitExceeded { limit: "max_lights", ceiling: 1, asked: 2 }));

	let material = scene.add_material(opaque(1)).unwrap();
	assert_eq!(scene.add_material(opaque(2)), Err(Error::LimitExceeded { limit: "max_materials", ceiling: 1, asked: 2 }));

	let camera = Camera::perspective(first, core::f32::consts::FRAC_PI_2, 1.0, 1.0, 100.0, u32::MAX).unwrap();
	scene.add_camera(camera).unwrap();
	assert_eq!(scene.add_camera(camera), Err(Error::LimitExceeded { limit: "max_cameras", ceiling: 1, asked: 2 }));

	let instance = Instance { transform: Mat4::IDENTITY, colour: Vec4::new(1.0, 1.0, 1.0, 1.0) };
	let too_many = Drawable::new(first, 0, material).with_instances(vec![instance; 3]);
	assert_eq!(scene.add_drawable(too_many), Err(Error::LimitExceeded { limit: "max_instances_per_drawable", ceiling: 2, asked: 3 }));

	scene.add_drawable(Drawable::new(first, 0, material)).unwrap();
	assert_eq!(scene.add_drawable(Drawable::new(first, 0, material)), Err(Error::LimitExceeded { limit: "max_drawables", ceiling: 1, asked: 2 }));
}

#[test]
fn a_thing_that_does_not_exist_is_named_rather_than_ignored() {
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	assert_eq!(scene.add_node(Node::identity().with_parent(4)), Err(Error::NoSuchNode { node: 4 }));
	let node = scene.add_node(Node::identity()).unwrap();
	assert_eq!(scene.set_parent(node, Some(9)), Err(Error::NoSuchNode { node: 9 }));
	assert_eq!(scene.add_drawable(Drawable::new(node, 0, 7)), Err(Error::NoSuchMaterial { material: 7 }));
	let material = scene.add_material(opaque(1)).unwrap();
	assert_eq!(scene.add_drawable(Drawable::new(9, 0, material)), Err(Error::NoSuchNode { node: 9 }));
}

// ---------------------------------------------------------------------------------------------
// The camera.
// ---------------------------------------------------------------------------------------------

#[test]
// A CAMERA IS A NODE, and its view is the INVERSE of that node's world transform - which is what
// lets one be parented to a vehicle without a second mechanism.
fn a_cameras_view_is_the_inverse_of_the_node_it_hangs_on() {
	// @covers: PerspectiveCamera, ViewFromNodeTransform
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let rig = scene.add_node(at(Vec3::new(0.0, 0.0, 5.0))).unwrap();
	let camera = Camera::perspective(rig, core::f32::consts::FRAC_PI_2, 1.0, 1.0, 100.0, u32::MAX).unwrap();
	scene.update();
	let view = scene.view_of(&camera).unwrap();
	let seen = view.transform_point(Vec3::ZERO);
	assert!(near(seen.z, -5.0), "the view puts the origin five units down -Z: {}", seen.z);

	// A zero scale has no inverse, and it is named as arithmetic rather than as a missing node.
	let mut flat = Scene::new(Limits::PROFILE_MINIMUM);
	let crushed = flat.add_node(Node::identity().with_scale(Vec3::new(1.0, 1.0, 0.0))).unwrap();
	flat.update();
	let camera = Camera::perspective(crushed, core::f32::consts::FRAC_PI_2, 1.0, 1.0, 100.0, u32::MAX).unwrap();
	assert!(matches!(flat.view_of(&camera), Err(Error::Degenerate { .. })));
}

#[test]
// A DEGENERATE PROJECTION IS REFUSED AT THE CALL THAT SETS IT, because it produces a matrix of
// infinities and every vertex after it is a NaN with nothing left to say where they came from. AN
// INFINITE FAR PLANE IS NOT DEGENERATE: with a `[0, 1]` depth range it is well conditioned.
fn a_projection_that_cannot_be_one_is_refused_and_an_infinite_far_plane_is_not() {
	// @covers: InfiniteFarPlane, CustomProjection, DegenerateProjectionRefusal
	let fov = core::f32::consts::FRAC_PI_2;
	assert!(matches!(Camera::perspective(0, fov, 1.0, 0.0, 100.0, u32::MAX), Err(Error::Degenerate { .. })), "a zero near plane divides by zero");
	assert!(matches!(Camera::perspective(0, fov, 1.0, 10.0, 10.0, u32::MAX), Err(Error::Degenerate { .. })), "a far at near has no depth range");
	assert!(matches!(Camera::perspective(0, fov, 0.0, 1.0, 100.0, u32::MAX), Err(Error::Degenerate { .. })), "a zero aspect has no width");
	assert!(matches!(Camera::perspective(0, core::f32::consts::PI, 1.0, 1.0, 100.0, u32::MAX), Err(Error::Degenerate { .. })), "a half-turn field of view sees a plane");
	assert!(matches!(Camera::orthographic(0, 0.0, 1.0, 1.0, 100.0, u32::MAX), Err(Error::Degenerate { .. })));
	assert!(matches!(Camera::from_projection(0, Mat4::from_scale(Vec3::new(f32::NAN, 1.0, 1.0)), u32::MAX), Err(Error::Degenerate { .. })));

	let infinite = Camera::perspective(0, fov, 1.0, 0.5, f32::INFINITY, u32::MAX).unwrap();
	let very_far = infinite.projection().transform_point(Vec3::new(0.0, 0.0, -1.0e9)).perspective_divide().unwrap();
	assert!(very_far.z <= 1.0, "nothing ever leaves the volume through a far plane: {}", very_far.z);
}

// ---------------------------------------------------------------------------------------------
// Bounds.
// ---------------------------------------------------------------------------------------------

#[test]
// THE WORLD SPHERE TAKES THE LARGEST AXIS SCALE. The average would produce a sphere smaller than the
// geometry it claims to contain, which is a cull that removes something visible.
fn a_world_sphere_is_the_local_box_transformed_with_the_largest_scale() {
	// @covers: LocalBoundingBox, WorldBoundingSphere
	let local = cube();
	let transform = Mat4::from_translation(Vec3::new(0.0, 5.0, 0.0)).mul(&Mat4::from_scale(Vec3::new(3.0, 1.0, 1.0)));
	let sphere = local.world_sphere(&transform);
	assert!(near(sphere.centre.y, 5.0), "the centre follows the transform: {:?}", sphere.centre);
	// The unit cube's own radius is half its diagonal, and the largest scale is three.
	assert!(near(sphere.radius, cube().bounding_sphere().radius * 3.0), "the radius takes the largest scale: {}", sphere.radius);
}

#[test]
// A SPHERE IS THE PER-FRAME VOLUME BECAUSE RE-FITTING A BOX GROWS IT. Two eighth turns re-fitted
// give a box half again as large as one quarter turn; the sphere is the same either way.
fn a_refitted_box_grows_under_rotation_and_a_sphere_does_not() {
	// @covers: WorldBoundingSphere
	let square = Aabb::new(Vec3::new(-1.0, -1.0, -1.0), Vec3::new(1.0, 1.0, 1.0));
	let eighth = Quat::from_axis_angle(Vec3::new(0.0, 0.0, 1.0), core::f32::consts::FRAC_PI_4).unwrap().to_mat4();
	let once = square.transformed(&eighth);
	assert!(near(once.maximum.x, core::f32::consts::SQRT_2), "an eighth turn widens the box to its diagonal: {}", once.maximum.x);
	let twice = once.transformed(&eighth);
	assert!(twice.maximum.x > once.maximum.x + 0.5, "re-fitting again grows it further: {} then {}", once.maximum.x, twice.maximum.x);

	let sphere = square.world_sphere(&eighth);
	let again = square.world_sphere(&eighth.mul(&eighth));
	assert!(near(sphere.radius, again.radius), "a sphere does not grow under rotation: {} then {}", sphere.radius, again.radius);
}

#[test]
fn a_union_contains_both_and_a_contained_volume_changes_nothing() {
	let large = Aabb::new(Vec3::new(-10.0, -10.0, -10.0), Vec3::new(10.0, 10.0, 10.0));
	let small = cube();
	assert_eq!(large.union(&small), large);
	let inner = Sphere::new(Vec3::ZERO, 1.0);
	let outer = Sphere::new(Vec3::new(0.5, 0.0, 0.0), 5.0);
	assert_eq!(outer.union(&inner), outer, "a sphere that contains the other is the union");
	let far = Sphere::new(Vec3::new(10.0, 0.0, 0.0), 1.0);
	assert!(near(inner.union(&far).radius, 6.0));
}

#[test]
// THE NORMAL MATRIX IS THE INVERSE TRANSPOSE, and the case that shows it is a non-uniform scale:
// under a squash, transforming a normal by the position's matrix tilts it the wrong way.
fn a_normal_survives_a_non_uniform_scale_and_a_position_does_not() {
	// @covers: NormalMatrix
	// A 45-degree normal on a surface squashed to a tenth in Y.
	let squash = Mat4::from_scale(Vec3::new(1.0, 0.1, 1.0));
	let normal = Vec3::new(1.0, 1.0, 0.0).normalise().unwrap();
	let wrong = squash.transform_direction(normal).normalise().unwrap();
	let right = normal_matrix(&squash).mul_vector(normal).normalise().unwrap();
	// Squashing in Y makes the surface FLATTER, so its normal turns TOWARDS +Y, not away from it.
	assert!(right.y > right.x, "the correct normal leans towards the squashed axis: {right:?}");
	assert!(wrong.y < wrong.x, "and the position's matrix leans it the other way: {wrong:?}");
	// Under a uniform scale the two agree, which is why the mistake survives most scenes.
	let uniform = Mat4::from_scale(Vec3::new(3.0, 3.0, 3.0));
	let plain = uniform.transform_direction(normal).normalise().unwrap();
	let cofactor = normal_matrix(&uniform).mul_vector(normal).normalise().unwrap();
	assert!(near(plain.x, cofactor.x) && near(plain.y, cofactor.y));
}

// ---------------------------------------------------------------------------------------------
// Culling.
// ---------------------------------------------------------------------------------------------

/// A 90-degree square frustum at the origin looking down -Z, near 1, far 100.
fn known_frustum() -> (Mat4, Frustum) {
	let projection = perspective_rh_zo(core::f32::consts::FRAC_PI_2, 1.0, 1.0, 100.0).unwrap();
	let view = look_at_rh(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), Vec3::new(0.0, 1.0, 0.0)).unwrap();
	let view_projection = projection.mul(&view);
	(view_projection, Frustum::from_view_projection(&view_projection))
}

#[test]
// THE FRUSTUM IS THE ONE THE PROJECTION DESCRIBES, and every case is worked out from the 90-degree
// half-angle rather than from what the extraction happens to produce.
fn culling_answers_a_frustum_whose_planes_are_known_by_hand() {
	// @covers: FrustumPlaneExtraction, SphereFrustumTest
	let (_, frustum) = known_frustum();
	assert_eq!(frustum.test_sphere(&Sphere::new(Vec3::new(0.0, 0.0, -10.0), 1.0)), Visibility::Inside);
	assert_eq!(frustum.test_sphere(&Sphere::new(Vec3::new(0.0, 0.0, 10.0), 1.0)), Visibility::Outside, "behind the eye");
	assert_eq!(frustum.test_sphere(&Sphere::new(Vec3::new(0.0, 0.0, -0.5), 0.25)), Visibility::Outside, "nearer than the near plane");
	assert_eq!(frustum.test_sphere(&Sphere::new(Vec3::new(0.0, 0.0, -1.0), 0.5)), Visibility::Intersecting, "straddling it");
	assert_eq!(frustum.test_sphere(&Sphere::new(Vec3::new(0.0, 0.0, -200.0), 1.0)), Visibility::Outside, "past the far plane");
	// At 90 degrees the half-width equals the depth.
	assert_eq!(frustum.test_sphere(&Sphere::new(Vec3::new(20.0, 0.0, -10.0), 1.0)), Visibility::Outside);
	assert_eq!(frustum.test_sphere(&Sphere::new(Vec3::new(10.0, 0.0, -10.0), 1.0)), Visibility::Intersecting, "on the right plane");
}

#[test]
// THE PLANE ORDER IS NEAR, FAR, LEFT, RIGHT, BOTTOM, TOP, and it is observable: a volume outside two
// planes is rejected by whichever is tested first, so two implementations that disagree about the
// order name different planes in their reports.
fn the_first_plane_a_volume_is_outside_of_is_the_one_named() {
	// @covers: FixedPlaneTestOrder
	let (_, frustum) = known_frustum();
	assert_eq!(frustum.rejected_by(&Sphere::new(Vec3::new(0.0, 0.0, 10.0), 1.0)), Err(Side::Near));
	assert_eq!(frustum.rejected_by(&Sphere::new(Vec3::new(0.0, 0.0, -500.0), 1.0)), Err(Side::Far));
	assert_eq!(frustum.rejected_by(&Sphere::new(Vec3::new(-40.0, 0.0, -10.0), 1.0)), Err(Side::Left));
	assert_eq!(frustum.rejected_by(&Sphere::new(Vec3::new(40.0, 0.0, -10.0), 1.0)), Err(Side::Right));
	assert_eq!(frustum.rejected_by(&Sphere::new(Vec3::new(0.0, -40.0, -10.0), 1.0)), Err(Side::Bottom));
	assert_eq!(frustum.rejected_by(&Sphere::new(Vec3::new(0.0, 40.0, -10.0), 1.0)), Err(Side::Top));
	// Outside BOTH the near plane and the right one: near is tested first and names itself.
	assert_eq!(frustum.rejected_by(&Sphere::new(Vec3::new(40.0, 0.0, 10.0), 1.0)), Err(Side::Near));
}

#[test]
// THE BOX TEST IS THE p/n-VERTEX TEST, and the case that separates it from a sphere test is a long
// thin box: its bounding sphere leaves the frustum while the box does not.
fn a_box_is_tested_by_its_corners_and_not_by_its_sphere() {
	// @covers: BoxFrustumTest
	let (_, frustum) = known_frustum();
	let slab = Aabb::new(Vec3::new(-1.0, -1.0, -50.0), Vec3::new(1.0, 1.0, -2.0));
	assert_eq!(frustum.test_aabb(&slab), Visibility::Inside);
	assert_eq!(frustum.test_sphere(&slab.bounding_sphere()), Visibility::Intersecting, "its sphere reaches outside");
	assert_eq!(frustum.test_aabb(&Aabb::new(Vec3::new(100.0, 100.0, -10.0), Vec3::new(101.0, 101.0, -9.0))), Visibility::Outside);
	assert_eq!(frustum.test_aabb(&Aabb::new(Vec3::new(-1.0, -1.0, -3.0), Vec3::new(1.0, 1.0, 1.0))), Visibility::Intersecting);
}

// ---------------------------------------------------------------------------------------------
// Queues.
// ---------------------------------------------------------------------------------------------

#[test]
// OPAQUE FRONT TO BACK so the depth test rejects early; TRANSPARENT BACK TO FRONT because blending
// is not commutative. ALPHA-MASK IS ITS OWN QUEUE because a discarding fragment stage cannot take
// the early depth path, and running it after the opaque one means fewer of its fragments survive.
fn the_three_queues_are_sorted_in_the_directions_their_reasons_require() {
	// @covers: OpaqueQueue, AlphaMaskQueue, TransparentQueue, FixedQueueOrder
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let plain = scene.add_material(opaque(1)).unwrap();
	let masked = scene.add_material(opaque(2).with_blending(Blending::AlphaMask { threshold: 0.5 })).unwrap();
	let glass = scene.add_material(blended(3)).unwrap();
	for depth in [5.0_f32, 10.0, 20.0] {
		let node = scene.add_node(at(Vec3::new(0.0, 0.0, -depth))).unwrap();
		for material in [plain, masked, glass] {
			scene.add_drawable(Drawable::new(node, 0, material).with_bounds(cube())).unwrap();
		}
	}
	let (_, frustum) = known_frustum();
	let queue = crate::queue::build_with(&mut scene, Vec3::ZERO, &frustum, u32::MAX);

	let depths: Vec<f32> = queue.opaque.iter().map(|entry| entry.depth).collect();
	assert!(depths.windows(2).all(|pair| pair[0] <= pair[1]), "opaque is front to back: {depths:?}");
	let depths: Vec<f32> = queue.alpha_mask.iter().map(|entry| entry.depth).collect();
	assert!(depths.windows(2).all(|pair| pair[0] <= pair[1]), "alpha-mask is front to back too: {depths:?}");
	let depths: Vec<f32> = queue.transparent.iter().map(|entry| entry.depth).collect();
	assert!(depths.windows(2).all(|pair| pair[0] >= pair[1]), "transparent is back to front: {depths:?}");
	assert_eq!((queue.opaque.len(), queue.alpha_mask.len(), queue.transparent.len(), queue.culled), (3, 3, 3, 0));
}

#[test]
// THE SORT KEY IS THE DISTANCE TO THE SPHERE'S CENTRE AND NOT TO ITS NEAREST POINT, because the
// nearest point makes a large object sort ahead of a small one it contains.
fn a_large_object_does_not_sort_ahead_of_what_it_contains() {
	// @covers: DistanceSortKey
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let material = scene.add_material(opaque(1)).unwrap();
	let node = scene.add_node(at(Vec3::new(0.0, 0.0, -20.0))).unwrap();
	let enclosing = scene.add_drawable(Drawable::new(node, 0, material).with_bounds(Aabb::new(Vec3::new(-8.0, -8.0, -8.0), Vec3::new(8.0, 8.0, 8.0)))).unwrap();
	let inner = scene.add_drawable(Drawable::new(node, 1, material).with_bounds(cube())).unwrap();
	let (_, frustum) = known_frustum();
	let queue = crate::queue::build_with(&mut scene, Vec3::ZERO, &frustum, u32::MAX);
	// Both centres are at the same place, so submission order breaks the tie and the large one does
	// NOT come first by virtue of reaching closer.
	assert_eq!(queue.opaque.iter().map(|entry| entry.drawable).collect::<Vec<_>>(), vec![enclosing, inner]);
	assert!(near(queue.opaque[0].depth, queue.opaque[1].depth), "one key, one distance");
}

#[test]
// THE TIEBREAK IS SUBMISSION ORDER AND IT RUNS THE SAME WAY IN EVERY QUEUE. If the transparent queue
// reversed the tiebreak with the distance, two objects at one distance would swap between queues and
// a frame would differ from itself.
fn objects_at_one_distance_keep_a_stable_order_in_both_directions() {
	// @covers: StableTiebreak
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let plain = scene.add_material(opaque(1)).unwrap();
	let glass = scene.add_material(blended(2)).unwrap();
	for _ in 0..4 {
		let node = scene.add_node(at(Vec3::new(0.0, 0.0, -10.0))).unwrap();
		scene.add_drawable(Drawable::new(node, 0, plain).with_bounds(cube())).unwrap();
		scene.add_drawable(Drawable::new(node, 0, glass).with_bounds(cube())).unwrap();
	}
	let (_, frustum) = known_frustum();
	let queue = crate::queue::build_with(&mut scene, Vec3::ZERO, &frustum, u32::MAX);
	assert_eq!(queue.opaque.iter().map(|entry| entry.drawable).collect::<Vec<_>>(), vec![0, 2, 4, 6]);
	assert_eq!(queue.transparent.iter().map(|entry| entry.drawable).collect::<Vec<_>>(), vec![1, 3, 5, 7], "the tiebreak runs the same way in the reversed queue");
}

#[test]
// A DRAWABLE WITH NO BOUNDS IS NEVER CULLED. An unbounded drawable is one the scene cannot reason
// about, and dropping it would make it disappear for a reason nobody can see.
fn an_unbounded_drawable_is_drawn_wherever_it_is() {
	// @covers: UnboundedNeverCulled
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let material = scene.add_material(opaque(1)).unwrap();
	let behind = scene.add_node(at(Vec3::new(0.0, 0.0, 500.0))).unwrap();
	scene.add_drawable(Drawable::new(behind, 0, material)).unwrap();
	scene.add_drawable(Drawable::new(behind, 1, material).with_bounds(cube())).unwrap();
	let (_, frustum) = known_frustum();
	let queue = crate::queue::build_with(&mut scene, Vec3::ZERO, &frustum, u32::MAX);
	assert_eq!(queue.opaque.len(), 1, "the bounded one was culled and the unbounded one was not");
	assert_eq!(queue.opaque[0].drawable, 0);
	assert_eq!(queue.culled, 1);
}

#[test]
// CULLING IS PER INSTANCE, WITH THE SURVIVORS COMPACTED IN THEIR ORIGINAL ORDER. Culling the whole
// set because one instance is outside would draw a city because one building is visible.
fn instances_are_culled_one_at_a_time_and_compacted_in_order() {
	// @covers: InstanceStream, PerInstanceCulling, InstanceCompaction, SingleSortPerInstanceSet
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let material = scene.add_material(opaque(1)).unwrap();
	let node = scene.add_node(Node::identity()).unwrap();
	let places = [Vec3::new(0.0, 0.0, -10.0), Vec3::new(0.0, 0.0, 500.0), Vec3::new(1.0, 0.0, -12.0)];
	let instances: Vec<Instance> = places.iter().map(|place| Instance { transform: Mat4::from_translation(*place), colour: Vec4::new(1.0, 1.0, 1.0, 1.0) }).collect();
	scene.add_drawable(Drawable::new(node, 0, material).with_bounds(cube()).with_instances(instances)).unwrap();
	let (_, frustum) = known_frustum();
	let queue = crate::queue::build_with(&mut scene, Vec3::ZERO, &frustum, u32::MAX);
	assert_eq!(queue.opaque.len(), 1);
	assert_eq!(queue.opaque[0].instances, vec![0, 2], "the survivors keep their original order");
	assert_eq!(queue.opaque[0].instance_count(), 2);
	assert_eq!(queue.culled_instances, 1);
	assert_eq!(queue.culled, 0, "the drawable itself was not culled");

	// AN INSTANCED DRAWABLE SORTS ONCE, BY THE SPHERE OF THE WHOLE SET - which includes the instance
	// that was culled, so the key does not change as instances leave the frustum.
	assert!(queue.opaque[0].depth > 100.0, "the whole set's centre is pulled towards the distant instance: {}", queue.opaque[0].depth);
}

#[test]
// A MASK SAYS WHICH VIEWS SEE A THING, so a reflection and a main pass draw different subsets of one
// scene without a second scene. AND A MASK IS NOT A CULL: nothing it excludes is counted as culled,
// because a caller tuning its bounds would read that number as geometry the frustum rejected.
fn a_view_draws_only_what_its_mask_admits() {
	// @covers: VisibilityMask
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let material = scene.add_material(opaque(1)).unwrap();
	let node = scene.add_node(at(Vec3::new(0.0, 0.0, -10.0))).unwrap();
	scene.add_drawable(Drawable::new(node, 0, material).with_bounds(cube()).with_visibility(0b01)).unwrap();
	scene.add_drawable(Drawable::new(node, 1, material).with_bounds(cube()).with_visibility(0b10)).unwrap();
	let (_, frustum) = known_frustum();
	assert_eq!(crate::queue::build_with(&mut scene, Vec3::ZERO, &frustum, 0b01).opaque.len(), 1);
	assert_eq!(crate::queue::build_with(&mut scene, Vec3::ZERO, &frustum, 0b11).opaque.len(), 2);
	let none = crate::queue::build_with(&mut scene, Vec3::ZERO, &frustum, 0b100);
	assert_eq!(none.opaque.len(), 0);
	assert_eq!(none.culled, 0, "a mask is not a cull");
}

#[test]
fn a_disabled_subtree_reaches_no_queue() {
	// @covers: NodeEnable
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let material = scene.add_material(opaque(1)).unwrap();
	let group = scene.add_node(Node::identity().disabled()).unwrap();
	let node = scene.add_node(at(Vec3::new(0.0, 0.0, -10.0)).with_parent(group)).unwrap();
	scene.add_drawable(Drawable::new(node, 0, material).with_bounds(cube())).unwrap();
	let (_, frustum) = known_frustum();
	assert!(crate::queue::build_with(&mut scene, Vec3::ZERO, &frustum, u32::MAX).opaque.is_empty());
}

#[test]
// BUILDING FROM A CAMERA TAKES THE VIEW, THE PROJECTION, THE FRUSTUM AND THE MASK FROM ONE PLACE, so
// a frustum built from one camera cannot be used with another's eye position.
fn a_queue_built_from_a_camera_culls_against_that_cameras_own_frustum() {
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let material = scene.add_material(opaque(1)).unwrap();
	// The camera looks down -Z from ten units back, so an object at the origin is in front of it.
	let rig = scene.add_node(at(Vec3::new(0.0, 0.0, 10.0))).unwrap();
	let camera = Camera::perspective(rig, core::f32::consts::FRAC_PI_2, 1.0, 1.0, 100.0, u32::MAX).unwrap();
	let seen = scene.add_node(Node::identity()).unwrap();
	let behind = scene.add_node(at(Vec3::new(0.0, 0.0, 40.0))).unwrap();
	scene.add_drawable(Drawable::new(seen, 0, material).with_bounds(cube())).unwrap();
	scene.add_drawable(Drawable::new(behind, 1, material).with_bounds(cube())).unwrap();
	let queue = crate::queue::build(&mut scene, &camera).unwrap();
	assert_eq!(queue.opaque.len(), 1);
	assert_eq!(queue.opaque[0].drawable, 0);
	assert!(near(queue.opaque[0].depth, 10.0), "the distance is from the camera's own position: {}", queue.opaque[0].depth);
}

// ---------------------------------------------------------------------------------------------
// Lighting.
// ---------------------------------------------------------------------------------------------

#[test]
// THE FALL-OFF REACHES EXACTLY ZERO AT THE RANGE. A plain inverse square clamped at the range leaves
// a visible edge; the multiplicative window is what removes it.
fn a_point_lights_fall_off_is_the_profiles_equation_and_ends_at_its_range() {
	// @covers: PointAttenuation, DirectionalNoAttenuation
	let kind = LightKind::Point { radius: 1.0, range: 10.0 };
	assert!(near(crate::light::attenuation(&kind, 0.0), 1.0), "one at the source");
	// At one radius: 1 / (1 + 1) times (1 - (0.1)^4)^2.
	let expected = 0.5 * (1.0 - 0.0001_f32).powi(2);
	assert!(near(crate::light::attenuation(&kind, 1.0), expected), "{}", crate::light::attenuation(&kind, 1.0));
	assert_eq!(crate::light::attenuation(&kind, 10.0), 0.0, "exactly zero at the range, not nearly");
	assert_eq!(crate::light::attenuation(&kind, 20.0), 0.0);
	// A DIRECTIONAL LIGHT DOES NOT ATTENUATE: it is infinitely far away.
	assert_eq!(crate::light::attenuation(&LightKind::Directional { direction: Vec3::new(0.0, -1.0, 0.0) }, 1000.0), 1.0);
}

#[test]
// AN INNER ANGLE EQUAL TO THE OUTER IS A HARD EDGE, not a division by zero - which is what a plain
// `smoothstep` gives and what shows as a cone that is entirely black.
fn a_spot_cone_is_soft_between_its_angles_and_hard_when_they_coincide() {
	// @covers: LightSpot, SpotConeAttenuation
	let soft = LightKind::Spot { direction: Vec3::new(0.0, 0.0, -1.0), radius: 1.0, range: 100.0, inner: 0.2, outer: 0.6 };
	// The light is five units up the +Z axis and the surface is at the origin, so the surface is
	// exactly on the cone's axis.
	let on_axis = Vec3::new(0.0, 0.0, 5.0);
	assert!(near(crate::light::cone(&soft, on_axis), 1.0), "the axis is fully lit");
	// A surface far off the axis is outside the outer angle.
	assert!(near(crate::light::cone(&soft, Vec3::new(-20.0, 0.0, 5.0)), 0.0));
	// And one between the angles is partly lit.
	let between = crate::light::cone(&soft, Vec3::new(-2.0, 0.0, 5.0));
	assert!(between > 0.0 && between < 1.0, "the edge is soft: {between}");

	let hard = LightKind::Spot { direction: Vec3::new(0.0, 0.0, -1.0), radius: 1.0, range: 100.0, inner: 0.4, outer: 0.4 };
	assert!(near(crate::light::cone(&hard, on_axis), 1.0));
	assert!(near(crate::light::cone(&hard, Vec3::new(-20.0, 0.0, 5.0)), 0.0), "a coincident pair is a step and not a NaN");
}

#[test]
// THE ACCUMULATION ORDER IS AMBIENT, DIRECTIONAL, THEN POINT AND SPOT BY DESCENDING IRRADIANCE.
// Floating-point addition is not associative, so two implementations that accumulate in different
// orders produce different colours - and the difference is invisible until a conformance comparison
// fails for a reason nobody can find.
fn the_lights_that_reach_a_drawable_come_back_in_the_accumulation_order() {
	// @covers: LightAmbient, LightDirectional, LightPoint, LightSelectionOrder, LightsPerDrawableLimit
	let limits = Limits { max_lights_per_drawable: 3, ..Limits::PROFILE_MINIMUM };
	let mut scene = Scene::new(limits);
	let white = Vec3::new(1.0, 1.0, 1.0);
	let origin = scene.add_node(Node::identity()).unwrap();
	// Added in the WRONG order on purpose: a dim point light first, then a bright one, then the sun,
	// then the ambient term.
	let dim = scene.add_node(at(Vec3::new(0.0, 0.0, 8.0))).unwrap();
	let bright = scene.add_node(at(Vec3::new(0.0, 0.0, 2.0))).unwrap();
	scene.add_light(Light { node: dim, kind: LightKind::Point { radius: 1.0, range: 50.0 }, colour: white, intensity: 1.0, visibility: u32::MAX }).unwrap();
	scene.add_light(Light { node: bright, kind: LightKind::Point { radius: 1.0, range: 50.0 }, colour: white, intensity: 1.0, visibility: u32::MAX }).unwrap();
	scene.add_light(Light { node: origin, kind: LightKind::Directional { direction: Vec3::new(0.0, -1.0, 0.0) }, colour: white, intensity: 1.0, visibility: u32::MAX }).unwrap();
	scene.add_light(Light { node: origin, kind: LightKind::Ambient, colour: white, intensity: 1.0, visibility: u32::MAX }).unwrap();
	scene.update();
	let selected = crate::light::select(&scene, &Sphere::new(Vec3::ZERO, 1.0), u32::MAX);
	assert_eq!(selected, vec![3, 2, 1], "ambient, then the sun, then the nearer point light");

	// TRUNCATION IS AT THE END, so what is dropped is what matters least. The dim light is the one
	// that falls off the list of three.
	assert!(!selected.contains(&0), "the dimmest light is the one dropped: {selected:?}");
	// A light out of range does not reach the drawable at all.
	let far = scene.add_node(at(Vec3::new(0.0, 0.0, 1000.0))).unwrap();
	scene.add_light(Light { node: far, kind: LightKind::Point { radius: 1.0, range: 5.0 }, colour: white, intensity: 100.0, visibility: u32::MAX }).unwrap();
	scene.update();
	assert!(!crate::light::select(&scene, &Sphere::new(Vec3::ZERO, 1.0), u32::MAX).contains(&4));
}

#[test]
fn a_light_that_cannot_be_a_light_is_refused() {
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let node = scene.add_node(Node::identity()).unwrap();
	let white = Vec3::new(1.0, 1.0, 1.0);
	let sound = Light { node, kind: LightKind::Ambient, colour: white, intensity: 1.0, visibility: u32::MAX };
	assert!(matches!(scene.add_light(Light { intensity: -1.0, ..sound }), Err(Error::Degenerate { .. })));
	assert!(matches!(scene.add_light(Light { kind: LightKind::Point { radius: 0.0, range: 10.0 }, ..sound }), Err(Error::Degenerate { .. })), "a zero radius divides by zero");
	assert!(matches!(scene.add_light(Light { kind: LightKind::Directional { direction: Vec3::ZERO }, ..sound }), Err(Error::Degenerate { .. })));
	let cone = LightKind::Spot { direction: Vec3::new(0.0, 0.0, -1.0), radius: 1.0, range: 10.0, inner: 1.0, outer: 0.5 };
	assert!(matches!(scene.add_light(Light { kind: cone, ..sound }), Err(Error::Degenerate { .. })), "an inner angle past the outer");
}

// ---------------------------------------------------------------------------------------------
// Materials.
// ---------------------------------------------------------------------------------------------

#[test]
// THE FOUR EQUATIONS, EACH AGAINST A VALUE WORKED OUT BY HAND.
fn each_core_material_computes_the_equation_the_profile_states() {
	// @covers: MaterialUnlit, MaterialVertexColor, MaterialLambert, MaterialBlinnPhong
	let surface = Surface::new(Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0));
	let eye = Vec3::new(0.0, 0.0, 1.0);
	let lit = Incident { to_light: Vec3::new(0.0, 0.0, 1.0), radiance: Vec3::new(1.0, 1.0, 1.0) };

	// Unlit: base * texture, with no light applied at all.
	let unlit = Material::new(MaterialKind::Unlit, GraphicsPipeline(1), 1).with_base_colour(Vec4::new(0.5, 0.25, 0.125, 1.0));
	let colour = crate::material::shade(&unlit, &surface.with_texture(Vec4::new(1.0, 1.0, 1.0, 1.0)), eye, Vec3::new(9.0, 9.0, 9.0), &[lit]).unwrap();
	assert!(near(colour.x, 0.5) && near(colour.y, 0.25) && near(colour.z, 0.125), "unlit ignores every light: {colour:?}");

	// VertexColor: the vertex colour times the texture, also unlit.
	let vertex = Material::new(MaterialKind::VertexColor, GraphicsPipeline(1), 1);
	let surface_with = surface.with_vertex_colour(Vec4::new(0.2, 0.4, 0.6, 1.0)).with_texture(Vec4::new(0.5, 0.5, 0.5, 1.0));
	let colour = crate::material::shade(&vertex, &surface_with, eye, Vec3::ZERO, &[lit]).unwrap();
	assert!(near(colour.x, 0.1) && near(colour.y, 0.2) && near(colour.z, 0.3), "{colour:?}");

	// Lambert: base * (ambient + light * max(dot(N, L), 0)).
	let lambert = Material::new(MaterialKind::Lambert, GraphicsPipeline(1), 1);
	let half_lit = Incident { to_light: Vec3::new(0.0, 0.0, 1.0), radiance: Vec3::new(0.5, 0.5, 0.5) };
	let colour = crate::material::shade(&lambert, &surface, eye, Vec3::new(0.1, 0.1, 0.1), &[half_lit]).unwrap();
	assert!(near(colour.x, 0.6), "a head-on light plus the ambient term: {colour:?}");
	// A light behind the surface contributes nothing rather than a negative.
	let behind = Incident { to_light: Vec3::new(0.0, 0.0, -1.0), radiance: Vec3::new(1.0, 1.0, 1.0) };
	let colour = crate::material::shade(&lambert, &surface, eye, Vec3::new(0.1, 0.1, 0.1), &[behind]).unwrap();
	assert!(near(colour.x, 0.1), "the ambient term alone: {colour:?}");

	// Blinn-Phong: the same diffuse, plus specular about the HALF vector, with the specular carrying
	// its OWN colour rather than multiplying the base. Head on, H is N, so the highlight is at its
	// maximum whatever the exponent: base a half, plus a specular quarter.
	let phong = Material::new(MaterialKind::BlinnPhong, GraphicsPipeline(1), 1).with_base_colour(Vec4::new(0.5, 0.5, 0.5, 1.0)).with_specular(Vec3::new(0.25, 0.25, 0.25), 32);
	let colour = crate::material::shade(&phong, &surface, eye, Vec3::ZERO, &[lit]).unwrap();
	assert!(near(colour.x, 0.75), "diffuse a half plus specular a quarter: {colour:?}");
	// At a grazing angle the half vector is well away from the normal and the highlight is gone.
	let grazing = Incident { to_light: Vec3::new(1.0, 0.0, 0.05).normalise().unwrap(), radiance: Vec3::new(1.0, 1.0, 1.0) };
	let colour = crate::material::shade(&phong, &surface, eye, Vec3::ZERO, &[grazing]).unwrap();
	assert!(colour.x < 0.05, "a grazing light leaves almost nothing: {colour:?}");
}

#[test]
// THE RESULT IS CLAMPED IN LINEAR LIGHT, BEFORE THE TRANSFER FUNCTION, because the core profile has
// no high dynamic range for a value above one to go to. AND LIGHTING NEVER CHANGES ALPHA: coverage
// is not brightness, and a lit surface that became more opaque where the light fell would composite
// differently from the same surface in shadow.
fn a_lit_colour_is_clamped_before_the_transfer_function_and_the_alpha_is_left_alone() {
	let surface = Surface::new(Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0));
	let lambert = Material::new(MaterialKind::Lambert, GraphicsPipeline(1), 1).with_base_colour(Vec4::new(1.0, 1.0, 1.0, 0.25));
	let fierce = Incident { to_light: Vec3::new(0.0, 0.0, 1.0), radiance: Vec3::new(40.0, 40.0, 40.0) };
	let colour = crate::material::shade(&lambert, &surface, Vec3::new(0.0, 0.0, 1.0), Vec3::new(9.0, 9.0, 9.0), &[fierce]).unwrap();
	assert_eq!((colour.x, colour.y, colour.z), (1.0, 1.0, 1.0), "clamped to one, not left at forty-nine");
	assert!(near(colour.w, 0.25), "the alpha is the albedo's: {}", colour.w);
}

#[test]
// THE BLEND STATE AND THE BLENDING MUST AGREE, and the specular exponent is a LOOP COUNT with a
// ceiling: an unbounded one is a frame that never ends.
fn a_material_whose_state_contradicts_itself_is_refused() {
	// @covers: PerDrawBlendState, PerDrawColorWriteMask
	let base = Material::new(MaterialKind::BlinnPhong, GraphicsPipeline(1), 1);
	assert_eq!(base.validate(), Ok(()));
	assert_eq!(base.with_blending(Blending::Blended).validate(), Ok(()), "the blending sets the state that blending implies");

	let lying = Material { blending: Blending::Blended, ..base };
	assert!(matches!(lying.validate(), Err(Error::Degenerate { .. })), "transparent, with a pipeline that does not blend");
	let also_lying = Material { blend: render3d::AttachmentBlend { enabled: true, ..base.blend }, ..base };
	assert!(matches!(also_lying.validate(), Err(Error::Degenerate { .. })), "opaque, with a pipeline that does");

	// THE COLOUR WRITE MASK IS PER DRAW and travels with the blend state, which is where `render3d`
	// carries it: a depth-only prepass is a material that writes no channels.
	let prepass = Material { blend: render3d::AttachmentBlend { write_mask: render3d::ColorWriteMask::NONE, ..base.blend }, ..base };
	assert_eq!(prepass.validate(), Ok(()));
	assert!(prepass.blend.write_mask.writes_nothing());
	assert!(base.blend.write_mask == render3d::ColorWriteMask::ALL);

	assert!(matches!(base.with_specular(Vec3::new(1.0, 1.0, 1.0), 0).validate(), Err(Error::Degenerate { .. })));
	assert!(matches!(base.with_specular(Vec3::new(1.0, 1.0, 1.0), crate::material::MAX_SHININESS + 1).validate(), Err(Error::Degenerate { .. })));
	assert_eq!(base.with_specular(Vec3::new(1.0, 1.0, 1.0), crate::material::MAX_SHININESS).validate(), Ok(()));
}

#[test]
// A FRAGMENT WITH ALPHA STRICTLY BELOW THE THRESHOLD IS DISCARDED, so a threshold of zero discards
// nothing - which is what makes "no threshold" and "a threshold of zero" the same material.
fn the_alpha_threshold_discards_strictly_below_and_a_zero_threshold_discards_nothing() {
	// @covers: AlphaThresholdDiscard
	let surface = Surface::new(Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0));
	let base = Material::new(MaterialKind::Unlit, GraphicsPipeline(1), 1);
	let half = base.with_blending(Blending::AlphaMask { threshold: 0.5 });

	let just_under = half.with_base_colour(Vec4::new(1.0, 1.0, 1.0, 0.499));
	assert!(crate::material::shade(&just_under, &surface, Vec3::ZERO, Vec3::ZERO, &[]).is_none());
	let exactly = half.with_base_colour(Vec4::new(1.0, 1.0, 1.0, 0.5));
	assert!(crate::material::shade(&exactly, &surface, Vec3::ZERO, Vec3::ZERO, &[]).is_some(), "at the threshold it survives");

	let none = base.with_blending(Blending::AlphaMask { threshold: 0.0 }).with_base_colour(Vec4::new(1.0, 1.0, 1.0, 0.0));
	assert!(crate::material::shade(&none, &surface, Vec3::ZERO, Vec3::ZERO, &[]).is_some(), "a threshold of zero discards nothing");

	// A threshold outside zero to one is refused rather than clamped.
	assert!(matches!(base.with_blending(Blending::AlphaMask { threshold: 2.0 }).validate(), Err(Error::Degenerate { .. })));
}

#[test]
// THE NORMAL IS RENORMALISED, because interpolating unit vectors does not produce unit vectors and
// the error is largest in the middle of a large triangle. AND IT IS FLIPPED FOR A BACK-FACING
// FRAGMENT OF A TWO-SIDED MATERIAL: without that, the lit side of a leaf is the side away from the
// light.
fn an_interpolated_normal_is_renormalised_and_a_two_sided_surface_flips_it() {
	// @covers: NormalRenormalisation, TwoSidedNormalFlip
	let eye = Vec3::new(0.0, 0.0, 1.0);
	let lambert = Material::new(MaterialKind::Lambert, GraphicsPipeline(1), 1);
	let lit = Incident { to_light: Vec3::new(0.0, 0.0, 1.0), radiance: Vec3::new(1.0, 1.0, 1.0) };

	let unit = Surface::new(Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0));
	let long = Surface::new(Vec3::ZERO, Vec3::new(0.0, 0.0, 7.0));
	let first = crate::material::shade(&lambert, &unit, eye, Vec3::ZERO, &[lit]).unwrap();
	let second = crate::material::shade(&lambert, &long, eye, Vec3::ZERO, &[lit]).unwrap();
	assert!(near(first.x, second.x), "the length of the interpolated normal does not change the shading");

	let back = unit.back_facing();
	let one_sided = crate::material::shade(&lambert, &back, eye, Vec3::ZERO, &[lit]).unwrap();
	let two_sided = crate::material::shade(&lambert.two_sided(), &back, eye, Vec3::ZERO, &[lit]).unwrap();
	assert!(near(one_sided.x, 1.0), "a one-sided material does not flip: {one_sided:?}");
	assert!(near(two_sided.x, 0.0), "a two-sided one flips away from this light: {two_sided:?}");
}

#[test]
// THE MATERIAL DECIDES THE QUEUE, so a node's queue changes when its material does and the two can
// never disagree the way a queue flag on a node can.
fn the_queue_and_the_depth_write_follow_the_materials_blending() {
	// @covers: QueueFromBlending
	let base = Material::new(MaterialKind::Unlit, GraphicsPipeline(1), 1);
	assert_eq!(base.queue(), QueueKind::Opaque);
	assert_eq!(base.with_blending(Blending::AlphaMask { threshold: 0.5 }).queue(), QueueKind::AlphaMask);
	assert_eq!(base.with_blending(Blending::Blended).queue(), QueueKind::Transparent);
	assert!(base.writes_depth() && base.writes_id());
	let glass = base.with_blending(Blending::Blended);
	assert!(!glass.writes_depth(), "a transparent surface that wrote depth would hide the one behind it");
	assert!(!glass.writes_id(), "and a pick through glass answers what is behind it");
}

// ---------------------------------------------------------------------------------------------
// Picking.
// ---------------------------------------------------------------------------------------------

#[test]
// ZERO IS NOTHING, so a pick on the background is unambiguous rather than being drawable zero. Two
// drawables claiming one identity would make a pick answer the wrong object, so it is refused.
fn a_drawable_gets_a_non_zero_identity_and_two_may_not_share_one() {
	// @covers: ObjectIdAttachment, ReservedZeroIdentity, ApplicationAssignedIdentity
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let material = scene.add_material(opaque(1)).unwrap();
	let node = scene.add_node(Node::identity()).unwrap();
	let first = scene.add_drawable(Drawable::new(node, 0, material)).unwrap();
	assert_ne!(scene.drawables()[first as usize].id(), 0, "the scene assigns an identity");
	let chosen = scene.add_drawable(Drawable::new(node, 1, material).with_id(4242)).unwrap();
	assert_eq!(scene.drawables()[chosen as usize].id(), 4242, "the application may set one");
	assert_eq!(scene.add_drawable(Drawable::new(node, 2, material).with_id(4242)), Err(Error::DuplicateId { id: 4242 }));

	assert_eq!(scene.drawable_of_id(4242), Some(chosen));
	assert_eq!(scene.drawable_of_id(0), None, "zero is nothing");
	assert_eq!(scene.drawable_of_id(99), None);
}

#[test]
// A PICK OUTSIDE THE ATTACHMENT IS REFUSED RATHER THAN CLAMPED, because a clamped pick answers about
// a pixel the caller did not ask about and the caller has no way to tell.
fn a_pick_outside_the_attachment_is_refused() {
	// @covers: SelectionPass, IdentityReadback, DepthReadback, ColourReadback, AsynchronousReadback, PickOutsideAttachmentRefusal
	use crate::pick::Readback;
	assert!(crate::pick::request(Readback::Identity, 0, 0, 800, 600, 1, 7).is_ok());
	assert!(crate::pick::request(Readback::Identity, 799, 599, 800, 600, 1, 7).is_ok());
	assert!(matches!(crate::pick::request(Readback::Identity, 800, 0, 800, 600, 1, 7), Err(Error::OutsideAttachment { .. })));
	assert!(matches!(crate::pick::request(Readback::Identity, 0, 600, 800, 600, 1, 7), Err(Error::OutsideAttachment { .. })));
	assert!(matches!(crate::pick::request(Readback::Identity, 0, 0, 0, 600, 1, 7), Err(Error::OutsideAttachment { .. })));

	// THE ANSWER ARRIVES WITH THE FRAME, so the pending pick carries the submission it completes
	// with rather than a promise that it is readable now. A software backend may report that serial
	// complete at once and a GPU one later, without the shape of any of this changing.
	let pending = crate::pick::request(Readback::Identity, 10, 20, 800, 600, 3, 99).unwrap();
	assert_eq!((pending.readback, pending.serial), (3, 99));
	assert!(!pending.is_answered_by(98));
	assert!(pending.is_answered_by(99) && pending.is_answered_by(100));

	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let material = scene.add_material(opaque(1)).unwrap();
	let node = scene.add_node(Node::identity()).unwrap();
	let drawable = scene.add_drawable(Drawable::new(node, 0, material).with_id(77)).unwrap();
	assert_eq!(crate::pick::answer(&scene, &pending, 77), Ok(Some(drawable)));
	assert_eq!(crate::pick::answer(&scene, &pending, 0), Ok(None), "zero is nothing");
	// A DEPTH READBACK DOES NOT NAME A DRAWABLE: a depth value reinterpreted as an identity names
	// whichever drawable happens to hold that number, which is wrong and looks right.
	let depth = crate::pick::request(Readback::Depth, 10, 20, 800, 600, 4, 99).unwrap();
	assert!(matches!(crate::pick::answer(&scene, &depth, 77), Err(Error::Degenerate { .. })));
	let colour = crate::pick::request(Readback::Colour, 10, 20, 800, 600, 5, 99).unwrap();
	assert!(matches!(crate::pick::answer(&scene, &colour, 77), Err(Error::Degenerate { .. })));
	// The three readbacks share ONE completion contract rather than three: an editor wants the
	// identity, a CAD viewport the depth to place a cursor in the world, and a colour picker the
	// pixel, and all three read the same pass.
	assert!(depth.is_answered_by(99) && colour.is_answered_by(99));

	// THE SELECTION PASS WRITES THE IDENTITY BESIDE THE COLOUR AND THE DEPTH IT IS RESOLVED AGAINST,
	// in ONE pass - a second pass would depth-test against its own buffer and could answer something
	// the person cannot see.
	let pass = crate::pick::selection_pass(5, 1, 2, 3);
	assert_eq!(pass.writes, vec![1, 2, 3]);
	assert!(pass.reads.is_empty());
	let mut graph = PassGraph::new();
	graph.add(pass);
	graph.add(Pass { id: 6, writes: vec![], reads: vec![2] });
	assert_eq!(graph.order().unwrap(), vec![5, 6], "and whatever samples the identity runs after it");
}

#[test]
// AND THE SAME ANSWER WITHOUT A FRAME: a ray against the drawables that write an identity gives what
// the attachment would have held - the nearest one that passed the depth test.
fn resolving_a_pick_finds_the_nearest_drawable_that_writes_an_identity() {
	// @covers: TransparentWritesNoIdentity
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let plain = scene.add_material(opaque(1)).unwrap();
	let glass = scene.add_material(blended(2)).unwrap();
	let far = scene.add_node(at(Vec3::new(0.0, 0.0, -20.0))).unwrap();
	let close = scene.add_node(at(Vec3::new(0.0, 0.0, -5.0))).unwrap();
	let behind = scene.add_drawable(Drawable::new(far, 0, plain).with_bounds(cube()).with_visibility(0b01)).unwrap();
	let nearest = scene.add_drawable(Drawable::new(close, 1, plain).with_bounds(cube()).with_visibility(0b10)).unwrap();
	scene.update();

	let (view_projection, _) = known_frustum();
	let inverse = view_projection.inverse().unwrap();
	let viewport = Viewport::new(0.0, 0.0, 800.0, 800.0);
	let ray = crate::pick::ray_from_window(400.0, 400.0, &viewport, &inverse).unwrap();
	let hit = crate::pick::resolve(&scene, &ray, u32::MAX).unwrap();
	assert_eq!(hit.drawable, nearest, "the nearer object is picked even though it was added last");
	assert_eq!(hit.id, scene.drawables()[nearest as usize].id());

	// A TRANSPARENT DRAWABLE WRITES NO IDENTITY, so a pick through glass answers what is behind it.
	let pane = scene.add_node(at(Vec3::new(0.0, 0.0, -2.0))).unwrap();
	scene.add_drawable(Drawable::new(pane, 2, glass).with_bounds(cube())).unwrap();
	scene.update();
	assert_eq!(crate::pick::resolve(&scene, &ray, u32::MAX).unwrap().drawable, nearest, "the glass in front changed nothing");

	// A MASK EXCLUDES A DRAWABLE FROM A PICK the same way it excludes it from a view, so a pick in a
	// view that cannot see the nearer object answers the one behind it.
	assert_eq!(crate::pick::resolve(&scene, &ray, 0b01).unwrap().drawable, behind);
	assert!(crate::pick::resolve(&scene, &ray, 0b100).is_none(), "a mask nothing is in picks nothing");
	// And a pixel that looks past everything.
	let corner = crate::pick::ray_from_window(2.0, 2.0, &viewport, &inverse).unwrap();
	assert!(crate::pick::resolve(&scene, &corner, u32::MAX).is_none());
}

#[test]
// THE Y INVERSION IS INHERITED FROM `window_from_ndc` AND NOT REINVENTED. A pick that got it
// backwards selects the object mirrored about the horizon, which looks like a scene bug.
fn the_window_to_ray_mapping_inverts_y_the_way_the_viewport_does() {
	let (view_projection, _) = known_frustum();
	let inverse = view_projection.inverse().unwrap();
	let viewport = Viewport::new(0.0, 0.0, 800.0, 800.0);
	let upper = crate::pick::ray_from_window(400.0, 100.0, &viewport, &inverse).unwrap();
	assert!(upper.direction.y > 0.0, "the top of the window is +Y: {}", upper.direction.y);
	let lower = crate::pick::ray_from_window(400.0, 700.0, &viewport, &inverse).unwrap();
	assert!(lower.direction.y < 0.0, "and the bottom is -Y: {}", lower.direction.y);
	// Outside the viewport is refused here too.
	assert!(matches!(crate::pick::ray_from_window(900.0, 400.0, &viewport, &inverse), Err(Error::OutsideAttachment { .. })));
}

#[test]
// UNDER AN ORTHOGRAPHIC PROJECTION EVERY RAY IS PARALLEL and the camera position is on none of them,
// which is why the ray is two unprojected points and not an eye and a direction.
fn an_orthographic_pick_uses_parallel_rays() {
	// @covers: OrthographicCamera
	let projection = render_math::orthographic_rh_zo(-10.0, 10.0, -10.0, 10.0, 1.0, 100.0).unwrap();
	let view = look_at_rh(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), Vec3::new(0.0, 1.0, 0.0)).unwrap();
	let inverse = projection.mul(&view).inverse().unwrap();
	let viewport = Viewport::new(0.0, 0.0, 200.0, 200.0);

	let centre = crate::pick::ray_from_window(100.0, 100.0, &viewport, &inverse).unwrap();
	let offset = crate::pick::ray_from_window(150.0, 100.0, &viewport, &inverse).unwrap();
	let scale = offset.direction.length() / centre.direction.length();
	assert!(near(offset.direction.x / scale, centre.direction.x) && near(offset.direction.y / scale, centre.direction.y));
	assert!(near(offset.origin.x, 5.0), "the origin moves with the pixel: {}", offset.origin.x);

	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let material = scene.add_material(opaque(1)).unwrap();
	let node = scene.add_node(at(Vec3::new(5.0, 0.0, -20.0))).unwrap();
	scene.add_drawable(Drawable::new(node, 0, material).with_bounds(cube())).unwrap();
	scene.update();
	assert!(crate::pick::resolve(&scene, &offset, u32::MAX).is_some());
	assert!(crate::pick::resolve(&scene, &centre, u32::MAX).is_none());
}

#[test]
// AN INSTANCE IS PICKABLE, because a field of grass the cursor cannot select is a scene layer that
// stops working the moment instancing is used.
fn an_instanced_drawable_is_picked_by_any_of_its_instances() {
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let material = scene.add_material(opaque(1)).unwrap();
	let node = scene.add_node(Node::identity()).unwrap();
	let instances = vec![
		Instance { transform: Mat4::from_translation(Vec3::new(0.0, 0.0, -30.0)), colour: Vec4::new(1.0, 1.0, 1.0, 1.0) },
		Instance { transform: Mat4::from_translation(Vec3::new(0.0, 0.0, -6.0)), colour: Vec4::new(1.0, 1.0, 1.0, 1.0) },
	];
	let drawable = scene.add_drawable(Drawable::new(node, 0, material).with_bounds(cube()).with_instances(instances)).unwrap();
	scene.update();
	let ray = crate::pick::Ray { origin: Vec3::new(0.0, 0.0, 10.0), direction: Vec3::new(0.0, 0.0, -1.0) };
	let hit = crate::pick::resolve(&scene, &ray, u32::MAX).unwrap();
	assert_eq!(hit.drawable, drawable);
	// The NEAREST instance decides the distance, which is the one at six units.
	assert!(near(hit.distance, 15.5), "the near face of the nearer instance: {}", hit.distance);
}

// ---------------------------------------------------------------------------------------------
// The pass graph.
// ---------------------------------------------------------------------------------------------

fn pass(id: u32, writes: &[u32], reads: &[u32]) -> Pass {
	Pass { id, writes: writes.to_vec(), reads: reads.to_vec() }
}

#[test]
// THE ORDER IS DERIVED AND NOT DECLARED: a pass that writes an offscreen target runs before the one
// that samples it, whichever order they were added in. A caller that stated the order would have to
// restate it every time a pass was added.
fn a_pass_runs_after_everything_it_reads_was_written() {
	// @covers: RenderPassGraph, DerivedPassOrder
	let mut graph = PassGraph::new();
	graph.add(pass(10, &[], &[1, 2]));
	graph.add(pass(20, &[1], &[]));
	graph.add(pass(30, &[2], &[1]));
	assert_eq!(graph.order().unwrap(), vec![20, 30, 10], "the writers come first, and the reader of both is last");
}

#[test]
// TWO INDEPENDENT PASSES RUN IN THE ORDER THEY WERE ADDED. A frame whose pass order varies between
// runs is a frame that differs from itself.
fn independent_passes_keep_the_order_they_were_given_in() {
	let mut graph = PassGraph::new();
	graph.add(pass(7, &[1], &[]));
	graph.add(pass(8, &[2], &[]));
	graph.add(pass(9, &[3], &[]));
	assert_eq!(graph.order().unwrap(), vec![7, 8, 9]);
}

#[test]
// A CYCLE NAMES A PASS ON IT. "The graph has a cycle" is not something a caller with thirty passes
// can act on.
fn a_cycle_in_the_pass_graph_is_refused_with_a_pass_named() {
	// @covers: PassCycleRefusal
	let mut graph = PassGraph::new();
	graph.add(pass(1, &[10], &[11]));
	graph.add(pass(2, &[11], &[10]));
	match graph.order() {
		Err(Error::PassCycle { pass }) => assert!(pass == 1 || pass == 2, "the pass named is on the cycle: {pass}"),
		other => panic!("a cycle was not refused: {other:?}"),
	}
}

#[test]
fn a_pass_that_reads_what_nothing_writes_is_refused() {
	let mut graph = PassGraph::new();
	graph.add(pass(1, &[], &[42]));
	assert_eq!(graph.order(), Err(Error::NoSuchTarget { target: 42 }));
}

// ---------------------------------------------------------------------------------------------
// Emission.
// ---------------------------------------------------------------------------------------------

struct OneMesh {
	indexed: bool,
}

impl Geometry for OneMesh {
	fn draw_of(&self, mesh: u32) -> Option<MeshDraw> {
		if mesh == 99 {
			return None;
		}
		Some(MeshDraw {
			topology: Topology::TriangleList,
			state: PipelineState {
				topology: Topology::TriangleList,
				cull: Cull::Back,
				depth_test: Some(render3d::CompareOp::LessOrEqual),
				// The geometry claims a depth write; the queue's rule is what decides.
				depth_write: true,
				samples: 1,
				per_sample_shading: false,
			},
			vertex_buffer: Buffer(1),
			instance_buffer: Some(Buffer(3)),
			vertices: 36,
			indices: self.indexed.then_some(Indices { buffer: Buffer(2), count: 36, wide: false }),
		})
	}
}

fn colour_target() -> RenderTargetView {
	RenderTargetView { texture: 1, view: TextureViewDesc { dimension: TextureDimension::D2, aspect: Aspect::Colour, base_mip: 0, mip_count: 1, base_layer: 0, layer_count: 1 }, format: "RGBA8", samples: 1, width: 800, height: 800, load: LoadOp::Clear, store: StoreOp::Store }
}

/// A depth-only attachment, which is the whole of what a shadow pass writes into.
fn shadow_target() -> render3d::resource::DepthStencilView {
	render3d::resource::DepthStencilView { texture: 2, view: TextureViewDesc { dimension: TextureDimension::D2, aspect: Aspect::Depth, base_mip: 0, mip_count: 1, base_layer: 0, layer_count: 1 }, format: shadow::MAP_FORMAT, samples: 1, width: 64, height: 64, depth_load: LoadOp::Clear, depth_store: StoreOp::Store, stencil_load: LoadOp::Discard, stencil_store: StoreOp::Discard }
}

fn pass_targets() -> PassTargets {
	PassTargets { targets: 0, viewport: Rect { x: 0, y: 0, width: 800, height: 800 }, samples: 1, id_set: Some(64) }
}

#[test]
fn a_shadow_pass_draws_the_casters_and_not_the_glass() {
	// A SURFACE THAT LETS LIGHT THROUGH DOES NOT STOP IT. A shadow map holds ONE depth per texel and
	// has no way to say "some of the light got past", so a transparent caster written into it casts
	// a SOLID shadow - the opposite of what the surface does. Leaving the transparent queue out is
	// the profile's shape and not a simplification.
	let mut scene = Scene::new(Limits::EXTENDED_MINIMUM);
	let plain = scene.add_material(opaque(1)).unwrap();
	let masked = scene.add_material(opaque(2).with_blending(Blending::AlphaMask { threshold: 0.5 })).unwrap();
	let glass = scene.add_material(blended(3)).unwrap();
	let rig = scene.add_node(at(Vec3::new(0.0, 0.0, 0.0))).unwrap();
	let camera = Camera::perspective(rig, core::f32::consts::FRAC_PI_2, 1.0, 1.0, 100.0, u32::MAX).unwrap();
	scene.add_camera(camera).unwrap();
	let group = scene.add_node(at(Vec3::new(0.0, 0.0, -10.0))).unwrap();
	scene.add_drawable(Drawable::new(group, 0, plain).with_bounds(cube())).unwrap();
	scene.add_drawable(Drawable::new(group, 0, masked).with_bounds(cube())).unwrap();
	scene.add_drawable(Drawable::new(group, 0, glass).with_bounds(cube())).unwrap();
	let queue = crate::queue::build(&mut scene, &camera).unwrap();

	let map = shadow_target();
	let depth_only = RenderTargetSet { colour: &[], depth_stencil: Some(map), resolve: &[] };
	let pass = crate::emit::CasterPass { targets: 7, viewport: Rect { x: 0, y: 0, width: 64, height: 64 }, pipeline: GraphicsPipeline(11), masked_pipeline: Some(GraphicsPipeline(12)), light_set: 5 };
	let mut list = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	crate::emit::record_casters(&scene, &queue, &OneMesh { indexed: true }, &mut list, &depth_only, &pass).unwrap();
	list.finish().unwrap();

	let commands = list.commands();
	let draws = commands.iter().filter(|command| matches!(command, Command::Draw { .. } | Command::DrawIndexed { .. })).count();
	assert_eq!(draws, 2, "the opaque caster and the masked one, and not the glass");

	// THE MASKED CASTERS GET THEIR OWN PIPELINE, because an alpha mask decides whether a fragment
	// EXISTS and a leaf drawn as a solid quad is worse than no leaf shadow at all.
	let pipelines: Vec<u32> = commands.iter().filter_map(|command| if let Command::BindPipeline(pipeline) = command { Some(pipeline.0) } else { None }).collect();
	assert_eq!(pipelines, vec![11, 12], "the depth-only pipeline for the opaque queue and the cutout one for the mask queue");

	// AND THE DEPTH WRITE IS FORCED ON. A caster pass that inherited the lighting pass's per-queue
	// rule would write nothing for the mask queue on a layer that shared one state, and an empty
	// shadow map reads exactly like a scene with no shadows in it. Read from the decision itself,
	// because `BindPipeline` carries the pipeline and not the state it was validated against.
	let arrived = PipelineState { topology: Topology::TriangleList, cull: Cull::Back, depth_test: Some(render3d::depth::CompareOp::LessOrEqual), depth_write: false, samples: 1, per_sample_shading: false };
	assert!(crate::emit::caster_state(arrived).depth_write, "every caster writes depth, whatever its geometry arrived carrying");
	assert!(matches!(commands.first(), Some(Command::BeginRenderPass { targets: 7 })));
	assert!(matches!(commands.last(), Some(Command::EndRenderPass)));
}

#[test]
fn a_shadow_pass_with_no_cutout_pipeline_leaves_the_masked_casters_out() {
	// THE HONEST ANSWER FOR A LAYER THAT HAS NO SUCH PIPELINE. Drawing a masked caster with the
	// opaque one would put a solid silhouette where the cutout is, so `None` means it is not drawn -
	// which is a shadow that is missing rather than a shadow that is wrong.
	let mut scene = Scene::new(Limits::EXTENDED_MINIMUM);
	let masked = scene.add_material(opaque(2).with_blending(Blending::AlphaMask { threshold: 0.5 })).unwrap();
	let rig = scene.add_node(at(Vec3::new(0.0, 0.0, 0.0))).unwrap();
	let camera = Camera::perspective(rig, core::f32::consts::FRAC_PI_2, 1.0, 1.0, 100.0, u32::MAX).unwrap();
	scene.add_camera(camera).unwrap();
	let group = scene.add_node(at(Vec3::new(0.0, 0.0, -10.0))).unwrap();
	scene.add_drawable(Drawable::new(group, 0, masked).with_bounds(cube())).unwrap();
	let queue = crate::queue::build(&mut scene, &camera).unwrap();

	let map = shadow_target();
	let depth_only = RenderTargetSet { colour: &[], depth_stencil: Some(map), resolve: &[] };
	let pass = crate::emit::CasterPass { targets: 7, viewport: Rect { x: 0, y: 0, width: 64, height: 64 }, pipeline: GraphicsPipeline(11), masked_pipeline: None, light_set: 5 };
	let mut list = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	crate::emit::record_casters(&scene, &queue, &OneMesh { indexed: true }, &mut list, &depth_only, &pass).unwrap();
	list.finish().unwrap();
	let draws = list.commands().iter().filter(|command| matches!(command, Command::Draw { .. } | Command::DrawIndexed { .. })).count();
	assert_eq!(draws, 0, "no cutout pipeline means no masked caster in the map");
}

#[test]
fn the_shadow_and_environment_resources_are_what_the_profile_names() {
	// A SHADOW MAP IS AN ARRAY WITH NO MIPS, and the no-mips half is the one worth holding: a mip is
	// an AVERAGE of depths, and the average of a near depth and a far one is a depth nothing in the
	// scene is at - so a filtered shadow map compares against a surface that does not exist.
	let limits = Limits::EXTENDED_MINIMUM;
	let cascades = shadow::cascade_map_desc(1024, 4, &limits).expect("four cascades at the profile's floor");
	assert_eq!(cascades.dimension, TextureDimension::D2Array);
	assert_eq!((cascades.width, cascades.height, cascades.layers), (1024, 1024, 4));
	assert_eq!(cascades.mip_levels, 1, "a shadow map is compared and not filtered by magnitude");
	assert_eq!(cascades.format, "Depth32F");
	assert!(cascades.usage.sampled && cascades.usage.depth_stencil_attachment, "it is written by the caster pass and read by the lighting one");

	// AND A COUNT ABOVE THE LIMIT IS REFUSED RATHER THAN SILENTLY GIVEN FEWER, which is the
	// profile's own rule for cascades.
	assert!(matches!(shadow::cascade_map_desc(1024, limits.max_shadow_cascades + 1, &limits), Err(Error::LimitExceeded { limit: "max_shadow_cascades", .. })));
	assert!(matches!(shadow::cascade_map_desc(0, 1, &limits), Err(Error::Degenerate { .. })), "a map with no extent");

	// A POINT LIGHT'S IS A CUBE, because the face is chosen by the major axis of a direction - which
	// is what a cube sampler does from the direction itself.
	let cube_map = shadow::cube_map_desc(512, &limits).expect("a point light's shadow cube");
	assert_eq!(cube_map.dimension, TextureDimension::Cube);
	assert_eq!(cube_map.layers, 6, "six faces, and the value is checked rather than assumed");

	// THE ENVIRONMENT'S MIPS ARE THE ROUGHNESS AXIS. 256 halves to 8 in six steps, so six levels -
	// and the chain stops there because below 8x8 the filter is wider than the face.
	let environment = crate::environment::cube_desc(256).expect("an environment cube");
	assert_eq!(environment.mip_levels, crate::environment::prefilter_levels(256));
	assert_eq!(environment.mip_levels, 6, "256, 128, 64, 32, 16, 8");
	assert_eq!(environment.format, crate::environment::HDR_FORMAT);
	assert!(matches!(crate::environment::cube_desc(4), Err(Error::Degenerate { .. })), "faces below the smallest prefilter level");

	// AND THE HDR TARGET IS HALF FLOATS, because the bloom threshold sits at luminance 1.0 and a
	// normalised format has nothing above it to spread.
	let hdr = crate::environment::hdr_target_desc(320, 240).expect("an HDR target");
	assert_eq!(hdr.format, "RGBA16F");
	assert_eq!(hdr.mip_levels, 1, "the bloom pyramid is its own chain of targets");
}

#[test]
// EVERY SCENE FEATURE ENDS IN A `render3d` COMMAND AND NOWHERE ELSE. The scene here uses the
// hierarchy, a camera on a node, lights, visibility masks, culling, all three queues, instancing, an
// offscreen target and picking identities, and the whole of its output is the recorded list.
fn a_scene_that_uses_every_feature_emits_only_render3d_commands() {
	// @covers: OffscreenTarget
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let plain = scene.add_material(opaque(1)).unwrap();
	let masked = scene.add_material(opaque(2).with_blending(Blending::AlphaMask { threshold: 0.5 })).unwrap();
	let glass = scene.add_material(blended(3)).unwrap();
	let rig = scene.add_node(at(Vec3::new(0.0, 0.0, 0.0))).unwrap();
	let camera = Camera::perspective(rig, core::f32::consts::FRAC_PI_2, 1.0, 1.0, 100.0, u32::MAX).unwrap();
	scene.add_camera(camera).unwrap();
	scene.add_light(Light { node: rig, kind: LightKind::Directional { direction: Vec3::new(0.0, -1.0, 0.0) }, colour: Vec3::new(1.0, 1.0, 1.0), intensity: 3.0, visibility: u32::MAX }).unwrap();
	let group = scene.add_node(at(Vec3::new(0.0, 0.0, -10.0))).unwrap();
	let child = scene.add_node(at(Vec3::new(1.0, 0.0, 0.0)).with_parent(group)).unwrap();
	let hidden = scene.add_node(Node::identity().disabled().with_parent(group)).unwrap();
	let outside = scene.add_node(at(Vec3::new(0.0, 0.0, 500.0))).unwrap();
	scene.add_drawable(Drawable::new(group, 0, plain).with_bounds(cube())).unwrap();
	let field: Vec<Instance> = (0..64).map(|step| Instance { transform: Mat4::from_translation(Vec3::new(step as f32 * 0.01, 0.0, -11.0)), colour: Vec4::new(1.0, 1.0, 1.0, 1.0) }).collect();
	scene.add_drawable(Drawable::new(child, 0, masked).with_bounds(cube()).with_instances(field)).unwrap();
	scene.add_drawable(Drawable::new(child, 0, glass).with_bounds(cube())).unwrap();
	scene.add_drawable(Drawable::new(hidden, 0, plain).with_bounds(cube())).unwrap();
	scene.add_drawable(Drawable::new(outside, 0, plain).with_bounds(cube())).unwrap();

	let queue = crate::queue::build(&mut scene, &camera).unwrap();
	assert_eq!(queue.culled, 1, "the drawable behind the eye was rejected");

	let colour = [colour_target()];
	let set = RenderTargetSet { colour: &colour, depth_stencil: None, resolve: &[] };
	let mut list = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	crate::emit::record(&scene, &queue, &OneMesh { indexed: true }, &mut list, &set, &pass_targets()).unwrap();
	list.finish().unwrap();

	let commands = list.commands();
	assert!(matches!(commands.first(), Some(Command::BeginRenderPass { targets: 0 })));
	assert!(matches!(commands.last(), Some(Command::EndRenderPass)));
	let draws: Vec<&Command> = commands.iter().filter(|command| matches!(command, Command::Draw { .. } | Command::DrawIndexed { .. })).collect();
	assert_eq!(draws.len(), 3, "one draw per drawable that survived culling and enablement, and no others");
	// INSTANCING IS THE COUNT AND NOT A SECOND PATH.
	assert!(draws.iter().any(|command| matches!(command, Command::DrawIndexed { instances: 64, .. })), "the instanced draw carries its count: {draws:?}");
	assert!(draws.iter().all(|command| matches!(command, Command::DrawIndexed { indices: 36, .. })));
	assert_eq!(commands.iter().filter(|command| matches!(command, Command::BeginRenderPass { .. })).count(), 1);
	assert_eq!(commands.iter().filter(|command| matches!(command, Command::EndRenderPass)).count(), 1);
	// THE IDENTITY SET IS BOUND FOR THE TWO OPAQUE QUEUES AND NOT FOR THE TRANSPARENT ONE.
	assert_eq!(commands.iter().filter(|command| matches!(command, Command::BindResources { set: 64 })).count(), 2, "the transparent drawable bound no identity");
}

#[test]
// THE DEPTH WRITE IS THE QUEUE'S AND NOT THE GEOMETRY'S, whatever the pipeline state arrived with.
// A transparent surface that wrote depth would hide the one behind it, and no caller should be able
// to cause that.
fn a_transparent_drawable_is_recorded_without_a_depth_write() {
	// @covers: TransparentDepthNoWrite
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let glass = scene.add_material(blended(3)).unwrap();
	let node = scene.add_node(at(Vec3::new(0.0, 0.0, -10.0))).unwrap();
	scene.add_drawable(Drawable::new(node, 0, glass).with_bounds(cube())).unwrap();
	let (_, frustum) = known_frustum();
	let queue = crate::queue::build_with(&mut scene, Vec3::ZERO, &frustum, u32::MAX);

	// The recorder is what applies the rule, so the fixture reads it back from the queue's own
	// answer and from the material's - the two the recorder consults.
	assert_eq!(queue.transparent.len(), 1);
	assert!(!QueueKind::Transparent.writes_depth());
	assert!(!scene.materials()[glass as usize].writes_depth());

	let colour = [colour_target()];
	let set = RenderTargetSet { colour: &colour, depth_stencil: None, resolve: &[] };
	let mut list = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	crate::emit::record(&scene, &queue, &OneMesh { indexed: false }, &mut list, &set, &pass_targets()).unwrap();
	assert!(list.commands().iter().any(|command| matches!(command, Command::Draw { .. })));
	assert_eq!(list.commands().iter().filter(|command| matches!(command, Command::BindResources { set: 64 })).count(), 0);
}

#[test]
// REDUNDANT BINDS ARE ELIDED. The queues sort by distance and not by material, so consecutive draws
// often share a pipeline; rebinding it is a command a backend has to look at to learn it changes
// nothing.
fn consecutive_draws_that_share_a_pipeline_bind_it_once() {
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let material = scene.add_material(opaque(1)).unwrap();
	for depth in [5.0_f32, 10.0, 20.0] {
		let node = scene.add_node(at(Vec3::new(0.0, 0.0, -depth))).unwrap();
		scene.add_drawable(Drawable::new(node, 0, material).with_bounds(cube())).unwrap();
	}
	let (_, frustum) = known_frustum();
	let queue = crate::queue::build_with(&mut scene, Vec3::ZERO, &frustum, u32::MAX);
	let colour = [colour_target()];
	let set = RenderTargetSet { colour: &colour, depth_stencil: None, resolve: &[] };
	let mut list = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	crate::emit::record(&scene, &queue, &OneMesh { indexed: false }, &mut list, &set, &PassTargets { id_set: None, ..pass_targets() }).unwrap();
	assert_eq!(list.commands().iter().filter(|command| matches!(command, Command::BindPipeline(_))).count(), 1, "three draws of one material bind the pipeline once");
	assert_eq!(list.commands().iter().filter(|command| matches!(command, Command::Draw { .. })).count(), 3);
	assert_eq!(list.commands().iter().filter(|command| matches!(command, Command::BindVertexBuffer { .. })).count(), 1);
}

#[test]
// A DRAWABLE WHOSE GEOMETRY THE SOURCE DOES NOT KNOW IS NOT SKIPPED. Drawing the rest of the frame
// without it is the defect that shows as an object which is sometimes missing.
fn geometry_the_source_does_not_know_is_refused_rather_than_skipped() {
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let material = scene.add_material(opaque(1)).unwrap();
	let node = scene.add_node(at(Vec3::new(0.0, 0.0, -10.0))).unwrap();
	scene.add_drawable(Drawable::new(node, 99, material).with_bounds(cube())).unwrap();
	let (_, frustum) = known_frustum();
	let queue = crate::queue::build_with(&mut scene, Vec3::ZERO, &frustum, u32::MAX);
	let colour = [colour_target()];
	let set = RenderTargetSet { colour: &colour, depth_stencil: None, resolve: &[] };
	let mut list = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	let outcome = crate::emit::record(&scene, &queue, &OneMesh { indexed: false }, &mut list, &set, &pass_targets());
	assert!(matches!(outcome, Err(render3d::Error::InvalidRenderState { .. })), "{outcome:?}");
}

#[test]
// TRANSPARENT INSTANCING IS PERMITTED AND DOCUMENTED AS APPROXIMATE. Instances within one set are
// not sorted against each other, so overlapping transparent instances may composite in the wrong
// order. Refusing it would make grass and particles impossible; hiding the limitation would make the
// wrong picture a mystery.
fn transparent_instances_within_one_set_are_not_sorted_against_each_other() {
	// @covers: TransparentInstancingApproximate
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	let glass = scene.add_material(blended(3)).unwrap();
	let node = scene.add_node(Node::identity()).unwrap();
	// Deliberately back to front in the stream: the near one first.
	let instances = vec![
		Instance { transform: Mat4::from_translation(Vec3::new(0.0, 0.0, -5.0)), colour: Vec4::new(1.0, 1.0, 1.0, 0.5) },
		Instance { transform: Mat4::from_translation(Vec3::new(0.0, 0.0, -30.0)), colour: Vec4::new(1.0, 1.0, 1.0, 0.5) },
	];
	scene.add_drawable(Drawable::new(node, 0, glass).with_bounds(cube()).with_instances(instances)).unwrap();
	let (_, frustum) = known_frustum();
	let queue = crate::queue::build_with(&mut scene, Vec3::ZERO, &frustum, u32::MAX);
	assert_eq!(queue.transparent.len(), 1, "one set, one entry, one draw");
	assert_eq!(queue.transparent[0].instances, vec![0, 1], "the stream order is kept and NOT sorted back to front");
}

#[test]
// THE EQUATIONS ARE EVALUATED IN LINEAR LIGHT and nothing here encodes. Lighting in an encoded space
// is the classic too-dark shadow, and it is wrong in a way that looks like an artistic choice: a
// half-bright light on a half-bright surface is a quarter, not the 0.73 an sRGB round trip gives.
fn the_equations_are_evaluated_in_linear_light_and_nothing_here_encodes() {
	// @covers: LinearColourSpace
	let surface = Surface::new(Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0));
	let lambert = Material::new(MaterialKind::Lambert, GraphicsPipeline(1), 1).with_base_colour(Vec4::new(0.5, 0.5, 0.5, 1.0));
	let half = Incident { to_light: Vec3::new(0.0, 0.0, 1.0), radiance: Vec3::new(0.5, 0.5, 0.5) };
	let colour = crate::material::shade(&lambert, &surface, Vec3::new(0.0, 0.0, 1.0), Vec3::ZERO, &[half]).unwrap();
	assert!(near(colour.x, 0.25), "a half times a half is a quarter in linear light: {}", colour.x);
	// And the texture is multiplied in the same space, not gamma-composed.
	let textured = crate::material::shade(&lambert, &surface.with_texture(Vec4::new(0.5, 0.5, 0.5, 1.0)), Vec3::new(0.0, 0.0, 1.0), Vec3::ZERO, &[half]).unwrap();
	assert!(near(textured.x, 0.125), "{}", textured.x);
}

// ---------------------------------------------------------------------------------------------
// `Scene3D Extended Profile 1`'s limits, on the same terms as the core ones above.
// ---------------------------------------------------------------------------------------------

#[test]
// THE EXTENDED LIMITS ARE THAT PROFILE'S, BY NAME - and the profile is frozen, so this fixture is
// what stops the two drifting in either direction: a limit the document names and this layer does
// not enforce, or one enforced that the document never promised.
fn every_extended_limit_the_profile_names_is_one_this_layer_enforces() {
	let limits = Limits::EXTENDED_MINIMUM;
	for entry in graphics_profile::scene3d_extended::SCENE3D_EXTENDED_1_MIN_LIMITS {
		let held = limits.by_name(entry.name).unwrap_or_else(|| panic!("the Extended profile names `{}` and this layer has no such limit", entry.name));
		assert_eq!(held, entry.minimum, "`{}` must be the Extended profile's floor", entry.name);
	}
	for name in ["max_shadow_cascades", "max_shadow_maps", "max_skeleton_joints", "max_morph_targets", "max_animation_tracks", "environment_prefilter_levels", "max_lod_levels"] {
		assert!(graphics_profile::scene3d_extended::SCENE3D_EXTENDED_1_MIN_LIMITS.iter().any(|entry| entry.name == name), "`{name}` is enforced but the Extended profile does not name it");
	}
}

#[test]
// AND EXTENDED IS ADDITIVE: claiming it changes nothing a core-conforming scene already admitted.
// A part that quietly raised or lowered a core floor would be a second profile wearing the first
// one's name.
fn claiming_the_extended_profile_moves_no_core_limit() {
	let core = Limits::PROFILE_MINIMUM;
	let extended = Limits::EXTENDED_MINIMUM;
	for entry in graphics_profile::scene3d::SCENE3D_PROFILE_1_MIN_LIMITS {
		assert_eq!(extended.by_name(entry.name), core.by_name(entry.name), "`{}` is a core limit and Extended does not move it", entry.name);
	}
}

#[test]
// ENTIRELY OR NOT AT ALL, which is the part's own rule. A core scene claims none of it, an Extended
// one claims all of it, and a scene carrying some of the six claims nothing - because an
// application finding shadows and no skinning, with nothing saying which half it had, is exactly
// what that rule refuses.
fn the_extended_profile_is_claimed_whole_or_not_at_all() {
	assert!(!Limits::PROFILE_MINIMUM.claims_extended(), "a core scene claims no part of Extended");
	assert!(Limits::EXTENDED_MINIMUM.claims_extended(), "and an Extended one claims all of it");
	// Every single omission is a scene that claims nothing, checked one field at a time rather than
	// on one example - a predicate that had dropped a term would pass the example and fail here.
	for name in ["max_shadow_cascades", "max_shadow_maps", "max_skeleton_joints", "max_morph_targets", "max_animation_tracks", "environment_prefilter_levels", "max_lod_levels"] {
		let mut partial = Limits::EXTENDED_MINIMUM;
		match name {
			"max_shadow_cascades" => partial.max_shadow_cascades = 0,
			"max_shadow_maps" => partial.max_shadow_maps = 0,
			"max_skeleton_joints" => partial.max_skeleton_joints = 0,
			"max_morph_targets" => partial.max_morph_targets = 0,
			"max_animation_tracks" => partial.max_animation_tracks = 0,
			_ => partial.environment_prefilter_levels = 0,
		}
		assert!(!partial.claims_extended(), "a scene missing `{name}` claims no Extended profile");
	}
}

// ---------------------------------------------------------------------------------------------
// Level of detail: `Scene3D Extended Profile 1`'s `detail` group. One fixture per rule, with the
// coverages worked out by hand from the definition.
// ---------------------------------------------------------------------------------------------

/// A scene claiming Extended, which is the only kind that may build a ladder at all.
fn extended() -> Limits {
	Limits::EXTENDED_MINIMUM
}

#[test]
// COVERAGE IS THE PROJECTED RADIUS OVER HALF THE VIEWPORT HEIGHT, and the number a person can
// reason about falls out of it: a sphere exactly filling the frame vertically covers 1.
//
// WORKED BY HAND. A 90 degree vertical field of view gives `tan(45 deg) = 1`, so a unit sphere ten
// units from the eye covers `1 / (10 * 1)` = 0.1; the same sphere at five units covers 0.2, and at
// one unit it covers 1 - which is the sphere touching the top and bottom of the frame.
fn coverage_is_the_projected_radius_as_a_fraction_of_half_the_frame() {
	let eye = Vec3::new(0.0, 0.0, 0.0);
	let tan_half = 1.0f32;
	for (distance, expected) in [(10.0f32, 0.1f32), (5.0, 0.2), (1.0, 1.0), (20.0, 0.05)] {
		let sphere = Sphere::new(Vec3::new(0.0, 0.0, -distance), 1.0);
		let coverage = detail::coverage_perspective(&sphere, eye, tan_half).expect("a well-formed coverage");
		assert!((coverage - expected).abs() < 1e-6, "a unit sphere at {distance} covers {expected}, got {coverage}");
	}
	// AND A NARROWER FIELD OF VIEW COVERS MORE AT THE SAME DISTANCE, which is the whole reason the
	// metric is not distance: the same object at the same place is bigger on screen when the camera
	// zooms in, and a distance-based ladder would keep drawing the coarse mesh.
	let sphere = Sphere::new(Vec3::new(0.0, 0.0, -10.0), 1.0);
	let wide = detail::coverage_perspective(&sphere, eye, 1.0).expect("wide");
	let narrow = detail::coverage_perspective(&sphere, eye, 0.5).expect("narrow");
	assert!(narrow > wide, "zooming in raises coverage: {narrow} must exceed {wide}");
	assert!((narrow - 0.2).abs() < 1e-6, "tan(fov/2) halved doubles the coverage, got {narrow}");
}

#[test]
// AN ORTHOGRAPHIC CAMERA HAS NO DISTANCE TERM AT ALL, which is not a special case but the same
// formula: the projection is the half-height, and moving the object does not change its size.
fn orthographic_coverage_does_not_move_with_the_object() {
	for depth in [-1.0f32, -50.0, -1000.0] {
		let sphere = Sphere::new(Vec3::new(0.0, 0.0, depth), 2.0);
		let coverage = detail::coverage_orthographic(&sphere, 8.0).expect("a well-formed coverage");
		assert!((coverage - 0.25).abs() < 1e-6, "a radius of 2 in a half-height of 8 covers 0.25 at every depth, got {coverage} at {depth}");
	}
}

#[test]
// THE EYE INSIDE THE SPHERE IS THE FINEST LEVEL AND NOT A DIVISION BY ZERO. A camera standing inside
// a drawable is what walking into a room is, and the arithmetic for it divides by a distance of
// zero - which without this answer is an infinity or a NaN, and a NaN compares false against every
// threshold and silently selects the FINEST level anyway by accident rather than by decision.
fn the_eye_at_the_centre_covers_everything_and_a_point_covers_nothing() {
	let eye = Vec3::new(3.0, 4.0, 5.0);
	let around = Sphere::new(eye, 2.0);
	assert_eq!(detail::coverage_perspective(&around, eye, 1.0).expect("inside"), f32::INFINITY);
	let point = Sphere::new(eye, 0.0);
	assert_eq!(detail::coverage_perspective(&point, eye, 1.0).expect("a point"), 0.0);
	// AND A DEGENERATE CAMERA IS REFUSED RATHER THAN ANSWERED. A field of view of zero is not a
	// camera that sees nothing; it is a projection with no inverse, and answering it would put a
	// number nobody can check into a ladder.
	assert!(matches!(detail::coverage_perspective(&around, eye, 0.0), Err(Error::Degenerate { .. })));
	assert!(matches!(detail::coverage_orthographic(&around, 0.0), Err(Error::Degenerate { .. })));
	let broken = Sphere::new(Vec3::new(f32::NAN, 0.0, 0.0), 1.0);
	assert!(matches!(detail::coverage_perspective(&broken, eye, 1.0), Err(Error::Degenerate { .. })));
}

#[test]
// THE LADDER IS READ MOST DETAILED FIRST AND EACH LEVEL TAKES OVER AT OR BELOW ITS THRESHOLD.
// The defaults halve: 0.5, 0.25, 0.125, 0.0625, so each level draws roughly a quarter of the pixels
// of the one above it.
fn the_default_ladder_halves_and_each_level_takes_over_at_its_own_threshold() {
	let ladder = Ladder::with_default_thresholds(&[10, 11, 12, 13], &extended()).expect("four levels");
	assert_eq!(ladder.levels(), 4);
	assert_eq!(ladder.threshold(0), None, "the finest level has no threshold");
	assert_eq!(ladder.threshold(1), Some(0.5));
	assert_eq!(ladder.threshold(2), Some(0.25));
	assert_eq!(ladder.threshold(3), Some(0.125));
	// AT OR BELOW, so the threshold itself belongs to the coarser level. Stated in the profile, and
	// a boundary that belonged to neither would leave a coverage with no level at all.
	for (coverage, level, mesh) in [(0.9f32, 0u32, 10u32), (0.5, 1, 11), (0.3, 1, 11), (0.25, 2, 12), (0.126, 2, 12), (0.125, 3, 13), (0.001, 3, 13)] {
		let chosen = ladder.select(coverage, None);
		assert_eq!(chosen, Detail::Level(level), "coverage {coverage} is level {level}");
		assert_eq!(ladder.mesh_of(chosen), Some(mesh), "level {level} draws mesh {mesh}");
	}
}

#[test]
// A MESH WITH ONE LEVEL NEVER CONSULTS A THRESHOLD, which is what makes the feature free for the
// meshes that do not use it.
fn one_level_is_always_that_level() {
	let ladder = Ladder::single(7);
	assert_eq!(ladder.levels(), 1);
	for coverage in [1000.0f32, 1.0, 0.001, 0.0] {
		assert_eq!(ladder.select(coverage, None), Detail::Level(0));
		assert_eq!(ladder.mesh_of(Detail::Level(0)), Some(7));
	}
}

#[test]
// A LADDER THAT DOES NOT DESCEND IS REFUSED AT LOAD AND NOT SORTED. Sorting would draw a scene the
// author did not write and would hide the authoring error for ever; refusing it happens where a mesh
// is built rather than where it is drawn.
fn a_ladder_that_does_not_descend_is_refused_rather_than_sorted() {
	let limits = extended();
	let rising = vec![Level { mesh: 1, threshold: 0.25 }, Level { mesh: 2, threshold: 0.5 }];
	assert!(matches!(Ladder::new(0, rising, &limits), Err(Error::Degenerate { .. })));
	// EQUAL IS NOT DESCENDING EITHER: two levels with one threshold means the second can never be
	// selected, which is a level an author wrote and nothing will ever draw.
	let flat = vec![Level { mesh: 1, threshold: 0.25 }, Level { mesh: 2, threshold: 0.25 }];
	assert!(matches!(Ladder::new(0, flat, &limits), Err(Error::Degenerate { .. })));
	let broken = vec![Level { mesh: 1, threshold: f32::NAN }];
	assert!(matches!(Ladder::new(0, broken, &limits), Err(Error::Degenerate { .. })));
	// AND THE PROFILE'S LIMIT IS A LIMIT. `max_lod_levels` is 4 at the Extended floor, so a fifth is
	// refused by name rather than silently dropped.
	let five: Vec<u32> = vec![0, 1, 2, 3, 4];
	assert!(matches!(Ladder::with_default_thresholds(&five, &limits), Err(Error::LimitExceeded { limit: "max_lod_levels", ceiling: 4, asked: 5 })));
	// AND A SCENE THAT NEVER CLAIMED EXTENDED CANNOT BUILD A LADDER AT ALL, because its limit is
	// zero - which is what "entirely or not at all" looks like from inside this module.
	assert!(matches!(Ladder::with_default_thresholds(&[0, 1], &Limits::PROFILE_MINIMUM), Err(Error::LimitExceeded { limit: "max_lod_levels", ceiling: 0, asked: 2 })));
	// `Ladder::single` IS THE EXCEPTION AND IS NOT AN EXTENDED FEATURE. One level is "draw this
	// mesh", which is what the core layer already does; it takes no limits because there is nothing
	// for a limit to bound, and a core scene reaches it without claiming anything.
	assert_eq!(Ladder::single(0).levels(), 1);
}

#[test]
// HYSTERESIS IS WHY A DRAWABLE SITTING ON A THRESHOLD DOES NOT FLICKER, and it is applied to the
// level HELD rather than to the one the ladder would pick.
//
// WORKED BY HAND against the ladder 0.5 / 0.25. Holding level 0, the boundary to level 1 is 0.5 and
// the widened one is 0.45: a coverage of 0.48 is below the threshold and stays at level 0, and 0.44
// finally moves. Holding level 1, the boundary back to level 0 is 0.5 widened to 0.55: 0.52 is above
// the raw threshold and stays at level 1, and 0.58 moves back. So between 0.45 and 0.55 the answer
// depends on what was drawn last frame, which is exactly the band a flickering drawable lived in.
fn a_drawable_on_a_threshold_holds_the_level_it_drew_last_frame() {
	let ladder = Ladder::with_default_thresholds(&[0, 1, 2], &extended()).expect("three levels");
	assert_eq!(ladder.select(0.48, Some(Detail::Level(0))), Detail::Level(0), "0.48 is inside the widened band, so level 0 is held");
	assert_eq!(ladder.select(0.44, Some(Detail::Level(0))), Detail::Level(1), "0.44 is past it, so the level is left");
	assert_eq!(ladder.select(0.52, Some(Detail::Level(1))), Detail::Level(1), "0.52 is inside the band from the other side");
	assert_eq!(ladder.select(0.58, Some(Detail::Level(1))), Detail::Level(0), "0.58 is past it, so it comes back");
	// AND THE FIRST FRAME HAS NO BAND AT ALL: a drawable that appears already small starts small,
	// rather than starting detailed and stepping down in view.
	assert_eq!(ladder.select(0.48, None), Detail::Level(1), "with no previous frame the ladder is read directly");
	assert_eq!(ladder.select(0.52, None), Detail::Level(0));
	// A BIG JUMP STILL LANDS WHERE THE LADDER SAYS. Hysteresis decides WHETHER the level is left, not
	// where it goes: an object that moves far in one frame does not step down one level per frame.
	assert_eq!(ladder.select(0.01, Some(Detail::Level(0))), Detail::Level(2), "leaving level 0 lands at the level the coverage names");
}

#[test]
// BELOW THE LAST THRESHOLD THE LAST LEVEL KEEPS BEING DRAWN. A ladder that ran out and drew nothing
// would delete distant geometry for a reason the author never wrote.
fn below_the_last_threshold_the_coarsest_level_keeps_being_drawn() {
	let ladder = Ladder::with_default_thresholds(&[0, 1], &extended()).expect("two levels");
	for coverage in [0.4f32, 0.01, 1e-6, 0.0] {
		assert_eq!(ladder.select(coverage, None), Detail::Level(1), "coverage {coverage} still draws the coarsest level");
	}
}

#[test]
// UNLESS THE MESH DECLARED A COVERAGE IT VANISHES AT, which is a decision a scene makes about its own
// content rather than one the ladder makes for it. VANISHED IS NOT CULLED: the drawable is on screen
// and the scene chose not to draw it, and the two are told apart by the answer rather than inferred.
fn a_mesh_may_declare_a_coverage_below_which_it_is_not_drawn() {
	let ladder = Ladder::with_default_thresholds(&[0, 1], &extended()).expect("two levels").vanishing_below(0.01).expect("a vanishing coverage");
	assert_eq!(ladder.select(0.02, None), Detail::Level(1));
	assert_eq!(ladder.select(0.005, None), Detail::Vanished);
	assert_eq!(ladder.mesh_of(Detail::Vanished), None, "a vanished drawable draws no mesh");
	// AND THE SAME TENTH GUARDS IT, because the vanishing coverage is a threshold like any other:
	// 0.0095 is below 0.01 and a drawn drawable holds on; 0.0105 is above it and a vanished one
	// stays away.
	assert_eq!(ladder.select(0.0095, Some(Detail::Level(1))), Detail::Level(1), "a drawn drawable holds inside the band");
	assert_eq!(ladder.select(0.0085, Some(Detail::Level(1))), Detail::Vanished, "past it, it goes");
	assert_eq!(ladder.select(0.0105, Some(Detail::Vanished)), Detail::Vanished, "a vanished drawable holds inside the band");
	assert_eq!(ladder.select(0.0115, Some(Detail::Vanished)), Detail::Level(1), "past it, it comes back");
	// A DEFAULT LADDER HAS NO VANISHING COVERAGE, and a degenerate one is refused.
	assert!(matches!(Ladder::single(0).vanishing_below(0.0), Err(Error::Degenerate { .. })));
}

#[test]
// THE COVERAGE COMES FROM THE BOUNDS THE DRAWABLE HAS NOW, which is the rule the deformation work
// joins to: a character that raises an arm GROWS its bounding sphere, and a coverage taken from the
// rest pose would step that arm down a level while it is still on screen.
//
// The module takes a `Sphere` rather than a mesh exactly so a rest-pose bound cannot be passed by
// accident; this fixture holds the consequence, which is that the larger sphere selects the finer
// level at the same camera.
fn a_grown_bound_selects_a_finer_level_than_the_rest_pose_would() {
	let ladder = Ladder::with_default_thresholds(&[0, 1, 2], &extended()).expect("three levels");
	let eye = Vec3::new(0.0, 0.0, 0.0);
	let at = Vec3::new(0.0, 0.0, -10.0);
	// A rest pose of radius 2.4 at ten units under a 90 degree field of view covers 0.24, which is
	// just under the 0.25 threshold and is therefore level 2. The same character with an arm up has
	// a radius of 2.6 and covers 0.26, which is above it and is level 1.
	let rest = Sphere::new(at, 2.4);
	let posed = Sphere::new(at, 2.6);
	let rest_coverage = detail::coverage_perspective(&rest, eye, 1.0).expect("rest");
	let posed_coverage = detail::coverage_perspective(&posed, eye, 1.0).expect("posed");
	assert!(rest_coverage < 0.25 && posed_coverage > 0.25, "the fixture straddles the threshold: {rest_coverage} and {posed_coverage}");
	assert_eq!(ladder.select(rest_coverage, None), Detail::Level(2));
	assert_eq!(ladder.select(posed_coverage, None), Detail::Level(1), "the pose the drawable is actually in chooses the level");
}

// ---------------------------------------------------------------------------------------------
// Skinning and morph targets: `Scene3D Extended Profile 1`'s `animation` group, the deformation
// half. Every value is worked out by hand from the rule rather than from this implementation.
// ---------------------------------------------------------------------------------------------

#[test]
// WEIGHTS ARE NORMALISED WHEN THE INFLUENCES ARE BUILT, NOT WHEN THE VERTEX IS DRAWN. An exporter
// that wrote 2, 2, 0, 0 meant half and half, and the correction happens once for the life of the
// asset rather than on every vertex of every frame.
fn skinning_weights_are_normalised_at_load() {
	let influences = Influences::new([0, 1, 0, 0], [2.0, 2.0, 0.0, 0.0]).expect("two even influences");
	assert_eq!(influences.weights(), &[0.5, 0.5, 0.0, 0.0]);
	let lopsided = Influences::new([3, 4, 5, 6], [0.3, 0.1, 0.1, 0.0]).expect("three influences");
	let total: f32 = lopsided.weights().iter().sum();
	assert!((total - 1.0).abs() < 1e-6, "the weights sum to one, got {total}");
	assert!((lopsided.weights()[0] - 0.6).abs() < 1e-6, "0.3 of 0.5 is 0.6, got {}", lopsided.weights()[0]);
	// A RIGID VERTEX IS ONE INFLUENCE AND CANNOT FAIL, which is the common case.
	assert_eq!(Influences::rigid(9).weights(), &[1.0, 0.0, 0.0, 0.0]);
	assert_eq!(Influences::rigid(9).joints()[0], 9);
	// AND A SET THAT CANNOT BE NORMALISED IS REFUSED RATHER THAN REPAIRED: weights summing to zero
	// name no joint, and a vertex with no joint stays at the origin while the mesh moves, which
	// reads as a tear in the geometry.
	assert!(matches!(Influences::new([0, 1, 2, 3], [0.0; 4]), Err(Error::Degenerate { .. })));
	assert!(matches!(Influences::new([0, 1, 2, 3], [-1.0, 1.0, 0.0, 0.0]), Err(Error::Degenerate { .. })));
	assert!(matches!(Influences::new([0, 1, 2, 3], [f32::NAN, 1.0, 0.0, 0.0]), Err(Error::Degenerate { .. })));
}

#[test]
// THE JOINT MATRIX IS `joint_world * inverse_bind`, composed once per frame per skeleton.
//
// WORKED BY HAND. A joint whose bind pose is a translation of +5 along x has an inverse bind of -5;
// if its world transform this frame is also +5, the composition is the identity and a vertex
// authored at the bind pose does not move. Move the joint to +8 and the same vertex moves by +3,
// which is the joint's motion SINCE the bind and not its position.
fn a_pose_carries_a_vertex_by_the_joint_s_motion_since_the_bind() {
	let bind = Mat4::from_translation(Vec3::new(5.0, 0.0, 0.0));
	let inverse_bind = bind.inverse().expect("an invertible bind");
	let at_rest = Pose::compose(&[bind], &[inverse_bind], &extended()).expect("a one-joint pose");
	let vertex = Vec3::new(5.0, 1.0, 0.0);
	let unmoved = at_rest.skin_point(vertex, &Influences::rigid(0));
	assert!(unmoved.sub(vertex).length() < 1e-5, "at the bind pose a vertex does not move, got {unmoved:?}");

	let moved_joint = Mat4::from_translation(Vec3::new(8.0, 0.0, 0.0));
	let posed = Pose::compose(&[moved_joint], &[inverse_bind], &extended()).expect("a moved pose");
	let moved = posed.skin_point(vertex, &Influences::rigid(0));
	assert!(moved.sub(Vec3::new(8.0, 1.0, 0.0)).length() < 1e-5, "the vertex moves by the joint's +3, got {moved:?}");

	// TWO JOINTS AT HALF WEIGHT PUT THE VERTEX HALFWAY, which is what linear blend skinning is: the
	// weighted sum of the point through each joint, not a blend of the joints themselves.
	let still = Mat4::from_translation(Vec3::new(5.0, 0.0, 0.0));
	let both = Pose::compose(&[moved_joint, still], &[inverse_bind, inverse_bind], &extended()).expect("two joints");
	let blended = both.skin_point(vertex, &Influences::new([0, 1, 0, 0], [0.5, 0.5, 0.0, 0.0]).expect("half and half"));
	assert!(blended.sub(Vec3::new(6.5, 1.0, 0.0)).length() < 1e-5, "half of +3 and half of nothing is +1.5, got {blended:?}");
}

#[test]
// A SKELETON IS REFUSED RATHER THAN TRUNCATED when it does not match its bind pose, and the
// profile's joint limit is a limit.
fn a_skeleton_that_does_not_match_its_bind_pose_is_refused() {
	let identity = Mat4::from_translation(Vec3::new(0.0, 0.0, 0.0));
	assert!(matches!(Pose::compose(&[identity, identity], &[identity], &extended()), Err(Error::Degenerate { .. })));
	let broken = Mat4::from_translation(Vec3::new(f32::NAN, 0.0, 0.0));
	assert!(matches!(Pose::compose(&[broken], &[identity], &extended()), Err(Error::Degenerate { .. })));
	// `max_skeleton_joints` IS 128 AT THE EXTENDED FLOOR.
	let many: Vec<Mat4> = core::iter::repeat(identity).take(129).collect();
	assert!(matches!(Pose::compose(&many, &many, &extended()), Err(Error::LimitExceeded { limit: "max_skeleton_joints", ceiling: 128, asked: 129 })));
	// AND A CORE SCENE HAS NO SKELETON AT ALL, because it never claimed the part.
	assert!(matches!(Pose::compose(&[identity], &[identity], &Limits::PROFILE_MINIMUM), Err(Error::LimitExceeded { limit: "max_skeleton_joints", ceiling: 0, asked: 1 })));
}

#[test]
// MORPH TARGETS ARE ADDITIVE, which is why two at once do both rather than blending between them: a
// raised brow and a smile displace different vertices and applying both should do both.
fn morph_targets_add_rather_than_blend() {
	let base = Vec3::new(1.0, 2.0, 3.0);
	let up = [Vec3::new(0.0, 1.0, 0.0)];
	let across = [Vec3::new(1.0, 0.0, 0.0)];
	let both = [MorphTarget { displacement: &up, weight: 1.0 }, MorphTarget { displacement: &across, weight: 1.0 }];
	assert_eq!(deform::morphed(base, 0, &both), Vec3::new(2.0, 3.0, 3.0), "both displacements are applied");
	let half = [MorphTarget { displacement: &up, weight: 0.5 }, MorphTarget { displacement: &across, weight: 0.5 }];
	assert_eq!(deform::morphed(base, 0, &half), Vec3::new(1.5, 2.5, 3.0), "half of each, and not a blend between them");
	// A WEIGHT OF ZERO IS NOT APPLIED AND COSTS NOTHING.
	let none = [MorphTarget { displacement: &up, weight: 0.0 }];
	assert_eq!(deform::morphed(base, 0, &none), base);
	assert_eq!(deform::morphed(base, 0, &[]), base);
}

#[test]
// A MORPH TARGET SHORTER THAN THE MESH IS AN ASSET DEFECT AND THE WHOLE SET IS REFUSED, because
// discovering it at vertex 4,000 leaves the first 3,999 already displaced.
fn a_short_or_overlong_morph_set_is_refused_before_any_of_it_is_applied() {
	let limits = extended();
	let short = [Vec3::new(0.0, 0.0, 0.0)];
	assert!(matches!(deform::check_targets(2, &[MorphTarget { displacement: &short, weight: 1.0 }], &limits), Err(Error::Degenerate { .. })));
	assert!(deform::check_targets(1, &[MorphTarget { displacement: &short, weight: 1.0 }], &limits).is_ok());
	assert!(matches!(deform::check_targets(1, &[MorphTarget { displacement: &short, weight: f32::INFINITY }], &limits), Err(Error::Degenerate { .. })));
	// `max_morph_targets` IS 32 AT THE EXTENDED FLOOR.
	let many: Vec<MorphTarget<'_>> = core::iter::repeat(MorphTarget { displacement: &short, weight: 0.0 }).take(33).collect();
	assert!(matches!(deform::check_targets(1, &many, &limits), Err(Error::LimitExceeded { limit: "max_morph_targets", ceiling: 32, asked: 33 })));
}

#[test]
// THE ORDER IS MORPH THEN SKIN, AND IT IS OBSERVABLE. A morph applied after skinning would displace
// along a rest-pose direction while the vertex is somewhere else, which moves it out of the pose.
//
// WORKED BY HAND. The joint rotates a quarter turn about z, so a rest-pose +x becomes +y. A vertex
// at (1,0,0) with a morph target displacing it by (1,0,0) is at (2,0,0) before skinning and lands at
// (0,2,0). Skinning first would put it at (0,1,0) and then add (1,0,0) for (1,1,0) - a different
// place, and the one a reader of the rule would not expect.
fn a_morph_is_applied_in_the_rest_pose_and_carried_by_the_skin() {
	let quarter_turn = Mat4::from_linear(&Quat::from_axis_angle(Vec3::new(0.0, 0.0, 1.0), core::f32::consts::FRAC_PI_2).expect("a unit axis").to_mat3(), Vec3::new(0.0, 0.0, 0.0));
	let identity = Mat4::from_translation(Vec3::new(0.0, 0.0, 0.0));
	let pose = Pose::compose(&[quarter_turn], &[identity], &extended()).expect("a rotating joint");
	let base = [Vec3::new(1.0, 0.0, 0.0)];
	let displacement = [Vec3::new(1.0, 0.0, 0.0)];
	let targets = [MorphTarget { displacement: &displacement, weight: 1.0 }];
	let influences = [Influences::rigid(0)];
	let bounds = deform::deformed_bounds(&base, &targets, &influences, &pose, &extended()).expect("deformed bounds");
	let centre = bounds.centre();
	assert!(centre.sub(Vec3::new(0.0, 2.0, 0.0)).length() < 1e-5, "morph then skin puts the vertex at (0,2,0), got {centre:?}");
}

#[test]
// THE BOUNDS ARE THE POSE'S AND NOT THE REST POSE'S, which is the join to the level-of-detail rule:
// a character that raises an arm grows, and every question asked of its bounds is asked of the pose
// on screen.
//
// WORKED BY HAND. Two vertices at x = 0 and x = 1, the second bound to a joint that lifts it by 3.
// The rest pose spans y from 0 to 0; the posed one spans 0 to 3, so the deformed box is taller by
// exactly the joint's motion - and the sphere around it is larger, which is what selects the finer
// level of detail.
fn deformed_bounds_grow_with_the_pose_and_feed_the_level_of_detail() {
	let identity = Mat4::from_translation(Vec3::new(0.0, 0.0, 0.0));
	let lifted = Mat4::from_translation(Vec3::new(0.0, 3.0, 0.0));
	let pose = Pose::compose(&[identity, lifted], &[identity, identity], &extended()).expect("two joints");
	let base = [Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0)];
	let influences = [Influences::rigid(0), Influences::rigid(1)];
	let bounds = deform::deformed_bounds(&base, &[], &influences, &pose, &extended()).expect("deformed bounds");
	assert_eq!(bounds.minimum, Vec3::new(0.0, 0.0, 0.0));
	assert_eq!(bounds.maximum, Vec3::new(1.0, 3.0, 0.0));
	let rest = Aabb::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0));
	assert!(bounds.bounding_sphere().radius > rest.bounding_sphere().radius, "the posed bound is larger than the rest pose's");

	// AND A MESH WITH NOTHING IN IT HAS NO BOUNDS, rather than an inverted box that would contain
	// everything or nothing depending on who asked.
	assert!(matches!(deform::deformed_bounds(&[], &[], &[], &pose, &extended()), Err(Error::Degenerate { .. })));
	assert!(matches!(deform::deformed_bounds(&base, &[], &influences[..1], &pose, &extended()), Err(Error::Degenerate { .. })));
}

// ---------------------------------------------------------------------------------------------
// Animation: the rest of `Scene3D Extended Profile 1`'s `animation` group - interpolation, the
// shorter arc, root motion and the loop seam.
// ---------------------------------------------------------------------------------------------

fn linear<T: Copy>(keys: &[(f32, T)]) -> Curve<T> {
	Curve::Linear(keys.iter().map(|(time, value)| Key { time: *time, value: *value }).collect())
}

fn translation_track(joint: u16, keys: &[(f32, Vec3)]) -> Track {
	Track { target: Target::Joint(joint), channel: Channel::Translation(linear(keys)) }
}

fn rotation_track(joint: u16, keys: &[(f32, Quat)]) -> Track {
	Track { target: Target::Joint(joint), channel: Channel::Rotation(linear(keys)) }
}

fn turn(radians: f32) -> Quat {
	Quat::from_axis_angle(Vec3::new(0.0, 0.0, 1.0), radians).expect("a unit axis")
}

#[test]
// TRANSLATION AND SCALE ARE LINEAR, and the value at the midpoint of a span is the midpoint of its
// two keys - which is what "linear" has to mean for two implementations to agree.
fn translation_and_scale_interpolate_linearly() {
	let track = translation_track(0, &[(0.0, Vec3::new(0.0, 0.0, 0.0)), (2.0, Vec3::new(4.0, 8.0, 0.0))]);
	let clip = Clip::new(vec![track], 2.0, Ending::Clamp, 0, true, &extended()).expect("a one-track clip");
	for (time, expected) in [(0.0f32, Vec3::new(0.0, 0.0, 0.0)), (0.5, Vec3::new(1.0, 2.0, 0.0)), (1.0, Vec3::new(2.0, 4.0, 0.0)), (2.0, Vec3::new(4.0, 8.0, 0.0))] {
		let sampled = clip.sample(time).joint(0).copied().expect("the joint is driven");
		let value = sampled.translation.expect("a translation");
		assert!(value.sub(expected).length() < 1e-6, "at {time} the value is {expected:?}, got {value:?}");
	}
	// BEFORE THE FIRST KEY AND AFTER THE LAST, THE END VALUE HOLDS rather than being extrapolated:
	// extrapolation puts a joint somewhere the author never authored.
	let clamped = clip.sample(-5.0).joint(0).copied().expect("driven").translation.expect("a translation");
	assert!(clamped.length() < 1e-6, "before the clip the first key holds, got {clamped:?}");
}

#[test]
// ROTATION IS SPHERICAL LINEAR AND NOT COMPONENT-WISE, which is a measurable difference and not a
// preference: at the midpoint of a 90 degree turn, slerp gives exactly 45 degrees, while a
// normalised linear blend gives a different angle - the error that makes a turn crawl at its ends.
//
// WORKED BY HAND. Rotating (1,0,0) by the midpoint of a quarter turn about z must land on
// (cos 45, sin 45, 0), which is (0.7071, 0.7071, 0).
fn rotation_interpolates_along_the_arc_and_not_through_the_chord() {
	let track = rotation_track(0, &[(0.0, turn(0.0)), (1.0, turn(core::f32::consts::FRAC_PI_2))]);
	let clip = Clip::new(vec![track], 1.0, Ending::Clamp, 0, true, &extended()).expect("a rotation clip");
	let half = clip.sample(0.5).joint(0).copied().expect("driven").rotation.expect("a rotation");
	let spun = half.rotate(Vec3::new(1.0, 0.0, 0.0));
	let root_half = core::f32::consts::FRAC_1_SQRT_2;
	assert!(spun.sub(Vec3::new(root_half, root_half, 0.0)).length() < 1e-4, "the midpoint of a quarter turn is 45 degrees, got {spun:?}");
	// AND THE ANGLE ADVANCES EVENLY, which is the property a normalised linear blend does not have:
	// a quarter of the way through is a quarter of the angle.
	let quarter = clip.sample(0.25).joint(0).copied().expect("driven").rotation.expect("a rotation");
	let at_quarter = quarter.rotate(Vec3::new(1.0, 0.0, 0.0));
	let eighth = core::f32::consts::FRAC_PI_8;
	let expected = Vec3::new(cosine(eighth), sine(eighth), 0.0);
	assert!(at_quarter.sub(expected).length() < 1e-4, "a quarter of the way is a quarter of the angle, got {at_quarter:?}");
}

#[test]
// THE SHORTER ARC, which is what makes `q` and `-q` the same rotation rather than two.
//
// WORKED BY HAND. A turn of 10 degrees written with its end quaternion negated describes the same
// rotation; interpolated towards the negated form without choosing the sign it takes the long way
// round - 350 degrees instead of 10 - and the midpoint lands at 175 degrees rather than at 5.
fn a_rotation_takes_the_shorter_arc_between_two_keys() {
	let ten = 10.0f32.to_radians();
	let end = turn(ten);
	let negated = Quat::from_components(-end.x, -end.y, -end.z, -end.w).expect("a unit quaternion");
	let track = rotation_track(0, &[(0.0, turn(0.0)), (1.0, negated)]);
	let clip = Clip::new(vec![track], 1.0, Ending::Clamp, 0, true, &extended()).expect("a rotation clip");
	let half = clip.sample(0.5).joint(0).copied().expect("driven").rotation.expect("a rotation");
	let spun = half.rotate(Vec3::new(1.0, 0.0, 0.0));
	let five = 5.0f32.to_radians();
	let expected = Vec3::new(cosine(five), sine(five), 0.0);
	assert!(spun.sub(expected).length() < 1e-4, "the shorter arc puts the midpoint at 5 degrees, got {spun:?}");
}

#[test]
// ROOT MOTION IS EXTRACTED BY DEFAULT: the root's translation leaves the pose and is handed back as
// a delta, so an application that never asked for it gets a character walking on the spot rather
// than one drifting out of the world.
fn root_motion_is_extracted_unless_the_clip_keeps_it() {
	let walk = [(0.0f32, Vec3::new(0.0, 0.0, 0.0)), (1.0, Vec3::new(0.0, 0.0, -2.0))];
	let tracks = vec![translation_track(0, &walk), translation_track(1, &[(0.0, Vec3::new(1.0, 0.0, 0.0)), (1.0, Vec3::new(1.0, 0.0, 0.0))])];
	let extracting = Clip::new(tracks.clone(), 1.0, Ending::Clamp, 0, false, &extended()).expect("an extracting clip");
	let pose = extracting.sample(0.5);
	assert!(pose.joint(0).expect("the root is driven").translation.is_none(), "the root's translation left the pose");
	assert!(pose.root_delta().sub(Vec3::new(0.0, 0.0, -1.0)).length() < 1e-6, "and came back as the delta, got {:?}", pose.root_delta());
	// A NON-ROOT JOINT IS UNTOUCHED, which is what makes this extraction rather than a filter.
	assert!(pose.joint(1).expect("driven").translation.expect("kept").sub(Vec3::new(1.0, 0.0, 0.0)).length() < 1e-6);

	// AND A CLIP MAY DECLARE THAT IT KEEPS IT, which is the case where the animation IS the motion.
	let keeping = Clip::new(tracks, 1.0, Ending::Clamp, 0, true, &extended()).expect("a keeping clip");
	let kept = keeping.sample(0.5);
	assert!(kept.joint(0).expect("driven").translation.expect("kept").sub(Vec3::new(0.0, 0.0, -1.0)).length() < 1e-6, "the motion stayed in the pose");
	assert_eq!(kept.root_delta(), Vec3::new(0.0, 0.0, 0.0), "and is not handed back as well, which would apply it twice");
}

#[test]
// A LOOPING CLIP WHOSE SEAM DOES NOT CLOSE IS REFUSED AT LOAD, because a seam that does not close
// pops once a cycle for the life of the asset and is found by watching rather than by testing.
fn a_looping_clip_whose_ends_disagree_is_refused() {
	let limits = extended();
	let closed = vec![translation_track(0, &[(0.0, Vec3::new(1.0, 0.0, 0.0)), (1.0, Vec3::new(2.0, 0.0, 0.0)), (2.0, Vec3::new(1.0, 0.0, 0.0))])];
	assert!(Clip::new(closed, 2.0, Ending::Loop, 0, true, &limits).is_ok(), "a clip that returns to its first key loops");
	let open = vec![translation_track(0, &[(0.0, Vec3::new(1.0, 0.0, 0.0)), (2.0, Vec3::new(2.0, 0.0, 0.0))])];
	assert!(matches!(Clip::new(open.clone(), 2.0, Ending::Loop, 0, true, &limits), Err(Error::Degenerate { .. })));
	// THE SAME CLIP IS FINE AS A ONE-SHOT, which is what makes this a property of looping rather
	// than of the keys.
	assert!(Clip::new(open, 2.0, Ending::Clamp, 0, true, &limits).is_ok());

	// A ROTATION SEAM IS COMPARED BY THE ABSOLUTE DOT, so `q` and `-q` close the loop: they are one
	// rotation, and a comparison that missed it would report a full turn where the author wrote
	// none.
	let start = turn(0.3);
	let negated = Quat::from_components(-start.x, -start.y, -start.z, -start.w).expect("a unit quaternion");
	let mirrored = vec![rotation_track(0, &[(0.0, start), (1.0, turn(1.0)), (2.0, negated)])];
	assert!(Clip::new(mirrored, 2.0, Ending::Loop, 0, true, &limits).is_ok(), "the same rotation written with the other sign closes the loop");
	let genuinely_open = vec![rotation_track(0, &[(0.0, turn(0.0)), (2.0, turn(1.0))])];
	assert!(matches!(Clip::new(genuinely_open, 2.0, Ending::Loop, 0, true, &limits), Err(Error::Degenerate { .. })));
}

#[test]
// A LOOPING CLIP WRAPS AND A ONE-SHOT CLAMPS. Both are decisions: a one-shot that wrapped would
// restart a death animation, and a loop that clamped would freeze on its last frame.
fn a_loop_wraps_and_a_one_shot_clamps() {
	let keys = vec![translation_track(0, &[(0.0, Vec3::new(0.0, 0.0, 0.0)), (2.0, Vec3::new(0.0, 0.0, 0.0))])];
	let looping = Clip::new(keys.clone(), 2.0, Ending::Loop, 0, true, &extended()).expect("a loop");
	assert!((looping.at(2.5) - 0.5).abs() < 1e-6, "2.5 into a 2 second loop is 0.5");
	assert!((looping.at(-0.5) - 1.5).abs() < 1e-6, "and a negative time wraps forwards, not backwards past zero");
	let once = Clip::new(keys, 2.0, Ending::Clamp, 0, true, &extended()).expect("a one-shot");
	assert!((once.at(2.5) - 2.0).abs() < 1e-6, "a one-shot holds its last frame");
	assert!((once.at(-0.5) - 0.0).abs() < 1e-6);
}

#[test]
// KEYFRAME TIMES ASCEND, LIE INSIDE THE CLIP, AND A TRACK HAS SOME. Each refusal is one that would
// otherwise be a picture: two keys at one time make the value depend on which the sampler found
// first, and a key past the duration is one nothing will ever reach.
fn a_malformed_track_is_refused_rather_than_sampled() {
	let limits = extended();
	let out_of_order = vec![translation_track(0, &[(1.0, Vec3::new(0.0, 0.0, 0.0)), (0.5, Vec3::new(1.0, 0.0, 0.0))])];
	assert!(matches!(Clip::new(out_of_order, 2.0, Ending::Clamp, 0, true, &limits), Err(Error::Degenerate { .. })));
	let coincident = vec![translation_track(0, &[(1.0, Vec3::new(0.0, 0.0, 0.0)), (1.0, Vec3::new(1.0, 0.0, 0.0))])];
	assert!(matches!(Clip::new(coincident, 2.0, Ending::Clamp, 0, true, &limits), Err(Error::Degenerate { .. })));
	let past_the_end = vec![translation_track(0, &[(0.0, Vec3::new(0.0, 0.0, 0.0)), (3.0, Vec3::new(1.0, 0.0, 0.0))])];
	assert!(matches!(Clip::new(past_the_end, 2.0, Ending::Clamp, 0, true, &limits), Err(Error::Degenerate { .. })));
	let empty = vec![Track { target: Target::Joint(0), channel: Channel::Translation(Curve::Linear(Vec::new())) }];
	assert!(matches!(Clip::new(empty, 2.0, Ending::Clamp, 0, true, &limits), Err(Error::Degenerate { .. })));
	assert!(matches!(Clip::new(Vec::new(), 0.0, Ending::Clamp, 0, true, &limits), Err(Error::Degenerate { .. })));
	// `max_animation_tracks` IS 256 AT THE EXTENDED FLOOR, and a core scene has none at all.
	let many: Vec<Track> = (0..257).map(|joint| translation_track(joint as u16, &[(0.0, Vec3::new(0.0, 0.0, 0.0)), (1.0, Vec3::new(0.0, 0.0, 0.0))])).collect();
	assert!(matches!(Clip::new(many, 1.0, Ending::Clamp, 0, true, &limits), Err(Error::LimitExceeded { limit: "max_animation_tracks", ceiling: 256, asked: 257 })));
}

/// `sin` and `cos` for the fixtures above, from the crate the profile already uses for them - so a
/// hand-computed expectation is checked against the same reduction the implementation uses rather
/// than against a second one written here.
fn sine(radians: f32) -> f32 {
	turn(radians).rotate(Vec3::new(1.0, 0.0, 0.0)).y
}

fn cosine(radians: f32) -> f32 {
	turn(radians).rotate(Vec3::new(1.0, 0.0, 0.0)).x
}

// ---------------------------------------------------------------------------------------------
// The physically based material: `Scene3D Extended Profile 1`'s `material` group. Every expected
// value is computed by hand from the profile's own equations, so an implementation that changes the
// arithmetic and keeps the equations passes, and one that changes an equation fails.
// ---------------------------------------------------------------------------------------------

/// A head-on fragment: normal, view and light all along +z, which is where the equations are
/// simplest to evaluate by hand.
fn head_on() -> (PbrSurface, Vec3, Vec<Incident>) {
	let surface = PbrSurface::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0));
	let eye = Vec3::new(0.0, 0.0, 1.0);
	let lights = vec![Incident { to_light: Vec3::new(0.0, 0.0, 1.0), radiance: Vec3::new(1.0, 1.0, 1.0) }];
	(surface, eye, lights)
}

#[test]
// THE WHOLE DIRECT TERM, WORKED BY HAND. Head-on, roughness 0.5, a white dielectric.
//
//   a = roughness^2 = 0.25, a^2 = 0.0625
//   D  = a^2 / (pi * (1 * (a^2 - 1) + 1)^2) = a^2 / (pi * a^4) = 1 / (pi * 0.0625) = 5.0929582
//   V  = 0.5 / (1 * sqrt(0.9375 + 0.0625) + 1 * sqrt(0.9375 + 0.0625)) = 0.5 / 2 = 0.25
//   F  = F0 + (1 - F0) * (1 - 1)^5 = F0 = 0.04
//   diffuse  = (1 - 0) * 1 / pi * (1 - 0.04) = 0.96 / pi = 0.3055775
//   specular = D * V * F = 5.0929582 * 0.25 * 0.04 = 0.0509296
//   colour   = (0.3055775 + 0.0509296) * 1 * 1 = 0.3565071
fn the_direct_term_is_the_product_the_profile_writes() {
	let (surface, eye, lights) = head_on();
	let material = PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 1.0), 0.0, 0.5);
	let colour = pbr::shade(&material, &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).expect("nothing is discarded");
	assert!((colour.x - 0.3565071).abs() < 1e-5, "the direct term is 0.3565071, got {}", colour.x);
	assert_eq!(colour.w, 1.0, "lighting never changes coverage");

	// AND THE TERMS THEMSELVES, so a failure above says WHICH of the four moved.
	assert!((pbr::distribution_ggx(1.0, 0.25) - 5.0929582).abs() < 1e-4, "GGX at the peak is 1 / (pi * a^2)");
	assert!((pbr::visibility_smith(1.0, 1.0, 0.25) - 0.25).abs() < 1e-6, "Smith head-on is 0.25");
	let f0 = Vec3::new(0.04, 0.04, 0.04);
	assert!((pbr::fresnel_schlick(1.0, f0).x - 0.04).abs() < 1e-6, "Fresnel along the half vector is F0 itself");
	assert!((pbr::fresnel_schlick(0.0, f0).x - 1.0).abs() < 1e-6, "and at grazing incidence it is 1, whatever F0 was");
}

#[test]
// A METAL HAS NO DIFFUSE TERM AND ITS BASE COLOUR IS ITS F0, which is what `metallic` selects
// between and what stops a renderer keeping a dim diffuse glow on gold.
//
// WORKED BY HAND. Head-on with `D * V` = 5.0929582 * 0.25 = 1.2732395, and F = base colour, so the
// answer is the base colour times 1.2732395 exactly.
fn a_metal_reflects_its_base_colour_and_has_no_diffuse() {
	let (surface, eye, lights) = head_on();
	let gold = PbrMaterial::new(Vec4::new(1.0, 0.5, 0.25, 1.0), 1.0, 0.5);
	let colour = pbr::shade(&gold, &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).expect("nothing is discarded");
	for (channel, base) in [(colour.x, 1.0f32), (colour.y, 0.5), (colour.z, 0.25)] {
		let expected = base * 1.2732395;
		assert!((channel - expected).abs() < 1e-4, "a metal reflects {expected}, got {channel}");
	}
	// A DIELECTRIC OF THE SAME COLOUR IS A DIFFERENT MATERIAL, and the difference is NOT that one is
	// darker. The metal is BRIGHTER here, because its F0 is the base colour itself (0.5 in green)
	// against a dielectric's 0.04 - so a test that only compared magnitudes would pass for a
	// renderer that had the two the wrong way round. What distinguishes them is the SHAPE of the
	// sum, so both are computed from the profile:
	//
	//   metal green      = F * D * V                        = 0.5  * 1.2732395 = 0.6366197
	//   dielectric green = base * (1 - F) / pi + F * D * V
	//                    = 0.5 * 0.96 / pi + 0.04 * 1.2732395
	//                    = 0.1527887      + 0.0509296       = 0.2037183
	//
	// The metal's whole answer is its specular term; the dielectric's is three quarters diffuse.
	let plastic = PbrMaterial::new(Vec4::new(1.0, 0.5, 0.25, 1.0), 0.0, 0.5);
	let dielectric = pbr::shade(&plastic, &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).expect("shaded");
	assert!((dielectric.y - 0.2037183).abs() < 1e-5, "the dielectric's green is diffuse plus specular, got {}", dielectric.y);
	let dielectric_specular = 0.04 * 1.2732395;
	assert!(dielectric.y - dielectric_specular > 0.15, "and most of it is the diffuse term the metal does not have at all");
}

#[test]
// THE ROUGHNESS MINIMUM IS CLAMPED ON THE PERCEPTUAL VALUE, BEFORE `a = roughness^2`. A roughness of
// zero makes `D` a delta function: an infinite highlight at one pixel and a NaN in a filtered
// environment lookup.
fn a_roughness_of_zero_is_the_profile_s_minimum_and_not_a_delta_function() {
	let (surface, eye, lights) = head_on();
	let mirror = PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 1.0), 0.0, 0.0);
	let floor = PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 1.0), 0.0, pbr::MIN_ROUGHNESS);
	let clamped = pbr::shade(&mirror, &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).expect("shaded");
	let stated = pbr::shade(&floor, &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).expect("shaded");
	assert!(clamped.x.is_finite(), "a roughness of zero is finite, got {}", clamped.x);
	assert!((clamped.x - stated.x).abs() < 1e-5, "and is exactly the profile's minimum, {} against {}", clamped.x, stated.x);
	// THE REMAPPING IS PERCEPTUAL AND IS NOT THE IDENTITY. A renderer that fed the perceptual value
	// straight into `D` goes from mirror to matte in the first quarter of the slider, so 0.5 and its
	// square must not shade the same.
	let half = pbr::shade(&PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 1.0), 1.0, 0.5), &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).expect("shaded");
	let squared = pbr::shade(&PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 1.0), 1.0, 0.25), &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).expect("shaded");
	assert!((half.x - squared.x).abs() > 1e-3, "roughness is remapped, so 0.5 and 0.25 are different materials");
}

#[test]
// EVERY DOT PRODUCT IS CLAMPED, AND `dot(N,V)` NEVER REACHES ZERO. A light behind the surface
// contributes nothing rather than a negative amount, and a grazing view does not divide by zero -
// which without the clamp is a silhouette of NaN pixels around every sphere.
fn a_light_behind_the_surface_contributes_nothing_and_a_grazing_view_is_finite() {
	let surface = PbrSurface::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0));
	let material = PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 1.0), 0.0, 0.5);
	let behind = vec![Incident { to_light: Vec3::new(0.0, 0.0, -1.0), radiance: Vec3::new(10.0, 10.0, 10.0) }];
	let colour = pbr::shade(&material, &surface, Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, 0.0, 0.0), &behind).expect("shaded");
	assert_eq!(colour.x, 0.0, "a light behind the surface adds nothing");
	// EXACTLY EDGE ON: the view is perpendicular to the normal, so `dot(N,V)` is zero before the
	// clamp and the visibility term would divide by zero.
	let grazing = vec![Incident { to_light: Vec3::new(0.0, 0.0, 1.0), radiance: Vec3::new(1.0, 1.0, 1.0) }];
	let edge = pbr::shade(&material, &surface, Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 0.0), &grazing).expect("shaded");
	assert!(edge.x.is_finite(), "a grazing view is finite, got {}", edge.x);
	assert!((1.0 - pbr::MIN_N_DOT_V) < 1.0, "the clamp is below one so it only bites at grazing angles");
}

#[test]
// OCCLUSION REACHES THE AMBIENT TERM AND NOTHING ELSE. It says how much of the sky a point can see;
// applying it to a light the scene placed would darken a surface that light demonstrably reaches.
fn occlusion_darkens_the_ambient_and_never_the_direct_light() {
	let (surface, eye, lights) = head_on();
	let ambient = Vec3::new(0.5, 0.5, 0.5);
	let material = PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 1.0), 0.0, 0.5);
	let open = pbr::shade(&material, &surface, eye, ambient, &lights).expect("shaded");
	let occluded = pbr::shade(&material, &surface.with_occlusion(0.0), eye, ambient, &lights).expect("shaded");
	assert!((open.x - occluded.x - 0.5).abs() < 1e-5, "the whole ambient 0.5 is removed and nothing else, got {} and {}", open.x, occluded.x);
	// AND WITH NO LIGHT AT ALL, FULL OCCLUSION LEAVES NOTHING.
	let dark = pbr::shade(&material, &surface.with_occlusion(0.0), eye, ambient, &[]).expect("shaded");
	assert!(dark.x.abs() < 1e-6, "a fully occluded surface with no direct light is black, got {}", dark.x);
	// `strength` IS HOW MUCH OF THE MAP IS APPLIED: at zero the map does nothing at all.
	let ignored = PbrMaterial { occlusion_strength: 0.0, ..material };
	let unaffected = pbr::shade(&ignored, &surface.with_occlusion(0.0), eye, ambient, &lights).expect("shaded");
	assert!((unaffected.x - open.x).abs() < 1e-6, "a strength of zero ignores the map");
}

#[test]
// EMISSIVE IS A SOURCE AND NOT A RECEIVER: added after everything, unattenuated, and untouched by
// occlusion. A renderer that occluded it would darken a glowing panel because of the wall behind it.
fn emissive_is_added_last_and_is_not_occluded() {
	let surface = PbrSurface::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0)).with_occlusion(0.0);
	let material = PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 1.0), 0.0, 0.5).with_emissive(Vec3::new(2.0, 0.0, 0.0));
	let colour = pbr::shade(&material, &surface, Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.5, 0.5, 0.5), &[]).expect("shaded");
	assert!((colour.x - 2.0).abs() < 1e-6, "the emissive arrives whole through full occlusion, got {}", colour.x);
	// AND IT IS NOT CLAMPED TO ONE, because tone mapping is LAST: a material that clamped its own
	// output would throw away everything bloom exists to spread before bloom ever saw it.
	assert!(colour.x > 1.0, "the result is linear radiance and not a display value");
}

#[test]
// glTF's PACKING: ROUGHNESS IN GREEN, METALLIC IN BLUE. A renderer that swapped them produces a
// rough metal wherever the author wrote a smooth dielectric, which is the whole surface and not a
// subtle difference.
fn the_metallic_roughness_map_is_read_green_then_blue() {
	let (surface, eye, lights) = head_on();
	let material = PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 1.0), 1.0, 1.0);
	// Green 0.5, blue 1: a half-rough metal. Its answer must equal the same material with the
	// factors carrying those numbers and an identity map.
	let mapped = pbr::shade(&material, &surface.with_metallic_roughness(Vec3::new(0.0, 0.5, 1.0)), eye, Vec3::new(0.0, 0.0, 0.0), &lights).expect("shaded");
	let factored = pbr::shade(&PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 1.0), 1.0, 0.5), &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).expect("shaded");
	assert!((mapped.x - factored.x).abs() < 1e-5, "green is roughness, got {} against {}", mapped.x, factored.x);
	// AND SWAPPING THE TWO CHANNELS IS A DIFFERENT MATERIAL, which is what makes the naming matter.
	let swapped = pbr::shade(&material, &surface.with_metallic_roughness(Vec3::new(0.0, 1.0, 0.5)), eye, Vec3::new(0.0, 0.0, 0.0), &lights).expect("shaded");
	assert!((swapped.x - mapped.x).abs() > 1e-3, "reading blue as roughness is a visibly different surface");
	// THE RED AND ALPHA CHANNELS ARE IGNORED.
	let noisy = pbr::shade(&material, &surface.with_metallic_roughness(Vec3::new(0.9, 0.5, 1.0)), eye, Vec3::new(0.0, 0.0, 0.0), &lights).expect("shaded");
	assert!((noisy.x - mapped.x).abs() < 1e-6, "red is ignored");
}

#[test]
// A MISSING TANGENT MEANS NO NORMAL MAP, which is a decision and not an omission: deriving one from
// screen-space derivatives makes the frame depend on the rasteriser's derivative rule, so a mesh
// whose author did not export tangents would look different on each backend.
fn the_normal_map_needs_a_tangent_and_uses_the_plus_y_up_convention() {
	let (flat, eye, lights) = head_on();
	let material = PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 1.0), 0.0, 0.5);
	let plain = pbr::shade(&material, &flat, eye, Vec3::new(0.0, 0.0, 0.0), &lights).expect("shaded");
	// A map with no tangent changes nothing at all.
	let tilted_texel = Vec3::new(0.6, 0.0, 0.8);
	let no_tangent = pbr::shade(&material, &flat.with_normal_map(tilted_texel), eye, Vec3::new(0.0, 0.0, 0.0), &lights).expect("shaded");
	assert!((no_tangent.x - plain.x).abs() < 1e-6, "with no tangent the normal map is not applied");
	// WITH A TANGENT, A FLAT TEXEL IS THE IDENTITY - which is what makes an unused normal map free.
	let with_tangent = flat.with_tangent(Vec3::new(1.0, 0.0, 0.0), 1.0);
	let flat_texel = pbr::shade(&material, &with_tangent.with_normal_map(Vec3::new(0.0, 0.0, 1.0)), eye, Vec3::new(0.0, 0.0, 0.0), &lights).expect("shaded");
	assert!((flat_texel.x - plain.x).abs() < 1e-6, "a flat normal map leaves the normal alone");
	// AND A TILTED ONE TURNS THE SURFACE AWAY FROM A HEAD-ON LIGHT, so it shades darker.
	let tilted = pbr::shade(&material, &with_tangent.with_normal_map(tilted_texel), eye, Vec3::new(0.0, 0.0, 0.0), &lights).expect("shaded");
	assert!(tilted.x < plain.x, "a tilted normal faces away from the light: {} against {}", tilted.x, plain.x);
	// THE HANDEDNESS IS glTF's `w` AND IT IS OBSERVABLE: the same texel with the bitangent mirrored
	// tilts the surface the other way, which is the +Y-down convention this profile refuses.
	let up = with_tangent.with_normal_map(Vec3::new(0.0, 0.6, 0.8));
	let down = flat.with_tangent(Vec3::new(1.0, 0.0, 0.0), -1.0).with_normal_map(Vec3::new(0.0, 0.6, 0.8));
	let side_light = vec![Incident { to_light: Vec3::new(0.0, 0.6, 0.8).normalise().expect("unit"), radiance: Vec3::new(1.0, 1.0, 1.0) }];
	let toward = pbr::shade(&material, &up, eye, Vec3::new(0.0, 0.0, 0.0), &side_light).expect("shaded");
	let away = pbr::shade(&material, &down, eye, Vec3::new(0.0, 0.0, 0.0), &side_light).expect("shaded");
	assert!(toward.x > away.x, "the handedness decides which way the crevice faces: {} against {}", toward.x, away.x);
}

#[test]
// A DOUBLE-SIDED SURFACE FLIPS ITS NORMAL BEFORE ANYTHING ELSE READS IT, so the lighting sees the
// flipped one. Flipping after shading would light a leaf's underside as though it were its top.
fn a_double_sided_back_face_is_lit_by_the_light_it_actually_faces() {
	let surface = PbrSurface::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0)).back_facing();
	let eye = Vec3::new(0.0, 0.0, -1.0);
	let behind = vec![Incident { to_light: Vec3::new(0.0, 0.0, -1.0), radiance: Vec3::new(1.0, 1.0, 1.0) }];
	let one_sided = PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 1.0), 0.0, 0.5);
	let two_sided = one_sided.two_sided();
	let unflipped = pbr::shade(&one_sided, &surface, eye, Vec3::new(0.0, 0.0, 0.0), &behind).expect("shaded");
	let flipped = pbr::shade(&two_sided, &surface, eye, Vec3::new(0.0, 0.0, 0.0), &behind).expect("shaded");
	assert_eq!(unflipped.x, 0.0, "without the flag the back face faces away from its own light");
	assert!(flipped.x > 0.0, "with it the back face is lit, got {}", flipped.x);
	assert!((flipped.x - 0.3565071).abs() < 1e-5, "and by exactly the head-on amount, got {}", flipped.x);
}

#[test]
// `Mask` DISCARDS STRICTLY BELOW ITS THRESHOLD, so a threshold of zero discards nothing - the core
// profile's rule under glTF's name for it. A DISCARDED FRAGMENT IS NOT A TRANSPARENT ONE: it writes
// no depth and no picking identity, which is why the answer is `None` rather than a zero alpha.
fn an_alpha_masked_fragment_below_the_threshold_is_discarded_rather_than_shaded() {
	let (surface, eye, lights) = head_on();
	let masked = PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 0.4), 0.0, 0.5).with_blending(Blending::AlphaMask { threshold: 0.5 });
	assert!(pbr::shade(&masked, &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).is_none(), "0.4 is below 0.5 and is discarded");
	let kept = PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 0.5), 0.0, 0.5).with_blending(Blending::AlphaMask { threshold: 0.5 });
	assert!(pbr::shade(&kept, &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).is_some(), "0.5 is not below 0.5 and is kept");
	let none = PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 0.0), 0.0, 0.5).with_blending(Blending::AlphaMask { threshold: 0.0 });
	assert!(pbr::shade(&none, &surface, eye, Vec3::new(0.0, 0.0, 0.0), &lights).is_some(), "a threshold of zero discards nothing");
	// AND THE BASE-COLOUR MAP'S ALPHA COUNTS: the test is on the product, not on the factor.
	let mapped = PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 1.0), 0.0, 0.5).with_blending(Blending::AlphaMask { threshold: 0.5 });
	let cut = PbrSurface { base_colour_texel: Vec4::new(1.0, 1.0, 1.0, 0.2), ..surface };
	assert!(pbr::shade(&mapped, &cut, eye, Vec3::new(0.0, 0.0, 0.0), &lights).is_none(), "the map's alpha is part of the product the threshold tests");
}

// ---------------------------------------------------------------------------------------------
// Environment lighting: `Scene3D Extended Profile 1`'s `environment` group. The split sum's two
// halves, the prefilter's shape, and the irradiance term.
// ---------------------------------------------------------------------------------------------

/// A sphere of directions with equal solid angles, for integrating an environment by hand.
///
/// THE GOLDEN-ANGLE SPIRAL, because a latitude-longitude grid concentrates points at the poles and
/// would weight the top of the sky more heavily than the sides - which is exactly the error the
/// `solid_angle` argument exists to prevent, so a fixture built on one could not detect it.
fn sphere_directions(count: u32) -> Vec<Vec3> {
	let golden = core::f32::consts::PI * (3.0 - render_math::sqrt(5.0));
	(0..count)
		.map(|index| {
			let z = 1.0 - 2.0 * (index as f32 + 0.5) / count as f32;
			let radius = render_math::sqrt((1.0 - z * z).max(0.0));
			let (sine, cosine) = render_math::quaternion::sin_cos(golden * index as f32);
			Vec3::new(radius * cosine, radius * sine, z)
		})
		.collect()
}

#[test]
// THE WHITE FURNACE. A sky of uniform radiance 1 delivers an irradiance of exactly `pi` to a surface
// of ANY orientation - the integral of `cos(theta)` over a hemisphere - and it is the one test that
// catches a wrong band factor, a wrong basis normalisation and a missing solid angle all at once.
//
// MEASURED: 3.1416056 at the pole and 3.1416214 on the diagonal, against `pi` = 3.1415927. The
// residual is the quadrature's and not the convolution's.
fn a_uniform_sky_delivers_pi_to_every_orientation() {
	let count = 4096u32;
	let solid_angle = 4.0 * core::f32::consts::PI / count as f32;
	let mut sky = Irradiance::new();
	for direction in sphere_directions(count) {
		sky.add(direction, Vec3::new(1.0, 1.0, 1.0), solid_angle);
	}
	for normal in [Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, 0.0, -1.0), Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.577, 0.577, 0.577)] {
		let irradiance = sky.evaluate(normal);
		assert!((irradiance.x - core::f32::consts::PI).abs() < 1e-3, "a uniform sky gives pi at {normal:?}, got {}", irradiance.x);
		assert!((irradiance.x - irradiance.z).abs() < 1e-6, "and the three channels agree");
	}
	// THE ZEROTH COEFFICIENT IS THE ONE THE CONSTANT LANDS IN: `Y00 * 4pi` = 3.5449.
	let zeroth = sky.coefficient(0).expect("nine coefficients");
	assert!((zeroth.x - 3.544_908).abs() < 1e-3, "the constant lands in L00, got {}", zeroth.x);
	// AND THE RESULT IS IRRADIANCE AND NOT AN EXIT RADIANCE: a Lambertian surface's contribution is
	// `albedo * E / pi`, the same `1 / pi` the direct diffuse carries, so a white surface under this
	// sky reflects radiance 1 rather than pi.
	let reflected = sky.evaluate(Vec3::new(0.0, 0.0, 1.0)).scale(1.0 / core::f32::consts::PI);
	assert!((reflected.x - 1.0).abs() < 1e-3, "a white Lambertian surface under a unit sky reflects 1, got {}", reflected.x);
}

#[test]
// A DIRECTIONAL SKY IS BRIGHTEST WHERE IT POINTS. The furnace above cannot tell a correct linear
// band from a missing one, because a constant has no linear band at all.
fn irradiance_follows_the_direction_the_light_comes_from() {
	let count = 4096u32;
	let solid_angle = 4.0 * core::f32::consts::PI / count as f32;
	let mut sky = Irradiance::new();
	for direction in sphere_directions(count) {
		// Bright above, dark below.
		let radiance = if direction.z > 0.0 { Vec3::new(1.0, 1.0, 1.0) } else { Vec3::new(0.0, 0.0, 0.0) };
		sky.add(direction, radiance, solid_angle);
	}
	let up = sky.evaluate(Vec3::new(0.0, 0.0, 1.0)).x;
	let side = sky.evaluate(Vec3::new(1.0, 0.0, 0.0)).x;
	let down = sky.evaluate(Vec3::new(0.0, 0.0, -1.0)).x;
	assert!(up > side && side > down, "a surface facing the bright half is brightest: {up}, {side}, {down}");
	assert!(down >= 0.0, "and nothing goes negative, got {down}");
	// A HEMISPHERE OF RADIANCE 1 FACING THE NORMAL IS THE FURNACE'S ANSWER: `pi`. The three-band
	// truncation undershoots it slightly, which is the approximation the profile's threshold admits.
	assert!((up - core::f32::consts::PI).abs() < 0.2, "facing the bright hemisphere is near pi, got {up}");
}

#[test]
// THE BRDF TABLE AT A SMOOTH SURFACE HEAD-ON IS EXACTLY `(1, 0)`: the lobe is a delta along the
// normal, so `dot(V,H)` is 1, Schlick's fifth power is zero, and the whole integral is the
// visibility term at normal incidence times four, which is one.
//
// MEASURED: `(1.0, 4.1e-17)`.
fn the_brdf_table_is_one_and_zero_at_a_smooth_surface_head_on() {
	let (scale, bias) = environment::brdf_integration(1.0, pbr::MIN_ROUGHNESS, environment::PREFILTER_SAMPLES);
	assert!((scale - 1.0).abs() < 1e-3, "a smooth surface head-on scales F0 by one, got {scale}");
	assert!(bias.abs() < 1e-3, "and adds nothing, got {bias}");
	// AND THE TABLE IS WHAT MAKES ONE PREFILTER SERVE EVERY F0: with `(1, 0)` the environment term
	// is the prefiltered radiance times F0 and nothing else.
	let f0 = Vec3::new(0.2, 0.4, 0.6);
	let prefiltered = Vec3::new(2.0, 2.0, 2.0);
	let term = environment::environment_term(f0, prefiltered, (1.0, 0.0));
	assert!(term.sub(f0.scale(2.0)).length() < 1e-6, "the split sum is `prefiltered * (F0 * A + B)`, got {term:?}");
}

#[test]
// ROUGHNESS TAKES ENERGY OUT AND GRAZING PUTS IT INTO THE BIAS, which is the shape the two channels
// exist to carry. Neither is a tuning constant: both fall out of the profile's own GGX and Smith.
//
// MEASURED at 1024 samples: head-on, `A` falls from 1.000 at the minimum roughness to 0.307 at 1.0;
// at `dot(N,V)` = 0.1 and roughness 0.5, `B` rises to 0.140 against 0.022 at `dot(N,V)` = 0.5.
fn the_brdf_table_loses_energy_with_roughness_and_gains_bias_at_grazing() {
	let samples = environment::PREFILTER_SAMPLES;
	let smooth = environment::brdf_integration(1.0, pbr::MIN_ROUGHNESS, samples);
	let rough = environment::brdf_integration(1.0, 1.0, samples);
	assert!(rough.0 < smooth.0, "a rougher surface reflects less of F0: {} against {}", rough.0, smooth.0);
	assert!((rough.0 - 0.307).abs() < 0.02, "and by the amount the profile's terms give, got {}", rough.0);

	let grazing = environment::brdf_integration(0.1, 0.5, samples);
	let facing = environment::brdf_integration(0.5, 0.5, samples);
	assert!(grazing.1 > facing.1, "Fresnel rises at grazing, so the bias does: {} against {}", grazing.1, facing.1);
	assert!((grazing.1 - 0.140).abs() < 0.02, "by the amount the profile's terms give, got {}", grazing.1);

	// ENERGY IS NOT CREATED ANYWHERE IN THE TABLE, which is the invariant that holds over the whole
	// of it rather than at the four points above.
	for step in 0..8u32 {
		let n_dot_v = (step as f32 + 0.5) / 8.0;
		for rough_step in 0..8u32 {
			let roughness = (rough_step as f32 + 0.5) / 8.0;
			let (scale, bias) = environment::brdf_integration(n_dot_v, roughness, 256);
			assert!(scale >= 0.0 && bias >= 0.0, "neither channel goes negative at ({n_dot_v}, {roughness})");
			assert!(scale + bias <= 1.0 + 1e-3, "and a surface reflects no more than it receives at ({n_dot_v}, {roughness}): {scale} + {bias}");
		}
	}
}

#[test]
// THE PREFILTER DESCENDS TO 8x8 AND NO FURTHER, because below that the filter is wider than the face
// and the result is the average of the whole environment anyway. A 256-texel face therefore has
// SIX levels, which is exactly the profile's `environment_prefilter_levels` minimum.
fn the_prefilter_has_the_levels_the_profile_guarantees() {
	assert_eq!(environment::prefilter_levels(256), 6, "256 down to 8 is six levels");
	assert_eq!(environment::prefilter_levels(8), 1, "a face already at the floor has one");
	assert_eq!(environment::prefilter_levels(4), 0, "and one below it has none");
	assert_eq!(environment::prefilter_levels(256), Limits::EXTENDED_MINIMUM.environment_prefilter_levels, "the code and the profile's limit agree");
	// LEVEL `i` HOLDS `roughness = i / (levels - 1)`: the first is a mirror and the last is fully
	// rough, with the steps evenly spaced in the PERCEPTUAL value.
	assert_eq!(environment::prefilter_roughness(0, 6), 0.0);
	assert_eq!(environment::prefilter_roughness(5, 6), 1.0);
	assert!((environment::prefilter_roughness(3, 6) - 0.6).abs() < 1e-6);
	assert_eq!(environment::prefilter_roughness(0, 1), 0.0, "a single level is a mirror and not a division by zero");
}

#[test]
// PREFILTERING A CONSTANT ENVIRONMENT RETURNS THAT CONSTANT, at every roughness and in every
// direction. It is the prefilter's own white furnace: any correctly weighted average of one value
// is that value, so a wrong weight, a missing normalisation or a sample left out of the divisor all
// show up here and nowhere else in a single image.
fn prefiltering_a_uniform_environment_returns_it_unchanged() {
	let sky = Vec3::new(2.0, 3.0, 4.0);
	for roughness in [0.0f32, 0.25, 0.5, 1.0] {
		for direction in [Vec3::new(0.0, 0.0, 1.0), Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.577, 0.577, 0.577)] {
			let filtered = environment::prefilter_direction(direction, roughness, 256, |_, _| sky);
			assert!(filtered.sub(sky).length() < 1e-4, "a uniform sky prefilters to itself at roughness {roughness}, got {filtered:?}");
		}
	}
	// AND THE SAMPLE'S OWN SOLID ANGLE REACHES THE SOURCE, which is what the profile requires so a
	// bright texel the lobe barely touches is taken from a coarser mip rather than becoming a
	// firefly. A rougher lobe spreads its samples, so each covers more.
	let mut widest_at_mirror = 0.0f32;
	let _ = environment::prefilter_direction(Vec3::new(0.0, 0.0, 1.0), 0.05, 256, |_, solid_angle| {
		if solid_angle > widest_at_mirror {
			widest_at_mirror = solid_angle;
		}
		sky
	});
	let mut widest_at_rough = 0.0f32;
	let _ = environment::prefilter_direction(Vec3::new(0.0, 0.0, 1.0), 1.0, 256, |_, solid_angle| {
		if solid_angle > widest_at_rough {
			widest_at_rough = solid_angle;
		}
		sky
	});
	assert!(widest_at_rough > widest_at_mirror, "a rough lobe's samples cover more sky: {widest_at_rough} against {widest_at_mirror}");
}

#[test]
// THE IMPORTANCE SAMPLE IS THE NORMAL AT THE CENTRE OF THE LOBE AND SPREADS WITH ROUGHNESS, and the
// basis around the normal is orthonormal even where the usual cross product degenerates.
fn the_ggx_importance_sample_is_centred_on_the_normal_and_spreads_with_roughness() {
	let normal = Vec3::new(0.0, 0.0, 1.0);
	// `u2 = 0` is the centre of the distribution: `cos(theta)` is exactly 1 whatever the roughness.
	for roughness in [0.05f32, 0.5, 1.0] {
		let centre = environment::importance_sample_ggx((0.0, 0.0), roughness * roughness, normal);
		assert!(centre.sub(normal).length() < 1e-5, "the first sample is the normal itself, got {centre:?}");
	}
	// AND A ROUGHER LOBE REACHES FURTHER FROM IT.
	let near = environment::importance_sample_ggx((0.0, 0.9), 0.05 * 0.05, normal).z;
	let far = environment::importance_sample_ggx((0.0, 0.9), 1.0, normal).z;
	assert!(far < near, "a rough lobe tilts further from the normal: {far} against {near}");
	// THE BASIS IS ORTHONORMAL EVEN AT THE POLES, where crossing with `+z` gives a zero vector - a
	// NaN after normalisation, and a black texel at exactly one direction per cube face.
	for axis in [Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, 0.0, -1.0), Vec3::new(1.0, 0.0, 0.0)] {
		let (tangent, bitangent) = environment::orthonormal_basis(axis);
		assert!(tangent.is_finite() && bitangent.is_finite(), "the basis at {axis:?} is finite");
		assert!(tangent.dot(axis).abs() < 1e-5, "the tangent is perpendicular to the normal at {axis:?}");
		assert!(bitangent.dot(axis).abs() < 1e-5, "and so is the bitangent");
		assert!((tangent.length() - 1.0).abs() < 1e-5 && (bitangent.length() - 1.0).abs() < 1e-5, "and both are unit at {axis:?}");
	}
}

#[test]
// HAMMERSLEY IS THE SEQUENCE THIS IMPLEMENTATION USES AND NOT ONE THE PROFILE REQUIRES. The
// profile's tolerance for an environment lookup says as much - the prefilter's residual noise is the
// largest term in the comparison - so a conforming renderer may use another. What this fixture holds
// is that ours is the sequence it claims to be.
fn the_sample_sequence_is_hammersley_and_stays_inside_the_unit_square() {
	assert_eq!(environment::hammersley(0, 1024), (0.0, 0.0), "the first point is the origin");
	// The radical inverse in base 2: index 1 reverses to the top bit, which is a half.
	assert!((environment::hammersley(1, 1024).1 - 0.5).abs() < 1e-6);
	assert!((environment::hammersley(2, 1024).1 - 0.25).abs() < 1e-6);
	assert!((environment::hammersley(3, 1024).1 - 0.75).abs() < 1e-6);
	for index in 0..1024u32 {
		let (first, second) = environment::hammersley(index, 1024);
		assert!((0.0..1.0).contains(&first) && (0.0..1.0).contains(&second), "sample {index} is inside the unit square: {first}, {second}");
	}
	assert_eq!(environment::hammersley(0, 0), (0.0, 0.0), "and a count of zero is answered rather than divided by");
}

// ---------------------------------------------------------------------------------------------
// Shadows: `Scene3D Extended Profile 1`'s `shadows` group. The bias, the filter and the cascades.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_directional_cascade_is_fitted_to_its_slice_s_bounding_sphere() {
	// A SPHERE AND NOT THE SLICE'S BOX, which is the profile's rule and the reason the shadow edge
	// does not crawl when the camera turns: a box fitted to the slice changes SIZE with the view's
	// orientation and a sphere does not.
	//
	// The light points straight down, the sphere is the unit sphere at the origin, so the eye is one
	// radius up at (0, 1, 0), near is 0 at the eye and far is the diameter. Every expected value
	// below is worked out from that rather than read back from the matrix.
	let down = Vec3::new(0.0, -1.0, 0.0);
	let at = shadow::directional_projection(down, Vec3::new(0.0, 0.0, 0.0), 1.0).expect("a unit sphere under a downward light");

	// The sphere's centre sits at the middle of the depth range: near 0, far 2, so half of it.
	let centre = at.transform_point(Vec3::new(0.0, 0.0, 0.0));
	assert!((centre.z - 0.5).abs() < 1e-5, "the centre is half way between the near and far planes, got {}", centre.z);
	assert!(centre.x.abs() < 1e-5 && centre.y.abs() < 1e-5, "and at the middle of the map");

	// A point one radius BELOW the centre is at the far plane; one radius above is at the near one.
	let bottom = at.transform_point(Vec3::new(0.0, -1.0, 0.0));
	let top = at.transform_point(Vec3::new(0.0, 1.0, 0.0));
	assert!((bottom.z - 1.0).abs() < 1e-5, "the far side of the sphere is at the far plane, got {}", bottom.z);
	assert!(top.z.abs() < 1e-5, "and the near side at the near plane, got {}", top.z);

	// THE VOLUME IS EXACTLY THE DIAMETER ACROSS, so the sphere's equator lands on the map's edges
	// rather than inside them - a volume any wider is texels spent on nothing.
	let east = at.transform_point(Vec3::new(1.0, 0.0, 0.0));
	assert!((east.x.abs() - 1.0).abs() < 1e-5, "one radius sideways is at the edge of the map, got {}", east.x);

	// AND THE UP AXIS FALLS BACK when the light points ALONG +Y, which is the case a `look_at` has
	// no basis for at all: the failure there is a matrix of NaNs and not a wrong picture.
	let straight = shadow::directional_projection(Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 0.0), 1.0).expect("a light pointing along the up axis still has a projection");
	assert!(straight.is_finite(), "the fallback up axis is what keeps it finite");
}

#[test]
fn a_directional_cascade_refuses_what_it_cannot_fit() {
	// NAMED RATHER THAN CLAMPED. A radius of zero is a slice with no volume and a direction of zero
	// is a light that points nowhere; both are arithmetic that cannot be done, and answering with a
	// matrix would answer a question nobody asked.
	let down = Vec3::new(0.0, -1.0, 0.0);
	assert!(matches!(shadow::directional_projection(down, Vec3::new(0.0, 0.0, 0.0), 0.0), Err(Error::Degenerate { .. })), "a cascade with no radius");
	assert!(matches!(shadow::directional_projection(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 0.0), 1.0), Err(Error::Degenerate { .. })), "a light with no direction");
	assert!(matches!(shadow::directional_projection(down, Vec3::new(f32::NAN, 0.0, 0.0), 1.0), Err(Error::Degenerate { .. })), "a centre that is not a point");
}

#[test]
fn a_spot_light_s_frustum_is_twice_its_cone_and_not_its_cone() {
	// HALVING IT ONCE TOO OFTEN is the defect this holds: a cone's half-angle is measured from its
	// axis and a perspective's field of view across the whole frustum, so a map built with the cone
	// angle covers the MIDDLE of the cone and everything outside reads as unshadowed - a shadow that
	// ends in mid-air.
	//
	// A 45-degree outer cone means a 90-degree frustum, whose edge ray at 45 degrees from the axis
	// lands exactly on the edge of the map. Worked out from the angle, not from the matrix.
	let quarter = core::f32::consts::FRAC_PI_4;
	let at = shadow::spot_projection(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, -1.0), quarter, 1.0, 100.0).expect("a spot light with a 45 degree outer cone");
	let edge = at.transform_point(Vec3::new(10.0, 0.0, -10.0));
	assert!(((edge.x / edge.w).abs() - 1.0).abs() < 1e-4, "the ray at the cone's outer angle is at the edge of the map, got {}", edge.x / edge.w);
	let inside = at.transform_point(Vec3::new(5.0, 0.0, -10.0));
	assert!((inside.x / inside.w).abs() < 1.0, "and a ray inside the cone is inside the map");

	assert!(matches!(shadow::spot_projection(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, -1.0), quarter, 1.0, 1.0), Err(Error::Degenerate { .. })), "a range that does not reach past the near plane");
	assert!(matches!(shadow::spot_projection(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 0.0), quarter, 1.0, 100.0), Err(Error::Degenerate { .. })), "a spot with no direction");
}

#[test]
fn the_six_cube_faces_tile_the_sphere_with_no_gap_and_no_overlap() {
	// NINETY DEGREES EXACTLY. A wider field spends texels on what the neighbouring face already has;
	// a narrower one leaves a band around every edge that no face covers, which reads as a
	// cross-shaped seam of unshadowed surface radiating from the light.
	//
	// The test is the tiling itself: a direction is sent to its face by the profile's major-axis
	// rule, and that face's own projection must put it INSIDE the map. A gap would be a direction
	// whose own face does not cover it.
	let position = Vec3::new(1.0, 2.0, 3.0);
	for direction in [
		Vec3::new(1.0, 0.0, 0.0),
		Vec3::new(-1.0, 0.0, 0.0),
		Vec3::new(0.0, 1.0, 0.0),
		Vec3::new(0.0, -1.0, 0.0),
		Vec3::new(0.0, 0.0, 1.0),
		Vec3::new(0.0, 0.0, -1.0),
		// And the awkward ones: just inside a face, and along an edge between two.
		Vec3::new(0.99, 0.98, 0.0),
		Vec3::new(1.0, 1.0, 0.0),
		Vec3::new(-0.9, 0.2, 0.89),
	] {
		let face = shadow::cube_face(direction).expect("every non-zero direction falls on a face");
		let at = shadow::point_face_projection(position, face, 0.1, 50.0).expect("a face of a point light's cube");
		let clip = at.transform_point(position.add(direction.scale(10.0)));
		assert!(clip.w > 0.0, "a point on the face is in front of it, {direction:?}");
		let (x, y) = (clip.x / clip.w, clip.y / clip.w);
		assert!(x.abs() <= 1.0 + 1e-4 && y.abs() <= 1.0 + 1e-4, "and inside the face's own map, {direction:?} landed at ({x}, {y})");
	}

	assert!(matches!(shadow::point_face_projection(position, shadow::CubeFace::PositiveX, 0.0, 50.0), Err(Error::Degenerate { .. })), "a near plane at zero");
	assert!(matches!(shadow::point_face_projection(position, shadow::CubeFace::PositiveX, 1.0, 0.5), Err(Error::Degenerate { .. })), "a range inside the near plane");
}

#[test]
// THE BIAS IS RELATIVE TO THE DEPTH'S OWN PRECISION, which is what `r = 2^(exponent(z) - 23)` in the
// 3D profile's equation means. A constant bias in absolute units is far too small near the camera
// and far too large at the far plane, and a scene tuned at one distance acnes at the other.
fn the_depth_bias_scales_with_the_float_s_own_step() {
	// At a depth of 1 the exponent is 0, so the step is `2^-23`.
	let step_at_one = shadow::representable_step(1.0);
	assert!((step_at_one - (2.0f32).powi(-23)).abs() < 1e-12, "the step at 1.0 is 2^-23, got {step_at_one}");
	// At 1024 the exponent is 10, so the step is 2^-13 - a thousand times larger, which is exactly
	// the ratio a fixed bias gets wrong.
	let step_far = shadow::representable_step(1024.0);
	assert!((step_far / step_at_one - 1024.0).abs() < 1.0, "the step grows with the depth, got a ratio of {}", step_far / step_at_one);
	assert_eq!(shadow::representable_step(f32::NAN), 0.0, "a non-finite depth has no step");
	assert!(shadow::representable_step(0.0) > 0.0, "and zero has the smallest one rather than none");

	// THE SLOPE TERM IS WHAT GROWS ON A SURFACE SEEN EDGE-ON TO THE LIGHT, where one texel spans a
	// long way along the surface and acne appears first.
	let flat = shadow::depth_bias(1.0, 0.0);
	let steep = shadow::depth_bias(1.0, 0.001);
	assert!(steep > flat, "a sloped surface gets more bias: {steep} against {flat}");
	assert!((steep - (shadow::BIAS_CONSTANT * step_at_one + shadow::BIAS_SLOPE * 0.001)).abs() < 1e-9, "and by the profile's equation exactly, got {steep}");
	// CLAMPED, BECAUSE A SLOPE CAN BE ENORMOUS: a polygon almost parallel to the light has an
	// unbounded derivative, and an unbounded bias detaches the shadow from what casts it.
	assert_eq!(shadow::depth_bias(1.0, 1000.0), shadow::BIAS_CLAMP, "the bias is bounded whatever the slope");
}

#[test]
// COMPARE THEN FILTER, NOT FILTER THEN COMPARE. Averaging nine depths and comparing once gives a
// wrong answer at every depth discontinuity - the mean of a near and a far occluder is a depth
// neither of them has - and the difference is a halo around every silhouette.
//
// WORKED BY HAND. A receiver at 0.5 against a map where four of the nine taps hold 0.9 (behind it,
// so lit) and five hold 0.1 (in front, so shadowed) is lit in four ninths. Filtering first would
// average to 0.456, compare once, and call the whole footprint SHADOWED.
fn the_percentage_closer_filter_compares_before_it_averages() {
	let lit = shadow::percentage_closer(0.5, (0.1, 0.1), (0.5, 0.5), |u, _| if u > 0.5 { 0.9 } else { 0.1 });
	assert!((lit - 3.0 / 9.0).abs() < 1e-6, "three of the nine taps are behind the receiver, got {lit}");
	// The naive order: the mean of those nine depths is below the receiver, so a single comparison
	// would answer zero - a different picture, not a rounding difference.
	let mean = (3.0 * 0.9 + 6.0 * 0.1) / 9.0;
	assert!(mean < 0.5, "and filtering first would have called the whole footprint shadowed");

	// EQUAL WEIGHTS, so the answer is one of ten values and a tap at the corner counts as much as
	// the one at the centre.
	let all_lit = shadow::percentage_closer(0.0, (0.1, 0.1), (0.5, 0.5), |_, _| 1.0);
	let none = shadow::percentage_closer(1.0, (0.1, 0.1), (0.5, 0.5), |_, _| 0.0);
	assert_eq!(all_lit, 1.0);
	assert_eq!(none, 0.0);
	let corner_only = shadow::percentage_closer(0.5, (0.1, 0.1), (0.5, 0.5), |u, v| if u < 0.45 && v < 0.45 { 0.9 } else { 0.1 });
	assert!((corner_only - 1.0 / 9.0).abs() < 1e-6, "one corner tap is one ninth, got {corner_only}");
	// AND `LessOrEqual`, the 3D profile's own order: a surface at exactly its own depth in the map
	// is LIT, which is what keeps a caster from shadowing itself.
	assert_eq!(shadow::percentage_closer(0.5, (0.1, 0.1), (0.5, 0.5), |_, _| 0.5), 1.0, "equal depth is lit");
}

#[test]
// THE SPLITS ARE A BLEND OF THE UNIFORM AND LOGARITHMIC SCHEMES AT LAMBDA 0.5. Logarithmic alone
// puts almost every texel near the camera; uniform alone wastes the near cascades on geometry that
// occupies few pixels. The profile fixes the blend rather than the scheme.
//
// WORKED BY HAND for near 1, far 100, four cascades:
//   logarithmic at i/n:  100^0.25 = 3.1623, 100^0.5 = 10, 100^0.75 = 31.623, 100
//   uniform at i/n:      1 + 99 * 0.25 = 25.75, 50.5, 75.25, 100
//   blended at 0.5:      14.456, 30.25, 53.436, 100
fn the_cascade_splits_blend_the_uniform_and_logarithmic_schemes() {
	let splits = shadow::split_distances(1.0, 100.0, 4, shadow::CASCADE_SPLIT_LAMBDA, &extended()).expect("four cascades");
	assert_eq!(splits.len(), 4);
	for (index, expected) in [(0usize, 14.456f32), (1, 30.25), (2, 53.436), (3, 100.0)] {
		assert!((splits[index] - expected).abs() < 0.05, "split {index} is {expected}, got {}", splits[index]);
	}
	// THE LAST SPLIT IS THE FAR PLANE EXACTLY, so a fragment at the far plane does not fall off the
	// end of the last cascade by a few bits.
	assert_eq!(splits[3], 100.0);
	// AND THE SPLITS ASCEND, which is what makes `cascade_for`'s first-match walk correct.
	for pair in splits.windows(2) {
		assert!(pair[1] > pair[0], "the splits ascend: {pair:?}");
	}
	// LAMBDA PICKS BETWEEN THE TWO SCHEMES AND THE ENDS ARE THE SCHEMES THEMSELVES.
	let uniform = shadow::split_distances(1.0, 100.0, 4, 0.0, &extended()).expect("uniform");
	assert!((uniform[0] - 25.75).abs() < 0.01, "lambda 0 is the uniform scheme, got {}", uniform[0]);
	let logarithmic = shadow::split_distances(1.0, 100.0, 4, 1.0, &extended()).expect("logarithmic");
	assert!((logarithmic[0] - 3.1623).abs() < 0.01, "lambda 1 is the logarithmic one, got {}", logarithmic[0]);
}

#[test]
// A SCENE THAT ASKS FOR MORE CASCADES THAN THE PROFILE ADMITS IS REFUSED RATHER THAN SILENTLY GIVEN
// FEWER, which is the profile's own word for it: a scene that asked for six and got four would
// render with a far distance it did not choose.
fn a_cascade_count_past_the_limit_is_refused_by_name() {
	let limits = extended();
	assert!(matches!(shadow::split_distances(1.0, 100.0, 5, 0.5, &limits), Err(Error::LimitExceeded { limit: "max_shadow_cascades", ceiling: 4, asked: 5 })));
	assert!(matches!(shadow::split_distances(1.0, 100.0, 0, 0.5, &limits), Err(Error::Degenerate { .. })));
	assert!(matches!(shadow::split_distances(0.0, 100.0, 4, 0.5, &limits), Err(Error::Degenerate { .. })));
	assert!(matches!(shadow::split_distances(100.0, 100.0, 4, 0.5, &limits), Err(Error::Degenerate { .. })));
	assert!(matches!(shadow::split_distances(1.0, 100.0, 4, 1.5, &limits), Err(Error::Degenerate { .. })));
	// AND A CORE SCENE HAS NO CASCADES AT ALL, because it never claimed the part.
	assert!(matches!(shadow::split_distances(1.0, 100.0, 1, 0.5, &Limits::PROFILE_MINIMUM), Err(Error::LimitExceeded { limit: "max_shadow_cascades", ceiling: 0, asked: 1 })));
}

#[test]
// THE CASCADE IS CHOSEN BY THE FRAGMENT'S OWN VIEW-SPACE DEPTH, and WHAT IS BEYOND THE LAST ONE IS
// UNSHADOWED. Extending the last cascade to infinity makes its texels useless everywhere; saying
// where the shadows stop lets a scene choose its far distance.
fn a_fragment_beyond_the_last_cascade_is_unshadowed() {
	let splits = shadow::split_distances(1.0, 100.0, 4, 0.5, &extended()).expect("four cascades");
	assert_eq!(shadow::cascade_for(2.0, &splits), Some(0), "near geometry is in the first cascade");
	assert_eq!(shadow::cascade_for(20.0, &splits), Some(1));
	assert_eq!(shadow::cascade_for(40.0, &splits), Some(2));
	assert_eq!(shadow::cascade_for(90.0, &splits), Some(3));
	assert_eq!(shadow::cascade_for(100.0, &splits), Some(3), "the far plane itself is still in the last cascade");
	assert_eq!(shadow::cascade_for(100.1, &splits), None, "and past it there is no cascade, which means unshadowed");
	assert_eq!(shadow::cascade_for(f32::NAN, &splits), None);
}

#[test]
// THE SEAM BETWEEN CASCADES IS A GRADIENT OVER THE LAST TENTH OF EACH RANGE. Without it the
// resolution change is a visible edge across the ground at a fixed distance from the camera, which
// moves with the camera and reads as a fault in the world rather than as a shadow technique.
//
// WORKED BY HAND. Cascade 0 runs from the near plane at 1 to 14.456, a range of 13.456; its last
// tenth begins at 14.456 - 1.3456 = 13.110. So 13.0 is wholly inside, 13.783 is halfway through the
// transition, and 14.456 is wholly in the next.
fn the_transition_between_cascades_is_a_blend_and_not_a_line() {
	let near = 1.0f32;
	let splits = shadow::split_distances(near, 100.0, 4, 0.5, &extended()).expect("four cascades");
	assert_eq!(shadow::cascade_blend(13.0, near, &splits, 0), 0.0, "before the last tenth there is no blend");
	let halfway = shadow::cascade_blend(13.783, near, &splits, 0);
	assert!((halfway - 0.5).abs() < 0.02, "halfway through the transition is 0.5, got {halfway}");
	assert!((shadow::cascade_blend(14.456, near, &splits, 0) - 1.0).abs() < 1e-3, "at the split it is wholly the next cascade");
	// THE LAST CASCADE BLENDS INTO NOTHING, because what is beyond it is unshadowed rather than
	// another map - and fading into no shadow is what unshadowed already looks like.
	assert_eq!(shadow::cascade_blend(99.9, near, &splits, 3), 0.0, "the last cascade has nothing to blend into");
	assert_eq!(shadow::cascade_blend(50.0, near, &splits, 9), 0.0, "and a cascade that does not exist blends nothing");
}

#[test]
// A POINT LIGHT'S SHADOW IS A CUBE, ITS FACE CHOSEN BY THE MAJOR AXIS, in the 3D profile's index
// order - so a cube built for any other system loads without a flip.
fn a_point_light_s_cube_face_is_the_major_axis() {
	for (direction, face) in [
		(Vec3::new(2.0, 1.0, 1.0), CubeFace::PositiveX),
		(Vec3::new(-2.0, 1.0, 1.0), CubeFace::NegativeX),
		(Vec3::new(1.0, 2.0, 1.0), CubeFace::PositiveY),
		(Vec3::new(1.0, -2.0, 1.0), CubeFace::NegativeY),
		(Vec3::new(1.0, 1.0, 2.0), CubeFace::PositiveZ),
		(Vec3::new(1.0, 1.0, -2.0), CubeFace::NegativeZ),
	] {
		assert_eq!(shadow::cube_face(direction), Some(face), "{direction:?} falls on {face:?}");
	}
	// A TIE GOES TO THE EARLIER AXIS, so a direction exactly on a face edge picks one face rather
	// than depending on which comparison the compiler evaluated first.
	assert_eq!(shadow::cube_face(Vec3::new(1.0, 1.0, 0.0)), Some(CubeFace::PositiveX));
	assert_eq!(shadow::cube_face(Vec3::new(0.0, 1.0, 1.0)), Some(CubeFace::PositiveY));
	// AND A DIRECTION THAT IS NOT ONE IS ANSWERED RATHER THAN GUESSED AT.
	assert_eq!(shadow::cube_face(Vec3::new(0.0, 0.0, 0.0)), None);
	assert_eq!(shadow::cube_face(Vec3::new(f32::NAN, 0.0, 0.0)), None);
}

// ---------------------------------------------------------------------------------------------
// HDR and post-processing: `Scene3D Extended Profile 1`'s `postprocess` group.
// ---------------------------------------------------------------------------------------------

#[test]
// THE LUMINANCE IS THE IMAGE-COLOUR PROFILE'S OWN TRIPLE, and its coefficients sum to exactly one -
// which is what makes white have a luminance of one and is the cheapest check that none of the
// three has been mistyped.
fn the_luminance_is_linear_rec_709_and_white_is_one() {
	let sum = postprocess::LUMINANCE_REC709.x + postprocess::LUMINANCE_REC709.y + postprocess::LUMINANCE_REC709.z;
	assert!((sum - 1.0).abs() < 1e-6, "the coefficients sum to one, got {sum}");
	assert!((postprocess::luminance(Vec3::new(1.0, 1.0, 1.0)) - 1.0).abs() < 1e-6);
	assert!((postprocess::luminance(Vec3::new(0.0, 1.0, 0.0)) - 0.7152).abs() < 1e-6, "green carries most of it");
	assert_eq!(postprocess::luminance(Vec3::new(0.0, 0.0, 0.0)), 0.0);
}

#[test]
// EXTENDED REINHARD MAPS THE PROFILE'S WHITE TO EXACTLY ONE, which is the property that makes
// `WHITE` mean what its name says: the luminance the operator sends to display white.
//
// WORKED BY HAND at `WHITE` = 4 and `KNEE` = 0.8. The shoulder's own white point is
// `(4 - 0.8) / (1 - 0.8)` = 16, and extended Reinhard sends its white point to one -
// `16 * (1 + 16/256) / 17` = 1 - so `0.8 + 0.2 * 1` = 1. Below 0.8 there is no arithmetic at all.
fn the_tone_map_sends_the_profile_s_white_to_one_and_leaves_the_rest_alone() {
	let white = postprocess::tone_map_white();
	assert_eq!(white, 4.0, "the white point comes from the image-colour profile");
	let at_white = postprocess::tone_map(Vec3::new(white, white, white));
	assert!((at_white.x - 1.0).abs() < 1e-5, "the white point maps to one, got {}", at_white.x);
	// BELOW THE KNEE IT IS THE IDENTITY, BIT FOR BIT. That is what a compositor needs from it, and
	// "close enough" is a different claim.
	for value in [0.0f32, 0.125, 0.5, 0.799] {
		assert_eq!(postprocess::tone_map(Vec3::new(value, value, value)).x, value, "the identity below the knee at {value}");
	}
	// AND ABOVE IT THE SHOULDER RUNS, worked by hand: at a luminance of one the shoulder's input is
	// `(1 - 0.8) / 0.2` = 1, `1 * (1 + 1/256) / 2` = 0.501953, and `0.8 + 0.2 * 0.501953` = 0.900391.
	let at_one = postprocess::tone_map(Vec3::new(1.0, 1.0, 1.0));
	assert!((at_one.x - 0.900_391).abs() < 1e-5, "the curve at one is 0.900391, got {}", at_one.x);
	// AND A HIGHLIGHT KEEPS ITS ORDER rather than flattening, which is the difference between a
	// bright window and a white rectangle: 0.975446 at two times white and 0.991211 at three.
	let two = postprocess::tone_map(Vec3::new(2.0, 2.0, 2.0)).x;
	let three = postprocess::tone_map(Vec3::new(3.0, 3.0, 3.0)).x;
	assert!((two - 0.975_446).abs() < 1e-5, "two times white is 0.975446, got {two}");
	assert!((three - 0.991_211).abs() < 1e-5, "three times white is 0.991211, got {three}");
	assert!(two < three && three < 1.0, "and they are ordered and inside the range");
	assert_eq!(postprocess::tone_map(Vec3::new(0.0, 0.0, 0.0)), Vec3::new(0.0, 0.0, 0.0), "black stays black");
	// AND THERE IS NO STEP ANYWHERE, INCLUDING AT THE KNEE, which is the join a knee could have
	// introduced: the shoulder meets the identity with the SAME SLOPE, because extended Reinhard has
	// slope one at zero.
	let below = postprocess::tone_map(Vec3::new(0.9999, 0.9999, 0.9999)).x;
	let above = postprocess::tone_map(Vec3::new(1.0001, 1.0001, 1.0001)).x;
	assert!((above - below).abs() < 1e-3, "the curve is continuous at one: {below} then {above}");
	let under = postprocess::tone_map(Vec3::new(0.7999, 0.7999, 0.7999)).x;
	let over = postprocess::tone_map(Vec3::new(0.8001, 0.8001, 0.8001)).x;
	assert!((over - under).abs() < 1e-3 && over > under, "and at the knee: {under} then {over}");
	// AND IT IS MONOTONIC, so a brighter input is never a darker output.
	let mut previous = 0.0f32;
	for step in 0..64u32 {
		let value = step as f32 * 0.5;
		let mapped = postprocess::tone_map(Vec3::new(value, value, value)).x;
		assert!(mapped >= previous - 1e-6, "the curve does not go backwards at {value}: {mapped} after {previous}");
		previous = mapped;
	}
	// ON LUMINANCE AND NOT PER CHANNEL: the hue survives, so the ratio between channels is the same
	// after mapping as before. A per-channel curve shifts it most on saturated colours.
	let saturated = Vec3::new(8.0, 2.0, 1.0);
	let mapped = postprocess::tone_map(saturated);
	assert!((mapped.x / mapped.y - saturated.x / saturated.y).abs() < 1e-4, "the hue is unchanged, got {mapped:?}");
	// A NaN FALLS THROUGH UNMAPPED rather than being scaled by a NaN ratio, which would spread one
	// bad pixel across the whole frame at the next downsample.
	assert!(postprocess::tone_map(Vec3::new(f32::NAN, 0.0, 0.0)).x.is_nan());
}

#[test]
// THE BLOOM KNEE IS SOFT, AND ITS QUADRATIC JOINS SMOOTHLY AT BOTH ENDS. A hard threshold makes a
// specular glint drifting across a surface cross it in one frame and bloom at full strength, which
// reads as a flash rather than as a highlight.
//
// WORKED BY HAND with threshold 1 and knee 0.5. The quadratic is `(L - 0.5)^2 / 2`:
//   at L = 0.5  it is 0, with zero slope - so it joins the nothing below it
//   at L = 1.0  it is 0.125
//   at L = 1.5  it is 0.5, which is exactly `L - 1` - so it joins the excess above it
fn the_bloom_threshold_is_a_knee_that_joins_at_both_ends() {
	let grey = |light: f32| Vec3::new(light, light, light);
	assert_eq!(postprocess::bloom_prefilter(grey(0.4)), Vec3::new(0.0, 0.0, 0.0), "below the knee nothing blooms");
	assert_eq!(postprocess::bloom_prefilter(grey(0.5)), Vec3::new(0.0, 0.0, 0.0), "and the knee starts at zero");
	let at_one = postprocess::bloom_prefilter(grey(1.0)).x;
	assert!((at_one - 0.125).abs() < 1e-5, "at the threshold the quadratic gives 0.125, got {at_one}");
	let at_knee_end = postprocess::bloom_prefilter(grey(1.5)).x;
	assert!((at_knee_end - 0.5).abs() < 1e-5, "at the far end it is the excess itself, got {at_knee_end}");
	// THE TWO BRANCHES MEET, which is what the knee is for: a thousandth more light either side of
	// the join changes the answer by a thousandth rather than by a step.
	let just_below = postprocess::bloom_prefilter(grey(1.499)).x;
	let just_past = postprocess::bloom_prefilter(grey(1.501)).x;
	assert!((just_past - just_below).abs() < 3e-3, "the branches join: {just_below} then {just_past}");
	let far_above = postprocess::bloom_prefilter(grey(10.0)).x;
	assert!((far_above - 9.0).abs() < 1e-4, "above the knee the whole excess blooms, got {far_above}");
	// THE HUE SURVIVES: the excess is a fraction of the luminance and scales all three channels,
	// rather than each channel being thresholded on its own.
	let coloured = Vec3::new(4.0, 1.0, 0.5);
	let bloomed = postprocess::bloom_prefilter(coloured);
	assert!((bloomed.x / bloomed.y - coloured.x / coloured.y).abs() < 1e-4, "the bloom keeps the colour's hue");
	assert_eq!(postprocess::bloom_prefilter(Vec3::new(f32::NAN, 0.0, 0.0)), Vec3::new(0.0, 0.0, 0.0), "a NaN contributes nothing");
}

#[test]
// BOTH PYRAMID KERNELS PARTITION UNITY, which is the invariant that catches a mistyped weight, a
// missing tap and a wrong divisor at once: a constant field must downsample and upsample to itself,
// or the bloom brightens or darkens every frame it is added to.
fn the_bloom_kernels_preserve_a_constant() {
	let constant = Vec3::new(0.25, 0.5, 0.75);
	let down = postprocess::downsample_13((0.5, 0.5), (0.01, 0.01), |_, _| constant);
	assert!(down.sub(constant).length() < 1e-6, "the 13-tap downsample preserves a constant, got {down:?}");
	let up = postprocess::upsample_tent_9((0.5, 0.5), (0.01, 0.01), |_, _| constant);
	assert!(up.sub(constant).length() < 1e-6, "and so does the 9-tap tent, got {up:?}");
	// THE TENT IS WEIGHTED TOWARD ITS CENTRE, which is what makes it a tent rather than a box: a
	// single bright centre texel contributes four sixteenths and a corner one sixteenth.
	let centre_only = postprocess::upsample_tent_9((0.5, 0.5), (0.1, 0.1), |x, y| if (x - 0.5).abs() < 0.01 && (y - 0.5).abs() < 0.01 { Vec3::new(16.0, 16.0, 16.0) } else { Vec3::new(0.0, 0.0, 0.0) });
	assert!((centre_only.x - 4.0).abs() < 1e-5, "the centre carries four sixteenths, got {}", centre_only.x);
	let corner_only = postprocess::upsample_tent_9((0.5, 0.5), (0.1, 0.1), |x, y| if x < 0.45 && y < 0.45 { Vec3::new(16.0, 16.0, 16.0) } else { Vec3::new(0.0, 0.0, 0.0) });
	assert!((corner_only.x - 1.0).abs() < 1e-5, "and a corner one sixteenth, got {}", corner_only.x);
	// THE PYRAMID IS SIX LEVELS AND IS ADDED BACK AT THE PROFILE'S WEIGHT.
	assert_eq!(postprocess::BLOOM_LEVELS, 6);
	let combined = postprocess::combine(Vec3::new(1.0, 1.0, 1.0), Vec3::new(10.0, 0.0, 0.0), postprocess::BLOOM_WEIGHT);
	assert!((combined.x - 1.4).abs() < 1e-5, "0.04 of ten is 0.4, got {}", combined.x);
}

#[test]
// FOG IS EXPONENTIAL-SQUARED, WHICH HAS NO VISIBLE START PLANE. A linear fog begins abruptly at a
// distance the author picked, and that edge sweeps across the world as the camera moves.
//
// WORKED BY HAND. `f = exp(-(density * distance)^2)`: at `density * distance` = 1 the surviving
// fraction is `exp(-1)` = 0.36788, and at 2 it is `exp(-4)` = 0.018316.
fn fog_is_exponential_squared_and_has_no_start_plane() {
	let surface = Vec3::new(1.0, 1.0, 1.0);
	let grey = Vec3::new(0.0, 0.0, 0.0);
	assert_eq!(postprocess::fog(surface, grey, 0.1, 0.0), surface, "at the camera there is no fog");
	let at_one = postprocess::fog(surface, grey, 1.0, 1.0).x;
	assert!((at_one - 0.367_879).abs() < 1e-4, "one unit of optical depth leaves exp(-1), got {at_one}");
	let at_two = postprocess::fog(surface, grey, 1.0, 2.0).x;
	assert!((at_two - 0.018_316).abs() < 1e-4, "two units leave exp(-4), got {at_two}");
	// THE SQUARE IS WHAT MAKES THE NEAR FIELD FLAT: at a tenth of the way the surface is still
	// almost untouched, which a linear fog would already have dimmed by a tenth.
	let near = postprocess::fog(surface, grey, 1.0, 0.1).x;
	assert!(near > 0.99, "the near field is flat, got {near}");
	// AND IT MIXES TOWARD THE FOG'S OWN COLOUR RATHER THAN TOWARD BLACK.
	let white_fog = postprocess::fog(Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 1.0), 1.0, 10.0);
	assert!(white_fog.x > 0.99, "far away everything is the fog colour, got {}", white_fog.x);
	assert_eq!(postprocess::fog(surface, grey, 0.0, 100.0), surface, "a density of zero is no fog at all");
	assert_eq!(postprocess::fog(surface, grey, f32::NAN, 1.0), surface, "and a degenerate density is ignored rather than answered");
}

#[test]
// TONE MAPPING IS LAST. `resolve` is the profile's order in one place, and the fixture holds the
// consequence rather than the ordering: a bright bloom added to a frame is compressed by the curve,
// which is what it is for - tone mapping first would compress the frame and then add an untouched
// bloom on top of it, which can exceed one and clip.
fn the_chain_tone_maps_after_the_bloom_is_added() {
	let scene = Vec3::new(3.0, 3.0, 3.0);
	let bloom = Vec3::new(20.0, 20.0, 20.0);
	let resolved = postprocess::resolve(scene, bloom, postprocess::BLOOM_WEIGHT);
	// 3 + 0.04 * 20 = 3.8. The shoulder's input is `(3.8 - 0.8) / 0.2` = 15, and
	// `15 * (1 + 15/256) / 16` = 0.992432, so `0.8 + 0.2 * 0.992432` = 0.998486.
	assert!((resolved.x - 0.998_486).abs() < 1e-4, "the sum is tone-mapped, got {}", resolved.x);

	// THE OTHER ORDER IS A DIFFERENT PICTURE, and by more than rounding:
	//   profile's order:  tone_map(0.5 + 0.04 * 8) = tone_map(0.82)  = 0.818189
	//   mapped first:     tone_map(0.5) + 0.04 * tone_map(8)         = 0.540878
	// The second compresses the highlight BEFORE spreading it, so what the bloom spreads has already
	// been flattened - which is what bloom exists to avoid and what the profile's order protects.
	//
	// BOTH VALUES ARE INSIDE THE RANGE ON PURPOSE. The pair this used before were 1.546875 and
	// 1.427938, and both clamp to white on any ordinary target - so the difference the assertion
	// measured was one nobody could see. A test for "a different picture" has to stay where pictures
	// are.
	let bright = Vec3::new(8.0, 8.0, 8.0);
	let dim = Vec3::new(0.5, 0.5, 0.5);
	let right_way = postprocess::resolve(dim, bright, postprocess::BLOOM_WEIGHT);
	let wrong_way = postprocess::combine(postprocess::tone_map(dim), postprocess::tone_map(bright), postprocess::BLOOM_WEIGHT);
	assert!((right_way.x - 0.818_189).abs() < 1e-4, "the profile's order gives 0.818189, got {}", right_way.x);
	assert!((wrong_way.x - 0.540_878).abs() < 1e-4, "and the other gives 0.540878, got {}", wrong_way.x);
	assert!(right_way.x - wrong_way.x > 0.1, "which is a different picture and not a rounding difference");
	assert!(right_way.x < 1.0 && wrong_way.x < 1.0, "and both are inside the range, where that difference is visible");
}

// ---------------------------------------------------------------------------------------------
// Playing a clip: the interpolation modes, the endings, and blending two clips.
// ---------------------------------------------------------------------------------------------

fn step_track(joint: u16, keys: &[(f32, Vec3)]) -> Track {
	Track { target: Target::Joint(joint), channel: Channel::Translation(Curve::Step(keys.iter().map(|(time, value)| Key { time: *time, value: *value }).collect())) }
}

fn cubic_track(joint: u16, keys: &[(f32, f32, f32, f32)]) -> Track {
	// `(time, value, in, out)` on the x axis, which is where the arithmetic is easiest to check.
	let keys = keys.iter().map(|(time, value, into, out)| CubicKey { time: *time, value: Vec3::new(*value, 0.0, 0.0), in_tangent: Vec3::new(*into, 0.0, 0.0), out_tangent: Vec3::new(*out, 0.0, 0.0) }).collect();
	Track { target: Target::Joint(joint), channel: Channel::Translation(Curve::Cubic(keys)) }
}

fn driven(clip: &Clip, time: f32, joint: u16) -> Vec3 {
	clip.sample(time).joint(joint).expect("the joint is driven").translation.expect("a translation")
}

#[test]
// STEP HOLDS THE PREVIOUS KEY UNTIL THE NEXT KEY'S TIME IS REACHED, and the value AT a key is that
// key's own - so a step track changes exactly at its keyframes and nowhere else, which is what a
// discrete channel means. A visibility flag interpolated linearly is half-visible for half a second.
fn a_step_track_changes_only_at_its_keyframes() {
	let track = step_track(0, &[(0.0, Vec3::new(1.0, 0.0, 0.0)), (2.0, Vec3::new(5.0, 0.0, 0.0))]);
	let clip = Clip::new(vec![track], 2.0, Ending::Clamp, 0, true, &extended()).expect("a step clip");
	for time in [0.0f32, 0.5, 1.0, 1.9999] {
		assert_eq!(driven(&clip, time, 0).x, 1.0, "at {time} the previous key still holds");
	}
	assert_eq!(driven(&clip, 2.0, 0).x, 5.0, "and at the next key it is that key's own value");
	assert_eq!(driven(&clip, 5.0, 0).x, 5.0, "past the end the last key holds");
}

#[test]
// THE CUBIC IS HERMITE WITH AUTHORED TANGENTS, and its shape is not the linear one.
//
// WORKED BY HAND. Keys at 0 and 1, values 0 and 1, both tangents zero: the Hermite basis collapses
// to `3t^2 - 2t^3`, so at a quarter of the way it is `3 * 0.0625 - 2 * 0.015625` = 0.15625 - where
// a linear track would be at 0.25.
fn a_cubic_track_follows_its_tangents_and_not_the_chord() {
	let flat_ends = cubic_track(0, &[(0.0, 0.0, 0.0, 0.0), (1.0, 1.0, 0.0, 0.0)]);
	let clip = Clip::new(vec![flat_ends], 1.0, Ending::Clamp, 0, true, &extended()).expect("a cubic clip");
	assert!((driven(&clip, 0.25, 0).x - 0.15625).abs() < 1e-5, "at a quarter the cubic is 0.15625, got {}", driven(&clip, 0.25, 0).x);
	assert!((driven(&clip, 0.5, 0).x - 0.5).abs() < 1e-5, "and at the midpoint it is 0.5, where the chord also is");
	assert_eq!(driven(&clip, 0.0, 0).x, 0.0, "it passes through its keys");
	assert_eq!(driven(&clip, 1.0, 0).x, 1.0);

	// THE TANGENTS ARE PER SECOND AND SCALED BY THE SPAN, which is what makes a slope mean the same
	// thing however far apart the keys are.
	//
	// WORKED BY HAND. Keys at 0 and 2, both values 0, the first with an OUT tangent of 1 and the
	// second with an IN tangent of 0. At the halfway point `t = 0.5` the only non-zero basis term is
	// `(t^3 - 2t^2 + t) * span * out` = `(0.125 - 0.5 + 0.5) * 2 * 1` = 0.25.
	let leaning = cubic_track(0, &[(0.0, 0.0, 0.0, 1.0), (2.0, 0.0, 0.0, 0.0)]);
	let wide = Clip::new(vec![leaning], 2.0, Ending::Clamp, 0, true, &extended()).expect("a cubic clip");
	assert!((driven(&wide, 1.0, 0).x - 0.25).abs() < 1e-5, "the span scales the tangent, got {}", driven(&wide, 1.0, 0).x);
	// AND THE SAME SLOPE OVER HALF THE SPAN REACHES HALF AS FAR, which is what "per second" means:
	// a velocity of one unit a second carries further when it is given longer, and that is the
	// behaviour a per-span tangent would not have.
	let narrow_track = cubic_track(0, &[(0.0, 0.0, 0.0, 1.0), (1.0, 0.0, 0.0, 0.0)]);
	let narrow = Clip::new(vec![narrow_track], 1.0, Ending::Clamp, 0, true, &extended()).expect("a cubic clip");
	assert!((driven(&narrow, 0.5, 0).x - 0.125).abs() < 1e-5, "half the span reaches half as far, got {}", driven(&narrow, 0.5, 0).x);
}

#[test]
// PING-PONG PLAYS FORWARD THEN BACKWARD WITH A PERIOD OF TWICE THE DURATION, and its turning frames
// are VISITED ONCE PER PERIOD AND NOT TWICE. A turn that held the last frame for two frames stutters
// at both ends, once a cycle, for the life of the clip.
fn ping_pong_turns_at_its_end_keys_rather_than_holding_them() {
	let keys = vec![translation_track(0, &[(0.0, Vec3::new(0.0, 0.0, 0.0)), (2.0, Vec3::new(2.0, 0.0, 0.0))])];
	let clip = Clip::new(keys, 2.0, Ending::PingPong, 0, true, &extended()).expect("a ping-pong clip");
	for (time, expected) in [(0.0f32, 0.0f32), (1.0, 1.0), (2.0, 2.0), (2.5, 1.5), (3.0, 1.0), (4.0, 0.0), (4.5, 0.5)] {
		assert!((clip.at(time) - expected).abs() < 1e-5, "ping-pong at {time} is {expected}, got {}", clip.at(time));
	}
	// THE TURN IS AT THE END KEY: just before and just after the turn the clip is at the same place,
	// which is what makes the frame appear once rather than being held across two.
	let before = clip.at(2.0 - 1e-3);
	let after = clip.at(2.0 + 1e-3);
	assert!((before - after).abs() < 1e-5, "the turn is symmetric about the end key: {before} then {after}");
	assert!(before < 2.0 && after < 2.0, "and neither side sits ON it, so it is one frame and not two");
	// A PING-PONG NEEDS NO CLOSED SEAM, because its ends meet themselves - the same keys that a
	// looping clip would be refused for.
	let open = vec![translation_track(0, &[(0.0, Vec3::new(0.0, 0.0, 0.0)), (2.0, Vec3::new(9.0, 0.0, 0.0))])];
	assert!(Clip::new(open.clone(), 2.0, Ending::PingPong, 0, true, &extended()).is_ok(), "a ping-pong may end anywhere");
	assert!(matches!(Clip::new(open, 2.0, Ending::Loop, 0, true, &extended()), Err(Error::Degenerate { .. })), "and the same clip as a loop is refused");
}

#[test]
// A CROSS-FADE IS A WEIGHTED BLEND OF TWO SAMPLED POSES, and a target only one of them drives is
// taken from it UNCHANGED. Scaling that joint by its clip's weight would pull it toward the origin
// as the blend moves away, which is a limb collapsing rather than a blend.
fn blending_two_poses_mixes_what_both_drive_and_keeps_what_only_one_does() {
	let walk = vec![
		translation_track(0, &[(0.0, Vec3::new(0.0, 0.0, 0.0)), (1.0, Vec3::new(0.0, 0.0, -2.0))]),
		translation_track(1, &[(0.0, Vec3::new(0.0, 0.0, 0.0)), (1.0, Vec3::new(10.0, 0.0, 0.0))]),
	];
	let run = vec![
		translation_track(0, &[(0.0, Vec3::new(0.0, 0.0, 0.0)), (1.0, Vec3::new(0.0, 0.0, -6.0))]),
		translation_track(2, &[(0.0, Vec3::new(0.0, 0.0, 0.0)), (1.0, Vec3::new(4.0, 0.0, 0.0))]),
	];
	let walking = Clip::new(walk, 1.0, Ending::Clamp, 0, false, &extended()).expect("a walk").sample(1.0);
	let running = Clip::new(run, 1.0, Ending::Clamp, 0, false, &extended()).expect("a run").sample(1.0);

	let quarter = blend(&walking, &running, 0.25);
	// ROOT MOTION IS THE SAME WEIGHTED SUM: a quarter of the way from -2 to -6 is -3, and NOT the
	// sum of the two, which would make the character briefly outrun both clips.
	assert!((quarter.root_delta().z + 3.0).abs() < 1e-5, "the blended speed is between the two, got {:?}", quarter.root_delta());
	// A JOINT ONLY THE WALK DRIVES ARRIVES WHOLE.
	let only_walk = quarter.joint(1).expect("driven by the walk").translation.expect("a translation");
	assert!((only_walk.x - 10.0).abs() < 1e-5, "a joint only one pose drives is unchanged, got {only_walk:?}");
	// AND SO DOES ONE ONLY THE RUN DRIVES.
	let only_run = quarter.joint(2).expect("driven by the run").translation.expect("a translation");
	assert!((only_run.x - 4.0).abs() < 1e-5, "from either side, got {only_run:?}");
	// THE ENDS ARE THE TWO POSES THEMSELVES, which is what makes a cross-fade one rule rather than
	// three: the weight moving from zero to one is the whole of it.
	assert!((blend(&walking, &running, 0.0).root_delta().z + 2.0).abs() < 1e-5);
	assert!((blend(&walking, &running, 1.0).root_delta().z + 6.0).abs() < 1e-5);
	// A ROTATION BLENDS SPHERICALLY, exactly as within one clip.
	let turning = Clip::new(vec![rotation_track(3, &[(0.0, turn(0.0)), (1.0, turn(0.0))])], 1.0, Ending::Clamp, 0, true, &extended()).expect("still").sample(0.0);
	let turned = Clip::new(vec![rotation_track(3, &[(0.0, turn(core::f32::consts::FRAC_PI_2)), (1.0, turn(core::f32::consts::FRAC_PI_2))])], 1.0, Ending::Clamp, 0, true, &extended()).expect("turned").sample(0.0);
	let half = blend(&turning, &turned, 0.5).joint(3).expect("driven").rotation.expect("a rotation");
	let spun = half.rotate(Vec3::new(1.0, 0.0, 0.0));
	let root_half = core::f32::consts::FRAC_1_SQRT_2;
	assert!(spun.sub(Vec3::new(root_half, root_half, 0.0)).length() < 1e-4, "half a quarter turn is 45 degrees, got {spun:?}");
}

#[test]
// A MORPH-WEIGHT TRACK DRIVES ONE TARGET'S WEIGHT, under the same three interpolation modes, and
// the weights are NOT normalised across targets - because morph targets are ADDITIVE, and
// normalising them would make a second expression undo half of the first.
fn a_morph_weight_track_drives_one_target_and_is_not_normalised() {
	let tracks = vec![
		Track { target: Target::Morph(3), channel: Channel::MorphWeight(Curve::Linear(vec![Key { time: 0.0, value: 0.0 }, Key { time: 1.0, value: 1.0 }])) },
		Track { target: Target::Morph(7), channel: Channel::MorphWeight(Curve::Linear(vec![Key { time: 0.0, value: 1.0 }, Key { time: 1.0, value: 1.0 }])) },
	];
	let clip = Clip::new(tracks, 1.0, Ending::Clamp, 0, true, &extended()).expect("a morph clip");
	let pose = clip.sample(0.5);
	assert!((pose.morph(3).expect("target 3 is driven") - 0.5).abs() < 1e-6, "a linear weight is halfway");
	assert!((pose.morph(7).expect("target 7 is driven") - 1.0).abs() < 1e-6, "and the other is untouched by it");
	// NOT NORMALISED: the two together sum to 1.5, and that is the point - both expressions are
	// applied, and a normalising blend would make the second undo half of the first.
	let total: f32 = pose.morphs().map(|(_, weight)| weight).sum();
	assert!((total - 1.5).abs() < 1e-6, "the weights are additive and sum to what they sum to, got {total}");
	assert_eq!(pose.morph(9), None, "a target nothing drives says nothing");
	// A MORPH TRACK DOES NOT APPEAR AS A JOINT, which is what the separate target kinds are for.
	assert!(pose.joint(3).is_none(), "a morph target is not a joint");
	// AND `max_animation_tracks` COUNTS THEM like any other track.
	assert_eq!(clip.tracks(), 2);
}

// ---------------------------------------------------------------------------------------------
// The wiring: the scene selecting levels of detail, and a clip driving its nodes.
// ---------------------------------------------------------------------------------------------

#[test]
// THE SCENE SELECTS A LEVEL PER DRAWABLE PER VIEW, and the two views do not fight. A single cached
// level on the drawable would make each view's hysteresis overwrite the other's - a flicker that
// appears only once a second view exists and is traced to anything but the cache.
//
// WORKED BY HAND, AND THE RADIUS IS THE TRAP. A local box from (-1,-1,-1) to (1,1,1) has a bounding
// SPHERE of radius `sqrt(3)` = 1.7320508 and not 1 - the sphere has to contain the box's corners,
// which is the core profile's own derivation and the number a reader most easily assumes wrong.
//
// So under a 90 degree vertical field of view, where `tan(fov/2)` is 1:
//   at ten units:  1.7320508 / 10 = 0.17320508, which is at or below the second threshold of 0.25
//                  and above the third of 0.125 - level 2
//   at one unit:   1.7320508, which is above the first threshold of 0.5 - level 0
fn two_views_of_one_scene_choose_their_own_levels_and_keep_them_apart() {
	let mut scene = Scene::new(extended());
	let node = scene.add_node(Node::identity()).expect("a node");
	let material = scene.add_material(Material::new(MaterialKind::Unlit, GraphicsPipeline(0), 0)).expect("a material");
	let ladder = Ladder::with_default_thresholds(&[10, 11, 12, 13], &extended()).expect("four levels");
	let bounds = Aabb::new(Vec3::new(-1.0, -1.0, -1.0), Vec3::new(1.0, 1.0, 1.0));
	let drawable = scene.add_drawable(Drawable::new(node, 10, material).with_bounds(bounds).with_lod(ladder)).expect("a drawable");

	let far_node = scene.add_node(Node::identity().with_translation(Vec3::new(0.0, 0.0, 10.0))).expect("a camera node");
	let near_node = scene.add_node(Node::identity().with_translation(Vec3::new(0.0, 0.0, 1.0))).expect("a camera node");
	let quarter_turn = core::f32::consts::FRAC_PI_2;
	let far = Camera::perspective(far_node, quarter_turn, 1.0, 0.1, 100.0, u32::MAX).expect("a camera");
	let near = Camera::perspective(near_node, quarter_turn, 1.0, 0.1, 100.0, u32::MAX).expect("a camera");

	let mut from_far = ViewDetail::new();
	let mut from_near = ViewDetail::new();
	detail::select(&mut scene, &far, &mut from_far).expect("a selection");
	detail::select(&mut scene, &near, &mut from_near).expect("a selection");
	assert_eq!(from_far.of(drawable), Some(Detail::Level(2)), "ten units away is the third level down");
	assert_eq!(from_near.of(drawable), Some(Detail::Level(0)), "one unit away is the finest");

	// AND EACH VIEW HOLDS ITS OWN LEVEL ACROSS FRAMES: running the far view again does not read the
	// near view's memory, which is what a shared cache would have made it do.
	detail::select(&mut scene, &far, &mut from_far).expect("a second frame");
	assert_eq!(from_far.of(drawable), Some(Detail::Level(2)), "the far view is unmoved by the near one");
	assert_eq!(from_far.len(), 1, "one drawable, one remembered level");
	from_far.clear();
	assert!(from_far.is_empty(), "and a view can forget what it saw");
}

#[test]
// A DRAWABLE WITH NO LADDER IS NOT TOUCHED AT ALL, which is what makes the feature free for the
// meshes that do not use it; and one with no BOUNDS keeps its finest level rather than being given
// one at random - the core profile already says an unbounded drawable is never culled, and giving
// it a coarse mesh would be the same disappearance by another route.
fn a_drawable_with_no_ladder_or_no_bounds_is_left_alone() {
	let mut scene = Scene::new(extended());
	let node = scene.add_node(Node::identity()).expect("a node");
	let material = scene.add_material(Material::new(MaterialKind::Unlit, GraphicsPipeline(0), 0)).expect("a material");
	let plain = scene.add_drawable(Drawable::new(node, 1, material)).expect("no ladder");
	let unbounded = scene.add_drawable(Drawable::new(node, 2, material).with_lod(Ladder::with_default_thresholds(&[2, 3], &extended()).expect("two levels"))).expect("a ladder and no bounds");
	let camera_node = scene.add_node(Node::identity().with_translation(Vec3::new(0.0, 0.0, 50.0))).expect("a camera node");
	let camera = Camera::perspective(camera_node, core::f32::consts::FRAC_PI_2, 1.0, 0.1, 100.0, u32::MAX).expect("a camera");

	let mut view = ViewDetail::new();
	detail::select(&mut scene, &camera, &mut view).expect("a selection");
	assert_eq!(view.of(plain), None, "a drawable with no ladder is never asked about");
	assert_eq!(view.of(unbounded), Some(Detail::Level(0)), "and one with no bounds keeps its finest level");
}

#[test]
// AN ORTHOGRAPHIC CAMERA IS TOLD APART BY ITS OWN MATRIX and not by a flag: row 3 of column 3 is
// zero when the projection divides by `w` and one when it does not. The camera's shape is read out
// of the projection for the same reason `cull` extracts its planes that way - a camera may carry a
// projection its constructors cannot describe.
fn the_coverage_reads_the_camera_s_shape_out_of_its_projection() {
	let mut scene = Scene::new(extended());
	let node = scene.add_node(Node::identity()).expect("a node");
	let material = scene.add_material(Material::new(MaterialKind::Unlit, GraphicsPipeline(0), 0)).expect("a material");
	let ladder = Ladder::with_default_thresholds(&[1, 2, 3, 4], &extended()).expect("four levels");
	let bounds = Aabb::new(Vec3::new(-1.0, -1.0, -1.0), Vec3::new(1.0, 1.0, 1.0));
	let drawable = scene.add_drawable(Drawable::new(node, 1, material).with_bounds(bounds).with_lod(ladder)).expect("a drawable");
	let camera_node = scene.add_node(Node::identity().with_translation(Vec3::new(0.0, 0.0, 500.0))).expect("a camera node");

	// THE SAME `sqrt(3)` RADIUS, in an orthographic half-height of `2 * sqrt(3)` = 3.4641016, covers
	// EXACTLY 0.5 at every distance - so a camera five hundred units away still draws level 1, where
	// a distance-based ladder would have sent it to the coarsest. The half-height is chosen to land
	// on the first threshold, which also holds the profile's "AT OR BELOW": 0.5 belongs to the level
	// the threshold names rather than to the one above it.
	let half_height = 2.0 * render_math::sqrt(3.0);
	let flat = Camera::orthographic(camera_node, half_height, half_height, 0.1, 1000.0, u32::MAX).expect("an orthographic camera");
	let mut view = ViewDetail::new();
	detail::select(&mut scene, &flat, &mut view).expect("a selection");
	assert_eq!(view.of(drawable), Some(Detail::Level(1)), "0.5 is at the first threshold, which belongs to the level below it");

	// THE SAME SPHERE AT THE SAME PLACE UNDER A PERSPECTIVE CAMERA IS TINY, which is the difference
	// the matrix carries and a flag would have had to carry beside it.
	let deep = Camera::perspective(camera_node, core::f32::consts::FRAC_PI_2, 1.0, 0.1, 1000.0, u32::MAX).expect("a camera");
	let mut other = ViewDetail::new();
	detail::select(&mut scene, &deep, &mut other).expect("a selection");
	assert_eq!(other.of(drawable), Some(Detail::Level(3)), "five hundred units away is the coarsest level");
}

#[test]
// A CLIP DRIVES ITS NODES THROUGH A SKELETON, because a clip talks about JOINTS and a scene is made
// of NODES - and the two numberings are not the same one.
//
// AND A CHANNEL THE CLIP DOES NOT DRIVE IS LEFT ALONE. This is the property the function exists for:
// writing an identity into it would make a clip that only rotates a wrist also move that wrist to
// the origin, and the clip would look correct in isolation and destroy any pose it was blended into.
fn a_clip_drives_the_nodes_its_skeleton_names_and_touches_nothing_else() {
	let mut scene = Scene::new(extended());
	let root = scene.add_node(Node::identity()).expect("a node");
	let wrist = scene.add_node(Node::identity().with_parent(root).with_translation(Vec3::new(5.0, 0.0, 0.0)).with_scale(Vec3::new(2.0, 2.0, 2.0))).expect("a node");
	let skeleton = Skeleton::new(vec![root, wrist], &extended()).expect("a two-joint skeleton");

	// A clip that ROTATES joint 1 and says nothing about its translation or scale.
	let spin = vec![rotation_track(1, &[(0.0, turn(0.0)), (1.0, turn(core::f32::consts::FRAC_PI_2))])];
	let clip = Clip::new(spin, 1.0, Ending::Clamp, 0, true, &extended()).expect("a clip");
	animate::apply(&mut scene, &skeleton, &clip.sample(1.0)).expect("the pose applies");

	let posed = scene.nodes()[wrist as usize];
	assert!(posed.translation().sub(Vec3::new(5.0, 0.0, 0.0)).length() < 1e-6, "the translation the clip never mentioned is untouched, got {:?}", posed.translation());
	assert!(posed.scale().sub(Vec3::new(2.0, 2.0, 2.0)).length() < 1e-6, "and so is the scale, got {:?}", posed.scale());
	let spun = posed.rotation().rotate(Vec3::new(1.0, 0.0, 0.0));
	assert!(spun.sub(Vec3::new(0.0, 1.0, 0.0)).length() < 1e-4, "and the rotation it did drive is a quarter turn, got {spun:?}");

	// AND THE WORLD TRANSFORM FOLLOWS, which is what makes this a scene change rather than a
	// bookkeeping one: the wrist's parent still carries it.
	scene.update();
	let world = scene.transforms()[wrist as usize];
	assert!(world.translation().sub(Vec3::new(5.0, 0.0, 0.0)).length() < 1e-5, "the posed node's world transform is refreshed");

	// A JOINT THE SKELETON DOES NOT MAP IS REFUSED rather than skipped: a clip naming a joint the
	// rig does not have is an asset mismatch, and animating part of a character and leaving the rest
	// in its bind pose reads as a broken rig rather than as a mismatched pair.
	let stray = vec![rotation_track(7, &[(0.0, turn(0.0)), (1.0, turn(0.5))])];
	let wrong = Clip::new(stray, 1.0, Ending::Clamp, 0, true, &extended()).expect("a clip");
	assert!(matches!(animate::apply(&mut scene, &skeleton, &wrong.sample(0.5)), Err(Error::Degenerate { .. })));
	// AND A SKELETON IS BOUNDED BY THE PROFILE'S JOINT LIMIT like everything else.
	let many: Vec<u32> = (0..129).map(|_| root).collect();
	assert!(matches!(Skeleton::new(many, &extended()), Err(Error::LimitExceeded { limit: "max_skeleton_joints", ceiling: 128, asked: 129 })));
	assert!(matches!(Skeleton::new(Vec::new(), &extended()), Err(Error::Degenerate { .. })));
}

// ---------------------------------------------------------------------------------------------
// The PBR material's join: a second table, and one drawable naming one material in one of them.
// ---------------------------------------------------------------------------------------------

/// An Extended material with the plainest factors there are, so a fixture about WHERE it lives is
/// not also a fixture about what it computes - `pbr`'s own ten hold that.
fn physical(pipeline: u32) -> ExtendedMaterial {
	ExtendedMaterial::new(PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 1.0), 0.0, 0.5), GraphicsPipeline(pipeline), pipeline)
}

#[test]
// A PHYSICALLY BASED MATERIAL LIVES IN ITS OWN TABLE AND THE CORE LIST STAYS FOUR. The core profile
// enumerates exactly four material kinds; a fifth would change what `Scene3D Core Profile 1` means,
// and giving a PBR material one of the four would put a lie in the inventory. So it goes somewhere
// else, and `material_of` is what knows where.
fn an_extended_material_lives_in_its_own_table_and_the_core_list_stays_four() {
	let mut scene = Scene::new(extended());
	let core = scene.add_material(opaque(1)).expect("a core material");
	let physically = scene.add_extended_material(physical(2)).expect("an Extended material");
	assert_eq!(core, 0, "the two tables have their own indices, and both start at zero");
	assert_eq!(physically, 0);
	assert_eq!(scene.materials().len(), 1, "the core table did not grow");
	assert_eq!(scene.extended_materials().len(), 1);

	let node = scene.add_node(at(Vec3::new(0.0, 0.0, -10.0))).unwrap();
	let plain = scene.add_drawable(Drawable::new(node, 0, core).with_bounds(cube())).unwrap();
	let shiny = scene.add_drawable(Drawable::extended(node, 0, physically).with_bounds(cube())).unwrap();
	assert_eq!(scene.drawables()[plain as usize].material, MaterialRef::Core(0));
	assert_eq!(scene.drawables()[shiny as usize].material, MaterialRef::Extended(0));

	// AND THE RESOLUTION IS BY TABLE AND NOT BY INDEX. Both drawables name material ZERO, and the
	// two zeroes are different materials - which is the whole reason the table is part of the name.
	let resolved = scene.material_of(&scene.drawables()[plain as usize]).expect("the core material resolves");
	assert!(matches!(resolved, Shading::Core(_)));
	assert!(!resolved.is_extended());
	assert_eq!(resolved.pipeline(), GraphicsPipeline(1));
	let resolved = scene.material_of(&scene.drawables()[shiny as usize]).expect("the Extended material resolves");
	assert!(matches!(resolved, Shading::Extended(_)));
	assert!(resolved.is_extended());
	assert_eq!(resolved.pipeline(), GraphicsPipeline(2), "and the second table's pipeline is the one recorded");
}

#[test]
// A SCENE THAT DOES NOT CLAIM EXTENDED HAS NO SECOND TABLE AT ALL. The six Extended limits are set
// together or left at zero, and this is the one entry point through which a layer could otherwise
// acquire a piece of the part - a material it could draw with, under a profile it does not claim.
fn a_scene_that_does_not_claim_extended_refuses_an_extended_material() {
	let mut scene = Scene::new(Limits::PROFILE_MINIMUM);
	assert!(!Limits::PROFILE_MINIMUM.claims_extended());
	assert!(matches!(scene.add_extended_material(physical(1)), Err(Error::NotExtended { operation: "add_extended_material" })));
	assert!(scene.extended_materials().is_empty(), "and nothing was added on the way to the refusal");

	// AND THE CORE TABLE IS UNAFFECTED, because refusing the part is not refusing the profile.
	assert!(scene.add_material(opaque(1)).is_ok());
}

#[test]
// BOTH TABLES ARE COUNTED AGAINST ONE CEILING, for the reason `max_transparent_items` was refused: a
// physically based material IS a material, the core profile already says how many a scene may hold,
// and a second limit over the second table is two numbers that can disagree.
fn both_material_tables_are_counted_against_one_ceiling() {
	let limits = Limits { max_materials: 2, ..extended() };
	let mut scene = Scene::new(limits);
	scene.add_material(opaque(1)).expect("the first of two");
	scene.add_extended_material(physical(2)).expect("the second of two, in the other table");
	assert_eq!(scene.material_count(), 2);
	// THE THIRD IS REFUSED WHICHEVER TABLE IT IS FOR, and both refusals name the core limit.
	assert!(matches!(scene.add_material(opaque(3)), Err(Error::LimitExceeded { limit: "max_materials", ceiling: 2, asked: 3 })));
	assert!(matches!(scene.add_extended_material(physical(3)), Err(Error::LimitExceeded { limit: "max_materials", ceiling: 2, asked: 3 })));
}

#[test]
// A DRAWABLE NAMING A MATERIAL THE TABLE DOES NOT HOLD IS REFUSED, and the refusal says WHICH table,
// because the two carry their own indices and "no such material" would send a reader to the wrong
// list.
fn a_drawable_naming_a_material_no_table_holds_is_refused_by_table() {
	let mut scene = Scene::new(extended());
	let node = scene.add_node(at(Vec3::ZERO)).unwrap();
	scene.add_material(opaque(1)).unwrap();
	assert!(matches!(scene.add_drawable(Drawable::new(node, 0, 1)), Err(Error::NoSuchMaterial { material: 1 })));
	// INDEX ZERO EXISTS IN THE CORE TABLE AND NOT IN THE OTHER ONE, which is exactly the confusion
	// one shared refusal would hide.
	assert!(matches!(scene.add_drawable(Drawable::extended(node, 0, 0)), Err(Error::NoSuchExtendedMaterial { material: 0 })));
}

#[test]
// THE QUEUE RULE IS ONE RULE AND BOTH FAMILIES OBEY IT. `queue`, `writes_depth` and `writes_id` are
// functions of the blending and of nothing else, so they live on `Blending` and both materials
// delegate - a second copy is how a PBR surface comes to hide what is behind it while a
// `BlinnPhong` one does not.
fn an_extended_material_takes_its_queue_from_the_same_rule_as_a_core_one() {
	for blending in [Blending::Opaque, Blending::AlphaMask { threshold: 0.5 }, Blending::Blended] {
		let core = opaque(1).with_blending(blending);
		let physically = ExtendedMaterial::new(PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 1.0), 0.0, 0.5).with_blending(blending), GraphicsPipeline(2), 2);
		assert_eq!(core.queue(), physically.queue(), "the same blending is the same queue: {blending:?}");
		assert_eq!(core.writes_depth(), physically.writes_depth());
		assert_eq!(core.writes_id(), physically.writes_id());
		assert_eq!(core.blend, physically.blend, "and the blend state a blending implies is the same state");
		physically.validate().expect("and each of the three validates");
	}

	// AND THE SCENE PUTS IT IN THAT QUEUE. A blended Extended material is sorted with the glass and
	// writes neither depth nor an identity, which is the rule reaching the passes rather than only
	// the type.
	let mut scene = Scene::new(extended());
	let glass = ExtendedMaterial::new(PbrMaterial::new(Vec4::new(1.0, 1.0, 1.0, 0.5), 0.0, 0.5).with_blending(Blending::Blended), GraphicsPipeline(2), 2);
	let index = scene.add_extended_material(glass).expect("a transparent Extended material");
	let node = scene.add_node(at(Vec3::new(0.0, 0.0, -10.0))).unwrap();
	scene.add_drawable(Drawable::extended(node, 0, index).with_bounds(cube())).unwrap();
	let (_, frustum) = known_frustum();
	let queue = crate::queue::build_with(&mut scene, Vec3::ZERO, &frustum, u32::MAX);
	assert_eq!(queue.transparent.len(), 1, "a blended physically based surface is in the transparent queue");
	assert!(queue.opaque.is_empty());

	// AND A PICK PASSES THROUGH IT, which is `writes_id` reaching the third caller.
	let ray = crate::pick::Ray { origin: Vec3::ZERO, direction: Vec3::new(0.0, 0.0, -1.0) };
	assert!(crate::pick::resolve(&scene, &ray, u32::MAX).is_none(), "the glass writes no identity, so the ray answers nothing");
}

#[test]
// AN EXTENDED MATERIAL IS REFUSED FOR WHAT IT IS RATHER THAN DRAWN WRONG. Both factors are mixes,
// and a mix outside zero to one is not one: a metallic above one drives the diffuse term negative
// and a roughness above one takes `a = roughness^2` past the range the fits were made over. Neither
// reports itself, because both still produce a colour.
fn an_extended_material_refuses_factors_that_are_not_mixes() {
	let mut scene = Scene::new(extended());
	let mut wrong = physical(1);
	wrong.shading.metallic = 1.5;
	assert!(matches!(scene.add_extended_material(wrong), Err(Error::Degenerate { .. })));
	let mut wrong = physical(1);
	wrong.shading.roughness = -0.1;
	assert!(matches!(scene.add_extended_material(wrong), Err(Error::Degenerate { .. })));
	let mut wrong = physical(1);
	wrong.shading.occlusion_strength = 2.0;
	assert!(matches!(scene.add_extended_material(wrong), Err(Error::Degenerate { .. })));
	// AND THE BLEND STATE MUST AGREE WITH THE BLENDING on the same terms as a core material's: one
	// that says it is transparent and carries a pipeline that does not blend draws opaque, which
	// reads as a missing texture.
	let lying = physical(1).with_blend_state(Blending::Blended.blend_state());
	assert!(matches!(scene.add_extended_material(lying), Err(Error::Degenerate { .. })));
	assert!(scene.extended_materials().is_empty(), "no refusal left anything behind");
}

// ---------------------------------------------------------------------------------------------
// Every Extended feature, end to end: the sixth clause of the host-test item.
// ---------------------------------------------------------------------------------------------

/// A geometry source where a mesh identifier IS its vertex buffer, so the recorded list says WHICH
/// level was drawn rather than only that something was.
struct MeshPerLevel;

impl Geometry for MeshPerLevel {
	fn draw_of(&self, mesh: u32) -> Option<MeshDraw> {
		Some(MeshDraw { topology: Topology::TriangleList, state: PipelineState { topology: Topology::TriangleList, cull: Cull::Back, depth_test: Some(render3d::depth::CompareOp::LessOrEqual), depth_write: true, samples: 1, per_sample_shading: false }, vertex_buffer: Buffer(mesh), instance_buffer: Some(Buffer(3)), vertices: 36, indices: Some(Indices { buffer: Buffer(2), count: 36, wide: false }) })
	}
}

#[test]
// EVERY EXTENDED FEATURE ENDS IN A `render3d` COMMAND AND NOWHERE ELSE, which is the sixth clause of
// the host-test item and the one the core fixture of this shape does not cover: that one ranges over
// materials, instancing, culling, lights and a camera, and no Extended feature appears in it at all.
//
// THIS SCENE CARRIES ALL OF THEM: a level-of-detail ladder that chooses a coarser mesh, a second
// ladder that vanishes, a skeleton driven by a clip, a bound recomputed after morphing and skinning,
// a physically based material out of the second table, all three queues, and a shadow caster pass.
// The whole of its output is the two recorded lists, and each feature is read back from the commands
// rather than from the structures that produced them.
fn a_scene_that_uses_every_extended_feature_emits_only_render3d_commands() {
	let limits = extended();
	let mut scene = Scene::new(limits);

	// THE BOUND THE LADDER MEASURES IS THE DEFORMED ONE and not the rest pose's, which is the whole
	// of why `deformed_bounds` exists: a mesh a clip has moved is measured where it now is.
	let base = [Vec3::new(-0.5, -0.5, -0.5), Vec3::new(0.5, 0.5, 0.5)];
	let displacement = [Vec3::new(0.0, 0.5, 0.0), Vec3::new(0.0, 0.5, 0.0)];
	let targets = [crate::deform::MorphTarget { displacement: &displacement, weight: 1.0 }];
	let influences = [Influences::new([0, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]).expect("one joint at full weight"); 2];
	let pose = Pose::compose(&[Mat4::IDENTITY], &[Mat4::IDENTITY], &limits).expect("a one-joint pose");
	let deformed = crate::deform::deformed_bounds(&base, &targets, &influences, &pose, &limits).expect("the deformed bound");
	assert!(near(deformed.maximum.y, 1.0), "the morph raised the top of the box, got {:?}", deformed.maximum);

	// THE MATERIALS: one out of each table, plus the two the caster pass has rules about.
	let plain = scene.add_material(opaque(1)).unwrap();
	let masked = scene.add_material(opaque(2).with_blending(Blending::AlphaMask { threshold: 0.5 })).unwrap();
	let glass = scene.add_material(blended(3)).unwrap();
	let physically = scene.add_extended_material(physical(5)).expect("a physically based material");

	let rig = scene.add_node(at(Vec3::ZERO)).unwrap();
	let camera = Camera::perspective(rig, core::f32::consts::FRAC_PI_2, 1.0, 1.0, 100.0, u32::MAX).unwrap();
	scene.add_camera(camera).unwrap();
	scene.add_light(Light { node: rig, kind: LightKind::Directional { direction: Vec3::new(0.0, -1.0, 0.0) }, colour: Vec3::new(1.0, 1.0, 1.0), intensity: 3.0, visibility: u32::MAX }).unwrap();

	// THE HERO: the physically based material, a ladder it stays at the finest level of, and the
	// deformed bound. Its node is what the clip drives.
	let joint = scene.add_node(at(Vec3::new(0.0, 0.0, -10.0))).unwrap();
	let fine = Ladder::new(10, vec![crate::detail::Level { mesh: 11, threshold: 1e-6 }], &limits).expect("a two-level ladder");
	let hero = scene.add_drawable(Drawable::extended(joint, 10, physically).with_bounds(deformed).with_lod(fine)).unwrap();

	// THE DISTANT ONE: a ladder whose coarse level takes over at any coverage this scene produces.
	let far = scene.add_node(at(Vec3::new(3.0, 0.0, -10.0))).unwrap();
	let coarsening = Ladder::new(20, vec![crate::detail::Level { mesh: 21, threshold: 0.9 }], &limits).expect("a coarsening ladder");
	let distant = scene.add_drawable(Drawable::new(far, 20, plain).with_bounds(cube()).with_lod(coarsening)).unwrap();

	// THE SPECK: a ladder that says this is too small to be worth a draw call at all.
	let speck_node = scene.add_node(at(Vec3::new(-3.0, 0.0, -10.0))).unwrap();
	let vanishing = Ladder::new(30, Vec::new(), &limits).expect("a one-level ladder").vanishing_below(0.9).expect("a vanishing coverage");
	let speck = scene.add_drawable(Drawable::new(speck_node, 30, plain).with_bounds(cube()).with_lod(vanishing)).unwrap();

	// AND THE TWO THE CASTER PASS HAS RULES ABOUT, with no ladder at all - a feature every scene may
	// leave out has to stay free for the ones that do.
	scene.add_drawable(Drawable::new(far, 40, masked).with_bounds(cube())).unwrap();
	scene.add_drawable(Drawable::new(far, 41, glass).with_bounds(cube())).unwrap();

	// THE ANIMATION, APPLIED: a quarter turn about z on the hero's joint.
	let skeleton = Skeleton::new(vec![joint], &limits).expect("a one-joint skeleton");
	let clip = Clip::new(vec![Track { target: Target::Joint(0), channel: Channel::Rotation(linear(&[(0.0, turn(0.0)), (1.0, turn(core::f32::consts::FRAC_PI_2))])) }], 1.0, Ending::Clamp, 0, true, &limits).expect("a rotation clip");
	animate::apply(&mut scene, &skeleton, &clip.sample(1.0)).expect("the pose applies");

	// THE LEVELS, CHOSEN BY THE VIEW, then the queue built at them.
	let mut state = ViewDetail::new();
	crate::detail::select(&mut scene, &camera, &mut state).expect("the levels are chosen");
	assert_eq!(state.of(hero), Some(Detail::Level(0)), "the hero stays at its finest level");
	assert_eq!(state.of(distant), Some(Detail::Level(1)), "the distant one takes its coarse level");
	assert_eq!(state.of(speck), Some(Detail::Vanished), "and the speck vanishes");

	let queue = crate::queue::build_detailed(&mut scene, &camera, &state).expect("a queue at the chosen levels");
	assert_eq!(queue.vanished, 1, "the vanished drawable is counted in its own tally");
	assert_eq!(queue.culled, 0, "and not as a culled one, which is a different question");
	assert_eq!(queue.opaque.len(), 2, "the hero and the distant one");

	// THE ANIMATION REACHED THE ENTRY. The queue carries the world transform the recorder draws at,
	// so a clip that moved a joint moved the thing that is drawn.
	let entry = queue.opaque.iter().find(|entry| entry.drawable == hero).expect("the hero is queued");
	assert!(near(entry.world.at(1, 0), 1.0) && near(entry.world.at(0, 0), 0.0), "the hero's world transform carries the quarter turn, got {:?}", entry.world);

	let colour = [colour_target()];
	let set = RenderTargetSet { colour: &colour, depth_stencil: None, resolve: &[] };
	let mut list = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	crate::emit::record(&scene, &queue, &MeshPerLevel, &mut list, &set, &pass_targets()).unwrap();
	list.finish().unwrap();
	let commands = list.commands();

	// THE LEVEL REACHED THE DRAW. A mesh identifier is its vertex buffer here, so the coarse level
	// is a command and the fine one is not - which is the assertion that would have failed for as
	// long as the recorder drew `drawable.mesh`.
	let buffers: Vec<u32> = commands.iter().filter_map(|command| if let Command::BindVertexBuffer { slot: 0, buffer, .. } = command { Some(buffer.0) } else { None }).collect();
	assert!(buffers.contains(&21), "the distant drawable was drawn at its COARSE mesh: {buffers:?}");
	assert!(!buffers.contains(&20), "and not at its fine one");
	assert!(buffers.contains(&10), "the hero was drawn at its finest");
	assert!(!buffers.contains(&30), "and the vanished one was not drawn at all");

	// THE PHYSICALLY BASED MATERIAL REACHED THE DRAW, out of the second table and through the same
	// recorder: its pipeline is bound and its uniform block with it.
	let pipelines: Vec<u32> = commands.iter().filter_map(|command| if let Command::BindPipeline(pipeline) = command { Some(pipeline.0) } else { None }).collect();
	assert!(pipelines.contains(&5), "the Extended material's pipeline is bound: {pipelines:?}");
	assert!(commands.iter().any(|command| matches!(command, Command::BindResources { set: 5 })), "and its uniform block");

	// AND NOTHING LEFT THE LIST BY ANY OTHER ROUTE. Every command is one of the nine `render3d`
	// records, which is the claim this fixture is named for.
	assert!(commands.iter().all(|command| matches!(command, Command::BeginRenderPass { .. } | Command::EndRenderPass | Command::SetViewport(_) | Command::SetScissor(_) | Command::BindPipeline(_) | Command::BindVertexBuffer { .. } | Command::BindIndexBuffer { .. } | Command::BindResources { .. } | Command::Draw { .. } | Command::DrawIndexed { .. })), "the whole of the scene's output is `render3d` commands");

	// AND THE SHADOW HALF, which is the one Extended feature with a pass of its own.
	let map = shadow_target();
	let depth_only = RenderTargetSet { colour: &[], depth_stencil: Some(map), resolve: &[] };
	let caster = crate::emit::CasterPass { targets: 7, viewport: Rect { x: 0, y: 0, width: 64, height: 64 }, pipeline: GraphicsPipeline(11), masked_pipeline: Some(GraphicsPipeline(12)), light_set: 5 };
	let mut casters = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	crate::emit::record_casters(&scene, &queue, &MeshPerLevel, &mut casters, &depth_only, &caster).unwrap();
	casters.finish().unwrap();
	let shadow_buffers: Vec<u32> = casters.commands().iter().filter_map(|command| if let Command::BindVertexBuffer { slot: 0, buffer, .. } = command { Some(buffer.0) } else { None }).collect();
	assert!(shadow_buffers.contains(&21) && !shadow_buffers.contains(&20), "a caster is drawn at the same level its surface is: {shadow_buffers:?}");
	assert!(!shadow_buffers.contains(&41), "and the glass casts no shadow");

	// THE THREE RESOURCE DESCRIPTIONS ARE DESCRIPTIONS AND NOT COMMANDS, which is what the profile
	// says they are: a shadow map, an environment cube and an HDR target are things a layer CREATES,
	// and the passes over them are `render3d` work. Named here so the list above is read as complete
	// rather than as a list that forgot them.
	shadow::cascade_map_desc(1024, limits.max_shadow_cascades, &limits).expect("a cascade array");
	crate::environment::cube_desc(256).expect("an environment cube");
	crate::environment::hdr_target_desc(320, 240).expect("an HDR target");
}

#[test]
// THE FIVE MAPS ARE NAMED BY THE MATERIAL, which is what "the PBR material carries base-colour,
// metallic-roughness, normal, occlusion and emissive maps" means in code: a material a backend can
// draw has to say WHICH texture each of the five is, and the sampled values stay in `PbrSurface`
// where a fragment carries them.
fn a_physically_based_material_names_its_five_maps_and_their_colour_spaces() {
	let mut scene = Scene::new(extended());
	let mut material = physical(1);
	for (index, map) in PbrMap::ALL.iter().enumerate() {
		material = material.with_map(*map, 100 + index as u32);
	}
	assert_eq!(material.maps.count(), 5, "all five are nameable at once");
	assert_eq!(material.maps.of(PbrMap::BaseColour), Some(100));
	assert_eq!(material.maps.of(PbrMap::Emissive), Some(104), "and each names its own texture rather than the last one written");
	let index = scene.add_extended_material(material).expect("a material with five maps");
	assert_eq!(scene.extended_materials()[index as usize].maps.of(PbrMap::Normal), Some(102));

	// A MATERIAL WITH NO MAPS IS THE SAME TYPE AND THE SAME PATH, which is what makes the absence of
	// a map the identity rather than a second material model.
	assert_eq!(physical(2).maps.count(), 0);

	// AND THE COLOUR SPACE OF EACH IS THE PROFILE'S, AS A FUNCTION. Base colour and emissive are
	// COLOUR and are transfer-decoded; metallic-roughness, normal and occlusion are DATA and are
	// never decoded. Reading a roughness map as sRGB makes a whole material glossier and reading a
	// normal map that way bends every normal toward the surface, and neither reports itself.
	assert!(PbrMap::BaseColour.semantics().is_colour());
	assert!(PbrMap::Emissive.semantics().is_colour());
	for map in [PbrMap::MetallicRoughness, PbrMap::Normal, PbrMap::Occlusion] {
		assert!(!map.semantics().is_colour(), "{map:?} is data and is never transfer-decoded");
		assert_eq!(map.semantics(), graphics_profile::image::Semantics::Data);
	}
}

#[test]
// THE HDR FRAME IS A GRAPH OF PASSES AND NOT A LIST OF OPERATORS. A layer that had only the
// threshold, the two kernels, the fog and the tone map would still have to decide how many targets a
// bloom needs, what each pass reads and what order they run in - and every layer would decide it
// differently, which is the drift the frozen operators exist to prevent.
fn the_hdr_chain_is_a_pass_graph_whose_resolve_is_last() {
	let pyramid: [u32; 6] = [10, 11, 12, 13, 14, 15];
	let ascent: [u32; 5] = [20, 21, 22, 23, 24];
	let chain = crate::postprocess::Chain { scene: 1, pyramid: &pyramid, ascent: &ascent };
	let passes = crate::postprocess::chain(&chain, 100).expect("a frame's chain");
	assert_eq!(passes.downsample.len(), 6, "one pass per pyramid level");
	assert_eq!(passes.upsample.len(), 5, "and one per level except the coarsest");

	// THE ORDER IS DERIVED AND NOT DECLARED, so this reads it back out of the graph rather than out
	// of the order the passes were added in.
	let order = passes.graph.order().expect("the graph orders");
	assert_eq!(order.len(), 13, "the scene, six downsamples, five upsamples and the resolve");
	assert_eq!(order.first(), Some(&passes.scene), "nothing runs before the pass that fills the HDR target");
	assert_eq!(order.last(), Some(&passes.resolve), "and TONE MAPPING IS LAST, which is the profile's order");
	let at = |id: u32| order.iter().position(|pass| *pass == id).expect("every pass is in the order");
	for (level, pass) in passes.downsample.iter().enumerate() {
		assert!(at(*pass) > at(passes.scene), "a downsample runs after the scene");
		if level > 0 {
			assert!(at(*pass) > at(passes.downsample[level - 1]), "and after the level it reads");
		}
	}
	// THE ASCENT RUNS COARSEST FIRST, each after the level below it has been written.
	for (step, pass) in passes.upsample.iter().enumerate() {
		assert!(at(*pass) > at(passes.downsample[passes.downsample.len() - 1]), "every upsample runs after the pyramid is built");
		if step > 0 {
			assert!(at(*pass) > at(passes.upsample[step - 1]), "and after the coarser step it reads");
		}
	}

	// TWO CHAINS AND NOT ONE, and the graph is why: an upsample that wrote back into the level it
	// read would make one target carry two writers and a reader of itself, which has no order.
	assert!(passes.graph.passes().iter().all(|pass| pass.writes.iter().all(|target| !pass.reads.contains(target))), "no pass reads what it writes");
	let mut written: Vec<u32> = passes.graph.passes().iter().flat_map(|pass| pass.writes.clone()).collect();
	let before = written.len();
	written.sort_unstable();
	written.dedup();
	assert_eq!(written.len(), before, "every target is written exactly once per frame");

	// AND THE REFUSALS ARE ABOUT THE FRAME THAT WAS ASKED FOR.
	let short: [u32; 5] = [10, 11, 12, 13, 14];
	assert!(matches!(crate::postprocess::chain(&crate::postprocess::Chain { scene: 1, pyramid: &short, ascent: &ascent }, 0), Err(Error::Degenerate { .. })), "five levels is a different bloom at the same weight");
	let collided: [u32; 6] = [1, 11, 12, 13, 14, 15];
	assert!(matches!(crate::postprocess::chain(&crate::postprocess::Chain { scene: 1, pyramid: &collided, ascent: &ascent }, 0), Err(Error::Degenerate { .. })), "a level sharing the scene target has no order");
}

#[test]
// THE PYRAMID'S TARGETS ARE HALF FLOATS AND ITS LEVELS ARE HALVINGS, and a render too small to
// carry six of them is REFUSED rather than given a shorter pyramid: a bloom built from four levels
// has a different shape at the same weight, so a small window would bloom differently from a large
// one rather than not at all.
fn the_bloom_pyramid_is_six_halvings_of_half_floats() {
	let levels = crate::postprocess::bloom_pyramid_desc(320, 240).expect("a pyramid for a 320x240 render");
	assert_eq!(levels.len(), crate::postprocess::BLOOM_LEVELS as usize);
	assert_eq!((levels[0].width, levels[0].height), (160, 120), "the first level is half the render");
	assert_eq!((levels[5].width, levels[5].height), (5, 3), "and the sixth is a sixty-fourth of it");
	assert!(levels.iter().all(|level| level.format == crate::environment::HDR_FORMAT), "the pyramid carries radiance above one, which a normalised format would clip");
	assert!(levels.iter().all(|level| level.mip_levels == 1), "the pyramid IS the chain; a level is not a mip of another");
	assert!(levels.iter().all(|level| level.usage.sampled && level.usage.colour_attachment), "each level is written by a pass and read by the next");

	assert_eq!(crate::postprocess::BLOOM_MINIMUM_EXTENT, 64, "six halvings need sixty-four texels");
	assert!(matches!(crate::postprocess::bloom_pyramid_desc(64, 48), Err(Error::Degenerate { .. })), "a 64x48 render cannot carry the profile's pyramid");
	assert!(crate::postprocess::bloom_pyramid_desc(64, 64).is_ok(), "and the smallest that can is exactly the minimum");
}
