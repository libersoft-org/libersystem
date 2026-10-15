use super::*;

use font_contract::Fixed266;
use font_parse::Face;
use unicode_bidi::ParagraphDirection;
use unicode_tables::Script;

/// AN ALLOCATION LIMIT FOR ONE THREAD, which is how a fixture reaches the refusal a real Domain
/// reaches when it runs out: every allocation on the thread that asked for a limit and is larger
/// than it fails, and every other thread is untouched. Without it "allocation on this path is
/// fallible" is a claim nothing can watch fail.
mod limited {
	use std::alloc::{GlobalAlloc, Layout, System};
	use std::cell::Cell;

	std::thread_local! {
		static LIMIT: Cell<usize> = const { Cell::new(usize::MAX) };
	}

	pub struct Limited;

	// SAFETY: every call is forwarded to the system allocator unchanged, except an allocation the
	// thread's limit refuses, which answers null - the documented way an allocator reports failure.
	unsafe impl GlobalAlloc for Limited {
		unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
			if layout.size() > LIMIT.try_with(Cell::get).unwrap_or(usize::MAX) {
				return core::ptr::null_mut();
			}
			unsafe { System.alloc(layout) }
		}

		unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
			unsafe { System.dealloc(pointer, layout) }
		}

		unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
			if size > LIMIT.try_with(Cell::get).unwrap_or(usize::MAX) {
				return core::ptr::null_mut();
			}
			unsafe { System.realloc(pointer, layout, size) }
		}
	}

	#[global_allocator]
	static ALLOCATOR: Limited = Limited;

	/// Run `work` with every allocation on this thread larger than `bytes` failing.
	pub fn within<T>(bytes: usize, work: impl FnOnce() -> T) -> T {
		LIMIT.with(|limit| limit.set(bytes));
		let out = work();
		LIMIT.with(|limit| limit.set(usize::MAX));
		out
	}
}

/// Faces built here, with a real `cmap`: the shaping stage asks a face what a character maps to, and
/// a fixture that handed it glyphs would be testing its own arithmetic.
mod build {
	fn u16(out: &mut std::vec::Vec<u8>, value: u16) {
		out.extend_from_slice(&value.to_be_bytes());
	}

	fn u32(out: &mut std::vec::Vec<u8>, value: u32) {
		out.extend_from_slice(&value.to_be_bytes());
	}

	fn head() -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, 0);
		u32(&mut out, 0);
		u32(&mut out, 0);
		u32(&mut out, 0x5F0F_3CF5);
		u16(&mut out, 0);
		u16(&mut out, 1000);
		out.extend_from_slice(&[0u8; 16]);
		u16(&mut out, 0);
		u16(&mut out, 0);
		u16(&mut out, 1000);
		u16(&mut out, 1000);
		u16(&mut out, 0);
		u16(&mut out, 8);
		u16(&mut out, 2);
		u16(&mut out, 0);
		u16(&mut out, 0);
		out
	}

	fn hhea(metrics: u16) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, 0);
		u16(&mut out, 800);
		u16(&mut out, (-200i16) as u16);
		u16(&mut out, 0);
		out.extend_from_slice(&[0u8; 24]);
		u16(&mut out, metrics);
		out
	}

	fn maxp(glyphs: u16) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u32(&mut out, 0x0001_0000);
		u16(&mut out, glyphs);
		out.extend_from_slice(&[0u8; 26]);
		out
	}

	fn hmtx(advances: &[u16]) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		for advance in advances {
			u16(&mut out, *advance);
			u16(&mut out, 0);
		}
		out
	}

	fn cmap(entries: &[(char, u16)]) -> std::vec::Vec<u8> {
		let mut sorted = entries.to_vec();
		sorted.sort_by_key(|(character, _)| *character as u32);
		let mut out = std::vec::Vec::new();
		u16(&mut out, 0);
		u16(&mut out, 1);
		u16(&mut out, 3);
		u16(&mut out, 10);
		u32(&mut out, 12);
		u16(&mut out, 12);
		u16(&mut out, 0);
		u32(&mut out, (16 + 12 * sorted.len()) as u32);
		u32(&mut out, 0);
		u32(&mut out, sorted.len() as u32);
		for (character, glyph) in sorted {
			u32(&mut out, character as u32);
			u32(&mut out, character as u32);
			u32(&mut out, glyph as u32);
		}
		out
	}

	fn coverage(glyph: u16) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, 1);
		u16(&mut out, glyph);
		out
	}

	fn lookup(kind: u16, subtable: std::vec::Vec<u8>) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, kind);
		u16(&mut out, 0);
		u16(&mut out, 1);
		u16(&mut out, 8);
		out.extend_from_slice(&subtable);
		out
	}

	/// A single substitution: `from` becomes `to`.
	pub fn single(from: u16, to: u16) -> std::vec::Vec<u8> {
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 2);
		u16(&mut subtable, 8);
		u16(&mut subtable, 1);
		u16(&mut subtable, to);
		subtable.extend_from_slice(&coverage(from));
		lookup(1, subtable)
	}

	/// A kerning pair: `first` before `second` changes the first's advance by `kern`.
	pub fn kern(first: u16, second: u16, kern: i16) -> std::vec::Vec<u8> {
		let mut pair_set = std::vec::Vec::new();
		u16(&mut pair_set, 1);
		u16(&mut pair_set, second);
		u16(&mut pair_set, kern as u16);
		let coverage = coverage(first);
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 1);
		u16(&mut subtable, 12);
		u16(&mut subtable, 0x0004);
		u16(&mut subtable, 0x0000);
		u16(&mut subtable, 1);
		u16(&mut subtable, (12 + coverage.len()) as u16);
		subtable.extend_from_slice(&coverage);
		subtable.extend_from_slice(&pair_set);
		lookup(2, subtable)
	}

	/// A layout table under `DFLT` with one lookup per feature, the default language system
	/// selecting `default` and the named ones theirs.
	pub fn layout(features: &[([u8; 4], std::vec::Vec<u8>)], default: &[u16], languages: &[([u8; 4], &[u16])]) -> std::vec::Vec<u8> {
		let lang_sys = |indices: &[u16]| {
			let mut out = std::vec::Vec::new();
			u16(&mut out, 0);
			u16(&mut out, 0xFFFF);
			u16(&mut out, indices.len() as u16);
			for index in indices {
				u16(&mut out, *index);
			}
			out
		};
		let default_system = lang_sys(default);
		let named: std::vec::Vec<std::vec::Vec<u8>> = languages.iter().map(|(_, indices)| lang_sys(indices)).collect();
		let header = 4 + 6 * languages.len();
		let mut script = std::vec::Vec::new();
		u16(&mut script, header as u16);
		u16(&mut script, languages.len() as u16);
		let mut at = header + default_system.len();
		for ((tag, _), body) in languages.iter().zip(&named) {
			script.extend_from_slice(tag);
			u16(&mut script, at as u16);
			at += body.len();
		}
		script.extend_from_slice(&default_system);
		for body in &named {
			script.extend_from_slice(body);
		}
		let mut script_list = std::vec::Vec::new();
		u16(&mut script_list, 1);
		script_list.extend_from_slice(b"DFLT");
		u16(&mut script_list, 8);
		script_list.extend_from_slice(&script);

		let mut feature_list = std::vec::Vec::new();
		u16(&mut feature_list, features.len() as u16);
		for (index, (tag, _)) in features.iter().enumerate() {
			feature_list.extend_from_slice(tag);
			u16(&mut feature_list, (2 + 6 * features.len() + 6 * index) as u16);
		}
		for index in 0..features.len() {
			u16(&mut feature_list, 0);
			u16(&mut feature_list, 1);
			u16(&mut feature_list, index as u16);
		}

		let mut lookup_list = std::vec::Vec::new();
		u16(&mut lookup_list, features.len() as u16);
		let mut at = 2 + 2 * features.len();
		for (_, lookup) in features {
			u16(&mut lookup_list, at as u16);
			at += lookup.len();
		}
		for (_, lookup) in features {
			lookup_list.extend_from_slice(lookup);
		}

		let feature_at = 10 + script_list.len();
		let lookup_at = feature_at + feature_list.len();
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, 0);
		u16(&mut out, 10);
		u16(&mut out, feature_at as u16);
		u16(&mut out, lookup_at as u16);
		out.extend_from_slice(&script_list);
		out.extend_from_slice(&feature_list);
		out.extend_from_slice(&lookup_list);
		out
	}

	/// A face mapping the characters given, with one advance per glyph and the layout tables given.
	pub fn face(characters: &[(char, u16)], advances: &[u16], gsub: Option<std::vec::Vec<u8>>, gpos: Option<std::vec::Vec<u8>>) -> std::vec::Vec<u8> {
		let mut tables: std::vec::Vec<([u8; 4], std::vec::Vec<u8>)> = std::vec::Vec::new();
		if let Some(gpos) = gpos {
			tables.push((*b"GPOS", gpos));
		}
		if let Some(gsub) = gsub {
			tables.push((*b"GSUB", gsub));
		}
		tables.push((*b"cmap", cmap(characters)));
		tables.push((*b"head", head()));
		tables.push((*b"hhea", hhea(advances.len() as u16)));
		tables.push((*b"hmtx", hmtx(advances)));
		tables.push((*b"maxp", maxp(advances.len() as u16)));
		let mut out = std::vec::Vec::new();
		u32(&mut out, 0x0001_0000);
		u16(&mut out, tables.len() as u16);
		u16(&mut out, 0);
		u16(&mut out, 0);
		u16(&mut out, 0);
		let mut offset = 12 + tables.len() * 16;
		let mut body = std::vec::Vec::new();
		for (tag, bytes) in &tables {
			out.extend_from_slice(tag);
			u32(&mut out, 0);
			u32(&mut out, offset as u32);
			u32(&mut out, bytes.len() as u32);
			body.extend_from_slice(bytes);
			while body.len() % 4 != 0 {
				body.push(0);
			}
			offset = 12 + tables.len() * 16 + body.len();
		}
		out.extend_from_slice(&body);
		out
	}
}

/// The glyphs of the main fixture face.
mod glyph {
	pub const A: u16 = 1;
	pub const T: u16 = 2;
	pub const HYPHEN: u16 = 3;
	pub const V: u16 = 4;
	pub const ALEF: u16 = 5;
	pub const BET: u16 = 6;
	pub const LESS: u16 = 7;
	pub const GREATER: u16 = 8;
	pub const SPACE: u16 = 9;
	pub const E: u16 = 10;
	pub const ACUTE: u16 = 11;
	pub const I: u16 = 12;
	pub const I_TURKISH: u16 = 13;
	pub const PAREN: u16 = 14;
	pub const PAREN_MIRRORED: u16 = 15;
	pub const SMALL_A: u16 = 16;
	pub const X: u16 = 1;
}

/// THE MAIN FIXTURE FACE: Latin and Hebrew, `<` and `>` both and a `(` with no `)`, `e` and the
/// combining acute but no precomposed `é`, a Turkish local form of `i` under `locl`, a mirrored form
/// of `(` under `rtlm`, and a kerning pair between the hyphen and `T`.
fn main_face() -> std::vec::Vec<u8> {
	use glyph::*;
	let characters = [
		('A', A),
		('T', T),
		('-', HYPHEN),
		('V', V),
		('\u{05D0}', ALEF),
		('\u{05D1}', BET),
		('<', LESS),
		('>', GREATER),
		(' ', SPACE),
		('e', E),
		('\u{0301}', ACUTE),
		('i', I),
		('(', PAREN),
		('a', SMALL_A),
	];
	let mut advances = std::vec![500u16; 17];
	advances[HYPHEN as usize] = 300;
	advances[SPACE as usize] = 250;
	advances[ACUTE as usize] = 0;
	let gsub = build::layout(&[(*b"locl", build::single(I, I_TURKISH)), (*b"rtlm", build::single(PAREN, PAREN_MIRRORED))], &[1], &[(*b"TRK ", &[0, 1])]);
	let gpos = build::layout(&[(*b"kern", build::kern(HYPHEN, T, -100))], &[0], &[]);
	build::face(&characters, &advances, Some(gsub), Some(gpos))
}

/// A SECOND FACE, covering only `x`, for the cluster the first one does not.
fn other_face() -> std::vec::Vec<u8> {
	build::face(&[('x', glyph::X)], &[500, 700], None, None)
}

/// Faces by id, answering coverage from their own `cmap` - the question a real caller asks.
struct Installed<'a>(std::vec::Vec<(u16, Face<'a>)>);

impl Coverage for Installed<'_> {
	fn covers(&self, face: u16, spelling: &[char]) -> bool {
		let Some((_, face)) = self.0.iter().find(|(id, _)| *id == face) else { return false };
		!spelling.is_empty() && spelling.iter().all(|character| matches!(face.glyph_for(*character), Ok(Some(_))))
	}
}

impl Faces for Installed<'_> {
	fn face(&self, face: u16) -> Option<&Face<'_>> {
		self.0.iter().find(|(id, _)| *id == face).map(|(_, face)| face)
	}
}

/// The two fixture faces as ids 1 and 2, and a policy that tries them in that order.
fn installed<'a>(main: &'a [u8], other: &'a [u8]) -> (Installed<'a>, Policy) {
	let faces = Installed(std::vec![(1, Face::open(main, 0).expect("the main fixture face")), (2, Face::open(other, 0).expect("the second fixture face"))]);
	let policy = Policy::new(&[FaceEntry { face: 1, script: None, language: None, rank: 0 }, FaceEntry { face: 2, script: None, language: None, rank: 1 }], None);
	(faces, policy)
}

/// The size every fixture measures at: ten pixels to the em, so a 500-unit advance is five pixels.
const SIZE: i16 = 10;

/// A paragraph through stages 1 to 6.
fn through_shaping<'a>(source: Source<'a>, direction: ParagraphDirection, faces: &Installed<'_>, policy: &Policy) -> Result<Shaped<'a>, Error> {
	let levelled = stages::resolve_levels(stages::itemise(source), direction);
	let faced = stages::choose_faces(levelled, policy, faces)?;
	stages::shape(faced, faces, Fixed266::from_pixels(SIZE))
}

fn glyphs_of(run: &ShapedRun) -> std::vec::Vec<u16> {
	run.buffer.infos.iter().map(|info| info.glyph).collect()
}

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
}

#[test]
// THE CANONICAL VIEW IS BOTH SPELLINGS OF ONE CLUSTER, and neither is the buffer.
fn a_cluster_is_seen_in_both_of_its_spellings() {
	// é, composed.
	let view = CanonicalView::of(&['\u{00E9}']).expect("a short cluster");
	assert_eq!(view.composed(), &['\u{00E9}']);
	assert_eq!(view.decomposed(), &['e', '\u{0301}']);
	// é, decomposed: the SAME view.
	let other = CanonicalView::of(&['e', '\u{0301}']).expect("a short cluster");
	assert_eq!(view, other, "two spellings of one cluster have one canonical view");
	// TWO MARKS IN EITHER ORDER ARE ONE STRING, which the canonical ordering is what makes true: a
	// view that did not sort them would call two identical clusters different, and fallback would
	// then differ too.
	let first = CanonicalView::of(&['a', '\u{0301}', '\u{0327}']).expect("a short cluster");
	let second = CanonicalView::of(&['a', '\u{0327}', '\u{0301}']).expect("a short cluster");
	assert_eq!(first, second);
	// And a cluster with nothing to compose or decompose is itself, both ways.
	let plain = CanonicalView::of(&['x']).expect("a short cluster");
	assert_eq!(plain.composed(), &['x']);
	assert_eq!(plain.decomposed(), &['x']);
	// A CLUSTER TOO LONG FOR THE VIEW HAS NONE, and is asked about as it stands rather than through
	// a spelling with some of its characters cut off.
	let long: std::vec::Vec<char> = core::iter::once('a').chain(core::iter::repeat_n('\u{0301}', canonical::MAX_VIEW)).collect();
	assert_eq!(CanonicalView::of(&long), None);
}

#[test]
// THE REGRESSION THE POLICY ASKS FOR: the composed and decomposed spellings of one string take the
// SAME fallback decision. Without the canonical view they take different faces - which is not a
// theoretical case, it is what a font set with precomposed Latin and a separate mark face does.
fn both_spellings_take_the_same_fallback_decision() {
	let faces = TwoFaces { precomposed: std::vec!['\u{00E9}', 'a'], decomposed: std::vec!['e', '\u{0301}', 'a'] };
	// Face 0 is asked first and covers the composed spelling, so both inputs choose it.
	assert_eq!(canonical::face_for(&faces, [0, 1], &['\u{00E9}']), Some(0));
	assert_eq!(canonical::face_for(&faces, [0, 1], &['e', '\u{0301}']), Some(0), "the decomposed input reaches the precomposed face");
	// And with the precomposed face absent, both reach the mark face - again the same decision.
	let faces = TwoFaces { precomposed: std::vec!['a'], decomposed: std::vec!['e', '\u{0301}', 'a'] };
	assert_eq!(canonical::face_for(&faces, [0, 1], &['\u{00E9}']), Some(1), "the composed input reaches the decomposing face");
	assert_eq!(canonical::face_for(&faces, [0, 1], &['e', '\u{0301}']), Some(1));
	// A cluster no face covers is a refusal rather than a silent notdef.
	let faces = TwoFaces { precomposed: std::vec!['a'], decomposed: std::vec!['a'] };
	assert_eq!(canonical::face_for(&faces, [0, 1], &['\u{4E00}']), None);
}

#[test]
// AND FOR A NON-LATIN SCRIPT, because a Latin-only proof of this is the case that always works.
// Devanagari KA with a nukta has both spellings too, and a font set that carries one and not the
// other is ordinary.
fn the_same_holds_for_a_non_latin_script() {
	// U+0958 is KA WITH NUKTA, canonically KA plus the nukta - and a composition EXCLUSION, so the
	// canonical view's composed form is the decomposed one. That is the correct answer and the case
	// an implementation that recomposed everything would get wrong.
	let view = CanonicalView::of(&['\u{0958}']).expect("a short cluster");
	assert_eq!(view.decomposed(), &['\u{0915}', '\u{093C}']);
	assert_eq!(view.composed(), &['\u{0915}', '\u{093C}'], "an excluded composite is never recomposed");
	let faces = TwoFaces { precomposed: std::vec!['\u{0958}'], decomposed: std::vec!['\u{0915}', '\u{093C}'] };
	assert_eq!(canonical::face_for(&faces, [0, 1], &['\u{0958}']), Some(1), "the decomposing face covers what the view asks about");
	assert_eq!(canonical::face_for(&faces, [0, 1], &['\u{0915}', '\u{093C}']), Some(1));
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
	assert_eq!(items.items[0].script, Script::Latin);
	assert_eq!(items.items[1].script, Script::Hebrew);
	// Every item boundary is a cluster boundary.
	for item in &items.items {
		assert!(items.clusters.contains(&item.start), "an item starts where a cluster does");
		assert!(items.clusters.contains(&item.end), "and ends where one does");
	}
}

#[test]
// THE SCRIPT RULE: a character that decides no script never starts a run of its own. At the start
// of a paragraph it joins the first script after it - a quotation mark opening a Hebrew sentence is
// part of the Hebrew run, not a run of punctuation shaped on its own - and a paragraph with nothing
// to decide by is one `Common` run.
fn a_leading_stretch_that_decides_no_script_joins_the_script_after_it() {
	let items = stages::itemise(Source::new("\u{00AB}\u{05D0}\u{05D1}\u{00BB} 12").expect("short enough"));
	assert_eq!(items.items.len(), 1, "the guillemets, the space and the digits are all in the Hebrew run");
	assert_eq!(items.items[0].script, Script::Hebrew);
	let items = stages::itemise(Source::new("12 ab").expect("short enough"));
	assert_eq!(items.items.len(), 1);
	assert_eq!(items.items[0].script, Script::Latin);
	let items = stages::itemise(Source::new("12 ?").expect("short enough"));
	assert_eq!(items.items[0].script, Script::Common);
}

#[test]
// THE LANGUAGE IS AN EXPLICIT INPUT, and a change of language is a change of run: a Turkish word in a
// German sentence is shaped with Turkish forms, and only it.
fn the_language_is_stated_by_the_caller_and_starts_a_run_where_it_changes() {
	let spans = [LanguageSpan { start: 3, language: *b"TRK " }];
	let source = Source::with_languages("ab cd", &spans).expect("a span on a character boundary");
	assert_eq!(source.language_at(0), stages::DEFAULT_LANGUAGE, "text before the first span is in no stated language");
	assert_eq!(source.language_at(3), *b"TRK ");
	let items = stages::itemise(source);
	assert_eq!(items.items.len(), 2, "one script, two languages, two runs");
	assert_eq!((items.items[0].start, items.items[0].end, items.items[0].language), (0, 3, stages::DEFAULT_LANGUAGE));
	assert_eq!((items.items[1].start, items.items[1].end, items.items[1].language), (3, 5, *b"TRK "));
	// A SPAN THAT IS NOT TRUE OF THE TEXT IS REFUSED rather than rounded: one inside a character, and
	// one that does not follow the span before it.
	let inside = [LanguageSpan { start: 1, language: *b"TRK " }];
	assert_eq!(Source::with_languages("\u{00E9}", &inside).err(), Some(Error::Language { at: 1 }));
	let backwards = [LanguageSpan { start: 3, language: *b"TRK " }, LanguageSpan { start: 1, language: *b"DEU " }];
	assert_eq!(Source::with_languages("ab cd", &backwards).err(), Some(Error::Language { at: 1 }));
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
}

#[test]
// THE ORDER IS STATED AND TOTAL, in three bands: the faces preferred for this script AND language,
// then for the script whatever the language, then the general ones, and the last resort last.
//
// FALLBACK THAT DEPENDS ON DIRECTORY ORDER IS A RENDERING DIFFERENCE BETWEEN TWO MACHINES, which is
// what this replaces: two installations with the same faces found in a different order would pick
// different fonts for the same string, and both would be "just using the system font".
fn the_fallback_order_is_stated_rather_than_found() {
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
	assert_eq!(policy.order_for(Script::Han, *b"JAN "), Ok(std::vec![10, 20, 30, 99]));
	// Chinese Han: the language-specific Japanese face is NOT preferred, which is the case that
	// makes a Han face choice a policy rather than a lookup - the same character is drawn
	// differently in the two languages.
	assert_eq!(policy.order_for(Script::Han, *b"ZHS "), Ok(std::vec![20, 30, 99]));
	// A script nothing is preferred for falls to the general faces.
	assert_eq!(policy.order_for(Script::Latin, *b"dflt"), Ok(std::vec![30, 99]));
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
	assert_eq!(ranked.order_for(Script::Latin, *b"dflt"), Ok(std::vec![6, 7, 5]));
}

#[test]
// THE PIPELINE'S FALLBACK STAGE IS THE POLICY, PER RUN. Two faces cover the same characters and the
// coverage provider knows nothing about order; which face each cluster is drawn with is decided by
// the script and the language of the run it is in - so one paragraph draws its Hebrew with the
// Hebrew face, its Latin with the general one, and its Japanese and Chinese ideographs differently.
fn the_fallback_stage_takes_the_policys_order_for_each_runs_script_and_language() {
	let policy = Policy::new(
		&[
			FaceEntry { face: 1, script: None, language: None, rank: 0 },
			FaceEntry { face: 2, script: Some(Script::Hebrew), language: None, rank: 0 },
			FaceEntry { face: 3, script: Some(Script::Han), language: Some(*b"JAN "), rank: 0 },
		],
		None,
	);
	let everything = std::vec!['a', ' ', '\u{05D0}', '\u{4E00}'];
	let coverage = Covering(std::vec![(1, everything.clone()), (2, everything.clone()), (3, everything)]);
	let faced = |text: &str, spans: &[LanguageSpan]| {
		let source = Source::with_languages(text, spans).expect("a valid source");
		stages::choose_faces(stages::resolve_levels(stages::itemise(source), ParagraphDirection::LeftToRight), &policy, &coverage).expect("every cluster covered").faces
	};
	assert_eq!(faced("a \u{05D0}", &[]), std::vec![1, 1, 2], "the Hebrew cluster takes the face preferred for Hebrew");
	let japanese = [LanguageSpan { start: 0, language: *b"JAN " }];
	assert_eq!(faced("\u{4E00}", &japanese), std::vec![3], "Japanese takes the Japanese face");
	assert_eq!(faced("\u{4E00}", &[]), std::vec![1], "and a run in no stated language does not");
}

#[test]
// FALLBACK OPERATES ON WHOLE CLUSTERS. Splitting a combining sequence across two faces is the defect
// this item exists to prevent: an `e` from one font and its acute from another do not line up.
fn a_cluster_is_never_split_between_two_faces() {
	// Face 1 has the base letter and NOT the mark; face 2 has both.
	let coverage = Covering(std::vec![(1, std::vec!['e']), (2, std::vec!['e', '\u{0301}', '\u{00E9}'])]);
	// The cluster goes to face 2 whole, rather than the letter to face 1 and the mark to face 2.
	assert_eq!(fallback::face_for_cluster(&[1, 2], None, &coverage, &['e', '\u{0301}']), Some(2));
	// A cluster that IS covered by the earlier face still takes it.
	assert_eq!(fallback::face_for_cluster(&[1, 2], None, &coverage, &['e']), Some(1));
	// And nothing covers what nothing has.
	assert_eq!(fallback::face_for_cluster(&[1, 2], None, &coverage, &['\u{4E00}']), None);
}

#[test]
// A RUN ENDS WHERE THE FACE CHANGES, OR THE SCRIPT, OR THE LANGUAGE, OR THE LEVEL - which is what
// makes every shaped run face-, script-, language- and direction-homogeneous, the shared contract's
// own requirement. And each run is shaped with ITS language: the Turkish `i` takes the face's
// Turkish form and the one beside it in no stated language does not.
fn the_shaped_runs_are_homogeneous_and_shaped_with_their_own_language() {
	use glyph::*;
	let (main, other) = (main_face(), other_face());
	let (faces, policy) = installed(&main, &other);
	// A face change: `x` is only in the second face.
	let shaped = through_shaping(Source::new("AxA").expect("short enough"), ParagraphDirection::LeftToRight, &faces, &policy).expect("shaped");
	let runs: std::vec::Vec<(usize, usize, u16)> = shaped.runs.iter().map(|run| (run.start, run.end, run.face)).collect();
	assert_eq!(runs, std::vec![(0, 1, 1), (1, 2, 2), (2, 3, 1)]);
	// A script and a level change: Latin, then Hebrew at level 1, then Latin.
	let shaped = shaped_paragraph("T\u{05D0}\u{05D1}V", &faces, &policy);
	let runs: std::vec::Vec<(usize, usize, Script, u8)> = shaped.runs.iter().map(|run| (run.start, run.end, run.script, run.level)).collect();
	assert_eq!(runs, std::vec![(0, 1, Script::Latin, 0), (1, 5, Script::Hebrew, 1), (5, 6, Script::Latin, 0)]);
	assert_eq!(shaped.runs[1].direction, font_contract::Direction::RightToLeft);
	// A language change, and the language reaching the face's language system.
	let spans = [LanguageSpan { start: 1, language: *b"TRK " }];
	let shaped = through_shaping(Source::with_languages("ii", &spans).expect("a valid source"), ParagraphDirection::LeftToRight, &faces, &policy).expect("shaped");
	assert_eq!(shaped.runs.len(), 2);
	assert_eq!(glyphs_of(&shaped.runs[0]), std::vec![I], "no stated language: the default form");
	assert_eq!(glyphs_of(&shaped.runs[1]), std::vec![I_TURKISH], "Turkish: the face's local form, through locl");
	assert_eq!(shaped.runs[1].language, *b"TRK ");
}

fn shaped_paragraph<'a>(text: &'a str, faces: &Installed<'_>, policy: &Policy) -> Shaped<'a> {
	through_shaping(Source::new(text).expect("short enough"), ParagraphDirection::Auto, faces, policy).expect("shaped")
}

#[test]
// MIRRORING IS APPLIED, NOT ONLY ANSWERED. At an odd level a mirrored character is drawn as its
// mirror when the face has a glyph for it - `<` as `>` - and otherwise the face is asked for its own
// mirrored form through `rtlm`. At an even level nothing is mirrored at all.
fn a_mirrored_character_is_drawn_as_its_mirror_at_an_odd_level() {
	use glyph::*;
	let (main, other) = (main_face(), other_face());
	let (faces, policy) = installed(&main, &other);
	let shaped = shaped_paragraph("\u{05D0}<\u{05D1}", &faces, &policy);
	assert_eq!(shaped.runs.len(), 1);
	assert_eq!(glyphs_of(&shaped.runs[0]), std::vec![ALEF, GREATER, BET], "the face has `>`, so `<` is drawn from it");
	assert_eq!(shaped.runs[0].buffer.infos[1].cluster, 2, "and its cluster is still the `<` the caller wrote");
	// The face has no `)`: the `(` is drawn as itself and the face's own mirrored form applies.
	let shaped = shaped_paragraph("\u{05D0}(\u{05D1}", &faces, &policy);
	assert_eq!(glyphs_of(&shaped.runs[0]), std::vec![ALEF, PAREN_MIRRORED, BET]);
	// Left to right: nothing mirrors.
	let shaped = shaped_paragraph("A<(V", &faces, &policy);
	assert_eq!(glyphs_of(&shaped.runs[0]), std::vec![A, LESS, PAREN, V]);
}

#[test]
// THE CANONICAL SPELLING FALLBACK CHOSE IS THE ONE THE SHAPER IS HANDED. The face has `e` and the
// combining acute and no precomposed `é`: fallback chose it through the canonical view, and it is
// shaped with the decomposition - both glyphs reporting the cluster's start, because neither came
// from a byte the caller wrote.
fn a_cluster_is_shaped_in_the_canonical_spelling_its_face_covers() {
	use glyph::*;
	let (main, other) = (main_face(), other_face());
	let (faces, policy) = installed(&main, &other);
	let shaped = shaped_paragraph("A\u{00E9}", &faces, &policy);
	let infos: std::vec::Vec<(u16, u32)> = shaped.runs[0].buffer.infos.iter().map(|info| (info.glyph, info.cluster)).collect();
	assert_eq!(infos, std::vec![(A, 0), (E, 1), (ACUTE, 1)]);
	// The caller's own decomposed spelling is shaped as written, each glyph its own offset.
	let shaped = shaped_paragraph("Ae\u{0301}", &faces, &policy);
	let infos: std::vec::Vec<(u16, u32)> = shaped.runs[0].buffer.infos.iter().map(|info| (info.glyph, info.cluster)).collect();
	assert_eq!(infos, std::vec![(A, 0), (E, 1), (ACUTE, 2)]);
}

#[test]
// A RUN LONGER THAN THE SHAPER WILL TAKE IS SHAPED IN PIECES, split at the last space before the
// ceiling - where no joining and almost no font's context crosses - rather than refused as a whole
// paragraph. The paragraph's own input ceiling is sixteen times the run's, so without this every
// long paragraph in one face would be refused.
fn a_run_past_the_shapers_input_ceiling_is_split_at_its_last_space() {
	let (main, other) = (main_face(), other_face());
	let (faces, policy) = installed(&main, &other);
	let text: std::string::String = core::iter::repeat_n("aaaa ", 1000).collect();
	let shaped = shaped_paragraph(&text, &faces, &policy);
	let ceiling = opentype_profile::limits::RUN_INPUT as usize;
	assert!(shaped.runs.len() > 1);
	let mut next = 0usize;
	for run in &shaped.runs {
		assert_eq!(run.start, next, "the runs cover the paragraph without a gap");
		assert!(run.end - run.start <= ceiling, "a run of {} characters", run.end - run.start);
		next = run.end;
	}
	assert_eq!(next, text.len());
	assert_eq!(&text[shaped.runs[0].end - 1..shaped.runs[0].end], " ", "the first run ends after a space");
}

#[test]
// THE MISSING LAST LINE AND THE LOST OPPORTUNITY. A line is wrapped to the width before a mandatory
// break as well as before an allowed one - the last line of a paragraph included - and after a line
// ends, the opportunity that showed it was full is asked again from the new line's start.
fn every_line_is_wrapped_to_the_width_the_last_one_included() {
	let lines = lines_of("aaa bbb ccc", 1, 64 * 9 / 2);
	assert_eq!(lines, std::vec![(0, 4), (4, 8), (8, 11)]);
	// AND A LINE NEVER ENDS INSIDE A GRAPHEME CLUSTER. A space and the combining mark after it are
	// one cluster and two line-break classes, and UAX #14 allows a break between them; a line ending
	// there would split the cluster between two lines.
	let lines = lines_of("aa \u{0301}bb", 1, 64 * 2);
	assert_eq!(lines, std::vec![(0, 7)], "the only opportunity is inside a cluster, so the line is not broken there");
}

/// The lines a text breaks into at a width in 26.6, with every character one glyph of `advance`
/// pixels - a fixture of numbers rather than of a face.
fn lines_of(text: &str, advance: i16, width: i32) -> std::vec::Vec<(usize, usize)> {
	let levelled = stages::resolve_levels(stages::itemise(Source::new(text).expect("short enough")), ParagraphDirection::LeftToRight);
	let count = levelled.items.clusters.len() - 1;
	let mut buffer = font_shape::Buffer::new();
	let mut advances = std::vec::Vec::new();
	for (offset, _) in text.char_indices() {
		buffer.push(1, offset as u32).expect("room for a glyph");
		advances.push(Fixed266::from_pixels(advance));
	}
	let run = ShapedRun { start: 0, end: text.len(), face: 0, script: Script::Latin, language: stages::DEFAULT_LANGUAGE, level: 0, direction: font_contract::Direction::LeftToRight, buffer, advances };
	let faced = Faced { levelled, faces: std::vec![0; count], levels: std::vec![0; count], mirrored: std::vec![None; count] };
	let measured = stages::measure(Shaped { faced, size: Fixed266::from_pixels(SIZE), runs: std::vec![run] }).expect("measured");
	stages::break_lines(measured, Fixed266::from_raw(width)).expect("broken").lines.iter().map(|line| (line.start, line.end)).collect()
}

#[test]
// A BREAK CAN CHANGE WHAT A RUN WAS SHAPED AGAINST, so every run a break splits is shaped again,
// piece by piece. Here the face kerns the hyphen against a `T`: shaped whole, the hyphen is narrowed
// for the `T` after it - and once the line ends after the hyphen, that `T` is on the next line and
// the narrowing is a width the line does not have. The reshaped piece carries the hyphen's own
// advance. A run a line holds whole is left exactly as it was shaped.
fn every_run_a_break_splits_is_shaped_again_piece_by_piece() {
	use glyph::*;
	let (main, other) = (main_face(), other_face());
	let (faces, policy) = installed(&main, &other);
	let shaped = shaped_paragraph("A-T", &faces, &policy);
	assert_eq!(shaped.runs[0].buffer.positions[1].x_advance, 300 - 100, "shaped whole, the hyphen is kerned against the T");
	let measured = stages::measure(shaped).expect("measured");
	// Eight pixels: `A-` fits, `A-T` does not.
	let lines = stages::break_lines(measured, Fixed266::from_pixels(8)).expect("broken");
	assert_eq!(lines.lines.iter().map(|line| (line.start, line.end)).collect::<std::vec::Vec<_>>(), std::vec![(0, 2), (2, 3)]);
	let reshaped = stages::reshape(lines, &faces).expect("reshaped");
	assert_eq!(reshaped.runs.len(), 2);
	assert_eq!(glyphs_of(&reshaped.runs[0][0]), std::vec![A, HYPHEN]);
	assert_eq!(reshaped.runs[0][0].buffer.positions[1].x_advance, 300, "the line's own hyphen, kerned against nothing");
	assert_eq!(glyphs_of(&reshaped.runs[1][0]), std::vec![T]);
	assert_eq!((reshaped.runs[1][0].start, reshaped.runs[1][0].end), (2, 3));
	// A run held whole by its line is the run stage 6 shaped.
	let shaped = shaped_paragraph("A-T", &faces, &policy);
	let whole = shaped.runs[0].clone();
	let lines = stages::break_lines(stages::measure(shaped).expect("measured"), Fixed266::ZERO).expect("broken");
	let reshaped = stages::reshape(lines, &faces).expect("reshaped");
	assert_eq!(reshaped.runs, std::vec![std::vec![whole]]);
}

#[test]
// THE ONE MISTAKE THE TYPES EXIST TO PREVENT: bidi reordering is applied PER LINE, after the breaks
// are known. A paragraph reordered once and then wrapped is wrong wherever it wraps - and here there
// is no path to `Visual` that does not go through `Lines` and `Reshaped`.
fn the_visual_order_is_produced_per_line_and_only_after_wrapping() {
	let source = Source::new("a\u{05D0}\nb").expect("short enough");
	let items = stages::itemise(source);
	let levelled = stages::resolve_levels(items, ParagraphDirection::Auto);
	// The paragraph is left to right, with one right-to-left letter in it.
	assert_eq!(levelled.levels.paragraph_level, 0);
	let shaped = Shaped { faced: Faced { levelled, faces: std::vec![], levels: std::vec![], mirrored: std::vec![] }, size: Fixed266::from_pixels(SIZE), runs: std::vec![] };
	let measured = stages::measure(shaped).expect("no advances to overflow");
	let lines = stages::break_lines(measured, Fixed266::from_pixels(100)).expect("broken");
	// The newline is a MANDATORY break, so there are two lines whatever the width is.
	assert_eq!(lines.lines.len(), 2, "a mandatory break ends a line whatever the width");
	let reshaped = stages::reshape(lines, &Installed(std::vec![])).expect("nothing to reshape");
	let visual = stages::reorder(reshaped).expect("reordered");
	assert_eq!(visual.order.len(), 2, "one visual order PER LINE, not one for the paragraph");
	// The first line holds three characters - the newline that ended it is ON it, which is what makes
	// rule L1's "the whitespace at the end of a line" a thing that exists - and the second holds one.
	assert_eq!(visual.order[0].len(), 3);
	assert_eq!(visual.order[1].len(), 1);
}

/// A run of `count` glyphs, for the paragraph ceilings.
fn run_of(count: usize) -> ShapedRun {
	let mut buffer = font_shape::Buffer::new();
	buffer.reserve(count).expect("room for the run");
	for _ in 0..count {
		buffer.push(1, 0).expect("room for a glyph");
	}
	ShapedRun { start: 0, end: 1, face: 0, script: Script::Latin, language: stages::DEFAULT_LANGUAGE, level: 0, direction: font_contract::Direction::LeftToRight, buffer, advances: std::vec![Fixed266::ZERO; count] }
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
	let shaped = Shaped { faced: Faced { levelled, faces: std::vec![], levels: std::vec![], mirrored: std::vec![] }, size: Fixed266::from_pixels(SIZE), runs: std::vec![run_of(ceiling / 2), run_of(ceiling / 2 + 1)] };
	assert_eq!(stages::measure(shaped).err(), Some(crate::Error::Exceeded { limit: "paragraph output", ceiling: opentype_profile::limits::PARAGRAPH_OUTPUT, asked: ceiling as u64 + 1 }));
}

#[test]
// A CLUSTER NO FACE COVERS WOULD OTHERWISE TRY EVERY FACE IN THE CATALOGUE, once per cluster, and the
// coverage question is not free: it is asked in BOTH canonical spellings. The walk stops at the
// profile's ceiling - and THE LAST RESORT IS ALWAYS AMONG THE FACES ASKED, because the cluster that
// needed the seventeenth face is exactly the one the last resort exists for, and it is the LAST entry
// of the order a ceiling would otherwise cut.
fn the_fallback_walk_stops_at_the_frozen_number_of_faces() {
	let ceiling = opentype_profile::limits::FALLBACK_FACES as usize;
	// The covering face is placed one past the ceiling, behind faces that cover nothing.
	let preferred: std::vec::Vec<u16> = (0..=ceiling as u16).collect();
	let only = |covering: usize| Covering(preferred.iter().map(|face| (*face, if *face as usize == covering { std::vec!['a'] } else { std::vec![] })).collect());
	assert_eq!(fallback::face_for_cluster(&preferred, None, &only(ceiling), &['a']), None, "the face past the ceiling is not reached");
	// Move it to the last position the ceiling admits and it IS reached, which is what makes the
	// number a ceiling rather than an accident of the order.
	assert_eq!(fallback::face_for_cluster(&preferred, None, &only(ceiling - 1), &['a']), Some(ceiling as u16 - 1));

	// THE LAST RESORT, behind more preferred faces than the ceiling admits, is still asked - and it is
	// counted in the ceiling: the preferred faces get the rest of it.
	let many: std::vec::Vec<u16> = (0..ceiling as u16 + 4).collect();
	let last_resort = 999u16;
	let coverage = Covering(std::vec![(last_resort, std::vec!['a']), (ceiling as u16 - 2, std::vec!['b']), (ceiling as u16 - 1, std::vec!['c'])]);
	assert_eq!(fallback::face_for_cluster(&many, Some(last_resort), &coverage, &['a']), Some(last_resort), "an uncovered cluster reaches the last resort");
	assert_eq!(fallback::face_for_cluster(&many, Some(last_resort), &coverage, &['b']), Some(ceiling as u16 - 2), "the last preferred place the ceiling leaves is asked");
	assert_eq!(fallback::face_for_cluster(&many, Some(last_resort), &coverage, &['c']), None, "and the one the last resort took is not");
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
	let shaped = Shaped { faced: Faced { levelled, faces: std::vec![], levels: std::vec![], mirrored: std::vec![] }, size: Fixed266::from_pixels(SIZE), runs: std::vec![run_of(ceiling)] };
	assert!(stages::measure(shaped).is_ok(), "exactly the ceiling's worth of glyphs is measured");
}

#[test]
// STORAGE THAT IS NOT THERE IS A TYPED REFUSAL, NOT AN EXIT. A Domain that cannot give a paragraph
// what it needs gets `Allocation` back from the first stage that answers a result - and the
// process that asked is still running to read it.
fn storage_the_paragraph_cannot_have_is_refused_rather_than_fatal() {
	let (main, other) = (main_face(), other_face());
	let (faces, policy) = installed(&main, &other);
	let text: std::string::String = core::iter::repeat_n('a', 2000).collect();
	// Itemisation's cluster list is the first input-sized storage; it is carried as a refusal and
	// reported by fallback, the first stage that returns one.
	let refused = limited::within(1024, || {
		let levelled = stages::resolve_levels(stages::itemise(Source::new(&text).expect("short enough")), ParagraphDirection::LeftToRight);
		stages::choose_faces(levelled, &policy, &faces).err()
	});
	assert_eq!(refused, Some(Error::Allocation));
	// The shaping stage's own storage, once fallback has had what it needed.
	let levelled = stages::resolve_levels(stages::itemise(Source::new(&text).expect("short enough")), ParagraphDirection::LeftToRight);
	let faced = stages::choose_faces(levelled, &policy, &faces).expect("fallback, unlimited");
	let refused = limited::within(1024, || stages::shape(faced, &faces, Fixed266::from_pixels(SIZE)).err());
	assert_eq!(refused, Some(Error::Allocation));
}
