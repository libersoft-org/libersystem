// build.rs - selects the linker script by target arch and exposes the product
// metadata from product.conf (the single source of truth) to the kernel as
// compile-time environment variables.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
	println!("cargo:rerun-if-env-changed=TEST_TAGS");
	select_linker_script();
	let conf: Vec<(String, String)> = read_product_conf();
	export_product_metadata(&conf);
	// The test path table is generated from the MANIFEST alone - no built artifact is read - so
	// it stays here where the kernel's tests include it. Assembling the packages does read built
	// artifacts, and that is `mkpackages`, a separate step.
	let workspace = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set")).join("..");
	let manifest = system_manifest::Manifest::load_workspace(&workspace).unwrap_or_else(|error| panic!("{error}"));
	generate_test_volume_paths(&manifest);
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

fn generate_test_volume_paths(manifest: &system_manifest::Manifest) {
	let mut library_arms = String::new();
	for library in manifest.libraries.values() {
		library_arms.push_str(&format!("\t\"{}.lslib\" => Some(\"{}\"),\n", library.name, library.destination.as_str()));
	}
	let mut program_arms = String::new();
	for program in manifest.programs.values().filter(|program| program.stage == system_manifest::Stage::Volume && included(program)) {
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
	for destination in manifest.libraries.values().map(|library| library.destination.as_str()).chain(manifest.programs.values().filter(|program| program.stage == system_manifest::Stage::Volume && included(program)).map(|program| program.destination.as_str())).chain(manifest.factory_files.values().map(|file| file.destination.as_str())) {
		declared_arms.push_str(&format!("\t\"{destination}\" => true,\n"));
	}
	let source = format!("// @generated from user/services/manifest.toml by build.rs - do not edit.\nfn test_library_path(name: &str) -> Option<&'static str> {{\n\tmatch name {{\n{library_arms}\t\t_ => None,\n\t}}\n}}\n\nfn test_program_path(name: &str) -> Option<&'static str> {{\n\tmatch name {{\n{program_arms}\t\t_ => None,\n\t}}\n}}\n\nfn test_factory_path(name: &str) -> Option<&'static str> {{\n\tmatch name {{\n{factory_arms}\t\t_ => None,\n\t}}\n}}\n\nfn test_runtime_path(name: &str) -> Option<&'static str> {{\n\tmatch name {{\n{runtime_arms}\t\t_ => None,\n\t}}\n}}\n\nfn test_volume_path_is_declared(path: &str) -> bool {{\n\tmatch path {{\n{declared_arms}\t\t_ => false,\n\t}}\n}}\n");
	let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
	fs::write(out_dir.join("library_paths.rs"), source).expect("write test library paths");
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
	println!("cargo:rerun-if-env-changed=LIBER_DEVELOPMENT");
	env::var("LIBER_DEVELOPMENT").as_deref() == Ok("1")
}
