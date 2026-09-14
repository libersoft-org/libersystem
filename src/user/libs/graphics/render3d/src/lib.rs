//! THE BACKEND-NEUTRAL 3D API.
//!
//! WHAT THIS LAYER IS FOR. `Render3D Core Profile 1` is a closed list of what a conforming backend
//! implements, and a list alone settles nothing: "blend state per attachment" cannot be
//! conformance-tested without knowing what a state may contain, "4x MSAA" without sample positions
//! is two different coverage results that both satisfy the words, and "a depth format" without a
//! rounding rule is two depth buffers that disagree at half the values. This crate is where those
//! become types and arithmetic.
//!
//! IT READS THE FROZEN PROFILE AND DOES NOT RESTATE IT. The sample positions, the depth format
//! table, the clip volume and the minimum limits live in `graphics-profile`, are generated into
//! `RENDER3D_PROFILE_1.md` and are bound to it by hash. A second copy here would be a second list to
//! keep in step with the document, and the first one somebody corrects without the other.
//!
//! NO BACKEND, NO DISPLAY, NO ALLOCATION ON A DECISION PATH. Everything here is a value, a check or a
//! piece of arithmetic that a backend performs and a conformance suite compares.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod blend;
pub mod clip;
pub mod command;
pub mod depth;
pub mod error;
pub mod interop;
pub mod limits;
pub mod msaa;
pub mod resource;
pub mod submission;

pub use blend::{ALL_BLEND_FACTORS, ALL_BLEND_OPS, AttachmentBlend, BlendEquation, BlendFactor, BlendOp, BlendState, ColorWriteMask, blend};
pub use clip::{Classification, ClipCoordQ, ClipVertex, PLANES, Plane, classify, classify_positions, clip_polygon, fan_triangles, provoking_vertex};
pub use command::{Buffer, Command, CommandList, Cull, GraphicsPipeline, PipelineState, Rect, Sampler, ShaderProgram, Texture, Topology, VertexAttribute, VertexLayout, VertexStream};
pub use depth::{ALL_COMPARE_OPS, ALL_DEPTH_FORMATS, ALL_STENCIL_OPS, CompareOp, DepthFormat, Outcome, StencilFace, StencilOp, Stored};
pub use error::{AttachmentFault, Error, MeshFault};
pub use interop::{Bridge, admit_as_texture, bridge_for_attachment};
pub use limits::{Granted, Render3DLimits, negotiate};
pub use msaa::{SAMPLE_COUNTS, ShadingRate, alpha_to_coverage, centroid, coverage_after_masks, resolve_colour, resolve_first, sample_positions, shading_rate};
pub use resource::{Aspect, BufferDesc, BufferUsage, Contents, DepthStencilView, HostVisibility, LoadOp, RenderTargetSet, RenderTargetView, StoreOp, TextureDesc, TextureDimension, TextureUsage, TextureViewDesc};
pub use submission::{Completion, Queue, ReadbackTicket, Status, Submission};

#[cfg(test)]
mod tests;
