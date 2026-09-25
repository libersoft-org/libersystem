//! TRIANGLE SETUP, BINNING AND COVERAGE.
//!
//! A BACK-FACING TRIANGLE IS REWOUND RATHER THAN GIVEN ITS OWN PATH. Reversing two vertices makes
//! the signed area positive, so the edge tests, the fill rule and the barycentric weights are one
//! piece of code instead of two with opposite comparisons - and two comparisons that must be exact
//! mirror images of each other is how a shared edge between a front and a back face gets a seam. The
//! original facing is carried instead, because the fragment stage needs it.
//!
//! A SUB-PIXEL TRIANGLE IS NOT SKIPPED. It either covers a sample point or it does not. Dropping a
//! primitive because it is small makes distant geometry flicker as the camera moves, which looks
//! like a level-of-detail bug and is not one.
//!
//! A DEGENERATE TRIANGLE IS SKIPPED DETERMINISTICALLY - zero area means no sample can be strictly
//! inside all three edges, and the fill rule cannot rescue it because the three edge values are all
//! zero at every sample. It produces nothing, on every architecture, rather than producing a line on
//! one of them.
//!
//! BINNING IS BY TILE AND THE BINS ARE REUSED. A frame allocates its tile storage once and clears it
//! per frame; nothing here allocates per primitive, which is what the no-steady-state-allocation
//! rule means on the 3D side.

use alloc::vec;
use alloc::vec::Vec;

use render_math::{Vec3, Vec4};

use crate::fixed::{self, FixedFault, Subpixel};

/// A tile's side, in pixels. THE BIN LIST IS PER TILE, so this trades memory for how much of a
/// triangle's bounding box is examined: too small and the bin lists dominate, too large and a thin
/// triangle drags a whole tile's worth of samples through the edge test.
pub const TILE: u32 = 32;

/// How a primitive faces the camera.
///
/// DECIDED FROM THE SIGNED AREA IN WINDOW SPACE, which is the same answer `render-math`'s `facing`
/// gives on the NDC positions BEFORE the viewport inverts Y - the inversion reverses apparent
/// winding, so the two conventions agree only because this one is stated against the flipped space.
/// A fixture holds them against each other.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Facing {
	Front,
	Back,
}

/// An integer rectangle of pixels, half-open on the high side.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PixelBox {
	pub x0: i64,
	pub y0: i64,
	pub x1: i64,
	pub y1: i64,
}

impl PixelBox {
	pub const fn is_empty(&self) -> bool {
		self.x1 <= self.x0 || self.y1 <= self.y0
	}

	pub fn clamped(&self, width: u32, height: u32) -> Self {
		Self { x0: self.x0.max(0), y0: self.y0.max(0), x1: self.x1.min(width as i64), y1: self.y1.min(height as i64) }
	}
}

/// A triangle ready to be sampled.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Setup {
	/// The three window-space vertices in subpixel units, REWOUND so the area is positive.
	pub vertex: [(Subpixel, Subpixel); 3],
	/// Which of the three source vertices each rewound one came from, so a caller can read the
	/// varyings back in the order it supplied them.
	pub source: [usize; 3],
	/// Twice the signed area, always positive here.
	pub double_area: i64,
	/// Whether each edge is a top or a left one, for the fill rule.
	pub top_or_left: [bool; 3],
	/// `1/w` per rewound vertex, for perspective-correct interpolation.
	pub inverse_w: [f32; 3],
	/// Window-space depth per rewound vertex, already mapped through the viewport's depth range.
	pub depth: [f32; 3],
	pub bounds: PixelBox,
	pub facing: Facing,
}

/// Why a triangle could not be set up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RasterFault {
	Coordinate(FixedFault),
}

impl From<RasterFault> for render3d::Error {
	/// A COORDINATE THE RASTERISER CANNOT TAKE IS A TRANSFORM PROBLEM, not a mesh one: the vertices
	/// were legal and the matrix put them somewhere the raster grid does not reach, which is what
	/// the caller has to fix.
	fn from(_: RasterFault) -> Self {
		render3d::Error::InvalidTransform
	}
}

/// Set up a triangle from its window-space positions.
///
/// `window` carries `(x, y, depth)` in pixels and viewport depth; `inverse_w` is `1/w` from the
/// perspective divide, which the caller already computed and which interpolation needs.
///
/// `Ok(None)` MEANS DEGENERATE AND NOT AN ERROR. A zero-area triangle is something a legal scene
/// produces - an edge-on quad, a collapsed vertex after skinning - and refusing it would fail a
/// frame for geometry that simply covers nothing.
pub fn setup(window: [Vec3; 3], inverse_w: [f32; 3], width: u32, height: u32) -> Result<Option<Setup>, RasterFault> {
	let mut vertex = [(Subpixel(0), Subpixel(0)); 3];
	for index in 0..3 {
		let x = fixed::quantise(window[index].x).map_err(RasterFault::Coordinate)?;
		let y = fixed::quantise(window[index].y).map_err(RasterFault::Coordinate)?;
		vertex[index] = (x, y);
	}
	let mut source = [0, 1, 2];
	let mut area = fixed::double_area(vertex[0], vertex[1], vertex[2]);
	let facing = if area > 0 {
		Facing::Front
	} else if area < 0 {
		Facing::Back
	} else {
		return Ok(None);
	};
	if area < 0 {
		vertex.swap(1, 2);
		source.swap(1, 2);
		area = -area;
	}
	let inverse_w = [inverse_w[source[0]], inverse_w[source[1]], inverse_w[source[2]]];
	let depth = [window[source[0]].z, window[source[1]].z, window[source[2]].z];
	let top_or_left = [fixed::is_top_or_left(vertex[0], vertex[1]), fixed::is_top_or_left(vertex[1], vertex[2]), fixed::is_top_or_left(vertex[2], vertex[0])];
	let bounds = bounding_box(&vertex).clamped(width, height);
	Ok(Some(Setup { vertex, source, double_area: area, top_or_left, inverse_w, depth, bounds, facing }))
}

/// The pixel bounding box of three subpixel vertices, half-open.
///
/// THE HIGH EDGE IS ROUNDED UP AND THE LOW ONE DOWN, so a triangle that ends part-way through a
/// pixel still has that pixel examined. Rounding the high edge down is the commonest way to lose the
/// last row of a triangle.
fn bounding_box(vertex: &[(Subpixel, Subpixel); 3]) -> PixelBox {
	let xs = [vertex[0].0.0, vertex[1].0.0, vertex[2].0.0];
	let ys = [vertex[0].1.0, vertex[1].1.0, vertex[2].1.0];
	let low = |values: [i64; 3]| values.iter().copied().min().unwrap_or(0) >> fixed::SUBPIXEL_BITS;
	let high = |values: [i64; 3]| {
		let maximum = values.iter().copied().max().unwrap_or(0);
		(maximum + fixed::SUBPIXEL_ONE - 1) >> fixed::SUBPIXEL_BITS
	};
	PixelBox { x0: low(xs), y0: low(ys), x1: high(xs) + 1, y1: high(ys) + 1 }
}

/// Whether this triangle is drawn at all under a cull mode.
pub fn culled(facing: Facing, cull: render3d::Cull) -> bool {
	match cull {
		render3d::Cull::None => false,
		render3d::Cull::Front => facing == Facing::Front,
		render3d::Cull::Back => facing == Facing::Back,
	}
}

/// The three edge values at a sample point.
pub fn edges(setup: &Setup, at: (Subpixel, Subpixel)) -> [i64; 3] {
	[fixed::edge(setup.vertex[0], setup.vertex[1], at), fixed::edge(setup.vertex[1], setup.vertex[2], at), fixed::edge(setup.vertex[2], setup.vertex[0], at)]
}

/// Whether a sample is inside, applying the top-left rule to every tie.
pub fn inside(setup: &Setup, at: (Subpixel, Subpixel)) -> bool {
	let values = edges(setup, at);
	(0..3).all(|index| fixed::covered_by(values[index], setup.top_or_left[index]))
}

/// The sample mask of one pixel: which of the `count` sample points the triangle covers.
///
/// THE SAMPLE POSITIONS ARE THE PROFILE'S, taken from `render3d` rather than restated here - two
/// copies of a sample grid is two grids that can disagree, and the disagreement is an MSAA edge that
/// is right in the reference and wrong in the renderer.
pub fn coverage(setup: &Setup, pixel_x: i64, pixel_y: i64, samples: u32) -> Result<u32, render3d::Error> {
	let positions = render3d::msaa::sample_positions(samples)?;
	let mut mask = 0;
	for position in positions {
		let x = pixel_x * fixed::SUBPIXEL_ONE + (position.x as f64 * fixed::SUBPIXEL_ONE as f64) as i64;
		let y = pixel_y * fixed::SUBPIXEL_ONE + (position.y as f64 * fixed::SUBPIXEL_ONE as f64) as i64;
		if inside(setup, (Subpixel(x), Subpixel(y))) {
			mask |= 1 << position.index;
		}
	}
	Ok(mask)
}

/// The barycentric weights at a sample, from the edge values.
///
/// THE WEIGHTS ARE SCREEN-SPACE AND ARE NOT WHAT A VARYING IS INTERPOLATED WITH. They are the input
/// to the perspective correction in `interp`, and using them directly is exactly the affine-texture
/// warp that made early software renderers famous.
pub fn barycentric(setup: &Setup, at: (Subpixel, Subpixel)) -> [f32; 3] {
	let values = edges(setup, at);
	let area = setup.double_area as f64;
	// `edges[1]` is opposite vertex 0, `edges[2]` opposite vertex 1, `edges[0]` opposite vertex 2.
	[(values[1] as f64 / area) as f32, (values[2] as f64 / area) as f32, (values[0] as f64 / area) as f32]
}

/// The tile grid of one target, and the triangle indices binned into each tile.
///
/// REUSED ACROSS FRAMES. `clear` keeps the allocations and empties the lists, so a steady-state
/// frame allocates nothing here.
pub struct Bins {
	across: u32,
	down: u32,
	tiles: Vec<Vec<u32>>,
	/// THE HIERARCHICAL DEPTH BOUND: the FURTHEST depth anything in this tile currently holds. A
	/// triangle whose nearest vertex is further than this cannot produce a surviving fragment under
	/// a nearer-wins test, so the whole triangle is rejected before a sample is touched.
	///
	/// CONSERVATIVE BY CONSTRUCTION. It only ever moves NEARER as a tile fills in, and a tile that
	/// has drawn nothing holds `1.0` - the far plane - which rejects nothing. Turning it off changes
	/// the frame's speed and not its picture.
	far: Vec<f32>,
}

impl Bins {
	pub fn new(width: u32, height: u32) -> Self {
		let across = width.div_ceil(TILE).max(1);
		let down = height.div_ceil(TILE).max(1);
		Self { across, down, tiles: vec![Vec::new(); (across * down) as usize], far: vec![1.0; (across * down) as usize] }
	}

	/// The furthest depth this tile can still be beaten by.
	pub fn tile_far(&self, x: u32, y: u32) -> f32 {
		self.far.get((y * self.across + x) as usize).copied().unwrap_or(1.0)
	}

	/// Move a tile's bound nearer, which is the only direction it may move within a frame.
	pub fn narrow_tile_far(&mut self, x: u32, y: u32, depth: f32) {
		if let Some(slot) = self.far.get_mut((y * self.across + x) as usize) {
			if depth < *slot {
				*slot = depth;
			}
		}
	}

	pub fn across(&self) -> u32 {
		self.across
	}

	pub fn down(&self) -> u32 {
		self.down
	}

	/// Empty every bin, keeping the memory.
	/// Empty every bin, keeping the memory AND keeping the hierarchical depth bound - which belongs
	/// to the frame and not to the draw: a later draw is exactly what the bound is there to reject.
	pub fn clear(&mut self) {
		for tile in &mut self.tiles {
			tile.clear();
		}
	}

	/// Forget every tile's depth bound. Called once per frame, because the bound is only valid
	/// against a depth buffer that has not been cleared since.
	pub fn reset_depth(&mut self) {
		for bound in &mut self.far {
			*bound = 1.0;
		}
	}

	/// Resize for a new target, reusing what is already allocated where it fits.
	pub fn resize(&mut self, width: u32, height: u32) {
		let across = width.div_ceil(TILE).max(1);
		let down = height.div_ceil(TILE).max(1);
		if across == self.across && down == self.down {
			self.clear();
			return;
		}
		self.across = across;
		self.down = down;
		self.tiles.resize((across * down) as usize, Vec::new());
		self.far.resize((across * down) as usize, 1.0);
		self.clear();
		self.reset_depth();
	}

	/// Put a triangle into every tile its bounding box touches.
	pub fn insert(&mut self, triangle: u32, bounds: &PixelBox) {
		if bounds.is_empty() {
			return;
		}
		let first_x = (bounds.x0.max(0) as u32) / TILE;
		let first_y = (bounds.y0.max(0) as u32) / TILE;
		let last_x = ((bounds.x1 - 1).max(0) as u32 / TILE).min(self.across - 1);
		let last_y = ((bounds.y1 - 1).max(0) as u32 / TILE).min(self.down - 1);
		for tile_y in first_y..=last_y {
			for tile_x in first_x..=last_x {
				self.tiles[(tile_y * self.across + tile_x) as usize].push(triangle);
			}
		}
	}

	/// The bytes this tile grid owns, for the caller to charge against its process Domain.
	pub fn reserved_bytes(&self) -> usize {
		core::mem::size_of::<Vec<u32>>() * self.tiles.capacity() + self.tiles.iter().map(|tile| tile.capacity() * core::mem::size_of::<u32>()).sum::<usize>() + self.far.capacity() * core::mem::size_of::<f32>()
	}

	pub fn tile(&self, x: u32, y: u32) -> &[u32] {
		&self.tiles[(y * self.across + x) as usize]
	}

	/// The pixel rectangle a tile covers, clamped to the target.
	pub fn tile_box(&self, x: u32, y: u32, width: u32, height: u32) -> PixelBox {
		tile_area(x, y, width, height)
	}

	/// Every tile's bin, and every tile's depth bound to narrow - apart, because a frame shading its
	/// tiles at once reads the first while each tile writes its own slot of the second.
	pub(crate) fn parts(&mut self) -> (&[Vec<u32>], &mut [f32]) {
		(&self.tiles, &mut self.far)
	}
}

/// The pixel rectangle a tile covers, clamped to the target.
pub fn tile_area(x: u32, y: u32, width: u32, height: u32) -> PixelBox {
	PixelBox { x0: (x * TILE) as i64, y0: (y * TILE) as i64, x1: ((x + 1) * TILE) as i64, y1: ((y + 1) * TILE) as i64 }.clamped(width, height)
}

/// The window-space position and `1/w` of a clip-space vertex.
///
/// REFUSES `w` AT OR BELOW ZERO rather than dividing. A vertex on or behind the eye plane has no
/// projection, and the clipper is what removes it - a rasteriser that met one has been handed
/// geometry that was not clipped, and inventing a coordinate for it would draw a triangle that is
/// not the one submitted.
pub fn project(clip: Vec4, viewport: &render_math::Viewport) -> Result<(Vec3, f32), render3d::Error> {
	if !clip.is_finite() {
		return Err(render3d::Error::NonFinite { what: "a clip-space position" });
	}
	if clip.w <= 0.0 {
		return Err(render3d::Error::InvalidTransform);
	}
	let inverse_w = 1.0 / clip.w;
	let ndc = Vec3::new(clip.x * inverse_w, clip.y * inverse_w, clip.z * inverse_w);
	let window = render_math::window_from_ndc(ndc, viewport).map_err(|_| render3d::Error::InvalidTransform)?;
	Ok((window, inverse_w))
}
