// One place decides what a path belongs to.
//
// Two sources compete on the same footing - the declared rules in registry.toml and plain crate
// containment - and the longest match wins. That is what lets `src/kernel/arch/riscv64` be its own
// component while still living inside the `kernel` crate rooted at `src/kernel`, which the
// architecture policy depends on completely.
//
// The third outcome is the important one: a path matching NOTHING is unknown, and unknown reach
// selects everything. That is not a fallback, it is the invariant.

use crate::crates::Crate;
use crate::registry::{Registry, prefix_match};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Owner {
	// The path belongs to this component.
	Component { component: String, rule: String },
	// The path is declared not to be code: it selects nothing.
	NonCode { reason: String },
	// Nobody claims it. Selects everything.
	Unknown,
}

pub struct Ownership<'a> {
	registry: &'a Registry,
	crate_dirs: Vec<(String, String)>,
	// A `[[bin]]`'s own source file, which is a longer match than the crate around it - so
	// `src/user/apps/tools/src/audioconv.rs` is `bin.audioconv` while everything else in that crate
	// is `tools`. Without this, one of fifty-odd tools sharing a crate would be all fifty.
	binaries: Vec<(String, String)>,
}

impl<'a> Ownership<'a> {
	pub fn new(registry: &'a Registry, crates: &[Crate]) -> Self {
		let mut crate_dirs: Vec<(String, String)> = crates.iter().map(|entry| (entry.dir.clone(), entry.name.clone())).collect();
		crate_dirs.sort();
		let mut binaries: Vec<(String, String)> = crates.iter().flat_map(|entry| entry.binaries.iter().map(|binary| (binary.path.clone(), crate::graph::binary_component(&binary.name)))).collect();
		binaries.sort();
		Ownership { registry, crate_dirs, binaries }
	}

	// EVERY PATH THIS RESOLVER DECIDES BY, which is what a caller has to probe to see a resolution
	// change (added 2026-09-04). `owner` answers by longest prefix over four lists, so the places an
	// answer can change are exactly the paths in those lists - a caller comparing two models has to
	// ask at each of them, and comparing rule TEXTS instead is the trap this whole comparison was
	// corrected for twice.
	pub fn rule_paths(&self) -> Vec<&str> {
		let mut out: Vec<&str> = Vec::new();
		out.extend(self.registry.non_code.iter().map(|rule| rule.path.as_str()));
		out.extend(self.registry.ownership.iter().map(|rule| rule.path.as_str()));
		out.extend(self.crate_dirs.iter().map(|(dir, _)| dir.as_str()));
		out.extend(self.binaries.iter().map(|(file, _)| file.as_str()));
		out
	}

	pub fn owner(&self, path: &str) -> Owner {
		let path = path.trim_start_matches("./");
		let mut best_len: Option<usize> = None;
		let mut best = Owner::Unknown;

		// Not code is a rule like any other and competes by length, so a code file inside a
		// documented directory can still be claimed back by a longer, more specific rule.
		for rule in &self.registry.non_code {
			if let Some(len) = prefix_match(&rule.path, path)
				&& best_len.is_none_or(|best| len > best)
			{
				best_len = Some(len);
				best = Owner::NonCode { reason: rule.reason.clone() };
			}
		}
		for rule in &self.registry.ownership {
			if let Some(len) = prefix_match(&rule.path, path)
				&& best_len.is_none_or(|best| len > best)
			{
				best_len = Some(len);
				best = Owner::Component { component: rule.component.clone(), rule: rule.path.clone() };
			}
		}
		for (dir, name) in &self.crate_dirs {
			if let Some(len) = prefix_match(dir, path)
				&& best_len.is_none_or(|best| len > best)
			{
				best_len = Some(len);
				best = Owner::Component { component: name.clone(), rule: dir.clone() };
			}
		}
		for (file, name) in &self.binaries {
			if let Some(len) = prefix_match(file, path)
				&& best_len.is_none_or(|best| len > best)
			{
				best_len = Some(len);
				best = Owner::Component { component: name.clone(), rule: file.clone() };
			}
		}
		best
	}

	// Every tracked file is owned by exactly one component or explicitly marked not code.
	//
	// This is the gate behind "one place decides what a path belongs to". Without it the model
	// degrades silently in the safe-looking direction: unowned paths fail open to the full suite,
	// every run stays green, and the fact that a whole subtree stopped being understood shows up
	// only as a bill nobody can explain.
	pub fn unowned(&self, paths: &[String]) -> Vec<String> {
		paths.iter().filter(|path| matches!(self.owner(path), Owner::Unknown)).cloned().collect()
	}
}
