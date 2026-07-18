// build.rs - selects the linker script by target arch and exposes the product
// metadata from product.conf (the single source of truth) to the kernel as
// compile-time environment variables.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
	println!("cargo:rerun-if-env-changed=TEST_TAGS");
	select_linker_script();
	let conf: Vec<(String, String)> = read_product_conf();
	export_product_metadata(&conf);
	assemble_init_package(&conf);
	assemble_volume_package(&conf);
	embed_aarch64_demo();
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

// On aarch64 / riscv64, embed the pre-built `echo` userspace ELF so the kernel can load
// and run a real rt-based program as an end-to-end bring-up demo. Written to OUT_DIR
// (empty when the ELF is absent, e.g. a bare `cargo build` without the userspace
// built first), so the kernel always builds and the demo simply does not run. On
// x86_64 the embedded blob is empty (it routes userspace via the init package instead).
fn embed_aarch64_demo() {
	let out_dir: PathBuf = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
	let dest: PathBuf = out_dir.join("echo_demo.elf");
	let arch: String = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
	let bytes: Vec<u8> = if arch == "aarch64" || arch == "riscv64" {
		let manifest_dir: String = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
		let elf: PathBuf = PathBuf::from(&manifest_dir).join(format!("../user/tools/target/{}/debug/echo", user_target()));
		println!("cargo:rerun-if-changed={}", elf.display());
		fs::read(&elf).unwrap_or_default()
	} else {
		Vec::new()
	};
	fs::write(&dest, &bytes).unwrap_or_else(|e: std::io::Error| panic!("cannot write {}: {e}", dest.display()));

	// Expose the assembled volume package at a stable path so the aarch64 QEMU
	// runner can lay it (the factory archive) onto a virtio-blk disk at LBA 0, which
	// StorageService reads to format and seed the vol://system volume.
	if arch == "aarch64" || arch == "riscv64" {
		let manifest_dir: String = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
		let build_dir: PathBuf = PathBuf::from(&manifest_dir).join("../boot/.build");
		let _ = fs::create_dir_all(&build_dir);
		let vol_src: PathBuf = out_dir.join("volume.pkg");
		if vol_src.exists() {
			let _ = fs::copy(&vol_src, build_dir.join(format!("volume-{arch}.pkg")));
		}
	}
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
// (../user/services/manifest.txt) - the single source of truth ServiceManager also
// generates its dependency table from, so the runtime service set and the staged
// programs cannot drift. Each staged program is one manifest row: `kind name crate
// stage [deps...]`. The kind and stage columns sort a row into the init package (the
// pinned bootstrap set on the path to mounting the system volume, plus the bootstrap
// block driver that backs it) or onto the system volume (every other service, driver,
// tool and demo component, loaded from there once it is mounted).

// A staged program parsed from a manifest row: its kind, its package entry name, the
// crate directory under ../user that builds it, and where it is staged.
#[derive(Clone)]
struct ManifestRow {
	kind: String,
	name: String,
	crate_dir: String,
	stage: String,
	features: Option<String>,
	providers: Vec<String>,
}

// Read and parse the shared service manifest, keeping every row that names a staged
// program (an `instance` row is a managed service backed by another program's ELF, so
// it stages nothing of its own - its `crate` is `-` and its `stage` is `none`).
fn read_manifest(manifest: &Path) -> Vec<ManifestRow> {
	let path: PathBuf = manifest.join("../user/services/manifest.txt");
	let text: String = fs::read_to_string(&path).unwrap_or_else(|e: std::io::Error| panic!("cannot read {}: {e}", path.display()));
	println!("cargo:rerun-if-changed={}", path.display());
	let mut rows: Vec<ManifestRow> = Vec::new();
	for line in text.lines() {
		let trimmed: &str = line.trim();
		if trimmed.is_empty() || trimmed.starts_with('#') {
			continue;
		}
		let mut fields = trimmed.split_whitespace();
		let kind: String = fields.next().expect("manifest row missing kind").to_string();
		let name: String = fields.next().expect("manifest row missing name").to_string();
		let crate_dir: String = fields.next().expect("manifest row missing crate").to_string();
		let stage: String = fields.next().expect("manifest row missing stage").to_string();
		let features = (kind == "library").then(|| fields.next().expect("library manifest row missing feature set").to_string());
		let providers: Vec<String> = fields.map(String::from).collect();
		rows.push(ManifestRow { kind, name, crate_dir, stage, features, providers });
	}
	rows
}

fn valid_library_name(name: &str) -> bool {
	!name.is_empty() && !name.starts_with("lib") && name.len() <= 58 && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn audit_linked_artifact(row: &ManifestRow, bytes: &[u8], libraries: &[String], require_lsrt: bool) {
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
	if require_lsrt {
		assert!(!expected.is_empty(), "{} {} has no providers", row.kind, row.name);
		assert!(expected.binary_search(&String::from("lsrt.lslib")).is_ok(), "{} {} does not directly need lsrt.lslib", row.kind, row.name);
	}

	let image = bootproto::elf::Elf::parse_for_machine(bytes, user_elf_machine()).unwrap_or_else(|| panic!("{} {} is not a valid target ELF", row.kind, row.name));
	assert_eq!(image.image_type, bootproto::elf::ET_DYN, "{} {} is not ET_DYN", row.kind, row.name);
	let dynamic = image.dynamic_info().flatten().unwrap_or_else(|| panic!("{} {} has no valid terminated PT_DYNAMIC", row.kind, row.name));
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

fn audit_dynamic_order(row: &ManifestRow, bytes: &[u8], libraries: &[String]) {
	assert!(!bytes.is_empty() && bytes.len() <= 64 * 65 && bytes.last() == Some(&b'\n'), "dynamic {} has malformed canonical provider order", row.name);
	let text = core::str::from_utf8(bytes).unwrap_or_else(|_| panic!("dynamic {} provider order is not UTF-8", row.name));
	let mut names: Vec<&str> = Vec::new();
	for name in text.lines() {
		let stem = name.strip_suffix(".lslib").unwrap_or_else(|| panic!("dynamic {} order names non-library {name:?}", row.name));
		assert!(valid_library_name(stem) && libraries.binary_search(&String::from(stem)).is_ok(), "dynamic {} order names invalid or unstaged provider {name}", row.name);
		assert!(!names.contains(&name), "dynamic {} repeats provider {name} in canonical order", row.name);
		names.push(name);
	}
	assert!(!names.is_empty() && names.len() <= 64, "dynamic {} has an empty or oversized canonical provider order", row.name);
}

// The debug-build target path of a userspace ELF: each crate builds to its own target dir.
// The target triple follows the kernel's target arch, so an aarch64 kernel stages the
// aarch64 userspace ELFs (and x86_64 the x86_64 ones).
fn user_elf_path(manifest: &Path, crate_dir: &str, name: &str) -> PathBuf {
	manifest.join(format!("../user/{crate_dir}/target/{}/debug/{name}", user_target()))
}

fn user_shared_path(manifest: &Path, crate_dir: &str, name: &str) -> PathBuf {
	let root = if matches!(crate_dir, "proto" | "wire" | "wasm") { manifest.join(format!("../{crate_dir}")) } else { manifest.join(format!("../user/{crate_dir}")) };
	root.join(format!("shared/{}/{}.lslib", user_target(), name))
}

fn user_dynamic_path(manifest: &Path, crate_dir: &str, name: &str) -> PathBuf {
	manifest.join(format!("../user/{crate_dir}/shared/{}/{}", user_target(), name))
}

fn user_dynamic_order_path(manifest: &Path, crate_dir: &str, name: &str) -> PathBuf {
	manifest.join(format!("../user/{crate_dir}/shared/{}/{}.order", user_target(), name))
}

fn identity_path(artifact: &Path) -> PathBuf {
	PathBuf::from(format!("{}.identity", artifact.display()))
}

fn sha256_file(path: &Path) -> String {
	let output = Command::new("sha256sum").arg(path).output().unwrap_or_else(|error| panic!("cannot hash {}: {error}", path.display()));
	assert!(output.status.success(), "sha256sum failed for {}", path.display());
	String::from_utf8(output.stdout).expect("sha256sum output is UTF-8").split_whitespace().next().expect("sha256sum digest").to_string()
}

fn audit_identity(row: &ManifestRow, artifact: &Path, libraries: &[ManifestRow], expected_rustc_commit: &str) -> Vec<u8> {
	let path = identity_path(artifact);
	let bytes = fs::read(&path).unwrap_or_else(|error| panic!("cannot read identity for {} at {}: {error}", row.name, path.display()));
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
			let provider_artifact = user_shared_path(&PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR")), &provider_row.crate_dir, provider);
			format!("provider={provider}:{}", sha256_file(&identity_path(&provider_artifact)))
		})
		.collect();
	expected_providers.sort();
	assert_eq!(&lines[10..], expected_providers.as_slice(), "{} identity provider chain", row.name);

	let digest = sha256_file(&path);
	let note_path = env::temp_dir().join(format!("liber-identity-note-{}-{}", std::process::id(), row.name));
	let status = Command::new("llvm-objcopy").arg("--dump-section").arg(format!(".note.liber.identity={}", note_path.display())).arg(artifact).status().unwrap_or_else(|error| panic!("cannot read identity note from {}: {error}", artifact.display()));
	assert!(status.success(), "{} has no readable identity note", row.name);
	let note = fs::read(&note_path).unwrap_or_else(|error| panic!("cannot read {}: {error}", note_path.display()));
	let _ = fs::remove_file(&note_path);
	assert!(note.len() == 52 && &note[..20] == b"\x06\0\0\0\x20\0\0\0\x01\0\0\0LIBER\0\0\0", "{} has malformed identity note", row.name);
	let note_digest: String = note[20..].iter().map(|byte| format!("{byte:02x}")).collect();
	assert_eq!(note_digest, digest, "{} identity note differs from its record", row.name);
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

// Where the assembled packages are written. On aarch64 and riscv64 there is no
// bootloader module hand-off (the kernel is booted directly via `-kernel`), so the
// packages go to OUT_DIR and are embedded into the kernel image; on x86_64 they go to
// boot/.build for mkimage.sh to place as boot modules (the loader loads them alongside
// the kernel and hands their addresses to it in the BootInfo).
fn package_out_dir(manifest: &Path) -> PathBuf {
	match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
		Ok("aarch64") | Ok("riscv64") => PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set")),
		_ => manifest.join("../boot/.build"),
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
// and its demo client. It is written to boot/.build/init.pkg, where mkimage.sh
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
			sources.push((executable_artifact_name(&row.name), user_elf_path(&manifest, &row.crate_dir, &row.name)));
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
	fs::write(&out_pkg, &package).unwrap_or_else(|e: std::io::Error| panic!("cannot write {}: {e}", out_pkg.display()));
}

// Assemble the ramdisk volume package: every regular file under src/volume is
// packed into boot/.build/volume.pkg using the same archive format as the init
// package, keyed by its file name. The kernel loads it as a second boot module
// and serves its files through the userspace StorageService over vol://.
fn assemble_volume_package(conf: &[(String, String)]) {
	let manifest_dir: String = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
	let manifest: PathBuf = PathBuf::from(&manifest_dir);
	let vol_dir: PathBuf = manifest.join("../volume");
	let out_dir: PathBuf = package_out_dir(&manifest);
	let out_pkg: PathBuf = out_dir.join(conf_get(conf, "VOLUME_PACKAGE"));

	println!("cargo:rerun-if-changed={}", vol_dir.display());
	fs::create_dir_all(&out_dir).unwrap_or_else(|e: std::io::Error| panic!("cannot create {}: {e}", out_dir.display()));

	// Collect (name, bytes) for every regular file, sorted by name for a stable
	// archive layout. A missing or empty directory yields an empty package.
	let mut files: Vec<(String, Vec<u8>)> = Vec::new();
	if let Ok(read_dir) = fs::read_dir(&vol_dir) {
		for entry in read_dir.flatten() {
			let path: PathBuf = entry.path();
			if !path.is_file() {
				continue;
			}
			let name: String = match path.file_name().and_then(|n| n.to_str()) {
				Some(n) => n.to_string(),
				None => continue,
			};
			let bytes: Vec<u8> = fs::read(&path).unwrap_or_else(|e: std::io::Error| panic!("cannot read {}: {e}", path.display()));
			println!("cargo:rerun-if-changed={}", path.display());
			files.push((name, bytes));
		}
	} else {
		println!("cargo:warning=volume directory not found at {} - writing an empty volume package", vol_dir.display());
	}

	let rows = read_manifest(&manifest);
	let library_rows: Vec<ManifestRow> = rows.iter().filter(|row| row.kind == "library" && row.stage == "volume").cloned().collect();
	let lsrt_row = library_rows.iter().find(|row| row.name == "lsrt").expect("lsrt library row");
	let lsrt_artifact = user_shared_path(&manifest, &lsrt_row.crate_dir, &lsrt_row.name);
	let lsrt_identity = fs::read_to_string(identity_path(&lsrt_artifact)).expect("lsrt identity record");
	let expected_rustc_commit = lsrt_identity.lines().find_map(|line| line.strip_prefix("rustc-commit=")).expect("lsrt rustc identity").to_string();
	assert!(expected_rustc_commit.len() == 40 && expected_rustc_commit.bytes().all(|byte| byte.is_ascii_hexdigit()), "lsrt rustc identity is malformed");
	let mut libraries: Vec<String> = rows.iter().filter(|row| row.kind == "library" && row.stage == "volume").map(|row| row.name.clone()).collect();
	libraries.sort();
	assert!(!libraries.windows(2).any(|pair| pair[0] == pair[1]), "duplicate staged library identity");

	// Also stage the tool and non-bootstrap driver ELFs onto the system volume
	// under bin/ and drivers/, so they can later be loaded from there. They are stripped
	// of symbol/debug sections (the on-disk copies are executed by the loader, which needs
	// only the program image), keeping the seed archive to a few megabytes. A missing or
	// unstrippable ELF is skipped.
	for row in rows {
		if row.kind == "library" {
			let features = row.features.as_deref().expect("library feature set");
			assert!(features == "-" || features.split(',').all(valid_library_name), "library {} has invalid feature set {features:?}", row.name);
		}
		let dest: String = match row.kind.as_str() {
			"tool" => format!("bin/{}", executable_artifact_name(&row.name)),
			"service" | "component" if row.stage == "volume" => format!("bin/{}", executable_artifact_name(&row.name)),
			"driver" if row.stage == "volume" => format!("drivers/{}", executable_artifact_name(&row.name)),
			"library" if row.stage == "volume" => format!("lib/{}.lslib", row.name),
			"dynamic" if row.stage == "volume" => format!("bin/{}", executable_artifact_name(&row.name)),
			_ => continue,
		};
		let path: PathBuf = match row.kind.as_str() {
			"library" => user_shared_path(&manifest, &row.crate_dir, &row.name),
			"dynamic" => user_dynamic_path(&manifest, &row.crate_dir, &row.name),
			_ => user_elf_path(&manifest, &row.crate_dir, &row.name),
		};
		println!("cargo:rerun-if-changed={}", path.display());
		if row.kind == "dynamic" || row.kind == "library" {
			let identity = audit_identity(&row, &path, &library_rows, &expected_rustc_commit);
			let identity_kind = if row.kind == "library" { "lib" } else { "bin" };
			files.push((format!("identity/{identity_kind}/{}", row.name), identity));
		}
		// Strip the ELF to its loadable image; fall back to the raw ELF when no
		// `strip` supports the target (the host binutils cannot strip aarch64), so
		// the program is still staged - the loader ignores the extra sections.
		match read_stripped(&path).or_else(|| fs::read(&path).ok()) {
			Some(bytes) => {
				if row.kind == "dynamic" {
					audit_linked_artifact(&row, &bytes, &libraries, true);
					let order_path = user_dynamic_order_path(&manifest, &row.crate_dir, &row.name);
					println!("cargo:rerun-if-changed={}", order_path.display());
					let order = fs::read(&order_path).unwrap_or_else(|error| panic!("cannot read canonical order for dynamic {} at {}: {error}", row.name, order_path.display()));
					audit_dynamic_order(&row, &order, &libraries);
					files.push((format!("order/{}", row.name), order));
				} else if row.kind == "library" {
					audit_linked_artifact(&row, &bytes, &libraries, row.name != "lsrt");
				}
				files.push((dest, bytes));
			}
			None => println!("cargo:warning={} ELF not found at {} - omitting from system volume (run `just user` or `just build`)", row.name, path.display()),
		}
	}
	files.sort_by(|a, b| a.0.cmp(&b.0));

	let entries: Vec<(&str, Vec<u8>)> = files.iter().map(|(name, data): &(String, Vec<u8>)| (name.as_str(), data.clone())).collect();
	let package: Vec<u8> = build_package(&entries);
	fs::write(&out_pkg, &package).unwrap_or_else(|e: std::io::Error| panic!("cannot write {}: {e}", out_pkg.display()));
}

// Serialize a boot package: an 8-byte magic, a u32 entry count and a reserved
// u32, then one 40-byte entry per file (a 32-byte NUL-padded name, a u32 absolute
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
