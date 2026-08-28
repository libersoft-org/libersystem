// What has run, when, on which target, in which environment and in which configuration.
//
// Keyed on `PlanItemKey` and nothing shorter. A test ID alone loses the architecture; adding the
// architecture still loses the environment; all three still lose the configuration - and the
// architecture policy deliberately boots ONE target for ordinary userspace changes, so the steady
// state it produces is x86_64 fresh while the other two age. A bound keyed on the test would report
// the whole suite as fresh while two targets rot.
//
// The universe is the catalog: age ranges over keys the catalog says exist, never over keys the
// history happens to contain. A key that stopped existing is dropped rather than reported as
// eternally stale, and a key that exists and has never run is the most stale thing there is.

use crate::plan::PlanItemKey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Record {
	// Seconds since the epoch. Stored as a number rather than a formatted date because the only
	// question ever asked of it is "how long ago", and a format is a second thing to get wrong.
	pub last_run: u64,
	pub last_status: String,
	pub runs: u64,
	pub failures: u64,
	pub last_seconds: f64,
	// The model this evidence was produced under. A record made by a different selector over a
	// different graph does not describe what runs today, which is the same argument that makes
	// TRUSTED a certificate rather than a property of a name.
	pub model_hash: String,
	// WHETHER THIS COST WAS INVENTED BY DIVISION.
	//
	// `record_step` takes the fixed term off the top and splits what remains EVENLY over the keys a
	// step discharged. For a boot that is an approximation which adds up to the truth. For a MERGED
	// step - fifty-six gates in one `check.sh` call - it is one duration divided fifty-six ways, and
	// every per-gate figure on disk is an artefact of how the gates happened to be batched. Ordering
	// on those is sorting on the batching.
	//
	// Recorded rather than inferred, because after the fact nothing distinguishes a real measurement
	// from a divided one, and a migration that has to guess would throw away the good ones too.
	#[serde(default)]
	pub cost_was_divided: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct History {
	pub schema: u32,
	pub entries: BTreeMap<String, Record>,
	// How far under its fixed term a run has come in, per `architecture/environment`. See
	// `record_step`: this is the observation that used to be clamped away, and it is the only thing
	// in this file that can say a constant in `CostModel::default` is wrong.
	//
	// `#[serde(default)]` so the histories already on disk load and read as "never overshot".
	#[serde(default)]
	pub fixed_overshoot: BTreeMap<String, f64>,
}

// The history file's format version. Bumped when a recorded value stops meaning what it meant.
pub const SCHEMA: u32 = 3;

impl History {
	pub fn path(repo_root: &Path) -> PathBuf {
		repo_root.join(".build/state/verify-history.json")
	}

	pub fn load(repo_root: &Path) -> Result<Self, String> {
		let path = Self::path(repo_root);
		if !path.is_file() {
			return Ok(History { schema: SCHEMA, entries: BTreeMap::new(), fixed_overshoot: BTreeMap::new() });
		}
		let text = fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
		// A history that cannot be read is an empty history, not a failure. It records what has run;
		// losing it costs a wider next sweep, which is the safe direction, and refusing to plan
		// because a cache is corrupt would be the unsafe one.
		let mut history: History = serde_json::from_str(&text).unwrap_or(History { schema: SCHEMA, entries: BTreeMap::new(), fixed_overshoot: BTreeMap::new() });
		// EVIDENCE RECORDED UNDER A RULE THAT WAS WRONG IS NOT EVIDENCE. Schema 1 recorded an
		// overshoot for any step that came in under its fixed term, including steps that did not run
		// at all - see `record_step`. The per-key records are unaffected and are kept; the overshoot
		// is dropped, because it is a claim about a constant and the observations behind it were not
		// observations.
		if history.schema < SCHEMA {
			history.fixed_overshoot.clear();
			// AND EVERY COST WRITTEN BEFORE THE MARKER EXISTED GOES WITH IT.
			//
			// `cost_was_divided` is recorded rather than inferred, because after the fact nothing
			// distinguishes a real measurement from a merged step's duration divided by how many
			// things were batched into it. A record written before schema 3 carries no marker
			// and therefore cannot be told apart - so its COST is dropped once, here, and the run
			// that measures it again writes a figure that says which kind it is.
			//
			// ONLY THE COST. When the key last ran and whether it passed are still true, and
			// throwing those away would turn a cost migration into a freshness reset - every key
			// suddenly overdue for a reason that has nothing to do with whether it ran.
			for entry in history.entries.values_mut() {
				entry.last_seconds = 0.0;
			}
			history.schema = SCHEMA;
		}
		Ok(history)
	}

	pub fn save(&self, repo_root: &Path) -> Result<(), String> {
		let path = Self::path(repo_root);
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
		}
		let text = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
		fs::write(&path, text).map_err(|error| format!("{}: {error}", path.display()))
	}

	// A STEP's wall clock, distributed over the keys it discharged.
	//
	// Recording the whole duration against every key was the first thing tried and it is badly
	// wrong: one 110-second kernel boot became 199 keys each claiming 110 seconds, and the estimator
	// summed them into six hours. The decomposition that matches the cost model is
	// `duration = fixed(architecture, environment) + sum(variable)`, so the fixed term comes off the
	// top once and what remains is split evenly. Evenly is an approximation - the tests inside a boot
	// do not cost the same - but it is an approximation that adds up to the truth, which the
	// alternative did not.
	pub fn record_step(&mut self, keys: &[PlanItemKey], passed: bool, step_seconds: f64, model_hash: &str, cost: &CostModel) {
		if keys.is_empty() {
			return;
		}
		let mut pairs: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
		for key in keys {
			pairs.insert((key.architecture.clone(), key.environment.as_str().to_string()));
		}
		let fixed: f64 = pairs.iter().map(|pair| cost.fixed_seconds.get(pair).copied().unwrap_or(0.0)).sum();
		// A NEGATIVE RESIDUAL IS A MEASUREMENT, and clamping it to zero is what stopped this history
		// from ever contradicting the constant above it.
		//
		// The aarch64 suite measured 2069 s against the fixed term OF THE TIME, 2300 - a figure the
		// nine-run calibration has since replaced, kept here because it is what the defect below
		// happened to. The residual was negative,
		// `.max(0.0)` made it zero, every key recorded zero seconds, and `estimate` then ignored the
		// record because it requires `last_seconds > 0.0` and fell back to the 0.5 s default. So the
		// two targets whose costs actually decide whether a selection stays scoped were the two the
		// history could never learn anything about - and the one observation that would have said
		// "the fixed term is too high" was the observation being discarded.
		//
		// It is recorded instead: the overshoot goes into `fixed_overshoot`, keyed by the pair it
		// was measured for, and the per-key share stays zero because there is genuinely nothing left
		// to distribute. `verify-model cost` reports it, which is how a constant that is wrong
		// becomes visible rather than merely inert.
		let residual = step_seconds - fixed;
		// A STEP THAT DID NOT RUN IS NOT A MEASUREMENT OF WHAT RUNNING COSTS.
		//
		// The overshoot was recorded for any negative residual, and `verify.sh` records whatever
		// `$((SECONDS - started))` came to - so a step that failed at its first instruction, or one
		// whose runner short-circuited, recorded ZERO seconds and the whole fixed term became
		// "evidence" that the constant is a whole-suite figure. That is what the three entries on
		// disk were: two of them equal to their fixed term exactly, which is a step of zero seconds,
		// and the third an x86_64 guest step of a second and a half, which is less than the time it
		// takes to boot one.
		//
		// So the conclusion needs a step that ran AND finished: `passed`, because a failure may have
		// died before doing the work, and a positive duration, because a suite cannot take no time.
		// The residual is still recorded as zero share below either way - what changes is only
		// whether the run is allowed to contradict a constant.
		if residual < 0.0 && passed && step_seconds > 0.0 {
			for pair in &pairs {
				let seen = self.fixed_overshoot.entry(format!("{}/{}", pair.0, pair.1)).or_insert(0.0);
				// The LARGEST overshoot seen, not the last: the fixed term is a floor on what a run
				// costs, so the run that came in furthest under it is the strongest evidence about
				// how far off the constant is.
				if -residual > *seen {
					*seen = -residual;
				}
			}
		}
		let share = residual.max(0.0) / keys.len() as f64;
		// A KEY'S SHARE IS A MEASUREMENT ONLY WHERE THE THING IT NAMES COULD HAVE BEEN SCHEDULED ON
		// ITS OWN. Two hundred kernel tests inside one boot cannot: their share is an approximation
		// that adds up to the truth, and it is the best number available. Fifty-six gates inside one
		// `check.sh` call could each have been a step - `check.sh --gate <one>` is a command - so
		// their share is one duration divided by how they happened to be batched.
		let divided = keys.len() > 1 && keys.iter().any(|key| key.environment == crate::catalog::Environment::Host);
		for key in keys {
			self.record_with(&key.display(), passed, share, model_hash, divided);
		}
	}

	pub fn record(&mut self, key: &str, passed: bool, seconds: f64, model_hash: &str) {
		self.record_with(key, passed, seconds, model_hash, false);
	}

	pub fn record_with(&mut self, key: &str, passed: bool, seconds: f64, model_hash: &str, cost_was_divided: bool) {
		let entry = self.entries.entry(key.to_string()).or_default();
		entry.cost_was_divided = cost_was_divided;
		entry.last_run = now();
		entry.last_status = String::from(if passed { "passed" } else { "failed" });
		entry.runs += 1;
		if !passed {
			entry.failures += 1;
		}
		entry.last_seconds = seconds;
		entry.model_hash = model_hash.to_string();
	}

	// DISCARD EVERY COST THAT WAS INVENTED BY DIVISION, and the freshness of the ones that never ran.
	//
	// Ordering cheapest-first on a divided figure is sorting on how the gates happened to be batched,
	// so those costs go before the first such run rather than being inherited by it. ONLY THE COSTS
	// WHERE THE STEP PASSED: a record also carries when a key last ran and whether it did, and
	// throwing that away for a step that really ran every member would turn a cost migration into a
	// freshness reset - every key suddenly overdue for a reason that has nothing to do with running.
	//
	// A FAILED MERGED STEP IS DIFFERENT AND ITS FRESHNESS GOES TOO. `check.sh` runs its gates in
	// order and stops at the first failure, and `record_step` stamps every key of the step. Measured
	// 2026-08-28: `capability-trace` refused, it was the fifth gate of forty-five, and all forty-five
	// were recorded as having run and failed - forty of them never started. Keeping that freshness
	// means forty gates that never ran count as recently checked.
	pub fn discard_divided_costs(&mut self) -> (usize, usize) {
		let (mut costs, mut freshness) = (0, 0);
		self.entries.retain(|_, entry| {
			if !entry.cost_was_divided {
				return true;
			}
			if entry.last_status != "passed" {
				freshness += 1;
				return false;
			}
			if entry.last_seconds > 0.0 {
				entry.last_seconds = 0.0;
				costs += 1;
			}
			entry.cost_was_divided = false;
			true
		});
		(costs, freshness)
	}

	pub fn get(&self, key: &str) -> Option<&Record> {
		self.entries.get(key)
	}

	// Keys the catalog says exist and that no run has covered within the window.
	//
	// Never-run keys come first and are reported with no age at all, because "has not run in 30
	// days" and "has never run" are different facts and only one of them can be fixed by waiting.
	pub fn stale(&self, universe: &[PlanItemKey], window_days: u64) -> Vec<(String, Option<u64>)> {
		let cutoff = now().saturating_sub(window_days * 86_400);
		let mut stale = Vec::new();
		for key in universe {
			let display = key.display();
			match self.entries.get(&display) {
				None => stale.push((display, None)),
				Some(record) if record.last_run < cutoff => {
					let age = now().saturating_sub(record.last_run) / 86_400;
					stale.push((display, Some(age)));
				}
				Some(_) => {}
			}
		}
		stale.sort_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(&right.0)));
		stale
	}

	// Entries for keys the catalog no longer has. Reported rather than deleted: a key that vanished
	// is usually a rename, and a rename that silently zeroes its history is exactly what the rule
	// against deriving IDs from function names exists to prevent.
	pub fn orphans(&self, universe: &[PlanItemKey]) -> Vec<String> {
		let known: std::collections::BTreeSet<String> = universe.iter().map(PlanItemKey::display).collect();
		self.entries.keys().filter(|key| !known.contains(*key)).cloned().collect()
	}
}

pub fn now() -> u64 {
	SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_secs()).unwrap_or(0)
}

// What a selection is expected to cost, as fixed plus variable.
//
// A plain sum of test durations is badly wrong here, and the reason survives every revision of the
// numbers: a run has a fixed cost - a build, a boot - that a smaller selection does not reduce. So a
// selection that is 90% smaller can be a run that is barely cheaper, and escalating on a COUNT would
// conclude that scoping is worth the bookkeeping when what is worth it is removing a boot.
//
// THE NUMBERS ARE IN `Default`, MEASURED. They are not repeated here, and that is the point: an
// earlier version of this comment carried "~100 s on x86_64 and ~1450 s on aarch64, against ~0.2 s
// and ~7 s per test" - true before the nine-run calibration and contradicting the constants twenty
// lines below it afterwards. A careful reader of the milestone came away with a wrong conclusion
// about the state of the work, and the only thing that misled them was this comment.
//
// Every recorded run refines the variable term; the fixed term is what is left when the variable one
// is subtracted, and it is deliberately not learned from a single sample.
pub struct CostModel {
	pub fixed_seconds: BTreeMap<(String, String), f64>,
	// PER TARGET, because the per-test cost is not a property of a test - it is a property of how
	// fast the machine running it goes. One global 0.5 s said an emulated test costs what a native
	// one costs, and it is fifteen to twenty times more.
	pub variable_seconds: BTreeMap<(String, String), f64>,
	// The fallback for a pair nothing was measured for.
	pub default_variable: f64,
	// THE CONSERVATIVE SEED FOR A STEP THAT STANDS IN FOR A WHOLE SUITE, in tests.
	//
	// `guest.whole-suite` runs everything on a target the model could not enumerate, and pricing it
	// as one key - which is what a key with no history gets - makes the most expensive thing in the
	// plan the cheapest thing in the estimate. The count cannot come from the discovery, because the
	// discovery is what failed; it is the number of test ids the SOURCE declares, which is an upper
	// bound and therefore the safe direction. Zero means nobody set it, and the aggregate is then
	// priced like any other key rather than silently as free.
	pub whole_suite_tests: usize,
}

impl Default for CostModel {
	fn default() -> Self {
		// MEASURED, 2026-08-12, nine runs: 2 tests, 20 tests and the whole suite on each target,
		// with `fixed` and `per-test` taken from the least-squares line through the three points.
		//
		//   x86_64    23 s /  26 s /  131 s (240 tests) -> fixed  19.5 s, per-test 0.46 s
		//   aarch64  695 s / 723 s / 2256 s (226 tests) -> fixed 632.3 s, per-test 7.17 s
		//   riscv64  537 s / 587 s / 2600 s (226 tests) -> fixed 460.6 s, per-test 9.44 s
		//
		// Every one of the old numbers was a whole-suite time sitting in a field that means STARTUP
		// cost, and the milestone's own arithmetic is why that mattered: the planner widens a
		// selection to the whole set when `scoped / whole > 0.9`, and with a fixed term that large
		// the ratio was 0.953 on aarch64 and 0.966 on riscv64 for a selection of ONE test. Every
		// scoped selection on the two targets where scoping is worth most therefore ran everything.
		// With these, the same one-test ratio is 0.284 and 0.181.
		//
		// The other half was hidden by the same mistake: one global 0.5 s per test. An emulated test
		// costs fifteen to twenty times a native one, so the model priced the thing that actually
		// differs between targets as if it did not differ at all.
		//
		// The three points per target are NOT collinear, and the residuals say why rather than
		// hiding it: the twenty-test sample overshoots its fit by 53 s on aarch64 and 63 s on
		// riscv64 because a stride sample of twenty happens to miss the handful of tests that
		// dominate the suite. `per-test` is therefore an average over the whole suite and not a
		// prediction for any one test, which is exactly what an estimator summing over a selection
		// needs - and what `record_step` refines per key from real runs.
		let mut fixed = BTreeMap::new();
		for (architecture, environment, seconds) in [("x86_64", "test-guest", 19.5), ("aarch64", "test-guest", 632.0), ("riscv64", "test-guest", 461.0), ("x86_64", "dev-guest", 120.0), ("host", "host", 0.0)] {
			fixed.insert((architecture.to_string(), environment.to_string()), seconds);
		}
		let mut variable = BTreeMap::new();
		for (architecture, environment, seconds) in [("x86_64", "test-guest", 0.46), ("aarch64", "test-guest", 7.17), ("riscv64", "test-guest", 9.44)] {
			variable.insert((architecture.to_string(), environment.to_string()), seconds);
		}
		CostModel { fixed_seconds: fixed, variable_seconds: variable, default_variable: 0.5, whole_suite_tests: 0 }
	}
}

impl CostModel {
	// What a WHOLE suite of `tests` costs on one target: the startup cost once, plus the per-test
	// cost for each.
	//
	// The budget gate used to read `fixed_seconds` alone and call it the suite's cost, which was
	// true only while the fixed term WAS a whole-suite figure. Now that it means what it says, the
	// gate has to add the tests back or it compares a fifteen-minute budget against a ten-minute
	// boot and concludes the suite fits.
	pub fn full_suite_seconds(&self, architecture: &str, environment: &str, tests: usize) -> f64 {
		let pair = (architecture.to_string(), environment.to_string());
		let fixed = self.fixed_seconds.get(&pair).copied().unwrap_or(0.0);
		let variable = self.variable_seconds.get(&pair).copied().unwrap_or(self.default_variable);
		fixed + variable * tests as f64
	}

	// `cost(architecture, environment, selection) = fixed(architecture, environment) + sum(items)`.
	//
	// The fixed term is charged ONCE per (architecture, environment) pair that appears, which is the
	// whole point: two hundred selected kernel tests are one boot. Charging it per item would make
	// every estimate proportional to the count again and reproduce the error being corrected.
	pub fn estimate(&self, history: &History, items: &[PlanItemKey]) -> f64 {
		let mut pairs: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
		let mut variable = 0.0;
		for key in items {
			pairs.insert((key.architecture.clone(), key.environment.as_str().to_string()));
			let per_test = self.variable_seconds.get(&(key.architecture.clone(), key.environment.as_str().to_string())).copied().unwrap_or(self.default_variable);
			variable += match history.get(&key.display()) {
				// A MEASURED COST, AND NOT ONE THAT WAS INVENTED BY DIVIDING.
				//
				// `record_step` splits a merged step's wall clock evenly across every key it
				// discharged, and marks each entry `cost_was_divided` for exactly this reason - but
				// the estimator read `last_seconds` without looking at the marker, so an eight-way
				// split of one gate step came back as eight per-key measurements and the
				// cheapest-first order sorted on how the steps had happened to be batched. The
				// marker is only worth recording if something reads it. A divided entry falls back
				// to the measured per-test cost for its own target, which is a worse estimate of one
				// key and a much better one than a number that describes a batch.
				Some(record) if record.last_seconds > 0.0 && !record.cost_was_divided => record.last_seconds,
				// The aggregate is not one test. Seeded from what the source declares, because the
				// target it stands for is the one whose tests could not be counted.
				_ if key.check == "guest.whole-suite" && self.whole_suite_tests > 0 => per_test * self.whole_suite_tests as f64,
				// The measured per-test cost for THIS target, not one number for all of them.
				_ => per_test,
			};
		}
		let fixed: f64 = pairs.iter().map(|pair| self.fixed_seconds.get(pair).copied().unwrap_or(0.0)).sum();
		fixed + variable
	}
}
