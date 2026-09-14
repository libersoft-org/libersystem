//! BOUNDING VOLUMES: a box a mesh can state exactly, and a sphere that survives being transformed.
//!
//! THE BOX IS IN LOCAL SPACE AND THE SPHERE IS IN WORLD SPACE, which is `Scene3D Core Profile 1`'s
//! own division and not a convenience. A mesh knows its own extent exactly, so its bound is a box;
//! but re-fitting a box to a rotated mesh every frame makes the box GROW - the rotated box's axis
//! bound is larger than the original, and re-fitting that gives a larger one again, until after a
//! few hundred frames everything is visible. A sphere has no orientation, so transforming it is
//! idempotent.
//!
//! THE RADIUS TAKES THE LARGEST OF THE THREE AXIS SCALES. The average would produce a sphere smaller
//! than the geometry it claims to contain, which is a cull that removes something visible - the one
//! failure mode culling must not have.

use render_math::{Mat4, Vec3};

/// A bounding sphere.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Sphere {
	pub centre: Vec3,
	pub radius: f32,
}

impl Sphere {
	pub const fn new(centre: Vec3, radius: f32) -> Self {
		Self { centre, radius }
	}

	/// Whether the sphere is a sphere: finite, with a radius at or above zero.
	pub fn is_well_formed(&self) -> bool {
		self.centre.is_finite() && self.radius.is_finite() && self.radius >= 0.0
	}

	/// The sphere around this one after a transform.
	pub fn transformed(&self, by: &Mat4) -> Self {
		Self { centre: by.transform_point(self.centre).truncate(), radius: self.radius * largest_axis_scale(by) }
	}

	/// The smallest sphere containing both.
	///
	/// THE CONTAINMENT CASE IS SHORT-CIRCUITED, because the general formula applied to a sphere that
	/// already contains the other produces one slightly LARGER than the container - and a union built
	/// up over a hierarchy would then grow with every level for no reason.
	pub fn union(&self, other: &Self) -> Self {
		let between = other.centre.sub(self.centre);
		let distance = between.length();
		if distance + other.radius <= self.radius {
			return *self;
		}
		if distance + self.radius <= other.radius {
			return *other;
		}
		let radius = (distance + self.radius + other.radius) * 0.5;
		// The new centre sits on the line between them, `radius - self.radius` along it.
		let along = if distance > 0.0 { (radius - self.radius) / distance } else { 0.0 };
		Self { centre: self.centre.add(between.scale(along)), radius }
	}
}

/// An axis-aligned bounding box.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Aabb {
	pub minimum: Vec3,
	pub maximum: Vec3,
}

impl Aabb {
	pub const fn new(minimum: Vec3, maximum: Vec3) -> Self {
		Self { minimum, maximum }
	}

	pub fn centre(&self) -> Vec3 {
		self.minimum.add(self.maximum).scale(0.5)
	}

	pub fn half_extent(&self) -> Vec3 {
		self.maximum.sub(self.minimum).scale(0.5)
	}

	/// Whether the box is a box: finite, and with every minimum at or below its maximum.
	///
	/// AN INVERTED BOX IS NOT AN EMPTY ONE. The slab test reads a negative extent as a box turned
	/// inside out and reports a miss; the p/n-vertex test reads the same box and reports it visible.
	/// It is a caller's mistake, and the scene refuses it where it enters rather than leaving each
	/// test to guess what was meant.
	pub fn is_well_formed(&self) -> bool {
		self.minimum.is_finite() && self.maximum.is_finite() && self.minimum.x <= self.maximum.x && self.minimum.y <= self.maximum.y && self.minimum.z <= self.maximum.z
	}

	/// The box around the transformed box.
	///
	/// THE ABSOLUTE VALUE OF THE LINEAR PART APPLIED TO THE HALF-EXTENT, which is the same answer as
	/// transforming eight corners and taking their bound, in a third of the arithmetic: a rotation
	/// spreads each half-extent component across the axes, and the magnitude of that spread is what
	/// the absolute value gives.
	///
	/// USED FOR A ONE-OFF QUESTION AND NOT PER FRAME. Re-fitting a box each frame is the growing
	/// bound this module's own header warns about; a drawable's per-frame volume is `world_sphere`.
	pub fn transformed(&self, by: &Mat4) -> Self {
		let centre = by.transform_point(self.centre()).truncate();
		let half = self.half_extent();
		let linear = by.linear();
		let spread = Vec3::new(linear.at(0, 0).abs() * half.x + linear.at(0, 1).abs() * half.y + linear.at(0, 2).abs() * half.z, linear.at(1, 0).abs() * half.x + linear.at(1, 1).abs() * half.y + linear.at(1, 2).abs() * half.z, linear.at(2, 0).abs() * half.x + linear.at(2, 1).abs() * half.y + linear.at(2, 2).abs() * half.z);
		Self { minimum: centre.sub(spread), maximum: centre.add(spread) }
	}

	/// The sphere around this box, in the box's own space.
	pub fn bounding_sphere(&self) -> Sphere {
		Sphere { centre: self.centre(), radius: self.half_extent().length() }
	}

	/// THE WORLD SPHERE OF A LOCAL BOX, which is the derivation the profile fixes: the local centre
	/// transformed, and the local radius scaled by the largest of the three axis scales.
	pub fn world_sphere(&self, world: &Mat4) -> Sphere {
		self.bounding_sphere().transformed(world)
	}

	pub fn union(&self, other: &Self) -> Self {
		Self { minimum: Vec3::new(self.minimum.x.min(other.minimum.x), self.minimum.y.min(other.minimum.y), self.minimum.z.min(other.minimum.z)), maximum: Vec3::new(self.maximum.x.max(other.maximum.x), self.maximum.y.max(other.maximum.y), self.maximum.z.max(other.maximum.z)) }
	}
}

/// The largest of the three axis scale factors of a transform: the length of the longest column of
/// its linear part.
pub fn largest_axis_scale(matrix: &Mat4) -> f32 {
	let linear = matrix.linear();
	let mut largest = 0.0_f32;
	for axis in 0..3 {
		let length = linear.column(axis).length();
		if length > largest {
			largest = length;
		}
	}
	largest
}

/// THE NORMAL MATRIX: the cofactor matrix of the upper 3x3, which is its inverse transpose times the
/// determinant.
///
/// THE MATRIX ITSELF IS CORRECT ONLY FOR A UNIFORM SCALE. Under a squash, transforming a normal by
/// the same matrix as the position tilts it the wrong way, and the lighting on one squashed object
/// in a scene is subtly wrong in a way nothing else in the frame shows.
///
/// THE COFACTOR AND NOT THE INVERSE TRANSPOSE DIRECTLY. They differ by the determinant, which is a
/// scale the caller removes by renormalising - and the cofactor form needs no division, so it has an
/// answer for a singular transform where the inverse has none.
pub fn normal_matrix(world: &Mat4) -> render_math::Mat3 {
	let m = world.linear();
	let cofactor = |row: usize, column: usize| {
		let rows = [(row + 1) % 3, (row + 2) % 3];
		let columns = [(column + 1) % 3, (column + 2) % 3];
		m.at(rows[0], columns[0]) * m.at(rows[1], columns[1]) - m.at(rows[0], columns[1]) * m.at(rows[1], columns[0])
	};
	render_math::Mat3::from_columns(Vec3::new(cofactor(0, 0), cofactor(1, 0), cofactor(2, 0)), Vec3::new(cofactor(0, 1), cofactor(1, 1), cofactor(2, 1)), Vec3::new(cofactor(0, 2), cofactor(1, 2), cofactor(2, 2)))
}
