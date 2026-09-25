//! RENDER PASSES: the attachments a frame writes, and what happens at each end of one.
//!
//! LOAD AND STORE ARE PER ATTACHMENT AND ARE NOT A CONVENIENCE. `Clear` has a value, `Load` keeps
//! what is there, and `Discard` says NOBODY MAY READ IT. The third is the one that matters: a
//! discarded attachment whose contents are read is one backend's uninitialised memory becoming
//! another's black, so this crate FILLS a discarded attachment with a poison value rather than
//! leaving it - a frame that reads what it discarded then looks wrong everywhere instead of looking
//! right on the machine it was written on.
//!
//! THE FRAGMENT PATH IS THE FROZEN ORDER AND NOTHING ELSE: sample mask, then alpha-to-coverage, then
//! the stencil test, then the depth test, then blending, then the colour write mask. Every one of
//! those is `render3d`'s own arithmetic; this file schedules it over the samples of a pixel and owns
//! no equation of its own.
//!
//! MULTISAMPLING RESOLVES BY AVERAGING COLOUR AND BY TAKING SAMPLE ZERO FOR AN INTEGER. Averaging an
//! object identity produces an identity that belongs to no object, which is the defect that makes a
//! pick at a silhouette select something that is not there.

use alloc::vec;
use alloc::vec::Vec;

use render_math::Vec4;
use render3d::blend::{AttachmentBlend, blend};
use render3d::depth::{DepthFormat, Stored};
use render3d::resource::{LoadOp, StoreOp};
use render3d::{CompareOp, Error};

use crate::raster::TILE;

/// The value a discarded attachment is filled with.
///
/// A POISON AND NOT A CLEAR. A caller that reads a discarded attachment gets something obviously
/// wrong everywhere rather than something plausible on the machine it was developed on - which is
/// the difference between a defect found in an afternoon and one found by a user.
pub const POISON: Vec4 = Vec4 { x: 1.0, y: 0.0, z: 1.0, w: 1.0 };

/// Where a pixel's samples live in an attachment's storage.
///
/// TILE-MAJOR, AND PRIVATE TO THIS FILE. Each `TILE`-square tile of the target is one contiguous
/// run - tile rows top to bottom, the tiles of a row left to right, and inside a tile its pixels
/// row by row with a pixel's samples together - and the tiles on the right and bottom edges are as
/// narrow as the target leaves them, so the storage is exactly `width x height x samples` values
/// and no memory formula depends on the order. The order is what lets a frame hand each tile's
/// pixels to a different worker as a slice of its own: a row-major attachment interleaves every
/// tile of a row with its neighbours, and there is no safe way to give two workers two columns of
/// one row.
///
/// NOTHING OUTSIDE THIS FILE CAN SEE IT. Every read and write goes through a pixel address, and
/// an address outside the target is answered as outside rather than folded into a neighbour -
/// which the row-major form did for an `x` past the right edge, landing on the next row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Layout {
	width: u32,
	height: u32,
	samples: u32,
}

impl Layout {
	/// The position of one sample in the whole attachment, or `None` outside it.
	fn index(&self, x: u32, y: u32, sample: u32) -> Option<usize> {
		if x >= self.width || y >= self.height || sample >= self.samples {
			return None;
		}
		let (band, column) = (y / TILE * TILE, x / TILE * TILE);
		let rows = (self.height - band).min(TILE) as usize;
		let columns = (self.width - column).min(TILE) as usize;
		let before = band as usize * self.width as usize + column as usize * rows;
		let within = (y - band) as usize * columns + (x - column) as usize;
		Some((before + within) * self.samples as usize + sample as usize)
	}

	/// How many values each tile holds, in the order the storage holds the tiles.
	fn tile_lengths(&self) -> impl Iterator<Item = usize> + use<> {
		let Layout { width, height, samples } = *self;
		(0..height.div_ceil(TILE)).flat_map(move |row| {
			let rows = (height - row * TILE).min(TILE) as usize;
			(0..width.div_ceil(TILE)).map(move |column| (width - column * TILE).min(TILE) as usize * rows * samples as usize)
		})
	}
}

/// Cut `values` into one slice per tile, answering each with where it starts in the whole.
///
/// IT STOPS RATHER THAN PANICS when the storage is shorter than the extent says - which only a
/// caller that rewrote the public extent fields can arrange - so the frame counts fewer tiles than
/// it binned and refuses the draw instead of shading a view that is not the tile it names.
fn split<T>(values: &mut [T], layout: Layout) -> impl Iterator<Item = (usize, &mut [T])> {
	let mut rest = values;
	let mut base = 0;
	layout.tile_lengths().map_while(move |length| {
		if length > rest.len() {
			return None;
		}
		let (head, tail) = core::mem::take(&mut rest).split_at_mut(length);
		rest = tail;
		let start = base;
		base += length;
		Some((start, head))
	})
}

/// One colour attachment's storage: `samples` values per pixel.
#[derive(Clone, PartialEq, Debug)]
pub struct Colour {
	pub width: u32,
	pub height: u32,
	pub samples: u32,
	/// Whether this attachment holds an INTEGER identity rather than colour, which changes how it
	/// resolves and forbids blending it.
	pub integer: bool,
	values: Vec<Vec4>,
}

impl Colour {
	pub fn new(width: u32, height: u32, samples: u32, integer: bool) -> Self {
		Self { width, height, samples, integer, values: vec![Vec4::ZERO; (width * height * samples) as usize] }
	}

	fn layout(&self) -> Layout {
		Layout { width: self.width, height: self.height, samples: self.samples }
	}

	pub fn at(&self, x: u32, y: u32, sample: u32) -> Vec4 {
		self.layout().index(x, y, sample).and_then(|index| self.values.get(index)).copied().unwrap_or(Vec4::ZERO)
	}

	pub fn set(&mut self, x: u32, y: u32, sample: u32, value: Vec4) {
		if let Some(slot) = self.layout().index(x, y, sample).and_then(|index| self.values.get_mut(index)) {
			*slot = value;
		}
	}

	pub fn fill(&mut self, value: Vec4) {
		for slot in &mut self.values {
			*slot = value;
		}
	}

	/// The whole attachment, as something a fragment is written into.
	pub fn view(&mut self) -> ColourView<'_> {
		ColourView { layout: self.layout(), integer: self.integer, base: 0, values: &mut self.values }
	}

	/// One view per tile, in row-major tile order - each of them the only way to reach its pixels.
	pub fn tiles(&mut self) -> impl Iterator<Item = ColourView<'_>> {
		let (layout, integer) = (self.layout(), self.integer);
		split(&mut self.values, layout).map(move |(base, values)| ColourView { layout, integer, base, values })
	}

	/// Resolve to one value per pixel.
	///
	/// COLOUR AVERAGES AND AN INTEGER TAKES SAMPLE ZERO. Averaging an identity produces one that
	/// belongs to no object, which makes a pick at a silhouette select something that is not there.
	pub fn resolve(&self) -> Result<Vec<Vec4>, Error> {
		let mut out = Vec::with_capacity((self.width * self.height) as usize);
		let mut samples = Vec::with_capacity(self.samples as usize);
		for y in 0..self.height {
			for x in 0..self.width {
				samples.clear();
				for sample in 0..self.samples {
					samples.push(self.at(x, y, sample));
				}
				out.push(if self.integer { render3d::msaa::resolve_first(&samples)? } else { render3d::msaa::resolve_colour(&samples)? });
			}
		}
		Ok(out)
	}
}

/// A colour attachment, or one tile of it, as a fragment writes it.
///
/// A TILE'S VIEW REACHES ITS OWN PIXELS AND NO OTHERS. An address outside it reads as zero and
/// writes nowhere - the same answer an address outside the target gets - so a worker handed one
/// tile cannot touch its neighbour's even by a defect in its arithmetic.
pub struct ColourView<'a> {
	layout: Layout,
	integer: bool,
	/// Where `values[0]` is in the whole attachment.
	base: usize,
	values: &'a mut [Vec4],
}

impl ColourView<'_> {
	pub fn samples(&self) -> u32 {
		self.layout.samples
	}

	pub fn integer(&self) -> bool {
		self.integer
	}

	fn slot(&self, x: u32, y: u32, sample: u32) -> Option<usize> {
		self.layout.index(x, y, sample)?.checked_sub(self.base).filter(|index| *index < self.values.len())
	}

	pub fn at(&self, x: u32, y: u32, sample: u32) -> Vec4 {
		self.slot(x, y, sample).map_or(Vec4::ZERO, |index| self.values[index])
	}

	pub fn set(&mut self, x: u32, y: u32, sample: u32, value: Vec4) {
		if let Some(index) = self.slot(x, y, sample) {
			self.values[index] = value;
		}
	}
}

/// The depth and stencil attachment.
#[derive(Clone, PartialEq, Debug)]
pub struct DepthStencil {
	pub width: u32,
	pub height: u32,
	pub samples: u32,
	pub format: DepthFormat,
	depth: Vec<Stored>,
	stencil: Vec<u8>,
}

impl DepthStencil {
	pub fn new(width: u32, height: u32, samples: u32, format: DepthFormat) -> Self {
		let count = (width * height * samples) as usize;
		Self { width, height, samples, format, depth: vec![render3d::depth::clear_depth(format, 1.0); count], stencil: vec![0; count] }
	}

	fn layout(&self) -> Layout {
		Layout { width: self.width, height: self.height, samples: self.samples }
	}

	pub fn depth_at(&self, x: u32, y: u32, sample: u32) -> Stored {
		self.layout().index(x, y, sample).and_then(|index| self.depth.get(index)).copied().unwrap_or(Stored::Float(1.0))
	}

	pub fn stencil_at(&self, x: u32, y: u32, sample: u32) -> u8 {
		self.layout().index(x, y, sample).and_then(|index| self.stencil.get(index)).copied().unwrap_or(0)
	}

	/// The FURTHEST depth stored anywhere in a rectangle, which is the hierarchical bound a tile can
	/// be rejected against.
	///
	/// READ FROM THE BUFFER AND NOT INFERRED FROM WHAT WAS DRAWN. A bound built from the triangles
	/// that covered a tile is only valid when one of them covered the WHOLE tile, which after
	/// clipping almost never happens - a clipped polygon is fan-triangulated and each piece covers
	/// part of it. Reading the buffer is exact, costs one pass over the tile, and cannot be wrong.
	pub fn furthest_in(&self, x0: u32, y0: u32, x1: u32, y1: u32) -> f32 {
		let mut furthest = 0.0_f32;
		for y in y0..y1.min(self.height) {
			for x in x0..x1.min(self.width) {
				for sample in 0..self.samples {
					let value = render3d::depth::readback(self.format, self.depth_at(x, y, sample));
					if value > furthest {
						furthest = value;
					}
				}
			}
		}
		furthest
	}

	pub fn clear(&mut self, depth: f32, stencil: u8) {
		let stored = render3d::depth::clear_depth(self.format, depth);
		for slot in &mut self.depth {
			*slot = stored;
		}
		for slot in &mut self.stencil {
			*slot = stencil;
		}
	}

	/// The whole attachment, as something a fragment is tested against and written into.
	pub fn view(&mut self) -> DepthView<'_> {
		DepthView { layout: self.layout(), format: self.format, base: 0, depth: &mut self.depth, stencil: &mut self.stencil }
	}

	/// One view per tile, in row-major tile order.
	pub fn tiles(&mut self) -> impl Iterator<Item = DepthView<'_>> {
		let (layout, format) = (self.layout(), self.format);
		split(&mut self.depth, layout).zip(split(&mut self.stencil, layout)).map(move |((base, depth), (_, stencil))| DepthView { layout, format, base, depth, stencil })
	}
}

/// The depth and stencil attachment, or one tile of it, as a fragment tests and writes it.
pub struct DepthView<'a> {
	layout: Layout,
	format: DepthFormat,
	base: usize,
	depth: &'a mut [Stored],
	stencil: &'a mut [u8],
}

impl DepthView<'_> {
	pub fn format(&self) -> DepthFormat {
		self.format
	}

	fn slot(&self, x: u32, y: u32, sample: u32) -> Option<usize> {
		self.layout.index(x, y, sample)?.checked_sub(self.base).filter(|index| *index < self.depth.len())
	}

	pub fn depth_at(&self, x: u32, y: u32, sample: u32) -> Stored {
		self.slot(x, y, sample).map_or(Stored::Float(1.0), |index| self.depth[index])
	}

	pub fn stencil_at(&self, x: u32, y: u32, sample: u32) -> u8 {
		self.slot(x, y, sample).map_or(0, |index| self.stencil[index])
	}

	/// `DepthStencil::furthest_in`, over what this view reaches.
	pub fn furthest_in(&self, x0: u32, y0: u32, x1: u32, y1: u32) -> f32 {
		let mut furthest = 0.0_f32;
		for y in y0..y1.min(self.layout.height) {
			for x in x0..x1.min(self.layout.width) {
				for sample in 0..self.layout.samples {
					let value = render3d::depth::readback(self.format, self.depth_at(x, y, sample));
					if value > furthest {
						furthest = value;
					}
				}
			}
		}
		furthest
	}
}

/// What a pass does with one attachment at each end.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Operations {
	pub load: LoadOp,
	pub store: StoreOp,
	/// The value `LoadOp::Clear` writes.
	pub clear: Vec4,
}

impl Operations {
	pub const CLEAR_BLACK: Self = Self { load: LoadOp::Clear, store: StoreOp::Store, clear: Vec4 { x: 0.0, y: 0.0, z: 0.0, w: 1.0 } };
}

/// Apply a load operation to a colour attachment.
pub fn load_colour(attachment: &mut Colour, operations: &Operations) {
	match operations.load {
		LoadOp::Load => {}
		LoadOp::Clear => attachment.fill(operations.clear),
		// SEE THE HEADER: a discard is poisoned rather than left, so reading it is obvious.
		LoadOp::Discard => attachment.fill(POISON),
	}
}

/// Apply a store operation, answering whether the contents may be read afterwards.
///
/// A LATER READ OF A DISCARDED ATTACHMENT IS A TYPED ERROR AND NOT A VALUE, which is what makes
/// `Discard` different from `Clear`: a clear has a value and a discard has none.
pub fn store_colour(attachment: &mut Colour, operations: &Operations) -> bool {
	match operations.store {
		StoreOp::Store => true,
		StoreOp::Discard => {
			attachment.fill(POISON);
			false
		}
	}
}

/// Everything the fragment path needs about one pixel's state.
pub struct Fragment<'a> {
	pub x: u32,
	pub y: u32,
	/// Which samples the rasteriser covered.
	pub coverage: u32,
	pub depth: f32,
	pub colour: Vec4,
	pub blend: &'a AttachmentBlend,
	pub depth_compare: CompareOp,
	pub depth_write: bool,
	pub stencil: Option<(&'a render3d::depth::StencilFace, bool)>,
	/// The pipeline's static sample mask.
	pub sample_mask: u32,
	/// Whether alpha-to-coverage is on, which narrows coverage from the fragment's own alpha.
	pub alpha_to_coverage: bool,
}

/// Write one fragment to one colour attachment and the depth-stencil one.
///
/// THE ORDER IS THE FROZEN ONE. Running the depth test before the stencil one writes a different
/// stencil for every fragment the two disagree about, which is most of the fragments a stencil is
/// used for.
pub fn write_fragment(colour: &mut Colour, depth_stencil: Option<&mut DepthStencil>, fragment: &Fragment<'_>) -> Result<u32, Error> {
	let mut depth_stencil = depth_stencil.map(|buffer| buffer.view());
	write_fragment_into(&mut colour.view(), depth_stencil.as_mut(), fragment)
}

/// `write_fragment` into views: the whole of each attachment, or one tile of each.
pub fn write_fragment_into(colour: &mut ColourView<'_>, depth_stencil: Option<&mut DepthView<'_>>, fragment: &Fragment<'_>) -> Result<u32, Error> {
	let alpha_mask = if fragment.alpha_to_coverage { Some(render3d::msaa::alpha_to_coverage(fragment.colour.w, colour.samples())?) } else { None };
	let coverage = render3d::msaa::coverage_after_masks(fragment.coverage, fragment.sample_mask, alpha_mask);
	if coverage == 0 {
		return Ok(0);
	}
	let mut written = 0;
	let mut depth_stencil = depth_stencil;
	for sample in 0..colour.samples() {
		if coverage & (1 << sample) == 0 {
			continue;
		}
		let passed = match depth_stencil.as_deref_mut() {
			Some(buffer) => {
				let stored = buffer.depth_at(fragment.x, fragment.y, sample);
				let stencil = fragment.stencil.map(|(face, _)| (face, buffer.stencil_at(fragment.x, fragment.y, sample)));
				let outcome = render3d::depth::test(buffer.format, fragment.depth_compare, fragment.depth_write, fragment.depth, stored, stencil);
				if let Some(index) = buffer.slot(fragment.x, fragment.y, sample) {
					if let Some(value) = outcome.depth {
						buffer.depth[index] = value;
					}
					if let Some(value) = outcome.stencil {
						buffer.stencil[index] = value;
					}
				}
				outcome.passed
			}
			None => true,
		};
		if !passed {
			continue;
		}
		let destination = colour.at(fragment.x, fragment.y, sample);
		// AN INTEGER ATTACHMENT CANNOT BLEND, and the profile says so by name rather than by
		// silently ignoring the state: an identity averaged with another identity is neither.
		let value = if colour.integer() {
			if fragment.blend.enabled {
				return Err(Error::UnsupportedFormat { format: "an integer attachment", used_as: "a blend target" });
			}
			fragment.colour
		} else {
			blend(fragment.blend, fragment.colour, destination, Vec4::ZERO)
		};
		// THE WRITE MASK IS APPLIED AFTER BLENDING, so a masked channel keeps the destination's
		// value rather than blending into it.
		let mask = fragment.blend.write_mask;
		let masked = Vec4::new(if mask.red { value.x } else { destination.x }, if mask.green { value.y } else { destination.y }, if mask.blue { value.z } else { destination.z }, if mask.alpha { value.w } else { destination.w });
		colour.set(fragment.x, fragment.y, sample, masked);
		written += 1;
	}
	Ok(written)
}
