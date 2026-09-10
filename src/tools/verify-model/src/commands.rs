// A plan is per KEY; a run is per COMMAND, and the two are not the same shape.
//
// Selected kernel tests share a QEMU boot; independently runnable builds, gates and conformance
// suites keep their own steps. The model lowers those commands so the shell only executes them.

use crate::catalog::{CheckKind, Environment};
use crate::plan::Plan;
use crate::registry::Configuration;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
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
	// HOW MANY GUESTS THIS STEP STARTS AT ONCE, which is the number `--jobs` is the answer for.
	//
	// Zero for everything that boots nothing and for a step whose guests are started SERIALLY: a
	// gate that boots one machine after another needs one slot however many boots it makes, and the
	// runner already puts every such step behind a barrier and runs it alone.
	//
	// It is not zero for a step whose whole subject is OVERLAP. `concurrent-selection` starts two
	// same-architecture suites at the same time, and the runner counted it as a host gate - so at
	// `--jobs 1` two guests ran on a machine whose one answer to "how many QEMUs may run" was one.
	// The count travels with the step so the scheduler can refuse to start it inside a budget that
	// cannot hold it, rather than the step deciding for itself.
	pub guests: usize,
}

// THE DEPENDENCY GRAPH, VALIDATED BEFORE ANYTHING WALKS IT.
//
// M4 asks for exactly three properties and the emitter checked none of them: ids are unique, every
// dependency names a step that exists, and the graph is acyclic. What the layering did instead was
// silently absorb all three failures - `layers.insert` overwrites a duplicate id, an unknown
// dependency reads as depth zero through `map_or(0, ..)`, and the relaxation runs a fixed number of
// passes and stops, so a cycle simply produces whatever it produced.
//
// Each one is a wrong ANSWER rather than a crash, which is why they have to be refused here. Two
// steps sharing an id merge two costs into one figure that describes neither and give the recorder
// one key for two runs. A dependency naming nothing is a step the runner will never see satisfied -
// it either waits for news that cannot come or, worse, treats the prerequisite as already met and
// runs a step before what it reads. A cycle has no valid order at all, so the emitted one is
// arbitrary and the plan's ordering claim is empty.
//
// Answered as a list rather than at the first fault: a plan with three bad edges should say so once.
pub fn validate(steps: &[Step]) -> Result<(), Vec<String>> {
	let mut faults: Vec<String> = Vec::new();
	let mut seen: BTreeSet<&str> = BTreeSet::new();
	for step in steps {
		if !seen.insert(step.id.as_str()) {
			faults.push(format!("two steps share the id '{}' - their costs and their recorded outcomes would merge into one", step.id));
		}
	}
	for step in steps {
		for required in &step.requires {
			if !seen.contains(required.as_str()) {
				faults.push(format!("step '{}' requires '{required}', which no step emits", step.id));
			}
		}
	}
	// A CYCLE, BY EXHAUSTING THE ORDER RATHER THAN BY COLOURING. Kahn's algorithm removes every step
	// whose prerequisites are already out; whatever is left when nothing more can be removed is
	// exactly the set that depends on itself. Missing edges were reported above and are ignored here
	// so one broken reference does not also read as a cycle.
	let known: BTreeSet<&str> = seen.clone();
	let mut remaining: Vec<(&str, Vec<&str>)> = steps.iter().map(|step| (step.id.as_str(), step.requires.iter().map(String::as_str).filter(|id| known.contains(id)).collect())).collect();
	let mut settled: BTreeSet<&str> = BTreeSet::new();
	loop {
		let before = settled.len();
		for (id, requires) in &remaining {
			if !settled.contains(id) && requires.iter().all(|need| settled.contains(need)) {
				settled.insert(id);
			}
		}
		if settled.len() == before {
			break;
		}
	}
	remaining.retain(|(id, _)| !settled.contains(id));
	if !remaining.is_empty() {
		let mut names: Vec<&str> = remaining.iter().map(|(id, _)| *id).collect();
		names.sort_unstable();
		faults.push(format!("these steps depend on each other and have no order: {}", names.join(", ")));
	}
	if faults.is_empty() { Ok(()) } else { Err(faults) }
}

// Reconsider readiness after every step: a cheap dependent becomes eligible as soon as its last
// prerequisite is emitted, even while expensive roots remain. Stable IDs break equal-cost ties.
pub fn order_by_cost(mut pending: Vec<Step>, cost: impl Fn(&Step) -> f64) -> Result<Vec<Step>, Vec<String>> {
	validate(&pending)?;
	let mut ordered = Vec::with_capacity(pending.len());
	let mut completed = BTreeSet::new();
	while !pending.is_empty() {
		let next = pending.iter().enumerate().filter(|(_, step)| step.requires.iter().all(|required| completed.contains(required))).min_by(|(_, left), (_, right)| cost(left).partial_cmp(&cost(right)).unwrap_or(std::cmp::Ordering::Equal).then(left.id.cmp(&right.id))).map(|(index, _)| index).expect("validated dependency graph has a ready step");
		let step = pending.remove(next);
		completed.insert(step.id.clone());
		ordered.push(step);
	}
	Ok(ordered)
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
	steps_for_items(&plan.items, kernel_tests_per_target, registry)
}

// THE RELEASE PLAN: every release-required key at this revision, lowered into steps by the same code
// an ordinary plan is lowered by, plus the three kinds of step an ordinary plan never contains.
//
//   PRODUCERS      the shipping images are BUILT WORK: one step per image row, before the gates
//                    that read the image, which name it as their prerequisite in the catalog
//   WHOLE SUITES   one `./test.sh --arch A` per target carrying the `suite.kernel` key; the guest
//                    runner's envelope discharges every kernel test key of that target, so the
//                    kernel tests are not steps of their own here
//   THE LIFECYCLE  one step brings the development guest up, runs the four development checks
//                    against it and tears it down; it carries the lifecycle key AND the four
//                    development keys, because the checks cannot run against a guest that a
//                    separate step has already torn down
//
// Everything else - builds, host suites, conformance suites, profile rows and the other gates -
// is lowered exactly as an ordinary plan is, so a release step has the same id, the same command
// and the same measured cost as the step a scoped run would have taken.
pub fn release_steps(catalog: &crate::catalog::Catalog, registry: &crate::registry::Registry) -> Vec<Step> {
	use crate::catalog::CheckClass;
	use crate::plan::{PlanItem, PlanItemKey};
	let item = |check: &crate::catalog::Check, variant: &crate::catalog::Variant| PlanItem { key: PlanItemKey { check: check.id.clone(), architecture: variant.architecture.clone(), environment: variant.environment.clone(), configuration: variant.configuration.clone() }, kind: check.kind, command: check.command.replace("{arch}", &variant.architecture), reason: String::from("release-required") };
	let mut items: Vec<PlanItem> = Vec::new();
	for check in catalog.checks.iter().filter(|check| check.release_required) {
		if matches!(check.class, CheckClass::Producer | CheckClass::WholeSuite | CheckClass::DevCheck) {
			continue;
		}
		for variant in &check.variants {
			items.push(item(check, variant));
		}
	}
	let mut steps = steps_for_items(&items, &BTreeMap::new(), registry);
	// The last build step of each target: what every producer and every guest of that target
	// cannot start before.
	let mut build_last: BTreeMap<String, String> = BTreeMap::new();
	for architecture in crate::registry::ARCHITECTURES {
		for part in crate::catalog::BUILD_PARTS {
			let id = scoped_id("build", architecture, &[part.to_string()]);
			if steps.iter().any(|step| step.id == id) {
				build_last.insert(architecture.to_string(), id);
			}
		}
	}
	let mut producer_ids: BTreeMap<String, String> = BTreeMap::new();
	for check in catalog.checks.iter().filter(|check| check.release_required && check.class == CheckClass::Producer && check.id.starts_with("image.")) {
		for variant in &check.variants {
			let id = scoped_id("producer", &variant.architecture, &[check.id.clone()]);
			producer_ids.insert(check.id.clone(), id.clone());
			steps.push(Step { id, requires: build_last.get(&variant.architecture).cloned().into_iter().collect(), label: format!("{} (image producer)", check.id), command: item(check, variant).command, keys: vec![item(check, variant).key], note: Some(String::from("built work: the image the gates of this run boot, published into the run as an immutable artifact")), guests: 0 });
		}
	}
	let mut suite_ids: Vec<String> = Vec::new();
	for check in catalog.checks.iter().filter(|check| check.release_required && check.class == CheckClass::WholeSuite) {
		for variant in &check.variants {
			let id = scoped_id("guest", &variant.architecture, &[String::from("all")]);
			suite_ids.push(id.clone());
			steps.push(Step { id, requires: build_last.get(&variant.architecture).cloned().into_iter().collect(), label: format!("kernel suite {} (whole)", variant.architecture), command: item(check, variant).command, keys: vec![item(check, variant).key], note: Some(String::from("the whole suite: the guest runner's envelope discharges every kernel test key of this target")), guests: 1 });
		}
	}
	if let Some(lifecycle) = catalog.checks.iter().find(|check| check.release_required && check.id == crate::catalog::DEV_LIFECYCLE_PRODUCER.0) {
		for variant in &lifecycle.variants {
			let mut keys = vec![item(lifecycle, variant).key];
			for check in catalog.checks.iter().filter(|check| check.release_required && check.class == CheckClass::DevCheck) {
				for dev_variant in &check.variants {
					keys.push(item(check, dev_variant).key);
				}
			}
			let mut requires: Vec<String> = build_last.get(&variant.architecture).cloned().into_iter().collect();
			requires.extend(lifecycle.prerequisites.iter().filter_map(|id| producer_ids.get(id).cloned()));
			steps.push(Step { id: scoped_id("dev", "lifecycle", &[]), requires, label: String::from("development lifecycle (image, boot, readiness, the development checks, teardown)"), command: item(lifecycle, variant).command, keys, note: Some(String::from("one self-contained step owns the development guest: the four development checks run against the instance it brought up and publish their own envelopes")), guests: 1 });
		}
	}
	// The catalog's prerequisite edges - a gate that reads an image requires the step that built
	// it - and the guest edges the appended steps introduced.
	for step in &mut steps {
		if let Some(key) = step.keys.first() {
			if let Some(check) = catalog.get(&key.check) {
				step.requires.extend(check.prerequisites.iter().filter_map(|id| producer_ids.get(id).cloned()));
			}
		}
		if step.id.starts_with("gate-after-guest") {
			step.requires.extend(suite_ids.iter().cloned());
		}
		step.requires.sort();
		step.requires.dedup();
	}
	steps
}

pub fn steps_for_items(items: &[crate::plan::PlanItem], kernel_tests_per_target: &BTreeMap<String, usize>, registry: &crate::registry::Registry) -> Vec<Step> {
	let mut steps = Vec::new();

	// Each independently runnable build part gets its own duration and budget decision. Preserve
	// build.sh's producer order within each target; Cargo reuses unchanged dependencies across calls.
	let mut build_ids: BTreeMap<String, String> = BTreeMap::new();
	let architectures: BTreeSet<&str> = items.iter().filter(|item| item.kind == CheckKind::Build).map(|item| item.key.architecture.as_str()).collect();
	for architecture in architectures {
		for part in crate::catalog::BUILD_PARTS {
			let Some(item) = items.iter().find(|item| item.kind == CheckKind::Build && item.key.architecture == architecture && item.key.check == format!("build.{part}")) else { continue };
			let id = scoped_id("build", architecture, &[part.to_string()]);
			steps.push(Step { id: id.clone(), requires: build_ids.get(architecture).cloned().into_iter().collect(), label: format!("build {architecture} {part}"), command: item.command.clone(), keys: vec![item.key.clone()], note: None, guests: 0 });
			build_ids.insert(architecture.to_string(), id);
		}
	}

	// Host suites, one per crate per configuration. The configuration is in the command because it
	// is in the key: for the sixteen crates declaring `shared-image`, the default configuration is
	// the one that never ships, and running only that one is what this model exists to stop.
	for item in items.iter().filter(|item| item.kind == CheckKind::HostSuite) {
		let crate_name = item.key.check.strip_prefix("host.").unwrap_or(&item.key.check);
		steps.push(Step { id: scoped_id("host", crate_name, &[item.key.configuration.clone()]), requires: Vec::new(), label: format!("host suite {crate_name} ({})", item.key.configuration), command: lower(CheckKind::HostSuite, &item.command, registry.configuration(&item.key.configuration).unwrap_or(&DEFAULT_CONFIGURATION)), keys: vec![item.key.clone()], note: None, guests: 0 });
	}

	// Gates each have a separate step. Gates that consume guest output also require that guest.
	//
	// `capability-trace` reads a log only a guest run produces and compares it against the kernel
	// binary the build just refreshed - so in one merged step, ordered before the guests, it cannot
	// pass on a clean tree. Measured twice on 2026-08-28; the second run had a trace confirmed green
	// minutes earlier and the gate failed inside the sweep anyway, because the sweep's own build
	// restaled it. A step whose prerequisite is invalidated by a step that runs before it is not a
	// scheduling preference, it is the graph missing.
	//
	// The rest stay in front, where they are cheap and catch things early.
	let gate_items: Vec<&crate::plan::PlanItem> = items.iter().filter(|item| item.kind == CheckKind::Gate).collect();
	let gate_name = |item: &crate::plan::PlanItem| item.key.check.strip_prefix("gate.").unwrap_or(&item.key.check).to_string();
	// A GATE THAT STARTS GUESTS AT THE SAME TIME GETS ITS OWN STEP, and says how many.
	//
	// Merged into the batch it would carry its count for every gate beside it, so a budget too small
	// to hold the overlap would skip a dozen cheap host gates with it. Split out, the scheduler can
	// price it, refuse it inside a `--jobs` that cannot hold it, and run everything else regardless -
	// which is the same shape `GATES_AFTER_A_GUEST` already has for a different reason.
	let concurrent: Vec<&crate::plan::PlanItem> = gate_items.iter().copied().filter(|item| crate::catalog::gate_concurrent_guests(&gate_name(item)) > 1).collect();
	let after_guest: Vec<&crate::plan::PlanItem> = gate_items.iter().copied().filter(|item| crate::catalog::GATES_AFTER_A_GUEST.contains(&gate_name(item).as_str())).collect();
	// A PROFILE ROW IS ITS OWN STEP, and this is where the catalog's split becomes a schedule.
	//
	// Giving each profile a catalog entry gave it a KEY; folding it into the batch below took away
	// the id and the duration that make the key useful, so twelve emulated profiles were one step
	// with one measured cost divided evenly among them. M3.6 asks for the opposite and the
	// definition of done says no cost derived from a merged step may survive - see
	// `catalog::PROFILE_ROW_GATES` (fixed 2026-09-02).
	let profile_rows: Vec<&crate::plan::PlanItem> = gate_items.iter().copied().filter(|item| crate::catalog::gate_is_profile_row(&gate_name(item))).collect();
	// AND A GATE THAT BOOTS ONE GUEST GETS ITS OWN STEP TOO (2026-09-03).
	//
	// The split above caught the gate that boots TWO and the sixteen profile rows, and left every
	// gate that boots exactly one in the batch of cheap host gates - `implementation-mutations`,
	// `qemu-virtio-iommu-x86_64`, `smp-core-cap` and five more. That step is emitted under
	// `STEPGUESTS 0`, so the one `--jobs` bound did not count them and a machine at its slot limit
	// started one more; and the batch has one cost for fifty members, so a budget could admit hours
	// of guest work against a host-key estimate and not one of them was ever separately timed.
	let booting: Vec<&crate::plan::PlanItem> = gate_items.iter().copied().filter(|item| crate::catalog::gate_boots_a_guest(&gate_name(item)) && !crate::catalog::gate_is_profile_row(&gate_name(item)) && crate::catalog::gate_concurrent_guests(&gate_name(item)) <= 1).collect();
	for item in booting.iter() {
		let name = gate_name(item);
		steps.push(Step { id: scoped_id("gate-guest", "host", &[name.clone()]), requires: Vec::new(), label: format!("{name} gate (boots a guest)"), command: format!("./check.sh --gate {name}"), keys: vec![item.key.clone()], note: Some(String::from("this gate boots a guest of its own: its own step, its own key, its own measured cost and its own guest slot")), guests: 1 });
	}
	let before_guest: Vec<&crate::plan::PlanItem> = gate_items.iter().copied().filter(|item| !crate::catalog::GATES_AFTER_A_GUEST.contains(&gate_name(item).as_str()) && crate::catalog::gate_concurrent_guests(&gate_name(item)) <= 1 && !crate::catalog::gate_is_profile_row(&gate_name(item)) && !crate::catalog::gate_boots_a_guest(&gate_name(item))).collect();
	for item in profile_rows.iter() {
		let name = gate_name(item);
		steps.push(Step { id: scoped_id("gate-profile", "host", &[name.clone()]), requires: Vec::new(), label: format!("{name} profile"), command: format!("./check.sh --gate {name}"), keys: vec![item.key.clone()], note: Some(String::from("one profile of a multi-profile gate: its own step, its own key, its own measured cost and its own guest slot")), guests: 1 });
	}
	for item in concurrent.iter() {
		let name = gate_name(item);
		let guests = crate::catalog::gate_concurrent_guests(&name);
		steps.push(Step { id: scoped_id("gate-concurrent", "host", &[name.clone()]), requires: Vec::new(), label: format!("{name} gate ({guests} guests at once)"), command: format!("./check.sh --gate {name}"), keys: vec![item.key.clone()], note: Some(format!("this gate's subject is overlap: it starts {guests} guests at the same time, so it needs that many of the runner's slots")), guests });
	}
	// ONE STEP PER GATE, BECAUSE ONE GATE IS ONE SEPARATELY SCHEDULABLE UNIT (corrected 2026-09-03).
	//
	// These were batched into a single comma-list `check.sh --gate a,b,c` on the reasoning that
	// `check.sh` takes a list. It does, and that is a property of the RUNNER rather than of the
	// work: `check.sh --gate <one>` is a command, so every member is independently runnable, and
	// merged they shared one `StepId`, one duration and one budget decision. M4 asks for every
	// separately schedulable unit to be separately timed and for the plan to be ordered
	// cheapest-first, and neither is expressible over a batch - the cheap ones cannot go first,
	// nothing measures any of them, and a budget admits all fifty or none.
	//
	// The split that already existed for profile rows and guest-booting gates is the same split;
	// what was left was the assumption that the cheap ones are too cheap to be worth an id.
	for item in before_guest.iter() {
		let name = gate_name(item);
		steps.push(Step { id: scoped_id("gate", "host", &[name.clone()]), requires: Vec::new(), label: format!("{name} gate"), command: format!("./check.sh --gate {name}"), keys: vec![item.key.clone()], note: None, guests: 0 });
	}
	let gates_after_guest: Vec<&crate::plan::PlanItem> = after_guest;
	let conformance_items: Vec<&crate::plan::PlanItem> = items.iter().filter(|item| item.kind == CheckKind::Conformance).collect();
	// AND ONE STEP PER CONFORMANCE SUITE, for the reason above and with the same shape:
	// `check.sh --conformance <one>` is a command, so a suite is a unit the scheduler can order,
	// time and admit on its own.
	for item in conformance_items.iter() {
		let name = item.key.check.strip_prefix("conformance.").unwrap_or(&item.key.check).to_string();
		steps.push(Step { id: scoped_id("conformance", "host", &[name.clone()]), requires: Vec::new(), label: format!("{name} conformance suite"), command: format!("./check.sh --conformance {name}"), keys: vec![item.key.clone()], note: None, guests: 0 });
	}

	// One boot per architecture, whatever the selection inside it.
	//
	// A target receives its exact selected IDs. The planner already accounts for the fixed boot
	// cost and decides whether selecting the whole target suite is cheaper.
	let mut kernel_by_arch: BTreeMap<&str, Vec<crate::plan::PlanItemKey>> = BTreeMap::new();
	for item in items.iter().filter(|item| item.kind == CheckKind::KernelTest) {
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
		steps.push(Step { id, requires: build_ids.get(*architecture).cloned().into_iter().collect(), label: format!("kernel suite {architecture}"), command, keys: selected.clone(), note, guests: 1 });
	}

	// The two guest steps that stand in for a target's tests, and they are steps like any other.
	//
	// This was a KEYLESS step invented here, for two different states at once, under a note claiming
	// the model could not enumerate the target - which was false whenever enumeration had worked and
	// the selection was merely empty. `record_step` returns on an empty key list, so the largest item
	// in a driver plan could never acquire a cost however many times it ran. Both states are catalog
	// checks now, chosen by the planner, so `--plan` shows them, the estimator can price them and the
	// recorder has a key to file against. What is left here is emitting what the plan decided.
	for item in items.iter().filter(|item| item.kind == CheckKind::GuestFallback) {
		let architecture = item.key.architecture.as_str();
		let (label, note) = if item.key.check == "guest.whole-suite" { (format!("kernel suite {architecture} (unenumerated)"), String::from("the model could not enumerate this target's tests, so the whole suite runs and is recorded against one aggregate key")) } else { (format!("boot check {architecture}"), String::from("this target is booted and no test selected it, so it runs a named boot check rather than everything or nothing")) };
		steps.push(Step { id: scoped_id("guest", architecture, &[item.key.check.clone()]), requires: build_ids.get(architecture).cloned().into_iter().collect(), label, command: item.command.clone(), keys: vec![item.key.clone()], note: Some(note), guests: 1 });
	}

	// The gates that read what a guest wrote, after the guests wrote it. Every guest step emitted
	// above is a prerequisite: which of them produced the log a given gate reads is the gate's own
	// business, and requiring all of them is the conservative answer rather than a guess.
	if !gates_after_guest.is_empty() {
		let names: Vec<String> = gates_after_guest.iter().map(|item| gate_name(item)).collect();
		let guest_ids: Vec<String> = steps.iter().filter(|step| step.id.starts_with("guest:")).map(|step| step.id.clone()).collect();
		steps.push(Step { id: scoped_id("gate-after-guest", "host", &names), requires: guest_ids, label: format!("{} gate(s) that read a guest run", names.len()), command: format!("./check.sh --gate {}", names.join(",")), keys: gates_after_guest.iter().map(|item| item.key.clone()).collect(), note: Some(String::from("these read a log a guest run wrote, so they cannot run before one")), guests: 0 });
	}

	// Development checks mutate the same persistent instance, so each waits for its predecessor.
	// They retain separate keys and timings while independent guest profiles may run in parallel.
	let mut previous_dev: Option<String> = None;
	for item in items.iter().filter(|item| item.kind == CheckKind::DevCheck) {
		let id = scoped_id("dev", &item.key.check, &[]);
		steps.push(Step { id: id.clone(), requires: previous_dev.into_iter().collect(), label: format!("{} ({})", item.key.check, Environment::DevGuest.as_str()), command: item.command.clone(), keys: vec![item.key.clone()], note: Some(String::from("needs a running development instance: ./dev.sh up")), guests: 1 });
		previous_dev = Some(id);
	}

	// Profile and development gates boot through scripts too. Their slot declarations must not
	// let them run before the selected artifacts exist merely because their command is check.sh.
	for step in &mut steps {
		if step.guests > 0 && !step.id.starts_with("guest:") {
			step.requires.extend(build_ids.values().cloned());
			step.requires.sort();
			step.requires.dedup();
		}
	}
	steps
}
