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
	/// A `GSUB` whose lookup list holds SEVERAL lookups, with the feature naming the first.
	///
	/// THE ONE-LOOKUP HELPER BELOW CANNOT EXPRESS A CHAIN, and a chain is exactly what a contextual
	/// rule builds: lookup 0 invokes lookup 1, which invokes lookup 2. Without a list a fixture can
	/// only ever reach depth one, which is the depth nothing interesting happens at.
	pub fn layout_many(tag: &[u8; 4], lookups: &[std::vec::Vec<u8>]) -> std::vec::Vec<u8> {
		let mut lang_sys = std::vec::Vec::new();
		u16(&mut lang_sys, 0);
		u16(&mut lang_sys, 0xFFFF);
		u16(&mut lang_sys, 1);
		u16(&mut lang_sys, 0);

		let mut script = std::vec::Vec::new();
		u16(&mut script, 4);
		u16(&mut script, 0);
		script.extend_from_slice(&lang_sys);

		let mut script_list = std::vec::Vec::new();
		u16(&mut script_list, 1);
		script_list.extend_from_slice(b"DFLT");
		u16(&mut script_list, 8);
		script_list.extend_from_slice(&script);

		let mut feature = std::vec::Vec::new();
		u16(&mut feature, 0);
		u16(&mut feature, 1);
		u16(&mut feature, 0); // the feature names the FIRST lookup, and the chain reaches the rest

		let mut feature_list = std::vec::Vec::new();
		u16(&mut feature_list, 1);
		feature_list.extend_from_slice(tag);
		u16(&mut feature_list, 8);
		feature_list.extend_from_slice(&feature);

		let mut lookup_list = std::vec::Vec::new();
		u16(&mut lookup_list, lookups.len() as u16);
		let mut at = 2 + lookups.len() * 2;
		for lookup in lookups {
			u16(&mut lookup_list, at as u16);
			at += lookup.len();
		}
		for lookup in lookups {
			lookup_list.extend_from_slice(lookup);
		}

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

	/// A contextual substitution, format 1: the covered glyph, matched alone, invoking one lookup.
	pub fn gsub_context(glyph: u16, invokes: u16) -> std::vec::Vec<u8> {
		// One rule: a one-glyph sequence (so no further input after the covered glyph) and one
		// substitution record naming the lookup to run at sequence index 0.
		let mut rule = std::vec::Vec::new();
		u16(&mut rule, 1); // glyph count, the covered one included
		u16(&mut rule, 1); // one substitution record
		u16(&mut rule, 0); // at sequence index 0
		u16(&mut rule, invokes);

		let mut set = std::vec::Vec::new();
		u16(&mut set, 1); // one rule
		u16(&mut set, 4); // which follows the header and the one offset
		set.extend_from_slice(&rule);

		let coverage = coverage(&[glyph]);
		let coverage_at = 6usize + 2; // format, coverage offset, set count, one set offset
		let set_at = coverage_at + coverage.len();
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 1); // format 1
		u16(&mut subtable, coverage_at as u16);
		u16(&mut subtable, 1); // one rule set
		u16(&mut subtable, set_at as u16);
		subtable.extend_from_slice(&coverage);
		subtable.extend_from_slice(&set);
		lookup(5, 0, subtable)
	}

	/// A multiple substitution: the covered glyph becomes the sequence given.
	pub fn gsub_multiple(from: u16, into: &[u16]) -> std::vec::Vec<u8> {
		let mut sequence = std::vec::Vec::new();
		u16(&mut sequence, into.len() as u16);
		for glyph in into {
			u16(&mut sequence, *glyph);
		}
		let coverage = coverage(&[from]);
		let coverage_at = 8usize; // format, coverage offset, sequence count, one sequence offset
		let sequence_at = coverage_at + coverage.len();
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 1);
		u16(&mut subtable, coverage_at as u16);
		u16(&mut subtable, 1);
		u16(&mut subtable, sequence_at as u16);
		subtable.extend_from_slice(&coverage);
		subtable.extend_from_slice(&sequence);
		layout(b"liga", lookup(2, 0, subtable))
	}

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
	/// A bare single-substitution LOOKUP, for a fixture that assembles its own lookup list.
	pub fn single_lookup(from: u16, to: u16) -> std::vec::Vec<u8> {
		let coverage = coverage(&[from]);
		let coverage_at = 8usize;
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 2);
		u16(&mut subtable, coverage_at as u16);
		u16(&mut subtable, 1);
		u16(&mut subtable, to);
		subtable.extend_from_slice(&coverage);
		lookup(1, 0, subtable)
	}

	/// The sample font with a stated glyph count, for a fixture whose substitutions produce more
	/// glyphs than the five the ordinary one declares.
	pub fn sample_many(gsub: Option<std::vec::Vec<u8>>, gpos: Option<std::vec::Vec<u8>>, gdef: Option<std::vec::Vec<u8>>, glyphs: u16) -> std::vec::Vec<u8> {
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
		tables.push((*b"hhea", hhea(glyphs)));
		let advances: std::vec::Vec<u16> = (0..glyphs).map(|index| 200 + index * 3).collect();
		tables.push((*b"hmtx", hmtx(&advances)));
		tables.push((*b"maxp", maxp(glyphs)));
		font(&tables)
	}

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

	/// A `cmap` with one format 12 subtable in the Windows full-Unicode record, mapping the
	/// characters given - which is what lets a fixture ask the face the question a real caller asks.
	pub fn cmap(entries: &[(char, u16)]) -> std::vec::Vec<u8> {
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

	/// A layout table with ONE LOOKUP PER FEATURE - feature `i` names lookup `i` - under `DFLT`,
	/// whose default language system selects the features `default` names and whose named language
	/// systems select theirs.
	pub fn layout_features(features: &[([u8; 4], std::vec::Vec<u8>)], default: &[u16], languages: &[([u8; 4], &[u16])]) -> std::vec::Vec<u8> {
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

		let bodies: std::vec::Vec<std::vec::Vec<u8>> = (0..features.len())
			.map(|index| {
				let mut body = std::vec::Vec::new();
				u16(&mut body, 0);
				u16(&mut body, 1);
				u16(&mut body, index as u16);
				body
			})
			.collect();
		let mut feature_list = std::vec::Vec::new();
		u16(&mut feature_list, features.len() as u16);
		let mut at = 2 + 6 * features.len();
		for ((tag, _), body) in features.iter().zip(&bodies) {
			feature_list.extend_from_slice(tag);
			u16(&mut feature_list, at as u16);
			at += body.len();
		}
		for body in &bodies {
			feature_list.extend_from_slice(body);
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

		let script_at = 10usize;
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

	/// A bare two-component ligature LOOKUP: `first` + `second` becomes `result`.
	pub fn ligature_lookup(first: u16, second: u16, result: u16) -> std::vec::Vec<u8> {
		let mut ligature = std::vec::Vec::new();
		u16(&mut ligature, result);
		u16(&mut ligature, 2);
		u16(&mut ligature, second);
		let mut set = std::vec::Vec::new();
		u16(&mut set, 1);
		u16(&mut set, 4);
		set.extend_from_slice(&ligature);
		let coverage = coverage(&[first]);
		let coverage_at = 8usize;
		let set_at = coverage_at + coverage.len();
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 1);
		u16(&mut subtable, coverage_at as u16);
		u16(&mut subtable, 1);
		u16(&mut subtable, set_at as u16);
		subtable.extend_from_slice(&coverage);
		subtable.extend_from_slice(&set);
		lookup(4, 0, subtable)
	}

	/// A bare pair-kerning LOOKUP: `first` before `second` changes the first's advance by `kern`.
	pub fn kern_lookup(first: u16, second: u16, kern: i16) -> std::vec::Vec<u8> {
		let mut pair_set = std::vec::Vec::new();
		u16(&mut pair_set, 1);
		u16(&mut pair_set, second);
		u16(&mut pair_set, kern as u16);
		let coverage = coverage(&[first]);
		let coverage_at = 12usize;
		let set_at = coverage_at + coverage.len();
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 1);
		u16(&mut subtable, coverage_at as u16);
		u16(&mut subtable, 0x0004);
		u16(&mut subtable, 0x0000);
		u16(&mut subtable, 1);
		u16(&mut subtable, set_at as u16);
		subtable.extend_from_slice(&coverage);
		subtable.extend_from_slice(&pair_set);
		lookup(2, 0, subtable)
	}

	/// A bare mark-to-base LOOKUP: `mark` attaches to `base` at the anchors given.
	pub fn mark_lookup(base: u16, mark: u16, base_anchor: (i16, i16), mark_anchor: (i16, i16)) -> std::vec::Vec<u8> {
		let anchor = |(x, y): (i16, i16)| {
			let mut out = std::vec::Vec::new();
			u16(&mut out, 1);
			u16(&mut out, x as u16);
			u16(&mut out, y as u16);
			out
		};
		let mark_coverage = coverage(&[mark]);
		let base_coverage = coverage(&[base]);
		let mut mark_array = std::vec::Vec::new();
		u16(&mut mark_array, 1);
		u16(&mut mark_array, 0);
		u16(&mut mark_array, 6);
		mark_array.extend_from_slice(&anchor(mark_anchor));
		let mut base_array = std::vec::Vec::new();
		u16(&mut base_array, 1);
		u16(&mut base_array, 4);
		base_array.extend_from_slice(&anchor(base_anchor));
		let mark_coverage_at = 12usize;
		let base_coverage_at = mark_coverage_at + mark_coverage.len();
		let mark_array_at = base_coverage_at + base_coverage.len();
		let base_array_at = mark_array_at + mark_array.len();
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 1);
		u16(&mut subtable, mark_coverage_at as u16);
		u16(&mut subtable, base_coverage_at as u16);
		u16(&mut subtable, 1);
		u16(&mut subtable, mark_array_at as u16);
		u16(&mut subtable, base_array_at as u16);
		subtable.extend_from_slice(&mark_coverage);
		subtable.extend_from_slice(&base_coverage);
		subtable.extend_from_slice(&mark_array);
		subtable.extend_from_slice(&base_array);
		lookup(4, 0, subtable)
	}

	/// A face of `glyphs` glyphs carrying a `cmap` and whichever layout tables are given.
	pub fn mapped(characters: &[(char, u16)], gsub: Option<std::vec::Vec<u8>>, gpos: Option<std::vec::Vec<u8>>, gdef: Option<std::vec::Vec<u8>>, glyphs: u16) -> std::vec::Vec<u8> {
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
		tables.push((*b"cmap", cmap(characters)));
		tables.push((*b"head", head()));
		tables.push((*b"hhea", hhea(glyphs)));
		let advances: std::vec::Vec<u16> = (0..glyphs).map(|index| 200 + index * 3).collect();
		tables.push((*b"hmtx", hmtx(&advances)));
		tables.push((*b"maxp", maxp(glyphs)));
		font(&tables)
	}

	/// A lookup with the flags given and one subtable - and, when the flags name one, the mark filtering
	/// set, which the format puts after the subtable offsets.
	pub fn flagged(kind: u16, flags: u16, subtable: std::vec::Vec<u8>, set: Option<u16>) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, kind);
		u16(&mut out, flags);
		u16(&mut out, 1);
		u16(&mut out, if set.is_some() { 10 } else { 8 });
		if let Some(set) = set {
			u16(&mut out, set);
		}
		out.extend_from_slice(&subtable);
		out
	}

	fn anchor_bytes((x, y): (i16, i16)) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, x as u16);
		u16(&mut out, y as u16);
		out
	}

	/// A ligature subtable: `first` and then `rest` become `result`.
	pub fn ligature_subtable(first: u16, rest: &[u16], result: u16) -> std::vec::Vec<u8> {
		let mut ligature = std::vec::Vec::new();
		u16(&mut ligature, result);
		u16(&mut ligature, rest.len() as u16 + 1);
		for glyph in rest {
			u16(&mut ligature, *glyph);
		}
		let mut set = std::vec::Vec::new();
		u16(&mut set, 1);
		u16(&mut set, 4);
		set.extend_from_slice(&ligature);
		let coverage = coverage(&[first]);
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 1);
		u16(&mut subtable, 8);
		u16(&mut subtable, 1);
		u16(&mut subtable, 8 + coverage.len() as u16);
		subtable.extend_from_slice(&coverage);
		subtable.extend_from_slice(&set);
		subtable
	}

	/// A single substitution subtable, format 2.
	pub fn single_subtable(from: u16, to: u16) -> std::vec::Vec<u8> {
		let coverage = coverage(&[from]);
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 2);
		u16(&mut subtable, 8);
		u16(&mut subtable, 1);
		u16(&mut subtable, to);
		subtable.extend_from_slice(&coverage);
		subtable
	}

	/// A pair subtable, format 1: `first` before `second` changes the first's advance by `kern`.
	pub fn pair_subtable(first: u16, second: u16, kern: i16) -> std::vec::Vec<u8> {
		let mut pair_set = std::vec::Vec::new();
		u16(&mut pair_set, 1);
		u16(&mut pair_set, second);
		u16(&mut pair_set, kern as u16);
		let coverage = coverage(&[first]);
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 1);
		u16(&mut subtable, 12);
		u16(&mut subtable, 0x0004);
		u16(&mut subtable, 0x0000);
		u16(&mut subtable, 1);
		u16(&mut subtable, 12 + coverage.len() as u16);
		subtable.extend_from_slice(&coverage);
		subtable.extend_from_slice(&pair_set);
		subtable
	}

	/// A single adjustment subtable, format 1: every covered glyph is PLACED `(x, y)` off its pen.
	pub fn placement_subtable(glyph: u16, (x, y): (i16, i16)) -> std::vec::Vec<u8> {
		let coverage = coverage(&[glyph]);
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 1);
		u16(&mut subtable, 10);
		u16(&mut subtable, 0x0003);
		u16(&mut subtable, x as u16);
		u16(&mut subtable, y as u16);
		subtable.extend_from_slice(&coverage);
		subtable
	}

	/// A mark-to-base or mark-to-mark subtable - the two share one layout: `marks` as `(glyph, class,
	/// anchor)` and what they attach to as `(glyph, one anchor per class, or none)`.
	pub fn mark_subtable(marks: &[(u16, u16, (i16, i16))], bases: &[(u16, std::vec::Vec<Option<(i16, i16)>>)], classes: u16) -> std::vec::Vec<u8> {
		let mut sorted_marks = marks.to_vec();
		sorted_marks.sort_by_key(|(glyph, _, _)| *glyph);
		let mut sorted_bases = bases.to_vec();
		sorted_bases.sort_by_key(|(glyph, _)| *glyph);
		let mark_coverage = coverage(&sorted_marks.iter().map(|(glyph, _, _)| *glyph).collect::<std::vec::Vec<u16>>());
		let base_coverage = coverage(&sorted_bases.iter().map(|(glyph, _)| *glyph).collect::<std::vec::Vec<u16>>());
		let mut mark_array = std::vec::Vec::new();
		u16(&mut mark_array, sorted_marks.len() as u16);
		let mut at = 2 + 4 * sorted_marks.len();
		let mut anchors = std::vec::Vec::new();
		for (_, class, point) in &sorted_marks {
			u16(&mut mark_array, *class);
			u16(&mut mark_array, at as u16);
			let bytes = anchor_bytes(*point);
			at += bytes.len();
			anchors.extend_from_slice(&bytes);
		}
		mark_array.extend_from_slice(&anchors);
		let mut base_array = std::vec::Vec::new();
		u16(&mut base_array, sorted_bases.len() as u16);
		let mut at = 2 + 2 * classes as usize * sorted_bases.len();
		let mut anchors = std::vec::Vec::new();
		for (_, points) in &sorted_bases {
			for class in 0..classes as usize {
				match points.get(class).copied().flatten() {
					Some(point) => {
						u16(&mut base_array, at as u16);
						let bytes = anchor_bytes(point);
						at += bytes.len();
						anchors.extend_from_slice(&bytes);
					}
					None => u16(&mut base_array, 0),
				}
			}
		}
		base_array.extend_from_slice(&anchors);
		let mark_coverage_at = 12usize;
		let base_coverage_at = mark_coverage_at + mark_coverage.len();
		let mark_array_at = base_coverage_at + base_coverage.len();
		let base_array_at = mark_array_at + mark_array.len();
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 1);
		u16(&mut subtable, mark_coverage_at as u16);
		u16(&mut subtable, base_coverage_at as u16);
		u16(&mut subtable, classes);
		u16(&mut subtable, mark_array_at as u16);
		u16(&mut subtable, base_array_at as u16);
		subtable.extend_from_slice(&mark_coverage);
		subtable.extend_from_slice(&base_coverage);
		subtable.extend_from_slice(&mark_array);
		subtable.extend_from_slice(&base_array);
		subtable
	}

	/// A mark-to-ligature subtable, one mark class: `marks` as `(glyph, anchor)` and one ligature with
	/// an anchor per component.
	pub fn mark_ligature_subtable(marks: &[(u16, (i16, i16))], ligature: u16, components: &[(i16, i16)]) -> std::vec::Vec<u8> {
		let mark_coverage = coverage(&marks.iter().map(|(glyph, _)| *glyph).collect::<std::vec::Vec<u16>>());
		let ligature_coverage = coverage(&[ligature]);
		let mut mark_array = std::vec::Vec::new();
		u16(&mut mark_array, marks.len() as u16);
		let mut at = 2 + 4 * marks.len();
		let mut anchors = std::vec::Vec::new();
		for (_, point) in marks {
			u16(&mut mark_array, 0);
			u16(&mut mark_array, at as u16);
			let bytes = anchor_bytes(*point);
			at += bytes.len();
			anchors.extend_from_slice(&bytes);
		}
		mark_array.extend_from_slice(&anchors);
		// The ligature attach table: a component count and one anchor offset per component.
		let mut attach = std::vec::Vec::new();
		u16(&mut attach, components.len() as u16);
		let mut at = 2 + 2 * components.len();
		let mut anchors = std::vec::Vec::new();
		for point in components {
			u16(&mut attach, at as u16);
			let bytes = anchor_bytes(*point);
			at += bytes.len();
			anchors.extend_from_slice(&bytes);
		}
		attach.extend_from_slice(&anchors);
		let mut ligature_array = std::vec::Vec::new();
		u16(&mut ligature_array, 1);
		u16(&mut ligature_array, 4);
		ligature_array.extend_from_slice(&attach);
		let mark_coverage_at = 12usize;
		let ligature_coverage_at = mark_coverage_at + mark_coverage.len();
		let mark_array_at = ligature_coverage_at + ligature_coverage.len();
		let ligature_array_at = mark_array_at + mark_array.len();
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 1);
		u16(&mut subtable, mark_coverage_at as u16);
		u16(&mut subtable, ligature_coverage_at as u16);
		u16(&mut subtable, 1);
		u16(&mut subtable, mark_array_at as u16);
		u16(&mut subtable, ligature_array_at as u16);
		subtable.extend_from_slice(&mark_coverage);
		subtable.extend_from_slice(&ligature_coverage);
		subtable.extend_from_slice(&mark_array);
		subtable.extend_from_slice(&ligature_array);
		subtable
	}

	/// A cursive subtable: each covered glyph with its entry and exit anchors, either of which may be
	/// absent. The glyphs must be given in ascending order.
	pub fn cursive_subtable(entries: &[(u16, Option<(i16, i16)>, Option<(i16, i16)>)]) -> std::vec::Vec<u8> {
		let coverage = coverage(&entries.iter().map(|(glyph, _, _)| *glyph).collect::<std::vec::Vec<u16>>());
		let header = 6 + 4 * entries.len();
		let mut records = std::vec::Vec::new();
		let mut anchors = std::vec::Vec::new();
		let mut at = header + coverage.len();
		for (_, entry, exit) in entries {
			for point in [entry, exit] {
				match point {
					Some(point) => {
						u16(&mut records, at as u16);
						let bytes = anchor_bytes(*point);
						at += bytes.len();
						anchors.extend_from_slice(&bytes);
					}
					None => u16(&mut records, 0),
				}
			}
		}
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 1);
		u16(&mut subtable, header as u16);
		u16(&mut subtable, entries.len() as u16);
		subtable.extend_from_slice(&records);
		subtable.extend_from_slice(&coverage);
		subtable.extend_from_slice(&anchors);
		subtable
	}

	/// A chaining contextual subtable, format 3: a backtrack, an input and a lookahead, each glyph a
	/// coverage of the glyphs given, and the nested lookups as `(sequence index, lookup index)`.
	pub fn chain_subtable(backtrack: &[&[u16]], input: &[&[u16]], lookahead: &[&[u16]], nested: &[(u16, u16)]) -> std::vec::Vec<u8> {
		let coverages: std::vec::Vec<std::vec::Vec<u8>> = backtrack.iter().chain(input).chain(lookahead).map(|glyphs| coverage(glyphs)).collect();
		let header = 2 + 2 + 2 * backtrack.len() + 2 + 2 * input.len() + 2 + 2 * lookahead.len() + 2 + 4 * nested.len();
		let mut offsets = std::vec::Vec::new();
		let mut at = header;
		for bytes in &coverages {
			offsets.push(at as u16);
			at += bytes.len();
		}
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 3);
		let mut next = offsets.iter();
		for count in [backtrack.len(), input.len(), lookahead.len()] {
			u16(&mut subtable, count as u16);
			for _ in 0..count {
				u16(&mut subtable, *next.next().expect("an offset per coverage"));
			}
		}
		u16(&mut subtable, nested.len() as u16);
		for (sequence, lookup) in nested {
			u16(&mut subtable, *sequence);
			u16(&mut subtable, *lookup);
		}
		for bytes in &coverages {
			subtable.extend_from_slice(bytes);
		}
		subtable
	}

	/// `GDEF` version 1.2: the glyph classes, the mark attachment classes, and the mark glyph sets.
	pub fn gdef_full(classes: &[(u16, u16, u16)], mark_classes: &[(u16, u16, u16)], sets: &[&[u16]]) -> std::vec::Vec<u8> {
		let class_def = |ranges: &[(u16, u16, u16)]| {
			let mut out = std::vec::Vec::new();
			u16(&mut out, 2);
			u16(&mut out, ranges.len() as u16);
			for (first, last, class) in ranges {
				u16(&mut out, *first);
				u16(&mut out, *last);
				u16(&mut out, *class);
			}
			out
		};
		let glyph_classes = class_def(classes);
		let attach_classes = class_def(mark_classes);
		let mut mark_sets = std::vec::Vec::new();
		u16(&mut mark_sets, 1);
		u16(&mut mark_sets, sets.len() as u16);
		let coverages: std::vec::Vec<std::vec::Vec<u8>> = sets.iter().map(|glyphs| coverage(glyphs)).collect();
		let mut at = 4 + 4 * sets.len();
		for bytes in &coverages {
			u32(&mut mark_sets, at as u32);
			at += bytes.len();
		}
		for bytes in &coverages {
			mark_sets.extend_from_slice(bytes);
		}
		let header = 14usize;
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, 2);
		u16(&mut out, header as u16);
		u16(&mut out, 0);
		u16(&mut out, 0);
		u16(&mut out, (header + glyph_classes.len()) as u16);
		u16(&mut out, (header + glyph_classes.len() + attach_classes.len()) as u16);
		out.extend_from_slice(&glyph_classes);
		out.extend_from_slice(&attach_classes);
		out.extend_from_slice(&mark_sets);
		out
	}

	/// A face whose glyphs have the advances given, carrying whichever layout tables are given.
	pub fn with_advances(gsub: Option<std::vec::Vec<u8>>, gpos: Option<std::vec::Vec<u8>>, gdef: Option<std::vec::Vec<u8>>, advances: &[u16]) -> std::vec::Vec<u8> {
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
		tables.push((*b"hhea", hhea(advances.len() as u16)));
		tables.push((*b"hmtx", hmtx(advances)));
		tables.push((*b"maxp", maxp(advances.len() as u16)));
		font(&tables)
	}
}

/// A buffer of the glyphs given, each its own cluster.
fn glyphs(glyphs: &[u16]) -> Buffer {
	let mut buffer = Buffer::new();
	for (index, glyph) in glyphs.iter().enumerate() {
		buffer.push(*glyph, index as u32).expect("room for a glyph");
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
	buffer.decompose(1, &[7, 8, 9]).expect("a decomposition well inside the ceiling");
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
	assert_eq!(cursive_forms(&['\u{0628}', '\u{0628}']), Ok(std::vec![Form::Initial, Form::Final]));
	// Three of them: initial, medial, final.
	assert_eq!(cursive_forms(&['\u{0628}', '\u{0628}', '\u{0628}']), Ok(std::vec![Form::Initial, Form::Medial, Form::Final]));
	// A letter alone is isolated.
	assert_eq!(cursive_forms(&['\u{0628}']), Ok(std::vec![Form::Isolated]));
	// ALEF joins BACKWARD only, so what follows it starts a new join: `با` is initial, final; but
	// `اب` is isolated, isolated - the alef cannot reach forward.
	assert_eq!(cursive_forms(&['\u{0628}', '\u{0627}']), Ok(std::vec![Form::Initial, Form::Final]));
	assert_eq!(cursive_forms(&['\u{0627}', '\u{0628}']), Ok(std::vec![Form::Isolated, Form::Isolated]));
	// A SPACE BREAKS THE JOIN, which is what makes a word a word.
	assert_eq!(cursive_forms(&['\u{0628}', ' ', '\u{0628}']), Ok(std::vec![Form::Isolated, Form::Isolated, Form::Isolated]));
	// AND A MARK BETWEEN TWO LETTERS IS TRANSPARENT. This is the rule a shaper looking at its
	// immediate neighbour gets wrong, and in Arabic a vowel mark sits between letters constantly -
	// so that shaper breaks the join almost everywhere.
	assert_eq!(cursive_forms(&['\u{0628}', '\u{064E}', '\u{0628}']), Ok(std::vec![Form::Initial, Form::Isolated, Form::Final]));
}

#[test]
// THE MASK IS WHAT MAKES A POSITIONAL FEATURE POSITIONAL. `fina` applies to the last letter of a
// word and to no other; a shaper that could only turn it on globally would render a word made
// entirely of endings.
fn the_positional_features_apply_only_where_their_form_was_decided() {
	use crate::scripts::{FINA, INIT, MEDI, apply_cursive_masks};
	let characters = ['\u{0628}', '\u{0628}', '\u{0628}'];
	let mut buffer = glyphs(&[1, 1, 1]);
	apply_cursive_masks(&mut buffer, &characters).expect("masks");
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
	apply_cursive_masks(&mut buffer, &characters).expect("masks");
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
	assert_eq!(syllable_starts(&characters), Ok(std::vec![0]), "one syllable");
	let mut buffer = glyphs(&[10, 11]);
	reorder_indic(&characters, &mut buffer).expect("reordering");
	assert_eq!(buffer.infos.iter().map(|info| info.glyph).collect::<std::vec::Vec<_>>(), std::vec![11, 10], "the vowel sign is drawn first");
	// A post-base sign is left where it is: ा is `Right`, not `Left`.
	let characters = ['\u{0915}', '\u{093E}'];
	let mut buffer = glyphs(&[10, 12]);
	reorder_indic(&characters, &mut buffer).expect("reordering");
	assert_eq!(buffer.infos.iter().map(|info| info.glyph).collect::<std::vec::Vec<_>>(), std::vec![10, 12]);
	// TWO SYLLABLES REORDER INDEPENDENTLY, which is what the syllable boundaries are for: the vowel
	// of the second must not travel to the front of the first.
	let characters = ['\u{0915}', '\u{093F}', '\u{0916}', '\u{093F}'];
	assert_eq!(syllable_starts(&characters), Ok(std::vec![0, 2]));
	let mut buffer = glyphs(&[10, 11, 20, 21]);
	reorder_indic(&characters, &mut buffer).expect("reordering");
	assert_eq!(buffer.infos.iter().map(|info| info.glyph).collect::<std::vec::Vec<_>>(), std::vec![11, 10, 21, 20]);
	// AND A CONJUNCT IS ONE SYLLABLE: consonant, virama, consonant.
	let characters = ['\u{0915}', '\u{094D}', '\u{0937}'];
	assert_eq!(syllable_starts(&characters), Ok(std::vec![0]), "the virama binds the next consonant in");
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
	let composed = compose_hangul(&characters, &mut buffer).expect("composition");
	assert_eq!(composed, std::vec!['\u{D55C}']);
	assert_eq!(buffer.len(), 1, "the buffer follows the characters");
	assert_eq!(buffer.infos[0].cluster, 0, "and keeps the first jamo's cluster");
	// Two jamo with no trailing consonant compose too: ᄀ ᅡ becomes 가.
	let characters = ['\u{1100}', '\u{1161}'];
	let mut buffer = glyphs(&[1, 2]);
	assert_eq!(compose_hangul(&characters, &mut buffer), Ok(std::vec!['\u{AC00}']));
	// A LEAD WITH NOTHING TO COMPOSE WITH IS LEFT ALONE rather than dropped, which is what a font
	// carrying real jamo glyphs needs.
	let characters = ['\u{1100}', 'a'];
	let mut buffer = glyphs(&[1, 2]);
	assert_eq!(compose_hangul(&characters, &mut buffer), Ok(std::vec!['\u{1100}', 'a']));
	assert_eq!(buffer.len(), 2);
	// And an already-composed syllable passes through untouched.
	let characters = ['\u{D55C}'];
	let mut buffer = glyphs(&[9]);
	assert_eq!(compose_hangul(&characters, &mut buffer), Ok(std::vec!['\u{D55C}']));
}

#[test]
// MYANMAR REORDERS BY CATEGORY rather than by position: the medial `ra` and the pre-base vowel are
// both written after their consonant and drawn before it, and the medial goes first of all.
fn myanmar_moves_its_medial_and_pre_base_vowel_in_front() {
	use crate::scripts::reorder_myanmar;
	// က ြ ေ - KA, medial RA, vowel E: drawn as medial, vowel, consonant.
	let characters = ['\u{1000}', '\u{103C}', '\u{1031}'];
	let mut buffer = glyphs(&[10, 11, 12]);
	reorder_myanmar(&characters, &mut buffer).expect("reordering");
	assert_eq!(buffer.infos.iter().map(|info| info.glyph).collect::<std::vec::Vec<_>>(), std::vec![11, 12, 10]);
	// A consonant with neither is left where it is.
	let characters = ['\u{1000}', '\u{102C}'];
	let mut buffer = glyphs(&[10, 13]);
	reorder_myanmar(&characters, &mut buffer).expect("reordering");
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
	let out = shape_run(&face, &mut buffer, &characters, *b"dflt", false).expect("shaping");
	assert_eq!(out.len(), 3, "no composition happens in Arabic");
	assert_eq!(buffer.infos.iter().map(|info| info.glyph).collect::<std::vec::Vec<_>>(), std::vec![1, 1, 4]);
	// A Hangul run: the characters THEMSELVES change, which is why the call answers them.
	let characters = ['\u{1112}', '\u{1161}', '\u{11AB}'];
	let mut buffer = glyphs(&[1, 2, 3]);
	let out = shape_run(&face, &mut buffer, &characters, *b"dflt", false).expect("shaping");
	assert_eq!(out, std::vec!['\u{D55C}']);
	assert_eq!(buffer.len(), 1);
	// A Devanagari run: the buffer is reordered before any lookup sees it.
	let characters = ['\u{0915}', '\u{093F}'];
	let mut buffer = glyphs(&[1, 2]);
	shape_run(&face, &mut buffer, &characters, *b"dflt", false).expect("shaping");
	assert_eq!(buffer.infos.iter().map(|info| info.glyph).collect::<std::vec::Vec<_>>(), std::vec![2, 1]);
	// And a Latin run is untouched by any of it, which is the case that proves nothing on its own.
	let characters = ['a', 'b'];
	let mut buffer = glyphs(&[1, 2]);
	shape_run(&face, &mut buffer, &characters, *b"dflt", false).expect("shaping");
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
	assert_eq!(crate::shape::shape_run(&face, &mut buffer, &characters, *b"dflt", false).err(), Some(font_parse::Error::Unsupported(font_parse::Unsupported::Exceeded { limit: "run input", ceiling: opentype_profile::limits::RUN_INPUT, asked: ceiling as u64 + 1 })));
	// AND EXACTLY AT THE CEILING IT IS NOT REFUSED, which is what makes the number a ceiling.
	let characters: std::vec::Vec<char> = core::iter::repeat_n('a', ceiling).collect();
	let mut buffer = Buffer::new();
	for (index, _) in characters.iter().enumerate() {
		buffer.push(1, index as u32).expect("room for a glyph");
	}
	assert!(crate::shape::shape_run(&face, &mut buffer, &characters, *b"dflt", false).is_ok());
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
	// AND EXACTLY AT THE CEILING THE RUN IS SHAPED, which is what makes the published number the
	// number rather than one short of it.
	let features: std::vec::Vec<([u8; 4], u32)> = features.into_iter().take(ceiling).collect();
	let mut buffer = glyphs(&[1, 2]);
	assert!(crate::shape::shape_with_masks(&face, &mut buffer, *b"latn", *b"dflt", &features).is_ok());
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
					buffer.push(index as u16 + 1, index as u32).expect("room for a glyph");
				}
				let _ = crate::shape::shape_run(&face, &mut buffer, &['a', 'b', 'c'], *b"dflt", false);
				// AND THROUGH THE INDIC PLAN, which asks the face's `GSUB` about pairs of glyphs
				// before any lookup runs - another walk of the same offsets.
				let mut buffer = glyphs(&[1, 2, 3]);
				let _ = crate::shape::shape_run(&face, &mut buffer, &['\u{0930}', '\u{094D}', '\u{0915}'], *b"dflt", false);
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

#[test]
// A ONE-TO-MANY SUBSTITUTION APPLIED REPEATEDLY TURNS A WORD INTO A PAGE, which is what the expansion
// rule caps - and it caps the MULTIPLIER rather than the product, which is why the absolute input
// ceiling exists under it. Both are checked here at the exact bound and one past it.
fn a_run_expanded_past_the_frozen_ratio_is_refused_by_name() {
	let ceiling = opentype_profile::limits::OUTPUT_EXPANSION as usize;
	let shaped = |into: &[u16]| {
		let bytes = build::sample_many(Some(build::gsub_multiple(1, into)), None, None, into.len() as u16 + 2);
		let face = Face::open(&bytes, 0).expect("a font this tree built");
		let mut buffer = glyphs(&[1]);
		crate::shape::shape(&face, &mut buffer, *b"latn", *b"dflt", &[*b"liga"]).map(|()| buffer.len())
	};
	// EXACTLY THE RATIO IS DRAWN: one glyph becoming sixty-four is sixty-four times its input, which
	// is the number - and a rule that refused at its own value would be a different number than the
	// one published.
	let into: std::vec::Vec<u16> = (0..ceiling as u16).map(|index| index + 2).collect();
	assert_eq!(shaped(&into).expect("a run exactly at the ratio"), ceiling);

	// One more is refused by name, with the ratio it reached.
	let into: std::vec::Vec<u16> = (0..ceiling as u16 + 1).map(|index| index + 2).collect();
	assert_eq!(shaped(&into).err(), Some(font_parse::Error::Unsupported(font_parse::Unsupported::Exceeded { limit: "output expansion", ceiling: opentype_profile::limits::OUTPUT_EXPANSION, asked: ceiling as u64 + 1 })));
}

#[test]
// A CONTEXTUAL RULE INVOKES ANOTHER LOOKUP, WHICH MAY INVOKE IT AGAIN, and a font arranging that
// sixty-four deep is arranging for a shaper to recurse until the stack ends. At the ceiling the chain
// runs and one past it is refused by name - and the refusal matters as much as the bound: declining
// to apply the lookup silently would leave the run shaped as though the font had not asked.
fn a_contextual_chain_runs_to_the_frozen_depth_and_no_further() {
	let ceiling = opentype_profile::limits::CONTEXT_DEPTH as usize;
	let chain = |links: usize| {
		// Lookups 0..links-1 are contextual and each invokes the next; the last is a single
		// substitution, so the chain has something to do when it arrives.
		let mut lookups: std::vec::Vec<std::vec::Vec<u8>> = std::vec::Vec::new();
		for index in 0..links {
			lookups.push(build::gsub_context(1, index as u16 + 1));
		}
		lookups.push(build::single_lookup(1, 2));
		let gsub = build::layout_many(b"liga", &lookups);
		let bytes = build::sample_many(Some(gsub), None, None, 4);
		let face = Face::open(&bytes, 0).expect("a font this tree built");
		let mut buffer = glyphs(&[1]);
		crate::shape::shape(&face, &mut buffer, *b"latn", *b"dflt", &[*b"liga"]).map(|()| buffer.infos[0].glyph)
	};

	// A chain of `CONTEXT_DEPTH` contextual lookups reaches its substitution at exactly the ceiling.
	assert_eq!(chain(ceiling).expect("a chain exactly as deep as the ceiling"), 2, "the chain must actually reach its substitution, or this fixture measures nothing");

	// One more link is refused by name.
	assert_eq!(chain(ceiling + 1).err(), Some(font_parse::Error::Unsupported(font_parse::Unsupported::Exceeded { limit: "context depth", ceiling: opentype_profile::limits::CONTEXT_DEPTH, asked: opentype_profile::limits::CONTEXT_DEPTH as u64 + 1 })));
}

/// Shape a string the way a caller does: every character mapped through the face's `cmap`, every
/// glyph's cluster the byte offset of the character it came from.
fn shaped_buffer(face: &Face<'_>, text: &str, language: [u8; 4]) -> Buffer {
	let characters: std::vec::Vec<char> = text.chars().collect();
	let mut buffer = Buffer::new();
	for (offset, character) in text.char_indices() {
		let glyph = face.glyph_for(character).expect("a readable cmap").expect("a character the fixture face maps");
		buffer.push(glyph, offset as u32).expect("room for a glyph");
	}
	shape_run(face, &mut buffer, &characters, language, false).expect("shaping");
	buffer
}

fn glyph_ids(buffer: &Buffer) -> std::vec::Vec<u16> {
	buffer.infos.iter().map(|info| info.glyph).collect()
}

#[test]
// `locl` IS WHAT A LANGUAGE CHANGES. The profile's languages are defined by it - Turkish dotless i,
// Romanian comma-below, the Serbian italic forms - and a shaper that selected the language system and
// never applied `locl` rendered every one of them as the default language.
fn the_local_forms_of_a_language_apply_under_that_language_only() {
	let gsub = build::layout_features(&[(*b"locl", build::single_lookup(1, 4))], &[], &[(*b"TRK ", &[0])]);
	let bytes = build::sample(Some(gsub), None, None);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut buffer = glyphs(&[1]);
	shape_run(&face, &mut buffer, &['i'], *b"TRK ", false).expect("shaping");
	assert_eq!(glyph_ids(&buffer), std::vec![4], "Turkish selects its local form");
	let mut buffer = glyphs(&[1]);
	shape_run(&face, &mut buffer, &['i'], *b"dflt", false).expect("shaping");
	assert_eq!(glyph_ids(&buffer), std::vec![1], "and the default language does not");
}

#[test]
// A SCRIPT OUTSIDE THE PROFILE IS REFUSED, NOT SHAPED. Adlam and Mandaic join and the profile names
// neither: shaping them with the cursive engine under `DFLT` was a silent best effort, with rules
// nobody undertook to get right for them, and text in any other unnamed script went the same way.
fn text_in_a_script_the_profile_does_not_name_is_refused() {
	use crate::scripts::{Shaper, profile_script, shaper_for};
	let bytes = build::sample(None, None, None);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	for (text, name) in [(&['\u{1E900}', '\u{1E901}'][..], "Adlam"), (&['\u{0840}'][..], "Mandaic"), (&['\u{13A0}'][..], "Cherokee"), (&['a', ' ', '\u{1E900}'][..], "Adlam")] {
		let mut buffer = glyphs(&std::vec![1; text.len()]);
		assert_eq!(shape_run(&face, &mut buffer, text, *b"dflt", false).err(), Some(font_parse::Error::Unsupported(font_parse::Unsupported::TextScript(name))), "{name} text was shaped");
	}
	assert_eq!(shaper_for(&['\u{1E900}']), Shaper::Default, "no engine is chosen for a script the profile does not name");
	// TEXT THAT DECIDES NO SCRIPT IS NEVER REFUSED - digits, a mark with nothing before it, a
	// private-use icon - and every script the profile does name is shaped: Bopomofo too, which the
	// shaper had no tag for.
	for text in [&['1', '2'][..], &['\u{0301}'][..], &['\u{E000}'][..], &['\u{3105}'][..], &['a', '1'][..]] {
		let mut buffer = glyphs(&std::vec![1; text.len()]);
		assert!(shape_run(&face, &mut buffer, text, *b"dflt", false).is_ok(), "{text:?} was refused");
	}
	assert_eq!(profile_script(&['\u{3105}']), Ok(*b"bopo"));
	assert_eq!(profile_script(&[' ', '\u{E000}']), Ok(*b"DFLT"), "nothing to decide by");
}

#[test]
// KERNING IS NOT A LATIN FEATURE. An Arabic face kerns its joined forms the way any face kerns its
// letters, and the cursive shaper's set never asked for `kern`.
fn a_cursive_run_is_kerned() {
	let gpos = build::layout_features(&[(*b"kern", build::kern_lookup(1, 2, -50))], &[0], &[]);
	let bytes = build::sample(None, Some(gpos), None);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut buffer = glyphs(&[1, 2]);
	shape_run(&face, &mut buffer, &['\u{0628}', '\u{0628}'], *b"dflt", false).expect("shaping");
	assert_eq!(buffer.positions[0].x_advance, 300 - 50, "300 from hmtx, less the pair the face declares");
}

#[test]
// `rtlm` REACHES THE GLYPHS THE CALLER MARKED AND NO OTHERS. A right-to-left glyph the caller drew
// from the mirror character already faces the right way, and the face's own mirrored form applied
// to it as well would turn it back - so the caller marks the ones it did not mirror, and the joining
// pass must keep that mark rather than overwrite it with the joining forms.
fn the_faces_mirrored_forms_apply_where_the_caller_marked_them() {
	let gsub = build::layout_features(&[(*b"rtlm", build::single_lookup(1, 4))], &[0], &[]);
	let bytes = build::sample(Some(gsub), None, None);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut buffer = glyphs(&[1, 1]);
	buffer.infos[0].mask |= crate::buffer::RTLM;
	shape_run(&face, &mut buffer, &['\u{0628}', '\u{0628}'], *b"dflt", false).expect("shaping");
	assert_eq!(glyph_ids(&buffer), std::vec![4, 1]);
}

/// The glyphs of the Indic fixture face.
mod indic_face {
	pub const KA: u16 = 1;
	pub const RA: u16 = 3;
	pub const VIRAMA: u16 = 4;
	pub const AA: u16 = 5;
	pub const REPH: u16 = 6;
	pub const HALF_KA: u16 = 7;
	pub const RA_BELOW: u16 = 8;
	pub const ANUSVARA: u16 = 9;
	pub const ZWJ: u16 = 10;
	pub const SIGN_I: u16 = 11;
	pub const GLYPHS: u16 = 12;

	/// Devanagari, Bengali and Telugu share the fixture's glyphs: what is under test is where each
	/// script's shaping puts them, not what they look like.
	pub const CHARACTERS: [(char, u16); 15] = [
		('\u{0915}', KA),
		('\u{0930}', RA),
		('\u{094D}', VIRAMA),
		('\u{093E}', AA),
		('\u{093F}', SIGN_I),
		('\u{0902}', ANUSVARA),
		('\u{0995}', KA),
		('\u{09B0}', RA),
		('\u{09CD}', VIRAMA),
		('\u{09BE}', AA),
		('\u{0C15}', KA),
		('\u{0C30}', RA),
		('\u{0C4D}', VIRAMA),
		('\u{200D}', ZWJ),
		('\u{0020}', 2),
	];
}

/// A face for the Indic fixtures: `rphf` forms the reph from Ra and virama, `half` the half form of
/// ka - and, when `below` is set, `blwf` a below-base form of Ra.
fn indic_face(below: bool, gpos: Option<std::vec::Vec<u8>>, gdef: Option<std::vec::Vec<u8>>) -> std::vec::Vec<u8> {
	use indic_face::*;
	let mut features = std::vec![(*b"rphf", build::ligature_lookup(RA, VIRAMA, REPH)), (*b"half", build::ligature_lookup(KA, VIRAMA, HALF_KA))];
	if below {
		features.push((*b"blwf", build::ligature_lookup(VIRAMA, RA, RA_BELOW)));
	}
	let all: std::vec::Vec<u16> = (0..features.len() as u16).collect();
	let gsub = build::layout_features(&features, &all, &[]);
	build::mapped(&CHARACTERS, Some(gsub), gpos, gdef, GLYPHS)
}

#[test]
// A POSITIONAL FEATURE APPLIED TO THE WHOLE RUN IS NOT POSITIONAL. `half` is the form of a consonant
// BEFORE its syllable's base and `rphf` is a syllable-initial Ra with a base after it. Applied
// everywhere, the dead consonant that ends a word became a half form and a word-final `र्` a reph
// floating over nothing.
fn the_indic_forms_apply_only_where_their_syllable_puts_them() {
	use indic_face::*;
	let bytes = indic_face(false, None, None);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	// क् ending a word: the consonant is its own base, so it keeps its virama and takes no half form.
	assert_eq!(glyph_ids(&shaped_buffer(&face, "\u{0915}\u{094D}", *b"dflt")), std::vec![KA, VIRAMA]);
	// कर्: the र् ends the word with no base after it - a dead Ra, not a reph.
	assert_eq!(glyph_ids(&shaped_buffer(&face, "\u{0915}\u{0930}\u{094D}", *b"dflt")), std::vec![KA, RA, VIRAMA]);
	// AND WHERE THEY DO BELONG THEY STILL FORM: a half form before the base,
	assert_eq!(glyph_ids(&shaped_buffer(&face, "\u{0915}\u{094D}\u{0930}", *b"dflt")), std::vec![HALF_KA, RA]);
	// and one asked for explicitly, with a ZWJ after the virama at the end of a word.
	assert_eq!(glyph_ids(&shaped_buffer(&face, "\u{0915}\u{094D}\u{200D}", *b"dflt")), std::vec![HALF_KA, ZWJ]);
}

#[test]
// THE REPH IS WRITTEN FIRST AND DRAWN LAST. Once `rphf` has formed it, the final reordering moves it
// to where its script draws it, and its cluster goes with it - the cluster follows the glyph, as it
// does for a pre-base vowel sign.
fn the_reph_is_moved_to_where_its_script_draws_it() {
	use indic_face::*;
	let bytes = indic_face(false, None, None);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let pairs = |text: &str| shaped_buffer(&face, text, *b"dflt").infos.iter().map(|info| (info.glyph, info.cluster)).collect::<std::vec::Vec<_>>();
	// र्क: after its base, still carrying the Ra's cluster.
	assert_eq!(pairs("\u{0930}\u{094D}\u{0915}"), std::vec![(KA, 6), (REPH, 0)]);
	// DEVANAGARI DRAWS IT AT THE END OF THE SYLLABLE: after a right-side vowel sign, before the
	// anusvara.
	assert_eq!(glyph_ids(&shaped_buffer(&face, "\u{0930}\u{094D}\u{0915}\u{093E}\u{0902}", *b"dflt")), std::vec![KA, AA, REPH, ANUSVARA]);
	// A PRE-BASE VOWEL SIGN GOES IN FRONT OF THE BASE AND BEHIND THE REPH STILL TO FORM, or it would
	// split the two characters the reph is made of: र्कि.
	assert_eq!(glyph_ids(&shaped_buffer(&face, "\u{0930}\u{094D}\u{0915}\u{093F}", *b"dflt")), std::vec![SIGN_I, KA, REPH]);
	// BENGALI DRAWS IT AFTER THE BASE AND BEFORE A RIGHT-SIDE VOWEL SIGN.
	assert_eq!(glyph_ids(&shaped_buffer(&face, "\u{09B0}\u{09CD}\u{0995}\u{09BE}", *b"dflt")), std::vec![KA, REPH, AA]);
	// TELUGU WRITES IT EXPLICITLY, as Ra, virama, ZWJ - and there Ra, virama before a consonant is not
	// a reph at all.
	assert_eq!(glyph_ids(&shaped_buffer(&face, "\u{0C30}\u{0C4D}\u{200D}\u{0C15}", *b"dflt")), std::vec![ZWJ, KA, REPH]);
	assert_eq!(glyph_ids(&shaped_buffer(&face, "\u{0C30}\u{0C4D}\u{0C15}", *b"dflt")), std::vec![RA, VIRAMA, KA]);
}

#[test]
// THE BASE IS THE FACE'S QUESTION AS MUCH AS THE TEXT'S. In क्र the र is either a consonant of its
// own or a form drawn below the क; with a face whose `blwf` has that form the क is the base - so it
// takes no half form, and the र is drawn below it.
fn a_consonant_the_face_draws_below_the_base_does_not_become_the_base() {
	use indic_face::*;
	let bytes = indic_face(true, None, None);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	assert_eq!(glyph_ids(&shaped_buffer(&face, "\u{0915}\u{094D}\u{0930}", *b"dflt")), std::vec![KA, RA_BELOW]);
}

#[test]
// THE INDIC MARK FEATURES. A face written for these scripts places its reph, its below-base forms
// and its vowel signs through `abvm` and `blwm` rather than through `mark`, and a shaper that never
// asked for them left the reph at the pen.
fn the_reph_is_placed_by_the_above_base_mark_feature() {
	use indic_face::*;
	let gpos = build::layout_features(&[(*b"abvm", build::mark_lookup(KA, REPH, (300, 700), (50, 0)))], &[0], &[]);
	let bytes = indic_face(false, Some(gpos), Some(build::gdef(&[REPH])));
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let buffer = shaped_buffer(&face, "\u{0930}\u{094D}\u{0915}", *b"dflt");
	assert_eq!(glyph_ids(&buffer), std::vec![KA, REPH]);
	// The base's anchor, less the mark's, less the advance the pen made over the base: 300 - 50 - 203.
	assert_eq!((buffer.positions[1].x_offset, buffer.positions[1].y_offset), (47, 700));
	assert_eq!(buffer.infos[1].attached_to, Some(0));
}

#[test]
// A FACE THAT MULTIPLIES EVERY GLYPH IS REFUSED AS IT GROWS, not after: the run's output ceiling is
// held where a decomposition grows the buffer, or the memory for the whole expansion would have been
// handed over before the count was ever looked at.
fn a_decomposition_past_the_frozen_output_ceiling_is_refused_while_it_grows() {
	let ceiling = opentype_profile::limits::RUN_OUTPUT as usize;
	let mut buffer = Buffer::new();
	for index in 0..ceiling - 1 {
		buffer.push(1, index as u32).expect("room for a glyph");
	}
	assert!(buffer.decompose(0, &[1, 2]).is_ok(), "growing to exactly the ceiling is a decomposition");
	assert_eq!(buffer.len(), ceiling);
	assert_eq!(buffer.decompose(0, &[1, 2]).err(), Some(font_parse::Error::Unsupported(font_parse::Unsupported::Exceeded { limit: "run output", ceiling: opentype_profile::limits::RUN_OUTPUT, asked: ceiling as u64 + 1 })));
	assert_eq!(buffer.len(), ceiling, "and the refused one changed nothing");
}

/// AN ALLOCATION LIMIT FOR ONE THREAD: every allocation on the thread that set it and is larger than
/// it fails, and every other thread is untouched - which is how a fixture reaches the refusal a real
/// Domain reaches when it runs out.
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

#[test]
// STORAGE A RUN CANNOT HAVE IS A TYPED REFUSAL, NOT AN EXIT. The shaper copies its input, builds its
// syllable and joining tables and grows its buffer, all in proportion to the run; a Domain that
// cannot give it that gets `Allocation` back, and the process that asked is still running to read it.
fn storage_the_run_cannot_have_is_refused_rather_than_fatal() {
	let bytes = build::sample(None, None, None);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let characters: std::vec::Vec<char> = core::iter::repeat_n('a', 2000).collect();
	let mut buffer = glyphs(&std::vec![1; characters.len()]);
	let refused = limited::within(1024, || crate::shape::shape_run(&face, &mut buffer, &characters, *b"dflt", false).err());
	assert_eq!(refused, Some(font_parse::Error::Allocation));
	// And the buffer refuses to grow past what it can have, rather than ending the process.
	let mut buffer = Buffer::new();
	let refused = limited::within(1024, || (0..2000u32).try_for_each(|index| buffer.push(1, index)).err());
	assert_eq!(refused, Some(font_parse::Error::Allocation));
}

// -------------------------------------------------------------------------------------------------
// THE LOOKUP FLAGS, THE STACKED MARKS AND THE CURSIVE JOINS. Every lookup above has flags 0, which is
// the case where every glyph after a match's first is simply the next buffer slot - and the case a
// shaper that ignores the flags passes.
// -------------------------------------------------------------------------------------------------

/// The glyphs of the fixtures below, by role.
mod roles {
	pub const LAM: u16 = 1;
	pub const ALEF: u16 = 2;
	pub const FATHA: u16 = 3;
	pub const LAM_ALEF: u16 = 4;
	pub const OTHER_MARK: u16 = 5;
	pub const LIGATURE: u16 = 6;
}

/// The one-feature-per-lookup table the fixtures below build their layout tables from.
fn features(entries: &[([u8; 4], std::vec::Vec<u8>)]) -> std::vec::Vec<u8> {
	let indices: std::vec::Vec<u16> = (0..entries.len() as u16).collect();
	build::layout_features(entries, &indices, &[])
}

/// A buffer of glyphs with the clusters given.
fn clustered(glyphs: &[(u16, u32)]) -> Buffer {
	let mut buffer = Buffer::new();
	for (glyph, cluster) in glyphs {
		buffer.push(*glyph, *cluster).expect("room for a glyph");
	}
	buffer
}

#[test]
// A LIGATURE THAT IGNORES MARKS FORMS ACROSS ONE, and the mark stays - on the component it followed. A
// lam, a fatha and an alef are a lam-alef and the fatha, and the fatha goes over the LAM half: the
// mark-to-ligature rule takes the component the substitution recorded rather than guessing it from the
// clusters' bytes. A mark that followed the whole ligature takes its last component.
fn a_ligature_ignoring_marks_forms_across_a_mark_and_keeps_it_on_its_component() {
	use crate::layout::IGNORE_MARKS;
	use roles::*;
	let gsub = features(&[(*b"rlig", build::flagged(4, IGNORE_MARKS, build::ligature_subtable(LAM, &[ALEF], LAM_ALEF), None))]);
	let gpos = features(&[(*b"mark", build::flagged(5, 0, build::mark_ligature_subtable(&[(FATHA, (50, 0))], LAM_ALEF, &[(100, 700), (450, 650)]), None))]);
	let gdef = build::gdef_full(&[(LAM, ALEF, 1), (FATHA, FATHA, 3), (LAM_ALEF, LAM_ALEF, 2), (OTHER_MARK, OTHER_MARK, 3)], &[], &[]);
	let bytes = build::with_advances(Some(gsub), Some(gpos), Some(gdef), &[500, 400, 300, 0, 600, 0, 700]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");

	let mut buffer = clustered(&[(LAM, 0), (FATHA, 2), (ALEF, 4)]);
	shape(&face, &mut buffer, *b"DFLT", *b"dflt", &[*b"rlig", *b"mark"]).expect("shaping");
	assert_eq!(glyph_ids(&buffer), std::vec![LAM_ALEF, FATHA], "the ligature formed across the mark, and kept it");
	assert_eq!(buffer.infos[1].cluster, 2, "the mark keeps its own cluster");
	assert!(buffer.infos[0].ligature != 0 && buffer.infos[1].ligature == buffer.infos[0].ligature, "the mark records the ligature it sat in");
	assert_eq!(buffer.infos[1].component, 1, "after the first component");
	assert_eq!(buffer.infos[1].attached_to, Some(0));
	// Over the lam half: its anchor, less the fatha's, less the ligature's advance the pen crossed.
	assert_eq!((buffer.positions[1].x_offset, buffer.positions[1].y_offset), (100 - 50 - 600, 700));

	let mut buffer = clustered(&[(LAM, 0), (ALEF, 2), (FATHA, 4)]);
	shape(&face, &mut buffer, *b"DFLT", *b"dflt", &[*b"rlig", *b"mark"]).expect("shaping");
	assert_eq!(glyph_ids(&buffer), std::vec![LAM_ALEF, FATHA]);
	assert_eq!(buffer.infos[1].component, 0, "a mark after the ligature sat in no part of it");
	assert_eq!((buffer.positions[1].x_offset, buffer.positions[1].y_offset), (450 - 50 - 600, 650), "and takes the last component");
}

#[test]
// A PAIR THAT IGNORES MARKS IS KERNED ACROSS ONE, and the mark still attaches to the first of the two -
// measured back over the KERNED advance, because the pen crosses the advance positioning left. The
// same pair without the flag sees the mark as the second glyph, and kerns nothing.
fn a_kerning_pair_ignoring_marks_is_kerned_across_a_mark() {
	use crate::layout::IGNORE_MARKS;
	let (a, v, acute) = (1u16, 2u16, 3u16);
	let gdef = build::gdef_full(&[(a, v, 1), (acute, acute, 3)], &[], &[]);
	for (flags, kerned) in [(IGNORE_MARKS, 700 - 80), (0, 700)] {
		let gpos = features(&[
			(*b"kern", build::flagged(2, flags, build::pair_subtable(a, v, -80), None)),
			(*b"mark", build::flagged(4, 0, build::mark_subtable(&[(acute, 0, (60, 0))], &[(a, std::vec![Some((350, 700))])], 1), None)),
		]);
		let bytes = build::with_advances(None, Some(gpos), Some(gdef.clone()), &[500, 700, 700, 0]);
		let face = Face::open(&bytes, 0).expect("a font this tree built");
		let mut buffer = glyphs(&[a, acute, v]);
		shape(&face, &mut buffer, *b"DFLT", *b"dflt", &[*b"kern", *b"mark"]).expect("shaping");
		assert_eq!(buffer.positions[0].x_advance, kerned, "flags {flags:#06x}");
		assert_eq!(buffer.infos[1].attached_to, Some(0));
		assert_eq!((buffer.positions[1].x_offset, buffer.positions[1].y_offset), (350 - 60 - kerned, 700), "flags {flags:#06x}");
	}
}

#[test]
// A CONTEXTUAL RULE THAT IGNORES MARKS MATCHES ACROSS THEM - in its input, its backtrack and its
// lookahead - and applies its nested lookup to the glyph its sequence index FOUND, not to the slot that
// many places after the start: here the second input glyph is two slots on, past a mark.
fn a_contextual_rule_ignoring_marks_matches_across_them_and_applies_where_it_matched() {
	use crate::layout::IGNORE_MARKS;
	let (first, second, mark, before, after, replaced) = (1u16, 2u16, 3u16, 4u16, 5u16, 6u16);
	let chain = build::flagged(6, IGNORE_MARKS, build::chain_subtable(&[&[before]], &[&[first], &[second]], &[&[after]], &[(1, 1)]), None);
	let gsub = build::layout_many(b"calt", &[chain, build::flagged(1, 0, build::single_subtable(second, replaced), None)]);
	let gdef = build::gdef_full(&[(first, second, 1), (mark, mark, 3), (before, replaced, 1)], &[], &[]);
	let bytes = build::with_advances(Some(gsub), None, Some(gdef), &[500, 300, 300, 0, 300, 300, 300]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut buffer = glyphs(&[before, mark, first, mark, second, mark, after]);
	shape(&face, &mut buffer, *b"DFLT", *b"dflt", &[*b"calt"]).expect("shaping");
	assert_eq!(glyph_ids(&buffer), std::vec![before, mark, first, mark, replaced, mark, after], "the rule matched across every mark and substituted the glyph it found");
}

#[test]
// THE TWO FLAGS THAT NAME MARKS. A lookup restricted to a mark glyph set sees only the marks in it, and
// one restricted to a mark attachment class sees only the marks of that class: every other mark is
// passed over as though it were not there - so the pair is kerned across it - while a mark it names is
// a glyph like any other, and a pair across that one is no pair at all.
fn a_mark_set_or_an_attachment_class_names_the_marks_a_lookup_sees() {
	use crate::layout::USE_MARK_FILTERING_SET;
	use roles::{FATHA, OTHER_MARK};
	let (a, v) = (1u16, 2u16);
	let gdef = build::gdef_full(&[(a, v, 1), (FATHA, FATHA, 3), (OTHER_MARK, OTHER_MARK, 3)], &[(FATHA, FATHA, 1), (OTHER_MARK, OTHER_MARK, 2)], &[&[FATHA]]);
	for (name, kern) in [
		("the mark set", build::flagged(2, USE_MARK_FILTERING_SET, build::pair_subtable(a, v, -80), Some(0))),
		("the attachment class", build::flagged(2, 0x0100, build::pair_subtable(a, v, -80), None)),
	] {
		let bytes = build::with_advances(None, Some(features(&[(*b"kern", kern)])), Some(gdef.clone()), &[500, 700, 700, 0, 0, 0]);
		let face = Face::open(&bytes, 0).expect("a font this tree built");
		let mut passed = glyphs(&[a, OTHER_MARK, v]);
		shape(&face, &mut passed, *b"DFLT", *b"dflt", &[*b"kern"]).expect("shaping");
		assert_eq!(passed.positions[0].x_advance, 620, "{name}: a mark it does not name is passed over");
		let mut seen = glyphs(&[a, FATHA, v]);
		shape(&face, &mut seen, *b"DFLT", *b"dflt", &[*b"kern"]).expect("shaping");
		assert_eq!(seen.positions[0].x_advance, 700, "{name}: a mark it names is a glyph of the match");
	}
}

#[test]
// AND THE TWO THAT IGNORE A CLASS OTHER THAN MARKS: a lookup ignoring base glyphs pairs two marks across
// the letter between them, and one ignoring ligatures pairs two letters across a ligature.
fn a_lookup_ignoring_bases_or_ligatures_matches_across_them() {
	use crate::layout::{IGNORE_BASE_GLYPHS, IGNORE_LIGATURES};
	use roles::{FATHA, LIGATURE, OTHER_MARK};
	let (a, v) = (1u16, 2u16);
	let gdef = build::gdef_full(&[(a, v, 1), (FATHA, OTHER_MARK, 3), (LIGATURE, LIGATURE, 2)], &[], &[]);
	let advances = [500, 700, 700, 0, 0, 0, 900];
	let bytes = build::with_advances(None, Some(features(&[(*b"kern", build::flagged(2, IGNORE_BASE_GLYPHS, build::pair_subtable(FATHA, OTHER_MARK, -30), None))])), Some(gdef.clone()), &advances);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut buffer = glyphs(&[FATHA, a, OTHER_MARK]);
	shape(&face, &mut buffer, *b"DFLT", *b"dflt", &[*b"kern"]).expect("shaping");
	assert_eq!(buffer.positions[0].x_advance, -30, "the two marks are a pair across the letter");
	let bytes = build::with_advances(None, Some(features(&[(*b"kern", build::flagged(2, IGNORE_LIGATURES, build::pair_subtable(a, v, -80), None))])), Some(gdef), &advances);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut buffer = glyphs(&[a, LIGATURE, v]);
	shape(&face, &mut buffer, *b"DFLT", *b"dflt", &[*b"kern"]).expect("shaping");
	assert_eq!(buffer.positions[0].x_advance, 620, "the two letters are a pair across the ligature");
}

#[test]
// A MARK ON A MARK IS PLACED WHERE ITS PARENT IS DRAWN. The second accent's anchors meet the first's,
// and the first was itself moved onto the letter - so the second carries that whole move: placed from
// the first accent's pen it lands a letter's width off, toward the next one.
fn a_mark_on_a_mark_is_placed_where_its_parent_is_drawn() {
	let (base, first, second) = (1u16, 2u16, 3u16);
	let gpos = features(&[
		(*b"mark", build::flagged(4, 0, build::mark_subtable(&[(first, 0, (40, 0))], &[(base, std::vec![Some((300, 600))])], 1), None)),
		(*b"mkmk", build::flagged(6, 0, build::mark_subtable(&[(second, 0, (30, 0))], &[(first, std::vec![Some((45, 120))])], 1), None)),
	]);
	let gdef = build::gdef_full(&[(base, base, 1), (first, second, 3)], &[], &[]);
	let bytes = build::with_advances(None, Some(gpos), Some(gdef), &[500, 500, 0, 0]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut buffer = glyphs(&[base, first, second]);
	shape(&face, &mut buffer, *b"DFLT", *b"dflt", &[*b"mark", *b"mkmk"]).expect("shaping");
	assert_eq!((buffer.positions[1].x_offset, buffer.positions[1].y_offset), (300 - 40 - 500, 600), "the first accent on the letter");
	assert_eq!(buffer.infos[2].attached_to, Some(1));
	assert_eq!((buffer.positions[2].x_offset, buffer.positions[2].y_offset), (45 - 30 + (300 - 40 - 500), 120 + 600), "the second on the first, where the first is drawn");
}

#[test]
// A MARK THAT FOLLOWS A LETTER DOES NOT HANG FROM THE LETTER BEFORE'S ACCENT. The mark-to-mark search
// looks at the one glyph before the mark, and that is a base: it ends there, rather than walking on
// past the letter to an accent the rule happens to cover.
fn a_mark_after_a_base_does_not_hang_from_the_previous_letters_mark() {
	let (base, other_base, first, second) = (1u16, 2u16, 3u16, 4u16);
	let gpos = features(&[
		(*b"mark", build::flagged(4, 0, build::mark_subtable(&[(first, 0, (40, 0))], &[(base, std::vec![Some((300, 600))])], 1), None)),
		(*b"mkmk", build::flagged(6, 0, build::mark_subtable(&[(second, 0, (30, 0))], &[(first, std::vec![Some((45, 120))])], 1), None)),
	]);
	let gdef = build::gdef_full(&[(base, other_base, 1), (first, second, 3)], &[], &[]);
	let bytes = build::with_advances(None, Some(gpos), Some(gdef), &[500, 500, 500, 0, 0]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut buffer = glyphs(&[base, first, other_base, second]);
	shape(&face, &mut buffer, *b"DFLT", *b"dflt", &[*b"mark", *b"mkmk"]).expect("shaping");
	assert_eq!(buffer.infos[3].attached_to, None, "the accent after the second letter hangs from nothing the face offers it");
	assert_eq!((buffer.positions[3].x_offset, buffer.positions[3].y_offset), (0, 0));
}

#[test]
// A BASE MOVED BY A PLACEMENT CARRIES ITS MARK. The accent's anchors meet the letter's where the letter
// is DRAWN, and a single adjustment that moves the letter moves the point the accent hangs from.
fn a_base_moved_by_a_placement_carries_its_mark() {
	let (base, mark) = (1u16, 2u16);
	let gpos = features(&[
		(*b"kern", build::flagged(1, 0, build::placement_subtable(base, (30, 10)), None)),
		(*b"mark", build::flagged(4, 0, build::mark_subtable(&[(mark, 0, (40, 0))], &[(base, std::vec![Some((300, 600))])], 1), None)),
	]);
	let gdef = build::gdef_full(&[(base, base, 1), (mark, mark, 3)], &[], &[]);
	let bytes = build::with_advances(None, Some(gpos), Some(gdef), &[500, 500, 0]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut buffer = glyphs(&[base, mark]);
	shape(&face, &mut buffer, *b"DFLT", *b"dflt", &[*b"kern", *b"mark"]).expect("shaping");
	assert_eq!((buffer.positions[0].x_offset, buffer.positions[0].y_offset), (30, 10));
	assert_eq!((buffer.positions[1].x_offset, buffer.positions[1].y_offset), (300 - 40 - 500 + 30, 600 + 10));
}

/// Where each glyph of a shaped run is DRAWN - its origin in visual order, from a pen starting at zero
/// and moving the way the run is read - by logical index.
fn drawn_origins(buffer: &Buffer) -> std::vec::Vec<(i32, i32)> {
	let mut origins = std::vec![(0, 0); buffer.len()];
	let mut pen = 0;
	let order: std::vec::Vec<usize> = if buffer.right_to_left { (0..buffer.len()).rev().collect() } else { (0..buffer.len()).collect() };
	for at in order {
		let position = buffer.positions[at];
		origins[at] = (pen + position.x_offset, position.y_offset);
		pen += position.x_advance;
	}
	origins
}

/// A cursive face of three glyphs and the run they make, shaped in the direction and with the flags
/// given: `(entry, exit)` per glyph, and the three advances.
fn joined(right_to_left: bool, flags: u16, anchors: &[(Option<(i16, i16)>, Option<(i16, i16)>); 3], advances: [u16; 3]) -> Buffer {
	let entries: std::vec::Vec<(u16, Option<(i16, i16)>, Option<(i16, i16)>)> = anchors.iter().enumerate().map(|(index, (entry, exit))| (index as u16 + 1, *entry, *exit)).collect();
	let gpos = features(&[(*b"curs", build::flagged(3, flags, build::cursive_subtable(&entries), None))]);
	let bytes = build::with_advances(None, Some(gpos), None, &[500, advances[0], advances[1], advances[2]]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut buffer = glyphs(&[1, 2, 3]);
	buffer.right_to_left = right_to_left;
	shape(&face, &mut buffer, *b"DFLT", *b"dflt", &[*b"curs"]).expect("shaping");
	buffer
}

/// Every join of a three-glyph run meets: the exit anchor of each glyph, where it is drawn, is the entry
/// anchor of the next in the text, where that one is drawn.
fn every_join_meets(buffer: &Buffer, anchors: &[(Option<(i16, i16)>, Option<(i16, i16)>); 3]) {
	let origins = drawn_origins(buffer);
	for pair in 0..2 {
		let exit = anchors[pair].1.expect("an exit");
		let entry = anchors[pair + 1].0.expect("an entry");
		let left = (origins[pair].0 + i32::from(exit.0), origins[pair].1 + i32::from(exit.1));
		let right = (origins[pair + 1].0 + i32::from(entry.0), origins[pair + 1].1 + i32::from(entry.1));
		assert_eq!(left, right, "the join after glyph {pair} does not meet: {:?}", buffer.positions);
	}
}

#[test]
// A RIGHT-TO-LEFT CURSIVE WORD JOINS, LETTER TO LETTER, WITH ITS LAST LETTER ON THE BASELINE. Drawn right
// to left, a letter starts at its exit and the one after it in the text ends at its entry - which is
// the arithmetic the other direction has the other way round - and under the `rightToLeft` flag each
// letter hangs from the one after it, so the middle letter's lift is carried to the first.
fn a_right_to_left_cursive_word_joins_letter_to_letter() {
	use crate::layout::RIGHT_TO_LEFT;
	let anchors = [(None, Some((40, 120))), (Some((430, 60)), Some((30, 160))), (Some((560, 40)), None)];
	let buffer = joined(true, RIGHT_TO_LEFT, &anchors, [520, 460, 600]);
	let placed: std::vec::Vec<(i32, i32, i32)> = buffer.positions.iter().map(|position| (position.x_advance, position.x_offset, position.y_offset)).collect();
	assert_eq!(placed, std::vec![(480, -40, -180), (400, -30, -120), (560, 0, 0)]);
	every_join_meets(&buffer, &anchors);
	// WITHOUT THE FLAG THE FIRST LETTER STAYS ON THE BASELINE and each later one hangs from the one before.
	let buffer = joined(true, 0, &anchors, [520, 460, 600]);
	assert_eq!(buffer.positions.iter().map(|position| position.y_offset).collect::<std::vec::Vec<_>>(), std::vec![0, 60, 180]);
	every_join_meets(&buffer, &anchors);
}

#[test]
// AND A LEFT-TO-RIGHT ONE CARRIES EACH LETTER'S SHIFT TO THE NEXT. The second letter is moved back to
// meet the first and its advance shortened by the same amount, so the third follows from where the
// second really ends - and the lift across the line accumulates along the chain.
fn a_left_to_right_cursive_word_carries_each_shift_to_the_next() {
	let anchors = [(None, Some((500, 120))), (Some((20, 60)), Some((440, 160))), (Some((30, 40)), None)];
	let buffer = joined(false, 0, &anchors, [520, 460, 600]);
	let placed: std::vec::Vec<(i32, i32, i32)> = buffer.positions.iter().map(|position| (position.x_advance, position.x_offset, position.y_offset)).collect();
	assert_eq!(placed, std::vec![(500, 0, 0), (420, -20, 60), (570, -30, 180)]);
	every_join_meets(&buffer, &anchors);
}
