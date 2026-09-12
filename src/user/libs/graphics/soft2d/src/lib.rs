//! `soft2d`: the COMPLETE CPU implementation of `Render2D Core Profile 1`.
//!
//! IT IMPLEMENTS THE WHOLE PROFILE. A command in Profile 1 that this backend cannot execute is a
//! DEFECT and not an `Unsupported`: the profile is a closed list precisely so that "the backend does
//! not do conic gradients" is a bug report rather than a design.
//!
//! `no_std`, no `DisplayService`, no surface. It draws into an `ImageViewMut` or an `OwnedImage`, and
//! everything it needs to know about where those came from is in the `TargetDescription`. That is
//! what lets its whole test suite be host tests and its benchmark a headless replay.
//!
//! TWO PHASES, because "no steady-state allocation" and a profile containing layers, clip masks,
//! blur, backdrop filters, path flattening, glyph caches and image pyramids cannot both be true
//! without saying WHEN the scratch is worked out. `prepare` flattens, bins, resolves bounds and
//! reserves; `render` replays and allocates nothing.
//!
//! AND IT IS ONE BACKEND AMONG FUTURE OTHERS. Its public surface is the `render2d::Backend` trait,
//! not an application-facing drawing API - a consumer that named `soft2d` in its own types would
//! have to be rewritten when a GPU backend arrived, which is the whole reason the trait exists.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod backend;
pub mod clip;
pub mod filter;
pub mod glyph;
pub mod layer;
pub mod paint;
pub mod raster;
pub mod span;
pub mod stroke;
pub mod target;
pub mod tile;

pub use backend::{Soft2d, SoftPrepared};
pub use glyph::{GlyphImage, GlyphProvider, GlyphRaster};
pub use target::ImageSource;

/// The backend's name and version, which a prepared list is bound to.
///
/// A PREPARED LIST IS THIS BACKEND'S OWN FORM. Another backend's bytes are not a prepared list, they
/// are bytes - so the identity is part of the key rather than a label.
pub const BACKEND_NAME: &str = "soft2d";
pub const BACKEND_VERSION: u32 = 1;

/// How wide and tall a tile is.
///
/// TILING IS WHAT MAKES THE SCRATCH BOUNDED. A layer, a clip mask and a filter intermediate are all
/// sized to a tile plus what a filter reaches past it, rather than to the surface - so a
/// four-thousand-pixel-wide drawing with three nested layers costs three tiles of scratch and not
/// three screens of it. Sixty-four squared is four thousand pixels, which is a working set that stays
/// in a cache while a span is composited.
pub const TILE_SIZE: u32 = 64;

#[cfg(test)]
mod tests;
