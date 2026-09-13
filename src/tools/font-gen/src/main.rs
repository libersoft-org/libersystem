//! THE LAST-RESORT FACE, AUTHORED RATHER THAN IMPORTED.
//!
//! `docs/todo/P02M0136.md` decided that a face entering this tree must be PUBLIC DOMAIN OR
//! UNLICENSE and nothing else. A face is not linked code: it is redistributed verbatim in every
//! image, so an attribution or notice obligation attaches to the artifact itself rather than to a
//! build step, and the project's answer is to carry none. The faces a text stack would reach for are
//! all outside that set - Noto, Liberation, Inter and Unicode's own Last Resort are SIL OFL, DejaVu
//! carries the Bitstream Vera terms, Roboto and Droid are Apache-2.0 - so nothing could be staged
//! and the catalogue had nothing to read.
//!
//! The plan names the route out, and this is it: AUTHOR one. A last-resort face's job is to draw a
//! visible box for a code point nothing else covers, which is a mechanical design this tree can
//! generate the way it generates its Unicode tables - and anything this tree writes is Unlicense, so
//! the licence question does not arise.
//!
//! WHAT IT IS NOT. It is not a text face and it does not make this system able to display Arabic,
//! Devanagari, Thai or Khmer: every code point it covers draws the same box. What it gives the tree
//! is a REAL face - real `glyf` outlines, a real `cmap`, real metrics - staged at the canonical
//! destination, so the catalogue has something to enumerate, the truth oracle has something to
//! check, and the guest gate has something to shape, lay out and rasterise. The multi-script
//! conformance corpus the host gates want is a type-design job and is still owed.
//!
//! `--check` regenerates and compares rather than writing, so a gate can prove the staged bytes are
//! the bytes this generator produces.

use bootproto::sha256;
use font_parse::Face;
use std::path::PathBuf;

// The design grid. A thousand units to the em is what a `glyf` face conventionally uses, and every
// number below is in it.
const UNITS_PER_EM: u16 = 1000;
const ASCENDER: i16 = 800;
const DESCENDER: i16 = -200;
const LINE_GAP: i16 = 0;

// The box, and the space it sits in. The advance is the same for every covered code point: a face
// whose glyphs are all one box is monospaced by construction, and saying so is more honest than
// inventing per-character widths nothing measured.
const BOX_ADVANCE: u16 = 800;
const SPACE_ADVANCE: u16 = 500;
const BOX_LEFT: i16 = 100;
const BOX_RIGHT: i16 = 700;
const BOX_BOTTOM: i16 = 0;
const BOX_TOP: i16 = 800;
const STROKE: i16 = 80;

// Three glyphs, and each is there for a reason.
//
//   0  `.notdef`, which every face must have at index zero. A `cmap` may not map to it: zero is how
//      a lookup says NOT COVERED, so a face that mapped a code point there would be claiming coverage
//      and delivering the absence of it.
//   1  the space, which must be BLANK. A last-resort face draws a box for everything it covers, and
//      a box where a space belongs would break every line break and every advance in the layout
//      above it - the one character whose correct rendering is nothing at all.
//   2  the box, which is what every other covered code point maps to.
const GLYPH_NOTDEF: u16 = 0;
const GLYPH_SPACE: u16 = 1;
const GLYPH_BOX: u16 = 2;
const GLYPH_COUNT: u16 = 3;

// The family and style the declaration beside the face has to agree with, letter for letter.
const FAMILY: &str = "LiberSystem Last Resort";
const STYLE: &str = "Regular";
const VERSION_STRING: &str = "Version 1.000";
const POSTSCRIPT_NAME: &str = "LiberSystemLastResort-Regular";
const VENDOR: &[u8; 4] = b"LIBR";
const WEIGHT_CLASS: u16 = 400;
const WIDTH_CLASS: u16 = 5;

fn main() -> std::process::ExitCode {
	let check = std::env::args().any(|argument| argument == "--check");
	let Some(root) = repository_root() else {
		eprintln!("font-gen: could not find the repository root");
		return std::process::ExitCode::FAILURE;
	};
	let directory = root.join("src/volume/share/fonts");
	let face_path = directory.join("lastresort.ttf");
	let declaration_path = directory.join("lastresort.ttf.face");

	let face = build_face();
	// PARSED BY THE PARSER THAT WILL READ IT, in the run that wrote it. A generator that checked its
	// output with its own writer would only ever prove that it agrees with itself.
	if let Err(error) = Face::open(&face, 0) {
		eprintln!("font-gen: the face this generator produced does not open: {error:?}");
		return std::process::ExitCode::FAILURE;
	}
	let declaration = build_declaration(&face);
	// AND THE SIDECAR IS PARSED BY THE CATALOGUE'S OWN VOCABULARY, for the same reason.
	let digest = sha256::digest(&face);
	if let Err(error) = service_logic::font_record::parse(&declaration, &digest) {
		eprintln!("font-gen: the declaration this generator produced is refused: {error:?}");
		return std::process::ExitCode::FAILURE;
	}

	if check {
		let mut drifted = Vec::new();
		for (path, wanted) in [(&face_path, face.as_slice()), (&declaration_path, declaration.as_bytes())] {
			match std::fs::read(path) {
				Ok(found) if found == wanted => {}
				Ok(_) => drifted.push(format!("{} is not what this generator produces", path.display())),
				Err(_) => drifted.push(format!("{} is not staged", path.display())),
			}
		}
		if drifted.is_empty() {
			println!("font-gen: the staged last-resort face is exactly what this generator produces");
			return std::process::ExitCode::SUCCESS;
		}
		for problem in &drifted {
			eprintln!("font-gen: {problem}");
		}
		eprintln!("font-gen: run `cargo run --manifest-path src/tools/font-gen/Cargo.toml` to restage it");
		return std::process::ExitCode::FAILURE;
	}

	if let Err(error) = std::fs::create_dir_all(&directory) {
		eprintln!("font-gen: {} could not be created: {error}", directory.display());
		return std::process::ExitCode::FAILURE;
	}
	if let Err(error) = std::fs::write(&face_path, &face).and_then(|()| std::fs::write(&declaration_path, &declaration)) {
		eprintln!("font-gen: the face could not be staged: {error}");
		return std::process::ExitCode::FAILURE;
	}
	println!("font-gen: wrote {} ({} bytes) and its declaration", face_path.display(), face.len());
	std::process::ExitCode::SUCCESS
}

fn repository_root() -> Option<PathBuf> {
	let mut at: PathBuf = std::env::current_dir().ok()?;
	loop {
		if at.join("product.conf").is_file() && at.join("src").is_dir() {
			return Some(at);
		}
		if !at.pop() {
			return None;
		}
	}
}

/// The declaration the catalogue reads, in the vocabulary `font_record` defines.
fn build_declaration(face: &[u8]) -> String {
	let digest = sha256::digest(face);
	let mut hex = String::new();
	for byte in digest {
		hex.push_str(&format!("{byte:02x}"));
	}
	format!(
		"# Authored by `src/tools/font-gen`, which is the only way a face enters this tree: the\n\
		 # licence decision in P02M0136 admits public domain and Unlicense only, and nothing this\n\
		 # project did not write can satisfy it. Regenerate with\n\
		 #   cargo run --manifest-path src/tools/font-gen/Cargo.toml\n\
		 family = {FAMILY}\n\
		 style = {STYLE}\n\
		 format = truetype-glyf\n\
		 face-index = 0\n\
		 weight = {WEIGHT_CLASS}\n\
		 width = normal\n\
		 slant = upright\n\
		 digest = {hex}\n"
	)
}

// ---------------------------------------------------------------------------------------------
// The writer. Big-endian, which is the only endianness this format has.
// ---------------------------------------------------------------------------------------------

fn u8v(out: &mut Vec<u8>, value: u8) {
	out.push(value);
}

fn u16v(out: &mut Vec<u8>, value: u16) {
	out.extend_from_slice(&value.to_be_bytes());
}

fn i16v(out: &mut Vec<u8>, value: i16) {
	out.extend_from_slice(&value.to_be_bytes());
}

fn u32v(out: &mut Vec<u8>, value: u32) {
	out.extend_from_slice(&value.to_be_bytes());
}

// ---------------------------------------------------------------------------------------------
// The glyphs.
// ---------------------------------------------------------------------------------------------

/// One simple glyph out of closed contours of on-curve points.
///
/// EVERY POINT IS ON THE CURVE and every delta is written at full width. A generator that packed
/// deltas into the short forms would be exercising the parser's packing paths with its only staged
/// face, and what this face is for is being READ correctly - not being small.
fn simple_glyph(contours: &[Vec<(i16, i16)>]) -> Vec<u8> {
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

/// The box: an outer contour and an inner one wound the other way, so a non-zero fill leaves a ring.
///
/// The winding is the whole of it. Both contours wound the same way would fill solid under a
/// non-zero rule, and a solid rectangle is not what a reader recognises as "this system has no face
/// for this character".
fn box_glyph() -> Vec<u8> {
	let outer = vec![(BOX_LEFT, BOX_BOTTOM), (BOX_LEFT, BOX_TOP), (BOX_RIGHT, BOX_TOP), (BOX_RIGHT, BOX_BOTTOM)];
	let inner = vec![
		(BOX_LEFT + STROKE, BOX_BOTTOM + STROKE),
		(BOX_RIGHT - STROKE, BOX_BOTTOM + STROKE),
		(BOX_RIGHT - STROKE, BOX_TOP - STROKE),
		(BOX_LEFT + STROKE, BOX_TOP - STROKE),
	];
	simple_glyph(&[outer, inner])
}

// ---------------------------------------------------------------------------------------------
// The tables.
// ---------------------------------------------------------------------------------------------

fn head(index_to_loc_long: bool) -> Vec<u8> {
	let mut out = Vec::new();
	u32v(&mut out, 0x0001_0000);
	u32v(&mut out, 0x0001_0000);
	// Filled in once the whole file exists; it is a checksum OF the file and cannot be known before.
	u32v(&mut out, 0);
	u32v(&mut out, 0x5F0F_3CF5);
	// Baseline at y=0 and left sidebearing at x=0 are both true of this design, and saying so is
	// what lets a consumer trust the bounding box below.
	u16v(&mut out, 0x0003);
	u16v(&mut out, UNITS_PER_EM);
	// Created and modified: a FIXED instant, because a generator that stamped the clock would write
	// a different file every run and the `--check` gate could never pass twice.
	for _ in 0..2 {
		u32v(&mut out, 0);
		u32v(&mut out, 0);
	}
	i16v(&mut out, BOX_LEFT);
	i16v(&mut out, BOX_BOTTOM);
	i16v(&mut out, BOX_RIGHT);
	i16v(&mut out, BOX_TOP);
	// Not bold, not italic, not anything: the style bits and the `OS/2` selection say the same thing
	// and a face whose two answers differ is a face a menu renders wrongly.
	u16v(&mut out, 0);
	u16v(&mut out, 8);
	i16v(&mut out, 2);
	i16v(&mut out, i16::from(index_to_loc_long));
	i16v(&mut out, 0);
	out
}

fn hhea() -> Vec<u8> {
	let mut out = Vec::new();
	u32v(&mut out, 0x0001_0000);
	i16v(&mut out, ASCENDER);
	i16v(&mut out, DESCENDER);
	i16v(&mut out, LINE_GAP);
	u16v(&mut out, BOX_ADVANCE);
	i16v(&mut out, BOX_LEFT);
	i16v(&mut out, BOX_ADVANCE as i16 - BOX_RIGHT);
	i16v(&mut out, BOX_RIGHT);
	i16v(&mut out, 1);
	i16v(&mut out, 0);
	i16v(&mut out, 0);
	for _ in 0..4 {
		i16v(&mut out, 0);
	}
	i16v(&mut out, 0);
	// Every glyph carries its own advance, so there is no trailing run to compress.
	u16v(&mut out, GLYPH_COUNT);
	out
}

fn maxp() -> Vec<u8> {
	let mut out = Vec::new();
	u32v(&mut out, 0x0001_0000);
	u16v(&mut out, GLYPH_COUNT);
	// The box is the largest glyph: eight points in two contours.
	u16v(&mut out, 8);
	u16v(&mut out, 2);
	// No composites, so every composite bound is zero and the depth is zero.
	u16v(&mut out, 0);
	u16v(&mut out, 0);
	u16v(&mut out, 0);
	// No hinting: no zones, no twilight points, no storage, no function or instruction definitions.
	u16v(&mut out, 0);
	u16v(&mut out, 0);
	u16v(&mut out, 0);
	u16v(&mut out, 0);
	u16v(&mut out, 0);
	u16v(&mut out, 0);
	u16v(&mut out, 0);
	u16v(&mut out, 0);
	u16v(&mut out, 0);
	out
}

fn hmtx() -> Vec<u8> {
	let mut out = Vec::new();
	for (advance, bearing) in [(BOX_ADVANCE, BOX_LEFT), (SPACE_ADVANCE, 0), (BOX_ADVANCE, BOX_LEFT)] {
		u16v(&mut out, advance);
		i16v(&mut out, bearing);
	}
	out
}

/// `cmap`: one Windows/BMP format 4 subtable, mapping the whole basic plane to the box.
///
/// FORMAT 4 WITH A GLYPH ARRAY, and the size is the price of the design. Format 4's `idDelta` maps a
/// segment to CONSECUTIVE glyph ids, so a segment cannot send a range to one glyph; the array is how
/// a many-to-one mapping is expressed in the formats this profile admits. Format 13 exists in the
/// specification for exactly this and is NOT in the admitted set, so adding it would be a profile
/// change rather than a face.
fn cmap() -> Vec<u8> {
	let mut subtable = Vec::new();
	// Two segments: the basic plane, and the mandatory 0xFFFF terminator.
	let segment_count: u16 = 2;
	let entries: usize = 0xFFFF;
	// format, length, language
	u16v(&mut subtable, 4);
	let length: usize = 16 + 8 * segment_count as usize + 2 * entries;
	u16v(&mut subtable, length as u16);
	u16v(&mut subtable, 0);
	u16v(&mut subtable, segment_count * 2);
	// searchRange, entrySelector, rangeShift, as the format defines them for two segments.
	u16v(&mut subtable, 4);
	u16v(&mut subtable, 1);
	u16v(&mut subtable, 0);
	// endCode
	u16v(&mut subtable, 0xFFFE);
	u16v(&mut subtable, 0xFFFF);
	u16v(&mut subtable, 0);
	// startCode
	u16v(&mut subtable, 0x0000);
	u16v(&mut subtable, 0xFFFF);
	// idDelta: the first segment's mapping comes from the array, so its delta is zero; the
	// terminator maps 0xFFFF to glyph 0, which is the absence of coverage.
	i16v(&mut subtable, 0);
	i16v(&mut subtable, 1);
	// idRangeOffset: the first segment points at the array that follows the offsets; the second
	// uses its delta.
	u16v(&mut subtable, segment_count * 2);
	u16v(&mut subtable, 0);
	// The array itself: the box everywhere, blank for the space, and NOTHING for the code points
	// that must not be claimed.
	for code in 0..entries as u32 {
		let glyph = match code {
			// U+0000 is not a character anybody renders, and the control range is not coverage
			// either: a face that claimed them would be answering for code points a text stack is
			// supposed to handle before it ever asks a face.
			0x0000..=0x001F | 0x007F..=0x009F => GLYPH_NOTDEF,
			0x0020 | 0x00A0 => GLYPH_SPACE,
			_ => GLYPH_BOX,
		};
		u16v(&mut subtable, glyph);
	}

	let mut out = Vec::new();
	u16v(&mut out, 0);
	u16v(&mut out, 1);
	u16v(&mut out, 3);
	u16v(&mut out, 1);
	u32v(&mut out, 12);
	out.extend_from_slice(&subtable);
	out
}

fn os2(first: u16, last: u16) -> Vec<u8> {
	let mut out = Vec::new();
	// Version 4: the last version whose fields are all defined without the two `OS/2` v5 sizes,
	// which describe optical size ranges this face does not have.
	u16v(&mut out, 4);
	i16v(&mut out, BOX_ADVANCE as i16);
	u16v(&mut out, WEIGHT_CLASS);
	u16v(&mut out, WIDTH_CLASS);
	// Installable embedding: the face is Unlicense, so there is nothing to restrict.
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
	// PANOSE: all zeros is "any", which is the truthful answer for a face of boxes.
	for _ in 0..10 {
		u8v(&mut out, 0);
	}
	// The Unicode and code-page ranges a face CLAIMS. Zero throughout: this face covers the basic
	// plane with one box, and claiming script coverage it does not have would make a selector pick
	// it over a face that could actually draw the script.
	for _ in 0..4 {
		u32v(&mut out, 0);
	}
	out.extend_from_slice(VENDOR);
	// fsSelection: REGULAR, and nothing else. The `head` style bits say the same.
	u16v(&mut out, 0x0040);
	u16v(&mut out, first);
	u16v(&mut out, last);
	i16v(&mut out, ASCENDER);
	i16v(&mut out, DESCENDER);
	i16v(&mut out, LINE_GAP);
	u16v(&mut out, ASCENDER as u16);
	u16v(&mut out, DESCENDER.unsigned_abs());
	u32v(&mut out, 0);
	u32v(&mut out, 0);
	i16v(&mut out, 500);
	i16v(&mut out, 700);
	// The default and break characters, and the longest context any feature needs - none, because
	// this face has no layout tables at all.
	u16v(&mut out, 0);
	u16v(&mut out, 0x0020);
	u16v(&mut out, 0);
	out
}

fn post() -> Vec<u8> {
	let mut out = Vec::new();
	// Version 3.0: no glyph names. The names would be `.notdef`, `space` and a box, and a table of
	// three strings nothing looks up is a table to keep correct for no reader.
	u32v(&mut out, 0x0003_0000);
	u32v(&mut out, 0);
	i16v(&mut out, 0);
	i16v(&mut out, 0);
	// Monospaced, which is true by construction: every covered code point is the same box.
	u32v(&mut out, 1);
	for _ in 0..4 {
		u32v(&mut out, 0);
	}
	out
}

fn name() -> Vec<u8> {
	// name id -> string. The typographic pair (16, 17) is given as well as the legacy pair (1, 2)
	// because the oracle prefers the typographic one and a face that stated only the legacy pair
	// would be checked against a fallback rather than against what it means.
	let records: [(u16, &str); 8] = [(1, FAMILY), (2, STYLE), (3, POSTSCRIPT_NAME), (4, FAMILY), (5, VERSION_STRING), (6, POSTSCRIPT_NAME), (16, FAMILY), (17, STYLE)];
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

// ---------------------------------------------------------------------------------------------
// The file.
// ---------------------------------------------------------------------------------------------

fn table_checksum(bytes: &[u8]) -> u32 {
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

fn build_face() -> Vec<u8> {
	let glyphs: [Vec<u8>; GLYPH_COUNT as usize] = [box_glyph(), Vec::new(), box_glyph()];
	let mut glyf = Vec::new();
	let mut offsets: Vec<u32> = vec![0];
	for glyph in &glyphs {
		glyf.extend_from_slice(glyph);
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

	let tables: Vec<([u8; 4], Vec<u8>)> = vec![
		(*b"OS/2", os2(0x0020, 0xFFFE)),
		(*b"cmap", cmap()),
		(*b"glyf", glyf),
		(*b"head", head(true)),
		(*b"hhea", hhea()),
		(*b"hmtx", hmtx()),
		(*b"loca", loca),
		(*b"maxp", maxp()),
		(*b"name", name()),
		(*b"post", post()),
	];

	let count = tables.len();
	let mut directory = Vec::new();
	u32v(&mut directory, 0x0001_0000);
	u16v(&mut directory, count as u16);
	// searchRange, entrySelector and rangeShift for ten tables: the largest power of two at or below
	// the count is eight.
	u16v(&mut directory, 8 * 16);
	u16v(&mut directory, 3);
	u16v(&mut directory, (count as u16 - 8) * 16);

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

	// THE FILE'S OWN CHECKSUM, WRITTEN LAST BECAUSE IT IS ABOUT THE FINISHED FILE. `checkSumAdjustment`
	// is the constant minus the checksum of everything, computed with that field zero - which is what
	// it already is.
	let adjustment = 0xB1B0_AFBAu32.wrapping_sub(table_checksum(&out));
	out[head_offset + 8..head_offset + 12].copy_from_slice(&adjustment.to_be_bytes());
	// And the `head` record's own checksum was taken over the table with the field zero, which is
	// what the format asks for - so it is left as it is.
	out
}
