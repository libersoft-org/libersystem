//! `gvar`: the outline deltas that make a glyph's SHAPE vary.
//!
//! THE OTHER HALF OF A VARIABLE FONT. `HVAR` makes the widths right at a coordinate and this makes
//! the shapes right; a font with one and not the other is drawn at the wrong weight with the right
//! spacing, or at the right weight with the wrong spacing, and neither is better than the other.
//!
//! THE DELTAS ARE SPARSE, AND THAT IS WHAT INFERENCE IS FOR. A tuple may give deltas for a few points
//! and leave the rest to be INTERPOLATED from their neighbours - the format's own rule, because a
//! font that listed every point of every glyph for every master would be several times larger. An
//! implementation that skipped the inference would move the referenced points and leave the others
//! where they were, which TEARS the outline apart rather than failing: a stem moves and the serif it
//! carries stays behind.
//!
//! A COMPOSITE GLYPH IS ADDRESSED BY COMPONENT, NOT BY POINT. Its deltas move whole components, and
//! a parser that applied them to the expanded points of the components would move an accent by the
//! delta meant for the letter under it. Inference does not apply there and is not performed: there
//! is no coordinate to interpolate ALONG between two component origins, and inventing one would move
//! a component nothing asked to move.
//!
//! THE FOUR PHANTOM POINTS ARE PART OF THE COUNT. Every glyph's delta list ends with four points the
//! glyph does not contain - the two side bearings and the two vertical ones - and a parser that
//! leaves them out of the count reads the y deltas starting four values early, which is every point
//! of every varied glyph moved by somebody else's number. They are counted here and answered
//! separately, because without `HVAR` they are where the advance variation comes from.
//!
//! NO ALLOCATION HERE. The deltas are accumulated into slices the CALLER owns - the same buffers it
//! is going to draw from - so a glyph's worth of deltas is charged to whoever asked for the glyph.

use crate::glyf::{Outline, Point};
use crate::reader::Reader;
use crate::tables::Face;
use crate::variations::{MAX_AXES, Normalised};
use crate::{Error, MAX_COMPOSITE_DEPTH, Malformed};

/// The side bearings and vertical origins every glyph's delta list ends with.
pub const PHANTOM_POINTS: usize = 4;

/// One point's delta, in font units.
pub type Delta = (i16, i16);

/// A 2.14 fixed-point one, which is the unit every tuple scalar is expressed in.
const ONE: i64 = 16384;

fn bad() -> Error {
	Error::Malformed(Malformed::InconsistentTable { table: *b"gvar" })
}

/// The profile's axis ceiling, met by a table rather than by a declaration.
fn font_parse_exceeded(asked: usize) -> opentype_profile::Unsupported {
	opentype_profile::Unsupported::Exceeded { limit: "variation axes", ceiling: opentype_profile::limits::VARIATION_AXES, asked: asked as u64 }
}

/// What a glyph's deltas ADDRESS: its own points, or the components it is built from.
#[derive(Clone, Copy)]
pub enum Geometry<'g> {
	/// A simple glyph, whose deltas move points and whose gaps are interpolated.
	Simple(&'g [Point]),
	/// A composite glyph, whose deltas move whole components and are never interpolated.
	Composite(usize),
}

impl Geometry<'_> {
	/// How many entries the glyph itself contributes, before the phantom points.
	pub fn len(&self) -> usize {
		match self {
			Geometry::Simple(points) => points.len(),
			Geometry::Composite(count) => *count,
		}
	}

	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}

	/// The whole delta list, phantom points included - the number the format counts in.
	fn total(&self) -> usize {
		self.len() + PHANTOM_POINTS
	}
}

/// Accumulate a glyph's variation deltas at a coordinate.
///
/// `deltas` and `touched` must both hold `geometry.total()` entries: the glyph's own, then the four
/// phantom points. Answers whether anything varied at all, so a caller can take the unvaried path
/// rather than adding zeroes to every point.
pub fn apply(face: &Face<'_>, glyph: u16, coordinates: &[Normalised], geometry: Geometry<'_>, deltas: &mut [Delta], touched: &mut [bool]) -> Result<bool, Error> {
	let total = geometry.total();
	if deltas.len() < total || touched.len() < total {
		return Err(bad());
	}
	for delta in deltas.iter_mut().take(total) {
		*delta = (0, 0);
	}
	let Some(gvar) = face.table(b"gvar")? else { return Ok(false) };

	let mut reader = gvar;
	let major = reader.u16().ok_or_else(bad)?;
	let _minor = reader.u16().ok_or_else(bad)?;
	if major != 1 {
		return Err(Error::Unsupported(opentype_profile::Unsupported::TableVersion { tag: *b"gvar", major, minor: 0 }));
	}
	let axis_count = reader.u16().ok_or_else(bad)? as usize;
	if axis_count > MAX_AXES {
		return Err(Error::Unsupported(font_parse_exceeded(axis_count)));
	}
	let shared_count = reader.u16().ok_or_else(bad)? as usize;
	let shared_at = reader.u32().ok_or_else(bad)? as usize;
	let glyph_count = reader.u16().ok_or_else(bad)? as usize;
	let flags = reader.u16().ok_or_else(bad)?;
	let data_at = reader.u32().ok_or_else(bad)? as usize;
	if glyph as usize >= glyph_count {
		// A glyph past the table's own count has no variation data, which is what a font with
		// variable letters and a static notdef looks like.
		return Ok(false);
	}

	// THE OFFSET ARRAY IS SHORT OR LONG AND ONE BIT SAYS WHICH. Reading it wrong halves or doubles
	// every offset in the table, which lands the parser in the middle of somebody else's tuple.
	let long = flags & 0x0001 != 0;
	let offsets_at = reader.position();
	let (start, end) = if long {
		let offsets = gvar.slice(offsets_at, (glyph_count + 1).checked_mul(4).ok_or_else(bad)?).ok_or_else(bad)?;
		(offsets.u32_at(glyph as usize).ok_or_else(bad)? as usize, offsets.u32_at(glyph as usize + 1).ok_or_else(bad)? as usize)
	} else {
		// The short form stores HALF the offset, as `loca` does, so every glyph's data is even.
		let offsets = gvar.slice(offsets_at, (glyph_count + 1).checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
		(offsets.u16_at(glyph as usize).ok_or_else(bad)? as usize * 2, offsets.u16_at(glyph as usize + 1).ok_or_else(bad)? as usize * 2)
	};
	if end <= start {
		// An empty range is a glyph that does not vary, which is ordinary rather than malformed.
		return Ok(false);
	}
	let glyph_at = data_at.checked_add(start).ok_or_else(bad)?;
	let data = gvar.slice(glyph_at, end - start).ok_or_else(bad)?;
	let shared = gvar.slice(shared_at, shared_count.checked_mul(axis_count.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?).ok_or_else(bad)?;

	let mut header = data;
	let count_field = header.u16().ok_or_else(bad)?;
	let tuple_count = (count_field & 0x0FFF) as usize;
	let has_shared_points = count_field & 0x8000 != 0;
	let serialised_at = header.u16().ok_or_else(bad)? as usize;

	// The serialised half holds the shared point numbers, when the glyph has any, and then each
	// tuple's own data one after another in tuple order.
	let mut serialised = data;
	serialised.seek(serialised_at).ok_or_else(bad)?;
	let mut shared_points: Option<(usize, usize)> = None;
	if has_shared_points {
		let count = packed_point_count(&mut serialised)?;
		if count != 0 {
			shared_points = Some((serialised.position(), count));
		}
		skip_packed_points(&mut serialised, count)?;
	}
	let mut tuple_data_at = serialised.position();

	let mut varied = false;
	let mut header_at = header.position();
	for _ in 0..tuple_count {
		let mut tuple = data;
		tuple.seek(header_at).ok_or_else(bad)?;
		let size = tuple.u16().ok_or_else(bad)? as usize;
		let index = tuple.u16().ok_or_else(bad)?;
		let embedded = index & 0x8000 != 0;
		let intermediate = index & 0x4000 != 0;
		let private = index & 0x2000 != 0;
		let shared_index = (index & 0x0FFF) as usize;

		// The peak: written into this tuple's header, or one of the tuples every glyph shares.
		let mut peak = [0i16; MAX_AXES];
		if embedded {
			for slot in peak.iter_mut().take(axis_count) {
				*slot = tuple.i16().ok_or_else(bad)?;
			}
		} else {
			if shared_index >= shared_count {
				return Err(bad());
			}
			for (axis, slot) in peak.iter_mut().take(axis_count).enumerate() {
				*slot = shared.u16_at(shared_index * axis_count + axis).ok_or_else(bad)? as i16;
			}
		}
		// AN INTERMEDIATE REGION STATES ITS OWN START AND END rather than taking them from the peak,
		// which is how a master applies over PART of an axis instead of all the way to its end. The
		// plain form is the peak's own side of zero, which is what the format means by leaving them
		// out.
		let mut lower = [0i16; MAX_AXES];
		let mut upper = [0i16; MAX_AXES];
		if intermediate {
			for slot in lower.iter_mut().take(axis_count) {
				*slot = tuple.i16().ok_or_else(bad)?;
			}
			for slot in upper.iter_mut().take(axis_count) {
				*slot = tuple.i16().ok_or_else(bad)?;
			}
		} else {
			for axis in 0..axis_count {
				lower[axis] = peak[axis].min(0);
				upper[axis] = peak[axis].max(0);
			}
		}
		header_at = tuple.position();

		let this_tuple_at = tuple_data_at;
		tuple_data_at = tuple_data_at.checked_add(size).ok_or_else(bad)?;
		let scalar = scalar_for(&peak[..axis_count], &lower[..axis_count], &upper[..axis_count], coordinates);
		if scalar == 0 {
			// A tuple that does not apply is SKIPPED, not added with a zero scalar: the same answer
			// and a great deal less reading, on a table where most tuples do not apply.
			continue;
		}
		let mut body = data;
		body.seek(this_tuple_at).ok_or_else(bad)?;

		// Which entries this tuple gives deltas for: its own list, the shared one, or all of them.
		let listed = if private {
			let count = packed_point_count(&mut body)?;
			let at = body.position();
			skip_packed_points(&mut body, count)?;
			if count == 0 { None } else { Some((at, count)) }
		} else {
			shared_points
		};
		// A count of zero is the format's shorthand for EVERY entry, phantom points included, and
		// that is also the count the x and y delta runs are measured by.
		let affected = match listed {
			Some((_, count)) => count,
			None => total,
		};
		apply_tuple(&Tuple { table: data, listed, affected }, &mut body, scalar, geometry, deltas, touched)?;
		varied = true;
	}
	Ok(varied)
}

/// How much of a tuple's deltas apply at a coordinate, in 2.14.
fn scalar_for(peak: &[i16], lower: &[i16], upper: &[i16], coordinates: &[Normalised]) -> i32 {
	let mut scalar: i64 = ONE;
	for axis in 0..peak.len() {
		let peak_value = peak[axis] as i64;
		if peak_value == 0 {
			// An axis this master does not speak about does not reduce it.
			continue;
		}
		let coordinate = coordinates.get(axis).copied().unwrap_or(0) as i64;
		if coordinate == peak_value {
			continue;
		}
		let (low, high) = (lower[axis] as i64, upper[axis] as i64);
		if coordinate <= low || coordinate >= high {
			// OUTSIDE THE REGION IT CONTRIBUTES NOTHING, not a reduced amount: a partially applied
			// master is an instance mixed out of two designs that were never meant to meet.
			return 0;
		}
		let factor = if coordinate < peak_value { ((coordinate - low) * ONE) / (peak_value - low) } else { ((high - coordinate) * ONE) / (high - peak_value) };
		scalar = (scalar * factor) / ONE;
	}
	scalar as i32
}

/// The count at the head of a packed point-number list. Zero is the format's shorthand for ALL.
fn packed_point_count(reader: &mut Reader<'_>) -> Result<usize, Error> {
	let first = reader.u8().ok_or_else(bad)? as usize;
	if first & 0x80 == 0 {
		return Ok(first);
	}
	let second = reader.u8().ok_or_else(bad)? as usize;
	Ok(((first & 0x7F) << 8) | second)
}

/// Step over a packed point-number list without decoding it.
fn skip_packed_points(reader: &mut Reader<'_>, count: usize) -> Result<(), Error> {
	let mut read = 0usize;
	while read < count {
		let control = reader.u8().ok_or_else(bad)?;
		let run = ((control & 0x7F) as usize) + 1;
		let words = control & 0x80 != 0;
		reader.skip(run.checked_mul(if words { 2 } else { 1 }).ok_or_else(bad)?).ok_or_else(bad)?;
		read = read.checked_add(run).ok_or_else(bad)?;
	}
	Ok(())
}

/// The point number at an index of a packed list.
///
/// THE LIST IS DELTA-ENCODED, so reading one number means walking to it - which is why this is a
/// function and not an index. Walking it is the price of not decoding the whole list into a buffer
/// the size of a glyph, in a parser that must not put a glyph on the stack.
fn packed_point_at(table: Reader<'_>, at: usize, count: usize, wanted: usize) -> Result<u16, Error> {
	let mut reader = table;
	reader.seek(at).ok_or_else(bad)?;
	let mut read = 0usize;
	let mut value: u32 = 0;
	while read < count {
		let control = reader.u8().ok_or_else(bad)?;
		let run = ((control & 0x7F) as usize) + 1;
		let words = control & 0x80 != 0;
		for _ in 0..run {
			let step = if words { reader.u16().ok_or_else(bad)? as u32 } else { reader.u8().ok_or_else(bad)? as u32 };
			value = value.checked_add(step).ok_or_else(bad)?;
			if read == wanted {
				return u16::try_from(value).map_err(|_| bad());
			}
			read += 1;
		}
	}
	Err(bad())
}

/// Where one tuple's serialised half is, and which entries it speaks about.
struct Tuple<'t> {
	/// The glyph's variation data, which every offset below is measured inside.
	table: Reader<'t>,
	/// The packed point-number list, or `None` when the tuple speaks about every entry.
	listed: Option<(usize, usize)>,
	/// How many entries this tuple gives deltas for, which is also how long each delta run is.
	affected: usize,
}

/// One tuple's deltas, scaled and accumulated, with the unreferenced points inferred.
fn apply_tuple(tuple: &Tuple<'_>, body: &mut Reader<'_>, scalar: i32, geometry: Geometry<'_>, deltas: &mut [Delta], touched: &mut [bool]) -> Result<(), Error> {
	let Tuple { table, listed, affected } = *tuple;
	let total = geometry.total();
	// The deltas: every x value, then every y value, each packed the same way. The y run starts
	// where the x run ends, which is only knowable by walking the x run.
	let x_at = body.position();
	let mut measure = *body;
	skip_packed_deltas(&mut measure, affected)?;
	let y_at = measure.position();

	for slot in touched.iter_mut().take(total) {
		*slot = false;
	}
	for index in 0..affected {
		let entry = match listed {
			Some((at, count)) => packed_point_at(table, at, count, index)? as usize,
			None => index,
		};
		if entry >= total {
			// A point number past the glyph's own points, phantom ones included, is a font that
			// contradicts itself; treating it as a wrap would write a delta onto another point.
			return Err(bad());
		}
		let dx = packed_delta_at(table, x_at, affected, index)?;
		let dy = packed_delta_at(table, y_at, affected, index)?;
		deltas[entry].0 = deltas[entry].0.saturating_add(scale(dx, scalar));
		deltas[entry].1 = deltas[entry].1.saturating_add(scale(dy, scalar));
		touched[entry] = true;
	}
	if listed.is_none() {
		// Every entry was given a delta, so there is nothing left to infer.
		return Ok(());
	}
	let Geometry::Simple(points) = geometry else {
		// A COMPOSITE IS NOT INTERPOLATED. Its entries are component origins, and there is no
		// coordinate to interpolate along between two of them - inventing one would move a component
		// the font did not ask to move.
		return Ok(());
	};
	// THE INFERENCE IS PER CONTOUR. A point is interpolated from its neighbours in ITS OWN contour;
	// reaching into the next one pulls two separate shapes together.
	let mut start = 0usize;
	for (index, point) in points.iter().enumerate() {
		if point.ends_contour {
			infer(points, deltas, touched, start, index);
			start = index + 1;
		}
	}
	if start < points.len() {
		// A trailing run with no end flag is a glyph whose last contour was not closed; it is still
		// a contour, and leaving it uninterpolated would tear exactly one shape per glyph.
		infer(points, deltas, touched, start, points.len() - 1);
	}
	Ok(())
}

/// One delta, reduced by a tuple's scalar and held inside a font unit.
fn scale(delta: i16, scalar: i32) -> i16 {
	let scaled = (delta as i64 * scalar as i64) / ONE;
	scaled.clamp(i16::MIN as i64, i16::MAX as i64) as i16
}

/// Interpolate the untouched points of one contour.
fn infer(points: &[Point], deltas: &mut [Delta], touched: &[bool], start: usize, end: usize) {
	if end < start {
		return;
	}
	let length = end - start + 1;
	// A CONTOUR NOTHING REFERENCED DOES NOT MOVE. There is nothing to interpolate from, and moving it
	// by the glyph's average would be a number the font never wrote down.
	if !touched[start..=end].iter().any(|touched| *touched) {
		return;
	}
	for index in start..=end {
		if touched[index] {
			continue;
		}
		// The nearest referenced point on either side, WRAPPING AROUND the contour - which is what
		// makes a contour a closed shape rather than a line with two ends.
		let mut before = None;
		let mut after = None;
		for step in 1..=length {
			let candidate = start + ((index - start) + length - step) % length;
			if touched[candidate] {
				before = Some(candidate);
				break;
			}
		}
		for step in 1..=length {
			let candidate = start + ((index - start) + step) % length;
			if touched[candidate] {
				after = Some(candidate);
				break;
			}
		}
		let (Some(before), Some(after)) = (before, after) else { continue };
		deltas[index].0 = interpolate(points[index].x, points[before].x, points[after].x, deltas[before].0, deltas[after].0);
		deltas[index].1 = interpolate(points[index].y, points[before].y, points[after].y, deltas[before].1, deltas[after].1);
	}
}

/// One coordinate of an inferred delta, as the format defines the rule.
///
/// OUTSIDE THE TWO REFERENCED POINTS THE NEARER ONE'S DELTA IS TAKEN WHOLE, and only between them is
/// it interpolated by position. Extrapolating instead would move a point FURTHER than either of its
/// neighbours went, which shows on the outline as a spike where the font has a straight edge.
///
/// TWO REFERENCES AT THE SAME COORDINATE THAT MOVED DIFFERENTLY SAY NOTHING about a point between
/// them - there is no direction to interpolate along - so the answer is zero rather than one of the
/// two picked arbitrarily. When they moved the SAME amount there is no ambiguity and that is the
/// answer.
fn interpolate(value: i16, before: i16, after: i16, before_delta: i16, after_delta: i16) -> i16 {
	let (low, low_delta, high, high_delta) = if before <= after { (before, before_delta, after, after_delta) } else { (after, after_delta, before, before_delta) };
	if low == high {
		return if low_delta == high_delta { low_delta } else { 0 };
	}
	if value <= low {
		return low_delta;
	}
	if value >= high {
		return high_delta;
	}
	let along = (value as i64 - low as i64) * ONE / (high as i64 - low as i64);
	let range = high_delta as i64 - low_delta as i64;
	(low_delta as i64 + (range * along) / ONE) as i16
}

/// Step over a packed delta list.
fn skip_packed_deltas(reader: &mut Reader<'_>, count: usize) -> Result<(), Error> {
	let mut read = 0usize;
	while read < count {
		let control = reader.u8().ok_or_else(bad)?;
		let run = ((control & 0x3F) as usize) + 1;
		// The two high bits say whether the run is zeros, words, or bytes - and a zero run occupies
		// no bytes at all, which is how a font says nothing here moved.
		let width: usize = if control & 0x80 != 0 {
			0
		} else if control & 0x40 != 0 {
			2
		} else {
			1
		};
		reader.skip(run.checked_mul(width).ok_or_else(bad)?).ok_or_else(bad)?;
		read = read.checked_add(run).ok_or_else(bad)?;
	}
	Ok(())
}

/// One delta of a packed list, by index.
fn packed_delta_at(table: Reader<'_>, at: usize, count: usize, wanted: usize) -> Result<i16, Error> {
	let mut reader = table;
	reader.seek(at).ok_or_else(bad)?;
	let mut read = 0usize;
	while read < count {
		let control = reader.u8().ok_or_else(bad)?;
		let run = ((control & 0x3F) as usize) + 1;
		let zeros = control & 0x80 != 0;
		let words = control & 0x40 != 0;
		for _ in 0..run {
			let value = if zeros {
				0
			} else if words {
				reader.i16().ok_or_else(bad)?
			} else {
				reader.i8().ok_or_else(bad)? as i16
			};
			if read == wanted {
				return Ok(value);
			}
			read += 1;
		}
	}
	Err(bad())
}

/// The buffers a varied walk needs, all owned by the caller.
///
/// ONE GLYPH'S WORTH, CHARGED TO WHOEVER ASKED FOR THE GLYPH. A composite spends part of them on its
/// own components and hands the REST to each component it walks, so nesting is bounded by the buffer
/// the caller was willing to pay for rather than by a number this module invented.
pub struct Scratch<'s> {
	pub points: &'s mut [Point],
	pub deltas: &'s mut [Delta],
	pub touched: &'s mut [bool],
}

impl Scratch<'_> {
	/// The same buffers, borrowed for a shorter while - which is how one walk hands them to the walk
	/// it is nested inside without giving them away.
	fn reborrow(&mut self) -> Scratch<'_> {
		Scratch { points: &mut *self.points, deltas: &mut *self.deltas, touched: &mut *self.touched }
	}
}

/// Walk one glyph's outline at a variation coordinate.
///
/// The same walk `glyf` performs, with every point moved by its delta. A face with no `gvar`, or a
/// coordinate no tuple applies at, produces exactly the default outline - so a caller does not need
/// to ask first which kind of face it has.
pub fn walk_varied(face: &Face<'_>, glyph: u16, coordinates: &[Normalised], scratch: Scratch<'_>, outline: &mut impl Outline) -> Result<(), Error> {
	walk_varied_at(face, glyph, coordinates, 0, (0, 0), scratch, outline)
}

fn walk_varied_at(face: &Face<'_>, glyph: u16, coordinates: &[Normalised], depth: u8, at: (i16, i16), scratch: Scratch<'_>, outline: &mut dyn Outline) -> Result<(), Error> {
	if depth > MAX_COMPOSITE_DEPTH {
		return Err(Error::Malformed(Malformed::BadGlyph { glyph }));
	}
	if crate::glyf::is_composite(face, glyph)? {
		return composite_varied(face, glyph, coordinates, depth, at, scratch, outline);
	}
	let (dx, dy) = at;
	let Scratch { points, deltas, touched } = scratch;
	// A simple glyph is collected first because the inference needs its POINTS: a delta list that
	// names three of them is interpolated along the outline between those three, and the outline is
	// not knowable from the delta list alone.
	let mut collect = Collect { points, filled: 0, overflowed: false };
	crate::glyf::walk(face, glyph, &mut collect)?;
	if collect.overflowed {
		return Err(Error::Malformed(Malformed::BadGlyph { glyph }));
	}
	let filled = collect.filled;
	let (points, _) = points.split_at_mut(filled);
	if filled == 0 {
		// An empty glyph - a space - has nothing to move.
		return Ok(());
	}
	let total = filled + PHANTOM_POINTS;
	if deltas.len() < total || touched.len() < total {
		return Err(bad());
	}
	let varied = apply(face, glyph, coordinates, Geometry::Simple(points), deltas, touched)?;
	for (index, point) in points.iter().enumerate() {
		let (delta_x, delta_y) = if varied { deltas[index] } else { (0, 0) };
		let moved = Point { x: point.x.saturating_add(delta_x).saturating_add(dx), y: point.y.saturating_add(delta_y).saturating_add(dy), kind: point.kind, ends_contour: point.ends_contour };
		if !outline.point(moved) {
			return Ok(());
		}
	}
	Ok(())
}

/// A composite glyph at a coordinate: every component placed by its own delta.
fn composite_varied(face: &Face<'_>, glyph: u16, coordinates: &[Normalised], depth: u8, at: (i16, i16), scratch: Scratch<'_>, outline: &mut dyn Outline) -> Result<(), Error> {
	let (dx, dy) = at;
	let Scratch { points, deltas, touched } = scratch;
	let count = crate::glyf::components(face, glyph, &mut |_, _| Ok(true))?;
	let total = count + PHANTOM_POINTS;
	if deltas.len() < total || touched.len() < total {
		return Err(bad());
	}
	let varied = apply(face, glyph, coordinates, Geometry::Composite(count), deltas, touched)?;
	// THE PARENT'S DELTAS ARE HELD BACK FROM THE CHILDREN. Each component is walked with what is
	// left of the buffers, so a component cannot overwrite the delta that places it - and a nesting
	// deeper than the caller's buffers allows is refused rather than silently misplaced.
	let (mine, rest_deltas) = deltas.split_at_mut(total);
	let (_, rest_touched) = touched.split_at_mut(total);
	let mut rest = Scratch { points, deltas: rest_deltas, touched: rest_touched };
	let mut failure: Option<Error> = None;
	let mut stopped = false;
	crate::glyf::components(face, glyph, &mut |index, component| {
		if stopped {
			return Ok(false);
		}
		let (delta_x, delta_y) = if varied { mine[index] } else { (0, 0) };
		let placed = (dx.saturating_add(component.dx).saturating_add(delta_x), dy.saturating_add(component.dy).saturating_add(delta_y));
		let result = walk_varied_at(face, component.glyph, coordinates, depth + 1, placed, rest.reborrow(), outline);
		if let Err(error) = result {
			failure = Some(error);
			stopped = true;
			return Ok(false);
		}
		Ok(true)
	})?;
	match failure {
		Some(error) => Err(error),
		None => Ok(()),
	}
}

/// Collect a simple glyph's points into a buffer the caller owns.
struct Collect<'p> {
	points: &'p mut [Point],
	filled: usize,
	overflowed: bool,
}

impl Outline for Collect<'_> {
	fn point(&mut self, point: Point) -> bool {
		match self.points.get_mut(self.filled) {
			Some(slot) => {
				*slot = point;
				self.filled += 1;
				true
			}
			None => {
				// A GLYPH BIGGER THAN THE BUFFER IS A REFUSAL, not a truncated outline: half a glyph
				// drawn is a shape the font does not contain.
				self.overflowed = true;
				false
			}
		}
	}
}

/// The advance variation a font without `HVAR` carries in its phantom points.
///
/// `HVAR` IS NOT ALWAYS THERE, and its absence does not mean the advances do not vary: the first two
/// phantom points ARE the left side bearing and the advance, and a font that varies its widths only
/// through `gvar` is common enough that treating a missing `HVAR` as "nothing varies" mis-spaces the
/// whole face. `deltas` and `touched` must hold the glyph's entries plus [`PHANTOM_POINTS`].
pub fn phantom_advance_delta(face: &Face<'_>, glyph: u16, coordinates: &[Normalised], scratch: Scratch<'_>) -> Result<i32, Error> {
	let Scratch { points, deltas, touched } = scratch;
	let geometry_length = if crate::glyf::is_composite(face, glyph)? {
		crate::glyf::components(face, glyph, &mut |_, _| Ok(true))?
	} else {
		let mut collect = Collect { points, filled: 0, overflowed: false };
		crate::glyf::walk(face, glyph, &mut collect)?;
		if collect.overflowed {
			return Err(Error::Malformed(Malformed::BadGlyph { glyph }));
		}
		collect.filled
	};
	let geometry = if crate::glyf::is_composite(face, glyph)? { Geometry::Composite(geometry_length) } else { Geometry::Simple(&points[..geometry_length]) };
	if !apply(face, glyph, coordinates, geometry, deltas, touched)? {
		return Ok(0);
	}
	// The advance is the distance between the two horizontal phantom points, so it varies by the
	// DIFFERENCE of their deltas rather than by either one alone.
	let left = deltas[geometry_length].0 as i32;
	let right = deltas[geometry_length + 1].0 as i32;
	Ok(right - left)
}
