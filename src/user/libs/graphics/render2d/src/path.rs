//! PATHS: what a shape IS, and what may be asked of one.
//!
//! A PATH THAT ONLY SUPPORTS CONSTRUCTION IS HALF A PATH. A vector editor, a map, a chart, a diagram
//! and every custom control that responds to a click needs to ask whether a path contains a point -
//! and an application that cannot ask implements its own geometry, which is the private-rasteriser
//! failure one level up.
//!
//! THE VERBS AND THE POINTS ARE TWO ARRAYS, not one array of enums with inline coordinates. A path is
//! walked far more often than it is built, and a verb stream a backend can scan without touching the
//! points is what lets it count, bound and bin without decoding.

use alloc::vec::Vec;

use graphics_core::geom::{PointF, RectF};

use crate::Error;
use crate::transform::Transform;

/// One instruction of a path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verb {
	/// Begins a new subpath. One point.
	MoveTo,
	/// One point.
	LineTo,
	/// A quadratic: one control point and an end point.
	QuadTo,
	/// A cubic: two control points and an end point.
	CubicTo,
	/// Closes the current subpath back to its first point. No points.
	Close,
}

impl Verb {
	/// How many points this verb consumes.
	pub const fn points(self) -> usize {
		match self {
			Verb::MoveTo | Verb::LineTo => 1,
			Verb::QuadTo => 2,
			Verb::CubicTo => 3,
			Verb::Close => 0,
		}
	}
}

/// Which side of a crossing counts as inside.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FillRule {
	/// A point is inside when the winding number is not zero. The default, and the rule a boolean
	/// operation's RESULT is always in.
	#[default]
	NonZero,
	/// Inside when the crossing count is odd.
	EvenOdd,
}

/// A path: a verb stream and the points it consumes.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Path {
	verbs: Vec<Verb>,
	points: Vec<PointF>,
}

impl Path {
	pub fn verbs(&self) -> &[Verb] {
		&self.verbs
	}

	pub fn points(&self) -> &[PointF] {
		&self.points
	}

	pub fn is_empty(&self) -> bool {
		self.verbs.is_empty()
	}

	/// How many subpaths, which is what a limit is checked against.
	pub fn subpaths(&self) -> usize {
		self.verbs.iter().filter(|verb| **verb == Verb::MoveTo).count()
	}

	/// The bounding box of the path's own POINTS, control points included.
	///
	/// THE CONTROL POINTS ARE IN IT AND THE BOUNDS ARE THEREFORE LOOSE. A tight bound needs the
	/// curves' extrema, which is [`Self::tight_bounds`]; this one is what a broad-phase cull and a
	/// scratch estimate use, and it is never smaller than the drawing - which is the direction that
	/// matters, because a bound smaller than the drawing clips it.
	pub fn bounds(&self) -> Option<RectF> {
		bounds_of(self.points.iter().copied())
	}

	/// The bounding box of the CURVE, which is not its control polygon's.
	///
	/// A CONTROL POINT IS OFTEN WELL OUTSIDE THE CURVE, so the loose bound of a rounded rectangle can
	/// be noticeably larger than the shape - and a layer sized by it allocates an offscreen bigger
	/// than it needs, every frame.
	pub fn tight_bounds(&self) -> Option<RectF> {
		let mut extremes: Vec<PointF> = Vec::new();
		let mut index = 0usize;
		let mut current = PointF::default();
		for verb in &self.verbs {
			let taken = verb.points();
			let points = self.points.get(index..index + taken)?;
			index += taken;
			match verb {
				Verb::MoveTo | Verb::LineTo => {
					current = points[0];
					extremes.push(current);
				}
				Verb::QuadTo => {
					extremes.push(current);
					extremes.push(points[1]);
					// The quadratic's extremum on each axis, where its derivative is zero.
					for axis in 0..2 {
						let (p0, p1, p2) = (axis_of(current, axis), axis_of(points[0], axis), axis_of(points[1], axis));
						let denominator = p0 - 2.0 * p1 + p2;
						if denominator.abs() > f32::EPSILON {
							let t = (p0 - p1) / denominator;
							if t > 0.0 && t < 1.0 {
								extremes.push(quad_at(current, points[0], points[1], t));
							}
						}
					}
					current = points[1];
				}
				Verb::CubicTo => {
					extremes.push(current);
					extremes.push(points[2]);
					for axis in 0..2 {
						let (p0, p1, p2, p3) = (axis_of(current, axis), axis_of(points[0], axis), axis_of(points[1], axis), axis_of(points[2], axis));
						// The cubic's derivative is a quadratic; its roots are the extrema.
						let a = -p0 + 3.0 * p1 - 3.0 * p2 + p3;
						let b = 2.0 * (p0 - 2.0 * p1 + p2);
						let c = p1 - p0;
						for t in quadratic_roots(3.0 * a, 2.0 * b / 2.0 * 2.0, 3.0 * c) {
							if t > 0.0 && t < 1.0 {
								extremes.push(cubic_at(current, points[0], points[1], points[2], t));
							}
						}
					}
					current = points[2];
				}
				Verb::Close => {}
			}
		}
		bounds_of(extremes.into_iter())
	}

	/// Is a point inside the path, under a fill rule?
	///
	/// THE QUESTION A HIT TEST IS, and it is answered here rather than by every application. The curves
	/// are flattened at the profile's ONE tolerance, which is why a point can never be inside for a
	/// hit test and outside for the fill that drew it.
	pub fn contains(&self, point: PointF, rule: FillRule) -> bool {
		let mut winding = 0i32;
		let mut crossings = 0u32;
		self.for_each_line(|from, to| {
			if (from.y <= point.y) != (to.y <= point.y) {
				let t = (point.y - from.y) / (to.y - from.y);
				let x = from.x + t * (to.x - from.x);
				if x > point.x {
					crossings += 1;
					winding += if to.y > from.y { 1 } else { -1 };
				}
			}
		});
		match rule {
			FillRule::NonZero => winding != 0,
			FillRule::EvenOdd => crossings % 2 == 1,
		}
	}

	/// Walk the path as line segments at the profile's flattening tolerance, with every subpath
	/// CLOSED - which is what a fill and a hit test both see.
	pub fn for_each_line(&self, mut each: impl FnMut(PointF, PointF)) {
		let tolerance = graphics_profile::geometry::FLATTENING_TOLERANCE_PIXELS as f32;
		let mut index = 0usize;
		let mut current = PointF::default();
		let mut start = PointF::default();
		let mut open = false;
		for verb in &self.verbs {
			let taken = verb.points();
			let Some(points) = self.points.get(index..index + taken) else { return };
			index += taken;
			match verb {
				Verb::MoveTo => {
					if open && (current.x != start.x || current.y != start.y) {
						each(current, start);
					}
					current = points[0];
					start = current;
					open = true;
				}
				Verb::LineTo => {
					each(current, points[0]);
					current = points[0];
				}
				Verb::QuadTo => {
					let steps = subdivisions(current, points[1], tolerance);
					let mut previous = current;
					for step in 1..=steps {
						let t = step as f32 / steps as f32;
						let next = quad_at(current, points[0], points[1], t);
						each(previous, next);
						previous = next;
					}
					current = points[1];
				}
				Verb::CubicTo => {
					let steps = subdivisions(current, points[2], tolerance);
					let mut previous = current;
					for step in 1..=steps {
						let t = step as f32 / steps as f32;
						let next = cubic_at(current, points[0], points[1], points[2], t);
						each(previous, next);
						previous = next;
					}
					current = points[2];
				}
				Verb::Close => {
					if open {
						each(current, start);
						current = start;
					}
				}
			}
		}
		// AN OPEN SUBPATH IS CLOSED FOR A FILL, which is what every fill rule assumes - a winding
		// number over an open contour is not defined.
		if open && (current.x != start.x || current.y != start.y) {
			each(current, start);
		}
	}

	/// The path under a transform, or `None` when a point is beyond the projective horizon.
	pub fn transformed(&self, transform: &Transform) -> Option<Path> {
		let mut points = Vec::with_capacity(self.points.len());
		for point in &self.points {
			points.push(transform.map_point(*point)?);
		}
		Some(Path { verbs: self.verbs.clone(), points })
	}
}

fn axis_of(point: PointF, axis: usize) -> f32 {
	if axis == 0 { point.x } else { point.y }
}

fn quad_at(from: PointF, control: PointF, to: PointF, t: f32) -> PointF {
	let inverse = 1.0 - t;
	PointF { x: inverse * inverse * from.x + 2.0 * inverse * t * control.x + t * t * to.x, y: inverse * inverse * from.y + 2.0 * inverse * t * control.y + t * t * to.y }
}

fn cubic_at(from: PointF, first: PointF, second: PointF, to: PointF, t: f32) -> PointF {
	let inverse = 1.0 - t;
	let (a, b, c, d) = (inverse * inverse * inverse, 3.0 * inverse * inverse * t, 3.0 * inverse * t * t, t * t * t);
	PointF { x: a * from.x + b * first.x + c * second.x + d * to.x, y: a * from.y + b * first.y + c * second.y + d * to.y }
}

/// How many segments a curve becomes. BOUNDED by the profile's maximum subdivision depth, and the
/// remaining error at that bound is below anything a reader can see - so the segment is emitted
/// rather than the primitive refused.
fn subdivisions(from: PointF, to: PointF, tolerance: f32) -> u32 {
	let span = (to.x - from.x).abs().max((to.y - from.y).abs());
	let wanted = if tolerance > 0.0 { (span / tolerance) as u32 } else { 1 };
	let ceiling = 1u32 << graphics_profile::geometry::MAX_SUBDIVISION_DEPTH.min(12);
	wanted.clamp(1, ceiling)
}

fn quadratic_roots(a: f32, b: f32, c: f32) -> impl Iterator<Item = f32> {
	let mut roots = [f32::NAN; 2];
	if a.abs() < f32::EPSILON {
		if b.abs() > f32::EPSILON {
			roots[0] = -c / b;
		}
	} else {
		let discriminant = b * b - 4.0 * a * c;
		if discriminant >= 0.0 {
			let root = newton_sqrt(discriminant);
			roots[0] = (-b + root) / (2.0 * a);
			roots[1] = (-b - root) / (2.0 * a);
		}
	}
	roots.into_iter().filter(|value| value.is_finite())
}

fn newton_sqrt(value: f32) -> f32 {
	// A NaN OR A NEGATIVE HAS NO ROOT, and the comparison is written out because every comparison with
	// NaN is false: a plain `value <= 0.0` would fall through for a NaN and iterate on one.
	if !matches!(value.partial_cmp(&0.0), Some(core::cmp::Ordering::Greater)) {
		return 0.0;
	}
	let mut estimate = f32::from_bits((value.to_bits() >> 1) + (127u32 << 22));
	for _ in 0..4 {
		estimate = 0.5 * (estimate + value / estimate);
	}
	estimate
}

fn bounds_of(points: impl Iterator<Item = PointF>) -> Option<RectF> {
	let mut minimum = PointF { x: f32::INFINITY, y: f32::INFINITY };
	let mut maximum = PointF { x: f32::NEG_INFINITY, y: f32::NEG_INFINITY };
	let mut any = false;
	for point in points {
		any = true;
		minimum.x = minimum.x.min(point.x);
		minimum.y = minimum.y.min(point.y);
		maximum.x = maximum.x.max(point.x);
		maximum.y = maximum.y.max(point.y);
	}
	any.then(|| RectF::new(minimum.x, minimum.y, maximum.x - minimum.x, maximum.y - minimum.y))
}

/// Builds a path, refusing one that exceeds the profile's own ceilings.
///
/// THE LIMITS ARE CHECKED WHILE BUILDING AND NOT AFTER. A limit that could only be discovered by
/// exceeding it is a limit that has already allocated.
#[derive(Default)]
pub struct PathBuilder {
	path: Path,
	open: bool,
}

impl PathBuilder {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn move_to(&mut self, point: PointF) -> Result<&mut Self, Error> {
		self.push(Verb::MoveTo, &[point])?;
		self.open = true;
		Ok(self)
	}

	pub fn line_to(&mut self, point: PointF) -> Result<&mut Self, Error> {
		self.require_open()?;
		self.push(Verb::LineTo, &[point])
	}

	pub fn quad_to(&mut self, control: PointF, point: PointF) -> Result<&mut Self, Error> {
		self.require_open()?;
		self.push(Verb::QuadTo, &[control, point])
	}

	pub fn cubic_to(&mut self, first: PointF, second: PointF, point: PointF) -> Result<&mut Self, Error> {
		self.require_open()?;
		self.push(Verb::CubicTo, &[first, second, point])
	}

	pub fn close(&mut self) -> Result<&mut Self, Error> {
		self.require_open()?;
		self.push(Verb::Close, &[])
	}

	/// A rectangle, as the four lines it is - so that everything downstream has ONE kind of shape to
	/// handle rather than a rectangle special case that then needs its own clip, stroke and hit test.
	pub fn add_rect(&mut self, rect: RectF) -> Result<&mut Self, Error> {
		self.move_to(PointF { x: rect.x, y: rect.y })?;
		self.line_to(PointF { x: rect.right(), y: rect.y })?;
		self.line_to(PointF { x: rect.right(), y: rect.bottom() })?;
		self.line_to(PointF { x: rect.x, y: rect.bottom() })?;
		self.close()
	}

	pub fn finish(self) -> Path {
		self.path
	}

	/// A DRAWING CALL BEFORE A `move_to` IS A CALLER'S MISTAKE AND NOT AN IMPLICIT ORIGIN. Starting a
	/// subpath at (0,0) for them draws a line from the corner of the surface, which is a visible
	/// artefact whose cause is three functions away.
	fn require_open(&self) -> Result<(), Error> {
		if self.open { Ok(()) } else { Err(Error::PathNotStarted) }
	}

	fn push(&mut self, verb: Verb, points: &[PointF]) -> Result<&mut Self, Error> {
		let limits = graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS;
		if self.path.verbs.len() as u64 + 1 > limits.max_path_verbs as u64 {
			return Err(Error::LimitExceeded { limit: "path verbs", ceiling: limits.max_path_verbs as u64 });
		}
		if self.path.points.len() as u64 + points.len() as u64 > limits.max_path_points as u64 {
			return Err(Error::LimitExceeded { limit: "path points", ceiling: limits.max_path_points as u64 });
		}
		if verb == Verb::MoveTo && self.path.subpaths() as u64 + 1 > limits.max_subpaths as u64 {
			return Err(Error::LimitExceeded { limit: "subpaths", ceiling: limits.max_subpaths as u64 });
		}
		self.path.verbs.push(verb);
		self.path.points.extend_from_slice(points);
		Ok(self)
	}
}

/// How a stroke's ends are drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Cap {
	#[default]
	Butt,
	Round,
	Square,
}

/// How a stroke's corners are drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Join {
	#[default]
	Miter,
	Round,
	Bevel,
}

/// Everything about a stroke except its paint.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct StrokeStyle {
	pub width: f32,
	pub cap: Cap,
	pub join: Join,
	/// Past this ratio a miter becomes a bevel. WITHOUT IT A NEARLY PARALLEL JOIN PRODUCES A SPIKE
	/// hundreds of pixels long, which is the artefact that looks like a corrupt path.
	pub miter_limit: f32,
	/// Where the width, the dashes and the miter limit are measured.
	pub scaling: crate::transform::StrokeScaling,
	/// The dash pattern, as on-off lengths, and the offset into it. Empty is a solid stroke.
	pub dash: Option<DashHandleRange>,
}

/// A dash pattern's place in the list's own table, plus its phase.
///
/// THE PATTERN IS ON-OFF LENGTHS AND THE PHASE IS HOW FAR INTO IT THE FIRST DASH STARTS, which is
/// what makes a marching-ants selection an animation of one number rather than a new pattern per
/// frame. An odd-length pattern repeats with its roles swapped, which is the convention every
/// drawing API that has dashes uses.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct DashHandleRange {
	pub pattern: crate::resource::DashHandle,
	pub phase: f32,
}

impl Default for StrokeStyle {
	fn default() -> Self {
		Self { width: 1.0, cap: Cap::default(), join: Join::default(), miter_limit: 4.0, scaling: crate::transform::StrokeScaling::default(), dash: None }
	}
}
