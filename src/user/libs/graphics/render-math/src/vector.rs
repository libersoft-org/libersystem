//! Vectors, in the one convention this crate freezes.
//!
//! PLAIN ARITHMETIC PROPAGATES NaN, which is the crate-level rule: an add is an add. What refuses is
//! the operation that would otherwise DIVIDE BY ZERO - normalisation - and it refuses by name rather
//! than answering a vector of NaN that looks like a direction until something renders.

use crate::{Error, ZERO_LENGTH_EPSILON, all_finite};

macro_rules! vector {
	($name:ident, $count:literal, $($field:ident),+) => {
		#[derive(Clone, Copy, PartialEq, Debug, Default)]
		pub struct $name {
			$(pub $field: f32),+
		}

		impl $name {
			pub const ZERO: Self = Self { $($field: 0.0),+ };

			pub const fn new($($field: f32),+) -> Self {
				Self { $($field),+ }
			}

			/// The components in the order they are declared, which is the order they are stored in
			/// and the order a column of a matrix is read in.
			pub const fn to_array(self) -> [f32; $count] {
				[$(self.$field),+]
			}

			pub const fn from_array(values: [f32; $count]) -> Self {
				let mut index = 0;
				$(
					let $field = values[index];
					index += 1;
				)+
				let _ = index;
				Self { $($field),+ }
			}

			pub fn is_finite(self) -> bool {
				all_finite(&self.to_array())
			}

			pub fn add(self, other: Self) -> Self {
				Self { $($field: self.$field + other.$field),+ }
			}

			pub fn sub(self, other: Self) -> Self {
				Self { $($field: self.$field - other.$field),+ }
			}

			/// Component-wise, which is what a scale is. A dot product is `dot`.
			pub fn mul(self, other: Self) -> Self {
				Self { $($field: self.$field * other.$field),+ }
			}

			pub fn scale(self, factor: f32) -> Self {
				Self { $($field: self.$field * factor),+ }
			}

			pub fn negate(self) -> Self {
				Self { $($field: -self.$field),+ }
			}

			pub fn dot(self, other: Self) -> f32 {
				let mut sum = 0.0;
				$(sum += self.$field * other.$field;)+
				sum
			}

			/// The squared length, which is what every comparison against a length actually wants:
			/// two lengths compare the same way their squares do, and the square root is an
			/// operation and an error term that changes no answer.
			pub fn length_squared(self) -> f32 {
				self.dot(self)
			}

			pub fn length(self) -> f32 {
				sqrt(self.length_squared())
			}

			/// The unit vector in this direction.
			///
			/// REFUSES RATHER THAN DIVIDING BY ZERO. A vector shorter than `ZERO_LENGTH_EPSILON`
			/// squared has no direction to answer with, and a non-finite one has no length.
			pub fn normalise(self) -> Result<Self, Error> {
				if !self.is_finite() {
					return Err(Error::NotFinite);
				}
				let squared = self.length_squared();
				if squared <= ZERO_LENGTH_EPSILON {
					return Err(Error::ZeroLength);
				}
				let inverse = 1.0 / sqrt(squared);
				Ok(self.scale(inverse))
			}

			/// Linear interpolation. `at` outside `[0, 1]` extrapolates, which is the useful
			/// behaviour and is stated rather than clamped silently.
			pub fn lerp(self, other: Self, at: f32) -> Self {
				self.add(other.sub(self).scale(at))
			}
		}
	};
}

vector!(Vec2, 2, x, y);
vector!(Vec3, 3, x, y, z);
vector!(Vec4, 4, x, y, z, w);

impl Vec3 {
	/// The cross product, RIGHT-HANDED: `x cross y` is `+z`. That is the same handedness the world
	/// and view spaces are in, and it is what makes `look_at_rh`'s basis come out right-handed.
	pub fn cross(self, other: Self) -> Self {
		Self { x: self.y * other.z - self.z * other.y, y: self.z * other.x - self.x * other.z, z: self.x * other.y - self.y * other.x }
	}

	pub const fn extend(self, w: f32) -> Vec4 {
		Vec4 { x: self.x, y: self.y, z: self.z, w }
	}

	pub const fn truncate(self) -> Vec2 {
		Vec2 { x: self.x, y: self.y }
	}
}

impl Vec4 {
	pub const fn truncate(self) -> Vec3 {
		Vec3 { x: self.x, y: self.y, z: self.z }
	}

	/// The perspective divide: clip space to normalised device coordinates.
	///
	/// REFUSES A `w` OF ZERO, which is a point on the camera plane and has no projection. A renderer
	/// clips against `w = epsilon` before it divides; this is the boundary that says so when one has
	/// not.
	pub fn perspective_divide(self) -> Result<Vec3, Error> {
		if !self.is_finite() {
			return Err(Error::NotFinite);
		}
		if self.w.abs() <= ZERO_LENGTH_EPSILON {
			return Err(Error::ZeroLength);
		}
		let inverse = 1.0 / self.w;
		Ok(Vec3 { x: self.x * inverse, y: self.y * inverse, z: self.z * inverse })
	}
}

impl Vec2 {
	pub const fn extend(self, z: f32) -> Vec3 {
		Vec3 { x: self.x, y: self.y, z }
	}
}

/// A square root without a maths library, by Newton's method on the reciprocal-free form.
///
/// `no_std` HAS `f32::sqrt` ONLY BEHIND `std`, and this crate is one of the layers that cannot take
/// `libm` as a dependency without putting it in every consumer. Six iterations from a bit-trick seed
/// reach the last representable `f32` for every finite non-negative input this crate produces, which
/// the fixtures check against `f64::sqrt` over a spread of magnitudes.
pub(crate) fn sqrt(value: f32) -> f32 {
	if value.is_nan() || value < 0.0 {
		return f32::NAN;
	}
	if value == 0.0 || value == f32::INFINITY {
		return value;
	}
	// The classic exponent-halving seed: shifting the biased exponent right halves it, and the added
	// constant re-biases it. It lands within a few per cent, and Newton doubles the correct digits.
	let bits = value.to_bits();
	let mut guess = f32::from_bits((bits >> 1) + 0x1fc0_0000);
	let mut round = 0;
	while round < 6 {
		guess = 0.5 * (guess + value / guess);
		round += 1;
	}
	guess
}
