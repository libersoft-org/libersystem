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
//! AND `f-ext` IS ARRIVING BESIDE IT RATHER THAN INSIDE IT. `Scene3D Extended Profile 1` is a
//! separately activated part with its own closed list and its own hash, and its modules are their
//! own: `detail` is the first of them. A scene that does not claim Extended carries its limits at
//! zero and never constructs any of it, so the core layer pays nothing for features most scenes do
//! not use - which is the property the line this replaces was protecting.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod animate;
pub mod bounds;
pub mod cull;
pub mod deform;
pub mod detail;
pub mod emit;
pub mod environment;
pub mod graph;
pub mod light;
pub mod material;
pub mod pbr;
pub mod pick;
pub mod postprocess;
pub mod queue;
pub mod scene;
pub mod shadow;

pub use animate::{Channel, Clip, CubicKey, Curve, Ending, Key, Sampled, SampledPose, Skeleton, Target, Track, apply, blend};
pub use bounds::{Aabb, Sphere, largest_axis_scale, normal_matrix};
pub use cull::{Frustum, Side, Visibility};
pub use deform::{Influences, MorphTarget, Pose};
pub use detail::{Detail, Ladder, Level, ViewDetail};
pub use emit::{Geometry, Indices, MeshDraw, PassTargets};
pub use environment::Irradiance;
pub use graph::{Pass, PassGraph};
pub use light::{Light, LightKind};
pub use material::{Blending, ExtendedMaterial, Incident, Material, MaterialKind, PbrMap, PbrMaps, Shading, Surface, shade};
pub use pbr::{PbrMaterial, PbrSurface};
pub use pick::{Hit, Pending, PickRequest, Ray, Readback};
pub use queue::{Queue, QueueKind, Queued};
pub use scene::{Camera, Drawable, DrawableId, Error, Instance, Limits, MaterialRef, Node, Scene, VisibilityMask};
pub use shadow::CubeFace;

#[cfg(test)]
mod tests;
