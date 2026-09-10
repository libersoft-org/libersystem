// The unsafe-boundary inventory: every production `unsafe` boundary in this tree, across the
// artifact and configuration matrix that is DERIVED from the build rather than named.
//
// WHY A MATRIX. The kernel that ships is Cargo's dev profile, so `#[cfg(debug_assertions)]` code is
// in it; static userspace is the dev profile with `development` on for two crates; shared-image
// userspace is release, `--no-default-features --features shared-image`, a custom target and
// `-Z build-std`; the shipping WASM component is a release build for `wasm32-unknown-unknown`.
// The same source reaches different boundaries in each, and an inventory that read the source once
// would count a row's disabled branches as shipped and miss what a build script wrote.
//
// WHAT THIS TOOL DOES, AND WHAT IT LEAVES TO A PERSON. It enumerates each row's in-tree crate
// closure from `cargo metadata`, walks every module file the crate root reaches with the row's
// `cfg` set applied, reads the generated files a build left in `OUT_DIR`, records every boundary
// with a stable id, and merges the rows into one inventory: which rows reach a site, which sites
// are test-only, which are generated, which are macro templates. It then applies the CLASSIFICATION
// FILE - the auditor's decisions, keyed by group patterns and site ids - and reports what is left
// unclassified. Judging an invariant is not a thing a parser does; making sure no site can be
// missing from the judgement is.
//
// The mechanism is toolchain-pinned: the inventory records the compiler, the source revision, the
// matrix and the commands it ran, and `--expand` can additionally reconcile a row's cfg- and
// macro-expanded code against the source sites through `rustc -Zunpretty=expanded`.

mod scan;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(test)]
use scan::eval_cfg;
use scan::{CfgSet, Found, Kind, Provenance, Scanner};
#[cfg(test)]
use syn::Meta;

fn die(message: &str) -> ! {
	eprintln!("unsafe-inventory: {message}");
	std::process::exit(1)
}

// ---------------------------------------------------------------------------------------------
// The matrix

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct Row {
	id: String,
	// shipping | development | test
	kind: String,
	description: String,
	// The root crates: each named by its Cargo manifest and package, because the userspace crates
	// are not one workspace. A row may also derive roots from the system manifest.
	#[serde(default)]
	roots: Vec<RootCrate>,
	#[serde(default)]
	manifest_roots: Option<ManifestRoots>,
	target: String,
	profile: String,
	#[serde(default)]
	features: Vec<String>,
	#[serde(default)]
	no_default_features: bool,
	#[serde(default)]
	build_std: Vec<String>,
	#[serde(default)]
	rustflags: Vec<String>,
	#[serde(default)]
	env: BTreeMap<String, String>,
	cfg: Vec<String>,
	// Where builds of this row leave their artifacts, so `OUT_DIR` files can be read - several,
	// because the shared-image build keeps libraries and programs apart. The profile directory
	// (`debug`/`release`) and the target directory name are derived.
	#[serde(default)]
	target_dirs: Vec<String>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct RootCrate {
	manifest: String,
	package: String,
	// The root's own feature selection, where the build gives roots different ones - the
	// shared-image build compiles each staged library with the manifest row's features and every
	// dynamic program with `--no-default-features --features shared-image`. Empty means the row's.
	#[serde(default)]
	features: Vec<String>,
	#[serde(default)]
	no_default_features: bool,
}

// Roots taken from the system manifest: every staged shared library's crate, and every program of
// one linkage, resolved through the manifest's `[[sources]]` rows to a crate directory.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct ManifestRoots {
	manifest: String,
	#[serde(default)]
	libraries: bool,
	// "static" | "dynamic" | "none"
	#[serde(default = "default_programs")]
	programs: String,
}

fn default_programs() -> String {
	String::from("none")
}

#[derive(Clone, Debug, serde::Deserialize)]
struct Matrix {
	#[serde(default)]
	exclude_crates: Vec<String>,
	row: Vec<Row>,
}

// ---------------------------------------------------------------------------------------------
// cargo metadata

#[derive(Debug, serde::Deserialize)]
struct Metadata {
	packages: Vec<MetaPackage>,
	resolve: Option<Resolve>,
	workspace_root: String,
}

#[derive(Debug, serde::Deserialize)]
struct MetaPackage {
	name: String,
	id: String,
	manifest_path: String,
	targets: Vec<MetaTarget>,
	#[serde(default)]
	version: String,
}

#[derive(Debug, serde::Deserialize)]
struct MetaTarget {
	kind: Vec<String>,
	src_path: String,
}

#[derive(Debug, serde::Deserialize)]
struct Resolve {
	nodes: Vec<ResolveNode>,
}

#[derive(Debug, serde::Deserialize)]
struct ResolveNode {
	id: String,
	// Every edge with its kinds: only NORMAL dependencies are part of the artifact. A build
	// dependency is host code that runs at build time (its OUTPUT is scanned through `OUT_DIR`), and
	// a dev dependency is test-only.
	#[serde(default)]
	deps: Vec<ResolveDep>,
	// The features cargo resolved for this package under the row's selection - what `cfg(feature
	// = "...")` answers in that crate.
	#[serde(default)]
	features: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
struct ResolveDep {
	pkg: String,
	#[serde(default)]
	dep_kinds: Vec<DepKind>,
}

#[derive(Debug, serde::Deserialize)]
struct DepKind {
	#[serde(default)]
	kind: Option<String>,
}

impl ResolveNode {
	fn normal_dependencies(&self) -> Vec<String> {
		self.deps.iter().filter(|d| d.dep_kinds.is_empty() || d.dep_kinds.iter().any(|k| k.kind.is_none())).map(|d| d.pkg.clone()).collect()
	}
}

#[derive(Clone, Debug, serde::Serialize)]
struct CrateRow {
	name: String,
	manifest: String,
	roots: Vec<String>,
	features: Vec<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
struct RowReport {
	id: String,
	kind: String,
	description: String,
	target: String,
	profile: String,
	features: Vec<String>,
	no_default_features: bool,
	build_std: Vec<String>,
	rustflags: Vec<String>,
	env: BTreeMap<String, String>,
	cfg: Vec<String>,
	command: String,
	crates: Vec<CrateRow>,
	third_party: Vec<String>,
	files_scanned: usize,
	problems: Vec<String>,
	// How many of the row's crates have a built `OUT_DIR` - a crate without a build script has
	// none, which is not a missing build.
	crates_with_out_dir: usize,
}

// The root crates of a row: the ones written in the matrix plus the ones derived from the system
// manifest. A derived root's package name is read from its own Cargo manifest.
fn row_roots(repo: &Path, row: &Row) -> Vec<RootCrate> {
	let mut roots: Vec<RootCrate> = row.roots.clone();
	if let Some(derive) = &row.manifest_roots {
		let text = std::fs::read_to_string(repo.join(&derive.manifest)).unwrap_or_else(|e| die(&format!("cannot read {}: {e}", derive.manifest)));
		let value: toml::Value = toml::from_str(&text).unwrap_or_else(|e| die(&format!("{}: {e}", derive.manifest)));
		let src_dir = repo.join(&derive.manifest).parent().and_then(|p| p.parent()).and_then(|p| p.parent()).map(Path::to_path_buf).unwrap_or_else(|| repo.join("src"));
		let mut sources: BTreeMap<String, String> = BTreeMap::new();
		if let Some(list) = value.get("sources").and_then(|v| v.as_array()) {
			for entry in list {
				if let (Some(owner), Some(path)) = (entry.get("owner").and_then(|v| v.as_str()), entry.get("path").and_then(|v| v.as_str())) {
					sources.insert(owner.to_string(), path.to_string());
				}
			}
		}
		// (owner, features, no-default-features)
		let mut owners: Vec<(String, Vec<String>, bool)> = Vec::new();
		if derive.libraries
			&& let Some(list) = value.get("libraries").and_then(|v| v.as_array())
		{
			for entry in list {
				if let Some(owner) = entry.get("owner").and_then(|v| v.as_str()) {
					let features: Vec<String> = entry.get("features").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|f| f.as_str().map(String::from)).collect()).unwrap_or_default();
					// `build-shared.sh`: a library with a feature list is built `--no-default-features
					// --features <list>`; one with none is built with its defaults.
					let no_default = !features.is_empty();
					owners.push((owner.to_string(), features, no_default));
				}
			}
		}
		if derive.programs != "none"
			&& let Some(list) = value.get("programs").and_then(|v| v.as_array())
		{
			for entry in list {
				let linkage = entry.get("linkage").and_then(|v| v.as_str()).unwrap_or("static");
				let wanted = derive.programs == "all" || linkage == derive.programs;
				if wanted && let Some(owner) = entry.get("owner").and_then(|v| v.as_str()) {
					// A dynamic program is compiled `--no-default-features --features shared-image` -
					// where its crate defines that feature. The probe crate does not, and is built
					// with its defaults; the crate's own manifest is what decides.
					let defines_shared_image = sources.get(owner).map(|path| src_dir.join(path).join("Cargo.toml")).and_then(|m| std::fs::read_to_string(m).ok()).and_then(|t| toml::from_str::<toml::Value>(&t).ok()).map(|v| v.get("features").and_then(|f| f.get("shared-image")).is_some()).unwrap_or(false);
					let (features, no_default) = if linkage == "dynamic" && defines_shared_image { (vec![String::from("shared-image")], true) } else { (Vec::new(), false) };
					owners.push((owner.to_string(), features, no_default));
				}
			}
		}
		owners.sort();
		owners.dedup();
		for (owner, features, no_default_features) in owners {
			let Some(path) = sources.get(&owner) else {
				die(&format!("{}: owner `{owner}` has no [[sources]] row", derive.manifest));
			};
			let manifest_path = src_dir.join(path).join("Cargo.toml");
			let cargo_text = std::fs::read_to_string(&manifest_path).unwrap_or_else(|e| die(&format!("cannot read {}: {e}", manifest_path.display())));
			let cargo: toml::Value = toml::from_str(&cargo_text).unwrap_or_else(|e| die(&format!("{}: {e}", manifest_path.display())));
			let package = cargo.get("package").and_then(|p| p.get("name")).and_then(|v| v.as_str()).unwrap_or(&owner).to_string();
			let rel = manifest_path.strip_prefix(repo).unwrap_or(&manifest_path).to_string_lossy().to_string();
			if !roots.iter().any(|r| r.manifest == rel && r.package == package) {
				roots.push(RootCrate { manifest: rel, package, features, no_default_features });
			}
		}
	}
	roots
}

fn metadata(repo: &Path, row: &Row, manifest_rel: &str, features: &[String], no_default_features: bool) -> Metadata {
	let manifest = repo.join(manifest_rel);
	let mut command = Command::new("cargo");
	command.arg("metadata").arg("--format-version").arg("1").arg("--manifest-path").arg(&manifest).arg("--offline");
	if no_default_features {
		command.arg("--no-default-features");
	}
	if !features.is_empty() {
		command.arg("--features").arg(features.join(","));
	}
	// A custom JSON target spec is given by path; `--filter-platform` takes the triple or the
	// spec path exactly as `--target` would.
	let target_arg = if row.target.ends_with(".json") { repo.join(&row.target).to_string_lossy().to_string() } else { row.target.clone() };
	command.arg("--filter-platform").arg(&target_arg);
	command.env("RUSTC_BOOTSTRAP", "1");
	let output = command.output().unwrap_or_else(|e| die(&format!("cannot run cargo metadata for {}: {e}", row.id)));
	if !output.status.success() {
		// Retry without the platform filter, which a custom spec can make cargo refuse.
		let mut retry = Command::new("cargo");
		retry.arg("metadata").arg("--format-version").arg("1").arg("--manifest-path").arg(&manifest).arg("--offline");
		if no_default_features {
			retry.arg("--no-default-features");
		}
		if !features.is_empty() {
			retry.arg("--features").arg(features.join(","));
		}
		let output = retry.output().unwrap_or_else(|e| die(&format!("cannot run cargo metadata for {}: {e}", row.id)));
		if !output.status.success() {
			die(&format!("cargo metadata failed for row {}: {}", row.id, String::from_utf8_lossy(&output.stderr)));
		}
		return serde_json::from_slice(&output.stdout).unwrap_or_else(|e| die(&format!("cargo metadata for {} is not the JSON this tool reads: {e}", row.id)));
	}
	serde_json::from_slice(&output.stdout).unwrap_or_else(|e| die(&format!("cargo metadata for {} is not the JSON this tool reads: {e}", row.id)))
}

// The in-tree closure of the row's root packages: every package reachable through the resolve
// graph whose manifest is inside the repository. Registry packages are named and excluded.
// (name, manifest, roots, enabled features)
type CrateEntry = (String, String, Vec<String>, Vec<String>);

fn closure(repo: &Path, meta: &Metadata, roots: &[String], exclude: &[String]) -> (Vec<CrateEntry>, Vec<String>) {
	let by_id: BTreeMap<&str, &MetaPackage> = meta.packages.iter().map(|p| (p.id.as_str(), p)).collect();
	let by_name: BTreeMap<&str, &MetaPackage> = meta.packages.iter().filter(|p| Path::new(&p.manifest_path).starts_with(repo)).map(|p| (p.name.as_str(), p)).collect();
	let deps: BTreeMap<&str, Vec<String>> = meta.resolve.as_ref().map(|r| r.nodes.iter().map(|n| (n.id.as_str(), n.normal_dependencies())).collect()).unwrap_or_default();
	let enabled: BTreeMap<&str, &Vec<String>> = meta.resolve.as_ref().map(|r| r.nodes.iter().map(|n| (n.id.as_str(), &n.features)).collect()).unwrap_or_default();
	let mut queue: Vec<String> = Vec::new();
	for root in roots {
		match by_name.get(root.as_str()) {
			Some(p) => queue.push(p.id.clone()),
			None => die(&format!("root package `{root}` is not in {}", meta.workspace_root)),
		}
	}
	let mut seen: BTreeSet<String> = BTreeSet::new();
	let mut in_tree: Vec<CrateEntry> = Vec::new();
	let mut third: BTreeSet<String> = BTreeSet::new();
	while let Some(id) = queue.pop() {
		if !seen.insert(id.clone()) {
			continue;
		}
		let Some(package) = by_id.get(id.as_str()) else { continue };
		if !Path::new(&package.manifest_path).starts_with(repo) {
			third.insert(format!("{} {}", package.name, package.version));
			continue;
		}
		if exclude.iter().any(|e| e == &package.name) {
			continue;
		}
		let roots: Vec<String> = package.targets.iter().filter(|t| t.kind.iter().any(|k| matches!(k.as_str(), "lib" | "bin" | "rlib" | "staticlib" | "cdylib" | "proc-macro"))).map(|t| t.src_path.clone()).collect();
		let manifest = Path::new(&package.manifest_path).strip_prefix(repo).unwrap_or(Path::new(&package.manifest_path)).to_string_lossy().to_string();
		let mut features: Vec<String> = enabled.get(id.as_str()).map(|f| (*f).clone()).unwrap_or_default();
		features.sort();
		in_tree.push((package.name.clone(), manifest, roots, features));
		if let Some(list) = deps.get(id.as_str()) {
			for dep in list.iter() {
				queue.push(dep.clone());
			}
		}
	}
	in_tree.sort();
	(in_tree, third.into_iter().collect())
}

// The `OUT_DIR` a build of this row left for a crate: the newest `build/<crate>-*/out` under the
// row's target and profile directories.
fn out_dir(repo: &Path, row: &Row, crate_name: &str) -> Option<PathBuf> {
	let profile_dir = if row.profile == "release" { "release" } else { "debug" };
	let target_name = Path::new(&row.target).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| row.target.clone());
	let prefix = format!("{}-", crate_name.replace('-', "_"));
	let prefix_dash = format!("{crate_name}-");
	let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
	for target_dir in &row.target_dirs {
		let build = repo.join(target_dir).join(&target_name).join(profile_dir).join("build");
		let Ok(entries) = std::fs::read_dir(&build) else { continue };
		for entry in entries.flatten() {
			let name = entry.file_name().to_string_lossy().to_string();
			if !(name.starts_with(&prefix) || name.starts_with(&prefix_dash)) {
				continue;
			}
			let out = entry.path().join("out");
			if !out.is_dir() {
				continue;
			}
			let modified = std::fs::metadata(&out).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
			if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
				best = Some((modified, out));
			}
		}
	}
	best.map(|(_, p)| p)
}

// ---------------------------------------------------------------------------------------------
// The merged inventory

#[derive(Clone, Debug, serde::Serialize)]
struct Site {
	id: String,
	#[serde(rename = "crate")]
	crate_name: String,
	file: String,
	line: usize,
	kind: Kind,
	item: String,
	ordinal: usize,
	detail: String,
	cfg: Vec<String>,
	provenance: Provenance,
	// The rows in which this site is compiled into the artifact.
	reachable_in: Vec<String>,
	// The rows in which it is present in the source closure but behind a cfg the row does not set.
	present_not_reachable_in: Vec<String>,
	test_only: bool,
	in_tree_dependency: bool,
	emits: Vec<Kind>,
	expansions: usize,
	group: String,
	category: String,
	owner: String,
	disposition: String,
	notes: String,
}

fn site_id(found: &Found) -> String {
	use sha2::Digest;
	let mut hasher = sha2::Sha256::new();
	hasher.update(found.crate_name.as_bytes());
	hasher.update(b"\0");
	hasher.update(found.file.as_bytes());
	hasher.update(b"\0");
	hasher.update(found.kind.name().as_bytes());
	hasher.update(b"\0");
	hasher.update(found.item.as_bytes());
	hasher.update(b"\0");
	hasher.update(found.ordinal.to_string().as_bytes());
	let digest = hasher.finalize();
	digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------------------------
// The classification

#[derive(Clone, Debug, serde::Deserialize, Default)]
struct Classification {
	#[serde(default)]
	group: Vec<GroupRule>,
	#[serde(default)]
	site: Vec<SiteRule>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct GroupRule {
	id: String,
	invariant: String,
	#[serde(rename = "match")]
	matches: Vec<MatchRule>,
	category: String,
	owner: String,
	disposition: String,
	#[serde(default)]
	notes: String,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct MatchRule {
	#[serde(default)]
	file: Option<String>,
	#[serde(default)]
	kind: Vec<Kind>,
	#[serde(default)]
	item: Option<String>,
	#[serde(default, rename = "crate")]
	crate_name: Option<String>,
	// Match only test-only sites (true) or only production ones (false).
	#[serde(default)]
	test_only: Option<bool>,
	// Match only sites reachable in no row (true) or in at least one (false).
	#[serde(default)]
	unreachable: Option<bool>,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct SiteRule {
	id: String,
	#[serde(default)]
	group: Option<String>,
	#[serde(default)]
	category: Option<String>,
	#[serde(default)]
	owner: Option<String>,
	#[serde(default)]
	disposition: Option<String>,
	#[serde(default)]
	notes: Option<String>,
}

const CATEGORIES: [&str; 5] = ["required-caller-enforced", "contained-implementation", "overly-broad", "abi-linkage", "unclear-defect"];

// A glob over a path: `**` spans directories, `*` a name fragment.
fn glob_matches(pattern: &str, path: &str) -> bool {
	fn go(p: &[char], s: &[char]) -> bool {
		if p.is_empty() {
			return s.is_empty();
		}
		if p.len() >= 2 && p[0] == '*' && p[1] == '*' {
			let rest = if p.len() > 2 && p[2] == '/' { &p[3..] } else { &p[2..] };
			(0..=s.len()).any(|i| go(rest, &s[i..]))
		} else if p[0] == '*' {
			(0..=s.len()).take_while(|&i| i == 0 || s[i - 1] != '/').any(|i| go(&p[1..], &s[i..]))
		} else {
			!s.is_empty() && p[0] == s[0] && go(&p[1..], &s[1..])
		}
	}
	go(&pattern.chars().collect::<Vec<_>>(), &path.chars().collect::<Vec<_>>())
}

fn rule_matches(rule: &MatchRule, site: &Site) -> bool {
	if let Some(file) = &rule.file
		&& !glob_matches(file, &site.file)
	{
		return false;
	}
	if !rule.kind.is_empty() && !rule.kind.contains(&site.kind) {
		return false;
	}
	if let Some(item) = &rule.item
		&& !glob_matches(item, &site.item)
	{
		return false;
	}
	if let Some(name) = &rule.crate_name
		&& name != &site.crate_name
	{
		return false;
	}
	if let Some(test_only) = rule.test_only
		&& test_only != site.test_only
	{
		return false;
	}
	if let Some(unreachable) = rule.unreachable
		&& unreachable != site.reachable_in.is_empty()
	{
		return false;
	}
	true
}

// ---------------------------------------------------------------------------------------------
// Output

#[derive(Debug, serde::Serialize)]
struct Inventory {
	schema: &'static str,
	revision: String,
	tree: String,
	toolchain: BTreeMap<String, String>,
	regenerate: String,
	rows: Vec<RowReport>,
	groups: Vec<GroupOut>,
	sites: Vec<Site>,
	totals: Totals,
}

#[derive(Debug, serde::Serialize)]
struct GroupOut {
	id: String,
	invariant: String,
	category: String,
	owner: String,
	disposition: String,
	notes: String,
	sites: usize,
}

#[derive(Debug, Default, serde::Serialize)]
struct Totals {
	sites: usize,
	reachable_shipping: usize,
	test_only: usize,
	generated: usize,
	macro_templates: usize,
	present_not_reachable_anywhere: usize,
	unclassified: usize,
	by_kind: BTreeMap<String, usize>,
	by_row: BTreeMap<String, usize>,
	by_category: BTreeMap<String, usize>,
}

fn git(repo: &Path, args: &[&str]) -> String {
	Command::new("git").args(args).current_dir(repo).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

fn tool_version(tool: &str) -> String {
	Command::new(tool).arg("--version").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_else(|_| String::from("unavailable"))
}

fn main() {
	let mut repo: Option<PathBuf> = None;
	let mut matrix_path: Option<PathBuf> = None;
	let mut classification_path: Option<PathBuf> = None;
	let mut out_json: Option<PathBuf> = None;
	let mut out_md: Option<PathBuf> = None;
	let mut only_rows: Vec<String> = Vec::new();
	let mut expand: bool = false;
	let mut strict: bool = false;
	let mut argv = std::env::args().skip(1);
	while let Some(flag) = argv.next() {
		let mut value = || argv.next().unwrap_or_else(|| die(&format!("{flag} needs a value")));
		match flag.as_str() {
			"--repo" => repo = Some(PathBuf::from(value())),
			"--matrix" => matrix_path = Some(PathBuf::from(value())),
			"--classification" => classification_path = Some(PathBuf::from(value())),
			"--out-json" => out_json = Some(PathBuf::from(value())),
			"--out-md" => out_md = Some(PathBuf::from(value())),
			"--row" => only_rows.push(value()),
			"--expand" => expand = true,
			"--strict" => strict = true,
			"-h" | "--help" => {
				println!("usage: unsafe-inventory --repo ROOT [--matrix FILE] [--classification FILE] [--out-json FILE] [--out-md FILE] [--row ID]... [--expand] [--strict]");
				println!("  --strict   exit 1 when any site is unclassified or any row reported a scan problem");
				return;
			}
			other => die(&format!("unknown argument {other}")),
		}
	}
	let repo = repo.unwrap_or_else(|| die("--repo is required")).canonicalize().unwrap_or_else(|e| die(&format!("--repo: {e}")));
	let matrix_path = matrix_path.unwrap_or_else(|| repo.join("src/tools/unsafe-inventory/matrix.toml"));
	let classification_path = classification_path.unwrap_or_else(|| repo.join("AI/audit/unsafe-classification.toml"));
	let matrix: Matrix = toml::from_str(&std::fs::read_to_string(&matrix_path).unwrap_or_else(|e| die(&format!("cannot read {}: {e}", matrix_path.display())))).unwrap_or_else(|e| die(&format!("{}: {e}", matrix_path.display())));
	let classification: Classification = match std::fs::read_to_string(&classification_path) {
		Ok(text) => toml::from_str(&text).unwrap_or_else(|e| die(&format!("{}: {e}", classification_path.display()))),
		Err(_) => Classification::default(),
	};
	for rule in &classification.group {
		if !CATEGORIES.contains(&rule.category.as_str()) {
			die(&format!("group `{}` has category `{}`, not one of {}", rule.id, rule.category, CATEGORIES.join(", ")));
		}
	}

	// THE SCAN, ROW BY ROW.
	let mut merged: BTreeMap<String, Site> = BTreeMap::new();
	let mut rows_out: Vec<RowReport> = Vec::new();
	// Macro invocations, by crate and name: a template's expansions are the invocations of its
	// name in its own crate (thirty protocol crates each define a `forward!` of their own).
	let mut invocations: BTreeMap<(String, String), usize> = BTreeMap::new();
	let mut expansion_notes: Vec<String> = Vec::new();
	for row in &matrix.row {
		if !only_rows.is_empty() && !only_rows.contains(&row.id) {
			continue;
		}
		if !matches!(row.kind.as_str(), "shipping" | "development" | "test") {
			die(&format!("row {} has kind `{}`, not shipping, development or test", row.id, row.kind));
		}
		let roots = row_roots(&repo, row);
		if roots.is_empty() {
			die(&format!("row {} names no root crate", row.id));
		}
		let mut by_manifest: BTreeMap<(String, Vec<String>, bool), Vec<String>> = BTreeMap::new();
		for root in &roots {
			let (features, no_default) = if root.features.is_empty() && !root.no_default_features { (row.features.clone(), row.no_default_features) } else { (root.features.clone(), root.no_default_features) };
			by_manifest.entry((root.manifest.clone(), features, no_default)).or_default().push(root.package.clone());
		}
		let mut crates: Vec<CrateEntry> = Vec::new();
		let mut third_party: BTreeSet<String> = BTreeSet::new();
		for ((manifest_rel, features, no_default), packages) in &by_manifest {
			let meta = metadata(&repo, row, manifest_rel, features, *no_default);
			let (found, third) = closure(&repo, &meta, packages, &matrix.exclude_crates);
			for entry in found {
				// A crate reached from two roots with different feature sets is scanned under the
				// UNION of what was enabled for it, which is what the linked image contains.
				if let Some(existing) = crates.iter_mut().find(|c| c.0 == entry.0) {
					for feature in entry.3 {
						if !existing.3.contains(&feature) {
							existing.3.push(feature);
						}
					}
					existing.3.sort();
				} else {
					crates.push(entry);
				}
			}
			third_party.extend(third);
		}
		crates.sort();
		let cfg = CfgSet::parse(&row.cfg);
		let mut report = RowReport { id: row.id.clone(), kind: row.kind.clone(), description: row.description.clone(), target: row.target.clone(), profile: row.profile.clone(), features: row.features.clone(), no_default_features: row.no_default_features, build_std: row.build_std.clone(), rustflags: row.rustflags.clone(), env: row.env.clone(), cfg: row.cfg.clone(), command: build_command(row, &roots), crates: Vec::new(), third_party: third_party.into_iter().collect(), files_scanned: 0, problems: Vec::new(), crates_with_out_dir: 0 };
		let root_names: Vec<String> = roots.iter().map(|r| r.package.clone()).collect();
		let root_crates: BTreeSet<&str> = root_names.iter().map(String::as_str).collect();
		for (name, manifest, roots, features) in &crates {
			let out = out_dir(&repo, row, name);
			if out.is_some() {
				report.crates_with_out_dir += 1;
			}
			// The row's cfg plus this crate's resolved features.
			let mut crate_cfg = cfg.clone();
			for feature in features {
				crate_cfg.pairs.push((String::from("feature"), feature.clone()));
			}
			let mut scanner = Scanner { repo: &repo, crate_name: name.clone(), cfg: crate_cfg, out_dir: out, scan: Default::default() };
			for root in roots {
				scanner.scan_root(Path::new(root));
			}
			let scan = scanner.scan;
			report.files_scanned += scan.files.len();
			report.problems.extend(scan.problems.iter().map(|p| format!("{name}: {p}")));
			for (macro_name, _item, _file, _line) in &scan.invocations {
				*invocations.entry((name.clone(), macro_name.clone())).or_insert(0) += 1;
			}
			let dependency = !root_crates.contains(name.as_str());
			for found in scan.found {
				let id = site_id(&found);
				let entry = merged.entry(id.clone()).or_insert_with(|| Site { id: id.clone(), crate_name: found.crate_name.clone(), file: found.file.clone(), line: found.line, kind: found.kind, item: found.item.clone(), ordinal: found.ordinal, detail: found.detail.clone(), cfg: found.cfgs.clone(), provenance: found.provenance.clone(), reachable_in: Vec::new(), present_not_reachable_in: Vec::new(), test_only: found.test_only, in_tree_dependency: dependency, emits: found.emits.clone(), expansions: 0, group: String::new(), category: String::from("unclassified"), owner: String::new(), disposition: String::new(), notes: String::new() });
				if found.active && !found.test_only {
					if !entry.reachable_in.contains(&row.id) {
						entry.reachable_in.push(row.id.clone());
					}
				} else if !entry.present_not_reachable_in.contains(&row.id) {
					entry.present_not_reachable_in.push(row.id.clone());
				}
				entry.test_only = entry.test_only && found.test_only;
				entry.in_tree_dependency = entry.in_tree_dependency && dependency;
			}
			report.crates.push(CrateRow { name: name.clone(), manifest: manifest.clone(), roots: roots.iter().map(|r| Path::new(r).strip_prefix(&repo).unwrap_or(Path::new(r)).to_string_lossy().to_string()).collect(), features: features.clone() });
		}
		if expand {
			expansion_notes.extend(reconcile_expanded(&repo, row, &report.crates, &cfg, &merged));
		}
		rows_out.push(report);
	}

	// MACRO TEMPLATES: how often each is invoked, across every scanned row.
	for site in merged.values_mut() {
		if site.kind == Kind::MacroTemplate {
			let name = site.item.rsplit("macro_rules! ").next().unwrap_or_default().to_string();
			site.expansions = *invocations.get(&(site.crate_name.clone(), name)).unwrap_or(&0);
		}
	}

	// THE CLASSIFICATION: group rules in order, first match wins; site rules override.
	let site_rules: BTreeMap<&str, &SiteRule> = classification.site.iter().map(|r| (r.id.as_str(), r)).collect();
	let mut group_counts: BTreeMap<String, usize> = BTreeMap::new();
	for site in merged.values_mut() {
		for rule in &classification.group {
			if rule.matches.iter().any(|m| rule_matches(m, site)) {
				site.group = rule.id.clone();
				site.category = rule.category.clone();
				site.owner = rule.owner.clone();
				site.disposition = rule.disposition.clone();
				site.notes = rule.notes.clone();
				break;
			}
		}
		if let Some(rule) = site_rules.get(site.id.as_str()) {
			if let Some(group) = &rule.group {
				site.group = group.clone();
			}
			if let Some(category) = &rule.category {
				if !CATEGORIES.contains(&category.as_str()) {
					die(&format!("site {} has category `{category}`, not one of {}", site.id, CATEGORIES.join(", ")));
				}
				site.category = category.clone();
			}
			if let Some(owner) = &rule.owner {
				site.owner = owner.clone();
			}
			if let Some(disposition) = &rule.disposition {
				site.disposition = disposition.clone();
			}
			if let Some(notes) = &rule.notes {
				site.notes = notes.clone();
			}
		}
		*group_counts.entry(site.group.clone()).or_insert(0) += 1;
	}
	for rule in &classification.site {
		if !merged.contains_key(rule.id.as_str()) {
			eprintln!("unsafe-inventory: site rule `{}` names no site in this inventory - stale after an edit, or a typo", rule.id);
		}
	}

	// TOTALS.
	let mut totals = Totals::default();
	let shipping_rows: BTreeSet<&str> = matrix.row.iter().filter(|r| r.kind == "shipping").map(|r| r.id.as_str()).collect();
	for site in merged.values() {
		totals.sites += 1;
		if site.reachable_in.iter().any(|r| shipping_rows.contains(r.as_str())) {
			totals.reachable_shipping += 1;
		}
		if site.test_only {
			totals.test_only += 1;
		}
		if matches!(site.provenance, Provenance::Generated { .. }) {
			totals.generated += 1;
		}
		if site.kind == Kind::MacroTemplate {
			totals.macro_templates += 1;
		}
		if site.reachable_in.is_empty() && !site.test_only {
			totals.present_not_reachable_anywhere += 1;
		}
		if site.category == "unclassified" {
			totals.unclassified += 1;
		}
		*totals.by_kind.entry(site.kind.name().to_string()).or_insert(0) += 1;
		for row in &site.reachable_in {
			*totals.by_row.entry(row.clone()).or_insert(0) += 1;
		}
		*totals.by_category.entry(site.category.clone()).or_insert(0) += 1;
	}

	let groups: Vec<GroupOut> = classification.group.iter().map(|g| GroupOut { id: g.id.clone(), invariant: g.invariant.clone(), category: g.category.clone(), owner: g.owner.clone(), disposition: g.disposition.clone(), notes: g.notes.clone(), sites: *group_counts.get(&g.id).unwrap_or(&0) }).collect();
	let mut toolchain = BTreeMap::new();
	toolchain.insert(String::from("rustc"), tool_version("rustc"));
	toolchain.insert(String::from("cargo"), tool_version("cargo"));
	for (dir, key) in [("src/user/rust-toolchain.toml", "userspace-channel"), ("src/kernel/rust-toolchain.toml", "kernel-channel")] {
		if let Ok(text) = std::fs::read_to_string(repo.join(dir)) {
			let channel = text.lines().find(|l| l.trim_start().starts_with("channel")).map(|l| l.split('=').nth(1).unwrap_or("").trim().trim_matches('"').to_string()).unwrap_or_default();
			toolchain.insert(String::from(key), channel);
		}
	}
	let changed = git(&repo, &["status", "--porcelain"]);
	let tree = if changed.is_empty() { String::from("clean") } else { format!("dirty ({} changed path(s))", changed.lines().count()) };
	let inventory = Inventory { schema: "libersystem-unsafe-inventory/1", revision: git(&repo, &["rev-parse", "HEAD"]), tree, toolchain, regenerate: String::from("cd src/tools/unsafe-inventory && cargo run --quiet -- --repo ../../.. [--expand]"), rows: rows_out, groups, sites: merged.into_values().collect(), totals };
	let json = serde_json::to_string_pretty(&inventory).unwrap_or_else(|e| die(&format!("cannot encode the inventory: {e}")));
	let out_json = out_json.unwrap_or_else(|| repo.join("AI/audit/unsafe-inventory.json"));
	if let Some(parent) = out_json.parent() {
		let _ = std::fs::create_dir_all(parent);
	}
	std::fs::write(&out_json, json).unwrap_or_else(|e| die(&format!("cannot write {}: {e}", out_json.display())));
	let out_md = out_md.unwrap_or_else(|| repo.join("AI/audit/unsafe-inventory.md"));
	std::fs::write(&out_md, render_markdown(&inventory, &expansion_notes)).unwrap_or_else(|e| die(&format!("cannot write {}: {e}", out_md.display())));
	eprintln!("unsafe-inventory: {} site(s) across {} row(s); {} reachable in a shipping row, {} test-only, {} generated, {} macro template(s), {} unclassified -> {} and {}", inventory.totals.sites, inventory.rows.len(), inventory.totals.reachable_shipping, inventory.totals.test_only, inventory.totals.generated, inventory.totals.macro_templates, inventory.totals.unclassified, out_json.display(), out_md.display());
	let problems: usize = inventory.rows.iter().map(|r| r.problems.len()).sum();
	if problems > 0 {
		eprintln!("unsafe-inventory: {problems} scan problem(s) - see the rows in the JSON");
	}
	if strict && (inventory.totals.unclassified > 0 || problems > 0) {
		std::process::exit(1);
	}
}

fn build_command(row: &Row, roots: &[RootCrate]) -> String {
	let mut parts: Vec<String> = Vec::new();
	for (k, v) in &row.env {
		parts.push(format!("{k}={v}"));
	}
	parts.push(String::from("cargo build"));
	if row.profile == "release" {
		parts.push(String::from("--release"));
	}
	parts.push(format!("--target {}", row.target));
	if row.no_default_features {
		parts.push(String::from("--no-default-features"));
	}
	if !row.features.is_empty() {
		parts.push(format!("--features {}", row.features.join(",")));
	}
	if !row.build_std.is_empty() {
		parts.push(format!("-Z build-std={}", row.build_std.join(",")));
	}
	if !row.rustflags.is_empty() {
		parts.insert(0, format!("RUSTFLAGS=\"{}\"", row.rustflags.join(" ")));
	}
	parts.push(format!("(roots: {})", roots.iter().map(|r| format!("{}@{}", r.package, r.manifest)).collect::<Vec<_>>().join(", ")));
	parts.join(" ")
}

// `rustc -Zunpretty=expanded` for each crate of the row, parsed with the same scanner and compared
// with the source scan: a site the expansion has and the source does not is macro-produced (and
// named by the item it lands in); a source site that is reachable and absent from the expansion is
// reported as a discrepancy. Nothing here changes the inventory; it is the check that the source
// scan's reachability agrees with what the compiler expands.
fn reconcile_expanded(repo: &Path, row: &Row, crates: &[CrateRow], cfg: &CfgSet, merged: &BTreeMap<String, Site>) -> Vec<String> {
	let mut notes = Vec::new();
	for krate in crates {
		let manifest = repo.join(&krate.manifest);
		let mut command = Command::new("cargo");
		command.arg("rustc").arg("--quiet").arg("--manifest-path").arg(&manifest).arg("--offline");
		if row.profile == "release" {
			command.arg("--release");
		}
		let target_arg = if row.target.ends_with(".json") { repo.join(&row.target).to_string_lossy().to_string() } else { row.target.clone() };
		command.arg("--target").arg(&target_arg);
		if row.no_default_features {
			command.arg("--no-default-features");
		}
		if !row.features.is_empty() {
			command.arg("--features").arg(row.features.join(","));
		}
		if !row.build_std.is_empty() {
			command.arg(format!("-Zbuild-std={}", row.build_std.join(",")));
			command.arg("-Zbuild-std-features=compiler-builtins-mem");
		}
		command.arg("--").arg("-Zunpretty=expanded");
		command.env("RUSTC_BOOTSTRAP", "1");
		let expanded_dir = repo.join(row.target_dirs.first().cloned().unwrap_or_else(|| String::from(".build/cargo/unsafe-inventory"))).join("expanded");
		command.env("CARGO_TARGET_DIR", &expanded_dir);
		if !row.rustflags.is_empty() {
			command.env("RUSTFLAGS", row.rustflags.join(" "));
		}
		for (k, v) in &row.env {
			command.env(k, v);
		}
		let output = match command.output() {
			Ok(output) => output,
			Err(error) => {
				notes.push(format!("{}/{}: cargo rustc could not run: {error}", row.id, krate.name));
				continue;
			}
		};
		if !output.status.success() {
			notes.push(format!("{}/{}: expansion failed: {}", row.id, krate.name, String::from_utf8_lossy(&output.stderr).lines().last().unwrap_or("")));
			continue;
		}
		let text = String::from_utf8_lossy(&output.stdout).to_string();
		let parsed = match syn::parse_file(&text) {
			Ok(parsed) => parsed,
			Err(error) => {
				notes.push(format!("{}/{}: the expanded crate does not parse: {error}", row.id, krate.name));
				continue;
			}
		};
		// Count boundaries per (kind) in the expanded crate; the whole crate is one file here.
		let mut scanner = Scanner { repo, crate_name: krate.name.clone(), cfg: cfg.clone(), out_dir: None, scan: Default::default() };
		let tmp = expanded_dir.join(format!("{}.expanded.rs", krate.name));
		let _ = std::fs::create_dir_all(tmp.parent().unwrap_or(Path::new(".")));
		let _ = std::fs::write(&tmp, &text);
		let _ = parsed;
		scanner.scan_root(&tmp);
		let mut expanded: BTreeMap<Kind, usize> = BTreeMap::new();
		for found in &scanner.scan.found {
			if found.active && found.kind != Kind::MacroTemplate {
				*expanded.entry(found.kind).or_insert(0) += 1;
			}
		}
		let mut source: BTreeMap<Kind, usize> = BTreeMap::new();
		for site in merged.values() {
			if site.crate_name == krate.name && site.reachable_in.contains(&row.id) && site.kind != Kind::MacroTemplate {
				*source.entry(site.kind).or_insert(0) += 1;
			}
		}
		for kind in Kind::all() {
			let e = *expanded.get(&kind).unwrap_or(&0);
			let s = *source.get(&kind).unwrap_or(&0);
			if e != s {
				notes.push(format!("{}/{}: {} - source scan {s}, expansion {e} ({})", row.id, krate.name, kind.name(), if e > s { "macro-produced or generated sites beyond the source" } else { "source sites the expansion does not carry" }));
			}
		}
		notes.push(format!("{}/{}: expanded and compared ({} site kinds)", row.id, krate.name, expanded.len()));
	}
	notes
}

fn render_markdown(inventory: &Inventory, expansion_notes: &[String]) -> String {
	let mut out = String::new();
	out.push_str("# Unsafe-boundary inventory\n\n");
	out.push_str(&format!("Schema `{}`, revision `{}` ({}), rustc `{}`, cargo `{}`.\n\n", inventory.schema, inventory.revision, inventory.tree, inventory.toolchain.get("rustc").cloned().unwrap_or_default(), inventory.toolchain.get("cargo").cloned().unwrap_or_default()));
	out.push_str(&format!("Regenerate: `{}`. This file is rendered from `unsafe-inventory.json`; edit the classification, not this.\n\n", inventory.regenerate));
	out.push_str("## The configuration matrix\n\n| row | kind | target | profile | features | build-std | rustflags | crates | third-party excluded | files |\n| --- | --- | --- | --- | --- | --- | --- | ---: | ---: | ---: |\n");
	for row in &inventory.rows {
		out.push_str(&format!("| `{}` | {} | `{}` | {} | {} | {} | `{}` | {} | {} | {} |\n", row.id, row.kind, row.target, row.profile, if row.features.is_empty() { String::from("default") } else { format!("{}{}", if row.no_default_features { "no-default + " } else { "" }, row.features.join(",")) }, row.build_std.join(","), row.rustflags.join(" "), row.crates.len(), row.third_party.len(), row.files_scanned));
	}
	out.push_str("\nEach row's build command, `cfg` set and crate closure are in the JSON. ");
	out.push_str("A crate whose generated file cannot be read is a problem on its row, never a silent omission.\n\n");
	for row in &inventory.rows {
		out.push_str(&format!("- `{}`: {} - `{}`; cfg: {}; {} crate(s) with a built OUT_DIR.\n", row.id, row.description, row.command, row.cfg.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", "), row.crates_with_out_dir));
		for problem in &row.problems {
			out.push_str(&format!("  - problem: {problem}\n"));
		}
	}
	out.push_str("\n## Totals\n\n");
	let t = &inventory.totals;
	out.push_str(&format!("| sites | reachable in a shipping row | test-only | generated | macro templates | present in no row | unclassified |\n| ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n| {} | {} | {} | {} | {} | {} | {} |\n\n", t.sites, t.reachable_shipping, t.test_only, t.generated, t.macro_templates, t.present_not_reachable_anywhere, t.unclassified));
	out.push_str("| kind | sites |\n| --- | ---: |\n");
	for (kind, n) in &t.by_kind {
		out.push_str(&format!("| `{kind}` | {n} |\n"));
	}
	out.push_str("\n| row | reachable sites |\n| --- | ---: |\n");
	for (row, n) in &t.by_row {
		out.push_str(&format!("| `{row}` | {n} |\n"));
	}
	out.push_str("\n| category | sites |\n| --- | ---: |\n");
	for (category, n) in &t.by_category {
		out.push_str(&format!("| {category} | {n} |\n"));
	}
	out.push_str("\n## Groups\n\n| group | category | owner | disposition | sites | invariant |\n| --- | --- | --- | --- | ---: | --- |\n");
	for group in &inventory.groups {
		out.push_str(&format!("| `{}` | {} | {} | {} | {} | {} |\n", group.id, group.category, group.owner, group.disposition, group.sites, group.invariant.replace('|', "\\|")));
	}
	// Per-crate table of reachable sites by kind.
	let mut per_crate: BTreeMap<&str, BTreeMap<&str, usize>> = BTreeMap::new();
	for site in &inventory.sites {
		if site.reachable_in.is_empty() {
			continue;
		}
		*per_crate.entry(site.crate_name.as_str()).or_default().entry(site.kind.name()).or_insert(0) += 1;
	}
	out.push_str("\n## Reachable sites per crate\n\n| crate | unsafe fn | unsafe block | unsafe impl | raw pointer | static mut | asm | global asm | extern | linkage attr | templates |\n| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
	for (krate, kinds) in &per_crate {
		let g = |k: &str| *kinds.get(k).unwrap_or(&0);
		out.push_str(&format!("| `{krate}` | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n", g("unsafe-fn"), g("unsafe-block"), g("unsafe-impl") + g("unsafe-trait"), g("raw-pointer"), g("static-mut"), g("inline-asm") + g("naked-asm"), g("global-asm"), g("extern-block") + g("extern-abi-fn"), g("linkage-attr"), g("macro-template")));
	}
	if !expansion_notes.is_empty() {
		out.push_str("\n## Expansion reconciliation\n\n");
		for note in expansion_notes {
			out.push_str(&format!("- {note}\n"));
		}
	}
	let unclassified: Vec<&Site> = inventory.sites.iter().filter(|s| s.category == "unclassified").collect();
	out.push_str(&format!("\n## Unclassified sites ({})\n\n", unclassified.len()));
	if unclassified.is_empty() {
		out.push_str("None: every site is under a group or a site rule.\n");
	} else {
		out.push_str("| id | crate | file:line | kind | item | reachable in |\n| --- | --- | --- | --- | --- | --- |\n");
		for site in unclassified.iter().take(400) {
			out.push_str(&format!("| `{}` | `{}` | `{}:{}` | `{}` | `{}` | {} |\n", site.id, site.crate_name, site.file, site.line, site.kind.name(), site.item.replace('|', "\\|"), if site.reachable_in.is_empty() { String::from("-") } else { site.reachable_in.join(", ") }));
		}
		if unclassified.len() > 400 {
			out.push_str(&format!("\n... and {} more in the JSON.\n", unclassified.len() - 400));
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	fn fixture_root() -> PathBuf {
		Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
	}

	fn repo_root() -> PathBuf {
		Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(3).expect("src/tools/unsafe-inventory is three below the root").to_path_buf()
	}

	fn scan_probe(cfg: &[&str], out_dir: Option<PathBuf>) -> scan::CrateScan {
		let mut scanner = Scanner { repo: &repo_root(), crate_name: String::from("inventory-fixture-probe"), cfg: CfgSet::parse(&cfg.iter().map(|c| c.to_string()).collect::<Vec<_>>()), out_dir, scan: Default::default() };
		scanner.scan_root(&fixture_root().join("probe/src/lib.rs"));
		scanner.scan
	}

	// THE COMPLETENESS FIXTURES: a class of site that goes missing from the scan fails here rather
	// than silently thinning the audit.
	#[test]
	fn a_cfg_only_site_is_present_in_every_row_and_reachable_only_where_its_cfg_holds() {
		let riscv = scan_probe(&[r#"target_arch = "riscv64""#], None);
		let x86 = scan_probe(&[r#"target_arch = "x86_64""#], None);
		let on_riscv = riscv.found.iter().find(|f| f.item.ends_with("only_on_riscv") && f.kind == Kind::UnsafeFn).expect("the cfg-only site is found on riscv64");
		let on_x86 = x86.found.iter().find(|f| f.item.ends_with("only_on_riscv") && f.kind == Kind::UnsafeFn).expect("the cfg-only site is present in the x86_64 scan too");
		assert!(on_riscv.active, "reachable where the cfg holds");
		assert!(!on_x86.active, "present and not reachable where it does not");
		assert_eq!(on_x86.cfgs, vec![r#"target_arch = "riscv64""#.to_string()], "the cfg chain is recorded");
		assert_eq!(site_id(on_riscv), site_id(on_x86), "one site, one id, across rows");
	}

	#[test]
	fn a_macro_template_is_a_site_and_its_expansions_are_counted() {
		let scan = scan_probe(&[], None);
		let template = scan.found.iter().find(|f| f.kind == Kind::MacroTemplate).expect("the template is a site");
		assert!(template.item.ends_with("macro_rules! read_volatile_at"));
		assert_eq!(template.emits, vec![Kind::UnsafeBlock, Kind::RawPointer].into_iter().filter(|k| template.emits.contains(k)).collect::<Vec<_>>());
		assert!(template.emits.contains(&Kind::UnsafeBlock), "the template's body carries an unsafe block");
		assert_eq!(scan.invocations.iter().filter(|(name, _, _, _)| name == "read_volatile_at").count(), 2, "both invocations are counted for the template");
		assert!(!scan.found.iter().any(|f| f.kind == Kind::UnsafeBlock && f.item.ends_with("expands_the_template")), "the expansion's unsafe is not a source site of the calling function - it belongs to the template");
	}

	#[test]
	fn a_generated_site_is_read_from_out_dir_with_its_provenance_and_missing_out_dir_is_a_problem() {
		let out = std::env::temp_dir().join(format!("unsafe-inventory-fixture-{}", std::process::id()));
		std::fs::create_dir_all(&out).expect("temp out dir");
		std::fs::write(out.join("generated_boundary.rs"), "pub unsafe fn generated_boundary() -> u32 { 7 }\n").expect("write");
		let scan = scan_probe(&[], Some(out.clone()));
		let generated = scan.found.iter().find(|f| f.item.ends_with("generated_boundary")).expect("the generated site is found");
		assert_eq!(generated.kind, Kind::UnsafeFn);
		match &generated.provenance {
			Provenance::Generated { generator, included_from } => {
				assert_eq!(generator, "inventory-fixture-probe/build.rs");
				assert!(included_from.ends_with("probe/src/lib.rs:7"), "the include site is named: {included_from}");
			}
			other => panic!("provenance {other:?}"),
		}
		let _ = std::fs::remove_dir_all(&out);
		let without = scan_probe(&[], None);
		assert!(!without.found.iter().any(|f| f.item.ends_with("generated_boundary")), "no OUT_DIR, no generated site");
		assert!(without.problems.iter().any(|p| p.contains("OUT_DIR")), "and the scan says so rather than staying silent: {:?}", without.problems);
	}

	#[test]
	fn a_test_only_site_is_recorded_and_not_reachable_in_a_shipping_row() {
		let scan = scan_probe(&[], None);
		let site = scan.found.iter().find(|f| f.item.ends_with("only_in_tests")).expect("the test-only site is found");
		assert!(site.test_only && !site.active);
		let with_test = scan_probe(&["test"], None);
		let site = with_test.found.iter().find(|f| f.item.ends_with("only_in_tests")).expect("found under test");
		assert!(!site.test_only && site.active, "a row that sets `test` reaches it");
	}

	#[test]
	fn an_in_tree_dependency_is_in_the_closure_and_a_registry_crate_is_not() {
		let repo = repo_root();
		let row = Row { id: String::from("fixture"), kind: String::from("shipping"), description: String::new(), roots: vec![RootCrate { manifest: String::from("src/tools/unsafe-inventory/fixtures/probe/Cargo.toml"), package: String::from("inventory-fixture-probe"), features: vec![], no_default_features: false }], manifest_roots: None, target: String::from("x86_64-unknown-linux-gnu"), profile: String::from("dev"), features: vec![], no_default_features: false, build_std: vec![], rustflags: vec![], env: BTreeMap::new(), cfg: vec![], target_dirs: vec![] };
		let meta = metadata(&repo, &row, &row.roots[0].manifest, &[], false);
		let (crates, third) = closure(&repo, &meta, &[String::from("inventory-fixture-probe")], &[]);
		let names: Vec<&str> = crates.iter().map(|c| c.0.as_str()).collect();
		assert!(names.contains(&"inventory-fixture-dep"), "the path dependency is in the closure: {names:?}");
		assert!(names.contains(&"inventory-fixture-probe"));
		assert!(third.is_empty(), "the fixture has no registry dependency: {third:?}");
		let mut scanner = Scanner { repo: &repo, crate_name: String::from("inventory-fixture-dep"), cfg: CfgSet::default(), out_dir: None, scan: Default::default() };
		scanner.scan_root(&fixture_root().join("dep/src/lib.rs"));
		assert!(scanner.scan.found.iter().any(|f| f.kind == Kind::UnsafeFn && f.item.ends_with("dependency_boundary")), "the dependency's own site is scanned");
	}

	#[test]
	fn the_baseline_sites_have_stable_ids_and_the_expected_kinds() {
		let scan = scan_probe(&[], None);
		let counter = scan.found.iter().find(|f| f.kind == Kind::StaticMut).expect("static mut COUNTER");
		assert!(counter.item.ends_with("COUNTER"));
		let block = scan.found.iter().find(|f| f.kind == Kind::UnsafeBlock && f.item.ends_with("uses_the_dependency")).expect("the unsafe block in the caller");
		assert_eq!(block.ordinal, 1);
		let pointer = scan.found.iter().find(|f| f.kind == Kind::RawPointer && f.item.ends_with("uses_the_dependency")).expect("the raw pointer parameter");
		assert!(pointer.detail.contains("parameter pointer"));
		assert_eq!(site_id(block).len(), 16, "an id is sixteen hex digits");
		assert_eq!(site_id(block), site_id(&scan_probe(&[], None).found.iter().find(|f| f.kind == Kind::UnsafeBlock && f.item.ends_with("uses_the_dependency")).cloned().unwrap()), "the same site scans to the same id");
	}

	#[test]
	fn cfg_predicates_evaluate_as_rustc_would() {
		let set = CfgSet::parse(&[r#"target_arch = "x86_64""#.to_string(), "debug_assertions".to_string(), r#"feature = "development""#.to_string()]);
		let eval = |text: &str| eval_cfg(&syn::parse_str::<Meta>(text).expect("meta"), &set);
		assert!(eval(r#"target_arch = "x86_64""#));
		assert!(!eval(r#"target_arch = "aarch64""#));
		assert!(eval("debug_assertions"));
		assert!(!eval("test"));
		assert!(eval(r#"all(target_arch = "x86_64", debug_assertions)"#));
		assert!(eval(r#"any(target_arch = "aarch64", feature = "development")"#));
		assert!(eval(r#"not(target_arch = "riscv64")"#));
		assert!(!eval(r#"all(target_arch = "x86_64", not(debug_assertions))"#));
		assert!(!eval("unknown_predicate"), "a predicate the row does not define is false");
	}

	#[test]
	fn globs_span_directories_only_with_two_stars() {
		assert!(glob_matches("src/kernel/**", "src/kernel/mem/tlb.rs"));
		assert!(glob_matches("src/kernel/*/mod.rs", "src/kernel/mem/mod.rs"));
		assert!(!glob_matches("src/kernel/*/mod.rs", "src/kernel/mem/deep/mod.rs"));
		assert!(glob_matches("**/tests.rs", "src/kernel/dma_policy/tests.rs"));
		assert!(!glob_matches("src/user/**", "src/kernel/main.rs"));
	}
}
