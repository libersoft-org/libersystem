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
fn test_guest_universe() -> crate::shadow::Universe {
	crate::shadow::Universe::TestGuest
}

pub const REQUIRED_CLEAN_RUNS: usize = 5;
pub const REQUIRED_ARCHITECTURES: usize = 2;

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

	// A component is only fully trusted when every universe that can judge it has said so.
	pub fn trusted_everywhere(&self, component: &str, model_hash: &str) -> bool {
		[crate::shadow::Universe::Host, crate::shadow::Universe::TestGuest].into_iter().all(|universe| self.level(component, model_hash, universe) == Level::Trusted)
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
		if architectures.len() < REQUIRED_ARCHITECTURES {
			return Err(format!("evidence from {} target(s) ({}), {REQUIRED_ARCHITECTURES} needed - a component validated on one target says nothing about the others", architectures.len(), architectures.join(", ")));
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
