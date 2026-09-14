//! WHAT THE CORPUS DECLARES, case by case.
//!
//! EVERY NUMBER HERE COMES FROM THE FACE'S DESIGN and not from a run of the shaper. An advance is
//! what the face's `hmtx` states for that glyph; a kerned advance is that plus the pair adjustment
//! the face declares; a mark's offset is the anchor arithmetic the format defines - the base's
//! anchor, minus the mark's, minus everything the pen advanced between them, the base included.
//!
//! A CASE THAT IS MERELY RECORDED IS NOT A CASE. If one of these ever has to be changed to make the
//! gate pass, the question is which of the two is wrong - and that question is the whole point.

use crate::{Case, Expect};
use unicode_bidi::ParagraphDirection;

const ALPHA: &str = "conformance-alpha.ttf";
const COMPLEX: &str = "conformance-complex.ttf";
const EMOJI: &str = "conformance-emoji.ttf";
const VARIABLE: &str = "conformance-variable.ttf";

/// A glyph that is where its own advance puts it, which is every glyph no rule has moved.
const fn at(glyph: &'static str, cluster: u32, x_advance: i32) -> Expect {
	Expect { glyph, cluster, x_advance, x_offset: 0, y_offset: 0 }
}

/// A mark, which has no advance of its own and sits where its anchors put it.
const fn attached(glyph: &'static str, cluster: u32, x_offset: i32, y_offset: i32) -> Expect {
	Expect { glyph, cluster, x_advance: 0, x_offset, y_offset }
}

pub static CASES: &[Case<'static>] = &[
	// -----------------------------------------------------------------------------------------
	// Latin: the case that proves nothing on its own, and is here because the rest is built on it.
	// -----------------------------------------------------------------------------------------
	Case { name: "a Latin ligature forms and takes its OWN advance", face: ALPHA, text: "fi", expect: &[at("f_i", 0, 600)] },
	Case { name: "a three-component ligature forms", face: ALPHA, text: "ffl", expect: &[at("f_f_l", 0, 900)] },
	// THE LONGEST MATCH WINS, and the shorter one still forms after it. A shaper that took the
	// two-component entry first would render `ffl` as `f` followed by a ligature that is not there.
	Case { name: "the longest ligature is tried first and the shorter still forms", face: ALPHA, text: "ffi", expect: &[at("f", 0, 340), at("f_i", 1, 600)] },
	// The kern is on the FIRST glyph's advance, which is what a pair adjustment is.
	Case { name: "a kerning pair narrows the first glyph", face: ALPHA, text: "AV", expect: &[at("A", 0, 700 - 80), at("V", 1, 700)] },
	Case { name: "the other order is a different pair", face: ALPHA, text: "VA", expect: &[at("V", 0, 700 - 60), at("A", 1, 700)] },
	Case { name: "a pair the face does not declare is not kerned", face: ALPHA, text: "TV", expect: &[at("T", 0, 660), at("V", 1, 700)] },
	// -----------------------------------------------------------------------------------------
	// Arabic: all four joining forms, from a face that carries them and a shaper that must decide
	// which one each letter is in before any lookup runs.
	// -----------------------------------------------------------------------------------------
	Case { name: "an Arabic letter alone is isolated", face: ALPHA, text: "\u{0628}", expect: &[at("beh.isol", 0, 620)] },
	Case { name: "two joining letters are initial and final", face: ALPHA, text: "\u{0628}\u{062C}", expect: &[at("beh.init", 0, 520), at("jeem.fina", 2, 580)] },
	Case { name: "three give all four forms their turn", face: ALPHA, text: "\u{0628}\u{062C}\u{0628}", expect: &[at("beh.init", 0, 520), at("jeem.medi", 2, 460), at("beh.fina", 4, 600)] },
	// ALEF JOINS ONLY TO ITS RIGHT, so it has no initial form and the letter AFTER it starts again.
	// A shaper that decided forms by position rather than by joining type would give both letters
	// here a form the face does not even carry.
	Case { name: "a right-joining letter does not let the next one join back", face: ALPHA, text: "\u{0627}\u{0628}", expect: &[at("alef.isol", 0, 340), at("beh.isol", 2, 620)] },
	Case { name: "and takes a final form when something joins into it", face: ALPHA, text: "\u{0628}\u{0627}", expect: &[at("beh.init", 0, 520), at("alef.fina", 2, 380)] },
	// THE REQUIRED LIGATURE IS FORMED FROM THE POSITIONAL FORMS, which is why the lookup order in
	// the face is what it is: `rlig` runs after the joining lookups, never before them.
	Case { name: "the required ligature forms from the positional forms", face: ALPHA, text: "\u{0644}\u{0627}", expect: &[at("lam_alef", 0, 600)] },
	// -----------------------------------------------------------------------------------------
	// Hebrew: marks, and the two classes one base carries.
	// -----------------------------------------------------------------------------------------
	// 300 - 45 - 620: the base's anchor, less the mark's, less the advance the pen made over the base.
	Case { name: "a mark is placed by its anchors", face: ALPHA, text: "\u{05D0}\u{05BC}", expect: &[at("he.alef", 0, 620), attached("dagesh", 2, -365, 65)] },
	// TWO CLASSES ON ONE BASE. A shaper that ignored the mark class would put both marks at the same
	// anchor, which is a dot on top of a vowel rather than one inside the letter and one below it.
	Case { name: "two marks of different classes take different anchors", face: ALPHA, text: "\u{05D0}\u{05BC}\u{05B8}", expect: &[at("he.alef", 0, 620), attached("dagesh", 2, -365, 65), attached("qamats", 4, -390, 15)] },
	Case { name: "a second base has its own anchors", face: ALPHA, text: "\u{05D1}\u{05B0}", expect: &[at("he.bet", 0, 580), attached("sheva", 2, -340, 45)] },
	// -----------------------------------------------------------------------------------------
	// Devanagari: the reordering that happens BEFORE any lookup, and the conjuncts after it.
	// -----------------------------------------------------------------------------------------
	// THE VOWEL IS WRITTEN AFTER ITS CONSONANT AND DRAWN BEFORE IT. The cluster each glyph carries
	// is what a caret and a hit test are built from, so it must follow the glyph rather than the
	// position: the vowel's cluster is still 3.
	Case { name: "a pre-base vowel sign is drawn before its consonant", face: COMPLEX, text: "\u{0915}\u{093F}", expect: &[at("dv.vowel-i", 3, 260), at("dv.ka", 0, 560)] },
	Case { name: "a half form joins what follows it into one conjunct", face: COMPLEX, text: "\u{0915}\u{094D}\u{0930}", expect: &[at("dv.ka_ra", 0, 560)] },
	Case { name: "a syllable-initial ra becomes the reph", face: COMPLEX, text: "\u{0930}\u{094D}\u{0915}", expect: &[attached("dv.repha", 0, 0, 0), at("dv.ka", 6, 560)] },
	// -----------------------------------------------------------------------------------------
	// Thai: one character DRAWN AS TWO GLYPHS, which is what `ccmp` is for.
	// -----------------------------------------------------------------------------------------
	Case { name: "sara am is decomposed and its upper half attaches", face: COMPLEX, text: "\u{0E01}\u{0E33}", expect: &[at("th.ko", 0, 540), attached("th.nikhahit", 3, -345, -40), at("th.sara-aa", 3, 300)] },
	Case { name: "a tone mark sits over its consonant", face: COMPLEX, text: "\u{0E01}\u{0E48}", expect: &[at("th.ko", 0, 540), attached("th.mai-ek", 3, -340, -160)] },
	// -----------------------------------------------------------------------------------------
	// Khmer: the coeng that binds what follows into a subjoined form under the base.
	// -----------------------------------------------------------------------------------------
	Case { name: "a coeng stacks the consonant after it below the base", face: COMPLEX, text: "\u{1780}\u{17D2}\u{1781}", expect: &[at("km.ka", 0, 600), attached("km.kha.sub", 3, -520, 20)] },
	// -----------------------------------------------------------------------------------------
	// Emoji: a mapping above the basic plane, and sequences that are ONE glyph.
	// -----------------------------------------------------------------------------------------
	Case { name: "a single emoji is its own glyph", face: EMOJI, text: "\u{1F44D}", expect: &[at("thumbsup", 0, 1000)] },
	// THE MODIFIER IS PART OF THE LIGATURE. A shaper that dropped it would draw the unmodified glyph
	// and a reader would see the wrong person.
	Case { name: "a skin-tone modifier makes one glyph with its base", face: EMOJI, text: "\u{1F44D}\u{1F3FB}", expect: &[at("thumbsup.light", 0, 1000)] },
	// AND THE JOINER IS PART OF IT TOO: five characters, one glyph, one cluster.
	Case { name: "a ZWJ sequence is one glyph and one cluster", face: EMOJI, text: "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F466}", expect: &[at("family.mwb", 0, 1400)] },
];

/// A mixed-direction paragraph and the order its clusters are drawn in.
pub struct Paragraph<'a> {
	pub name: &'static str,
	pub text: &'static str,
	pub direction: ParagraphDirection,
	pub paragraph_level: u8,
	/// Borrowed for the same reason the expectations above are.
	pub order: &'a [usize],
}

pub static PARAGRAPHS: &[Paragraph<'static>] = &[
	// THE RIGHT-TO-LEFT RUN IS REVERSED AND THE REST IS NOT. The two Hebrew letters are clusters 1
	// and 2, and they come out as 2 then 1 with the Latin on either side left where it was.
	Paragraph { name: "a left-to-right paragraph with a right-to-left run", text: "a\u{05D0}\u{05D1}b", direction: ParagraphDirection::Auto, paragraph_level: 0, order: &[0, 2, 1, 3] },
	// AND THE OTHER WAY ROUND: a paragraph whose first strong character is right-to-left takes
	// level 1, and the Latin inside it is a level-2 run that does NOT reverse within itself.
	Paragraph { name: "a right-to-left paragraph with a left-to-right run", text: "\u{05D0}a\u{05D1}", direction: ParagraphDirection::Auto, paragraph_level: 1, order: &[2, 1, 0] },
];

/// One instance of the variable face, and what it measures there.
pub struct Instance {
	pub name: &'static str,
	pub face: &'static str,
	pub glyph: &'static str,
	/// The user coordinate on the one axis, in design units.
	pub coordinate: i32,
	pub advance: i32,
	pub point: usize,
	pub x: i16,
}

/// THE CASE THE PLAN NAMES: metrics that vary INDEPENDENTLY of outlines.
///
/// The face gives the two their own halves of the axis. Above the default the advance moves through
/// `HVAR` and the outline does not; below it the outline moves through `gvar` and the advance does
/// not. A stack that took the advance from the outline's phantom points, or the outline from the
/// advance, gets one of the four rows below wrong - and each row is a number the face declares
/// rather than a number a previous run produced.
pub static INSTANCES: &[Instance] = &[
	Instance { name: "the default instance is the face as drawn", face: VARIABLE, glyph: "bar", coordinate: 400, advance: 600, point: 2, x: 400 },
	Instance { name: "at the top of the axis the advance moves and the outline does not", face: VARIABLE, glyph: "bar", coordinate: 900, advance: 780, point: 2, x: 400 },
	Instance { name: "half way up, half the advance delta", face: VARIABLE, glyph: "bar", coordinate: 650, advance: 690, point: 2, x: 400 },
	Instance { name: "at the bottom the outline moves and the advance does not", face: VARIABLE, glyph: "bar", coordinate: 100, advance: 600, point: 2, x: 550 },
	Instance { name: "half way down, half the outline delta", face: VARIABLE, glyph: "bar", coordinate: 250, advance: 600, point: 2, x: 475 },
];
