//! WHAT THE CORPUS DECLARES, case by case - read by the host gate and by the guest run alike.
//!
//! EVERY NUMBER HERE COMES FROM THE FACE'S DESIGN and not from a run of the shaper. An advance is
//! what the face's `hmtx` states for that glyph; a kerned advance is that plus the pair adjustment
//! the face declares; a mark's offset is the anchor arithmetic the format defines - the base's
//! anchor, minus the mark's, minus everything the pen advanced between them, the base included.
//!
//! A CASE THAT IS MERELY RECORDED IS NOT A CASE. If one of these ever has to be changed to make the
//! gate pass, the question is which of the two is wrong - and that question is the whole point.
//!
//! ONE FILE, TWO READERS. `src/tools/text-corpus` includes it to shape every case on the build machine;
//! `bin/textconf` compiles it to shape the cases named at the end of this file inside a booted guest,
//! on each architecture. It lives beside the guest program rather than beside the host gate because
//! the image build tracks a program's own sources and nothing outside its crate: a guest program that
//! read this file from the gate's directory would keep a stale copy after an expectation changed,
//! while a host gate run by cargo follows the include wherever it points.
//!
//! IT IS `no_std` AND HOLDS NO STORAGE: plain statics of borrowed data, so the guest program carries
//! the same bytes the host gate reads and no allocator stands between them.

use unicode_bidi::ParagraphDirection;

const ALPHA: &str = "conformance-alpha.ttf";
const COMPLEX: &str = "conformance-complex.ttf";
const EMOJI: &str = "conformance-emoji.ttf";

/// One expected glyph: which glyph, which cluster it came from, where it sits, and - for a mark - the
/// glyph it is attached to.
pub struct Expect {
	pub glyph: &'static str,
	pub cluster: u32,
	pub x_advance: i32,
	pub x_offset: i32,
	pub y_offset: i32,
	/// THE GLYPH A MARK IS ATTACHED TO, by its index in this list. The offset above is measured from it
	/// in logical order, so a right-to-left run, drawn the other way round, cannot be placed without it.
	pub base: Option<u16>,
}

/// One shaping case: a string, the face that draws it, and what must come out.
pub struct Case<'a> {
	pub name: &'static str,
	pub face: &'static str,
	pub text: &'static str,
	/// BORROWED RATHER THAN `'static` so a self-test can build a deliberately wrong one and drive it
	/// through the same comparison the real cases use. A second comparison written for the self-test
	/// would be the thing that goes stale.
	pub expect: &'a [Expect],
}

/// A glyph that is where its own advance puts it, which is every glyph no rule has moved.
const fn at(glyph: &'static str, cluster: u32, x_advance: i32) -> Expect {
	Expect { glyph, cluster, x_advance, x_offset: 0, y_offset: 0, base: None }
}

/// A mark the face gives no anchor on the glyph before it: no advance, and nothing to move it by.
const fn unanchored(glyph: &'static str, cluster: u32) -> Expect {
	Expect { glyph, cluster, x_advance: 0, x_offset: 0, y_offset: 0, base: None }
}

/// A mark, which has no advance of its own and sits where its anchors on `base` put it.
const fn attached(glyph: &'static str, cluster: u32, base: u16, x_offset: i32, y_offset: i32) -> Expect {
	Expect { glyph, cluster, x_advance: 0, x_offset, y_offset, base: Some(base) }
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
	Case { name: "a mark is placed by its anchors", face: ALPHA, text: "\u{05D0}\u{05BC}", expect: &[at("he.alef", 0, 620), attached("dagesh", 2, 0, -365, 65)] },
	// TWO CLASSES ON ONE BASE. A shaper that ignored the mark class would put both marks at the same
	// anchor, which is a dot on top of a vowel rather than one inside the letter and one below it.
	Case { name: "two marks of different classes take different anchors", face: ALPHA, text: "\u{05D0}\u{05BC}\u{05B8}", expect: &[at("he.alef", 0, 620), attached("dagesh", 2, 0, -365, 65), attached("qamats", 4, 0, -390, 15)] },
	Case { name: "a second base has its own anchors", face: ALPHA, text: "\u{05D1}\u{05B0}", expect: &[at("he.bet", 0, 580), attached("sheva", 2, 0, -340, 45)] },
	// -----------------------------------------------------------------------------------------
	// Devanagari: the reordering that happens BEFORE any lookup, and the conjuncts after it.
	// -----------------------------------------------------------------------------------------
	// THE VOWEL IS WRITTEN AFTER ITS CONSONANT AND DRAWN BEFORE IT. The cluster each glyph carries
	// is what a caret and a hit test are built from, so it must follow the glyph rather than the
	// position: the vowel's cluster is still 3.
	Case { name: "a pre-base vowel sign is drawn before its consonant", face: COMPLEX, text: "\u{0915}\u{093F}", expect: &[at("dv.vowel-i", 3, 260), at("dv.ka", 0, 560)] },
	Case { name: "a half form joins what follows it into one conjunct", face: COMPLEX, text: "\u{0915}\u{094D}\u{0930}", expect: &[at("dv.ka_ra", 0, 560)] },
	// THE REPH IS WRITTEN FIRST AND DRAWN LAST. `rphf` forms it from the syllable-initial ra and its
	// virama, the final reordering moves it after its base - carrying the ra's cluster, because the
	// cluster follows the glyph - and the face's `abvm` places it: 430 - 100 - 560, the base's
	// anchor less the mark's less the advance the pen made over the base; 620 - 640 up.
	Case { name: "a syllable-initial ra becomes the reph, drawn over the base after it", face: COMPLEX, text: "\u{0930}\u{094D}\u{0915}", expect: &[at("dv.ka", 6, 560), attached("dv.repha", 0, 0, -230, -20)] },
	// AND NOT WHERE THE SYLLABLE DOES NOT PUT THEM. A consonant and virama ending a word is its own
	// base: no half form, which the face would otherwise give it. A ra and virama ending a word have
	// no base after them: no reph, which the face would otherwise form.
	Case { name: "a consonant and virama ending a word take no half form", face: COMPLEX, text: "\u{0915}\u{094D}", expect: &[at("dv.ka", 0, 560), unanchored("dv.virama", 3)] },
	Case { name: "a ra and virama ending a word are not a reph", face: COMPLEX, text: "\u{0915}\u{0930}\u{094D}", expect: &[at("dv.ka", 0, 560), at("dv.ra", 3, 480), unanchored("dv.virama", 6)] },
	// -----------------------------------------------------------------------------------------
	// Thai: one character DRAWN AS TWO GLYPHS, which is what `ccmp` is for.
	// -----------------------------------------------------------------------------------------
	Case { name: "sara am is decomposed and its upper half attaches", face: COMPLEX, text: "\u{0E01}\u{0E33}", expect: &[at("th.ko", 0, 540), attached("th.nikhahit", 3, 0, -345, -40), at("th.sara-aa", 3, 300)] },
	Case { name: "a tone mark sits over its consonant", face: COMPLEX, text: "\u{0E01}\u{0E48}", expect: &[at("th.ko", 0, 540), attached("th.mai-ek", 3, 0, -340, -160)] },
	// -----------------------------------------------------------------------------------------
	// Khmer: the coeng that binds what follows into a subjoined form under the base.
	// -----------------------------------------------------------------------------------------
	Case { name: "a coeng stacks the consonant after it below the base", face: COMPLEX, text: "\u{1780}\u{17D2}\u{1781}", expect: &[at("km.ka", 0, 600), attached("km.kha.sub", 3, 0, -520, 20)] },
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

/// One line of a paragraph, as the pipeline lays it out.
pub struct Line<'a> {
	/// The order the line's characters are drawn in.
	pub order: &'a [usize],
	/// The glyphs its runs were shaped to, run by run, each in logical order.
	pub glyphs: &'a [&'static str],
}

/// A paragraph, through every stage of the pipeline: its lines, the order each is drawn in and the
/// glyphs each was shaped to.
pub struct Paragraph<'a> {
	pub name: &'static str,
	pub face: &'static str,
	pub text: &'static str,
	pub direction: ParagraphDirection,
	pub paragraph_level: u8,
	/// The width lines are broken at, in pixels at the corpus size. Zero breaks only where a break is
	/// mandatory.
	pub width: i16,
	/// Borrowed for the same reason the expectations above are.
	pub lines: &'a [Line<'a>],
}

/// The size the paragraph cases are laid out at, in pixels to the em: the corpus faces are drawn on a
/// grid of a thousand units, so one unit is a tenth of a pixel. It is part of what a paragraph
/// declares, because where a line breaks depends on it.
pub const PARAGRAPH_SIZE: i16 = 100;

pub static PARAGRAPHS: &[Paragraph<'static>] = &[
	// THE RIGHT-TO-LEFT RUN IS REVERSED AND THE REST IS NOT. The two Hebrew letters are clusters 1
	// and 2, and they come out as 2 then 1 with the Latin on either side left where it was.
	Paragraph { name: "a left-to-right paragraph with a right-to-left run", face: ALPHA, text: "T\u{05D0}\u{05D1}V", direction: ParagraphDirection::Auto, paragraph_level: 0, width: 0, lines: &[Line { order: &[0, 2, 1, 3], glyphs: &["T", "he.alef", "he.bet", "V"] }] },
	// AND THE OTHER WAY ROUND: a paragraph whose first strong character is right-to-left takes
	// level 1, and the Latin inside it is a level-2 run that does NOT reverse within itself.
	Paragraph { name: "a right-to-left paragraph with a left-to-right run", face: ALPHA, text: "\u{05D0}T\u{05D1}", direction: ParagraphDirection::Auto, paragraph_level: 1, width: 0, lines: &[Line { order: &[2, 1, 0], glyphs: &["he.alef", "T", "he.bet"] }] },
	// MIRRORING IS APPLIED BEFORE SHAPING. `<` at an odd level is drawn from the face's `>` - and it
	// is not a bracket, so the paired-bracket table could never have answered for it.
	Paragraph { name: "a mirrored character in a right-to-left run is drawn as its mirror", face: ALPHA, text: "\u{05D0}<\u{05D1}", direction: ParagraphDirection::Auto, paragraph_level: 1, width: 0, lines: &[Line { order: &[2, 1, 0], glyphs: &["he.alef", "greater", "he.bet"] }] },
	Paragraph { name: "the same character in a left-to-right run is drawn as itself", face: ALPHA, text: "T<V", direction: ParagraphDirection::Auto, paragraph_level: 0, width: 0, lines: &[Line { order: &[0, 1, 2], glyphs: &["T", "less", "V"] }] },
	// A BREAK THAT CHANGES A JOINING CONTEXT RESHAPES BOTH SIDES. The zero-width space is transparent
	// to joining, so shaped whole the word joins straight through it - the letters either side of it
	// medial. Broken there, each line is shaped on its own: the first ends in a final form and the
	// second begins with an initial one. 150 pixels at the corpus size holds the first word and not
	// both.
	Paragraph { name: "a break at a zero-width space reshapes the letters either side of it", face: ALPHA, text: "\u{0628}\u{0628}\u{200B}\u{0628}\u{0628}", direction: ParagraphDirection::Auto, paragraph_level: 1, width: 150, lines: &[Line { order: &[2, 1, 0], glyphs: &["beh.init", "beh.fina", "zwsp"] }, Line { order: &[1, 0], glyphs: &["beh.init", "beh.fina"] }] },
];

/// THE CASES THE GUEST RUN SHAPES, LAYS OUT AND DRAWS on each architecture, by name.
///
/// ONE OF EACH KIND THE THREE-ARCHITECTURE CLAIM IS ABOUT and not the whole corpus: Arabic in its
/// joining forms and its required ligature, Devanagari reordered before and after its base, Thai
/// decomposed and stacked, Hebrew marks on a right-to-left base, and a paragraph whose two directions
/// meet on one line. Each is drawn as
/// well as shaped, and every glyph of every one of them is an axis-aligned rectangle, so the picture
/// is compared against coverage computed from the outlines rather than against a captured image.
///
/// THE HOST GATE REFUSES A NAME THAT IS NOT A CASE ABOVE, so renaming a case without renaming it here
/// fails on the build machine rather than in a guest.
pub static GUEST_CASES: &[&str] = &[
	"three give all four forms their turn",
	"the required ligature forms from the positional forms",
	"a pre-base vowel sign is drawn before its consonant",
	"a syllable-initial ra becomes the reph, drawn over the base after it",
	"sara am is decomposed and its upper half attaches",
	"a tone mark sits over its consonant",
	"a mark is placed by its anchors",
	"two marks of different classes take different anchors",
];

pub static GUEST_PARAGRAPHS: &[&str] = &["a left-to-right paragraph with a right-to-left run"];
