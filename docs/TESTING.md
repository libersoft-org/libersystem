# Testing

**One command answers "what does this change need verified": `./verify.sh`.**

```sh
./verify.sh                  # plan and run what the working tree's changes need
./verify.sh --plan           # print the plan, run nothing
./verify.sh --explain        # ...and say why every item is in it
```

Everything else on this page is what that command is made of, and when to reach past it.

**Build once first.** On a fresh checkout there is no kernel test binary, so the model cannot
enumerate which tests exist on which target - and it refuses to guess, which means your first plan
is a FULL one across all three architectures. `./build.sh --arch x86_64` is enough to get scoped
answers back; the plan says so when it happens.

## Why there is a planner at all

The suite is honest and thorough; what it was not is selective. Changing one small tool used to run
more than half of it, and the half it ran was mostly unrelated - so the cost of a one-line change
grew with the size of the system rather than with the size of the change.

The measurement that shaped the fix is worth carrying in your head, because it is counter-intuitive:

| | fixed cost per run | per test | halving the suite saves |
|---|---|---|---|
| x86_64 (KVM) | ~100 s | ~0.2 s | ~15% |
| aarch64 (TCG) | ~1450 s | ~7 s | ~25% |

**Running fewer tests is the smallest lever there is.** The build, the image and the boot are paid
once and dominate everything. What actually pays is not booting an emulated target that cannot be
affected, and not booting at all when a host suite answers the question. A codec change costs about
2% of a full verification, and almost all of that saving is the two emulated boots it skipped.

## The plan

`./verify.sh` asks a model - `src/tools/verify-model/` - and the model answers in one shape:

```
changed paths -> component ownership -> reverse dependency closure -> affected components
              -> checks whose `covers` intersects them -> exact PlanItemKeys -> architecture policy
```

A **PlanItemKey** is `(check, architecture, environment, configuration)`. Each field is a way the
same check can differ: a riscv64 result is not an x86_64 result, a dev-guest run is not a test-guest
run, and a crate's shipping build does not contain the same dependencies as its default one.

The plan is per key; the run collapses keys into commands, because two hundred selected kernel tests
are one boot.

## Everyday use

```sh
./verify.sh --for src/user/libs/audio/flac      # plan for a path instead of asking git
./verify.sh --for-range HEAD~3..HEAD            # plan for what a commit range touched
./verify.sh --json                              # the plan, for anything that is not a person
./verify.sh --age                               # which keys have not run inside the window
./verify.sh --catalog                           # every check that exists and its variants
```

Two commands sit deliberately outside the optimisation:

```sh
./verify.sh --sweep      # the whole suite on all three targets at one revision, in a git worktree
./verify.sh --release    # the release run: one sealed snapshot, every required key, one dossier
```

`--sweep` is not what the age bound does. Stale keys joining the next manual run spreads coverage
over many different trees; a sweep establishes that **one** revision passes everything.

## What it will refuse to do

Every default errs toward running more, and the failures are loud:

- an unrecognised path selects **everything** - unknown reach is tested with everything;
- a change to the harness, the packager, the ABI, the manifest or the selector itself selects
  everything, and the plan names which one and why;
- a planner that crashes, prints nothing, or prints something unparseable makes `verify.sh` exit
  non-zero with `FULL VERIFICATION REQUIRED`. **A failure to produce a plan is never a pass.**

The only way out with nothing to run is that every changed path is declared not code, and the plan
says so per path, with the reason.

## Slow, or stopped?

Two things in the runner exist because that question cost two hours, and neither was answerable from
a log:

- **Every test over a second prints how long it took**: `name...  [ok] (24 s)`. Below a second the
  number is noise and is left out. On the emulated targets this is what tells you a region of the
  suite is expensive rather than broken.
- **A per-TEST watchdog** stops a run when no test has COMPLETED for `TEST_STALL` seconds (default
  900) and names the test that was running. `--timeout` bounds the whole suite, so a run that stops
  on test 83 of 228 otherwise burns the entire remaining budget and then reports the same thing a
  genuinely slow run reports.

The watchdog does not claim more than it knows. "No test completed in fifteen minutes" is not "this
is wedged" - a single riscv64 test can legitimately run into minutes, and if one exceeds the window
the answer is `TEST_STALL=1800` and not a smaller suite. The `[ok] (N s)` figures are what tell you
which it is.

**The trap this replaces, written down because it will be tempting again:** a riscv64 run that
produced no output for ten minutes, with QEMU at 400% CPU and `tlb: shootdown timed out` lines around
it, looks exactly like a livelock. It was not. That target emits shootdown timeouts in every run
including the passing ones, 400% CPU is eight emulated cores doing work, and the region really does
take minutes per test. Check the clock and the load before the diff - and now, check the per-test
timings, which is the line that would have settled it in one glance.

## Reading a guest run's logs

`test.sh` writes two files per run and names both when it finishes:

| | |
|---|---|
| `<stem>-run.log` | the harness's own output: the build, the runner, and on some targets the kernel's serial |
| `<stem>-guest.log` | whatever the guest wrote to the serial device the harness attached |

**Which one carries the test output depends on the architecture**, and the trap is worth naming
because it costs half an hour the first time. On x86_64 and aarch64 the kernel's serial lands in the
GUEST log. On riscv64 it lands in the RUN log, and the guest log holds only U-Boot and the loader -
1359 bytes, identically, on every run, ending at `loader: no GOP framebuffer`. Read that file alone
and a perfectly healthy riscv64 run looks like a kernel hung before its first line of output.

The harness is not confused by this - it greps both - so this only bites someone inspecting the
files by hand. Grep both, or grep the run log first.

## The lower-level entry points

`verify.sh` calls these; reach for them directly when you already know what you want to run.

| | |
|---|---|
| `./build.sh [--arch A] [--part P]` | compile. `verify.sh` puts the builds a change needs INTO its plan and runs them as its first steps |
| `./check.sh [--gate N] [--conformance F]` | host gates and image conformance; no arguments means all |
| `./test.sh [--arch A] [--tags T]` | the in-kernel suite, inside a booted guest |

`./test.sh` has no `--for` flag. It used to, backed by a hand-written path→tag table, and that table
was wrong in both directions: it said `src/fs` was tested with `filesystem,storage,volume` while the
kernel and the loader both statically link LiberFS, and picking any tag that most tests carry
collapsed a one-tool change to half the suite. The question moved to `verify.sh`, which derives its
answer instead.

## Adding to the model

Most of it is derived and needs nothing from you. A new crate, a new `[[bin]]`, a new dependency or
a new manifest provider all appear in the graph on the next run.

What needs a human is in `src/tools/verify-model/model/`:

- **`registry.toml`** - ownership for paths outside any crate, what is not code, which components
  select everything, edges no linker can see (generation, IPC, device), and the architecture policy.
  Every entry carries a reason; the rule for adding one is *if the answer is written down elsewhere
  in the tree, read it there instead*.
- **`configurations.toml`** - what `default` and `shared-image` mean, hashed by content.
- **`regressions.toml`** - real commit ranges with the keys their plan must and must not contain.

Two gates keep it honest, and both run in `./check.sh`:

```sh
./check.sh --gate verify-model         # ownership is total, the graph has no dangling names, the catalog is valid
./check.sh --gate verify-model-tests   # property tests, negative fixtures, the regression corpus
```

## Gates prove they refuse before they approve

Every gate in `./check.sh` starts by feeding itself inputs it must reject, and fails loudly if one
is accepted. This is not belt-and-braces: a validator run only over a currently-valid tree passes,
and would pass identically if it had stopped looking - an `exit 0` at the top, a `grep` whose
pattern no longer matches, a `jq` selector that selects nothing. Several of these gates were found
that way.

Two rules if you add one:

- **Never inject by editing a tracked file.** Copy it, damage the copy, or supply the input through
  an environment override. A self-test killed between the damage and the repair leaves the working
  tree corrupted, with the gate then failing on what looks like a real cause. That happened here.
- **Assert the injection LANDED.** An injection that quietly changed nothing hands the gate a valid
  input, the gate passes, and the self-test reads that pass as a correct refusal. That happened here
  too, inside a self-test written to prevent exactly this class of thing.

### Narrowing the kernel suite

A kernel test with no `covers` declaration is **always selected**. That is the safe default and the
migration path: annotating a test can only make the suite cheaper, never less safe, so it is done a
file at a time.

```rust
tagged_test!(audioconv_converts_across_volumes, [Audio, Service, Storage], covers = ["bin.audioconv", "audioconv", "wav", "flac"]);
```

`covers: X` means **the test contains an assertion able to detect a regression in X's contract**. It
does *not* mean the execution path goes through X - every integration test here runs the scheduler,
the allocator, IPC and the loader, so if touching counted, every test would cover everything and the
selection would be the full suite wearing new metadata. A name the model does not know fails the
gate.

### The suite runs untranslated, and an ordinary run does not

`./run.sh` boots a machine with a `virtio-iommu` in it and every virtio endpoint behind it. `./test.sh`
does not, and the difference is deliberate rather than an oversight.

Putting a controller under the whole suite would change what several hundred tests are testing -
every one of them would then be exercising the translated DMA path as well as its own subject -
without adding a claim that is not already made somewhere sharper. The enforcing profile has its own
gate, `qemu-virtio-iommu-x86_64`, which boots the shipping image under a controller, requires the
kernel to confirm it took the device out of bypass, drives five hostile cases at real memory through
a device told to reach past its mapping, requires a real DHCP lease through the translated path, and
then boots the DEFAULT machine to check that an ordinary run really is the isolated one.

So: a failing test is a failure of what the test is about, and a failure that appears only under
translation belongs to that gate. `./run.sh --no-iommu` reproduces the machine the suite boots on
every target - and on x86_64 boots the image signed for it, `libersystem-no-iommu.iso`, because a
shipping image's DMA mode is a signed field frozen when it is assembled; the ports' per-run media
carry the harness record and need no second image.

Every boot carries that mode, and the runner writes it for every boot whose manifests carry none:
`test.sh` says `LIBER_RUN_MODE=test`, `run.sh` says `public`, the lab says `development` and every
gate says `gate`, each only when the variable is unset, and the value the harness writes follows
from the run mode and the machine - `no-iommu` for the suite, which builds no controller on any
target, and under `--no-iommu`; `enforcing-required` where a virtio-iommu is in the machine, which
is every ordinary boot on all three targets. The suite therefore refuses `virtio_net` by name (it
declares `iommu-required`), and NetworkService comes up WITHOUT A LINK on that row - online, so the
services that depend on it start, and answering every link-bound operation with a typed `io`
refusal; the console says `network: no network provider on this boot` where the DHCP line would
be. The enforcing gates are where the network is proved. The format is `dma-mode-carrier`, the
x86_64 rows are `dma-mode-x86_64`, and the two ports' rows are `dma-mode-aarch64` and
`dma-mode-riscv64` (emulated, slow, last): the enforcing machine on both entry paths, the explicit
degraded machine on both, and the refusals.

The ports' enforcing profiles have gates of their own, one per phase: `iommu-<arch>-<profile>-<phase>`
- `aarch64-direct-gicv2`, `aarch64-direct-gicv3-its`, `aarch64-uefi-gicv2`, `riscv64-direct-aia`
and `riscv64-uefi-aia`, each a `hostile` or `transition` phase on the test kernel (the `edu`
fixture exists only there) and an `ordinary` phase on the built system through `run.sh`. Every
phase requires the kernel to confirm the bypass-off transition, every firmware-touched endpoint to
have confirmed its reset by class before its bus mastering was cleared - a virtio device to status
zero, an xHCI controller halted and reset, an NVMe controller with `CC.EN` clear and `CSTS.RDY`
zero - and keeps the kernel's own census of the bus masters it admitted. The rows (`iommu-aarch64-
direct-gicv2` and so on, and `iommu-ports` for all of them) are umbrellas a person runs by name; the
release obligation is the phases.

The rollback floor has its own x86_64 OVMF gate, `rollback-floor-x86_64` (emulated firmware under
KVM, one persistent variables image across the whole sequence, run last like every other gate that
assembles media): it provisions the floor with the ceremony tool, boots generation N, refuses N-1
from the original and a cloned disk, advances on N+1, deletes either slot and requires N still
refused, boots the interrupted-advance state and the partial provisioning states, runs the recovery
and cross-use manifests, the mixed-generation set, and the pre-policy and non-enforcing loaders
against the rotated signer. The record format, the state table and the compare-and-advance order are
host tests in `bootproto::rollback`; the three trust profiles are told apart by `trust-profile`.

## Shadow and trust

A scoped answer should not be believed because it is plausible - and the machinery that will enforce
that is built but **not yet wired into the ordinary run**. Today `./verify.sh` executes its scoped
plan without consulting the trust store; shadow comparison is something you invoke. Until that is
closed, treat a scoped green as good evidence rather than as equivalent to a full
verification, and use `--sweep` before anything that matters.

```sh
./verify.sh --shadow    # run the FULL suite and compare it against the selection that was NOT run
./verify.sh --trust     # what is TRUSTED under the current model, and what is short
```

Shadow is dry by default: one boot serves both answers. Every test in the selection passing while a
test outside it fails is the shape of a missed edge - reported as a **candidate**, never as a
finding, because this tree has a test on record that failed three times and then passed twice with
no change.

Trust is a certificate bound to a **model hash** over the ownership registry, the dependency graph,
the check catalog, the actual `covers` declarations, the architecture and environment policies, the
configuration catalog and the selector version. Change any of them and every certificate lapses -
evidence proves that a particular selector over a particular model did not miss anything, and a new
model has no evidence yet however clean the old record looked.

## Release evidence

A release is one run over one sealed revision, and it produces one dossier that proves every
release-required key ran - not an umbrella exit code.

```sh
./verify.sh --release                           # a clean tree: snapshot, seal, run everything, dossier
./verify.sh --release --out-root /srv/runs      # publish the run somewhere durable (default .build/runs)
./verify.sh --release --in-place --required FILE  # a REHEARSAL over this tree and a narrowed key set
```

What the run is made of:

- **The release set is frozen.** `src/tools/verify-model/model/release-required.toml` lists every
  required key; the catalog derives the same set from each row's class and `verify-model check`
  compares the two exactly, both ways. Deleting, reclassifying or replacing a mandatory row fails
  before anything runs. Adding a required profile is an edit to that file, reviewed as its own
  artifact - `verify-model release-required --write` regenerates it.
- **The plan is the release plan.** `verify-model release-plan` lowers every required key into the
  executor's own steps: builds, then the three shipping-image producers, the three whole suites,
  every host suite, conformance suite and gate profile, and one development-lifecycle step. A gate
  that boots a shipping image requires the producer that built it; the gate that reads a guest log
  requires the suites.
- **The snapshot is sealed.** A detached worktree of `HEAD`, made read-only except `.build`, so a
  producer cannot consume an edit nobody sees; `--in-place` skips both and is a rehearsal. The
  identity block - the Git tree id (or a content digest of a working tree), the resolved tools by
  hash and version, the firmware images by hash, the compiler configurations, and every permitted
  environment variable - is collected before anything runs. An undeclared `LIBER_*`, `OVMF_*`,
  `AAVMF_*`, `QEMU_*` or `TEST_*` override refuses the run.
- **Producers publish.** With `LIBER_VERIFY_RUN` set, `check.sh` publishes an envelope per gate with
  the gate's captured output, `test-kernel.sh` publishes the whole-suite key with the staged kernel
  and the medium by digest and every `[ok]` test as a discharge, `image.sh` stores each shipping ISO
  in the run (immutable, with its receipts) and publishes it, and the lifecycle gate publishes its
  own key and the four development checks'. Multi-boot gates copy their phase logs into the run from
  their exit traps, before cleanup and on failure too. For a required key whose producer published
  nothing, the executor publishes one from the step's captured output.
- **Everything the guest uses is bound to the object.** The staged test kernel is published
  atomically, read-only, and digested under the build lock; the medium's inputs are snapshotted
  under the producers' lock; and the runner opens the medium, the firmware image, the kernel and the
  QEMU executable once, hashes each through its descriptor and uses that descriptor - so a file
  replaced at its pathname after the hash is not what boots. `LIBER_HARNESS_HOLD` is the fixture
  hook that proves it.
- **The dossier is fail-closed.** `verify-model dossier` refuses a required key with no envelope,
  distinctly from one that ran and failed; a duplicate, an unknown key, an envelope from another run
  or another identity, a kept log that is missing or altered, an unparsable file, and a tree whose
  source identity moved during the run. Re-rendering the same result set is byte-identical apart
  from its last `rendered-at:` line. The run directory is made read-only when the run ends, whatever
  its state.

`./check.sh --gate verify-evidence` is the fixture gate for all of this, and
`./check.sh --gate development-lifecycle` brings the development guest up in run-private state, runs
the four development checks against it and tears it down - `./dev.sh up` by hand is no longer what
those rows depend on.

## Where the reasoning lives

The code comments carry the design and the measurements, including what did not pay and the defects
found on the way in; every non-obvious rule in the model says why
it exists next to what it does.
