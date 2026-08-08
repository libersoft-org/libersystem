// The model behind ./verify.sh: what exists, what depends on what, and what a change reaches.
//
// Nothing here runs a test. It answers one question - given these changed paths, which
// `PlanItemKey`s must run - and it answers it the same way every time, so shadow mode, the
// regression corpus, the cost estimator, the age scheduler and the person at the terminal all
// consult ONE selector rather than four that drift.

pub mod catalog;
pub mod commands;
pub mod crates;
pub mod graph;
pub mod history;
pub mod kerneltests;
pub mod ownership;
pub mod plan;
pub mod registry;

#[cfg(test)]
mod tests;

use catalog::Catalog;
use crates::Crate;
use graph::Graph;
use ownership::Ownership;
use registry::Registry;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

// Bumped deliberately when the selection ALGORITHM changes in a way that could alter a plan.
//
// Part of `model_hash`, so bumping it demotes every TRUSTED component back to SHADOW. That is the
// point: evidence proves that a particular selector over a particular model did not miss anything,
// and a new selector has no evidence yet no matter how clean the old record looked.
pub const SELECTOR_VERSION: u32 = 1;

pub struct Model {
	pub repo_root: PathBuf,
	pub registry: Registry,
	pub crates: Vec<Crate>,
	pub manifest: system_manifest::Manifest,
	pub graph: Graph,
	pub catalog: Catalog,
	pub kernel_tests: kerneltests::Discovery,
}

impl Model {
	pub fn load(repo_root: &Path) -> Result<Self, String> {
		let registry = Registry::load(&repo_root.join("src/tools/verify-model/model"))?;
		let crates = crates::discover(repo_root)?;
		let manifest = system_manifest::Manifest::load_workspace(&repo_root.join("src")).map_err(|error| format!("services/manifest.toml: {error}"))?;
		let graph = Graph::build(&crates, &manifest, &registry);
		graph.validate(&crates, &registry)?;
		let kernel_tests = kerneltests::discover(repo_root, &registry::ARCHITECTURES)?;
		let catalog = Catalog::build(&crates, &registry, &graph, &kernel_tests.tests);
		catalog.validate(&registry)?;
		Ok(Model { repo_root: repo_root.to_path_buf(), registry, crates, manifest, graph, catalog, kernel_tests })
	}

	pub fn ownership(&self) -> Ownership<'_> {
		Ownership::new(&self.registry, &self.crates)
	}

	// One hash over everything that can change a selection.
	//
	// One, not a list of eight: a list invites a ninth input to be added later and not hashed. And
	// it is over CONTENT, never over shape - turning `covers: [flac]` into `covers: []` leaves
	// every schema identical and must still destroy the evidence that depended on it. The
	// configuration catalog is the subtle one: `shared-image` is a label whose meaning lives in
	// Cargo.toml, so adding a feature to it changes what is tested while the label stands still.
	pub fn model_hash(&self) -> String {
		let mut hasher = Sha256::new();
		hasher.update(b"verify-model/1\n");
		hasher.update(SELECTOR_VERSION.to_le_bytes());
		hasher.update(b"\nregistry\n");
		hasher.update(self.registry.registry_text.as_bytes());
		hasher.update(b"\nconfigurations\n");
		hasher.update(self.registry.configurations_text.as_bytes());
		hasher.update(b"\ngraph\n");
		for edge in &self.graph.edges {
			hasher.update(format!("{} {} {}\n", edge.from, edge.kind, edge.to).as_bytes());
		}
		hasher.update(b"\ncatalog\n");
		for check in &self.catalog.checks {
			hasher.update(format!("{} {:?} covers={}\n", check.id, check.kind, check.covers.join("+")).as_bytes());
			for variant in &check.variants {
				hasher.update(format!("  {} {} {}\n", variant.architecture, variant.environment.as_str(), variant.configuration).as_bytes());
			}
		}
		format!("{:x}", hasher.finalize())
	}

	// Every tracked file is owned by exactly one component or explicitly marked not code.
	pub fn unowned_paths(&self) -> Result<Vec<String>, String> {
		let ownership = self.ownership();
		let tracked = tracked_files(&self.repo_root)?;
		Ok(ownership.unowned(&tracked))
	}
}

pub fn tracked_files(repo_root: &Path) -> Result<Vec<String>, String> {
	let output = std::process::Command::new("git").arg("-C").arg(repo_root).arg("ls-files").output().map_err(|error| format!("git ls-files: {error}"))?;
	if !output.status.success() {
		return Err(format!("git ls-files failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
	}
	Ok(String::from_utf8_lossy(&output.stdout).lines().map(str::to_string).filter(|line| !line.is_empty()).collect())
}
