//! Quaternions, with the normalisation policy STATED rather than assumed.
//!
//! THE POLICY, in three sentences:
//!
//!   * CONSTRUCTORS NORMALISE. `from_axis_angle` normalises its axis and answers a unit quaternion;
//!     `from_components` normalises what it is given. There is no way to obtain a non-unit
//!     quaternion from this module by accident.
//!   * OPERATIONS PRESERVE UNIT LENGTH. The product of two unit quaternions is a unit quaternion in
//!     exact arithmetic and drifts in `f32`, so `mul` renormalises - which costs one square root and
//!     removes the class of defect where a rotation accumulated over a thousand frames quietly
//!     becomes a scale.
//!   * A ZERO-LENGTH QUATERNION IS AN ERROR, not a NaN generator. Normalising it divides by zero and
//!     produces four NaNs that look like a rotation until something renders; `Error::ZeroLength` is
//!     the answer instead.
//!
//! RIGHT-HANDED, LIKE EVERYTHING ELSE HERE: a positive angle about `+Z` turns `+X` towards `+Y`.

use crate::matrix::{Mat3, Mat4};
use crate::vector::{Vec3, sqrt};
use crate::{Error, ZERO_LENGTH_EPSILON, all_finite};

/// A unit quaternion. Every value of this type that this module produces has unit length.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Quat {
	pub x: f32,
	pub y: f32,
	pub z: f32,
	pub w: f32,
}

impl Quat {
	pub const IDENTITY: Self = Self { x: 0.0, y: 0.0, z: 0.0, w: 1.0 };

	/// From four components, NORMALISED. Refuses a zero-length or non-finite input.
	pub fn from_components(x: f32, y: f32, z: f32, w: f32) -> Result<Self, Error> {
		Self { x, y, z, w }.normalise()
	}

	/// A rotation of `radians` about `axis`, right-handed.
	///
	/// THE AXIS IS NORMALISED HERE, so a caller passing a direction of any length gets the rotation
	/// it describes rather than one scaled by its length - which is a quaternion that is not a
	/// rotation at all.
	pub fn from_axis_angle(axis: Vec3, radians: f32) -> Result<Self, Error> {
		if !radians.is_finite() {
			return Err(Error::NotFinite);
		}
		let axis = axis.normalise()?;
		let half = radians * 0.5;
		let (sine, cosine) = sin_cos(half);
		Ok(Self { x: axis.x * sine, y: axis.y * sine, z: axis.z * sine, w: cosine })
	}

	pub fn is_finite(self) -> bool {
		all_finite(&[self.x, self.y, self.z, self.w])
	}

	pub fn length_squared(self) -> f32 {
		self.x * self.x + self.y * self.y + self.z * self.z + self.w * self.w
	}

	pub fn length(self) -> f32 {
		sqrt(self.length_squared())
	}

	pub fn normalise(self) -> Result<Self, Error> {
		if !self.is_finite() {
			return Err(Error::NotFinite);
		}
		let squared = self.length_squared();
		if squared <= ZERO_LENGTH_EPSILON {
			return Err(Error::ZeroLength);
		}
		let inverse = 1.0 / sqrt(squared);
		Ok(Self { x: self.x * inverse, y: self.y * inverse, z: self.z * inverse, w: self.w * inverse })
	}

	/// The inverse rotation. For a unit quaternion that is the conjugate, and every quaternion this
	/// module produces is a unit one.
	pub const fn conjugate(self) -> Self {
		Self { x: -self.x, y: -self.y, z: -self.z, w: self.w }
	}

	/// `self * other` applies `other` FIRST, the way matrix composition does here.
	///
	/// RENORMALISED, for the reason at the top of this file.
	pub fn mul(self, other: Self) -> Self {
		let product = Self { x: self.w * other.x + self.x * other.w + self.y * other.z - self.z * other.y, y: self.w * other.y - self.x * other.z + self.y * other.w + self.z * other.x, z: self.w * other.z + self.x * other.y - self.y * other.x + self.z * other.w, w: self.w * other.w - self.x * other.x - self.y * other.y - self.z * other.z };
		// A product of two unit quaternions cannot be zero-length, so this cannot refuse - and
		// answering the unrenormalised product rather than propagating an impossible error keeps this
		// operation infallible, which is what every caller composing rotations needs it to be.
		product.normalise().unwrap_or(product)
	}

	/// Rotate a vector. `q * v * q^-1`, written out.
	pub fn rotate(self, vector: Vec3) -> Vec3 {
		// The standard expansion, which is two cross products rather than two quaternion products.
		let axis = Vec3::new(self.x, self.y, self.z);
		let first = axis.cross(vector).add(vector.scale(self.w));
		vector.add(axis.cross(first).scale(2.0))
	}

	/// The rotation as a 3x3 matrix, in this crate's column-major storage.
	pub fn to_mat3(self) -> Mat3 {
		let (x, y, z, w) = (self.x, self.y, self.z, self.w);
		let (x2, y2, z2) = (x + x, y + y, z + z);
		let (xx, xy, xz) = (x * x2, x * y2, x * z2);
		let (yy, yz, zz) = (y * y2, y * z2, z * z2);
		let (wx, wy, wz) = (w * x2, w * y2, w * z2);
		Mat3::from_columns(Vec3::new(1.0 - (yy + zz), xy + wz, xz - wy), Vec3::new(xy - wz, 1.0 - (xx + zz), yz + wx), Vec3::new(xz + wy, yz - wx, 1.0 - (xx + yy)))
	}

	pub fn to_mat4(self) -> Mat4 {
		Mat4::from_linear(&self.to_mat3(), Vec3::ZERO)
	}

	/// Spherical linear interpolation, along the SHORTER arc.
	///
	/// TWO QUATERNIONS DESCRIBE EVERY ROTATION - `q` and `-q` - and interpolating towards the wrong
	/// one takes the long way round: a turn of a few degrees becomes a turn of nearly a full circle.
	/// The sign is chosen here rather than left to the caller.
	pub fn slerp(self, other: Self, at: f32) -> Self {
		let mut dot = self.x * other.x + self.y * other.y + self.z * other.z + self.w * other.w;
		let mut end = other;
		if dot < 0.0 {
			end = Self { x: -other.x, y: -other.y, z: -other.z, w: -other.w };
			dot = -dot;
		}
		// NEARLY PARALLEL IS LINEAR, because the angle's sine is then near zero and dividing by it
		// amplifies the error rather than the interpolation.
		if dot > 0.9995 {
			let blended = Self { x: self.x + (end.x - self.x) * at, y: self.y + (end.y - self.y) * at, z: self.z + (end.z - self.z) * at, w: self.w + (end.w - self.w) * at };
			return blended.normalise().unwrap_or(self);
		}
		let angle = acos(dot.clamp(-1.0, 1.0));
		let sine = sin_cos(angle).0;
		if sine.abs() <= ZERO_LENGTH_EPSILON {
			return self;
		}
		let (from, to) = (sin_cos(angle * (1.0 - at)).0 / sine, sin_cos(angle * at).0 / sine);
		let blended = Self { x: self.x * from + end.x * to, y: self.y * from + end.y * to, z: self.z * from + end.z * to, w: self.w * from + end.w * to };
		blended.normalise().unwrap_or(self)
	}
}

/// Sine and cosine together, because every caller here wants both and they share the reduction.
///
/// `no_std` AGAIN: the Taylor series after a range reduction to `[-pi/4, pi/4]`, which is accurate to
/// the last representable `f32` over the range a rotation uses. The fixtures hold it against `f64`'s
/// own sine and cosine at a spread of angles including the quadrant boundaries.
pub fn sin_cos(radians: f32) -> (f32, f32) {
	if !radians.is_finite() {
		return (f32::NAN, f32::NAN);
	}
	const PI_OVER_2: f64 = core::f64::consts::FRAC_PI_2;
	let wide = radians as f64;
	// Which quadrant, and how far into it. `round` rather than `floor`, so the remainder is in
	// `[-pi/4, pi/4]` where the series converges fastest.
	let quadrant = round_to_nearest(wide / PI_OVER_2);
	let remainder = wide - quadrant * PI_OVER_2;
	let (sine, cosine) = (series_sin(remainder), series_cos(remainder));
	let index = ((quadrant as i64) % 4 + 4) % 4;
	let (sine, cosine) = match index {
		0 => (sine, cosine),
		1 => (cosine, -sine),
		2 => (-sine, -cosine),
		_ => (-cosine, sine),
	};
	(sine as f32, cosine as f32)
}

/// Round half away from zero, without `std`.
///
/// `f64::round` LIVES IN `std` and this crate is `no_std`. The values rounded here are quadrant
/// counts, so an `i64` holds every one a rotation can produce, and a float-to-integer cast in Rust
/// SATURATES rather than wrapping - which is the behaviour an absurd angle should get.
fn round_to_nearest(value: f64) -> f64 {
	if !value.is_finite() {
		return value;
	}
	if value >= 0.0 { (value + 0.5) as i64 as f64 } else { (value - 0.5) as i64 as f64 }
}

fn series_sin(value: f64) -> f64 {
	let square = value * value;
	value * (1.0 - square / 6.0 * (1.0 - square / 20.0 * (1.0 - square / 42.0 * (1.0 - square / 72.0))))
}

fn series_cos(value: f64) -> f64 {
	let square = value * value;
	1.0 - square / 2.0 * (1.0 - square / 12.0 * (1.0 - square / 30.0 * (1.0 - square / 56.0)))
}

/// Arc cosine, by Newton on `cos`. Used by `slerp` alone, over `[-1, 1]`.
fn acos(value: f32) -> f32 {
	let target = value as f64;
	// A monotone starting point that is exact at the ends and within a few per cent in between.
	let mut angle = core::f64::consts::FRAC_PI_2 - target * (1.0 + 0.195 * target * target);
	let mut round = 0;
	while round < 12 {
		let (sine, cosine) = (series_sin_full(angle), series_cos_full(angle));
		if sine.abs() < 1e-12 {
			break;
		}
		angle += (cosine - target) / sine;
		round += 1;
	}
	angle as f32
}

fn series_sin_full(value: f64) -> f64 {
	let quadrant = round_to_nearest(value / core::f64::consts::FRAC_PI_2);
	let remainder = value - quadrant * core::f64::consts::FRAC_PI_2;
	let (sine, cosine) = (series_sin(remainder), series_cos(remainder));
	match ((quadrant as i64) % 4 + 4) % 4 {
		0 => sine,
		1 => cosine,
		2 => -sine,
		_ => -cosine,
	}
}

fn series_cos_full(value: f64) -> f64 {
	let quadrant = round_to_nearest(value / core::f64::consts::FRAC_PI_2);
	let remainder = value - quadrant * core::f64::consts::FRAC_PI_2;
	let (sine, cosine) = (series_sin(remainder), series_cos(remainder));
	match ((quadrant as i64) % 4 + 4) % 4 {
		0 => cosine,
		1 => -sine,
		2 => -cosine,
		_ => sine,
	}
}
