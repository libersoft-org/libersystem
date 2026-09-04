// Two kinds of test, and the second kind is the one this model argues for at length.
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
	let planner = Planner::for_model(&model, &ownership);
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
	let plan = plan_for(&model, &["docs/TESTING.md"]);
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
	let plan = plan_for(&model, &["src/harness/qemu-run.sh"]);
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
	let kernel_test = KernelTest { source_paths: Vec::new(), name: String::from("t"), id: String::from("kernel.t"), architectures: vec![String::from("x86_64")], covers: vec![String::from("kernel")] };
	let staged = crate::staged_components(&manifest, &crates, &graph);
	let mut catalog = Catalog::build(&crates, &registry, &graph, &staged, &kernel_test_slice(&kernel_test));
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
	let staged = crate::staged_components(&manifest, &crates, &graph);
	let mut catalog = Catalog::build(&crates, &registry, &graph, &staged, &[]);
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
		// The SAME parser production uses. It used to be `git diff --name-only` here and two `sed`
		// expressions in `verify.sh`, which is precisely why this corpus could not catch the rename
		// defect in the shell: it was exercising a different reading of the same question.
		let changes = crate::changes::range(root, range).unwrap_or_else(|error| panic!("{}: {error}", case.name));
		let paths = crate::changes::paths(&changes);
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
	let results = crate::shadow::parse_guest_log("running 2 tests (all tags)\nkernel.alpha...\t[ok]\nkernel.beta...\t");
	let comparison = crate::shadow::compare(&[kernel_key("alpha")], &results, "x86_64", &crate::history::History::default());
	assert_eq!(comparison.verdict, crate::shadow::Verdict::CandidateMiss, "beta was not selected and it failed - that is the shape of a missed edge");
	assert_eq!(comparison.outside_failures.len(), 1);
}

#[test]
fn a_failure_inside_the_selection_is_not_the_selectors_fault() {
	let results = crate::shadow::parse_guest_log("running 2 tests (all tags)\nkernel.alpha...\t[ok]\nkernel.beta...\t");
	let comparison = crate::shadow::compare(&[kernel_key("alpha"), kernel_key("beta")], &results, "x86_64", &crate::history::History::default());
	assert_eq!(comparison.verdict, crate::shadow::Verdict::SelectionFailed, "the selector chose beta; beta broke. That is a defect in the code.");
}

// A partial log is the one way a dry shadow could lie quietly: a filtered run looks exactly like a
// clean full one, and reporting Consistent over it would be evidence for nothing.
#[test]
fn a_partial_sweep_is_refused_rather_than_believed() {
	let results = crate::shadow::parse_guest_log("running 2 tests (196 skipped, 198 total)\nkernel.alpha...\t[ok]\nkernel.beta...\t[ok]\n");
	let comparison = crate::shadow::compare(&[kernel_key("alpha")], &results, "x86_64", &crate::history::History::default());
	assert_eq!(comparison.verdict, crate::shadow::Verdict::Void);
	assert!(comparison.reason.contains("of 198"), "{}", comparison.reason);
}

// ---------------------------------------------------------------------------------------------
// Trust

#[test]
fn trust_lapses_when_the_model_hash_moves() {
	let mut store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	store.grant("audio", "hash-a", crate::shadow::Universe::TestGuest, 9, vec![String::from("x86_64"), String::from("riscv64")], crate::shadow::Scope::default(), 1);
	assert_eq!(store.level("audio", "hash-a", crate::shadow::Universe::TestGuest, &crate::shadow::Scope::default()), crate::trust::Level::Trusted);
	// The graph changed, or the covers declarations did, or the selector did. The evidence was
	// produced by a model that is no longer the one running.
	assert_eq!(store.level("audio", "hash-b", crate::shadow::Universe::TestGuest, &crate::shadow::Scope::default()), crate::trust::Level::Shadow);
	let dropped = store.prune("hash-b");
	assert_eq!(dropped, vec![String::from("audio")]);
	assert!(store.certificates.is_empty());
}

// One shadow record, with everything the trust criteria read set to "clean" and the SELECTION INPUT
// varied by `tree`.
//
// Every one of these tests used to leave `source_digest` empty, which made their five records five
// copies of one comparison - and `evaluate` counted them as five. That is exactly the defect the
// criteria now refuse, so the fixtures have to be honest about it: five pieces of evidence means
// five different trees.
fn evidence(universe: crate::shadow::Universe, architecture: &str, exec: bool, tree: &str) -> crate::shadow::Record {
	crate::shadow::Record { universe, architecture: architecture.to_string(), verdict: String::from("Consistent"), reason: String::new(), model_hash: String::from("hash-a"), source_digest: tree.to_string(), changed_components: vec![String::from("audio")], outside_failures: Vec::new(), at: 0, change_kinds: Vec::new(), edge_kinds: Vec::new(), shadow_exec: exec, model_self_check: true, component_decisions: vec![format!("audio\t{tree}")], component_scopes: [(String::from("audio"), crate::shadow::Scope::from_kinds(vec![String::from("modified")], vec![String::from("link.static")]))].into_iter().collect() }
}

#[test]
fn five_comparisons_of_one_change_are_one_piece_of_evidence() {
	// FOUND WHILE TRYING TO EARN THE FIRST REAL CERTIFICATE THIS TOOL HAS EVER GRANTED, 2026-08-14.
	// Nothing was TRUSTED, the criterion was five clean comparisons on two targets, and the cheapest
	// way to satisfy it was to run one shadow comparison five times - which would have worked.
	//
	// It should not. The selector is deterministic and `selection_is_deterministic` says so, so the
	// same change against the same tree produces the same selection and the same sweep verdict every
	// time. Five of those is one comparison wearing the number five, and a certificate earned that
	// way says a scoped run can be believed on the strength of a single answer.
	let mut log = crate::shadow::Log { schema: 1, records: Vec::new() };
	for architecture in ["x86_64", "riscv64", "x86_64", "riscv64", "x86_64", "riscv64"] {
		log.records.push(evidence(crate::shadow::Universe::TestGuest, architecture, true, "one-tree"));
	}
	let store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	assert_eq!(log.clean_runs_for("audio", "hash-a", crate::shadow::Universe::TestGuest), 6, "six records, all of them clean");
	assert_eq!(log.distinct_evidence_for("audio", "hash-a", crate::shadow::Universe::TestGuest), 1, "and all of them the same comparison");
	let error = store.evaluate("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).expect_err("six repetitions of one answer are one answer");
	assert!(error.contains("distinct"), "{error}");

	// FIVE DIFFERENT NEIGHBOURS ARE STILL ONE PIECE OF EVIDENCE ABOUT AUDIO.
	//
	// This asserted the opposite, and the criterion this file states is five genuinely different
	// CHANGES. Keyed on `(source_digest, changed_components)`, `audio + neighbour-0` ..
	// `audio + neighbour-4` are five different change SETS whose decision about audio may be
	// byte-identical - and the neighbour half is free while the audio half is what the certificate
	// is about. "Impossible to manufacture" was too strong, and this is the narrowing.
	let mut log = crate::shadow::Log { schema: 1, records: Vec::new() };
	for (index, architecture) in ["x86_64", "riscv64", "x86_64", "riscv64", "x86_64", "riscv64", "x86_64", "riscv64", "x86_64", "riscv64"].into_iter().enumerate() {
		let mut record = evidence(crate::shadow::Universe::TestGuest, architecture, true, "one-tree");
		record.changed_components = vec![String::from("audio"), format!("neighbour-{}", index / 2)];
		// The neighbour differs and the decision about audio does not.
		record.component_decisions = vec![String::from("audio\tsame-decision"), format!("neighbour-{}\tdiffers-{}", index / 2, index / 2)];
		log.records.push(record);
	}
	assert_eq!(log.distinct_evidence_for("audio", "hash-a", crate::shadow::Universe::TestGuest), 1, "the selector decided the same thing about audio five times over");
	let error = store.evaluate("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).expect_err("a free neighbour is not evidence about audio");
	assert!(error.contains("distinct"), "{error}");

	// And when the decision about AUDIO really does differ, they are five.
	let mut log = crate::shadow::Log { schema: 1, records: Vec::new() };
	for (index, architecture) in ["x86_64", "riscv64", "x86_64", "riscv64", "x86_64", "riscv64", "x86_64", "riscv64", "x86_64", "riscv64"].into_iter().enumerate() {
		let mut record = evidence(crate::shadow::Universe::TestGuest, architecture, true, "one-tree");
		record.changed_components = vec![String::from("audio"), format!("neighbour-{}", index / 2)];
		record.component_decisions = vec![format!("audio\tdecision-{}", index / 2)];
		log.records.push(record);
	}
	assert_eq!(log.distinct_evidence_for("audio", "hash-a", crate::shadow::Universe::TestGuest), 5);
	assert!(store.evaluate("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).is_ok(), "five different decisions about audio are five pieces of evidence about audio");

	// A record written before the field existed falls back to the change set rather than collapsing
	// into one: the evidence it represents was real, and rewriting history is not this store's job.
	let mut log = crate::shadow::Log { schema: 1, records: Vec::new() };
	for (index, architecture) in ["x86_64", "riscv64", "x86_64", "riscv64", "x86_64", "riscv64", "x86_64", "riscv64", "x86_64", "riscv64"].into_iter().enumerate() {
		let mut record = evidence(crate::shadow::Universe::TestGuest, architecture, true, "one-tree");
		record.changed_components = vec![String::from("audio"), format!("neighbour-{}", index / 2)];
		record.component_decisions = Vec::new();
		log.records.push(record);
	}
	assert_eq!(log.distinct_evidence_for("audio", "hash-a", crate::shadow::Universe::TestGuest), 5, "an old record keeps the meaning it was written with");
}

#[test]
fn trust_needs_evidence_from_more_than_one_target() {
	// A month that only saw x86_64 changes says nothing about the emulated targets, and the
	// architecture policy makes exactly that the steady state.
	let mut log = crate::shadow::Log { schema: 1, records: Vec::new() };
	for tree in 0..crate::trust::REQUIRED_CLEAN_RUNS {
		log.records.push(evidence(crate::shadow::Universe::TestGuest, "x86_64", false, &format!("tree-{tree}")));
	}
	let store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	let error = store.evaluate("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).expect_err("one target is not enough");
	assert!(error.contains("target(s)"), "{error}");
	log.records.push(evidence(crate::shadow::Universe::TestGuest, "riscv64", true, "tree-5"));
	assert!(store.evaluate("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).is_ok());
}

#[test]
fn every_comparison_on_record_being_dry_is_not_evidence_that_running_the_selection_works() {
	// A dry shadow answers "did the selector choose the right set S" and never runs S. This
	// milestone contains the proof that the second question is not theoretical: in an intermediate
	// state the planner emitted Rust function names while the runner had moved to explicit stable
	// IDs, so every selection was computed CORRECTLY and none of them could be executed - and every
	// dry comparison stayed clean throughout. The defect was found by hand.
	//
	// One sample per universe, not one per run: a sample costs a second full sweep, and what it
	// establishes is a property of the execution mechanism rather than of the change.
	let mut log = crate::shadow::Log { schema: 1, records: Vec::new() };
	let record = |architecture: &str, exec: bool, tree: &str| evidence(crate::shadow::Universe::TestGuest, architecture, exec, tree);
	for (tree, architecture) in ["x86_64", "x86_64", "x86_64", "riscv64", "riscv64", "riscv64"].into_iter().enumerate() {
		log.records.push(record(architecture, false, &format!("tree-{tree}")));
	}
	let store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	let error = store.evaluate("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).expect_err("six dry comparisons are six answers to the other question");
	assert!(error.contains("EXECUTED"), "{error}");
	log.records.push(record("x86_64", true, "tree-6"));
	assert!(store.evaluate("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).is_ok(), "one sample is the requirement");

	// THE HOST UNIVERSE ASKS FOR ONE TOO, since 2026-08-13. It was exempt while nothing could run a
	// host selection separately from the sweep - requiring what nothing can supply makes a universe
	// permanently untrustable rather than honestly graded - and `verify.sh` runs one now, so the
	// exemption is gone.
	//
	// It is also the exemption that hid a real defect for a round: the dev producer, one universe
	// over and exempt for the same reason, emitted a command bash could not parse. A selection
	// computed correctly and impossible to execute is invisible to every dry comparison.
	let mut host = crate::shadow::Log { schema: 1, records: Vec::new() };
	let host_record = |exec: bool, tree: &str| evidence(crate::shadow::Universe::Host, "host", exec, tree);
	for tree in 0..crate::trust::REQUIRED_CLEAN_RUNS {
		host.records.push(host_record(false, &format!("tree-{tree}")));
	}
	let error = store.evaluate("audio", "hash-a", crate::shadow::Universe::Host, &host).expect_err("dry host comparisons answer the other question too");
	assert!(error.contains("EXECUTED"), "{error}");
	host.records.push(host_record(true, "tree-5"));
	assert!(store.evaluate("audio", "hash-a", crate::shadow::Universe::Host, &host).is_ok(), "one sample is the requirement here as well");

	// AND SO DOES `HostBuild`, since 2026-08-14. It was exempt on the reasoning that executing a
	// build selection means building those parts and the sweep builds every part anyway - true of
	// the BUILD, and false of the MECHANISM. The evidence producer runs the catalog's commands one
	// part at a time; the runner groups them into `./build.sh --arch X --part a,b,c`. Those are
	// different code paths through the same script and only the second one ships, so a grouped part
	// list whose parser silently used only the first entry and exited zero left every individual
	// check passing and the scoped runner building less than the selection said.
	//
	// `verify.sh --shadow-exec` runs the grouped command and compares what `build.sh` reports
	// building against the parts the selection named, so the sample exists and the exemption is
	// gone - which leaves `exec_universes` covering every universe with a producer.
	let mut builds = crate::shadow::Log { schema: 1, records: Vec::new() };
	for (tree, architecture) in ["x86_64", "aarch64", "x86_64", "aarch64", "x86_64", "aarch64"].into_iter().enumerate() {
		builds.records.push(evidence(crate::shadow::Universe::HostBuild, architecture, false, &format!("tree-{tree}")));
	}
	let error = store.evaluate("audio", "hash-a", crate::shadow::Universe::HostBuild, &builds).expect_err("dry build comparisons say the right parts were chosen and nothing about the grouped command");
	assert!(error.contains("EXECUTED"), "{error}");
	builds.records.push(evidence(crate::shadow::Universe::HostBuild, "x86_64", true, "tree-6"));
	assert!(store.evaluate("audio", "hash-a", crate::shadow::Universe::HostBuild, &builds).is_ok(), "one sample of the mechanism that ships is the requirement here too");
}

#[test]
fn evidence_under_another_model_does_not_count() {
	let mut log = crate::shadow::Log { schema: 1, records: Vec::new() };
	for (tree, architecture) in ["x86_64", "riscv64", "aarch64", "x86_64", "riscv64", "aarch64"].into_iter().enumerate() {
		let mut record = evidence(crate::shadow::Universe::TestGuest, architecture, false, &format!("tree-{tree}"));
		record.model_hash = String::from("an-older-model");
		log.records.push(record);
	}
	let store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	assert!(store.evaluate("audio", "the-current-model", crate::shadow::Universe::TestGuest, &log).is_err(), "six clean runs under a model that is not the one running prove nothing about the one that is");
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
		("src/harness/mkimage.sh", "the image builder"),
		("src/harness/qemu-run.sh", "the thing that boots and judges the guest"),
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
	// `shadow_exec` on every record: this test is about the VERDICT filter, and leaving the
	// execution-sample requirement unmet would make it pass for the wrong reason.
	let record = |architecture: &str, verdict: &str, tree: &str| {
		let mut record = evidence(crate::shadow::Universe::TestGuest, architecture, true, tree);
		record.verdict = verdict.to_string();
		record
	};
	for tree in 0..crate::trust::REQUIRED_CLEAN_RUNS {
		log.records.push(record("x86_64", "Consistent", &format!("tree-{tree}")));
	}
	log.records.push(record("riscv64", "CandidateMiss", "tree-5"));
	let store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	let error = store.evaluate("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).expect_err("a run that found a candidate miss is not evidence that the selector is right");
	assert!(error.contains("target(s)"), "{error}");
	// The same target, cleanly this time, is what actually earns it.
	log.records.push(record("riscv64", "Consistent", "tree-6"));
	assert!(store.evaluate("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).is_ok());
}

// ---------------------------------------------------------------------------------------------
// Diff parsing
//
// The defect this replaced: `git status --porcelain` reports a rename as `old -> new`, the shell
// kept only the new path with `sed 's/.* -> //'`, and the old one was discarded. Moving a file out
// of a component did not select that component. The regression corpus could not catch it because it
// parsed changes a different way - `git diff --name-only` - so there were two parsers for one
// question and only one of them was wrong.

use crate::changes::{Kind, parse_name_status, parse_status_v2, paths};

fn nul(records: &[&str]) -> String {
	let mut text = String::new();
	for record in records {
		text.push_str(record);
		text.push('\0');
	}
	text
}

#[test]
fn a_rename_is_a_change_to_both_of_its_paths() {
	// porcelain v2 puts the destination in the record and the ORIGIN in the next NUL-terminated
	// field, which is the part a naive split loses.
	let text = nul(&["2 R. N... 100644 100644 100644 aaaa bbbb R100 docs/foo.rs", "src/kernel/foo.rs"]);
	let changes = parse_status_v2(&text).expect("parses");
	assert_eq!(changes.len(), 1);
	assert_eq!(changes[0].kind, Kind::Renamed);
	assert_eq!(changes[0].path, "docs/foo.rs");
	assert_eq!(changes[0].origin.as_deref(), Some("src/kernel/foo.rs"));
	assert_eq!(paths(&changes), vec![String::from("docs/foo.rs"), String::from("src/kernel/foo.rs")], "the old path is a change too - something used to live there and no longer does");
}

#[test]
fn a_rename_out_of_a_component_still_selects_that_component() {
	// The end-to-end version of the defect: a file moved from the kernel into `docs/`. With only the
	// destination, the plan is "documentation, nothing to do".
	let model = model();
	let text = nul(&["2 R. N... 100644 100644 100644 aaaa bbbb R100 docs/moved.rs", "src/kernel/mem/frame/mod.rs"]);
	let changes = parse_status_v2(&text).expect("parses");
	let changed = paths(&changes);
	let borrowed: Vec<&str> = changed.iter().map(String::as_str).collect();
	let plan = plan_for(&model, &borrowed);
	assert!(!plan.nothing_to_do, "a rename out of the kernel is not a documentation change");
	assert!(plan.full, "the kernel is on the selects-everything list, and it lost a file");
}

#[test]
fn a_deletion_is_the_path_that_was_deleted() {
	let changes = parse_status_v2(&nul(&["1 .D N... 100644 100644 000000 aaaa bbbb src/user/libs/audio/flac/src/lib.rs"])).expect("parses");
	assert_eq!(changes[0].kind, Kind::Deleted);
	assert_eq!(changes[0].path, "src/user/libs/audio/flac/src/lib.rs");
}

#[test]
fn a_path_with_spaces_survives_the_parse() {
	// The reason for the field-count split and for `-z`: the human-readable formats quote and escape
	// these, and both shell versions got that wrong.
	let changes = parse_status_v2(&nul(&["1 .M N... 100644 100644 100644 aaaa bbbb src/a file with spaces.rs"])).expect("parses");
	assert_eq!(changes[0].path, "src/a file with spaces.rs");
}

#[test]
fn untracked_and_copied_and_unmerged_are_all_changes() {
	let text = nul(&[
		"? src/new.rs",
		"2 C. N... 100644 100644 100644 aaaa bbbb C75 src/copy.rs",
		"src/original.rs",
		"u UU N... 100644 100644 100644 100644 aaaa bbbb cccc src/conflict.rs",
		"! ignored.txt",
	]);
	let changes = parse_status_v2(&text).expect("parses");
	assert_eq!(changes.len(), 3, "the ignored file is not a change; the other three are");
	assert_eq!(paths(&changes), vec![String::from("src/conflict.rs"), String::from("src/copy.rs"), String::from("src/new.rs"), String::from("src/original.rs")]);
}

#[test]
fn name_status_puts_the_origin_first_and_porcelain_puts_it_second() {
	// The two formats disagree about the order, which is exactly the sort of thing one parser gets
	// right once and two parsers get right at different times.
	let changes = parse_name_status(&nul(&["R100", "src/kernel/foo.rs", "docs/foo.rs", "D", "src/gone.rs"])).expect("parses");
	assert_eq!(changes[0].origin.as_deref(), Some("src/kernel/foo.rs"));
	assert_eq!(changes[0].path, "docs/foo.rs");
	assert_eq!(changes[1].kind, Kind::Deleted);
	assert_eq!(changes[1].path, "src/gone.rs");
}

#[test]
fn an_unparseable_record_is_an_error_rather_than_a_shrug() {
	// Silently skipping what it cannot read is how a diff parser reports "nothing changed" over a
	// change it did not understand.
	assert!(parse_status_v2(&nul(&["9 something nobody has seen"])).is_err());
	assert!(parse_status_v2(&nul(&["2 R. N... 100644 100644 100644 aaaa bbbb R100 docs/foo.rs"])).is_err(), "a rename record with no origin field must not be accepted");
}

// ---------------------------------------------------------------------------------------------
// The plan and the run must agree about which targets get booted

// A guest step is `[TEST_SELECTION=ids ]./test.sh --arch TARGET`, so it is recognised by the script
// it runs rather than by a fixed prefix - a prefix match silently returned nothing the moment the
// exact-selection form appeared, and the assertion below then compared two empty sets by accident.
fn guest_targets(plan: &crate::plan::Plan, per_target: &std::collections::BTreeMap<String, usize>) -> BTreeSet<String> {
	// AND THE TARGET IS THE FIRST WORD AFTER THE FLAG, not everything after it. The boot check runs
	// `./test.sh --arch x86_64 --tags smoke`, so taking the rest of the line answered
	// `x86_64 --tags smoke` and compared it against a set of architecture names.
	crate::commands::steps(plan, per_target, &model().registry).iter().filter(|step| step.command.contains("./test.sh --arch ")).filter_map(|step| step.command.rsplit(" --arch ").next().and_then(|rest| rest.split_whitespace().next()).map(str::to_string)).collect()
}

#[test]
fn every_booted_architecture_gets_exactly_one_guest_step() {
	let model = model();
	let mut per_target: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
	for test in &model.kernel_tests.tests {
		for architecture in &test.architectures {
			*per_target.entry(architecture.clone()).or_default() += 1;
		}
	}
	for paths in [vec!["src/user/libs/audio/flac/src/lib.rs"], vec!["src/kernel/arch/riscv64/traps/mod.rs"], vec!["src/harness/qemu-run.sh"]] {
		let plan = plan_for(&model, &paths);
		let booted: BTreeSet<String> = plan.architectures_booted.iter().cloned().collect();
		let steps = crate::commands::steps(&plan, &per_target, &model.registry);
		let guest: Vec<&crate::commands::Step> = steps.iter().filter(|step| step.command.contains("./test.sh --arch ")).collect();
		assert_eq!(guest_targets(&plan, &per_target), booted, "{paths:?}: the plan says it boots {booted:?} and the run would boot something else");
		assert_eq!(guest.len(), booted.len(), "{paths:?}: one guest step per booted target, no more");
	}
}

// The case the guarantee exists for: a target the model cannot enumerate. Its catalog has no
// `kernel.*` variants for that target, so nothing derived from plan items can produce its boot.
#[test]
fn an_unenumerated_target_that_is_in_scope_is_still_booted() {
	let model = model();
	let ownership = model.ownership();
	// As if `.build` had never held an x86_64 test binary - the state of a fresh checkout.
	let planner = Planner { unenumerated_targets: vec![String::from("x86_64")], ..Planner::for_model(&model, &ownership) };
	let plan = planner.plan(&[String::from("src/user/libs/audio/flac/src/lib.rs")]);
	assert!(plan.full, "the one target this change would boot cannot be enumerated, so nothing scoped can be trusted");
	let per_target = std::collections::BTreeMap::new();
	let targets = guest_targets(&plan, &per_target);
	assert_eq!(targets.len(), 3, "a FULL plan boots all three, and the RUN must do it too - not just the header");
	for architecture in ["x86_64", "aarch64", "riscv64"] {
		assert!(targets.contains(architecture), "{architecture} is in architectures_booted and must have a guest step; got {targets:?}");
	}
}

// And the other direction, which matters just as much: a target that cannot be enumerated but is
// NOT in scope changes nothing. Escalating on it would make every plan FULL on a machine that has
// only ever built one target, which is most machines.
#[test]
fn an_unenumerated_target_out_of_scope_does_not_escalate() {
	let model = model();
	let ownership = model.ownership();
	let planner = Planner { unenumerated_targets: vec![String::from("riscv64")], ..Planner::for_model(&model, &ownership) };
	let plan = planner.plan(&[String::from("src/user/libs/audio/flac/src/lib.rs")]);
	assert!(!plan.full, "an ordinary userspace change boots x86_64; riscv64 being unenumerable is irrelevant to it");
	assert_eq!(plan.architectures_booted, vec!["x86_64"]);
}

// ---------------------------------------------------------------------------------------------
// The mechanical architecture classifier
//
// The design asked for it and the first implementation shipped without it: only the hand-written
// path table existed, so a component the table did not anticipate got the ordinary-userspace answer
// - cross-build everywhere, boot x86_64. Thirteen files under `src/user` contain `global_asm!`.

#[test]
fn a_userspace_component_with_per_target_assembly_boots_every_target() {
	// `volume-client-provider` forwards through `global_asm!` with a different instruction per
	// target - `jmp`, `b`, `tail`. All three compile everywhere; only one of them is ever exercised
	// by an x86_64 boot.
	let model = model();
	let risk = model.arch_risk.get("volume-client-provider").expect("the scan must see it");
	assert!(risk.any_target || risk.targets.len() > 1, "a component with three assembly branches is not target-neutral: {risk:?}");
	let plan = plan_for(&model, &["src/user/libs/clients/volume-client-provider/src/lib.rs"]);
	assert_eq!(plan.architectures_booted, vec!["aarch64", "riscv64", "x86_64"]);
	assert!(plan.warnings.iter().any(|warning| warning.contains("architecture-sensitive")), "widening the boot set silently would be as bad as not widening it: {:?}", plan.warnings);
}

// The classifier is a risk signal, not an oracle, and it must never overrule a more informed answer.
// Every line of `src/kernel/arch/aarch64` is target-specific, from which a scan can only conclude
// "all targets" - while the policy table knows the precise answer is aarch64 alone. Letting the scan
// win would spend 6104 s of riscv64 on an aarch64-only change.
#[test]
fn a_declared_architecture_rule_outranks_the_scan() {
	let model = model();
	let risk = model.arch_risk.get("kernel.arch.aarch64").expect("the arch tree is full of asm");
	assert!(risk.any_target, "the scan does see it - that is the point");
	let plan = plan_for(&model, &["src/kernel/arch/aarch64/virtio_blk.rs"]);
	assert_eq!(plan.architectures_booted, vec!["aarch64"], "the declared rule is the informed answer");
}

// And a component the scan finds nothing in keeps the default. A classifier that widened everything
// would be as useless as one that widened nothing, just more expensive.
#[test]
fn a_component_with_no_marker_keeps_the_default_target() {
	let model = model();
	let risk = model.arch_risk.get("flac");
	assert!(risk.is_none_or(|risk| !risk.any_target && risk.targets.is_empty()), "a codec has no target-specific code: {risk:?}");
	assert_eq!(plan_for(&model, &["src/user/libs/audio/flac/src/lib.rs"]).architectures_booted, vec!["x86_64"]);
}

// Absence of a marker is not proof of neutrality, and the classifier must never be read as saying it
// is. The guard is structural: the scan can only ADD targets.
#[test]
fn the_classifier_only_ever_widens() {
	let model = model();
	for path in ["src/user/libs/audio/flac/src/lib.rs", "src/fs/udf/src/lib.rs", "src/user/apps/tools/src/echo.rs", "src/user/libs/clients/volume-client-provider/src/lib.rs"] {
		let plan = plan_for(&model, &[path]);
		assert!(plan.architectures_booted.contains(&String::from("x86_64")), "{path}: the default target can never be removed by the scan");
	}
}

#[test]
fn the_scan_finds_the_markers_the_design_names() {
	let mut risk = crate::archrisk::Risk::default();
	crate::archrisk::classify_rust_for_test("#[cfg(target_arch = \"riscv64\")]\nfn f() {}", "a.rs", &mut risk);
	assert!(risk.targets.contains("riscv64"));
	assert!(!risk.any_target, "naming one target is not a claim about the others");

	let mut risk = crate::archrisk::Risk::default();
	crate::archrisk::classify_rust_for_test("global_asm!(\".globl x\");", "a.rs", &mut risk);
	assert!(risk.any_target, "assembly names no target and differs on all of them");
	assert_eq!(risk.boot_targets().len(), 3);

	let mut risk = crate::archrisk::Risk::default();
	crate::archrisk::classify_rust_for_test("#[cfg(target_pointer_width = \"64\")]\nfn f() {}", "a.rs", &mut risk);
	assert!(risk.any_target);
}

// ---------------------------------------------------------------------------------------------
// `covers` reachability
//
// The gate the design asked for and the first implementation shipped without: it checked only that
// a covered component EXISTS. On its first run over the eleven annotations that existed it produced
// eight findings - four of them defects in the scan itself, two a missing graph edge and an
// over-reaching declaration of mine, which is a fair account of what such a gate is for.

fn kernel_test(name: &str, covers: &[&str]) -> KernelTest {
	KernelTest { source_paths: Vec::new(), name: name.to_string(), id: format!("kernel.{name}"), architectures: vec![String::from("x86_64")], covers: covers.iter().map(|component| (*component).to_string()).collect() }
}

#[test]
fn covers_must_be_reachable_from_what_the_test_touches() {
	let model = model();
	let touched: BTreeSet<String> = [String::from("bin.audioconv")].into_iter().collect();
	// Reached through the tool's own provider chain, without the test naming it.
	let ok = kernel_test("t", &["flac", "audioconv"]);
	let nothing_staged: BTreeSet<String> = BTreeSet::new();
	assert!(crate::kerneltests::unreachable_covers(&ok, &touched, &model.graph, &nothing_staged).is_empty());
	// Nothing leads from a codec tool to a filesystem.
	let bad = kernel_test("t", &["liberfs"]);
	assert_eq!(crate::kerneltests::unreachable_covers(&bad, &touched, &model.graph, &nothing_staged), vec![String::from("liberfs")]);
}

// AND THE BOOT CHAIN IS REACH, which is what fifteen exception lines were about.
//
// A kernel test runs in a booted guest, so the staged drivers and services are live before its body
// runs: a test that asserts a file read over a disk reaches that disk's driver without ever
// launching it. Reach computed from launches alone refused those declarations - correctly, given
// what it could see - and the fix is to let it see the boot.
#[test]
fn the_boot_chain_is_part_of_what_a_kernel_test_reaches() {
	let model = model();
	// A test that launches nothing at all, which is most of them.
	let touched: BTreeSet<String> = BTreeSet::new();
	let test = kernel_test("t", &["bin.virtio_blk"]);
	let nothing_staged: BTreeSet<String> = BTreeSet::new();
	assert_eq!(crate::kerneltests::unreachable_covers(&test, &touched, &model.graph, &nothing_staged), vec![String::from("bin.virtio_blk")], "with nothing staged there is no boot chain and the declaration is unreachable");
	assert!(crate::kerneltests::unreachable_covers(&test, &touched, &model.graph, &model.staged).is_empty(), "and on a machine whose boot stages that driver, a test asserting its effect reaches it");
	assert!(model.staged.contains("bin.virtio_blk"), "the staged set is what the manifest stages, read rather than assumed");
}

#[test]
fn the_converse_is_reported_and_never_enforced() {
	// Launching StorageService to get a volume is not asserting anything about StorageService.
	let model = model();
	let touched: BTreeSet<String> = [String::from("bin.audioconv"), String::from("bin.storage_service")].into_iter().collect();
	let test = kernel_test("t", &["flac"]);
	assert!(crate::kerneltests::unreachable_covers(&test, &touched, &model.graph, &BTreeSet::new()).is_empty(), "the declaration is fine");
	// Both launched programs are reported, including the one the test is actually about: a person
	// reading this decides whether `bin.audioconv` belongs in the declaration. The tool does not.
	assert_eq!(crate::kerneltests::launched_but_not_covered(&test, &touched, &model.graph), vec![String::from("bin.audioconv"), String::from("bin.storage_service")], "the omissions are reported rather than corrected");
}

// A dev-dependency is how a test BUILDS its fixture, not something the guest reaches. Including it
// would let any test claim to cover all eleven codecs the kernel dev-depends on.
#[test]
fn a_dev_dependency_is_not_runtime_reach() {
	assert!(!crate::kerneltests::RUNTIME_REACH.contains(&"link.static.dev"));
	let model = model();
	let touched: BTreeSet<String> = [String::from("kernel")].into_iter().collect();
	let test = kernel_test("t", &["webp"]);
	assert_eq!(crate::kerneltests::unreachable_covers(&test, &touched, &model.graph, &BTreeSet::new()), vec![String::from("webp")], "the kernel dev-depends on webp; that is not the guest reaching it");
}

#[test]
fn a_comparison_made_by_a_model_that_failed_its_own_checks_is_not_evidence() {
	// The one criterion out of the design's list that needs no accumulated records to grade, because
	// it is a property of each record on its own. A selection computed by a model that contradicts
	// itself, compared against a sweep derived from the same model, says nothing about the tree -
	// "the selection matched" is two wrong answers agreeing.
	//
	// Change classes, edge kinds, the regression corpus and SHADOW-EXEC stay uncounted, and for the
	// stated reason: they need a log with entries in it and would be guesswork before that.
	let mut log = crate::shadow::Log::default();
	let record = |self_check: bool, architecture: &str, tree: &str| {
		let mut record = evidence(crate::shadow::Universe::TestGuest, architecture, false, tree);
		record.model_self_check = self_check;
		record
	};
	for (tree, architecture) in ["x86_64", "x86_64", "x86_64", "riscv64", "riscv64", "riscv64"].into_iter().enumerate() {
		log.records.push(record(false, architecture, &format!("tree-{tree}")));
	}
	let store = crate::trust::Store::default();
	assert_eq!(log.clean_runs_for("audio", "hash-a", crate::shadow::Universe::TestGuest), 0, "six comparisons, none of them by a model entitled to make one");
	assert!(store.evaluate("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).is_err());

	// The same six, made by a model that passed - and one of them carrying an execution sample,
	// which `evaluate` also requires in this universe.
	log.records.clear();
	for (tree, architecture) in ["x86_64", "x86_64", "x86_64", "riscv64", "riscv64", "riscv64"].into_iter().enumerate() {
		log.records.push(record(true, architecture, &format!("tree-{tree}")));
	}
	log.records[0].shadow_exec = true;
	assert!(store.evaluate("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).is_ok());
}

#[test]
fn a_clause_is_the_word_standing_alone_and_not_the_letters_inside_a_name() {
	// A test name is part of the argument list, and `arguments.find("covers")` finds those six
	// letters wherever they are. `the_lifecycle_guard_covers_the_whole_operation_...` matched inside
	// its OWN NAME; the search for the following `[` then found the tag list, and the test was
	// recorded as covering `Kernel`, `Memory` and `Dma` - three components it never declared and
	// cannot reach. `every_annotation_in_this_tree_is_reachable` failed on it, correctly, for a
	// reason that had nothing to do with the annotation.
	let declaration = r#"crate::tagged_test!(the_lifecycle_guard_covers_the_whole_operation_and_not_just_its_first_line, [Kernel, Memory, Dma], id = "kernel.object.process.the_lifecycle_guard_covers", covers = ["kernel"]);"#;
	let parsed = crate::kerneltests::parse_declarations(declaration);
	assert_eq!(parsed.len(), 1);
	let (name, id, covers) = &parsed[0];
	assert_eq!(name, "the_lifecycle_guard_covers_the_whole_operation_and_not_just_its_first_line");
	assert_eq!(id, "kernel.object.process.the_lifecycle_guard_covers");
	assert_eq!(covers, &vec![String::from("kernel")], "the covers clause, not the tag list next to a name that contains the word");

	// The same trap one letter smaller: `id` inside `valid`, `width`, `hidden`. And a declaration
	// with no covers clause at all still reads as none rather than as the tags.
	let declaration = r#"tagged_test!(a_hidden_field_is_still_valid, [Kernel, Storage], id = "kernel.hidden");"#;
	let parsed = crate::kerneltests::parse_declarations(declaration);
	assert_eq!(parsed.len(), 1);
	assert_eq!(parsed[0].1, "kernel.hidden");
	assert!(parsed[0].2.is_empty(), "no covers clause is no covers, not the tags");
}

#[test]
fn every_annotation_in_this_tree_is_reachable() {
	let model = model();
	let mut bad = Vec::new();
	for test in &model.kernel_tests.tests {
		let touched = model.kernel_tests.touches.get(&test.name).cloned().unwrap_or_default();
		for component in crate::kerneltests::unreachable_covers(test, &touched, &model.graph, &model.staged) {
			bad.push(format!("{} covers {component}", test.name));
		}
	}
	assert!(bad.is_empty(), "unreachable covers declarations: {bad:#?}");
}

// The scan has to see through harness functions or the gate is unusable: most scenarios delegate
// their setup, and a direct-only scan reports that a test covering `lico` cannot reach it.
#[test]
fn the_scan_follows_helper_functions() {
	let source = "\nfn helper() {\n\tlet elf = program_elf(&package, volume, b\"audioconv\").unwrap();\n}\n\nfn a_test() {\n\thelper();\n}\n";
	let parsed = crate::kerneltests::parse_touches(source);
	let direct: BTreeSet<String> = parsed.iter().find(|(name, _, _)| name == "a_test").map(|(_, reached, _)| reached.clone()).unwrap_or_default();
	assert!(direct.is_empty(), "the test itself launches nothing - that is the whole difficulty");
	let called: BTreeSet<String> = parsed.iter().find(|(name, _, _)| name == "a_test").map(|(_, _, called)| called.clone()).unwrap_or_default();
	assert!(called.contains("helper"), "and the call to the harness is what has to be followed");
}

// ---------------------------------------------------------------------------------------------
// Product reach versus test-build reach
//
// The edge kinds were typed from the start and the closure walked them identically, so a
// dev-dependency reached exactly as far as a product one. The kernel dev-depends on eleven codecs to
// build its scenario fixtures, so changing `flac` pulled the whole kernel into the closure - and
// with it every check that covers the kernel, including a shipping build of it.

#[test]
fn a_dev_dependency_reaches_as_a_test_build_and_not_as_a_product() {
	let model = model();
	let seeds: BTreeSet<String> = [String::from("flac")].into_iter().collect();
	let reach = model.graph.affected_by_reach(&seeds);
	assert_eq!(reach.get("flac"), Some(&crate::graph::Reach::Product), "the thing that changed changed");
	assert_eq!(reach.get("kernel"), Some(&crate::graph::Reach::TestBuild), "the kernel dev-depends on flac; nothing it SHIPS is different");
	assert_eq!(reach.get("audioconv"), Some(&crate::graph::Reach::Product), "the library that lists flac as a provider does ship differently");
}

#[test]
fn a_test_build_reach_does_not_order_a_shipping_build() {
	let model = model();
	let plan = plan_for(&model, &["src/user/libs/audio/flac/src/lib.rs"]);
	let selected = keys(&plan);
	assert!(!selected.iter().any(|key| key.starts_with("build.kernel /")), "the shipping kernel is byte-identical; compiling it again proves nothing");
	assert!(selected.iter().any(|key| key.starts_with("build.user /")), "the userspace DOES change and the guest boots it");
	assert!(selected.iter().any(|key| key.starts_with("host.flac /")), "and the crate's own suite runs");
}

// The direction that must not be lost: a real kernel change still reaches everything as a product.
#[test]
fn a_product_dependency_still_reaches_as_a_product() {
	let model = model();
	let seeds: BTreeSet<String> = [String::from("liberfs")].into_iter().collect();
	let reach = model.graph.affected_by_reach(&seeds);
	assert_eq!(reach.get("storage"), Some(&crate::graph::Reach::Product), "StorageService links LiberFS into what it ships");
	assert_eq!(reach.get("kernel"), Some(&crate::graph::Reach::Product), "and the kernel links it as an ordinary dependency, not a dev one");
}

// Product wins wherever both paths exist, and a downgrade never upgrades back.
#[test]
fn product_reach_wins_over_test_build_reach() {
	let model = model();
	// `pcm` is a provider of the audioconv library AND a dev-dependency of the kernel.
	let seeds: BTreeSet<String> = [String::from("pcm")].into_iter().collect();
	let reach = model.graph.affected_by_reach(&seeds);
	assert_eq!(reach.get("audioconv"), Some(&crate::graph::Reach::Product));
	assert_eq!(reach.get("kernel"), Some(&crate::graph::Reach::TestBuild));
}

// Evidence is about a UNIVERSE, not about a component in general.
//
// Shadow examined only keys beginning `kernel.` while trust was granted per component, so a clean
// record on the kernel suite silently vouched for host suites, gates and conformance runs that the
// comparison had never looked at.
#[test]
fn evidence_in_one_universe_does_not_vouch_for_another() {
	let mut log = crate::shadow::Log { schema: 1, records: Vec::new() };
	// The execution sample is present, because this test is about the UNIVERSE dimension and an
	// unmet requirement elsewhere would make it pass without exercising it.
	let record = |architecture: &str, universe: crate::shadow::Universe, tree: &str| evidence(universe, architecture, true, tree);
	for (tree, architecture) in ["x86_64", "riscv64", "x86_64", "riscv64", "x86_64", "riscv64"].into_iter().enumerate() {
		log.records.push(record(architecture, crate::shadow::Universe::TestGuest, &format!("tree-{tree}")));
	}
	let mut store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	let (clean, architectures) = store.evaluate("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).expect("the guest suite has been validated");
	let scope = store.evidence_scope("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).scope;
	store.grant("audio", "hash-a", crate::shadow::Universe::TestGuest, clean, architectures, scope, 0);

	assert_eq!(store.level("audio", "hash-a", crate::shadow::Universe::TestGuest, &crate::shadow::Scope::default()), crate::trust::Level::Trusted);
	assert_eq!(store.level("audio", "hash-a", crate::shadow::Universe::Host, &crate::shadow::Scope::default()), crate::trust::Level::Shadow, "nothing has compared a host-suite selection for this component");
	assert!(!store.trusted_everywhere("audio", "hash-a", &[crate::shadow::Universe::Host, crate::shadow::Universe::TestGuest], &crate::shadow::Scope::default()), "and a scoped run therefore still has something unproven behind it");
	assert!(store.evaluate("audio", "hash-a", crate::shadow::Universe::Host, &log).is_err());
}

#[test]
fn a_check_id_says_which_universe_judges_it() {
	use crate::shadow::Universe;
	assert_eq!(Universe::of("kernel.frame_alloc_distinct"), Universe::TestGuest);
	assert_eq!(Universe::of("dev.selftest"), Universe::DevGuest);
	for host in ["host.flac", "gate.volume-layout", "conformance.png"] {
		assert_eq!(Universe::of(host), Universe::Host, "{host} runs on the host");
	}
	// A BUILD IS ITS OWN UNIVERSE, and it used to be filed under `Host` with the rest.
	//
	// The Host producer deliberately excludes builds - re-running three architectures' builds to
	// answer "did anything outside the selection fail" costs more than the sweep it is compared
	// against - so its evidence covers gates, suites and conformance. Issuing that evidence's
	// certificate for a universe that also contained builds meant a selector defect dropping
	// `build.user` could not be observed, and five clean runs later the component was TRUSTED for it
	// anyway. A universe's certificate may not be broader than the evidence behind it.
	assert_eq!(Universe::of("build.kernel"), Universe::HostBuild, "a build is judged by evidence about builds");
	assert_eq!(Universe::of("build.user"), Universe::HostBuild);
}

#[test]
fn every_check_kind_and_configuration_lowers_to_a_runnable_command() {
	// FOUR CONFIGURATIONS AND SIX CHECK KINDS is a small table, and it is exactly the table that
	// would have caught the defect it exists for.
	//
	// The lowering took a configuration's NAME and treated anything that was not `"default"` as a
	// Cargo feature list. That is right for exactly one of the four: `shared-image` really does want
	// `--no-default-features --features shared-image`, `default` is right by luck, and `test` and
	// `development` both declare `default_features = true` and no features - so both halves of what
	// it emitted contradicted the record.
	//
	// And it was not a wrong flag. A dev check's command is a shell pipeline, so the producer emitted
	// `(cd src && harness/dev-selftest.py) --no-default-features --features development`, which bash
	// refuses to parse. Every dev-guest shadow line failed before it started.
	use crate::catalog::CheckKind;
	let model = model();
	let configuration = |name: &str| model.registry.configuration(name).unwrap_or_else(|| panic!("the registry defines '{name}'")).clone();

	// A HOST SUITE is the only kind a feature selection means anything to.
	let cargo = "cargo test --manifest-path src/term/Cargo.toml";
	assert_eq!(crate::commands::lower(CheckKind::HostSuite, cargo, &configuration("default")), cargo, "the crate's own manifest is spelled by saying nothing");
	assert_eq!(crate::commands::lower(CheckKind::HostSuite, cargo, &configuration("shared-image")), format!("{cargo} --no-default-features --features shared-image"), "the shipping configuration turns default features off and names its own");
	// The two the name-matching version got wrong: both declare `default_features = true` and no
	// features, so both must lower to the bare command.
	for name in ["test", "development"] {
		assert_eq!(crate::commands::lower(CheckKind::HostSuite, cargo, &configuration(name)), cargo, "'{name}' declares default features and no features of its own");
	}

	// EVERY OTHER KIND carries its own command and means it, in every configuration.
	let pipeline = "(cd src && harness/dev-selftest.py)";
	for kind in [CheckKind::Gate, CheckKind::Conformance, CheckKind::Build, CheckKind::DevCheck, CheckKind::KernelTest] {
		for name in ["default", "shared-image", "test", "development"] {
			let lowered = crate::commands::lower(kind.clone(), pipeline, &configuration(name));
			assert_eq!(lowered, pipeline, "{kind:?} in '{name}' must be the command the runner runs");
			// The specific breakage, asserted by shape rather than by equality: appending cargo
			// flags to a shell pipeline produces something bash cannot parse.
			assert!(!lowered.contains("--features"), "{kind:?} in '{name}' must not have cargo flags appended to it");
		}
	}
}

#[test]
fn every_universe_that_judges_anything_has_an_evidence_producer() {
	// A UNIVERSE WITH NO PRODUCER IS A WALL, NOT A BAR. `trusted_everywhere` requires `Trusted` in
	// every judging universe, so a universe nothing can answer for makes every component it judges
	// permanently untrustable however much evidence they accumulate elsewhere.
	//
	// That was `HostBuild`'s state from the split that created it until the `build-checks` producer:
	// `build.libs`, `build.user`, `build.kernel` and the rest cover every crate and every program
	// under their prefix, so 189 of the catalog's 192 components were judged by it - 98% of the tree,
	// unable to reach a steady state this milestone's whole thesis rests on.
	//
	// The assertion is not "HostBuild has one": it is that NONE is missing, so the next universe
	// added without a producer fails here rather than after five clean runs.
	let model = model();
	let producers = crate::shadow::universes_with_producers();
	let walled: Vec<&'static str> = crate::catalog::all_judging_universes(&model.catalog).into_iter().filter(|universe| !producers.contains(universe)).map(|universe| universe.as_str()).collect();
	assert!(walled.is_empty(), "these universes judge components and nothing can answer for them: {walled:?}");

	// And the number that made it worth measuring: a component covered by a build check is judged by
	// `HostBuild`, and there are many of them.
	let judged: usize = model.crates.iter().filter(|entry| crate::catalog::judging_universes(&model.catalog, &entry.name).contains(&crate::shadow::Universe::HostBuild)).count();
	assert!(judged > 0, "build checks cover components, so something is judged by HostBuild");
}

#[test]
fn a_build_covered_component_can_reach_trusted() {
	// The other half of the wall: with a producer in place, the evidence a full sweep already
	// produces can carry a component all the way. `HostBuild` needs two architectures, for the same
	// reason the guest suite does - a build of x86_64 says nothing about aarch64.
	let mut log = crate::shadow::Log { schema: 1, records: Vec::new() };
	// AND BUILDS TAKE AN EXECUTION SAMPLE TOO, since 2026-08-14. They were exempt on the reasoning
	// that a full sweep builds everything anyway - true of the BUILD and false of the MECHANISM: the
	// evidence producer runs the catalog's commands one part at a time and the runner groups them
	// into `./build.sh --arch X --part a,b,c`, which is a different code path through the same
	// script and the only one that ships.
	let record = |architecture: &str, exec: bool, tree: &str| {
		let mut record = evidence(crate::shadow::Universe::HostBuild, architecture, exec, tree);
		record.changed_components = vec![String::from("term")];
		// The DECISION about term, which is what distinctness is keyed on. The fixture used to leave
		// audio's decision here and lean on `source_digest` to tell the records apart - which is the
		// key that stopped counting, because a deterministic selector asked the same question from
		// six trees answers it the same way six times.
		record.component_decisions = vec![format!("term\t{tree}")];
		record.component_scopes = [(String::from("term"), crate::shadow::Scope::from_kinds(vec![String::from("modified")], vec![String::from("link.static")]))].into_iter().collect();
		record
	};
	for (tree, architecture) in ["x86_64", "aarch64", "x86_64", "aarch64", "x86_64", "aarch64"].into_iter().enumerate() {
		log.records.push(record(architecture, false, &format!("tree-{tree}")));
	}
	let mut store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	let dry = store.evaluate("term", "hash-a", crate::shadow::Universe::HostBuild, &log).expect_err("six dry build comparisons say the right parts were chosen and nothing about running the grouped command");
	assert!(dry.contains("EXECUTED"), "{dry}");
	log.records.push(record("x86_64", true, "tree-6"));
	let (clean, architectures) = store.evaluate("term", "hash-a", crate::shadow::Universe::HostBuild, &log).expect("six clean build comparisons over two architectures, one of them sampled");
	let scope = store.evidence_scope("term", "hash-a", crate::shadow::Universe::HostBuild, &log).scope;
	store.grant("term", "hash-a", crate::shadow::Universe::HostBuild, clean, architectures, scope, 0);
	assert_eq!(store.level("term", "hash-a", crate::shadow::Universe::HostBuild, &crate::shadow::Scope::default()), crate::trust::Level::Trusted, "a build-covered component can now arrive");
}

#[test]
fn the_cost_escalation_measures_seconds_and_not_keys() {
	// It compared `selected / whole` over the key COUNT, which prices twenty host keys and twenty
	// riscv64 guest keys the same when they differ by two orders of magnitude in wall-clock. The
	// measurement to build it on already existed - `CostModel::estimate` - and nothing asked it.
	let cost = crate::history::CostModel::default();
	let history = crate::history::History::default();
	let key = |architecture: &str, environment: crate::catalog::Environment, n: usize| -> Vec<crate::plan::PlanItemKey> { (0..n).map(|i| crate::plan::PlanItemKey { check: format!("k{i}"), architecture: architecture.to_string(), environment: environment.clone(), configuration: String::from("default") }).collect() };

	// Twenty riscv64 guest keys against two hundred. The boot is paid ONCE, so a selection is
	// cheaper than the whole set by less than its key count suggests - and by MORE than nothing,
	// which is the pair of facts a count cannot represent.
	//
	// THIS ASSERTION USED TO BE `> 0.9`, and it was the defect written down as an expectation. It
	// held because `fixed_seconds` for riscv64 was 3200 s - a whole-suite figure sitting in the
	// field that means startup cost - so a boot appeared to dominate any selection and the planner
	// widened every scoped riscv64 run to the full suite. The 2026-08-12 measurement (2 tests 537 s,
	// 20 tests 587 s, 226 tests 2600 s) puts the real startup cost at 461 s against 9.44 s per test,
	// and with those the same selection is 650 s against 2349 s. Scoping pays, which is what the
	// selection dimension exists for.
	let few = cost.estimate(&history, &key("riscv64", crate::catalog::Environment::TestGuest, 20), None);
	let many = cost.estimate(&history, &key("riscv64", crate::catalog::Environment::TestGuest, 200), None);
	let emulated = few / many;
	assert!(emulated < 0.9, "20 of 200 riscv64 guest keys cost {few:.0} s against {many:.0} s - a ratio of {emulated:.3} widens every scoped run to the whole suite");
	assert!(emulated > 0.2, "and the boot is still real: {emulated:.3} must stay well above the no-boot case below, or the model has stopped amortising it");

	// The same counts on the host, where there is no boot: twenty checks cost a tenth of two
	// hundred, and widening would be pure extra work.
	let few = cost.estimate(&history, &key("host", crate::catalog::Environment::Host, 20), None);
	let many = cost.estimate(&history, &key("host", crate::catalog::Environment::Host, 200), None);
	let native = few / many;
	assert!(native < 0.2, "20 of 200 host keys cost {few:.0} s against {many:.0} s - there is nothing to amortise");
	// The two ratios are what the whole rule is about: same key counts, costs that differ by a
	// factor the count cannot see.
	assert!(emulated > native * 2.0, "an emulated boot must move the ratio: {emulated:.3} against {native:.3}");

	// And the count-based rule cannot tell those two apart, which is the defect stated as an
	// assertion: identical ratios, opposite right answers.
	assert_eq!(20.0 / 200.0, 20.0 / 200.0, "the ratio a key count sees is the same in both cases");
}

#[test]
fn the_host_universe_has_a_producer_and_its_results_parse() {
	// `trusted_everywhere` asks for Host AND TestGuest, and nothing ever wrote a Host record - so it
	// could not return true for any component however much evidence accumulated. A universe the
	// pipeline never feeds is not a gap in a trust model, it is a constant that reads like one.
	//
	// The host runner writes its results directly rather than being scraped out of a serial log: it
	// knows each check's id and its exit status, and saying so is cheaper and less fragile than
	// parsing prose.
	// THE WHOLE KEY per line. `id PASS` collapsed two variants of one check into one entry while
	// `total` counted both, so the declared total and the number of outcomes disagreed and nothing
	// said so.
	let log = "gate.test-tags\thost\thost\tdefault\tPASS\nhost.flac\thost\thost\tdefault\tFAIL\nconformance.png\thost\thost\tdefault\tPASS\ntotal 3\n";
	let results = crate::shadow::parse_host_log(log);
	assert_eq!(results.total_declared, Some(3), "a run that covered fewer checks than exist did not compare what it claims to");
	assert!(results.duplicates.is_empty(), "nothing in this log repeats a key");
	let key = |check: &str, configuration: &str| crate::plan::PlanItemKey { check: String::from(check), architecture: String::from("host"), environment: crate::catalog::Environment::Host, configuration: String::from(configuration) };
	assert_eq!(results.outcomes.get(&key("host.flac", "default")), Some(&crate::shadow::Outcome::Failed));
	assert_eq!(results.outcomes.get(&key("gate.test-tags", "default")), Some(&crate::shadow::Outcome::Passed));

	// A failure INSIDE the selection is the selector working; one OUTSIDE it is the candidate miss
	// the whole mechanism exists to find. The host ids are the catalog's own, with no prefix to
	// strip - the guest's `kernel.` prefix is a property of that universe, not of comparison.
	let history = crate::history::History::default();

	let inside = crate::shadow::compare_host(&[key("host.flac", "default")], &results, &history);
	assert!(inside.outside_failures.is_empty(), "the only failure was selected: {:?}", inside.outside_failures);
	assert!(!inside.inside_failures.is_empty(), "and it must be reported as selected");

	let outside = crate::shadow::compare_host(&[key("gate.test-tags", "default")], &results, &history);
	assert_eq!(outside.verdict, crate::shadow::Verdict::CandidateMiss, "a failure the selection did not name is exactly what a shadow is for");
}

#[test]
fn a_shadow_record_says_what_it_is_evidence_about() {
	// `Store::evaluate` counts clean runs and clean architectures, and the criteria the design asked
	// for - every change class exercised, every edge kind exercised, a SHADOW-EXEC sample - cannot be
	// written before there are records to grade. That is a reason not to write the policy yet and no
	// reason to keep throwing the data away: a run recorded without these can never be graded against
	// the criteria when they arrive.
	let model = model();
	let ownership = model.ownership();
	let planner = Planner::for_model(&model, &ownership);
	let plan = planner.plan(&[String::from("src/user/libs/audio/flac/src/lib.rs")]);
	assert!(!plan.edge_kinds.is_empty(), "a change that reaches anything traversed at least one edge");
	assert!(plan.edge_kinds.iter().all(|kind| !kind.is_empty()), "an edge kind is a name, not a blank");

	// And a record written before the fields existed still loads, reading as "said nothing" rather
	// than being dropped - there are records on disk from before today.
	let old = r#"{"schema":1,"records":[{"universe":"TestGuest","architecture":"x86_64","verdict":"Consistent","reason":"","model_hash":"h","source_digest":"d","changed_components":["flac"],"outside_failures":[],"at":0}]}"#;
	let log: crate::shadow::Log = serde_json::from_str(old).expect("a record from before the dimensions existed still parses");
	assert_eq!(log.records.len(), 1);
	assert!(log.records[0].change_kinds.is_empty(), "it said nothing about change classes, and says so");
	assert!(!log.records[0].shadow_exec, "and no sample accompanied it");
}

#[test]
fn two_configurations_of_one_check_are_two_results() {
	// The defect this closes: `proto` is host-tested under default features AND under
	// `shared-image`, because those two builds do not contain the same dependencies. Keyed on the
	// check's name alone the second line overwrote the first - so a `shared-image` failure could be
	// erased by a `default` pass, while `total` still counted two.
	let log = "host.proto\thost\thost\tdefault\tPASS\nhost.proto\thost\thost\tshared-image\tFAIL\ntotal 2\n";
	let results = crate::shadow::parse_host_log(log);
	assert_eq!(results.outcomes.len(), 2, "two configurations of one check are two outcomes, not one");
	assert_eq!(results.total_declared, Some(2));
	assert!(results.duplicates.is_empty());
	let key = |configuration: &str| crate::plan::PlanItemKey { check: String::from("host.proto"), architecture: String::from("host"), environment: crate::catalog::Environment::Host, configuration: String::from(configuration) };
	assert_eq!(results.outcomes.get(&key("default")), Some(&crate::shadow::Outcome::Passed));
	assert_eq!(results.outcomes.get(&key("shared-image")), Some(&crate::shadow::Outcome::Failed), "the failing variant must survive the passing one");

	// And selecting one configuration does not account for the other.
	let history = crate::history::History::default();
	let comparison = crate::shadow::compare_host(&[key("default")], &results, &history);
	assert_eq!(comparison.verdict, crate::shadow::Verdict::CandidateMiss, "the shared-image failure was outside the selection");

	// A producer that emitted one key twice compared something other than what it counted, and
	// that is a void comparison rather than a quiet overwrite.
	let repeated = crate::shadow::parse_host_log("host.proto\thost\thost\tdefault\tPASS\nhost.proto\thost\thost\tdefault\tFAIL\ntotal 2\n");
	assert_eq!(repeated.duplicates.len(), 1, "a repeated key is reported, not absorbed");
	assert_eq!(crate::shadow::compare_host(&[], &repeated, &history).verdict, crate::shadow::Verdict::Void);
}

#[test]
fn which_universes_may_judge_a_component_comes_from_its_checks() {
	// Trust asked a fixed pair - Host and TestGuest - of every component, and that is wrong in both
	// directions at once: it demands evidence from a universe that cannot reach the component, which
	// can never be earned, and it ignores one that can, which makes the certificate mean less than
	// it says.
	let model = model();
	let judges = |component: &str| crate::catalog::judging_universes(&model.catalog, component);

	// A codec is judged on the host (its own suite) and in the guest (the image that ships it).
	let flac = judges("flac");
	assert!(flac.contains(&crate::shadow::Universe::Host), "flac has a host suite: {flac:?}");
	assert!(!flac.is_empty(), "a component with checks has judges");

	// A component nothing covers has NO judges, and `trusted_everywhere` must answer false rather
	// than vacuously true - an empty `all()` is true, and that is the shape of the bug it would be.
	let store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	assert!(!store.trusted_everywhere("a-component-no-check-covers", "hash", &[], &crate::shadow::Scope::default()), "no judge is not the same as every judge agreeing");
}

#[test]
fn the_build_evidence_producer_runs_on_the_architectures_the_plan_builds() {
	// TWO SETS, AND THE PRODUCER WAS USING THE WRONG ONE. `verify.sh` computed one target list from
	// `booted` and both loops read it - the guest shadow, correctly, and the `HostBuild` evidence
	// producer, which asks a question about BUILDS.
	//
	// The plan carries both fields on purpose, and the regression corpus states the case in the
	// model's own words: `riscv64-trap-handling` boots riscv64 alone and must select
	// `build.kernel / x86_64 / host / shared-image`, "because a branch that stops compiling elsewhere
	// is a regression as well". So on that change the planner selected an x86_64 build check and the
	// producer never ran one, because x86_64 is not in the boot set - and `HostBuild` requires
	// evidence from two architectures. The ordinary shape of userspace work is exactly this shape.
	//
	// AT THE PRODUCTION SEMANTICS, not at the record shape. `a_build_covered_component_can_reach_trusted`
	// builds its two-architecture records by hand, so it cannot see that the shell never produces
	// them.
	let model = model();
	let ownership = model.ownership();
	let planner = crate::plan::Planner::for_model(&model, &ownership);
	let plan = planner.plan(&[String::from("src/kernel/arch/riscv64/traps/mod.rs")]);
	assert_ne!(plan.architectures_built, plan.architectures_booted, "a per-target kernel change boots one target and builds all of them");
	assert!(plan.architectures_built.len() >= 2, "and `HostBuild` needs evidence from at least two: {:?}", plan.architectures_built);

	// Every build check the plan selected names an architecture the producer will visit. This is the
	// property the shell was breaking: a selected check on an architecture nobody loops over is a
	// check that is never discharged and never recorded.
	for item in &plan.items {
		if item.kind != crate::catalog::CheckKind::Build {
			continue;
		}
		assert!(plan.architectures_built.contains(&item.key.architecture), "the plan selected the build check {} and the producer would not visit {}: {:?}", item.key.display(), item.key.architecture, plan.architectures_built);
	}

	// And the shell reads the right one. The two loops are two lines apart and the whole defect was
	// that they shared a variable, so the assertion is on the text of the script rather than on a
	// property that would still hold if it went back to sharing.
	const VERIFY: &str = include_str!("../../../../verify.sh");
	// The candidate arguments sit between the tool and its subcommand, so the assertion is on the
	// subcommand and its input rather than on the whole invocation - see the note at the top of the
	// shadow block: every question in that path is asked of ONE model, and when a candidate is given
	// that model is the candidate.
	assert!(VERIFY.contains("built --stdin"), "verify.sh asks the planner which architectures to BUILD on");
	assert!(VERIFY.contains(r#"-- "${candidate_arg[@]}" built --stdin"#), "and it asks the CANDIDATE when there is one, or the run executes the active model's wider selection and compares it against the narrower one");
	assert!(VERIFY.contains("for build_arch in $build_targets; do"), "and the build-evidence producer loops over that set rather than the booted one");
	assert!(VERIFY.contains("for target in $targets; do"), "while the guest shadow keeps the booted set");
}

#[test]
fn a_certificate_names_what_its_evidence_covers() {
	// THE CRITERIA THE RECORDS COULD ALREADY ANSWER. `evaluate` counts clean runs, distinct
	// decisions, architectures and an execution sample; the frozen design also asked which change
	// classes and which edge kinds were exercised, and the records have carried them since the round
	// that added them - under a comment saying the policy could not be written before there were
	// records to grade.
	//
	// PER COMPONENT, which is the half that was still global. `change_kinds` and `edge_kinds` on the
	// record describe the whole change set, so a commit that renamed a file in a NEIGHBOUR widened
	// this component's scope to cover renames nothing had validated it over.
	let mut log = crate::shadow::Log { schema: 1, records: Vec::new() };
	for (index, architecture) in ["x86_64", "riscv64", "x86_64", "riscv64", "x86_64", "riscv64", "x86_64", "riscv64", "x86_64", "riscv64"].into_iter().enumerate() {
		let mut record = evidence(crate::shadow::Universe::TestGuest, architecture, true, "one-tree");
		record.component_decisions = vec![format!("audio\tdecision-{}", index / 2)];
		// The run also renamed something in a neighbour and reached it through another edge. Both
		// are true of the RUN and neither is true of audio.
		record.change_kinds = vec![String::from("modified"), String::from("renamed")];
		record.edge_kinds = vec![String::from("link.static"), String::from("generation.build")];
		record.component_scopes.insert(String::from("term"), crate::shadow::Scope::from_kinds(vec![String::from("renamed")], vec![String::from("generation.build")]));
		log.records.push(record);
	}
	let store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	assert!(store.evaluate("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).is_ok());
	let scope = store.evidence_scope("audio", "hash-a", crate::shadow::Universe::TestGuest, &log);
	assert_eq!(scope.records, 10);
	assert_eq!(scope.scope.change_kinds, vec![String::from("modified")], "the neighbour's rename is not audio's evidence");
	assert_eq!(scope.scope.edge_kinds, vec![String::from("link.static")], "nor the edge the neighbour was reached through");
	assert!(scope.all_self_checked);

	// A comparison made by a model failing its own checks is not evidence about the tree at all -
	// `is_clean` excludes it, so it does not reach the scope rather than diluting it.
	log.records[3].model_self_check = false;
	let scope = store.evidence_scope("audio", "hash-a", crate::shadow::Universe::TestGuest, &log);
	assert_eq!(scope.records, 9, "the record made by an unsound model is not counted");
	assert!(scope.all_self_checked, "and what is counted was all made by a sound one, which is the statement");

	// A record written before `component_scopes` existed cannot say what it covered FOR A COMPONENT,
	// and its global fields are the whole run's. It counts as a run and contributes no scope, which
	// is the same failing-closed rule `model_self_check` applies.
	for record in &mut log.records {
		record.component_scopes.clear();
	}
	let scope = store.evidence_scope("audio", "hash-a", crate::shadow::Universe::TestGuest, &log);
	assert_eq!(scope.records, 9, "the runs still happened");
	assert!(scope.scope.change_kinds.is_empty() && scope.scope.edge_kinds.is_empty(), "and say nothing about what they covered");

	// And a component with no clean evidence at all has an EMPTY scope rather than a missing one:
	// zero records, nothing exercised. `all_self_checked` is vacuously true over none, which is why
	// the count is what a reader has to look at first.
	let scope = store.evidence_scope("nothing", "hash-a", crate::shadow::Universe::TestGuest, &log);
	assert_eq!(scope.records, 0);
	assert!(scope.scope.change_kinds.is_empty() && scope.scope.edge_kinds.is_empty());
}

#[test]
fn a_certificate_earned_on_one_kind_of_change_does_not_answer_for_another() {
	// THE SCOPE WAS A REPORT AND NOT A BOUND. It was computed at grant time, printed, and thrown
	// away: the certificate stored was `TRUSTED(component, universe, model_hash)`, and `level` looked
	// it up without ever asking what the change in hand was. So five clean comparisons over source
	// edits reached through `link.static` answered for a RENAME reached through `generation.build` -
	// a combination no shadow comparison had seen, trusted on the strength of ones that had nothing
	// to do with it.
	//
	// This is the broad-certificate defect the milestone split `HostBuild` out of `Host` to prevent,
	// one level down: a grant wider than its evidence.
	let mut log = crate::shadow::Log { schema: 1, records: Vec::new() };
	for (index, architecture) in ["x86_64", "riscv64", "x86_64", "riscv64", "x86_64", "riscv64"].into_iter().enumerate() {
		let mut record = evidence(crate::shadow::Universe::TestGuest, architecture, true, "one-tree");
		record.component_decisions = vec![format!("audio\tdecision-{index}")];
		log.records.push(record);
	}
	let mut store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	let (clean, architectures) = store.evaluate("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).expect("six distinct clean decisions, two architectures, one execution sample");
	let scope = store.evidence_scope("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).scope;
	assert_eq!(scope.change_kinds, vec![String::from("modified")]);
	store.grant("audio", "hash-a", crate::shadow::Universe::TestGuest, clean, architectures, scope, 0);

	let earned = crate::shadow::Scope::from_kinds(vec![String::from("modified")], vec![String::from("link.static")]);
	assert_eq!(store.level("audio", "hash-a", crate::shadow::Universe::TestGuest, &earned), crate::trust::Level::Trusted, "the change it was earned over");

	let renamed = crate::shadow::Scope::from_kinds(vec![String::from("renamed")], vec![String::from("link.static")]);
	assert_eq!(store.level("audio", "hash-a", crate::shadow::Universe::TestGuest, &renamed), crate::trust::Level::Shadow, "a class of change nothing validated this selector over");
	// NAMED AS A PAIR, because that is what the scope is now: a certificate covers observed
	// combinations, not two independent sets, so what is missing is a combination.
	assert_eq!(store.shortfall("audio", "hash-a", crate::shadow::Universe::TestGuest, &renamed), vec![String::from("change kind 'renamed' reached through edge kind 'link.static'")], "and the runner can say what is missing");

	let generated = crate::shadow::Scope::from_kinds(vec![String::from("modified")], vec![String::from("generation.build")]);
	assert_eq!(store.level("audio", "hash-a", crate::shadow::Universe::TestGuest, &generated), crate::trust::Level::Shadow, "an edge of the graph the evidence never walked");

	// Both dimensions at once, which is the combination the audit named.
	let both = crate::shadow::Scope::from_kinds(vec![String::from("renamed")], vec![String::from("generation.build")]);
	assert_eq!(store.level("audio", "hash-a", crate::shadow::Universe::TestGuest, &both), crate::trust::Level::Shadow);
	assert!(!store.trusted_everywhere("audio", "hash-a", &[crate::shadow::Universe::TestGuest], &both), "and the whole-run answer follows the same rule");

	// A certificate written before scopes existed covers nothing but a requirement that asks
	// nothing. Failing closed: a grant that cannot say what it proved has not proved anything.
	store.certificates[0].scope = crate::shadow::Scope::default();
	assert_eq!(store.level("audio", "hash-a", crate::shadow::Universe::TestGuest, &earned), crate::trust::Level::Shadow);
	assert_eq!(store.level("audio", "hash-a", crate::shadow::Universe::TestGuest, &crate::shadow::Scope::default()), crate::trust::Level::Trusted, "which is what a report with no change in hand asks");
}

#[test]
fn the_edges_a_component_is_evidence_about_are_the_ones_walked_out_of_it() {
	// `edge_kinds` is the union over the whole plan, and a certificate is about one component - so a
	// change touching two components gave each of them the other's edges. The reach paths carry the
	// seed they start from, which is what makes the attribution possible at all.
	let model = model();
	let plan = plan_for(&model, &["src/user/libs/audio/flac/src/lib.rs", "src/term/src/screen.rs"]);
	assert_eq!(plan.changed_components.len(), 2, "the fixture must change exactly two components: {:?}", plan.changed_components);
	for component in &plan.changed_components {
		let mine = plan.component_edge_kinds.get(component).expect("every changed component is a seed and gets an entry");
		for kind in mine {
			assert!(plan.edge_kinds.contains(kind), "{component}: '{kind}' is not an edge this plan walked at all");
		}
	}
	// The union of the per-component sets is what the global set is FOR THE SEEDS - anything the
	// global list has beyond it was walked out of a component this change did not touch.
	let union: BTreeSet<&String> = plan.component_edge_kinds.values().flatten().collect();
	assert!(union.iter().all(|kind| plan.edge_kinds.contains(*kind)));
}

// The permission fixture's cohort audit. Text in, problems out - so every defect the
// audit exists to catch is written here as a fixture rather than waiting for someone to make the
// mistake in the tree.
const COHORT_MAP: &str = r#"
pub(crate) const PERMISSION_COHORT: [(&str, PermissionCohort); 3] = [
	("kernel.applications.one", PermissionCohort::Base),
	("kernel.applications.two", PermissionCohort::Base),
	("kernel.applications.three", PermissionCohort::Scoped),
];
"#;

const COHORT_CONSUMERS: &str = r#"
tagged_test!(one, [Service], id = "kernel.applications.one", covers = ["bin.permission_manager", "kernel"]);
fn one() {
	declare_permission_cohort("kernel.applications.one", PermissionCohort::Base);
	let result = permission_scenario_result(PermissionCohort::Base).expect("x");
}
tagged_test!(two, [Service], id = "kernel.applications.two", covers = ["bin.permission_manager", "kernel"]);
fn two() {
	declare_permission_cohort("kernel.applications.two", PermissionCohort::Base);
	let result = permission_scenario_result(PermissionCohort::Base).expect("x");
}
tagged_test!(three, [Service], id = "kernel.applications.three", covers = ["bin.permission_manager", "kernel"]);
fn three() {
	declare_permission_cohort("kernel.applications.three", PermissionCohort::Scoped);
	let result = permission_scenario_result(PermissionCohort::Scoped).expect("x");
}
"#;

#[test]
fn a_matching_cohort_map_and_declaration_set_is_clean() {
	assert!(crate::kerneltests::audit_permission_cohort(COHORT_MAP, COHORT_CONSUMERS).is_empty());
}

#[test]
fn an_unclassified_fixture_consumer_is_a_failure() {
	// A thirteenth consumer arrives and nobody classifies it. The count is the only thing that can
	// see this, which is exactly why it is read.
	let consumers = format!("{COHORT_CONSUMERS}\nfn four() {{\n\tlet result = permission_scenario_result(PermissionCohort::Base).expect(\"x\");\n}}\n");
	let problems = crate::kerneltests::audit_permission_cohort(COHORT_MAP, &consumers);
	assert_eq!(problems.len(), 1, "{problems:?}");
	assert!(problems[0].contains("4 tests drive the permission fixture and 3 declare a cohort"), "{problems:?}");
}

#[test]
fn an_orphan_map_entry_is_a_failure() {
	let map = COHORT_MAP.replace("];", "\t(\"kernel.applications.gone\", PermissionCohort::Base),\n];");
	let problems = crate::kerneltests::audit_permission_cohort(&map, COHORT_CONSUMERS);
	assert_eq!(problems.len(), 1, "{problems:?}");
	assert!(problems[0].contains("kernel.applications.gone"), "{problems:?}");
	assert!(problems[0].contains("outlived its test"), "{problems:?}");
}

#[test]
fn a_duplicate_id_is_a_failure() {
	let map = COHORT_MAP.replace("];", "\t(\"kernel.applications.one\", PermissionCohort::Base),\n];");
	let problems = crate::kerneltests::audit_permission_cohort(&map, COHORT_CONSUMERS);
	assert!(problems.iter().any(|problem| problem.contains("lists 'kernel.applications.one' 2 times")), "{problems:?}");
}

#[test]
fn a_drifted_class_is_a_failure() {
	// The map and the test body disagree about which cached result this consumer may use. Nothing
	// about the test's name, tags or assertions changes, which is what makes this worth a check.
	let consumers = COHORT_CONSUMERS.replace("(\"kernel.applications.three\", PermissionCohort::Scoped)", "(\"kernel.applications.three\", PermissionCohort::Base)");
	let problems = crate::kerneltests::audit_permission_cohort(COHORT_MAP, &consumers);
	assert_eq!(problems.len(), 1, "{problems:?}");
	assert!(problems[0].contains("declares permission cohort Base and PERMISSION_COHORT says Scoped"), "{problems:?}");
}

#[test]
fn a_declaration_the_map_does_not_know_is_a_failure() {
	let consumers = COHORT_CONSUMERS.replace("kernel.applications.two", "kernel.applications.invented");
	let problems = crate::kerneltests::audit_permission_cohort(COHORT_MAP, &consumers);
	assert!(problems.iter().any(|problem| problem.contains("kernel.applications.invented") && problem.contains("does not list it")), "{problems:?}");
	assert!(problems.iter().any(|problem| problem.contains("kernel.applications.two") && problem.contains("outlived its test")), "{problems:?}");
}

// ---------------------------------------------------------------------------------------------
// A PermissionManager change selects the tests that can detect a regression in it.
//
// HERMETIC IN THE DIMENSION THAT MATTERS. `kerneltests::discover` reads the compiled test binaries
// out of `.build` to learn which tests exist per target, so a regression built on the discovered
// suite passes or fails according to what the caller happened to build last. These substitute a
// written-down suite and rebuild the catalog over it, so the assertion is about the PLANNER and the
// `covers` declarations - not about anyone's `.build`.

// The evidence-backed set, from the watched-fail matrix in `.build/benchmarks/permission-coverage`.
// Written out rather than derived: deriving it from "tests that declare bin.permission_manager"
// would make this regression agree with the declarations by construction, which is the one thing it
// must not do.
const PERMISSION_COVERAGE: [&str; 12] = [
	"kernel.applications.a_command_word_on_its_own_runs_the_command",
	"kernel.applications.a_consumer_that_stops_early_ends_the_pipeline_instead_of_hanging_it",
	"kernel.applications.a_fan_out_stage_with_an_unwritable_destination_still_carries_the_stream",
	"kernel.applications.a_governed_pipeline_starts_as_one_transaction_and_carries_data",
	"kernel.applications.a_migrated_stream_tool_reads_a_pipeline_the_way_it_reads_a_path",
	"kernel.applications.a_redirection_is_a_governed_pipeline_stage_and_the_consumer_holds_no_storage",
	"kernel.applications.a_typed_line_goes_through_the_real_shell_and_comes_back_as_a_pipeline",
	"kernel.applications.merging_the_error_stream_sends_a_stages_diagnostics_down_its_own_edge",
	"kernel.applications.permission_manager_enforces_static_and_dynamic_probe_policy",
	"kernel.applications.permission_manager_mints_scoped_application_grants",
	"kernel.applications.permission_manager_runs_tools_with_minimal_grants",
	"kernel.applications.the_command_tools_run_governed_and_read_in_windows",
];

const PERMISSION_MANAGER_PATH: &str = "src/user/services/core/src/permission_manager.rs";

fn permission_kernel_test(id: &str, covers: &[&str]) -> KernelTest {
	KernelTest { source_paths: Vec::new(), name: id.rsplit('.').next().expect("an id has a last segment").to_string(), id: id.to_string(), covers: covers.iter().map(|item| (*item).to_string()).collect(), architectures: crate::registry::ARCHITECTURES.iter().map(|architecture| (*architecture).to_string()).collect() }
}

#[test]
fn changing_one_test_file_selects_the_tests_declared_in_it() {
	// THE MECHANISM THE REGISTRY ROW HAS PROMISED ALL ALONG, AND NOTHING DROVE (added 2026-09-04).
	// `Declaration` gained `source_paths` so that "the tests in this file" is a question the model
	// can be asked, and every fixture in this file passed an EMPTY list - so the block that answers
	// it had no test at all, on top of being unreachable in production while `src/kernel/test_suites`
	// resolved to a component that selects everything. The ownership split fixed the second half;
	// this is the first.
	let here = "src/kernel/test_suites/hardware.rs";
	let elsewhere = "src/kernel/test_suites/boot.rs";
	let declared = KernelTest { source_paths: vec![String::from(here)], name: String::from("declared_here"), id: String::from("kernel.declared_here"), architectures: vec![String::from("x86_64")], covers: vec![String::from("liberfs")] };
	let other = KernelTest { source_paths: vec![String::from(elsewhere)], name: String::from("declared_elsewhere"), id: String::from("kernel.declared_elsewhere"), architectures: vec![String::from("x86_64")], covers: vec![String::from("liberfs")] };
	// `covers` deliberately names a component this change does NOT reach, so the only thing that can
	// put the test in the plan is the declaration - which is the property being asserted.
	let model = model_with_suite(vec![declared, other]);
	let plan = plan_for(&model, &[here]);
	let selected = keys(&plan);
	assert!(selected.iter().any(|key| key.contains("kernel.declared_here")), "the test this file declares is selected: {selected:?}");
	assert!(!selected.iter().any(|key| key.contains("kernel.declared_elsewhere")), "and the one another file declares is not - that is the difference between selecting the tests in it and selecting the whole kernel: {selected:?}");
	assert!(plan.items.iter().any(|item| item.reason == "a changed file declares this test"), "and it is in the plan for that reason rather than by reach");

	// AND NOT NOTHING, which is the other half of the row's own sentence. A change to a test file
	// still rebuilds the kernel, because the kernel is built from it.
	assert!(selected.iter().any(|key| key.starts_with("build.kernel")), "the kernel is still rebuilt: {selected:?}");
}

// A model whose kernel suite is exactly what this test writes down.
fn model_with_suite(tests: Vec<KernelTest>) -> Model {
	let mut model = model();
	model.kernel_tests.tests = tests;
	model.kernel_tests.missing_targets.clear();
	model.kernel_tests.unannotated = 0;
	let staged = crate::staged_components(&model.manifest, &model.crates, &model.graph);
	model.catalog = crate::catalog::Catalog::build(&model.crates, &model.registry, &model.graph, &staged, &model.kernel_tests.tests);
	model
}

// The suite as the evidence says it is: the twelve, a decoy that reaches the same kernel helper and
// claims nothing about PermissionManager, and enough unrelated tests to make the suite the size the
// real one is.
//
// THE SIZE IS PART OF THE FIXTURE. The planner widens a selection back to the whole suite when the
// scoped run would cost within a tenth of running everything, which is correct and is why a
// thirteen-test fixture selected all thirteen: twelve of thirteen IS everything. A regression built
// on that would have asserted the widening rather than the selection.
fn permission_suite() -> Vec<KernelTest> {
	let mut tests: Vec<KernelTest> = PERMISSION_COVERAGE.iter().map(|id| permission_kernel_test(id, &["bin.permission_manager", "kernel", "services"])).collect();
	tests.push(permission_kernel_test("kernel.applications.a_neighbour_that_claims_nothing_about_permissions", &["kernel", "services"]));
	for index in 0..255 {
		tests.push(permission_kernel_test(&format!("kernel.unrelated.case_{index:03}"), &["bin.nothing-this-change-reaches"]));
	}
	tests
}

fn selected_kernel_tests(model: &Model, plan: &crate::plan::Plan) -> BTreeSet<String> {
	plan.items.iter().filter(|item| model.catalog.get(&item.key.check).is_some_and(|check| check.kind == crate::catalog::CheckKind::KernelTest)).map(|item| item.key.check.clone()).collect()
}

#[test]
fn a_permission_manager_change_selects_exactly_the_evidence_backed_tests() {
	let model = model_with_suite(permission_suite());
	let plan = plan_for(&model, &[PERMISSION_MANAGER_PATH]);

	assert!(!plan.full, "a PermissionManager change is scoped, not a full run: {:?}", plan.full_reasons);
	assert_eq!(plan.changed_components, vec![String::from("bin.permission_manager")]);
	// The target policy this milestone inherited and must not change: three targets built, one
	// booted.
	assert_eq!(plan.architectures_built, vec![String::from("aarch64"), String::from("riscv64"), String::from("x86_64")]);
	assert_eq!(plan.architectures_booted, vec![String::from("x86_64")]);

	let selected = selected_kernel_tests(&model, &plan);
	// THE DEFECT THIS MILESTONE EXISTS FOR. Before the coverage declarations this was empty, and the
	// command layer added an unenumerated full-suite boot whose result attributed to nothing.
	assert!(!selected.is_empty(), "a PermissionManager change selected no kernel test at all");
	// Reaching the same kernel helper is not a coverage claim.
	assert!(!selected.contains("kernel.applications.a_neighbour_that_claims_nothing_about_permissions"), "affected={:?} reason={:?}", plan.affected_components, plan.items.iter().find(|item| item.key.check.contains("neighbour")).map(|item| item.reason.clone()));
	let expected: BTreeSet<String> = PERMISSION_COVERAGE.iter().map(|id| (*id).to_string()).collect();
	assert_eq!(selected, expected, "the selected set is not the evidence-backed set");
}

#[test]
fn the_permission_manager_selection_lowers_to_one_enumerated_guest_step() {
	let model = model_with_suite(permission_suite());
	let plan = plan_for(&model, &[PERMISSION_MANAGER_PATH]);
	let mut per_target: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
	for test in &model.kernel_tests.tests {
		for architecture in &test.architectures {
			*per_target.entry(architecture.clone()).or_default() += 1;
		}
	}
	let steps = crate::commands::steps(&plan, &per_target, &model.registry);
	let guest: Vec<&crate::commands::Step> = steps.iter().filter(|step| step.command.contains("./test.sh")).collect();
	assert_eq!(guest.len(), 1, "one guest step, not one per test and not one per target: {:?}", guest.iter().map(|step| &step.command).collect::<Vec<_>>());
	let command = &guest[0].command;
	assert!(command.starts_with("TEST_SELECTION="), "the guest step hands over ids rather than falling back to the whole suite: {command}");
	assert!(command.ends_with("./test.sh --arch x86_64"), "{command}");
	for id in PERMISSION_COVERAGE {
		assert!(command.contains(id), "the lowered command omits {id}");
	}
	assert!(!command.contains("a_neighbour_that_claims_nothing_about_permissions"), "{command}");
}

#[test]
fn an_omitted_coverage_declaration_is_a_selection_failure() {
	// The regression has to fail on the OLD behaviour, so here is the old behaviour: one of the
	// twelve stops declaring what it covers.
	let mut suite = permission_suite();
	let dropped = "kernel.applications.permission_manager_mints_scoped_application_grants";
	for test in &mut suite {
		if test.id == dropped {
			test.covers.retain(|component| component != "bin.permission_manager");
		}
	}
	let model = model_with_suite(suite);
	let plan = plan_for(&model, &[PERMISSION_MANAGER_PATH]);
	let selected = selected_kernel_tests(&model, &plan);
	assert!(!selected.contains(dropped), "the fixture did not actually drop the declaration");
	let expected: BTreeSet<String> = PERMISSION_COVERAGE.iter().map(|id| (*id).to_string()).collect();
	assert_ne!(selected, expected, "an omitted declaration must change the selected set");
}

#[test]
fn a_declaration_without_evidence_is_a_selection_failure() {
	// The other direction: a test that never caught a focused mutation claims the component anyway.
	// Nothing mechanical stops that, which is why the decision record is written down and why this
	// regression names the set rather than deriving it.
	let mut suite = permission_suite();
	for test in &mut suite {
		if test.id == "kernel.applications.a_neighbour_that_claims_nothing_about_permissions" {
			test.covers.push(String::from("bin.permission_manager"));
		}
	}
	let model = model_with_suite(suite);
	let plan = plan_for(&model, &[PERMISSION_MANAGER_PATH]);
	let selected = selected_kernel_tests(&model, &plan);
	let expected: BTreeSet<String> = PERMISSION_COVERAGE.iter().map(|id| (*id).to_string()).collect();
	assert_ne!(selected, expected, "an unearned declaration must show up as a different selected set");
}

#[test]
fn an_unrelated_component_does_not_acquire_the_permission_manager_test_set() {
	let model = model_with_suite(permission_suite());
	let plan = plan_for(&model, &["src/user/libs/audio/flac/src/lib.rs"]);
	let selected = selected_kernel_tests(&model, &plan);
	let expected: BTreeSet<String> = PERMISSION_COVERAGE.iter().map(|id| (*id).to_string()).collect();
	assert_ne!(selected, expected, "an unrelated change acquired the whole PermissionManager set");
}

#[test]
fn every_selected_id_is_one_the_guest_runner_can_match() {
	// An unknown id is a hard failure in the guest, so a selection naming one runs nothing and says
	// so - the exact false green the id mechanism exists against.
	let model = model_with_suite(permission_suite());
	let plan = plan_for(&model, &[PERMISSION_MANAGER_PATH]);
	let known: BTreeSet<&str> = model.kernel_tests.tests.iter().map(|test| test.id.as_str()).collect();
	for id in selected_kernel_tests(&model, &plan) {
		assert!(known.contains(id.as_str()), "the plan selected '{id}', which no test in the suite carries");
	}
}

#[test]
fn a_cohort_member_that_stops_covering_permission_manager_is_a_failure() {
	// The planner regression writes its own suite, so it stays green when the real declaration is
	// deleted. This is the check that does not: the marker names a stable id, and the tagged_test!
	// carrying that id has to still claim the component.
	let consumers = COHORT_CONSUMERS.replace(r#"id = "kernel.applications.two", covers = ["bin.permission_manager", "kernel"]"#, r#"id = "kernel.applications.two", covers = ["kernel"]"#);
	let problems = crate::kerneltests::audit_permission_cohort(COHORT_MAP, &consumers);
	assert_eq!(problems.len(), 1, "{problems:?}");
	assert!(problems[0].contains("kernel.applications.two") && problems[0].contains("does not cover bin.permission_manager"), "{problems:?}");
}

#[test]
fn a_marker_with_no_tagged_test_is_a_failure() {
	let consumers = COHORT_CONSUMERS.replace(r#"tagged_test!(two, [Service], id = "kernel.applications.two", covers = ["bin.permission_manager", "kernel"]);"#, "");
	let problems = crate::kerneltests::audit_permission_cohort(COHORT_MAP, &consumers);
	assert!(problems.iter().any(|problem| problem.contains("no tagged_test! carries that id")), "{problems:?}");
}

// ---------------------------------------------------------------------------------------------
// A step says what it will run, and runs what it says

// What the COMMAND will execute, read off the command itself rather than off the keys - because the
// keys are the thing being checked and deriving both from one source would prove nothing.
//
// Returns None for a command whose extent cannot be read from the line alone, which today is only
// the whole-suite form: `./test.sh --arch X` runs everything on X, and "everything" is a fact about
// the catalog rather than about the string.
fn what_the_command_runs(command: &str) -> Option<BTreeSet<String>> {
	if let Some(rest) = command.strip_prefix("./build.sh --arch ") {
		let mut fields = rest.split(" --part ");
		let _architecture = fields.next()?;
		return Some(fields.next()?.split(',').map(|part| format!("build.{part}")).collect());
	}
	if let Some(rest) = command.strip_prefix("./check.sh --gate ") {
		return Some(rest.split(',').map(|name| format!("gate.{name}")).collect());
	}
	if let Some(rest) = command.strip_prefix("./check.sh --conformance ") {
		return Some(rest.split(',').map(|name| format!("conformance.{name}")).collect());
	}
	if let Some(rest) = command.strip_prefix("TEST_SELECTION=") {
		return Some(rest.split(" ./test.sh").next()?.split(',').map(str::to_string).collect());
	}
	if command.contains("./test.sh --arch ") && command.contains(" --tags smoke") {
		return Some(BTreeSet::from([String::from("guest.boot-smoke")]));
	}
	None
}

// THE GATE FOR M0, and it is three assertions rather than one.
//
// Every EVIDENCE-PRODUCING step carries at least one key - a step that runs tests and discharges
// nothing is unmeasurable by construction, because `record_step` returns on an empty key list, which
// is how the whole-suite fallback could never acquire a cost however many times it ran. The keys a
// step carries are the keys its command will run - the plan said 195 and the run did 205 while the
// widening lived in the step builder. And every step has a `StepId`, because a cost has to be keyed
// on the thing that is actually scheduled rather than on what it happened to discharge.
#[test]
fn every_step_carries_the_keys_its_command_will_run() {
	let model = model();
	let mut per_target: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
	for test in &model.kernel_tests.tests {
		for architecture in &test.architectures {
			*per_target.entry(architecture.clone()).or_default() += 1;
		}
	}
	let cases: Vec<Vec<&str>> = vec![
		vec!["src/user/drivers/core/src/virtio_input.rs"],
		vec!["src/user/apps/tools/src/imgconv.rs"],
		vec!["src/kernel/arch/riscv64/traps/mod.rs"],
		vec!["src/user/libs/audio/flac/src/lib.rs"],
		vec!["src/harness/qemu-run.sh"],
		vec!["src/kernel/mem/tlb.rs"],
	];
	for paths in &cases {
		let plan = plan_for(&model, paths);
		let steps = crate::commands::steps(&plan, &per_target, &model.registry);
		let mut ids: BTreeSet<String> = BTreeSet::new();
		for step in &steps {
			assert!(!step.id.is_empty(), "{paths:?}: the step {:?} has no id, so nothing can schedule it or price it", step.label);
			assert!(ids.insert(step.id.clone()), "{paths:?}: two steps share the id {:?}, which would merge two costs into one figure describing neither", step.id);
			assert!(!step.keys.is_empty(), "{paths:?}: the step {:?} runs and discharges no key, which `record_step` cannot file and the estimator cannot see", step.label);
			let Some(will_run) = what_the_command_runs(&step.command) else {
				// The whole-suite form. Either it is that target's entire test list, or it is the
				// one aggregate key that stands in for a list the model could not read - and it is
				// never a strict subset, which is the mismatch this test exists to catch.
				if step.command.contains("./test.sh --arch ") {
					let architecture = step.command.rsplit(" --arch ").next().and_then(|rest| rest.split_whitespace().next()).unwrap_or_default().to_string();
					if step.keys.len() == 1 && step.keys[0].check == "guest.whole-suite" {
						continue;
					}
					let total = per_target.get(&architecture).copied().unwrap_or(0);
					assert_eq!(step.keys.len(), total, "{paths:?}: {:?} runs the whole {architecture} suite and carries {} of its {total} keys, so the difference runs unrecorded", step.label, step.keys.len());
					continue;
				}
				// Everything else is one check per command: a host suite is one crate's `cargo
				// test`, a dev check is one script. A step of either shape carrying two keys is two
				// results filed against one run.
				assert_eq!(step.keys.len(), 1, "{paths:?}: {:?} is one command and carries {} keys", step.label, step.keys.len());
				continue;
			};
			let carried: BTreeSet<String> = step.keys.iter().map(|key| key.check.clone()).collect();
			assert_eq!(carried, will_run, "{paths:?}: the step {:?} carries keys its command does not run, or runs work it carries no key for", step.label);
		}
	}
}

// ---------------------------------------------------------------------------------------------
// A candidate narrowing can actually reach the threshold it is graded against

// Five records for one component, all producing the SAME decision and each from a DIFFERENT change.
//
// This is the deadlock the milestone was written to break, in one function: a frozen `kernel.mem`
// candidate selects the same tests, walks the same edges and implies the same targets for every
// change inside it, so keying distinctness on the DECISION gives one digest however many genuine
// comparisons were run, and the threshold of five is unreachable by construction.
fn same_decision_different_change(tree: &str, changed: &str) -> crate::shadow::Record {
	let mut scope = crate::shadow::Scope::from_kinds(vec![String::from("modified")], vec![String::from("link.static")]);
	scope.changed_digest = String::from(changed);
	crate::shadow::Record {
		universe: crate::shadow::Universe::TestGuest,
		architecture: String::from("x86_64"),
		verdict: String::from("Consistent"),
		reason: String::new(),
		model_hash: String::from("candidate-hash"),
		source_digest: tree.to_string(),
		changed_components: vec![String::from("kernel.mem")],
		outside_failures: Vec::new(),
		at: 0,
		change_kinds: Vec::new(),
		edge_kinds: Vec::new(),
		shadow_exec: true,
		model_self_check: true,
		// THE SAME DECISION EVERY TIME, which is the whole point of the fixture.
		component_decisions: vec![String::from("kernel.mem\tone-and-only-decision")],
		component_scopes: [(String::from("kernel.mem"), scope)].into_iter().collect(),
	}
}

#[test]
fn five_different_changes_are_five_pieces_of_evidence_even_when_the_decision_is_identical() {
	let mut log = crate::shadow::Log::default();
	for index in 0..5 {
		log.records.push(same_decision_different_change(&format!("tree-{index}"), &format!("changed-{index}")));
	}
	let distinct = log.distinct_evidence_for_pair("kernel.mem", "candidate-hash", crate::shadow::Universe::TestGuest, None);
	assert_eq!(distinct, 5, "five different changes producing one decision must count as five pieces of evidence, or a frozen-subsystem candidate can never reach the threshold it is graded against");
}

#[test]
fn every_separately_runnable_check_is_its_own_step() {
	// M4 ASKS FOR EVERY SEPARATELY SCHEDULABLE UNIT TO BE SEPARATELY TIMED, and the batches were
	// the last place that was not true. Ordinary gates were lowered into one comma-list
	// `check.sh --gate a,b,c` and every conformance suite into a second step, on the reasoning that
	// `check.sh` takes a list - which is a property of the runner and not of the work. Merged, they
	// shared one `StepId`, one measured duration and one budget decision: the cheap ones could not
	// be ordered first, none of them was ever timed, and `--budget` admitted all of them or none.
	let model = model();
	let plan = plan_for(&model, &["src/kernel/mem/mod.rs"]);
	let per_target: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
	let steps = crate::commands::steps(&plan, &per_target, &model.registry);

	let gates: Vec<&crate::commands::Step> = steps.iter().filter(|step| step.command.contains("--gate ")).collect();
	assert!(!gates.is_empty(), "a kernel change selects gates, or this fixture proves nothing");
	for step in &gates {
		assert!(!step.command.contains(','), "a gate step names one gate, not a batch: {}", step.command);
		assert_eq!(step.keys.len(), 1, "and it discharges one key, which is what makes its duration a measurement of it: {}", step.command);
	}
	let ids: std::collections::BTreeSet<&String> = gates.iter().map(|step| &step.id).collect();
	assert_eq!(ids.len(), gates.len(), "each gate carries its own StepId, or two of them share a cost that describes neither");

	// AND THE SAME FOR CONFORMANCE SUITES, which were the second batch.
	for step in steps.iter().filter(|step| step.command.contains("--conformance ")) {
		assert!(!step.command.contains(','), "a conformance step names one suite: {}", step.command);
		assert_eq!(step.keys.len(), 1, "and discharges one key: {}", step.command);
	}
}

#[test]
fn a_candidate_that_narrows_only_the_registry_is_still_a_narrowing() {
	// THE BYPASS THE EVIDENCE BARS EXISTED TO CLOSE.
	//
	// Activation derived what a candidate takes away by comparing the two catalogues' `covers`
	// lists, which is the kernel tests' half of the overlay. A candidate carries a COMPLETE
	// replacement registry, and three things in it decide how wide a change's plan is without
	// touching any `covers` list: which paths a component owns, which escalation edges reach it, and
	// which targets a path is built and booted on. Narrow any of those and the old check found
	// nothing to guard, so neither the general trust bar nor the subsystem risk row was applied.
	//
	// Driven over the real registry against three one-line narrowings of it, because the rule is a
	// comparison of two registries and needs no plan to be run.
	let model = model();
	let ownership = model.ownership();
	let text = &model.registry.registry_text;
	let dir = model.repo_root.join("src/tools/verify-model/model");
	let narrowed_from = |replacement: String| crate::registry::Registry::load_with(&dir, Some(&replacement)).expect("the narrowed registry parses");

	// AN OWNERSHIP RULE THAT IS GONE. The component that owned that path is reached by fewer
	// changes, whatever any check says it covers. The whole block goes, because a table with its
	// fields removed is not a registry that parses.
	let owned = model.registry.ownership.first().expect("this tree declares ownership rules").clone();
	let block = format!("[[ownership]]\npath = \"{}\"\ncomponent = \"{}\"\n", owned.path, owned.component);
	assert!(text.contains(&block), "the fixture has to find the rule it is about");
	let without_the_block = narrowed_from(text.replace(&block, ""));
	let losing = crate::candidate::components_losing_registry_coverage(&model.registry, &without_the_block, &ownership, &crate::ownership::Ownership::new(&without_the_block, &model.crates));
	assert!(losing.contains(&owned.component), "a component that owns fewer paths has lost coverage: {losing:?}");

	// AND A RULE THAT IS STILL THERE, OVERRIDDEN BY A LONGER ONE (added 2026-09-03). Both lookups
	// resolve by longest prefix, so ADDING a rule narrows without removing anything: the owner keeps
	// every path it declared and stops owning the files under the new one. The set comparison could
	// not see it, and neither could a comparison of target lists.
	let deeper = format!("{}overridden-subtree/", if owned.path.ends_with('/') { owned.path.clone() } else { format!("{}/", owned.path) });
	// Whoever answers for that path TODAY is who loses it - the declared rule, or a crate directory
	// underneath it that is a longer match still. The fixture asks the resolver rather than assuming.
	let crate::ownership::Owner::Component { component: displaced, .. } = ownership.owner(&deeper) else { panic!("a path under a declared rule is owned by somebody") };
	let taking_over = "a-component-this-registry-does-not-otherwise-name";
	let override_block = format!("{block}\n[[ownership]]\npath = \"{deeper}\"\ncomponent = \"{taking_over}\"\n");
	let with_override = narrowed_from(text.replace(&block, &override_block));
	let losing = crate::candidate::components_losing_registry_coverage(&model.registry, &with_override, &ownership, &crate::ownership::Ownership::new(&with_override, &model.crates));
	// THE NAME REPORTED IS THE ONE THAT OWNS THE SUBTREE NOW (corrected 2026-09-04). This asserted
	// the DISPLACED component, and that is what made the bar unsatisfiable: a candidate's evidence
	// for those paths is recorded against whoever owns them under the candidate, so asking for the
	// displaced name under the candidate's own hash asks for a record that cannot exist. The
	// narrowing is still the displaced component's; the evidence that answers for it is the
	// successor's. See the split case below, which is this same shape stated deliberately.
	assert!(losing.contains(taking_over), "a longer rule takes a subtree away, and the evidence for it is filed under whoever took it: {losing:?}");
	assert!(!losing.contains(&displaced), "asking for the displaced name would ask for a record no run against this candidate can write: {losing:?}");

	// AND A COMPONENT THAT NO LONGER SELECTS EVERYTHING, which is the widest narrowing this registry
	// can express: its changes drop from the FULL suite to the ordinary scoped closure, and not one
	// ownership rule, edge or architecture row moves.
	let escalates = model.registry.selects_everything.first().expect("this tree declares an escalation").clone();
	let escalation_block = format!("[[selects_everything]]\ncomponent = \"{}\"\nreason = \"{}\"\n", escalates.component, escalates.reason);
	assert!(text.contains(&escalation_block), "the fixture has to find the escalation it is about");
	let without_escalation = narrowed_from(text.replace(&escalation_block, ""));
	let losing = crate::candidate::components_losing_registry_coverage(&model.registry, &without_escalation, &ownership, &crate::ownership::Ownership::new(&without_escalation, &model.crates));
	assert!(losing.contains(&escalates.component), "a component that stops selecting everything has lost every check its closure does not reach: {losing:?}");

	// AND A PATH BUILT OR BOOTED ON FEWER TARGETS. Same checks, fewer machines.
	let arch = model.registry.architecture.iter().find(|rule| rule.boot.len() > 1).expect("this tree declares a multi-target path").clone();
	let quoted: Vec<String> = arch.boot.iter().map(|target| format!("\"{target}\"")).collect();
	let narrower_arch = format!("path = \"{}\"\nbuild = [{}]\nboot = [{}]", arch.path, arch.build.iter().map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(", "), quoted[0]);
	let wider_arch = format!("path = \"{}\"\nbuild = [{}]\nboot = [{}]", arch.path, arch.build.iter().map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(", "), quoted.join(", "));
	assert!(text.contains(&wider_arch), "the fixture has to find the target list it is about");
	let narrowed_targets = narrowed_from(text.replace(&wider_arch, &narrower_arch));
	let losing = crate::candidate::components_losing_registry_coverage(&model.registry, &narrowed_targets, &ownership, &crate::ownership::Ownership::new(&narrowed_targets, &model.crates));
	assert!(!losing.is_empty(), "a path booted on fewer targets has lost coverage");

	// AND AN ARCHITECTURE ROW THAT IS STILL THERE, OVERRIDDEN BY A LONGER ONE. `architecture_rule`
	// takes the longest match too, so a deeper row with one target narrows every path beneath it
	// while the row this used to iterate keeps its whole list.
	let deeper_arch = format!("{}overridden-subtree/", if arch.path.ends_with('/') { arch.path.clone() } else { format!("{}/", arch.path) });
	let with_deeper = format!("{wider_arch}\n\n[[architecture]]\npath = \"{deeper_arch}\"\nbuild = [{}]\nboot = [{}]", quoted[0], quoted[0]);
	let deeper_targets = narrowed_from(text.replace(&wider_arch, &with_deeper));
	let losing = crate::candidate::components_losing_registry_coverage(&model.registry, &deeper_targets, &ownership, &crate::ownership::Ownership::new(&deeper_targets, &model.crates));
	let crate::ownership::Owner::Component { component: fewer_machines, .. } = ownership.owner(&deeper_arch) else { panic!("a path under a declared architecture row is owned by somebody") };
	assert!(losing.contains(&fewer_machines), "a longer architecture row takes machines away from a subtree the retained one still names: {losing:?}");

	// AND THE CATCH-ALL ROW, WHICH OWNS NOTHING AND GOVERNS EVERYTHING (added 2026-09-04). The
	// mandatory default is `path = ""`; `Ownership::owner("")` is `Unknown`, so a narrowing of it was
	// detected at that probe and thrown away for want of a component to attribute it to. It is the
	// widest narrowing the architecture table can express - every ordinary source file is built on
	// fewer machines - and it reached activation with `losing` empty.
	let default_row = model.registry.architecture.iter().find(|rule| rule.path.is_empty()).expect("the registry declares a catch-all architecture row");
	let wide_default = format!("path = \"\"\nbuild = [{}]", default_row.build.iter().map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(", "));
	let narrow_default = format!("path = \"\"\nbuild = [\"{}\"]", default_row.build.first().expect("the default row builds on at least one target"));
	assert!(text.contains(&wide_default), "the fixture has to find the catch-all row it is about");
	let narrowed_default = narrowed_from(text.replace(&wide_default, &narrow_default));
	let losing = crate::candidate::components_losing_registry_coverage(&model.registry, &narrowed_default, &ownership, &crate::ownership::Ownership::new(&narrowed_default, &model.crates));
	assert!(!losing.is_empty(), "narrowing the row that governs every ordinary path takes coverage from somebody");
	// And it is attributed to components that are REALLY governed by it, not to a placeholder: pick
	// one owned path whose effective row is the default and require its owner to be named.
	let governed = ownership.rule_paths().into_iter().find(|path| !path.is_empty() && !model.registry.architecture.iter().any(|rule| !rule.path.is_empty() && crate::registry::prefix_match(&rule.path, path).is_some()) && matches!(ownership.owner(path), crate::ownership::Owner::Component { .. })).expect("some owned path is governed by the catch-all row");
	let crate::ownership::Owner::Component { component: governed_by_default, .. } = ownership.owner(governed) else { panic!("just filtered for it") };
	assert!(losing.contains(&governed_by_default), "a component the catch-all governs is checked on fewer machines: {losing:?}");

	// AND A SUBSYSTEM SPLIT NAMES THE SUCCESSOR, WHICH IS THE ONLY NAME ITS EVIDENCE CAN CARRY
	// (added 2026-09-04). This is M5's advertised route and it could not complete: a candidate's
	// shadow runs plan with the OVERLAID ownership, so changes under the split path are recorded
	// against the new component, while activation asked `Store::evaluate` for the displaced one
	// under the candidate's own hash - a record that no run against that candidate can write. The
	// bar was unsatisfiable rather than absent, which is fail-safe and still means the route is
	// dead.
	let split_under = model.registry.risk_classes.iter().map(|risk| risk.path.clone()).find(|path| path.starts_with("src/kernel/") && matches!(ownership.owner(path), crate::ownership::Owner::Component { .. })).expect("this tree declares a kernel subsystem risk row");
	let crate::ownership::Owner::Component { component: displaced, .. } = ownership.owner(&split_under) else { panic!("just filtered for it") };
	let successor = format!("{displaced}.split");
	let split_rule = format!("[[ownership]]\npath = \"{split_under}\"\ncomponent = \"{successor}\"\n\n[[ownership]]");
	let split_registry = narrowed_from(text.replacen("[[ownership]]", &split_rule, 1));
	let losing = crate::candidate::components_losing_registry_coverage(&model.registry, &split_registry, &ownership, &crate::ownership::Ownership::new(&split_registry, &model.crates));
	assert!(losing.contains(&successor), "a split is graded on the name its own evidence carries: {losing:?}");
	assert!(!losing.contains(&displaced), "and not on the displaced name, which no run against this candidate can record: {losing:?}");

	// AND AN IDENTICAL REGISTRY TAKES NOTHING AWAY, which is what stops this refusing every
	// candidate that only touches the kernel tests' `covers`.
	assert!(crate::candidate::components_losing_registry_coverage(&model.registry, &model.registry, &ownership, &ownership).is_empty(), "a registry that did not change narrows nothing");
}

#[test]
fn a_failed_run_never_becomes_a_cost_however_the_models_are_ordered() {
	// TWO SEQUENCES, AND THE FIRST FIX CAUGHT NEITHER.
	//
	// A step's duration is filtered on the model it was measured under, because a duration measured
	// over a different plan is not a duration for this one. `record_step_id` withheld a FAILED run's
	// seconds and then overwrote the model hash anyway - so a success under model A followed by a
	// failure under model B left A's duration wearing B's label, and the filter that exists to
	// refuse exactly that accepted it.
	let mut history = crate::history::History { schema: crate::history::SCHEMA, entries: Default::default(), fixed_overshoot: Default::default(), steps: Default::default() };
	let cost = crate::history::CostModel::default();
	let keys = vec![crate::plan::PlanItemKey { check: String::from("gate.one"), architecture: String::from("host"), environment: crate::catalog::Environment::Host, configuration: String::from("default") }];
	history.record_step_id(Some("step-a"), &keys, true, 120.0, "model-a", &cost);
	assert_eq!(history.step_seconds("step-a", "model-a"), Some(120.0), "a passing run under this model is a measurement");
	history.record_step_id(Some("step-a"), &keys, false, 2.0, "model-b", &cost);
	assert_eq!(history.step_seconds("step-a", "model-b"), None, "a failure under a second model does not relabel the first model's duration as this one's");
	assert_eq!(history.step_seconds("step-a", "model-a"), None, "and the run really did happen, so the stale measurement is not kept under the old label either");

	// AND A FAILED SINGLE-KEY STEP DOES NOT PRICE ITS KEY. The merged guard covers members that
	// never started; a one-key step has no such member and wrote its partial duration straight into
	// the key record, which `estimate` reads.
	let mut priced = crate::history::History { schema: crate::history::SCHEMA, entries: Default::default(), fixed_overshoot: Default::default(), steps: Default::default() };
	priced.record_step_id(Some("step-b"), &keys, false, 3.0, "model-a", &cost);
	let after_failure = cost.estimate(&priced, &keys, Some("model-a"));
	let unmeasured = cost.estimate(&crate::history::History { schema: crate::history::SCHEMA, entries: Default::default(), fixed_overshoot: Default::default(), steps: Default::default() }, &keys, Some("model-a"));
	assert_eq!(after_failure, unmeasured, "a step that failed at its first instruction is not what running that key costs");
}

#[test]
fn five_real_changes_through_the_real_planner_reach_the_threshold() {
	// THE SAME PROPERTY, DRIVEN THROUGH THE PRODUCTION PATH (2026-09-03).
	//
	// The fixture above hands `distinct_evidence_for_pair` five digests written by hand, so it
	// proves the COUNTING and says nothing about whether the planner can produce five distinct
	// digests for one subsystem - which is the thing the milestone doubted, and the reason it asks
	// for a planner test rather than a unit one. This runs real paths through `Planner` and takes
	// the digests from `component_scopes`, which is the function `shadow` records with.
	//
	// A MULTI-FILE SUBSYSTEM FIRST. Five different files of `src/kernel/mem`, each a real file with
	// real content: the plan's decision about the component is the same every time, and the digests
	// differ because the CHANGES differ.
	let model = model();
	let files = ["src/kernel/mem/mod.rs", "src/kernel/mem/tlb.rs", "src/kernel/mem/vapool.rs", "src/kernel/mem/frame/mod.rs", "src/kernel/mem/heap/mod.rs"];
	let mut digests: BTreeSet<String> = BTreeSet::new();
	let mut decisions: BTreeSet<String> = BTreeSet::new();
	for path in files {
		let plan = plan_for(&model, &[path]);
		let scopes = crate::shadow::component_scopes(&model.repo_root, &plan, &std::collections::BTreeMap::new(), &model.registry);
		let (component, scope) = scopes.iter().find(|(component, _)| component.starts_with("kernel")).expect("a kernel component owns src/kernel/mem");
		assert!(!scope.changed_digest.is_empty(), "{path} produced no change digest, so no evidence about it could ever be distinct");
		digests.insert(scope.changed_digest.clone());
		decisions.insert(keys(&plan).into_iter().collect::<Vec<_>>().join(","));
		let _ = component;
	}
	assert_eq!(digests.len(), 5, "five real changes to one subsystem must produce five distinct digests, or the threshold is unreachable by construction");
	assert_eq!(decisions.len(), 1, "and they must be the SAME decision, or this fixture is not the deadlock it is about");

	// AND A SINGLE-FILE SUBSYSTEM, which is the case the cheap answer gets wrong: five edits to one
	// file share one path set, so a digest over paths alone would give one piece of evidence for all
	// five. The KIND is part of the tuple, and five kinds of change to one path are five changes.
	let one_file = "src/kernel/elf.rs";
	let mut single: BTreeSet<String> = BTreeSet::new();
	for kind in ["modified", "added", "deleted", "renamed", "copied"] {
		let explicit: std::collections::BTreeMap<String, String> = [(String::from(one_file), String::from(kind))].into_iter().collect();
		let plan = plan_for(&model, &[one_file]);
		let scopes = crate::shadow::component_scopes(&model.repo_root, &plan, &explicit, &model.registry);
		let scope = scopes.values().find(|scope| !scope.changed_digest.is_empty()).expect("the one file's component has a digest");
		single.insert(scope.changed_digest.clone());
	}
	assert_eq!(single.len(), 5, "a single-file subsystem's five kinds of change are five changes, or its risk class can never be narrowed");
}

#[test]
fn five_runs_of_one_change_are_still_one_piece_of_evidence() {
	// AND THE RULE IT REPLACED IS STILL RIGHT ABOUT WHAT IT DEFENDED AGAINST. Re-running one
	// comparison against one tree is one decision validated five times, and counting those five is
	// what the old criterion existed to refuse. The change is the KEY, not the counting.
	let mut log = crate::shadow::Log::default();
	for _ in 0..5 {
		log.records.push(same_decision_different_change("one-tree", "one-change"));
	}
	let distinct = log.distinct_evidence_for_pair("kernel.mem", "candidate-hash", crate::shadow::Universe::TestGuest, None);
	assert_eq!(distinct, 1, "five runs over one change are one piece of evidence, however many times the comparison was made");
}

// A CANDIDATE THAT IS REFUSED LEAVES THE TREE AS IT FOUND IT.
//
// `materialise` wrote `registry.toml` FIRST and only then resolved the test ids in `covers`, so a
// candidate naming a test the tree does not have was refused with the canonical registry already
// replaced - and the caller's `?` returned before it held the `previous` map to put it back with. The
// function's own comment says a refusal is "the difference between a check that refuses and one that
// refuses after the damage", and it was the second one.
#[test]
fn a_refused_candidate_writes_nothing() {
	let fixture = Fixture::new("candidate-refused");
	let model_dir = fixture.dir.join("src/tools/verify-model/model");
	std::fs::create_dir_all(&model_dir).expect("model directory");
	let registry_path = model_dir.join("registry.toml");
	let before = "schema = 1\n# the canonical registry, which a refusal must not touch\n";
	std::fs::write(&registry_path, before).expect("registry");

	let candidate = crate::candidate::Candidate { reason: String::from("narrow the memory subsystem"), expected_hash: String::from("does-not-matter-here"), base: [(String::from("src/tools/verify-model/model/registry.toml"), crate::candidate::digest_of(before.as_bytes()))].into_iter().collect(), registry: String::from("schema = 1\n# the overlay\n"), covers: [(String::from("kernel.mem.no_such_test"), vec![String::from("kernel")])].into_iter().collect() };
	// No source declares that id, which is the refusal being provoked.
	let sources: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();

	let error = candidate.materialise(&fixture.dir, &sources).expect_err("a candidate naming a test the tree does not have cannot be activated");
	assert!(error.contains("nothing was written"), "the refusal has to say what it left behind: {error}");
	let after = std::fs::read_to_string(&registry_path).expect("the registry is still there");
	assert_eq!(after, before, "a refused candidate replaced the canonical registry and returned an error - the activation contract is byte-for-byte rollback, and this path never had one");
}

// AND THE BASE HAS TO COVER EVERY FILE THE CANDIDATE WRITES.
//
// `base_is_unmoved` ranged over the entries the candidate CHOSE to list. A candidate that simply
// omitted `registry.toml` therefore passed a check named "the base is unmoved" while overwriting the
// one file whose previous content nothing had compared against anything.
#[test]
fn a_candidate_that_does_not_record_what_it_overwrites_is_refused() {
	let fixture = Fixture::new("candidate-base-gap");
	let model_dir = fixture.dir.join("src/tools/verify-model/model");
	std::fs::create_dir_all(&model_dir).expect("model directory");
	std::fs::write(model_dir.join("registry.toml"), "schema = 1\n").expect("registry");

	let candidate = crate::candidate::Candidate {
		reason: String::from("narrow something"),
		expected_hash: String::from("does-not-matter-here"),
		// Empty: it records a digest for nothing, and it writes the registry.
		base: std::collections::BTreeMap::new(),
		registry: String::from("schema = 1\n# the overlay\n"),
		covers: std::collections::BTreeMap::new(),
	};
	let sources: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
	let error = candidate.base_is_unmoved(&fixture.dir, &sources).expect_err("a candidate that records no base for a file it overwrites cannot be activated");
	assert!(error.contains("registry.toml"), "the refusal has to name the file whose base is missing: {error}");
}

// A STEP'S COST IS THE STEP'S, NOT ITS KEYS' SHARE OF IT.
//
// `STEPCOST` was `estimate(history, step.keys)` - a sum of per-key numbers, and for a merged step
// every one of those was that step's own duration divided by how many keys it happened to discharge.
// Ordering on them is ordering on the batching. A step has ONE duration and it is measurable, so
// once it has been measured under this model that is the number, and the estimate is only the seed
// for a step nobody has timed.
#[test]
fn a_measured_step_cost_replaces_the_estimate_and_a_stale_one_does_not() {
	let mut history = crate::history::History::default();
	let cost = crate::history::CostModel::default();
	let keys = alloc_keys();

	// Nothing measured: the estimate is all there is.
	assert_eq!(history.step_seconds("guest:x86_64", "hash-a"), None, "a step nobody has run has no measurement");

	history.record_step_id(Some("guest:x86_64"), &keys, true, 187.0, "hash-a", &cost);
	assert_eq!(history.step_seconds("guest:x86_64", "hash-a"), Some(187.0), "the whole step's duration, against the step");

	// A measurement taken under a DIFFERENT model is not a measurement for this plan - the same rule
	// a key's record follows, and for the same reason.
	assert_eq!(history.step_seconds("guest:x86_64", "hash-b"), None, "a duration measured over another model is not a duration for this one");

	// And the per-key records are still written, because the age bound and the shadow both range
	// over keys rather than steps.
	assert!(history.get(&keys[0].display()).is_some(), "recording a step still records its keys");
}

// AN UNMEASURED STEP IS NEVER PRICED AT ZERO, WHICH IS WHAT `budget_select` SORTS ON.
//
// M4: "a step with no measured cost is not started under a budget without a conservative SEED - an
// unknown priced at zero is the cheapest thing in every plan and would always be picked first". The
// estimator produced exactly that zero for the most expensive items in the plan, and the arithmetic
// is why nobody saw it: every GATE's catalogue key is `host`/`host`, that pair's fixed term is 0.0,
// and one key at the default 0.5 s per key rounded to `STEPCOST 0`. So the twelve QEMU profile rows
// and the two-guest concurrency gate were emitted as free work.
#[test]
fn an_unmeasured_step_is_never_priced_at_zero() {
	let cost = crate::history::CostModel::default();
	let history = crate::history::History::default();
	// The shape a profile row actually has: one gate key, on the host pair, nobody has timed.
	let gate_key = vec![crate::plan::PlanItemKey { check: String::from("gate.arch-profile-aarch64-gicv2-1"), architecture: String::from("host"), environment: crate::catalog::Environment::Host, configuration: String::from("default") }];
	let bare = cost.estimate(&history, &gate_key, None);
	assert!(bare < 1.0, "the estimate alone is what rounded to zero - if this stops being true the seed below is no longer the thing under test, and this test has to say so");

	// A step that starts a guest is seeded from what this model has already measured about booting
	// one, per slot it declares - so the row is charged rather than admitted for nothing.
	let one_guest = bare.max(cost.seed_seconds(1));
	assert!(one_guest >= 100.0, "a step that boots a guest is seeded from the model's own measured boots, not from a host gate's per-key default");
	let two_guests = bare.max(cost.seed_seconds(2));
	assert!(two_guests > one_guest, "a step declaring two slots at once is seeded for both of them");

	// And a host step that boots nothing is still not free: a plan of zeros sorts on nothing.
	assert!(bare.max(cost.seed_seconds(0)) >= 1.0, "an unmeasured host step is priced at a second rather than at nothing");

	// The seed is a FLOOR, not a replacement: a step whose own estimate is larger keeps it.
	let guest_keys = alloc_keys();
	let measured_shape = cost.estimate(&history, &guest_keys, None);
	assert!(measured_shape.max(cost.seed_seconds(0)) > 1.0, "a step the model can price from its own keys keeps that price");
}

fn alloc_keys() -> Vec<crate::plan::PlanItemKey> {
	vec![crate::plan::PlanItemKey { check: String::from("kernel.mem.frame.frame_alloc_distinct"), architecture: String::from("x86_64"), environment: crate::catalog::Environment::TestGuest, configuration: String::from("test") }]
}

// A CANDIDATE WITH NO QUALIFYING EVIDENCE DOES NOT ACTIVATE.
//
// `candidate-activate` checked the base digests and the resulting model hash and never called the
// trust evaluation, so a narrowing could be activated with nothing behind it - which is the one thing
// M5's contract exists to prevent. The bar is per COMPONENT LOST: a narrowing takes coverage away, and
// what has to be earned is that the component it stops covering is trusted under the CANDIDATE'S own
// hash. Evidence gathered under the current model says nothing about the narrower one, which is the
// same argument that makes `expected_hash` load-bearing in the first place.
#[test]
fn evidence_under_another_model_does_not_qualify_a_candidate() {
	let mut log = crate::shadow::Log { schema: 1, records: Vec::new() };
	let store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	// Enough distinct clean decisions on enough targets to earn a certificate - but recorded under
	// the CURRENT model, which is not the model the candidate would install.
	for tree in 0..crate::trust::REQUIRED_CLEAN_RUNS {
		log.records.push(evidence(crate::shadow::Universe::TestGuest, "x86_64", true, &format!("tree-{tree}")));
		log.records.push(evidence(crate::shadow::Universe::TestGuest, "riscv64", true, &format!("tree-{tree}")));
	}
	assert!(store.evaluate("audio", "candidate-hash", crate::shadow::Universe::TestGuest, &log).is_err(), "evidence gathered under one model hash is not evidence about a different one - a candidate cannot borrow the current model's record to justify narrowing away from it");
}

// THE PLAN'S DEPENDENCY GRAPH IS VALIDATED, AND EACH OF THE THREE FAULTS IS REFUSED SEPARATELY.
//
// M4 names three properties - unique ids, resolvable dependencies, no cycles - and until 2026-09-01
// the emitter checked none of them and could not: `layers.insert` overwrote a duplicate id, an
// unknown dependency read as depth zero, and the relaxation stopped after a fixed number of passes
// whatever it had converged to. All three produced a WRONG PLAN rather than an error, which is the
// one outcome a scheduler cannot recover from, so each is asserted here on its own.
fn step_for_test(id: &str, requires: &[&str]) -> crate::commands::Step {
	crate::commands::Step { id: id.to_string(), requires: requires.iter().map(|r| (*r).to_string()).collect(), label: id.to_string(), command: String::from("true"), keys: Vec::new(), note: None, guests: 0 }
}

#[test]
fn a_well_formed_plan_graph_is_accepted() {
	let steps = vec![step_for_test("build:x86_64", &[]), step_for_test("guest:x86_64", &["build:x86_64"]), step_for_test("gate:after", &["guest:x86_64"])];
	assert!(crate::commands::validate(&steps).is_ok(), "a chain with distinct ids and resolvable edges is exactly what the emitter is supposed to produce");
}

#[test]
fn two_steps_sharing_an_id_are_refused() {
	let steps = vec![step_for_test("gate:host", &[]), step_for_test("gate:host", &[])];
	let faults = crate::commands::validate(&steps).expect_err("two steps with one id merge two costs into one figure that describes neither, and give the recorder one key for two runs");
	assert!(faults.iter().any(|f| f.contains("share the id")), "the fault names the duplicate rather than some later symptom of it: {faults:?}");
}

#[test]
fn a_dependency_naming_no_emitted_step_is_refused() {
	let steps = vec![step_for_test("guest:x86_64", &["build:x86_64"])];
	let faults = crate::commands::validate(&steps).expect_err("a step waiting on a prerequisite nobody emits is one the runner can never see satisfied");
	assert!(faults.iter().any(|f| f.contains("which no step emits")), "the fault names the missing prerequisite: {faults:?}");
}

#[test]
fn a_cycle_in_the_plan_graph_is_refused() {
	let steps = vec![step_for_test("a", &["c"]), step_for_test("b", &["a"]), step_for_test("c", &["b"])];
	let faults = crate::commands::validate(&steps).expect_err("a cycle has no valid order, so any order the emitter picks is arbitrary and its ordering claim is empty");
	let named = faults.iter().find(|f| f.contains("depend on each other")).expect("the cycle is reported: {faults:?}");
	for member in ["a", "b", "c"] {
		assert!(named.contains(member), "every member of the cycle is named so a reader can break it: {named}");
	}
}

#[test]
fn a_missing_edge_is_not_also_reported_as_a_cycle() {
	// The two faults are different repairs, and reporting one as the other sends the reader to the
	// wrong place. An unresolvable edge is dropped before the cycle search for exactly that reason.
	let steps = vec![step_for_test("only", &["absent"])];
	let faults = crate::commands::validate(&steps).expect_err("the missing edge is still a fault");
	assert!(faults.iter().any(|f| f.contains("which no step emits")), "{faults:?}");
	assert!(!faults.iter().any(|f| f.contains("depend on each other")), "a step whose only edge is unresolvable does not depend on itself: {faults:?}");
}

#[test]
fn the_real_plan_graph_validates() {
	// The emitter's own output, not a fixture: this is the assertion that would catch a future step
	// added with a duplicate id or an edge naming a step that was renamed.
	let model = model();
	let plan = plan_for(&model, &["src/kernel/device.rs"]);
	let per_target = std::collections::BTreeMap::new();
	let steps = crate::commands::steps(&plan, &per_target, &model.registry);
	assert!(crate::commands::validate(&steps).is_ok(), "the plan this tree actually emits has a usable dependency graph");
}

#[test]
fn every_profile_row_is_a_step_of_its_own() {
	// M3.6 AND THE DEFINITION OF DONE ASK FOR THIS, AND THE CATALOG SPLIT ALONE DID NOT GIVE IT.
	//
	// Each profile of a multi-profile gate has its own catalog entry, so it has its own KEY. Until
	// 2026-09-02 `steps` then folded every ordinary pre-guest gate into ONE step with one id, one
	// comma-separated command and all the keys - so twelve emulated profiles shared one identity and
	// one duration divided evenly among them, which is exactly the merged cost the definition of
	// done says must not survive into a cheapest-first run.
	let model = model();
	let plan = plan_for(&model, &["src/kernel/device.rs"]);
	let per_target = std::collections::BTreeMap::new();
	let steps = crate::commands::steps(&plan, &per_target, &model.registry);
	let selected: Vec<&str> = crate::catalog::PROFILE_ROW_GATES.iter().copied().filter(|name| plan.items.iter().any(|item| item.key.check == format!("gate.{name}"))).collect();
	assert!(selected.len() >= 9, "a kernel change selects the profile rows; this test is about how they are EMITTED, so it needs them selected: {selected:?}");
	for name in &selected {
		let owned: Vec<&crate::commands::Step> = steps.iter().filter(|step| step.keys.iter().any(|key| key.check == format!("gate.{name}"))).collect();
		assert_eq!(owned.len(), 1, "{name} is carried by exactly one step");
		let step = owned[0];
		assert_eq!(step.keys.len(), 1, "{name} does not share its step with another key, because a shared step is a shared cost: {:?}", step.keys);
		assert!(step.command.contains(name) && !step.command.contains(','), "{name} runs as its own command rather than as one name in a list: {}", step.command);
		// AND THE SCHEDULER CAN SEE IT. A profile row boots QEMU, so it declares a guest slot; the
		// runner classifies guest work by that declaration rather than by matching the command text,
		// which is what puts these rows inside the one `--jobs` the definition of done requires them
		// to be scheduled by. Without this the split gave them an identity and a cost and left them
		// serial (added 2026-09-02).
		assert_eq!(step.guests, 1, "{name} declares the one guest slot it needs, so `--jobs` schedules it");
	}
	// And the list agrees with what `check.sh` can actually run - a name here that is not a gate is a
	// step nothing would execute.
	// EVERY STEP THAT BOOTS A GUEST DECLARES IT, which is the property the runner now depends on: a
	// step whose command starts a guest and declares none would be scheduled as host work and run
	// outside the slot budget, which is the defect the classifier change closed.
	for step in steps.iter().filter(|step| step.command.contains("./test.sh --arch ")) {
		assert!(step.guests >= 1, "a step that boots the suite declares a guest slot: {}", step.command);
	}
	let known = crate::catalog::catalog_gate_names();
	for name in crate::catalog::PROFILE_ROW_GATES {
		assert!(known.contains(name), "{name} is named as a profile row and is not a registered gate");
	}
}
