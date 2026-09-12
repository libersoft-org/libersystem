//! The table directory, and the tables a face cannot be measured without.
//!
//! EVERY OFFSET IS CHECKED AGAINST THE FILE BEFORE THE TABLE IS OPENED, and every table's own
//! internal counts are checked against ITS length rather than against the file's. The difference
//! matters: a `hmtx` whose count says 60000 in a file that is large enough is a table that reads
//! sixty thousand entries out of whatever follows it, and only the table's own length says no.

use opentype_profile::{Unsupported, tables as profile};

use crate::reader::Reader;
use crate::{Error, Malformed, SIGNATURE_COLLECTION, SIGNATURE_OPENTYPE, SIGNATURE_TRUETYPE};

/// How many tables a face may declare. The format's count is a `u16`; this is the statement that a
/// directory is read only as far as the file actually goes.
const MAX_TABLES: usize = u16::MAX as usize;

/// One face of a font file, opened and checked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Face<'a> {
	bytes: Reader<'a>,
	/// Where the table directory for THIS face starts - a collection has one per face.
	directory: usize,
	table_count: usize,
	pub header: FaceHeader,
	pub metrics: Metrics,
}

/// What `head` says about the face.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FaceHeader {
	/// The design grid every outline coordinate is in.
	pub units_per_em: u16,
	/// `head.indexToLocFormat`: whether `loca` is short or long.
	pub long_loca: bool,
	pub x_min: i16,
	pub y_min: i16,
	pub x_max: i16,
	pub y_max: i16,
}

/// What `hhea`, `maxp` and `hmtx` say between them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Metrics {
	pub glyph_count: u16,
	pub ascender: i16,
	pub descender: i16,
	pub line_gap: i16,
	/// How many entries `hmtx` carries before its run of side bearings.
	pub horizontal_metrics: u16,
}

impl<'a> Face<'a> {
	/// Open one face of a font file.
	///
	/// `index` selects the face within a collection, and is 0 for a single-face file. A non-zero
	/// index on a single-face file is a REFUSAL rather than a silent 0: a caller asking for face 3
	/// has a declaration that says something this file does not.
	pub fn open(bytes: &'a [u8], index: u32) -> Result<Self, Error> {
		let reader = Reader::new(bytes);
		// THE FILE'S OWN SIZE, BEFORE ANYTHING IN IT IS READ. A face larger than any real one is a
		// document trying to exhaust memory before it is parsed, and the ceiling is the profile's
		// rather than this parser's: a limit each reader chose for itself is a limit nobody froze.
		if reader.len() as u64 > opentype_profile::limits::FONT_BYTES as u64 {
			return Err(Error::Unsupported(Unsupported::Exceeded { limit: "font bytes", ceiling: opentype_profile::limits::FONT_BYTES, asked: reader.len() as u64 }));
		}
		let mut header = reader;
		let signature = header.u32().ok_or(Error::Malformed(Malformed::NotAFont))?;
		let directory = match signature {
			SIGNATURE_TRUETYPE | SIGNATURE_OPENTYPE => {
				if index != 0 {
					return Err(Error::Malformed(Malformed::NoSuchFace { index, faces: 1 }));
				}
				0
			}
			SIGNATURE_COLLECTION => {
				// A collection: a version, a face count, then one directory offset per face.
				let _version = header.u32().ok_or(Error::Malformed(Malformed::Truncated { table: *b"ttcf", wanted: 8 }))?;
				let faces = header.u32().ok_or(Error::Malformed(Malformed::Truncated { table: *b"ttcf", wanted: 12 }))?;
				if index >= faces {
					return Err(Error::Malformed(Malformed::NoSuchFace { index, faces }));
				}
				let at = header.position();
				let offsets = reader.slice(at, reader.len().checked_sub(at).unwrap_or(0)).ok_or(Error::Malformed(Malformed::NotAFont))?;
				let offset = offsets.u32_at(index as usize).ok_or(Error::Malformed(Malformed::Truncated { table: *b"ttcf", wanted: at + 4 }))?;
				offset as usize
			}
			_ => return Err(Error::Malformed(Malformed::NotAFont)),
		};

		// The face's own directory: a signature, a table count, and three fields nothing reads.
		let mut face = reader;
		face.seek(directory).ok_or(Error::Malformed(Malformed::TableOutOfBounds { table: *b"head" }))?;
		let signature = face.u32().ok_or(Error::Malformed(Malformed::NotAFont))?;
		if !matches!(signature, SIGNATURE_TRUETYPE | SIGNATURE_OPENTYPE) {
			return Err(Error::Malformed(Malformed::NotAFont));
		}
		let table_count = face.u16().ok_or(Error::Malformed(Malformed::NotAFont))? as usize;
		if table_count > MAX_TABLES {
			return Err(Error::Malformed(Malformed::InconsistentTable { table: *b"head" }));
		}
		// The whole directory must be inside the file before any entry of it is read.
		let entries_at = directory.checked_add(12).ok_or(Error::Malformed(Malformed::NotAFont))?;
		let entries_len = table_count.checked_mul(16).ok_or(Error::Malformed(Malformed::NotAFont))?;
		reader.slice(entries_at, entries_len).ok_or(Error::Malformed(Malformed::Truncated { table: *b"head", wanted: entries_at.saturating_add(entries_len) }))?;

		let mut opened = Self { bytes: reader, directory, table_count, header: FaceHeader { units_per_em: 0, long_loca: false, x_min: 0, y_min: 0, x_max: 0, y_max: 0 }, metrics: Metrics { glyph_count: 0, ascender: 0, descender: 0, line_gap: 0, horizontal_metrics: 0 } };
		opened.header = opened.read_head()?;
		opened.metrics = opened.read_metrics()?;
		Ok(opened)
	}

	/// The bytes of a table, checked against the file and against the profile.
	///
	/// A TABLE THE PROFILE DOES NOT ADMIT IS `Unsupported` HERE, at the one place a table is opened -
	/// rather than at each of the places one is read, where the check would be missing from exactly
	/// the table nobody thought about.
	pub fn table(&self, tag: &[u8; 4]) -> Result<Option<Reader<'a>>, Error> {
		if profile::table_of(tag).is_none() {
			return Err(Error::Unsupported(Unsupported::Table(*tag)));
		}
		self.table_unchecked(tag)
	}

	/// The same, without the profile check - for the caller that is ASKING whether a table the
	/// profile excludes is present, which is a different question from wanting to read it.
	pub fn table_unchecked(&self, tag: &[u8; 4]) -> Result<Option<Reader<'a>>, Error> {
		let entries_at = self.directory.checked_add(12).ok_or(Error::Malformed(Malformed::NotAFont))?;
		for index in 0..self.table_count {
			let entry_at = entries_at.checked_add(index.checked_mul(16).ok_or(Error::Malformed(Malformed::NotAFont))?).ok_or(Error::Malformed(Malformed::NotAFont))?;
			let mut entry = self.bytes;
			entry.seek(entry_at).ok_or(Error::Malformed(Malformed::TableOutOfBounds { table: *tag }))?;
			let found = entry.tag().ok_or(Error::Malformed(Malformed::TableOutOfBounds { table: *tag }))?;
			let _checksum = entry.u32().ok_or(Error::Malformed(Malformed::TableOutOfBounds { table: *tag }))?;
			let offset = entry.u32().ok_or(Error::Malformed(Malformed::TableOutOfBounds { table: *tag }))? as usize;
			let length = entry.u32().ok_or(Error::Malformed(Malformed::TableOutOfBounds { table: *tag }))? as usize;
			if found != *tag {
				continue;
			}
			// AND THE TABLE'S OWN CEILING. One table claiming most of a file is a length nobody drew,
			// and it is refused by name rather than read: a reader that accepted it would do the work
			// the ceiling exists to refuse before discovering the table was nonsense.
			if length as u64 > opentype_profile::limits::TABLE_BYTES as u64 {
				return Err(Error::Unsupported(Unsupported::Exceeded { limit: "table bytes", ceiling: opentype_profile::limits::TABLE_BYTES, asked: length as u64 }));
			}
			// THE TABLE'S OWN RANGE, checked before anything inside it is read. A length that
			// overflows the file is the commonest thing a crafted font does.
			return self.bytes.slice(offset, length).map(Some).ok_or(Error::Malformed(Malformed::TableOutOfBounds { table: *tag }));
		}
		Ok(None)
	}

	/// A table that must be there.
	fn required(&self, tag: &[u8; 4]) -> Result<Reader<'a>, Error> {
		self.table(tag)?.ok_or(Error::Malformed(Malformed::MissingTable { table: *tag }))
	}

	fn read_head(&self) -> Result<FaceHeader, Error> {
		let table = self.required(b"head")?;
		let mut reader = table;
		let major = reader.u16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"head", wanted: 2 }))?;
		let _minor = reader.u16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"head", wanted: 4 }))?;
		if !profile::admits_version(b"head", major) {
			return Err(Error::Unsupported(Unsupported::TableVersion { tag: *b"head", major, minor: 0 }));
		}
		// The revision, the checksum adjustment, the magic number and the flags: fourteen bytes
		// between the version and the design grid, and getting that count wrong reads the FLAGS as
		// the grid - which is a zero, and therefore a face every scaled metric would divide by.
		reader.skip(14).ok_or(Error::Malformed(Malformed::Truncated { table: *b"head", wanted: 18 }))?;
		let units_per_em = reader.u16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"head", wanted: 20 }))?;
		// A design grid of zero is a face whose every scaled metric would be a division by zero.
		if units_per_em == 0 {
			return Err(Error::Malformed(Malformed::InconsistentTable { table: *b"head" }));
		}
		reader.skip(16).ok_or(Error::Malformed(Malformed::Truncated { table: *b"head", wanted: 36 }))?;
		let x_min = reader.i16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"head", wanted: 38 }))?;
		let y_min = reader.i16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"head", wanted: 40 }))?;
		let x_max = reader.i16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"head", wanted: 42 }))?;
		let y_max = reader.i16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"head", wanted: 44 }))?;
		reader.skip(6).ok_or(Error::Malformed(Malformed::Truncated { table: *b"head", wanted: 50 }))?;
		let index_to_loc = reader.i16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"head", wanted: 52 }))?;
		let long_loca = match index_to_loc {
			0 => false,
			1 => true,
			// The field has two values and a font that states a third is one this parser would be
			// guessing about the size of every `loca` entry in.
			_ => return Err(Error::Malformed(Malformed::InconsistentTable { table: *b"head" })),
		};
		Ok(FaceHeader { units_per_em, long_loca, x_min, y_min, x_max, y_max })
	}

	fn read_metrics(&self) -> Result<Metrics, Error> {
		let maxp = self.required(b"maxp")?;
		let mut reader = maxp;
		let version = reader.u32().ok_or(Error::Malformed(Malformed::Truncated { table: *b"maxp", wanted: 4 }))?;
		// 0.5 for a `CFF` face and 1.0 for a `glyf` one; both carry the glyph count next.
		if !matches!(version, 0x0000_5000 | 0x0001_0000) {
			return Err(Error::Unsupported(Unsupported::TableVersion { tag: *b"maxp", major: (version >> 16) as u16, minor: version as u16 }));
		}
		let glyph_count = reader.u16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"maxp", wanted: 6 }))?;

		let hhea = self.required(b"hhea")?;
		let mut reader = hhea;
		let major = reader.u16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"hhea", wanted: 2 }))?;
		let _minor = reader.u16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"hhea", wanted: 4 }))?;
		if !profile::admits_version(b"hhea", major) {
			return Err(Error::Unsupported(Unsupported::TableVersion { tag: *b"hhea", major, minor: 0 }));
		}
		let ascender = reader.i16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"hhea", wanted: 6 }))?;
		let descender = reader.i16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"hhea", wanted: 8 }))?;
		let line_gap = reader.i16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"hhea", wanted: 10 }))?;
		// TWENTY-FOUR BYTES between the line gap and the metric count: the advance and side-bearing
		// extremes, the caret slope and offset, four reserved fields and the metric data format.
		// Twenty-two - which is the count if the metric data format is forgotten - reads the format
		// field as the count, and a font whose format is 0 then declares no horizontal metrics at
		// all. Found by a fixture that patched the real offset and got a refusal.
		reader.skip(24).ok_or(Error::Malformed(Malformed::Truncated { table: *b"hhea", wanted: 34 }))?;
		let horizontal_metrics = reader.u16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"hhea", wanted: 36 }))?;
		// `hhea` says how many entries `hmtx` has, and a count above the glyph count is a table that
		// would be read past the glyphs it is about.
		if horizontal_metrics == 0 || horizontal_metrics > glyph_count {
			return Err(Error::Malformed(Malformed::InconsistentTable { table: *b"hhea" }));
		}
		Ok(Metrics { glyph_count, ascender, descender, line_gap, horizontal_metrics })
	}

	/// The advance width and left side bearing of a glyph, in font units.
	///
	/// THE LAST ENTRY REPEATS, which is the format's own compression: a font whose glyphs mostly
	/// share an advance stores it once, and the side bearings that follow are one per remaining
	/// glyph. Reading that wrong gives every glyph past the run the same side bearing, which looks
	/// like a spacing bug rather than a parser one.
	pub fn advance(&self, glyph: u16) -> Result<(u16, i16), Error> {
		if glyph >= self.metrics.glyph_count {
			return Err(Error::Malformed(Malformed::BadGlyph { glyph }));
		}
		let hmtx = self.required(b"hmtx")?;
		let entries = self.metrics.horizontal_metrics as usize;
		let index = (glyph as usize).min(entries - 1);
		let at = index.checked_mul(4).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"hmtx" }))?;
		let mut reader = hmtx;
		reader.seek(at).ok_or(Error::Malformed(Malformed::Truncated { table: *b"hmtx", wanted: at }))?;
		let advance = reader.u16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"hmtx", wanted: at + 2 }))?;
		let bearing = if (glyph as usize) < entries {
			reader.i16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"hmtx", wanted: at + 4 }))?
		} else {
			// Past the run of full entries, the side bearings are a short array of their own.
			let tail = entries.checked_mul(4).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"hmtx" }))?;
			let extra = (glyph as usize - entries).checked_mul(2).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"hmtx" }))?;
			let at = tail.checked_add(extra).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"hmtx" }))?;
			let mut reader = hmtx;
			reader.seek(at).ok_or(Error::Malformed(Malformed::Truncated { table: *b"hmtx", wanted: at }))?;
			reader.i16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"hmtx", wanted: at + 2 }))?
		};
		Ok((advance, bearing))
	}

	/// Where a glyph's outline is in `glyf`, as `(offset, length)`. An empty glyph - a space - has a
	/// length of zero, which is not an error and must not be read as one.
	pub fn glyph_range(&self, glyph: u16) -> Result<(usize, usize), Error> {
		if glyph >= self.metrics.glyph_count {
			return Err(Error::Malformed(Malformed::BadGlyph { glyph }));
		}
		let loca = self.required(b"loca")?;
		let (start, end) = if self.header.long_loca {
			let start = loca.u32_at(glyph as usize).ok_or(Error::Malformed(Malformed::Truncated { table: *b"loca", wanted: 0 }))? as usize;
			let end = loca.u32_at(glyph as usize + 1).ok_or(Error::Malformed(Malformed::Truncated { table: *b"loca", wanted: 0 }))? as usize;
			(start, end)
		} else {
			// The short form stores half the offset, which is why a short `loca` font's tables are
			// padded to even lengths.
			let start = loca.u16_at(glyph as usize).ok_or(Error::Malformed(Malformed::Truncated { table: *b"loca", wanted: 0 }))? as usize * 2;
			let end = loca.u16_at(glyph as usize + 1).ok_or(Error::Malformed(Malformed::Truncated { table: *b"loca", wanted: 0 }))? as usize * 2;
			(start, end)
		};
		// A descending pair is a font that contradicts itself, and subtracting it would wrap.
		if end < start {
			return Err(Error::Malformed(Malformed::InconsistentTable { table: *b"loca" }));
		}
		Ok((start, end - start))
	}

	/// The `glyf` table, for the caller that walks outlines.
	pub fn outlines(&self) -> Result<Reader<'a>, Error> {
		self.required(b"glyf")
	}

	/// The glyph a character maps to, through `cmap`. `None` when the font has no glyph for it,
	/// which is an ordinary answer rather than an error.
	pub fn glyph_for(&self, character: char) -> Result<Option<u16>, Error> {
		let cmap = self.required(b"cmap")?;
		let mut reader = cmap;
		let _version = reader.u16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"cmap", wanted: 2 }))?;
		let count = reader.u16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"cmap", wanted: 4 }))? as usize;
		let mut best: Option<usize> = None;
		let mut best_rank = 0u8;
		for index in 0..count {
			let at = 4usize.checked_add(index.checked_mul(8).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
			let mut record = cmap;
			record.seek(at).ok_or(Error::Malformed(Malformed::Truncated { table: *b"cmap", wanted: at }))?;
			let platform = record.u16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"cmap", wanted: at + 2 }))?;
			let encoding = record.u16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"cmap", wanted: at + 4 }))?;
			let offset = record.u32().ok_or(Error::Malformed(Malformed::Truncated { table: *b"cmap", wanted: at + 8 }))? as usize;
			// THE SUBTABLE THIS SYSTEM PREFERS, in the order that gets the most characters: full
			// Unicode first, then the basic plane. A font's first subtable is not its best one.
			let rank = match (platform, encoding) {
				(3, 10) | (0, 4) | (0, 6) => 3,
				(3, 1) | (0, 3) => 2,
				(0, 0) | (0, 1) | (0, 2) => 1,
				_ => 0,
			};
			if rank > best_rank {
				best_rank = rank;
				best = Some(offset);
			}
		}
		let Some(offset) = best else { return Ok(None) };
		let subtable = cmap.slice(offset, cmap.len().checked_sub(offset).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
		lookup_cmap(subtable, character as u32)
	}
}

/// One `cmap` subtable, in the formats the profile admits.
fn lookup_cmap(table: Reader<'_>, code_point: u32) -> Result<Option<u16>, Error> {
	let mut reader = table;
	let format = reader.u16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"cmap", wanted: 2 }))?;
	match format {
		4 => lookup_format4(table, code_point),
		6 => lookup_format6(table, code_point),
		12 => lookup_format12(table, code_point),
		// Format 14 is variation sequences, which are asked for through a different call: a lookup
		// by code point alone has no sequence to resolve.
		14 => Ok(None),
		other => Err(Error::Unsupported(Unsupported::SubtableFormat { table: *b"cmap", format: other })),
	}
}

/// Format 4: segment mapping, the basic-plane workhorse.
fn lookup_format4(table: Reader<'_>, code_point: u32) -> Result<Option<u16>, Error> {
	if code_point > 0xFFFF {
		return Ok(None);
	}
	let code_point = code_point as u16;
	let mut reader = table;
	reader.skip(6).ok_or(Error::Malformed(Malformed::Truncated { table: *b"cmap", wanted: 6 }))?;
	let segment_bytes = reader.u16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"cmap", wanted: 8 }))? as usize;
	if segment_bytes == 0 || segment_bytes % 2 != 0 {
		return Err(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }));
	}
	let segments = segment_bytes / 2;
	// The four parallel arrays, each `segments` long, at fixed offsets after the header.
	let ends = table.slice(14, segment_bytes).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
	let starts_at = 16usize.checked_add(segment_bytes).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
	let starts = table.slice(starts_at, segment_bytes).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
	let deltas_at = starts_at.checked_add(segment_bytes).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
	let deltas = table.slice(deltas_at, segment_bytes).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
	let ranges_at = deltas_at.checked_add(segment_bytes).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
	let ranges = table.slice(ranges_at, segment_bytes).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;

	for segment in 0..segments {
		let end = ends.u16_at(segment).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
		if code_point > end {
			continue;
		}
		let start = starts.u16_at(segment).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
		if code_point < start {
			return Ok(None);
		}
		let delta = deltas.u16_at(segment).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
		let range_offset = ranges.u16_at(segment).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
		if range_offset == 0 {
			let glyph = code_point.wrapping_add(delta);
			return Ok(if glyph == 0 { None } else { Some(glyph) });
		}
		// THE ONE PLACE THE FORMAT ADDRESSES ITSELF BY POINTER ARITHMETIC: the range offset is a
		// byte offset from the array entry ITSELF into the glyph array that follows. It is the
		// commonest place a font is crafted to read out of bounds, and it is checked here like
		// everything else.
		let entry_at = ranges_at.checked_add(segment.checked_mul(2).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
		let index = (code_point - start) as usize;
		let at = entry_at.checked_add(range_offset as usize).and_then(|at| at.checked_add(index.checked_mul(2)?)).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
		let mut glyphs = table;
		glyphs.seek(at).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
		let glyph = glyphs.u16().ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
		if glyph == 0 {
			return Ok(None);
		}
		return Ok(Some(glyph.wrapping_add(delta)));
	}
	Ok(None)
}

/// Format 6: a single trimmed range.
fn lookup_format6(table: Reader<'_>, code_point: u32) -> Result<Option<u16>, Error> {
	let mut reader = table;
	reader.skip(6).ok_or(Error::Malformed(Malformed::Truncated { table: *b"cmap", wanted: 6 }))?;
	let first = reader.u16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"cmap", wanted: 8 }))? as u32;
	let count = reader.u16().ok_or(Error::Malformed(Malformed::Truncated { table: *b"cmap", wanted: 10 }))? as u32;
	if code_point < first || code_point >= first + count {
		return Ok(None);
	}
	let index = (code_point - first) as usize;
	let glyphs = table.slice(10, (count as usize).checked_mul(2).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
	let glyph = glyphs.u16_at(index).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
	Ok(if glyph == 0 { None } else { Some(glyph) })
}

/// Format 12: segmented coverage, which is how anything above the basic plane is reached.
fn lookup_format12(table: Reader<'_>, code_point: u32) -> Result<Option<u16>, Error> {
	let mut reader = table;
	reader.skip(12).ok_or(Error::Malformed(Malformed::Truncated { table: *b"cmap", wanted: 12 }))?;
	let groups = reader.u32().ok_or(Error::Malformed(Malformed::Truncated { table: *b"cmap", wanted: 16 }))? as usize;
	let bytes = groups.checked_mul(12).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
	let table_of_groups = table.slice(16, bytes).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
	for index in 0..groups {
		let at = index.checked_mul(12).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
		let mut group = table_of_groups;
		group.seek(at).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
		let first = group.u32().ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
		let last = group.u32().ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
		let glyph = group.u32().ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
		if last < first {
			return Err(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }));
		}
		if code_point < first {
			return Ok(None);
		}
		if code_point <= last {
			let glyph = glyph.checked_add(code_point - first).ok_or(Error::Malformed(Malformed::InconsistentTable { table: *b"cmap" }))?;
			return Ok(u16::try_from(glyph).ok().filter(|glyph| *glyph != 0));
		}
	}
	Ok(None)
}
