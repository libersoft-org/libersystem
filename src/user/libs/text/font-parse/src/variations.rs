//! Variable fonts: the axes, the instances, and the values that vary with them.
//!
//! OUTLINES AND METRICS, NOT ONLY OUTLINES. `fvar` and `gvar` make a shape vary and do nothing for a
//! width: a font instanced without `HVAR` has correct outlines at the wrong advances, which is text
//! that is subtly mis-spaced at every non-default coordinate and reads as a rendering bug rather
//! than as a missing table. That is why `HVAR` and `MVAR` are profile entries rather than optional
//! extras, and why this module reads them.
//!
//! THE COORDINATE IS NORMALISED BEFORE ANYTHING IS LOOKED UP. A user coordinate - weight 650 - means
//! nothing to a delta set; what does is its position in `-1..1` between the axis's minimum, default
//! and maximum, adjusted by `avar` where the font maps its axes. Skipping the `avar` step gives an
//! instance that is somewhere else on the axis than the one the user asked for, and on a font with a
//! non-linear weight axis it is visibly the wrong weight.

use crate::reader::Reader;
use crate::tables::Face;
use crate::{Error, Malformed};

/// How many axes one face may declare.
///
/// THE FORMAT'S COUNT IS A `u16` AND THE PROFILE'S CEILING IS WHAT THIS READER STOPS AT. Every axis
/// multiplies the regions a delta is scaled over, so the axis count is work per glyph and not merely
/// a declaration - and it is the profile's number rather than this reader's, because a ceiling each
/// reader picked for itself is a ceiling nobody froze.
pub const MAX_AXES: usize = opentype_profile::limits::VARIATION_AXES as usize;

/// A normalised coordinate: `-1.0` to `1.0` in 2.14 fixed point, as the format stores it.
pub type Normalised = i16;

/// One variation axis, as `fvar` declares it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Axis {
	pub tag: [u8; 4],
	/// The design range, in 16.16 fixed point as the format stores it.
	pub minimum: i32,
	pub default: i32,
	pub maximum: i32,
}

/// The axes of a face, and how many instances it names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Variations<'a> {
	fvar: Reader<'a>,
	axes_at: usize,
	pub axis_count: usize,
	pub instance_count: usize,
	instance_size: usize,
	axis_size: usize,
}

fn bad(table: &[u8; 4]) -> Error {
	Error::Malformed(Malformed::InconsistentTable { table: *table })
}

impl<'a> Variations<'a> {
	/// The variation axes of a face, or `None` when it is not a variable font - which is not an
	/// error and is most fonts.
	pub fn of(face: &Face<'a>) -> Result<Option<Self>, Error> {
		let Some(fvar) = face.table(b"fvar")? else { return Ok(None) };
		let mut reader = fvar;
		let major = reader.u16().ok_or_else(|| bad(b"fvar"))?;
		let _minor = reader.u16().ok_or_else(|| bad(b"fvar"))?;
		if major != 1 {
			return Err(Error::Unsupported(opentype_profile::Unsupported::TableVersion { tag: *b"fvar", major, minor: 0 }));
		}
		let axes_at = reader.u16().ok_or_else(|| bad(b"fvar"))? as usize;
		let _reserved = reader.u16().ok_or_else(|| bad(b"fvar"))?;
		let axis_count = reader.u16().ok_or_else(|| bad(b"fvar"))? as usize;
		let axis_size = reader.u16().ok_or_else(|| bad(b"fvar"))? as usize;
		let instance_count = reader.u16().ok_or_else(|| bad(b"fvar"))? as usize;
		let instance_size = reader.u16().ok_or_else(|| bad(b"fvar"))? as usize;
		if axis_count > MAX_AXES {
			return Err(Error::Unsupported(opentype_profile::Unsupported::Exceeded { limit: "variation axes", ceiling: opentype_profile::limits::VARIATION_AXES, asked: axis_count as u64 }));
		}
		if axis_count == 0 || axis_size < 20 {
			return Err(bad(b"fvar"));
		}
		// The whole axis array must be inside the table before any of it is read.
		fvar.slice(axes_at, axis_count.checked_mul(axis_size).ok_or_else(|| bad(b"fvar"))?).ok_or_else(|| bad(b"fvar"))?;
		Ok(Some(Self { fvar, axes_at, axis_count, instance_count, instance_size, axis_size }))
	}

	/// One axis.
	pub fn axis(&self, index: usize) -> Result<Axis, Error> {
		if index >= self.axis_count {
			return Err(bad(b"fvar"));
		}
		let at = self.axes_at.checked_add(index.checked_mul(self.axis_size).ok_or_else(|| bad(b"fvar"))?).ok_or_else(|| bad(b"fvar"))?;
		let mut reader = self.fvar;
		reader.seek(at).ok_or_else(|| bad(b"fvar"))?;
		let tag = reader.tag().ok_or_else(|| bad(b"fvar"))?;
		let minimum = reader.i32().ok_or_else(|| bad(b"fvar"))?;
		let default = reader.i32().ok_or_else(|| bad(b"fvar"))?;
		let maximum = reader.i32().ok_or_else(|| bad(b"fvar"))?;
		// An axis whose range is not ordered is one no coordinate can be placed on.
		if minimum > default || default > maximum {
			return Err(bad(b"fvar"));
		}
		Ok(Axis { tag, minimum, default, maximum })
	}

	/// One NAMED instance's coordinates, in the font's own design units.
	///
	/// A NAMED INSTANCE IS A POSITION AND NOT A SEPARATE FACE, which is what makes "Bold" in a
	/// variable font the same face at a coordinate - and why a catalogue that listed the instances
	/// as faces would publish four faces backed by one file.
	pub fn instance(&self, index: usize, into: &mut [i32]) -> Result<(), Error> {
		if index >= self.instance_count || into.len() < self.axis_count {
			return Err(bad(b"fvar"));
		}
		let instances_at = self.axes_at.checked_add(self.axis_count.checked_mul(self.axis_size).ok_or_else(|| bad(b"fvar"))?).ok_or_else(|| bad(b"fvar"))?;
		let at = instances_at.checked_add(index.checked_mul(self.instance_size).ok_or_else(|| bad(b"fvar"))?).ok_or_else(|| bad(b"fvar"))?;
		let mut reader = self.fvar;
		reader.seek(at).ok_or_else(|| bad(b"fvar"))?;
		// A name id, then flags, then one 16.16 coordinate per axis.
		reader.skip(4).ok_or_else(|| bad(b"fvar"))?;
		for slot in into.iter_mut().take(self.axis_count) {
			*slot = reader.i32().ok_or_else(|| bad(b"fvar"))?;
		}
		Ok(())
	}

	/// Normalise a user coordinate onto one axis, as the format defines it.
	///
	/// THE DEFAULT IS ZERO AND THE ENDS ARE `-1` AND `1`, with the two halves scaled separately -
	/// which matters because an axis's default is almost never its midpoint: weight runs 100 to 900
	/// with a default of 400, so 700 is not "three quarters of the way up".
	pub fn normalise(&self, index: usize, coordinate: i32) -> Result<Normalised, Error> {
		let axis = self.axis(index)?;
		let clamped = coordinate.clamp(axis.minimum, axis.maximum);
		let value = if clamped < axis.default {
			let span = (axis.default as i64) - (axis.minimum as i64);
			if span == 0 { 0 } else { -(((axis.default as i64 - clamped as i64) * 16384) / span) }
		} else if clamped > axis.default {
			let span = (axis.maximum as i64) - (axis.default as i64);
			if span == 0 { 0 } else { ((clamped as i64 - axis.default as i64) * 16384) / span }
		} else {
			0
		};
		Ok(value.clamp(-16384, 16384) as i16)
	}

	/// Apply `avar`'s own mapping to an already-normalised coordinate.
	///
	/// WITHOUT THIS AN INSTANCE LANDS SOMEWHERE ELSE ON THE AXIS. `avar` is how a font says that the
	/// middle of its weight range is not the middle of its design - and on a font that maps its axes
	/// non-linearly, skipping it is visibly the wrong weight rather than a rounding difference.
	pub fn adjust(&self, face: &Face<'a>, index: usize, normalised: Normalised) -> Result<Normalised, Error> {
		let Some(avar) = face.table(b"avar")? else { return Ok(normalised) };
		let mut reader = avar;
		let major = reader.u16().ok_or_else(|| bad(b"avar"))?;
		let _minor = reader.u16().ok_or_else(|| bad(b"avar"))?;
		// Version 2 is a different axis-mapping model and is excluded by the profile by name.
		if major != 1 {
			return Err(Error::Unsupported(opentype_profile::Unsupported::TableVersion { tag: *b"avar", major, minor: 0 }));
		}
		let _reserved = reader.u16().ok_or_else(|| bad(b"avar"))?;
		let count = reader.u16().ok_or_else(|| bad(b"avar"))? as usize;
		if count != self.axis_count {
			// An `avar` with a different number of segment maps than the font has axes is a font
			// that contradicts itself.
			return Err(bad(b"avar"));
		}
		// Walk to this axis's segment map: each is a count followed by that many pairs.
		let mut at = reader.position();
		for _ in 0..index {
			let mut map = avar;
			map.seek(at).ok_or_else(|| bad(b"avar"))?;
			let pairs = map.u16().ok_or_else(|| bad(b"avar"))? as usize;
			at = at.checked_add(2 + pairs.checked_mul(4).ok_or_else(|| bad(b"avar"))?).ok_or_else(|| bad(b"avar"))?;
		}
		let mut map = avar;
		map.seek(at).ok_or_else(|| bad(b"avar"))?;
		let pairs = map.u16().ok_or_else(|| bad(b"avar"))? as usize;
		let mut previous: Option<(i16, i16)> = None;
		for _ in 0..pairs {
			let from = map.i16().ok_or_else(|| bad(b"avar"))?;
			let to = map.i16().ok_or_else(|| bad(b"avar"))?;
			if normalised == from {
				return Ok(to);
			}
			if let Some((previous_from, previous_to)) = previous
				&& normalised > previous_from
				&& normalised < from
			{
				// Between two stated points, the map is linear.
				let span = (from as i64) - (previous_from as i64);
				if span == 0 {
					return Ok(previous_to);
				}
				let along = (normalised as i64) - (previous_from as i64);
				let range = (to as i64) - (previous_to as i64);
				return Ok((previous_to as i64 + (along * range) / span).clamp(-16384, 16384) as i16);
			}
			previous = Some((from, to));
		}
		// Outside everything the map states, the coordinate is unchanged - which is what a map with
		// no pairs means too.
		Ok(normalised)
	}
}

/// The advance of a glyph at a coordinate, through `HVAR`.
///
/// THIS IS THE HALF A TABLE LIST FORGETS. Without it a variable font's glyphs are the right shape at
/// the wrong widths, which is text that is subtly mis-spaced everywhere except at the default
/// instance - and nothing about it looks like a missing table.
pub fn advance_delta(face: &Face<'_>, glyph: u16, coordinates: &[Normalised]) -> Result<i32, Error> {
	let Some(hvar) = face.table(b"HVAR")? else { return Ok(0) };
	let mut reader = hvar;
	let major = reader.u16().ok_or_else(|| bad(b"HVAR"))?;
	let _minor = reader.u16().ok_or_else(|| bad(b"HVAR"))?;
	if major != 1 {
		return Err(Error::Unsupported(opentype_profile::Unsupported::TableVersion { tag: *b"HVAR", major, minor: 0 }));
	}
	let store_at = reader.u32().ok_or_else(|| bad(b"HVAR"))? as usize;
	let map_at = reader.u32().ok_or_else(|| bad(b"HVAR"))? as usize;
	let store = hvar.slice(store_at, hvar.len().checked_sub(store_at).ok_or_else(|| bad(b"HVAR"))?).ok_or_else(|| bad(b"HVAR"))?;
	// The delta-set index map says which delta set a glyph uses; without one the glyph id IS the
	// index, which is what the format means by an offset of zero.
	let (outer, inner) = if map_at == 0 {
		(0u16, glyph)
	} else {
		let map = hvar.slice(map_at, hvar.len().checked_sub(map_at).ok_or_else(|| bad(b"HVAR"))?).ok_or_else(|| bad(b"HVAR"))?;
		delta_set_index(map, glyph)?
	};
	item_delta(store, outer, inner, coordinates, b"HVAR")
}

/// The four-character names of the font-wide metrics `MVAR` varies, as this tree reads them.
///
/// NAMED RATHER THAN NUMBERED, because a caller asking for "the ascender at this coordinate" should
/// not have to know that the tag is `hasc` - and a tag written out at a call site is a typo away
/// from silently reading a metric the font does not vary.
pub mod metric {
	/// The horizontal ascender.
	pub const ASCENDER: [u8; 4] = *b"hasc";
	/// The horizontal descender.
	pub const DESCENDER: [u8; 4] = *b"hdsc";
	/// The horizontal line gap.
	pub const LINE_GAP: [u8; 4] = *b"hlgp";
	/// The x-height.
	pub const X_HEIGHT: [u8; 4] = *b"xhgt";
	/// The cap height.
	pub const CAP_HEIGHT: [u8; 4] = *b"cpht";
	/// Where an underline sits.
	pub const UNDERLINE_OFFSET: [u8; 4] = *b"undo";
	/// How thick an underline is.
	pub const UNDERLINE_SIZE: [u8; 4] = *b"unds";
}

/// A font-wide metric at a coordinate, through `MVAR`.
///
/// THE LINE IS AS MUCH A METRIC AS THE ADVANCE IS. `HVAR` makes the glyphs sit at the right distance
/// along a line and this makes the LINES sit at the right distance from each other; a variable face
/// whose weight axis thickens its strokes almost always raises its ascender with them, and a layout
/// that reads `hhea` alone sets every instance on the leading of the default one. That shows as lines
/// that touch at one end of the axis and drift apart at the other, which looks like a layout bug
/// rather than a table nobody read.
///
/// A tag this font does not vary answers ZERO, which is the honest answer: the static table already
/// holds its value and nothing moves it.
pub fn metric_delta(face: &Face<'_>, tag: [u8; 4], coordinates: &[Normalised]) -> Result<i32, Error> {
	let Some(mvar) = face.table(b"MVAR")? else { return Ok(0) };
	let mut reader = mvar;
	let major = reader.u16().ok_or_else(|| bad(b"MVAR"))?;
	let _minor = reader.u16().ok_or_else(|| bad(b"MVAR"))?;
	if major != 1 {
		return Err(Error::Unsupported(opentype_profile::Unsupported::TableVersion { tag: *b"MVAR", major, minor: 0 }));
	}
	let _reserved = reader.u16().ok_or_else(|| bad(b"MVAR"))?;
	// THE RECORD SIZE IS THE STRIDE THE FONT STATES, not the one this version happens to know: a
	// later minor version adds fields to the record, and walking it by a hard-coded eight would read
	// the second record out of the middle of the first.
	let record_size = reader.u16().ok_or_else(|| bad(b"MVAR"))? as usize;
	let record_count = reader.u16().ok_or_else(|| bad(b"MVAR"))? as usize;
	let store_at = reader.u16().ok_or_else(|| bad(b"MVAR"))? as usize;
	if record_count == 0 || store_at == 0 {
		// A font may carry the table and vary nothing, which is not a fault.
		return Ok(0);
	}
	if record_size < 8 {
		return Err(bad(b"MVAR"));
	}
	let records_at = reader.position();
	let mut found = None;
	for index in 0..record_count {
		let at = records_at.checked_add(index.checked_mul(record_size).ok_or_else(|| bad(b"MVAR"))?).ok_or_else(|| bad(b"MVAR"))?;
		let mut record = mvar;
		record.seek(at).ok_or_else(|| bad(b"MVAR"))?;
		let this = record.tag().ok_or_else(|| bad(b"MVAR"))?;
		if this != tag {
			continue;
		}
		let outer = record.u16().ok_or_else(|| bad(b"MVAR"))?;
		let inner = record.u16().ok_or_else(|| bad(b"MVAR"))?;
		found = Some((outer, inner));
		break;
	}
	let Some((outer, inner)) = found else { return Ok(0) };
	let store = mvar.slice(store_at, mvar.len().checked_sub(store_at).ok_or_else(|| bad(b"MVAR"))?).ok_or_else(|| bad(b"MVAR"))?;
	item_delta(store, outer, inner, coordinates, b"MVAR")
}

/// One entry of a delta-set index map.
fn delta_set_index(map: Reader<'_>, glyph: u16) -> Result<(u16, u16), Error> {
	let mut reader = map;
	let format = reader.u16().ok_or_else(|| bad(b"HVAR"))?;
	let count = reader.u16().ok_or_else(|| bad(b"HVAR"))? as usize;
	// The high byte of the format says how many bits the inner index takes, and the low two bits how
	// many bytes an entry is. Reading either wrong shifts every glyph's delta by one.
	let inner_bits = ((format >> 8) & 0x0F) + 1;
	let entry_size = ((format & 0x0003) + 1) as usize;
	let index = (glyph as usize).min(count.saturating_sub(1));
	let at = 4usize.checked_add(index.checked_mul(entry_size).ok_or_else(|| bad(b"HVAR"))?).ok_or_else(|| bad(b"HVAR"))?;
	let mut entry = map;
	entry.seek(at).ok_or_else(|| bad(b"HVAR"))?;
	let value = match entry_size {
		1 => entry.u8().ok_or_else(|| bad(b"HVAR"))? as u32,
		2 => entry.u16().ok_or_else(|| bad(b"HVAR"))? as u32,
		3 => entry.u24().ok_or_else(|| bad(b"HVAR"))?,
		_ => entry.u32().ok_or_else(|| bad(b"HVAR"))?,
	};
	let inner_mask = (1u32 << inner_bits) - 1;
	Ok(((value >> inner_bits) as u16, (value & inner_mask) as u16))
}

/// One delta from an item variation store, scaled by how far the coordinate is into each region.
///
/// THE SCALAR IS A PRODUCT OVER THE AXES, and a region that does not apply contributes ZERO rather
/// than being skipped - which is the same thing only when every other axis applies fully.
fn item_delta(store: Reader<'_>, outer: u16, inner: u16, coordinates: &[Normalised], table: &[u8; 4]) -> Result<i32, Error> {
	let mut reader = store;
	let format = reader.u16().ok_or_else(|| bad(table))?;
	if format != 1 {
		return Err(Error::Unsupported(opentype_profile::Unsupported::SubtableFormat { table: *table, format }));
	}
	let regions_at = reader.u32().ok_or_else(|| bad(table))? as usize;
	let data_count = reader.u16().ok_or_else(|| bad(table))? as usize;
	if outer as usize >= data_count {
		return Ok(0);
	}
	let data_offset = store.u32_at(2 + outer as usize).ok_or_else(|| bad(table))? as usize;
	let data = store.slice(data_offset, store.len().checked_sub(data_offset).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
	let mut reader = data;
	let item_count = reader.u16().ok_or_else(|| bad(table))? as usize;
	let short_count = reader.u16().ok_or_else(|| bad(table))? as usize;
	let region_count = reader.u16().ok_or_else(|| bad(table))? as usize;
	if inner as usize >= item_count || short_count > region_count {
		return Ok(0);
	}
	let indices_at = reader.position();
	let row_size = short_count * 2 + (region_count - short_count);
	let row_at = indices_at.checked_add(region_count.checked_mul(2).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
	let row_at = row_at.checked_add((inner as usize).checked_mul(row_size).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;

	let regions = store.slice(regions_at, store.len().checked_sub(regions_at).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
	let mut total: i64 = 0;
	let mut row = data;
	row.seek(row_at).ok_or_else(|| bad(table))?;
	for index in 0..region_count {
		let delta = if index < short_count { row.i16().ok_or_else(|| bad(table))? as i32 } else { row.i8().ok_or_else(|| bad(table))? as i32 };
		let region = data.u16_at(indices_at / 2 + index).ok_or_else(|| bad(table))?;
		let scalar = region_scalar(regions, region, coordinates, table)?;
		total += (delta as i64 * scalar as i64) / 16384;
	}
	Ok(total.clamp(i32::MIN as i64, i32::MAX as i64) as i32)
}

/// How much of a region's delta applies at a coordinate: a product over the axes, in 2.14.
fn region_scalar(regions: Reader<'_>, region: u16, coordinates: &[Normalised], table: &[u8; 4]) -> Result<i32, Error> {
	let mut reader = regions;
	let axis_count = reader.u16().ok_or_else(|| bad(table))? as usize;
	let region_count = reader.u16().ok_or_else(|| bad(table))? as usize;
	// EACH REGION IS READ FOR EVERY DELTA, so a store's width is work per glyph rather than a size.
	if region_count > opentype_profile::limits::VARIATION_REGIONS as usize {
		return Err(Error::Unsupported(opentype_profile::Unsupported::Exceeded { limit: "variation regions", ceiling: opentype_profile::limits::VARIATION_REGIONS, asked: region_count as u64 }));
	}
	if axis_count > MAX_AXES {
		return Err(Error::Unsupported(opentype_profile::Unsupported::Exceeded { limit: "variation axes", ceiling: opentype_profile::limits::VARIATION_AXES, asked: axis_count as u64 }));
	}
	if region as usize >= region_count {
		return Ok(0);
	}
	let at = 4usize.checked_add((region as usize).checked_mul(axis_count.checked_mul(6).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?).ok_or_else(|| bad(table))?;
	let mut reader = regions;
	reader.seek(at).ok_or_else(|| bad(table))?;
	let mut scalar: i64 = 16384;
	for index in 0..axis_count {
		let start = reader.i16().ok_or_else(|| bad(table))? as i64;
		let peak = reader.i16().ok_or_else(|| bad(table))? as i64;
		let end = reader.i16().ok_or_else(|| bad(table))? as i64;
		// A peak of zero means this axis does not narrow the region at all.
		if peak == 0 {
			continue;
		}
		let coordinate = coordinates.get(index).copied().unwrap_or(0) as i64;
		if coordinate == peak {
			continue;
		}
		// OUTSIDE THE REGION THE WHOLE DELTA IS ZERO, not merely reduced: a region that does not
		// apply contributes nothing, and treating it as partially applied is how an instance ends up
		// between two masters that were never meant to be mixed.
		if coordinate <= start || coordinate >= end {
			return Ok(0);
		}
		let factor = if coordinate < peak { ((coordinate - start) * 16384) / (peak - start) } else { ((end - coordinate) * 16384) / (end - peak) };
		scalar = (scalar * factor) / 16384;
	}
	Ok(scalar as i32)
}
