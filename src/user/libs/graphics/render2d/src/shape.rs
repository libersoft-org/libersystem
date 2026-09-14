//! THE SHAPES THE PROFILE NAMES, built into paths - and the conventions that make each of them ONE
//! shape rather than a family of similar ones.
//!
//! A BACKEND NEVER LEARNS THAT A PATH WAS A CIRCLE, which looks like a reason not to freeze any of
//! this. It is not: a fill rule, a dash phase, a stroke's first cap and a hit test all depend on
//! WHERE a shape starts and WHICH WAY it goes, so a circle that began at the top instead of the
//! right is a different drawing wherever any of those applies. The answers are in
//! `graphics_profile::geometry::SHAPE_RULES`, they are generated into the profile document, and
//! they are what this module implements.
//!
//! AND A QUARTER CIRCLE HAS NO EXACT CUBIC FORM, so the control ratio is a CHOICE and is frozen with
//! them. `QUADRANT_CONTROL_RATIO` is the value that puts the curve through the quadrant's midpoint
//! exactly; the other common answer is fitted to minimise the maximum radial error instead, and is a
//! better curve and a different one.

use graphics_core::geom::{PointF, RectF};
use graphics_profile::geometry::QUADRANT_CONTROL_RATIO;

use crate::Error;
use crate::path::PathBuilder;

/// The control-point ratio, at the width the points are stored in.
const KAPPA: f32 = QUADRANT_CONTROL_RATIO as f32;

/// A quarter turn, which is the most a single cubic of an arc may span.
const QUARTER_TURN: f32 = core::f32::consts::FRAC_PI_2;

/// A full turn, which is the most an arc may sweep however much is asked for.
const FULL_TURN: f32 = core::f32::consts::TAU;

impl PathBuilder {
	/// A rectangle with rounded corners, as four lines and four cubics.
	///
	/// THE RADII ARE SCALED TOGETHER WHEN THEY DO NOT FIT, by the smallest ratio any side demands.
	/// Clamping each corner to its own side's limit instead produces a rectangle whose corners have
	/// different curvatures - a shape nobody asked for, arrived at silently.
	pub fn add_rounded_rect(&mut self, rect: RectF, radius_x: f32, radius_y: f32) -> Result<&mut Self, Error> {
		let (radius_x, radius_y) = (finite_radius(radius_x, "a rounded rectangle's x radius")?, finite_radius(radius_y, "a rounded rectangle's y radius")?);
		// A RADIUS OF ZERO IS A SQUARE CORNER, and the shape is then EXACTLY the rectangle - the same
		// four verbs, so a caller that computed a radius of zero gets the shape it would have drawn.
		if radius_x <= 0.0 || radius_y <= 0.0 {
			return self.add_rect(rect);
		}
		let fit = fit_factor(rect.width, radius_x).min(fit_factor(rect.height, radius_y)).min(1.0);
		let (radius_x, radius_y) = (radius_x * fit, radius_y * fit);
		let (left, top, right, bottom) = (rect.x, rect.y, rect.right(), rect.bottom());
		let (grip_x, grip_y) = (radius_x * KAPPA, radius_y * KAPPA);

		self.move_to(PointF { x: left + radius_x, y: top })?;
		self.line_to(PointF { x: right - radius_x, y: top })?;
		self.cubic_to(PointF { x: right - radius_x + grip_x, y: top }, PointF { x: right, y: top + radius_y - grip_y }, PointF { x: right, y: top + radius_y })?;
		self.line_to(PointF { x: right, y: bottom - radius_y })?;
		self.cubic_to(PointF { x: right, y: bottom - radius_y + grip_y }, PointF { x: right - radius_x + grip_x, y: bottom }, PointF { x: right - radius_x, y: bottom })?;
		self.line_to(PointF { x: left + radius_x, y: bottom })?;
		self.cubic_to(PointF { x: left + radius_x - grip_x, y: bottom }, PointF { x: left, y: bottom - radius_y + grip_y }, PointF { x: left, y: bottom - radius_y })?;
		self.line_to(PointF { x: left, y: top + radius_y })?;
		self.cubic_to(PointF { x: left, y: top + radius_y - grip_y }, PointF { x: left + radius_x - grip_x, y: top }, PointF { x: left + radius_x, y: top })?;
		self.close()
	}

	/// A circle, which is the ellipse with one radius.
	pub fn add_circle(&mut self, centre: PointF, radius: f32) -> Result<&mut Self, Error> {
		self.add_ellipse(centre, radius, radius)
	}

	/// An ellipse, as FOUR cubics starting at the +x extreme and wound clockwise on screen.
	///
	/// NOT AS AN ARC OF A FULL TURN, although that would produce the same shape: the quadrant
	/// points and their control points are EXACT here - no sine, no cosine, no tangent - so a circle
	/// is the same bytes on every architecture rather than the same shape to within a rounding.
	pub fn add_ellipse(&mut self, centre: PointF, radius_x: f32, radius_y: f32) -> Result<&mut Self, Error> {
		let (radius_x, radius_y) = (finite_radius(radius_x, "an ellipse's x radius")?, finite_radius(radius_y, "an ellipse's y radius")?);
		let (grip_x, grip_y) = (radius_x * KAPPA, radius_y * KAPPA);
		let (x, y) = (centre.x, centre.y);

		self.move_to(PointF { x: x + radius_x, y })?;
		self.cubic_to(PointF { x: x + radius_x, y: y + grip_y }, PointF { x: x + grip_x, y: y + radius_y }, PointF { x, y: y + radius_y })?;
		self.cubic_to(PointF { x: x - grip_x, y: y + radius_y }, PointF { x: x - radius_x, y: y + grip_y }, PointF { x: x - radius_x, y })?;
		self.cubic_to(PointF { x: x - radius_x, y: y - grip_y }, PointF { x: x - grip_x, y: y - radius_y }, PointF { x, y: y - radius_y })?;
		self.cubic_to(PointF { x: x + grip_x, y: y - radius_y }, PointF { x: x + radius_x, y: y - grip_y }, PointF { x: x + radius_x, y })?;
		self.close()
	}

	/// An elliptical arc from `start_angle` through `sweep`, both in radians.
	///
	/// IT LINES TO ITS FIRST POINT WHEN A SUBPATH IS OPEN AND MOVES TO IT OTHERWISE, which is what
	/// makes an arc usable as one segment of a larger outline - a rounded tab, a pie slice with a
	/// straight edge - rather than only as a shape of its own. The subpath is left OPEN: closing it
	/// is the caller's decision, and it is the difference between a pie slice and an arc.
	pub fn add_arc(&mut self, centre: PointF, radius_x: f32, radius_y: f32, start_angle: f32, sweep: f32) -> Result<&mut Self, Error> {
		let (radius_x, radius_y) = (finite_radius(radius_x, "an arc's x radius")?, finite_radius(radius_y, "an arc's y radius")?);
		if !start_angle.is_finite() || !sweep.is_finite() {
			return Err(Error::DegenerateShape { what: "an arc angle that is not a finite number" });
		}
		// A SWEEP BEYOND A FULL TURN IS CLAMPED TO ONE. The second lap is invisible for a fill and
		// doubles the winding number for the non-zero rule, which turns a ring into a disc.
		let sweep = sweep.clamp(-FULL_TURN, FULL_TURN);
		let first = on_ellipse(centre, radius_x, radius_y, start_angle);
		if self.is_open() {
			self.line_to(first)?
		} else {
			self.move_to(first)?
		};
		// EQUAL SEGMENTS OF AT MOST A QUARTER TURN. Equal rather than "quarters and a remainder"
		// because a short final segment has a different error from its neighbours, which is visible
		// on a large arc as one flat patch.
		let segments = (abs(sweep) / QUARTER_TURN).ceil_to_u32().max(1);
		let step = sweep / segments as f32;
		let (grip_sine, grip_cosine) = sin_cos_f32(step * 0.25);
		// `4/3 * tan(step/4)`, which IS the frozen quadrant ratio when the step is a quarter turn.
		let grip = if grip_cosine == 0.0 { 0.0 } else { 4.0 / 3.0 * (grip_sine / grip_cosine) };
		let mut angle = start_angle;
		for _ in 0..segments {
			let next = angle + step;
			let (from, to) = (on_ellipse(centre, radius_x, radius_y, angle), on_ellipse(centre, radius_x, radius_y, next));
			let (from_tangent, to_tangent) = (tangent(radius_x, radius_y, angle), tangent(radius_x, radius_y, next));
			let first_control = PointF { x: from.x + grip * from_tangent.x, y: from.y + grip * from_tangent.y };
			let second_control = PointF { x: to.x - grip * to_tangent.x, y: to.y - grip * to_tangent.y };
			self.cubic_to(first_control, second_control, to)?;
			angle = next;
		}
		Ok(self)
	}

	/// A single segment, as its own OPEN subpath.
	pub fn add_line(&mut self, from: PointF, to: PointF) -> Result<&mut Self, Error> {
		self.move_to(from)?;
		self.line_to(to)
	}

	/// A chain of segments, OPEN.
	pub fn add_polyline(&mut self, points: &[PointF]) -> Result<&mut Self, Error> {
		self.add_chain(points, false, 2, "a polyline of fewer than two points")
	}

	/// A chain of segments, CLOSED - which is the whole difference from a polyline, and the reason
	/// this is a constructor rather than a polyline the caller remembered to close.
	pub fn add_polygon(&mut self, points: &[PointF]) -> Result<&mut Self, Error> {
		self.add_chain(points, true, 3, "a polygon of fewer than three points")
	}

	fn add_chain(&mut self, points: &[PointF], closed: bool, least: usize, what: &'static str) -> Result<&mut Self, Error> {
		if points.len() < least {
			return Err(Error::DegenerateShape { what });
		}
		self.move_to(points[0])?;
		for point in &points[1..] {
			self.line_to(*point)?;
		}
		if closed { self.close() } else { Ok(self) }
	}
}

/// A radius, or a refusal.
///
/// NEGATIVE AND NON-FINITE ARE REFUSED RATHER THAN CLAMPED. A radius below zero is a computed value
/// that went wrong - a subtraction of two sizes, usually - and drawing the clamped shape hides the
/// mistake in a picture that is merely slightly wrong.
fn finite_radius(radius: f32, what: &'static str) -> Result<f32, Error> {
	if !radius.is_finite() || radius < 0.0 {
		return Err(Error::DegenerateShape { what });
	}
	Ok(radius)
}

/// How much a side of this length allows a radius of this size, as a factor of one.
fn fit_factor(side: f32, radius: f32) -> f32 {
	if radius <= 0.0 || !side.is_finite() {
		return 1.0;
	}
	side / (radius * 2.0)
}

fn on_ellipse(centre: PointF, radius_x: f32, radius_y: f32, angle: f32) -> PointF {
	let (sine, cosine) = sin_cos_f32(angle);
	PointF { x: centre.x + radius_x * cosine, y: centre.y + radius_y * sine }
}

/// The derivative of `on_ellipse` with respect to the angle, which is what an arc's control points
/// are placed along.
fn tangent(radius_x: f32, radius_y: f32, angle: f32) -> PointF {
	let (sine, cosine) = sin_cos_f32(angle);
	PointF { x: -radius_x * sine, y: radius_y * cosine }
}

fn abs(value: f32) -> f32 {
	if value < 0.0 { -value } else { value }
}

/// `f32` has `ceil` in `std` and this crate is `no_std`, and the values here are segment counts.
trait CeilToU32 {
	fn ceil_to_u32(self) -> u32;
}

impl CeilToU32 for f32 {
	fn ceil_to_u32(self) -> u32 {
		if !self.is_finite() || self <= 0.0 {
			return 0;
		}
		let truncated = self as u32;
		if self > truncated as f32 { truncated.saturating_add(1) } else { truncated }
	}
}

/// Sine and cosine together, because an arc wants both at the same angle and they share the
/// reduction.
///
/// WRITTEN HERE AND NOT BORROWED FROM THE 3D MATH CRATE, deliberately. `render-math` is the 3D
/// contract - handedness, clip depth, winding - and a 2D drawing library that depended on it would
/// make every 2D consumer carry the 3D conventions in its provider list to get a sine. The tree
/// already keeps the two stacks' scalar maths separate for the same reason `sqrt_f32` is in
/// `transform`.
///
/// THE REDUCTION IS IN `f64` AND THE SERIES IS THE TAYLOR ONE over `[-pi/4, pi/4]`, where it is
/// accurate to the last representable `f32`. The fixtures hold it against the host's own sine and
/// cosine at the quadrant boundaries, which is where a reduction is wrong if it is wrong anywhere.
pub(crate) fn sin_cos_f32(radians: f32) -> (f32, f32) {
	if !radians.is_finite() {
		return (f32::NAN, f32::NAN);
	}
	const PI_OVER_2: f64 = core::f64::consts::FRAC_PI_2;
	let wide = radians as f64;
	let quadrant = round_to_nearest(wide / PI_OVER_2);
	let remainder = wide - quadrant * PI_OVER_2;
	let square = remainder * remainder;
	let sine = remainder * (1.0 - square / 6.0 * (1.0 - square / 20.0 * (1.0 - square / 42.0 * (1.0 - square / 72.0))));
	let cosine = 1.0 - square / 2.0 * (1.0 - square / 12.0 * (1.0 - square / 30.0 * (1.0 - square / 56.0)));
	let (sine, cosine) = match ((quadrant as i64) % 4 + 4) % 4 {
		0 => (sine, cosine),
		1 => (cosine, -sine),
		2 => (-sine, -cosine),
		_ => (-cosine, sine),
	};
	(sine as f32, cosine as f32)
}

fn round_to_nearest(value: f64) -> f64 {
	if value >= 0.0 { (value + 0.5) as i64 as f64 } else { (value - 0.5) as i64 as f64 }
}
