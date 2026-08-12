#[allow(dead_code)]
#[path = "../../build.rs"]
mod common;

use std::env;
use std::fs;
use std::path::PathBuf;
use system_manifest::{DriverLifecycle, Manifest, MatchPriority, Restart, Stage};

fn main() {
	common::configure();
	let crate_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));
	let workspace = crate_root.join("../../..");
	println!("cargo:rerun-if-changed={}", workspace.join("user/services/manifest.toml").display());
	let manifest = Manifest::load_workspace(&workspace).unwrap_or_else(|error| panic!("{error}"));
	generate_services(&manifest);
	generate_library_paths(&manifest);
	generate_program_paths(&manifest);
	generate_driver_registry(&manifest);
}

// The driver registry, from the manifest, as a table DeviceManager walks instead of a `match`.
//
// It replaces `driver_for(&DeviceInfo)` - seven arms of virtio type numbers and one hardcoded PCI
// address - which is the milestone's third item and the completion gate's first clause. The table
// carries what a name could not: the lifecycle class, how specific the match is, and the rules
// themselves, which is what everything attached to a binding downstream needs.
//
// Generated rather than written because the manifest already knows every driver, and a second
// hand-maintained list is exactly what this milestone exists to delete. `system-manifest` refuses
// the ambiguous and impossible cases before this runs, so the emitted table needs no defence.
fn generate_driver_registry(manifest: &Manifest) {
	// A DEVELOPMENT-ONLY DRIVER IS NOT IN A SHIPPING REGISTRY, which is what the `#[cfg(feature =
	// "development")]` around the old `dev_channel` arm did and what a generated table would
	// otherwise lose. Without this a shipping image would match the second virtio-console at its
	// pinned address to a driver the image does not contain - a device left unbound by a rule that
	// should not be there rather than by hardware that is not there.
	let development = env::var_os("CARGO_FEATURE_DEVELOPMENT").is_some();
	let mut entries = String::new();
	let mut count = 0usize;
	for program in manifest.programs.values() {
		let Some(driver) = &program.driver else { continue };
		if program.development && !development {
			continue;
		}
		let lifecycle = match driver.lifecycle {
			DriverLifecycle::BootCritical => "Lifecycle::BootCritical",
			DriverLifecycle::Controller => "Lifecycle::Controller",
			DriverLifecycle::Function => "Lifecycle::Function",
			DriverLifecycle::Interface => "Lifecycle::Interface",
		};
		let priority = match driver.priority {
			MatchPriority::Generic => "Priority::Generic",
			MatchPriority::Exact => "Priority::Exact",
			MatchPriority::Quirk => "Priority::Quirk",
		};
		let rules = driver
			.rules
			.iter()
			.map(|rule| {
				let address = match rule.pci_address {
					Some(address) => format!("Some(Address {{ bus: {}, dev: {}, func: {} }})", address.bus, address.dev, address.func),
					None => String::from("None"),
				};
				format!("Rule {{ device_type: {}, pci_class: {}, pci_subclass: {}, pci_interface: {}, pci_address: {address} }}", option32(rule.device_type), option(rule.pci_class), option(rule.pci_subclass), option(rule.pci_interface),)
			})
			.collect::<Vec<_>>()
			.join(", ");
		entries.push_str(&format!(
			"\tEntry {{ name: b\"{}\", artifact: b\"{}\", lifecycle: {lifecycle}, priority: {priority}, rules: &[{rules}] }},\n",
			program.name,
			// The staged file name. A pinned driver is looked up in `init.pkg` by this rather than
			// by its program name, and deriving one from the other is how the two come to disagree.
			program.destination.as_str().rsplit('/').next().unwrap_or(program.destination.as_str()),
		));
		count += 1;
	}
	let generated = format!("// @generated from services/manifest.toml by build.rs - do not edit.\nconst DRIVER_REGISTRY: [Entry; {count}] = [\n{entries}];\n");
	write_generated("driver_registry.rs", &generated);

	// The names alone, for ServiceManager's status view.
	//
	// It held a `[(&'static [u8], bool); 6]` literal - six driver names written in Rust, with an
	// arity that had to be edited to add a seventh. Two of the eight drivers in the image were
	// missing from it and nothing could say so, because the list and the image had no common
	// source. They have one now, and it is the same one DeviceManager binds from.
	let names = registry_names(manifest, development).join(", ");
	let name_count = registry_names(manifest, development).len();
	write_generated("driver_names.rs", &format!("// @generated from services/manifest.toml by build.rs - do not edit.\nconst DRIVER_NAMES: [&[u8]; {name_count}] = [{names}];\n"));
}

fn registry_names(manifest: &Manifest, development: bool) -> Vec<String> {
	manifest
		.programs
		.values()
		.filter(|program| program.driver.is_some() && (development || !program.development))
		// The `driver.` prefix is the status view's naming convention - `lssvc` filters on it - so
		// the generated list carries it rather than every consumer re-adding it.
		.map(|program| format!("b\"driver.{}\"", program.name))
		.collect()
}

fn option(value: Option<u8>) -> String {
	match value {
		Some(value) => format!("Some({value})"),
		None => String::from("None"),
	}
}

fn option32(value: Option<u32>) -> String {
	match value {
		Some(value) => format!("Some({value})"),
		None => String::from("None"),
	}
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

fn generate_program_paths(manifest: &Manifest) {
	let mut arms = String::new();
	for program in manifest.programs.values().filter(|program| program.stage == Stage::Volume) {
		arms.push_str(&format!("\t\"{}\" => Some(\"vol://system/{}\"),\n", program.name, program.destination.as_str()));
	}
	let mut factory_arms = String::new();
	for factory_file in manifest.factory_files.values() {
		factory_arms.push_str(&format!("\t\"{}\" => Some(\"vol://system/{}\"),\n", factory_file.name, factory_file.destination.as_str()));
	}
	let mut runtime_arms = String::new();
	for runtime_path in manifest.runtime_paths.values() {
		runtime_arms.push_str(&format!("\t\"{}\" => Some(\"vol://system/{}\"),\n", runtime_path.name, runtime_path.destination.as_str()));
	}
	let generated = format!("// @generated from services/manifest.toml by build.rs - do not edit.\n#[allow(dead_code)]\nfn program_path(name: &str) -> Option<&'static str> {{\n\tmatch name {{\n{arms}\t\t_ => None,\n\t}}\n}}\n\n#[allow(dead_code)]\nfn factory_path(name: &str) -> Option<&'static str> {{\n\tmatch name {{\n{factory_arms}\t\t_ => None,\n\t}}\n}}\n\n#[allow(dead_code)]\nfn runtime_path(name: &str) -> Option<&'static str> {{\n\tmatch name {{\n{runtime_arms}\t\t_ => None,\n\t}}\n}}\n");
	write_generated("program_paths.rs", &generated);
}

fn write_generated(name: &str, contents: &str) {
	let destination = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set")).join(name);
	fs::write(&destination, contents).unwrap_or_else(|error| panic!("cannot write {}: {error}", destination.display()));
}
