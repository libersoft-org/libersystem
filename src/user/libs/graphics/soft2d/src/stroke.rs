//! STROKING BY CONVERSION TO A FILL, and never by a second rasterisation path.
//!
//! A SECOND PATH WOULD BE A SECOND SET OF RULES ABOUT WHERE AN EDGE IS. A stroke drawn by its own
//! rasteriser and a fill drawn by the coverage rasteriser disagree at exactly the places a drawing
//! puts them next to each other - a filled shape with its own outline is the commonest drawing there
//! is, and the disagreement shows as a seam of background between the two.
//!
//! THE OUTLINE IS BUILT IN DEVICE SPACE from the already-flattened contours, so a stroke under a
//! projective transform is the outline of the transformed CURVE rather than the transform of a
//! straight-line outline - which is the version that makes a stroked circle's far side too thin.
//!
//! AND THE RESULT IS FILLED NON-ZERO. The outline of a self-overlapping stroke has its overlaps
//! wound the same way, so non-zero fills them once; even-odd would punch a hole at every place the
//! pen crossed its own path, which is what a rounded join at a sharp corner looks like.

use alloc::vec::Vec;

use graphics_core::geom::PointF;
use render2d::flatten::Contour;
use render2d::path::{Cap, Join, StrokeStyle};

/// Everything the outline builder needs that is not the geometry.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct StrokeParameters {
	/// The stroke width IN DEVICE PIXELS, already scaled by the stroke's own scaling rule.
	pub width: f32,
	pub cap: Cap,
	pub join: Join,
	pub miter_limit: f32,
}

impl StrokeParameters {
	pub fn from_style(style: &StrokeStyle, device_scale: f32) -> Self {
		let width = match style.scaling {
			// THE WIDTH IS IN USER SPACE AND IS SCALED WITH THE DRAWING, which is what a shape scaled
			// up should do with its outline.
			render2d::transform::StrokeScaling::WithTransform => style.width * device_scale,
			// A HAIRLINE STAYS ONE PIXEL. A diagram's grid, a selection rectangle and a chart's axis
			// are all drawn at a width the zoom must not change.
			render2d::transform::StrokeScaling::NonScaling => style.width,
		};
		Self { width, cap: style.cap, join: style.join, miter_limit: style.miter_limit }
	}
}

/// How many segments a round join or cap becomes.
///
/// THE SAME TOLERANCE AS EVERY OTHER CURVE. A round cap flattened more coarsely than the path it
/// caps is visibly a polygon at exactly the stroke widths where a cap is large enough to see.
fn arc_steps(radius: f32) -> u32 {
	let tolerance = render2d::flatten::tolerance().max(1.0 / 1024.0);
	if !(radius.is_finite() && radius > tolerance) {
		return 2;
	}
	// The chord error of an arc of `n` segments over half a turn is about `r * (pi/n)^2 / 8`.
	let steps = sqrt_f32(radius * 1.234 / tolerance) as u32;
	steps.clamp(4, 256)
}

/// Convert a flattened contour set into the outline to fill.
pub fn outline(contours: &[Contour], parameters: StrokeParameters) -> Vec<Contour> {
	let half = parameters.width * 0.5;
	let mut out: Vec<Contour> = Vec::new();
	if !(half.is_finite() && half > 0.0) {
		return out;
	}
	for contour in contours {
		let points = dedupe(&contour.points);
		if points.len() < 2 {
			// A DEGENERATE SUBPATH IS A DOT UNDER A ROUND OR SQUARE CAP AND NOTHING UNDER A BUTT ONE,
			// which is the rule every drawing API that has caps uses - a zero-length segment has no
			// direction, so a butt cap has nothing to be perpendicular to.
			if let Some(point) = points.first() {
				match parameters.cap {
					Cap::Round => out.push(circle(*point, half)),
					Cap::Square => out.push(square(*point, half)),
					Cap::Butt => {}
				}
			}
			continue;
		}
		if contour.closed {
			// A CLOSED CONTOUR HAS TWO OUTLINES AND NO CAPS: the outer side and the inner side, wound
			// oppositely so a non-zero fill leaves the middle empty.
			out.push(Contour { points: offset_side(&points, half, parameters, true), closed: true });
			let mut reversed = points.clone();
			reversed.reverse();
			out.push(Contour { points: offset_side(&reversed, half, parameters, true), closed: true });
		} else {
			// AN OPEN CONTOUR IS ONE LOOP: down one side, round the cap, back the other side, round
			// the other cap. Building it as two separate outlines would leave the caps as their own
			// shapes, and a non-zero fill of two abutting shapes is not the same as one of their
			// union when the stroke doubles back on itself.
			let mut loop_points = offset_side(&points, half, parameters, false);
			let last = points[points.len() - 1];
			let before_last = points[points.len() - 2];
			cap_points(&mut loop_points, before_last, last, half, parameters.cap);
			let mut reversed = points.clone();
			reversed.reverse();
			let back = offset_side(&reversed, half, parameters, false);
			loop_points.extend_from_slice(&back);
			cap_points(&mut loop_points, points[1], points[0], half, parameters.cap);
			out.push(Contour { points: loop_points, closed: true });
		}
	}
	out
}

/// Walk one side of a polyline at a distance, inserting a join at every corner.
fn offset_side(points: &[PointF], half: f32, parameters: StrokeParameters, closed: bool) -> Vec<PointF> {
	let mut out: Vec<PointF> = Vec::new();
	let count = points.len();
	let segments = if closed { count } else { count - 1 };
	for index in 0..segments {
		let from = points[index];
		let to = points[(index + 1) % count];
		let Some(normal) = normal_of(from, to, half) else { continue };
		out.push(PointF { x: from.x + normal.x, y: from.y + normal.y });
		out.push(PointF { x: to.x + normal.x, y: to.y + normal.y });
		// THE JOIN IS BETWEEN THIS SEGMENT'S END AND THE NEXT ONE'S START, on the outer side.
		let next_index = (index + 2) % count;
		let has_next = closed || index + 2 < count;
		if !has_next {
			continue;
		}
		let next = points[next_index];
		let Some(next_normal) = normal_of(to, next, half) else { continue };
		join_points(&mut out, to, normal, next_normal, half, parameters);
	}
	out
}

/// The outward normal of a segment, at the stroke's half width.
fn normal_of(from: PointF, to: PointF, half: f32) -> Option<PointF> {
	let (dx, dy) = (to.x - from.x, to.y - from.y);
	let length = sqrt_f32(dx * dx + dy * dy);
	if !(length.is_finite() && length > 0.0) {
		return None;
	}
	Some(PointF { x: dy / length * half, y: -dx / length * half })
}

/// Add the join between two segment normals at a corner.
fn join_points(out: &mut Vec<PointF>, corner: PointF, from: PointF, to: PointF, half: f32, parameters: StrokeParameters) {
	// THE TURN'S SIGN DECIDES WHICH SIDE IS OUTER. On the inner side the two offsets cross, and the
	// crossing is left for the non-zero fill to resolve rather than trimmed here: trimming it needs
	// the intersection of two offsets, which is a second geometry problem with its own degenerate
	// cases.
	let cross = from.x * to.y - from.y * to.x;
	if cross <= 0.0 {
		return;
	}
	match parameters.join {
		Join::Bevel => {}
		Join::Round => {
			let steps = arc_steps(half);
			let start = angle_of(from);
			let end = angle_of(to);
			let mut sweep = end - start;
			while sweep <= -core::f32::consts::PI {
				sweep += core::f32::consts::TAU;
			}
			while sweep > core::f32::consts::PI {
				sweep -= core::f32::consts::TAU;
			}
			for step in 1..steps {
				let angle = start + sweep * (step as f32 / steps as f32);
				out.push(PointF { x: corner.x + cos_f32(angle) * half, y: corner.y + sin_f32(angle) * half });
			}
		}
		Join::Miter => {
			// PAST THE LIMIT A MITER BECOMES A BEVEL. Without it a nearly parallel join produces a
			// spike hundreds of pixels long, which is the artefact that looks like a corrupt path.
			let middle = PointF { x: from.x + to.x, y: from.y + to.y };
			let length = sqrt_f32(middle.x * middle.x + middle.y * middle.y);
			if !(length.is_finite() && length > 0.0) {
				return;
			}
			let cosine = length / (2.0 * half);
			if cosine <= 0.0 {
				return;
			}
			let reach = half / cosine;
			if reach > half * parameters.miter_limit.max(1.0) {
				return;
			}
			out.push(PointF { x: corner.x + middle.x / length * reach, y: corner.y + middle.y / length * reach });
		}
	}
}

/// Add the cap that closes one end of an open stroke.
fn cap_points(out: &mut Vec<PointF>, before: PointF, end: PointF, half: f32, cap: Cap) {
	let Some(normal) = normal_of(before, end, half) else { return };
	let direction = PointF { x: -normal.y, y: normal.x };
	match cap {
		// A BUTT CAP IS THE STRAIGHT LINE ACROSS, which the outline already closes by returning along
		// the other side - so it adds nothing.
		Cap::Butt => {}
		Cap::Square => {
			out.push(PointF { x: end.x + normal.x + direction.x, y: end.y + normal.y + direction.y });
			out.push(PointF { x: end.x - normal.x + direction.x, y: end.y - normal.y + direction.y });
		}
		Cap::Round => {
			// THE ARC SWEEPS THE WAY THE OUTLINE IS GOING, from this side's normal round to the
			// other's. Sweeping the other way draws the half-circle back over the stroke's own body,
			// which leaves the cap missing and a notch where the two sides meet.
			let steps = arc_steps(half);
			let start = angle_of(normal);
			for step in 1..steps {
				let angle = start + core::f32::consts::PI * (step as f32 / steps as f32);
				out.push(PointF { x: end.x + cos_f32(angle) * half, y: end.y + sin_f32(angle) * half });
			}
		}
	}
}

/// Cut a contour into the pieces a dash pattern leaves.
///
/// THE PATTERN IS WALKED BY ARC LENGTH ALONG THE FLATTENED CONTOUR, which is the same length the path
/// query answers - so a dash pattern and a label placed at a distance agree about where they are. The
/// phase is where in the pattern the first dash starts, and an odd-length pattern repeats with its
/// roles swapped.
pub fn dashed(contours: &[Contour], pattern: &[f32], phase: f32) -> Vec<Contour> {
	let total: f32 = pattern.iter().filter(|length| length.is_finite() && **length > 0.0).sum();
	if !(total.is_finite() && total > 0.0) {
		return contours.to_vec();
	}
	let mut out: Vec<Contour> = Vec::new();
	for contour in contours {
		let points = dedupe(&contour.points);
		if points.len() < 2 {
			continue;
		}
		let mut walk: Vec<PointF> = points.clone();
		if contour.closed {
			walk.push(points[0]);
		}
		// THE PHASE IS TAKEN MODULO THE PATTERN so a marching-ants animation is one growing number
		// rather than a pattern index that eventually overflows.
		let period = total * if pattern.len() % 2 == 1 { 2.0 } else { 1.0 };
		let mut remaining = phase - libm::floorf(phase / period) * period;
		let mut index = 0usize;
		let mut on = true;
		while remaining > 0.0 {
			let length = pattern[index % pattern.len()].max(0.0);
			if remaining < length {
				break;
			}
			remaining -= length;
			index += 1;
			on = !on;
		}
		let mut left = (pattern[index % pattern.len()].max(0.0) - remaining).max(0.0);
		let mut current: Vec<PointF> = Vec::new();
		if on {
			current.push(walk[0]);
		}
		for window in walk.windows(2) {
			let (from, to) = (window[0], window[1]);
			let (dx, dy) = (to.x - from.x, to.y - from.y);
			let length = sqrt_f32(dx * dx + dy * dy);
			if !(length.is_finite() && length > 0.0) {
				continue;
			}
			let mut walked = 0.0f32;
			while length - walked > left {
				walked += left;
				let t = walked / length;
				let point = PointF { x: from.x + dx * t, y: from.y + dy * t };
				if on {
					current.push(point);
					out.push(Contour { points: core::mem::take(&mut current), closed: false });
				} else {
					current.clear();
					current.push(point);
				}
				on = !on;
				index += 1;
				left = pattern[index % pattern.len()].max(0.0);
				// A ZERO-LENGTH ENTRY WOULD NEVER ADVANCE THE WALK. The recorder refuses a pattern
				// with no positive length at all; a single zero inside one is skipped here.
				if left <= 0.0 {
					left = total;
				}
			}
			left -= length - walked;
			if on {
				current.push(to);
			}
		}
		if on && current.len() > 1 {
			out.push(Contour { points: current, closed: false });
		}
	}
	out
}

/// Drop points that repeat their predecessor, which have no direction to offset perpendicular to.
fn dedupe(points: &[PointF]) -> Vec<PointF> {
	let mut out: Vec<PointF> = Vec::with_capacity(points.len());
	for point in points {
		if !(point.x.is_finite() && point.y.is_finite()) {
			continue;
		}
		match out.last() {
			Some(last) if render2d::flatten::near(*last, *point) => {}
			_ => out.push(*point),
		}
	}
	out
}

fn circle(centre: PointF, radius: f32) -> Contour {
	let steps = arc_steps(radius);
	let mut points = Vec::with_capacity(steps as usize);
	for step in 0..steps {
		let angle = core::f32::consts::TAU * (step as f32 / steps as f32);
		points.push(PointF { x: centre.x + cos_f32(angle) * radius, y: centre.y + sin_f32(angle) * radius });
	}
	Contour { points, closed: true }
}

fn square(centre: PointF, half: f32) -> Contour {
	Contour {
		points: alloc::vec![
			PointF { x: centre.x - half, y: centre.y - half },
			PointF { x: centre.x + half, y: centre.y - half },
			PointF { x: centre.x + half, y: centre.y + half },
			PointF { x: centre.x - half, y: centre.y + half },
		],
		closed: true,
	}
}

fn angle_of(vector: PointF) -> f32 {
	libm::atan2f(vector.y, vector.x)
}

fn cos_f32(angle: f32) -> f32 {
	libm::cosf(angle)
}

fn sin_f32(angle: f32) -> f32 {
	libm::sinf(angle)
}

fn sqrt_f32(value: f32) -> f32 {
	libm::sqrtf(value)
}
