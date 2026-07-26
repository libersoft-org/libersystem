// build.rs - selects the linker script by target arch and exposes the product
// metadata from product.conf (the single source of truth) to the kernel as
// compile-time environment variables.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn timing_event(phase: &str, event: &str) {
	let Ok(path) = env::var("LIBER_TIMING_LOG") else { return };
	let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else { return };
	let timestamp_ns: u128 = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
	let _ = writeln!(file, "{timestamp_ns}\t{phase}\t{event}");
}

fn main() {
	println!("cargo:rerun-if-env-changed=TEST_TAGS");
	select_linker_script();
	let conf: Vec<(String, String)> = read_product_conf();
	export_product_metadata(&conf);
	timing_event("package_init", "start");
	assemble_init_package(&conf);
	timing_event("package_init", "end");
	timing_event("package_volume", "start");
	assemble_volume_package(&conf);
	timing_event("package_volume", "end");
	export_cross_arch_volume();
}

fn select_linker_script() {
	let arch: String = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
	let script: &str = match arch.as_str() {
		"x86_64" => "linker/x86_64.ld",
		"aarch64" => "linker/aarch64.ld",
		"riscv64" => "linker/riscv64.ld",
		other => panic!("unsupported architecture: {other}"),
	};
	println!("cargo:rustc-link-arg=-T{script}");
	println!("cargo:rerun-if-changed={script}");
	println!("cargo:rerun-if-changed=build.rs");
}

// Expose the assembled volume package at a stable path so the direct AArch64/RISC-V QEMU
// runners can lay the factory archive onto virtio-blk at LBA 0.
fn export_cross_arch_volume() {
	let arch: String = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
	if arch == "aarch64" || arch == "riscv64" {
		let out_dir: PathBuf = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
		let manifest_dir: String = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
		let build_dir: PathBuf = PathBuf::from(&manifest_dir).join("../../.build/boot");
		let _ = fs::create_dir_all(&build_dir);
		let vol_src: PathBuf = out_dir.join("volume.pkg");
		if vol_src.exists() {
			let bytes: Vec<u8> = fs::read(&vol_src).unwrap_or_else(|error| panic!("cannot read {}: {error}", vol_src.display()));
			write_if_changed(&build_dir.join(format!("volume-{arch}.pkg")), &bytes);
		}
	}
}

fn write_if_changed(path: &Path, bytes: &[u8]) {
	if fs::read(path).is_ok_and(|existing| existing == bytes) {
		return;
	}
	let file_name = path.file_name().and_then(|name| name.to_str()).expect("output file name");
	let temporary = path.with_file_name(format!("{file_name}.{}.tmp", std::process::id()));
	fs::write(&temporary, bytes).unwrap_or_else(|error| panic!("cannot write {}: {error}", temporary.display()));
	fs::rename(&temporary, path).unwrap_or_else(|error| panic!("cannot publish {}: {error}", path.display()));
}

// Parse ../../product.conf (shell-style KEY="value") into key/value pairs (the
// single source of truth for both the product metadata and the boot artifact
// filenames).
fn read_product_conf() -> Vec<(String, String)> {
	let manifest_dir: String = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
	let path: PathBuf = PathBuf::from(&manifest_dir).join("../../product.conf");
	let text: String = fs::read_to_string(&path).unwrap_or_else(|e: std::io::Error| panic!("cannot read {}: {e}", path.display()));
	println!("cargo:rerun-if-changed={}", path.display());
	let mut pairs: Vec<(String, String)> = Vec::new();
	for line in text.lines() {
		let trimmed: &str = line.trim();
		if trimmed.is_empty() || trimmed.starts_with('#') {
			continue;
		}
		let Some((key, value)) = trimmed.split_once('=') else {
			continue;
		};
		pairs.push((key.trim().to_string(), value.trim().trim_matches('"').to_string()));
	}
	pairs
}

// Re-export every product.conf entry as a rustc env var so the kernel can read it
// via env!("PRODUCT_NAME"), env!("INIT_PACKAGE"), etc.
fn export_product_metadata(conf: &[(String, String)]) {
	for (key, value) in conf {
		println!("cargo:rustc-env={key}={value}");
	}
}

// Look up a required key from the parsed product.conf.
fn conf_get<'a>(conf: &'a [(String, String)], key: &str) -> &'a str {
	for (k, v) in conf {
		if k.as_str() == key {
			return v.as_str();
		}
	}
	panic!("missing {key} in product.conf");
}

// The userspace programs staged at boot, read from the shared service manifest
// (../user/services/manifest.toml) - the single source of truth ServiceManager also
// generates its dependency table from, so the runtime service set and the staged
// programs cannot drift. Source rows map logical owners to workspace-relative Cargo
// roots; artifact rows retain `kind name crate stage [deps...]`. The kind and stage
// columns sort a row into the init package (the pinned bootstrap set on the path to
// mounting the system volume, plus the bootstrap block driver that backs it) or onto the
// system volume (every other service, driver, tool and demo component, loaded from there
// once it is mounted).

// A staged program parsed from a manifest row: its kind, package entry name, logical
// owner, explicit source path and staging class.
#[derive(Clone)]
struct ManifestRow {
	kind: String,
	name: String,
	crate_dir: String,
	crate_path: String,
	stage: String,
	destination: Option<String>,
	features: Option<String>,
	providers: Vec<String>,
}

// Read and parse the shared service manifest, keeping every row that names a staged
// program (an `instance` row is a managed service backed by another program's ELF, so
// it stages nothing of its own - its `crate` is `-` and its `stage` is `none`).
fn read_manifest(manifest: &Path) -> Vec<ManifestRow> {
	use system_manifest::{Linkage, ProgramRole, Stage};

	let workspace = manifest.join("..");
	let model = system_manifest::Manifest::load_workspace(&workspace).unwrap_or_else(|error| panic!("{error}"));
	let mut rows: Vec<ManifestRow> = Vec::new();
	for library in model.libraries.values() {
		let source = model.sources.get(&library.owner).expect("validated library owner");
		let features = if library.features.is_empty() { String::from("-") } else { library.features.iter().map(|feature| feature.as_str()).collect::<Vec<_>>().join(",") };
		rows.push(ManifestRow { kind: String::from("library"), name: library.name.as_str().to_string(), crate_dir: library.owner.as_str().to_string(), crate_path: source.path.as_str().to_string(), stage: String::from("volume"), destination: Some(library.destination.as_str().to_string()), features: Some(features), providers: library.providers.iter().map(|provider| provider.as_str().to_string()).collect() });
	}
	for program in model.programs.values() {
		let source = model.sources.get(&program.owner).expect("validated program owner");
		let kind = match (program.role, program.linkage) {
			(ProgramRole::Service, Linkage::Dynamic) => "dynamic-service",
			(ProgramRole::Service, Linkage::Static) => "service",
			(ProgramRole::Launcher, _) => "launcher",
			(ProgramRole::Driver, _) => "driver",
			(ProgramRole::Probe, Linkage::Static) => "probe",
			(ProgramRole::Probe | ProgramRole::Tool | ProgramRole::Helper, Linkage::Dynamic) => "dynamic",
			(ProgramRole::Tool | ProgramRole::Helper, Linkage::Static) => panic!("validated static tool/helper"),
		};
		rows.push(ManifestRow {
			kind: kind.to_string(),
			name: program.name.as_str().to_string(),
			crate_dir: program.owner.as_str().to_string(),
			crate_path: source.path.as_str().to_string(),
			stage: match program.stage {
				Stage::Pinned => "pinned",
				Stage::Volume => "volume",
			}
			.to_string(),
			destination: Some(program.destination.as_str().to_string()),
			features: None,
			providers: program.providers.iter().map(|provider| provider.as_str().to_string()).collect(),
		});
	}
	rows
}

fn generate_test_volume_paths(manifest: &system_manifest::Manifest) {
	let mut library_arms = String::new();
	for library in manifest.libraries.values() {
		library_arms.push_str(&format!("\t\"{}.lslib\" => Some(\"{}\"),\n", library.name, library.destination.as_str()));
	}
	let mut program_arms = String::new();
	for program in manifest.programs.values().filter(|program| program.stage == system_manifest::Stage::Volume) {
		program_arms.push_str(&format!("\t\"{}\" => Some(\"{}\"),\n", program.name, program.destination.as_str()));
	}
	let mut factory_arms = String::new();
	for factory_file in manifest.factory_files.values() {
		factory_arms.push_str(&format!("\t\"{}\" => Some(\"{}\"),\n", factory_file.name, factory_file.destination.as_str()));
	}
	let mut runtime_arms = String::new();
	for runtime_path in manifest.runtime_paths.values() {
		runtime_arms.push_str(&format!("\t\"{}\" => Some(\"{}\"),\n", runtime_path.name, runtime_path.destination.as_str()));
	}
	let mut declared_arms = String::new();
	for destination in manifest.libraries.values().map(|library| library.destination.as_str()).chain(manifest.programs.values().filter(|program| program.stage == system_manifest::Stage::Volume).map(|program| program.destination.as_str())).chain(manifest.factory_files.values().map(|file| file.destination.as_str())) {
		declared_arms.push_str(&format!("\t\"{destination}\" => true,\n"));
	}
	let source = format!("// @generated from user/services/manifest.toml by build.rs - do not edit.\nfn test_library_path(name: &str) -> Option<&'static str> {{\n\tmatch name {{\n{library_arms}\t\t_ => None,\n\t}}\n}}\n\nfn test_program_path(name: &str) -> Option<&'static str> {{\n\tmatch name {{\n{program_arms}\t\t_ => None,\n\t}}\n}}\n\nfn test_factory_path(name: &str) -> Option<&'static str> {{\n\tmatch name {{\n{factory_arms}\t\t_ => None,\n\t}}\n}}\n\nfn test_runtime_path(name: &str) -> Option<&'static str> {{\n\tmatch name {{\n{runtime_arms}\t\t_ => None,\n\t}}\n}}\n\nfn test_volume_path_is_declared(path: &str) -> bool {{\n\tmatch path {{\n{declared_arms}\t\t_ => false,\n\t}}\n}}\n");
	let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
	fs::write(out_dir.join("library_paths.rs"), source).expect("write test library paths");
}

fn valid_library_name(name: &str) -> bool {
	!name.is_empty() && !name.starts_with("lib") && name.len() <= 58 && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn audit_dynamic_relocations(row: &ManifestRow, image: &bootproto::elf::Elf<'_>, dynamic: &bootproto::elf::DynamicInfo) {
	let rela = image.rela_entries(dynamic).unwrap_or_else(|| panic!("{} {} has malformed RELA metadata", row.kind, row.name));
	let plt_rela = image.plt_rela_entries(dynamic).unwrap_or_else(|| panic!("{} {} has malformed PLT RELA metadata", row.kind, row.name));
	for relocation in rela.chain(plt_rela) {
		let kind = bootproto::elf::dynamic_relocation_kind(user_elf_machine(), relocation.relocation_type()).unwrap_or_else(|| panic!("{} {} uses dynamic relocation type {} outside the {} loader allowlist", row.kind, row.name, relocation.relocation_type(), user_target()));
		assert!(kind.accepts_symbol(relocation.symbol()), "{} {} uses a relative relocation with symbol {}", row.kind, row.name, relocation.symbol());
	}
}

fn audit_linked_artifact(row: &ManifestRow, bytes: &[u8], libraries: &[String], require_provider: bool) {
	const PT_INTERP: u32 = 3;
	const DT_RPATH: i64 = 15;
	const DT_TEXTREL: i64 = 22;
	const DT_RUNPATH: i64 = 29;

	let mut expected: Vec<String> = row
		.providers
		.iter()
		.map(|provider| {
			assert!(valid_library_name(provider), "dynamic {} names invalid provider {provider:?}", row.name);
			assert!(libraries.binary_search(provider).is_ok(), "dynamic {} names unstaged provider {provider}", row.name);
			format!("{provider}.lslib")
		})
		.collect();
	expected.sort();
	assert!(!expected.windows(2).any(|pair| pair[0] == pair[1]), "{} {} repeats a provider", row.kind, row.name);
	if require_provider {
		assert!(!expected.is_empty(), "{} {} has no providers", row.kind, row.name);
	}

	let image = bootproto::elf::Elf::parse_for_machine(bytes, user_elf_machine()).unwrap_or_else(|| panic!("{} {} is not a valid target ELF", row.kind, row.name));
	assert_eq!(image.image_type, bootproto::elf::ET_DYN, "{} {} is not ET_DYN", row.kind, row.name);
	let dynamic = image.dynamic_info().flatten().unwrap_or_else(|| panic!("{} {} has no valid terminated PT_DYNAMIC", row.kind, row.name));
	image.symbols(&dynamic).unwrap_or_else(|| panic!("{} {} has malformed dynamic symbols", row.kind, row.name)).for_each(drop);
	audit_dynamic_relocations(row, &image, &dynamic);
	for entry in image.dynamic_entries().flatten().unwrap_or_else(|| panic!("{} {} has no PT_DYNAMIC", row.kind, row.name)) {
		assert!(!matches!(entry.tag, DT_RPATH | DT_RUNPATH | DT_TEXTREL), "{} {} has forbidden dynamic tag {}", row.kind, row.name, entry.tag);
	}
	for index in 0..image.segment_count() {
		let segment = image.segment(index).unwrap_or_else(|| panic!("{} {} has a malformed program-header table", row.kind, row.name));
		assert_ne!(segment.p_type, PT_INTERP, "{} {} has PT_INTERP", row.kind, row.name);
		assert!(segment.p_flags & (bootproto::elf::PF_W | bootproto::elf::PF_X) != (bootproto::elf::PF_W | bootproto::elf::PF_X), "{} {} has a W+X segment", row.kind, row.name);
	}

	let mut actual: Vec<String> = image.needed_names(&dynamic).unwrap_or_else(|| panic!("{} {} has malformed DT_NEEDED names", row.kind, row.name)).map(String::from).collect();
	actual.sort();
	assert!(!actual.windows(2).any(|pair| pair[0] == pair[1]), "{} {} repeats a DT_NEEDED provider", row.kind, row.name);
	assert_eq!(actual, expected, "{} {} DT_NEEDED providers differ from the manifest", row.kind, row.name);
}

fn derive_dynamic_order(row: &ManifestRow, libraries: &[ManifestRow]) -> Vec<String> {
	let mut closure: Vec<&ManifestRow> = Vec::new();
	let mut depths: Vec<(&str, usize)> = Vec::new();
	let mut pending: Vec<(&str, usize)> = row.providers.iter().map(|provider| (provider.as_str(), 0)).collect();
	while let Some((name, depth)) = pending.pop() {
		assert!(depth < bootproto::elf::MAX_DYNAMIC_DEPENDENCY_DEPTH, "dynamic {} provider graph exceeds dependency depth {}", row.name, bootproto::elf::MAX_DYNAMIC_DEPENDENCY_DEPTH);
		if let Some((_, known_depth)) = depths.iter_mut().find(|(provider, _)| *provider == name) {
			if *known_depth >= depth {
				continue;
			}
			*known_depth = depth;
		} else {
			depths.push((name, depth));
		}
		if let Some(library) = closure.iter().find(|library| library.name == name) {
			pending.extend(library.providers.iter().map(|provider| (provider.as_str(), depth + 1)));
			continue;
		}
		let library = libraries.iter().find(|library| library.name == name).unwrap_or_else(|| panic!("dynamic {} order closure names unstaged provider {name}", row.name));
		assert!(closure.len() < bootproto::elf::MAX_DYNAMIC_MODULES, "dynamic {} has an oversized provider closure", row.name);
		closure.push(library);
		pending.extend(library.providers.iter().map(|provider| (provider.as_str(), depth + 1)));
	}
	let mut expected: Vec<String> = Vec::with_capacity(closure.len());
	while expected.len() < closure.len() {
		let next = closure.iter().filter(|library| !expected.iter().any(|name| name == &format!("{}.lslib", library.name)) && library.providers.iter().all(|provider| expected.iter().any(|name| name == &format!("{provider}.lslib")))).min_by(|left, right| left.name.cmp(&right.name)).unwrap_or_else(|| panic!("dynamic {} provider graph contains a cycle", row.name));
		expected.push(format!("{}.lslib", next.name));
	}
	expected
}

// The debug-build target path of a userspace ELF: each crate builds to its own target dir.
// The target triple follows the kernel's target arch, so an aarch64 kernel stages the
// aarch64 userspace ELFs (and x86_64 the x86_64 ones).
fn user_elf_path(manifest: &Path, _crate_path: &str, name: &str) -> PathBuf {
	manifest.join(format!("../../.build/cargo/user/{}/debug/{name}", user_target()))
}

fn user_shared_path(manifest: &Path, destination: &str) -> PathBuf {
	manifest.join(format!("../../.build/system-image/{}/{}", user_target(), destination))
}

fn user_dynamic_path(manifest: &Path, destination: &str) -> PathBuf {
	let path = destination.strip_suffix(abi::EXECUTABLE_SUFFIX).unwrap_or_else(|| panic!("dynamic destination has no executable suffix: {destination}"));
	manifest.join(format!("../../.build/system-image/{}/{}", user_target(), path))
}

fn identity_record(artifact: &Path) -> Vec<u8> {
	let bytes = fs::read(artifact).unwrap_or_else(|error| panic!("cannot read {}: {error}", artifact.display()));
	let image = bootproto::elf::Elf::parse_for_machine(&bytes, user_elf_machine()).unwrap_or_else(|| panic!("{} has no valid target ELF", artifact.display()));
	image.liber_identity_note().unwrap_or_else(|| panic!("{} has no valid identity note", artifact.display())).to_vec()
}

fn sha256_hex(bytes: &[u8]) -> String {
	bootproto::sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn audit_identity(row: &ManifestRow, artifact: &Path, libraries: &[ManifestRow], expected_rustc_commit: &str) -> Vec<u8> {
	let bytes = identity_record(artifact);
	assert!(bytes.ends_with(b"\n"), "{} identity record is not newline terminated", row.name);
	let text = core::str::from_utf8(&bytes).unwrap_or_else(|_| panic!("identity for {} is not UTF-8", row.name));
	let lines: Vec<&str> = text.lines().collect();
	assert!(lines.len() >= 10 && lines[0] == "format=liber-image-identity-v1", "{} has malformed identity record", row.name);
	let expected_kind = if row.kind == "library" { "library" } else { "executable" };
	assert_eq!(lines[1], format!("kind={expected_kind}"), "{} identity kind", row.name);
	assert_eq!(lines[2], format!("artifact={}", row.name), "{} identity artifact", row.name);
	assert_eq!(lines[3], format!("package={}", row.crate_dir), "{} identity package", row.name);
	assert!(lines[4].strip_prefix("source-sha256=").is_some_and(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())), "{} identity source digest", row.name);
	assert_eq!(lines[5], format!("rustc-commit={expected_rustc_commit}"), "{} identity toolchain", row.name);
	assert_eq!(lines[6], format!("target={}", user_target()), "{} identity target", row.name);
	assert_eq!(lines[7], "profile=release", "{} identity profile", row.name);
	assert!(lines[8].starts_with("rustflags=-C relocation-model=pic"), "{} identity codegen flags", row.name);
	assert!(lines[9].starts_with("features="), "{} identity features", row.name);
	let mut expected_providers: Vec<String> = row
		.providers
		.iter()
		.map(|provider| {
			let provider_row = libraries.iter().find(|candidate| candidate.name == *provider).unwrap_or_else(|| panic!("{} identity names unknown provider {provider}", row.name));
			let provider_artifact = user_shared_path(&PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR")), provider_row.destination.as_deref().expect("provider destination"));
			format!("provider={provider}:{}", sha256_hex(&identity_record(&provider_artifact)))
		})
		.collect();
	expected_providers.sort();
	assert_eq!(&lines[10..], expected_providers.as_slice(), "{} identity provider chain", row.name);
	bytes
}

fn executable_artifact_name(name: &str) -> String {
	format!("{name}.lsexe")
}

// The userspace target triple matching the kernel's target arch.
fn user_target() -> &'static str {
	match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
		Ok("aarch64") => "aarch64-unknown-none",
		Ok("riscv64") => "riscv64gc-unknown-none-elf",
		_ => "x86_64-unknown-none",
	}
}

fn user_elf_machine() -> u16 {
	match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
		Ok("aarch64") => bootproto::elf::EM_AARCH64,
		Ok("riscv64") => bootproto::elf::EM_RISCV,
		_ => bootproto::elf::EM_X86_64,
	}
}

// Where the assembled packages are written. On AArch64 and RISC-V there is no
// bootloader module hand-off, so the packages go to OUT_DIR and are embedded into the
// kernel image. On x86_64 they go to the repository build root for mkimage.sh to place as
// separate boot modules. Content-aware writes below preserve timestamps when package
// bytes are unchanged, avoiding an embedded kernel relink or x86_64 image restaging.
fn package_out_dir(manifest: &Path) -> PathBuf {
	match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
		Ok("aarch64") | Ok("riscv64") => PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set")),
		_ => manifest.join("../../.build/boot"),
	}
}

// Read a userspace ELF and strip its symbol and debug sections, returning the smaller
// loadable image (both archives execute only the program image, so the symbol and debug
// sections are dead weight - on the volume they bloat the seed archive, in the init
// package they bloat the kernel binary and boot memory). Returns None if the ELF is
// absent (the build still succeeds - the program is simply not staged) or if no `strip`
// tool is available.
fn read_stripped(path: &Path) -> Option<Vec<u8>> {
	if !path.exists() {
		return None;
	}
	let tmp: PathBuf = env::temp_dir().join(format!("liberseed-{}-{}", std::process::id(), path.file_name()?.to_str()?));
	if fs::copy(path, &tmp).is_err() {
		return None;
	}
	// llvm-strip strips any target's ELF (the host binutils `strip` cannot handle a
	// cross-arch ELF, e.g. aarch64 on an x86 host); fall back to the host strip.
	let mut ok = false;
	for (cmd, arg) in [("llvm-strip", "--strip-all"), ("strip", "-s")] {
		if let Ok(status) = Command::new(cmd).arg(arg).arg(&tmp).status() {
			if status.success() {
				ok = true;
				break;
			}
		}
	}
	if !ok {
		println!("cargo:warning=no usable strip tool - omitting {} from the package", path.display());
	}
	let stripped: Option<Vec<u8>> = if ok { fs::read(&tmp).ok() } else { None };
	let _ = fs::remove_file(&tmp);
	stripped
}

// Assemble the init package that the kernel loads as a boot module. The package
// is a tiny archive (a header plus fixed-size entries plus the concatenated file
// blobs) holding the userspace programs - SystemManager plus the StorageService
// and its demo client. It is written to .build/boot/init.pkg, where mkimage.sh
// picks it up.
//
// The userspace ELFs are built separately (the `just user` recipe, a dependency
// of the build/run/test recipes), so by the time the kernel builds they are
// present. Any that are missing - e.g. a bare `cargo build` outside `just`, or
// rust-analyzer - are skipped with a warning, so the kernel build still succeeds
// (the kernel handles an absent program gracefully at runtime).
fn assemble_init_package(conf: &[(String, String)]) {
	let manifest_dir: String = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
	let manifest: PathBuf = PathBuf::from(&manifest_dir);
	let out_dir: PathBuf = package_out_dir(&manifest);
	let out_pkg: PathBuf = out_dir.join(conf_get(conf, "INIT_PACKAGE"));

	// (package entry name, ELF path). The init package holds only the pinned bootstrap set:
	// the pinned services and the bootstrap block driver. Every other service,
	// manager, driver and tool is loaded from the system volume, so it is staged there by
	// assemble_volume_package instead. A pinned row with a real crate (not an `instance`
	// backed by another program) contributes its ELF.
	let mut sources: Vec<(String, PathBuf)> = Vec::new();
	for row in read_manifest(&manifest) {
		if row.stage == "pinned" && row.crate_dir != "-" {
			sources.push((executable_artifact_name(&row.name), user_elf_path(&manifest, &row.crate_path, &row.name)));
		}
	}

	fs::create_dir_all(&out_dir).unwrap_or_else(|e: std::io::Error| panic!("cannot create {}: {e}", out_dir.display()));

	let mut entries: Vec<(&str, Vec<u8>)> = Vec::new();
	for (name, path) in &sources {
		println!("cargo:rerun-if-changed={}", path.display());
		// Strip the pinned ELF to its loadable image, the same as the volume package -
		// the loader executes only the program image, so the symbol and debug sections are
		// dead weight in the kernel binary and boot memory. Fall back to the raw ELF when
		// no `strip` tool is available, so the boot set is never dropped; an absent ELF is
		// skipped with a warning (the kernel handles it gracefully at runtime).
		match read_stripped(path).or_else(|| fs::read(path).ok()) {
			Some(bytes) => entries.push((name.as_str(), bytes)),
			None => println!("cargo:warning={name} ELF not found at {} - omitting from init package (run `just user` or `just build`)", path.display()),
		}
	}

	let package: Vec<u8> = build_package(&entries);
	write_if_changed(&out_pkg, &package);
}

// Assemble the ramdisk volume package: every regular file under src/volume is
// packed into .build/boot/volume.pkg using its relative path. The kernel loads it
// as a second boot module and serves its files through StorageService over vol://.
fn assemble_volume_package(conf: &[(String, String)]) {
	let manifest_dir: String = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
	let manifest: PathBuf = PathBuf::from(&manifest_dir);
	let out_dir: PathBuf = package_out_dir(&manifest);
	let out_pkg: PathBuf = out_dir.join(conf_get(conf, "VOLUME_PACKAGE"));

	fs::create_dir_all(&out_dir).unwrap_or_else(|e: std::io::Error| panic!("cannot create {}: {e}", out_dir.display()));

	let workspace = manifest.join("..");
	let factory_manifest = system_manifest::Manifest::load_workspace(&workspace).unwrap_or_else(|error| panic!("{error}"));
	// Collect every manifest-declared factory record. Sorting below makes archive
	// layout independent of declaration and filesystem enumeration order.
	let mut files: Vec<(String, Vec<u8>)> = Vec::new();
	for file in factory_manifest.factory_files.values() {
		let path = match file.kind {
			system_manifest::FactoryFileKind::Source => manifest.join("..").join(file.source.as_ref().expect("validated factory source").as_str()),
			system_manifest::FactoryFileKind::SdkComponent => manifest.join("../../.build/cargo/sdk/wasm32-unknown-unknown/release/liber_component.wasm"),
		};
		println!("cargo:rerun-if-changed={}", path.display());
		let bytes = fs::read(&path).unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
		files.push((file.destination.as_str().to_string(), bytes));
	}

	let rows = read_manifest(&manifest);
	generate_test_volume_paths(&factory_manifest);
	let library_rows: Vec<ManifestRow> = rows.iter().filter(|row| row.kind == "library" && row.stage == "volume").cloned().collect();
	let lsrt_row = library_rows.iter().find(|row| row.name == "lsrt").expect("lsrt library row");
	let lsrt_artifact = user_shared_path(&manifest, lsrt_row.destination.as_deref().expect("lsrt destination"));
	let lsrt_identity = identity_record(&lsrt_artifact);
	let expected_rustc_commit = core::str::from_utf8(&lsrt_identity).expect("lsrt identity record is UTF-8").lines().find_map(|line| line.strip_prefix("rustc-commit=")).expect("lsrt rustc identity").to_string();
	assert!(expected_rustc_commit.len() == 40 && expected_rustc_commit.bytes().all(|byte| byte.is_ascii_hexdigit()), "lsrt rustc identity is malformed");
	let mut libraries: Vec<String> = rows.iter().filter(|row| row.kind == "library" && row.stage == "volume").map(|row| row.name.clone()).collect();
	libraries.sort();
	assert!(!libraries.windows(2).any(|pair| pair[0] == pair[1]), "duplicate staged library identity");

	// Stage every manifest-declared volume ELF at its exact destination. The copies are
	// stripped of symbol/debug sections because the loader needs only the program image,
	// keeping the seed archive to a few megabytes. A missing or unstrippable ELF is skipped.
	for row in &rows {
		if row.kind == "library" {
			let features = row.features.as_deref().expect("library feature set");
			assert!(features == "-" || features.split(',').all(valid_library_name), "library {} has invalid feature set {features:?}", row.name);
		}
		let dest: String = match row.kind.as_str() {
			"driver" if row.stage == "volume" => row.destination.clone().expect("driver destination"),
			"library" if row.stage == "volume" => row.destination.clone().expect("library destination"),
			"dynamic" | "dynamic-service" if row.stage == "volume" => row.destination.clone().expect("program destination"),
			_ => continue,
		};
		let path: PathBuf = match row.kind.as_str() {
			"library" => user_shared_path(&manifest, row.destination.as_deref().expect("library destination")),
			"dynamic" | "dynamic-service" => user_dynamic_path(&manifest, row.destination.as_deref().expect("program destination")),
			_ => user_elf_path(&manifest, &row.crate_path, &row.name),
		};
		println!("cargo:rerun-if-changed={}", path.display());
		let identity = if row.kind == "dynamic" || row.kind == "dynamic-service" || row.kind == "library" { Some(audit_identity(&row, &path, &library_rows, &expected_rustc_commit)) } else { None };
		// Strip the ELF to its loadable image; fall back to the raw ELF when no
		// `strip` supports the target (the host binutils cannot strip aarch64), so
		// the program is still staged - the loader ignores the extra sections.
		match read_stripped(&path).or_else(|| fs::read(&path).ok()) {
			Some(bytes) => {
				if let Some(identity) = identity.as_deref() {
					let image = bootproto::elf::Elf::parse_for_machine(&bytes, user_elf_machine()).unwrap_or_else(|| panic!("staged {} is not a valid target ELF", row.name));
					assert_eq!(image.liber_identity_note(), Some(identity), "staged {} identity record differs from its source ELF", row.name);
				}
				if row.kind == "dynamic" || row.kind == "dynamic-service" {
					audit_linked_artifact(&row, &bytes, &libraries, true);
					let _order = derive_dynamic_order(&row, &library_rows);
				} else if row.kind == "library" {
					audit_linked_artifact(&row, &bytes, &libraries, row.name != "lsrt");
				}
				files.push((dest, bytes));
			}
			None => println!("cargo:warning={} ELF not found at {} - omitting from system volume (run `just user` or `just build`)", row.name, path.display()),
		}
	}
	files.sort_by(|a, b| a.0.cmp(&b.0));
	assert!(!files.windows(2).any(|pair| pair[0].0 == pair[1].0), "duplicate volume package destination");
	let expected_entries = factory_manifest.volume_destinations();
	let actual_entries = files.iter().map(|(name, _)| name.clone()).collect::<BTreeSet<_>>();
	assert_eq!(actual_entries, expected_entries, "system volume entries differ from the manifest");
	for (name, bytes) in &files {
		if rows.iter().any(|row| matches!(row.kind.as_str(), "dynamic" | "dynamic-service") && row.stage == "volume" && row.destination.as_deref() == Some(name.as_str())) {
			let image = bootproto::elf::Elf::parse_for_machine(bytes, user_elf_machine()).unwrap_or_else(|| panic!("staged /{name} is not a valid target ELF"));
			assert_eq!(image.image_type, bootproto::elf::ET_DYN, "staged /{name} is static ET_EXEC");
		}
	}

	let entries: Vec<(&str, Vec<u8>)> = files.iter().map(|(name, data): &(String, Vec<u8>)| (name.as_str(), data.clone())).collect();
	let package: Vec<u8> = build_package(&entries);
	write_if_changed(&out_pkg, &package);
}

// Serialize a boot package: an 8-byte magic, a u32 entry count and a reserved
// u32, then one 72-byte entry per file (a 64-byte NUL-padded name, a u32 absolute
// byte offset and a u32 size), then the concatenated file blobs. All integers are
// little-endian. Must match the parser in src/kernel/pkg.rs.
fn build_package(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
	use abi::{PKG_ENTRY_LEN as ENTRY_LEN, PKG_HEADER_LEN as HEADER_LEN, PKG_NAME_LEN as NAME_LEN};

	for (index, (name, _)) in entries.iter().enumerate() {
		for (other, _) in &entries[index + 1..] {
			assert!(!abi::executable_aliases_ambiguous(name.as_bytes(), other.as_bytes()), "ambiguous executable artifacts: {name} and {other}");
		}
	}

	let table_len: usize = HEADER_LEN + ENTRY_LEN * entries.len();
	let mut out: Vec<u8> = Vec::new();
	out.extend_from_slice(abi::PKG_MAGIC);
	out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
	out.extend_from_slice(&0u32.to_le_bytes());

	let mut blob_offset: usize = table_len;
	let mut blobs: Vec<u8> = Vec::new();
	for (name, data) in entries {
		let mut name_field: [u8; NAME_LEN] = [0u8; NAME_LEN];
		let name_bytes: &[u8] = name.as_bytes();
		assert!(name_bytes.len() <= NAME_LEN, "package entry name exceeds {NAME_LEN} bytes: {name}");
		name_field[..name_bytes.len()].copy_from_slice(name_bytes);
		out.extend_from_slice(&name_field);
		out.extend_from_slice(&(blob_offset as u32).to_le_bytes());
		out.extend_from_slice(&(data.len() as u32).to_le_bytes());
		blob_offset += data.len();
		blobs.extend_from_slice(data);
	}
	out.extend_from_slice(&blobs);
	out
}
