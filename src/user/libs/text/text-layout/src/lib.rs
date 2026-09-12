//! LINE LAYOUT ABOVE SHAPING: where the runs of a line actually sit, and where a caret lands.
//!
//! SHAPING ANSWERS "WHAT GLYPHS", AND THIS ANSWERS "WHERE". A shaped run knows its own advances and
//! nothing about the line it is on - not how wide the line is, not what else is on it, not which end
//! of it is the beginning. Those are the questions between a correct run and a correct paragraph, and
//! every one of them has a decision in it that is invisible once made and wrong forever if made
//! badly.
//!
//! THE LINE IS COMPOSED IN VISUAL ORDER AND ADDRESSED IN LOGICAL ORDER, which is the same split the
//! run seam makes and for the same reason: the pixels go left to right whatever the text's direction,
//! while a caret, a selection and a hit test are questions about the STRING.
//!
//! DIRECTION IS NOT A PROPERTY OF THE LAYOUT, IT IS A PROPERTY OF THE PARAGRAPH. "Align to the start"
//! means the left in a left-to-right paragraph and the RIGHT in a right-to-left one; a layout that
//! spelled alignment as left and right would make every right-to-left document ragged on the wrong
//! side, which is a defect a reader of that script sees immediately and a developer of it never does.
//!
//! JUSTIFICATION HAS A POLICY, NOT A FORMULA. Which points may absorb space is a property of the TEXT
//! and belongs to the layer that has the text; how much each of them gets, what happens when there
//! are none, and whether the last line is stretched are properties of the LAYOUT and are decided
//! here. Splitting it the other way is how two callers justify the same paragraph differently.
//!
//! NOTHING HERE MEASURES A FONT. Every input is already in the seam's 26.6, so this layer is host
//! testable on numbers a fixture states - which is what lets the decisions below be checked rather
//! than looked at.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod align;
pub mod caret;
pub mod compose;
pub mod justify;
pub mod tabs;
pub mod truncate;

pub use align::{Alignment, align};
pub use caret::{Selection, caret_position, selection_pieces};
pub use compose::{Line, PlacedRun, compose};
pub use justify::{Elastic, Justification, justify};
pub use tabs::TabStops;
pub use truncate::{Truncation, truncate};

/// Which way the paragraph runs, which is what "start" and "end" mean.
///
/// THE PARAGRAPH'S DIRECTION AND NOT THE RUN'S. A line of Latin inside an Arabic paragraph is still
/// laid out from the right; taking the direction from the first run is how one English word at the
/// top of an Arabic document flips the whole page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParagraphDirection {
	LeftToRight,
	RightToLeft,
}

impl ParagraphDirection {
	pub const fn is_right_to_left(self) -> bool {
		matches!(self, ParagraphDirection::RightToLeft)
	}
}

/// What kept a line from being laid out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
	/// A sum of admissible 26.6 values that left 26.6. A line whose extent does not fit is one this
	/// layer will not draw; saturating would hand a renderer a position that is silently wrong.
	Overflow(font_contract::Overflow),
	/// Storage this line needed and could not have. A REFUSAL and not an exit: infallible allocation
	/// in userspace ends the process, which would take a caller's whole program down over one line.
	Allocation,
	/// A run and a cluster map that do not describe the same thing - a map whose clusters index
	/// glyphs the run does not have. Refused rather than laid out around.
	Mismatched,
}

impl From<font_contract::Overflow> for Error {
	fn from(overflow: font_contract::Overflow) -> Self {
		Error::Overflow(overflow)
	}
}

#[cfg(test)]
mod tests;
