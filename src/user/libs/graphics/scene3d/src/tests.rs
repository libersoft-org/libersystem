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

fn pass_targets() -> PassTargets {
	PassTargets { targets: 0, viewport: Rect { x: 0, y: 0, width: 800, height: 800 }, samples: 1, id_set: Some(64) }
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
