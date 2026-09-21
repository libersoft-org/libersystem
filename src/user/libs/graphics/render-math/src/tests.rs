//! The fixtures. Every one of them holds a CONVENTION rather than an implementation: the values are
//! hand-computed or computed in `f64` from the definition, so a rewrite that changed the arithmetic
//! and kept the contract passes, and one that changed the contract fails.

use super::*;

/// How close two `f32` values must be. Six decimal digits is what an `f32` carries, and every value
/// below is small enough that an absolute bound is the honest one.
const CLOSE: f32 = 1e-5;

fn near(left: f32, right: f32) -> bool {
	(left - right).abs() <= CLOSE
}

fn near_vec3(left: Vec3, right: Vec3) -> bool {
	near(left.x, right.x) && near(left.y, right.y) && near(left.z, right.z)
}

#[test]
// THE SQUARE ROOT THIS CRATE CARRIES ITS OWN COPY OF, against the one the host has. `no_std` has no
// `sqrt`, and a renderer whose lengths are wrong in the sixth digit is a renderer whose normals are
// wrong everywhere - so the substitute is held to the real thing over the range it is used in.
fn the_square_root_agrees_with_the_hosts_over_every_magnitude_it_is_used_at() {
	for value in [0.0f64, 1e-8, 1e-4, 0.5, 1.0, 2.0, 3.0, 7.0, 100.0, 12345.0, 1e8, 1e12] {
		let ours = vector::sqrt(value as f32) as f64;
		let theirs = value.sqrt();
		let tolerance = theirs.max(1.0) * 1e-6;
		assert!((ours - theirs).abs() <= tolerance, "sqrt({value}) is {ours} and should be {theirs}");
	}
	assert!(vector::sqrt(-1.0).is_nan(), "a negative has no real root");
	assert_eq!(vector::sqrt(0.0), 0.0);
	assert_eq!(vector::sqrt(f32::INFINITY), f32::INFINITY);
}

#[test]
// AND THE SINE AND COSINE, at the quadrant boundaries where a range reduction is wrong if it is
// wrong anywhere.
fn sine_and_cosine_agree_with_the_hosts_including_at_the_quadrant_boundaries() {
	let quarter = core::f64::consts::FRAC_PI_2;
	for step in -9..=9 {
		for offset in [0.0, 1e-3, -1e-3, 0.3, -0.3] {
			let angle = step as f64 * quarter + offset;
			let (sine, cosine) = quaternion::sin_cos(angle as f32);
			assert!((sine as f64 - angle.sin()).abs() <= 1e-5, "sin({angle}) is {sine} and should be {}", angle.sin());
			assert!((cosine as f64 - angle.cos()).abs() <= 1e-5, "cos({angle}) is {cosine} and should be {}", angle.cos());
		}
	}
	assert!(quaternion::sin_cos(f32::NAN).0.is_nan(), "a non-finite angle has no sine");
}

#[test]
// THE STORAGE ORDER IS THE CONTRACT. A matrix leaves this crate as sixteen floats and the receiver
// reads them in SOME order; row-major and column-major are each other's transpose, which for a
// rotation is its inverse. This fixture is what makes that order checkable rather than conventional.
fn a_matrix_is_column_major_and_its_fourth_column_is_the_translation() {
	let translation = Mat4::from_translation(Vec3::new(7.0, 8.0, 9.0));
	let array = translation.to_array();
	assert_eq!(&array[12..16], &[7.0, 8.0, 9.0, 1.0], "the translation is the LAST four floats, which is what column-major means");
	assert_eq!(translation.translation(), Vec3::new(7.0, 8.0, 9.0));
	assert_eq!(translation.at(0, 3), 7.0, "row 0, column 3 is the x translation");
	assert_eq!(translation.at(3, 0), 0.0, "and row 3, column 0 is not - which is the transpose, and the defect this fixture exists for");
	assert_eq!(Mat4::from_array(array), translation, "the round trip is the identity");
}

#[test]
// `M * v` WITH THE VECTOR ON THE RIGHT, and a point translated where a direction is not. The
// distinction is the whole reason a 3D transform is four by four.
fn a_point_is_translated_and_a_direction_is_not() {
	let transform = Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0));
	assert_eq!(transform.transform_point(Vec3::new(1.0, 2.0, 3.0)).truncate(), Vec3::new(11.0, 2.0, 3.0));
	assert_eq!(transform.transform_direction(Vec3::new(1.0, 2.0, 3.0)), Vec3::new(1.0, 2.0, 3.0), "a direction has no position to move");
}

#[test]
// COMPOSITION READS RIGHT TO LEFT, the way the transform does: `a.mul(b)` applies `b` first. An
// implementation with the order swapped passes every test that uses one matrix and fails the world.
fn composition_applies_the_right_hand_matrix_first() {
	let scale = Mat4::from_scale(Vec3::new(2.0, 2.0, 2.0));
	let translate = Mat4::from_translation(Vec3::new(1.0, 0.0, 0.0));
	// Scale THEN translate: the translation is not scaled.
	let scale_then_translate = translate.mul(&scale);
	assert_eq!(scale_then_translate.transform_point(Vec3::new(1.0, 0.0, 0.0)).truncate(), Vec3::new(3.0, 0.0, 0.0));
	// Translate THEN scale: the translation is scaled too.
	let translate_then_scale = scale.mul(&translate);
	assert_eq!(translate_then_scale.transform_point(Vec3::new(1.0, 0.0, 0.0)).truncate(), Vec3::new(4.0, 0.0, 0.0));
}

#[test]
// INVERSION ANSWERS A TYPED REFUSAL AND NEVER A MATRIX OF INFINITIES.
fn inversion_round_trips_and_refuses_a_singular_matrix_by_name() {
	let transform = Mat4::from_translation(Vec3::new(3.0, -4.0, 5.0)).mul(&Mat4::from_scale(Vec3::new(2.0, 4.0, 0.5)));
	let inverse = transform.inverse().expect("an invertible transform");
	let round_trip = inverse.mul(&transform);
	for (index, (got, want)) in round_trip.to_array().iter().zip(Mat4::IDENTITY.to_array().iter()).enumerate() {
		assert!(near(*got, *want), "entry {index} of the round trip is {got} and should be {want}");
	}
	// A scale that flattens one axis has no inverse, and this is the case that used to produce a
	// matrix of numbers around 1e30 rather than an error.
	assert_eq!(Mat4::from_scale(Vec3::new(1.0, 0.0, 1.0)).inverse(), Err(Error::Singular));
	// And a matrix with a NaN in it is refused before the determinant is even taken.
	let mut poisoned = Mat4::IDENTITY.to_array();
	poisoned[5] = f32::NAN;
	assert_eq!(Mat4::from_array(poisoned).inverse(), Err(Error::NotFinite));
}

#[test]
// A ZERO-LENGTH VECTOR HAS NO DIRECTION, and normalising it is a division by zero that produces a
// NaN that looks like one.
fn normalising_refuses_a_zero_length_and_a_non_finite_vector() {
	assert_eq!(Vec3::ZERO.normalise(), Err(Error::ZeroLength));
	assert_eq!(Vec3::new(f32::INFINITY, 0.0, 0.0).normalise(), Err(Error::NotFinite));
	let unit = Vec3::new(3.0, 4.0, 0.0).normalise().expect("a vector with length");
	assert!(near(unit.length(), 1.0));
	assert!(near_vec3(unit, Vec3::new(0.6, 0.8, 0.0)));
}

#[test]
// THE CROSS PRODUCT IS RIGHT-HANDED, which is what makes `look_at_rh`'s basis come out right-handed
// and therefore what makes the camera look along `-Z`.
fn the_cross_product_is_right_handed() {
	assert_eq!(Vec3::new(1.0, 0.0, 0.0).cross(Vec3::new(0.0, 1.0, 0.0)), Vec3::new(0.0, 0.0, 1.0), "x cross y is +z");
	assert_eq!(Vec3::new(0.0, 1.0, 0.0).cross(Vec3::new(0.0, 0.0, 1.0)), Vec3::new(1.0, 0.0, 0.0), "y cross z is +x");
}

#[test]
// CONSTRUCTORS NORMALISE, OPERATIONS PRESERVE UNIT LENGTH, AND A ZERO-LENGTH QUATERNION IS AN ERROR.
// The three sentences of the policy, one assertion each.
fn the_quaternion_normalisation_policy_holds_in_all_three_of_its_parts() {
	// A constructor handed an axis of length five answers a unit quaternion.
	let rotation = Quat::from_axis_angle(Vec3::new(0.0, 5.0, 0.0), core::f32::consts::FRAC_PI_2).expect("an axis with length");
	assert!(near(rotation.length(), 1.0), "the constructor normalised: length is {}", rotation.length());
	// A thousand compositions do not drift into a scale.
	let mut accumulated = Quat::IDENTITY;
	for _ in 0..1000 {
		accumulated = accumulated.mul(rotation);
	}
	assert!(near(accumulated.length(), 1.0), "a thousand products are still unit: length is {}", accumulated.length());
	// And zero has no direction.
	assert_eq!(Quat::from_axis_angle(Vec3::ZERO, 1.0), Err(Error::ZeroLength));
	assert_eq!(Quat::from_components(0.0, 0.0, 0.0, 0.0), Err(Error::ZeroLength));
	assert_eq!(Quat::from_axis_angle(Vec3::new(0.0, 1.0, 0.0), f32::NAN), Err(Error::NotFinite));
}

#[test]
// A POSITIVE ANGLE ABOUT `+Z` TURNS `+X` TOWARDS `+Y`, which is the right-handed rotation and the
// same handedness as the cross product above.
fn a_rotation_turns_the_right_way_and_agrees_with_its_matrix() {
	let quarter = Quat::from_axis_angle(Vec3::new(0.0, 0.0, 1.0), core::f32::consts::FRAC_PI_2).expect("an axis");
	let turned = quarter.rotate(Vec3::new(1.0, 0.0, 0.0));
	assert!(near_vec3(turned, Vec3::new(0.0, 1.0, 0.0)), "+X turned towards +Y, and came out {turned:?}");
	// THE MATRIX FORM IS THE SAME ROTATION. Two representations that disagree are a scene that looks
	// right until something switches between them.
	let matrix = quarter.to_mat3();
	assert!(near_vec3(matrix.mul_vector(Vec3::new(1.0, 0.0, 0.0)), turned));
	// The inverse undoes it.
	assert!(near_vec3(quarter.conjugate().rotate(turned), Vec3::new(1.0, 0.0, 0.0)));
}

#[test]
// SLERP TAKES THE SHORTER ARC. Two quaternions describe every rotation - `q` and `-q` - and
// interpolating towards the wrong one turns nearly the whole way round instead of a few degrees.
fn slerp_takes_the_shorter_arc_whichever_sign_it_is_given() {
	let start = Quat::IDENTITY;
	let eighth = Quat::from_axis_angle(Vec3::new(0.0, 0.0, 1.0), core::f32::consts::FRAC_PI_4).expect("an axis");
	let negated = Quat { x: -eighth.x, y: -eighth.y, z: -eighth.z, w: -eighth.w };
	let half = start.slerp(eighth, 0.5);
	let half_negated = start.slerp(negated, 0.5);
	let sixteenth = Quat::from_axis_angle(Vec3::new(0.0, 0.0, 1.0), core::f32::consts::FRAC_PI_8).expect("an axis");
	assert!(near_vec3(half.rotate(Vec3::new(1.0, 0.0, 0.0)), sixteenth.rotate(Vec3::new(1.0, 0.0, 0.0))));
	assert!(near_vec3(half_negated.rotate(Vec3::new(1.0, 0.0, 0.0)), sixteenth.rotate(Vec3::new(1.0, 0.0, 0.0))), "the negated quaternion is the same rotation and interpolates the same way");
	assert!(near(half.length(), 1.0));
}

#[test]
// THE CAMERA LOOKS ALONG `-Z`, and a point in front of it lands in the frustum. This is the fixture
// that would catch a projection built for a left-handed view space.
fn a_perspective_projection_puts_the_near_plane_at_zero_and_the_far_plane_at_one() {
	let projection = perspective_rh_zo(core::f32::consts::FRAC_PI_2, 1.0, 1.0, 100.0).expect("a frustum");
	// A point ON the near plane is at view-space z = -1.
	let near_point = projection.transform_point(Vec3::new(0.0, 0.0, -1.0)).perspective_divide().expect("w is the depth");
	assert!(near(near_point.z, 0.0), "the near plane is clip depth ZERO, and came out {}", near_point.z);
	let far_point = projection.transform_point(Vec3::new(0.0, 0.0, -100.0)).perspective_divide().expect("w is the depth");
	assert!(near(far_point.z, 1.0), "the far plane is clip depth ONE, and came out {}", far_point.z);
	// A ninety-degree vertical field of view puts the top of the near plane at y = 1 in view space.
	let top = projection.transform_point(Vec3::new(0.0, 1.0, -1.0)).perspective_divide().expect("in front");
	assert!(near(top.y, 1.0), "the top edge is NDC +1, and came out {}", top.y);
	// AND A POINT BEHIND THE CAMERA COMES OUT WITH A NEGATIVE `w`, which is what a clipper tests.
	assert!(projection.transform_point(Vec3::new(0.0, 0.0, 1.0)).w < 0.0, "a point behind the camera is behind the near plane");
}

#[test]
fn a_perspective_projection_refuses_every_frustum_that_is_not_one() {
	let quarter = core::f32::consts::FRAC_PI_2;
	assert_eq!(perspective_rh_zo(f32::NAN, 1.0, 1.0, 2.0), Err(Error::NotFinite));
	assert_eq!(perspective_rh_zo(0.0, 1.0, 1.0, 2.0), Err(Error::DegenerateFrustum), "a field of view of nothing sees nothing");
	assert_eq!(perspective_rh_zo(core::f32::consts::PI, 1.0, 1.0, 2.0), Err(Error::DegenerateFrustum), "and a half turn is not a frustum");
	assert_eq!(perspective_rh_zo(quarter, 0.0, 1.0, 2.0), Err(Error::EmptyViewport));
	assert_eq!(perspective_rh_zo(quarter, 1.0, 0.0, 2.0), Err(Error::DegenerateFrustum), "a near plane at zero is what this transform divides by");
	assert_eq!(perspective_rh_zo(quarter, 1.0, 2.0, 2.0), Err(Error::DegenerateFrustum), "equal planes have no range");
	assert_eq!(perspective_rh_zo(quarter, 1.0, 3.0, 2.0), Err(Error::DegenerateFrustum), "and a reversed pair is not a correction to make silently");
}

#[test]
// THE ORTHOGRAPHIC PROJECTION TAKES ITS PLANES THE SAME WAY THE PERSPECTIVE ONE DOES - distances in
// front of the camera - so switching projections does not move the near plane.
fn an_orthographic_projection_maps_its_box_onto_the_unit_cube() {
	let projection = orthographic_rh_zo(-2.0, 2.0, -1.0, 1.0, 1.0, 5.0).expect("a box");
	let near_middle = projection.transform_point(Vec3::new(0.0, 0.0, -1.0));
	assert!(near(near_middle.z, 0.0), "the near plane is clip depth ZERO");
	let far_middle = projection.transform_point(Vec3::new(0.0, 0.0, -5.0));
	assert!(near(far_middle.z, 1.0), "the far plane is clip depth ONE");
	let corner = projection.transform_point(Vec3::new(2.0, 1.0, -1.0));
	assert!(near(corner.x, 1.0) && near(corner.y, 1.0), "the top right of the box is NDC (+1, +1)");
	assert!(near(projection.transform_point(Vec3::ZERO).w, 1.0), "an orthographic projection does not divide");
	assert_eq!(orthographic_rh_zo(1.0, 1.0, -1.0, 1.0, 1.0, 2.0), Err(Error::EmptyViewport));
	assert_eq!(orthographic_rh_zo(-1.0, 1.0, -1.0, 1.0, 2.0, 1.0), Err(Error::DegenerateFrustum));
}

#[test]
// A VIEW MATRIX PUTS THE EYE AT THE ORIGIN LOOKING ALONG `-Z`, which is the definition of view space
// here and the thing every other camera rule is stated against.
fn a_view_matrix_looks_along_negative_z_and_refuses_a_degenerate_basis() {
	let view = look_at_rh(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0)).expect("a basis");
	let eye_in_view = view.transform_point(Vec3::new(0.0, 0.0, 10.0)).truncate();
	assert!(near_vec3(eye_in_view, Vec3::ZERO), "the eye is the origin of view space, and came out {eye_in_view:?}");
	let target_in_view = view.transform_point(Vec3::ZERO).truncate();
	assert!(near(target_in_view.z, -10.0), "what the camera looks at is along -Z, at {}", target_in_view.z);
	assert!(near(target_in_view.x, 0.0) && near(target_in_view.y, 0.0));
	// A point to the world's right is to the camera's right.
	let right_in_view = view.transform_point(Vec3::new(1.0, 0.0, 10.0)).truncate();
	assert!(near(right_in_view.x, 1.0), "+X stays +X when the camera looks down -Z from +Z");
	// Degenerate configurations refuse rather than collapsing the world.
	assert_eq!(look_at_rh(Vec3::ZERO, Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0)), Err(Error::ZeroLength), "an eye at the target has no direction");
	assert_eq!(look_at_rh(Vec3::new(0.0, 0.0, 1.0), Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0)), Err(Error::ZeroLength), "an up parallel to the view has no right vector");
}

#[test]
// THE Y INVERSION IS IN THE VIEWPORT AND NOWHERE ELSE. NDC `+1` is the TOP of the image and a
// surface's row origin is `TopLeft`, so the top of the image is row zero.
fn the_viewport_inverts_y_so_that_ndc_plus_one_is_the_top_row() {
	let viewport = Viewport::new(0.0, 0.0, 800.0, 600.0);
	let top = window_from_ndc(Vec3::new(0.0, 1.0, 0.0), &viewport).expect("a viewport with area");
	assert!(near(top.y, 0.0), "NDC +1 is the TOP, which is row zero, and came out {}", top.y);
	let bottom = window_from_ndc(Vec3::new(0.0, -1.0, 0.0), &viewport).expect("a viewport");
	assert!(near(bottom.y, 600.0), "NDC -1 is the bottom, which is the last row");
	let middle = window_from_ndc(Vec3::ZERO, &viewport).expect("a viewport");
	assert!(near(middle.x, 400.0) && near(middle.y, 300.0));
	let left = window_from_ndc(Vec3::new(-1.0, 0.0, 0.0), &viewport).expect("a viewport");
	assert!(near(left.x, 0.0), "X is NOT inverted");
	// The depth range is applied as stated.
	let deep = window_from_ndc(Vec3::new(0.0, 0.0, 1.0), &Viewport { min_depth: 0.25, max_depth: 0.75, ..viewport }).expect("a viewport");
	assert!(near(deep.z, 0.75));
	assert_eq!(window_from_ndc(Vec3::ZERO, &Viewport::new(0.0, 0.0, 0.0, 600.0)), Err(Error::EmptyViewport));
}

#[test]
// THE ONE THAT ONLY MAKES SENSE WITH ALL FOUR CHOICES TOGETHER, and the reason this crate states the
// ordering rather than the winding alone.
//
// A triangle wound counter-clockwise in NDC is the FRONT. Run the same three vertices through the
// viewport's Y inversion and their apparent winding REVERSES - so a renderer that culled after the
// inversion would cull exactly the faces it should keep, while satisfying every other sentence in
// this crate. This fixture asserts both halves: the facing before, and the reversal after.
fn front_faces_are_counter_clockwise_in_ndc_and_the_viewport_reverses_that() {
	let (a, b, c) = (Vec3::new(-0.5, -0.5, 0.0), Vec3::new(0.5, -0.5, 0.0), Vec3::new(0.0, 0.5, 0.0));
	assert_eq!(facing(a, b, c), Facing::Front, "counter-clockwise in NDC with +Y up is the front");
	assert_eq!(facing(a, c, b), Facing::Back, "and the other order is the back");

	let viewport = Viewport::new(0.0, 0.0, 100.0, 100.0);
	let to_window = |point: Vec3| window_from_ndc(point, &viewport).expect("a viewport");
	let (wa, wb, wc) = (to_window(a), to_window(b), to_window(c));
	assert!(winding::signed_area_doubled(wa, wb, wc) < 0.0, "the same triangle is CLOCKWISE in window coordinates - which is why the facing is decided before this, and the whole reason this fixture exists");

	// A degenerate triangle picks no side: it has no area to draw with either way.
	assert_eq!(facing(a, a, c), Facing::Degenerate);
	assert_eq!(facing(a, b, Vec3::new(f32::NAN, 0.0, 0.0)), Facing::Degenerate, "and a vertex that is not a number is discarded rather than guessed at");
}

#[test]
// THE WHOLE CHAIN, from a world-space point to a window pixel, through every convention at once.
// Each of the pieces above is checked alone; this is the one that fails if two of them are each
// consistent and wrong together.
fn a_world_point_reaches_the_pixel_every_convention_together_says_it_should() {
	// A camera ten units back on +Z looking at the origin, 90 degrees vertical, square viewport.
	let view = look_at_rh(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0)).expect("a basis");
	let projection = perspective_rh_zo(core::f32::consts::FRAC_PI_2, 1.0, 1.0, 100.0).expect("a frustum");
	let viewport = Viewport::new(0.0, 0.0, 200.0, 200.0);
	let to_pixel = |world: Vec3| {
		let clip = projection.mul(&view).transform_point(world);
		let ndc = clip.perspective_divide().expect("in front of the camera");
		window_from_ndc(ndc, &viewport).expect("a viewport")
	};
	// The origin is the centre of the image.
	let centre = to_pixel(Vec3::ZERO);
	assert!(near(centre.x, 100.0) && near(centre.y, 100.0), "the point the camera looks at is the centre pixel, and came out ({}, {})", centre.x, centre.y);
	// A point ABOVE the origin is ABOVE the centre, which on a top-left origin means a SMALLER row.
	let above = to_pixel(Vec3::new(0.0, 1.0, 0.0));
	assert!(above.y < centre.y, "world +Y is up, and up is a smaller row: {} against {}", above.y, centre.y);
	// A point to the RIGHT is at a larger column.
	let right = to_pixel(Vec3::new(1.0, 0.0, 0.0));
	assert!(right.x > centre.x, "world +X is right, and right is a larger column");
	// And a point further away is deeper.
	assert!(to_pixel(Vec3::new(0.0, 0.0, -5.0)).z > centre.z, "further from the camera is a larger depth, because the near plane is zero");
}

#[test]
// AN INFINITE FAR PLANE IS THE LIMIT OF THE FINITE ONE, and with a `[0, 1]` depth range it is well
// conditioned rather than a trick: the depth precision is spent near the camera either way.
fn an_infinite_far_plane_is_the_limit_of_a_finite_one() {
	let infinite = camera::perspective_infinite_rh_zo(core::f32::consts::FRAC_PI_2, 1.0, 0.5).unwrap();
	let distant = camera::perspective_rh_zo(core::f32::consts::FRAC_PI_2, 1.0, 0.5, 1.0e9).unwrap();
	for index in 0..16 {
		let (left, right) = (infinite.to_array()[index], distant.to_array()[index]);
		assert!((left - right).abs() <= 1e-6, "element {index} differs: {left} against {right}");
	}
	// The near plane still lands at depth 0 exactly, which is what the clip volume requires.
	let on_near = infinite.transform_point(Vec3::new(0.0, 0.0, -0.5)).perspective_divide().unwrap();
	assert!(on_near.z.abs() <= 1e-6, "the near plane is depth zero: {}", on_near.z);
	// And a point a long way off approaches depth 1 from below, so nothing is ever clipped by a far
	// plane. At ten thousand near-lengths the value is still distinct from 1 in `f32`; beyond that it
	// rounds to 1 and the assertion is that it never EXCEEDS it.
	let far_away = infinite.transform_point(Vec3::new(0.0, 0.0, -1.0e4)).perspective_divide().unwrap();
	assert!(far_away.z < 1.0 && far_away.z > 0.9999, "a distant point is inside the volume: {}", far_away.z);
	let very_far = infinite.transform_point(Vec3::new(0.0, 0.0, -1.0e12)).perspective_divide().unwrap();
	assert!(very_far.z <= 1.0, "nothing ever leaves the volume through a far plane: {}", very_far.z);

	assert!(camera::perspective_infinite_rh_zo(core::f32::consts::FRAC_PI_2, 1.0, 0.0).is_err());
	assert!(camera::perspective_infinite_rh_zo(core::f32::consts::FRAC_PI_2, 0.0, 0.5).is_err());
	assert!(camera::perspective_infinite_rh_zo(f32::NAN, 1.0, 0.5).is_err());
}

#[test]
// `ln` AGAINST `f64::ln`, ACROSS TWELVE ORDERS OF MAGNITUDE. A logarithm wrong in the sixth digit is
// a cascade split in the wrong place, which is a shadow map at the wrong resolution over a band of
// the view - visible as a seam and traced to anything but the logarithm.
fn the_natural_logarithm_matches_the_definition() {
	for value in [1.0e-6f64, 1.0e-3, 0.1, 0.5, 0.9999, 1.0, 1.0001, 1.5, 2.0, core::f64::consts::E, 10.0, 100.0, 1000.0, 65536.0, 1.0e6] {
		let ours = vector::ln(value as f32) as f64;
		let theirs = value.ln();
		assert!((ours - theirs).abs() <= 1e-5 * theirs.abs().max(1.0), "ln({value}) is {ours} and should be {theirs}");
	}
	assert_eq!(vector::ln(1.0), 0.0, "the logarithm of one is exactly zero");
	assert_eq!(vector::ln(0.0), f32::NEG_INFINITY);
	assert!(vector::ln(-1.0).is_nan(), "a negative has no real logarithm");
	assert_eq!(vector::ln(f32::INFINITY), f32::INFINITY);
	// A SUBNORMAL IS SCALED INTO RANGE FIRST. Its stored exponent is zero and its mantissa is not
	// the number's, so reading the bits as a normal would answer for a different value entirely.
	let subnormal = f32::from_bits(1);
	assert!((vector::ln(subnormal) as f64 - (subnormal as f64).ln()).abs() < 1e-3, "a subnormal is answered for itself");
}

#[test]
// `exp` AGAINST `f64::exp`, INCLUDING THE ENDS. The fog equation is `exp(-(density * distance)^2)`,
// so the arguments a scene actually produces are large and negative, and the answer there has to be
// a clean zero rather than a denormal that costs a hundred cycles a pixel.
fn the_exponential_matches_the_definition_and_saturates_cleanly() {
	for value in [-20.0f64, -5.0, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0, 5.0, 20.0, 50.0, 80.0] {
		let ours = vector::exp(value as f32) as f64;
		let theirs = value.exp();
		assert!((ours - theirs).abs() <= 1e-5 * theirs.abs().max(1.0e-6), "exp({value}) is {ours} and should be {theirs}");
	}
	assert_eq!(vector::exp(0.0), 1.0, "e to the nothing is exactly one");
	assert_eq!(vector::exp(1000.0), f32::INFINITY, "past the format's range it saturates");
	assert_eq!(vector::exp(-1000.0), 0.0, "and at the other end it is a clean zero");
	assert!(vector::exp(f32::NAN).is_nan());
	// ROUND TRIP, which catches a wrong `ln(2)` in either function: they would have to be wrong by
	// the same amount in opposite directions to pass this.
	for value in [0.25f32, 1.0, 7.5, 1000.0] {
		let round_trip = vector::exp(vector::ln(value));
		assert!((round_trip - value).abs() <= 1e-4 * value, "exp(ln({value})) is {round_trip}");
	}
}

#[test]
// `powf` IS `exp(y * ln(x))` WITH THE TWO LIMITS THAT ARE NOT. A power of zero is one for every base
// and a base of zero is zero for every positive power; computing either through the logarithm gives
// a NaN and an infinity, and both are what a caller actually means.
fn a_real_power_matches_the_definition_and_answers_its_limits() {
	for (base, exponent) in [(2.0f64, 10.0f64), (2.0, 0.5), (10.0, -2.0), (1.5, 3.0), (100.0, 0.25), (0.5, 8.0)] {
		let ours = vector::powf(base as f32, exponent as f32) as f64;
		let theirs = base.powf(exponent);
		assert!((ours - theirs).abs() <= 1e-4 * theirs.abs().max(1.0), "{base}^{exponent} is {ours} and should be {theirs}");
	}
	assert_eq!(vector::powf(0.0, 0.0), 1.0, "anything to the nothing is one, including nothing");
	assert_eq!(vector::powf(1.0e30, 0.0), 1.0);
	assert_eq!(vector::powf(0.0, 2.0), 0.0);
	assert_eq!(vector::powf(0.0, -2.0), f32::INFINITY);
	assert!(vector::powf(-2.0, 0.5).is_nan(), "a negative base has no real root here");
	// THE SQUARE ROOT BY EITHER ROUTE AGREES, which is the cheapest check that the two series are
	// consistent with the Newton iteration beside them.
	for value in [2.0f32, 7.0, 1000.0] {
		assert!((vector::powf(value, 0.5) - vector::sqrt(value)).abs() <= 1e-3 * value.max(1.0), "the two routes to a square root agree at {value}");
	}
}
