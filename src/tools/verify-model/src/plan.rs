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
}

impl Planner<'_> {
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

		// Architecture policy: the union over changed paths, because a change touching both an
		// x86_64 tree and a riscv64 one has to answer for both.
		let (mut built, mut booted) = (BTreeSet::new(), BTreeSet::new());
		if full || changed.is_empty() {
			built.extend(ARCHITECTURES.iter().map(|architecture| (*architecture).to_string()));
			booted.extend(ARCHITECTURES.iter().map(|architecture| (*architecture).to_string()));
		} else {
			for verdict in &paths {
				if verdict.outcome == "not code" {
					continue;
				}
				let rule = self.architecture_rule(&verdict.path);
				built.extend(rule.0);
				booted.extend(rule.1);
			}
		}
		// A target whose test binary could not be read has NO catalog variants for that target, and
		// a check with no variants contributes no items - so the plan would quietly boot nothing
		// there while its header claimed the target was in scope. A missing enumeration and an
		// empty one are indistinguishable from here, which is exactly what fail-open is for.
		for target in &self.unenumerated_targets {
			if booted.contains(target) {
				full = true;
				full_reasons.push(format!("the {target} kernel test list could not be read, so nothing scoped for it can be trusted - build it first: ./build.sh --arch {target}"));
			}
		}
		if full {
			built.extend(ARCHITECTURES.iter().map(|architecture| (*architecture).to_string()));
			booted.extend(ARCHITECTURES.iter().map(|architecture| (*architecture).to_string()));
		}

		let mut items = Vec::new();
		for check in &self.catalog.checks {
			let selection = self.select(check, &affected, &reached, full);
			let Some(reason) = selection else { continue };
			for variant in &check.variants {
				if !self.variant_applies(check, variant, &built, &booted) {
					continue;
				}
				items.push(PlanItem { key: PlanItemKey { check: check.id.clone(), architecture: variant.architecture.clone(), environment: variant.environment.clone(), configuration: variant.configuration.clone() }, kind: check.kind, command: check.command.replace("{arch}", &variant.architecture), reason: reason.clone() });
			}
		}
		items.sort_by(|left, right| left.key.cmp(&right.key));

		// The one outcome that must never happen quietly. Something was changed, it was understood,
		// and the answer was to run nothing at all - that is a selector fault, not a clean bill.
		if items.is_empty() && !nothing_to_do && !changed.is_empty() {
			warnings.push(String::from("the selection was empty for a change that is not declared non-code; escalating to FULL"));
			return self.full_plan(paths, seeds, affected, vec![String::from("an empty selection for a change with owned paths")], warnings);
		}

		Plan { full, full_reasons, paths, changed_components: seeds.into_iter().collect(), affected_components: affected.into_iter().collect(), architectures_built: built.into_iter().collect(), architectures_booted: booted.into_iter().collect(), items, nothing_to_do, warnings }
	}

	// Why this check is in the plan, or None for "it is not".
	fn select(&self, check: &Check, affected: &BTreeSet<String>, reached: &BTreeMap<String, Vec<Edge>>, full: bool) -> Option<String> {
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

	fn architecture_rule(&self, path: &str) -> (Vec<String>, Vec<String>) {
		let mut best: Option<(usize, &crate::registry::ArchitectureRule)> = None;
		for rule in &self.registry.architecture {
			if let Some(len) = prefix_match(&rule.path, path)
				&& best.is_none_or(|(best_len, _)| len > best_len)
			{
				best = Some((len, rule));
			}
		}
		match best {
			Some((_, rule)) => (rule.build.clone(), rule.boot.clone()),
			// Unreachable while the registry validates a default rule exists, and still handled:
			// the answer to "no architecture answer" is every architecture.
			None => (ARCHITECTURES.iter().map(|architecture| (*architecture).to_string()).collect(), ARCHITECTURES.iter().map(|architecture| (*architecture).to_string()).collect()),
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
		Plan { full: true, full_reasons: reasons, paths, changed_components: seeds.into_iter().collect(), affected_components: affected.into_iter().collect(), architectures_built: all.clone(), architectures_booted: all, items, nothing_to_do: false, warnings }
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
