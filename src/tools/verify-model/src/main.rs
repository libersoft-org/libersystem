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
  guest-selection --arch A            the guest test ids a change selects on one target
  shadow --guest-log F --arch A       compare a full sweep against what a change would have scoped
                                      (add --scoped-log F to also EXECUTE the selection and compare)
  shadow --host-log F | --dev-log F   the same comparison for the host and dev-guest universes
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
	// Which STEP a `record` is about, so its whole duration can be stored against it rather than
	// only divided among the keys it discharged. See `History::steps`.
	let mut step_id: Option<String> = None;
	// A frozen narrowing to plan against, instead of the canonical model. See `Model::load_with_candidate`.
	let mut candidate_path: Option<String> = None;
	let mut scoped_log: Option<String> = None;
	// The SCOPED run's log for the keyed universes - the selection executed on its own, before the
	// sweep, so the comparison can ask whether it is executable at all rather than only whether the
	// selector chose the right set.
	let mut host_scoped_log: Option<String> = None;
	let mut dev_scoped_log: Option<String> = None;
	let mut guest_log: Option<String> = None;
	let mut host_log: Option<String> = None;
	let mut dev_log: Option<String> = None;
	// The build universe's log. A full sweep's builds ARE its evidence; this is where they are read
	// back from, and until it existed 189 of the catalog's 192 components could never reach TRUSTED.
	let mut build_log: Option<String> = None;
	let mut build_arch: String = String::from("x86_64");
	let mut build_exec = false;
	// `--scoped` narrows a producer to the keys the SELECTION names, which is what an execution
	// sample runs. Without it a producer emits the whole universe, which is the sweep.
	let mut scoped_only = false;
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
			"--scoped-log" => {
				index += 1;
				scoped_log = Some(arguments.get(index).ok_or("--scoped-log needs a path")?.clone());
			}
			"--guest-log" => {
				index += 1;
				guest_log = Some(arguments.get(index).ok_or("--guest-log needs a path")?.clone());
			}
			// The host universe's results file, which the runner writes directly rather than being
			// scraped out of a serial log. See `parse_host_log`.
			"--host-log" => {
				index += 1;
				host_log = Some(arguments.get(index).ok_or("--host-log needs a path")?.clone());
			}
			// The DEV guest's results file, same format as the host's - a line per check and a
			// `total` line, written by the runner rather than scraped.
			"--dev-log" => {
				index += 1;
				dev_log = Some(arguments.get(index).ok_or("--dev-log needs a path")?.clone());
			}
			// The BUILD universe's results file, and the architecture it describes. Builds run per
			// target, so one log is one target's evidence - `required_architectures` asks for two.
			"--scoped" => {
				scoped_only = true;
			}
			"--host-scoped-log" => {
				index += 1;
				host_scoped_log = Some(arguments.get(index).ok_or("--host-scoped-log needs a path")?.clone());
			}
			"--dev-scoped-log" => {
				index += 1;
				dev_scoped_log = Some(arguments.get(index).ok_or("--dev-scoped-log needs a path")?.clone());
			}
			// Whether a SHADOW-EXEC sample for the BUILD universe came back agreeing.
			//
			// The evidence producer runs the catalog's commands one part at a time; the runner
			// groups them. `verify.sh` runs the grouped one and compares what `build.sh` reports
			// building against the parts the selection named - so the mechanism that ships is the
			// one that was sampled, which is the whole argument SHADOW-EXEC is made of.
			"--build-exec" => {
				index += 1;
				build_exec = arguments.get(index).map(|value| value == "ok").unwrap_or(false);
			}
			"--build-log" => {
				index += 1;
				build_log = Some(arguments.get(index).ok_or("--build-log needs a path")?.clone());
			}
			"--build-arch" => {
				index += 1;
				build_arch = arguments.get(index).ok_or("--build-arch needs an architecture")?.clone();
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
			"--candidate" => {
				index += 1;
				candidate_path = Some(arguments.get(index).ok_or("--candidate needs the path of a candidate file")?.clone());
			}
			"--step-id" => {
				index += 1;
				step_id = Some(arguments.get(index).ok_or("--step-id needs a StepId")?.clone());
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
		&& matches!(first.as_str(), "plan" | "commands" | "host-suites" | "host-checks" | "dev-checks" | "build-checks" | "build-steps" | "booted" | "built" | "guest-selection" | "changes" | "age" | "record" | "reach" | "level" | "source-digest" | "volume-sources" | "shadow" | "trust" | "catalog" | "graph" | "owner" | "check" | "model-hash" | "discard-divided-costs" | "candidate-activate")
	{
		command = positional.remove(0);
	}
	// `KIND\tPATH` OR A BARE PATH, and the tab is what closes the `--for-range` hole.
	//
	// `change_kinds_for` reads the WORKING TREE and nothing else, and `verify.sh --for-range`
	// resolved its range to a list of paths and passed only the paths onward. On a clean tree the
	// needed scope therefore had an empty `change_kinds`, and `Scope::covers` is satisfied trivially
	// by an empty requirement - so a certificate earned over `modified` answered for a range
	// containing a RENAME that nothing had verified. The range knows its own change kinds; they just
	// had nowhere to travel.
	//
	// A bare path still means "ask the working tree", which is what `--for PATH` has always meant
	// and what the regression corpus drives.
	let mut path_change_kinds: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
	if from_stdin {
		let mut text = String::new();
		io::stdin().read_to_string(&mut text).map_err(|error| format!("stdin: {error}"))?;
		for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
			match line.split_once('\t') {
				Some((kind, path)) if !kind.is_empty() && !path.is_empty() => {
					path_change_kinds.insert(String::from(path), String::from(kind));
					paths.push(String::from(path));
				}
				_ => paths.push(String::from(line)),
			}
		}
	}

	let repo_root = find_repo_root()?;
	// THE MODEL EVERY COMMAND WORKS AGAINST, and `--candidate` is what makes a NARROWER one available
	// without installing it.
	//
	// M5's shape is evidence first, split second: the authoritative run stays FULL while the narrower
	// selection is computed beside it, and the comparison is recorded under the candidate's own hash.
	// Before this the only candidate command was `candidate-activate`, which WRITES the overlay - so
	// the only way to plan against a candidate was to activate it, and activation is what the evidence
	// is for. `--candidate` is refused for `candidate-activate` itself, which must read the canonical
	// files with no overlay in its path or the hash it compares would be the overlay agreeing with
	// itself.
	let candidate_overlay: Option<verify_model::candidate::Candidate> = match &candidate_path {
		Some(path) if command == "candidate-activate" => return Err(format!("--candidate is not for `candidate-activate`: it reads the canonical files with no overlay in its path, which is what makes the hash it compares mean anything ({path})")),
		Some(path) => Some(verify_model::candidate::Candidate::load(std::path::Path::new(path))?),
		None => None,
	};
	let model = Model::load_with_candidate(&repo_root, candidate_overlay.as_ref())?;
	if let Some(candidate) = &candidate_overlay {
		let planned = model.model_hash();
		if planned != candidate.expected_hash {
			return Err(format!("this candidate plans as model {planned} and says its evidence is gathered under {}, so a record written now would be filed against a model this overlay does not produce - refreeze it", candidate.expected_hash));
		}
		eprintln!("verify-model: planning against candidate {planned} - {}", candidate.reason);
	}

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
		// Every check that runs on the HOST, as `id<TAB>command`, for the shadow producer.
		//
		// A command in the model rather than a `jq` filter in the shell, for the reason the `booted`
		// command exists: every decision that needs the model belongs in the model, and a fragile
		// parse in `verify.sh` is the same decision made twice and drifting.
		// Every check that runs in the DEV guest, as `id<TAB>command`, for the third shadow producer.
		//
		// `DevGuest` was a declared universe with no producer: `Universe::of` routed `dev.*` checks
		// to it, `required_architectures` answered 1 for it, and nothing ever wrote a record. So
		// `trusted_everywhere` - which asks the catalog which universes may judge a component -
		// could never be satisfied for `bin.dev_agent`, `bin.dev_channel`, `harness.boot` or
		// `proto`, whatever evidence they accumulated elsewhere. A universe that cannot be fed is a
		// universe that only ever answers no.
		"dev-checks" => {
			// The SELECTION's keys when `--scoped` is given: that is what an execution sample runs,
			// and running the whole universe would be the sweep rather than a sample of it.
			let scoped: Option<std::collections::BTreeSet<verify_model::plan::PlanItemKey>> = if scoped_only {
				let ownership = model.ownership();
				let planner = Planner::for_model(&model, &ownership);
				Some(planner.plan(&paths).items.iter().map(|item| item.key.clone()).collect())
			} else {
				None
			};
			for check in &model.catalog.checks {
				for variant in &check.variants {
					if variant.environment == verify_model::catalog::Environment::DevGuest {
						if let Some(scoped) = &scoped
							&& !scoped.iter().any(|key| key.check == check.id && key.architecture == variant.architecture && key.configuration == variant.configuration)
						{
							continue;
						}
						// The whole key, like the host producer beside it - one shape for both, so
						// the model reads one format and the comparison rebuilds nothing.
						//
						// THROUGH THE RECORD, not the configuration's name. This ran a dev check's
						// command - `(cd src && harness/dev-selftest.py)` - through a lowering that
						// appends cargo flags to anything not called `default`, and emitted
						// `(cd src && harness/dev-selftest.py) --no-default-features --features
						// development`: a bash syntax error, so every dev-guest shadow line failed
						// before it started and clean `DevGuest` evidence was unobtainable.
						let configuration = model.registry.configuration(&variant.configuration).ok_or_else(|| format!("the dev check '{}' names configuration '{}', which the registry does not define", check.id, variant.configuration))?;
						let command = verify_model::commands::lower(check.kind.clone(), &check.command.replace("{arch}", &variant.architecture), configuration);
						println!("{}\t{}\t{}\t{}\t{}", check.id, variant.architecture, variant.environment.as_str(), variant.configuration, command);
					}
				}
			}
			Ok(ExitCode::SUCCESS)
		}
		// THE THIRD PRODUCER, and the one whose absence made 98% of the tree permanently untrustable.
		//
		// `HostBuild` was split out of `Host` because a certificate earned over gates, suites and
		// conformance was standing for builds nothing had compared - the right change, and it left a
		// universe with no evidence producer at all. `trusted_everywhere` requires `Trusted` in every
		// judging universe, and `build.libs`, `build.user`, `build.kernel` and the rest cover every
		// crate and every program under their prefix: 189 of the catalog's 192 components are judged
		// by `HostBuild`, so 189 of them could accumulate any amount of evidence elsewhere and never
		// arrive. Fail-closed, and a steady state 98% of the tree cannot enter is not a steady state.
		//
		// It costs no extra run. A full sweep already builds every part on every architecture; this
		// says which keys those runs discharge, in the same shape the other two producers emit, so
		// `compare_keyed` is pointed at it unchanged.
		// The COLLAPSED build steps the production runner ships, one per architecture, with the keys
		// each claims to discharge: `label \t command \t key|key|...`.
		//
		// `build-checks` above lists the catalog's per-part commands, which is what the evidence
		// producer runs - one `--part` at a time. The runner groups them: `./build.sh --arch X
		// --part a,b,c`. Those are different code paths through the same script and only the second
		// one was ever compared. A grouped `--part a,b,c` whose parser silently used only `a` and
		// exited zero would leave every individual build check passing, every `HostBuild` record
		// clean, a certificate granted - and the scoped runner building less than the selection
		// said. That is the precise shape of the defect SHADOW-EXEC was built for.
		"build-steps" => {
			let ownership = model.ownership();
			let planner = Planner::for_model(&model, &ownership);
			let plan = if paths.is_empty() { planner.full_plan(Vec::new(), BTreeSet::new(), BTreeSet::new(), vec![String::from("no changed paths were given")], Vec::new()) } else { planner.plan(&paths) };
			let kernel_tests: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
			for step in verify_model::commands::steps(&plan, &kernel_tests, &model.registry) {
				if !step.command.starts_with("./build.sh") {
					continue;
				}
				let keys: Vec<String> = step.keys.iter().map(|key| key.display()).collect();
				println!("{}\t{}\t{}", step.label, step.command, keys.join("|"));
			}
			Ok(ExitCode::SUCCESS)
		}
		"build-checks" => {
			for check in &model.catalog.checks {
				if check.kind != verify_model::catalog::CheckKind::Build {
					continue;
				}
				for variant in &check.variants {
					// The whole key, like the two producers beside it: `(check, architecture,
					// environment, configuration)` is what a `PlanItemKey` is, and emitting the id
					// alone made two variants of one check produce two lines carrying one name.
					let command = check.command.replace("{arch}", &variant.architecture);
					println!("{}\t{}\t{}\t{}\t{}", check.id, variant.architecture, variant.environment.as_str(), variant.configuration, command);
				}
			}
			Ok(ExitCode::SUCCESS)
		}
		"host-checks" => {
			let host_scoped: Option<std::collections::BTreeSet<verify_model::plan::PlanItemKey>> = if scoped_only {
				let ownership = model.ownership();
				let planner = Planner::for_model(&model, &ownership);
				Some(planner.plan(&paths).items.iter().map(|item| item.key.clone()).collect())
			} else {
				None
			};
			for check in &model.catalog.checks {
				// Builds are excluded. They run on the host and they can fail, but a shadow
				// comparison asks "did anything OUTSIDE the selection fail" - and re-running three
				// architectures' builds to answer it would cost more than the sweep it is comparing
				// against, over artifacts the sweep has already produced. What is left is the checks:
				// gates, host suites, conformance.
				if check.kind == verify_model::catalog::CheckKind::Build {
					continue;
				}
				for variant in &check.variants {
					if variant.environment == verify_model::catalog::Environment::Host {
						if let Some(scoped) = &host_scoped
							&& !scoped.iter().any(|key| key.check == check.id && key.architecture == variant.architecture && key.configuration == variant.configuration)
						{
							continue;
						}
						// THE WHOLE KEY, not the check id. `(check, architecture, environment,
						// configuration)` is what a `PlanItemKey` is, and emitting the id alone
						// meant two variants of one check produced two lines carrying the same
						// name: the parser's map kept the last and `total` counted both, so the
						// declared total and the number of outcomes disagreed silently.
						//
						// And the command through the shared lowering, so a `shared-image` variant
						// is actually run as one - from the configuration RECORD, which is what says
						// whether a feature selection means anything to this kind of check at all.
						let configuration = model.registry.configuration(&variant.configuration).ok_or_else(|| format!("the check '{}' names configuration '{}', which the registry does not define", check.id, variant.configuration))?;
						let command = verify_model::commands::lower(check.kind.clone(), &check.command.replace("{arch}", &variant.architecture), configuration);
						println!("{}\t{}\t{}\t{}\t{}", check.id, variant.architecture, variant.environment.as_str(), variant.configuration, command);
					}
				}
			}
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
				println!("{}\t{}", test.id, reachable.into_iter().collect::<Vec<_>>().join(","));
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
			// `KIND\tPATH`, which is what the stdin reader above accepts. Both modes print it, so
			// there is one format rather than one per caller - and a range, which has no working
			// tree to be asked about afterwards, carries its own classes of change onward instead of
			// arriving with an empty scope that covers everything.
			for change in &changes {
				let path = change.origin.as_deref().unwrap_or(change.path.as_str());
				let _ = path;
				println!("{}\t{}", format!("{:?}", change.kind).to_lowercase(), change.path);
			}
			Ok(ExitCode::SUCCESS)
		}
		// Which targets a change must be BUILT on, one per line.
		//
		// SEPARATE FROM `booted`, and this milestone separated the two fields on purpose: a change
		// that boots one target still has to COMPILE on the other two, which the regression corpus
		// states in its own words - `riscv64-trap-handling` boots riscv64 alone and must select
		// `build.kernel / x86_64 / host / shared-image`, "a branch that stops compiling elsewhere is
		// a regression as well".
		//
		// `verify.sh`'s build-evidence producer looped over the BOOTED set, so on exactly that change
		// the planner selected an x86_64 build check and the shadow producer never ran one. The
		// ordinary shape of userspace work - cross-build everything, boot x86_64 - recorded build
		// evidence for one architecture, and `HostBuild` requires two. The wall was removed from the
		// data model two rounds ago; the shell was still building the wrong side of it.
		"built" => {
			let ownership = model.ownership();
			let planner = Planner::for_model(&model, &ownership);
			let plan = if paths.is_empty() { planner.full_plan(Vec::new(), BTreeSet::new(), BTreeSet::new(), vec![String::from("no changed paths were given")], Vec::new()) } else { planner.plan(&paths) };
			for architecture in &plan.architectures_built {
				println!("{architecture}");
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
		// The guest test IDS a change selects on one target, one per line - the exact list the
		// runner takes as `TEST_SELECTION`.
		//
		// It exists for SHADOW-EXEC: the selection has to be RUN before a run of it can be compared
		// with the sweep, and the ordinary plan lowers it into a shell command rather than handing
		// the bare list out. The ids are the check ids VERBATIM - a kernel check's id IS the test's
		// declared id, which is the equality `self_check` exists to hold, and those ids already
		// begin with `kernel.`.
		"guest-selection" => {
			let architecture = architecture.ok_or("guest-selection needs --arch")?;
			if paths.is_empty() {
				return Err(String::from("guest-selection needs the change it is selecting for (--paths or --stdin)"));
			}
			let ownership = model.ownership();
			let planner = Planner::for_model(&model, &ownership);
			let plan = planner.plan(&paths);
			for item in &plan.items {
				if item.key.environment == verify_model::catalog::Environment::TestGuest && item.key.architecture == architecture {
					println!("{}", item.key.check);
				}
			}
			Ok(ExitCode::SUCCESS)
		}
		// DRY shadow: the scoped set is COMPUTED and never run; the full sweep that already
		// happened is what it is compared against. One boot, two answers.
		"shadow" => {
			// The HOST universe, when a host results file is what was given.
			//
			// One command, two universes, because the comparison is the same question asked of a
			// different runner: what ran, what failed, and was the failure inside the selection.
			// Nothing produced a Host record before this, which is why `trusted_everywhere` - Host
			// AND TestGuest - could never be true for anything.
			if let Some(path) = &host_log {
				if paths.is_empty() {
					return Err(String::from("shadow needs the change it is validating (--paths or --stdin); comparing against nothing proves nothing"));
				}
				let text = std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?;
				let results = verify_model::shadow::parse_host_log(&text);
				let ownership = model.ownership();
				let planner = Planner::for_model(&model, &ownership);
				let plan = planner.plan(&paths);
				let selected: Vec<verify_model::plan::PlanItemKey> = plan.items.iter().map(|item| item.key.clone()).collect();
				let history = verify_model::history::History::load(&repo_root)?;
				// THE EXECUTION SAMPLE, when one was taken. A dry comparison answers "did the
				// selector choose the right set" and cannot answer "does running that set work" -
				// and this universe has the proof that the second question is not theoretical: the
				// dev producer next door emitted a command bash could not parse, and every dry
				// comparison stayed clean.
				let host_exec = match &host_scoped_log {
					Some(path) => {
						let scoped_text = std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?;
						let scoped_results = verify_model::shadow::parse_host_log(&scoped_text);
						let exec = verify_model::shadow::compare_exec_keyed(&selected, &scoped_results, &results, "host", |key| key.environment == verify_model::catalog::Environment::Host && !model.catalog.checks.iter().any(|check| check.id == key.check && check.kind == verify_model::catalog::CheckKind::Build));
						println!("shadow-exec (host): {:?}", exec.verdict);
						println!("  {}", exec.reason);
						for key in &exec.inside_failures {
							println!("  SELECTED BUT DID NOT RUN: {key}");
						}
						Some(exec)
					}
					None => None,
				};
				// A sample that FAILED is not a sample.
				let host_exec_clean = host_exec.as_ref().is_some_and(|exec| exec.verdict == verify_model::shadow::Verdict::Consistent);
				let comparison = verify_model::shadow::compare_host(&selected, &results, &history);
				println!("shadow (host): {:?}", comparison.verdict);
				println!("  {}", comparison.reason);
				println!("  {} check(s) ran; {} of them were in the scoped selection", comparison.ran, comparison.scoped);
				for key in &comparison.inside_failures {
					println!("  FAILED (selected): {key}");
				}
				for key in &comparison.outside_failures {
					println!("  FAILED (not selected): {key}");
				}
				let mut log = verify_model::shadow::Log::load(&repo_root);
				log.schema = 1;
				log.records.push(verify_model::shadow::Record { universe: verify_model::shadow::Universe::Host, architecture: String::from("host"), verdict: format!("{:?}", comparison.verdict), reason: comparison.reason.clone(), model_hash: model.model_hash(), source_digest: verify_model::shadow::source_digest(&repo_root)?, changed_components: plan.changed_components.clone(), outside_failures: comparison.outside_failures.clone(), at: verify_model::history::now(), change_kinds: verify_model::shadow::change_kinds_for(&repo_root, &paths, &path_change_kinds), edge_kinds: plan.edge_kinds.clone(), shadow_exec: host_exec_clean, model_self_check: self_check_failures(&model, false).is_empty(), component_decisions: plan.component_decisions.clone(), component_scopes: verify_model::shadow::component_scopes(&repo_root, &plan, &path_change_kinds, &model.registry) });
				log.save(&repo_root)?;
				return Ok(if comparison.verdict == verify_model::shadow::Verdict::Consistent { ExitCode::SUCCESS } else { ExitCode::FAILURE });
			}
			// THE BUILD UNIVERSE. Same shape as the two branches around it, and the reason it exists
			// is that without it `HostBuild` had no producer at all: every component a build check
			// covers - 189 of 192 - was permanently untrustable, because `trusted_everywhere` asks
			// every judging universe and one of them could never answer.
			//
			// Per architecture, because a build of x86_64 says nothing about aarch64: that is what
			// `required_architectures` asks for and what the record has to be able to supply.
			if let Some(path) = &build_log {
				if paths.is_empty() {
					return Err(String::from("shadow needs the change it is validating (--paths or --stdin); comparing against nothing proves nothing"));
				}
				let text = std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?;
				let results = verify_model::shadow::parse_host_log(&text);
				let ownership = model.ownership();
				let planner = Planner::for_model(&model, &ownership);
				let plan = planner.plan(&paths);
				let selected: Vec<verify_model::plan::PlanItemKey> = plan.items.iter().map(|item| item.key.clone()).collect();
				let history = verify_model::history::History::load(&repo_root)?;
				let comparison = verify_model::shadow::compare_build(&selected, &results, &history, &model.catalog, &build_arch);
				println!("shadow (host-build, {build_arch}): {:?}", comparison.verdict);
				println!("  {}", comparison.reason);
				println!("  {} build(s) ran; {} of them were in the scoped selection", comparison.ran, comparison.scoped);
				for key in &comparison.inside_failures {
					println!("  FAILED (selected): {key}");
				}
				for key in &comparison.outside_failures {
					println!("  FAILED (not selected): {key}");
				}
				let mut log = verify_model::shadow::Log::load(&repo_root);
				log.schema = 1;
				log.records.push(verify_model::shadow::Record { universe: verify_model::shadow::Universe::HostBuild, architecture: build_arch.clone(), verdict: format!("{:?}", comparison.verdict), reason: comparison.reason.clone(), model_hash: model.model_hash(), source_digest: verify_model::shadow::source_digest(&repo_root)?, changed_components: plan.changed_components.clone(), outside_failures: comparison.outside_failures.clone(), at: verify_model::history::now(), change_kinds: verify_model::shadow::change_kinds_for(&repo_root, &paths, &path_change_kinds), edge_kinds: plan.edge_kinds.clone(), shadow_exec: build_exec, model_self_check: self_check_failures(&model, false).is_empty(), component_decisions: plan.component_decisions.clone(), component_scopes: verify_model::shadow::component_scopes(&repo_root, &plan, &path_change_kinds, &model.registry) });
				log.save(&repo_root)?;
				return Ok(if comparison.verdict == verify_model::shadow::Verdict::Consistent { ExitCode::SUCCESS } else { ExitCode::FAILURE });
			}
			// The DEV guest universe. Same shape as the host branch: one results file, one record.
			if let Some(path) = &dev_log {
				if paths.is_empty() {
					return Err(String::from("shadow needs the change it is validating (--paths or --stdin); comparing against nothing proves nothing"));
				}
				let text = std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?;
				let results = verify_model::shadow::parse_host_log(&text);
				let ownership = model.ownership();
				let planner = Planner::for_model(&model, &ownership);
				let plan = planner.plan(&paths);
				let selected: Vec<verify_model::plan::PlanItemKey> = plan.items.iter().map(|item| item.key.clone()).collect();
				let history = verify_model::history::History::load(&repo_root)?;
				// The same execution sample, and the universe whose producer the mechanism would
				// have caught: `(cd src && harness/dev-selftest.py) --no-default-features --features
				// development` is a bash syntax error, so every dev shadow line failed before it
				// started and no dry comparison could see it.
				let dev_exec = match &dev_scoped_log {
					Some(path) => {
						let scoped_text = std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?;
						let scoped_results = verify_model::shadow::parse_host_log(&scoped_text);
						let exec = verify_model::shadow::compare_exec_keyed(&selected, &scoped_results, &results, "dev-guest", |key| key.environment == verify_model::catalog::Environment::DevGuest);
						println!("shadow-exec (dev-guest): {:?}", exec.verdict);
						println!("  {}", exec.reason);
						for key in &exec.inside_failures {
							println!("  SELECTED BUT DID NOT RUN: {key}");
						}
						Some(exec)
					}
					None => None,
				};
				let dev_exec_clean = dev_exec.as_ref().is_some_and(|exec| exec.verdict == verify_model::shadow::Verdict::Consistent);
				let comparison = verify_model::shadow::compare_dev(&selected, &results, &history);
				println!("shadow (dev-guest): {:?}", comparison.verdict);
				println!("  {}", comparison.reason);
				println!("  {} check(s) ran; {} of them were in the scoped selection", comparison.ran, comparison.scoped);
				for key in &comparison.inside_failures {
					println!("  FAILED (selected): {key}");
				}
				for key in &comparison.outside_failures {
					println!("  FAILED (not selected): {key}");
				}
				let mut log = verify_model::shadow::Log::load(&repo_root);
				log.schema = 1;
				log.records.push(verify_model::shadow::Record { universe: verify_model::shadow::Universe::DevGuest, architecture: String::from("x86_64"), verdict: format!("{:?}", comparison.verdict), reason: comparison.reason.clone(), model_hash: model.model_hash(), source_digest: verify_model::shadow::source_digest(&repo_root)?, changed_components: plan.changed_components.clone(), outside_failures: comparison.outside_failures.clone(), at: verify_model::history::now(), change_kinds: verify_model::shadow::change_kinds_for(&repo_root, &paths, &path_change_kinds), edge_kinds: plan.edge_kinds.clone(), shadow_exec: dev_exec_clean, model_self_check: self_check_failures(&model, false).is_empty(), component_decisions: plan.component_decisions.clone(), component_scopes: verify_model::shadow::component_scopes(&repo_root, &plan, &path_change_kinds, &model.registry) });
				log.save(&repo_root)?;
				return Ok(if comparison.verdict == verify_model::shadow::Verdict::Consistent { ExitCode::SUCCESS } else { ExitCode::FAILURE });
			}
			let guest_log = guest_log.ok_or("shadow needs --guest-log, --host-log or --dev-log")?;
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

			// SHADOW-EXEC, when a scoped run's log was given as well.
			//
			// A dry comparison answers "did the selector choose the right set S" and never runs S,
			// so it cannot answer "does running S work" - and this milestone contains the proof that
			// the second question is not theoretical: a planner emitting Rust function names against
			// a runner that had moved to stable IDs computed every selection correctly and could
			// execute none of them, and every dry comparison stayed clean throughout.
			//
			// With `--scoped-log` the two are compared, the run is marked as carrying an execution
			// sample, and `evaluate` requires one before a certificate is granted.
			let exec = match &scoped_log {
				Some(path) => {
					let scoped_text = std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?;
					let scoped_results = verify_model::shadow::parse_guest_log(&scoped_text);
					let exec = verify_model::shadow::compare_exec(&selected, &scoped_results, &results, &architecture);
					println!("shadow-exec ({architecture}): {:?}", exec.verdict);
					println!("  {}", exec.reason);
					for key in &exec.inside_failures {
						println!("  SELECTED BUT DID NOT RUN: {key}");
					}
					Some(exec)
				}
				None => None,
			};
			// A sample that FAILED is not a sample: it is the finding the mechanism exists to make,
			// and filing it as evidence would be the false green in its purest form.
			let exec_clean = exec.as_ref().is_some_and(|exec| exec.verdict == verify_model::shadow::Verdict::Consistent);

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
			log.records.push(verify_model::shadow::Record { universe: verify_model::shadow::Universe::TestGuest, architecture: architecture.clone(), verdict: format!("{:?}", comparison.verdict), reason: comparison.reason.clone(), model_hash: model.model_hash(), source_digest: verify_model::shadow::source_digest(&repo_root)?, changed_components: plan.changed_components.clone(), outside_failures: comparison.outside_failures.clone(), at: verify_model::history::now(), change_kinds: verify_model::shadow::change_kinds_for(&repo_root, &paths, &path_change_kinds), edge_kinds: plan.edge_kinds.clone(), shadow_exec: exec_clean, model_self_check: self_check_failures(&model, false).is_empty(), component_decisions: plan.component_decisions.clone(), component_scopes: verify_model::shadow::component_scopes(&repo_root, &plan, &path_change_kinds, &model.registry) });
			log.save(&repo_root)?;
			// Only Consistent is green, and the other three are green in different wrong ways.
			// CandidateMiss is the selector's problem; SelectionFailed is the code's and the sweep
			// found it; Void means the comparison judged nothing at all. Returning 0 for the last
			// two let `verify.sh` print "no evidence against the selection" over a run that had
			// produced no evidence either way, which is the exact shape of false green.
			// An execution sample that failed fails the command, whatever the dry comparison said -
			// they are different questions and only one of them can be answered by not running.
			if exec.is_some() && !exec_clean {
				return Ok(ExitCode::FAILURE);
			}
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
			// AND TRUSTED FOR THIS CHANGE. A certificate is earned over particular classes of edit
			// reached through particular graph edges; a rename that pulls a component in through
			// `generation.build` is not what five clean source-edit comparisons validated. The
			// scope travels with the grant now, so the question the runner asks is a subset test
			// rather than a lookup.
			// AND THE TARGETS THIS CHANGE NEEDS, which the lookup never asked for. `Certificate`
			// has carried `architectures` since it existed and `level` matched on the model hash and
			// the scope alone, so a certificate earned from x86_64 and riscv64 evidence answered an
			// aarch64-only change on which no clean record had ever run.
			let mut required: std::collections::BTreeSet<String> = plan.architectures_built.iter().cloned().collect();
			required.extend(plan.architectures_booted.iter().cloned());
			let required: Vec<String> = required.into_iter().collect();
			let scopes: std::collections::BTreeMap<String, verify_model::shadow::Scope> = verify_model::shadow::component_scopes(&repo_root, &plan, &path_change_kinds, &model.registry).into_iter().map(|(component, scope)| (component, scope.with_architectures(required.clone()))).collect();
			let empty = verify_model::shadow::Scope::default();
			let untrusted: Vec<&String> = plan.changed_components.iter().filter(|component| !store.trusted_everywhere(component, &hash, &verify_model::catalog::judging_universes(&model.catalog, component), scopes.get(component.as_str()).unwrap_or(&empty))).collect();
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
				// AND WHAT IS MISSING, WHICH `shortfall()` HAS ALWAYS COMPUTED AND NOBODY CALLED.
				//
				// The refusal arrived without the one sentence that says what to do about it: an
				// operator was told a component falls back to shadow and not whether it is a change
				// class, a graph edge or a target that the certificate does not cover. The
				// difference matters - the first two are answered by gathering evidence over that
				// kind of change, and the third by running the sweep on that target.
				for component in &untrusted {
					let needed = scopes.get(component.as_str()).unwrap_or(&empty);
					for universe in verify_model::catalog::judging_universes(&model.catalog, component) {
						let missing = store.shortfall(component, &hash, universe, needed);
						if !missing.is_empty() {
							println!("  {component} ({universe:?}) is not covered for: {}", missing.join(", "));
						}
					}
				}
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
				for universe in [
					verify_model::shadow::Universe::Host,
					verify_model::shadow::Universe::HostBuild,
					verify_model::shadow::Universe::TestGuest,
					verify_model::shadow::Universe::DevGuest,
				] {
					match store.evaluate(&component, &hash, universe, &log) {
						Ok((clean, architectures)) => {
							// WHAT THE EVIDENCE COVERS, computed BEFORE the grant and stored ON it.
							// `evaluate` counts clean runs, distinct decisions, architectures and an
							// execution sample; the design also asked which change classes and edge
							// kinds were exercised. This used to be computed after the grant and
							// printed, so the certificate that was stored could not name its own
							// scope and `level` had nothing to compare a change against.
							let scope = store.evidence_scope(&component, &hash, universe, &log);
							store.grant(&component, &hash, universe, clean, architectures, scope.scope.clone(), verify_model::history::now());
							println!("{component} is TRUSTED for {universe:?} under model {hash}");
							println!("  evidence: {} clean record(s), change kinds [{}], edge kinds [{}], model self-check {}", scope.records, scope.scope.change_kinds.join(", "), scope.scope.edge_kinds.join(", "), if scope.all_self_checked { "green in every one" } else { "NOT green in all of them" });
							println!("  and it answers for those and nothing else: a change of another class falls back to shadow");
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
			// THE SCOPE BESIDE THE LEVEL. `summary` answered `Trusted`/`Shadow` and ignored the
			// scope it was summarising, so a reader saw a component listed as TRUSTED with no way to
			// know that the trust covers modifications through static links and nothing else - which
			// is exactly the misreading the scope was added to prevent.
			for (component, entry) in store.summary(&hash) {
				match entry.scope {
					Some(scope) if !scope.pairs.is_empty() => println!("  {component}: {:?} for [{}] on [{}]", entry.level, scope.pairs.iter().map(|pair| pair.replace('\t', " through ")).collect::<Vec<String>>().join(", "), scope.architectures.join(", ")),
					_ => println!("  {component}: {:?} (the certificate names no scope, so it covers nothing)", entry.level),
				}
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
		// ACTIVATE A CANDIDATE NARROWING, or refuse it having written nothing.
		//
		// Four steps and every one of them is load-bearing. The base is verified BEFORE the first
		// write, because comparing only the RESULT says nothing about what was there: an activation
		// that overwrote a `registry.toml` edited in the meantime would still produce the hash the
		// candidate predicts, and would certify a model nobody reviewed. The overlay is materialised
		// into the canonical files. The active model is then re-read from those files with NO overlay
		// in its path - if the candidate planner and the active planner read the same thing, an
		// identical hash says the overlay equals itself and proves nothing. Only that hash is
		// compared, and a mismatch is rolled back byte for byte.
		"candidate-activate" => {
			let path = positional.first().ok_or("candidate-activate needs the path of a candidate file")?;
			let candidate = verify_model::candidate::Candidate::load(std::path::Path::new(path))?;
			let mut sources: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
			for test in &model.kernel_tests.tests {
				sources.insert(test.id.clone(), test.source_paths.clone());
			}
			// The base is checked against the files this candidate will WRITE, which is why the
			// source map is built first: a candidate that omits one of them from its base records no
			// digest for a file it replaces, and "the base is unmoved" would be true of a smaller set.
			candidate.base_is_unmoved(&repo_root, &sources)?;
			// AND THE EVIDENCE BAR, WHICH NOTHING WAS ASKING FOR.
			//
			// Activation checked the base digests and the resulting model hash and never called the
			// trust evaluation at all - so a candidate could activate a narrowing with NO qualifying
			// evidence behind it, which is the one thing M5's contract is for. A narrowing takes
			// coverage AWAY from components: for every component a test stops covering under this
			// candidate, the shadow log has to have earned that component a certificate UNDER THIS
			// CANDIDATE'S OWN HASH. Evidence gathered under the current model says nothing about the
			// narrower one, which is the same argument that makes `expected_hash` load-bearing.
			{
				let log = verify_model::shadow::Log::load(&repo_root);
				let store = verify_model::trust::Store::load(&repo_root);
				let mut short: Vec<String> = Vec::new();
				for (id, narrowed) in &candidate.covers {
					let Some(check) = model.catalog.checks.iter().find(|check| &check.id == id) else { continue };
					for lost in check.covers.iter().filter(|component| !narrowed.contains(component)) {
						if short.iter().any(|line| line.starts_with(&format!("{lost}:"))) {
							continue;
						}
						// The universe a kernel test is judged in. A narrowing of a guest test is
						// answered by guest evidence; asking for host evidence about it would be a
						// bar nothing could ever meet.
						if let Err(why) = store.evaluate(lost, &candidate.expected_hash, verify_model::shadow::Universe::TestGuest, &log) {
							short.push(format!("{lost}: {why}"));
						}
					}
				}
				if !short.is_empty() {
					return Err(format!("this candidate narrows coverage of component(s) that have not earned it under its own model hash, so activating it would take away checking nothing has shown to be spare:\n    {}\n  Gather the evidence under {} first; nothing was written.", short.join("\n    "), candidate.expected_hash));
				}
			}
			let previous = candidate.materialise(&repo_root, &sources)?;
			let active = match Model::load(&repo_root) {
				Ok(active) => active,
				Err(error) => {
					verify_model::candidate::Candidate::roll_back(&repo_root, &previous)?;
					return Err(format!("the materialised model does not load ({error}); nothing was kept"));
				}
			};
			let actual = active.model_hash();
			if actual != candidate.expected_hash {
				verify_model::candidate::Candidate::roll_back(&repo_root, &previous)?;
				return Err(format!("the activated model hashes {actual} and this candidate's evidence was gathered under {}, so the two are not the same model - rolled back, nothing kept", candidate.expected_hash));
			}
			println!("verify-model: candidate activated - {}", candidate.reason);
			println!("verify-model: the active model hashes {actual}, which is what its evidence was gathered under");
			Ok(ExitCode::SUCCESS)
		}
		// Throw away every cost that was invented by dividing a merged step's duration.
		//
		// Run once, before the first cheapest-first run: ordering on those figures is sorting on how
		// the gates happened to be batched, and inheriting them would make the first ordered run the
		// least trustworthy one. Freshness survives where the step PASSED, because a step that
		// passed really did run every member of itself.
		"discard-divided-costs" => {
			let mut history = verify_model::history::History::load(&repo_root)?;
			let (costs, freshness) = history.discard_divided_costs();
			history.save(&repo_root)?;
			println!("verify-model: {costs} invented cost(s) discarded, {freshness} record(s) dropped for a merged step that failed before reaching them");
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
			history.record_step_id(step_id.as_deref(), &keys, passed, seconds, &hash, &verify_model::history::CostModel::default());
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
			let cost = verify_model::history::CostModel { whole_suite_tests: model.kernel_tests.declared_ids, ..verify_model::history::CostModel::default() };
			let history = verify_model::history::History::load(&model.repo_root).unwrap_or_default();
			let model_hash = model.model_hash();
			// CHEAPEST FIRST, AMONG THE STEPS WHOSE PREREQUISITES ARE MET.
			//
			// A plan that is going to fail runs its cheapest evidence last, which is minutes of
			// waiting for news that a two-second host suite already had. Ordering by cost alone
			// would emit a guest before the build it cannot start without, so the sort is by LAYER
			// first - how deep in the dependency graph a step sits - and by cost inside a layer.
			//
			// The id breaks ties, so the emission is stable: a plan that reorders itself between two
			// identical runs is one nobody can diff.
			let mut ordered = verify_model::commands::steps(&plan, &per_target, &model.registry);
			// VALIDATED BEFORE IT IS WALKED - see `commands::validate`. A plan whose graph is wrong
			// is not a plan to emit with a warning: the runner would read it, wait on a step nobody
			// emits, or run one before what it reads.
			if let Err(faults) = verify_model::commands::validate(&ordered) {
				for fault in &faults {
					eprintln!("verify-model: {fault}");
				}
				eprintln!("verify-model: the plan's dependency graph is not usable, so no plan is emitted");
				std::process::exit(1);
			}
			let mut layers: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
			for _ in 0..ordered.len() {
				for step in &ordered {
					let depth = step.requires.iter().map(|id| layers.get(id).copied().map_or(0, |d| d + 1)).max().unwrap_or(0);
					layers.insert(step.id.clone(), depth);
				}
			}
			ordered.sort_by(|left, right| {
				let (dl, dr) = (layers.get(&left.id).copied().unwrap_or(0), layers.get(&right.id).copied().unwrap_or(0));
				// The same number the STEPCOST line will carry: measured where it has been measured.
				let cl = history.step_seconds(&left.id, &model_hash).unwrap_or_else(|| cost.estimate(&history, &left.keys));
				let cr = history.step_seconds(&right.id, &model_hash).unwrap_or_else(|| cost.estimate(&history, &right.keys));
				dl.cmp(&dr).then(cl.partial_cmp(&cr).unwrap_or(std::cmp::Ordering::Equal)).then(left.id.cmp(&right.id))
			});
			for (index, step) in ordered.into_iter().enumerate() {
				println!("STEP\t{}\t{}\t{}\t{}\t{}", index, step.keys.len(), step.label, step.command, step.note.unwrap_or_default());
				// ITS OWN LINE, not a seventh field. The runner reads a STEP line into six names and
				// puts everything after the fifth tab into the last one, so a field appended there
				// would be glued onto the note. A new marker is skipped by a reader that does not
				// know it and read by one that does, which is what lets the scheduler arrive later
				// without a flag day.
				println!("STEPID\t{index}\t{}", step.id);
				// WHAT IT CANNOT START BEFORE, and WHAT IT IS EXPECTED TO COST. Both on their own
				// lines for the reason the id is: a reader that does not know a marker skips it, so
				// the runner can learn about them without a flag day.
				//
				// The cost is an ESTIMATE over the keys this step discharges, which is the only
				// number available before it has ever run. A budget is a sum of estimates and is not
				// a timeout: it decides what to START, never what to kill.
				for required in &step.requires {
					println!("STEPREQ\t{index}\t{required}");
				}
				// MEASURED IF IT HAS BEEN, ESTIMATED IF IT HAS NOT.
				//
				// `estimate` sums per-key costs, which for a merged step is the batching's own
				// arithmetic handed back as a prediction. A step has one duration; once it has been
				// run under this model, that duration is the answer and the estimate is only the
				// seed for a step nobody has timed yet.
				let measured = history.step_seconds(&step.id, &model_hash);
				println!("STEPCOST\t{index}\t{:.0}", measured.unwrap_or_else(|| cost.estimate(&history, &step.keys)));
				// HOW MANY GUEST SLOTS THIS STEP NEEDS AT ONCE - see `Step::guests`. Emitted only for
				// a step that needs more than one, because one is what the runner already assumes for
				// anything that boots and zero is what it assumes for everything else.
				if step.guests > 1 {
					println!("STEPGUESTS\t{index}\t{}", step.guests);
				}
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
				// cost of a run dominates so heavily - `CostModel::default` carries the measured
				// terms, and the per-test one is a fraction of a second against tens of seconds of
				// fixed cost on every target - that a selection worth 80% of the whole is
				// bookkeeping for nothing.
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
// The model's own gates, as a list rather than an exit status.
//
// Split out so a shadow record can say whether the model that made the comparison was consistent at
// the time. A comparison produced by a model failing its own checks is not evidence about the tree,
// and the record could not say which it was.
fn self_check_failures(model: &Model, report: bool) -> Vec<String> {
	let mut failures = Vec::new();

	// EVERY DECLARED CHANGE GROUP MATCHES SOMETHING, AND EVERY REQUIRED ONE IS DECLARED.
	//
	// `risk_class.required_groups` is a bar, and a bar naming a group that matches no path in the
	// tree is one nobody can meet - the same defect as a check that skips what it cannot read,
	// reached from the other side. A group whose paths have been renamed away stops matching
	// silently, and the subsystem it guards then becomes unprovable rather than unproven.
	let tracked: Vec<String> = verify_model::tracked_files(&model.repo_root).unwrap_or_default();
	for group in &model.registry.change_groups {
		let matches = group.paths.iter().any(|declared| tracked.iter().any(|path| path == declared || path.starts_with(&format!("{declared}/"))));
		if !matches {
			failures.push(format!("the change group `{}` names {} and no tracked file is under any of them - a group that matches nothing is a bar nobody can meet", group.name, group.paths.join(", ")));
		}
	}
	let declared: std::collections::BTreeSet<&str> = model.registry.change_groups.iter().map(|group| group.name.as_str()).collect();
	for risk in &model.registry.risk_classes {
		for required in &risk.required_groups {
			if !declared.contains(required.as_str()) {
				failures.push(format!("`{}` requires evidence from the change group `{required}`, which no `[[change_group]]` declares", risk.path));
			}
		}
		// AND THE PROSE AND THE FIELDS AGREE ABOUT THE ABI, which is the one of the four that reads
		// as a sentence and is checkable. `syscall` says "shadow-clean plus the ABI unchanged"; a
		// row saying that with `abi_unchanged = false` is two answers to one question.
		if risk.evidence.contains("ABI unchanged") != risk.abi_unchanged {
			failures.push(format!("`{}` says `{}` and sets abi_unchanged = {} - the sentence and the field disagree", risk.path, risk.evidence, risk.abi_unchanged));
		}
	}

	// EVERY JUDGING UNIVERSE HAS AN EVIDENCE PRODUCER, or the components it judges can never be
	// trusted whatever they accumulate elsewhere.
	//
	// `trusted_everywhere` requires `Trusted` in every universe that judges a component, so a
	// universe with no producer is a wall rather than a bar. That is exactly what `HostBuild` was
	// between the split that created it and the producer that answers it: 189 of the catalog's 192
	// components are judged by a build check, so 98% of the tree could accumulate any amount of
	// evidence and never arrive. Fail-closed, and not a steady state.
	//
	// Written as a self-check rather than fixed once, because the next universe added without a
	// producer arrives the same way this one did - as the right shape, with the other half missing.
	{
		let producers = verify_model::shadow::universes_with_producers();
		let mut walled: Vec<&'static str> = verify_model::catalog::all_judging_universes(&model.catalog).into_iter().filter(|universe| !producers.contains(universe)).map(|universe| universe.as_str()).collect();
		walled.sort_unstable();
		if !walled.is_empty() {
			failures.push(format!("{} universe(s) judge components and have no shadow producer, so nothing they judge can ever be TRUSTED:\n  {}", walled.len(), walled.join("\n  ")));
		}
	}

	// The one equality an exact selection rests on: the string the model puts in `TEST_SELECTION`
	// is the string the guest runner matches. The runner compares against the declaration's `id`,
	// so every kernel check id must BE a declared id - not derived from it, not prefixed, not
	// stripped.
	//
	// This check exists because that equality broke silently. `id` used to default to the function
	// name, so `kernel.{name}` and the runner's identity were the same string; making ids mandatory
	// and namespacing them moved one side and nothing was watching the other. Every scoped kernel
	// run then handed the guest names it could not find, and an unknown id is a hard failure - loud,
	// but only for whoever ran one, and nothing scoped had run since.
	{
		let declared: BTreeSet<&str> = model.kernel_tests.tests.iter().map(|test| test.id.as_str()).collect();
		let mut wrong: Vec<String> = Vec::new();
		for check in model.catalog.checks.iter().filter(|check| check.kind == verify_model::catalog::CheckKind::KernelTest) {
			if !declared.contains(check.id.as_str()) {
				wrong.push(check.id.clone());
			}
		}
		if !wrong.is_empty() {
			failures.push(format!("{} kernel check id(s) are not a test's declared `id`, so an exact selection would name tests the guest cannot match:\n  {}", wrong.len(), wrong.join("\n  ")));
		}
		let mut empty: Vec<&str> = model.kernel_tests.tests.iter().filter(|test| test.id.is_empty()).map(|test| test.name.as_str()).collect();
		empty.sort_unstable();
		if !empty.is_empty() {
			failures.push(format!("{} kernel test(s) carry no id: {}", empty.len(), empty.join(", ")));
		}
		if declared.len() != model.kernel_tests.tests.len() {
			failures.push(format!("{} kernel tests share {} distinct ids - an id names exactly one test or it names nothing", model.kernel_tests.tests.len(), declared.len()));
		}
	}

	// The suite's budget against the suite's measured cost.
	//
	// `test-kernel.sh` carries a per-architecture `FULL_TIMEOUT` and the model carries what a full
	// run of that architecture costs. Nothing compared them, so the budget drifted behind the work
	// twice: once before 2026-08-06 (fixed by raising the number) and again by 2026-08-10, when a
	// suite grown from 150 tests to 217 met a number that had not moved. A run then fails at the
	// wall - and a timeout that means "the suite got bigger" is indistinguishable from one that
	// means "something hung", which is the confusion the per-test watchdog exists to end.
	//
	// Comparing them here makes outgrowing the budget a fact the model states in a second, rather
	// than one discovered forty-five minutes into a sweep.
	{
		let script = model.repo_root.join("src/harness/test-kernel.sh");
		match std::fs::read_to_string(&script) {
			Ok(text) => {
				let cost = verify_model::history::CostModel::default();
				for architecture in ["x86_64", "aarch64", "riscv64"] {
					let Some(budget) = full_timeout_seconds(&text, architecture) else {
						failures.push(format!("test-kernel.sh names no FULL_TIMEOUT for {architecture}, so nothing bounds its suite"));
						continue;
					};
					// FIXED PLUS THE TESTS, which is what a whole suite costs.
					//
					// This read `fixed_seconds` alone, and that was right only while the fixed term
					// WAS a whole-suite figure - the defect the 2026-08-12 measurement removed. With
					// it meaning startup cost, reading it alone compares a fifteen-minute budget
					// against a ten-minute boot and concludes the suite fits however far it has
					// outgrown its wall.
					let tests = model.kernel_tests.tests.iter().filter(|test| test.architectures.iter().any(|declared| declared == architecture)).count();
					let estimate = cost.full_suite_seconds(architecture, "test-guest", tests);
					if estimate <= 0.0 {
						continue;
					}
					// A fifth over the estimate. Less is a run that fails on a slow day rather than
					// on a defect, which is the same false signal from the other side.
					if budget < estimate * 1.2 {
						failures.push(format!("test-kernel.sh gives {architecture} {budget:.0} s and a full suite there is measured at {estimate:.0} s - a budget under {:.0} s fails at the wall and reads exactly like a hang", estimate * 1.2));
					}
				}
			}
			Err(error) => failures.push(format!("{}: {error}", script.display())),
		}
	}

	// AND THE HISTORY'S OWN CONTRADICTION OF THOSE CONSTANTS.
	//
	// `record_step` used to clamp a negative residual to zero, so a run that came in UNDER its fixed
	// term produced no evidence at all - the one observation that says "this constant is too high"
	// was the one being thrown away, on exactly the two targets whose fixed terms decide whether a
	// selection can stay scoped. The overshoot is recorded now, and this is where it becomes a
	// statement rather than a number in a file.
	//
	// A tenth is the margin: a full suite is not a constant-time thing and a run that beats the
	// estimate slightly is ordinary. A run that beats it by more than that means the term is a
	// whole-suite figure sitting in a field that means startup cost, which is the defect.
	{
		let cost = verify_model::history::CostModel::default();
		match verify_model::history::History::load(&model.repo_root) {
			Ok(history) => {
				for (pair, overshoot) in &history.fixed_overshoot {
					let Some((architecture, environment)) = pair.split_once('/') else { continue };
					let fixed = cost.fixed_seconds.get(&(architecture.to_string(), environment.to_string())).copied().unwrap_or(0.0);
					if fixed > 0.0 && *overshoot > fixed * 0.1 {
						failures.push(format!("the fixed cost for {pair} is {fixed:.0} s and a measured run came in {overshoot:.0} s under it - the term is a whole-suite figure in a field that means startup cost, so every scoped selection there widens to the full suite"));
					}
				}
			}
			// A history that will not load is not a reason to abandon the rest of the self-check,
			// and an absent one is the ordinary state of a fresh tree.
			Err(_) => {}
		}
	}

	// Not `?`: this returns a failure list rather than a `Result`, and a check that cannot be made
	// is itself a failure rather than a reason to abandon the rest of them.
	match model.unowned_paths() {
		Ok(unowned) if !unowned.is_empty() => failures.push(format!("{} tracked file(s) are owned by no component and are not declared non-code:\n  {}", unowned.len(), unowned.join("\n  "))),
		Ok(_) => {}
		Err(error) => failures.push(format!("could not check path ownership: {error}")),
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
			Some(_) => {
				if report {
					println!("verify-model: host suite '{}' is excluded - {}", rule.crate_name, rule.reason);
				}
			}
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
		for component in verify_model::kerneltests::unreachable_covers(test, &touched, &model.graph, &model.staged) {
			unreachable.push(format!("{} covers '{component}', which its body cannot reach - it launches nothing that leads there", test.id));
		}
		for component in verify_model::kerneltests::launched_but_not_covered(test, &touched, &model.graph) {
			uncovered.push(format!("{} launches {component} and does not claim to cover it", test.id));
		}
	}
	failures.extend(unreachable);
	if !uncovered.is_empty() {
		// A REPORT, never a failure. Launching something is not asserting anything about it.
		if report {
			println!("verify-model: {} test(s) reach a program they do not claim to cover - read, do not fix mechanically:", uncovered.len());
		}
		for line in uncovered.iter().take(8) {
			if report {
				println!("    {line}");
			}
		}
		if uncovered.len() > 8 {
			if report {
				println!("    ... and {} more", uncovered.len() - 8);
			}
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

	// The permission fixture's twelve consumers against the map that classifies them.
	// The classification decides which cached result a consumer may use, so a consumer nobody
	// classified, an entry whose test was deleted, a duplicate or a drifted class is a real defect
	// and not a bookkeeping detail.
	{
		let map_path = model.repo_root.join("src/kernel/tests.rs");
		let consumer_path = model.repo_root.join("src/kernel/test_suites/applications.rs");
		match (std::fs::read_to_string(&map_path), std::fs::read_to_string(&consumer_path)) {
			(Ok(map_text), Ok(consumer_text)) => failures.extend(verify_model::kerneltests::audit_permission_cohort(&map_text, &consumer_text)),
			(Err(error), _) => failures.push(format!("{}: {error}", map_path.display())),
			(_, Err(error)) => failures.push(format!("{}: {error}", consumer_path.display())),
		}
	}

	// A risk class naming a path that no longer exists is a plan for a tree that has moved on.
	for rule in &model.registry.risk_classes {
		if !model.repo_root.join(&rule.path).exists() {
			failures.push(format!("risk class names '{}', which is not in the tree", rule.path));
		}
	}
	let sensitive: Vec<(&String, &verify_model::archrisk::Risk)> = model.arch_risk.iter().filter(|(_, risk)| risk.any_target || !risk.targets.is_empty()).collect();
	if !sensitive.is_empty() {
		if report {
			println!("verify-model: {} of {} components are architecture-sensitive by mechanical scan - a change to one boots more than the default target:", sensitive.len(), model.graph.components.len());
		}
		for (component, risk) in sensitive.iter().take(12) {
			if report {
				println!("    {:<34} {:<22} {}", component, if risk.any_target { String::from("all targets") } else { risk.targets.iter().cloned().collect::<Vec<_>>().join(",") }, risk.evidence.first().map(String::as_str).unwrap_or(""));
			}
		}
		if sensitive.len() > 12 {
			if report {
				println!("    ... and {} more", sensitive.len() - 12);
			}
		}
	}

	if !model.registry.risk_classes.is_empty() {
		if report {
			println!("verify-model: {} kernel subsystem(s) are declared narrowable and NOT narrowed - they still select everything:", model.registry.risk_classes.len());
		}
		for rule in &model.registry.risk_classes {
			if report {
				println!("    {:<28} {:<14} needs: {}", rule.path, rule.class, rule.evidence);
			}
		}
	}

	if !model.kernel_tests.missing_targets.is_empty() {
		if report {
			println!("verify-model: no kernel test binary for {} - those targets cannot be scoped and will be booted whole", model.kernel_tests.missing_targets.join(", "));
		}
	}

	failures
}

fn self_check(model: &Model) -> Result<ExitCode, String> {
	let failures = self_check_failures(model, true);
	if failures.is_empty() {
		println!("verify-model: model is consistent");
		println!("  {} crates, {} components, {} edges", model.crates.len(), model.graph.components.len(), model.graph.edges.len());
		println!("  {} checks, {} runnable keys", model.catalog.checks.len(), model.catalog.checks.iter().map(|check| check.variants.len()).sum::<usize>());
		println!("  {} kernel tests, {} without a covers declaration", model.kernel_tests.tests.len(), model.kernel_tests.unannotated);
		if !model.kernel_tests.declared_not_built.is_empty() {
			println!("  {} declared and in no built suite (so not selectable, and not running): {}", model.kernel_tests.declared_not_built.len(), model.kernel_tests.declared_not_built.join(", "));
		}
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

// `FULL_TIMEOUT=90m` for one architecture's branch of `test-kernel.sh`.
//
// A line walk with the current `case` label carried along, rather than a split on the label: the
// script has more than one `case` over the same architecture names, and splitting found the first
// of them and read a value out of the wrong block. The last assignment under a label wins, which is
// what the shell itself does.
fn full_timeout_seconds(text: &str, architecture: &str) -> Option<f64> {
	let mut label: Option<&str> = None;
	let mut found = None;
	for line in text.lines() {
		let trimmed = line.trim();
		if let Some(name) = trimmed.strip_suffix(')')
			&& !name.is_empty()
			&& name.chars().all(|character| character.is_ascii_alphanumeric() || character == '_')
		{
			label = Some(name);
			continue;
		}
		// `;;` ends a branch. Without this the default `*)` arm - whose label the check above
		// rejects, being punctuation - left the previous label standing, and the default's own
		// 15-minute timeout was read as riscv64's.
		if trimmed == ";;" {
			label = None;
			continue;
		}
		if label == Some(architecture)
			&& let Some(value) = trimmed.strip_prefix("FULL_TIMEOUT=")
		{
			let (number, scale) = match value.strip_suffix('m') {
				Some(minutes) => (minutes, 60.0),
				None => (value.strip_suffix('s').unwrap_or(value), 1.0),
			};
			found = number.parse::<f64>().ok().map(|parsed| parsed * scale);
		}
	}
	found
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
