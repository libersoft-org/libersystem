//! An OpenType or TrueType font, read as HOSTILE INPUT.
//!
//! A FONT IS UNTRUSTED CONTENT. It arrives from a document, a download or a package, and font
//! parsers are a classic memory-safety target: the format is a web of offsets into itself, most of
//! them unsigned, none of them checked by anything but the reader. Every length, offset and index
//! here is checked BEFORE it is used, and a structure that does not hold up is a typed refusal
//! naming what was wrong - never a panic, never a wild index, and never a best effort.
//!
//! NO `unsafe`, NO ALLOCATION, NO PANIC PATH. The whole parser borrows the caller's bytes and reads
//! them through one bounded reader; there is no arithmetic on an offset that is not checked, and
//! every indexing operation goes through `get`. The crate's own fixtures fuzz it - truncation at
//! every length, and every single byte of a valid font flipped - and assert that what comes back is
//! a refusal rather than a crash.
//!
//! WHAT IT READS IS WHAT THE PROFILE ADMITS. `opentype-profile` says which tables and which versions
//! exist for this system; anything else is `Unsupported`, carrying what was outside the profile.
//! A parser that decided that for itself would be a second profile, which is the thing this
//! milestone's ordering forbids.

#![cfg_attr(not(test), no_std)]

pub mod glyf;
pub mod gvar;
pub mod metadata;
pub mod reader;
pub mod tables;
pub mod variations;

pub use opentype_profile::Unsupported;
pub use reader::Reader;
pub use tables::{Face, FaceHeader, Metrics};
pub use variations::{Axis, MAX_AXES, Variations};

/// Why a font could not be read.
///
/// TWO KINDS, KEPT APART. `Malformed` is a file that contradicts itself - an offset past the end, a
/// length that does not fit, a count the table cannot hold. `Unsupported` is a file that is
/// perfectly well formed and asks for something this system has decided not to do. A caller acts on
/// them differently: the first is a broken font, the second is a font for a different system.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
	/// The file contradicts itself, at the structure named.
	Malformed(Malformed),
	/// The file is well formed and outside `OpenType Profile 1`.
	Unsupported(Unsupported),
}

/// What was wrong with a font that does not hold up.
///
/// NAMED RATHER THAN COUNTED. "Malformed font" is not something a report, a staging tool or a person
/// can act on; the table and the reason are.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Malformed {
	/// The file is shorter than the structure it claims to contain.
	Truncated { table: [u8; 4], wanted: usize },
	/// A table directory entry points outside the file.
	TableOutOfBounds { table: [u8; 4] },
	/// The file does not begin with a recognised signature.
	NotAFont,
	/// A collection index past the number of faces the file holds.
	NoSuchFace { index: u32, faces: u32 },
	/// A table this font cannot be read without.
	MissingTable { table: [u8; 4] },
	/// A count, an index or an offset that the table it is in cannot hold.
	InconsistentTable { table: [u8; 4] },
	/// A glyph description that does not hold up - a point count past the table, a composite that
	/// refers to itself, a nesting depth past what the format allows.
	BadGlyph { glyph: u16 },
}

/// The sfnt signatures this system opens.
///
/// `ttcf` IS A COLLECTION and the others are single faces. `true` and `typ1` are Apple's, and are
/// NOT here: the first is a TrueType variant this profile does not admit and the second is a Type 1
/// font, which is a different format wearing an sfnt wrapper.
pub(crate) const SIGNATURE_TRUETYPE: u32 = 0x0001_0000;
pub(crate) const SIGNATURE_OPENTYPE: u32 = 0x4F54_544F; // 'OTTO'
pub(crate) const SIGNATURE_COLLECTION: u32 = 0x7474_6366; // 'ttcf'

/// How deep a composite glyph may nest.
///
/// THE FORMAT DOES NOT SAY, SO THE PROFILE DOES - and it is taken from the profile rather than chosen
/// here. A composite that refers to a composite is ordinary; one that does so five levels down is a
/// font nobody drew, and following it without a bound is how a parser is made to recurse until the
/// stack ends. A ceiling each reader picked for itself would be a ceiling nobody froze, which is the
/// whole reason the numbers live in one place.
pub const MAX_COMPOSITE_DEPTH: u8 = opentype_profile::limits::COMPOSITE_DEPTH as u8;

/// How many points one glyph may have AFTER every component is expanded.
///
/// DEPTH ALONE BOUNDS THE STACK AND NOT THE WORK. Five levels of nesting MULTIPLY: a composite of ten
/// composites of ten composites is a thousand glyphs' worth of points inside a structure whose every
/// offset is in range. The point count is what the drawing costs, so the point count is what is
/// capped, and the cap is over the expansion rather than over any single glyph description.
pub const MAX_GLYPH_POINTS: usize = opentype_profile::limits::COMPOSITE_POINTS as usize;

#[cfg(test)]
mod tests;
