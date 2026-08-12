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

// The command that runs one host check in one configuration. `command` already has `{arch}`
// substituted; what this adds is the configuration's features.
//
// ONE LOWERING, because there were two. The shadow producer emitted `check.command` raw, so a
// `shared-image` variant was run as though it were the default one and its result was filed under
// an id that did not say which variant it was. A comparison between the run and the shadow is only
// a comparison if both sides lowered the key the same way, and the only way to be sure of that is
// for there to be one place that does it.
pub fn host_command(command: &str, configuration: &str) -> String {
	match configuration {
		"default" => command.to_string(),
		other => format!("{command} --no-default-features --features {other}"),
	}
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
		steps.push(Step { label: format!("host suite {crate_name} ({})", item.key.configuration), command: host_command(&item.command, &item.key.configuration), keys: vec![item.key.clone()], note: None });
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
		// A strict subset is handed over EXACTLY, by stable ID. The runner refuses an ID it does not
		// have, so a selection naming a renamed test fails loudly instead of quietly running less.
		//
		// Measured on an idle machine: 2 tests take 9 s, 20 take 12 s, 205 take 108 s - a fixed cost
		// of about eight seconds and roughly half a second per test. An earlier note in P02M0118 put the
		// fixed cost at ~100 s and concluded that selection was the smallest lever this milestone
		// had; that arithmetic mixed a run made under load 115 with one made idle, and it was wrong.
		// Selecting twenty tests out of two hundred is an 89% saving on the guest run.
		// Hand over an exact list only when it BUYS something.
		//
		// 195 of 205 selected produced a nine-kilobyte command line and saved a few seconds, because
		// the cost of a guest run is about eight seconds fixed plus half a second per test - the
		// saving is proportional to the tests dropped, and dropping ten of them is not worth an
		// environment variable nobody can read in a log. Below the threshold the list is worth it:
		// twenty tests run in 12 s against 108 s for all of them.
		let worth_selecting = total > 0 && selected.len() * 5 < total * 4;
		let (command, note) = if worth_selecting {
			// The check id VERBATIM. It used to be stripped of its `kernel.` prefix, which matched
			// the guest runner's identity only while that identity was `stringify!($name)`; the
			// runner now matches the declaration's namespaced `id`, and an id it cannot find is a
			// hard failure by design. The two strings have to be the same string.
			let ids: Vec<&str> = selected.iter().map(|key| key.check.as_str()).collect();
			(format!("TEST_SELECTION={} ./test.sh --arch {architecture}", ids.join(",")), Some(format!("{} of {total} tests, handed over by id", selected.len())))
		} else if selected.len() < total {
			(format!("./test.sh --arch {architecture}"), Some(format!("{} of {total} selected, close enough to all of them that handing over a list would cost more than it saves", selected.len())))
		} else {
			(format!("./test.sh --arch {architecture}"), None)
		};
		steps.push(Step { label: format!("kernel suite {architecture}"), command, keys: selected.clone(), note });
	}

	// Every booted architecture gets a guest step, whether or not the catalog has tests for it.
	//
	// Guest steps were derived purely from the kernel-test items in the plan, and a target whose test
	// binary could not be enumerated contributes none - so a plan could report
	// `booted: x86_64, aarch64, riscv64` and emit no aarch64 boot at all. The escalation that fires
	// on a missing enumeration made the plan FULL without making the step appear, which is a plan
	// claiming a target and a run silently skipping it. That is the whole failure mode.
	//
	// The added step carries no keys: nothing was enumerated, so there is nothing to record against,
	// and pretending otherwise would file history for tests the model cannot name.
	for architecture in &plan.architectures_booted {
		if kernel_by_arch.contains_key(architecture.as_str()) {
			continue;
		}
		steps.push(Step { label: format!("kernel suite {architecture} (unenumerated)"), command: format!("./test.sh --arch {architecture}"), keys: Vec::new(), note: Some(String::from("the model could not enumerate this target's tests, so the whole suite runs and nothing is recorded against individual keys")) });
	}

	// The development guest last: it needs an instance `just dev-up` left running, and qemu-run.sh
	// refuses to combine DEV_PROFILE with TEST, so it can never share a boot with the suite above.
	for item in plan.items.iter().filter(|item| item.kind == CheckKind::DevCheck) {
		steps.push(Step { label: format!("{} ({})", item.key.check, Environment::DevGuest.as_str()), command: item.command.clone(), keys: vec![item.key.clone()], note: Some(String::from("needs a running development instance: just dev-up")) });
	}

	steps
}
