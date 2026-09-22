use super::*;

use graphics_core::geom::{Extent2D, PointF, RectF};
use graphics_core::layout::{ImageLayout, RowOrigin};
use graphics_core::pixel::OutputLuminance;
use graphics_core::semantics::ImageSemantics;
use graphics_core::{AlphaMode, ColorSpace, OwnedImage, PixelFormat, PixelStorage};
use render2d::backend::{Backend, TargetDescription};
use render2d::blend::{Antialias, BlendMode, Operator};
use render2d::paint::{Color, Paint};
use render2d::path::{FillRule, PathBuilder};
use render2d::{Canvas, DrawList};

/// A target in the commonest surface format: eight-bit sRGB with straight alpha.
fn target(width: u32, height: u32) -> OwnedImage {
	let semantics = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Straight };
	let storage = PixelStorage::Known(PixelFormat::R8G8B8A8Unorm);
	let layout = ImageLayout::new(Extent2D::new(width, height), width * 4, storage, RowOrigin::TopLeft, semantics).expect("a layout");
	OwnedImage::new(layout).expect("an image")
}

fn description(image: &OwnedImage) -> TargetDescription {
	TargetDescription { extent: image.layout().extent, format: PixelFormat::R8G8B8A8Unorm, color_space: ColorSpace::Srgb, scale: 1.0, luminance: OutputLuminance::UNKNOWN }
}

/// The stored bytes of one pixel, which is what a fixture states its expectations in.
fn pixel(image: &OwnedImage, x: u32, y: u32) -> [u8; 4] {
	let view = image.view();
	let row = view.row(y).expect("a row");
	let start = x as usize * 4;
	[row[start], row[start + 1], row[start + 2], row[start + 3]]
}

fn red() -> Paint {
	Paint::Solid(Color::new(1.0, 0.0, 0.0, 1.0, ColorSpace::Srgb))
}

fn draw(list: &DrawList, image: &mut OwnedImage) {
	let mut backend = Soft2d::new();
	let description = description(image);
	let prepared = backend.prepare(list, &description).expect("a preparation");
	let mut view = image.view_mut();
	backend.render(&prepared, &mut view).expect("a frame");
}

fn rect_path(rect: RectF) -> render2d::path::Path {
	let mut builder = PathBuilder::new();
	builder.add_rect(rect).expect("a rectangle");
	builder.finish()
}

#[test]
// THE FIRST THING A RASTERISER HAS TO GET RIGHT IS A RECTANGLE. `Rect` is half-open, a fill covers
// every sample point inside it, and a zero-width rectangle draws nothing and is not an error.
fn a_filled_rectangle_is_exact_and_half_open() {
	let mut image = target(8, 8);
	let mut canvas = Canvas::new();
	canvas.fill_path(rect_path(RectF::new(2.0, 2.0, 4.0, 4.0)), red(), FillRule::NonZero).expect("a fill");
	draw(&canvas.finish().expect("a list"), &mut image);

	assert_eq!(pixel(&image, 3, 3), [255, 0, 0, 255], "inside is the paint");
	assert_eq!(pixel(&image, 0, 0), [0, 0, 0, 0], "outside is untouched");
	// HALF-OPEN: the pixel at the right edge is OUTSIDE a rectangle that ends there.
	assert_eq!(pixel(&image, 5, 3), [255, 0, 0, 255], "the last covered pixel");
	assert_eq!(pixel(&image, 6, 3), [0, 0, 0, 0], "and the first one past it is not covered");

	// A ZERO-WIDTH RECTANGLE DRAWS NOTHING AND IS NOT AN ERROR.
	let mut empty = target(4, 4);
	let mut canvas = Canvas::new();
	canvas.fill_path(rect_path(RectF::new(1.0, 1.0, 0.0, 2.0)), red(), FillRule::NonZero).expect("a fill");
	draw(&canvas.finish().expect("a list"), &mut empty);
	assert_eq!(pixel(&empty, 1, 1), [0, 0, 0, 0]);
}

#[test]
// COVERAGE ALONG A KNOWN EDGE. A rectangle covering half of a column of pixels must composite at half
// alpha - not at none, not at all of it, and not at a value that depends on which sub-scanline the
// edge happened to land on.
fn coverage_is_analytic_along_a_known_edge() {
	let mut image = target(8, 4);
	let mut canvas = Canvas::new();
	// From 2.5 to 6.0: the pixel at x = 2 is half covered, and 3 to 5 are whole.
	canvas.fill_path(rect_path(RectF::new(2.5, 0.0, 3.5, 4.0)), red(), FillRule::NonZero).expect("a fill");
	draw(&canvas.finish().expect("a list"), &mut image);

	let half = pixel(&image, 2, 1);
	assert!((half[3] as i32 - 128).abs() <= 2, "a half-covered pixel is half-covered: {half:?}");
	assert_eq!(pixel(&image, 3, 1)[3], 255);
	assert_eq!(pixel(&image, 6, 1)[3], 0);

	// AND THE ALIASED PATH IS A PIXEL IN OR OUT, which a diagram's grid and a screenshot comparison
	// both need. The pixel whose centre the edge passes is decided by its centre.
	let mut aliased = target(8, 4);
	let mut canvas = Canvas::new();
	canvas.set_antialias(Antialias::Off);
	canvas.fill_path(rect_path(RectF::new(2.5, 0.0, 3.5, 4.0)), red(), FillRule::NonZero).expect("a fill");
	draw(&canvas.finish().expect("a list"), &mut aliased);
	for x in 0..8 {
		let alpha = pixel(&aliased, x, 1)[3];
		assert!(alpha == 0 || alpha == 255, "an aliased edge has no partial coverage: {x} is {alpha}");
	}
}

#[test]
// THE TWO FILL RULES ARE DIFFERENT SHAPES, and a backend that implements one of them has implemented
// half of the profile: a ring drawn as two rectangles wound the same way is solid under non-zero and
// hollow under even-odd.
fn both_fill_rules_are_implemented_and_differ() {
	let mut builder = PathBuilder::new();
	builder.add_rect(RectF::new(0.0, 0.0, 8.0, 8.0)).expect("the outside");
	builder.add_rect(RectF::new(2.0, 2.0, 4.0, 4.0)).expect("the inside, wound the same way");
	let path = builder.finish();

	let mut solid = target(8, 8);
	let mut canvas = Canvas::new();
	canvas.fill_path(path.clone(), red(), FillRule::NonZero).expect("a fill");
	draw(&canvas.finish().expect("a list"), &mut solid);
	assert_eq!(pixel(&solid, 4, 4)[3], 255, "under non-zero the middle is filled");

	let mut hollow = target(8, 8);
	let mut canvas = Canvas::new();
	canvas.fill_path(path, red(), FillRule::EvenOdd).expect("a fill");
	draw(&canvas.finish().expect("a list"), &mut hollow);
	assert_eq!(pixel(&hollow, 4, 4)[3], 0, "under even-odd it is a hole");
	assert_eq!(pixel(&hollow, 1, 4)[3], 255, "and the ring around it is filled");
}

#[test]
// A STROKE IS A FILL OF ITS OUTLINE, and the caps and joins are what make the outline. A butt cap
// stops at the endpoint, a square cap reaches half a width past it, and a round one reaches the same
// distance with a curve - so the three differ at exactly the pixels past the end.
fn caps_and_joins_reach_where_they_should() {
	let line = || {
		let mut builder = PathBuilder::new();
		builder.move_to(PointF { x: 4.0, y: 8.0 }).expect("a start");
		builder.line_to(PointF { x: 12.0, y: 8.0 }).expect("a line");
		builder.finish()
	};
	let stroke = |cap: render2d::path::Cap| {
		let mut image = target(20, 16);
		let mut canvas = Canvas::new();
		let style = render2d::path::StrokeStyle { width: 4.0, cap, ..render2d::path::StrokeStyle::default() };
		canvas.stroke_path(line(), red(), style).expect("a stroke");
		draw(&canvas.finish().expect("a list"), &mut image);
		image
	};
	let butt = stroke(render2d::path::Cap::Butt);
	let square = stroke(render2d::path::Cap::Square);
	let round = stroke(render2d::path::Cap::Round);

	// The stroke's body is drawn by all three.
	for image in [&butt, &square, &round] {
		assert_eq!(pixel(image, 8, 8)[3], 255, "the body of the stroke");
		assert_eq!(pixel(image, 8, 4)[3], 0, "and not two widths away");
	}
	// PAST THE ENDPOINT is where they differ: a butt cap stops, the other two reach.
	assert_eq!(pixel(&butt, 13, 8)[3], 0, "a butt cap stops at the endpoint");
	assert_eq!(pixel(&square, 13, 8)[3], 255, "a square cap reaches half a width past it");
	// A ROUND CAP REACHES THE SAME DISTANCE ON THE AXIS, and the pixel there is partly outside the
	// circle - which is the coverage a round cap is for.
	assert!(pixel(&round, 13, 8)[3] > 128, "a round cap reaches past the endpoint on the axis: {:?}", pixel(&round, 13, 8));
	// A ROUND CAP IS ROUND: its corner is not filled, and a square cap's is.
	assert_eq!(pixel(&square, 13, 6)[3], 255, "the square cap's corner");
	assert!(pixel(&round, 13, 6)[3] < 128, "the round cap's corner is outside the circle: {:?}", pixel(&round, 13, 6));

	// A MITER JOIN REACHES PAST A SHARP CORNER and a bevel does not, which is what the miter limit is
	// a limit on.
	let corner = || {
		let mut builder = PathBuilder::new();
		builder.move_to(PointF { x: 4.0, y: 14.0 }).expect("a start");
		builder.line_to(PointF { x: 10.0, y: 4.0 }).expect("up");
		builder.line_to(PointF { x: 16.0, y: 14.0 }).expect("down");
		builder.finish()
	};
	let joined = |join: render2d::path::Join| {
		let mut image = target(20, 20);
		let mut canvas = Canvas::new();
		let style = render2d::path::StrokeStyle { width: 3.0, join, miter_limit: 8.0, ..render2d::path::StrokeStyle::default() };
		canvas.stroke_path(corner(), red(), style).expect("a stroke");
		draw(&canvas.finish().expect("a list"), &mut image);
		image
	};
	let miter = joined(render2d::path::Join::Miter);
	let bevel = joined(render2d::path::Join::Bevel);
	let tip = pixel(&miter, 10, 2);
	assert!(tip[3] > pixel(&bevel, 10, 2)[3], "a miter reaches past the corner and a bevel cuts it off: {tip:?}");
}

#[test]
// A DASH PATTERN IS WALKED BY ARC LENGTH, so the gaps are where the pattern says and the phase moves
// them - which is what makes a marching-ants selection an animation of one number.
fn a_dash_pattern_leaves_the_gaps_it_states() {
	let mut canvas = Canvas::new();
	let pattern = canvas.resources().add_dashes(alloc::vec![4.0, 4.0]).expect("a pattern");
	let mut builder = PathBuilder::new();
	builder.move_to(PointF { x: 0.0, y: 4.0 }).expect("a start");
	builder.line_to(PointF { x: 16.0, y: 4.0 }).expect("a line");
	let style = render2d::path::StrokeStyle { width: 4.0, dash: Some(render2d::path::DashHandleRange { pattern, phase: 0.0 }), ..render2d::path::StrokeStyle::default() };
	canvas.stroke_path(builder.finish(), red(), style).expect("a stroke");
	let mut image = target(16, 8);
	draw(&canvas.finish().expect("a list"), &mut image);

	assert_eq!(pixel(&image, 1, 4)[3], 255, "the first dash is drawn");
	assert_eq!(pixel(&image, 6, 4)[3], 0, "the first gap is not");
	assert_eq!(pixel(&image, 9, 4)[3], 255, "and the second dash is");

	// A PATTERN WITH NO POSITIVE LENGTH IS REFUSED WHERE IT IS RECORDED, because a dasher walking it
	// never advances.
	let mut canvas = Canvas::new();
	assert_eq!(canvas.resources().add_dashes(alloc::vec![0.0, 0.0]).err(), Some(render2d::Error::DegenerateDash));
}

#[test]
// CLIPPING IS A COVERAGE MASK, so a rounded clip, a path clip and a nested pair cost the same
// machinery as a rectangle - and nesting is the PRODUCT of the masks, which is what makes two
// antialiased edges crossing at a corner let through the product of their coverages.
fn clips_nest_intersect_and_invert() {
	let mut image = target(16, 16);
	let mut canvas = Canvas::new();
	canvas.save().expect("a state");
	canvas.set_clip(rect_path(RectF::new(0.0, 0.0, 8.0, 16.0)), FillRule::NonZero).expect("the first clip");
	canvas.set_clip(rect_path(RectF::new(4.0, 0.0, 12.0, 16.0)), FillRule::NonZero).expect("the second");
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 16.0, 16.0)), red(), FillRule::NonZero).expect("a fill");
	canvas.restore().expect("both clips popped");
	draw(&canvas.finish().expect("a list"), &mut image);

	assert_eq!(pixel(&image, 5, 8)[3], 255, "the intersection of the two clips is drawn");
	assert_eq!(pixel(&image, 2, 8)[3], 0, "outside the second clip is not");
	assert_eq!(pixel(&image, 12, 8)[3], 0, "and neither is outside the first: clips INTERSECT");

	// AN INVERSE CLIP KEEPS EVERYTHING THE SHAPE MISSES, which is what a knockout and a spotlight are.
	let mut knockout = target(16, 16);
	let mut canvas = Canvas::new();
	canvas.save().expect("a state");
	canvas.set_clip_inverse(rect_path(RectF::new(4.0, 4.0, 8.0, 8.0)), FillRule::NonZero).expect("an inverse clip");
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 16.0, 16.0)), red(), FillRule::NonZero).expect("a fill");
	canvas.restore().expect("the clip popped");
	draw(&canvas.finish().expect("a list"), &mut knockout);
	assert_eq!(pixel(&knockout, 8, 8)[3], 0, "inside the inverse clip's shape nothing is drawn");
	assert_eq!(pixel(&knockout, 1, 1)[3], 255, "and outside it everything is");

	// A ROUNDED CLIP IS THE SAME MACHINERY: its corner is partially covered rather than square.
	let mut rounded = target(16, 16);
	let mut builder = PathBuilder::new();
	builder.move_to(PointF { x: 8.0, y: 0.0 }).expect("a start");
	builder.cubic_to(PointF { x: 16.0, y: 0.0 }, PointF { x: 16.0, y: 16.0 }, PointF { x: 8.0, y: 16.0 }).expect("a curve");
	builder.cubic_to(PointF { x: 0.0, y: 16.0 }, PointF { x: 0.0, y: 0.0 }, PointF { x: 8.0, y: 0.0 }).expect("and back");
	builder.close().expect("closed");
	let mut canvas = Canvas::new();
	canvas.save().expect("a state");
	canvas.set_clip(builder.finish(), FillRule::NonZero).expect("a rounded clip");
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 16.0, 16.0)), red(), FillRule::NonZero).expect("a fill");
	canvas.restore().expect("the clip popped");
	draw(&canvas.finish().expect("a list"), &mut rounded);
	assert_eq!(pixel(&rounded, 8, 8)[3], 255, "the middle of a rounded clip is open");
	assert_eq!(pixel(&rounded, 0, 0)[3], 0, "and its corner is not");
}

#[test]
// GROUP OPACITY CANNOT BE DONE BY MULTIPLYING EACH DRAWING'S ALPHA. Two overlapping shapes at half
// opacity each show the seam between them; the same two shapes in a layer at half opacity do not -
// and the reference value is worked out by hand here rather than taken from the implementation.
fn a_layer_composites_as_one_thing_at_its_opacity() {
	let overlapping = |layered: bool| {
		let mut image = target(16, 16);
		let mut canvas = Canvas::new();
		if layered {
			canvas.begin_layer(None, 0.5, BlendMode::Normal, None).expect("a layer");
		} else {
			canvas.set_opacity(0.5);
		}
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 10.0, 16.0)), red(), FillRule::NonZero).expect("one");
		canvas.fill_path(rect_path(RectF::new(6.0, 0.0, 10.0, 16.0)), red(), FillRule::NonZero).expect("the other");
		if layered {
			canvas.end_layer().expect("closed");
		}
		draw(&canvas.finish().expect("a list"), &mut image);
		image
	};
	let separate = overlapping(false);
	let grouped = overlapping(true);

	// WHERE THEY DO NOT OVERLAP the two agree: one shape at half alpha.
	assert!((separate_alpha(&separate, 2, 8) as i32 - 128).abs() <= 2, "{:?}", pixel(&separate, 2, 8));
	assert!((separate_alpha(&grouped, 2, 8) as i32 - 128).abs() <= 2, "{:?}", pixel(&grouped, 2, 8));
	// WHERE THEY DO, the separate drawings composite twice - a half over a half is three quarters -
	// and the layer composites once.
	assert!((separate_alpha(&separate, 8, 8) as i32 - 191).abs() <= 3, "half over half is three quarters: {:?}", pixel(&separate, 8, 8));
	assert!((separate_alpha(&grouped, 8, 8) as i32 - 128).abs() <= 3, "a group is composited ONCE: {:?}", pixel(&grouped, 8, 8));
}

fn separate_alpha(image: &OwnedImage, x: u32, y: u32) -> u8 {
	pixel(image, x, y)[3]
}

#[test]
// EVERY PORTER-DUFF OPERATOR AND EVERY BLEND MODE, through the backend and against the value the
// algebra gives. The arithmetic is checked in `graphics-core`; what this checks is that the backend
// actually reaches it - an operator recorded and silently composited as source-over is the failure a
// per-operator fixture exists to catch.
fn the_operators_and_blends_reach_the_pixels() {
	let draw_with = |operator: Operator, blend: BlendMode| {
		let mut image = target(8, 8);
		let mut canvas = Canvas::new();
		// A GREEN BACKDROP AND NOT A WHITE ONE. White is the identity for Multiply, the annihilator
		// for Screen and the fixed point of half the list: over white, most of the sixteen modes
		// produce the source unchanged, and a fixture that used it would pass with every one of them
		// unimplemented.
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 8.0, 8.0)), Paint::Solid(Color::new(0.0, 1.0, 0.25, 1.0, ColorSpace::Srgb)), FillRule::NonZero).expect("a backdrop");
		canvas.set_operator(operator);
		canvas.set_blend_mode(blend);
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 8.0, 8.0)), Paint::Solid(Color::new(1.0, 0.0, 0.0, 0.5, ColorSpace::Srgb)), FillRule::NonZero).expect("a source");
		draw(&canvas.finish().expect("a list"), &mut image);
		pixel(&image, 4, 4)
	};

	// `Clear` leaves nothing, `Dst` leaves the backdrop, `Src` replaces it.
	assert_eq!(draw_with(Operator::Clear, BlendMode::Normal)[3], 0);
	assert_eq!(draw_with(Operator::Dst, BlendMode::Normal)[1], 255, "Dst leaves the green backdrop");
	assert!((draw_with(Operator::Src, BlendMode::Normal)[3] as i32 - 128).abs() <= 2);
	// `SrcOver` of a half-transparent red over green is half of each, at full alpha.
	let over = draw_with(Operator::SrcOver, BlendMode::Normal);
	assert_eq!(over[3], 255);
	assert!(over[0] > 100 && over[1] > 100, "half red over green is both: {over:?}");
	// A BLEND MODE CHANGES THE COLOUR AND NOT THE COVERAGE: red times green is black, so the blended
	// result is darker than the plain one and its alpha is the same.
	let multiplied = draw_with(Operator::SrcOver, BlendMode::Multiply);
	assert_eq!(multiplied[3], 255);
	assert!(multiplied[0] < over[0], "red multiplied by green keeps neither: {multiplied:?} against {over:?}");
	// AND EVERY ONE OF THEM RUNS. A mode the backend silently treated as Normal would be found here.
	let mut distinct = alloc::vec::Vec::new();
	for mode in graphics_core::composite::ALL_BLEND_MODES {
		distinct.push(draw_with(Operator::SrcOver, mode));
	}
	assert_eq!(distinct.len(), 16);
	let normal = distinct[0];
	assert!(distinct.iter().filter(|value| **value != normal).count() >= 8, "most blend modes must differ from Normal over this pair");
}

#[test]
// DAMAGE IS CONSERVATIVE AND NOT EXACT. Exact damage would mean comparing old and new pixels - a
// source-over with alpha zero changes nothing while covering a rectangle - and that cost defeats the
// purpose. What it must never be is too SMALL.
fn damage_is_conservative_and_clipped_to_the_target() {
	let mut image = target(64, 64);
	let mut canvas = Canvas::new();
	canvas.fill_path(rect_path(RectF::new(8.0, 8.0, 16.0, 16.0)), red(), FillRule::NonZero).expect("a fill");
	let list = canvas.finish().expect("a list");
	let mut backend = Soft2d::new();
	let prepared = backend.prepare(&list, &description(&image)).expect("a preparation");
	let damage = prepared.damage().expect("something was drawn");
	assert!(damage.x <= 8 && damage.y <= 8, "{damage:?}");
	assert!(damage.x + damage.width >= 24 && damage.y + damage.height >= 24, "{damage:?}");
	assert!(damage.fits(image.layout().extent), "damage is clipped to the target: {damage:?}");
	assert_eq!(prepared.commands(), 1);
	assert_eq!(prepared.command_damage(0), Some(damage), "one command's damage is the frame's");

	// A DRAWING ENTIRELY OFF THE TARGET DAMAGES NOTHING, which is an empty answer and not a refusal.
	let mut canvas = Canvas::new();
	canvas.fill_path(rect_path(RectF::new(500.0, 500.0, 4.0, 4.0)), red(), FillRule::NonZero).expect("a fill");
	let list = canvas.finish().expect("a list");
	let prepared = backend.prepare(&list, &description(&image)).expect("a preparation");
	assert_eq!(prepared.damage(), None);
	let mut view = image.view_mut();
	backend.render(&prepared, &mut view).expect("a frame that draws nothing");
}

#[test]
// A RENDERER THAT WRITES PAST ITS TARGET IS A SECURITY BUG AND NOT A DRAWING BUG. The canaries are the
// pitch padding inside every row and the bytes after the last visible one: a drawing that reaches
// either has written outside the image it was given.
fn nothing_is_written_outside_the_target() {
	let semantics = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Straight };
	let storage = PixelStorage::Known(PixelFormat::R8G8B8A8Unorm);
	// A pitch eight bytes wider than the rows need, and sixty-four bytes of tail.
	let (width, height, pitch) = (16u32, 16u32, 16 * 4 + 8);
	let layout = ImageLayout::new(Extent2D::new(width, height), pitch, storage, RowOrigin::TopLeft, semantics).expect("a layout");
	let mut bytes = alloc::vec![0xA5u8; pitch as usize * height as usize + 64];
	let mut canvas = Canvas::new();
	canvas.fill_path(rect_path(RectF::new(-8.0, -8.0, 64.0, 64.0)), red(), FillRule::NonZero).expect("a fill reaching past every edge");
	let list = canvas.finish().expect("a list");
	let mut backend = Soft2d::new();
	let description = TargetDescription { extent: Extent2D::new(width, height), format: PixelFormat::R8G8B8A8Unorm, color_space: ColorSpace::Srgb, scale: 1.0, luminance: OutputLuminance::UNKNOWN };
	let prepared = backend.prepare(&list, &description).expect("a preparation");
	{
		let mut view = graphics_core::ImageViewMut::new(layout, &mut bytes).expect("a view");
		backend.render(&prepared, &mut view).expect("a frame");
	}
	for row in 0..height {
		let padding_start = row as usize * pitch as usize + width as usize * 4;
		for byte in &bytes[padding_start..padding_start + 8] {
			assert_eq!(*byte, 0xA5, "the pitch padding of row {row} was written");
		}
	}
	let tail = pitch as usize * height as usize;
	for byte in &bytes[tail..] {
		assert_eq!(*byte, 0xA5, "the bytes after the image were written");
	}
	// And the drawing itself did happen.
	assert_eq!(bytes[3], 255);
}

/// An image source for a fixture: one image, under one identity.
struct OneImage {
	identity: u64,
	image: OwnedImage,
}

impl ImageSource for OneImage {
	fn image(&self, identity: u64) -> Option<graphics_core::ImageView<'_>> {
		(identity == self.identity).then(|| self.image.view())
	}
}

/// A checkerboard, which is the pattern that shows minification: it averages to a flat grey and
/// aliases to noise.
fn checkerboard(size: u32) -> OwnedImage {
	let semantics = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Opaque };
	let storage = PixelStorage::Known(PixelFormat::R8G8B8X8Unorm);
	let layout = ImageLayout::new(Extent2D::new(size, size), size * 4, storage, RowOrigin::TopLeft, semantics).expect("a layout");
	let mut image = OwnedImage::new(layout).expect("an image");
	{
		let mut view = image.view_mut();
		for y in 0..size {
			for x in 0..size {
				let value = if (x + y) % 2 == 0 { 1.0 } else { 0.0 };
				graphics_core::pixel::write(&mut view, x, y, graphics_core::pixel::Rgba::new(value, value, value, 1.0));
			}
		}
	}
	image
}

#[test]
// EVERY PAINT, INCLUDING CONIC. A pie chart, a colour wheel and a loading spinner are conic
// gradients, and a backend that typed them and refused them makes each of those an image somebody
// generated somewhere else.
fn every_gradient_paints_and_every_spread_mode_differs() {
	let stops = |canvas: &mut Canvas| {
		canvas
			.resources()
			.add_stops(alloc::vec![
				render2d::paint::GradientStop { offset: 0.0, color: Color::new(1.0, 0.0, 0.0, 1.0, ColorSpace::Srgb) },
				render2d::paint::GradientStop { offset: 1.0, color: Color::new(0.0, 0.0, 1.0, 1.0, ColorSpace::Srgb) },
			])
			.expect("stops")
	};

	// A LINEAR GRADIENT ACROSS THE MIDDLE EIGHT PIXELS, clamped at both ends.
	let mut image = target(16, 4);
	let mut canvas = Canvas::new();
	let handle = stops(&mut canvas);
	let paint = Paint::Linear { from: PointF { x: 4.0, y: 0.0 }, to: PointF { x: 12.0, y: 0.0 }, stops: handle, spread: graphics_core::sample::Spread::Clamp, transform: render2d::transform::Transform::IDENTITY };
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 16.0, 4.0)), paint, FillRule::NonZero).expect("a fill");
	draw(&canvas.finish().expect("a list"), &mut image);
	assert_eq!(pixel(&image, 1, 2)[0], 255, "clamped before the start is the first stop");
	assert_eq!(pixel(&image, 14, 2)[2], 255, "and after the end is the last");
	let middle = pixel(&image, 8, 2);
	assert!(middle[0] > 100 && middle[2] > 100, "the middle is a mix of both: {middle:?}");

	// THE THREE SPREAD MODES ARE THREE DIFFERENT PICTURES past the gradient's own span.
	let spread_pixel = |spread: graphics_core::sample::Spread| {
		let mut image = target(16, 4);
		let mut canvas = Canvas::new();
		let handle = stops(&mut canvas);
		let paint = Paint::Linear { from: PointF { x: 0.0, y: 0.0 }, to: PointF { x: 4.0, y: 0.0 }, stops: handle, spread, transform: render2d::transform::Transform::IDENTITY };
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 16.0, 4.0)), paint, FillRule::NonZero).expect("a fill");
		draw(&canvas.finish().expect("a list"), &mut image);
		pixel(&image, 5, 2)
	};
	let clamped = spread_pixel(graphics_core::sample::Spread::Clamp);
	let repeated = spread_pixel(graphics_core::sample::Spread::Repeat);
	let mirrored = spread_pixel(graphics_core::sample::Spread::Mirror);
	assert_eq!(clamped[2], 255, "clamp holds the last stop: {clamped:?}");
	assert!(repeated[0] > repeated[2], "repeat starts the ramp again: {repeated:?}");
	assert!(mirrored[2] > mirrored[0], "mirror runs it backwards: {mirrored:?}");

	// A RADIAL GRADIENT IS ROUND: its centre is the first stop and its rim the last.
	let mut radial = target(16, 16);
	let mut canvas = Canvas::new();
	let handle = stops(&mut canvas);
	let paint = Paint::Radial { from: PointF { x: 8.0, y: 8.0 }, from_radius: 0.0, to: PointF { x: 8.0, y: 8.0 }, to_radius: 8.0, stops: handle, spread: graphics_core::sample::Spread::Clamp, transform: render2d::transform::Transform::IDENTITY };
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 16.0, 16.0)), paint, FillRule::NonZero).expect("a fill");
	draw(&canvas.finish().expect("a list"), &mut radial);
	assert!(pixel(&radial, 8, 8)[0] > 200, "the centre: {:?}", pixel(&radial, 8, 8));
	assert!(pixel(&radial, 0, 8)[2] > 200, "the rim: {:?}", pixel(&radial, 0, 8));

	// AND A CONIC GRADIENT SWEEPS AROUND ITS CENTRE: two points at different angles and the same
	// radius are different colours, which is the property that distinguishes it from a radial.
	let mut conic = target(16, 16);
	let mut canvas = Canvas::new();
	let handle = stops(&mut canvas);
	let paint = Paint::Conic { centre: PointF { x: 8.0, y: 8.0 }, start_angle: 0.0, end_angle: core::f32::consts::TAU, stops: handle, spread: graphics_core::sample::Spread::Clamp, transform: render2d::transform::Transform::IDENTITY };
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 16.0, 16.0)), paint, FillRule::NonZero).expect("a fill");
	draw(&canvas.finish().expect("a list"), &mut conic);
	let right = pixel(&conic, 14, 8);
	let below = pixel(&conic, 8, 14);
	assert!(right != below, "a conic gradient's colour depends on the ANGLE: {right:?} against {below:?}");
	assert!(right[0] > below[0], "and it sweeps from the start angle: {right:?} against {below:?}");
}

#[test]
// A GENERAL IMAGE RENDERER THAT ONLY HAS BILINEAR PRODUCES SHIMMERING THUMBNAILS. A checkerboard
// scaled down to a few pixels must average to a flat grey; sampled without a pyramid it comes out as
// whichever texels the sample points happened to land on.
fn images_are_drawn_at_quality_and_minified_without_aliasing() {
	let source = OneImage { identity: 7, image: checkerboard(32) };
	let record = render2d::list::ImageRecord { identity: 7, layout_generation: 1, content_generation: 1 };

	let draw_scaled = |quality: render2d::paint::ImageQuality| {
		let mut image = target(4, 4);
		let mut canvas = Canvas::new();
		canvas.draw_image(record, RectF::new(0.0, 0.0, 32.0, 32.0), RectF::new(0.0, 0.0, 4.0, 4.0), quality).expect("an image");
		let list = canvas.finish().expect("a list");
		let mut backend = Soft2d::new().with_images(&source);
		let prepared = backend.prepare(&list, &description(&image)).expect("a preparation");
		{
			let mut view = image.view_mut();
			backend.render(&prepared, &mut view).expect("a frame");
		}
		image
	};

	let mipmapped = draw_scaled(render2d::paint::ImageQuality::Mipmapped);
	// EIGHT TIMES DOWN, so every output pixel covers sixty-four texels of which half are white: the
	// answer is the average and not one of them.
	for (x, y) in [(0u32, 0u32), (1, 2), (3, 3)] {
		let value = pixel(&mipmapped, x, y);
		assert!(value[0] > 60 && value[0] < 220, "a minified checkerboard is grey and not noise: {value:?}");
	}
	// NEAREST IS THE OTHER ANSWER, and it is a real one: pixel art must not be softened.
	//
	// THE CLAIM IS THAT A TEXEL SURVIVES, NOT THAT ITS VALUE IS 255. This counted pixels that were
	// exactly 0 or exactly 255, which stopped being the same question when the image-colour profile
	// named a KNEE: diffuse white is above it, so white comes back at 243 rather than 255 - and a
	// count of 0-or-255 then reads a softened image where there is none. What nearest sampling
	// promises is that an output pixel IS one of the two texels it landed on rather than a blend of
	// them, so the two surviving values are read off the image and the pixels are counted against
	// THEM.
	let nearest = draw_scaled(render2d::paint::ImageQuality::Nearest);
	let values: alloc::vec::Vec<u8> = (0..4).flat_map(|y| (0..4).map(move |x| (x, y))).map(|(x, y)| pixel(&nearest, x, y)[0]).collect();
	let light = *values.iter().max().expect("sixteen pixels");
	// WITHIN ONE LEVEL OF THE BRIGHTEST, because the mapped white is not a representable 8-bit level
	// and the ordered dither spreads it over the two either side of it. A blend of the two texels
	// would land in the middle of the range, nowhere near either end.
	let extremes = values.iter().filter(|value| **value == 0 || **value + 1 >= light).count();
	assert!(extremes > 8, "nearest keeps the texels it lands on: {extremes} of sixteen are 0 or about {light}, from {values:?}");

	// A PROJECTIVE TRANSFORM IS IN THE PROFILE, and an image under one is drawn rather than refused:
	// the near edge is larger than the far one, which is what perspective means.
	let mut perspective = target(32, 32);
	let mut canvas = Canvas::new();
	canvas.set_transform(render2d::transform::Transform { m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, -0.01, 1.0]] });
	canvas.draw_image(record, RectF::new(0.0, 0.0, 32.0, 32.0), RectF::new(0.0, 0.0, 32.0, 32.0), render2d::paint::ImageQuality::Mipmapped).expect("an image");
	let list = canvas.finish().expect("a list");
	let mut backend = Soft2d::new().with_images(&source);
	let prepared = backend.prepare(&list, &description(&perspective)).expect("a preparation");
	{
		let mut view = perspective.view_mut();
		backend.render(&prepared, &mut view).expect("a frame");
	}
	let drawn = (0..32).flat_map(|y| (0..32).map(move |x| (x, y))).filter(|(x, y)| pixel(&perspective, *x, *y)[3] > 0).count();
	assert!(drawn > 100, "a projectively transformed image is drawn: {drawn} pixels");
}

#[test]
// THE COLOUR IS CONVERTED, TONE MAPPED AND DITHERED on the way to the target, which is the end of the
// one pipeline: a paint in a wide-gamut space is not the same numbers in a narrow one, a value above
// diffuse white is compressed rather than clipped, and a value between two levels is dithered rather
// than banded.
fn colour_conversion_tone_mapping_and_dithering_reach_the_target() {
	// A Display P3 green is OUTSIDE sRGB, so writing its numbers through unchanged would be a
	// different colour presented as the right one.
	let mut image = target(4, 4);
	let mut canvas = Canvas::new();
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 4.0, 4.0)), Paint::Solid(Color::new(0.0, 0.8, 0.2, 1.0, ColorSpace::DisplayP3)), FillRule::NonZero).expect("a fill");
	draw(&canvas.finish().expect("a list"), &mut image);
	let converted = pixel(&image, 1, 1);
	let mut plain = target(4, 4);
	let mut canvas = Canvas::new();
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 4.0, 4.0)), Paint::Solid(Color::new(0.0, 0.8, 0.2, 1.0, ColorSpace::Srgb)), FillRule::NonZero).expect("a fill");
	draw(&canvas.finish().expect("a list"), &mut plain);
	assert!(converted != pixel(&plain, 1, 1), "the same numbers in two spaces are two colours: {converted:?}");

	// ADDITIVE LIGHT GOES ABOVE ONE and the narrow target TONE MAPS it rather than clipping: the
	// result is brighter than either input and is not saturated white.
	let mut bright = target(4, 4);
	let mut canvas = Canvas::new();
	let grey = Paint::Solid(Color::new(0.7, 0.7, 0.7, 1.0, ColorSpace::Srgb));
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 4.0, 4.0)), grey, FillRule::NonZero).expect("a backdrop");
	canvas.set_operator(Operator::Plus);
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 4.0, 4.0)), grey, FillRule::NonZero).expect("and again, additively");
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 4.0, 4.0)), grey, FillRule::NonZero).expect("and a third time");
	draw(&canvas.finish().expect("a list"), &mut bright);
	let added = pixel(&bright, 1, 1);
	assert!(added[0] > pixel(&plain, 1, 1)[1], "additive light is brighter: {added:?}");
	assert!(added[0] < 255, "and a narrow target compresses it rather than clipping it to white: {added:?}");

	// THE DITHER IS ORDERED AND ITS PHASE IS THE TARGET'S ORIGIN, so a colour that falls between two
	// levels is a pattern rather than a band - and the pattern repeats every eight pixels.
	let mut dithered = target(16, 16);
	let mut canvas = Canvas::new();
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 16.0, 16.0)), Paint::Solid(Color::new(0.5019, 0.5019, 0.5019, 1.0, ColorSpace::Srgb)), FillRule::NonZero).expect("a fill");
	draw(&canvas.finish().expect("a list"), &mut dithered);
	let values: alloc::vec::Vec<u8> = (0..8).map(|x| pixel(&dithered, x, 0)[0]).collect();
	let distinct = values.iter().filter(|value| **value != values[0]).count();
	assert!(distinct > 0, "a value between two levels is dithered: {values:?}");
	for x in 0..8 {
		assert_eq!(pixel(&dithered, x, 0)[0], pixel(&dithered, x + 8, 0)[0], "the Bayer pattern repeats every eight pixels");
	}
}

#[test]
// EVERY FILTER NODE, ONE AT A TIME. A graph whose nodes were tested only in composition hides the one
// that does nothing: a blur that returns its input looks correct behind an offset and a flood.
fn every_filter_node_does_its_own_work() {
	let with_graph = |build: &dyn Fn(&mut render2d::filter::FilterGraph)| {
		let mut graph = render2d::filter::FilterGraph::default();
		build(&mut graph);
		let mut image = target(32, 32);
		let mut canvas = Canvas::new();
		let handle = canvas.resources().add_filter(graph).expect("a graph");
		canvas.begin_layer(None, 1.0, BlendMode::Normal, Some(handle)).expect("a filtered layer");
		canvas.fill_path(rect_path(RectF::new(12.0, 12.0, 8.0, 8.0)), red(), FillRule::NonZero).expect("a fill");
		canvas.end_layer().expect("closed");
		draw(&canvas.finish().expect("a list"), &mut image);
		image
	};
	use render2d::filter::FilterNode;

	// SOURCE alone is the drawing itself.
	let plain = with_graph(&|graph| {
		graph.push(FilterNode::Source).expect("a node");
	});
	assert_eq!(pixel(&plain, 16, 16)[3], 255);
	assert_eq!(pixel(&plain, 4, 16)[3], 0);

	// A BLUR SPREADS PAST THE SHAPE'S OWN BOUNDS, which is what the graph's bounds map grows the
	// layer by - a blur clipped to its input is the defect that gives every shadow a straight edge.
	let blurred = with_graph(&|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		graph.push(FilterNode::Blur { input: source, x: 2.0, y: 2.0 }).expect("a blur");
	});
	assert!(pixel(&blurred, 16, 16)[3] > 128, "the middle survives a blur: {:?}", pixel(&blurred, 16, 16));
	assert!(pixel(&blurred, 10, 16)[3] > 0, "and it reaches past the shape: {:?}", pixel(&blurred, 10, 16));

	// AN OFFSET MOVES IT, in the direction it says.
	let offset = with_graph(&|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		graph.push(FilterNode::Offset { input: source, dx: 6.0, dy: 0.0 }).expect("an offset");
	});
	assert_eq!(pixel(&offset, 16, 16)[3], 0, "what was here has moved");
	assert_eq!(pixel(&offset, 22, 16)[3], 255, "to six pixels along");

	// A COLOUR MATRIX REPLACES THE COLOUR: red through a matrix that swaps red and blue is blue.
	let swapped = with_graph(&|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		let matrix = [[0.0, 0.0, 1.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0, 0.0]];
		graph.push(FilterNode::ColorMatrix { input: source, matrix }).expect("a matrix");
	});
	let value = pixel(&swapped, 16, 16);
	assert!(value[2] > 200 && value[0] < 60, "red with its channels swapped is blue: {value:?}");

	// A FLOOD FILLS THE WHOLE OUTPUT, which is what makes a tint and a shadow's colour.
	let flooded = with_graph(&|graph| {
		graph.push(FilterNode::Flood { color: Color::new(0.0, 0.0, 1.0, 1.0, ColorSpace::Srgb) }).expect("a flood");
	});
	assert!(pixel(&flooded, 2, 2)[2] > 200, "a flood covers the layer: {:?}", pixel(&flooded, 2, 2));

	// `In` KEEPS ONLY WHERE THE MASK HAS ALPHA, which is what every clip-shaped effect is built on -
	// a flood kept inside the drawing's own alpha is a tint of the drawing.
	let tinted = with_graph(&|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		let flood = graph.push(FilterNode::Flood { color: Color::new(0.0, 0.0, 1.0, 1.0, ColorSpace::Srgb) }).expect("a flood");
		graph.push(FilterNode::In { input: flood, mask: source }).expect("kept inside");
	});
	assert!(pixel(&tinted, 16, 16)[2] > 200, "inside the shape the flood survives: {:?}", pixel(&tinted, 16, 16));
	assert_eq!(pixel(&tinted, 2, 2)[3], 0, "and outside it is gone");

	// COMPOSITE AND BLEND TAKE TWO INPUTS, which is what makes a shadow a shadow: the blurred,
	// offset, flooded copy UNDER the thing that cast it.
	let shadow = with_graph(&|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		let blur = graph.push(FilterNode::Blur { input: source, x: 2.0, y: 2.0 }).expect("a blur");
		let offset = graph.push(FilterNode::Offset { input: blur, dx: 4.0, dy: 4.0 }).expect("an offset");
		let flood = graph.push(FilterNode::Flood { color: Color::new(0.0, 0.0, 0.0, 1.0, ColorSpace::Srgb) }).expect("a flood");
		let tint = graph.push(FilterNode::In { input: flood, mask: offset }).expect("kept inside the blur");
		graph.push(FilterNode::Composite { source, backdrop: tint, operator: Operator::SrcOver }).expect("the thing over its shadow");
	});
	assert!(pixel(&shadow, 16, 16)[0] > 200, "the shape is still red: {:?}", pixel(&shadow, 16, 16));
	let under = pixel(&shadow, 23, 23);
	assert!(under[3] > 0 && under[0] < 60, "and its shadow is dark and below-right of it: {under:?}");

	// AND THE BACKDROP NODE READS WHAT IS UNDER THE LAYER, which is what a frosted panel is. Without
	// it the whole class of backdrop effects has to be built by drawing the scene twice.
	let mut image = target(32, 32);
	let mut canvas = Canvas::new();
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 32.0, 16.0)), Paint::Solid(Color::new(0.0, 0.0, 1.0, 1.0, ColorSpace::Srgb)), FillRule::NonZero).expect("something underneath");
	let mut graph = render2d::filter::FilterGraph::default();
	let backdrop = graph.push(FilterNode::Backdrop).expect("a node");
	graph.push(FilterNode::Blur { input: backdrop, x: 3.0, y: 3.0 }).expect("a blur of it");
	assert!(graph.reads_backdrop(), "a graph that reads the backdrop says so, which is what a compositor asks before it reorders anything");
	let handle = canvas.resources().add_filter(graph).expect("a graph");
	canvas.begin_layer(None, 1.0, BlendMode::Normal, Some(handle)).expect("a filtered layer");
	canvas.fill_path(rect_path(RectF::new(8.0, 8.0, 16.0, 16.0)), Paint::Solid(Color::new(1.0, 1.0, 1.0, 0.1, ColorSpace::Srgb)), FillRule::NonZero).expect("a pane");
	canvas.end_layer().expect("closed");
	draw(&canvas.finish().expect("a list"), &mut image);
	// The blue under the panel has been blurred across the boundary it had.
	let blurred_edge = pixel(&image, 16, 17);
	assert!(blurred_edge[2] > 0, "the backdrop's blue is blurred past the edge it had: {blurred_edge:?}");
}

/// A provider that answers with one form per kind, so every kind the profile lists is drawn.
struct EveryKind;

impl GlyphProvider for EveryKind {
	fn glyph(&self, key: &font_contract::cache::GlyphCacheKey) -> GlyphImage {
		use font_contract::glyph::{GlyphKind, RasterisationMode};
		match key.kind {
			GlyphKind::Outline => {
				let mut builder = PathBuilder::new();
				builder.add_rect(RectF::new(0.0, -4.0, 4.0, 4.0)).expect("a box");
				GlyphImage::Outline(builder.finish())
			}
			GlyphKind::GrayscaleMask => GlyphImage::Mask { left: 0, top: -4, width: 4, height: 4, coverage: alloc::vec![255u8; 16], mode: RasterisationMode::Grayscale },
			GlyphKind::SubpixelMask => GlyphImage::Mask { left: 0, top: -4, width: 4, height: 4, coverage: alloc::vec![255u8; 48], mode: key.mode },
			GlyphKind::BitmapStrike => {
				let semantics = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Straight };
				let storage = PixelStorage::Known(PixelFormat::R8G8B8A8Unorm);
				let layout = ImageLayout::new(Extent2D::new(4, 4), 16, storage, RowOrigin::TopLeft, semantics).expect("a layout");
				let mut image = OwnedImage::new(layout).expect("an image");
				{
					let mut view = image.view_mut();
					for y in 0..4 {
						for x in 0..4 {
							graphics_core::pixel::write(&mut view, x, y, graphics_core::pixel::Rgba::new(0.0, 0.0, 1.0, 1.0));
						}
					}
				}
				GlyphImage::Bitmap { left: 0, top: -4, image }
			}
			GlyphKind::ColrLayers | GlyphKind::ColrPaintGraph => {
				let mut builder = PathBuilder::new();
				builder.add_rect(RectF::new(0.0, -4.0, 4.0, 4.0)).expect("a box");
				GlyphImage::Layers(alloc::vec![(builder.finish(), Color::new(0.0, 1.0, 0.0, 1.0, ColorSpace::Srgb))])
			}
		}
	}
}

#[test]
// EVERY GLYPH KIND THE PROFILE LISTS. An outline, a grayscale mask, a subpixel mask, a bitmap strike
// and a colour-layer glyph are five different things to composite, and a renderer that only has the
// first draws a page of text and no emoji.
fn every_glyph_kind_is_drawn_and_the_cache_keys_them_apart() {
	use font_contract::glyph::{GlyphKind, RasterisationMode, SubpixelLayout};
	let run = |kind: GlyphKind, mode: RasterisationMode| render2d::list::RecordedGlyphRun { face: font_contract::FaceRef { face: font_contract::FaceIdentity { file: font_contract::face::FileIdentity([7u8; 32]), index: 0 }, generation: font_contract::face::Generation(1) }, size: font_contract::Fixed266::from_pixels(8), variation: font_contract::VariationCoordinates::default(), script: font_contract::ScriptTag::from_bytes(*b"latn"), direction: font_contract::Direction::LeftToRight, mode, origin_x: font_contract::Fixed266::from_pixels(4), origin_y: font_contract::Fixed266::from_pixels(12), glyphs: alloc::vec![font_contract::PositionedGlyph { glyph: 42, x_offset: font_contract::Fixed266::ZERO, y_offset: font_contract::Fixed266::ZERO, x_advance: font_contract::Fixed266::from_pixels(8), y_advance: font_contract::Fixed266::ZERO, kind, selection: font_contract::cache::KindSelection { strike: None, palette: None } }], clusters: alloc::vec![] };

	let provider = EveryKind;
	let drawn = |kind: GlyphKind, mode: RasterisationMode| {
		let mut image = target(24, 24);
		let mut canvas = Canvas::new();
		canvas.draw_glyph_run(run(kind, mode), red()).expect("a run");
		let list = canvas.finish().expect("a list");
		let mut backend = Soft2d::new().with_glyphs(&provider);
		let prepared = backend.prepare(&list, &description(&image)).expect("a preparation");
		{
			let mut view = image.view_mut();
			backend.render(&prepared, &mut view).expect("a frame");
		}
		(image, backend.glyph_cache().len())
	};

	let (outline, _) = drawn(GlyphKind::Outline, RasterisationMode::Grayscale);
	assert_eq!(pixel(&outline, 5, 9)[3], 255, "an outline glyph is filled with the run's paint");
	assert_eq!(pixel(&outline, 5, 9)[0], 255, "in the paint's own colour");

	let (grayscale, _) = drawn(GlyphKind::GrayscaleMask, RasterisationMode::Grayscale);
	assert!(grayscale.view().row(9).expect("a row")[4 * 4 + 3] > 0, "a grayscale mask composites its coverage");

	let (subpixel, _) = drawn(GlyphKind::SubpixelMask, RasterisationMode::Subpixel(SubpixelLayout::RgbHorizontal));
	assert!(pixel(&subpixel, 5, 9)[3] > 0, "a subpixel mask is composited per channel");

	let (bitmap, _) = drawn(GlyphKind::BitmapStrike, RasterisationMode::Grayscale);
	assert!(pixel(&bitmap, 5, 9)[2] > 200, "a bitmap strike brings its OWN colour, not the paint's: {:?}", pixel(&bitmap, 5, 9));

	let (layers, _) = drawn(GlyphKind::ColrLayers, RasterisationMode::Grayscale);
	assert!(pixel(&layers, 5, 9)[1] > 200, "a colour-layer glyph uses its palette colour: {:?}", pixel(&layers, 5, 9));

	// AND THE CACHE KEYS THEM APART. Two glyphs differing only in KIND, or only in rasterisation
	// MODE, must not share an entry - which is the stale-pixel case the eleven-field key exists for.
	let mut cache = GlyphRaster::default();
	let key = |kind: GlyphKind, mode: RasterisationMode| font_contract::cache::GlyphCacheKey { face: font_contract::FaceIdentity { file: font_contract::face::FileIdentity([7u8; 32]), index: 0 }, generation: font_contract::face::Generation(1), glyph: 42, size: font_contract::Fixed266::from_pixels(8), variation: font_contract::VariationCoordinates::default(), transform: font_contract::glyph::TransformKey::IDENTITY, phase: font_contract::glyph::SubpixelPhase { x: 0, y: 0 }, kind, selection: font_contract::cache::KindSelection { strike: None, palette: None }, mode };
	assert_eq!(cache.get(&key(GlyphKind::Outline, RasterisationMode::Grayscale), &provider).kind(), Some(GlyphKind::Outline));
	assert_eq!(cache.get(&key(GlyphKind::GrayscaleMask, RasterisationMode::Grayscale), &provider).kind(), Some(GlyphKind::GrayscaleMask));
	assert_eq!(cache.len(), 2, "two kinds are two entries");
	cache.get(&key(GlyphKind::SubpixelMask, RasterisationMode::Subpixel(SubpixelLayout::RgbHorizontal)), &provider);
	cache.get(&key(GlyphKind::SubpixelMask, RasterisationMode::Subpixel(SubpixelLayout::BgrVertical)), &provider);
	assert_eq!(cache.len(), 4, "one glyph rasterised for two subpixel geometries is two entries");
	// A CLEARED CACHE IS A NEW GENERATION, which is what makes every prepared list bound to the old
	// one refuse by name rather than replay against entries that are gone.
	let before = cache.generation();
	cache.clear();
	assert!(cache.is_empty() && cache.generation() != before);
}

#[test]
// THE WIDE PATH AND THE SCALAR REFERENCE ARE ONE ALGORITHM, so they agree BIT FOR BIT and not within
// a tolerance. A tolerance between them would let the fast path drift until the difference showed up
// as a seam between the pixels one covered and the pixels the other did.
fn the_wide_span_path_agrees_with_the_scalar_reference_exactly() {
	use graphics_core::pixel::Rgba;
	let value = |seed: u32| {
		let part = |shift: u32| ((seed >> shift) & 0xff) as f32 / 255.0;
		let alpha = part(24);
		// Premultiplied, which is what a span carries: the colour may not exceed the alpha.
		Rgba::new(part(0) * alpha, part(8) * alpha, part(16) * alpha, alpha)
	};
	// LENGTHS AROUND THE LANE WIDTH, because the tail is where a wide loop goes wrong and every
	// length that is an exact multiple of it would hide it.
	for length in [0usize, 1, 3, 4, 5, 7, 8, 9, 15, 16, 17, 64, 129] {
		let source: alloc::vec::Vec<Rgba> = (0..length).map(|index| value((index as u32).wrapping_mul(2_654_435_761))).collect();
		let start: alloc::vec::Vec<Rgba> = (0..length).map(|index| value((index as u32).wrapping_mul(40_503).wrapping_add(7))).collect();
		let mut wide = start.clone();
		let mut reference = start.clone();
		crate::span::composite_span(&mut wide, &source, Operator::SrcOver, BlendMode::Normal);
		crate::span::composite_span_scalar(&mut reference, &source, Operator::SrcOver, BlendMode::Normal);
		for (index, (fast, slow)) in wide.iter().zip(reference.iter()).enumerate() {
			assert_eq!(fast.red.to_bits(), slow.red.to_bits(), "length {length}, pixel {index}: {fast:?} against {slow:?}");
			assert_eq!(fast.alpha.to_bits(), slow.alpha.to_bits(), "length {length}, pixel {index}");
		}

		// AND THE SAME FOR THE COVERAGE MULTIPLY, which is the other loop that was widened.
		let coverage: alloc::vec::Vec<f32> = (0..length).map(|index| (index % 17) as f32 / 16.0).collect();
		let mut wide = start.clone();
		let mut reference = start.clone();
		crate::span::scale_span(&mut wide, &coverage);
		crate::span::scale_span_scalar(&mut reference, &coverage);
		assert!(wide.iter().zip(reference.iter()).all(|(fast, slow)| fast.red.to_bits() == slow.red.to_bits() && fast.alpha.to_bits() == slow.alpha.to_bits()), "length {length}");
	}
	assert_eq!(crate::span::LANES, 4);
}

/// A cancellation that fires after a stated number of questions.
struct StopAfter {
	limit: core::cell::Cell<u32>,
}

impl backend::Cancellation for StopAfter {
	fn cancelled(&self) -> bool {
		let left = self.limit.get();
		if left == 0 {
			return true;
		}
		self.limit.set(left - 1);
		false
	}
}

#[test]
// A SURFACE THAT WAS CLOSED OR RESIZED MID-FRAME IS A FRAME NOBODY WILL SEE. Finishing it costs the
// whole drawing for nothing, and on a resize it costs it at the WRONG SIZE - so the loop asks between
// tiles and says plainly that it stopped.
fn a_cancelled_frame_stops_and_says_so() {
	let mut image = target(256, 256);
	let mut canvas = Canvas::new();
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 256.0, 256.0)), red(), FillRule::NonZero).expect("a fill");
	let list = canvas.finish().expect("a list");
	let stop = StopAfter { limit: core::cell::Cell::new(2) };
	let mut backend = Soft2d::new().with_cancellation(&stop);
	let prepared = backend.prepare(&list, &description(&image)).expect("a preparation");
	{
		let mut view = image.view_mut();
		assert_eq!(backend.render(&prepared, &mut view).err(), Some(render2d::Error::Cancelled));
	}
	// WHAT WAS DRAWN STAYS DRAWN, in whole tiles: the first two tiles are complete and the rest are
	// untouched, which is a stop at a boundary rather than a shape cut in half.
	assert_eq!(pixel(&image, 4, 4)[3], 255, "the first tile was finished");
	assert_eq!(pixel(&image, 200, 200)[3], 0, "and the last was never started");
}

#[test]
// A PREPARED LIST IS A CACHE, and a cache whose validity conditions are not enumerated eventually
// replays a drawing that is not the one recorded. Each dependency is changed ALONE here, and each
// refusal names the one that changed.
fn a_prepared_list_is_bound_to_what_it_was_prepared_against() {
	let mut image = target(32, 32);
	let mut canvas = Canvas::new();
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 8.0, 8.0)), red(), FillRule::NonZero).expect("a fill");
	let list = canvas.finish().expect("a list");
	let mut backend = Soft2d::new();
	let description = description(&image);
	let prepared = backend.prepare(&list, &description).expect("a preparation");
	let key = render2d::backend::Prepared::key(&prepared);
	assert_eq!(key.backend, BACKEND_NAME);

	let against = |changed: TargetDescription| render2d::prepared::PreparedKey::of(&list, &changed, (BACKEND_NAME, BACKEND_VERSION), backend.glyph_cache().generation());
	assert_eq!(key.compatible_with(&against(description)), Ok(()));
	assert_eq!(key.compatible_with(&against(TargetDescription { extent: Extent2D::new(64, 32), ..description })), Err(render2d::prepared::RePrepare::Extent));
	assert_eq!(key.compatible_with(&against(TargetDescription { scale: 2.0, ..description })), Err(render2d::prepared::RePrepare::Scale));
	assert_eq!(key.compatible_with(&against(TargetDescription { format: PixelFormat::B8G8R8A8Unorm, ..description })), Err(render2d::prepared::RePrepare::Format));
	assert_eq!(key.compatible_with(&against(TargetDescription { color_space: ColorSpace::DisplayP3, ..description })), Err(render2d::prepared::RePrepare::ColorSpace));

	// AND A RENDER AGAINST A TARGET OF THE WRONG SIZE IS REFUSED rather than drawing off the end of
	// it: the preparation's tiling is the target's, and a different target is a different tiling.
	let mut wider = target(64, 32);
	let mut view = wider.view_mut();
	assert!(backend.render(&prepared, &mut view).is_err());
	let mut view = image.view_mut();
	backend.render(&prepared, &mut view).expect("the target it was prepared for");
}

#[test]
// HOSTILE INPUT IS ANSWERED AND NEVER CRASHED ON. Coordinates at the edges of the type, degenerate
// curves, singular and projective transforms, a pitch with padding, a deep filter graph and a clip
// stack at its ceiling: none of them may panic, and none may write outside the target - which the
// canary around this one checks on every iteration.
fn hostile_input_is_answered_rather_than_crashed_on() {
	let hostile = [0.0f32, -0.0, 1.0, -1.0, f32::MAX, f32::MIN, f32::EPSILON, 1e30, -1e30, f32::NAN, f32::INFINITY, f32::NEG_INFINITY];
	for value in hostile {
		let mut builder = PathBuilder::new();
		let _ = builder.move_to(PointF { x: value, y: value });
		let _ = builder.line_to(PointF { x: -value, y: value });
		let _ = builder.quad_to(PointF { x: value, y: -value }, PointF { x: 0.0, y: 0.0 });
		let _ = builder.cubic_to(PointF { x: value, y: 0.0 }, PointF { x: 0.0, y: value }, PointF { x: 4.0, y: 4.0 });
		let _ = builder.close();
		let path = builder.finish();

		let transforms = [
			render2d::transform::Transform::IDENTITY,
			render2d::transform::Transform::scale(value, value),
			render2d::transform::Transform { m: [[value, 0.0, 0.0], [0.0, value, 0.0], [value, value, 0.0]] },
			render2d::transform::Transform { m: [[0.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]] },
		];
		for transform in transforms {
			let mut canvas = Canvas::new();
			canvas.set_transform(transform);
			let _ = canvas.fill_path(path.clone(), red(), FillRule::EvenOdd);
			let _ = canvas.stroke_path(path.clone(), red(), render2d::path::StrokeStyle { width: value, miter_limit: value, ..render2d::path::StrokeStyle::default() });
			let Ok(list) = canvas.finish() else { continue };
			// A PITCH WITH PADDING AND A CANARY AFTER THE IMAGE, so a write outside either is caught.
			let semantics = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Straight };
			let storage = PixelStorage::Known(PixelFormat::R8G8B8A8Unorm);
			let layout = ImageLayout::new(Extent2D::new(9, 7), 9 * 4 + 5, storage, RowOrigin::TopLeft, semantics).expect("a layout");
			let mut bytes = alloc::vec![0xC3u8; (9 * 4 + 5) * 7 + 32];
			let mut backend = Soft2d::new();
			let description = TargetDescription { extent: Extent2D::new(9, 7), format: PixelFormat::R8G8B8A8Unorm, color_space: ColorSpace::Srgb, scale: 1.0, luminance: OutputLuminance::UNKNOWN };
			if let Ok(prepared) = backend.prepare(&list, &description) {
				let mut view = graphics_core::ImageViewMut::new(layout, &mut bytes).expect("a view");
				let _ = backend.render(&prepared, &mut view);
			}
			let tail = (9 * 4 + 5) * 7;
			assert!(bytes[tail..].iter().all(|byte| *byte == 0xC3), "a hostile drawing wrote past the image: {value}");
		}
	}

	// A DEEP FILTER GRAPH, to the profile's own ceiling: every node reads the one before it, which is
	// the deepest chain the type allows.
	// SIXTEEN DEEP AND NOT THE PROFILE'S SIXTY-FOUR. Each node of a chain of blurs grows the layer's
	// scratch by its own reach, so a chain at the ceiling is a quarter of a megabyte per node over a
	// surface a quarter of a megapixel - twenty seconds of work to learn what sixteen already show.
	// That the BUILDER refuses past the ceiling is the profile's own fixture, in `render2d`.
	let mut graph = render2d::filter::FilterGraph::default();
	let mut previous = graph.push(render2d::filter::FilterNode::Source).expect("a source");
	for _ in 0..15 {
		previous = graph.push(render2d::filter::FilterNode::Blur { input: previous, x: 0.5, y: 0.5 }).expect("a blur");
	}
	assert_eq!(graph.nodes().len(), 16);
	let mut canvas = Canvas::new();
	let handle = canvas.resources().add_filter(graph).expect("a graph");
	canvas.begin_layer(None, 1.0, BlendMode::Normal, Some(handle)).expect("a filtered layer");
	canvas.fill_path(rect_path(RectF::new(1.0, 1.0, 4.0, 4.0)), red(), FillRule::NonZero).expect("a fill");
	canvas.end_layer().expect("closed");
	let list = canvas.finish().expect("a list");
	let mut image = target(16, 16);
	let mut backend = Soft2d::new();
	match backend.prepare(&list, &description(&image)) {
		// EITHER IT FITS AND DRAWS, OR IT IS REFUSED BY NAME. What it must not be is a partial frame.
		Ok(prepared) => {
			let mut view = image.view_mut();
			let _ = backend.render(&prepared, &mut view);
		}
		Err(error) => assert!(matches!(error, render2d::Error::LimitExceeded { .. }), "{error:?}"),
	}

	// AND A CLIP STACK AT ITS CEILING, which is the other bound a drawing can reach.
	let limits = graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS;
	let mut canvas = Canvas::new();
	for index in 0..limits.max_clip_depth.min(16) {
		canvas.save().expect("a state");
		canvas.set_clip(rect_path(RectF::new(index as f32 * 0.25, 0.0, 16.0, 16.0)), FillRule::NonZero).expect("a clip");
	}
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 16.0, 16.0)), red(), FillRule::NonZero).expect("a fill");
	for _ in 0..limits.max_clip_depth.min(16) {
		canvas.restore().expect("popped");
	}
	let list = canvas.finish().expect("a list");
	let mut image = target(16, 16);
	let mut backend = Soft2d::new();
	let prepared = backend.prepare(&list, &description(&image)).expect("a preparation");
	let mut view = image.view_mut();
	backend.render(&prepared, &mut view).expect("a frame under sixteen nested clips");
	assert!(pixel(&image, 8, 8)[3] > 0, "the drawing survives the nesting");
}

#[test]
// THE ALIASED INTEGER LINE IS AN EXPLICIT FAST PATH and keeps the rule it already had: both endpoints
// included, ties broken toward the smaller minor coordinate, and clipping applied BEFORE rasterising -
// so a clipped line covers the same pixels as the visible part of the unclipped one.
fn an_aliased_one_pixel_line_covers_exactly_its_pixels() {
	let line = |from: PointF, to: PointF, width: u32, height: u32| {
		let mut image = target(width, height);
		let mut canvas = Canvas::new();
		canvas.set_antialias(Antialias::Off);
		let mut builder = PathBuilder::new();
		builder.move_to(from).expect("a start");
		builder.line_to(to).expect("a line");
		canvas.stroke_path(builder.finish(), red(), render2d::path::StrokeStyle { width: 1.0, ..render2d::path::StrokeStyle::default() }).expect("a stroke");
		draw(&canvas.finish().expect("a list"), &mut image);
		image
	};

	// A HORIZONTAL LINE IS ONE ROW OF PIXELS, both ends included.
	let horizontal = line(PointF { x: 1.5, y: 3.5 }, PointF { x: 6.5, y: 3.5 }, 8, 8);
	for x in 1..=6 {
		assert_eq!(pixel(&horizontal, x, 3)[3], 255, "pixel {x} of the line");
	}
	assert_eq!(pixel(&horizontal, 0, 3)[3], 0, "and nothing before its start");
	assert_eq!(pixel(&horizontal, 7, 3)[3], 0, "or after its end");
	assert_eq!(pixel(&horizontal, 3, 2)[3], 0, "one row is one row");

	// A DIAGONAL STEPS ONE PIXEL AT A TIME, with no partial coverage anywhere.
	let diagonal = line(PointF { x: 0.5, y: 0.5 }, PointF { x: 7.5, y: 7.5 }, 8, 8);
	for index in 0..8 {
		assert_eq!(pixel(&diagonal, index, index)[3], 255, "the diagonal at {index}");
	}
	for y in 0..8 {
		for x in 0..8 {
			let alpha = pixel(&diagonal, x, y)[3];
			assert!(alpha == 0 || alpha == 255, "an aliased line has no partial coverage at {x},{y}: {alpha}");
		}
	}

	// CLIPPING HAPPENS BEFORE RASTERISING, so the visible part is the same pixels it would have had.
	let clipped = line(PointF { x: -20.5, y: 2.5 }, PointF { x: 20.5, y: 2.5 }, 8, 8);
	for x in 0..8 {
		assert_eq!(pixel(&clipped, x, 2)[3], 255, "the visible part of a line that starts off the target: {x}");
	}
}

/// A source whose image is PLANES: what a decoder or a camera actually hands over.
struct VideoFrame {
	layout: graphics_core::planar::MultiPlaneLayout,
	luma: alloc::vec::Vec<u8>,
	chroma: alloc::vec::Vec<u8>,
}

impl ImageSource for VideoFrame {
	fn image(&self, _identity: u64) -> Option<graphics_core::ImageView<'_>> {
		// A PLANAR SOURCE HAS NO SINGLE-PLANE VIEW. Answering one here would be the full-frame
		// conversion per frame that the multi-plane model exists to remove.
		None
	}

	fn planes(&self, identity: u64) -> Option<graphics_core::planar::MultiPlaneView<'_>> {
		(identity == 11).then(|| graphics_core::planar::MultiPlaneView::new(self.layout, [&self.luma, &self.chroma, &[]]).expect("a view"))
	}
}

#[test]
// A VIDEO FRAME IS DRAWN FROM ITS PLANES, through the same sampler, the same transfer function and
// the same compositor as every other image. Without this a player converts every frame to RGBA
// first - a full-frame conversion on the CPU, once per frame, for the one workload where that cost
// is least affordable.
fn a_planar_video_frame_draws_without_being_converted_first() {
	use graphics_core::planar::{MultiPlaneLayout, PlanarFormat, YuvMatrix, YuvRange};
	let extent = Extent2D::new(8, 8);
	let layout = MultiPlaneLayout::new(extent, PlanarFormat::Nv12, YuvMatrix::Bt709, YuvRange::Limited, ColorSpace::Srgb, [8, 8, 0]).expect("a layout");
	// The left half is white and the right half is black, so the drawing has an edge to be in the
	// right place.
	let mut luma = alloc::vec![16u8; 64];
	for y in 0..8 {
		for x in 0..4 {
			luma[y * 8 + x] = 235;
		}
	}
	let source = VideoFrame { layout, luma, chroma: alloc::vec![128u8; 64] };

	let mut image = target(16, 16);
	let mut canvas = Canvas::new();
	canvas.draw_image(render2d::list::ImageRecord { identity: 11, layout_generation: 1, content_generation: 1 }, RectF::new(0.0, 0.0, 8.0, 8.0), RectF::new(0.0, 0.0, 16.0, 16.0), render2d::paint::ImageQuality::Nearest).expect("a frame");
	let list = canvas.finish().expect("a list");
	let mut backend = Soft2d::new().with_images(&source);
	let prepared = backend.prepare(&list, &description(&image)).expect("a preparation");
	{
		let mut view = image.view_mut();
		backend.render(&prepared, &mut view).expect("a frame");
	}

	let white = pixel(&image, 2, 8);
	let black = pixel(&image, 13, 8);
	assert!(white[0] > 240 && white[1] > 240 && white[2] > 240, "limited-range 235 is white: {white:?}");
	assert!(black[0] < 20, "limited-range 16 is black: {black:?}");
	assert_eq!(white[3], 255, "a video frame is opaque");
	// AND THE EDGE IS WHERE THE SOURCE PUT IT, doubled by the scale rather than shifted by a
	// reconstruction that sited its chroma wrongly.
	assert!(pixel(&image, 7, 8)[0] > 240 && pixel(&image, 8, 8)[0] < 20, "the edge is in the middle: {:?} then {:?}", pixel(&image, 7, 8), pixel(&image, 8, 8));
}

#[test]
// THE FROZEN COVERAGE THRESHOLD, MEASURED AGAINST THE ANALYTIC AREA IN BOTH DIRECTIONS.
//
// `graphics-profile` freezes render2d antialiasing coverage at 2/255 absolute per pixel against the
// analytic area, with 1/255 mean over the covered region, and it freezes the edge policy: a pixel
// whose analytic coverage is exactly 0 or exactly 1 is compared EXACTLY. Nothing here had ever
// checked that, and the rasteriser did not meet it: it sampled the vertical direction at a sixteenth
// of a row, which puts a near-horizontal edge up to 8/255 out.
//
// AN AXIS-ALIGNED RECTANGLE IS THE SHAPE THE ANSWER IS KNOWN FOR. Its overlap with a pixel is the
// product of two one-dimensional overlaps - a number this test computes from the rectangle rather
// than from the rasteriser - so the comparison is against the geometry and not against a previous
// run. The offsets walk a whole pixel in both axes, so every phase of an edge against the grid is
// covered, and the fractional SIZE means the far edges are never at the same phase as the near ones.
fn coverage_meets_the_frozen_threshold_against_the_analytic_area() {
	const PER_PIXEL: f64 = 2.0 / 255.0;
	const MEAN: f64 = 1.0 / 255.0;
	let overlap = |low: f64, high: f64, from: f64, to: f64| (high.min(to) - low.max(from)).max(0.0);

	let mut worst = 0.0f64;
	let mut worst_at = (0u32, 0u32, 0.0f64);
	let mut total = 0.0f64;
	let mut partial = 0usize;
	for step in 0..9 {
		let offset = step as f64 / 8.0;
		let (x, y, width, height) = (3.0 + offset, 2.0 + offset, 5.5, 4.25);
		let mut image = target(16, 12);
		let mut canvas = Canvas::new();
		canvas.fill_path(rect_path(RectF::new(x as f32, y as f32, width as f32, height as f32)), Paint::Solid(Color::new(1.0, 1.0, 1.0, 1.0, ColorSpace::Srgb)), FillRule::NonZero).expect("a rectangle");
		draw(&canvas.finish().expect("a list"), &mut image);
		for row in 0..12u32 {
			for column in 0..16u32 {
				let area = overlap(column as f64, column as f64 + 1.0, x, x + width) * overlap(row as f64, row as f64 + 1.0, y, y + height);
				let drawn = pixel(&image, column, row)[3] as f64 / 255.0;
				let difference = (drawn - area).abs();
				if area <= 0.0 || area >= 1.0 {
					// A FULLY COVERED PIXEL THAT DIFFERS AT ALL is a colour error wearing an
					// antialiasing tolerance, so the frozen edge policy compares it exactly - which
					// for an eight-bit target means the stored byte.
					assert!(difference <= 0.5 / 255.0, "pixel ({column}, {row}) at offset {offset} has analytic coverage {area} and was drawn {drawn}");
					continue;
				}
				partial += 1;
				total += difference;
				if difference > worst {
					worst = difference;
					worst_at = (column, row, offset);
				}
			}
		}
	}
	assert!(partial > 0, "the sweep of offsets produced partially covered pixels to compare");
	assert!(worst <= PER_PIXEL, "the worst pixel differs by {worst} at {worst_at:?}, and the frozen per-pixel bound is {PER_PIXEL}");
	let mean = total / partial as f64;
	assert!(mean <= MEAN, "the mean difference over {partial} partially covered pixel(s) is {mean}, and the frozen bound is {MEAN}");
}

#[test]
// AND THE SAME FOR A SHAPE WHOSE EDGES ARE NOT AXIS-ALIGNED, where the vertical sampling used to be
// worst: a near-horizontal edge crossed a pixel row over many columns, and every one of them carried
// the same quantised value. A triangle with one shallow edge is that case.
//
// WHAT IS ASSERTED IS THE SUM, not each pixel: the analytic area of a triangle clipped to a pixel is
// a five-case polygon clip, and writing that here would be writing a second rasteriser to check the
// first. The total ink of the drawing is the triangle's own area, which is one multiplication - and a
// rasteriser that quantises a shallow edge loses it there, because the loss is systematic.
fn a_shallow_edge_carries_the_area_it_covers() {
	let mut worst = 0.0f64;
	for step in 0..9 {
		let offset = step as f64 / 8.0;
		let mut image = target(64, 16);
		let mut builder = PathBuilder::new();
		// A long, shallow triangle: eight pixels tall over fifty wide, so its upper edge crosses one
		// pixel row every six columns.
		builder.move_to(PointF { x: 4.0, y: (4.0 + offset) as f32 }).expect("a start");
		builder.line_to(PointF { x: 54.0, y: (4.0 + offset) as f32 }).expect("along the top");
		builder.line_to(PointF { x: 54.0, y: (12.0 + offset) as f32 }).expect("down the right");
		builder.close().expect("closed");
		let mut canvas = Canvas::new();
		canvas.fill_path(builder.finish(), Paint::Solid(Color::new(1.0, 1.0, 1.0, 1.0, ColorSpace::Srgb)), FillRule::NonZero).expect("a triangle");
		draw(&canvas.finish().expect("a list"), &mut image);
		let mut ink = 0.0f64;
		for row in 0..16u32 {
			for column in 0..64u32 {
				ink += pixel(&image, column, row)[3] as f64 / 255.0;
			}
		}
		let area = 50.0 * 8.0 / 2.0;
		let difference = (ink - area).abs();
		if difference > worst {
			worst = difference;
		}
		// HALF A CODE VALUE PER PIXEL IS THE FLOOR, and the triangle covers about four hundred of
		// them, so the whole drawing may be two hundredths of a pixel out per pixel and no more.
		assert!(difference <= 400.0 * 0.5 / 255.0, "at offset {offset} the triangle's ink is {ink} where its area is {area}");
	}
	assert!(worst < 1.0, "no offset loses a whole pixel of ink: the worst is {worst}");
}

#[test]
// A PIXEL NO COMMAND REACHES COMES BACK EXACTLY AS IT WAS, bytes and all, because "unchanged" is not
// a value that can be nearly right.
//
// THIS IS A CONTRACT AND NOT A PROOF OF THE OPTIMISATION BESIDE IT. Tiles with no commands are
// skipped now rather than decoded into the working space and re-encoded, and for THIS target the
// round trip was lossless anyway - so the skip is worth time and not pixels, and this fixture would
// pass either way. What it pins is the property a target whose round trip is NOT lossless would lose
// first, and the reason the skip is safe to make at all.
fn a_tile_no_command_reaches_is_left_byte_for_byte() {
	let mut image = target(64, 64);
	// A recognisable backdrop, including values that do not survive a careless round trip.
	{
		let mut view = image.view_mut();
		for y in 0..64u32 {
			for x in 0..64u32 {
				let row = view.row_mut(y).expect("a row");
				let at = x as usize * 4;
				row[at] = (x * 4) as u8;
				row[at + 1] = (y * 4) as u8;
				row[at + 2] = 1;
				row[at + 3] = 253;
			}
		}
	}
	let before: alloc::vec::Vec<u8> = (0..64u32).flat_map(|y| image.view().row(y).expect("a row").to_vec()).collect();

	// An empty list first: nothing anywhere.
	let empty = Canvas::new().finish().expect("an empty list");
	draw(&empty, &mut image);
	let after_empty: alloc::vec::Vec<u8> = (0..64u32).flat_map(|y| image.view().row(y).expect("a row").to_vec()).collect();
	assert_eq!(after_empty, before, "an empty draw list leaves every byte of the target alone");

	// Then a drawing in one corner: the tiles it does not reach are just as untouched.
	let mut canvas = Canvas::new();
	canvas.fill_path(rect_path(RectF::new(2.0, 2.0, 6.0, 6.0)), Paint::Solid(Color::new(1.0, 0.0, 0.0, 1.0, ColorSpace::Srgb)), FillRule::NonZero).expect("a corner");
	draw(&canvas.finish().expect("a list"), &mut image);
	assert_eq!(pixel(&image, 4, 4)[0], 255, "the corner was drawn");
	let far = pixel(&image, 60, 60);
	assert_eq!(far, [240, 240, 1, 253], "a pixel the drawing never reaches is exactly what it was: {far:?}");
}

#[test]
// THE SIX NODES THAT ARE NOT AN EFFECT WITH A NAME - a convolution, two morphologies, a displacement,
// a crop and a tile. Each is in the profile because the alternative is an application reaching for a
// backend it cannot have: a sharpen, a thickened outline, a ripple, a bounded effect and a pattern
// are all things a drawing does, and none of them is a blur.
fn the_general_filter_nodes_do_what_the_profile_says_they_do() {
	use render2d::filter::{Channel, FilterNode};

	// ONE OPAQUE SQUARE, eight pixels on a side, with its own layer around it - so that every
	// assertion below is about a shape whose extent is known exactly.
	let with_graph = |build: &dyn Fn(&mut render2d::filter::FilterGraph)| {
		let mut graph = render2d::filter::FilterGraph::default();
		build(&mut graph);
		let mut image = target(32, 32);
		let mut canvas = Canvas::new();
		let handle = canvas.resources().add_filter(graph).expect("a graph");
		canvas.begin_layer(None, 1.0, BlendMode::Normal, Some(handle)).expect("a filtered layer");
		canvas.fill_path(rect_path(RectF::new(12.0, 12.0, 8.0, 8.0)), red(), FillRule::NonZero).expect("a fill");
		canvas.end_layer().expect("closed");
		draw(&canvas.finish().expect("a list"), &mut image);
		image
	};

	// A CONVOLUTION WITH THE IDENTITY KERNEL IS THE IDENTITY, which is the check that distinguishes
	// "the node ran" from "the node did nothing": a node that returned its input passes every
	// assertion an effect makes about the middle of a shape.
	let identity = with_graph(&|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		let weights = [[0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.0]];
		graph.push(FilterNode::Convolution { input: source, weights, divisor: 1.0, bias: 0.0 }).expect("a convolution");
	});
	assert_eq!(
		pixel(&identity, 16, 16),
		pixel(
			&with_graph(&|graph| {
				graph.push(FilterNode::Source).expect("a node");
			}),
			16,
			16
		)
	);

	// AND AN EDGE-DETECT KERNEL IS ZERO WHERE THE PICTURE IS FLAT AND NOT ZERO AT AN EDGE, which is
	// a property of the KERNEL rather than of this implementation: its weights sum to zero.
	let edges = with_graph(&|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		let weights = [[0.0, -1.0, 0.0], [-1.0, 4.0, -1.0], [0.0, -1.0, 0.0]];
		// A DIVISOR OF ZERO MEANS THE SUM OF THE WEIGHTS, and this kernel's sum is zero - so the
		// stated fallback of one is what applies, and the test would fail with a division by zero if
		// the node had taken the sum literally.
		graph.push(FilterNode::Convolution { input: source, weights, divisor: 0.0, bias: 0.0 }).expect("a convolution");
	});
	assert_eq!(pixel(&edges, 16, 16)[3], 0, "the inside of a flat square has no edges in it");
	assert!(pixel(&edges, 12, 16)[3] > 0, "and its left edge does: {:?}", pixel(&edges, 12, 16));

	// DILATE GROWS THE SHAPE BY THE RADIUS AND ERODE SHRINKS IT BY THE SAME, which is the pair that
	// makes them worth having separately: an outline is the difference between the two.
	let dilated = with_graph(&|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		graph.push(FilterNode::MorphologyDilate { input: source, x: 2.0, y: 2.0 }).expect("a dilation");
	});
	assert_eq!(pixel(&dilated, 10, 16)[3], 255, "two pixels outside the shape is now inside it");
	assert_eq!(pixel(&dilated, 9, 16)[3], 0, "and three is still outside");
	let eroded = with_graph(&|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		graph.push(FilterNode::MorphologyErode { input: source, x: 2.0, y: 2.0 }).expect("an erosion");
	});
	assert_eq!(pixel(&eroded, 13, 16)[3], 0, "one pixel inside the edge is eroded away");
	assert_eq!(pixel(&eroded, 16, 16)[3], 255, "and the middle survives");

	// A DISPLACEMENT MAP OF FLAT HALF-GREY IS THE IDENTITY. That is what the minus a half in the
	// node's definition buys, and it is why a map can be a gradient, a noise field or a rendered
	// shape without the caller biasing it first.
	let undisplaced = with_graph(&|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		let map = graph.push(FilterNode::Flood { color: Color::new(0.5, 0.5, 0.5, 1.0, ColorSpace::SrgbLinear) }).expect("a flat map");
		graph.push(FilterNode::DisplacementMap { input: source, map, scale: 8.0, x_channel: Channel::Red, y_channel: Channel::Green }).expect("a displacement");
	});
	assert_eq!(pixel(&undisplaced, 16, 16)[3], 255, "a flat half-grey map moves nothing");
	assert_eq!(pixel(&undisplaced, 10, 16)[3], 0, "and nothing arrives from anywhere else");

	// A MAP AT ONE MOVES BY HALF THE SCALE, in the direction the channel names. `1.0 - 0.5` is a
	// half, so a scale of eight is a displacement of four - and reading the map at its stated channel
	// is what the two channel fields are for.
	let displaced = with_graph(&|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		let map = graph.push(FilterNode::Flood { color: Color::new(1.0, 0.5, 0.5, 1.0, ColorSpace::SrgbLinear) }).expect("a map");
		graph.push(FilterNode::DisplacementMap { input: source, map, scale: 8.0, x_channel: Channel::Red, y_channel: Channel::Green }).expect("a displacement");
	});
	assert_eq!(pixel(&displaced, 12, 16)[3], 255, "the shape is read from four pixels to the right, so it appears four to the left");
	assert_eq!(pixel(&displaced, 18, 16)[3], 0, "and its right edge has moved with it");

	// A CROP IS THE INPUT INSIDE A RECTANGLE AND NOTHING OUTSIDE IT, which is the cheap way to bound
	// an effect: the bounds map says so too, so what is outside is never computed.
	let cropped = with_graph(&|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		graph.push(FilterNode::Crop { input: source, rect: RectF::new(12.0, 12.0, 4.0, 8.0) }).expect("a crop");
	});
	assert_eq!(pixel(&cropped, 13, 16)[3], 255, "inside the crop the shape is there");
	assert_eq!(pixel(&cropped, 17, 16)[3], 0, "and outside it there is nothing");

	// A TILE REPEATS ONE RECTANGLE OVER THE WHOLE OUTPUT, wrapped about the RECTANGLE'S OWN origin -
	// so the tile that lands on the rectangle is the rectangle, and moving the pattern moves it
	// rather than reshuffling it.
	let tiled = with_graph(&|graph| {
		let source = graph.push(FilterNode::Source).expect("a node");
		graph.push(FilterNode::Tile { input: source, rect: RectF::new(12.0, 12.0, 8.0, 8.0) }).expect("a tile");
	});
	assert_eq!(pixel(&tiled, 16, 16)[3], 255, "the tile itself is unchanged");
	assert_eq!(pixel(&tiled, 24, 24)[3], 255, "and it repeats one period along both axes");
	assert_eq!(pixel(&tiled, 4, 4)[3], 255, "in the negative direction too, which a plain remainder gets wrong");

	// A TILE WITH NO AREA IS REFUSED WHERE IT IS BUILT. A period of zero is a loop that does not
	// advance, which is a frame that never finishes rather than a picture that is wrong.
	let mut degenerate = render2d::filter::FilterGraph::default();
	let source = degenerate.push(FilterNode::Source).expect("a node");
	assert!(matches!(degenerate.push(FilterNode::Tile { input: source, rect: RectF::new(0.0, 0.0, 0.0, 8.0) }), Err(render2d::Error::DegenerateShape { .. })));
}

#[test]
// A FILL OUTSIDE A CLIP'S BOUNDS IS STILL CLIPPED, and this is the case a tiled clip mask gets wrong:
// the mask is built over the clip's own rectangle, and a tile the mask does not cover has to mean
// NOTHING PASSES rather than everything does. A live capture of the 2D demo showed the far end of a
// scrolling column standing outside the rounded panel that was clipping it.
fn a_shape_beyond_the_clips_bounds_is_clipped_away() {
	// BIG ENOUGH TO SPAN SEVERAL TILES, which is the whole point: a clip mask built only for the
	// tiles the clip touches leaves every other tile with no mask at all, and "no mask" has to mean
	// nothing passes rather than everything does. A single-tile target cannot tell the two apart.
	let mut image = target(256, 256);
	let mut canvas = Canvas::new();
	canvas.save().expect("a save");
	let mut rounded = PathBuilder::new();
	rounded.add_rounded_rect(RectF::new(16.0, 16.0, 96.0, 64.0), 12.0, 12.0).expect("a rounded rectangle");
	canvas.set_clip(rounded.finish(), FillRule::NonZero).expect("a clip");
	// One rectangle inside the clip and one well below it, in the same drawing.
	canvas.fill_path(rect_path(RectF::new(32.0, 32.0, 48.0, 24.0)), red(), FillRule::NonZero).expect("inside");
	canvas.fill_path(rect_path(RectF::new(32.0, 200.0, 48.0, 24.0)), red(), FillRule::NonZero).expect("three tile rows below");
	canvas.restore().expect("a restore");
	draw(&canvas.finish().expect("a list"), &mut image);

	assert_eq!(pixel(&image, 40, 40)[3], 255, "the shape inside the clip is drawn: {:?}", pixel(&image, 40, 40));
	assert_eq!(pixel(&image, 40, 208)[3], 0, "and the one three tile rows beyond the clip is not: {:?}", pixel(&image, 40, 208));
	assert_eq!(pixel(&image, 40, 120)[3], 0, "nor anything between them: {:?}", pixel(&image, 40, 120));
}

#[test]
// A TILE SOMETHING OVERWRITES WHOLE IS NOT DECODED OUT OF THE TARGET FIRST, and the hazard in that
// is exactly one thing: a tile wrongly believed covered shows the PREVIOUS TILE'S pixels, because
// the scratch is reused across tiles and nothing put this tile's backdrop into it.
//
// EVERY CASE HERE IS A WAY THE COVER CAN BE WRONG. A shape that is not a rectangle, a paint that is
// not opaque, a blend that reads the backdrop, a clip that narrows the fill to less than the tile,
// and a layer that sends it somewhere else - each would leave a tile partly unwritten, and each
// keeps its decode. The proof in every case is the same: what was in the target before the frame is
// still visible where the drawing did not reach.
fn a_tile_an_opaque_fill_covers_needs_no_backdrop_and_the_others_still_have_one() {
	let tile = crate::TILE_SIZE;
	let backdrop = [0x00u8, 0x00, 0xff, 0xff];
	// THE MECHANISM IS ASSERTED AND NOT ONLY ITS PIXELS. A frame whose output is right because the
	// fast path fired correctly and one whose output is right because it never fired at all look
	// identical in the target, and the second is what a disabled optimisation looks like for ever.
	let skipped = |list: &DrawList, image: &OwnedImage| -> usize {
		let mut backend = Soft2d::new();
		backend.prepare(list, &description(image)).expect("a preparation").tiles_without_backdrop()
	};
	let prefill = |image: &mut OwnedImage| {
		for y in 0..image.layout().extent.height {
			let row = image.view_mut().row_mut(y).expect("a row").to_vec();
			let mut painted = row;
			for pixel in painted.chunks_mut(4) {
				pixel.copy_from_slice(&backdrop);
			}
			image.view_mut().row_mut(y).expect("a row").copy_from_slice(&painted);
		}
	};

	// 1. A FULL-TARGET OPAQUE RECTANGLE. Every tile is covered, every decode is skipped, and every
	//    pixel is the fill - which is what says the skipped decode did not lose the frame.
	let mut image = target(tile * 2, tile * 2);
	prefill(&mut image);
	let mut canvas = Canvas::new();
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, (tile * 2) as f32, (tile * 2) as f32)), red(), FillRule::NonZero).expect("a fill");
	let list = canvas.finish().expect("a list");
	assert_eq!(skipped(&list, &image), 4, "all four tiles are covered whole");
	draw(&list, &mut image);
	assert_eq!(pixel(&image, 0, 0), [0xff, 0x00, 0x00, 0xff], "the covering fill reaches the first tile");
	assert_eq!(pixel(&image, tile * 2 - 1, tile * 2 - 1), [0xff, 0x00, 0x00, 0xff], "and the last one");

	// 2. A RECTANGLE COVERING ONE TILE OF FOUR. The covered tile takes the fill; the other three
	//    keep their backdrop, which is the half a wrongly skipped decode destroys.
	let mut image = target(tile * 2, tile * 2);
	prefill(&mut image);
	let mut canvas = Canvas::new();
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, tile as f32, tile as f32)), red(), FillRule::NonZero).expect("a fill");
	let list = canvas.finish().expect("a list");
	assert_eq!(skipped(&list, &image), 1, "one tile of the four is covered whole");
	draw(&list, &mut image);
	assert_eq!(pixel(&image, tile / 2, tile / 2), [0xff, 0x00, 0x00, 0xff], "the covered tile is the fill");
	assert_eq!(pixel(&image, tile + tile / 2, tile / 2), backdrop, "and the tile beside it still holds what was under it");
	assert_eq!(pixel(&image, tile / 2, tile + tile / 2), backdrop, "and the one below it");
	assert_eq!(pixel(&image, tile + tile / 2, tile + tile / 2), backdrop, "and the one diagonally from it");

	// 3. A HALF-TRANSPARENT FILL COVERS NOTHING, however rectangular it is: the result depends on
	//    the backdrop, so the backdrop has to have been read.
	let mut image = target(tile, tile);
	prefill(&mut image);
	let mut canvas = Canvas::new();
	let translucent = Paint::Solid(Color::new(1.0, 0.0, 0.0, 0.5, ColorSpace::Srgb));
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, tile as f32, tile as f32)), translucent, FillRule::NonZero).expect("a fill");
	let list = canvas.finish().expect("a list");
	assert_eq!(skipped(&list, &image), 0, "a half-transparent fill covers nothing, so every tile keeps its decode");
	draw(&list, &mut image);
	let blended = pixel(&image, tile / 2, tile / 2);
	assert!(blended[0] > 0x40 && blended[2] > 0x40, "half of the fill over half of the backdrop, not either alone: {blended:?}");

	// 4. A CLIPPED FILL COVERS NOTHING THIS CAN PROVE. The clip could narrow it to less than the
	//    tile - here it does - so the tile keeps its decode and the backdrop survives outside it.
	let mut image = target(tile, tile);
	prefill(&mut image);
	let mut canvas = Canvas::new();
	canvas.save().expect("a save");
	canvas.set_clip(rect_path(RectF::new(0.0, 0.0, (tile / 2) as f32, tile as f32)), FillRule::NonZero).expect("a clip");
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, tile as f32, tile as f32)), red(), FillRule::NonZero).expect("a fill");
	canvas.restore().expect("a restore");
	let list = canvas.finish().expect("a list");
	assert_eq!(skipped(&list, &image), 0, "a clipped fill covers nothing this can prove, so every tile keeps its decode");
	draw(&list, &mut image);
	assert_eq!(pixel(&image, tile / 4, tile / 2), [0xff, 0x00, 0x00, 0xff], "inside the clip is the fill");
	assert_eq!(pixel(&image, tile - 1, tile / 2), backdrop, "and outside it the backdrop is still there");

	// 5. A FILL INSIDE A LAYER GOES TO THE LAYER and not to the tile, so the tile keeps its decode.
	//    With the layer at half opacity the result is a blend, which is only right if the backdrop
	//    was read.
	let mut image = target(tile, tile);
	prefill(&mut image);
	let mut canvas = Canvas::new();
	canvas.begin_layer(None, 0.5, BlendMode::Normal, None).expect("a layer");
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, tile as f32, tile as f32)), red(), FillRule::NonZero).expect("a fill");
	canvas.end_layer().expect("the layer closes");
	let list = canvas.finish().expect("a list");
	assert_eq!(skipped(&list, &image), 0, "a fill inside a layer goes to the layer, so every tile keeps its decode");
	draw(&list, &mut image);
	let layered = pixel(&image, tile / 2, tile / 2);
	assert!(layered[0] > 0x40 && layered[2] > 0x40, "a half-opaque layer over the backdrop, not the fill alone: {layered:?}");

	// 6. A ROTATED RECTANGLE IS NOT AN AXIS-ALIGNED ONE, and its corners leave the tile's corners
	//    untouched - which is the geometric version of the same mistake.
	let mut image = target(tile, tile);
	prefill(&mut image);
	let mut canvas = Canvas::new();
	canvas.save().expect("a save");
	// A ROTATION BY HAND, because the transform type carries the matrix and not a constructor per
	// shape of it. Thirty degrees is enough that no edge stays axis-aligned.
	let (sine, cosine) = (0.5f32, 0.866_025_4f32);
	canvas.concat_transform(&render2d::transform::Transform { m: [[cosine, -sine, 0.0], [sine, cosine, 0.0], [0.0, 0.0, 1.0]] });
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, tile as f32, tile as f32)), red(), FillRule::NonZero).expect("a fill");
	canvas.restore().expect("a restore");
	let list = canvas.finish().expect("a list");
	assert_eq!(skipped(&list, &image), 0, "a rotated rectangle is not an axis-aligned one, so every tile keeps its decode");
	draw(&list, &mut image);
	assert_eq!(pixel(&image, tile - 1, 0), backdrop, "a corner the rotated rectangle does not reach still holds the backdrop");
}

#[test]
// THE ROUND TRIP COVERS WHAT CAN CHANGE, AND EVERYTHING ELSE IS LEFT ALONE.
//
// A tile is decoded out of the target and encoded back into it, and for a drawing that touches three
// pixels of a tile that is sixty-four rows of conversion for nothing. The region is bounded by the
// commands binned to the tile - which `prepare` already knows - so the pixels outside it are neither
// read nor written.
//
// WHAT COULD GO WRONG IS THE WHOLE POINT OF THE FIXTURE: a region computed too SMALL loses part of a
// drawing, and a region computed too LARGE is only slow. So this asserts both halves - the drawing
// arrives whole, and the target outside it is BYTE-IDENTICAL to what was there before, which is what
// says the narrowed round trip is not silently re-encoding pixels it should not have touched.
fn only_the_part_of_a_tile_that_can_change_makes_the_round_trip() {
	let tile = crate::TILE_SIZE;
	let mut image = target(tile, tile);
	// A GRADIENT RATHER THAN A FLAT COLOUR, so a pixel written back from a different place in the
	// tile is a different value and is caught.
	for y in 0..tile {
		let mut view = image.view_mut();
		let row = view.row_mut(y).expect("a row");
		for (x, pixel) in row.chunks_mut(4).enumerate() {
			pixel.copy_from_slice(&[(x as u8).wrapping_mul(3), (y as u8).wrapping_mul(5), 0x40, 0xff]);
		}
	}
	let before: Vec<[u8; 4]> = (0..tile).flat_map(|y| (0..tile).map(move |x| (x, y))).map(|(x, y)| pixel(&image, x, y)).collect();

	// One small opaque rectangle in the middle of the tile.
	let mut canvas = Canvas::new();
	let (left, top, size) = (20.0f32, 24.0f32, 8.0f32);
	canvas.fill_path(rect_path(RectF::new(left, top, size, size)), red(), FillRule::NonZero).expect("a fill");
	draw(&canvas.finish().expect("a list"), &mut image);

	// THE DRAWING ARRIVED WHOLE - a region computed too small would clip it.
	assert_eq!(pixel(&image, 20, 24), [0xff, 0x00, 0x00, 0xff], "its first pixel");
	assert_eq!(pixel(&image, 27, 31), [0xff, 0x00, 0x00, 0xff], "and its last");

	// AND EVERYTHING OUTSIDE IT IS EXACTLY WHAT IT WAS. Not "close enough": the decode and the encode
	// are a lossy pair for some formats, so a pixel that made the trip without needing to is a pixel
	// that may come back different - which is how a redraw of one corner comes to change a whole
	// tile by a least significant bit.
	for y in 0..tile {
		for x in 0..tile {
			if (20..28).contains(&x) && (24..32).contains(&y) {
				continue;
			}
			let expected = before[(y * tile + x) as usize];
			assert_eq!(pixel(&image, x, y), expected, "the target outside the drawing is untouched at ({x}, {y})");
		}
	}
}
