//! The text pipeline: ONE ordered sequence, with the order enforced by the types.
//!
//! THE ORDER IS NORMATIVE BECAUSE THE PLAUSIBLE ONES ARE WRONG IN WAYS THAT LOOK RIGHT. The stages
//! are:
//!
//! ```text
//!   1  validation, the canonical-equivalence rule, and the languages the caller states
//!   2  itemisation into grapheme, script and language runs
//!   3  bidi paragraph level and embedding levels
//!   4  face fallback, applied CLUSTER-ATOMICALLY, in the policy's order for each run's script and
//!      language
//!   5  mirroring
//!   6  shaping, per face-, script-, language- and direction-homogeneous run
//!   7  width measurement
//!   8  line breaking
//!   9  boundary-sensitive reshaping: every run a break split, shaped again piece by piece
//!  10  per-line visual REORDERING
//! ```
//!
//! BIDI IS NOT ONE PARAGRAPH-LEVEL REORDER, and that is the mistake this type structure exists to
//! make impossible. UAX #9's reordering is applied PER LINE, after the line boundaries are known: a
//! paragraph reordered once and then wrapped is wrong wherever it wraps, and the error is invisible
//! until a line happens to break inside a right-to-left run. So `Visual` can be built from the
//! reshaped lines and from nothing else - there is no path through these types that reorders before
//! wrapping, or that reaches the visual order with a run a break split still shaped as one.
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
pub use fallback::{FaceEntry, Policy};
pub use stages::{Faced, Faces, Items, LanguageSpan, Levelled, Lines, Measured, Reshaped, Shaped, ShapedRun, Source, Visual};

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
	/// A language span that does not start on a character boundary of the text, or does not start
	/// after the span before it - a statement about the text that cannot be true of it.
	Language { at: usize },
	/// The shaper refused a run: the face does not hold up, the run is in a script the profile does
	/// not name, or a ceiling was met inside it.
	Shaping(font_parse::Error),
	/// A scaled advance that 26.6 cannot hold.
	Overflow(font_contract::Overflow),
	/// STORAGE THIS PARAGRAPH NEEDED AND COULD NOT HAVE.
	///
	/// A REFUSAL AND NOT AN EXIT. Infallible allocation in userspace ends the process when it fails,
	/// so a pipeline that allocated infallibly could not produce the typed refusal this milestone
	/// promises: it would take the caller's whole program down over one paragraph.
	Allocation,
}

impl From<font_parse::Error> for Error {
	fn from(error: font_parse::Error) -> Self {
		match error {
			// THE SHAPER'S STORAGE IS THE PARAGRAPH'S STORAGE, and it is one refusal whichever layer
			// ran out.
			font_parse::Error::Allocation => Error::Allocation,
			other => Error::Shaping(other),
		}
	}
}

/// Storage for this many entries, or the typed refusal.
pub(crate) fn storage<T>(count: usize) -> Result<alloc::vec::Vec<T>, Error> {
	let mut out = alloc::vec::Vec::new();
	out.try_reserve_exact(count).map_err(|_| Error::Allocation)?;
	Ok(out)
}

/// Room for one more entry, or the typed refusal.
pub(crate) fn room<T>(vector: &mut alloc::vec::Vec<T>) -> Result<(), Error> {
	vector.try_reserve(1).map_err(|_| Error::Allocation)
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
