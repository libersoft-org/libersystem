use super::*;

use crate::glyf::{Outline, Point};

/// A TrueType font this tree BUILT, so the parser has valid input to be measured against.
///
/// AUTHORED RATHER THAN IMPORTED, and that is deliberate: a real face is a licensed third-party
/// binary and a reviewed import, and none of what is checked here needs one. What this exercises is
/// the FORMAT - a table directory, a `cmap` segment map, a simple outline and a composite one - and
/// a font written here can be made to contradict itself on purpose, which a real one cannot.
mod build {
	/// Big-endian, because the format is.
	fn u16(out: &mut std::vec::Vec<u8>, value: u16) {
		out.extend_from_slice(&value.to_be_bytes());
	}

	fn u32(out: &mut std::vec::Vec<u8>, value: u32) {
		out.extend_from_slice(&value.to_be_bytes());
	}

	/// `head`: the design grid and the `loca` format.
	fn head() -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1); // major
		u16(&mut out, 0); // minor
		u32(&mut out, 0); // font revision
		u32(&mut out, 0); // checksum adjustment
		u32(&mut out, 0x5F0F_3CF5); // magic
		u16(&mut out, 0); // flags
		u16(&mut out, 1000); // units per em
		out.extend_from_slice(&[0u8; 16]); // created, modified
		u16(&mut out, 0); // x min
		u16(&mut out, 0); // y min
		u16(&mut out, 1000); // x max
		u16(&mut out, 1000); // y max
		u16(&mut out, 0); // mac style
		u16(&mut out, 8); // lowest rec ppem
		u16(&mut out, 2); // font direction hint
		u16(&mut out, 0); // index to loc format: short
		u16(&mut out, 0); // glyph data format
		out
	}

	fn hhea(metrics: u16) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, 0);
		u16(&mut out, 800); // ascender
		u16(&mut out, (-200i16) as u16); // descender
		u16(&mut out, 0); // line gap
		// The advance and side-bearing extremes, the caret slope and offset, four reserved fields and
		// the metric data format: twenty-four bytes, and the count follows them at offset 34.
		out.extend_from_slice(&[0u8; 24]);
		u16(&mut out, metrics);
		out
	}

	fn maxp(glyphs: u16) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u32(&mut out, 0x0001_0000);
		u16(&mut out, glyphs);
		out.extend_from_slice(&[0u8; 26]);
		out
	}

	/// One full entry per glyph, which is the simple case the side-bearing run compresses.
	fn hmtx(advances: &[(u16, i16)]) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		for (advance, bearing) in advances {
			u16(&mut out, *advance);
			u16(&mut out, *bearing as u16);
		}
		out
	}

	/// A `cmap` with one format 4 subtable mapping one character.
	pub fn cmap(character: char, glyph: u16) -> std::vec::Vec<u8> {
		let code = character as u16;
		let mut subtable = std::vec::Vec::new();
		u16(&mut subtable, 4); // format
		u16(&mut subtable, 32); // length
		u16(&mut subtable, 0); // language
		u16(&mut subtable, 4); // segCountX2: two segments
		u16(&mut subtable, 4); // search range
		u16(&mut subtable, 1); // entry selector
		u16(&mut subtable, 0); // range shift
		u16(&mut subtable, code); // end codes
		u16(&mut subtable, 0xFFFF);
		u16(&mut subtable, 0); // reserved pad
		u16(&mut subtable, code); // start codes
		u16(&mut subtable, 0xFFFF);
		u16(&mut subtable, glyph.wrapping_sub(code)); // id deltas
		u16(&mut subtable, 1);
		u16(&mut subtable, 0); // id range offsets
		u16(&mut subtable, 0);

		let mut out = std::vec::Vec::new();
		u16(&mut out, 0); // version
		u16(&mut out, 1); // one encoding record
		u16(&mut out, 3); // platform: Windows
		u16(&mut out, 1); // encoding: basic plane
		u32(&mut out, 12); // offset to the subtable
		out.extend_from_slice(&subtable);
		out
	}

	/// A simple glyph: one closed contour through the points given.
	pub fn simple_glyph(points: &[(i16, i16)]) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1); // one contour
		u16(&mut out, 0); // x min
		u16(&mut out, 0); // y min
		u16(&mut out, 1000); // x max
		u16(&mut out, 1000); // y max
		u16(&mut out, points.len() as u16 - 1); // the contour's last point
		u16(&mut out, 0); // no instructions
		// One flag byte per point: on-curve, with the short forms whose sign bit says the direction.
		let mut previous = (0i16, 0i16);
		let mut flags = std::vec::Vec::new();
		let mut xs = std::vec::Vec::new();
		let mut ys = std::vec::Vec::new();
		for (x, y) in points {
			let dx = x - previous.0;
			let dy = y - previous.1;
			let mut flag = 0x01u8; // on curve
			if dx.abs() < 256 {
				flag |= 0x02;
				if dx >= 0 {
					flag |= 0x10;
				}
				xs.push(dx.unsigned_abs() as u8);
			} else {
				xs.extend_from_slice(&dx.to_be_bytes());
			}
			if dy.abs() < 256 {
				flag |= 0x04;
				if dy >= 0 {
					flag |= 0x20;
				}
				ys.push(dy.unsigned_abs() as u8);
			} else {
				ys.extend_from_slice(&dy.to_be_bytes());
			}
			flags.push(flag);
			previous = (*x, *y);
		}
		out.extend_from_slice(&flags);
		out.extend_from_slice(&xs);
		out.extend_from_slice(&ys);
		out
	}

	/// A composite glyph with several components, which is how an expansion is made to multiply.
	pub fn composite_glyphs(components: &[(u16, i16, i16)]) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, (-1i16) as u16);
		u16(&mut out, 0);
		u16(&mut out, 0);
		u16(&mut out, 1000);
		u16(&mut out, 1000);
		for (index, (component, dx, dy)) in components.iter().enumerate() {
			let more = if index + 1 < components.len() { 0x0020 } else { 0 };
			u16(&mut out, 0x0001 | 0x0002 | more);
			u16(&mut out, *component);
			u16(&mut out, *dx as u16);
			u16(&mut out, *dy as u16);
		}
		out
	}

	/// A composite glyph: one component, placed by offset.
	pub fn composite_glyph(component: u16, dx: i16, dy: i16) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, (-1i16) as u16); // a negative contour count IS the composite marker
		u16(&mut out, 0);
		u16(&mut out, 0);
		u16(&mut out, 1000);
		u16(&mut out, 1000);
		u16(&mut out, 0x0001 | 0x0002); // words, and the arguments are offsets
		u16(&mut out, component);
		u16(&mut out, dx as u16);
		u16(&mut out, dy as u16);
		out
	}

	/// A `name` table with the records given: (platform, encoding, language, name id, the string).
	///
	/// THE STRING IS ENCODED THE WAY THE PLATFORM SAYS, because that is what a real font does and
	/// what makes the record-preference rule worth testing.
	pub fn name_table(records: &[(u16, u16, u16, u16, &str)]) -> std::vec::Vec<u8> {
		let mut storage = std::vec::Vec::new();
		let mut entries = std::vec::Vec::new();
		for (platform, encoding, language, name, text) in records {
			let bytes: std::vec::Vec<u8> = if *platform == 1 {
				text.chars().map(|character| character as u8).collect()
			} else {
				let mut out = std::vec::Vec::new();
				for unit in text.encode_utf16() {
					out.extend_from_slice(&unit.to_be_bytes());
				}
				out
			};
			entries.push((*platform, *encoding, *language, *name, storage.len(), bytes.len()));
			storage.extend_from_slice(&bytes);
		}
		let mut out = std::vec::Vec::new();
		u16(&mut out, 0); // format 0
		u16(&mut out, entries.len() as u16);
		u16(&mut out, (6 + entries.len() * 12) as u16);
		for (platform, encoding, language, name, offset, length) in &entries {
			u16(&mut out, *platform);
			u16(&mut out, *encoding);
			u16(&mut out, *language);
			u16(&mut out, *name);
			u16(&mut out, *length as u16);
			u16(&mut out, *offset as u16);
		}
		out.extend_from_slice(&storage);
		out
	}

	/// An `OS/2` table at the version given.
	pub fn os2(version: u16, weight: u16, width: u16, selection: u16, x_height: i16) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, version);
		u16(&mut out, 500); // average character width
		u16(&mut out, weight);
		u16(&mut out, width);
		u16(&mut out, 0); // embedding permissions
		out.extend_from_slice(&[0u8; 16]); // the sub- and superscript sizes and offsets
		out.extend_from_slice(&[0u8; 6]); // the strikeout and the family class
		out.extend_from_slice(&[0u8; 10]); // PANOSE
		out.extend_from_slice(&[0u8; 16]); // the unicode ranges
		out.extend_from_slice(b"TEST");
		u16(&mut out, selection);
		u16(&mut out, 32); // first character
		u16(&mut out, 0xFFFF); // last character
		u16(&mut out, 800); // typographic ascender
		u16(&mut out, (-200i16) as u16); // typographic descender
		u16(&mut out, 100); // typographic line gap
		u16(&mut out, 900); // windows ascent
		u16(&mut out, 250); // windows descent
		if version >= 1 {
			out.extend_from_slice(&[0u8; 8]); // the code page ranges
		}
		if version >= 2 {
			u16(&mut out, x_height as u16);
			u16(&mut out, 700); // cap height
			u16(&mut out, 0); // default character
			u16(&mut out, 32); // break character
			u16(&mut out, 0); // maximum context
		}
		out
	}

	/// A `post` table. `names` are the glyph names, or empty for a version 3.0 table with none.
	pub fn post(names: &[&str]) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u32(&mut out, if names.is_empty() { 0x0003_0000 } else { 0x0002_0000 });
		u32(&mut out, (-12i32 << 16) as u32); // italic angle: twelve degrees, and NEGATIVE
		u16(&mut out, (-100i16) as u16); // underline position
		u16(&mut out, 50); // underline thickness
		u32(&mut out, 1); // fixed pitch
		out.extend_from_slice(&[0u8; 16]); // the four memory fields nothing reads
		if names.is_empty() {
			return out;
		}
		u16(&mut out, names.len() as u16);
		let mut strings = std::vec::Vec::new();
		let mut custom = 0usize;
		for name in names {
			// A NAME THE STANDARD LIST ALREADY HAS COSTS NO STORAGE, which is the point of the list.
			match crate::metadata::STANDARD_NAMES.iter().position(|standard| standard == name) {
				Some(index) => u16(&mut out, index as u16),
				None => {
					u16(&mut out, (258 + custom) as u16);
					custom += 1;
					strings.push(name.len() as u8);
					strings.extend_from_slice(name.as_bytes());
				}
			}
		}
		out.extend_from_slice(&strings);
		out
	}

	/// Assemble a font out of its tables.
	pub fn font(tables: &[([u8; 4], std::vec::Vec<u8>)]) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u32(&mut out, 0x0001_0000);
		u16(&mut out, tables.len() as u16);
		u16(&mut out, 0);
		u16(&mut out, 0);
		u16(&mut out, 0);
		// Every table is padded to four bytes, which is what the format asks for and what a short
		// `loca` needs to stay aligned.
		let mut offset = 12 + tables.len() * 16;
		let mut body = std::vec::Vec::new();
		for (tag, bytes) in tables {
			out.extend_from_slice(tag);
			u32(&mut out, 0);
			u32(&mut out, offset as u32);
			u32(&mut out, bytes.len() as u32);
			body.extend_from_slice(bytes);
			while body.len() % 4 != 0 {
				body.push(0);
			}
			offset = 12 + tables.len() * 16 + body.len();
		}
		out.extend_from_slice(&body);
		out
	}

	/// The whole font this tree's fixtures are measured against: two real glyphs and a composite.
	pub fn sample() -> std::vec::Vec<u8> {
		let square = simple_glyph(&[(100, 100), (900, 100), (900, 900), (100, 900)]);
		let triangle = simple_glyph(&[(100, 100), (900, 100), (500, 900)]);
		let composite = composite_glyph(1, 50, 60);
		let mut glyf = std::vec::Vec::new();
		let mut loca = std::vec::Vec::new();
		// `loca` has one entry per glyph PLUS one: entry `i` is where glyph `i` starts and entry
		// `i + 1` is where it ends. Glyph 0 is EMPTY - the notdef of a font built for a test - so its
		// two entries are equal, and an empty glyph is not an error.
		u16(&mut loca, 0);
		u16(&mut loca, 0);
		for glyph in [&square, &triangle, &composite] {
			glyf.extend_from_slice(glyph);
			while glyf.len() % 2 != 0 {
				glyf.push(0);
			}
			u16(&mut loca, (glyf.len() / 2) as u16);
		}
		font(&[
			(*b"cmap", cmap('A', 1)),
			(*b"glyf", glyf),
			(*b"head", head()),
			(*b"hhea", hhea(4)),
			(*b"hmtx", hmtx(&[(500, 0), (1000, 100), (800, 100), (1000, 150)])),
			(*b"loca", loca),
			(*b"maxp", maxp(4)),
		])
	}
}

/// An outline walk that counts what it was given, which is all a parser fixture needs to know.
#[derive(Default)]
struct Counter {
	points: usize,
	contours: usize,
	first: Option<Point>,
}

impl Outline for Counter {
	fn point(&mut self, point: Point) -> bool {
		self.points += 1;
		if self.first.is_none() {
			self.first = Some(point);
		}
		if point.ends_contour {
			self.contours += 1;
		}
		true
	}
}

#[test]
// THE POSITIVE CASE FIRST, because everything below is about refusing - and a parser that refused
// everything would pass every one of those and be useless.
fn a_well_formed_font_is_read() {
	let bytes = build::sample();
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	assert_eq!(face.header.units_per_em, 1000);
	assert!(!face.header.long_loca);
	assert_eq!(face.metrics.glyph_count, 4);
	assert_eq!(face.metrics.ascender, 800);
	assert_eq!(face.metrics.descender, -200);
	assert_eq!(face.advance(1).expect("glyph 1"), (1000, 100));
	assert_eq!(face.glyph_for('A').expect("a readable cmap"), Some(1));
	assert_eq!(face.glyph_for('B').expect("a readable cmap"), None, "a character the font has no glyph for is not an error");

	let mut counter = Counter::default();
	crate::glyf::walk(&face, 1, &mut counter).expect("a square");
	assert_eq!(counter.points, 4);
	assert_eq!(counter.contours, 1);
	assert_eq!(counter.first, Some(Point { x: 100, y: 100, on_curve: true, ends_contour: false }));

	let mut counter = Counter::default();
	crate::glyf::walk(&face, 2, &mut counter).expect("a triangle");
	assert_eq!(counter.points, 3);

	// AN EMPTY GLYPH IS NOT AN ERROR. A space has no outline, and reading that as a malformed glyph
	// is how a font with a space in it stops rendering.
	let mut counter = Counter::default();
	crate::glyf::walk(&face, 0, &mut counter).expect("the empty glyph");
	assert_eq!(counter.points, 0);
}

#[test]
// A COMPOSITE IS THE ONE PLACE RECURSION ENTERS A FONT PARSER, so it is walked, and its component's
// points come back moved by the offset the composite states.
fn a_composite_glyph_is_resolved_through_its_component() {
	let bytes = build::sample();
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut counter = Counter::default();
	crate::glyf::walk(&face, 3, &mut counter).expect("a composite");
	assert_eq!(counter.points, 4, "the component's four points");
	assert_eq!(counter.first, Some(Point { x: 150, y: 160, on_curve: true, ends_contour: false }), "moved by the composite's offset");
}

#[test]
// EVERY TRUNCATION OF A VALID FONT. A file that stops in the middle of any structure must come back
// as a refusal - never a panic, and never a read past the end of what was given.
fn a_font_truncated_anywhere_is_refused_rather_than_read_past() {
	let bytes = build::sample();
	for length in 0..bytes.len() {
		let truncated = &bytes[..length];
		exercise(truncated);
	}
}

#[test]
// EVERY SINGLE BYTE FLIPPED. This is the fuzz the item asks for, and it is exhaustive rather than
// random at this size: a font of a few hundred bytes has a few hundred single-byte mutations, and
// running all of them is cheaper than arguing about which ones matter.
fn a_font_with_any_single_byte_changed_is_refused_rather_than_crashed_on() {
	let bytes = build::sample();
	for index in 0..bytes.len() {
		for pattern in [0xFFu8, 0x01, 0x80, 0x7F] {
			let mut mutated = bytes.clone();
			mutated[index] ^= pattern;
			exercise(&mutated);
		}
	}
}

#[test]
// AND THE SHAPES A CRAFTED FONT ACTUALLY TAKES, named one at a time rather than left to the fuzz to
// stumble on: each of these is a structure that contradicts itself in a way that would make an
// unchecked parser read somewhere it was not given.
fn the_crafted_shapes_are_each_refused_by_name() {
	// A table directory entry pointing past the end of the file. Patched on `head`, because that is
	// a table `open` actually reads - an out-of-bounds entry for a table nobody opens is not an
	// error until somebody opens it, and asserting otherwise would be asserting eagerness this
	// parser deliberately does not have.
	let mut bytes = build::sample();
	let entry = find_entry(&bytes, b"head") + 8;
	bytes[entry..entry + 4].copy_from_slice(&0xFFFF_0000u32.to_be_bytes());
	assert!(matches!(Face::open(&bytes, 0), Err(Error::Malformed(Malformed::TableOutOfBounds { .. }))));

	// A file that is not a font at all.
	assert_eq!(Face::open(b"not a font", 0), Err(Error::Malformed(Malformed::NotAFont)));
	assert_eq!(Face::open(&[], 0), Err(Error::Malformed(Malformed::NotAFont)));

	// A face index on a single-face file: the declaration says something the file does not.
	let bytes = build::sample();
	assert_eq!(Face::open(&bytes, 3), Err(Error::Malformed(Malformed::NoSuchFace { index: 3, faces: 1 })));

	// A design grid of zero, which every scaled metric would divide by.
	let mut bytes = build::sample();
	let head_at = find_table(&bytes, b"head");
	bytes[head_at + 18..head_at + 20].copy_from_slice(&0u16.to_be_bytes());
	assert_eq!(Face::open(&bytes, 0), Err(Error::Malformed(Malformed::InconsistentTable { table: *b"head" })));

	// A `loca` format the field has no third value for.
	let mut bytes = build::sample();
	bytes[head_at + 50..head_at + 52].copy_from_slice(&7u16.to_be_bytes());
	assert_eq!(Face::open(&bytes, 0), Err(Error::Malformed(Malformed::InconsistentTable { table: *b"head" })));

	// More horizontal metrics than there are glyphs: a `hmtx` read past the glyphs it is about.
	let mut bytes = build::sample();
	let hhea_at = find_table(&bytes, b"hhea");
	bytes[hhea_at + 34..hhea_at + 36].copy_from_slice(&999u16.to_be_bytes());
	assert_eq!(Face::open(&bytes, 0), Err(Error::Malformed(Malformed::InconsistentTable { table: *b"hhea" })));

	// A table the profile does not admit is refused as UNSUPPORTED rather than as malformed: the
	// font is fine, and this system has decided not to read it.
	let bytes = build::sample();
	let face = Face::open(&bytes, 0).expect("valid");
	assert_eq!(face.table(b"morx"), Err(Error::Unsupported(opentype_profile::Unsupported::Table(*b"morx"))));
	assert_eq!(face.table(b"SVG "), Err(Error::Unsupported(opentype_profile::Unsupported::Table(*b"SVG "))));
	// And a table the profile admits but the font does not carry is simply absent.
	assert_eq!(face.table(b"GSUB").expect("a readable directory").is_none(), true);
}

#[test]
// A COMPONENT THAT IS ITS OWN PARENT IS A LOOP, and the depth bound alone would only make it a slow
// one. This is the shape that makes an unbounded parser recurse until the stack ends.
fn a_composite_that_refers_to_itself_is_refused() {
	let square = build::simple_glyph(&[(100, 100), (900, 100), (900, 900), (100, 900)]);
	let looping = build::composite_glyph(1, 0, 0); // glyph 1 refers to glyph 1
	let mut glyf = std::vec::Vec::new();
	let mut loca = std::vec::Vec::new();
	loca.extend_from_slice(&0u16.to_be_bytes());
	for glyph in [&square, &looping] {
		glyf.extend_from_slice(glyph);
		while glyf.len() % 2 != 0 {
			glyf.push(0);
		}
		loca.extend_from_slice(&((glyf.len() / 2) as u16).to_be_bytes());
	}
	// Glyph 0 is the looping composite, and it refers to glyph 1 - which is itself a composite
	// referring back. Two glyphs, and `loca` therefore has three entries.
	let self_referring = build::composite_glyph(0, 0, 0);
	let mut glyf = std::vec::Vec::new();
	let mut loca = std::vec::Vec::new();
	loca.extend_from_slice(&0u16.to_be_bytes());
	for glyph in [&looping, &self_referring] {
		glyf.extend_from_slice(glyph);
		while glyf.len() % 2 != 0 {
			glyf.push(0);
		}
		loca.extend_from_slice(&((glyf.len() / 2) as u16).to_be_bytes());
	}
	let bytes = build::font(&[
		(*b"cmap", build::cmap('A', 1)),
		(*b"glyf", glyf),
		(*b"head", {
			let sample = build::sample();
			let at = find_table(&sample, b"head");
			sample[at..at + 54].to_vec()
		}),
		(*b"hhea", {
			let sample = build::sample();
			let at = find_table(&sample, b"hhea");
			let mut hhea = sample[at..at + 36].to_vec();
			// Two glyphs, so two horizontal metrics: a count above the glyph count is refused by the
			// parser before it ever reaches the glyph this fixture is about.
			hhea[34..36].copy_from_slice(&2u16.to_be_bytes());
			hhea
		}),
		(*b"hmtx", std::vec![0u8; 8]),
		(*b"loca", loca),
		(*b"maxp", {
			let sample = build::sample();
			let at = find_table(&sample, b"maxp");
			let mut maxp = sample[at..at + 32].to_vec();
			maxp[4..6].copy_from_slice(&2u16.to_be_bytes());
			maxp
		}),
	]);
	let face = Face::open(&bytes, 0).expect("a font with a loop in it is still a readable font");
	let mut counter = Counter::default();
	// A TWO-GLYPH CYCLE IS NOT A SELF-REFERENCE, so the name check cannot see it and the DEPTH
	// ceiling is what stops it - which is the whole reason a depth ceiling exists beside the name
	// check. The refusal says which ceiling was met and by how much.
	assert_eq!(crate::glyf::walk(&face, 0, &mut counter), Err(Error::Unsupported(crate::Unsupported::Exceeded { limit: "composite depth", ceiling: opentype_profile::limits::COMPOSITE_DEPTH, asked: opentype_profile::limits::COMPOSITE_DEPTH as u64 + 1 })));

	// AND A GLYPH THAT REFERS TO ITSELF IS STILL REFUSED BY NAME, one level down rather than five:
	// the depth ceiling alone would make it a slow refusal instead of an immediate one.
	let itself = build::composite_glyph(0, 0, 0);
	let mut glyf = std::vec::Vec::new();
	let mut loca = std::vec::Vec::new();
	loca.extend_from_slice(&0u16.to_be_bytes());
	glyf.extend_from_slice(&itself);
	while glyf.len() % 2 != 0 {
		glyf.push(0);
	}
	loca.extend_from_slice(&((glyf.len() / 2) as u16).to_be_bytes());
	loca.extend_from_slice(&((glyf.len() / 2) as u16).to_be_bytes());
	let sample = build::sample();
	let mut hhea = table_bytes(&sample, b"hhea");
	hhea[34..36].copy_from_slice(&2u16.to_be_bytes());
	let mut maxp = table_bytes(&sample, b"maxp");
	maxp[4..6].copy_from_slice(&2u16.to_be_bytes());
	let bytes = build::font(&[
		(*b"cmap", build::cmap('A', 1)),
		(*b"glyf", glyf),
		(*b"head", table_bytes(&sample, b"head")),
		(*b"hhea", hhea),
		(*b"hmtx", std::vec![0u8; 8]),
		(*b"loca", loca),
		(*b"maxp", maxp),
	]);
	let face = Face::open(&bytes, 0).expect("a font with a loop in it is still a readable font");
	let mut counter = Counter::default();
	assert_eq!(crate::glyf::walk(&face, 0, &mut counter), Err(Error::Malformed(Malformed::BadGlyph { glyph: 0 })));
}

/// Where a table's DIRECTORY ENTRY is in a font this tree built.
fn find_entry(bytes: &[u8], tag: &[u8; 4]) -> usize {
	let count = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
	for index in 0..count {
		let entry = 12 + index * 16;
		if &bytes[entry..entry + 4] == tag {
			return entry;
		}
	}
	panic!("the fixture's own font has no {} table", core::str::from_utf8(tag).unwrap_or("????"));
}

/// Where a table's bytes start in a font this tree built.
fn find_table(bytes: &[u8], tag: &[u8; 4]) -> usize {
	let count = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
	for index in 0..count {
		let entry = 12 + index * 16;
		if &bytes[entry..entry + 4] == tag {
			return u32::from_be_bytes([bytes[entry + 8], bytes[entry + 9], bytes[entry + 10], bytes[entry + 11]]) as usize;
		}
	}
	panic!("the fixture's own font has no {} table", core::str::from_utf8(tag).unwrap_or("????"));
}

/// A whole table, copied out of a font this tree built - length included, so a fixture that borrows
/// one does not have to repeat a byte count that the directory already states.
fn table_bytes(bytes: &[u8], tag: &[u8; 4]) -> std::vec::Vec<u8> {
	let entry = find_entry(bytes, tag);
	let at = u32::from_be_bytes([bytes[entry + 8], bytes[entry + 9], bytes[entry + 10], bytes[entry + 11]]) as usize;
	let length = u32::from_be_bytes([bytes[entry + 12], bytes[entry + 13], bytes[entry + 14], bytes[entry + 15]]) as usize;
	bytes[at..at + length].to_vec()
}

/// Put a byte string through everything the parser does, and require that it ANSWERS.
///
/// WHAT IS ASSERTED IS THAT IT RETURNS. Whether a mutated font is readable is not the question - some
/// mutations produce a font that is still perfectly valid - and the test would be worthless if it
/// demanded a particular answer. What must never happen is a panic, an out-of-bounds read or a walk
/// that does not come back, and that is what running every path over every mutation checks.
fn exercise(bytes: &[u8]) {
	let Ok(face) = Face::open(bytes, 0) else { return };
	let _ = face.advance(0);
	let _ = face.advance(1);
	let _ = face.advance(u16::MAX);
	let _ = face.glyph_for('A');
	let _ = face.glyph_for('\u{10FFFF}');
	let _ = face.table(b"GSUB");
	for glyph in 0..face.metrics.glyph_count.min(8) {
		let mut counter = Counter::default();
		let _ = crate::glyf::walk(&face, glyph, &mut counter);
	}
}

/// The variable-font halves of the fixture font.
mod variable {
	pub fn u16(out: &mut std::vec::Vec<u8>, value: u16) {
		out.extend_from_slice(&value.to_be_bytes());
	}

	pub fn i16(out: &mut std::vec::Vec<u8>, value: i16) {
		out.extend_from_slice(&value.to_be_bytes());
	}

	pub fn u32(out: &mut std::vec::Vec<u8>, value: u32) {
		out.extend_from_slice(&value.to_be_bytes());
	}

	/// `fvar` with one weight axis and one named instance at its top.
	pub fn fvar() -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1); // major
		u16(&mut out, 0); // minor
		u16(&mut out, 16); // the axis array follows this header
		u16(&mut out, 2); // reserved
		u16(&mut out, 1); // one axis
		u16(&mut out, 20); // each 20 bytes
		u16(&mut out, 1); // one named instance
		u16(&mut out, 8); // each: a name id, flags, and one coordinate
		// The axis: weight, 100 to 900, default 400 - a default that is NOT the midpoint, which is
		// what makes the two halves of the normalisation scale separately.
		out.extend_from_slice(b"wght");
		u32(&mut out, 100 << 16);
		u32(&mut out, 400 << 16);
		u32(&mut out, 900 << 16);
		u16(&mut out, 0); // flags
		u16(&mut out, 256); // name id
		// The instance: "Bold" at 700.
		u16(&mut out, 257); // name id
		u16(&mut out, 0); // flags
		u32(&mut out, 700 << 16);
		out
	}

	/// `avar` mapping the middle of the axis somewhere else.
	pub fn avar() -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, 0);
		u16(&mut out, 0); // reserved
		u16(&mut out, 1); // one segment map, for the one axis
		u16(&mut out, 3); // three pairs
		i16(&mut out, -16384);
		i16(&mut out, -16384);
		i16(&mut out, 0);
		i16(&mut out, 0);
		i16(&mut out, 16384);
		i16(&mut out, 16384);
		out
	}

	/// An item variation store: one region over the top half of the axis, one delta per item.
	///
	/// THE SHARED DELTA STORAGE. `HVAR`, `MVAR`, `VVAR`, `GDEF` and CFF2 all read through this same
	/// structure, so a fixture that built it twice would be checking two readers instead of one.
	pub fn item_store(deltas: &[i16]) -> std::vec::Vec<u8> {
		// One region: start 0, peak 16384, end 16384 - the top half of the axis.
		let mut regions = std::vec::Vec::new();
		u16(&mut regions, 1); // one axis
		u16(&mut regions, 1); // one region
		i16(&mut regions, 0);
		i16(&mut regions, 16384);
		i16(&mut regions, 16384);

		// One item variation data: one short delta per item.
		let mut data = std::vec::Vec::new();
		u16(&mut data, deltas.len() as u16);
		u16(&mut data, 1); // the first one region's deltas are shorts
		u16(&mut data, 1); // one region
		u16(&mut data, 0); // region index 0
		for delta in deltas {
			i16(&mut data, *delta);
		}

		// The store: format, the region list, and one data offset.
		// format(2), the region list offset(4), the data count(2), and one data offset(4): twelve.
		let store_header = 12usize;
		let regions_at = store_header;
		let data_at = regions_at + regions.len();
		let mut store = std::vec::Vec::new();
		u16(&mut store, 1);
		u32(&mut store, regions_at as u32);
		u16(&mut store, 1);
		u32(&mut store, data_at as u32);
		store.extend_from_slice(&regions);
		store.extend_from_slice(&data);
		store
	}

	/// `MVAR` varying two font-wide metrics at the top of the axis.
	pub fn mvar(ascender: i16, descender: i16) -> std::vec::Vec<u8> {
		let store = item_store(&[ascender, descender]);
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1); // major
		u16(&mut out, 0); // minor
		u16(&mut out, 0); // reserved
		u16(&mut out, 8); // each value record is a tag and two indices
		u16(&mut out, 2); // two of them
		// The header is twelve bytes and the two records sixteen, so the store follows at
		// twenty-eight - and the offset is a SHORT one, unlike `HVAR`'s.
		u16(&mut out, 28);
		out.extend_from_slice(b"hasc");
		u16(&mut out, 0);
		u16(&mut out, 0);
		out.extend_from_slice(b"hdsc");
		u16(&mut out, 0);
		u16(&mut out, 1);
		out.extend_from_slice(&store);
		out
	}

	/// `HVAR` giving glyph 1 an advance delta at the top of the axis.
	pub fn hvar(delta: i16) -> std::vec::Vec<u8> {
		let store = item_store(&[0, delta]);

		// The header is a version and FOUR offsets - the store, the advance map and the two side
		// bearing maps - which is twenty bytes, and stating twelve points the store at its own
		// middle.
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, 0);
		u32(&mut out, 20); // the store follows this header
		u32(&mut out, 0); // no delta-set index map: the glyph id IS the index
		u32(&mut out, 0); // no left side bearing map
		u32(&mut out, 0); // no right side bearing map
		out.extend_from_slice(&store);
		out
	}

	/// One tuple's worth of glyph variation data: a shared peak, private point numbers, and deltas.
	fn glyph_data(serialised: &[u8]) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1); // one tuple, and no shared point numbers
		u16(&mut out, 8); // the serialised half follows the one tuple header
		u16(&mut out, serialised.len() as u16);
		// Shared tuple 0 - the top of the axis - with this tuple's OWN point numbers.
		u16(&mut out, 0x2000);
		out.extend_from_slice(serialised);
		while out.len() % 2 != 0 {
			out.push(0);
		}
		out
	}

	/// `gvar` for the sample font: a square whose right side moves, a triangle with an interpolated
	/// point and a varying advance, and a composite whose component is displaced.
	pub fn gvar() -> std::vec::Vec<u8> {
		// THE SQUARE: points 1 and 3 are referenced and points 0 and 2 are INFERRED from them, which
		// is the rule a parser that only moved what was listed would tear the outline apart on.
		let square = {
			let mut out = std::vec::Vec::new();
			out.extend_from_slice(&[0x02, 0x01, 0x01, 0x02]); // two point numbers: 1 and 3
			out.extend_from_slice(&[0x01, 100, 0]); // x: point 1 by 100, point 3 by nothing
			out.extend_from_slice(&[0x41, 0x00, 0x00, 0x00, 0xC8]); // y: 0 and 200, as words
			out
		};
		// THE TRIANGLE: two points and one PHANTOM point, which is where the advance variation of a
		// font without `HVAR` lives.
		let triangle = {
			let mut out = std::vec::Vec::new();
			out.extend_from_slice(&[0x03, 0x02, 0x00, 0x01, 0x03]); // point numbers 0, 1 and 4
			out.extend_from_slice(&[0x42, 0x00, 0x00, 0x01, 0x2C, 0x00, 0xFA]); // x: 0, 300, 250
			out.extend_from_slice(&[0x82]); // y: three zeroes, which occupy no bytes at all
			out
		};
		// THE COMPOSITE: entry 0 is its one COMPONENT, not a point of it.
		let composite = {
			let mut out = std::vec::Vec::new();
			out.extend_from_slice(&[0x01, 0x00, 0x00]); // one entry: component 0
			out.extend_from_slice(&[0x00, 0xD8]); // x: minus forty
			out.extend_from_slice(&[0x00, 0x46]); // y: seventy
			out
		};
		let bodies = [glyph_data(&square), glyph_data(&triangle), glyph_data(&composite)];

		let mut data = std::vec::Vec::new();
		let mut offsets = std::vec::Vec::new();
		// Glyph 0 is empty and does not vary, which is an empty RANGE rather than an absent entry.
		offsets.push(0usize);
		offsets.push(0usize);
		for body in &bodies {
			data.extend_from_slice(body);
			offsets.push(data.len());
		}

		let mut shared = std::vec::Vec::new();
		i16(&mut shared, 16384); // one shared tuple, peaking at the top of the one axis

		let header = 20usize;
		let offsets_length = offsets.len() * 2;
		let shared_at = header + offsets_length;
		let data_at = shared_at + shared.len();

		let mut out = std::vec::Vec::new();
		u16(&mut out, 1); // major
		u16(&mut out, 0); // minor
		u16(&mut out, 1); // one axis
		u16(&mut out, 1); // one shared tuple
		u32(&mut out, shared_at as u32);
		u16(&mut out, 4); // four glyphs
		u16(&mut out, 0); // short offsets, which store HALF the offset
		u32(&mut out, data_at as u32);
		for offset in &offsets {
			u16(&mut out, (offset / 2) as u16);
		}
		out.extend_from_slice(&shared);
		out.extend_from_slice(&data);
		out
	}
}

#[test]
// A NAMED INSTANCE IS A POSITION AND NOT A SEPARATE FACE, which is what makes "Bold" in a variable
// font the same face at a coordinate - and why a catalogue that listed the instances as faces would
// publish four faces backed by one file.
fn a_variable_font_declares_its_axes_and_its_named_instances() {
	let bytes = build::font(&[
		(*b"fvar", variable::fvar()),
		(*b"head", {
			let sample = build::sample();
			let at = find_table(&sample, b"head");
			sample[at..at + 54].to_vec()
		}),
		(*b"hhea", {
			let sample = build::sample();
			let at = find_table(&sample, b"hhea");
			sample[at..at + 36].to_vec()
		}),
		(*b"hmtx", std::vec![0u8; 16]),
		(*b"maxp", {
			let sample = build::sample();
			let at = find_table(&sample, b"maxp");
			sample[at..at + 32].to_vec()
		}),
	]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let variations = Variations::of(&face).expect("readable").expect("a variable font");
	assert_eq!(variations.axis_count, 1);
	assert_eq!(variations.instance_count, 1);
	let axis = variations.axis(0).expect("the weight axis");
	assert_eq!(axis.tag, *b"wght");
	assert_eq!((axis.minimum >> 16, axis.default >> 16, axis.maximum >> 16), (100, 400, 900));
	let mut coordinates = [0i32; 1];
	variations.instance(0, &mut coordinates).expect("the named instance");
	assert_eq!(coordinates[0] >> 16, 700);
	// An axis index the font does not have is a refusal rather than a default.
	assert!(variations.axis(1).is_err());
}

#[test]
// THE TWO HALVES OF AN AXIS SCALE SEPARATELY, because a default is almost never the midpoint: weight
// runs 100 to 900 with a default of 400, so 700 is not three quarters of the way up.
fn a_user_coordinate_normalises_with_each_half_of_the_axis_scaled_on_its_own() {
	let bytes = build::font(&[
		(*b"fvar", variable::fvar()),
		(*b"head", {
			let sample = build::sample();
			let at = find_table(&sample, b"head");
			sample[at..at + 54].to_vec()
		}),
		(*b"hhea", {
			let sample = build::sample();
			let at = find_table(&sample, b"hhea");
			sample[at..at + 36].to_vec()
		}),
		(*b"hmtx", std::vec![0u8; 16]),
		(*b"maxp", {
			let sample = build::sample();
			let at = find_table(&sample, b"maxp");
			sample[at..at + 32].to_vec()
		}),
	]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let variations = Variations::of(&face).expect("readable").expect("a variable font");
	// The default is zero, the ends are the full range.
	assert_eq!(variations.normalise(0, 400 << 16).expect("in range"), 0);
	assert_eq!(variations.normalise(0, 100 << 16).expect("in range"), -16384);
	assert_eq!(variations.normalise(0, 900 << 16).expect("in range"), 16384);
	// 650 is HALF WAY between the default and the maximum - and a single linear scale over 100..900
	// would have called it 68% of the way up instead.
	assert_eq!(variations.normalise(0, 650 << 16).expect("in range"), 8192);
	// 700 is 300 of the 500 above the default, which is 60% - not the 75% a single scale would say.
	assert_eq!(variations.normalise(0, 700 << 16).expect("in range"), 9830);
	// And 250 is half way down the lower half, which is a different span again.
	assert_eq!(variations.normalise(0, 250 << 16).expect("in range"), -8192);
	// AND A COORDINATE OUTSIDE THE AXIS IS CLAMPED rather than extrapolated: a font asked for a
	// weight it does not have gives its heaviest, not something it never drew.
	assert_eq!(variations.normalise(0, 2000 << 16).expect("clamped"), 16384);
	assert_eq!(variations.normalise(0, 0).expect("clamped"), -16384);
	// A FONT WITH NO `avar` IS UNCHANGED BY IT, which is what most variable fonts are.
	assert_eq!(variations.adjust(&face, 0, 8192).expect("readable"), 8192);
}

#[test]
// `avar` IS HOW A FONT SAYS THE MIDDLE OF ITS AXIS IS NOT THE MIDDLE OF ITS DESIGN. Skipping it puts
// an instance somewhere else on the axis than the one that was asked for - on a font with a
// non-linear weight axis, visibly the wrong weight rather than a rounding difference.
fn the_axis_mapping_moves_a_coordinate_where_the_font_says() {
	let head = {
		let sample = build::sample();
		let at = find_table(&sample, b"head");
		sample[at..at + 54].to_vec()
	};
	let hhea = {
		let sample = build::sample();
		let at = find_table(&sample, b"hhea");
		sample[at..at + 36].to_vec()
	};
	let maxp = {
		let sample = build::sample();
		let at = find_table(&sample, b"maxp");
		sample[at..at + 32].to_vec()
	};
	// An `avar` that maps the middle of the upper half to three quarters of the way up: a font
	// saying its weight axis is not linear.
	let mut avar = std::vec::Vec::new();
	variable::u16(&mut avar, 1);
	variable::u16(&mut avar, 0);
	variable::u16(&mut avar, 0);
	variable::u16(&mut avar, 1);
	variable::u16(&mut avar, 4);
	variable::i16(&mut avar, -16384);
	variable::i16(&mut avar, -16384);
	variable::i16(&mut avar, 0);
	variable::i16(&mut avar, 0);
	variable::i16(&mut avar, 8192);
	variable::i16(&mut avar, 12288);
	variable::i16(&mut avar, 16384);
	variable::i16(&mut avar, 16384);
	let bytes = build::font(&[(*b"avar", avar), (*b"fvar", variable::fvar()), (*b"head", head), (*b"hhea", hhea), (*b"hmtx", std::vec![0u8; 16]), (*b"maxp", maxp)]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let variations = Variations::of(&face).expect("readable").expect("a variable font");
	// The stated points map exactly.
	assert_eq!(variations.adjust(&face, 0, 0).expect("readable"), 0);
	assert_eq!(variations.adjust(&face, 0, 8192).expect("readable"), 12288, "the font moved the middle of its upper half");
	assert_eq!(variations.adjust(&face, 0, 16384).expect("readable"), 16384);
	// And between two stated points the map is linear: half way from 0 to 8192 lands half way from
	// 0 to 12288.
	assert_eq!(variations.adjust(&face, 0, 4096).expect("readable"), 6144);
	// The default `avar` this fixture also builds changes nothing, which is the identity case.
	let identity = build::font(&[
		(*b"avar", variable::avar()),
		(*b"fvar", variable::fvar()),
		(*b"head", {
			let sample = build::sample();
			let at = find_table(&sample, b"head");
			sample[at..at + 54].to_vec()
		}),
		(*b"hhea", {
			let sample = build::sample();
			let at = find_table(&sample, b"hhea");
			sample[at..at + 36].to_vec()
		}),
		(*b"hmtx", std::vec![0u8; 16]),
		(*b"maxp", {
			let sample = build::sample();
			let at = find_table(&sample, b"maxp");
			sample[at..at + 32].to_vec()
		}),
	]);
	let identity = Face::open(&identity, 0).expect("a font this tree built");
	let variations = Variations::of(&identity).expect("readable").expect("a variable font");
	assert_eq!(variations.adjust(&identity, 0, 8192).expect("readable"), 8192);
}

#[test]
// THE HALF A TABLE LIST FORGETS: `HVAR` is what makes a metric correct at a non-default coordinate.
// Without it a variable font's glyphs are the right shape at the wrong widths - text that is subtly
// mis-spaced everywhere except at the default instance, and nothing about it looks like a missing
// table.
fn an_advance_varies_with_the_coordinate_through_hvar() {
	let bytes = build::font(&[
		(*b"HVAR", variable::hvar(100)),
		(*b"fvar", variable::fvar()),
		(*b"head", {
			let sample = build::sample();
			let at = find_table(&sample, b"head");
			sample[at..at + 54].to_vec()
		}),
		(*b"hhea", {
			let sample = build::sample();
			let at = find_table(&sample, b"hhea");
			sample[at..at + 36].to_vec()
		}),
		(*b"hmtx", std::vec![0u8; 16]),
		(*b"maxp", {
			let sample = build::sample();
			let at = find_table(&sample, b"maxp");
			sample[at..at + 32].to_vec()
		}),
	]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	// At the default coordinate the region does not apply at all, so the delta is zero.
	assert_eq!(crate::variations::advance_delta(&face, 1, &[0]).expect("readable"), 0);
	// At the top of the axis the whole delta applies.
	assert_eq!(crate::variations::advance_delta(&face, 1, &[16384]).expect("readable"), 100);
	// Half way up, half of it.
	assert_eq!(crate::variations::advance_delta(&face, 1, &[8192]).expect("readable"), 50);
	// BELOW THE REGION THE DELTA IS ZERO, not negative: a region that does not apply contributes
	// nothing, and treating it as partially applied is how an instance ends up between two masters
	// that were never meant to be mixed.
	assert_eq!(crate::variations::advance_delta(&face, 1, &[-8192]).expect("readable"), 0);
	// A glyph the store has no delta for varies not at all.
	assert_eq!(crate::variations::advance_delta(&face, 0, &[16384]).expect("readable"), 0);
	// And a font with no `HVAR` has no variation to apply, which is not an error.
	let plain = build::sample();
	let plain = Face::open(&plain, 0).expect("a font this tree built");
	assert_eq!(crate::variations::advance_delta(&plain, 1, &[16384]).expect("readable"), 0);
	assert!(Variations::of(&plain).expect("readable").is_none(), "a font with no fvar is not a variable font, which is not an error");
}

/// An outline walk that KEEPS what it was given, because a varied outline is checked point by point.
#[derive(Default)]
struct Recorder {
	points: std::vec::Vec<Point>,
}

impl Outline for Recorder {
	fn point(&mut self, point: Point) -> bool {
		self.points.push(point);
		true
	}
}

/// The sample font with its axis and its outline deltas: a real variable face, built here.
fn varying_sample() -> std::vec::Vec<u8> {
	let sample = build::sample();
	build::font(&[
		(*b"cmap", table_bytes(&sample, b"cmap")),
		(*b"fvar", variable::fvar()),
		(*b"glyf", table_bytes(&sample, b"glyf")),
		(*b"gvar", variable::gvar()),
		(*b"head", table_bytes(&sample, b"head")),
		(*b"hhea", table_bytes(&sample, b"hhea")),
		(*b"hmtx", table_bytes(&sample, b"hmtx")),
		(*b"loca", table_bytes(&sample, b"loca")),
		(*b"maxp", table_bytes(&sample, b"maxp")),
	])
}

/// Walk one glyph at a coordinate, with buffers this fixture owns - which is how the parser is used.
fn varied_points(face: &Face<'_>, glyph: u16, coordinate: i16) -> std::vec::Vec<Point> {
	let mut points = [Point { x: 0, y: 0, on_curve: false, ends_contour: false }; 64];
	let mut deltas = [(0i16, 0i16); 64];
	let mut touched = [false; 64];
	let mut recorder = Recorder::default();
	let scratch = crate::gvar::Scratch { points: &mut points, deltas: &mut deltas, touched: &mut touched };
	crate::gvar::walk_varied(face, glyph, &[coordinate], scratch, &mut recorder).expect("a font this tree built");
	recorder.points
}

#[test]
// THE OTHER HALF OF A VARIABLE FONT. `HVAR` makes the widths right at a coordinate and `gvar` makes
// the SHAPES right; an instance with correct metrics and default outlines is a font drawn at the
// wrong weight with the right spacing, which is no better than the reverse.
fn an_outline_varies_with_the_coordinate_through_gvar() {
	let bytes = varying_sample();
	let face = Face::open(&bytes, 0).expect("a font this tree built");

	// At the default coordinate the one region does not apply at all, so the glyph is its own shape.
	let default = varied_points(&face, 1, 0);
	let plain = build::sample();
	let plain = Face::open(&plain, 0).expect("a font this tree built");
	let mut unvaried = Recorder::default();
	crate::glyf::walk(&plain, 1, &mut unvaried).expect("a font this tree built");
	assert_eq!(default.len(), 4);
	for (varied, plain) in default.iter().zip(unvaried.points.iter()) {
		assert_eq!((varied.x, varied.y), (plain.x, plain.y), "a coordinate no tuple applies at must produce the DEFAULT outline");
	}

	// At the top of the axis the whole delta applies: the square's right side moves out by a hundred
	// and its top moves up by two hundred.
	let full = varied_points(&face, 1, 16384);
	let full: std::vec::Vec<(i16, i16)> = full.iter().map(|point| (point.x, point.y)).collect();
	assert_eq!(full, std::vec![(100, 100), (1000, 100), (1000, 1100), (100, 1100)]);

	// Half way up, half of it - and the inference runs on the SCALED deltas rather than being scaled
	// after the fact, which is the same answer here and is not once three masters overlap.
	let half = varied_points(&face, 1, 8192);
	let half: std::vec::Vec<(i16, i16)> = half.iter().map(|point| (point.x, point.y)).collect();
	assert_eq!(half, std::vec![(100, 100), (950, 100), (950, 1000), (100, 1000)]);

	// BELOW THE REGION NOTHING APPLIES, and a partially applied master would be an instance mixed out
	// of two designs that were never meant to meet.
	let below = varied_points(&face, 1, -8192);
	let below: std::vec::Vec<(i16, i16)> = below.iter().map(|point| (point.x, point.y)).collect();
	assert_eq!(below, std::vec![(100, 100), (900, 100), (900, 900), (100, 900)]);
}

#[test]
// THE DELTAS ARE SPARSE AND THAT IS WHAT THE INFERENCE IS FOR. A tuple names three of a glyph's
// points and leaves the rest to be interpolated; a parser that moved only what was listed would move
// a stem and leave the serif it carries behind, which tears the outline apart rather than failing.
fn the_points_no_tuple_mentions_are_inferred_from_the_ones_that_are() {
	let bytes = varying_sample();
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let full = varied_points(&face, 2, 16384);
	let full: std::vec::Vec<(i16, i16)> = full.iter().map(|point| (point.x, point.y)).collect();
	// The triangle's two base points are referenced - the left one by nothing, the right one by three
	// hundred - and its apex at x 500 is HALF WAY BETWEEN THEM, so it takes half the difference.
	assert_eq!(full, std::vec![(100, 100), (1200, 100), (650, 900)]);
	// Its y is interpolated between two references that moved by the same amount - nothing - which is
	// unambiguous and is zero.
	let half = varied_points(&face, 2, 8192);
	let half: std::vec::Vec<(i16, i16)> = half.iter().map(|point| (point.x, point.y)).collect();
	assert_eq!(half, std::vec![(100, 100), (1050, 100), (575, 900)]);
}

#[test]
// A COMPOSITE IS ADDRESSED BY COMPONENT, NOT BY POINT. Its entries move whole components, and a
// parser that applied them to the expanded points of the components would move an accent by the
// delta meant for the letter under it.
fn a_composite_varies_by_component_rather_than_by_point() {
	let bytes = varying_sample();
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let full = varied_points(&face, 3, 16384);
	let full: std::vec::Vec<(i16, i16)> = full.iter().map(|point| (point.x, point.y)).collect();
	// The component is the square, which varies on its OWN account, placed at the composite's offset
	// of (50, 60) moved by this glyph's component delta of (-40, 70).
	assert_eq!(full, std::vec![(110, 230), (1010, 230), (1010, 1230), (110, 1230)]);
	// At the default coordinate neither the placement nor the component varies.
	let default = varied_points(&face, 3, 0);
	let default: std::vec::Vec<(i16, i16)> = default.iter().map(|point| (point.x, point.y)).collect();
	assert_eq!(default, std::vec![(150, 160), (950, 160), (950, 960), (150, 960)]);
}

#[test]
// `HVAR` IS NOT ALWAYS THERE, and its absence does not mean the advances do not vary: the first two
// phantom points ARE the left side bearing and the advance. Treating a missing `HVAR` as "nothing
// varies" mis-spaces a whole face that states its widths this way instead.
fn an_advance_varies_through_the_phantom_points_when_there_is_no_hvar() {
	let bytes = varying_sample();
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	assert!(face.table(b"HVAR").expect("readable").is_none(), "this fixture states its widths through gvar alone");
	let mut points = [Point { x: 0, y: 0, on_curve: false, ends_contour: false }; 64];
	let mut deltas = [(0i16, 0i16); 64];
	let mut touched = [false; 64];
	let advance = |coordinate: i16, points: &mut [Point], deltas: &mut [(i16, i16)], touched: &mut [bool]| {
		let scratch = crate::gvar::Scratch { points, deltas, touched };
		crate::gvar::phantom_advance_delta(&face, 2, &[coordinate], scratch).expect("a font this tree built")
	};
	// The triangle's advance phantom point moves by two hundred and fifty at the top of the axis, and
	// its left side bearing point does not move - so the advance grows by the difference.
	assert_eq!(advance(16384, &mut points, &mut deltas, &mut touched), 250);
	assert_eq!(advance(8192, &mut points, &mut deltas, &mut touched), 125);
	// A glyph whose tuple never names a phantom point has an advance that does not vary, which is not
	// the same as a glyph that does not vary.
	let scratch = crate::gvar::Scratch { points: &mut points, deltas: &mut deltas, touched: &mut touched };
	assert_eq!(crate::gvar::phantom_advance_delta(&face, 1, &[16384], scratch).expect("readable"), 0);
}

#[test]
// A BUFFER TOO SMALL IS A REFUSAL, not a truncated outline: half a glyph drawn is a shape the font
// does not contain, and a caller that gets one back has no way to tell.
fn a_glyph_larger_than_the_buffer_it_was_given_is_refused() {
	let bytes = varying_sample();
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut points = [Point { x: 0, y: 0, on_curve: false, ends_contour: false }; 2];
	let mut deltas = [(0i16, 0i16); 64];
	let mut touched = [false; 64];
	let mut recorder = Recorder::default();
	let scratch = crate::gvar::Scratch { points: &mut points, deltas: &mut deltas, touched: &mut touched };
	assert!(crate::gvar::walk_varied(&face, 1, &[16384], scratch, &mut recorder).is_err(), "four points do not fit in two");

	// And so is a delta buffer that cannot hold the phantom points, which are part of the count.
	let mut points = [Point { x: 0, y: 0, on_curve: false, ends_contour: false }; 64];
	let mut deltas = [(0i16, 0i16); 4];
	let mut touched = [false; 4];
	let mut recorder = Recorder::default();
	let scratch = crate::gvar::Scratch { points: &mut points, deltas: &mut deltas, touched: &mut touched };
	assert!(crate::gvar::walk_varied(&face, 1, &[16384], scratch, &mut recorder).is_err(), "four points plus four phantom points do not fit in four");
}

#[test]
// THE SAME BOUND EVERY OTHER TABLE IS HELD TO. `gvar` is a web of offsets into offsets - a glyph's
// range, a tuple's size, a delta-encoded point list - and every one of them is a place a crafted
// font can point somewhere else. What is asserted is that it ANSWERS: no panic, no read past the
// end, no walk that does not come back.
fn a_gvar_font_corrupted_anywhere_is_refused_rather_than_read_past() {
	let bytes = varying_sample();
	let at = find_table(&bytes, b"gvar");
	let length = {
		let entry = find_entry(&bytes, b"gvar");
		u32::from_be_bytes([bytes[entry + 12], bytes[entry + 13], bytes[entry + 14], bytes[entry + 15]]) as usize
	};
	for index in 0..length {
		for pattern in [0x01u8, 0x7F, 0x80, 0xFF] {
			let mut mutated = bytes.clone();
			mutated[at + index] ^= pattern;
			let Ok(face) = Face::open(&mutated, 0) else { continue };
			for glyph in 0..4u16 {
				for coordinate in [-16384i16, 0, 8192, 16384] {
					let mut points = [Point { x: 0, y: 0, on_curve: false, ends_contour: false }; 64];
					let mut deltas = [(0i16, 0i16); 64];
					let mut touched = [false; 64];
					let mut recorder = Recorder::default();
					let scratch = crate::gvar::Scratch { points: &mut points, deltas: &mut deltas, touched: &mut touched };
					let _ = crate::gvar::walk_varied(&face, glyph, &[coordinate], scratch, &mut recorder);
					let scratch = crate::gvar::Scratch { points: &mut points, deltas: &mut deltas, touched: &mut touched };
					let _ = crate::gvar::phantom_advance_delta(&face, glyph, &[coordinate], scratch);
				}
			}
		}
	}
}

#[test]
// THE LINE IS AS MUCH A METRIC AS THE ADVANCE IS. `HVAR` makes the glyphs sit at the right distance
// along a line and `MVAR` makes the LINES sit at the right distance from each other. A variable face
// whose weight axis thickens its strokes almost always raises its ascender with them, and a layout
// that reads `hhea` alone sets every instance on the leading of the default one - lines that touch at
// one end of the axis and drift apart at the other, which looks like a layout bug rather than a
// table nobody read.
fn the_font_wide_metrics_vary_with_the_coordinate_through_mvar() {
	let sample = build::sample();
	let bytes = build::font(&[
		(*b"MVAR", variable::mvar(120, -40)),
		(*b"fvar", variable::fvar()),
		(*b"head", table_bytes(&sample, b"head")),
		(*b"hhea", table_bytes(&sample, b"hhea")),
		(*b"hmtx", table_bytes(&sample, b"hmtx")),
		(*b"maxp", table_bytes(&sample, b"maxp")),
	]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	use crate::variations::metric;
	// At the default coordinate the region does not apply, so the static tables stand unaltered.
	assert_eq!(crate::variations::metric_delta(&face, metric::ASCENDER, &[0]).expect("readable"), 0);
	// At the top of the axis the whole delta applies, and half way up, half of it.
	assert_eq!(crate::variations::metric_delta(&face, metric::ASCENDER, &[16384]).expect("readable"), 120);
	assert_eq!(crate::variations::metric_delta(&face, metric::ASCENDER, &[8192]).expect("readable"), 60);
	// A descender that grows DOWNWARD is a negative delta, and a reader that took the magnitude would
	// close the line up exactly as far as the font meant to open it.
	assert_eq!(crate::variations::metric_delta(&face, metric::DESCENDER, &[16384]).expect("readable"), -40);
	// A TAG THIS FONT DOES NOT VARY ANSWERS ZERO, which is the honest answer: the static table already
	// holds its value and nothing moves it.
	assert_eq!(crate::variations::metric_delta(&face, metric::LINE_GAP, &[16384]).expect("readable"), 0);
	assert_eq!(crate::variations::metric_delta(&face, metric::X_HEIGHT, &[16384]).expect("readable"), 0);
	// And a font with no `MVAR` varies none of them, which is not an error.
	let plain = build::sample();
	let plain = Face::open(&plain, 0).expect("a font this tree built");
	assert_eq!(crate::variations::metric_delta(&plain, metric::ASCENDER, &[16384]).expect("readable"), 0);
}

#[test]
// THE SAME BOUND AS EVERY OTHER TABLE. `MVAR` is a stride the font states, a record count and an
// offset into a shared store, and each of them is a place a crafted font can point somewhere else.
fn an_mvar_corrupted_anywhere_is_refused_rather_than_read_past() {
	let sample = build::sample();
	let bytes = build::font(&[
		(*b"MVAR", variable::mvar(120, -40)),
		(*b"fvar", variable::fvar()),
		(*b"head", table_bytes(&sample, b"head")),
		(*b"hhea", table_bytes(&sample, b"hhea")),
		(*b"hmtx", table_bytes(&sample, b"hmtx")),
		(*b"maxp", table_bytes(&sample, b"maxp")),
	]);
	let at = find_table(&bytes, b"MVAR");
	let entry = find_entry(&bytes, b"MVAR");
	let length = u32::from_be_bytes([bytes[entry + 12], bytes[entry + 13], bytes[entry + 14], bytes[entry + 15]]) as usize;
	for index in 0..length {
		for pattern in [0x01u8, 0x7F, 0x80, 0xFF] {
			let mut mutated = bytes.clone();
			mutated[at + index] ^= pattern;
			let Ok(face) = Face::open(&mutated, 0) else { continue };
			for tag in [crate::variations::metric::ASCENDER, crate::variations::metric::DESCENDER, crate::variations::metric::CAP_HEIGHT] {
				for coordinate in [-16384i16, 0, 8192, 16384] {
					let _ = crate::variations::metric_delta(&face, tag, &[coordinate]);
				}
			}
		}
	}
}

#[test]
// ONE TABLE CLAIMING MOST OF A FILE IS A LENGTH NOBODY DREW, and it is refused by NAME before
// anything inside it is read: a reader that accepted it would do the work the ceiling exists to
// refuse in order to discover the table was nonsense.
fn a_table_larger_than_the_frozen_ceiling_is_refused_by_name() {
	let mut bytes = build::sample();
	let entry = find_entry(&bytes, b"glyf");
	let asked = opentype_profile::limits::TABLE_BYTES as u32 + 1;
	bytes[entry + 12..entry + 16].copy_from_slice(&asked.to_be_bytes());
	let face = Face::open(&bytes, 0).expect("a directory entry is not read until the table is asked for");
	assert_eq!(face.table(b"glyf").err(), Some(Error::Unsupported(crate::Unsupported::Exceeded { limit: "table bytes", ceiling: opentype_profile::limits::TABLE_BYTES, asked: asked as u64 })));
}

#[test]
// DEPTH ALONE BOUNDS THE STACK AND NOT THE WORK. Five levels of nesting MULTIPLY, so a composite of
// composites is a glyph whose expanded point count has nothing to do with the size of any glyph
// description in it - and every offset in it is in range.
fn the_points_a_composite_expands_to_are_counted_across_the_whole_walk() {
	// A leaf with six thousand points, which is under the ceiling on its own.
	let leaf: std::vec::Vec<(i16, i16)> = (0..6000).map(|index| ((index % 900) as i16 + 50, (index / 900) as i16 + 50)).collect();
	let leaf = build::simple_glyph(&leaf);
	// A composite that draws it twice, which is twelve thousand and is over.
	let pair = build::composite_glyphs(&[(1, 0, 0), (1, 10, 10)]);
	let mut glyf = std::vec::Vec::new();
	let mut loca = std::vec::Vec::new();
	u16_at(&mut loca, 0);
	for glyph in [&pair, &leaf] {
		glyf.extend_from_slice(glyph);
		while glyf.len() % 2 != 0 {
			glyf.push(0);
		}
		u16_at(&mut loca, (glyf.len() / 2) as u16);
	}
	let sample = build::sample();
	let mut hhea = table_bytes(&sample, b"hhea");
	hhea[34..36].copy_from_slice(&2u16.to_be_bytes());
	let mut maxp = table_bytes(&sample, b"maxp");
	maxp[4..6].copy_from_slice(&2u16.to_be_bytes());
	let bytes = build::font(&[(*b"glyf", glyf), (*b"head", table_bytes(&sample, b"head")), (*b"hhea", hhea), (*b"hmtx", std::vec![0u8; 8]), (*b"loca", loca), (*b"maxp", maxp)]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");

	// The leaf on its own is drawn: six thousand points is under the ceiling.
	let mut counter = Counter::default();
	crate::glyf::walk(&face, 1, &mut counter).expect("six thousand points is within the ceiling");
	assert_eq!(counter.points, 6000);

	// The composite that draws it twice is REFUSED, and the refusal names the ceiling and the ask.
	let mut counter = Counter::default();
	assert_eq!(crate::glyf::walk(&face, 0, &mut counter), Err(Error::Unsupported(crate::Unsupported::Exceeded { limit: "composite points", ceiling: opentype_profile::limits::COMPOSITE_POINTS, asked: 12000 })));
}

#[test]
// EVERY AXIS MULTIPLIES THE REGIONS A DELTA IS SCALED OVER, so an axis count is work per glyph rather
// than a declaration - and a face past the profile's ceiling is refused by name rather than read
// until the reader's own array runs out.
fn a_face_declaring_more_axes_than_the_profile_freezes_is_refused_by_name() {
	let mut fvar = variable::fvar();
	let asked = opentype_profile::limits::VARIATION_AXES as u16 + 1;
	// The axis count is the fifth `u16` of the header.
	fvar[8..10].copy_from_slice(&asked.to_be_bytes());
	let sample = build::sample();
	let bytes = build::font(&[
		(*b"fvar", fvar),
		(*b"head", table_bytes(&sample, b"head")),
		(*b"hhea", table_bytes(&sample, b"hhea")),
		(*b"hmtx", table_bytes(&sample, b"hmtx")),
		(*b"maxp", table_bytes(&sample, b"maxp")),
	]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	assert_eq!(Variations::of(&face).err(), Some(Error::Unsupported(crate::Unsupported::Exceeded { limit: "variation axes", ceiling: opentype_profile::limits::VARIATION_AXES, asked: asked as u64 })));
}

/// A big-endian `u16`, for the fixtures above that assemble a `loca` by hand.
fn u16_at(out: &mut std::vec::Vec<u8>, value: u16) {
	out.extend_from_slice(&value.to_be_bytes());
}

/// A face carrying only the metadata tables, which is all these fixtures ask about.
fn metadata_font(extra: &[([u8; 4], std::vec::Vec<u8>)]) -> std::vec::Vec<u8> {
	let sample = build::sample();
	let mut tables: std::vec::Vec<([u8; 4], std::vec::Vec<u8>)> = std::vec![
		(*b"head", table_bytes(&sample, b"head")),
		(*b"hhea", table_bytes(&sample, b"hhea")),
		(*b"hmtx", table_bytes(&sample, b"hmtx")),
		(*b"maxp", table_bytes(&sample, b"maxp")),
	];
	tables.extend_from_slice(extra);
	// The directory is read by tag, so the order only has to be stable - and sorting it is what a
	// real font does.
	tables.sort_by_key(|(tag, _)| *tag);
	build::font(&tables)
}

#[test]
// PICKING "THE FIRST RECORD" MAKES THE ANSWER DEPEND ON THE ORDER A FONT TOOL HAPPENED TO WRITE THEM
// IN, so two files with identical names could disagree about their own family. The preference is
// stated and total: Windows English, then Windows in any language, then Macintosh English, then
// anything at all.
fn the_name_a_face_answers_with_is_chosen_by_a_stated_preference() {
	use crate::metadata::{Names, name_id};
	// The Macintosh record is written FIRST and the Windows English one LAST, so a reader that took
	// the first would answer with the Macintosh one.
	let names = build::name_table(&[
		(1, 0, 0, name_id::FAMILY, "Mac Family"),
		(3, 1, 0x0407, name_id::FAMILY, "Deutsche Familie"),
		(3, 1, 0x0409, name_id::FAMILY, "Test Family"),
		(3, 1, 0x0409, name_id::SUBFAMILY, "Bold Italic"),
		(1, 0, 0, name_id::POSTSCRIPT, "TestFamily-BoldItalic"),
	]);
	let bytes = metadata_font(&[(*b"name", names)]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let names = Names::of(&face).expect("readable").expect("a face with names");
	assert!(names.get(name_id::FAMILY).expect("readable").expect("a family").equals("Test Family"));
	assert!(names.get(name_id::SUBFAMILY).expect("readable").expect("a subfamily").equals("Bold Italic"));
	// A name only the Macintosh platform carries is still answered, at its own place in the order.
	assert!(names.get(name_id::POSTSCRIPT).expect("readable").expect("a postscript name").equals("TestFamily-BoldItalic"));
	// A name the face does not carry is `None` rather than an empty string, which are different
	// answers: a face with no typographic family is not a face whose typographic family is blank.
	assert!(names.get(name_id::TYPOGRAPHIC_FAMILY).expect("readable").is_none());
}

#[test]
// THE UPPER HALF OF MAC ROMAN IS NOT LATIN-1, and reading it as Latin-1 gives a family name that is
// wrong in a way that looks like a corrupt font rather than a wrong decoder. And a UTF-16 name
// outside the basic plane is a PAIR of units, which a decoder taking each unit as a character turns
// into two replacement characters.
fn a_name_is_decoded_in_the_encoding_its_record_declares() {
	use crate::metadata::{Names, name_id};
	let names = build::name_table(&[(3, 1, 0x0409, name_id::FAMILY, "Ma\u{0148}ka \u{1F600}")]);
	let bytes = metadata_font(&[(*b"name", names)]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let names = Names::of(&face).expect("readable").expect("a face with names");
	let family = names.get(name_id::FAMILY).expect("readable").expect("a family");
	// Seven characters: the emoji is ONE of them, not two.
	assert_eq!(family.len(), 7);
	assert!(family.equals("Ma\u{0148}ka \u{1F600}"));
	let mut buffer = [' '; 8];
	assert_eq!(family.copy_into(&mut buffer).expect("room"), 7);
	assert_eq!(buffer[6], '\u{1F600}');
	// A BUFFER TOO SMALL IS A REFUSAL. Truncating a family name is how two different families become
	// one, and the caller cannot tell that it happened.
	let mut small = [' '; 3];
	assert!(family.copy_into(&mut small).is_err());

	// And the Macintosh half: byte 0xA5 is a BULLET in Mac Roman and a yen sign in Latin-1.
	let mut table = build::name_table(&[(1, 0, 0, name_id::FAMILY, "ab")]);
	let storage = table.len() - 2;
	table[storage] = 0xA5;
	table[storage + 1] = 0xD5;
	let bytes = metadata_font(&[(*b"name", table)]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let names = Names::of(&face).expect("readable").expect("a face with names");
	let family = names.get(name_id::FAMILY).expect("readable").expect("a family");
	assert!(family.equals("\u{2022}\u{2019}"), "0xA5 is a bullet and 0xD5 a right single quote, not a yen sign and an O with a tilde");
}

#[test]
// A STYLE IS A NUMBER AND A SET OF BITS, NOT A WORD. "Bold Italic" is a string a designer chose in a
// language they chose it in, and matching on it is how a face called `Fett` stops being bold.
fn the_style_is_read_from_os2_as_numbers_rather_than_from_a_name() {
	use crate::metadata::{Os2, selection};
	let bytes = metadata_font(&[(*b"OS/2", build::os2(4, 700, 3, selection::BOLD | selection::ITALIC | selection::USE_TYPO_METRICS, 520))]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let os2 = Os2::of(&face).expect("readable").expect("a face with OS/2");
	assert_eq!(os2.weight_class, 700);
	assert_eq!(os2.width_class, 3);
	assert!(os2.is_slanted());
	// THE FACE ASKING FOR ITS TYPOGRAPHIC METRICS CHANGES THE LEADING OF EVERY LINE, and a layout
	// that ignored the bit sets the face too tight or too loose everywhere.
	assert!(os2.uses_typographic_metrics());
	assert_eq!((os2.typo_ascender, os2.typo_descender, os2.typo_line_gap), (800, -200, 100));
	assert_eq!((os2.win_ascent, os2.win_descent), (900, 250));
	assert_eq!(os2.x_height, Some(520));
	assert_eq!(os2.cap_height, Some(700));

	// A FACE THAT SETS ONLY THE OBLIQUE BIT IS STILL NOT UPRIGHT.
	let bytes = metadata_font(&[(*b"OS/2", build::os2(4, 400, 5, selection::OBLIQUE, 500))]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	assert!(Os2::of(&face).expect("readable").expect("OS/2").is_slanted());

	// VERSION 1 HAS NO X-HEIGHT, and the VERSION says what is there rather than the length: a table
	// padded out to a later version's size still does not carry them, and reading past the version
	// would give two metrics out of somebody's padding.
	let bytes = metadata_font(&[(*b"OS/2", build::os2(1, 400, 5, selection::REGULAR, 0))]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let os2 = Os2::of(&face).expect("readable").expect("OS/2");
	assert_eq!(os2.x_height, None);
	assert_eq!(os2.cap_height, None);

	// AND VERSION 0 IS OUTSIDE THE PROFILE, refused by name: it predates the selection bits and the
	// typographic metrics a style and a line height are decided from, so it is a face this system
	// cannot answer the questions about.
	let bytes = metadata_font(&[(*b"OS/2", build::os2(0, 400, 5, 0, 0))]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	assert_eq!(Os2::of(&face).err(), Some(Error::Unsupported(crate::Unsupported::TableVersion { tag: *b"OS/2", major: 0, minor: 0 })));
}

#[test]
// A GLYPH NAME IS WHAT A PDF EXPORTER WRITES, WHAT AN ACCESSIBILITY LAYER READS BACK AND WHAT A
// SUBSETTER MATCHES ON. A face that carries them and a reader that ignores them produce a document
// nothing else can get the text out of.
fn post_carries_the_italic_angle_the_underline_and_the_glyph_names() {
	use crate::metadata::{GlyphNames, MAX_GLYPH_NAME, Post};
	let bytes = metadata_font(&[(*b"post", build::post(&[".notdef", "A", "uniE001", "b"]))]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let post = Post::of(&face).expect("readable").expect("a face with post");
	// THE ORDINARY FORWARD SLANT IS A NEGATIVE ANGLE, and the sign is the thing that is got wrong.
	assert_eq!(post.italic_angle >> 16, -12);
	assert_eq!(post.underline_position, -100);
	assert_eq!(post.underline_thickness, 50);
	assert!(post.is_fixed_pitch);

	let names = GlyphNames::of(&face).expect("readable").expect("a version 2.0 table");
	assert_eq!(names.len(), 4);
	let mut buffer = [0u8; MAX_GLYPH_NAME];
	let name_of = |glyph: u16, buffer: &mut [u8; MAX_GLYPH_NAME]| {
		let length = names.get(glyph, buffer).expect("readable").expect("a named glyph");
		core::str::from_utf8(&buffer[..length]).expect("the format says a glyph name is ASCII").to_string()
	};
	// The two standard names cost NO storage in the file, which is the point of the standard list.
	assert_eq!(name_of(0, &mut buffer), ".notdef");
	assert_eq!(name_of(1, &mut buffer), "A");
	// And the custom one is a Pascal string after them.
	assert_eq!(name_of(2, &mut buffer), "uniE001");
	assert_eq!(name_of(3, &mut buffer), "b");
	// A glyph past the table's own count is `None` rather than a name out of somebody else's bytes.
	assert!(names.get(4, &mut buffer).expect("readable").is_none());

	// VERSION 3.0 CARRIES NO NAMES AND THAT IS NOT AN ERROR - it is what almost every modern face
	// states, and treating it as a fault would refuse most of the fonts in the world.
	let bytes = metadata_font(&[(*b"post", build::post(&[]))]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	assert!(Post::of(&face).expect("readable").is_some());
	assert!(GlyphNames::of(&face).expect("readable").is_none());
}

#[test]
// THE SAME BOUND AS EVERY OTHER TABLE. A name table is a record array pointing into a storage area,
// `OS/2` is a struct whose length depends on a version it states itself, and `post` is a walk over
// Pascal strings - each of them a place a crafted font can point somewhere else. What is asserted is
// that they ANSWER.
fn the_metadata_tables_corrupted_anywhere_are_refused_rather_than_read_past() {
	use crate::metadata::{GlyphNames, MAX_GLYPH_NAME, Names, Os2, Post, name_id};
	let bytes = metadata_font(&[
		(*b"OS/2", build::os2(4, 700, 3, 0x21, 520)),
		(*b"name", build::name_table(&[(3, 1, 0x0409, name_id::FAMILY, "Test Family"), (1, 0, 0, name_id::FULL, "Test")])),
		(*b"post", build::post(&[".notdef", "uniE001"])),
	]);
	for tag in [b"OS/2", b"name", b"post"] {
		let at = find_table(&bytes, tag);
		let entry = find_entry(&bytes, tag);
		let length = u32::from_be_bytes([bytes[entry + 12], bytes[entry + 13], bytes[entry + 14], bytes[entry + 15]]) as usize;
		for index in 0..length {
			for pattern in [0x01u8, 0x7F, 0x80, 0xFF] {
				let mut mutated = bytes.clone();
				mutated[at + index] ^= pattern;
				let Ok(face) = Face::open(&mutated, 0) else { continue };
				let _ = Os2::of(&face);
				let _ = Post::of(&face);
				if let Ok(Some(names)) = Names::of(&face) {
					for wanted in [name_id::FAMILY, name_id::SUBFAMILY, name_id::FULL, name_id::POSTSCRIPT] {
						if let Ok(Some(name)) = names.get(wanted) {
							let mut buffer = [' '; 64];
							let _ = name.copy_into(&mut buffer);
							let _ = name.len();
							let _ = name.equals("Test Family");
						}
					}
				}
				if let Ok(Some(names)) = GlyphNames::of(&face) {
					let mut buffer = [0u8; MAX_GLYPH_NAME];
					for glyph in 0..8u16 {
						let _ = names.get(glyph, &mut buffer);
					}
				}
			}
		}
	}
}
