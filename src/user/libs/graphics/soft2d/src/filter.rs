//! THE WHOLE `FilterGraph`, evaluated over surfaces of the canonical intermediate.
//!
//! A FILTER CHAIN IS WHAT AN APPLICATION ACTUALLY WANTS: a shadow is a blur of an alpha channel,
//! offset, tinted and composited UNDER the thing that cast it. An implementation with three named
//! effects has three effects; one with a graph has all of them.
//!
//! EVERY NODE READS ONLY NODES BEFORE IT, which the graph type guarantees by construction - so this
//! evaluator is a forward walk with no cycle check and no recursion, and a graph that would not
//! terminate cannot be built.
//!
//! THE BLUR IS SEPARABLE AND ITS KERNEL IS THE GAUSSIAN, not three box passes. Three boxes are faster
//! and are a different blur: the profile's bounds map grows the input by three standard deviations
//! because that is where a GAUSSIAN has fallen to nothing, and a box approximation with that bound
//! has a visible edge where the profile says there is none.

use alloc::vec::Vec;

use graphics_core::geom::{PixelRect, RectF};
use graphics_core::pixel::{Rgba, Working};
use render2d::Error;
use render2d::filter::{FilterGraph, FilterNode};

use crate::layer::Pool;
use crate::paint::to_working;
use crate::target::Surface;

/// Evaluate a graph over `source`, into a surface covering `bounds`.
///
/// THE SOURCE IS THE THING BEING FILTERED and the graph's `Source` node is where it enters. A graph
/// with no `Source` is a legal graph that ignores its input - a flood, a gradient of nodes, an image
/// composited on its own - and it is not an error.
///
/// THE NODE TABLE IS THE LANE'S AND THE KERNELS ARE THE PREPARED LIST'S. `results` holds one entry per
/// node while the graph runs and is reserved at `prepare` for the list's largest graph, and a blur's
/// weights were computed there with the graph (`kernels`), so evaluating a filter in a tile allocates
/// nothing - it used to build both, in every tile the filtered layer reached.
///
/// EACH NODE IS EVALUATED OVER WHAT THE NODES AFTER IT READ OF IT, and the last over `wanted` - the part
/// of `bounds` the layer is composited from. `bounds` is the tile grown by the graph's whole expansion,
/// which every node used to fill: an offset's three pixels widened a flood, a mask and a composite by
/// the blur's twelve. The regions come from the profile's own bounds map (`FilterNode::required_input`),
/// walked backwards from `wanted`, so a pixel inside `wanted` is the same number it was.
#[allow(clippy::too_many_arguments)]
pub fn evaluate(graph: &FilterGraph, kernels: &[Option<BlurKernel>], source: &Surface, backdrop: &dyn crate::target::Raster, bounds: PixelRect, wanted: PixelRect, pool: &mut Pool, spans: &mut crate::backend::Spans, results: &mut Vec<Option<Surface>>, working: Working, images: &dyn crate::paint::ImageLookup) -> Result<Surface, Error> {
	results.clear();
	let regions = regions(graph, bounds, wanted);
	let mut outcome: Result<Surface, Error> = Err(Error::Allocation);
	for (index, node) in graph.nodes().iter().enumerate() {
		let region = regions.get(index).copied().flatten().unwrap_or(PixelRect::new(bounds.x, bounds.y, 0, 0));
		// A NODE THAT SETS EVERY PIXEL OF `bounds` is given a surface cleared only beyond them; one that
		// may leave pixels unwritten - a missing input, a blur, a morphology, a tile with nothing to
		// repeat, an image that is not there - still starts from a cleared one.
		let whole = {
			let present = |slot: &u16| results.get(*slot as usize).is_some_and(Option::is_some);
			match node {
				FilterNode::Source | FilterNode::Backdrop | FilterNode::Flood { .. } => true,
				FilterNode::Offset { input, .. } | FilterNode::ColorMatrix { input, .. } | FilterNode::Convolution { input, .. } | FilterNode::Crop { input, .. } => present(input),
				FilterNode::Composite { source, backdrop, .. } | FilterNode::Blend { source, backdrop, .. } => present(source) && present(backdrop),
				FilterNode::DisplacementMap { input, map, .. } => present(input) && present(map),
				FilterNode::In { input, mask } => present(input) && present(mask),
				_ => false,
			}
		};
		let taken = if whole { pool.take_overwritten(bounds, region) } else { pool.take(bounds) };
		let Some(mut into) = taken else {
			// THE POOL IS THE RESERVATION `prepare` MADE, so exhausting it is a budget that was too
			// small rather than a machine that is out of memory - and the caller is told which.
			give_back(pool, results);
			return Err(Error::LimitExceeded { limit: "prepared scratch", ceiling: graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS.max_prepared_scratch_bytes });
		};
		let input = |slot: u16| -> Option<&Surface> { results.get(slot as usize).and_then(|entry| entry.as_ref()) };
		match node {
			FilterNode::Source => copy(source, &mut into, region),
			// THE BACKDROP IS WHAT IS ALREADY THERE, read before the layer composites over it - which
			// is what makes a frosted panel a blur of the scene rather than of itself.
			FilterNode::Backdrop => copy_from(backdrop, &mut into, region),
			FilterNode::Image(handle) => {
				if let Some((view, _, table)) = images.lookup(handle.0)
					&& let Ok(sampler) = graphics_core::sample::Sampler::with_table(view, working, graphics_core::sample::Spread::Clamp, table)
				{
					for y in region.y..region.y.saturating_add(region.height) {
						for x in region.x..region.x.saturating_add(region.width) {
							into.set(x, y, sampler.sample((x - bounds.x) as f32 + 0.5, (y - bounds.y) as f32 + 0.5, graphics_core::sample::Quality::Bilinear));
						}
					}
				}
			}
			FilterNode::Blur { input: slot, .. } => {
				// THE SEPARABLE BLUR NEEDS ONE MORE SURFACE, for what the horizontal pass wrote and
				// the vertical pass reads. Borrowing it from the pool rather than from the stack is
				// what keeps a tall tile from being a stack overflow.
				let horizontal = pool.take(bounds);
				let weights = kernels.get(index).and_then(|entry| entry.as_ref());
				match (input(*slot), horizontal, weights) {
					(Some(from), Some(mut horizontal), Some((across, down))) => {
						blur(from, &mut horizontal, &mut into, bounds, region, across, down, spans);
						pool.give(horizontal);
					}
					(_, Some(horizontal), _) => pool.give(horizontal),
					(_, None, _) => {
						pool.give(into);
						give_back(pool, results);
						return Err(Error::LimitExceeded { limit: "prepared scratch", ceiling: graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS.max_prepared_scratch_bytes });
					}
				}
			}
			FilterNode::Offset { input: slot, dx, dy } => {
				// A WHOLE-PIXEL OFFSET IS A SHIFTED FETCH: a bilinear sample at a whole pixel weighs its
				// other three taps by zero, so the pixel itself is the sample - one read where there
				// were four, and the drop shadow every interface draws is offset by whole pixels.
				let whole_pixels = libm::floorf(*dx) == *dx && libm::floorf(*dy) == *dy && libm::fabsf(*dx) < 65_536.0 && libm::fabsf(*dy) < 65_536.0;
				if let Some(from) = input(*slot)
					&& whole_pixels
				{
					let (shift_x, shift_y) = (*dx as i64, *dy as i64);
					for target_y in region.y..region.y.saturating_add(region.height) {
						for target_x in region.x..region.x.saturating_add(region.width) {
							let (source_x, source_y) = (target_x as i64 - shift_x, target_y as i64 - shift_y);
							let value = if source_x < 0 || source_y < 0 || source_x > u32::MAX as i64 || source_y > u32::MAX as i64 { Rgba::TRANSPARENT } else { from.get(source_x as u32, source_y as u32) };
							into.set(target_x, target_y, value);
						}
					}
				} else if let Some(from) = input(*slot) {
					for target_y in region.y..region.y.saturating_add(region.height) {
						for target_x in region.x..region.x.saturating_add(region.width) {
							// THE SAMPLE COMES FROM WHERE THE OFFSET CAME FROM, which is the source
							// MINUS the offset - getting the sign the other way moves a shadow to the
							// opposite side of the thing that cast it.
							let source_x = target_x as f32 - dx;
							let source_y = target_y as f32 - dy;
							into.set(target_x, target_y, sample_bilinear(from, source_x, source_y));
						}
					}
				}
			}
			FilterNode::ColorMatrix { input: slot, matrix } => {
				if let Some(from) = input(*slot) {
					for y in region.y..region.y.saturating_add(region.height) {
						for x in region.x..region.x.saturating_add(region.width) {
							into.set(x, y, colour_matrix(from.get(x, y), matrix));
						}
					}
				}
			}
			FilterNode::Flood { color } => {
				let colour = to_working(*color, working);
				for y in region.y..region.y.saturating_add(region.height) {
					for x in region.x..region.x.saturating_add(region.width) {
						into.set(x, y, colour);
					}
				}
			}
			FilterNode::Composite { source: source_slot, backdrop: backdrop_slot, operator } => {
				if let (Some(over), Some(under)) = (input(*source_slot), input(*backdrop_slot)) {
					for y in region.y..region.y.saturating_add(region.height) {
						for x in region.x..region.x.saturating_add(region.width) {
							into.set(x, y, graphics_core::composite::composite(*operator, render2d::blend::BlendMode::Normal, over.get(x, y), under.get(x, y)));
						}
					}
				}
			}
			FilterNode::Blend { source: source_slot, backdrop: backdrop_slot, mode } => {
				if let (Some(over), Some(under)) = (input(*source_slot), input(*backdrop_slot)) {
					for y in region.y..region.y.saturating_add(region.height) {
						for x in region.x..region.x.saturating_add(region.width) {
							into.set(x, y, graphics_core::composite::composite(render2d::blend::Operator::SrcOver, *mode, over.get(x, y), under.get(x, y)));
						}
					}
				}
			}
			// THE CONVOLUTION IS THREE BY THREE AND READS THROUGH THE SURFACE rather than through a
			// span buffer: nine taps a pixel is not what the blur's row cache exists for, and a
			// second buffered path would be a second place for the edge rule to be wrong.
			FilterNode::Convolution { input: slot, weights, divisor, bias } => {
				if let Some(from) = input(*slot) {
					// A DIVISOR OF ZERO IS THE SUM OF THE WEIGHTS, and a sum of zero is one - so an
					// edge-detect kernel, whose weights cancel, does not have to state the obvious.
					let sum: f32 = weights.iter().flatten().sum();
					let divisor = if *divisor != 0.0 {
						*divisor
					} else if sum != 0.0 {
						sum
					} else {
						1.0
					};
					for y in region.y..region.y.saturating_add(region.height) {
						for x in region.x..region.x.saturating_add(region.width) {
							let mut total = Rgba::TRANSPARENT;
							for (row, line) in weights.iter().enumerate() {
								for (column, weight) in line.iter().enumerate() {
									let tap_x = x as i64 + column as i64 - 1;
									let tap_y = y as i64 + row as i64 - 1;
									total = total.plus(at(from, tap_x, tap_y).scaled(*weight));
								}
							}
							into.set(x, y, finish_channels(total, divisor, *bias));
						}
					}
				}
			}
			FilterNode::MorphologyDilate { input: slot, x: radius_x, y: radius_y } => {
				if let Some(from) = input(*slot) {
					morphology(from, &mut into, region, *radius_x, *radius_y, true);
				}
			}
			FilterNode::MorphologyErode { input: slot, x: radius_x, y: radius_y } => {
				if let Some(from) = input(*slot) {
					morphology(from, &mut into, region, *radius_x, *radius_y, false);
				}
			}
			FilterNode::DisplacementMap { input: slot, map, scale, x_channel, y_channel } => {
				if let (Some(from), Some(displacement)) = (input(*slot), input(*map)) {
					for y in region.y..region.y.saturating_add(region.height) {
						for x in region.x..region.x.saturating_add(region.width) {
							// THE MAP IS READ UNPREMULTIPLIED, because a displacement is a NUMBER
							// carried in a channel rather than a colour: a half-transparent red that
							// meant "half a scale to the right" would mean a quarter of one premultiplied.
							let value = straight(displacement.get(x, y));
							let offset_x = scale * (channel_of(value, *x_channel) - 0.5);
							let offset_y = scale * (channel_of(value, *y_channel) - 0.5);
							into.set(x, y, sample_bilinear(from, x as f32 + offset_x, y as f32 + offset_y));
						}
					}
				}
			}
			FilterNode::Crop { input: slot, rect } => {
				if let Some(from) = input(*slot) {
					let keep = pixel_rect(*rect);
					for y in region.y..region.y.saturating_add(region.height) {
						for x in region.x..region.x.saturating_add(region.width) {
							let inside = x >= keep.0 && y >= keep.1 && x < keep.2 && y < keep.3;
							into.set(x, y, if inside { from.get(x, y) } else { Rgba::TRANSPARENT });
						}
					}
				}
			}
			FilterNode::Tile { input: slot, rect } => {
				if let Some(from) = input(*slot) {
					let keep = pixel_rect(*rect);
					let (width, height) = (keep.2.saturating_sub(keep.0), keep.3.saturating_sub(keep.1));
					if width > 0 && height > 0 {
						for y in region.y..region.y.saturating_add(region.height) {
							for x in region.x..region.x.saturating_add(region.width) {
								// WRAPPED ABOUT THE RECTANGLE'S OWN ORIGIN, so the tile that lands on
								// the rectangle is the rectangle - a wrap about the surface origin
								// would shift the pattern whenever the tile moved.
								let tile_x = keep.0 + wrap(x as i64 - keep.0 as i64, width);
								let tile_y = keep.1 + wrap(y as i64 - keep.1 as i64, height);
								into.set(x, y, from.get(tile_x, tile_y));
							}
						}
					}
				}
			}
			FilterNode::In { input: slot, mask } => {
				if let (Some(from), Some(mask)) = (input(*slot), input(*mask)) {
					for y in region.y..region.y.saturating_add(region.height) {
						for x in region.x..region.x.saturating_add(region.width) {
							// KEEP ONLY WHERE THE MASK HAS ALPHA, which is what every clip-shaped
							// effect is built on.
							into.set(x, y, from.get(x, y).scaled(mask.get(x, y).alpha));
						}
					}
				}
			}
		}
		let last = index + 1 == graph.nodes().len();
		if last {
			outcome = Ok(into);
			results.push(None);
		} else {
			results.push(Some(into));
		}
	}
	give_back(pool, results);
	outcome
}

// THE REGION EACH NODE IS EVALUATED OVER, walked backwards from the last node's `wanted`: a node's
// input must cover what the node's own region needs of it, which is the profile's bounds map; a node
// nothing reads is evaluated over nothing. EVERY REGION LIES INSIDE `bounds`, where the surfaces are.
//
// THE BLUR READS ITS INPUT OVER ALL OF `bounds` and writes only its region: where `bounds` ends is where
// its source ends, whatever part of it the nodes after it read.
fn regions(graph: &FilterGraph, bounds: PixelRect, wanted: PixelRect) -> [Option<PixelRect>; MAX_NODES] {
	let mut regions = [None; MAX_NODES];
	let nodes = graph.nodes();
	let count = nodes.len().min(MAX_NODES);
	if count == 0 {
		return regions;
	}
	regions[count - 1] = Some(wanted.intersection(&bounds));
	for index in (0..count).rev() {
		let Some(want) = regions[index] else { continue };
		let node = &nodes[index];
		let asked = RectF::new(want.x as f32, want.y as f32, want.width as f32, want.height as f32);
		let needed = crate::tile::cover(node.required_input(asked)).intersection(&bounds);
		for input in node.inputs() {
			if let Some(slot) = regions.get_mut(input as usize) {
				*slot = Some(match *slot {
					Some(held) => union(held, needed),
					None => needed,
				});
			}
		}
	}
	regions
}

// The smallest rectangle covering both - an empty one covers nothing.
fn union(left: PixelRect, right: PixelRect) -> PixelRect {
	if left.width == 0 || left.height == 0 {
		return right;
	}
	if right.width == 0 || right.height == 0 {
		return left;
	}
	let (x, y) = (left.x.min(right.x), left.y.min(right.y));
	let right_edge = left.x.saturating_add(left.width).max(right.x.saturating_add(right.width));
	let bottom_edge = left.y.saturating_add(left.height).max(right.y.saturating_add(right.height));
	PixelRect::new(x, y, right_edge - x, bottom_edge - y)
}

// The most nodes a graph holds, which the profile bounds.
const MAX_NODES: usize = graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS.max_filter_nodes as usize;

fn give_back(pool: &mut Pool, results: &mut Vec<Option<Surface>>) {
	for surface in results.drain(..).flatten() {
		pool.give(surface);
	}
}

fn copy_from(from: &dyn crate::target::Raster, into: &mut Surface, bounds: PixelRect) {
	for y in bounds.y..bounds.y.saturating_add(bounds.height) {
		for x in bounds.x..bounds.x.saturating_add(bounds.width) {
			into.set(x, y, from.get(x, y));
		}
	}
}

fn copy(from: &Surface, into: &mut Surface, bounds: PixelRect) {
	for y in bounds.y..bounds.y.saturating_add(bounds.height) {
		for x in bounds.x..bounds.x.saturating_add(bounds.width) {
			into.set(x, y, from.get(x, y));
		}
	}
}

/// A SEPARABLE GAUSSIAN: the horizontal pass into the target, then the vertical pass in place.
///
/// SEPARABLE BECAUSE A TWO-DIMENSIONAL GAUSSIAN IS THE PRODUCT OF TWO ONE-DIMENSIONAL ONES, which
/// turns a radius-squared kernel into two radius-sized ones - at three standard deviations and a
/// twenty-pixel blur that is the difference between four thousand taps a pixel and a hundred and
/// twenty.
///
/// THE SOURCE IS ALL OF `bounds` AND THE OUTPUT ONLY `region`: the horizontal pass fills the region's columns
/// over its rows and the vertical kernel's reach above and below them, and the vertical pass the region -
/// each pixel from the same taps, added in the same order, as when the whole of `bounds` was filled.
#[allow(clippy::too_many_arguments)]
fn blur(from: &Surface, horizontal_pass: &mut Surface, into: &mut Surface, bounds: PixelRect, region: PixelRect, horizontal: &[f32], vertical: &[f32], spans: &mut crate::backend::Spans) {
	let (left, top) = (bounds.x, bounds.y);
	let (width, height) = (bounds.width as usize, bounds.height as usize);
	let reach = |kernel: &[f32]| (kernel.len() as i64 - 1) / 2;
	// THE REGION IN `bounds`' OWN COLUMNS AND ROWS, and the rows the horizontal pass must fill for it.
	let region = region.intersection(&bounds);
	let (columns_from, columns_to) = ((region.x - left) as usize, (region.x - left + region.width) as usize);
	let (rows_from, rows_to) = ((region.y - top) as usize, (region.y - top + region.height) as usize);
	let down_reach = reach(vertical).max(0) as usize;
	let (pass_from, pass_to) = (rows_from.saturating_sub(down_reach), (rows_to + down_reach).min(height));
	// THE ROW IS READ ONCE, CONVOLVED IN A BUFFER AND WRITTEN ONCE. Reading each tap through the
	// surface would fetch one pixel at a time through a bounds check, a row lookup and a half-float
	// decode - twenty-five times per pixel for a four-pixel blur.
	if width > 0 && spans.filter_input.len() >= width {
		let offset = reach(horizontal);
		for y in top + pass_from as u32..top + pass_to as u32 {
			from.read_span(left, y, &mut spans.filter_input[..width]);
			// THE INTERIOR HAS NO EDGE TO TEST FOR, and it is nearly all of the row. Every tap of
			// every pixel asked whether it had fallen off the source - two comparisons and a branch
			// inside a loop that runs `2 * radius + 1` times per pixel, which for the blur in a real
			// scene is sixty times. A pixel at least `offset` from either end cannot have a tap
			// outside, so the question is answered once for the whole run rather than per tap.
			//
			// OUTSIDE THE SOURCE IS TRANSPARENT AND NOT THE EDGE PIXEL, which is what the edges below
			// still evaluate. A blur that clamped its edge would smear the border of a layer outward,
			// which is visible as a bright rim around every shadow.
			//
			// THE ORDER OF THE ADDITIONS IS UNCHANGED - tap zero to tap last, for every pixel - which
			// is what makes this the same number and not merely a close one.
			let interior = (offset.max(0) as usize).min(width)..width.saturating_sub(offset.max(0) as usize).max((offset.max(0) as usize).min(width));
			for x in columns_from..columns_to {
				let mut sum = Rgba::TRANSPARENT;
				if interior.contains(&x) {
					let base = x - offset.max(0) as usize;
					for (index, weight) in horizontal.iter().enumerate() {
						sum = sum.plus(spans.filter_input[base + index].scaled(*weight));
					}
				} else {
					for (index, weight) in horizontal.iter().enumerate() {
						let tap = x as i64 + index as i64 - offset;
						if tap >= 0 && (tap as usize) < width {
							sum = sum.plus(spans.filter_input[tap as usize].scaled(*weight));
						}
					}
				}
				spans.filter_output[x] = sum;
			}
			horizontal_pass.write_span(region.x, y, &spans.filter_output[columns_from..columns_to]);
		}
	}
	if height > 0 && spans.filter_input.len() >= height {
		let offset = reach(vertical);
		for x in region.x..region.x + region.width {
			// THE COLUMN IS READ AND WRITTEN AS A RUN, which is what the first pass already did for
			// its rows. Walking it with `get` and `set` recomputed the local coordinates, the bounds
			// check and the byte offset for every pixel - twice, once each way - over the whole of
			// the second pass of every blur in the frame.
			horizontal_pass.read_column(x, top, &mut spans.filter_input[..height]);
			// The same split as the first pass, for the same reason.
			let interior = (offset.max(0) as usize).min(height)..height.saturating_sub(offset.max(0) as usize).max((offset.max(0) as usize).min(height));
			for y in rows_from..rows_to {
				let mut sum = Rgba::TRANSPARENT;
				if interior.contains(&y) {
					let base = y - offset.max(0) as usize;
					for (index, weight) in vertical.iter().enumerate() {
						sum = sum.plus(spans.filter_input[base + index].scaled(*weight));
					}
				} else {
					for (index, weight) in vertical.iter().enumerate() {
						let tap = y as i64 + index as i64 - offset;
						if tap >= 0 && (tap as usize) < height {
							sum = sum.plus(spans.filter_input[tap as usize].scaled(*weight));
						}
					}
				}
				spans.filter_output[y] = sum;
			}
			into.write_column(x, region.y, &spans.filter_output[rows_from..rows_to]);
		}
	}
}

/// A blur's weights across and down.
pub type BlurKernel = (Vec<f32>, Vec<f32>);

/// Every blur node's two kernels, in node order, for `prepare` to hold with the graph.
pub fn kernels(graph: &FilterGraph) -> Vec<Option<BlurKernel>> {
	graph
		.nodes()
		.iter()
		.map(|node| match node {
			FilterNode::Blur { x, y, .. } => Some((kernel(*x), kernel(*y))),
			_ => None,
		})
		.collect()
}

/// A normalised Gaussian kernel out to three standard deviations, which is the reach the profile's
/// bounds map promises.
fn kernel(sigma: f32) -> Vec<f32> {
	let sigma = sigma.abs();
	if !(sigma.is_finite() && sigma > 0.0) {
		return alloc::vec![1.0];
	}
	let ceiling = graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS.max_filter_radius as f32;
	let radius = (libm::ceilf(sigma * 3.0).min(ceiling)).max(1.0) as usize;
	let mut weights = Vec::with_capacity(radius * 2 + 1);
	let mut total = 0.0f32;
	for index in 0..=radius * 2 {
		let distance = index as f32 - radius as f32;
		let weight = libm::expf(-(distance * distance) / (2.0 * sigma * sigma));
		weights.push(weight);
		total += weight;
	}
	if total > 0.0 {
		for weight in weights.iter_mut() {
			*weight /= total;
		}
	}
	weights
}

fn sample_bilinear(from: &Surface, x: f32, y: f32) -> Rgba {
	let (x0, y0) = (libm::floorf(x) as i64, libm::floorf(y) as i64);
	let (fx, fy) = (x - x0 as f32, y - y0 as f32);
	let at = |x: i64, y: i64| -> Rgba {
		if x < 0 || y < 0 {
			return Rgba::TRANSPARENT;
		}
		from.get(x as u32, y as u32)
	};
	let top = at(x0, y0).scaled(1.0 - fx).plus(at(x0 + 1, y0).scaled(fx));
	let bottom = at(x0, y0 + 1).scaled(1.0 - fx).plus(at(x0 + 1, y0 + 1).scaled(fx));
	top.scaled(1.0 - fy).plus(bottom.scaled(fy))
}

/// A colour matrix, applied to UNPREMULTIPLIED linear colour - which is what the node's definition
/// says and is not what the surface holds, so the value is divided out and multiplied back.
fn colour_matrix(colour: Rgba, matrix: &[[f32; 5]; 4]) -> Rgba {
	let straight = if colour.alpha > 0.0 { [colour.red / colour.alpha, colour.green / colour.alpha, colour.blue / colour.alpha, colour.alpha] } else { [0.0, 0.0, 0.0, 0.0] };
	let mut out = [0.0f32; 4];
	for (row, weights) in matrix.iter().enumerate() {
		out[row] = weights[0] * straight[0] + weights[1] * straight[1] + weights[2] * straight[2] + weights[3] * straight[3] + weights[4];
	}
	let alpha = out[3].clamp(0.0, 1.0);
	Rgba::new(out[0].clamp(0.0, 1.0) * alpha, out[1].clamp(0.0, 1.0) * alpha, out[2].clamp(0.0, 1.0) * alpha, alpha)
}

/// A tap outside the surface is TRANSPARENT BLACK, which is the graph's stated edge rule - and the
/// reason a convolution of a shape does not smear its border outward the way a clamped edge would.
fn at(from: &Surface, x: i64, y: i64) -> Rgba {
	if x < 0 || y < 0 { Rgba::TRANSPARENT } else { from.get(x as u32, y as u32) }
}

/// Divide a convolution's weighted sum, add its bias, and keep the result a colour.
///
/// THE CLAMP IS WHAT MAKES A SHARPEN A PICTURE. An unclamped kernel produces channels above one and
/// below zero at every edge it sharpens, and a premultiplied colour whose channels exceed its alpha
/// composites as a colour nobody chose.
fn finish_channels(total: Rgba, divisor: f32, bias: f32) -> Rgba {
	let apply = |value: f32| (value / divisor + bias).clamp(0.0, 1.0);
	let alpha = apply(total.alpha);
	Rgba::new(apply(total.red).min(alpha), apply(total.green).min(alpha), apply(total.blue).min(alpha), alpha)
}

/// The per-channel maximum or minimum over a RECTANGULAR structuring element.
///
/// RECTANGULAR BECAUSE IT IS SEPARABLE and a disc is not: two passes of `2r` taps rather than one of
/// `4r^2`. What that costs is the corner - a dilated square has square corners where a disc would
/// round them - and at the radii a UI uses, thickening text or fattening an outline, that is the
/// whole of the difference.
fn morphology(from: &Surface, into: &mut Surface, bounds: PixelRect, radius_x: f32, radius_y: f32, dilate: bool) {
	let reach_x = radius_x.abs() as i64;
	let reach_y = radius_y.abs() as i64;
	let pick = |left: f32, right: f32| if dilate { left.max(right) } else { left.min(right) };
	for y in bounds.y..bounds.y.saturating_add(bounds.height) {
		for x in bounds.x..bounds.x.saturating_add(bounds.width) {
			// THE ELEMENT STARTS AT THE PIXEL ITSELF and not at transparent black: an erosion seeded
			// with zero erodes everything, and a dilation seeded with one fills the surface.
			let mut value = from.get(x, y);
			for tap_y in -reach_y..=reach_y {
				for tap_x in -reach_x..=reach_x {
					let tap = at(from, x as i64 + tap_x, y as i64 + tap_y);
					value = Rgba::new(pick(value.red, tap.red), pick(value.green, tap.green), pick(value.blue, tap.blue), pick(value.alpha, tap.alpha));
				}
			}
			into.set(x, y, value);
		}
	}
}

/// A premultiplied colour as a straight one, which is how a displacement map's channels are read.
fn straight(colour: Rgba) -> Rgba {
	if colour.alpha > 0.0 { Rgba::new(colour.red / colour.alpha, colour.green / colour.alpha, colour.blue / colour.alpha, colour.alpha) } else { Rgba::TRANSPARENT }
}

fn channel_of(colour: Rgba, channel: render2d::filter::Channel) -> f32 {
	match channel {
		render2d::filter::Channel::Red => colour.red,
		render2d::filter::Channel::Green => colour.green,
		render2d::filter::Channel::Blue => colour.blue,
		render2d::filter::Channel::Alpha => colour.alpha,
	}
}

/// A rectangle in device space as the half-open pixel range it covers: left, top, right, bottom.
fn pixel_rect(rect: graphics_core::geom::RectF) -> (u32, u32, u32, u32) {
	let clamp = |value: f32| -> u32 { if !value.is_finite() || value <= 0.0 { 0 } else { value as u32 } };
	(clamp(rect.x), clamp(rect.y), clamp(rect.right()), clamp(rect.bottom()))
}

/// A non-negative remainder, which is what a tile needs and what `%` does not give for a negative.
fn wrap(value: i64, period: u32) -> u32 {
	let period = period as i64;
	(((value % period) + period) % period) as u32
}
