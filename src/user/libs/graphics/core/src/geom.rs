//! TWO COORDINATE SPACES, and which is which.
//!
//! A PLATFORM WITH ONLY INTEGER PIXEL COORDINATES CANNOT DO HiDPI, fractional scaling, zoom, rotation
//! or smooth animation - and those are not desktop luxuries, they are what a phone does on every
//! orientation change. So the PUBLIC drawing coordinate is `f32` and SIGNED: geometry has to be
//! positionable between pixels and has to clip off the left and top edges as naturally as off the
//! right and bottom, and an unsigned integer parameter makes "half a pixel to the left of the origin"
//! unrepresentable.
//!
//! AND THEY ARE TWO TYPES, because one rectangle cannot serve both and the version that tried was
//! wrong at one end or the other. A damage rectangle crossing an interface boundary must be whole
//! PHYSICAL pixels; a drawing rectangle must be fractional and signed. A wire format with an `f32`
//! rectangle whose meaning depends on a transform stack the receiver cannot see is not a wire format.
//!
//! BOTH KINDS ARE HALF-OPEN, `[x, x + width) x [y, y + height)`, so adjacent rectangles tile without
//! overlapping and an empty rectangle is one with a zero width or height. Closed rectangles are why
//! two adjacent damage regions redraw a shared column twice, which is invisible until it flickers.

/// A point in the DRAWING space: fractional and signed.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct PointF {
	pub x: f32,
	pub y: f32,
}

/// A rectangle in the drawing space. HALF-OPEN, fractional and signed.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct RectF {
	pub x: f32,
	pub y: f32,
	pub width: f32,
	pub height: f32,
}

impl RectF {
	pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
		Self { x, y, width, height }
	}

	/// A rectangle with no area.
	///
	/// A NaN EXTENT IS EMPTY, and saying that needs the comparison written out: every comparison with
	/// NaN is false, so a plain `width <= 0.0` answers `false` for a NaN width - which makes a
	/// rectangle nobody can place count as something to draw.
	pub fn is_empty(&self) -> bool {
		let positive = |value: f32| matches!(value.partial_cmp(&0.0), Some(core::cmp::Ordering::Greater));
		!positive(self.width) || !positive(self.height)
	}

	pub fn right(&self) -> f32 {
		self.x + self.width
	}

	pub fn bottom(&self) -> f32 {
		self.y + self.height
	}

	/// HALF-OPEN CONTAINMENT: a point exactly on the right or bottom edge is OUTSIDE, which is what
	/// makes adjacent rectangles tile rather than overlap.
	pub fn contains(&self, point: PointF) -> bool {
		point.x >= self.x && point.x < self.right() && point.y >= self.y && point.y < self.bottom()
	}

	/// The overlap, or an empty rectangle. Never a negative extent.
	pub fn intersection(&self, other: &RectF) -> RectF {
		let x = self.x.max(other.x);
		let y = self.y.max(other.y);
		let right = self.right().min(other.right());
		let bottom = self.bottom().min(other.bottom());
		RectF { x, y, width: (right - x).max(0.0), height: (bottom - y).max(0.0) }
	}
}

/// An offset in PHYSICAL pixels, which may be negative - a surface scrolled up is at a negative y.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PixelOffset {
	pub x: i32,
	pub y: i32,
}

/// A size in physical pixels. Never negative, and never on the wire as anything else.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Extent2D {
	pub width: u32,
	pub height: u32,
}

impl Extent2D {
	pub const fn new(width: u32, height: u32) -> Self {
		Self { width, height }
	}

	pub const fn is_empty(&self) -> bool {
		self.width == 0 || self.height == 0
	}

	/// The pixel count, CHECKED: a width times a height is exactly the product that overflows on a
	/// crafted layout.
	pub fn pixels(&self) -> Option<u64> {
		(self.width as u64).checked_mul(self.height as u64)
	}
}

/// A rectangle in PHYSICAL pixels: what damage, a post-rasterisation scissor, a surface extent and
/// everything on the wire is expressed in. HALF-OPEN.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PixelRect {
	pub x: u32,
	pub y: u32,
	pub width: u32,
	pub height: u32,
}

impl PixelRect {
	pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
		Self { x, y, width, height }
	}

	pub const fn is_empty(&self) -> bool {
		self.width == 0 || self.height == 0
	}

	/// The exclusive right edge, CHECKED - a rectangle whose right edge overflows is a rectangle a
	/// receiver must refuse rather than wrap.
	pub fn right(&self) -> Option<u32> {
		self.x.checked_add(self.width)
	}

	pub fn bottom(&self) -> Option<u32> {
		self.y.checked_add(self.height)
	}

	/// Is this rectangle wholly inside an extent? The question every damage rectangle is asked at the
	/// boundary it crosses.
	pub fn fits(&self, extent: Extent2D) -> bool {
		match (self.right(), self.bottom()) {
			(Some(right), Some(bottom)) => right <= extent.width && bottom <= extent.height,
			_ => false,
		}
	}

	/// Is `other` wholly inside this rectangle? An EMPTY `other` is inside anything, which is the
	/// same convention the intersection above keeps: a rectangle with no pixels asks nothing of the
	/// one it is compared against.
	pub fn contains_rect(&self, other: &PixelRect) -> bool {
		if other.is_empty() {
			return true;
		}
		let (Some(right), Some(bottom)) = (self.right(), self.bottom()) else { return false };
		let (Some(other_right), Some(other_bottom)) = (other.right(), other.bottom()) else { return false };
		other.x >= self.x && other.y >= self.y && other_right <= right && other_bottom <= bottom
	}

	pub fn intersection(&self, other: &PixelRect) -> PixelRect {
		let x = self.x.max(other.x);
		let y = self.y.max(other.y);
		let right = self.right().unwrap_or(u32::MAX).min(other.right().unwrap_or(u32::MAX));
		let bottom = self.bottom().unwrap_or(u32::MAX).min(other.bottom().unwrap_or(u32::MAX));
		PixelRect { x, y, width: right.saturating_sub(x), height: bottom.saturating_sub(y) }
	}
}
