#!/usr/bin/env bash
# Every gate and conformance suite, behind one command.
#
# These were 28 Justfile recipes - 16 `*-check` and 12 `*-conformance` - and the second group was a
# LIST OF DATA written as code: eleven recipes differing only in an image format's name. A caller
# wants all of them (CI) or one of them (a person chasing a failure), and neither wants to read 28
# names to find out which exist.

SCRIPT_NAME=check.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
source "$SRC_DIR/tools/evidence.sh"
install_guest_cleanup

# EVERY GATE RUNS UNDER `gate`, set here only when unset so an outer runner's value survives: a gate
# is the outermost entry point that knows what kind of boot it is starting, and the test runner it
# invokes for its phases leaves the value alone.
export LIBER_RUN_MODE="${LIBER_RUN_MODE:-gate}"

# name -> command, run from src/. The static-injection family shares one script and differs by its
# argument, which is exactly the shape that became six recipe names.
declare -A GATES=(
	["development-gate"]="tools/check-development-gate.sh"
	# Every symbol in an architecture's compiled contract is a path that
	# architecture can execute. Twenty `todo!()` bodies used to answer the x86 loader hand-off on the
	# two ports that never arrive through it, and nothing but a paragraph of prose separated "dormant
	# by construction" from "unfinished".
	["arch-surface"]="tools/check-arch-surface.sh"
	# The staged-tree consistency check, mutated until it refuses. Eight ways an
	# identity note can be unreadable, missing or contradictory, each applied to the real staged tree
	# and put back - because what a check does with input it cannot read IS the check, and this one
	# used to skip all of them and print that everything matched.
	["staged-consistency"]="tools/check-staged-consistency.sh"
	# The system volume's two shapes are two artifacts, built in both orders. One name for a volume
	# with a kernel and a volume without one meant whichever command ran last decided what every
	# consumer read - a suite booting the shipping kernel, an ISO naming a volume that no longer
	# existed - and five investigations in one round ended at the order of two commands.
	["build-order"]="tools/check-build-order.sh"
	# Every named interrupt and SMP profile, booted, with an oracle per claim.
	# A GICv2, a GICv3 without its ITS, one with it, and a RISC-V AIA, at one core and at four - each
	# of which used to be a runner knob turned by hand once, with its output pasted into a document.
	# aarch64 and riscv64 are emulated here, so this is minutes rather than seconds.
	["qemu-arch-profiles"]="tools/check-qemu-arch-profiles.sh"
	# The capability transfer model, explored exhaustively under each published
	# configuration. It runs TLC from the JAR pinned in `toolchain.lock` with NO NETWORK, and names
	# `./bootstrap.sh tla2tools` when the artifact is absent rather than fetching one.
	["capability-model"]="tools/check-capability-model.sh"
	# A two-node QEMU machine, read from its SRAT and SLIT, with the frame
	# allocator partitioned per node - and placement proved by the physical addresses that come back
	# rather than by timing, which an emulated topology cannot show.
	["qemu-numa"]="tools/check-qemu-numa.sh"
	# The virtio-iommu wire codec, its feature negotiation and its event
	# parser, driven against malformed and hostile answers a real device does not readily produce.
	["virtio-iommu-protocol"]="tools/check-virtio-iommu-protocol.sh"
	# The other half of that, and the one that is isolation evidence rather than enumeration: a
	# QEMU profile with a virtio-iommu and two hostile `edu` endpoints, each told to reach memory it
	# was never given. What is checked is the SENTINEL MEMORY, not the error code.
	["qemu-virtio-iommu-x86_64"]="tools/check-qemu-virtio-iommu-x86_64.sh"
	# THE DMA MODE. The frozen carrier record, its producer and its consumer, on the host in
	# milliseconds; then every row of the x86_64 matrix booted - the development pair, the shipping
	# pair, the host pairing check and every producible refusal; then the two device-tree ports on
	# both entry paths, emulated and slow.
	# THE IPv6 LAYER AGAINST A CONTROLLABLE PEER. Emulated and slow, so it is its own row rather than
	# part of a sweep: it boots three guests, each against a scripted far end of the wire.
	["ipv6-peer"]="tools/check-ipv6-peer.sh"
	# THE SELECTION SLOT, through the real launch path. A consumer with no `DT_NEEDED` edge to its
	# provider, the candidate carried by digest in the authenticated record, and the binding done
	# before the first thread runs - none of which is observable from outside a launch, so this boots
	# a guest and reads what the bound provider actually agreed to.
	["icd-selection"]="tools/check-icd-selection.sh"
	# THE GRAPHICS PROFILES, WHICH ARE CODE. Two closed enumerations - `Render2D Core Profile 1` and
	# `Render3D Core Profile 1` - from which every table, checklist, conformance matrix and capability
	# report is generated, hashed so a change to a profile is a line in a diff. It also runs the three
	# checks no reviewer reliably catches: a handler for every backend-owned feature, a conformance
	# test for every feature, and no test claiming a feature the profile does not have. Host-only and
	# seconds; the two coverage halves report NOT PERFORMED until a backend and a suite exist.
	["graphics-profile"]="tools/check-graphics-profile.sh"
	# Static initialisation is a mechanism the converged closure ADMITS, so it gets positive gates:
	# order across the provider DAG, what a constructor that stopped half way leaves behind,
	# destructors in reverse on a normal exit, and which of them a crash does not run.
	["lifecycle"]="tools/check-lifecycle.sh"
	# The ported loader ITSELF, in a guest, against a synthetic ICD. A substrate that satisfies every
	# undefined symbol and still cannot load an ICD has prepared nothing, and a gate that never runs
	# the thing being ported proves the wrong half.
	["foreign-loader-guest"]="tools/check-foreign-loader-guest.sh"
	# Every admitted foreign ABI facility, called once in a guest with its answer CHECKED. The static
	# gates hold the substrate to its exact surface and none of them shows that a facility works - a
	# symbol that is present and wrong is a link that succeeds and a driver that misbehaves later.
	["foreign-facilities-guest"]="tools/check-foreign-facilities-guest.sh"
	["dma-mode-carrier"]="tools/check-dma-mode-carrier.sh"
	["dma-mode-x86_64"]="tools/check-dma-mode-x86_64.sh"
	["dma-mode-ports"]="tools/check-dma-mode-ports.sh"
	["dma-mode-aarch64"]="tools/check-dma-mode-ports.sh --only aarch64"
	["dma-mode-riscv64"]="tools/check-dma-mode-ports.sh --only riscv64"
	# THE VIRTIO-IOMMU PROFILES OF THE PORTS: five rows of two phases each - a hostile or transition
	# phase on the test kernel, an ordinary phase on the built system - every phase its own gate and
	# its own catalog key, the rows and `iommu-ports` umbrellas a person runs by name. Emulated, so
	# minutes per phase.
	["iommu-ports"]="tools/check-qemu-iommu-ports.sh"
	["iommu-aarch64-direct-gicv2"]="tools/check-qemu-iommu-ports.sh --only aarch64:direct-gicv2"
	["iommu-aarch64-direct-gicv2-hostile"]="tools/check-qemu-iommu-ports.sh --only aarch64:direct-gicv2:hostile"
	["iommu-aarch64-direct-gicv2-ordinary"]="tools/check-qemu-iommu-ports.sh --only aarch64:direct-gicv2:ordinary"
	["iommu-aarch64-direct-gicv3-its"]="tools/check-qemu-iommu-ports.sh --only aarch64:direct-gicv3-its"
	["iommu-aarch64-direct-gicv3-its-transition"]="tools/check-qemu-iommu-ports.sh --only aarch64:direct-gicv3-its:transition"
	["iommu-aarch64-direct-gicv3-its-ordinary"]="tools/check-qemu-iommu-ports.sh --only aarch64:direct-gicv3-its:ordinary"
	["iommu-aarch64-uefi-gicv2"]="tools/check-qemu-iommu-ports.sh --only aarch64:uefi-gicv2"
	["iommu-aarch64-uefi-gicv2-transition"]="tools/check-qemu-iommu-ports.sh --only aarch64:uefi-gicv2:transition"
	["iommu-aarch64-uefi-gicv2-ordinary"]="tools/check-qemu-iommu-ports.sh --only aarch64:uefi-gicv2:ordinary"
	["iommu-riscv64-direct-aia"]="tools/check-qemu-iommu-ports.sh --only riscv64:direct-aia"
	["iommu-riscv64-direct-aia-hostile"]="tools/check-qemu-iommu-ports.sh --only riscv64:direct-aia:hostile"
	["iommu-riscv64-direct-aia-ordinary"]="tools/check-qemu-iommu-ports.sh --only riscv64:direct-aia:ordinary"
	["iommu-riscv64-uefi-aia"]="tools/check-qemu-iommu-ports.sh --only riscv64:uefi-aia"
	["iommu-riscv64-uefi-aia-transition"]="tools/check-qemu-iommu-ports.sh --only riscv64:uefi-aia:transition"
	["iommu-riscv64-uefi-aia-ordinary"]="tools/check-qemu-iommu-ports.sh --only riscv64:uefi-aia:ordinary"
	# TWO SUITES OF ONE ARCHITECTURE AT ONCE, each proving it ran its OWN selection. The per-run
	# staging of the kernel, the medium and the loader was argued for in comments and reproduced by
	# hand once; this is the standing proof, and it is the one gate here that deliberately overlaps
	# two guests rather than avoiding it.
	["concurrent-selection"]="tools/check-concurrent-selection.sh"
	# The other direction. `capability-model` explores what the model can do; this
	# replays what the KERNEL did against it, and requires the checker to refuse eleven deliberate
	# defects before it is trusted with a real trace.
	["capability-trace"]="tools/check-capability-trace.sh --require-live"
	# The same five defects in the KERNEL rather than in the model, each
	# required to be caught by the QEMU conformance fixture. Builds and boots a mutated kernel from a
	# COPY of the tree - nothing here writes a source file.
	["implementation-mutations"]="tools/check-implementation-mutations.sh"
	# The gate that proves the gate can fail. Six deliberate defects, each required to be
	# caught by the invariant that names it, and nine covers each required to be REFUTED.
	["model-mutations"]="tools/check-model-mutations.sh"
	# The two trust profiles differ in the BINARY. The test key's private half is
	# published on purpose, which is exactly why a release loader must contain none of it.
	["trust-profile"]="tools/check-trust-profile.sh"
	# The boot that must NOT happen. One successful signed boot proves the
	# pieces fit; only a refused one proves the check is load-bearing.
	["signed-boot"]="tools/check-signed-boot.sh"
	# The firmware verifies the LOADER, or the loader does not run. Preflights its four
	# host tools by name and skips nothing when one is missing.
	["secure-boot"]="tools/check-secure-boot.sh"
	["rollback-floor-x86_64"]="tools/check-rollback-floor-x86_64.sh"
	# The other configuration of the same source, compiled. `development-gate` above checks which
	# artifacts a configuration STAGES; it never builds the one it is guarding, and the profile it
	# guards stopped compiling twice in one release without anything noticing.
	["development-build"]="tools/check-development-build.sh"
	["artifact-metadata"]="tools/check-artifact-metadata.sh"
	["dynamic-report"]="tools/check-dynamic-report.sh --check"
	# The checker above decides whether the tracked reports still describe the tree, and it used to
	# prove itself by re-invoking itself - a second full ELF sweep that accepted any nonzero status
	# as proof. This tests its exit contract from outside, against a fixture, in seconds.
	["dynamic-report-regressions"]="tools/check-dynamic-report-regressions.sh"
	["test-tags"]="harness/check-test-tags.sh"
	# The harness that decides whether every other test passed, tested against fakes. An audit found
	# four of its oracles reporting success without measuring their subject; each looked correct on a
	# reading, which is why this is a gate rather than a review note.
	["boot-harness"]="harness/harness-test.py"
	["host-tests"]="tools/check-host-tests.sh"
	# The shell scheduler `verify.sh` is, driven over plans written for it: failed-descendant
	# suppression, a prerequisite shared by two branches, FAIL outranking INCOMPLETE, an unmeasured
	# cost against a budget, and the guest-slot reservation. Everything it decides was unreachable by
	# a test until this existed, which is why its ordering defects were found by reading.
	["verify-scheduler"]="tools/check-verify-scheduler.sh"
	["verify-model"]="cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- check"
	["verify-model-tests"]="cargo test --quiet --manifest-path tools/verify-model/Cargo.toml"
	# The evidence path's own fixtures: a kept log outlives its producer, a replaced tool or
	# firmware image is not what boots, a release snapshot refuses a write, a moved tree fails
	# its dossier. Boots two short x86_64 guests.
	["verify-evidence"]="tools/check-verify-evidence.sh"
	# THE DEVELOPMENT GUEST'S LIFECYCLE: image, boot, readiness, the four development checks
	# against that instance, teardown - run-private state throughout, so it never depends on a
	# guest somebody left running and never disturbs one. Its catalog row is the lifecycle
	# producer, not a gate row: the development checks name it as their prerequisite.
	["development-lifecycle"]="tools/check-development-lifecycle.sh"
	["static-image"]="tools/check-static-injection.sh static"
	["undeclared-edge"]="tools/check-static-injection.sh undeclared-edge"
	["duplicate-edge"]="tools/check-static-injection.sh duplicate-edge"
	["malformed-dynamic"]="tools/check-static-injection.sh malformed-dynamic"
	["malformed-symbol-relocation"]="tools/check-static-injection.sh malformed-symbol-relocation"
	["identity-note"]="tools/check-static-injection.sh identity-note"
	["volume-layout"]="tools/check-volume-layout.sh ../.build/boot/volume-x86_64.pkg"
	["milestone-index"]="tools/check-milestone-index.sh"
	# THE DRIVER PROTOCOL VERSION, IN THE BYTES THAT SHIPPED. It is read before a driver is given a
	# device, so a note that did not survive the link and the strip would make that refusal a check
	# of nothing - silently, because a driver with no note reads exactly like one that declares no
	# version.
	["driver-protocol-note"]="tools/check-driver-protocol-note.sh"
	# Generated or compiled artifacts below src, in the working tree and anywhere in reachable
	# history. Two Justfile recipes that nothing called; cheap enough to be part of "check it", and
	# a tree that has committed a build output does not un-commit it by nobody looking.
	# The dependency and licensing policy, which is three refusals rather than a description: an
	# unreviewed licence, a source the build downloads, and a copied implementation whose attribution
	# was lost. Each is a build failure, because a warning on a licence question is read once and the
	# failure it prevents is found by somebody else in a distributed binary.
	["dependency-policy"]="tools/check-dependency-policy.sh"
	# The foreign-substrate lockfile against the tree it pins. A cross file or a sysroot header
	# edited after the freeze changes the ABI every later measurement was taken under, silently -
	# and the inventory would then describe a compile nobody can reproduce.
	["foreign-pin"]="tools/check-foreign-pin.sh"
	# The foreign ABI facilities are EXACTLY what the derived inventory names. "Do not grow a general
	# POSIX layer" is a rule about a direction of travel and every step along it looks reasonable;
	# it holds only if adding a symbol the inventory does not name fails.
	["foreign-facilities"]="tools/check-foreign-facilities.sh"
	# The profile sysroot declares exactly what the substrate provides - the inverse of the bootstrap
	# sysroot's rule, which is sized by the option set. An inversion nobody checks decays back into
	# what it inverted, one declaration at a time, and the link failure arrives weeks later.
	["profile-sysroot"]="tools/check-profile-sysroot.sh"
	# A foreign artifact's identity is load bearing field by field: the compiler, the archiver, the
	# linker, the flags, the sysroot and the configure inputs each change what the artifact IS without
	# changing a source byte, so each must move its digest, its consumer edges and its cache key.
	["foreign-identity"]="tools/check-foreign-identity.sh"
	# Pass 2: the converged audit link. An archive does not resolve its external symbols, so pass 1
	# measures a CANDIDATE surface; what the substrate must provide is what a link actually resolved,
	# and the gate is the fixed point rather than the first link.
	["foreign-audit-link"]="tools/check-foreign-audit-link.sh"
	# The C++ ABI decision matrix, on the artifact. Exception tables, RTTI and `atexit` registration
	# are FORBIDDEN under this pin, which means flags that stop them being emitted AND a check that
	# catches them if a flag, a compiler or a source ever changes that.
	["foreign-cxx-abi"]="tools/check-foreign-cxx-abi.sh"
	# The audit-linked artifact itself, through the generic checks, as a file. This milestone
	# previously exempted the one artifact whose surface it claims - only a synthetic one was checked
	# - which would have let it close with a substrate derived from an ELF the importer rejects.
	["foreign-audit-artifact"]="tools/check-foreign-audit-artifact.sh"
	["source-hygiene"]="tools/check-source-hygiene.sh --current"
	["source-history-hygiene"]="tools/check-source-hygiene.sh --history"
	["single-cap-receive"]="tools/check-single-cap-receive.sh"
	# A kernel allocation ring 3 can trigger must be able to refuse. Three audits closed that class
	# by enumeration and a fourth would have found the next member; this is the rule instead.
	["kernel-allocations"]="tools/check-kernel-allocations.sh"
	# A frame a page table ever pointed at goes back through `frame::retire`. The module's own doc
	# comment said so and the next round still wrote a rollback that unmapped a page and called
	# `deallocate` - a rule in a comment is a rule the next diff does not read.
	["frame-retirement"]="tools/check-frame-retirement.sh"
	# The hand-written bootstrap ladder and the generated role plan describe the same wiring while
	# the tree migrates from one to the other, and two descriptions of one fact is what that
	# migration exists to remove. Until the ladder is empty they are compared, tag by tag and in
	# order - a role in the wrong position has already displaced every read after it.
	["bootstrap-plan"]="tools/check-bootstrap-plan.py"
	# A hand-written `extern` declaration and the generated function it is forwarded to are joined
	# by a bare jump, so a signature that disagrees is a silent argument-register mismatch rather
	# than a link error. One such pair made every transactional write in the system return "no
	# answer", and it compiled without a warning.
	["forwarded-abi"]="tools/check-forwarded-abi.py"
	# A manifest role naming an LSIDL interface that does not exist. The field is a reference and
	# nothing resolved it, so four of the twenty names in the file were wrong from the day they
	# were written - harmlessly, because no generator reads it yet, which is exactly how a
	# declaration rots: it is read by people, who believe it.
	["declared-interfaces"]="tools/check-declared-interfaces.py"
	["no-fixed-provider-slots"]="tools/check-no-fixed-provider-slots.sh"
	["provider-routing"]="tools/check-provider-routing.py"
	["provider-media-order"]="tools/check-provider-media-order.sh"
	# DeviceManager watched the development agent in a SECOND wait in front of its one wait, and that
	# set had no catalogue root in it - so the development configuration deadlocked on the first
	# service to ask the catalogue for a connection, and nothing boots that configuration to notice.
	["one-wait"]="tools/check-one-wait.sh"
	["driver-event-dispatch"]="python3 tools/check-driver-event-dispatch.py"
	["guest-verdict"]="python3 tools/check-guest-verdict.py"
	# A capability the grant loop never walks is one no manifest row can deliver. `lsdev` held
	# `DevicePolicy` from the day the operator verbs were built and received the CONFIG client under
	# its tag, so every operator verb was unreachable; `kill` had the same hole and could end nothing.
	["grant-vocabulary"]="tools/check-grant-vocabulary.sh"
	["smp-core-cap"]="tools/check-smp-core-cap.sh"
	# The line addressed to a tool appears only where a tool is reading it. Two boots, and the
	# ABSENCE on an interactive profile is half of what it proves.
	["perf-anchor"]="tools/check-perf-anchor.sh"
	["gate-result-logs"]="tools/check-gate-result-logs.sh"
	["gate-oracles"]="tools/check-gate-oracles.sh"
	["component-oracles"]="tools/check-component-oracles.sh"
	# ONE ENTRY PER PROFILE, and the umbrella above kept for a person who wants all eight.
	#
	# Eight profiles inside one step is one duration divided eight ways - and `record_step` divides
	# evenly, so every per-profile figure on disk was an artefact of the batching rather than a
	# measurement. An emulated four-core aarch64 profile and a one-core riscv64 one differ by more
	# than that arithmetic can express, and the scheduler needs to be able to tell them apart.
	["arch-profile-aarch64-gicv2-1"]="tools/check-qemu-arch-profiles.sh --only aarch64:gicv2:1"
	["arch-profile-aarch64-gicv2-4"]="tools/check-qemu-arch-profiles.sh --only aarch64:gicv2:4"
	["arch-profile-aarch64-gicv3-1"]="tools/check-qemu-arch-profiles.sh --only aarch64:gicv3:1"
	["arch-profile-aarch64-gicv3-4"]="tools/check-qemu-arch-profiles.sh --only aarch64:gicv3:4"
	["arch-profile-aarch64-gicv3-its-1"]="tools/check-qemu-arch-profiles.sh --only aarch64:gicv3-its:1"
	["arch-profile-aarch64-gicv3-its-4"]="tools/check-qemu-arch-profiles.sh --only aarch64:gicv3-its:4"
	["arch-profile-aarch64-gicv3-its-device-4"]="tools/check-qemu-arch-profiles.sh --only aarch64:gicv3-its-device:4"
	["arch-profile-aarch64-uefi-1"]="tools/check-qemu-arch-profiles.sh --only aarch64:uefi:1"
	["arch-profile-riscv64-aia-1"]="tools/check-qemu-arch-profiles.sh --only riscv64:aia:1"
	["arch-profile-riscv64-aia-4"]="tools/check-qemu-arch-profiles.sh --only riscv64:aia:4"
	["arch-profile-riscv64-uefi-1"]="tools/check-qemu-arch-profiles.sh --only riscv64:uefi:1"
	# THE POSITIVE NO-DEVICE-TREE ROWS. Each builds a loader that declines to pass the firmware's tree
	# on, so the kernel is handed a machine with none and selects the static descriptor its named
	# profile authorises - the half a boot WITH a tree cannot show.
	["arch-profile-aarch64-no-dt-1"]="tools/check-qemu-arch-profiles.sh --only aarch64:no-dt:1"
	["arch-profile-riscv64-no-dt-1"]="tools/check-qemu-arch-profiles.sh --only riscv64:no-dt:1"
	# THE SAME TREELESS ROWS WITH THE DMA-MODE RECORD ABSENT: the fail-closed half on the one path
	# where no carrier that depends on a tree could answer. The loader halts before a kernel loads.
	["arch-profile-aarch64-no-dt-absent-1"]="tools/check-qemu-arch-profiles.sh --only aarch64:no-dt-absent:1"
	["arch-profile-riscv64-no-dt-absent-1"]="tools/check-qemu-arch-profiles.sh --only riscv64:no-dt-absent:1"
	# THE THREE NUMA PROFILES, ONE STEP EACH, for the reason directly above.
	#
	# `qemu-numa` boots x86_64 under KVM and then aarch64 and riscv64 under emulation, and as one
	# step the scheduler could neither run them against each other nor measure them apart: one
	# duration divided three ways, with the KVM boot and the two emulated ones averaged together.
	# It is now the union of these three, and like `qemu-arch-profiles` it stays runnable by name
	# and is never selected.
	["numa-profile-x86_64"]="tools/check-qemu-numa.sh --only x86_64"
	["numa-profile-aarch64"]="tools/check-qemu-numa.sh --only aarch64"
	["numa-profile-riscv64"]="tools/check-qemu-numa.sh --only riscv64"
	# A warning answered by switching the lint off rather than by fixing the code. Ninety-one such
	# attributes had accumulated, hiding a hundred and twenty more warnings than the build printed -
	# and hiding them UNEVENLY, so the same code was reported on one target and silent on another.
	["no-suppression"]="tools/check-no-suppression.sh"
	# The boot manifest and its host-tested verifier were written first, and the two defects the
	# system actually had were in neither: the x86_64 boot medium carried no manifest at all, and the
	# kernel read from a boot medium was checked against nothing. A verifier that is right about files nobody
	# reads is not integrity - this asks the loader's question of the media on disk.
	["boot-manifest"]="tools/check-boot-manifest.sh"
	# The `--artifact` fast path knows a library's DEPENDENCIES, or it reports an artifact as
	# current after a crate it compiles against changed - which makes every test result taken on
	# that artifact meaningless. Its own family's `quick` and `provider` modes are not gates because
	# they rebuild the whole graph; this one is two targeted builds.
	["targeted-cache"]="tools/check-shared-cache.sh targeted"
)

FORMATS=(bmp gif ico icns jpeg pcx png ppm qoi tga webp)

# A gate that can also REGENERATE what it checks, and the command that does it.
#
# The dynamic report is three tracked TSVs describing the staged ELF graph; the gate compares them
# against the tree and `--write` produces them. Those were two Justfile recipes with nothing to say
# they were two halves of one thing - and the writing half is the one that must be reached
# deliberately, since it overwrites files under review.
declare -A REFRESH=(
	["dynamic-report"]="tools/check-dynamic-report.sh --write"
)

# Checks that take arguments, so they are flags rather than gate names - and, like --conformance,
# they run only when asked. Each rebuilds or re-stages something, which is why none belongs in the
# set `./check.sh` with no arguments runs.
#
#   --staged-image [T...]      are the staged images on disk the ones this tree produces
#   --cache-check MODE         quick | provider | targeted - build-cache invalidation, end to end
#   --fast-path [T] [A...]     the targeted build and the authoritative rebuild produce equal bytes

help() {
	usage_and_exit <<EOF
usage: check.sh [--gate NAME[,NAME...]] [--conformance [FORMAT[,FORMAT...]]]
                [--refresh NAME] [--staged-image [TARGET...]] [--cache-check MODE]
                [--fast-path [TARGET] [ARTIFACT...]] [--list]

Runs the build gates and the image conformance suites. With no arguments, runs everything.

  --gate NAME          run these gates only ('all' for every gate)
  --conformance [FMT]  run these conformance suites only (no value, or 'all', means every format)
  --refresh NAME       REGENERATE what a gate checks, instead of checking it
  --staged-image [T]   are the staged images the ones this tree produces (default: all staged)
  --cache-check MODE   build-cache invalidation end to end: quick | provider | targeted
  --fast-path [T] [A]  the targeted build and the authoritative rebuild produce equal bytes
  --list               print the names and exit
  -h, --help           this text

gates:
  ${!GATES[*]}

refreshable:
  ${!REFRESH[*]}

conformance formats:
  ${FORMATS[*]}

examples:
  ./check.sh                            # everything
  ./check.sh --gate volume-layout       # one gate
  ./check.sh --conformance png,webp     # two formats
  ./check.sh --conformance              # every format, no gates
  ./check.sh --refresh dynamic-report   # rewrite the tracked reports from the built tree

The four argument-taking checks rebuild or re-stage something, so they run only when named - never
as part of a bare ./check.sh.

Gates that inspect built artifacts expect a build to exist; run ./build.sh first.
EOF
}

# Report HOW a gate failed, not just that the run stopped.
#
# `set -e` made a failing gate end the run with whatever the gate had printed - which for a gate
# that is KILLED is nothing at all. Three runs ended that way in one night, each at a different
# point, and the empty log left no way to tell a gate that refused its input from one that was
# taken out from under us. A status is not a diagnosis, but it separates those two: an ordinary
# non-zero exit means the gate decided, and a signal means something else decided for it.
run_gate() {
	local name="$1" cmd="${GATES[$1]:-}" status=0 started=$SECONDS
	[[ -n "$cmd" ]] || die "unknown gate '$name' (--list to see them)"
	note "gate: $name"
	# THE KEY THIS GATE DISCHARGES, told to the gate so it can keep its phase logs under it, and
	# used here to publish its envelope. The lifecycle gate is the one `check.sh` gate whose catalog
	# row is not a gate row: it is the development-instance producer, and the development checks
	# it runs publish their own envelopes from inside it.
	local key="gate.$name / host / host / default"
	[[ "$name" == development-lifecycle ]] && key="dev.lifecycle / x86_64 / dev-guest / development"
	export LIBER_GATE_KEY="$key"
	# Backgrounded and waited for, so a signal to this script is acted on now rather than after the
	# gate finishes - see `guest_cleanup` in lib.sh. A gate is also a SUBSHELL, which is why a trap
	# inside the gate script itself does not help: it never hears the signal.
	#
	# INSIDE A RUN THE GATE'S OUTPUT IS ALSO ITS RESULT LOG: captured through `tee` - the person
	# watching still sees it - and copied into the run by the envelope. `pipefail` makes the
	# pipeline's status the gate's, a killed gate included.
	local gate_log=""
	if evidence_active; then
		gate_log="$(mktemp "${TMPDIR:-/tmp}/liber-gate-$name.XXXXXX")"
		{ (cd "$SRC_DIR" && eval "$cmd") 2>&1 | tee "$gate_log"; } &
	else
		(cd "$SRC_DIR" && eval "$cmd") &
	fi
	wait $! || status=$?
	unset LIBER_GATE_KEY
	if [[ -n "$gate_log" ]]; then
		# PUBLISHED BEFORE THE FAILURE IS REPORTED, because reporting returns. `--if-absent`: a gate
		# that published its own envelope - the lifecycle producer - is not published over.
		local outcome=passed
		[[ "$status" -eq 0 ]] || outcome=failed
		evidence_publish "$key" "check.sh gate $name" "$outcome" "$((SECONDS - started))" --if-absent --log "$gate_log"
		rm -f "$gate_log"
	fi
	if [[ "$status" -ne 0 ]]; then
		# Bash reports a killed child as 128 + the signal number.
		if [[ "$status" -gt 128 ]]; then
			note "gate '$name' was KILLED by signal $((status - 128)) - it did not fail, something stopped it"
		else
			note "gate '$name' failed (exit $status)"
		fi
		# And whatever execution trace the gate left behind.
		#
		# The injection gates write one as they go, precisely because their unexplained deaths are the
		# ones where nothing is left alive to report: a signal skips the gate's own EXIT trap, so the
		# gate cannot print its own trace and the only thing that can is out here. Printed and then
		# removed, so a later run is never read as this one's.
		local trace
		for trace in "${TMPDIR:-/tmp}"/liber-injection-trace.*.log; do
			[[ -s "$trace" ]] || continue
			note "the last commands '$name' ran, from $trace:"
			tail -n 15 "$trace" | sed 's/^/    /' >&2
			rm -f "$trace"
		done
		return "$status"
	fi
}

run_conformance() {
	local fmt="$1"
	[[ " ${FORMATS[*]} " == *" $fmt "* ]] || die "unknown conformance format '$fmt' (--list to see them)"
	note "conformance: $fmt"
	(cd "$SRC_DIR" && cargo run --release --manifest-path "tools/$fmt-conformance/Cargo.toml")
}

gates=()
formats=()
refreshes=()
# Each is "did the caller ask" plus the arguments it gave, because all three take an OPTIONAL list.
staged_image=0
staged_image_targets=()
cache_check=""
fast_path=0
fast_path_args=()
want_gates=1
want_conformance=1

while [[ $# -gt 0 ]]; do
	case "$1" in
	-h | --help) help ;;
	--list)
		echo "gates:       ${!GATES[*]}"
		echo "conformance: ${FORMATS[*]}"
		exit 0
		;;
	--gate)
		[[ $# -ge 2 ]] || die "--gate needs a name"
		# Command substitution, NOT process substitution: `parse_list` refuses an unknown name by
		# exiting, and inside `< <(...)` that exit is the subshell's alone - the script carried on
		# with an empty selection, which then fell through to "nothing selected means everything"
		# and ran every gate. A validation failure that ends up running MORE than was asked is
		# worse than one that runs nothing.
		picked_raw="$(parse_list "$2" gate "${!GATES[*]}")"
		mapfile -t picked <<<"$picked_raw"
		gates+=("${picked[@]}")
		want_conformance=0
		shift 2
		;;
	--conformance)
		# The value is optional: `--conformance` alone means every format.
		if [[ $# -ge 2 && "$2" != -* ]]; then
			picked_raw="$(parse_list "$2" format "${FORMATS[*]}")"
			mapfile -t picked <<<"$picked_raw"
			formats+=("${picked[@]}")
			shift 2
		else
			formats=("${FORMATS[@]}")
			shift
		fi
		want_gates=0
		;;
	--refresh)
		[[ $# -ge 2 ]] || die "--refresh needs a name (refreshable: ${!REFRESH[*]})"
		[[ -n "${REFRESH[$2]:-}" ]] || die "'$2' cannot be refreshed (refreshable: ${!REFRESH[*]})"
		refreshes+=("$2")
		want_gates=0
		want_conformance=0
		shift 2
		;;
	--staged-image)
		staged_image=1
		want_gates=0
		want_conformance=0
		shift
		# Every following non-flag word is a target. With none, the script checks every staged image.
		while [[ $# -gt 0 && "$1" != -* ]]; do
			staged_image_targets+=("$1")
			shift
		done
		;;
	--cache-check)
		[[ $# -ge 2 ]] || die "--cache-check needs a mode (quick, provider or targeted)"
		cache_check="$2"
		want_gates=0
		want_conformance=0
		shift 2
		;;
	--fast-path)
		fast_path=1
		want_gates=0
		want_conformance=0
		shift
		# The target, then the artifacts to sample. With none, the script picks its own defaults.
		while [[ $# -gt 0 && "$1" != -* ]]; do
			fast_path_args+=("$1")
			shift
		done
		;;
	*) die "unexpected argument '$1' (try --help)" ;;
	esac
done

# Nothing selected means everything - the CI case, and the one a person means by "check it".
if [[ ${#gates[@]} -eq 0 && $want_gates -eq 1 ]]; then
	gates=("${!GATES[@]}")
fi
if [[ ${#formats[@]} -eq 0 && $want_conformance -eq 1 ]]; then
	formats=("${FORMATS[@]}")
fi

for gate in "${gates[@]:-}"; do
	[[ -n "$gate" ]] && run_gate "$gate"
done
for name in "${refreshes[@]:-}"; do
	[[ -n "$name" ]] || continue
	note "refresh: $name"
	(cd "$SRC_DIR" && eval "${REFRESH[$name]}")
done
if ((staged_image)); then
	note "staged-image: ${staged_image_targets[*]:-every staged target}"
	(cd "$SRC_DIR" && tools/check-staged-image.sh "${staged_image_targets[@]}")
fi
if [[ -n "$cache_check" ]]; then
	note "cache-check: $cache_check"
	(cd "$SRC_DIR" && tools/check-shared-cache.sh "$cache_check")
fi
if ((fast_path)); then
	# The script's own default target when none is named, so the flag alone means what the recipe
	# it replaces meant.
	set -- "${fast_path_args[@]}"
	[[ $# -ge 1 ]] || set -- x86_64
	note "fast-path: $*"
	(cd "$SRC_DIR" && tools/check-fast-path-parity.sh "$@")
fi

note "all selected checks passed"
