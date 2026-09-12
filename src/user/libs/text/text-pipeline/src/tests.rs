use super::*;

use font_contract::Fixed266;
use unicode_bidi::ParagraphDirection;

/// A face set for the regression: face 0 carries the PRECOMPOSED letters and no combining marks;
/// face 1 carries the base letters and the marks and no precomposed forms.
///
/// THE TWO HALVES OF THE POLICY'S OWN CASE. A real font set looks exactly like this - a face with
/// precomposed Latin and a face with marks - and it is why the coverage question has to be asked in
/// the canonical view rather than of the bytes the caller happened to send.
struct TwoFaces {
	/// Which characters each face has a glyph for.
	precomposed: alloc::vec::Vec<char>,
	decomposed: alloc::vec::Vec<char>,
}

impl Coverage for TwoFaces {
	fn covers(&self, face: u16, spelling: &[char]) -> bool {
		let set = if face == 0 { &self.precomposed } else { &self.decomposed };
		!spelling.is_empty() && spelling.iter().all(|character| set.contains(character))
	}

	fn faces(&self) -> &[u16] {
		&[0, 1]
	}
}

#[test]
// THE CANONICAL VIEW IS BOTH SPELLINGS OF ONE CLUSTER, and neither is the buffer.
fn a_cluster_is_seen_in_both_of_its_spellings() {
	// é, composed.
	let view = CanonicalView::of(&['\u{00E9}']);
	assert_eq!(view.composed, std::vec!['\u{00E9}']);
	assert_eq!(view.decomposed, std::vec!['e', '\u{0301}']);
	// é, decomposed: the SAME view.
	let other = CanonicalView::of(&['e', '\u{0301}']);
	assert_eq!(view, other, "two spellings of one cluster have one canonical view");
	// TWO MARKS IN EITHER ORDER ARE ONE STRING, which the canonical ordering is what makes true: a
	// view that did not sort them would call two identical clusters different, and fallback would
	// then differ too.
	let first = CanonicalView::of(&['a', '\u{0301}', '\u{0327}']);
	let second = CanonicalView::of(&['a', '\u{0327}', '\u{0301}']);
	assert_eq!(first, second);
	// And a cluster with nothing to compose or decompose is itself, both ways.
	let plain = CanonicalView::of(&['x']);
	assert_eq!(plain.composed, std::vec!['x']);
	assert_eq!(plain.decomposed, std::vec!['x']);
}

#[test]
// THE REGRESSION THE POLICY ASKS FOR: the composed and decomposed spellings of one string take the
// SAME fallback decision. Without the canonical view they take different faces - which is not a
// theoretical case, it is what a font set with precomposed Latin and a separate mark face does.
fn both_spellings_take_the_same_fallback_decision() {
	let faces = TwoFaces { precomposed: std::vec!['\u{00E9}', 'a'], decomposed: std::vec!['e', '\u{0301}', 'a'] };
	// Face 0 is asked first and covers the composed spelling, so both inputs choose it.
	assert_eq!(canonical::face_for(&faces, &['\u{00E9}']), Some(0));
	assert_eq!(canonical::face_for(&faces, &['e', '\u{0301}']), Some(0), "the decomposed input reaches the precomposed face");
	// And with the precomposed face absent, both reach the mark face - again the same decision.
	let faces = TwoFaces { precomposed: std::vec!['a'], decomposed: std::vec!['e', '\u{0301}', 'a'] };
	assert_eq!(canonical::face_for(&faces, &['\u{00E9}']), Some(1), "the composed input reaches the decomposing face");
	assert_eq!(canonical::face_for(&faces, &['e', '\u{0301}']), Some(1));
	// A cluster no face covers is a refusal rather than a silent notdef.
	let faces = TwoFaces { precomposed: std::vec!['a'], decomposed: std::vec!['a'] };
	assert_eq!(canonical::face_for(&faces, &['\u{4E00}']), None);
}

#[test]
// AND FOR A NON-LATIN SCRIPT, because a Latin-only proof of this is the case that always works.
// Devanagari KA with a nukta has both spellings too, and a font set that carries one and not the
// other is ordinary.
fn the_same_holds_for_a_non_latin_script() {
	// U+0958 is KA WITH NUKTA, canonically KA plus the nukta - and a composition EXCLUSION, so the
	// canonical view's composed form is the decomposed one. That is the correct answer and the case
	// an implementation that recomposed everything would get wrong.
	let view = CanonicalView::of(&['\u{0958}']);
	assert_eq!(view.decomposed, std::vec!['\u{0915}', '\u{093C}']);
	assert_eq!(view.composed, std::vec!['\u{0915}', '\u{093C}'], "an excluded composite is never recomposed");
	let faces = TwoFaces { precomposed: std::vec!['\u{0958}'], decomposed: std::vec!['\u{0915}', '\u{093C}'] };
	assert_eq!(canonical::face_for(&faces, &['\u{0958}']), Some(1), "the decomposing face covers what the view asks about");
	assert_eq!(canonical::face_for(&faces, &['\u{0915}', '\u{093C}']), Some(1));
}

#[test]
// THE BUFFER IS NEVER REWRITTEN, which is the load-bearing half of the policy: every offset the
// pipeline reports is an offset into the caller's own bytes, and the two spellings differ only as
// their own byte lengths do.
fn the_offsets_are_into_the_callers_own_bytes() {
	let composed = "caf\u{00E9}";
	let decomposed = "cafe\u{0301}";
	let items_composed = stages::itemise(Source::new(composed).expect("short enough"));
	let items_decomposed = stages::itemise(Source::new(decomposed).expect("short enough"));
	assert_eq!(items_composed.source.text(), composed, "the source is the caller's string, unchanged");
	assert_eq!(items_decomposed.source.text(), decomposed);
	// Four clusters either way - the composition and its decomposition are ONE cluster.
	assert_eq!(items_composed.clusters.len(), 5);
	assert_eq!(items_decomposed.clusters.len(), 5);
	// And the spans differ only as the two inputs' byte lengths do: 2 bytes for é, 3 for e + acute.
	assert_eq!(items_composed.clusters, std::vec![0, 1, 2, 3, 5]);
	assert_eq!(items_decomposed.clusters, std::vec![0, 1, 2, 3, 6]);
}

#[test]
// ITEMISATION DOES NOT SPLIT A CLUSTER, and a character that decides no script joins the run it is
// in rather than starting one - which is what keeps a space between two Latin words from making
// three items.
fn itemisation_keeps_clusters_whole_and_does_not_split_on_spaces() {
	let items = stages::itemise(Source::new("ab cd").expect("short enough"));
	assert_eq!(items.items.len(), 1, "one Latin run, spaces and all");
	let items = stages::itemise(Source::new("ab \u{05D0}\u{05D1}").expect("short enough"));
	assert_eq!(items.items.len(), 2, "Latin then Hebrew");
	assert_eq!(items.items[0].script, unicode_tables::Script::Latin);
	assert_eq!(items.items[1].script, unicode_tables::Script::Hebrew);
	// Every item boundary is a cluster boundary.
	for item in &items.items {
		assert!(items.clusters.contains(&item.start), "an item starts where a cluster does");
		assert!(items.clusters.contains(&item.end), "and ends where one does");
	}
}

#[test]
// THE ONE MISTAKE THE TYPES EXIST TO PREVENT: bidi reordering is applied PER LINE, after the breaks
// are known. A paragraph reordered once and then wrapped is wrong wherever it wraps - and here there
// is no path to `Visual` that does not go through `Lines`.
fn the_visual_order_is_produced_per_line_and_only_after_wrapping() {
	let faces = TwoFaces { precomposed: std::vec![], decomposed: std::vec![] };
	let _ = &faces;
	let source = Source::new("a\u{05D0}\nb").expect("short enough");
	let items = stages::itemise(source);
	let levelled = stages::resolve_levels(items, ParagraphDirection::Auto);
	// The paragraph is left to right, with one right-to-left letter in it.
	assert_eq!(levelled.levels.paragraph_level, 0);
	let shaped = Shaped { faced: Faced { levelled, faces: std::vec![], mirrored: std::vec![] }, runs: std::vec![] };
	let measured = stages::measure(shaped).expect("no advances to overflow");
	let lines = stages::break_lines(measured, Fixed266::from_pixels(100));
	// The newline is a MANDATORY break, so there are two lines whatever the width is.
	assert_eq!(lines.lines.len(), 2, "a mandatory break ends a line whatever the width");
	let visual = stages::reorder(lines);
	assert_eq!(visual.order.len(), 2, "one visual order PER LINE, not one for the paragraph");
	// The first line holds three characters - the newline that ended it is ON it, which is what makes
	// rule L1's "the whitespace at the end of a line" a thing that exists - and the second holds one.
	assert_eq!(visual.order[0].len(), 3);
	assert_eq!(visual.order[1].len(), 1);
}

/// A face set where each face covers a stated list of characters.
struct Covering(std::vec::Vec<(u16, std::vec::Vec<char>)>);

impl Coverage for Covering {
	fn covers(&self, face: u16, spelling: &[char]) -> bool {
		match self.0.iter().find(|(candidate, _)| *candidate == face) {
			Some((_, set)) => !spelling.is_empty() && spelling.iter().all(|character| set.contains(character)),
			None => false,
		}
	}

	fn faces(&self) -> &[u16] {
		&[]
	}
}

#[test]
// THE ORDER IS STATED AND TOTAL, in three bands: the faces preferred for this script AND language,
// then for the script whatever the language, then the general ones, and the last resort last.
//
// FALLBACK THAT DEPENDS ON DIRECTORY ORDER IS A RENDERING DIFFERENCE BETWEEN TWO MACHINES, which is
// what this replaces: two installations with the same faces found in a different order would pick
// different fonts for the same string, and both would be "just using the system font".
fn the_fallback_order_is_stated_rather_than_found() {
	use unicode_tables::Script;
	let policy = Policy::new(
		&[
			FaceEntry { face: 30, script: None, language: None, rank: 0 },
			FaceEntry { face: 20, script: Some(Script::Han), language: None, rank: 0 },
			FaceEntry { face: 10, script: Some(Script::Han), language: Some(*b"JAN "), rank: 0 },
		],
		Some(99),
	);
	// Japanese Han: the language-specific face, then the script's, then the general one, then the
	// last resort - which is the whole policy in one answer.
	assert_eq!(policy.order_for(Script::Han, *b"JAN "), std::vec![10, 20, 30, 99]);
	// Chinese Han: the language-specific Japanese face is NOT preferred, which is the case that
	// makes a Han face choice a policy rather than a lookup - the same character is drawn
	// differently in the two languages.
	assert_eq!(policy.order_for(Script::Han, *b"ZHS "), std::vec![20, 30, 99]);
	// A script nothing is preferred for falls to the general faces.
	assert_eq!(policy.order_for(Script::Latin, *b"dflt"), std::vec![30, 99]);
	// THE ORDER IS INDEPENDENT OF THE ORDER THE FACES WERE GIVEN IN, which is the property the whole
	// item is about: the same faces listed backwards produce the same answer.
	let reversed = Policy::new(
		&[
			FaceEntry { face: 10, script: Some(Script::Han), language: Some(*b"JAN "), rank: 0 },
			FaceEntry { face: 20, script: Some(Script::Han), language: None, rank: 0 },
			FaceEntry { face: 30, script: None, language: None, rank: 0 },
		],
		Some(99),
	);
	assert_eq!(reversed.order_for(Script::Han, *b"JAN "), policy.order_for(Script::Han, *b"JAN "));
	// And a stated rank decides within a band, with the face id breaking a tie.
	let ranked = Policy::new(
		&[
			FaceEntry { face: 5, script: None, language: None, rank: 2 },
			FaceEntry { face: 7, script: None, language: None, rank: 1 },
			FaceEntry { face: 6, script: None, language: None, rank: 1 },
		],
		None,
	);
	assert_eq!(ranked.order_for(Script::Latin, *b"dflt"), std::vec![6, 7, 5]);
}

#[test]
// FALLBACK OPERATES ON WHOLE CLUSTERS. Splitting a combining sequence across two faces is the defect
// this item exists to prevent: an `e` from one font and its acute from another do not line up.
fn a_cluster_is_never_split_between_two_faces() {
	use unicode_tables::Script;
	let policy = Policy::new(&[FaceEntry { face: 1, script: None, language: None, rank: 0 }, FaceEntry { face: 2, script: None, language: None, rank: 1 }], None);
	// Face 1 has the base letter and NOT the mark; face 2 has both.
	let coverage = Covering(std::vec![(1, std::vec!['e']), (2, std::vec!['e', '\u{0301}', '\u{00E9}'])]);
	let faces = Faces { policy: &policy, coverage: &coverage, script: Script::Latin, language: *b"dflt" };
	// The cluster goes to face 2 whole, rather than the letter to face 1 and the mark to face 2.
	assert_eq!(fallback::face_for_cluster(&faces, &['e', '\u{0301}']), Some(2));
	// A cluster that IS covered by the earlier face still takes it.
	assert_eq!(fallback::face_for_cluster(&faces, &['e']), Some(1));
	// And nothing covers what nothing has.
	assert_eq!(fallback::face_for_cluster(&faces, &['\u{4E00}']), None);
}

#[test]
// A RUN ENDS WHERE THE FACE CHANGES, which is what makes the shaped runs face-homogeneous - the
// shared contract's own requirement, and what a per-character fallback could never produce.
fn the_runs_are_face_homogeneous() {
	use unicode_tables::Script;
	let policy = Policy::new(&[FaceEntry { face: 1, script: None, language: None, rank: 0 }, FaceEntry { face: 2, script: None, language: None, rank: 1 }], None);
	let coverage = Covering(std::vec![(1, std::vec!['a', 'b']), (2, std::vec!['\u{4E00}'])]);
	let faces = Faces { policy: &policy, coverage: &coverage, script: Script::Latin, language: *b"dflt" };
	let clusters = std::vec![std::vec!['a'], std::vec!['b'], std::vec!['\u{4E00}'], std::vec!['a']];
	assert_eq!(fallback::runs_for(&faces, &clusters), Some(std::vec![(0, 2, 1), (2, 3, 2), (3, 4, 1)]));
	// A cluster no face covers fails the whole run rather than being dropped: a run with a hole in
	// it is a run whose clusters no longer line up with the text they came from.
	let clusters = std::vec![std::vec!['a'], std::vec!['z']];
	assert_eq!(fallback::runs_for(&faces, &clusters), None);
}

#[test]
// AN INPUT CAP ALONE IS NOT A BOUND. A paragraph within its code-point ceiling that a pathological
// face expands sixty-four fold is millions of glyphs, every offset in range and every input ceiling
// met - which is precisely the exhaustion the checked offsets do not prevent.
fn a_paragraph_past_the_frozen_output_ceiling_is_refused_by_name() {
	let source = Source::new("ab").expect("short enough");
	let items = stages::itemise(source);
	let levelled = stages::resolve_levels(items, ParagraphDirection::Auto);
	let ceiling = crate::MAX_PARAGRAPH_GLYPHS;
	// Two runs that between them are one glyph over: the ceiling is on the PARAGRAPH and not on any
	// run, so a limit checked per run would pass this.
	let run = |count: usize| stages::ShapedRun { start: 0, end: 1, face: 0, script: unicode_tables::Script::Latin, direction: font_contract::Direction::LeftToRight, glyphs: std::vec![1u16; count], clusters: std::vec![0u32; count], advances: std::vec![Fixed266::ZERO; count] };
	let shaped = Shaped { faced: Faced { levelled, faces: std::vec![], mirrored: std::vec![] }, runs: std::vec![run(ceiling / 2), run(ceiling / 2 + 1)] };
	assert_eq!(stages::measure(shaped).err(), Some(crate::Error::Exceeded { limit: "paragraph output", ceiling: opentype_profile::limits::PARAGRAPH_OUTPUT, asked: ceiling as u64 + 1 }));
}

#[test]
// A CLUSTER NO FACE COVERS WOULD OTHERWISE TRY EVERY FACE IN THE CATALOGUE, once per cluster, and the
// coverage question is not free: it is asked in BOTH canonical spellings. The walk stops at the
// profile's ceiling, and a cluster that would have needed the seventeenth face is answered the way a
// cluster nothing covers is - which is what the last resort is for.
fn the_fallback_walk_stops_at_the_frozen_number_of_faces() {
	let ceiling = opentype_profile::limits::FALLBACK_FACES as usize;
	// The covering face is placed one past the ceiling, behind faces that cover nothing.
	let mut entries: std::vec::Vec<fallback::FaceEntry> = std::vec::Vec::new();
	let mut covering: std::vec::Vec<(u16, std::vec::Vec<char>)> = std::vec::Vec::new();
	for index in 0..=ceiling {
		let face = index as u16;
		entries.push(fallback::FaceEntry { face, script: None, language: None, rank: index as u16 });
		covering.push((face, if index == ceiling { std::vec!['a'] } else { std::vec![] }));
	}
	let policy = fallback::Policy::new(&entries, None);
	let coverage = Covering(covering);
	let faces = fallback::Faces { policy: &policy, coverage: &coverage, script: unicode_tables::Script::Latin, language: *b"dflt" };
	assert_eq!(fallback::face_for_cluster(&faces, &['a']), None, "the face past the ceiling is not reached");

	// Move it to the last position the ceiling admits and it IS reached, which is what makes the
	// number a ceiling rather than an accident of the order.
	let mut covering: std::vec::Vec<(u16, std::vec::Vec<char>)> = std::vec::Vec::new();
	for index in 0..=ceiling {
		covering.push((index as u16, if index == ceiling - 1 { std::vec!['a'] } else { std::vec![] }));
	}
	let coverage = Covering(covering);
	let faces = fallback::Faces { policy: &policy, coverage: &coverage, script: unicode_tables::Script::Latin, language: *b"dflt" };
	assert_eq!(fallback::face_for_cluster(&faces, &['a']), Some(ceiling as u16 - 1));
}

#[test]
// THE ABSOLUTE INPUT CEILING, AT ITS EXACT BOUND AND ONE PAST IT. A ceiling tested only past its
// value could be off by one in either direction and nothing would say so - which for a limit that
// decides whether a document is refused is the difference between "long" and "too long".
fn a_paragraph_is_read_to_the_frozen_ceiling_and_no_further() {
	let ceiling = crate::MAX_PARAGRAPH;
	let exactly: std::string::String = core::iter::repeat_n('a', ceiling).collect();
	assert!(Source::new(&exactly).is_ok(), "a paragraph of exactly the ceiling is laid out");
	let one_more: std::string::String = core::iter::repeat_n('a', ceiling + 1).collect();
	assert_eq!(Source::new(&one_more).err(), Some(crate::Error::TooLong { characters: ceiling + 1, limit: ceiling }));
}

#[test]
// AND THE ABSOLUTE OUTPUT CEILING, AT ITS EXACT BOUND. An input cap does not imply it: a paragraph
// within its code-point limit that a pathological face expands sixty-four fold is millions of glyphs
// with every offset in range.
fn a_paragraph_produces_glyphs_to_the_frozen_ceiling_and_no_further() {
	let source = Source::new("ab").expect("short enough");
	let items = stages::itemise(source);
	let levelled = stages::resolve_levels(items, ParagraphDirection::Auto);
	let ceiling = crate::MAX_PARAGRAPH_GLYPHS;
	let run = |count: usize| stages::ShapedRun { start: 0, end: 1, face: 0, script: unicode_tables::Script::Latin, direction: font_contract::Direction::LeftToRight, glyphs: std::vec![1u16; count], clusters: std::vec![0u32; count], advances: std::vec![Fixed266::ZERO; count] };
	let shaped = Shaped { faced: Faced { levelled, faces: std::vec![], mirrored: std::vec![] }, runs: std::vec![run(ceiling)] };
	assert!(stages::measure(shaped).is_ok(), "exactly the ceiling's worth of glyphs is measured");
}
