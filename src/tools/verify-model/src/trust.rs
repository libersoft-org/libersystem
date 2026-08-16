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
	// WHAT THE EVIDENCE COVERED, carried on the grant rather than printed beside it.
	//
	// This was computed at grant time, written to stdout and thrown away, so the certificate stored
	// was `TRUSTED(component, universe, model_hash)` - and `level` had nothing to compare a change
	// against. Evidence gathered over source edits reached through `link.static` then answered for a
	// rename reached through `generation.build`, a combination no shadow comparison had ever seen.
	// A grant that names its own scope and a check that ignores it is a report, not a bound.
	//
	// Defaulted empty for certificates written before the field, which covers nothing but an empty
	// requirement - the same failing-closed rule `model_self_check` applies to records.
	#[serde(default)]
	pub scope: crate::shadow::Scope,
	pub granted_at: u64,
	pub note: String,
}

// One line of `trust`'s report: what a certificate says, and what it says it about. A certificate
// from a stale model carries no scope, because it describes a system that is not running.
#[derive(Clone, Debug)]
pub struct Summary {
	pub level: Level,
	pub scope: Option<crate::shadow::Scope>,
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

// What a component's clean evidence actually covers, beside how much of it there is.
//
// A certificate says "this component's scoped runs can be believed"; the design's criteria say that
// sentence needs a scope. These are the parts of it the records can answer today.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct EvidenceScope {
	pub records: usize,
	// The classes of change the clean evidence was made over and the graph edges it walked out of
	// this component. Five perfect verifications of one class are not evidence about another.
	pub scope: crate::shadow::Scope,
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
	//
	// AND NEITHER IS ONE EARNED OVER A DIFFERENT KIND OF CHANGE. `needed` is the scope of the change
	// being judged - its classes of edit, and the graph edges the selector walked out of this
	// component to reach what it selected. A certificate answers for it only when it covers both.
	// Pass `Scope::default()` where there is no change in hand, which is every reporting caller: an
	// empty requirement asks nothing and is covered by anything.
	pub fn level(&self, component: &str, model_hash: &str, universe: crate::shadow::Universe, needed: &crate::shadow::Scope) -> Level {
		match self.certificates.iter().find(|certificate| certificate.component == component && certificate.universe == universe) {
			Some(certificate) if certificate.model_hash == model_hash && certificate.scope.covers(needed) => Level::Trusted,
			_ => Level::Shadow,
		}
	}

	// Why a certificate did not answer for this change, for a runner that has to say what it is
	// falling back to shadow FOR.
	pub fn shortfall(&self, component: &str, model_hash: &str, universe: crate::shadow::Universe, needed: &crate::shadow::Scope) -> Vec<String> {
		match self.certificates.iter().find(|certificate| certificate.component == component && certificate.universe == universe) {
			Some(certificate) if certificate.model_hash == model_hash => certificate.scope.shortfall(needed),
			_ => Vec::new(),
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
	pub fn trusted_everywhere(&self, component: &str, model_hash: &str, universes: &[crate::shadow::Universe], needed: &crate::shadow::Scope) -> bool {
		if universes.is_empty() {
			return false;
		}
		universes.iter().all(|universe| self.level(component, model_hash, *universe, needed) == Level::Trusted)
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
	//
	// THE COMPONENT-WIDE CHECK IS A PRECONDITION, NOT THE GRANT. It answers cheaply and with a
	// readable message when there is plainly not enough evidence yet; what the certificate may
	// actually CLAIM is decided per scope pair by `evidence_scope`, because a threshold met once
	// over the union grants combinations no five distinct decisions ever backed. The final check
	// below is that at least one pair survived that grading - a component with five clean runs
	// spread across five different kinds of change qualifies for none of them, and must be told so
	// rather than handed an empty certificate.
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
			return Err(format!("{clean} clean comparison(s) but only {distinct} distinct one(s) - {REQUIRED_CLEAN_RUNS} different DECISIONS about this component are needed, because the selector is deterministic and the same decision validated again is one piece of evidence however many trees it was asked from"));
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
		// AND AT LEAST ONE SCOPE PAIR SURVIVES THE SAME GRADING ON ITS OWN.
		let candidates = log.candidate_pairs(component, model_hash, universe);
		let mut reasons: Vec<String> = Vec::new();
		let mut qualified = 0usize;
		for pair in &candidates {
			match pair_qualifies(log, component, model_hash, universe, pair) {
				Ok(()) => qualified += 1,
				Err(reason) => reasons.push(format!("{}: {reason}", describe_pair(pair))),
			}
		}
		if qualified == 0 {
			return Err(format!("the component has enough evidence overall and none of its {} scope pair(s) does - a certificate promises per pair, and evidence spread across kinds of change is not evidence about any one of them: {}", candidates.len(), reasons.join("; ")));
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
	// Carried onto the certificate rather than printed beside it. It used to be computed AFTER the
	// grant and written to stdout, which made the scope a report about a certificate that did not
	// contain it - so `level` could not consult it and the change being judged was never compared
	// against what the evidence had covered.
	//
	// PER COMPONENT, from the record's own `component_scopes`. The global `change_kinds` and
	// `edge_kinds` describe the whole change set, so a commit that renamed a file in a neighbour
	// widened this component's scope to cover renames it was never validated over - the same free
	// neighbour that distinctness had to be keyed away from. A record written before those existed
	// contributes nothing, which fails closed.
	// AND EVERY PAIR IN IT MEETS THE THRESHOLD ON ITS OWN.
	//
	// The union used to be taken over ALL clean records and the threshold checked once, globally -
	// so five `modified/link.static` records qualified the component and one
	// `renamed/generation.build` record, on one target, with no execution sample, extended the
	// certificate to cover `renamed/generation.build`. The grant then promised something no five
	// distinct decisions had ever backed.
	//
	// So each candidate pair is graded by `pair_qualifies` against the records that EXHIBIT it, and
	// only the ones that pass go into the scope. A component with plenty of evidence about one kind
	// of change and a single record about another now gets a certificate covering the first and
	// silent about the second, which is what the evidence says.
	pub fn evidence_scope(&self, component: &str, model_hash: &str, universe: crate::shadow::Universe, log: &Log) -> EvidenceScope {
		let mut all_self_checked = true;
		let mut records = 0usize;
		for record in log.records.iter().filter(|record| Log::is_clean(record, component, model_hash, universe)) {
			records += 1;
			all_self_checked &= record.model_self_check;
		}
		let mut qualified: BTreeSet<String> = BTreeSet::new();
		let mut architectures: BTreeSet<String> = BTreeSet::new();
		for pair in log.candidate_pairs(component, model_hash, universe) {
			if pair_qualifies(log, component, model_hash, universe, &pair).is_ok() {
				architectures.extend(log.clean_architectures_for_pair(component, model_hash, universe, Some(&pair)));
				qualified.insert(pair);
			}
		}
		// The targets a certificate may claim are the ones the QUALIFYING evidence ran on, not every
		// target that ever produced a clean record: a pair that did not earn its place should not
		// contribute the architecture it happened to run on either.
		EvidenceScope { records, scope: crate::shadow::Scope::from_pairs(qualified).with_architectures(architectures.into_iter().collect()), all_self_checked }
	}

	// `architectures` is the DISPLAY copy of `scope.architectures` and is written from it, so the
	// number a reader sees and the number `covers` checks cannot drift apart - which is the shape of
	// every accounting defect this tree has found.
	pub fn grant(&mut self, component: &str, model_hash: &str, universe: crate::shadow::Universe, clean_runs: usize, _architectures: Vec<String>, scope: crate::shadow::Scope, at: u64) {
		let architectures = scope.architectures.clone();
		self.certificates.retain(|certificate| !(certificate.component == component && certificate.universe == universe));
		self.certificates.push(Certificate { component: component.to_string(), universe, model_hash: model_hash.to_string(), clean_runs, architectures, scope, granted_at: at, note: String::from("earned by clean dry-shadow comparisons under this model hash, in this universe alone, over the (change kind, edge kind) pairs in `scope` and on the targets it names, and nothing beside them; it lapses the moment the hash moves") });
		self.certificates.sort_by(|left, right| (&left.component, left.universe).cmp(&(&right.component, right.universe)));
	}

	// The level AND the scope it is a level FOR.
	//
	// This answered a bare `Level`, so a reader saw `audio (TestGuest): Trusted` with nothing saying
	// the trust covers modifications reached through static links and nothing else - the exact
	// misreading the scope exists to prevent, produced by the function whose job is to report it.
	pub fn summary(&self, model_hash: &str) -> BTreeMap<String, Summary> {
		self.certificates
			.iter()
			.map(|certificate| {
				let current = certificate.model_hash == model_hash;
				(format!("{} ({:?})", certificate.component, certificate.universe), Summary { level: if current { Level::Trusted } else { Level::Shadow }, scope: current.then(|| certificate.scope.clone()) })
			})
			.collect()
	}
}

// A readable name for a `change kind\tedge kind` pair.
fn describe_pair(pair: &str) -> String {
	match pair.split_once('\t') {
		Some((change, "")) => format!("change kind '{change}'"),
		Some(("", edge)) => format!("edge kind '{edge}'"),
		Some((change, edge)) => format!("'{change}' through '{edge}'"),
		None => format!("'{pair}'"),
	}
}

// Whether the records that exhibit ONE scope pair meet the whole threshold on their own.
//
// The same four questions `evaluate` asks about a component - enough clean comparisons, enough
// DISTINCT decisions among them, enough targets, and an execution sample where the universe has one
// - restricted to the records that actually cover this pair. That restriction is the finding: the
// threshold was evaluated once over the union and the scope was then widened past it, so a single
// record about a different kind of change rode into the grant on evidence that was about something
// else.
fn pair_qualifies(log: &Log, component: &str, model_hash: &str, universe: crate::shadow::Universe, pair: &str) -> Result<(), String> {
	let clean = log.clean_runs_for_pair(component, model_hash, universe, Some(pair));
	if clean < REQUIRED_CLEAN_RUNS {
		return Err(format!("{clean} clean comparison(s), {REQUIRED_CLEAN_RUNS} needed"));
	}
	let distinct = log.distinct_evidence_for_pair(component, model_hash, universe, Some(pair));
	if distinct < REQUIRED_CLEAN_RUNS {
		return Err(format!("{clean} clean comparison(s) but only {distinct} distinct decision(s), {REQUIRED_CLEAN_RUNS} needed"));
	}
	let targets = log.clean_architectures_for_pair(component, model_hash, universe, Some(pair));
	let needed = required_architectures(universe);
	if targets.len() < needed {
		return Err(format!("evidence from {} target(s) ({}), {needed} needed in this universe", targets.len(), targets.iter().cloned().collect::<Vec<String>>().join(", ")));
	}
	if exec_universes(universe) && !log.has_exec_sample_for(component, model_hash, universe, Some(pair)) {
		return Err(String::from("no run covering it has EXECUTED the selection"));
	}
	Ok(())
}
