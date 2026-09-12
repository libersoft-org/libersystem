//! The staged metadata record a font catalogue reads, and the closed vocabularies it is written in.
//!
//! THE CATALOGUE PARSES NO FONT. Family, style, axes, face index and format come out of OpenType and
//! TTC bytes, so anything that DERIVES them is a font parser - and a font parser written before the
//! item that publishes the closed profile is the second, unprofiled, hostile-input parser the
//! ordering exists to prevent. The record is therefore DECLARED: a face enters this image because
//! somebody added it to the source tree, and its record is a checked-in declaration beside it,
//! authored in the same review.
//!
//! WHAT IS CHECKED HERE AND WHAT IS NOT. This validates: the field vocabulary is closed, the values
//! are inside it, the encoded record fits `MAX_FACE_METADATA_BYTES`, and the declared digest is the
//! digest of the bytes beside it. It does NOT check whether the declaration is TRUE - a
//! hand-authored family name the font does not carry passes everything here. That oracle is owed by
//! the profiled parser, which parses every staged face and requires what it recovers to equal the
//! declaration; until then the declarations are trusted-by-review, and this comment is where that is
//! said rather than implied.
//!
//! THE FORMAT IS TEXT, because a person writes it. One `key = value` per line, `#` comments, and an
//! `axis` line repeated per axis. A binary sidecar would be a file nobody can author or review, and
//! the review is the only thing standing behind the declaration until the parser oracle exists.

use alloc::string::String;
use alloc::vec::Vec;

/// WHERE THE FACES ARE, and there is exactly one answer.
///
/// The path is not in the manifest - a `Role` carries a tag, a kind, a provider, a presence, an
/// interface and a source, and no path - so it lives in code, and it lives HERE rather than in the
/// supervisor because two programs need it: the supervisor mints the read-only directory client over
/// it, and the catalogue reads through that client with full paths. Two copies of one path is the
/// drift that makes a mint succeed over a directory nobody reads.
pub const FONT_DIRECTORY: &str = "vol://system/share/fonts";

/// The suffix of the declaration staged beside a face: `sans.ttf` is declared by `sans.ttf.face`.
///
/// THE WHOLE FILE NAME PLUS A SUFFIX, not the stem plus one - `sans.ttf` and `sans.otf` are two
/// faces and a stem-keyed sidecar would be one declaration claiming both.
pub const RECORD_SUFFIX: &str = ".face";

/// How many faces may be installed. Sixty-four is an appliance's system font set with room to spare,
/// and it is small enough that the whole catalogue is a fixed array rather than a growing one.
pub const MAX_INSTALLED_FACES: usize = 64;

/// The record's ENCODED WIRE bytes - framing included, which is what makes the reply bound below
/// arithmetic rather than hope. It holds a family name, a style name, the axes, the face index, the
/// format and the identity digest with margin.
pub const MAX_FACE_METADATA_BYTES: usize = 256;

/// The reserved envelope of a LIST reply: the list count, the result tag, the correlation id and
/// whatever framing a reply grows later. Deliberately far larger than today's handful of bytes,
/// because a bound that has to be recomputed the next time a header gains a field is a bound that
/// will be wrong again.
pub const LIST_REPLY_ENVELOPE_BYTES: usize = 4096;

/// The whole LIST reply: every record at its maximum, plus the envelope. Far inside the message
/// bound, so a full LIST is one message and needs no paging.
pub const MAX_LIST_REPLY_BYTES: usize = MAX_INSTALLED_FACES * MAX_FACE_METADATA_BYTES + LIST_REPLY_ENVELOPE_BYTES;

/// What a LIST reply carries BESIDE its records, today: the list count, the result tag and the
/// correlation id a generated service reply puts in front of the result.
///
/// MODELLED HERE SO THE BOUND IS ARITHMETIC. A previous version fixed the reply bound at exactly
/// `64 * 256` and said the full reply fits inside it - but the records ALONE are exactly that, and a
/// reply is never only its records. The envelope above is the RESERVE that makes the bound survive a
/// header gaining a field; this is what is actually spent today.
pub const LIST_REPLY_FRAMING_BYTES: usize = 2 + 1 + 4;

/// How many axes a declaration may carry. The same bound the shared font contract carries, for the
/// same reason: a record is a fixed-size thing and a growable axis list is not.
pub const MAX_DECLARED_AXES: usize = 8;

/// The longest family and style names a declaration may carry. Bounded because the record is, and
/// stated here rather than derived from the ceiling so a reader can see what the 256 is spent on.
pub const MAX_FAMILY_BYTES: usize = 64;
pub const MAX_STYLE_BYTES: usize = 32;

/// The face formats this system admits. CLOSED: a declaration outside it is refused rather than
/// published, because the vocabulary is what a fallback decision and a resolve are steered by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FaceFormat {
	/// Outlines in `glyf`.
	TruetypeGlyf,
	/// Outlines in `CFF `.
	OpentypeCff,
	/// Outlines in `CFF2`, which is the variable one.
	OpentypeCff2,
	/// A collection file: several faces, selected by the face index.
	Collection,
}

/// The width axis, as the nine values the vocabulary has.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FaceWidth {
	UltraCondensed,
	ExtraCondensed,
	Condensed,
	SemiCondensed,
	Normal,
	SemiExpanded,
	Expanded,
	ExtraExpanded,
	UltraExpanded,
}

/// The slant axis. `Italic` and `Oblique` are different designs and not two words for one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FaceSlant {
	Upright,
	Italic,
	Oblique,
}

/// One variation axis as the declaration states it: the OpenType tag and the design range.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeclaredAxis {
	/// The four-character tag, big-endian - `wght`, `wdth`, `slnt`.
	pub tag: u32,
	pub minimum: i32,
	pub default: i32,
	pub maximum: i32,
}

/// The declared record for one face.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FaceRecord {
	pub family: String,
	pub style: String,
	pub format: FaceFormat,
	/// Which face within the file; non-zero only for a collection.
	pub face_index: u32,
	/// The weight axis value, 1..=1000 as OpenType defines it.
	pub weight: u16,
	pub width: FaceWidth,
	pub slant: FaceSlant,
	pub axes: Vec<DeclaredAxis>,
	/// The digest of the face FILE the record is beside.
	pub digest: [u8; 32],
}

/// Why a declaration was refused.
///
/// ONE REASON PER REFUSAL, NAMED, because the report says what was rejected and why - and because a
/// refusal that said only "invalid" would leave whoever staged the face guessing which of eleven
/// rules it broke.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RecordError {
	/// A line that is not `key = value`.
	Malformed,
	/// A key the vocabulary does not have.
	UnknownKey,
	/// A value outside the closed vocabulary of its key.
	UnknownValue,
	/// A required field the declaration never states.
	Missing,
	/// A field stated twice, which is a declaration that contradicts itself.
	Repeated,
	/// A name or an axis count past its bound.
	TooLong,
	/// The ENCODED RECORD past `MAX_FACE_METADATA_BYTES`.
	///
	/// ITS OWN CASE, because it is one of the three installation CEILINGS and the reconciliation
	/// rule treats a ceiling differently from a malformed declaration: a ceiling breach publishes
	/// nothing rather than dropping one face.
	PastMetadataCeiling,
	/// A number that is not one, or is outside its range.
	OutOfRange,
	/// The declared digest is not the digest of the bytes beside it.
	DigestMismatch,
}

impl FaceFormat {
	fn parse(value: &str) -> Option<Self> {
		Some(match value {
			"truetype-glyf" => Self::TruetypeGlyf,
			"opentype-cff" => Self::OpentypeCff,
			"opentype-cff2" => Self::OpentypeCff2,
			"collection" => Self::Collection,
			_ => return None,
		})
	}
}

impl FaceWidth {
	fn parse(value: &str) -> Option<Self> {
		Some(match value {
			"ultra-condensed" => Self::UltraCondensed,
			"extra-condensed" => Self::ExtraCondensed,
			"condensed" => Self::Condensed,
			"semi-condensed" => Self::SemiCondensed,
			"normal" => Self::Normal,
			"semi-expanded" => Self::SemiExpanded,
			"expanded" => Self::Expanded,
			"extra-expanded" => Self::ExtraExpanded,
			"ultra-expanded" => Self::UltraExpanded,
			_ => return None,
		})
	}
}

impl FaceSlant {
	fn parse(value: &str) -> Option<Self> {
		Some(match value {
			"upright" => Self::Upright,
			"italic" => Self::Italic,
			"oblique" => Self::Oblique,
			_ => return None,
		})
	}
}

impl FaceRecord {
	/// The record's ENCODED WIRE size: what `MAX_FACE_METADATA_BYTES` bounds.
	///
	/// COUNTED THE WAY THE WIRE COUNTS IT - a length before each variable field, a fixed width for
	/// each scalar - because the bound is about what a LIST reply carries and not about how long the
	/// text file was. A declaration can be comfortable to read and still not fit.
	pub fn encoded_len(&self) -> usize {
		// family and style: a u16 length each, then the bytes.
		2 + self.family.len() + 2 + self.style.len()
		// format, width, slant: one byte each. face index and weight: four and two.
		+ 1 + 1 + 1 + 4 + 2
		// the axes: a u16 count, then a tag and three 32-bit values each.
		+ 2 + self.axes.len() * (4 + 4 + 4 + 4)
		// the digest.
		+ 32
	}

	/// Is this a replacement of the SAME face, or a relabelling of it?
	///
	/// THE ONE RULE THAT CLOSES THE CAPTURE CASE. A replacement is a new version of the same face; a
	/// different family, style, axis set, face index or format is a DIFFERENT face and arrives under
	/// its own name. A replacement that changes any of them is refused - which is exactly the attack
	/// it exists for: relabelling an installed face to capture somebody else's fallback.
	///
	/// The DIGEST is deliberately not part of this: a replacement always changes the bytes, and
	/// comparing them would make every replacement a relabelling.
	pub fn declares_same_identity_as(&self, other: &Self) -> bool {
		self.family == other.family && self.style == other.style && self.format == other.format && self.face_index == other.face_index && self.axes == other.axes
	}
}

/// Parse a declaration, and check it against the bytes it is beside.
///
/// `digest_of_face` is the digest the catalogue computed over the face FILE. Passing it in rather
/// than computing it here is what keeps this module free of a hash implementation and testable
/// without one - and the comparison is the point: a record whose digest does not match the face
/// beside it is not published.
pub fn parse(text: &str, digest_of_face: &[u8; 32]) -> Result<FaceRecord, RecordError> {
	let mut family: Option<String> = None;
	let mut style: Option<String> = None;
	let mut format: Option<FaceFormat> = None;
	let mut face_index: Option<u32> = None;
	let mut weight: Option<u16> = None;
	let mut width: Option<FaceWidth> = None;
	let mut slant: Option<FaceSlant> = None;
	let mut digest: Option<[u8; 32]> = None;
	let mut axes: Vec<DeclaredAxis> = Vec::new();

	for line in text.lines() {
		let line = match line.find('#') {
			Some(at) => &line[..at],
			None => line,
		};
		let line = line.trim();
		if line.is_empty() {
			continue;
		}
		let Some((key, value)) = line.split_once('=') else {
			return Err(RecordError::Malformed);
		};
		let key = key.trim();
		let value = value.trim();
		if value.is_empty() {
			return Err(RecordError::Malformed);
		}
		match key {
			"family" => {
				if family.is_some() {
					return Err(RecordError::Repeated);
				}
				if value.len() > MAX_FAMILY_BYTES {
					return Err(RecordError::TooLong);
				}
				family = Some(String::from(value));
			}
			"style" => {
				if style.is_some() {
					return Err(RecordError::Repeated);
				}
				if value.len() > MAX_STYLE_BYTES {
					return Err(RecordError::TooLong);
				}
				style = Some(String::from(value));
			}
			"format" => {
				if format.is_some() {
					return Err(RecordError::Repeated);
				}
				format = Some(FaceFormat::parse(value).ok_or(RecordError::UnknownValue)?);
			}
			"face-index" => {
				if face_index.is_some() {
					return Err(RecordError::Repeated);
				}
				face_index = Some(value.parse::<u32>().map_err(|_| RecordError::OutOfRange)?);
			}
			"weight" => {
				if weight.is_some() {
					return Err(RecordError::Repeated);
				}
				let parsed = value.parse::<u16>().map_err(|_| RecordError::OutOfRange)?;
				if !(1..=1000).contains(&parsed) {
					return Err(RecordError::OutOfRange);
				}
				weight = Some(parsed);
			}
			"width" => {
				if width.is_some() {
					return Err(RecordError::Repeated);
				}
				width = Some(FaceWidth::parse(value).ok_or(RecordError::UnknownValue)?);
			}
			"slant" => {
				if slant.is_some() {
					return Err(RecordError::Repeated);
				}
				slant = Some(FaceSlant::parse(value).ok_or(RecordError::UnknownValue)?);
			}
			"digest" => {
				if digest.is_some() {
					return Err(RecordError::Repeated);
				}
				digest = Some(parse_digest(value)?);
			}
			"axis" => {
				if axes.len() == MAX_DECLARED_AXES {
					return Err(RecordError::TooLong);
				}
				axes.push(parse_axis(value)?);
			}
			_ => return Err(RecordError::UnknownKey),
		}
	}

	let record = FaceRecord { family: family.ok_or(RecordError::Missing)?, style: style.ok_or(RecordError::Missing)?, format: format.ok_or(RecordError::Missing)?, face_index: face_index.ok_or(RecordError::Missing)?, weight: weight.ok_or(RecordError::Missing)?, width: width.ok_or(RecordError::Missing)?, slant: slant.ok_or(RecordError::Missing)?, axes, digest: digest.ok_or(RecordError::Missing)? };
	// A NON-COLLECTION FILE HAS ONE FACE. A declaration that selects face 3 of a single-face file is
	// a declaration nobody can resolve, and it is checkable without opening the font.
	if record.face_index != 0 && record.format != FaceFormat::Collection {
		return Err(RecordError::OutOfRange);
	}
	if record.encoded_len() > MAX_FACE_METADATA_BYTES {
		return Err(RecordError::PastMetadataCeiling);
	}
	if record.digest != *digest_of_face {
		return Err(RecordError::DigestMismatch);
	}
	Ok(record)
}

fn parse_digest(value: &str) -> Result<[u8; 32], RecordError> {
	if value.len() != 64 {
		return Err(RecordError::OutOfRange);
	}
	let bytes = value.as_bytes();
	let mut digest = [0u8; 32];
	for (at, slot) in digest.iter_mut().enumerate() {
		let high = hex(bytes[at * 2]).ok_or(RecordError::OutOfRange)?;
		let low = hex(bytes[at * 2 + 1]).ok_or(RecordError::OutOfRange)?;
		*slot = (high << 4) | low;
	}
	Ok(digest)
}

fn hex(character: u8) -> Option<u8> {
	Some(match character {
		b'0'..=b'9' => character - b'0',
		b'a'..=b'f' => character - b'a' + 10,
		_ => return None,
	})
}

/// `axis = wght 100 400 900` - the tag and the design range.
fn parse_axis(value: &str) -> Result<DeclaredAxis, RecordError> {
	let mut words = value.split_whitespace();
	let tag = words.next().ok_or(RecordError::Malformed)?;
	if tag.len() != 4 || !tag.bytes().all(|byte| byte.is_ascii_graphic()) {
		return Err(RecordError::OutOfRange);
	}
	let mut packed = [0u8; 4];
	packed.copy_from_slice(tag.as_bytes());
	let mut number = || -> Result<i32, RecordError> { words.next().ok_or(RecordError::Malformed)?.parse::<i32>().map_err(|_| RecordError::OutOfRange) };
	let minimum = number()?;
	let default = number()?;
	let maximum = number()?;
	if words.next().is_some() {
		return Err(RecordError::Malformed);
	}
	if minimum > default || default > maximum {
		return Err(RecordError::OutOfRange);
	}
	Ok(DeclaredAxis { tag: u32::from_be_bytes(packed), minimum, default, maximum })
}

// THE DIGEST IS NOT COMPUTED HERE, AND THE REASON IS THE SHARED IMAGE. It is `bootproto`'s SHA-256 -
// the tree's one implementation - and a wrapper in this crate would put that symbol into the
// `service-util` LIBRARY, which every consumer of these modules then has to find a provider for. The
// two callers that need it - the catalogue and the staging tool - link `bootproto` themselves and
// call it directly, so there is still exactly one implementation and no new edge in the image graph.
// Everything here TAKES a digest as an argument, which is also what makes it testable without one.

#[cfg(test)]
#[path = "font_record/tests.rs"]
mod tests;
