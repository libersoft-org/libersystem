//! `glyf`: the quadratic outlines, simple and composite.
//!
//! THE TABLE IS A WEB OF COUNTS THAT INDEX EACH OTHER. A glyph says how many contours it has, each
//! contour says which point ends it, the flags run is compressed so its length is not known until it
//! has been read, and the coordinates are deltas whose widths come from those flags. Every one of
//! those is an opportunity for a crafted font to make a parser read past something, and every one of
//! them is checked here against the table's own length.
//!
//! A COMPOSITE GLYPH REFERS TO OTHER GLYPHS, so it is the one place recursion enters a font parser.
//! The depth is bounded by `MAX_COMPOSITE_DEPTH` and a component that refers to its own glyph is
//! refused: a font whose glyph 5 is built out of glyph 5 is a loop, and following it is how a parser
//! is made to recurse until the stack ends.

use crate::reader::Reader;
use crate::tables::Face;
use crate::{Error, MAX_COMPOSITE_DEPTH, Malformed};

/// WHAT A POINT IS ON THE CURVE, and which curve.
///
/// AN ENUM AND NOT A BOOLEAN, because this system has TWO outline formats and they draw different
/// curves. `glyf` is quadratic - one control point between two on-curve ones - and `CFF`/`CFF2` are
/// CUBIC, with two. A boolean `on_curve` cannot tell two quadratic controls in a row, which imply an
/// on-curve point half way between them, from the two controls of one cubic; a consumer that read a
/// cubic as a pair of quadratics draws a different shape, and nothing in the data says so. An enum
/// makes a consumer that handles only one of them REFUSE rather than draw the wrong curve.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PointKind {
	/// A point the curve passes through.
	OnCurve,
	/// The single control point of a quadratic segment, as `glyf` draws.
	Quadratic,
	/// One of the two control points of a cubic segment, as `CFF` and `CFF2` draw.
	Cubic,
}

/// One point of an outline, in font units.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Point {
	pub x: i16,
	pub y: i16,
	pub kind: PointKind,
	/// The last point of a contour, which is where it closes back to its first.
	pub ends_contour: bool,
}

impl Point {
	/// Whether the curve passes through this point, for a caller that only needs to know that much.
	pub const fn on_curve(&self) -> bool {
		matches!(self.kind, PointKind::OnCurve)
	}
}

/// What a glyph's outline is made of, as it is walked.
pub trait Outline {
	/// One point of the current contour. `false` stops the walk, which is how a caller applies a
	/// bound of its own without this one inventing a number.
	fn point(&mut self, point: Point) -> bool;
}

/// Walk one glyph's outline, resolving composites.
///
/// THE TRANSFORM OF A COMPONENT IS APPLIED AS THE FORMAT DEFINES IT, in 2.14 fixed point, and a
/// component placed by matching POINTS rather than by offset is refused rather than misplaced: it is
/// a rare form, it needs the parent's points resolved before the child's, and placing it wrong moves
/// an accent onto the wrong part of a letter.
pub fn walk(face: &Face<'_>, glyph: u16, outline: &mut impl Outline) -> Result<(), Error> {
	// THE EXPANDED POINT COUNT IS COUNTED ACROSS THE WHOLE WALK, not per glyph description. Five
	// levels of nesting MULTIPLY: a composite of ten composites of ten composites is a thousand
	// glyphs' worth of points inside a structure whose every offset is in range, and a per-glyph cap
	// would pass every one of them.
	let mut points = 0usize;
	walk_at(face, glyph, 0, 0, 0, &mut points, outline)
}

fn walk_at(face: &Face<'_>, glyph: u16, depth: u8, dx: i16, dy: i16, points: &mut usize, outline: &mut impl Outline) -> Result<(), Error> {
	if depth > MAX_COMPOSITE_DEPTH {
		return Err(Error::Unsupported(crate::Unsupported::Exceeded { limit: "composite depth", ceiling: opentype_profile::limits::COMPOSITE_DEPTH, asked: depth as u64 }));
	}
	let (offset, length) = face.glyph_range(glyph)?;
	if length == 0 {
		// An empty glyph - a space - is not an error, and reading it as one is how a font with a
		// space in it stops rendering.
		return Ok(());
	}
	let table = face.outlines()?;
	let description = table.slice(offset, length).ok_or(Error::Malformed(Malformed::BadGlyph { glyph }))?;
	let mut reader = description;
	let contours = reader.i16().ok_or(Error::Malformed(Malformed::BadGlyph { glyph }))?;
	reader.skip(8).ok_or(Error::Malformed(Malformed::BadGlyph { glyph }))?;
	if contours >= 0 { simple(description, contours as usize, glyph, dx, dy, points, outline) } else { composite(face, description, glyph, depth, (dx, dy), points, outline) }
}

/// A simple glyph: contours, flags, and delta-encoded coordinates.
///
/// THREE READERS, NO BUFFER. The flags are run-length compressed and the coordinates are deltas
/// whose WIDTHS come from those flags, so the obvious implementation decodes the flags into an
/// array and then reads the two coordinate arrays - which is a hundred and ninety kilobytes of stack
/// at the format's own maximum point count, in a function that RECURSES for composite glyphs. That
/// is a stack overflow a crafted font can ask for, and it is the shape this item exists to refuse.
///
/// So the flags are walked TWICE instead: once to measure how many bytes the x deltas take, and then
/// with three readers advancing in step - flags, x, y - emitting each point as it is decoded. The
/// cost is reading one compressed stream a second time; the gain is constant stack.
fn simple(description: Reader<'_>, contours: usize, glyph: u16, dx: i16, dy: i16, expanded: &mut usize, outline: &mut impl Outline) -> Result<(), Error> {
	let bad = || Error::Malformed(Malformed::BadGlyph { glyph });
	// The contour end points, which also say how many points there are.
	let ends = description.slice(10, contours.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
	let mut last_end = 0usize;
	for index in 0..contours {
		let end = ends.u16_at(index).ok_or_else(bad)? as usize;
		if index > 0 && end < last_end {
			// Ends must ascend, or the contour lengths they imply are negative.
			return Err(bad());
		}
		last_end = end;
	}
	let points = if contours == 0 { 0 } else { last_end.checked_add(1).ok_or_else(bad)? };
	*expanded = expanded.checked_add(points).ok_or_else(bad)?;
	if *expanded > crate::MAX_GLYPH_POINTS {
		return Err(Error::Unsupported(crate::Unsupported::Exceeded { limit: "composite points", ceiling: opentype_profile::limits::COMPOSITE_POINTS, asked: *expanded as u64 }));
	}
	// The instructions, which this profile does not execute and therefore only measures.
	let after_ends = 10usize.checked_add(contours.checked_mul(2).ok_or_else(bad)?).ok_or_else(bad)?;
	let mut reader = description;
	reader.seek(after_ends).ok_or_else(bad)?;
	let instruction_length = reader.u16().ok_or_else(bad)? as usize;
	let flags_at = reader.position().checked_add(instruction_length).ok_or_else(bad)?;

	// PASS ONE: measure. How long the flag stream is, and how many bytes the x deltas take.
	let mut flags = description;
	flags.seek(flags_at).ok_or_else(bad)?;
	let mut x_bytes = 0usize;
	let mut counted = 0usize;
	while counted < points {
		let flag = flags.u8().ok_or_else(bad)?;
		let repeats = if flag & 0x08 != 0 { flags.u8().ok_or_else(bad)? as usize } else { 0 };
		let run = repeats.checked_add(1).ok_or_else(bad)?;
		if counted.checked_add(run).ok_or_else(bad)? > points {
			// A repeat count that runs past the point count is a font that contradicts itself.
			return Err(bad());
		}
		counted += run;
		let width: usize = if flag & 0x02 != 0 {
			1
		} else if flag & 0x10 != 0 {
			0
		} else {
			2
		};
		x_bytes = x_bytes.checked_add(width.checked_mul(run).ok_or_else(bad)?).ok_or_else(bad)?;
	}
	let x_at = flags.position();
	let y_at = x_at.checked_add(x_bytes).ok_or_else(bad)?;

	// PASS TWO: three readers in step.
	let mut flags = description;
	flags.seek(flags_at).ok_or_else(bad)?;
	let mut x_reader = description;
	x_reader.seek(x_at).ok_or_else(bad)?;
	let mut y_reader = description;
	y_reader.seek(y_at).ok_or_else(bad)?;

	let mut current_flag = 0u8;
	let mut repeats_left = 0usize;
	let mut x = 0i32;
	let mut y = 0i32;
	let mut contour = 0usize;
	let mut contour_end = if contours == 0 { None } else { Some(ends.u16_at(0).ok_or_else(bad)? as usize) };
	for index in 0..points {
		if repeats_left > 0 {
			repeats_left -= 1;
		} else {
			current_flag = flags.u8().ok_or_else(bad)?;
			if current_flag & 0x08 != 0 {
				repeats_left = flags.u8().ok_or_else(bad)? as usize;
			}
		}
		let delta_x = if current_flag & 0x02 != 0 {
			let value = x_reader.u8().ok_or_else(bad)? as i32;
			if current_flag & 0x10 != 0 { value } else { -value }
		} else if current_flag & 0x10 != 0 {
			0
		} else {
			x_reader.i16().ok_or_else(bad)? as i32
		};
		let delta_y = if current_flag & 0x04 != 0 {
			let value = y_reader.u8().ok_or_else(bad)? as i32;
			if current_flag & 0x20 != 0 { value } else { -value }
		} else if current_flag & 0x20 != 0 {
			0
		} else {
			y_reader.i16().ok_or_else(bad)? as i32
		};
		x = x.checked_add(delta_x).ok_or_else(bad)?;
		y = y.checked_add(delta_y).ok_or_else(bad)?;
		let ends_contour = contour_end == Some(index);
		let point = Point { x: i16::try_from(x).map_err(|_| bad())?.saturating_add(dx), y: i16::try_from(y).map_err(|_| bad())?.saturating_add(dy), kind: if current_flag & 0x01 != 0 { PointKind::OnCurve } else { PointKind::Quadratic }, ends_contour };
		if !outline.point(point) {
			return Ok(());
		}
		if ends_contour {
			contour += 1;
			contour_end = if contour < contours { Some(ends.u16_at(contour).ok_or_else(bad)? as usize) } else { None };
		}
	}
	Ok(())
}

/// One component of a composite glyph, as its record places it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Component {
	pub glyph: u16,
	pub dx: i16,
	pub dy: i16,
}

/// Whether a glyph is built out of other glyphs.
///
/// THE TWO KINDS ARE ADDRESSED DIFFERENTLY BY EVERYTHING ELSE - variation deltas reach a simple
/// glyph's POINTS and a composite's COMPONENTS - so the question is asked here, once, off the
/// glyph's own header rather than guessed from what a walk produced.
pub fn is_composite(face: &Face<'_>, glyph: u16) -> Result<bool, Error> {
	let (offset, length) = face.glyph_range(glyph)?;
	if length == 0 {
		return Ok(false);
	}
	let table = face.outlines()?;
	let description = table.slice(offset, length).ok_or(Error::Malformed(Malformed::BadGlyph { glyph }))?;
	let mut reader = description;
	let contours = reader.i16().ok_or(Error::Malformed(Malformed::BadGlyph { glyph }))?;
	Ok(contours < 0)
}

/// Walk the components of a composite glyph, answering how many there were.
///
/// `each` answers `false` to stop early, which is how a caller that only needs the count avoids
/// paying for the rest. A simple glyph has no components and answers zero rather than an error: it
/// is a fair question to ask of any glyph.
pub fn components(face: &Face<'_>, glyph: u16, each: &mut dyn FnMut(usize, Component) -> Result<bool, Error>) -> Result<usize, Error> {
	let (offset, length) = face.glyph_range(glyph)?;
	if length == 0 {
		return Ok(0);
	}
	let table = face.outlines()?;
	let description = table.slice(offset, length).ok_or(Error::Malformed(Malformed::BadGlyph { glyph }))?;
	let mut reader = description;
	let contours = reader.i16().ok_or(Error::Malformed(Malformed::BadGlyph { glyph }))?;
	if contours >= 0 {
		return Ok(0);
	}
	components_of(description, glyph, each)
}

/// A composite glyph: other glyphs, each with a placement.
fn composite(face: &Face<'_>, description: Reader<'_>, glyph: u16, depth: u8, at: (i16, i16), points: &mut usize, outline: &mut impl Outline) -> Result<(), Error> {
	let (dx, dy) = at;
	components_of(description, glyph, &mut |_, component| {
		walk_at(face, component.glyph, depth + 1, dx.saturating_add(component.dx), dy.saturating_add(component.dy), points, outline)?;
		Ok(true)
	})?;
	Ok(())
}

/// The component records, read once and handed out - so the placement rules below are stated in ONE
/// place rather than once per caller that needs to know where a component sits.
fn components_of(description: Reader<'_>, glyph: u16, each: &mut dyn FnMut(usize, Component) -> Result<bool, Error>) -> Result<usize, Error> {
	let bad = || Error::Malformed(Malformed::BadGlyph { glyph });
	let mut reader = description;
	reader.seek(10).ok_or_else(bad)?;
	let mut count = 0usize;
	loop {
		let flags = reader.u16().ok_or_else(bad)?;
		let component = reader.u16().ok_or_else(bad)?;
		// A COMPONENT THAT IS ITS OWN PARENT IS A LOOP, and the depth bound alone would only make it
		// a slow one. Refused by name so the report says what the font did.
		if component == glyph {
			return Err(bad());
		}
		let (offset_x, offset_y) = if flags & 0x0001 != 0 {
			// Words.
			let x = reader.i16().ok_or_else(bad)?;
			let y = reader.i16().ok_or_else(bad)?;
			(x, y)
		} else {
			let x = reader.i8().ok_or_else(bad)? as i16;
			let y = reader.i8().ok_or_else(bad)? as i16;
			(x, y)
		};
		// ARGS_ARE_XY_VALUES: when it is clear, the two numbers are POINT INDICES to match rather
		// than an offset. Refused rather than treated as an offset, which would move an accent onto
		// the wrong part of the letter.
		if flags & 0x0002 == 0 {
			return Err(bad());
		}
		// The transform, which this walk does not apply: a scaled component needs the points
		// transformed rather than translated, and silently dropping the scale would draw it at the
		// wrong size. Refused until the renderer that needs it asks for it.
		if flags & 0x0008 != 0 {
			reader.skip(2).ok_or_else(bad)?;
			return Err(bad());
		}
		if flags & 0x0040 != 0 {
			reader.skip(4).ok_or_else(bad)?;
			return Err(bad());
		}
		if flags & 0x0080 != 0 {
			reader.skip(8).ok_or_else(bad)?;
			return Err(bad());
		}
		let index = count;
		count += 1;
		let more = flags & 0x0020 != 0;
		if !each(index, Component { glyph: component, dx: offset_x, dy: offset_y })? {
			return Ok(count);
		}
		if !more {
			break;
		}
	}
	Ok(count)
}
