use super::*;

/// The levels of a string at a direction, with rule L1 applied as a single line.
fn levels_of(text: &str, direction: ParagraphDirection) -> Levels {
	let classes: std::vec::Vec<BidiClass> = text.chars().map(bidi_class).collect();
	let mut resolved = levels(text, direction);
	resolved.reset_whitespace(&classes);
	resolved
}

#[test]
// P2 AND P3: the first STRONG character decides, and a paragraph with none runs left to right.
fn the_paragraph_takes_its_direction_from_its_first_strong_character() {
	assert_eq!(paragraph_level("hello", ParagraphDirection::Auto), 0);
	assert_eq!(paragraph_level("\u{05D0}\u{05D1}", ParagraphDirection::Auto), 1, "Hebrew is right to left");
	assert_eq!(paragraph_level("123", ParagraphDirection::Auto), 0, "a number is not strong");
	assert_eq!(paragraph_level("  \u{0628}", ParagraphDirection::Auto), 1, "leading spaces do not decide");
	// AN ISOLATE'S CONTENTS DO NOT DECIDE THE PARAGRAPH, which is the whole point of an isolate: a
	// quoted Hebrew phrase inside an English sentence must not turn the sentence around.
	assert_eq!(paragraph_level("\u{2066}\u{05D0}\u{2069}a", ParagraphDirection::Auto), 0);
	// And an explicit direction overrides the text entirely.
	assert_eq!(paragraph_level("\u{05D0}", ParagraphDirection::LeftToRight), 0);
	assert_eq!(paragraph_level("a", ParagraphDirection::RightToLeft), 1);
}

#[test]
// THE LEVELS ARE WHAT A RENDERER READS: even is left to right, odd is right to left, and a number
// inside right-to-left text goes up TWO so that it stays in its own order inside a reversed run.
fn a_number_inside_right_to_left_text_keeps_its_own_order() {
	let resolved = levels_of("\u{05D0}12\u{05D1}", ParagraphDirection::Auto);
	assert_eq!(resolved.paragraph_level, 1);
	assert_eq!(resolved.levels, std::vec![1, 2, 2, 1], "the digits are one level above the letters, not below them");
	// The visual order puts the letters in reverse and the digits forward inside them.
	let removed = std::vec![false; resolved.levels.len()];
	assert_eq!(reorder_visual(&resolved.levels, &removed), std::vec![3, 1, 2, 0]);
}

#[test]
// L1: the whitespace at the end of a line goes back to the paragraph level, or a trailing space in a
// right-to-left paragraph is drawn on the wrong side of the line.
fn trailing_whitespace_returns_to_the_paragraph_level() {
	let resolved = levels_of("\u{05D0} ", ParagraphDirection::Auto);
	assert_eq!(resolved.levels, std::vec![1, 1]);
	let resolved = levels_of("a\u{05D0} ", ParagraphDirection::LeftToRight);
	assert_eq!(resolved.levels, std::vec![0, 1, 0], "the space goes back to 0, the Hebrew letter does not");
}

#[test]
// AN ISOLATE IS LAID OUT AT THE LEVEL OUTSIDE IT while its contents are laid out inside, which is
// the difference between an isolate and an embedding and the reason isolates exist.
fn an_isolate_does_not_leak_its_direction_outwards() {
	// `a` RLI Hebrew PDI `b`
	let resolved = levels_of("a\u{2067}\u{05D0}\u{2069}b", ParagraphDirection::Auto);
	assert_eq!(resolved.paragraph_level, 0);
	assert_eq!(resolved.levels, std::vec![0, 0, 1, 0, 0]);
}

#[test]
// N0: a bracket pair takes the direction of what is INSIDE it, which is the rule that makes
// parentheses around a Hebrew phrase point the right way.
fn a_bracket_pair_takes_the_direction_of_what_is_inside_it() {
	let resolved = levels_of("a(\u{05D0})b", ParagraphDirection::LeftToRight);
	assert_eq!(resolved.levels, std::vec![0, 0, 1, 0, 0], "the brackets stay left to right around a Hebrew word");
	let resolved = levels_of("\u{05D0}(\u{05D1})\u{05D2}", ParagraphDirection::Auto);
	assert_eq!(resolved.levels, std::vec![1, 1, 1, 1, 1], "and right to left when everything around them is");
}

#[test]
// L2 REVERSES FROM THE HIGHEST LEVEL DOWN, and a nested case is where doing it any other way goes
// wrong: one pass per run gives the right answer for one level and the wrong one for two.
fn the_visual_order_reverses_by_level_from_the_top_down() {
	let levels = [0u8, 1, 2, 1, 0];
	let removed = std::vec![false; levels.len()];
	assert_eq!(reorder_visual(&levels, &removed), std::vec![0, 3, 2, 1, 4]);
	// Nothing to reverse when everything is left to right.
	assert_eq!(reorder_visual(&[0, 0, 0], &std::vec![false; 3]), std::vec![0, 1, 2]);
	// And the characters X9 removed are not in the order at all: they are not drawn.
	assert_eq!(reorder_visual(&[0, 0, 0], &std::vec![false, true, false]), std::vec![0, 2]);
}

#[test]
// L4: a mirrored character is ANSWERED rather than substituted, because which glyph is drawn is the
// font's business - a face may carry its own mirrored form.
fn a_mirrored_character_is_answered_and_not_applied() {
	assert_eq!(mirrored('('), Some(')'));
	assert_eq!(mirrored(')'), Some('('));
	assert_eq!(mirrored('a'), None);
	assert_eq!(mirrored('\u{05D0}'), None);
}

#[test]
// THE EMPTY PARAGRAPH AND THE BOUND, which are the two edges.
fn the_empty_paragraph_and_the_bound_are_answered() {
	let resolved = levels("", ParagraphDirection::Auto);
	assert!(resolved.levels.is_empty());
	assert_eq!(resolved.paragraph_level, 0);
	assert_eq!(reorder_visual(&[], &[]), std::vec![] as std::vec::Vec<usize>);
	// A paragraph longer than the bound is TRUNCATED by this layer rather than allocated for, which
	// is the behaviour the layer above's own ceilings are stated against.
	let long: std::string::String = core::iter::repeat_n('a', MAX_PARAGRAPH + 10).collect();
	assert_eq!(levels(&long, ParagraphDirection::Auto).levels.len(), MAX_PARAGRAPH);
}

#[test]
// THE ALGORITHM'S OWN MAXIMUM DEPTH, AT ITS EXACT BOUND AND ONE PAST IT.
//
// TWO DIFFERENT MAXIMA, AND CONFUSING THEM IS THE MISTAKE. 125 is the greatest EXPLICIT level an
// embedding may push; 126 is the greatest RESOLVED level a character may end at, because the implicit
// rules raise a left-to-right character sitting at an odd level by one. A bound written as "no level
// above 125" refuses correct text, and one written as "no level above 126" lets an embedding push a
// level it should have overflowed on.
//
// PAST THE BOUND THE EMBEDDING OVERFLOWS AND IS IGNORED, and - this is the part that is got wrong -
// its matching terminator must be ignored too. A terminator matched to an embedding that was never
// pushed pops a level that was never there, and every character after it is laid out one level off.
fn embeddings_nest_to_the_algorithms_own_depth_and_overflow_past_it() {
	use crate::ParagraphDirection;
	// Right-to-left embeddings, one per level, around a single strong left-to-right letter.
	let nest = |count: usize| {
		let mut text = std::string::String::new();
		for _ in 0..count {
			text.push('\u{202B}'); // RIGHT-TO-LEFT EMBEDDING
		}
		text.push('a');
		for _ in 0..count {
			text.push('\u{202C}'); // POP DIRECTIONAL FORMATTING
		}
		text
	};
	let deepest = |text: &str| crate::levels(text, ParagraphDirection::LeftToRight).levels.iter().copied().max().unwrap_or(0);
	let resolved_maximum = crate::MAX_DEPTH + 1;

	// Each right-to-left embedding takes the least ODD level above the last, so `MAX_DEPTH / 2 + 1`
	// of them reach exactly `MAX_DEPTH`. The letter inside is left to right at an odd level, which
	// the implicit rules raise by one.
	let at_bound = crate::MAX_DEPTH as usize / 2 + 1;
	assert_eq!(deepest(&nest(at_bound)), resolved_maximum, "at the bound the nesting must actually reach it, or this fixture measures nothing");

	// ONE MORE EMBEDDING CANNOT RAISE IT. The next odd level would be above the maximum, so the
	// embedding overflows and is ignored - and the answer is the same as at the bound rather than one
	// level higher.
	assert_eq!(deepest(&nest(at_bound + 1)), resolved_maximum, "an overflowing embedding must not raise the level past the maximum");
	assert_eq!(deepest(&nest(at_bound + 40)), resolved_maximum, "and neither must forty of them");

	// AND THE TEXT AFTER EVERY EMBEDDING IS CLOSED IS BACK AT THE PARAGRAPH'S OWN LEVEL. This is what
	// says the overflowed embeddings' terminators were ignored rather than popping levels that were
	// never pushed: a terminator that popped one would leave everything after it one level off, and
	// with forty of them the rest of the paragraph would be laid out right to left.
	for count in [at_bound, at_bound + 1, at_bound + 40] {
		let text = alloc::format!("{}b", nest(count));
		let levels = crate::levels(&text, ParagraphDirection::LeftToRight);
		assert_eq!(levels.levels.last().copied(), Some(0), "the text after every embedding is closed must be at the paragraph's own level, and it was not at {count}");
	}
}
