#[allow(dead_code)]
#[path = "../../build.rs"]
mod common;

use std::env;
use std::fs;
use std::path::PathBuf;
use system_manifest::{Manifest, Restart, Stage};

fn main() {
	common::configure();
	let crate_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));
	let workspace = crate_root.join("../../..");
	let manifest = Manifest::load_workspace(&workspace).unwrap_or_else(|error| panic!("{error}"));
	generate_services(&manifest);
	generate_library_paths(&manifest);
}

fn generate_services(manifest: &Manifest) {
	let mut entries = String::new();
	for service in manifest.services.values() {
		let program = manifest.programs.get(&service.program).expect("validated service program");
		let restart = match service.restart {
			Restart::Transparent => "Restart::Transparent",
			Restart::Escalate => "Restart::Escalate",
		};
		let dependencies = service.dependencies.iter().map(|dependency| format!("b\"{dependency}\" as &'static [u8]")).collect::<Vec<_>>().join(", ");
		entries.push_str(&format!("\tService {{ name: b\"{}\", program: b\"{}\", pinned: {}, restart: {restart}, deps: &[{dependencies}] }},\n", service.name, service.program, program.stage == Stage::Pinned));
	}
	let generated = format!("// @generated from services/manifest.toml by build.rs - do not edit.\nconst N: usize = {};\nconst MANIFEST: [Service; N] = [\n{entries}];\n", manifest.services.len());
	write_generated("manifest.rs", &generated);
}

fn generate_library_paths(manifest: &Manifest) {
	let mut arms = String::new();
	for library in manifest.libraries.values() {
		arms.push_str(&format!("\t\"{}.lslib\" => Some(\"vol://system/{}\"),\n", library.name, library.destination.as_str()));
	}
	let generated = format!("// @generated from services/manifest.toml by build.rs - do not edit.\nfn library_path(name: &str) -> Option<&'static str> {{\n\tmatch name {{\n{arms}\t\t_ => None,\n\t}}\n}}\n");
	write_generated("library_paths.rs", &generated);
}

fn write_generated(name: &str, contents: &str) {
	let destination = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set")).join(name);
	fs::write(&destination, contents).unwrap_or_else(|error| panic!("cannot write {}: {error}", destination.display()));
}
