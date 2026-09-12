//! TILES AND THE BINNING THAT MAKES THEM WORTH HAVING.
//!
//! A TILE-LESS SOFTWARE RENDERER TOUCHES THE WHOLE SURFACE FOR EVERY COMMAND. A drawing of a thousand
//! small shapes then walks a four-megabyte target a thousand times, and the working set is the
//! surface rather than the cache. Binning each command into the tiles it can touch turns that into a
//! thousand shapes each visiting the handful of tiles it is actually in.
//!
//! AND IT IS WHAT MAKES THE SCRATCH BOUNDED. A layer, a clip mask and a filter intermediate are sized
//! to a tile plus what a filter reaches past it, rather than to the surface - so nesting three
//! layers over a four-thousand-pixel-wide drawing costs three tiles of storage.
//!
//! THE BINS ARE COMPUTED IN `prepare` AND READ IN `render`, which is the same split as everything
//! else here: the bounds of a command are a function of the list and the target, so they are worked
//! out when those are known and not once per frame.

use alloc::vec::Vec;

use graphics_core::geom::{Extent2D, PixelRect, RectF};

/// The tiles of a target, in a fixed order.
///
/// ROW-MAJOR AND NOT AN ITERATION ORDER SOMETHING ELSE CHOSE, because the profile requires a given
/// input to produce a given output and a renderer whose tile order varied would produce the same
/// pixels by a different path - which is only indistinguishable until something depends on the order,
/// such as a cancellation that stops halfway.
pub struct Tiling {
	pub extent: Extent2D,
	pub size: u32,
	pub columns: u32,
	pub rows: u32,
}

impl Tiling {
	pub fn new(extent: Extent2D, size: u32) -> Self {
		let size = size.max(1);
		Self { extent, size, columns: extent.width.div_ceil(size), rows: extent.height.div_ceil(size) }
	}

	pub fn count(&self) -> usize {
		self.columns as usize * self.rows as usize
	}

	pub fn tile(&self, index: usize) -> PixelRect {
		let column = (index % self.columns.max(1) as usize) as u32;
		let row = (index / self.columns.max(1) as usize) as u32;
		let x = column * self.size;
		let y = row * self.size;
		PixelRect::new(x, y, self.size.min(self.extent.width.saturating_sub(x)), self.size.min(self.extent.height.saturating_sub(y)))
	}

	/// Which tiles a device rectangle touches.
	pub fn range(&self, bounds: PixelRect) -> (u32, u32, u32, u32) {
		if bounds.is_empty() {
			return (0, 0, 0, 0);
		}
		let first_column = bounds.x / self.size;
		let first_row = bounds.y / self.size;
		let last_column = (bounds.x.saturating_add(bounds.width).saturating_sub(1)) / self.size;
		let last_row = (bounds.y.saturating_add(bounds.height).saturating_sub(1)) / self.size;
		(first_column, first_row, last_column.min(self.columns.saturating_sub(1)), last_row.min(self.rows.saturating_sub(1)))
	}
}

/// Which commands each tile has to replay.
///
/// EVERY COMMAND THAT CHANGES STATE IS IN EVERY TILE. A clip push, a clip pop, a layer begin and a
/// layer end are not drawings - skipping one in a tile it does not draw in would leave that tile
/// replaying the rest of the list under the wrong clip, which is the bug that looks like a random
/// rectangle of missing content.
#[derive(Default)]
pub struct Bins {
	/// One list of command indices per tile.
	lists: Vec<Vec<u32>>,
}

impl Bins {
	pub fn build(tiling: &Tiling, bounds: &[Option<PixelRect>]) -> Self {
		let mut lists: Vec<Vec<u32>> = Vec::with_capacity(tiling.count());
		lists.resize_with(tiling.count(), Vec::new);
		for (index, bound) in bounds.iter().enumerate() {
			match bound {
				// A STATE COMMAND HAS NO BOUNDS AND GOES EVERYWHERE.
				None => {
					for list in lists.iter_mut() {
						list.push(index as u32);
					}
				}
				Some(bound) => {
					let (first_column, first_row, last_column, last_row) = tiling.range(*bound);
					if bound.is_empty() {
						continue;
					}
					for row in first_row..=last_row {
						for column in first_column..=last_column {
							let slot = row as usize * tiling.columns.max(1) as usize + column as usize;
							if let Some(list) = lists.get_mut(slot) {
								list.push(index as u32);
							}
						}
					}
				}
			}
		}
		Self { lists }
	}

	pub fn commands(&self, tile: usize) -> &[u32] {
		self.lists.get(tile).map(|list| list.as_slice()).unwrap_or(&[])
	}

	pub fn scratch_bytes(&self) -> u64 {
		self.lists.iter().map(|list| (list.capacity() * core::mem::size_of::<u32>()) as u64).sum()
	}
}

/// The device pixels a float rectangle can touch: OUTWARD on every side.
///
/// CONSERVATIVE AND NOT EXACT, which is the rule for every bound in this backend: a bound that is one
/// pixel too small clips the drawing it was computed for, and one that is a pixel too large costs a
/// pixel.
pub fn cover(rect: RectF) -> PixelRect {
	if !(rect.x.is_finite() && rect.y.is_finite() && rect.width.is_finite() && rect.height.is_finite()) {
		return PixelRect::new(0, 0, 0, 0);
	}
	let left = libm::floorf(rect.x).max(0.0);
	let top = libm::floorf(rect.y).max(0.0);
	let right = libm::ceilf(rect.right()).max(0.0);
	let bottom = libm::ceilf(rect.bottom()).max(0.0);
	if right <= left || bottom <= top {
		return PixelRect::new(0, 0, 0, 0);
	}
	let clamp = |value: f32| value.min(u32::MAX as f32) as u32;
	PixelRect::new(clamp(left), clamp(top), clamp(right - left), clamp(bottom - top))
}
