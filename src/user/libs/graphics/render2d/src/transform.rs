//! THE TRANSFORM, and what a stroke width MEANS under one.
//!
//! THE QUESTION IS NOT DECORATIVE. Whether a stroke's width, its dash lengths and its miter limit are
//! measured in USER space before the transform or in DEVICE space after it decides what a scaled or
//! projectively transformed stroke looks like - and under a projective transform the two differ
//! enough that a user interface drawn with one and tested against the other is visibly wrong.
//!
//! PROJECTIVE AND NOT AFFINE, because the affine version is the one that has to be replaced later. A
//! perspective transform is what a card flip, a page turn and a map tilt are, and an API whose
//! transform cannot express one makes each of those a special case somewhere else.

use graphics_core::geom::{PointF, RectF};

/// A 2D projective transform, row-major.
///
/// THE LAST ROW IS CARRIED EVEN WHEN IT IS `0, 0, 1`, and `is_affine` is what a backend takes its
/// fast path on. Storing only six numbers and adding the projective case later means changing every
/// signature that passes one.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Transform {
	pub m: [[f32; 3]; 3],
}

impl Default for Transform {
	fn default() -> Self {
		Self::IDENTITY
	}
}

impl Transform {
	pub const IDENTITY: Self = Self { m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] };

	pub const fn translate(dx: f32, dy: f32) -> Self {
		Self { m: [[1.0, 0.0, dx], [0.0, 1.0, dy], [0.0, 0.0, 1.0]] }
	}

	pub const fn scale(sx: f32, sy: f32) -> Self {
		Self { m: [[sx, 0.0, 0.0], [0.0, sy, 0.0], [0.0, 0.0, 1.0]] }
	}

	/// Whether the last row is `0, 0, 1`. A backend's fast path, and the question a stroke's scaling
	/// rule changes meaning under.
	pub fn is_affine(&self) -> bool {
		self.m[2] == [0.0, 0.0, 1.0]
	}

	/// `self` then `other`, which is the order a canvas concatenates in: the new transform applies
	/// FIRST, inside the one already set.
	pub fn concat(&self, other: &Transform) -> Transform {
		let mut out = [[0.0f32; 3]; 3];
		for (row, slot) in out.iter_mut().enumerate() {
			for (column, value) in slot.iter_mut().enumerate() {
				*value = (0..3).map(|inner| self.m[row][inner] * other.m[inner][column]).sum();
			}
		}
		Transform { m: out }
	}

	/// Map a point, dividing by `w`.
	///
	/// A POINT AT OR BEYOND THE HORIZON HAS NO IMAGE, and the answer is `None` rather than a very
	/// large number: dividing by a `w` near zero produces a vertex at ten million pixels and a
	/// rasteriser that spends a second on one triangle.
	pub fn map_point(&self, point: PointF) -> Option<PointF> {
		let x = self.m[0][0] * point.x + self.m[0][1] * point.y + self.m[0][2];
		let y = self.m[1][0] * point.x + self.m[1][1] * point.y + self.m[1][2];
		let w = self.m[2][0] * point.x + self.m[2][1] * point.y + self.m[2][2];
		// AT OR BEYOND THE HORIZON THERE IS NO IMAGE, and a NaN `w` is beyond it: the comparison is
		// written out because every comparison with NaN is false, so a plain `w <= epsilon` would let
		// a NaN through and divide by it.
		let epsilon = graphics_profile::geometry::PROJECTIVE_W_EPSILON as f32;
		if !matches!(w.partial_cmp(&epsilon), Some(core::cmp::Ordering::Greater)) {
			return None;
		}
		// AN AFFINE TRANSFORM DIVIDES BY EXACTLY ONE, and that is the identity in IEEE 754 - so this
		// is the same two numbers and not an approximation of them. It is worth a branch because
		// almost every transform in a drawing is affine and this is called PER PIXEL by every
		// gradient and every image shader: two f32 divisions, which are a dozen cycles each and
		// cannot start until `w` is finished.
		//
		// THE TEST IS ON `w` AND NOT ON THE MATRIX. `is_affine` asks whether the last row is
		// `0, 0, 1`, which is sufficient and not necessary: a projective transform still yields
		// `w == 1` along a whole line of its domain, and those points are free too.
		//
		// AND IT MOVED NO BENCHMARK SCENE MEASURABLY (2026-09-16), which is recorded so nobody
		// measures it again expecting otherwise. The divides are real and are gone; they are simply
		// not what those four scenes are waiting on.
		if w == 1.0 {
			return Some(PointF { x, y });
		}
		Some(PointF { x: x / w, y: y / w })
	}

	/// The transform that undoes this one, or `None` when it undoes nothing.
	///
	/// AN APPLICATION THAT CANNOT MAP A POINT BACK HAS NO HIT TESTING. A click arrives in device
	/// pixels and the thing it hit was drawn in some nested user space; without an inverse every
	/// caller either keeps a parallel stack of transforms by hand or tests against device-space
	/// geometry it has to rebuild. It is the full 3x3 adjugate and not the affine shortcut, because a
	/// projective transform is exactly the case where the shortcut is wrong.
	pub fn inverse(&self) -> Option<Transform> {
		let m = &self.m;
		let cofactor = [
			[m[1][1] * m[2][2] - m[1][2] * m[2][1], m[0][2] * m[2][1] - m[0][1] * m[2][2], m[0][1] * m[1][2] - m[0][2] * m[1][1]],
			[m[1][2] * m[2][0] - m[1][0] * m[2][2], m[0][0] * m[2][2] - m[0][2] * m[2][0], m[0][2] * m[1][0] - m[0][0] * m[1][2]],
			[m[1][0] * m[2][1] - m[1][1] * m[2][0], m[0][1] * m[2][0] - m[0][0] * m[2][1], m[0][0] * m[1][1] - m[0][1] * m[1][0]],
		];
		let determinant = m[0][0] * cofactor[0][0] + m[0][1] * cofactor[1][0] + m[0][2] * cofactor[2][0];
		// A SINGULAR TRANSFORM HAS NO INVERSE AND SAYS SO. A matrix that collapses the plane onto a
		// line has many points mapping to each image, and returning any one of them would be an
		// answer the caller cannot tell from a correct one.
		if !determinant.is_finite() || determinant == 0.0 {
			return None;
		}
		let mut inverse = [[0.0f32; 3]; 3];
		for row in 0..3 {
			for column in 0..3 {
				inverse[row][column] = cofactor[row][column] / determinant;
				if !inverse[row][column].is_finite() {
					return None;
				}
			}
		}
		Some(Transform { m: inverse })
	}

	/// Map a point WITHOUT the perspective divide, which is what clipping against the horizon needs.
	///
	/// DIVIDING FIRST AND CLIPPING AFTER IS THE VERSION THAT PRODUCES A VERTEX AT TEN MILLION PIXELS.
	/// A segment that crosses `w == 0` has to be cut at the `w = epsilon` plane while it is still a
	/// straight line in homogeneous space; after the divide it is not one any more.
	pub fn map_homogeneous(&self, point: PointF) -> [f32; 3] {
		[
			self.m[0][0] * point.x + self.m[0][1] * point.y + self.m[0][2],
			self.m[1][0] * point.x + self.m[1][1] * point.y + self.m[1][2],
			self.m[2][0] * point.x + self.m[2][1] * point.y + self.m[2][2],
		]
	}

	/// The bounding box of a mapped rectangle, or `None` when any corner is beyond the horizon.
	///
	/// ALL FOUR CORNERS, because a rotated rectangle's bounds are not its mapped corners' pairwise
	/// extremes unless all four are taken - and a bounds computed from two of them is smaller than
	/// the drawing, which is how a damage rectangle comes to clip the thing it was computed for.
	pub fn map_rect(&self, rect: RectF) -> Option<RectF> {
		let corners = [PointF { x: rect.x, y: rect.y }, PointF { x: rect.right(), y: rect.y }, PointF { x: rect.x, y: rect.bottom() }, PointF { x: rect.right(), y: rect.bottom() }];
		let mut minimum = PointF { x: f32::INFINITY, y: f32::INFINITY };
		let mut maximum = PointF { x: f32::NEG_INFINITY, y: f32::NEG_INFINITY };
		for corner in corners {
			let mapped = self.map_point(corner)?;
			minimum.x = minimum.x.min(mapped.x);
			minimum.y = minimum.y.min(mapped.y);
			maximum.x = maximum.x.max(mapped.x);
			maximum.y = maximum.y.max(mapped.y);
		}
		Some(RectF::new(minimum.x, minimum.y, maximum.x - minimum.x, maximum.y - minimum.y))
	}

	/// How much this transform scales length, as the geometric mean of its axes.
	///
	/// ONE NUMBER FOR AN ANISOTROPIC TRANSFORM IS AN APPROXIMATION AND SAYS SO. It is what a stroke
	/// width scaled `WithTransform` is multiplied by, and a transform that scales x and y differently
	/// makes a circular pen elliptical - which is correct and is not what one number can express. A
	/// backend that strokes by converting to a fill uses the transform itself; this is for the
	/// bounds estimate and for the flattening tolerance.
	pub fn approximate_scale(&self) -> f32 {
		let determinant = self.m[0][0] * self.m[1][1] - self.m[0][1] * self.m[1][0];
		let scale = determinant.abs();
		if scale > 0.0 { sqrt_f32(scale) } else { 0.0 }
	}
}

/// ONE SQUARE ROOT IMPLEMENTATION FOR THE WHOLE STACK, AND THIS LAYER CARRIES ITS OWN COPY OF IT.
///
/// THIS WAS A SECOND ALGORITHM AND IT WAS THE SLOW KIND. Its own documentation said "two Newton
/// steps from a bit-level estimate" and the loop ran FOUR - four serially dependent divisions, a
/// dozen cycles each and impossible to pipeline behind one another. It is on the flattening path, so
/// every curve segment of every prepared list paid it. It is `libm`'s correctly rounded square root
/// now, which is what `graphics_core` calls too: one ALGORITHM, and the same answer bit for bit,
/// which is what the pixel-exact half of the conformance contract needs.
///
/// AND THE CALL CROSSES A LIBRARY BOUNDARY ON PURPOSE, WHICH TOOK TWO ATTEMPTS TO GET RIGHT.
/// `render2d` is a shared library and this is the only thing it reads from `graphics_core`, so
/// whether the boundary is crossed at all was left to the compiler: x86_64 and aarch64 inlined it
/// and riscv64 emitted a dynamic import, and the riscv64 image failed to link with "import ... has
/// no direct provider" while the other two were clean. Carrying `libm` here instead was tried and
/// is worse: `soft2d` declares both this library and `graphics_core`, so every libm symbol then had
/// two exporters and the build refused the ambiguity. The answer is on the other side -
/// `graphics_core::composite::sqrt_f32` is `#[inline(never)]`, so all three targets emit the import
/// and the provider row below is true on all three.
pub(crate) fn sqrt_f32(value: f32) -> f32 {
	graphics_core::composite::sqrt_f32(value)
}

/// WHERE A STROKE'S WIDTH IS MEASURED.
///
/// BOTH ARE WANTED AND THE DEFAULT IS STATED. A shape scaled up should usually get a thicker outline,
/// which is `WithTransform`; a hairline, a selection rectangle and a diagram's grid should stay one
/// pixel however far the view is zoomed, which is `NonScaling`. An API with only the first makes the
/// second a caller dividing by its own zoom, which is wrong under rotation and meaningless under
/// perspective.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StrokeScaling {
	/// The width, the dash lengths and the miter limit are in USER space and are transformed with the
	/// geometry. A scaled shape gets a scaled outline.
	#[default]
	WithTransform,
	/// They are in DEVICE space: the geometry is transformed and the pen is not. A one-pixel line
	/// stays one pixel at any zoom.
	NonScaling,
}
