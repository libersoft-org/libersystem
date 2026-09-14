//! Which way a triangle faces, decided BEFORE the viewport inverts Y.
//!
//! COUNTER-CLOCKWISE IS THE FRONT, in NDC, with `+Y` up. That sentence is only unambiguous because
//! this crate also fixes that NDC `+1` is the top of the image and that the viewport inverts Y on
//! the way to window coordinates - and the inversion reverses apparent winding, so a renderer
//! evaluating this AFTER it culls exactly the wrong faces while satisfying every other rule here.
//!
//! THE ORDER IS THE POINT, so this function takes NDC positions and says so in its type, and
//! `camera::window_from_ndc` is documented as coming after it.

use crate::vector::Vec3;

/// Which side of a triangle is towards the viewer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Facing {
	/// Counter-clockwise in NDC: the front.
	Front,
	/// Clockwise in NDC: the back.
	Back,
	/// Zero signed area - a degenerate triangle, or one seen exactly edge on. Neither front nor
	/// back, and a renderer discards it rather than picking a side: a triangle with no area
	/// contributes no coverage, so the side it would have faced changes nothing it could draw.
	Degenerate,
}

/// Twice the signed area of the triangle in NDC. Positive is counter-clockwise.
///
/// TWICE, because the factor of a half is the same on both sides of every comparison this is used
/// in, and halving costs an operation to answer the same question.
pub fn signed_area_doubled(a: Vec3, b: Vec3, c: Vec3) -> f32 {
	(b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y)
}

/// The facing of a triangle given its three vertices in NDC, in the order the index buffer gives
/// them.
///
/// A NON-FINITE VERTEX IS `Degenerate` rather than a refusal: this is per-triangle and on the hot
/// path, a renderer has already clipped against the near plane by here, and the answer a caller
/// needs for a triangle it cannot compute an area for is the one that discards it.
pub fn facing(a: Vec3, b: Vec3, c: Vec3) -> Facing {
	let area = signed_area_doubled(a, b, c);
	if !area.is_finite() || area == 0.0 {
		return Facing::Degenerate;
	}
	if area > 0.0 { Facing::Front } else { Facing::Back }
}
