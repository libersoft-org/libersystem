//! THE WRITER: one description of a face, turned into the bytes a parser reads.
//!
//! WHY A DESCRIPTION AND NOT A PROCEDURE. This tool authors more than one face - a last-resort face
//! of boxes, and a conformance corpus whose whole purpose is to carry real `GSUB` and `GPOS` - and a
//! second procedure per face would be a second chance to get `head`'s bounding box, `hhea`'s metric
//! count or the file checksum subtly wrong in only one of them. Everything below is derived from the
//! glyphs and the mapping rather than restated beside them, so the two faces cannot disagree about
//! what their own outlines are.
//!
//! BIG-ENDIAN THROUGHOUT, which is the only endianness this format has.

pub fn u8v(out: &mut Vec<u8>, value: u8) {
	out.push(value);
}

pub fn u16v(out: &mut Vec<u8>, value: u16) {
	out.extend_from_slice(&value.to_be_bytes());
}

pub fn i16v(out: &mut Vec<u8>, value: i16) {
	out.extend_from_slice(&value.to_be_bytes());
}

pub fn u32v(out: &mut Vec<u8>, value: u32) {
	out.extend_from_slice(&value.to_be_bytes());
}

/// One glyph: its outline, and what it measures.
pub struct Glyph {
	/// Closed contours of on-curve points. Empty is a glyph too - the space is one.
	pub contours: Vec<Vec<(i16, i16)>>,
	pub advance: u16,
	pub left_bearing: i16,
}

impl Glyph {
	pub fn blank(advance: u16) -> Self {
		Self { contours: Vec::new(), advance, left_bearing: 0 }
	}

	/// A filled rectangle, which is the shape every conformance glyph is: what those faces are read
	/// for is their glyph INDICES and their POSITIONS, and an outline that varied per glyph would be
	/// a design decision nothing checks.
	pub fn rectangle(left: i16, bottom: i16, right: i16, top: i16, advance: u16) -> Self {
		Self { contours: vec![vec![(left, bottom), (left, top), (right, top), (right, bottom)]], advance, left_bearing: left }
	}

	/// The bounding box of the outline, or `None` for an empty glyph - which has no box, rather than
	/// a box at the origin.
	pub fn bounds(&self) -> Option<(i16, i16, i16, i16)> {
		let points: Vec<(i16, i16)> = self.contours.iter().flatten().copied().collect();
		if points.is_empty() {
			return None;
		}
		Some((points.iter().map(|point| point.0).min().unwrap_or(0), points.iter().map(|point| point.1).min().unwrap_or(0), points.iter().map(|point| point.0).max().unwrap_or(0), points.iter().map(|point| point.1).max().unwrap_or(0)))
	}

	pub fn points(&self) -> usize {
		self.contours.iter().map(|contour| contour.len()).sum()
	}
}

/// How the face maps characters to glyphs.
pub enum Cmap {
	/// One format 4 subtable whose glyph array covers the whole basic plane. A many-to-one mapping
	/// needs the array: format 4's `idDelta` maps a segment to CONSECUTIVE glyph ids, so a segment
	/// cannot send a range to one glyph, and format 13 - which exists for exactly that - is not in
	/// the admitted set.
	BasicPlane(Vec<u16>),
	/// The code points this face actually covers, each named. A format 4 subtable carries those
	/// below `0x10000` and a format 12 subtable carries all of them when any is above it.
	Named(Vec<(u32, u16)>),
}

/// The strings a face is identified by. They are what the declaration beside a staged face has to
/// agree with, letter for letter.
pub struct Names {
	pub family: String,
	pub style: String,
	pub postscript: String,
	pub version: String,
}

/// Everything a face is, before it is bytes.
pub struct FaceSpec {
	pub names: Names,
	pub units_per_em: u16,
	pub ascender: i16,
	pub descender: i16,
	pub line_gap: i16,
	pub weight_class: u16,
	pub width_class: u16,
	pub vendor: [u8; 4],
	/// `OS/2`'s average advance. STATED rather than averaged: the mean of a face's advances is a
	/// number about the glyph set this face happens to have, and what the field means is the width
	/// a consumer should expect of its text.
	pub average_advance: i16,
	pub fixed_pitch: bool,
	pub glyphs: Vec<Glyph>,
	pub cmap: Cmap,
	/// The tables this writer does not derive: `GSUB`, `GPOS`, `GDEF`, `fvar`, `gvar`, `HVAR`. Each
	/// is built by the face that wants it and merged into the directory in tag order.
	pub extra: Vec<([u8; 4], Vec<u8>)>,
}

/// One simple glyph out of closed contours of on-curve points.
///
/// EVERY POINT IS ON THE CURVE and every delta is written at full width. A generator that packed
/// deltas into the short forms would be exercising the parser's packing paths with its only staged
/// face, and what these faces are for is being READ correctly - not being small.
pub fn simple_glyph(contours: &[Vec<(i16, i16)>]) -> Vec<u8> {
	let points: Vec<(i16, i16)> = contours.iter().flatten().copied().collect();
	if points.is_empty() {
		// An empty glyph is a glyph: `loca` gives it a zero length and there is no record at all.
		return Vec::new();
	}
	let mut out = Vec::new();
	i16v(&mut out, contours.len() as i16);
	i16v(&mut out, points.iter().map(|point| point.0).min().unwrap_or(0));
	i16v(&mut out, points.iter().map(|point| point.1).min().unwrap_or(0));
	i16v(&mut out, points.iter().map(|point| point.0).max().unwrap_or(0));
	i16v(&mut out, points.iter().map(|point| point.1).max().unwrap_or(0));
	let mut end: u16 = 0;
	for contour in contours {
		end += contour.len() as u16;
		u16v(&mut out, end - 1);
	}
	// No hinting. The profile does not interpret instructions, and a face carrying bytes nothing
	// executes would be carrying a claim nothing checks.
	u16v(&mut out, 0);
	for _ in &points {
		// ON_CURVE_POINT only: no repeat, no short deltas, no "same as previous".
		u8v(&mut out, 0x01);
	}
	let mut previous: i16 = 0;
	for point in &points {
		i16v(&mut out, point.0 - previous);
		previous = point.0;
	}
	let mut previous: i16 = 0;
	for point in &points {
		i16v(&mut out, point.1 - previous);
		previous = point.1;
	}
	while out.len() % 2 != 0 {
		out.push(0);
	}
	out
}

pub fn table_checksum(bytes: &[u8]) -> u32 {
	let mut sum: u32 = 0;
	let mut at = 0;
	while at < bytes.len() {
		let mut word = [0u8; 4];
		let take = (bytes.len() - at).min(4);
		word[..take].copy_from_slice(&bytes[at..at + take]);
		sum = sum.wrapping_add(u32::from_be_bytes(word));
		at += 4;
	}
	sum
}

// ---------------------------------------------------------------------------------------------
// The derived tables. Every number below comes from the glyphs or the mapping.
// ---------------------------------------------------------------------------------------------

/// The bounding box over the glyphs that HAVE one. An empty glyph contributes nothing rather than a
/// point at the origin, which would drag the box to include a place no outline is.
fn font_bounds(spec: &FaceSpec) -> (i16, i16, i16, i16) {
	let mut bounds: Option<(i16, i16, i16, i16)> = None;
	for glyph in &spec.glyphs {
		let Some(this) = glyph.bounds() else { continue };
		bounds = Some(match bounds {
			None => this,
			Some(so_far) => (so_far.0.min(this.0), so_far.1.min(this.1), so_far.2.max(this.2), so_far.3.max(this.3)),
		});
	}
	bounds.unwrap_or((0, 0, 0, 0))
}

fn head(spec: &FaceSpec, index_to_loc_long: bool) -> Vec<u8> {
	let (x_min, y_min, x_max, y_max) = font_bounds(spec);
	let mut out = Vec::new();
	u32v(&mut out, 0x0001_0000);
	u32v(&mut out, 0x0001_0000);
	// Filled in once the whole file exists; it is a checksum OF the file and cannot be known before.
	u32v(&mut out, 0);
	u32v(&mut out, 0x5F0F_3CF5);
	// Baseline at y=0 and left sidebearing at x=0 are both true of these designs, and saying so is
	// what lets a consumer trust the bounding box below.
	u16v(&mut out, 0x0003);
	u16v(&mut out, spec.units_per_em);
	// Created and modified: a FIXED instant, because a generator that stamped the clock would write
	// a different file every run and the `--check` gate could never pass twice.
	for _ in 0..2 {
		u32v(&mut out, 0);
		u32v(&mut out, 0);
	}
	i16v(&mut out, x_min);
	i16v(&mut out, y_min);
	i16v(&mut out, x_max);
	i16v(&mut out, y_max);
	// Not bold, not italic, not anything: the style bits and the `OS/2` selection say the same thing
	// and a face whose two answers differ is a face a menu renders wrongly.
	u16v(&mut out, 0);
	u16v(&mut out, 8);
	i16v(&mut out, 2);
	i16v(&mut out, i16::from(index_to_loc_long));
	i16v(&mut out, 0);
	out
}

fn hhea(spec: &FaceSpec) -> Vec<u8> {
	// THE SIDE BEARINGS ARE OVER THE GLYPHS THAT HAVE OUTLINES. A blank glyph has no left bearing to
	// be the minimum of: counting its zero would tell a consumer this face reaches the origin when
	// nothing in it does.
	let drawn: Vec<&Glyph> = spec.glyphs.iter().filter(|glyph| glyph.bounds().is_some()).collect();
	let advance_max = spec.glyphs.iter().map(|glyph| glyph.advance).max().unwrap_or(0);
	let min_left = drawn.iter().map(|glyph| glyph.left_bearing).min().unwrap_or(0);
	let min_right = drawn.iter().map(|glyph| glyph.advance as i32 - glyph.bounds().unwrap_or((0, 0, 0, 0)).2 as i32).min().unwrap_or(0);
	let max_extent = drawn.iter().map(|glyph| glyph.bounds().unwrap_or((0, 0, 0, 0)).2).max().unwrap_or(0);
	let mut out = Vec::new();
	u32v(&mut out, 0x0001_0000);
	i16v(&mut out, spec.ascender);
	i16v(&mut out, spec.descender);
	i16v(&mut out, spec.line_gap);
	u16v(&mut out, advance_max);
	i16v(&mut out, min_left);
	i16v(&mut out, min_right as i16);
	i16v(&mut out, max_extent);
	i16v(&mut out, 1);
	i16v(&mut out, 0);
	i16v(&mut out, 0);
	for _ in 0..4 {
		i16v(&mut out, 0);
	}
	i16v(&mut out, 0);
	// Every glyph carries its own advance, so there is no trailing run to compress.
	u16v(&mut out, spec.glyphs.len() as u16);
	out
}

fn maxp(spec: &FaceSpec) -> Vec<u8> {
	let mut out = Vec::new();
	u32v(&mut out, 0x0001_0000);
	u16v(&mut out, spec.glyphs.len() as u16);
	u16v(&mut out, spec.glyphs.iter().map(|glyph| glyph.points()).max().unwrap_or(0) as u16);
	u16v(&mut out, spec.glyphs.iter().map(|glyph| glyph.contours.len()).max().unwrap_or(0) as u16);
	// No composites, so every composite bound is zero and the depth is zero.
	u16v(&mut out, 0);
	u16v(&mut out, 0);
	u16v(&mut out, 0);
	// No hinting: no zones, no twilight points, no storage, no function or instruction definitions.
	for _ in 0..8 {
		u16v(&mut out, 0);
	}
	out
}

fn hmtx(spec: &FaceSpec) -> Vec<u8> {
	let mut out = Vec::new();
	for glyph in &spec.glyphs {
		u16v(&mut out, glyph.advance);
		i16v(&mut out, glyph.left_bearing);
	}
	out
}

/// The largest power of two at or below `count`, and its exponent. Both `cmap` format 4 and the
/// table directory store them so a reader may binary-search, and both get them wrong in the same way
/// if they are written out by hand.
fn binary_search_range(count: usize) -> (usize, usize) {
	let selector = usize::BITS - 1 - count.leading_zeros();
	(1 << selector, selector as usize)
}

/// A format 4 subtable whose glyph array covers `0x0000..=0xFFFE` in one segment.
fn cmap_format4_array(array: &[u16]) -> Vec<u8> {
	let mut out = Vec::new();
	let segment_count: u16 = 2;
	let entries: usize = array.len();
	u16v(&mut out, 4);
	let length: usize = 16 + 8 * segment_count as usize + 2 * entries;
	u16v(&mut out, length as u16);
	u16v(&mut out, 0);
	u16v(&mut out, segment_count * 2);
	// searchRange, entrySelector, rangeShift, as the format defines them for two segments.
	u16v(&mut out, 4);
	u16v(&mut out, 1);
	u16v(&mut out, 0);
	// endCode
	u16v(&mut out, 0xFFFE);
	u16v(&mut out, 0xFFFF);
	u16v(&mut out, 0);
	// startCode
	u16v(&mut out, 0x0000);
	u16v(&mut out, 0xFFFF);
	// idDelta: the first segment's mapping comes from the array, so its delta is zero; the
	// terminator maps 0xFFFF to glyph 0, which is the absence of coverage.
	i16v(&mut out, 0);
	i16v(&mut out, 1);
	// idRangeOffset: the first segment points at the array that follows the offsets; the second
	// uses its delta.
	u16v(&mut out, segment_count * 2);
	u16v(&mut out, 0);
	for glyph in array {
		u16v(&mut out, *glyph);
	}
	out
}

/// A format 4 subtable over named code points, as segments of consecutive characters whose glyphs
/// are consecutive too - which is the only shape `idDelta` can express without an array.
fn cmap_format4_named(named: &[(u32, u16)]) -> Vec<u8> {
	let mut basic: Vec<(u16, u16)> = named.iter().filter(|(code, _)| *code < 0xFFFF).map(|(code, glyph)| (*code as u16, *glyph)).collect();
	basic.sort_unstable();
	let mut segments: Vec<(u16, u16, i16)> = Vec::new();
	for (code, glyph) in basic {
		match segments.last_mut() {
			Some((_, end, delta)) if *end + 1 == code && (code as i32 + *delta as i32) as u16 == glyph => *end = code,
			_ => segments.push((code, code, glyph.wrapping_sub(code) as i16)),
		}
	}
	// The mandatory terminator, which every format 4 subtable ends with.
	segments.push((0xFFFF, 0xFFFF, 1));

	let count = segments.len() as u16;
	let (power, selector) = binary_search_range(segments.len());
	let mut out = Vec::new();
	u16v(&mut out, 4);
	u16v(&mut out, 16 + 8 * count);
	u16v(&mut out, 0);
	u16v(&mut out, count * 2);
	u16v(&mut out, (power * 2) as u16);
	u16v(&mut out, selector as u16);
	u16v(&mut out, (count as usize * 2 - power * 2) as u16);
	for (_, end, _) in &segments {
		u16v(&mut out, *end);
	}
	u16v(&mut out, 0);
	for (start, _, _) in &segments {
		u16v(&mut out, *start);
	}
	for (_, _, delta) in &segments {
		i16v(&mut out, *delta);
	}
	for _ in &segments {
		u16v(&mut out, 0);
	}
	out
}

/// A format 12 subtable: the only admitted format that reaches above the basic plane.
fn cmap_format12(named: &[(u32, u16)]) -> Vec<u8> {
	let mut sorted: Vec<(u32, u16)> = named.to_vec();
	sorted.sort_unstable();
	// GROUPS OF CONSECUTIVE CODE POINTS MAPPING TO CONSECUTIVE GLYPHS, which is what the format
	// stores. A run is extended only while both sides stay consecutive; anything else starts a new
	// group rather than being folded into one that would then name the wrong glyph.
	let mut groups: Vec<(u32, u32, u16)> = Vec::new();
	for (code, glyph) in sorted {
		let extended = match groups.last_mut() {
			Some((start, end, first)) if *end + 1 == code && u32::from(*first) + (code - *start) == u32::from(glyph) => {
				*end = code;
				true
			}
			_ => false,
		};
		if !extended {
			groups.push((code, code, glyph));
		}
	}
	let mut out = Vec::new();
	u16v(&mut out, 12);
	u16v(&mut out, 0);
	u32v(&mut out, 16 + 12 * groups.len() as u32);
	u32v(&mut out, 0);
	u32v(&mut out, groups.len() as u32);
	for (start, end, glyph) in &groups {
		u32v(&mut out, *start);
		u32v(&mut out, *end);
		u32v(&mut out, *glyph as u32);
	}
	out
}

fn cmap(spec: &FaceSpec) -> Vec<u8> {
	// (platform, encoding, subtable)
	let subtables: Vec<(u16, u16, Vec<u8>)> = match &spec.cmap {
		Cmap::BasicPlane(array) => vec![(3, 1, cmap_format4_array(array))],
		Cmap::Named(named) => {
			let mut tables = vec![(3u16, 1u16, cmap_format4_named(named))];
			// A FORMAT 12 SUBTABLE ONLY WHEN SOMETHING IS ABOVE THE BASIC PLANE. Writing one
			// unconditionally would make every face carry a second mapping of the same code points,
			// and two mappings that can disagree are a defect waiting for one of them to be edited.
			if named.iter().any(|(code, _)| *code > 0xFFFF) {
				tables.push((3, 10, cmap_format12(named)));
			}
			tables
		}
	};
	let mut out = Vec::new();
	u16v(&mut out, 0);
	u16v(&mut out, subtables.len() as u16);
	let mut offset = 4 + 8 * subtables.len() as u32;
	for (platform, encoding, bytes) in &subtables {
		u16v(&mut out, *platform);
		u16v(&mut out, *encoding);
		u32v(&mut out, offset);
		offset += bytes.len() as u32;
	}
	for (_, _, bytes) in &subtables {
		out.extend_from_slice(bytes);
	}
	out
}

fn os2(spec: &FaceSpec) -> Vec<u8> {
	let (first, last) = match &spec.cmap {
		Cmap::BasicPlane(_) => (0x0020, 0xFFFE),
		Cmap::Named(named) => {
			let basic: Vec<u32> = named.iter().map(|(code, _)| *code).filter(|code| *code <= 0xFFFF).collect();
			// `usLastCharIndex` is a `u16`: a face reaching above the basic plane says so with
			// `0xFFFF`, which is what the field means rather than a truncation of the real answer.
			let highest = if named.iter().any(|(code, _)| *code > 0xFFFF) { 0xFFFF } else { basic.iter().copied().max().unwrap_or(0) as u16 };
			(basic.iter().copied().min().unwrap_or(0) as u16, highest)
		}
	};
	let mut out = Vec::new();
	// Version 4: the last version whose fields are all defined without the two `OS/2` v5 sizes,
	// which describe optical size ranges these faces do not have.
	u16v(&mut out, 4);
	i16v(&mut out, spec.average_advance);
	u16v(&mut out, spec.weight_class);
	u16v(&mut out, spec.width_class);
	// Installable embedding: these faces are Unlicense, so there is nothing to restrict.
	u16v(&mut out, 0);
	i16v(&mut out, 650);
	i16v(&mut out, 600);
	i16v(&mut out, 0);
	i16v(&mut out, -75);
	i16v(&mut out, 650);
	i16v(&mut out, 600);
	i16v(&mut out, 0);
	i16v(&mut out, 350);
	i16v(&mut out, 50);
	i16v(&mut out, 400);
	i16v(&mut out, 0);
	// PANOSE: all zeros is "any", which is the truthful answer for faces of rectangles.
	for _ in 0..10 {
		u8v(&mut out, 0);
	}
	// The Unicode and code-page ranges a face CLAIMS. Zero throughout: claiming script coverage a
	// face does not have would make a selector pick it over one that could actually draw the script.
	for _ in 0..4 {
		u32v(&mut out, 0);
	}
	out.extend_from_slice(&spec.vendor);
	// fsSelection: REGULAR, and nothing else. The `head` style bits say the same.
	u16v(&mut out, 0x0040);
	u16v(&mut out, first);
	u16v(&mut out, last);
	i16v(&mut out, spec.ascender);
	i16v(&mut out, spec.descender);
	i16v(&mut out, spec.line_gap);
	u16v(&mut out, spec.ascender as u16);
	u16v(&mut out, spec.descender.unsigned_abs());
	u32v(&mut out, 0);
	u32v(&mut out, 0);
	i16v(&mut out, 500);
	i16v(&mut out, 700);
	// The default and break characters, and the longest context any feature needs.
	u16v(&mut out, 0);
	u16v(&mut out, 0x0020);
	u16v(&mut out, 0);
	out
}

fn post(spec: &FaceSpec) -> Vec<u8> {
	let mut out = Vec::new();
	// Version 3.0: no glyph names. The names would be a list of strings nothing looks up, and a
	// table to keep correct for no reader is a table that goes wrong unnoticed.
	u32v(&mut out, 0x0003_0000);
	u32v(&mut out, 0);
	i16v(&mut out, 0);
	i16v(&mut out, 0);
	u32v(&mut out, u32::from(spec.fixed_pitch));
	for _ in 0..4 {
		u32v(&mut out, 0);
	}
	out
}

fn name(spec: &FaceSpec) -> Vec<u8> {
	// name id -> string. The typographic pair (16, 17) is given as well as the legacy pair (1, 2)
	// because the oracle prefers the typographic one and a face that stated only the legacy pair
	// would be checked against a fallback rather than against what it means.
	let names = &spec.names;
	let records: [(u16, &str); 8] = [
		(1, &names.family),
		(2, &names.style),
		(3, &names.postscript),
		(4, &names.family),
		(5, &names.version),
		(6, &names.postscript),
		(16, &names.family),
		(17, &names.style),
	];
	let mut strings = Vec::new();
	let mut offsets = Vec::new();
	for (_, text) in records {
		offsets.push((strings.len() as u16, (text.chars().count() * 2) as u16));
		for unit in text.encode_utf16() {
			strings.extend_from_slice(&unit.to_be_bytes());
		}
	}
	let mut out = Vec::new();
	u16v(&mut out, 0);
	u16v(&mut out, records.len() as u16);
	u16v(&mut out, (6 + 12 * records.len()) as u16);
	for (index, (id, _)) in records.iter().enumerate() {
		// Windows, UCS-2, US English: the encoding every consumer in this tree reads.
		u16v(&mut out, 3);
		u16v(&mut out, 1);
		u16v(&mut out, 0x0409);
		u16v(&mut out, *id);
		u16v(&mut out, offsets[index].1);
		u16v(&mut out, offsets[index].0);
	}
	out.extend_from_slice(&strings);
	out
}

/// The face, assembled.
pub fn build(spec: &FaceSpec) -> Vec<u8> {
	let mut glyf = Vec::new();
	let mut offsets: Vec<u32> = vec![0];
	for glyph in &spec.glyphs {
		glyf.extend_from_slice(&simple_glyph(&glyph.contours));
		while glyf.len() % 4 != 0 {
			glyf.push(0);
		}
		offsets.push(glyf.len() as u32);
	}
	// LONG `loca`, unconditionally. The short form stores offsets divided by two and therefore
	// cannot express an odd one; choosing it on size would make the format depend on how big the
	// outlines happen to be, which is a difference nothing downstream should have to care about.
	let mut loca = Vec::new();
	for offset in &offsets {
		u32v(&mut loca, *offset);
	}

	let mut tables: Vec<([u8; 4], Vec<u8>)> = vec![
		(*b"OS/2", os2(spec)),
		(*b"cmap", cmap(spec)),
		(*b"glyf", glyf),
		(*b"head", head(spec, true)),
		(*b"hhea", hhea(spec)),
		(*b"hmtx", hmtx(spec)),
		(*b"loca", loca),
		(*b"maxp", maxp(spec)),
		(*b"name", name(spec)),
		(*b"post", post(spec)),
	];
	for (tag, bytes) in &spec.extra {
		tables.push((*tag, bytes.clone()));
	}
	// THE DIRECTORY IS SORTED BY TAG, which the format requires and a reader may binary-search.
	tables.sort_by(|left, right| left.0.cmp(&right.0));

	let count = tables.len();
	let mut directory = Vec::new();
	u32v(&mut directory, 0x0001_0000);
	u16v(&mut directory, count as u16);
	// searchRange, entrySelector and rangeShift, as the format defines them: the largest power of
	// two at or below the table count, times the record size.
	let (power, selector) = binary_search_range(count);
	u16v(&mut directory, (power * 16) as u16);
	u16v(&mut directory, selector as u16);
	u16v(&mut directory, ((count - power) * 16) as u16);

	let mut body = Vec::new();
	let mut offset = 12 + count * 16;
	let mut head_record_at = 0usize;
	for (tag, bytes) in &tables {
		if tag == b"head" {
			head_record_at = directory.len();
		}
		directory.extend_from_slice(tag);
		u32v(&mut directory, table_checksum(bytes));
		u32v(&mut directory, offset as u32);
		u32v(&mut directory, bytes.len() as u32);
		body.extend_from_slice(bytes);
		while body.len() % 4 != 0 {
			body.push(0);
		}
		offset = 12 + count * 16 + body.len();
	}
	let mut out = directory;
	let head_offset = u32::from_be_bytes([out[head_record_at + 8], out[head_record_at + 9], out[head_record_at + 10], out[head_record_at + 11]]) as usize;
	out.extend_from_slice(&body);

	// THE FILE'S OWN CHECKSUM, WRITTEN LAST BECAUSE IT IS ABOUT THE FINISHED FILE.
	// `checkSumAdjustment` is the constant minus the checksum of everything, computed with that
	// field zero - which is what it already is.
	let adjustment = 0xB1B0_AFBAu32.wrapping_sub(table_checksum(&out));
	out[head_offset + 8..head_offset + 12].copy_from_slice(&adjustment.to_be_bytes());
	// And the `head` record's own checksum was taken over the table with the field zero, which is
	// what the format asks for - so it is left as it is.
	out
}
