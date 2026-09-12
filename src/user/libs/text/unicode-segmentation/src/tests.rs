use super::*;

/// The boundaries of a string, as a vector, so a fixture reads as what it asserts.
fn breaks(text: &str, walk: fn(&str, &mut [usize]) -> usize) -> std::vec::Vec<usize> {
	let mut into = std::vec![0usize; text.len() + 2];
	let count = walk(text, &mut into);
	into[..count].to_vec()
}

#[test]
// WHAT A USER CALLS A CHARACTER IS NOT A CODE POINT, and these are the cases where the difference is
// what a backspace does. The normative conformance files are run by the gate; these are here so a
// reader can see what the algorithm is FOR without opening one.
fn a_grapheme_cluster_is_what_a_backspace_deletes() {
	assert_eq!(breaks("abc", grapheme_boundaries), std::vec![0, 1, 2, 3]);
	// A combining mark is not a character of its own.
	assert_eq!(breaks("e\u{0301}x", grapheme_boundaries), std::vec![0, 3, 4], "e plus combining acute is one cluster");
	// A CRLF is one cluster, which is what stops a cursor landing between them.
	assert_eq!(breaks("a\r\nb", grapheme_boundaries), std::vec![0, 1, 3, 4]);
	// A flag is one cluster and two flags are two, which is the regional indicator pairing.
	assert_eq!(breaks("\u{1F1E8}\u{1F1FF}", grapheme_boundaries), std::vec![0, 8]);
	assert_eq!(breaks("\u{1F1E8}\u{1F1FF}\u{1F1E9}\u{1F1EA}", grapheme_boundaries), std::vec![0, 8, 16]);
	// An emoji with a skin tone modifier and a ZWJ sequence: five code points, one thing to delete.
	assert_eq!(breaks("\u{1F469}\u{1F3FD}\u{200D}\u{1F4BB}", grapheme_boundaries), std::vec![0, 15]);
	// AND THE CONJUNCT, which is the rule an implementation written before Unicode 15.1 does not
	// have: a Devanagari consonant, virama, consonant is ONE letter, and breaking it puts the cursor
	// inside a character.
	assert_eq!(breaks("\u{0915}\u{094D}\u{0937}", grapheme_boundaries), std::vec![0, 9], "क्ष is one cluster");
	assert!(is_grapheme_boundary("e\u{0301}x", 0));
	assert!(!is_grapheme_boundary("e\u{0301}x", 1), "inside a cluster");
	assert!(is_grapheme_boundary("e\u{0301}x", 3));
	// The cursor walks clusters, not bytes.
	let walked: std::vec::Vec<&str> = GraphemeCursor::new("e\u{0301}x\u{1F1E8}\u{1F1FF}").collect();
	assert_eq!(walked, std::vec!["e\u{0301}", "x", "\u{1F1E8}\u{1F1FF}"]);
}

#[test]
// A WORD IS NOT WHAT IS BETWEEN SPACES. Every one of these is a case where splitting on whitespace
// gives a different answer from the one a double click should.
fn a_word_boundary_is_what_a_double_click_selects() {
	assert_eq!(breaks("don't", word_boundaries), std::vec![0, 5], "the apostrophe is inside the word");
	assert_eq!(breaks("1,000.50", word_boundaries), std::vec![0, 8], "a number is one thing");
	assert_eq!(breaks("a-b", word_boundaries), std::vec![0, 1, 2, 3], "a hyphen is not");
	assert_eq!(breaks("hi there", word_boundaries), std::vec![0, 2, 3, 8]);
	// A combining mark does not end a word, which is the rule that ignores what a later rule reads.
	assert_eq!(breaks("ab\u{0301}c", word_boundaries), std::vec![0, 5]);
	// AND TWO SPACES WITH A MARK BETWEEN THEM DO NOT JOIN, because the rule that joins spaces is
	// applied before the one that ignores marks - which is the case a filtered-first implementation
	// gets wrong and no hand-written test finds.
	assert_eq!(breaks(" \u{0308} ", word_boundaries), std::vec![0, 3, 4]);
	assert!(is_word_boundary("hi there", 2));
	assert!(!is_word_boundary("hi there", 1));
}

#[test]
// A LINE BREAK OPPORTUNITY IS A PERMISSION, and the mandatory ones are not the same thing as the
// allowed ones - a layout that treated them alike would either ignore a paragraph break or break
// every line at the first space.
fn a_line_break_opportunity_is_allowed_or_mandatory() {
	let opportunities = |text: &str| -> std::vec::Vec<(usize, LineBreakOpportunity)> {
		let mut into = std::vec![(0usize, LineBreakOpportunity::Prohibited); text.len() + 2];
		let count = line_break_opportunities(text, &mut into);
		into[..count].to_vec()
	};
	assert_eq!(opportunities("a b"), std::vec![(2, LineBreakOpportunity::Allowed), (3, LineBreakOpportunity::Mandatory)]);
	assert_eq!(opportunities("a\nb"), std::vec![(2, LineBreakOpportunity::Mandatory), (3, LineBreakOpportunity::Mandatory)], "a newline MUST end the line");
	// A no-break space glues, which is the whole reason it exists.
	assert_eq!(opportunities("a\u{00A0}b"), std::vec![(4, LineBreakOpportunity::Mandatory)]);
	// A hyphen allows a break after it, and the one at the start of a line does not.
	assert_eq!(opportunities("re-do"), std::vec![(3, LineBreakOpportunity::Allowed), (5, LineBreakOpportunity::Mandatory)]);
	// An ideograph may break on either side without a space anywhere.
	assert_eq!(opportunities("\u{4E00}\u{4E01}").len(), 2);
}

#[test]
// THE VERSION IS PART OF THE ANSWER, because a boundary that moves between Unicode releases is one a
// user sees move - so what produced these answers is nameable rather than implied.
fn the_algorithms_say_which_unicode_they_are() {
	assert_eq!(UNICODE_VERSION, "17.0.0");
}

#[test]
// AN EMPTY STRING HAS ONE BOUNDARY AND NO CLUSTERS, which is the edge every walk gets wrong once.
fn the_empty_string_is_answered_rather_than_crashed_on() {
	assert_eq!(breaks("", grapheme_boundaries), std::vec![0]);
	assert_eq!(breaks("", word_boundaries), std::vec![0]);
	let mut into = [(0usize, LineBreakOpportunity::Prohibited); 4];
	assert_eq!(line_break_opportunities("", &mut into), 0, "there is nowhere to break in nothing");
	assert_eq!(GraphemeCursor::new("").count(), 0);
	assert!(is_grapheme_boundary("", 0));
	assert!(is_word_boundary("", 0));
}

#[test]
// A CALLER'S BUFFER IS A BOUND, NOT A SUGGESTION. The walks answer how many boundaries there are and
// write as many as fit, so a short buffer truncates the ANSWER rather than the caller's memory.
fn a_short_buffer_is_filled_and_the_count_is_still_true() {
	let mut into = [0usize; 2];
	let count = grapheme_boundaries("abcd", &mut into);
	assert_eq!(count, 5, "four clusters have five boundaries");
	assert_eq!(into, [0, 1], "and only what fits was written");
}
