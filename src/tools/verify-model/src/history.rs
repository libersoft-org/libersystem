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

impl History {
	pub fn path(repo_root: &Path) -> PathBuf {
		repo_root.join(".build/state/verify-history.json")
	}

	pub fn load(repo_root: &Path) -> Result<Self, String> {
		let path = Self::path(repo_root);
		if !path.is_file() {
			return Ok(History { schema: 1, entries: BTreeMap::new(), fixed_overshoot: BTreeMap::new() });
		}
		let text = fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
		// A history that cannot be read is an empty history, not a failure. It records what has run;
		// losing it costs a wider next sweep, which is the safe direction, and refusing to plan
		// because a cache is corrupt would be the unsafe one.
		Ok(serde_json::from_str(&text).unwrap_or(History { schema: 1, entries: BTreeMap::new(), fixed_overshoot: BTreeMap::new() }))
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
		// The aarch64 suite measured 2069 s against a fixed term of 2300. The residual was negative,
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
		if residual < 0.0 {
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
		for key in keys {
			self.record(&key.display(), passed, share, model_hash);
		}
	}

	pub fn record(&mut self, key: &str, passed: bool, seconds: f64, model_hash: &str) {
		let entry = self.entries.entry(key.to_string()).or_default();
		entry.last_run = now();
		entry.last_status = String::from(if passed { "passed" } else { "failed" });
		entry.runs += 1;
		if !passed {
			entry.failures += 1;
		}
		entry.last_seconds = seconds;
		entry.model_hash = model_hash.to_string();
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
// A plain sum of test durations is badly wrong here. Measured on this tree: the fixed term is ~100 s
// on x86_64 and ~1450 s on aarch64, against ~0.2 s and ~7 s per test - so a selection that is 90%
// smaller is a run that is 15% cheaper. Escalating on a COUNT would conclude that scoping the x86_64
// suite is worth the bookkeeping, and it is not; what is worth it is removing a boot.
//
// These are the starting constants, measured under load 115. Every recorded run refines the variable
// term; the fixed term is what is left when the variable one is subtracted, and it is deliberately
// not learned from a single sample.
pub struct CostModel {
	pub fixed_seconds: BTreeMap<(String, String), f64>,
	pub default_variable: f64,
}

impl Default for CostModel {
	fn default() -> Self {
		let mut fixed = BTreeMap::new();
		// aarch64 measured 2300 s for 216 tests on 2026-08-10, so 1450 is now well under it. riscv64
		// declared 217 tests and completed 82 in its 2700 s budget that same day - one of them alone
		// took 707 s - and that run competed with concurrent builds, so its extrapolation is a
		// pessimistic bound rather than a clean figure. 3200 is the clean-rate projection from the
		// 2026-08-06 measurement (150 tests in 2161 s) carried to today's count; the gate in
		// `self_check` compares it against the harness budget so neither can drift alone again.
		for (architecture, environment, seconds) in [("x86_64", "test-guest", 100.0), ("aarch64", "test-guest", 2300.0), ("riscv64", "test-guest", 3200.0), ("x86_64", "dev-guest", 120.0), ("host", "host", 0.0)] {
			fixed.insert((architecture.to_string(), environment.to_string()), seconds);
		}
		CostModel { fixed_seconds: fixed, default_variable: 0.5 }
	}
}

impl CostModel {
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
			variable += match history.get(&key.display()) {
				Some(record) if record.last_seconds > 0.0 => record.last_seconds,
				_ => self.default_variable,
			};
		}
		let fixed: f64 = pairs.iter().map(|pair| self.fixed_seconds.get(pair).copied().unwrap_or(0.0)).sum();
		fixed + variable
	}
}
