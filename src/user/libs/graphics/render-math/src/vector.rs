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
///
/// PUBLIC SINCE `Scene3D Extended 1` NEEDED A SCALAR ONE. It was `pub(crate)` while every caller was
/// a vector length; the physically based material's Smith visibility term takes the square root of
/// two scalars, and the alternative to exporting this was a second Newton iteration in the scene
/// layer - which is the one thing a shared maths crate exists to prevent. A second implementation
/// would also be a second set of fixtures, and the two would drift at the last bit.
pub fn sqrt(value: f32) -> f32 {
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

/// The natural logarithm, without `std` and without `libm`.
///
/// THE SAME REASON AS `sqrt`: this crate is a layer that cannot take `libm` as a dependency without
/// putting it in every consumer, and `f32::ln` is behind `std`. It arrived when `Scene3D Extended 1`
/// needed a logarithmic cascade split and an exponential fog, neither of which is expressible with
/// square roots at an arbitrary cascade count.
///
/// THE DECOMPOSITION IS EXACT AND THE SERIES IS SHORT. `x = m * 2^e` with the mantissa taken
/// straight out of the bits, then `m` is folded into `[sqrt(2)/2, sqrt(2))` so the series argument
/// `s = (m - 1) / (m + 1)` has `|s| <= 0.1716`. Six odd terms of `2 * atanh(s)` then reach the last
/// representable `f32` - the next term is below `1e-9` of the result - which is what the fixtures
/// check against `f64::ln` across twelve orders of magnitude.
pub fn ln(value: f32) -> f32 {
	if value.is_nan() || value < 0.0 {
		return f32::NAN;
	}
	if value == 0.0 {
		return f32::NEG_INFINITY;
	}
	if value == f32::INFINITY {
		return value;
	}
	const LN_2: f32 = core::f32::consts::LN_2;
	let bits = value.to_bits();
	// A SUBNORMAL IS SCALED INTO THE NORMAL RANGE FIRST, because its stored exponent is zero and the
	// mantissa is not the number's - reading it as a normal would answer for a different value.
	if bits < 0x0080_0000 {
		return ln(value * 16_777_216.0) - 24.0 * LN_2;
	}
	let mut exponent = ((bits >> 23) as i32) - 127;
	let mut mantissa = f32::from_bits((bits & 0x007f_ffff) | 0x3f80_0000);
	// FOLDED ABOUT `sqrt(2)`, which halves the series argument and so quarters the term count.
	if mantissa > core::f32::consts::SQRT_2 {
		mantissa *= 0.5;
		exponent += 1;
	}
	let s = (mantissa - 1.0) / (mantissa + 1.0);
	let s2 = s * s;
	let mut term = s;
	let mut sum = s;
	let mut odd = 3.0f32;
	let mut round = 0;
	while round < 5 {
		term *= s2;
		sum += term / odd;
		odd += 2.0;
		round += 1;
	}
	2.0 * sum + exponent as f32 * LN_2
}

/// `e` to a power, without `std` and without `libm`.
///
/// `x = k * ln(2) + r` WITH `|r| <= ln(2) / 2`, so `exp(x) = 2^k * exp(r)` and the Taylor series for
/// `exp(r)` needs nine terms to fall below `1e-11` of the result. The `2^k` is built from the
/// exponent bits rather than multiplied up, so it is exact and costs nothing.
///
/// IT SATURATES RATHER THAN PRODUCING A DENORMAL OR AN INFINITY BY ACCIDENT: an argument past the
/// `f32` range answers `0` or `INFINITY` at the boundary the format has, which is what a fog factor
/// or a tone-map operator wants from a distance of a million.
pub fn exp(value: f32) -> f32 {
	if value.is_nan() {
		return f32::NAN;
	}
	if value > 88.72 {
		return f32::INFINITY;
	}
	if value < -103.0 {
		return 0.0;
	}
	const LN_2: f32 = core::f32::consts::LN_2;
	// ROUND TO NEAREST, so the remainder stays inside half a `ln(2)` and the series argument is
	// never larger than 0.347 - truncating instead would double it and cost three more terms.
	let k = (value / LN_2 + if value >= 0.0 { 0.5 } else { -0.5 }) as i32;
	let r = value - k as f32 * LN_2;
	let mut term = 1.0f32;
	let mut sum = 1.0f32;
	let mut index = 1u32;
	while index <= 9 {
		term *= r / index as f32;
		sum += term;
		index += 1;
	}
	// `2^k` FROM THE EXPONENT FIELD. A `k` outside the normal range is folded in two steps rather
	// than answered wrongly: the bounds above keep the total in range, and this keeps each step in
	// it too.
	let scale = |power: i32| -> f32 {
		let biased = power + 127;
		if biased <= 0 {
			0.0
		} else if biased >= 255 {
			f32::INFINITY
		} else {
			f32::from_bits((biased as u32) << 23)
		}
	};
	if k > 127 || k < -126 {
		let half = k / 2;
		return sum * scale(half) * scale(k - half);
	}
	sum * scale(k)
}

/// `x` to an arbitrary real power, for the one caller that needs it: a logarithmic cascade split at
/// a cascade count that is not a power of two.
///
/// `x^y = exp(y * ln(x))` AND NOTHING CLEVERER, with the two special cases that are not that: a
/// power of zero is one for every base, and a base of zero is zero for every positive power. Both
/// are the limits and both are what a caller means; computing them through the logarithm would give
/// a NaN and an infinity.
pub fn powf(base: f32, exponent: f32) -> f32 {
	if exponent == 0.0 {
		return 1.0;
	}
	if base == 0.0 {
		return if exponent > 0.0 { 0.0 } else { f32::INFINITY };
	}
	if base < 0.0 {
		return f32::NAN;
	}
	exp(exponent * ln(base))
}
