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

	// The inverse, for reading a key back out of a shadow log. Exhaustive against `as_str` so the
	// two cannot drift: a name this does not know is not silently mapped to a default.
	pub fn from_str(text: &str) -> Option<Environment> {
		match text {
			"host" => Some(Environment::Host),
			"test-guest" => Some(Environment::TestGuest),
			"dev-guest" => Some(Environment::DevGuest),
			_ => None,
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
const GATES: [(&str, &str); 42] = [
	("development-gate", "harness.tools"),
	// No unreachable body in the compiled architecture surface. Its subject is the
	// kernel, so a kernel change selects it - which is what makes it a rule rather than a list.
	("arch-surface", "kernel"),
	// Every named interrupt and SMP profile, booted. Its subject is the architecture bring-up the
	// profiles exercise, so a kernel change selects it - and it is slow, because two of the three
	// ports are emulated here.
	("qemu-arch-profiles", "kernel"),
	// The staged tree's provider chains, and the seven ways the check that reads them can be given
	// input it cannot read. Its subject is what the build stages, so a userspace change selects it.
	("staged-consistency", "userspace.build"),
	// The capability transfer model. Its subject is the handle and channel state
	// machines, so a kernel change selects it - the specification is a model OF that code, and a
	// change to it that the model no longer describes is exactly what this is for.
	("capability-model", "kernel"),
	("capability-trace", "kernel"),
	("virtio-iommu-protocol", "dma"),
	("qemu-numa", "kernel"),
	("qemu-virtio-iommu-x86_64", "kernel"),
	("implementation-mutations", "kernel"),
	// The model's invariants proved capable of failing. Same subject as the model
	// itself, because a mutation is a statement about the code the model describes.
	("model-mutations", "kernel"),
	// Which keys a loader carries. Its subject is the loader, so a boot change
	// selects it - and the two profiles differing only in a comment is the failure it exists for.
	("trust-profile", "bin.libersystem-loader"),
	// The boot that must NOT happen: a medium whose signed manifest was altered must stop it. Its
	// subject is the loader and the media the harness stages, so a boot change selects it.
	("signed-boot", "bin.libersystem-loader"),
	// The firmware's own verification of the loader. Subject is the loader.
	("secure-boot", "bin.libersystem-loader"),
	// The development configuration COMPILES. Its subject is every services and
	// drivers crate at once, which is a set no single component names - so it takes the
	// always-selected label for the reason the entry above it does, and for a second one: the fault
	// it catches is a source change that only the other configuration reads, and a change like that
	// need not touch anything the model would trace back to it.
	("development-build", "harness.tools"),
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
	// The 1 -> 4 handle migration's gate: building a capability list from ONE received
	// handle is banned outside the runtime primitive. It was added to `check.sh` and not here, so
	// `verify-model check` reported a gate nothing would ever select - which is the same class of
	// drift the two lists exist to catch, caught by them.
	("single-cap-receive", "harness.tools"),
	// An infallible allocation on a kernel path ring 3 can drive. Its subject is
	// the kernel, so a kernel change selects it - which is what makes it a rule rather than a list.
	// It arrived in `check.sh` before it arrived here, and `verify-model check` said so on the next
	// run, which is the drift these two lists exist to catch.
	("kernel-allocations", "kernel"),
	// The `--artifact` fast path must know a library's dependency closure, or it
	// reports an artifact current after a crate it compiles against changed - which makes every test
	// result taken on that artifact meaningless. Registered in `check.sh` on 2026-08-12 and not
	// here, so `verify-model check` had been reporting it as unselectable ever since.
	("targeted-cache", "harness.tools"),
	// A warning answered by switching the lint off. Its subject is every crate
	// under `src/`, so it belongs to the harness and every change selects it - which is right, since
	// the attribute it refuses can be added anywhere and costs milliseconds to look for.
	("no-suppression", "harness.tools"),
	// Every source the loader may boot from carries a manifest naming what will be
	// read from it. Its subject is the staging - `mkpackages`, `mkimage.sh`, `qemu-run.sh` - and the
	// media they write, so it belongs to the harness and every change selects it.
	("boot-manifest", "harness.tools"),
	// The boot harness tested against fakes - the scenario oracles, the broker's
	// reply framing, the instance identity records and the preflight producers. Its subject is the
	// harness itself, so a harness change selects it, and it needs no guest and no QEMU.
	//
	// REGISTERED LATE, which is the drift the entries above keep describing: it went into `check.sh`
	// first and `verify-model check` would have reported it as a gate nothing selects.
	("boot-harness", "harness.tools"),
	// A hand-written `extern` declaration and the generated function it is
	// forwarded to are joined by a bare jump, so a signature that disagrees is a silent
	// argument-register mismatch rather than a link error. One such pair made every transactional
	// write in the system return "no answer" and compiled without a warning. Its subject is the
	// generated protocol code and every client crate that declares into it, which is a set no single
	// crate label names - so it takes the always-selected one, for the reason `milestone-index`
	// does: it reads source, costs milliseconds, and a pair that drifts apart is not something to
	// find only when the change that broke it is far behind.
	("forwarded-abi", "harness.tools"),
	// The migration gate: while the bootstrap ladder and the generated role plan both
	// describe the wiring, they must agree. Its subject is the manifest and the supervisor, and it
	// reads source rather than building anything.
	("bootstrap-plan", "harness.tools"),
	// The dynamic-report checker's own exit contract, mode dispatch and refusal
	// behaviour, against a disposable fixture. The subject is that checker, which is harness tooling.
	("dynamic-report-regressions", "harness.tools"),
	// A frame a page table ever pointed at goes back through `frame::retire`,
	// and every plain `deallocate` says why no core can still translate it. Its subject is the
	// kernel, so a kernel change selects it. Registered here in the same commit as `check.sh` this
	// time - the two gates before it both arrived on one side first and were reported as
	// unselectable on the next run, which is the drift these lists exist to catch.
	("frame-retirement", "kernel"),
	// Two gates over one subject: generated or compiled artifacts below `src`, in the working tree and anywhere
	// in reachable history. They were Justfile recipes that nothing called and nothing selected;
	// moving them into `check.sh` is what makes a change to the tree able to select them, and this
	// is the other half of that - registered in the same change, not on the next run.
	//
	// Their subject is the SOURCE TREE rather than any built thing, so they belong to the harness,
	// which every run depends on. The history half reads reachable Git history and does not vary by
	// target either.
	("source-hygiene", "harness.tools"),
	("source-history-hygiene", "harness.tools"),
	// Every LSIDL interface a manifest role names must be one LSIDL
	// defines. Its subject is the manifest and the IDL, neither of which is a crate, and it reads
	// declarations rather than generated bindings - so it takes the always-selected label for the
	// same reason `milestone-index` and `forwarded-abi` do, and costs milliseconds.
	("declared-interfaces", "harness.tools"),
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
// Every universe that judges ANY component, which is what the model self-check asks against the
// producers: a universe nothing can answer for makes every component it judges permanently
// untrustable.
pub fn all_judging_universes(catalog: &Catalog) -> BTreeSet<crate::shadow::Universe> {
	let mut seen: BTreeSet<crate::shadow::Universe> = BTreeSet::new();
	for check in &catalog.checks {
		if check.covers.is_empty() {
			continue;
		}
		for variant in &check.variants {
			seen.insert(match variant.environment {
				Environment::Host if check.kind == CheckKind::Build => crate::shadow::Universe::HostBuild,
				Environment::Host => crate::shadow::Universe::Host,
				Environment::TestGuest => crate::shadow::Universe::TestGuest,
				Environment::DevGuest => crate::shadow::Universe::DevGuest,
			});
		}
	}
	seen
}

pub fn judging_universes(catalog: &Catalog, component: &str) -> Vec<crate::shadow::Universe> {
	let mut seen: BTreeSet<crate::shadow::Universe> = BTreeSet::new();
	for check in &catalog.checks {
		if !check.covers.iter().any(|covered| covered == component) {
			continue;
		}
		for variant in &check.variants {
			seen.insert(match variant.environment {
				// A BUILD IS ITS OWN UNIVERSE. Both run on the host, and only one of them has a
				// shadow producer - so grouping them let a certificate earned over gates, suites and
				// conformance stand for builds nothing had compared.
				Environment::Host if check.kind == CheckKind::Build => crate::shadow::Universe::HostBuild,
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
			("dev.selftest", "harness/dev-selftest.py", "harness.boot"),
			("dev.proto-test", "harness/proto-test.py", "proto"),
			("dev.perf-gate", "harness/perf-gate.py", "harness.boot"),
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
		"loader" => "src/boot/loader",
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
