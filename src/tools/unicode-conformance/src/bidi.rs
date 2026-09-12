//! The two normative bidi files, run in full.
//!
//! TWO FILES BECAUSE THEY TEST DIFFERENT HALVES. `BidiTest` states CLASS sequences and every
//! paragraph direction each should be run at - thousands of orderings, with no real characters in
//! them and therefore no brackets. `BidiCharacterTest` states real code points, which is the only
//! way to exercise rule N0's paired brackets at all. An implementation that passes one and not the
//! other is wrong in exactly the half it did not run.

use std::path::Path;

use unicode_bidi::{ParagraphDirection, reorder_visual};
use unicode_tables::BidiClass;

/// How many failures are printed before the rest are counted.
const SHOWN: usize = 8;

/// The class a `BidiTest` token names.
///
/// BY THE UCD SPELLING the enum itself gives back, rather than by a table written here: a second
/// list of the class names is a second thing to be wrong, and this one would be wrong in the
/// direction that makes a conformance run pass.
fn class_of(token: &str) -> Option<BidiClass> {
	(0u8..=32).map(BidiClass::from_ordinal).find(|class| class.ucd_name() == token)
}

/// `BidiTest.txt`: `@Levels`, `@Reorder` and then class sequences with a direction bitset.
pub fn run_class_file(path: &Path) -> bool {
	let text = match std::fs::read_to_string(path) {
		Ok(text) => text,
		Err(error) => {
			eprintln!("unicode-conformance: cannot read {}: {error}", path.display());
			return false;
		}
	};
	let mut expected_levels: Vec<Option<u8>> = Vec::new();
	let mut expected_order: Vec<usize> = Vec::new();
	let mut total = 0usize;
	let mut failures = 0usize;
	for (number, line) in text.lines().enumerate() {
		let line = line.split('#').next().unwrap_or("").trim();
		if line.is_empty() {
			continue;
		}
		if let Some(rest) = line.strip_prefix("@Levels:") {
			expected_levels = rest.split_whitespace().map(|token| if token == "x" { None } else { token.parse::<u8>().ok() }).collect();
			continue;
		}
		if let Some(rest) = line.strip_prefix("@Reorder:") {
			expected_order = rest.split_whitespace().filter_map(|token| token.parse::<usize>().ok()).collect();
			continue;
		}
		let Some((sequence, bitset)) = line.split_once(';') else { continue };
		let Ok(bitset) = bitset.trim().parse::<u8>() else { continue };
		let classes: Option<Vec<BidiClass>> = sequence.split_whitespace().map(class_of).collect();
		let Some(classes) = classes else {
			eprintln!("unicode-conformance: bidi line {}: a class this tree does not have: {sequence}", number + 1);
			return false;
		};
		// The bitset says which paragraph directions this case is stated for: 1 auto, 2 left to
		// right, 4 right to left.
		for (bit, direction) in [(1u8, ParagraphDirection::Auto), (2, ParagraphDirection::LeftToRight), (4, ParagraphDirection::RightToLeft)] {
			if bitset & bit == 0 {
				continue;
			}
			total += 1;
			if !check(&classes, direction, &expected_levels, &expected_order, &mut failures, number + 1, sequence) {
				// counted inside
			}
		}
	}
	report("bidi (class sequences)", total, failures)
}

/// One case of either file.
fn check(classes: &[BidiClass], direction: ParagraphDirection, expected_levels: &[Option<u8>], expected_order: &[usize], failures: &mut usize, line: usize, shown: &str) -> bool {
	let mut levels = unicode_bidi::levels_of_classes(classes, direction);
	levels.reset_whitespace(classes);
	let removed = removed_of(classes);
	let produced_levels: Vec<Option<u8>> = levels.levels.iter().enumerate().map(|(index, level)| if removed[index] { None } else { Some(*level) }).collect();
	let produced_order = reorder_visual(&levels.levels, &removed);
	if produced_levels == expected_levels && produced_order == expected_order {
		return true;
	}
	*failures += 1;
	if *failures <= SHOWN {
		eprintln!("unicode-conformance: bidi line {line} ({shown}) at {direction:?}");
		eprintln!("unicode-conformance:   expected levels {expected_levels:?} order {expected_order:?}");
		eprintln!("unicode-conformance:   produced levels {produced_levels:?} order {produced_order:?}");
	}
	false
}

/// Which characters rule X9 removes - the ones the file writes as `x` and leaves out of the order.
fn removed_of(classes: &[BidiClass]) -> Vec<bool> {
	classes.iter().map(|class| matches!(class, BidiClass::RLE | BidiClass::LRE | BidiClass::RLO | BidiClass::LRO | BidiClass::PDF | BidiClass::BN)).collect()
}

/// `BidiCharacterTest.txt`: code points, the direction, the paragraph level, the levels and the order.
pub fn run_character_file(path: &Path) -> bool {
	let text = match std::fs::read_to_string(path) {
		Ok(text) => text,
		Err(error) => {
			eprintln!("unicode-conformance: cannot read {}: {error}", path.display());
			return false;
		}
	};
	let mut total = 0usize;
	let mut failures = 0usize;
	for (number, line) in text.lines().enumerate() {
		let line = line.split('#').next().unwrap_or("").trim();
		if line.is_empty() {
			continue;
		}
		let fields: Vec<&str> = line.split(';').collect();
		if fields.len() < 5 {
			continue;
		}
		let characters: Option<String> = fields[0].split_whitespace().map(|token| u32::from_str_radix(token, 16).ok().and_then(char::from_u32)).collect();
		let Some(characters) = characters else { continue };
		let direction = match fields[1].trim() {
			"0" => ParagraphDirection::LeftToRight,
			"1" => ParagraphDirection::RightToLeft,
			_ => ParagraphDirection::Auto,
		};
		let Ok(expected_paragraph) = fields[2].trim().parse::<u8>() else { continue };
		let expected_levels: Vec<Option<u8>> = fields[3].split_whitespace().map(|token| if token == "x" { None } else { token.parse::<u8>().ok() }).collect();
		let expected_order: Vec<usize> = fields[4].split_whitespace().filter_map(|token| token.parse::<usize>().ok()).collect();
		total += 1;

		let classes: Vec<BidiClass> = characters.chars().map(unicode_tables::bidi_class).collect();
		let mut levels = unicode_bidi::levels(&characters, direction);
		levels.reset_whitespace(&classes);
		let removed = removed_of(&classes);
		let produced_levels: Vec<Option<u8>> = levels.levels.iter().enumerate().map(|(index, level)| if removed[index] { None } else { Some(*level) }).collect();
		let produced_order = reorder_visual(&levels.levels, &removed);
		if levels.paragraph_level == expected_paragraph && produced_levels == expected_levels && produced_order == expected_order {
			continue;
		}
		failures += 1;
		if failures <= SHOWN {
			eprintln!("unicode-conformance: bidi character line {}: {}", number + 1, fields[0].trim());
			eprintln!("unicode-conformance:   expected paragraph {expected_paragraph} levels {expected_levels:?} order {expected_order:?}");
			eprintln!("unicode-conformance:   produced paragraph {} levels {produced_levels:?} order {produced_order:?}", levels.paragraph_level);
		}
	}
	report("bidi (characters)", total, failures)
}

fn report(what: &str, total: usize, failures: usize) -> bool {
	if failures == 0 {
		println!("unicode-conformance: {what} - {total} cases, all of them");
		return true;
	}
	eprintln!("unicode-conformance: {what} - {failures} of {total} cases FAILED");
	false
}
