//! THE DRAWING AND THE COMPARISON every run of this program goes through: runs drawn through
//! `render2d` into `soft2d`, an oracle computed from the outlines the face declares, and the comparison
//! against the frozen render2d thresholds.
//!
//! THE ORACLE IS THE GEOMETRY AND NOT A PREVIOUS RUN. Every glyph this program draws is made of
//! axis-aligned rectangles - rings in the staged last-resort face, solid boxes in the conformance
//! corpus - so the coverage of each pixel is an exact product of two one-dimensional overlaps: a
//! number computed from the outline the face declares, without asking the rasteriser anything. A
//! baseline image captured from `soft2d` would agree with `soft2d` by construction, which is what makes
//! a recorded baseline a regression test rather than a conformance one.
//!
//! PURE, and kept apart from the program's capabilities for that reason: nothing here sends, receives
//! or prints. What a caller is told along the way arrives through the `note` it passes in.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use font_contract::Fixed266;
use font_parse::Face;
use font_parse::glyf::{Outline, Point, PointKind};
use graphics_core::geom::{Extent2D, PointF};
use graphics_core::layout::{ImageLayout, RowOrigin};
use graphics_core::semantics::ImageSemantics;
use graphics_core::{AlphaMode, ColorSpace, OwnedImage, PixelFormat, PixelStorage};
use render2d::Canvas;
use render2d::backend::{Backend, TargetDescription};
use render2d::list::RecordedGlyphRun;
use render2d::paint::{Color, Paint};
use render2d::path::PathBuilder;
use soft2d::Soft2d;
use soft2d::glyph::{GlyphImage, GlyphProvider};

/// The frozen render2d thresholds, restated here as the numbers they are.
///
/// A PIXEL WHOSE ANALYTIC COVERAGE IS EXACTLY 0 OR EXACTLY 1 IS COMPARED EXACTLY. Only a partially
/// covered pixel carries a tolerance, and it carries two: an absolute bound per pixel, and a bound
/// on the MEAN over the covered region - which is what stops a systematic bias passing because every
/// pixel is within one step of it.
///
/// AND THE SAME BOUND IN BOTH DIRECTIONS, which it was not when this program was first written.
/// `soft2d` used to be exact in X and SAMPLED IN Y at a sixteenth of a row, which put a
/// near-horizontal edge up to 8/255 out - four times the frozen bound - and this comparison was what
/// measured it. The rasteriser accumulates area now and meets the bound in both directions, so the
/// comparison applies the frozen numbers everywhere and the direction an edge runs in is reported
/// rather than tolerated.
pub const PER_PIXEL: f64 = 2.0 / 255.0;
pub const MEAN: f64 = 1.0 / 255.0;

/// The target a line is drawn into, and where the line sits in it - in whole device pixels, so that a
/// size at which every edge of a face lands on a pixel boundary keeps that property once placed.
#[derive(Clone, Copy)]
pub struct Target {
	pub width: u32,
	pub height: u32,
	pub baseline: f64,
	pub origin_x: f64,
}

/// One glyph where the oracle says it belongs.
///
/// THE POSITION IS 26.6 AND NOT FLOATING POINT, because that is the arithmetic the renderer itself
/// does: it advances the pen in 26.6 and converts once, at the end, to place the outline. An oracle
/// that carried exact real positions would disagree with the renderer by the rounding of every
/// advance, and would be blaming the rasteriser for the run's own quantisation.
pub struct Placed {
	pub glyph: u16,
	/// The glyph's origin in device pixels: the pen, with the glyph's own offset applied.
	pub x: Fixed266,
	pub y: Fixed266,
}

/// Draw `runs`, build the oracle from `placed`, compare - and prove the comparison refuses.
///
/// THE COMPARISON PROVES IT REFUSES, here rather than in a separate run. A conformance program that
/// only ever agreed with itself would keep agreeing if its comparison quietly stopped comparing - and
/// in a guest there is no second process to notice. One pixel of shift is the discriminating
/// mutation: it leaves the same ink, the same count and the same mean, and moves every edge.
pub fn check(face: &Face<'_>, runs: &[RecordedGlyphRun], placed: &[Placed], size: f64, exact: bool, target: Target, note: &mut dyn FnMut(&str)) -> Result<String, String> {
	let scale = size / face.header.units_per_em as f64;
	let drawn = draw(runs, face, size, target, note)?;
	let (oracle, cut_in_y) = analytic(face, placed, scale, target)?;
	let report = compare(&drawn, &oracle, &cut_in_y, exact, target)?;
	let (shifted, shifted_cut) = shifted_right(&oracle, &cut_in_y, target);
	if compare(&drawn, &shifted, &shifted_cut, exact, target).is_ok() {
		return Err(String::from("the comparison accepted an oracle shifted by one pixel, so it is not comparing"));
	}
	Ok(format!("{report}, and a one-pixel shift is refused"))
}

/// The same oracle, one pixel to the right.
fn shifted_right(oracle: &[f64], cut_in_y: &[bool], target: Target) -> (Vec<f64>, Vec<bool>) {
	let mut shifted = alloc::vec![0.0f64; oracle.len()];
	let mut shifted_cut = alloc::vec![false; cut_in_y.len()];
	for y in 0..target.height {
		for x in 1..target.width {
			shifted[(y * target.width + x) as usize] = oracle[(y * target.width + x - 1) as usize];
			shifted_cut[(y * target.width + x) as usize] = cut_in_y[(y * target.width + x - 1) as usize];
		}
	}
	(shifted, shifted_cut)
}

/// The alpha channel of the target, after the runs are drawn through `render2d` into `soft2d`.
pub fn draw(runs: &[RecordedGlyphRun], face: &Face<'_>, size: f64, target: Target, note: &mut dyn FnMut(&str)) -> Result<Vec<f64>, String> {
	let semantics = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Straight };
	let storage = PixelStorage::Known(PixelFormat::R8G8B8A8Unorm);
	let layout = ImageLayout::new(Extent2D::new(target.width, target.height), target.width * 4, storage, RowOrigin::TopLeft, semantics).map_err(|error| format!("the target layout is refused: {error:?}"))?;
	let mut image = OwnedImage::new(layout).map_err(|error| format!("the target could not be allocated: {error:?}"))?;

	let mut canvas = Canvas::new();
	for run in runs {
		canvas.draw_glyph_run(run.clone(), Paint::Solid(Color::new(1.0, 1.0, 1.0, 1.0, ColorSpace::Srgb))).map_err(|error| format!("the run was refused by the drawing API: {error:?}"))?;
	}
	let list = canvas.finish().map_err(|error| format!("the list was refused: {error:?}"))?;

	let provider = FaceGlyphs { face, scale: size / face.header.units_per_em as f64, asked: core::cell::Cell::new(0), answered: core::cell::Cell::new(0), refusal: core::cell::Cell::new(None) };
	let mut backend = Soft2d::new().with_glyphs(&provider);
	let description = TargetDescription { extent: image.layout().extent, format: PixelFormat::R8G8B8A8Unorm, color_space: ColorSpace::Srgb, scale: 1.0, luminance: graphics_core::pixel::OutputLuminance::UNKNOWN };
	let prepared = backend.prepare(&list, &description).map_err(|error| format!("the backend refused to prepare: {error:?}"))?;
	{
		let mut view = image.view_mut();
		backend.render(&prepared, &mut view).map_err(|error| format!("the backend refused to render: {error:?}"))?;
	}

	// WHAT THE GLYPH CACHE ASKED FOR, which is the evidence that it keys correctly: one form per
	// distinct glyph, and none at all for a glyph with no outline.
	note(&format!(
		"the provider was asked for {} form(s) and produced {}{}",
		provider.asked.get(),
		provider.answered.get(),
		match provider.refusal.get() {
			Some(error) => format!(", refusing one with {error:?}"),
			None => String::new(),
		}
	));
	let view = image.view();
	let mut alpha = Vec::with_capacity((target.width * target.height) as usize);
	for y in 0..target.height {
		let row = view.row(y).ok_or_else(|| format!("the target has no row {y}"))?;
		for x in 0..target.width {
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

/// THE ORACLE. Every contour drawn here is an axis-aligned rectangle, so a pixel's coverage is the
/// product of its overlap with the rectangle in each axis - an exact number, computed from the outline
/// the face declares and from nothing the rasteriser did.
///
/// TWO CONTOURS WOUND OPPOSITE WAYS MAKE A RING under the non-zero rule, and the oracle says so the
/// same way the face does: a glyph's coverage is its first rectangle's, less every later one's.
///
/// IT ALSO ANSWERS WHICH DIRECTION CUT EACH PIXEL, because a regression in the rasteriser is in one of
/// them, and a comparison that could not tell them apart would report "the rasteriser is out" where it
/// could have said which sweep.
///
/// AND IT REFUSES A PIXEL TWO GLYPHS EACH COVER PARTLY. Where one glyph covers a pixel wholly the
/// answer is a whole pixel however the run is composited, and where a glyph covers it not at all the
/// other glyph's coverage is the answer. Where two cover it in part, what the pixel holds is decided
/// by how the renderer composites one glyph over another - an operator, not geometry - so an oracle
/// that computed it would be asserting a rule nothing froze. The sizes this program draws at keep
/// every such pixel away, and this is what proves they do.
pub fn analytic(face: &Face<'_>, placed: &[Placed], scale: f64, target: Target) -> Result<(Vec<f64>, Vec<bool>), String> {
	let pixels = (target.width * target.height) as usize;
	let mut coverage = alloc::vec![0.0f64; pixels];
	let mut cut_in_y = alloc::vec![false; pixels];
	// Which glyph left a partial coverage at each pixel, by its index in `placed`; `u32::MAX` for none.
	let mut partial_from = alloc::vec![u32::MAX; pixels];
	for (index, entry) in placed.iter().enumerate() {
		let rectangles = contours_of(face, entry.glyph)?;
		let Some(outer) = rectangles.first() else { continue };
		let origin_x = pixels_of(entry.x);
		let origin_y = pixels_of(entry.y);
		// ONLY THE PIXELS THE OUTER RECTANGLE CAN REACH: every pixel outside it overlaps nothing, so it
		// adds nothing and cuts nothing, and walking the whole target per glyph is time an emulated
		// guest pays for no answer.
		let (left, right) = (origin_x + outer.0 as f64 * scale, origin_x + outer.2 as f64 * scale);
		let (top, bottom) = (origin_y - outer.3 as f64 * scale, origin_y - outer.1 as f64 * scale);
		let (first_x, last_x) = (span_start(left, target.width), span_end(right, target.width));
		let (first_y, last_y) = (span_start(top, target.height), span_end(bottom, target.height));
		for y in first_y..last_y {
			for x in first_x..last_x {
				let mut value = 0.0f64;
				let mut partial_in_y = false;
				for (contour, rectangle) in rectangles.iter().enumerate() {
					let left = origin_x + rectangle.0 as f64 * scale;
					let right = origin_x + rectangle.2 as f64 * scale;
					// The outline is measured up from the baseline; the device counts down.
					let top = origin_y - rectangle.3 as f64 * scale;
					let bottom = origin_y - rectangle.1 as f64 * scale;
					let across = overlap(x as f64, x as f64 + 1.0, left, right);
					let down = overlap(y as f64, y as f64 + 1.0, top, bottom);
					// The first contour fills and every later one is wound the other way, which under
					// the non-zero rule takes coverage away.
					if contour == 0 {
						value += across * down;
					} else {
						value -= across * down;
					}
					// A HORIZONTAL EDGE CUT THIS PIXEL if its vertical overlap with a contour that
					// reaches it at all is a fraction.
					if across > 0.0 && down > 0.0 && down < 1.0 {
						partial_in_y = true;
					}
				}
				let at = (y * target.width + x) as usize;
				let value = value.clamp(0.0, 1.0);
				if value > 0.0 && value < 1.0 {
					if partial_from[at] != u32::MAX && partial_from[at] != index as u32 {
						return Err(format!("pixel ({x}, {y}) is covered in part by glyph {} and in part by glyph {} of the run, and what two partial coverages make is the renderer's compositing rather than geometry this oracle can compute", partial_from[at], index));
					}
					partial_from[at] = index as u32;
				}
				coverage[at] = (coverage[at] + value).clamp(0.0, 1.0);
				cut_in_y[at] |= partial_in_y;
			}
		}
	}
	Ok((coverage, cut_in_y))
}

/// The first pixel a span starting at `low` can touch, inside a target of `extent` pixels.
fn span_start(low: f64, extent: u32) -> u32 {
	let floor = whole(low);
	if floor <= 0.0 {
		0
	} else if floor >= extent as f64 {
		extent
	} else {
		floor as u32
	}
}

/// One past the last pixel a span ending at `high` can touch.
fn span_end(high: f64, extent: u32) -> u32 {
	let ceiling = -whole(-high);
	if ceiling <= 0.0 {
		0
	} else if ceiling >= extent as f64 {
		extent
	} else {
		ceiling as u32
	}
}

/// A pixel measurement in the 26.6 the renderer uses. The rounding rule is the format's own: a
/// value exactly half way goes to even, which is what keeps a sum of advances from drifting.
pub fn fixed(pixels: f64) -> Fixed266 {
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
pub fn pixels_of(value: Fixed266) -> f64 {
	value.raw() as f64 / 64.0
}

/// `floor` without a maths library. This program is `no_std` and the only values it floors are
/// small, so the truncation towards zero that a cast performs needs correcting for negatives alone.
pub fn whole(value: f64) -> f64 {
	let truncated = value as i64 as f64;
	if value < truncated { truncated - 1.0 } else { truncated }
}

/// How much of `[a, b)` lies inside `[c, d)`.
fn overlap(a: f64, b: f64, c: f64, d: f64) -> f64 {
	let low = if a > c { a } else { c };
	let high = if b < d { b } else { d };
	if high > low { high - low } else { 0.0 }
}

/// The bounding rectangle of each contour of a glyph, in font units, as `(left, bottom, right, top)`.
///
/// A CONTOUR DRAWN HERE IS ITS OWN BOUNDING BOX, which is what makes the oracle exact - and the check
/// below is what keeps that true: a face whose contour is not a rectangle would make this oracle a
/// claim rather than a computation, so it is refused rather than approximated.
pub fn contours_of(face: &Face<'_>, glyph: u16) -> Result<Vec<(i16, i16, i16, i16)>, String> {
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
pub fn compare(drawn: &[f64], oracle: &[f64], cut_in_y: &[bool], exact: bool, target: Target) -> Result<String, String> {
	let width = target.width;
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
				return Err(format!("pixel ({}, {}) has analytic coverage {want} and was drawn {got}, and a pixel that is wholly covered or wholly clear is compared exactly; drawn ink {} and expected ink {}", at as u32 % width, at as u32 / width, extent(drawn, width), extent(oracle, width)));
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
		return Err(format!("pixel ({}, {}) is cut by a VERTICAL edge and differs by {worst_across}, and the frozen per-pixel bound is {PER_PIXEL}", worst_across_at as u32 % width, worst_across_at as u32 / width));
	}
	if worst_down > PER_PIXEL {
		return Err(format!("pixel ({}, {}) is cut by a HORIZONTAL edge and differs by {worst_down}, and the frozen per-pixel bound is {PER_PIXEL}", worst_down_at as u32 % width, worst_down_at as u32 / width));
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
fn extent(coverage: &[f64], width: u32) -> String {
	let mut bounds: Option<(u32, u32, u32, u32)> = None;
	let mut lit = 0usize;
	for (at, value) in coverage.iter().enumerate() {
		if *value <= 0.0 {
			continue;
		}
		lit += 1;
		let (x, y) = (at as u32 % width, at as u32 / width);
		bounds = Some(match bounds {
			None => (x, y, x, y),
			Some((left, top, right, bottom)) => (left.min(x), top.min(y), right.max(x), bottom.max(y)),
		});
	}
	match bounds {
		None => String::from("nowhere"),
		Some((left, top, right, bottom)) => format!("{lit} pixel(s) in ({left},{top})..({right},{bottom})"),
	}
}

/// A face's identity for the glyph cache key. It has to be a function of the BYTES: two faces sharing
/// an identity would share cache entries, which is the stale-pixel case the key exists for.
pub fn digest_of(bytes: &[u8]) -> [u8; 32] {
	let mut out = [0u8; 32];
	let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
	for (index, byte) in bytes.iter().enumerate() {
		hash ^= *byte as u64;
		hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
		out[index % 32] ^= (hash >> 24) as u8;
	}
	out
}
