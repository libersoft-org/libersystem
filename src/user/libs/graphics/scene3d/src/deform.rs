//! SKINNING AND MORPH TARGETS: the two ways a mesh's vertices move without its drawable moving.
//!
//! THE ORDER IS MORPH THEN SKIN, and the profile states it because it is not a preference. A morph
//! target is an additive displacement authored in the REST POSE - a face's expression, a bulge, a
//! blend shape - and skinning is what carries the rest pose into the pose the skeleton is in. A
//! morph applied after skinning would displace a vertex along a rest-pose direction while the vertex
//! is somewhere else entirely, which moves it out of the pose rather than within it.
//!
//! THE WEIGHTS ARE NORMALISED AT LOAD AND NOT AT DRAW. A shader that normalised every frame would be
//! paying for an authoring error every frame, on every vertex, for the life of the asset; doing it
//! once when the influences are built costs nothing afterwards and makes an unnormalised set a thing
//! that cannot exist rather than a thing that is corrected.
//!
//! AND THE JOINT TRANSFORM IS COMPOSED ON THE HOST. `joint_world * inverse_bind`, once per frame per
//! skeleton, so the vertex stage only multiplies and adds - which is what makes a 128-joint skeleton
//! affordable in a software rasteriser at all.

use alloc::vec::Vec;

use render_math::{Mat4, Vec3};

use crate::bounds::Aabb;
use crate::scene::{Error, Limits};

/// How many joints may move one vertex.
///
/// FOUR, WHICH IS THE PROFILE'S NUMBER AND ALSO THE HARDWARE'S. A fifth influence on a vertex is
/// almost always an authoring artefact whose weight rounds to nothing, and admitting it would make
/// every vertex in every mesh carry a variable-length list for the sake of a case that does not
/// improve the picture.
pub const MAX_INFLUENCES: usize = 4;

/// Which joints move one vertex, and how much.
///
/// NORMALISED ON CONSTRUCTION, so there is no such thing as an unnormalised `Influences`. An unused
/// slot is a weight of zero, and its joint index is then never read.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Influences {
	joints: [u16; MAX_INFLUENCES],
	weights: [f32; MAX_INFLUENCES],
}

impl Influences {
	/// The influences of one vertex, normalised to sum to one.
	///
	/// REFUSED RATHER THAN REPAIRED when the weights cannot be normalised: a set that sums to zero
	/// names no joint at all, and a vertex with no joint is one that stays at the origin while the
	/// mesh around it moves - which looks like a tear in the geometry and is traced back to the
	/// exporter rather than to the renderer only after somebody has looked at the shader.
	pub fn new(joints: [u16; MAX_INFLUENCES], weights: [f32; MAX_INFLUENCES]) -> Result<Self, Error> {
		let mut total = 0.0f32;
		for weight in weights {
			if !weight.is_finite() || weight < 0.0 {
				return Err(Error::Degenerate { reason: "a skinning weight must be finite and at or above zero" });
			}
			total += weight;
		}
		if total <= 0.0 {
			return Err(Error::Degenerate { reason: "a vertex's skinning weights must sum to more than zero" });
		}
		let mut normalised = [0.0f32; MAX_INFLUENCES];
		for (slot, weight) in normalised.iter_mut().zip(weights) {
			*slot = weight / total;
		}
		Ok(Self { joints, weights: normalised })
	}

	/// A vertex bound rigidly to one joint, which is the common case and cannot fail.
	pub fn rigid(joint: u16) -> Self {
		let mut weights = [0.0f32; MAX_INFLUENCES];
		weights[0] = 1.0;
		Self { joints: [joint, 0, 0, 0], weights }
	}

	pub fn joints(&self) -> &[u16; MAX_INFLUENCES] {
		&self.joints
	}

	pub fn weights(&self) -> &[f32; MAX_INFLUENCES] {
		&self.weights
	}
}

/// A skeleton's joint matrices for one frame: `joint_world * inverse_bind`, already composed.
#[derive(Clone, PartialEq, Debug)]
pub struct Pose {
	matrices: Vec<Mat4>,
}

impl Pose {
	/// Compose one frame's pose from the skeleton's world transforms and its bind pose.
	///
	/// THE TWO LISTS MUST BE THE SAME LENGTH, and a mismatch is refused rather than truncated: a
	/// skeleton with more joints than inverse binds would silently drop the last ones, which moves
	/// part of a character and leaves the rest behind.
	pub fn compose(joint_world: &[Mat4], inverse_bind: &[Mat4], limits: &Limits) -> Result<Self, Error> {
		if joint_world.len() != inverse_bind.len() {
			return Err(Error::Degenerate { reason: "a skeleton needs one inverse bind matrix per joint" });
		}
		let asked = joint_world.len() as u32;
		if asked > limits.max_skeleton_joints {
			return Err(Error::LimitExceeded { limit: "max_skeleton_joints", ceiling: limits.max_skeleton_joints, asked });
		}
		let mut matrices: Vec<Mat4> = Vec::with_capacity(joint_world.len());
		for (world, bind) in joint_world.iter().zip(inverse_bind) {
			if !world.is_finite() || !bind.is_finite() {
				return Err(Error::Degenerate { reason: "a joint transform must be finite" });
			}
			matrices.push(world.mul(bind));
		}
		Ok(Self { matrices })
	}

	pub fn joints(&self) -> u32 {
		self.matrices.len() as u32
	}

	pub fn matrix(&self, joint: u16) -> Option<&Mat4> {
		self.matrices.get(joint as usize)
	}

	/// Where a rest-pose point ends up: the weighted sum of the point through each influencing
	/// joint.
	///
	/// A JOINT INDEX PAST THE SKELETON CONTRIBUTES NOTHING, and its weight goes with it. The
	/// alternative - refusing - would turn one bad index in one vertex of one asset into a frame
	/// that does not draw, and the weights are normalised, so what is left is the pose the remaining
	/// joints give rather than a vertex at the origin.
	pub fn skin_point(&self, point: Vec3, influences: &Influences) -> Vec3 {
		let mut out = Vec3::new(0.0, 0.0, 0.0);
		let mut carried = 0.0f32;
		for (joint, weight) in influences.joints.iter().zip(influences.weights) {
			if weight <= 0.0 {
				continue;
			}
			let Some(matrix) = self.matrices.get(*joint as usize) else { continue };
			out = out.add(matrix.transform_point(point).truncate().scale(weight));
			carried += weight;
		}
		if carried <= 0.0 { point } else { out.scale(1.0 / carried) }
	}

	/// The same for a direction, which carries the linear part and not the translation.
	///
	/// THE BLENDED LINEAR PART, WHICH IS WHAT "THE VERTEX STAGE MULTIPLIES AND ADDS" MEANS. The core
	/// profile's inverse-transpose rule is about a drawable's WORLD transform and is applied there;
	/// what a skinned normal needs here is the same blend the position got, or the normal and the
	/// position disagree about which pose the vertex is in.
	pub fn skin_direction(&self, direction: Vec3, influences: &Influences) -> Vec3 {
		let mut out = Vec3::new(0.0, 0.0, 0.0);
		let mut carried = 0.0f32;
		for (joint, weight) in influences.joints.iter().zip(influences.weights) {
			if weight <= 0.0 {
				continue;
			}
			let Some(matrix) = self.matrices.get(*joint as usize) else { continue };
			out = out.add(matrix.transform_direction(direction).scale(weight));
			carried += weight;
		}
		if carried <= 0.0 { direction } else { out.scale(1.0 / carried) }
	}
}

/// One morph target's displacements and how much of it is applied.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct MorphTarget<'a> {
	/// One displacement per vertex, in the REST POSE and in the mesh's own space.
	pub displacement: &'a [Vec3],
	pub weight: f32,
}

/// Check a mesh's morph targets before any of them is applied.
///
/// UP FRONT, ONCE, RATHER THAN PER VERTEX. A target shorter than the mesh is an asset defect, and
/// discovering it at vertex 4,000 leaves the first 3,999 already displaced - so the whole set is
/// admitted or none of it is.
pub fn check_targets(vertices: usize, targets: &[MorphTarget<'_>], limits: &Limits) -> Result<(), Error> {
	let asked = targets.len() as u32;
	if asked > limits.max_morph_targets {
		return Err(Error::LimitExceeded { limit: "max_morph_targets", ceiling: limits.max_morph_targets, asked });
	}
	for target in targets {
		if target.displacement.len() != vertices {
			return Err(Error::Degenerate { reason: "a morph target needs one displacement per vertex" });
		}
		if !target.weight.is_finite() {
			return Err(Error::Degenerate { reason: "a morph target's weight must be finite" });
		}
	}
	Ok(())
}

/// One vertex after its morph targets, before skinning.
///
/// ADDITIVE, WHICH IS WHY TWO TARGETS AT HALF WEIGHT ARE NOT A BLEND BETWEEN THEM. A raised brow and
/// a smile are different displacements of different vertices and applying both at once should do
/// both; a normalising blend would make the second expression undo half of the first.
pub fn morphed(base: Vec3, vertex: usize, targets: &[MorphTarget<'_>]) -> Vec3 {
	let mut out = base;
	for target in targets {
		if target.weight == 0.0 {
			continue;
		}
		let Some(displacement) = target.displacement.get(vertex) else { continue };
		out = out.add(displacement.scale(target.weight));
	}
	out
}

/// The bounds of a mesh IN THE POSE IT IS ACTUALLY IN.
///
/// THIS IS WHAT THE LEVEL-OF-DETAIL RULE MEANS BY "THE CURRENT BOUNDS". A character that raises an
/// arm is larger than its rest pose, and every question asked of its bounds - culling, sorting, and
/// which level of detail it is drawn at - is asked of the pose on screen rather than of the pose it
/// was authored in. A rest-pose bound culls the raised arm at the edge of the frame and steps the
/// character down a level while the arm is still visible.
///
/// IT COSTS A PASS OVER THE VERTICES, which is why it is a function a caller reaches for rather than
/// something the scene does for every drawable: a mesh that does not deform has bounds that do not
/// change, and this is for the ones that do.
pub fn deformed_bounds(base: &[Vec3], targets: &[MorphTarget<'_>], influences: &[Influences], pose: &Pose, limits: &Limits) -> Result<Aabb, Error> {
	if base.is_empty() {
		return Err(Error::Degenerate { reason: "a mesh with no vertices has no bounds" });
	}
	if influences.len() != base.len() {
		return Err(Error::Degenerate { reason: "a skinned mesh needs influences for every vertex" });
	}
	check_targets(base.len(), targets, limits)?;
	let mut minimum = Vec3::new(f32::INFINITY, f32::INFINITY, f32::INFINITY);
	let mut maximum = Vec3::new(f32::NEG_INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
	for (vertex, (position, influence)) in base.iter().zip(influences).enumerate() {
		// MORPH THEN SKIN, which is the profile's order and the reason this function exists in one
		// place rather than being open-coded wherever bounds are wanted.
		let moved = pose.skin_point(morphed(*position, vertex, targets), influence);
		if !moved.is_finite() {
			return Err(Error::Degenerate { reason: "a deformed vertex is not finite" });
		}
		minimum = Vec3::new(minimum.x.min(moved.x), minimum.y.min(moved.y), minimum.z.min(moved.z));
		maximum = Vec3::new(maximum.x.max(moved.x), maximum.y.max(moved.y), maximum.z.max(moved.z));
	}
	Ok(Aabb::new(minimum, maximum))
}
