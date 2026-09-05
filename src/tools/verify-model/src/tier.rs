// The checked handoff between ordinary verification's two tiers. History informs selection;
// only this run's per-key outcomes discharge the obligations carried between those tiers.
use crate::Model;
use crate::catalog::{CheckKind, Environment};
use crate::commands::{self, Step};
use crate::plan::{Plan, PlanItem, PlanItemKey, Planner};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

type Keys = BTreeSet<PlanItemKey>;

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
	let output = Command::new("git").arg("-C").arg(root).args(args).output().map_err(|error| error.to_string())?;
	if !output.status.success() {
		return Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim()));
	}
	Ok(output.stdout)
}

fn field(hash: &mut Sha256, bytes: &[u8]) {
	hash.update((bytes.len() as u64).to_be_bytes());
	hash.update(bytes);
}

fn entry(hash: &mut Sha256, path: &[u8], mode: &[u8], content: &[u8]) {
	field(hash, path);
	field(hash, mode);
	field(hash, content);
}

// The index supplies names only. Its blob contents and staging state never enter this identity.
// Byte paths, NUL records, and git's executable-bit domain make this identical to commit_tree.
pub fn effective_tree(root: &Path) -> Result<String, String> {
	for record in git(root, &["ls-files", "--stage", "-z"])?.split(|byte| *byte == 0).filter(|record| !record.is_empty()) {
		if record.starts_with(b"160000 ") {
			return Err("effective tree refuses gitlinks (submodules are unsupported)".into());
		}
	}
	let names = git(root, &["ls-files", "-co", "--exclude-standard", "-z"])?;
	let names: BTreeSet<&[u8]> = names.split(|byte| *byte == 0).filter(|name| !name.is_empty()).collect();
	let mut hash = Sha256::new();
	hash.update(b"verify-effective-tree/1\n");
	for name in names {
		let path = root.join(OsStr::from_bytes(name));
		let metadata = match fs::symlink_metadata(&path) {
			Ok(metadata) => metadata,
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
			Err(error) => return Err(format!("{}: {error}", path.display())),
		};
		if metadata.file_type().is_symlink() {
			let target = fs::read_link(&path).map_err(|error| format!("{}: {error}", path.display()))?;
			entry(&mut hash, name, b"120000", target.as_os_str().as_bytes());
		} else if metadata.is_file() {
			let mode: &[u8] = if metadata.permissions().mode() & 0o100 != 0 { b"100755" } else { b"100644" };
			let content = fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
			entry(&mut hash, name, mode, &content);
		} else {
			return Err(format!("effective tree refuses unsupported file kind at {} (including nested repositories)", path.display()));
		}
	}
	Ok(format!("{:x}", hash.finalize()))
}

pub fn commit_tree(root: &Path, revision: &str) -> Result<String, String> {
	let listing = git(root, &["ls-tree", "-r", "-z", revision])?;
	let mut entries = BTreeMap::new();
	for record in listing.split(|byte| *byte == 0).filter(|record| !record.is_empty()) {
		let tab = record.iter().position(|byte| *byte == b'\t').ok_or("malformed git tree record")?;
		let fields: Vec<_> = record[..tab].split(|byte| *byte == b' ').collect();
		if fields.len() != 3 || !matches!(fields[0], b"100644" | b"100755" | b"120000") || fields[1] != b"blob" {
			return Err(format!("commit tree refuses unsupported entry {} (gitlinks are unsupported)", String::from_utf8_lossy(record)));
		}
		entries.insert(record[tab + 1..].to_vec(), (fields[0].to_vec(), fields[2].to_vec()));
	}
	// One batch process avoids one git process per source file. Feed concurrently to avoid a
	// pipe deadlock when both the list of object IDs and returned source contents are large.
	let mut child = Command::new("git").arg("-C").arg(root).args(["cat-file", "--batch"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|error| error.to_string())?;
	let mut input = child.stdin.take().ok_or("cat-file stdin unavailable")?;
	let objects: Vec<_> = entries.values().map(|(_, oid)| oid.clone()).collect();
	let writer = std::thread::spawn(move || -> std::io::Result<()> {
		for oid in objects {
			input.write_all(&oid)?;
			input.write_all(b"\n")?;
		}
		Ok(())
	});
	let output = child.wait_with_output().map_err(|error| error.to_string())?;
	writer.join().map_err(|_| "cat-file writer failed")?.map_err(|error| error.to_string())?;
	if !output.status.success() {
		return Err(format!("cat-file: {}", String::from_utf8_lossy(&output.stderr)));
	}
	let mut remaining = output.stdout.as_slice();
	let mut hash = Sha256::new();
	hash.update(b"verify-effective-tree/1\n");
	for (name, (mode, oid)) in entries {
		let newline = remaining.iter().position(|byte| *byte == b'\n').ok_or("missing cat-file header")?;
		let header = std::str::from_utf8(&remaining[..newline]).map_err(|error| error.to_string())?;
		let fields: Vec<_> = header.split(' ').collect();
		if fields.len() != 3 || fields[0].as_bytes() != oid || fields[1] != "blob" {
			return Err(format!("unexpected cat-file record: {header}"));
		}
		let size: usize = fields[2].parse().map_err(|_| "invalid cat-file size")?;
		remaining = &remaining[newline + 1..];
		if remaining.len() <= size || remaining[size] != b'\n' {
			return Err("truncated cat-file object".into());
		}
		entry(&mut hash, &name, &mode, &remaining[..size]);
		remaining = &remaining[size + 1..];
	}
	if !remaining.is_empty() {
		return Err("unexpected extra cat-file output".into());
	}
	Ok(format!("{:x}", hash.finalize()))
}

pub fn head(root: &Path) -> Result<String, String> {
	String::from_utf8(git(root, &["rev-parse", "--verify", "HEAD^{commit}"])?).map(|value| value.trim().into()).map_err(|error| error.to_string())
}

fn parent(root: &Path, revision: &str) -> Result<String, String> {
	let output = String::from_utf8(git(root, &["rev-list", "--parents", "-n", "1", revision])?).map_err(|error| error.to_string())?;
	let fields: Vec<_> = output.split_whitespace().collect();
	if fields.len() != 2 {
		return Err(format!("merge revision {revision} must have exactly one parent; found {}", fields.len().saturating_sub(1)));
	}
	Ok(fields[1].into())
}

pub fn keys(plan: &Plan) -> Keys {
	plan.items.iter().map(|item| item.key.clone()).collect()
}

pub fn plan_digest(keys: &Keys) -> String {
	let mut hash = Sha256::new();
	hash.update(b"verify-plan-keys/1\n");
	for key in keys {
		field(&mut hash, key.display().as_bytes());
	}
	format!("{:x}", hash.finalize())
}

fn counts(model: &Model) -> BTreeMap<String, usize> {
	let mut counts = BTreeMap::new();
	for test in &model.kernel_tests.tests {
		for target in &test.architectures {
			*counts.entry(target.clone()).or_default() += 1;
		}
	}
	counts
}

// Classify complete executable steps, then propagate deferred dependencies to consumers. A host
// gate reading port output cannot stay inner merely because its key's architecture is "host".
pub fn partition(steps: &[Step], original: &Keys) -> Result<(Keys, Keys), String> {
	commands::validate(steps).map_err(|errors| errors.join("; "))?;
	let mut deferred_steps: BTreeSet<String> = steps.iter().filter(|step| step.keys.iter().any(|key| matches!(key.architecture.as_str(), "aarch64" | "riscv64") || key.environment == Environment::DevGuest || (key.check.starts_with("gate.") && step.guests > 0))).map(|step| step.id.clone()).collect();
	loop {
		let before = deferred_steps.len();
		for step in steps {
			if step.requires.iter().any(|required| deferred_steps.contains(required)) {
				deferred_steps.insert(step.id.clone());
			}
		}
		if before == deferred_steps.len() {
			break;
		}
	}
	let mut inner = Keys::new();
	let mut deferred = Keys::new();
	let mut seen = Keys::new();
	for step in steps {
		for key in &step.keys {
			if !seen.insert(key.clone()) {
				return Err(format!("duplicate step accounting for {}", key.display()));
			}
			if deferred_steps.contains(&step.id) {
				deferred.insert(key.clone());
			} else {
				inner.insert(key.clone());
			}
		}
	}
	if seen != *original || !inner.is_disjoint(&deferred) {
		return Err("tier partition does not cover exactly the original selected keys".into());
	}
	Ok((inner, deferred))
}

// A prerequisite can execute again, but only owned keys are recorded by this tier. Empty-key
// steps survive only when another selected step actually requires them.
pub fn closed_steps(steps: &[Step], owned: &Keys) -> Result<Vec<Step>, String> {
	commands::validate(steps).map_err(|errors| errors.join("; "))?;
	let mut needed: BTreeSet<String> = steps.iter().filter(|step| step.keys.iter().any(|key| owned.contains(key))).map(|step| step.id.clone()).collect();
	loop {
		let before = needed.len();
		for step in steps {
			if needed.contains(&step.id) {
				needed.extend(step.requires.iter().cloned());
			}
		}
		if before == needed.len() {
			break;
		}
	}
	let selected: Vec<_> = steps
		.iter()
		.filter(|step| needed.contains(&step.id))
		.map(|step| {
			let mut step = step.clone();
			step.keys.retain(|key| owned.contains(key));
			step
		})
		.collect();
	let recorded: Vec<_> = selected.iter().flat_map(|step| step.keys.iter().cloned()).collect();
	if recorded.len() != owned.len() || recorded.into_iter().collect::<Keys>() != *owned {
		return Err("lowered tier did not account for every owned key exactly once".into());
	}
	commands::validate(&selected).map_err(|errors| errors.join("; "))?;
	Ok(selected)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Handoff {
	pub version: u32,
	pub effective_tree: String,
	pub source_model_hash: String,
	pub model_hash: String,
	pub original_plan_digest: String,
	pub plan: Plan,
	pub change_kinds: BTreeMap<String, String>,
	pub inner: Keys,
	pub deferred: Keys,
	pub inner_outcomes: BTreeMap<String, bool>,
	pub inner_complete: bool,
}

impl Handoff {
	fn validate(&self, completed: bool) -> Result<(), String> {
		if self.version != 1 {
			return Err(format!("unsupported handoff version {}", self.version));
		}
		let original = keys(&self.plan);
		if !self.inner.is_disjoint(&self.deferred) || self.inner.union(&self.deferred).cloned().collect::<Keys>() != original {
			return Err("handoff inner and deferred sets overlap or fail to partition the original plan".into());
		}
		if plan_digest(&original) != self.original_plan_digest {
			return Err("handoff original plan digest disagrees with its keys".into());
		}
		if completed {
			if !self.inner_complete {
				return Err("handoff has no completed inner share".into());
			}
			check_outcomes(&self.inner, &self.inner_outcomes)?;
		}
		Ok(())
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeState {
	pub handoff: Handoff,
	pub revision: String,
	pub parent: String,
	pub plan: Plan,
	pub change_kinds: BTreeMap<String, String>,
	pub inventory_targets: BTreeSet<String>,
	pub work: Option<Keys>,
	pub retired: BTreeMap<String, String>,
	pub complete: bool,
}

pub fn inventory_targets(handoff: &Handoff, plan: &Plan) -> BTreeSet<String> {
	let mut targets: BTreeSet<_> = handoff.inner.iter().chain(&handoff.deferred).chain(plan.items.iter().map(|item| &item.key)).map(|key| key.architecture.clone()).collect();
	for relevant in [&handoff.plan, plan] {
		targets.extend(relevant.architectures_built.iter().cloned());
		targets.extend(relevant.architectures_booted.iter().cloned());
	}
	targets.retain(|target| crate::registry::ARCHITECTURES.contains(&target.as_str()));
	targets
}

pub fn check_snapshot(root: &Path, expected_tree: &str, revision: Option<&str>) -> Result<(), String> {
	let current = effective_tree(root)?;
	if current != expected_tree {
		return Err(format!("effective tree mismatch: inner {expected_tree}, current {current}"));
	}
	if let Some(revision) = revision {
		let current = head(root)?;
		if current != revision {
			return Err(format!("HEAD moved: pinned REV {revision}, current HEAD {current}"));
		}
	}
	Ok(())
}

fn source_matches(model: &Model, handoff: &Handoff) -> Result<(), String> {
	let current = model.source_model_hash()?;
	if current != handoff.source_model_hash {
		return Err(format!("source model mismatch: inner {}, current {current}", handoff.source_model_hash));
	}
	let diagnostic = model.model_hash();
	if diagnostic != handoff.model_hash {
		eprintln!("tier: artifact model drift: inner {}, current {diagnostic}; source model agrees", handoff.model_hash);
	}
	Ok(())
}

fn selection(model: &Model, paths: &[String]) -> Plan {
	let ownership = model.ownership();
	let planner = Planner::for_model(model, &ownership);
	if paths.is_empty() { planner.full_plan(Vec::new(), BTreeSet::new(), BTreeSet::new(), vec!["no changed paths were given".into()], Vec::new()) } else { planner.plan(paths) }
}

fn proposed_plan(model: &Model, parent: &str, revision: &str) -> Result<(Plan, BTreeMap<String, String>), String> {
	let changes = crate::changes::range(&model.repo_root, &format!("{parent}..{revision}"))?;
	let mut kinds = BTreeMap::new();
	for change in &changes {
		let kind = format!("{:?}", change.kind).to_lowercase();
		kinds.insert(change.path.clone(), kind.clone());
		if let Some(origin) = &change.origin {
			kinds.insert(origin.clone(), kind);
		}
	}
	Ok((selection(model, &crate::changes::paths(&changes)), kinds))
}

pub fn begin_inner(model: &Model, snapshot: String, paths: &[String], change_kinds: BTreeMap<String, String>) -> Result<(Handoff, Vec<Step>), String> {
	let plan = selection(model, paths);
	let original = keys(&plan);
	let steps = commands::steps(&plan, &counts(model), &model.registry);
	let (inner, deferred) = partition(&steps, &original)?;
	let selected = closed_steps(&steps, &inner)?;
	let handoff = Handoff { version: 1, effective_tree: snapshot, source_model_hash: model.source_model_hash()?, model_hash: model.model_hash(), original_plan_digest: plan_digest(&original), plan, change_kinds, inner, deferred, inner_outcomes: BTreeMap::new(), inner_complete: false };
	handoff.validate(false)?;
	Ok((handoff, selected))
}

pub fn finish_inner(root: &Path, handoff: &mut Handoff, outcomes: BTreeMap<String, bool>) -> Result<(), String> {
	check_snapshot(root, &handoff.effective_tree, None)?;
	handoff.validate(false)?;
	check_outcomes(&handoff.inner, &outcomes)?;
	handoff.inner_outcomes = outcomes;
	handoff.inner_complete = true;
	Ok(())
}

pub fn prepare_merge(model: &Model, handoff: Handoff) -> Result<MergeState, String> {
	handoff.validate(true)?;
	let revision = head(&model.repo_root)?;
	let parent = parent(&model.repo_root, &revision)?;
	check_snapshot(&model.repo_root, &handoff.effective_tree, Some(&revision))?;
	let committed = commit_tree(&model.repo_root, &revision)?;
	if committed != handoff.effective_tree {
		return Err(format!("proposed commit tree mismatch: inner {}, current {}, REV {revision} tree {committed}", handoff.effective_tree, effective_tree(&model.repo_root)?));
	}
	source_matches(model, &handoff)?;
	let (plan, change_kinds) = proposed_plan(model, &parent, &revision)?;
	let current_digest = plan_digest(&keys(&plan));
	if current_digest != handoff.original_plan_digest {
		eprintln!("tier: plan drift: inner {}, proposed {current_digest}; reconciling obligations", handoff.original_plan_digest);
	}
	let inventory_targets = inventory_targets(&handoff, &plan);
	Ok(MergeState { handoff, revision, parent, plan, change_kinds, inventory_targets, work: None, retired: BTreeMap::new(), complete: false })
}

fn catalog_item(model: &Model, key: &PlanItemKey) -> Result<Option<PlanItem>, String> {
	if model.registry.configuration(&key.configuration).is_none() {
		return Err(format!("cannot lower {}: unknown configuration", key.display()));
	}
	let Some(check) = model.catalog.checks.iter().find(|check| check.id == key.check) else { return Ok(None) };
	if !check.variants.iter().any(|variant| variant.architecture == key.architecture && variant.environment == key.environment && variant.configuration == key.configuration) {
		return Ok(None);
	}
	Ok(Some(PlanItem { key: key.clone(), kind: check.kind, command: check.command.replace("{arch}", &key.architecture), reason: "owed by the checked tier handoff or refreshed commit selection".into() }))
}

// Retirement is limited to a previously known kernel variant on an actually refreshed target.
// A malformed configuration, disappearing gate or unrelated lowering error is not a retirement.
pub fn reconcile(model: &Model, state: &MergeState, refreshed: &Plan) -> Result<(Keys, BTreeMap<String, String>), String> {
	source_matches(model, &state.handoff)?;
	let p0: Keys = state.handoff.inner.union(&state.handoff.deferred).cloned().collect();
	let p1 = keys(&state.plan);
	let mut work: Keys = state.handoff.deferred.union(&p1.difference(&p0).cloned().collect()).cloned().collect();
	let mut retired = BTreeMap::new();
	for key in work.clone() {
		if catalog_item(model, &key)?.is_some() {
			continue;
		}
		let was_kernel = state.handoff.plan.items.iter().chain(&state.plan.items).any(|item| item.key == key && item.kind == CheckKind::KernelTest);
		if !was_kernel || !state.inventory_targets.contains(&key.architecture) || model.kernel_tests.missing_targets.contains(&key.architecture) {
			return Err(format!("cannot lower {}; no refreshed kernel inventory proves this variant absent", key.display()));
		}
		let reason = format!("fresh {} test inventory contains no such variant; source model {} agrees", key.architecture, state.handoff.source_model_hash);
		retired.insert(key.display(), reason);
		work.remove(&key);
	}
	let previous: Keys = p0.union(&p1).cloned().collect();
	work.extend(keys(refreshed).difference(&previous).cloned());
	for key in &work {
		if catalog_item(model, key)?.is_none() {
			return Err(format!("refreshed work remains unlowerable: {}", key.display()));
		}
	}
	Ok((work, retired))
}

pub fn merge_commands(model: &Model, state: &mut MergeState) -> Result<Vec<Step>, String> {
	if state.work.is_some() {
		return Err("merge inventory has already been reconciled; start a new merge run".into());
	}
	check_snapshot(&model.repo_root, &state.handoff.effective_tree, Some(&state.revision))?;
	source_matches(model, &state.handoff)?;
	for target in &state.inventory_targets {
		if model.kernel_tests.missing_targets.contains(target) {
			return Err(format!("inventory preparation did not produce a descriptor-bearing {target} test binary"));
		}
	}
	let (refreshed, kinds) = proposed_plan(model, &state.parent, &state.revision)?;
	let (work, retired) = reconcile(model, state, &refreshed)?;
	// Reconstitute the relevant plan to preserve its existing prerequisite graph. Lower once;
	// closure strips already answered keys while retaining the steps their consumers require.
	let relevant: Keys = keys(&state.handoff.plan).union(&keys(&state.plan)).cloned().collect::<Keys>().union(&keys(&refreshed)).cloned().collect();
	let mut executable = refreshed.clone();
	let built: BTreeSet<_> = state.handoff.plan.architectures_built.iter().chain(&state.plan.architectures_built).chain(&refreshed.architectures_built).cloned().collect();
	let booted: BTreeSet<_> = state.handoff.plan.architectures_booted.iter().chain(&state.plan.architectures_booted).chain(&refreshed.architectures_booted).cloned().collect();
	executable.architectures_built = built.into_iter().collect();
	executable.architectures_booted = booted.into_iter().collect();
	executable.items.clear();
	for key in &relevant {
		if let Some(item) = catalog_item(model, key)? {
			executable.items.push(item);
		}
	}
	let steps = closed_steps(&commands::steps(&executable, &counts(model), &model.registry), &work)?;
	for (key, reason) in &retired {
		eprintln!("RETIRED\t{key}\t{reason}");
	}
	let original: Keys = state.handoff.inner.union(&state.handoff.deferred).cloned().collect();
	for key in work.difference(&original) {
		eprintln!("ADDED\t{}", key.display());
	}
	eprintln!("tier: {} merge key(s), {} retirement(s)", work.len(), retired.len());
	state.plan = refreshed;
	state.change_kinds = kinds;
	state.work = Some(work);
	state.retired = retired;
	Ok(steps)
}

pub fn check_outcomes(required: &Keys, outcomes: &BTreeMap<String, bool>) -> Result<(), String> {
	for key in required {
		match outcomes.get(&key.display()) {
			Some(true) => {}
			Some(false) => return Err(format!("failed outcome does not discharge {}", key.display())),
			None => return Err(format!("missing outcome for {}", key.display())),
		}
	}
	let allowed: BTreeSet<_> = required.iter().map(PlanItemKey::display).collect();
	if let Some(extra) = outcomes.keys().find(|key| !allowed.contains(*key)) {
		return Err(format!("outcome names a key this tier does not own: {extra}"));
	}
	Ok(())
}

pub fn finish_merge(root: &Path, state: &mut MergeState, outcomes: &BTreeMap<String, bool>) -> Result<(), String> {
	check_snapshot(root, &state.handoff.effective_tree, Some(&state.revision))?;
	state.handoff.validate(true)?;
	let work = state.work.as_ref().ok_or("merge has no reconciled work set")?;
	check_outcomes(work, outcomes)?;
	state.complete = true;
	Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, String> {
	serde_json::from_slice(&fs::read(path).map_err(|error| format!("{path}: {error}"))?).map_err(|error| format!("{path}: {error}"))
}

fn write_json<T: Serialize>(path: &str, value: &T) -> Result<(), String> {
	let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
	// Pending and sealed files have separate paths. Replacing one file atomically prevents a
	// interrupted write from being confused with a complete handoff.
	let temporary = format!("{path}.writing-{}", std::process::id());
	fs::write(&temporary, bytes).map_err(|error| format!("{temporary}: {error}"))?;
	fs::rename(&temporary, path).map_err(|error| format!("{path}: {error}"))
}

fn read_outcomes(path: &str) -> Result<BTreeMap<String, bool>, String> {
	let text = fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?;
	let mut outcomes = BTreeMap::new();
	for line in text.lines().filter(|line| !line.is_empty()) {
		let (status, key) = line.split_once('\t').ok_or("outcome needs status<TAB>key")?;
		let passed = match status {
			"passed" => true,
			"failed" => false,
			_ => return Err(format!("unknown outcome {status}")),
		};
		outcomes.entry(key.into()).and_modify(|previous| *previous &= passed).or_insert(passed);
	}
	Ok(outcomes)
}

fn evidence_level(model: &Model, plan: &Plan, selected: &Keys, kinds: &BTreeMap<String, String>, inner: bool) -> Result<String, String> {
	if plan.full && !inner {
		return Ok("FULL".into());
	}
	if plan.nothing_to_do {
		return Ok(if inner { "INCOMPLETE" } else { "TRUSTED" }.into());
	}
	let hash = model.model_hash();
	let mut store = crate::trust::Store::load(&model.repo_root);
	store.prune(&hash);
	let required: BTreeSet<_> = plan.architectures_built.iter().chain(&plan.architectures_booted).cloned().collect();
	let scopes: BTreeMap<_, _> = crate::shadow::component_scopes(&model.repo_root, plan, kinds, &model.registry).into_iter().map(|(component, scope)| (component, scope.with_architectures(required.iter().cloned().collect()))).collect();
	let empty = crate::shadow::Scope::default();
	let untrusted: Vec<_> = plan.changed_components.iter().filter(|component| !store.trusted_everywhere(component, &hash, &crate::catalog::judging_universes(&model.catalog, component), scopes.get(*component).unwrap_or(&empty))).cloned().collect();
	let history = crate::history::History::load(&model.repo_root)?;
	let window = std::env::var("VERIFY_AGE_DAYS").ok().and_then(|value| value.parse().ok()).unwrap_or(30);
	let universe: Vec<_> = model.catalog.checks.iter().flat_map(|check| check.variants.iter().map(|variant| PlanItemKey { check: check.id.clone(), architecture: variant.architecture.clone(), environment: variant.environment.clone(), configuration: variant.configuration.clone() })).collect();
	let selected: BTreeSet<_> = selected.iter().map(PlanItemKey::display).collect();
	let overdue = history.stale(&universe, window).into_iter().filter(|(key, _)| !selected.contains(key)).count();
	Ok(if overdue > 0 {
		format!("STALE\t{overdue}")
	} else if !untrusted.is_empty() {
		format!("SHADOW\t{}", untrusted.join(","))
	} else if inner {
		"INCOMPLETE".into()
	} else {
		"TRUSTED".into()
	})
}

// Thin CLI seams used by verify.sh. Inventory compilation stays in the shell; all decisions and
// validation stay here. No command consults mutable history as a replacement for inner outcomes.
pub fn run(root: &Path, arguments: &[String], emit: fn(&Model, Vec<Step>) -> Result<(), String>) -> Result<(), String> {
	let command = arguments.first().ok_or("missing tier command")?.as_str();
	if command.starts_with("tier-merge") && arguments.iter().any(|argument| argument == "--budget" || argument.starts_with("--budget=")) {
		return Err("--merge does not accept --budget; no inventory build was started".into());
	}
	let mut options = BTreeMap::new();
	let mut stdin = false;
	let mut index = 1;
	while index < arguments.len() {
		let name = arguments[index].as_str();
		if name == "--stdin" {
			stdin = true;
			index += 1;
			continue;
		}
		if !matches!(name, "--state" | "--handoff" | "--outcomes" | "--paths-file" | "--revision") {
			return Err(format!("unknown tier option {name}"));
		}
		index += 1;
		options.insert(name, arguments.get(index).ok_or_else(|| format!("{name} needs a value"))?.as_str());
		index += 1;
	}
	let option = |name: &str| options.get(name).copied().ok_or_else(|| format!("{command} needs {name}"));
	match command {
		"effective-tree" => {
			println!("{}", if let Some(revision) = options.get("--revision") { commit_tree(root, revision)? } else { effective_tree(root)? });
		}
		"source-model-hash" => {
			println!("{}", Model::load(root)?.source_model_hash()?);
		}
		"tier-inner" => {
			let snapshot = effective_tree(root)?;
			let model = Model::load(root)?;
			let mut input = String::new();
			if stdin {
				std::io::stdin().read_to_string(&mut input).map_err(|error| error.to_string())?;
			}
			if let Some(path) = options.get("--paths-file") {
				input.push_str(&fs::read_to_string(path).map_err(|error| error.to_string())?);
			}
			let mut kinds = BTreeMap::new();
			let mut paths = Vec::new();
			for line in input.lines().filter(|line| !line.is_empty()) {
				if let Some((kind, path)) = line.split_once('\t') {
					kinds.insert(path.into(), kind.into());
					paths.push(path.into());
				} else {
					paths.push(line.into());
				}
			}
			let (handoff, steps) = begin_inner(&model, snapshot, &paths, kinds)?;
			write_json(option("--state")?, &handoff)?;
			println!("STATUS\tscoped\tinner partition: {} inner, {} deferred; merge is required", handoff.inner.len(), handoff.deferred.len());
			emit(&model, steps)?;
		}
		"tier-inner-finish" => {
			let mut handoff: Handoff = read_json(option("--state")?)?;
			finish_inner(root, &mut handoff, read_outcomes(option("--outcomes")?)?)?;
			write_json(option("--state")?, &handoff)?;
			write_json(option("--handoff")?, &handoff)?;
			println!("INCOMPLETE\t{} inner keys passed; {} deferred keys; commit tested content then ./verify.sh --merge {}", handoff.inner.len(), handoff.deferred.len(), option("--handoff")?);
		}
		"tier-merge-prepare" => {
			let model = Model::load(root)?;
			let state = prepare_merge(&model, read_json(option("--handoff")?)?)?;
			write_json(option("--state")?, &state)?;
			for target in &state.inventory_targets {
				println!("TARGET\t{target}");
			}
			eprintln!("tier: pinned REV {} (parent {})", state.revision, state.parent);
		}
		"tier-merge-commands" => {
			let mut state: MergeState = read_json(option("--state")?)?;
			let model = Model::load(root)?;
			let steps = merge_commands(&model, &mut state)?;
			write_json(option("--state")?, &state)?;
			println!("STATUS\tscoped\tmerge reconciliation: {} owed, {} retired", state.work.as_ref().map_or(0, Keys::len), state.retired.len());
			emit(&model, steps)?;
		}
		"tier-merge-finish" => {
			let mut state: MergeState = read_json(option("--state")?)?;
			finish_merge(root, &mut state, &read_outcomes(option("--outcomes")?)?)?;
			write_json(option("--state")?, &state)?;
			println!("DISCHARGED\t{} inner keys, {} merge keys passed, {} retired", state.handoff.inner.len(), state.work.as_ref().map_or(0, Keys::len), state.retired.len());
		}
		"tier-level" | "tier-paths" => {
			let bytes = fs::read(option("--state")?).map_err(|error| error.to_string())?;
			if let Ok(state) = serde_json::from_slice::<MergeState>(&bytes) {
				if command == "tier-paths" {
					for (path, kind) in state.change_kinds {
						println!("{kind}\t{path}");
					}
				} else {
					if !state.complete {
						return Err("merge is not complete; no completed evidence level exists".into());
					}
					let selected: Keys = state.handoff.inner.union(state.work.as_ref().ok_or("merge work missing")?).cloned().collect();
					println!("{}", evidence_level(&Model::load(root)?, &state.plan, &selected, &state.change_kinds, false)?);
				}
			} else {
				let handoff: Handoff = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
				if command == "tier-paths" {
					for (path, kind) in handoff.change_kinds {
						println!("{kind}\t{path}");
					}
				} else {
					handoff.validate(true)?;
					println!("{}", evidence_level(&Model::load(root)?, &handoff.plan, &handoff.inner, &handoff.change_kinds, true)?);
				}
			}
		}
		_ => return Err(format!("unknown tier command {command}")),
	}
	Ok(())
}

#[cfg(test)]
mod tests;
