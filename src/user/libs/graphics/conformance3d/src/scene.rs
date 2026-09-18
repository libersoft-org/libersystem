//! THE `Scene3D Core Profile 1` HALF OF THE SUITE: the RETAINED layer's own contract.
//!
//! A SCENE FEATURE IS A DECISION AND NOT AN ABILITY. `Render3D` is asked whether it can draw a thing;
//! a scene layer is asked WHICH thing to draw, in what order, with which lights, and what a click
//! answers - and every one of those is visible to an application. So the scenes below are property
//! checks on the layer's own answers rather than pictures: the order two transparent drawables come
//! out in, which plane a cull was rejected by, which light ranks first. A picture would agree with
//! itself whatever order it was composed in.
//!
//! THEY RUN AGAINST THE LAYER AND NOT THROUGH A BACKEND on purpose. Emitting commands and rasterising
//! them would make every one of these scenes fail whenever the rasteriser did, which is the opposite
//! of a suite that names the feature that broke. The one place the two meet - a recorded command list
//! - is checked by the scenes that are ABOUT the seam.

pub mod camera;
pub mod culling;
pub mod graph;
pub mod hierarchy;
pub mod instancing;
pub mod lighting;
pub mod materials;
pub mod picking;
pub mod queues;

use crate::Trouble;
use render_math::Vec3;
use scene3d::{Blending, Camera, Limits, Material, MaterialKind, Node, Scene};

/// A scene at the profile's MINIMUM limits, which is the one a conforming implementation is allowed
/// to be: a suite that built its scenes at some larger size would never reach a limit at all.
pub fn world() -> Scene {
	Scene::new(Limits::PROFILE_MINIMUM)
}

/// A material of a stated kind and blending, with a pipeline handle standing for whatever a backend
/// would have compiled. THE HANDLE IS NEVER DEREFERENCED by this layer - the scene layer carries it
/// and hands it back - so a scene about the queues does not need a backend to exist.
pub fn material(kind: MaterialKind, blending: Blending) -> Material {
	Material::new(kind, render3d::GraphicsPipeline(1), 0).with_blending(blending)
}

/// A camera five units back along `+z` looking down `-z`, which is where every scene that asks about
/// a queue or a frustum views its drawables from.
pub fn camera_at(scene: &mut Scene, position: Vec3) -> Result<Camera, Trouble> {
	let node = scene.add_node(Node::identity().with_translation(position))?;
	Ok(Camera::perspective(node, 1.0, 1.0, 0.1, 100.0, u32::MAX)?)
}

/// A scene-layer refusal of something `Scene3D Core Profile 1` requires is `Unsupported`, on the same
/// terms as a backend's: the profile is a closed list, so a refusal inside it is a layer that does not
/// conform rather than a layer with a gap.
impl From<scene3d::Error> for Trouble {
	fn from(error: scene3d::Error) -> Self {
		Trouble::Unsupported(alloc::format!("the scene layer refused a Profile 1 operation with {error:?}"))
	}
}
