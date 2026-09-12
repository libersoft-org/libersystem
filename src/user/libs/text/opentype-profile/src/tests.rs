use super::*;

#[test]
// THE PROFILE'S OWN PROMISE: the two sentences that could not both be met by a table list. Producing
// outlines from CFF/CFF2 needs the charstring interpreter and CFF2's `blend`; correct METRICS at a
// non-default coordinate needs `HVAR` and `MVAR`. A profile with the outline half and not the metric
// half renders text that is correctly shaped and wrongly spaced.
fn the_profile_carries_both_halves_of_correct_at_any_coordinate() {
	assert!(tables::table_of(b"CFF ").is_some());
	assert!(tables::table_of(b"CFF2").is_some());
	assert!(variations::mechanism("CFF2 blend").is_some(), "CFF2 varies in the charstring, not through a delta table");
	for required in ["HVAR advances", "MVAR font metrics", "item variation store", "delta-set index map"] {
		assert!(variations::mechanism(required).is_some(), "{required} is what makes a metric correct at a coordinate");
	}
	// AND THE VERTICAL HALF, because the shared run contract admits vertical directions and a
	// profile that excluded these would make that contract unimplementable.
	assert!(tables::table_of(b"vhea").is_some() && tables::table_of(b"vmtx").is_some());
	assert!(variations::mechanism("VVAR advances").is_some());
}

#[test]
// A TABLE AT A VERSION THE PROFILE HAS NOT SEEN IS A REFUSAL, not a parse that guesses at a layout.
fn a_table_outside_the_profile_or_at_an_unknown_version_is_refused() {
	assert!(tables::admits_version(b"OS/2", 4));
	assert!(!tables::admits_version(b"OS/2", 6), "a version this parser has never seen is one it would be guessing at");
	assert!(!tables::admits_version(b"SVG ", 0), "SVG outlines are excluded by name");
	assert!(!tables::admits_version(b"morx", 2), "AAT is a second shaping engine and is excluded");
	// A TABLE WITH NO VERSION FIELD ADMITS ANY, because there is nothing to check - and saying so is
	// better than a check that always passes while looking like one that does something.
	assert!(tables::admits_version(b"hmtx", 0) && tables::admits_version(b"hmtx", 99));
	assert!(tables::table_of(b"hmtx").expect("in the profile").majors.is_empty());
}

#[test]
// THE EXCLUSIONS ARE NAMED, so a reader can tell a decision from an oversight - and each carries its
// reason, because "not supported" is not a reason.
fn every_exclusion_states_why() {
	assert!(tables::EXCLUDED.len() >= 6);
	for excluded in tables::EXCLUDED {
		assert!(!excluded.what.is_empty() && excluded.reason.len() > 20, "{} has no stated reason", excluded.what);
	}
	// AND NOTHING IS BOTH SUPPORTED AND EXCLUDED, which is the contradiction a growing list invites.
	// Checked against the tags an exclusion NAMES rather than against its prose: `avar` version 2 is
	// excluded while `avar` is supported, and a scan of the sentence could not tell those apart.
	for excluded in tables::EXCLUDED {
		for tag in excluded.tags {
			assert!(tables::table_of(tag).is_none(), "{} is named as both supported and excluded", core::str::from_utf8(tag).unwrap_or("????"));
		}
	}
	// THE NARROW EXCLUSIONS NAME NO TAG, and that is what makes the check above exact.
	assert!(tables::EXCLUDED.iter().any(|excluded| excluded.tags.is_empty() && excluded.what.contains("avar")));
}

#[test]
// ALL EIGHT `GSUB` TYPES AND ALL NINE `GPOS` TYPES. A lookup type missing from a profile is text
// that renders plausibly and wrongly: the substitution the font asked for simply does not happen.
fn every_lookup_type_the_format_has_is_in_the_profile() {
	for kind in 1..=8u16 {
		assert!(layout::gsub_lookup(kind).is_some(), "GSUB lookup type {kind} is missing");
	}
	assert!(layout::gsub_lookup(9).is_none(), "the format has eight; a ninth is a font that is wrong");
	for kind in 1..=9u16 {
		assert!(layout::gpos_lookup(kind).is_some(), "GPOS lookup type {kind} is missing");
	}
	assert!(layout::gpos_lookup(10).is_none());
	// EXTENSION IS NOT OPTIONAL: a large font puts its real lookups behind it, so a parser without it
	// sees an empty feature rather than a refusal.
	assert_eq!(layout::gsub_lookup(7).expect("extension").name, "Extension");
	assert_eq!(layout::gpos_lookup(9).expect("extension").name, "Extension");
	// AND THE FLAGS, all six, including the two that are a class rather than a bit.
	assert_eq!(layout::LOOKUP_FLAGS.len(), 6);
	assert!(layout::LOOKUP_FLAGS.iter().any(|flag| flag.name == "useMarkFilteringSet"));
	assert!(layout::LOOKUP_FLAGS.iter().any(|flag| flag.bit == 0xFF00));
}

#[test]
// THE TWO MECHANISMS THAT MAKE A LOOKUP DEPEND ON THE INSTANCE. Without them a variable font is
// positioned correctly at its default coordinate and wrongly everywhere else.
fn the_conditional_mechanisms_are_in_the_profile() {
	assert!(layout::CONDITIONAL_MECHANISMS.iter().any(|entry| entry.structure == "FeatureVariations"));
	assert!(layout::CONDITIONAL_MECHANISMS.iter().any(|entry| entry.structure == "VariationIndex"));
	assert!(variations::mechanism("FeatureVariations").is_some());
	// `cmap` FORMAT 14 IS THE ONE A TABLE LIST FORGETS: without it a variation sequence renders the
	// default form while the document asked for another.
	assert!(layout::admits_format("cmap", 14));
	assert!(layout::admits_format("cmap", 4) && layout::admits_format("cmap", 12));
	assert!(!layout::admits_format("cmap", 13), "format 13 is a many-to-one map this profile does not read");
	assert!(layout::admits_format("Coverage", 2) && layout::admits_format("ClassDef", 2));
}

#[test]
// THE PAINT GRAPH IS ENUMERATED, and a paint's variable counterpart is admitted with it - a `Var`
// paint is the same drawing operation reading its numbers through the variation store.
fn the_colour_paints_and_their_variable_counterparts_are_admitted_together() {
	for format in [1u8, 2, 4, 6, 8, 10, 11, 12, 32] {
		assert!(colour::paint(format).is_some(), "paint format {format} is part of COLR v1");
	}
	assert_eq!(colour::paint(5).expect("the variable linear gradient").name, "PaintLinearGradient");
	// NOT EVERY PAINT HAS A VARIABLE FORM. `PaintComposite` is 32 and there is no 33; a rule that
	// admitted `format + 1` for everything would have admitted one.
	assert_eq!(colour::paint(33), None, "there is no paint format 33");
	assert_eq!(colour::paint(12).expect("transform").varies, true);
	assert_eq!(colour::paint(1).expect("layers").varies, false, "a layer list carries no numbers of its own to vary");
	// THE COMPOSITE MODES ARE THE RENDERER'S OWN SET: a profile naming a mode the renderer does not
	// have would be a promise nothing could keep.
	assert_eq!(colour::COMPOSITE_MODES.len(), 28);
	for name in ["Clear", "SourceOver", "Xor", "Plus", "Multiply", "HslLuminosity"] {
		assert!(colour::COMPOSITE_MODES.iter().any(|mode| mode.name == name), "{name} is a composite mode COLR v1 may name");
	}
	assert!(colour::composite_mode(3).is_some() && colour::composite_mode(200).is_none());
	assert_eq!(colour::EXTEND_MODES.len(), 3);
	// AND BOTH BITMAP FAMILIES, because a colour emoji font carries one or the other.
	assert!(colour::BITMAP_FORMATS.iter().any(|format| format.table == "sbix"));
	assert!(colour::BITMAP_FORMATS.iter().any(|format| format.table == "CBDT"));
}

#[test]
// "INDIC" IS NOT A PROFILE ENTRY. Every script is named, and each carries the shaping CLASS that says
// how much work it is - which is what makes "supports Tamil" a claim anybody can check.
fn every_script_is_named_and_carries_its_shaping_class() {
	for (tag, class) in [
		(b"latn", ShapingClass::Default),
		(b"arab", ShapingClass::Cursive),
		(b"dev2", ShapingClass::IndicReordering),
		(b"mlm2", ShapingClass::IndicReordering),
		(b"khmr", ShapingClass::Khmer),
		(b"mym2", ShapingClass::Myanmar),
		(b"hang", ShapingClass::Hangul),
	] {
		let entry = scripts::script_support(tag).expect("a named script");
		assert_eq!(entry.class, class, "{} runs through the wrong engine", entry.name);
	}
	// THE RIGHT-TO-LEFT ONES ARE MARKED, and joining is a different question from direction: Hebrew
	// is right to left and does not join, Arabic is both.
	assert!(scripts::script_support(b"hebr").expect("Hebrew").right_to_left);
	assert_eq!(scripts::script_support(b"hebr").expect("Hebrew").class, ShapingClass::Default);
	assert!(scripts::script_support(b"arab").expect("Arabic").right_to_left);
	// A SCRIPT OUTSIDE THE LIST IS A REFUSAL, and that is a real answer rather than silence.
	assert!(scripts::script_support(b"egyp").is_none(), "Egyptian hieroglyphs are outside Profile 1");
	// NO SCRIPT IS NAMED TWICE, and no two share a tag.
	for (index, entry) in scripts::SCRIPTS.iter().enumerate() {
		for other in &scripts::SCRIPTS[index + 1..] {
			assert_ne!(entry.tag, other.tag, "{} is listed twice", entry.name);
		}
	}
	assert!(scripts::SCRIPTS.iter().any(|entry| entry.tag == *b"DFLT"), "every run passes through the default script");
}

#[test]
// A LANGUAGE IS IN THE LIST BECAUSE IT CHANGES SOMETHING, and what it changes is written down. A
// language with no stated effect did not need to be in a finite list.
fn every_language_states_what_selecting_it_changes() {
	assert!(scripts::language_support(b"TRK ").expect("Turkish").effect.contains("dotless"));
	assert!(scripts::language_support(b"ROM ").expect("Romanian").effect.contains("comma-below"));
	for entry in scripts::LANGUAGES {
		assert!(entry.effect.len() > 20, "{} does not say what it changes", entry.name);
		assert_eq!(entry.tag.len(), 4, "an OpenType LangSys tag is four bytes, space padded");
	}
	// AN UNKNOWN LANGUAGE FALLS BACK TO `dflt` RATHER THAN REFUSING, which is what the format itself
	// does - and it is the one fallback in this profile, stated so the difference is deliberate.
	assert!(scripts::language_support(b"XXX ").is_none());
	assert!(scripts::language_support(b"dflt").is_some());
}

#[test]
// THE REFUSAL CARRIES WHAT WAS OUTSIDE THE PROFILE. A caller that cannot tell an unsupported table
// from an unsupported lookup cannot report anything useful, and neither can a conformance suite.
fn a_refusal_names_what_it_refused() {
	let refusals = [
		Unsupported::Table(*b"morx"),
		Unsupported::TableVersion { tag: *b"OS/2", major: 6, minor: 0 },
		Unsupported::SubtableFormat { table: *b"cmap", format: 13 },
		Unsupported::GsubLookup(9),
		Unsupported::GposLookup(10),
		Unsupported::Paint(33),
		Unsupported::Script(*b"egyp"),
		Unsupported::Variation("avar 2"),
		Unsupported::ExcludedByProfile("hinting programs"),
	];
	for (index, refusal) in refusals.iter().enumerate() {
		for other in &refusals[index + 1..] {
			assert_ne!(refusal, other, "two refusals must not compare equal, or a report cannot tell them apart");
		}
	}
	assert_eq!(PROFILE_VERSION, 1);
	assert_eq!(Tag::new(b"GSUB").as_str(), "GSUB");
}
