//! mkpackages - assemble the boot packages from an already-built userspace.
//!
//! This is the packaging step, and it deliberately compiles nothing. It reads the manifest,
//! collects the userspace ELFs the build produced, and writes `init.pkg` (the programs that must
//! run before the system volume is readable) and `volume.pkg` (the volume image itself).
//!
//! It used to live in `kernel/build.rs`, which made building the kernel depend on having built
//! the userspace and let a bare `cargo build` produce a silently incomplete package. The kernel
//! crate now compiles the kernel and nothing else.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

// Check that every artifact the manifest names has actually been built, BEFORE assembling
// anything. This is the whole point of packaging being its own phase: it verifies and then
// assembles, so a missing program is one clear message here rather than a panic halfway through
// an audit, or - worse - a package quietly missing a program and an image that fails at boot.
//
// Every missing artifact is reported, not just the first: being told about one, rebuilding, and
// being told about the next is a slow way to learn that the userspace was never built.
fn verify_artifacts() {
	let manifest = kernel_anchor();
	let mut missing: Vec<String> = Vec::new();
	for row in read_manifest(&manifest) {
		let path: PathBuf = match (row.kind.as_str(), row.stage.as_str()) {
			(_, "pinned") if row.crate_dir != "-" => user_elf_path(&manifest, &row.crate_path, &row.name),
			("library", "volume") => user_shared_path(&manifest, row.destination.as_deref().expect("library destination")),
			("dynamic" | "dynamic-service", "volume") => user_dynamic_path(&manifest, row.destination.as_deref().expect("program destination")),
			("driver" | "service", "volume") => user_elf_path(&manifest, &row.crate_path, &row.name),
			_ => continue,
		};
		if !path.exists() {
			missing.push(format!("  {} ({})", row.name, path.display()));
		}
	}
	if !missing.is_empty() {
		eprintln!("mkpackages: {} artifact(s) named by the manifest are not built:", missing.len());
		for entry in &missing {
			eprintln!("{entry}");
		}
		eprintln!("mkpackages: packaging compiles nothing - run `just user` first");
		std::process::exit(1);
	}
}

// The repository root, found by walking up for `product.conf` rather than by counting `..`
// segments from wherever this binary happens to live.
fn repo_root() -> PathBuf {
	let mut dir: PathBuf = env::current_dir().expect("current directory");
	loop {
		if dir.join("product.conf").is_file() {
			return dir;
		}
		if !dir.pop() {
			panic!("cannot locate product.conf above the current directory");
		}
	}
}

// This code was moved out of `kernel/build.rs`, where every path was written relative to the
// kernel crate. Keeping that anchor means the joins below are the same ones that were tested
// there, rather than a dozen rewrites done in one go.
fn kernel_anchor() -> PathBuf {
	repo_root().join("src/kernel")
}

// Both packages now go to `.build/boot` for every architecture. They used to land in the
// kernel's OUT_DIR for aarch64 and riscv64 because the kernel embedded them; it no longer does.
fn boot_dir() -> PathBuf {
	let dir = repo_root().join(".build/boot");
	let _ = fs::create_dir_all(&dir);
	dir
}

// The whole image, held in memory while it is built and written out once. A system volume is
// tens of megabytes, which is nothing on a build host, and it keeps the block device trivial.
struct Image {
	bytes: Vec<u8>,
	block: usize,
}

impl fscore::BlockDevice for Image {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		let start = index as usize * self.block;
		let Some(src) = self.bytes.get(start..start + buf.len()) else { return false };
		buf.copy_from_slice(src);
		true
	}

	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
		let start = index as usize * self.block;
		let Some(dst) = self.bytes.get_mut(start..start + buf.len()) else { return false };
		dst.copy_from_slice(buf);
		true
	}
}

// Build the system volume as a LiberFS image: the same files the archive carries, plus the
// kernel and the pinned bootstrap set at real paths.
//
// This is the artifact M0138 is about. Until it existed there was no filesystem on the disk at
// the moment the loader ran - the disk carried an archive and the storage service formatted a
// volume from it after boot - so the loader had nothing to read and no program that runs had a
// file the user could look at.
//
// Written BESIDE the archive rather than instead of it, deliberately and temporarily: the
// storage service still seeds itself from `volume.pkg`, so replacing it in the same step that
// teaches the loader to read a filesystem would change two things at once and leave no way to
// tell which one broke the boot. Retiring the archive is its own item.
fn assemble_system_volume(conf: &[(String, String)], files: &[(String, Vec<u8>)]) {
	const BLOCK: usize = 4096;
	let out_dir: PathBuf = boot_dir();
	// Per architecture, like the volume archive it replaces: the staged binaries differ, so one
	// image cannot serve all three.
	let arch = env::var("CARGO_CFG_TARGET_ARCH").expect("architecture set by main");
	let stem = conf_get(conf, "SYSTEM_VOLUME");
	let name = stem.strip_suffix(".img").unwrap_or(stem);
	let out_img: PathBuf = out_dir.join(format!("{name}-{arch}.img"));
	let manifest: PathBuf = kernel_anchor();

	// Everything the archive carries, at the same destinations.
	let mut staged: Vec<(String, Vec<u8>)> = files.to_vec();

	// The kernel, so the loader can read it from the volume rather than from the ESP - but ONLY
	// when asked for.
	//
	// Staging whatever happens to be at `.build/boot/kernel` made the volume's contents depend on
	// which recipe ran last: a test run after `just img` booted the shipping kernel from the
	// volume instead of the test binary the harness had just built, and sat at a shell prompt
	// until the watchdog fired. A shipping image wants its kernel on the volume; a test medium
	// wants the harness's kernel, which is built elsewhere and staged on the ESP.
	// `--with-kernel=<path>`, and the path is REQUIRED rather than assumed.
	//
	// It used to read `.build/boot/kernel`, a slot every image builder writes to and nobody owns.
	// The volume is built before that slot is written, so it took whatever the previous recipe had
	// left there - and a `just img` after a test run put the TEST kernel on the shipping volume,
	// which then booted into the test suite off a disk image. This is the second time that shared
	// slot has done exactly this; naming the file removes the slot from the path entirely.
	if let Some(arg) = env::args().find(|arg| arg.starts_with("--with-kernel=")) {
		let kernel = PathBuf::from(&arg["--with-kernel=".len()..]);
		match fs::read(&kernel) {
			Ok(bytes) => staged.push((String::from("kernel"), bytes)),
			Err(error) => panic!("mkpackages: cannot read the kernel at {}: {error}", kernel.display()),
		}
	}

	// The pinned bootstrap set at real paths. This is the point of the milestone: a program that
	// runs should have a file on the volume, not only an entry inside an archive.
	//
	// The list of them is written to the volume as well, because the loader has no manifest: it
	// needs to know WHICH files to read before the system that knows is running. This is the
	// "manifest-named list" the milestone describes - `init.pkg` stops being an artifact and
	// becomes a list, with the archive itself assembled in memory by the loader so the kernel and
	// SystemManager keep receiving exactly what they receive today.
	let mut bootstrap = String::new();
	// The pinned programs alone, kept aside for the boot medium's fallback copy. `staged` is the
	// whole volume - 154 files - and the fallback needs only what the loader must read before the
	// system that knows the rest is running.
	let mut fallback: Vec<(String, Vec<u8>)> = Vec::new();
	for row in read_manifest(&manifest) {
		if row.stage == "pinned" && row.crate_dir != "-" {
			let name = executable_artifact_name(&row.name);
			let path = user_elf_path(&manifest, &row.crate_path, &row.name);
			match read_stripped(&path).or_else(|| fs::read(&path).ok()) {
				Some(bytes) => {
					// The entry name inside the archive, then the path to read it from. The
					// loader needs both: the kernel looks entries up by the name they have
					// today, which is not the path they now live at.
					bootstrap.push_str(&format!("{name} libexec/{name}\n"));
					fallback.push((format!("libexec/{name}"), bytes.clone()));
					staged.push((format!("libexec/{name}"), bytes));
				}
				None => eprintln!("mkpackages: pinned {name} not built: {}", path.display()),
			}
		}
	}
	staged.push((String::from("etc/bootstrap.list"), bootstrap.clone().into_bytes()));

	// The SAME files, written out for staging on the boot medium's own filesystem.
	//
	// This is what lets `init.pkg` stop existing. A machine whose system volume is missing or
	// unreadable still needs a bootstrap set, and it used to get one as a packaged archive staged
	// beside the volume - a second mechanism for a job the volume already does one way. The loader
	// now reads a list and files in both places, so the fallback is the same code reading the same
	// shapes, and the programs on it can be replaced one at a time instead of rebuilt as a blob.
	{
		fallback.push((String::from("etc/bootstrap.list"), bootstrap.clone().into_bytes()));
		// Architecture-qualified, like `init-<arch>.pkg` beside it. An unqualified directory is the
		// same trap that put x86_64 programs on a riscv64 ESP: every architecture's build writes
		// it, so it holds whichever built last.
		let arch: String = env::args().nth(1).unwrap_or_default();
		let root = boot_dir().join(format!("bootstrap-{arch}"));
		let _ = fs::remove_dir_all(&root);
		for (name, bytes) in &fallback {
			let path = root.join(name);
			if let Some(parent) = path.parent() {
				let _ = fs::create_dir_all(parent);
			}
			write_if_changed(&path, bytes);
		}
	}

	// Sized to hold what is staged with room to grow, rounded to whole blocks. LiberFS needs
	// metadata beyond the file bytes, so the slack is not decoration.
	let payload: usize = staged.iter().map(|(name, bytes)| name.len() + bytes.len()).sum();
	let size = ((payload * 2).max(16 * 1024 * 1024) + BLOCK - 1) / BLOCK * BLOCK;
	let opts = liberfs::FormatOpts { uuid: *b"libersystem-vol\0", label: b"system".to_vec(), compress: false };
	let mut fs_image = liberfs::LiberFs::format_opts(Image { bytes: vec![0u8; size], block: BLOCK }, (size / BLOCK) as u64, opts).unwrap_or_else(|error| panic!("mkpackages: cannot format a {size}-byte system volume: {error:?}"));

	let mut made: BTreeSet<String> = BTreeSet::new();
	for (destination, bytes) in &staged {
		let destination = destination.trim_start_matches('/');
		if let Some((dirs, _)) = destination.rsplit_once('/') {
			let mut prefix = String::new();
			for segment in dirs.split('/') {
				if !prefix.is_empty() {
					prefix.push('/');
				}
				prefix.push_str(segment);
				if made.insert(prefix.clone()) {
					fs_image.mkdir(prefix.as_bytes()).unwrap_or_else(|error| panic!("mkpackages: cannot create /{prefix}: {error:?}"));
				}
			}
		}
		fs_image.write_file(destination.as_bytes(), bytes).unwrap_or_else(|error| panic!("mkpackages: cannot write /{destination} ({} bytes): {error:?}", bytes.len()));
	}

	// Read the image back through the same path the loader takes, before it is written out. A
	// volume that formats but does not mount is worth catching here rather than on a machine
	// that will not boot.
	let image = fs_image.into_device();
	let mut check = liberfs::LiberFs::mount(Image { bytes: image.bytes.clone(), block: BLOCK }).unwrap_or_else(|| panic!("mkpackages: the system volume does not mount"));
	for (destination, bytes) in &staged {
		let destination = destination.trim_start_matches('/');
		let read = check.read_file(destination.as_bytes()).unwrap_or_else(|error| panic!("mkpackages: /{destination} was written but cannot be read back: {error:?}"));
		assert_eq!(read.len(), bytes.len(), "mkpackages: /{destination} reads back a different size");
	}

	write_if_changed(&out_img, &image.bytes);
}

fn main() {
	// The target architecture the packages are for, as cargo spells it - the caller passes what
	// build.rs used to read from CARGO_CFG_TARGET_ARCH.
	let arch: String = env::args().nth(1).unwrap_or_else(|| {
		eprintln!("usage: mkpackages <target-arch>");
		std::process::exit(2);
	});
	unsafe {
		env::set_var("CARGO_CFG_TARGET_ARCH", &arch);
	}
	let conf: Vec<(String, String)> = read_product_conf();

	// The system volume image is built in a SEPARATE step, after the kernel is linked, because it
	// carries the kernel. The archives cannot move after the kernel for the opposite reason - the
	// kernel embeds them - so the two cannot share one pass, and building the image alongside the
	// archives would stage whichever kernel the previous build left behind.
	if env::args().nth(2).as_deref() == Some("system-volume") {
		verify_artifacts();
		assemble_system_volume(&conf, &volume_files(&conf));
		return;
	}

	verify_artifacts();
	assemble_init_package(&conf);
	assemble_volume_package(&conf);
	export_cross_arch_volume();
}

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

// Parse ../../product.conf (shell-style KEY="value") into key/value pairs (the
// single source of truth for both the product metadata and the boot artifact
// filenames).
fn read_product_conf() -> Vec<(String, String)> {
	let path: PathBuf = repo_root().join("product.conf");
	let text: String = fs::read_to_string(&path).unwrap_or_else(|e: std::io::Error| panic!("cannot read {}: {e}", path.display()));
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

// Look up a required key from the parsed product.conf.
fn conf_get<'a>(conf: &'a [(String, String)], key: &str) -> &'a str {
	for (k, v) in conf {
		if k.as_str() == key {
			return v.as_str();
		}
	}
	panic!("missing {key} in product.conf");
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

fn timing_event(phase: &str, event: &str) {
	let Ok(path) = env::var("LIBER_TIMING_LOG") else { return };
	let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else { return };
	let timestamp_ns: u128 = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
	let _ = writeln!(file, "{timestamp_ns}\t{phase}\t{event}");
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

// Whether a manifest program belongs in this build at all. A development-only program is
// built and staged only when the `development` feature is on, so a shipped image does not
// contain the development agent, its artifact registry or the control port's transport -
// absent, rather than present and refusing to run, which is the difference between a
// boundary and a policy.
fn included(program: &system_manifest::Program) -> bool {
	!program.development || development_configuration()
}

// Whether this build wants the development-only programs. Read from the environment cargo
// sets for build scripts, NOT with `cfg!(feature = ...)`: a build script is a separate
// compilation and that macro does not see the crate's features there, so it silently reports
// false and the development configuration stages the shipping set - which is exactly what it
// did before this was written down.
// Whether this build wants the development-only programs. Read from the plain environment
// variable the build recipes set, and declared so cargo re-runs this script when it flips.
//
// Not from a cargo feature, and that took two attempts to get right. `cfg!(feature = ...)`
// does not work here at all: a build script is a separate compilation and the macro silently
// reports false. Reading `CARGO_FEATURE_DEVELOPMENT` does see the feature, but this script
// emits `rerun-if-changed`, which switches cargo to re-running only for what was declared -
// and `rerun-if-env-changed` does not apply to the `CARGO_FEATURE_*` variables cargo sets
// itself, so the script kept its previous output and the development build went on staging
// the shipping set. Both failures are silent, and both make every assertion inside the build
// agree with itself while producing the wrong image. A plain variable has neither problem.
fn development_configuration() -> bool {
	env::var("LIBER_DEVELOPMENT").as_deref() == Ok("1")
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
	for program in model.programs.values().filter(|program| included(program)) {
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

fn valid_library_name(name: &str) -> bool {
	!name.is_empty() && !name.starts_with("lib") && name.len() <= 58 && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn sha256_hex(bytes: &[u8]) -> String {
	bootproto::sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn executable_artifact_name(name: &str) -> String {
	format!("{name}.lsexe")
}

// The debug-build target path of a userspace ELF: each crate builds to its own target dir.
// The target triple follows the kernel's target arch, so an aarch64 kernel stages the
// aarch64 userspace ELFs (and x86_64 the x86_64 ones).
fn user_elf_path(manifest: &Path, _crate_path: &str, name: &str) -> PathBuf {
	manifest.join(format!("../../.build/cargo/user/{}/debug/{name}", user_target()))
}

fn user_shared_path(manifest: &Path, destination: &str) -> PathBuf {
	manifest.join(format!("../../.build/image/{}/{}", user_target(), destination))
}

fn user_dynamic_path(manifest: &Path, destination: &str) -> PathBuf {
	let path = destination.strip_suffix(abi::EXECUTABLE_SUFFIX).unwrap_or_else(|| panic!("dynamic destination has no executable suffix: {destination}"));
	manifest.join(format!("../../.build/image/{}/{}", user_target(), path))
}

fn identity_record(artifact: &Path) -> Vec<u8> {
	let bytes = fs::read(artifact).unwrap_or_else(|error| panic!("cannot read {}: {error}", artifact.display()));
	let image = bootproto::elf::Elf::parse_for_machine(&bytes, user_elf_machine()).unwrap_or_else(|| panic!("{} has no valid target ELF", artifact.display()));
	image.liber_identity_note().unwrap_or_else(|| panic!("{} has no valid identity note", artifact.display())).to_vec()
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
			let provider_artifact = user_shared_path(&kernel_anchor(), provider_row.destination.as_deref().expect("provider destination"));
			format!("provider={provider}:{}", sha256_hex(&identity_record(&provider_artifact)))
		})
		.collect();
	expected_providers.sort();
	assert_eq!(&lines[10..], expected_providers.as_slice(), "{} identity provider chain", row.name);
	bytes
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
		eprintln!("mkpackages: no usable strip tool - omitting {} from the package", path.display());
	}
	let stripped: Option<Vec<u8>> = if ok { fs::read(&tmp).ok() } else { None };
	let _ = fs::remove_file(&tmp);
	stripped
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
	let manifest: PathBuf = kernel_anchor();
	let out_dir: PathBuf = boot_dir();
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
	let mut missing: usize = 0;
	for (name, path) in &sources {
		// Strip the pinned ELF to its loadable image, the same as the volume package -
		// the loader executes only the program image, so the symbol and debug sections are
		// dead weight in the kernel binary and boot memory. Fall back to the raw ELF when
		// no `strip` tool is available, so the boot set is never dropped. A MISSING ELF is fatal:
		// this step decides whether a bootable system exists, and a boot package assembled
		// without one of the programs the system needs before its volume is readable produces an
		// image that fails at boot rather than here. That was the old behaviour - a warning
		// nobody reads and a package quietly missing a program.
		match read_stripped(path).or_else(|| fs::read(path).ok()) {
			Some(bytes) => entries.push((name.as_str(), bytes)),
			None => {
				eprintln!("mkpackages: {name} not built: {} (run `just user`)", path.display());
				missing += 1;
			}
		}
	}

	let package: Vec<u8> = build_package(&entries);
	if missing != 0 {
		eprintln!("mkpackages: {missing} program(s) of the boot set are not built - refusing to assemble an image that cannot boot");
		std::process::exit(1);
	}
	write_if_changed(&out_pkg, &package);
}

// Assemble the ramdisk volume package: every regular file under src/volume is
// packed into .build/boot/volume.pkg using its relative path. The kernel loads it
// as a second boot module and serves its files through StorageService over vol://.
fn assemble_volume_package(conf: &[(String, String)]) {
	let files = volume_files(conf);
	let out_dir: PathBuf = boot_dir();
	let out_pkg: PathBuf = out_dir.join(conf_get(conf, "VOLUME_PACKAGE"));
	let entries: Vec<(&str, Vec<u8>)> = files.iter().map(|(name, data): &(String, Vec<u8>)| (name.as_str(), data.clone())).collect();
	write_if_changed(&out_pkg, &build_package(&entries));
}

// Every file the system volume carries, at its manifest destination. Shared by the archive and
// the LiberFS image so the two cannot stage different sets - which is exactly the drift between
// an artifact and the volume that this milestone removes.
fn volume_files(conf: &[(String, String)]) -> Vec<(String, Vec<u8>)> {
	let _ = conf;
	let manifest: PathBuf = kernel_anchor();
	let out_dir: PathBuf = boot_dir();

	fs::create_dir_all(&out_dir).unwrap_or_else(|e: std::io::Error| panic!("cannot create {}: {e}", out_dir.display()));

	let workspace = manifest.join("..");
	let factory_manifest = system_manifest::Manifest::load_workspace(&workspace).unwrap_or_else(|error| panic!("{error}"));
	// Collect every manifest-declared factory record. Sorting below makes archive
	// layout independent of declaration and filesystem enumeration order.
	let mut files: Vec<(String, Vec<u8>)> = Vec::new();
	let mut missing: usize = 0;
	for file in factory_manifest.factory_files.values() {
		let path = match file.kind {
			system_manifest::FactoryFileKind::Source => manifest.join("..").join(file.source.as_ref().expect("validated factory source").as_str()),
			system_manifest::FactoryFileKind::SdkComponent => manifest.join("../../.build/cargo/sdk/wasm32-unknown-unknown/release/liber_component.wasm"),
		};
		let bytes = fs::read(&path).unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
		files.push((file.destination.as_str().to_string(), bytes));
	}

	let rows = read_manifest(&manifest);
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
			// A static service staged to the volume, rather than pinned into the init package:
			// the development agent is spawned by DeviceManager from the volume the way a
			// driver is, so it must not be linked dynamically (that path needs ProcessService)
			// and must not be pinned (it is development-only and has no business in the
			// boot-critical bundle).
			"driver" | "service" if row.stage == "volume" => row.destination.clone().expect("program destination"),
			"library" if row.stage == "volume" => row.destination.clone().expect("library destination"),
			"dynamic" | "dynamic-service" if row.stage == "volume" => row.destination.clone().expect("program destination"),
			_ => continue,
		};
		let path: PathBuf = match row.kind.as_str() {
			"library" => user_shared_path(&manifest, row.destination.as_deref().expect("library destination")),
			"dynamic" | "dynamic-service" => user_dynamic_path(&manifest, row.destination.as_deref().expect("program destination")),
			_ => user_elf_path(&manifest, &row.crate_path, &row.name),
		};
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
			None => {
				eprintln!("mkpackages: {} not built: {} (run `just user`)", row.name, path.display());
				missing += 1;
			}
		}
	}
	files.sort_by(|a, b| a.0.cmp(&b.0));
	assert!(!files.windows(2).any(|pair| pair[0].0 == pair[1].0), "duplicate volume package destination");
	let expected_entries = factory_manifest.volume_destinations(development_configuration());
	let actual_entries = files.iter().map(|(name, _)| name.clone()).collect::<BTreeSet<_>>();
	assert_eq!(actual_entries, expected_entries, "system volume entries differ from the manifest");
	for (name, bytes) in &files {
		if rows.iter().any(|row| matches!(row.kind.as_str(), "dynamic" | "dynamic-service") && row.stage == "volume" && row.destination.as_deref() == Some(name.as_str())) {
			let image = bootproto::elf::Elf::parse_for_machine(bytes, user_elf_machine()).unwrap_or_else(|| panic!("staged /{name} is not a valid target ELF"));
			assert_eq!(image.image_type, bootproto::elf::ET_DYN, "staged /{name} is static ET_EXEC");
		}
	}

	if missing != 0 {
		eprintln!("mkpackages: {missing} program(s) of the system volume are not built - refusing to assemble an image that cannot boot");
		std::process::exit(1);
	}
	files
}

// Expose the assembled volume package at a stable path so the direct AArch64/RISC-V QEMU
// runners can lay the factory archive onto virtio-blk at LBA 0.
// Wrap the two boot packages into ONE archive for the architectures that can only be handed a
// single blob. aarch64 and riscv64 virt have no bootloader to pass files, so the runner loads
// this archive into memory and the kernel finds it there.
//
// The `boot-packages-<arch>.pkg` wrapper this used to build is gone (M0138c). It existed only
// because a machine booted with `-kernel` can be handed exactly one blob; all three architectures
// now boot through the loader, which hands over each module under its own name.
fn export_cross_arch_volume() {
	let arch: String = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
	if arch == "aarch64" || arch == "riscv64" {
		let out_dir: PathBuf = boot_dir();
		let build_dir: PathBuf = boot_dir();
		let _ = fs::create_dir_all(&build_dir);
		let vol_src: PathBuf = out_dir.join("volume.pkg");
		let init_src: PathBuf = out_dir.join("init.pkg");
		if vol_src.exists() {
			let bytes: Vec<u8> = fs::read(&vol_src).unwrap_or_else(|error| panic!("cannot read {}: {error}", vol_src.display()));
			write_if_changed(&build_dir.join(format!("volume-{arch}.pkg")), &bytes);
		}
		// `init.pkg` is written under one name by EVERY architecture's build, so the copy in
		// `.build/boot` is simply whichever built last. That is invisible until something reads it
		// by that name: the UEFI ESP builder did, and staged x86_64 programs onto a riscv64 boot
		// disk, where the kernel loaded the package fine and then failed to start SystemManager
		// because its ELF was for another machine. Exported per architecture for the same reason
		// the volume already is.
		if init_src.exists() {
			let bytes: Vec<u8> = fs::read(&init_src).unwrap_or_else(|error| panic!("cannot read {}: {error}", init_src.display()));
			write_if_changed(&build_dir.join(format!("init-{arch}.pkg")), &bytes);
		}
	}
}
