use super::*;

#[test]
// THE TABLE'S SHAPE IS WHAT THE LOOKUP RESTS ON, so it is checked rather than assumed: sorted by
// `first`, no overlaps, and every run non-empty. An overlap would mean two values for one character
// and a binary search that returns whichever it happened to land on.
fn every_generated_table_is_sorted_and_disjoint() {
	let tables: [(&str, &[(u32, u32, u8)]); 10] = [
		("BidiClass", BIDI_CLASS),
		("GraphemeBreak", GRAPHEME_BREAK),
		("WordBreak", WORD_BREAK),
		("LineBreak", LINE_BREAK),
		("IndicSyllabic", INDIC_SYLLABIC),
		("IndicPositional", INDIC_POSITIONAL),
		("JoiningType", JOINING_TYPE),
		("Script", SCRIPT),
		("GeneralCategory", GENERAL_CATEGORY),
		("IndicConjunctBreak", INDIC_CONJUNCT_BREAK),
	];
	for (name, table) in tables {
		assert!(!table.is_empty(), "{name} generated no runs at all");
		let mut previous_last: Option<u32> = None;
		for (first, last, value) in table {
			assert!(first <= last, "{name}: the run {first:#x}..{last:#x} is empty");
			assert!(*last <= 0x10FFFF, "{name}: {last:#x} is not a code point");
			// `Bidi_Class` is the one property that KEEPS its default runs, because its default is per
			// BLOCK: an unassigned code point in a right-to-left block is `R` while an assigned
			// left-to-right one in the same block is `L`, and dropping the second would make it take
			// the first.
			if name != "BidiClass" {
				assert_ne!(*value, 0, "{name}: a run carrying the default value is a run that need not exist");
			}
			if let Some(previous) = previous_last {
				assert!(previous < *first, "{name}: {first:#x} overlaps or repeats the run ending {previous:#x}");
			}
			previous_last = Some(*last);
		}
	}
	// AND THE PICTOGRAPHIC RANGES, the same way.
	let mut previous_last: Option<u32> = None;
	for (first, last) in EXTENDED_PICTOGRAPHIC {
		assert!(first <= last);
		if let Some(previous) = previous_last {
			assert!(previous < *first);
		}
		previous_last = Some(*last);
	}
}

#[test]
// SPOT VALUES A PERSON CAN CHECK BY HAND, from the files themselves. A table whose shape is right
// and whose CONTENT is wrong passes every structural check, so a handful of characters whose
// properties are not in dispute are asserted directly.
fn the_tables_answer_what_the_ucd_says() {
	assert_eq!(UNICODE_VERSION, "17.0.0");
	// Grapheme break: the two line terminators, a combining mark, the zero-width joiner, a regional
	// indicator and a Hangul syllable part.
	assert_eq!(grapheme_break('\r'), GraphemeBreak::CR);
	assert_eq!(grapheme_break('\n'), GraphemeBreak::LF);
	assert_eq!(grapheme_break('\u{0301}'), GraphemeBreak::Extend, "combining acute is Extend");
	assert_eq!(grapheme_break('\u{200D}'), GraphemeBreak::ZWJ);
	assert_eq!(grapheme_break('\u{1F1E6}'), GraphemeBreak::RegionalIndicator, "REGIONAL INDICATOR SYMBOL LETTER A");
	assert_eq!(grapheme_break('\u{1100}'), GraphemeBreak::L, "HANGUL CHOSEONG KIYEOK");
	assert_eq!(grapheme_break('a'), GraphemeBreak::Other);
	// Word break.
	assert_eq!(word_break('a'), WordBreak::ALetter);
	assert_eq!(word_break('1'), WordBreak::Numeric);
	assert_eq!(word_break('\''), WordBreak::SingleQuote);
	assert_eq!(word_break(' '), WordBreak::WSegSpace);
	assert_eq!(word_break('\u{05D0}'), WordBreak::HebrewLetter, "HEBREW LETTER ALEF");
	// Line break: the classes the rules turn on most.
	assert_eq!(line_break(' '), LineBreak::SP);
	assert_eq!(line_break('-'), LineBreak::HY, "HYPHEN-MINUS is the ambiguous one: it is also a minus sign");
	assert_eq!(line_break('\u{2010}'), LineBreak::HH, "HYPHEN is the unambiguous one, and the class this release added");
	assert_eq!(line_break('('), LineBreak::OP);
	assert_eq!(line_break(')'), LineBreak::CP);
	assert_eq!(line_break('\u{00A0}'), LineBreak::GL, "no-break space glues");
	assert_eq!(line_break('a'), LineBreak::AL);
	assert_eq!(line_break('\u{4E00}'), LineBreak::ID, "a CJK ideograph breaks on either side");
	// The shaping properties.
	assert_eq!(joining_type('\u{0628}'), JoiningType::D, "ARABIC LETTER BEH joins both ways");
	assert_eq!(joining_type('\u{0627}'), JoiningType::R, "ARABIC LETTER ALEF joins only to its right");
	assert_eq!(joining_type('a'), JoiningType::U);
	// THE FILE'S OWN DERIVED DEFAULT: a code point `ArabicShaping.txt` does not list is `T` when its
	// general category is a mark or a format character, and `U` otherwise. Without it every Arabic
	// vowel mark is non-joining rather than transparent, and a mark between two letters breaks the
	// join - which in Arabic is most places.
	assert_eq!(joining_type('\u{064E}'), JoiningType::T, "ARABIC FATHA is transparent, and the file does not list it");
	assert_eq!(joining_type('\u{0301}'), JoiningType::T, "so is any other non-spacing mark");
	assert_eq!(joining_type('\u{200B}'), JoiningType::T, "ZERO WIDTH SPACE is a FORMAT character despite its name, so it is transparent too");
	assert_eq!(joining_type(' '), JoiningType::U, "an ordinary space is non-joining, which is what breaks a word");
	assert_eq!(script('a'), Script::Latin);
	assert_eq!(script('\u{0905}'), Script::Devanagari);
	assert_eq!(script('\u{0628}'), Script::Arabic);
	assert_eq!(general_category('a'), GeneralCategory::Ll);
	assert_eq!(general_category('A'), GeneralCategory::Lu);
	assert_eq!(general_category('\u{0301}'), GeneralCategory::Mn);
	assert_eq!(indic_syllabic('\u{0905}'), IndicSyllabic::VowelIndependent);
	assert_eq!(indic_syllabic('\u{094D}'), IndicSyllabic::Virama, "DEVANAGARI SIGN VIRAMA");
	assert_eq!(indic_positional('\u{093F}'), IndicPositional::Left, "DEVANAGARI VOWEL SIGN I sits left of its base");
	// The Indic conjunct break property GB9c is written in terms of, which grapheme clustering has
	// been wrong without since Unicode 15.1.
	assert_eq!(indic_conjunct_break('\u{094D}'), IndicConjunctBreak::Linker);
	assert_eq!(indic_conjunct_break('\u{0915}'), IndicConjunctBreak::Consonant, "DEVANAGARI LETTER KA");
	assert_eq!(indic_conjunct_break('a'), IndicConjunctBreak::None);
	// The bidi class, including the per-block default an unassigned code point takes.
	assert_eq!(bidi_class('a'), BidiClass::L);
	assert_eq!(bidi_class('\u{05D0}'), BidiClass::R, "HEBREW LETTER ALEF");
	assert_eq!(bidi_class('\u{0628}'), BidiClass::AL, "ARABIC LETTER BEH");
	assert_eq!(bidi_class('\u{0661}'), BidiClass::AN, "ARABIC-INDIC DIGIT ONE");
	assert_eq!(bidi_class('1'), BidiClass::EN);
	assert_eq!(bidi_class('\u{05EB}'), BidiClass::R, "an UNASSIGNED code point in the Hebrew block is right to left, not L");
	assert_eq!(bidi_class('\u{2066}'), BidiClass::LRI);
	assert_eq!(bidi_class('\u{2069}'), BidiClass::PDI);
	// And the pictographic property.
	assert!(is_extended_pictographic('\u{1F600}'), "GRINNING FACE");
	assert!(!is_extended_pictographic('a'));
}

#[test]
// THE UCD SPELLING SURVIVES THE IDENTIFIER. `Regional_Indicator` is a variant called
// `RegionalIndicator`, and a conformance file, a report and a document all name the first.
fn a_value_can_say_what_the_ucd_calls_it() {
	assert_eq!(GraphemeBreak::RegionalIndicator.ucd_name(), "Regional_Indicator");
	assert_eq!(WordBreak::ExtendNumLet.ucd_name(), "ExtendNumLet");
	assert_eq!(IndicSyllabic::VowelDependent.ucd_name(), "Vowel_Dependent");
	assert_eq!(LineBreak::XX.ucd_name(), "XX");
	// AND AN ORDINAL OUT OF RANGE IS THE DEFAULT rather than a panic: the tables this generator
	// writes cannot produce one, and the function is also what reads an ordinal from elsewhere.
	assert_eq!(GraphemeBreak::from_ordinal(200), GraphemeBreak::Other);
	assert_eq!(LineBreak::from_ordinal(0), LineBreak::XX);
}

#[test]
// THE LOOKUP IS A BINARY SEARCH, and the edges are where one goes wrong: the first run, the last
// run, the code point just below a run and the one just above it.
fn the_lookup_answers_at_the_edges() {
	let table: &[(u32, u32, u8)] = &[(0x10, 0x1f, 1), (0x30, 0x30, 2), (0x100, 0x1000, 3)];
	assert_eq!(lookup(table, 0x0f), 0);
	assert_eq!(lookup(table, 0x10), 1);
	assert_eq!(lookup(table, 0x1f), 1);
	assert_eq!(lookup(table, 0x20), 0);
	assert_eq!(lookup(table, 0x30), 2);
	assert_eq!(lookup(table, 0x31), 0);
	assert_eq!(lookup(table, 0x100), 3);
	assert_eq!(lookup(table, 0x1000), 3);
	assert_eq!(lookup(table, 0x1001), 0);
	assert_eq!(lookup(&[], 0x41), 0, "an empty table answers the default rather than reading past it");
}

#[test]
// THE CANONICAL MAPPING BETWEEN THE TWO SPELLINGS OF ONE STRING, which is what the text pipeline's
// coverage question is asked in terms of - not normalisation, and never a rewrite of the caller's
// bytes.
fn the_canonical_decompositions_and_compositions_agree_with_each_other() {
	// é is a letter and a mark, either way round.
	assert_eq!(canonical_decomposition('\u{00E9}'), Some(('e', Some('\u{0301}'))));
	assert_eq!(canonical_composition('e', '\u{0301}'), Some('\u{00E9}'));
	// A character with no decomposition has none, which is not an error.
	assert_eq!(canonical_decomposition('a'), None);
	assert_eq!(canonical_composition('a', 'b'), None);
	// RECURSION IS THE CALLER'S: U+1E17 decomposes to a character that decomposes further, and this
	// answers one step so the caller can stop where its own cluster ends.
	let (first, second) = canonical_decomposition('\u{1E17}').expect("a recursive decomposition");
	assert_eq!(first, '\u{0113}');
	assert!(second.is_some());
	assert!(canonical_decomposition(first).is_some(), "and the first half decomposes again");
	// A COMPATIBILITY DECOMPOSITION IS NOT A CANONICAL ONE. U+FB01 is the `fi` ligature and says it
	// LOOKS like `f` `i`, not that it is - and treating the two alike is how a text engine decides a
	// font covers a character it has no glyph for.
	assert_eq!(canonical_decomposition('\u{FB01}'), None);
	// AND THE COMPOSITION EXCLUSIONS ARE OUT: U+0344 decomposes but must never be recomposed.
	assert!(canonical_decomposition('\u{0344}').is_some());
	assert_eq!(canonical_composition('\u{0308}', '\u{0301}'), None, "a composition exclusion is not a composition");
	// The combining classes that order a decomposition's marks.
	assert_eq!(combining_class('\u{0301}'), 230, "a mark above");
	assert_eq!(combining_class('\u{0327}'), 202, "a mark below");
	assert_eq!(combining_class('a'), 0);
}
