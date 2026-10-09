//! The scanner proves it REFUSES before it is trusted to APPROVE.
//!
//! A generated matrix must refuse missing handlers, missing tests and invalid claims. The small
//! source tree below also distinguishes real markers from prose mentioning their convention.

use std::path::Path;

use crate::scan::Claim;
use graphics_profile::capability::{Coverage, Range};
use graphics_profile::{FeatureOwner, ProfileEntry, RENDER2D_CORE_PROFILE_1};

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
	// Exercise the actual gate, including deletion of the entire handler or test set.
	static REQUIRED: &[ProfileEntry<()>] = &[
		ProfileEntry { feature: (), group: "fixture", owner: FeatureOwner::Backend, name: "FillNonZero" },
		ProfileEntry { feature: (), group: "fixture", owner: FeatureOwner::Render2D, name: "QueryPathLength" },
	];
	let claim = |feature: &str| Claim { feature: feature.to_owned(), file: "fixture.rs".to_owned(), line: 1 };
	let real_handler = [claim("FillNonZero")];
	let real_tests = [claim("FillNonZero"), claim("QueryPathLength")];
	ok &= expect("complete actual handler and test sets pass", crate::checks("self-test", REQUIRED, &real_handler, &real_tests));
	ok &= expect("deleting every handler fails", !crate::checks("self-test", REQUIRED, &[], &real_tests));
	ok &= expect("deleting every conformance claim fails", !crate::checks("self-test", REQUIRED, &real_handler, &[]));
	ok &= expect("deleting one conformance claim fails", !crate::checks("self-test", REQUIRED, &real_handler, &real_tests[..1]));
	ok &= expect("a non-backend handler fails the actual gate", !crate::checks("self-test", REQUIRED, &real_tests, &real_tests));
	ok &= expect("an unknown conformance claim fails the actual gate", !crate::checks("self-test", REQUIRED, &real_handler, &[claim("FillZigzag")]));

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
