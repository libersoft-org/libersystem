#[path = "../../build.rs"]
mod common;

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::PathBuf;
use system_manifest::{DriverLifecycle, Manifest, MatchPriority, Presence, Restart, RoleKind, Stage};

fn main() {
	common::main();
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
		// THE ANSWER, NOT THE VOCABULARY. Selection asks whether a driver is needed to mount the
		// volume and how specific its match is; it never names a lifecycle or a priority. Emitting
		// the enums instead put four lifecycle names and three priority names in every image, of
		// which a shipping manifest constructs two and one.
		let boot_critical = matches!(driver.lifecycle, DriverLifecycle::BootCritical);
		let priority = match driver.priority {
			MatchPriority::Generic => "0 /* generic */",
			MatchPriority::Exact => "1 /* exact */",
			MatchPriority::Quirk => "2 /* quirk */",
		};
		let rules = driver
			.rules
			.iter()
			.map(|rule| {
				let address = match rule.pci_address {
					Some(address) => format!("Some(Address {{ bus: {}, dev: {}, func: {} }})", address.bus, address.dev, address.func),
					None => String::from("None"),
				};
				format!("Rule {{ transport: {}, virtio_type: {}, pci_class: {}, pci_subclass: {}, pci_interface: {}, pci_vendor: {}, pci_product: {}, pci_address: {address} }}", option(rule.transport), option32(rule.virtio_type), option(rule.pci_class), option(rule.pci_subclass), option(rule.pci_interface), option16(rule.pci_vendor), option16(rule.pci_product),)
			})
			.collect::<Vec<_>>()
			.join(", ");
		let requires = driver.requires.iter().map(|kind| kind.wire().to_string()).collect::<Vec<_>>().join(", ");
		let provides = driver.provides.iter().map(|entry| format!("({}, {}, {})", entry.kind.wire(), entry.most, entry.consumers)).collect::<Vec<_>>().join(", ");
		let heartbeat = option32(driver.heartbeat_deadline);
		entries.push_str(&format!(
			"\tEntry {{ name: b\"{}\", artifact: b\"{}\", boot_critical: {boot_critical}, priority: {priority}, requires: &[{requires}], provides: &[{provides}], heartbeat_deadline: {heartbeat}, rules: &[{rules}] }},\n",
			program.name,
			// The staged file name. A pinned driver is looked up in `init.pkg` by this rather than
			// by its program name, and deriving one from the other is how the two come to disagree.
			program.destination.as_str().rsplit('/').next().unwrap_or(program.destination.as_str()),
		));
		count += 1;
	}
	// HOW MANY PROVIDERS THIS IMAGE CAN EVER HOLD, ADDED UP FROM WHAT ITS DRIVERS DECLARE.
	//
	// The catalogue was `[Option<Provider>; 32]` - a number chosen in DeviceManager, unrelated to the
	// registry, and a valid publication past it was CLOSED. So an image whose drivers declare more
	// than that silently loses the last of them, and one that declares far fewer carries a table it
	// can never fill. The registry already states the bound per driver (`provides` is a kind and at
	// most how many), and the sum of those bounds is the only number that can be right: nothing in
	// the image can publish more, and nothing in this file decides how many a machine may have.
	//
	// A CEILING OF ONE where an image declares none, because a zero-length array is a catalogue that
	// cannot hold the publication a driver added to the manifest after this was generated - the
	// refusal would then be the array's shape rather than the declaration it is supposed to enforce.
	// The SAME filter the entries above are emitted under: a development-only driver is not in a
	// shipping registry, so its declarations are not part of a shipping image's bound either.
	let declared: usize = manifest.programs.values().filter(|program| development || !program.development).filter_map(|program| program.driver.as_ref()).map(|driver| driver.provides.iter().map(|entry| entry.most as usize).sum::<usize>()).sum();
	let generated = format!("// @generated from services/manifest.toml by build.rs - do not edit.\nconst DRIVER_REGISTRY: [Entry; {count}] = [\n{entries}];\n// The sum of every `provides` bound this image's registry declares. See build.rs.\nconst MAX_PROVIDERS: usize = {};\n", declared.max(1));
	write_generated("driver_registry.rs", &generated);

	// THE SAME NUMBER, FOR THE SUPERVISOR THAT RECEIVES WHAT THE MANAGER PUBLISHES.
	//
	// ServiceManager is its own binary and cannot see DeviceManager's `MAX_PROVIDERS`, so it held a
	// four-element probe array - a count of disks compiled into the receiving side of a hand-off
	// whose SENDING side carries its own count. A machine with a fifth block provider had that
	// provider's probe connection closed, and the loader-selected root volume cannot be found on a
	// disk nobody probes. One computation above, emitted twice.
	write_generated("provider_bound.rs", &format!("// @generated from services/manifest.toml by build.rs - do not edit.\n// The sum of every `provides` bound this image's registry declares. See build.rs.\nconst MOST_PROVIDERS: usize = {};\n", declared.max(1)));

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

fn option16(value: Option<u16>) -> String {
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
	let mut plans = String::new();
	for service in manifest.services.values() {
		let program = manifest.programs.get(&service.program).expect("validated service program");
		let restart = match service.restart {
			Restart::Transparent => "Restart::Transparent",
			Restart::Escalate => "Restart::Escalate",
		};
		let dependencies = service.dependencies.iter().map(|dependency| format!("b\"{dependency}\" as &'static [u8]")).collect::<Vec<_>>().join(", ");
		let roles = service
			.roles
			.iter()
			.map(|role| {
				let kind = match role.kind {
					RoleKind::ServeRoot => "RoleKind::ServeRoot",
					RoleKind::Client => "RoleKind::Client",
					RoleKind::Factory => "RoleKind::Factory",
					RoleKind::Privilege => "RoleKind::Privilege",
					// The validator refuses a power role outright, so reaching here means that check
					// was weakened without this generator being told.
					RoleKind::Power => panic!("services/manifest.toml declares a power role, which the executor has no way to deliver"),
					RoleKind::Package => "RoleKind::Package",
					RoleKind::Device => "RoleKind::Device",
					RoleKind::Payload => "RoleKind::Payload",
				};
				let required = role.presence == Presence::Required;
				format!("Role {{ tag: b\"{}\", kind: {kind}, provider: b\"{}\", source: b\"{}\", required: {required}, exclusive: {}, handed_on: {} }}", role.tag, role.provider, role.source, role.exclusive, role.handed_on)
			})
			.collect::<Vec<_>>()
			.join(", ");
		entries.push_str(&format!("\tService {{ name: b\"{}\", program: b\"{}\", pinned: {}, restart: {restart}, deps: &[{dependencies}] }},\n", service.name, service.program, program.stage == Stage::Pinned));
		plans.push_str(&format!("\t&[{roles}],\n"));
	}
	// THE PLAN IS INDEXED THE SAME WAY THE MANIFEST IS, deliberately: two arrays over one order
	// cannot drift the way two tables keyed by name can, and the supervisor already walks services
	// by index. `ROLES[i]` is what `MANIFEST[i]` must be handed, in the order it must arrive.
	let generated = format!("// @generated from services/manifest.toml by build.rs - do not edit.\nconst N: usize = {};\nconst MANIFEST: [Service; N] = [\n{entries}];\nconst ROLES: [&[Role]; N] = [\n{plans}];\n", manifest.services.len());
	write_generated("manifest.rs", &generated);
	generate_role_tags(manifest);
	generate_receive_plans(manifest);
}

// THE SAME PLAN, WRITTEN FOR THE RECEIVING END.
//
// `manifest.rs` above is what the supervisor SENDS; this is what each service EXPECTS, generated
// from the identical rows so the two cannot disagree. A service that reads its bootstrap by hand
// agrees with the sender only because somebody keeps them agreeing - and when three programs once
// read theirs in an order the sender does not use, a blocking tagged read consumed the message
// that was actually next and then waited forever for one nobody sends. It surfaced 170 tests away.
//
// One file per service rather than one table for all of them: a service includes its own list
// under a fixed name and cannot reach anybody else's, and nothing is compiled into a binary that
// has no use for it.
fn generate_receive_plans(manifest: &Manifest) {
	for service in manifest.services.values() {
		let roles = service
			.roles
			.iter()
			.map(|role| {
				let kind = match role.kind {
					RoleKind::ServeRoot => "ServeRoot",
					RoleKind::Client => "Client",
					RoleKind::Factory => "Factory",
					RoleKind::Privilege => "Privilege",
					RoleKind::Power => panic!("services/manifest.toml declares a power role, which the executor has no way to deliver"),
					RoleKind::Package => "Package",
					RoleKind::Device => "Device",
					RoleKind::Payload => "Payload",
				};
				let required = role.presence == Presence::Required;
				format!("\trt::Role {{ tag: b\"{}\", kind: rt::RoleKind::{kind}, required: {required} }},\n", role.tag)
			})
			.collect::<String>();
		// Fully qualified, because a service that includes this may have types of its own by these
		// names - the supervisor does - and a generated file must not depend on what is in scope
		// where it lands.
		let generated = format!("// @generated from services/manifest.toml by build.rs - do not edit.\nconst BOOTSTRAP_ROLES: [rt::Role; {}] = [\n{roles}];\n", service.roles.len());
		write_generated(&format!("roles_{}.rs", service.name), &generated);
	}
}

// THE ROLE TAGS, AS ONE SET OF CONSTANTS BOTH ENDS READ.
//
// A tag is written twice today: once where the supervisor sends it and once where the service
// reads it, each spelled by hand. That is how three programs came to read their bootstrap in an
// order the sender does not use - a blocking tagged read consumes the message that was actually
// next and then waits for one nobody will send, and it took a bisect over 170 tests to find,
// because the failure surfaced in an unrelated service. Two hand-written spellings of one name is
// the shape of that defect; one generated constant is the shape that cannot have it.
fn generate_role_tags(manifest: &Manifest) {
	let mut tags: BTreeSet<&str> = BTreeSet::new();
	for service in manifest.services.values() {
		for role in &service.roles {
			tags.insert(role.tag.as_str());
		}
	}
	let mut out = String::from("// @generated from services/manifest.toml by build.rs - do not edit.\n");
	for tag in &tags {
		out.push_str(&format!("pub const ROLE_{tag}: &[u8] = b\"{tag}\";\n"));
	}
	out.push_str(&format!("pub const ROLE_TAGS: [&[u8]; {}] = [{}];\n", tags.len(), tags.iter().map(|tag| format!("ROLE_{tag}")).collect::<Vec<_>>().join(", ")));
	write_generated("role_tags.rs", &out);
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
	// ONE FILE PER LOOKUP, because a consumer includes what it asks and nothing else. As one file
	// holding all three, every consumer got two functions it does not call - which is dead code in
	// nine binaries, reported nine times, and nothing a reader could do about it.
	let header = "// @generated from services/manifest.toml by build.rs - do not edit.\n";
	write_generated("program_path.rs", &format!("{header}fn program_path(name: &str) -> Option<&'static str> {{\n\tmatch name {{\n{arms}\t\t_ => None,\n\t}}\n}}\n"));
	write_generated("factory_path.rs", &format!("{header}fn factory_path(name: &str) -> Option<&'static str> {{\n\tmatch name {{\n{factory_arms}\t\t_ => None,\n\t}}\n}}\n"));
	write_generated("runtime_path.rs", &format!("{header}fn runtime_path(name: &str) -> Option<&'static str> {{\n\tmatch name {{\n{runtime_arms}\t\t_ => None,\n\t}}\n}}\n"));
}

fn write_generated(name: &str, contents: &str) {
	let destination = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set")).join(name);
	fs::write(&destination, contents).unwrap_or_else(|error| panic!("cannot write {}: {error}", destination.display()));
}
