IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0170 (2026-09-09T19:00:00Z):

Scope: the ten items of `docs/todo/P02M0170.md`. This record is written as the work proceeds; the
verification section at the end says what was actually run.

## What the tree has, verified before designing

- `harness/test-kernel.sh` already builds the selection-specific test kernel under
  `.build/state/kernel-test-build.lock`, reads the executable cargo named (`--message-format=json`),
  copies it to `.build/state/kernel-test-<arch>.<pid>.elf` while the lock is held, and invokes the
  runner on the copy. The copy is an ordinary writable file, no digest is computed, and nothing
  downstream names it.
- `mkimage.sh` holds `<slug>.image.lock` over its own assembly, computes
  `liber-boot-image-input-v4` over its inputs BEFORE assembly, recomputes it AFTER and dies when the
  two differ. Its inputs are the kernel, the loader EFI, `init-<arch>.pkg`, the bootable volume and
  its uuid sidecar (or `volume-<arch>.pkg` for the test medium), the service manifest, the fallback
  bootstrap set, `product.conf`, `stage-kernel.sh` and itself.
- `mkpackages` publishes every output with `write_if_changed` (temp file, then rename) - atomic per
  file, under no lock, so a consumer copying two inputs can take one from before a publication and
  one from after. `build.sh` takes a lock only around the loader build.
- `qemu-run.sh` hands QEMU the shared content-keyed ISO by PATHNAME (`-cdrom "$iso"`); the USB and
  system disks and the ports' ESPs are already run-private copies.
- `verify.sh --release` is `build.sh --arch all`, three serial `test.sh` runs and `check.sh`, with
  no image build and no evidence beyond per-key history; `check.sh` records a gate's exit status;
  the multi-boot gates keep phase logs in a `mktemp -d` removed on exit.
- The catalog `Check` has `id`, `kind`, `covers`, `variants`, `command`; umbrella gates are excluded
  by design; the two `GuestFallback` rows carry no `covers`.

## Decisions (phase A - the immutable executable and the medium boundary, M1)

- The staged test kernel is published atomically (`.tmp.<pid>` then `mv`), made read-only
  (`chmod 0444`), and digested under the SAME lock; the digest is exported as
  `LIBER_STAGED_KERNEL_DIGEST`, written beside the copy as `<staged>.sha256`, and printed by the
  runner and by the suite's result line so every downstream artifact and log can name it.
- `mkimage.sh` acquires every medium input into a run-private, read-only snapshot directory under
  the producers' lock (`kernel-test-build.lock`, held for the copy only) and assembles from the
  snapshot alone; the input key is computed over the snapshot, and the after-assembly recheck stays
  as the second signal (over the snapshot, which nothing else can move).
- `mkpackages` gains `LIBER_PUBLISH_LOCK`: with it set, every output is written to its temporary
  name first and the WHOLE SET is renamed into place in one `flock`-held step, so a snapshot taken
  under that lock sees a complete generation and never a mixed one. Without it (the gates' private
  output directories) the per-file rename stays.
- `qemu-run.sh` binds the x86_64 medium to the OBJECT: the ISO is opened once, hashed through that
  descriptor, and QEMU is given `/proc/self/fd/<n>` - the inherited descriptor resolves to the
  inode the hash was taken over whatever happens to the pathname during QEMU's startup. The digest
  is printed into the run log.

## Implementation record (phase B - the catalog, the evidence, the release run; M2-M10)

Files and functions, by item:

- **M4 (catalog, frozen set)** - `src/tools/verify-model/src/catalog.rs`: `CheckClass` (Build,
  HostSuite, Profile, Umbrella, Fallback, WholeSuite, KernelTest, Conformance, DevCheck, Producer)
  with `release_required()` decided per CLASS; `Check` gained `class`, `release_required`,
  `prerequisites`, `inputs`, `produces`, `evidence`; `IMAGE_PRODUCERS` (three ISO rows),
  `GATE_IMAGE_INPUTS` (secure-boot, signed-boot, qemu-virtio-iommu-x86_64 - the gates that CONSUME
  a shipping image without assembling it; `dma-mode-x86_64` and `rollback-floor-x86_64` build their
  own media and are their own producers), `DEV_LIFECYCLE_PRODUCER`, the `suite.kernel` WholeSuite
  row, `DEV_LIFECYCLE_GATE`. `evidence.rs`: `derived_release_required`, `release_invariants`,
  `load_required`/`render_required`; `model/release-required.toml` is the checked-in frozen list
  (199 keys: 21 build, 76 host, 79 gate, 11 conformance, 3 image, 3 suite, 5 dev) and `verify-model
  check` compares it with the derived set both ways. Tests: `the_frozen_release_set_equals_...`,
  `deleting_reclassifying_or_replacing_a_mandatory_row_is_detected_against_the_frozen_set`,
  `producers_prerequisites_and_inputs_are_closed_over_the_catalog`.
- **M5 (images are built work)** - `commands::release_steps` emits one producer step per image row
  before the gates that name it in `prerequisites`; `image.sh` (`publish_image_evidence`) stores each
  producer ISO in `<run>/artifacts/` immutable with its `.build-key`/`.build-digest` receipts and
  publishes the producer key; `src/tools/evidence.sh::evidence_image` hands a gate the run's
  artifact and FAILS inside a run when the run produced none; `check-secure-boot.sh`,
  `check-signed-boot.sh`, `check-qemu-virtio-iommu-x86_64.sh` read the image through it and print
  the digest they boot. A gate that assembles media of its own runs with `LIBER_GATE_KEY` set and
  `image.sh` publishes nothing for it.
- **M6 (development lifecycle)** - `src/tools/check-development-lifecycle.sh` (new, `check.sh` gate
  `development-lifecycle`, catalog row `dev.lifecycle`): run-private state under a short `mktemp`
  directory exported as `LIBER_DEV_STATE`, a free host port in `HOSTFWD_PORT`, `./dev.sh up`
  (image build, boot, prompt, agent handshake), the immutable boot artifact and its digest asserted,
  the four development checks in catalog order each publishing its own envelope with the artifact as
  input, teardown from the EXIT trap with the serial/QEMU/up/down logs kept. `lab.py`: `DEV_STATE`
  and every dev-instance path (lock, sockets, logs, boot record, scenario lease, seen-file, pgid
  file, monitor/QMP sockets) under it; `cmd_dev_up` copies the ISO to an immutable private path,
  prints `lab: boot artifact sha256=...` and records `boot_artifact` in the instance identity;
  `run_command(displays, image)`. `qemu-run.sh`: `dev_channel_socket`, monitor/QMP sockets and the
  writable image set (`-dev-<state>` suffix) follow `LIBER_DEV_STATE`, which is also what lets the
  instance through the disk-conflict guard. `perf-gate.py` unseals its probe file for the span of
  its own edit and seals it again (a release snapshot is read-only). `release_steps` carries the
  four DevCheck keys on the lifecycle step, since the checks cannot run against a guest a separate
  step has already torn down.
- **M7 (envelopes)** - `evidence.rs`: `Envelope` (schema `libersystem-evidence/1`), `Run::keep_log`,
  `Run::kept_logs`, `Run::publish` (duplicate refused at the producer), `Run::publish_if_absent`;
  commands `envelope` (`--if-absent`, merges kept logs), `keep-log`, `dossier`, `run-start`,
  `identity`, `release-required`, `release-plan [--required FILE]`. `src/tools/evidence.sh`
  (`evidence_keep`, `evidence_keep_gate`, `evidence_publish`, `evidence_store_artifact`,
  `evidence_image`; no-ops unless `LIBER_VERIFY_RUN` names a run; never fail the producer).
  Producers: `check.sh::run_gate` (captures the gate through `tee`, exports `LIBER_GATE_KEY`,
  publishes `--if-absent`); `harness/test-kernel.sh::publish_suite_evidence` from the EXIT trap
  (whole suite only; inputs = staged kernel and the medium the runner named; logs = run and guest
  logs; discharges = every `[ok]` id); `image.sh`; the lifecycle gate; the multi-boot gates keep
  their phase logs from their EXIT traps (secure-boot, signed-boot, dma-mode-x86_64,
  dma-mode-ports, qemu-arch-profiles, qemu-numa, perf-anchor, smp-core-cap, concurrent-selection,
  rollback-floor, qemu-virtio-iommu); `verify.sh::release_publish_fallback` for a required key
  whose producer published nothing.
- **M8 (identity, binding)** - `identity.rs`: `source_identity` (git-tree for a clean checkout,
  NUL-safe content digest over path, mode and bytes otherwise, plus submodules, lockfiles and
  declared generated inputs), `TOOLS`/`FIRMWARE`/`CONFIGURATIONS`/`PERMITTED_ENVIRONMENT`/
  `GUARDED_PREFIXES`, `undeclared_overrides` refusing the run; the U-Boot default corrected to the
  `u-boot.bin` the runner boots; `LIBER_GATE_KEY` and `LIBER_CONCURRENT_GUESTS` permitted because
  the run's own scripts set them. `qemu-run.sh`: `bind_object`/`bind_tool`/`harness_hold`; the OVMF
  and AAVMF code images, U-Boot, the direct-boot kernel and init package, and the QEMU executable
  (executed as `/proc/self/fd/N` with `exec -a`) on all three targets; `LIBER_HARNESS_HOLD` is the
  fixture hook documented with the other switches. `src/tools/release-snapshot.sh` (create / seal /
  remove) is the one definition of the sealed snapshot.
- **M9 (the release run)** - `verify.sh`: `release_prepare` (clean tree, detached worktree,
  `run-start --out-root` OUTSIDE the snapshot, seal, `LIBER_VERIFY_RUN`/`LIBER_RELEASE_REQUIRED`,
  the executor re-homed into the snapshot), the release plan through the ordinary executor,
  `release_finish` (dossier collected whatever happened, run directory made read-only, a failed
  step or a refusal is a non-zero exit); flags `--out-root`, `--required` (rehearsal), `--in-place`
  (rehearsal). The dossier records the identity block and the publisher version.
- **M10 (fail-closed, reproducible)** - `evidence::collect` refusals: MissingRequired,
  FailedRequired, Duplicate, Unknown, CrossRun, Stale, LogMissing, LogAltered, Unparsable, Schema,
  SourceChanged (the tree's source identity re-read at collection against the run's identity);
  `render` deterministic with the timestamp as its last line. Tests:
  `the_dossier_refuses_missing_substituted_foreign_stale_duplicate_and_corrupt_evidence_distinctly`,
  `kept_logs_are_named_by_the_envelope_and_the_fallback_defers_to_the_producer`,
  `the_release_plan_discharges_every_required_key_once_with_producers_before_consumers`. Shell
  fixtures: `src/tools/check-verify-evidence.sh` (gate `verify-evidence`): a kept log outlives a
  failing producer and its dossier says FAILED and not missing, altered/deleted copies refused by
  name, a real gate's envelope through `check.sh`; a tool and a firmware image replaced at their
  pathnames inside the hold window are not what boots (mutate-use-restore for the two input classes
  the source seal does not cover, with the mutation after the hash and before the use); a sealed
  snapshot refuses an append and a new file, its identity is a Git tree id, a file changed during
  the run refuses the dossier with `source-changed` and restoring it lifts the refusal; the release
  path rehearsed in place over three cheap keys renders a `rehearsal` dossier.
- **M2 (concurrency proves identity)** - `check-concurrent-selection.sh`: each run's staged-kernel
  digest (the suite's `STAGED-KERNEL` line, the runner's `staged kernel sha256=` line in its own
  result log) - present, different between the two runs, and never the other's; the media bound
  by digest and different; the medium handed over through a held descriptor; and the OVERLAP
  OBSERVED by a watcher counting `qemu-system-x86_64` processes above the pre-gate baseline (by
  executable name, `ps -eo comm`), which is a failure of the gate when no such moment occurs.
- **M3** - no selector rewrite; the strict scoped contract is pinned by the existing regressions
  (`trust_lapses_when_the_model_hash_moves`, `evidence_under_another_model_does_not_count`,
  `evidence_under_another_model_does_not_qualify_a_candidate`, `a_refused_candidate_writes_nothing`,
  `the_model_hash_moves_when_a_configuration_changes_meaning`) and `verify.sh`'s SHADOW (exit 4) /
  STALE (exit 5) / `--allow-shadow` epilogue, which the prepared-plan and release paths bypass by
  design. See the verification section for what was actually run.
- Documentation: `docs/TESTING.md` gained "Release evidence".

Decisions worth recording:

- The release plan is produced by the model (`release-plan`) and executed by the ordinary executor,
  which contradicts the old "flat by design" release path. A release REQUIRES the model anyway -
  the frozen set and the catalog's classes are what define it - and reusing the executor gives the
  release the same step ids, prerequisite edges, guest slots and measured costs as any other plan.
- The whole-suite step carries only the `suite.kernel` key; the guest runner's envelope discharges
  the kernel-test keys. The alternative - hundreds of envelopes written by the runner's fallback for
  keys it did not itself observe - would have made the runner claim what only the guest saw.
- `GATE_IMAGE_INPUTS` was narrowed to the three gates that consume a shipping image without
  assembling it. The DMA-mode gate's subject is the signed field each assembly freezes and the
  rollback gate needs media at three generations; declaring them consumers of the release's
  artifacts would have made them boot something other than what they assert about.
- A narrowed required set (`--required`) and an in-place run are REHEARSALS: the dossier's state
  says so and never reads `complete`, so a caller cannot mistake either for a release. This is what
  makes the release path testable in minutes (fixture 4 of `verify-evidence`) without a second
  implementation of it.
- The concurrent gate now REQUIRES observed overlap; whether the two guests overlap on this machine
  depends on the second suite's compile time against the first suite's guest time, and the
  verification section records what was measured.

## Verification (2026-09-09T19:40:00Z)

Commands and outcomes, in the order they were run. Every guest run below was on x86_64 under KVM;
the emulated ports were NOT booted for this milestone (see "not run").

- `cargo test --quiet` in `src/tools/verify-model`: 157 passed, 0 failed (the 148 before this
  milestone plus the class/frozen-set tests, the collector's refusals, the kept-log and
  fallback test, the release-plan test and the M3 contract test).
- `cargo run --quiet -- check` (the model's self-check, also `./check.sh --gate verify-model`):
  "model is consistent" - the frozen `release-required.toml` (199 keys) equals the derived set both
  ways, every invariant holds, check.sh's gate names and the catalog's agree.
- `cargo run --quiet -- release-plan`: 195 steps carrying 199 keys, every required key exactly
  once; the three image producers require `build:x86_64:volume`, the secure-boot, signed-boot and
  virtio-IOMMU gates require `producer:x86_64:image.libersystem-iso`, `gate-after-guest`
  (capability-trace) requires the three whole suites, `dev:lifecycle` carries five keys.
- `./build.sh --arch x86_64` then `./test.sh --arch x86_64 --tags smoke`: PASS, 7 tests; the run
  log names the medium, the firmware image and the QEMU executable by digest.
- `./check.sh --gate concurrent-selection`: PASSED (38 s) - two suites of 49 and 48 disjoint
  `kernel.object` tests, both passed, each naming its own staged-kernel digest in its own log and
  never the other's, two media bound by digest, the overlap OBSERVED by the watcher. Before the
  selections were enlarged the same gate reported no overlap twice: two-test guests exit before
  the second compile finishes, so the gate now requires the overlap and makes it happen.
- `./check.sh --gate source-hygiene,gate-oracles,gate-result-logs,dma-mode-carrier,verify-model,verify-scheduler`:
  all PASSED after three corrections this run surfaced: the kernel's build script joined the
  manifest-reader allowlist (it consumes the manifest through the library and names the file only
  for `rerun-if-changed`); three early-close pipelines in `check-dma-mode-carrier.sh` and
  `check-rollback-floor-x86_64.sh` (written earlier in this session) were rewritten; and the
  scheduler and shadow-log fixtures stage `evidence.sh` beside the `verify.sh` they copy.
- `./check.sh --gate verify-evidence`: PASSED (about three minutes) - (1) the kept log outlives its
  failing producer, the dossier says failed-required and not log-missing, an altered copy is
  log-altered and a deleted copy log-missing, and a real gate through `check.sh` publishes its
  envelope with its captured output; (2) the QEMU executable and the OVMF image were replaced at
  their pathnames while the runner was held after binding, the guest booted to the loader's
  rollback line on the bound bytes, the run log names the digests that ran, and the substitute
  never ran; (3) a sealed snapshot (immutable attribute and read-only mode, ext4, as root) refused
  an append and a new file, its identity was the Git tree id, one file changed during the run
  refused the dossier with `source-changed` and restoring it lifted the refusal; (4)
  `./verify.sh --release --in-place --required <three keys>` started a run, narrowed the plan to
  those keys, published the host suite's envelope from the executor's fallback and the two gates'
  from the gate runner, and rendered a `rehearsal (in place ...; narrowed required set ...)`
  dossier in a run directory made read-only. Eight earlier runs of this gate each found one thing
  to fix, recorded here because each is a defect the gate is for: the envelope file name used
  three `+` per separator; the runner executed the tool as `/proc/self/fd/N`, which names the
  process after the descriptor and blinded every `qemu-system-<arch>` lookup in the tree (fixed by
  the verified-copy form); root is not held by `chmod`, so the seal is `chattr +i`; the verdict
  tool discards the runner's output, so the fixture drives the runner directly; a QEMU asked to
  terminate stayed in its main loop for 28 minutes, so the fixture escalates to KILL; and the
  loader prints its rollback line AFTER `kernel loaded`, so that is the line waited for.
- `./check.sh --gate development-lifecycle`: RAN TO COMPLETION TWICE and FAILED both times on the
  same row. The lifecycle itself holds: private state under `/tmp/liber-dev.*` with a free host
  port, the image built and an immutable copy booted, its digest printed and bound by the runner
  (`qemu-run: medium sha256=` equal to the artifact's), readiness (prompt and agent handshake),
  `dev.selftest` passed (20 s), `dev.proto-test` passed (93 s), `dev.gpu-restart` passed
  (38-39 s), teardown from the trap with the logs kept. `dev.perf-gate` FAILED: the warm leaf
  iteration is over its budgets on this machine (build 13.1 s against 3.6 s, run 6.7 s against
  2.6 s, total 20.7 s against 6.0 s; the no-change build and the proportionality checks pass).
  `.build/state/verify-history.json` records that key at 17 runs, 17 failures before this
  milestone touched it, so this is a pre-existing red row the lifecycle gate now surfaces with
  its own envelope rather than a regression of this work; its budgets belong to the development
  loop's owner. The second run additionally reported "build or packaging recipe changed" - caused
  by this session editing `src/tools` scripts while the instance was up, not by the gate.
- `./check.sh --gate verify-scheduler`, `gate-result-logs`: PASSED (the executor's prepared-plan
  cases and the shadow-log paths, both over the changed `verify.sh`).

Not run, and why:

- `./verify.sh --release` over a sealed snapshot of HEAD for the whole frozen set. HEAD does not
  contain this milestone (nothing is committed), so a snapshot of it would run the previous
  release path; and the full matrix is hours of emulated boots. The release path was verified in
  place over a narrowed set (fixture 4) and the snapshot's seal, identity and tree-moved refusal
  separately (fixture 3). The first full release run is the user's to start once this work is
  committed: `./verify.sh --release --out-root <durable dir>`.
- The aarch64 and riscv64 runner paths (bound firmware, kernel, init package, U-Boot; the
  verified-copy executable) are exercised only by the emulated port gates, which run at the end of
  the whole job with the rest of the long tests.
- `dev.perf-gate` was not made green; see above.
