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
