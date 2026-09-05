use super::*;
use crate::catalog::Catalog;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);
const TOOL: &str = "src/tools/image-bench/src/main.rs";

struct Fixture {
	root: PathBuf,
}
impl Fixture {
	fn new() -> Self {
		let root = std::env::temp_dir().join(format!("verify-tier-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
		fs::create_dir_all(&root).unwrap();
		let fixture = Self { root };
		fixture.git(&["init", "-q"]);
		fixture.git(&["config", "user.email", "tier@example.invalid"]);
		fixture.git(&["config", "user.name", "Tier fixture"]);
		fixture.git(&["config", "core.filemode", "true"]);
		fixture.write(".gitignore", ".build/\n");
		fixture.write("ordinary.rs", "original\n");
		fixture.commit();
		fixture
	}
	fn git(&self, arguments: &[&str]) -> String {
		String::from_utf8(git(&self.root, arguments).unwrap()).unwrap().trim().into()
	}
	fn write(&self, path: &str, text: &str) {
		let path = self.root.join(path);
		fs::create_dir_all(path.parent().unwrap()).unwrap();
		fs::write(path, text).unwrap();
	}
	fn commit(&self) -> String {
		self.git(&["add", "-A"]);
		self.git(&["commit", "-qm", "fixture", "--allow-empty"]);
		head(&self.root).unwrap()
	}
	fn model(&self, names: &[(&str, &str)]) -> Model {
		let actual = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(3).unwrap();
		let mut model = Model::load(actual).unwrap();
		for file in fs::read_dir(actual.join("src/tools/verify-model/src")).unwrap().flatten() {
			if file.path().extension().is_some_and(|extension| extension == "rs") {
				self.write(&format!("src/tools/verify-model/src/{}", file.file_name().to_str().unwrap()), &fs::read_to_string(file.path()).unwrap());
			}
		}
		let source: String = names.iter().map(|(name, covers)| format!("tagged_test!({name}, [Boot], id = \"kernel.{name}\", covers = [{covers}]);\n")).collect();
		self.write("src/kernel/cases.rs", &source);
		self.write(TOOL, "fn main() {}\n");
		model.repo_root = self.root.clone();
		self.refresh(&mut model);
		model
	}
	// Real object symbols go through the production nm/discovery scanner. The object is tiny
	// and host-compiled; architecture applicability is controlled at the same discovery seam.
	fn inventory(&self, target: &str, names: &[&str]) {
		let triple = match target {
			"x86_64" => "x86_64-unknown-none",
			"aarch64" => "aarch64-unknown-none",
			"riscv64" => "riscv64gc-unknown-none-elf",
			_ => panic!("unknown target"),
		};
		let directory = self.root.join(".build/cargo/kernel").join(triple).join("debug/deps");
		fs::create_dir_all(&directory).unwrap();
		let assembly: String = names
			.iter()
			.map(|name| {
				let symbol = format!("_RNvCs1_6kernel{}{name}4CASE", name.len());
				format!(".globl {symbol}\n{symbol}:\n.byte 0\n")
			})
			.collect();
		let source = self.root.join(".build/inventory.s");
		fs::write(&source, assembly).unwrap();
		let status = Command::new("cc").arg("-c").arg(&source).arg("-o").arg(directory.join("kernel-fixture")).status().unwrap();
		assert!(status.success());
	}
	fn refresh(&self, model: &mut Model) {
		model.kernel_tests = crate::kerneltests::discover(&self.root, &crate::registry::ARCHITECTURES).unwrap();
		model.catalog = Catalog::build(&model.crates, &model.registry, &model.graph, &model.staged, &model.kernel_tests.tests);
	}
	fn inner(&self, model: &Model, paths: &[String]) -> Handoff {
		let (mut handoff, steps) = begin_inner(model, effective_tree(&self.root).unwrap(), paths, BTreeMap::new()).unwrap();
		let recorded: Keys = steps.iter().flat_map(|step| step.keys.iter().cloned()).collect();
		assert_eq!(recorded, handoff.inner);
		finish_inner(&self.root, &mut handoff, pass(&recorded)).unwrap();
		handoff
	}
}
impl Drop for Fixture {
	fn drop(&mut self) {
		let _ = fs::remove_dir_all(&self.root);
	}
}
fn pass(keys: &Keys) -> BTreeMap<String, bool> {
	keys.iter().map(|key| (key.display(), true)).collect()
}
fn tool_paths() -> Vec<String> {
	vec![TOOL.into()]
}

#[test]
fn effective_tree_is_commit_stable_for_add_delete_rename_modes_and_byte_paths() {
	for operation in ["modify", "add", "delete", "rename", "symlink", "user-exec", "group-exec", "other-exec", "bytes"] {
		let fixture = Fixture::new();
		match operation {
			"modify" => fixture.write("ordinary.rs", "changed\n"),
			"add" => fixture.write("added.rs", "new\n"),
			"delete" => fs::remove_file(fixture.root.join("ordinary.rs")).unwrap(),
			"rename" => fs::rename(fixture.root.join("ordinary.rs"), fixture.root.join("renamed.rs")).unwrap(),
			"symlink" => std::os::unix::fs::symlink("ordinary.rs", fixture.root.join("link.rs")).unwrap(),
			"user-exec" => fs::set_permissions(fixture.root.join("ordinary.rs"), fs::Permissions::from_mode(0o754)).unwrap(),
			"group-exec" => fs::set_permissions(fixture.root.join("ordinary.rs"), fs::Permissions::from_mode(0o654)).unwrap(),
			"other-exec" => fs::set_permissions(fixture.root.join("ordinary.rs"), fs::Permissions::from_mode(0o645)).unwrap(),
			"bytes" => fs::write(fixture.root.join(OsStr::from_bytes(b"line\nwith\tbytes-\xff.rs")), b"new").unwrap(),
			_ => unreachable!(),
		}
		fixture.write(".build/ignored", "does not enter identity");
		let before = effective_tree(&fixture.root).unwrap();
		let revision = fixture.commit();
		assert_eq!(before, effective_tree(&fixture.root).unwrap(), "{operation}");
		assert_eq!(before, commit_tree(&fixture.root, &revision).unwrap(), "{operation}");
	}
}

#[test]
fn identity_rejects_gitlinks() {
	let fixture = Fixture::new();
	let revision = head(&fixture.root).unwrap();
	fixture.git(&["update-index", "--add", "--cacheinfo", &format!("160000,{revision},module")]);
	assert!(effective_tree(&fixture.root).unwrap_err().contains("gitlinks"));
	fixture.git(&["commit", "-qm", "submodule"]);
	assert!(commit_tree(&fixture.root, "HEAD").unwrap_err().contains("gitlinks"));
}

#[test]
fn partial_staging_and_every_post_inner_tree_mutation_are_refused() {
	let fixture = Fixture::new();
	let model = fixture.model(&[("hot", "kernel")]);
	fixture.commit();
	fixture.write("ordinary.rs", "staged A\n");
	fixture.git(&["add", "ordinary.rs"]);
	fixture.write("ordinary.rs", "tested B\n");
	let handoff = fixture.inner(&model, &["docs/example.md".into()]);
	fixture.git(&["commit", "-qm", "staged A"]);
	assert!(prepare_merge(&model, handoff.clone()).unwrap_err().contains("proposed commit tree mismatch"));
	fixture.commit();
	assert!(prepare_merge(&model, handoff.clone()).is_ok(), "index disagreement is fine once tested B is proposed");
	let original = fs::read(fixture.root.join("ordinary.rs")).unwrap();
	for mutation in ["content", "mode", "delete", "rename"] {
		match mutation {
			"content" => fixture.write("ordinary.rs", "moved"),
			"mode" => fs::set_permissions(fixture.root.join("ordinary.rs"), fs::Permissions::from_mode(0o755)).unwrap(),
			"delete" => fs::remove_file(fixture.root.join("ordinary.rs")).unwrap(),
			"rename" => fs::rename(fixture.root.join("ordinary.rs"), fixture.root.join("renamed.rs")).unwrap(),
			_ => unreachable!(),
		}
		let error = prepare_merge(&model, handoff.clone()).unwrap_err();
		assert!(error.contains("effective tree mismatch") && error.contains(&handoff.effective_tree), "{mutation}: {error}");
		let _ = fs::remove_file(fixture.root.join("renamed.rs"));
		fs::write(fixture.root.join("ordinary.rs"), &original).unwrap();
		fs::set_permissions(fixture.root.join("ordinary.rs"), fs::Permissions::from_mode(0o644)).unwrap();
	}
}

#[test]
fn ordinary_dirty_commit_transitions_are_accepted_by_merge() {
	for operation in ["modify", "add", "delete", "rename"] {
		let fixture = Fixture::new();
		let model = fixture.model(&[("hot", "kernel")]);
		fixture.commit();
		match operation {
			"modify" => fixture.write("ordinary.rs", "changed\n"),
			"add" => fixture.write("added.rs", "new\n"),
			"delete" => fs::remove_file(fixture.root.join("ordinary.rs")).unwrap(),
			"rename" => fs::rename(fixture.root.join("ordinary.rs"), fixture.root.join("renamed.rs")).unwrap(),
			_ => unreachable!(),
		}
		let changed = crate::changes::paths(&crate::changes::working_tree(&fixture.root).unwrap());
		let handoff = fixture.inner(&model, &changed);
		fixture.commit();
		let state = prepare_merge(&model, handoff).unwrap();
		assert_eq!(keys(&state.plan), keys(&state.handoff.plan), "{operation} should add no work");
	}
}

#[test]
fn source_hash_is_stable_across_missing_stale_and_refreshed_binaries() {
	let fixture = Fixture::new();
	let mut model = fixture.model(&[("old", "kernel"), ("new", "kernel")]);
	let source_hash = model.source_model_hash().unwrap();
	let absent_hash = model.model_hash();
	assert!(model.kernel_tests.tests.is_empty());
	for target in crate::registry::ARCHITECTURES {
		fixture.inventory(target, &["old"]);
	}
	fixture.refresh(&mut model);
	assert!(model.kernel_tests.declared_not_built.iter().any(|id| id == "kernel.new"));
	assert_eq!(source_hash, model.source_model_hash().unwrap());
	assert_ne!(absent_hash, model.model_hash());
	for target in crate::registry::ARCHITECTURES {
		fixture.inventory(target, &["old", "new"]);
	}
	fixture.refresh(&mut model);
	assert_eq!(source_hash, model.source_model_hash().unwrap());
	let handoff = fixture.inner(&model, &["docs/change.md".into()]);
	fixture.write("src/kernel/cases.rs", "tagged_test!(old, [Boot], id = \"kernel.old\", covers = [kernel]);\ntagged_test!(new, [Boot], id = \"kernel.new\", covers = [services]);\n");
	assert_ne!(source_hash, model.source_model_hash().unwrap(), "a declaration absent from old binaries must still enter the hash");
	let refusal = source_matches(&model, &handoff).unwrap_err();
	assert!(refusal.contains("source model mismatch") && refusal.contains(&source_hash));
}

#[test]
fn stale_port_and_inner_owned_x86_inventories_discover_keys_with_individual_outcomes() {
	let fixture = Fixture::new();
	let mut model = fixture.model(&[("old", "image-bench"), ("new", "image-bench")]);
	for target in crate::registry::ARCHITECTURES {
		fixture.inventory(target, &["old"]);
	}
	fixture.refresh(&mut model);
	fixture.commit();
	fixture.write(TOOL, "fn main() { let changed = 1; }\n");
	let handoff = fixture.inner(&model, &tool_paths());
	assert!(!handoff.plan.full, "a genuinely scopeable host-tool fixture is required");
	assert!(!handoff.deferred.is_empty());
	assert!(handoff.inner.iter().any(|key| key.architecture == "x86_64"));
	assert!(handoff.deferred.iter().all(|key| matches!(key.architecture.as_str(), "aarch64" | "riscv64")));
	fixture.commit();
	let mut state = prepare_merge(&model, handoff).unwrap();
	assert_eq!(state.inventory_targets, crate::registry::ARCHITECTURES.into_iter().map(String::from).collect());
	for target in &state.inventory_targets {
		fixture.inventory(target, &["old", "new"]);
	}
	fixture.refresh(&mut model);
	let steps = merge_commands(&model, &mut state).unwrap();
	let recorded: Keys = steps.iter().flat_map(|step| step.keys.iter().cloned()).collect();
	for target in crate::registry::ARCHITECTURES {
		assert!(recorded.iter().any(|key| key.check == "kernel.new" && key.architecture == target), "new test must receive a key on {target}");
	}
	assert!(state.handoff.deferred.is_subset(&recorded));
	finish_merge(&fixture.root, &mut state, &pass(&recorded)).unwrap();
}

#[test]
fn first_compilation_retains_whole_suite_fallback_alongside_discovered_keys() {
	let fixture = Fixture::new();
	let mut model = fixture.model(&[("old", "image-bench")]);
	fixture.commit();
	fixture.write(TOOL, "fn main() { let changed = 1; }\n");
	let handoff = fixture.inner(&model, &tool_paths());
	assert!(handoff.deferred.iter().any(|key| key.check == "guest.whole-suite"));
	fixture.commit();
	let mut state = prepare_merge(&model, handoff).unwrap();
	for target in &state.inventory_targets {
		fixture.inventory(target, &["old"]);
	}
	fixture.refresh(&mut model);
	let steps = merge_commands(&model, &mut state).unwrap();
	let recorded: Keys = steps.iter().flat_map(|step| step.keys.iter().cloned()).collect();
	assert!(recorded.iter().any(|key| key.check == "guest.whole-suite"));
	assert!(recorded.iter().any(|key| key.check == "kernel.old"));
	finish_merge(&fixture.root, &mut state, &pass(&recorded)).unwrap();
}

#[test]
fn both_tiers_refuse_an_ordinary_edit_that_leaves_the_source_model_hash_unchanged() {
	let fixture = Fixture::new();
	let mut model = fixture.model(&[("old", "kernel")]);
	for target in crate::registry::ARCHITECTURES {
		fixture.inventory(target, &["old"]);
	}
	fixture.refresh(&mut model);
	fixture.commit();
	fixture.write("ordinary.rs", "tested\n");
	let (mut pending, _) = begin_inner(&model, effective_tree(&fixture.root).unwrap(), &["docs/example.md".into()], BTreeMap::new()).unwrap();
	let source_hash = model.source_model_hash().unwrap();
	fixture.write("ordinary.rs", "edited during inner\n");
	assert_eq!(source_hash, model.source_model_hash().unwrap());
	assert!(finish_inner(&fixture.root, &mut pending, BTreeMap::new()).unwrap_err().contains("effective tree mismatch"));
	assert!(!pending.inner_complete);
	fixture.write("ordinary.rs", "tested\n");
	finish_inner(&fixture.root, &mut pending, BTreeMap::new()).unwrap();
	fixture.commit();
	let mut state = prepare_merge(&model, pending).unwrap();
	let steps = merge_commands(&model, &mut state).unwrap();
	let recorded: Keys = steps.iter().flat_map(|step| step.keys.iter().cloned()).collect();
	fixture.write("ordinary.rs", "edited during merge\n");
	assert_eq!(source_hash, model.source_model_hash().unwrap());
	assert!(finish_merge(&fixture.root, &mut state, &pass(&recorded)).unwrap_err().contains("effective tree mismatch"));
}

#[test]
fn head_movement_to_a_different_parent_same_tree_is_refused_at_both_merge_boundaries() {
	let fixture = Fixture::new();
	let mut model = fixture.model(&[("old", "kernel")]);
	fixture.commit();
	fixture.write("docs/change.md", "change\n");
	let handoff = fixture.inner(&model, &["docs/change.md".into()]);
	fixture.commit();
	let mut state = prepare_merge(&model, handoff).unwrap();
	merge_commands(&model, &mut state).unwrap();
	let original = state.revision.clone();
	let tree = fixture.git(&["rev-parse", "HEAD^{tree}"]);
	let new_head = fixture.git(&["commit-tree", &tree, "-p", &fixture.git(&["rev-parse", "HEAD~2"]), "-m", "changed parent"]);
	fixture.git(&["update-ref", "HEAD", &new_head]);
	assert_eq!(state.handoff.effective_tree, effective_tree(&fixture.root).unwrap());
	assert!(finish_merge(&fixture.root, &mut state, &BTreeMap::new()).unwrap_err().contains(&original));
	state.work = None;
	fixture.refresh(&mut model);
	assert!(merge_commands(&model, &mut state).unwrap_err().contains("HEAD moved"));
}

#[test]
fn same_tree_over_a_different_parent_adds_delta_obligations_even_when_deferred_is_empty() {
	for nonempty in [false, true] {
		let fixture = Fixture::new();
		let mut model = fixture.model(&[("old", "kernel")]);
		for target in crate::registry::ARCHITECTURES {
			fixture.inventory(target, &["old"]);
		}
		fixture.refresh(&mut model);
		let older = fixture.commit();
		fixture.write(TOOL, "fn main() { let earlier = 1; }\n");
		fixture.commit();
		let paths = if nonempty {
			fixture.write("src/user/services/core/src/log_service.rs", "a service change\n");
			vec!["src/user/services/core/src/log_service.rs".into()]
		} else {
			fixture.write("docs/change.md", "docs\n");
			vec!["docs/change.md".into()]
		};
		let handoff = fixture.inner(&model, &paths);
		assert_eq!(!handoff.deferred.is_empty(), nonempty);
		fixture.commit();
		let tree = fixture.git(&["rev-parse", "HEAD^{tree}"]);
		let revision = fixture.git(&["commit-tree", &tree, "-p", &older, "-m", "same tree new parent"]);
		fixture.git(&["update-ref", "HEAD", &revision]);
		let mut state = prepare_merge(&model, handoff).unwrap();
		let original: Keys = state.handoff.inner.union(&state.handoff.deferred).cloned().collect();
		assert!(!keys(&state.plan).difference(&original).collect::<Vec<_>>().is_empty());
		let steps = merge_commands(&model, &mut state).unwrap();
		let recorded: Keys = steps.iter().flat_map(|step| step.keys.iter().cloned()).collect();
		assert!(!recorded.difference(&original).collect::<Vec<_>>().is_empty());
		finish_merge(&fixture.root, &mut state, &pass(&recorded)).unwrap();
	}
}

#[test]
fn missing_failed_or_overlapping_inner_evidence_is_never_accepted() {
	let fixture = Fixture::new();
	let model = fixture.model(&[("old", "kernel")]);
	fixture.commit();
	fixture.write(TOOL, "changed\n");
	let handoff = fixture.inner(&model, &tool_paths());
	assert!(!handoff.inner.is_empty());
	fixture.commit();
	for defect in ["missing", "failed", "overlap", "unsealed"] {
		let mut broken = handoff.clone();
		let key = broken.inner.first().unwrap().clone();
		match defect {
			"missing" => {
				broken.inner_outcomes.remove(&key.display());
			}
			"failed" => {
				broken.inner_outcomes.insert(key.display(), false);
			}
			"overlap" => {
				broken.deferred.insert(key);
			}
			"unsealed" => {
				broken.inner_complete = false;
			}
			_ => unreachable!(),
		}
		assert!(prepare_merge(&model, broken).is_err(), "{defect}");
	}
}

#[test]
fn a_shared_prerequisite_executes_twice_but_its_key_is_accounted_once() {
	let host = PlanItemKey { check: "build.kernel".into(), architecture: "x86_64".into(), environment: Environment::Host, configuration: "test".into() };
	let port = PlanItemKey { check: "kernel.port".into(), architecture: "aarch64".into(), environment: Environment::TestGuest, configuration: "test".into() };
	let steps = vec![
		Step { id: "producer".into(), requires: vec![], label: "producer".into(), command: "produce".into(), keys: vec![host.clone()], note: None, guests: 0 },
		Step { id: "consumer".into(), requires: vec!["producer".into()], label: "consumer".into(), command: "consume".into(), keys: vec![port.clone()], note: None, guests: 1 },
	];
	let (inner, deferred) = partition(&steps, &[host.clone(), port.clone()].into_iter().collect()).unwrap();
	assert_eq!(inner, [host].into_iter().collect());
	assert_eq!(deferred, [port].into_iter().collect());
	let first = closed_steps(&steps, &inner).unwrap();
	let second = closed_steps(&steps, &deferred).unwrap();
	assert_eq!(first.len(), 1);
	assert_eq!(second.len(), 2);
	assert!(second[0].keys.is_empty());
	assert_eq!(second[1].requires, vec!["producer"]);
	let mut broken = steps;
	broken[1].requires = vec!["missing".into()];
	assert!(closed_steps(&broken, &deferred).is_err());
}

#[test]
fn host_only_work_needs_no_inventory_and_full_inner_cannot_report_full() {
	let fixture = Fixture::new();
	let model = fixture.model(&[("old", "kernel")]);
	let handoff = fixture.inner(&model, &["docs/change.md".into()]);
	assert!(handoff.deferred.is_empty());
	assert!(inventory_targets(&handoff, &handoff.plan).is_empty());
	let mut host_only = handoff.clone();
	let host = model.catalog.checks.iter().find(|check| check.kind == CheckKind::HostSuite).unwrap();
	let variant = host.variants.first().unwrap();
	host_only.plan.items.push(PlanItem { key: PlanItemKey { check: host.id.clone(), architecture: variant.architecture.clone(), environment: variant.environment.clone(), configuration: variant.configuration.clone() }, kind: host.kind, command: host.command.clone(), reason: "host-only selection fixture".into() });
	host_only.inner = keys(&host_only.plan);
	assert!(inventory_targets(&host_only, &host_only.plan).is_empty(), "an actual host key with no concrete policy target needs no kernel inventory");
	let full = fixture.inner(&model, &["verify.sh".into()]);
	assert!(full.plan.full);
	assert!(evidence_level(&model, &full.plan, &full.inner, &BTreeMap::new(), true).unwrap().starts_with("STALE"));
	let mut history = crate::history::History::default();
	for check in &model.catalog.checks {
		for variant in &check.variants {
			let key = PlanItemKey { check: check.id.clone(), architecture: variant.architecture.clone(), environment: variant.environment.clone(), configuration: variant.configuration.clone() };
			history.record(&key.display(), true, 1.0, &model.model_hash());
		}
	}
	history.save(&fixture.root).unwrap();
	assert!(evidence_level(&model, &full.plan, &full.inner, &BTreeMap::new(), true).unwrap().starts_with("SHADOW"));
}

#[test]
fn merge_budget_is_rejected_before_loading_or_building_any_inventory() {
	let fixture = Fixture::new();
	for budget in ["0", "1", "0.001"] {
		let error = run(&fixture.root, &["tier-merge-prepare".into(), "--budget".into(), budget.into()], |_, _| panic!("no steps may be emitted")).unwrap_err();
		assert!(error.contains("no inventory build was started"));
		assert!(!fixture.root.join(".build").exists());
	}
}

fn cost_fixture() -> (Fixture, Model, Vec<String>) {
	let fixture = Fixture::new();
	let mut declarations = vec![("hot".to_string(), "image-bench".to_string())];
	declarations.extend((0..39).map(|index| (format!("cold_{index}"), "unrelated-component".into())));
	let borrowed: Vec<_> = declarations.iter().map(|(name, covers)| (name.as_str(), covers.as_str())).collect();
	let mut model = fixture.model(&borrowed);
	let names: Vec<_> = declarations.iter().map(|(name, _)| name.clone()).collect();
	for target in crate::registry::ARCHITECTURES {
		fixture.inventory(target, &names.iter().map(String::as_str).collect::<Vec<_>>());
	}
	fixture.refresh(&mut model);
	fixture.commit();
	fixture.write(TOOL, "fn main() { let changed = 1; }\n");
	(fixture, model, names)
}

fn hot_cost(model: &Model, history: &mut crate::history::History, target: &str, wide: bool) {
	let key = PlanItemKey { check: "kernel.hot".into(), architecture: target.into(), environment: Environment::TestGuest, configuration: "test".into() };
	let cost = crate::history::CostModel::default();
	let fixed = cost.fixed_seconds[&(target.into(), "test-guest".into())];
	history.record_step_id(Some(&format!("hot-{target}")), &[key], true, fixed + if wide { 1_000_000.0 } else { 1.0 }, &model.model_hash(), &cost);
}

#[test]
fn live_history_crosses_the_escalation_threshold_both_ways_including_inner_step_recording() {
	for expanding in [true, false] {
		let (fixture, model, _) = cost_fixture();
		let mut history = crate::history::History::load(&fixture.root).unwrap();
		for target in crate::registry::ARCHITECTURES {
			hot_cost(&model, &mut history, target, !expanding);
		}
		history.save(&fixture.root).unwrap();
		let (mut handoff, steps) = begin_inner(&model, effective_tree(&fixture.root).unwrap(), &tool_paths(), BTreeMap::new()).unwrap();
		assert!(!handoff.plan.full);
		let cold_before = handoff.plan.items.iter().filter(|item| item.key.check.starts_with("kernel.cold_")).count();
		assert_eq!(cold_before > 0, !expanding);
		for step in &steps {
			let seconds = if expanding { 1_000_000.0 } else { 100.0 };
			history.record_step_id(Some(&step.id), &step.keys, true, seconds, &model.model_hash(), &crate::history::CostModel::default());
		}
		// Other completed runs may also alter the shared cache; merge must read it live.
		for target in ["aarch64", "riscv64"] {
			hot_cost(&model, &mut history, target, expanding);
		}
		history.save(&fixture.root).unwrap();
		let outcomes = pass(&handoff.inner);
		finish_inner(&fixture.root, &mut handoff, outcomes).unwrap();
		fixture.commit();
		let mut state = prepare_merge(&model, handoff).unwrap();
		let original: Keys = state.handoff.inner.union(&state.handoff.deferred).cloned().collect();
		if expanding {
			assert!(!keys(&state.plan).difference(&original).collect::<Vec<_>>().is_empty());
		} else {
			assert!(!state.handoff.deferred.difference(&keys(&state.plan)).collect::<Vec<_>>().is_empty());
		}
		let steps = merge_commands(&model, &mut state).unwrap();
		let recorded: Keys = steps.iter().flat_map(|step| step.keys.iter().cloned()).collect();
		assert!(state.handoff.deferred.is_subset(&recorded), "cost contraction cannot erase D - P1");
		if expanding {
			assert!(!recorded.difference(&original).collect::<Vec<_>>().is_empty());
		}
		finish_merge(&fixture.root, &mut state, &pass(&recorded)).unwrap();
	}
}

#[test]
fn refreshed_absence_retires_deferred_and_preinventory_extra_variants_but_preserves_cost_contracted_work() {
	for from_extra in [false, true] {
		let (fixture, mut model, names) = cost_fixture();
		// Source already excludes this variant while the pre-inventory binary still carries it.
		let source = fs::read_to_string(fixture.root.join("src/kernel/cases.rs")).unwrap();
		fixture.write("src/kernel/cases.rs", &source.replace("tagged_test!(cold_0,", "#[cfg(not(target_arch = \"aarch64\"))]\ntagged_test!(cold_0,"));
		fixture.commit();
		fixture.write(TOOL, "fn main() { let changed_again = 2; }\n");
		let mut history = crate::history::History::load(&fixture.root).unwrap();
		hot_cost(&model, &mut history, "aarch64", !from_extra);
		hot_cost(&model, &mut history, "riscv64", true);
		history.save(&fixture.root).unwrap();
		let handoff = fixture.inner(&model, &tool_paths());
		let vanished = PlanItemKey { check: "kernel.cold_0".into(), architecture: "aarch64".into(), environment: Environment::TestGuest, configuration: "test".into() };
		assert_eq!(handoff.deferred.contains(&vanished), !from_extra);
		hot_cost(&model, &mut history, "aarch64", from_extra);
		hot_cost(&model, &mut history, "riscv64", false);
		history.save(&fixture.root).unwrap();
		fixture.commit();
		let mut state = prepare_merge(&model, handoff).unwrap();
		let p1 = keys(&state.plan);
		assert_eq!(p1.contains(&vanished), from_extra);
		let retained: Keys = state.handoff.deferred.difference(&p1).filter(|key| **key != vanished).cloned().collect();
		assert!(!retained.is_empty());
		fixture.inventory("aarch64", &names.iter().filter(|name| name.as_str() != "cold_0").map(String::as_str).collect::<Vec<_>>());
		fixture.refresh(&mut model);
		let steps = merge_commands(&model, &mut state).unwrap();
		let recorded: Keys = steps.iter().flat_map(|step| step.keys.iter().cloned()).collect();
		assert!(!recorded.contains(&vanished));
		assert!(retained.is_subset(&recorded), "present variants contracted out by history remain owed");
		assert!(state.retired.get(&vanished.display()).unwrap().contains("fresh aarch64 test inventory"));
		assert!(!steps.iter().any(|step| step.command.contains("--arch aarch64") && step.command.contains("kernel.cold_0,")), "unknown variant cannot reach a guest selection");
		finish_merge(&fixture.root, &mut state, &pass(&recorded)).unwrap();
	}
}

#[test]
fn malformed_kernel_configuration_is_not_misreported_as_a_retired_variant() {
	let (fixture, model, _) = cost_fixture();
	let handoff = fixture.inner(&model, &tool_paths());
	fixture.commit();
	let mut state = prepare_merge(&model, handoff).unwrap();
	let mut bad = state.handoff.plan.items.iter().find(|item| item.kind == CheckKind::KernelTest).unwrap().clone();
	bad.key.configuration = "misspelled".into();
	state.plan.items.push(bad);
	assert!(reconcile(&model, &state, &state.plan).unwrap_err().contains("unknown configuration"));
}

#[test]
fn different_parent_with_no_extra_keys_is_accepted_and_root_or_merge_commits_are_refused() {
	let fixture = Fixture::new();
	let model = fixture.model(&[("old", "kernel")]);
	let older_parent = fixture.commit();
	fixture.commit(); // A different parent, with the identical parent tree.
	fixture.write("docs/change.md", "change\n");
	let handoff = fixture.inner(&model, &["docs/change.md".into()]);
	fixture.commit();
	let tree = fixture.git(&["rev-parse", "HEAD^{tree}"]);
	let revision = fixture.git(&["commit-tree", &tree, "-p", &older_parent, "-m", "same selected delta"]);
	fixture.git(&["update-ref", "HEAD", &revision]);
	let mut state = prepare_merge(&model, handoff.clone()).unwrap();
	assert_eq!(keys(&state.plan), keys(&handoff.plan));
	assert!(merge_commands(&model, &mut state).unwrap().is_empty());
	finish_merge(&fixture.root, &mut state, &BTreeMap::new()).unwrap();
	let root_revision = fixture.git(&["commit-tree", &tree, "-m", "unsupported root"]);
	fixture.git(&["update-ref", "HEAD", &root_revision]);
	assert!(prepare_merge(&model, handoff.clone()).unwrap_err().contains("exactly one parent; found 0"));
	let merge_revision = fixture.git(&["commit-tree", &tree, "-p", &older_parent, "-p", &root_revision, "-m", "unsupported merge"]);
	fixture.git(&["update-ref", "HEAD", &merge_revision]);
	assert!(prepare_merge(&model, handoff).unwrap_err().contains("exactly one parent; found 2"));
}
