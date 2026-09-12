//! "EVERY NUMERIC LIMIT IS TESTED" IS A CLAIM, AND THIS IS WHAT MAKES IT CHECKABLE.
//!
//! THE MILESTONE ASKS FOR EACH CEILING AT ITS EXACT BOUND AND ONE PAST IT, and that is a sentence
//! nothing enforces. A ceiling tested only past its value could be off by one in either direction and
//! nothing would say so - which for a limit that decides whether a document is refused is exactly the
//! difference between "long" and "too long". A ceiling with no fixture at all is worse: it is a
//! number in a published table that no code has ever been shown to respect.
//!
//! SO THE CLAIM IS A TABLE AND THE TABLE IS CHECKED. Every ceiling the profile publishes has one
//! entry here, naming either the two fixtures that exercise it or the reason it has no enforcement
//! site yet. A ceiling in the profile with no entry is refused; an entry naming a ceiling the profile
//! does not publish is refused; and an entry naming a fixture that is not in the file it says it is
//! in is refused, so a renamed or deleted test is caught rather than silently stopping.
//!
//! A "NO SITE" ENTRY IS NOT AN EXEMPTION. It is a written statement that the code a ceiling bounds
//! does not exist, with the reason, and it is why this tool reports them separately and loudly rather
//! than folding them into a pass. A gate that counted them as covered would be approving the absence
//! of the thing it exists to check.
//!
//! AND IT PROVES IT REFUSES BEFORE IT IS TRUSTED TO APPROVE. A checker exercised only over a
//! currently-valid tree is not a checker; `--self-test` hands it a table with a ceiling missing and a
//! table naming a fixture that does not exist, and requires it to reject both.

use std::path::{Path, PathBuf};

/// How a ceiling is covered.
enum Coverage {
	/// Exercised by fixtures, in a named crate's test file.
	Bound {
		file: &'static str,
		/// The fixture that shows the ceiling ADMITS its own value.
		at: &'static str,
		/// The fixture that shows it REFUSES one past it. Often the same one.
		past: &'static str,
	},
	/// No enforcement site, because the code this ceiling bounds is not written. The reason is
	/// required and is printed.
	NoSite { reason: &'static str },
}

/// HOW MANY CEILINGS MAY HAVE NO ENFORCEMENT SITE, and it is a FLOOR that only ever comes down.
///
/// A "NO SITE" ENTRY IS NOT AN EXEMPTION, it is a statement that the code a ceiling bounds is not
/// written. That is a fair answer and it is also the answer somebody reaches for when a ceiling is in
/// the way - so the number of them is capped, and demoting a ceiling to "no site" fails this gate
/// until the cap is lowered in the same edit. Raising it is a decision with a reviewer; letting it
/// drift is not.
///
/// Measured 2026-09-12: three, and all three bound an ITERATION this tree does not yet perform.
const MAX_NO_SITE: usize = 3;

/// One entry per ceiling the profile publishes. Checked against that list in both directions.
const COVERAGE: &[(&str, Coverage)] = &[
	("font bytes", Coverage::Bound { file: "src/user/libs/text/font-parse/src/tests.rs", at: "a_file_larger_than_the_frozen_ceiling_is_refused_before_anything_is_read", past: "a_file_larger_than_the_frozen_ceiling_is_refused_before_anything_is_read" }),
	("table bytes", Coverage::Bound { file: "src/user/libs/text/font-parse/src/tests.rs", at: "a_table_larger_than_the_frozen_ceiling_is_refused_by_name", past: "a_table_larger_than_the_frozen_ceiling_is_refused_by_name" }),
	("composite depth", Coverage::Bound { file: "src/user/libs/text/font-parse/src/tests.rs", at: "a_composite_is_nested_to_the_frozen_depth_and_no_further", past: "a_composite_is_nested_to_the_frozen_depth_and_no_further" }),
	("composite points", Coverage::Bound { file: "src/user/libs/text/font-parse/src/tests.rs", at: "the_points_a_composite_expands_to_are_counted_across_the_whole_walk", past: "the_points_a_composite_expands_to_are_counted_across_the_whole_walk" }),
	("charstring depth", Coverage::Bound { file: "src/user/libs/text/font-parse/src/tests.rs", at: "a_charstring_past_the_frozen_depth_or_stack_is_refused_by_name", past: "a_charstring_past_the_frozen_depth_or_stack_is_refused_by_name" }),
	("charstring stack", Coverage::Bound { file: "src/user/libs/text/font-parse/src/tests.rs", at: "a_charstring_past_the_frozen_depth_or_stack_is_refused_by_name", past: "a_charstring_past_the_frozen_depth_or_stack_is_refused_by_name" }),
	("paint depth", Coverage::Bound { file: "src/user/libs/text/font-parse/src/tests.rs", at: "a_colour_glyph_is_read_as_the_layers_or_the_graph_the_face_states", past: "a_paint_graph_past_the_frozen_depth_is_refused_by_name" }),
	("paint nodes", Coverage::Bound { file: "src/user/libs/text/font-parse/src/tests.rs", at: "a_colour_glyph_is_read_as_the_layers_or_the_graph_the_face_states", past: "a_paint_graph_past_the_frozen_node_count_is_refused_by_name" }),
	("context depth", Coverage::Bound { file: "src/user/libs/text/font-shape/src/tests.rs", at: "a_contextual_chain_runs_to_the_frozen_depth_and_no_further", past: "a_contextual_chain_runs_to_the_frozen_depth_and_no_further" }),
	("output expansion", Coverage::Bound { file: "src/user/libs/text/font-shape/src/tests.rs", at: "a_run_expanded_past_the_frozen_ratio_is_refused_by_name", past: "a_run_expanded_past_the_frozen_ratio_is_refused_by_name" }),
	("variation axes", Coverage::Bound { file: "src/user/libs/text/font-parse/src/tests.rs", at: "a_face_declaring_more_axes_than_the_profile_freezes_is_refused_by_name", past: "a_face_declaring_more_axes_than_the_profile_freezes_is_refused_by_name" }),
	("variation regions", Coverage::Bound { file: "src/user/libs/text/font-parse/src/tests.rs", at: "an_item_variation_store_holds_regions_to_the_frozen_ceiling_and_no_further", past: "an_item_variation_store_holds_regions_to_the_frozen_ceiling_and_no_further" }),
	("features", Coverage::Bound { file: "src/user/libs/text/font-shape/src/tests.rs", at: "more_features_than_the_profile_freezes_are_refused_rather_than_dropped", past: "more_features_than_the_profile_freezes_are_refused_rather_than_dropped" }),
	("bidi depth", Coverage::Bound { file: "src/user/libs/text/unicode-bidi/src/tests.rs", at: "embeddings_nest_to_the_algorithms_own_depth_and_overflow_past_it", past: "embeddings_nest_to_the_algorithms_own_depth_and_overflow_past_it" }),
	("fallback faces", Coverage::Bound { file: "src/user/libs/text/text-pipeline/src/tests.rs", at: "the_fallback_walk_stops_at_the_frozen_number_of_faces", past: "the_fallback_walk_stops_at_the_frozen_number_of_faces" }),
	("shaping retries", Coverage::NoSite { reason: "nothing re-shapes a run yet; a retry count cannot be exceeded before there are retries" }),
	("line passes", Coverage::NoSite { reason: "the line layout is single-pass by construction; a pass count will first apply to a justification that re-shapes, as Arabic kashida justification does" }),
	("paragraph passes", Coverage::NoSite { reason: "the same, one level up: there is no iterative paragraph fit to bound" }),
	("run input", Coverage::Bound { file: "src/user/libs/text/font-shape/src/tests.rs", at: "a_run_past_the_frozen_input_ceiling_is_refused_before_any_work_is_done", past: "a_run_past_the_frozen_input_ceiling_is_refused_before_any_work_is_done" }),
	("paragraph input", Coverage::Bound { file: "src/user/libs/text/text-pipeline/src/tests.rs", at: "a_paragraph_is_read_to_the_frozen_ceiling_and_no_further", past: "a_paragraph_is_read_to_the_frozen_ceiling_and_no_further" }),
	("run output", Coverage::Bound { file: "src/user/libs/text/font-run/src/tests.rs", at: "a_run_past_the_frozen_output_ceiling_is_refused_by_name", past: "a_run_past_the_frozen_output_ceiling_is_refused_by_name" }),
	("paragraph output", Coverage::Bound { file: "src/user/libs/text/text-pipeline/src/tests.rs", at: "a_paragraph_produces_glyphs_to_the_frozen_ceiling_and_no_further", past: "a_paragraph_past_the_frozen_output_ceiling_is_refused_by_name" }),
];

fn main() -> std::process::ExitCode {
	let root = match repository_root() {
		Some(root) => root,
		None => {
			eprintln!("text-limits: could not find the repository root");
			return std::process::ExitCode::FAILURE;
		}
	};
	let self_test = std::env::args().any(|argument| argument == "--self-test");
	if self_test {
		return match prove_it_refuses(&root) {
			Ok(()) => {
				println!("text-limits: self-test passed - a missing ceiling and a missing fixture are both refused");
				std::process::ExitCode::SUCCESS
			}
			Err(reason) => {
				eprintln!("text-limits: SELF-TEST FAILED - {reason}");
				std::process::ExitCode::FAILURE
			}
		};
	}
	match check(&root, COVERAGE) {
		Ok(report) => {
			println!("text-limits: {} ceiling(s), {} exercised at bound and one past", report.total, report.bound);
			for reason in &report.no_site {
				println!("text-limits: NO SITE - {reason}");
			}
			std::process::ExitCode::SUCCESS
		}
		Err(problems) => {
			for problem in problems {
				eprintln!("text-limits: {problem}");
			}
			std::process::ExitCode::FAILURE
		}
	}
}

struct Report {
	total: usize,
	bound: usize,
	no_site: Vec<String>,
}

/// The check itself, over a coverage table given rather than the constant - which is what lets the
/// self-test hand it a broken one.
fn check(root: &Path, coverage: &[(&str, Coverage)]) -> Result<Report, Vec<String>> {
	let mut problems = Vec::new();
	let mut bound = 0usize;
	let mut no_site = Vec::new();

	// EVERY PUBLISHED CEILING HAS AN ENTRY. A ceiling with no entry is a number in a table that no
	// code has been shown to respect.
	for limit in opentype_profile::limits::LIMITS {
		if !coverage.iter().any(|(name, _)| *name == limit.name) {
			problems.push(format!("the profile publishes the ceiling '{}' and nothing here claims to exercise it", limit.name));
		}
	}
	// AND EVERY ENTRY NAMES A PUBLISHED CEILING, so a renamed limit does not leave a claim behind
	// pointing at nothing.
	for (name, entry) in coverage {
		let Some(limit) = opentype_profile::limits::limit(name) else {
			problems.push(format!("'{name}' is claimed here and the profile publishes no such ceiling"));
			continue;
		};
		match entry {
			Coverage::Bound { file, at, past } => {
				for fixture in [at, past] {
					let path = root.join(file);
					match std::fs::read_to_string(&path) {
						Ok(text) => {
							if !text.contains(&format!("fn {fixture}(")) {
								problems.push(format!("'{}' names the fixture '{fixture}', which is not in {file} - a renamed or deleted test is a ceiling nothing exercises", limit.name));
							}
						}
						Err(error) => problems.push(format!("'{}' names {file}, which could not be read: {error}", limit.name)),
					}
				}
				bound += 1;
			}
			Coverage::NoSite { reason } => {
				if reason.len() < 30 {
					problems.push(format!("'{}' claims no enforcement site and gives no reason worth the name", limit.name));
				}
				no_site.push(format!("{}: {reason}", limit.name));
			}
		}
	}
	if no_site.len() > MAX_NO_SITE {
		problems.push(format!("{} ceiling(s) claim no enforcement site and at most {MAX_NO_SITE} may - a ceiling demoted to \"no site\" is refused here until the cap comes down in the same edit", no_site.len()));
	}
	if problems.is_empty() { Ok(Report { total: coverage.len(), bound, no_site }) } else { Err(problems) }
}

/// Prove the check REFUSES before trusting it to APPROVE.
fn prove_it_refuses(root: &Path) -> Result<(), String> {
	// A table with a ceiling missing.
	let short: Vec<(&str, Coverage)> = COVERAGE.iter().skip(1).map(|(name, entry)| (*name, clone_entry(entry))).collect();
	if check(root, &short).is_ok() {
		return Err("a coverage table with a ceiling missing was accepted".into());
	}
	// A table naming a fixture that does not exist.
	let broken: Vec<(&str, Coverage)> = COVERAGE
		.iter()
		.map(|(name, entry)| match entry {
			Coverage::Bound { file, .. } => (*name, Coverage::Bound { file, at: "a_fixture_that_does_not_exist", past: "a_fixture_that_does_not_exist" }),
			other => (*name, clone_entry(other)),
		})
		.collect();
	if check(root, &broken).is_ok() {
		return Err("a coverage table naming a fixture that does not exist was accepted".into());
	}
	// A table naming a ceiling the profile does not publish.
	let mut invented: Vec<(&str, Coverage)> = COVERAGE.iter().map(|(name, entry)| (*name, clone_entry(entry))).collect();
	invented.push(("a ceiling nobody froze", Coverage::NoSite { reason: "this reason is long enough to pass the length check above" }));
	if check(root, &invented).is_ok() {
		return Err("a coverage table naming a ceiling the profile does not publish was accepted".into());
	}
	// A "no site" entry with no reason.
	let unreasoned: Vec<(&str, Coverage)> = COVERAGE.iter().map(|(name, _)| (*name, Coverage::NoSite { reason: "" })).collect();
	if check(root, &unreasoned).is_ok() {
		return Err("a coverage table whose entries give no reason was accepted".into());
	}
	// AND ONE MORE CEILING DEMOTED TO "no site" than the cap admits, which is the shape the drift
	// this cap exists to stop actually has.
	let mut demoted: Vec<(&str, Coverage)> = COVERAGE.iter().map(|(name, entry)| (*name, clone_entry(entry))).collect();
	let first_bound = demoted.iter().position(|(_, entry)| matches!(entry, Coverage::Bound { .. }));
	if let Some(index) = first_bound {
		demoted[index].1 = Coverage::NoSite { reason: "a reason long enough to pass the length check, and no fixture behind it" };
		if check(root, &demoted).is_ok() {
			return Err("a coverage table with one more ceiling demoted to \"no site\" than the cap admits was accepted".into());
		}
	}
	// And the real table must pass, or every refusal above proves nothing.
	check(root, COVERAGE).map_err(|problems| format!("the real coverage table does not pass: {}", problems.join("; ")))?;
	Ok(())
}

fn clone_entry(entry: &Coverage) -> Coverage {
	match entry {
		Coverage::Bound { file, at, past } => Coverage::Bound { file, at, past },
		Coverage::NoSite { reason } => Coverage::NoSite { reason },
	}
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
