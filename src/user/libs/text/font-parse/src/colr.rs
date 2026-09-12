//! `COLR`: a colour glyph, which is a GRAPH rather than a shape.
//!
//! VERSION 0 IS A LIST AND VERSION 1 IS A TREE, and that is the whole difference. A version 0 glyph
//! is layers of ordinary outlines, each with a palette colour; a version 1 glyph is a paint graph -
//! gradients, transforms, clips and composites, with paints referring to other paints and to other
//! colour glyphs. A graph over untrusted input is a cycle waiting to be followed, which is why the
//! depth and the node count are the profile's frozen numbers and why a glyph that revisits itself is
//! refused rather than followed.
//!
//! THE OFFSETS ARE TWENTY-FOUR BITS, and that is the trap. Almost every offset inside a paint is
//! three bytes rather than two or four; reading one as a `u16` lands two-thirds of the way into the
//! table and reading it as a `u32` swallows the byte after it. A parser written from the shape of the
//! other tables gets every paint wrong.
//!
//! THE VALUES ARE LEFT IN THE FORMAT'S OWN FIXED POINT. An angle and a scale are 2.14, a transform is
//! 16.16, and a coordinate is a font unit; converting them here would put a rounding decision in a
//! parser, which is the layer with the least idea what the result is for.
//!
//! WHAT THIS DOES NOT DO IS DRAW. It walks the graph and hands each node to a consumer. Rasterising a
//! gradient is the renderer's, and a parser that tried would be deciding what a colour space is.

use crate::reader::Reader;
use crate::tables::Face;
use crate::{Error, Malformed, Unsupported};

fn bad() -> Error {
	Error::Malformed(Malformed::InconsistentTable { table: *b"COLR" })
}

/// One node of a colour glyph's paint graph, with its values in the format's own fixed point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Paint<'a> {
	/// A run of layers in the layer list, painted in order.
	Layers { first: u32, count: u8 },
	/// A palette entry at an alpha, in 2.14.
	Solid { palette: u16, alpha: i16 },
	/// A gradient between two points, with a third that rotates it.
	LinearGradient { stops: Stops<'a>, x0: i16, y0: i16, x1: i16, y1: i16, x2: i16, y2: i16 },
	/// A gradient between two circles.
	RadialGradient { stops: Stops<'a>, x0: i16, y0: i16, radius0: u16, x1: i16, y1: i16, radius1: u16 },
	/// An angular gradient about a centre, with its angles in 2.14 turns.
	SweepGradient { stops: Stops<'a>, centre_x: i16, centre_y: i16, start_angle: i16, end_angle: i16 },
	/// A glyph outline used as a CLIP for the paint below it - not as a shape to fill.
	Glyph { glyph: u16 },
	/// Another colour glyph, by glyph id. The one place the graph leaves this glyph.
	ColrGlyph { glyph: u16 },
	/// An affine transform of the paint below, in 16.16.
	Transform { transform: [i32; 6] },
	/// A translation, in font units.
	Translate { dx: i16, dy: i16 },
	/// A scale about a centre, in 2.14. A uniform scale states the same value twice.
	Scale { x: i16, y: i16, centre_x: i16, centre_y: i16 },
	/// A rotation about a centre, in 2.14 turns.
	Rotate { angle: i16, centre_x: i16, centre_y: i16 },
	/// A skew about a centre, in 2.14 turns.
	Skew { x_angle: i16, y_angle: i16, centre_x: i16, centre_y: i16 },
	/// Two paints under one composite mode - the source first, then the backdrop.
	Composite { mode: u8 },
}

/// Where a gradient's colour stops are, and how it continues past them.
///
/// A HANDLE RATHER THAN THE STOPS THEMSELVES, because a colour line has as many stops as the font
/// says and this parser does not allocate. The consumer reads them through [`Colr::stops`], which is
/// also where the bound on how many it will read lives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stops<'a> {
	/// THE SPACE THE OFFSET IS IN, carried rather than assumed. A colour line's offset is from its
	/// PAINT's start, and a paint lives either in the base glyph list or in the layer list - two
	/// different origins. Resolving it against the table would land the reader in whichever of the two
	/// the fixture happened not to use.
	base: Reader<'a>,
	at: usize,
	/// Whether the stops carry variation indices, which makes each one four bytes longer.
	varies: bool,
}

/// One colour stop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stop {
	/// Where along the gradient, in 2.14.
	pub offset: i16,
	pub palette: u16,
	/// In 2.14.
	pub alpha: i16,
}

/// How a gradient continues past its last stop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Extend {
	Pad,
	Repeat,
	Reflect,
}

/// What walks a paint graph.
pub trait Paints {
	/// One node, at its depth in the graph. Answer `false` to stop descending into it, which is how a
	/// consumer applies a bound of its own without this one inventing a number.
	fn paint(&mut self, paint: Paint<'_>, depth: u8) -> bool;
}

/// A face's `COLR` table.
pub struct Colr<'a> {
	version: u16,
	/// Version 0's base glyph records, and how many.
	base_records: Option<(Reader<'a>, u16)>,
	layer_records: Option<(Reader<'a>, u16)>,
	/// Version 1's base glyph list and layer list, each sliced from its own offset because every
	/// offset inside them is measured from that start rather than from the table's.
	base_list: Option<Reader<'a>>,
	layer_list: Option<Reader<'a>>,
}

/// Which colour form a glyph has, for a caller that must choose between them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Form {
	/// Version 0's layers: ordinary outlines, each with a palette colour.
	Layers,
	/// Version 1's paint graph.
	PaintGraph,
}

/// One version 0 layer: an ordinary outline and the palette entry it is filled with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layer {
	pub glyph: u16,
	pub palette: u16,
}

/// How many colour stops this reads out of one gradient.
///
/// A BOUND BECAUSE THE TABLE IS INPUT. A gradient with more stops than this is a font describing
/// something other than a gradient, and reading them all is work a font should not be able to ask for.
pub const MAX_STOPS: usize = 256;

impl<'a> Colr<'a> {
	/// Read a face's `COLR`, or `None` for a face with none.
	pub fn of(face: &Face<'a>) -> Result<Option<Self>, Error> {
		let Some(table) = face.table(b"COLR")? else { return Ok(None) };
		let mut reader = table;
		let version = reader.u16().ok_or_else(bad)?;
		if version > 1 {
			return Err(Error::Unsupported(Unsupported::TableVersion { tag: *b"COLR", major: version, minor: 0 }));
		}
		let base_count = reader.u16().ok_or_else(bad)?;
		let base_at = reader.u32().ok_or_else(bad)? as usize;
		let layers_at = reader.u32().ok_or_else(bad)? as usize;
		let layer_count = reader.u16().ok_or_else(bad)?;
		let base_records = match base_at {
			0 => None,
			at => Some((table.slice(at, (base_count as usize).checked_mul(6).ok_or_else(bad)?).ok_or_else(bad)?, base_count)),
		};
		let layer_records = match layers_at {
			0 => None,
			at => Some((table.slice(at, (layer_count as usize).checked_mul(4).ok_or_else(bad)?).ok_or_else(bad)?, layer_count)),
		};
		let (mut base_list, mut layer_list) = (None, None);
		if version == 1 {
			let base_list_at = reader.u32().ok_or_else(bad)? as usize;
			let layer_list_at = reader.u32().ok_or_else(bad)? as usize;
			if base_list_at != 0 {
				base_list = Some(table.slice(base_list_at, table.len().checked_sub(base_list_at).ok_or_else(bad)?).ok_or_else(bad)?);
			}
			if layer_list_at != 0 {
				layer_list = Some(table.slice(layer_list_at, table.len().checked_sub(layer_list_at).ok_or_else(bad)?).ok_or_else(bad)?);
			}
		}
		Ok(Some(Self { version, base_records, layer_records, base_list, layer_list }))
	}

	pub fn version(&self) -> u16 {
		self.version
	}

	/// WHICH COLOUR FORM A GLYPH HAS, without walking anything.
	///
	/// ONE READER FOR THIS TABLE AND NOT TWO. A run says which form each of its glyphs is, and that
	/// question is a lookup in a sorted record list rather than a decode - but answering it with a
	/// second, smaller reader beside this one is how the two come to disagree about a font. This is
	/// that lookup, here, where the table is already open.
	///
	/// THE RICHER FORM WINS where a face carries both, because a face that carries a paint graph AND
	/// a layer list for one glyph means the graph and drew the layers for consumers that cannot paint.
	pub fn form_of(&self, glyph: u16) -> Result<Option<Form>, Error> {
		if let Some(list) = self.base_list {
			let mut reader = list;
			let count = reader.u32().ok_or_else(bad)? as usize;
			let records_at = reader.position();
			let records = list.slice(records_at, count.checked_mul(6).ok_or_else(bad)?).ok_or_else(bad)?;
			if search(&records, count, glyph)?.is_some() {
				return Ok(Some(Form::PaintGraph));
			}
		}
		if let Some((records, count)) = self.base_records
			&& search(&records, count as usize, glyph)?.is_some()
		{
			return Ok(Some(Form::Layers));
		}
		Ok(None)
	}

	/// Version 0's layers for a glyph: the outlines it is drawn out of, in order.
	///
	/// ANSWERS HOW MANY, and writes them into a buffer the caller owns. A glyph with more layers than
	/// the buffer holds is a REFUSAL rather than a truncated glyph: half a colour glyph drawn is a
	/// shape the font does not contain.
	pub fn layers(&self, glyph: u16, into: &mut [Layer]) -> Result<Option<usize>, Error> {
		let (Some((records, count)), Some((layers, layer_count))) = (self.base_records, self.layer_records) else {
			return Ok(None);
		};
		let Some(index) = search(&records, count as usize, glyph)? else { return Ok(None) };
		let first = records.u16_at(index * 3 + 1).ok_or_else(bad)? as usize;
		let number = records.u16_at(index * 3 + 2).ok_or_else(bad)? as usize;
		if first.checked_add(number).ok_or_else(bad)? > layer_count as usize {
			return Err(bad());
		}
		if number > into.len() {
			return Err(bad());
		}
		for (slot, layer) in into.iter_mut().enumerate().take(number) {
			*layer = Layer { glyph: layers.u16_at((first + slot) * 2).ok_or_else(bad)?, palette: layers.u16_at((first + slot) * 2 + 1).ok_or_else(bad)? };
		}
		Ok(Some(number))
	}

	/// Walk version 1's paint graph for a glyph.
	///
	/// A GRAPH OVER UNTRUSTED INPUT IS A CYCLE WAITING TO BE FOLLOWED. The depth and the node count
	/// are the profile's, and both refuse by name: a bounded depth over an unbounded breadth is still
	/// unbounded work, which is why there are two.
	pub fn walk(&self, glyph: u16, paints: &mut impl Paints) -> Result<bool, Error> {
		let Some(list) = self.base_list else { return Ok(false) };
		let mut reader = list;
		let count = reader.u32().ok_or_else(bad)? as usize;
		let records_at = reader.position();
		let records = list.slice(records_at, count.checked_mul(6).ok_or_else(bad)?).ok_or_else(bad)?;
		let Some(index) = search(&records, count, glyph)? else { return Ok(false) };
		// THE PAINT OFFSET IS A `u32` AT AN ODD PLACE - two bytes into a six-byte record - so it is
		// read by seeking rather than by an index, which would assume a four-byte stride.
		let mut record = list;
		record.seek(records_at.checked_add(index.checked_mul(6).ok_or_else(bad)?).ok_or_else(bad)?.checked_add(2).ok_or_else(bad)?).ok_or_else(bad)?;
		let at = record.u32().ok_or_else(bad)? as usize;
		let mut nodes = 0usize;
		self.paint_at(list, at, 0, &mut nodes, paints)?;
		Ok(true)
	}

	/// One paint node and everything under it.
	fn paint_at(&self, base: Reader<'a>, at: usize, depth: u8, nodes: &mut usize, paints: &mut impl Paints) -> Result<(), Error> {
		if depth as u32 >= opentype_profile::limits::PAINT_DEPTH {
			return Err(Error::Unsupported(Unsupported::Exceeded { limit: "paint depth", ceiling: opentype_profile::limits::PAINT_DEPTH, asked: depth as u64 + 1 }));
		}
		*nodes += 1;
		if *nodes as u32 > opentype_profile::limits::PAINT_NODES {
			return Err(Error::Unsupported(Unsupported::Exceeded { limit: "paint nodes", ceiling: opentype_profile::limits::PAINT_NODES, asked: *nodes as u64 }));
		}
		let mut reader = base;
		reader.seek(at).ok_or_else(bad)?;
		let format = reader.u8().ok_or_else(bad)?;
		// THE ODD FORMATS ARE THE VARIABLE COUNTERPARTS of the even ones: the same fields, with a
		// variation index appended. Reading an odd format as its even neighbour is correct for every
		// field and then leaves four bytes unread, which is only wrong when a caller walks past the
		// paint - so the format is carried rather than rounded down.
		let varies = format % 2 == 1 && format != 1 && format != 11;
		let base_format = if varies { format - 1 } else { format };
		let paint = match base_format {
			1 => {
				let count = reader.u8().ok_or_else(bad)?;
				let first = reader.u32().ok_or_else(bad)?;
				Paint::Layers { first, count }
			}
			2 => Paint::Solid { palette: reader.u16().ok_or_else(bad)?, alpha: reader.i16().ok_or_else(bad)? },
			4 => {
				let stops = self.stops_at(base, at, reader.u24().ok_or_else(bad)? as usize, varies)?;
				Paint::LinearGradient { stops, x0: reader.i16().ok_or_else(bad)?, y0: reader.i16().ok_or_else(bad)?, x1: reader.i16().ok_or_else(bad)?, y1: reader.i16().ok_or_else(bad)?, x2: reader.i16().ok_or_else(bad)?, y2: reader.i16().ok_or_else(bad)? }
			}
			6 => {
				let stops = self.stops_at(base, at, reader.u24().ok_or_else(bad)? as usize, varies)?;
				Paint::RadialGradient { stops, x0: reader.i16().ok_or_else(bad)?, y0: reader.i16().ok_or_else(bad)?, radius0: reader.u16().ok_or_else(bad)?, x1: reader.i16().ok_or_else(bad)?, y1: reader.i16().ok_or_else(bad)?, radius1: reader.u16().ok_or_else(bad)? }
			}
			8 => {
				let stops = self.stops_at(base, at, reader.u24().ok_or_else(bad)? as usize, varies)?;
				Paint::SweepGradient { stops, centre_x: reader.i16().ok_or_else(bad)?, centre_y: reader.i16().ok_or_else(bad)?, start_angle: reader.i16().ok_or_else(bad)?, end_angle: reader.i16().ok_or_else(bad)? }
			}
			10 => {
				let child = reader.u24().ok_or_else(bad)? as usize;
				let glyph = reader.u16().ok_or_else(bad)?;
				if paints.paint(Paint::Glyph { glyph }, depth) {
					self.paint_at(base, at.checked_add(child).ok_or_else(bad)?, depth + 1, nodes, paints)?;
				}
				return Ok(());
			}
			11 => Paint::ColrGlyph { glyph: reader.u16().ok_or_else(bad)? },
			12 => {
				let child = reader.u24().ok_or_else(bad)? as usize;
				let transform_at = at.checked_add(reader.u24().ok_or_else(bad)? as usize).ok_or_else(bad)?;
				let mut transform_reader = base;
				transform_reader.seek(transform_at).ok_or_else(bad)?;
				let mut transform = [0i32; 6];
				for slot in transform.iter_mut() {
					*slot = transform_reader.i32().ok_or_else(bad)?;
				}
				if paints.paint(Paint::Transform { transform }, depth) {
					self.paint_at(base, at.checked_add(child).ok_or_else(bad)?, depth + 1, nodes, paints)?;
				}
				return Ok(());
			}
			14 | 16 | 18 | 20 | 22 | 24 | 26 | 28 | 30 => {
				let child = reader.u24().ok_or_else(bad)? as usize;
				let paint = match base_format {
					14 => Paint::Translate { dx: reader.i16().ok_or_else(bad)?, dy: reader.i16().ok_or_else(bad)? },
					16 => Paint::Scale { x: reader.i16().ok_or_else(bad)?, y: reader.i16().ok_or_else(bad)?, centre_x: 0, centre_y: 0 },
					18 => {
						let (x, y) = (reader.i16().ok_or_else(bad)?, reader.i16().ok_or_else(bad)?);
						Paint::Scale { x, y, centre_x: reader.i16().ok_or_else(bad)?, centre_y: reader.i16().ok_or_else(bad)? }
					}
					// A UNIFORM SCALE IS THE SAME VALUE TWICE, stated once in the font. Keeping a
					// separate variant would make every consumer handle two spellings of one thing.
					20 => {
						let scale = reader.i16().ok_or_else(bad)?;
						Paint::Scale { x: scale, y: scale, centre_x: 0, centre_y: 0 }
					}
					22 => {
						let scale = reader.i16().ok_or_else(bad)?;
						Paint::Scale { x: scale, y: scale, centre_x: reader.i16().ok_or_else(bad)?, centre_y: reader.i16().ok_or_else(bad)? }
					}
					24 => Paint::Rotate { angle: reader.i16().ok_or_else(bad)?, centre_x: 0, centre_y: 0 },
					26 => {
						let angle = reader.i16().ok_or_else(bad)?;
						Paint::Rotate { angle, centre_x: reader.i16().ok_or_else(bad)?, centre_y: reader.i16().ok_or_else(bad)? }
					}
					28 => Paint::Skew { x_angle: reader.i16().ok_or_else(bad)?, y_angle: reader.i16().ok_or_else(bad)?, centre_x: 0, centre_y: 0 },
					_ => {
						let (x_angle, y_angle) = (reader.i16().ok_or_else(bad)?, reader.i16().ok_or_else(bad)?);
						Paint::Skew { x_angle, y_angle, centre_x: reader.i16().ok_or_else(bad)?, centre_y: reader.i16().ok_or_else(bad)? }
					}
				};
				if paints.paint(paint, depth) {
					self.paint_at(base, at.checked_add(child).ok_or_else(bad)?, depth + 1, nodes, paints)?;
				}
				return Ok(());
			}
			32 => {
				let source = reader.u24().ok_or_else(bad)? as usize;
				let mode = reader.u8().ok_or_else(bad)?;
				let backdrop = reader.u24().ok_or_else(bad)? as usize;
				if opentype_profile::colour::composite_mode(mode).is_none() {
					// A COMPOSITE MODE OUTSIDE THE PROFILE IS REFUSED BY NAME. Falling back to
					// source-over would draw the glyph with the wrong blend, which looks like a
					// designer's choice rather than a mode nobody implemented.
					return Err(Error::Unsupported(Unsupported::Paint(format)));
				}
				if paints.paint(Paint::Composite { mode }, depth) {
					// THE BACKDROP IS PAINTED FIRST AND THE SOURCE OVER IT, whatever order the table
					// states them in - which is the one thing about a composite that a reader can get
					// backwards and still produce a picture.
					self.paint_at(base, at.checked_add(backdrop).ok_or_else(bad)?, depth + 1, nodes, paints)?;
					self.paint_at(base, at.checked_add(source).ok_or_else(bad)?, depth + 1, nodes, paints)?;
				}
				return Ok(());
			}
			_ => return Err(Error::Unsupported(Unsupported::Paint(format))),
		};
		// The leaves, and the one node whose children are a RUN in the layer list rather than an
		// offset inside this paint.
		if paints.paint(paint, depth)
			&& let Paint::Layers { first, count } = paint
		{
			let Some(layers) = self.layer_list else { return Err(bad()) };
			let mut reader = layers;
			let total = reader.u32().ok_or_else(bad)? as usize;
			let offsets_at = reader.position();
			for index in 0..count as usize {
				let which = (first as usize).checked_add(index).ok_or_else(bad)?;
				if which >= total {
					return Err(bad());
				}
				let mut offset = layers;
				offset.seek(offsets_at.checked_add(which.checked_mul(4).ok_or_else(bad)?).ok_or_else(bad)?).ok_or_else(bad)?;
				let child = offset.u32().ok_or_else(bad)? as usize;
				self.paint_at(layers, child, depth + 1, nodes, paints)?;
			}
		}
		Ok(())
	}

	/// Where a paint's colour line is, given the paint's own position.
	fn stops_at(&self, base: Reader<'a>, paint_at: usize, offset: usize, varies: bool) -> Result<Stops<'a>, Error> {
		Ok(Stops { base, at: paint_at.checked_add(offset).ok_or_else(bad)?, varies })
	}

	/// The colour stops of a gradient, written into a buffer the caller owns.
	///
	/// Answers how far the gradient continues past its ends and how many stops were written. A
	/// gradient with more stops than the buffer holds is a REFUSAL rather than a shortened gradient:
	/// a gradient missing its last colours is a different picture, not a smaller one.
	pub fn stops(&self, stops: Stops<'_>, into: &mut [Stop]) -> Result<(Extend, usize), Error> {
		let mut reader = stops.base;
		reader.seek(stops.at).ok_or_else(bad)?;
		let extend = match reader.u8().ok_or_else(bad)? {
			0 => Extend::Pad,
			1 => Extend::Repeat,
			2 => Extend::Reflect,
			// AN EXTEND MODE THIS PROFILE DOES NOT CARRY IS `Pad`, which is the format's own rule for
			// an unrecognised value - the one place in this parser where a default is what the
			// specification says rather than what a reader chose.
			_ => Extend::Pad,
		};
		let count = reader.u16().ok_or_else(bad)? as usize;
		if count > MAX_STOPS || count > into.len() {
			return Err(Error::Unsupported(Unsupported::Exceeded { limit: "paint nodes", ceiling: MAX_STOPS as u32, asked: count as u64 }));
		}
		let stride = if stops.varies { 10 } else { 6 };
		let base = reader.position();
		for (index, out) in into.iter_mut().enumerate().take(count) {
			let mut stop = stops.base;
			stop.seek(base.checked_add(index.checked_mul(stride).ok_or_else(bad)?).ok_or_else(bad)?).ok_or_else(bad)?;
			*out = Stop { offset: stop.i16().ok_or_else(bad)?, palette: stop.u16().ok_or_else(bad)?, alpha: stop.i16().ok_or_else(bad)? };
		}
		Ok((extend, count))
	}
}

/// A binary search over a sorted record array whose first field is a `u16` glyph id and whose stride
/// is six bytes - which both of `COLR`'s base glyph lists are.
fn search(records: &Reader<'_>, count: usize, glyph: u16) -> Result<Option<usize>, Error> {
	let mut low = 0usize;
	let mut high = count;
	while low < high {
		let middle = low + (high - low) / 2;
		let value = records.u16_at(middle * 3).ok_or_else(bad)?;
		match value.cmp(&glyph) {
			core::cmp::Ordering::Equal => return Ok(Some(middle)),
			core::cmp::Ordering::Less => low = middle + 1,
			core::cmp::Ordering::Greater => high = middle,
		}
	}
	Ok(None)
}
