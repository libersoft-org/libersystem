//! The tables the profile reads, AT THE VERSIONS IT READS THEM - and the ones it refuses by name.
//!
//! A VERSION IS PART OF SUPPORT. A table whose major version this parser has never seen is not a
//! table it can read carefully; it is one whose layout it would be guessing at. So every entry
//! carries the versions admitted, and a font at any other version is a typed refusal naming the tag
//! and the version rather than a parse that half works.

use crate::Tag;

/// One table the profile reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TableSupport {
	pub tag: Tag,
	/// The major versions admitted, in the order the format numbers them. A table with no version
	/// field of its own carries an empty list, and `versions_are_checked` says which those are.
	pub majors: &'static [u16],
	/// Why this table is in the profile - one line, because a reader asking "why is `MVAR` here"
	/// deserves the answer beside it rather than in a paragraph somewhere else.
	pub reason: &'static str,
}

/// Something the profile REFUSES, named so a reader can tell a decision from an oversight.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Excluded {
	pub what: &'static str,
	/// The table tags this excludes ENTIRELY. Empty when the exclusion is narrower than a table -
	/// `avar` version 2 is excluded while `avar` is supported, and the hinting instructions live
	/// inside a `glyf` this profile reads.
	///
	/// A FIELD RATHER THAN A SENTENCE, because the check that nothing is both supported and excluded
	/// has to be exact. Scraping a tag out of the prose could not tell those two cases apart, and a
	/// version-level exclusion read as a table-level one would report a contradiction that is not
	/// there - or, worse, miss one that is.
	pub tags: &'static [[u8; 4]],
	pub reason: &'static str,
}

const fn table(tag: &'static [u8; 4], majors: &'static [u16], reason: &'static str) -> TableSupport {
	TableSupport { tag: Tag::new(tag), majors, reason }
}

/// The tables `OpenType Profile 1` reads.
pub const TABLES: &[TableSupport] = &[
	// The required core. A font without these is not one this parser will open at all.
	table(b"head", &[1], "the design grid, the index-to-loc format and the font-wide bounding box"),
	table(b"hhea", &[1], "the horizontal metrics header, and how many are in `hmtx`"),
	table(b"hmtx", &[], "horizontal advances and left side bearings"),
	table(b"maxp", &[0, 1], "the glyph count; version 0.5 for `CFF` outlines and 1.0 for `glyf`"),
	table(b"cmap", &[0], "character to glyph, in the subtable formats the profile admits"),
	table(b"name", &[0, 1], "the family, subfamily and instance names a catalogue declaration is checked against"),
	table(b"OS/2", &[1, 2, 3, 4, 5], "the weight, width and slant class, and the typographic metrics line layout reads"),
	table(b"post", &[2, 3], "glyph names where version 2.0 carries them; 3.0 carries none and that is not an error"),
	// Outlines. One of the two is required, and a font carrying both is one whose `glyf` wins,
	// because `head.indexToLocFormat` and `loca` are what a `glyf` font is read through.
	table(b"loca", &[], "the offset of each glyph in `glyf`, short or long per `head`"),
	table(b"glyf", &[], "quadratic outlines, composite glyphs and their point-matching form"),
	table(b"CFF ", &[1], "Type 2 charstrings with local and global subroutines"),
	table(b"CFF2", &[2], "CFF2 charstrings with `blend` and variation-store selection"),
	// Vertical layout. IN THE PROFILE BECAUSE THE SHARED RUN CONTRACT ADMITS VERTICAL DIRECTIONS -
	// `font-contract`'s `Direction` carries `TopToBottom` and `BottomToTop` - and a profile that
	// excluded these would make that contract unimplementable.
	table(b"vhea", &[1], "the vertical metrics header"),
	table(b"vmtx", &[], "vertical advances and top side bearings"),
	table(b"VORG", &[1], "the vertical origin, where a font states it rather than deriving it"),
	// Shaping.
	table(b"GDEF", &[1], "glyph classes, mark attachment classes, mark glyph sets and the variation store"),
	table(b"GSUB", &[1], "substitution, including `FeatureVariations`"),
	table(b"GPOS", &[1], "positioning, including device and variation adjustments"),
	// Variation.
	table(b"fvar", &[1], "the axes and the named instances"),
	table(b"gvar", &[1], "outline deltas for `glyf` fonts"),
	table(b"avar", &[1], "axis normalisation mapping"),
	table(b"HVAR", &[1], "horizontal advance variation - what makes a metric correct at a non-default coordinate"),
	table(b"VVAR", &[1], "the same for vertical advances"),
	table(b"MVAR", &[1], "the font-wide metrics line layout reads, varied"),
	// Colour and bitmaps.
	table(b"COLR", &[0, 1], "layer lists (v0) and the paint graph (v1)"),
	table(b"CPAL", &[0, 1], "the palettes a colour glyph names"),
	table(b"sbix", &[1], "bitmap strikes"),
	table(b"CBDT", &[2, 3], "embedded colour bitmap data"),
	table(b"CBLC", &[2, 3], "the location table for `CBDT`"),
];

/// What the profile refuses BY NAME, so an omission is visibly a decision.
pub const EXCLUDED: &[Excluded] = &[
	Excluded { what: "SVG glyph outlines", tags: &[*b"SVG "], reason: "SVG parsing is deferred by name elsewhere in this tree; admitting it here would make a font an entry point to an XML parser" },
	Excluded { what: "the legacy kern table", tags: &[*b"kern"], reason: "GPOS is the mechanism this profile positions with, and honouring both means deciding which wins in the fonts that carry two disagreeing answers" },
	Excluded { what: "Apple Advanced Typography", tags: &[*b"morx", *b"mort", *b"kerx", *b"feat"], reason: "a second, differently shaped shaping engine, for fonts that also carry GSUB and GPOS - two engines means deciding which one a font meant" },
	Excluded { what: "monochrome bitmap strikes as a glyph source", tags: &[*b"EBDT", *b"EBLC", *b"EBSC"], reason: "the run contract requires a face that can be transformed and scaled, and a strike-only face cannot answer that; colour strikes are admitted because they are a colour glyph's own form rather than the face's outlines" },
	Excluded { what: "avar version 2", tags: &[], reason: "its axis-mapping model is a different one, and a profile that admitted it silently would change what a named instance means; avar version 1 IS supported, which is why this exclusion names no tag" },
	Excluded { what: "hinting programs, and the instructions inside glyf", tags: &[*b"fpgm", *b"prep", *b"cvt "], reason: "a bytecode interpreter is a second execution engine reading untrusted input; this profile rasterises hinting-free, which is what its subpixel positioning is for" },
];

/// Is this tag in the profile at all?
pub fn table_of(tag: &[u8; 4]) -> Option<&'static TableSupport> {
	TABLES.iter().find(|entry| entry.tag.0 == *tag)
}

/// Does the profile admit this table at this major version?
///
/// A TABLE WITH NO VERSION FIELD ADMITS ANY, because there is nothing to check: `hmtx` is a run of
/// numbers whose shape comes from `hhea`, and pretending to check a version it does not have would
/// be a check that always passes dressed as one that does something.
pub fn admits_version(tag: &[u8; 4], major: u16) -> bool {
	match table_of(tag) {
		Some(entry) => entry.majors.is_empty() || entry.majors.contains(&major),
		None => false,
	}
}
