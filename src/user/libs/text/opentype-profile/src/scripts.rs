//! The scripts this profile SHAPES, and the languages whose forms it selects.
//!
//! "INDIC" IS NOT A PROFILE ENTRY, WHICH IS WHY EVERY SCRIPT IS NAMED. A category is not something
//! anybody can implement or test against: Devanagari and Malayalam are both "Indic" and their
//! reordering rules differ, so a profile that named the category would be one where a missing script
//! and a wrong one look the same from outside. Each entry names the SHAPING CLASS it needs, which is
//! what says how much work it is and which engine it runs through.
//!
//! A SCRIPT OUTSIDE THIS LIST IS A TYPED REFUSAL, and that refusal is a real answer: the text is not
//! rendered wrong, it is reported as unshapeable, and a fallback may then render it with default
//! positioning rather than with rules the font wanted and nothing applied.
//!
//! THE LIST IS DELIBERATELY FINITE AND DELIBERATELY NOT EVERYTHING UNICODE HAS. Every script here is
//! one whose shaping this system undertakes to get right; adding one is a decision with work behind
//! it rather than a line in a table.

/// Which shaping engine a script runs through.
///
/// THE CLASS IS THE WORK. A `Default` script needs the lookups applied in order and nothing else; a
/// `Cursive` one needs joining-form state before any lookup runs; an `IndicReordering` one needs the
/// syllable broken, the matras moved and the cluster put back together. Naming the class beside the
/// script is what makes "supports Tamil" a checkable claim.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShapingClass {
	/// Apply the features in order; no reordering and no joining state.
	Default,
	/// Joining forms - initial, medial, final, isolated - computed before the lookups run.
	Cursive,
	/// Syllable identification, matra and reph reordering, then the lookups.
	IndicReordering,
	/// The Universal Shaping Engine's cluster model, for the scripts whose rules it was written for.
	Universal,
	/// Khmer's own reordering, which the Indic model does not describe.
	Khmer,
	/// Myanmar's own reordering, for the same reason.
	Myanmar,
	/// Jamo composition and decomposition before the lookups.
	Hangul,
}

/// One script the profile shapes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScriptSupport {
	/// The OpenType script tag, which is what `GSUB`/`GPOS` are indexed by.
	pub tag: [u8; 4],
	pub name: &'static str,
	pub class: ShapingClass,
	/// Whether the script is written right to left by default.
	pub right_to_left: bool,
}

/// One language whose forms the profile selects.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LanguageSupport {
	/// The OpenType `LangSys` tag, space-padded as the format stores it.
	pub tag: [u8; 4],
	pub name: &'static str,
	/// What selecting it CHANGES. A language with no stated effect is a language that did not need
	/// to be in a finite list.
	pub effect: &'static str,
}

const fn script(tag: &'static [u8; 4], name: &'static str, class: ShapingClass, right_to_left: bool) -> ScriptSupport {
	ScriptSupport { tag: *tag, name, class, right_to_left }
}

/// The scripts `OpenType Profile 1` shapes.
pub const SCRIPTS: &[ScriptSupport] = &[
	// Alphabetic, left to right.
	script(b"latn", "Latin", ShapingClass::Default, false),
	script(b"grek", "Greek", ShapingClass::Default, false),
	script(b"cyrl", "Cyrillic", ShapingClass::Default, false),
	script(b"armn", "Armenian", ShapingClass::Default, false),
	script(b"geor", "Georgian", ShapingClass::Default, false),
	script(b"ethi", "Ethiopic", ShapingClass::Default, false),
	// Right to left. Hebrew and Thaana do not join; the rest do.
	script(b"hebr", "Hebrew", ShapingClass::Default, true),
	script(b"thaa", "Thaana", ShapingClass::Default, true),
	script(b"arab", "Arabic", ShapingClass::Cursive, true),
	script(b"syrc", "Syriac", ShapingClass::Cursive, true),
	script(b"nko ", "N'Ko", ShapingClass::Cursive, true),
	// The Indic reordering family, named one by one because their rules differ.
	script(b"dev2", "Devanagari", ShapingClass::IndicReordering, false),
	script(b"bng2", "Bengali", ShapingClass::IndicReordering, false),
	script(b"gur2", "Gurmukhi", ShapingClass::IndicReordering, false),
	script(b"gjr2", "Gujarati", ShapingClass::IndicReordering, false),
	script(b"ory2", "Oriya", ShapingClass::IndicReordering, false),
	script(b"tml2", "Tamil", ShapingClass::IndicReordering, false),
	script(b"tel2", "Telugu", ShapingClass::IndicReordering, false),
	script(b"knd2", "Kannada", ShapingClass::IndicReordering, false),
	script(b"mlm2", "Malayalam", ShapingClass::IndicReordering, false),
	script(b"sinh", "Sinhala", ShapingClass::IndicReordering, false),
	// South-east Asian, each with its own model.
	script(b"khmr", "Khmer", ShapingClass::Khmer, false),
	script(b"mym2", "Myanmar", ShapingClass::Myanmar, false),
	script(b"thai", "Thai", ShapingClass::Universal, false),
	script(b"lao ", "Lao", ShapingClass::Universal, false),
	script(b"tibt", "Tibetan", ShapingClass::Universal, false),
	// East Asian.
	script(b"hani", "Han", ShapingClass::Default, false),
	script(b"kana", "Kana", ShapingClass::Default, false),
	script(b"bopo", "Bopomofo", ShapingClass::Default, false),
	script(b"hang", "Hangul", ShapingClass::Hangul, false),
	// The two pseudo-scripts every run passes through.
	script(b"DFLT", "Default", ShapingClass::Default, false),
	script(b"zyyy", "Common", ShapingClass::Default, false),
];

const fn language(tag: &'static [u8; 4], name: &'static str, effect: &'static str) -> LanguageSupport {
	LanguageSupport { tag: *tag, name, effect }
}

/// The languages whose forms the profile selects, each with what it changes.
pub const LANGUAGES: &[LanguageSupport] = &[
	language(b"dflt", "Default", "the script's own features, with no language-specific substitution"),
	language(b"TRK ", "Turkish", "dotless i and its casing pair, through `locl`"),
	language(b"AZE ", "Azerbaijani", "the same dotless i behaviour as Turkish"),
	language(b"CRT ", "Crimean Tatar", "the same dotless i behaviour"),
	language(b"DEU ", "German", "the capital sharp s, and ligature suppression across compound boundaries"),
	language(b"NLD ", "Dutch", "the IJ digraph as one letter for casing and for selection"),
	language(b"CAT ", "Catalan", "the middle dot in `l·l`, which must not become a ligature"),
	language(b"ROM ", "Romanian", "comma-below rather than cedilla on s and t"),
	language(b"MOL ", "Moldavian", "the same comma-below forms"),
	language(b"SRB ", "Serbian", "the italic Cyrillic forms of be, ghe, de, pe and te"),
	language(b"MKD ", "Macedonian", "the italic Cyrillic forms of ghe and de"),
	language(b"ARA ", "Arabic", "the default Arabic language forms"),
	language(b"FAR ", "Persian", "Persian forms of yeh, kaf and the digits"),
	language(b"URD ", "Urdu", "Urdu forms, including the heh and yeh variants"),
	language(b"SND ", "Sindhi", "the Sindhi forms of the implosive letters, which Arabic and Urdu shape differently"),
	language(b"ZHS ", "Chinese, simplified", "the regional glyph forms of shared ideographs"),
	language(b"ZHT ", "Chinese, traditional", "the regional glyph forms of shared ideographs"),
	language(b"JAN ", "Japanese", "the Japanese regional forms"),
	language(b"KOR ", "Korean", "the Korean regional forms"),
];

/// Does the profile shape this script?
pub fn script_support(tag: &[u8; 4]) -> Option<&'static ScriptSupport> {
	SCRIPTS.iter().find(|entry| entry.tag == *tag)
}

/// Does the profile select forms for this language?
///
/// A LANGUAGE THE PROFILE DOES NOT NAME IS NOT A REFUSAL - it falls back to `dflt`, which is what the
/// format itself does with an unknown `LangSys`. That is the one place in this profile where the
/// answer is a fallback rather than a refusal, and it is stated here so the difference is deliberate.
pub fn language_support(tag: &[u8; 4]) -> Option<&'static LanguageSupport> {
	LANGUAGES.iter().find(|entry| entry.tag == *tag)
}

impl ShapingClass {
	pub fn name(self) -> &'static str {
		match self {
			Self::Default => "default",
			Self::Cursive => "cursive-joining",
			Self::IndicReordering => "indic-reordering",
			Self::Universal => "universal",
			Self::Khmer => "khmer",
			Self::Myanmar => "myanmar",
			Self::Hangul => "hangul",
		}
	}
}
