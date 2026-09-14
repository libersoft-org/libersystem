//! `ClipCoordQ` AND THE CLIPPER, in the one space and the one order the profile freezes.
//!
//! CLIPPING IS HOMOGENEOUS AND AT `f32`, BEFORE ANY DIVIDE. Dividing first and clipping after
//! produces coordinates of ten million pixels for a vertex just behind the eye, and a rasteriser that
//! spends a frame on one triangle. A backend that clips after the divide, or in fixed point,
//! produces different vertices at the same input - and the conformance suite compares vertices.
//!
//! THE PLANE ORDER IS PART OF THE PROFILE, and this module reads it rather than choosing one.
//! Clipping is NOT ASSOCIATIVE in floating point: the vertex where a triangle crosses two planes
//! depends on which plane cut it first, and two backends that disagree about the order disagree about
//! that vertex by an amount a comparison can see.
//!
//! THREE ANSWERS AND NOT TWO, which is the distinction the profile makes and a clipper usually does
//! not:
//!
//!   * A NON-FINITE COMPONENT REFUSES THE PRIMITIVE. A NaN or an infinity makes every inequality
//!     false, so a clipper cannot decide the primitive at all - and silently dropping it would make a
//!     shader bug look like a culling rule.
//!   * `w <= 0` ON EVERY VERTEX IS A CULL. The primitive is behind the eye, which is the ordinary
//!     state of most of a scene, and a refusal would report it as an error every frame.
//!   * `w <= 0` ON SOME IS A CLIP against `w = CLIP_W_EPSILON` first. Clipping against `w = 0`
//!     exactly produces a vertex at infinity after the divide.
//!
//! AND THE INTERSECTION PARAMETER IS COMPUTED AT `f32` AND KEPT AT `f32`. No wider precision and a
//! round: the suite compares positions bit-exactly under StrictF32, so a backend that computed `t` at
//! `f64` would be right by a different amount.

use alloc::vec::Vec;

use crate::error::Error;
use graphics_profile::render3d_spec::{CLIP_PLANE_ORDER, CLIP_W_EPSILON};
use render_math::Vec4;

/// The `w` below which a vertex is on the horizon rather than in front of the eye, as an `f32`.
///
/// THE PROFILE STATES IT AT `f64` because it states one value for the 2D and 3D profiles together;
/// this is that value in the arithmetic the clip is performed in, converted once, here.
pub const W_EPSILON: f32 = CLIP_W_EPSILON as f32;

/// A vertex position after the vertex stage and before the divide.
///
/// A NEWTYPE AND NOT A `Vec4`, because "a vec4" is not a definition: what this type carries is the
/// promise that its value came out of the vertex stage and has not been divided, which is what every
/// rule below is about. A position that had been divided would pass every check here and be wrong.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ClipCoordQ(pub Vec4);

impl ClipCoordQ {
	pub const fn new(position: Vec4) -> Self {
		Self(position)
	}

	pub fn is_finite(self) -> bool {
		self.0.is_finite()
	}

	/// Whether this vertex is inside the clip volume: `-w <= x <= w`, `-w <= y <= w`, `0 <= z <= w`,
	/// with `w > 0`.
	pub fn inside_volume(self) -> bool {
		let Vec4 { x, y, z, w } = self.0;
		w > 0.0 && -w <= x && x <= w && -w <= y && y <= w && 0.0 <= z && z <= w
	}
}

/// What a clipper decided about a primitive, BEFORE it produced any vertices.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Classification {
	/// Every vertex is inside every plane. Nothing is cut.
	Inside,
	/// Some vertex is outside some plane. The primitive is clipped.
	Clipped,
	/// Every vertex is outside ONE plane, so the primitive cannot intersect the volume. Dropped
	/// without cutting anything - which is what makes a scene mostly off screen cheap.
	Outside,
	/// `w <= 0` on every vertex: entirely behind the eye. A CULL and not a refusal.
	BehindEye,
}

/// The plane a clipper cuts against, in the profile's own order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Plane {
	/// `w >= CLIP_W_EPSILON`, which is cut FIRST.
	W,
	NearZ,
	FarZ,
	NegativeX,
	PositiveX,
	NegativeY,
	PositiveY,
}

/// The planes in the order the profile freezes. READ FROM THE PROFILE'S OWN LIST rather than written
/// out again: the order is the contract, and a second copy is a second order.
pub const PLANES: [Plane; 7] = [Plane::W, Plane::NearZ, Plane::FarZ, Plane::NegativeX, Plane::PositiveX, Plane::NegativeY, Plane::PositiveY];

impl Plane {
	/// The name the frozen list uses, so a reader can go from a value here to the row that orders it.
	pub const fn name(self) -> &'static str {
		match self {
			Self::W => "w = CLIP_W_EPSILON",
			Self::NearZ => "z >= 0",
			Self::FarZ => "z <= w",
			Self::NegativeX => "x >= -w",
			Self::PositiveX => "x <= w",
			Self::NegativeY => "y >= -w",
			Self::PositiveY => "y <= w",
		}
	}

	/// The signed distance to the plane, positive INSIDE. The homogeneous form: no divide, which is
	/// the whole reason the clip happens here.
	pub fn distance(self, position: Vec4) -> f32 {
		let Vec4 { x, y, z, w } = position;
		match self {
			Self::W => w - W_EPSILON,
			Self::NearZ => z,
			Self::FarZ => w - z,
			Self::NegativeX => x + w,
			Self::PositiveX => w - x,
			Self::NegativeY => y + w,
			Self::PositiveY => w - y,
		}
	}
}

/// One vertex as the clipper carries it: its position and its interpolated attributes.
///
/// THE ATTRIBUTES TRAVEL WITH THE POSITION because they are cut at the same parameter. A clipper
/// that produced positions and left a caller to interpolate attributes would need the caller to
/// recompute `t`, and two computations of one `t` are two values of it.
#[derive(Clone, Debug)]
pub struct ClipVertex {
	pub position: ClipCoordQ,
	/// `smooth` attributes, interpolated at the homogeneous `t`.
	pub smooth: Vec<f32>,
}

impl ClipVertex {
	pub fn new(position: Vec4, smooth: Vec<f32>) -> Self {
		Self { position: ClipCoordQ::new(position), smooth }
	}
}

/// Classify a primitive before cutting it.
///
/// EVERY ANSWER IS DECIDED HERE AND IN THIS ORDER: a non-finite component refuses, everything behind
/// the eye culls, everything outside ONE plane drops, everything inside every plane passes through,
/// and what is left is clipped. Deciding "outside" after the first cut would cut a primitive that
/// never needed it.
pub fn classify(vertices: &[ClipVertex]) -> Result<Classification, Error> {
	// The attributes play no part in the decision, so the answer is the positions' alone - which is
	// what lets a backend that stores its varyings some other way reach the SAME decision rather
	// than a second implementation of it.
	let positions: Vec<Vec4> = vertices.iter().map(|vertex| vertex.position.0).collect();
	classify_positions(&positions)
}

/// The same decision, over positions alone.
///
/// SEPARATE SO A BACKEND NEED NOT BUILD A `ClipVertex` TO ASK. A software rasteriser that keeps its
/// varyings inline - to allocate nothing per primitive - cannot construct the `Vec`-carrying form
/// without allocating exactly what it was avoiding, and a backend that reimplemented the decision
/// would be a second answer to "what is outside".
pub fn classify_positions(positions: &[Vec4]) -> Result<Classification, Error> {
	if positions.is_empty() {
		return Err(Error::InvalidMesh { reason: crate::error::MeshFault::TooFewVertices { topology: "a clipped primitive", needs: 1, has: 0 } });
	}
	// A NON-FINITE COMPONENT REFUSES THE PRIMITIVE, not the vertex: every inequality is false for a
	// NaN, so there is no decision to make, and dropping it would make a shader bug look like culling.
	for position in positions {
		if !position.is_finite() {
			return Err(Error::NonFinite { what: "a clip-space vertex position" });
		}
	}
	// BEHIND THE EYE IS A CULL AND NOT A REFUSAL: it is the ordinary state of geometry behind the
	// camera, and reporting it would report most of a scene as an error every frame.
	if positions.iter().all(|position| position.w <= 0.0) {
		return Ok(Classification::BehindEye);
	}
	let mut any_outside = false;
	for plane in PLANES {
		let mut all_outside = true;
		for position in positions {
			if plane.distance(*position) >= 0.0 {
				all_outside = false;
			} else {
				any_outside = true;
			}
		}
		if all_outside {
			return Ok(Classification::Outside);
		}
	}
	Ok(if any_outside { Classification::Clipped } else { Classification::Inside })
}

/// Clip a convex polygon against every plane, in the frozen order.
///
/// SUTHERLAND-HODGMAN, one plane at a time, because that is what the frozen ORDER is an order of. The
/// parameter is computed at `f32` from the homogeneous distances and every component - INCLUDING `w`
/// - is interpolated at that same `t`, which is what makes a `smooth` attribute correct: the `t` that
/// makes the position right is the `t` that makes a perspective-correct attribute right, because both
/// are linear in homogeneous space.
pub fn clip_polygon(vertices: &[ClipVertex]) -> Result<Vec<ClipVertex>, Error> {
	match classify(vertices)? {
		Classification::Inside => Ok(vertices.to_vec()),
		Classification::Outside | Classification::BehindEye => Ok(Vec::new()),
		Classification::Clipped => {
			let mut current: Vec<ClipVertex> = vertices.to_vec();
			for plane in PLANES {
				if current.is_empty() {
					break;
				}
				current = clip_against(&current, plane);
			}
			Ok(current)
		}
	}
}

/// One Sutherland-Hodgman pass against one plane.
fn clip_against(vertices: &[ClipVertex], plane: Plane) -> Vec<ClipVertex> {
	let mut out: Vec<ClipVertex> = Vec::new();
	let count = vertices.len();
	for index in 0..count {
		let current = &vertices[index];
		let next = &vertices[(index + 1) % count];
		let current_distance = plane.distance(current.position.0);
		let next_distance = plane.distance(next.position.0);
		let current_inside = current_distance >= 0.0;
		let next_inside = next_distance >= 0.0;
		if current_inside {
			out.push(current.clone());
		}
		// A CROSSING PRODUCES EXACTLY ONE VERTEX, and a vertex exactly ON the plane is inside rather
		// than a crossing - so an edge lying in the plane does not produce a pair of coincident
		// vertices, which is the degenerate triangle every naive clipper emits.
		if current_inside != next_inside {
			out.push(intersect(current, next, current_distance, next_distance));
		}
	}
	out
}

/// The vertex where an edge crosses a plane.
///
/// `t = d0 / (d0 - d1)`, AT `f32`. The profile says the parameter and every interpolated attribute
/// are computed at `f32` and KEPT at `f32` - no wider precision and a round - because the suite
/// compares positions bit-exactly.
fn intersect(from: &ClipVertex, to: &ClipVertex, from_distance: f32, to_distance: f32) -> ClipVertex {
	let denominator = from_distance - to_distance;
	// The two distances have opposite signs here, so the denominator cannot be zero - but a caller
	// reaching this with equal ones would get the `from` endpoint rather than a division by zero.
	let t = if denominator == 0.0 { 0.0 } else { from_distance / denominator };
	let position = Vec4::new(from.position.0.x + (to.position.0.x - from.position.0.x) * t, from.position.0.y + (to.position.0.y - from.position.0.y) * t, from.position.0.z + (to.position.0.z - from.position.0.z) * t, from.position.0.w + (to.position.0.w - from.position.0.w) * t);
	let mut smooth = Vec::new();
	// The attribute list is the caller's and both endpoints have the same length; a shorter one is
	// interpolated as far as it goes rather than panicking on a mismatch this layer cannot fix.
	let shared = from.smooth.len().min(to.smooth.len());
	for index in 0..shared {
		smooth.push(from.smooth[index] + (to.smooth[index] - from.smooth[index]) * t);
	}
	ClipVertex { position: ClipCoordQ::new(position), smooth }
}

/// Triangulate a clipped polygon as a FAN FROM ITS FIRST VERTEX.
///
/// THE FAN IS THE PROFILE'S CHOICE AND IT IS AN IMPLEMENTATION DETAIL OF CLIPPING THAT MUST NOT BE
/// VISIBLE. Every resulting triangle carries the ORIGINAL primitive's provoking value, which is why
/// this function answers indices rather than vertices: a caller keeping the flat attributes of the
/// primitive it started with cannot accidentally take them from a vertex the clipper invented.
pub fn fan_triangles(vertex_count: usize) -> Vec<[usize; 3]> {
	let mut out = Vec::new();
	if vertex_count < 3 {
		return out;
	}
	for index in 1..vertex_count - 1 {
		out.push([0, index, index + 1]);
	}
	out
}

/// Which vertex of a primitive a `flat` attribute comes from, per topology.
///
/// STATED PER TOPOLOGY AND NOT AS ONE SENTENCE, because a strip and a fan number their vertices
/// differently and "the first vertex" means a different thing in each. A fan's is the SECOND vertex -
/// the one that is not the shared hub - because the hub is in every triangle and choosing it would
/// give every triangle of a fan the same flat value.
pub fn provoking_vertex(topology: crate::command::Topology, primitive: u32) -> u32 {
	use crate::command::Topology;
	match topology {
		Topology::TriangleList => 3 * primitive,
		Topology::TriangleStrip => primitive,
		// Vertex 0 is the hub; triangle `i` is (0, i+1, i+2) and its provoking vertex is `i + 1`.
		Topology::TriangleFan => primitive + 1,
		Topology::LineList => 2 * primitive,
		Topology::LineStrip => primitive,
		Topology::PointList => primitive,
	}
}

/// The frozen plane order, as the names the profile lists - so a fixture can hold this module's own
/// order against it rather than against a copy.
pub fn frozen_order() -> &'static [&'static str] {
	CLIP_PLANE_ORDER
}
