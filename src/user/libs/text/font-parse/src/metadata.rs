//! `name`, `OS/2` and `post`: what a face SAYS it is.
//!
//! THIS IS WHAT THE CATALOGUE'S TRUTH ORACLE IS MEASURED AGAINST. A staged face arrives with a
//! DECLARED record - family, style, axes, face index, format - and the declaration is trusted by
//! review until something can check it. Checking it means recovering the same fields from the file
//! itself, which is what this reads.
//!
//! THE STYLE IS A NUMBER AND A SET OF BITS, NOT A WORD. "Bold Italic" is a string a designer chose,
//! in a language they chose it in, and matching on it is how a face called `Fett` or `Gras` stops
//! being bold. `OS/2` states the weight as a number, the width as a number and the slope as a bit,
//! and those are what a style is compared on.
//!
//! A NAME TABLE HAS MANY RECORDS FOR ONE NAME, and picking "the first one" makes the answer depend
//! on the order a font tool happened to write them in - two files with identical names could then
//! disagree about their own family. The preference below is STATED and total.
//!
//! NO ALLOCATION, WHICH IS WHY A NAME IS A DECODER RATHER THAN A STRING. The stored form is UTF-16
//! big-endian or Mac Roman, neither of which is `str`, and a parser that returned an owned `String`
//! would allocate once per name on a path that must not allocate at all. The caller supplies the
//! buffer, and a name too long for it is a refusal rather than a truncated family name - two
//! families whose first thirty characters agree are not the same family.

use crate::reader::Reader;
use crate::tables::Face;
use crate::{Error, Malformed};

fn bad(table: &[u8; 4]) -> Error {
	Error::Malformed(Malformed::InconsistentTable { table: *table })
}

/// The name identifiers this system asks for, by the numbers the format gives them.
pub mod name_id {
	/// The family a user picks from a list.
	pub const FAMILY: u16 = 1;
	/// The style within that family.
	pub const SUBFAMILY: u16 = 2;
	/// The identifier that is unique across faces.
	pub const UNIQUE: u16 = 3;
	/// The full name - family and style together, as the designer wrote it.
	pub const FULL: u16 = 4;
	/// The PostScript name, which is ASCII and has its own character rules.
	pub const POSTSCRIPT: u16 = 6;
	/// The TYPOGRAPHIC family, which is what a family with more than four styles actually is.
	pub const TYPOGRAPHIC_FAMILY: u16 = 16;
	/// The typographic style within it.
	pub const TYPOGRAPHIC_SUBFAMILY: u16 = 17;
}

/// How a name record's bytes are encoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Encoding {
	/// UTF-16 big-endian, which is what every Windows record and every modern Mac record is.
	Utf16,
	/// Mac Roman, whose upper half is NOT Latin-1 - reading it as Latin-1 turns every accented
	/// letter in an older face's name into a different letter.
	MacRoman,
}

/// The `name` table, found once per face.
#[derive(Clone, Copy)]
pub struct Names<'a> {
	table: Reader<'a>,
	count: usize,
	storage_at: usize,
}

/// One name, as bytes that have not been decoded yet.
#[derive(Clone, Copy)]
pub struct Name<'a> {
	bytes: &'a [u8],
	encoding: Encoding,
}

impl<'a> Names<'a> {
	/// Read the table, or answer `None` for a face that carries no names - which is unusual but is
	/// not a malformed font.
	pub fn of(face: &Face<'a>) -> Result<Option<Self>, Error> {
		let Some(table) = face.table(b"name")? else { return Ok(None) };
		let mut reader = table;
		let format = reader.u16().ok_or_else(|| bad(b"name"))?;
		// FORMAT 1 ADDS LANGUAGE TAGS AND CHANGES NOTHING ELSE. Its extra array sits after the name
		// records, so the records are read the same way; a format this profile does not know is
		// refused by name rather than read as the one it resembles.
		if format > 1 {
			return Err(Error::Unsupported(opentype_profile::Unsupported::SubtableFormat { table: *b"name", format }));
		}
		let count = reader.u16().ok_or_else(|| bad(b"name"))? as usize;
		let storage_at = reader.u16().ok_or_else(|| bad(b"name"))? as usize;
		Ok(Some(Self { table, count, storage_at }))
	}

	/// One name, chosen by the stated preference.
	///
	/// THE PREFERENCE IS TOTAL AND IT IS WRITTEN DOWN: Windows English first, then Windows in any
	/// language, then Macintosh English, then anything at all. A face that carries the same name in
	/// eleven languages answers the same way on every machine, which "the first record" does not.
	pub fn get(&self, wanted: u16) -> Result<Option<Name<'a>>, Error> {
		let mut best: Option<(u8, Name<'a>)> = None;
		for index in 0..self.count {
			let at = 6usize.checked_add(index.checked_mul(12).ok_or_else(|| bad(b"name"))?).ok_or_else(|| bad(b"name"))?;
			let mut record = self.table;
			record.seek(at).ok_or_else(|| bad(b"name"))?;
			let platform = record.u16().ok_or_else(|| bad(b"name"))?;
			let encoding = record.u16().ok_or_else(|| bad(b"name"))?;
			let language = record.u16().ok_or_else(|| bad(b"name"))?;
			let name = record.u16().ok_or_else(|| bad(b"name"))?;
			let length = record.u16().ok_or_else(|| bad(b"name"))? as usize;
			let offset = record.u16().ok_or_else(|| bad(b"name"))? as usize;
			if name != wanted {
				continue;
			}
			let Some(rank) = rank_of(platform, language) else { continue };
			let Some(encoding) = encoding_of(platform, encoding) else { continue };
			let start = self.storage_at.checked_add(offset).ok_or_else(|| bad(b"name"))?;
			let bytes = self.table.bytes(start, length).ok_or_else(|| bad(b"name"))?;
			// LOWER RANK WINS, and a tie keeps the EARLIER record so that a font with two identical
			// records answers the same way whichever order they were written in.
			if best.as_ref().is_none_or(|(current, _)| rank < *current) {
				best = Some((rank, Name { bytes, encoding }));
			}
		}
		Ok(best.map(|(_, name)| name))
	}
}

/// How much this system prefers a record, lower being better. `None` for a platform it does not read.
fn rank_of(platform: u16, language: u16) -> Option<u8> {
	match platform {
		// Windows, which is where a modern face's names really are.
		3 => Some(if language == 0x0409 { 0 } else { 1 }),
		// Macintosh, whose language 0 is English.
		1 => Some(if language == 0 { 2 } else { 3 }),
		// Unicode, which carries no language at all.
		0 => Some(4),
		_ => None,
	}
}

/// How a record's bytes are encoded, from the platform and encoding it declares.
fn encoding_of(platform: u16, encoding: u16) -> Option<Encoding> {
	match (platform, encoding) {
		// Windows: symbol and BMP and full repertoire are all UTF-16 in the name table.
		(3, _) => Some(Encoding::Utf16),
		// Unicode platform: always UTF-16.
		(0, _) => Some(Encoding::Utf16),
		// Macintosh Roman, and nothing else from that platform: the other Mac scripts are legacy
		// encodings this system does not carry tables for, and guessing at one produces a family
		// name that is wrong rather than missing.
		(1, 0) => Some(Encoding::MacRoman),
		_ => None,
	}
}

impl Name<'_> {
	/// Decode into a buffer the caller owns, answering how many characters were written.
	///
	/// A NAME TOO LONG FOR THE BUFFER IS A REFUSAL. Truncating a family name is how two different
	/// families become one, and the caller cannot tell that it happened.
	pub fn copy_into(&self, into: &mut [char]) -> Result<usize, Error> {
		let mut written = 0usize;
		let push = |character: char, into: &mut [char], written: &mut usize| -> Result<(), Error> {
			match into.get_mut(*written) {
				Some(slot) => {
					*slot = character;
					*written += 1;
					Ok(())
				}
				None => Err(bad(b"name")),
			}
		};
		match self.encoding {
			Encoding::MacRoman => {
				for byte in self.bytes {
					push(mac_roman(*byte), into, &mut written)?;
				}
			}
			Encoding::Utf16 => {
				// SURROGATE PAIRS ARE PAIRS. A decoder that took every unit as a character turns an
				// emoji or a rarer CJK ideograph in a family name into two replacement characters.
				let mut index = 0usize;
				while index + 1 < self.bytes.len() {
					let unit = u16::from_be_bytes([self.bytes[index], self.bytes[index + 1]]);
					index += 2;
					let character = if (0xD800..0xDC00).contains(&unit) {
						if index + 1 >= self.bytes.len() {
							// A HIGH SURROGATE WITH NOTHING AFTER IT is a truncated name, and the
							// replacement character says so rather than dropping it silently.
							char::REPLACEMENT_CHARACTER
						} else {
							let low = u16::from_be_bytes([self.bytes[index], self.bytes[index + 1]]);
							if (0xDC00..0xE000).contains(&low) {
								index += 2;
								let value = 0x1_0000u32 + (((unit as u32) - 0xD800) << 10) + ((low as u32) - 0xDC00);
								char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER)
							} else {
								char::REPLACEMENT_CHARACTER
							}
						}
					} else {
						char::from_u32(unit as u32).unwrap_or(char::REPLACEMENT_CHARACTER)
					};
					push(character, into, &mut written)?;
				}
			}
		}
		Ok(written)
	}

	/// How many characters this name decodes to, without decoding it into anything.
	pub fn len(&self) -> usize {
		match self.encoding {
			Encoding::MacRoman => self.bytes.len(),
			Encoding::Utf16 => {
				let mut count = 0usize;
				let mut index = 0usize;
				while index + 1 < self.bytes.len() {
					let unit = u16::from_be_bytes([self.bytes[index], self.bytes[index + 1]]);
					index += if (0xD800..0xDC00).contains(&unit) && index + 3 < self.bytes.len() { 4 } else { 2 };
					count += 1;
				}
				count
			}
		}
	}

	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}

	/// Does this name equal a string, character for character?
	///
	/// THE COMPARISON A TRUTH ORACLE MAKES, and it is here so that it is made ONCE: a caller doing it
	/// with a buffer and a loop would have to decide what to do about a name longer than its buffer,
	/// and the answer - that it is not equal - is not a decision worth making twice.
	pub fn equals(&self, text: &str) -> bool {
		let mut expected = text.chars();
		match self.encoding {
			Encoding::MacRoman => {
				for byte in self.bytes {
					if expected.next() != Some(mac_roman(*byte)) {
						return false;
					}
				}
			}
			Encoding::Utf16 => {
				let mut buffer = [' '; 1];
				let mut index = 0usize;
				while index + 1 < self.bytes.len() {
					let unit = u16::from_be_bytes([self.bytes[index], self.bytes[index + 1]]);
					let consumed = if (0xD800..0xDC00).contains(&unit) && index + 3 < self.bytes.len() { 4 } else { 2 };
					let slice = Name { bytes: &self.bytes[index..index + consumed], encoding: Encoding::Utf16 };
					index += consumed;
					if slice.copy_into(&mut buffer).is_err() || expected.next() != Some(buffer[0]) {
						return false;
					}
				}
			}
		}
		expected.next().is_none()
	}
}

/// One Mac Roman byte as the character it means.
///
/// THE UPPER HALF IS NOT LATIN-1, and that is the whole reason this table exists: `0xA5` is a bullet
/// and not a yen sign, `0xD5` is a right single quote and not a capital O with a tilde. Reading an
/// older face's name as Latin-1 gives a family name that is wrong in a way that looks like a corrupt
/// font.
fn mac_roman(byte: u8) -> char {
	const UPPER: [char; 128] = [
		'Ä',
		'Å',
		'Ç',
		'É',
		'Ñ',
		'Ö',
		'Ü',
		'á',
		'à',
		'â',
		'ä',
		'ã',
		'å',
		'ç',
		'é',
		'è',
		'ê',
		'ë',
		'í',
		'ì',
		'î',
		'ï',
		'ñ',
		'ó',
		'ò',
		'ô',
		'ö',
		'õ',
		'ú',
		'ù',
		'û',
		'ü',
		'†',
		'°',
		'¢',
		'£',
		'§',
		'•',
		'¶',
		'ß',
		'®',
		'©',
		'™',
		'´',
		'¨',
		'≠',
		'Æ',
		'Ø',
		'∞',
		'±',
		'≤',
		'≥',
		'¥',
		'µ',
		'∂',
		'∑',
		'∏',
		'π',
		'∫',
		'ª',
		'º',
		'Ω',
		'æ',
		'ø',
		'¿',
		'¡',
		'¬',
		'√',
		'ƒ',
		'≈',
		'∆',
		'«',
		'»',
		'…',
		'\u{00A0}',
		'À',
		'Ã',
		'Õ',
		'Œ',
		'œ',
		'–',
		'—',
		'“',
		'”',
		'‘',
		'’',
		'÷',
		'◊',
		'ÿ',
		'Ÿ',
		'⁄',
		'€',
		'‹',
		'›',
		'ﬁ',
		'ﬂ',
		'‡',
		'·',
		'‚',
		'„',
		'‰',
		'Â',
		'Ê',
		'Á',
		'Ë',
		'È',
		'Í',
		'Î',
		'Ï',
		'Ì',
		'Ó',
		'Ô',
		'\u{F8FF}',
		'Ò',
		'Ú',
		'Û',
		'Ù',
		'ı',
		'ˆ',
		'˜',
		'¯',
		'˘',
		'˙',
		'˚',
		'¸',
		'˝',
		'˛',
		'ˇ',
	];
	if byte < 0x80 { byte as char } else { UPPER[(byte - 0x80) as usize] }
}

/// What `OS/2` says about a face, which is what a STYLE actually is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Os2 {
	pub version: u16,
	/// 1 to 1000, where 400 is regular and 700 is bold. A NUMBER, because "Bold" is a word in a
	/// language and this is not.
	pub weight_class: u16,
	/// 1 to 9, where 5 is normal.
	pub width_class: u16,
	/// The embedding permissions the face declares.
	pub fs_type: u16,
	/// The selection bits: italic, bold, regular, oblique and the rest.
	pub fs_selection: u16,
	pub typo_ascender: i16,
	pub typo_descender: i16,
	pub typo_line_gap: i16,
	pub win_ascent: u16,
	pub win_descent: u16,
	/// Present from version 2. `None` on a face that states an earlier version.
	pub x_height: Option<i16>,
	/// The same.
	pub cap_height: Option<i16>,
}

/// The `fsSelection` bits this system reads.
pub mod selection {
	pub const ITALIC: u16 = 0x0001;
	pub const BOLD: u16 = 0x0020;
	pub const REGULAR: u16 = 0x0040;
	/// USE_TYPO_METRICS: the face asks that its typographic metrics be used for line spacing rather
	/// than its Windows ones, which is the difference between correct leading and cramped leading.
	pub const USE_TYPO_METRICS: u16 = 0x0080;
	pub const OBLIQUE: u16 = 0x0200;
}

impl Os2 {
	/// Read the table, or answer `None` for a face that carries none.
	pub fn of(face: &Face<'_>) -> Result<Option<Self>, Error> {
		let Some(table) = face.table(b"OS/2")? else { return Ok(None) };
		let mut reader = table;
		let version = reader.u16().ok_or_else(|| bad(b"OS/2"))?;
		// THE PROFILE ADMITS 1 TO 5 AND NOT 0, and that is the profile's call rather than this
		// reader's: version 0 predates the selection bits and the typographic metrics that a style
		// and a line height are decided from, so a face that states it is a face this system cannot
		// answer the questions about - which is a refusal, not a default.
		if !opentype_profile::tables::table_of(b"OS/2").is_some_and(|table| table.majors.contains(&version)) {
			return Err(Error::Unsupported(opentype_profile::Unsupported::TableVersion { tag: *b"OS/2", major: version, minor: 0 }));
		}
		let _x_avg_char_width = reader.i16().ok_or_else(|| bad(b"OS/2"))?;
		let weight_class = reader.u16().ok_or_else(|| bad(b"OS/2"))?;
		let width_class = reader.u16().ok_or_else(|| bad(b"OS/2"))?;
		let fs_type = reader.u16().ok_or_else(|| bad(b"OS/2"))?;
		// The sub- and superscript sizes and offsets, the strikeout, and the family class: eight
		// pairs and three more, none of which this system reads.
		reader.skip(2 * 8 + 6).ok_or_else(|| bad(b"OS/2"))?;
		// PANOSE, the unicode ranges and the vendor: ten bytes, sixteen and four.
		reader.skip(10 + 16 + 4).ok_or_else(|| bad(b"OS/2"))?;
		let fs_selection = reader.u16().ok_or_else(|| bad(b"OS/2"))?;
		let _first_char = reader.u16().ok_or_else(|| bad(b"OS/2"))?;
		let _last_char = reader.u16().ok_or_else(|| bad(b"OS/2"))?;
		let typo_ascender = reader.i16().ok_or_else(|| bad(b"OS/2"))?;
		let typo_descender = reader.i16().ok_or_else(|| bad(b"OS/2"))?;
		let typo_line_gap = reader.i16().ok_or_else(|| bad(b"OS/2"))?;
		let win_ascent = reader.u16().ok_or_else(|| bad(b"OS/2"))?;
		let win_descent = reader.u16().ok_or_else(|| bad(b"OS/2"))?;
		// VERSION 2 IS WHERE THE X-HEIGHT AND CAP HEIGHT BEGIN, and a version 0 table that is padded
		// out to a later version's length still does not have them: the VERSION says what is there,
		// not the length, and reading past it gives two metrics out of somebody's padding.
		let (x_height, cap_height) = if version >= 2 {
			// The two code page ranges version 1 added sit between.
			reader.skip(8).ok_or_else(|| bad(b"OS/2"))?;
			(Some(reader.i16().ok_or_else(|| bad(b"OS/2"))?), Some(reader.i16().ok_or_else(|| bad(b"OS/2"))?))
		} else {
			(None, None)
		};
		Ok(Some(Self { version, weight_class, width_class, fs_type, fs_selection, typo_ascender, typo_descender, typo_line_gap, win_ascent, win_descent, x_height, cap_height }))
	}

	/// Is this face italic or oblique? Either bit means slanted, and a face that sets only the
	/// oblique one is still not upright.
	pub fn is_slanted(&self) -> bool {
		self.fs_selection & (selection::ITALIC | selection::OBLIQUE) != 0
	}

	/// Does the face ask for its TYPOGRAPHIC metrics to be used for line spacing?
	///
	/// THE ANSWER CHANGES THE LEADING OF EVERY LINE. A face that sets this and is laid out on its
	/// Windows metrics is set too tight or too loose everywhere, and it looks like a layout bug.
	pub fn uses_typographic_metrics(&self) -> bool {
		self.fs_selection & selection::USE_TYPO_METRICS != 0
	}
}

/// What `post` says: the italic angle, the underline, and whether the face is monospaced.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Post {
	/// The version as the format writes it, in 16.16.
	pub version: u32,
	/// The italic angle in degrees, counter-clockwise from vertical, in 16.16 fixed point. NEGATIVE
	/// for the ordinary forward slant, which is the sign that is got wrong.
	pub italic_angle: i32,
	pub underline_position: i16,
	pub underline_thickness: i16,
	/// Whether every glyph has the same advance - which is what a terminal needs to know and what no
	/// other table states.
	pub is_fixed_pitch: bool,
}

impl Post {
	pub fn of(face: &Face<'_>) -> Result<Option<Self>, Error> {
		let Some(table) = face.table(b"post")? else { return Ok(None) };
		let mut reader = table;
		let version = reader.u32().ok_or_else(|| bad(b"post"))?;
		// THE HEADER IS THE SAME IN EVERY VERSION AND ONLY WHAT FOLLOWS IT DIFFERS, which is exactly
		// why the version is checked here rather than where the names are read: a version 1.0 table
		// has a readable header and no name array at all, and a reader that got as far as the names
		// before noticing would have answered questions about a face it should have refused.
		let major = (version >> 16) as u16;
		if !opentype_profile::tables::table_of(b"post").is_some_and(|table| table.majors.contains(&major)) {
			return Err(Error::Unsupported(opentype_profile::Unsupported::TableVersion { tag: *b"post", major, minor: (version & 0xFFFF) as u16 }));
		}
		let italic_angle = reader.i32().ok_or_else(|| bad(b"post"))?;
		let underline_position = reader.i16().ok_or_else(|| bad(b"post"))?;
		let underline_thickness = reader.i16().ok_or_else(|| bad(b"post"))?;
		let is_fixed_pitch = reader.u32().ok_or_else(|| bad(b"post"))? != 0;
		Ok(Some(Self { version, italic_angle, underline_position, underline_thickness, is_fixed_pitch }))
	}
}

/// The `post` version 2.0 glyph names, which is the only place a glyph's own name is written down.
///
/// A NAME IS NOT A DECORATION. It is what a PDF exporter writes, what an accessibility layer reads
/// back and what a subsetter matches on, and a font that carries names and a reader that ignores them
/// produce a document nothing else can read the text out of.
#[derive(Clone, Copy)]
pub struct GlyphNames<'a> {
	table: Reader<'a>,
	count: u16,
	indices_at: usize,
	strings_at: usize,
}

/// The most bytes one glyph name may be, which is the format's own Pascal-string maximum.
pub const MAX_GLYPH_NAME: usize = 255;

impl<'a> GlyphNames<'a> {
	/// Read the names a face carries, or `None` for one that carries none.
	///
	/// VERSION 3.0 CARRIES NO NAMES AND THAT IS NOT AN ERROR - it is what almost every modern face
	/// states, and treating it as a fault would refuse most of the fonts in the world.
	pub fn of(face: &Face<'a>) -> Result<Option<Self>, Error> {
		let Some(post) = Post::of(face)? else { return Ok(None) };
		if post.version != 0x0002_0000 {
			return Ok(None);
		}
		let Some(table) = face.table(b"post")? else { return Ok(None) };
		let mut reader = table;
		// The header this reader has already read: the version, the angle, the underline, the pitch
		// and four memory fields nothing uses.
		reader.seek(32).ok_or_else(|| bad(b"post"))?;
		let count = reader.u16().ok_or_else(|| bad(b"post"))?;
		let indices_at = reader.position();
		let strings_at = indices_at.checked_add((count as usize).checked_mul(2).ok_or_else(|| bad(b"post"))?).ok_or_else(|| bad(b"post"))?;
		Ok(Some(Self { table, count, indices_at, strings_at }))
	}

	/// How many glyphs this table names.
	pub fn len(&self) -> u16 {
		self.count
	}

	pub fn is_empty(&self) -> bool {
		self.count == 0
	}

	/// One glyph's name, written into a buffer the caller owns, answering its length in bytes.
	///
	/// GLYPH NAMES ARE ASCII by the format's own rule, so bytes are the honest form; a reader that
	/// handed back a `str` would be asserting something about a font's contents that the font did not
	/// promise. An index below the standard count names one of the Macintosh standard glyphs and
	/// COSTS NO STORAGE IN THE FILE, which is why almost every entry of a real table is one.
	pub fn get(&self, glyph: u16, into: &mut [u8; MAX_GLYPH_NAME]) -> Result<Option<usize>, Error> {
		if glyph >= self.count {
			return Ok(None);
		}
		let mut indices = self.table;
		indices.seek(self.indices_at).ok_or_else(|| bad(b"post"))?;
		let index = indices.u16_at(glyph as usize).ok_or_else(|| bad(b"post"))? as usize;
		if index < STANDARD_NAMES.len() {
			let name = STANDARD_NAMES[index].as_bytes();
			into[..name.len()].copy_from_slice(name);
			return Ok(Some(name.len()));
		}
		// THE STRINGS ARE PASCAL STRINGS ONE AFTER ANOTHER, so reaching the nth means walking the
		// n before it - there is no index into them, and inventing one by assuming a fixed length
		// would read a name out of the middle of another.
		let wanted = index - STANDARD_NAMES.len();
		let mut reader = self.table;
		reader.seek(self.strings_at).ok_or_else(|| bad(b"post"))?;
		for _ in 0..wanted {
			let length = reader.u8().ok_or_else(|| bad(b"post"))? as usize;
			reader.skip(length).ok_or_else(|| bad(b"post"))?;
		}
		let length = reader.u8().ok_or_else(|| bad(b"post"))? as usize;
		let at = reader.position();
		let bytes = self.table.bytes(at, length).ok_or_else(|| bad(b"post"))?;
		into[..length].copy_from_slice(bytes);
		Ok(Some(length))
	}
}

/// The 258 standard Macintosh glyph names, which an index below that count refers to.
///
/// IN THE FILE THEY COST NOTHING, which is the point of them: a face whose glyphs are the ordinary
/// Latin ones names all of them without storing a single string, and a reader without this table
/// could not answer for any of those glyphs at all.
pub const STANDARD_NAMES: [&str; 258] = [
	".notdef",
	".null",
	"nonmarkingreturn",
	"space",
	"exclam",
	"quotedbl",
	"numbersign",
	"dollar",
	"percent",
	"ampersand",
	"quotesingle",
	"parenleft",
	"parenright",
	"asterisk",
	"plus",
	"comma",
	"hyphen",
	"period",
	"slash",
	"zero",
	"one",
	"two",
	"three",
	"four",
	"five",
	"six",
	"seven",
	"eight",
	"nine",
	"colon",
	"semicolon",
	"less",
	"equal",
	"greater",
	"question",
	"at",
	"A",
	"B",
	"C",
	"D",
	"E",
	"F",
	"G",
	"H",
	"I",
	"J",
	"K",
	"L",
	"M",
	"N",
	"O",
	"P",
	"Q",
	"R",
	"S",
	"T",
	"U",
	"V",
	"W",
	"X",
	"Y",
	"Z",
	"bracketleft",
	"backslash",
	"bracketright",
	"asciicircum",
	"underscore",
	"grave",
	"a",
	"b",
	"c",
	"d",
	"e",
	"f",
	"g",
	"h",
	"i",
	"j",
	"k",
	"l",
	"m",
	"n",
	"o",
	"p",
	"q",
	"r",
	"s",
	"t",
	"u",
	"v",
	"w",
	"x",
	"y",
	"z",
	"braceleft",
	"bar",
	"braceright",
	"asciitilde",
	"Adieresis",
	"Aring",
	"Ccedilla",
	"Eacute",
	"Ntilde",
	"Odieresis",
	"Udieresis",
	"aacute",
	"agrave",
	"acircumflex",
	"adieresis",
	"atilde",
	"aring",
	"ccedilla",
	"eacute",
	"egrave",
	"ecircumflex",
	"edieresis",
	"iacute",
	"igrave",
	"icircumflex",
	"idieresis",
	"ntilde",
	"oacute",
	"ograve",
	"ocircumflex",
	"odieresis",
	"otilde",
	"uacute",
	"ugrave",
	"ucircumflex",
	"udieresis",
	"dagger",
	"degree",
	"cent",
	"sterling",
	"section",
	"bullet",
	"paragraph",
	"germandbls",
	"registered",
	"copyright",
	"trademark",
	"acute",
	"dieresis",
	"notequal",
	"AE",
	"Oslash",
	"infinity",
	"plusminus",
	"lessequal",
	"greaterequal",
	"yen",
	"mu",
	"partialdiff",
	"summation",
	"product",
	"pi",
	"integral",
	"ordfeminine",
	"ordmasculine",
	"Omega",
	"ae",
	"oslash",
	"questiondown",
	"exclamdown",
	"logicalnot",
	"radical",
	"florin",
	"approxequal",
	"Delta",
	"guillemotleft",
	"guillemotright",
	"ellipsis",
	"nonbreakingspace",
	"Agrave",
	"Atilde",
	"Otilde",
	"OE",
	"oe",
	"endash",
	"emdash",
	"quotedblleft",
	"quotedblright",
	"quoteleft",
	"quoteright",
	"divide",
	"lozenge",
	"ydieresis",
	"Ydieresis",
	"fraction",
	"currency",
	"guilsinglleft",
	"guilsinglright",
	"fi",
	"fl",
	"daggerdbl",
	"periodcentered",
	"quotesinglbase",
	"quotedblbase",
	"perthousand",
	"Acircumflex",
	"Ecircumflex",
	"Aacute",
	"Edieresis",
	"Egrave",
	"Iacute",
	"Icircumflex",
	"Idieresis",
	"Igrave",
	"Oacute",
	"Ocircumflex",
	"apple",
	"Ograve",
	"Uacute",
	"Ucircumflex",
	"Ugrave",
	"dotlessi",
	"circumflex",
	"tilde",
	"macron",
	"breve",
	"dotaccent",
	"ring",
	"cedilla",
	"hungarumlaut",
	"ogonek",
	"caron",
	"Lslash",
	"lslash",
	"Scaron",
	"scaron",
	"Zcaron",
	"zcaron",
	"brokenbar",
	"Eth",
	"eth",
	"Yacute",
	"yacute",
	"Thorn",
	"thorn",
	"minus",
	"multiply",
	"onesuperior",
	"twosuperior",
	"threesuperior",
	"onehalf",
	"onequarter",
	"threequarters",
	"franc",
	"Gbreve",
	"gbreve",
	"Idotaccent",
	"Scedilla",
	"scedilla",
	"Cacute",
	"cacute",
	"Ccaron",
	"ccaron",
	"dcroat",
];
