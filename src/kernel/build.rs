// build.rs - selects the linker script by target arch and exposes the product
// metadata from product.conf (the single source of truth) to the kernel as
// compile-time environment variables.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
	select_linker_script();
	load_product_metadata();
	assemble_init_package();
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

// Parse ../../product.conf (shell-style KEY="value") and re-export each entry as
// a rustc env var so the kernel can read it via env!("PRODUCT_NAME") etc.
fn load_product_metadata() {
	let manifest_dir: String = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
	let path: PathBuf = PathBuf::from(&manifest_dir).join("../../product.conf");
	let text: String = fs::read_to_string(&path).unwrap_or_else(|e: std::io::Error| panic!("cannot read {}: {e}", path.display()));
	for line in text.lines() {
		let trimmed: &str = line.trim();
		if trimmed.is_empty() || trimmed.starts_with('#') {
			continue;
		}
		let Some((key, value)) = trimmed.split_once('=') else {
			continue;
		};
		println!("cargo:rustc-env={}={}", key.trim(), value.trim().trim_matches('"'));
	}
	println!("cargo:rerun-if-changed={}", path.display());
}

// Assemble the init package that the kernel loads as a Limine module. The package
// is a tiny archive (a header plus fixed-size entries plus the concatenated file
// blobs) holding the userspace programs - for now just SystemManager. It is
// written to boot/.build/init.pkg, where mkimage.sh picks it up.
//
// The userspace ELF is built separately (the `just user` recipe, a dependency of
// the build/run/test recipes), so by the time the kernel builds it is present. If
// it is missing - e.g. a bare `cargo build` outside `just`, or rust-analyzer - an
// empty package is written and a warning emitted, so the kernel build still
// succeeds (the kernel handles an absent SystemManager gracefully at runtime).
fn assemble_init_package() {
	let manifest_dir: String = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
	let manifest: PathBuf = PathBuf::from(&manifest_dir);
	let user_elf: PathBuf = manifest.join("../user/system_manager/target/x86_64-unknown-none/debug/system_manager");
	let out_dir: PathBuf = manifest.join("../boot/.build");
	let out_pkg: PathBuf = out_dir.join("init.pkg");

	println!("cargo:rerun-if-changed={}", user_elf.display());
	fs::create_dir_all(&out_dir).unwrap_or_else(|e: std::io::Error| panic!("cannot create {}: {e}", out_dir.display()));

	let entries: Vec<(&str, Vec<u8>)> = match fs::read(&user_elf) {
		Ok(bytes) => vec![("system_manager", bytes)],
		Err(_) => {
			println!("cargo:warning=system_manager ELF not found at {} - writing an empty init package (run `just user` or `just build`)", user_elf.display());
			Vec::new()
		}
	};

	let package: Vec<u8> = build_package(&entries);
	fs::write(&out_pkg, &package).unwrap_or_else(|e: std::io::Error| panic!("cannot write {}: {e}", out_pkg.display()));
}

// Serialize the init package: an 8-byte magic, a u32 entry count and a reserved
// u32, then one 32-byte entry per file (a 24-byte NUL-padded name, a u32 absolute
// byte offset and a u32 size), then the concatenated file blobs. All integers are
// little-endian. Must match the parser in src/kernel/pkg.rs.
fn build_package(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
	const HEADER_LEN: usize = 16;
	const ENTRY_LEN: usize = 32;
	const NAME_LEN: usize = 24;

	let table_len: usize = HEADER_LEN + ENTRY_LEN * entries.len();
	let mut out: Vec<u8> = Vec::new();
	out.extend_from_slice(b"LIBERPK1");
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
