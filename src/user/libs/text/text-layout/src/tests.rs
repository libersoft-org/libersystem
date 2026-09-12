use super::*;

use font_contract::cluster::{Caret, CaretAffinity, Cluster, SourceRange};
use font_contract::{ClusterMap, Direction, FaceIdentity, FaceRef, FileIdentity, Fixed266, Generation, GlyphKind, GlyphRun, KindSelection, PositionedGlyph, RasterisationMode, ScriptTag, VariationCoordinates};

/// Runs are STATED here rather than shaped, because what this layer decides is arithmetic over
/// advances: a fixture that had to shape a font first would be checking the shaper.
fn glyphs(advances: &[i32]) -> std::vec::Vec<PositionedGlyph> {
	advances.iter().enumerate().map(|(index, advance)| PositionedGlyph { glyph: index as u32 + 1, x_offset: Fixed266::ZERO, y_offset: Fixed266::ZERO, x_advance: Fixed266::from_raw(*advance), y_advance: Fixed266::ZERO, kind: GlyphKind::Outline, selection: KindSelection::default() }).collect()
}

fn run<'a>(glyphs: &'a [PositionedGlyph], direction: Direction) -> GlyphRun<'a> {
	GlyphRun { face: FaceRef { face: FaceIdentity { file: FileIdentity([1u8; 32]), index: 0 }, generation: Generation(1) }, size: Fixed266::from_pixels(16), variation: VariationCoordinates::NONE, script: ScriptTag::from_bytes(*b"latn"), direction, mode: RasterisationMode::Grayscale, origin_x: Fixed266::ZERO, origin_y: Fixed266::ZERO, glyphs }
}

/// One cluster per glyph, one source byte each, starting at `from`.
fn simple_clusters(count: usize, from: u32) -> std::vec::Vec<Cluster> {
	(0..count).map(|index| Cluster { source: SourceRange { start: from + index as u32, end: from + index as u32 + 1 }, first_glyph: index as u16, glyph_count: 1, first_caret: 0, caret_count: 0 }).collect()
}

fn identity_order(count: usize) -> std::vec::Vec<u16> {
	(0..count as u16).collect()
}

#[test]
// START IS THE LEFT IN A LEFT-TO-RIGHT PARAGRAPH AND THE RIGHT IN A RIGHT-TO-LEFT ONE. A layout
// spelled left and right makes every right-to-left document ragged on the wrong side - a defect a
// reader of that script sees immediately and a developer of it never does.
fn a_line_is_aligned_against_the_edge_its_paragraph_begins_at() {
	let advances = glyphs(&[100, 100, 100]);
	let clusters = simple_clusters(3, 0);
	let order = identity_order(3);
	let map = ClusterMap::new(&clusters, &order, &[], 3).expect("a mapping this fixture built");
	let pairs = [(run(&advances, Direction::LeftToRight), map)];
	let box_width = Fixed266::from_raw(500);

	for (direction, alignment, expected) in [
		(ParagraphDirection::LeftToRight, Alignment::Start, 0),
		(ParagraphDirection::LeftToRight, Alignment::End, 200),
		(ParagraphDirection::RightToLeft, Alignment::Start, 200),
		(ParagraphDirection::RightToLeft, Alignment::End, 0),
		// Left and Right mean the pixels whatever the language, which is what a column of numbers
		// needs.
		(ParagraphDirection::RightToLeft, Alignment::Left, 0),
		(ParagraphDirection::LeftToRight, Alignment::Right, 200),
	] {
		let mut line = compose(&pairs, box_width, direction).expect("a line");
		assert_eq!(line.width, Fixed266::from_raw(300));
		assert_eq!(line.slack(), Fixed266::from_raw(200));
		align(&mut line, alignment).expect("a shift that fits");
		assert_eq!(line.runs[0].x, Fixed266::from_raw(expected), "{direction:?} {alignment:?}");
	}
}

#[test]
// THE ODD HALF-UNIT GOES TO THE START SIDE. Rounding toward zero would move a centred line left in a
// right-to-left paragraph, which is the one direction it must not drift.
fn a_centred_line_puts_its_odd_unit_on_the_side_the_text_begins_at() {
	let advances = glyphs(&[100]);
	let clusters = simple_clusters(1, 0);
	let order = identity_order(1);
	let map = ClusterMap::new(&clusters, &order, &[], 1).expect("a mapping");
	let pairs = [(run(&advances, Direction::LeftToRight), map)];
	// A slack of 101 cannot be halved evenly.
	let box_width = Fixed266::from_raw(201);

	let mut line = compose(&pairs, box_width, ParagraphDirection::LeftToRight).expect("a line");
	align(&mut line, Alignment::Centre).expect("a shift");
	assert_eq!(line.runs[0].x, Fixed266::from_raw(50), "the extra unit is to the RIGHT of a left-to-right line, so the text starts one unit earlier");

	let mut line = compose(&pairs, box_width, ParagraphDirection::RightToLeft).expect("a line");
	align(&mut line, Alignment::Centre).expect("a shift");
	assert_eq!(line.runs[0].x, Fixed266::from_raw(51), "and to the LEFT of a right-to-left one");
}

#[test]
// A LINE FILLED TO ITS BOX AND NOT NEARLY. Dropping the remainder leaves justified text a few
// sixty-fourths short of its measure, which shows as a ragged right edge - the exact thing
// justification was asked for.
fn justification_fills_the_line_exactly() {
	let advances = glyphs(&[100, 20, 100, 20, 100]);
	let clusters = simple_clusters(5, 0);
	let order = identity_order(5);
	let map = ClusterMap::new(&clusters, &order, &[], 5).expect("a mapping");
	let pairs = [(run(&advances, Direction::LeftToRight), map)];
	// Natural width 340, box 441: a slack of 101 over two equal points, which does not divide.
	let mut line = compose(&pairs, Fixed266::from_raw(441), ParagraphDirection::LeftToRight).expect("a line");
	let elastic = [Elastic { run: 0, glyph: 1, weight: 1 }, Elastic { run: 0, glyph: 3, weight: 1 }];
	assert_eq!(justify(&mut line, &elastic, false).expect("a justification"), Justification::Filled);
	assert_eq!(line.width, Fixed266::from_raw(441), "the line reaches its box exactly");
	assert_eq!(line.slack(), Fixed266::ZERO);
}

#[test]
// SPREADING THE SLACK BETWEEN EVERY PAIR OF GLYPHS IS WHAT A LAYOUT DOES WHEN IT REFUSES TO ADMIT IT
// CANNOT JUSTIFY A LINE, and it is worse than a ragged edge: it changes the rhythm of the word
// itself, and on a line with one long word it is unreadable.
fn a_line_with_nothing_to_stretch_is_left_ragged_rather_than_letter_spaced() {
	let advances = glyphs(&[100, 100]);
	let clusters = simple_clusters(2, 0);
	let order = identity_order(2);
	let map = ClusterMap::new(&clusters, &order, &[], 2).expect("a mapping");
	let pairs = [(run(&advances, Direction::LeftToRight), map)];
	let mut line = compose(&pairs, Fixed266::from_raw(600), ParagraphDirection::LeftToRight).expect("a line");
	assert_eq!(justify(&mut line, &[], false).expect("an answer"), Justification::NoOpportunity);
	assert_eq!(line.width, Fixed266::from_raw(200), "the line is not widened by a single unit");
	// A point named with weight zero is named and NOT used, which is how a caller lists every
	// candidate and lets the policy decide.
	let named = [Elastic { run: 0, glyph: 0, weight: 0 }];
	assert_eq!(justify(&mut line, &named, false).expect("an answer"), Justification::NoOpportunity);
	assert_eq!(line.width, Fixed266::from_raw(200));

	// AND THE LAST LINE OF A PARAGRAPH IS NOT JUSTIFIED, which is the single most recognisable sign
	// of a layout engine no typographer ever read.
	let elastic = [Elastic { run: 0, glyph: 0, weight: 1 }];
	assert_eq!(justify(&mut line, &elastic, true).expect("an answer"), Justification::NoOpportunity);
	assert_eq!(line.width, Fixed266::from_raw(200));
	// While the same line, not last, IS filled.
	assert_eq!(justify(&mut line, &elastic, false).expect("an answer"), Justification::Filled);
	assert_eq!(line.width, Fixed266::from_raw(600));
}

#[test]
// A TAB IS NOT A WIDE SPACE: its width depends on where the pen already is, which makes it the only
// thing on a line whose advance is not a property of the font.
fn a_tab_advances_strictly_past_the_pen() {
	let stops = TabStops::every(Fixed266::from_raw(100));
	assert_eq!(stops.advance_from(Fixed266::ZERO).expect("an advance"), Fixed266::from_raw(100));
	assert_eq!(stops.advance_from(Fixed266::from_raw(40)).expect("an advance"), Fixed266::from_raw(60));
	// STRICTLY PAST, and this is the case that matters: a tab landing ON a stop advances to the NEXT
	// one, so two tabs in a row are two columns rather than one.
	assert_eq!(stops.advance_from(Fixed266::from_raw(100)).expect("an advance"), Fixed266::from_raw(100));

	// Stated stops, with the interval continuing past the last of them - counted from the LINE'S
	// start, so a stated stop that is not a multiple of the interval does not shift every column
	// after it.
	let explicit = [Fixed266::from_raw(30), Fixed266::from_raw(250)];
	let stops = TabStops::stated(&explicit, Fixed266::from_raw(100));
	assert_eq!(stops.advance_from(Fixed266::ZERO).expect("an advance"), Fixed266::from_raw(30));
	assert_eq!(stops.advance_from(Fixed266::from_raw(30)).expect("an advance"), Fixed266::from_raw(220));
	assert_eq!(stops.advance_from(Fixed266::from_raw(250)).expect("an advance"), Fixed266::from_raw(100), "past the last stated stop the interval continues from IT");
	assert_eq!(stops.advance_from(Fixed266::from_raw(380)).expect("an advance"), Fixed266::from_raw(70));

	// AN INTERVAL OF ZERO ADVANCES BY NOTHING rather than looping: it is a document that states no
	// stops at all, and a layer searching for the next one would not return.
	let stops = TabStops::every(Fixed266::ZERO);
	assert_eq!(stops.advance_from(Fixed266::from_raw(10)).expect("an advance"), Fixed266::ZERO);
}

#[test]
// A LIGATURE IS ONE GLYPH FOR SEVERAL CHARACTERS, and a caret that could only stand at its two ends
// makes the middle of a word unreachable: press the arrow key and the caret jumps two characters. The
// dividing position is the face's own, because `fi` is not two equal halves.
fn a_caret_inside_a_ligature_lands_where_the_face_says_it_divides() {
	let advances = glyphs(&[120]);
	// Two source characters, one glyph: the first cluster owns it and carries the divider, the second
	// was absorbed and owns no glyph of its own.
	let clusters = [
		Cluster { source: SourceRange { start: 0, end: 1 }, first_glyph: 0, glyph_count: 1, first_caret: 0, caret_count: 1 },
		Cluster { source: SourceRange { start: 1, end: 2 }, first_glyph: 0, glyph_count: 0, first_caret: 0, caret_count: 0 },
	];
	let carets = [Fixed266::from_raw(70)];
	let order = identity_order(2);
	let map = ClusterMap::new(&clusters, &order, &carets, 2).expect("a mapping");
	let pairs = [(run(&advances, Direction::LeftToRight), map)];
	let line = compose(&pairs, Fixed266::from_raw(400), ParagraphDirection::LeftToRight).expect("a line");

	let at = |offset: u32, affinity: CaretAffinity| caret_position(&line, Caret { offset, affinity }).expect("readable");
	assert_eq!(at(0, CaretAffinity::Leading), Some(Fixed266::ZERO));
	// THE DIVIDER, at seventy rather than at the sixty an equal split would have given.
	assert_eq!(at(1, CaretAffinity::Leading), Some(Fixed266::from_raw(70)));
	assert_eq!(at(2, CaretAffinity::Trailing), Some(Fixed266::from_raw(120)));
}

#[test]
// ONE CONTIGUOUS RANGE OF TEXT IS NOT ONE RECTANGLE. In mixed-direction content a selection across a
// direction boundary is drawn as separate pieces with a gap between them, and a layer that assumed
// one rectangle draws a highlight over text the user did not select.
fn a_selection_across_a_direction_boundary_is_several_pieces_and_touching_ones_are_merged() {
	// Three runs on one line: Latin, then Arabic drawn right to left, then Latin again. The middle
	// run's clusters are in logical order and its glyphs are in visual order, which is the seam's own
	// arrangement.
	let latin_one = glyphs(&[50, 50]);
	let arabic = glyphs(&[40, 40]);
	let latin_two = glyphs(&[50, 50]);

	let clusters_one = simple_clusters(2, 0);
	// The Arabic run covers bytes 2..4, and its first cluster is drawn at the RIGHT of the run.
	let clusters_two = [
		Cluster { source: SourceRange { start: 2, end: 3 }, first_glyph: 1, glyph_count: 1, first_caret: 0, caret_count: 0 },
		Cluster { source: SourceRange { start: 3, end: 4 }, first_glyph: 0, glyph_count: 1, first_caret: 0, caret_count: 0 },
	];
	let clusters_three = simple_clusters(2, 4);
	let order = identity_order(2);
	let reversed: std::vec::Vec<u16> = std::vec![1, 0];

	let pairs = [
		(run(&latin_one, Direction::LeftToRight), ClusterMap::new(&clusters_one, &order, &[], 6).expect("a mapping")),
		(run(&arabic, Direction::RightToLeft), ClusterMap::new(&clusters_two, &reversed, &[], 6).expect("a mapping")),
		(run(&latin_two, Direction::LeftToRight), ClusterMap::new(&clusters_three, &order, &[], 6).expect("a mapping")),
	];
	let line = compose(&pairs, Fixed266::from_raw(400), ParagraphDirection::LeftToRight).expect("a line");
	assert_eq!(line.width, Fixed266::from_raw(280));

	// The whole line selected is ONE piece: ten adjacent rectangles would leave a seam at every
	// boundary on a surface that blends.
	let whole = selection_pieces(&line, SourceRange { start: 0, end: 6 }).expect("pieces");
	assert_eq!(whole, std::vec![Selection { left: Fixed266::ZERO, right: Fixed266::from_raw(280) }]);

	// A range that starts inside the Latin run and ends inside the Arabic one, taking byte 3 - which
	// is drawn at the LEFT of the Arabic run - is two separate pieces with a gap.
	let split = selection_pieces(&line, SourceRange { start: 0, end: 2 }).expect("pieces");
	assert_eq!(split, std::vec![Selection { left: Fixed266::ZERO, right: Fixed266::from_raw(100) }]);
	let across = selection_pieces(&line, SourceRange { start: 1, end: 3 }).expect("pieces");
	// Byte 1 is the second Latin cluster (50..100) and byte 2 is the Arabic cluster drawn SECOND,
	// which sits at 140..180 - so the two do not touch.
	assert_eq!(across, std::vec![Selection { left: Fixed266::from_raw(50), right: Fixed266::from_raw(100) }, Selection { left: Fixed266::from_raw(140), right: Fixed266::from_raw(180) }]);
}

#[test]
// A CARET AT A LINE'S OWN EDGE BELONGS TO ONE LINE, and affinity is what says which. Answering from
// both is how a caret is drawn twice; answering from neither is how it disappears at a wrap.
fn a_caret_at_a_line_edge_is_owned_by_one_line_and_its_affinity_says_which() {
	let advances = glyphs(&[50, 50]);
	let clusters = simple_clusters(2, 4);
	let order = identity_order(2);
	let map = ClusterMap::new(&clusters, &order, &[], 10).expect("a mapping");
	let pairs = [(run(&advances, Direction::LeftToRight), map)];
	let line = compose(&pairs, Fixed266::from_raw(400), ParagraphDirection::LeftToRight).expect("a line");

	let at = |offset: u32, affinity: CaretAffinity| caret_position(&line, Caret { offset, affinity }).expect("readable");
	// At the line's FIRST offset: a leading caret belongs here, a trailing one to the line before.
	assert_eq!(at(4, CaretAffinity::Leading), Some(Fixed266::ZERO));
	assert_eq!(at(4, CaretAffinity::Trailing), None);
	// At its LAST: a trailing caret belongs here, a leading one to the line after.
	assert_eq!(at(6, CaretAffinity::Trailing), Some(Fixed266::from_raw(100)));
	assert_eq!(at(6, CaretAffinity::Leading), None);
	// An offset not on this line at all is not answered from it.
	assert_eq!(at(9, CaretAffinity::Leading), None);
	assert_eq!(at(0, CaretAffinity::Leading), None);
}

#[test]
// THE ELLIPSIS GOES AT THE PARAGRAPH'S END, which in a right-to-left paragraph is the LEFT. Always
// putting it on the right truncates an Arabic line at its beginning and marks it at its end, which is
// a sentence with its first word missing and a mark saying the last one is.
fn truncation_stops_at_a_cluster_boundary_and_marks_the_paragraph_s_own_end() {
	let advances = glyphs(&[100, 100, 100, 100]);
	let clusters = simple_clusters(4, 0);
	let order = identity_order(4);
	let map = ClusterMap::new(&clusters, &order, &[], 4).expect("a mapping");
	let ellipsis = Fixed266::from_raw(60);

	// A line that fits is not touched.
	let pairs = [(run(&advances, Direction::LeftToRight), map)];
	let line = compose(&pairs, Fixed266::from_raw(900), ParagraphDirection::LeftToRight).expect("a line");
	let answer = truncate(&line, ellipsis).expect("an answer");
	assert!(!answer.truncated);
	assert_eq!(answer.kept, SourceRange { start: 0, end: 4 });

	// A box of 260 leaves 200 for the text once the ellipsis is allowed for: two clusters, and the
	// cut is at a CLUSTER boundary rather than inside one.
	let line = compose(&pairs, Fixed266::from_raw(260), ParagraphDirection::LeftToRight).expect("a line");
	let answer = truncate(&line, ellipsis).expect("an answer");
	assert!(answer.truncated);
	assert_eq!(answer.kept, SourceRange { start: 0, end: 2 });
	assert_eq!(answer.ellipsis_at, Fixed266::from_raw(200));

	// The same line in a right-to-left paragraph keeps the same TEXT and puts the mark on the other
	// side.
	let line = compose(&pairs, Fixed266::from_raw(260), ParagraphDirection::RightToLeft).expect("a line");
	let answer = truncate(&line, ellipsis).expect("an answer");
	assert_eq!(answer.kept, SourceRange { start: 0, end: 2 });
	assert_eq!(answer.ellipsis_at, Fixed266::from_raw(0), "the kept text is at the right, so the mark is to its left");
}

#[test]
// A MAPPING THAT DOES NOT DESCRIBE ITS RUN IS REFUSED rather than laid out around: every caret
// answered from it would be wrong in a way nothing downstream could detect.
fn a_mapping_that_does_not_describe_its_run_is_refused() {
	let advances = glyphs(&[100]);
	// A cluster naming two glyphs in a run that has one.
	let clusters = [Cluster { source: SourceRange { start: 0, end: 1 }, first_glyph: 0, glyph_count: 2, first_caret: 0, caret_count: 0 }];
	let order = identity_order(1);
	let map = ClusterMap::new(&clusters, &order, &[], 1).expect("the seam checks the map, not the run");
	let pairs = [(run(&advances, Direction::LeftToRight), map)];
	assert_eq!(compose(&pairs, Fixed266::from_raw(400), ParagraphDirection::LeftToRight).err(), Some(Error::Mismatched));
}
