//! ANIMATION: keyframed tracks, the curves between them, and what a clip does at its ends.
//!
//! THE INTERPOLATION IS PART OF THE PROFILE AND NOT A QUALITY SETTING. Translation and scale are
//! linear; rotation is SPHERICAL LINEAR along the shorter arc. The cheap alternative - interpolating
//! the quaternion's four components linearly and normalising - is one line shorter and produces a
//! visibly uneven rotation speed: the same turn accelerates through its middle and crawls at its
//! ends. Two renderers that chose differently animate the same clip differently, which is why the
//! choice is stated rather than left open.
//!
//! AND THE MODE IS PER TRACK, NOT PER CLIP. A visibility flag wants STEP, a slide wants LINEAR and a
//! bounce wants CUBIC; forcing one mode on a whole clip is what makes an author fake the others with
//! extra keys, and a faked curve is one nothing can retime.
//!
//! ROOT MOTION IS EXTRACTED BY DEFAULT. A walk cycle authored with the character moving forward
//! carries that motion in its root joint; an application that plays the clip without asking for the
//! motion should get a character walking on the spot, not one drifting away from where the game put
//! it. So the root's translation is REMOVED from the pose and handed back as a delta, and a clip
//! that wants it kept says so.
//!
//! A LOOPING CLIP'S SEAM IS CHECKED AT LOAD. Its last keyframe has to equal its first, within the
//! profile's threshold, or the clip is refused - because a seam that does not close pops once a
//! cycle, for ever, and it is found by watching rather than by testing.

use alloc::vec::Vec;

use render_math::{Quat, Vec3};

use crate::scene::{Error, Limits};

/// How far apart the ends of a looping clip may be. See the profile's "a clip's loop seam".
pub const LOOP_SEAM_TOLERANCE: f32 = 1e-5;

/// One keyframe of a step or linear curve.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Key<T> {
	pub time: f32,
	pub value: T,
}

/// One keyframe of a cubic curve, with the two tangents the Hermite form needs.
///
/// THE TANGENTS ARE AUTHORED AND NOT DERIVED, which is why the form is Hermite rather than
/// Catmull-Rom: a derived tangent changes when a NEIGHBOURING key moves, so editing one key alters a
/// curve three keys away and an author cannot fix a bounce without breaking the landing.
///
/// THEY ARE IN THE VALUE'S UNITS PER SECOND, and the span scales them inside the equation. Stored
/// per span instead, a clip retimed by changing its duration would change SHAPE rather than speed.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CubicKey<T> {
	pub time: f32,
	pub value: T,
	pub in_tangent: T,
	pub out_tangent: T,
}

/// One channel's keys, and the curve that joins them.
#[derive(Clone, PartialEq, Debug)]
pub enum Curve<T> {
	/// Holds the PREVIOUS key's value until the next key's time is reached, so the value changes
	/// exactly at the keyframes and nowhere else.
	Step(Vec<Key<T>>),
	Linear(Vec<Key<T>>),
	Cubic(Vec<CubicKey<T>>),
}

impl<T: Copy> Curve<T> {
	pub fn len(&self) -> usize {
		match self {
			Curve::Step(keys) | Curve::Linear(keys) => keys.len(),
			Curve::Cubic(keys) => keys.len(),
		}
	}

	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}

	fn time(&self, index: usize) -> Option<f32> {
		match self {
			Curve::Step(keys) | Curve::Linear(keys) => keys.get(index).map(|key| key.time),
			Curve::Cubic(keys) => keys.get(index).map(|key| key.time),
		}
	}

	fn value(&self, index: usize) -> Option<T> {
		match self {
			Curve::Step(keys) | Curve::Linear(keys) => keys.get(index).map(|key| key.value),
			Curve::Cubic(keys) => keys.get(index).map(|key| key.value),
		}
	}
}

/// What a track drives.
#[derive(Clone, PartialEq, Debug)]
pub enum Channel {
	Translation(Curve<Vec3>),
	Rotation(Curve<Quat>),
	Scale(Curve<Vec3>),
	/// ONE morph target's weight. NOT normalised across targets, because morph targets are additive:
	/// normalising them would make a second expression undo half of the first.
	MorphWeight(Curve<f32>),
}

/// What a track is addressed to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
	Joint(u16),
	Morph(u16),
}

/// One track: what it drives and the channel it drives it with.
#[derive(Clone, PartialEq, Debug)]
pub struct Track {
	pub target: Target,
	pub channel: Channel,
}

/// What a clip does at its ends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ending {
	/// Holds the first key before the clip and the last after it.
	Clamp,
	/// Wraps, and the seam has to close.
	Loop,
	/// Forward then backward, with a period of TWICE the duration.
	PingPong,
}

/// A joint's sampled transform.
///
/// THE THREE ARE SEPARATE AND ARE COMPOSED BY THE CALLER in the profile's order - translation,
/// rotation, scale - because composing them here would hide that order inside a sampler, where the
/// hierarchy rule that fixes it could not be read.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Sampled {
	pub translation: Option<Vec3>,
	pub rotation: Option<Quat>,
	pub scale: Option<Vec3>,
}

impl Sampled {
	pub const NOTHING: Self = Self { translation: None, rotation: None, scale: None };
}

/// One frame's worth of a clip.
///
/// NAMED FOR THE THRESHOLD THAT MEASURES IT - the profile's "a sampled pose" - and deliberately not
/// `Pose`, which in this crate is the SKELETON's composed joint matrices. The two are a stage apart:
/// this is what a clip says the joints should be, and `deform::Pose` is what the hierarchy made of
/// it after composing and applying the inverse bind.
#[derive(Clone, PartialEq, Debug)]
pub struct SampledPose {
	joints: Vec<(u16, Sampled)>,
	morphs: Vec<(u16, f32)>,
	root_delta: Vec3,
}

impl SampledPose {
	pub fn empty() -> Self {
		Self { joints: Vec::new(), morphs: Vec::new(), root_delta: Vec3::new(0.0, 0.0, 0.0) }
	}

	/// What this pose says about one joint, or nothing if no track drives it.
	pub fn joint(&self, joint: u16) -> Option<&Sampled> {
		self.joints.iter().find(|(named, _)| *named == joint).map(|(_, sampled)| sampled)
	}

	/// What this pose says about one morph target's weight.
	pub fn morph(&self, target: u16) -> Option<f32> {
		self.morphs.iter().find(|(named, _)| *named == target).map(|(_, weight)| *weight)
	}

	pub fn joints(&self) -> impl Iterator<Item = (u16, &Sampled)> {
		self.joints.iter().map(|(joint, sampled)| (*joint, sampled))
	}

	pub fn morphs(&self) -> impl Iterator<Item = (u16, f32)> + '_ {
		self.morphs.iter().map(|(target, weight)| (*target, *weight))
	}

	/// The root's translation, removed from the pose and handed back here.
	///
	/// ZERO WHEN THE CLIP KEEPS ITS ROOT MOTION, because then the motion is still in the pose and
	/// handing it back as well would apply it twice.
	pub fn root_delta(&self) -> Vec3 {
		self.root_delta
	}

	fn slot(&mut self, joint: u16) -> usize {
		match self.joints.iter().position(|(named, _)| *named == joint) {
			Some(index) => index,
			None => {
				self.joints.push((joint, Sampled::NOTHING));
				self.joints.len() - 1
			}
		}
	}
}

/// A clip: tracks, a duration, and what it does at its ends.
#[derive(Clone, PartialEq, Debug)]
pub struct Clip {
	tracks: Vec<Track>,
	duration: f32,
	ending: Ending,
	root: u16,
	keeps_root_motion: bool,
}

impl Clip {
	/// Build a clip, checking everything that can be checked once.
	///
	/// EVERY REFUSAL HERE IS ONE THAT WOULD OTHERWISE BE A PICTURE. Unordered keys sample the wrong
	/// value, a key outside the duration is one nothing will ever reach, and a loop seam that does
	/// not close pops once a cycle - none of the three is visible in the data and all three are
	/// visible on screen.
	pub fn new(tracks: Vec<Track>, duration: f32, ending: Ending, root: u16, keeps_root_motion: bool, limits: &Limits) -> Result<Self, Error> {
		let asked = tracks.len() as u32;
		if asked > limits.max_animation_tracks {
			return Err(Error::LimitExceeded { limit: "max_animation_tracks", ceiling: limits.max_animation_tracks, asked });
		}
		if !duration.is_finite() || duration <= 0.0 {
			return Err(Error::Degenerate { reason: "a clip's duration must be finite and above zero" });
		}
		// A LOOP NEEDS A CLOSED SEAM AND A PING-PONG DOES NOT, because a ping-pong's ends meet
		// themselves: it turns at its last key rather than jumping back to its first.
		let closed = matches!(ending, Ending::Loop);
		for track in &tracks {
			match &track.channel {
				Channel::Translation(curve) | Channel::Scale(curve) => check_curve(curve, duration, closed)?,
				Channel::Rotation(curve) => check_curve(curve, duration, closed)?,
				Channel::MorphWeight(curve) => check_curve(curve, duration, closed)?,
			}
		}
		Ok(Self { tracks, duration, ending, root, keeps_root_motion })
	}

	pub fn duration(&self) -> f32 {
		self.duration
	}

	pub fn ending(&self) -> Ending {
		self.ending
	}

	pub fn keeps_root_motion(&self) -> bool {
		self.keeps_root_motion
	}

	pub fn tracks(&self) -> usize {
		self.tracks.len()
	}

	/// Where in the clip a time lands.
	///
	/// A LOOP WRAPS, A ONE-SHOT CLAMPS AND A PING-PONG TURNS, and all three are decisions rather
	/// than conveniences: a one-shot that wrapped would restart a death animation, a loop that
	/// clamped would freeze on its last frame, and a ping-pong that held its turning frame for two
	/// frames would stutter at both ends once a cycle.
	pub fn at(&self, time: f32) -> f32 {
		if !time.is_finite() {
			return 0.0;
		}
		match self.ending {
			Ending::Clamp => time.clamp(0.0, self.duration),
			Ending::Loop => wrap(time, self.duration),
			Ending::PingPong => {
				// THE PERIOD IS TWICE THE DURATION, and the turn is AT the end key: at a phase of
				// exactly `duration` the time is `duration`, and just past it the time is just
				// below - so the turning frame is visited once per period rather than held.
				let phase = wrap(time, 2.0 * self.duration);
				if phase <= self.duration { phase } else { 2.0 * self.duration - phase }
			}
		}
	}

	/// Sample every track at one time.
	pub fn sample(&self, time: f32) -> SampledPose {
		let at = self.at(time);
		let mut pose = SampledPose::empty();
		for track in &self.tracks {
			match (&track.target, &track.channel) {
				(Target::Joint(joint), Channel::Translation(curve)) => {
					let slot = pose.slot(*joint);
					pose.joints[slot].1.translation = sample_curve(curve, at);
				}
				(Target::Joint(joint), Channel::Scale(curve)) => {
					let slot = pose.slot(*joint);
					pose.joints[slot].1.scale = sample_curve(curve, at);
				}
				(Target::Joint(joint), Channel::Rotation(curve)) => {
					let slot = pose.slot(*joint);
					pose.joints[slot].1.rotation = sample_curve(curve, at);
				}
				(Target::Morph(target), Channel::MorphWeight(curve)) => {
					if let Some(weight) = sample_curve(curve, at) {
						pose.morphs.push((*target, weight));
					}
				}
				// A TRACK ADDRESSED TO THE WRONG KIND OF TARGET DRIVES NOTHING. It is not an error
				// the frame should stop for: a morph weight on a joint says nothing about that
				// joint, and the rest of the clip is still what the author wrote.
				_ => {}
			}
		}
		// EXTRACTED BY DEFAULT: the root's translation leaves the pose and becomes the delta, so an
		// application that does not ask for root motion gets a character that does not drift.
		if !self.keeps_root_motion {
			if let Some((_, sampled)) = pose.joints.iter_mut().find(|(joint, _)| *joint == self.root) {
				if let Some(translation) = sampled.translation.take() {
					pose.root_delta = translation;
				}
			}
		}
		pose
	}
}

/// A weighted blend of two sampled poses, which is what a cross-fade is made of.
///
/// `weight` IS HOW MUCH OF `later`, so zero is `earlier` unchanged and one is `later` unchanged. A
/// cross-fade is this with the weight moving over time, which is why there is one function rather
/// than a separate cross-fade: two rules would be two behaviours at the ends.
///
/// A TARGET ONLY ONE POSE DRIVES IS TAKEN FROM IT UNCHANGED, AT FULL VALUE. The other clip says
/// NOTHING about that joint, which is not the same as saying it should be at rest; scaling it by its
/// clip's weight would pull the joint toward the origin as the blend moves away, which is a limb
/// collapsing rather than a blend.
pub fn blend(earlier: &SampledPose, later: &SampledPose, weight: f32) -> SampledPose {
	let weight = weight.clamp(0.0, 1.0);
	let mut out = SampledPose::empty();
	for (joint, from) in earlier.joints() {
		let slot = out.slot(joint);
		out.joints[slot].1 = match later.joint(joint) {
			Some(to) => mix(from, to, weight),
			None => *from,
		};
	}
	for (joint, to) in later.joints() {
		if earlier.joint(joint).is_none() {
			let slot = out.slot(joint);
			out.joints[slot].1 = *to;
		}
	}
	for (target, from) in earlier.morphs() {
		let value = match later.morph(target) {
			Some(to) => from + (to - from) * weight,
			None => from,
		};
		out.morphs.push((target, value));
	}
	for (target, to) in later.morphs() {
		if earlier.morph(target).is_none() {
			out.morphs.push((target, to));
		}
	}
	// ROOT MOTION IS THE SAME WEIGHTED SUM. A cross-fade from a walk to a run then produces a speed
	// BETWEEN them; adding the two deltas would make the character briefly move faster than either
	// clip ever asks for.
	out.root_delta = earlier.root_delta().lerp(later.root_delta(), weight);
	out
}

fn mix(from: &Sampled, to: &Sampled, weight: f32) -> Sampled {
	Sampled {
		translation: pick(from.translation, to.translation, |a, b| a.lerp(b, weight)),
		// SPHERICAL, ALONG THE SHORTER ARC, exactly as within one clip.
		rotation: pick(from.rotation, to.rotation, |a, b| a.slerp(b, weight)),
		scale: pick(from.scale, to.scale, |a, b| a.lerp(b, weight)),
	}
}

fn pick<T: Copy>(from: Option<T>, to: Option<T>, blend: impl Fn(T, T) -> T) -> Option<T> {
	match (from, to) {
		(Some(a), Some(b)) => Some(blend(a, b)),
		(Some(a), None) => Some(a),
		(None, Some(b)) => Some(b),
		(None, None) => None,
	}
}

/// A time folded into `[0, period)`.
///
/// WITHOUT `%`, WHICH IS A LIBRARY CALL HERE AND NOT AN INSTRUCTION. The float remainder operator
/// lowers to `fmodf`, and this tree provides no libm - a `no_std` library that reached for one would
/// make every consumer of this crate link it. Truncating toward zero through an integer is the same
/// operation `fmodf` performs and is a couple of instructions; the conversion SATURATES rather than
/// wrapping, so a time far past what an `i64` holds clamps instead of landing somewhere arbitrary.
fn wrap(time: f32, period: f32) -> f32 {
	let whole = (time / period) as i64 as f32;
	let wrapped = time - whole * period;
	if wrapped < 0.0 { wrapped + period } else { wrapped }
}

/// What a curve's values can do between two keys.
///
/// A PRIVATE TRAIT SO THE SPAN LOGIC IS WRITTEN ONCE. Finding the span, holding the ends and scaling
/// the tangents are the same for a translation, a rotation and a morph weight; three copies of them
/// would be three places for an off-by-one at the last key.
trait Interpolate: Copy {
	fn blend(from: Self, to: Self, at: f32) -> Self;
	fn hermite(from: Self, out_of: Self, to: Self, into: Self, at: f32, span: f32) -> Self;
	fn finite(self) -> bool;
	/// How far apart two values of this channel are, in the channel's OWN terms - which is what
	/// lets one loop-seam check serve a translation, a rotation and a scalar.
	fn seam(self, other: Self) -> f32;
}

impl Interpolate for Vec3 {
	fn blend(from: Self, to: Self, at: f32) -> Self {
		from.lerp(to, at)
	}

	fn hermite(from: Self, out_of: Self, to: Self, into: Self, at: f32, span: f32) -> Self {
		let (h00, h10, h01, h11) = hermite_basis(at, span);
		from.scale(h00).add(out_of.scale(h10)).add(to.scale(h01)).add(into.scale(h11))
	}

	fn finite(self) -> bool {
		self.is_finite()
	}

	fn seam(self, other: Self) -> f32 {
		self.sub(other).length()
	}
}

impl Interpolate for f32 {
	fn blend(from: Self, to: Self, at: f32) -> Self {
		from + (to - from) * at
	}

	fn hermite(from: Self, out_of: Self, to: Self, into: Self, at: f32, span: f32) -> Self {
		let (h00, h10, h01, h11) = hermite_basis(at, span);
		from * h00 + out_of * h10 + to * h01 + into * h11
	}

	fn finite(self) -> bool {
		self.is_finite()
	}

	fn seam(self, other: Self) -> f32 {
		let apart = self - other;
		if apart < 0.0 { -apart } else { apart }
	}
}

impl Interpolate for Quat {
	fn blend(from: Self, to: Self, at: f32) -> Self {
		from.slerp(to, at)
	}

	/// COMPONENT-WISE AND THEN NORMALISED, WHICH IS NOT A CONTRADICTION OF THE LINEAR RULE. A cubic
	/// through four control points has no spherical form at all, so the choice here is between a
	/// component-wise cubic and no cubic - unlike the linear case, where a spherical form exists and
	/// the profile therefore refuses the cheap one.
	fn hermite(from: Self, out_of: Self, to: Self, into: Self, at: f32, span: f32) -> Self {
		let (h00, h10, h01, h11) = hermite_basis(at, span);
		let x = from.x * h00 + out_of.x * h10 + to.x * h01 + into.x * h11;
		let y = from.y * h00 + out_of.y * h10 + to.y * h01 + into.y * h11;
		let z = from.z * h00 + out_of.z * h10 + to.z * h01 + into.z * h11;
		let w = from.w * h00 + out_of.w * h10 + to.w * h01 + into.w * h11;
		Quat::from_components(x, y, z, w).unwrap_or(from)
	}

	fn finite(self) -> bool {
		self.is_finite()
	}

	/// `1 - |dot|`, AND THE ABSOLUTE DOT IS THE POINT: `q` and `-q` are ONE rotation, so a seam
	/// compared without it would refuse a clip for a full turn the author never wrote.
	fn seam(self, other: Self) -> f32 {
		let dot = self.x * other.x + self.y * other.y + self.z * other.z + self.w * other.w;
		1.0 - if dot < 0.0 { -dot } else { dot }
	}
}

/// The four Hermite basis weights, with the tangent ones already scaled by the span.
fn hermite_basis(at: f32, span: f32) -> (f32, f32, f32, f32) {
	let t2 = at * at;
	let t3 = t2 * at;
	(2.0 * t3 - 3.0 * t2 + 1.0, (t3 - 2.0 * t2 + at) * span, -2.0 * t3 + 3.0 * t2, (t3 - t2) * span)
}

/// Key times must ascend strictly, lie inside the clip, and there must be some.
fn check_curve<T: Interpolate>(curve: &Curve<T>, duration: f32, closed: bool) -> Result<(), Error> {
	let count = curve.len();
	if count == 0 {
		return Err(Error::Degenerate { reason: "a track with no keyframes drives nothing" });
	}
	let mut previous = f32::NEG_INFINITY;
	for index in 0..count {
		let time = curve.time(index).unwrap_or(f32::NAN);
		if !time.is_finite() || time < 0.0 || time > duration {
			return Err(Error::Degenerate { reason: "a keyframe's time must lie inside the clip" });
		}
		if time <= previous {
			// STRICTLY ASCENDING. Two keys at one time make the value at that instant depend on
			// which one the sampler happened to find first.
			return Err(Error::Degenerate { reason: "a track's keyframes must ascend in time" });
		}
		previous = time;
		let value = curve.value(index).unwrap_or(curve.value(0).expect("a non-empty curve"));
		if !value.finite() {
			return Err(Error::Degenerate { reason: "a keyframe value must be finite" });
		}
	}
	if closed {
		let (Some(first), Some(last)) = (curve.value(0), curve.value(count - 1)) else {
			return Err(Error::Degenerate { reason: "a track with no keyframes drives nothing" });
		};
		if !closes(first, last) {
			return Err(Error::Degenerate { reason: "a looping clip's last keyframe must equal its first" });
		}
	}
	Ok(())
}

/// Whether a looping track's two ends meet, within the profile's threshold.
///
/// EACH CHANNEL IS MEASURED IN ITS OWN TERMS, which `Interpolate::seam` is for: a distance for a
/// translation or a scale, a magnitude for a scalar weight, and `1 - |dot|` for a rotation - where
/// the ABSOLUTE dot is what makes `q` and `-q` one rotation rather than a full turn apart.
fn closes<T: Interpolate>(first: T, last: T) -> bool {
	first.seam(last) <= LOOP_SEAM_TOLERANCE
}

/// One sample of a curve at a time already folded into the clip.
///
/// BEFORE THE FIRST KEY AND AFTER THE LAST, THE END VALUE HOLDS. A track that began at one second
/// and extrapolated backwards would put the joint somewhere the author never authored.
fn sample_curve<T: Interpolate>(curve: &Curve<T>, at: f32) -> Option<T> {
	let count = curve.len();
	if count == 0 {
		return None;
	}
	let first = curve.time(0)?;
	if at <= first {
		return curve.value(0);
	}
	let last = curve.time(count - 1)?;
	if at >= last {
		return curve.value(count - 1);
	}
	let mut index = 0usize;
	while index + 1 < count && curve.time(index + 1).unwrap_or(f32::INFINITY) <= at {
		index += 1;
	}
	let (before, after) = (index, index + 1);
	let start = curve.time(before)?;
	let end = curve.time(after)?;
	let span = end - start;
	let fraction = if span > 0.0 { (at - start) / span } else { 0.0 };
	match curve {
		// STEP HOLDS THE PREVIOUS KEY until the next key's time is REACHED, which the early return
		// above already gave: at `end` exactly, `at >= last` or the walk moves on.
		Curve::Step(keys) => Some(keys[before].value),
		Curve::Linear(keys) => Some(T::blend(keys[before].value, keys[after].value, fraction)),
		Curve::Cubic(keys) => Some(T::hermite(keys[before].value, keys[before].out_tangent, keys[after].value, keys[after].in_tangent, fraction, span)),
	}
}

/// Which node of the scene each joint drives.
///
/// A CLIP TALKS ABOUT JOINTS AND A SCENE IS MADE OF NODES, and the two numberings are not the same
/// one: a skeleton's joint 0 is whichever node the asset's rig put first, and a scene holds cameras,
/// lights and geometry in the same table. Keeping the map here rather than assuming `joint == node`
/// is what lets one clip drive two characters that were built at different times.
#[derive(Clone, PartialEq, Debug)]
pub struct Skeleton {
	nodes: Vec<u32>,
}

impl Skeleton {
	pub fn new(nodes: Vec<u32>, limits: &Limits) -> Result<Self, Error> {
		let asked = nodes.len() as u32;
		if asked > limits.max_skeleton_joints {
			return Err(Error::LimitExceeded { limit: "max_skeleton_joints", ceiling: limits.max_skeleton_joints, asked });
		}
		if nodes.is_empty() {
			return Err(Error::Degenerate { reason: "a skeleton with no joints drives nothing" });
		}
		Ok(Self { nodes })
	}

	pub fn node_of(&self, joint: u16) -> Option<u32> {
		self.nodes.get(joint as usize).copied()
	}

	pub fn joints(&self) -> u32 {
		self.nodes.len() as u32
	}
}

/// Apply a sampled pose to a scene's nodes.
///
/// A CHANNEL THE CLIP DOES NOT DRIVE IS LEFT ALONE, which is the property this function exists for.
/// Writing an identity into it instead would make a clip that only ROTATES a wrist also move that
/// wrist to the origin and scale it to nothing - and the clip would look correct in isolation and
/// destroy any pose it was blended into.
///
/// A JOINT THE SKELETON DOES NOT MAP IS REFUSED rather than skipped, because a clip that names a
/// joint the rig does not have is an asset mismatch: skipping it animates part of a character and
/// leaves the rest in its bind pose, which reads as a broken rig rather than as a mismatched pair.
pub fn apply(scene: &mut crate::scene::Scene, skeleton: &Skeleton, pose: &SampledPose) -> Result<(), Error> {
	for (joint, sampled) in pose.joints() {
		let Some(node) = skeleton.node_of(joint) else {
			return Err(Error::Degenerate { reason: "a clip drives a joint this skeleton does not map" });
		};
		if let Some(translation) = sampled.translation {
			scene.set_translation(node, translation)?;
		}
		if let Some(rotation) = sampled.rotation {
			scene.set_rotation(node, rotation)?;
		}
		if let Some(scale) = sampled.scale {
			scene.set_scale(node, scale)?;
		}
	}
	Ok(())
}
