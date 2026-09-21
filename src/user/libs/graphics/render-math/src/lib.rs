//! THE 3D MATH CONTRACT, WITH THE VALUES PICKED RATHER THAN DEMANDED.
//!
//! "State the handedness" is not a specification; a handedness is. Every convention below is chosen
//! here, once, and every type and constructor in this crate is written against it:
//!
//! ```text
//!   angles            RADIANS, everywhere, in and out
//!   matrix storage    COLUMN-MAJOR: a `Mat4` is four columns, and `m.column(3)` is the translation
//!   transform         `M * v`, a column vector on the right
//!   world and view    RIGHT-HANDED, `+Y` up, the camera looking along `-Z`
//!   clip depth        `[0, 1]` - zero at the near plane, one at the far plane
//!   front faces       COUNTER-CLOCKWISE, evaluated in NDC BEFORE the viewport's Y inversion
//!   NDC `+1` in Y     the TOP of the image
//!   row origin        `TopLeft`, which is what `graphics-core` stores and what a surface is
//!   viewport Y        `y_window = (1 - (y_ndc * 0.5 + 0.5)) * height`
//! ```
//!
//! FOUR OF THOSE ONLY BECOME UNAMBIGUOUS TOGETHER, and leaving the last one out inverts what gets
//! culled. `+Y` up, NDC `+1` at the top, a top-left row origin and a Y-inverting viewport are one
//! decision in four sentences: a Y flip reverses apparent winding, so a renderer that culled AFTER
//! it would cull the opposite faces while satisfying every other sentence here. `winding::facing`
//! therefore takes NDC positions and is documented as running before `camera::window_from_ndc`.
//!
//! CONSTRUCTORS ARE NAMED FOR WHAT THEY ENCODE - `perspective_rh_zo`, `orthographic_rh_zo`,
//! `look_at_rh` - so a caller cannot pick the wrong convention by passing the wrong arguments to a
//! generically named function. There is no `perspective`.
//!
//! WHAT HAPPENS TO NaN AND INFINITY, stated once and obeyed by every entry point:
//!
//!   * A CONSTRUCTOR OR A FALLIBLE OPERATION REFUSES. Anything returning `Result` checks its inputs
//!     are finite first and answers `Error::NotFinite` - so a camera built from a NaN field of view,
//!     a rotation about an infinite axis and the inverse of a matrix holding a NaN are refusals
//!     rather than matrices full of NaN that poison every later frame silently.
//!   * ORDINARY ARITHMETIC PROPAGATES, because that is what IEEE 754 does and pretending otherwise
//!     would mean a branch on every add. `Vec3::add` of a NaN is a NaN, and the refusal happens at
//!     the next boundary that has one.
//!   * `is_finite` IS PUBLIC ON EVERY TYPE, so a caller at a boundary this crate does not own can
//!     apply the same rule.
//!
//! NO `unsafe`, NO ALLOCATION, NO PANIC PATH. Every index is a compile-time constant or checked,
//! every division that could be by zero is a `Result`, and the crate is `no_std`.

#![cfg_attr(not(test), no_std)]

pub mod camera;
pub mod matrix;
pub mod quaternion;
pub mod vector;
pub mod winding;

pub use camera::{Viewport, look_at_rh, orthographic_rh_zo, perspective_infinite_rh_zo, perspective_rh_zo, window_from_ndc};
pub use matrix::{Mat3, Mat4};
pub use quaternion::Quat;
pub use vector::{Vec2, Vec3, Vec4, exp, ln, powf, sqrt};
pub use winding::{Facing, facing};

/// What this crate refuses, and why each one is a refusal rather than a value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
	/// A direction, an axis or a quaternion with no length. There is no unit vector to answer with
	/// and normalising it would be a division by zero, which produces a NaN that then spreads.
	ZeroLength,
	/// A matrix with no inverse. The alternative is a matrix of infinities, which multiplies into
	/// every later transform and shows up as geometry that has vanished rather than as an error.
	Singular,
	/// A NaN or an infinity where a finite number was required. Refused at the boundary rather than
	/// propagated, because the frame it poisons is not the one that would be blamed.
	NotFinite,
	/// A near or far plane that cannot describe a depth range: equal planes, a reversed pair, or a
	/// near plane at zero in a perspective projection, where the transform divides by it.
	DegenerateFrustum,
	/// A viewport with no area. Mapping a normalised coordinate into it would divide by zero.
	EmptyViewport,
}

/// How close to zero a determinant may be before a matrix is called singular.
///
/// A THRESHOLD AND NOT AN EQUALITY TEST, because a determinant computed in `f32` from a matrix that
/// IS singular almost never comes out as exactly zero - and an inverse computed from a determinant of
/// `1e-30` is a matrix of numbers around `1e30`, which is an infinity with extra steps.
pub const SINGULAR_EPSILON: f32 = 1e-6;

/// How close to zero a vector's squared length may be before it has no direction.
///
/// SQUARED, because that is what the caller has computed by then and taking a square root to compare
/// against zero adds an operation and an error term to answer a question neither changes.
pub const ZERO_LENGTH_EPSILON: f32 = 1e-12;

/// Every value of a slice is finite. The one place that test is written.
pub(crate) fn all_finite(values: &[f32]) -> bool {
	let mut index = 0;
	while index < values.len() {
		if !values[index].is_finite() {
			return false;
		}
		index += 1;
	}
	true
}

#[cfg(test)]
mod tests;
