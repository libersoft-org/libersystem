//! `sbix` and `CBLC`/`CBDT`: the glyphs that are PICTURES.
//!
//! A STRIKE IS MADE AT A SIZE, and that is what makes it different from everything else a face
//! carries. An outline is one description drawn at any size; a strike is a photograph of a glyph at
//! one pixel size, and a face has several. Choosing the wrong one is a blurred glyph rather than a
//! wrong one, which is the kind of defect that is never filed and never fixed.
//!
//! THIS HANDS BACK THE IMAGE'S BYTES AND DOES NOT DECODE THEM. A strike's contents are PNG, TIFF or
//! JPEG - whole image formats, with their own decoders and their own history of vulnerabilities - and
//! a font parser that decoded them would be an image decoder reached through a document. What kind of
//! image it is, where it sits and how big it is are this layer's; the pixels are the image decoder's,
//! which this system already has and which is reached deliberately.
//!
//! THE TWO MECHANISMS ARE NOT VARIANTS OF ONE. `sbix` is a strike per size with a blob per glyph;
//! `CBLC` is an index of index subtables in FIVE formats, pointing into `CBDT`. They share nothing
//! but the idea, so they are read separately and answer the same type.

use crate::reader::Reader;
use crate::tables::Face;
use crate::{Error, Malformed, Unsupported};

fn bad(table: &[u8; 4]) -> Error {
	Error::Malformed(Malformed::InconsistentTable { table: *table })
}

/// What kind of image a strike holds. NAMED RATHER THAN GUESSED from the bytes: a face states it, and
/// sniffing a magic number is how a mislabelled blob reaches the wrong decoder.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImageFormat {
	Png,
	Tiff,
	Jpeg,
}

/// One glyph's picture, as the face stores it.
#[derive(Clone, Copy)]
pub struct Image<'a> {
	pub format: ImageFormat,
	/// The image's own bytes, undecoded.
	pub bytes: &'a [u8],
	/// Where the image sits relative to the glyph's origin, in PIXELS of this strike - not in font
	/// units, which is the conversion a caller must not skip.
	pub origin: (i16, i16),
	/// The pixel size this strike was drawn at, which is what a caller scales from.
	pub ppem: u16,
}

/// A face's bitmap strikes, whichever mechanism it uses.
#[derive(Clone, Copy)]
pub struct Bitmaps<'a> {
	sbix: Option<Reader<'a>>,
	cblc: Option<Reader<'a>>,
	cbdt: Option<Reader<'a>>,
	glyph_count: u16,
}

impl<'a> Bitmaps<'a> {
	/// Read what a face carries, or `None` for a face with no strikes at all.
	pub fn of(face: &Face<'a>) -> Result<Option<Self>, Error> {
		let sbix = face.table(b"sbix")?;
		let cblc = face.table(b"CBLC")?;
		let cbdt = face.table(b"CBDT")?;
		if sbix.is_none() && cblc.is_none() {
			return Ok(None);
		}
		Ok(Some(Self { sbix, cblc, cbdt, glyph_count: face.metrics.glyph_count }))
	}

	/// How many strikes the face has.
	pub fn strike_count(&self) -> Result<usize, Error> {
		if let Some(sbix) = self.sbix {
			let mut reader = sbix;
			reader.skip(4).ok_or_else(|| bad(b"sbix"))?;
			return Ok(reader.u32().ok_or_else(|| bad(b"sbix"))? as usize);
		}
		let Some(cblc) = self.cblc else { return Ok(0) };
		let mut reader = cblc;
		reader.skip(4).ok_or_else(|| bad(b"CBLC"))?;
		Ok(reader.u32().ok_or_else(|| bad(b"CBLC"))? as usize)
	}

	/// One glyph's image out of one strike, or `None` when that strike does not draw it.
	pub fn image(&self, glyph: u16, strike: u16) -> Result<Option<Image<'a>>, Error> {
		if self.sbix.is_some() {
			return self.sbix_image(glyph, strike, 0);
		}
		self.cbdt_image(glyph, strike)
	}

	/// `sbix`: a strike per size, with one blob per glyph.
	///
	/// `depth` bounds the `dupe` chain - one glyph's data saying "use another glyph's" - which is a
	/// real mechanism and a cycle waiting to be followed.
	fn sbix_image(&self, glyph: u16, strike: u16, depth: u8) -> Result<Option<Image<'a>>, Error> {
		let tag = b"sbix";
		let Some(sbix) = self.sbix else { return Ok(None) };
		if depth > 2 {
			// A `dupe` POINTING AT A `dupe` IS A CHAIN, and one that comes back is a loop. Two steps
			// is more than any real face uses and is a bound rather than a guess.
			return Err(bad(tag));
		}
		let mut reader = sbix;
		let _version = reader.u16().ok_or_else(|| bad(tag))?;
		let _flags = reader.u16().ok_or_else(|| bad(tag))?;
		let count = reader.u32().ok_or_else(|| bad(tag))? as usize;
		if strike as usize >= count {
			return Ok(None);
		}
		let offsets_at = reader.position();
		let offsets = sbix.slice(offsets_at, count.checked_mul(4).ok_or_else(|| bad(tag))?).ok_or_else(|| bad(tag))?;
		let strike_at = offsets.u32_at(strike as usize).ok_or_else(|| bad(tag))? as usize;
		let body = sbix.slice(strike_at, sbix.len().checked_sub(strike_at).ok_or_else(|| bad(tag))?).ok_or_else(|| bad(tag))?;
		let mut header = body;
		let ppem = header.u16().ok_or_else(|| bad(tag))?;
		let _ppi = header.u16().ok_or_else(|| bad(tag))?;
		let data_at = header.position();
		let data = body.slice(data_at, (self.glyph_count as usize + 1).checked_mul(4).ok_or_else(|| bad(tag))?).ok_or_else(|| bad(tag))?;
		if glyph >= self.glyph_count {
			return Ok(None);
		}
		let start = data.u32_at(glyph as usize).ok_or_else(|| bad(tag))? as usize;
		let end = data.u32_at(glyph as usize + 1).ok_or_else(|| bad(tag))? as usize;
		if end <= start {
			// AN EMPTY RANGE IS A GLYPH THIS STRIKE DOES NOT DRAW, which is ordinary: a colour-emoji
			// face has a strike for its emoji and nothing for its letters.
			return Ok(None);
		}
		// The glyph record: two origins, a four-character type, and then the image.
		let record = body.slice(start, end - start).ok_or_else(|| bad(tag))?;
		let mut reader = record;
		let origin_x = reader.i16().ok_or_else(|| bad(tag))?;
		let origin_y = reader.i16().ok_or_else(|| bad(tag))?;
		let kind = reader.tag().ok_or_else(|| bad(tag))?;
		let bytes_at = reader.position();
		// `dupe` IS NOT AN IMAGE FORMAT: it is a glyph id saying "this glyph is drawn as that one",
		// which is how a face stores one picture for several code points.
		if &kind == b"dupe" {
			let other = reader.u16().ok_or_else(|| bad(tag))?;
			if other == glyph {
				return Err(bad(tag));
			}
			return self.sbix_image(other, strike, depth + 1);
		}
		let format = match &kind {
			b"png " => ImageFormat::Png,
			b"tiff" => ImageFormat::Tiff,
			b"jpg " => ImageFormat::Jpeg,
			_ => return Err(Error::Unsupported(Unsupported::SubtableFormat { table: *tag, format: u16::from_be_bytes([kind[0], kind[1]]) })),
		};
		let bytes = record.bytes(bytes_at, (end - start).checked_sub(bytes_at).ok_or_else(|| bad(tag))?).ok_or_else(|| bad(tag))?;
		Ok(Some(Image { format, bytes, origin: (origin_x, origin_y), ppem }))
	}

	/// `CBLC`/`CBDT`: an index of index subtables, in five formats, pointing into a second table.
	fn cbdt_image(&self, glyph: u16, strike: u16) -> Result<Option<Image<'a>>, Error> {
		let tag = b"CBLC";
		let (Some(cblc), Some(cbdt)) = (self.cblc, self.cbdt) else { return Ok(None) };
		let mut reader = cblc;
		reader.skip(4).ok_or_else(|| bad(tag))?;
		let sizes = reader.u32().ok_or_else(|| bad(tag))? as usize;
		if strike as usize >= sizes {
			return Ok(None);
		}
		// A `BitmapSize` record is forty-eight bytes.
		let record_at = reader.position().checked_add((strike as usize).checked_mul(48).ok_or_else(|| bad(tag))?).ok_or_else(|| bad(tag))?;
		let mut record = cblc;
		record.seek(record_at).ok_or_else(|| bad(tag))?;
		let array_at = record.u32().ok_or_else(|| bad(tag))? as usize;
		let _tables_size = record.u32().ok_or_else(|| bad(tag))?;
		let subtables = record.u32().ok_or_else(|| bad(tag))? as usize;
		record.skip(4 + 12 + 12).ok_or_else(|| bad(tag))?;
		let first = record.u16().ok_or_else(|| bad(tag))?;
		let last = record.u16().ok_or_else(|| bad(tag))?;
		let ppem = record.u8().ok_or_else(|| bad(tag))? as u16;
		if glyph < first || glyph > last {
			return Ok(None);
		}

		for index in 0..subtables {
			let entry_at = array_at.checked_add(index.checked_mul(8).ok_or_else(|| bad(tag))?).ok_or_else(|| bad(tag))?;
			let mut entry = cblc;
			entry.seek(entry_at).ok_or_else(|| bad(tag))?;
			let from = entry.u16().ok_or_else(|| bad(tag))?;
			let to = entry.u16().ok_or_else(|| bad(tag))?;
			let subtable_at = array_at.checked_add(entry.u32().ok_or_else(|| bad(tag))? as usize).ok_or_else(|| bad(tag))?;
			if glyph < from || glyph > to {
				continue;
			}
			let mut subtable = cblc;
			subtable.seek(subtable_at).ok_or_else(|| bad(tag))?;
			let index_format = subtable.u16().ok_or_else(|| bad(tag))?;
			let image_format = subtable.u16().ok_or_else(|| bad(tag))?;
			let image_base = subtable.u32().ok_or_else(|| bad(tag))? as usize;
			let (offset, length) = match index_format {
				// FORMAT 1 IS FOUR-BYTE OFFSETS AND FORMAT 3 IS TWO-BYTE ONES, and a glyph's image is
				// the range between its offset and the NEXT one - so the array has one entry more
				// than the range it covers, and a reader that allocated exactly `to - from` entries
				// reads the last glyph's length out of whatever follows.
				1 | 3 => {
					let which = (glyph - from) as usize;
					let wide = index_format == 1;
					let at = subtable.position();
					let stride = if wide { 4 } else { 2 };
					let mut offsets = cblc;
					offsets.seek(at.checked_add(which.checked_mul(stride).ok_or_else(|| bad(tag))?).ok_or_else(|| bad(tag))?).ok_or_else(|| bad(tag))?;
					let start = if wide { offsets.u32().ok_or_else(|| bad(tag))? as usize } else { offsets.u16().ok_or_else(|| bad(tag))? as usize };
					let end = if wide { offsets.u32().ok_or_else(|| bad(tag))? as usize } else { offsets.u16().ok_or_else(|| bad(tag))? as usize };
					if end <= start {
						return Ok(None);
					}
					(image_base.checked_add(start).ok_or_else(|| bad(tag))?, end - start)
				}
				// FORMAT 2 IS A FIXED SIZE PER GLYPH, with the metrics stated once for the whole run.
				2 => {
					let size = subtable.u32().ok_or_else(|| bad(tag))? as usize;
					let which = (glyph - from) as usize;
					(image_base.checked_add(which.checked_mul(size).ok_or_else(|| bad(tag))?).ok_or_else(|| bad(tag))?, size)
				}
				// FORMATS 4 AND 5 ARE SPARSE: a list of the glyphs that are actually drawn, which is
				// what a strike covering scattered code points needs. Taking the record's own range
				// as the coverage would claim images that are not there.
				4 => {
					let pairs = subtable.u32().ok_or_else(|| bad(tag))? as usize;
					let at = subtable.position();
					let mut found = None;
					for pair in 0..pairs {
						let mut entry = cblc;
						entry.seek(at.checked_add(pair.checked_mul(4).ok_or_else(|| bad(tag))?).ok_or_else(|| bad(tag))?).ok_or_else(|| bad(tag))?;
						let id = entry.u16().ok_or_else(|| bad(tag))?;
						let start = entry.u16().ok_or_else(|| bad(tag))? as usize;
						if id != glyph {
							continue;
						}
						let end = entry.u16().ok_or_else(|| bad(tag))? as usize;
						found = Some((start, end));
						break;
					}
					let Some((start, end)) = found else { return Ok(None) };
					if end <= start {
						return Ok(None);
					}
					(image_base.checked_add(start).ok_or_else(|| bad(tag))?, end - start)
				}
				5 => {
					let size = subtable.u32().ok_or_else(|| bad(tag))? as usize;
					subtable.skip(8).ok_or_else(|| bad(tag))?; // the metrics, stated once
					let count = subtable.u32().ok_or_else(|| bad(tag))? as usize;
					let at = subtable.position();
					let mut which = None;
					for slot in 0..count {
						let mut entry = cblc;
						entry.seek(at.checked_add(slot.checked_mul(2).ok_or_else(|| bad(tag))?).ok_or_else(|| bad(tag))?).ok_or_else(|| bad(tag))?;
						if entry.u16().ok_or_else(|| bad(tag))? == glyph {
							which = Some(slot);
							break;
						}
					}
					let Some(slot) = which else { return Ok(None) };
					(image_base.checked_add(slot.checked_mul(size).ok_or_else(|| bad(tag))?).ok_or_else(|| bad(tag))?, size)
				}
				_ => return Err(Error::Unsupported(Unsupported::SubtableFormat { table: *tag, format: index_format })),
			};

			// The image record in `CBDT`, whose leading metrics depend on the IMAGE format the index
			// subtable declared - and reading the wrong number of them takes the first bytes of the
			// picture for metrics and the metrics for picture.
			let record = cbdt.slice(offset, length).ok_or_else(|| bad(b"CBDT"))?;
			let mut reader = record;
			let (origin, skipped) = match image_format {
				17 => {
					let _height = reader.u8().ok_or_else(|| bad(b"CBDT"))?;
					let _width = reader.u8().ok_or_else(|| bad(b"CBDT"))?;
					let bearing_x = reader.i8().ok_or_else(|| bad(b"CBDT"))? as i16;
					let bearing_y = reader.i8().ok_or_else(|| bad(b"CBDT"))? as i16;
					let _advance = reader.u8().ok_or_else(|| bad(b"CBDT"))?;
					let data_length = reader.u32().ok_or_else(|| bad(b"CBDT"))? as usize;
					((bearing_x, bearing_y), (reader.position(), data_length))
				}
				18 => {
					let _height = reader.u8().ok_or_else(|| bad(b"CBDT"))?;
					let _width = reader.u8().ok_or_else(|| bad(b"CBDT"))?;
					let bearing_x = reader.i8().ok_or_else(|| bad(b"CBDT"))? as i16;
					let bearing_y = reader.i8().ok_or_else(|| bad(b"CBDT"))? as i16;
					reader.skip(4).ok_or_else(|| bad(b"CBDT"))?; // the advance and the vertical metrics
					let data_length = reader.u32().ok_or_else(|| bad(b"CBDT"))? as usize;
					((bearing_x, bearing_y), (reader.position(), data_length))
				}
				// FORMAT 19 TAKES ITS METRICS FROM `CBLC`, which is why its record is nothing but a
				// length and the data - and why a reader that expected metrics here would take the
				// first five bytes of the PNG for them.
				19 => {
					let data_length = reader.u32().ok_or_else(|| bad(b"CBDT"))? as usize;
					((0, 0), (reader.position(), data_length))
				}
				_ => return Err(Error::Unsupported(Unsupported::SubtableFormat { table: *b"CBDT", format: image_format })),
			};
			let (bytes_at, data_length) = skipped;
			let bytes = record.bytes(bytes_at, data_length).ok_or_else(|| bad(b"CBDT"))?;
			// EVERY `CBDT` IMAGE THIS PROFILE ADMITS IS A PNG, which is the profile's own list rather
			// than this reader's guess.
			return Ok(Some(Image { format: ImageFormat::Png, bytes, origin, ppem }));
		}
		Ok(None)
	}
}
