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

// The host producer's results, keyed on the whole `PlanItemKey` rather than on a check's name. The
// guest keeps `GuestResults`: a kernel test's identity IS its name, and the target and environment
// are properties of the run rather than of the line.
pub struct KeyedResults {
	pub outcomes: BTreeMap<PlanItemKey, Outcome>,
	pub total_declared: Option<usize>,
	pub duplicates: Vec<String>,
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
			// A dot is part of a name now: the guest names tests by their id
			// (`kernel.object.channel.foo`) rather than by the bare function name, so that every
			// layer - log, plan key, history, selection - spells a test the same way. Without the
			// dot here the whole log parsed as nothing and the comparison returned Void, which is
			// the guard doing its job and not an answer.
			if !name.is_empty() && name.chars().all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '.') {
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

// Which part of the model a comparison is evidence ABOUT.
//
// Shadow used to look only at keys beginning `kernel.`, while trust was granted per component - so a
// selector that dropped `host.flac`, `gate.volume-layout` or `build.volume` could not be caught by
// it, and the certificate said nothing about which kind of check the evidence covered. A component
// is trusted per universe now, and a universe with no evidence stays in shadow whatever the others
// have shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Universe {
	// Crate suites, gates and conformance runs. NOT builds - see `HostBuild`.
	//
	// It was "everything that happens on the host" until 2026-08-12, and that was a certificate
	// broader than its evidence. The Host producer deliberately excludes builds, for a good reason
	// it states in place: re-running three architectures' builds to answer "did anything outside the
	// selection fail" costs more than the sweep it is compared against, over artifacts the sweep has
	// already made. Sound about cost, and it left a universe whose evidence covered gates, suites
	// and conformance issuing certificates for a universe that also contained builds - so a selector
	// defect that drops `build.user` could not be observed by the producer, and five clean runs
	// later the component was TRUSTED for it anyway.
	//
	// The serialized name stays `Host`, so the records already on disk keep meaning what they said:
	// they were produced by that same producer, over that same evidence.
	Host,
	// Builds, which no shadow producer covers today. A separate universe so its certificate can only
	// be earned by evidence about builds - which is to say, not yet - rather than inherited from a
	// comparison that never looked at one. A universe's certificate may not be broader than the
	// evidence behind it, and this is that rule made structural rather than remembered.
	HostBuild,
	// The tagged suite inside `qemu-run.sh TEST=1`.
	TestGuest,
	// `dev-selftest.py` and friends, inside a guest `DEV_PROFILE=1` left running.
	DevGuest,
}

// The universes that have a shadow evidence producer.
//
// ONE PLACE, so the model can ask - a universe with no producer is a wall rather than a bar, because
// `trusted_everywhere` requires every JUDGING universe to answer and nothing can answer for it.
// `HostBuild` was in exactly that state between the split that created it and the `build-checks`
// producer, and 189 of the catalog's 192 components are judged by it.
pub fn universes_with_producers() -> std::collections::BTreeSet<Universe> {
	[Universe::Host, Universe::HostBuild, Universe::DevGuest, Universe::TestGuest].into_iter().collect()
}

impl Universe {
	// The serialized name, which is also what a diagnostic prints.
	pub fn as_str(&self) -> &'static str {
		match self {
			Universe::Host => "Host",
			Universe::HostBuild => "HostBuild",
			Universe::TestGuest => "TestGuest",
			Universe::DevGuest => "DevGuest",
		}
	}

	pub fn of(check: &str) -> Self {
		if check.starts_with("kernel.") {
			Universe::TestGuest
		} else if check.starts_with("dev.") {
			Universe::DevGuest
		} else if check.starts_with("build.") {
			Universe::HostBuild
		} else {
			Universe::Host
		}
	}
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

// A results file for the HOST universe: one `id PASS` or `id FAIL` per line, and a `total N` line.
//
// The guest gets its results by parsing a serial log, because that is the only channel a booted
// kernel has. The host runner has no such constraint - it knows the id of every check it ran and
// whether it passed - so it says so directly rather than being scraped.
//
// A `total` line is required for the same reason the guest's is: a run that covered fewer checks
// than exist did not compare what it claims to, and a filtered run looks exactly like a clean one.
// `check<TAB>architecture<TAB>environment<TAB>configuration<TAB>PASS|FAIL` per line, then `total N`.
//
// KEYED ON THE WHOLE `PlanItemKey`. This read `id PASS` into a `BTreeMap<String, _>`, so a check
// with two Host variants collapsed into one entry while `total` counted both - a disagreement
// nothing reported. The frozen model has four fields in the key and evidence that discards one of
// them cannot be evidence about it.
pub fn parse_host_log(text: &str) -> KeyedResults {
	let mut outcomes: BTreeMap<PlanItemKey, Outcome> = BTreeMap::new();
	let mut total_declared = None;
	let mut duplicates: Vec<String> = Vec::new();
	for line in text.lines() {
		let line = line.trim();
		if let Some(rest) = line.strip_prefix("total ") {
			total_declared = rest.trim().parse::<usize>().ok();
			continue;
		}
		let fields: Vec<&str> = line.split('\t').collect();
		let [check, architecture, environment, configuration, verdict] = fields[..] else { continue };
		let outcome = match verdict.trim() {
			"PASS" => Outcome::Passed,
			"FAIL" => Outcome::Failed,
			_ => continue,
		};
		let Some(environment) = crate::catalog::Environment::from_str(environment.trim()) else { continue };
		let key = PlanItemKey { check: check.trim().to_string(), architecture: architecture.trim().to_string(), environment, configuration: configuration.trim().to_string() };
		// Said out loud rather than overwritten: two lines for one key means the producer emitted
		// the same variant twice, and the old shape hid exactly that.
		if outcomes.insert(key.clone(), outcome).is_some() {
			duplicates.push(key.display());
		}
	}
	KeyedResults { outcomes, total_declared, duplicates }
}

// The guest comparison: kernel test names, one target, the test-guest environment.
pub fn compare(selected: &[PlanItemKey], results: &GuestResults, architecture: &str, history: &History) -> Comparison {
	compare_in(selected, results, architecture, "test-guest", "test", None, history)
}

// SHADOW-EXEC: the selection is RUN, and the run is compared against the sweep.
//
// A dry shadow answers "did the selector choose the right set S". It never runs S, so it cannot
// answer "does running S work" - and this milestone contains the proof that the second question is
// not theoretical. In an intermediate state the planner emitted Rust function names while the runner
// had moved to explicit stable IDs: the selection was computed CORRECTLY and could not be executed.
// Every dry comparison in that state was clean. The defect was found by hand.
//
// So this asks three things a dry comparison cannot:
//
//   * EXECUTABLE - every key in S actually ran in the scoped log. A selection naming a test the
//     runner cannot find is the defect above, and it is invisible to a comparison that never runs.
//   * NOT WIDER - the scoped run ran nothing but S. A run that quietly widened to the whole suite
//     is safe and it is also not a selection; measuring one and calling it the other is how the
//     selection dimension comes to look like it pays.
//   * AGREEING - every key in S has the same outcome in both runs. A test that passes alone and
//     fails in the suite (or the reverse) means running S is not running that part of the sweep,
//     whatever the selector chose.
// THE SAME EXECUTION SAMPLE FOR THE KEYED UNIVERSES - host checks and dev checks.
//
// SHADOW-EXEC was `TestGuest`-only, and the exemption was honest at the time: the host and dev
// producers had no way to run a SELECTION separately from the sweep, so requiring a sample would have
// made those universes permanently untrustable rather than honestly graded.
//
// It is also the exemption that hid a real defect for a round. The dev producer lowered
// `(cd src && boot/dev-selftest.py)` through a rule that appends cargo flags to anything not called
// `default`, and emitted a bash syntax error - a selection computed correctly and impossible to
// execute, which is precisely what an execution sample detects and exactly the class of defect this
// mechanism exists for.
//
// The three questions are the guest's three: did every selected key RUN, did anything run that was
// not selected, and did any key disagree between the scoped run and the sweep.
pub fn compare_exec_keyed(selected: &[PlanItemKey], scoped_run: &KeyedResults, sweep: &KeyedResults, universe: &str, in_universe: impl Fn(&PlanItemKey) -> bool) -> Comparison {
	let wanted: BTreeSet<&PlanItemKey> = selected.iter().filter(|key| in_universe(key)).collect();
	let ran: BTreeSet<&PlanItemKey> = scoped_run.outcomes.keys().collect();
	let mut reasons: Vec<String> = Vec::new();
	let missing: Vec<String> = wanted.iter().filter(|key| !ran.contains(**key)).map(|key| key.display()).collect();
	let extra: Vec<String> = ran.iter().filter(|key| !wanted.contains(**key)).map(|key| key.display()).collect();
	let mut disagreed: Vec<String> = Vec::new();
	for key in &wanted {
		match (scoped_run.outcomes.get(*key), sweep.outcomes.get(*key)) {
			(Some(alone), Some(in_sweep)) if alone != in_sweep => disagreed.push(format!("{} ({alone:?} alone, {in_sweep:?} in the sweep)", key.display())),
			_ => {}
		}
	}
	// NOTHING TO COMPARE IS NOT AGREEMENT - the same rule the guest sample states.
	if wanted.is_empty() || scoped_run.outcomes.is_empty() {
		return Comparison { verdict: Verdict::Void, reason: format!("nothing to execute: the selection names {} {universe} check(s) and the scoped run parsed {} outcome(s)", wanted.len(), scoped_run.outcomes.len()), architecture: universe.to_string(), scoped: wanted.len(), ran: scoped_run.outcomes.len(), outside_failures: Vec::new(), inside_failures: Vec::new(), previously_failed: Vec::new() };
	}
	if !missing.is_empty() {
		reasons.push(format!("{} selected check(s) did not run when the selection was executed: {}", missing.len(), missing.join(", ")));
	}
	if !extra.is_empty() {
		reasons.push(format!("{} check(s) ran that the selection did not name: {}", extra.len(), extra.join(", ")));
	}
	if !disagreed.is_empty() {
		reasons.push(format!("{} check(s) answered differently alone than in the sweep: {}", disagreed.len(), disagreed.join(", ")));
	}
	// `SelectionFailed` for the same reason the guest sample uses it: a selection that cannot be
	// executed is the selector's finding, not the code's.
	let verdict = if reasons.is_empty() { Verdict::Consistent } else { Verdict::SelectionFailed };
	let reason = if reasons.is_empty() { format!("every {universe} check the selection named ran, nothing else did, and each agreed with the sweep") } else { reasons.join("; ") };
	Comparison { verdict, reason, architecture: universe.to_string(), scoped: wanted.len(), ran: scoped_run.outcomes.len(), outside_failures: extra, inside_failures: missing, previously_failed: Vec::new() }
}

pub fn compare_exec(selected: &[PlanItemKey], scoped_run: &GuestResults, sweep: &GuestResults, architecture: &str) -> Comparison {
	// THE WHOLE CHECK ID, unstripped. A kernel check's id IS the test's declared id - the model's
	// own self-check asserts that equality, because it is what makes `TEST_SELECTION` work - and
	// those ids already begin with `kernel.`. Stripping the prefix produced a selection the runner
	// refused with `no test with id 'applications....'`.
	//
	// Recorded because it is this mechanism's own first finding, on its first run, and it is exactly
	// the class of defect the mechanism exists for: a selection computed correctly and impossible to
	// execute, invisible to every dry comparison.
	let wanted: BTreeSet<String> = selected.iter().filter(|key| key.environment == crate::catalog::Environment::TestGuest && key.architecture == architecture).map(|key| key.check.clone()).collect();
	let mut reasons: Vec<String> = Vec::new();
	let missing: Vec<String> = wanted.iter().filter(|name| !scoped_run.outcomes.contains_key(*name)).cloned().collect();
	let extra: Vec<String> = scoped_run.outcomes.keys().filter(|name| !wanted.contains(*name)).cloned().collect();
	let mut disagreed: Vec<String> = Vec::new();
	for name in &wanted {
		match (scoped_run.outcomes.get(name), sweep.outcomes.get(name)) {
			(Some(alone), Some(in_sweep)) if alone != in_sweep => disagreed.push(format!("{name} ({alone:?} alone, {in_sweep:?} in the sweep)")),
			_ => {}
		}
	}
	// NOTHING TO COMPARE IS NOT AGREEMENT. An empty selection, or a scoped log that parsed as
	// nothing, would otherwise sail through all three checks and be filed as evidence.
	if wanted.is_empty() || scoped_run.outcomes.is_empty() {
		return Comparison { verdict: Verdict::Void, reason: format!("nothing to execute: the selection names {} guest test(s) on {architecture} and the scoped run parsed {} outcome(s)", wanted.len(), scoped_run.outcomes.len()), architecture: architecture.to_string(), scoped: wanted.len(), ran: scoped_run.outcomes.len(), outside_failures: Vec::new(), inside_failures: Vec::new(), previously_failed: Vec::new() };
	}
	if !missing.is_empty() {
		reasons.push(format!("{} selected test(s) did not run when the selection was executed: {}", missing.len(), missing.join(", ")));
	}
	if !extra.is_empty() {
		reasons.push(format!("the scoped run ran {} test(s) outside the selection - it widened rather than selecting: {}", extra.len(), extra.iter().take(8).cloned().collect::<Vec<_>>().join(", ")));
	}
	if !disagreed.is_empty() {
		reasons.push(format!("{} test(s) answered differently alone than in the sweep: {}", disagreed.len(), disagreed.join(", ")));
	}
	Comparison { verdict: if reasons.is_empty() { Verdict::Consistent } else { Verdict::SelectionFailed }, reason: if reasons.is_empty() { format!("the selection ran, ran only itself, and agreed with the sweep on all {} test(s)", wanted.len()) } else { reasons.join("; ") }, architecture: architecture.to_string(), scoped: wanted.len(), ran: scoped_run.outcomes.len(), outside_failures: Vec::new(), inside_failures: missing, previously_failed: Vec::new() }
}

// The HOST comparison, which had no producer at all.
//
// `trusted_everywhere` asks for Host AND TestGuest, and nothing wrote a Host record - so it could
// not return true for any component, ever. A trust model with a universe it never feeds is not a
// model with a gap in it, it is a constant that reads like a model.
//
// The ids are the catalog's own (`gate.x`, `host.y`), so nothing is stripped; the architecture is
// `host`, which is the only one this universe has.
// The HOST universe, compared key against key.
//
// This used to hand `compare_in` a hardcoded architecture, environment and configuration and let it
// REBUILD a key from the check's name - `"host" / "host" / "shared-image"` for everything. Gates,
// conformance suites and the default host-suite variants are `default`, not `shared-image`, so the
// key it rebuilt was the wrong one for most of them: the history lookup missed, and a `shared-image`
// variant and a `default` variant of one crate were indistinguishable. The producer now emits the
// real key, so nothing has to be reconstructed.
pub fn compare_host(selected: &[PlanItemKey], results: &KeyedResults, history: &History) -> Comparison {
	compare_keyed(selected, results, crate::catalog::Environment::Host, "host", history)
}

// The DEV guest, through the same comparison. It hardcoded `x86_64 / dev-guest / development` and
// rebuilt a key from the check's name for the same reason the host one did; one architecture makes
// that less wrong rather than right, and there is no reason for two shapes here.
pub fn compare_dev(selected: &[PlanItemKey], results: &KeyedResults, history: &History) -> Comparison {
	compare_keyed(selected, results, crate::catalog::Environment::DevGuest, "x86_64", history)
}

// BUILDS, through the same comparison, against the catalog rather than against a name.
//
// `HostBuild` and `Host` share an `Environment` - the split is in the UNIVERSE, which the catalog
// derives from the check's KIND - so a comparison filtering on environment alone would put every
// build key in the host universe and vice versa. The catalog is passed rather than a
// `check.starts_with("build.")` test, because deciding from a name is the mistake this milestone has
// now recorded three times.
pub fn compare_build(selected: &[PlanItemKey], results: &KeyedResults, history: &History, catalog: &crate::catalog::Catalog, architecture: &str) -> Comparison {
	compare_with(selected, results, history, architecture, |key| key.environment == crate::catalog::Environment::Host && is_build(catalog, &key.check) && key.architecture == architecture)
}

// Whether a check id names a build, asked of the catalog.
fn is_build(catalog: &crate::catalog::Catalog, check: &str) -> bool {
	catalog.checks.iter().any(|entry| entry.id == check && entry.kind == crate::catalog::CheckKind::Build)
}

fn compare_keyed(selected: &[PlanItemKey], results: &KeyedResults, environment: crate::catalog::Environment, architecture: &str, history: &History) -> Comparison {
	compare_with(selected, results, history, architecture, |key| key.environment == environment)
}

fn compare_with(selected: &[PlanItemKey], results: &KeyedResults, history: &History, architecture: &str, in_universe: impl Fn(&PlanItemKey) -> bool) -> Comparison {
	let scoped: BTreeSet<&PlanItemKey> = selected.iter().filter(|key| in_universe(key)).collect();
	let mut outside = Vec::new();
	let mut inside = Vec::new();
	let mut previously_failed = Vec::new();
	for (key, outcome) in &results.outcomes {
		if *outcome != Outcome::Failed {
			continue;
		}
		let display = key.display();
		if history.get(&display).is_some_and(|record| record.failures > 0) {
			previously_failed.push(display.clone());
		}
		if scoped.contains(key) { inside.push(display) } else { outside.push(display) }
	}
	let ran = results.outcomes.len();
	// A producer that emitted one key twice compared something other than what it counted.
	if !results.duplicates.is_empty() {
		return Comparison { verdict: Verdict::Void, reason: format!("the log carries {} duplicate key(s) ({}), so its outcomes and its total describe different runs", results.duplicates.len(), results.duplicates.join(", ")), architecture: architecture.to_string(), scoped: scoped.len(), ran, outside_failures: outside, inside_failures: inside, previously_failed };
	}
	if let Some(total) = results.total_declared
		&& ran < total
		&& inside.is_empty()
		&& outside.is_empty()
	{
		return Comparison { verdict: Verdict::Void, reason: format!("the log records {ran} of {total} checks and no failure - a full sweep is what this compares against, so a partial one proves nothing"), architecture: architecture.to_string(), scoped: scoped.len(), ran, outside_failures: outside, inside_failures: inside, previously_failed };
	}
	let (verdict, reason) = if !inside.is_empty() {
		(Verdict::SelectionFailed, format!("{} selected check(s) failed - the selector chose them, so this is a defect in the code and not in the selection", inside.len()))
	} else if !outside.is_empty() {
		(Verdict::CandidateMiss, format!("{} check(s) OUTSIDE the selection failed while every selected check passed - confirm before believing it", outside.len()))
	} else {
		(Verdict::Consistent, format!("{ran} checks ran, none failed; the selection of {} is not contradicted", scoped.len()))
	};
	Comparison { verdict, reason, architecture: architecture.to_string(), scoped: scoped.len(), ran, outside_failures: outside, inside_failures: inside, previously_failed }
}

fn compare_in(selected: &[PlanItemKey], results: &GuestResults, architecture: &str, environment: &str, configuration: &str, strip: Option<&str>, history: &History) -> Comparison {
	let scoped: BTreeSet<&str> = selected
		.iter()
		.filter(|key| key.architecture == architecture)
		.filter_map(|key| match strip {
			Some(prefix) => key.check.strip_prefix(prefix),
			None => Some(key.check.as_str()),
		})
		.collect();
	let mut outside = Vec::new();
	let mut inside = Vec::new();
	let mut previously_failed = Vec::new();
	for (name, outcome) in &results.outcomes {
		if *outcome != Outcome::Failed {
			continue;
		}
		let key = format!("{}{name} / {architecture} / {environment} / {configuration}", strip.unwrap_or(""));
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
// Records written before the universe field existed only ever compared the kernel suite, which is
// what this says on their behalf.
fn test_guest() -> Universe {
	Universe::TestGuest
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Record {
	// Which universe this comparison examined. Defaulted for records written before the field
	// existed: they only ever compared the kernel suite, which is what they say.
	#[serde(default = "test_guest")]
	pub universe: Universe,
	pub architecture: String,
	pub verdict: String,
	pub reason: String,
	pub model_hash: String,
	pub source_digest: String,
	pub changed_components: Vec<String>,
	pub outside_failures: Vec<String>,
	pub at: u64,
	// WHAT THIS RECORD IS EVIDENCE ABOUT, beyond "a run happened".
	//
	// `Store::evaluate` counts clean runs and clean architectures. The design also asked for every
	// change class exercised, every edge kind exercised, a regression corpus caught, property tests
	// green and a SHADOW-EXEC sample - and those criteria cannot be written before there are records
	// to grade them against. That is a fair reason not to write the POLICY yet. It is not a reason
	// to keep discarding the DATA: a run recorded without these can never be graded against the
	// criteria when they arrive, so the cost of waiting is paid in evidence that has to be re-earned.
	//
	// Defaulted, because records written before the fields existed carry none of it and should say
	// so rather than be dropped.
	#[serde(default)]
	pub change_kinds: Vec<String>,
	#[serde(default)]
	pub edge_kinds: Vec<String>,
	// Whether a SHADOW-EXEC sample accompanied this comparison - running the scoped selection and
	// then the full suite, rather than computing the selection and only running the full one. False
	// on every record today; the field exists so the day it is true is recorded rather than
	// reconstructed.
	#[serde(default)]
	pub shadow_exec: bool,
	// Whether the model's own `self_check` passed at the time. A comparison made by a model that
	// fails its own checks is not evidence about the tree.
	#[serde(default)]
	pub model_self_check: bool,
}

// The kinds of change a set of paths represents, read from the working tree.
//
// Best effort by design: the regression corpus drives `shadow` with `--paths`, where there is no
// working tree to ask and the answer is legitimately empty. A record that says nothing about its
// change classes is better than one that invents them.
pub fn change_kinds_for(repo_root: &Path, paths: &[String]) -> Vec<String> {
	let Ok(changes) = crate::changes::working_tree(repo_root) else { return Vec::new() };
	let wanted: BTreeSet<&str> = paths.iter().map(String::as_str).collect();
	changes.iter().filter(|change| wanted.contains(change.path.as_str()) || change.origin.as_deref().is_some_and(|origin| wanted.contains(origin))).map(|change| format!("{:?}", change.kind).to_lowercase()).collect::<BTreeSet<String>>().into_iter().collect()
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
	// A CLEAN RUN IS ALSO ONE THE MODEL WAS ENTITLED TO MAKE, which `model_self_check` records.
	//
	// A comparison produced by a model that failed its own gates is not evidence about the tree: the
	// selection it computed came from a model that contradicts itself, and "the selection matched
	// the sweep" says nothing when both sides were derived from it. This is the one criterion out of
	// the design's list that needs no accumulated records to grade - it is a property of each record
	// on its own - so it is consulted, while change classes, edge kinds, the regression corpus and
	// SHADOW-EXEC stay uncounted until there is a log to grade them against.
	//
	// A record written before the field existed says nothing, and `#[serde(default)]` on a bool is
	// `false` - so it does not count. That is FAILING CLOSED and it is the right direction here, by
	// the same argument that makes an unresolved handle cardinality a refusal: a record that cannot
	// say whether the model passed its own checks is not evidence that it did. The cost is evidence
	// that has to be re-earned, which is what this milestone already says the cost of waiting is.
	fn is_clean(record: &Record, component: &str, model_hash: &str, universe: Universe) -> bool {
		record.model_hash == model_hash && record.universe == universe && record.verdict == "Consistent" && record.model_self_check && record.changed_components.iter().any(|changed| changed == component)
	}

	// Whether any clean record for this component carries an execution sample.
	//
	// One is enough and every run is too many: a SHADOW-EXEC sample costs a second full boot, and
	// the question it answers - "can this selection be executed at all" - is a property of the
	// mechanism rather than of the change. What it must not be is zero, which is what it was while
	// a planner emitting Rust function names computed selections no runner could match and every dry
	// comparison stayed clean.
	pub fn has_exec_sample(&self, component: &str, model_hash: &str, universe: Universe) -> bool {
		self.records.iter().any(|record| Self::is_clean(record, component, model_hash, universe) && record.shadow_exec)
	}

	pub fn clean_runs_for(&self, component: &str, model_hash: &str, universe: Universe) -> usize {
		self.records.iter().filter(|record| Self::is_clean(record, component, model_hash, universe)).count()
	}

	// Targets this component has CLEAN evidence on. Filtering on the verdict is the whole point and
	// was missing: five clean x86_64 comparisons plus one riscv64 CandidateMiss counted as "evidence
	// from two targets" and could earn a certificate on the strength of a run that found a fault.
	pub fn clean_architectures_seen(&self, component: &str, model_hash: &str, universe: Universe) -> BTreeSet<String> {
		self.records.iter().filter(|record| Self::is_clean(record, component, model_hash, universe)).map(|record| record.architecture.clone()).collect()
	}
}

// A digest of every source file a build reads, so "did the tree move under us" is answered by
// CONTENT. The same question `lib.sh`'s `source_digest` answers for builds, asked here for
// comparisons - and for the same reason: modification times cannot answer it.
pub fn source_digest(repo_root: &Path) -> Result<String, String> {
	// Through `changes.rs`, which is the ONE place in this tool that reads git.
	//
	// This parsed `git status --porcelain` by hand: it split each line on `" -> "` and kept the
	// right-hand side, which is the exact rename defect `changes.rs` was written to fix, and it was
	// not NUL-safe, so a path with a newline in it read as two files. Both of those change what this
	// digest covers, and this digest is the thing that decides whether a shadow comparison is still
	// about the tree that was compared.
	//
	// A second git parser in the tool whose subject is "the model must not quietly be wrong" is the
	// kind of duplication that is only ever noticed after it has been wrong.
	let head = std::process::Command::new("git").arg("-C").arg(repo_root).arg("rev-parse").arg("HEAD").output().map_err(|error| format!("git rev-parse: {error}"))?;
	let changes = crate::changes::working_tree(repo_root)?;
	let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
	sha2::Digest::update(&mut hasher, head.stdout);
	for change in &changes {
		// The KIND as well as the paths: the same file deleted and re-added is not the same tree as
		// the file left alone, and both would otherwise hash to whatever its bytes are now.
		sha2::Digest::update(&mut hasher, format!("\n{:?} ", change.kind).as_bytes());
		// BOTH sides of a rename. Keeping only the destination was how a rename out of a directory
		// looked identical to a file appearing in one.
		if let Some(origin) = &change.origin {
			sha2::Digest::update(&mut hasher, origin.as_bytes());
			sha2::Digest::update(&mut hasher, b" -> ");
		}
		sha2::Digest::update(&mut hasher, change.path.as_bytes());
		// And the content, because a file edited twice has the same name and different bytes. A
		// deletion has none, and its absence is part of what makes the digest differ.
		match fs::read(repo_root.join(&change.path)) {
			Ok(bytes) => sha2::Digest::update(&mut hasher, bytes),
			Err(_) => sha2::Digest::update(&mut hasher, b"<absent>"),
		}
	}
	Ok(format!("{:x}", sha2::Digest::finalize(hasher)))
}
