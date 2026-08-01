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
	factory_files: Vec<RawFactoryFile>,
	#[serde(default)]
	runtime_paths: Vec<RawRuntimePath>,
	#[serde(default)]
	services: Vec<RawService>,
	#[serde(default)]
	libraries: Vec<RawLibrary>,
	#[serde(default)]
	boot_artifacts: Vec<RawBootArtifact>,
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
	// A development-only program: built and staged only when the development feature is on,
	// and absent from a shipped image rather than present and refusing to work. Declaring it
	// here keeps the manifest the single place that says what the system is made of, in both
	// configurations.
	#[serde(default)]
	development: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawFactoryFile {
	name: String,
	kind: FactoryFileKind,
	#[serde(default)]
	source: Option<String>,
	destination: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawRuntimePath {
	name: String,
	owner: String,
	destination: String,
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

// The pieces of the boot chain: the kernel and the UEFI loader. They are not userspace
// programs - nothing stages them onto a volume and no service supervises them - but the
// manifest is the final assembly of the whole system, so what goes into an ISO or IMG has to
// be named here rather than hard-coded in the image builder.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawBootArtifact {
	name: String,
	owner: String,
	kind: BootArtifactKind,
	destination: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BootArtifactKind {
	// The kernel ELF the loader hands control to.
	Kernel,
	// The UEFI application the firmware starts, staged as BOOTX64.EFI (or its per-target name).
	Loader,
	// The init package: the pinned userspace set, handed to the kernel as a boot module rather
	// than linked into it. Named here because packaging must fail when it is missing, and
	// because the kernel binary genuinely does not contain it.
	InitPackage,
	// The volume package: everything staged onto the system volume rather than pinned into the
	// boot module - the programs this manifest marks `stage = "volume"`.
	VolumePackage,
}

#[derive(Clone, Debug, Serialize)]
pub struct BootArtifact {
	pub name: Name,
	pub owner: Name,
	pub kind: BootArtifactKind,
	pub destination: RelativePath,
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
pub enum FactoryFileKind {
	Source,
	SdkComponent,
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
	// Built and staged only in the development configuration; see RawProgram.
	pub development: bool,
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
pub struct FactoryFile {
	pub name: Name,
	pub kind: FactoryFileKind,
	pub source: Option<RelativePath>,
	pub destination: RelativePath,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimePath {
	pub name: Name,
	pub owner: Name,
	pub destination: RelativePath,
}

#[derive(Clone, Debug, Serialize)]
pub struct Manifest {
	pub schema: u32,
	pub sources: BTreeMap<Name, Source>,
	pub programs: BTreeMap<Name, Program>,
	pub factory_files: BTreeMap<Name, FactoryFile>,
	pub runtime_paths: BTreeMap<Name, RuntimePath>,
	pub services: BTreeMap<Name, Service>,
	pub libraries: BTreeMap<Name, Library>,
	pub boot_artifacts: BTreeMap<Name, BootArtifact>,
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
			if programs.insert(name.clone(), Program { name, owner, role: raw_program.role, linkage: raw_program.linkage, stage: raw_program.stage, destination, providers, development: raw_program.development }).is_some() {
				push_error(&mut errors, format!("{location}.name"), "duplicate program name");
			}
		}

		let mut factory_files = BTreeMap::new();
		let mut factory_sources = BTreeSet::new();
		for raw_factory_file in raw.factory_files {
			let location = format!("factory_files.{}", raw_factory_file.name);
			let Some(name) = validate_name(&raw_factory_file.name, &format!("{location}.name"), &mut errors) else { continue };
			let Some(destination) = validate_relative_path(&raw_factory_file.destination, &format!("{location}.destination"), &mut errors) else { continue };
			let source = match raw_factory_file.kind {
				FactoryFileKind::Source => {
					let Some(raw_source) = raw_factory_file.source else {
						push_error(&mut errors, format!("{location}.source"), "source factory files require a source path");
						continue;
					};
					let Some(source) = validate_relative_path(&raw_source, &format!("{location}.source"), &mut errors) else { continue };
					if !source.as_str().starts_with("volume/") {
						push_error(&mut errors, format!("{location}.source"), "source factory files must live below volume/");
					}
					if !workspace_root.join(source.as_str()).is_file() {
						push_error(&mut errors, format!("{location}.source"), format!("no factory file at {}", source.as_str()));
					}
					if !factory_sources.insert(source.clone()) {
						push_error(&mut errors, format!("{location}.source"), format!("duplicate factory source {}", source.as_str()));
					}
					Some(source)
				}
				FactoryFileKind::SdkComponent => {
					if raw_factory_file.source.is_some() {
						push_error(&mut errors, format!("{location}.source"), "SDK component payloads do not accept a source path");
					}
					None
				}
			};
			validate_factory_file_shape(raw_factory_file.kind, source.as_ref(), &destination, &location, &mut errors);
			if !destinations.insert(destination.clone()) {
				push_error(&mut errors, format!("{location}.destination"), "duplicate staged destination");
			}
			if factory_files.insert(name.clone(), FactoryFile { name, kind: raw_factory_file.kind, source, destination }).is_some() {
				push_error(&mut errors, format!("{location}.name"), "duplicate factory file name");
			}
		}

		let mut runtime_paths = BTreeMap::new();
		for raw_runtime_path in raw.runtime_paths {
			let location = format!("runtime_paths.{}", raw_runtime_path.name);
			let Some(name) = validate_name(&raw_runtime_path.name, &format!("{location}.name"), &mut errors) else { continue };
			let Some(owner) = validate_name(&raw_runtime_path.owner, &format!("{location}.owner"), &mut errors) else { continue };
			let Some(destination) = validate_relative_path(&raw_runtime_path.destination, &format!("{location}.destination"), &mut errors) else { continue };
			if !programs.contains_key(&owner) {
				push_error(&mut errors, format!("{location}.owner"), format!("unknown program owner {owner}"));
			}
			validate_runtime_path_shape(&name, &owner, &destination, &location, &mut errors);
			if !destinations.insert(destination.clone()) {
				push_error(&mut errors, format!("{location}.destination"), "duplicate staged or runtime destination");
			}
			if runtime_paths.insert(name.clone(), RuntimePath { name, owner, destination }).is_some() {
				push_error(&mut errors, format!("{location}.name"), "duplicate runtime path name");
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

		// The boot chain. Each kind may appear exactly once: two kernels or two loaders in one
		// image is not a configuration, it is a mistake, and the image builder would silently
		// pick whichever it saw last.
		let mut boot_artifacts = BTreeMap::new();
		let mut boot_kinds: BTreeMap<String, Name> = BTreeMap::new();
		for raw_artifact in raw.boot_artifacts {
			let location = format!("boot_artifacts.{}", raw_artifact.name);
			let Some(name) = validate_name(&raw_artifact.name, &format!("{location}.name"), &mut errors) else { continue };
			let Some(owner) = validate_name(&raw_artifact.owner, &format!("{location}.owner"), &mut errors) else { continue };
			let Some(destination) = validate_relative_path(&raw_artifact.destination, &format!("{location}.destination"), &mut errors) else { continue };
			if !sources.contains_key(&owner) {
				push_error(&mut errors, format!("{location}.owner"), format!("unknown source owner {owner}"));
			}
			let kind_label = format!("{:?}", raw_artifact.kind);
			if let Some(previous) = boot_kinds.insert(kind_label.clone(), name.clone()) {
				push_error(&mut errors, format!("{location}.kind"), format!("{kind_label} is already provided by {previous}"));
			}
			if !destinations.insert(destination.clone()) {
				push_error(&mut errors, format!("{location}.destination"), "duplicate staged destination");
			}
			if boot_artifacts.insert(name.clone(), BootArtifact { name, owner, kind: raw_artifact.kind, destination }).is_some() {
				push_error(&mut errors, format!("{location}.name"), "duplicate boot artifact name");
			}
		}
		// An image without a kernel or without a loader does not boot, so their absence is an
		// error here rather than a discovery at packaging time.
		for required in ["Kernel", "Loader", "InitPackage", "VolumePackage"] {
			if !boot_kinds.contains_key(required) {
				push_error(&mut errors, "boot_artifacts", format!("no artifact provides {required}"));
			}
		}
		validate_references(&libraries, &programs, &services, &mut errors);
		validate_graph("libraries", &libraries, |library| &library.providers, MAX_PROVIDER_DEPTH, MAX_PROVIDER_MODULES, &mut errors);
		validate_graph("services", &services, |service| &service.dependencies, usize::MAX, usize::MAX, &mut errors);
		validate_program_closures(&programs, &libraries, &mut errors);
		validate_user_source_coverage(workspace_root, &sources, &mut errors);
		validate_factory_source_coverage(workspace_root, &factory_files, &mut errors);
		if !factory_files.values().any(|file| file.kind == FactoryFileKind::SdkComponent) {
			push_error(&mut errors, "factory_files", "no SDK component payload is declared");
		}
		validate_executable_aliases(&programs, &mut errors);

		errors.sort();
		errors.dedup();
		if !errors.is_empty() {
			return Err(LoadError::Validation(errors));
		}
		Ok(Self { schema: raw.schema, sources, programs, factory_files, runtime_paths, services, libraries, boot_artifacts })
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

	// Every destination the system volume is expected to carry. `development` selects the
	// configuration: false is the shipping volume, which omits the development-only programs
	// entirely, and the two answers must not be conflated - a build that stages one set and
	// checks against the other is exactly the mistake this returns a parameter to prevent.
	pub fn volume_destinations(&self, development: bool) -> BTreeSet<String> {
		self.libraries.values().map(|library| library.destination.as_str().to_string()).chain(self.programs.values().filter(|program| program.stage == Stage::Volume && (development || !program.development)).map(|program| program.destination.as_str().to_string())).chain(self.factory_files.values().map(|file| file.destination.as_str().to_string())).collect()
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
	let expected_name = format!("{name}.lsexe");
	if raw.stage == Stage::Pinned {
		if destination.as_str() != expected_name {
			push_error(errors, format!("{location}.destination"), format!("expected {expected_name}"));
		}
		return;
	}
	let expected = match raw.role {
		ProgramRole::Tool => format!("bin/{expected_name}"),
		ProgramRole::Driver => format!("drivers/{expected_name}"),
		ProgramRole::Service | ProgramRole::Probe | ProgramRole::Helper if name.as_str() == "config_service" => format!("libexec/config_service/{expected_name}"),
		ProgramRole::Service | ProgramRole::Probe | ProgramRole::Helper => format!("libexec/{expected_name}"),
		ProgramRole::Launcher => unreachable!("launchers are pinned"),
	};
	if destination.as_str() != expected {
		push_error(errors, format!("{location}.destination"), format!("expected {expected}"));
	}
}

fn validate_factory_file_shape(kind: FactoryFileKind, source: Option<&RelativePath>, destination: &RelativePath, location: &str, errors: &mut Vec<ValidationError>) {
	match kind {
		FactoryFileKind::Source => {
			let Some(source) = source else { return };
			let Some(source_destination) = source.as_str().strip_prefix("volume/") else { return };
			if source_destination != destination.as_str() {
				push_error(errors, format!("{location}.destination"), format!("expected {source_destination}"));
			}
			let valid = matches!(destination.as_str(), "hello.txt" | "motd.txt" | "audio/test.mp3") || destination.as_str().strip_prefix("wallpapers/").is_some_and(|name| !name.is_empty() && !name.contains('/') && name.ends_with(".webp"));
			if !valid {
				push_error(errors, format!("{location}.destination"), "factory source files must be hello.txt, motd.txt, audio/test.mp3, or a wallpapers/*.webp file");
			}
		}
		FactoryFileKind::SdkComponent => {
			if destination.as_str() != "components/liber_component/app.wasm" {
				push_error(errors, format!("{location}.destination"), "SDK component payloads must stage at components/liber_component/app.wasm");
			}
		}
	}
}

fn validate_runtime_path_shape(name: &Name, owner: &Name, destination: &RelativePath, location: &str, errors: &mut Vec<ValidationError>) {
	let expected = match (name.as_str(), owner.as_str()) {
		("command-directory", "shell") => Some("bin"),
		("config-tree", "config_service") => Some("libexec/config_service/config.tree"),
		("liber-component-output", "component_host") => Some("components/liber_component/out.txt"),
		("system-journal", "log_service") => Some("log"),
		_ => None,
	};
	match expected {
		Some(expected) if destination.as_str() == expected => {}
		Some(expected) => push_error(errors, format!("{location}.destination"), format!("expected {expected}")),
		None => push_error(errors, format!("{location}.name"), format!("unsupported runtime path {} for {}", name.as_str(), owner.as_str())),
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

fn validate_factory_source_coverage(workspace_root: &Path, factory_files: &BTreeMap<Name, FactoryFile>, errors: &mut Vec<ValidationError>) {
	fn collect(directory: &Path, workspace_root: &Path, output: &mut BTreeSet<String>) {
		let Ok(entries) = fs::read_dir(directory) else { return };
		for entry in entries.flatten() {
			let path = entry.path();
			if path.is_dir() {
				collect(&path, workspace_root, output);
			} else if path.is_file()
				&& let Ok(relative) = path.strip_prefix(workspace_root)
			{
				output.insert(relative.to_string_lossy().replace('\\', "/"));
			}
		}
	}
	let mut physical = BTreeSet::new();
	collect(&workspace_root.join("volume"), workspace_root, &mut physical);
	let declared = factory_files.values().filter_map(|file| file.source.as_ref().map(|source| source.as_str().to_string())).collect::<BTreeSet<_>>();
	for missing in physical.difference(&declared) {
		push_error(errors, "factory_files", format!("physical factory file {missing} is not declared"));
	}
	for missing in declared.difference(&physical) {
		push_error(errors, "factory_files", format!("declared factory file {missing} is not physical"));
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
