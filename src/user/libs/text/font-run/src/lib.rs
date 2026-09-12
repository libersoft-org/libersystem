//! The PRODUCER of the frozen glyph-run seam: a shaped buffer becomes a `GlyphRun` here.
//!
//! ONE CONVERSION SITE, AND THIS IS IT. Shaping works in FONT UNITS - a design grid of a thousand or
//! two thousand to the em, whatever the face declares - and the seam is 26.6 fixed point. Every
//! offset and every advance crosses that line exactly once, through
//! [`font_contract::Fixed266::from_font_units`], which rounds half-to-even. Rounding a second time
//! anywhere downstream would make the result depend on the order two libraries were written in, and
//! that is a difference nobody can see in a picture and nobody can reproduce from one.
//!
//! THE Y AXIS IS FLIPPED HERE TOO, and it is the same kind of decision. A font measures upward from
//! the baseline and the device space this seam names measures DOWNWARD; a producer that passed font
//! units straight through would put every mark on the wrong side of its base, which reads as a font
//! bug rather than a sign error.
//!
//! THE RUN IS IN VISUAL ORDER AND THE CLUSTERS ARE IN LOGICAL ORDER. Both are the seam's own rule,
//! and they are not the same order: the glyphs are drawn left to right whatever the text's direction,
//! while the clusters stay in the order the text was written so a hit test can answer a question
//! about the STRING without undoing a bidi decision first.
//!
//! A CLUSTER IS A GRAPHEME CLUSTER, not a character and not a glyph. It is where a caret may stand,
//! and a combining mark is not a place a caret stands; deriving clusters from the buffer alone would
//! also lose every cluster a ligature absorbed, which is precisely the caret position this mapping
//! exists to carry.
//!
//! OVERFLOW IS A REFUSAL. A scaled font unit that does not fit 26.6 refuses the run rather than
//! saturating: a saturated advance is a position that is silently wrong, and wrong positions are
//! what the seam was frozen to prevent.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod carets;
pub mod kind;
mod produce;

pub use produce::{Request, Run, produce};

/// What kept a run from being produced.
///
/// NAMED RATHER THAN COUNTED, because each of these is acted on differently: a bad font is a staging
/// problem, an overflow is a layout that must be split, and a run longer than the seam can index is a
/// caller that must break its text up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
	/// The face does not hold up, at the structure the parser names.
	Font(font_parse::Error),
	/// A scaled value outside 26.6, or a sum of admissible ones that left it.
	Overflow(font_contract::Overflow),
	/// More glyphs or more clusters than the seam's own indices can address.
	TooMany,
	/// A NUMERIC CEILING the profile freezes, met by this run. It names which one and by how much.
	Exceeded { limit: &'static str, ceiling: u32, asked: u64 },
	/// STORAGE THIS RUN NEEDED AND COULD NOT HAVE.
	///
	/// A REFUSAL AND NOT AN EXIT. Userspace infallible allocation ends the process when it fails, so a
	/// layout that allocated infallibly could not produce the typed refusal this seam promises: it
	/// would take the caller's whole program down over one paragraph.
	Allocation,
	/// A shaped glyph whose cluster is outside the source range it was shaped from.
	ClusterOutOfRange,
	/// The mapping this produced failed the seam's own invariant check - which is a defect HERE, and
	/// is refused rather than handed on.
	InconsistentMapping,
}

impl From<font_parse::Error> for Error {
	fn from(error: font_parse::Error) -> Self {
		Error::Font(error)
	}
}

impl From<font_contract::Overflow> for Error {
	fn from(overflow: font_contract::Overflow) -> Self {
		Error::Overflow(overflow)
	}
}

#[cfg(test)]
mod tests;
