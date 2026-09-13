//! THE TWO PHASES, and the tile loop between them.
//!
//! `prepare` VALIDATES, FLATTENS, STROKES, BINS, BUILDS PYRAMIDS AND RESERVES. Everything that
//! depends on the list and the target but not on the frame happens once, and what it cannot fit it
//! refuses BEFORE any pixel is written - a frame that fails halfway through a filter chain leaves a
//! half-drawn picture on the screen, which is worse than not drawing it.
//!
//! `render` REPLAYS AND ALLOCATES NOTHING. Every buffer it uses came out of the reservation: the
//! coverage row, the clip masks, the layer surfaces and the tile's working copy. A frame that repeats
//! with the same list and the same target therefore costs what the drawing costs and nothing else.
//!
//! AND THE REPLAY IS TILED. Each tile decodes its own rectangle of the target once, replays the
//! commands binned to it, and encodes once - so the conversion is at the edges of a tile rather than
//! inside its command loop, and the working set is a tile rather than a surface.

use alloc::vec::Vec;

use graphics_core::geom::{PixelRect, PointF, RectF};
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

use crate::clip::{ClipLevel, ClipStack, MaskPool, write_mask_row};
use crate::glyph::{GlyphImage, GlyphProvider, GlyphRaster, NoGlyphs};
use crate::layer::{Layer, Pool, layer_bounds};
use crate::paint::{ImageLookup, Shader, shader, to_working};
use crate::raster::Rasteriser;
use crate::stroke::{StrokeParameters, dashed, outline};
use crate::target::{ImageSource, NoImages, Raster, Tile};
use crate::tile::{Bins, Tiling, cover};
use crate::{BACKEND_NAME, BACKEND_VERSION, TILE_SIZE};

/// Whether a frame should stop.
///
/// A SURFACE THAT WAS CLOSED OR RESIZED MID-FRAME IS A FRAME NOBODY WILL SEE. Finishing it costs the
/// whole drawing for nothing, and on a resize it costs it at the wrong size - so the loop asks
/// between tiles, which is often enough to stop promptly and rare enough to cost nothing.
pub trait Cancellation {
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
	Glyphs {
		run: u32,
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

/// What `prepare` produced.
pub struct SoftPrepared {
	key: PreparedKey,
	steps: Vec<Step>,
	bounds: Vec<Option<PixelRect>>,
	bins: Bins,
	tiling: Tiling,
	pyramids: Vec<(u32, Pyramid)>,
	images: Vec<ImageRecord>,
	stops: Vec<Vec<render2d::paint::GradientStop>>,
	filters: Vec<render2d::filter::FilterGraph>,
	glyph_runs: Vec<render2d::list::RecordedGlyphRun>,
	working: Working,
	target_transfer: graphics_profile::image::Transfer,
	expansion: u32,
	scratch_bytes: u64,
	damage: Option<PixelRect>,
}

impl SoftPrepared {
	/// THE CONSERVATIVE DAMAGE of the whole list: the union of every command's bound, clipped to the
	/// target. Exact damage would mean comparing old and new pixels - a source-over with alpha zero
	/// changes nothing while covering a rectangle - and that cost defeats the purpose.
	pub fn damage(&self) -> Option<PixelRect> {
		self.damage
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
	cache: GlyphRaster,
	raster: Rasteriser,
	pool: Pool,
	masks: MaskPool,
	spans: Spans,
	tile: Option<Tile>,
	/// THE TRANSFER FUNCTIONS THIS DRAWING NEEDS, built once in `prepare`. One per distinct function
	/// rather than one per image: four images in sRGB share one table, and building a table per image
	/// would spend more on the tables than the powers they replace.
	tables: Vec<(graphics_profile::image::Transfer, TransferTable)>,
}

impl Default for Soft2d<'_> {
	fn default() -> Self {
		Self::new()
	}
}

impl<'a> Soft2d<'a> {
	/// A backend for a drawing that references no images and no glyphs.
	pub fn new() -> Self {
		Self { images: &NoImages, glyphs: &NoGlyphs, cancellation: None, cache: GlyphRaster::default(), raster: Rasteriser::new(), pool: Pool::new(), masks: MaskPool::new(), spans: Spans::default(), tile: None, tables: Vec::new() }
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
		let target_rect = PixelRect::new(0, 0, target.extent.width, target.extent.height);
		let mut widest_edges = 0usize;
		for command in list.commands() {
			let (step, bound) = match command {
				Command::FillPath { path, paint, rule, transform, antialias, blend, operator, opacity } => {
					let contours = flatten_handle(resources, path.0, transform)?;
					let bound = contour_bounds(&contours).map(cover).map(|rect| rect.intersection(&target_rect));
					widest_edges = widest_edges.max(edge_count(&contours));
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
					(Step::Glyphs { run: run.0, paint: *paint, transform: *transform, blend: *blend, operator: *operator, opacity: *opacity }, bound)
				}
				Command::PushClipMask { image, transform, inverse } => (Step::PushClipMask { image: *image, transform: *transform, inverse: *inverse }, None),
				Command::PushClip { path, rule, transform, antialias, inverse } => {
					let contours = flatten_handle(resources, path.0, transform)?;
					let clip_bounds = contour_bounds(&contours).map(cover).unwrap_or(PixelRect::new(0, 0, 0, 0)).intersection(&target_rect);
					widest_edges = widest_edges.max(edge_count(&contours));
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
		expansion = expansion.min(graphics_profile::RENDER2D_PROFILE_1_MINIMA.max_filter_radius);

		// THE PYRAMIDS, for the images this list samples with minification. Built here because a
		// pyramid is an allocation and a pyramid per frame is the allocation this design exists to
		// remove.
		let mut pyramids: Vec<(u32, Pyramid)> = Vec::new();
		for (index, record) in resources.images.iter().enumerate() {
			if !wants_pyramid(list, index as u32) {
				continue;
			}
			let planar_space = self.images.planes(record.identity).map(|view| view.layout().color_space);
			let packed_space = self.images.image(record.identity).and_then(|view| view.layout().semantics.color_space());
			let Some(space) = planar_space.or(packed_space) else { continue };
			self.ensure_table(space.transfer());
			let table = Self::table_for(&self.tables, space.transfer());
			// A PLANAR SOURCE GETS ITS PYRAMID FROM THE SAME SAMPLER the drawing will use, so its
			// levels are the same decoded light rather than a second reconstruction.
			let built = match self.images.planes(record.identity) {
				Some(view) => graphics_core::sample::Sampler::planar(view, working, graphics_core::sample::Spread::Clamp, table).and_then(|sampler| Pyramid::from_sampler(&sampler, working)),
				None => {
					let Some(view) = self.images.image(record.identity) else { continue };
					Pyramid::build_with(&view, working, table)
				}
			};
			match built {
				Ok(pyramid) => pyramids.push((index as u32, pyramid)),
				Err(error) => return Err(crate::target::from_core(error)),
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
		let scratch_extent = (TILE_SIZE + expansion * 2, TILE_SIZE + expansion * 2);
		// ONE SURFACE PER OPEN LAYER, one per filter node of the largest graph, one for the blur's
		// second pass, and one for the tile itself.
		let layers_wanted = list.commands().iter().filter(|command| matches!(command, Command::BeginLayer { .. })).count();
		let filter_nodes = resources.filters.iter().map(|graph| graph.nodes().len()).max().unwrap_or(0);
		let surfaces = layers_wanted + filter_nodes + 2;
		self.pool.reserve(surfaces, scratch_extent, target.color_space)?;
		let clips_wanted = list.commands().iter().filter(|command| matches!(command, Command::PushClip { .. } | Command::PushClipMask { .. })).count();
		self.masks.reserve(clips_wanted, scratch_extent.0 as usize * scratch_extent.1 as usize);
		self.raster.reserve(scratch_extent.0 as usize, widest_edges);
		self.spans.reserve(scratch_extent.0 as usize);
		if self.tile.is_none() {
			self.tile = Some(Tile::new(TILE_SIZE));
		}

		let scratch_bytes = self.pool.scratch_bytes() + self.masks.scratch_bytes() + self.raster.scratch_bytes() + self.spans.scratch_bytes() + bins.scratch_bytes() + self.tile.as_ref().map(|tile| tile.scratch_bytes()).unwrap_or(0) + pyramids.iter().map(|(_, pyramid)| pyramid_bytes(pyramid)).sum::<u64>();
		let ceiling = graphics_profile::RENDER2D_PROFILE_1_MINIMA.max_prepared_scratch_bytes;
		if scratch_bytes > ceiling {
			// A FRAME THAT CANNOT FIT SAYS SO BEFORE IT STARTS DRAWING. Discovering it halfway through
			// a filter chain leaves a half-drawn frame, which is worse than an honest refusal.
			return Err(Error::LimitExceeded { limit: "prepared scratch", ceiling });
		}

		let damage = bounds.iter().flatten().copied().filter(|rect| !rect.is_empty()).reduce(union);
		Ok(SoftPrepared { key: PreparedKey::of(list, target, (BACKEND_NAME, BACKEND_VERSION), self.cache.generation()), steps, bounds, bins, tiling, pyramids, images: resources.images.clone(), stops: resources.stops.clone(), filters: resources.filters.clone(), glyph_runs: resources.glyph_runs.clone(), working, target_transfer: target.color_space.transfer(), expansion, scratch_bytes, damage })
	}

	fn render(&mut self, prepared: &Self::Prepared, target: &mut ImageViewMut<'_>) -> Result<(), Error> {
		let extent = target.layout().extent;
		if extent != prepared.tiling.extent {
			return Err(Error::LimitExceeded { limit: "target extent", ceiling: prepared.tiling.extent.width as u64 });
		}
		// THE BACKEND IS TAKEN APART INTO ITS FIELDS so the shaders can borrow the image table while
		// the tile loop borrows the scratch. Both are `self`, and only disjoint field borrows let one
		// be read while the other is written.
		let Soft2d { images, glyphs, cancellation, cache, raster, pool, masks, spans, tile: tile_surface, tables } = self;
		let lookup = Lookup { source: *images, records: &prepared.images, pyramids: &prepared.pyramids, tables };
		// EVERY SHADER IS BUILT ONCE PER FRAME AND NOT ONCE PER TILE. A solid paint's colour has to be
		// converted into the working space, a gradient's stops have to be resolved into a ramp, and an
		// image's sampler has to derive a colour-space matrix - and a drawing of two hundred commands
		// over eighty tiles built every one of those sixteen thousand times.
		let shaders: Vec<Shader<'_>> = prepared
			.steps
			.iter()
			.map(|step| match step {
				Step::Fill { paint, transform, .. } | Step::Image { paint, transform, .. } | Step::Glyphs { paint, transform, .. } | Step::AliasedLines { paint, transform, .. } => shader(paint, transform, prepared.working, &prepared.stops, &lookup),
				_ => Shader::Nothing,
			})
			.collect();
		let target_table = tables.iter().find(|(kind, _)| *kind == prepared.target_transfer).map(|(_, table)| table);
		let mut surface = tile_surface.take().ok_or(Error::Allocation)?;
		let mut outcome = Ok(());
		for index in 0..prepared.tiling.count() {
			// THE CANCELLATION IS ASKED BETWEEN TILES: often enough to stop promptly, rare enough to
			// cost nothing, and at a point where what has been drawn is whole tiles rather than a
			// shape cut in half.
			if cancellation.map(|cancellation| cancellation.cancelled()).unwrap_or(false) {
				outcome = Err(Error::Cancelled);
				break;
			}
			let tile = prepared.tiling.tile(index);
			if tile.is_empty() {
				continue;
			}
			surface.rebase((tile.x, tile.y));
			let scratch = Scratch { raster, pool, masks, spans, cache, glyphs: *glyphs };
			if let Err(error) = replay(prepared, target, tile, index, &mut surface, &shaders, &lookup, target_table, scratch) {
				outcome = Err(error);
				break;
			}
		}
		*tile_surface = Some(surface);
		outcome
	}
}

/// The mutable scratch one tile's replay needs, gathered so the loop can hand it over in one move.
struct Scratch<'a, 'b> {
	raster: &'a mut Rasteriser,
	pool: &'a mut Pool,
	masks: &'a mut MaskPool,
	spans: &'a mut Spans,
	cache: &'a mut GlyphRaster,
	glyphs: &'b dyn GlyphProvider,
}

#[allow(clippy::too_many_arguments)]
fn replay(prepared: &SoftPrepared, target: &mut ImageViewMut<'_>, tile: PixelRect, index: usize, surface: &mut Tile, shaders: &[Shader<'_>], lookup: &Lookup<'_>, table: Option<&TransferTable>, scratch: Scratch<'_, '_>) -> Result<(), Error> {
	{
		let Scratch { raster, pool, masks, spans, cache, glyphs } = scratch;
		surface.load(target, tile, prepared.working, table)?;
		let mut clips = ClipStack::new();
		clips.reset(tile);
		let mut layers: Vec<Layer> = Vec::new();
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
					let (into, bounds): (&mut dyn Raster, PixelRect) = match layers.last_mut() {
						Some(layer) => (&mut layer.surface, layer.bounds),
						None => (&mut *surface, tile),
					};
					fill_edges(raster, spans, edges, rule, antialias, bounds, into, shader, &clips, *opacity, *blend, *operator);
				}
				Step::Glyphs { run, transform, blend, operator, opacity, .. } => {
					let Some(recorded) = prepared.glyph_runs.get(*run as usize) else { continue };
					let Some(shader) = shaders.get(*command as usize) else { continue };
					let (into, bounds): (&mut dyn Raster, PixelRect) = match layers.last_mut() {
						Some(layer) => (&mut layer.surface, layer.bounds),
						None => (&mut *surface, tile),
					};
					draw_glyphs(raster, spans, cache, glyphs, recorded, transform, bounds, into, shader, &clips, *opacity, *blend, *operator, prepared.working);
				}
				Step::AliasedLines { points, blend, operator, opacity, .. } => {
					let Some(shader) = shaders.get(*command as usize) else { continue };
					let (into, bounds): (&mut dyn Raster, PixelRect) = match layers.last_mut() {
						Some(layer) => (&mut layer.surface, layer.bounds),
						None => (&mut *surface, tile),
					};
					draw_aliased_lines(points, bounds, into, shader, &clips, *opacity, *blend, *operator);
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
							raster.fill_edges(edges, *rule, *antialias, level_bounds, |y, coverage| {
								let Some(row) = y.checked_sub(level_bounds.y) else { return };
								let start = row as usize * stride;
								if let Some(slice) = mask.get_mut(start..start + stride.min(coverage.len())) {
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
						None => return Err(Error::LimitExceeded { limit: "prepared scratch", ceiling: graphics_profile::RENDER2D_PROFILE_1_MINIMA.max_prepared_scratch_bytes }),
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
							Some(crate::filter::evaluate(graph, &layer.surface, backdrop, layer.bounds, pool, spans, prepared.working, lookup)?)
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
		for layer in layers {
			pool.give(layer.surface);
		}
		clips.drain_into(masks);
		surface.store(target, tile, prepared.working, table, &mut spans.filter_input)
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
	fn reserve(&mut self, width: usize) {
		if self.source.len() < width {
			self.source.resize(width, Rgba::TRANSPARENT);
			self.destination.resize(width, Rgba::TRANSPARENT);
			self.weights.resize(width, 0.0);
			self.filter_input.resize(width, Rgba::TRANSPARENT);
			self.filter_output.resize(width, Rgba::TRANSPARENT);
		}
	}

	fn scratch_bytes(&self) -> u64 {
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
	raster.fill_edges(edges, rule, antialias, bounds, |y, coverage| {
		// THE RUN IS TRIMMED TO WHAT THE SHAPE ACTUALLY COVERS, because a row of a tile is sixty-four
		// pixels and a shape's edge crosses a handful of them: compositing the whole row would spend
		// the cost of a full-width span on a one-pixel line.
		let mut first = coverage.len();
		let mut last = 0usize;
		for (offset, value) in coverage.iter().enumerate() {
			let mut weight = value.clamp(0.0, 1.0) * opacity;
			if weight > 0.0 && !rectangular {
				weight *= clips.coverage(bounds.x + offset as u32, y);
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
		let length = last - first;
		for offset in 0..length {
			let x = bounds.x + (first + offset) as u32;
			spans.source[offset] = shader.at(x as f32, y as f32);
		}
		crate::span::scale_span(&mut spans.source[..length], &spans.weights[first..last]);
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

/// Draw one recorded run, glyph by glyph, through the cache.
#[allow(clippy::too_many_arguments)]
fn draw_glyphs(raster: &mut Rasteriser, spans: &mut Spans, cache: &mut GlyphRaster, provider: &dyn GlyphProvider, run: &render2d::list::RecordedGlyphRun, transform: &Transform, bounds: PixelRect, into: &mut dyn Raster, shader: &Shader<'_>, clips: &ClipStack, opacity: f32, blend: BlendMode, operator: Operator, working: Working) {
	let mut pen_x = run.origin_x;
	let mut pen_y = run.origin_y;
	for glyph in &run.glyphs {
		let x = add_fixed(pen_x, glyph.x_offset);
		let y = add_fixed(pen_y, glyph.y_offset);
		let key = font_contract::cache::GlyphCacheKey { face: run.face.face, generation: run.face.generation, glyph: glyph.glyph, size: run.size, variation: run.variation, transform: font_contract::glyph::TransformKey::new([transform.m[0][0], transform.m[1][0], transform.m[0][1], transform.m[1][1], 0.0, 0.0]).unwrap_or(font_contract::glyph::TransformKey::IDENTITY), phase: font_contract::glyph::SubpixelPhase::of(x, y), kind: glyph.kind, selection: glyph.selection, mode: run.mode };
		let origin = PointF { x: pixels(x), y: pixels(y) };
		match cache.get(&key, provider) {
			GlyphImage::Missing => {}
			GlyphImage::Outline(path) => {
				let placed = Transform::translate(origin.x, origin.y);
				let contours = render2d::flatten::flatten(path, Some(&placed));
				fill_edges(raster, spans, &crate::raster::Edges::build(&contours), FillRule::NonZero, Antialias::On, bounds, into, shader, clips, opacity, blend, operator);
			}
			GlyphImage::Layers(layers) => {
				// `COLR` LAYERS ARE DRAWN IN ORDER, each with its own palette colour - which is what
				// makes an emoji an emoji rather than a silhouette.
				let placed = Transform::translate(origin.x, origin.y);
				for (path, colour) in layers {
					let contours = render2d::flatten::flatten(path, Some(&placed));
					let solid = Shader::Solid(to_working(*colour, working));
					fill_edges(raster, spans, &crate::raster::Edges::build(&contours), FillRule::NonZero, Antialias::On, bounds, into, &solid, clips, opacity, blend, operator);
				}
			}
			GlyphImage::Mask { left, top, width, height, coverage, mode } => {
				let base_x = origin.x as i64 + *left as i64;
				let base_y = origin.y as i64 + *top as i64;
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
			GlyphImage::Bitmap { left, top, image } => {
				let base_x = origin.x as i64 + *left as i64;
				let base_y = origin.y as i64 + *top as i64;
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
		pen_x = add_fixed(pen_x, glyph.x_advance);
		pen_y = add_fixed(pen_y, glyph.y_advance);
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
	list.commands().iter().any(|command| match command {
		Command::DrawImage { image: handle, quality, .. } => handle.0 == image && matches!(quality, ImageQuality::Mipmapped),
		Command::FillPath { paint: Paint::Image { image: handle, quality, .. }, .. } | Command::StrokePath { paint: Paint::Image { image: handle, quality, .. }, .. } | Command::DrawGlyphRun { paint: Paint::Image { image: handle, quality, .. }, .. } => handle.0 == image && matches!(quality, ImageQuality::Mipmapped),
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
