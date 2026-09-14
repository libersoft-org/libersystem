//! THE RETAINED SCENE LAYER: what an application uses when it does not want to build command lists.
//!
//! IT IMPLEMENTS A HIERARCHY RATHER THAN ADMITTING ONE. A scene layer that cannot express a
//! parent-child transform is one every application replaces with its own, and then the layer is a
//! dependency that buys nothing. So the hierarchy, the cameras, the lights, the bounds, the culling,
//! the queues and the pass graph are all here.
//!
//! AND IT CONTAINS NO BACKEND-SPECIFIC PATH. Everything it produces is a `render3d` command; there is
//! no "fast path" that reaches a backend directly, because the day a second backend exists that path
//! is a second implementation of the whole layer. A fixture asserts it: every scene feature emits
//! only `render3d` commands.
//!
//! WHAT IS NOT HERE IS `f-ext`: LOD selection, skinning, morph targets and an animation pose input.
//! Each is a separate item in a separate part, and putting them here would make the core layer
//! carry the cost of features most scenes do not use.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod bounds;
pub mod cull;
pub mod emit;
pub mod graph;
pub mod light;
pub mod material;
pub mod pick;
pub mod queue;
pub mod scene;

pub use bounds::{Aabb, Sphere, largest_axis_scale, normal_matrix};
pub use cull::{Frustum, Side, Visibility};
pub use emit::{Geometry, Indices, MeshDraw, PassTargets};
pub use graph::{Pass, PassGraph};
pub use light::{Light, LightKind};
pub use material::{Blending, Incident, Material, MaterialKind, Surface, shade};
pub use pick::{Hit, Pending, PickRequest, Ray, Readback};
pub use queue::{Queue, QueueKind, Queued};
pub use scene::{Camera, Drawable, DrawableId, Error, Instance, Limits, Node, Scene, VisibilityMask};

#[cfg(test)]
mod tests;
