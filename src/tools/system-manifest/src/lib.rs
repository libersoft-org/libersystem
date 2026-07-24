use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_PROVIDER_DEPTH: usize = 16;
pub const MAX_PROVIDER_MODULES: usize = 64;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
	schema: u32,
	#[serde(default)]
	sources: Vec<RawSource>,
	#[serde(default)]
	programs: Vec<RawProgram>,
	#[serde(default)]
	services: Vec<RawService>,
	#[serde(default)]
	libraries: Vec<RawLibrary>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
	owner: String,
	path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawProgram {
	name: String,
	owner: String,
	role: ProgramRole,
	linkage: Linkage,
	stage: Stage,
	destination: String,
	#[serde(default)]
	providers: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawService {
	name: String,
	program: String,
	restart: Restart,
	#[serde(default)]
	dependencies: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawLibrary {
	name: String,
	owner: String,
	destination: String,
	#[serde(default)]
	features: Vec<String>,
	#[serde(default)]
	providers: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProgramRole {
	Launcher,
	Service,
	Probe,
	Driver,
	Tool,
	Helper,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Linkage {
	Static,
	Dynamic,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
	Pinned,
	Volume,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Restart {
	Transparent,
	Escalate,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct Name(String);

impl Name {
	pub fn as_str(&self) -> &str {
		&self.0
	}
}

impl fmt::Display for Name {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str(&self.0)
	}
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct RelativePath(String);

impl RelativePath {
	pub fn as_str(&self) -> &str {
		&self.0
	}
}

#[derive(Clone, Debug, Serialize)]
pub struct Source {
	pub owner: Name,
	pub path: RelativePath,
}

#[derive(Clone, Debug, Serialize)]
pub struct Program {
	pub name: Name,
	pub owner: Name,
	pub role: ProgramRole,
	pub linkage: Linkage,
	pub stage: Stage,
	pub destination: RelativePath,
	pub providers: Vec<Name>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Service {
	pub name: Name,
	pub program: Name,
	pub restart: Restart,
	pub dependencies: Vec<Name>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Library {
	pub name: Name,
	pub owner: Name,
	pub destination: RelativePath,
	pub features: Vec<Name>,
	pub providers: Vec<Name>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Manifest {
	pub schema: u32,
	pub sources: BTreeMap<Name, Source>,
	pub programs: BTreeMap<Name, Program>,
	pub services: BTreeMap<Name, Service>,
	pub libraries: BTreeMap<Name, Library>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ValidationError {
	pub location: String,
	pub message: String,
}

impl fmt::Display for ValidationError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(formatter, "manifest: {}: {}", self.location, self.message)
	}
}

#[derive(Debug)]
pub enum LoadError {
	Io { path: PathBuf, error: std::io::Error },
	Toml(toml::de::Error),
	Validation(Vec<ValidationError>),
}

impl fmt::Display for LoadError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Io { path, error } => write!(formatter, "cannot read {}: {error}", path.display()),
			Self::Toml(error) => write!(formatter, "manifest TOML: {error}"),
			Self::Validation(errors) => {
				for (index, error) in errors.iter().enumerate() {
					if index != 0 {
						formatter.write_str("\n")?;
					}
					write!(formatter, "{error}")?;
				}
				Ok(())
			}
		}
	}
}

impl std::error::Error for LoadError {}

impl Manifest {
	pub fn load_workspace(workspace_root: &Path) -> Result<Self, LoadError> {
		Self::load(&workspace_root.join("user/services/manifest.toml"), workspace_root)
	}

	pub fn load(path: &Path, workspace_root: &Path) -> Result<Self, LoadError> {
		let text = fs::read_to_string(path).map_err(|error| LoadError::Io { path: path.to_path_buf(), error })?;
		Self::parse(&text, workspace_root).map_err(|error| match error {
			LoadError::Toml(mut source) => {
				source.set_input(Some(&text));
				LoadError::Toml(source)
			}
			other => other,
		})
	}

	pub fn parse(text: &str, workspace_root: &Path) -> Result<Self, LoadError> {
		let raw: RawManifest = toml::from_str(text).map_err(LoadError::Toml)?;
		let mut errors = Vec::new();
		if raw.schema != SCHEMA_VERSION {
			push_error(&mut errors, "schema", format!("unsupported version {}, expected {SCHEMA_VERSION}", raw.schema));
		}

		let mut source_paths = BTreeSet::new();
		let mut sources = BTreeMap::new();
		for raw_source in raw.sources {
			let location = format!("sources.{}", raw_source.owner);
			let Some(owner) = validate_name(&raw_source.owner, &format!("{location}.owner"), &mut errors) else { continue };
			let Some(path) = validate_relative_path(&raw_source.path, &format!("{location}.path"), &mut errors) else { continue };
			if !workspace_root.join(path.as_str()).join("Cargo.toml").is_file() {
				push_error(&mut errors, format!("{location}.path"), format!("no Cargo.toml at {}", path.as_str()));
			}
			if !source_paths.insert(path.clone()) {
				push_error(&mut errors, format!("{location}.path"), format!("duplicate source path {}", path.as_str()));
			}
			if sources.insert(owner.clone(), Source { owner, path }).is_some() {
				push_error(&mut errors, format!("{location}.owner"), "duplicate source owner");
			}
		}

		let mut destinations = BTreeSet::new();
		let mut libraries = BTreeMap::new();
		for raw_library in raw.libraries {
			let location = format!("libraries.{}", raw_library.name);
			let Some(name) = validate_name(&raw_library.name, &format!("{location}.name"), &mut errors) else { continue };
			let Some(owner) = validate_name(&raw_library.owner, &format!("{location}.owner"), &mut errors) else { continue };
			let Some(destination) = validate_relative_path(&raw_library.destination, &format!("{location}.destination"), &mut errors) else { continue };
			let features = validate_name_list(raw_library.features, &format!("{location}.features"), &mut errors);
			let providers = validate_name_list(raw_library.providers, &format!("{location}.providers"), &mut errors);
			if !sources.contains_key(&owner) {
				push_error(&mut errors, format!("{location}.owner"), format!("unknown source owner {owner}"));
			}
			let expected = sources.get(&owner).and_then(|source| library_category(name.as_str(), owner.as_str(), source.path.as_str())).map(|category| format!("lib/{category}/{name}.lslib"));
			match expected {
				Some(expected) if destination.as_str() != expected => push_error(&mut errors, format!("{location}.destination"), format!("expected {expected}")),
				None if sources.contains_key(&owner) => push_error(&mut errors, format!("{location}.destination"), "source has no library ownership category"),
				_ => {}
			}
			if !destinations.insert(destination.clone()) {
				push_error(&mut errors, format!("{location}.destination"), "duplicate staged destination");
			}
			if libraries.insert(name.clone(), Library { name, owner, destination, features, providers }).is_some() {
				push_error(&mut errors, format!("{location}.name"), "duplicate library name");
			}
		}

		let mut programs = BTreeMap::new();
		for raw_program in raw.programs {
			let location = format!("programs.{}", raw_program.name);
			let Some(name) = validate_name(&raw_program.name, &format!("{location}.name"), &mut errors) else { continue };
			let Some(owner) = validate_name(&raw_program.owner, &format!("{location}.owner"), &mut errors) else { continue };
			let destination = validate_relative_path(&raw_program.destination, &format!("{location}.destination"), &mut errors).unwrap_or_else(|| RelativePath(raw_program.destination.clone()));
			validate_program_shape(&raw_program, &name, &destination, &location, &mut errors);
			let providers = validate_name_list(raw_program.providers, &format!("{location}.providers"), &mut errors);
			if !sources.contains_key(&owner) {
				push_error(&mut errors, format!("{location}.owner"), format!("unknown source owner {owner}"));
			}
			if !destinations.insert(destination.clone()) {
				push_error(&mut errors, format!("{location}.destination"), "duplicate staged destination");
			}
			if programs.insert(name.clone(), Program { name, owner, role: raw_program.role, linkage: raw_program.linkage, stage: raw_program.stage, destination, providers }).is_some() {
				push_error(&mut errors, format!("{location}.name"), "duplicate program name");
			}
		}

		let mut services = BTreeMap::new();
		for raw_service in raw.services {
			let location = format!("services.{}", raw_service.name);
			let Some(name) = validate_name(&raw_service.name, &format!("{location}.name"), &mut errors) else { continue };
			let Some(program) = validate_name(&raw_service.program, &format!("{location}.program"), &mut errors) else { continue };
			let dependencies = validate_name_list(raw_service.dependencies, &format!("{location}.dependencies"), &mut errors);
			if services.insert(name.clone(), Service { name, program, restart: raw_service.restart, dependencies }).is_some() {
				push_error(&mut errors, format!("{location}.name"), "duplicate service name");
			}
		}

		validate_references(&libraries, &programs, &services, &mut errors);
		validate_graph("libraries", &libraries, |library| &library.providers, MAX_PROVIDER_DEPTH, MAX_PROVIDER_MODULES, &mut errors);
		validate_graph("services", &services, |service| &service.dependencies, usize::MAX, usize::MAX, &mut errors);
		validate_program_closures(&programs, &libraries, &mut errors);
		validate_user_source_coverage(workspace_root, &sources, &mut errors);
		validate_executable_aliases(&programs, &mut errors);

		errors.sort();
		errors.dedup();
		if !errors.is_empty() {
			return Err(LoadError::Validation(errors));
		}
		Ok(Self { schema: raw.schema, sources, programs, services, libraries })
	}

	pub fn source_path(&self, owner: &str) -> Option<&str> {
		self.sources.iter().find(|(name, _)| name.as_str() == owner).map(|(_, source)| source.path.as_str())
	}

	pub fn canonical_json(&self) -> Result<String, serde_json::Error> {
		serde_json::to_string_pretty(self).map(|mut json| {
			json.push('\n');
			json
		})
	}
}

fn validate_name(value: &str, location: &str, errors: &mut Vec<ValidationError>) -> Option<Name> {
	let valid = !value.is_empty() && value.len() <= 64 && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
	if !valid {
		push_error(errors, location, format!("invalid logical name {value:?}"));
		return None;
	}
	Some(Name(value.to_string()))
}

fn validate_relative_path(value: &str, location: &str, errors: &mut Vec<ValidationError>) -> Option<RelativePath> {
	let path = Path::new(value);
	let valid = !value.is_empty() && !path.is_absolute() && path.components().all(|component| matches!(component, Component::Normal(_))) && !value.contains('\\');
	if !valid {
		push_error(errors, location, format!("invalid normalized relative path {value:?}"));
		return None;
	}
	Some(RelativePath(value.to_string()))
}

fn validate_name_list(values: Vec<String>, location: &str, errors: &mut Vec<ValidationError>) -> Vec<Name> {
	let mut names = Vec::new();
	let mut seen = BTreeSet::new();
	for value in values {
		let Some(name) = validate_name(&value, location, errors) else { continue };
		if !seen.insert(name.clone()) {
			push_error(errors, location, format!("duplicate {name}"));
		}
		names.push(name);
	}
	names.sort();
	names
}

fn library_category<'a>(name: &str, owner: &str, source: &'a str) -> Option<&'a str> {
	if let Some(relative) = source.strip_prefix("user/libs/") {
		let (category, leaf) = relative.split_once('/')?;
		return (leaf == owner && !category.is_empty() && !category.contains('/')).then_some(category);
	}
	match (name, owner, source) {
		("lsrt", "rt", "user/runtime/rt") => Some("runtime"),
		("wire", "wire", "wire") => Some("ipc"),
		("wasm", "wasm", "wasm") => Some("component"),
		("term", "term", "term") => Some("terminal"),
		("service-util", "services", "user/services/core") => Some("service"),
		_ => None,
	}
}

fn validate_program_shape(raw: &RawProgram, name: &Name, destination: &RelativePath, location: &str, errors: &mut Vec<ValidationError>) {
	if raw.linkage == Linkage::Dynamic && raw.stage != Stage::Volume {
		push_error(errors, format!("{location}.stage"), "dynamic programs must be volume staged");
	}
	if raw.role == ProgramRole::Launcher && (raw.linkage != Linkage::Static || raw.stage != Stage::Pinned) {
		push_error(errors, format!("{location}.role"), "launchers must be static and pinned");
	}
	if matches!(raw.role, ProgramRole::Tool | ProgramRole::Helper) && raw.stage != Stage::Volume {
		push_error(errors, format!("{location}.stage"), "tools and helpers must be volume staged");
	}
	let expected = match (raw.stage, raw.role) {
		(Stage::Pinned, _) => format!("{name}.lsexe"),
		(Stage::Volume, ProgramRole::Driver) => format!("drivers/{name}.lsexe"),
		(Stage::Volume, _) => format!("bin/{name}.lsexe"),
	};
	if destination.as_str() != expected {
		push_error(errors, format!("{location}.destination"), format!("expected {expected}"));
	}
}

fn validate_references(libraries: &BTreeMap<Name, Library>, programs: &BTreeMap<Name, Program>, services: &BTreeMap<Name, Service>, errors: &mut Vec<ValidationError>) {
	for (name, library) in libraries {
		for provider in &library.providers {
			if provider == name {
				push_error(errors, format!("libraries.{name}.providers"), "self provider edge");
			} else if !libraries.contains_key(provider) {
				push_error(errors, format!("libraries.{name}.providers"), format!("unknown library {provider}"));
			}
		}
	}
	for (name, program) in programs {
		for provider in &program.providers {
			if !libraries.contains_key(provider) {
				push_error(errors, format!("programs.{name}.providers"), format!("unknown library {provider}"));
			}
		}
	}
	for (name, service) in services {
		if !programs.contains_key(&service.program) {
			push_error(errors, format!("services.{name}.program"), format!("unknown program {}", service.program));
		}
		for dependency in &service.dependencies {
			if dependency == name {
				push_error(errors, format!("services.{name}.dependencies"), "self dependency edge");
			} else if !services.contains_key(dependency) {
				push_error(errors, format!("services.{name}.dependencies"), format!("unknown service {dependency}"));
			}
		}
	}
}

fn validate_graph<T, F>(namespace: &str, nodes: &BTreeMap<Name, T>, edges: F, max_depth: usize, max_modules: usize, errors: &mut Vec<ValidationError>)
where
	F: Fn(&T) -> &[Name],
{
	#[allow(clippy::too_many_arguments)]
	fn visit<T, F>(name: &Name, nodes: &BTreeMap<Name, T>, edges: &F, visiting: &mut Vec<Name>, visited: &mut BTreeSet<Name>, max_depth: usize, errors: &mut Vec<ValidationError>, namespace: &str)
	where
		F: Fn(&T) -> &[Name],
	{
		if visited.contains(name) || !nodes.contains_key(name) {
			return;
		}
		if let Some(index) = visiting.iter().position(|current| current == name) {
			let cycle = visiting[index..].iter().chain(std::iter::once(name)).map(Name::as_str).collect::<Vec<_>>().join(" -> ");
			push_error(errors, format!("{namespace}.{name}"), format!("dependency cycle: {cycle}"));
			return;
		}
		if visiting.len() >= max_depth {
			push_error(errors, format!("{namespace}.{name}"), format!("dependency depth exceeds {max_depth}"));
			return;
		}
		visiting.push(name.clone());
		for dependency in edges(&nodes[name]) {
			visit(dependency, nodes, edges, visiting, visited, max_depth, errors, namespace);
		}
		visiting.pop();
		visited.insert(name.clone());
	}

	if nodes.len() > max_modules {
		push_error(errors, namespace, format!("module count {} exceeds {max_modules}", nodes.len()));
	}
	let mut visited = BTreeSet::new();
	for name in nodes.keys() {
		visit(name, nodes, &edges, &mut Vec::new(), &mut visited, max_depth, errors, namespace);
	}
}

fn validate_program_closures(programs: &BTreeMap<Name, Program>, libraries: &BTreeMap<Name, Library>, errors: &mut Vec<ValidationError>) {
	fn collect(name: &Name, libraries: &BTreeMap<Name, Library>, modules: &mut BTreeSet<Name>) {
		if !modules.insert(name.clone()) {
			return;
		}
		if let Some(library) = libraries.get(name) {
			for provider in &library.providers {
				collect(provider, libraries, modules);
			}
		}
	}
	for (name, program) in programs {
		let mut modules = BTreeSet::new();
		for provider in &program.providers {
			collect(provider, libraries, &mut modules);
		}
		if modules.len() > MAX_PROVIDER_MODULES {
			push_error(errors, format!("programs.{name}.providers"), format!("provider closure {} exceeds {MAX_PROVIDER_MODULES}", modules.len()));
		}
	}
}

fn validate_user_source_coverage(workspace_root: &Path, sources: &BTreeMap<Name, Source>, errors: &mut Vec<ValidationError>) {
	fn collect(directory: &Path, workspace_root: &Path, output: &mut BTreeSet<String>) {
		let Ok(entries) = fs::read_dir(directory) else { return };
		for entry in entries.flatten() {
			let path = entry.path();
			if path.is_dir() {
				if path.join("Cargo.toml").is_file()
					&& let Ok(relative) = path.strip_prefix(workspace_root)
				{
					output.insert(relative.to_string_lossy().replace('\\', "/"));
				}
				collect(&path, workspace_root, output);
			}
		}
	}
	let mut physical = BTreeSet::new();
	collect(&workspace_root.join("user"), workspace_root, &mut physical);
	let declared = sources.values().filter(|source| source.path.as_str().starts_with("user/")).map(|source| source.path.as_str().to_string()).collect::<BTreeSet<_>>();
	for missing in physical.difference(&declared) {
		push_error(errors, "sources", format!("physical userspace crate {missing} has no source owner"));
	}
	for missing in declared.difference(&physical) {
		push_error(errors, "sources", format!("declared userspace crate {missing} is not physical"));
	}
}

fn validate_executable_aliases(programs: &BTreeMap<Name, Program>, errors: &mut Vec<ValidationError>) {
	for (first_index, first) in programs.keys().enumerate() {
		for second in programs.keys().skip(first_index + 1) {
			let ambiguous = second.as_str() == format!("{}.lsexe", first.as_str()) || first.as_str() == format!("{}.lsexe", second.as_str());
			if ambiguous {
				push_error(errors, format!("programs.{first}.name"), format!("ambiguous executable alias {second}"));
			}
		}
	}
}

fn push_error(errors: &mut Vec<ValidationError>, location: impl Into<String>, message: impl Into<String>) {
	errors.push(ValidationError { location: location.into(), message: message.into() });
}

#[cfg(test)]
mod tests;
