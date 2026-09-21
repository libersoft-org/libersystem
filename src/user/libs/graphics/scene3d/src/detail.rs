//! LEVEL OF DETAIL: which mesh is drawn, which is the one extended decision that changes the picture
//! on purpose.
//!
//! CULLING IS INVISIBLE AND THIS IS NOT. The core profile can say a culled drawable "must not change
//! the picture", because a drawable outside the frustum contributes nothing either way. A level of
//! detail is a DIFFERENT MESH: it is chosen to be cheaper and it looks different, and the whole
//! question is WHEN. Two implementations swapping levels at different moments on the same scene is a
//! visible difference neither could be said to have got wrong, which is why the thresholds are in
//! the profile rather than in an application's tuning.
//!
//! THE METRIC IS SCREEN COVERAGE AND NOT DISTANCE. Distance picks a different level at the same
//! apparent size whenever the field of view or the viewport changes - a scene tuned at one field of
//! view pops at another, and the same scene on a taller window swaps levels in different places.
//! Coverage is the thing a person actually sees: the bounding sphere's projected radius as a
//! fraction of half the viewport height.
//!
//! AND THE COVERAGE COMES FROM THE CURRENT BOUNDS. `coverage_perspective` takes a `Sphere` rather
//! than a mesh or a drawable exactly so that a rest-pose bound cannot be passed by accident: a
//! character that raises an arm grows its bounds, and a coverage taken from the rest pose steps that
//! arm down a level while it is still on screen.

use alloc::vec::Vec;

use render_math::Vec3;

use crate::bounds::Sphere;
use crate::scene::{Error, Limits};

/// How far past a threshold the coverage has to go before the level held last frame is left.
///
/// A TENTH, AND IT IS WHY A DRAWABLE SITTING ON A BOUNDARY DOES NOT FLICKER. Without it, coverage
/// wandering by a thousandth across a threshold swaps mesh every frame, which reads as a fault
/// rather than as detail. Applied to the level ACTUALLY HELD rather than to the one the ladder would
/// pick, so the widened band belongs to the current level and not to an imaginary one.
pub const HYSTERESIS: f32 = 0.1;

/// The first four default thresholds, each a halving of coverage.
///
/// A HALVING IS A QUARTER OF THE PIXELS, which is the step that makes a ladder worth having: a level
/// that draws nine tenths of the pixels of the one above it costs a level and buys nothing. Longer
/// ladders continue the halving; the profile names these four because `max_lod_levels` is four.
pub const DEFAULT_THRESHOLDS: [f32; 4] = [0.5, 0.25, 0.125, 0.0625];

/// One coarser level: the mesh, and the coverage AT OR BELOW WHICH it takes over.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Level {
	pub mesh: u32,
	pub threshold: f32,
}

/// What the ladder chose.
///
/// THE LEVEL IS READABLE, WHICH IS THE DIFFERENCE FROM CULLING. A culled drawable is invisible in
/// the picture and in the API alike; a level of detail is a decision an application asked for, so it
/// can be asserted rather than inferred from pixels - which is also what lets a conformance scene
/// test the ladder without rendering it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Detail {
	/// Index into the ladder: 0 is the finest, and `levels() - 1` the coarsest.
	Level(u32),
	/// Below the coverage the mesh declared it vanishes at. NOT the same as being culled: the
	/// drawable is on screen and the scene chose not to draw it.
	Vanished,
}

/// A mesh's levels, most detailed first.
///
/// THE FINEST LEVEL CARRIES NO THRESHOLD, and the type says so rather than a comment. A ladder whose
/// first entry had a threshold would carry a number nothing reads - and a number nothing reads is
/// one an author sets, and then wonders why it did nothing.
#[derive(Clone, PartialEq, Debug)]
pub struct Ladder {
	finest: u32,
	coarser: Vec<Level>,
	vanish_below: Option<f32>,
}

impl Ladder {
	/// A mesh with one level: always that level, and no threshold is ever consulted. THIS IS WHAT
	/// MAKES THE FEATURE FREE for the meshes that do not use it.
	pub fn single(mesh: u32) -> Self {
		Self { finest: mesh, coarser: Vec::new(), vanish_below: None }
	}

	/// A ladder with thresholds the caller states.
	///
	/// REFUSED RATHER THAN SORTED if the thresholds do not strictly descend. Sorting them would draw
	/// a scene the author did not write and would hide the authoring error for ever; the refusal
	/// happens at load, which is where a mesh is built rather than where it is drawn.
	pub fn new(finest: u32, coarser: Vec<Level>, limits: &Limits) -> Result<Self, Error> {
		let asked = coarser.len() as u32 + 1;
		if asked > limits.max_lod_levels {
			return Err(Error::LimitExceeded { limit: "max_lod_levels", ceiling: limits.max_lod_levels, asked });
		}
		let mut previous = f32::INFINITY;
		for level in &coarser {
			if !level.threshold.is_finite() || level.threshold <= 0.0 {
				return Err(Error::Degenerate { reason: "a level-of-detail threshold must be finite and above zero" });
			}
			if level.threshold >= previous {
				return Err(Error::Degenerate { reason: "level-of-detail thresholds must strictly descend" });
			}
			previous = level.threshold;
		}
		Ok(Self { finest, coarser, vanish_below: None })
	}

	/// The same ladder with the profile's default thresholds, which is what a mesh that states
	/// levels and no numbers gets.
	pub fn with_default_thresholds(meshes: &[u32], limits: &Limits) -> Result<Self, Error> {
		let Some((finest, rest)) = meshes.split_first() else {
			return Err(Error::Degenerate { reason: "a level-of-detail ladder needs at least one mesh" });
		};
		let mut coarser: Vec<Level> = Vec::with_capacity(rest.len());
		for (index, mesh) in rest.iter().enumerate() {
			// The stated four, then the same halving continued - so a longer ladder is the same rule
			// rather than a second one.
			let threshold = match DEFAULT_THRESHOLDS.get(index) {
				Some(stated) => *stated,
				None => DEFAULT_THRESHOLDS[DEFAULT_THRESHOLDS.len() - 1] / (1u32 << (index + 1 - DEFAULT_THRESHOLDS.len())) as f32,
			};
			coarser.push(Level { mesh: *mesh, threshold });
		}
		Self::new(*finest, coarser, limits)
	}

	/// Declare a coverage below which this mesh is not drawn at all.
	///
	/// THE DEFAULT IS THAT THERE IS NONE. Vanishing is a decision a scene makes about its own
	/// content; a threshold ladder that made it automatically would delete distant geometry an
	/// author wanted kept.
	pub fn vanishing_below(self, coverage: f32) -> Result<Self, Error> {
		if !coverage.is_finite() || coverage <= 0.0 {
			return Err(Error::Degenerate { reason: "a vanishing coverage must be finite and above zero" });
		}
		Ok(Self { vanish_below: Some(coverage), ..self })
	}

	/// How many levels, counting the finest.
	pub fn levels(&self) -> u32 {
		self.coarser.len() as u32 + 1
	}

	/// The mesh at one level, or `None` past the end.
	pub fn mesh(&self, level: u32) -> Option<u32> {
		match level {
			0 => Some(self.finest),
			other => self.coarser.get(other as usize - 1).map(|entry| entry.mesh),
		}
	}

	/// The mesh this choice draws, which is `None` for a vanished drawable.
	pub fn mesh_of(&self, detail: Detail) -> Option<u32> {
		match detail {
			Detail::Level(level) => self.mesh(level),
			Detail::Vanished => None,
		}
	}

	/// The coverage below which a level takes over, or `None` for the finest, which has none.
	pub fn threshold(&self, level: u32) -> Option<f32> {
		(level > 0).then(|| self.coarser.get(level as usize - 1).map(|entry| entry.threshold)).flatten()
	}

	/// Where the ladder alone puts this coverage, with no memory of the last frame.
	fn direct(&self, coverage: f32) -> u32 {
		let mut level = 0u32;
		for (index, entry) in self.coarser.iter().enumerate() {
			// STRICTLY DESCENDING, so the first threshold the coverage is above ends the walk. A
			// scan of the whole list would give the same answer and would also give an answer on a
			// ladder that does not descend, which `new` refuses precisely so this can be short.
			if coverage <= entry.threshold {
				level = index as u32 + 1;
			} else {
				break;
			}
		}
		level
	}

	/// Choose a level for this coverage, holding what was chosen last frame.
	///
	/// `previous` IS WHAT THE LAST FRAME DREW, and `None` means there was no last frame: the ladder
	/// is then read directly with no hysteresis, so a drawable that appears already small starts
	/// small rather than starting detailed and stepping down in view.
	pub fn select(&self, coverage: f32, previous: Option<Detail>) -> Detail {
		if let Some(below) = self.vanish_below {
			// THE SAME TENTH, because the vanishing coverage is a threshold like any other and a
			// drawable sitting on it would otherwise blink.
			let vanished = match previous {
				Some(Detail::Vanished) => coverage <= below * (1.0 + HYSTERESIS),
				Some(Detail::Level(_)) => coverage < below * (1.0 - HYSTERESIS),
				None => coverage < below,
			};
			if vanished {
				return Detail::Vanished;
			}
		}
		let direct = self.direct(coverage);
		let Some(Detail::Level(previous)) = previous else {
			// No last frame, or the last frame drew nothing - either way there is no level to hold.
			return Detail::Level(direct);
		};
		let previous = previous.min(self.levels() - 1);
		if direct == previous {
			return Detail::Level(previous);
		}
		let held = if direct > previous {
			// Going COARSER: the boundary is the threshold of the level immediately below the one
			// held, and it has to be passed by a tenth before it is left.
			self.coarser[previous as usize].threshold * (1.0 - HYSTERESIS) < coverage
		} else {
			// Going FINER: the boundary is the threshold that admitted the level held.
			coverage <= self.coarser[previous as usize - 1].threshold * (1.0 + HYSTERESIS)
		};
		if held { Detail::Level(previous) } else { Detail::Level(direct) }
	}
}

/// The coverage of a sphere under a perspective camera: its projected radius as a fraction of HALF
/// THE VIEWPORT HEIGHT.
///
/// HALF THE HEIGHT RATHER THAN THE WHOLE, because that is what `tan(fov_y / 2)` is the tangent of -
/// so a sphere exactly filling the frame vertically has a coverage of 1, which is the number an
/// author can reason about without knowing this function.
pub fn coverage_perspective(sphere: &Sphere, eye: Vec3, tan_half_fov_y: f32) -> Result<f32, Error> {
	if !sphere.is_well_formed() {
		return Err(Error::Degenerate { reason: "a level-of-detail coverage needs a well-formed bounding sphere" });
	}
	if !tan_half_fov_y.is_finite() || tan_half_fov_y <= 0.0 {
		return Err(Error::Degenerate { reason: "a level-of-detail coverage needs a positive tan(fov_y / 2)" });
	}
	if !eye.is_finite() {
		return Err(Error::Degenerate { reason: "a level-of-detail coverage needs a finite eye position" });
	}
	let distance = sphere.centre.sub(eye).length();
	let denominator = distance * tan_half_fov_y;
	if denominator <= 0.0 {
		// THE EYE IS AT THE CENTRE. A sphere with a radius there fills everything, which is the
		// finest level; a sphere with no radius covers nothing wherever it is.
		return Ok(if sphere.radius > 0.0 { f32::INFINITY } else { 0.0 });
	}
	Ok(sphere.radius / denominator)
}

/// The same under an orthographic camera, where coverage does not depend on distance at all.
pub fn coverage_orthographic(sphere: &Sphere, half_height: f32) -> Result<f32, Error> {
	if !sphere.is_well_formed() {
		return Err(Error::Degenerate { reason: "a level-of-detail coverage needs a well-formed bounding sphere" });
	}
	if !half_height.is_finite() || half_height <= 0.0 {
		return Err(Error::Degenerate { reason: "a level-of-detail coverage needs a positive orthographic half-height" });
	}
	Ok(sphere.radius / half_height)
}

/// One VIEW's memory of what each drawable drew last frame.
///
/// PER VIEW AND NOT PER DRAWABLE, which is the whole reason this type exists rather than a field on
/// `Drawable`. The ladder belongs to the mesh and is the same everywhere; the level chosen from it
/// belongs to the view, because coverage is a function of where the camera is. Two cameras looking
/// at one scene pick different levels for one drawable, and a single cached level would make each
/// view's hysteresis fight the other's - which is a flicker that appears only when a second view
/// exists and is traced to anything but the cache.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ViewDetail {
	held: Vec<(u32, Detail)>,
}

impl ViewDetail {
	pub fn new() -> Self {
		Self { held: Vec::new() }
	}

	/// What this view drew for a drawable last frame, or nothing if it has not seen it.
	pub fn of(&self, drawable: u32) -> Option<Detail> {
		self.held.iter().find(|(index, _)| *index == drawable).map(|(_, detail)| *detail)
	}

	/// Remember what was chosen, replacing what was there.
	pub fn remember(&mut self, drawable: u32, detail: Detail) {
		match self.held.iter_mut().find(|(index, _)| *index == drawable) {
			Some(slot) => slot.1 = detail,
			None => self.held.push((drawable, detail)),
		}
	}

	/// Forget everything, which is what a view does when the scene it was looking at is replaced.
	pub fn clear(&mut self) {
		self.held.clear();
	}

	pub fn len(&self) -> usize {
		self.held.len()
	}

	pub fn is_empty(&self) -> bool {
		self.held.is_empty()
	}
}

/// Choose a level for every drawable that has a ladder, in one camera's view.
///
/// THE COVERAGE COMES FROM THE DRAWABLE'S CURRENT WORLD SPHERE, which is the bound the scene holds
/// NOW - so a mesh whose bounds were recomputed after skinning and morphing is measured in the pose
/// it is actually in, exactly as the profile requires. `deform::deformed_bounds` is what produces
/// that bound; this function does not know or care which kind it was given.
///
/// THE CAMERA'S SHAPE IS READ OUT OF ITS PROJECTION MATRIX and not out of the fields it was built
/// from, for the same reason `cull` extracts its planes that way: a camera may carry a projection
/// its constructors cannot describe, and a coverage computed from a remembered field of view would
/// then be a coverage for a different camera than the one the vertices go through.
pub fn select(scene: &mut crate::scene::Scene, camera: &crate::scene::Camera, state: &mut ViewDetail) -> Result<(), Error> {
	scene.update();
	let projection = *camera.projection();
	// COLUMN-MAJOR: row 1 of column 1 is `cot(fov_y / 2)` for a perspective projection and
	// `1 / half_height` for an orthographic one, and row 3 of column 3 tells the two apart - it is
	// zero when the projection divides by `w` and one when it does not.
	let focal = projection.at(1, 1);
	if !focal.is_finite() || focal == 0.0 {
		return Err(Error::Degenerate { reason: "a projection with no vertical scale has no screen coverage" });
	}
	let orthographic = projection.at(3, 3) != 0.0;
	let Some(eye) = scene.transforms().get(camera.node as usize).map(|world| world.translation()) else {
		return Err(Error::NoSuchNode { node: camera.node });
	};
	for index in 0..scene.drawables().len() {
		let drawable = &scene.drawables()[index];
		let Some(ladder) = drawable.lod.as_ref() else { continue };
		// A DRAWABLE WITH NO BOUNDS HAS NO COVERAGE, and it keeps its finest level rather than being
		// given one at random. The core profile already says an unbounded drawable is never culled;
		// giving it a coarse mesh would be the same disappearance by another route.
		let Some(local) = drawable.bounds else {
			state.remember(index as u32, Detail::Level(0));
			continue;
		};
		let Some(world) = scene.transforms().get(drawable.node as usize).copied() else { continue };
		let sphere = local.world_sphere(&world);
		let coverage = if orthographic { coverage_orthographic(&sphere, 1.0 / focal)? } else { coverage_perspective(&sphere, eye, 1.0 / focal)? };
		let chosen = ladder.select(coverage, state.of(index as u32));
		state.remember(index as u32, chosen);
	}
	Ok(())
}
