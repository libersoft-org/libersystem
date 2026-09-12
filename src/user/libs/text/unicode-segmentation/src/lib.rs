//! Where text may be broken: grapheme clusters, words and lines.
//!
//! THREE DIFFERENT QUESTIONS, AND THEY ARE NOT THE SAME ONE. A grapheme cluster is what a user calls
//! a character - what an arrow key moves over and a backspace deletes - and it is not a code point:
//! an emoji with a skin tone modifier is five code points and one thing to delete. A word boundary
//! is what a double click selects. A line break opportunity is where a line may be broken to fit,
//! and it is the only one of the three that a font, a width and a language can change.
//!
//! IMPLEMENTED FROM THE RULES, OVER THE GENERATED TABLES. The properties come from
//! `unicode-tables`, which is generated from the pinned UCD; the rules here are UAX #29 and UAX #14
//! in the order those documents number them, and the comments name the rule so a reader can check
//! one against the other. What decides whether they are right is the NORMATIVE conformance files,
//! which the gate runs in full rather than in a sample.
//!
//! WHAT IS DELIBERATELY NOT HERE: the tailorings. UAX #14 says a line breaking implementation may
//! tailor its rules by language, and this does not - the default algorithm is what the conformance
//! file measures and what this implements, and a tailoring that arrives later is a decision with its
//! own gate rather than a difference nobody wrote down.

#![cfg_attr(not(test), no_std)]

pub mod grapheme;
pub mod line;
pub mod word;

pub use grapheme::{GraphemeCursor, grapheme_boundaries, is_grapheme_boundary};
pub use line::{LineBreakOpportunity, line_break_opportunities};
pub use word::{is_word_boundary, word_boundaries};

/// The Unicode release the tables under all three algorithms were generated from.
pub use unicode_tables::UNICODE_VERSION;

#[cfg(test)]
mod tests;
