//! The shaping surface: which `GSUB` and `GPOS` lookups, which flags, which subtable formats.
//!
//! ENUMERATED BY TYPE, because a lookup type is a different algorithm rather than a different value.
//! "Supports GSUB" is not a statement anybody can implement against: a font using chained contextual
//! substitution and one using ligature substitution ask for two unrelated pieces of code, and a
//! parser that has one and not the other must REFUSE the other rather than ignore it - an ignored
//! substitution renders text that looks plausible and is wrong, which is the failure this whole
//! profile is shaped against.

/// A `GSUB` lookup type the profile applies.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GsubLookup {
	pub kind: u16,
	pub name: &'static str,
	/// The subtable formats admitted for it.
	pub formats: &'static [u16],
}

/// A `GPOS` lookup type the profile applies.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GposLookup {
	pub kind: u16,
	pub name: &'static str,
	pub formats: &'static [u16],
}

/// A lookup flag the profile honours.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LookupFlag {
	pub bit: u16,
	pub name: &'static str,
	pub effect: &'static str,
}

/// A format of one of the shared structures - `cmap` subtables, `Coverage`, `ClassDef`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SubtableFormat {
	pub structure: &'static str,
	pub format: u16,
	pub what: &'static str,
}

/// The eight `GSUB` lookup types. All of them: a profile missing one is a font whose text is
/// silently wrong rather than refused.
pub const GSUB_LOOKUPS: &[GsubLookup] = &[
	GsubLookup { kind: 1, name: "Single", formats: &[1, 2] },
	GsubLookup { kind: 2, name: "Multiple", formats: &[1] },
	GsubLookup { kind: 3, name: "Alternate", formats: &[1] },
	GsubLookup { kind: 4, name: "Ligature", formats: &[1] },
	GsubLookup { kind: 5, name: "Contextual", formats: &[1, 2, 3] },
	GsubLookup { kind: 6, name: "ChainedContextual", formats: &[1, 2, 3] },
	GsubLookup { kind: 7, name: "Extension", formats: &[1] },
	GsubLookup { kind: 8, name: "ReverseChainedContextualSingle", formats: &[1] },
];

/// The nine `GPOS` lookup types.
pub const GPOS_LOOKUPS: &[GposLookup] = &[
	GposLookup { kind: 1, name: "SingleAdjustment", formats: &[1, 2] },
	GposLookup { kind: 2, name: "PairAdjustment", formats: &[1, 2] },
	GposLookup { kind: 3, name: "CursiveAttachment", formats: &[1] },
	GposLookup { kind: 4, name: "MarkToBase", formats: &[1] },
	GposLookup { kind: 5, name: "MarkToLigature", formats: &[1] },
	GposLookup { kind: 6, name: "MarkToMark", formats: &[1] },
	GposLookup { kind: 7, name: "Contextual", formats: &[1, 2, 3] },
	GposLookup { kind: 8, name: "ChainedContextual", formats: &[1, 2, 3] },
	GposLookup { kind: 9, name: "Extension", formats: &[1] },
];

/// The lookup flags the profile honours. A flag it did not honour would apply a lookup to glyphs the
/// font excluded from it, which is a positioning error that looks like a font bug.
pub const LOOKUP_FLAGS: &[LookupFlag] = &[
	LookupFlag { bit: 0x0001, name: "rightToLeft", effect: "cursive attachment takes its direction from the run rather than the font" },
	LookupFlag { bit: 0x0002, name: "ignoreBaseGlyphs", effect: "base glyphs are skipped when matching" },
	LookupFlag { bit: 0x0004, name: "ignoreLigatures", effect: "ligature glyphs are skipped when matching" },
	LookupFlag { bit: 0x0008, name: "ignoreMarks", effect: "mark glyphs are skipped when matching" },
	LookupFlag { bit: 0x0010, name: "useMarkFilteringSet", effect: "only the marks in the named `GDEF` mark glyph set participate" },
	LookupFlag { bit: 0xFF00, name: "markAttachmentType", effect: "only marks of the named `GDEF` attachment class participate" },
];

/// The subtable formats of the shared structures.
///
/// `cmap` FORMAT 14 IS HERE ON PURPOSE: Unicode variation sequences are what select the right form
/// of a CJK character and the text presentation of an emoji, and a parser without it renders the
/// default form while the document asked for another.
pub const SUBTABLE_FORMATS: &[SubtableFormat] = &[
	SubtableFormat { structure: "cmap", format: 4, what: "segment mapping to delta values, the BMP workhorse" },
	SubtableFormat { structure: "cmap", format: 6, what: "a single trimmed range" },
	SubtableFormat { structure: "cmap", format: 12, what: "segmented coverage above the BMP" },
	SubtableFormat { structure: "cmap", format: 14, what: "Unicode variation sequences" },
	SubtableFormat { structure: "Coverage", format: 1, what: "a sorted glyph list" },
	SubtableFormat { structure: "Coverage", format: 2, what: "sorted ranges" },
	SubtableFormat { structure: "ClassDef", format: 1, what: "a contiguous run of class values" },
	SubtableFormat { structure: "ClassDef", format: 2, what: "class ranges" },
];

/// The two mechanisms that make a lookup's effect depend on the instance, rather than on the glyph.
///
/// BOTH ARE PART OF THE PROFILE AND NEITHER IS OPTIONAL. `FeatureVariations` is how a variable font
/// changes WHICH lookups apply at a coordinate - a font can substitute a different `$` at a heavy
/// weight - and device/variation adjustments are how a `GPOS` value varies. A parser with the
/// variation tables and neither of these is one whose positioning is right at the default instance
/// and wrong everywhere else.
pub const CONDITIONAL_MECHANISMS: &[SubtableFormat] = &[
	SubtableFormat { structure: "FeatureVariations", format: 1, what: "condition sets selecting an alternate feature table at a coordinate" },
	SubtableFormat { structure: "ConditionSet", format: 1, what: "axis ranges a condition is true over" },
	SubtableFormat { structure: "Device", format: 1, what: "per-ppem adjustment deltas" },
	SubtableFormat { structure: "Device", format: 2, what: "per-ppem adjustment deltas" },
	SubtableFormat { structure: "Device", format: 3, what: "per-ppem adjustment deltas" },
	SubtableFormat { structure: "VariationIndex", format: 0x8000, what: "a `GPOS` value varying through the item variation store" },
];

/// Does the profile apply this `GSUB` lookup type?
pub fn gsub_lookup(kind: u16) -> Option<&'static GsubLookup> {
	GSUB_LOOKUPS.iter().find(|lookup| lookup.kind == kind)
}

/// Does the profile apply this `GPOS` lookup type?
pub fn gpos_lookup(kind: u16) -> Option<&'static GposLookup> {
	GPOS_LOOKUPS.iter().find(|lookup| lookup.kind == kind)
}

/// Does the profile admit this format of one of the shared structures?
pub fn admits_format(structure: &str, format: u16) -> bool {
	SUBTABLE_FORMATS.iter().any(|entry| entry.structure == structure && entry.format == format)
}
