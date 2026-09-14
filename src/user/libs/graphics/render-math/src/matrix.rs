//! Matrices, COLUMN-MAJOR, transforming a column vector on the right: `M * v`.
//!
//! WHY THE STORAGE ORDER IS PART OF THE CONTRACT AND NOT AN IMPLEMENTATION DETAIL. A matrix leaves
//! this crate as sixteen floats - into a uniform buffer, a shader, a serialised scene - and the
//! receiver reads them in SOME order. Row-major and column-major storage of the same transform are
//! each other's transpose, which for a rotation is its inverse: a composition that is off by this is
//! not subtly wrong, it turns the wrong way. So the order is frozen here, `to_array` documents it,
//! and `column` is the accessor a reader reaches for.
//!
//! `m.column(3)` IS THE TRANSLATION, which is the property that makes this order the one to pick:
//! the three basis vectors and the origin are four contiguous columns, and every consumer that wants
//! one of them takes a slice rather than a stride.

use crate::vector::{Vec3, Vec4};
use crate::{Error, SINGULAR_EPSILON, all_finite};

/// A 3x3 matrix: a rotation, a scale, or the linear part of an affine transform.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Mat3 {
	/// Three columns of three. `columns[c][r]` is row `r` of column `c`.
	columns: [[f32; 3]; 3],
}

impl Mat3 {
	pub const IDENTITY: Self = Self { columns: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] };

	pub const fn from_columns(x: Vec3, y: Vec3, z: Vec3) -> Self {
		Self { columns: [[x.x, x.y, x.z], [y.x, y.y, y.z], [z.x, z.y, z.z]] }
	}

	pub const fn column(&self, index: usize) -> Vec3 {
		let column = self.columns[index];
		Vec3 { x: column[0], y: column[1], z: column[2] }
	}

	/// Row `row`, column `column`, which is the mathematical subscript rather than the storage one.
	pub const fn at(&self, row: usize, column: usize) -> f32 {
		self.columns[column][row]
	}

	/// COLUMN-MAJOR, sixteen floats in the order a consumer reads them: column 0 first.
	pub const fn to_array(&self) -> [f32; 9] {
		let [x, y, z] = self.columns;
		[x[0], x[1], x[2], y[0], y[1], y[2], z[0], z[1], z[2]]
	}

	pub fn is_finite(&self) -> bool {
		all_finite(&self.to_array())
	}

	pub fn mul_vector(&self, vector: Vec3) -> Vec3 {
		self.column(0).scale(vector.x).add(self.column(1).scale(vector.y)).add(self.column(2).scale(vector.z))
	}

	pub fn mul(&self, other: &Self) -> Self {
		Self::from_columns(self.mul_vector(other.column(0)), self.mul_vector(other.column(1)), self.mul_vector(other.column(2)))
	}

	pub fn transpose(&self) -> Self {
		let mut out = [[0.0f32; 3]; 3];
		let mut row = 0;
		while row < 3 {
			let mut column = 0;
			while column < 3 {
				out[row][column] = self.columns[column][row];
				column += 1;
			}
			row += 1;
		}
		Self { columns: out }
	}

	pub fn determinant(&self) -> f32 {
		let (x, y, z) = (self.column(0), self.column(1), self.column(2));
		x.dot(y.cross(z))
	}
}

/// A 4x4 matrix: an affine transform, or a projection.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Mat4 {
	columns: [[f32; 4]; 4],
}

impl Mat4 {
	pub const IDENTITY: Self = Self { columns: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]] };

	pub const fn from_columns(x: Vec4, y: Vec4, z: Vec4, w: Vec4) -> Self {
		Self { columns: [[x.x, x.y, x.z, x.w], [y.x, y.y, y.z, y.w], [z.x, z.y, z.z, z.w], [w.x, w.y, w.z, w.w]] }
	}

	pub const fn column(&self, index: usize) -> Vec4 {
		let column = self.columns[index];
		Vec4 { x: column[0], y: column[1], z: column[2], w: column[3] }
	}

	pub const fn at(&self, row: usize, column: usize) -> f32 {
		self.columns[column][row]
	}

	/// COLUMN-MAJOR, in the order a uniform buffer or a shader reads them.
	pub const fn to_array(&self) -> [f32; 16] {
		let [x, y, z, w] = self.columns;
		[x[0], x[1], x[2], x[3], y[0], y[1], y[2], y[3], z[0], z[1], z[2], z[3], w[0], w[1], w[2], w[3]]
	}

	pub const fn from_array(values: [f32; 16]) -> Self {
		Self {
			columns: [
				[values[0], values[1], values[2], values[3]],
				[values[4], values[5], values[6], values[7]],
				[values[8], values[9], values[10], values[11]],
				[values[12], values[13], values[14], values[15]],
			],
		}
	}

	pub fn is_finite(&self) -> bool {
		all_finite(&self.to_array())
	}

	/// The translation an affine transform carries, which is its fourth column.
	pub const fn translation(&self) -> Vec3 {
		self.column(3).truncate()
	}

	/// The linear part: the upper-left 3x3, without the translation.
	pub fn linear(&self) -> Mat3 {
		Mat3::from_columns(self.column(0).truncate(), self.column(1).truncate(), self.column(2).truncate())
	}

	pub const fn from_translation(by: Vec3) -> Self {
		Self { columns: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [by.x, by.y, by.z, 1.0]] }
	}

	pub const fn from_scale(by: Vec3) -> Self {
		Self { columns: [[by.x, 0.0, 0.0, 0.0], [0.0, by.y, 0.0, 0.0], [0.0, 0.0, by.z, 0.0], [0.0, 0.0, 0.0, 1.0]] }
	}

	/// An affine transform from a linear part and a translation.
	pub fn from_linear(linear: &Mat3, translation: Vec3) -> Self {
		Self::from_columns(linear.column(0).extend(0.0), linear.column(1).extend(0.0), linear.column(2).extend(0.0), translation.extend(1.0))
	}

	pub fn mul_vector(&self, vector: Vec4) -> Vec4 {
		self.column(0).scale(vector.x).add(self.column(1).scale(vector.y)).add(self.column(2).scale(vector.z)).add(self.column(3).scale(vector.w))
	}

	/// A POINT, which carries `w = 1` and is therefore translated.
	pub fn transform_point(&self, point: Vec3) -> Vec4 {
		self.mul_vector(point.extend(1.0))
	}

	/// A DIRECTION, which carries `w = 0` and is therefore NOT translated. The distinction is the
	/// whole reason a 3D transform is four by four, and a normal transformed as a point is a normal
	/// that moves with the object's position.
	pub fn transform_direction(&self, direction: Vec3) -> Vec3 {
		self.mul_vector(direction.extend(0.0)).truncate()
	}

	/// `self * other`, which applies `other` FIRST. Composition reads right to left, like the
	/// transform does.
	pub fn mul(&self, other: &Self) -> Self {
		Self::from_columns(self.mul_vector(other.column(0)), self.mul_vector(other.column(1)), self.mul_vector(other.column(2)), self.mul_vector(other.column(3)))
	}

	pub fn transpose(&self) -> Self {
		let mut out = [[0.0f32; 4]; 4];
		let mut row = 0;
		while row < 4 {
			let mut column = 0;
			while column < 4 {
				out[row][column] = self.columns[column][row];
				column += 1;
			}
			row += 1;
		}
		Self { columns: out }
	}

	/// The determinant, by cofactor expansion. Computed in `f64` and answered in `f32`: the
	/// intermediate products of a projection matrix span several orders of magnitude, and a
	/// determinant that cancels to zero in `f32` while the matrix is invertible is a refusal nobody
	/// can act on.
	pub fn determinant(&self) -> f32 {
		determinant_f64(&self.as_f64()) as f32
	}

	/// The inverse.
	///
	/// A TYPED REFUSAL AND NEVER A MATRIX OF INFINITIES. A singular matrix has no inverse; dividing
	/// the adjugate by a determinant near zero produces entries around `1e30` that multiply into
	/// every later transform, and what the frame shows then is geometry that has vanished rather
	/// than an error anybody can find.
	pub fn inverse(&self) -> Result<Self, Error> {
		if !self.is_finite() {
			return Err(Error::NotFinite);
		}
		let wide = self.as_f64();
		let determinant = determinant_f64(&wide);
		if !(determinant.abs() > SINGULAR_EPSILON as f64) {
			return Err(Error::Singular);
		}
		let inverse = 1.0 / determinant;
		let mut out = [[0.0f32; 4]; 4];
		let mut row = 0;
		while row < 4 {
			let mut column = 0;
			while column < 4 {
				// THE ADJUGATE IS THE TRANSPOSE OF THE COFACTORS, which is where an index swap
				// silently transposes the answer - so the swap is written out rather than folded
				// into the loop bounds.
				let cofactor = cofactor_f64(&wide, column, row);
				out[column][row] = (cofactor * inverse) as f32;
				column += 1;
			}
			row += 1;
		}
		let result = Self { columns: out };
		if !result.is_finite() {
			// A determinant above the threshold can still leave an entry that overflows `f32`, and a
			// matrix with an infinity in it is the thing this function exists to not return.
			return Err(Error::Singular);
		}
		Ok(result)
	}

	fn as_f64(&self) -> [[f64; 4]; 4] {
		let mut out = [[0.0f64; 4]; 4];
		let mut column = 0;
		while column < 4 {
			let mut row = 0;
			while row < 4 {
				out[column][row] = self.columns[column][row] as f64;
				row += 1;
			}
			column += 1;
		}
		out
	}
}

/// The 3x3 determinant of the matrix with `row` and `column` struck out, with its sign.
fn cofactor_f64(matrix: &[[f64; 4]; 4], row: usize, column: usize) -> f64 {
	let mut minor = [[0.0f64; 3]; 3];
	let mut target_column = 0;
	let mut source_column = 0;
	while source_column < 4 {
		if source_column == column {
			source_column += 1;
			continue;
		}
		let mut target_row = 0;
		let mut source_row = 0;
		while source_row < 4 {
			if source_row == row {
				source_row += 1;
				continue;
			}
			minor[target_column][target_row] = matrix[source_column][source_row];
			target_row += 1;
			source_row += 1;
		}
		target_column += 1;
		source_column += 1;
	}
	let value = minor[0][0] * (minor[1][1] * minor[2][2] - minor[2][1] * minor[1][2]) - minor[1][0] * (minor[0][1] * minor[2][2] - minor[2][1] * minor[0][2]) + minor[2][0] * (minor[0][1] * minor[1][2] - minor[1][1] * minor[0][2]);
	if (row + column) % 2 == 0 { value } else { -value }
}

fn determinant_f64(matrix: &[[f64; 4]; 4]) -> f64 {
	let mut total = 0.0;
	let mut column = 0;
	while column < 4 {
		total += matrix[column][0] * cofactor_f64(matrix, 0, column);
		column += 1;
	}
	total
}
