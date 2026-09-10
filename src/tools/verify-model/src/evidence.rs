// Evidence: what one verification run actually did, published by the producers that did it.
//
// AN ENVELOPE PER KEY. Every producer - a gate, a guest suite, a build, an image assembly, the
// development lifecycle - publishes one envelope for every plan-item key it discharged: the run it
// belongs to, the key, the revision and configuration identity it ran under, every input artifact
// it consumed with its digest, the outcome, the duration, and the result logs it produced - COPIED
// into the run's durable output before the producer's own cleanup, with their digests. An envelope
// that names a log which no longer exists describes nothing; the multi-boot gates used to keep
// their phase logs in a `mktemp -d` removed on exit, including on failure, which is when the log
// matters.
//
// THE COLLECTOR IS FAIL-CLOSED. A dossier is rendered only from a complete, consistent set: every
// release-required key has exactly one envelope, every envelope belongs to this run and this
// identity, names a key the catalog has, and every log it names exists with the recorded digest.
// Each way of failing is its own reason, because "the release is not proven" has to say which
// evidence is missing, stale or foreign - and a required key with no envelope is reported apart
// from a key whose envelope records a failure.
//
// Rendering is deterministic: the same envelope set renders the same bytes, apart from the
// invocation timestamp, which is written on its own line so a comparison can strip it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const ENVELOPE_SCHEMA: &str = "libersystem-evidence/1";
pub const DOSSIER_SCHEMA: &str = "libersystem-dossier/1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
	pub path: String,
	pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
	pub schema: String,
	pub run: String,
	pub key: String,
	// The identity block's digest, so an envelope from another revision or configuration is
	// distinguishable from one of this run's even when it carries this run's id.
	pub identity: String,
	pub producer: String,
	pub outcome: String,
	pub duration_seconds: u64,
	#[serde(default)]
	pub inputs: Vec<Artifact>,
	#[serde(default)]
	pub outputs: Vec<Artifact>,
	#[serde(default)]
	pub logs: Vec<Artifact>,
	// Keys this envelope discharges BESIDE its own - a whole-suite guest run discharges every kernel
	// test key of its target, read from the guest log.
	#[serde(default)]
	pub discharges: Vec<String>,
	pub published_at: String,
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
	let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
	Ok(hex(&Sha256::digest(&bytes)))
}

pub fn hex(bytes: &[u8]) -> String {
	bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// A key as a file name: each run of separators in the display form becomes one `+`, so
// `gate.x / host / host / default` is `gate.x+host+host+default.json` and a listing reads like the key.
pub fn key_file_name(key: &str) -> String {
	let mut out = String::new();
	let mut in_separator = false;
	for c in key.chars() {
		if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
			out.push(c);
			in_separator = false;
		} else if !in_separator {
			out.push('+');
			in_separator = true;
		}
	}
	out + ".json"
}

// The run's layout: `identity.json` at the root, envelopes under `evidence/`, log copies under
// `logs/<key>/`, the dossier at the root once collected.
pub struct Run {
	pub root: PathBuf,
}

impl Run {
	pub fn new(root: &Path) -> Run {
		Run { root: root.to_path_buf() }
	}

	pub fn id(&self) -> Result<String, String> {
		let path = self.root.join("run-id");
		std::fs::read_to_string(&path).map(|s| s.trim().to_string()).map_err(|error| format!("{}: {error}", path.display()))
	}

	pub fn identity_digest(&self) -> Result<String, String> {
		sha256_file(&self.root.join("identity.json"))
	}

	// Copy a producer's log into the run before the producer cleans up, and return the stored
	// artifact. The copy is what the envelope names - never the producer's temporary path.
	pub fn keep_log(&self, key: &str, source: &Path) -> Result<Artifact, String> {
		let dir = self.root.join("logs").join(key_file_name(key).trim_end_matches(".json"));
		std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
		let name = source.file_name().and_then(|n| n.to_str()).ok_or_else(|| format!("{}: no file name", source.display()))?;
		let mut target = dir.join(name);
		// Two logs of one key with one name: the second is suffixed rather than overwriting the first.
		let mut n = 1;
		while target.exists() {
			n += 1;
			target = dir.join(format!("{name}.{n}"));
		}
		let temporary = target.with_extension(format!("{}.tmp", std::process::id()));
		std::fs::copy(source, &temporary).map_err(|error| format!("cannot keep {}: {error}", source.display()))?;
		std::fs::rename(&temporary, &target).map_err(|error| format!("cannot publish {}: {error}", target.display()))?;
		let sha256 = sha256_file(&target)?;
		Ok(Artifact { path: target.strip_prefix(&self.root).unwrap_or(&target).to_string_lossy().to_string(), sha256 })
	}

	// Publish one envelope atomically. A second envelope for the same key is REFUSED here, at the
	// producer, rather than left for the collector to find: two producers claiming one key is a
	// run that cannot say which evidence is its own.
	pub fn publish(&self, envelope: &Envelope) -> Result<PathBuf, String> {
		if envelope.schema != ENVELOPE_SCHEMA {
			return Err(format!("envelope schema {} is not {ENVELOPE_SCHEMA}", envelope.schema));
		}
		let run = self.id()?;
		if envelope.run != run {
			return Err(format!("the envelope names run {} and this is run {run}", envelope.run));
		}
		let dir = self.root.join("evidence");
		std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
		let target = dir.join(key_file_name(&envelope.key));
		if target.exists() {
			return Err(format!("an envelope for `{}` is already published in this run - a key is discharged once", envelope.key));
		}
		let text = serde_json::to_string_pretty(envelope).map_err(|error| error.to_string())?;
		let temporary = dir.join(format!("{}.{}.tmp", key_file_name(&envelope.key), std::process::id()));
		std::fs::write(&temporary, text).map_err(|error| format!("{}: {error}", temporary.display()))?;
		std::fs::rename(&temporary, &target).map_err(|error| format!("{}: {error}", target.display()))?;
		Ok(target)
	}

	// Publish unless an envelope for the key is already there. The runner's fallback for a producer
	// that publishes its own evidence: the producer's envelope wins, and the runner's is written only
	// for a producer that crashed before it could say anything.
	pub fn publish_if_absent(&self, envelope: &Envelope) -> Result<Option<PathBuf>, String> {
		if self.root.join("evidence").join(key_file_name(&envelope.key)).exists() {
			return Ok(None);
		}
		self.publish(envelope).map(Some)
	}

	// The logs a producer already KEPT under a key - `keep-log` from inside its cleanup - so the
	// envelope written afterwards names them without the caller having to remember each path.
	pub fn kept_logs(&self, key: &str) -> Result<Vec<Artifact>, String> {
		let dir = self.root.join("logs").join(key_file_name(key).trim_end_matches(".json"));
		let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(Vec::new()) };
		let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_file() && !p.to_string_lossy().ends_with(".tmp")).collect();
		paths.sort();
		let mut out = Vec::new();
		for path in paths {
			out.push(Artifact { path: path.strip_prefix(&self.root).unwrap_or(&path).to_string_lossy().to_string(), sha256: sha256_file(&path)? });
		}
		Ok(out)
	}

	pub fn envelopes(&self) -> Result<Vec<(PathBuf, Envelope)>, String> {
		let dir = self.root.join("evidence");
		let mut out = Vec::new();
		let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(out) };
		let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
		paths.sort();
		for path in paths {
			let text = std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
			let envelope: Envelope = serde_json::from_str(&text).map_err(|error| format!("{}: unparsable envelope: {error}", path.display()))?;
			out.push((path, envelope));
		}
		Ok(out)
	}
}

// Why a dossier cannot be rendered, each distinguishable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum Refusal {
	MissingRequired { key: String },
	FailedRequired { key: String, outcome: String },
	Duplicate { key: String, paths: Vec<String> },
	Unknown { key: String, path: String },
	CrossRun { key: String, run: String, expected: String },
	Stale { key: String, identity: String, expected: String },
	LogMissing { key: String, path: String },
	LogAltered { key: String, path: String, recorded: String, actual: String },
	Unparsable { path: String, error: String },
	Schema { path: String, schema: String },
	// The tree moved while the run was in progress: the source identity read when the dossier is
	// collected is not the one the run started with. Envelopes carry the run's identity digest, so
	// they all still match each other - which is exactly why the collector has to look at the tree
	// itself rather than at the envelopes.
	SourceChanged { recorded: String, current: String },
}

impl Refusal {
	pub fn message(&self) -> String {
		match self {
			Refusal::MissingRequired { key } => format!("required key `{key}` has NO envelope - it did not run, or its producer published nothing"),
			Refusal::FailedRequired { key, outcome } => format!("required key `{key}` ran and its envelope records `{outcome}`"),
			Refusal::Duplicate { key, paths } => format!("key `{key}` has {} envelopes ({}) - a key is discharged once", paths.len(), paths.join(", ")),
			Refusal::Unknown { key, path } => format!("{path} names key `{key}`, which the catalog does not have"),
			Refusal::CrossRun { key, run, expected } => format!("the envelope for `{key}` belongs to run {run}, not this run {expected}"),
			Refusal::Stale { key, identity, expected } => format!("the envelope for `{key}` was produced under identity {identity}, not this run's {expected}"),
			Refusal::LogMissing { key, path } => format!("the envelope for `{key}` names log {path}, which does not exist"),
			Refusal::LogAltered { key, path, recorded, actual } => format!("the log {path} named by `{key}` has digest {actual}, not the recorded {recorded}"),
			Refusal::Unparsable { path, error } => format!("{path} is not an envelope: {error}"),
			Refusal::Schema { path, schema } => format!("{path} carries schema `{schema}`, not {ENVELOPE_SCHEMA}"),
			Refusal::SourceChanged { recorded, current } => format!("the tree changed while the run was in progress: the run started at source identity {recorded} and the tree now reads {current}"),
		}
	}
}

#[derive(Clone, Debug, Serialize)]
pub struct DossierRow {
	pub key: String,
	pub required: bool,
	pub outcome: String,
	pub duration_seconds: u64,
	pub producer: String,
	pub inputs: Vec<Artifact>,
	pub outputs: Vec<Artifact>,
	pub logs: Vec<Artifact>,
	pub discharges: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct Dossier {
	pub schema: String,
	pub run: String,
	pub identity: String,
	pub identity_block: serde_json::Value,
	pub publisher: String,
	pub required_keys: usize,
	pub rows: Vec<DossierRow>,
	pub refusals: Vec<Refusal>,
	pub state: String,
}

// Collect a run's envelopes against the required key set and the catalog's known keys.
// `current_source` is the tree's source identity as read NOW; the run's identity block recorded it
// at the start, and a difference is a refusal of its own. `rehearsal` names why this run is not a
// release - an in-place run over a writable tree, or a narrowed required set - and keeps the state
// from ever reading `complete`.
pub fn collect(run: &Run, required: &BTreeSet<String>, known: &BTreeSet<String>, publisher: &str, current_source: Option<&str>, rehearsal: Option<&str>) -> Result<Dossier, String> {
	let run_id = run.id()?;
	let identity = run.identity_digest()?;
	let identity_block: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(run.root.join("identity.json")).map_err(|error| error.to_string())?).map_err(|error| format!("identity.json: {error}"))?;
	let mut refusals: Vec<Refusal> = Vec::new();
	if let Some(current) = current_source {
		let recorded = identity_block.get("source").and_then(|s| s.get("value")).and_then(|v| v.as_str()).unwrap_or("").to_string();
		if recorded != current {
			refusals.push(Refusal::SourceChanged { recorded, current: current.to_string() });
		}
	}
	let mut by_key: BTreeMap<String, Vec<(PathBuf, Envelope)>> = BTreeMap::new();
	let dir = run.root.join("evidence");
	if let Ok(entries) = std::fs::read_dir(&dir) {
		let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
		paths.sort();
		for path in paths {
			let rel = path.strip_prefix(&run.root).unwrap_or(&path).to_string_lossy().to_string();
			let text = match std::fs::read_to_string(&path) {
				Ok(text) => text,
				Err(error) => {
					refusals.push(Refusal::Unparsable { path: rel, error: error.to_string() });
					continue;
				}
			};
			match serde_json::from_str::<Envelope>(&text) {
				Ok(envelope) => {
					if envelope.schema != ENVELOPE_SCHEMA {
						refusals.push(Refusal::Schema { path: rel, schema: envelope.schema.clone() });
						continue;
					}
					by_key.entry(envelope.key.clone()).or_default().push((path, envelope));
				}
				Err(error) => refusals.push(Refusal::Unparsable { path: rel, error: error.to_string() }),
			}
		}
	}
	let mut rows: Vec<DossierRow> = Vec::new();
	for (key, entries) in &by_key {
		if entries.len() > 1 {
			refusals.push(Refusal::Duplicate { key: key.clone(), paths: entries.iter().map(|(p, _)| p.strip_prefix(&run.root).unwrap_or(p).to_string_lossy().to_string()).collect() });
			continue;
		}
		let (path, envelope) = &entries[0];
		let rel = path.strip_prefix(&run.root).unwrap_or(path).to_string_lossy().to_string();
		if !known.contains(key) {
			refusals.push(Refusal::Unknown { key: key.clone(), path: rel });
			continue;
		}
		if envelope.run != run_id {
			refusals.push(Refusal::CrossRun { key: key.clone(), run: envelope.run.clone(), expected: run_id.clone() });
			continue;
		}
		if envelope.identity != identity {
			refusals.push(Refusal::Stale { key: key.clone(), identity: envelope.identity.clone(), expected: identity.clone() });
			continue;
		}
		for log in &envelope.logs {
			let full = run.root.join(&log.path);
			match sha256_file(&full) {
				Err(_) => refusals.push(Refusal::LogMissing { key: key.clone(), path: log.path.clone() }),
				Ok(actual) if actual != log.sha256 => refusals.push(Refusal::LogAltered { key: key.clone(), path: log.path.clone(), recorded: log.sha256.clone(), actual }),
				Ok(_) => {}
			}
		}
		let required_key = required.contains(key);
		if required_key && envelope.outcome != "passed" {
			refusals.push(Refusal::FailedRequired { key: key.clone(), outcome: envelope.outcome.clone() });
		}
		rows.push(DossierRow { key: key.clone(), required: required_key, outcome: envelope.outcome.clone(), duration_seconds: envelope.duration_seconds, producer: envelope.producer.clone(), inputs: envelope.inputs.clone(), outputs: envelope.outputs.clone(), logs: envelope.logs.clone(), discharges: envelope.discharges.len() });
	}
	// A required key may also be discharged by another envelope (a whole-suite run discharges its
	// kernel tests); a key with neither its own envelope nor a discharger is MISSING.
	let discharged: BTreeSet<&String> = by_key.values().flat_map(|entries| entries.iter().flat_map(|(_, e)| e.discharges.iter())).collect();
	for key in required {
		if !by_key.contains_key(key) && !discharged.contains(key) {
			refusals.push(Refusal::MissingRequired { key: key.clone() });
		}
	}
	refusals.sort_by_key(|r| serde_json::to_string(r).unwrap_or_default());
	let state = match (refusals.is_empty(), rehearsal) {
		(false, _) => String::from("incomplete"),
		(true, Some(reason)) => format!("rehearsal ({reason})"),
		(true, None) => String::from("complete"),
	};
	Ok(Dossier { schema: String::from(DOSSIER_SCHEMA), run: run_id, identity, identity_block, publisher: publisher.to_string(), required_keys: required.len(), rows, refusals, state })
}

// The deterministic rendering: the same dossier is the same bytes, and the invocation timestamp is
// the LAST line, on its own, so a comparison strips exactly one line.
pub fn render(dossier: &Dossier, rendered_at: &str) -> String {
	let mut out = String::new();
	out.push_str(&format!("# Release dossier - run {} ({})\n\n", dossier.run, dossier.state));
	out.push_str(&format!("Schema `{}`; identity `{}`; publisher `{}`; {} required key(s), {} row(s), {} refusal(s).\n\n", dossier.schema, dossier.identity, dossier.publisher, dossier.required_keys, dossier.rows.len(), dossier.refusals.len()));
	out.push_str("## Identity\n\n```json\n");
	out.push_str(&serde_json::to_string_pretty(&dossier.identity_block).unwrap_or_default());
	out.push_str("\n```\n\n");
	if !dossier.refusals.is_empty() {
		out.push_str("## Refusals\n\n");
		for refusal in &dossier.refusals {
			out.push_str(&format!("- {}\n", refusal.message()));
		}
		out.push('\n');
	}
	out.push_str("## Rows\n\n| key | required | outcome | seconds | producer | inputs | outputs | logs | discharges |\n| --- | --- | --- | ---: | --- | --- | --- | --- | ---: |\n");
	for row in &dossier.rows {
		let fmt = |a: &[Artifact]| a.iter().map(|x| format!("`{}` {}", x.path, &x.sha256[..12])).collect::<Vec<_>>().join("<br>");
		out.push_str(&format!("| `{}` | {} | {} | {} | {} | {} | {} | {} | {} |\n", row.key, if row.required { "yes" } else { "no" }, row.outcome, row.duration_seconds, row.producer, fmt(&row.inputs), fmt(&row.outputs), fmt(&row.logs), row.discharges));
	}
	out.push_str(&format!("\nrendered-at: {rendered_at}\n"));
	out
}

// Write a file atomically: a temporary beside it, renamed into place.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
	}
	let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| format!("{}: no file name", path.display()))?;
	let temporary = path.with_file_name(format!("{name}.{}.tmp", std::process::id()));
	std::fs::write(&temporary, bytes).map_err(|error| format!("{}: {error}", temporary.display()))?;
	std::fs::rename(&temporary, path).map_err(|error| format!("{}: {error}", path.display()))?;
	Ok(())
}

// The current UTC time as `YYYY-MM-DDTHH:MM:SSZ`, from the system clock and the civil-date
// arithmetic, so nothing here depends on a clock library.
pub fn now_utc() -> String {
	let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
	let days = (seconds / 86_400) as i64;
	let rem = seconds % 86_400;
	// Howard Hinnant's days-to-civil.
	let z = days + 719_468;
	let era = z.div_euclid(146_097);
	let doe = z.rem_euclid(146_097);
	let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
	let y = yoe + era * 400;
	let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
	let mp = (5 * doy + 2) / 153;
	let d = doy - (153 * mp + 2) / 5 + 1;
	let m = if mp < 10 { mp + 3 } else { mp - 9 };
	let y = if m <= 2 { y + 1 } else { y };
	format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, (rem % 3600) / 60, rem % 60)
}

// Start a run: a directory under `root` named by a collision-free id, holding the run id and the
// identity block. The root is OUTSIDE the worktree by contract - a failed or incomplete run keeps
// its evidence when the snapshot that produced it is removed.
pub fn start_run(root: &Path, repo: &Path) -> Result<Run, String> {
	let identity = crate::identity::collect(repo)?;
	let stamp = now_utc().replace(['-', ':'], "");
	let head = identity.source.head.chars().take(8).collect::<String>();
	let nonce = {
		let mut hasher = Sha256::new();
		hasher.update(std::process::id().to_le_bytes());
		hasher.update(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0).to_le_bytes());
		hex(&hasher.finalize())[..6].to_string()
	};
	let id = format!("{stamp}-{head}-{}-{nonce}", std::process::id());
	let dir = root.join(&id);
	if dir.exists() {
		return Err(format!("{} already exists - the run id collided", dir.display()));
	}
	std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
	write_atomically(&dir.join("run-id"), format!("{id}\n").as_bytes())?;
	write_atomically(&dir.join("identity.json"), serde_json::to_string_pretty(&identity).map_err(|error| error.to_string())?.as_bytes())?;
	std::fs::create_dir_all(dir.join("evidence")).map_err(|error| error.to_string())?;
	std::fs::create_dir_all(dir.join("logs")).map_err(|error| error.to_string())?;
	Ok(Run { root: dir })
}

// THE FROZEN RELEASE SET, as a checked-in TOML list of fully qualified keys.
#[derive(Debug, Deserialize)]
struct RequiredFile {
	#[allow(dead_code)]
	schema: u32,
	keys: Vec<String>,
}

pub fn load_required(path: &Path) -> Result<BTreeSet<String>, String> {
	let text = std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
	let file: RequiredFile = toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
	Ok(file.keys.into_iter().collect())
}

pub fn render_required(keys: &BTreeSet<String>) -> String {
	let mut out = String::new();
	out.push_str("# THE RELEASE-REQUIRED KEYS, FROZEN. Derived from the catalog's classes by\n# `verify-model release-required --write` and compared against the catalog EXACTLY by\n# `verify-model check`, in both directions: a required key the catalog no longer derives fails,\n# and a derived required key missing here fails. Cardinality and per-target presence do not detect\n# a substitution - deleting one mandatory row and inserting another of the same class keeps both -\n# so this list is what says which rows are OBLIGATORY. Adding a required profile is an edit to\n# this file, reviewed as its own artifact.\n\nschema = 1\n\nkeys = [\n");
	for key in keys {
		out.push_str(&format!("\t\"{key}\",\n"));
	}
	out.push_str("]\n");
	out
}

pub fn derived_release_required(catalog: &crate::catalog::Catalog) -> BTreeSet<String> {
	catalog.checks.iter().filter(|check| check.release_required).flat_map(|check| check.keys().into_iter().map(|key| key.display())).collect()
}

// Invariants that do not come from the catalog itself.
pub fn release_invariants(catalog: &crate::catalog::Catalog) -> Vec<(String, String)> {
	use crate::catalog::CheckClass;
	let mut failures = Vec::new();
	let ids: BTreeSet<&str> = catalog.checks.iter().map(|c| c.id.as_str()).collect();
	let produced: BTreeSet<&str> = catalog.checks.iter().flat_map(|c| c.produces.iter().map(String::as_str)).collect();
	for check in &catalog.checks {
		if check.release_required != check.class.release_required() {
			failures.push((check.id.clone(), format!("release_required is {} and its class {} says {}", check.release_required, check.class.as_str(), check.class.release_required())));
		}
		if check.release_required && matches!(check.class, CheckClass::Umbrella | CheckClass::Fallback) {
			failures.push((check.id.clone(), String::from("an umbrella or fallback row may not be release-required")));
		}
		// Per-target classes appear on every supported architecture.
		if matches!(check.class, CheckClass::Build | CheckClass::WholeSuite) {
			for architecture in crate::registry::ARCHITECTURES {
				if !check.variants.iter().any(|v| v.architecture == architecture) {
					failures.push((check.id.clone(), format!("a per-target class with no `{architecture}` variant")));
				}
			}
		}
		for prerequisite in &check.prerequisites {
			if !ids.contains(prerequisite.as_str()) {
				failures.push((check.id.clone(), format!("prerequisite `{prerequisite}` is not a catalog row")));
			}
		}
		for input in &check.inputs {
			if !produced.contains(input.as_str()) {
				failures.push((check.id.clone(), format!("input `{input}` has no producer row")));
			}
		}
		if !check.evidence.iter().any(|e| e == "envelope") {
			failures.push((check.id.clone(), String::from("every row publishes an envelope")));
		}
	}
	// Every release-required class has at least its declared cardinality.
	for (class, minimum) in [
		(CheckClass::Build, 7 * 3),
		(CheckClass::WholeSuite, 3),
		(CheckClass::Profile, 1),
		(CheckClass::Producer, 4),
		(CheckClass::Conformance, 1),
		(CheckClass::HostSuite, 1),
		(CheckClass::DevCheck, 1),
	] {
		let count: usize = catalog.checks.iter().filter(|c| c.class == class).map(|c| c.variants.len()).sum();
		if count < minimum {
			failures.push((format!("class {}", class.as_str()), format!("{count} key(s), fewer than the declared minimum of {minimum}")));
		}
	}
	failures
}
