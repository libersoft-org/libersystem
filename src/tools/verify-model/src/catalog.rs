// The check catalog: everything the planner can emit, and the variants each of those has.
//
// It covers EVERY executable planner item without exception - builds, host suites, gates,
// conformance runs, kernel tests and the development checks. Builds are in it for the same reason
// as everything else: `build.kernel / riscv64 / host / test` is something the planner emits, and
// the invariant is that the planner emits nothing outside the catalog.
//
// What a build detects is a compile failure, so its `covers` is the set of components it compiles.
//
// The catalog is also the answer to "which keys exist at all". Tests are compiled per target and
// the counts differ - 205 on x86_64, 196 on aarch64 - so a bound keyed on (test, architecture)
// without this reports the missing pairs as permanently stale and chases combinations that cannot
// exist. Applicability cannot be read off a path either: `cfg(target_arch)` gating is scattered
// through `test_suites/kernel.rs`, `hardware.rs` and `sched/tests.rs`, not confined to the arch
// trees. It has to come from what actually compiles.

use crate::crates::Crate;
use crate::graph::Graph;
use crate::registry::{ARCHITECTURES, Registry};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckKind {
	Build,
	HostSuite,
	Gate,
	Conformance,
	KernelTest,
	DevCheck,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Environment {
	#[serde(rename = "host")]
	Host,
	#[serde(rename = "test-guest")]
	TestGuest,
	#[serde(rename = "dev-guest")]
	DevGuest,
}

impl Environment {
	pub fn as_str(&self) -> &'static str {
		match self {
			Environment::Host => "host",
			Environment::TestGuest => "test-guest",
			Environment::DevGuest => "dev-guest",
		}
	}
}

// One runnable thing. `architecture` is "host" for work that is not per target.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Variant {
	pub architecture: String,
	pub environment: Environment,
	pub configuration: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Check {
	pub id: String,
	pub kind: CheckKind,
	pub covers: Vec<String>,
	pub variants: Vec<Variant>,
	// How the runner invokes it. Kept in the catalog so the plan is executable rather than a
	// description of one.
	pub command: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Catalog {
	pub checks: Vec<Check>,
}

// The parts `./build.sh --part` accepts, and what each of them compiles. A build is per
// architecture and its configuration is the shipping one, because that is the build being checked.
const BUILD_PARTS: [&str; 7] = ["sdk", "libs", "user", "kernel", "loader", "packages", "volume"];

// The image conformance suites, mirroring check.sh's FORMATS. They are host work over external
// tool recipes and do not vary by target.
const CONFORMANCE_FORMATS: [&str; 11] = ["bmp", "gif", "ico", "icns", "jpeg", "pcx", "png", "ppm", "qoi", "tga", "webp"];

// The gates check.sh runs, and the component each one is about. A gate whose subject cannot be
// named belongs to the harness, which every guest run depends on anyway.
//
// This list and check.sh's must agree, and `verify-model check` compares them by reading check.sh
// rather than trusting that they do: a gate added there and not here would never be selected by a
// change to its subject, which is a false green of exactly the kind this milestone exists to close.
const GATES: [(&str, &str); 15] = [
	("development-gate", "harness.tools"),
	("artifact-metadata", "harness.tools"),
	("dynamic-report", "manifest"),
	("test-tags", "kernel"),
	("host-tests", "harness.tools"),
	("static-image", "harness.tools"),
	("undeclared-edge", "manifest"),
	("duplicate-edge", "manifest"),
	("malformed-dynamic", "harness.tools"),
	("malformed-symbol-relocation", "harness.tools"),
	("identity-note", "harness.tools"),
	("volume-layout", "manifest"),
	// Its subject is the documentation rather than any built thing, so it belongs to the harness -
	// which also means every change selects it, and it costs milliseconds.
	("milestone-index", "harness.tools"),
	("verify-model", "verify-model"),
	("verify-model-tests", "verify-model"),
];

// check.sh's gate names, read from the script. Parsing a shell array is crude and correct here:
// the alternative is a second hand-maintained list, which is the thing being prevented.
pub fn gates_declared_in_check_sh(repo_root: &std::path::Path) -> Result<BTreeSet<String>, String> {
	let path = repo_root.join("check.sh");
	let text = std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
	let start = text.find("declare -A GATES=(").ok_or_else(|| format!("{}: no GATES array", path.display()))?;
	let end = text[start..].find("\n)").ok_or_else(|| format!("{}: unterminated GATES array", path.display()))? + start;
	let mut names = BTreeSet::new();
	for line in text[start..end].lines().skip(1) {
		let line = line.trim();
		let Some(rest) = line.strip_prefix("[\"") else { continue };
		let Some(name) = rest.split('"').next() else { continue };
		names.insert(name.to_string());
	}
	if names.is_empty() {
		return Err(format!("{}: parsed no gate names, so the comparison would pass vacuously", path.display()));
	}
	Ok(names)
}

// The universes that can judge `component`: the environments its own checks run in.
//
// Trust was asked of a fixed pair, Host and TestGuest, for every component - which is wrong in both
// directions. A development-only binary is judged by the dev guest and by nothing else; a host tool
// that never enters an image is judged on the host. Asking a universe that cannot reach a component
// for evidence about it produces a certificate that can never be earned, and not asking one that
// can produces a certificate that means less than it says.
pub fn judging_universes(catalog: &Catalog, component: &str) -> Vec<crate::shadow::Universe> {
	let mut seen: BTreeSet<crate::shadow::Universe> = BTreeSet::new();
	for check in &catalog.checks {
		if !check.covers.iter().any(|covered| covered == component) {
			continue;
		}
		for variant in &check.variants {
			seen.insert(match variant.environment {
				Environment::Host => crate::shadow::Universe::Host,
				Environment::TestGuest => crate::shadow::Universe::TestGuest,
				Environment::DevGuest => crate::shadow::Universe::DevGuest,
			});
		}
	}
	seen.into_iter().collect()
}

pub fn catalog_gate_names() -> BTreeSet<String> {
	GATES.iter().map(|(name, _)| (*name).to_string()).collect()
}

impl Catalog {
	pub fn build(crates: &[Crate], registry: &Registry, graph: &Graph, staged: &BTreeSet<String>, kernel_tests: &[KernelTest]) -> Self {
		let mut catalog = Catalog::default();

		// Builds. Every part on every target, in the configuration that ships.
		for part in BUILD_PARTS {
			catalog.checks.push(Check { id: format!("build.{part}"), kind: CheckKind::Build, covers: build_covers(part, crates, staged), variants: ARCHITECTURES.iter().map(|architecture| Variant { architecture: (*architecture).to_string(), environment: Environment::Host, configuration: String::from("shared-image") }).collect(), command: format!("./build.sh --arch {{arch}} --part {part}") });
		}

		// Host suites. One per crate that has a `#[test]`, in every configuration that crate can
		// be built in - which for the sixteen crates declaring `shared-image` is two, because for
		// those the default configuration is the one that never ships.
		for entry in crates {
			if !entry.has_host_tests {
				continue;
			}
			// A crate whose suite cannot be executed on the host gets no key rather than a key that
			// always fails. The debt is recorded in the registry with its cause, and `check` reports
			// it every run, so this is a stated exception rather than a quiet omission.
			if !registry.host_tests_runnable(&entry.name) {
				continue;
			}
			let mut variants = vec![Variant { architecture: String::from("host"), environment: Environment::Host, configuration: String::from("default") }];
			if entry.features.contains("shared-image") && registry.configuration("shared-image").is_some() && configuration_runnable(registry, graph, &entry.name, "shared-image") {
				variants.push(Variant { architecture: String::from("host"), environment: Environment::Host, configuration: String::from("shared-image") });
			}
			catalog.checks.push(Check { id: format!("host.{}", entry.name), kind: CheckKind::HostSuite, covers: vec![entry.name.clone()], variants, command: format!("cargo test --manifest-path {}/Cargo.toml", entry.dir) });
		}

		for (gate, subject) in GATES {
			catalog.checks.push(Check { id: format!("gate.{gate}"), kind: CheckKind::Gate, covers: vec![subject.to_string()], variants: vec![Variant { architecture: String::from("host"), environment: Environment::Host, configuration: String::from("default") }], command: format!("./check.sh --gate {gate}") });
		}

		for format in CONFORMANCE_FORMATS {
			catalog.checks.push(Check { id: format!("conformance.{format}"), kind: CheckKind::Conformance, covers: vec![format.to_string()], variants: vec![Variant { architecture: String::from("host"), environment: Environment::Host, configuration: String::from("default") }], command: format!("./check.sh --conformance {format}") });
		}

		// Kernel tests: derived per target from the compiled test binaries, so the variant list is
		// the truth about where each test exists rather than a guess from its path.
		for test in kernel_tests {
			catalog.checks.push(Check { id: test.id.clone(), kind: CheckKind::KernelTest, covers: test.covers.clone(), variants: test.architectures.iter().map(|architecture| Variant { architecture: architecture.clone(), environment: Environment::TestGuest, configuration: String::from("test") }).collect(), command: String::from("./test.sh --arch {arch}") });
		}

		// The development guest. qemu-run.sh refuses DEV_PROFILE together with TEST, so these can
		// never share a boot with the kernel suite - which is why the environment is part of the
		// key rather than a detail of the runner.
		// What exists ONLY in the development configuration: a `[[bin]]` behind
		// `required-features = ["development"]`, which the shipping build does not enable. Derived
		// rather than listed, because the manifest already states it and `crates.rs` now reads it.
		//
		// `dev_agent` is the reason the dev guest exists, and the design's own worked example -
		// change `dev_agent.rs`, plan a dev-guest run - did not hold, because no dev check claimed
		// to cover it. It does now: `dev-selftest.py` drives publication, refusal and rollback
		// through that agent, so it is exactly the check a regression in it would fail.
		let development_only: Vec<String> = crates.iter().flat_map(|entry| entry.binaries.iter()).filter(|binary| binary.required_features.iter().any(|feature| feature == "development")).map(|binary| crate::graph::binary_component(&binary.name)).collect();
		for (id, script, subject) in [
			("dev.selftest", "boot/dev-selftest.py", "harness.boot"),
			("dev.proto-test", "boot/proto-test.py", "proto"),
			("dev.perf-gate", "boot/perf-gate.py", "harness.boot"),
		] {
			let mut covers = vec![subject.to_string()];
			if id == "dev.selftest" {
				covers.extend(development_only.iter().cloned());
				covers.sort();
				covers.dedup();
			}
			catalog.checks.push(Check { id: id.to_string(), kind: CheckKind::DevCheck, covers, variants: vec![Variant { architecture: String::from("x86_64"), environment: Environment::DevGuest, configuration: String::from("development") }], command: format!("(cd src && {script})") });
		}

		catalog.checks.sort();
		catalog
	}

	pub fn get(&self, id: &str) -> Option<&Check> {
		self.checks.iter().find(|check| check.id == id)
	}

	// Every variant names a configuration the catalog defines, every architecture is real, and no
	// two checks share an ID. The ID rule is the load-bearing one: an ID is what age, shadow, cost
	// and regression history are all keyed on, so two checks sharing one silently merge four
	// separate records.
	pub fn validate(&self, registry: &Registry) -> Result<(), String> {
		let mut errors = Vec::new();
		let mut seen: BTreeSet<&str> = BTreeSet::new();
		for check in &self.checks {
			if !seen.insert(check.id.as_str()) {
				errors.push(format!("duplicate check id '{}'", check.id));
			}
			if check.variants.is_empty() {
				errors.push(format!("check '{}' has no variants, so it can never run", check.id));
			}
			for variant in &check.variants {
				if registry.configuration(&variant.configuration).is_none() {
					errors.push(format!("check '{}' names configuration '{}', which configurations.toml does not define", check.id, variant.configuration));
				}
				if variant.architecture != "host" && !ARCHITECTURES.contains(&variant.architecture.as_str()) {
					errors.push(format!("check '{}' names architecture '{}'", check.id, variant.architecture));
				}
			}
		}
		errors.sort();
		errors.dedup();
		if errors.is_empty() { Ok(()) } else { Err(errors.join("\n")) }
	}
}

// Whether a crate's suite can be executed on the host in this configuration.
//
// Measured, not assumed: turning on `shared-image` makes fifteen protocol crates reach `ipc-client`
// and therefore `rt`, which defines the `panic_impl` lang item that `std` - which the test harness
// needs - defines too. The build fails with E0152 before a test runs. Declaring the variant anyway
// would put fifteen permanently red keys in the catalog, which is a worse lie than omitting them.
pub fn configuration_runnable(registry: &Registry, graph: &Graph, crate_name: &str, configuration: &str) -> bool {
	for rule in &registry.host_configuration_unrunnable {
		if rule.configuration != configuration {
			continue;
		}
		if graph.reaches(crate_name, &["link.static"]).contains(&rule.when_static_reach) {
			return false;
		}
	}
	true
}

// What a build compiles, and therefore what a compile failure in it would be about.
fn build_covers(part: &str, crates: &[Crate], staged: &BTreeSet<String>) -> Vec<String> {
	let prefix = match part {
		"sdk" => "src/sdk",
		"libs" => "src/user/libs",
		"user" => "src/user",
		"kernel" => "src/kernel",
		"loader" => "src/loader",
		// Packaging and volume assembly compile nothing; they ASSEMBLE, and their inputs are every
		// artifact the manifest stages. Declaring only the manifest and the packager was the defect:
		// `build.sh` deliberately does not chain `user` into `packages`, so a change that rebuilt
		// CoreServices need not have re-packaged it, and the guest booted the previous userspace.
		// The closure of everything staged is the honest input set, and it is deliberately wide -
		// packaging is cheap and a stale volume is not.
		"packages" | "volume" => {
			let mut covers: Vec<String> = staged.iter().cloned().collect();
			covers.push(String::from("manifest"));
			covers.push(String::from("harness.tools"));
			covers.push(String::from("volume.factory"));
			covers.sort();
			covers.dedup();
			return covers;
		}
		_ => return Vec::new(),
	};
	// The crates under this part AND the programs they build.
	//
	// Crate names alone was a false-green path, and annotation is what exposed it: while every
	// kernel test was selected regardless, a build was always in the plan for some other reason.
	// Once `covers` narrowed the suite, a change to `src/user/apps/tools/src/audioconv.rs` - which
	// resolves to `bin.audioconv`, a longer prefix than its crate - selected the audioconv scenario
	// and NO build, so the guest would have booted the previous tool and passed. A change to
	// `bin.xhci` selected nothing at all and only the empty-selection escalation caught it.
	crates.iter().filter(|entry| entry.dir == prefix || entry.dir.starts_with(&format!("{prefix}/"))).flat_map(|entry| std::iter::once(entry.name.clone()).chain(entry.binaries.iter().map(|binary| crate::graph::binary_component(&binary.name)))).collect()
}

// One kernel test, as the compiled binaries report it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct KernelTest {
	// The Rust function name, which is what the compiled binary's symbols carry. Used to join the
	// binary's per-architecture presence to the source declaration, and to say WHERE something is
	// in a diagnostic - never as the test's identity.
	pub name: String,
	// The identity: the `id = ".."` literal the declaration is required to carry. This is the
	// string the guest runner matches an exact selection against, so it is also the check id.
	//
	// It used to be `format!("kernel.{name}")`, which was the same string as the runner's identity
	// only for as long as `id` defaulted to `stringify!($name)`. Making ids mandatory and
	// namespacing them changed one side of that equality and nothing owned the other, so every
	// scoped kernel selection handed the guest names it could not match - and an unmatched id is
	// deliberately a hard failure. Taking the id from the declaration makes the two sides one
	// string by construction rather than by coincidence.
	pub id: String,
	pub architectures: Vec<String>,
	pub covers: Vec<String>,
}
