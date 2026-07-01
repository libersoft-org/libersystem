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

// The userspace programs staged at boot, the single source of truth for both archives.
// The pinned bootstrap set (M61 box 8): the only programs kept in the init package.
// Most are on the path to mounting the system volume - SystemManager and ServiceManager
// (the launchers), LogService (every service depends on it), DeviceManager and the
// StorageService (which brings the volume up), and ProcessService (which loads everything
// else off the volume); a service on this path cannot itself be loaded from a volume that
// is not mounted yet. The two probes are the exceptions: the supervisor raw-spawns
// watchdog_probe during its self-test after stopping DeviceManager (which drops
// virtio_blk, so the volume is gone), and ResourceManager spawns resource_probe into a
// bounded sub-Domain (which ProcessService cannot do), so both ship in the package.
// (package entry name, crate directory under ../user).
const PINNED_SERVICES: [(&str, &str); 8] = [("system_manager", "system_manager"), ("service_manager", "services"), ("log_service", "services"), ("device_manager", "services"), ("process_service", "services"), ("storage_service", "storage"), ("watchdog_probe", "services"), ("resource_probe", "services")];

// Every other service, manager and demo component: loaded from the system volume's `bin/`
// via ProcessService, so they are staged onto the volume and NOT kept in the init package
// (M61 box 8). (package entry name, crate directory under ../user).
const VOLUME_SERVICES: [(&str, &str); 18] = [("device_service", "services"), ("config_service", "services"), ("network_service", "services"), ("time_service", "services"), ("console_service", "services"), ("audio_service", "services"), ("input_service", "services"), ("system_graph_service", "services"), ("permission_manager", "services"), ("resource_manager", "services"), ("session_service", "services"), ("sandbox_probe", "services"), ("request_probe", "services"), ("wasi_host", "services"), ("component_host", "services"), ("file_picker", "services"), ("shell", "services"), ("storage_client", "storage")];

// The bootstrap block driver - it must ship in the init package because it backs the
// system volume everything else is seeded onto, so it can never live on that volume.
const BOOT_DRIVER_NAMES: [&str; 1] = ["virtio_blk"];

// Non-bootstrap drivers - loaded from the system volume under drivers/ (M61 box 8), so
// staged there and not kept in the init package.
const NONBOOT_DRIVER_NAMES: [&str; 6] = ["virtio_net", "virtio_console", "virtio_input", "virtio_gpu", "virtio_snd", "xhci"];

// Command-line tools - loaded from the system volume under bin/, so staged there and not
// kept in the init package.
const TOOL_NAMES: [&str; 31] = ["date", "cat", "write", "rm", "ls", "mkdir", "rmdir", "log", "snap", "dev", "config", "set", "beep", "usage", "ps", "run", "perm", "stop", "lsvol", "echo", "ping", "ip", "nslookup", "tcp", "nc", "arp", "httpd", "ss", "script", "ptyecho", "readln"];

// The debug-build target path of a userspace ELF: each crate builds to its own target dir.
fn user_elf_path(manifest: &Path, crate_dir: &str, name: &str) -> PathBuf {
	manifest.join(format!("../user/{crate_dir}/target/x86_64-unknown-none/debug/{name}"))
}

// Read a userspace ELF and strip its symbol and debug sections, returning the smaller
// loadable image staged onto the system volume (the on-disk copies are executed by the
// loader, which needs only the program image, while the init package keeps the full debug
// ELFs). Returns None if the ELF is absent (the build still succeeds - the program is
// simply not staged) or if no `strip` tool is available.
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
	// assemble_volume_package instead.
	let mut sources: Vec<(&str, PathBuf)> = Vec::new();
	for (name, crate_dir) in PINNED_SERVICES {
		sources.push((name, user_elf_path(&manifest, crate_dir, name)));
	}
	for name in BOOT_DRIVER_NAMES {
		sources.push((name, user_elf_path(&manifest, "drivers", name)));
	}

	fs::create_dir_all(&out_dir).unwrap_or_else(|e: std::io::Error| panic!("cannot create {}: {e}", out_dir.display()));

	let mut entries: Vec<(&str, Vec<u8>)> = Vec::new();
	for (name, path) in &sources {
		println!("cargo:rerun-if-changed={}", path.display());
		match fs::read(path) {
			Ok(bytes) => entries.push((name, bytes)),
			Err(_) => println!("cargo:warning={name} ELF not found at {} - omitting from init package (run `just user` or `just build`)", path.display()),
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
	// only the program image; the init package keeps the full debug ELFs), keeping the
	// seed archive to a few megabytes. A missing or unstrippable ELF is skipped.
	for name in TOOL_NAMES {
		let path: PathBuf = user_elf_path(&manifest, "tools", name);
		println!("cargo:rerun-if-changed={}", path.display());
		match read_stripped(&path) {
			Some(bytes) => files.push((format!("bin/{name}"), bytes)),
			None => println!("cargo:warning={name} ELF not found at {} - omitting from system volume (run `just user` or `just build`)", path.display()),
		}
	}
	// M61 box 2 / box 8: stage the services, managers and demo components that load from
	// the volume under bin/ too, so ProcessService can start them from there.
	for (name, crate_dir) in VOLUME_SERVICES {
		let path: PathBuf = user_elf_path(&manifest, crate_dir, name);
		println!("cargo:rerun-if-changed={}", path.display());
		match read_stripped(&path) {
			Some(bytes) => files.push((format!("bin/{name}"), bytes)),
			None => println!("cargo:warning={name} ELF not found at {} - omitting from system volume (run `just user` or `just build`)", path.display()),
		}
	}
	for name in NONBOOT_DRIVER_NAMES {
		let path: PathBuf = user_elf_path(&manifest, "drivers", name);
		println!("cargo:rerun-if-changed={}", path.display());
		match read_stripped(&path) {
			Some(bytes) => files.push((format!("drivers/{name}"), bytes)),
			None => println!("cargo:warning={name} ELF not found at {} - omitting from system volume (run `just user` or `just build`)", path.display()),
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
