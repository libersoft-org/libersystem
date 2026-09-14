// textconf - the text and render conformance run, executed inside a booted guest.
//
// WHY THIS IS A PROGRAM AND NOT A HOST TEST. "The host tests pass on all three architectures" has no
// executable meaning here: a host test runs on the machine that built the image, once, on one
// architecture. What the three-architecture claim is ABOUT is the stack running on the target - the
// same shaping, the same layout, the same rasteriser arithmetic - and the only way to make that claim
// checkable is to run it there. So this is a staged executable, launched under its own permission
// manifest, and what it prints is the verdict the suite reads.
//
// AND IT IS GOVERNED, which is the second half. It holds ONE capability - `font-catalogue` - and
// reaches its face through it: list the installed faces, ask for the length of the one it wants,
// create a memory object, narrow a handle down to map and write, and have the catalogue fill it.
// A conformance program that opened a file would be testing a stack that no application can use.
//
// THE ORACLE IS THE GEOMETRY AND NOT A PREVIOUS RUN. Every glyph of the staged face is an
// axis-aligned ring, so the coverage of each pixel is an exact product of two one-dimensional
// overlaps - a number this program computes from the outline the face declares, without asking the
// rasteriser anything. A baseline image captured from `soft2d` would agree with `soft2d` by
// construction, which is what makes a recorded baseline a regression test rather than a conformance
// one.
//
// TWO SIZES, AND THE SECOND IS THE INTERESTING ONE. At 50 pixels to the em every edge of this face
// lands on a pixel boundary, so every pixel is fully covered or not at all and the frozen edge
// policy compares it EXACTLY. At 37 the edges fall mid-pixel, so most of the outline's boundary is
// partially covered and the frozen antialiasing tolerance applies - 2/255 per pixel and 1/255 mean.
// A run at the aligned size alone would never exercise a tolerance; a run at the fractional size
// alone would never exercise the exactness.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use font_client::FontClient;
use font_contract::Fixed266;
use font_parse::Face;
use font_parse::glyf::{Outline, Point, PointKind};
use font_run::{Request, produce};
use graphics_core::geom::{Extent2D, PointF};
use graphics_core::layout::{ImageLayout, RowOrigin};
use graphics_core::semantics::ImageSemantics;
use graphics_core::{AlphaMode, ColorSpace, OwnedImage, PixelFormat, PixelStorage};
use proto::generated::liber::font::v1 as font;
use proto::system::LaunchContext;
use render2d::Canvas;
use render2d::backend::{Backend, TargetDescription};
use render2d::paint::{Color, Paint};
use render2d::path::PathBuilder;
use rt::*;
use soft2d::Soft2d;
use soft2d::glyph::{GlyphImage, GlyphProvider};

/// The family this run is about. NAMED rather than "the first face": a catalogue with two faces
/// would otherwise make the answer depend on the order it happened to enumerate them.
const FAMILY: &str = "LiberSystem Last Resort";

/// The string this run shapes, lays out and draws.
///
/// EVERY CHARACTER OF IT IS COVERED BY THE STAGED FACE and one of them is the space, which the face
/// draws as NOTHING. A corpus of letters alone would never show that the blank glyph advances the
/// pen without marking the target - which is the one case where drawing what the face says and
/// drawing something are different.
const CORPUS: &str = "Ab Cd";

/// The two sizes, in pixels to the em. See the note at the top of this file for why there are two.
const ALIGNED_SIZE: f64 = 50.0;
const FRACTIONAL_SIZE: f64 = 37.0;

/// The target, and where the baseline sits in it.
const WIDTH: u32 = 256;
const HEIGHT: u32 = 72;
const BASELINE: f64 = 56.0;
const ORIGIN_X: f64 = 4.0;

/// The frozen render2d thresholds, restated here as the numbers they are.
///
/// A PIXEL WHOSE ANALYTIC COVERAGE IS EXACTLY 0 OR EXACTLY 1 IS COMPARED EXACTLY. Only a partially
/// covered pixel carries a tolerance, and it carries two: an absolute bound per pixel, and a bound
/// on the MEAN over the covered region - which is what stops a systematic bias passing because every
/// pixel is within one step of it.
const PER_PIXEL: f64 = 2.0 / 255.0;
const MEAN: f64 = 1.0 / 255.0;

/// AND THE SAME BOUND IN BOTH DIRECTIONS, which it was not when this run was first written.
///
/// `soft2d` used to be exact in X and SAMPLED IN Y at a sixteenth of a row, which put a
/// near-horizontal edge up to 8/255 out - four times the frozen bound - and this comparison was what
/// measured it. The rasteriser accumulates area now and meets the bound in both directions, so the
/// comparison applies the frozen numbers everywhere and the direction an edge runs in is reported
/// rather than tolerated.

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	inherit_stdout(bootstrap);
	let _context: Option<LaunchContext> = recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode);
	// THE ONE CAPABILITY. A conformance program holding a volume client would be proving something
	// about a stack nothing else can reach.
	let catalogue: u64 = match recv_tagged(bootstrap, &mut buf, b"FONT") {
		Some(handle) => handle,
		None => {
			print(b"textconf: FAILED no font catalogue capability\n");
			exit();
		}
	};

	let bytes: Vec<u8> = match load_face(catalogue) {
		Ok(bytes) => bytes,
		Err(problem) => {
			print(format!("textconf: FAILED {problem}\n").as_bytes());
			exit();
		}
	};
	print(format!("textconf: loaded {} bytes of '{FAMILY}' through the catalogue\n", bytes.len()).as_bytes());

	let face = match Face::open(&bytes, 0) {
		Ok(face) => face,
		Err(error) => {
			print(format!("textconf: FAILED the staged face does not open: {error:?}\n").as_bytes());
			exit();
		}
	};

	let mut failures = 0usize;
	for (size, exact) in [(ALIGNED_SIZE, true), (FRACTIONAL_SIZE, false)] {
		match run_one(&face, &bytes, size, exact) {
			Ok(report) => print(format!("textconf: {report}\n").as_bytes()),
			Err(problem) => {
				print(format!("textconf: FAILED at {size} px: {problem}\n").as_bytes());
				failures += 1;
			}
		}
	}
	if failures == 0 {
		print(b"textconf: PASSED the corpus renders to the analytic oracle at both sizes\n");
	}
	exit();
}

/// The face's bytes, through the catalogue and through nothing else.
fn load_face(catalogue: u64) -> Result<Vec<u8>, alloc::string::String> {
	let mut client = FontClient::new(catalogue);
	let faces: Vec<font::FaceRecord> = match client.list() {
		Some(Ok(faces)) => faces,
		Some(Err(error)) => return Err(format!("the catalogue refused the listing ({error:?})")),
		None => return Err(alloc::string::String::from("no answer from the font catalogue")),
	};
	let Some(record) = faces.iter().find(|face| face.family == FAMILY) else {
		return Err(format!("the catalogue lists {} face(s) and none is '{FAMILY}'", faces.len()));
	};
	let info = match client.resolve_info(&record.identity) {
		Some(Ok(info)) => info,
		Some(Err(error)) => return Err(format!("the catalogue refused to size the face ({error:?})")),
		None => return Err(alloc::string::String::from("no answer to resolve-info")),
	};
	if info.length == 0 || info.length > 8 * 1024 * 1024 {
		return Err(format!("the catalogue answered a length of {} bytes", info.length));
	}

	// THE HANDLE IS NARROWED BEFORE THE SEND, and the original is kept: `duplicate` narrows and never
	// widens, so what the catalogue receives can map and write and nothing else, and this program
	// still holds the object it created.
	let object = memory_object_create(info.length);
	if object < 0 {
		return Err(format!("a {} byte object could not be created ({object})", info.length));
	}
	let object = object as u64;
	let narrowed = duplicate(object, RIGHT_READ | RIGHT_WRITE | RIGHT_MAP | RIGHT_TRANSFER);
	if narrowed < 0 {
		return Err(format!("the object handle could not be narrowed ({narrowed})"));
	}
	let written = match client.resolve_into(&record.identity, info.generation, narrowed as u64) {
		Some(Ok(font::ResolveOutcome::Filled(written))) => written,
		Some(Ok(outcome)) => return Err(format!("the catalogue did not fill the object: {outcome:?}")),
		Some(Err(error)) => return Err(format!("the catalogue refused the read ({error:?})")),
		None => return Err(alloc::string::String::from("no answer to resolve-into")),
	};
	if written != info.length {
		return Err(format!("the catalogue wrote {written} of {} bytes", info.length));
	}
	let Some(base) = (unsafe { map_object(object) }) else {
		return Err(alloc::string::String::from("the filled object could not be mapped"));
	};
	// SAFETY: the kernel mapped the whole object at `base` and it stays mapped until `unmap_object`,
	// which is below; `written` is the catalogue's own answer and is checked against the length the
	// object was created with.
	let bytes = unsafe { core::slice::from_raw_parts(base as *const u8, written as usize) }.to_vec();
	unmap_object(object);
	Ok(bytes)
}

/// One size: shape, lay out, draw, and compare against the analytic oracle.
fn run_one(face: &Face<'_>, bytes: &[u8], size: f64, exact: bool) -> Result<alloc::string::String, alloc::string::String> {
	let units = face_units(face)?;
	let scale = size / units as f64;
	let (recorded, placed) = shape_and_place(face, bytes, size)?;
	let drawn = draw(&recorded, face, size)?;
	let (oracle, cut_in_y) = analytic(face, &placed, scale)?;
	let report = compare(&drawn, &oracle, &cut_in_y, exact)?;

	// AND THE COMPARISON PROVES IT REFUSES, here rather than in a separate run. A conformance
	// program that only ever agreed with itself would keep agreeing if its comparison quietly
	// stopped comparing - and in a guest there is no second process to notice. One pixel of shift is
	// the discriminating mutation: it leaves the same ink, the same count and the same mean, and
	// moves every edge.
	let mut shifted = alloc::vec![0.0f64; oracle.len()];
	for y in 0..HEIGHT {
		for x in 1..WIDTH {
			shifted[(y * WIDTH + x) as usize] = oracle[(y * WIDTH + x - 1) as usize];
		}
	}
	let mut shifted_cut = alloc::vec![false; cut_in_y.len()];
	for y in 0..HEIGHT {
		for x in 1..WIDTH {
			shifted_cut[(y * WIDTH + x) as usize] = cut_in_y[(y * WIDTH + x - 1) as usize];
		}
	}
	if compare(&drawn, &shifted, &shifted_cut, exact).is_ok() {
		return Err(alloc::string::String::from("the comparison accepted an oracle shifted by one pixel, so it is not comparing"));
	}
	Ok(format!("{size} px: {report}, and a one-pixel shift is refused"))
}

fn face_units(face: &Face<'_>) -> Result<u16, alloc::string::String> {
	Ok(face.header.units_per_em)
}

/// One glyph, where the layout put it.
///
/// THE POSITIONS ARE 26.6 AND NOT FLOATING POINT, because that is the arithmetic the renderer
/// itself does: it advances the pen in 26.6 and converts once, at the end, to place the outline. An
/// oracle that carried exact real positions would disagree with the renderer by the rounding of
/// every advance, and would be blaming the rasteriser for the run's own quantisation.
struct Placed {
	glyph: u16,
	/// Where this glyph's origin sits, in 26.6 device pixels - the pen the renderer will have
	/// reached by the time it draws this glyph, computed the way the renderer computes it.
	pen: Fixed266,
}

/// Shape the corpus, produce the shared run, and lay it out.
///
/// THROUGH THE ORDERED PIPELINE AND THROUGH THE SEAM, not around either. The stages exist so that a
/// caller cannot reorder before it wraps; `font-run` exists so that font units become 26.6 in ONE
/// place; `text-layout` exists so that where a run sits on a line is decided once. A conformance run
/// that did any of those itself would be exercising a path no application takes - and would agree
/// with itself about the arithmetic it was supposed to be checking.
fn shape_and_place(face: &Face<'_>, bytes: &[u8], size: f64) -> Result<(render2d::list::RecordedGlyphRun, Vec<Placed>), alloc::string::String> {
	use font_shape::Buffer;
	use font_shape::buffer::GlyphInfo;

	let source = text_pipeline::Source::new(CORPUS).map_err(|error| format!("the corpus is refused: {error:?}"))?;
	let items = text_pipeline::stages::itemise(source);
	let levelled = text_pipeline::stages::resolve_levels(items, unicode_bidi::ParagraphDirection::Auto);
	if levelled.levels.paragraph_level != 0 {
		return Err(format!("the corpus is left to right and the pipeline answered level {}", levelled.levels.paragraph_level));
	}
	if levelled.items.items.len() != 1 {
		return Err(format!("the corpus is one item and the pipeline found {}", levelled.items.items.len()));
	}

	let characters: Vec<char> = CORPUS.chars().collect();
	let mut buffer = Buffer::default();
	let mut at = 0u32;
	for character in &characters {
		let glyph = face.glyph_for(*character).map_err(|error| format!("the face refused a character: {error:?}"))?.ok_or_else(|| format!("the staged face has no glyph for U+{:04X}", *character as u32))?;
		buffer.infos.push(GlyphInfo::new(glyph, at));
		buffer.positions.push(font_shape::Position::default());
		at += character.len_utf8() as u32;
	}
	font_shape::shape_run(face, &mut buffer, &characters, *b"dflt").map_err(|error| format!("shaping refused the corpus: {error:?}"))?;

	let identity = font_contract::FaceRef { face: font_contract::FaceIdentity { file: font_contract::face::FileIdentity(digest_of(bytes)), index: 0 }, generation: font_contract::face::Generation(1) };
	let request = Request { face, identity, size: fixed(size), variation: font_contract::VariationCoordinates::default(), script: font_contract::ScriptTag::from_bytes(*b"latn"), direction: font_contract::Direction::LeftToRight, mode: font_contract::glyph::RasterisationMode::Grayscale, origin: (fixed(ORIGIN_X), fixed(BASELINE)), text: CORPUS, start: 0, end: CORPUS.len() };
	let run = produce(&request, &buffer).map_err(|error| format!("the run could not be produced: {error:?}"))?;
	let map = run.cluster_map().ok_or_else(|| alloc::string::String::from("the produced run has no cluster map"))?;

	// THE LINE, COMPOSED. One run makes this a short composition and not a pointless one: it is where
	// the run's LEFT edge on the line is decided, and a renderer is handed that rather than the pen
	// the shaper finished at.
	// TEXT-LAYOUT HAS ITS OWN DIRECTION TYPE and reads no text at all, which is what keeps its
	// decisions checkable on numbers alone. The pipeline's answer above is what chooses which one.
	let line = text_layout::compose::compose(&[(run.run(), map)], fixed(WIDTH as f64 - ORIGIN_X * 2.0), text_layout::ParagraphDirection::LeftToRight).map_err(|error| format!("the line could not be composed: {error:?}"))?;
	let first = line.runs.first().ok_or_else(|| alloc::string::String::from("the composed line has no runs"))?;

	let shared = first.run;
	let mut placed = Vec::new();
	let mut pen = shared.origin_x;
	for glyph in shared.glyphs {
		let index = u16::try_from(glyph.glyph).map_err(|_| format!("glyph {} is not one this face has", glyph.glyph))?;
		placed.push(Placed { glyph: index, pen });
		pen = pen.checked_add(glyph.x_advance).map_err(|error| format!("the pen left the coordinate space: {error:?}"))?;
	}

	let recorded = render2d::list::RecordedGlyphRun { face: shared.face, size: shared.size, variation: shared.variation, script: shared.script, direction: shared.direction, mode: shared.mode, origin_x: shared.origin_x, origin_y: shared.origin_y, glyphs: shared.glyphs.to_vec(), clusters: run.clusters().to_vec() };
	Ok((recorded, placed))
}

/// The alpha channel of the target, after the run is drawn through `render2d` into `soft2d`.
fn draw(run: &render2d::list::RecordedGlyphRun, face: &Face<'_>, size: f64) -> Result<Vec<f64>, alloc::string::String> {
	let semantics = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Straight };
	let storage = PixelStorage::Known(PixelFormat::R8G8B8A8Unorm);
	let layout = ImageLayout::new(Extent2D::new(WIDTH, HEIGHT), WIDTH * 4, storage, RowOrigin::TopLeft, semantics).map_err(|error| format!("the target layout is refused: {error:?}"))?;
	let mut image = OwnedImage::new(layout).map_err(|error| format!("the target could not be allocated: {error:?}"))?;

	let mut canvas = Canvas::new();
	canvas.draw_glyph_run(run.clone(), Paint::Solid(Color::new(1.0, 1.0, 1.0, 1.0, ColorSpace::Srgb))).map_err(|error| format!("the run was refused by the drawing API: {error:?}"))?;
	let list = canvas.finish().map_err(|error| format!("the list was refused: {error:?}"))?;

	let provider = FaceGlyphs { face, scale: size / face.header.units_per_em as f64, asked: core::cell::Cell::new(0), answered: core::cell::Cell::new(0), refusal: core::cell::Cell::new(None) };
	let mut backend = Soft2d::new().with_glyphs(&provider);
	let description = TargetDescription { extent: image.layout().extent, format: PixelFormat::R8G8B8A8Unorm, color_space: ColorSpace::Srgb, scale: 1.0, luminance: graphics_core::pixel::OutputLuminance::UNKNOWN };
	let prepared = backend.prepare(&list, &description).map_err(|error| format!("the backend refused to prepare: {error:?}"))?;
	{
		let mut view = image.view_mut();
		backend.render(&prepared, &mut view).map_err(|error| format!("the backend refused to render: {error:?}"))?;
	}

	// WHAT THE GLYPH CACHE ASKED FOR, which is the evidence that it keys correctly: five glyphs in
	// the run and two distinct forms behind them, one of which is the space and has no outline at all.
	print(
		format!(
			"textconf: the provider was asked for {} form(s) and produced {}{}\n",
			provider.asked.get(),
			provider.answered.get(),
			match provider.refusal.get() {
				Some(error) => format!(", refusing one with {error:?}"),
				None => alloc::string::String::new(),
			}
		)
		.as_bytes(),
	);
	let view = image.view();
	let mut alpha = Vec::with_capacity((WIDTH * HEIGHT) as usize);
	for y in 0..HEIGHT {
		let row = view.row(y).ok_or_else(|| format!("the target has no row {y}"))?;
		for x in 0..WIDTH {
			alpha.push(row[x as usize * 4 + 3] as f64 / 255.0);
		}
	}
	Ok(alpha)
}

/// A provider that walks the face's own outlines into device-pixel paths.
struct FaceGlyphs<'a> {
	face: &'a Face<'a>,
	scale: f64,
	asked: core::cell::Cell<u32>,
	answered: core::cell::Cell<u32>,
	refusal: core::cell::Cell<Option<font_parse::Error>>,
}

impl GlyphProvider for FaceGlyphs<'_> {
	fn glyph(&self, key: &font_contract::cache::GlyphCacheKey) -> GlyphImage {
		self.asked.set(self.asked.get() + 1);
		let mut builder = Builder { builder: PathBuilder::new(), scale: self.scale, open: false, drawn: false, failed: false };
		let mut points = [Point { x: 0, y: 0, kind: PointKind::OnCurve, ends_contour: false }; 256];
		let mut deltas = [(0i16, 0i16); 256];
		let mut touched = [false; 256];
		let scratch = font_parse::gvar::Scratch { points: &mut points, deltas: &mut deltas, touched: &mut touched };
		// THE SEAM CARRIES A 32-BIT GLYPH AND A FACE HAS AT MOST 65535 OF THEM, so a key naming one
		// above that names a glyph no face has - which draws nothing rather than the wrong thing.
		let Ok(glyph) = u16::try_from(key.glyph) else { return GlyphImage::Missing };
		if let Err(error) = font_parse::outline::walk(self.face, glyph, &[], scratch, &mut builder) {
			self.refusal.set(Some(error));
			return GlyphImage::Missing;
		}
		if builder.failed {
			// A GLYPH THE PROVIDER HAS NO FORM FOR DRAWS NOTHING rather than a substitute: a renderer
			// inventing a glyph is a renderer lying about what the text says.
			return GlyphImage::Missing;
		}
		// AN EMPTY GLYPH IS NOT A MISSING ONE, and the difference is the space: a face that draws
		// nothing for it is doing the one correct thing, so the answer is an empty form rather than a
		// refusal. `drawn` is separate from `open` because the last point of the last contour CLOSES
		// it - so "a contour is open" is false at the end of every well-formed glyph.
		if !builder.drawn {
			return GlyphImage::Missing;
		}
		self.answered.set(self.answered.get() + 1);
		GlyphImage::Outline(builder.builder.finish())
	}
}

/// The walk, turned into a path. THE Y AXIS IS FLIPPED HERE and nowhere else: a font's outline is
/// measured up from the baseline and a device's pixels are counted down from the top.
struct Builder {
	builder: PathBuilder,
	scale: f64,
	/// Whether a contour is open and the next point continues it.
	open: bool,
	/// Whether ANY point was ever taken, which is what says the glyph has an outline at all.
	drawn: bool,
	failed: bool,
}

impl Outline for Builder {
	fn point(&mut self, point: Point) -> bool {
		let at = PointF { x: (point.x as f64 * self.scale) as f32, y: (-(point.y as f64) * self.scale) as f32 };
		let result = if self.open { self.builder.line_to(at) } else { self.builder.move_to(at) };
		if result.is_err() {
			self.failed = true;
			return false;
		}
		self.open = true;
		self.drawn = true;
		if point.ends_contour {
			if self.builder.close().is_err() {
				self.failed = true;
				return false;
			}
			self.open = false;
		}
		true
	}
}

/// THE ORACLE. Every contour of this face is an axis-aligned rectangle, so a pixel's coverage is the
/// product of its overlap with the rectangle in each axis - an exact number, computed from the
/// outline the face declares and from nothing the rasteriser did.
///
/// TWO CONTOURS WOUND OPPOSITE WAYS MAKE A RING under the non-zero rule, and the oracle says so the
/// same way the face does: coverage is the outer rectangle's, less the inner one's.
///
/// IT ALSO ANSWERS WHICH DIRECTION CUT EACH PIXEL, because the rasteriser is exact in one of them and
/// sampled in the other, and a comparison that could not tell them apart would have to apply the
/// weaker bound everywhere.
fn analytic(face: &Face<'_>, placed: &[Placed], scale: f64) -> Result<(Vec<f64>, Vec<bool>), alloc::string::String> {
	let mut coverage = alloc::vec![0.0f64; (WIDTH * HEIGHT) as usize];
	let mut cut_in_y = alloc::vec![false; (WIDTH * HEIGHT) as usize];
	for entry in placed {
		let rectangles = contours_of(face, entry.glyph)?;
		if rectangles.is_empty() {
			continue;
		}
		let pen = pixels_of(entry.pen);
		for y in 0..HEIGHT {
			for x in 0..WIDTH {
				let mut value = 0.0f64;
				let mut partial_in_y = false;
				for (index, rectangle) in rectangles.iter().enumerate() {
					let left = pen + rectangle.0 as f64 * scale;
					let right = pen + rectangle.2 as f64 * scale;
					// The outline is measured up from the baseline; the device counts down.
					let top = BASELINE - rectangle.3 as f64 * scale;
					let bottom = BASELINE - rectangle.1 as f64 * scale;
					let across = overlap(x as f64, x as f64 + 1.0, left, right);
					let down = overlap(y as f64, y as f64 + 1.0, top, bottom);
					// The first contour fills and every later one is wound the other way, which under
					// the non-zero rule takes coverage away.
					if index == 0 {
						value += across * down;
					} else {
						value -= across * down;
					}
					// A HORIZONTAL EDGE CUT THIS PIXEL if its vertical overlap with a contour that
					// reaches it at all is a fraction. That is the case the sweep samples.
					if across > 0.0 && down > 0.0 && down < 1.0 {
						partial_in_y = true;
					}
				}
				let at = (y * WIDTH + x) as usize;
				coverage[at] = (coverage[at] + value.clamp(0.0, 1.0)).clamp(0.0, 1.0);
				cut_in_y[at] |= partial_in_y;
			}
		}
	}
	Ok((coverage, cut_in_y))
}

/// A pixel measurement in the 26.6 the renderer uses. The rounding rule is the format's own: a
/// value exactly half way goes to even, which is what keeps a sum of advances from drifting.
fn fixed(pixels: f64) -> Fixed266 {
	let raw = pixels * 64.0;
	let floor = whole(raw);
	let fraction = raw - floor;
	let rounded = if fraction > 0.5 {
		floor + 1.0
	} else if fraction < 0.5 {
		floor
	} else if (floor / 2.0) == whole(floor / 2.0) {
		floor
	} else {
		floor + 1.0
	};
	Fixed266::from_raw(rounded as i32)
}

/// The same value as a real number, which is exact: a 26.6 value is an integer over sixty-four.
fn pixels_of(value: Fixed266) -> f64 {
	value.raw() as f64 / 64.0
}

/// `floor` without a maths library. This program is `no_std` and the only values it floors are
/// small, so the truncation towards zero that a cast performs needs correcting for negatives alone.
fn whole(value: f64) -> f64 {
	let truncated = value as i64 as f64;
	if value < truncated { truncated - 1.0 } else { truncated }
}

/// How much of `[a, b)` lies inside `[c, d)`.
fn overlap(a: f64, b: f64, c: f64, d: f64) -> f64 {
	let low = if a > c { a } else { c };
	let high = if b < d { b } else { d };
	if high > low { high - low } else { 0.0 }
}

/// The bounding rectangle of each contour of a glyph, in font units.
///
/// A CONTOUR OF THIS FACE IS ITS OWN BOUNDING BOX, which is what makes the oracle exact - and the
/// check below is what keeps that true: a face whose contour is not a rectangle would make this
/// oracle a claim rather than a computation, so it is refused rather than approximated.
fn contours_of(face: &Face<'_>, glyph: u16) -> Result<Vec<(i16, i16, i16, i16)>, alloc::string::String> {
	struct Contours {
		current: Vec<(i16, i16)>,
		all: Vec<Vec<(i16, i16)>>,
		curved: bool,
	}
	impl Outline for Contours {
		fn point(&mut self, point: Point) -> bool {
			if !matches!(point.kind, PointKind::OnCurve) {
				self.curved = true;
			}
			self.current.push((point.x, point.y));
			if point.ends_contour {
				self.all.push(core::mem::take(&mut self.current));
			}
			true
		}
	}
	let mut out = Contours { current: Vec::new(), all: Vec::new(), curved: false };
	let mut points = [Point { x: 0, y: 0, kind: PointKind::OnCurve, ends_contour: false }; 256];
	let mut deltas = [(0i16, 0i16); 256];
	let mut touched = [false; 256];
	let scratch = font_parse::gvar::Scratch { points: &mut points, deltas: &mut deltas, touched: &mut touched };
	font_parse::outline::walk(face, glyph, &[], scratch, &mut out).map_err(|error| format!("glyph {glyph} could not be walked: {error:?}"))?;
	if out.curved {
		return Err(format!("glyph {glyph} carries a curve, and this oracle is exact only for axis-aligned rectangles"));
	}
	let mut rectangles = Vec::new();
	for contour in &out.all {
		if contour.len() != 4 {
			return Err(format!("glyph {glyph} has a contour of {} points, and this oracle is exact only for rectangles", contour.len()));
		}
		let left = contour.iter().map(|point| point.0).min().unwrap_or(0);
		let bottom = contour.iter().map(|point| point.1).min().unwrap_or(0);
		let right = contour.iter().map(|point| point.0).max().unwrap_or(0);
		let top = contour.iter().map(|point| point.1).max().unwrap_or(0);
		// Every point of a rectangle is on one of its two horizontal and one of its two vertical
		// lines. A quadrilateral that is not axis-aligned would pass the count above and not this.
		for point in contour {
			if (point.0 != left && point.0 != right) || (point.1 != bottom && point.1 != top) {
				return Err(format!("glyph {glyph} has a contour that is not an axis-aligned rectangle"));
			}
		}
		rectangles.push((left, bottom, right, top));
	}
	Ok(rectangles)
}

/// The comparison, against the frozen render2d thresholds.
fn compare(drawn: &[f64], oracle: &[f64], cut_in_y: &[bool], exact: bool) -> Result<alloc::string::String, alloc::string::String> {
	// EVERY PIXEL OF THE TARGET IS COMPARED. There is no border exclusion and no mask, because an
	// excluded border is where clipping, coverage and the edge rule all fail at once.
	let mut worst_across = 0.0f64;
	let mut worst_across_at = 0usize;
	let mut worst_down = 0.0f64;
	let mut worst_down_at = 0usize;
	let mut sum = 0.0f64;
	let mut partial = 0usize;
	let mut sampled = 0usize;
	let mut solid = 0usize;
	for (at, (got, want)) in drawn.iter().zip(oracle.iter()).enumerate() {
		let difference = (got - want).abs();
		if *want <= 0.0 || *want >= 1.0 {
			solid += 1;
			// A FULLY COVERED PIXEL THAT DIFFERS AT ALL is a colour error wearing an antialiasing
			// tolerance, so it is compared exactly - which for an 8-bit target means the stored byte.
			if difference > 0.5 / 255.0 {
				return Err(format!("pixel ({}, {}) has analytic coverage {want} and was drawn {got}, and a pixel that is wholly covered or wholly clear is compared exactly; drawn ink {} and expected ink {}", at as u32 % WIDTH, at as u32 / WIDTH, extent(drawn), extent(oracle)));
			}
			continue;
		}
		partial += 1;
		sum += difference;
		if cut_in_y.get(at).copied().unwrap_or(false) {
			sampled += 1;
			if difference > worst_down {
				worst_down = difference;
				worst_down_at = at;
			}
		} else if difference > worst_across {
			worst_across = difference;
			worst_across_at = at;
		}
	}
	if exact && partial != 0 {
		return Err(format!("this size was chosen so every edge lands on a pixel boundary and {partial} pixel(s) are partially covered"));
	}
	// THE DIRECTION IS REPORTED AND NOT TOLERATED. Both carry the frozen bound; keeping them apart is
	// what tells a reader WHICH axis a regression is in, which is the difference between "the
	// rasteriser is out" and "the vertical sweep is out".
	if worst_across > PER_PIXEL {
		return Err(format!("pixel ({}, {}) is cut by a VERTICAL edge and differs by {worst_across}, and the frozen per-pixel bound is {PER_PIXEL}", worst_across_at as u32 % WIDTH, worst_across_at as u32 / WIDTH));
	}
	if worst_down > PER_PIXEL {
		return Err(format!("pixel ({}, {}) is cut by a HORIZONTAL edge and differs by {worst_down}, and the frozen per-pixel bound is {PER_PIXEL}", worst_down_at as u32 % WIDTH, worst_down_at as u32 / WIDTH));
	}
	let mean = if partial == 0 { 0.0 } else { sum / partial as f64 };
	// THE MEAN BOUND IS THE FROZEN ONE AND IT APPLIES TO THE WHOLE COVERED REGION, because what it
	// exists to catch is a systematic bias - and a bias that lived only in the sampled direction
	// would be exactly as wrong.
	if mean > MEAN {
		return Err(format!("the mean difference over the covered region is {mean} and the frozen bound is {MEAN}"));
	}
	Ok(format!("{solid} exact pixel(s), {partial} partial ({sampled} cut by a horizontal edge), worst across {worst_across:.6}, worst down {worst_down:.6}, mean {mean:.6}"))
}

/// Where the ink is, as a rectangle. A comparison that failed at one pixel leaves the next reader
/// unable to tell "nothing was drawn" from "everything was drawn one pixel to the left", and the two
/// are different defects.
fn extent(coverage: &[f64]) -> alloc::string::String {
	let mut bounds: Option<(u32, u32, u32, u32)> = None;
	let mut lit = 0usize;
	for (at, value) in coverage.iter().enumerate() {
		if *value <= 0.0 {
			continue;
		}
		lit += 1;
		let (x, y) = (at as u32 % WIDTH, at as u32 / WIDTH);
		bounds = Some(match bounds {
			None => (x, y, x, y),
			Some((left, top, right, bottom)) => (left.min(x), top.min(y), right.max(x), bottom.max(y)),
		});
	}
	match bounds {
		None => alloc::string::String::from("nowhere"),
		Some((left, top, right, bottom)) => format!("{lit} pixel(s) in ({left},{top})..({right},{bottom})"),
	}
}

/// The face's identity for the glyph cache key. It has to be a function of the BYTES: two faces
/// sharing an identity would share cache entries, which is the stale-pixel case the key exists for.
fn digest_of(bytes: &[u8]) -> [u8; 32] {
	let mut out = [0u8; 32];
	let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
	for (index, byte) in bytes.iter().enumerate() {
		hash ^= *byte as u64;
		hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
		out[index % 32] ^= (hash >> 24) as u8;
	}
	out
}
