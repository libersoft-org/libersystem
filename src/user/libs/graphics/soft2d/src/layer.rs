//! LAYERS, GROUP OPACITY, AND THE POOL THAT MAKES THEM FREE OF ALLOCATION.
//!
//! GROUP OPACITY CANNOT BE DONE BY MULTIPLYING EACH DRAWING'S ALPHA. Two overlapping shapes at half
//! opacity each show the seam between them; the same two shapes in a layer at half opacity do not.
//! That is what an offscreen layer is FOR, and it is why a layer is a surface rather than a state
//! flag.
//!
//! THE SURFACES COME FROM A POOL RESERVED IN `prepare`. Taking one out and giving it back is a move
//! rather than an allocation, so a frame that repeats with the same list genuinely allocates nothing -
//! which is the promise the two phases exist to keep.

use alloc::vec::Vec;

use graphics_core::ColorSpace;
use graphics_core::geom::{PixelRect, RectF};
use render2d::Error;
use render2d::blend::{BlendMode, Operator};
use render2d::resource::FilterHandle;

use crate::target::Surface;

/// Surfaces of one size, handed out and taken back.
#[derive(Default)]
pub struct Pool {
	free: Vec<Surface>,
	/// The size every surface in the pool has, which is the largest any layer or filter node needs.
	extent: (u32, u32),
}

impl Pool {
	pub fn new() -> Self {
		Self::default()
	}

	/// Reserve `count` surfaces of `extent`. CALLED IN `prepare`.
	///
	/// EVERY SURFACE IS THE SAME SIZE, which wastes a little storage on a small layer and buys the
	/// property that matters: any surface fits any need, so taking one is a pop rather than a search
	/// for one that is big enough.
	pub fn reserve(&mut self, count: usize, extent: (u32, u32), space: ColorSpace) -> Result<(), Error> {
		if self.extent != extent {
			self.free.clear();
			self.extent = extent;
		}
		while self.free.len() < count {
			self.free.push(Surface::new(PixelRect::new(0, 0, extent.0.max(1), extent.1.max(1)), space)?);
		}
		Ok(())
	}

	/// Take a cleared surface aimed at `bounds`, or `None` when the reservation is exhausted - which
	/// is a refusal the caller turns into a typed error rather than an allocation.
	pub fn take(&mut self, bounds: PixelRect) -> Option<Surface> {
		let mut surface = self.free.pop()?;
		surface.rebase((bounds.x, bounds.y));
		surface.clear();
		Some(surface)
	}

	pub fn give(&mut self, surface: Surface) {
		self.free.push(surface);
	}

	pub fn available(&self) -> usize {
		self.free.len()
	}

	pub fn scratch_bytes(&self) -> u64 {
		self.free.iter().map(|surface| surface.scratch_bytes()).sum()
	}
}

/// One open layer: what it draws into, and how it composites when it closes.
pub struct Layer {
	pub surface: Surface,
	pub bounds: PixelRect,
	pub opacity: f32,
	pub blend: BlendMode,
	pub operator: Operator,
	pub filter: Option<FilterHandle>,
	/// How deep the clip stack was when the layer opened, so closing it cannot leave a clip behind.
	pub clip_depth: usize,
}

/// The bounds a layer occupies inside a tile.
///
/// A LAYER WITHOUT STATED BOUNDS IS THE WHOLE TILE, which is the conservative answer: a layer whose
/// contents reach further than the caller said would otherwise be silently cut, and a caller that
/// knows its bounds passes them and pays less.
pub fn layer_bounds(bounds: Option<RectF>, tile: PixelRect, expansion: u32) -> PixelRect {
	let expanded = PixelRect::new(tile.x.saturating_sub(expansion), tile.y.saturating_sub(expansion), tile.width.saturating_add(expansion.saturating_mul(2)), tile.height.saturating_add(expansion.saturating_mul(2)));
	match bounds {
		None => expanded,
		Some(rect) => {
			let device = crate::tile::cover(rect);
			expanded.intersection(&device)
		}
	}
}
