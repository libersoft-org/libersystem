// A plan is per KEY; a run is per COMMAND, and the two are not the same shape.
//
// Two hundred selected kernel tests are one QEMU boot, nine selected builds are three invocations
// of build.sh, and eleven conformance suites are one call to check.sh. Collapsing them here rather
// than in the shell keeps `verify.sh` thin enough to be the thing that cannot break - it reads
// lines and runs them, and every decision that needed the model was already made.

use crate::catalog::{CheckKind, Environment};
use crate::plan::Plan;
use crate::registry::Configuration;
use std::collections::{BTreeMap, BTreeSet};

pub struct Step {
	// WHEN TWO STEPS ARE THE SAME WORK. What schedules this one, what its dependencies are stated
	// in, and what its cost is keyed on - which is a different question from what it DISCHARGES, and
	// running the two together is why a merged gate step's per-key costs are an artefact of how the
	// gates happened to be batched.
	//
	// It contains the kind, the architecture or crate, the configuration, and what the step was
	// ASKED to do - selection ids, tags, or build parts. It contains no run identity: an id that
	// changed every run would have no history and every step would be priced as unseen.
	pub id: String,
	// THE STEPS THIS ONE CANNOT START BEFORE, by their ids.
	//
	// `Step` carried no dependencies at all, so "prerequisite-closed branch" was a phrase with
	// nothing behind it - and the ordering defect it left is not a preference: `capability-trace`
	// reads a log only a guest run produces, and every full path ran the checks BEFORE the tests, so
	// the gate could not pass however clean the tree was.
	//
	// Ids rather than indexes, because an index is a position in one emission and a dependency has
	// to survive being reordered - which is the next thing that happens to it.
	pub requires: Vec<String>,
	pub label: String,
	pub command: String,
	// The keys this one command discharges. Carried rather than counted, because a run has to be
	// RECORDED against them: the history, the age bound and the cost model all range over
	// PlanItemKeys, and a step that only knows how many it covered can update none of them.
	//
	// EMPTY IS LEGITIMATE FOR A PREREQUISITE AND FOR NOTHING ELSE. A step that runs tests and
	// discharges nothing is unmeasurable by construction - `record_step` returns on an empty key
	// list - so the two guest steps that stand in for a target's tests are catalog checks with keys
	// of their own rather than the keyless step this used to invent.
	pub keys: Vec<crate::plan::PlanItemKey>,
	pub note: Option<String>,
}

// A long list is digested rather than spelled. Two hundred selected ids make a nine-kilobyte name,
// and an identity has to be stable and distinct, not readable.
fn scoped_id(kind: &str, scope: &str, parts: &[String]) -> String {
	if parts.is_empty() {
		return format!("{kind}:{scope}");
	}
	let mut sorted: Vec<&str> = parts.iter().map(String::as_str).collect();
	sorted.sort_unstable();
	let joined = sorted.join("+");
	if joined.len() <= 96 {
		return format!("{kind}:{scope}:{joined}");
	}
	let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
	sha2::Digest::update(&mut hasher, joined.as_bytes());
	let digest = sha2::Digest::finalize(hasher);
	format!("{kind}:{scope}:{}:{:x}", sorted.len(), digest)
}

// The command that runs one check of `kind` in one configuration. `command` already has `{arch}`
// substituted; what this adds is whatever the CONFIGURATION RECORD says.
//
// ONE LOWERING, because there were two. The shadow producer emitted `check.command` raw, so a
// `shared-image` variant was run as though it were the default one and its result was filed under an
// id that did not say which variant it was. A comparison between the run and the shadow is only a
// comparison if both sides lowered the key the same way.
//
// FROM THE RECORD, NOT THE NAME, and that is the second half of the same lesson. The name-matching
// version treated everything that was not `"default"` as a Cargo feature list, which is right for
// exactly one of the four configurations: `shared-image` really does want
// `--no-default-features --features shared-image`, `default` is right by luck, and `test` and
// `development` both declare `default_features = true` and no features at all - so both halves of
// what it emitted contradicted the record.
//
// It was not a wrong flag. A dev check's command is `(cd src && harness/dev-selftest.py)`, so the
// producer emitted `(cd src && harness/dev-selftest.py) --no-default-features --features development` -
// a bash SYNTAX ERROR, which every dev-guest shadow line then failed on before it started, making
// clean `DevGuest` evidence unobtainable for `bin.dev_agent`, `bin.dev_channel`, `harness.boot` and
// `proto`. The shared lowering removed a divergence on the host path and created one on the dev path.
pub fn lower(kind: CheckKind, command: &str, configuration: &Configuration) -> String {
	match kind {
		// A cargo invocation is the only kind a feature selection means anything to. `default` is
		// the crate's own manifest, so it is spelled by saying nothing.
		CheckKind::HostSuite => {
			if configuration.default_features && configuration.features.is_empty() {
				return command.to_string();
			}
			let mut lowered = String::from(command);
			if !configuration.default_features {
				lowered.push_str(" --no-default-features");
			}
			if !configuration.features.is_empty() {
				lowered.push_str(" --features ");
				lowered.push_str(&configuration.features.join(","));
			}
			lowered
		}
		// Everything else carries its own command and means it: a gate is a script, a conformance
		// run is a script, a build takes `--arch` and `--part`, a dev check is a shell pipeline, and
		// a kernel test is selected by tag rather than by feature. Appending cargo flags to any of
		// them produces something that is not the command the runner runs - and for the dev check,
		// something that is not a command at all.
		CheckKind::Gate | CheckKind::Conformance | CheckKind::Build | CheckKind::DevCheck | CheckKind::KernelTest | CheckKind::GuestFallback => command.to_string(),
	}
}

// The fallback when a key names a configuration the registry does not define, which the model's own
// check refuses - a `default`-shaped record rather than a panic, so a malformed model produces a
// wrong command rather than no output at all.
static DEFAULT_CONFIGURATION: std::sync::LazyLock<Configuration> = std::sync::LazyLock::new(|| Configuration { name: String::from("default"), default_features: true, features: Vec::new(), profile: String::from("dev"), build_mode: String::from("host-test"), description: String::from("the crate's own manifest") });

pub fn steps(plan: &Plan, kernel_tests_per_target: &BTreeMap<String, usize>, registry: &crate::registry::Registry) -> Vec<Step> {
	let mut steps = Vec::new();

	// Builds first, and per architecture rather than per part: build.sh takes a part list, and
	// nine separate invocations would recompile shared dependencies nine times.
	let mut parts_by_arch: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
	let mut build_keys: BTreeMap<&str, Vec<crate::plan::PlanItemKey>> = BTreeMap::new();
	for item in plan.items.iter().filter(|item| item.kind == CheckKind::Build) {
		let part = item.key.check.strip_prefix("build.").unwrap_or(&item.key.check);
		parts_by_arch.entry(&item.key.architecture).or_default().insert(part);
		build_keys.entry(&item.key.architecture).or_default().push(item.key.clone());
	}
	// Kept so a guest step can name the build it cannot start before. A dependency by id survives
	// the reordering below; an index would not.
	let mut build_ids: BTreeMap<String, String> = BTreeMap::new();
	for (architecture, parts) in &parts_by_arch {
		let part_names: Vec<String> = parts.iter().map(|part| (*part).to_string()).collect();
		build_ids.insert((*architecture).to_string(), scoped_id("build", architecture, &part_names));
		steps.push(Step { id: scoped_id("build", architecture, &part_names), requires: Vec::new(), label: format!("build {architecture}"), command: format!("./build.sh --arch {architecture} --part {}", parts.iter().copied().collect::<Vec<_>>().join(",")), keys: build_keys.get(architecture).cloned().unwrap_or_default(), note: None });
	}

	// Host suites, one per crate per configuration. The configuration is in the command because it
	// is in the key: for the sixteen crates declaring `shared-image`, the default configuration is
	// the one that never ships, and running only that one is what this model exists to stop.
	for item in plan.items.iter().filter(|item| item.kind == CheckKind::HostSuite) {
		let crate_name = item.key.check.strip_prefix("host.").unwrap_or(&item.key.check);
		steps.push(Step { id: scoped_id("host", crate_name, &[item.key.configuration.clone()]), requires: Vec::new(), label: format!("host suite {crate_name} ({})", item.key.configuration), command: lower(CheckKind::HostSuite, &item.command, registry.configuration(&item.key.configuration).unwrap_or(&DEFAULT_CONFIGURATION)), keys: vec![item.key.clone()], note: None });
	}

	// Gates and conformance suites each collapse into one call, because check.sh takes a list.
	// TWO GATE STEPS, NOT ONE, AND THE SPLIT IS A DEPENDENCY RATHER THAN A PREFERENCE.
	//
	// `capability-trace` reads a log only a guest run produces and compares it against the kernel
	// binary the build just refreshed - so in one merged step, ordered before the guests, it cannot
	// pass on a clean tree. Measured twice on 2026-08-28; the second run had a trace confirmed green
	// minutes earlier and the gate failed inside the sweep anyway, because the sweep's own build
	// restaled it. A step whose prerequisite is invalidated by a step that runs before it is not a
	// scheduling preference, it is the graph missing.
	//
	// The rest stay in front, where they are cheap and catch things early.
	let gate_items: Vec<&crate::plan::PlanItem> = plan.items.iter().filter(|item| item.kind == CheckKind::Gate).collect();
	let gate_name = |item: &crate::plan::PlanItem| item.key.check.strip_prefix("gate.").unwrap_or(&item.key.check).to_string();
	let after_guest: Vec<&crate::plan::PlanItem> = gate_items.iter().copied().filter(|item| crate::catalog::GATES_AFTER_A_GUEST.contains(&gate_name(item).as_str())).collect();
	let before_guest: Vec<&crate::plan::PlanItem> = gate_items.iter().copied().filter(|item| !crate::catalog::GATES_AFTER_A_GUEST.contains(&gate_name(item).as_str())).collect();
	if !before_guest.is_empty() {
		let names: Vec<String> = before_guest.iter().map(|item| gate_name(item)).collect();
		steps.push(Step { id: scoped_id("gate", "host", &names), requires: Vec::new(), label: format!("{} gate(s)", names.len()), command: format!("./check.sh --gate {}", names.join(",")), keys: before_guest.iter().map(|item| item.key.clone()).collect(), note: None });
	}
	let gates_after_guest: Vec<&crate::plan::PlanItem> = after_guest;
	let conformance_items: Vec<&crate::plan::PlanItem> = plan.items.iter().filter(|item| item.kind == CheckKind::Conformance).collect();
	if !conformance_items.is_empty() {
		let names: Vec<&str> = conformance_items.iter().map(|item| item.key.check.strip_prefix("conformance.").unwrap_or(&item.key.check)).collect();
		let format_names: Vec<String> = names.iter().map(|name| (*name).to_string()).collect();
		steps.push(Step { id: scoped_id("conformance", "host", &format_names), requires: Vec::new(), label: format!("{} conformance suite(s)", names.len()), command: format!("./check.sh --conformance {}", names.join(",")), keys: conformance_items.iter().map(|item| item.key.clone()).collect(), note: None });
	}

	// One boot per architecture, whatever the selection inside it.
	//
	// The note is the honest part. The runner cannot yet be handed an exact test list, so a strict
	// subset is run as the whole suite - which over-runs rather than under-runs, and says so. This
	// is also where the milestone's own measurement lands: a boot has a fixed cost that dwarfs the
	// per-test one - see `CostModel::default` for the current figures - so selecting fewer tests
	// inside a boot that is happening anyway was never where the saving was.
	let mut kernel_by_arch: BTreeMap<&str, Vec<crate::plan::PlanItemKey>> = BTreeMap::new();
	for item in plan.items.iter().filter(|item| item.kind == CheckKind::KernelTest) {
		kernel_by_arch.entry(&item.key.architecture).or_default().push(item.key.clone());
	}
	for (architecture, selected) in &kernel_by_arch {
		let total = kernel_tests_per_target.get(*architecture).copied().unwrap_or(selected.len());
		// A strict subset is handed over EXACTLY, by stable ID. The runner refuses an ID it does not
		// have, so a selection naming a renamed test fails loudly instead of quietly running less.
		//
		// AND WHETHER TO SUBSET IS NOT DECIDED HERE ANY MORE. This used to run the WHOLE suite when
		// the selection was within a fifth of it, while attaching only the SELECTED keys - so the
		// plan said 195 keys and the run did 205, and ten tests ran unrecorded. That is a widening,
		// the planner makes widenings, and it makes this one now: what arrives here is either a
		// subset worth handing over or the whole thing, and this emits what it was given.
		//
		// Measured on an idle machine: 2 tests take 9 s, 20 take 12 s, 205 take 108 s - a fixed cost
		// of about eight seconds and roughly half a second per test. The nine-run calibration in
		// `CostModel::default` is what the model uses.
		let ids: Vec<String> = selected.iter().map(|key| key.check.clone()).collect();
		let (id, command, note) = if selected.len() < total {
			// The check id VERBATIM. It used to be stripped of its `kernel.` prefix, which matched
			// the guest runner's identity only while that identity was `stringify!($name)`; the
			// runner now matches the declaration's namespaced `id`, and an id it cannot find is a
			// hard failure by design. The two strings have to be the same string.
			(scoped_id("guest", architecture, &ids), format!("TEST_SELECTION={} ./test.sh --arch {architecture}", ids.join(",")), Some(format!("{} of {total} tests, handed over by id", selected.len())))
		} else {
			(scoped_id("guest", architecture, &[String::from("all")]), format!("./test.sh --arch {architecture}"), None)
		};
		steps.push(Step { id, requires: build_ids.get(*architecture).cloned().into_iter().collect(), label: format!("kernel suite {architecture}"), command, keys: selected.clone(), note });
	}

	// The two guest steps that stand in for a target's tests, and they are steps like any other.
	//
	// This was a KEYLESS step invented here, for two different states at once, under a note claiming
	// the model could not enumerate the target - which was false whenever enumeration had worked and
	// the selection was merely empty. `record_step` returns on an empty key list, so the largest item
	// in a driver plan could never acquire a cost however many times it ran. Both states are catalog
	// checks now, chosen by the planner, so `--plan` shows them, the estimator can price them and the
	// recorder has a key to file against. What is left here is emitting what the plan decided.
	for item in plan.items.iter().filter(|item| item.kind == CheckKind::GuestFallback) {
		let architecture = item.key.architecture.as_str();
		let (label, note) = if item.key.check == "guest.whole-suite" { (format!("kernel suite {architecture} (unenumerated)"), String::from("the model could not enumerate this target's tests, so the whole suite runs and is recorded against one aggregate key")) } else { (format!("boot check {architecture}"), String::from("this target is booted and no test selected it, so it runs a named boot check rather than everything or nothing")) };
		steps.push(Step { id: scoped_id("guest", architecture, &[item.key.check.clone()]), requires: build_ids.get(architecture).cloned().into_iter().collect(), label, command: item.command.clone(), keys: vec![item.key.clone()], note: Some(note) });
	}

	// The gates that read what a guest wrote, after the guests wrote it. Every guest step emitted
	// above is a prerequisite: which of them produced the log a given gate reads is the gate's own
	// business, and requiring all of them is the conservative answer rather than a guess.
	if !gates_after_guest.is_empty() {
		let names: Vec<String> = gates_after_guest.iter().map(|item| gate_name(item)).collect();
		let guest_ids: Vec<String> = steps.iter().filter(|step| step.id.starts_with("guest:")).map(|step| step.id.clone()).collect();
		steps.push(Step { id: scoped_id("gate-after-guest", "host", &names), requires: guest_ids, label: format!("{} gate(s) that read a guest run", names.len()), command: format!("./check.sh --gate {}", names.join(",")), keys: gates_after_guest.iter().map(|item| item.key.clone()).collect(), note: Some(String::from("these read a log a guest run wrote, so they cannot run before one")) });
	}

	// The development guest last: it needs an instance `./dev.sh up` left running, and qemu-run.sh
	// refuses to combine DEV_PROFILE with TEST, so it can never share a boot with the suite above.
	for item in plan.items.iter().filter(|item| item.kind == CheckKind::DevCheck) {
		steps.push(Step { id: scoped_id("dev", &item.key.check, &[]), requires: Vec::new(), label: format!("{} ({})", item.key.check, Environment::DevGuest.as_str()), command: item.command.clone(), keys: vec![item.key.clone()], note: Some(String::from("needs a running development instance: ./dev.sh up")) });
	}

	steps
}
