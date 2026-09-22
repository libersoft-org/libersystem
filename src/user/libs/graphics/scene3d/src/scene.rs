//! THE SCENE: a hierarchy, and the things hung on it.
//!
//! A NODE IS ONE KIND OF THING. A transform with no drawable is drawn as nothing, rather than being
//! a different kind of object with its own list - which is what lets a group be turned into a
//! drawable, or the reverse, without re-parenting anything.
//!
//! THE LOCAL TRANSFORM IS TRANSLATION * ROTATION * SCALE, and the order is stated because the three
//! do not commute: scaling after rotating shears, and a scene authored under one order looks wrong
//! under the other.
//!
//! THE ROTATION IS A UNIT QUATERNION AND IS NORMALISED ON COMPOSITION. Euler angles are ambiguous in
//! their order; a matrix drifts away from orthonormal as it is composed, which shows up as a child
//! that slowly shears.
//!
//! WORLD TRANSFORMS ARE COMPUTED LAZILY, ONCE PER FRAME, PER DIRTY SUBTREE, IN ONE TRAVERSAL. The
//! cost of the bookkeeping is one boolean per node and the propagation is the traversal itself: a
//! node is recomputed when it is dirty OR its parent was recomputed, and the traversal order is
//! topological, so a parent is always visited first. That is what makes "a parent's change dirties
//! its whole subtree" true by construction rather than by a propagation that can be missed - and a
//! missed propagation is an object that stays where it was, which is correct until something moves.
//!
//! A CYCLE IS REFUSED AT THE EDGE THAT WOULD CREATE IT, and so is a hierarchy deeper than the
//! profile's limit. Discovering either during a traversal means the traversal is already in an
//! infinite loop or has already overflowed a stack.

use alloc::vec;
use alloc::vec::Vec;

use render_math::{Mat4, Quat, Vec3};

use crate::bounds::Aabb;
use crate::light::Light;
use crate::material::{ExtendedMaterial, Material, Shading};

/// Which layers a camera sees. A bitmask, ANDed with the camera's own and with every ancestor's.
///
/// A MASK AND NOT A LAYER NUMBER, because an object belongs to more than one: a piece of geometry is
/// in the main view and the picking pass and not in the reflection, which one number cannot say.
pub type VisibilityMask = u32;

/// A drawable's picking identity. ZERO IS RESERVED FOR NOTHING, so a pick on the background is
/// unambiguous rather than being drawable zero.
pub type DrawableId = u32;

/// Why a scene operation was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
	/// A node index nothing declared.
	NoSuchNode { node: u32 },
	/// A material index nothing declared.
	NoSuchMaterial { material: u32 },
	/// An Extended material index nothing declared. NAMED SEPARATELY FROM THE CORE ONE, because the
	/// two tables carry their own indices and a refusal that said only "no such material" would send
	/// a reader to the wrong list.
	NoSuchExtendedMaterial { material: u32 },
	/// An operation from `Scene3D Extended Profile 1` on a scene whose limits do not claim it. THE
	/// PART IS ENTIRE OR ABSENT, and a scene that had acquired one Extended material would hold half
	/// of a part the six limits say a layer has whole or not at all.
	NotExtended { operation: &'static str },
	/// A drawable index nothing declared.
	NoSuchDrawable { drawable: u32 },
	/// A parenting that would make a node its own ancestor. REFUSED AT THE EDGE, so the scene is
	/// always a forest.
	WouldCycle { node: u32, parent: u32 },
	/// A hierarchy deeper than the profile's limit, refused when the parent is set - so a traversal
	/// needs no depth counter and cannot overflow a stack.
	TooDeep { node: u32, ceiling: u32, asked: u32 },
	/// More nodes, lights, materials, cameras, drawables or instances than the profile admits. A
	/// layer without a bound is one whose worst case is a caller's loop.
	LimitExceeded { limit: &'static str, ceiling: u32, asked: u32 },
	/// A pass graph whose dependencies cycle.
	PassCycle { pass: u32 },
	/// A pass that reads a target nothing writes.
	NoSuchTarget { target: u32 },
	/// Two drawables claiming one picking identity, which would make a pick ambiguous.
	DuplicateId { id: DrawableId },
	/// A pick at a pixel the attachment does not have. REFUSED RATHER THAN CLAMPED: a clamped pick
	/// answers about a pixel the caller did not ask about.
	OutsideAttachment { x: u32, y: u32, width: u32, height: u32 },
	/// A matrix, a projection, a bound or a viewport the arithmetic cannot use: a zero scale, a far
	/// plane at or below near, an inverted box, a non-finite value. NAMED SEPARATELY FROM A MISSING
	/// NODE, because the thing exists and the arithmetic is what failed.
	Degenerate { reason: &'static str },
}

/// What the scene admits, with the names `Scene3D Core Profile 1` gives them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
	pub max_nodes: u32,
	pub max_hierarchy_depth: u32,
	pub max_drawables: u32,
	pub max_instances_per_drawable: u32,
	pub max_lights: u32,
	pub max_lights_per_drawable: u32,
	pub max_materials: u32,
	pub max_cameras: u32,
	/// THE EXTENDED PROFILE'S LIMITS, AND ZERO MEANS THIS SCENE DOES NOT CLAIM IT.
	///
	/// `Scene3D Extended Profile 1` is a SEPARATELY ACTIVATED part with its own closed list and its
	/// own hash, and its own rule: a build claims it entirely or not at all. Zero is what "not at
	/// all" looks like from here - every Extended operation is refused against it by the same
	/// `LimitExceeded` every core one uses, so a scene that never asked for the part cannot acquire
	/// half of it by accident.
	///
	/// THE SEVEN ARE THE SEVEN THE FREEZE NAMES, AND THE SEVENTH ARRIVED BY AMENDMENT.
	///
	/// Six of them were frozen with the document. `max_lod_levels` joined them when the part was
	/// activated and its first deliverable - the closed enumerated feature list - was written: level
	/// of detail had no stated rule anywhere in the profile, so the item asking for "LOD selection
	/// with stated thresholds" could only have been implemented against an invented one. The rules
	/// and the limit went in together, the specification hash moved, and the amendment is recorded
	/// where the part is. That is the loud version of the act the next paragraph refuses quietly.
	///
	/// `max_transparent_items` is STILL not among them although this part's item text mentions it,
	/// and that is deliberate rather than an omission: a transparent item IS a drawable, which
	/// `max_drawables` already bounds. Moving a frozen hash is for a rule the profile is missing,
	/// not for a second name over a bound that already exists.
	pub max_shadow_cascades: u32,
	pub max_shadow_maps: u32,
	pub max_skeleton_joints: u32,
	pub max_morph_targets: u32,
	pub max_animation_tracks: u32,
	pub environment_prefilter_levels: u32,
	pub max_lod_levels: u32,
}

impl Limits {
	/// The profile's minimums, as a scene that only just conforms. THE FLOOR AND NOT A
	/// RECOMMENDATION - a fixture holds each field against the frozen list by name.
	pub const PROFILE_MINIMUM: Self = Self {
		max_nodes: 65_536,
		max_hierarchy_depth: 64,
		max_drawables: 16_384,
		max_instances_per_drawable: 4_096,
		max_lights: 256,
		max_lights_per_drawable: 8,
		max_materials: 4_096,
		max_cameras: 8,
		// CORE CLAIMS NOTHING EXTENDED, which is what these zeros say - see the field notes above.
		max_shadow_cascades: 0,
		max_shadow_maps: 0,
		max_skeleton_joints: 0,
		max_morph_targets: 0,
		max_animation_tracks: 0,
		environment_prefilter_levels: 0,
		max_lod_levels: 0,
	};

	/// The same scene claiming `Scene3D Extended Profile 1` as well, at that profile's own floor.
	///
	/// THE CORE MINIMUMS ARE UNCHANGED BY IT. Extended is additive: a build that claims it still
	/// admits exactly what a core-conforming one does, and the seven numbers below are the extra
	/// things it now admits rather than a different scene.
	pub const EXTENDED_MINIMUM: Self = Self { max_shadow_cascades: 4, max_shadow_maps: 8, max_skeleton_joints: 128, max_morph_targets: 32, max_animation_tracks: 256, environment_prefilter_levels: 6, max_lod_levels: 4, ..Self::PROFILE_MINIMUM };

	/// Whether this scene claims the Extended profile - which it does only by admitting ALL of it.
	///
	/// ENTIRELY OR NOT AT ALL IS THE PART'S OWN RULE, and a scene carrying four of the seven limits
	/// is exactly what that rule refuses: an application would find shadows and no skinning, with
	/// nothing anywhere saying which half it had.
	pub fn claims_extended(&self) -> bool {
		self.max_shadow_cascades > 0 && self.max_shadow_maps > 0 && self.max_skeleton_joints > 0 && self.max_morph_targets > 0 && self.max_animation_tracks > 0 && self.environment_prefilter_levels > 0 && self.max_lod_levels > 0
	}

	/// The value the profile's own name refers to, so a fixture can compare the two lists by name
	/// rather than by position.
	pub fn by_name(&self, name: &str) -> Option<u32> {
		Some(match name {
			"max_nodes" => self.max_nodes,
			"max_hierarchy_depth" => self.max_hierarchy_depth,
			"max_drawables" => self.max_drawables,
			"max_instances_per_drawable" => self.max_instances_per_drawable,
			"max_lights" => self.max_lights,
			"max_lights_per_drawable" => self.max_lights_per_drawable,
			"max_materials" => self.max_materials,
			"max_cameras" => self.max_cameras,
			"max_shadow_cascades" => self.max_shadow_cascades,
			"max_shadow_maps" => self.max_shadow_maps,
			"max_skeleton_joints" => self.max_skeleton_joints,
			"max_morph_targets" => self.max_morph_targets,
			"max_animation_tracks" => self.max_animation_tracks,
			"environment_prefilter_levels" => self.environment_prefilter_levels,
			"max_lod_levels" => self.max_lod_levels,
			_ => return None,
		})
	}
}

/// One node of the hierarchy.
///
/// THE FIELDS ARE READ THROUGH ACCESSORS AND WRITTEN THROUGH THE SCENE, because only the scene owns
/// the dirty flags. A public field would let a transform change without its subtree being marked,
/// which is the propagation defect this layer exists to make impossible.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Node {
	parent: Option<u32>,
	translation: Vec3,
	rotation: Quat,
	scale: Vec3,
	visibility: VisibilityMask,
	enabled: bool,
	/// How far below a root this node is. Cached so `max_hierarchy_depth` is a check at the edge.
	depth: u32,
}

impl Node {
	pub fn identity() -> Self {
		Self { parent: None, translation: Vec3::ZERO, rotation: Quat::IDENTITY, scale: Vec3::new(1.0, 1.0, 1.0), visibility: u32::MAX, enabled: true, depth: 0 }
	}

	pub fn with_parent(self, parent: u32) -> Self {
		Self { parent: Some(parent), ..self }
	}

	pub fn with_translation(self, translation: Vec3) -> Self {
		Self { translation, ..self }
	}

	pub fn with_rotation(self, rotation: Quat) -> Self {
		Self { rotation, ..self }
	}

	pub fn with_scale(self, scale: Vec3) -> Self {
		Self { scale, ..self }
	}

	pub fn with_visibility(self, visibility: VisibilityMask) -> Self {
		Self { visibility, ..self }
	}

	pub fn disabled(self) -> Self {
		Self { enabled: false, ..self }
	}

	pub fn parent(&self) -> Option<u32> {
		self.parent
	}

	pub fn translation(&self) -> Vec3 {
		self.translation
	}

	pub fn rotation(&self) -> Quat {
		self.rotation
	}

	pub fn scale(&self) -> Vec3 {
		self.scale
	}

	pub fn visibility(&self) -> VisibilityMask {
		self.visibility
	}

	pub fn is_enabled(&self) -> bool {
		self.enabled
	}

	pub fn depth(&self) -> u32 {
		self.depth
	}

	/// This node's own transform, before its parent's: TRANSLATION * ROTATION * SCALE, with the
	/// rotation NORMALISED so composition does not drift.
	pub fn local(&self) -> Mat4 {
		let rotation = self.rotation.normalise().unwrap_or(Quat::IDENTITY);
		Mat4::from_translation(self.translation).mul(&rotation.to_mat4()).mul(&Mat4::from_scale(self.scale))
	}
}

/// A camera: where it is, what it projects with, and what it sees.
///
/// A CAMERA IS A NODE LIKE ANYTHING ELSE, which is what lets one be parented to a vehicle without a
/// second mechanism; its view transform is the INVERSE of that node's world transform.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Camera {
	pub node: u32,
	projection: Mat4,
	pub visibility: VisibilityMask,
}

impl Camera {
	/// A perspective camera. `far` may be `f32::INFINITY`, which with a `[0, 1]` depth range is well
	/// conditioned and removes the far clip entirely.
	///
	/// VERTICAL FIELD OF VIEW RATHER THAN HORIZONTAL, because a window that changes width then keeps
	/// the same amount of vertical content, which is what a person expects. A DEGENERATE PROJECTION
	/// IS REFUSED HERE, at the call that sets it: it produces a matrix of infinities, and every
	/// vertex after it is a NaN with nothing left to say where they came from.
	pub fn perspective(node: u32, vertical_fov_radians: f32, aspect: f32, near: f32, far: f32, visibility: VisibilityMask) -> Result<Self, Error> {
		let projection = if far == f32::INFINITY { render_math::perspective_infinite_rh_zo(vertical_fov_radians, aspect, near) } else { render_math::perspective_rh_zo(vertical_fov_radians, aspect, near, far) };
		let projection = projection.map_err(|_| Error::Degenerate { reason: "a perspective projection with a zero near plane, a far at or below near, a field of view at or past a half turn, or a zero aspect" })?;
		Ok(Self { node, projection, visibility })
	}

	/// An orthographic camera, given as HALF-EXTENTS centred on the view axis - so it can be swapped
	/// for a perspective one without the content moving sideways.
	pub fn orthographic(node: u32, half_width: f32, half_height: f32, near: f32, far: f32, visibility: VisibilityMask) -> Result<Self, Error> {
		let projection = render_math::orthographic_rh_zo(-half_width, half_width, -half_height, half_height, near, far).map_err(|_| Error::Degenerate { reason: "an orthographic projection with a zero extent or a far plane at or below near" })?;
		Ok(Self { node, projection, visibility })
	}

	/// A camera whose projection the caller built itself - an oblique near plane, a reversed range,
	/// anything the two constructors cannot describe. REFUSES A NON-FINITE MATRIX, which is the one
	/// property no projection can have.
	pub fn from_projection(node: u32, projection: Mat4, visibility: VisibilityMask) -> Result<Self, Error> {
		if !projection.is_finite() {
			return Err(Error::Degenerate { reason: "a projection matrix with a non-finite element" });
		}
		Ok(Self { node, projection, visibility })
	}

	pub fn projection(&self) -> &Mat4 {
		&self.projection
	}
}

/// One instance of a drawable: a world transform and a colour, in a per-instance vertex stream.
///
/// NOT A NODE. A node has a hierarchy and an identity, and a million of them is a million
/// traversals; an instance has neither, which is the whole of why instancing exists.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Instance {
	pub transform: Mat4,
	pub colour: render_math::Vec4,
}

/// Which table a drawable's material is in, and where in it.
///
/// THE TABLE IS PART OF THE NAME rather than a flag beside an index. A drawable carries exactly one
/// material and cannot carry one of each: a core index with an optional Extended index beside it
/// would admit a drawable that one pass reads as a `BlinnPhong` surface and another as a
/// physically based one, drawn as two different surfaces in a single frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MaterialRef {
	/// An index into the scene's core materials, which is the only kind a core layer has.
	Core(u32),
	/// An index into the scene's Extended materials, which only a scene claiming
	/// `Scene3D Extended Profile 1` holds at all.
	Extended(u32),
}

/// One drawable: a mesh, a material, and where it is.
#[derive(Clone, PartialEq, Debug)]
pub struct Drawable {
	pub node: u32,
	/// The caller's own identifier for the geometry.
	pub mesh: u32,
	/// Which material this drawable is drawn with, in whichever table holds it. THE MATERIAL DECIDES
	/// THE QUEUE, so the two can never disagree the way a queue flag on a node can.
	pub material: MaterialRef,
	/// The mesh's bound, in LOCAL space. `None` is NEVER CULLED: an unbounded drawable is one the
	/// scene cannot reason about, and dropping it would make it disappear for a reason nobody can
	/// see.
	pub bounds: Option<Aabb>,
	pub visibility: VisibilityMask,
	/// The per-instance stream. EMPTY means one instance at the node's own world transform, which is
	/// the same command with a count of one rather than a second path.
	pub instances: Vec<Instance>,
	/// `Scene3D Extended Profile 1`'s level-of-detail ladder, or `None` for a drawable that has one
	/// mesh and never consults a threshold.
	///
	/// THE LADDER IS HERE AND THE CHOSEN LEVEL IS NOT, and that is the whole of why the two are
	/// apart: a ladder belongs to the MESH and is the same in every view, while the level chosen
	/// from it belongs to a VIEW - two cameras looking at one scene pick different levels for the
	/// same drawable, and a single cached level on the drawable would make the second view flicker
	/// against the first. The level lives in `detail::ViewDetail`, one per view.
	pub lod: Option<crate::detail::Ladder>,
	id: DrawableId,
}

impl Drawable {
	/// A drawable of one of the four CORE materials. The signature is a core index and stays one, so
	/// a program that implements only `Scene3D Core Profile 1` never names the other table.
	pub fn new(node: u32, mesh: u32, material: u32) -> Self {
		Self { node, mesh, material: MaterialRef::Core(material), bounds: None, visibility: u32::MAX, instances: Vec::new(), lod: None, id: 0 }
	}

	/// A drawable of an Extended material, which only a scene claiming
	/// `Scene3D Extended Profile 1` can hold one of.
	pub fn extended(node: u32, mesh: u32, material: u32) -> Self {
		Self { node, mesh, material: MaterialRef::Extended(material), bounds: None, visibility: u32::MAX, instances: Vec::new(), lod: None, id: 0 }
	}

	pub fn with_bounds(self, bounds: Aabb) -> Self {
		Self { bounds: Some(bounds), ..self }
	}

	pub fn with_visibility(self, visibility: VisibilityMask) -> Self {
		Self { visibility, ..self }
	}

	pub fn with_instances(self, instances: Vec<Instance>) -> Self {
		Self { instances, ..self }
	}

	/// Give this drawable a level-of-detail ladder.
	pub fn with_lod(self, lod: crate::detail::Ladder) -> Self {
		Self { lod: Some(lod), ..self }
	}

	/// Set the picking identity the application wants this drawable to answer with.
	pub fn with_id(self, id: DrawableId) -> Self {
		Self { id, ..self }
	}

	pub fn id(&self) -> DrawableId {
		self.id
	}

	/// How many instances this drawable draws: the stream's length, or one.
	pub fn instance_count(&self) -> u32 {
		if self.instances.is_empty() { 1 } else { self.instances.len() as u32 }
	}
}

/// The whole scene.
pub struct Scene {
	nodes: Vec<Node>,
	/// The cached world transforms, refreshed by `update`.
	world: Vec<Mat4>,
	dirty: Vec<bool>,
	/// Scratch for the propagation: which nodes this update recomputed.
	touched: Vec<bool>,
	/// A topological order over the nodes, depth-first in child order.
	order: Vec<u32>,
	cameras: Vec<Camera>,
	lights: Vec<Light>,
	materials: Vec<Material>,
	/// `Scene3D Extended Profile 1`'s material table, EMPTY on every scene that does not claim the
	/// part - and unreachable on one, because the one entry point that fills it refuses first.
	extended: Vec<ExtendedMaterial>,
	drawables: Vec<Drawable>,
	next_id: DrawableId,
	limits: Limits,
}

impl Scene {
	pub fn new(limits: Limits) -> Self {
		Self {
			nodes: Vec::new(),
			world: Vec::new(),
			dirty: Vec::new(),
			touched: Vec::new(),
			order: Vec::new(),
			cameras: Vec::new(),
			lights: Vec::new(),
			materials: Vec::new(),
			extended: Vec::new(),
			drawables: Vec::new(),
			// ZERO IS NOTHING, so the first identity the scene hands out is one.
			next_id: 1,
			limits,
		}
	}

	pub fn limits(&self) -> &Limits {
		&self.limits
	}

	pub fn nodes(&self) -> &[Node] {
		&self.nodes
	}

	pub fn cameras(&self) -> &[Camera] {
		&self.cameras
	}

	pub fn lights(&self) -> &[Light] {
		&self.lights
	}

	pub fn materials(&self) -> &[Material] {
		&self.materials
	}

	/// The Extended material table, which is empty unless the scene claims the part.
	pub fn extended_materials(&self) -> &[ExtendedMaterial] {
		&self.extended
	}

	/// How many materials the scene holds across BOTH tables, which is what `max_materials` bounds.
	///
	/// ONE CEILING OVER THE TWO, for the reason `max_transparent_items` was refused: a physically
	/// based material IS a material, the core profile already says how many a scene may hold, and a
	/// second limit for the second table is two numbers that can disagree.
	pub fn material_count(&self) -> u32 {
		(self.materials.len() + self.extended.len()) as u32
	}

	pub fn drawables(&self) -> &[Drawable] {
		&self.drawables
	}

	/// Resolve a drawable into whichever table holds its material.
	pub fn material_of(&self, drawable: &Drawable) -> Result<Shading<'_>, Error> {
		match drawable.material {
			MaterialRef::Core(index) => self.materials.get(index as usize).map(Shading::Core).ok_or(Error::NoSuchMaterial { material: index }),
			MaterialRef::Extended(index) => self.extended.get(index as usize).map(Shading::Extended).ok_or(Error::NoSuchExtendedMaterial { material: index }),
		}
	}

	// -----------------------------------------------------------------------------------------
	// The hierarchy.
	// -----------------------------------------------------------------------------------------

	pub fn add_node(&mut self, node: Node) -> Result<u32, Error> {
		if self.nodes.len() as u32 >= self.limits.max_nodes {
			return Err(Error::LimitExceeded { limit: "max_nodes", ceiling: self.limits.max_nodes, asked: self.nodes.len() as u32 + 1 });
		}
		if !node.rotation.is_finite() || node.rotation.length_squared() <= 0.0 {
			return Err(Error::Degenerate { reason: "a rotation that is not a quaternion, which cannot be normalised on composition" });
		}
		if !node.translation.is_finite() || !node.scale.is_finite() {
			return Err(Error::Degenerate { reason: "a transform with a non-finite translation or scale" });
		}
		let depth = match node.parent {
			Some(parent) => {
				let parent_node = self.nodes.get(parent as usize).ok_or(Error::NoSuchNode { node: parent })?;
				parent_node.depth + 1
			}
			None => 0,
		};
		if depth >= self.limits.max_hierarchy_depth {
			return Err(Error::TooDeep { node: self.nodes.len() as u32, ceiling: self.limits.max_hierarchy_depth, asked: depth + 1 });
		}
		let index = self.nodes.len() as u32;
		self.nodes.push(Node { depth, ..node });
		self.world.push(Mat4::IDENTITY);
		self.dirty.push(true);
		self.touched.push(false);
		// APPENDING KEEPS THE ORDER TOPOLOGICAL, because the parent already exists and therefore is
		// already in it. The order is rebuilt depth-first only when a re-parenting changes the shape.
		self.order.push(index);
		Ok(index)
	}

	/// Re-parent a node. REFUSES A CYCLE AND A HIERARCHY TOO DEEP, both at the edge.
	pub fn set_parent(&mut self, node: u32, parent: Option<u32>) -> Result<(), Error> {
		if node as usize >= self.nodes.len() {
			return Err(Error::NoSuchNode { node });
		}
		if let Some(parent) = parent {
			if parent as usize >= self.nodes.len() {
				return Err(Error::NoSuchNode { node: parent });
			}
			if parent == node || self.is_ancestor(node, parent) {
				return Err(Error::WouldCycle { node, parent });
			}
		}
		let was = self.nodes[node as usize].parent;
		self.nodes[node as usize].parent = parent;
		self.rebuild_order();
		// THE DEPTH IS CHECKED AFTER THE SHAPE IS KNOWN, because moving a subtree deepens every node
		// in it and only the rebuilt order says how deep the deepest became.
		if let Some(deepest) = self.nodes.iter().map(|node| node.depth).max() {
			if deepest >= self.limits.max_hierarchy_depth {
				self.nodes[node as usize].parent = was;
				self.rebuild_order();
				return Err(Error::TooDeep { node, ceiling: self.limits.max_hierarchy_depth, asked: deepest + 1 });
			}
		}
		self.mark_dirty(node);
		Ok(())
	}

	pub fn set_translation(&mut self, node: u32, translation: Vec3) -> Result<(), Error> {
		if !translation.is_finite() {
			return Err(Error::Degenerate { reason: "a non-finite translation" });
		}
		self.node_mut(node)?.translation = translation;
		self.mark_dirty(node);
		Ok(())
	}

	pub fn set_rotation(&mut self, node: u32, rotation: Quat) -> Result<(), Error> {
		if !rotation.is_finite() || rotation.length_squared() <= 0.0 {
			return Err(Error::Degenerate { reason: "a rotation that is not a quaternion, which cannot be normalised on composition" });
		}
		self.node_mut(node)?.rotation = rotation;
		self.mark_dirty(node);
		Ok(())
	}

	pub fn set_scale(&mut self, node: u32, scale: Vec3) -> Result<(), Error> {
		if !scale.is_finite() {
			return Err(Error::Degenerate { reason: "a non-finite scale" });
		}
		self.node_mut(node)?.scale = scale;
		self.mark_dirty(node);
		Ok(())
	}

	pub fn set_enabled(&mut self, node: u32, enabled: bool) -> Result<(), Error> {
		self.node_mut(node)?.enabled = enabled;
		Ok(())
	}

	pub fn set_visibility(&mut self, node: u32, visibility: VisibilityMask) -> Result<(), Error> {
		self.node_mut(node)?.visibility = visibility;
		Ok(())
	}

	fn node_mut(&mut self, node: u32) -> Result<&mut Node, Error> {
		self.nodes.get_mut(node as usize).ok_or(Error::NoSuchNode { node })
	}

	/// Mark a node's transform stale. THE SUBTREE IS NOT WALKED HERE: the traversal in `update`
	/// propagates, which is what makes the propagation impossible to miss.
	fn mark_dirty(&mut self, node: u32) {
		if let Some(flag) = self.dirty.get_mut(node as usize) {
			*flag = true;
		}
	}

	/// Whether `candidate` is at or under `node`, which is what makes a parenting a cycle.
	fn is_ancestor(&self, node: u32, candidate: u32) -> bool {
		let mut at = Some(candidate);
		let mut steps = 0;
		while let Some(current) = at {
			if current == node {
				return true;
			}
			steps += 1;
			// A SCENE THAT IS ALREADY A FOREST CANNOT LOOP HERE, and the counter is what makes that
			// true rather than assumed.
			if steps > self.nodes.len() {
				return true;
			}
			at = self.nodes.get(current as usize).and_then(|node| node.parent);
		}
		false
	}

	/// Rebuild the traversal order, depth-first in child order, and with it every node's depth.
	///
	/// LINEAR AND NOT QUADRATIC: the children are bucketed by a counting pass rather than by
	/// scanning for each parent, so a scene at the node limit costs one pass and not four billion
	/// comparisons.
	fn rebuild_order(&mut self) {
		let count = self.nodes.len();
		let mut counts = vec![0_u32; count];
		let mut roots: Vec<u32> = Vec::new();
		for (index, node) in self.nodes.iter().enumerate() {
			match node.parent {
				Some(parent) => counts[parent as usize] += 1,
				None => roots.push(index as u32),
			}
		}
		let mut starts = vec![0_u32; count + 1];
		let mut running = 0;
		for index in 0..count {
			starts[index] = running;
			running += counts[index];
		}
		starts[count] = running;
		let mut fill = starts.clone();
		let mut children = vec![0_u32; running as usize];
		for (index, node) in self.nodes.iter().enumerate() {
			if let Some(parent) = node.parent {
				children[fill[parent as usize] as usize] = index as u32;
				fill[parent as usize] += 1;
			}
		}
		let mut order: Vec<u32> = Vec::with_capacity(count);
		let mut stack: Vec<u32> = roots.iter().rev().copied().collect();
		while let Some(node) = stack.pop() {
			let depth = match self.nodes[node as usize].parent {
				Some(parent) => self.nodes[parent as usize].depth + 1,
				None => 0,
			};
			self.nodes[node as usize].depth = depth;
			order.push(node);
			// Pushed in reverse so they are popped in the order they were added.
			for slot in (starts[node as usize]..fill[node as usize]).rev() {
				stack.push(children[slot as usize]);
			}
		}
		self.order = order;
	}

	/// Refresh every dirty subtree's world transform and hand the whole table over.
	///
	/// IDEMPOTENT AND CHEAP WHEN NOTHING MOVED, which is what makes a second view of one scene free
	/// rather than a second traversal.
	pub fn update(&mut self) -> &[Mat4] {
		for slot in self.touched.iter_mut() {
			*slot = false;
		}
		for position in 0..self.order.len() {
			let index = self.order[position] as usize;
			let parent = self.nodes[index].parent;
			let parent_moved = match parent {
				Some(parent) => self.touched[parent as usize],
				None => false,
			};
			if !self.dirty[index] && !parent_moved {
				continue;
			}
			let local = self.nodes[index].local();
			self.world[index] = match parent {
				Some(parent) => self.world[parent as usize].mul(&local),
				None => local,
			};
			self.dirty[index] = false;
			self.touched[index] = true;
		}
		&self.world
	}

	/// The cached world transforms, without refreshing them.
	pub fn transforms(&self) -> &[Mat4] {
		&self.world
	}

	/// Whether every world transform is current.
	pub fn is_up_to_date(&self) -> bool {
		!self.dirty.iter().any(|dirty| *dirty)
	}

	/// Whether a node and every ancestor of it is enabled.
	///
	/// AN ANCESTOR'S `enabled` HIDES ITS WHOLE SUBTREE, which is what makes hiding a group one edit
	/// rather than one per descendant.
	pub fn effectively_enabled(&self, node: u32) -> bool {
		let mut at = Some(node);
		let mut steps = 0;
		while let Some(current) = at {
			let Some(node) = self.nodes.get(current as usize) else { return false };
			if !node.enabled {
				return false;
			}
			steps += 1;
			if steps > self.nodes.len() {
				return false;
			}
			at = node.parent;
		}
		true
	}

	/// The visibility mask a node effectively has: its own ANDed with every ancestor's.
	///
	/// ANDed, because a mask says which views see a subtree and a child cannot be seen by a view its
	/// parent is hidden from - a child that could would make a mask on a group mean nothing.
	pub fn effective_visibility(&self, node: u32) -> VisibilityMask {
		let mut mask = u32::MAX;
		let mut at = Some(node);
		let mut steps = 0;
		while let Some(current) = at {
			let Some(node) = self.nodes.get(current as usize) else { return 0 };
			mask &= node.visibility;
			steps += 1;
			if steps > self.nodes.len() {
				return 0;
			}
			at = node.parent;
		}
		mask
	}

	// -----------------------------------------------------------------------------------------
	// The things hung on it.
	// -----------------------------------------------------------------------------------------

	pub fn add_camera(&mut self, camera: Camera) -> Result<u32, Error> {
		if self.cameras.len() as u32 >= self.limits.max_cameras {
			return Err(Error::LimitExceeded { limit: "max_cameras", ceiling: self.limits.max_cameras, asked: self.cameras.len() as u32 + 1 });
		}
		if camera.node as usize >= self.nodes.len() {
			return Err(Error::NoSuchNode { node: camera.node });
		}
		self.cameras.push(camera);
		Ok(self.cameras.len() as u32 - 1)
	}

	pub fn add_light(&mut self, light: Light) -> Result<u32, Error> {
		if self.lights.len() as u32 >= self.limits.max_lights {
			return Err(Error::LimitExceeded { limit: "max_lights", ceiling: self.limits.max_lights, asked: self.lights.len() as u32 + 1 });
		}
		if light.node as usize >= self.nodes.len() {
			return Err(Error::NoSuchNode { node: light.node });
		}
		light.validate()?;
		self.lights.push(light);
		Ok(self.lights.len() as u32 - 1)
	}

	pub fn add_material(&mut self, material: Material) -> Result<u32, Error> {
		if self.material_count() >= self.limits.max_materials {
			return Err(Error::LimitExceeded { limit: "max_materials", ceiling: self.limits.max_materials, asked: self.material_count() + 1 });
		}
		material.validate()?;
		self.materials.push(material);
		Ok(self.materials.len() as u32 - 1)
	}

	/// Add a material from `Scene3D Extended Profile 1`'s physically based model.
	///
	/// REFUSED OUTRIGHT ON A SCENE THAT DOES NOT CLAIM EXTENDED. The six Extended limits are set
	/// together or left at zero, and this is the one entry point through which a layer could
	/// otherwise acquire a piece of the part without them - a material it could then draw with,
	/// under a profile it does not claim.
	pub fn add_extended_material(&mut self, material: ExtendedMaterial) -> Result<u32, Error> {
		if !self.limits.claims_extended() {
			return Err(Error::NotExtended { operation: "add_extended_material" });
		}
		if self.material_count() >= self.limits.max_materials {
			return Err(Error::LimitExceeded { limit: "max_materials", ceiling: self.limits.max_materials, asked: self.material_count() + 1 });
		}
		material.validate()?;
		self.extended.push(material);
		Ok(self.extended.len() as u32 - 1)
	}

	/// Add a drawable, assigning it a picking identity if it does not carry one.
	pub fn add_drawable(&mut self, drawable: Drawable) -> Result<u32, Error> {
		if self.drawables.len() as u32 >= self.limits.max_drawables {
			return Err(Error::LimitExceeded { limit: "max_drawables", ceiling: self.limits.max_drawables, asked: self.drawables.len() as u32 + 1 });
		}
		if drawable.node as usize >= self.nodes.len() {
			return Err(Error::NoSuchNode { node: drawable.node });
		}
		match drawable.material {
			MaterialRef::Core(index) => {
				if index as usize >= self.materials.len() {
					return Err(Error::NoSuchMaterial { material: index });
				}
			}
			MaterialRef::Extended(index) => {
				if index as usize >= self.extended.len() {
					return Err(Error::NoSuchExtendedMaterial { material: index });
				}
			}
		}
		if drawable.instances.len() as u32 > self.limits.max_instances_per_drawable {
			return Err(Error::LimitExceeded { limit: "max_instances_per_drawable", ceiling: self.limits.max_instances_per_drawable, asked: drawable.instances.len() as u32 });
		}
		if let Some(bounds) = drawable.bounds {
			if !bounds.is_well_formed() {
				return Err(Error::Degenerate { reason: "an axis-aligned bound whose minimum is past its maximum is a box turned inside out, which one test reports visible and another reports missed" });
			}
		}
		if drawable.instances.iter().any(|instance| !instance.transform.is_finite()) {
			return Err(Error::Degenerate { reason: "an instance transform with a non-finite element" });
		}
		let mut drawable = drawable;
		if drawable.id == 0 {
			drawable.id = self.next_id;
			self.next_id = self.next_id.saturating_add(1);
		} else if self.drawables.iter().any(|other| other.id == drawable.id) {
			// TWO DRAWABLES WITH ONE IDENTITY MAKE A PICK AMBIGUOUS, and the ambiguity would only
			// show as a click that selects the wrong object.
			return Err(Error::DuplicateId { id: drawable.id });
		}
		self.drawables.push(drawable);
		Ok(self.drawables.len() as u32 - 1)
	}

	/// Which drawable answers to a picking identity. `0` is NOTHING and answers `None`.
	pub fn drawable_of_id(&self, id: DrawableId) -> Option<u32> {
		if id == 0 {
			return None;
		}
		self.drawables.iter().position(|drawable| drawable.id == id).map(|index| index as u32)
	}

	/// The view matrix of a camera: the inverse of its node's world transform.
	pub fn view_of(&self, camera: &Camera) -> Result<Mat4, Error> {
		let world = self.world.get(camera.node as usize).ok_or(Error::NoSuchNode { node: camera.node })?;
		world.inverse().map_err(|_| Error::Degenerate { reason: "a camera whose world transform has no inverse, which is a zero scale on it or an ancestor" })
	}

	/// The view-projection matrix of a camera, which is what the frustum is extracted from.
	pub fn view_projection_of(&self, camera: &Camera) -> Result<Mat4, Error> {
		Ok(camera.projection.mul(&self.view_of(camera)?))
	}
}
