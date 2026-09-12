//! The scanner proves it REFUSES before it is trusted to APPROVE.
//!
//! WHY THIS EXISTS TODAY OF ALL DAYS. Nothing in the tree claims a feature yet, so both coverage
//! checks report NOT PERFORMED and a clean run proves nothing about a scan that has stopped
//! matching. This builds a small tree with the four cases that matter - a real claim, a sentence
//! that only mentions the marker, a name the profile does not have, and a handler for a feature no
//! backend owns - and asserts the scan and the checks answer each one.

use std::path::Path;

use graphics_profile::capability::{Coverage, Range};
use graphics_profile::{FeatureOwner, RENDER2D_CORE_PROFILE_1};

use crate::scan::{self, Marker};

/// The tree the self-test reads, written as `(relative path, contents)`.
const FIXTURES: &[(&str, &str)] = &[
	// A REAL CLAIM, in the shape the convention asks for.
	("backend/src/fill.rs", "// @handles: FillNonZero, FillEvenOdd\nfn fill() {}\n"),
	// PROSE THAT MENTIONS THE MARKER. Every file documenting the convention looks like this, and
	// reading these as claims is the failure that made this self-test necessary.
	("docs/src/convention.rs", "//! A backend writes `@handles: <feature>` on the code that does it.\n//! A test writes `@covers: FillNonZero` on the test that measures it, so a deletion\n"),
	// A NAME THE PROFILE DOES NOT HAVE: a typo, or an extension reporting Profile 1 coverage.
	("suite/src/extension.rs", "// @covers: FillZigzag\nfn measured() {}\n"),
	// A HANDLER FOR A FEATURE NO BACKEND OWNS - the stub the owner field exists to prevent.
	("backend/src/query.rs", "// @handles: QueryPathLength\nfn length() {}\n"),
];

/// Run the self-test, printing one line per case. Returns false if any case answered wrongly.
pub fn run(directory: &Path) -> bool {
	if let Err(error) = write_fixtures(directory) {
		eprintln!("profile-doc: self-test could not write its tree: {error}");
		return false;
	}
	let handled = match scan::claims(directory, Marker::Handles) {
		Ok(claims) => claims,
		Err(error) => {
			eprintln!("profile-doc: self-test scan failed: {error}");
			return false;
		}
	};
	let covered = match scan::claims(directory, Marker::Covers) {
		Ok(claims) => claims,
		Err(error) => {
			eprintln!("profile-doc: self-test scan failed: {error}");
			return false;
		}
	};

	let mut ok = true;
	let handled_names: Vec<&str> = handled.iter().map(|claim| claim.feature.as_str()).collect();
	let covered_names: Vec<&str> = covered.iter().map(|claim| claim.feature.as_str()).collect();

	ok &= expect("a claim is found where it is written", handled_names.contains(&"FillNonZero") && handled_names.contains(&"FillEvenOdd"));
	ok &= expect("a claim names its file and line", handled.iter().any(|claim| claim.feature == "FillNonZero" && claim.file.ends_with("fill.rs") && claim.line == 1));
	// THE PROSE FILE CLAIMS NOTHING. It mentions both markers twice between them.
	ok &= expect("a sentence that mentions the marker is not a claim", !handled.iter().chain(covered.iter()).any(|claim| claim.file.contains("convention")));

	let handlers = Coverage::new(RENDER2D_CORE_PROFILE_1, &handled_names, Range::OwnedBy(FeatureOwner::Backend));
	let coverage = Coverage::new(RENDER2D_CORE_PROFILE_1, &covered_names, Range::EveryFeature);
	ok &= expect("a name the profile does not have is refused", coverage.outside_profile().eq(["FillZigzag"]));
	ok &= expect("a handler for a feature no backend owns is refused", handlers.outside_range().eq(["QueryPathLength"]));
	// AND THE MISSING HALF IS STILL MISSING. A scan that found two handlers and called the profile
	// complete would be the failure this whole gate is against.
	ok &= expect("what is not claimed is reported missing", handlers.missing().any(|entry| entry.name == "StrokeWidth") && !handlers.complete());
	ok
}

fn expect(what: &str, held: bool) -> bool {
	if held {
		println!("profile-doc: self-test ok - {what}");
	} else {
		eprintln!("profile-doc: SELF-TEST FAILED - {what}");
	}
	held
}

fn write_fixtures(directory: &Path) -> std::io::Result<()> {
	for (path, contents) in FIXTURES {
		let path = directory.join(path);
		if let Some(parent) = path.parent() {
			std::fs::create_dir_all(parent)?;
		}
		std::fs::write(path, contents)?;
	}
	Ok(())
}
