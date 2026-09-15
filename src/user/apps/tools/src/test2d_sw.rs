// test2d-sw - the INTERACTIVE proof of the 2D platform, drawn through `render2d` and nothing else.
//
// WHAT THIS IS FOR, AND WHY IT IS NOT THE CONFORMANCE SUITE. That one walks the profile entry by
// entry and answers "is each feature implemented"; this one answers "is this a platform an
// application can be written against" - one scene made of the things a real drawing is made of, on a
// real surface, with a real present queue under it, paced by the frame loop and driven by keys.
// Neither substitutes for the other: a suite that passes every scene and a demo that cannot hold
// sixty frames a second are both possible, and so is the reverse.
//
// IT RENDERS THROUGH `render2d` INTO THE SURFACE'S OWN IMAGE. There is no application-local
// rasteriser here and no second drawing loop: a `Canvas` records a list, `soft2d` replays it into
// the mapped image the display service handed over, and the damage that goes with the present is
// computed from what the scene moved rather than from what it feels like.
//
// THE PHASES ARE THE POINT OF THE DAMAGE HALF. "An animated background" and "two non-contiguous
// damage rectangles that reach the driver as two" contradict each other - a background that changes
// every frame changes every pixel, and its only honest damage is `Full` - so the scene runs in
// phases and each one states what it damages:
//
//     1 full        everything drawn, damage = Full
//     2 partial     background held still, two objects move, damage = old bounds + new bounds
//     3 multi-rect  exactly two distant regions change, presented as TWO rectangles
//     4 full-again  the background's parameters change, so damage is Full again
//     5 resize      a new generation, whose first frame is Full by the rule above
//     6 scale       the scale factor changes with the logical size held: the layout is unchanged
//                   and every edge is resolved at the new physical resolution
//
// THE TEXT IS DRAWN FROM FORMS THIS PROGRAM CARRIES, including a colour-layer one. The grants here
// are `display` and `input-keys` and nothing else - no font catalogue, no volume - so a line of text
// with a colour glyph in it is proved the only way a program with those grants can: by handing
// `render2d` the glyph forms itself. What is under test is the DRAWING of a run, which is this
// library's half of text; the shaping stack has its own conformance run and its own capability.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use graphics_app::{FrameLoop, Step};
use graphics_core::geom::{Extent2D, PointF, RectF};
use graphics_core::layout::{ImageLayout, RowOrigin};
use graphics_core::semantics::ImageSemantics;
use graphics_core::{AlphaMode, ColorSpace, ImageView, ImageViewMut, OwnedImage, PixelFormat, PixelStorage};
use keys::usage;
use proto::system::{LaunchContext, input};
use render2d::backend::{Backend, TargetDescription};
use render2d::blend::BlendMode;
use render2d::filter::{FilterGraph, FilterNode};
use render2d::list::DrawList;
use render2d::paint::{Color, GradientStop, ImageQuality, Paint, SpreadMode};
use render2d::path::{FillRule, Join, PathBuilder, StrokeStyle};
use render2d::transform::Transform;
use render2d::{Canvas, Error};
use rt::*;
use soft2d::Soft2d;

/// THE SIZE THE SURFACE IS ASKED FOR, AND IT IS THE SCREEN'S OWN.
///
/// ZERO MEANS "WHATEVER THE OUTPUT IS", and that is the whole reason it is zero here: a surface with
/// a logical size of its own is NOT reconfigured when the output changes, so a demo that asked for
/// 640 by 480 would never see the resize - and the phase a resize enters is the one this demo exists
/// to show. The scene lays itself out for whatever extent each frame arrives with.
const WIDTH: u32 = 0;
const HEIGHT: u32 = 0;

/// How many frames each phase runs for before the next one begins.
const PHASE_FRAMES: u32 = 24;

/// A hard ceiling on loop iterations, so a demo that stops making progress ENDS rather than hanging
/// whatever is watching it.
const ITERATIONS: u32 = 200_000;

/// The phases, in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
	Full,
	Partial,
	MultiRect,
	FullAgain,
	Resize,
	Scale,
}

impl Phase {
	const ORDER: [Phase; 6] = [Phase::Full, Phase::Partial, Phase::MultiRect, Phase::FullAgain, Phase::Resize, Phase::Scale];

	fn name(self) -> &'static [u8] {
		match self {
			Phase::Full => b"full",
			Phase::Partial => b"partial",
			Phase::MultiRect => b"multi-rect",
			Phase::FullAgain => b"full-again",
			Phase::Resize => b"resize",
			Phase::Scale => b"scale",
		}
	}
}

/// The animation state, which is everything the scene is a function of.
struct Scene {
	/// The frame counter the animation is driven by. NOT a clock: a demo whose picture depends on
	/// how fast the machine runs cannot be compared between two runs, and the deterministic controls
	/// exist so that a capture on one architecture is the capture on another.
	tick: u32,
	/// How far the clipped column has scrolled, which the arrow keys move.
	scroll: f32,
	paused: bool,
}

impl Scene {
	fn new() -> Scene {
		Scene { tick: 0, scroll: 0.0, paused: false }
	}

	/// Where the two moving objects are on a given tick. The damage for the partial phase is the
	/// union of this and the previous tick's answer.
	fn movers(&self, tick: u32, extent: Extent2D) -> [RectF; 2] {
		// SIZED FROM THE EXTENT AND NOT IN FIXED PIXELS. A scene laid out for one screen and run on a
		// smaller one puts its objects past the edge, and a damage rectangle that leaves the surface
		// is a present the service refuses - which is a demo that stops with nothing to say.
		let width = extent.width as f32;
		let height = extent.height as f32;
		let size = (height * 0.12).max(8.0);
		let span = (width - size * 2.0 - 16.0).max(1.0);
		let progress = ((tick % 48) as f32) / 48.0;
		let x = 8.0 + span * progress;
		[RectF::new(x, height * 0.55, size, size), RectF::new(width - x - size * 0.7 - 8.0, height * 0.74, size * 0.7, size * 0.7)]
	}

	/// The two distant regions the multi-rect phase changes, and nothing else changes with them.
	fn patches(&self, tick: u32, extent: Extent2D) -> [RectF; 2] {
		let blink = (tick / 6) % 2 == 0;
		let full = (extent.height as f32 * 0.16).max(6.0);
		let size = if blink { full } else { full * 0.7 };
		let inset = (extent.height as f32 * 0.08).max(4.0);
		[RectF::new(inset, inset, size, size), RectF::new(extent.width as f32 - inset - size, extent.height as f32 - inset - size, size, size)]
	}

	/// THE WHOLE SCENE, as a recorded list. Every part of it is something an application does: a
	/// gradient background, filled and stroked curves under both fill rules, an antialiased shallow
	/// edge, a projectively transformed image, a clipped and scrolled column, a nested and an inverse
	/// clip, a group-opacity layer, blend modes including a non-separable one, a backdrop-blurred
	/// panel and a line of text with a colour glyph in it.
	fn record(&self, canvas: &mut Canvas, extent: Extent2D, phase: Phase, images: &Images) -> Result<DrawList, Error> {
		// THE CANVAS IS REUSED AND NOT REBUILT. `restart` keeps the resource tables and their
		// capacity, which is what makes re-recording the same scene every frame cost no new
		// allocation for the paths, the stops and the filter graphs it holds - a recorder that built
		// a new one each frame would allocate sixty times a second for ever.
		canvas.restart();
		let width = extent.width as f32;
		let height = extent.height as f32;
		// THE BACKGROUND IS A MULTI-STOP CONIC GRADIENT, whose angle moves only in the phases that
		// damage the whole surface. In the partial and multi-rect phases it is held STILL, because a
		// background that changed every frame would make every honest damage rectangle the whole
		// screen.
		let animated = matches!(phase, Phase::Full | Phase::FullAgain | Phase::Resize | Phase::Scale);
		let turn = if animated { (self.tick % 120) as f32 / 120.0 * core::f32::consts::TAU } else { 0.0 };
		let offset = if matches!(phase, Phase::FullAgain) { 0.35 } else { 0.0 };
		let stops = canvas.resources().add_stops(alloc::vec![
			GradientStop { offset: 0.0, color: srgb(0.06, 0.08, 0.16, 1.0) },
			GradientStop { offset: 0.35, color: srgb(0.10, 0.22, 0.38 + offset, 1.0) },
			GradientStop { offset: 0.70, color: srgb(0.26, 0.10, 0.34, 1.0) },
			GradientStop { offset: 1.0, color: srgb(0.06, 0.08, 0.16, 1.0) },
		])?;
		let background = Paint::Conic { centre: PointF { x: width * 0.5, y: height * 0.5 }, start_angle: turn, end_angle: turn + core::f32::consts::TAU, stops, spread: SpreadMode::Clamp, transform: Transform::IDENTITY };
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, width, height)), background, FillRule::NonZero)?;

		// A FILLED CURVE UNDER THE EVEN-ODD RULE AND THE SAME ONE STROKED. The star's own crossings
		// are what makes the rule visible: under non-zero it is a solid, under even-odd it is a ring.
		let star = star_path(PointF { x: width * 0.22, y: height * 0.28 }, 82.0, 34.0);
		canvas.fill_path(star.clone(), Paint::Solid(srgb(0.95, 0.80, 0.25, 1.0)), FillRule::EvenOdd)?;
		canvas.stroke_path(star, Paint::Solid(srgb(0.10, 0.05, 0.00, 1.0)), StrokeStyle { width: 3.0, join: Join::Round, ..StrokeStyle::default() })?;

		// AN ANTIALIASED EDGE AT A SHALLOW ANGLE, which is where a coverage rasteriser is either
		// analytic or obviously not: a staircase is visible on this and on almost nothing else.
		let mut sliver = PathBuilder::new();
		sliver.add_polygon(&[
			PointF { x: width * 0.06, y: height * 0.50 },
			PointF { x: width * 0.94, y: height * 0.545 },
			PointF { x: width * 0.94, y: height * 0.556 },
			PointF { x: width * 0.06, y: height * 0.511 },
		])?;
		canvas.fill_path(sliver.finish(), Paint::Solid(srgb(1.0, 1.0, 1.0, 0.85)), FillRule::NonZero)?;

		// A PROJECTIVELY TRANSFORMED IMAGE: the perspective terms are what make this a plane in
		// space rather than a parallelogram, and they are sampled through the divide per pixel.
		canvas.save()?;
		let mut perspective = Transform::translate(width * 0.56, height * 0.10);
		perspective.m[2][0] = 0.0016;
		canvas.concat_transform(&perspective);
		canvas.draw_image(images.record(), RectF::new(0.0, 0.0, 8.0, 8.0), RectF::new(0.0, 0.0, 200.0, 140.0), ImageQuality::Bilinear)?;
		canvas.restore()?;

		// A ROUNDED-RECTANGLE CLIP WITH CONTENT SCROLLING UNDER IT, a NESTED clip inside it, and an
		// INVERSE one punching a hole - the three shapes of clipping an application actually uses.
		let panel = RectF::new(width * 0.06, height * 0.62, width * 0.40, height * 0.30);
		let mut rounded = PathBuilder::new();
		rounded.add_rounded_rect(panel, 18.0, 18.0)?;
		canvas.save()?;
		canvas.set_clip(rounded.finish(), FillRule::NonZero)?;
		let mut row = 0;
		while (row as f32) * 26.0 < panel.height + 52.0 {
			let y = panel.y + (row as f32) * 26.0 - wrap_to(self.scroll, 26.0);
			let shade = 0.35 + 0.1 * ((row % 3) as f32);
			canvas.fill_path(rect_path(RectF::new(panel.x + 10.0, y, panel.width - 20.0, 18.0)), Paint::Solid(srgb(shade, shade * 0.6, 0.8, 1.0)), FillRule::NonZero)?;
			row += 1;
		}
		canvas.save()?;
		canvas.set_clip(rect_path(RectF::new(panel.x, panel.y, panel.width * 0.5, panel.height)), FillRule::NonZero)?;
		canvas.fill_path(rect_path(panel), Paint::Solid(srgb(0.0, 0.0, 0.0, 0.25)), FillRule::NonZero)?;
		canvas.restore()?;
		let mut hole = PathBuilder::new();
		hole.add_circle(PointF { x: panel.x + panel.width * 0.78, y: panel.y + panel.height * 0.5 }, 26.0)?;
		canvas.save()?;
		canvas.set_clip_inverse(hole.finish(), FillRule::NonZero)?;
		canvas.fill_path(rect_path(RectF::new(panel.x + panel.width * 0.6, panel.y + 8.0, panel.width * 0.36, panel.height - 16.0)), Paint::Solid(srgb(1.0, 0.95, 0.85, 0.35)), FillRule::NonZero)?;
		canvas.restore()?;
		canvas.restore()?;

		// A GROUP-OPACITY LAYER WITH TWO OVERLAPPING CHILDREN. The overlap is the whole point: the
		// opacity applies to the GROUP, so the two children do not show through each other.
		canvas.begin_layer(None, 0.55, BlendMode::Normal, None)?;
		canvas.fill_path(rect_path(RectF::new(width * 0.55, height * 0.60, 120.0, 90.0)), Paint::Solid(srgb(0.95, 0.35, 0.25, 1.0)), FillRule::NonZero)?;
		canvas.fill_path(rect_path(RectF::new(width * 0.55 + 60.0, height * 0.60 + 40.0, 120.0, 90.0)), Paint::Solid(srgb(0.25, 0.85, 0.55, 1.0)), FillRule::NonZero)?;
		canvas.end_layer()?;

		// SEVERAL BLEND MODES, ONE OF THEM NON-SEPARABLE. `Luminosity` needs the whole pixel rather
		// than a per-channel function, which is what makes it the one an implementation leaves out.
		for (index, mode) in [BlendMode::Multiply, BlendMode::Screen, BlendMode::Luminosity].into_iter().enumerate() {
			canvas.set_blend_mode(mode);
			let x = width * 0.55 + (index as f32) * 66.0;
			canvas.fill_path(rect_path(RectF::new(x, height * 0.40, 58.0, 58.0)), Paint::Solid(srgb(0.60, 0.65, 0.80, 1.0)), FillRule::NonZero)?;
		}
		canvas.set_blend_mode(BlendMode::Normal);

		// A BACKDROP-BLURRED PANEL, which is a blur of the SCENE and not of the panel: without the
		// backdrop node the whole class of frosted surfaces has to be built by drawing twice.
		let mut graph = FilterGraph::default();
		let backdrop = graph.push(FilterNode::Backdrop)?;
		graph.push(FilterNode::Blur { input: backdrop, x: 6.0, y: 6.0 })?;
		let filter = canvas.resources().add_filter(graph)?;
		canvas.begin_layer(Some(RectF::new(width * 0.28, height * 0.12, 200.0, 96.0)), 1.0, BlendMode::Normal, Some(filter))?;
		canvas.fill_path(rect_path(RectF::new(width * 0.28, height * 0.12, 200.0, 96.0)), Paint::Solid(srgb(1.0, 1.0, 1.0, 0.12)), FillRule::NonZero)?;
		canvas.end_layer()?;

		// A LINE OF TEXT, INCLUDING A COLOUR GLYPH - drawn as a run, which is the only way text
		// enters this API.
		canvas.draw_glyph_run(text_run(24.0, height * 0.11), Paint::Solid(srgb(1.0, 1.0, 1.0, 1.0)))?;

		// THE MOVING OBJECTS, whose bounds are the partial phase's damage.
		for (index, rect) in self.movers(self.tick, extent).into_iter().enumerate() {
			let colour = if index == 0 { srgb(1.0, 0.85, 0.2, 1.0) } else { srgb(0.4, 0.9, 1.0, 1.0) };
			let mut shape = PathBuilder::new();
			shape.add_rounded_rect(rect, 8.0, 8.0)?;
			canvas.fill_path(shape.finish(), Paint::Solid(colour), FillRule::NonZero)?;
		}

		// AND THE TWO DISTANT PATCHES, which are the only thing that changes in the multi-rect phase.
		for rect in self.patches(self.tick, extent) {
			canvas.fill_path(rect_path(rect), Paint::Solid(srgb(0.9, 0.2, 0.5, 1.0)), FillRule::NonZero)?;
		}

		canvas.finish()
	}
}

/// A value wrapped into `[0, period)`.
///
/// WRITTEN OUT RATHER THAN `%`. A float remainder is `fmodf`, which is a libm symbol this program
/// would then have to carry a provider for - and the scroll offset of a list is one subtraction.
fn wrap_to(value: f32, period: f32) -> f32 {
	let laps = (value / period) as i32 as f32;
	let remainder = value - laps * period;
	if remainder < 0.0 { remainder + period } else { remainder }
}

/// A colour in the surface's own space.
fn srgb(red: f32, green: f32, blue: f32, alpha: f32) -> Color {
	Color::new(red, green, blue, alpha, ColorSpace::Srgb)
}

fn rect_path(rect: RectF) -> render2d::path::Path {
	let mut builder = PathBuilder::new();
	let _ = builder.add_rect(rect);
	builder.finish()
}

/// A five-pointed star as ONE self-intersecting contour, which is what makes the fill rule visible.
fn star_path(centre: PointF, outer: f32, inner: f32) -> render2d::path::Path {
	let mut points = Vec::new();
	for step in 0..10 {
		let radius = if step % 2 == 0 { outer } else { inner };
		let angle = (step as f32) * core::f32::consts::TAU / 10.0 - core::f32::consts::FRAC_PI_2;
		let (sine, cosine) = sin_cos(angle);
		points.push(PointF { x: centre.x + radius * cosine, y: centre.y + radius * sine });
	}
	let mut builder = PathBuilder::new();
	let _ = builder.add_polygon(&points);
	builder.finish()
}

/// Sine and cosine for the one shape that needs them.
///
/// THE REDUCTION IS IN `f64` AND THE SERIES IS THE TAYLOR ONE over a quarter turn, which is where it
/// is accurate to the last representable `f32`. A star has ten points and this runs ten times a
/// frame, so what it costs is not worth a dependency.
fn sin_cos(radians: f32) -> (f32, f32) {
	const PI_OVER_2: f64 = core::f64::consts::FRAC_PI_2;
	let wide = radians as f64;
	let quadrant = if wide >= 0.0 { ((wide / PI_OVER_2) + 0.5) as i64 as f64 } else { ((wide / PI_OVER_2) - 0.5) as i64 as f64 };
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

/// The images this program draws, which it makes itself: it has no volume and no asset capability.
struct Images {
	checker: OwnedImage,
}

impl Images {
	fn new() -> Option<Images> {
		let semantics = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Straight };
		let layout = ImageLayout::new(Extent2D::new(8, 8), 32, PixelStorage::Known(PixelFormat::R8G8B8A8Unorm), RowOrigin::TopLeft, semantics).ok()?;
		let mut checker = OwnedImage::new(layout).ok()?;
		{
			let mut view = checker.view_mut();
			for y in 0..8 {
				for x in 0..8 {
					let light = (x + y) % 2 == 0;
					let colour = if light { graphics_core::pixel::Rgba::new(0.95, 0.95, 0.98, 1.0) } else { graphics_core::pixel::Rgba::new(0.15, 0.25, 0.45, 1.0) };
					graphics_core::pixel::write(&mut view, x, y, colour);
				}
			}
		}
		Some(Images { checker })
	}

	fn record(&self) -> render2d::list::ImageRecord {
		render2d::list::ImageRecord { identity: 1, layout_generation: 1, content_generation: 1 }
	}
}

impl soft2d::target::ImageSource for Images {
	fn image(&self, identity: u64) -> Option<ImageView<'_>> {
		(identity == 1).then(|| self.checker.view())
	}
}

/// The glyph forms this program carries, one of them a COLOUR one.
struct Forms;

impl soft2d::glyph::GlyphProvider for Forms {
	fn glyph(&self, key: &font_contract::cache::GlyphCacheKey) -> soft2d::glyph::GlyphImage {
		// THE LAST GLYPH OF THE LINE IS A COLOUR ONE, in its own palette colours and not the run's
		// paint - which is what an emoji in a line of text is.
		if key.glyph == COLOUR_GLYPH {
			let mut left = PathBuilder::new();
			let _ = left.add_circle(PointF { x: 7.0, y: -7.0 }, 7.0);
			let mut right = PathBuilder::new();
			let _ = right.add_circle(PointF { x: 7.0, y: -7.0 }, 3.0);
			return soft2d::glyph::GlyphImage::Layers(alloc::vec![(left.finish(), srgb(0.98, 0.72, 0.10, 1.0)), (right.finish(), srgb(0.20, 0.10, 0.00, 1.0))]);
		}
		// EVERY OTHER GLYPH IS AN OUTLINE, and its shape is a function of its id so that the line
		// reads as a line of different letters rather than a row of identical boxes.
		let mut builder = PathBuilder::new();
		let width = 4.0 + ((key.glyph % 3) as f32) * 2.0;
		let _ = builder.add_rect(RectF::new(0.0, -12.0, width, 12.0));
		let _ = builder.add_rect(RectF::new(0.0, -7.0 + ((key.glyph % 2) as f32) * 3.0, width + 3.0, 3.0));
		soft2d::glyph::GlyphImage::Outline(builder.finish())
	}
}

/// The glyph id the provider answers with colour layers.
const COLOUR_GLYPH: u32 = 99;

/// A line of eleven glyphs, the last of which is the colour one.
fn text_run(origin_x: f32, origin_y: f32) -> render2d::list::RecordedGlyphRun {
	let mut glyphs = Vec::new();
	let mut advance = 0.0f32;
	for index in 0..11u32 {
		let glyph = if index == 10 { COLOUR_GLYPH } else { 20 + index };
		glyphs.push(font_contract::PositionedGlyph { glyph, x_offset: font_contract::Fixed266::ZERO, y_offset: font_contract::Fixed266::ZERO, x_advance: font_contract::Fixed266::from_pixels(14), y_advance: font_contract::Fixed266::ZERO, kind: font_contract::glyph::GlyphKind::Outline, selection: font_contract::cache::KindSelection { strike: None, palette: None } });
		advance += 14.0;
	}
	let _ = advance;
	render2d::list::RecordedGlyphRun { face: font_contract::FaceRef { face: font_contract::FaceIdentity { file: font_contract::face::FileIdentity([9u8; 32]), index: 0 }, generation: font_contract::face::Generation(1) }, size: font_contract::Fixed266::from_pixels(16), variation: font_contract::VariationCoordinates::default(), script: font_contract::ScriptTag::from_bytes(*b"latn"), direction: font_contract::Direction::LeftToRight, mode: font_contract::glyph::RasterisationMode::Grayscale, origin_x: font_contract::Fixed266::from_raw((origin_x * 64.0) as i32), origin_y: font_contract::Fixed266::from_raw((origin_y * 64.0) as i32), glyphs, clusters: Vec::new() }
}

/// The deterministic controls, which are NOT in the ordinary UI.
///
/// A DEMO NOBODY CAN DRIVE IS A DEMO NOBODY CAN GATE. `--frames N` ends the run after N presents,
/// `--phase-frames N` shortens each phase, and `--no-input` skips the key subscription for a harness
/// that has no input service to offer. They are launch ARGUMENTS and not keys, because a key that
/// changed the run would be a key a person could press by accident.
struct Controls {
	frames: u32,
	phase_frames: u32,
	input: bool,
	second_surface: bool,
	/// The logical size to ask for, or zero for the screen's own.
	width: u32,
	height: u32,
}

impl Controls {
	fn parse(args: &[u8]) -> Controls {
		let mut controls = Controls { frames: 0, phase_frames: PHASE_FRAMES, input: true, second_surface: true, width: WIDTH, height: HEIGHT };
		for word in args.split(|byte| *byte == b' ' || *byte == 0) {
			if let Some(value) = word.strip_prefix(b"--frames=") {
				controls.frames = number(value);
			} else if let Some(value) = word.strip_prefix(b"--phase-frames=") {
				controls.phase_frames = number(value).max(1);
			} else if word == b"--no-input" {
				controls.input = false;
			} else if word == b"--no-second-surface" {
				controls.second_surface = false;
			} else if let Some(value) = word.strip_prefix(b"--size=") {
				// A SIZE OF ITS OWN, for a MEASUREMENT run. A surface with a logical size is not
				// reconfigured when the output changes, which is exactly wrong for the phase machine
				// and exactly right for comparing two runs at the same number of pixels.
				if let Some(cross) = value.iter().position(|byte| *byte == b'x') {
					controls.width = number(&value[..cross]);
					controls.height = number(&value[cross + 1..]);
				}
			}
		}
		controls
	}
}

fn number(text: &[u8]) -> u32 {
	let mut value: u32 = 0;
	for byte in text {
		if !byte.is_ascii_digit() {
			return value;
		}
		value = value.saturating_mul(10).saturating_add((byte - b'0') as u32);
	}
	value
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0; 256];
	inherit_stdout(bootstrap);
	// THE ARGUMENTS COME OUT OF THE LAUNCH CONTEXT and not out of its bytes. The context is a wire
	// record - arguments, working directory, environment - and a program that scanned the encoded
	// form for its own flags would find none of them and run with its defaults, which is a demo that
	// never ends and a harness that waits for it.
	let launch = recv_launch_bytes(bootstrap).unwrap_or_default();
	let arguments = LaunchContext::decode(&launch).map(|context| context.arguments.clone().into_bytes()).unwrap_or_default();
	let controls = Controls::parse(&arguments);
	let display: u64 = recv_tagged(bootstrap, &mut buf, b"DISPLAY").unwrap_or(0);
	if display == 0 {
		print(b"test2d-sw: no display\n");
		exit();
	}
	// AND THE KEYBOARD IS OPTIONAL, under the tag every launcher in this tree sends it under. A
	// harness with no input service to offer runs the demo with `--no-input`, and a demo that waited
	// for the capability anyway would hang there - which is a hang with no output at all, because it
	// happens before the first line it prints.
	let input_channel: u64 = if controls.input { recv_tagged(bootstrap, &mut buf, b"INPUT_KEYS").unwrap_or(0) } else { 0 };

	let client = surface::connect(display);
	// TWO IMAGES, which is the fewest a present queue negotiates and the smallest number that can
	// show a limit being reached at all.
	let Some(Ok(mut frames)) = FrameLoop::open(&client, controls.width, controls.height, 2) else {
		print(b"test2d-sw: no surface\n");
		exit();
	};
	let Some(images) = Images::new() else {
		print(b"test2d-sw: no scratch\n");
		exit();
	};
	print(b"test2d-sw: open\n");

	// THE KEYS, THROUGH THE SURFACE'S OWN FOCUS. A demo that read the console would take input that
	// was meant for the shell, and would take it while it was in the background.
	let mut key_stream = 0u64;
	if input_channel != 0
		&& let Some(Ok(focus)) = frames.surface().input_focus()
	{
		key_stream = surface::subscribe_keys(input_channel, focus).unwrap_or(0);
	}

	let mut scene = Scene::new();
	let mut canvas = Canvas::new();
	let mut backend = Soft2d::new();
	let provider = Forms;
	let mut phase_index = 0usize;
	let mut phase_frame = 0u32;
	let mut presented = 0u32;
	let mut rebuilt = 0u32;
	let mut multi_rect_presents = 0u32;
	let mut background = 0u32;
	let mut partial_presents = 0u32;
	// THE LAST TWO FRAMES' BOUNDS AND NOT THE LAST ONE'S. The queue holds TWO images, so the image
	// being drawn into now is the one presented two frames ago - and a damage rectangle covering only
	// what moved since the LAST frame leaves that image's older content on the screen wherever the
	// thing moved in between. It shows as a band of stale pixels trailing whatever is moving, which
	// is exactly what a live capture of the first version showed.
	let mut recent_movers: [Option<[RectF; 2]>; 2] = [None, None];
	// HOW MANY WHOLE-SURFACE FRAMES ARE STILL OWED. A phase change is a change to the WHOLE picture -
	// the background stops animating, or starts again - and each image in the queue holds a frame
	// from a different moment, so every one of them has to be repainted completely once before a
	// partial update means anything. Owed for the queue's depth, and set by a rebuild for the same
	// reason: a live capture of the version without this shows bands of an older background standing
	// where nothing has been damaged since.
	let mut full_frames_owed: u32 = 2;
	// WHAT A FRAME COSTS AND WHAT IT IS SPACED AT, which are two different numbers. The DRAW is what
	// this renderer owes - record the list, replay it into the image - and the INTERVAL is what the
	// loop achieved with the display's own pacing in it. A demo that reported only the second would
	// look fast on a machine that throttled it and slow on one that did not.
	let mut draw_total_ns: u64 = 0;
	let mut draw_worst_ns: u64 = 0;
	let mut draw_count: u64 = 0;
	let mut interval_total_ns: u64 = 0;
	let mut interval_worst_ns: u64 = 0;
	let mut interval_count: u64 = 0;
	let mut last_present_ns: u64 = 0;
	let mut generation = frames.surface().generation();
	let mut previous_scale = (frames.surface().configuration().scale.numerator, frames.surface().configuration().scale.denominator);
	let mut previous_logical = (frames.surface().configuration().logical_extent.width, frames.surface().configuration().logical_extent.height);
	let mut exit_requested = false;
	let mut key_frame: [u8; 32] = [0; 32];

	for _ in 0..ITERATIONS {
		if exit_requested || frames.close_requested() {
			break;
		}
		if controls.frames != 0 && presented >= controls.frames {
			break;
		}
		// THE NATURAL END IS THE PHASE LIST, not a frame count. The last phase a run can REACH is the
		// one a rebuild enters, so the demo ends when that phase has drawn its frames - and the frame
		// count above is the backstop for a run where no rebuild ever happens.
		if phase_index >= 4 && phase_frame >= controls.phase_frames {
			break;
		}
		// THE KEYS ARE READ WITHOUT BLOCKING, because the frame loop decides when this program
		// waits: a demo that blocked on input would stop drawing whenever nobody was typing.
		if key_stream != 0 {
			while let Some(event) = read_key(key_stream, &mut key_frame) {
				match key_action(event.0, event.1, &mut scene) {
					Action::Exit => exit_requested = true,
					Action::None => {}
				}
			}
		}

		match frames.step() {
			Step::Draw => {
				let Some(frame) = frames.acquire() else { continue };
				let extent = frame.layout.extent;
				let phase = Phase::ORDER[phase_index];
				let list = match scene.record(&mut canvas, extent, phase, &images) {
					Ok(list) => list,
					Err(_) => {
						print(b"test2d-sw: the scene was refused\n");
						frames.abandon(frame);
						break;
					}
				};
				let drawing_began_ns = clock_ns();
				if !draw(&mut backend, &provider, &images, &list, &frame) {
					print(b"test2d-sw: the backend refused the frame\n");
					frames.abandon(frame);
					break;
				}
				let drawing_took_ns = clock_ns().saturating_sub(drawing_began_ns);
				draw_total_ns = draw_total_ns.saturating_add(drawing_took_ns);
				draw_worst_ns = draw_worst_ns.max(drawing_took_ns);
				draw_count += 1;
				// THE DAMAGE IS WHAT THE SCENE MOVED, and the phase says which shape that takes.
				let movers = scene.movers(scene.tick, extent);
				let presented_ok = if full_frames_owed > 0 {
					full_frames_owed -= 1;
					frames.present_whole(frame)
				} else {
					match phase {
						// A NEW GENERATION'S FIRST FRAME IS THE WHOLE SURFACE, and so is a frame whose
						// background changed - which is every frame of the phases that animate it.
						Phase::Full | Phase::FullAgain | Phase::Resize | Phase::Scale => frames.present_whole(frame),
						Phase::Partial => {
							// OLD BOUNDS PLUS NEW BOUNDS. A damage rectangle covering only where a thing
							// IS leaves the pixels where it WAS on the screen.
							let mut rects = Vec::new();
							for rect in movers.iter().chain(recent_movers.iter().flatten().flatten()) {
								rects.push(clamp_rect(*rect, extent));
							}
							partial_presents += 1;
							frames.present_rects(frame, &rects)
						}
						Phase::MultiRect => {
							// EXACTLY TWO, and they are far apart: what this phase is for is that they
							// reach the driver as TWO rectangles rather than as one union covering the
							// screen between them.
							// THE SAME RULE AS THE PARTIAL PHASE, and it is still exactly two rectangles:
							// each patch's damage is its own bounds over the last three frames, which is
							// one rectangle per patch because a patch only grows and shrinks in place.
							let patches = scene.patches(scene.tick, extent);
							let one_back = scene.patches(scene.tick.wrapping_sub(1), extent);
							let two_back = scene.patches(scene.tick.wrapping_sub(2), extent);
							let rects = [clamp_rect(union_of(union_of(patches[0], one_back[0]), two_back[0]), extent), clamp_rect(union_of(union_of(patches[1], one_back[1]), two_back[1]), extent)];
							multi_rect_presents += 1;
							frames.present_rects(frame, &rects)
						}
					}
				};
				if presented_ok {
					presented += 1;
					let now_ns = clock_ns();
					if last_present_ns != 0 {
						let interval = now_ns.saturating_sub(last_present_ns);
						interval_total_ns = interval_total_ns.saturating_add(interval);
						interval_worst_ns = interval_worst_ns.max(interval);
						interval_count += 1;
					}
					last_present_ns = now_ns;
					recent_movers[1] = recent_movers[0];
					recent_movers[0] = Some(movers);
					if !scene.paused {
						scene.tick = scene.tick.wrapping_add(1);
					}
					phase_frame += 1;
					// THE COUNTER WALKS THE FIRST FOUR PHASES AND STOPS. The last two are entered by
					// a REBUILD and by nothing else, because "the first frame of a new generation" is
					// not a number of frames - it is a thing that happened to the surface.
					if phase_frame >= controls.phase_frames && phase_index + 1 < 4 {
						phase_index += 1;
						phase_frame = 0;
						full_frames_owed = 2;
						report_phase(Phase::ORDER[phase_index]);
					}
				}
			}
			Step::AwaitCompletion => frames.park(None),
			Step::Rebuild => {
				rebuilt += 1;
				match frames.rebuild() {
					Some(Ok(rebuilt_to)) => {
						// A REBUILD IS WHAT ENTERS THE LAST TWO PHASES, and which of them it is comes
						// from the configuration rather than from a counter: a changed SCALE with the
						// logical size held is the scale phase, and anything else is a resize. The
						// first frame after either is Full by the rule above, which is why the
						// previous bounds are dropped with it.
						generation = rebuilt_to.generation;
						recent_movers = [None, None];
						let configuration = frames.surface().configuration();
						let scale = (configuration.scale.numerator, configuration.scale.denominator);
						let logical = (configuration.logical_extent.width, configuration.logical_extent.height);
						phase_index = if logical == previous_logical && scale != previous_scale { 5 } else { 4 };
						full_frames_owed = 2;
						previous_scale = scale;
						previous_logical = logical;
						phase_frame = 0;
						report_phase(Phase::ORDER[phase_index]);
					}
					_ => {
						print(b"test2d-sw: rebuild failed\n");
						break;
					}
				}
			}
			Step::Idle { until } => {
				// A HIDDEN LOOP AND A PACED ONE TAKE THE SAME STEP AND MEAN DIFFERENT THINGS. One is
				// this demo waiting its turn between frames; the other is it declining to draw frames
				// nobody can see - and the second is counted, because "it stopped drawing while it
				// was in the background" is the claim a harness taking the screen away is checking.
				if !frames.pacing().visible() {
					background += 1;
				}
				frames.park(until);
			}
		}
	}

	// THE SECOND SURFACE, WHICH IS THE OBJECT MODEL'S OWN PROOF. `P02M0103a-wsi` introduced more than
	// one surface per client, and an object model nobody opened twice is a claim: this opens a second
	// one, presents to it while it owns the screen, and closes it - and the first one's resources and
	// generation survive that.
	let mut second_presents = 0u32;
	if controls.second_surface {
		match FrameLoop::open(&client, WIDTH, HEIGHT, 2) {
			Some(Ok(mut second)) => {
				for _ in 0..64u32 {
					match second.step() {
						Step::Draw => {
							let Some(frame) = second.acquire() else { continue };
							let extent = frame.layout.extent;
							match scene.record(&mut canvas, extent, Phase::Full, &images) {
								Ok(list) if draw(&mut backend, &provider, &images, &list, &frame) => {
									if second.present_whole(frame) {
										second_presents += 1;
									}
								}
								_ => {
									second.abandon(frame);
									break;
								}
							}
							if second_presents >= 2 {
								break;
							}
						}
						Step::AwaitCompletion => second.park(None),
						Step::Rebuild => {
							if second.rebuild().is_none_or(|outcome| outcome.is_err()) {
								break;
							}
						}
						Step::Idle { until } => {
							// A HIDDEN SURFACE DRAWS NOTHING. That is the rule this proves from the
							// application's side: while the other surface owns the screen, this one
							// is told it is not visible and stops rather than presenting frames
							// nobody can see.
							if !second.pacing().visible() {
								break;
							}
							second.park(until);
						}
					}
				}
			}
			_ => print(b"test2d-sw: the second surface was refused\n"),
		}
	}

	// WHAT IT DID, one line, which is what a harness reads.
	let mut line: Vec<u8> = Vec::new();
	line.extend_from_slice(b"test2d-sw:");
	for (name, value) in [
		(&b" presented="[..], presented),
		(b" partial=", partial_presents),
		(b" multi-rect=", multi_rect_presents),
		(b" rebuilt=", rebuilt),
		(b" second=", second_presents),
		(b" generation=", generation as u32),
		(b" phase=", phase_index as u32),
		(b" events=", frames.events_seen()),
		(b" configures=", frames.configures_seen()),
		// HOW MANY TIMES THE SCREEN CHANGED HANDS, which is the number a harness taking the screen
		// away and giving it back reads to see that this loop NOTICED - a client that kept drawing
		// while hidden and a client that never heard look identical in a frame count.
		(b" visibility=", frames.visibility_seen()),
		(b" background=", background),
		(b" visible=", frames.pacing().visible() as u32),
		// MICROSECONDS, because a nanosecond figure printed per frame is six digits nobody reads and
		// the budget this is compared against is stated in milliseconds.
		(b" draw-mean-us=", (draw_total_ns / draw_count.max(1) / 1_000) as u32),
		(b" draw-worst-us=", (draw_worst_ns / 1_000) as u32),
		(b" interval-mean-us=", (interval_total_ns / interval_count.max(1) / 1_000) as u32),
		(b" interval-worst-us=", (interval_worst_ns / 1_000) as u32),
	] {
		line.extend_from_slice(name);
		push_number(&mut line, value);
	}
	line.push(b'\n');
	print(&line);
	print(b"test2d-sw: done\n");
	exit()
}

/// What a key did.
enum Action {
	None,
	Exit,
}

/// THE CONTROLS A PERSON HAS, and they are the ones every interactive program in this tree has:
/// Esc or q exits, Space pauses, R resets, the arrows scroll the clipped column.
fn key_action(code: u16, pressed: bool, scene: &mut Scene) -> Action {
	if !pressed {
		return Action::None;
	}
	match code {
		usage::ESCAPE | usage::Q => return Action::Exit,
		usage::SPACE => scene.paused = !scene.paused,
		usage::R => {
			scene.tick = 0;
			scene.scroll = 0.0;
			scene.paused = false;
		}
		usage::UP => scene.scroll -= 13.0,
		usage::DOWN => scene.scroll += 13.0,
		usage::LEFT => scene.scroll -= 3.0,
		usage::RIGHT => scene.scroll += 3.0,
		_ => {}
	}
	Action::None
}

/// One key event, or nothing - without blocking.
fn read_key(stream: u64, frame: &mut [u8; 32]) -> Option<(u16, bool)> {
	// THE MULTI-CAPABILITY POLL, and it is a POLL: the frame loop decides when this program waits, so
	// a key read that blocked would stop the animation whenever nobody was typing. It also takes
	// every capability the message carried - the single-handle receive drops the rest.
	match try_recv_caps(stream, frame) {
		PolledCaps::Message { len, mut handles } => {
			let event = input::subscribe_keys_read(&frame[..len], &mut handles);
			for handle in handles.as_slice() {
				close(*handle);
			}
			event.map(|event| (event.code, event.pressed))
		}
		_ => None,
	}
}

/// Replay the list into the acquired image.
fn draw(backend: &mut Soft2d<'_>, provider: &Forms, images: &Images, list: &DrawList, frame: &graphics_app::Frame) -> bool {
	let span = match frame.layout.backend_access_span(true) {
		Some(span) => span as usize,
		None => return false,
	};
	// SAFETY: the mapping is live for as long as the frame is held, and the span is the layout's own
	// answer for what a backend may touch.
	let bytes = unsafe { core::slice::from_raw_parts_mut(frame.addr as *mut u8, span) };
	let Ok(mut view) = ImageViewMut::new(frame.layout.clone(), bytes) else { return false };
	let format = match frame.layout.storage {
		PixelStorage::Known(format) => format,
		_ => return false,
	};
	let description = TargetDescription { extent: frame.layout.extent, format, color_space: ColorSpace::Srgb, scale: 1.0, luminance: graphics_core::pixel::OutputLuminance::UNKNOWN };
	let mut backend = core::mem::replace(backend, Soft2d::new()).with_images(images).with_glyphs(provider);
	let ok = match backend.prepare(list, &description) {
		Ok(prepared) => backend.render(&prepared, &mut view).is_ok(),
		Err(_) => false,
	};
	ok
}

/// The union of two rectangles.
fn union_of(left: RectF, right: RectF) -> RectF {
	let x = left.x.min(right.x);
	let y = left.y.min(right.y);
	RectF::new(x, y, left.right().max(right.right()) - x, left.bottom().max(right.bottom()) - y)
}

/// A rectangle as the surface spells one, ROUNDED OUTWARDS and clipped to the surface.
///
/// OUTWARDS ON EVERY SIDE, because a damage rectangle rounded inwards leaves the antialiased edge of
/// whatever moved on the screen - and CLIPPED, because a rectangle that leaves the surface is a
/// present the service refuses rather than a hint it trims.
fn clamp_rect(rect: RectF, extent: Extent2D) -> surface::Rect {
	let left = rect.x.max(0.0) as u32;
	let top = rect.y.max(0.0) as u32;
	let right = ((rect.right() + 1.0).max(0.0) as u32).min(extent.width);
	let bottom = ((rect.bottom() + 1.0).max(0.0) as u32).min(extent.height);
	surface::Rect { x: left.min(extent.width), y: top.min(extent.height), width: right.saturating_sub(left), height: bottom.saturating_sub(top) }
}

fn report_phase(phase: Phase) {
	let mut line: Vec<u8> = Vec::new();
	line.extend_from_slice(b"test2d-sw: phase ");
	line.extend_from_slice(phase.name());
	line.push(b'\n');
	print(&line);
}

fn push_number(out: &mut Vec<u8>, value: u32) {
	if value == 0 {
		out.push(b'0');
		return;
	}
	let mut digits = [0u8; 10];
	let mut count = 0;
	let mut value = value;
	while value > 0 {
		digits[count] = b'0' + (value % 10) as u8;
		value /= 10;
		count += 1;
	}
	while count > 0 {
		count -= 1;
		out.push(digits[count]);
	}
}
