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
./verify.sh --release    # the release gate: build all, check all, boot all three, nothing skipped
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

## Shadow and trust

A scoped answer should not be believed because it is plausible - and the machinery that will enforce
that is built but **not yet wired into the ordinary run**. Today `./verify.sh` executes its scoped
plan without consulting the trust store; shadow comparison is something you invoke. Until that is
closed (P02M0118 follow-ups), treat a scoped green as good evidence rather than as equivalent to a full
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

## Where the reasoning lives

`docs/todo/P02M0118.md` is the design and the measurements, including what did not pay and the defects
found on the way in. The code comments carry the rest; every non-obvious rule in the model says why
it exists next to what it does.
