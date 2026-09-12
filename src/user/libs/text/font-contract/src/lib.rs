//! THE ONE SHARED FONT-RESOURCE CONTRACT, frozen before either side implements against it.
//!
//! WHY IT IS A CRATE AND NOT TWO PARAGRAPHS IN TWO PLANS. The text stack PRODUCES shaped runs and
//! the 2D renderer CONSUMES them, and everything they have to agree about - what a face IS, who
//! issues its identity, who owns the validated bytes, what a run carries, in what units, and what
//! distinguishes two rasterisations of one glyph - was described twice, in two documents, in two
//! wordings. Two wordings of one seam is two seams. This is the definition; neither side restates
//! it, and a change to it is a change to both sides in the same edit.
//!
//! WHAT IT FIXES:
//!
//! ```text
//!   a FACE            a content-derived file identity ISSUED BY THE CATALOGUE, plus a face index -
//!                       because a collection is one file with several faces
//!   OWNERSHIP         the library TAKES the validated bytes by copying them into storage it owns
//!                       before parsing, and no decoded structure references bytes that can move.
//!                       The transport buffer belongs to the caller and stays writable: this kernel
//!                       has no seal, so the copy is where immutability comes from
//!   DECODED FORMS     outline, grayscale mask, subpixel mask, bitmap strike, COLR v0 layers and
//!                       v1 paint graph - enumerated, because "access to decoded resources" is not
//!                       something a consumer can ask for
//!   LIFETIME          a generation, published by the catalogue, carried by everything derived from
//!                       a face, so a replacement invalidates what it should
//!   a LAYOUT RESULT   a sequence of face-, script- and direction-homogeneous runs,
//!                       unconditionally - the per-glyph-face alternative is rejected and not
//!                       deferred, because it would put a face switch inside the structure whose
//!                       purpose is to be painted in one operation
//!   THE CACHE KEY     one normative definition, with a fixture per field proving that two entries
//!                       differing only in that field do not collide
//! ```
//!
//! NO DEPENDENCIES AND `no_std`: the seam must be linkable by both sides and testable on the host.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod cache;
pub mod cluster;
pub mod face;
pub mod fixed;
pub mod glyph;
pub mod run;

pub use cache::{GlyphCacheKey, KindSelection};
pub use cluster::{Caret, CaretAffinity, Cluster, ClusterMap, SourceRange};
pub use face::{FaceBytes, FaceIdentity, FaceRef, FileIdentity, Generation};
pub use fixed::{Fixed266, Overflow};
pub use glyph::{GlyphKind, MAX_VARIATION_AXES, RasterisationMode, SubpixelLayout, SubpixelPhase, TransformKey, VariationCoordinates};
pub use run::{Direction, GlyphRun, PositionedGlyph, ScriptTag};

#[cfg(test)]
mod tests;
