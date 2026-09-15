//! THE `Canvas`: an immediate-mode surface that RECORDS.
//!
//! `save`, `restore`, `set_transform`, `concat_transform`, `set_clip`, `set_opacity`,
//! `set_blend_mode`, `begin_layer`, `end_layer` and the drawing calls - the shape every 2D API has,
//! because it is the shape callers already know. What it PRODUCES is the difference.
//!
//! SAVED STATE IS A STACK AND `restore` WITHOUT A `save` IS AN ERROR. Treating it as a no-op is how a
//! component that restores one time too many silently inherits its parent's clip - and the drawing
//! that results is wrong somewhere else, in a component that did nothing.

use alloc::vec::Vec;

use graphics_core::geom::RectF;

use crate::Error;
use crate::blend::{Antialias, BlendMode, Operator};
use crate::list::{Command, DrawList, DrawListBuilder, ImageRecord, RecordedGlyphRun};
use crate::paint::{ImageQuality, Paint};
use crate::path::{FillRule, Path, StrokeStyle};
use crate::resource::FilterHandle;
use crate::transform::Transform;

/// Everything `save` saves.
#[derive(Clone, Copy, PartialEq, Debug)]
struct State {
	transform: Transform,
	opacity: f32,
	blend: BlendMode,
	operator: Operator,
	antialias: Antialias,
	/// How many clips this state pushed, so `restore` pops exactly those.
	clips: u32,
}

impl Default for State {
	fn default() -> Self {
		Self { transform: Transform::IDENTITY, opacity: 1.0, blend: BlendMode::Normal, operator: Operator::SrcOver, antialias: Antialias::On, clips: 0 }
	}
}

/// Records a drawing.
pub struct Canvas {
	builder: DrawListBuilder,
	state: State,
	saved: Vec<State>,
	layers: u32,
}

impl Default for Canvas {
	fn default() -> Self {
		Self::new()
	}
}

impl Canvas {
	pub fn new() -> Self {
		Self { builder: DrawListBuilder::new(), state: State::default(), saved: Vec::new(), layers: 0 }
	}

	/// Start a new recording, KEEPING the capacity - which is what makes the next frame free.
	pub fn restart(&mut self) {
		self.builder.restart();
		self.state = State::default();
		self.saved.clear();
		self.layers = 0;
	}

	pub fn save(&mut self) -> Result<(), Error> {
		let limits = graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS;
		if self.saved.len() as u64 + 1 > limits.max_clip_depth as u64 {
			return Err(Error::LimitExceeded { limit: "saved states", ceiling: limits.max_clip_depth as u64 });
		}
		// THE PUSHED STATE KEEPS ITS OWN CLIP COUNT and the live one restarts at zero, so a restore
		// pops the clips pushed SINCE this save and not the ones its parent pushed.
		self.saved.push(self.state);
		self.state.clips = 0;
		Ok(())
	}

	/// Undo to the last `save`, popping every clip pushed since it.
	pub fn restore(&mut self) -> Result<(), Error> {
		let previous = self.saved.pop().ok_or(Error::UnbalancedSave)?;
		for _ in 0..self.state.clips {
			self.builder.push(Command::PopClip)?;
		}
		self.state = previous;
		Ok(())
	}

	pub fn set_transform(&mut self, transform: Transform) {
		self.state.transform = transform;
	}

	/// Apply a transform INSIDE the one already set, which is what nesting means.
	pub fn concat_transform(&mut self, transform: &Transform) {
		self.state.transform = self.state.transform.concat(transform);
	}

	pub fn transform(&self) -> Transform {
		self.state.transform
	}

	pub fn set_opacity(&mut self, opacity: f32) {
		self.state.opacity = opacity.clamp(0.0, 1.0);
	}

	pub fn set_blend_mode(&mut self, blend: BlendMode) {
		self.state.blend = blend;
	}

	pub fn set_operator(&mut self, operator: Operator) {
		self.state.operator = operator;
	}

	pub fn set_antialias(&mut self, antialias: Antialias) {
		self.state.antialias = antialias;
	}

	/// Intersect the clip with a path. CLIPS INTERSECT AND NEVER REPLACE: a clip that replaced would
	/// let a child draw outside its parent, which is the one thing a clip exists to prevent.
	pub fn set_clip(&mut self, path: Path, rule: FillRule) -> Result<(), Error> {
		self.clip(path, rule, false)
	}

	/// Intersect the clip with everything OUTSIDE a path: a knockout, a spotlight, a hole in a panel.
	pub fn set_clip_inverse(&mut self, path: Path, rule: FillRule) -> Result<(), Error> {
		self.clip(path, rule, true)
	}

	fn clip(&mut self, path: Path, rule: FillRule, inverse: bool) -> Result<(), Error> {
		self.has_image(&path)?;
		let handle = self.builder.add_path(path)?;
		self.builder.push(Command::PushClip { path: handle, rule, transform: self.state.transform, antialias: self.state.antialias, inverse })?;
		self.state.clips = self.state.clips.saturating_add(1);
		Ok(())
	}

	/// Intersect the clip with an IMAGE'S ALPHA, which is the mask whose shape has no outline.
	pub fn set_clip_mask(&mut self, image: ImageRecord, inverse: bool) -> Result<(), Error> {
		let handle = self.builder.add_image(image)?;
		self.builder.push(Command::PushClipMask { image: handle, transform: self.state.transform, inverse })?;
		self.state.clips = self.state.clips.saturating_add(1);
		Ok(())
	}

	pub fn fill_path(&mut self, path: Path, paint: Paint, rule: FillRule) -> Result<(), Error> {
		self.has_image(&path)?;
		let handle = self.builder.add_path(path)?;
		self.builder.push(Command::FillPath { path: handle, paint, rule, transform: self.state.transform, antialias: self.state.antialias, blend: self.state.blend, operator: self.state.operator, opacity: self.state.opacity })
	}

	pub fn stroke_path(&mut self, path: Path, paint: Paint, style: StrokeStyle) -> Result<(), Error> {
		self.has_image(&path)?;
		let handle = self.builder.add_path(path)?;
		self.builder.push(Command::StrokePath { path: handle, paint, style, transform: self.state.transform, antialias: self.state.antialias, blend: self.state.blend, operator: self.state.operator, opacity: self.state.opacity })
	}

	pub fn draw_image(&mut self, image: ImageRecord, source: RectF, destination: RectF, quality: ImageQuality) -> Result<(), Error> {
		let handle = self.builder.add_image(image)?;
		self.builder.push(Command::DrawImage { image: handle, source, destination, quality, transform: self.state.transform, blend: self.state.blend, operator: self.state.operator, opacity: self.state.opacity })
	}

	/// TEXT ENTERS AS A RUN AND NEVER AS A STRING. A drawing API that took a string would be a text
	/// layout engine with a rasteriser attached - it would need a font database, a shaper, a bidi
	/// implementation and a line breaker, and every caller that already had those would be running
	/// them twice.
	pub fn draw_glyph_run(&mut self, run: RecordedGlyphRun, paint: Paint) -> Result<(), Error> {
		let handle = self.builder.add_glyph_run(run)?;
		self.builder.push(Command::DrawGlyphRun { run: handle, paint, transform: self.state.transform, blend: self.state.blend, operator: self.state.operator, opacity: self.state.opacity })
	}

	/// Begin an offscreen layer. Everything until `end_layer` draws into it, and the layer is then
	/// composited as ONE thing - which is what group opacity means and why it cannot be done by
	/// multiplying each drawing's alpha.
	pub fn begin_layer(&mut self, bounds: Option<RectF>, opacity: f32, blend: BlendMode, filter: Option<FilterHandle>) -> Result<(), Error> {
		let limits = graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS;
		if self.layers as u64 + 1 > limits.max_layer_depth as u64 {
			return Err(Error::LimitExceeded { limit: "layer depth", ceiling: limits.max_layer_depth as u64 });
		}
		self.layers += 1;
		self.builder.push(Command::BeginLayer { bounds, opacity: opacity.clamp(0.0, 1.0), blend, operator: self.state.operator, filter })
	}

	pub fn end_layer(&mut self) -> Result<(), Error> {
		self.layers = self.layers.checked_sub(1).ok_or(Error::UnbalancedLayer)?;
		self.builder.push(Command::EndLayer)
	}

	/// Freeze the recording.
	///
	/// AN UNBALANCED CANVAS IS REFUSED HERE rather than producing a list that replays wrongly: a
	/// missing `end_layer` means everything since the `begin_layer` was recorded into an offscreen
	/// nothing composites, and the drawing would simply be missing.
	pub fn finish(&self) -> Result<DrawList, Error> {
		let mut list = DrawList::default();
		self.finish_into(&mut list)?;
		Ok(list)
	}

	/// The same freeze, into a list the caller already holds - see `DrawListBuilder::finish_into`.
	///
	/// A frame loop records the same shape of list every tick, and a `finish` that allocated a fresh
	/// one each time would charge a `Vec` per resource kind sixty times a second for ever. With
	/// `restart` on the way in and this on the way out, a steady loop records a frame and allocates
	/// nothing at all.
	/// ONE RULE FOR EVERY REFUSAL: an error leaves the caller's list EMPTY. These two checks refuse
	/// before anything is written, so the list would otherwise keep the PREVIOUS frame's recording -
	/// which a caller that ignored the error would present again as though it were this frame's.
	pub fn finish_into(&self, list: &mut DrawList) -> Result<(), Error> {
		if !self.saved.is_empty() {
			list.clear();
			return Err(Error::UnbalancedSave);
		}
		if self.layers != 0 {
			list.clear();
			return Err(Error::UnbalancedLayer);
		}
		self.builder.finish_into(list)
	}

	/// Refuse, AT THE CALL, a geometry that the current transform leaves with no image at all.
	///
	/// ONLY UNDER A PROJECTIVE TRANSFORM IS THERE ANYTHING TO CHECK, so an affine drawing pays
	/// nothing: an affine transform has no horizon, every point has an image, and flattening a path
	/// here to learn that would make every fill cost a flattening it does not need.
	fn has_image(&self, path: &Path) -> Result<(), Error> {
		if self.state.transform.is_affine() {
			return Ok(());
		}
		crate::flatten::flatten_checked(path, Some(&self.state.transform)).map(|_| ())
	}

	/// The builder, for a caller adding a resource before it draws with it.
	pub fn resources(&mut self) -> &mut DrawListBuilder {
		&mut self.builder
	}
}
