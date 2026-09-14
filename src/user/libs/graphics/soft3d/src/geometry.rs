//! PRIMITIVE ASSEMBLY, AND THE TWO TOPOLOGIES THE TRIANGLE RULES DO NOT COVER.
//!
//! A PROFILE THAT MANDATES SIX TOPOLOGIES AND SPECIFIES ONE IS A PROFILE WITH FIVE GAPS, so the line
//! and point rules are frozen beside the triangle ones and this implements them. A line has no
//! interior and no winding; a point has neither and no edges either. Every question the triangle
//! rules answer has to be answered again for them.
//!
//! LINES USE THE DIAMOND-EXIT RULE, for the same reason triangles use the top-left one: two segments
//! that meet end to end must together cover each pixel EXACTLY once. A midpoint or Bresenham walk
//! covers the shared endpoint's pixel from both segments, which is visible the moment a polyline is
//! drawn with any blending at all.
//!
//! BACK-FACE CULLING APPLIES TO TRIANGLES ONLY. A line and a point have no winding, so a cull mode
//! cannot remove them; a backend that applied the triangle rule to them would make a wireframe
//! overlay disappear at half the angles.

use alloc::vec::Vec;

use render3d::command::Topology;
use render3d::{Error, error::MeshFault};

use crate::fixed::{self, Subpixel};

/// The index value that ends a strip or a fan. FIXED BY WIDTH and not configurable: a restart index
/// a caller chose would be a legal vertex index in some other draw.
pub const RESTART_U16: u16 = 0xFFFF;
pub const RESTART_U32: u32 = 0xFFFF_FFFF;

/// The clamp on a point's size, in pixels. A larger point is a quad the application should draw as
/// one, with the texture coordinates it wants.
pub const MIN_POINT_SIZE: f32 = 1.0;
pub const MAX_POINT_SIZE: f32 = 64.0;

/// Where a draw's indices come from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Indices<'a> {
	/// A non-indexed draw: vertex `i` is index `i`.
	None,
	U16(&'a [u16]),
	U32(&'a [u32]),
}

impl Indices<'_> {
	fn len(&self) -> usize {
		match self {
			Self::None => 0,
			Self::U16(values) => values.len(),
			Self::U32(values) => values.len(),
		}
	}

	/// The index at a position, or `None` when it is the restart value.
	fn at(&self, position: usize, base_vertex: i32) -> Result<Option<u32>, Error> {
		let raw = match self {
			Self::None => position as u32,
			Self::U16(values) => {
				let value = *values.get(position).ok_or(Error::InvalidMesh { reason: MeshFault::IndexOutOfRange { index: position as u32, vertices: values.len() as u32 } })?;
				if value == RESTART_U16 {
					return Ok(None);
				}
				value as u32
			}
			Self::U32(values) => {
				let value = *values.get(position).ok_or(Error::InvalidMesh { reason: MeshFault::IndexOutOfRange { index: position as u32, vertices: values.len() as u32 } })?;
				if value == RESTART_U32 {
					return Ok(None);
				}
				value
			}
		};
		// THE BASE VERTEX IS SIGNED AND THE SUM IS CHECKED. A negative base with a small index is a
		// read before the buffer, which is exactly the case a per-index bound check catches and a
		// per-draw one does not.
		let shifted = raw as i64 + base_vertex as i64;
		if shifted < 0 || shifted > u32::MAX as i64 {
			return Err(Error::InvalidMesh { reason: MeshFault::IndexOutOfRange { index: raw, vertices: 0 } });
		}
		Ok(Some(shifted as u32))
	}
}

/// One assembled primitive, with the index of the primitive it is within the draw - which is what
/// the provoking-vertex rule is stated against.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Primitive {
	Triangle { vertices: [u32; 3], index: u32 },
	Line { vertices: [u32; 2], index: u32 },
	Point { vertex: u32, index: u32 },
}

impl Primitive {
	pub fn index(&self) -> u32 {
		match self {
			Self::Triangle { index, .. } | Self::Line { index, .. } | Self::Point { index, .. } => *index,
		}
	}
}

/// Assemble a draw's primitives.
///
/// PRIMITIVE RESTART ENDS A STRIP OR A FAN AND IS REFUSED ON A LIST, because a list has nothing to
/// restart: the indices are independent and a restart in the middle of one would silently drop a
/// primitive rather than mean anything.
///
/// NOTHING IS ALLOCATED PER PRIMITIVE DURING TRAVERSAL by a caller that reuses `into` - it is
/// cleared, not reallocated.
pub fn assemble(topology: Topology, indices: Indices<'_>, count: u32, base_vertex: i32, restart: bool, into: &mut Vec<Primitive>, run: &mut Vec<u32>) -> Result<(), Error> {
	into.clear();
	run.clear();
	if restart && matches!(topology, Topology::TriangleList | Topology::LineList | Topology::PointList) {
		return Err(Error::InvalidRenderState { reason: "primitive restart on a list topology, which has nothing to restart" });
	}
	if !matches!(indices, Indices::None) && count as usize > indices.len() {
		return Err(Error::InvalidMesh { reason: MeshFault::IndexOutOfRange { index: count, vertices: indices.len() as u32 } });
	}
	let mut primitive = 0_u32;
	// The run of indices since the last restart, which is what a strip or a fan is built over. THE
	// CALLER OWNS IT so an assembled draw allocates nothing.
	let mut hub: Option<u32> = None;
	for position in 0..count as usize {
		match indices.at(position, base_vertex)? {
			None => {
				run.clear();
				hub = None;
				continue;
			}
			Some(index) => {
				run.push(index);
				if hub.is_none() {
					hub = Some(index);
				}
			}
		}
		match topology {
			Topology::TriangleList => {
				if run.len() == 3 {
					into.push(Primitive::Triangle { vertices: [run[0], run[1], run[2]], index: primitive });
					primitive += 1;
					run.clear();
				}
			}
			Topology::TriangleStrip => {
				if run.len() >= 3 {
					let last = run.len() - 1;
					// THE WINDING ALTERNATES so every triangle of a strip faces the same way. A strip
					// that did not alternate would have every other triangle culled.
					let vertices = if (last - 2) % 2 == 0 { [run[last - 2], run[last - 1], run[last]] } else { [run[last - 1], run[last - 2], run[last]] };
					into.push(Primitive::Triangle { vertices, index: primitive });
					primitive += 1;
				}
			}
			Topology::TriangleFan => {
				if run.len() >= 3 {
					let last = run.len() - 1;
					into.push(Primitive::Triangle { vertices: [run[0], run[last - 1], run[last]], index: primitive });
					primitive += 1;
				}
			}
			Topology::LineList => {
				if run.len() == 2 {
					into.push(Primitive::Line { vertices: [run[0], run[1]], index: primitive });
					primitive += 1;
					run.clear();
				}
			}
			Topology::LineStrip => {
				if run.len() >= 2 {
					let last = run.len() - 1;
					into.push(Primitive::Line { vertices: [run[last - 1], run[last]], index: primitive });
					primitive += 1;
				}
			}
			Topology::PointList => {
				into.push(Primitive::Point { vertex: run[0], index: primitive });
				primitive += 1;
				run.clear();
			}
		}
	}
	let _ = hub;
	Ok(())
}

/// The vertex of a primitive a `flat` attribute comes from. `render3d`'s rule, not a second copy.
pub fn provoking(topology: Topology, primitive: u32) -> u32 {
	render3d::clip::provoking_vertex(topology, primitive)
}

// ---------------------------------------------------------------------------------------------
// Lines.
// ---------------------------------------------------------------------------------------------

/// An exact non-negative rational, for the diamond-exit parameters.
///
/// EXACT AND NOT `f32`. Line coverage is on the bit-exact side of this stack's split, and a
/// parameter computed in floating point puts the exit on the other side of a pixel boundary often
/// enough to matter at a shared endpoint - which is the one place the rule exists to get right.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Frac {
	numerator: i64,
	/// Always positive.
	denominator: i64,
}

impl Frac {
	const ZERO: Self = Self { numerator: 0, denominator: 1 };
	const ONE: Self = Self { numerator: 1, denominator: 1 };

	fn new(numerator: i64, denominator: i64) -> Self {
		if denominator < 0 { Self { numerator: -numerator, denominator: -denominator } } else { Self { numerator, denominator } }
	}

	fn less(self, other: Self) -> bool {
		(self.numerator as i128) * (other.denominator as i128) < (other.numerator as i128) * (self.denominator as i128)
	}
}

fn maximum(left: Frac, right: Frac) -> Frac {
	if left.less(right) { right } else { left }
}

fn minimum(left: Frac, right: Frac) -> Frac {
	if left.less(right) { left } else { right }
}

/// Whether a segment covers a pixel under the DIAMOND-EXIT rule.
///
/// The diamond is the set of points whose Manhattan distance from the pixel centre is BELOW half a
/// pixel, which is four half-planes. The segment covers the pixel when it is strictly inside that
/// region somewhere in `[0, 1)` AND is NOT strictly inside it at `t = 1` - that is, it EXITS. The
/// second half is what makes two segments meeting end to end light the shared pixel exactly once:
/// the first ends inside the diamond and does not light it, the second starts there and leaves.
pub fn line_covers_pixel(from: (Subpixel, Subpixel), to: (Subpixel, Subpixel), pixel_x: i64, pixel_y: i64) -> bool {
	let centre_x = pixel_x * fixed::SUBPIXEL_ONE + fixed::SUBPIXEL_HALF;
	let centre_y = pixel_y * fixed::SUBPIXEL_ONE + fixed::SUBPIXEL_HALF;
	let x0 = from.0.0 - centre_x;
	let y0 = from.1.0 - centre_y;
	let dx = to.0.0 - from.0.0;
	let dy = to.1.0 - from.1.0;
	// The four half-planes `s_x * x + s_y * y < HALF`.
	let signs = [(1_i64, 1_i64), (1, -1), (-1, 1), (-1, -1)];
	let mut low = Frac::ZERO;
	let mut high = Frac::ONE;
	let mut inside_at_end = true;
	for (sign_x, sign_y) in signs {
		let offset = sign_x * x0 + sign_y * y0;
		let slope = sign_x * dx + sign_y * dy;
		// `offset + t * slope < HALF`
		if slope == 0 {
			// A constraint the segment cannot change: either it is satisfied along the whole segment
			// or it is satisfied nowhere.
			if offset >= fixed::SUBPIXEL_HALF {
				return false;
			}
			continue;
		}
		let bound = Frac::new(fixed::SUBPIXEL_HALF - offset, slope);
		if slope > 0 {
			high = minimum(high, bound);
		} else {
			low = maximum(low, bound);
		}
		if offset + slope >= fixed::SUBPIXEL_HALF {
			inside_at_end = false;
		}
	}
	if !low.less(high) {
		return false;
	}
	// It was inside somewhere in the segment; it covers the pixel only if it LEFT.
	!inside_at_end
}

/// The window-space parameter along a segment at a pixel centre, for interpolating varyings.
///
/// THE DISTANCE ALONG THE SEGMENT IN WINDOW SPACE, so a `noperspective` varying is linear along the
/// drawn line. The projection of the pixel centre onto the segment is what a reader would compute by
/// hand, and it is what this computes.
pub fn line_parameter(from: (Subpixel, Subpixel), to: (Subpixel, Subpixel), pixel_x: i64, pixel_y: i64) -> f32 {
	let dx = (to.0.0 - from.0.0) as f64;
	let dy = (to.1.0 - from.1.0) as f64;
	let length_squared = dx * dx + dy * dy;
	if length_squared == 0.0 {
		return 0.0;
	}
	let px = (pixel_x * fixed::SUBPIXEL_ONE + fixed::SUBPIXEL_HALF - from.0.0) as f64;
	let py = (pixel_y * fixed::SUBPIXEL_ONE + fixed::SUBPIXEL_HALF - from.1.0) as f64;
	(((px * dx + py * dy) / length_squared).clamp(0.0, 1.0)) as f32
}

// ---------------------------------------------------------------------------------------------
// Points.
// ---------------------------------------------------------------------------------------------

/// The point size a vertex stage asked for, clamped and rounded.
///
/// A SIZE OF ZERO DRAWS NOTHING rather than one pixel, because a program that computes a size from a
/// distance expects the point to vanish. Everything else is clamped into the profile's range and
/// rounded half away from zero, which is the same rule the raster grid uses.
pub fn point_size(requested: f32) -> u32 {
	if !requested.is_finite() || requested <= 0.0 {
		return 0;
	}
	let clamped = requested.clamp(MIN_POINT_SIZE, MAX_POINT_SIZE);
	(clamped as f64 + 0.5) as u32
}

/// Whether a point of `size` pixels centred at `centre` covers a sample.
///
/// THE SQUARE IS HALF-OPEN, which is the axis-aligned form of the top-left rule: two adjacent points
/// of the same size tile without overlapping, the same requirement two triangles sharing an edge
/// meet.
pub fn point_covers(centre: (Subpixel, Subpixel), size: u32, sample: (Subpixel, Subpixel)) -> bool {
	if size == 0 {
		return false;
	}
	let half = (size as i64 * fixed::SUBPIXEL_ONE) / 2;
	let low_x = centre.0.0 - half;
	let low_y = centre.1.0 - half;
	let high_x = low_x + size as i64 * fixed::SUBPIXEL_ONE;
	let high_y = low_y + size as i64 * fixed::SUBPIXEL_ONE;
	sample.0.0 >= low_x && sample.0.0 < high_x && sample.1.0 >= low_y && sample.1.0 < high_y
}
