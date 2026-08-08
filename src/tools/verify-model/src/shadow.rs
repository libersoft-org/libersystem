// Shadow validation, and it is DRY by default.
//
// The expensive version runs the scoped selection and then the full suite. It is not needed for
// most cases, because one QEMU boot already serves the whole suite: compute the scoped set S
// without running it, run the full suite once, and compare. Every test in S passing while a test
// outside S fails is proof the selector missed an edge - at HALF the QEMU cost, which against a
// 6104-second riscv64 sweep is not cosmetic.
//
// What this cannot do is validate the execution MECHANISM - `TEST_SELECTION`, test ordering, state
// leaking between selected tests. That needs SHADOW-EXEC, really running S and then the full suite,
// and it is sampled rather than routine.

use crate::history::History;
use crate::plan::PlanItemKey;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
	Passed,
	// The guest aborts on the first failure, so at most one test is ever seen in this state per
	// run. That bounds what one sweep can prove and does not weaken it: one test outside S failing
	// while all of S passed is still a missed edge.
	Failed,
	// Started and never finished, and not the one that failed - the suite ended under it. Reported
	// separately because "did not run" and "ran and failed" are different facts.
	Unfinished,
}

pub struct GuestResults {
	pub outcomes: BTreeMap<String, Outcome>,
	pub total_declared: Option<usize>,
}

// `name...\t[ok]` per test, from `impl Testable for TaggedTest`.
//
// A state machine rather than a line match, because `[ok]` is NOT reliably on the same line as the
// name: a test may print while it runs, and then its own output sits between the two. Matching the
// line was the first thing tried and it marked the last test of a clean 205-test sweep as failed -
// a parser that manufactures a shadow failure on every green run, which is worse than none.
//
// So: a name followed by `...` opens a test, the next `[ok]` closes it as passed, and a test still
// open when another one starts, or when the log ends, did not finish. A test that FAILS panics, so
// the one the log ends under is the one that failed.
pub fn parse_guest_log(text: &str) -> GuestResults {
	let mut outcomes = BTreeMap::new();
	let mut total_declared = None;
	let mut open: Option<String> = None;
	for raw in text.lines() {
		let line = raw.trim_end_matches(['\r', '\n']);
		if let Some(rest) = line.strip_prefix("running ") {
			// `running N tests (M skipped, T total)`, or `running N tests (all tags)`. T is what
			// exists on this target; N is what this run chose.
			total_declared = match rest.split_once("total") {
				Some((head, _)) => head.rsplit(['(', ' ', ',']).find_map(|value| value.parse::<usize>().ok()),
				None => rest.split_whitespace().next().and_then(|value| value.parse::<usize>().ok()),
			};
		}
		// The name comes first, THEN the closing marker, and the order matters: the common case is
		// `name...\t[ok]` on ONE line, so checking for `[ok]` before opening the test closed the
		// PREVIOUS one and left every second test unpaired. That showed up as 130 of 205 parsed and
		// a Void verdict - the guard catching it rather than a clean-looking lie, which is the only
		// reason this was cheap to find.
		if let Some(index) = line.find("...") {
			let name = line[..index].trim();
			if !name.is_empty() && name.chars().all(|character| character.is_ascii_alphanumeric() || character == '_') {
				if let Some(previous) = open.take() {
					outcomes.insert(previous, Outcome::Unfinished);
				}
				open = Some(name.to_string());
			}
		}
		if line.contains("[ok]")
			&& let Some(name) = open.take()
		{
			outcomes.insert(name, Outcome::Passed);
		}
	}
	// Whatever was still open when the log ended is what the suite died under.
	if let Some(last) = open {
		outcomes.insert(last, Outcome::Failed);
	}
	GuestResults { outcomes, total_declared }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Verdict {
	// Everything that failed was inside the scoped set, or nothing failed. No evidence against the
	// selector - which is not the same as evidence for it.
	Consistent,
	// A test OUTSIDE the scoped selection failed while everything inside it passed. That is the
	// shape of a missed edge, and it is a CANDIDATE rather than a finding: this tree has a test on
	// record that failed three times and then passed twice with no change, and charging that to the
	// selector would hold a component in SHADOW forever for a reason that has nothing to do with it.
	CandidateMiss,
	// Something inside the scoped set failed. The selector chose correctly; the code is broken.
	SelectionFailed,
	// The comparison could not be trusted - see the reason.
	Void,
}

#[derive(Clone, Debug, Serialize)]
pub struct Comparison {
	pub verdict: Verdict,
	pub reason: String,
	pub architecture: String,
	pub scoped: usize,
	pub ran: usize,
	pub outside_failures: Vec<String>,
	pub inside_failures: Vec<String>,
	// Failures that have failed before under a different change. A key with a failure history is a
	// flake candidate, and the difference decides whether the selector is charged for it.
	pub previously_failed: Vec<String>,
}

pub fn compare(selected: &[PlanItemKey], results: &GuestResults, architecture: &str, history: &History) -> Comparison {
	let scoped: BTreeSet<&str> = selected.iter().filter(|key| key.architecture == architecture).filter_map(|key| key.check.strip_prefix("kernel.")).collect();
	let mut outside = Vec::new();
	let mut inside = Vec::new();
	let mut previously_failed = Vec::new();
	for (name, outcome) in &results.outcomes {
		if *outcome != Outcome::Failed {
			continue;
		}
		let key = format!("kernel.{name} / {architecture} / test-guest / test");
		if history.get(&key).is_some_and(|record| record.failures > 0) {
			previously_failed.push(key.clone());
		}
		if scoped.contains(name.as_str()) { inside.push(key) } else { outside.push(key) }
	}

	// A full sweep that ran fewer tests than exist did not compare what it claimed to. This is the
	// one way a dry shadow lies quietly: a filtered run looks exactly like a clean one.
	if let Some(total) = results.total_declared
		&& results.outcomes.len() < total
		&& inside.is_empty()
		&& outside.is_empty()
	{
		return Comparison { verdict: Verdict::Void, reason: format!("the log records {} of {total} tests and no failure - a full sweep is what this compares against, so a partial one proves nothing", results.outcomes.len()), architecture: architecture.to_string(), scoped: scoped.len(), ran: results.outcomes.len(), outside_failures: outside, inside_failures: inside, previously_failed };
	}

	let (verdict, reason) = if !inside.is_empty() {
		(Verdict::SelectionFailed, format!("{} selected test(s) failed - the selector chose them, so this is a defect in the code and not in the selection", inside.len()))
	} else if !outside.is_empty() {
		(Verdict::CandidateMiss, format!("{} test(s) OUTSIDE the selection failed while every selected test passed - confirm before believing it", outside.len()))
	} else {
		(Verdict::Consistent, format!("{} tests ran, none failed; the selection of {} is not contradicted", results.outcomes.len(), scoped.len()))
	};
	Comparison { verdict, reason, architecture: architecture.to_string(), scoped: scoped.len(), ran: results.outcomes.len(), outside_failures: outside, inside_failures: inside, previously_failed }
}

// A shadow record is only worth keeping if it says WHAT it compared.
//
// There is no CI: this runs on one development machine where an emulated sweep takes tens of
// minutes to hours, and the tree changes while it does. A comparison across a changed tree compares
// two different systems, so the digests are recorded and a comparison whose digests moved is
// refused rather than filed.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Record {
	pub architecture: String,
	pub verdict: String,
	pub reason: String,
	pub model_hash: String,
	pub source_digest: String,
	pub changed_components: Vec<String>,
	pub outside_failures: Vec<String>,
	pub at: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Log {
	pub schema: u32,
	pub records: Vec<Record>,
}

impl Log {
	pub fn path(repo_root: &Path) -> PathBuf {
		repo_root.join(".build/state/verify-shadow.json")
	}

	pub fn load(repo_root: &Path) -> Self {
		let path = Self::path(repo_root);
		let Ok(text) = fs::read_to_string(&path) else { return Log { schema: 1, records: Vec::new() } };
		serde_json::from_str(&text).unwrap_or(Log { schema: 1, records: Vec::new() })
	}

	pub fn save(&self, repo_root: &Path) -> Result<(), String> {
		let path = Self::path(repo_root);
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
		}
		fs::write(&path, serde_json::to_string_pretty(self).map_err(|error| error.to_string())?).map_err(|error| format!("{}: {error}", path.display()))
	}

	// Clean records for a component, under the CURRENT model. Evidence produced by a different
	// selector over a different graph does not describe what runs today.
	pub fn clean_runs_for(&self, component: &str, model_hash: &str) -> usize {
		self.records.iter().filter(|record| record.model_hash == model_hash && record.verdict == "Consistent" && record.changed_components.iter().any(|changed| changed == component)).count()
	}

	// Targets this component has CLEAN evidence on. Filtering on the verdict is the whole point and
	// was missing: five clean x86_64 comparisons plus one riscv64 CandidateMiss counted as "evidence
	// from two targets" and could earn a certificate on the strength of a run that found a fault.
	pub fn clean_architectures_seen(&self, component: &str, model_hash: &str) -> BTreeSet<String> {
		self.records.iter().filter(|record| record.model_hash == model_hash && record.verdict == "Consistent" && record.changed_components.iter().any(|changed| changed == component)).map(|record| record.architecture.clone()).collect()
	}
}

// A digest of every source file a build reads, so "did the tree move under us" is answered by
// CONTENT. The same question `lib.sh`'s `source_digest` answers for builds, asked here for
// comparisons - and for the same reason: modification times cannot answer it.
pub fn source_digest(repo_root: &Path) -> Result<String, String> {
	let output = std::process::Command::new("git").arg("-C").arg(repo_root).arg("status").arg("--porcelain").output().map_err(|error| format!("git status: {error}"))?;
	let dirty = String::from_utf8_lossy(&output.stdout);
	let head = std::process::Command::new("git").arg("-C").arg(repo_root).arg("rev-parse").arg("HEAD").output().map_err(|error| format!("git rev-parse: {error}"))?;
	let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
	sha2::Digest::update(&mut hasher, head.stdout);
	// The dirty set by NAME is not enough - a file edited twice has the same name and different
	// content - so every changed file's bytes go in too.
	for line in dirty.lines() {
		let path = line.get(3..).unwrap_or("").rsplit(" -> ").next().unwrap_or("");
		if path.is_empty() {
			continue;
		}
		sha2::Digest::update(&mut hasher, path.as_bytes());
		if let Ok(bytes) = fs::read(repo_root.join(path)) {
			sha2::Digest::update(&mut hasher, bytes);
		}
	}
	Ok(format!("{:x}", sha2::Digest::finalize(hasher)))
}
