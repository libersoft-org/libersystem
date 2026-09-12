//! Shaping: what a font does to a run of characters before anything is drawn.
//!
//! LATIN ALONE IS THE CASE THAT MAKES A DESIGN LOOK FINISHED AND PROVES NOTHING. A shaper that
//! substitutes ligatures and applies kerning renders English beautifully and renders Arabic as
//! disconnected letters, Devanagari with its vowels on the wrong side, and Khmer as a pile. What
//! makes a shaper real is the rest: joining forms decided before any lookup runs, syllables
//! reordered, marks attached to the glyph they belong to rather than placed at the pen.
//!
//! THE TABLES ARE READ AS HOSTILE INPUT, through `font-parse`'s bounded reader. A `GSUB` table is a
//! graph of offsets into itself with counts at every node, and a shaper that trusts one is a shaper
//! a font can walk out of its own table. Every offset here is resolved through the table's own
//! bounds and every count is checked against what the table actually holds.
//!
//! WHAT IS APPLIED IS WHAT THE PROFILE ADMITS. `opentype-profile` names the lookup types and the
//! subtable formats; a font asking for anything else is a typed refusal rather than a lookup
//! silently skipped - an ignored substitution renders text that looks plausible and is wrong, which
//! is the failure this whole milestone is shaped against.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod buffer;
pub mod context;
pub mod gpos;
pub mod gsub;
pub mod layout;
pub mod scripts;
pub mod shape;

pub use buffer::{Buffer, GlyphInfo, Position};
pub use layout::{Feature, LayoutTable, Lookup};
pub use scripts::{Form, Shaper, shaper_for};
pub use shape::{MAX_LOOKUPS, shape, shape_run, shape_with_masks};

/// Which of the two layout tables a lookup came from. They share every structure above the lookup
/// itself and share none below it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Table {
	Substitution,
	Positioning,
}

impl Table {
	pub const fn tag(self) -> [u8; 4] {
		match self {
			Self::Substitution => *b"GSUB",
			Self::Positioning => *b"GPOS",
		}
	}
}

#[cfg(test)]
mod tests;
