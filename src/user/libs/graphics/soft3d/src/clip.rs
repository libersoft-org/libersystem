//! QUALIFIER-AWARE CLIPPING: the position, and every kind of varying, cut at the right parameter.
//!
//! THE THREE QUALIFIERS ARE CUT BY THREE DIFFERENT RULES, and the frozen profile states each with
//! its reason. Using one rule for all three is the commonest clipper defect and each of its three
//! symptoms looks like a different bug:
//!
//! - `smooth` follows the HOMOGENEOUS parameter `t`, and so does every component of the position
//!   INCLUDING `w`. The `t` that makes the position right is the `t` that makes a perspective-correct
//!   attribute right, because both are linear in homogeneous space.
//! - `noperspective` follows the PROJECTED parameter `u = t*w1 / ((1-t)*w0 + t*w1)`. An attribute
//!   declared linear on the screen must stay linear on the screen after a cut; the homogeneous `t`
//!   would put a kink exactly at the clip edge, which reads as a bent line in the wrong place.
//! - `flat` is CAPTURED FROM THE ORIGINAL PRIMITIVE'S PROVOKING VERTEX BEFORE CLIPPING and copied
//!   unchanged to every triangle the cut produces - including when that vertex was clipped away
//!   entirely. A flat attribute identifies the primitive: a material index, an object id. Taking it
//!   from a vertex the clipper invented would make an id depend on where the camera is.
//!
//! THE BOUND IS PROVEN AND NOT CONFIGURED. Sutherland-Hodgman against a convex volume adds at most
//! one vertex per half-space, so a triangle against seven planes reaches at most `3 + 6 = 9`
//! vertices - the `w` plane is a half-space of the same volume and cannot add a tenth beyond the six
//! that bound it - and fan triangulation of nine vertices is `9 - 2 = 7` triangles.
//!
//! AND NOTHING HERE ALLOCATES. A varying set is a fixed array with a length, not a `Vec`, and the two
//! working polygons live in a `Workspace` the caller keeps between primitives. The milestone's words
//! are "allocate nothing per primitive during traversal and clipping", and a clipper that allocated
//! one small vector per vertex would allocate several per primitive - which is a frame whose cost
//! depends on the allocator rather than on the geometry.

use render_math::Vec4;
use render3d::Error;
use render3d::clip::{Classification, PLANES, Plane, classify_positions};

/// The most varying components one vertex may carry, across all three qualifiers TOGETHER.
///
/// EIGHT `vec4`s, which is the floor every graphics API of the last twenty years has guaranteed, and
/// a bound rather than a `Vec` because it is what keeps the clipper allocation-free. A shader that
/// declares more is REFUSED at pipeline preparation, where a caller can do something about it,
/// rather than at the first draw.
pub const MAX_VARYING_COMPONENTS: usize = 32;

/// The most vertices a clipped triangle can have. PROVEN above, not chosen.
pub const MAX_CLIPPED_VERTICES: usize = 9;

/// The most triangles fan triangulation can produce from that.
pub const MAX_TRIANGLES_AFTER_CLIP: usize = MAX_CLIPPED_VERTICES - 2;

/// One vertex's varyings of one qualifier: a fixed array and a length.
#[derive(Clone, Copy, Debug)]
pub struct Varyings {
	values: [f32; MAX_VARYING_COMPONENTS],
	len: u8,
}

impl Default for Varyings {
	fn default() -> Self {
		Self::EMPTY
	}
}

impl PartialEq for Varyings {
	/// ONLY THE COMPONENTS THAT ARE THERE. The tail of the array is whatever the last longer vertex
	/// left, and comparing it would make two equal vertices unequal.
	fn eq(&self, other: &Self) -> bool {
		self.len == other.len && self.as_slice() == other.as_slice()
	}
}

impl Varyings {
	pub const EMPTY: Self = Self { values: [0.0; MAX_VARYING_COMPONENTS], len: 0 };

	/// Build from a slice. REFUSES a slice longer than the bound rather than truncating it: a
	/// truncated varying set is a surface shaded from values that are not the ones the vertex stage
	/// wrote, which looks like a shader bug.
	pub fn from_slice(components: &[f32]) -> Result<Self, Error> {
		if components.len() > MAX_VARYING_COMPONENTS {
			return Err(Error::LimitExceeded { limit: "varying components", ceiling: MAX_VARYING_COMPONENTS as u64, asked: components.len() as u64 });
		}
		let mut out = Self::EMPTY;
		out.values[..components.len()].copy_from_slice(components);
		out.len = components.len() as u8;
		Ok(out)
	}

	pub fn as_slice(&self) -> &[f32] {
		&self.values[..self.len as usize]
	}

	pub fn len(&self) -> usize {
		self.len as usize
	}

	pub fn is_empty(&self) -> bool {
		self.len == 0
	}

	/// Append one component, answering whether there was room.
	pub fn push(&mut self, value: f32) -> bool {
		if self.len as usize >= MAX_VARYING_COMPONENTS {
			return false;
		}
		self.values[self.len as usize] = value;
		self.len += 1;
		true
	}

	pub fn clear(&mut self) {
		self.len = 0;
	}

	/// Component-wise interpolation at one parameter.
	fn lerp(from: &Self, to: &Self, at: f32) -> Self {
		let mut out = Self::EMPTY;
		let count = from.len().min(to.len());
		for index in 0..count {
			out.values[index] = lerp(from.values[index], to.values[index], at);
		}
		out.len = count as u8;
		out
	}
}

/// One vertex as this clipper carries it. `Copy`, so a polygon is an array and not a graph of
/// allocations.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Vertex {
	/// Clip space, before the perspective divide.
	pub position: Vec4,
	/// Attributes interpolated at the homogeneous parameter.
	pub smooth: Varyings,
	/// Attributes interpolated at the PROJECTED parameter.
	pub noperspective: Varyings,
}

impl Vertex {
	pub fn new(position: Vec4) -> Self {
		Self { position, smooth: Varyings::EMPTY, noperspective: Varyings::EMPTY }
	}

	pub fn with_smooth(self, smooth: Varyings) -> Self {
		Self { smooth, ..self }
	}

	pub fn with_noperspective(self, noperspective: Varyings) -> Self {
		Self { noperspective, ..self }
	}
}

/// A primitive after clipping: its vertices, and the flat attributes of the ORIGINAL provoking
/// vertex.
#[derive(Clone, Copy, Debug)]
pub struct Clipped {
	vertices: [Vertex; MAX_CLIPPED_VERTICES],
	count: u8,
	/// CAPTURED BEFORE THE CUT and never recomputed.
	pub flat: Varyings,
}

impl Default for Clipped {
	fn default() -> Self {
		Self { vertices: [Vertex::new(Vec4::ZERO); MAX_CLIPPED_VERTICES], count: 0, flat: Varyings::EMPTY }
	}
}

impl Clipped {
	pub fn vertices(&self) -> &[Vertex] {
		&self.vertices[..self.count as usize]
	}

	pub fn is_empty(&self) -> bool {
		self.count == 0
	}

	/// How many triangles the fan triangulation produces.
	pub fn triangle_count(&self) -> usize {
		(self.count as usize).saturating_sub(2)
	}

	/// One triangle of the fan, as indices into `vertices`.
	///
	/// A FAN FROM VERTEX ZERO, which is `render3d`'s own triangulation - a clipped polygon is convex,
	/// so every fan from any vertex covers it exactly, and fixing the vertex is what makes two
	/// implementations produce the same triangles in the same order.
	pub fn triangle(&self, index: usize) -> Option<[usize; 3]> {
		(index < self.triangle_count()).then_some([0, index + 1, index + 2])
	}
}

/// The two working polygons, kept by the caller between primitives so nothing is allocated per
/// primitive.
pub struct Workspace {
	front: [Vertex; MAX_CLIPPED_VERTICES],
	front_len: usize,
	back: [Vertex; MAX_CLIPPED_VERTICES],
	back_len: usize,
}

impl Default for Workspace {
	fn default() -> Self {
		Self { front: [Vertex::new(Vec4::ZERO); MAX_CLIPPED_VERTICES], front_len: 0, back: [Vertex::new(Vec4::ZERO); MAX_CLIPPED_VERTICES], back_len: 0 }
	}
}

/// Clip a triangle, carrying every kind of varying by its own rule.
///
/// `flat` is the provoking vertex's attributes, which the CALLER captured from the primitive as
/// assembled - this function cannot recapture them, which is the point: by the time a vertex has
/// been cut away there is nothing left to read.
pub fn clip_triangle_into(vertices: &[Vertex; 3], flat: Varyings, work: &mut Workspace, out: &mut Clipped) -> Result<(), Error> {
	out.count = 0;
	out.flat = flat;
	// Classification is `render3d`'s, so this clipper and the API's own agree about what is outside.
	let positions = [vertices[0].position, vertices[1].position, vertices[2].position];
	match classify_positions(&positions)? {
		Classification::Inside => {
			out.vertices[..3].copy_from_slice(vertices);
			out.count = 3;
			return Ok(());
		}
		Classification::Outside | Classification::BehindEye => return Ok(()),
		Classification::Clipped => {}
	}
	work.front[..3].copy_from_slice(vertices);
	work.front_len = 3;
	for plane in PLANES {
		if work.front_len == 0 {
			break;
		}
		work.back_len = cut(&work.front[..work.front_len], plane, &mut work.back)?;
		core::mem::swap(&mut work.front, &mut work.back);
		work.front_len = work.back_len;
	}
	if work.front_len >= 3 {
		out.vertices[..work.front_len].copy_from_slice(&work.front[..work.front_len]);
		out.count = work.front_len as u8;
	}
	Ok(())
}

/// The same, for a caller that does not keep a workspace - a fixture, or a one-off query.
///
/// IT BOXES ONE, because the workspace is a few kilobytes and a stack copy of it per call is the
/// thing the in-place form exists to avoid. A renderer uses `clip_triangle_into`.
pub fn clip_triangle(vertices: &[Vertex; 3], flat: Varyings) -> Result<Clipped, Error> {
	let mut work = alloc::boxed::Box::<Workspace>::default();
	let mut out = Clipped::default();
	clip_triangle_into(vertices, flat, &mut work, &mut out)?;
	Ok(out)
}

/// One Sutherland-Hodgman pass against one plane, answering how many vertices it produced.
fn cut(polygon: &[Vertex], plane: Plane, into: &mut [Vertex; MAX_CLIPPED_VERTICES]) -> Result<usize, Error> {
	let mut count = 0;
	let mut put = |vertex: Vertex, count: &mut usize| -> Result<(), Error> {
		if *count >= MAX_CLIPPED_VERTICES {
			// THE BOUND IS AN ASSERTION AND NOT A CLAMP. Exceeding it means the algorithm is not the
			// one the proof is about, and truncating would silently draw a different shape.
			return Err(Error::LimitExceeded { limit: "clipped vertices", ceiling: MAX_CLIPPED_VERTICES as u64, asked: *count as u64 + 1 });
		}
		into[*count] = vertex;
		*count += 1;
		Ok(())
	};
	for index in 0..polygon.len() {
		let current = &polygon[index];
		let next = &polygon[(index + 1) % polygon.len()];
		let here = plane.distance(current.position);
		let there = plane.distance(next.position);
		let inside_here = here >= 0.0;
		let inside_there = there >= 0.0;
		if inside_here {
			put(*current, &mut count)?;
		}
		if inside_here != inside_there {
			let denominator = here - there;
			// A ZERO DENOMINATOR PRODUCES NO NEW VERTEX and the result follows from the inside and
			// outside classification, which the pushes above and below already express. Dividing
			// would give an infinity whose sign decided the shape.
			if denominator == 0.0 || !denominator.is_finite() {
				continue;
			}
			put(intersect(current, next, here / denominator), &mut count)?;
		}
	}
	Ok(count)
}

/// The vertex at parameter `t` along an edge, with each qualifier cut by its own rule.
fn intersect(from: &Vertex, to: &Vertex, t: f32) -> Vertex {
	// EVERY COMPONENT OF THE POSITION INCLUDING `w`, at the homogeneous parameter.
	let position = Vec4::new(lerp(from.position.x, to.position.x, t), lerp(from.position.y, to.position.y, t), lerp(from.position.z, to.position.z, t), lerp(from.position.w, to.position.w, t));
	let smooth = Varyings::lerp(&from.smooth, &to.smooth, t);
	// THE PROJECTED PARAMETER for a `noperspective` attribute.
	let denominator = (1.0 - t) * from.position.w + t * to.position.w;
	let u = if denominator == 0.0 || !denominator.is_finite() { t } else { t * to.position.w / denominator };
	let noperspective = Varyings::lerp(&from.noperspective, &to.noperspective, u);
	Vertex { position, smooth, noperspective }
}

fn lerp(from: f32, to: f32, at: f32) -> f32 {
	// `(1-t)*a + t*b` AND NOT `a + t*(b - a)`. The first is exact at both ends whatever the rounding;
	// the second can miss the endpoint, which puts a clipped vertex slightly off the plane it was
	// cut against - and then the next plane's test sees a vertex that should have been exactly on it.
	(1.0 - at) * from + at * to
}
