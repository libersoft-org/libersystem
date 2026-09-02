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
	// A guest step chosen by the STATE of a target rather than by what it covers.
	//
	// Two of them, and they exist because a booted target with no selected test used to be lowered
	// into a step carrying NO KEYS - which `record_step` returns on, so the largest item in a driver
	// plan was invisible to the estimator that ordered it, permanently and by construction. A step
	// that runs tests and discharges nothing is the one shape this model may not emit.
	//
	// `select` never picks these: their reason is "this target is booted and nothing else answers
	// for it", which is a fact about the plan and not about coverage, so the planner adds them after
	// the ordinary selection has run.
	GuestFallback,
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
// GATES THAT READ WHAT A GUEST RUN WROTE, and therefore cannot run before one.
//
// `capability-trace` compares the newest x86_64 capability trace against the kernel binary beside
// it, and the trace is produced by the kernel suite. A full run builds first, which makes every
// existing trace older than the binary - so ordered before the guest steps this gate cannot pass on
// a clean tree however fresh the trace was when the run started. Measured twice on 2026-08-28, the
// second time with a trace confirmed green minutes earlier.
//
// A LIST RATHER THAN A GUESS. Nothing in a gate's command line says it consumes a guest's output,
// and inferring it from "the script mentions a log" would catch the ones that write their own.
pub const GATES_AFTER_A_GUEST: [&str; 1] = ["capability-trace"];

const GATES: [(&str, &str); 66] = [
	("development-gate", "harness.tools"),
	// No unreachable body in the compiled architecture surface. Its subject is the
	// kernel, so a kernel change selects it - which is what makes it a rule rather than a list.
	("arch-surface", "kernel"),
	// Every named interrupt and SMP profile, booted. Its subject is the architecture bring-up the
	// profiles exercise, so a kernel change selects it - and it is slow, because two of the three
	// ports are emulated here.
	("qemu-arch-profiles", "kernel"),
	// A machine with more cores than the kernel holds boots on the supported count, says what it
	// parked, and retires nothing. Its subject is the kernel, because what it proves is the bring-up
	// cap and the shootdown that depends on it - and it boots one guest, so it is not a fast gate.
	("smp-core-cap", "kernel"),
	// The harness anchor is published to the harness and to nobody else. Its subject is the kernel:
	// the emission is in `boot_main` and the condition is an arch `boot_profile`, so a kernel change
	// is what can break it - and it boots two guests, so it is not a fast gate.
	("perf-anchor", "kernel"),
	// Every gate that boots a guest reads the logs that guest named, rather than the newest file in
	// a shared directory. Its subject is the harness: the rule is about how a gate finds its
	// evidence, and it reads source, so it costs milliseconds and every harness change selects it.
	("gate-result-logs", "harness.tools"),
	// Every kernel test id a gate names is one the tree still declares. Its subject is the KERNEL,
	// because a rename in the test suite is what breaks it and a kernel change is what carries one -
	// the gate scripts themselves rot only when the tests under them move.
	("gate-oracles", "kernel"),
	// Every staged driver and service has a test that names it, or a written reason why it has none.
	// Its subject is the manifest, because what it enumerates is what the manifest stages: adding a
	// driver there is what makes this ask a new question.
	("component-oracles", "manifest"),
	// ONE KEY PER PROFILE. `qemu-arch-profiles` runs them all in one step, which is one duration
	// divided evenly - and `record_step` divides evenly, so every per-profile cost on disk was
	// an artefact of the batching. These are the same profiles as separately schedulable
	// steps, each with its own measured cost, which is what a cheapest-first order needs before it
	// can be trusted. The umbrella stays for a person who wants all of them in one command.
	("arch-profile-aarch64-gicv2-1", "kernel"),
	("arch-profile-aarch64-gicv2-4", "kernel"),
	("arch-profile-aarch64-gicv3-1", "kernel"),
	("arch-profile-aarch64-gicv3-4", "kernel"),
	("arch-profile-aarch64-gicv3-its-1", "kernel"),
	("arch-profile-aarch64-gicv3-its-4", "kernel"),
	// THE DEVICE-MSI CHECKPOINT ROW, which is not a discovery profile: it boots the ITS machine
	// through firmware, because the volume package a real driver's artifact is read from is what a
	// direct boot does not carry. Its subject is still the kernel - the delivery path it asserts on
	// is the GIC's acknowledge handler and the teardown is the claim release.
	("arch-profile-aarch64-gicv3-its-device-4", "kernel"),
	// The single-node UEFI regression rows. Not discovery evidence and not a no-DT boot: what they
	// carry is the loader path, and that a boot WITH a tree does not reach the static descriptor.
	("arch-profile-aarch64-uefi-1", "kernel"),
	("arch-profile-riscv64-aia-1", "kernel"),
	("arch-profile-riscv64-aia-4", "kernel"),
	("arch-profile-riscv64-uefi-1", "kernel"),
	// The staged tree's provider chains, and the eight ways the check that reads them can be given
	// input it cannot read. Its subject is what the build stages, so a userspace change selects it.
	("staged-consistency", "userspace.build"),
	// The system volume's two shapes are two artifacts, built in both orders. Its subject is what
	// the build produces rather than what any component contains - one name for a volume with a
	// kernel and a volume without one meant whichever command ran last decided what every consumer
	// read - so a change to what the build stages is what selects it.
	("build-order", "userspace.build"),
	// The capability transfer model. Its subject is the handle and channel state
	// machines, so a kernel change selects it - the specification is a model OF that code, and a
	// change to it that the model no longer describes is exactly what this is for.
	("capability-model", "kernel"),
	("capability-trace", "kernel"),
	("virtio-iommu-protocol", "dma"),
	// THE NUMA UMBRELLA AND ITS THREE PROFILES, exactly as `qemu-arch-profiles` above: the umbrella
	// stays runnable by name and is never selected, and the three profiles carry the keys.
	//
	// One step meant one duration divided three ways, with an x86_64 KVM boot and two emulated ones
	// averaged together, and the outer `--jobs` could not schedule them against each other at all.
	("qemu-numa", "kernel"),
	("numa-profile-x86_64", "kernel"),
	("numa-profile-aarch64", "kernel"),
	("numa-profile-riscv64", "kernel"),
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
	// The shell scheduler, driven over prepared plans: failed-descendant suppression, a shared
	// prerequisite, FAIL over INCOMPLETE, an unmeasured cost against a budget, and the guest-slot
	// reservation. Its subject is `verify-model` like the two below, because the plan format it is
	// driven with is that model's output and a change to either is what can break it - and it reads
	// no source and boots nothing, so it costs milliseconds.
	("verify-scheduler", "verify-model"),
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
	// TWO SUITES OF ONE ARCHITECTURE AT THE SAME TIME, each proving it ran its own selection - the
	// standing proof that the per-run staging of the kernel, the medium and the loader actually
	// isolates concurrent runs. Its subject is the harness, because the staging it exercises is the
	// harness's, and a change there is exactly what could break it.
	//
	// REGISTERED LATE, LIKE `boot-harness` ABOVE - and this time the drift was CAUGHT rather than
	// described (2026-09-01). The gate went into `check.sh` and not into this list, and
	// `verify-model check` failed with the message the entries above predict: "check.sh runs gate
	// 'concurrent-selection', which the catalog does not know about - nothing would ever select it".
	// The two lists exist to disagree loudly, and they did their job.
	("concurrent-selection", "harness.tools"),
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
	// Every staged driver carries a `.liberdrv.note` declaring the protocol version its own
	// source emits, and the count in the volume is floored from the manifest. Its subject is
	// `driver-protocol`, where the version the note declares lives: bump it and every staged
	// artifact has to be rebuilt to agree.
	//
	// A SINGLE SUBJECT UNDERSTATES IT, and that is worth saying rather than hiding. The gate also
	// fails when a linker script stops KEEP-ing the section - `userspace.link.*` - and the table
	// this list is written in holds one subject per gate. The protocol version is the failure worth
	// catching early; a dropped KEEP is caught by the same gate on the next change that selects it.
	("driver-protocol-note", "driver-protocol"),
	// No numbered per-provider local in DeviceManager. Four `blockN_client` variables were a count
	// of disks compiled into the manager, so a fifth had nowhere to go and which volume was which
	// depended on which driver finished first. Its subject is the crate that holds the manager, so a
	// change to it selects the gate - and it reads source, so it costs milliseconds.
	("no-fixed-provider-slots", "services"),
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

// GATES THAT ARE EXACTLY THE UNION OF OTHER GATES, and must therefore never be SELECTED.
//
// `check.sh` runs `qemu-arch-profiles` (every interrupt profile, one command) and also runs the
// `arch-profile-*` entries individually, because a person typing one name wants all of them and
// the scheduler needs separately measured costs. Both were in the catalog and both cover
// `kernel`, so any kernel change selected all of them - and `commands::steps` merges pre-guest gates into
// one `./check.sh --gate a,b,...`, so the expensive emulated profiles ran ONCE EACH under their
// own keys and then a SECOND time inside the umbrella. Measured in a full sweep: the gate step carried
// the umbrella key and every profile key.
//
// The entry stays in `GATES` so the two lists still agree - check.sh really does run it - and it gets
// no catalog check, so nothing can select it. Running the union by hand stays a command a person can
// type; paying for it twice in a sweep does not.
const UMBRELLA_GATES: [&str; 2] = ["qemu-arch-profiles", "qemu-numa"];

// THE ROWS OF THOSE UMBRELLAS, WHICH MUST EACH BE A STEP OF THEIR OWN.
//
// M3.6 and the definition of done both say each profile of a profile gate is its own step with its
// own key, "and no cost derived from a merged step survives into the first cheapest-first run".
// Splitting them in the CATALOG achieved neither on its own: `commands::steps` folds every ordinary
// pre-guest gate into one `Step` with one id, one comma-separated `check.sh --gate a,b,...` and all
// the keys, so twelve emulated profiles came out as part of one step - no independent id, no
// independent measured cost, and nothing the outer `--jobs` could schedule (fixed 2026-09-02).
//
// They are named here rather than matched on a prefix: a rule that reads a name is a rule that breaks
// the first time somebody calls a gate `arch-profile-something-else`, and this list is checked
// against `GATES` by a test.
//
// NOT one step per profile with a `--jobs` of its own - that is the second scheduler M3.6 refuses.
// Each is an ordinary serial step that boots its guests one at a time; what it gains is an identity
// and a duration of its own.
pub const PROFILE_ROW_GATES: [&str; 14] = [
	"arch-profile-aarch64-gicv2-1",
	"arch-profile-aarch64-gicv2-4",
	"arch-profile-aarch64-gicv3-1",
	"arch-profile-aarch64-gicv3-4",
	"arch-profile-aarch64-gicv3-its-1",
	"arch-profile-aarch64-gicv3-its-4",
	"arch-profile-aarch64-gicv3-its-device-4",
	"arch-profile-aarch64-uefi-1",
	"arch-profile-riscv64-aia-1",
	"arch-profile-riscv64-aia-4",
	"arch-profile-riscv64-uefi-1",
	"numa-profile-x86_64",
	"numa-profile-aarch64",
	"numa-profile-riscv64",
];

// Whether this gate is one profile row of a multi-profile gate. See `PROFILE_ROW_GATES`.
pub fn gate_is_profile_row(name: &str) -> bool {
	PROFILE_ROW_GATES.contains(&name)
}

// HOW MANY GUESTS A GATE STARTS AT THE SAME TIME.
//
// One for a gate that boots nothing or boots serially - which is every other gate in this tree,
// including the eight-profile ones: a barrier and a slot is the whole of what they need, and the
// runner already gives them that.
//
// `concurrent-selection` is the exception and the reason this exists. Its subject IS overlap - two
// same-architecture suites running at once, which is the collision P02M0167 measured - so it cannot
// be made serial without deleting what it proves. Declaring the count is what lets the ONE scheduler
// account for it: `verify.sh` takes that many of its slots and refuses to start the gate inside a
// `--jobs` that cannot hold them, rather than the gate quietly making its own answer to "how many
// QEMUs may run on this machine".
pub fn gate_concurrent_guests(gate: &str) -> usize {
	match gate {
		"concurrent-selection" => 2,
		_ => 1,
	}
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
			// The union entries are declared and never selected - see UMBRELLA_GATES.
			if UMBRELLA_GATES.contains(&gate) {
				continue;
			}
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

		// THE TWO GUEST STEPS THAT ARE NOT A TEST LIST, in the catalog so that each has an id, a
		// variant per target and somewhere for its measured cost to live.
		//
		// `guest.boot-smoke` is for a target that WAS enumerated and selected nothing: the
		// architecture policy has already decided this target boots, so it boots something named
		// rather than everything or nothing. `guest.whole-suite` is for a target the model could not
		// enumerate at all, where booting blind is the safe answer to a model that cannot see.
		//
		// Both carry no `covers` on purpose and are skipped by `select`; the planner adds the one the
		// target's state calls for. Giving them a real `covers` would run them BESIDE the tests they
		// stand in for, which is the opposite of what they are.
		catalog.checks.push(Check { id: String::from("guest.boot-smoke"), kind: CheckKind::GuestFallback, covers: Vec::new(), variants: ARCHITECTURES.iter().map(|architecture| Variant { architecture: (*architecture).to_string(), environment: Environment::TestGuest, configuration: String::from("test") }).collect(), command: String::from("./test.sh --arch {arch} --tags smoke") });
		catalog.checks.push(Check { id: String::from("guest.whole-suite"), kind: CheckKind::GuestFallback, covers: Vec::new(), variants: ARCHITECTURES.iter().map(|architecture| Variant { architecture: (*architecture).to_string(), environment: Environment::TestGuest, configuration: String::from("test") }).collect(), command: String::from("./test.sh --arch {arch}") });

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
		//
		// `dev.gpu-restart` is the fourth, and it is here rather than in `check.sh` for the reason
		// this whole list exists: it needs a guest that can be TOLD to do something after it has
		// booted. P02M0159's M4 asks the enforcing profile to show the display driver surviving a
		// restart, and the gate that watches that profile requires the driver to come up EXACTLY
		// ONCE - correctly, since on a cold boot a second bind is a restart loop. Asking for the
		// restart is a different check from refusing one, and the persistent instance is the machine
		// that can be asked: it boots through `run.sh`, whose default x86_64 machine puts every
		// virtio endpoint behind a virtio-iommu, and `lsdev --disable`/`--enable` drive the
		// operator's own policy path from inside it.
		for (id, script, subject) in [
			("dev.selftest", "harness/dev-selftest.py", "harness.boot"),
			("dev.proto-test", "harness/proto-test.py", "proto"),
			("dev.perf-gate", "harness/perf-gate.py", "harness.boot"),
			("dev.gpu-restart", "harness/dev-gpu-restart.py", "harness.boot"),
		] {
			let mut covers = vec![subject.to_string()];
			if id == "dev.selftest" {
				covers.extend(development_only.iter().cloned());
				covers.sort();
				covers.dedup();
			}
			// WHOSE RESTART IT IS. The check drives the display driver through DeviceManager's
			// policy verbs, so a change to either is exactly what could break it - and a check that
			// covered only the harness would not be selected by the change it exists to catch.
			if id == "dev.gpu-restart" {
				covers.extend(["bin.virtio_gpu".to_string(), "bin.device_manager".to_string()]);
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
	// The files that declare this test. PLURAL: an arch-gated test is declared once per target under
	// one id, and keeping one path would keep whichever file was read last.
	//
	// This is what makes "a test file reaches the tests in it" answerable. Without it the model knows
	// only that a file under `src/kernel` changed, which is the whole kernel.
	pub source_paths: Vec<String>,
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
