// The declared half of the model: what no file in the tree already states.
//
// Everything derivable is derived elsewhere - `crates.rs` reads the Cargo manifests, `graph.rs`
// reads services/manifest.toml. What lands here is ownership of paths outside any crate, the edges
// a linker cannot see, which targets a change can affect, and what is not code at all.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub const ARCHITECTURES: [&str; 3] = ["x86_64", "aarch64", "riscv64"];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRegistry {
	schema: u32,
	#[serde(default)]
	ownership: Vec<RawOwnership>,
	#[serde(default)]
	non_code: Vec<RawNonCode>,
	#[serde(default)]
	host_tests_unrunnable: Vec<RawUnrunnable>,
	#[serde(default)]
	host_configuration_unrunnable: Vec<RawConfigurationUnrunnable>,
	#[serde(default)]
	selects_everything: Vec<RawSelectsEverything>,
	#[serde(default)]
	risk_class: Vec<RawRiskClass>,
	#[serde(default)]
	change_group: Vec<RawChangeGroup>,
	#[serde(default)]
	edge: Vec<RawEdge>,
	#[serde(default)]
	architecture: Vec<RawArchitecture>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUnrunnable {
	#[serde(rename = "crate")]
	crate_name: String,
	reason: String,
}

// One rule with one cause, rather than one exemption per crate. Fifteen protocol crates cannot be
// host-tested in the shipping configuration for the same structural reason, and writing that reason
// fifteen times would make it look like fifteen unrelated accidents.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfigurationUnrunnable {
	configuration: String,
	when_static_reach: String,
	reason: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSelectsEverything {
	component: String,
	reason: String,
}

// A subsystem that COULD be narrowed once there is evidence, recorded so the debt is visible.
// Parsed and validated rather than left as prose: a class naming a path that no longer exists is a
// plan somebody made for a tree that has moved on.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRiskClass {
	path: String,
	class: String,
	evidence: String,
	// THE FOUR FIELDS `evidence` DESCRIBES IN PROSE, so a check can hold a narrowing to them.
	//
	// `evidence` says things like "shadow-clean on all three targets for allocator and page-table
	// changes" and "shadow-clean plus the ABI unchanged". A sentence cannot be checked, and three
	// fields would not have been enough: `mem` names two change groups, `object` names four and
	// `sched` two, so a bar built on targets, a count and an ABI flag would pass on five edits to one
	// corner of the subsystem.
	#[serde(default)]
	targets: Vec<String>,
	#[serde(default)]
	distinct_changes: usize,
	#[serde(default)]
	abi_unchanged: bool,
	#[serde(default)]
	required_groups: Vec<String>,
}

// A NAMED KIND OF CHANGE, AND THE PATHS THAT CONSTITUTE IT.
//
// `risk_class.required_groups` names them and nothing produced them: `Record` carries
// `change_kinds`, but those say how a component was REACHED - what it ships, what its tests are
// built from - not what the edit was ABOUT, so no record could answer "was this an allocator
// change". Declared here as a name and its paths, matched against what a change set touched, and
// written into the evidence record beside the rest.
//
// A group matching no tracked path is REFUSED rather than left standing: a bar nobody can meet is
// the same defect as a check that skips itself, reached from the other side.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawChangeGroup {
	name: String,
	paths: Vec<String>,
	reason: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOwnership {
	path: String,
	component: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNonCode {
	path: String,
	reason: String,
}

// `from` and `to` each accept one name or a list, because the honest shape of these edges is
// many-to-many: sixteen protocol crates are generated from one IDL set by one generator. Writing
// that as thirty-two single-line entries would bury the one fact it states.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEdge {
	from: Names,
	to: Names,
	kind: String,
	reason: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum Names {
	One(String),
	Many(Vec<String>),
}

impl Names {
	fn into_vec(self) -> Vec<String> {
		match self {
			Names::One(name) => vec![name],
			Names::Many(names) => names,
		}
	}
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawArchitecture {
	path: String,
	build: Vec<String>,
	boot: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfigurations {
	schema: u32,
	#[serde(default)]
	configuration: Vec<Configuration>,
}

// Hashed by CONTENT into the model hash - every field here is part of what a configuration MEANS,
// so adding a feature to `shared-image` moves the hash even though its name did not change.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
	pub name: String,
	pub default_features: bool,
	pub features: Vec<String>,
	pub profile: String,
	pub build_mode: String,
	pub description: String,
}

#[derive(Clone, Debug)]
pub struct OwnershipRule {
	pub path: String,
	pub component: String,
}

#[derive(Clone, Debug)]
pub struct NonCodeRule {
	pub path: String,
	pub reason: String,
}

#[derive(Clone, Debug)]
pub struct DeclaredEdge {
	pub from: String,
	pub to: String,
	pub kind: String,
	pub reason: String,
}

#[derive(Clone, Debug)]
pub struct ArchitectureRule {
	pub path: String,
	pub build: Vec<String>,
	pub boot: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct RiskClass {
	pub path: String,
	pub class: String,
	// The prose, kept: it is what a person reads, and the four fields below are what a check reads.
	// Both, because a field that contradicts its sentence is worse than either alone.
	pub evidence: String,
	pub targets: Vec<String>,
	pub distinct_changes: usize,
	pub abi_unchanged: bool,
	pub required_groups: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ChangeGroup {
	pub name: String,
	pub paths: Vec<String>,
	pub reason: String,
}

#[derive(Clone, Debug)]
pub struct Unrunnable {
	pub crate_name: String,
	pub reason: String,
}

#[derive(Clone, Debug)]
pub struct ConfigurationUnrunnable {
	pub configuration: String,
	pub when_static_reach: String,
	pub reason: String,
}

#[derive(Clone, Debug)]
pub struct SelectsEverything {
	pub component: String,
	pub reason: String,
}

#[derive(Clone, Debug)]
pub struct Registry {
	pub ownership: Vec<OwnershipRule>,
	pub non_code: Vec<NonCodeRule>,
	pub host_tests_unrunnable: Vec<Unrunnable>,
	pub risk_classes: Vec<RiskClass>,
	pub change_groups: Vec<ChangeGroup>,
	pub host_configuration_unrunnable: Vec<ConfigurationUnrunnable>,
	pub selects_everything: Vec<SelectsEverything>,
	pub edges: Vec<DeclaredEdge>,
	pub architecture: Vec<ArchitectureRule>,
	pub configurations: Vec<Configuration>,
	// The exact bytes both files were read from. The model hash is over these rather than over the
	// parsed structures: a reordering that changes nothing should not move the hash, but a comment
	// explaining WHY a rule exists is part of the rule, and a parsed struct silently drops it.
	pub registry_text: String,
	pub configurations_text: String,
}

impl Registry {
	pub fn load(model_dir: &Path) -> Result<Self, String> {
		let registry_path = model_dir.join("registry.toml");
		let configurations_path = model_dir.join("configurations.toml");
		let registry_text = fs::read_to_string(&registry_path).map_err(|error| format!("{}: {error}", registry_path.display()))?;
		let configurations_text = fs::read_to_string(&configurations_path).map_err(|error| format!("{}: {error}", configurations_path.display()))?;
		let raw: RawRegistry = toml::from_str(&registry_text).map_err(|error| format!("{}: {error}", registry_path.display()))?;
		let raw_configurations: RawConfigurations = toml::from_str(&configurations_text).map_err(|error| format!("{}: {error}", configurations_path.display()))?;
		if raw.schema != 1 {
			return Err(format!("{}: unsupported schema {}", registry_path.display(), raw.schema));
		}
		if raw_configurations.schema != 1 {
			return Err(format!("{}: unsupported schema {}", configurations_path.display(), raw_configurations.schema));
		}

		let mut edges = Vec::new();
		for edge in raw.edge {
			let kind = edge.kind.clone();
			let reason = edge.reason.clone();
			let targets = edge.to.into_vec();
			for from in edge.from.into_vec() {
				for to in &targets {
					edges.push(DeclaredEdge { from: from.clone(), to: to.clone(), kind: kind.clone(), reason: reason.clone() });
				}
			}
		}

		let registry = Registry { ownership: raw.ownership.into_iter().map(|rule| OwnershipRule { path: rule.path, component: rule.component }).collect(), non_code: raw.non_code.into_iter().map(|rule| NonCodeRule { path: rule.path, reason: rule.reason }).collect(), change_groups: raw.change_group.into_iter().map(|rule| ChangeGroup { name: rule.name, paths: rule.paths, reason: rule.reason }).collect(), risk_classes: raw.risk_class.into_iter().map(|rule| RiskClass { path: rule.path, class: rule.class, evidence: rule.evidence, targets: rule.targets, distinct_changes: rule.distinct_changes, abi_unchanged: rule.abi_unchanged, required_groups: rule.required_groups }).collect(), host_tests_unrunnable: raw.host_tests_unrunnable.into_iter().map(|rule| Unrunnable { crate_name: rule.crate_name, reason: rule.reason }).collect(), host_configuration_unrunnable: raw.host_configuration_unrunnable.into_iter().map(|rule| ConfigurationUnrunnable { configuration: rule.configuration, when_static_reach: rule.when_static_reach, reason: rule.reason }).collect(), selects_everything: raw.selects_everything.into_iter().map(|rule| SelectsEverything { component: rule.component, reason: rule.reason }).collect(), edges, architecture: raw.architecture.into_iter().map(|rule| ArchitectureRule { path: rule.path, build: rule.build, boot: rule.boot }).collect(), configurations: raw_configurations.configuration, registry_text, configurations_text };
		registry.validate()?;
		Ok(registry)
	}

	// Refuse a registry that cannot mean what it says, rather than letting it produce a plan that
	// is quietly narrower than intended. Every one of these has the same failure shape: the file
	// still parses, the planner still runs, and the answer is wrong in the direction of testing
	// less.
	fn validate(&self) -> Result<(), String> {
		let mut errors = Vec::new();
		let names: BTreeMap<&str, &Configuration> = self.configurations.iter().map(|configuration| (configuration.name.as_str(), configuration)).collect();
		if names.len() != self.configurations.len() {
			errors.push(String::from("configurations.toml: two configurations share a name"));
		}
		for rule in &self.architecture {
			for architecture in rule.build.iter().chain(rule.boot.iter()) {
				if !ARCHITECTURES.contains(&architecture.as_str()) {
					errors.push(format!("architecture rule '{}': unknown architecture '{architecture}'", rule.path));
				}
			}
			// A target booted but not built cannot run: the image it would boot was never made.
			for architecture in &rule.boot {
				if !rule.build.contains(architecture) {
					errors.push(format!("architecture rule '{}': boots {architecture} without building it", rule.path));
				}
			}
			if rule.build.is_empty() {
				errors.push(format!("architecture rule '{}': builds nothing", rule.path));
			}
		}
		for rule in &self.risk_classes {
			if !["foundational", "contract", "narrowable"].contains(&rule.class.as_str()) {
				errors.push(format!("risk class '{}': unknown class '{}'", rule.path, rule.class));
			}
			if rule.evidence.is_empty() {
				errors.push(format!("risk class '{}': no evidence criterion, so nothing could ever discharge it", rule.path));
			}
		}
		// The catch-all. Without it a path matching no rule has no architecture answer at all, and
		// "no answer" is the state every fail-open rule in this model exists to avoid.
		if !self.architecture.iter().any(|rule| rule.path.is_empty()) {
			errors.push(String::from("registry.toml: no default architecture rule (an entry with path = \"\")"));
		}
		if errors.is_empty() { Ok(()) } else { Err(errors.join("\n")) }
	}

	pub fn host_tests_runnable(&self, crate_name: &str) -> bool {
		!self.host_tests_unrunnable.iter().any(|rule| rule.crate_name == crate_name)
	}

	pub fn configuration(&self, name: &str) -> Option<&Configuration> {
		self.configurations.iter().find(|configuration| configuration.name == name)
	}
}

// Longest matching prefix, where a rule path matches a file path exactly or as a directory prefix.
//
// Returned with the match length so callers can compare rules from different sources on the same
// footing - which is the whole reason `src/kernel/arch/x86_64` beats the `kernel` crate rooted at
// `src/kernel`, and why the architecture policy works at all.
pub fn prefix_match(rule: &str, path: &str) -> Option<usize> {
	if rule.is_empty() {
		return Some(0);
	}
	if path == rule || path.starts_with(&format!("{rule}/")) { Some(rule.len()) } else { None }
}
