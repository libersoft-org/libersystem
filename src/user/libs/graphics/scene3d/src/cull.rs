//! FRUSTUM CULLING, with the planes taken from the matrix rather than from the camera's numbers.
//!
//! EXTRACTED FROM THE VIEW-PROJECTION MATRIX, which is the only way it stays correct when the matrix
//! is something the camera's fields cannot describe: an oblique near plane, an infinite far plane, a
//! projection built by hand. A culler that rebuilt the planes from a field of view and an aspect
//! would cull against a different frustum than the one the vertices are clipped against - and the
//! difference is geometry that disappears near the edge of the screen.
//!
//! THE PLANE ORDER IS NEAR, FAR, LEFT, RIGHT, BOTTOM, TOP, which is `render3d`'s clip order with the
//! `w` plane dropped, and the profile fixes it: a volume outside two planes is rejected by whichever
//! is tested first, so two implementations that disagree about the order name different planes in
//! their reports. Near first because it rejects the most in an ordinary scene.
//!
//! CULLING IS CONSERVATIVE AND SAYS SO. `Visibility::Outside` means the volume cannot intersect the
//! frustum; `Inside` means it cannot leave it; `Intersecting` means it might. A culler that answered
//! only in and out would have to pick a side for the third case, and picking "out" drops geometry -
//! which is the one failure mode culling must not have, because culling is observable only in
//! performance.

use render_math::{Mat4, Vec3, Vec4};

use crate::bounds::{Aabb, Sphere};

/// What a cull decided.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Visibility {
	/// Wholly inside every plane. Nothing it contains needs testing again.
	Inside,
	/// Crossing at least one plane.
	Intersecting,
	/// Wholly outside at least one plane, so it cannot intersect the frustum at all.
	Outside,
}

/// Which plane a volume was rejected by. The order is the test order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
	Near,
	Far,
	Left,
	Right,
	Bottom,
	Top,
}

/// The six planes in test order, so a report can name the one that rejected a volume.
pub const SIDES: [Side; 6] = [Side::Near, Side::Far, Side::Left, Side::Right, Side::Bottom, Side::Top];

/// Six planes, as `ax + by + cz + d = 0` with the normal pointing INSIDE.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Frustum {
	planes: [Vec4; 6],
}

impl Frustum {
	/// Extract the planes from a view-projection matrix.
	///
	/// THE ROWS OF THE MATRIX ARE THE CLIP-SPACE PLANES. A point is inside `x >= -w` when
	/// `row3 + row0` dotted with it is non-negative, and so on around the volume; the near plane is
	/// `row2` ALONE because this stack's clip depth is `[0, 1]` rather than `[-1, 1]` - which is the
	/// one line a culler written for the other convention gets wrong, and it culls everything near
	/// the camera.
	pub fn from_view_projection(matrix: &Mat4) -> Self {
		// The rows, which for a column-major matrix are taken across the columns.
		let row = |index: usize| Vec4::new(matrix.at(index, 0), matrix.at(index, 1), matrix.at(index, 2), matrix.at(index, 3));
		let (x, y, z, w) = (row(0), row(1), row(2), row(3));
		let planes = [
			normalise(z),        // near:   z >= 0, the zero-to-one depth range
			normalise(w.sub(z)), // far:    z <= w
			normalise(w.add(x)), // left:   x >= -w
			normalise(w.sub(x)), // right:  x <= w
			normalise(w.add(y)), // bottom: y >= -w
			normalise(w.sub(y)), // top:    y <= w
		];
		Self { planes }
	}

	/// The signed distance from a point to a plane, positive inside.
	fn distance(plane: Vec4, point: Vec3) -> f32 {
		plane.x * point.x + plane.y * point.y + plane.z * point.z + plane.w
	}

	/// Test a sphere. THE REJECTION IS `dot(n, centre) + d < -radius`, which needs the plane
	/// normalised: the comparison is against a radius in world units.
	pub fn test_sphere(&self, sphere: &Sphere) -> Visibility {
		match self.rejected_by(sphere) {
			Err(_) => Visibility::Outside,
			Ok(crossing) if crossing => Visibility::Intersecting,
			Ok(_) => Visibility::Inside,
		}
	}

	/// The same test, reporting WHICH plane rejected the sphere - the first one in the fixed order
	/// that it lies wholly outside of. `Ok(true)` means it crosses at least one.
	pub fn rejected_by(&self, sphere: &Sphere) -> Result<bool, Side> {
		let mut crossing = false;
		for (index, plane) in self.planes.iter().enumerate() {
			let distance = Self::distance(*plane, sphere.centre);
			if distance < -sphere.radius {
				return Err(SIDES[index]);
			}
			if distance < sphere.radius {
				crossing = true;
			}
		}
		Ok(crossing)
	}

	/// Test an axis-aligned box.
	///
	/// THE p/n-VERTEX TEST: for each plane, the corner FURTHEST along its normal decides "outside"
	/// and the corner furthest against it decides "inside". Testing the centre and a radius - which
	/// is the sphere test on the box's bounding sphere - is correct and much looser, and the
	/// difference is a box near a corner of the screen that is drawn for no reason.
	///
	/// USED FOR A ONE-OFF QUESTION. A drawable's per-frame volume is its world sphere, because a box
	/// re-fitted every frame grows; this is for a static box, a light's volume or a caller's own
	/// query.
	pub fn test_aabb(&self, box_: &Aabb) -> Visibility {
		let mut crossing = false;
		for plane in &self.planes {
			let positive = Vec3::new(if plane.x >= 0.0 { box_.maximum.x } else { box_.minimum.x }, if plane.y >= 0.0 { box_.maximum.y } else { box_.minimum.y }, if plane.z >= 0.0 { box_.maximum.z } else { box_.minimum.z });
			if Self::distance(*plane, positive) < 0.0 {
				return Visibility::Outside;
			}
			let negative = Vec3::new(if plane.x >= 0.0 { box_.minimum.x } else { box_.maximum.x }, if plane.y >= 0.0 { box_.minimum.y } else { box_.maximum.y }, if plane.z >= 0.0 { box_.minimum.z } else { box_.maximum.z });
			if Self::distance(*plane, negative) < 0.0 {
				crossing = true;
			}
		}
		if crossing { Visibility::Intersecting } else { Visibility::Inside }
	}

	pub fn planes(&self) -> &[Vec4; 6] {
		&self.planes
	}
}

/// A plane with a unit normal, so a distance is a DISTANCE rather than a scaled one.
///
/// WITHOUT THIS THE SPHERE TEST IS WRONG. Comparing an unnormalised distance against a radius
/// compares two different units, and the error is largest exactly where a projection's planes are
/// most skewed - at the edges of a wide field of view.
fn normalise(plane: Vec4) -> Vec4 {
	let length = Vec3::new(plane.x, plane.y, plane.z).length();
	if length <= 0.0 { plane } else { plane.scale(1.0 / length) }
}
