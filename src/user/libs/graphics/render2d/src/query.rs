//! WHAT MAY BE ASKED OF A PATH, beyond whether a point is inside it.
//!
//! A VECTOR EDITOR, A MAP, A CHART AND A DIAGRAM ALL ASK THESE. Where is the point a third of the way
//! along this curve; which way is it heading there; how long is it; does this click land on the
//! STROKE rather than the fill; how big is the stroke's bounding box. An application that cannot ask
//! computes them itself - from its own flattening, at its own tolerance, disagreeing with the one the
//! drawing used.
//!
//! EVERY ANSWER COMES THROUGH THE ONE FLATTENING, which is what makes them agree with the drawing.

use graphics_core::geom::{PointF, RectF};

use crate::flatten::flatten;
use crate::path::{Cap, FillRule, Path, StrokeStyle};
use crate::transform::{StrokeScaling, Transform, sqrt_f32};

/// The path's total length, in the space it is flattened in.
///
/// THE FLATTENED LENGTH AND NOT THE TRUE ARC LENGTH, and the difference is bounded by the tolerance
/// times the segment count - which is the same approximation the drawing used, and that is the
/// property that matters: a dash pattern laid out along a length the rasteriser disagrees with ends
/// in the wrong place.
pub fn length(path: &Path, transform: Option<&Transform>) -> f32 {
	let mut total = 0.0f32;
	for contour in flatten(path, transform) {
		let count = contour.points.len();
		let last = if contour.closed { count } else { count.saturating_sub(1) };
		for index in 0..last {
			let from = contour.points[index];
			let to = contour.points[(index + 1) % count];
			total += distance(from, to);
		}
	}
	total
}

/// The point and the unit tangent at a distance along the path.
///
/// BOTH AT ONCE, because every caller that wants one wants the other: placing a label along a curve
/// needs where AND which way, and computing them in two passes walks the path twice and can land on
/// two different segments at a segment boundary.
pub fn at_distance(path: &Path, distance_along: f32, transform: Option<&Transform>) -> Option<(PointF, PointF)> {
	let mut remaining = distance_along.max(0.0);
	let mut last: Option<(PointF, PointF)> = None;
	for contour in flatten(path, transform) {
		let count = contour.points.len();
		let end = if contour.closed { count } else { count.saturating_sub(1) };
		for index in 0..end {
			let from = contour.points[index];
			let to = contour.points[(index + 1) % count];
			let span = distance(from, to);
			if span <= 0.0 {
				continue;
			}
			let direction = PointF { x: (to.x - from.x) / span, y: (to.y - from.y) / span };
			if remaining <= span {
				let t = remaining / span;
				return Some((PointF { x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t }, direction));
			}
			remaining -= span;
			last = Some((to, direction));
		}
	}
	// PAST THE END IS THE END, with the last segment's direction. Answering `None` would make a dash
	// pattern or a label that ran one rounding unit past the path disappear instead of finishing at
	// its tip.
	last
}

/// Does a click land on the STROKE of a path?
///
/// NOT THE SAME QUESTION AS THE FILL, and a diagram makes that obvious: a line has no interior, so a
/// hit test that only asked about the fill would say nothing in a drawing is clickable.
///
/// THE WIDTH IS INTERPRETED THE WAY THE STYLE SAYS. Under `NonScaling` the pen is in device space, so
/// the test is against the transformed geometry at the stated width; under `WithTransform` the width
/// is scaled with everything else - and using the wrong one makes a zoomed-in diagram's lines
/// unclickable at exactly the zoom where they are easiest to see.
pub fn stroke_contains(path: &Path, style: &StrokeStyle, point: PointF, transform: Option<&Transform>) -> bool {
	let half = half_width(style, transform);
	if !greater(half, 0.0) {
		return false;
	}
	for contour in flatten(path, transform) {
		let count = contour.points.len();
		let end = if contour.closed { count } else { count.saturating_sub(1) };
		for index in 0..end {
			let from = contour.points[index];
			let to = contour.points[(index + 1) % count];
			let (t, perpendicular) = projection(point, from, to);
			// INSIDE THE SEGMENT'S OWN SPAN the pen covers a band of the half-width, whatever the cap.
			if (0.0..=1.0).contains(&t) {
				if perpendicular <= half {
					return true;
				}
				continue;
			}
			// PAST AN END. At an INTERIOR vertex the join covers it - approximated here as a round
			// join, which is the join that covers least, so this never claims a hit the drawing does
			// not make. At the contour's own ENDS the CAP decides, and butt against square differ by
			// exactly the half-width at both ends of every open path in a drawing.
			let at_start = !contour.closed && index == 0 && t < 0.0;
			let at_finish = !contour.closed && index + 1 == end && t > 1.0;
			let tip = if t < 0.0 { from } else { to };
			let reach = match (at_start || at_finish, style.cap) {
				(false, _) | (true, Cap::Round) => half,
				(true, Cap::Butt) => 0.0,
				// A SQUARE CAP EXTENDS THE LINE BY THE HALF-WIDTH, so the corner of the square is the
				// half-width diagonally out - which is the reach a click at the very corner needs.
				(true, Cap::Square) => half * core::f32::consts::SQRT_2,
			};
			if reach > 0.0 && distance(point, tip) <= reach {
				return true;
			}
		}
	}
	false
}

/// The bounding box of what a STROKE covers, which is not the path's own.
///
/// IT GROWS BY THE HALF-WIDTH AND BY THE MITER, and the miter is the part that is forgotten: a nearly
/// parallel join reaches out by the half-width times the miter limit, which on a default limit of
/// four is four times further than the naive answer - and a layer sized by the naive answer clips
/// exactly the sharp corners somebody drew on purpose.
pub fn stroke_bounds(path: &Path, style: &StrokeStyle, transform: Option<&Transform>) -> Option<RectF> {
	let mut bounds: Option<RectF> = None;
	for contour in flatten(path, transform) {
		for point in &contour.points {
			bounds = Some(match bounds {
				Some(rect) => grow(rect, *point),
				None => RectF::new(point.x, point.y, 0.0, 0.0),
			});
		}
	}
	let rect = bounds?;
	let half = half_width(style, transform);
	let reach = half * style.miter_limit.max(1.0);
	Some(RectF::new(rect.x - reach, rect.y - reach, rect.width + reach * 2.0, rect.height + reach * 2.0))
}

/// Does a click land on the FILL, under a rule? The same flattening the drawing used.
pub fn fill_contains(path: &Path, rule: FillRule, point: PointF, transform: Option<&Transform>) -> bool {
	let mut winding = 0i32;
	let mut crossings = 0u32;
	for contour in flatten(path, transform) {
		let count = contour.points.len();
		for index in 0..count {
			let from = contour.points[index];
			let to = contour.points[(index + 1) % count];
			if (from.y <= point.y) != (to.y <= point.y) {
				let t = (point.y - from.y) / (to.y - from.y);
				let x = from.x + t * (to.x - from.x);
				if x > point.x {
					crossings += 1;
					winding += if to.y > from.y { 1 } else { -1 };
				}
			}
		}
	}
	match rule {
		FillRule::NonZero => winding != 0,
		FillRule::EvenOdd => crossings % 2 == 1,
	}
}

fn half_width(style: &StrokeStyle, transform: Option<&Transform>) -> f32 {
	let scale = match (style.scaling, transform) {
		(StrokeScaling::NonScaling, _) | (_, None) => 1.0,
		(StrokeScaling::WithTransform, Some(transform)) => transform.approximate_scale(),
	};
	// The geometry is already in device space after flattening, so a `WithTransform` width has to be
	// scaled to match it - and a `NonScaling` one must NOT be, which is the whole distinction.
	style.width * 0.5 * if matches!(style.scaling, StrokeScaling::WithTransform) { scale } else { 1.0 }
}

fn distance(a: PointF, b: PointF) -> f32 {
	let (dx, dy) = (b.x - a.x, b.y - a.y);
	sqrt_f32(dx * dx + dy * dy)
}

/// Where a point projects onto a segment, UNCLAMPED, and how far it is from the infinite line.
///
/// UNCLAMPED IS THE POINT. Clamping first answers "how far from the capsule", which is the round-cap
/// answer for every cap - and the whole difference between a butt cap and a round one is what happens
/// outside `0..=1`.
fn projection(point: PointF, from: PointF, to: PointF) -> (f32, f32) {
	let (dx, dy) = (to.x - from.x, to.y - from.y);
	let length_squared = dx * dx + dy * dy;
	if length_squared <= 0.0 {
		return (0.0, distance(point, from));
	}
	let t = ((point.x - from.x) * dx + (point.y - from.y) * dy) / length_squared;
	let projected = PointF { x: from.x + dx * t, y: from.y + dy * t };
	(t, distance(point, projected))
}

fn grow(rect: RectF, point: PointF) -> RectF {
	let x = rect.x.min(point.x);
	let y = rect.y.min(point.y);
	let right = rect.right().max(point.x);
	let bottom = rect.bottom().max(point.y);
	RectF::new(x, y, right - x, bottom - y)
}

/// Whether one value is strictly greater than another, with a NaN answering NO.
///
/// WRITTEN OUT BECAUSE EVERY COMPARISON WITH NaN IS FALSE, so `!(a > b)` and `a <= b` are different
/// questions when either can be NaN - and in geometry either can. This is the one that treats a NaN
/// as "not greater", which is what every caller here wants.
fn greater(left: f32, right: f32) -> bool {
	matches!(left.partial_cmp(&right), Some(core::cmp::Ordering::Greater))
}
