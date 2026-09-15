//! THE COVERAGE RASTERISER, which is the core of the whole 2D stack.
//!
//! SHAPES, STROKES, ROUNDED RECTANGLES, ELLIPSES, CLIP MASKS AND GLYPH MASKS ARE ALL PATHS THROUGH
//! HERE. There is one filling algorithm and everything else converts into it - a second rasteriser
//! for strokes would be a second set of rules about where an edge is, and the two would disagree at
//! exactly the places a drawing puts them next to each other.
//!
//! EXACT IN BOTH DIRECTIONS, by accumulating AREA rather than by sampling. Each edge is clipped to
//! the pixel row it crosses and then to each pixel column it passes through, and what is accumulated
//! per pixel is two numbers: the signed vertical extent the edge spans there, and the area of that
//! pixel lying to the RIGHT of the edge. A left-to-right sweep of the row turns those into the
//! winding-weighted coverage of every pixel, exactly - a straight edge at any sub-pixel position
//! produces the area it actually covers, in x and in y alike.
//!
//! IT USED TO SAMPLE THE VERTICAL DIRECTION at a sixteenth of a row, on the argument that the
//! quantisation was "below what the conformance tolerance allows". It is not: the frozen render2d
//! threshold is 2/255 per pixel AGAINST THE ANALYTIC AREA, and half a sixteenth of a level is 8/255 -
//! four times it. That was measured by a text conformance run comparing a drawn frame against an
//! oracle computed from the geometry, and it is why this file is an accumulation rasteriser now. The
//! accumulation is also CHEAPER: one pass over the edges of a row rather than sixteen sweeps of
//! crossings, each with a sort.
//!
//! THE ALIASED PATH IS STILL A SAMPLE, and it has to be: "a pixel is in or it is out" is a different
//! question from "how much of it is covered", and a threshold on an area is not the same answer as a
//! sample at the centre for a sliver narrower than half a pixel.
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
	/// The signed vertical extent each pixel's edges span, and the area of each pixel to the RIGHT of
	/// them. One row's worth; a left-to-right sweep turns the pair into coverage.
	cover: Vec<f32>,
	area: Vec<f32>,
	crossings: Vec<(f32, i32)>,
	/// The edges that cross the row being filled, as indices into the shape's own list.
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
		if self.cover.len() < width {
			self.cover.resize(width, 0.0);
		}
		if self.area.len() < width {
			self.area.resize(width, 0.0);
		}
		self.crossings.reserve(edges.saturating_sub(self.crossings.capacity()));
	}

	/// What the reservation costs, which a caller compares against a budget.
	pub fn scratch_bytes(&self) -> u64 {
		((self.coverage.capacity() + self.cover.capacity() + self.area.capacity()) * core::mem::size_of::<f32>() + self.crossings.capacity() * core::mem::size_of::<(f32, i32)>() + self.active.capacity() * core::mem::size_of::<u32>()) as u64
	}

	/// Fill contours, emitting one row of coverage at a time.
	///
	/// THE EMITTED SLICE IS INDEXED FROM `bounds.x + start`, and `start` is the second argument. It
	/// used to be indexed from `bounds.x` alone, over the whole width of the bounds - and most rows
	/// of most shapes cover a handful of columns of a sixty-four-wide tile, so a caller then scanned
	/// sixty-four values to find three. The origin is still explicit rather than implied, which is
	/// the arithmetic that goes wrong when a tile's origin and a clip's origin are not the same
	/// number; there is simply one more term in it.
	pub fn fill(&mut self, contours: &[Contour], rule: FillRule, antialias: Antialias, bounds: PixelRect, row: impl FnMut(u32, usize, &[f32])) {
		let edges = Edges::build(contours);
		self.fill_edges(&edges, rule, antialias, bounds, row);
	}

	/// Fill a prebuilt edge list, emitting one row of coverage at a time. See `fill` for the slice's
	/// indexing.
	pub fn fill_edges(&mut self, edges: &Edges, rule: FillRule, antialias: Antialias, bounds: PixelRect, row: impl FnMut(u32, usize, &[f32])) {
		if bounds.is_empty() || !edges.reaches(bounds.y as f32, (bounds.y + bounds.height) as f32) {
			return;
		}
		match antialias {
			Antialias::On => self.fill_by_area(edges, rule, bounds, row),
			// A PIXEL IS IN OR IT IS OUT, which is a different question from how much of it is
			// covered: a pixel-exact grid, a one-pixel rule and a screenshot comparison all need the
			// binary answer, and thresholding an area is not the same answer for a sliver narrower
			// than half a pixel.
			Antialias::Off => self.fill_by_sample(edges, rule, bounds, row),
		}
	}

	/// THE ANTIALIASED FILL: exact area, accumulated.
	///
	/// For every edge, clipped to this row and then to each pixel column it passes through, two
	/// numbers are accumulated: `cover` is the signed vertical extent it spans in that pixel, which
	/// every pixel to its RIGHT is fully covered by; `area` is the part of that pixel itself lying to
	/// the right of it. A running sum of `cover` from the left, plus the pixel's own `area`, is the
	/// winding-weighted coverage - exactly, at any sub-pixel position, in both directions.
	fn fill_by_area(&mut self, edges: &Edges, rule: FillRule, bounds: PixelRect, mut row: impl FnMut(u32, usize, &[f32])) {
		let width = bounds.width as usize;
		if self.coverage.len() < width {
			self.coverage.resize(width, 0.0);
		}
		if self.cover.len() < width {
			self.cover.resize(width, 0.0);
		}
		if self.area.len() < width {
			self.area.resize(width, 0.0);
		}
		let left = bounds.x as f32;
		// THE ACCUMULATORS START CLEAN AND ARE LEFT CLEAN. Each row zeroes only the columns IT
		// touched, at the end of the row, so the next row finds them already zero - which is what
		// makes the per-row cost proportional to what the shape covers rather than to the tile's
		// width. `resize` above only zeroes the part it grows, so this pass is what establishes the
		// invariant the rows then keep.
		for value in self.cover[..width].iter_mut() {
			*value = 0.0;
		}
		for value in self.area[..width].iter_mut() {
			*value = 0.0;
		}

		// THE SWEEP STARTS AT THIS TILE'S TOP. Everything that begins above it and reaches into it is
		// collected once, and everything that begins inside it is added as the sweep reaches it.
		self.active.clear();
		let mut cursor = 0usize;
		let start = bounds.y as f32;
		while cursor < edges.edges.len() && edges.edges[cursor].top_y <= start {
			if edges.edges[cursor].bottom_y > start {
				self.active.push(cursor as u32);
			}
			cursor += 1;
		}

		for y in bounds.y..bounds.y.saturating_add(bounds.height) {
			let top = y as f32;
			let bottom = top + 1.0;
			while cursor < edges.edges.len() && edges.edges[cursor].top_y < bottom {
				if edges.edges[cursor].bottom_y > top {
					self.active.push(cursor as u32);
				}
				cursor += 1;
			}
			// AN EDGE LEAVES THE LIST WHEN THE SWEEP PASSES ITS BOTTOM, which is a retain over the
			// active list and not a search over the shape.
			self.active.retain(|index| edges.edges[*index as usize].bottom_y > top);
			if self.active.is_empty() {
				continue;
			}
			// WHAT LIES LEFT OF THE TILE COVERS ALL OF IT. An edge outside the tile on that side is
			// not skipped - every pixel in the row is to its right - and keeping that as a seed rather
			// than clamping the edge's x is what makes the answer the same whatever the tiling is.
			let mut seed = 0.0f32;
			let mut touched = Touched::none();
			for index in self.active.iter() {
				let edge = &edges.edges[*index as usize];
				let from_y = if edge.top_y > top { edge.top_y } else { top };
				let to_y = if edge.bottom_y < bottom { edge.bottom_y } else { bottom };
				if !(to_y > from_y) {
					continue;
				}
				let from_x = edge.top_x + (from_y - edge.top_y) * edge.inverse_slope;
				let to_x = edge.top_x + (to_y - edge.top_y) * edge.inverse_slope;
				if !(from_x.is_finite() && to_x.is_finite()) {
					continue;
				}
				accumulate(&mut self.cover[..width], &mut self.area[..width], &mut seed, &mut touched, from_x - left, from_y, to_x - left, to_y, edge.winding as f32);
			}
			// THE COLUMNS OUTSIDE THE TOUCHED RANGE HAVE ONE ANSWER EACH, and it is the same answer
			// for the whole run: nothing accumulated there, so the winding is the seed on the left
			// and whatever it became on the right. A run of one value is a fill rather than a
			// per-column evaluation, and where that value is zero the run is not emitted at all.
			let outside_left = wind(seed, rule);
			let Some((low, high)) = touched.range() else {
				if outside_left > 0.0 {
					self.coverage[..width].fill(outside_left);
					row(y, 0, &self.coverage[..width]);
				}
				continue;
			};
			let mut running = seed;
			for index in low..=high {
				let value = running + self.area[index];
				running += self.cover[index];
				self.coverage[index] = wind(value, rule);
				// LEFT CLEAN FOR THE NEXT ROW, in the same pass that reads them: a second loop over
				// the same range would touch the same cache lines twice for no reason.
				self.area[index] = 0.0;
				self.cover[index] = 0.0;
			}
			let outside_right = wind(running, rule);
			let first = if outside_left > 0.0 { 0 } else { low };
			let last = if outside_right > 0.0 { width } else { high + 1 };
			if first < low {
				self.coverage[first..low].fill(outside_left);
			}
			if last > high + 1 {
				self.coverage[high + 1..last].fill(outside_right);
			}
			row(y, first, &self.coverage[first..last]);
		}
	}

	/// THE ALIASED FILL: one sample at each pixel's centre, which is the rule it has always had.
	fn fill_by_sample(&mut self, edges: &Edges, rule: FillRule, bounds: PixelRect, mut row: impl FnMut(u32, usize, &[f32])) {
		let (left, right) = (bounds.x as f32, bounds.x as f32 + bounds.width as f32);
		let width = bounds.width as usize;
		if self.coverage.len() < width {
			self.coverage.resize(width, 0.0);
		}
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
			let scan = y as f32 + 0.5;
			while cursor < edges.edges.len() && edges.edges[cursor].top_y <= scan {
				if edges.edges[cursor].bottom_y > scan {
					self.active.push(cursor as u32);
				}
				cursor += 1;
			}
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
			if self.crossings.len() >= 2 {
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
						touched |= add_span(&mut self.coverage[..width], span_start.max(left), x.min(right), left, 1.0, true);
					}
				}
			}
			if touched {
				// THE ALIASED PATH EMITS THE WHOLE ROW, because it sorts crossings rather than
				// accumulating per column and therefore has no cheap answer for where the covered
				// run begins. Its callers scan for it, which is what they did for both paths before.
				row(y, 0, &self.coverage[..width]);
			}
		}
	}
}

/// The fill rule, applied to an accumulated winding-weighted area.
///
/// THE VALUE IS NOT A WINDING NUMBER AND NOT A COVERAGE, but the two multiplied: a pixel wholly
/// inside one contour accumulates `1`, one wholly inside two nested contours wound the same way
/// accumulates `2`, and a pixel an edge crosses accumulates the fraction. Saturating it is what makes
/// the non-zero rule an area; folding it is what makes the even-odd rule one.
fn wind(value: f32, rule: FillRule) -> f32 {
	match rule {
		FillRule::NonZero => {
			let magnitude = if value < 0.0 { -value } else { value };
			if magnitude > 1.0 { 1.0 } else { magnitude }
		}
		FillRule::EvenOdd => {
			let magnitude = if value < 0.0 { -value } else { value };
			let folded = magnitude - 2.0 * libm::floorf(magnitude / 2.0);
			if folded > 1.0 { 2.0 - folded } else { folded }
		}
	}
}

/// One edge, clipped to a row already, accumulated into that row's `cover` and `area`.
///
/// THE COORDINATES ARE TILE-RELATIVE: column zero is the tile's first pixel. `seed` takes everything
/// left of it, because every pixel of the row is to the right of an edge that far over.
/// THE COLUMNS ONE ROW'S EDGES REACHED, which is what makes the per-row work proportional to the
/// shape rather than to the tile.
///
/// A row of a thin stroke crossing a sixty-four-wide tile touches two or three columns; zeroing,
/// summing and evaluating all sixty-four was most of what such a row cost. Held as a pair rather
/// than a `Range` because the empty state has to be representable and `lo > hi` says it without a
/// second field.
struct Touched {
	low: usize,
	high: usize,
}

impl Touched {
	fn none() -> Self {
		Self { low: usize::MAX, high: 0 }
	}

	fn mark(&mut self, index: usize) {
		self.low = self.low.min(index);
		self.high = self.high.max(index);
	}

	fn range(&self) -> Option<(usize, usize)> {
		(self.low <= self.high).then_some((self.low, self.high))
	}
}

#[allow(clippy::too_many_arguments)]
fn accumulate(cover: &mut [f32], area: &mut [f32], seed: &mut f32, touched: &mut Touched, x0: f32, y0: f32, x1: f32, y1: f32, winding: f32) {
	let width = cover.len();
	let dx = x1 - x0;
	let dy = y1 - y0;
	if dy == 0.0 {
		return;
	}
	if dx == 0.0 {
		// A VERTICAL EDGE IS ONE COLUMN, and the column it is in is the one its x falls in - except
		// at a column boundary, where it belongs to the column on its right, which is the half-open
		// rule every other clip here uses.
		let column = libm::floorf(x0);
		emit(cover, area, seed, touched, width, column, x0, x0, dy * winding);
		return;
	}
	let (low_x, high_x) = if x0 < x1 { (x0, x1) } else { (x1, x0) };
	// EVERYTHING LEFT OF THE TILE IN ONE PIECE, rather than one column at a time: an edge a thousand
	// pixels to the left of a tile must not cost a thousand iterations.
	if low_x < 0.0 {
		let t = ((0.0 - x0) / dx).clamp(0.0, 1.0);
		let (outside_from, outside_to) = if x0 < x1 { (0.0, t) } else { (t, 1.0) };
		if outside_to > outside_from {
			*seed += (outside_to - outside_from) * dy * winding;
		}
	}
	if high_x <= 0.0 || low_x >= width as f32 {
		return;
	}
	// THE COLUMNS THE SEGMENT ACTUALLY CROSSES, clamped to the tile. A column whose clipped piece is
	// empty - which is what the one past the end is - is skipped by the `to > from` test below rather
	// than by arithmetic here, because an off-by-one in a bound is harder to see than a loop that
	// does nothing on its last step.
	let first = libm::floorf(low_x.max(0.0)) as usize;
	let last = (libm::floorf(high_x.min(width as f32)) as usize).min(width.saturating_sub(1));
	for column in first..=last {
		// THE SEGMENT CLIPPED TO ONE COLUMN, in the parameter it is straight in.
		let lower = (column as f32 - x0) / dx;
		let upper = ((column + 1) as f32 - x0) / dx;
		let (from, to) = if lower <= upper { (lower, upper) } else { (upper, lower) };
		let from = from.max(0.0);
		let to = to.min(1.0);
		if !(to > from) {
			continue;
		}
		let at_from = x0 + from * dx;
		let at_to = x0 + to * dx;
		emit(cover, area, seed, touched, width, column as f32, at_from, at_to, (to - from) * dy * winding);
	}
}

/// One piece of an edge inside one column.
#[allow(clippy::too_many_arguments)]
fn emit(cover: &mut [f32], area: &mut [f32], seed: &mut f32, touched: &mut Touched, width: usize, column: f32, from_x: f32, to_x: f32, extent: f32) {
	if extent == 0.0 {
		return;
	}
	if column < 0.0 {
		*seed += extent;
		return;
	}
	let index = column as usize;
	if index >= width {
		return;
	}
	// THE AREA OF THIS PIXEL TO THE RIGHT OF THE EDGE, which is what the pixel itself is covered by;
	// the pixels beyond it are covered by the whole extent, which is what `cover` carries.
	let middle = (from_x + to_x) * 0.5;
	let right = (column + 1.0 - middle).clamp(0.0, 1.0);
	area[index] += extent * right;
	cover[index] += extent;
	touched.mark(index);
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
