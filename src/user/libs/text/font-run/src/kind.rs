//! WHICH FORM OF A GLYPH THE FACE ACTUALLY OFFERS - the flag that tells a rasteriser what to draw.
//!
//! "DRAW THE GLYPH" IS NOT ONE INSTRUCTION. The same glyph index may be an outline, a colour layer
//! list, a colour paint graph or a bitmap out of a strike, and they are drawn by four different
//! pieces of code with four different inputs. A run that did not say which would leave every consumer
//! to ask the face again, once per glyph, and to disagree with the producer about the answer.
//!
//! THIS READS THE INDICES AND NOTHING ELSE. Whether a glyph HAS a colour form is a lookup in a sorted
//! record list; DECODING that form is the rasteriser's work and another item's. Reading only the
//! index also means a face with a colour table this profile cannot yet decode is still described
//! honestly - the run says `ColrPaintGraph`, and a consumer that cannot draw one refuses the run
//! instead of silently drawing a blank outline.

use font_contract::{GlyphKind, KindSelection};
use font_parse::reader::Reader;
use font_parse::tables::Face;

use crate::Error;

/// What forms a face offers, read once per run rather than once per glyph.
#[derive(Clone, Copy)]
pub struct Forms<'a> {
	/// `COLR` version 1's base glyph list: the paint graphs.
	paint_graphs: Option<Reader<'a>>,
	/// `COLR` version 0's base glyph records: the layer lists.
	layer_lists: Option<(Reader<'a>, u16)>,
	/// The palette a colour glyph is drawn with, when the face declares any.
	palette: Option<u16>,
	/// `sbix`, with its strike table.
	sbix: Option<Reader<'a>>,
	/// `CBLC`, with its bitmap size records.
	cblc: Option<Reader<'a>>,
	glyph_count: u16,
}

fn bad(table: &[u8; 4]) -> Error {
	Error::Font(font_parse::Error::Malformed(font_parse::Malformed::InconsistentTable { table: *table }))
}

impl<'a> Forms<'a> {
	/// Read what the face declares. A face with none of these tables offers outlines alone, which is
	/// most faces and is not a special case.
	pub fn of(face: &Face<'a>) -> Result<Self, Error> {
		let mut forms = Forms { paint_graphs: None, layer_lists: None, palette: None, sbix: None, cblc: None, glyph_count: face.metrics.glyph_count };
		if let Some(colr) = face.table(b"COLR")? {
			let mut reader = colr;
			let version = reader.u16().ok_or_else(|| bad(b"COLR"))?;
			let base_count = reader.u16().ok_or_else(|| bad(b"COLR"))?;
			let base_at = reader.u32().ok_or_else(|| bad(b"COLR"))? as usize;
			let _layers_at = reader.u32().ok_or_else(|| bad(b"COLR"))?;
			let _layer_count = reader.u16().ok_or_else(|| bad(b"COLR"))?;
			if base_at != 0 && base_count != 0 {
				let records = colr.slice(base_at, (base_count as usize).checked_mul(6).ok_or_else(|| bad(b"COLR"))?).ok_or_else(|| bad(b"COLR"))?;
				forms.layer_lists = Some((records, base_count));
			}
			// VERSION 1 ADDS A SECOND, RICHER LIST and does not replace the first: a face carries both
			// so that a consumer which only understands version 0 still draws something. The richer
			// one is preferred, and a version this profile does not know is refused by NAME rather
			// than read as the version it resembles.
			if version == 1 {
				let list_at = reader.u32().ok_or_else(|| bad(b"COLR"))? as usize;
				if list_at != 0 {
					let list = colr.slice(list_at, colr.len().checked_sub(list_at).ok_or_else(|| bad(b"COLR"))?).ok_or_else(|| bad(b"COLR"))?;
					forms.paint_graphs = Some(list);
				}
			} else if version != 0 {
				return Err(Error::Font(font_parse::Error::Unsupported(font_parse::Unsupported::TableVersion { tag: *b"COLR", major: version, minor: 0 })));
			}
		}
		if let Some(cpal) = face.table(b"CPAL")? {
			let mut reader = cpal;
			let _version = reader.u16().ok_or_else(|| bad(b"CPAL"))?;
			let _entries = reader.u16().ok_or_else(|| bad(b"CPAL"))?;
			let palettes = reader.u16().ok_or_else(|| bad(b"CPAL"))?;
			// THE DEFAULT PALETTE IS THE FIRST ONE, which is what the format says and what a face
			// with one palette means. Choosing another is the consumer's, and it rebuilds the key.
			forms.palette = (palettes > 0).then_some(0);
		}
		forms.sbix = face.table(b"sbix")?;
		forms.cblc = face.table(b"CBLC")?;
		Ok(forms)
	}

	/// Which form this glyph is, and the strike or palette the form needs.
	///
	/// THE ORDER IS RICHEST FIRST and it is a decision, not an accident: a face that carries both a
	/// paint graph and a bitmap for one glyph means the paint graph, and drew the bitmap for consumers
	/// that cannot paint. `size_pixels` selects among the bitmap strikes, because a strike is made at
	/// a size and picking the wrong one is a blurred glyph rather than a wrong one.
	pub fn kind_of(&self, glyph: u16, size_pixels: u16) -> Result<(GlyphKind, KindSelection), Error> {
		if let Some(list) = self.paint_graphs
			&& base_paint_record(list, glyph)?
		{
			return Ok((GlyphKind::ColrPaintGraph, KindSelection { strike: None, palette: self.palette }));
		}
		if let Some((records, count)) = self.layer_lists
			&& base_glyph_record(records, count, glyph)?
		{
			return Ok((GlyphKind::ColrLayers, KindSelection { strike: None, palette: self.palette }));
		}
		if let Some(sbix) = self.sbix
			&& let Some(strike) = sbix_strike(sbix, glyph, self.glyph_count, size_pixels)?
		{
			return Ok((GlyphKind::BitmapStrike, KindSelection { strike: Some(strike), palette: None }));
		}
		if let Some(cblc) = self.cblc
			&& let Some(strike) = cblc_strike(cblc, glyph, size_pixels)?
		{
			return Ok((GlyphKind::BitmapStrike, KindSelection { strike: Some(strike), palette: None }));
		}
		Ok((GlyphKind::Outline, KindSelection::default()))
	}
}

/// Is this glyph in `COLR` version 1's base glyph list?
///
/// BINARY SEARCH, because the format states the list is sorted by glyph id and a face with several
/// thousand colour glyphs is ordinary. A list that is NOT sorted answers "no" for some glyphs rather
/// than reading out of bounds - the search is bounded by the record count either way.
fn base_paint_record(list: Reader<'_>, glyph: u16) -> Result<bool, Error> {
	let mut reader = list;
	let count = reader.u32().ok_or_else(|| bad(b"COLR"))? as usize;
	let records_at = reader.position();
	let records = list.slice(records_at, count.checked_mul(6).ok_or_else(|| bad(b"COLR"))?).ok_or_else(|| bad(b"COLR"))?;
	search_u16(&records, count, 3, glyph)
}

/// Is this glyph in `COLR` version 0's base glyph records?
fn base_glyph_record(records: Reader<'_>, count: u16, glyph: u16) -> Result<bool, Error> {
	search_u16(&records, count as usize, 3, glyph)
}

/// A binary search over a sorted array of records whose FIRST field is a `u16` glyph id.
///
/// `stride` is the record's length in `u16`s, which is how a `u16_at` index reaches the next record.
/// A list that is NOT sorted answers "no" for some glyphs rather than reading out of bounds: the
/// search is bounded by the record count either way, which is the property that matters here.
fn search_u16(records: &Reader<'_>, count: usize, stride: usize, glyph: u16) -> Result<bool, Error> {
	let mut low = 0usize;
	let mut high = count;
	while low < high {
		let middle = low + (high - low) / 2;
		let value = records.u16_at(middle * stride).ok_or_else(|| bad(b"COLR"))?;
		match value.cmp(&glyph) {
			core::cmp::Ordering::Equal => return Ok(true),
			core::cmp::Ordering::Less => low = middle + 1,
			core::cmp::Ordering::Greater => high = middle,
		}
	}
	Ok(false)
}

/// Which `sbix` strike holds this glyph, if any.
///
/// A STRIKE IS MADE AT A SIZE. The one chosen is the smallest whose ppem is at least the size asked
/// for - scaling a bitmap DOWN keeps the detail the designer drew and scaling one up does not - and
/// the largest available when every strike is smaller than the text.
fn sbix_strike(sbix: Reader<'_>, glyph: u16, glyph_count: u16, size_pixels: u16) -> Result<Option<u16>, Error> {
	let mut reader = sbix;
	let _version = reader.u16().ok_or_else(|| bad(b"sbix"))?;
	let _flags = reader.u16().ok_or_else(|| bad(b"sbix"))?;
	let count = reader.u32().ok_or_else(|| bad(b"sbix"))? as usize;
	let offsets_at = reader.position();
	let offsets = sbix.slice(offsets_at, count.checked_mul(4).ok_or_else(|| bad(b"sbix"))?).ok_or_else(|| bad(b"sbix"))?;
	let mut best: Option<(u16, u16)> = None;
	for index in 0..count {
		let at = offsets.u32_at(index).ok_or_else(|| bad(b"sbix"))? as usize;
		let strike = sbix.slice(at, sbix.len().checked_sub(at).ok_or_else(|| bad(b"sbix"))?).ok_or_else(|| bad(b"sbix"))?;
		let mut header = strike;
		let ppem = header.u16().ok_or_else(|| bad(b"sbix"))?;
		let _ppi = header.u16().ok_or_else(|| bad(b"sbix"))?;
		let data_at = header.position();
		let data = strike.slice(data_at, (glyph_count as usize + 1).checked_mul(4).ok_or_else(|| bad(b"sbix"))?).ok_or_else(|| bad(b"sbix"))?;
		let start = data.u32_at(glyph as usize).ok_or_else(|| bad(b"sbix"))?;
		let end = data.u32_at(glyph as usize + 1).ok_or_else(|| bad(b"sbix"))?;
		if end <= start {
			// AN EMPTY RANGE IS A GLYPH THIS STRIKE DOES NOT DRAW, which is ordinary: a colour-emoji
			// face has a strike for its emoji and nothing for its letters.
			continue;
		}
		let index = u16::try_from(index).map_err(|_| bad(b"sbix"))?;
		best = Some(better_strike(best, (index, ppem), size_pixels));
	}
	Ok(best.map(|(index, _)| index))
}

/// Which `CBLC` strike holds this glyph, if any.
fn cblc_strike(cblc: Reader<'_>, glyph: u16, size_pixels: u16) -> Result<Option<u16>, Error> {
	let mut reader = cblc;
	let _major = reader.u16().ok_or_else(|| bad(b"CBLC"))?;
	let _minor = reader.u16().ok_or_else(|| bad(b"CBLC"))?;
	let sizes = reader.u32().ok_or_else(|| bad(b"CBLC"))? as usize;
	let records_at = reader.position();
	let mut best: Option<(u16, u16)> = None;
	for index in 0..sizes {
		// A `BitmapSize` record is forty-eight bytes, and the fields this needs are at its two ends.
		let at = records_at.checked_add(index.checked_mul(48).ok_or_else(|| bad(b"CBLC"))?).ok_or_else(|| bad(b"CBLC"))?;
		let mut record = cblc;
		record.seek(at).ok_or_else(|| bad(b"CBLC"))?;
		let array_at = record.u32().ok_or_else(|| bad(b"CBLC"))? as usize;
		let _tables_size = record.u32().ok_or_else(|| bad(b"CBLC"))?;
		let subtables = record.u32().ok_or_else(|| bad(b"CBLC"))? as usize;
		record.skip(4 + 12 + 12).ok_or_else(|| bad(b"CBLC"))?;
		let first = record.u16().ok_or_else(|| bad(b"CBLC"))?;
		let last = record.u16().ok_or_else(|| bad(b"CBLC"))?;
		let ppem_x = record.u8().ok_or_else(|| bad(b"CBLC"))?;
		if glyph < first || glyph > last {
			continue;
		}
		// THE RECORD'S RANGE IS ITS OUTER CLAIM AND THE SUBTABLES ARE THE ACTUAL COVERAGE. Taking the
		// outer claim alone would tell a rasteriser to draw a bitmap the strike does not contain,
		// which is a missing glyph rather than a fallback to the outline it does have.
		let mut covered = false;
		for subtable in 0..subtables {
			let entry_at = array_at.checked_add(subtable.checked_mul(8).ok_or_else(|| bad(b"CBLC"))?).ok_or_else(|| bad(b"CBLC"))?;
			let mut entry = cblc;
			entry.seek(entry_at).ok_or_else(|| bad(b"CBLC"))?;
			let from = entry.u16().ok_or_else(|| bad(b"CBLC"))?;
			let to = entry.u16().ok_or_else(|| bad(b"CBLC"))?;
			if glyph >= from && glyph <= to {
				covered = true;
				break;
			}
		}
		if !covered {
			continue;
		}
		let index = u16::try_from(index).map_err(|_| bad(b"CBLC"))?;
		best = Some(better_strike(best, (index, ppem_x as u16), size_pixels));
	}
	Ok(best.map(|(index, _)| index))
}

/// Of two strikes that both hold the glyph, which one the text should be drawn from.
fn better_strike(current: Option<(u16, u16)>, candidate: (u16, u16), wanted: u16) -> (u16, u16) {
	let Some(current) = current else { return candidate };
	let fits = |ppem: u16| ppem >= wanted;
	match (fits(current.1), fits(candidate.1)) {
		// Both large enough: the smaller one, which is the least scaling down.
		(true, true) => {
			if candidate.1 < current.1 {
				candidate
			} else {
				current
			}
		}
		(true, false) => current,
		(false, true) => candidate,
		// Both too small: the largest, which is the least scaling up.
		(false, false) => {
			if candidate.1 > current.1 {
				candidate
			} else {
				current
			}
		}
	}
}
