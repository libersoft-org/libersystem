// build.rs - selects the linker script by target arch and exposes the product
// metadata from product.conf (the single source of truth) to the kernel as
// compile-time environment variables.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
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

// On aarch64, embed the pre-built `echo` userspace ELF so the kernel can load and
// run a real rt-based program as an end-to-end bring-up demo. Written to OUT_DIR
// (empty when the ELF is absent, e.g. a bare `cargo build` without the userspace
// built first), so the kernel always builds and the demo simply does not run. On
// other targets the embedded blob is empty (they route userspace via the init
// package instead).
fn embed_aarch64_demo() {
	let out_dir: PathBuf = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
	let dest: PathBuf = out_dir.join("echo_demo.elf");
	let arch: String = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
	let bytes: Vec<u8> = if arch == "aarch64" {
		let manifest_dir: String = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
		let elf: PathBuf = PathBuf::from(&manifest_dir).join("../user/tools/target/aarch64-unknown-none/debug/echo");
		println!("cargo:rerun-if-changed={}", elf.display());
		fs::read(&elf).unwrap_or_default()
	} else {
		Vec::new()
	};
	fs::write(&dest, &bytes).unwrap_or_else(|e: std::io::Error| panic!("cannot write {}: {e}", dest.display()));
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
struct ManifestRow {
	kind: String,
	name: String,
	crate_dir: String,
	stage: String,
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
		rows.push(ManifestRow { kind, name, crate_dir, stage });
	}
	rows
}

// The debug-build target path of a userspace ELF: each crate builds to its own target dir.
fn user_elf_path(manifest: &Path, crate_dir: &str, name: &str) -> PathBuf {
	manifest.join(format!("../user/{crate_dir}/target/x86_64-unknown-none/debug/{name}"))
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
	let stripped: Option<Vec<u8>> = match Command::new("strip").arg("-s").arg(&tmp).status() {
		Ok(status) if status.success() => fs::read(&tmp).ok(),
		_ => {
			println!("cargo:warning=strip unavailable - omitting {} from the system volume", path.display());
			None
		}
	};
	let _ = fs::remove_file(&tmp);
	stripped
}

// Assemble the init package that the kernel loads as a Limine module. The package
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
	let out_dir: PathBuf = manifest.join("../boot/.build");
	let out_pkg: PathBuf = out_dir.join(conf_get(conf, "INIT_PACKAGE"));

	// (package entry name, ELF path). The init package holds only the pinned bootstrap set
	// (M61 box 8): the pinned services and the bootstrap block driver. Every other service,
	// manager, driver and tool is loaded from the system volume, so it is staged there by
	// assemble_volume_package instead. A pinned row with a real crate (not an `instance`
	// backed by another program) contributes its ELF.
	let mut sources: Vec<(String, PathBuf)> = Vec::new();
	for row in read_manifest(&manifest) {
		if row.stage == "pinned" && row.crate_dir != "-" {
			sources.push((row.name.clone(), user_elf_path(&manifest, &row.crate_dir, &row.name)));
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
// package, keyed by its file name. The kernel loads it as a second Limine module
// and serves its files through the userspace StorageService over vol://.
fn assemble_volume_package(conf: &[(String, String)]) {
	let manifest_dir: String = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
	let manifest: PathBuf = PathBuf::from(&manifest_dir);
	let vol_dir: PathBuf = manifest.join("../volume");
	let out_dir: PathBuf = manifest.join("../boot/.build");
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

	// M61 box 7: also stage the tool and non-bootstrap driver ELFs onto the system volume
	// under bin/ and drivers/, so they can later be loaded from there. They are stripped
	// of symbol/debug sections (the on-disk copies are executed by the loader, which needs
	// only the program image), keeping the seed archive to a few megabytes. A missing or
	// unstrippable ELF is skipped.
	for row in read_manifest(&manifest) {
		let dest: String = match row.kind.as_str() {
			"tool" => format!("bin/{}", row.name),
			"service" | "component" if row.stage == "volume" => format!("bin/{}", row.name),
			"driver" if row.stage == "volume" => format!("drivers/{}", row.name),
			_ => continue,
		};
		let path: PathBuf = user_elf_path(&manifest, &row.crate_dir, &row.name);
		println!("cargo:rerun-if-changed={}", path.display());
		match read_stripped(&path) {
			Some(bytes) => files.push((dest, bytes)),
			None => println!("cargo:warning={} ELF not found at {} - omitting from system volume (run `just user` or `just build`)", row.name, path.display()),
		}
	}
	files.sort_by(|a, b| a.0.cmp(&b.0));

	let entries: Vec<(&str, Vec<u8>)> = files.iter().map(|(name, data): &(String, Vec<u8>)| (name.as_str(), data.clone())).collect();
	let package: Vec<u8> = build_package(&entries);
	fs::write(&out_pkg, &package).unwrap_or_else(|e: std::io::Error| panic!("cannot write {}: {e}", out_pkg.display()));
}

// Serialize the init package: an 8-byte magic, a u32 entry count and a reserved
// u32, then one 32-byte entry per file (a 24-byte NUL-padded name, a u32 absolute
// byte offset and a u32 size), then the concatenated file blobs. All integers are
// little-endian. Must match the parser in src/kernel/pkg.rs.
fn build_package(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
	use abi::{PKG_ENTRY_LEN as ENTRY_LEN, PKG_HEADER_LEN as HEADER_LEN, PKG_NAME_LEN as NAME_LEN};

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
		let copy: usize = name_bytes.len().min(NAME_LEN);
		name_field[..copy].copy_from_slice(&name_bytes[..copy]);
		out.extend_from_slice(&name_field);
		out.extend_from_slice(&(blob_offset as u32).to_le_bytes());
		out.extend_from_slice(&(data.len() as u32).to_le_bytes());
		blob_offset += data.len();
		blobs.extend_from_slice(data);
	}
	out.extend_from_slice(&blobs);
	out
}
