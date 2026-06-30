// build.rs - selects the linker script by target arch and exposes the product
// metadata from product.conf (the single source of truth) to the kernel as
// compile-time environment variables.

use std::env;
use std::fs;
use std::path::PathBuf;

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

	// (package entry name, ELF path). The services and storage crates each produce
	// several binaries.
	let sources: [(&str, PathBuf); 59] = [("system_manager", manifest.join("../user/system_manager/target/x86_64-unknown-none/debug/system_manager")), ("service_manager", manifest.join("../user/services/target/x86_64-unknown-none/debug/service_manager")), ("log_service", manifest.join("../user/services/target/x86_64-unknown-none/debug/log_service")), ("device_manager", manifest.join("../user/services/target/x86_64-unknown-none/debug/device_manager")), ("device_service", manifest.join("../user/services/target/x86_64-unknown-none/debug/device_service")), ("process_service", manifest.join("../user/services/target/x86_64-unknown-none/debug/process_service")), ("config_service", manifest.join("../user/services/target/x86_64-unknown-none/debug/config_service")), ("network_service", manifest.join("../user/services/target/x86_64-unknown-none/debug/network_service")), ("time_service", manifest.join("../user/services/target/x86_64-unknown-none/debug/time_service")), ("console_service", manifest.join("../user/services/target/x86_64-unknown-none/debug/console_service")), ("audio_service", manifest.join("../user/services/target/x86_64-unknown-none/debug/audio_service")), ("input_service", manifest.join("../user/services/target/x86_64-unknown-none/debug/input_service")), ("system_graph_service", manifest.join("../user/services/target/x86_64-unknown-none/debug/system_graph_service")), ("permission_manager", manifest.join("../user/services/target/x86_64-unknown-none/debug/permission_manager")), ("resource_manager", manifest.join("../user/services/target/x86_64-unknown-none/debug/resource_manager")), ("watchdog_probe", manifest.join("../user/services/target/x86_64-unknown-none/debug/watchdog_probe")), ("sandbox_probe", manifest.join("../user/services/target/x86_64-unknown-none/debug/sandbox_probe")), ("date", manifest.join("../user/tools/target/x86_64-unknown-none/debug/date")), ("cat", manifest.join("../user/tools/target/x86_64-unknown-none/debug/cat")), ("write", manifest.join("../user/tools/target/x86_64-unknown-none/debug/write")), ("rm", manifest.join("../user/tools/target/x86_64-unknown-none/debug/rm")), ("ls", manifest.join("../user/tools/target/x86_64-unknown-none/debug/ls")), ("mkdir", manifest.join("../user/tools/target/x86_64-unknown-none/debug/mkdir")), ("rmdir", manifest.join("../user/tools/target/x86_64-unknown-none/debug/rmdir")), ("log", manifest.join("../user/tools/target/x86_64-unknown-none/debug/log")), ("snap", manifest.join("../user/tools/target/x86_64-unknown-none/debug/snap")), ("dev", manifest.join("../user/tools/target/x86_64-unknown-none/debug/dev")), ("config", manifest.join("../user/tools/target/x86_64-unknown-none/debug/config")), ("set", manifest.join("../user/tools/target/x86_64-unknown-none/debug/set")), ("beep", manifest.join("../user/tools/target/x86_64-unknown-none/debug/beep")), ("usage", manifest.join("../user/tools/target/x86_64-unknown-none/debug/usage")), ("ps", manifest.join("../user/tools/target/x86_64-unknown-none/debug/ps")), ("run", manifest.join("../user/tools/target/x86_64-unknown-none/debug/run")), ("perm", manifest.join("../user/tools/target/x86_64-unknown-none/debug/perm")), ("request_probe", manifest.join("../user/services/target/x86_64-unknown-none/debug/request_probe")), ("resource_probe", manifest.join("../user/services/target/x86_64-unknown-none/debug/resource_probe")), ("wasi_host", manifest.join("../user/services/target/x86_64-unknown-none/debug/wasi_host")), ("component_host", manifest.join("../user/services/target/x86_64-unknown-none/debug/component_host")), ("file_picker", manifest.join("../user/services/target/x86_64-unknown-none/debug/file_picker")), ("shell", manifest.join("../user/services/target/x86_64-unknown-none/debug/shell")), ("storage_service", manifest.join("../user/storage/target/x86_64-unknown-none/debug/storage_service")), ("storage_client", manifest.join("../user/storage/target/x86_64-unknown-none/debug/storage_client")), ("virtio_blk", manifest.join("../user/drivers/target/x86_64-unknown-none/debug/virtio_blk")), ("virtio_net", manifest.join("../user/drivers/target/x86_64-unknown-none/debug/virtio_net")), ("virtio_console", manifest.join("../user/drivers/target/x86_64-unknown-none/debug/virtio_console")), ("virtio_input", manifest.join("../user/drivers/target/x86_64-unknown-none/debug/virtio_input")), ("virtio_gpu", manifest.join("../user/drivers/target/x86_64-unknown-none/debug/virtio_gpu")), ("virtio_snd", manifest.join("../user/drivers/target/x86_64-unknown-none/debug/virtio_snd")), ("echo", manifest.join("../user/tools/target/x86_64-unknown-none/debug/echo")), ("ping", manifest.join("../user/tools/target/x86_64-unknown-none/debug/ping")), ("ip", manifest.join("../user/tools/target/x86_64-unknown-none/debug/ip")), ("nslookup", manifest.join("../user/tools/target/x86_64-unknown-none/debug/nslookup")), ("tcp", manifest.join("../user/tools/target/x86_64-unknown-none/debug/tcp")), ("nc", manifest.join("../user/tools/target/x86_64-unknown-none/debug/nc")), ("arp", manifest.join("../user/tools/target/x86_64-unknown-none/debug/arp")), ("httpd", manifest.join("../user/tools/target/x86_64-unknown-none/debug/httpd")), ("ss", manifest.join("../user/tools/target/x86_64-unknown-none/debug/ss")), ("script", manifest.join("../user/tools/target/x86_64-unknown-none/debug/script")), ("ptyecho", manifest.join("../user/tools/target/x86_64-unknown-none/debug/ptyecho"))];

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
