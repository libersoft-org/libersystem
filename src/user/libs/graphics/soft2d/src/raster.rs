//! THE COVERAGE RASTERISER, which is the core of the whole 2D stack.
//!
//! SHAPES, STROKES, ROUNDED RECTANGLES, ELLIPSES, CLIP MASKS AND GLYPH MASKS ARE ALL PATHS THROUGH
//! HERE. There is one filling algorithm and everything else converts into it - a second rasteriser
//! for strokes would be a second set of rules about where an edge is, and the two would disagree at
//! exactly the places a drawing puts them next to each other.
//!
//! EXACT IN X, SAMPLED IN Y. Each pixel row is cut into sub-scanlines; on each of them the crossings
//! are computed exactly, sorted, and the inside intervals added to the row with ANALYTIC horizontal
//! coverage - a partially covered pixel at the end of an interval gets the fraction it is covered by,
//! not a sample count. The vertical direction is sampled because an analytic answer there needs the
//! edges sorted into an active list with their slopes, which buys accuracy the tolerance does not
//! ask for. A near-horizontal edge is therefore the worst case, at a sixteenth of a level per step.
//!
//! DETERMINISTIC FOR A GIVEN INPUT, which the profile requires: the crossings are sorted by a total
//! order, the intervals are walked in that order, and nothing here depends on a container's iteration
//! order or on how the work was divided into tiles.
//!
//! AND IT ALLOCATES NOTHING WHILE DRAWING. The two buffers are sized in `prepare`; `fill` reuses
//! them, and a shape wider than the reservation is clipped to it rather than growing it.

use alloc::vec::Vec;

use graphics_core::geom::PixelRect;
use render2d::blend::Antialias;
use render2d::flatten::Contour;
use render2d::path::FillRule;

/// Sub-scanlines per pixel row.
///
/// SIXTEEN IS THE NUMBER THE CONTRACT CAN AFFORD. The profile fixes that an edge HAS a coverage value
/// in `0..=255` and leaves the algorithm to the backend; sixteen steps quantise a near-horizontal
/// edge to sixteen levels, which is below what the conformance tolerance for 2D coverage allows, and
/// doubling it doubles the cost of every fill.
pub const SUBSAMPLES: u32 = 16;

/// One edge, as the rasteriser wants it: sorted by y, with the winding direction it lost by sorting.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Edge {
	top_x: f32,
	top_y: f32,
	bottom_y: f32,
	/// `dx/dy`, which is what a crossing costs once the edge is in the list.
	inverse_slope: f32,
	winding: i32,
}

/// A shape's edges, built ONCE.
///
/// BUILT IN `prepare` AND NOT PER TILE. A stroke of six curves is six hundred edges after flattening,
/// and a drawing that rebuilt them for each of the eighty tiles it crosses spends eighty times what
/// the geometry costs - which is the difference between a tiled renderer and a renderer that draws
/// the whole surface eighty times.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Edges {
	edges: Vec<Edge>,
	/// The vertical span the edges cover, so a tile outside it is skipped without touching them.
	top: f32,
	bottom: f32,
}

impl Edges {
	/// Build the edge list of a flattened contour set.
	pub fn build(contours: &[Contour]) -> Self {
		let mut edges = Vec::new();
		let mut top = f32::INFINITY;
		let mut bottom = f32::NEG_INFINITY;
		for contour in contours {
			let points = &contour.points;
			if points.len() < 2 {
				continue;
			}
			// A FILL CLOSES AN OPEN CONTOUR, because a winding number over an open one is not defined.
			for index in 0..points.len() {
				let from = points[index];
				let to = points[(index + 1) % points.len()];
				if !(from.x.is_finite() && from.y.is_finite() && to.x.is_finite() && to.y.is_finite()) {
					continue;
				}
				if from.y == to.y {
					// A HORIZONTAL EDGE CROSSES NO SCANLINE. It contributes nothing to a winding
					// number, and keeping it would put a crossing at every sub-scanline it lies on.
					continue;
				}
				let (top_point, bottom_point, winding) = if from.y < to.y { (from, to, 1) } else { (to, from, -1) };
				let inverse_slope = (bottom_point.x - top_point.x) / (bottom_point.y - top_point.y);
				top = top.min(top_point.y);
				bottom = bottom.max(bottom_point.y);
				edges.push(Edge { top_x: top_point.x, top_y: top_point.y, bottom_y: bottom_point.y, inverse_slope, winding });
			}
		}
		// SORTED BY THE SCANLINE THEY START AT, which is what lets the fill keep an ACTIVE list rather
		// than testing every edge of the shape against every sub-scanline. A stroke of six curves is
		// six hundred edges and a tile is a thousand sub-scanlines; without this the two multiply.
		edges.sort_by(|left, right| left.top_y.partial_cmp(&right.top_y).unwrap_or(core::cmp::Ordering::Equal));
		Self { edges, top, bottom }
	}

	pub fn len(&self) -> usize {
		self.edges.len()
	}

	pub fn is_empty(&self) -> bool {
		self.edges.is_empty()
	}

	/// Whether any edge reaches into a band of scanlines.
	fn reaches(&self, top: f32, bottom: f32) -> bool {
		!self.edges.is_empty() && self.bottom > top && self.top < bottom
	}
}

/// The fill machinery, with its scratch.
#[derive(Default)]
pub struct Rasteriser {
	coverage: Vec<f32>,
	crossings: Vec<(f32, i32)>,
	/// The edges that cross the scanline being swept, as indices into the shape's own list.
	active: Vec<u32>,
}

impl Rasteriser {
	pub fn new() -> Self {
		Self::default()
	}

	/// Reserve for a target of this width and a drawing of this many edges. CALLED IN `prepare`.
	pub fn reserve(&mut self, width: usize, edges: usize) {
		if self.coverage.len() < width {
			self.coverage.resize(width, 0.0);
		}
		self.crossings.reserve(edges.saturating_sub(self.crossings.capacity()));
	}

	/// What the reservation costs, which a caller compares against a budget.
	pub fn scratch_bytes(&self) -> u64 {
		(self.coverage.capacity() * core::mem::size_of::<f32>() + self.crossings.capacity() * core::mem::size_of::<(f32, i32)>() + self.active.capacity() * core::mem::size_of::<u32>()) as u64
	}

	/// Fill contours, emitting one row of coverage at a time.
	///
	/// THE EMITTED SLICE IS INDEXED FROM `bounds.x`, so a caller composites at `bounds.x + index`
	/// without arithmetic of its own - which is the arithmetic that goes wrong when a tile's origin
	/// and a clip's origin are not the same number.
	pub fn fill(&mut self, contours: &[Contour], rule: FillRule, antialias: Antialias, bounds: PixelRect, row: impl FnMut(u32, &[f32])) {
		let edges = Edges::build(contours);
		self.fill_edges(&edges, rule, antialias, bounds, row);
	}

	/// Fill a prebuilt edge list, emitting one row of coverage at a time.
	///
	/// THE EMITTED SLICE IS INDEXED FROM `bounds.x`, so a caller composites at `bounds.x + index`
	/// without arithmetic of its own - which is the arithmetic that goes wrong when a tile's origin
	/// and a clip's origin are not the same number.
	pub fn fill_edges(&mut self, edges: &Edges, rule: FillRule, antialias: Antialias, bounds: PixelRect, mut row: impl FnMut(u32, &[f32])) {
		if bounds.is_empty() || !edges.reaches(bounds.y as f32, (bounds.y + bounds.height) as f32) {
			return;
		}
		let (left, right) = (bounds.x as f32, bounds.x as f32 + bounds.width as f32);
		let width = bounds.width as usize;
		if self.coverage.len() < width {
			self.coverage.resize(width, 0.0);
		}
		let samples = match antialias {
			Antialias::On => SUBSAMPLES,
			// THE ALIASED PATH IS ONE SAMPLE AT THE PIXEL'S CENTRE, which is the rule a pixel-exact
			// grid, a one-pixel rule and a screenshot comparison need: a pixel is in or it is out.
			Antialias::Off => 1,
		};
		let weight = 1.0 / samples as f32;
		// THE SWEEP STARTS AT THIS TILE'S TOP. Everything that begins above it and reaches into it is
		// collected once, and everything that begins inside it is added as the sweep reaches it - so
		// the cost per sub-scanline is the edges that actually cross it.
		let start = bounds.y as f32;
		self.active.clear();
		let mut cursor = 0usize;
		while cursor < edges.edges.len() && edges.edges[cursor].top_y <= start {
			if edges.edges[cursor].bottom_y > start {
				self.active.push(cursor as u32);
			}
			cursor += 1;
		}
		for y in bounds.y..bounds.y.saturating_add(bounds.height) {
			for value in self.coverage[..width].iter_mut() {
				*value = 0.0;
			}
			let mut touched = false;
			for sample in 0..samples {
				let scan = y as f32 + (sample as f32 + 0.5) / samples as f32;
				while cursor < edges.edges.len() && edges.edges[cursor].top_y <= scan {
					if edges.edges[cursor].bottom_y > scan {
						self.active.push(cursor as u32);
					}
					cursor += 1;
				}
				// AN EDGE LEAVES THE LIST WHEN THE SWEEP PASSES ITS BOTTOM, which is a retain over the
				// active list and not a search over the shape.
				self.active.retain(|index| edges.edges[*index as usize].bottom_y > scan);
				self.crossings.clear();
				for index in self.active.iter() {
					let edge = &edges.edges[*index as usize];
					// HALF-OPEN IN Y: an edge covers `top <= scan < bottom`, so a vertex shared by two
					// edges is counted once rather than twice or not at all.
					if scan < edge.top_y {
						continue;
					}
					let x = edge.top_x + (scan - edge.top_y) * edge.inverse_slope;
					if x.is_finite() {
						self.crossings.push((x, edge.winding));
					}
				}
				if self.crossings.len() < 2 {
					continue;
				}
				self.crossings.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal));
				let mut winding = 0i32;
				let mut span_start = 0.0f32;
				let mut inside = false;
				for (x, direction) in self.crossings.iter().copied() {
					let was_inside = inside;
					winding += direction;
					inside = match rule {
						FillRule::NonZero => winding != 0,
						FillRule::EvenOdd => winding % 2 != 0,
					};
					if !was_inside && inside {
						span_start = x;
					} else if was_inside && !inside {
						touched |= add_span(&mut self.coverage[..width], span_start.max(left), x.min(right), left, weight, matches!(antialias, Antialias::Off));
					}
				}
			}
			if touched {
				row(y, &self.coverage[..width]);
			}
		}
	}
}

/// Add one horizontal interval's coverage to a row, ANALYTICALLY.
///
/// A PARTIALLY COVERED PIXEL GETS THE FRACTION IT IS COVERED BY. Rounding the ends of the interval to
/// whole pixels is what makes a slowly rotating rectangle's edge crawl, and it is the difference
/// between antialiasing and a jagged edge drawn sixteen times.
fn add_span(coverage: &mut [f32], from: f32, to: f32, origin: f32, weight: f32, aliased: bool) -> bool {
	if !(from.is_finite() && to.is_finite()) || to <= from {
		return false;
	}
	let (start, end) = if aliased {
		// THE ALIASED PATH IS A PIXEL IN OR OUT, in BOTH directions. Sampling the row at its centre
		// and then measuring the horizontal coverage exactly would produce a partial value on the one
		// axis and a binary one on the other, which is neither of the two things a caller can ask for.
		let first = libm::ceilf(from - origin - 0.5);
		let last = libm::ceilf(to - origin - 0.5);
		(first, last)
	} else {
		(from - origin, to - origin)
	};
	let first = libm::floorf(start).max(0.0) as usize;
	let last = (libm::ceilf(end).max(0.0) as usize).min(coverage.len());
	let mut touched = false;
	for (index, value) in coverage.iter_mut().enumerate().take(last).skip(first) {
		let pixel_left = index as f32;
		let pixel_right = pixel_left + 1.0;
		let covered = end.min(pixel_right) - start.max(pixel_left);
		if covered > 0.0 {
			*value += covered.min(1.0) * weight;
			touched = true;
		}
	}
	touched
}

/// THE ALIASED INTEGER LINE, kept as an explicit fast path with the rule it already had.
///
/// BOTH ENDPOINTS INCLUDED, ties at `error == 0` broken toward the SMALLER minor coordinate, and
/// clipping applied BEFORE rasterising so a clipped line covers the same pixels as the visible part
/// of the unclipped one. Those three sentences are the difference between two implementations of
/// Bresenham that agree and two that differ by a pixel at every shallow angle.
///
/// IT IS WRITTEN AS THE MAJOR AXIS WALKED EXACTLY AND THE MINOR ONE ROUNDED, rather than as an error
/// accumulator, because the tie rule is then a property of one rounding function that a fixture can
/// state - in the accumulator form the same rule is a condition on a running remainder, which is
/// where two implementations of "the same" algorithm come to differ.
pub fn aliased_line(from: (i32, i32), to: (i32, i32), clip: PixelRect, mut plot: impl FnMut(u32, u32)) {
	let Some(right) = clip.right() else { return };
	let Some(bottom) = clip.bottom() else { return };
	let inside = |x: i32, y: i32| x >= clip.x as i32 && y >= clip.y as i32 && (x as i64) < right as i64 && (y as i64) < bottom as i64;
	let (dx, dy) = (to.0 as i64 - from.0 as i64, to.1 as i64 - from.1 as i64);
	let steps = dx.abs().max(dy.abs());
	if steps == 0 {
		// BOTH ENDPOINTS ARE INCLUDED, and a line whose endpoints are one point is that point.
		if inside(from.0, from.1) {
			plot(from.0 as u32, from.1 as u32);
		}
		return;
	}
	for step in 0..=steps {
		let (x, y) = if dx.abs() >= dy.abs() {
			let x = from.0 as i64 + step * dx.signum();
			(x, from.1 as i64 + round_half_down(step * dy * dx.signum(), dx))
		} else {
			let y = from.1 as i64 + step * dy.signum();
			(from.0 as i64 + round_half_down(step * dx * dy.signum(), dy), y)
		};
		let (Ok(x), Ok(y)) = (i32::try_from(x), i32::try_from(y)) else { continue };
		if inside(x, y) {
			plot(x as u32, y as u32);
		}
	}
}

/// Round `numerator / denominator` to nearest, with an exact half going toward the SMALLER value.
fn round_half_down(numerator: i64, denominator: i64) -> i64 {
	let (numerator, denominator) = if denominator < 0 { (-numerator, -denominator) } else { (numerator, denominator) };
	if denominator == 0 {
		return 0;
	}
	(2 * numerator + denominator - 1).div_euclid(2 * denominator)
}
