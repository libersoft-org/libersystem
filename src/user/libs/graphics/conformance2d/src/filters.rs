//! FILTERS: a bounded GRAPH, which is why the scenes below are one node each plus the two that are
//! only meaningful as a composition - a drop shadow and a backdrop filter.
//!
//! THE THREE THAT READ PAST THE PIXEL THEY WRITE ARE DRAWN ACROSS TILE BOUNDARIES AND COMPARED WHOLE: a
//! blur, a shadow and a backdrop, each on a target of several tiles, every pixel against the graph
//! evaluated at f64 within the frozen kernel tolerance - and exactly where the drawing covers a pixel
//! wholly or not at all. A backend that computes a filter one tile at a time and blurs against nothing at
//! the tile's edge is wrong by half the drawing's alpha there, and only a scene larger than a tile, read
//! everywhere, can see it.
//!
//! AND EVERY SCENE WHOSE PICTURE IS WHOLE PIXELS IS COMPARED WHOLE TOO: a colour matrix, a composite and a
//! blend inside the graph, both morphologies, a displacement by a whole number of pixels, a crop and a tile
//! each leave every pixel of the target wholly covered or empty, so every one is compared exactly. The
//! convolution alone is read at probes: its kernel's weights cancel, so its output leaves the range a colour
//! can hold and its picture depends on what a backend makes of that, which the profile does not state.

use crate::Outcome;
use crate::harness::{Frame, WIDE, blurred, byte, draw, draw_linear, every_pixel, gaussian, rect_path, solid, stored, white};
use graphics_core::geom::RectF;
use render2d::blend::{BlendMode, Operator};
use render2d::filter::{Channel, FilterGraph, FilterNode};
use render2d::path::FillRule;

/// An eight-pixel opaque square at (12,12), inside a layer carrying the graph.
fn filtered(build: impl Fn(&mut FilterGraph)) -> Result<Frame, crate::Trouble> {
	let mut graph = FilterGraph::default();
	build(&mut graph);
	draw(32, 32, |canvas| {
		let handle = canvas.resources().add_filter(graph)?;
		canvas.begin_layer(None, 1.0, BlendMode::Normal, Some(handle))?;
		canvas.fill_path(rect_path(RectF::new(12.0, 12.0, 8.0, 8.0)), white(), FillRule::NonZero)?;
		canvas.end_layer()
	})
}

/// Whether `at` lies in the half-open span `from..to`, as a coverage of nothing or everything.
fn inside(at: i64, from: i64, to: i64) -> f64 {
	if (from..to).contains(&at) { 1.0 } else { 0.0 }
}

/// EVERY PIXEL OF A PICTURE OF WHOLE PIXELS: an opaque straight colour - or nothing - inside the half-open
/// rectangle `left, top, right, bottom`, and another outside it, compared exactly at every pixel of the target.
fn whole(frame: &Frame, (left, top, right, bottom): (u32, u32, u32, u32), within: Option<[f32; 3]>, outside: Option<[f32; 3]>) -> Outcome {
	every_pixel(frame, |x, y| {
		let shown = if (left..right).contains(&x) && (top..bottom).contains(&y) { within } else { outside };
		match shown {
			Some(colour) => [stored(colour[0], x, y), stored(colour[1], x, y), stored(colour[2], x, y), byte(1.0)],
			None => [0; 4],
		}
	})
}

// @covers: FilterGaussianBlur
/// A blur IS THE SEPARABLE GAUSSIAN, AT EVERY PIXEL. An opaque white rectangle whose edges are whole pixels,
/// blurred by a different deviation on each axis, across the tile boundaries at 64 and 128 - and every
/// pixel's four channels compared with the product of the two one-dimensional Gaussian sums at f64. The
/// middle stays at one, the edges fall off as the kernel says on both sides of every boundary, and nothing
/// is left where the kernel does not reach.
pub fn filter_gaussian_blur() -> Outcome {
	let (left, top, right, bottom) = (36i64, 44i64, 136i64, 136i64);
	let (across, down) = (3.0f32, 2.0f32);
	let frame = draw_linear(WIDE, WIDE, |canvas| {
		let mut graph = FilterGraph::default();
		let source = graph.push(FilterNode::Source)?;
		graph.push(FilterNode::Blur { input: source, x: across, y: down })?;
		let handle = canvas.resources().add_filter(graph)?;
		canvas.begin_layer(None, 1.0, BlendMode::Normal, Some(handle))?;
		canvas.fill_path(rect_path(RectF::new(left as f32, top as f32, (right - left) as f32, (bottom - top) as f32)), white(), FillRule::NonZero)?;
		canvas.end_layer()
	})?;
	let (horizontal, vertical) = (gaussian(across as f64), gaussian(down as f64));
	frame.compare(|x, y| {
		let alpha = blurred(&horizontal, x as i64, |at| inside(at, left, right)) * blurred(&vertical, y as i64, |at| inside(at, top, bottom));
		([alpha; 4], false)
	})
}

// @covers: FilterDropShadow
/// A DROP SHADOW IS A COMPOSITION AND NOT A NODE - blur, offset, flood, keep-inside, composite under -
/// which is the whole reason the profile requires a graph: an API with a `drop_shadow` call has one
/// shadow and no way to make another.
///
/// AND IT IS THAT COMPOSITION AT EVERY PIXEL, across tile boundaries: where the white rectangle is, the
/// rectangle exactly; everywhere else the blue flood at the alpha of the rectangle blurred and moved five
/// pixels down and right, which is the Gaussian at f64 - so a shadow cut off where a tile ends, or missing
/// where the shape that cast it lies in the tile beside it, is a pixel that differs.
pub fn filter_drop_shadow() -> Outcome {
	let (left, top, right, bottom) = (40i64, 36i64, 116i64, 124i64);
	let (dx, dy) = (5i64, 5i64);
	let frame = draw_linear(WIDE, WIDE, |canvas| {
		let mut graph = FilterGraph::default();
		let source = graph.push(FilterNode::Source)?;
		let blur = graph.push(FilterNode::Blur { input: source, x: 2.0, y: 2.0 })?;
		let offset = graph.push(FilterNode::Offset { input: blur, dx: dx as f32, dy: dy as f32 })?;
		let flood = graph.push(FilterNode::Flood { color: render2d::paint::Color::new(0.0, 0.0, 1.0, 1.0, graphics_core::ColorSpace::SrgbLinear) })?;
		let tint = graph.push(FilterNode::In { input: flood, mask: offset })?;
		graph.push(FilterNode::Composite { source, backdrop: tint, operator: Operator::SrcOver })?;
		let handle = canvas.resources().add_filter(graph)?;
		canvas.begin_layer(None, 1.0, BlendMode::Normal, Some(handle))?;
		canvas.fill_path(rect_path(RectF::new(left as f32, top as f32, (right - left) as f32, (bottom - top) as f32)), white(), FillRule::NonZero)?;
		canvas.end_layer()
	})?;
	let kernel = gaussian(2.0);
	frame.compare(|x, y| {
		let (x, y) = (x as i64, y as i64);
		if inside(x, left, right) * inside(y, top, bottom) > 0.0 {
			// The shape over its shadow is the shape: an opaque source leaves nothing of what is under it.
			return ([1.0; 4], true);
		}
		let shadow = blurred(&kernel, x - dx, |at| inside(at, left, right)) * blurred(&kernel, y - dy, |at| inside(at, top, bottom));
		([0.0, 0.0, shadow, shadow], false)
	})
}

// @covers: FilterColorMatrix
/// A colour matrix is applied to UNPREMULTIPLIED colour, so a matrix that moves red into blue moves
/// the colour and not the coverage.
pub fn filter_color_matrix() -> Outcome {
	let frame = draw(32, 32, |canvas| {
		let mut graph = FilterGraph::default();
		let source = graph.push(FilterNode::Source)?;
		let matrix = [[0.0, 0.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0, 0.0]];
		graph.push(FilterNode::ColorMatrix { input: source, matrix })?;
		let handle = canvas.resources().add_filter(graph)?;
		canvas.begin_layer(None, 1.0, BlendMode::Normal, Some(handle))?;
		canvas.fill_path(rect_path(RectF::new(12.0, 12.0, 8.0, 8.0)), solid(0.8, 0.0, 0.0, 1.0), FillRule::NonZero)?;
		canvas.end_layer()
	})?;
	// Red moved into blue, and the alpha row left the coverage alone - where the square is and nowhere else.
	whole(&frame, (12, 12, 20, 20), Some([0.0, 0.0, 0.8]), None)
}

// @covers: FilterComposite
/// Two inputs under a Porter-Duff operator, INSIDE the graph - which is what makes a shadow's tint
/// land under its caster rather than over it.
pub fn filter_composite() -> Outcome {
	let frame = filtered(|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		let flood = graph.push(FilterNode::Flood { color: render2d::paint::Color::new(0.0, 0.0, 1.0, 1.0, graphics_core::ColorSpace::SrgbLinear) }).expect("a flood");
		// THE FLOOD KEPT ONLY WHERE THE SOURCE IS NOT, which is `SourceOut` and is a shape's surround.
		graph.push(FilterNode::Composite { source: flood, backdrop: source, operator: Operator::SrcOut }).expect("a composite");
	})?;
	// Where the shape is the flood was cut away, and everywhere else it survives.
	whole(&frame, (12, 12, 20, 20), None, Some([0.0, 0.0, 1.0]))
}

// @covers: FilterBlend
/// Two inputs under a BLEND MODE, which is a different question from an operator: this is about the
/// colour where both are, not about which survives.
pub fn filter_blend() -> Outcome {
	let frame = draw(32, 32, |canvas| {
		let mut graph = FilterGraph::default();
		let source = graph.push(FilterNode::Source)?;
		let flood = graph.push(FilterNode::Flood { color: render2d::paint::Color::new(0.5, 0.5, 0.5, 1.0, graphics_core::ColorSpace::SrgbLinear) })?;
		graph.push(FilterNode::Blend { source: flood, backdrop: source, mode: BlendMode::Multiply })?;
		let handle = canvas.resources().add_filter(graph)?;
		canvas.begin_layer(None, 1.0, BlendMode::Normal, Some(handle))?;
		canvas.fill_path(rect_path(RectF::new(12.0, 12.0, 8.0, 8.0)), solid(0.8, 0.8, 0.8, 1.0), FillRule::NonZero)?;
		canvas.end_layer()
	})?;
	// Half multiplied by four fifths is two fifths where the square is; everywhere else the backdrop is
	// nothing, and the flood over nothing is the flood.
	whole(&frame, (12, 12, 20, 20), Some([0.4, 0.4, 0.4]), Some([0.5, 0.5, 0.5]))
}

// @covers: FilterConvolution
/// A three by three kernel whose weights CANCEL is zero where the picture is flat and not zero at an
/// edge - a property of the kernel rather than of the implementation, which is what makes it a
/// conformance check rather than a comparison.
pub fn filter_convolution() -> Outcome {
	let frame = filtered(|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		let weights = [[0.0, -1.0, 0.0], [-1.0, 4.0, -1.0], [0.0, -1.0, 0.0]];
		graph.push(FilterNode::Convolution { input: source, weights, divisor: 0.0, bias: 0.0 }).expect("a convolution");
	})?;
	require!(frame.empty(16, 16), "the flat inside of the square has no edges in it: {:?}", frame.pixel(16, 16));
	require!(frame.alpha(12, 16) > 0, "and its left edge does: {:?}", frame.pixel(12, 16));
	Ok(())
}

// @covers: FilterMorphologyDilate
/// Dilation is the per-channel MAXIMUM over the structuring element, so the shape grows by exactly
/// the radius.
pub fn filter_morphology_dilate() -> Outcome {
	let frame = filtered(|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		graph.push(FilterNode::MorphologyDilate { input: source, x: 2.0, y: 2.0 }).expect("a dilation");
	})?;
	// Two pixels out is now inside the shape and three is not: it grew by the radius on every side, not more.
	whole(&frame, (10, 10, 22, 22), Some([1.0; 3]), None)
}

// @covers: FilterMorphologyErode
/// Erosion is the MINIMUM over the same element, so the shape shrinks by the radius - and the two
/// together are how an outline is made.
pub fn filter_morphology_erode() -> Outcome {
	let frame = filtered(|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		graph.push(FilterNode::MorphologyErode { input: source, x: 2.0, y: 2.0 }).expect("an erosion");
	})?;
	// The two pixels inside every edge are eroded away and the middle survives.
	whole(&frame, (14, 14, 18, 18), Some([1.0; 3]), None)
}

// @covers: FilterDisplacementMap
/// `scale * (channel - 0.5)`: a map of flat half-grey moves nothing, and a map at one moves by half
/// the scale. The half is what makes a map composable with a gradient or a rendered shape.
pub fn filter_displacement_map() -> Outcome {
	let still = filtered(|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		let map = graph.push(FilterNode::Flood { color: render2d::paint::Color::new(0.5, 0.5, 0.5, 1.0, graphics_core::ColorSpace::SrgbLinear) }).expect("a flat map");
		graph.push(FilterNode::DisplacementMap { input: source, map, scale: 8.0, x_channel: Channel::Red, y_channel: Channel::Green }).expect("a displacement");
	})?;
	// A flat half-grey map is the identity.
	whole(&still, (12, 12, 20, 20), Some([1.0; 3]), None)?;
	let moved = filtered(|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		let map = graph.push(FilterNode::Flood { color: render2d::paint::Color::new(1.0, 0.5, 0.5, 1.0, graphics_core::ColorSpace::SrgbLinear) }).expect("a map");
		graph.push(FilterNode::DisplacementMap { input: source, map, scale: 8.0, x_channel: Channel::Red, y_channel: Channel::Green }).expect("a displacement");
	})?;
	// A map at one reads four pixels to the right, so the shape appears four to the left, far edge and all.
	whole(&moved, (8, 12, 16, 20), Some([1.0; 3]), None)
}

// @covers: FilterCrop
/// A crop is the input inside a rectangle and transparent black outside it.
pub fn filter_crop() -> Outcome {
	let frame = filtered(|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		graph.push(FilterNode::Crop { input: source, rect: RectF::new(12.0, 12.0, 4.0, 8.0) }).expect("a crop");
	})?;
	// Inside the crop the shape is there, and outside it there is nothing.
	whole(&frame, (12, 12, 16, 20), Some([1.0; 3]), None)
}

// @covers: FilterTile
/// A tile repeats one rectangle over the whole output, wrapped about the RECTANGLE'S own origin - so
/// the tile that lands on the rectangle is the rectangle.
pub fn filter_tile() -> Outcome {
	let frame = filtered(|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		graph.push(FilterNode::Tile { input: source, rect: RectF::new(12.0, 12.0, 8.0, 8.0) }).expect("a tile");
	})?;
	// The rectangle is the square's own inside, so its repetition - a period along, and in the negative
	// direction too - covers the whole target.
	whole(&frame, (0, 0, 32, 32), Some([1.0; 3]), None)
}

// @covers: FilterBackdrop
/// A BACKDROP FILTER READS WHAT IS UNDER THE LAYER - which is what a frosted panel is, and without it
/// the whole class of them has to be built by drawing the scene twice.
///
/// UNDER IT, AND NOT UNDER ONE TILE OF IT. Blue on the left of the target and red on the right, and a panel
/// across both and across the tile boundaries whose graph is the backdrop blurred: inside the panel every
/// pixel is that blur - of the backdrop inside the panel's bounds, which is what the layer is - composited
/// over the backdrop, at f64; outside it every pixel is the backdrop, exactly. A panel that blurred each
/// tile's own part of the scene against nothing has a seam at every tile boundary it crosses.
pub fn filter_backdrop() -> Outcome {
	let split = 80i64;
	let (left, top, right, bottom) = (24i64, 24i64, 136i64, 136i64);
	let frame = draw_linear(WIDE, WIDE, |canvas| {
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, split as f32, WIDE as f32)), solid(0.0, 0.0, 1.0, 1.0), FillRule::NonZero)?;
		canvas.fill_path(rect_path(RectF::new(split as f32, 0.0, (WIDE as i64 - split) as f32, WIDE as f32)), solid(1.0, 0.0, 0.0, 1.0), FillRule::NonZero)?;
		let mut graph = FilterGraph::default();
		let backdrop = graph.push(FilterNode::Backdrop)?;
		graph.push(FilterNode::Blur { input: backdrop, x: 3.0, y: 3.0 })?;
		let handle = canvas.resources().add_filter(graph)?;
		canvas.begin_layer(Some(RectF::new(left as f32, top as f32, (right - left) as f32, (bottom - top) as f32)), 1.0, BlendMode::Normal, Some(handle))?;
		canvas.fill_path(rect_path(RectF::new(left as f32, top as f32, (right - left) as f32, (bottom - top) as f32)), solid(1.0, 1.0, 1.0, 0.1), FillRule::NonZero)?;
		canvas.end_layer()
	})?;
	let kernel = gaussian(3.0);
	// THE BACKDROP, premultiplied: opaque blue left of the split and opaque red from it.
	let under = |x: i64| if x < split { [0.0, 0.0, 1.0, 1.0] } else { [1.0, 0.0, 0.0, 1.0] };
	frame.compare(|x, y| {
		let (x, y) = (x as i64, y as i64);
		let backdrop = under(x);
		if inside(x, left, right) * inside(y, top, bottom) == 0.0 {
			return (backdrop, true);
		}
		// The backdrop inside the panel's bounds, blurred: it varies only across, so the blur is a sum across
		// times a sum down, and outside the bounds there is nothing for it to read.
		let down = blurred(&kernel, y, |at| inside(at, top, bottom));
		let blur: [f64; 4] = core::array::from_fn(|channel| blurred(&kernel, x, |at| inside(at, left, right) * under(at)[channel]) * down);
		// And composited over the backdrop at full opacity.
		(core::array::from_fn(|channel| blur[channel] + (1.0 - blur[3]) * backdrop[channel]), false)
	})
}
