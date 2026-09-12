//! The normative Unicode conformance files, run IN FULL against this tree's segmentation.
//!
//! NOT A REPRESENTATIVE SAMPLE. Unicode publishes the answers - every case, including the ones an
//! implementer would not think to write - and a sample is exactly the set of cases somebody already
//! believed they handled. The files are pinned by SHA-256 in `toolchain.lock`, so what is run
//! against is a fixed artifact rather than whatever a mirror serves.
//!
//! IT READS THE CACHE AND NEVER THE NETWORK. A gate that can fetch is a gate whose answer depends on
//! today's mirror; this one names `./bootstrap.sh` when the cache is absent and fails.
//!
//! WHAT A FAILURE PRINTS. The line number, the code points, and the two boundary sets - expected and
//! produced - because "1273 of 9836 passed" is a number nobody can act on.

use std::path::{Path, PathBuf};

mod bidi;

use unicode_segmentation::{LineBreakOpportunity, grapheme_boundaries, line_break_opportunities, word_boundaries};

/// How many failures are printed before the rest are counted. A broken rule fails thousands of
/// cases, and a terminal full of them says less than the first few and a total.
const SHOWN: usize = 12;

fn main() -> std::process::ExitCode {
	let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
	let root = root.canonicalize().unwrap_or(root);
	let cache = root.join(format!(".build/ucd/{}", unicode_tables::UNICODE_VERSION));
	if !cache.is_dir() {
		eprintln!("unicode-conformance: the pinned UCD is not cached at {}", cache.display());
		eprintln!("unicode-conformance: fetch it with `./bootstrap.sh` - this gate never reaches the network itself");
		return std::process::ExitCode::FAILURE;
	}
	println!("unicode-conformance: Unicode {}", unicode_tables::UNICODE_VERSION);
	let mut ok = true;
	ok &= run(&cache.join("GraphemeBreakTest.txt"), Kind::Grapheme);
	ok &= run(&cache.join("WordBreakTest.txt"), Kind::Word);
	ok &= run(&cache.join("LineBreakTest.txt"), Kind::Line);
	ok &= bidi::run_class_file(&cache.join("BidiTest.txt"));
	ok &= bidi::run_character_file(&cache.join("BidiCharacterTest.txt"));
	if ok { std::process::ExitCode::SUCCESS } else { std::process::ExitCode::FAILURE }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
	Grapheme,
	Word,
	Line,
}

impl Kind {
	fn name(self) -> &'static str {
		match self {
			Self::Grapheme => "grapheme cluster",
			Self::Word => "word",
			Self::Line => "line break",
		}
	}
}

fn run(path: &PathBuf, kind: Kind) -> bool {
	let text = match std::fs::read_to_string(path) {
		Ok(text) => text,
		Err(error) => {
			eprintln!("unicode-conformance: cannot read {}: {error}", path.display());
			return false;
		}
	};
	let mut total = 0usize;
	let mut failures = 0usize;
	for (number, line) in text.lines().enumerate() {
		let Some(case) = parse(line) else { continue };
		total += 1;
		let produced = boundaries(&case.text, kind);
		if produced != case.expected {
			failures += 1;
			if failures <= SHOWN {
				let points: Vec<String> = case.text.chars().map(|character| format!("{:04X}", character as u32)).collect();
				eprintln!("unicode-conformance: {} line {}: {}", kind.name(), number + 1, points.join(" "));
				eprintln!("unicode-conformance:   expected breaks at {:?}", case.expected);
				eprintln!("unicode-conformance:   produced breaks at {produced:?}");
			}
		}
	}
	if failures == 0 {
		println!("unicode-conformance: {} - {total} cases, all of them", kind.name());
		return true;
	}
	eprintln!("unicode-conformance: {} - {failures} of {total} cases FAILED", kind.name());
	false
}

/// One case: the text, and the byte offsets a break is expected at.
struct Case {
	text: String,
	expected: Vec<usize>,
}

/// A conformance line: `÷ 0020 ÷ 0308 × 0020 ÷ # comment`.
///
/// THE START OF TEXT IS A CASE OF ITS OWN. The grapheme and word files mark it with a leading `÷`
/// and the line break file marks it with a leading `×`, because LB2 says a line never begins by
/// breaking - so the leading marker is READ rather than assumed, and a file that changed its mind
/// about it would fail loudly instead of quietly passing.
fn parse(line: &str) -> Option<Case> {
	let line = line.split('#').next().unwrap_or("").trim();
	if line.is_empty() {
		return None;
	}
	let mut text = String::new();
	let mut expected = Vec::new();
	for token in line.split_whitespace() {
		match token {
			"÷" => expected.push(text.len()),
			"×" => {}
			hex => {
				let point = u32::from_str_radix(hex, 16).ok()?;
				text.push(char::from_u32(point)?);
			}
		}
	}
	Some(Case { text, expected })
}

/// The boundaries this tree produces for one case, in the same shape the file states them.
fn boundaries(text: &str, kind: Kind) -> Vec<usize> {
	match kind {
		Kind::Grapheme => {
			let mut into = vec![0usize; text.len() + 2];
			let count = grapheme_boundaries(text, &mut into);
			into[..count].to_vec()
		}
		Kind::Word => {
			let mut into = vec![0usize; text.len() + 2];
			let count = word_boundaries(text, &mut into);
			into[..count].to_vec()
		}
		Kind::Line => {
			let mut into = vec![(0usize, LineBreakOpportunity::Prohibited); text.len() + 2];
			let count = line_break_opportunities(text, &mut into);
			into[..count].iter().map(|(offset, _)| *offset).collect()
		}
	}
}
