//! WHICH PIXELS A DRAWING TOUCHED.
//!
//! A COMPOSITOR THAT REDRAWS EVERYTHING EVERY FRAME IS A COMPOSITOR THAT BURNS A BATTERY on a blinking
//! cursor. Damage is what makes a small change cost a small amount, and computing it from the recorded
//! list rather than from the pixels is one of the reasons the list exists at all.
//!
//! IT IS A BOUNDED LIST WITH AN EXPLICIT WHOLE-SURFACE FALLBACK. An unbounded list is an allocation an
//! application decides the size of, and a list that silently merged everything into one rectangle when
//! it got long would look like damage tracking while doing none - so the fallback is a VALUE a
//! consumer can see.

use alloc::vec::Vec;

use graphics_core::geom::{Extent2D, PixelRect, RectF};

use crate::flatten::flatten;
use crate::list::{Command, DrawList};

/// What a drawing touched.
#[derive(Clone, PartialEq, Debug)]
pub enum Damage {
	/// These rectangles, and nothing else.
	Regions(Vec<PixelRect>),
	/// Everything. NOT an empty list and not one enormous rectangle: a consumer must be able to tell
	/// "the whole surface" from "a rectangle that happens to cover it", because the second can be
	/// intersected with a smaller clip and the first cannot.
	Whole,
}

/// How many rectangles are worth tracking before the whole surface is cheaper.
///
/// A NUMBER RATHER THAN A FEELING. Past it, the bookkeeping and the per-rectangle setup cost more than
/// redrawing everything - and having the number here means the point is the same on every backend
/// rather than wherever each one happened to give up.
pub const MAX_REGIONS: usize = 32;

/// Compute the damage a list produces on a surface.
pub fn of(list: &DrawList, extent: Extent2D) -> Damage {
	let mut regions: Vec<PixelRect> = Vec::new();
	for command in list.commands() {
		let Some(bounds) = bounds_of(list, command) else {
			// A COMMAND WHOSE BOUNDS CANNOT BE COMPUTED DAMAGES EVERYTHING. A layer with no stated
			// bounds, or geometry that crossed the projective horizon, could have touched anything -
			// and guessing smaller is how a stale region is left on the screen.
			return Damage::Whole;
		};
		let Some(rect) = to_pixels(bounds, extent) else { continue };
		if rect.is_empty() {
			continue;
		}
		if !regions.contains(&rect) {
			regions.push(rect);
		}
		if regions.len() > MAX_REGIONS {
			return Damage::Whole;
		}
	}
	Damage::Regions(regions)
}

/// The device bounds of one command, or `None` when it could have touched anything.
fn bounds_of(list: &DrawList, command: &Command) -> Option<RectF> {
	match command {
		Command::FillPath { path, transform, .. } | Command::PushClip { path, transform, .. } => {
			let path = list.resources().paths.get(path.0 as usize)?;
			contour_bounds(&flatten(path, Some(transform)))
		}
		Command::StrokePath { path, transform, style, .. } => {
			let path = list.resources().paths.get(path.0 as usize)?;
			let bounds = contour_bounds(&flatten(path, Some(transform)))?;
			// THE STROKE REACHES PAST THE PATH by its half-width and its miter, and a damage rectangle
			// that forgot the miter leaves the tip of every sharp corner on the screen.
			let reach = style.width * 0.5 * style.miter_limit.max(1.0);
			Some(RectF::new(bounds.x - reach, bounds.y - reach, bounds.width + reach * 2.0, bounds.height + reach * 2.0))
		}
		Command::DrawImage { destination, transform, .. } => transform.map_rect(*destination),
		// A MASK CLIP'S EXTENT IS THE IMAGE'S OWN and a list carries an image's identity rather than
		// its size, so what it damages is not answerable here: `None` is the honest answer and turns
		// into the whole surface, which is what a clip whose extent is unknown has to be.
		Command::PushClipMask { .. } => None,
		Command::DrawGlyphRun { run, transform, .. } => {
			let run = list.resources().glyph_runs.get(run.0 as usize)?;
			let mut advance = 0i32;
			for glyph in &run.glyphs {
				advance = advance.saturating_add(glyph.x_advance.raw());
			}
			// The run's own box, in the 26.6 the seam carries - and then through the transform, which
			// is where a rotated label's damage gets its real extent.
			let x = run.origin_x.raw() as f32 / 64.0;
			let y = run.origin_y.raw() as f32 / 64.0;
			let width = advance as f32 / 64.0;
			let height = run.size.raw() as f32 / 64.0 * 2.0;
			transform.map_rect(RectF::new(x, y - height * 0.5, width, height))
		}
		// A LAYER WITH STATED BOUNDS DAMAGES THEM; one without could be anything, and answering `None`
		// is what turns that into the whole surface rather than into a guess.
		Command::BeginLayer { bounds, .. } => *bounds,
		Command::PopClip | Command::EndLayer => Some(RectF::default()),
	}
}

fn contour_bounds(contours: &[crate::flatten::Contour]) -> Option<RectF> {
	let mut minimum = (f32::INFINITY, f32::INFINITY);
	let mut maximum = (f32::NEG_INFINITY, f32::NEG_INFINITY);
	let mut any = false;
	for contour in contours {
		for point in &contour.points {
			any = true;
			minimum.0 = minimum.0.min(point.x);
			minimum.1 = minimum.1.min(point.y);
			maximum.0 = maximum.0.max(point.x);
			maximum.1 = maximum.1.max(point.y);
		}
	}
	any.then(|| RectF::new(minimum.0, minimum.1, maximum.0 - minimum.0, maximum.1 - minimum.1))
}

/// A drawing rectangle as whole PHYSICAL pixels, clipped to the surface.
///
/// OUTWARDS ON EVERY SIDE. A damage rectangle rounded inwards leaves the antialiased edge of the thing
/// that moved on the screen, which is a one-pixel ghost - the artefact that gets reported as "the
/// window is dirty" and never reproduced.
fn to_pixels(rect: RectF, extent: Extent2D) -> Option<PixelRect> {
	if rect.is_empty() {
		return Some(PixelRect::default());
	}
	let left = floor(rect.x).max(0.0);
	let top = floor(rect.y).max(0.0);
	let right = ceil(rect.right()).min(extent.width as f32);
	let bottom = ceil(rect.bottom()).min(extent.height as f32);
	if !greater(right, left) || !greater(bottom, top) {
		return Some(PixelRect::default());
	}
	Some(PixelRect::new(left as u32, top as u32, (right - left) as u32, (bottom - top) as u32))
}

fn floor(value: f32) -> f32 {
	let truncated = value as i64 as f32;
	if value < truncated { truncated - 1.0 } else { truncated }
}

fn ceil(value: f32) -> f32 {
	let truncated = value as i64 as f32;
	if value > truncated { truncated + 1.0 } else { truncated }
}

/// Whether one value is strictly greater than another, with a NaN answering NO.
///
/// WRITTEN OUT BECAUSE EVERY COMPARISON WITH NaN IS FALSE, so `!(a > b)` and `a <= b` are different
/// questions when either can be NaN - and in geometry either can. This is the one that treats a NaN
/// as "not greater", which is what every caller here wants.
fn greater(left: f32, right: f32) -> bool {
	matches!(left.partial_cmp(&right), Some(core::cmp::Ordering::Greater))
}
