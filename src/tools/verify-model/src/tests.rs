// Two kinds of test, and the second kind is the one M0148 argues for at length.
//
// The property tests pin the selector's shape: monotonicity, determinism, idempotence,
// full-absorption, total ownership. The NEGATIVE fixtures pin what each validator must REJECT,
// because "a gate that breaks fails loudly on its own" is not true - `exit 0` at the top of a
// checker breaks it catastrophically and silently, and a validator tested only by running its
// current version over a currently-valid tree is not tested at all.

use crate::catalog::{Catalog, KernelTest};
use crate::crates::discover;
use crate::graph::Graph;
use crate::ownership::{Owner, Ownership};
use crate::plan::Planner;
use crate::registry::Registry;
use crate::{Model, tracked_files};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
	// The crate lives at src/tools/verify-model, so the root is three levels up. Derived from
	// CARGO_MANIFEST_DIR rather than from the working directory, which `cargo test` does not
	// promise anything about.
	Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(3).expect("src/tools/verify-model is three below the root").to_path_buf()
}

fn model() -> Model {
	Model::load(&repo_root()).expect("the model in this tree must load")
}

// ---------------------------------------------------------------------------------------------
// The model, as this tree actually is

#[test]
fn every_tracked_file_is_owned_or_declared_not_code() {
	let model = model();
	let unowned = model.unowned_paths().expect("git ls-files");
	assert!(unowned.is_empty(), "{} tracked file(s) belong to no component:\n  {}", unowned.len(), unowned.join("\n  "));
}

#[test]
fn every_crate_with_host_tests_is_in_the_catalog_or_declared_unrunnable() {
	let model = model();
	let missing: Vec<&str> = model.crates.iter().filter(|entry| entry.has_host_tests && model.registry.host_tests_runnable(&entry.name) && model.catalog.get(&format!("host.{}", entry.name)).is_none()).map(|entry| entry.name.as_str()).collect();
	assert!(missing.is_empty(), "crates with host tests and no catalog entry: {missing:?}");
	// And the escape hatch cannot be used for a crate that would work: a stale exemption keeps a
	// suite out of the gate long after the reason for excluding it has gone.
	for rule in &model.registry.host_tests_unrunnable {
		let entry = model.crates.iter().find(|entry| entry.name == rule.crate_name).unwrap_or_else(|| panic!("host_tests_unrunnable names '{}', which is not a crate", rule.crate_name));
		assert!(entry.has_host_tests, "host_tests_unrunnable names '{}', which has no host tests to exclude", rule.crate_name);
		assert!(!rule.reason.is_empty(), "an exclusion without a stated cause is a gap wearing a label");
	}
}

// The defect that made the configuration dimension necessary. If this ever passes trivially it is
// because the graph went back to reading one resolved configuration, and the answer to "who uses
// the IPC plumbing" became "nobody".
#[test]
fn the_static_graph_is_configuration_complete() {
	let model = model();
	// Asserted at the EDGE, not at reachability, and the difference matters. The three sources are
	// unioned, so a lost static edge is often covered by a dynamic one and the closure still looks
	// right - which would make a reachability assertion pass over a graph that had quietly stopped
	// reading half the Cargo manifests. What has to hold is that the optional dependency itself is
	// present as `link.static`.
	for (from, to) in [("proto", "ipc-client"), ("storage-proto", "ipc-client"), ("audio-proto", "ipc-client"), ("tools", "audio-client"), ("tools", "process-client-provider")] {
		let found = model.graph.edges.iter().any(|edge| edge.from == from && edge.to == to && edge.kind == "link.static");
		assert!(found, "{from} declares {to} optional behind a feature only `shared-image` enables; a resolver asked in the default configuration would report the edge does not exist");
	}
	// And the closure must still arrive, by whichever route.
	let seeds: BTreeSet<String> = [String::from("ipc-client")].into_iter().collect();
	let affected = model.graph.affected(&seeds);
	for expected in ["proto", "storage-proto", "tools", "bin.audioconv"] {
		assert!(affected.contains(expected), "changing ipc-client must reach {expected}");
	}
}

// The defect round four found: the manifest is the dynamic graph, and all of src/fs is outside it.
#[test]
fn static_filesystem_edges_reach_their_dependents() {
	let model = model();
	let seeds: BTreeSet<String> = [String::from("liberfs")].into_iter().collect();
	let affected = model.graph.affected(&seeds);
	for expected in ["storage", "kernel", "loader", "mkpackages"] {
		assert!(affected.contains(expected), "changing liberfs must reach {expected}; it is a static path dependency and services/manifest.toml never mentions it");
	}
}

// The inverted edge: a userspace codec reaches the kernel, because the kernel's test binary builds
// its audioconv fixtures with the same codecs the scenario asserts against.
#[test]
fn a_codec_reaches_the_kernel_test_binary() {
	let model = model();
	let seeds: BTreeSet<String> = [String::from("flac")].into_iter().collect();
	let affected = model.graph.affected(&seeds);
	assert!(affected.contains("kernel"), "flac is a dev-dependency of the kernel; changing it rebuilds the kernel test binary");
}

#[test]
fn build_dependencies_are_their_own_edge_kind() {
	let model = model();
	let edge = model.graph.edges.iter().find(|edge| edge.from == "kernel" && edge.to == "system-manifest").expect("the kernel build-depends on system-manifest");
	assert_eq!(edge.kind, "generation.build", "a build dependency is neither linked nor tested; collapsing it into link.static loses what it means");
}

// ---------------------------------------------------------------------------------------------
// Properties of the selector

fn plan_for(model: &Model, paths: &[&str]) -> crate::plan::Plan {
	let ownership = model.ownership();
	let planner = Planner { registry: &model.registry, graph: &model.graph, ownership: &ownership, catalog: &model.catalog, unenumerated_targets: model.kernel_tests.missing_targets.clone() };
	planner.plan(&paths.iter().map(|path| (*path).to_string()).collect::<Vec<_>>())
}

fn keys(plan: &crate::plan::Plan) -> BTreeSet<String> {
	plan.items.iter().map(|item| item.key.display()).collect()
}

#[test]
fn selection_is_deterministic() {
	let model = model();
	let first = keys(&plan_for(&model, &["src/user/libs/audio/flac/src/lib.rs"]));
	let second = keys(&plan_for(&model, &["src/user/libs/audio/flac/src/lib.rs"]));
	assert_eq!(first, second);
}

// A larger change selects a superset. This is the property that makes a scoped run trustworthy:
// adding a file to a change can never remove work from the plan.
#[test]
fn selection_is_monotone() {
	let model = model();
	let small = keys(&plan_for(&model, &["src/user/libs/audio/flac/src/lib.rs"]));
	let large = keys(&plan_for(&model, &["src/user/libs/audio/flac/src/lib.rs", "src/user/libs/image/png/src/lib.rs"]));
	assert!(small.is_subset(&large), "adding a changed path removed {:?} from the plan", small.difference(&large).collect::<Vec<_>>());
}

// Two files in the same component are one component's worth of work, however many there are.
#[test]
fn selection_is_idempotent_within_a_component() {
	let model = model();
	let once = keys(&plan_for(&model, &["src/user/libs/audio/flac/src/lib.rs"]));
	let twice = keys(&plan_for(&model, &["src/user/libs/audio/flac/src/lib.rs", "src/user/libs/audio/flac/src/lib.rs"]));
	assert_eq!(once, twice);
}

// Any full-selecting path makes the WHOLE answer full, however small the rest of the change is.
#[test]
fn full_absorbs_everything_else() {
	let model = model();
	let plan = plan_for(&model, &["src/user/libs/audio/flac/src/lib.rs", "src/abi/src/lib.rs"]);
	assert!(plan.full, "a change to the ABI selects everything, and mixing it with a codec change cannot narrow that");
	assert_eq!(plan.architectures_booted, vec!["aarch64", "riscv64", "x86_64"]);
}

#[test]
fn an_unknown_path_selects_everything() {
	let model = model();
	let plan = plan_for(&model, &["src/something/nobody/declared.rs"]);
	assert!(plan.full);
	assert!(plan.full_reasons.iter().any(|reason| reason.contains("owned by no component")));
}

#[test]
fn documentation_selects_nothing_and_says_so() {
	let model = model();
	let plan = plan_for(&model, &["docs/todo/M0148.md"]);
	assert!(plan.nothing_to_do, "a documentation change has nothing to verify");
	assert!(plan.items.is_empty());
	assert!(!plan.full);
}

// The half of the false-green defect that path ownership alone did not close: a change to the
// harness has to boot every target, because qemu-run.sh carries thirteen architecture-specific
// branches and a fault in its riscv64 one is invisible from an x86_64 run.
#[test]
fn a_harness_change_boots_every_target() {
	let model = model();
	let plan = plan_for(&model, &["src/boot/qemu-run.sh"]);
	assert!(plan.full);
	assert_eq!(plan.architectures_booted, vec!["aarch64", "riscv64", "x86_64"]);
}

#[test]
fn a_per_target_file_selects_its_own_target() {
	let model = model();
	let plan = plan_for(&model, &["src/kernel/arch/riscv64/traps/mod.rs"]);
	assert_eq!(plan.architectures_booted, vec!["riscv64"], "a riscv64 tree boots riscv64");
	assert_eq!(plan.architectures_built, vec!["aarch64", "riscv64", "x86_64"], "and still cross-builds, because a branch that stops compiling elsewhere is a regression too");
}

// The distinction that is easy to get wrong: a file that CHOOSES between targets selects all of
// them, because a bug in the mapping is invisible on the target it maps correctly.
#[test]
fn a_file_that_chooses_between_targets_selects_all_of_them() {
	let model = model();
	let chooser = plan_for(&model, &["src/user/build.rs"]);
	assert_eq!(chooser.architectures_booted, vec!["aarch64", "riscv64", "x86_64"]);
	let chosen = plan_for(&model, &["src/user/user-riscv64.ld"]);
	assert_eq!(chosen.architectures_booted, vec!["riscv64"]);
}

// An ordinary codec change must not boot the emulated targets. This is the row that pays for the
// milestone: 2877 s and 6104 s not spent, against ~20 s saved by running fewer tests.
#[test]
fn an_ordinary_userspace_change_boots_one_target() {
	let model = model();
	let plan = plan_for(&model, &["src/user/libs/audio/flac/src/lib.rs"]);
	assert!(!plan.full, "a codec is not on the selects-everything list");
	assert_eq!(plan.architectures_booted, vec!["x86_64"]);
}

#[test]
fn a_codec_change_selects_its_own_host_suite_and_its_consumers() {
	let model = model();
	let plan = plan_for(&model, &["src/user/libs/audio/flac/src/lib.rs"]);
	let selected = keys(&plan);
	assert!(selected.iter().any(|key| key.starts_with("host.flac /")), "the crate's own host suite is the cheapest and most precise check there is");
	// `audioconv` is both a library and a program, which is why programs have their own namespace:
	// the library lists flac among its providers, and the tool loads the library.
	assert!(plan.affected_components.contains(&String::from("audioconv")), "flac is a provider of the audioconv library in services/manifest.toml");
	assert!(plan.affected_components.contains(&String::from("bin.audioconv")), "and the tool loads that library");
	assert!(plan.affected_components.contains(&String::from("bin.play")), "play lists the codecs directly, because it calls the decoders itself");
}

// The planner may emit nothing outside the catalog. Everything downstream - age, cost, shadow, the
// regression corpus - ranges over exactly that universe, so an item outside it has no history and
// can never be judged stale.
#[test]
fn every_plan_item_is_a_catalog_variant() {
	let model = model();
	for paths in [vec!["src/user/libs/audio/flac/src/lib.rs"], vec!["src/abi/src/lib.rs"], vec!["src/kernel/arch/riscv64/traps/mod.rs"]] {
		let plan = plan_for(&model, &paths);
		for item in &plan.items {
			let check = model.catalog.get(&item.key.check).unwrap_or_else(|| panic!("{} is not in the catalog", item.key.check));
			let matches = check.variants.iter().any(|variant| variant.architecture == item.key.architecture && variant.environment == item.key.environment && variant.configuration == item.key.configuration);
			assert!(matches, "{} is not a variant the catalog declares", item.key.display());
		}
	}
}

// The shipping configuration of the protocol crates is not merely untested - it is currently
// UNTESTABLE on the host, and that has to stay visible rather than becoming an absence.
//
// Measured 2026-08-08: enabling `shared-image` activates `ipc-client`, which links `rt`, whose
// `panic_impl` collides with the `std` that `cargo test` needs. E0152 before a test runs. So the
// catalog offers no key for it - a key that can only ever be red is a worse lie than none - and the
// registry carries the reason. The graph dimension is unaffected: the EDGE is still there, which is
// where the configuration work actually pays.
#[test]
fn an_untestable_shipping_configuration_is_recorded_rather_than_forgotten() {
	let model = model();
	let check = model.catalog.get("host.proto").expect("proto has host tests");
	let configurations: BTreeSet<&str> = check.variants.iter().map(|variant| variant.configuration.as_str()).collect();
	assert!(configurations.contains("default"));
	assert!(!configurations.contains("shared-image"), "the shipping configuration of proto cannot be built for `cargo test`; offering the key would put a permanently red entry in the catalog");
	let rule = model.registry.host_configuration_unrunnable.iter().find(|rule| rule.configuration == "shared-image").expect("the exclusion must be declared, with its cause, or it is just a gap");
	assert!(rule.reason.contains("panic_impl"), "the recorded reason must name the actual cause: {}", rule.reason);
	// And the edge the configuration dimension exists for is still in the graph.
	assert!(model.graph.edges.iter().any(|edge| edge.from == "proto" && edge.to == "ipc-client" && edge.kind == "link.static"));
}

// ---------------------------------------------------------------------------------------------
// Negative fixtures: what each validator must refuse
//
// Built in a temporary directory so a real registry is never the thing under test. The point is
// not that these inputs are exotic - it is that every one of them leaves the model PARSEABLE and
// the planner RUNNING, and produces a plan that is quietly too small.

struct Fixture {
	dir: PathBuf,
}

impl Fixture {
	fn new(name: &str) -> Self {
		let dir = std::env::temp_dir().join(format!("verify-model-fixture-{name}-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("fixture directory");
		Fixture { dir }
	}

	fn write(&self, name: &str, text: &str) {
		std::fs::write(self.dir.join(name), text).expect("fixture file");
	}

	fn load(&self) -> Result<Registry, String> {
		Registry::load(&self.dir)
	}
}

impl Drop for Fixture {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.dir);
	}
}

const VALID_CONFIGURATIONS: &str = "schema = 1\n[[configuration]]\nname = \"default\"\ndefault_features = true\nfeatures = []\nprofile = \"dev\"\nbuild_mode = \"host-test\"\ndescription = \"d\"\n";

#[test]
fn a_registry_without_a_default_architecture_rule_is_refused() {
	let fixture = Fixture::new("no-default-arch");
	fixture.write("registry.toml", "schema = 1\n[[architecture]]\npath = \"src/kernel\"\nbuild = [\"x86_64\"]\nboot = [\"x86_64\"]\n");
	fixture.write("configurations.toml", VALID_CONFIGURATIONS);
	let error = fixture.load().expect_err("a registry with no catch-all leaves some paths with no architecture answer at all");
	assert!(error.contains("no default architecture rule"), "{error}");
}

#[test]
fn a_rule_that_boots_what_it_does_not_build_is_refused() {
	let fixture = Fixture::new("boot-without-build");
	fixture.write("registry.toml", "schema = 1\n[[architecture]]\npath = \"\"\nbuild = [\"x86_64\"]\nboot = [\"x86_64\", \"riscv64\"]\n");
	fixture.write("configurations.toml", VALID_CONFIGURATIONS);
	let error = fixture.load().expect_err("booting a target whose image was never built cannot work");
	assert!(error.contains("boots riscv64 without building it"), "{error}");
}

#[test]
fn an_unknown_architecture_is_refused() {
	let fixture = Fixture::new("unknown-arch");
	fixture.write("registry.toml", "schema = 1\n[[architecture]]\npath = \"\"\nbuild = [\"sparc64\"]\nboot = []\n");
	fixture.write("configurations.toml", VALID_CONFIGURATIONS);
	let error = fixture.load().expect_err("a typo in an architecture name would silently stop matching");
	assert!(error.contains("unknown architecture 'sparc64'"), "{error}");
}

#[test]
fn a_registry_with_an_unknown_field_is_refused() {
	// `deny_unknown_fields`, checked rather than assumed: a renamed key that is silently ignored
	// is a rule that silently stops applying.
	let fixture = Fixture::new("unknown-field");
	fixture.write("registry.toml", "schema = 1\n[[ownership]]\npath = \"src/x\"\ncomponents = \"x\"\n[[architecture]]\npath = \"\"\nbuild = [\"x86_64\"]\nboot = []\n");
	fixture.write("configurations.toml", VALID_CONFIGURATIONS);
	assert!(fixture.load().is_err(), "an ownership rule spelled `components` claims nothing and must not be accepted");
}

#[test]
fn two_configurations_with_one_name_are_refused() {
	let fixture = Fixture::new("duplicate-configuration");
	fixture.write("registry.toml", "schema = 1\n[[architecture]]\npath = \"\"\nbuild = [\"x86_64\"]\nboot = []\n");
	fixture.write("configurations.toml", &format!("{VALID_CONFIGURATIONS}[[configuration]]\nname = \"default\"\ndefault_features = false\nfeatures = []\nprofile = \"release\"\nbuild_mode = \"shipping\"\ndescription = \"d\"\n"));
	let error = fixture.load().expect_err("two meanings for one configuration name make a PlanItemKey ambiguous");
	assert!(error.contains("share a name"), "{error}");
}

#[test]
fn an_edge_naming_a_component_that_does_not_exist_is_refused() {
	// The rename that got half done: the crate becomes `audio-conv`, the registry still says
	// `audioconv`, and the declared edge quietly points at nothing. Nothing else notices, because
	// a missing edge makes the plan SMALLER.
	let root = repo_root();
	let crates = discover(&root).expect("crates");
	let manifest = system_manifest::Manifest::load_workspace(&root.join("src")).expect("manifest");
	let fixture = Fixture::new("dangling-edge");
	fixture.write("registry.toml", "schema = 1\n[[edge]]\nfrom = \"kernel\"\nto = \"a-crate-that-was-renamed\"\nkind = \"generation\"\nreason = \"r\"\n[[architecture]]\npath = \"\"\nbuild = [\"x86_64\"]\nboot = []\n");
	fixture.write("configurations.toml", VALID_CONFIGURATIONS);
	let registry = fixture.load().expect("this registry is well formed; the edge is the problem");
	let graph = Graph::build(&crates, &manifest, &registry);
	let error = graph.validate(&crates, &registry).expect_err("an edge to a component that does not exist reaches nothing");
	assert!(error.contains("a-crate-that-was-renamed"), "{error}");
}

#[test]
fn a_catalog_naming_an_undefined_configuration_is_refused() {
	let root = repo_root();
	let crates = discover(&root).expect("crates");
	let fixture = Fixture::new("undefined-configuration");
	fixture.write("registry.toml", "schema = 1\n[[architecture]]\npath = \"\"\nbuild = [\"x86_64\"]\nboot = []\n");
	// No `shared-image` configuration is defined here, but crates declaring the feature would ask
	// the catalog for that variant.
	fixture.write("configurations.toml", VALID_CONFIGURATIONS);
	let registry = fixture.load().expect("well formed");
	let manifest = system_manifest::Manifest::load_workspace(&root.join("src")).expect("manifest");
	let graph = Graph::build(&crates, &manifest, &registry);
	let kernel_test = KernelTest { name: String::from("t"), architectures: vec![String::from("x86_64")], covers: vec![String::from("kernel")] };
	let mut catalog = Catalog::build(&crates, &registry, &graph, &kernel_test_slice(&kernel_test));
	catalog.checks[0].variants[0].configuration = String::from("a-configuration-nobody-defined");
	let error = catalog.validate(&registry).expect_err("a variant in an undefined configuration cannot be run or keyed");
	assert!(error.contains("a-configuration-nobody-defined"), "{error}");
}

#[test]
fn a_duplicate_check_id_is_refused() {
	let root = repo_root();
	let crates = discover(&root).expect("crates");
	let registry = Registry::load(&root.join("src/tools/verify-model/model")).expect("the real registry");
	let manifest = system_manifest::Manifest::load_workspace(&root.join("src")).expect("manifest");
	let graph = Graph::build(&crates, &manifest, &registry);
	let mut catalog = Catalog::build(&crates, &registry, &graph, &[]);
	let duplicate = catalog.checks[0].clone();
	catalog.checks.push(duplicate);
	let error = catalog.validate(&registry).expect_err("an ID is what age, shadow, cost and regression history are keyed on; two checks sharing one merges four separate records");
	assert!(error.contains("duplicate check id"), "{error}");
}

#[test]
fn an_unowned_path_is_reported_rather_than_ignored() {
	let root = repo_root();
	let crates = discover(&root).expect("crates");
	let registry = Registry::load(&root.join("src/tools/verify-model/model")).expect("the real registry");
	let ownership = Ownership::new(&registry, &crates);
	let unowned = ownership.unowned(&[String::from("src/a-subtree-nobody-declared/file.rs")]);
	assert_eq!(unowned.len(), 1, "an unowned path must be visible; it fails open to the full suite, which is safe and silent");
	assert!(matches!(ownership.owner("src/a-subtree-nobody-declared/file.rs"), Owner::Unknown));
}

// Ownership is a longest-prefix contest between declared rules and crate containment, and this is
// the case the architecture policy depends on completely: the arch tree must win over the crate
// that contains it.
#[test]
fn a_longer_declared_rule_beats_the_crate_that_contains_it() {
	let model = model();
	let ownership = model.ownership();
	match ownership.owner("src/kernel/arch/riscv64/traps/mod.rs") {
		Owner::Component { component, .. } => assert_eq!(component, "kernel.arch.riscv64"),
		other => panic!("expected the arch component, got {other:?}"),
	}
	match ownership.owner("src/kernel/sched/mod.rs") {
		Owner::Component { component, .. } => assert_eq!(component, "kernel"),
		other => panic!("expected the kernel crate, got {other:?}"),
	}
}

#[test]
fn the_model_hash_moves_when_a_configuration_changes_meaning() {
	// `shared-image` is a LABEL. Add a feature to it and the label does not move - so a hash over
	// names rather than content would keep saying the evidence is still valid.
	let root = repo_root();
	let before = Registry::load(&root.join("src/tools/verify-model/model")).expect("the real registry");
	let fixture = Fixture::new("configuration-meaning");
	fixture.write("registry.toml", &before.registry_text);
	fixture.write("configurations.toml", &before.configurations_text.replace("features = [\"shared-image\"]", "features = [\"shared-image\", \"something-new\"]"));
	let after = fixture.load().expect("still well formed");
	assert_ne!(before.configurations_text, after.configurations_text, "the fixture must actually differ, or this test proves nothing");
	assert_ne!(crate::registry::Registry::load(&root.join("src/tools/verify-model/model")).map(|registry| registry.configurations_text).unwrap(), after.configurations_text);
}

#[test]
fn tracked_files_is_not_empty() {
	// Guards the ownership gate against its own worst failure: a `git ls-files` that returns
	// nothing makes "every file is owned" trivially true.
	let files = tracked_files(&repo_root()).expect("git ls-files");
	assert!(files.len() > 100, "expected the repository's file list, got {} entries", files.len());
}

// A one-element slice, spelled out because `&[value]` of a moved local reads worse than it works.
fn kernel_test_slice(test: &KernelTest) -> Vec<KernelTest> {
	vec![test.clone()]
}

// A target that enumerated far fewer tests than its peers found something that is not its test
// binary. `build.sh --part kernel` writes the ordinary kernel into the same `deps/` directory under
// the same `kernel-<hash>` shape, and taking the newest file there reported zero tests for that
// target - no variants, no items, and a scoped plan that quietly booted nothing.
#[test]
fn every_enumerated_target_reports_a_comparable_test_count() {
	let model = model();
	let mut per_target: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
	for test in &model.kernel_tests.tests {
		for architecture in &test.architectures {
			*per_target.entry(architecture.as_str()).or_default() += 1;
		}
	}
	let Some(richest) = per_target.values().copied().max() else {
		// No target enumerated at all is handled by the fail-open path, not here.
		return;
	};
	for (architecture, count) in &per_target {
		assert!(*count * 2 >= richest, "{architecture} enumerated {count} tests against {richest} elsewhere - the wrong binary was read");
	}
}

// ---------------------------------------------------------------------------------------------
// The regression corpus
//
// Real commit ranges from this repository's history, replayed through the planner. The ranges are
// the point: a fixture that hands the planner a path list tests the closure and calls it a test of
// the selector, while a range also exercises diff parsing, renames, deletions and ownership.

#[derive(serde::Deserialize)]
struct Corpus {
	schema: u32,
	#[serde(default)]
	case: Vec<Case>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
	name: String,
	#[serde(default)]
	range: Option<String>,
	#[serde(default)]
	paths: Vec<String>,
	#[serde(default)]
	full: bool,
	#[serde(default)]
	nothing_to_do: bool,
	#[serde(default)]
	must_boot: Vec<String>,
	#[serde(default)]
	must_select: Vec<String>,
	#[serde(default)]
	must_not_select: Vec<String>,
	reason: String,
}

fn changed_paths_for(root: &Path, case: &Case) -> Vec<String> {
	if let Some(range) = &case.range {
		let output = std::process::Command::new("git").arg("-C").arg(root).arg("diff").arg("--name-only").arg(range).output().expect("git diff");
		assert!(output.status.success(), "{}: git diff {range} failed", case.name);
		let paths: Vec<String> = String::from_utf8_lossy(&output.stdout).lines().map(str::to_string).filter(|line| !line.is_empty()).collect();
		assert!(!paths.is_empty(), "{}: the range {range} touched no files - a corpus case that feeds the planner nothing proves nothing", case.name);
		return paths;
	}
	assert!(!case.paths.is_empty(), "{}: a case needs either a range or a path list", case.name);
	case.paths.clone()
}

#[test]
fn the_regression_corpus_replays() {
	let root = repo_root();
	let text = std::fs::read_to_string(root.join("src/tools/verify-model/model/regressions.toml")).expect("regressions.toml");
	let corpus: Corpus = toml::from_str(&text).expect("regressions.toml parses");
	assert_eq!(corpus.schema, 1);
	assert!(corpus.case.len() >= 5, "a corpus of fewer than five cases is a habit, not a check");
	let model = model();

	for case in &corpus.case {
		let paths = changed_paths_for(&root, case);
		let borrowed: Vec<&str> = paths.iter().map(String::as_str).collect();
		let plan = plan_for(&model, &borrowed);
		let selected = keys(&plan);

		assert_eq!(plan.nothing_to_do, case.nothing_to_do, "{}: nothing_to_do mismatch ({})", case.name, case.reason);
		if case.nothing_to_do {
			assert!(plan.items.is_empty(), "{}: nothing to do, yet {} items", case.name, plan.items.len());
			continue;
		}
		assert_eq!(plan.full, case.full, "{}: FULL mismatch ({}); reasons: {:?}", case.name, case.reason, plan.full_reasons);
		if !case.must_boot.is_empty() {
			let mut expected = case.must_boot.clone();
			expected.sort();
			assert_eq!(plan.architectures_booted, expected, "{}: wrong boot set ({})", case.name, case.reason);
		}
		for key in &case.must_select {
			assert!(selected.contains(key), "{}: '{key}' is missing from the plan - this is the run that would have caught the regression ({})", case.name, case.reason);
		}
		// Skipped for a FULL plan, which legitimately contains everything. Everywhere else this is
		// the assertion with teeth: without it the whole corpus passes the day the selector starts
		// returning everything, and a corpus that only ever agrees is not evidence.
		if !case.full {
			for key in &case.must_not_select {
				assert!(!selected.contains(key), "{}: '{key}' should not be in a scoped plan for this change ({})", case.name, case.reason);
			}
		}
	}
}

// ---------------------------------------------------------------------------------------------
// Shadow validation
//
// The parser is the part with teeth here. It was wrong twice, in opposite directions, and both
// failures are the kind this milestone is about: one manufactured a failure on every clean run, the
// other silently paired half the tests with the wrong line.

#[test]
fn a_clean_sweep_parses_as_all_passed() {
	let log = "running 3 tests (all tags)\nalpha...\t[ok]\nbeta...\t[ok]\ngamma...\t[ok]\ntest suite complete: 3 passed\n";
	let results = crate::shadow::parse_guest_log(log);
	assert_eq!(results.outcomes.len(), 3, "every test must be paired; the first version of this parser paired every second one");
	assert!(results.outcomes.values().all(|outcome| *outcome == crate::shadow::Outcome::Passed));
}

#[test]
fn a_test_that_prints_before_its_marker_still_passes() {
	// The real case that broke the first parser: `[ok]` is not reliably on the name's line, because
	// a test may print while it runs. Matching the line marked the last test of a clean 205-test
	// sweep as failed.
	let log = "running 2 tests (all tags)\nalpha...\t[ok]\nbeta...\tstorage: a note the test printed\n[ok]\n";
	let results = crate::shadow::parse_guest_log(log);
	assert_eq!(results.outcomes.get("beta"), Some(&crate::shadow::Outcome::Passed));
}

#[test]
fn the_test_the_suite_died_under_is_the_one_that_failed() {
	let log = "running 3 tests (all tags)\nalpha...\t[ok]\nbeta...\t";
	let results = crate::shadow::parse_guest_log(log);
	assert_eq!(results.outcomes.get("beta"), Some(&crate::shadow::Outcome::Failed));
	assert_eq!(results.outcomes.get("alpha"), Some(&crate::shadow::Outcome::Passed));
}

fn kernel_key(name: &str) -> crate::plan::PlanItemKey {
	crate::plan::PlanItemKey { check: format!("kernel.{name}"), architecture: String::from("x86_64"), environment: crate::catalog::Environment::TestGuest, configuration: String::from("test") }
}

#[test]
fn a_failure_outside_the_selection_is_a_candidate_miss() {
	let results = crate::shadow::parse_guest_log("running 2 tests (all tags)\nalpha...\t[ok]\nbeta...\t");
	let comparison = crate::shadow::compare(&[kernel_key("alpha")], &results, "x86_64", &crate::history::History::default());
	assert_eq!(comparison.verdict, crate::shadow::Verdict::CandidateMiss, "beta was not selected and it failed - that is the shape of a missed edge");
	assert_eq!(comparison.outside_failures.len(), 1);
}

#[test]
fn a_failure_inside_the_selection_is_not_the_selectors_fault() {
	let results = crate::shadow::parse_guest_log("running 2 tests (all tags)\nalpha...\t[ok]\nbeta...\t");
	let comparison = crate::shadow::compare(&[kernel_key("alpha"), kernel_key("beta")], &results, "x86_64", &crate::history::History::default());
	assert_eq!(comparison.verdict, crate::shadow::Verdict::SelectionFailed, "the selector chose beta; beta broke. That is a defect in the code.");
}

// A partial log is the one way a dry shadow could lie quietly: a filtered run looks exactly like a
// clean full one, and reporting Consistent over it would be evidence for nothing.
#[test]
fn a_partial_sweep_is_refused_rather_than_believed() {
	let results = crate::shadow::parse_guest_log("running 2 tests (196 skipped, 198 total)\nalpha...\t[ok]\nbeta...\t[ok]\n");
	let comparison = crate::shadow::compare(&[kernel_key("alpha")], &results, "x86_64", &crate::history::History::default());
	assert_eq!(comparison.verdict, crate::shadow::Verdict::Void);
	assert!(comparison.reason.contains("of 198"), "{}", comparison.reason);
}

// ---------------------------------------------------------------------------------------------
// Trust

#[test]
fn trust_lapses_when_the_model_hash_moves() {
	let mut store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	store.grant("audio", "hash-a", 9, vec![String::from("x86_64"), String::from("riscv64")], 1);
	assert_eq!(store.level("audio", "hash-a"), crate::trust::Level::Trusted);
	// The graph changed, or the covers declarations did, or the selector did. The evidence was
	// produced by a model that is no longer the one running.
	assert_eq!(store.level("audio", "hash-b"), crate::trust::Level::Shadow);
	let dropped = store.prune("hash-b");
	assert_eq!(dropped, vec![String::from("audio")]);
	assert!(store.certificates.is_empty());
}

#[test]
fn trust_needs_evidence_from_more_than_one_target() {
	// A month that only saw x86_64 changes says nothing about the emulated targets, and the
	// architecture policy makes exactly that the steady state.
	let mut log = crate::shadow::Log { schema: 1, records: Vec::new() };
	for _ in 0..crate::trust::REQUIRED_CLEAN_RUNS {
		log.records.push(crate::shadow::Record { architecture: String::from("x86_64"), verdict: String::from("Consistent"), reason: String::new(), model_hash: String::from("hash-a"), source_digest: String::new(), changed_components: vec![String::from("audio")], outside_failures: Vec::new(), at: 0 });
	}
	let store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	let error = store.evaluate("audio", "hash-a", &log).expect_err("one target is not enough");
	assert!(error.contains("target(s)"), "{error}");
	log.records.push(crate::shadow::Record { architecture: String::from("riscv64"), verdict: String::from("Consistent"), reason: String::new(), model_hash: String::from("hash-a"), source_digest: String::new(), changed_components: vec![String::from("audio")], outside_failures: Vec::new(), at: 0 });
	assert!(store.evaluate("audio", "hash-a", &log).is_ok());
}

#[test]
fn evidence_under_another_model_does_not_count() {
	let mut log = crate::shadow::Log { schema: 1, records: Vec::new() };
	for architecture in ["x86_64", "riscv64", "aarch64", "x86_64", "riscv64", "aarch64"] {
		log.records.push(crate::shadow::Record { architecture: architecture.to_string(), verdict: String::from("Consistent"), reason: String::new(), model_hash: String::from("an-older-model"), source_digest: String::new(), changed_components: vec![String::from("audio")], outside_failures: Vec::new(), at: 0 });
	}
	let store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	assert!(store.evaluate("audio", "the-current-model", &log).is_err(), "six clean runs under a model that is not the one running prove nothing about the one that is");
}

// ---------------------------------------------------------------------------------------------
// Fail open, as a list rather than as a habit
//
// The behaviour existed before this test did; what was missing was the enumeration. A fail-open rule
// that is only implemented is a rule that can be removed by an edit nobody reads as a removal - the
// plan just gets smaller, and smaller is the direction no error message ever arrives from.

#[test]
fn every_fail_open_trigger_selects_everything() {
	let model = model();
	let triggers: [(&str, &str); 9] = [
		("src/a-subtree-nobody-declared/thing.rs", "an unknown path: unknown reach is tested with everything"),
		("src/tools/verify-model/src/plan.rs", "the selector itself: it cannot vouch for a change to its own selection"),
		("src/kernel/tests.rs", "the test framework: what `tagged_test!` expands to decides what every tag means"),
		("src/user/services/manifest.toml", "component metadata: it declares every destination in the image"),
		("src/tools/mkpackages/src/main.rs", "the packager: its output IS the system volume"),
		("src/boot/mkimage.sh", "the image builder"),
		("src/boot/qemu-run.sh", "the thing that boots and judges the guest"),
		("lib.sh", "the entry points"),
		("src/abi/src/lib.rs", "the kernel/userspace contract"),
	];
	for (path, why) in triggers {
		let plan = plan_for(&model, &[path]);
		assert!(plan.full, "{path} must select everything - {why}");
		assert_eq!(plan.architectures_booted, vec!["aarch64", "riscv64", "x86_64"], "{path} must also boot every target");
		assert!(!plan.full_reasons.is_empty(), "{path} escalated without saying why, which makes the escalation impossible to audit");
	}
}

// The one outcome that must be impossible: a change that is understood, is not documentation, and
// selects nothing at all.
#[test]
fn a_change_with_owned_paths_never_selects_nothing() {
	let model = model();
	for path in [
		"src/user/libs/audio/flac/src/lib.rs",
		"src/fs/udf/src/lib.rs",
		"src/user/apps/tools/src/echo.rs",
		"src/kernel/arch/riscv64/traps/mod.rs",
		"src/user/drivers/core/src/xhci.rs",
	] {
		let plan = plan_for(&model, &[path]);
		assert!(!plan.items.is_empty(), "{path} produced an empty plan without being declared non-code");
		assert!(!plan.nothing_to_do, "{path} is code");
	}
}

// Found by an outside audit of the implementation rather than of the design, and both are the same
// mistake in different places: a state that is not "clean" being counted as if it were.

#[test]
fn only_a_consistent_verdict_counts_as_clean_evidence() {
	// Five clean comparisons on x86_64 and one riscv64 run that FOUND something used to satisfy
	// "evidence from two targets", so a certificate could be earned on the strength of a failure.
	let mut log = crate::shadow::Log { schema: 1, records: Vec::new() };
	let record = |architecture: &str, verdict: &str| crate::shadow::Record { architecture: architecture.to_string(), verdict: verdict.to_string(), reason: String::new(), model_hash: String::from("hash-a"), source_digest: String::new(), changed_components: vec![String::from("audio")], outside_failures: Vec::new(), at: 0 };
	for _ in 0..crate::trust::REQUIRED_CLEAN_RUNS {
		log.records.push(record("x86_64", "Consistent"));
	}
	log.records.push(record("riscv64", "CandidateMiss"));
	let store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	let error = store.evaluate("audio", "hash-a", &log).expect_err("a run that found a candidate miss is not evidence that the selector is right");
	assert!(error.contains("target(s)"), "{error}");
	// The same target, cleanly this time, is what actually earns it.
	log.records.push(record("riscv64", "Consistent"));
	assert!(store.evaluate("audio", "hash-a", &log).is_ok());
}
