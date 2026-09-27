//! THE TWO PHASES, and the tile loop between them.
//!
//! `prepare` VALIDATES, FLATTENS, STROKES, BINS, BUILDS PYRAMIDS AND RESERVES. Everything that
//! depends on the list and the target but not on the frame happens once, and what it cannot fit it
//! refuses BEFORE any pixel is written - a frame that fails halfway through a filter chain leaves a
//! half-drawn picture on the screen, which is worse than not drawing it.
//!
//! `render` REPLAYS AND ALLOCATES NOTHING, and a counting allocator holds it to that rather than this
//! sentence. Every buffer it uses came out of the reservation: the coverage row, the clip masks, the
//! clip and layer stacks, the layer surfaces, a filter's node table and the tile's working copy are a
//! LANE's, a gradient's ramp, a blur's kernel and every glyph's edges are the prepared list's, and the
//! shader table keeps its storage from one frame to the next. A frame that repeats with the same list
//! and the same target therefore costs what the drawing costs and nothing else.
//!
//! AND THE REPLAY IS CUT INTO UNITS that the caller's `Workers` run on its lanes - bands of tile rows,
//! or tiles through a tile-major intermediate - each replayed in the serial order of its own tiles,
//! so every schedule draws the pixels the serial walk draws.
//!
//! AND THE REPLAY IS TILED. Each tile decodes its own rectangle of the target once, replays the
//! commands binned to it, and encodes once - so the conversion is at the edges of a tile rather than
//! inside its command loop, and the working set is a tile rather than a surface.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use graphics_core::geom::{Extent2D, PixelRect, PointF, RectF};
use graphics_core::pixel::TransferTable;
use graphics_core::pixel::{Rgba, Working};
use graphics_core::sample::Pyramid;
use graphics_core::{ImageView, ImageViewMut};
use render2d::backend::{Backend, Prepared, TargetDescription};
use render2d::blend::{Antialias, BlendMode, Operator};
use render2d::flatten::Contour;
use render2d::list::{Command, DrawList, ImageRecord};
use render2d::paint::{ImageQuality, Paint};
use render2d::path::{FillRule, Path, PathBuilder, StrokeStyle};
use render2d::prepared::PreparedKey;
use render2d::transform::Transform;
use render2d::{Error, resource::FilterHandle};

use crate::clip::{ClipLevel, ClipStack, write_mask_row};
use crate::glyph::{GlyphImage, GlyphProvider, GlyphRaster, NoGlyphs};
use crate::layer::{Layer, layer_bounds};
use crate::paint::{ImageLookup, Ramp, Shader, ramp_for, shader, to_working};
use crate::raster::Rasteriser;
use crate::stroke::{StrokeParameters, dashed, outline};
use crate::target::{ImageSource, NoImages, Raster};
use crate::tile::{Bins, Tiling, cover};
use crate::workers::{Access, Lane, Serial, Unit, UnitKind, Workers};
use crate::{BACKEND_NAME, BACKEND_VERSION, TILE_SIZE};

/// Whether a frame should stop.
///
/// A SURFACE THAT WAS CLOSED OR RESIZED MID-FRAME IS A FRAME NOBODY WILL SEE. Finishing it costs the
/// whole drawing for nothing, and on a resize it costs it at the wrong size - so the loop asks
/// between tiles, which is often enough to stop promptly and rare enough to cost nothing.
///
/// `Sync`, BECAUSE EVERY LANE ASKS IT before each unit it takes, from whichever worker runs the lane.
pub trait Cancellation: Sync {
	fn cancelled(&self) -> bool;
}

/// One command, with everything that can be worked out before the frame.
enum Step {
	Fill {
		edges: crate::raster::Edges,
		rule: FillRule,
		paint: Paint,
		transform: Transform,
		antialias: Antialias,
		blend: BlendMode,
		operator: Operator,
		opacity: f32,
	},
	Image {
		edges: crate::raster::Edges,
		paint: Paint,
		transform: Transform,
		blend: BlendMode,
		operator: Operator,
		opacity: f32,
	},
	/// A RUN'S GLYPHS, DECODED AND PLACED AT `prepare`: the replay reads only these and never the
	/// cache or the provider, so a lane needs neither - and two lanes meeting the same glyph is not a
	/// race, because nobody writes the cache while a frame replays.
	Glyphs {
		placed: Vec<Placed>,
		paint: Paint,
		transform: Transform,
		blend: BlendMode,
		operator: Operator,
		opacity: f32,
	},
	/// A stroke that is an ALIASED ONE-PIXEL POLYLINE, which is a diagram's grid, a chart's axis and a
	/// pixel-exact rule - and which the coverage rasteriser would spend sixteen sub-scanlines on to
	/// produce the same one pixel per column.
	AliasedLines {
		points: Vec<(i32, i32)>,
		paint: Paint,
		transform: Transform,
		blend: BlendMode,
		operator: Operator,
		opacity: f32,
	},
	PushClip {
		edges: crate::raster::Edges,
		rule: FillRule,
		antialias: Antialias,
		bounds: PixelRect,
		inverse: bool,
	},
	/// A CLIP THAT IS AN AXIS-ALIGNED, PIXEL-ALIGNED RECTANGLE, which needs no mask at all.
	///
	/// The clip stack has always had a rectangle level that costs no storage and no per-pixel
	/// multiply - its own header says so - and nothing ever pushed one: every clip rasterised its
	/// edges into a full mask. A scrolling list of fifty rows clips fifty times, in every tile of the
	/// frame, and each of those was sixty-four rows of coverage written into a mask that says
	/// `255` inside a rectangle and `0` outside.
	///
	/// PIXEL-ALIGNED IS PART OF THE CONDITION AND NOT A DETAIL. A rectangle whose edge falls between
	/// two pixel centres has PARTIAL coverage along that edge, and a rectangle level answers `1` or
	/// `0` - so the fast path is taken only where the two are the same answer, which is when every
	/// edge is an integer.
	PushClipRect {
		bounds: PixelRect,
	},
	PushClipMask {
		image: render2d::resource::ImageHandle,
		transform: Transform,
		inverse: bool,
	},
	PopClip,
	BeginLayer {
		bounds: Option<RectF>,
		opacity: f32,
		blend: BlendMode,
		operator: Operator,
		filter: Option<FilterHandle>,
	},
	EndLayer,
}

/// One glyph of a run, where it lands on the target and in the form it is drawn in.
///
/// AN OUTLINE OR A COLOUR LAYER IS FLATTENED AT ITS PLACED ORIGIN AND EDGE-BUILT THERE, once per placed
/// glyph - exactly what a tile did with it before, moved and not changed - so its edges and its pixels
/// are the same; like a fill's edges they are the prepared form and not scratch. A MASK OR A BITMAP is
/// held by reference, sharing the cache's entry, with the arithmetic that placed it done here.
/// NOTHING IS PRE-RASTERISED: an outline turned into a mask would composite through the coverage gamma
/// that masks go through and outlines do not, and every text pixel would move.
enum Placed {
	Outline(crate::raster::Edges),
	Layers(Vec<(crate::raster::Edges, Rgba)>),
	Mask { form: Arc<GlyphImage>, x: i64, y: i64 },
	Bitmap { form: Arc<GlyphImage>, x: i64, y: i64 },
}

/// What `prepare` produced.
pub struct SoftPrepared {
	key: PreparedKey,
	steps: Vec<Step>,
	bounds: Vec<Option<PixelRect>>,
	bins: Bins,
	tiling: Tiling,
	pyramids: Vec<(u32, Pyramid)>,
	images: Vec<ImageRecord>,
	/// Each command's gradient ramp, resolved into the working space here - `None` for a paint that is
	/// not a gradient.
	ramps: Vec<Option<Ramp>>,
	filters: Vec<render2d::filter::FilterGraph>,
	/// Each filter graph's blur kernels, in node order, computed with the graph.
	kernels: Vec<Vec<Option<(Vec<f32>, Vec<f32>)>>>,
	working: Working,
	target_transfer: graphics_profile::image::Transfer,
	/// What the destination said it can show, carried from the description to the one place that
	/// encodes into it.
	output: graphics_core::pixel::OutputLuminance,
	expansion: u32,
	scratch_bytes: u64,
	damage: Option<PixelRect>,
	/// THE PART OF EACH TILE THE ROUND TRIP COVERS - see `tile_regions`.
	regions: Vec<PixelRect>,
	/// WHICH TILES NEED NO DECODE OF THE TARGET, one flag per tile - see `tiles_without_backdrop`.
	/// Worked out here rather than per frame, because it is a function of the list and the target
	/// and a prepared list is already bound to both.
	no_backdrop: Vec<bool>,
	/// THE LANES THIS FRAME RUNS WITH - what the ceiling left room for, at most what the pool offered
	/// and at most the units it is cut into - and HOW IT IS CUT: each unit a run of tiles, in serial
	/// order.
	lanes: usize,
	unit_kind: UnitKind,
	units: Vec<(u32, u32)>,
	/// One tile-major slot's length in bytes, when the units are tiles on more than one lane.
	slot_bytes: usize,
}

impl SoftPrepared {
	/// THE CONSERVATIVE DAMAGE of the whole list: the union of every command's bound, clipped to the
	/// target. Exact damage would mean comparing old and new pixels - a source-over with alpha zero
	/// changes nothing while covering a rectangle - and that cost defeats the purpose.
	pub fn damage(&self) -> Option<PixelRect> {
		self.damage
	}

	/// HOW MANY TILES THIS FRAME SKIPS THE DECODE FOR - see `tiles_without_backdrop`.
	///
	/// It is here so a fixture can assert the MECHANISM rather than only its pixels: a scene whose
	/// output is right because the optimisation never fired and one whose output is right because it
	/// fired correctly are indistinguishable from the target alone, and the first is what an
	/// accidentally disabled fast path looks like for ever.
	pub fn tiles_without_backdrop(&self) -> usize {
		self.no_backdrop.iter().filter(|skipped| **skipped).count()
	}

	/// THE DAMAGE OF ONE DRAW, which is what a compositor asks when it wants to know what a single
	/// command changed rather than what the frame did. It is the same conservative bound the binning
	/// used, clipped to the target, and it is EMPTY when the command reached no sample at all.
	pub fn command_damage(&self, command: usize) -> Option<PixelRect> {
		self.bounds.get(command).copied().flatten().filter(|rect| !rect.is_empty())
	}

	/// How many commands this preparation carries, so a caller can walk their damage.
	pub fn commands(&self) -> usize {
		self.bounds.len()
	}

	pub fn tiles(&self) -> usize {
		self.tiling.count()
	}

	/// THE LANES THIS FRAME RUNS WITH: what the prepared-scratch ceiling left room for beside the one
	/// lane every frame has, up to the lanes the pool offered and the units the frame is cut into.
	pub fn lanes(&self) -> usize {
		self.lanes
	}

	/// HOW MANY UNITS THE FRAME IS CUT INTO - bands or tiles that have something to draw.
	pub fn units(&self) -> usize {
		self.units.len()
	}

	/// What the units are.
	pub fn unit_kind(&self) -> UnitKind {
		self.unit_kind
	}

	/// HOW MANY IMAGES THIS FRAME SAMPLES THROUGH A DECODED COPY OR A PYRAMID, which a fixture asserts to
	/// know which copies the ceiling left a frame.
	pub fn decoded_images(&self) -> usize {
		self.pyramids.len()
	}
}

impl Prepared for SoftPrepared {
	fn key(&self) -> &PreparedKey {
		&self.key
	}

	fn scratch_bytes(&self) -> u64 {
		self.scratch_bytes
	}
}

/// The backend.
pub struct Soft2d<'a> {
	images: &'a dyn ImageSource,
	glyphs: &'a dyn GlyphProvider,
	cancellation: Option<&'a dyn Cancellation>,
	/// WHO RUNS A FRAME'S UNITS, held from construction as the image source is: the `Backend` trait's
	/// `prepare` and `render` carry no pool, and `prepare` is where the lanes are reserved.
	workers: &'a dyn Workers,
	unit_kind: UnitKind,
	cache: GlyphRaster,
	/// One lane per worker the last `prepare` reserved, the first of which every frame has.
	lanes: Vec<Lane>,
	/// STORAGE KEPT FROM ONE FRAME TO THE NEXT while nothing it held is: the shaders and the units borrow
	/// the frame, so they cannot outlive it - but their capacity can, which is the difference between a
	/// replay that allocates nothing and one that allocates per frame.
	shader_table: Vec<Shader<'static>>,
	unit_table: Vec<Unit<'static>>,
	/// Which units ran to their end, for the tile-major copy - one per unit.
	finished: Vec<bool>,
	/// THE TILE-MAJOR INTERMEDIATE, when a frame's units are tiles on more than one lane: one slot per
	/// unit, each a tile of the target's format.
	intermediate: Vec<u8>,
	/// THE TRANSFER FUNCTIONS THIS DRAWING NEEDS, built once in `prepare`. One per distinct function
	/// rather than one per image: four images in sRGB share one table, and building a table per image
	/// would spend more on the tables than the powers they replace.
	tables: Vec<(graphics_profile::image::Transfer, TransferTable)>,
}

/// The pool a backend nobody gave one runs on.
static SERIAL: Serial = Serial;

impl Default for Soft2d<'_> {
	fn default() -> Self {
		Self::new()
	}
}

impl<'a> Soft2d<'a> {
	/// A backend for a drawing that references no images and no glyphs.
	pub fn new() -> Self {
		Self { images: &NoImages, glyphs: &NoGlyphs, cancellation: None, workers: &SERIAL, unit_kind: UnitKind::Bands, cache: GlyphRaster::default(), lanes: Vec::new(), shader_table: Vec::new(), unit_table: Vec::new(), finished: Vec::new(), intermediate: Vec::new(), tables: Vec::new() }
	}

	/// Replay a frame's units on `workers`' lanes. `Serial` - this thread, unit after unit - is the
	/// default and the scalar reference.
	pub fn with_workers(mut self, workers: &'a dyn Workers) -> Self {
		self.workers = workers;
		self
	}

	/// Cut frames into units of this kind. MEASURED, NOT PREFERRED: the two kinds are what the choice of
	/// the unit was measured between, and a benchmark asks for each.
	pub fn with_units(mut self, kind: UnitKind) -> Self {
		self.unit_kind = kind;
		self
	}

	pub fn with_images(mut self, images: &'a dyn ImageSource) -> Self {
		self.images = images;
		self
	}

	pub fn with_glyphs(mut self, glyphs: &'a dyn GlyphProvider) -> Self {
		self.glyphs = glyphs;
		self
	}

	pub fn with_cancellation(mut self, cancellation: &'a dyn Cancellation) -> Self {
		self.cancellation = Some(cancellation);
		self
	}

	pub fn glyph_cache(&self) -> &GlyphRaster {
		&self.cache
	}

	pub fn glyph_cache_mut(&mut self) -> &mut GlyphRaster {
		&mut self.cache
	}

	/// Build the table for a transfer function if this backend has not got one.
	fn ensure_table(&mut self, transfer: graphics_profile::image::Transfer) {
		if !self.tables.iter().any(|(kind, _)| *kind == transfer) {
			self.tables.push((transfer, TransferTable::new(transfer)));
		}
	}

	fn table_for(tables: &[(graphics_profile::image::Transfer, TransferTable)], transfer: graphics_profile::image::Transfer) -> Option<&TransferTable> {
		tables.iter().find(|(kind, _)| *kind == transfer).map(|(_, table)| table)
	}

	/// Whether the frame in progress should stop. Asked between tiles.
	pub fn cancelled(&self) -> bool {
		self.cancellation.map(|cancellation| cancellation.cancelled()).unwrap_or(false)
	}
}

/// The device scale a transform applies, which is what a stroke width in user space is multiplied by.
fn device_scale(transform: &Transform) -> f32 {
	let scale = transform.approximate_scale();
	if scale.is_finite() && scale > 0.0 { scale } else { 1.0 }
}

/// The rectangle a set of contours covers, conservatively.
fn contour_bounds(contours: &[Contour]) -> Option<RectF> {
	let mut minimum = PointF { x: f32::INFINITY, y: f32::INFINITY };
	let mut maximum = PointF { x: f32::NEG_INFINITY, y: f32::NEG_INFINITY };
	for contour in contours {
		for point in &contour.points {
			if !(point.x.is_finite() && point.y.is_finite()) {
				continue;
			}
			minimum.x = minimum.x.min(point.x);
			minimum.y = minimum.y.min(point.y);
			maximum.x = maximum.x.max(point.x);
			maximum.y = maximum.y.max(point.y);
		}
	}
	(maximum.x >= minimum.x && maximum.y >= minimum.y).then(|| RectF::new(minimum.x, minimum.y, maximum.x - minimum.x, maximum.y - minimum.y))
}

/// THE RECTANGLE A COMMAND IS GUARANTEED TO LEAVE FULLY OPAQUE, or `None`.
///
/// WHY IT IS WORTH COMPUTING. Every tile is DECODED out of the target into the working space before
/// it is replayed and ENCODED back afterwards, and for a 640x480 frame that round trip alone was
/// twenty-one milliseconds against a sixteen-point-seven millisecond budget - the largest single term
/// in the simplest scene. A tile that some command overwrites completely, with pixels that owe
/// nothing to what was under them, never needed the decode: the whole point of reading the backdrop
/// is to blend with it.
///
/// EVERY CONDITION HERE IS NECESSARY AND THE SET IS DELIBERATELY SMALL. A solid, fully opaque paint
/// at full opacity, composited `Normal` over the backdrop or copied onto it, over a shape that is an
/// axis-aligned RECTANGLE. Anything else - a gradient, an image, a blend mode, a hairline of alpha -
/// can leave a pixel that depends on what was beneath it, and a backdrop that was never read is
/// whatever the previous tile left in the scratch.
///
/// THE ROUNDING IS INWARD, which is the opposite of every other bound in this backend. `cover` rounds
/// outward because a bound one pixel too small CLIPS a drawing; this rounds inward because a cover
/// one pixel too large would claim a partially covered pixel is fully painted - and that pixel would
/// then be composited against uninitialised scratch instead of against the backdrop.
fn opaque_cover(contours: &[Contour], paint: &Paint, opacity: f32, blend: BlendMode, operator: Operator) -> Option<RectF> {
	if opacity < 1.0 || !matches!(blend, BlendMode::Normal) || !matches!(operator, Operator::SrcOver | Operator::Src) {
		return None;
	}
	match paint {
		Paint::Solid(colour) if colour.alpha >= 1.0 => {}
		_ => return None,
	}
	axis_aligned_rect(contours)
}

/// The rectangle a flattened contour set IS, when it is exactly one axis-aligned rectangle.
///
/// FLATTENING HAS ALREADY HAPPENED, so this is asked of device-space points and answers about the
/// shape as it will actually be rasterised - a rotated rectangle is not one, and a rectangle under a
/// scale-and-translate transform still is.
fn axis_aligned_rect(contours: &[Contour]) -> Option<RectF> {
	let [contour] = contours else { return None };
	let points = &contour.points;
	// FOUR CORNERS, OR FIVE WITH THE FIRST REPEATED. A flattener may or may not close the ring
	// explicitly, and both spellings describe the same rectangle.
	let corners: &[PointF] = match points.len() {
		4 => points,
		5 if points[0] == points[4] => &points[..4],
		_ => return None,
	};
	for corner in corners {
		if !(corner.x.is_finite() && corner.y.is_finite()) {
			return None;
		}
	}
	// EVERY EDGE AXIS-ALIGNED, which is what makes the four points a rectangle rather than any
	// quadrilateral with the same bounding box.
	for index in 0..4 {
		let from = corners[index];
		let to = corners[(index + 1) % 4];
		if from.x != to.x && from.y != to.y {
			return None;
		}
	}
	let left = corners.iter().fold(f32::INFINITY, |least, point| least.min(point.x));
	let right = corners.iter().fold(f32::NEG_INFINITY, |most, point| most.max(point.x));
	let top = corners.iter().fold(f32::INFINITY, |least, point| least.min(point.y));
	let bottom = corners.iter().fold(f32::NEG_INFINITY, |most, point| most.max(point.y));
	// A DEGENERATE "RECTANGLE" IS NOT ONE. Two coincident corners describe a line, which covers
	// nothing at all.
	(right > left && bottom > top).then(|| RectF::new(left, top, right - left, bottom - top))
}

/// Whether every edge of a rectangle falls exactly on a pixel boundary.
///
/// IT IS WHAT LETS A RECTANGLE CLIP SKIP ITS MASK. A rectangle level answers one or zero, and an
/// edge between two pixel centres has an answer in between - so the two agree only here.
fn is_pixel_aligned(rect: RectF) -> bool {
	[rect.x, rect.y, rect.right(), rect.bottom()].into_iter().all(|edge| edge.is_finite() && libm::floorf(edge) == edge)
}

/// The pixels a float rectangle covers ENTIRELY: inward on every side. See `opaque_cover`.
fn covered(rect: RectF) -> PixelRect {
	if !(rect.x.is_finite() && rect.y.is_finite() && rect.width.is_finite() && rect.height.is_finite()) {
		return PixelRect::new(0, 0, 0, 0);
	}
	let left = libm::ceilf(rect.x).max(0.0);
	let top = libm::ceilf(rect.y).max(0.0);
	let right = libm::floorf(rect.right()).max(0.0);
	let bottom = libm::floorf(rect.bottom()).max(0.0);
	if right <= left || bottom <= top {
		return PixelRect::new(0, 0, 0, 0);
	}
	let clamp = |value: f32| value.min(u32::MAX as f32) as u32;
	PixelRect::new(clamp(left), clamp(top), clamp(right - left), clamp(bottom - top))
}

/// WHICH TILES NEED NO DECODE, worked out once in `prepare` rather than per frame.
///
/// A tile qualifies when some command in its own bin covers it completely and opaquely, with NO clip
/// pushed and NO layer open at that point in the list. The two depth counters are what make this
/// cheap and conservative at the same time: a clip could narrow the fill to less than the tile and a
/// layer would send it somewhere else entirely, and rather than reason about either, a tile whose
/// covering command is under one simply keeps its decode.
///
/// WHAT HAPPENS BEFORE THE COVERING COMMAND DOES NOT MATTER. Commands earlier in the bin blend
/// against scratch that holds the previous tile's pixels, and every one of those pixels is then
/// overwritten - the covering fill reaches all of them, which is what "covers it completely" means.
/// THE PART OF EACH TILE THAT CAN CHANGE, which is not always the whole of it.
///
/// A TILE IS DECODED AND RE-ENCODED WHOLE, and for a drawing of thin strokes that is sixty-four rows
/// of conversion to change three pixels of each. What CAN change is bounded by the commands binned to
/// the tile, and those bounds are already computed - so the round trip is over their union rather
/// than over the tile. Pixels outside it are read from the target and written back unchanged, which
/// is work with a known answer.
///
/// THE WHOLE TILE IS THE ANSWER WHENEVER ANYTHING IS NOT A PLAIN DRAW. A layer composites back over
/// its own bounds, which are a `BeginLayer` field rather than a binned bound; a filter reaches past
/// what it reads by its declared expansion; a clip mask is state. Rather than reason about each,
/// a tile whose bin holds any of them keeps its whole round trip - conservative, and it costs
/// nothing on the drawings this is for.
fn tile_regions(tiling: &Tiling, bins: &Bins, steps: &[Step], bounds: &[Option<PixelRect>]) -> Vec<PixelRect> {
	let mut answer: Vec<PixelRect> = Vec::with_capacity(tiling.count());
	for index in 0..tiling.count() {
		let tile = tiling.tile(index);
		let mut region: Option<PixelRect> = None;
		let mut whole = false;
		for command in bins.commands(index) {
			match steps.get(*command as usize) {
				Some(Step::BeginLayer { .. } | Step::EndLayer | Step::PushClipMask { .. }) => {
					whole = true;
					break;
				}
				Some(_) => {
					if let Some(bound) = bounds.get(*command as usize).copied().flatten() {
						let clipped = bound.intersection(&tile);
						if !clipped.is_empty() {
							region = Some(match region {
								Some(current) => union(current, clipped),
								None => clipped,
							});
						}
					}
				}
				None => {}
			}
		}
		answer.push(if whole { tile } else { region.unwrap_or(tile).intersection(&tile) });
	}
	answer
}

fn tiles_without_backdrop(tiling: &Tiling, bins: &Bins, steps: &[Step], covers: &[Option<PixelRect>]) -> Vec<bool> {
	let mut answer: Vec<bool> = Vec::with_capacity(tiling.count());
	for index in 0..tiling.count() {
		let tile = tiling.tile(index);
		let mut clips: u32 = 0;
		let mut layers: u32 = 0;
		let mut covered_tile = false;
		for command in bins.commands(index) {
			match steps.get(*command as usize) {
				Some(Step::PushClip { .. } | Step::PushClipRect { .. } | Step::PushClipMask { .. }) => clips += 1,
				Some(Step::PopClip) => clips = clips.saturating_sub(1),
				Some(Step::BeginLayer { .. }) => layers += 1,
				Some(Step::EndLayer) => layers = layers.saturating_sub(1),
				_ => {
					if clips == 0 && layers == 0 && covers.get(*command as usize).copied().flatten().is_some_and(|cover| cover.contains_rect(&tile)) {
						covered_tile = true;
						break;
					}
				}
			}
		}
		answer.push(covered_tile && !tile.is_empty());
	}
	answer
}

/// A rectangle as a path, which is how an image's destination enters the one rasteriser.
fn rect_contours(rect: RectF, transform: &Transform) -> Vec<Contour> {
	let mut builder = PathBuilder::new();
	if builder.add_rect(rect).is_err() {
		return Vec::new();
	}
	render2d::flatten::flatten(&builder.finish(), Some(transform))
}

impl<'a> Backend for Soft2d<'a> {
	type Prepared = SoftPrepared;

	fn identity(&self) -> (&'static str, u32) {
		(BACKEND_NAME, BACKEND_VERSION)
	}

	fn prepare(&mut self, list: &DrawList, target: &TargetDescription) -> Result<Self::Prepared, Error> {
		// THE VALIDATION BOUNDARY IS THE LIST'S OWN, once, and this asks it rather than re-checking:
		// a backend that re-checked would be the layer where the check that matters is the one nobody
		// wrote, and one that did not, over a list nobody had checked, is the other failure.
		list.validate()?;
		let working = Working::linear(target.color_space);
		self.ensure_table(target.color_space.transfer());
		let resources = list.resources();
		let mut steps: Vec<Step> = Vec::with_capacity(list.commands().len());
		let mut bounds: Vec<Option<PixelRect>> = Vec::with_capacity(list.commands().len());
		// WHAT EACH COMMAND LEAVES FULLY OPAQUE, beside what it can touch. The two are different
		// questions - one is conservative outward and the other conservative inward - and only the
		// second can say a tile's backdrop will not be read.
		let mut covers: Vec<Option<PixelRect>> = Vec::with_capacity(list.commands().len());
		let target_rect = PixelRect::new(0, 0, target.extent.width, target.extent.height);
		let mut widest_edges = 0usize;
		for command in list.commands() {
			let (step, bound) = match command {
				Command::FillPath { path, paint, rule, transform, antialias, blend, operator, opacity } => {
					let contours = flatten_handle(resources, path.0, transform)?;
					let bound = contour_bounds(&contours).map(cover).map(|rect| rect.intersection(&target_rect));
					widest_edges = widest_edges.max(edge_count(&contours));
					// THE ONLY COMMAND THAT CAN ANSWER THIS TODAY. A stroke's outline is a ring and
					// covers nothing solidly, an image's opacity is the image's business, and a glyph
					// run is glyphs - so the opaque cover is a filled rectangle's or nothing's.
					covers.push(opaque_cover(&contours, paint, *opacity, *blend, *operator).map(covered).map(|rect| rect.intersection(&target_rect)));
					(Step::Fill { edges: crate::raster::Edges::build(&contours), rule: *rule, paint: *paint, transform: *transform, antialias: *antialias, blend: *blend, operator: *operator, opacity: *opacity }, bound)
				}
				Command::StrokePath { path, paint, style, transform, antialias, blend, operator, opacity } => {
					// THE STROKE BECOMES A FILL HERE, IN `prepare`, and not per frame: the outline of
					// a stroke is a function of the path, the style and the transform, all of which a
					// prepared list is already bound to.
					let flattened = flatten_handle(resources, path.0, transform)?;
					let parameters = StrokeParameters::from_style(style, device_scale(transform));
					let dashed_contours = match style.dash {
						Some(dash) => {
							let pattern = resources.dashes.get(dash.pattern.0 as usize).ok_or(Error::UnknownResource { kind: render2d::resource::ResourceKind::Dashes, index: dash.pattern.0 })?;
							dashed(&flattened, pattern, dash.phase * device_scale(transform))
						}
						None => flattened,
					};
					// THE ALIASED ONE-PIXEL LINE IS AN EXPLICIT FAST PATH and keeps the rule it already
					// had: integer Bresenham with BOTH endpoints included, ties at `error == 0` broken
					// toward the smaller minor coordinate, and clipping applied BEFORE rasterising so
					// a clipped line covers the same pixels as the visible part of the unclipped one.
					if matches!(antialias, Antialias::Off) && parameters.width <= 1.0 && style.dash.is_none() {
						let points = integer_polyline(&dashed_contours);
						if let Some(points) = points {
							let bound = polyline_bounds(&points).map(|rect| rect.intersection(&target_rect));
							steps.push(Step::AliasedLines { points, paint: *paint, transform: *transform, blend: *blend, operator: *operator, opacity: *opacity });
							bounds.push(bound);
							covers.push(None);
							continue;
						}
					}
					let contours = outline(&dashed_contours, parameters);
					let bound = contour_bounds(&contours).map(cover).map(|rect| rect.intersection(&target_rect));
					widest_edges = widest_edges.max(edge_count(&contours));
					// AN OUTLINE IS FILLED NON-ZERO whatever rule the shape itself would use: the
					// overlaps of a self-crossing stroke are wound the same way, and even-odd would
					// punch a hole at every place the pen crossed its own path.
					(Step::Fill { edges: crate::raster::Edges::build(&contours), rule: FillRule::NonZero, paint: *paint, transform: *transform, antialias: *antialias, blend: *blend, operator: *operator, opacity: *opacity }, bound)
				}
				Command::DrawImage { image, source, destination, quality, transform, blend, operator, opacity } => {
					let contours = rect_contours(*destination, transform);
					let bound = contour_bounds(&contours).map(cover).map(|rect| rect.intersection(&target_rect));
					widest_edges = widest_edges.max(edge_count(&contours));
					// AN IMAGE IS AN IMAGE PAINT OVER ITS DESTINATION RECTANGLE, which is what makes
					// it one path through the rasteriser rather than a second blitter with its own
					// rules about edges.
					let mapping = source_to_destination(*source, *destination);
					let paint = Paint::Image { image: *image, source: *source, quality: *quality, spread: graphics_core::sample::Spread::Clamp, transform: mapping };
					(Step::Image { edges: crate::raster::Edges::build(&contours), paint, transform: *transform, blend: *blend, operator: *operator, opacity: *opacity }, bound)
				}
				Command::DrawGlyphRun { run, paint, transform, blend, operator, opacity } => {
					let bound = glyph_run_bounds(resources, run.0, transform).map(|rect| rect.intersection(&target_rect));
					// THE RUN IS DECODED AND PLACED HERE, and every distinct glyph is resolved through the
					// cache once - a miss decoded by the provider - so the replay touches neither.
					let placed = match resources.glyph_runs.get(run.0 as usize) {
						Some(recorded) => place_glyphs(&mut self.cache, self.glyphs, recorded, transform, working, &mut widest_edges),
						None => Vec::new(),
					};
					(Step::Glyphs { placed, paint: *paint, transform: *transform, blend: *blend, operator: *operator, opacity: *opacity }, bound)
				}
				Command::PushClipMask { image, transform, inverse } => (Step::PushClipMask { image: *image, transform: *transform, inverse: *inverse }, None),
				Command::PushClip { path, rule, transform, antialias, inverse } => {
					let contours = flatten_handle(resources, path.0, transform)?;
					let clip_bounds = contour_bounds(&contours).map(cover).unwrap_or(PixelRect::new(0, 0, 0, 0)).intersection(&target_rect);
					widest_edges = widest_edges.max(edge_count(&contours));
					// THE RECTANGLE FAST PATH, WHICH IS MOST OF THE CLIPS IN A USER INTERFACE. See
					// `Step::PushClipRect`: an inverse one is a hole and not a rectangle, so only the
					// ordinary direction takes it.
					if !*inverse
						&& let Some(rect) = axis_aligned_rect(&contours)
						&& is_pixel_aligned(rect)
					{
						steps.push(Step::PushClipRect { bounds: covered(rect).intersection(&target_rect) });
						bounds.push(None);
						covers.push(None);
						continue;
					}
					// A CLIP IS A STATE COMMAND AND GOES INTO EVERY TILE, even the ones its shape does
					// not reach: a tile that skipped the push would replay the rest of the list
					// unclipped, which is the bug that looks like a random rectangle of extra content.
					(Step::PushClip { edges: crate::raster::Edges::build(&contours), rule: *rule, antialias: *antialias, bounds: clip_bounds, inverse: *inverse }, None)
				}
				Command::PopClip => (Step::PopClip, None),
				Command::BeginLayer { bounds: layer, opacity, blend, operator, filter } => (Step::BeginLayer { bounds: *layer, opacity: *opacity, blend: *blend, operator: *operator, filter: *filter }, None),
				Command::EndLayer => (Step::EndLayer, None),
			};
			steps.push(step);
			bounds.push(bound);
			// EVERY OTHER COMMAND COVERS NOTHING THIS CAN PROVE, and the vectors stay the same length
			// as `steps` because they are indexed by the same command index.
			if covers.len() < steps.len() {
				covers.push(None);
			}
		}

		// THE FILTER EXPANSION IS WHAT A LAYER'S SCRATCH IS GROWN BY, and it is taken from the graphs
		// this list actually carries rather than from the profile's ceiling: reserving for a
		// two-hundred-and-fifty-six-pixel blur that nothing asks for is sixty megabytes of scratch a
		// drawing of rectangles would have to fit inside its budget.
		let mut expansion = 0u32;
		for graph in &resources.filters {
			let probe = RectF::new(0.0, 0.0, 1.0, 1.0);
			let needed = graph.required_input(probe);
			let grow = (probe.x - needed.x).max(probe.y - needed.y).max(needed.right() - probe.right()).max(needed.bottom() - probe.bottom());
			if grow.is_finite() && grow > 0.0 {
				expansion = expansion.max(libm::ceilf(grow) as u32);
			}
		}
		expansion = expansion.min(graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS.max_filter_radius);

		// THE PYRAMIDS, for the images this list samples with minification. Built here because a
		// pyramid is an allocation and a pyramid per frame is the allocation this design exists to
		// remove.
		let mut pyramids: Vec<(u32, Pyramid)> = Vec::new();
		// REQUIRED FIRST AND OPTIONAL AFTER, and the split is what keeps this from refusing a frame
		// that used to draw. A mipmapped draw NEEDS its pyramid - there is no other way to sample it -
		// and a bilinear or bicubic one merely goes faster with one. The optional ones are built here
		// and GIVEN BACK BELOW if the prepared-scratch budget turns out not to have room, because a
		// frame that used to draw must not start failing over an optimisation.
		let mut optional: Vec<bool> = Vec::new();
		for pass in 0..2u8 {
			for (index, record) in resources.images.iter().enumerate() {
				let required = wants_pyramid(list, index as u32);
				if pass == 0 && !required {
					continue;
				}
				if pass == 1 && (required || !wants_decoded(list, index as u32)) {
					continue;
				}
				let planar_space = self.images.planes(record.identity).map(|view| view.layout().color_space);
				let packed_space = self.images.image(record.identity).and_then(|view| view.layout().semantics.color_space());
				let Some(space) = planar_space.or(packed_space) else { continue };
				self.ensure_table(space.transfer());
				let table = Self::table_for(&self.tables, space.transfer());
				// A PLANAR SOURCE GETS ITS PYRAMID FROM THE SAME SAMPLER the drawing will use, so its
				// levels are the same decoded light rather than a second reconstruction.
				// A REQUIRED PYRAMID IS THE WHOLE CHAIN AND AN OPTIONAL ONE IS LEVEL ZERO. Bilinear and
				// bicubic read the source at its own resolution, so the halvings under it would be
				// prepare time and prepared memory that no draw touches.
				let built = match self.images.planes(record.identity) {
					Some(view) => graphics_core::sample::Sampler::planar(view, working, graphics_core::sample::Spread::Clamp, table).and_then(|sampler| if required { Pyramid::from_sampler(&sampler, working) } else { Pyramid::base_from_sampler(&sampler, working) }),
					None => {
						let Some(view) = self.images.image(record.identity) else { continue };
						if required { Pyramid::build_with(&view, working, table) } else { Pyramid::base_with(&view, working, table) }
					}
				};
				match built {
					Ok(pyramid) => {
						pyramids.push((index as u32, pyramid));
						optional.push(pass == 1);
					}
					// AN OPTIONAL ONE THAT WILL NOT BUILD IS NOT AN ERROR, for the same reason it is given
					// back below: the image samples the way it always did.
					Err(error) if pass == 0 => return Err(crate::target::from_core(error)),
					Err(_) => continue,
				}
			}
		}

		for record in resources.images.iter() {
			let space = self.images.planes(record.identity).map(|view| view.layout().color_space).or_else(|| self.images.image(record.identity).and_then(|view| view.layout().semantics.color_space()));
			if let Some(space) = space {
				self.ensure_table(space.transfer());
			}
		}
		let tiling = Tiling::new(target.extent, TILE_SIZE);
		let bins = Bins::build(&tiling, &bounds);
		let no_backdrop = tiles_without_backdrop(&tiling, &bins, &steps, &covers);
		let regions = tile_regions(&tiling, &bins, &steps, &bounds);
		let scratch_extent = (TILE_SIZE + expansion * 2, TILE_SIZE + expansion * 2);
		// ONE SURFACE PER OPEN LAYER, one per filter node of the largest graph, one for the blur's
		// second pass, and one for the tile itself.
		let layers_wanted = list.commands().iter().filter(|command| matches!(command, Command::BeginLayer { .. })).count();
		let filter_nodes = resources.filters.iter().map(|graph| graph.nodes().len()).max().unwrap_or(0);
		let surfaces = layers_wanted + filter_nodes + 2;
		// A RECTANGLE CLIP RESERVES NO MASK, because it never takes one - see `Step::PushClipRect`.
		// Counting it would be scratch nothing reads, charged against the prepared-scratch ceiling.
		let clips_wanted = steps.iter().filter(|step| matches!(step, Step::PushClip { .. } | Step::PushClipMask { .. })).count();
		// EVERY CLIP LEVEL a tile can push, rectangles included: what a lane's clip stack is reserved to.
		let clip_levels = steps.iter().filter(|step| matches!(step, Step::PushClip { .. } | Step::PushClipRect { .. } | Step::PushClipMask { .. })).count();
		let shape = LaneShape { surfaces, extent: scratch_extent, space: target.color_space, masks: clips_wanted, edges: widest_edges, clips: clip_levels, layers: layers_wanted, nodes: filter_nodes };

		// THE FIRST LANE IS SETTLED THE WAY THE ONE SET OF SCRATCH ALWAYS WAS, and every frame has it.
		if self.lanes.is_empty() {
			self.lanes.push(Lane::default());
		}
		shape.reserve(&mut self.lanes[0])?;
		let lane_bytes = self.lanes[0].scratch_bytes();
		let fixed = lane_bytes + bins.scratch_bytes();
		let ceiling = graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS.max_prepared_scratch_bytes;
		// THE OPTIONAL COPIES ARE GIVEN BACK BEFORE ANYTHING IS REFUSED, newest first, against the list's
		// own scratch and ONE lane's - so a frame that fitted before this optimisation existed still fits.
		// Each is a decoded source that makes an image sample faster, and giving one back is NOT free of
		// consequence: the copy holds its texels at half precision and the direct path decodes each tap
		// at single precision, so it can move an output pixel. Which is why the copies are settled here,
		// before a second lane is considered, and never displaced by one.
		while fixed + pyramids.iter().map(|(_, pyramid)| pyramid_bytes(pyramid)).sum::<u64>() > ceiling {
			let Some(last) = optional.iter().rposition(|entry| *entry) else { break };
			pyramids.remove(last);
			optional.remove(last);
		}
		let mut scratch_bytes = fixed + pyramids.iter().map(|(_, pyramid)| pyramid_bytes(pyramid)).sum::<u64>();
		if scratch_bytes > ceiling {
			// A FRAME THAT CANNOT FIT SAYS SO BEFORE IT STARTS DRAWING. Discovering it halfway through
			// a filter chain leaves a half-drawn frame, which is worse than an honest refusal.
			return Err(Error::LimitExceeded { limit: "prepared scratch", ceiling });
		}

		// LANES BEYOND THE FIRST ARE FITTED ONLY INTO WHAT IS LEFT, up to the lanes the pool offers and
		// the units there are to share out - so a lane never displaces a copy, and every worker count
		// keeps the copies the one-lane frame keeps. A COST THAT EXISTS ONLY WITH SEVERAL LANES - the
		// tile-major intermediate - is charged together with the second lane, and a frame with no room
		// for both runs on one lane without it.
		let tile_units = cut(&tiling, &bins, UnitKind::Tiles);
		let band_units = cut(&tiling, &bins, UnitKind::Bands);
		let slot_bytes = (TILE_SIZE as usize) * (TILE_SIZE as usize) * target.format.bytes_per_pixel() as usize;
		let (wanted_units, intermediate_bytes) = match self.unit_kind {
			UnitKind::Tiles => (tile_units.len(), (tile_units.len() * slot_bytes) as u64),
			UnitKind::Bands => (band_units.len(), 0),
		};
		let offered = self.workers.lanes().max(1).min(wanted_units.max(1));
		let mut lanes = 1usize;
		while lanes < offered {
			let extra = lane_bytes + if lanes == 1 { intermediate_bytes } else { 0 };
			if scratch_bytes + extra > ceiling {
				break;
			}
			scratch_bytes += extra;
			lanes += 1;
		}
		// ONE LANE IS THE SERIAL WALK, whatever the units were going to be: tiles need the intermediate
		// only to be written from several lanes at once, and a frame on one lane is cut into bands.
		let (unit_kind, units) = if lanes > 1 && self.unit_kind == UnitKind::Tiles { (UnitKind::Tiles, tile_units) } else { (UnitKind::Bands, band_units) };
		while self.lanes.len() < lanes {
			self.lanes.push(Lane::default());
		}
		for lane in self.lanes.iter_mut().take(lanes).skip(1) {
			shape.reserve(lane)?;
		}
		if unit_kind == UnitKind::Tiles {
			self.intermediate.resize(units.len() * slot_bytes, 0);
		}
		self.finished.resize(units.len(), false);
		// THE FRAME'S OWN TABLES, sized here so the replay only fills them.
		self.unit_table.reserve(units.len().saturating_sub(self.unit_table.len()));
		self.shader_table.reserve(steps.len().saturating_sub(self.shader_table.len()));

		let ramps = steps
			.iter()
			.map(|step| match step {
				Step::Fill { paint, .. } | Step::Image { paint, .. } | Step::Glyphs { paint, .. } | Step::AliasedLines { paint, .. } => ramp_for(paint, working, &resources.stops),
				_ => None,
			})
			.collect();
		let kernels = resources.filters.iter().map(crate::filter::kernels).collect();
		let damage = bounds.iter().flatten().copied().filter(|rect| !rect.is_empty()).reduce(union);
		Ok(SoftPrepared { key: PreparedKey::of(list, target, (BACKEND_NAME, BACKEND_VERSION), self.cache.generation()), steps, bounds, bins, tiling, pyramids, images: resources.images.clone(), ramps, filters: resources.filters.clone(), kernels, working, target_transfer: target.color_space.transfer(), output: target.luminance, expansion, scratch_bytes, damage, no_backdrop, regions, lanes, unit_kind, units, slot_bytes })
	}

	fn render(&mut self, prepared: &Self::Prepared, target: &mut ImageViewMut<'_>) -> Result<(), Error> {
		let extent = target.layout().extent;
		if extent != prepared.tiling.extent {
			return Err(Error::LimitExceeded { limit: "target extent", ceiling: prepared.tiling.extent.width as u64 });
		}
		// THE BACKEND IS TAKEN APART INTO ITS FIELDS so the shaders can borrow the image table while
		// the lanes borrow the scratch. Both are `self`, and only disjoint field borrows let one be read
		// while the other is written.
		let Soft2d { images, cancellation, workers, lanes, shader_table, unit_table, finished, intermediate, tables, .. } = self;
		// A PREPARED LIST FROM ANOTHER BACKEND reserved lanes this one does not have.
		if lanes.len() < prepared.lanes.max(1) || finished.len() < prepared.units.len() || (prepared.unit_kind == UnitKind::Tiles && intermediate.len() < prepared.units.len() * prepared.slot_bytes) {
			return Err(Error::Allocation);
		}
		let lookup = Lookup { source: *images, records: &prepared.images, pyramids: &prepared.pyramids, tables };
		// EVERY SHADER IS BUILT ONCE PER FRAME AND NOT ONCE PER TILE, into storage kept from the last
		// frame. A solid paint's colour has to be converted into the working space and an image's
		// sampler has to derive a colour-space matrix - and a drawing of two hundred commands over eighty
		// tiles built every one of those sixteen thousand times. A gradient's ramp is the prepared
		// list's, resolved once.
		let mut shaders: Vec<Shader<'_>> = recycle(core::mem::take(shader_table));
		shaders.extend(prepared.steps.iter().zip(prepared.ramps.iter()).map(|(step, ramp)| match step {
			Step::Fill { paint, transform, .. } | Step::Image { paint, transform, .. } | Step::Glyphs { paint, transform, .. } | Step::AliasedLines { paint, transform, .. } => shader(paint, transform, prepared.working, ramp.as_ref(), &lookup),
			_ => Shader::Nothing,
		}));
		let table = tables.iter().find(|(kind, _)| *kind == prepared.target_transfer).map(|(_, table)| table);
		let lanes = &mut lanes[..prepared.lanes.max(1)];
		for lane in lanes.iter_mut() {
			lane.failure = None;
		}
		let failed = AtomicUsize::new(usize::MAX);
		let stopped = AtomicBool::new(false);
		let cancellation = *cancellation;
		let shaders_ref: &[Shader<'_>] = &shaders;
		let work = |lane: &mut Lane, unit: &mut Unit<'_>| {
			// A UNIT HANDED OUT TWICE IS REPLAYED ONCE, and a unit after the lowest that failed is not
			// replayed at all: the serial walk would never have reached it.
			if unit.ran {
				return;
			}
			unit.ran = true;
			if unit.index > failed.load(Ordering::Relaxed) {
				return;
			}
			// THE CANCELLATION IS ASKED BEFORE EVERY UNIT, by every lane: often enough to stop promptly,
			// rare enough to cost nothing, and at a point where what has been drawn is whole units rather
			// than a shape cut in half. Once one lane has seen it, every lane stops at its next unit.
			if stopped.load(Ordering::Relaxed) || cancellation.is_some_and(|cancellation| cancellation.cancelled()) {
				stopped.store(true, Ordering::Relaxed);
				return;
			}
			for index in unit.first_tile as usize..(unit.first_tile + unit.tiles) as usize {
				let tile = prepared.tiling.tile(index);
				// A TILE NOTHING DRAWS INTO IS NOT REPLAYED. Replaying an empty bin still DECODED the tile
				// into the working space and RE-ENCODED it - for the eight-bit sRGB target that round trip
				// happens to be lossless, so what it cost was time and not pixels: an empty draw list cost
				// 21 ms of it, which was the largest single term in the simplest scene, and a compositor
				// redrawing one damaged corner paid it for every other tile of the frame.
				if tile.is_empty() || prepared.bins.commands(index).is_empty() {
					continue;
				}
				lane.tile.rebase((tile.x, tile.y));
				if let Err(error) = replay(prepared, &mut unit.access, tile, index, lane, shaders_ref, &lookup, table) {
					failed.fetch_min(unit.index, Ordering::Relaxed);
					if lane.failure.as_ref().is_none_or(|(at, _)| unit.index < *at) {
						lane.failure = Some((unit.index, error));
					}
					return;
				}
			}
			unit.whole = true;
		};
		let outcome = match prepared.unit_kind {
			UnitKind::Bands => {
				let mut units: Vec<Unit<'_>> = recycle(core::mem::take(unit_table));
				band_units(&mut units, target, &prepared.tiling, &prepared.units)?;
				workers.run(lanes, &mut units, &work);
				let outcome = verdict(&units, lanes, &stopped);
				*unit_table = recycle(units);
				outcome
			}
			UnitKind::Tiles => {
				let source = target.as_view();
				let mut units: Vec<Unit<'_>> = recycle(core::mem::take(unit_table));
				for ((index, &(first_tile, tiles)), slot) in prepared.units.iter().enumerate().zip(intermediate.chunks_mut(prepared.slot_bytes)) {
					let tile = prepared.tiling.tile(first_tile as usize);
					let pitch = TILE_SIZE as usize * source.layout().storage.bytes_per_pixel() as usize;
					units.push(Unit { index, first_tile, tiles, access: Access::Slot { source: &source, slot, tile, pitch }, ran: false, whole: false });
				}
				workers.run(lanes, &mut units, &work);
				let outcome = verdict(&units, lanes, &stopped);
				for (flag, unit) in finished.iter_mut().zip(units.iter()) {
					*flag = unit.whole;
				}
				*unit_table = recycle(units);
				// THE SLOTS ARE COPIED INTO THE TARGET ONCE EVERY UNIT HAS RETURNED, each unit's whole or
				// not at all - a unit a cancellation or a failure cut short left its slot, and nothing of it
				// reaches the target.
				assemble(target, prepared, intermediate, finished);
				outcome
			}
		};
		*shader_table = recycle(shaders);
		outcome
	}
}

/// A vector's storage, kept from one frame to the next while nothing it held is.
///
/// THE SHADERS AND THE UNITS BORROW THE FRAME, so they cannot outlive it - but their capacity can, and
/// that is the difference between a replay that allocates nothing and one that allocates per frame.
/// Emptied and collected again, a vector keeps its allocation: `Vec`'s in-place collection reuses it
/// when the element layout is unchanged, and a change of lifetime cannot change a layout. That is an
/// optimisation of the standard library and not a promise, so the counting allocator's warmed-frame
/// test is what holds it - a toolchain that stopped doing it fails there, and nothing here becomes
/// unsound either way.
fn recycle<T, U>(mut vector: Vec<T>) -> Vec<U> {
	vector.clear();
	vector.into_iter().map(|_| unreachable!("an emptied vector yields nothing")).collect()
}

/// How a frame's units went: refused if the pool returned before running every one, cancelled if a lane
/// was told to stop, and otherwise the lowest failing unit's error - the one the serial walk meets first.
fn verdict(units: &[Unit<'_>], lanes: &[Lane], stopped: &AtomicBool) -> Result<(), Error> {
	let failure = lanes.iter().filter_map(|lane| lane.failure.as_ref()).min_by_key(|(at, _)| *at);
	if let Some((_, error)) = failure {
		return Err(*error);
	}
	if stopped.load(Ordering::Relaxed) {
		return Err(Error::Cancelled);
	}
	if units.iter().any(|unit| !unit.ran) {
		return Err(Error::IncompletePool);
	}
	Ok(())
}

/// Cut a frame into units: the tile rows that have something to draw (BANDS), or the tiles that do
/// (TILES), each a run of tile indices in serial order.
fn cut(tiling: &Tiling, bins: &Bins, kind: UnitKind) -> Vec<(u32, u32)> {
	let draws = |index: usize| !tiling.tile(index).is_empty() && !bins.commands(index).is_empty();
	let mut units = Vec::new();
	match kind {
		UnitKind::Bands => {
			for row in 0..tiling.rows {
				let first = row * tiling.columns;
				if (first..first + tiling.columns).any(|index| draws(index as usize)) {
					units.push((first, tiling.columns));
				}
			}
		}
		UnitKind::Tiles => {
			for index in 0..tiling.count() {
				if draws(index) {
					units.push((index as u32, 1));
				}
			}
		}
	}
	units
}

/// The band units of a frame, each a view of its own rows of the target.
///
/// THE TARGET IS LAID OUT BY ROWS, so a band of rows is one contiguous run of its bytes and can be handed
/// out whole: split in memory order, which is the rows' order from the top for a top-left image and from
/// the bottom for a bottom-left one, and then put in the serial order.
fn band_units<'u>(units: &mut Vec<Unit<'u>>, target: &'u mut ImageViewMut<'_>, tiling: &Tiling, wanted: &[(u32, u32)]) -> Result<(), Error> {
	let layout = *target.layout();
	let pitch = layout.pitch as usize;
	let height = layout.extent.height;
	let bottom_left = layout.origin == graphics_core::layout::RowOrigin::BottomLeft;
	let mut rest: &'u mut [u8] = target.bytes_mut();
	let mut consumed = 0usize;
	for step in 0..tiling.rows {
		// The tile rows in MEMORY order.
		let row = if bottom_left { tiling.rows - 1 - step } else { step };
		let top = row * tiling.size;
		let rows = tiling.size.min(height.saturating_sub(top));
		// Where this band's rows start in memory, and how far the next one starts.
		let start = if bottom_left { (height - top - rows) as usize * pitch } else { top as usize * pitch };
		let end = (start + rows as usize * pitch).min(consumed + rest.len());
		let skip = start.saturating_sub(consumed);
		let taken = core::mem::take(&mut rest);
		let (_, after_skip) = taken.split_at_mut(skip.min(taken.len()));
		let length = (end - start).min(after_skip.len());
		let (band, after) = after_skip.split_at_mut(length);
		rest = after;
		consumed = end;
		let first = row * tiling.columns;
		let Some(position) = wanted.iter().position(|(unit_first, _)| *unit_first == first) else { continue };
		let band_layout = graphics_core::layout::ImageLayout::new(Extent2D::new(layout.extent.width, rows), layout.pitch, layout.storage, layout.origin, layout.semantics).map_err(crate::target::from_core)?;
		let view = ImageViewMut::new(band_layout, band).map_err(crate::target::from_core)?;
		units.push(Unit { index: position, first_tile: first, tiles: tiling.columns, access: Access::Band { view, top }, ran: false, whole: false });
	}
	if bottom_left {
		units.reverse();
	}
	Ok(())
}

/// Copy every finished unit's slot into the target: the part of each tile its replay wrote.
fn assemble(target: &mut ImageViewMut<'_>, prepared: &SoftPrepared, intermediate: &[u8], finished: &[bool]) {
	let bytes_per_pixel = target.layout().storage.bytes_per_pixel() as usize;
	let pitch = TILE_SIZE as usize * bytes_per_pixel;
	for ((&(first_tile, _), slot), whole) in prepared.units.iter().zip(intermediate.chunks(prepared.slot_bytes)).zip(finished.iter()) {
		if !*whole {
			continue;
		}
		let tile = prepared.tiling.tile(first_tile as usize);
		let region = prepared.regions.get(first_tile as usize).copied().unwrap_or(tile);
		let width = region.width as usize * bytes_per_pixel;
		for y in region.y..region.y.saturating_add(region.height) {
			let from = (y - tile.y) as usize * pitch + (region.x - tile.x) as usize * bytes_per_pixel;
			let Some(source) = slot.get(from..from + width) else { continue };
			let Some(row) = target.row_mut(y) else { continue };
			let at = region.x as usize * bytes_per_pixel;
			if let Some(destination) = row.get_mut(at..at + width) {
				destination.copy_from_slice(source);
			}
		}
	}
}

/// Replay one tile: every command binned to it, in bin order, into the lane's tile surface - then
/// encoded into the unit's part of the target.
#[allow(clippy::too_many_arguments)]
fn replay(prepared: &SoftPrepared, access: &mut Access<'_>, tile: PixelRect, index: usize, lane: &mut Lane, shaders: &[Shader<'_>], lookup: &Lookup<'_>, table: Option<&TransferTable>) -> Result<(), Error> {
	{
		let Lane { raster, pool, masks, spans, tile: surface, clips, layers, nodes, .. } = lane;
		// THE DECODE IS SKIPPED FOR A TILE SOMETHING OVERWRITES WHOLE. See `tiles_without_backdrop`:
		// the scratch then still holds the previous tile's pixels, every one of which the covering
		// command replaces, and the encode on the way out writes a full tile either way.
		// THE ROUND TRIP IS OVER WHAT CAN CHANGE - see `tile_regions` - and the tile is the answer
		// whenever anything in it is not a plain draw.
		let region = prepared.regions.get(index).copied().unwrap_or(tile);
		if !prepared.no_backdrop.get(index).copied().unwrap_or(false) {
			surface.load(access, region, prepared.working, table)?;
		}
		// THE STACKS ARE THE LANE'S, reserved at `prepare` to the depths the list reaches.
		clips.reset(tile);
		layers.clear();
		for command in prepared.bins.commands(index) {
			let Some(step) = prepared.steps.get(*command as usize) else { continue };
			match step {
				// A FILL AND AN IMAGE ARE THE SAME DRAW with different paints, which is what makes an
				// image one path through the rasteriser rather than a second blitter with its own
				// rules about where an edge is.
				Step::Fill { edges, blend, operator, opacity, .. } | Step::Image { edges, blend, operator, opacity, .. } => {
					let (rule, antialias) = match step {
						Step::Fill { rule, antialias, .. } => (*rule, *antialias),
						_ => (FillRule::NonZero, Antialias::On),
					};
					let Some(shader) = shaders.get(*command as usize) else { continue };
					// THE CLIP'S BOUNDS ARE PART OF THE DRAW'S BOUNDS, and this is where the fast path
					// below gets its right to exist: a stack of plain RECTANGLES is not consulted per
					// pixel because it is supposed to be in the bounds already - and it was not, so a
					// rectangular clip (including the EMPTY one a tile outside the clip's shape
					// pushes) was not applied at all. What that looked like was a shape drawn in full
					// in every tile the clip's own shape did not reach, three tile rows away from a
					// panel that was clipping it perfectly.
					let (into, bounds): (&mut dyn Raster, PixelRect) = match layers.last_mut() {
						Some(layer) => (&mut layer.surface, layer.bounds),
						None => (&mut *surface, tile),
					};
					let bounds = bounds.intersection(&clips.bounds());
					fill_edges(raster, spans, edges, rule, antialias, bounds, into, shader, clips, *opacity, *blend, *operator);
				}
				Step::Glyphs { placed, blend, operator, opacity, .. } => {
					let Some(shader) = shaders.get(*command as usize) else { continue };
					let (into, bounds): (&mut dyn Raster, PixelRect) = match layers.last_mut() {
						Some(layer) => (&mut layer.surface, layer.bounds),
						None => (&mut *surface, tile),
					};
					let bounds = bounds.intersection(&clips.bounds());
					draw_placed(raster, spans, placed, bounds, into, shader, clips, *opacity, *blend, *operator, prepared.working);
				}
				Step::AliasedLines { points, blend, operator, opacity, .. } => {
					let Some(shader) = shaders.get(*command as usize) else { continue };
					let (into, bounds): (&mut dyn Raster, PixelRect) = match layers.last_mut() {
						Some(layer) => (&mut layer.surface, layer.bounds),
						None => (&mut *surface, tile),
					};
					let bounds = bounds.intersection(&clips.bounds());
					draw_aliased_lines(points, bounds, into, shader, clips, *opacity, *blend, *operator);
				}
				Step::PushClip { edges, rule, antialias, bounds, inverse } => {
					let parent = match layers.last() {
						Some(layer) => layer.bounds,
						None => tile,
					};
					// AN INVERSE CLIP'S LEVEL COVERS THE PARENT and not the shape: what it keeps is
					// everything the shape misses, so a level bounded by the shape would clip away the
					// very region the inverse exists to keep.
					let level_bounds = if *inverse { parent } else { parent.intersection(bounds) };
					if edges.is_empty() || level_bounds.is_empty() {
						// An empty shape clips everything away, and its INVERSE clips nothing away.
						clips.push(if *inverse { ClipLevel::rectangle(parent) } else { ClipLevel::rectangle(PixelRect::new(0, 0, 0, 0)) });
						continue;
					}
					match masks.take_filled(if *inverse { 255 } else { 0 }) {
						Some(mut mask) => {
							let stride = level_bounds.width as usize;
							raster.fill_edges(edges, *rule, *antialias, level_bounds, |y, first, coverage| {
								let Some(row) = y.checked_sub(level_bounds.y) else { return };
								// THE ROW'S COVERED RUN AND NOT ITS WHOLE WIDTH. The rest of the row
								// is already what the mask was filled with - zero for an ordinary
								// clip, `255` for an inverse one - which is exactly what a column no
								// edge reached means in each direction.
								let start = row as usize * stride + first;
								if let Some(slice) = mask.get_mut(start..start + coverage.len().min(stride.saturating_sub(first))) {
									write_mask_row(slice, coverage, *inverse);
								}
							});
							clips.push(ClipLevel::with_mask(level_bounds, mask));
						}
						// THE RESERVATION IS EXHAUSTED, which is a budget that was too small. The clip
						// becomes its bounding rectangle rather than being ignored: a clip that
						// silently did nothing would let a child draw outside its parent.
						None => clips.push(ClipLevel::rectangle(level_bounds)),
					}
				}
				Step::PushClipRect { bounds } => {
					let parent = match layers.last() {
						Some(layer) => layer.bounds,
						None => tile,
					};
					clips.push(ClipLevel::rectangle(parent.intersection(bounds)));
				}
				Step::PushClipMask { image, transform, inverse } => {
					let parent = match layers.last() {
						Some(layer) => layer.bounds,
						None => tile,
					};
					let sampler = lookup.lookup(image.0).and_then(|(view, _, table)| graphics_core::sample::Sampler::with_table(view, prepared.working, graphics_core::sample::Spread::Clamp, table).ok());
					let inverse_transform = transform.inverse();
					match (sampler, inverse_transform, masks.take()) {
						(Some(sampler), Some(inverse_transform), Some(mut mask)) => {
							let stride = parent.width as usize;
							for y in parent.y..parent.y.saturating_add(parent.height) {
								for x in parent.x..parent.x.saturating_add(parent.width) {
									let Some(local) = inverse_transform.map_point(PointF { x: x as f32 + 0.5, y: y as f32 + 0.5 }) else { continue };
									// THE MASK IS THE IMAGE'S ALPHA, sampled through the transform the
									// clip was pushed under - which is what makes a mask rotate with
									// the thing it is masking.
									let alpha = sampler.sample(local.x, local.y, graphics_core::sample::Quality::Bilinear).alpha.clamp(0.0, 1.0);
									let value = graphics_core::pixel::quantise(alpha, 255.0) as u8;
									let index = (y - parent.y) as usize * stride + (x - parent.x) as usize;
									if let Some(slot) = mask.get_mut(index) {
										*slot = if *inverse { 255 - value } else { value };
									}
								}
							}
							clips.push(ClipLevel::with_mask(parent, mask));
						}
						(_, _, Some(mask)) => {
							// A MASK WHOSE IMAGE IS NOT THERE CLIPS NOTHING AWAY rather than
							// everything: a missing resource must not blank the drawing under it.
							masks.give(mask);
							clips.push(ClipLevel::rectangle(parent));
						}
						_ => clips.push(ClipLevel::rectangle(parent)),
					}
				}
				Step::PopClip => clips.pop(masks),
				Step::BeginLayer { bounds, opacity, blend, operator, filter } => {
					let parent = match layers.last() {
						Some(layer) => layer.bounds,
						None => tile,
					};
					let expansion = if filter.is_some() { prepared.expansion } else { 0 };
					let layer_rect = layer_bounds(*bounds, parent, expansion);
					match pool.take(layer_rect) {
						Some(layer_surface) => layers.push(Layer { surface: layer_surface, bounds: layer_rect, opacity: *opacity, blend: *blend, operator: *operator, filter: *filter, clip_depth: clips.depth() }),
						None => return Err(Error::LimitExceeded { limit: "prepared scratch", ceiling: graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS.max_prepared_scratch_bytes }),
					}
				}
				Step::EndLayer => {
					let Some(layer) = layers.pop() else { continue };
					// A LAYER CLOSES ITS OWN CLIPS. One left open would apply to whatever came after
					// the layer, which is a clip nobody pushed.
					while clips.depth() > layer.clip_depth {
						clips.pop(masks);
					}
					let filtered = match layer.filter.and_then(|handle| prepared.filters.get(handle.0 as usize)) {
						Some(graph) => {
							// THE BACKDROP IS WHAT THE LAYER IS ABOUT TO COMPOSITE ONTO, which is the
							// parent surface as it stands right now - so a graph that reads it sees
							// the scene under the layer and not the layer itself.
							let backdrop: &dyn Raster = match layers.last() {
								Some(parent) => &parent.surface,
								None => &*surface,
							};
							let kernels = layer.filter.and_then(|handle| prepared.kernels.get(handle.0 as usize)).map_or(&[][..], |kernels| kernels.as_slice());
							Some(crate::filter::evaluate(graph, kernels, &layer.surface, backdrop, layer.bounds, pool, spans, nodes, prepared.working, lookup)?)
						}
						None => None,
					};
					{
						let source = filtered.as_ref().unwrap_or(&layer.surface);
						let (into, into_bounds): (&mut dyn Raster, PixelRect) = match layers.last_mut() {
							Some(parent) => (&mut parent.surface, parent.bounds),
							None => (&mut *surface, tile),
						};
						let bounds = into_bounds.intersection(&layer.bounds);
						for y in bounds.y..bounds.y.saturating_add(bounds.height) {
							for x in bounds.x..bounds.x.saturating_add(bounds.width) {
								let clip = clips.coverage(x, y);
								if clip <= 0.0 {
									continue;
								}
								// THE WHOLE LAYER IS COMPOSITED AS ONE THING at its opacity, which is
								// what group opacity means and why it cannot be done by multiplying
								// each drawing's alpha as it was drawn.
								let value = source.get(x, y).scaled(layer.opacity.clamp(0.0, 1.0) * clip);
								let backdrop = into.get(x, y);
								into.set(x, y, graphics_core::composite::composite(layer.operator, layer.blend, value, backdrop));
							}
						}
					}
					if let Some(filtered) = filtered {
						pool.give(filtered);
					}
					pool.give(layer.surface);
				}
			}
		}
		// AN UNCLOSED LAYER CANNOT HAPPEN - the list refused it - but the storage is returned anyway,
		// because a tile that kept one would exhaust the pool on the next tile rather than here.
		for layer in layers.drain(..) {
			pool.give(layer.surface);
		}
		clips.drain_into(masks);
		surface.store(access, region, prepared.working, prepared.output, table, &mut spans.filter_input)
	}
}

/// What one lane is reserved to for a list: the same shape for every lane of a frame.
struct LaneShape {
	surfaces: usize,
	extent: (u32, u32),
	space: graphics_core::ColorSpace,
	masks: usize,
	edges: usize,
	clips: usize,
	layers: usize,
	nodes: usize,
}

impl LaneShape {
	fn reserve(&self, lane: &mut Lane) -> Result<(), Error> {
		lane.pool.reserve(self.surfaces, self.extent, self.space)?;
		lane.masks.reserve(self.masks, self.extent.0 as usize * self.extent.1 as usize);
		lane.raster.reserve(self.extent.0 as usize, self.edges);
		lane.spans.reserve(self.extent.0 as usize);
		lane.clips.reserve(self.clips);
		lane.layers.reserve(self.layers.saturating_sub(lane.layers.len()));
		lane.nodes.reserve(self.nodes.saturating_sub(lane.nodes.len()));
		Ok(())
	}
}

/// The row buffers a span composite works in, reserved once.
///
/// THE ROW AND NOT THE PIXEL IS THE UNIT. Reading one destination pixel through a bounds check and a
/// row lookup, compositing it and writing it back is most of the cost of a fill - and it is the shape
/// that stops the arithmetic being vectorised at all.
#[derive(Default)]
pub struct Spans {
	source: Vec<Rgba>,
	destination: Vec<Rgba>,
	weights: Vec<f32>,
	/// Two more runs, for the separable filter passes. A blur reads a whole row or column at once for
	/// the same reason a fill composites one: a per-pixel fetch through a bounds check and a row
	/// lookup is most of the cost of the loop around it.
	pub(crate) filter_input: Vec<Rgba>,
	pub(crate) filter_output: Vec<Rgba>,
}

impl Spans {
	pub(crate) fn reserve(&mut self, width: usize) {
		if self.source.len() < width {
			self.source.resize(width, Rgba::TRANSPARENT);
			self.destination.resize(width, Rgba::TRANSPARENT);
			self.weights.resize(width, 0.0);
			self.filter_input.resize(width, Rgba::TRANSPARENT);
			self.filter_output.resize(width, Rgba::TRANSPARENT);
		}
	}

	pub(crate) fn scratch_bytes(&self) -> u64 {
		((self.source.capacity() + self.destination.capacity() + self.filter_input.capacity() + self.filter_output.capacity()) * core::mem::size_of::<Rgba>() + self.weights.capacity() * core::mem::size_of::<f32>()) as u64
	}
}

/// Fill one shape into a surface through a shader, a clip and an operator.
#[allow(clippy::too_many_arguments)]
fn fill_edges(raster: &mut Rasteriser, spans: &mut Spans, edges: &crate::raster::Edges, rule: FillRule, antialias: Antialias, bounds: PixelRect, into: &mut dyn Raster, shader: &Shader<'_>, clips: &ClipStack, opacity: f32, blend: BlendMode, operator: Operator) {
	let opacity = opacity.clamp(0.0, 1.0);
	if opacity <= 0.0 || bounds.is_empty() {
		return;
	}
	spans.reserve(bounds.width as usize);
	// A CLIP STACK OF PLAIN RECTANGLES IS ALREADY IN THE BOUNDS. Asking it per pixel would walk the
	// stack and multiply by one, which on a scrolling list is most of what the composite costs.
	let rectangular = clips.is_rectangular();
	raster.fill_edges(edges, rule, antialias, bounds, |y, emitted, coverage| {
		// THE RUN IS TRIMMED TO WHAT THE SHAPE ACTUALLY COVERS, because a row of a tile is sixty-four
		// pixels and a shape's edge crosses a handful of them: compositing the whole row would spend
		// the cost of a full-width span on a one-pixel line.
		//
		// THE RASTERISER NOW TRIMS THE FIRST HALF OF THAT ITSELF - `emitted` is where the row it
		// handed over begins - so this walks what it was given rather than the tile.
		let mut first = coverage.len();
		let mut last = 0usize;
		for (offset, value) in coverage.iter().enumerate() {
			let mut weight = value.clamp(0.0, 1.0) * opacity;
			if weight > 0.0 && !rectangular {
				weight *= clips.coverage(bounds.x + (emitted + offset) as u32, y);
			}
			spans.weights[offset] = weight;
			if weight > 0.0 {
				first = first.min(offset);
				last = offset + 1;
			}
		}
		if first >= last {
			return;
		}
		let first = emitted + first;
		let last = emitted + last;
		let length = last - first;
		// A SOLID PAINT IS THE SAME COLOUR AT EVERY PIXEL, and asking it per pixel is a match on the
		// shader enum inside the hot loop - which is both the arithmetic and the reason the loop
		// cannot be vectorised. Filling the run is the same answer with the decision taken once.
		// It matters because a solid is most of what a user interface is made of.
		shader.row(bounds.x + first as u32, y, &mut spans.source[..length]);
		// AN OPAQUE RUN AT FULL COVERAGE IS A COPY, and the whole composite is arithmetic that
		// produces its own source.
		//
		// `Cs + Cb * (1 - as)` with `as = 1` is `Cs + Cb * 0`, which for any finite backdrop is
		// exactly `Cs` - so the backdrop read, the four multiplies of the coverage scale and the
		// eight of the blend all compute a number that is already in hand. This is the INTERIOR of
		// every filled shape, which is most of the pixels a drawing has; the edge, where coverage is
		// partial, falls through to the general path below and is unchanged.
		//
		// THE CONDITIONS ARE THE ONES THAT MAKE IT AN IDENTITY and no wider: a solid fully opaque
		// paint, a normal blend, source-over or a straight source, and every weight in the run at
		// one. The scan for that last one costs a pass over the run and saves three.
		let covered = matches!(blend, BlendMode::Normal) && matches!(operator, Operator::SrcOver | Operator::Src) && matches!(shader, Shader::Solid(colour) if colour.alpha >= 1.0) && spans.weights[first - emitted..last - emitted].iter().all(|weight| *weight >= 1.0);
		if covered {
			into.write_span(bounds.x + first as u32, y, &spans.source[..length]);
			return;
		}
		crate::span::scale_span(&mut spans.source[..length], &spans.weights[first - emitted..last - emitted]);
		into.read_span(bounds.x + first as u32, y, &mut spans.destination[..length]);
		crate::span::composite_span(&mut spans.destination[..length], &spans.source[..length], operator, blend);
		into.write_span(bounds.x + first as u32, y, &spans.destination[..length]);
	});
}

/// Draw an aliased polyline, one pixel per step.
#[allow(clippy::too_many_arguments)]
fn draw_aliased_lines(points: &[(i32, i32)], bounds: PixelRect, into: &mut dyn Raster, shader: &Shader<'_>, clips: &ClipStack, opacity: f32, blend: BlendMode, operator: Operator) {
	let opacity = opacity.clamp(0.0, 1.0);
	if opacity <= 0.0 {
		return;
	}
	for pair in points.windows(2) {
		crate::raster::aliased_line(pair[0], pair[1], bounds, |x, y| {
			let coverage = opacity * clips.coverage(x, y);
			if coverage <= 0.0 {
				return;
			}
			let source = shader.at(x as f32, y as f32).scaled(coverage);
			let backdrop = into.get(x, y);
			into.set(x, y, graphics_core::composite::composite(operator, blend, source, backdrop));
		});
	}
}

/// A flattened contour set as integer points, when it is ONE open polyline and nothing else.
///
/// THE FAST PATH IS NARROW ON PURPOSE. A one-pixel aliased line has an exact rule; a shape does not,
/// and a fast path that guessed which shapes it applied to would produce a different picture from the
/// rasteriser for the same drawing.
fn integer_polyline(contours: &[Contour]) -> Option<Vec<(i32, i32)>> {
	let [contour] = contours else { return None };
	if contour.closed || contour.points.len() < 2 || contour.points.len() > 1024 {
		return None;
	}
	let mut points = Vec::with_capacity(contour.points.len());
	for point in &contour.points {
		if !(point.x.is_finite() && point.y.is_finite()) {
			return None;
		}
		// THE PIXEL A COORDINATE FALLS IN, which for a one-pixel line is what "on" means: a line from
		// `(0, 0.5)` to `(10, 0.5)` covers the first row of pixels and not the boundary between two.
		points.push((libm::floorf(point.x) as i32, libm::floorf(point.y) as i32));
	}
	Some(points)
}

fn polyline_bounds(points: &[(i32, i32)]) -> Option<PixelRect> {
	let mut minimum = (i32::MAX, i32::MAX);
	let mut maximum = (i32::MIN, i32::MIN);
	for (x, y) in points {
		minimum = (minimum.0.min(*x), minimum.1.min(*y));
		maximum = (maximum.0.max(*x), maximum.1.max(*y));
	}
	if minimum.0 > maximum.0 {
		return None;
	}
	let x = minimum.0.max(0) as u32;
	let y = minimum.1.max(0) as u32;
	let right = maximum.0.max(0) as u32;
	let bottom = maximum.1.max(0) as u32;
	Some(PixelRect::new(x, y, right.saturating_sub(x) + 1, bottom.saturating_sub(y) + 1))
}

/// Place one recorded run's glyphs, at `prepare`.
///
/// EVERY GLYPH'S CACHE KEY AND DEVICE ORIGIN are the arithmetic a tile did in the replay - moved here
/// and not changed - and each distinct key is resolved through the cache once, a miss decoded by the
/// provider. An outline or a colour layer is flattened at its placed origin and its edges built there,
/// once per placed glyph where a tile used to do it in every tile the run reached.
fn place_glyphs(cache: &mut GlyphRaster, provider: &dyn GlyphProvider, run: &render2d::list::RecordedGlyphRun, transform: &Transform, working: Working, widest_edges: &mut usize) -> Vec<Placed> {
	let mut placed = Vec::with_capacity(run.glyphs.len());
	let mut pen_x = run.origin_x;
	let mut pen_y = run.origin_y;
	for glyph in &run.glyphs {
		let x = add_fixed(pen_x, glyph.x_offset);
		let y = add_fixed(pen_y, glyph.y_offset);
		// THE PEN IS IN USER SPACE AND THE GLYPH IS DRAWN IN DEVICE SPACE. Mapping it is what makes a
		// run inside a scrolled, scaled or rotated drawing land with the drawing rather than at the
		// coordinates it was recorded at: the FORM is the provider's, keyed by the transform below,
		// and WHERE it goes is this.
		let placed_pen = PointF { x: pixels(x), y: pixels(y) };
		let origin = transform.map_point(placed_pen).unwrap_or(placed_pen);
		// THE PHASE IS THE DEVICE POSITION'S, not the pen's. A run at a whole user-space coordinate
		// under a half-pixel translation lands between pixels, and rasterising it as though it were
		// aligned is exactly the drift subpixel positioning exists to remove.
		let device = |value: f32| font_contract::Fixed266::from_raw((value * 64.0) as i32);
		let key = font_contract::cache::GlyphCacheKey { face: run.face.face, generation: run.face.generation, glyph: glyph.glyph, size: run.size, variation: run.variation, transform: font_contract::glyph::TransformKey::new([transform.m[0][0], transform.m[1][0], transform.m[0][1], transform.m[1][1], 0.0, 0.0]).unwrap_or(font_contract::glyph::TransformKey::IDENTITY), phase: font_contract::glyph::SubpixelPhase::of(device(origin.x), device(origin.y)), kind: glyph.kind, selection: glyph.selection, mode: run.mode };
		let form = cache.shared(&key, provider);
		match &*form {
			GlyphImage::Missing => {}
			GlyphImage::Outline(path) => {
				let contours = render2d::flatten::flatten(path, Some(&Transform::translate(origin.x, origin.y)));
				*widest_edges = (*widest_edges).max(edge_count(&contours));
				placed.push(Placed::Outline(crate::raster::Edges::build(&contours)));
			}
			GlyphImage::Layers(layers) => {
				// `COLR` LAYERS ARE DRAWN IN ORDER, each with its own palette colour - which is what
				// makes an emoji an emoji rather than a silhouette.
				let at = Transform::translate(origin.x, origin.y);
				let mut built = Vec::with_capacity(layers.len());
				for (path, colour) in layers {
					let contours = render2d::flatten::flatten(path, Some(&at));
					*widest_edges = (*widest_edges).max(edge_count(&contours));
					built.push((crate::raster::Edges::build(&contours), to_working(*colour, working)));
				}
				placed.push(Placed::Layers(built));
			}
			GlyphImage::Mask { left, top, .. } => placed.push(Placed::Mask { x: origin.x as i64 + *left as i64, y: origin.y as i64 + *top as i64, form: form.clone() }),
			GlyphImage::Bitmap { left, top, .. } => placed.push(Placed::Bitmap { x: origin.x as i64 + *left as i64, y: origin.y as i64 + *top as i64, form: form.clone() }),
		}
		pen_x = add_fixed(pen_x, glyph.x_advance);
		pen_y = add_fixed(pen_y, glyph.y_advance);
	}
	placed
}

/// Draw a run's placed glyphs into a tile, from the prepared list alone.
#[allow(clippy::too_many_arguments)]
fn draw_placed(raster: &mut Rasteriser, spans: &mut Spans, placed: &[Placed], bounds: PixelRect, into: &mut dyn Raster, shader: &Shader<'_>, clips: &ClipStack, opacity: f32, blend: BlendMode, operator: Operator, working: Working) {
	for glyph in placed {
		match glyph {
			Placed::Outline(edges) => fill_edges(raster, spans, edges, FillRule::NonZero, Antialias::On, bounds, into, shader, clips, opacity, blend, operator),
			Placed::Layers(layers) => {
				for (edges, colour) in layers {
					let solid = Shader::Solid(*colour);
					fill_edges(raster, spans, edges, FillRule::NonZero, Antialias::On, bounds, into, &solid, clips, opacity, blend, operator);
				}
			}
			Placed::Mask { form, x: base_x, y: base_y } => {
				let GlyphImage::Mask { width, height, coverage, mode, .. } = &**form else { continue };
				let (base_x, base_y) = (*base_x, *base_y);
				let subpixel = !matches!(mode, font_contract::glyph::RasterisationMode::Grayscale);
				for row in 0..*height {
					for column in 0..*width {
						let index = (row * *width + column) as usize;
						let triple = if subpixel {
							let base = index * 3;
							[value_at(coverage, base), value_at(coverage, base + 1), value_at(coverage, base + 2)]
						} else {
							[value_at(coverage, index); 3]
						};
						let channels = crate::glyph::subpixel_channels(*mode, triple);
						if channels.alpha <= 0.0 {
							continue;
						}
						let (Ok(x), Ok(y)) = (u32::try_from(base_x + column as i64), u32::try_from(base_y + row as i64)) else { continue };
						if x < bounds.x || y < bounds.y || x >= bounds.x + bounds.width || y >= bounds.y + bounds.height {
							continue;
						}
						let clip = clips.coverage(x, y);
						if clip <= 0.0 {
							continue;
						}
						// THE COVERAGE GOES THROUGH THE PROFILE'S OWN GAMMA, which is what stops light
						// text on a dark background looking bolder than the reverse at one weight.
						let weight = crate::glyph::coverage_through_gamma(channels.alpha) * opacity.clamp(0.0, 1.0) * clip;
						let colour = shader.at(x as f32, y as f32);
						let source = Rgba::new(colour.red * channels.red, colour.green * channels.green, colour.blue * channels.blue, colour.alpha * weight);
						let backdrop = into.get(x, y);
						into.set(x, y, graphics_core::composite::composite(operator, blend, source, backdrop));
					}
				}
			}
			Placed::Bitmap { form, x: base_x, y: base_y } => {
				let GlyphImage::Bitmap { image, .. } = &**form else { continue };
				let (base_x, base_y) = (*base_x, *base_y);
				let view = image.view();
				let Ok(sampler) = graphics_core::sample::Sampler::new(view, working, graphics_core::sample::Spread::Clamp) else { continue };
				let extent = image.layout().extent;
				for row in 0..extent.height {
					for column in 0..extent.width {
						let (Ok(x), Ok(y)) = (u32::try_from(base_x + column as i64), u32::try_from(base_y + row as i64)) else { continue };
						if x < bounds.x || y < bounds.y || x >= bounds.x + bounds.width || y >= bounds.y + bounds.height {
							continue;
						}
						let clip = clips.coverage(x, y);
						if clip <= 0.0 {
							continue;
						}
						let source = sampler.sample(column as f32 + 0.5, row as f32 + 0.5, graphics_core::sample::Quality::Nearest).scaled(opacity.clamp(0.0, 1.0) * clip);
						let backdrop = into.get(x, y);
						into.set(x, y, graphics_core::composite::composite(operator, blend, source, backdrop));
					}
				}
			}
		}
	}
}

/// A 26.6 value in pixels. THE SEAM IS 26.6 AND THE RASTERISER IS FLOAT, and this is the one place
/// the conversion happens - a second one somewhere else is a second rounding.
fn pixels(value: font_contract::Fixed266) -> f32 {
	value.raw() as f32 / (1 << font_contract::fixed::FRACTIONAL_BITS) as f32
}

/// Add two 26.6 values, SATURATING rather than wrapping: a run whose positions overflow is a run that
/// draws at the edge of the coordinate space, not one that draws at the opposite edge.
fn add_fixed(left: font_contract::Fixed266, right: font_contract::Fixed266) -> font_contract::Fixed266 {
	font_contract::Fixed266::from_raw(left.raw().saturating_add(right.raw()))
}

fn value_at(coverage: &[u8], index: usize) -> f32 {
	coverage.get(index).map(|value| *value as f32 / 255.0).unwrap_or(0.0)
}

/// The transform mapping SOURCE TEXELS onto the destination rectangle.
///
/// THIS DIRECTION AND NOT THE OTHER. Every paint's transform maps the paint's own space into the
/// drawing's, and the shader inverts it once to walk device pixels back into paint space - so an
/// image whose transform mapped the other way would be inverted twice and land nowhere near the
/// rectangle it was asked to fill.
fn source_to_destination(source: RectF, destination: RectF) -> Transform {
	if source.width <= 0.0 || source.height <= 0.0 {
		return Transform::IDENTITY;
	}
	let scale_x = destination.width / source.width;
	let scale_y = destination.height / source.height;
	Transform { m: [[scale_x, 0.0, destination.x - source.x * scale_x], [0.0, scale_y, destination.y - source.y * scale_y], [0.0, 0.0, 1.0]] }
}

fn flatten_handle(resources: &render2d::list::ResourceTable, handle: u32, transform: &Transform) -> Result<Vec<Contour>, Error> {
	let path: &Path = resources.paths.get(handle as usize).ok_or(Error::UnknownResource { kind: render2d::resource::ResourceKind::Path, index: handle })?;
	render2d::flatten::flatten_checked(path, Some(transform))
}

fn edge_count(contours: &[Contour]) -> usize {
	contours.iter().map(|contour| contour.points.len()).sum()
}

fn glyph_run_bounds(resources: &render2d::list::ResourceTable, handle: u32, transform: &Transform) -> Option<PixelRect> {
	let run = resources.glyph_runs.get(handle as usize)?;
	let mut pen_x = pixels(run.origin_x);
	let pen_y = pixels(run.origin_y);
	let size = pixels(run.size).abs().max(1.0);
	let mut minimum = PointF { x: f32::INFINITY, y: f32::INFINITY };
	let mut maximum = PointF { x: f32::NEG_INFINITY, y: f32::NEG_INFINITY };
	for glyph in &run.glyphs {
		// A RUN'S BOUND IS CONSERVATIVE AND COMES FROM THE ADVANCES AND THE SIZE, because the actual
		// ink of a glyph is not known until it is decoded - and decoding every glyph to compute a
		// bound would decode them twice.
		let x = pen_x + pixels(glyph.x_offset);
		let y = pen_y + pixels(glyph.y_offset);
		for corner in [PointF { x: x - size, y: y - size * 2.0 }, PointF { x: x + size * 2.0, y: y + size }] {
			if let Some(mapped) = transform.map_point(corner) {
				minimum.x = minimum.x.min(mapped.x);
				minimum.y = minimum.y.min(mapped.y);
				maximum.x = maximum.x.max(mapped.x);
				maximum.y = maximum.y.max(mapped.y);
			}
		}
		pen_x += pixels(glyph.x_advance);
	}
	(maximum.x >= minimum.x).then(|| cover(RectF::new(minimum.x, minimum.y, maximum.x - minimum.x, maximum.y - minimum.y)))
}

fn wants_pyramid(list: &DrawList, image: u32) -> bool {
	sampled_as(list, image, |quality| matches!(quality, ImageQuality::Mipmapped))
}

/// Whether an image is sampled with a MULTI-TAP filter, which is what makes a decoded copy of it
/// worth the memory.
///
/// THE DECODE IS PER TAP AND THE TAPS OVERLAP. A bicubic pixel takes sixteen texels and decodes the
/// transfer function on every one of them - and its neighbour decodes most of the same texels again.
/// Measured on the benchmark's photo: **139 of the 254 milliseconds a full-screen bicubic draw costs
/// are that decode**, which is the largest single term in this backend. A pyramid's level zero is the
/// source already decoded into the working format, so sampling it removes the whole term.
fn wants_decoded(list: &DrawList, image: u32) -> bool {
	sampled_as(list, image, |quality| matches!(quality, ImageQuality::Bilinear | ImageQuality::Bicubic))
}

fn sampled_as(list: &DrawList, image: u32, wanted: impl Fn(&ImageQuality) -> bool) -> bool {
	list.commands().iter().any(|command| match command {
		Command::DrawImage { image: handle, quality, .. } => handle.0 == image && wanted(quality),
		Command::FillPath { paint: Paint::Image { image: handle, quality, .. }, .. } | Command::StrokePath { paint: Paint::Image { image: handle, quality, .. }, .. } | Command::DrawGlyphRun { paint: Paint::Image { image: handle, quality, .. }, .. } => handle.0 == image && wanted(quality),
		_ => false,
	})
}

fn pyramid_bytes(pyramid: &Pyramid) -> u64 {
	let mut total = 0u64;
	for index in 0..pyramid.levels() {
		if let Some(view) = pyramid.level(index) {
			total += view.layout().minimum_visible_bytes().unwrap_or(0);
		}
	}
	total
}

fn union(left: PixelRect, right: PixelRect) -> PixelRect {
	let x = left.x.min(right.x);
	let y = left.y.min(right.y);
	let right_edge = left.x.saturating_add(left.width).max(right.x.saturating_add(right.width));
	let bottom = left.y.saturating_add(left.height).max(right.y.saturating_add(right.height));
	PixelRect::new(x, y, right_edge - x, bottom - y)
}

/// Where a shader finds an image and the pyramid `prepare` built for it.
struct Lookup<'a> {
	source: &'a dyn ImageSource,
	records: &'a [ImageRecord],
	pyramids: &'a [(u32, Pyramid)],
	tables: &'a [(graphics_profile::image::Transfer, TransferTable)],
}

impl ImageLookup for Lookup<'_> {
	fn planes(&self, handle: u32) -> Option<(graphics_core::planar::MultiPlaneView<'_>, Option<&Pyramid>, Option<&TransferTable>)> {
		let record = self.records.get(handle as usize)?;
		let view = self.source.planes(record.identity)?;
		let pyramid = self.pyramids.iter().find(|(index, _)| *index == handle).map(|(_, pyramid)| pyramid);
		let transfer = view.layout().color_space.transfer();
		let table = self.tables.iter().find(|(kind, _)| *kind == transfer).map(|(_, table)| table);
		Some((view, pyramid, table))
	}

	fn lookup(&self, handle: u32) -> Option<(ImageView<'_>, Option<&Pyramid>, Option<&TransferTable>)> {
		let record = self.records.get(handle as usize)?;
		let view = self.source.image(record.identity)?;
		let pyramid = self.pyramids.iter().find(|(index, _)| *index == handle).map(|(_, pyramid)| pyramid);
		let table = view.layout().semantics.color_space().and_then(|space| self.tables.iter().find(|(kind, _)| *kind == space.transfer()).map(|(_, table)| table));
		Some((view, pyramid, table))
	}
}

/// A stroke style's width in device pixels, which a caller asking for bounds needs too.
pub fn device_width(style: &StrokeStyle, transform: &Transform) -> f32 {
	StrokeParameters::from_style(style, device_scale(transform)).width
}
