// A plan is per KEY; a run is per COMMAND, and the two are not the same shape.
//
// Two hundred selected kernel tests are one QEMU boot, nine selected builds are three invocations
// of build.sh, and eleven conformance suites are one call to check.sh. Collapsing them here rather
// than in the shell keeps `verify.sh` thin enough to be the thing that cannot break - it reads
// lines and runs them, and every decision that needed the model was already made.

use crate::catalog::{CheckKind, Environment};
use crate::plan::Plan;
use std::collections::{BTreeMap, BTreeSet};

pub struct Step {
	pub label: String,
	pub command: String,
	// The keys this one command discharges. Carried rather than counted, because a run has to be
	// RECORDED against them: the history, the age bound and the cost model all range over
	// PlanItemKeys, and a step that only knows how many it covered can update none of them.
	pub keys: Vec<crate::plan::PlanItemKey>,
	pub note: Option<String>,
}

pub fn steps(plan: &Plan, kernel_tests_per_target: &BTreeMap<String, usize>) -> Vec<Step> {
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
	for (architecture, parts) in &parts_by_arch {
		steps.push(Step { label: format!("build {architecture}"), command: format!("./build.sh --arch {architecture} --part {}", parts.iter().copied().collect::<Vec<_>>().join(",")), keys: build_keys.get(architecture).cloned().unwrap_or_default(), note: None });
	}

	// Host suites, one per crate per configuration. The configuration is in the command because it
	// is in the key: for the sixteen crates declaring `shared-image`, the default configuration is
	// the one that never ships, and running only that one is what this model exists to stop.
	for item in plan.items.iter().filter(|item| item.kind == CheckKind::HostSuite) {
		let crate_name = item.key.check.strip_prefix("host.").unwrap_or(&item.key.check);
		let command = match item.key.configuration.as_str() {
			"default" => item.command.clone(),
			other => format!("{} --no-default-features --features {other}", item.command),
		};
		steps.push(Step { label: format!("host suite {crate_name} ({})", item.key.configuration), command, keys: vec![item.key.clone()], note: None });
	}

	// Gates and conformance suites each collapse into one call, because check.sh takes a list.
	let gate_items: Vec<&crate::plan::PlanItem> = plan.items.iter().filter(|item| item.kind == CheckKind::Gate).collect();
	if !gate_items.is_empty() {
		let names: Vec<&str> = gate_items.iter().map(|item| item.key.check.strip_prefix("gate.").unwrap_or(&item.key.check)).collect();
		steps.push(Step { label: format!("{} gate(s)", names.len()), command: format!("./check.sh --gate {}", names.join(",")), keys: gate_items.iter().map(|item| item.key.clone()).collect(), note: None });
	}
	let conformance_items: Vec<&crate::plan::PlanItem> = plan.items.iter().filter(|item| item.kind == CheckKind::Conformance).collect();
	if !conformance_items.is_empty() {
		let names: Vec<&str> = conformance_items.iter().map(|item| item.key.check.strip_prefix("conformance.").unwrap_or(&item.key.check)).collect();
		steps.push(Step { label: format!("{} conformance suite(s)", names.len()), command: format!("./check.sh --conformance {}", names.join(",")), keys: conformance_items.iter().map(|item| item.key.clone()).collect(), note: None });
	}

	// One boot per architecture, whatever the selection inside it.
	//
	// The note is the honest part. The runner cannot yet be handed an exact test list, so a strict
	// subset is run as the whole suite - which over-runs rather than under-runs, and says so. This
	// is also where the milestone's own measurement lands: the fixed cost of a boot is ~100 s on
	// x86_64 against ~0.2 s per test, so selecting fewer tests inside a boot that is happening
	// anyway was never where the saving was.
	let mut kernel_by_arch: BTreeMap<&str, Vec<crate::plan::PlanItemKey>> = BTreeMap::new();
	for item in plan.items.iter().filter(|item| item.kind == CheckKind::KernelTest) {
		kernel_by_arch.entry(&item.key.architecture).or_default().push(item.key.clone());
	}
	for (architecture, selected) in &kernel_by_arch {
		let total = kernel_tests_per_target.get(*architecture).copied().unwrap_or(selected.len());
		let note = if selected.len() < total { Some(format!("{} of {total} tests are selected; the runner has no exact-selection mode yet, so the whole suite runs", selected.len())) } else { None };
		steps.push(Step { label: format!("kernel suite {architecture}"), command: format!("./test.sh --arch {architecture}"), keys: selected.clone(), note });
	}

	// The development guest last: it needs an instance `just dev-up` left running, and qemu-run.sh
	// refuses to combine DEV_PROFILE with TEST, so it can never share a boot with the suite above.
	for item in plan.items.iter().filter(|item| item.kind == CheckKind::DevCheck) {
		steps.push(Step { label: format!("{} ({})", item.key.check, Environment::DevGuest.as_str()), command: item.command.clone(), keys: vec![item.key.clone()], note: Some(String::from("needs a running development instance: just dev-up")) });
	}

	steps
}
