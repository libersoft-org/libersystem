// Changed paths in, one plan out.
//
//   changed paths
//         -> component ownership
//         -> reverse dependency closure
//         -> affected components
//         -> checks whose `covers` intersects them
//         -> exact PlanItemKeys
//         -> architecture policy
//
// Every default errs toward running more. An unrecognised path selects everything; a component on
// the selects-everything list selects everything; a check with no declared coverage is selected
// rather than skipped; and a target that cannot be enumerated is booted rather than assumed clean.
// A selector that fails and selects NOTHING must be impossible, not unlikely.

use crate::catalog::{Catalog, Check, CheckKind, Environment, Variant};
use crate::graph::{Edge, Graph};
use crate::ownership::{Owner, Ownership};
use crate::registry::{ARCHITECTURES, Registry, prefix_match};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

// The unit of work, and the key every history is stored under.
//
// Each field earns its place by being a way the same check can differ. The test ID alone loses the
// architecture; adding the architecture still loses the environment, so a regression fixture for
// the dev agent could confirm the right test on the right target while never checking that the
// planner chose the dev guest; and all three still lose the configuration, which is not academic
// here - `proto` must be host-tested under default features AND under `shared-image`, because
// those two builds do not contain the same dependencies.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct PlanItemKey {
	pub check: String,
	pub architecture: String,
	pub environment: Environment,
	pub configuration: String,
}

impl PlanItemKey {
	pub fn display(&self) -> String {
		format!("{} / {} / {} / {}", self.check, self.architecture, self.environment.as_str(), self.configuration)
	}
}

#[derive(Clone, Debug, Serialize)]
pub struct PlanItem {
	#[serde(flatten)]
	pub key: PlanItemKey,
	pub kind: CheckKind,
	pub command: String,
	pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct PathVerdict {
	pub path: String,
	pub outcome: String,
	pub detail: String,
}

// The selector's decision ABOUT each component: what it selected for it, how it reached it, and
// which architectures that implied - digested, so a record can carry one line per component instead
// of the whole plan.
//
// `Check::covers` is the mapping. A component the plan selected nothing for still gets an entry, so
// "nothing was selected for audio" is itself a decision two runs can agree or disagree about.
fn decisions(catalog: &crate::catalog::Catalog, items: &[PlanItem], edge_kinds: &BTreeMap<String, Vec<String>>, built: &[String], booted: &[String], components: &[String]) -> Vec<String> {
	let mut out: Vec<String> = Vec::new();
	for component in components {
		let mut parts: Vec<String> = Vec::new();
		for item in items {
			let covers = catalog.checks.iter().find(|check| check.id == item.key.check).map(|check| check.covers.contains(component)).unwrap_or(false);
			if covers {
				parts.push(item.key.display());
			}
		}
		parts.sort();
		parts.dedup();
		// THIS COMPONENT'S EDGES. The global list put the neighbour back into the decision the
		// digest exists to isolate: `audio + neighbour-A` and `audio + neighbour-B` reached through
		// different edge kinds gave audio two different digests, so the two counted as two pieces of
		// evidence about audio again.
		parts.push(format!("edges={}", edge_kinds.get(component).map(|kinds| kinds.join(",")).unwrap_or_default()));
		parts.push(format!("built={}", built.join(",")));
		parts.push(format!("booted={}", booted.join(",")));
		out.push(format!("{component}\t{:016x}", digest(&parts.join("\n"))));
	}
	out.sort();
	out.dedup();
	out
}

// A short content digest, enough to tell two decisions apart. Not cryptographic and not pretending
// to be: nothing here defends against a chosen collision, and the alternative - storing the whole
// decision in every record - makes the evidence log unreadable.
fn digest(text: &str) -> u64 {
	let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
	for byte in text.as_bytes() {
		hash ^= *byte as u64;
		hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
	}
	hash
}

#[derive(Clone, Debug, Serialize)]
pub struct Plan {
	// Whether this is the whole thing. Recorded rather than inferred from the item count, because
	// "everything happened to be selected" and "the selector gave up and asked for everything" are
	// different states and only one of them is a fault.
	pub full: bool,
	pub full_reasons: Vec<String>,
	pub paths: Vec<PathVerdict>,
	pub changed_components: Vec<String>,
	pub affected_components: Vec<String>,
	pub architectures_built: Vec<String>,
	pub architectures_booted: Vec<String>,
	pub items: Vec<PlanItem>,
	// WHAT THE SELECTOR DECIDED FOR EACH COMPONENT, as `component\tdigest` pairs.
	//
	// Distinctness in the trust store used to be keyed on the CHANGE SET, and the criterion it
	// serves is five genuinely different changes. On one unchanged tree, `audio + neighbour-0` ..
	// `audio + neighbour-4` are five different change sets whose decision about AUDIO may be
	// byte-identical - and the neighbour is free, while the audio half is what the certificate is
	// about. So the record carries what was decided about each component, and the store keys on that.
	//
	// Computed here because this is where the catalog is: `Check::covers` is what says which
	// component a selected item is about.
	pub component_decisions: Vec<String>,
	// WHICH EDGES the selection walked to reach what it selected.
	//
	// Recorded so a shadow record can say what it is evidence ABOUT. `Store::evaluate` counts clean
	// runs and clean architectures and nothing else, because the criteria the design asked for -
	// every change class exercised, every edge kind exercised - cannot be written before there are
	// records to grade. That is a good reason not to write the policy yet and no reason to keep
	// discarding the data: every run that happens before this lands is a record that can never be
	// graded against those criteria when they are written.
	pub edge_kinds: Vec<String>,
	// THE EDGES WALKED OUT OF EACH CHANGED COMPONENT, which is the per-component half of the field
	// above.
	//
	// `edge_kinds` is the union over the whole plan, and a certificate is about one component: a
	// change touching `audio` and `term` gave audio's record every edge kind the walk from TERM
	// used. What audio's evidence actually covers is the edges the selector walked out of AUDIO, so
	// the reach paths are attributed to the seed each one starts from.
	pub component_edge_kinds: BTreeMap<String, Vec<String>>,
	// Set when every changed path is declared non-code. Distinct from an empty plan for any other
	// reason, which cannot happen: this is the only way out with nothing to run.
	pub nothing_to_do: bool,
	pub warnings: Vec<String>,
}

pub struct Planner<'a> {
	pub registry: &'a Registry,
	pub graph: &'a Graph,
	pub ownership: &'a Ownership<'a>,
	pub catalog: &'a Catalog,
	// Targets whose kernel test list could not be read. They are booted whole rather than scoped,
	// because an empty enumeration and a target with no affected tests look identical.
	pub unenumerated_targets: Vec<String>,
	// What the mechanical scan found. It only ever WIDENS the boot set the policy table produced -
	// a component with no marker is a candidate for neutral, never a proof of it.
	pub arch_risk: BTreeMap<String, crate::archrisk::Risk>,
	// Measured per-key durations, for the cost escalation. Empty on a checkout that has run nothing,
	// which still leaves the fixed per-boot terms - and those are what dominate.
	pub history: crate::history::History,
}

impl<'a> Planner<'a> {
	// One place builds a planner.
	//
	// It was constructed by hand at five call sites, each repeating the same five fields, which is
	// how a sixth field gets added and one of them silently keeps the old behaviour. The ownership
	// has to be passed in because it borrows the model's crates and cannot be created here without
	// outliving the borrow.
	pub fn for_model(model: &'a crate::Model, ownership: &'a Ownership<'a>) -> Self {
		Planner { registry: &model.registry, graph: &model.graph, ownership, catalog: &model.catalog, unenumerated_targets: model.kernel_tests.missing_targets.clone(), arch_risk: model.arch_risk.clone(), history: crate::history::History::load(&model.repo_root).unwrap_or_default() }
	}

	pub fn plan(&self, changed: &[String]) -> Plan {
		let mut full = false;
		let mut full_reasons: Vec<String> = Vec::new();
		let mut paths = Vec::new();
		let mut seeds: BTreeSet<String> = BTreeSet::new();
		let mut warnings: Vec<String> = Vec::new();
		let mut non_code = 0usize;

		let everything: BTreeMap<&str, &str> = self.registry.selects_everything.iter().map(|rule| (rule.component.as_str(), rule.reason.as_str())).collect();

		for path in changed {
			match self.ownership.owner(path) {
				Owner::NonCode { reason } => {
					non_code += 1;
					paths.push(PathVerdict { path: path.clone(), outcome: String::from("not code"), detail: reason });
				}
				Owner::Unknown => {
					full = true;
					full_reasons.push(format!("'{path}' is owned by no component; unknown reach is tested with everything"));
					paths.push(PathVerdict { path: path.clone(), outcome: String::from("unknown"), detail: String::from("no ownership rule and no crate contains it") });
				}
				Owner::Component { component, rule } => {
					// The escalation applies to a program as well as to the crate it is built from.
					//
					// `bin.mkpackages` and `mkpackages` are one thing wearing two names here - the
					// program's own source file is a longer prefix than its crate directory, so
					// `src/tools/mkpackages/src/main.rs` resolves to the bin. Checking only the
					// declared name let the packager whose output IS the system volume plan a scoped
					// run, which is the exact defect an earlier review found in `lib.sh` and this
					// model was supposed to have retired. Derived rather than declared twice: a bin
					// is escalated by whatever escalates the crate it links.
					let escalates = everything.get(component.as_str()).map(|reason| (component.clone(), (*reason).to_string())).or_else(|| self.graph.edges_from(&component).into_iter().filter(|edge| edge.kind == "link.static").find_map(|edge| everything.get(edge.to.as_str()).map(|reason| (edge.to.clone(), (*reason).to_string()))).filter(|_| component.starts_with("bin.")));
					if let Some((named, reason)) = escalates {
						full = true;
						full_reasons.push(if named == component { format!("'{path}' belongs to {component}: {reason}") } else { format!("'{path}' is {component}, built from {named}: {reason}") });
					}
					// A changed component the graph has never heard of is a rename that got half
					// done. The closure would return the seed alone and quietly reach nothing.
					if !self.graph.contains(&component) {
						full = true;
						full_reasons.push(format!("'{path}' belongs to {component}, which is in no edge of the graph - the closure from it would reach nothing"));
					}
					paths.push(PathVerdict { path: path.clone(), outcome: component.clone(), detail: format!("matched '{rule}'") });
					seeds.insert(component);
				}
			}
		}

		let nothing_to_do = !full && seeds.is_empty() && non_code == changed.len() && !changed.is_empty();
		let reached = self.graph.affected_with_reasons(&seeds);
		let affected: BTreeSet<String> = reached.keys().cloned().collect();
		// The edge kinds this walk actually traversed, so a shadow record can say which parts of the
		// graph its evidence covers. A component reached through `link.dynamic` proves nothing about
		// `generation.build`.
		let edge_kinds: Vec<String> = reached.values().flatten().map(|edge| edge.kind.clone()).collect::<BTreeSet<String>>().into_iter().collect();
		// And the same walk attributed to the seed it started from. Every reach path is a chain from
		// a changed component outwards, so `path[0].to` names the seed - a component reached with an
		// empty path IS a seed, and contributes nothing to anyone else.
		let mut per_component: BTreeMap<String, BTreeSet<String>> = seeds.iter().map(|seed| (seed.clone(), BTreeSet::new())).collect();
		for edges in reached.values() {
			let Some(first) = edges.first() else { continue };
			let entry = per_component.entry(first.to.clone()).or_default();
			entry.extend(edges.iter().map(|edge| edge.kind.clone()));
		}
		let component_edge_kinds: BTreeMap<String, Vec<String>> = per_component.into_iter().map(|(component, kinds)| (component, kinds.into_iter().collect())).collect();
		// How each one was reached, which decides what has to happen because of it.
		let reach = self.graph.affected_by_reach(&seeds);

		// Architecture policy: the union over changed paths, because a change touching both an
		// x86_64 tree and a riscv64 one has to answer for both.
		let (mut built, mut booted) = (BTreeSet::new(), BTreeSet::new());
		// Components whose architecture answer came from an explicit rule rather than the default.
		let mut declared_architecture: BTreeSet<String> = BTreeSet::new();
		if full || changed.is_empty() {
			built.extend(ARCHITECTURES.iter().map(|architecture| (*architecture).to_string()));
			booted.extend(ARCHITECTURES.iter().map(|architecture| (*architecture).to_string()));
		} else {
			for verdict in &paths {
				if verdict.outcome == "not code" {
					continue;
				}
				let (rule_build, rule_boot, rule_path) = self.architecture_rule(&verdict.path);
				built.extend(rule_build);
				booted.extend(rule_boot);
				if !rule_path.is_empty() {
					// An explicit rule outranks the scan. `src/kernel/arch/aarch64` contains `asm!`
					// on every line that matters, and the scan can only conclude "all targets" from
					// that - while the policy table knows the precise answer is aarch64. Letting the
					// scan widen here would spend 6104 s of riscv64 on an aarch64-only change, which
					// is the cost this milestone exists to avoid rather than to incur.
					declared_architecture.insert(verdict.outcome.clone());
				}
			}
		}
		// The mechanical classifier, applied to the components that actually CHANGED.
		//
		// A marker proves target-dependence; its absence proves nothing, so this can add targets and
		// never remove them. `volume-client-provider` is the case it exists for: three `global_asm!`
		// branches, ordinary-userspace policy, and a riscv64 branch that compiles everywhere and is
		// only exercised on riscv64.
		if !full {
			for component in &seeds {
				if declared_architecture.contains(component) {
					continue;
				}
				let Some(risk) = self.arch_risk.get(component) else { continue };
				let widen = risk.boot_targets();
				let added: Vec<String> = widen.iter().filter(|target| !booted.contains(*target)).cloned().collect();
				if added.is_empty() {
					continue;
				}
				let why = risk.evidence.first().cloned().unwrap_or_else(|| String::from("a target-specific marker"));
				warnings.push(format!("{component} is architecture-sensitive ({why}), so {} is booted as well", added.join(", ")));
				booted.extend(widen.iter().cloned());
				built.extend(widen);
			}
		}

		// A target whose test binary could not be read has NO catalog variants for that target, and
		// a check with no variants contributes no items - so the plan would quietly boot nothing
		// there while its header claimed the target was in scope. A missing enumeration and an
		// empty one are indistinguishable from here, which is exactly what fail-open is for.
		for target in &self.unenumerated_targets {
			if booted.contains(target) {
				full = true;
				full_reasons.push(format!("no {target} kernel test binary has been built, so the model cannot enumerate that target's tests and no scoped answer for it can be trusted. This is the expected state of a fresh checkout. Build once - ./build.sh --arch {target} - and plan again for a scoped answer; running this FULL plan works too and is simply the expensive way round."));
			}
		}
		if full {
			built.extend(ARCHITECTURES.iter().map(|architecture| (*architecture).to_string()));
			booted.extend(ARCHITECTURES.iter().map(|architecture| (*architecture).to_string()));
		}

		let mut items = Vec::new();
		for check in &self.catalog.checks {
			let selection = self.select(check, &affected, &reached, &reach, full);
			let Some(reason) = selection else { continue };
			for variant in &check.variants {
				if !self.variant_applies(check, variant, &built, &booted) {
					continue;
				}
				items.push(PlanItem { key: PlanItemKey { check: check.id.clone(), architecture: variant.architecture.clone(), environment: variant.environment.clone(), configuration: variant.configuration.clone() }, kind: check.kind, command: check.command.replace("{arch}", &variant.architecture), reason: reason.clone() });
			}
		}
		items.sort_by(|left, right| left.key.cmp(&right.key));

		// Escalate on measured COST, per environment, rather than on a count of keys.
		//
		// It compared `selected / whole` over all keys, which treats twenty host keys and twenty
		// riscv64 guest keys as the same quantity when they differ by two orders of magnitude in
		// wall-clock. `CostModel::estimate` already answers the real question and nothing asked it.
		//
		// PER PAIR, and that is the substantive change. The fixed term is what dominates - a boot is
		// paid once however many tests run in it - so the question is never "should the whole plan
		// become FULL" but "having decided to boot riscv64 at all, is running its 4 selected tests
		// meaningfully cheaper than running all 205". Usually it is not, and taking the rest costs
		// almost nothing and gives the shadow record something complete to compare against. A global
		// rule cannot express that: it would answer the riscv64 question by also booting the two
		// targets nobody asked about.
		if !full && !items.is_empty() {
			let cost = crate::history::CostModel::default();
			let history = &self.history;
			let mut pairs: BTreeSet<(String, crate::catalog::Environment)> = BTreeSet::new();
			for item in &items {
				pairs.insert((item.key.architecture.clone(), item.key.environment.clone()));
			}
			let mut widened: Vec<PlanItem> = Vec::new();
			for (architecture, environment) in &pairs {
				let mine: Vec<PlanItemKey> = items.iter().filter(|item| &item.key.architecture == architecture && &item.key.environment == environment).map(|item| item.key.clone()).collect();
				let mut whole: Vec<PlanItem> = Vec::new();
				for check in &self.catalog.checks {
					for variant in &check.variants {
						if &variant.architecture != architecture || &variant.environment != environment || !self.variant_applies(check, variant, &built, &booted) {
							continue;
						}
						whole.push(PlanItem { key: PlanItemKey { check: check.id.clone(), architecture: variant.architecture.clone(), environment: variant.environment.clone(), configuration: variant.configuration.clone() }, kind: check.kind, command: check.command.replace("{arch}", &variant.architecture), reason: format!("this {architecture} {} run is within a tenth of the cost of all of it, so it runs all of it", environment.as_str()) });
					}
				}
				if whole.len() <= mine.len() {
					continue;
				}
				// Only where there is a BOOT to amortise. A host pair has no fixed term - nothing is
				// started, nothing is imaged, each check is its own cost - so widening it buys no
				// saving at all and simply runs more. The first version of this rule did not make
				// that distinction and pulled a shipping kernel build into a codec change, which is
				// the opposite of what the milestone is for.
				if cost.fixed_seconds.get(&(architecture.clone(), environment.as_str().to_string())).copied().unwrap_or(0.0) <= 0.0 {
					continue;
				}
				let scoped_cost = cost.estimate(history, &mine);
				let whole_cost = cost.estimate(history, &whole.iter().map(|item| item.key.clone()).collect::<Vec<_>>());
				if whole_cost > 0.0 && scoped_cost / whole_cost > 0.9 {
					warnings.push(format!("the {architecture} {} selection costs an estimated {scoped_cost:.0} s against {whole_cost:.0} s for all {} of its keys, so it takes all of them", environment.as_str(), whole.len()));
					widened.extend(whole);
				}
			}
			if !widened.is_empty() {
				let already: BTreeSet<PlanItemKey> = items.iter().map(|item| item.key.clone()).collect();
				items.extend(widened.into_iter().filter(|item| !already.contains(&item.key)));
				items.sort_by(|left, right| left.key.cmp(&right.key));
			}
		}

		// The one outcome that must never happen quietly. Something was changed, it was understood,
		// and the answer was to run nothing at all - that is a selector fault, not a clean bill.
		if items.is_empty() && !nothing_to_do && !changed.is_empty() {
			warnings.push(String::from("the selection was empty for a change that is not declared non-code; escalating to FULL"));
			return self.full_plan(paths, seeds, affected, vec![String::from("an empty selection for a change with owned paths")], warnings);
		}

		let changed_components: Vec<String> = seeds.into_iter().collect();
		let affected_components: Vec<String> = affected.into_iter().collect();
		let architectures_built: Vec<String> = built.into_iter().collect();
		let architectures_booted: Vec<String> = booted.into_iter().collect();
		let mut components: Vec<String> = changed_components.clone();
		components.extend(affected_components.iter().cloned());
		components.sort();
		components.dedup();
		let component_decisions = decisions(self.catalog, &items, &component_edge_kinds, &architectures_built, &architectures_booted, &components);
		Plan { full, full_reasons, paths, changed_components, affected_components, architectures_built, architectures_booted, items, component_decisions, edge_kinds, component_edge_kinds, nothing_to_do, warnings }
	}

	// Why this check is in the plan, or None for "it is not".
	fn select(&self, check: &Check, affected: &BTreeSet<String>, reached: &BTreeMap<String, Vec<Edge>>, reach: &BTreeMap<String, crate::graph::Reach>, full: bool) -> Option<String> {
		if full {
			return Some(String::from("the plan is FULL"));
		}
		// No declared coverage means unknown coverage. A check that cannot say what it would catch
		// is run, not skipped - and for the kernel suite that is the honest state today, which is
		// why annotating `covers` is what makes the suite scopeable rather than what makes it safe.
		if check.covers.is_empty() {
			return Some(String::from("no covers declaration, so its reach is unknown"));
		}
		let hit = check.covers.iter().find(|component| affected.contains(*component))?;
		// A component reached ONLY through a dev-dependency has not changed what it ships - the
		// change is in something its tests are built from. So its tests run and its shipping build
		// does not, which is the difference between `flac` costing a codec suite and `flac` costing
		// a kernel rebuild plus every gate that covers the kernel.
		// A dev edge means "this component's tests are built from the changed thing", so its own host
		// suite runs. It does NOT mean the guest behaves differently, so a kernel test covering the
		// kernel stays out: the kernel dev-depends on eleven codecs, and `frame_alloc_distinct` has
		// nothing to say about any of them. A kernel test that DOES assert on a codec names it in
		// `covers`, and that codec is reached as a product.
		if reach.get(hit) == Some(&crate::graph::Reach::TestBuild) && !matches!(check.kind, CheckKind::HostSuite) {
			// Unless something else it covers was reached properly.
			let product = check.covers.iter().find(|component| reach.get(*component) == Some(&crate::graph::Reach::Product))?;
			return Some(format!("covers {product}, which changed"));
		}
		Some(match reached.get(hit) {
			Some(path) if !path.is_empty() => format!("covers {hit}, reached by {}", describe(path)),
			_ => format!("covers {hit}, which changed"),
		})
	}

	// Builds answer to the BUILD set, guests to the BOOT set. Conflating them is how a target ends
	// up booted from an image that was never made, and how a compile-only regression on a target
	// nobody boots goes unnoticed.
	fn variant_applies(&self, check: &Check, variant: &Variant, built: &BTreeSet<String>, booted: &BTreeSet<String>) -> bool {
		if variant.architecture == "host" {
			return true;
		}
		match check.kind {
			CheckKind::Build => built.contains(&variant.architecture),
			_ => booted.contains(&variant.architecture),
		}
	}

	// Returns the matched rule's PATH as well, because an explicit rule is a more informed answer
	// than the mechanical scan and must not be widened by it.
	fn architecture_rule(&self, path: &str) -> (Vec<String>, Vec<String>, String) {
		let mut best: Option<(usize, &crate::registry::ArchitectureRule)> = None;
		for rule in &self.registry.architecture {
			if let Some(len) = prefix_match(&rule.path, path)
				&& best.is_none_or(|(best_len, _)| len > best_len)
			{
				best = Some((len, rule));
			}
		}
		match best {
			Some((_, rule)) => (rule.build.clone(), rule.boot.clone(), rule.path.clone()),
			// Unreachable while the registry validates a default rule exists, and still handled:
			// the answer to "no architecture answer" is every architecture.
			None => (ARCHITECTURES.iter().map(|architecture| (*architecture).to_string()).collect(), ARCHITECTURES.iter().map(|architecture| (*architecture).to_string()).collect(), String::new()),
		}
	}

	pub fn full_plan(&self, paths: Vec<PathVerdict>, seeds: BTreeSet<String>, affected: BTreeSet<String>, reasons: Vec<String>, warnings: Vec<String>) -> Plan {
		let all: Vec<String> = ARCHITECTURES.iter().map(|architecture| (*architecture).to_string()).collect();
		let mut items = Vec::new();
		for check in &self.catalog.checks {
			for variant in &check.variants {
				items.push(PlanItem { key: PlanItemKey { check: check.id.clone(), architecture: variant.architecture.clone(), environment: variant.environment.clone(), configuration: variant.configuration.clone() }, kind: check.kind, command: check.command.replace("{arch}", &variant.architecture), reason: String::from("the plan is FULL") });
			}
		}
		items.sort_by(|left, right| left.key.cmp(&right.key));
		let changed_components: Vec<String> = seeds.into_iter().collect();
		let affected_components: Vec<String> = affected.into_iter().collect();
		let mut components: Vec<String> = changed_components.clone();
		components.extend(affected_components.iter().cloned());
		components.sort();
		components.dedup();
		let component_decisions = decisions(self.catalog, &items, &BTreeMap::new(), &all, &all, &components);
		Plan { full: true, full_reasons: reasons, paths, changed_components, affected_components, architectures_built: all.clone(), architectures_booted: all, items, component_decisions, edge_kinds: Vec::new(), component_edge_kinds: BTreeMap::new(), nothing_to_do: false, warnings }
	}
}

fn describe(path: &[Edge]) -> String {
	let mut text = String::new();
	for (index, edge) in path.iter().enumerate() {
		if index > 0 {
			text.push(' ');
		}
		text.push_str(&format!("{} -{}-> {}", edge.from, edge.kind, edge.to));
	}
	text
}
