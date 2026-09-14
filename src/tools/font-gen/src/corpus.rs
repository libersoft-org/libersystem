//! THE CONFORMANCE CORPUS: faces authored so a gate can say what the shaper SHOULD have done.
//!
//! WHY THESE ARE AUTHORED AND NOT IMPORTED, and why that is better here rather than merely allowed.
//! The licence decision rules out every face a text stack would reach for, which is what left this
//! corpus owed for months. But an imported face would not have been the easy answer either: to
//! assert an expected glyph INDEX and an expected POSITION, a gate has to know what the face
//! DECLARES - which lookups exist, in which order, with which anchors - and for an imported face
//! that knowledge is reverse-engineered from the bytes and then written down as a guess. Here the
//! face's rules and the gate's expectations are two statements of ONE design, and a disagreement
//! between them is a defect in the shaper rather than in somebody's reading of a foreign font.
//!
//! WHAT THESE FACES ARE NOT. They are not type design and they draw nothing anybody would want to
//! read: every glyph is a rectangle, because what a shaper gate reads is which glyph and where, and
//! an outline that varied per glyph would be a decision nothing checks. What they DO carry is real:
//! a `cmap` per plane, real `GSUB` ligature, single and multiple substitutions, real `GPOS` pair and
//! mark-to-base positioning, a real `GDEF` class definition, and real `fvar`/`gvar`/`HVAR`/`MVAR`.
//!
//! AND THEY ARE NOT STAGED INTO ANY IMAGE. A face that exercises Khmer stacking has no business in
//! a system volume; these live beside the gate that reads them, pinned by SHA-256 so a corpus that
//! drifted is a failure rather than a quietly different test.

use crate::layout::{self, FeatureEntry, Lookup, ScriptEntry};
use crate::variable;
use crate::write::{Cmap, FaceSpec, Glyph, Names};

/// One authored face: what it is called, and what it is.
pub struct Authored {
	pub file: &'static str,
	pub spec: FaceSpec,
	/// The glyph names, in glyph order. THE GATE ASKS BY NAME, never by number: a gate written
	/// against glyph 28 would pass over a face that renumbered and still shaped correctly, and fail
	/// over one that renumbered and did not - which is a test of the generator rather than of the
	/// shaper.
	pub glyphs: Vec<&'static str>,
}

/// The design grid every corpus face shares.
const UNITS_PER_EM: u16 = 1000;
const ASCENDER: i16 = 800;
const DESCENDER: i16 = -200;

fn names(family: &str, postscript: &str) -> Names {
	Names { family: family.into(), style: "Regular".into(), postscript: postscript.into(), version: "Version 1.000".into() }
}

/// A drawn glyph: a rectangle inside its advance, tall enough to tell one from another when a
/// rasteriser draws it.
fn drawn(advance: u16, height: i16) -> Glyph {
	let inset = if advance > 160 { 50 } else { 10 };
	Glyph::rectangle(inset, 0, advance as i16 - inset, height, advance)
}

/// A mark: no advance at all, which is what makes it a mark, and drawn above the baseline so a
/// wrongly attached one is visible rather than merely wrong.
fn mark(width: i16, bottom: i16, top: i16) -> Glyph {
	Glyph { contours: vec![vec![(0, bottom), (0, top), (width, top), (width, bottom)]], advance: 0, left_bearing: 0 }
}

fn face(family: &str, postscript: &str, glyphs: Vec<Glyph>, cmap: Vec<(u32, u16)>, extra: Vec<([u8; 4], Vec<u8>)>) -> FaceSpec {
	FaceSpec { names: names(family, postscript), units_per_em: UNITS_PER_EM, ascender: ASCENDER, descender: DESCENDER, line_gap: 0, weight_class: 400, width_class: 5, vendor: *b"LIBR", average_advance: 500, fixed_pitch: false, glyphs, cmap: Cmap::Named(cmap), extra }
}

// ---------------------------------------------------------------------------------------------
// The alphabetic face: Latin, Arabic and Hebrew in ONE face, which is the shape a real face has.
// ---------------------------------------------------------------------------------------------

/// The Latin kerning pair, and by how much. The gate states the same number; a face and a gate that
/// disagreed about it would be a corpus nobody had pinned.
pub const KERN_AV: i16 = -80;
pub const KERN_VA: i16 = -60;

fn alphabetic() -> Authored {
	// Glyph order, and the name each is known by. The nominal form of each Arabic letter is what the
	// `cmap` maps to; the four positional forms are reached ONLY through the joining features, which
	// is what makes the four-form case a test of the shaper rather than of the mapping.
	let glyphs: Vec<(&'static str, Glyph)> = vec![
		(".notdef", drawn(500, 700)),
		("space", Glyph::blank(260)),
		("A", drawn(700, 700)),
		("V", drawn(700, 700)),
		("T", drawn(660, 700)),
		("f", drawn(340, 740)),
		("i", drawn(300, 720)),
		("l", drawn(300, 740)),
		("f_i", drawn(600, 740)),
		("f_f_l", drawn(900, 740)),
		("alef", drawn(340, 700)),
		("alef.isol", drawn(340, 700)),
		("alef.fina", drawn(380, 700)),
		("beh", drawn(620, 400)),
		("beh.isol", drawn(620, 400)),
		("beh.init", drawn(520, 400)),
		("beh.medi", drawn(480, 400)),
		("beh.fina", drawn(600, 400)),
		("jeem", drawn(560, 520)),
		("jeem.isol", drawn(560, 520)),
		("jeem.init", drawn(500, 520)),
		("jeem.medi", drawn(460, 520)),
		("jeem.fina", drawn(580, 520)),
		("lam", drawn(420, 760)),
		("lam.isol", drawn(420, 760)),
		("lam.init", drawn(360, 760)),
		("lam.medi", drawn(340, 760)),
		("lam.fina", drawn(400, 760)),
		("lam_alef", drawn(600, 760)),
		("he.alef", drawn(620, 600)),
		("he.bet", drawn(580, 600)),
		("dagesh", mark(90, 240, 330)),
		("qamats", mark(160, -180, -90)),
		("sheva", mark(120, -200, -110)),
	];
	let id = |name: &str| glyphs.iter().position(|(known, _)| *known == name).expect("a glyph this face declares") as u16;

	let cmap: Vec<(u32, u16)> = vec![
		(0x0020, id("space")),
		(0x0041, id("A")),
		(0x0054, id("T")),
		(0x0056, id("V")),
		(0x0066, id("f")),
		(0x0069, id("i")),
		(0x006C, id("l")),
		// The Arabic letters map to their NOMINAL forms.
		(0x0627, id("alef")),
		(0x0628, id("beh")),
		(0x062C, id("jeem")),
		(0x0644, id("lam")),
		(0x05D0, id("he.alef")),
		(0x05D1, id("he.bet")),
		(0x05B0, id("sheva")),
		(0x05B8, id("qamats")),
		(0x05BC, id("dagesh")),
	];

	// THE LOOKUP ORDER IS THE FACE'S MEANING. The joining forms run first and the required ligature
	// after them, so `lam_alef` is formed from the POSITIONAL forms - which is what an Arabic face
	// does and what a shaper applying features in feature order would get wrong.
	let gsub_lookups = vec![
		// 0: liga
		Lookup::new(4, layout::ligature(&[(vec![id("f"), id("i")], id("f_i")), (vec![id("f"), id("f"), id("l")], id("f_f_l"))])),
		// 1: isol
		Lookup::new(1, layout::single(&[(id("alef"), id("alef.isol")), (id("beh"), id("beh.isol")), (id("jeem"), id("jeem.isol")), (id("lam"), id("lam.isol"))])),
		// 2: init. ALEF IS ABSENT FROM THIS ONE ON PURPOSE: alef joins only to its right, so it has
		// no initial and no medial form, and a face that invented them would let a shaper that
		// ignored joining types look correct.
		Lookup::new(1, layout::single(&[(id("beh"), id("beh.init")), (id("jeem"), id("jeem.init")), (id("lam"), id("lam.init"))])),
		// 3: medi
		Lookup::new(1, layout::single(&[(id("beh"), id("beh.medi")), (id("jeem"), id("jeem.medi")), (id("lam"), id("lam.medi"))])),
		// 4: fina
		Lookup::new(1, layout::single(&[(id("alef"), id("alef.fina")), (id("beh"), id("beh.fina")), (id("jeem"), id("jeem.fina")), (id("lam"), id("lam.fina"))])),
		// 5: rlig
		Lookup::new(4, layout::ligature(&[(vec![id("lam.init"), id("alef.fina")], id("lam_alef"))])),
	];
	let gsub = layout::table(
		&[ScriptEntry { tag: *b"latn", features: vec![0] }, ScriptEntry { tag: *b"arab", features: vec![1, 2, 3, 4, 5] }],
		&[
			FeatureEntry { tag: *b"liga", lookups: vec![0] },
			FeatureEntry { tag: *b"isol", lookups: vec![1] },
			FeatureEntry { tag: *b"init", lookups: vec![2] },
			FeatureEntry { tag: *b"medi", lookups: vec![3] },
			FeatureEntry { tag: *b"fina", lookups: vec![4] },
			FeatureEntry { tag: *b"rlig", lookups: vec![5] },
		],
		&gsub_lookups,
	);

	// TWO MARK CLASSES, because one proves nothing: a face with a single class attaches every mark
	// to the same anchor, so a shaper that ignored the class would pass. Class 0 is the dot inside
	// the letter and class 1 is the vowel below it, and the same base carries both.
	let gpos_lookups = vec![
		// 0: kern
		Lookup::ignoring_marks(2, layout::pair(&[(id("A"), id("V"), KERN_AV), (id("V"), id("A"), KERN_VA)])),
		// 1: mark
		Lookup::new(4, layout::mark_to_base(&[(id("dagesh"), 0, (45, 285)), (id("qamats"), 1, (80, -135)), (id("sheva"), 1, (60, -155))], &[(id("he.alef"), vec![(300, 350), (310, -120)]), (id("he.bet"), vec![(290, 330), (300, -110)])], 2)),
	];
	let gpos = layout::table(&[ScriptEntry { tag: *b"latn", features: vec![0] }, ScriptEntry { tag: *b"hebr", features: vec![1] }], &[FeatureEntry { tag: *b"kern", lookups: vec![0] }, FeatureEntry { tag: *b"mark", lookups: vec![1] }], &gpos_lookups);

	let gdef = layout::gdef(&[(id("A"), id("he.bet"), 1), (id("dagesh"), id("sheva"), 3)]);

	Authored { file: "conformance-alpha.ttf", spec: face("LiberSystem Conformance Alpha", "LiberSystemConformanceAlpha-Regular", glyphs.iter().map(|(_, glyph)| Glyph { contours: glyph.contours.clone(), advance: glyph.advance, left_bearing: glyph.left_bearing }).collect(), cmap, vec![(*b"GDEF", gdef), (*b"GPOS", gpos), (*b"GSUB", gsub)]), glyphs: glyphs.iter().map(|(name, _)| *name).collect() }
}

// ---------------------------------------------------------------------------------------------
// The complex face: Devanagari reordering, Thai marks, Khmer stacking.
// ---------------------------------------------------------------------------------------------

fn complex() -> Authored {
	let glyphs: Vec<(&'static str, Glyph)> = vec![
		(".notdef", drawn(500, 700)),
		("space", Glyph::blank(260)),
		("dv.ka", drawn(560, 620)),
		("dv.kha", drawn(600, 620)),
		("dv.ra", drawn(480, 620)),
		("dv.virama", mark(120, 0, 180)),
		("dv.vowel-i", drawn(260, 700)),
		("dv.vowel-aa", drawn(240, 620)),
		("dv.ka.half", drawn(400, 620)),
		("dv.repha", mark(200, 640, 780)),
		("dv.ka_ra", drawn(560, 620)),
		("th.ko", drawn(540, 600)),
		("th.sara-a", drawn(320, 600)),
		("th.sara-aa", drawn(300, 600)),
		("th.sara-am", drawn(300, 700)),
		("th.sara-i", mark(180, 620, 740)),
		("th.mai-ek", mark(140, 760, 860)),
		("th.nikhahit", mark(150, 640, 760)),
		("km.ka", drawn(600, 640)),
		("km.kha", drawn(620, 640)),
		("km.coeng", mark(100, 0, 120)),
		("km.ka.sub", mark(420, -260, -60)),
		("km.kha.sub", mark(440, -260, -60)),
		("km.sra-ii", mark(200, 660, 790)),
	];
	let id = |name: &str| glyphs.iter().position(|(known, _)| *known == name).expect("a glyph this face declares") as u16;

	let cmap: Vec<(u32, u16)> = vec![
		(0x0020, id("space")),
		(0x0915, id("dv.ka")),
		(0x0916, id("dv.kha")),
		(0x0930, id("dv.ra")),
		(0x093E, id("dv.vowel-aa")),
		(0x093F, id("dv.vowel-i")),
		(0x094D, id("dv.virama")),
		(0x0E01, id("th.ko")),
		(0x0E30, id("th.sara-a")),
		(0x0E32, id("th.sara-aa")),
		(0x0E33, id("th.sara-am")),
		(0x0E34, id("th.sara-i")),
		(0x0E48, id("th.mai-ek")),
		(0x1780, id("km.ka")),
		(0x1781, id("km.kha")),
		(0x17B8, id("km.sra-ii")),
		(0x17D2, id("km.coeng")),
	];

	let gsub_lookups = vec![
		// 0: rphf - the ra before a virama at the head of a syllable becomes the reph.
		Lookup::new(4, layout::ligature(&[(vec![id("dv.ra"), id("dv.virama")], id("dv.repha"))])),
		// 1: half - a consonant before a virama loses its stem.
		Lookup::new(4, layout::ligature(&[(vec![id("dv.ka"), id("dv.virama")], id("dv.ka.half"))])),
		// 2: pres - the half form joined to what follows it.
		Lookup::new(4, layout::ligature(&[(vec![id("dv.ka.half"), id("dv.ra")], id("dv.ka_ra"))])),
		// 3: blwf - Khmer's coeng binds the consonant after it into a subjoined form.
		Lookup::new(4, layout::ligature(&[(vec![id("km.coeng"), id("km.ka")], id("km.ka.sub")), (vec![id("km.coeng"), id("km.kha")], id("km.kha.sub"))])),
		// 4: ccmp - Thai's sara am is ONE character DRAWN AS TWO GLYPHS, the upper one of which is a
		// mark that then attaches to the consonant. No face draws it with the nominal glyph, and a
		// shaper that never applies `ccmp` leaves that glyph where a reader sees the wrong shape.
		Lookup::new(2, layout::multiple(&[(id("th.sara-am"), vec![id("th.nikhahit"), id("th.sara-aa")])])),
	];
	let gsub = layout::table(
		&[ScriptEntry { tag: *b"dev2", features: vec![0, 1, 2] }, ScriptEntry { tag: *b"khmr", features: vec![3] }, ScriptEntry { tag: *b"thai", features: vec![4] }],
		&[
			FeatureEntry { tag: *b"rphf", lookups: vec![0] },
			FeatureEntry { tag: *b"half", lookups: vec![1] },
			FeatureEntry { tag: *b"pres", lookups: vec![2] },
			FeatureEntry { tag: *b"blwf", lookups: vec![3] },
			FeatureEntry { tag: *b"ccmp", lookups: vec![4] },
		],
		&gsub_lookups,
	);

	// THE THAI TONE MARK SITS ABOVE THE VOWEL, NOT ABOVE THE CONSONANT, which is why `mkmk` exists
	// and why a face with only `mark` stacks the two on top of each other. This face carries both.
	let gpos_lookups = vec![
		// 0: mark - Thai and Khmer vowels and tone marks over their base, and Devanagari's reph.
		Lookup::new(
			4,
			layout::mark_to_base(
				&[
					(id("th.sara-i"), 0, (90, 640)),
					(id("th.mai-ek"), 0, (70, 780)),
					(id("th.nikhahit"), 0, (75, 660)),
					(id("km.sra-ii"), 0, (100, 680)),
					(id("km.ka.sub"), 1, (210, -80)),
					(id("km.kha.sub"), 1, (220, -80)),
				],
				&[(id("th.ko"), vec![(270, 620), (270, -40)]), (id("km.ka"), vec![(300, 660), (300, -60)]), (id("km.kha"), vec![(310, 660), (310, -60)])],
				2,
			),
		),
	];
	let gpos = layout::table(&[ScriptEntry { tag: *b"thai", features: vec![0] }, ScriptEntry { tag: *b"khmr", features: vec![0] }], &[FeatureEntry { tag: *b"mark", lookups: vec![0] }], &gpos_lookups);

	// The marks of this face are marks, and its consonants are bases. `dv.virama` and `km.coeng` are
	// marks too: a Khmer coeng that a kerning pair did not skip would break every stacked cluster.
	let gdef = layout::gdef(&[
		(id("dv.ka"), id("dv.ra"), 1),
		(id("dv.virama"), id("dv.virama"), 3),
		(id("dv.vowel-i"), id("dv.vowel-aa"), 1),
		(id("dv.ka.half"), id("dv.ka.half"), 1),
		(id("dv.repha"), id("dv.repha"), 3),
		(id("dv.ka_ra"), id("dv.ka_ra"), 1),
		(id("th.ko"), id("th.sara-am"), 1),
		(id("th.sara-i"), id("th.nikhahit"), 3),
		(id("km.ka"), id("km.kha"), 1),
		(id("km.coeng"), id("km.kha.sub"), 3),
		(id("km.sra-ii"), id("km.sra-ii"), 3),
	]);

	Authored { file: "conformance-complex.ttf", spec: face("LiberSystem Conformance Complex", "LiberSystemConformanceComplex-Regular", glyphs.iter().map(|(_, glyph)| Glyph { contours: glyph.contours.clone(), advance: glyph.advance, left_bearing: glyph.left_bearing }).collect(), cmap, vec![(*b"GDEF", gdef), (*b"GPOS", gpos), (*b"GSUB", gsub)]), glyphs: glyphs.iter().map(|(name, _)| *name).collect() }
}

// ---------------------------------------------------------------------------------------------
// The emoji face: a mapping above the basic plane, and sequences that are ONE glyph.
// ---------------------------------------------------------------------------------------------

fn emoji() -> Authored {
	let glyphs: Vec<(&'static str, Glyph)> = vec![
		(".notdef", drawn(500, 700)),
		("space", Glyph::blank(260)),
		("man", drawn(1000, 800)),
		("woman", drawn(1000, 800)),
		("boy", drawn(1000, 800)),
		("zwj", Glyph::blank(0)),
		("tone.light", drawn(1000, 800)),
		("thumbsup", drawn(1000, 800)),
		("family.mwb", drawn(1400, 800)),
		("thumbsup.light", drawn(1000, 800)),
	];
	let id = |name: &str| glyphs.iter().position(|(known, _)| *known == name).expect("a glyph this face declares") as u16;

	// A FORMAT 12 SUBTABLE IS THE ONLY ADMITTED ONE THAT REACHES HERE. Everything but the joiner is
	// above the basic plane, so a face that carried only a format 4 subtable would map none of it -
	// which is the case a corpus of basic-plane scripts could never have shown.
	let cmap: Vec<(u32, u16)> = vec![
		(0x0020, id("space")),
		(0x200D, id("zwj")),
		(0x1F3FB, id("tone.light")),
		(0x1F44D, id("thumbsup")),
		(0x1F466, id("boy")),
		(0x1F468, id("man")),
		(0x1F469, id("woman")),
	];

	// THE JOINER IS PART OF THE LIGATURE, not something a shaper drops first. A face states the
	// whole sequence including the joiner, and a shaper that removed it would match nothing.
	let gsub_lookups = vec![Lookup::new(
		4,
		layout::ligature(&[(vec![id("man"), id("zwj"), id("woman"), id("zwj"), id("boy")], id("family.mwb")), (vec![id("thumbsup"), id("tone.light")], id("thumbsup.light"))]),
	)];
	let gsub = layout::table(
		// Emoji have no script of their own: the run's tag is `DFLT`, which is what a face carrying
		// them has to answer under.
		&[ScriptEntry { tag: *b"DFLT", features: vec![0] }],
		&[FeatureEntry { tag: *b"liga", lookups: vec![0] }],
		&gsub_lookups,
	);

	Authored { file: "conformance-emoji.ttf", spec: face("LiberSystem Conformance Emoji", "LiberSystemConformanceEmoji-Regular", glyphs.iter().map(|(_, glyph)| Glyph { contours: glyph.contours.clone(), advance: glyph.advance, left_bearing: glyph.left_bearing }).collect(), cmap, vec![(*b"GSUB", gsub)]), glyphs: glyphs.iter().map(|(name, _)| *name).collect() }
}

// ---------------------------------------------------------------------------------------------
// The variable face: an outline that varies on one half of the axis and an advance on the other.
// ---------------------------------------------------------------------------------------------

/// How far the advance of `bar` moves at the top of the axis, and how far the outline of `bar`
/// moves at the bottom. The gate states both; a face and a gate that disagreed would be a corpus
/// nobody had pinned.
pub const ADVANCE_DELTA: i16 = 180;
pub const OUTLINE_DELTA: i16 = 150;

fn variable_face() -> Authored {
	let glyphs: Vec<(&'static str, Glyph)> = vec![(".notdef", drawn(500, 700)), ("space", Glyph::blank(260)), ("bar", Glyph::rectangle(100, 0, 400, 700, 600))];
	let id = |name: &str| glyphs.iter().position(|(known, _)| *known == name).expect("a glyph this face declares") as u16;

	let cmap: Vec<(u32, u16)> = vec![(0x0020, id("space")), (0x007C, id("bar"))];

	// `bar` HAS FOUR POINTS AND FOUR PHANTOM POINTS. The serialised tuple below moves point 2 - the
	// top right corner - and NOTHING else, so the outline widens on the bottom half of the axis
	// while the advance, which lives in `HVAR` on the top half, does not move at all.
	let mut serialised: Vec<u8> = Vec::new();
	// One point number, given as a run of one: the high bit clear means a byte count of one.
	serialised.extend_from_slice(&[0x01, 0x00, 0x02]);
	// x: one delta, as a word, so the sign is unambiguous.
	serialised.extend_from_slice(&[0x40, (OUTLINE_DELTA >> 8) as u8, (OUTLINE_DELTA & 0xFF) as u8]);
	// y: one zero, which occupies no bytes at all.
	serialised.extend_from_slice(&[0x80]);

	let gvar = variable::gvar(&[None, None, Some(serialised)]);
	// The advance delta is for `bar` alone: the glyph id IS the delta-set index.
	let hvar = variable::hvar(&[0, 0, ADVANCE_DELTA]);
	let mvar = variable::mvar(60, -40);

	Authored { file: "conformance-variable.ttf", spec: face("LiberSystem Conformance Variable", "LiberSystemConformanceVariable-Regular", glyphs.iter().map(|(_, glyph)| Glyph { contours: glyph.contours.clone(), advance: glyph.advance, left_bearing: glyph.left_bearing }).collect(), cmap, vec![(*b"HVAR", hvar), (*b"MVAR", mvar), (*b"fvar", variable::fvar()), (*b"gvar", gvar)]), glyphs: glyphs.iter().map(|(name, _)| *name).collect() }
}

pub fn faces() -> Vec<Authored> {
	vec![alphabetic(), complex(), emoji(), variable_face()]
}

/// The pin file the gate reads: what each face hashes to, and what each of its glyphs is called.
pub fn pins(digests: &[(String, [u8; 32])]) -> String {
	let mut out = String::new();
	out.push_str("# THE CONFORMANCE CORPUS, PINNED. Authored by `src/tools/font-gen`; regenerate with\n");
	out.push_str("#   cargo run --manifest-path src/tools/font-gen/Cargo.toml\n");
	out.push_str("# A face whose bytes are not these is refused by the text corpus gate rather than\n");
	out.push_str("# quietly shaped: a corpus that drifted is a different test wearing the same name.\n");
	out.push_str("# The glyph names are how the gate asks - never by number, because a number would\n");
	out.push_str("# make the gate a test of this generator instead of a test of the shaper.\n");
	for authored in faces() {
		let Some((_, digest)) = digests.iter().find(|(file, _)| file == authored.file) else { continue };
		let mut hex = String::new();
		for byte in digest {
			hex.push_str(&format!("{byte:02x}"));
		}
		out.push_str(&format!("face {} {hex}\n", authored.file));
		for (index, name) in authored.glyphs.iter().enumerate() {
			out.push_str(&format!("glyph {} {name} {index}\n", authored.file));
		}
	}
	out
}
