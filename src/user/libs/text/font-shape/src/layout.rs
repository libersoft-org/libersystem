//! What `GSUB` and `GPOS` share: the script list, the feature list, the lookup list, and the two
//! structures every lookup is indexed through.
//!
//! THE SHAPE IS THE SAME FOR BOTH TABLES down to the lookup, and different for every lookup below
//! it. Sharing this half is not a tidiness choice: a font's `GPOS` selects its features through the
//! very same script and language machinery as its `GSUB`, and two copies of that machinery is two
//! chances to select the wrong language system - which renders Turkish with a dotted i, or Serbian
//! with the Russian italic forms, and looks like a font bug.

use font_parse::{Error, Malformed, Reader};
use opentype_profile::{Unsupported, layout as profile};

/// A `GSUB` or `GPOS` table, opened and checked.
#[derive(Clone, Copy)]
pub struct LayoutTable<'a> {
	table: Reader<'a>,
	scripts: usize,
	features: usize,
	lookups: usize,
	tag: [u8; 4],
}

/// One feature: its tag, and the lookups it turns on.
#[derive(Clone, Copy)]
pub struct Feature<'a> {
	pub tag: [u8; 4],
	indices: Reader<'a>,
	count: usize,
}

/// One lookup: its type, its flags, and its subtables.
#[derive(Clone, Copy)]
pub struct Lookup<'a> {
	pub kind: u16,
	pub flags: u16,
	/// The `GDEF` mark filtering set, when the flags name one.
	pub mark_filtering_set: Option<u16>,
	table: Reader<'a>,
	subtables: usize,
	offset: usize,
}

impl<'a> LayoutTable<'a> {
	/// Open a layout table. The version is checked against the profile before anything in it is read.
	pub fn new(table: Reader<'a>, tag: [u8; 4]) -> Result<Self, Error> {
		let bad = move || Error::Malformed(Malformed::InconsistentTable { table: tag });
		let mut reader = table;
		let major = reader.u16().ok_or_else(bad)?;
		let _minor = reader.u16().ok_or_else(bad)?;
		if !opentype_profile::tables::admits_version(&tag, major) {
			return Err(Error::Unsupported(Unsupported::TableVersion { tag, major, minor: 0 }));
		}
		let scripts = reader.u16().ok_or_else(bad)? as usize;
		let features = reader.u16().ok_or_else(bad)? as usize;
		let lookups = reader.u16().ok_or_else(bad)? as usize;
		// Each of the three is an offset from the table's start, and each must land inside it before
		// anything reads through it.
		for offset in [scripts, features, lookups] {
			if offset >= table.len() {
				return Err(bad());
			}
		}
		Ok(Self { table, scripts, features, lookups, tag })
	}

	fn bad(&self) -> Error {
		Error::Malformed(Malformed::InconsistentTable { table: self.tag })
	}

	/// The feature indices a script and language select.
	///
	/// THE FALLBACK CHAIN IS THE WHOLE OF LANGUAGE SUPPORT, and it is three deep: the language system
	/// the caller asked for, the script's default system, and then the `DFLT` script's. A shaper that
	/// stopped at the first miss would render a Turkish font with no Turkish forms and report
	/// nothing.
	pub fn features_for(&self, script: [u8; 4], language: [u8; 4]) -> Result<FeatureIndices<'a>, Error> {
		let list = self.table.slice(self.scripts, self.table.len() - self.scripts).ok_or_else(|| self.bad())?;
		let script_offset = self.find_script(list, script)?.or(self.find_script(list, *b"DFLT")?).or(self.find_script(list, *b"dflt")?);
		let Some(script_offset) = script_offset else {
			return Ok(FeatureIndices { table: None, count: 0, required: None });
		};
		let script_table = list.slice(script_offset, list.len().checked_sub(script_offset).ok_or_else(|| self.bad())?).ok_or_else(|| self.bad())?;
		let mut reader = script_table;
		let default_offset = reader.u16().ok_or_else(|| self.bad())? as usize;
		let language_count = reader.u16().ok_or_else(|| self.bad())? as usize;
		let mut chosen = None;
		for index in 0..language_count {
			let at = 4usize.checked_add(index.checked_mul(6).ok_or_else(|| self.bad())?).ok_or_else(|| self.bad())?;
			let mut record = script_table;
			record.seek(at).ok_or_else(|| self.bad())?;
			let tag = record.tag().ok_or_else(|| self.bad())?;
			let offset = record.u16().ok_or_else(|| self.bad())? as usize;
			if tag == language {
				chosen = Some(offset);
				break;
			}
		}
		// The script's own default is what an unnamed language falls back to, which is what the
		// format means by a `LangSys` offset of zero.
		let chosen = chosen.or(if default_offset == 0 { None } else { Some(default_offset) });
		let Some(offset) = chosen else {
			return Ok(FeatureIndices { table: None, count: 0, required: None });
		};
		let lang_sys = script_table.slice(offset, script_table.len().checked_sub(offset).ok_or_else(|| self.bad())?).ok_or_else(|| self.bad())?;
		let mut reader = lang_sys;
		let _lookup_order = reader.u16().ok_or_else(|| self.bad())?;
		let required = reader.u16().ok_or_else(|| self.bad())?;
		let count = reader.u16().ok_or_else(|| self.bad())? as usize;
		let indices = lang_sys.slice(6, count.checked_mul(2).ok_or_else(|| self.bad())?).ok_or_else(|| self.bad())?;
		Ok(FeatureIndices { table: Some(indices), count, required: if required == 0xFFFF { None } else { Some(required) } })
	}

	fn find_script(&self, list: Reader<'a>, wanted: [u8; 4]) -> Result<Option<usize>, Error> {
		let mut reader = list;
		let count = reader.u16().ok_or_else(|| self.bad())? as usize;
		for index in 0..count {
			let at = 2usize.checked_add(index.checked_mul(6).ok_or_else(|| self.bad())?).ok_or_else(|| self.bad())?;
			let mut record = list;
			record.seek(at).ok_or_else(|| self.bad())?;
			let tag = record.tag().ok_or_else(|| self.bad())?;
			let offset = record.u16().ok_or_else(|| self.bad())? as usize;
			if tag == wanted {
				return Ok(Some(offset));
			}
		}
		Ok(None)
	}

	/// The feature at an index of the feature list.
	pub fn feature(&self, index: u16) -> Result<Feature<'a>, Error> {
		let list = self.table.slice(self.features, self.table.len() - self.features).ok_or_else(|| self.bad())?;
		let mut reader = list;
		let count = reader.u16().ok_or_else(|| self.bad())? as usize;
		if index as usize >= count {
			return Err(self.bad());
		}
		let at = 2usize.checked_add((index as usize).checked_mul(6).ok_or_else(|| self.bad())?).ok_or_else(|| self.bad())?;
		let mut record = list;
		record.seek(at).ok_or_else(|| self.bad())?;
		let tag = record.tag().ok_or_else(|| self.bad())?;
		let offset = record.u16().ok_or_else(|| self.bad())? as usize;
		let feature = list.slice(offset, list.len().checked_sub(offset).ok_or_else(|| self.bad())?).ok_or_else(|| self.bad())?;
		let mut reader = feature;
		let _parameters = reader.u16().ok_or_else(|| self.bad())?;
		let lookup_count = reader.u16().ok_or_else(|| self.bad())? as usize;
		let indices = feature.slice(4, lookup_count.checked_mul(2).ok_or_else(|| self.bad())?).ok_or_else(|| self.bad())?;
		Ok(Feature { tag, indices, count: lookup_count })
	}

	/// The lookup at an index of the lookup list.
	pub fn lookup(&self, index: u16) -> Result<Lookup<'a>, Error> {
		let list = self.table.slice(self.lookups, self.table.len() - self.lookups).ok_or_else(|| self.bad())?;
		let mut reader = list;
		let count = reader.u16().ok_or_else(|| self.bad())? as usize;
		if index as usize >= count {
			return Err(self.bad());
		}
		let at = 2usize.checked_add((index as usize).checked_mul(2).ok_or_else(|| self.bad())?).ok_or_else(|| self.bad())?;
		let offset = list.u16_at(at / 2).ok_or_else(|| self.bad())? as usize;
		let table = list.slice(offset, list.len().checked_sub(offset).ok_or_else(|| self.bad())?).ok_or_else(|| self.bad())?;
		let mut reader = table;
		let kind = reader.u16().ok_or_else(|| self.bad())?;
		let flags = reader.u16().ok_or_else(|| self.bad())?;
		let subtables = reader.u16().ok_or_else(|| self.bad())? as usize;
		// THE LOOKUP TYPE IS CHECKED AGAINST THE PROFILE HERE, once, rather than at each place a
		// subtable is read. A type the profile does not have is a refusal naming it - never a lookup
		// quietly skipped, which would render text the font asked to change and this did not.
		let admitted = match self.tag {
			tag if tag == *b"GSUB" => profile::gsub_lookup(kind).is_some(),
			_ => profile::gpos_lookup(kind).is_some(),
		};
		if !admitted {
			return Err(Error::Unsupported(if self.tag == *b"GSUB" { Unsupported::GsubLookup(kind) } else { Unsupported::GposLookup(kind) }));
		}
		let mark_filtering_set = if flags & 0x0010 != 0 {
			let at = 6usize.checked_add(subtables.checked_mul(2).ok_or_else(|| self.bad())?).ok_or_else(|| self.bad())?;
			let mut reader = table;
			reader.seek(at).ok_or_else(|| self.bad())?;
			Some(reader.u16().ok_or_else(|| self.bad())?)
		} else {
			None
		};
		Ok(Lookup { kind, flags, mark_filtering_set, table, subtables, offset: 6 })
	}
}

/// The feature indices one language system selects.
#[derive(Clone, Copy)]
pub struct FeatureIndices<'a> {
	table: Option<Reader<'a>>,
	count: usize,
	/// The feature the language system REQUIRES, which is applied whether or not anybody asked for
	/// its tag - that is what "required" means and a shaper that treated it as optional would drop
	/// the one feature a script cannot be rendered without.
	pub required: Option<u16>,
}

impl FeatureIndices<'_> {
	pub fn len(&self) -> usize {
		self.count
	}

	pub fn is_empty(&self) -> bool {
		self.count == 0
	}

	pub fn get(&self, index: usize) -> Option<u16> {
		self.table?.u16_at(index)
	}
}

impl<'a> Feature<'a> {
	pub fn len(&self) -> usize {
		self.count
	}

	pub fn is_empty(&self) -> bool {
		self.count == 0
	}

	/// The index of one of this feature's lookups, in the table's lookup list.
	pub fn lookup_index(&self, index: usize) -> Option<u16> {
		if index >= self.count {
			return None;
		}
		self.indices.u16_at(index)
	}
}

impl<'a> Lookup<'a> {
	pub fn subtable_count(&self) -> usize {
		self.subtables
	}

	/// One subtable of this lookup, resolved through the lookup's own bounds.
	pub fn subtable(&self, index: usize) -> Option<Reader<'a>> {
		if index >= self.subtables {
			return None;
		}
		let at = self.offset.checked_add(index.checked_mul(2)?)?;
		let offset = self.table.u16_at(at / 2)? as usize;
		self.table.slice(offset, self.table.len().checked_sub(offset)?)
	}

	/// Whether this lookup skips a glyph, from its flags and the glyph's `GDEF` class.
	///
	/// THE FLAGS ARE NOT DECORATION. A lookup that says it ignores marks and is applied to them
	/// anyway attaches an accent to an accent; one that says it ignores ligatures and is not
	/// re-kerns inside a word that has none.
	pub fn skips(&self, class: u16) -> bool {
		let base = self.flags & 0x0002 != 0 && class == 1;
		let ligature = self.flags & 0x0004 != 0 && class == 2;
		let mark = self.flags & 0x0008 != 0 && class == 3;
		base || ligature || mark
	}
}

/// A `Coverage` table: which glyphs a lookup applies to, and at which index.
///
/// THE INDEX IS THE POINT, not the membership: almost every subtable stores its data in an array
/// parallel to its coverage, so "is this glyph covered" and "which entry is it" are one lookup.
pub fn coverage_index(table: Reader<'_>, glyph: u16) -> Result<Option<u16>, Error> {
	let bad = || Error::Malformed(Malformed::InconsistentTable { table: *b"GSUB" });
	let mut reader = table;
	let format = reader.u16().ok_or_else(bad)?;
	match format {
		1 => {
			let count = reader.u16().ok_or_else(bad)? as usize;
			let glyphs = table.slice(4, count.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
			// The list is sorted, which is what makes this a search rather than a scan.
			let mut low = 0usize;
			let mut high = count;
			while low < high {
				let middle = low + (high - low) / 2;
				let candidate = glyphs.u16_at(middle).ok_or_else(bad)?;
				if glyph < candidate {
					high = middle;
				} else if glyph > candidate {
					low = middle + 1;
				} else {
					return Ok(Some(middle as u16));
				}
			}
			Ok(None)
		}
		2 => {
			let count = reader.u16().ok_or_else(bad)? as usize;
			for index in 0..count {
				let at = 4usize.checked_add(index.checked_mul(6).ok_or_else(bad)?).ok_or_else(bad)?;
				let mut range = table;
				range.seek(at).ok_or_else(bad)?;
				let first = range.u16().ok_or_else(bad)?;
				let last = range.u16().ok_or_else(bad)?;
				let start_index = range.u16().ok_or_else(bad)?;
				if glyph < first {
					return Ok(None);
				}
				if glyph <= last {
					return Ok(Some(start_index.wrapping_add(glyph - first)));
				}
			}
			Ok(None)
		}
		other => Err(Error::Unsupported(Unsupported::SubtableFormat { table: *b"GSUB", format: other })),
	}
}

/// A `ClassDef` table: which class a glyph is in. Class 0 is every glyph the table does not mention,
/// which is the format's own default and not an error.
pub fn class_of(table: Reader<'_>, glyph: u16) -> Result<u16, Error> {
	let bad = || Error::Malformed(Malformed::InconsistentTable { table: *b"GDEF" });
	let mut reader = table;
	let format = reader.u16().ok_or_else(bad)?;
	match format {
		1 => {
			let first = reader.u16().ok_or_else(bad)?;
			let count = reader.u16().ok_or_else(bad)? as usize;
			if glyph < first {
				return Ok(0);
			}
			let index = (glyph - first) as usize;
			if index >= count {
				return Ok(0);
			}
			let classes = table.slice(6, count.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
			Ok(classes.u16_at(index).ok_or_else(bad)?)
		}
		2 => {
			let count = reader.u16().ok_or_else(bad)? as usize;
			for index in 0..count {
				let at = 4usize.checked_add(index.checked_mul(6).ok_or_else(bad)?).ok_or_else(bad)?;
				let mut range = table;
				range.seek(at).ok_or_else(bad)?;
				let first = range.u16().ok_or_else(bad)?;
				let last = range.u16().ok_or_else(bad)?;
				let class = range.u16().ok_or_else(bad)?;
				if glyph < first {
					return Ok(0);
				}
				if glyph <= last {
					return Ok(class);
				}
			}
			Ok(0)
		}
		other => Err(Error::Unsupported(Unsupported::SubtableFormat { table: *b"GDEF", format: other })),
	}
}
