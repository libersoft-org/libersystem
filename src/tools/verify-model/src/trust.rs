// Trust as a certificate against one model hash, not a permanent property of a name.
//
// Evidence proves that a particular selector, over a particular model, did not miss anything.
// Change the model and the evidence no longer describes what is running - but the word TRUSTED
// survives, which is how a selector gets silently more dangerous over time while its record still
// looks clean. So a certificate names the hash it was earned under, and a hash that has moved
// demotes it with no ceremony.
//
// Graded per component, because a month that only saw audio changes says nothing about the driver,
// kernel or arch edges - and grading them together would make the saving wait for the slowest proof
// in the system.

use crate::shadow::Log;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Level {
	// Every scoped decision about this component is validated against a full sweep before it is
	// believed. The default, and where everything starts.
	Shadow,
	// Enough clean evidence under THIS model that a scoped run is taken at its word.
	Trusted,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Certificate {
	pub component: String,
	// Evidence about the kernel suite says nothing about whether a host suite or a gate would have
	// been selected. Granting one certificate over all of them was the gap.
	#[serde(default = "test_guest_universe")]
	pub universe: crate::shadow::Universe,
	pub model_hash: String,
	pub clean_runs: usize,
	pub architectures: Vec<String>,
	pub granted_at: u64,
	pub note: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Store {
	pub schema: u32,
	pub certificates: Vec<Certificate>,
}

// What a component has to show before a scoped answer about it is taken at its word.
//
// Deliberately not "no failures for a month". A month that saw only audio changes proves nothing
// about the driver edges, so the criterion is over EVIDENCE rather than over time: clean shadow
// comparisons under the current model, on more than one target, with the regression corpus green.
// The universe a certificate written before universes existed belongs to. Serde needs a function.
fn test_guest_universe() -> crate::shadow::Universe {
	crate::shadow::Universe::TestGuest
}

pub const REQUIRED_CLEAN_RUNS: usize = 5;

// How many distinct targets a universe has to show evidence from, per universe.
//
// It was one constant, 2, applied to all three - which is right for `TestGuest` and IMPOSSIBLE for
// the other two. `Host` runs on the host and has exactly one target; `DevGuest` is built for x86_64
// alone. A universal number therefore made both permanently unqualifiable for a reason that has
// nothing to do with their evidence, and `trusted_everywhere` - which asks for Host AND TestGuest -
// could never return true for anything, ever. A trust model that can only answer "not trusted" is
// not a model, it is a constant.
//
// The number is a property of what the universe can REACH, so it lives beside the universe rather
// than beside the caller.
pub fn required_architectures(universe: crate::shadow::Universe) -> usize {
	match universe {
		// Guest suites run on all three targets, and a component validated on one says nothing about
		// the others - the whole reason this field exists.
		crate::shadow::Universe::TestGuest => 2,
		// One target by construction: the host is the host.
		crate::shadow::Universe::Host => 1,
		// Builds run for all three targets, so evidence about one says nothing about the others -
		// the same argument as the guest suite. (This said "there is no producer for this universe
		// yet" until 2026-08-14, which stopped being true when `build-checks` became the third
		// producer in the seventh round. A comment describing a previous state as a current one is
		// the defect this milestone has now recorded four times.)
		crate::shadow::Universe::HostBuild => 2,
		// Built for x86_64 only, so asking for two is asking for a target that does not exist.
		crate::shadow::Universe::DevGuest => 1,
	}
}

// Which universes have an execution mechanism a sample can be taken of.
//
// The guest suite has one: an exact `TEST_SELECTION` handed to a runner that can fail to match it.
// The host and dev producers lower a selection into shell commands, and a sample of those is exactly
// as valuable - `verify.sh` now runs the scoped selection on both before the sweep, so they are here
// too. The exemption they used to have is what hid a real defect for a round: the dev producer
// emitted a command bash could not parse, which is precisely what an execution sample detects.
//
// `HostBuild` stays exempt, and this is the reason rather than an omission: "executing the
// selection" there means building those parts, and the sweep builds every part anyway - so a scoped
// build run would be a second full build for a sample the sweep has already effectively taken. The
// day that stops being true, this is one line.
// What a component's clean evidence actually covers, beside how much of it there is.
//
// A certificate says "this component's scoped runs can be believed"; the design's criteria say that
// sentence needs a scope. These are the parts of it the records can answer today.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct EvidenceScope {
	pub records: usize,
	// The classes of change the clean evidence was made over - source edits, manifest edits, test
	// edits. Five perfect verifications of one class are not evidence about another.
	pub change_kinds: Vec<String>,
	// The graph edges the selector walked to reach this component in those runs.
	pub edge_kinds: Vec<String>,
	// Whether the model passed its own checks in every one of them - which `is_clean` already
	// requires, so this is `true` whenever `records` is non-zero. Kept because the scope is a
	// STATEMENT about what was proved, and "the model was sound throughout" is part of that
	// statement even when another rule is what enforces it.
	pub all_self_checked: bool,
}

// EVERY UNIVERSE WITH A PRODUCER, which is now all four.
//
// `HostBuild` was exempted on the reasoning that a full sweep builds everything anyway, so a scoped
// build proves nothing new. That is true of the BUILD and false of the MECHANISM: the evidence
// producer runs the catalog's commands one at a time - `./build.sh --arch X --part libs`, then
// `--part user` - and the production runner groups them into `./build.sh --arch X --part a,b,c`.
// Those are different code paths through the same script, and only the second one ships. A grouped
// part list whose parser silently used only the first entry and exited zero would leave every
// individual build check passing, every record clean, a certificate granted, and the scoped runner
// building less than the selection said - which is exactly the defect that produced SHADOW-EXEC: a
// planner naming Rust function names while the runner used stable IDs, every dry comparison clean,
// found by hand.
fn exec_universes(_universe: crate::shadow::Universe) -> bool {
	true
}

impl Store {
	pub fn path(repo_root: &Path) -> PathBuf {
		repo_root.join(".build/state/verify-trust.json")
	}

	pub fn load(repo_root: &Path) -> Self {
		let path = Self::path(repo_root);
		let Ok(text) = fs::read_to_string(&path) else { return Store { schema: 1, certificates: Vec::new() } };
		serde_json::from_str(&text).unwrap_or(Store { schema: 1, certificates: Vec::new() })
	}

	pub fn save(&self, repo_root: &Path) -> Result<(), String> {
		let path = Self::path(repo_root);
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
		}
		fs::write(&path, serde_json::to_string_pretty(self).map_err(|error| error.to_string())?).map_err(|error| format!("{}: {error}", path.display()))
	}

	// A certificate earned under a different model is not demoted so much as it stops applying: the
	// thing it vouched for is not the thing that runs now.
	pub fn level(&self, component: &str, model_hash: &str, universe: crate::shadow::Universe) -> Level {
		match self.certificates.iter().find(|certificate| certificate.component == component && certificate.universe == universe) {
			Some(certificate) if certificate.model_hash == model_hash => Level::Trusted,
			_ => Level::Shadow,
		}
	}

	// A component is only fully trusted when every universe that CAN judge it has said so.
	//
	// The pair was hard-coded, and one half of it had no producer, so this answered false for
	// everything forever. Both halves are wrong in the same way: which universes can judge a
	// component is a fact about the component, not a constant. A development-only binary is judged
	// by `DevGuest` and by nothing else; a host tool that never enters an image is judged by `Host`.
	//
	// `universes` comes from the catalog - the environments the component's own checks run in - so a
	// component acquires and loses judges as its checks do.
	pub fn trusted_everywhere(&self, component: &str, model_hash: &str, universes: &[crate::shadow::Universe]) -> bool {
		if universes.is_empty() {
			return false;
		}
		universes.iter().all(|universe| self.level(component, model_hash, *universe) == Level::Trusted)
	}

	pub fn stale(&self, model_hash: &str) -> Vec<&Certificate> {
		self.certificates.iter().filter(|certificate| certificate.model_hash != model_hash).collect()
	}

	// Drop certificates the current model has invalidated. Called on every read so a demotion never
	// waits for someone to remember to run something.
	pub fn prune(&mut self, model_hash: &str) -> Vec<String> {
		let dropped: Vec<String> = self.certificates.iter().filter(|certificate| certificate.model_hash != model_hash).map(|certificate| certificate.component.clone()).collect();
		self.certificates.retain(|certificate| certificate.model_hash == model_hash);
		dropped
	}

	// Whether the evidence on record would earn a certificate right now, and if not, what is short.
	pub fn evaluate(&self, component: &str, model_hash: &str, universe: crate::shadow::Universe, log: &Log) -> Result<(usize, Vec<String>), String> {
		let clean = log.clean_runs_for(component, model_hash, universe);
		let architectures: Vec<String> = log.clean_architectures_seen(component, model_hash, universe).into_iter().collect();
		if clean < REQUIRED_CLEAN_RUNS {
			return Err(format!("{clean} clean shadow comparison(s) under this model, {REQUIRED_CLEAN_RUNS} needed"));
		}
		// AND THEY HAVE TO BE FIVE DIFFERENT COMPARISONS, not one comparison five times.
		//
		// The selector is deterministic and there is a property test that says so, so running the
		// same change against the same tree five times produces the same selection and the same
		// sweep result five times over. Counting those as five would let a certificate be earned by
		// repetition - a green worth exactly one comparison, wearing the number five - which is the
		// failure this whole milestone is about, appearing inside its own trust criteria.
		//
		// Found 2026-08-14, working out how the first real certificate this tool would ever grant
		// could be earned: the cheapest way to get one was to run a single shadow comparison five
		// times over, and it would have worked.
		let distinct = log.distinct_evidence_for(component, model_hash, universe);
		if distinct < REQUIRED_CLEAN_RUNS {
			return Err(format!("{clean} clean comparison(s) but only {distinct} distinct one(s) - {REQUIRED_CLEAN_RUNS} different (tree, change) pairs are needed, because the selector is deterministic and the same comparison repeated is one piece of evidence"));
		}
		let needed = required_architectures(universe);
		if architectures.len() < needed {
			return Err(format!("evidence from {} target(s) ({}), {needed} needed in this universe - a component validated on one target says nothing about the others", architectures.len(), architectures.join(", ")));
		}
		// AND ONE RUN IN WHICH THE SELECTION WAS ACTUALLY EXECUTED.
		//
		// Every record above is a DRY comparison: the scoped set was computed and never run, so all
		// of them together answer "did the selector choose the right set" and none of them answers
		// "does running that set work". This milestone contains the proof that the second question
		// is not theoretical - a planner emitting Rust function names against a runner using stable
		// IDs computed every selection correctly, could execute none of them, and left every dry
		// comparison clean. That was found by hand.
		//
		// One sample per universe is the requirement, not one per run: a sample costs a second full
		// sweep, and what it establishes is a property of the execution mechanism rather than of the
		// change. `verify.sh --shadow-exec` produces it.
		if !exec_universes(universe) {
			return Ok((clean, architectures));
		}
		if !log.has_exec_sample(component, model_hash, universe) {
			return Err(String::from("no run has EXECUTED this selection - every comparison on record is dry, so they say the right set was chosen and nothing about whether running it works; ./verify.sh --shadow-exec produces the sample"));
		}
		Ok((clean, architectures))
	}

	// The criteria the RECORDS can already answer, which `evaluate` did not read.
	//
	// The frozen design named them - every relevant change class exercised, every relevant edge kind
	// exercised, the regression corpus green in the run that produced the record - and the records
	// have carried `change_kinds`, `edge_kinds` and `model_self_check` since the round that added
	// them, under a comment saying the policy could not be written before there were records to
	// grade. There are records now, and the data was being discarded at the point of judgement
	// rather than at the point of collection.
	//
	// The reason it matters is the one distinctness only partly addresses: five perfect verifications
	// of one KIND of change are not evidence about a different kind. Distinctness makes them five
	// different decisions; this makes them five different KINDS of decision.
	//
	// Graded rather than required: what is returned is what the evidence covers, so a caller can say
	// "trusted for source changes and manifest changes" instead of "trusted". A certificate that
	// cannot name its own scope is the broad-certificate defect this milestone split `HostBuild` out
	// to prevent, one level up.
	pub fn evidence_scope(&self, component: &str, model_hash: &str, universe: crate::shadow::Universe, log: &Log) -> EvidenceScope {
		let mut change_kinds: BTreeSet<String> = BTreeSet::new();
		let mut edge_kinds: BTreeSet<String> = BTreeSet::new();
		let mut all_self_checked = true;
		let mut records = 0usize;
		for record in log.records.iter().filter(|record| Log::is_clean(record, component, model_hash, universe)) {
			records += 1;
			change_kinds.extend(record.change_kinds.iter().cloned());
			edge_kinds.extend(record.edge_kinds.iter().cloned());
			all_self_checked &= record.model_self_check;
		}
		EvidenceScope { records, change_kinds: change_kinds.into_iter().collect(), edge_kinds: edge_kinds.into_iter().collect(), all_self_checked }
	}

	pub fn grant(&mut self, component: &str, model_hash: &str, universe: crate::shadow::Universe, clean_runs: usize, architectures: Vec<String>, at: u64) {
		self.certificates.retain(|certificate| !(certificate.component == component && certificate.universe == universe));
		self.certificates.push(Certificate { component: component.to_string(), universe, model_hash: model_hash.to_string(), clean_runs, architectures, granted_at: at, note: String::from("earned by clean dry-shadow comparisons under this model hash, in this universe alone; it lapses the moment the hash moves") });
		self.certificates.sort_by(|left, right| (&left.component, left.universe).cmp(&(&right.component, right.universe)));
	}

	pub fn summary(&self, model_hash: &str) -> BTreeMap<String, Level> {
		self.certificates.iter().map(|certificate| (format!("{} ({:?})", certificate.component, certificate.universe), if certificate.model_hash == model_hash { Level::Trusted } else { Level::Shadow })).collect()
	}
}
