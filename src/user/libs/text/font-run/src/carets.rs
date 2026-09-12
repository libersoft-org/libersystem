//! THE LIGATURE CARETS THE FACE ITSELF DECLARES, out of `GDEF`'s caret list.
//!
//! WHERE THE CARET GOES INSIDE `ffi`. A ligature is one glyph for three characters, and a caret that
//! could only stand at its two ends would make the middle of a word unreachable - press the arrow key
//! and the caret jumps three characters. The dividing positions are not derivable from the glyph's
//! width either: `ffi` is not three equal thirds, and the designer who drew it said where its parts
//! divide.
//!
//! THREE FORMATS AND ALL THREE ARE READ. A coordinate in design units, a POINT INDEX into the glyph's
//! own outline - resolved by walking that outline, because the point is where the designer put it and
//! nowhere else - and a coordinate with a device table, whose device adjustment is a hinting
//! correction for one pixel size and is deliberately not applied by a profile that does not hint.

use font_parse::glyf::{Outline, Point};
use font_parse::reader::Reader;
use font_parse::tables::Face;
use font_shape::layout::coverage_index;

use crate::Error;

/// The most caret positions this reads out of one ligature.
///
/// A BOUND BECAUSE THE TABLE IS INPUT. A ligature of more parts than this is refused rather than
/// allowed to ask for unbounded work; no script writes one, and a font that claims to is describing
/// something other than a ligature.
pub const MAX_CARETS: usize = 16;

fn bad() -> Error {
	Error::Font(font_parse::Error::Malformed(font_parse::Malformed::InconsistentTable { table: *b"GDEF" }))
}

/// `GDEF`'s ligature caret list, found once per run.
#[derive(Clone, Copy)]
pub struct Carets<'a> {
	list: Option<Reader<'a>>,
}

impl<'a> Carets<'a> {
	/// Read the caret list a face declares. A face with no `GDEF`, or one whose `GDEF` declares no
	/// caret list, has none - which is not an error and is most faces.
	pub fn of(face: &Face<'a>) -> Result<Self, Error> {
		let Some(gdef) = face.table(b"GDEF")? else { return Ok(Self { list: None }) };
		let mut reader = gdef;
		let _major = reader.u16().ok_or_else(bad)?;
		let _minor = reader.u16().ok_or_else(bad)?;
		let _classes_at = reader.u16().ok_or_else(bad)?;
		let _attach_at = reader.u16().ok_or_else(bad)?;
		let at = reader.u16().ok_or_else(bad)? as usize;
		if at == 0 {
			return Ok(Self { list: None });
		}
		let list = gdef.slice(at, gdef.len().checked_sub(at).ok_or_else(bad)?).ok_or_else(bad)?;
		Ok(Self { list: Some(list) })
	}

	/// The caret coordinates INSIDE one ligature glyph, in font units, in the order the format states
	/// them. Answers how many were written into `into`.
	pub fn of_glyph(&self, face: &Face<'a>, glyph: u16, into: &mut [i32; MAX_CARETS]) -> Result<usize, Error> {
		let Some(list) = self.list else { return Ok(0) };
		let mut reader = list;
		let coverage_at = reader.u16().ok_or_else(bad)? as usize;
		let count = reader.u16().ok_or_else(bad)? as usize;
		let coverage = list.slice(coverage_at, list.len().checked_sub(coverage_at).ok_or_else(bad)?).ok_or_else(bad)?;
		let Some(index) = coverage_index(coverage, glyph)? else { return Ok(0) };
		if index as usize >= count {
			// THE COVERAGE AND THE OFFSET ARRAY MUST AGREE, and a font where they do not is refused
			// rather than read at an index one of them does not have.
			return Err(bad());
		}
		let offsets_at = reader.position();
		let offsets = list.slice(offsets_at, count.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
		let glyph_at = offsets.u16_at(index as usize).ok_or_else(bad)? as usize;
		let ligature = list.slice(glyph_at, list.len().checked_sub(glyph_at).ok_or_else(bad)?).ok_or_else(bad)?;
		let mut reader = ligature;
		let caret_count = reader.u16().ok_or_else(bad)? as usize;
		if caret_count > MAX_CARETS {
			return Err(bad());
		}
		let caret_offsets_at = reader.position();
		let caret_offsets = ligature.slice(caret_offsets_at, caret_count.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
		for (slot, out) in into.iter_mut().enumerate().take(caret_count) {
			let at = caret_offsets.u16_at(slot).ok_or_else(bad)? as usize;
			let value = ligature.slice(at, ligature.len().checked_sub(at).ok_or_else(bad)?).ok_or_else(bad)?;
			*out = coordinate(face, glyph, value)?;
		}
		Ok(caret_count)
	}
}

/// One caret value, whichever of the three forms it is written in.
fn coordinate(face: &Face<'_>, glyph: u16, value: Reader<'_>) -> Result<i32, Error> {
	let mut reader = value;
	let format = reader.u16().ok_or_else(bad)?;
	match format {
		1 => Ok(reader.i16().ok_or_else(bad)? as i32),
		2 => {
			// A POINT INDEX INTO THE GLYPH'S OWN OUTLINE. Resolving it means walking the outline,
			// which is the only place the point's x coordinate exists - and a point index past the
			// glyph's points is a font contradicting itself rather than a caret at zero.
			let wanted = reader.u16().ok_or_else(bad)?;
			let mut finder = PointAt { wanted: wanted as usize, seen: 0, found: None };
			font_parse::glyf::walk(face, glyph, &mut finder)?;
			finder.found.map(|x| x as i32).ok_or_else(bad)
		}
		3 => {
			// The coordinate, and a DEVICE TABLE this profile does not apply: a device adjustment is
			// a hinting correction stated per pixel size, and applying one in a pipeline that does
			// not hint would move the caret away from the outline it divides.
			Ok(reader.i16().ok_or_else(bad)? as i32)
		}
		_ => Err(Error::Font(font_parse::Error::Unsupported(font_parse::Unsupported::SubtableFormat { table: *b"GDEF", format }))),
	}
}

/// An outline walk that answers where one numbered point is.
struct PointAt {
	wanted: usize,
	seen: usize,
	found: Option<i16>,
}

impl Outline for PointAt {
	fn point(&mut self, point: Point) -> bool {
		if self.seen == self.wanted {
			self.found = Some(point.x);
			return false;
		}
		self.seen += 1;
		true
	}
}
