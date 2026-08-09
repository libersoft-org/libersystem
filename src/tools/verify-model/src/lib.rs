// The model behind ./verify.sh: what exists, what depends on what, and what a change reaches.
//
// Nothing here runs a test. It answers one question - given these changed paths, which
// `PlanItemKey`s must run - and it answers it the same way every time, so shadow mode, the
// regression corpus, the cost estimator, the age scheduler and the person at the terminal all
// consult ONE selector rather than four that drift.

pub mod archrisk;
pub mod catalog;
pub mod changes;
pub mod commands;
pub mod crates;
pub mod graph;
pub mod history;
pub mod kerneltests;
pub mod ownership;
pub mod plan;
pub mod registry;
pub mod shadow;
pub mod trust;

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
	// Which components can behave differently per target, found by scanning rather than declared.
	pub arch_risk: std::collections::BTreeMap<String, archrisk::Risk>,
}

impl Model {
	pub fn load(repo_root: &Path) -> Result<Self, String> {
		let registry = Registry::load(&repo_root.join("src/tools/verify-model/model"))?;
		let crates = crates::discover(repo_root)?;
		let manifest = system_manifest::Manifest::load_workspace(&repo_root.join("src")).map_err(|error| format!("services/manifest.toml: {error}"))?;
		let graph = Graph::build(&crates, &manifest, &registry);
		graph.validate(&crates, &registry)?;
		let kernel_tests = kerneltests::discover(repo_root, &registry::ARCHITECTURES)?;
		let staged = staged_components(&manifest, &crates, &graph);
		let catalog = Catalog::build(&crates, &registry, &graph, &staged, &kernel_tests.tests);
		catalog.validate(&registry)?;
		let arch_risk = archrisk::scan(repo_root, &Ownership::new(&registry, &crates))?;
		Ok(Model { repo_root: repo_root.to_path_buf(), registry, crates, manifest, graph, catalog, kernel_tests, arch_risk })
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
		// What every feature MEANS. The configuration catalog is hashed by content and says
		// `features = ["shared-image"]`; what that name expands to lives in each Cargo.toml, and
		// adding a member to it changes the shipping build while leaving the catalog, the graph and
		// therefore the hash unmoved. Same failure as hashing a schema instead of its declarations.
		hasher.update(b"\nfeatures\n");
		for entry in &self.crates {
			for (feature, members) in &entry.feature_definitions {
				hasher.update(format!("{} {feature}={}\n", entry.name, members.join("+")).as_bytes());
			}
		}
		hasher.update(b"\narch-risk\n");
		for (component, risk) in &self.arch_risk {
			hasher.update(format!("{component} {} {}\n", risk.any_target, risk.targets.iter().cloned().collect::<Vec<_>>().join("+")).as_bytes());
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

// Every component the system volume is assembled from: the closure of each staged program and
// library over the edges that can change a shipped byte.
//
// One computation, used twice - by the catalog, so packaging and volume assembly declare their real
// inputs, and by the VOLUME_SOURCES gate, so `lib.sh`'s staleness digest cannot fall behind it.
pub fn staged_components(manifest: &system_manifest::Manifest, crates: &[Crate], graph: &Graph) -> std::collections::BTreeSet<String> {
	let mut owners: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
	for program in manifest.programs.values() {
		owners.insert(program.owner.as_str());
	}
	for library in manifest.libraries.values() {
		owners.insert(library.owner.as_str());
	}
	let mut components = std::collections::BTreeSet::new();
	for source in manifest.sources.values() {
		if !owners.contains(source.owner.as_str()) {
			continue;
		}
		let dir = format!("src/{}", source.path.as_str());
		let Some(entry) = crates.iter().find(|entry| entry.dir == dir) else { continue };
		components.extend(graph.reaches(&entry.name, &["link.static", "link.dynamic", "generation.build"]));
		// And the PROGRAMS that crate builds. A tool's own source file resolves to its `bin.`
		// component, which is a longer prefix than the crate directory - so without this a change to
		// `audioconv.rs` compiled the tool and never staged it, and the guest booted the previous
		// one out of a volume the staleness check was happy with.
		for binary in &entry.binaries {
			components.extend(graph.reaches(&crate::graph::binary_component(&binary.name), &["link.static", "link.dynamic", "generation.build"]));
		}
	}
	components
}

pub fn tracked_files(repo_root: &Path) -> Result<Vec<String>, String> {
	let output = std::process::Command::new("git").arg("-C").arg(repo_root).arg("ls-files").output().map_err(|error| format!("git ls-files: {error}"))?;
	if !output.status.success() {
		return Err(format!("git ls-files failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
	}
	Ok(String::from_utf8_lossy(&output.stdout).lines().map(str::to_string).filter(|line| !line.is_empty()).collect())
}
