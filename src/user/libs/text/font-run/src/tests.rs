use super::*;

use font_contract::cluster::CaretAffinity;
use font_contract::{Direction, FaceIdentity, FaceRef, FileIdentity, Fixed266, Generation, GlyphKind, RasterisationMode, ScriptTag, VariationCoordinates};
use font_parse::Face;
use font_shape::{Buffer, Position};

/// A font this tree BUILT, because what is exercised here is the conversion and the mapping, and a
/// font written here can be made to say exactly what a case needs - which a licensed real face,
/// which is a reviewed third-party import, cannot.
mod build {
	pub fn u16(out: &mut std::vec::Vec<u8>, value: u16) {
		out.extend_from_slice(&value.to_be_bytes());
	}

	pub fn u32(out: &mut std::vec::Vec<u8>, value: u32) {
		out.extend_from_slice(&value.to_be_bytes());
	}

	pub fn head(units_per_em: u16) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, 0);
		u32(&mut out, 0);
		u32(&mut out, 0);
		u32(&mut out, 0x5F0F_3CF5);
		u16(&mut out, 0);
		u16(&mut out, units_per_em);
		out.extend_from_slice(&[0u8; 16]);
		u16(&mut out, 0);
		u16(&mut out, 0);
		u16(&mut out, 1000);
		u16(&mut out, 1000);
		u16(&mut out, 0);
		u16(&mut out, 8);
		u16(&mut out, 2);
		u16(&mut out, 0); // short `loca`
		u16(&mut out, 0);
		out
	}

	pub fn hhea(metrics: u16) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, 0);
		u16(&mut out, 800);
		u16(&mut out, (-200i16) as u16);
		u16(&mut out, 0);
		out.extend_from_slice(&[0u8; 24]);
		u16(&mut out, metrics);
		out
	}

	pub fn maxp(glyphs: u16) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u32(&mut out, 0x0001_0000);
		u16(&mut out, glyphs);
		out.extend_from_slice(&[0u8; 26]);
		out
	}

	pub fn hmtx(advances: &[(u16, i16)]) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		for (advance, bearing) in advances {
			u16(&mut out, *advance);
			u16(&mut out, *bearing as u16);
		}
		out
	}

	/// A coverage table, format 1: the glyphs listed.
	pub fn coverage(glyphs: &[u16]) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, glyphs.len() as u16);
		for glyph in glyphs {
			u16(&mut out, *glyph);
		}
		out
	}

	/// `GDEF` with a ligature caret list: one glyph, with the caret coordinates given.
	pub fn gdef_carets(glyph: u16, coordinates: &[i16]) -> std::vec::Vec<u8> {
		// Each caret value is format 1: a format word and a design-unit coordinate.
		let mut values = std::vec::Vec::new();
		for coordinate in coordinates {
			u16(&mut values, 1);
			u16(&mut values, *coordinate as u16);
		}
		// The `LigGlyph`: a count, one offset per caret, then the values.
		let lig_header = 2 + coordinates.len() * 2;
		let mut lig = std::vec::Vec::new();
		u16(&mut lig, coordinates.len() as u16);
		for index in 0..coordinates.len() {
			u16(&mut lig, (lig_header + index * 4) as u16);
		}
		lig.extend_from_slice(&values);

		// The `LigCaretList`: a coverage offset, a count, one offset per covered glyph.
		let cover = coverage(&[glyph]);
		let list_header = 4 + 2;
		let mut list = std::vec::Vec::new();
		u16(&mut list, (list_header + lig.len()) as u16); // the coverage follows the ligature
		u16(&mut list, 1);
		u16(&mut list, list_header as u16);
		list.extend_from_slice(&lig);
		list.extend_from_slice(&cover);

		// The header: version and four offsets, of which only the caret list is present.
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, 0);
		u16(&mut out, 0); // no glyph class definition
		u16(&mut out, 0); // no attachment list
		u16(&mut out, 12); // the caret list follows this header
		u16(&mut out, 0); // no mark attachment classes
		out.extend_from_slice(&list);
		out
	}

	/// `COLR` version 0: the glyphs listed have layer lists.
	pub fn colr_v0(glyphs: &[u16]) -> std::vec::Vec<u8> {
		let mut records = std::vec::Vec::new();
		for glyph in glyphs {
			u16(&mut records, *glyph);
			u16(&mut records, 0); // first layer
			u16(&mut records, 1); // one layer
		}
		let mut out = std::vec::Vec::new();
		u16(&mut out, 0); // version
		u16(&mut out, glyphs.len() as u16);
		u32(&mut out, 14); // the records follow this header
		u32(&mut out, (14 + records.len()) as u32); // the layer records, which this does not read
		u16(&mut out, 1);
		out.extend_from_slice(&records);
		// One layer record: a glyph and a palette entry.
		u16(&mut out, 1);
		u16(&mut out, 0);
		out
	}

	/// `COLR` version 1: the glyphs listed have paint graphs, and the ones given to version 0 keep
	/// their layer lists - which is what a real face carries, so a consumer that knows only version 0
	/// still draws something.
	pub fn colr_v1(v0: &[u16], v1: &[u16]) -> std::vec::Vec<u8> {
		let mut records = std::vec::Vec::new();
		for glyph in v0 {
			u16(&mut records, *glyph);
			u16(&mut records, 0);
			u16(&mut records, 1);
		}
		let mut list = std::vec::Vec::new();
		u32(&mut list, v1.len() as u32);
		for glyph in v1 {
			u16(&mut list, *glyph);
			u32(&mut list, 0); // the paint, which this does not read
		}
		// version(2) + count(2) + records offset(4) + layers offset(4) + layer count(2)
		// + base glyph list(4) + layer list(4) + clip list(4) + var index map(4) + store(4) = 34.
		let header = 34usize;
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, v0.len() as u16);
		u32(&mut out, header as u32);
		u32(&mut out, (header + records.len()) as u32);
		u16(&mut out, 1);
		u32(&mut out, (header + records.len() + 4) as u32); // the base glyph list
		u32(&mut out, 0);
		u32(&mut out, 0);
		u32(&mut out, 0);
		u32(&mut out, 0);
		out.extend_from_slice(&records);
		u16(&mut out, 1); // the one layer record
		u16(&mut out, 0);
		out.extend_from_slice(&list);
		out
	}

	/// `CPAL` with one palette.
	pub fn cpal() -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u16(&mut out, 0);
		u16(&mut out, 1); // one entry
		u16(&mut out, 1); // one palette
		u16(&mut out, 1); // one colour record
		u32(&mut out, 14);
		u16(&mut out, 0);
		out.extend_from_slice(&[0, 0, 0, 255]);
		out
	}

	/// `sbix` with the strikes given, each holding the glyphs listed.
	pub fn sbix(glyph_count: u16, strikes: &[(u16, &[u16])]) -> std::vec::Vec<u8> {
		let mut bodies = std::vec::Vec::new();
		let mut offsets = std::vec::Vec::new();
		let header = 8 + strikes.len() * 4;
		for (ppem, glyphs) in strikes {
			offsets.push(header + bodies.len());
			let mut strike = std::vec::Vec::new();
			u16(&mut strike, *ppem);
			u16(&mut strike, 72); // ppi
			// One offset per glyph plus one: a glyph is present when its range is not empty.
			let mut at = 0u32;
			for glyph in 0..=glyph_count {
				u32(&mut strike, at);
				if glyphs.contains(&glyph) {
					at += 16;
				}
			}
			bodies.extend_from_slice(&strike);
		}
		let mut out = std::vec::Vec::new();
		u16(&mut out, 1);
		u16(&mut out, 0);
		u32(&mut out, strikes.len() as u32);
		for offset in &offsets {
			u32(&mut out, *offset as u32);
		}
		out.extend_from_slice(&bodies);
		out
	}

	/// Assemble a font out of its tables, which must be given in tag order.
	pub fn font(tables: &[([u8; 4], std::vec::Vec<u8>)]) -> std::vec::Vec<u8> {
		let mut out = std::vec::Vec::new();
		u32(&mut out, 0x0001_0000);
		u16(&mut out, tables.len() as u16);
		u16(&mut out, 0);
		u16(&mut out, 0);
		u16(&mut out, 0);
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

	/// The plainest face that can be opened: four glyphs, a thousand to the em.
	pub fn plain() -> std::vec::Vec<u8> {
		font(&[(*b"head", head(1000)), (*b"hhea", hhea(4)), (*b"hmtx", hmtx(&[(500, 0), (1000, 0), (800, 0), (600, 0)])), (*b"maxp", maxp(4))])
	}
}

fn identity() -> FaceRef {
	FaceRef { face: FaceIdentity { file: FileIdentity([7u8; 32]), index: 0 }, generation: Generation(3) }
}

fn request<'a>(face: &'a Face<'a>, text: &'a str, direction: Direction, size: Fixed266) -> Request<'a> {
	Request { face, identity: identity(), size, variation: VariationCoordinates::NONE, script: ScriptTag::from_bytes(*b"latn"), direction, mode: RasterisationMode::Grayscale, origin: (Fixed266::ZERO, Fixed266::ZERO), text, start: 0, end: text.len() }
}

/// A buffer as a shaper leaves it: glyphs in LOGICAL order, positions in font units.
fn buffer(entries: &[(u16, u32, Position)]) -> Buffer {
	let mut buffer = Buffer::new();
	for (glyph, cluster, position) in entries {
		buffer.push(*glyph, *cluster);
		let at = buffer.len() - 1;
		buffer.positions[at] = *position;
	}
	buffer
}

fn advance(x: i32) -> Position {
	Position { x_advance: x, y_advance: 0, x_offset: 0, y_offset: 0 }
}

#[test]
// THE ONE CONVERSION SITE. Font units become 26.6 here and nowhere else, rounding half to EVEN, so
// two implementations of the same shaping produce the same bytes. Rounding a second time downstream
// would make the result depend on the order two libraries were written in - a difference nobody can
// see in a picture and nobody can reproduce from one.
fn font_units_become_the_seam_s_units_at_one_place_and_round_half_to_even() {
	let bytes = build::plain();
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	// Sixteen pixels on a grid of a thousand: one font unit is 16/1000 of a pixel, and 64ths of a
	// pixel are what the seam carries.
	let size = Fixed266::from_pixels(16);
	// 500 units at 16 px is 8 px exactly, which is 512 in 26.6.
	let run = produce(&request(&face, "ab", Direction::LeftToRight, size), &buffer(&[(1, 0, advance(500)), (2, 1, advance(1000))])).expect("a run");
	let glyphs = run.run().glyphs;
	assert_eq!(glyphs[0].x_advance, Fixed266::from_raw(512));
	assert_eq!(glyphs[1].x_advance, Fixed266::from_raw(1024));

	// THE TIE. 25 units at 16 px on a grid of 1000 is 0.4 px, which is 25.6 in 26.6 - not a tie. A
	// value that IS a tie: 125 units is 2.0 px exactly... so the tie has to be built on purpose.
	// units * size / upem with size 64 and upem 1000: units = 125 gives 8000/1000 = 8, exact.
	// units = 25 gives 1600/1000 = 1.6 -> 2. units = 75 gives 4800/1000 = 4.8 -> 5.
	// A TIE is units where units*64 % 1000 == 500: units = 250 gives 16000/1000 = 16 exactly, no.
	// units*64/1000 ties when units*64 = 1000k + 500, so units*8 = 125k + 62.5 - impossible on this
	// grid, which is why the tie is checked on a grid of 128 below instead.
	let bytes = build::font(&[(*b"head", build::head(128)), (*b"hhea", build::hhea(2)), (*b"hmtx", build::hmtx(&[(0, 0), (0, 0)])), (*b"maxp", build::maxp(2))]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let size = Fixed266::from_pixels(1); // 64 in 26.6
	// units * 64 / 128 = units / 2: every odd unit count is an exact tie.
	let run = produce(&request(&face, "ab", Direction::LeftToRight, size), &buffer(&[(0, 0, advance(3)), (1, 1, advance(5))])).expect("a run");
	let glyphs = run.run().glyphs;
	// 1.5 rounds to 2, which is even; 2.5 rounds to 2, which is also even. Round-half-UP would have
	// answered 2 and 3, and the difference is exactly the tie.
	assert_eq!(glyphs[0].x_advance, Fixed266::from_raw(2));
	assert_eq!(glyphs[1].x_advance, Fixed266::from_raw(2));
}

#[test]
// A FONT MEASURES UPWARD AND THE SEAM MEASURES DOWNWARD. A producer that passed font units straight
// through would put every mark on the wrong side of its base, which reads as a broken font rather
// than as a sign nobody wrote down.
fn the_y_axis_turns_over_between_the_font_and_the_device() {
	let bytes = build::plain();
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let size = Fixed266::from_pixels(16);
	// A mark raised 250 units ABOVE the baseline in the font is 4 px UP, which is -4 px in a device
	// space whose y grows downward.
	let mark = Position { x_advance: 0, y_advance: 0, x_offset: 0, y_offset: 250 };
	let run = produce(&request(&face, "a", Direction::LeftToRight, size), &buffer(&[(1, 0, mark)])).expect("a run");
	assert_eq!(run.run().glyphs[0].y_offset, Fixed266::from_raw(-256));
}

#[test]
// THE GLYPHS ARE DRAWN LEFT TO RIGHT AND THE CLUSTERS STAY IN THE ORDER THE TEXT WAS WRITTEN. Storing
// the clusters visually would put a bidi decision inside the data every consumer reads, and a hit
// test would have to undo it to answer a question about the string.
fn a_right_to_left_run_is_drawn_in_visual_order_and_mapped_in_logical_order() {
	let bytes = build::plain();
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let size = Fixed266::from_pixels(16);
	let shaped = buffer(&[(1, 0, advance(500)), (2, 1, advance(1000)), (3, 2, advance(800))]);
	let run = produce(&request(&face, "abc", Direction::RightToLeft, size), &shaped).expect("a run");
	// The glyphs are reversed: the LAST character of the text is drawn first.
	let glyphs: std::vec::Vec<u32> = run.run().glyphs.iter().map(|glyph| glyph.glyph).collect();
	assert_eq!(glyphs, std::vec![3, 2, 1]);
	let map = run.cluster_map().expect("a mapping this crate just built");
	// The clusters are NOT reversed: cluster 0 is still the first character of the string.
	assert_eq!(map.clusters()[0].source, font_contract::SourceRange { start: 0, end: 1 });
	assert_eq!(map.clusters()[2].source, font_contract::SourceRange { start: 2, end: 3 });
	// And the first character is drawn LAST, which is what the visual mapping is for.
	assert_eq!(map.visual_of_logical(0), Some(2));
	assert_eq!(map.logical_of_visual(0), Some(2));
	// Every cluster owns the glyph it was shaped into, at its VISUAL index.
	assert_eq!(map.clusters()[0].first_glyph, 2);
	assert_eq!(map.clusters()[2].first_glyph, 0);
	// A selection of the whole string is contiguous however it was reordered.
	assert!(map.selection_is_contiguous(font_contract::SourceRange { start: 0, end: 3 }));
}

#[test]
// A CLUSTER IS A GRAPHEME CLUSTER AND NOT A CHARACTER. A caret does not stand between a letter and
// the accent on it, so a mapping that made the mark its own cluster would offer a caret position that
// is not a place in the text.
fn a_combining_mark_does_not_begin_a_cluster_of_its_own() {
	let bytes = build::plain();
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let size = Fixed266::from_pixels(16);
	// "e" then U+0301, which is two characters and ONE grapheme cluster - and two glyphs.
	let text = "e\u{0301}f";
	let mark = Position { x_advance: 0, y_advance: 0, x_offset: 0, y_offset: 250 };
	let shaped = buffer(&[(1, 0, advance(500)), (2, 1, mark), (3, 3, advance(800))]);
	let run = produce(&request(&face, text, Direction::LeftToRight, size), &shaped).expect("a run");
	let map = run.cluster_map().expect("a mapping");
	assert_eq!(map.clusters().len(), 2, "the letter with its accent is one cluster, and the next letter is the other");
	assert_eq!(map.clusters()[0].source, font_contract::SourceRange { start: 0, end: 3 });
	assert_eq!(map.clusters()[0].glyph_count, 2, "one cluster, two glyphs - which is what a decomposition is");
	assert_eq!(map.clusters()[1].source, font_contract::SourceRange { start: 3, end: 4 });
	// And a hit test lands in the cluster that contains the byte, including the mark's own bytes.
	assert_eq!(map.cluster_at(1), Some(0));
	assert_eq!(map.cluster_at(3), Some(1));
}

#[test]
// WHERE THE CARET GOES INSIDE A LIGATURE. `fi` is one glyph for two characters; a mapping that could
// only put the caret at its two ends would make the middle of the word unreachable - press the arrow
// key and the caret jumps two characters. The dividing position is not derivable from the width
// either: the designer who drew the ligature said where its parts divide, and that is what `GDEF`
// carries.
fn a_ligature_keeps_the_clusters_it_absorbed_and_carries_their_carets() {
	let bytes = build::font(&[
		(*b"GDEF", build::gdef_carets(1, &[300])),
		(*b"head", build::head(1000)),
		(*b"hhea", build::hhea(4)),
		(*b"hmtx", build::hmtx(&[(500, 0), (1000, 0), (800, 0), (600, 0)])),
		(*b"maxp", build::maxp(4)),
	]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let size = Fixed266::from_pixels(16);
	// One glyph for two characters, as a ligature leaves the buffer: the cluster of the result is the
	// FIRST one's, and the second is gone from the buffer but not from the text.
	let shaped = buffer(&[(1, 0, advance(1000))]);
	let run = produce(&request(&face, "fi", Direction::LeftToRight, size), &shaped).expect("a run");
	let map = run.cluster_map().expect("a mapping");
	assert_eq!(map.clusters().len(), 2, "two characters are two caret stops, whatever the shaper did with them");
	assert_eq!(map.clusters()[0].glyph_count, 1);
	// THE ABSORBED CLUSTER OWNS NO GLYPH OF ITS OWN and says so, while pointing at the glyph that
	// swallowed it - which is how a hit test knows which pixels to measure within.
	assert_eq!(map.clusters()[1].glyph_count, 0);
	assert_eq!(map.clusters()[1].first_glyph, 0);
	// The dividing caret the face declares: 300 units at 16 px on a grid of 1000 is 4.8 px, which is
	// 307 in 26.6 after round-half-to-even.
	let carets = map.intra_ligature_carets(0);
	assert_eq!(carets.len(), 1);
	assert_eq!(carets[0], Fixed266::from_raw(307));
	// The absorbed cluster carries none of its own: the dividing positions belong to the glyph.
	assert!(map.intra_ligature_carets(1).is_empty());
	// A face with no `GDEF` at all has no carets, which is not an error and is most faces.
	let plain = build::plain();
	let plain = Face::open(&plain, 0).expect("a font this tree built");
	let run = produce(&request(&plain, "fi", Direction::LeftToRight, size), &shaped).expect("a run");
	assert!(run.cluster_map().expect("a mapping").intra_ligature_carets(0).is_empty());
}

#[test]
// "DRAW THE GLYPH" IS NOT ONE INSTRUCTION. The same glyph index may be an outline, a colour layer
// list, a colour paint graph or a bitmap out of a strike, and they are drawn by four different pieces
// of code. A run that did not say which would leave every consumer to ask the face again, once per
// glyph, and to disagree with the producer about the answer.
fn a_glyph_is_marked_with_the_form_the_face_actually_offers() {
	// Version 0 alone: the covered glyph is a layer list, and the others are outlines.
	let bytes = build::font(&[
		(*b"CPAL", build::cpal()),
		(*b"COLR", build::colr_v0(&[2])),
		(*b"head", build::head(1000)),
		(*b"hhea", build::hhea(4)),
		(*b"hmtx", build::hmtx(&[(500, 0), (1000, 0), (800, 0), (600, 0)])),
		(*b"maxp", build::maxp(4)),
	]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let size = Fixed266::from_pixels(16);
	let shaped = buffer(&[(1, 0, advance(500)), (2, 1, advance(1000))]);
	let run = produce(&request(&face, "ab", Direction::LeftToRight, size), &shaped).expect("a run");
	let glyphs = run.run().glyphs;
	assert_eq!(glyphs[0].kind, GlyphKind::Outline);
	assert_eq!(glyphs[1].kind, GlyphKind::ColrLayers);
	// AND THE PALETTE IS PART OF THE ENTRY, because two colour glyphs differing only in palette are
	// two rasterisations and must not share a cache entry.
	assert_eq!(glyphs[1].selection.palette, Some(0));
	assert_eq!(glyphs[0].selection.palette, None, "an outline's key does not carry a palette that means nothing");

	// Version 1 beside version 0: the richer form wins, which is what a face carrying both means.
	let bytes = build::font(&[
		(*b"CPAL", build::cpal()),
		(*b"COLR", build::colr_v1(&[2], &[2, 3])),
		(*b"head", build::head(1000)),
		(*b"hhea", build::hhea(4)),
		(*b"hmtx", build::hmtx(&[(500, 0), (1000, 0), (800, 0), (600, 0)])),
		(*b"maxp", build::maxp(4)),
	]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let shaped = buffer(&[(2, 0, advance(1000)), (3, 1, advance(600)), (1, 2, advance(500))]);
	let run = produce(&request(&face, "abc", Direction::LeftToRight, size), &shaped).expect("a run");
	let glyphs = run.run().glyphs;
	assert_eq!(glyphs[0].kind, GlyphKind::ColrPaintGraph, "a glyph in both lists is the richer form");
	assert_eq!(glyphs[1].kind, GlyphKind::ColrPaintGraph);
	assert_eq!(glyphs[2].kind, GlyphKind::Outline);
}

#[test]
// A STRIKE IS MADE AT A SIZE. Picking the wrong one is a blurred glyph rather than a wrong one, which
// is the kind of defect that is never filed and never fixed.
fn a_bitmap_strike_is_chosen_by_the_size_the_text_is_set_at() {
	let tables = |strikes: &[(u16, &[u16])]| {
		build::font(&[
			(*b"head", build::head(1000)),
			(*b"hhea", build::hhea(4)),
			(*b"hmtx", build::hmtx(&[(500, 0), (1000, 0), (800, 0), (600, 0)])),
			(*b"maxp", build::maxp(4)),
			(*b"sbix", build::sbix(4, strikes)),
		])
	};
	let bytes = tables(&[(20u16, &[2u16][..]), (40u16, &[2u16][..]), (80u16, &[2u16][..])]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let shaped = buffer(&[(2, 0, advance(1000))]);
	// Thirty-two pixels: the smallest strike that is big enough is the forty, because scaling DOWN
	// keeps the detail the designer drew and scaling up does not.
	let run = produce(&request(&face, "a", Direction::LeftToRight, Fixed266::from_pixels(32)), &shaped).expect("a run");
	assert_eq!(run.run().glyphs[0].kind, GlyphKind::BitmapStrike);
	assert_eq!(run.run().glyphs[0].selection.strike, Some(1));
	// Exactly twenty: the strike that matches, not the one above it.
	let run = produce(&request(&face, "a", Direction::LeftToRight, Fixed266::from_pixels(20)), &shaped).expect("a run");
	assert_eq!(run.run().glyphs[0].selection.strike, Some(0));
	// Bigger than every strike: the largest, which is the least scaling up.
	let run = produce(&request(&face, "a", Direction::LeftToRight, Fixed266::from_pixels(120)), &shaped).expect("a run");
	assert_eq!(run.run().glyphs[0].selection.strike, Some(2));
	// A GLYPH NO STRIKE HOLDS IS AN OUTLINE, not a bitmap that does not exist.
	let shaped = buffer(&[(1, 0, advance(500))]);
	let run = produce(&request(&face, "a", Direction::LeftToRight, Fixed266::from_pixels(32)), &shaped).expect("a run");
	assert_eq!(run.run().glyphs[0].kind, GlyphKind::Outline);
	assert_eq!(run.run().glyphs[0].selection.strike, None);
}

#[test]
// OVERFLOW IS A REFUSAL AND NEVER A SATURATION. A saturated advance is a position that is silently
// wrong, and a position that is silently wrong is the failure that cannot be found from the picture.
fn a_value_outside_the_seam_s_numeric_form_refuses_the_run() {
	let bytes = build::font(&[(*b"head", build::head(1)), (*b"hhea", build::hhea(2)), (*b"hmtx", build::hmtx(&[(0, 0), (0, 0)])), (*b"maxp", build::maxp(2))]);
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	// One unit to the em and a large size: the scaled advance leaves 26.6 entirely.
	let size = Fixed266::from_pixels(30000);
	let shaped = buffer(&[(1, 0, advance(i32::MAX / 2))]);
	assert_eq!(produce(&request(&face, "a", Direction::LeftToRight, size), &shaped).err(), Some(Error::Overflow(font_contract::Overflow::Value)));
}

#[test]
// A RUN PAST THE PROFILE'S OUTPUT CEILING IS A DOCUMENT ASKING FOR WORK, and it is refused by name
// rather than laid out: a ceiling that produced a truncated run would be a document silently
// rendered wrong, which is the failure that is never reported because it does not look like one.
fn a_run_past_the_frozen_output_ceiling_is_refused_by_name() {
	let bytes = build::plain();
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let ceiling = opentype_profile::limits::RUN_OUTPUT as usize;
	let mut shaped = Buffer::new();
	for _ in 0..=ceiling {
		shaped.push(1, 0);
	}
	assert_eq!(produce(&request(&face, "a", Direction::LeftToRight, Fixed266::from_pixels(16)), &shaped).err(), Some(Error::Exceeded { limit: "run output", ceiling: opentype_profile::limits::RUN_OUTPUT, asked: ceiling as u64 + 1 }));
	// AND EXACTLY AT THE CEILING IT IS NOT REFUSED, which is what makes the number a ceiling rather
	// than an approximate one.
	let mut shaped = Buffer::new();
	for _ in 0..ceiling {
		shaped.push(1, 0);
	}
	assert!(produce(&request(&face, "a", Direction::LeftToRight, Fixed266::from_pixels(16)), &shaped).is_ok());
}

#[test]
// A RUN THAT NAMES A RANGE OUTSIDE ITS OWN TEXT IS REFUSED rather than trusted: every cluster range
// this produces is an offset into that string, and a range outside it would be a mapping that indexes
// past the text a hit test asks about.
fn a_range_outside_the_text_is_refused() {
	let bytes = build::plain();
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let mut asked = request(&face, "ab", Direction::LeftToRight, Fixed266::from_pixels(16));
	asked.end = 9;
	assert_eq!(produce(&asked, &buffer(&[(1, 0, advance(500))])).err(), Some(Error::ClusterOutOfRange));
}

#[test]
// THE CACHE KEY IS BUILT FROM THE RUN, BY THE RUN, so a consumer cannot assemble one that omits a
// field - and what this checks is that a produced run actually yields one.
fn a_produced_run_yields_the_cache_key_a_consumer_rasterises_through() {
	let bytes = build::plain();
	let face = Face::open(&bytes, 0).expect("a font this tree built");
	let size = Fixed266::from_pixels(16);
	let run = produce(&request(&face, "ab", Direction::LeftToRight, size), &buffer(&[(1, 0, advance(500)), (2, 1, advance(1000))])).expect("a run");
	let view = run.run();
	let key = view.cache_key(0).expect("a glyph this run has");
	assert_eq!(key.glyph, 1);
	assert_eq!(key.size, size);
	assert_eq!(key.generation, Generation(3));
	assert_eq!(key.kind, GlyphKind::Outline);
	// The whole run's advance, refused rather than saturated if it did not fit.
	assert_eq!(view.total_advance().expect("a total that fits"), (Fixed266::from_raw(1536), Fixed266::ZERO));
	// And the affinity type the mapping is read with exists on both sides of a boundary.
	assert_ne!(CaretAffinity::Leading, CaretAffinity::Trailing);
}
