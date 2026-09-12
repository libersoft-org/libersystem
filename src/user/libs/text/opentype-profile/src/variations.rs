//! The variation mechanisms, named as MECHANISMS rather than as tables.
//!
//! A TABLE LIST CANNOT SAY THIS. `fvar` and `gvar` make an outline vary; they do nothing for a
//! metric, and a font instanced without `HVAR` has correct outlines at the wrong advances - text
//! that is subtly mis-spaced at every non-default coordinate, which reads as a rendering bug rather
//! than as a missing table. `MVAR` is the same for the font-wide metrics line layout reads, and CFF2
//! varies through `blend` in the charstring rather than through a delta table at all.

/// One mechanism the profile implements.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VariationMechanism {
	pub name: &'static str,
	/// What it varies, and what is wrong without it.
	pub what: &'static str,
}

/// Every variation mechanism `OpenType Profile 1` implements.
pub const MECHANISMS: &[VariationMechanism] = &[
	VariationMechanism { name: "fvar axes", what: "the axes themselves and their ranges; without it a font has no instances to select" },
	VariationMechanism { name: "fvar named instances", what: "the named positions a catalogue declaration and a user both name" },
	VariationMechanism { name: "avar normalisation", what: "the mapping from a user coordinate to a normalised one; without it an instance lands somewhere else on a font that maps its axes" },
	VariationMechanism { name: "gvar outline deltas", what: "`glyf` outlines at a coordinate, including the inferred-point rule for unreferenced points" },
	VariationMechanism { name: "CFF2 blend", what: "CFF2 outlines, varied in the charstring through `blend` and the region selected by `vsindex`" },
	VariationMechanism { name: "item variation store", what: "the shared delta storage `HVAR`, `VVAR`, `MVAR`, `GDEF` and CFF2 all read through" },
	VariationMechanism { name: "delta-set index map", what: "the indirection from a glyph or a value to its delta set" },
	VariationMechanism { name: "HVAR advances", what: "horizontal advances at a coordinate - what makes a metric correct rather than merely present" },
	VariationMechanism { name: "VVAR advances", what: "the same vertically, which the run contract's vertical directions need" },
	VariationMechanism { name: "MVAR font metrics", what: "ascender, descender, line gap, x-height, cap height and the rest, at a coordinate" },
	VariationMechanism { name: "GDEF variation store", what: "varying attachment points and caret positions" },
	VariationMechanism { name: "FeatureVariations", what: "WHICH lookups apply at a coordinate; see the layout module" },
];

/// Is this mechanism in the profile?
pub fn mechanism(name: &str) -> Option<&'static VariationMechanism> {
	MECHANISMS.iter().find(|entry| entry.name == name)
}
