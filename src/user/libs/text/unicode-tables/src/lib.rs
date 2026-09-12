//! The Unicode character properties, GENERATED from the pinned UCD rather than written by hand.
//!
//! WHY THIS IS NOT A LIST OF RANGES IN A SOURCE FILE. A property table copied out of a document is a
//! snapshot nobody can re-derive: the first correction is a patch, the second is a patch on a patch,
//! and after a Unicode release the ranges say what one person believed on one afternoon. Here the
//! whole of `generated.rs` comes from `src/tools/ucd-gen` reading the files `toolchain.lock` pins by
//! SHA-256, a new release is a changed pin and a regeneration, and the gate regenerates and compares.
//!
//! THE VERSION IS PART OF THE ANSWER. Segmentation changes between Unicode releases - a boundary
//! that moves is one a user sees move - so `UNICODE_VERSION` is what a conformance run, a report and
//! a bug report all name.
//!
//! THE ENUMS ARE GENERATED WITH THE TABLES, so an ordinal and its meaning cannot drift apart. A
//! hand-written enum indexing a generated table is two lists that must agree with nothing checking
//! that they do, and the day they stop agreeing every character in one range quietly takes another
//! category's rules.

#![cfg_attr(not(test), no_std)]

mod generated;

pub use generated::*;

/// The ordinal a `(first, last, ordinal)` table gives a code point, or 0 - the default - when no run
/// contains it.
///
/// A BINARY SEARCH, which is what the table's shape is for: the runs are sorted by `first` and do
/// not overlap, and the fixtures check both rather than assuming them. A linear scan over four
/// thousand runs per character is the difference between shaping a paragraph and shaping a page.
pub fn lookup(table: &[(u32, u32, u8)], code_point: u32) -> u8 {
	let mut low = 0usize;
	let mut high = table.len();
	while low < high {
		let middle = low + (high - low) / 2;
		let (first, last, value) = table[middle];
		if code_point < first {
			high = middle;
		} else if code_point > last {
			low = middle + 1;
		} else {
			return value;
		}
	}
	0
}

/// The run a code point is in, or `None` when no run contains it.
///
/// THE DIFFERENCE BETWEEN "THE DEFAULT" AND "NOT MENTIONED", which matters for exactly one property:
/// `Bidi_Class` defaults per BLOCK rather than once, so a code point the records do not mention is
/// not the same as one they mention with the first value.
pub fn lookup_run(table: &[(u32, u32, u8)], code_point: u32) -> Option<u8> {
	let mut low = 0usize;
	let mut high = table.len();
	while low < high {
		let middle = low + (high - low) / 2;
		let (first, last, value) = table[middle];
		if code_point < first {
			high = middle;
		} else if code_point > last {
			low = middle + 1;
		} else {
			return Some(value);
		}
	}
	None
}

/// Is this character `Extended_Pictographic`? The emoji ZWJ rule is written in terms of it.
pub fn is_extended_pictographic(character: char) -> bool {
	let code_point = character as u32;
	let mut low = 0usize;
	let mut high = EXTENDED_PICTOGRAPHIC.len();
	while low < high {
		let middle = low + (high - low) / 2;
		let (first, last) = EXTENDED_PICTOGRAPHIC[middle];
		if code_point < first {
			high = middle;
		} else if code_point > last {
			low = middle + 1;
		} else {
			return true;
		}
	}
	false
}

#[cfg(test)]
mod tests;
