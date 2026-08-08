// Every Cargo crate in the tree, and the edges its manifest states.
//
// Read from the manifests rather than from `cargo metadata`, and that is a deliberate choice
// M0148 argues at length: `cargo metadata` resolves ONE configuration, and in this tree the default
// configuration is the wrong one. `tools` reaches its twenty client libraries only through
// `shared-image`; all fifteen protocol crates reach `ipc-client` only through
// `channel-client-impl`; and the image is built `--no-default-features --features shared-image`. A
// resolver asked in the default configuration reports that nothing depends on `ipc-client` - the
// IPC plumbing every service and every tool links.
//
// So an edge that exists under ANY buildable configuration is an edge here. `optional = true` is
// recorded, not obeyed. Over-selection from an inactive edge costs a test run; under-selection from
// an edge that was active in the shipped build costs a shipped regression.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DependencyKind {
	// Changes the shipped binary.
	Normal,
	// Rebuilds the test binary; does not ship. Kept apart from Normal because conflating them
	// drags the kernel into the closure of every codec: the kernel dev-depends on eleven userspace
	// codecs to build the fixtures its audioconv and imgconv scenarios assert against.
	Dev,
	// May change the dependent's build output through its build script - and that is deliberately
	// broader than "generates source". The build scripts here also emit `cargo:rustc-link-arg`
	// (the linker script, in both src/kernel/build.rs and src/user/build.rs) and
	// `cargo:rustc-env`. Defining this edge as code generation would invite an implementation that
	// only looks for generated files.
	Build,
}

impl DependencyKind {
	pub fn edge_kind(self) -> &'static str {
		match self {
			DependencyKind::Normal => "link.static",
			DependencyKind::Dev => "link.static.dev",
			DependencyKind::Build => "generation.build",
		}
	}
}

#[derive(Clone, Debug)]
pub struct Dependency {
	pub name: String,
	pub kind: DependencyKind,
	// Recorded so a report can say an edge is feature-gated. Never used to drop the edge.
	pub optional: bool,
}

// One `[[bin]]`, which is a component in its own right.
//
// This is what makes tool-level scoping work at all. Fifty-odd programs live in the one `tools`
// crate, so collapsing them to their crate would make every tool change a change to every tool -
// the driver-crate problem M0148 names, at fifty times the size. A `[[bin]]` names exactly one
// source file, and services/manifest.toml records providers per PROGRAM, so keeping programs
// distinct is also what lets a change to `flac` reach `audioconv` and `play` and nothing else.
#[derive(Clone, Debug)]
pub struct Binary {
	pub name: String,
	// Repository-relative path of the bin's root source file.
	pub path: String,
	// `required-features = ["development"]` - the bin exists only in some configurations, which is
	// how `dev_agent` is built for the development profile and not for the shipping one.
	pub required_features: BTreeSet<String>,
}

#[derive(Clone, Debug)]
pub struct Crate {
	pub name: String,
	// Repository-relative, forward slashes, no trailing slash: "src/user/libs/audio/flac".
	pub dir: String,
	pub dependencies: Vec<Dependency>,
	pub binaries: Vec<Binary>,
	pub features: BTreeSet<String>,
	// What each feature MEANS, not merely that it exists. `shared-image = ["a", "b"]` becoming
	// `["a", "b", "c"]` changes what ships while leaving the name, the feature set and often the
	// unioned graph untouched - so a model hash over names alone would keep vouching for evidence
	// gathered against a different build. That is the same mistake the configuration catalog was
	// added to prevent, one level further down.
	pub feature_definitions: Vec<(String, Vec<String>)>,
	// The build script this crate names, repository-relative. Cargo states it as `build = "..."` and
	// `cargo metadata` does not expose it as a dependency, so the edge to `src/user/build.rs` - the
	// script that picks one of three linker scripts - was invisible to the model.
	pub build_script: Option<String>,
	// Whether anything here is a `#[test]`. The kernel's `#[test_case]` suite is NOT one: it runs
	// inside a booted guest, is selected by tags, and cannot be run by `cargo test` on the host.
	pub has_host_tests: bool,
}

pub fn discover(repo_root: &Path) -> Result<Vec<Crate>, String> {
	let mut manifests = Vec::new();
	collect_manifests(&repo_root.join("src"), &mut manifests)?;
	manifests.sort();
	let mut crates = Vec::new();
	for manifest in manifests {
		crates.push(parse(repo_root, &manifest)?);
	}
	crates.sort_by(|left, right| left.name.cmp(&right.name));
	let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
	for entry in &crates {
		if let Some(previous) = seen.insert(entry.name.as_str(), entry.dir.as_str()) {
			return Err(format!("two crates are both named '{}': {previous} and {}", entry.name, entry.dir));
		}
	}
	Ok(crates)
}

fn collect_manifests(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
	let entries = match fs::read_dir(dir) {
		Ok(entries) => entries,
		Err(error) => return Err(format!("{}: {error}", dir.display())),
	};
	for entry in entries {
		let entry = entry.map_err(|error| format!("{}: {error}", dir.display()))?;
		let path = entry.path();
		let name = entry.file_name().to_string_lossy().into_owned();
		if path.is_dir() {
			// Build outputs are not sources. `target` is cargo's, and a stray one inside the tree
			// would otherwise contribute hundreds of vendored manifests to the graph.
			if name == "target" || name == ".git" || name.starts_with('.') {
				continue;
			}
			collect_manifests(&path, out)?;
		} else if name == "Cargo.toml" {
			out.push(path);
		}
	}
	Ok(())
}

fn parse(repo_root: &Path, manifest_path: &Path) -> Result<Crate, String> {
	let text = fs::read_to_string(manifest_path).map_err(|error| format!("{}: {error}", manifest_path.display()))?;
	let value: toml::Value = toml::from_str(&text).map_err(|error| format!("{}: {error}", manifest_path.display()))?;
	let name = value.get("package").and_then(|package| package.get("name")).and_then(toml::Value::as_str).ok_or_else(|| format!("{}: no [package] name", manifest_path.display()))?.to_string();
	let dir_path = manifest_path.parent().unwrap_or(repo_root);
	let dir = relative(repo_root, dir_path);

	let mut dependencies = Vec::new();
	collect_dependencies(&value, &mut dependencies);
	// Target-specific dependency tables count too: `[target.'cfg(...)'.dependencies]` is a real
	// edge that only some targets take, and the same argument as `optional` applies - a target we
	// are not currently building is still a target that ships.
	if let Some(targets) = value.get("target").and_then(toml::Value::as_table) {
		for (_, table) in targets {
			collect_dependencies(table, &mut dependencies);
		}
	}
	dependencies.sort_by(|left, right| (left.kind, &left.name).cmp(&(right.kind, &right.name)));
	dependencies.dedup_by(|left, right| left.kind == right.kind && left.name == right.name);

	let feature_table = value.get("features").and_then(toml::Value::as_table);
	let features: BTreeSet<String> = feature_table.map(|table| table.keys().cloned().collect()).unwrap_or_default();
	let feature_definitions: Vec<(String, Vec<String>)> = feature_table.map(|table| table.iter().map(|(name, members)| (name.clone(), members.as_array().map(|values| values.iter().filter_map(toml::Value::as_str).map(str::to_string).collect()).unwrap_or_default())).collect()).unwrap_or_default();

	let mut binaries = Vec::new();
	if let Some(entries) = value.get("bin").and_then(toml::Value::as_array) {
		for entry in entries {
			let Some(bin_name) = entry.get("name").and_then(toml::Value::as_str) else { continue };
			let Some(bin_path) = entry.get("path").and_then(toml::Value::as_str) else { continue };
			// The path is relative to the manifest, and `..` is used in this tree (the shared
			// userspace build script), so it has to be normalised rather than concatenated.
			// `required-features = ["development"]`: the bin exists only in some configurations, which
			// is how `dev_agent` is built for the development profile and not for the shipping one.
			let required_features = entry.get("required-features").and_then(toml::Value::as_array).map(|values| values.iter().filter_map(toml::Value::as_str).map(str::to_string).collect()).unwrap_or_default();
			binaries.push(Binary { name: bin_name.to_string(), path: normalise(&format!("{dir}/{bin_path}")), required_features });
		}
	}
	binaries.sort_by(|left, right| left.name.cmp(&right.name));

	let build_script = value.get("package").and_then(|package| package.get("build")).and_then(toml::Value::as_str).map(|script| normalise(&format!("{dir}/{script}")));

	Ok(Crate { name, dir, dependencies, binaries, features, feature_definitions, build_script, has_host_tests: scan_for_host_tests(dir_path)? })
}

// Collapse `a/b/../c` to `a/c`. Cargo paths are relative to the manifest and this tree uses `..`.
fn normalise(path: &str) -> String {
	let mut parts: Vec<&str> = Vec::new();
	for part in path.split('/') {
		match part {
			"." | "" => {}
			".." => {
				parts.pop();
			}
			other => parts.push(other),
		}
	}
	parts.join("/")
}

fn collect_dependencies(table: &toml::Value, out: &mut Vec<Dependency>) {
	for (section, kind) in [("dependencies", DependencyKind::Normal), ("dev-dependencies", DependencyKind::Dev), ("build-dependencies", DependencyKind::Build)] {
		let Some(entries) = table.get(section).and_then(toml::Value::as_table) else { continue };
		for (name, spec) in entries {
			// Only path dependencies are components of this system. A crates.io dependency is a
			// third-party artifact with its own release cadence; nothing in this tree changes it,
			// so nothing in this tree needs an edge to it.
			let Some(spec) = spec.as_table() else { continue };
			if !spec.contains_key("path") {
				continue;
			}
			let optional = spec.get("optional").and_then(toml::Value::as_bool).unwrap_or(false);
			// The dependency's PACKAGE name, which `package = "..."` can rename at the use site.
			let package = spec.get("package").and_then(toml::Value::as_str).unwrap_or(name.as_str());
			out.push(Dependency { name: package.to_string(), kind, optional });
		}
	}
}

// A crate has host tests if it contains a `#[test]`.
//
// Matched exactly, because `#[test_case]` starts the same way and means the opposite: those are the
// kernel's in-guest suite, which no `cargo test` on the host can run.
fn scan_for_host_tests(dir: &Path) -> Result<bool, String> {
	let entries = match fs::read_dir(dir) {
		Ok(entries) => entries,
		Err(error) => return Err(format!("{}: {error}", dir.display())),
	};
	for entry in entries {
		let entry = entry.map_err(|error| format!("{}: {error}", dir.display()))?;
		let path = entry.path();
		let name = entry.file_name().to_string_lossy().into_owned();
		if path.is_dir() {
			if name == "target" || name.starts_with('.') {
				continue;
			}
			// A nested crate's tests belong to that crate, not to this one.
			if path.join("Cargo.toml").is_file() {
				continue;
			}
			if scan_for_host_tests(&path)? {
				return Ok(true);
			}
		} else if name.ends_with(".rs") {
			let text = fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
			if text.contains("#[test]") {
				return Ok(true);
			}
		}
	}
	Ok(false)
}

pub fn relative(repo_root: &Path, path: &Path) -> String {
	path.strip_prefix(repo_root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}
