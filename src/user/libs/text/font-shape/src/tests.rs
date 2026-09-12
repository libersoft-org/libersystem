use super::*;

use font_parse::Face;

/// A font with layout tables, built here.
///
/// AUTHORED RATHER THAN IMPORTED, for the reason the parser's fixtures are: a real face is a
/// licensed third-party binary and a reviewed import, and what is being checked is the FORMAT. A
/// font written here can also be made to carry exactly one ligature and exactly one kerning pair,
/// which no real font does - and that is what makes an assertion about the result readable.
mod build {
	fn u16(out: &mut std::vec::Vec<u8>, value: u16) {
		out.extend_from_slice(&value.to_be_bytes());
	}

	fn u32(out: &mut std::vec::Vec<u8>, value: u32) {
		out.extend_from_slice(&value.to_be_bytes());
	}

	/// `head`, with the one field this crate's paths read.
	fn head() -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, 0);
		u32(&mut out, 0);
		u32(&mut out, 0);
		u32(&mut out, 0x5F0F_3CF5);
		u16(&mut out, 0);
		u16(&mut out, 1000); // units per em
		out.extend_from_slice(&[0u8; 16]);
		u16(&mut out, 0);
		u16(&mut out, 0);
		u16(&mut out, 1000);
		u16(&mut out, 1000);
		u16(&mut out, 0);
		u16(&mut out, 8);
		u16(&mut out, 2);
		u16(&mut out, 0); // short loca
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

	/// A `Coverage` format 1 over the glyphs given, which must be sorted.
	fn coverage(glyphs: &[u16]) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, glyphs.len() as u16);
		for glyph in glyphs {
			u16(&mut out, *glyph);
		}
		out
	}

	/// The script, feature and lookup lists every layout table begins with, assembled around one
	/// feature naming one lookup.
	fn layout(tag: &[u8; 4], lookup: std::vec::Vec<u8>) -> std::vec::Vec<u8> {
		// The three lists are laid out one after another, and every offset is from the table start.
		let mut lang_sys = std::vec::Vec::new();
		u16(&mut lang_sys, 0); // lookup order
		u16(&mut lang_sys, 0xFFFF); // no required feature
		u16(&mut lang_sys, 1); // one feature
		u16(&mut lang_sys, 0); // feature index 0

		let mut script = std::vec::Vec::new();
		u16(&mut script, 4); // the default language system follows this header
		u16(&mut script, 0); // no named language systems
		script.extend_from_slice(&lang_sys);

		let mut script_list = std::vec::Vec::new();
		u16(&mut script_list, 1);
		script_list.extend_from_slice(b"DFLT");
		u16(&mut script_list, 8); // the script table follows the one record
		script_list.extend_from_slice(&script);

		let mut feature = std::vec::Vec::new();
		u16(&mut feature, 0); // no parameters
		u16(&mut feature, 1); // one lookup
		u16(&mut feature, 0); // lookup index 0

		let mut feature_list = std::vec::Vec::new();
		u16(&mut feature_list, 1);
		feature_list.extend_from_slice(tag);
		u16(&mut feature_list, 8);
		feature_list.extend_from_slice(&feature);

		let mut lookup_list = std::vec::Vec::new();
		u16(&mut lookup_list, 1);
		u16(&mut lookup_list, 4); // the lookup follows the one offset
		lookup_list.extend_from_slice(&lookup);

		let header = 10usize;
		let script_at = header;
		let feature_at = script_at + script_list.len();
		let lookup_at = feature_at + feature_list.len();
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, 0);
		u16(&mut out, script_at as u16);
		u16(&mut out, feature_at as u16);
		u16(&mut out, lookup_at as u16);
		out.extend_from_slice(&script_list);
		out.extend_from_slice(&feature_list);
		out.extend_from_slice(&lookup_list);
		out
	}

	/// One lookup with one subtable.
	fn lookup(kind: u16, flags: u16, subtable: std::vec::Vec<u8>) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, kind);
		u16(&mut out, flags);
		u16(&mut out, 1);
		u16(&mut out, 8); // the subtable follows the header and the one offset
		out.extend_from_slice(&subtable);
		out
	}

	/// `GSUB` with one ligature: `first` + `second` becomes `result`.
	pub fn gsub_ligature(first: u16, second: u16, result: u16) -> std::vec::Vec<u8> {
		let mut ligature = std::vec::Vec::new();
		u16(&mut ligature, result);
		u16(&mut ligature, 2); // two components, the first of which is the covered glyph
		u16(&mut ligature, second);

		let mut set = std::vec::Vec::new();
		u16(&mut set, 1); // one ligature
		u16(&mut set, 4); // which follows the header and the one offset
		set.extend_from_slice(&ligature);

		let coverage = coverage(&[first]);
		// format, coverage offset, set count, one set offset = 8 bytes of header.
		let coverage_at = 8usize;
		let set_at = coverage_at + coverage.len();
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 1);
		u16(&mut subtable, coverage_at as u16);
		u16(&mut subtable, 1);
		u16(&mut subtable, set_at as u16);
		subtable.extend_from_slice(&coverage);
		subtable.extend_from_slice(&set);
		layout(b"liga", lookup(4, 0, subtable))
	}

	/// `GSUB` with one single substitution under the feature tag given.
	pub fn gsub_single(tag: &[u8; 4], from: u16, to: u16) -> std::vec::Vec<u8> {
		let coverage = coverage(&[from]);
		// format, coverage offset, glyph count, one glyph = 8 bytes of header.
		let coverage_at = 8usize;
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 2); // the list form, which says which glyph rather than by how much
		u16(&mut subtable, coverage_at as u16);
		u16(&mut subtable, 1);
		u16(&mut subtable, to);
		subtable.extend_from_slice(&coverage);
		layout(tag, lookup(1, 0, subtable))
	}

	/// `GPOS` with one kerning pair: `first` before `second` narrows the first's advance by `kern`.
	pub fn gpos_kern(first: u16, second: u16, kern: i16) -> std::vec::Vec<u8> {
		let mut pair_set = std::vec::Vec::new();
		u16(&mut pair_set, 1); // one pair
		u16(&mut pair_set, second);
		u16(&mut pair_set, kern as u16); // the first glyph's x advance adjustment

		let coverage = coverage(&[first]);
		// format, coverage, two value formats, set count, one set offset = 12 bytes.
		let coverage_at = 12usize;
		let set_at = coverage_at + coverage.len();
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 1);
		u16(&mut subtable, coverage_at as u16);
		u16(&mut subtable, 0x0004); // the first value carries an x advance
		u16(&mut subtable, 0x0000); // the second carries nothing
		u16(&mut subtable, 1);
		u16(&mut subtable, set_at as u16);
		subtable.extend_from_slice(&coverage);
		subtable.extend_from_slice(&pair_set);
		layout(b"kern", lookup(2, 0, subtable))
	}

	/// `GPOS` with one mark attachment: `mark` attaches to `base` at the anchors given.
	pub fn gpos_mark(base: u16, mark: u16, base_anchor: (i16, i16), mark_anchor: (i16, i16)) -> std::vec::Vec<u8> {
		let anchor = |(x, y): (i16, i16)| {
			let mut out = std::vec::Vec::new();
			u16(&mut out, 1);
			u16(&mut out, x as u16);
			u16(&mut out, y as u16);
			out
		};
		let mark_coverage = coverage(&[mark]);
		let base_coverage = coverage(&[base]);
		// A mark array: one record of (class, anchor offset), then the anchor itself.
		let mut mark_array = std::vec::Vec::new();
		u16(&mut mark_array, 1); // one mark
		u16(&mut mark_array, 0); // class 0
		u16(&mut mark_array, 6); // the anchor follows the header and the record
		mark_array.extend_from_slice(&anchor(mark_anchor));
		// A base array: one record of one anchor offset per class, then the anchor.
		let mut base_array = std::vec::Vec::new();
		u16(&mut base_array, 1); // one base
		u16(&mut base_array, 4); // the anchor follows
		base_array.extend_from_slice(&anchor(base_anchor));

		// format, mark coverage, base coverage, class count, mark array, base array = 12 bytes.
		let mark_coverage_at = 12usize;
		let base_coverage_at = mark_coverage_at + mark_coverage.len();
		let mark_array_at = base_coverage_at + base_coverage.len();
		let base_array_at = mark_array_at + mark_array.len();
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 1);
		u16(&mut subtable, mark_coverage_at as u16);
		u16(&mut subtable, base_coverage_at as u16);
		u16(&mut subtable, 1); // one mark class
		u16(&mut subtable, mark_array_at as u16);
		u16(&mut subtable, base_array_at as u16);
		subtable.extend_from_slice(&mark_coverage);
		subtable.extend_from_slice(&base_coverage);
		subtable.extend_from_slice(&mark_array);
		subtable.extend_from_slice(&base_array);
		layout(b"mark", lookup(4, 0, subtable))
	}

	/// `GDEF` saying which glyphs are marks.
	pub fn gdef(marks: &[u16]) -> std::vec::Vec<u8> {
		let mut classes = std::vec::Vec::new();
		u16(&mut classes, 2); // range format
		u16(&mut classes, marks.len() as u16);
		for mark in marks {
			u16(&mut classes, *mark);
			u16(&mut classes, *mark);
			u16(&mut classes, 3); // class 3 is a mark
		}
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, 0);
		u16(&mut out, 12); // the class definition follows the header
		u16(&mut out, 0); // no attachment list
		u16(&mut out, 0); // no ligature caret list
		u16(&mut out, 0); // no mark attachment class definition
		out.extend_from_slice(&classes);
		out
	}

	pub fn font(tables: &[([u8; 4], std::vec::Vec<u8>)]) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u32(&mut out, 0x0001_0000);
		u16(&mut out, tables.len() as u16);
		u16(&mut out, 0);
		u16(&mut out, 0);
		u16(&mut out, 0);
		let mut offset = 12 + tables.len() * 16;
		let mut body = std::vec::Vec::new();
		for (tag, bytes) in tables {
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

	/// Five glyphs: notdef, `f`, `i`, the `fi` ligature, and a mark.
	pub fn sample(gsub: Option<std::vec::Vec<u8>>, gpos: Option<std::vec::Vec<u8>>, gdef: Option<std::vec::Vec<u8>>) -> std::vec::Vec<u8> {
		let mut tables: std::vec::Vec<([u8; 4], std::vec::Vec<u8>)> = std::vec::Vec::new();
		if let Some(gdef) = gdef {
			tables.push((*b"GDEF", gdef));
		}
		if let Some(gpos) = gpos {
			tables.push((*b"GPOS", gpos));
		}
		if let Some(gsub) = gsub {
			tables.push((*b"GSUB", gsub));
		}
		tables.push((*b"head", head()));
		tables.push((*b"hhea", hhea(5)));
		tables.push((*b"hmtx", hmtx(&[500, 300, 200, 450, 0])));
		tables.push((*b"maxp", maxp(5)));
		font(&tables)
	}
}

/// A buffer of the glyphs given, each its own cluster.
fn glyphs(glyphs: &[u16]) -> Buffer {
	let mut buffer = Buffer::new();
	for (index, glyph) in glyphs.iter().enumerate() {
		buffer.push(*glyph, index as u32);
	}
	buffer
}

#[test]
// A LIGATURE IS ONE GLYPH FOR TWO CHARACTERS, and the cluster is what says so: the result keeps the
// first component's cluster, which is what a caret between `f` and `i` is later built from.
fn a_ligature_substitutes_two_glyphs_for_one_and_keeps_the_cluster() {
	let bytes = build::sample(Some(build::gsub_ligature(1, 2, 3)), None, None);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut buffer = glyphs(&[1, 2]);
	shape(&face, &mut buffer, *b"DFLT", *b"dflt", &[*b"liga"]).expect("shaping");
	assert_eq!(buffer.len(), 1);
	assert_eq!(buffer.infos[0].glyph, 3);
	assert_eq!(buffer.infos[0].cluster, 0);
	assert_eq!(buffer.positions[0].x_advance, 450, "the ligature's own advance, not the sum of its parts");
}

#[test]
// A FEATURE THAT WAS NOT ASKED FOR DOES NOT RUN. `liga` is on by default in most stacks and that is
// the CALLER's policy; a shaper that applied every feature it found would give a caller no way to
// turn one off.
fn a_feature_the_caller_did_not_ask_for_is_not_applied() {
	let bytes = build::sample(Some(build::gsub_ligature(1, 2, 3)), None, None);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut buffer = glyphs(&[1, 2]);
	shape(&face, &mut buffer, *b"DFLT", *b"dflt", &[]).expect("shaping");
	assert_eq!(buffer.len(), 2, "no ligature was asked for, so none was formed");
	assert_eq!(buffer.infos[0].glyph, 1);
	assert_eq!(buffer.positions[0].x_advance, 300);
	assert_eq!(buffer.positions[1].x_advance, 200);
}

#[test]
// KERNING ADJUSTS AN ADVANCE RATHER THAN SETTING ONE, which is why the advances are read from `hmtx`
// before any positioning runs: a shaper that positioned first would kern around zero.
fn a_kerning_pair_adjusts_the_advance_it_is_applied_to() {
	let bytes = build::sample(None, Some(build::gpos_kern(1, 2, -50)), None);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut buffer = glyphs(&[1, 2]);
	shape(&face, &mut buffer, *b"DFLT", *b"dflt", &[*b"kern"]).expect("shaping");
	assert_eq!(buffer.positions[0].x_advance, 250, "300 from hmtx, less the 50 the pair asks for");
	assert_eq!(buffer.positions[1].x_advance, 200, "the second glyph is untouched");
	// And a pair the font does not carry leaves both alone.
	let mut buffer = glyphs(&[2, 1]);
	shape(&face, &mut buffer, *b"DFLT", *b"dflt", &[*b"kern"]).expect("shaping");
	assert_eq!(buffer.positions[0].x_advance, 200);
	assert_eq!(buffer.positions[1].x_advance, 300);
}

#[test]
// THE RULE THAT MAKES A SCRIPT RENDER AT ALL: a mark attaches to its base at the anchors the font
// gives, rather than being placed at the pen. An accent at the pen is Arabic and Devanagari rendered
// as a row of disconnected pieces.
fn a_mark_attaches_to_its_base_at_the_anchors_the_font_gives() {
	let bytes = build::sample(None, Some(build::gpos_mark(1, 4, (150, 700), (20, 0))), Some(build::gdef(&[4])));
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut buffer = glyphs(&[1, 4]);
	shape(&face, &mut buffer, *b"DFLT", *b"dflt", &[*b"mark"]).expect("shaping");
	assert_eq!(buffer.infos[1].class, 3, "GDEF says glyph 4 is a mark");
	// The mark is placed so its anchor meets the base's, less the advance between them.
	assert_eq!(buffer.positions[1].x_offset, 150 - 20 - 300);
	assert_eq!(buffer.positions[1].y_offset, 700);
	assert_eq!(buffer.infos[1].attached_to, Some(0));
	assert_eq!(buffer.positions[1].x_advance, 0, "a mark takes no advance of its own");
}

#[test]
// A FONT WITH NO LAYOUT TABLES IS NOT AN ERROR. Most fonts in the world have neither, and a shaper
// that refused them would render nothing at all.
fn a_font_with_no_layout_tables_shapes_to_its_own_advances() {
	let bytes = build::sample(None, None, None);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut buffer = glyphs(&[1, 2, 3]);
	shape(&face, &mut buffer, *b"DFLT", *b"dflt", &[*b"liga", *b"kern"]).expect("shaping");
	assert_eq!(buffer.len(), 3);
	assert_eq!(buffer.positions.iter().map(|position| position.x_advance).collect::<std::vec::Vec<_>>(), std::vec![300, 200, 450]);
}

#[test]
// THE BUFFER'S OWN OPERATIONS, which every rule above is built out of: a decomposition shares its
// cluster, and a ligature takes the first one's.
fn the_buffer_keeps_clusters_through_substitution() {
	let mut buffer = glyphs(&[1, 2, 3]);
	buffer.decompose(1, &[7, 8, 9]);
	assert_eq!(buffer.infos.iter().map(|info| info.glyph).collect::<std::vec::Vec<_>>(), std::vec![1, 7, 8, 9, 3]);
	assert_eq!(buffer.infos.iter().map(|info| info.cluster).collect::<std::vec::Vec<_>>(), std::vec![0, 1, 1, 1, 2]);
	assert_eq!(buffer.positions.len(), buffer.infos.len(), "a position per glyph, always");
	buffer.ligate(1, 3, 12);
	assert_eq!(buffer.infos.iter().map(|info| info.glyph).collect::<std::vec::Vec<_>>(), std::vec![1, 12, 3]);
	assert_eq!(buffer.infos[1].cluster, 1);
	// A ligature past the end of the buffer changes nothing rather than reading past it.
	buffer.ligate(2, 9, 99);
	assert_eq!(buffer.len(), 3);
}

#[test]
// THE CURSIVE SHAPER, which is the case the item's own last sentence is about: Latin needs no shaper
// at all, so a stack that renders it beautifully has not exercised this at all.
//
// Every one of these is a word where the form of a letter is decided by what it can join to, and
// getting it wrong renders Arabic as a row of disconnected shapes - which a reader who does not know
// the script cannot tell from correct.
fn a_cursive_run_takes_the_joining_form_each_letter_can_reach() {
	use crate::scripts::{Form, cursive_forms};
	// BEH is dual-joining, ALEF joins only to its right. `بب` is initial then final.
	assert_eq!(cursive_forms(&['\u{0628}', '\u{0628}']), std::vec![Form::Initial, Form::Final]);
	// Three of them: initial, medial, final.
	assert_eq!(cursive_forms(&['\u{0628}', '\u{0628}', '\u{0628}']), std::vec![Form::Initial, Form::Medial, Form::Final]);
	// A letter alone is isolated.
	assert_eq!(cursive_forms(&['\u{0628}']), std::vec![Form::Isolated]);
	// ALEF joins BACKWARD only, so what follows it starts a new join: `با` is initial, final; but
	// `اب` is isolated, isolated - the alef cannot reach forward.
	assert_eq!(cursive_forms(&['\u{0628}', '\u{0627}']), std::vec![Form::Initial, Form::Final]);
	assert_eq!(cursive_forms(&['\u{0627}', '\u{0628}']), std::vec![Form::Isolated, Form::Isolated]);
	// A SPACE BREAKS THE JOIN, which is what makes a word a word.
	assert_eq!(cursive_forms(&['\u{0628}', ' ', '\u{0628}']), std::vec![Form::Isolated, Form::Isolated, Form::Isolated]);
	// AND A MARK BETWEEN TWO LETTERS IS TRANSPARENT. This is the rule a shaper looking at its
	// immediate neighbour gets wrong, and in Arabic a vowel mark sits between letters constantly -
	// so that shaper breaks the join almost everywhere.
	assert_eq!(cursive_forms(&['\u{0628}', '\u{064E}', '\u{0628}']), std::vec![Form::Initial, Form::Isolated, Form::Final]);
}

#[test]
// THE MASK IS WHAT MAKES A POSITIONAL FEATURE POSITIONAL. `fina` applies to the last letter of a
// word and to no other; a shaper that could only turn it on globally would render a word made
// entirely of endings.
fn the_positional_features_apply_only_where_their_form_was_decided() {
	use crate::scripts::{FINA, INIT, MEDI, apply_cursive_masks};
	let characters = ['\u{0628}', '\u{0628}', '\u{0628}'];
	let mut buffer = glyphs(&[1, 1, 1]);
	apply_cursive_masks(&mut buffer, &characters);
	assert_eq!(buffer.infos[0].mask & INIT, INIT);
	assert_eq!(buffer.infos[0].mask & FINA, 0, "the first letter is not a final form");
	assert_eq!(buffer.infos[1].mask & MEDI, MEDI);
	assert_eq!(buffer.infos[2].mask & FINA, FINA);
	assert_eq!(buffer.infos[2].mask & INIT, 0);
	// AND A FONT'S `fina` LOOKUP, APPLIED UNDER THAT MASK, REACHES THE LAST LETTER ALONE. This is the
	// assertion the whole mask mechanism exists for: the same lookup, applied globally, would put
	// every letter of the word into its final form.
	let bytes = build::sample(Some(build::gsub_single(b"fina", 1, 4)), None, None);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut buffer = glyphs(&[1, 1, 1]);
	apply_cursive_masks(&mut buffer, &characters);
	shape_with_masks(&face, &mut buffer, *b"DFLT", *b"dflt", &crate::scripts::cursive_features()).expect("shaping");
	assert_eq!(buffer.infos.iter().map(|info| info.glyph).collect::<std::vec::Vec<_>>(), std::vec![1, 1, 4], "only the final form was substituted");
}

#[test]
// THE SCRIPT DECIDES THE SHAPER, and it is decided from the characters rather than from what the
// caller says: a run of Arabic in a document that declared itself English is still Arabic.
fn the_shaper_is_chosen_from_the_characters() {
	use crate::scripts::{Shaper, shaper_for};
	assert_eq!(shaper_for(&['a', 'b']), Shaper::Default);
	assert_eq!(shaper_for(&['\u{0628}']), Shaper::Cursive, "Arabic joins");
	assert_eq!(shaper_for(&['\u{05D0}']), Shaper::Default, "Hebrew is right to left and does not join");
	assert_eq!(shaper_for(&['\u{0915}']), Shaper::Indic, "Devanagari reorders");
	assert_eq!(shaper_for(&['a', '\u{0628}']), Shaper::Cursive, "the first character that decides");
}

#[test]
// A PRE-BASE VOWEL SIGN IS WRITTEN AFTER ITS CONSONANT AND DRAWN BEFORE IT. That is the writing
// system rather than a font feature, and a shaper that leaves the order alone renders Devanagari
// with every one of them on the wrong side of its letter.
fn an_indic_pre_base_vowel_is_moved_in_front_of_its_consonant() {
	use crate::scripts::{reorder_indic, syllable_starts};
	// क ि - KA followed by the pre-base vowel sign I.
	let characters = ['\u{0915}', '\u{093F}'];
	assert_eq!(syllable_starts(&characters), std::vec![0], "one syllable");
	let mut buffer = glyphs(&[10, 11]);
	reorder_indic(&characters, &mut buffer);
	assert_eq!(buffer.infos.iter().map(|info| info.glyph).collect::<std::vec::Vec<_>>(), std::vec![11, 10], "the vowel sign is drawn first");
	// A post-base sign is left where it is: ा is `Right`, not `Left`.
	let characters = ['\u{0915}', '\u{093E}'];
	let mut buffer = glyphs(&[10, 12]);
	reorder_indic(&characters, &mut buffer);
	assert_eq!(buffer.infos.iter().map(|info| info.glyph).collect::<std::vec::Vec<_>>(), std::vec![10, 12]);
	// TWO SYLLABLES REORDER INDEPENDENTLY, which is what the syllable boundaries are for: the vowel
	// of the second must not travel to the front of the first.
	let characters = ['\u{0915}', '\u{093F}', '\u{0916}', '\u{093F}'];
	assert_eq!(syllable_starts(&characters), std::vec![0, 2]);
	let mut buffer = glyphs(&[10, 11, 20, 21]);
	reorder_indic(&characters, &mut buffer);
	assert_eq!(buffer.infos.iter().map(|info| info.glyph).collect::<std::vec::Vec<_>>(), std::vec![11, 10, 21, 20]);
	// AND A CONJUNCT IS ONE SYLLABLE: consonant, virama, consonant.
	let characters = ['\u{0915}', '\u{094D}', '\u{0937}'];
	assert_eq!(syllable_starts(&characters), std::vec![0], "the virama binds the next consonant in");
}

#[test]
// A KOREAN SYLLABLE IS WRITTEN AS TWO OR THREE JAMO AND DRAWN AS ONE SQUARE. Almost every font
// carries the precomposed syllables and not the jamo arranged into them, so a shaper that left the
// sequence alone renders three letters where a reader expects one block.
fn hangul_jamo_compose_into_the_syllable_a_font_has_a_glyph_for() {
	use crate::scripts::compose_hangul;
	// ᄒ ᅡ ᆫ becomes 한, three jamo to one syllable.
	let characters = ['\u{1112}', '\u{1161}', '\u{11AB}'];
	let mut buffer = glyphs(&[1, 2, 3]);
	let composed = compose_hangul(&characters, &mut buffer);
	assert_eq!(composed, std::vec!['\u{D55C}']);
	assert_eq!(buffer.len(), 1, "the buffer follows the characters");
	assert_eq!(buffer.infos[0].cluster, 0, "and keeps the first jamo's cluster");
	// Two jamo with no trailing consonant compose too: ᄀ ᅡ becomes 가.
	let characters = ['\u{1100}', '\u{1161}'];
	let mut buffer = glyphs(&[1, 2]);
	assert_eq!(compose_hangul(&characters, &mut buffer), std::vec!['\u{AC00}']);
	// A LEAD WITH NOTHING TO COMPOSE WITH IS LEFT ALONE rather than dropped, which is what a font
	// carrying real jamo glyphs needs.
	let characters = ['\u{1100}', 'a'];
	let mut buffer = glyphs(&[1, 2]);
	assert_eq!(compose_hangul(&characters, &mut buffer), std::vec!['\u{1100}', 'a']);
	assert_eq!(buffer.len(), 2);
	// And an already-composed syllable passes through untouched.
	let characters = ['\u{D55C}'];
	let mut buffer = glyphs(&[9]);
	assert_eq!(compose_hangul(&characters, &mut buffer), std::vec!['\u{D55C}']);
}

#[test]
// MYANMAR REORDERS BY CATEGORY rather than by position: the medial `ra` and the pre-base vowel are
// both written after their consonant and drawn before it, and the medial goes first of all.
fn myanmar_moves_its_medial_and_pre_base_vowel_in_front() {
	use crate::scripts::reorder_myanmar;
	// က ြ ေ - KA, medial RA, vowel E: drawn as medial, vowel, consonant.
	let characters = ['\u{1000}', '\u{103C}', '\u{1031}'];
	let mut buffer = glyphs(&[10, 11, 12]);
	reorder_myanmar(&characters, &mut buffer);
	assert_eq!(buffer.infos.iter().map(|info| info.glyph).collect::<std::vec::Vec<_>>(), std::vec![11, 12, 10]);
	// A consonant with neither is left where it is.
	let characters = ['\u{1000}', '\u{102C}'];
	let mut buffer = glyphs(&[10, 13]);
	reorder_myanmar(&characters, &mut buffer);
	assert_eq!(buffer.infos.iter().map(|info| info.glyph).collect::<std::vec::Vec<_>>(), std::vec![10, 13]);
}

#[test]
// EVERY SHAPING CLASS THE PROFILE NAMES HAS A SHAPER, which is the claim this item's last sentence
// is about. Checked against the profile's own list rather than against a list written here.
fn every_shaping_class_the_profile_names_is_reachable() {
	use crate::scripts::{Shaper, shaper_for};
	use opentype_profile::ShapingClass;
	// One character per class, taken from a script the profile puts in it.
	let cases: [(ShapingClass, char, Shaper); 7] = [
		(ShapingClass::Default, 'a', Shaper::Default),
		(ShapingClass::Cursive, '\u{0628}', Shaper::Cursive),
		(ShapingClass::IndicReordering, '\u{0915}', Shaper::Indic),
		(ShapingClass::Khmer, '\u{1780}', Shaper::Khmer),
		(ShapingClass::Myanmar, '\u{1000}', Shaper::Myanmar),
		(ShapingClass::Hangul, '\u{1100}', Shaper::Hangul),
		(ShapingClass::Universal, '\u{0E01}', Shaper::Universal),
	];
	for (class, character, expected) in cases {
		assert_eq!(shaper_for(&[character]), expected, "{class:?} has no shaper");
	}
	// AND THE PROFILE'S OWN LIST IS COVERED: every class it names appears above, so a class added
	// there without a shaper here fails this rather than rendering a script with the wrong engine.
	let named: std::vec::Vec<ShapingClass> = opentype_profile::scripts::SCRIPTS.iter().map(|script| script.class).collect();
	for class in named {
		assert!(cases.iter().any(|(candidate, _, _)| *candidate == class), "{class:?} is in the profile and has no case here");
	}
}

#[test]
// THE ENTRY POINT THAT MAKES THE REST TRUE: characters in, shaped buffer out, with the shaper the
// script needs picked and run before any lookup. A caller that went straight to the feature list
// would be doing what "Latin alone" means.
fn shaping_a_run_picks_the_shaper_and_runs_its_pass_first() {
	let bytes = build::sample(Some(build::gsub_single(b"fina", 1, 4)), None, None);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	// An Arabic run: the masks are set from the joining forms, so the font's `fina` lookup reaches
	// the last letter alone - and none of that is expressible as a feature list.
	let characters = ['\u{0628}', '\u{0628}', '\u{0628}'];
	let mut buffer = glyphs(&[1, 1, 1]);
	let out = shape_run(&face, &mut buffer, &characters, *b"dflt").expect("shaping");
	assert_eq!(out.len(), 3, "no composition happens in Arabic");
	assert_eq!(buffer.infos.iter().map(|info| info.glyph).collect::<std::vec::Vec<_>>(), std::vec![1, 1, 4]);
	// A Hangul run: the characters THEMSELVES change, which is why the call answers them.
	let characters = ['\u{1112}', '\u{1161}', '\u{11AB}'];
	let mut buffer = glyphs(&[1, 2, 3]);
	let out = shape_run(&face, &mut buffer, &characters, *b"dflt").expect("shaping");
	assert_eq!(out, std::vec!['\u{D55C}']);
	assert_eq!(buffer.len(), 1);
	// A Devanagari run: the buffer is reordered before any lookup sees it.
	let characters = ['\u{0915}', '\u{093F}'];
	let mut buffer = glyphs(&[1, 2]);
	shape_run(&face, &mut buffer, &characters, *b"dflt").expect("shaping");
	assert_eq!(buffer.infos.iter().map(|info| info.glyph).collect::<std::vec::Vec<_>>(), std::vec![2, 1]);
	// And a Latin run is untouched by any of it, which is the case that proves nothing on its own.
	let characters = ['a', 'b'];
	let mut buffer = glyphs(&[1, 2]);
	shape_run(&face, &mut buffer, &characters, *b"dflt").expect("shaping");
	assert_eq!(buffer.infos.iter().map(|info| info.glyph).collect::<std::vec::Vec<_>>(), std::vec![1, 2]);
}

#[test]
// THE SCRIPT TAG IS NOT THE UNICODE SCRIPT NAME. OpenType has two tags for most Indic scripts, and a
// font written for one carries no features under the other - so a shaper that guessed would render
// an Indic font with none of its features applied and report nothing.
fn the_script_tag_is_the_one_the_font_indexes_its_features_by() {
	use crate::scripts::script_tag;
	assert_eq!(script_tag(&['a']), *b"latn");
	assert_eq!(script_tag(&['\u{0915}']), *b"dev2", "the version-2 tag every font made this century uses");
	assert_eq!(script_tag(&['\u{0628}']), *b"arab");
	assert_eq!(script_tag(&['\u{D55C}']), *b"hang");
	// A character whose script decides nothing does not pick the tag for the run.
	assert_eq!(script_tag(&[' ', '1', '\u{0628}']), *b"arab");
	assert_eq!(script_tag(&[' ']), *b"DFLT", "a run with nothing to decide by");
}

#[test]
// A RATIO AGAINST AN UNBOUNDED INPUT IS NOT A BOUND. The expansion rule caps the MULTIPLIER and not
// the product, so an arbitrarily large run still demands arbitrarily large work however small the
// ratio - which is why the absolute input ceiling exists under it, and why it is checked BEFORE the
// shaper's own pass rewrites the whole run.
fn a_run_past_the_frozen_input_ceiling_is_refused_before_any_work_is_done() {
	let bytes = build::sample(None, None, None);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let ceiling = opentype_profile::limits::RUN_INPUT as usize;
	let characters: std::vec::Vec<char> = core::iter::repeat_n('a', ceiling + 1).collect();
	let mut buffer = Buffer::new();
	assert_eq!(crate::shape::shape_run(&face, &mut buffer, &characters, *b"dflt").err(), Some(font_parse::Error::Unsupported(font_parse::Unsupported::Exceeded { limit: "run input", ceiling: opentype_profile::limits::RUN_INPUT, asked: ceiling as u64 + 1 })));
	// AND EXACTLY AT THE CEILING IT IS NOT REFUSED, which is what makes the number a ceiling.
	let characters: std::vec::Vec<char> = core::iter::repeat_n('a', ceiling).collect();
	let mut buffer = Buffer::new();
	for (index, _) in characters.iter().enumerate() {
		buffer.push(1, index as u32);
	}
	assert!(crate::shape::shape_run(&face, &mut buffer, &characters, *b"dflt").is_ok());
}

#[test]
// A LOOKUP SILENTLY LEFT OUT IS A RUN SHAPED AS THOUGH THE FONT HAD NOT ASKED, which is a document
// rendered wrong with nothing to say so. The feature ceiling refuses instead.
fn more_features_than_the_profile_freezes_are_refused_rather_than_dropped() {
	let bytes = build::sample(None, None, None);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let ceiling = opentype_profile::limits::FEATURES as usize;
	let features: std::vec::Vec<([u8; 4], u32)> = (0..=ceiling).map(|index| ([b'a', b'a', (index / 26) as u8 + b'a', (index % 26) as u8 + b'a'], crate::buffer::GLOBAL)).collect();
	let mut buffer = glyphs(&[1, 2]);
	assert_eq!(crate::shape::shape_with_masks(&face, &mut buffer, *b"latn", *b"dflt", &features).err(), Some(font_parse::Error::Unsupported(font_parse::Unsupported::Exceeded { limit: "features", ceiling: opentype_profile::limits::FEATURES, asked: ceiling as u64 + 1 })));
}

#[test]
// THE SHAPER IS FUZZED THE WAY THE PARSER IS: exhaustively rather than randomly. A shaper walks a
// font's layout tables - a script list into a feature list into a lookup list into subtables, each
// one an offset into the next - and every one of those is a place a crafted font can point somewhere
// else. Random bytes would find the shallow ones and miss the deep ones; every byte of a font this
// tree built, flipped four ways, reaches all of them.
//
// WHAT IS ASSERTED IS THAT IT ANSWERS. Whether a mutated font still shapes is not the question, and
// a test that demanded a particular answer would be worthless: some mutations produce a font that is
// perfectly valid and shapes differently. What must never happen is a panic, an out-of-bounds read,
// or a walk that does not come back.
fn a_layout_table_corrupted_anywhere_is_refused_rather_than_read_past() {
	let bytes = build::sample(Some(build::gsub_ligature(1, 2, 4)), Some(build::gpos_kern(1, 2, -40)), Some(build::gdef(&[3])));
	// The three layout tables, which is where every offset this fixture is about lives.
	for tag in [b"GSUB", b"GPOS", b"GDEF"] {
		let at = find_table(&bytes, tag);
		let entry = find_entry(&bytes, tag);
		let length = u32::from_be_bytes([bytes[entry + 12], bytes[entry + 13], bytes[entry + 14], bytes[entry + 15]]) as usize;
		for index in 0..length {
			for pattern in [0x01u8, 0x7F, 0x80, 0xFF] {
				let mut mutated = bytes.clone();
				mutated[at + index] ^= pattern;
				let Ok(face) = Face::open(&mutated, 0) else { continue };
				let mut buffer = glyphs(&[1, 2, 3]);
				let _ = crate::shape::shape(&face, &mut buffer, *b"latn", *b"dflt", &[*b"liga", *b"kern", *b"mark"]);
				// And through the script path too, which runs a shaper's own pass before any lookup.
				let mut buffer = Buffer::new();
				for (index, _) in ['a', 'b', 'c'].iter().enumerate() {
					buffer.push(index as u16 + 1, index as u32);
				}
				let _ = crate::shape::shape_run(&face, &mut buffer, &['a', 'b', 'c'], *b"dflt");
			}
		}
	}
}

#[test]
// AND A TRUNCATION AT EVERY LENGTH, which is the other half: a table cut short is not the same input
// as a table with a wrong byte in it, and the offsets that fail are different ones.
fn a_font_truncated_anywhere_is_refused_rather_than_shaped_past() {
	let bytes = build::sample(Some(build::gsub_ligature(1, 2, 4)), Some(build::gpos_kern(1, 2, -40)), Some(build::gdef(&[3])));
	for length in 0..bytes.len() {
		let Ok(face) = Face::open(&bytes[..length], 0) else { continue };
		let mut buffer = glyphs(&[1, 2, 3]);
		let _ = crate::shape::shape(&face, &mut buffer, *b"latn", *b"dflt", &[*b"liga", *b"kern", *b"mark"]);
	}
}

/// Where a table's DIRECTORY ENTRY is in a font this tree built.
fn find_entry(bytes: &[u8], tag: &[u8; 4]) -> usize {
	let count = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
	for index in 0..count {
		let entry = 12 + index * 16;
		if &bytes[entry..entry + 4] == tag {
			return entry;
		}
	}
	panic!("the fixture's own font has no {} table", core::str::from_utf8(tag).unwrap_or("????"));
}

/// Where a table's bytes start in a font this tree built.
fn find_table(bytes: &[u8], tag: &[u8; 4]) -> usize {
	let entry = find_entry(bytes, tag);
	u32::from_be_bytes([bytes[entry + 8], bytes[entry + 9], bytes[entry + 10], bytes[entry + 11]]) as usize
}
