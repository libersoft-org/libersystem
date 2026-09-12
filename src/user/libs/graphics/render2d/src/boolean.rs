//! BOOLEAN OPERATIONS, implementing the profile's eight answers.
//!
//! TWO BACKENDS PRODUCE DIFFERENT UNIONS IF ANY OF THE EIGHT QUESTIONS IS LEFT OPEN, and the
//! difference is not subtle: it is a hole that is there in one and not the other. The answers are the
//! profile's; this is the arithmetic that obeys them.
//!
//! THE METHOD IS SPLIT-AND-CLASSIFY. Every edge of both operands is cut at every crossing, and each
//! resulting edge is kept or dropped by what is on each SIDE of it.
//!
//! THE SIDES AND NOT THE EDGE ITSELF, and that is the whole difficulty. An edge's own midpoint lies
//! exactly ON its own operand's boundary, where "inside" has no answer - and a classification that
//! asked there gets whatever the ray-casting convention happens to say, which drops the right-hand
//! edge of every square. So each edge is probed just off it, on both sides, and what is kept is the
//! edge that has the inside on one side and the outside on the other. That is what a boundary IS.
//!
//! THE PROBE DISTANCE IS THE COINCIDENCE EPSILON, which bounds what this can resolve: a feature
//! thinner than that is not a feature this operation can tell from a line, and the profile's own
//! epsilon is the number that says so.
//!
//! IT IS QUADRATIC IN THE EDGE COUNT AND SAYS SO. A sweep would be better asymptotically and is a
//! great deal easier to get subtly wrong at a coincident vertex; the profile's own path ceilings bound
//! the input, so the honest trade here is the simple algorithm with the stated cost.

use alloc::vec::Vec;

use graphics_core::geom::PointF;

use crate::Error;
use crate::flatten::{Contour, flatten, near};
use crate::path::{FillRule, Path, PathBuilder};

/// Which operation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Operation {
	Union,
	Intersection,
	/// `left` minus `right`.
	Difference,
	Xor,
}

/// Resolve one path's self-intersections into contours nothing crosses.
///
/// THE FIRST STEP OF EVERY BOOLEAN, and useful alone: a path drawn with a non-zero rule and one with
/// an even-odd rule are different SHAPES, and simplifying makes the shape explicit so that everything
/// afterwards can work on regions rather than on a rule.
pub fn simplify(path: &Path, rule: FillRule) -> Result<Path, Error> {
	let contours = flatten(path, None);
	let edges = split_all(&contours, &[]);
	let mut kept: Vec<Edge> = Vec::new();
	for edge in edges {
		let Some((left, right)) = edge.sides() else { continue };
		let inside = |point: PointF| {
			let (winding_number, crossings) = winding(&contours, point);
			match rule {
				FillRule::NonZero => winding_number != 0,
				FillRule::EvenOdd => crossings % 2 == 1,
			}
		};
		// AN EDGE WITH THE SAME ANSWER ON BOTH SIDES IS NOT A BOUNDARY. It is an edge in the interior
		// of the shape - which is exactly what a self-intersection leaves behind, and dropping it is
		// what "resolve the self-intersections" means.
		match (inside(left), inside(right)) {
			(true, false) => kept.push(edge),
			(false, true) => kept.push(edge.reversed()),
			_ => {}
		}
	}
	assemble(kept)
}

/// Combine two paths.
pub fn combine(left: &Path, left_rule: FillRule, right: &Path, right_rule: FillRule, operation: Operation) -> Result<Path, Error> {
	// EACH OPERAND IS RESOLVED UNDER ITS OWN FILL RULE FIRST, so the operation is on REGIONS and the
	// two rules never have to agree - which is the profile's answer to how two operands with different
	// rules combine.
	let left = flatten(&simplify(left, left_rule)?, None);
	let right = flatten(&simplify(right, right_rule)?, None);

	let left_edges = split_all(&left, &right);
	let right_edges = split_all(&right, &left);

	// AN EDGE IS ON THE RESULT'S BOUNDARY IFF THE RESULT IS INSIDE ON ONE SIDE AND OUTSIDE ON THE
	// OTHER. Classifying each operand's edges against the OTHER operand instead drops a COINCIDENT
	// edge twice - two rectangles meeting along a line lose the line, and the union comes out with a
	// gap in its boundary. Asking about the RESULT asks the only question that has one answer.
	let inside_left = |point: PointF| winding(&left, point).0 != 0;
	let inside_right = |point: PointF| winding(&right, point).0 != 0;
	let in_result = |point: PointF| {
		let (a, b) = (inside_left(point), inside_right(point));
		match operation {
			Operation::Union => a || b,
			Operation::Intersection => a && b,
			Operation::Difference => a && !b,
			Operation::Xor => a != b,
		}
	};
	let mut kept: Vec<Edge> = Vec::new();
	for edge in left_edges.into_iter().chain(right_edges) {
		let Some((left_side, right_side)) = edge.sides() else { continue };
		let oriented = match (in_result(left_side), in_result(right_side)) {
			// The result's interior is on the `(-dy, dx)` side, which is the profile's own winding.
			(true, false) => edge,
			(false, true) => edge.reversed(),
			_ => continue,
		};
		// A COINCIDENT EDGE IS KEPT ONCE. Both operands contributed it and both classified it the
		// same way, so the two copies are identical after orientation - and chaining a contour
		// through a doubled edge produces a contour that visits it twice.
		if kept.iter().any(|existing| near(existing.from, oriented.from) && near(existing.to, oriented.to)) {
			continue;
		}
		kept.push(oriented);
	}
	assemble(kept)
}

/// One directed segment.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Edge {
	from: PointF,
	to: PointF,
}

impl Edge {
	fn midpoint(&self) -> PointF {
		PointF { x: (self.from.x + self.to.x) * 0.5, y: (self.from.y + self.to.y) * 0.5 }
	}

	fn reversed(&self) -> Edge {
		Edge { from: self.to, to: self.from }
	}

	fn is_degenerate(&self) -> bool {
		near(self.from, self.to)
	}

	/// The two points just off this edge, at its midpoint: the `(-dy, dx)` side first.
	///
	/// FOR A POSITIVELY WOUND CONTOUR THE INTERIOR IS ON THE `(-dy, dx)` SIDE, which is the profile's
	/// own winding rule expressed as a direction - and stating it that way rather than as
	/// "counter-clockwise" is what makes it unambiguous when the y axis points down.
	fn sides(&self) -> Option<(PointF, PointF)> {
		let (dx, dy) = (self.to.x - self.from.x, self.to.y - self.from.y);
		let length = length_of(dx, dy);
		if !matches!(length.partial_cmp(&0.0), Some(core::cmp::Ordering::Greater)) {
			return None;
		}
		let probe = graphics_profile::geometry::COINCIDENCE_EPSILON_PIXELS as f32;
		let (nx, ny) = (-dy / length * probe, dx / length * probe);
		let middle = self.midpoint();
		Some((PointF { x: middle.x + nx, y: middle.y + ny }, PointF { x: middle.x - nx, y: middle.y - ny }))
	}
}

fn length_of(dx: f32, dy: f32) -> f32 {
	let squared = dx * dx + dy * dy;
	if !matches!(squared.partial_cmp(&0.0), Some(core::cmp::Ordering::Greater)) {
		return 0.0;
	}
	let mut estimate = f32::from_bits((squared.to_bits() >> 1) + (127u32 << 22));
	for _ in 0..4 {
		estimate = 0.5 * (estimate + squared / estimate);
	}
	estimate
}

/// Every edge of `contours`, cut at every crossing with itself and with `others`.
fn split_all(contours: &[Contour], others: &[Contour]) -> Vec<Edge> {
	let mut mine = edges_of(contours);
	let theirs = edges_of(others);
	let mut out: Vec<Edge> = Vec::new();
	for (index, edge) in mine.iter().enumerate() {
		// The parameters at which this edge is cut, collected and then sorted, so a single edge
		// crossed three times becomes four pieces in order rather than three overlapping ones.
		let mut cuts: Vec<f32> = Vec::new();
		for (other_index, other) in mine.iter().enumerate() {
			if other_index == index {
				continue;
			}
			if let Some(t) = crossing(edge, other) {
				cuts.push(t);
			}
		}
		for other in &theirs {
			if let Some(t) = crossing(edge, other) {
				cuts.push(t);
			}
		}
		cuts.push(0.0);
		cuts.push(1.0);
		cuts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
		for pair in cuts.windows(2) {
			let (start, end) = (pair[0], pair[1]);
			if end - start <= f32::EPSILON {
				continue;
			}
			let piece = Edge { from: at(edge, start), to: at(edge, end) };
			// DEGENERATE AND ZERO-LENGTH EDGES ARE DROPPED, which is the profile's own answer: an edge
			// with no direction cannot be classified and cannot be chained.
			if !piece.is_degenerate() {
				out.push(piece);
			}
		}
	}
	mine.clear();
	out
}

fn edges_of(contours: &[Contour]) -> Vec<Edge> {
	let mut out = Vec::new();
	for contour in contours {
		for index in 0..contour.points.len() {
			let from = contour.points[index];
			let to = contour.points[(index + 1) % contour.points.len()];
			let edge = Edge { from, to };
			if !edge.is_degenerate() {
				out.push(edge);
			}
		}
	}
	out
}

fn at(edge: &Edge, t: f32) -> PointF {
	PointF { x: edge.from.x + (edge.to.x - edge.from.x) * t, y: edge.from.y + (edge.to.y - edge.from.y) * t }
}

/// Where `edge` crosses `other`, as a parameter along `edge`, or `None`.
///
/// STRICTLY INSIDE THIS EDGE AND INCLUSIVELY ALONG THE OTHER. A crossing at THIS edge's own endpoint
/// is already a vertex and cutting there makes a zero-length piece; a crossing at the OTHER edge's
/// endpoint is the case that matters most, because it is exactly what a shared corner is - one
/// operand's vertex lying on the other's edge. Excluding it leaves that edge uncut, and an uncut edge
/// is then classified by a midpoint sitting on the other operand's boundary, where "inside" has no
/// answer.
fn crossing(edge: &Edge, other: &Edge) -> Option<f32> {
	let (r_x, r_y) = (edge.to.x - edge.from.x, edge.to.y - edge.from.y);
	let (s_x, s_y) = (other.to.x - other.from.x, other.to.y - other.from.y);
	let denominator = r_x * s_y - r_y * s_x;
	if denominator.abs() < f32::EPSILON {
		return None;
	}
	let (q_x, q_y) = (other.from.x - edge.from.x, other.from.y - edge.from.y);
	let t = (q_x * s_y - q_y * s_x) / denominator;
	let u = (q_x * r_y - q_y * r_x) / denominator;
	let strictly = |value: f32| value > 1e-6 && value < 1.0 - 1e-6;
	let along = |value: f32| (-1e-6..=1.0 + 1e-6).contains(&value);
	if strictly(t) && along(u) { Some(t) } else { None }
}

/// The winding number and the crossing count at a point, over a set of contours.
///
/// BOTH AT ONCE, because the two fill rules want different ones and computing them separately walks
/// the same edges twice.
fn winding(contours: &[Contour], point: PointF) -> (i32, u32) {
	let mut winding = 0i32;
	let mut crossings = 0u32;
	for contour in contours {
		for index in 0..contour.points.len() {
			let from = contour.points[index];
			let to = contour.points[(index + 1) % contour.points.len()];
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
	(winding, crossings)
}

/// Chain the kept edges into contours, and put them in the profile's canonical order and winding.
///
/// A RESULT WITH NO CONTOURS IS AN EMPTY PATH AND NOT A REFUSAL, which is the profile's answer: an
/// intersection of two shapes that do not touch is empty, and that is a correct answer rather than an
/// error.
fn assemble(edges: Vec<Edge>) -> Result<Path, Error> {
	let mut used = alloc::vec![false; edges.len()];
	let mut contours: Vec<Contour> = Vec::new();
	for start in 0..edges.len() {
		if used[start] {
			continue;
		}
		used[start] = true;
		let mut points = alloc::vec![edges[start].from];
		let mut end = edges[start].to;
		// A closed contour comes back to where it began; a chain that cannot be continued is closed
		// where it stopped, which is the only thing that can be done with it and is what the
		// profile's "open subpath is closed" answer says to do.
		loop {
			if near(end, points[0]) {
				break;
			}
			let Some(next) = (0..edges.len()).find(|index| !used[*index] && near(edges[*index].from, end)) else {
				break;
			};
			used[next] = true;
			points.push(edges[next].from);
			end = edges[next].to;
		}
		if points.len() >= 3 {
			contours.push(Contour { points, closed: true });
		}
	}
	// THE CANONICAL ORDER. The WINDING is already canonical: the classification oriented every edge
	// so the result's interior is on the `(-dy, dx)` side, which makes an outer contour come out
	// positive and a HOLE negative - and forcing every contour positive here would turn each hole
	// into a second filled region, which is the profile's rule read backwards.
	contours.sort_by(|left, right| {
		let key = |contour: &Contour| {
			let mut min_y = f32::INFINITY;
			let mut min_x = f32::INFINITY;
			for point in &contour.points {
				min_y = min_y.min(point.y);
				min_x = min_x.min(point.x);
			}
			// THE FIRST POINT IS THE THIRD KEY, because two contours can share a bounding-box corner -
			// a shape and the hole that touches it there - and an order that stopped at the box would
			// leave those two in whatever order the traversal happened to find them.
			(min_y, min_x, contour.points[0].y, contour.points[0].x)
		};
		let (left_key, right_key) = (key(left), key(right));
		let compare = |a: f32, b: f32| a.partial_cmp(&b).unwrap_or(core::cmp::Ordering::Equal);
		compare(left_key.0, right_key.0).then_with(|| compare(left_key.1, right_key.1)).then_with(|| compare(left_key.2, right_key.2)).then_with(|| compare(left_key.3, right_key.3))
	});

	let mut builder = PathBuilder::new();
	for contour in contours {
		builder.move_to(contour.points[0])?;
		for point in &contour.points[1..] {
			builder.line_to(*point)?;
		}
		builder.close()?;
	}
	Ok(builder.finish())
}
