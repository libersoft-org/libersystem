//! L1 and L2: what the levels mean for what is drawn, and the mirroring a renderer applies.
//!
//! L2 IS THE ONLY PLACE ANYTHING IS REVERSED, and it is written as the document writes it: from the
//! highest level down to the lowest odd one, reverse every run at or above that level. Doing it any
//! other way - sorting, or one pass per run - gives the right answer for one level of nesting and
//! the wrong one for two.

use alloc::vec::Vec;
use unicode_tables::BidiClass;

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
/// ANSWERED HERE AND APPLIED BY THE SHAPING STAGE, which is the one place that knows the face: a
/// character is drawn as its mirror when the face has a glyph for the mirror, and otherwise the face
/// is asked for its own mirrored form through `GSUB`'s `rtlm` feature. This says only what a
/// character mirrors to.
///
/// FROM `Bidi_Mirroring_Glyph` AND NOT FROM THE BRACKET PAIRS. Rule N0's pairs are brackets, and most
/// mirrored characters are not: `<`, `>`, the guillemets and the order and set operators all mirror,
/// and an answer taken from the bracket table drew every one of them facing the wrong way.
pub fn mirrored(character: char) -> Option<char> {
	let code_point = character as u32;
	let found = unicode_tables::BIDI_MIRRORING.binary_search_by_key(&code_point, |(candidate, _)| *candidate).ok()?;
	char::from_u32(unicode_tables::BIDI_MIRRORING[found].1)
}
