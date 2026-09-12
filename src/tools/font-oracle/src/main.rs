//! THE CATALOGUE'S TRUTH ORACLE: every staged face is parsed, and what is recovered must EQUAL what
//! was declared.
//!
//! THE CATALOGUE PUBLISHES DECLARED METADATA AND NEVER DERIVES IT. That is deliberate: deriving it
//! would put a second, unprofiled font parser inside a service, and the whole point of the closed
//! profile is that exactly one component reads a font. So the declaration beside each face is
//! trusted, and a declaration nobody checks is a claim rather than a fact. This is the check: the ONE
//! component allowed to read a font parses every staged face and requires the family, the style, the
//! format, the face index, the weight, the width, the slope and the axes it recovers to agree with
//! the record, with a disagreement naming the face AND the field.
//!
//! A STYLE IS A NUMBER AND A SET OF BITS, NOT A WORD, which is why the weight, the width and the
//! slope are compared against `OS/2` rather than against the style STRING. "Bold" is a word in a
//! language; 700 is not.
//!
//! IT HAS NOTHING TO CHECK UNTIL A FACE IS STAGED, and that is exactly why it proves it REFUSES. A
//! validator exercised only against a valid input is not exercised, and one exercised against NO
//! input could pass over an empty set for months while its comparison quietly stopped comparing. The
//! self-test builds a face, agrees with it, and is then required to disagree with the same face under
//! each field made wrong in turn.

mod build;

use std::path::{Path, PathBuf};

use font_parse::Face;
use font_parse::metadata::{Names, Os2, name_id, selection};
use service_logic::font_record::{FaceFormat, FaceRecord, FaceSlant, FaceWidth};

fn main() -> std::process::ExitCode {
	let Some(root) = repository_root() else {
		eprintln!("font-oracle: could not find the repository root");
		return std::process::ExitCode::FAILURE;
	};
	if std::env::args().any(|argument| argument == "--self-test") {
		return match prove_it_refuses() {
			Ok(cases) => {
				println!("font-oracle: self-test passed - a matching declaration is accepted and {cases} altered ones are each refused by field");
				std::process::ExitCode::SUCCESS
			}
			Err(reason) => {
				eprintln!("font-oracle: SELF-TEST FAILED - {reason}");
				std::process::ExitCode::FAILURE
			}
		};
	}
	let staged = root.join("src/volume/share/fonts");
	match check(&staged) {
		Ok(0) => {
			// NOT PERFORMED RATHER THAN A PASS. A gate that printed "ok" over an empty set would be
			// reporting the absence of the thing it exists to check as success.
			println!("font-oracle: NOT PERFORMED - no face is staged at src/volume/share/fonts, so nothing was checked");
			std::process::ExitCode::SUCCESS
		}
		Ok(count) => {
			println!("font-oracle: {count} staged face(s) agree with their declarations");
			std::process::ExitCode::SUCCESS
		}
		Err(problems) => {
			for problem in problems {
				eprintln!("font-oracle: {problem}");
			}
			std::process::ExitCode::FAILURE
		}
	}
}

/// Check every declaration in a directory, answering how many faces were checked.
fn check(staged: &Path) -> Result<usize, Vec<String>> {
	let Ok(entries) = std::fs::read_dir(staged) else {
		return Ok(0);
	};
	let mut problems = Vec::new();
	let mut checked = 0usize;
	let mut paths: Vec<PathBuf> = entries.filter_map(|entry| entry.ok()).map(|entry| entry.path()).filter(|path| path.extension().is_some_and(|extension| extension == "face")).collect();
	// SORTED, so two machines report the same faces in the same order - a gate whose output depends
	// on a directory's iteration order is a gate whose failures are hard to compare.
	paths.sort();
	for declaration in paths {
		let named = declaration.file_name().and_then(|name| name.to_str()).unwrap_or("?").to_string();
		// THE DECLARATION MUST NAME A FACE. `sans.ttf.face` sits beside `sans.ttf`; stripping the
		// suffix is what makes the sidecar convention checkable rather than a habit.
		let face_path = declaration.with_extension("");
		let (Ok(text), Ok(bytes)) = (std::fs::read_to_string(&declaration), std::fs::read(&face_path)) else {
			problems.push(format!("{named}: the declaration or the face beside it could not be read"));
			continue;
		};
		let digest = digest(&bytes);
		let record = match service_logic::font_record::parse(&text, &digest) {
			Ok(record) => record,
			Err(error) => {
				problems.push(format!("{named}: the declaration itself is refused: {error:?}"));
				continue;
			}
		};
		let face = match Face::open(&bytes, record.face_index) {
			Ok(face) => face,
			Err(error) => {
				problems.push(format!("{named}: the face does not open at the declared index {}: {error:?}", record.face_index));
				continue;
			}
		};
		for disagreement in compare(&face, &bytes, &record) {
			problems.push(format!("{named}: {disagreement}"));
		}
		checked += 1;
	}
	if problems.is_empty() { Ok(checked) } else { Err(problems) }
}

/// What the face says against what the record says, field by field.
///
/// EVERY DISAGREEMENT IS REPORTED AND NOT THE FIRST. Whoever staged the face has to fix all of them,
/// and a gate that stopped at the first makes that as many rounds as there are wrong fields.
fn compare(face: &Face<'_>, bytes: &[u8], record: &FaceRecord) -> Vec<String> {
	let mut disagreements = Vec::new();

	// THE FAMILY AND THE STYLE, preferring the TYPOGRAPHIC names: a family with more than four styles
	// states its real family there, and the legacy pair splits it into groups of four so that a
	// twenty-year-old menu could show it.
	let names = match Names::of(face) {
		Ok(names) => names,
		Err(error) => {
			disagreements.push(format!("the `name` table could not be read: {error:?}"));
			None
		}
	};
	if let Some(names) = names {
		for (field, typographic, legacy, declared) in [
			("family", name_id::TYPOGRAPHIC_FAMILY, name_id::FAMILY, record.family.as_str()),
			("style", name_id::TYPOGRAPHIC_SUBFAMILY, name_id::SUBFAMILY, record.style.as_str()),
		] {
			let found = names.get(typographic).ok().flatten().or_else(|| names.get(legacy).ok().flatten());
			match found {
				Some(name) if name.equals(declared) => {}
				Some(_) => disagreements.push(format!("the declared {field} `{declared}` is not the one the face states")),
				None => disagreements.push(format!("the declared {field} `{declared}` has no counterpart in the face's `name` table")),
			}
		}
	}

	// THE STYLE AS NUMBERS. "Bold" is a word in a language and 700 is not.
	match Os2::of(face) {
		Ok(Some(os2)) => {
			if os2.weight_class != record.weight {
				disagreements.push(format!("the declared weight {} is not the {} the face states", record.weight, os2.weight_class));
			}
			let width = width_of(os2.width_class);
			if width != Some(record.width) {
				disagreements.push(format!("the declared width {:?} is not the width class {} the face states", record.width, os2.width_class));
			}
			let slant = if os2.fs_selection & selection::OBLIQUE != 0 {
				FaceSlant::Oblique
			} else if os2.fs_selection & selection::ITALIC != 0 {
				FaceSlant::Italic
			} else {
				FaceSlant::Upright
			};
			if slant != record.slant {
				disagreements.push(format!("the declared slant {:?} is not the {slant:?} the face's selection bits state", record.slant));
			}
		}
		Ok(None) => disagreements.push(String::from("the face carries no `OS/2`, so its weight, width and slope cannot be checked against the declaration")),
		Err(error) => disagreements.push(format!("the `OS/2` table could not be read: {error:?}")),
	}

	// THE FORMAT, from the outlines the face actually has and from the file's own signature.
	let collection = bytes.starts_with(b"ttcf");
	let format = match font_parse::outline::format(face) {
		Ok(Some(font_parse::outline::Format::Quadratic)) => Some(FaceFormat::TruetypeGlyf),
		Ok(Some(font_parse::outline::Format::Cubic)) => match face.table_unchecked(b"CFF2") {
			Ok(Some(_)) => Some(FaceFormat::OpentypeCff2),
			_ => Some(FaceFormat::OpentypeCff),
		},
		Ok(None) => None,
		Err(error) => {
			disagreements.push(format!("the face's outline format could not be read: {error:?}"));
			None
		}
	};
	// A COLLECTION IS A PROPERTY OF THE FILE AND NOT OF THE FACE. Every face inside one has its own
	// outline format, so the declaration's `collection` is about the file it is beside.
	let expected = if collection { Some(FaceFormat::Collection) } else { format };
	match expected {
		Some(expected) if expected == record.format => {}
		Some(expected) => disagreements.push(format!("the declared format {:?} is not the {expected:?} the face has", record.format)),
		None => disagreements.push(format!("the declared format {:?} cannot be checked: the face has no outlines at all", record.format)),
	}
	// AND A NON-ZERO FACE INDEX ONLY MEANS SOMETHING IN A COLLECTION.
	if record.face_index != 0 && !collection {
		disagreements.push(format!("the declared face index {} names a face inside a collection, and this file is not one", record.face_index));
	}

	// THE AXES, tag by tag and range by range. A declaration that named the axes and got their ranges
	// wrong would let a caller ask for an instance outside the design space.
	match font_parse::Variations::of(face) {
		Ok(Some(variations)) => {
			if variations.axis_count != record.axes.len() {
				disagreements.push(format!("the declaration states {} axes and the face has {}", record.axes.len(), variations.axis_count));
			}
			for (index, declared) in record.axes.iter().enumerate() {
				let Ok(axis) = variations.axis(index) else {
					disagreements.push(format!("axis {index} is declared and the face's `fvar` does not hold it"));
					continue;
				};
				let tag = u32::from_be_bytes(axis.tag);
				if tag != declared.tag {
					disagreements.push(format!("axis {index} is declared as `{}` and the face states `{}`", tag_text(declared.tag), tag_text(tag)));
					continue;
				}
				// THE DECLARATION STATES WHOLE DESIGN UNITS AND `fvar` STATES 16.16, so the comparison
				// is exact in the FACE's units rather than truncated into the declaration's: an axis
				// whose range has a fractional end cannot be expressed by the declaration at all, and
				// a comparison that truncated would call it equal to the whole number below it.
				let stated = (declared.minimum << 16, declared.default << 16, declared.maximum << 16);
				if (axis.minimum, axis.default, axis.maximum) != stated {
					disagreements.push(format!("axis `{}` is declared as {}..{}..{} and the face states {}..{}..{}", tag_text(declared.tag), declared.minimum, declared.default, declared.maximum, axis.minimum >> 16, axis.default >> 16, axis.maximum >> 16));
				}
			}
		}
		Ok(None) => {
			if !record.axes.is_empty() {
				disagreements.push(format!("the declaration states {} axes and the face is not variable", record.axes.len()));
			}
		}
		Err(error) => disagreements.push(format!("the face's `fvar` could not be read: {error:?}")),
	}
	disagreements
}

/// The nine width values, from the class `OS/2` states. A class outside 1 to 9 is `None` rather than
/// clamped: a face declaring a tenth width is saying something the vocabulary cannot hold.
fn width_of(class: u16) -> Option<FaceWidth> {
	Some(match class {
		1 => FaceWidth::UltraCondensed,
		2 => FaceWidth::ExtraCondensed,
		3 => FaceWidth::Condensed,
		4 => FaceWidth::SemiCondensed,
		5 => FaceWidth::Normal,
		6 => FaceWidth::SemiExpanded,
		7 => FaceWidth::Expanded,
		8 => FaceWidth::ExtraExpanded,
		9 => FaceWidth::UltraExpanded,
		_ => return None,
	})
}

fn tag_text(tag: u32) -> String {
	String::from_utf8_lossy(&tag.to_be_bytes()).into_owned()
}

/// The face file's digest, which the declaration states and the catalogue checks.
///
/// SHA-256, WRITTEN OUT, because this tool has no business pulling a hash crate in for one use and
/// because the digest is compared rather than trusted.
fn digest(bytes: &[u8]) -> [u8; 32] {
	const K: [u32; 64] = [
		0x428a2f98,
		0x71374491,
		0xb5c0fbcf,
		0xe9b5dba5,
		0x3956c25b,
		0x59f111f1,
		0x923f82a4,
		0xab1c5ed5,
		0xd807aa98,
		0x12835b01,
		0x243185be,
		0x550c7dc3,
		0x72be5d74,
		0x80deb1fe,
		0x9bdc06a7,
		0xc19bf174,
		0xe49b69c1,
		0xefbe4786,
		0x0fc19dc6,
		0x240ca1cc,
		0x2de92c6f,
		0x4a7484aa,
		0x5cb0a9dc,
		0x76f988da,
		0x983e5152,
		0xa831c66d,
		0xb00327c8,
		0xbf597fc7,
		0xc6e00bf3,
		0xd5a79147,
		0x06ca6351,
		0x14292967,
		0x27b70a85,
		0x2e1b2138,
		0x4d2c6dfc,
		0x53380d13,
		0x650a7354,
		0x766a0abb,
		0x81c2c92e,
		0x92722c85,
		0xa2bfe8a1,
		0xa81a664b,
		0xc24b8b70,
		0xc76c51a3,
		0xd192e819,
		0xd6990624,
		0xf40e3585,
		0x106aa070,
		0x19a4c116,
		0x1e376c08,
		0x2748774c,
		0x34b0bcb5,
		0x391c0cb3,
		0x4ed8aa4a,
		0x5b9cca4f,
		0x682e6ff3,
		0x748f82ee,
		0x78a5636f,
		0x84c87814,
		0x8cc70208,
		0x90befffa,
		0xa4506ceb,
		0xbef9a3f7,
		0xc67178f2,
	];
	let mut state: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
	let mut message = bytes.to_vec();
	let length = (bytes.len() as u64) * 8;
	message.push(0x80);
	while message.len() % 64 != 56 {
		message.push(0);
	}
	message.extend_from_slice(&length.to_be_bytes());
	for chunk in message.chunks(64) {
		let mut w = [0u32; 64];
		for (index, slot) in w.iter_mut().enumerate().take(16) {
			let at = index * 4;
			*slot = u32::from_be_bytes([chunk[at], chunk[at + 1], chunk[at + 2], chunk[at + 3]]);
		}
		for index in 16..64 {
			let s0 = w[index - 15].rotate_right(7) ^ w[index - 15].rotate_right(18) ^ (w[index - 15] >> 3);
			let s1 = w[index - 2].rotate_right(17) ^ w[index - 2].rotate_right(19) ^ (w[index - 2] >> 10);
			w[index] = w[index - 16].wrapping_add(s0).wrapping_add(w[index - 7]).wrapping_add(s1);
		}
		let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
		for index in 0..64 {
			let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
			let choose = (e & f) ^ ((!e) & g);
			let temp1 = h.wrapping_add(s1).wrapping_add(choose).wrapping_add(K[index]).wrapping_add(w[index]);
			let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
			let majority = (a & b) ^ (a & c) ^ (b & c);
			let temp2 = s0.wrapping_add(majority);
			h = g;
			g = f;
			f = e;
			e = d.wrapping_add(temp1);
			d = c;
			c = b;
			b = a;
			a = temp1.wrapping_add(temp2);
		}
		for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
			*slot = slot.wrapping_add(value);
		}
	}
	let mut out = [0u8; 32];
	for (index, value) in state.iter().enumerate() {
		out[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
	}
	out
}

fn hex(digest: &[u8; 32]) -> String {
	digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Prove the comparison REFUSES before trusting it to approve, over a face this tool builds.
fn prove_it_refuses() -> Result<usize, String> {
	let axes = [(*b"wght", 100i32, 400, 900)];
	let bytes = build::face("Test Family", "Bold Italic", 700, 3, selection::BOLD | selection::ITALIC, &axes);
	let declaration = |family: &str, style: &str, format: &str, index: u32, weight: u16, width: &str, slant: &str, axis: &str| format!("family = {family}\nstyle = {style}\nformat = {format}\nface-index = {index}\nweight = {weight}\nwidth = {width}\nslant = {slant}\n{axis}digest = {}\n", hex(&digest(&bytes)));
	let good = declaration("Test Family", "Bold Italic", "truetype-glyf", 0, 700, "condensed", "italic", "axis = wght 100 400 900\n");

	// THE VALID DIRECTION FIRST, so every refusal below is known to be about what it changed.
	let agree = |text: &str| -> Result<Vec<String>, String> {
		let record = service_logic::font_record::parse(text, &digest(&bytes)).map_err(|error| format!("the fixture's own declaration is refused: {error:?}"))?;
		let face = Face::open(&bytes, record.face_index).map_err(|error| format!("the fixture's own face does not open: {error:?}"))?;
		Ok(compare(&face, &bytes, &record))
	};
	let clean = agree(&good)?;
	if !clean.is_empty() {
		return Err(format!("a face that matches its declaration was reported as disagreeing: {}", clean.join("; ")));
	}

	// AND EACH FIELD MADE WRONG IN TURN. A comparison that stopped comparing one of them would pass
	// every one of these but its own.
	let wrong: [(&str, String); 7] = [
		("family", declaration("Other Family", "Bold Italic", "truetype-glyf", 0, 700, "condensed", "italic", "axis = wght 100 400 900\n")),
		("style", declaration("Test Family", "Regular", "truetype-glyf", 0, 700, "condensed", "italic", "axis = wght 100 400 900\n")),
		("format", declaration("Test Family", "Bold Italic", "opentype-cff", 0, 700, "condensed", "italic", "axis = wght 100 400 900\n")),
		("weight", declaration("Test Family", "Bold Italic", "truetype-glyf", 0, 400, "condensed", "italic", "axis = wght 100 400 900\n")),
		("width", declaration("Test Family", "Bold Italic", "truetype-glyf", 0, 700, "normal", "italic", "axis = wght 100 400 900\n")),
		("slant", declaration("Test Family", "Bold Italic", "truetype-glyf", 0, 700, "condensed", "upright", "axis = wght 100 400 900\n")),
		("axis range", declaration("Test Family", "Bold Italic", "truetype-glyf", 0, 700, "condensed", "italic", "axis = wght 100 500 900\n")),
	];
	for (field, text) in &wrong {
		let disagreements = agree(text)?;
		if disagreements.is_empty() {
			return Err(format!("a declaration whose {field} does not match the face was accepted"));
		}
	}
	// AND A DECLARATION THAT STATES NO AXES AT ALL for a variable face, which is the omission a
	// comparison written as "check what is declared" would miss entirely.
	let none = declaration("Test Family", "Bold Italic", "truetype-glyf", 0, 700, "condensed", "italic", "");
	if agree(&none)?.is_empty() {
		return Err(String::from("a declaration that states no axes for a variable face was accepted"));
	}
	Ok(wrong.len() + 1)
}

/// The repository root, found from this tool's own location rather than from the working directory.
fn repository_root() -> Option<PathBuf> {
	let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	for _ in 0..4 {
		if path.join("check.sh").is_file() {
			return Some(path);
		}
		path = path.parent()?.to_path_buf();
	}
	None
}
