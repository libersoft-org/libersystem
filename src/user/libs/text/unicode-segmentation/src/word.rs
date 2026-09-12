//! UAX #29 word boundaries: what a double click selects.
//!
//! NOT A SPACE SCAN. "Split on whitespace" is wrong for every language that does not use spaces, and
//! wrong in English too: `don't` is one word and `1,000.50` is one number, while `a-b` is three
//! things. The rules below say so one at a time, and they are numbered as UAX #29 numbers them.
//!
//! THE RULES LOOK PAST FORMAT CHARACTERS. WB4 removes `Extend`, `Format` and `ZWJ` from the middle
//! of a word before any other rule reads it, so this walks a filtered view: the characters that
//! decide, with their real offsets kept. Doing it any other way puts the rule that ignores a
//! combining mark in twelve places instead of one.

use unicode_tables::{WordBreak, word_break};

/// One character as the rules see it: its class, and where it really is.
#[derive(Clone, Copy)]
struct Item {
	class: WordBreak,
	offset: usize,
}

/// Is the pair a boundary, given the two on each side that the lookahead rules need?
fn boundary(before_before: Option<WordBreak>, before: WordBreak, after: WordBreak, after_after: Option<WordBreak>, regional_indicators: usize) -> bool {
	use WordBreak::*;

	// WB3: CR x LF.
	if before == CR && after == LF {
		return false;
	}
	// WB3a and WB3b: a newline is a boundary on both sides.
	if matches!(before, Newline | CR | LF) || matches!(after, Newline | CR | LF) {
		return true;
	}
	// WB3c and WB3d are applied by the caller, on real adjacency, BEFORE the filtering this
	// function's arguments have already been through - see `word_boundaries`.
	// WB5: letters stay together.
	let letter = |class: WordBreak| matches!(class, ALetter | HebrewLetter);
	if letter(before) && letter(after) {
		return false;
	}
	// WB6 and WB7: a letter, a single punctuation, a letter - `don't`, `l'art`.
	if letter(before) && matches!(after, MidLetter | MidNumLet | SingleQuote) && after_after.is_some_and(letter) {
		return false;
	}
	if letter(before_before.unwrap_or(Other)) && matches!(before, MidLetter | MidNumLet | SingleQuote) && letter(after) {
		return false;
	}
	// WB7a, WB7b, WB7c: the Hebrew quotation rules.
	if before == HebrewLetter && after == SingleQuote {
		return false;
	}
	if before == HebrewLetter && after == DoubleQuote && after_after == Some(HebrewLetter) {
		return false;
	}
	if before_before == Some(HebrewLetter) && before == DoubleQuote && after == HebrewLetter {
		return false;
	}
	// WB8, WB9, WB10: numbers, and letters beside them.
	if before == Numeric && after == Numeric {
		return false;
	}
	if letter(before) && after == Numeric {
		return false;
	}
	if before == Numeric && letter(after) {
		return false;
	}
	// WB11 and WB12: a number, a separator, a number - `1,000`, `3.14`.
	if before_before == Some(Numeric) && matches!(before, MidNum | MidNumLet | SingleQuote) && after == Numeric {
		return false;
	}
	if before == Numeric && matches!(after, MidNum | MidNumLet | SingleQuote) && after_after == Some(Numeric) {
		return false;
	}
	// WB13: Katakana.
	if before == Katakana && after == Katakana {
		return false;
	}
	// WB13a and WB13b: the extender, which joins what is on either side of it.
	if matches!(before, ALetter | HebrewLetter | Numeric | Katakana | ExtendNumLet) && after == ExtendNumLet {
		return false;
	}
	if before == ExtendNumLet && matches!(after, ALetter | HebrewLetter | Numeric | Katakana) {
		return false;
	}
	// WB15 and WB16: regional indicators in pairs, as in grapheme clusters.
	if before == RegionalIndicator && after == RegionalIndicator {
		return regional_indicators % 2 == 0;
	}
	// WB999.
	true
}

/// Every word boundary in `text`, as byte offsets, INCLUDING both ends.
///
/// THE FIRST FOUR RULES SEE THE TEXT AS IT IS, AND THE REST SEE IT FILTERED. That order is the whole
/// shape of this function and it is not an optimisation: WB4 says the later rules ignore `Extend`,
/// `Format` and `ZWJ` in the middle of a word, but WB3a to WB3d are applied BEFORE it and read real
/// adjacency. A space, a combining mark and a space is a word boundary precisely because the two
/// spaces are not adjacent - and an implementation that filtered first would join them and be wrong
/// in a way no hand-written test would find.
pub fn word_boundaries(text: &str, into: &mut [usize]) -> usize {
	let mut count = 0usize;
	let mut push = |offset: usize| {
		if count < into.len() {
			into[count] = offset;
		}
		count += 1;
	};
	if text.is_empty() {
		push(0);
		return count;
	}

	// The text as it is, and the same text with what WB4 removes taken out - with each filtered item
	// remembering where it really was.
	let mut raw: [Item; MAX_ITEMS] = [Item { class: WordBreak::Other, offset: 0 }; MAX_ITEMS];
	let mut raw_characters: [char; MAX_ITEMS] = ['\0'; MAX_ITEMS];
	let mut raw_count = 0usize;
	for (offset, character) in text.char_indices() {
		if raw_count == MAX_ITEMS {
			break;
		}
		raw[raw_count] = Item { class: word_break(character), offset };
		raw_characters[raw_count] = character;
		raw_count += 1;
	}
	let mut items: [Item; MAX_ITEMS] = [Item { class: WordBreak::Other, offset: 0 }; MAX_ITEMS];
	// For each raw position, the number of filtered items before it - which is where it lands.
	let mut filtered_index: [usize; MAX_ITEMS] = [0usize; MAX_ITEMS];
	let mut item_count = 0usize;
	let mut previous_forced = true;
	for index in 0..raw_count {
		let class = raw[index].class;
		let ignorable = matches!(class, WordBreak::Extend | WordBreak::Format | WordBreak::ZWJ);
		filtered_index[index] = item_count;
		if ignorable && !previous_forced && item_count > 0 {
			continue;
		}
		previous_forced = matches!(class, WordBreak::Newline | WordBreak::CR | WordBreak::LF);
		items[item_count] = raw[index];
		item_count += 1;
	}

	push(0);
	let mut regional_indicators = 0usize;
	for index in 1..raw_count {
		let left = raw[index - 1];
		let right = raw[index];
		// WB3: CR x LF.
		if left.class == WordBreak::CR && right.class == WordBreak::LF {
			continue;
		}
		// WB3a and WB3b: a newline breaks on both sides.
		if matches!(left.class, WordBreak::Newline | WordBreak::CR | WordBreak::LF) || matches!(right.class, WordBreak::Newline | WordBreak::CR | WordBreak::LF) {
			push(right.offset);
			continue;
		}
		// WB3c: a zero-width joiner holds an emoji sequence together. READ ON REAL ADJACENCY, which
		// is why it is here rather than among the filtered rules.
		if left.class == WordBreak::ZWJ && unicode_tables::is_extended_pictographic(raw_characters[index]) {
			continue;
		}
		// WB3d: two spaces stay together only when they are actually next to each other.
		if left.class == WordBreak::WSegSpace && right.class == WordBreak::WSegSpace {
			continue;
		}
		// WB4: the later rules do not see these at all, so there is no boundary before one.
		if matches!(right.class, WordBreak::Extend | WordBreak::Format | WordBreak::ZWJ) {
			continue;
		}
		// WB5 onward, over the filtered text.
		let at = filtered_index[index];
		if at == 0 {
			continue;
		}
		let before = items[at - 1];
		let after = items[at];
		regional_indicators = if before.class == WordBreak::RegionalIndicator { regional_indicators + 1 } else { 0 };
		let before_before = if at >= 2 { Some(items[at - 2].class) } else { None };
		let after_after = if at + 1 < item_count { Some(items[at + 1].class) } else { None };
		if boundary(before_before, before.class, after.class, after_after, regional_indicators) {
			push(right.offset);
		}
	}
	push(text.len());
	count
}

/// The longest run of characters the rules are applied over. A paragraph longer than this is bounded
/// by the layer above - the text stack's own input ceilings - and this is the point at which THIS
/// function stops reading rather than allocating.
const MAX_ITEMS: usize = 4096;

/// Is `offset` a word boundary in `text`?
pub fn is_word_boundary(text: &str, offset: usize) -> bool {
	if offset == 0 || offset == text.len() {
		return true;
	}
	let mut boundaries = [0usize; MAX_ITEMS];
	let count = word_boundaries(text, &mut boundaries);
	boundaries[..count.min(MAX_ITEMS)].contains(&offset)
}
