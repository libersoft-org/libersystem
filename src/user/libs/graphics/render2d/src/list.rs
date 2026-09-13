//! THE `DrawList`: immutable, versioned, cacheable and replayable in-process.
//!
//! WHAT A CANVAS PRODUCES IS A LIST AND NOT PIXELS, and that indirection is the whole design. The
//! same drawing is then replayable by a CPU backend now and a GPU backend later, cacheable per
//! component, analysable for damage, and testable without any backend at all. A canvas that writes
//! straight into a slice is a CPU-only API however carefully it is written.
//!
//! THE RESOURCE TABLE IS TYPED HANDLES AND NOT INLINE REFERENCES. A list holding references cannot
//! outlive what it points at, cannot be hashed into a cache key, and cannot be encoded - so the
//! transportable form later becomes an EXTENSION of this schema rather than a replacement for it.
//!
//! AND IT IS VERSIONED, because a cached list outlives the code that recorded it. A schema change
//! that did not change this number would replay an old list under new meanings, which is a drawing
//! that is wrong in a way nothing reports.

use alloc::vec::Vec;

use font_contract::cluster::Cluster;
use font_contract::{Fixed266, PositionedGlyph};
use graphics_core::geom::RectF;

use crate::Error;
use crate::blend::{Antialias, BlendMode, Operator};
use crate::filter::FilterGraph;
use crate::paint::{GradientStop, Paint};
use crate::path::{FillRule, Path, StrokeStyle};
use crate::resource::{FilterHandle, GlyphRunHandle, ImageHandle, PaintHandle, PathHandle};
use crate::transform::Transform;

/// The command schema's version. A cached list outlives the code that recorded it.
pub const DRAW_LIST_VERSION: u32 = 2;

/// One recorded command.
#[derive(Clone, PartialEq, Debug)]
pub enum Command {
	/// Fill a path under a rule.
	FillPath { path: PathHandle, paint: Paint, rule: FillRule, transform: Transform, antialias: Antialias, blend: BlendMode, operator: Operator, opacity: f32 },
	/// Stroke a path.
	StrokePath { path: PathHandle, paint: Paint, style: StrokeStyle, transform: Transform, antialias: Antialias, blend: BlendMode, operator: Operator, opacity: f32 },
	/// Draw an image into a destination rectangle.
	DrawImage { image: ImageHandle, source: RectF, destination: RectF, quality: crate::paint::ImageQuality, transform: Transform, blend: BlendMode, operator: Operator, opacity: f32 },
	/// Draw a run of glyphs. TEXT ENTERS AS A RUN AND NEVER AS A STRING: a drawing API that took a
	/// string would be a text layout engine with a rasteriser attached.
	DrawGlyphRun { run: GlyphRunHandle, paint: Paint, transform: Transform, blend: BlendMode, operator: Operator, opacity: f32 },
	/// Intersect the clip with a path, or with everything OUTSIDE it.
	///
	/// INVERSE IS A FLAG AND NOT A SECOND COMMAND, because "everything except this shape" is one
	/// subtraction over the same mask - a knockout, a spotlight and a hole punched in a panel are all
	/// this, and an API without it makes each of them a path built by hand with the winding reversed.
	PushClip { path: PathHandle, rule: FillRule, transform: Transform, antialias: Antialias, inverse: bool },
	/// Intersect the clip with an IMAGE'S ALPHA.
	///
	/// AN ALPHA MASK IS IN THE PROFILE AND IS NOT A PATH. A soft-edged vignette, a decoded PNG used as
	/// a stencil and a gradient fade are masks whose shape has no outline, and an API that only clips
	/// to paths makes each of them a layer with a filter.
	PushClipMask { image: ImageHandle, transform: Transform, inverse: bool },
	/// Undo the innermost clip.
	PopClip,
	/// Begin an offscreen layer.
	BeginLayer { bounds: Option<RectF>, opacity: f32, blend: BlendMode, operator: Operator, filter: Option<FilterHandle> },
	/// Composite the innermost layer into what is under it.
	EndLayer,
}

/// A glyph run as a list records it: the seam's positioned glyphs, plus what the seam's `GlyphRun`
/// carries about the face they came from.
#[derive(Clone, PartialEq, Debug)]
pub struct RecordedGlyphRun {
	pub face: font_contract::FaceRef,
	pub size: Fixed266,
	pub variation: font_contract::VariationCoordinates,
	pub script: font_contract::ScriptTag,
	pub direction: font_contract::Direction,
	pub mode: font_contract::RasterisationMode,
	pub origin_x: Fixed266,
	pub origin_y: Fixed266,
	pub glyphs: Vec<PositionedGlyph>,
	/// The mapping, kept beside the glyphs so a hit test over a recorded drawing is possible at all.
	pub clusters: Vec<Cluster>,
}

/// Everything a list references, by kind.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ResourceTable {
	pub paths: Vec<Path>,
	pub images: Vec<ImageRecord>,
	pub stops: Vec<Vec<GradientStop>>,
	/// Dash patterns, as on-off lengths in the space the stroke's scaling rule measures.
	pub dashes: Vec<Vec<f32>>,
	pub glyph_runs: Vec<RecordedGlyphRun>,
	pub filters: Vec<FilterGraph>,
}

/// An image a list references, by the identity it was recorded against.
///
/// AN IDENTITY AND A GENERATION, NOT A POINTER. A prepared list is bound to the generation, so a
/// replaced image invalidates it - and an image that is merely REDRAWN, a video frame, bumps its
/// content counter instead, which refreshes one cache and re-flattens nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ImageRecord {
	pub identity: u64,
	/// Bumped when the image is REPLACED. Structural: invalidates a prepared list.
	pub layout_generation: u64,
	/// Bumped when its CONTENT changes. A new video frame; refreshes the upload cache alone.
	pub content_generation: u64,
}

/// An immutable, validated display list.
#[derive(Clone, PartialEq, Debug)]
pub struct DrawList {
	version: u32,
	commands: Vec<Command>,
	resources: ResourceTable,
}

impl DrawList {
	pub fn version(&self) -> u32 {
		self.version
	}

	pub fn commands(&self) -> &[Command] {
		&self.commands
	}

	pub fn resources(&self) -> &ResourceTable {
		&self.resources
	}

	/// EVERY HANDLE AGAINST THE LIST'S OWN TABLE, and every nesting balanced.
	///
	/// THE VALIDATION BOUNDARY IS HERE, once, so a backend may then assume. A backend that re-checked
	/// would be the layer where the check that matters is the one nobody wrote; a backend that did
	/// not, over a list nobody had checked, is the other failure.
	pub fn validate(&self) -> Result<(), Error> {
		let mut clips = 0usize;
		let mut layers = 0usize;
		let limits = graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS;
		for command in &self.commands {
			for (kind, index, length) in self.referenced(command) {
				if index as usize >= length {
					return Err(Error::UnknownResource { kind, index });
				}
			}
			match command {
				Command::PushClip { .. } | Command::PushClipMask { .. } => {
					clips += 1;
					if clips as u64 > limits.max_clip_depth as u64 {
						return Err(Error::LimitExceeded { limit: "clip depth", ceiling: limits.max_clip_depth as u64 });
					}
				}
				Command::PopClip => {
					clips = clips.checked_sub(1).ok_or(Error::UnbalancedClip)?;
				}
				Command::BeginLayer { .. } => {
					layers += 1;
					if layers as u64 > limits.max_layer_depth as u64 {
						return Err(Error::LimitExceeded { limit: "layer depth", ceiling: limits.max_layer_depth as u64 });
					}
				}
				Command::EndLayer => {
					layers = layers.checked_sub(1).ok_or(Error::UnbalancedLayer)?;
				}
				_ => {}
			}
		}
		// AN UNCLOSED LAYER IS NOT A DRAWING. Its contents were recorded into an offscreen nothing
		// composites, so replaying the list would silently lose everything inside it.
		if clips != 0 {
			return Err(Error::UnbalancedClip);
		}
		if layers != 0 {
			return Err(Error::UnbalancedLayer);
		}
		for run in &self.resources.glyph_runs {
			if run.glyphs.len() as u64 > limits.max_glyphs_per_run as u64 {
				return Err(Error::LimitExceeded { limit: "glyphs per run", ceiling: limits.max_glyphs_per_run as u64 });
			}
		}
		Ok(())
	}

	/// Which table entries a command names, as `(kind, index, how many there are)`.
	fn referenced(&self, command: &Command) -> Vec<(crate::resource::ResourceKind, u32, usize)> {
		use crate::resource::ResourceKind;
		let mut out = Vec::new();
		let paint_of = |paint: &Paint, out: &mut Vec<_>| match paint {
			Paint::Linear { stops, .. } | Paint::Radial { stops, .. } | Paint::Conic { stops, .. } => out.push((ResourceKind::Stops, stops.0, self.resources.stops.len())),
			Paint::Image { image, .. } => out.push((ResourceKind::Image, image.0, self.resources.images.len())),
			Paint::Solid(_) => {}
		};
		match command {
			Command::FillPath { path, paint, .. } => {
				out.push((ResourceKind::Path, path.0, self.resources.paths.len()));
				paint_of(paint, &mut out);
			}
			Command::StrokePath { path, paint, style, .. } => {
				out.push((ResourceKind::Path, path.0, self.resources.paths.len()));
				paint_of(paint, &mut out);
				if let Some(dash) = style.dash {
					out.push((ResourceKind::Dashes, dash.pattern.0, self.resources.dashes.len()));
				}
			}
			Command::DrawImage { image, .. } => out.push((ResourceKind::Image, image.0, self.resources.images.len())),
			Command::DrawGlyphRun { run, paint, .. } => {
				out.push((ResourceKind::GlyphRun, run.0, self.resources.glyph_runs.len()));
				paint_of(paint, &mut out);
			}
			Command::PushClip { path, .. } => out.push((ResourceKind::Path, path.0, self.resources.paths.len())),
			Command::PushClipMask { image, .. } => out.push((ResourceKind::Image, image.0, self.resources.images.len())),
			Command::BeginLayer { filter, .. } => {
				if let Some(filter) = filter {
					out.push((ResourceKind::Filter, filter.0, self.resources.filters.len()));
				}
			}
			Command::PopClip | Command::EndLayer => {}
		}
		out
	}
}

/// Builds a list, DEDUPLICATING its resources and retaining its capacity.
///
/// RE-RECORDING WITHIN THE RESERVATION ALLOCATES NOTHING. A transform, an opacity, a colour, a scroll
/// offset and an image frame are what an animation changes between frames, and a builder that
/// allocated for any of them would allocate sixty times a second forever.
#[derive(Default)]
pub struct DrawListBuilder {
	commands: Vec<Command>,
	resources: ResourceTable,
}

impl DrawListBuilder {
	pub fn new() -> Self {
		Self::default()
	}

	/// Clear the commands and the table but KEEP the capacity, which is what makes the next frame
	/// free.
	pub fn restart(&mut self) {
		self.commands.clear();
		self.resources.paths.clear();
		self.resources.images.clear();
		self.resources.stops.clear();
		self.resources.dashes.clear();
		self.resources.glyph_runs.clear();
		self.resources.filters.clear();
	}

	/// Add a path, REUSING an identical one already in the table.
	///
	/// DEDUPLICATION IS NOT AN OPTIMISATION HERE. A component that draws the same rounded rectangle
	/// forty times records it once, which is what keeps the command and resource ceilings meaningful
	/// - a list that stored forty copies would exceed them for a drawing that has one shape in it.
	pub fn add_path(&mut self, path: Path) -> Result<PathHandle, Error> {
		if let Some(index) = self.resources.paths.iter().position(|existing| *existing == path) {
			return Ok(PathHandle(index as u32));
		}
		self.reserve_resource(self.resources.paths.len())?;
		self.resources.paths.push(path);
		Ok(PathHandle(self.resources.paths.len() as u32 - 1))
	}

	pub fn add_image(&mut self, record: ImageRecord) -> Result<ImageHandle, Error> {
		if let Some(index) = self.resources.images.iter().position(|existing| *existing == record) {
			return Ok(ImageHandle(index as u32));
		}
		self.reserve_resource(self.resources.images.len())?;
		self.resources.images.push(record);
		Ok(ImageHandle(self.resources.images.len() as u32 - 1))
	}

	pub fn add_stops(&mut self, stops: Vec<GradientStop>) -> Result<PaintHandle, Error> {
		self.reserve_resource(self.resources.stops.len())?;
		self.resources.stops.push(stops);
		Ok(PaintHandle(self.resources.stops.len() as u32 - 1))
	}

	/// Add a dash pattern, as on-off lengths.
	///
	/// A PATTERN WITH NO POSITIVE LENGTH IS A SOLID STROKE and not an infinite loop: a dasher walking
	/// a pattern of zeroes never advances, and refusing it here is cheaper than detecting it in the
	/// loop that draws.
	pub fn add_dashes(&mut self, dashes: Vec<f32>) -> Result<crate::resource::DashHandle, Error> {
		if !dashes.iter().any(|length| length.is_finite() && *length > 0.0) {
			return Err(Error::DegenerateDash);
		}
		self.reserve_resource(self.resources.dashes.len())?;
		self.resources.dashes.push(dashes);
		Ok(crate::resource::DashHandle(self.resources.dashes.len() as u32 - 1))
	}

	pub fn add_glyph_run(&mut self, run: RecordedGlyphRun) -> Result<GlyphRunHandle, Error> {
		self.reserve_resource(self.resources.glyph_runs.len())?;
		self.resources.glyph_runs.push(run);
		Ok(GlyphRunHandle(self.resources.glyph_runs.len() as u32 - 1))
	}

	pub fn add_filter(&mut self, graph: FilterGraph) -> Result<FilterHandle, Error> {
		self.reserve_resource(self.resources.filters.len())?;
		self.resources.filters.push(graph);
		Ok(FilterHandle(self.resources.filters.len() as u32 - 1))
	}

	pub fn push(&mut self, command: Command) -> Result<(), Error> {
		let limits = graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS;
		if self.commands.len() as u64 + 1 > limits.max_commands as u64 {
			return Err(Error::LimitExceeded { limit: "commands", ceiling: limits.max_commands as u64 });
		}
		self.commands.push(command);
		Ok(())
	}

	/// Freeze the recording. The list is VALIDATED here, so a list that exists is a list that is
	/// valid - and a backend receiving one does not have to wonder.
	pub fn finish(&self) -> Result<DrawList, Error> {
		let list = DrawList { version: DRAW_LIST_VERSION, commands: self.commands.clone(), resources: self.resources.clone() };
		list.validate()?;
		Ok(list)
	}

	fn reserve_resource(&self, existing: usize) -> Result<(), Error> {
		let limits = graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS;
		if existing as u64 + 1 > limits.max_resources as u64 {
			return Err(Error::LimitExceeded { limit: "resources", ceiling: limits.max_resources as u64 });
		}
		Ok(())
	}
}
