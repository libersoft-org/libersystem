//! What claims to implement or to test a profile feature, found by reading the tree.
//!
//! THE CLAIM IS A MARKER IN THE SOURCE, not a registry beside it. `// @handles: FillNonZero` sits on
//! the code that does the thing and `// @covers: FillNonZero` on the test that measures it, so a
//! handler that is deleted takes its claim with it. A separate list would go on claiming coverage
//! for code that no longer exists, which is the failure the gate is for.
//!
//! ONE NAME PER CLAIM, COMMA-SEPARATED FOR SEVERAL, AND NOTHING ELSE ON THE LINE. The names are the
//! profile's own spelling, and a name the profile does not have is an error rather than an ignored
//! line - a test that claims `FillZigzag` is either a typo or a test measuring an extension while
//! reporting Profile 1.
//!
//! WHAT "NOTHING ELSE ON THE LINE" IS FOR, and it is not tidiness. A marker followed by anything
//! that is not a list of identifiers is PROSE - this paragraph is full of it, and so is every file
//! that documents the convention. Reading those as claims made the generator report its own
//! sentences as features, so a line whose remainder is not a comma-separated list of identifiers is
//! skipped rather than half-parsed.

use std::path::{Path, PathBuf};

/// One claim: what was claimed, by which kind of marker, and where.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Claim {
	pub feature: String,
	pub file: String,
	pub line: usize,
}

/// The two markers, and what each one is a claim about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Marker {
	/// A backend implements this feature.
	Handles,
	/// A conformance test measures this feature.
	Covers,
}

impl Marker {
	fn token(self) -> &'static str {
		match self {
			Self::Handles => "@handles:",
			Self::Covers => "@covers:",
		}
	}
}

/// Every claim of one kind under `root`, sorted, so a generated document is a function of the tree
/// and not of the order a directory happened to be read in.
pub fn claims(root: &Path, marker: Marker) -> Result<Vec<Claim>, String> {
	let mut files = Vec::new();
	collect(root, &mut files)?;
	files.sort();
	let mut found = Vec::new();
	for file in files {
		let text = match std::fs::read_to_string(&file) {
			Ok(text) => text,
			// NOT AN ERROR. A source tree carries fixtures that are deliberately not UTF-8, and a
			// gate that refused to run because of one would be a gate nobody could keep green.
			Err(_) => continue,
		};
		if !text.contains(marker.token()) {
			continue;
		}
		let shown = file.strip_prefix(root).unwrap_or(&file).to_string_lossy().into_owned();
		for (index, line) in text.lines().enumerate() {
			let Some(rest) = line.split_once(marker.token()) else {
				continue;
			};
			let names: Vec<&str> = rest.1.split(',').map(str::trim).collect();
			if names.is_empty() || !names.iter().all(|name| is_identifier(name)) {
				continue;
			}
			for name in names {
				found.push(Claim { feature: name.to_owned(), file: shown.clone(), line: index + 1 });
			}
		}
	}
	found.sort();
	Ok(found)
}

/// A feature name as the profile spells it: a Rust identifier and nothing else.
fn is_identifier(text: &str) -> bool {
	let mut characters = text.chars();
	matches!(characters.next(), Some(first) if first.is_ascii_alphabetic() || first == '_') && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn collect(directory: &Path, into: &mut Vec<PathBuf>) -> Result<(), String> {
	let entries = std::fs::read_dir(directory).map_err(|error| format!("{}: {error}", directory.display()))?;
	for entry in entries {
		let entry = entry.map_err(|error| format!("{}: {error}", directory.display()))?;
		let path = entry.path();
		let name = entry.file_name().to_string_lossy().into_owned();
		if path.is_dir() {
			// BUILD OUTPUT IS NOT SOURCE. `target` holds copies of this tree's own sources, and a
			// scan that read them would report every claim twice and name a path nobody can edit.
			if name == "target" || name == ".git" || name == "node_modules" {
				continue;
			}
			collect(&path, into)?;
			continue;
		}
		if name.ends_with(".rs") || name.ends_with(".c") || name.ends_with(".h") {
			into.push(path);
		}
	}
	Ok(())
}
