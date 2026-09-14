//! Cameras and the viewport, with every convention in the constructor's NAME.
//!
//! `perspective_rh_zo` SAYS WHAT IT ENCODES: right-handed view space, and a zero-to-one clip depth.
//! There is no `perspective`, because a generically named constructor is how a caller picks the
//! wrong convention and finds out three layers later, when the depth test is inverted or the world
//! is mirrored. A name that carries the convention cannot be called by accident.
//!
//! THE CAMERA LOOKS ALONG `-Z` and `+Y` IS UP, which is what "right-handed view space" means once
//! the up axis is fixed - and fixing it is the point of this crate.

use crate::matrix::Mat4;
use crate::vector::{Vec3, Vec4};
use crate::{Error, ZERO_LENGTH_EPSILON, all_finite};

/// A perspective projection: right-handed view space, clip depth in `[0, 1]`.
///
/// `vertical_fov_radians` is the FULL vertical field of view - not the half-angle - because that is
/// the number a camera is described by, and halving it is the one arithmetic mistake this signature
/// can still admit. `aspect` is width over height.
///
/// REFUSES a non-finite argument, a field of view at or past a half turn, an aspect of zero, and a
/// frustum that cannot describe a depth range - including `near` at zero, which this transform
/// divides by.
pub fn perspective_rh_zo(vertical_fov_radians: f32, aspect: f32, near: f32, far: f32) -> Result<Mat4, Error> {
	if !all_finite(&[vertical_fov_radians, aspect, near, far]) {
		return Err(Error::NotFinite);
	}
	if vertical_fov_radians <= 0.0 || vertical_fov_radians >= core::f32::consts::PI {
		return Err(Error::DegenerateFrustum);
	}
	if aspect <= 0.0 {
		return Err(Error::EmptyViewport);
	}
	if !(near > 0.0) || !(far > near) {
		return Err(Error::DegenerateFrustum);
	}
	let (sine, cosine) = crate::quaternion::sin_cos(vertical_fov_radians * 0.5);
	if sine.abs() <= ZERO_LENGTH_EPSILON {
		return Err(Error::DegenerateFrustum);
	}
	// `cot(fov/2)`, which is the focal length in NDC units.
	let focal = cosine / sine;
	let range = far - near;
	if !(range > 0.0) {
		return Err(Error::DegenerateFrustum);
	}
	// Column-major. The `-1` in column 2 row 3 is what makes `w` the view-space DEPTH of the point,
	// which is the whole of "right-handed, looking along -Z": a point in front of the camera has a
	// negative `z` in view space and must come out with a positive `w`.
	Ok(Mat4::from_columns(Vec4::new(focal / aspect, 0.0, 0.0, 0.0), Vec4::new(0.0, focal, 0.0, 0.0), Vec4::new(0.0, 0.0, far / (near - far), -1.0), Vec4::new(0.0, 0.0, (near * far) / (near - far), 0.0)))
}

/// A perspective projection with the far plane AT INFINITY.
///
/// WELL CONDITIONED, NOT A HACK. With a `[0, 1]` depth range the depth precision is spent near the
/// camera whatever the far plane is, so moving it to infinity costs nothing measurable and removes
/// the far clip entirely - which is what a scene with a sky, a horizon or a procedurally extended
/// world wants. The matrix is the limit of `perspective_rh_zo` as `far` grows: `far / (near - far)`
/// tends to `-1` and `near * far / (near - far)` tends to `-near`.
///
/// REFUSES the same degenerate arguments as the finite form, `near` at zero included.
pub fn perspective_infinite_rh_zo(vertical_fov_radians: f32, aspect: f32, near: f32) -> Result<Mat4, Error> {
	if !all_finite(&[vertical_fov_radians, aspect, near]) {
		return Err(Error::NotFinite);
	}
	if vertical_fov_radians <= 0.0 || vertical_fov_radians >= core::f32::consts::PI {
		return Err(Error::DegenerateFrustum);
	}
	if aspect <= 0.0 {
		return Err(Error::EmptyViewport);
	}
	if !(near > 0.0) {
		return Err(Error::DegenerateFrustum);
	}
	let (sine, cosine) = crate::quaternion::sin_cos(vertical_fov_radians * 0.5);
	if sine.abs() <= ZERO_LENGTH_EPSILON {
		return Err(Error::DegenerateFrustum);
	}
	let focal = cosine / sine;
	Ok(Mat4::from_columns(Vec4::new(focal / aspect, 0.0, 0.0, 0.0), Vec4::new(0.0, focal, 0.0, 0.0), Vec4::new(0.0, 0.0, -1.0, -1.0), Vec4::new(0.0, 0.0, -near, 0.0)))
}

/// An orthographic projection: right-handed view space, clip depth in `[0, 1]`.
///
/// `near` and `far` are DISTANCES along the view direction, positive in front of the camera, the
/// same way `perspective_rh_zo` takes them - so the two constructors can be swapped for one another
/// without moving the near plane, which is what a caller switching projections expects.
pub fn orthographic_rh_zo(left: f32, right: f32, bottom: f32, top: f32, near: f32, far: f32) -> Result<Mat4, Error> {
	if !all_finite(&[left, right, bottom, top, near, far]) {
		return Err(Error::NotFinite);
	}
	if !(right > left) || !(top > bottom) {
		return Err(Error::EmptyViewport);
	}
	if !(far > near) {
		return Err(Error::DegenerateFrustum);
	}
	let (width, height, range) = (right - left, top - bottom, far - near);
	Ok(Mat4::from_columns(Vec4::new(2.0 / width, 0.0, 0.0, 0.0), Vec4::new(0.0, 2.0 / height, 0.0, 0.0), Vec4::new(0.0, 0.0, -1.0 / range, 0.0), Vec4::new(-(right + left) / width, -(top + bottom) / height, -near / range, 1.0)))
}

/// A view matrix: the world seen from `eye`, looking at `target`, with `up` deciding the roll.
///
/// RIGHT-HANDED, so the basis it builds has the camera looking along `-Z`. REFUSES a degenerate
/// configuration rather than producing a matrix with a zero column: an eye at the target has no
/// direction, and an `up` parallel to that direction leaves the right vector undefined - both of
/// which produce a view that is a singular matrix and a world that has collapsed.
pub fn look_at_rh(eye: Vec3, target: Vec3, up: Vec3) -> Result<Mat4, Error> {
	if !(eye.is_finite() && target.is_finite() && up.is_finite()) {
		return Err(Error::NotFinite);
	}
	// `back` is `+Z` of the camera: the direction it looks AWAY along, which is what makes the basis
	// right-handed with the camera looking down `-Z`.
	let back = eye.sub(target).normalise()?;
	let right = up.cross(back).normalise()?;
	let true_up = back.cross(right);
	Ok(Mat4::from_columns(Vec4::new(right.x, true_up.x, back.x, 0.0), Vec4::new(right.y, true_up.y, back.y, 0.0), Vec4::new(right.z, true_up.z, back.z, 0.0), Vec4::new(-right.dot(eye), -true_up.dot(eye), -back.dot(eye), 1.0)))
}

/// A viewport in window pixels, with the depth range a fragment's `z` is mapped into.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Viewport {
	pub x: f32,
	pub y: f32,
	pub width: f32,
	pub height: f32,
	/// Where NDC depth `0` lands. Default `0.0`.
	pub min_depth: f32,
	/// Where NDC depth `1` lands. Default `1.0`.
	pub max_depth: f32,
}

impl Viewport {
	pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
		Self { x, y, width, height, min_depth: 0.0, max_depth: 1.0 }
	}
}

/// Normalised device coordinates to window coordinates.
///
/// THE Y INVERSION IS HERE AND NOWHERE ELSE, and it is the last of the four choices that only become
/// unambiguous together: NDC `+1` is the TOP of the image, a surface's row origin is `TopLeft`, so
/// the mapping is
///
/// ```text
///   x_window = x + (x_ndc * 0.5 + 0.5) * width
///   y_window = y + (1 - (y_ndc * 0.5 + 0.5)) * height
/// ```
///
/// FRONT-FACE CULLING HAPPENS BEFORE THIS. A Y flip reverses apparent winding, so a renderer that
/// culled after it would cull the opposite faces while satisfying every other rule in this crate.
/// `winding::facing` takes NDC positions for that reason, and this function's own documentation is
/// where the ordering is stated.
pub fn window_from_ndc(ndc: Vec3, viewport: &Viewport) -> Result<Vec3, Error> {
	if !ndc.is_finite() || !all_finite(&[viewport.x, viewport.y, viewport.width, viewport.height, viewport.min_depth, viewport.max_depth]) {
		return Err(Error::NotFinite);
	}
	if !(viewport.width > 0.0) || !(viewport.height > 0.0) {
		return Err(Error::EmptyViewport);
	}
	let x = viewport.x + (ndc.x * 0.5 + 0.5) * viewport.width;
	let y = viewport.y + (1.0 - (ndc.y * 0.5 + 0.5)) * viewport.height;
	let depth = viewport.min_depth + ndc.z * (viewport.max_depth - viewport.min_depth);
	Ok(Vec3::new(x, y, depth))
}
