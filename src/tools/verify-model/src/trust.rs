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
use std::collections::BTreeMap;
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
		// the same argument as the guest suite. There is no producer for this universe yet, so the
		// number is what it will need rather than what it currently gets.
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
fn exec_universes(universe: crate::shadow::Universe) -> bool {
	matches!(universe, crate::shadow::Universe::TestGuest | crate::shadow::Universe::Host | crate::shadow::Universe::DevGuest)
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

	pub fn grant(&mut self, component: &str, model_hash: &str, universe: crate::shadow::Universe, clean_runs: usize, architectures: Vec<String>, at: u64) {
		self.certificates.retain(|certificate| !(certificate.component == component && certificate.universe == universe));
		self.certificates.push(Certificate { component: component.to_string(), universe, model_hash: model_hash.to_string(), clean_runs, architectures, granted_at: at, note: String::from("earned by clean dry-shadow comparisons under this model hash, in this universe alone; it lapses the moment the hash moves") });
		self.certificates.sort_by(|left, right| (&left.component, left.universe).cmp(&(&right.component, right.universe)));
	}

	pub fn summary(&self, model_hash: &str) -> BTreeMap<String, Level> {
		self.certificates.iter().map(|certificate| (format!("{} ({:?})", certificate.component, certificate.universe), if certificate.model_hash == model_hash { Level::Trusted } else { Level::Shadow })).collect()
	}
}
