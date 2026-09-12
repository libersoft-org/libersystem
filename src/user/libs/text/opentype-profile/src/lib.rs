//! `OpenType Profile 1`: what this system's font parser reads, and what it REFUSES.
//!
//! WHY A CLOSED LIST AND NOT A TABLE OF TABLE NAMES. "Parse `CFF`/`CFF2`" and "correct at any
//! coordinate" cannot both be met by a list of four-character tags: producing outlines from
//! `CFF`/`CFF2` needs a bounded Type 2 / CFF2 charstring interpreter with subroutines, variation
//! store selection and `blend`; correct METRICS at a non-default coordinate needs `HVAR` for
//! horizontal advances - which is also what varies CFF2 advances - and `MVAR` for the font-wide
//! metrics line layout reads. A list that named the tags and not those mechanisms promised the
//! second sentence while describing only the first.
//!
//! ANYTHING OUTSIDE THIS IS A TYPED `Unsupported` REFUSAL. Not undefined behaviour, not a silent
//! wrong result, and not a best effort: a font is untrusted content that arrives from a document, a
//! download or a package, and "we did something with it" is the failure mode that makes font parsers
//! a classic memory-safety target. The refusal names what was outside the profile.
//!
//! AND THE EXCLUSIONS ARE ENUMERATED TOO, because a reader cannot tell a deliberate omission from a
//! forgotten one by looking at a list of what IS supported. `Excluded` says which, and why.
//!
//! THE PROFILE IS VERSIONED. `PROFILE_VERSION` is what a refusal, a document and a conformance run
//! all name; a change to the list is a change to that number in the same edit.

#![cfg_attr(not(test), no_std)]

pub mod colour;
pub mod layout;
pub mod limits;
pub mod scripts;
pub mod tables;
pub mod variations;

pub use colour::{BitmapFormat, CompositeMode, ExtendMode, PaintKind};
pub use layout::{GposLookup, GsubLookup, LookupFlag, SubtableFormat};
pub use limits::{Limit, limit};
pub use scripts::{LanguageSupport, ScriptSupport, ShapingClass};
pub use tables::{Excluded, TableSupport};
pub use variations::VariationMechanism;

/// The profile's version. A refusal names it, the generated document carries it, and a conformance
/// run is measured against it.
pub const PROFILE_VERSION: u32 = 1;

/// Why something a font asked for is not being done.
///
/// ONE REFUSAL TYPE FOR THE WHOLE PARSER, carrying WHAT was outside the profile rather than a bare
/// "unsupported": a caller that cannot tell an unsupported table from an unsupported lookup cannot
/// report anything useful, and neither can a conformance suite.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unsupported {
	/// A table whose tag the profile does not carry.
	Table([u8; 4]),
	/// A table the profile carries, at a version it does not.
	TableVersion { tag: [u8; 4], major: u16, minor: u16 },
	/// A `cmap`, `Coverage` or `ClassDef` subtable format outside the profile.
	SubtableFormat { table: [u8; 4], format: u16 },
	/// A `GSUB` lookup type outside the profile.
	GsubLookup(u16),
	/// A `GPOS` lookup type outside the profile.
	GposLookup(u16),
	/// A `COLR` v1 paint format outside the profile.
	Paint(u8),
	/// A script the profile does not shape.
	Script([u8; 4]),
	/// A variation mechanism outside the profile.
	Variation(&'static str),
	/// A structure the profile EXCLUDES by name - see `tables::EXCLUDED`.
	ExcludedByProfile(&'static str),
	/// A NUMERIC CEILING the profile freezes, met by a font or a document.
	///
	/// IT NAMES WHICH ONE AND BY HOW MUCH. "Too complex" is not something a report, a staging tool or
	/// a person can act on; "composite depth, ceiling 5, asked 41" is - and a conformance suite can
	/// assert on it.
	Exceeded { limit: &'static str, ceiling: u32, asked: u64 },
}

/// A four-character OpenType tag, as the format stores it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Tag(pub [u8; 4]);

impl Tag {
	pub const fn new(tag: &[u8; 4]) -> Self {
		Self(*tag)
	}

	/// The tag as text, for a document and a refusal message.
	pub fn as_str(&self) -> &str {
		// SAFETY-FREE: every tag in this crate is written as an ASCII literal, and `from_utf8`
		// answers for anything that somehow is not rather than this asserting.
		core::str::from_utf8(&self.0).unwrap_or("????")
	}
}

#[cfg(test)]
mod tests;
