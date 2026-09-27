//! WHO REPLAYS A FRAME'S UNITS, AND HOW MANY AT ONCE.
//!
//! THE PARALLELISM COMES FROM OUTSIDE THIS CRATE, which has no threads and no clock and runs in
//! processes that may have neither: a pool is the caller's, and this is the whole of what the crate
//! asks of one - the shape `soft3d::frame::Workers` has, so an application holds ONE pool for both
//! backends. A frame is cut into UNITS - disjoint parts of the target, each replayed in the serial
//! order of its own tiles - and a LANE per worker holds every piece of mutable scratch a unit writes.
//! So a pool shares nothing mutable, and its scheduling cannot change a pixel: inside a unit the
//! order is the serial one, and units do not meet.
//!
//! A POOL MUST call `work` once for every unit, with each lane used by one worker at a time, and
//! return only when every call has returned. `lanes()` is how many lanes the pool offers, at least
//! one; the frame may run on fewer, because every lane is scratch charged against the profile's
//! prepared-scratch ceiling. A pool that returns early is refused by the frame rather than trusted,
//! and a unit handed out twice is replayed once.

use alloc::vec::Vec;

use graphics_core::geom::PixelRect;
use graphics_core::layout::ImageLayout;
use graphics_core::{ImageView, ImageViewMut};
use render2d::Error;

use crate::clip::{ClipStack, MaskPool};
use crate::layer::{Layer, Pool};
use crate::raster::Rasteriser;
use crate::target::{Surface, Tile};

pub trait Workers {
	fn lanes(&self) -> usize;
	fn run<'u>(&self, lanes: &mut [Lane], units: &mut [Unit<'u>], work: &(dyn Fn(&mut Lane, &mut Unit<'u>) + Sync));
}

/// The caller's own thread, one unit after another in order: the SCALAR REFERENCE every other pool
/// is differential-tested against, and what a backend nobody gave a pool uses.
pub struct Serial;

impl Workers for Serial {
	fn lanes(&self) -> usize {
		1
	}

	fn run<'u>(&self, lanes: &mut [Lane], units: &mut [Unit<'u>], work: &(dyn Fn(&mut Lane, &mut Unit<'u>) + Sync)) {
		let Some(lane) = lanes.first_mut() else { return };
		for unit in units {
			work(lane, unit);
		}
	}
}

/// One worker's scratch: EVERYTHING A UNIT WRITES. The tile surface, the rasteriser, the surface pool
/// for layers and filters, the mask pool, the span buffers, the clip stack, the layer stack and a
/// filtered layer's node table - each reserved at `prepare` for the list it will replay, so no lane
/// grows whichever units it is handed.
pub struct Lane {
	pub(crate) raster: Rasteriser,
	pub(crate) pool: Pool,
	pub(crate) masks: MaskPool,
	pub(crate) spans: crate::backend::Spans,
	pub(crate) tile: Tile,
	pub(crate) clips: ClipStack,
	pub(crate) layers: Vec<Layer>,
	pub(crate) nodes: Vec<Option<Surface>>,
	/// The first unit this lane failed in, and why - the frame reports the lowest across lanes, which
	/// is the one the serial walk would have met first.
	pub(crate) failure: Option<(usize, Error)>,
}

impl Default for Lane {
	fn default() -> Self {
		Self { raster: Rasteriser::new(), pool: Pool::new(), masks: MaskPool::new(), spans: crate::backend::Spans::default(), tile: Tile::new(crate::TILE_SIZE), clips: ClipStack::new(), layers: Vec::new(), nodes: Vec::new(), failure: None }
	}
}

impl Lane {
	/// What this lane's scratch costs, which is what a second lane is charged against the ceiling.
	pub(crate) fn scratch_bytes(&self) -> u64 {
		self.pool.scratch_bytes() + self.masks.scratch_bytes() + self.raster.scratch_bytes() + self.spans.scratch_bytes() + self.tile.scratch_bytes() + self.clips.reserved_bytes() + (self.layers.capacity() * core::mem::size_of::<Layer>()) as u64 + (self.nodes.capacity() * core::mem::size_of::<Option<Surface>>()) as u64
	}
}

/// How a frame is cut into units - `Rectangles` unless a caller asks otherwise, because it was measured
/// fastest of the three; the other two are what it was measured against. A frame on one lane is cut into
/// bands whichever is asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnitKind {
	/// A BAND OF TILE ROWS: one contiguous slice of the target, every tile of one tile row. Safe to hand
	/// out whole because rows are what the target is laid out by - and capped at one unit per sixty-four
	/// rows, which is eight units for a 480-row frame.
	Bands,
	/// ONE TILE, through a TILE-MAJOR INTERMEDIATE: the unit reads the target (which nothing writes while
	/// the units run) and writes its encoded pixels into a slot of its own, and the slots are copied into
	/// the target once every unit has returned. As many units as the frame has tiles that draw, for the
	/// cost of the intermediate's memory and one copy of what was drawn.
	Tiles,
	/// ONE TILE, IN PLACE, through a DISJOINT-RECTANGLE WRITER: every row of the target is cut into the
	/// parts its tiles cover, and a unit holds the parts of its own tile's rows - so it reads and writes
	/// its rectangle of the target and no byte of any other. As many units as tiles, with no intermediate
	/// and no copy, for the cost of the cut: once per row of the frame, and a table of row parts per unit.
	Rectangles,
}

/// One unit of a frame: the tiles it replays, in serial order, and its part of the target.
pub struct Unit<'u> {
	/// Its place in the serial order, which is the order the serial walk meets it in and the order a
	/// failure is reported by.
	pub(crate) index: usize,
	/// The tiles it replays: a contiguous run of tile indices.
	pub(crate) first_tile: u32,
	pub(crate) tiles: u32,
	pub(crate) access: Access<'u>,
	/// Replayed, whether or not it succeeded - a pool that hands it out twice finds it done.
	pub(crate) ran: bool,
	/// Replayed to its end, so its pixels are whole: what a cancelled frame keeps.
	pub(crate) whole: bool,
}

impl Unit<'_> {
	/// Its place in the serial order, for a pool that wants to schedule by position.
	pub fn index(&self) -> usize {
		self.index
	}
}

/// A unit's part of the target.
///
/// THE RECTANGLE'S ROW PARTS ARE HELD INLINE, which makes every unit the size of the largest variant: a
/// table of them is kept from one frame to the next, and parts held anywhere else would be storage a
/// frame asks for.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Access<'u> {
	/// Every row of the unit's band and nothing else, as a view of its own whose row zero is the band's
	/// first row `top`.
	Band { view: ImageViewMut<'u>, top: u32 },
	/// The whole target, READ ONLY, and the unit's slot of the intermediate. `tile` is the rectangle the
	/// slot holds, and `pitch` its row length in bytes.
	Slot { source: &'u ImageView<'u>, slot: &'u mut [u8], tile: PixelRect, pitch: usize },
	/// The unit's tile of the target, row by row: `rows[n]` is the part of target row `tile.y + n` the tile
	/// covers, split off that row, and `layout` is the target's.
	Rect { rows: [&'u mut [u8]; crate::TILE_SIZE as usize], tile: PixelRect, layout: ImageLayout },
}
