//! THE SOFT2D PERFORMANCE FLOOR, measured headlessly.
//!
//! IT NEEDS NO SURFACE, NO DisplayService, NO GUEST AND NO APPLICATION. Four bounded `DrawList`
//! fixtures are recorded once and replayed into an `OwnedImage`, which is what makes this a number a
//! person can get in a second on a host rather than a boot away - and what stops the live application
//! becoming a prerequisite of the backend's own floor.
//!
//! THE FIXTURES ARE FROZEN AND THIS PROGRAM CHECKS THAT THEY ARE. Each one's command count, resource
//! count and extent are asserted against the numbers recorded here, so a later simplification cannot
//! quietly lower the workload and report the same milliseconds against an easier scene. A benchmark
//! whose workload can drift measures the workload and not the renderer.
//!
//! PREPARATION AND REPLAY ARE REPORTED SEPARATELY, because they are different claims: `prepare`
//! flattens, strokes, bins and reserves once, and `render` is what a repeated frame costs. The floor
//! is on the REPLAY, which is the frame.

use std::time::Instant;

use graphics_core::geom::{Extent2D, PointF, RectF};
use graphics_core::layout::{ImageLayout, RowOrigin};
use graphics_core::pixel::OutputLuminance;
use graphics_core::semantics::ImageSemantics;
use graphics_core::{AlphaMode, ColorSpace, ImageView, OwnedImage, PixelFormat, PixelStorage};
use render2d::backend::{Backend, TargetDescription};
use render2d::blend::{BlendMode, Operator};
use render2d::filter::{FilterGraph, FilterNode};
use render2d::list::{DrawList, ImageRecord};
use render2d::paint::{Color, GradientStop, ImageQuality, Paint};
use render2d::path::{Cap, FillRule, Join, PathBuilder, StrokeStyle};
use render2d::transform::Transform;
use render2d::{Canvas, Error};
use soft2d::{ImageSource, Soft2d};

/// THE MEASURED EXTENT. The profile's own benchmark size, so two measurements are comparable.
const WIDTH: u32 = 640;
const HEIGHT: u32 = 480;

/// How many frames are thrown away before measuring, and how many are kept.
///
/// THE WARMUP IS NOT A COURTESY. The first replay of a list touches every page of the reservation and
/// pulls the target into cache; including it would measure the allocator's first-touch cost once and
/// divide it into every sample.
const WARMUP: usize = 5;
const SAMPLES: usize = 30;

/// One scene's frozen shape and its budget.
struct Scene {
	name: &'static str,
	/// The ceiling, in milliseconds. FIXED INDEPENDENTLY OF THE IMPLEMENTATION: sixty and fifteen
	/// frames a second are what a user interface and a heavy scene respectively have to hold.
	ceiling_ms: f64,
	/// The frozen budget: the first accepted measurement or the ceiling, whichever is lower. IT MAY
	/// ONLY EVER BE LOWERED - raising it is how a regression becomes the new normal.
	budget_ms: f64,
	commands: usize,
	resources: usize,
}

/// The scenes, with the frozen counts a fixture is checked against.
///
/// `image-stress` WAS ONE SCENE AND IS TWO (2026-09-19), on the project owner's answer to the
/// question the milestone put to them: it mixed two workloads that have nothing to do with each
/// other and reported one number for both. RESAMPLING is what a pyramid and the three filters cost -
/// eight large downscales and an upscale at each quality, over one source, source space and target
/// space the same. COLOUR CONVERSION is what a wide-gamut source into an sRGB target and a YUV
/// source from its planes cost, and two of its three kinds of draw cover the WHOLE FRAME. One number
/// over the two says which of them is slow only by accident of how they were summed.
const SCENES: [Scene; 5] = [
	Scene { name: "UI-basic", ceiling_ms: 16.7, budget_ms: 16.7, commands: 252, resources: 153 },
	Scene { name: "UI-effects", ceiling_ms: 66.7, budget_ms: 66.7, commands: 45, resources: 34 },
	Scene { name: "vector-stress", ceiling_ms: 66.7, budget_ms: 66.7, commands: 240, resources: 241 },
	// BOTH CEILINGS ARE INHERITED FROM THE SCENE THESE CAME OUT OF AND NEITHER IS A NEW ANSWER. The
	// owner was asked whether a video scene's ceiling is 60 Hz at this content or whether the scene
	// is two scenes, and answered the second; that settles the split and leaves the first question
	// open for `image-convert`, which is the half that draws two full frames through a transfer
	// function and a matrix. `image-resample` at 16.7 is not in doubt - UI imagery at UI sizes is a
	// per-frame cost.
	Scene { name: "image-resample", ceiling_ms: 16.7, budget_ms: 16.7, commands: 11, resources: 1 },
	Scene { name: "image-convert", ceiling_ms: 16.7, budget_ms: 16.7, commands: 14, resources: 2 },
];

/// The images the scenes reference, under their recorded identities.
struct Images {
	photo: OwnedImage,
	icon: OwnedImage,
	wide: OwnedImage,
	/// THE YUV SOURCE the image scene names: planes, as a decoder hands them over, drawn without
	/// being converted to RGBA first.
	video: VideoFrame,
}

/// One `NV12` frame, generated from its coordinates so it is the same on every machine.
struct VideoFrame {
	layout: graphics_core::planar::MultiPlaneLayout,
	luma: Vec<u8>,
	chroma: Vec<u8>,
}

impl VideoFrame {
	fn new(extent: Extent2D) -> Self {
		use graphics_core::planar::{MultiPlaneLayout, PlanarFormat, YuvMatrix, YuvRange};
		let (width, height) = (extent.width as usize, extent.height as usize);
		let mut luma = vec![0u8; width * height];
		for y in 0..height {
			for x in 0..width {
				luma[y * width + x] = (16 + ((x + y) % 220)) as u8;
			}
		}
		let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
		let mut chroma = vec![128u8; chroma_width * 2 * chroma_height];
		for y in 0..chroma_height {
			for x in 0..chroma_width {
				chroma[y * chroma_width * 2 + x * 2] = (64 + (x % 128)) as u8;
				chroma[y * chroma_width * 2 + x * 2 + 1] = (64 + (y % 128)) as u8;
			}
		}
		let layout = MultiPlaneLayout::new(extent, PlanarFormat::Nv12, YuvMatrix::Bt709, YuvRange::Limited, ColorSpace::Rec2020, [width as u32, chroma_width as u32 * 2, 0]).expect("a layout");
		Self { layout, luma, chroma }
	}
}

impl ImageSource for Images {
	fn image(&self, identity: u64) -> Option<ImageView<'_>> {
		match identity {
			1 => Some(self.photo.view()),
			2 => Some(self.icon.view()),
			3 => Some(self.wide.view()),
			_ => None,
		}
	}

	fn planes(&self, identity: u64) -> Option<graphics_core::planar::MultiPlaneView<'_>> {
		(identity == 4).then(|| graphics_core::planar::MultiPlaneView::new(self.video.layout, [&self.video.luma, &self.video.chroma, &[]]).expect("a view"))
	}
}

fn main() {
	let check = std::env::args().any(|argument| argument == "--check");
	let images = build_images();
	if std::env::var("SOFT2D_BENCH_PROBE").is_ok() {
		println!("probe\tcommands\tprepare_ms\treplay_median_ms");
		for (name, list) in probes() {
			let mut target = target();
			let description = TargetDescription { extent: Extent2D::new(WIDTH, HEIGHT), format: PixelFormat::B8G8R8A8Unorm, color_space: ColorSpace::Srgb, scale: 1.0, luminance: OutputLuminance::UNKNOWN };
			let mut backend = Soft2d::new().with_images(&images);
			let started = Instant::now();
			let prepared = backend.prepare(&list, &description).expect("a preparation");
			let prepare_ms = started.elapsed().as_secs_f64() * 1_000.0;
			let mut samples = Vec::with_capacity(SAMPLES);
			for index in 0..WARMUP + SAMPLES {
				let started = Instant::now();
				{
					let mut view = target.view_mut();
					backend.render(&prepared, &mut view).expect("a frame");
				}
				let elapsed = started.elapsed().as_secs_f64() * 1_000.0;
				if index >= WARMUP {
					samples.push(elapsed);
				}
			}
			samples.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
			println!("{name}\t{}\t{prepare_ms:.3}\t{:.3}", list.commands().len(), samples[samples.len() / 2]);
		}
		return;
	}
	println!("scene\tcommands\tresources\tprepare_ms\treplay_median_ms\treplay_p99_ms\tbudget_ms\tceiling_ms\tverdict");
	let mut failed = false;
	for scene in SCENES.iter() {
		let list = match scene.name {
			"UI-basic" => ui_basic(),
			"UI-effects" => ui_effects(),
			"vector-stress" => vector_stress(),
			"image-resample" => image_resample(),
			_ => image_convert(),
		}
		.expect("a fixture this program records");
		// THE FROZEN SHAPE IS CHECKED BEFORE THE CLOCK STARTS. A fixture that lost half its commands
		// would otherwise report a comfortable number against a scene nobody agreed to.
		if std::env::var("SOFT2D_BENCH_FREEZE").is_ok() {
			// FREEZING IS AN EXPLICIT ACT. The counts below are what the scenes are, and this prints
			// them so the constants above can be written from a run rather than guessed - it never
			// rewrites them itself, because a benchmark that updates its own workload has none.
			eprintln!("freeze: {} commands={} resources={}", scene.name, list.commands().len(), resource_count(&list));
		} else {
			assert_eq!(list.commands().len(), scene.commands, "{}: the frozen command count changed", scene.name);
			assert_eq!(resource_count(&list), scene.resources, "{}: the frozen resource count changed", scene.name);
		}

		let mut target = target();
		let description = TargetDescription { extent: Extent2D::new(WIDTH, HEIGHT), format: PixelFormat::B8G8R8A8Unorm, color_space: ColorSpace::Srgb, scale: 1.0, luminance: OutputLuminance::UNKNOWN };
		let mut backend = Soft2d::new().with_images(&images);
		let started = Instant::now();
		let prepared = backend.prepare(&list, &description).expect("a preparation");
		let prepare_ms = started.elapsed().as_secs_f64() * 1_000.0;

		let mut samples = Vec::with_capacity(SAMPLES);
		for index in 0..WARMUP + SAMPLES {
			let started = Instant::now();
			{
				let mut view = target.view_mut();
				backend.render(&prepared, &mut view).expect("a frame");
			}
			let elapsed = started.elapsed().as_secs_f64() * 1_000.0;
			if index >= WARMUP {
				samples.push(elapsed);
			}
		}
		samples.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
		let median = samples[samples.len() / 2];
		// THE NINETY-NINTH PERCENTILE AND NOT THE MAXIMUM. One sample interrupted by the scheduler is
		// not the renderer's cost, and a floor set on a maximum is a floor set on the busiest machine
		// that ever ran it.
		let percentile = samples[((samples.len() as f64 * 0.99) as usize).min(samples.len() - 1)];
		let allowed = scene.budget_ms.min(scene.ceiling_ms);
		let met = median <= allowed;
		failed |= check && !met;
		println!("{}\t{}\t{}\t{prepare_ms:.3}\t{median:.3}\t{percentile:.3}\t{:.1}\t{:.1}\t{}", scene.name, scene.commands, scene.resources, scene.budget_ms, scene.ceiling_ms, if met { "met" } else { "OVER" });
	}
	if failed {
		eprintln!("soft2d-bench: a scene is over its frozen budget");
		std::process::exit(1);
	}
}

/// WHERE THE TIME GOES, AS SCENES SMALL ENOUGH TO NAME. `SOFT2D_BENCH_PROBE=1` runs these instead of
/// the frozen four.
///
/// THE FROZEN SCENES SAY WHETHER THE FLOOR IS MET AND NOT WHY IT IS NOT. Each of these isolates one
/// term of the per-frame cost at the same extent, so the difference between two rows is one thing:
/// an empty list is the tile round trip and nothing else, one opaque full-screen rectangle adds a
/// composite that reads no backdrop, a gradient one adds a shader evaluated per pixel, and the small
/// rectangles add the per-command overhead that a real interface is mostly made of. Nothing here is
/// a budget and nothing here is frozen - this is a measuring instrument, and the scenes it measures
/// are chosen to be subtracted from each other.
fn probes() -> Vec<(&'static str, DrawList)> {
	let mut out: Vec<(&'static str, DrawList)> = Vec::new();

	let empty = Canvas::new().finish().expect("an empty list");
	out.push(("empty", empty));

	let mut canvas = Canvas::new();
	rect(&mut canvas, RectF::new(0.0, 0.0, WIDTH as f32, HEIGHT as f32), Paint::Solid(Color::new(0.2, 0.3, 0.4, 1.0, ColorSpace::Srgb))).expect("a fill");
	out.push(("one-opaque-fullscreen", canvas.finish().expect("a list")));

	let mut canvas = Canvas::new();
	rect(&mut canvas, RectF::new(0.0, 0.0, WIDTH as f32, HEIGHT as f32), Paint::Solid(Color::new(0.2, 0.3, 0.4, 0.5, ColorSpace::Srgb))).expect("a fill");
	out.push(("one-translucent-fullscreen", canvas.finish().expect("a list")));

	// THE SAME FILL AS FOUR RECTANGLES, EACH A QUARTER OF THE FRAME. Same pixels, same colour, same
	// covered path - and four times the per-command and per-edge work. Subtracting the one-rectangle
	// row from this one says what a COMMAND costs against what a PIXEL costs, which is the question
	// every hypothesis about the forty-seven nanoseconds has needed and none has had an answer to.
	let mut canvas = Canvas::new();
	let (half_w, half_h) = (WIDTH as f32 / 2.0, HEIGHT as f32 / 2.0);
	for (x, y) in [(0.0, 0.0), (half_w, 0.0), (0.0, half_h), (half_w, half_h)] {
		rect(&mut canvas, RectF::new(x, y, half_w, half_h), Paint::Solid(Color::new(0.2, 0.3, 0.4, 1.0, ColorSpace::Srgb))).expect("a fill");
	}
	out.push(("four-opaque-quarters", canvas.finish().expect("a list")));

	// THE SAME PIXELS AGAIN, AS ONE RECTANGLE DRAWN FOUR TIMES over the whole frame. Four times the
	// pixels and four times the commands, against the row above which is four times the commands and
	// the SAME pixels: the pair separates the two terms rather than leaving them multiplied together.
	let mut canvas = Canvas::new();
	for _ in 0..4 {
		rect(&mut canvas, RectF::new(0.0, 0.0, WIDTH as f32, HEIGHT as f32), Paint::Solid(Color::new(0.2, 0.3, 0.4, 1.0, ColorSpace::Srgb))).expect("a fill");
	}
	out.push(("four-opaque-fullscreen", canvas.finish().expect("a list")));

	// THE TOP HALF OF EVERY TILE, which touches every tile the full-screen fill touches and dirties
	// half of each. IF THE FLUSH IS BOUNDED BY THE DIRTY RECTANGLE this costs about half of the
	// full-screen row; if it is bounded by the TILE it costs the same, and those are different
	// defects with different fixes. One rectangle per tile row, full width, half a tile tall.
	let mut canvas = Canvas::new();
	let mut top = 0u32;
	while top < HEIGHT {
		rect(&mut canvas, RectF::new(0.0, top as f32, WIDTH as f32, 32.0), Paint::Solid(Color::new(0.2, 0.3, 0.4, 1.0, ColorSpace::Srgb))).expect("a fill");
		top += 64;
	}
	out.push(("half-of-every-tile", canvas.finish().expect("a list")));

	// A HUNDRED SMALL RECTANGLES, which is what a user interface is: the per-command cost times the
	// number of commands, over an area that is a fraction of the frame.
	let mut canvas = Canvas::new();
	for index in 0..100u32 {
		let x = 8.0 + ((index % 10) as f32) * 62.0;
		let y = 8.0 + ((index / 10) as f32) * 46.0;
		rect(&mut canvas, RectF::new(x, y, 54.0, 38.0), Paint::Solid(Color::new(0.8, 0.4, 0.2, 1.0, ColorSpace::Srgb))).expect("a fill");
	}
	out.push(("hundred-small-opaque", canvas.finish().expect("a list")));

	// THE SAME HUNDRED, HALF TRANSPARENT, so the difference is the backdrop read and the blend.
	let mut canvas = Canvas::new();
	for index in 0..100u32 {
		let x = 8.0 + ((index % 10) as f32) * 62.0;
		let y = 8.0 + ((index / 10) as f32) * 46.0;
		rect(&mut canvas, RectF::new(x, y, 54.0, 38.0), Paint::Solid(Color::new(0.8, 0.4, 0.2, 0.5, ColorSpace::Srgb))).expect("a fill");
	}
	out.push(("hundred-small-translucent", canvas.finish().expect("a list")));

	// ONE PIXEL IN EVERY TILE, which pays the tile round trip for the whole frame and draws almost
	// nothing. Subtracting it from a scene that covers the frame separates what the TILES cost from
	// what the DRAWING costs, and the two have been guessed at long enough.
	let mut canvas = Canvas::new();
	for row in 0..(HEIGHT / 64 + 1) {
		for column in 0..(WIDTH / 64 + 1) {
			rect(&mut canvas, RectF::new((column * 64) as f32, (row * 64) as f32, 1.0, 1.0), Paint::Solid(Color::new(0.8, 0.4, 0.2, 1.0, ColorSpace::Srgb))).expect("a fill");
		}
	}
	out.push(("dot-per-tile", canvas.finish().expect("a list")));

	// THE IMAGE SCENE, TAKEN APART. It is twenty-one times its ceiling and the item calls that a
	// fixture question; before agreeing, the four things it draws are measured one at a time, because
	// the last time a scene was assumed to be inherently expensive the answer was a software square
	// root. Each of these is one full-screen draw of one kind.
	let photo = ImageRecord { identity: 1, layout_generation: 1, content_generation: 1 };
	let wide = ImageRecord { identity: 3, layout_generation: 1, content_generation: 1 };
	let video = ImageRecord { identity: 4, layout_generation: 1, content_generation: 1 };
	for (name, record, source, quality) in [
		("image-photo-bilinear", photo, 512.0f32, ImageQuality::Bilinear),
		("image-photo-mipmapped", photo, 512.0, ImageQuality::Mipmapped),
		("image-photo-bicubic", photo, 512.0, ImageQuality::Bicubic),
		("image-widegamut-bilinear", wide, 256.0, ImageQuality::Bilinear),
		("image-yuv-bilinear", video, 320.0, ImageQuality::Bilinear),
	] {
		let mut canvas = Canvas::new();
		canvas.draw_image(record, RectF::new(0.0, 0.0, source, source), RectF::new(0.0, 0.0, WIDTH as f32, HEIGHT as f32), quality).expect("an image draw");
		out.push((name, canvas.finish().expect("a list")));
	}

	// THE VECTOR SCENE, TAKEN APART. Its strokes are drawn with a LINEAR GRADIENT, so a frame pays a
	// rasteriser and a per-pixel shader together and the frozen scene cannot say which. The same
	// geometry is drawn with each paint: the difference between the two rows is the shader.
	for (name, gradient) in [("strokes-solid", false), ("strokes-gradient", true)] {
		let mut canvas = Canvas::new();
		let paint = if gradient {
			let stops = canvas
				.resources()
				.add_stops(vec![
					GradientStop { offset: 0.0, color: Color::new(0.9, 0.2, 0.1, 1.0, ColorSpace::Srgb) },
					GradientStop { offset: 0.5, color: Color::new(0.95, 0.8, 0.1, 1.0, ColorSpace::Srgb) },
					GradientStop { offset: 1.0, color: Color::new(0.1, 0.4, 0.9, 1.0, ColorSpace::Srgb) },
				])
				.expect("stops");
			Paint::Linear { from: PointF { x: 0.0, y: 0.0 }, to: PointF { x: WIDTH as f32, y: HEIGHT as f32 }, stops, spread: graphics_core::sample::Spread::Clamp, transform: Transform::IDENTITY }
		} else {
			Paint::Solid(Color::new(0.9, 0.2, 0.1, 1.0, ColorSpace::Srgb))
		};
		let style = StrokeStyle { width: 2.5, cap: Cap::Round, join: Join::Miter, miter_limit: 4.0, ..StrokeStyle::default() };
		for index in 0..120 {
			let phase = index as f32 * 0.11;
			let mut builder = PathBuilder::new();
			builder.move_to(PointF { x: 10.0 + phase * 4.0, y: 20.0 + (index % 20) as f32 * 22.0 }).expect("a move");
			for segment in 0..6 {
				let x = 10.0 + phase * 4.0 + segment as f32 * 100.0;
				let y = 20.0 + (index % 20) as f32 * 22.0;
				builder.cubic_to(PointF { x: x + 30.0, y: y - 40.0 }, PointF { x: x + 70.0, y: y + 40.0 }, PointF { x: x + 100.0, y }).expect("a curve");
			}
			canvas.stroke_path(builder.finish(), paint, style).expect("a stroke");
		}
		out.push((name, canvas.finish().expect("a list")));
	}

	// THE EFFECTS SCENE, TAKEN APART, which is the one this item has twice recorded as having NO
	// DOMINANT TERM - 49 ms of blur and a hundred more spread over forty-five commands with nothing
	// separating them. These five rows are that separation: each is a piece of `ui_effects` and
	// nothing else, so a difference between two of them is one term.
	//
	//   effects-base            the full-screen fill alone
	//   effects-image           the fill plus the 512-square image draw
	//   effects-layers-plain    the fill plus the ten layers WITHOUT their filter graphs
	//   effects-layers-filtered the same ten layers WITH them - the pair is what a filter costs
	//   effects-backdrop        the frosted panel alone, which is the one backdrop blur
	//
	// THE PAIR THAT MATTERS IS THE THIRD AND FOURTH. A layer is an allocation, two rectangles and a
	// composite back whether or not a filter runs over it, so subtracting them separates the FILTER
	// from the LAYER MACHINERY - and the item's reading so far has assumed the blur is the cost
	// without any row able to say so.
	for stage in [
		"effects-base",
		"effects-image",
		"effects-layers-plain",
		"effects-layers-blur",
		"effects-layers-blur-x",
		"effects-layers-blur-y",
		"effects-layers-filtered",
		"effects-backdrop",
	] {
		let mut canvas = Canvas::new();
		if stage != "effects-backdrop" {
			rect(&mut canvas, RectF::new(0.0, 0.0, WIDTH as f32, HEIGHT as f32), Paint::Solid(Color::new(0.2, 0.3, 0.5, 1.0, ColorSpace::Srgb))).expect("a backdrop");
		}
		if stage == "effects-image" {
			canvas.draw_image(ImageRecord { identity: 1, layout_generation: 1, content_generation: 1 }, RectF::new(0.0, 0.0, 512.0, 512.0), RectF::new(0.0, 0.0, WIDTH as f32, HEIGHT as f32), ImageQuality::Bilinear).expect("an image");
		}
		if stage.starts_with("effects-layers") {
			for index in 0..10 {
				let x = 20.0 + (index % 5) as f32 * 120.0;
				let y = 40.0 + (index / 5) as f32 * 200.0;
				// THE GRAPH IS THE SAME FIVE NODES, OR ONLY ITS BLUR, OR NOTHING. Three rows rather
				// than two, because "the filter costs 78 ms" does not say whether that is the
				// Gaussian - which the profile fixes and which therefore cannot be made cheaper
				// without changing what this system draws - or the four nodes around it, which it
				// does not fix and which can.
				let filter = match stage {
					"effects-layers-filtered" => {
						let mut graph = FilterGraph::default();
						let source = graph.push(FilterNode::Source).expect("a source");
						let blur = graph.push(FilterNode::Blur { input: source, x: 4.0, y: 4.0 }).expect("a blur");
						let offset = graph.push(FilterNode::Offset { input: blur, dx: 3.0, dy: 5.0 }).expect("an offset");
						let flood = graph.push(FilterNode::Flood { color: Color::new(0.0, 0.0, 0.0, 0.6, ColorSpace::Srgb) }).expect("a flood");
						let shadow = graph.push(FilterNode::In { input: flood, mask: offset }).expect("a mask");
						graph.push(FilterNode::Composite { source, backdrop: shadow, operator: Operator::SrcOver }).expect("a composite");
						Some(canvas.resources().add_filter(graph).expect("a filter"))
					}
					// THE TWO PASSES OF THE SEPARABLE BLUR, ONE AT A TIME. A sigma of zero on an axis
					// is a one-tap kernel, so that pass still walks its runs and does no arithmetic -
					// which is what makes the pair subtract to the cost of the OTHER pass. They read
					// the same pixels with the same kernel and differ only in DIRECTION, so a
					// difference between them is locality and nothing else: the horizontal pass walks
					// rows the cache has already fetched, and the vertical one walks columns.
					"effects-layers-blur" | "effects-layers-blur-x" | "effects-layers-blur-y" => {
						let (sigma_x, sigma_y) = match stage {
							"effects-layers-blur-x" => (4.0, 0.0),
							"effects-layers-blur-y" => (0.0, 4.0),
							_ => (4.0, 4.0),
						};
						let mut graph = FilterGraph::default();
						let source = graph.push(FilterNode::Source).expect("a source");
						graph.push(FilterNode::Blur { input: source, x: sigma_x, y: sigma_y }).expect("a blur");
						Some(canvas.resources().add_filter(graph).expect("a filter"))
					}
					_ => None,
				};
				canvas.begin_layer(Some(RectF::new(x - 12.0, y - 12.0, 124.0, 174.0)), 0.95, BlendMode::Normal, filter).expect("a layer");
				rect(&mut canvas, RectF::new(x, y, 100.0, 150.0), Paint::Solid(Color::new(0.95, 0.95, 0.98, 1.0, ColorSpace::Srgb))).expect("a card");
				rect(&mut canvas, RectF::new(x + 8.0, y + 8.0, 84.0, 40.0), Paint::Solid(Color::new(0.3, 0.6, 0.9, 1.0, ColorSpace::Srgb))).expect("a header");
				canvas.end_layer().expect("the layer ends");
			}
		}
		if stage == "effects-backdrop" {
			let mut graph = FilterGraph::default();
			let backdrop = graph.push(FilterNode::Backdrop).expect("a backdrop");
			graph.push(FilterNode::Blur { input: backdrop, x: 6.0, y: 6.0 }).expect("a blur");
			let handle = canvas.resources().add_filter(graph).expect("a filter");
			canvas.begin_layer(Some(RectF::new(80.0, 180.0, 480.0, 120.0)), 1.0, BlendMode::Normal, Some(handle)).expect("a layer");
			rect(&mut canvas, RectF::new(80.0, 180.0, 480.0, 120.0), Paint::Solid(Color::new(1.0, 1.0, 1.0, 0.15, ColorSpace::Srgb))).expect("a panel");
			canvas.end_layer().expect("the layer ends");
		}
		out.push((stage, canvas.finish().expect("a list")));
	}

	out
}

fn resource_count(list: &DrawList) -> usize {
	let resources = list.resources();
	resources.paths.len() + resources.images.len() + resources.stops.len() + resources.dashes.len() + resources.glyph_runs.len() + resources.filters.len()
}

fn target() -> OwnedImage {
	let semantics = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Premultiplied };
	let storage = PixelStorage::Known(PixelFormat::B8G8R8A8Unorm);
	let layout = ImageLayout::new(Extent2D::new(WIDTH, HEIGHT), WIDTH * 4, storage, RowOrigin::TopLeft, semantics).expect("a layout");
	OwnedImage::new(layout).expect("a target")
}

/// DETERMINISTIC CONTENT. Every image is generated from its coordinates, so the scene is the same on
/// every machine and in every run - a benchmark whose input is random measures the input.
fn build_images() -> Images {
	let make = |extent: Extent2D, space: ColorSpace, pattern: &dyn Fn(u32, u32) -> [f32; 4]| {
		let semantics = ImageSemantics::Color { color_space: space, alpha_mode: AlphaMode::Straight };
		let storage = PixelStorage::Known(PixelFormat::R8G8B8A8Unorm);
		let layout = ImageLayout::new(extent, extent.width * 4, storage, RowOrigin::TopLeft, semantics).expect("a layout");
		let mut image = OwnedImage::new(layout).expect("an image");
		{
			let mut view = image.view_mut();
			for y in 0..extent.height {
				for x in 0..extent.width {
					let [red, green, blue, alpha] = pattern(x, y);
					graphics_core::pixel::write(&mut view, x, y, graphics_core::pixel::Rgba::new(red, green, blue, alpha));
				}
			}
		}
		image
	};
	Images {
		photo: make(Extent2D::new(512, 512), ColorSpace::Srgb, &|x, y| [(x % 256) as f32 / 255.0, (y % 256) as f32 / 255.0, ((x ^ y) % 256) as f32 / 255.0, 1.0]),
		icon: make(Extent2D::new(32, 32), ColorSpace::Srgb, &|x, y| [1.0, (x + y) as f32 / 64.0, 0.25, if (x / 4 + y / 4) % 2 == 0 { 1.0 } else { 0.5 }]),
		// A WIDE-GAMUT SOURCE, which is the colour-conversion half of the image scene.
		wide: make(Extent2D::new(256, 256), ColorSpace::DisplayP3, &|x, y| [(x % 128) as f32 / 127.0, 0.5, (y % 128) as f32 / 127.0, 1.0]),
		// AND ONE YUV SOURCE, in Rec. 2020 limited range, which is what a decoded video frame is.
		video: VideoFrame::new(Extent2D::new(320, 240)),
	}
}

fn rect(canvas: &mut Canvas, rect: RectF, paint: Paint) -> Result<(), Error> {
	let mut builder = PathBuilder::new();
	builder.add_rect(rect)?;
	canvas.fill_path(builder.finish(), paint, FillRule::NonZero)
}

/// RECTANGLES, GLYPHS, IMAGES AND CLIPS: the shape of a user interface, which is what most drawings
/// actually are.
fn ui_basic() -> Result<DrawList, Error> {
	let mut canvas = Canvas::new();
	let icon = canvas.resources().add_image(ImageRecord { identity: 2, layout_generation: 1, content_generation: 1 })?;
	let _ = icon;
	let panel = Color::new(0.16, 0.18, 0.22, 1.0, ColorSpace::Srgb);
	let text = Color::new(0.9, 0.92, 0.95, 1.0, ColorSpace::Srgb);
	rect(&mut canvas, RectF::new(0.0, 0.0, WIDTH as f32, HEIGHT as f32), Paint::Solid(panel))?;
	for row in 0..50 {
		let y = 8.0 + row as f32 * 9.0;
		canvas.save()?;
		canvas.set_clip(
			{
				let mut builder = PathBuilder::new();
				builder.add_rect(RectF::new(4.0, y, 632.0, 8.0))?;
				builder.finish()
			},
			FillRule::NonZero,
		)?;
		rect(&mut canvas, RectF::new(6.0, y + 1.0, 120.0, 6.0), Paint::Solid(text))?;
		rect(&mut canvas, RectF::new(140.0, y + 1.0, 400.0, 6.0), Paint::Solid(Color::new(0.4, 0.5, 0.7, 0.8, ColorSpace::Srgb)))?;
		canvas.draw_image(ImageRecord { identity: 2, layout_generation: 1, content_generation: 1 }, RectF::new(0.0, 0.0, 32.0, 32.0), RectF::new(560.0, y - 2.0, 12.0, 12.0), ImageQuality::Bilinear)?;
		canvas.restore()?;
	}
	rect(&mut canvas, RectF::new(0.0, 460.0, WIDTH as f32, 20.0), Paint::Solid(Color::new(0.1, 0.11, 0.13, 1.0, ColorSpace::Srgb)))?;
	canvas.finish()
}

/// LAYERS, SHADOWS AND A BACKDROP BLUR: the scene whose cost is the filter graph rather than the
/// geometry.
fn ui_effects() -> Result<DrawList, Error> {
	let mut canvas = Canvas::new();
	rect(&mut canvas, RectF::new(0.0, 0.0, WIDTH as f32, HEIGHT as f32), Paint::Solid(Color::new(0.2, 0.3, 0.5, 1.0, ColorSpace::Srgb)))?;
	canvas.draw_image(ImageRecord { identity: 1, layout_generation: 1, content_generation: 1 }, RectF::new(0.0, 0.0, 512.0, 512.0), RectF::new(0.0, 0.0, 640.0, 480.0), ImageQuality::Bilinear)?;
	for index in 0..10 {
		let x = 20.0 + (index % 5) as f32 * 120.0;
		let y = 40.0 + (index / 5) as f32 * 200.0;
		// A SHADOW IS A BLUR OF AN ALPHA CHANNEL, offset, tinted and composited UNDER the thing that
		// cast it - five nodes, which is what an API with a `drop_shadow` call cannot express.
		let mut graph = FilterGraph::default();
		let source = graph.push(FilterNode::Source)?;
		let blur = graph.push(FilterNode::Blur { input: source, x: 4.0, y: 4.0 })?;
		let offset = graph.push(FilterNode::Offset { input: blur, dx: 3.0, dy: 5.0 })?;
		let flood = graph.push(FilterNode::Flood { color: Color::new(0.0, 0.0, 0.0, 0.6, ColorSpace::Srgb) })?;
		let shadow = graph.push(FilterNode::In { input: flood, mask: offset })?;
		graph.push(FilterNode::Composite { source, backdrop: shadow, operator: Operator::SrcOver })?;
		let handle = canvas.resources().add_filter(graph)?;
		canvas.begin_layer(Some(RectF::new(x - 12.0, y - 12.0, 124.0, 174.0)), 0.95, BlendMode::Normal, Some(handle))?;
		rect(&mut canvas, RectF::new(x, y, 100.0, 150.0), Paint::Solid(Color::new(0.95, 0.95, 0.98, 1.0, ColorSpace::Srgb)))?;
		rect(&mut canvas, RectF::new(x + 8.0, y + 8.0, 84.0, 40.0), Paint::Solid(Color::new(0.3, 0.6, 0.9, 1.0, ColorSpace::Srgb)))?;
		canvas.end_layer()?;
	}
	// AND ONE BACKDROP BLUR, which is the frosted panel: a blur of what is UNDER the layer.
	let mut graph = FilterGraph::default();
	let backdrop = graph.push(FilterNode::Backdrop)?;
	graph.push(FilterNode::Blur { input: backdrop, x: 6.0, y: 6.0 })?;
	let handle = canvas.resources().add_filter(graph)?;
	canvas.begin_layer(Some(RectF::new(80.0, 180.0, 480.0, 120.0)), 1.0, BlendMode::Normal, Some(handle))?;
	rect(&mut canvas, RectF::new(80.0, 180.0, 480.0, 120.0), Paint::Solid(Color::new(1.0, 1.0, 1.0, 0.15, ColorSpace::Srgb)))?;
	canvas.end_layer()?;
	canvas.finish()
}

/// PATHS AND CURVES AT A FIXED STROKE WIDTH AND JOIN SET, which is the scene whose cost is the
/// rasteriser itself.
fn vector_stress() -> Result<DrawList, Error> {
	let mut canvas = Canvas::new();
	let stops = canvas.resources().add_stops(vec![
		GradientStop { offset: 0.0, color: Color::new(0.9, 0.2, 0.1, 1.0, ColorSpace::Srgb) },
		GradientStop { offset: 0.5, color: Color::new(0.95, 0.8, 0.1, 1.0, ColorSpace::Srgb) },
		GradientStop { offset: 1.0, color: Color::new(0.1, 0.4, 0.9, 1.0, ColorSpace::Srgb) },
	])?;
	let gradient = Paint::Linear { from: PointF { x: 0.0, y: 0.0 }, to: PointF { x: 640.0, y: 480.0 }, stops, spread: graphics_core::sample::Spread::Clamp, transform: Transform::IDENTITY };
	let style = StrokeStyle { width: 2.5, cap: Cap::Round, join: Join::Miter, miter_limit: 4.0, ..StrokeStyle::default() };
	for index in 0..120 {
		let phase = index as f32 * 0.11;
		let mut builder = PathBuilder::new();
		builder.move_to(PointF { x: 10.0 + phase * 4.0, y: 20.0 + (index % 20) as f32 * 22.0 })?;
		for segment in 0..6 {
			let x = 10.0 + phase * 4.0 + segment as f32 * 100.0;
			let y = 20.0 + (index % 20) as f32 * 22.0;
			builder.cubic_to(PointF { x: x + 30.0, y: y - 40.0 }, PointF { x: x + 70.0, y: y + 40.0 }, PointF { x: x + 100.0, y })?;
		}
		canvas.stroke_path(builder.finish(), gradient, style)?;

		let mut fill = PathBuilder::new();
		let centre = PointF { x: 40.0 + (index % 12) as f32 * 50.0, y: 40.0 + (index / 12) as f32 * 44.0 };
		fill.move_to(PointF { x: centre.x, y: centre.y - 18.0 })?;
		fill.quad_to(PointF { x: centre.x + 24.0, y: centre.y }, PointF { x: centre.x, y: centre.y + 18.0 })?;
		fill.quad_to(PointF { x: centre.x - 24.0, y: centre.y }, PointF { x: centre.x, y: centre.y - 18.0 })?;
		fill.close()?;
		canvas.fill_path(fill.finish(), Paint::Solid(Color::new(0.2, 0.7, 0.4, 0.7, ColorSpace::Srgb)), FillRule::NonZero)?;
	}
	canvas.finish()
}

/// SCALING AND COLOUR CONVERSION, which is the scene whose cost is the sampler and the pipeline.
/// RESAMPLING ALONE: what a pyramid and the three filters cost, over one source and at UI sizes.
///
/// Every draw here is a RESAMPLE and none is a colour conversion, which is the point of the split:
/// source space and target space are the same, so what this measures is filtering and nothing else.
fn image_resample() -> Result<DrawList, Error> {
	let mut canvas = Canvas::new();
	let photo = ImageRecord { identity: 1, layout_generation: 1, content_generation: 1 };
	// A LARGE DOWNSCALE, which is what the pyramid is for.
	for index in 0..8 {
		let x = (index % 4) as f32 * 160.0;
		let y = (index / 4) as f32 * 120.0;
		canvas.draw_image(photo, RectF::new(0.0, 0.0, 512.0, 512.0), RectF::new(x, y, 158.0, 118.0), ImageQuality::Mipmapped)?;
	}
	// AN UPSCALE AT EACH QUALITY, which is where the three filters differ.
	for (index, quality) in [ImageQuality::Nearest, ImageQuality::Bilinear, ImageQuality::Bicubic].into_iter().enumerate() {
		canvas.draw_image(photo, RectF::new(0.0, 0.0, 64.0, 64.0), RectF::new(index as f32 * 200.0, 250.0, 190.0, 190.0), quality)?;
	}
	canvas.finish()
}

/// COLOUR CONVERSION ALONE: a wide-gamut source into an sRGB target, and a YUV source from its
/// planes.
///
/// TWO OF ITS THREE KINDS OF DRAW COVER THE WHOLE FRAME, which is what makes this a different
/// workload rather than more of the one above: a full-frame conversion is three hundred thousand
/// pixels through a transfer function and a matrix, and the resampling scene's largest draw is a
/// fifth of that.
fn image_convert() -> Result<DrawList, Error> {
	let mut canvas = Canvas::new();
	let wide = ImageRecord { identity: 3, layout_generation: 1, content_generation: 1 };
	// A WIDE-GAMUT SOURCE INTO AN sRGB TARGET, at tile sizes.
	for index in 0..12 {
		let x = (index % 6) as f32 * 106.0;
		let y = 250.0 + (index / 6) as f32 * 110.0;
		canvas.draw_image(wide, RectF::new(0.0, 0.0, 256.0, 256.0), RectF::new(x, y, 104.0, 108.0), ImageQuality::Bilinear)?;
	}
	canvas.set_operator(Operator::SrcOver);
	canvas.draw_image(wide, RectF::new(0.0, 0.0, 256.0, 256.0), RectF::new(0.0, 0.0, 640.0, 480.0), ImageQuality::Mipmapped)?;
	// THE YUV SOURCE, drawn from its planes at the size a player would: no conversion pass, and the
	// matrix, the range and the primaries all on the shared path.
	let video = ImageRecord { identity: 4, layout_generation: 1, content_generation: 1 };
	canvas.draw_image(video, RectF::new(0.0, 0.0, 320.0, 240.0), RectF::new(0.0, 0.0, 640.0, 480.0), ImageQuality::Bilinear)?;
	canvas.finish()
}
