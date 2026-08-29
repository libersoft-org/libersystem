# Working in this repository

## Testing: use `./verify.sh`

**After a change, run `./verify.sh`. Do not reach for `./test.sh --arch all`.**

```sh
./verify.sh --plan     # what a change needs verified, and why
./verify.sh            # ...and run exactly that
```

It works out which builds, host suites, gates, conformance runs and per-architecture guest runs a
change actually needs, from a derived dependency model rather than from a guess. Full reference:
[docs/TESTING.md](docs/TESTING.md).

Three things worth knowing before you distrust it:

- **It errs toward running more.** An unrecognised path, a change to the harness, the packager, the
  ABI, the manifest or the selector itself all select everything, and the plan names which and why.
- **A missing plan is never a pass.** If the planner crashes, prints nothing, or prints something
  unparseable, `verify.sh` exits non-zero with `FULL VERIFICATION REQUIRED`.
- **The saving is in the boots it skips, not in the tests it drops.** An emulated target costs 2877 s
  (aarch64) or 6104 s (riscv64) against ~100 s for x86_64, so the expensive decision is which guests
  to boot. Running fewer tests inside a boot that is happening anyway saves ~15%.

Scoped verification of a codec change costs about **2%** of a full one and takes about two and a half
minutes. Reach past it deliberately, not by habit:

```sh
./verify.sh --sweep      # every target, whole suite, one revision, in a git worktree
./verify.sh --release    # the release gate; every optimisation ignored
```

`./test.sh` has no `--for` flag - that question moved to `verify.sh`, which derives its answer
instead of consulting a hand-written table.

## When you add a test

A kernel test with no `covers` declaration is **always selected**, so annotating is a pure saving and
never a risk. If you can name what your test would catch, say so:

```rust
tagged_test!(name, [Tags], covers = ["bin.audioconv", "flac"]);
```

`covers: X` means the test contains an assertion able to detect a regression in **X's contract** -
not that its execution path passes through X.

Crate host suites need nothing: any crate containing a `#[test]` is discovered and gated
automatically, and there are 59 of them and they run in about eighteen seconds together.

## Two habits this tree expects

- **A green test is not evidence until you have watched it fail.** Break the thing deliberately,
  confirm the test catches it, put it back. Several defects in the verification machinery itself were
  found exactly this way, and two of them were tests that passed for the wrong reason.
- **Say what was measured, not what should be true.** Milestone documents in `docs/todo/` record
  numbers, including the ones that showed an idea did not pay.
