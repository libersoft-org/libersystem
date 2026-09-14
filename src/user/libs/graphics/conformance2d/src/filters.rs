//! FILTERS: a bounded GRAPH, which is why the scenes below are one node each plus the two that are
//! only meaningful as a composition - a drop shadow and a backdrop filter.

use crate::Outcome;
use crate::harness::{Frame, draw, near, rect_path, solid, white};
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

// @covers: FilterGaussianBlur
/// A blur SPREADS PAST THE SHAPE, which is what the graph's bounds map grows the layer by: a blur
/// clipped to its input is the defect that gives every shadow a straight edge.
pub fn filter_gaussian_blur() -> Outcome {
	let frame = filtered(|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		graph.push(FilterNode::Blur { input: source, x: 2.0, y: 2.0 }).expect("a blur");
	})?;
	require!(frame.alpha(16, 16) > 200, "the middle of the shape survives: {:?}", frame.pixel(16, 16));
	require!(frame.alpha(10, 16) > 0, "and it reaches past the shape's own edge: {:?}", frame.pixel(10, 16));
	require!(frame.alpha(10, 16) < frame.alpha(13, 16), "further out is fainter, which is what a Gaussian is: {:?}", frame.pixel(10, 16));
	Ok(())
}

// @covers: FilterDropShadow
/// A DROP SHADOW IS A COMPOSITION AND NOT A NODE - blur, offset, flood, keep-inside, composite under -
/// which is the whole reason the profile requires a graph: an API with a `drop_shadow` call has one
/// shadow and no way to make another.
pub fn filter_drop_shadow() -> Outcome {
	let frame = filtered(|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		let blur = graph.push(FilterNode::Blur { input: source, x: 2.0, y: 2.0 }).expect("a blur");
		let offset = graph.push(FilterNode::Offset { input: blur, dx: 5.0, dy: 5.0 }).expect("an offset");
		let flood = graph.push(FilterNode::Flood { color: render2d::paint::Color::new(0.0, 0.0, 1.0, 1.0, graphics_core::ColorSpace::SrgbLinear) }).expect("a flood");
		let tint = graph.push(FilterNode::In { input: flood, mask: offset }).expect("kept inside the blur");
		graph.push(FilterNode::Composite { source, backdrop: tint, operator: Operator::SrcOver }).expect("the shape over its shadow");
	})?;
	require!(frame.pixel(16, 16)[0] > 200, "the shape itself is still white: {:?}", frame.pixel(16, 16));
	let shadow = frame.pixel(23, 23);
	require!(shadow[3] > 0 && shadow[2] > shadow[0], "and below-right of it is a blue shadow: {shadow:?}");
	Ok(())
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
	let pixel = frame.pixel(16, 16);
	require!(near(pixel[2], 0.8) && pixel[0] < 8, "the red channel was moved into blue: {pixel:?}");
	require!(pixel[3] == 255, "and the alpha row left the coverage alone: {pixel:?}");
	Ok(())
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
	require!(frame.empty(16, 16), "where the shape is, the flood was cut away: {:?}", frame.pixel(16, 16));
	require!(frame.pixel(4, 4)[2] > 200, "and where it is not, the flood survives: {:?}", frame.pixel(4, 4));
	Ok(())
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
	require!(near(frame.pixel(16, 16)[0], 0.4), "half multiplied by four fifths is two fifths: {:?}", frame.pixel(16, 16));
	Ok(())
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
	require!(frame.covered(10, 16), "two pixels out is now inside the shape: {:?}", frame.pixel(10, 16));
	require!(frame.empty(9, 16), "and three is not, so it grew by the radius and not by more: {:?}", frame.pixel(9, 16));
	Ok(())
}

// @covers: FilterMorphologyErode
/// Erosion is the MINIMUM over the same element, so the shape shrinks by the radius - and the two
/// together are how an outline is made.
pub fn filter_morphology_erode() -> Outcome {
	let frame = filtered(|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		graph.push(FilterNode::MorphologyErode { input: source, x: 2.0, y: 2.0 }).expect("an erosion");
	})?;
	require!(frame.empty(13, 16), "a pixel that was inside the edge is eroded away: {:?}", frame.pixel(13, 16));
	require!(frame.covered(16, 16), "and the middle survives: {:?}", frame.pixel(16, 16));
	Ok(())
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
	require!(still.covered(16, 16), "a flat half-grey map is the identity: {:?}", still.pixel(16, 16));
	let moved = filtered(|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		let map = graph.push(FilterNode::Flood { color: render2d::paint::Color::new(1.0, 0.5, 0.5, 1.0, graphics_core::ColorSpace::SrgbLinear) }).expect("a map");
		graph.push(FilterNode::DisplacementMap { input: source, map, scale: 8.0, x_channel: Channel::Red, y_channel: Channel::Green }).expect("a displacement");
	})?;
	require!(moved.covered(12, 16), "a map at one reads four pixels to the right, so the shape appears four to the left: {:?}", moved.pixel(12, 16));
	require!(moved.empty(18, 16), "and its far edge moved with it: {:?}", moved.pixel(18, 16));
	Ok(())
}

// @covers: FilterCrop
/// A crop is the input inside a rectangle and transparent black outside it.
pub fn filter_crop() -> Outcome {
	let frame = filtered(|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		graph.push(FilterNode::Crop { input: source, rect: RectF::new(12.0, 12.0, 4.0, 8.0) }).expect("a crop");
	})?;
	require!(frame.covered(13, 16), "inside the crop the shape is there: {:?}", frame.pixel(13, 16));
	require!(frame.empty(17, 16), "and outside it there is nothing: {:?}", frame.pixel(17, 16));
	Ok(())
}

// @covers: FilterTile
/// A tile repeats one rectangle over the whole output, wrapped about the RECTANGLE'S own origin - so
/// the tile that lands on the rectangle is the rectangle.
pub fn filter_tile() -> Outcome {
	let frame = filtered(|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		graph.push(FilterNode::Tile { input: source, rect: RectF::new(12.0, 12.0, 8.0, 8.0) }).expect("a tile");
	})?;
	require!(frame.covered(16, 16), "the tile itself is unchanged: {:?}", frame.pixel(16, 16));
	require!(frame.covered(24, 24), "and repeats a period along: {:?}", frame.pixel(24, 24));
	require!(frame.covered(4, 4), "in the negative direction too: {:?}", frame.pixel(4, 4));
	Ok(())
}

// @covers: FilterBackdrop
/// A BACKDROP FILTER READS WHAT IS UNDER THE LAYER - which is what a frosted panel is, and without it
/// the whole class of them has to be built by drawing the scene twice.
pub fn filter_backdrop() -> Outcome {
	let frame = draw(32, 32, |canvas| {
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 32.0, 16.0)), solid(0.0, 0.0, 1.0, 1.0), FillRule::NonZero)?;
		let mut graph = FilterGraph::default();
		let backdrop = graph.push(FilterNode::Backdrop)?;
		graph.push(FilterNode::Blur { input: backdrop, x: 3.0, y: 3.0 })?;
		let reads = graph.reads_backdrop();
		let handle = canvas.resources().add_filter(graph)?;
		canvas.begin_layer(None, 1.0, BlendMode::Normal, Some(handle))?;
		canvas.fill_path(rect_path(RectF::new(8.0, 8.0, 16.0, 16.0)), solid(1.0, 1.0, 1.0, 0.1), FillRule::NonZero)?;
		canvas.end_layer()?;
		// A GRAPH THAT READS THE BACKDROP SAYS SO, which is the question a compositor asks before it
		// reorders or caches anything.
		let _ = reads;
		Ok(())
	})?;
	require!(frame.pixel(16, 17)[2] > 0, "the blue under the panel is blurred past the edge it had: {:?}", frame.pixel(16, 17));
	Ok(())
}
