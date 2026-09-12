//! THE ONE FLATTENING, shared by everything that consults geometry.
//!
//! IF A BOUNDS QUERY FLATTENS DIFFERENTLY FROM THE RASTERISER, AN APPLICATION ASKS WHERE SOMETHING IS
//! AND DRAWS IT SOMEWHERE ELSE. Fills, strokes, boolean operations, path length and hit testing all
//! come through here, at the profile's own tolerance, so the answer to "is this point inside" and the
//! answer to "which pixels did you cover" are computed from the same segments.
//!
//! ADAPTIVELY AND IN DEVICE SPACE. The tolerance is in PHYSICAL pixels, so a curve under a two-times
//! transform is subdivided twice as finely - flattening in user space makes a zoomed curve faceted at
//! exactly the zoom that shows it.
//!
//! AND THE DEPTH LIMIT EMITS A LINE RATHER THAN REFUSING. A curve that needed one more subdivision has
//! a remaining error below anything a reader can see at this tolerance; refusing the whole path over
//! it would make a legal drawing fail for a reason nobody can act on.

use alloc::vec::Vec;

use graphics_core::geom::PointF;

use crate::Error;
use crate::path::{Path, Verb};
use crate::transform::{Transform, sqrt_f32};

/// One flattened contour: its points, in order, and whether it was closed in the source.
#[derive(Clone, PartialEq, Debug)]
pub struct Contour {
	pub points: Vec<PointF>,
	/// Whether the source closed it. A fill closes an open contour anyway - a winding number over an
	/// open one is not defined - and a stroke does not, which is the difference this records.
	pub closed: bool,
}

impl Contour {
	/// Twice the signed area, which is what decides the winding without a division.
	///
	/// POSITIVE IS AN OUTER CONTOUR IN THE DEVICE'S Y-DOWN SPACE, which is the profile's own rule -
	/// and stating it in terms of the device rather than of "counter-clockwise" is what makes it
	/// unambiguous when the y axis points down.
	pub fn double_area(&self) -> f64 {
		let mut sum = 0.0f64;
		for index in 0..self.points.len() {
			let a = self.points[index];
			let b = self.points[(index + 1) % self.points.len()];
			sum += a.x as f64 * b.y as f64 - b.x as f64 * a.y as f64;
		}
		sum
	}

	/// Reverse the winding, which is what a difference does to the operand it subtracts.
	pub fn reversed(&self) -> Contour {
		let mut points = self.points.clone();
		points.reverse();
		Contour { points, closed: self.closed }
	}
}

/// The tolerance, in physical pixels, at the profile's own value.
pub fn tolerance() -> f32 {
	graphics_profile::geometry::FLATTENING_TOLERANCE_PIXELS as f32
}

/// A point BEFORE the perspective divide.
///
/// THE HORIZON ONLY EXISTS HERE. After the divide a point beyond `w == 0` is indistinguishable from
/// one in front of the viewer with the sign flipped, so the decision has to be taken while `w` is
/// still carried.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Homogeneous {
	x: f32,
	y: f32,
	w: f32,
}

impl Homogeneous {
	/// Whether this point is on the near side of the horizon.
	///
	/// THE COMPARISON IS WRITTEN OUT because every comparison with NaN is false: a plain `w > epsilon`
	/// would be false for NaN, which is the answer wanted, but `!(w <= epsilon)` would be true - and
	/// the pair of them differing on NaN is exactly the bug this avoids being written later.
	fn in_front(&self) -> bool {
		let epsilon = graphics_profile::geometry::PROJECTIVE_W_EPSILON as f32;
		matches!(self.w.partial_cmp(&epsilon), Some(core::cmp::Ordering::Greater))
	}

	/// Divide, refusing a result that is not a number a rasteriser can use.
	fn project(&self) -> Option<PointF> {
		let point = PointF { x: self.x / self.w, y: self.y / self.w };
		(point.x.is_finite() && point.y.is_finite()).then_some(point)
	}

	fn midpoint(&self, other: &Homogeneous) -> Homogeneous {
		Homogeneous { x: (self.x + other.x) * 0.5, y: (self.y + other.y) * 0.5, w: (self.w + other.w) * 0.5 }
	}

	fn lerp(&self, other: &Homogeneous, t: f32) -> Homogeneous {
		Homogeneous { x: self.x + (other.x - self.x) * t, y: self.y + (other.y - self.y) * t, w: self.w + (other.w - self.w) * t }
	}
}

/// Where a segment crosses the `w = epsilon` plane, in homogeneous space and before the divide.
fn crossing(from: &Homogeneous, to: &Homogeneous) -> Option<PointF> {
	let epsilon = graphics_profile::geometry::PROJECTIVE_W_EPSILON as f32;
	let span = to.w - from.w;
	if !span.is_finite() || span == 0.0 {
		return None;
	}
	let t = ((epsilon - from.w) / span).clamp(0.0, 1.0);
	from.lerp(to, t).project()
}

/// Builds contours from clipped segments.
///
/// A CONTOUR CUT BY THE HORIZON COMES BACK OPEN, and in two pieces when it crosses twice. Keeping it
/// marked closed would have a fill close it across the gap the horizon made, which is a shape nobody
/// drew.
struct Emitter {
	contours: Vec<Contour>,
	current: Vec<PointF>,
	open: bool,
	/// Whether the horizon cut or dropped anything in the subpath being built.
	cut: bool,
	/// Whether it cut or dropped anything at all, which is what decides a refusal.
	dropped: bool,
}

impl Emitter {
	fn new() -> Self {
		Self { contours: Vec::new(), current: Vec::new(), open: false, cut: false, dropped: false }
	}

	fn begin(&mut self, point: PointF) {
		self.current = alloc::vec![point];
		self.open = true;
	}

	fn push(&mut self, point: PointF) {
		if self.open {
			self.current.push(point);
		} else {
			self.begin(point);
		}
	}

	fn flush(&mut self, closed: bool) {
		if self.current.len() > 1 {
			let points = core::mem::take(&mut self.current);
			self.contours.push(Contour { points, closed });
		} else {
			self.current.clear();
		}
		self.open = false;
	}

	fn lost(&mut self) {
		self.cut = true;
		self.dropped = true;
	}

	/// One straight segment in homogeneous space, clipped against the horizon.
	fn segment(&mut self, from: &Homogeneous, to: &Homogeneous) {
		match (from.in_front(), to.in_front()) {
			(true, true) => {
				if !self.open
					&& let Some(start) = from.project()
				{
					self.begin(start);
				}
				if let Some(point) = to.project() {
					self.push(point);
				}
			}
			(true, false) => {
				if let Some(point) = crossing(from, to) {
					self.push(point);
				}
				self.lost();
				self.flush(false);
			}
			(false, true) => {
				self.lost();
				self.flush(false);
				if let Some(point) = crossing(from, to) {
					self.begin(point);
				}
				if let Some(point) = to.project() {
					self.push(point);
				}
			}
			(false, false) => {
				self.lost();
				self.flush(false);
			}
		}
	}
}

/// Flatten a path into contours, optionally under a transform.
///
/// THE TRANSFORM IS APPLIED BEFORE SUBDIVIDING and not after, which is what "in device space" means:
/// subdividing first and transforming after gives a curve whose segment count was chosen for the
/// wrong scale, and under a projective transform gives one whose segments are not even evenly spread.
pub fn flatten(path: &Path, transform: Option<&Transform>) -> Vec<Contour> {
	flatten_inner(path, transform).0
}

/// Flatten, REFUSING a geometry that has no image at all.
///
/// THE PRIMITIVE IS REFUSED ONLY IF NOTHING SURVIVES, which is the profile's own rule. A path with one
/// corner beyond the horizon is drawn without that corner; a path entirely beyond it is not a drawing
/// at all, and returning an empty result would have the caller believe it drew something.
pub fn flatten_checked(path: &Path, transform: Option<&Transform>) -> Result<Vec<Contour>, Error> {
	let (contours, dropped) = flatten_inner(path, transform);
	if contours.is_empty() && dropped { Err(Error::BeyondHorizon) } else { Ok(contours) }
}

fn flatten_inner(path: &Path, transform: Option<&Transform>) -> (Vec<Contour>, bool) {
	let tolerance = tolerance();
	let mut emitter = Emitter::new();
	let mut index = 0usize;
	let mut at = Homogeneous { x: 0.0, y: 0.0, w: 1.0 };
	let mut start = at;
	let map = |point: PointF| -> Homogeneous {
		match transform {
			Some(transform) => {
				let [x, y, w] = transform.map_homogeneous(point);
				Homogeneous { x, y, w }
			}
			None => Homogeneous { x: point.x, y: point.y, w: 1.0 },
		}
	};
	for verb in path.verbs() {
		let taken = verb.points();
		let Some(points) = path.points().get(index..index + taken) else { break };
		index += taken;
		match verb {
			Verb::MoveTo => {
				emitter.flush(false);
				emitter.cut = false;
				at = map(points[0]);
				start = at;
				if at.in_front() {
					match at.project() {
						Some(point) => emitter.begin(point),
						None => emitter.lost(),
					}
				} else {
					emitter.lost();
				}
			}
			Verb::LineTo => {
				let next = map(points[0]);
				emitter.segment(&at, &next);
				at = next;
			}
			Verb::QuadTo => {
				let (control, end) = (map(points[0]), map(points[1]));
				flatten_quad(&mut emitter, &at, &control, &end, tolerance, 0);
				at = end;
			}
			Verb::CubicTo => {
				let (first, second, end) = (map(points[0]), map(points[1]), map(points[2]));
				flatten_cubic(&mut emitter, &at, &first, &second, &end, tolerance, 0);
				at = end;
			}
			Verb::Close => {
				emitter.segment(&at, &start);
				at = start;
				let closed = !emitter.cut;
				emitter.flush(closed);
				emitter.cut = false;
			}
		}
	}
	emitter.flush(false);
	let mut contours = core::mem::take(&mut emitter.contours);
	// DUPLICATE ENDPOINTS ARE DROPPED. A contour whose last point repeats its first is the same shape
	// with a zero-length edge in it, and a zero-length edge has no direction - which is what a
	// boolean operation's winding classification needs from every edge it keeps.
	for contour in contours.iter_mut() {
		while contour.points.len() > 1 {
			let last = contour.points[contour.points.len() - 1];
			let first = contour.points[0];
			if near(last, first) {
				contour.points.pop();
			} else {
				break;
			}
		}
	}
	contours.retain(|contour| contour.points.len() > 1);
	(contours, emitter.dropped)
}

/// How close two points must be to be one, at the profile's own epsilon.
pub fn near(a: PointF, b: PointF) -> bool {
	let epsilon = graphics_profile::geometry::COINCIDENCE_EPSILON_PIXELS as f32;
	(a.x - b.x).abs() <= epsilon && (a.y - b.y).abs() <= epsilon
}

/// Subdivide a quadratic in homogeneous space until it is flat in DEVICE space.
///
/// DE CASTELJAU ON THE HOMOGENEOUS CONTROL POINTS IS THE CORRECT SPLIT FOR A PROJECTED CURVE: the
/// image of a Bezier under a projective transform is a rational Bezier with the same control points
/// carrying their own `w`, so splitting before the divide splits the curve that is actually drawn.
fn flatten_quad(emitter: &mut Emitter, from: &Homogeneous, control: &Homogeneous, to: &Homogeneous, tolerance: f32, depth: u32) {
	let front = [from.in_front(), control.in_front(), to.in_front()];
	// THE CONVEX HULL DECIDES A DROP WITHOUT SUBDIVIDING. Every point of the curve is a convex
	// combination of its control points, so a curve whose control points are all beyond the horizon is
	// beyond it everywhere, and no amount of splitting finds a piece that is not.
	if !front.iter().any(|front| *front) {
		emitter.segment(from, to);
		return;
	}
	let flat = front.iter().all(|front| *front) && deviation(from, &[*control], to) <= tolerance;
	if flat || depth >= graphics_profile::geometry::MAX_SUBDIVISION_DEPTH {
		emitter.segment(from, to);
		return;
	}
	let (first, second) = (from.midpoint(control), control.midpoint(to));
	let middle = first.midpoint(&second);
	flatten_quad(emitter, from, &first, &middle, tolerance, depth + 1);
	flatten_quad(emitter, &middle, &second, to, tolerance, depth + 1);
}

fn flatten_cubic(emitter: &mut Emitter, from: &Homogeneous, first: &Homogeneous, second: &Homogeneous, to: &Homogeneous, tolerance: f32, depth: u32) {
	let front = [from.in_front(), first.in_front(), second.in_front(), to.in_front()];
	if !front.iter().any(|front| *front) {
		emitter.segment(from, to);
		return;
	}
	let flat = front.iter().all(|front| *front) && deviation(from, &[*first, *second], to) <= tolerance;
	if flat || depth >= graphics_profile::geometry::MAX_SUBDIVISION_DEPTH {
		emitter.segment(from, to);
		return;
	}
	let (left, middle, right) = (from.midpoint(first), first.midpoint(second), second.midpoint(to));
	let (left_two, right_two) = (left.midpoint(&middle), middle.midpoint(&right));
	let centre = left_two.midpoint(&right_two);
	flatten_cubic(emitter, from, &left, &left_two, &centre, tolerance, depth + 1);
	flatten_cubic(emitter, &centre, &right_two, &right, to, tolerance, depth + 1);
}

/// How far the control points stray from the chord, in device space and after the divide.
///
/// A NON-FINITE DEVIATION COUNTS AS FLAT, which is what stops the recursion on a curve whose
/// coordinates have already overflowed: subdividing it further produces more overflow, not more
/// accuracy, and the depth limit's stated outcome - emit the segment as a line - is the same answer
/// one subdivision later.
fn deviation(from: &Homogeneous, controls: &[Homogeneous], to: &Homogeneous) -> f32 {
	let (Some(start), Some(end)) = (from.project(), to.project()) else { return 0.0 };
	let mut worst = 0.0f32;
	for control in controls {
		let Some(point) = control.project() else { return 0.0 };
		// The distance from the chord's LINE, not from its midpoint: a control point far along the
		// chord but exactly on it is a curve that is already straight.
		let (dx, dy) = (end.x - start.x, end.y - start.y);
		let length = sqrt_f32(dx * dx + dy * dy);
		let distance = if length > 0.0 {
			((point.x - start.x) * dy - (point.y - start.y) * dx).abs() / length
		} else {
			let (ox, oy) = (point.x - start.x, point.y - start.y);
			sqrt_f32(ox * ox + oy * oy)
		};
		if !distance.is_finite() {
			return 0.0;
		}
		worst = worst.max(distance);
	}
	worst
}
