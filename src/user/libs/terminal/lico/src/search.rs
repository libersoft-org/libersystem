//! Bounded literal search shared by the LiberCommander viewer and editor.
//!
//! Two kinds of query, kept apart because they are asked about different things: TEXT, which a
//! reader types and which may fold case or require whole words, and BYTES, which somebody
//! diagnosing a file format types as hexadecimal and which must never be reinterpreted as text.
//!
//! Both are LITERAL. A pattern language with repetition is an interpreter running input somebody
//! else wrote, and a viewer that can be made to loop by opening a file has a denial of service in
//! it - the same reason the syntax descriptors match literals rather than expressions.

extern crate alloc;

use alloc::vec::Vec;

/// The most bytes one query may name. Far past anything a person types, and it is what stops a
/// pasted megabyte from becoming a pattern the search then drags across every window of a file.
pub const MAX_PATTERN_BYTES: usize = 256;

/// Why a hexadecimal query could not be read.
///
/// Each case is separate because each needs a different sentence to fix, and because the whole
/// point of validating before scanning is to say WHICH part of what was typed is wrong.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HexPatternError {
	/// Nothing but separators.
	Empty,
	/// A token with an odd number of digits - `48 6` names half a byte, and guessing which half is
	/// how a search silently looks for something nobody asked for.
	OddNibble,
	/// A character that is neither a hexadecimal digit, a wildcard, nor a separator.
	NotHexadecimal,
	/// Past `MAX_PATTERN_BYTES`.
	TooLong,
}

/// A byte query: a run of literal bytes, some of which may be "any byte".
///
/// SPACING IS FREE AND CASE IS IGNORED, which is the one place this is lenient and it is lenient
/// about presentation rather than about meaning: `48 65 6C`, `4865 6c` and `48656C` are the same
/// three bytes, because that is how the same three bytes are written by a hex dump, a specification
/// and a person. `??` is any byte, and it is two characters rather than one so that a wildcard
/// occupies exactly the width of the byte it stands for.
#[derive(Debug)]
pub struct HexPattern {
	bytes: Vec<Option<u8>>,
}

impl HexPattern {
	pub fn parse(text: &[u8]) -> Result<HexPattern, HexPatternError> {
		let mut bytes: Vec<Option<u8>> = Vec::new();
		let mut high: Option<u8> = None;
		let mut index = 0;
		while index < text.len() {
			let byte = text[index];
			if byte.is_ascii_whitespace() || byte == b',' || byte == b'-' {
				// A SEPARATOR IN THE MIDDLE OF A BYTE IS AN ODD NIBBLE, not a place to resume: `4 8`
				// is not `48`, it is a person who has mistyped one of them.
				if high.is_some() {
					return Err(HexPatternError::OddNibble);
				}
				index += 1;
				continue;
			}
			if byte == b'?' {
				if high.is_some() {
					return Err(HexPatternError::OddNibble);
				}
				if text.get(index + 1) != Some(&b'?') {
					return Err(HexPatternError::NotHexadecimal);
				}
				push_bounded(&mut bytes, None)?;
				index += 2;
				continue;
			}
			let Some(nibble) = hex_value(byte) else {
				return Err(HexPatternError::NotHexadecimal);
			};
			match high.take() {
				Some(upper) => push_bounded(&mut bytes, Some(upper << 4 | nibble))?,
				None => high = Some(nibble),
			}
			index += 1;
		}
		if high.is_some() {
			return Err(HexPatternError::OddNibble);
		}
		if bytes.is_empty() {
			return Err(HexPatternError::Empty);
		}
		Ok(HexPattern { bytes })
	}

	pub fn len(&self) -> usize {
		self.bytes.len()
	}

	pub fn is_empty(&self) -> bool {
		self.bytes.is_empty()
	}

	/// Whether `window` is this pattern. `window` must be exactly `len()` long.
	pub fn matches(&self, window: &[u8]) -> bool {
		window.len() == self.bytes.len() && self.bytes.iter().zip(window).all(|(pattern, &byte)| pattern.is_none_or(|wanted| wanted == byte))
	}

	/// The next occurrence at or after `from`, or - going backward - strictly before it.
	pub fn find(&self, haystack: &[u8], from: usize, backward: bool) -> Option<usize> {
		scan(haystack, self.bytes.len(), from, backward, |window| self.matches(window))
	}
}

fn push_bounded(bytes: &mut Vec<Option<u8>>, value: Option<u8>) -> Result<(), HexPatternError> {
	if bytes.len() == MAX_PATTERN_BYTES {
		return Err(HexPatternError::TooLong);
	}
	bytes.try_reserve(1).map_err(|_| HexPatternError::TooLong)?;
	bytes.push(value);
	Ok(())
}

fn hex_value(byte: u8) -> Option<u8> {
	match byte {
		b'0'..=b'9' => Some(byte - b'0'),
		b'a'..=b'f' => Some(byte - b'a' + 10),
		b'A'..=b'F' => Some(byte - b'A' + 10),
		_ => None,
	}
}

/// A text query: literal bytes with optional case folding and whole-word matching.
#[derive(Clone, Copy)]
pub struct TextQuery<'a> {
	pub needle: &'a [u8],
	pub ignore_case: bool,
	/// A match must have a non-word byte (or an edge of the text) on both sides. What counts as a
	/// word byte is ASCII alphanumeric plus `_`, stated here rather than left to a locale: a rule a
	/// reader cannot predict is worse than a rule that is merely simple.
	pub whole_word: bool,
}

impl<'a> TextQuery<'a> {
	pub fn new(needle: &'a [u8]) -> TextQuery<'a> {
		TextQuery { needle, ignore_case: false, whole_word: false }
	}

	pub fn find(&self, haystack: &[u8], from: usize, backward: bool) -> Option<usize> {
		if self.needle.is_empty() {
			return None;
		}
		let hit = scan(haystack, self.needle.len(), from, backward, |window| if self.ignore_case { window.iter().zip(self.needle).all(|(a, b)| a.eq_ignore_ascii_case(b)) } else { window == self.needle });
		let hit = hit?;
		if !self.whole_word {
			return Some(hit);
		}
		if bounded(haystack, hit, self.needle.len()) {
			return Some(hit);
		}
		// A REJECTED MATCH IS NOT THE END OF THE SEARCH. Continuing from the next position is what
		// makes `whole_word` a filter rather than a way to stop at the first substring hit.
		let next = if backward { hit } else { hit + 1 };
		self.find(haystack, next, backward)
	}
}

fn bounded(haystack: &[u8], at: usize, len: usize) -> bool {
	let before = at.checked_sub(1).map(|index| haystack[index]);
	let after = haystack.get(at + len).copied();
	!before.is_some_and(is_word_byte) && !after.is_some_and(is_word_byte)
}

fn is_word_byte(byte: u8) -> bool {
	byte.is_ascii_alphanumeric() || byte == b'_'
}

// The one scan both queries use, so forward and backward cannot disagree about where a search
// starts. Forward is "at or after `from`" and backward is "strictly before `from`", which is what
// makes repeating a search advance instead of finding the same place forever.
fn scan(haystack: &[u8], width: usize, from: usize, backward: bool, matches: impl Fn(&[u8]) -> bool) -> Option<usize> {
	if width == 0 || width > haystack.len() {
		return None;
	}
	let last = haystack.len() - width;
	if backward {
		let mut at = from.min(haystack.len()).checked_sub(1)?.min(last);
		loop {
			if matches(&haystack[at..at + width]) {
				return Some(at);
			}
			at = at.checked_sub(1)?;
		}
	}
	let mut at = from.min(haystack.len());
	while at <= last {
		if matches(&haystack[at..at + width]) {
			return Some(at);
		}
		at += 1;
	}
	None
}
