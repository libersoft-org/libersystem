//! UAX #29 grapheme cluster boundaries: what a user calls a character.
//!
//! THE RULES ARE NUMBERED AS THE DOCUMENT NUMBERS THEM, and each is named at the line that
//! implements it, so a reader can check one against the other without holding both in their head.
//! Two of them are not a function of the pair of characters at the boundary - GB9c needs the Indic
//! conjunct state before it, and GB12/GB13 need the parity of the regional indicators before it - so
//! this walks with state rather than comparing neighbours.
//!
//! GB9c IS THE ONE A PRE-15.1 IMPLEMENTATION GETS WRONG. A Devanagari conjunct - consonant, virama,
//! consonant - is one grapheme cluster, and an implementation without the rule breaks it into two:
//! the cursor stops in the middle of a letter, and a backspace deletes half of it.

use unicode_tables::{GraphemeBreak, IndicConjunctBreak, grapheme_break, indic_conjunct_break, is_extended_pictographic};

/// What the walk has seen that a later rule depends on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct State {
	/// How many unbroken regional indicators immediately precede the boundary being considered -
	/// GB12 and GB13 join them in PAIRS, so what matters is the parity.
	regional_indicators: usize,
	/// GB11: the text before is an extended pictographic followed by zero or more `Extend` and then
	/// a `ZWJ`, which is the emoji sequence rule.
	pictographic_then_zwj: bool,
	/// GB11's first half: an extended pictographic, possibly followed by `Extend`.
	pictographic_run: bool,
	/// GB9c: a consonant has been seen, with only linkers and extends since, and at least one of
	/// them was a LINKER. Both halves matter - a consonant followed by an extend that is not a
	/// linker does not make the next consonant part of the same cluster.
	conjunct_consonant: bool,
	conjunct_linker_seen: bool,
}

impl State {
	/// Advance the state over one character.
	fn advance(&mut self, character: char) {
		let class = grapheme_break(character);
		let conjunct = indic_conjunct_break(character);

		self.regional_indicators = if class == GraphemeBreak::RegionalIndicator { self.regional_indicators + 1 } else { 0 };

		// GB11's two halves, tracked as the run is walked rather than looked back for.
		self.pictographic_then_zwj = self.pictographic_run && class == GraphemeBreak::ZWJ;
		self.pictographic_run = is_extended_pictographic(character) || (self.pictographic_run && class == GraphemeBreak::Extend);

		// GB9c: the consonant, then linkers and extends. An `Extend` that is not a linker keeps the
		// state alive but does not satisfy the linker requirement, which is what the rule says.
		match conjunct {
			IndicConjunctBreak::Consonant => {
				self.conjunct_consonant = true;
				self.conjunct_linker_seen = false;
			}
			IndicConjunctBreak::Linker if self.conjunct_consonant => self.conjunct_linker_seen = true,
			IndicConjunctBreak::Extend if self.conjunct_consonant => {}
			_ => {
				self.conjunct_consonant = false;
				self.conjunct_linker_seen = false;
			}
		}
	}
}

/// Is there a grapheme cluster boundary between `before` and `after`, given what came before them?
fn boundary(state: &State, before: char, after: char) -> bool {
	let left = grapheme_break(before);
	let right = grapheme_break(after);

	// GB3: CR x LF - a carriage return and a line feed are one cluster and never two.
	if left == GraphemeBreak::CR && right == GraphemeBreak::LF {
		return false;
	}
	// GB4: (Control | CR | LF) รท - a control always ends a cluster.
	if matches!(left, GraphemeBreak::Control | GraphemeBreak::CR | GraphemeBreak::LF) {
		return true;
	}
	// GB5: รท (Control | CR | LF) - and always begins one.
	if matches!(right, GraphemeBreak::Control | GraphemeBreak::CR | GraphemeBreak::LF) {
		return true;
	}
	// GB6, GB7, GB8: the Hangul syllable sequences. A jamo sequence is one cluster.
	if matches!(left, GraphemeBreak::L) && matches!(right, GraphemeBreak::L | GraphemeBreak::V | GraphemeBreak::LV | GraphemeBreak::LVT) {
		return false;
	}
	if matches!(left, GraphemeBreak::LV | GraphemeBreak::V) && matches!(right, GraphemeBreak::V | GraphemeBreak::T) {
		return false;
	}
	if matches!(left, GraphemeBreak::LVT | GraphemeBreak::T) && right == GraphemeBreak::T {
		return false;
	}
	// GB9: x (Extend | ZWJ) - a combining mark stays with what it combines with.
	if matches!(right, GraphemeBreak::Extend | GraphemeBreak::ZWJ) {
		return false;
	}
	// GB9a: x SpacingMark.
	if right == GraphemeBreak::SpacingMark {
		return false;
	}
	// GB9b: Prepend x.
	if left == GraphemeBreak::Prepend {
		return false;
	}
	// GB9c: a Devanagari-style conjunct is one cluster - consonant, linker, consonant.
	if state.conjunct_consonant && state.conjunct_linker_seen && indic_conjunct_break(after) == IndicConjunctBreak::Consonant {
		return false;
	}
	// GB11: an emoji ZWJ sequence is one cluster.
	if state.pictographic_then_zwj && is_extended_pictographic(after) {
		return false;
	}
	// GB12 and GB13: regional indicators join in PAIRS, so a flag is one cluster and two flags are
	// two - which is why the parity of what came before decides, rather than the pair at the
	// boundary.
	if left == GraphemeBreak::RegionalIndicator && right == GraphemeBreak::RegionalIndicator {
		return state.regional_indicators % 2 == 0;
	}
	// GB999: anything else breaks.
	true
}

/// Walk a string, answering the byte offset of every grapheme cluster boundary INCLUDING both ends.
///
/// GB1 AND GB2 ARE WHY BOTH ENDS ARE IN IT: the start of text and the end of text are boundaries, so
/// an empty string has exactly one and a one-cluster string has two. A caller that wants the
/// clusters takes the pairs.
pub fn grapheme_boundaries(text: &str, into: &mut [usize]) -> usize {
	let mut count = 0usize;
	let push = |offset: usize, into: &mut [usize], count: &mut usize| {
		if *count < into.len() {
			into[*count] = offset;
		}
		*count += 1;
	};
	push(0, into, &mut count);
	let mut state = State::default();
	let mut previous: Option<(usize, char)> = None;
	for (offset, character) in text.char_indices() {
		if let Some((_, before)) = previous
			&& boundary(&state, before, character)
		{
			push(offset, into, &mut count);
		}
		state.advance(character);
		previous = Some((offset, character));
	}
	if previous.is_some() {
		push(text.len(), into, &mut count);
	}
	count
}

/// Is `offset` a grapheme cluster boundary in `text`?
///
/// O(n) FROM THE START, deliberately: the state GB9c, GB11 and GB12 need is not a function of the
/// two characters at the offset, and an answer that pretended otherwise would be wrong exactly where
/// those rules apply. A caller walking a whole string uses `GraphemeCursor`, which pays that once.
pub fn is_grapheme_boundary(text: &str, offset: usize) -> bool {
	if offset == 0 || offset == text.len() {
		return true;
	}
	if !text.is_char_boundary(offset) {
		return false;
	}
	let mut state = State::default();
	let mut previous: Option<char> = None;
	for (at, character) in text.char_indices() {
		if at == offset {
			return match previous {
				Some(before) => boundary(&state, before, character),
				None => true,
			};
		}
		state.advance(character);
		previous = Some(character);
	}
	true
}

/// A forward walk over one string's clusters, carrying the state the rules need.
pub struct GraphemeCursor<'a> {
	text: &'a str,
	offset: usize,
}

impl<'a> GraphemeCursor<'a> {
	pub fn new(text: &'a str) -> Self {
		Self { text, offset: 0 }
	}
}

impl<'a> Iterator for GraphemeCursor<'a> {
	type Item = &'a str;

	fn next(&mut self) -> Option<&'a str> {
		if self.offset >= self.text.len() {
			return None;
		}
		let start = self.offset;
		let mut state = State::default();
		// The state is rebuilt from the start of the CLUSTER rather than the start of the text:
		// every rule this carries state for is bounded by a cluster, because a boundary is what ends
		// each of their runs.
		let mut previous: Option<char> = None;
		let mut end = self.text.len();
		for (at, character) in self.text[start..].char_indices() {
			if let Some(before) = previous
				&& boundary(&state, before, character)
			{
				end = start + at;
				break;
			}
			state.advance(character);
			previous = Some(character);
		}
		self.offset = end;
		Some(&self.text[start..end])
	}
}
