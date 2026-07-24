// build.rs - link every userspace program at the fixed base its loader expects,
// using the shared linker script in this directory, and expose the product metadata
// from product.conf (the single source of truth) to the userspace crates as
// compile-time environment variables (the shell renders it as the boot banner). One
// shared build script for all the userspace crates. It discovers the shared user
// infrastructure from CARGO_MANIFEST_DIR, so crate nesting does not affect linker or
// product metadata paths.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
	configure();
}

pub fn configure() {
	let user_root: PathBuf = find_user_root();
	select_linker_script(&user_root);
	export_product_metadata(&user_root);
}

fn find_user_root() -> PathBuf {
	let manifest_dir: String = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
	for ancestor in PathBuf::from(manifest_dir).ancestors() {
		if ancestor.join("build.rs").is_file() && ancestor.join("user.ld").is_file() && ancestor.join("rust-toolchain.toml").is_file() {
			return ancestor.to_path_buf();
		}
	}
	panic!("cannot find shared userspace infrastructure");
}

// Link every userspace program at the fixed base its loader expects, using the
// shared linker script for the target arch (the AArch64 script differs only in the
// ELF object format).
fn select_linker_script(user_root: &PathBuf) {
	if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("none") {
		return;
	}
	let arch: String = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
	let script_name: &str = match arch.as_str() {
		"aarch64" => "user-aarch64.ld",
		"riscv64" => "user-riscv64.ld",
		_ => "user.ld",
	};
	let script: PathBuf = user_root.join(script_name);
	println!("cargo:rustc-link-arg=-T{}", script.display());
	println!("cargo:rerun-if-changed={}", script.display());
	println!("cargo:rerun-if-changed={}", user_root.join("build.rs").display());
}

// Parse product.conf (shell-style KEY="value") and re-export every entry as
// a rustc env var so the userspace crates can read it via env!("PRODUCT_NAME"), etc.
// product.conf is the single source of truth, so this keeps the values from being
// duplicated in the source.
fn export_product_metadata(user_root: &PathBuf) {
	let path: PathBuf = user_root.join("../../product.conf");
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
