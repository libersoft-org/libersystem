// build.rs - link every userspace program at the fixed base its loader expects,
// using the shared linker script in this directory, and expose the product metadata
// from product.conf (the single source of truth) to the userspace crates as
// compile-time environment variables (the shell renders it as the boot banner). One
// shared build script for all the userspace crates; each points at it via
// `build = "../build.rs"` so the linker wiring lives in exactly one place. Build
// scripts run with the crate dir as the working directory, so the `../` paths
// resolve into this user/ directory.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
	println!("cargo:rustc-link-arg=-T../user.ld");
	println!("cargo:rerun-if-changed=../user.ld");
	println!("cargo:rerun-if-changed=../build.rs");
	export_product_metadata();
}

// Parse ../../../product.conf (shell-style KEY="value") and re-export every entry as
// a rustc env var so the userspace crates can read it via env!("PRODUCT_NAME"), etc.
// product.conf is the single source of truth, so this keeps the values from being
// duplicated in the source.
fn export_product_metadata() {
	let manifest_dir: String = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
	let path: PathBuf = PathBuf::from(&manifest_dir).join("../../../product.conf");
	let text: String = fs::read_to_string(&path).unwrap_or_else(|e: std::io::Error| panic!("cannot read {}: {e}", path.display()));
	println!("cargo:rerun-if-changed={}", path.display());
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
}
