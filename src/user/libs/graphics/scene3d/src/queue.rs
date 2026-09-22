//! RENDER QUEUES: which order things are drawn in, and whose job that is.
//!
//! TRANSPARENCY IS THE LIBRARY'S JOB AND NOT THE CALLER'S. An application that sorts its own
//! transparent objects has to know the camera, the bounds and the blend state - which is the whole of
//! this layer - and every application that does it gets it wrong the first time.
//!
//! OPAQUE IS FRONT TO BACK, because the depth test then rejects most fragments before they are
//! shaded. ALPHA-MASK IS FRONT TO BACK TOO but is its own queue, because a discarding fragment stage
//! cannot take the early depth path on most hardware - keeping it out is what lets the opaque queue
//! stay fast, and running it after the opaque one means fewer of its fragments survive.
//! TRANSPARENT IS BACK TO FRONT, because blending is not commutative.
//!
//! THE SORT KEY IS THE DISTANCE FROM THE CAMERA POSITION TO THE BOUNDING SPHERE'S CENTRE. Not the
//! nearest point of the bounds, which makes a large object sort ahead of a small one it contains.
//!
//! AND THE SORT IS STABLE, tied on submission order. Two objects at one distance drawn in a
//! different order on two runs is a frame that differs from itself, which a conformance comparison
//! sees and a person reads as flicker.

use alloc::vec::Vec;

use render_math::{Mat4, Vec3};

use crate::bounds::Sphere;
use crate::cull::{Frustum, Visibility};
use crate::scene::{Camera, Error, Scene, VisibilityMask};

/// Which queue a material's blending puts it in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QueueKind {
	Opaque,
	/// Alpha-tested: the fragment stage discards, so it cannot take the early depth path.
	AlphaMask,
	Transparent,
}

impl QueueKind {
	/// THE QUEUES RUN IN THIS ORDER AND A SCENE MAY NOT REORDER THEM: each queue's own sort order
	/// means something only in this sequence, and a caller free to reorder them would be free to
	/// blend before the depth buffer was filled.
	pub const ORDER: [QueueKind; 3] = [QueueKind::Opaque, QueueKind::AlphaMask, QueueKind::Transparent];

	/// Whether this queue writes depth.
	pub fn writes_depth(self) -> bool {
		!matches!(self, QueueKind::Transparent)
	}
}

/// One drawable, ready to draw.
#[derive(Clone, PartialEq, Debug)]
pub struct Queued {
	pub drawable: u32,
	/// THE MESH THIS ENTRY DRAWS, which is the drawable's own unless a level-of-detail ladder chose a
	/// coarser one for THIS VIEW.
	///
	/// RESOLVED HERE AND NOT AT THE DRAW. The queue is already the per-view list and the chosen level
	/// is already a per-view answer, so this is where the two meet; resolving it in the recorder
	/// would thread a second per-view structure through a function whose whole input is otherwise
	/// the queue - and two per-view answers is how a view comes to sort one mesh and draw another.
	pub mesh: u32,
	/// The distance from the camera position to the bounding sphere's centre, which is what the sort
	/// is on.
	pub depth: f32,
	/// The world transform of the drawable's node, for a drawable with no instance stream.
	pub world: Mat4,
	/// The world sphere the sort and the light selection both used.
	pub bounds: Sphere,
	/// The instances that survived the frustum, COMPACTED INTO THE STREAM IN THEIR ORIGINAL ORDER.
	/// Empty for a drawable with no instance stream, which draws once.
	pub instances: Vec<u32>,
}

impl Queued {
	/// How many instances this entry draws.
	pub fn instance_count(&self) -> u32 {
		if self.instances.is_empty() { 1 } else { self.instances.len() as u32 }
	}
}

/// The three queues of one view.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Queue {
	pub opaque: Vec<Queued>,
	pub alpha_mask: Vec<Queued>,
	pub transparent: Vec<Queued>,
	/// How many drawables the frustum rejected whole.
	pub culled: u32,
	/// How many drawables the level-of-detail ladder dropped as too small to be worth a draw call.
	///
	/// ITS OWN TALLY BESIDE `culled`, because the two answer different questions: how much geometry
	/// the frustum rejected, and how much the ladder did. One number for both would make a scene
	/// tuning its thresholds unable to see what its thresholds did.
	pub vanished: u32,
	/// How many individual instances the frustum rejected inside drawables that survived. CULLING
	/// THE WHOLE SET BECAUSE ONE BUILDING IS OUTSIDE WOULD DRAW A CITY, so instances are tested one
	/// at a time and this is how many that removed.
	pub culled_instances: u32,
}

impl Queue {
	/// The three queues in the order they are drawn.
	pub fn in_order(&self) -> [&[Queued]; 3] {
		[&self.opaque, &self.alpha_mask, &self.transparent]
	}
}

/// Build the queues for a camera's view, with every drawable at its finest level.
pub fn build(scene: &mut Scene, camera: &Camera) -> Result<Queue, Error> {
	build_for(scene, camera, None)
}

/// Build the queues for a camera's view, at the levels THAT view chose.
///
/// THE STATE IS THE VIEW'S AND SO IS THE QUEUE, which is why they are passed together: two cameras
/// looking at one scene pick different levels for one drawable, and a queue built against the other
/// view's state would draw the level the other camera wanted.
pub fn build_detailed(scene: &mut Scene, camera: &Camera, detail: &crate::detail::ViewDetail) -> Result<Queue, Error> {
	build_for(scene, camera, Some(detail))
}

fn build_for(scene: &mut Scene, camera: &Camera, detail: Option<&crate::detail::ViewDetail>) -> Result<Queue, Error> {
	scene.update();
	let view = scene.view_of(camera)?;
	let view_projection = camera.projection().mul(&view);
	let frustum = Frustum::from_view_projection(&view_projection);
	let eye = scene.transforms()[camera.node as usize].translation();
	Ok(build_with_detail(scene, eye, &frustum, camera.visibility, detail))
}

/// Build the queues against a frustum the caller already has.
///
/// CULLING AND QUEUEING ARE ONE PASS because they read the same things: a world transform, a bound
/// and a mask. Two passes would transform every bound twice.
pub fn build_with(scene: &mut Scene, eye: Vec3, frustum: &Frustum, mask: VisibilityMask) -> Queue {
	build_with_detail(scene, eye, frustum, mask, None)
}

/// The same, at the levels a view chose. `None` draws every drawable at its finest level, which is
/// what a scene that does not claim Extended has.
pub fn build_with_detail(scene: &mut Scene, eye: Vec3, frustum: &Frustum, mask: VisibilityMask, detail: Option<&crate::detail::ViewDetail>) -> Queue {
	scene.update();
	let mut queue = Queue::default();
	for index in 0..scene.drawables().len() {
		let drawable = &scene.drawables()[index];
		if !scene.effectively_enabled(drawable.node) {
			continue;
		}
		if scene.effective_visibility(drawable.node) & drawable.visibility & mask == 0 {
			continue;
		}
		let Some(world) = scene.transforms().get(drawable.node as usize).copied() else { continue };
		let Ok(material) = scene.material_of(drawable) else { continue };
		let kind = material.queue();
		// THE LEVEL THIS VIEW CHOSE, RESOLVED INTO THE ENTRY. A drawable with no ladder draws its own
		// mesh, and so does one whose view remembered no level for it yet - the first frame of a view
		// is the finest level, not no mesh at all.
		let mesh = match (drawable.lod.as_ref(), detail.and_then(|state| state.of(index as u32))) {
			(Some(ladder), Some(chosen)) => match ladder.mesh_of(chosen) {
				Some(mesh) => mesh,
				// VANISHED IS NOT CULLED. The drawable is on screen and the ladder decided it is too
				// small to be worth a draw call, which is a different answer and its own tally.
				None => {
					queue.vanished += 1;
					continue;
				}
			},
			_ => drawable.mesh,
		};

		let entry = if drawable.instances.is_empty() {
			// A drawable with no bounds is NEVER CULLED; its sort key is its node's position, which
			// is the only thing about it the scene knows.
			let bounds = match drawable.bounds {
				Some(local) => local.world_sphere(&world),
				None => Sphere::new(world.translation(), 0.0),
			};
			if drawable.bounds.is_some() && matches!(frustum.test_sphere(&bounds), Visibility::Outside) {
				queue.culled += 1;
				continue;
			}
			Queued { drawable: index as u32, mesh, depth: bounds.centre.sub(eye).length(), world, bounds, instances: Vec::new() }
		} else {
			let mut survivors: Vec<u32> = Vec::new();
			// THE SET'S SPHERE IS OVER EVERY INSTANCE AND NOT OVER THE SURVIVORS, because an
			// instanced drawable SORTS ONCE: a sort key that changed as instances left the frustum
			// would reorder the whole set as the camera moved.
			let mut whole: Option<Sphere> = None;
			for (slot, instance) in drawable.instances.iter().enumerate() {
				let sphere = match drawable.bounds {
					Some(local) => local.world_sphere(&instance.transform),
					None => Sphere::new(instance.transform.translation(), 0.0),
				};
				whole = Some(match whole {
					Some(union) => union.union(&sphere),
					None => sphere,
				});
				if drawable.bounds.is_some() && matches!(frustum.test_sphere(&sphere), Visibility::Outside) {
					queue.culled_instances += 1;
					continue;
				}
				survivors.push(slot as u32);
			}
			if survivors.is_empty() {
				queue.culled += 1;
				continue;
			}
			let bounds = whole.unwrap_or(Sphere::new(world.translation(), 0.0));
			Queued { drawable: index as u32, mesh, depth: bounds.centre.sub(eye).length(), world, bounds, instances: survivors }
		};

		match kind {
			QueueKind::Opaque => queue.opaque.push(entry),
			QueueKind::AlphaMask => queue.alpha_mask.push(entry),
			QueueKind::Transparent => queue.transparent.push(entry),
		}
	}
	sort_near_to_far(&mut queue.opaque);
	sort_near_to_far(&mut queue.alpha_mask);
	sort_far_to_near(&mut queue.transparent);
	queue
}

/// Front to back, STABLY, with submission order as the tiebreak.
fn sort_near_to_far(queue: &mut [Queued]) {
	queue.sort_by(|left, right| left.depth.partial_cmp(&right.depth).unwrap_or(core::cmp::Ordering::Equal).then(left.drawable.cmp(&right.drawable)));
}

/// Back to front, with the SAME tiebreak direction - not the reverse, or two objects at one distance
/// swap between the queues.
fn sort_far_to_near(queue: &mut [Queued]) {
	queue.sort_by(|left, right| right.depth.partial_cmp(&left.depth).unwrap_or(core::cmp::Ordering::Equal).then(left.drawable.cmp(&right.drawable)));
}
