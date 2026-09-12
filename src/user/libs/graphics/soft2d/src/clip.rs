//! CLIPPING AS A COVERAGE MASK and not as a rectangle test.
//!
//! A RECTANGLE TEST HAS NO ANSWER FOR A ROUNDED CLIP. Every API that started with one grew a second
//! path for rounded rectangles, a third for paths, and a fourth for nesting them - and the four
//! disagree about the antialiased edge, which is exactly where a clip is visible. One mask costs the
//! same machinery for all of them, and nesting is the product of two masks.
//!
//! THE AXIS-ALIGNED RECTANGLE KEEPS A FAST PATH, because it is most of the clips in a user interface
//! and it needs no storage at all: a rectangle intersects with another rectangle, and only when a
//! non-rectangular clip arrives does a mask have to exist.
//!
//! AN INVERSE CLIP IS ONE SUBTRACTION. `1 - coverage` over the same mask, which is what makes
//! "everything except this shape" cost what "this shape" costs.

use alloc::vec::Vec;

use graphics_core::geom::PixelRect;

/// One level of the clip stack.
///
/// A LEVEL IS EITHER A RECTANGLE OR A MASK, and the rectangle is not an optimisation of the mask: a
/// rectangular clip that allocated a full-tile mask would make every scrolling list in a user
/// interface pay for storage it never reads.
pub struct ClipLevel {
	bounds: PixelRect,
	mask: Option<Vec<u8>>,
	/// The mask's row stride, which is the bounds' width - kept explicitly so a caller cannot index
	/// with a tile's width by accident.
	stride: usize,
}

impl ClipLevel {
	pub fn rectangle(bounds: PixelRect) -> Self {
		Self { bounds, mask: None, stride: bounds.width as usize }
	}

	pub fn with_mask(bounds: PixelRect, mask: Vec<u8>) -> Self {
		let stride = bounds.width as usize;
		Self { bounds, mask: Some(mask), stride }
	}

	pub fn bounds(&self) -> PixelRect {
		self.bounds
	}

	/// The coverage this level allows at a target pixel, in `0..=1`.
	pub fn coverage(&self, x: u32, y: u32) -> f32 {
		let (Some(local_x), Some(local_y)) = (x.checked_sub(self.bounds.x), y.checked_sub(self.bounds.y)) else { return 0.0 };
		if local_x >= self.bounds.width || local_y >= self.bounds.height {
			return 0.0;
		}
		match &self.mask {
			None => 1.0,
			Some(mask) => {
				let index = local_y as usize * self.stride + local_x as usize;
				mask.get(index).map(|value| *value as f32 / 255.0).unwrap_or(0.0)
			}
		}
	}

	pub fn scratch_bytes(&self) -> u64 {
		self.mask.as_ref().map(|mask| mask.len() as u64).unwrap_or(0)
	}
}

/// The stack. CLIPS INTERSECT AND NEVER REPLACE: a clip that replaced would let a child draw outside
/// its parent, which is the one thing a clip exists to prevent.
#[derive(Default)]
pub struct ClipStack {
	levels: Vec<ClipLevel>,
	/// The intersection of every level's bounds, kept as it changes so a draw does not walk the stack
	/// to find out whether it is entirely outside.
	bounds: Vec<PixelRect>,
}

impl ClipStack {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn reset(&mut self, bounds: PixelRect) {
		self.levels.clear();
		self.bounds.clear();
		self.bounds.push(bounds);
	}

	pub fn bounds(&self) -> PixelRect {
		self.bounds.last().copied().unwrap_or(PixelRect::new(0, 0, 0, 0))
	}

	pub fn depth(&self) -> usize {
		self.levels.len()
	}

	pub fn push(&mut self, level: ClipLevel) {
		let intersected = self.bounds().intersection(&level.bounds());
		self.levels.push(level);
		self.bounds.push(intersected);
	}

	/// Pop, returning the mask's storage to the pool it came from.
	pub fn pop(&mut self, pool: &mut MaskPool) {
		if let Some(level) = self.levels.pop()
			&& let Some(mask) = level.mask
		{
			pool.give(mask);
		}
		if self.bounds.len() > 1 {
			self.bounds.pop();
		}
	}

	/// Return every level's storage, which is what ends a tile.
	pub fn drain_into(&mut self, pool: &mut MaskPool) {
		while !self.levels.is_empty() {
			self.pop(pool);
		}
	}

	/// The coverage every level allows at a pixel: the PRODUCT, which is what nesting means.
	///
	/// THE PRODUCT AND NOT THE MINIMUM. Two antialiased edges crossing at a corner let through the
	/// product of their coverages; taking the minimum makes the corner of a rounded clip inside
	/// another rounded clip too dark, which is visible as a bright notch.
	pub fn coverage(&self, x: u32, y: u32) -> f32 {
		let mut coverage = 1.0f32;
		for level in &self.levels {
			coverage *= level.coverage(x, y);
			if coverage <= 0.0 {
				return 0.0;
			}
		}
		coverage
	}

	/// Whether every level is a plain rectangle, which is the case a span can skip the per-pixel
	/// multiply for entirely.
	pub fn is_rectangular(&self) -> bool {
		self.levels.iter().all(|level| level.mask.is_none())
	}

	pub fn scratch_bytes(&self) -> u64 {
		self.levels.iter().map(|level| level.scratch_bytes()).sum()
	}
}

/// Mask storage, taken and given back rather than allocated per clip.
///
/// A CLIP PUSH INSIDE A TILE LOOP IS A PER-FRAME ALLOCATION otherwise, and "allocates nothing" is
/// the promise the two phases exist to keep. Every mask is the same size, so taking one is a pop.
#[derive(Default)]
pub struct MaskPool {
	free: Vec<Vec<u8>>,
	size: usize,
}

impl MaskPool {
	pub fn new() -> Self {
		Self::default()
	}

	/// Reserve `count` masks of `size` bytes. CALLED IN `prepare`.
	pub fn reserve(&mut self, count: usize, size: usize) {
		if self.size != size {
			self.free.clear();
			self.size = size;
		}
		while self.free.len() < count {
			self.free.push(alloc::vec![0u8; size]);
		}
	}

	pub fn take(&mut self) -> Option<Vec<u8>> {
		self.take_filled(0)
	}

	/// Take a mask filled with a value. AN INVERSE CLIP STARTS AT FULLY VISIBLE and has its shape
	/// subtracted, so the rows its path never reaches have to already be open - starting at zero and
	/// inverting only the touched rows is the version that clips away everything the shape misses.
	pub fn take_filled(&mut self, value: u8) -> Option<Vec<u8>> {
		let mut mask = self.free.pop()?;
		mask.fill(value);
		Some(mask)
	}

	pub fn give(&mut self, mask: Vec<u8>) {
		if mask.len() == self.size {
			self.free.push(mask);
		}
	}

	pub fn scratch_bytes(&self) -> u64 {
		self.free.iter().map(|mask| mask.len() as u64).sum()
	}
}

/// Turn a coverage row into a mask row, INVERTING it where the clip is an inverse one.
pub fn write_mask_row(mask: &mut [u8], coverage: &[f32], inverse: bool) {
	for (slot, value) in mask.iter_mut().zip(coverage.iter()) {
		let clamped = value.clamp(0.0, 1.0);
		let quantised = graphics_core::pixel::quantise(clamped, 255.0) as u8;
		*slot = if inverse { 255 - quantised } else { quantised };
	}
}
