//! The text pipeline: ONE ordered sequence, with the order enforced by the types.
//!
//! THE ORDER IS NORMATIVE BECAUSE THE PLAUSIBLE ONES ARE WRONG IN WAYS THAT LOOK RIGHT. The stages
//! are:
//!
//! ```text
//!   1  validation, and the canonical-equivalence rule
//!   2  itemisation into grapheme, script and language runs
//!   3  bidi paragraph level and embedding levels
//!   4  face fallback, applied CLUSTER-ATOMICALLY
//!   5  mirroring
//!   6  shaping, per face-, script- and direction-homogeneous run
//!   7  width measurement
//!   8  line breaking
//!   9  boundary-sensitive reshaping, where a break changed a joining or contextual context
//!  10  per-line visual REORDERING
//! ```
//!
//! BIDI IS NOT ONE PARAGRAPH-LEVEL REORDER, and that is the mistake this type structure exists to
//! make impossible. UAX #9's reordering is applied PER LINE, after the line boundaries are known: a
//! paragraph reordered once and then wrapped is wrong wherever it wraps, and the error is invisible
//! until a line happens to break inside a right-to-left run. So `Visual` can be built from `Lines`
//! and from nothing else - there is no path through these types that reorders before wrapping.
//!
//! EVERY STAGE IS A TYPE AND EVERY TRANSITION IS A FUNCTION THAT CONSUMES THE PREVIOUS ONE. A
//! pipeline written as a list of algorithms is a pipeline whose order is a comment; this one cannot
//! be run out of order, because the input of each stage exists only as the output of the one before.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod canonical;
pub mod fallback;
pub mod stages;

pub use canonical::{CanonicalView, Coverage};
pub use fallback::{FaceEntry, Faces, Policy};
pub use stages::{Faced, Items, Levelled, Lines, Measured, Shaped, Source, Visual};

#[cfg(test)]
mod tests;

/// What the pipeline refuses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
	/// A paragraph longer than this layer will read.
	TooLong { characters: usize, limit: usize },
	/// No face in the set covers a cluster, and there is no last resort either.
	NoFace { cluster: u32 },
	/// A run whose glyph count left what the contract can carry.
	TooManyGlyphs,
	/// A NUMERIC CEILING the profile freezes, met by this document.
	///
	/// IT NAMES WHICH ONE AND BY HOW MUCH, because "too complex" is not something a caller, a report
	/// or a person can act on.
	Exceeded { limit: &'static str, ceiling: u32, asked: u64 },
}

/// The longest paragraph one call will lay out, in code points.
///
/// THE PROFILE'S NUMBER RATHER THAN THIS LAYER'S. The ceilings were frozen in one place precisely so
/// that a document refused by the shaper and a document refused here are refused at the same size;
/// two layers that each chose their own would disagree about which one a document had exceeded.
pub const MAX_PARAGRAPH: usize = opentype_profile::limits::PARAGRAPH_INPUT as usize;

/// The most glyphs one paragraph may produce.
///
/// AN INPUT CAP ALONE IS NOT A BOUND. A paragraph of sixty-five thousand code points that a
/// pathological face expands sixty-four fold is four million glyphs, every offset in range and every
/// input ceiling met - which is the exhaustion the checked offsets do not prevent.
pub const MAX_PARAGRAPH_GLYPHS: usize = opentype_profile::limits::PARAGRAPH_OUTPUT as usize;
