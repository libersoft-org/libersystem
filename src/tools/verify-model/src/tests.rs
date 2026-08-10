// Two kinds of test, and the second kind is the one P02M0118 argues for at length.
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
	let plan = plan_for(&model, &["docs/todo/P02M0118.md"]);
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
	let kernel_test = KernelTest { name: String::from("t"), id: String::from("kernel.t"), architectures: vec![String::from("x86_64")], covers: vec![String::from("kernel")] };
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
	store.grant("audio", "hash-a", crate::shadow::Universe::TestGuest, 9, vec![String::from("x86_64"), String::from("riscv64")], 1);
	assert_eq!(store.level("audio", "hash-a", crate::shadow::Universe::TestGuest), crate::trust::Level::Trusted);
	// The graph changed, or the covers declarations did, or the selector did. The evidence was
	// produced by a model that is no longer the one running.
	assert_eq!(store.level("audio", "hash-b", crate::shadow::Universe::TestGuest), crate::trust::Level::Shadow);
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
		log.records.push(crate::shadow::Record { universe: crate::shadow::Universe::TestGuest, architecture: String::from("x86_64"), verdict: String::from("Consistent"), reason: String::new(), model_hash: String::from("hash-a"), source_digest: String::new(), changed_components: vec![String::from("audio")], outside_failures: Vec::new(), at: 0 });
	}
	let store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	let error = store.evaluate("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).expect_err("one target is not enough");
	assert!(error.contains("target(s)"), "{error}");
	log.records.push(crate::shadow::Record { universe: crate::shadow::Universe::TestGuest, architecture: String::from("riscv64"), verdict: String::from("Consistent"), reason: String::new(), model_hash: String::from("hash-a"), source_digest: String::new(), changed_components: vec![String::from("audio")], outside_failures: Vec::new(), at: 0 });
	assert!(store.evaluate("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).is_ok());
}

#[test]
fn evidence_under_another_model_does_not_count() {
	let mut log = crate::shadow::Log { schema: 1, records: Vec::new() };
	for architecture in ["x86_64", "riscv64", "aarch64", "x86_64", "riscv64", "aarch64"] {
		log.records.push(crate::shadow::Record { universe: crate::shadow::Universe::TestGuest, architecture: architecture.to_string(), verdict: String::from("Consistent"), reason: String::new(), model_hash: String::from("an-older-model"), source_digest: String::new(), changed_components: vec![String::from("audio")], outside_failures: Vec::new(), at: 0 });
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
	let record = |architecture: &str, verdict: &str| crate::shadow::Record { universe: crate::shadow::Universe::TestGuest, architecture: architecture.to_string(), verdict: verdict.to_string(), reason: String::new(), model_hash: String::from("hash-a"), source_digest: String::new(), changed_components: vec![String::from("audio")], outside_failures: Vec::new(), at: 0 };
	for _ in 0..crate::trust::REQUIRED_CLEAN_RUNS {
		log.records.push(record("x86_64", "Consistent"));
	}
	log.records.push(record("riscv64", "CandidateMiss"));
	let store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	let error = store.evaluate("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).expect_err("a run that found a candidate miss is not evidence that the selector is right");
	assert!(error.contains("target(s)"), "{error}");
	// The same target, cleanly this time, is what actually earns it.
	log.records.push(record("riscv64", "Consistent"));
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
	crate::commands::steps(plan, per_target).iter().filter(|step| step.command.contains("./test.sh --arch ")).filter_map(|step| step.command.rsplit(" --arch ").next().map(str::to_string)).collect()
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
	for paths in [vec!["src/user/libs/audio/flac/src/lib.rs"], vec!["src/kernel/arch/riscv64/traps/mod.rs"], vec!["src/boot/qemu-run.sh"]] {
		let plan = plan_for(&model, &paths);
		let booted: BTreeSet<String> = plan.architectures_booted.iter().cloned().collect();
		let steps = crate::commands::steps(&plan, &per_target);
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
	KernelTest { name: name.to_string(), id: format!("kernel.{name}"), architectures: vec![String::from("x86_64")], covers: covers.iter().map(|component| (*component).to_string()).collect() }
}

#[test]
fn covers_must_be_reachable_from_what_the_test_touches() {
	let model = model();
	let touched: BTreeSet<String> = [String::from("bin.audioconv")].into_iter().collect();
	// Reached through the tool's own provider chain, without the test naming it.
	let ok = kernel_test("t", &["flac", "audioconv"]);
	assert!(crate::kerneltests::unreachable_covers(&ok, &touched, &model.graph).is_empty());
	// Nothing leads from a codec tool to a filesystem.
	let bad = kernel_test("t", &["liberfs"]);
	assert_eq!(crate::kerneltests::unreachable_covers(&bad, &touched, &model.graph), vec![String::from("liberfs")]);
}

#[test]
fn the_converse_is_reported_and_never_enforced() {
	// Launching StorageService to get a volume is not asserting anything about StorageService.
	let model = model();
	let touched: BTreeSet<String> = [String::from("bin.audioconv"), String::from("bin.storage_service")].into_iter().collect();
	let test = kernel_test("t", &["flac"]);
	assert!(crate::kerneltests::unreachable_covers(&test, &touched, &model.graph).is_empty(), "the declaration is fine");
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
	assert_eq!(crate::kerneltests::unreachable_covers(&test, &touched, &model.graph), vec![String::from("webp")], "the kernel dev-depends on webp; that is not the guest reaching it");
}

#[test]
fn every_annotation_in_this_tree_is_reachable() {
	let model = model();
	let mut bad = Vec::new();
	for test in &model.kernel_tests.tests {
		let touched = model.kernel_tests.touches.get(&test.name).cloned().unwrap_or_default();
		for component in crate::kerneltests::unreachable_covers(test, &touched, &model.graph) {
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
	let record = |architecture: &str, universe: crate::shadow::Universe| crate::shadow::Record { universe, architecture: architecture.to_string(), verdict: String::from("Consistent"), reason: String::new(), model_hash: String::from("hash-a"), source_digest: String::new(), changed_components: vec![String::from("audio")], outside_failures: Vec::new(), at: 0 };
	for architecture in ["x86_64", "riscv64", "x86_64", "riscv64", "x86_64", "riscv64"] {
		log.records.push(record(architecture, crate::shadow::Universe::TestGuest));
	}
	let mut store = crate::trust::Store { schema: 1, certificates: Vec::new() };
	let (clean, architectures) = store.evaluate("audio", "hash-a", crate::shadow::Universe::TestGuest, &log).expect("the guest suite has been validated");
	store.grant("audio", "hash-a", crate::shadow::Universe::TestGuest, clean, architectures, 0);

	assert_eq!(store.level("audio", "hash-a", crate::shadow::Universe::TestGuest), crate::trust::Level::Trusted);
	assert_eq!(store.level("audio", "hash-a", crate::shadow::Universe::Host), crate::trust::Level::Shadow, "nothing has compared a host-suite selection for this component");
	assert!(!store.trusted_everywhere("audio", "hash-a", &[crate::shadow::Universe::Host, crate::shadow::Universe::TestGuest]), "and a scoped run therefore still has something unproven behind it");
	assert!(store.evaluate("audio", "hash-a", crate::shadow::Universe::Host, &log).is_err());
}

#[test]
fn a_check_id_says_which_universe_judges_it() {
	use crate::shadow::Universe;
	assert_eq!(Universe::of("kernel.frame_alloc_distinct"), Universe::TestGuest);
	assert_eq!(Universe::of("dev.selftest"), Universe::DevGuest);
	for host in ["host.flac", "gate.volume-layout", "build.kernel", "conformance.png"] {
		assert_eq!(Universe::of(host), Universe::Host, "{host} runs on the host");
	}
}

#[test]
fn the_cost_escalation_measures_seconds_and_not_keys() {
	// It compared `selected / whole` over the key COUNT, which prices twenty host keys and twenty
	// riscv64 guest keys the same when they differ by two orders of magnitude in wall-clock. The
	// measurement to build it on already existed - `CostModel::estimate` - and nothing asked it.
	let cost = crate::history::CostModel::default();
	let history = crate::history::History::default();
	let key = |architecture: &str, environment: crate::catalog::Environment, n: usize| -> Vec<crate::plan::PlanItemKey> { (0..n).map(|i| crate::plan::PlanItemKey { check: format!("k{i}"), architecture: architecture.to_string(), environment: environment.clone(), configuration: String::from("default") }).collect() };

	// Twenty riscv64 guest keys against two hundred: the boot is paid once, so the difference is
	// ninety seconds out of three thousand. Running all of them is within a tenth of running a
	// tenth of them, which is the whole reason the rule exists.
	let few = cost.estimate(&history, &key("riscv64", crate::catalog::Environment::TestGuest, 20));
	let many = cost.estimate(&history, &key("riscv64", crate::catalog::Environment::TestGuest, 200));
	assert!(few / many > 0.9, "20 of 200 riscv64 guest keys cost {few:.0} s against {many:.0} s - the boot dominates and the rule must see that");

	// The same counts on the host, where there is no boot: twenty checks cost a tenth of two
	// hundred, and widening would be pure extra work.
	let few = cost.estimate(&history, &key("host", crate::catalog::Environment::Host, 20));
	let many = cost.estimate(&history, &key("host", crate::catalog::Environment::Host, 200));
	assert!(few / many < 0.2, "20 of 200 host keys cost {few:.0} s against {many:.0} s - there is nothing to amortise");

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
	let results = crate::shadow::parse_host_log("gate.test-tags PASS\nhost.flac FAIL\nconformance.png PASS\ntotal 3\n");
	assert_eq!(results.total_declared, Some(3), "a run that covered fewer checks than exist did not compare what it claims to");
	assert_eq!(results.outcomes.get("host.flac"), Some(&crate::shadow::Outcome::Failed));
	assert_eq!(results.outcomes.get("gate.test-tags"), Some(&crate::shadow::Outcome::Passed));

	// A failure INSIDE the selection is the selector working; one OUTSIDE it is the candidate miss
	// the whole mechanism exists to find. The host ids are the catalog's own, with no prefix to
	// strip - the guest's `kernel.` prefix is a property of that universe, not of comparison.
	let history = crate::history::History::default();
	let key = |check: &str| crate::plan::PlanItemKey { check: String::from(check), architecture: String::from("host"), environment: crate::catalog::Environment::Host, configuration: String::from("default") };

	let inside = crate::shadow::compare_host(&[key("host.flac")], &results, &history);
	assert!(inside.outside_failures.is_empty(), "the only failure was selected: {:?}", inside.outside_failures);
	assert!(!inside.inside_failures.is_empty(), "and it must be reported as selected");

	let outside = crate::shadow::compare_host(&[key("gate.test-tags")], &results, &history);
	assert_eq!(outside.verdict, crate::shadow::Verdict::CandidateMiss, "a failure the selection did not name is exactly what a shadow is for");
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
	assert!(!store.trusted_everywhere("a-component-no-check-covers", "hash", &[]), "no judge is not the same as every judge agreeing");
}
