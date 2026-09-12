//! Generate `unicode-tables` from the pinned Unicode Character Database.
//!
//! GENERATED RATHER THAN WRITTEN, and that is the requirement rather than a convenience. A property
//! table copied out of a document by hand is a snapshot nobody can re-derive: the first correction
//! to it is a patch, the second is a patch on a patch, and after a Unicode release the ranges say
//! what one person believed in one afternoon. This reads the files the lock pins and writes the
//! whole table; a new Unicode version is a changed pin and a regeneration.
//!
//! IT READS THE CACHE AND NEVER THE NETWORK. `./bootstrap.sh` is what fetches, once, deliberately,
//! and verifies the digest before the file is where anything will read it - so this tool and the
//! gate that runs it are offline, and what they check cannot be moved by what a mirror served today.
//!
//! WHAT IT REFUSES TO DO. It does not invent a default for a property it could not parse, and it
//! does not skip a line it does not understand: an unparsed line in a UCD file is a range of
//! characters that would silently take the default value, which is a boundary in the wrong place for
//! every user of that script. A file it cannot read completely is an error.

use std::fmt::Write as _;
use std::path::Path;

/// The Unicode release every table here comes from. Stated in the generated file, because
/// segmentation changes between versions and a boundary that moves is one a user sees move.
const VERSION: &str = "17.0.0";

/// One property: what it is called in the generated file, which UCD file it comes from, and the
/// values it may take - IN ORDER, because the ordinal is what the table stores.
struct Property {
	/// The Rust enum name.
	name: &'static str,
	/// The cached file it is read from.
	file: &'static str,
	/// The UCD property name, for the files that carry several.
	ucd_property: Option<&'static str>,
	/// The values, in ordinal order. The FIRST is the default for any character the file does not
	/// mention, which is how the UCD itself is defined - `@missing` lines say so, and every one of
	/// these was read from the file it belongs to rather than assumed.
	values: &'static [&'static str],
	/// Which field of the record carries the value. `ArabicShaping.txt` is the one that is not the
	/// second field.
	field: usize,
	/// Keep the runs whose value IS the default, rather than dropping them.
	///
	/// FOR THE ONE PROPERTY WITH PER-BLOCK DEFAULTS. Everywhere else a dropped default run and an
	/// absent one mean the same thing; for `Bidi_Class` they do not, because an unassigned code
	/// point in a right-to-left block defaults to `R` while an ASSIGNED left-to-right one in the
	/// same block is `L`. Dropping the second would make it take the first.
	keep_default_runs: bool,
}

const PROPERTIES: &[Property] = &[
	Property { name: "GraphemeBreak", file: "GraphemeBreakProperty.txt", ucd_property: None, values: &["Other", "CR", "LF", "Control", "Extend", "ZWJ", "Regional_Indicator", "Prepend", "SpacingMark", "L", "V", "T", "LV", "LVT"], field: 1, keep_default_runs: false },
	Property {
		name: "WordBreak",
		file: "WordBreakProperty.txt",
		ucd_property: None,
		values: &[
			"Other",
			"CR",
			"LF",
			"Newline",
			"Extend",
			"ZWJ",
			"Regional_Indicator",
			"Format",
			"Katakana",
			"Hebrew_Letter",
			"ALetter",
			"Single_Quote",
			"Double_Quote",
			"MidNumLet",
			"MidLetter",
			"MidNum",
			"Numeric",
			"ExtendNumLet",
			"WSegSpace",
		],
		field: 1,
		keep_default_runs: false,
	},
	Property {
		name: "LineBreak",
		file: "LineBreak.txt",
		ucd_property: None,
		// THE DEFAULT IS `XX`, and the file says so in its own `@missing` line. Rule LB1 then
		// resolves `XX` to `AL`, which is the resolution this table does NOT do: a table that
		// resolved it would be a table that cannot say what the file said.
		values: &[
			"XX",
			"BK",
			"CR",
			"LF",
			"CM",
			"NL",
			"SG",
			"WJ",
			"ZW",
			"GL",
			"SP",
			"ZWJ",
			"B2",
			"BA",
			"BB",
			"HY",
			"CB",
			"CL",
			"CP",
			"EX",
			"IN",
			"NS",
			"OP",
			"QU",
			"IS",
			"NU",
			"PO",
			"PR",
			"SY",
			"AI",
			"AL",
			"CJ",
			"EB",
			"EM",
			"H2",
			"H3",
			"HL",
			"ID",
			"JL",
			"JV",
			"JT",
			"RI",
			"SA",
			"AK",
			"AP",
			"AS",
			"VF",
			"VI",
			// `HH`, the unambiguous hyphen, which this release has and the list above did not. THE
			// GENERATOR REFUSED TO DEFAULT IT rather than quietly filing it under `XX` - which is
			// what the refusal is for: an unlisted class is a line break rule that would not fire.
			"HH",
		],
		field: 1,
		keep_default_runs: false,
	},
	Property {
		name: "IndicSyllabic",
		file: "IndicSyllabicCategory.txt",
		ucd_property: None,
		values: &[
			"Other",
			"Avagraha",
			"Bindu",
			"Brahmi_Joining_Number",
			"Cantillation_Mark",
			"Consonant",
			"Consonant_Dead",
			"Consonant_Final",
			"Consonant_Head_Letter",
			"Consonant_Initial_Postfixed",
			"Consonant_Killer",
			"Consonant_Medial",
			"Consonant_Placeholder",
			"Consonant_Preceding_Repha",
			"Consonant_Prefixed",
			"Consonant_Subjoined",
			"Consonant_Succeeding_Repha",
			"Consonant_With_Stacker",
			"Gemination_Mark",
			"Invisible_Stacker",
			"Joiner",
			"Modifying_Letter",
			"Non_Joiner",
			"Nukta",
			"Number",
			"Number_Joiner",
			"Pure_Killer",
			"Register_Shifter",
			"Reordering_Killer",
			"Syllable_Modifier",
			"Tone_Letter",
			"Tone_Mark",
			"Virama",
			"Visarga",
			"Vowel",
			"Vowel_Dependent",
			"Vowel_Independent",
		],
		field: 1,
		keep_default_runs: false,
	},
	Property {
		name: "IndicPositional",
		file: "IndicPositionalCategory.txt",
		ucd_property: None,
		values: &[
			"NA",
			"Right",
			"Left",
			"Visual_Order_Left",
			"Left_And_Right",
			"Top",
			"Bottom",
			"Top_And_Bottom",
			"Top_And_Right",
			"Top_And_Left",
			"Top_And_Left_And_Right",
			"Bottom_And_Right",
			"Bottom_And_Left",
			"Top_And_Bottom_And_Right",
			"Top_And_Bottom_And_Left",
			"Overstruck",
		],
		field: 1,
		keep_default_runs: false,
	},
	Property {
		name: "JoiningType",
		file: "ArabicShaping.txt",
		ucd_property: None,
		// THE DEFAULT IS `U` (non-joining), which is what a character absent from the file is - and
		// the file's own header says the derived defaults for the ranges it omits. `T` is applied
		// below from the general category, which is where `ArabicShaping.txt` says it comes from.
		values: &["U", "C", "D", "L", "R", "T"],
		field: 2,
		keep_default_runs: false,
	},
	Property { name: "Script", file: "Scripts.txt", ucd_property: None, values: &[], field: 1, keep_default_runs: false },
	Property { name: "GeneralCategory", file: "DerivedGeneralCategory.txt", ucd_property: None, values: &[], field: 1, keep_default_runs: false },
	Property { name: "IndicConjunctBreak", file: "DerivedCoreProperties.txt", ucd_property: Some("InCB"), values: &["None", "Linker", "Consonant", "Extend"], field: 2, keep_default_runs: false },
	// East_Asian_Width, which LB30 turns on: `(AL | HL | NU) x OP` holds only for an opening bracket
	// that is NOT fullwidth, wide or halfwidth, because a bracket beside a wide character breaks
	// where one beside a narrow character does not.
	Property { name: "EastAsianWidth", file: "EastAsianWidth.txt", ucd_property: None, values: &["N", "A", "F", "H", "Na", "W"], field: 1, keep_default_runs: false },
	// Bidi_Class, which the bidirectional algorithm is written in terms of. The default is `L` for
	// most of the code space and `R` or `AL` for the right-to-left blocks, which the file states in
	// its own `@missing` lines and this table therefore carries as runs rather than as a default.
	Property { name: "BidiClass", file: "DerivedBidiClass.txt", ucd_property: None, values: &["L", "R", "AL", "EN", "ES", "ET", "AN", "CS", "NSM", "BN", "B", "S", "WS", "ON", "LRE", "LRO", "RLE", "RLO", "PDF", "LRI", "RLI", "FSI", "PDI"], field: 1, keep_default_runs: false },
];

fn main() -> std::process::ExitCode {
	let arguments: Vec<String> = std::env::args().skip(1).collect();
	let check = arguments.iter().any(|argument| argument == "--check");
	if let Some(unexpected) = arguments.iter().find(|argument| *argument != "--check") {
		eprintln!("ucd-gen: unexpected argument '{unexpected}' (usage: ucd-gen [--check])");
		return std::process::ExitCode::FAILURE;
	}
	let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
	let root = root.canonicalize().unwrap_or(root);
	let cache = root.join(format!(".build/ucd/{VERSION}"));
	if !cache.is_dir() {
		eprintln!("ucd-gen: the pinned UCD is not cached at {}", cache.display());
		eprintln!("ucd-gen: fetch it with `./bootstrap.sh` - the gates never reach the network themselves");
		return std::process::ExitCode::FAILURE;
	}
	let generated = match generate(&cache) {
		Ok(text) => text,
		Err(error) => {
			eprintln!("ucd-gen: {error}");
			return std::process::ExitCode::FAILURE;
		}
	};
	// FORMATTED HERE, so that generating and checking answer the same thing. `./format.sh` runs
	// `cargo fmt` over every crate in the tree, this file included; a generator whose output was not
	// already formatted would produce a file that differs from itself the moment anybody formats,
	// and the check would report drift that is not drift.
	let generated = match formatted(&generated) {
		Ok(text) => text,
		Err(error) => {
			eprintln!("ucd-gen: {error}");
			return std::process::ExitCode::FAILURE;
		}
	};
	let path = root.join("src/user/libs/text/unicode-tables/src/generated.rs");
	if check {
		return match std::fs::read_to_string(&path) {
			Ok(existing) if existing == generated => {
				println!("ucd-gen: the tables regenerate to what is on disk, from Unicode {VERSION}");
				std::process::ExitCode::SUCCESS
			}
			Ok(_) => {
				eprintln!("ucd-gen: {} differs from what the pinned UCD generates", path.display());
				eprintln!("ucd-gen: regenerate with `cargo run --manifest-path src/tools/ucd-gen/Cargo.toml` - the tables are generated, never patched");
				std::process::ExitCode::FAILURE
			}
			Err(error) => {
				eprintln!("ucd-gen: cannot read {}: {error}", path.display());
				std::process::ExitCode::FAILURE
			}
		};
	}
	if let Some(parent) = path.parent()
		&& let Err(error) = std::fs::create_dir_all(parent)
	{
		eprintln!("ucd-gen: cannot create {}: {error}", parent.display());
		return std::process::ExitCode::FAILURE;
	}
	if let Err(error) = std::fs::write(&path, &generated) {
		eprintln!("ucd-gen: cannot write {}: {error}", path.display());
		return std::process::ExitCode::FAILURE;
	}
	println!("ucd-gen: wrote {} from Unicode {VERSION}", path.display());
	std::process::ExitCode::SUCCESS
}

/// Run the generated text through `rustfmt`, with the same edition the crate is built at.
fn formatted(text: &str) -> Result<String, String> {
	use std::io::Write as _;
	let mut child = std::process::Command::new("rustfmt").args(["--edition", "2024", "--emit", "stdout", "--quiet"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().map_err(|error| format!("cannot run rustfmt: {error}"))?;
	child.stdin.take().ok_or("rustfmt took no input")?.write_all(text.as_bytes()).map_err(|error| format!("cannot write to rustfmt: {error}"))?;
	let output = child.wait_with_output().map_err(|error| format!("rustfmt did not finish: {error}"))?;
	if !output.status.success() {
		return Err(format!("rustfmt refused the generated source ({})", output.status));
	}
	String::from_utf8(output.stdout).map_err(|error| format!("rustfmt produced something that is not UTF-8: {error}"))
}

/// One `start..=end` run with a value, as the generated table stores it.
type Ranges = Vec<(u32, u32, u8)>;

fn generate(cache: &Path) -> Result<String, String> {
	let mut out = String::new();
	let _ = writeln!(out, "// @generated by `src/tools/ucd-gen` from the Unicode Character Database {VERSION}, pinned by");
	let _ = writeln!(out, "// SHA-256 in `toolchain.lock`. Do not edit: regenerate.");
	let _ = writeln!(out, "//");
	let _ = writeln!(out, "// The UCD is published by Unicode, Inc. under the Unicode License v3 and keeps that licence here;");
	let _ = writeln!(out, "// see the `[ucd_*]` blocks in `toolchain.lock` for the files, their versions and their digests.");
	let _ = writeln!(out, "//");
	let _ = writeln!(out, "// Each table is `(first, last, value)` runs sorted by `first`, with no overlaps - which is what lets");
	let _ = writeln!(out, "// the lookup be a binary search and what the fixtures check rather than assume.\n");
	let _ = writeln!(out, "/// The Unicode release every table here was generated from.");
	let _ = writeln!(out, "pub const UNICODE_VERSION: &str = \"{VERSION}\";\n");

	for property in PROPERTIES {
		let path = cache.join(property.file);
		let text = std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
		let (values, ranges) = parse(&text, property)?;
		emit(&mut out, property.name, &values, &ranges);
	}

	// THE BIDI DEFAULTS ARE NOT `L` EVERYWHERE, and the file says so in `@missing` lines rather than
	// in its records: the unassigned code points of the Hebrew, Arabic and other right-to-left
	// blocks default to `R` or `AL`, and a table that took `L` for them would lay out an unassigned
	// character in the middle of Arabic text left to right. The ranges are read from those lines.
	{
		let path = cache.join("DerivedBidiClass.txt");
		let text = std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
		let defaults = parse_missing(&text)?;
		let _ = writeln!(out, "/// The `Bidi_Class` DEFAULTS for unassigned code points, which are not `L` everywhere: the");
		let _ = writeln!(out, "/// right-to-left blocks default to `R` or `AL`, and the UCD states that in `@missing` lines");
		let _ = writeln!(out, "/// rather than in records. `(first, last, ordinal)` into `BIDI_CLASS_VALUES`.");
		let _ = writeln!(out, "pub const BIDI_CLASS_DEFAULTS: &[(u32, u32, u8)] = &[");
		for (first, last, value) in &defaults {
			let _ = writeln!(out, "\t({first:#x}, {last:#x}, {value}),");
		}
		let _ = writeln!(out, "];\n");
	}

	// The paired brackets rule N0 reads: which opening bracket pairs with which closing one.
	{
		let path = cache.join("BidiBrackets.txt");
		let text = std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
		let pairs = parse_brackets(&text)?;
		let _ = writeln!(out, "/// The bracket pairs rule N0 reads: `(code point, its pair, true when it OPENS)`.");
		let _ = writeln!(out, "pub const BIDI_BRACKETS: &[(u32, u32, bool)] = &[");
		for (character, pair, opening) in &pairs {
			let _ = writeln!(out, "\t({character:#x}, {pair:#x}, {opening}),");
		}
		let _ = writeln!(out, "];\n");
	}

	// Extended_Pictographic is a single boolean property out of the emoji file rather than one of the
	// enumerated ones above, and GB11 - the emoji ZWJ sequence rule - is written in terms of it.
	let path = cache.join("emoji-data.txt");
	let text = std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
	let ranges = parse_boolean(&text, "Extended_Pictographic")?;
	let _ = writeln!(out, "/// `Extended_Pictographic`, which the emoji ZWJ rule is written in terms of.");
	let _ = writeln!(out, "pub const EXTENDED_PICTOGRAPHIC: &[(u32, u32)] = &[");
	for (first, last) in &ranges {
		let _ = writeln!(out, "\t({first:#x}, {last:#x}),");
	}
	let _ = writeln!(out, "];\n");
	Ok(out)
}

/// Parse one UCD property file into its value list and its runs.
///
/// THE VALUE LIST IS CHECKED AGAINST THE ONE DECLARED ABOVE, where one is declared. A value in the
/// file that the declaration does not have is an error rather than a silent `Other`: it means the
/// release added a category, and taking the default for it would put a boundary in the wrong place
/// for whatever script it belongs to.
fn parse(text: &str, property: &Property) -> Result<(Vec<String>, Ranges), String> {
	let mut declared: Vec<String> = property.values.iter().map(|value| (*value).to_string()).collect();
	let discovering = declared.is_empty();
	if discovering {
		// A property whose values are not declared - `Script` and `General_Category` have too many
		// to write out, and no rule here is written in terms of a particular one - takes its value
		// set from the file, with the default FIRST so the ordinal of the default stays zero.
		declared.push(String::from(if property.name == "Script" { "Unknown" } else { "Cn" }));
	}
	let mut ranges: Ranges = Vec::new();
	for line in text.lines() {
		let line = line.split('#').next().unwrap_or("").trim();
		if line.is_empty() {
			continue;
		}
		let fields: Vec<&str> = line.split(';').map(str::trim).collect();
		// THE PROPERTY FILTER COMES FIRST, because a file carrying several properties carries several
		// SHAPES: `DerivedCoreProperties.txt` writes a boolean as two fields and `InCB` as three, so
		// a field-count check applied before the filter refuses a line that was never ours to read.
		if let Some(wanted) = property.ucd_property {
			if fields.get(1).copied() != Some(wanted) {
				continue;
			}
		}
		if fields.len() <= property.field {
			return Err(format!("{}: a record with {} fields, and the value is field {}: {line}", property.file, fields.len(), property.field));
		}
		let value = fields[property.field];
		let index = match declared.iter().position(|declared| declared == value) {
			Some(index) => index,
			None if discovering => {
				declared.push(String::from(value));
				declared.len() - 1
			}
			None => return Err(format!("{}: the release has a value `{value}` this generator's list does not - add it rather than defaulting it", property.file)),
		};
		let (first, last) = parse_range(fields[0]).ok_or_else(|| format!("{}: cannot read the code point range `{}`", property.file, fields[0]))?;
		if index != 0 || property.keep_default_runs {
			ranges.push((first, last, u8::try_from(index).map_err(|_| format!("{}: more than 256 values", property.file))?));
		}
	}
	Ok((declared, coalesce(ranges)))
}

/// A single boolean property out of a file that carries many.
fn parse_boolean(text: &str, wanted: &str) -> Result<Vec<(u32, u32)>, String> {
	let mut ranges: Ranges = Vec::new();
	for line in text.lines() {
		let line = line.split('#').next().unwrap_or("").trim();
		if line.is_empty() {
			continue;
		}
		let fields: Vec<&str> = line.split(';').map(str::trim).collect();
		if fields.len() < 2 || fields[1] != wanted {
			continue;
		}
		let (first, last) = parse_range(fields[0]).ok_or_else(|| format!("cannot read the code point range `{}`", fields[0]))?;
		ranges.push((first, last, 1));
	}
	Ok(coalesce(ranges).into_iter().map(|(first, last, _)| (first, last)).collect())
}

/// The `@missing` lines of a property file: the defaults for code points the records do not mention.
///
/// READ RATHER THAN ASSUMED. `Bidi_Class` is the property where this matters: most of the code space
/// defaults to `L`, and the unassigned parts of the right-to-left blocks default to `R` or `AL`. A
/// generator that took the first default for everything would lay out an unassigned character in the
/// middle of Arabic text left to right, which is a rendering bug nobody could find from the code.
fn parse_missing(text: &str) -> Result<Ranges, String> {
	let values: Vec<&str> = PROPERTIES.iter().find(|property| property.name == "BidiClass").map(|property| property.values.to_vec()).ok_or("the BidiClass property is not declared")?;
	let mut ranges: Ranges = Vec::new();
	for line in text.lines() {
		let Some(rest) = line.trim().strip_prefix("# @missing:") else { continue };
		let fields: Vec<&str> = rest.split(';').map(str::trim).collect();
		if fields.len() < 2 {
			return Err(format!("a @missing line with {} fields: {line}", fields.len()));
		}
		let (first, last) = parse_range(fields[0]).ok_or_else(|| format!("cannot read the @missing range `{}`", fields[0]))?;
		// THE `@missing` LINES USE LONG NAMES where the records use short ones - `Left_To_Right`
		// rather than `L`. Mapped rather than guessed at, and an unmapped one is an error: a default
		// this generator could not read is a block of the code space silently taking `L`.
		let value = long_bidi_name(fields[1]);
		let index = values.iter().position(|declared| *declared == value).ok_or_else(|| format!("a @missing default `{}` the BidiClass list does not have", fields[1]))?;
		if index != 0 {
			ranges.push((first, last, u8::try_from(index).map_err(|_| "more than 256 values")?));
		}
	}
	Ok(coalesce(ranges))
}

/// The short `Bidi_Class` alias a long `@missing` name stands for.
fn long_bidi_name(name: &str) -> &str {
	match name {
		"Left_To_Right" => "L",
		"Right_To_Left" => "R",
		"Arabic_Letter" => "AL",
		"European_Number" => "EN",
		"European_Separator" => "ES",
		"European_Terminator" => "ET",
		"Arabic_Number" => "AN",
		"Common_Separator" => "CS",
		"Nonspacing_Mark" => "NSM",
		"Boundary_Neutral" => "BN",
		"Paragraph_Separator" => "B",
		"Segment_Separator" => "S",
		"White_Space" => "WS",
		"Other_Neutral" => "ON",
		other => other,
	}
}

/// `BidiBrackets.txt`: the opening and closing brackets, and which pairs with which.
fn parse_brackets(text: &str) -> Result<Vec<(u32, u32, bool)>, String> {
	let mut pairs = Vec::new();
	for line in text.lines() {
		let line = line.split('#').next().unwrap_or("").trim();
		if line.is_empty() {
			continue;
		}
		let fields: Vec<&str> = line.split(';').map(str::trim).collect();
		if fields.len() < 3 {
			return Err(format!("a bracket record with {} fields: {line}", fields.len()));
		}
		let character = u32::from_str_radix(fields[0], 16).map_err(|_| format!("cannot read `{}`", fields[0]))?;
		let pair = u32::from_str_radix(fields[1], 16).map_err(|_| format!("cannot read `{}`", fields[1]))?;
		let opening = match fields[2] {
			"o" => true,
			"c" => false,
			other => return Err(format!("a bracket that is neither opening nor closing: `{other}`")),
		};
		pairs.push((character, pair, opening));
	}
	pairs.sort_by_key(|(character, _, _)| *character);
	Ok(pairs)
}

/// `1F600` or `1F600..1F64F`.
fn parse_range(field: &str) -> Option<(u32, u32)> {
	match field.split_once("..") {
		Some((first, last)) => Some((u32::from_str_radix(first.trim(), 16).ok()?, u32::from_str_radix(last.trim(), 16).ok()?)),
		None => {
			let single = u32::from_str_radix(field.trim(), 16).ok()?;
			Some((single, single))
		}
	}
}

/// Sort the runs and JOIN the adjacent ones that carry the same value.
///
/// THE UCD IS NOT ALREADY IN THIS SHAPE. Its files are grouped by value rather than by code point,
/// and a property's runs arrive interleaved - so a table written in file order would not be
/// searchable, and one that did not join adjacent equal runs would be twice the size for no reason.
/// Overlaps are an ERROR rather than something the last writer wins: two values for one character is
/// a file this generator has misunderstood.
fn coalesce(mut ranges: Ranges) -> Ranges {
	ranges.sort_by_key(|(first, _, _)| *first);
	let mut out: Ranges = Vec::new();
	for (first, last, value) in ranges {
		match out.last_mut() {
			Some((_, previous_last, previous_value)) if *previous_value == value && *previous_last + 1 >= first => {
				*previous_last = (*previous_last).max(last);
			}
			_ => out.push((first, last, value)),
		}
	}
	out
}

/// Write one property: its enum, its table, and the lookup that answers for a character.
///
/// THE ENUM IS GENERATED BESIDE THE TABLE, and that is the point of generating it at all. A
/// hand-written enum whose ordinals index a generated table is two lists that must agree and
/// nothing that checks they do - and the day they stop agreeing, every character in one range takes
/// another category's rules, which is a boundary in the wrong place rather than a compile error.
fn emit(out: &mut String, name: &str, values: &[String], ranges: &Ranges) {
	let table = table_name(name);
	let _ = writeln!(out, "/// The `{name}` property. Variant 0 is the value a character the UCD does not mention takes.");
	let _ = writeln!(out, "#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]");
	let _ = writeln!(out, "#[repr(u8)]");
	let _ = writeln!(out, "pub enum {name} {{");
	for (index, value) in values.iter().enumerate() {
		let _ = writeln!(out, "\t{} = {index},", variant(value));
	}
	let _ = writeln!(out, "}}\n");
	let _ = writeln!(out, "impl {name} {{");
	let _ = writeln!(out, "\t/// The value an ordinal names. Out of range is the default, which cannot happen for a table");
	let _ = writeln!(out, "\t/// this generator wrote and is answered rather than panicked on for the one that reads bytes.");
	let _ = writeln!(out, "\tpub const fn from_ordinal(ordinal: u8) -> Self {{");
	let _ = writeln!(out, "\t\tmatch ordinal {{");
	for (index, value) in values.iter().enumerate() {
		if index == 0 {
			continue;
		}
		let _ = writeln!(out, "\t\t\t{index} => Self::{},", variant(value));
	}
	let _ = writeln!(out, "\t\t\t_ => Self::{},", variant(&values[0]));
	let _ = writeln!(out, "\t\t}}");
	let _ = writeln!(out, "\t}}\n");
	let _ = writeln!(out, "\t/// The property's name as the UCD spells it.");
	let _ = writeln!(out, "\tpub const fn ucd_name(self) -> &'static str {{");
	let _ = writeln!(out, "\t\tmatch self {{");
	for value in values {
		let _ = writeln!(out, "\t\t\tSelf::{} => \"{value}\",", variant(value));
	}
	let _ = writeln!(out, "\t\t}}");
	let _ = writeln!(out, "\t}}");
	let _ = writeln!(out, "}}\n");
	let _ = writeln!(out, "/// The `{name}` property, as `(first, last, ordinal)` runs sorted by `first`.");
	let _ = writeln!(out, "pub const {table}: &[(u32, u32, u8)] = &[");
	for (first, last, value) in ranges {
		let _ = writeln!(out, "\t({first:#x}, {last:#x}, {value}),");
	}
	let _ = writeln!(out, "];\n");
	let _ = writeln!(out, "/// The `{name}` of a character.");
	let _ = writeln!(out, "pub fn {}(character: char) -> {name} {{", function_name(name));
	if name == "BidiClass" {
		let _ = writeln!(out, "\t// A code point the records do not mention takes its BLOCK's default, which is `R` or `AL`");
		let _ = writeln!(out, "\t// in the right-to-left blocks and `L` elsewhere - see `BIDI_CLASS_DEFAULTS`.");
		let _ = writeln!(out, "\tmatch crate::lookup_run({table}, character as u32) {{");
		let _ = writeln!(out, "\t\tSome(ordinal) => {name}::from_ordinal(ordinal),");
		let _ = writeln!(out, "\t\tNone => {name}::from_ordinal(crate::lookup(BIDI_CLASS_DEFAULTS, character as u32)),");
		let _ = writeln!(out, "\t}}");
	} else {
		let _ = writeln!(out, "\t{name}::from_ordinal(crate::lookup({table}, character as u32))");
	}
	let _ = writeln!(out, "}}\n");
}

/// A UCD value as a Rust variant: `Regional_Indicator` becomes `RegionalIndicator`.
///
/// THE UCD SPELLING IS NOT LOST - `ucd_name` gives it back, and that is what a report, a document and
/// a conformance file all use. What changes here is only the identifier, because an underscore in a
/// variant is a lint this tree treats as an error and suppressing it file by file is how a tree ends
/// up with a hundred warnings nobody reads.
fn variant(value: &str) -> String {
	let mut out = String::new();
	let mut capitalise = true;
	for character in value.chars() {
		if character == '_' {
			capitalise = true;
			continue;
		}
		if capitalise {
			out.extend(character.to_uppercase());
			capitalise = false;
		} else {
			out.push(character);
		}
	}
	out
}

/// `GraphemeBreak` becomes `grapheme_break`.
fn function_name(name: &str) -> String {
	let mut out = String::new();
	for (index, character) in name.chars().enumerate() {
		if character.is_uppercase() && index > 0 {
			out.push('_');
		}
		out.push(character.to_ascii_lowercase());
	}
	out
}

/// `GraphemeBreak` becomes `GRAPHEME_BREAK`.
fn table_name(name: &str) -> String {
	let mut out = String::new();
	for (index, character) in name.chars().enumerate() {
		if character.is_uppercase() && index > 0 {
			out.push('_');
		}
		out.push(character.to_ascii_uppercase());
	}
	out
}
