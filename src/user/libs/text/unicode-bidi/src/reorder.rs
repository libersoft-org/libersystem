//! L1 and L2: what the levels mean for what is drawn, and the mirroring a renderer applies.
//!
//! L2 IS THE ONLY PLACE ANYTHING IS REVERSED, and it is written as the document writes it: from the
//! highest level down to the lowest odd one, reverse every run at or above that level. Doing it any
//! other way - sorting, or one pass per run - gives the right answer for one level of nesting and
//! the wrong one for two.

use alloc::vec::Vec;
use unicode_tables::{BidiClass, bidi_class};

use crate::Levels;

impl Levels {
	/// L1: a paragraph separator, a segment separator and the whitespace before either - and at the
	/// end of the line - go back to the paragraph level.
	///
	/// WITHOUT THIS A TRAILING SPACE IN A RIGHT-TO-LEFT PARAGRAPH IS DRAWN ON THE WRONG SIDE, which
	/// is the visible half of the rule. The other half is that a caller applies it per LINE, after
	/// line breaking, which is why it is a method rather than something `levels` already did.
	pub fn reset_whitespace(&mut self, original: &[BidiClass]) {
		let mut trailing = true;
		for index in (0..self.levels.len()).rev() {
			match original.get(index).copied() {
				Some(BidiClass::B) | Some(BidiClass::S) => {
					self.levels[index] = self.paragraph_level;
					trailing = true;
				}
				Some(BidiClass::WS) | Some(BidiClass::LRI) | Some(BidiClass::RLI) | Some(BidiClass::FSI) | Some(BidiClass::PDI) if trailing => {
					self.levels[index] = self.paragraph_level;
				}
				Some(BidiClass::RLE) | Some(BidiClass::LRE) | Some(BidiClass::RLO) | Some(BidiClass::LRO) | Some(BidiClass::PDF) | Some(BidiClass::BN) if trailing => {
					// The characters X9 removed sit inside a trailing run without ending it.
					self.levels[index] = self.paragraph_level;
				}
				_ => trailing = false,
			}
		}
	}
}

/// L2: the visual order of a run of levels, as indices into the logical order.
///
/// The characters X9 removed are not in the result: they are not drawn, and leaving them in would
/// make a caller reverse around something that takes no space.
pub fn reorder_visual(levels: &[u8], removed: &[bool]) -> Vec<usize> {
	let mut order: Vec<usize> = (0..levels.len()).filter(|index| !removed.get(*index).copied().unwrap_or(false)).collect();
	let Some(highest) = order.iter().map(|index| levels[*index]).max() else {
		return order;
	};
	let lowest_odd = order.iter().map(|index| levels[*index]).filter(|level| level % 2 == 1).min().unwrap_or(highest + 1);
	let mut level = highest;
	while level >= lowest_odd && level > 0 {
		let mut start = 0usize;
		while start < order.len() {
			if levels[order[start]] < level {
				start += 1;
				continue;
			}
			let mut end = start;
			while end < order.len() && levels[order[end]] >= level {
				end += 1;
			}
			order[start..end].reverse();
			start = end;
		}
		level -= 1;
	}
	order
}

/// L4: the character a mirrored one is drawn as at an odd level.
///
/// ANSWERED RATHER THAN APPLIED. Which glyph is drawn is the renderer's decision - a font may have
/// its own mirrored form through `GSUB`'s `rtlm` feature - so this says what the character mirrors
/// to and leaves the substitution where the font is.
pub fn mirrored(character: char) -> Option<char> {
	// The bracket table carries the pairs, and every mirrored character in it mirrors to its pair.
	let code_point = character as u32;
	let found = unicode_tables::BIDI_BRACKETS.binary_search_by_key(&code_point, |(candidate, _, _)| *candidate).ok()?;
	let (_, pair, _) = unicode_tables::BIDI_BRACKETS[found];
	if bidi_class(character) == BidiClass::ON { char::from_u32(pair) } else { None }
}
