//! `GSUB`, `GPOS` and `GDEF`, built rather than described.
//!
//! WHY THE CORPUS NEEDS REAL LAYOUT TABLES AND NOT A FIXTURE. A unit test can hand the shaper one
//! lookup in isolation and check that it fires. What that cannot show is a face whose SCRIPT LIST,
//! FEATURE LIST and LOOKUP LIST have to be walked in the order the format defines - several features
//! naming the same lookup, several scripts in one face, positional features that must reach one
//! letter and not its neighbour. The corpus faces carry all three lists for several scripts at once,
//! which is the shape a real face has and the shape a mistake hides in.
//!
//! THE LOOKUP LIST DECIDES THE ORDER LOOKUPS RUN IN, not the feature list. So the order lookups are
//! given here is normative for what the face means: a face whose `rlig` lookup preceded its joining
//! lookups would form its ligature from the nominal forms and never reach the positional ones.

use crate::write::{i16v, u16v};

/// One lookup: a type, its flags and its subtables.
pub struct Lookup {
	pub kind: u16,
	pub flags: u16,
	pub subtables: Vec<Vec<u8>>,
}

impl Lookup {
	pub fn new(kind: u16, subtable: Vec<u8>) -> Self {
		Self { kind, flags: 0, subtables: vec![subtable] }
	}

	/// A lookup that IGNORES MARKS, which is what a joining or kerning rule needs: a mark between
	/// two letters must not break the pair, and a lookup without this flag would see the mark as the
	/// neighbour and match nothing.
	pub fn ignoring_marks(kind: u16, subtable: Vec<u8>) -> Self {
		Self { kind, flags: 0x0008, subtables: vec![subtable] }
	}

	fn bytes(&self) -> Vec<u8> {
		let mut out = Vec::new();
		u16v(&mut out, self.kind);
		u16v(&mut out, self.flags);
		u16v(&mut out, self.subtables.len() as u16);
		let mut at = 6 + 2 * self.subtables.len();
		for subtable in &self.subtables {
			u16v(&mut out, at as u16);
			at += subtable.len();
		}
		for subtable in &self.subtables {
			out.extend_from_slice(subtable);
		}
		out
	}
}

/// A feature: its tag, and the lookups it turns on by index.
pub struct FeatureEntry {
	pub tag: [u8; 4],
	pub lookups: Vec<u16>,
}

/// A script: its tag, and the features its default language system selects by index.
pub struct ScriptEntry {
	pub tag: [u8; 4],
	pub features: Vec<u16>,
}

/// Coverage, format 1: the glyphs a lookup applies to, in ascending order - which the format
/// requires and a reader may binary-search.
pub fn coverage(glyphs: &[u16]) -> Vec<u8> {
	let mut sorted = glyphs.to_vec();
	sorted.sort_unstable();
	sorted.dedup();
	let mut out = Vec::new();
	u16v(&mut out, 1);
	u16v(&mut out, sorted.len() as u16);
	for glyph in sorted {
		u16v(&mut out, glyph);
	}
	out
}

/// A class definition, format 2: ranges of glyphs and the class each falls in.
pub fn class_def(ranges: &[(u16, u16, u16)]) -> Vec<u8> {
	let mut sorted = ranges.to_vec();
	sorted.sort_unstable();
	let mut out = Vec::new();
	u16v(&mut out, 2);
	u16v(&mut out, sorted.len() as u16);
	for (start, end, class) in sorted {
		u16v(&mut out, start);
		u16v(&mut out, end);
		u16v(&mut out, class);
	}
	out
}

/// `GDEF`, carrying the glyph classes.
///
/// A FACE WITHOUT THESE CLASSES CANNOT POSITION A MARK. `mark` and `mkmk` both select by class, and
/// a lookup that ignores marks has nothing to ignore when the face never said which glyphs are
/// marks - so the accent lands at the pen and the kerning pair breaks on the accent between them.
pub fn gdef(classes: &[(u16, u16, u16)]) -> Vec<u8> {
	let class_def = class_def(classes);
	let mut out = Vec::new();
	u16v(&mut out, 1);
	u16v(&mut out, 0);
	// glyph class def, attach list, lig caret list, mark attach class def = four offsets.
	u16v(&mut out, 12);
	u16v(&mut out, 0);
	u16v(&mut out, 0);
	u16v(&mut out, 0);
	out.extend_from_slice(&class_def);
	out
}

/// Single substitution, format 2: each covered glyph names WHICH glyph it becomes.
///
/// FORMAT 2 AND NOT FORMAT 1, which adds a constant to the glyph id. A face whose positional forms
/// happened to be laid out at a fixed distance could use format 1, and then a glyph inserted into
/// the middle of the order would silently change every substitution in the face.
pub fn single(pairs: &[(u16, u16)]) -> Vec<u8> {
	let mut sorted = pairs.to_vec();
	sorted.sort_unstable();
	let coverage = coverage(&sorted.iter().map(|(from, _)| *from).collect::<Vec<u16>>());
	let coverage_at = 6 + 2 * sorted.len();
	let mut out = Vec::new();
	u16v(&mut out, 2);
	u16v(&mut out, coverage_at as u16);
	u16v(&mut out, sorted.len() as u16);
	for (_, to) in &sorted {
		u16v(&mut out, *to);
	}
	out.extend_from_slice(&coverage);
	out
}

/// Multiple substitution, format 1: one covered glyph becomes a sequence.
pub fn multiple(entries: &[(u16, Vec<u16>)]) -> Vec<u8> {
	let mut sorted = entries.to_vec();
	sorted.sort_by_key(|(from, _)| *from);
	let coverage = coverage(&sorted.iter().map(|(from, _)| *from).collect::<Vec<u16>>());
	let mut sequences: Vec<Vec<u8>> = Vec::new();
	for (_, into) in &sorted {
		let mut sequence = Vec::new();
		u16v(&mut sequence, into.len() as u16);
		for glyph in into {
			u16v(&mut sequence, *glyph);
		}
		sequences.push(sequence);
	}
	let coverage_at = 6 + 2 * sorted.len();
	let mut at = coverage_at + coverage.len();
	let mut out = Vec::new();
	u16v(&mut out, 1);
	u16v(&mut out, coverage_at as u16);
	u16v(&mut out, sorted.len() as u16);
	for sequence in &sequences {
		u16v(&mut out, at as u16);
		at += sequence.len();
	}
	out.extend_from_slice(&coverage);
	for sequence in &sequences {
		out.extend_from_slice(sequence);
	}
	out
}

/// Ligature substitution, format 1: a sequence of glyphs becomes one.
///
/// THE COMPONENTS AFTER THE FIRST ARE WHAT THE SET STORES, because the first one is the covered
/// glyph. A face that repeated it would ask the shaper to match it twice.
pub fn ligature(entries: &[(Vec<u16>, u16)]) -> Vec<u8> {
	let mut firsts: Vec<u16> = entries.iter().filter_map(|(components, _)| components.first().copied()).collect();
	firsts.sort_unstable();
	firsts.dedup();
	let coverage = coverage(&firsts);

	// LONGEST FIRST WITHIN A SET, which the format requires: the shaper takes the first ligature
	// that matches, so a two-component entry before a three-component one would win over it and the
	// longer ligature could never form.
	let mut sets: Vec<Vec<u8>> = Vec::new();
	for first in &firsts {
		let mut mine: Vec<(Vec<u16>, u16)> = entries.iter().filter(|(components, _)| components.first() == Some(first)).cloned().collect();
		mine.sort_by(|left, right| right.0.len().cmp(&left.0.len()));
		let mut ligatures: Vec<Vec<u8>> = Vec::new();
		for (components, result) in &mine {
			let mut bytes = Vec::new();
			u16v(&mut bytes, *result);
			u16v(&mut bytes, components.len() as u16);
			for component in &components[1..] {
				u16v(&mut bytes, *component);
			}
			ligatures.push(bytes);
		}
		let mut set = Vec::new();
		u16v(&mut set, ligatures.len() as u16);
		let mut at = 2 + 2 * ligatures.len();
		for bytes in &ligatures {
			u16v(&mut set, at as u16);
			at += bytes.len();
		}
		for bytes in &ligatures {
			set.extend_from_slice(bytes);
		}
		sets.push(set);
	}

	let coverage_at = 6 + 2 * sets.len();
	let mut at = coverage_at + coverage.len();
	let mut out = Vec::new();
	u16v(&mut out, 1);
	u16v(&mut out, coverage_at as u16);
	u16v(&mut out, sets.len() as u16);
	for set in &sets {
		u16v(&mut out, at as u16);
		at += set.len();
	}
	out.extend_from_slice(&coverage);
	for set in &sets {
		out.extend_from_slice(set);
	}
	out
}

/// Pair positioning, format 1: the first glyph's advance is adjusted when the second follows.
pub fn pair(pairs: &[(u16, u16, i16)]) -> Vec<u8> {
	let mut firsts: Vec<u16> = pairs.iter().map(|(first, _, _)| *first).collect();
	firsts.sort_unstable();
	firsts.dedup();
	let coverage = coverage(&firsts);

	let mut sets: Vec<Vec<u8>> = Vec::new();
	for first in &firsts {
		let mut mine: Vec<(u16, i16)> = pairs.iter().filter(|(left, _, _)| left == first).map(|(_, second, adjust)| (*second, *adjust)).collect();
		// SORTED BY THE SECOND GLYPH, which the format requires so a reader may binary-search a set.
		mine.sort_unstable();
		let mut set = Vec::new();
		u16v(&mut set, mine.len() as u16);
		for (second, adjust) in &mine {
			u16v(&mut set, *second);
			i16v(&mut set, *adjust);
		}
		sets.push(set);
	}

	let coverage_at = 10 + 2 * sets.len();
	let mut at = coverage_at + coverage.len();
	let mut out = Vec::new();
	u16v(&mut out, 1);
	u16v(&mut out, coverage_at as u16);
	// The first value carries an x advance; the second carries nothing.
	u16v(&mut out, 0x0004);
	u16v(&mut out, 0x0000);
	u16v(&mut out, sets.len() as u16);
	for set in &sets {
		u16v(&mut out, at as u16);
		at += set.len();
	}
	out.extend_from_slice(&coverage);
	for set in &sets {
		out.extend_from_slice(set);
	}
	out
}

fn anchor(point: (i16, i16)) -> Vec<u8> {
	let mut out = Vec::new();
	u16v(&mut out, 1);
	i16v(&mut out, point.0);
	i16v(&mut out, point.1);
	out
}

/// An array of anchors, one record per entry, with the anchors packed after the records.
fn anchor_array(records: &[Vec<u8>], header: usize, anchors: &[Vec<u8>]) -> Vec<u8> {
	let mut out = Vec::new();
	let mut at = header;
	let mut bodies = Vec::new();
	for (index, record) in records.iter().enumerate() {
		out.extend_from_slice(record);
		u16v(&mut out, at as u16);
		at += anchors[index].len();
		bodies.push(anchors[index].clone());
	}
	for body in bodies {
		out.extend_from_slice(&body);
	}
	out
}

/// Mark-to-base attachment, format 1.
///
/// `marks` are `(glyph, class, anchor)` and `bases` are `(glyph, anchor per class)`. THE CLASS IS
/// WHAT DECIDES WHICH BASE ANCHOR A MARK USES: a face with an above-base and a below-base class
/// attaches the same base's two anchors to two different marks, which is the case a single-class
/// fixture cannot show.
pub fn mark_to_base(marks: &[(u16, u16, (i16, i16))], bases: &[(u16, Vec<(i16, i16)>)], classes: u16) -> Vec<u8> {
	let mut sorted_marks = marks.to_vec();
	sorted_marks.sort_by_key(|(glyph, _, _)| *glyph);
	let mut sorted_bases = bases.to_vec();
	sorted_bases.sort_by_key(|(glyph, _)| *glyph);

	let mark_coverage = coverage(&sorted_marks.iter().map(|(glyph, _, _)| *glyph).collect::<Vec<u16>>());
	let base_coverage = coverage(&sorted_bases.iter().map(|(glyph, _)| *glyph).collect::<Vec<u16>>());

	// The mark array: one (class, anchor offset) record each.
	let mark_records: Vec<Vec<u8>> = sorted_marks
		.iter()
		.map(|(_, class, _)| {
			let mut record = Vec::new();
			u16v(&mut record, *class);
			record
		})
		.collect();
	let mark_anchors: Vec<Vec<u8>> = sorted_marks.iter().map(|(_, _, point)| anchor(*point)).collect();
	let mut mark_array = Vec::new();
	u16v(&mut mark_array, sorted_marks.len() as u16);
	mark_array.extend_from_slice(&anchor_array(&mark_records, 2 + 4 * sorted_marks.len(), &mark_anchors));

	// The base array: one anchor offset per class, per base.
	let mut base_array = Vec::new();
	u16v(&mut base_array, sorted_bases.len() as u16);
	let header = 2 + 2 * classes as usize * sorted_bases.len();
	let mut at = header;
	let mut anchors = Vec::new();
	for (_, points) in &sorted_bases {
		for class in 0..classes as usize {
			let point = points.get(class).copied().unwrap_or((0, 0));
			let bytes = anchor(point);
			u16v(&mut base_array, at as u16);
			at += bytes.len();
			anchors.push(bytes);
		}
	}
	for bytes in anchors {
		base_array.extend_from_slice(&bytes);
	}

	let mark_coverage_at = 12usize;
	let base_coverage_at = mark_coverage_at + mark_coverage.len();
	let mark_array_at = base_coverage_at + base_coverage.len();
	let base_array_at = mark_array_at + mark_array.len();
	let mut out = Vec::new();
	u16v(&mut out, 1);
	u16v(&mut out, mark_coverage_at as u16);
	u16v(&mut out, base_coverage_at as u16);
	u16v(&mut out, classes);
	u16v(&mut out, mark_array_at as u16);
	u16v(&mut out, base_array_at as u16);
	out.extend_from_slice(&mark_coverage);
	out.extend_from_slice(&base_coverage);
	out.extend_from_slice(&mark_array);
	out.extend_from_slice(&base_array);
	out
}

/// A whole `GSUB` or `GPOS`: the three lists, and every offset from the table start.
pub fn table(scripts: &[ScriptEntry], features: &[FeatureEntry], lookups: &[Lookup]) -> Vec<u8> {
	// THE SCRIPT LIST IS SORTED BY TAG and so is the feature list, which the format requires.
	let mut sorted_scripts: Vec<&ScriptEntry> = scripts.iter().collect();
	sorted_scripts.sort_by_key(|entry| entry.tag);

	let mut script_records = Vec::new();
	let mut script_bodies: Vec<Vec<u8>> = Vec::new();
	let mut at = 2 + 6 * sorted_scripts.len();
	for entry in &sorted_scripts {
		// One default language system, naming the features it selects. A face with no `DefaultLangSys`
		// answers nothing for a language it does not name, which is every language here.
		let mut lang_sys = Vec::new();
		u16v(&mut lang_sys, 0);
		u16v(&mut lang_sys, 0xFFFF);
		u16v(&mut lang_sys, entry.features.len() as u16);
		for index in &entry.features {
			u16v(&mut lang_sys, *index);
		}
		let mut script = Vec::new();
		// The default language system follows the script header; there are no named ones.
		u16v(&mut script, 4);
		u16v(&mut script, 0);
		script.extend_from_slice(&lang_sys);
		script_records.extend_from_slice(&entry.tag);
		u16v(&mut script_records, at as u16);
		at += script.len();
		script_bodies.push(script);
	}
	let mut script_list = Vec::new();
	u16v(&mut script_list, sorted_scripts.len() as u16);
	script_list.extend_from_slice(&script_records);
	for body in &script_bodies {
		script_list.extend_from_slice(body);
	}

	let mut feature_records = Vec::new();
	let mut feature_bodies: Vec<Vec<u8>> = Vec::new();
	let mut at = 2 + 6 * features.len();
	for entry in features {
		let mut feature = Vec::new();
		u16v(&mut feature, 0);
		u16v(&mut feature, entry.lookups.len() as u16);
		for index in &entry.lookups {
			u16v(&mut feature, *index);
		}
		feature_records.extend_from_slice(&entry.tag);
		u16v(&mut feature_records, at as u16);
		at += feature.len();
		feature_bodies.push(feature);
	}
	let mut feature_list = Vec::new();
	u16v(&mut feature_list, features.len() as u16);
	feature_list.extend_from_slice(&feature_records);
	for body in &feature_bodies {
		feature_list.extend_from_slice(body);
	}

	let encoded: Vec<Vec<u8>> = lookups.iter().map(Lookup::bytes).collect();
	let mut lookup_list = Vec::new();
	u16v(&mut lookup_list, encoded.len() as u16);
	let mut at = 2 + 2 * encoded.len();
	for lookup in &encoded {
		u16v(&mut lookup_list, at as u16);
		at += lookup.len();
	}
	for lookup in &encoded {
		lookup_list.extend_from_slice(lookup);
	}

	let header = 10usize;
	let script_at = header;
	let feature_at = script_at + script_list.len();
	let lookup_at = feature_at + feature_list.len();
	let mut out = Vec::new();
	u16v(&mut out, 1);
	u16v(&mut out, 0);
	u16v(&mut out, script_at as u16);
	u16v(&mut out, feature_at as u16);
	u16v(&mut out, lookup_at as u16);
	out.extend_from_slice(&script_list);
	out.extend_from_slice(&feature_list);
	out.extend_from_slice(&lookup_list);
	out
}
