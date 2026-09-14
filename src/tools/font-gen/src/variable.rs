//! `fvar`, `gvar`, `HVAR` and `MVAR`: a face whose outlines and whose metrics vary, and vary
//! SEPARATELY.
//!
//! THE CASE THIS EXISTS FOR is the one the plan names: an instance where the METRICS vary
//! independently of the OUTLINES. A variable face whose advance always moved with its outline would
//! hide the commonest defect in a variation implementation - an advance taken from the outline's
//! phantom points when the face carries an `HVAR` that says something else, or the reverse. So this
//! face gives the two their own regions of the axis: one half of the axis moves the outline and
//! leaves the advance alone, and the other moves the advance and leaves the outline alone.

use crate::write::{i16v, u16v, u32v};

/// The one axis this corpus varies on, and the values a gate may ask for.
pub const AXIS: [u8; 4] = *b"wght";
pub const AXIS_MIN: i32 = 100;
pub const AXIS_DEFAULT: i32 = 400;
pub const AXIS_MAX: i32 = 900;

/// `fvar`: one weight axis, and two named instances.
///
/// THE DEFAULT IS NOT THE MIDPOINT, deliberately. Normalisation scales the two halves of an axis
/// separately around the default, and an axis whose default sat in the middle would give the same
/// answer under either rule.
pub fn fvar() -> Vec<u8> {
	let mut out = Vec::new();
	u16v(&mut out, 1);
	u16v(&mut out, 0);
	u16v(&mut out, 16);
	u16v(&mut out, 2);
	u16v(&mut out, 1);
	u16v(&mut out, 20);
	u16v(&mut out, 2);
	u16v(&mut out, 8);
	out.extend_from_slice(&AXIS);
	u32v(&mut out, (AXIS_MIN << 16) as u32);
	u32v(&mut out, (AXIS_DEFAULT << 16) as u32);
	u32v(&mut out, (AXIS_MAX << 16) as u32);
	u16v(&mut out, 0);
	u16v(&mut out, 256);
	// "Light" at the bottom of the axis, where the OUTLINE varies.
	u16v(&mut out, 257);
	u16v(&mut out, 0);
	u32v(&mut out, (AXIS_MIN << 16) as u32);
	// "Bold" at the top, where the ADVANCE varies.
	u16v(&mut out, 258);
	u16v(&mut out, 0);
	u32v(&mut out, ((AXIS_MAX - 200) << 16) as u32);
	out
}

/// An item variation store over ONE region of the axis, with one delta per item.
///
/// `peak` is in the normalised `2.14` units the format stores: 16384 is the top of the axis and
/// -16384 the bottom.
fn item_store(peak: i16, deltas: &[i16]) -> Vec<u8> {
	let mut regions = Vec::new();
	u16v(&mut regions, 1);
	u16v(&mut regions, 1);
	i16v(&mut regions, peak.min(0));
	i16v(&mut regions, peak);
	i16v(&mut regions, peak.max(0));

	let mut data = Vec::new();
	u16v(&mut data, deltas.len() as u16);
	u16v(&mut data, 1);
	u16v(&mut data, 1);
	u16v(&mut data, 0);
	for delta in deltas {
		i16v(&mut data, *delta);
	}

	let store_header = 12usize;
	let regions_at = store_header;
	let data_at = regions_at + regions.len();
	let mut store = Vec::new();
	u16v(&mut store, 1);
	u32v(&mut store, regions_at as u32);
	u16v(&mut store, 1);
	u32v(&mut store, data_at as u32);
	store.extend_from_slice(&regions);
	store.extend_from_slice(&data);
	store
}

/// `HVAR`: advance deltas, one per glyph, over the TOP half of the axis alone.
///
/// The glyph id IS the delta-set index here, which is what an absent delta-set index map means.
pub fn hvar(deltas: &[i16]) -> Vec<u8> {
	let store = item_store(16384, deltas);
	let mut out = Vec::new();
	u16v(&mut out, 1);
	u16v(&mut out, 0);
	u32v(&mut out, 20);
	u32v(&mut out, 0);
	u32v(&mut out, 0);
	u32v(&mut out, 0);
	out.extend_from_slice(&store);
	out
}

/// `MVAR`: the font-wide ascender and descender, over the top half of the axis.
pub fn mvar(ascender: i16, descender: i16) -> Vec<u8> {
	let store = item_store(16384, &[ascender, descender]);
	let mut out = Vec::new();
	u16v(&mut out, 1);
	u16v(&mut out, 0);
	u16v(&mut out, 0);
	u16v(&mut out, 8);
	u16v(&mut out, 2);
	u16v(&mut out, 28);
	out.extend_from_slice(b"hasc");
	u16v(&mut out, 0);
	u16v(&mut out, 0);
	out.extend_from_slice(b"hdsc");
	u16v(&mut out, 0);
	u16v(&mut out, 1);
	out.extend_from_slice(&store);
	out
}

/// One glyph's variation data: one tuple, its own point numbers, and the deltas.
fn glyph_data(tuple: u16, serialised: &[u8]) -> Vec<u8> {
	let mut out = Vec::new();
	u16v(&mut out, 1);
	u16v(&mut out, 8);
	u16v(&mut out, serialised.len() as u16);
	// The shared tuple named, with this tuple's OWN point numbers.
	u16v(&mut out, 0x2000 | tuple);
	out.extend_from_slice(serialised);
	while out.len() % 2 != 0 {
		out.push(0);
	}
	out
}

/// `gvar` for the corpus's variable face: one glyph whose outline moves at the BOTTOM of the axis.
///
/// `entries` is one serialised tuple per glyph, or `None` for a glyph that does not vary - which is
/// an empty range rather than an absent entry.
pub fn gvar(entries: &[Option<Vec<u8>>]) -> Vec<u8> {
	let bodies: Vec<Option<Vec<u8>>> = entries.iter().map(|entry| entry.as_ref().map(|serialised| glyph_data(0, serialised))).collect();

	let mut data = Vec::new();
	let mut offsets: Vec<usize> = vec![0];
	for body in &bodies {
		if let Some(bytes) = body {
			data.extend_from_slice(bytes);
		}
		offsets.push(data.len());
	}

	let mut shared = Vec::new();
	// One shared tuple, peaking at the BOTTOM of the axis: the half `HVAR` above does not cover, so
	// an instance can move the outline without moving the advance and the other way round.
	i16v(&mut shared, -16384);

	let header = 20usize;
	let offsets_length = offsets.len() * 4;
	let shared_at = header + offsets_length;
	let data_at = shared_at + shared.len();

	let mut out = Vec::new();
	u16v(&mut out, 1);
	u16v(&mut out, 0);
	u16v(&mut out, 1);
	u16v(&mut out, 1);
	u32v(&mut out, shared_at as u32);
	u16v(&mut out, entries.len() as u16);
	// LONG OFFSETS, which store the offset itself rather than half of it. The short form cannot
	// express an odd offset, and choosing it on size would make the format depend on how big the
	// deltas happen to be.
	u16v(&mut out, 1);
	u32v(&mut out, data_at as u32);
	for offset in &offsets {
		u32v(&mut out, *offset as u32);
	}
	out.extend_from_slice(&shared);
	out.extend_from_slice(&data);
	out
}
