// The planner, as a separate process.
//
// That separation is the point rather than an implementation detail. "The selector crashes,
// therefore select everything" cannot be implemented INSIDE the selector: a planner that panics is
// in no position to choose its own fallback. `verify.sh` runs this, and on any non-zero exit, empty
// output or unparseable output falls through to a canonical FULL path written so plainly it cannot
// itself break. The absolute rule is narrower than "fall back": a planner failure must never
// produce a green exit status.

use std::collections::BTreeSet;
use std::env;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use verify_model::Model;
use verify_model::catalog::CheckKind;
use verify_model::plan::{Plan, Planner};

const USAGE: &str = "\
usage: verify-model <command> [options]

  plan [--paths P[,P...]] [--stdin]   the plan for a change (default command)
  commands [--paths ...]              the plan collapsed into the commands that run it
  catalog                             every check and the variants it has
  graph [--component NAME]            the component graph, or one component's edges
  owner PATH...                       which component owns each path, and why
  check                               the model's own gates: ownership, graph and catalog
  shadow --guest-log F --arch A       compare a full sweep against what a change would have scoped
  level --stdin                       what a scoped answer about this change is worth
  trust [--grant COMPONENT]           what is TRUSTED under the current model, and why not
  changes [--range A..B]              what changed, both sides of every rename, one path per line
  age                                 which keys have not run inside the window
  record --keys-file F [--failed]     record a step's outcome against the keys it discharged
  model-hash                          the hash TRUSTED evidence is bound to

  --json                              machine-readable output
  --explain                           say why every item is in the plan
  --quiet                             the plan's commands only

Paths are repository-relative. `plan` with no paths plans a FULL verification, which is also what
every failure mode here resolves to.";

fn main() -> ExitCode {
	match run() {
		Ok(code) => code,
		Err(error) => {
			eprintln!("verify-model: {error}");
			// Non-zero, always. The outer shell reads this as "plan unavailable" and escalates; an
			// exit 0 with no plan would read as "nothing to do", which is the false green this
			// whole mechanism exists to prevent.
			ExitCode::from(2)
		}
	}
}

fn run() -> Result<ExitCode, String> {
	let arguments: Vec<String> = env::args().skip(1).collect();
	let mut command = String::from("plan");
	let mut paths: Vec<String> = Vec::new();
	let mut component: Option<String> = None;
	let mut json = false;
	let mut explain = false;
	let mut quiet = false;
	let mut from_stdin = false;
	let mut keys_file: Option<String> = None;
	let mut guest_log: Option<String> = None;
	let mut architecture: Option<String> = None;
	let mut grant: Option<String> = None;
	let mut range: Option<String> = None;
	let mut passed = true;
	let mut seconds = 0.0f64;
	let mut positional: Vec<String> = Vec::new();

	let mut index = 0;
	while index < arguments.len() {
		let argument = arguments[index].as_str();
		match argument {
			"-h" | "--help" => {
				println!("{USAGE}");
				return Ok(ExitCode::SUCCESS);
			}
			"--json" => json = true,
			"--explain" => explain = true,
			"--quiet" => quiet = true,
			"--stdin" => from_stdin = true,
			"--failed" => passed = false,
			// Accepted and redundant on purpose: the runner spells the outcome at the call site
			// either way, so a step's result is readable there rather than implied by an absence.
			"--passed" => passed = true,
			"--guest-log" => {
				index += 1;
				guest_log = Some(arguments.get(index).ok_or("--guest-log needs a path")?.clone());
			}
			"--arch" => {
				index += 1;
				architecture = Some(arguments.get(index).ok_or("--arch needs a value")?.clone());
			}
			"--range" => {
				index += 1;
				range = Some(arguments.get(index).ok_or("--range needs a value like A..B")?.clone());
			}
			"--grant" => {
				index += 1;
				grant = Some(arguments.get(index).ok_or("--grant needs a component")?.clone());
			}
			"--keys-file" => {
				index += 1;
				keys_file = Some(arguments.get(index).ok_or("--keys-file needs a path")?.clone());
			}
			"--seconds" => {
				index += 1;
				seconds = arguments.get(index).ok_or("--seconds needs a number")?.parse().map_err(|_| "--seconds takes a number")?;
			}
			"--paths" => {
				index += 1;
				let value = arguments.get(index).ok_or("--paths needs a value")?;
				paths.extend(value.split(',').map(str::trim).filter(|path| !path.is_empty()).map(str::to_string));
			}
			"--component" => {
				index += 1;
				component = Some(arguments.get(index).ok_or("--component needs a value")?.clone());
			}
			_ if argument.starts_with('-') => return Err(format!("unknown option '{argument}'")),
			_ => positional.push(argument.to_string()),
		}
		index += 1;
	}
	if let Some(first) = positional.first()
		&& matches!(first.as_str(), "plan" | "commands" | "host-suites" | "booted" | "changes" | "age" | "record" | "reach" | "level" | "source-digest" | "volume-sources" | "shadow" | "trust" | "catalog" | "graph" | "owner" | "check" | "model-hash")
	{
		command = positional.remove(0);
	}
	if from_stdin {
		let mut text = String::new();
		io::stdin().read_to_string(&mut text).map_err(|error| format!("stdin: {error}"))?;
		paths.extend(text.lines().map(str::trim).filter(|path| !path.is_empty()).map(str::to_string));
	}

	let repo_root = find_repo_root()?;
	let model = Model::load(&repo_root)?;

	match command.as_str() {
		// The digest a shadow comparison is pinned to: HEAD plus the bytes of every dirty file.
		// Separate from the model hash because they answer different questions - the model hash asks
		// "would the selector decide differently", and this asks "is this still the same system".
		// The directories whose content can end up in the system volume, DERIVED.
		//
		// `lib.sh` carries this as a hand-written list and it was wrong: CoreServices statically
		// links `src/term` and the list did not contain it, so editing the terminal stack could
		// compile a new CoreServices, skip packaging, and leave `test.sh`'s staleness check content
		// that the volume was fresh - a guest booting the previous userspace and passing.
		"volume-sources" => {
			for directory in volume_sources(&model) {
				println!("{directory}");
			}
			Ok(ExitCode::SUCCESS)
		}
		"source-digest" => {
			println!("{}", verify_model::shadow::source_digest(&repo_root)?);
			Ok(ExitCode::SUCCESS)
		}
		"model-hash" => {
			println!("{}", model.model_hash());
			Ok(ExitCode::SUCCESS)
		}
		"catalog" => {
			if json {
				println!("{}", serde_json::to_string_pretty(&model.catalog).map_err(|error| error.to_string())?);
			} else {
				for check in &model.catalog.checks {
					let covers = if check.covers.is_empty() { String::from("(undeclared)") } else { check.covers.join(", ") };
					println!("{:<44} {:?}  covers {covers}", check.id, check.kind);
					for variant in &check.variants {
						println!("    {} / {} / {}", variant.architecture, variant.environment.as_str(), variant.configuration);
					}
				}
				println!();
				println!("{} checks, {} runnable keys", model.catalog.checks.len(), model.catalog.checks.iter().map(|check| check.variants.len()).sum::<usize>());
			}
			Ok(ExitCode::SUCCESS)
		}
		"graph" => {
			match component {
				Some(name) => {
					for edge in model.graph.edges_from(&name) {
						println!("{} -{}-> {}   ({})", edge.from, edge.kind, edge.to, edge.reason);
					}
					let seeds: BTreeSet<String> = [name.clone()].into_iter().collect();
					let affected = model.graph.affected(&seeds);
					println!();
					println!("changing {name} affects {} components: {}", affected.len(), affected.iter().cloned().collect::<Vec<_>>().join(", "));
				}
				None => {
					if json {
						println!("{}", serde_json::to_string_pretty(&model.graph.edges).map_err(|error| error.to_string())?);
					} else {
						for edge in &model.graph.edges {
							println!("{} -{}-> {}", edge.from, edge.kind, edge.to);
						}
						println!();
						println!("{} components, {} edges", model.graph.components.len(), model.graph.edges.len());
					}
				}
			}
			Ok(ExitCode::SUCCESS)
		}
		"owner" => {
			let ownership = model.ownership();
			for path in positional.iter().chain(paths.iter()) {
				println!("{path}: {:?}", ownership.owner(path));
			}
			Ok(ExitCode::SUCCESS)
		}
		"check" => self_check(&model),
		// What each kernel test can reach, one line per test. Written for the person annotating
		// `covers`: a declaration has to be something the test can back up, and this is the set it
		// has to choose from rather than guess at.
		"reach" => {
			for test in &model.kernel_tests.tests {
				let touched = model.kernel_tests.touches.get(&test.name).cloned().unwrap_or_default();
				let mut reachable: BTreeSet<String> = BTreeSet::new();
				for component in &touched {
					if model.graph.contains(component) {
						reachable.extend(model.graph.reaches(component, &verify_model::kerneltests::RUNTIME_REACH));
					}
				}
				println!("{}\t{}", test.name, reachable.into_iter().collect::<Vec<_>>().join(","));
			}
			Ok(ExitCode::SUCCESS)
		}
		// The one diff parser. `verify.sh` and the regression corpus both come through here, which
		// is the point: they used to parse a change two different ways, and the corpus therefore
		// could not catch the defect in the way production did it.
		"changes" => {
			let changes = match &range {
				Some(range) => verify_model::changes::range(&repo_root, range)?,
				None => verify_model::changes::working_tree(&repo_root)?,
			};
			for path in verify_model::changes::paths(&changes) {
				println!("{path}");
			}
			Ok(ExitCode::SUCCESS)
		}
		// Which targets a change must be booted on, one per line. A separate command because the
		// shell should not be grepping architecture names out of JSON: every decision that needs the
		// model belongs in the model, and a fragile parse in `verify.sh` is a decision made twice.
		"booted" => {
			let ownership = model.ownership();
			let planner = Planner::for_model(&model, &ownership);
			let plan = if paths.is_empty() { planner.full_plan(Vec::new(), BTreeSet::new(), BTreeSet::new(), vec![String::from("no changed paths were given")], Vec::new()) } else { planner.plan(&paths) };
			for architecture in &plan.architectures_booted {
				println!("{architecture}");
			}
			Ok(ExitCode::SUCCESS)
		}
		// DRY shadow: the scoped set is COMPUTED and never run; the full sweep that already
		// happened is what it is compared against. One boot, two answers.
		"shadow" => {
			let guest_log = guest_log.ok_or("shadow needs --guest-log")?;
			let architecture = architecture.ok_or("shadow needs --arch")?;
			if paths.is_empty() {
				return Err(String::from("shadow needs the change it is validating (--paths or --stdin); comparing against nothing proves nothing"));
			}
			let text = std::fs::read_to_string(&guest_log).map_err(|error| format!("{guest_log}: {error}"))?;
			let results = verify_model::shadow::parse_guest_log(&text);
			let ownership = model.ownership();
			let planner = Planner::for_model(&model, &ownership);
			let plan = planner.plan(&paths);
			let selected: Vec<verify_model::plan::PlanItemKey> = plan.items.iter().map(|item| item.key.clone()).collect();
			let history = verify_model::history::History::load(&repo_root)?;
			let comparison = verify_model::shadow::compare(&selected, &results, &architecture, &history);

			println!("shadow ({architecture}): {:?}", comparison.verdict);
			println!("  {}", comparison.reason);
			println!("  {} test(s) ran; {} of them were in the scoped selection", comparison.ran, comparison.scoped);
			for key in &comparison.inside_failures {
				println!("  FAILED (selected): {key}");
			}
			for key in &comparison.outside_failures {
				let flake = if comparison.previously_failed.contains(key) { "  <- has failed before; a flake candidate, not yet a missed edge" } else { "" };
				println!("  FAILED (not selected): {key}{flake}");
			}
			if comparison.verdict == verify_model::shadow::Verdict::CandidateMiss {
				println!();
				println!("A candidate is not a finding. Before charging this to the selector:");
				println!("  1. re-run the failing test on HEAD - this tree has one on record that failed three times and then passed twice with no change;");
				println!("  2. if it fails again, run it on the BASE revision;");
				println!("  3. only a reproducible HEAD-ONLY failure is a missed edge.");
			}

			// Filed with what it compared. A record that cannot say which tree and which model it
			// judged is a record that will be believed about a different system later.
			let mut log = verify_model::shadow::Log::load(&repo_root);
			log.schema = 1;
			log.records.push(verify_model::shadow::Record { universe: verify_model::shadow::Universe::TestGuest, architecture: architecture.clone(), verdict: format!("{:?}", comparison.verdict), reason: comparison.reason.clone(), model_hash: model.model_hash(), source_digest: verify_model::shadow::source_digest(&repo_root)?, changed_components: plan.changed_components.clone(), outside_failures: comparison.outside_failures.clone(), at: verify_model::history::now() });
			log.save(&repo_root)?;
			// Only Consistent is green, and the other three are green in different wrong ways.
			// CandidateMiss is the selector's problem; SelectionFailed is the code's and the sweep
			// found it; Void means the comparison judged nothing at all. Returning 0 for the last
			// two let `verify.sh` print "no evidence against the selection" over a run that had
			// produced no evidence either way, which is the exact shape of false green.
			Ok(match comparison.verdict {
				verify_model::shadow::Verdict::Consistent => ExitCode::SUCCESS,
				_ => ExitCode::FAILURE,
			})
		}
		// The verification LEVEL of a change: what the scoped answer about it is worth.
		//
		// `SHADOW` means no evidence has been gathered that a scoped answer about this component can
		// be trusted, so a scoped green is good evidence and not a substitute for a full run. That
		// distinction existed only in prose - `verify.sh` computed a plan and executed it whatever
		// the level - and this is what makes it consultable.
		"level" => {
			if paths.is_empty() {
				return Err(String::from("level needs the change it is judging (--paths or --stdin)"));
			}
			let ownership = model.ownership();
			let planner = Planner::for_model(&model, &ownership);
			let plan = planner.plan(&paths);
			let hash = model.model_hash();
			let mut store = verify_model::trust::Store::load(&repo_root);
			store.prune(&hash);
			// Trusted EVERYWHERE, not merely in the universe that happens to have evidence: a clean
			// record on the kernel suite says nothing about whether a host suite or a gate would have
			// been selected, and shadow only ever examined the kernel suite until now.
			let untrusted: Vec<&String> = plan.changed_components.iter().filter(|component| !store.trusted_everywhere(component, &hash)).collect();
			// The age BOUND, which is the part that makes it a bound rather than a report.
			//
			// A key past its window has not been exercised within the period this tree is willing to
			// go without exercising it, and a scoped run that does not cover it cannot be the only
			// green standing behind the change. Reporting alone left `age(key) <= N` as an aspiration
			// - a key could be two hundred days stale and the runner would keep mentioning it.
			let history = verify_model::history::History::load(&repo_root)?;
			let window: u64 = std::env::var("VERIFY_AGE_DAYS").ok().and_then(|value| value.parse().ok()).unwrap_or(30);
			let selected: std::collections::BTreeSet<String> = plan.items.iter().map(|item| item.key.display()).collect();
			let overdue = history.stale(&universe(&model), window).into_iter().filter(|(key, _)| !selected.contains(key)).count();

			if plan.full {
				println!("FULL");
			} else if overdue > 0 {
				println!("STALE\t{overdue}");
			} else if untrusted.is_empty() {
				println!("TRUSTED");
			} else {
				println!("SHADOW\t{}", untrusted.iter().map(|component| component.as_str()).collect::<Vec<_>>().join(","));
			}
			Ok(ExitCode::SUCCESS)
		}
		"trust" => {
			let hash = model.model_hash();
			let mut store = verify_model::trust::Store::load(&repo_root);
			let dropped = store.prune(&hash);
			for component in &dropped {
				println!("demoted {component} to SHADOW: the model hash moved, so the evidence it held no longer describes what runs");
			}
			let log = verify_model::shadow::Log::load(&repo_root);
			if let Some(component) = grant {
				// Granted per universe. A component with kernel-suite evidence and no host evidence is
				// trusted for one and not the other, which is the honest reading of what was proved.
				let mut granted = 0usize;
				for universe in [verify_model::shadow::Universe::Host, verify_model::shadow::Universe::TestGuest, verify_model::shadow::Universe::DevGuest] {
					match store.evaluate(&component, &hash, universe, &log) {
						Ok((clean, architectures)) => {
							store.grant(&component, &hash, universe, clean, architectures, verify_model::history::now());
							println!("{component} is TRUSTED for {universe:?} under model {hash}");
							granted += 1;
						}
						Err(reason) => println!("{component} stays in SHADOW for {universe:?}: {reason}"),
					}
				}
				if granted == 0 {
					store.save(&repo_root)?;
					return Ok(ExitCode::FAILURE);
				}
			}
			store.save(&repo_root)?;
			println!("model {hash}");
			if store.certificates.is_empty() {
				println!("nothing is TRUSTED yet - every component's scoped answers are validated against a full sweep before they are believed");
			}
			for (component, level) in store.summary(&hash) {
				println!("  {component}: {level:?}");
			}
			Ok(ExitCode::SUCCESS)
		}
		// What has run and what has gone stale, over the keys the catalog says exist.
		"age" => {
			let history = verify_model::history::History::load(&repo_root)?;
			let universe = universe(&model);
			let window: u64 = std::env::var("VERIFY_AGE_DAYS").ok().and_then(|value| value.parse().ok()).unwrap_or(30);
			let stale = history.stale(&universe, window);
			let never = stale.iter().filter(|(_, age)| age.is_none()).count();
			println!("{} of {} keys are stale against a {window}-day window ({never} have never run)", stale.len(), universe.len());
			for (key, age) in stale.iter().take(40) {
				match age {
					Some(days) => println!("  {key}  ({days} days)"),
					None => println!("  {key}  (never)"),
				}
			}
			if stale.len() > 40 {
				println!("  ... and {} more", stale.len() - 40);
			}
			let orphans = history.orphans(&universe);
			if !orphans.is_empty() {
				println!();
				println!("{} history entries name keys the catalog no longer has - usually a rename:", orphans.len());
				for key in orphans.iter().take(10) {
					println!("  {key}");
				}
			}
			Ok(ExitCode::SUCCESS)
		}
		// Record a step's outcome against every key it discharged.
		"record" => {
			let keys_file = keys_file.ok_or("record needs --keys-file")?;
			let text = std::fs::read_to_string(&keys_file).map_err(|error| format!("{keys_file}: {error}"))?;
			let mut history = verify_model::history::History::load(&repo_root)?;
			let hash = model.model_hash();
			// The keys come back as display strings, and the cost decomposition needs their parts -
			// so they are matched against the catalog's universe rather than re-parsed. A line that
			// matches nothing is a key the catalog no longer has, and it is dropped rather than
			// invented: history over keys that do not exist is what the orphan report is for.
			let universe = universe(&model);
			let by_display: std::collections::BTreeMap<String, verify_model::plan::PlanItemKey> = universe.into_iter().map(|key| (key.display(), key)).collect();
			let mut keys = Vec::new();
			let mut unknown = 0usize;
			for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
				match by_display.get(line) {
					Some(key) => keys.push(key.clone()),
					None => unknown += 1,
				}
			}
			history.record_step(&keys, passed, seconds, &hash, &verify_model::history::CostModel::default());
			history.save(&repo_root)?;
			eprintln!("verify-model: recorded {} key(s) as {}{}", keys.len(), if passed { "passed" } else { "failed" }, if unknown > 0 { format!(", {unknown} not in the catalog") } else { String::new() });
			Ok(ExitCode::SUCCESS)
		}
		// Every host suite the catalog knows about, for the gate that runs them. Derived, so the
		// inventory cannot drift: the hand-written list this replaces was nine crates into a job of
		// fifty-eight, and nothing said so.
		"host-suites" => {
			for check in model.catalog.checks.iter().filter(|check| check.kind == verify_model::catalog::CheckKind::HostSuite) {
				let name = check.id.strip_prefix("host.").unwrap_or(&check.id);
				let Some(entry) = model.crates.iter().find(|entry| entry.name == name) else { continue };
				for variant in &check.variants {
					println!("{}\t{}\t{}", entry.dir, name, variant.configuration);
				}
			}
			Ok(ExitCode::SUCCESS)
		}
		// The runner's interface, and deliberately a dumb one: `STATUS`, then one tab-separated
		// line per step. `verify.sh` must be simple enough to be the thing that cannot break, so
		// every decision that needed the model is already made by the time it reads this.
		"commands" => {
			let ownership = model.ownership();
			let planner = Planner::for_model(&model, &ownership);
			let plan = if paths.is_empty() { planner.full_plan(Vec::new(), BTreeSet::new(), BTreeSet::new(), vec![String::from("no changed paths were given")], Vec::new()) } else { planner.plan(&paths) };
			let mut per_target: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
			for test in &model.kernel_tests.tests {
				for architecture in &test.architectures {
					*per_target.entry(architecture.clone()).or_default() += 1;
				}
			}
			if plan.nothing_to_do {
				println!("STATUS\tnothing-to-do\tevery changed path is declared not code");
				return Ok(ExitCode::SUCCESS);
			}
			println!("STATUS\t{}\t{}", if plan.full { "full" } else { "scoped" }, plan.full_reasons.first().cloned().unwrap_or_else(|| format!("{} component(s) reaching {}", plan.changed_components.len(), plan.affected_components.len())));
			// One STEP line per command, then one KEY line per PlanItemKey it discharges. The keys
			// are emitted separately rather than crammed into the STEP line because a kernel-suite
			// step carries two hundred of them, and a line the runner has to split on two different
			// separators is a line somebody will parse wrong.
			for (index, step) in verify_model::commands::steps(&plan, &per_target).into_iter().enumerate() {
				println!("STEP\t{}\t{}\t{}\t{}\t{}", index, step.keys.len(), step.label, step.command, step.note.unwrap_or_default());
				for key in &step.keys {
					println!("KEY\t{index}\t{}", key.display());
				}
			}
			Ok(ExitCode::SUCCESS)
		}
		"plan" => {
			// No paths at all is not "nothing changed" - it is "nobody said what changed", and the
			// only safe reading of that is everything.
			let ownership = model.ownership();
			let planner = Planner::for_model(&model, &ownership);
			let plan = if paths.is_empty() { planner.full_plan(Vec::new(), BTreeSet::new(), BTreeSet::new(), vec![String::from("no changed paths were given")], Vec::new()) } else { planner.plan(&paths) };
			// Cost is reported, never used to widen the plan silently. Escalating on an ESTIMATE
			// would let a bad estimate quietly cost hours; escalating on a measured ratio the reader
			// can see is a decision they can disagree with.
			let history = verify_model::history::History::load(&repo_root)?;
			let model_cost = verify_model::history::CostModel::default();
			let selected: Vec<verify_model::plan::PlanItemKey> = plan.items.iter().map(|item| item.key.clone()).collect();
			let scoped = model_cost.estimate(&history, &selected);
			let whole = model_cost.estimate(&history, &universe(&model));
			emit(&plan, json, explain, quiet, &model)?;
			if !json && !quiet && !plan.nothing_to_do {
				let share = if whole > 0.0 { scoped / whole * 100.0 } else { 100.0 };
				println!("estimated {scoped:.0} s against {whole:.0} s for everything - {share:.0}% of a full run.");
				// The threshold is about removing a BOOT, not about running fewer tests. The fixed
				// cost of a run dominates so heavily here (~100 s on x86_64 against ~0.2 s per test)
				// that a selection worth 80% of the whole is bookkeeping for nothing.
				if !plan.full && share > 80.0 {
					println!("that is within 80% of everything, so the scoping is not paying for itself here - consider ./verify.sh --release or a full sweep.");
				}
			}
			Ok(ExitCode::SUCCESS)
		}
		other => Err(format!("unknown command '{other}'\n\n{USAGE}")),
	}
}

fn emit(plan: &Plan, json: bool, explain: bool, quiet: bool, model: &Model) -> Result<(), String> {
	if json {
		println!("{}", serde_json::to_string_pretty(plan).map_err(|error| error.to_string())?);
		return Ok(());
	}
	if quiet {
		for item in &plan.items {
			println!("{}", item.command);
		}
		return Ok(());
	}

	if plan.nothing_to_do {
		println!("nothing to verify: every changed path is declared not code.");
		for verdict in &plan.paths {
			println!("  {} - {}", verdict.path, verdict.detail);
		}
		return Ok(());
	}

	if plan.full {
		println!("FULL verification.");
		for reason in &plan.full_reasons {
			println!("  because {reason}");
		}
	} else {
		println!("scoped verification: {} changed component(s) reaching {}.", plan.changed_components.len(), plan.affected_components.len());
	}
	if explain {
		println!();
		println!("paths:");
		for verdict in &plan.paths {
			println!("  {:<52} -> {} ({})", verdict.path, verdict.outcome, verdict.detail);
		}
		if !plan.affected_components.is_empty() {
			println!();
			println!("affected: {}", plan.affected_components.join(", "));
		}
	}
	println!();
	println!("build: {}    boot: {}", join_or_none(&plan.architectures_built), join_or_none(&plan.architectures_booted));
	println!();

	let mut previous: Option<CheckKind> = None;
	for item in &plan.items {
		if previous != Some(item.kind) {
			println!("{:?}:", item.kind);
			previous = Some(item.kind);
		}
		if explain {
			println!("  {:<62} {}", item.key.display(), item.reason);
		} else {
			println!("  {}", item.key.display());
		}
	}

	println!();
	let total: usize = model.catalog.checks.iter().map(|check| check.variants.len()).sum();
	println!("{} of {} runnable keys selected.", plan.items.len(), total);
	for warning in &plan.warnings {
		println!("warning: {warning}");
	}
	if model.kernel_tests.unannotated > 0 {
		println!("note: {} kernel tests have no `covers` declaration, so each of them is always selected.", model.kernel_tests.unannotated);
	}
	Ok(())
}

fn join_or_none(items: &[String]) -> String {
	if items.is_empty() { String::from("none") } else { items.join(",") }
}

// The model's own gates. Each of these fails in the same silent direction if left unchecked: the
// file still parses, the planner still runs, and the plan is quietly narrower than it should be.
fn self_check(model: &Model) -> Result<ExitCode, String> {
	let mut failures = Vec::new();

	let unowned = model.unowned_paths()?;
	if !unowned.is_empty() {
		failures.push(format!("{} tracked file(s) are owned by no component and are not declared non-code:\n  {}", unowned.len(), unowned.join("\n  ")));
	}

	// A crate with host tests and no catalog entry is the inventory defect this milestone was
	// written about: the suites exist, they are milliseconds, and nothing runs them.
	let missing: Vec<&str> = model.crates.iter().filter(|entry| entry.has_host_tests && model.registry.host_tests_runnable(&entry.name) && model.catalog.get(&format!("host.{}", entry.name)).is_none()).map(|entry| entry.name.as_str()).collect();
	if !missing.is_empty() {
		failures.push(format!("crates with host tests and no catalog entry: {}", missing.join(", ")));
	}
	// A declared exception that no longer applies is worse than none: it silently keeps a suite out
	// of the gate after the reason for excluding it is gone.
	for rule in &model.registry.host_tests_unrunnable {
		match model.crates.iter().find(|entry| entry.name == rule.crate_name) {
			None => failures.push(format!("host_tests_unrunnable names '{}', which is not a crate", rule.crate_name)),
			Some(entry) if !entry.has_host_tests => failures.push(format!("host_tests_unrunnable names '{}', which has no host tests to exclude", rule.crate_name)),
			Some(_) => println!("verify-model: host suite '{}' is excluded - {}", rule.crate_name, rule.reason),
		}
	}

	// The catalog's gate list against check.sh's own. Two lists that must agree are two chances to
	// disagree, and the direction that hurts is a gate check.sh runs that the catalog never selects.
	match verify_model::catalog::gates_declared_in_check_sh(&model.repo_root) {
		Ok(declared) => {
			let known = verify_model::catalog::catalog_gate_names();
			for name in declared.difference(&known) {
				failures.push(format!("check.sh runs gate '{name}', which the catalog does not know about - nothing would ever select it"));
			}
			for name in known.difference(&declared) {
				failures.push(format!("the catalog names gate '{name}', which check.sh does not run"));
			}
		}
		Err(error) => failures.push(error),
	}

	// A `covers` entry naming a component nothing in the model knows about is a rename that got
	// half done, and it fails in the silent direction: the test simply stops being selected by the
	// component it used to guard.
	for check in &model.catalog.checks {
		for component in &check.covers {
			if !model.graph.contains(component) {
				failures.push(format!("check '{}' covers '{component}', which is not a component", check.id));
			}
		}
	}

	// `covers X` implies the test can REACH X - the enforceable half of the rule.
	//
	// The other half, "it reaches X therefore it covers X", is deliberately NOT enforced: a scenario
	// that starts StorageService to test `component_host` asserts nothing about StorageService, and
	// inferring coverage from a launch is the `touches = covers` collapse that would inflate every
	// declaration back to the full suite. That direction is the report below, for a person.
	//
	// Reach is the forward closure from what the test's body was seen to touch, so a test that
	// launches `bin.audioconv` legitimately covers `flac` without ever naming it.
	let mut unreachable = Vec::new();
	let mut uncovered: Vec<String> = Vec::new();
	for test in &model.kernel_tests.tests {
		let touched = model.kernel_tests.touches.get(&test.name).cloned().unwrap_or_default();
		for component in verify_model::kerneltests::unreachable_covers(test, &touched, &model.graph) {
			unreachable.push(format!("kernel.{} covers '{component}', which its body cannot reach - it launches nothing that leads there", test.name));
		}
		for component in verify_model::kerneltests::launched_but_not_covered(test, &touched, &model.graph) {
			uncovered.push(format!("kernel.{} launches {component} and does not claim to cover it", test.name));
		}
	}
	failures.extend(unreachable);
	if !uncovered.is_empty() {
		// A REPORT, never a failure. Launching something is not asserting anything about it.
		println!("verify-model: {} test(s) reach a program they do not claim to cover - read, do not fix mechanically:", uncovered.len());
		for line in uncovered.iter().take(8) {
			println!("    {line}");
		}
		if uncovered.len() > 8 {
			println!("    ... and {} more", uncovered.len() - 8);
		}
	}

	// Every component named by the selects-everything list has to exist, or the escalation it
	// promises never fires.
	for rule in &model.registry.selects_everything {
		if !model.graph.contains(&rule.component) {
			failures.push(format!("selects_everything names '{}', which is in no edge of the graph", rule.component));
		}
	}

	// A target that enumerated far fewer tests than its peers did not find its test binary - it
	// found something else shaped like one. That happened: `build.sh --part kernel` writes the
	// ordinary kernel into the same `deps/` directory under the same name shape, and picking the
	// newest file there reported zero tests for the target with no error at all. Zero variants means
	// zero items means a scoped plan that boots nothing and says it booted.
	let mut per_target: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
	for test in &model.kernel_tests.tests {
		for architecture in &test.architectures {
			*per_target.entry(architecture.as_str()).or_default() += 1;
		}
	}
	if let Some(richest) = per_target.values().copied().max() {
		for (architecture, count) in &per_target {
			if *count * 2 < richest {
				failures.push(format!("{architecture} enumerated only {count} kernel tests against {richest} elsewhere - that is a discovery failure, not a target with fewer tests"));
			}
		}
	}

	// `lib.sh`'s VOLUME_SOURCES against the derived answer. A literal list on a hot path is fine; a
	// literal list nobody checks is how `src/term` and `src/volume` went missing from it, and a
	// staleness check that cannot see a change lets a guest boot the previous userspace and pass.
	match std::fs::read_to_string(model.repo_root.join("lib.sh")) {
		Ok(text) => match text.split("VOLUME_SOURCES=(").nth(1).and_then(|rest| rest.split(')').next()) {
			Some(list) => {
				let declared: std::collections::BTreeSet<String> = list.split_whitespace().map(str::to_string).collect();
				let derived = volume_sources(model);
				for missing in derived.difference(&declared) {
					failures.push(format!("lib.sh's VOLUME_SOURCES is missing '{missing}' - a change there can reach the system volume and the staleness check would not see it"));
				}
				for extra in declared.difference(&derived) {
					failures.push(format!("lib.sh's VOLUME_SOURCES names '{extra}', which nothing staged in the volume reaches"));
				}
			}
			None => failures.push(String::from("lib.sh has no VOLUME_SOURCES array to check")),
		},
		Err(error) => failures.push(format!("lib.sh: {error}")),
	}

	// A risk class naming a path that no longer exists is a plan for a tree that has moved on.
	for rule in &model.registry.risk_classes {
		if !model.repo_root.join(&rule.path).exists() {
			failures.push(format!("risk class names '{}', which is not in the tree", rule.path));
		}
	}
	let sensitive: Vec<(&String, &verify_model::archrisk::Risk)> = model.arch_risk.iter().filter(|(_, risk)| risk.any_target || !risk.targets.is_empty()).collect();
	if !sensitive.is_empty() {
		println!("verify-model: {} of {} components are architecture-sensitive by mechanical scan - a change to one boots more than the default target:", sensitive.len(), model.graph.components.len());
		for (component, risk) in sensitive.iter().take(12) {
			println!("    {:<34} {:<22} {}", component, if risk.any_target { String::from("all targets") } else { risk.targets.iter().cloned().collect::<Vec<_>>().join(",") }, risk.evidence.first().map(String::as_str).unwrap_or(""));
		}
		if sensitive.len() > 12 {
			println!("    ... and {} more", sensitive.len() - 12);
		}
	}

	if !model.registry.risk_classes.is_empty() {
		println!("verify-model: {} kernel subsystem(s) are declared narrowable and NOT narrowed - they still select everything:", model.registry.risk_classes.len());
		for rule in &model.registry.risk_classes {
			println!("    {:<28} {:<14} needs: {}", rule.path, rule.class, rule.evidence);
		}
	}

	if !model.kernel_tests.missing_targets.is_empty() {
		println!("verify-model: no kernel test binary for {} - those targets cannot be scoped and will be booted whole", model.kernel_tests.missing_targets.join(", "));
	}

	if failures.is_empty() {
		println!("verify-model: model is consistent");
		println!("  {} crates, {} components, {} edges", model.crates.len(), model.graph.components.len(), model.graph.edges.len());
		println!("  {} checks, {} runnable keys", model.catalog.checks.len(), model.catalog.checks.iter().map(|check| check.variants.len()).sum::<usize>());
		println!("  {} kernel tests, {} without a covers declaration", model.kernel_tests.tests.len(), model.kernel_tests.unannotated);
		println!("  model hash {}", model.model_hash());
		return Ok(ExitCode::SUCCESS);
	}
	for failure in &failures {
		eprintln!("verify-model: {failure}");
	}
	Ok(ExitCode::FAILURE)
}

// Every key the catalog says can run. The age bound, the cost model and the orphan report all range
// over exactly this - never over whatever the history file happens to contain.
fn universe(model: &Model) -> Vec<verify_model::plan::PlanItemKey> {
	let mut keys = Vec::new();
	for check in &model.catalog.checks {
		for variant in &check.variants {
			keys.push(verify_model::plan::PlanItemKey { check: check.id.clone(), architecture: variant.architecture.clone(), environment: variant.environment.clone(), configuration: variant.configuration.clone() });
		}
	}
	keys
}

// Everything the volume is assembled FROM: the closure of every staged program and library over the
// edges that can change a shipped byte, mapped back to the directories those crates live in, plus
// the factory files and the packager itself.
//
// Returned as top-level directories under `src/` rather than as crate paths, because that is what
// the digest walks and because a new crate under an already-listed directory should not need this
// to be recomputed by a person.
fn volume_sources(model: &Model) -> BTreeSet<String> {
	let mut directories: BTreeSet<String> = BTreeSet::new();
	for component in verify_model::staged_components(&model.manifest, &model.crates, &model.graph) {
		if let Some(entry) = model.crates.iter().find(|entry| entry.name == component)
			&& let Some(top) = entry.dir.strip_prefix("src/").and_then(|rest| rest.split('/').next())
		{
			directories.insert(top.to_string());
		}
	}
	// The factory files the volume ships, the interface definitions every protocol is generated
	// from, and the program that writes the package. None is reached by a dependency edge.
	directories.insert(String::from("volume"));
	directories.insert(String::from("tools"));
	directories.insert(String::from("idl"));
	directories
}

fn find_repo_root() -> Result<PathBuf, String> {
	if let Ok(root) = env::var("LIBERSYSTEM_ROOT") {
		return Ok(PathBuf::from(root));
	}
	let mut directory = env::current_dir().map_err(|error| error.to_string())?;
	loop {
		if is_repo_root(&directory) {
			return Ok(directory);
		}
		if !directory.pop() {
			return Err(String::from("cannot find the repository root (no lib.sh beside a src/kernel)"));
		}
	}
}

fn is_repo_root(directory: &Path) -> bool {
	directory.join("lib.sh").is_file() && directory.join("src/kernel").is_dir()
}
