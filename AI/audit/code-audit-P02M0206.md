IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0206 (2026-10-26T01:40:22Z):

Started from HEAD a97a9ec0, a clean tree. The plan is `docs/todo/P02M0206.md` as audited 10/10 on 2026-10-25. Work
follows THE ORDER: G1, B1, B7, B3, B6 and C4 first; then B4, B5, B2; then C1, C3, C7, C8, C2, C6, C5; then D1 to D3,
E1, E2, F alongside; A3 after each group; A1 and A2 last; D4 and the final L2 at the very end. Each item's entry below
records what was actually changed, the files and functions, and the verification actually run - passed, failed or not
performed - with the command used.

PROGRESS (updated as items land):

G1 - `.build/logs` keeps what a person or a tool reads (implemented; real-run checks partly done, see below)
  - The plan says `./lab.sh dev-clean`; `lab.sh` refuses every `dev-*` verb and points to `./dev.sh`, so the command
    is `./dev.sh clean`, which execs `lab.py dev-clean`. Implemented there, as the plan requires ("extended, not a
    second pruner").
  - `src/harness/lab.py`: the newest-twenty test-log rule (`KEEP_TEST_RUNS`) is gone. New: `LOGS_ROOT`, `LOGS_LOCK`
    (`.build/state/logs.lock`), `logs_writers` (gate names parsed from `check.sh`'s GATES table, plus the stems of
    the scripts at the top level, in `src/tools` and in `src/harness`, without `check-`), `log_owner` (also accepts the
    name with a `-x86_64|-aarch64|-riscv64` suffix), `logs_documents` (`git ls-files -- docs AI '*.md'`),
    `brace_expansions`, `logs_pins`, `logs_pinned`, `suite_logs_kept`, `tree_mtime`, `logs_removals`, `logs_sweep`,
    `logs_lock`, `logs_exclusive`, `logs_hold`, `logs_summary`; `cmd_dev_clean` gains `--run PID`.
  - The rules, as implemented:
    - test/: per target, the newest ended run (by the UTC start in the name, then the pid); every failed run newer
      than the newest passed one ("passed" = `^test suite complete: [0-9]+ passed` in its run or guest log, the
      line test-kernel.sh:816 requires); the last name-sorted x86_64 guest log containing `captrace: begin` (the
      same choice check-capability-trace.sh:52-70 makes); and every log whose runner pid is alive.
    - Every other top-level folder: a folder named `NAME.XXXXXX` (6-8 characters, mktemp) whose NAME has an owner
      keeps the newest folder of that NAME, by the newest mtime inside it. `bluetooth-independent` and
      `qemu-2d-account`, which make one run folder inside their own (`PER_RUN_INSIDE`), keep only their newest run
      folder. Any other owned folder is a fixed folder and is kept whole. A folder with no owner, and every loose
      top-level file, goes.
    - Pins: every `.build/logs/...` path in the tracked documents, with `{a,b}` expanded. A path with no folder (a
      whole top-level folder) is a pointer and pins nothing. Pins are matched literally, so a naming pattern pins
      nothing. A pinned folder below the top level pins everything inside it. A pinned file is kept wherever it is,
      and its unpinned siblings go. Folders are removed only once they are empty.
    - When `check.sh` or the document list cannot be read, the sweep removes nothing and says so.
  - THE LOCK: `dev-clean --run PID` (the automatic form) takes the lock exclusively without waiting. If it gets it,
    the logs rules run and nothing else. Either way it then takes the lock shared, and forks a detached holder
    (`setsid`, stdio on /dev/null, every other descriptor closed) that keeps the lock until PID ends. It waits on a
    pidfd, as `run-files.py reap` does. The hand form (`./dev.sh clean [--dry-run]`) takes the lock exclusively
    without waiting, refuses with "a verification run holds the logs lock (.build/state/logs.lock)" while a run holds
    it, and otherwise runs the logs rules and every other rule as before.
  - IMPLEMENTATION DECISION (beyond the plan's text): the holder applies the logs rules once more after PID has
    ended, if it can then take the lock exclusively (that is, if no other run is alive).
    - Why: the plan's oracle says ten consecutive runs of one gate leave ONE kept set. With a sweep only at the start
      of a run, the tenth run's own set is written after its sweep, so two sets would remain: the kept one and the
      tenth run's.
    - The sweep at the start stays as planned. The extra one also runs only when the run is alone, so it keeps the
      plan's safety property.
    - It mirrors `run-files.py`'s sweep and reap pair (`run-files.py:11-20`), which the plan cites for this rule.
  - `lib.sh`: `logs_run_begin`. If `LIBER_LOGS_RUN` names a live pid, the run is nested and it returns. Otherwise it
    exports `LIBER_LOGS_RUN=$$` and runs `lab.py dev-clean --run $$`, noting a failure and going on. It is called:
    - in `verify.sh` for `run` and `shadow` actions, before `release_prepare`, so the lock is the tree's own;
    - in `test.sh` after argument parsing;
    - in `check.sh` before its gate loop.
  - `src/tools/verify-model/src/identity.rs`: `LIBER_LOGS_RUN` is added to `PERMITTED_ENVIRONMENT` (24 -> 25).
    Without it, `run-start` and `identity` refuse every release run, because `verify.sh` exports the marker before
    `run-start` ("undeclared environment override"). The test
    `undeclared_environment_overrides_are_refused_and_permitted_ones_are_recorded` (tests.rs) now includes it, and was
    watched failing with the entry removed: `left: ["LIBER_LOGS_RUN", "LIBER_SECRET_KNOB", "OVMF_FANCY"]`.
  - `src/tools/build-shared.sh`: `cargo-<target>.jsonl`, `cargo-<target>.stderr` and
    `services-seed-<target>.stderr` move from `$build_root/logs` to `$build_root/state`. The `mkdir` of
    `$build_root/logs` is dropped, since the build writes nothing there any more. The comment on `.build`'s shape is
    updated.
  - Tests (`src/harness/harness-test.py`, gate `boot-harness`):
    - `LogsRulesTest`, six tests: the newest run kept and older ones removed; an older x86_64 trace kept beside a
      newer log without one; failed runs kept until a later pass; a live run's logs left alone; pinned files kept
      and unpinned siblings removed, with pointers, patterns, brace pins and a pinned run folder; each gate's newest
      set, fixed folders whole, orphans and loose files removed.
    - `LogsLockTest`, four tests, run through copies of `lab.py`, `lib.sh` and `dev.sh` in a fixture repository:
      - a run sweeps nothing while another run holds the lock, and sweeps once it is alone;
      - `dev.sh clean` refuses while a run holds the lock, and cleans afterwards;
      - the automatic sweep leaves an unreferenced object generation in a fixture `.build/cache` and day-old
        `.build/tmp` scratch alone, while the hand clean still removes the stale object;
      - ten consecutive runs of a stand-in gate (`lib.sh`, `arm_run_verdict`, `logs_run_begin`, one
        `qemu-2d-demo.XXXXXX` folder per run) leave one folder.
  - WATCHED FAILING: each rule was broken in turn by a scripted one-line mutation, the tests were run, and the source
    was restored (driver in the session scratchpad). Every mutation below was caught:
    - no capability-trace rule;
    - failed runs not kept;
    - live runs not kept;
    - no pins;
    - a top-level folder pins;
    - a pattern pins (glob matching);
    - braces not expanded;
    - a pinned folder pins nothing inside;
    - per-run folders all kept;
    - orphans kept;
    - per-run-inside folders all kept;
    - no lock (always exclusive);
    - `--run` runs every rule;
    - the holder makes no final sweep (the ten-runs test then finds 2 folders);
    - `lib.sh` never sweeps (it finds 10).

    Two mutations were not caught on the first version of the code ("a top-level folder pins", "a pattern pins").
    The pointer rule was enforced twice, and the pattern rule had no observable effect. The code now enforces each
    rule in one place, the pin test gained an oldest run that only a glob would pin, and both mutations are caught.
  - Verification run on the real tree:
    - `python3 src/harness/harness-test.py`: 116 tests OK.
    - `./check.sh --gate boot-harness`: RESULT ok.
    - This was the first automatic sweep on the real tree. It removed 858 files (198,657 kB): the orphans (dma-*.log,
      development-lifecycle-acceptance*.log, release-integrity-host.log, the old image-graph cache files, the unpinned
      files of bt-pan-389ba, ipmi-oob-debug and p02m0177), older per-run demo folders, unpinned qemu-2d-account runs
      and old suite logs.
    - `.build/logs` went from 2,600 files and 530,116 kB to 1,742 files and 329,072 kB. Most of what is left is
      pinned by documents and audits.
    - `lab.py dev-clean --run <ended pid>` on the real tree takes 0.19 s.
  - "A build straight after a sweep still hits its image-graph cache": checked on the real tree.
    1. `LIBER_VERBOSE=1 ./build.sh --arch x86_64 --part libs`: 872 s, "Cargo image graph cache miss". This is the
       first build with the new paths, and build-shared.sh's own digest changed too.
    2. The sweep: `./check.sh --gate boot-harness`.
    3. The same build again ended in 2 s on its warm image snapshot, which is consulted before the graph. So the warm
       snapshot record (`.build/state/warm-x86_64-unknown-none.state`) was moved aside and the build run again: 63 s,
       "Cargo image graph cache hit" (graph stage 1 s). The build then rewrote the snapshot record.

    Watch-fail evidence: the first sweep's removal list contained the cache's old files in `.build/logs`
    (`cargo-<target>.jsonl` and `.stderr`, `services-seed-<target>.stderr`, for all three targets). A cache left
    there would not have survived the sweep.
  - NOT YET RUN: "`capability-trace` still finding its trace after a later `./test.sh --tags numa`" on the real tree.
    It needs an x86_64 rebuild, since the `src/tools/*.rs` edits moved the volume digest. It is batched with the real
    gate runs of B3/B4/B5/B7. Its host test equivalent (LogsRulesTest) passes and was watched failing.

B1 - `guest-console.py` ends when the console closes (implemented, host-verified)
  - `src/harness/guest-console.py`: an empty `recv` result without a timeout is a closed console. The driver writes
    `# guest-console: the console closed` into its log and stops recording, instead of spinning to `--seconds`.
  - Test: `GuestConsoleTest.test_the_driver_ends_when_the_console_closes` (harness-test.py). A fake guest on a UNIX
    socket prints the attach line, then a prompt, takes the typed line, prints a last line and closes.
    - New driver: OK in 0.9 s, the log holds the typed line and the last line.
    - HEAD's driver: FAILED, "60.05 not less than 20 : the driver spun on a closed console to its deadline".

B7 - the guest stops when its console driver does (implemented, host-verified; real before/after runs pending)
  - `src/tools/guest-gate.sh` `guest_gate_run`:
    - `timeout "$deadline" ./run.sh ...` now runs in the background (`guest_gate_guest`) beside the driver.
    - When the driver has exited, the `timeout` process is sent TERM, which it passes to its whole process group
      (the runner and QEMU). The guest is then waited for.
    - `guest_gate_cleanup` also kills the guest.
    - `GUEST_GATE_WINDOW=keep` leaves the guest to its deadline, as before. `check-power-ups.sh` and
      `check-brightness-usb.sh` set it, because their verdicts read `ups-sim.log` and `monitor-sim.log`, until B2
      names their completion lines.
    - The four gates that read QEMU's own output (`camera-usb`, `firmware-tree`, `usb-legacy`, `virtio-gpu-edid`)
      were checked: each only copies `run` into its kept folder.
  - Test: `GuestGateStopsWithItsDriver` (check-guest-verdict.py, gate `guest-verdict`). The stand-in `run.sh` never
    ends and has a `sleep 300` child standing in for QEMU.
    - New code: returns within seconds, and both processes are gone.
    - HEAD's guest-gate.sh: FAILED, "60.03 not less than 30 : the guest ran on to GUEST_GATE_TIMEOUT".
    - A kept window still runs to its deadline.
  - The existing `GuestGateImagePreparation` fixture had to change. Its stand-in driver wrote the log and exited at
    once, so under B7 the stand-in runner was stopped before it wrote its record (5 failures, 2 errors on the first
    run). Its stand-ins now behave like the real ones: the runner creates the console socket, and the driver records
    only once the socket exists. No assertion changed.
    - With HEAD's guest-gate.sh, all 7 old tests pass and only the new B7 test fails.
    - `./check.sh --gate guest-verdict`: 104 tests OK.
  - NOT YET RUN: "`usb-release` and `bluetooth-le` give the same verdict before and after on one tree" (real x86_64
    gate runs). Batched with the other real runs.

B3 - the staging says its profile, and the bootable volume its DMA mode (implemented, host-verified; real-tree run pending)
  - New `src/tools/staging-state.sh` (mode 755, like the other sourced helpers). It is sourced by `lib.sh`,
    `qemu-run.sh`, `mkimage.sh`, `test-kernel.sh` and `check-development-gate.sh`, and defines:
    - `STATE_REFUSED=75`, the status of its own;
    - `run_profile`: `development` with `LIBER_DEVELOPMENT=1`, else `shipping`;
    - `staging_build_command`;
    - `record_staging_profile`;
    - `staging_profile_refusal`;
    - `volume_mode_refusal`.
  - THE RECORD: `build.sh` writes `.build/state/built-<arch>-<part>.profile` beside its per-part source stamps, for
    `libs`, `user` (which also records `libs`, since `step_user` builds them), `packages` and the volume part
    (`volume` or `volume-test`, the same name its stamp has). `image.sh` stamps through the build.sh it runs.
  - THE REFUSALS (exit 75; each names the part or volume, the profile or mode needed, and the command that builds
    it):
    - `test.sh` `require_built`: after the source-digest check, every one of `libs user packages volume-test` must
      be in `run_profile`. A missing record refuses too.
    - `qemu-run.sh`, before the per-arch dispatch: a boot that reads the staging must find its parts in
      `run_profile`.
      - ports: `libs user packages volume-test`, or `libs user packages volume` with `SYSTEM_DISK`;
      - x86_64 without `BOOT_IMAGE`: the same parts, with `volume-test` under `TEST=1` and `volume` otherwise;
      - an x86_64 `BOOT_IMAGE` carries its own volume and is not checked.
      Checked first: every development-profile caller passes `LIBER_DEVELOPMENT=1` in its boot environment (lab.py
      dev-up and cold runs, check-system-suspend.py, check-tickless-idle.py, check-hibernate-ports.py,
      check-bluetooth-independent.py, guest_gate_development).
    - `qemu-run.sh` DMA mode, where a bootable volume is paired with a guest:
      - ports (`qemu_port_volume_pairing`, `SYSTEM_DISK`): the record must be `harness`, because the port medium is
        always signed `--dma-mode harness` (`stage_signed_boot_manifest`). Its three callers now pass the status on
        (`|| exit $?` instead of `|| exit 1`).
      - x86_64: when a new `RUN_DISK` would be seeded from `system-volume-bootable-x86_64.img`, its record must equal
        the medium's signed mode.
    - `mkimage.sh`: the old `die` (exit 1) for a volume signed for another mode is replaced by `volume_mode_refusal`
      and exit 75. A missing `.dma-mode` record now refuses the same way. A missing image or uuid still dies (exit
      1).
    - `check-development-gate.sh`: after `--manifest-only` and before anything is built, the volume package's
      `packages` record must be in `run_profile`. A package whose record and content disagree still fails check 4.
      This gate does not boot. It is included because the plan names its failure on "not the shipping
      configuration" among the profile-switch failures.
  - NO ENVELOPE, NO HISTORY:
    - `test-kernel.sh`: a runner status of 75 with a `qemu-run: REFUSED:` line is passed on as 75, and
      `publish_suite_evidence` (the EXIT trap) publishes nothing (`SUITE_REFUSED=1`).
    - `check.sh` `run_gate`: status 75 publishes no envelope, notes "REFUSED to run", and returns 75. `check.sh`'s
      exit then carries it.
    - `verify.sh`: `run_one_step` keeps the step's real status; it used to flatten every failure to 1.
      `record_one_step` on 75 notes "REFUSED: <label>", counts the step failed for the run's verdict and for
      blocking, and publishes no fallback envelope, files no tier outcome and records no history. Until B4 the
      refused step is not run again.
  - Fixture copy lists that copy the changed scripts now also copy `staging-state.sh`:
    - check-verify-scheduler.sh (the learning fixture);
    - test-shadow-logs.py;
    - check-run-verdict.sh (`private_root`);
    - check-guest-verdict.py (KernelBuildOnly, PerfImageIsolation; both failed until then);
    - harness-test.py (LogsLockTest).
    `logs_run_begin` returns at once in a tree without `src/harness/lab.py`, because those fixtures copy the entry
    scripts without the harness.
  - Tests:
    - `check-run-verdict.sh`, new case "a staging in the other profile": a private root with a current digest stamp
      and the four parts recorded `development`. `env -u LIBER_DEVELOPMENT test.sh --arch x86_64 --build-only` ->
      `RESULT failed exit=75` in 1 s, with "REFUSED: the x86_64 staging's 'libs' was built in the development
      profile, and this step needs the shipping one". Watched failing on HEAD's test.sh: exit 127, past the
      pre-flight into the missing harness (on a real tree that is the compile and the boot).
    - `check-verify-scheduler.sh`, new case 7: a prepared plan with a refusing step (`exit 75`) and a failing step,
      both with KEY lines and both in a fixture `LIBER_RELEASE_REQUIRED`, run under a fixture `LIBER_VERIFY_RUN`
      through a private copy with a recording `cargo`. The refused step is reported REFUSED and gets no envelope
      call and no `record` call; the failed step gets both; the run does not pass. Watched failing on HEAD's
      verify.sh: the refused step is not reported as refused, and it published an envelope and recorded history.
    - qemu-run.sh on the real tree (no QEMU started): `TEST=1 LIBER_RUN_MODE=test bash harness/qemu-run.sh x86_64
      <kernel>` and the same for aarch64, without `LIBER_DEVELOPMENT`, both exit 75 in 0 s. x86_64: "'packages'
      carries no profile record"; aarch64: "'libs' carries no profile record". No build has written the remaining
      records yet. `libs` and `user` on x86_64 were written by the run-verdict gate's own real `./build.sh --part
      libs,user`.
  - Gates run: `./check.sh --gate run-verdict,verify-scheduler,guest-verdict,boot-harness` -> RESULT ok (143 s);
    `./check.sh --gate source-hygiene` -> clean. The first hygiene run failed on the new file's mode 644, fixed
    to 755.
  - NOT YET RUN on the real tree: a real development staging followed by `./test.sh` refusing in seconds; a
    release rehearsal with a refused step. Batched with B4's real runs, which need the same states.

B6 - the gates that cannot pass are made able to (implemented; per-gate status below)
  1. concurrent-selection - `src/tools/check-concurrent-selection.sh`
     - Change: the `kernel.object` ids are no longer grepped from the source. The gate first compiles the whole
       x86_64 test kernel (`harness/test-kernel.sh x86_64 --build-only`), then reads its ids with a new model-free
       command, `verify-model kernel-tests --arch A`. That command calls the planner's own
       `kerneltests::discover` (nm over the test binary), takes 4 s, and refuses when no test binary exists.
     - Old reason, demonstrated: on the real tree the source declares 121 `kernel.object.*` ids and the x86_64 test
       kernel carries 120. The one missing is `kernel.object.port_range.a_machine_with_no_port_space_mints_nothing`,
       which only the ports compile; one of the two selections always contained it.
     - NOT YET RUN: the gate itself, which needs two x86_64 guests at once and may meet C3's port-5555 collision.
       Batched.
  2. signed-boot - `src/tools/check-signed-boot.sh`
     - Change: `medium_mode="$(src/harness/image-dma-mode.sh "$ISO")"`, and `resign_with` passes `--dma-mode
       "$medium_mode"` before the case's own arguments.
     - Old reason, demonstrated without a boot: the manifest rows re-signed exactly as `resign_with` did, from the
       ESP of `.build/boot/libersystem.iso` (built 2026-10-21, mode `enforcing-required`). Without `--dma-mode`,
       exit 1 with "--dma-mode is required"; with the medium's mode, it signs and verifies.
     - NOT YET RUN: the gate's boots, which need a current `./image.sh --format iso`. Batched.
  3. verify-evidence - `src/tools/check-verify-evidence.sh`
     - Change: `unset LIBER_RELEASE_REQUIRED` before its first nested run, with the reason. Its own envelope is
       published by the outer check.sh, which does not read the variable.
     - Old reason, reproduced: `LIBER_RELEASE_REQUIRED=<release-required.toml> verify-model run-start` refuses with
       "undeclared environment override(s) ... LIBER_RELEASE_REQUIRED".
     - Watched failing: HEAD's gate run with that variable set stops at step 1 ("run-start refused").
     - Passed: the new gate run the same way passes all four steps, including the nested in-place release rehearsal
       (run 20261026T032024Z-..., three envelopes, sealed).
  4. perf-anchor - `src/tools/verify-model/src/catalog.rs`
     - `IMAGE_PRODUCERS` becomes `(id, command, artifact, volume)`. Each image producer also produces the bootable
       volume it leaves in the staging (`artifact:system-volume-bootable-x86_64.<mode>`).
     - `GATE_IMAGE_INPUTS` gains `("perf-anchor", &["artifact:system-volume-bootable-x86_64.enforcing-required"])`.
     - A gate's prerequisites are the producers whose ISO or volume it names.
     - Test: `the_release_plan_discharges_every_required_key_once_with_producers_before_consumers` now also requires
       perf-anchor's release step to require, and come after, `producer:x86_64:image.libersystem-iso`. Passes;
       watched failing with the new `GATE_IMAGE_INPUTS` row removed (panic at the requires assertion).
     - NOT YET RUN: perf-anchor's own boots. Batched.
  5. tickless-idle on riscv64
     - Established first:
       - The wake counters key a device interrupt by "the identity the architecture dispatches it under" (IDT
         vector, INTID, interrupt-file identity; `idle/mod.rs:74-75`).
       - On riscv64 the kernel's console handler is armed on `WIRED_EID`, and the boot line says so (`main.rs:1459`).
       - A claim that takes the UART lets the APLIC source go (`release_line`), and `bind_wired_inner` arms it to
         `LINE_EID_BASE + slot`, an identity of the claim's own window. No line named that identity. The handoff
         line (`console_uart/mod.rs:601`) prints the APLIC source number, and it prints before the binding exists,
         so it cannot carry the identity.
       - x86_64 binds a legacy GSI at `IRQ_BASE + GSI`, the vector the kernel's handler used. aarch64's INTID does
         not change. So only riscv64 changes identity, as the plan says.
     - DEPARTURE FROM THE PLAN'S WORDING: "the handoff line gains it" is not possible, because the handoff precedes
       the binding. Instead the binding says it:
       - `src/kernel/arch/riscv64/interrupts/mod.rs` `bind_wired_inner`, when the bound source is the console's,
         prints "console: typed input raises interrupt {eid} now, through the claim that holds the UART";
       - `src/kernel/arch/riscv64/serial.rs` gains `console_source()`, for both builds.
     - `src/tools/check-tickless-idle.py`: new `HOLDER` pattern and `console_identity(log, armed)`. After the boot
       settles, the identity is the last holder the console named: the claim's identity, or the boot's again after
       "is the kernel's again". A changed identity is noted.
     - Checked:
       - `console_identity` on synthetic logs: claim -> 33, handed back -> 16, no handoff -> 16.
       - `cargo check --target riscv64gc-unknown-none-elf` for the kernel, and `TEST=1 TEST_TAGS="" cargo check
         --tests` for the same target: both clean, no warnings.
     - NOT YET RUN: the riscv64 gate. It is an emulated-target run, which this job's rule keeps for the very end.
  6. `verify.sh --release --plan` - `verify.sh`
     - The answering actions (`plan json catalog model-hash age trust`) with `--release` no longer run
       `release_prepare`: no snapshot, no run directory.
     - `case "$mode"` gains `release) changed=""`, and the "N changed path(s)" note is skipped for a release.
     - `--release --plan` prints `verify-model release-plan`, with `--required` resolved to an absolute path as the
       run does. `--release --json` refuses with a reason; the release plan has no JSON form.
     - The release-plan narrowing moved into a `release_plan_steps` helper in main.rs, shared with `account` (A3).
     - Watched failing: HEAD's `./verify.sh --release --in-place --plan` created a run directory and printed
       "changed: unbound variable" twice (exit 3). The stray run directory, run-id and identity.json only, was
       removed.
     - Passed: the new `--release --plan`, `--release --in-place --plan` and `--release --plan --required
       <release-required.toml>` each print 373 steps / 377 keys, exit 0, and create no run directory and no snapshot.
       `--release --model-hash` and `--release --age` exit 0 with no snapshot either. That is E2's second half, done
       here because it is the same branch.

C4 - test.sh's guard knows every private file qemu-run.sh creates (implemented, host-verified)
  - `test.sh` `require_no_stray_qemu` now also admits, owned by the live ancestry:
    - `usb-media*.<key>.legacy.<pid>.img`;
    - `logs/test/<stem>-usb-audio.pcap`;
    - `virtio-serialport*.<pid>.out`;
    - `uas-scratch`, `nvme-scratch`, `nvme-second`, `scsi-scratch`, `scsi-second`, `scsi-third`, `sd-scratch` and
      `ahci-scratch.<pid>.img`;
    - `nvdimm-[01].<pid>.img`.
    The list was taken from every `$$`-named path in qemu-run.sh that QEMU holds writable.
  - Test: `check-guest-verdict.py` `ExistingQemuAdmission`, two new tests over the 13 names.
    - Each is admitted with its owner's pid, directly and from a child.
    - Each, with another pid, is still refused.
    - Watched failing on HEAD's test.sh: 13 failures, one per name. With the change: 5 tests OK.

A3 - where the remaining time goes, after the first group (measured; recorded in the plan as well)
  - New `verify-model account [--jobs N] [--required F]`, backed by `history::Account` (`Account::of`, `total`,
    `floor`). Over the release plan (the L2 plan) it sums what each step last measured, under any model. It splits
    the sum into steps that run alone (no guest, or an exclusive guest step) and guest work that shares the slots,
    and prints the longest step, the sum over the width, the floor (remainder plus the larger of the longest shared
    step and the shared work over the width), the 15 dearest steps and every unpriced step.
  - Test: `an_account_separates_the_serial_remainder_from_the_shared_guest_work`. Watched failing with exclusive
    guest steps counted as shared ("left: 100.0").
  - Measured 2026-10-26 from the prices on disk. Nothing has been re-measured since the 2026-10-20/21 runs, so B1 and
    B7's effect is not in these numbers yet; the gates have not run under the runner since.
    - 373 steps: 331 priced (all under an older model), 42 never measured.
    - Sum 71,026 s (19.7 h).
    - Serial remainder 56,930 s (15.8 h); shared guest work 14,096 s (3.9 h).
    - Longest step: `gate-guest:host:hibernate-ports`, 6,842 s.
    - At width 1: sum over the width 19.7 h, floor 19.7 h. At width 2: sum over the width 9.9 h, floor 17.8 h.
  - The largest remaining cost is the serial remainder, mostly exclusive guest gates (hibernate-ports 6,842 s,
    boot-volume-controllers on both ports 3,901 s each, hibernate 2,795 s, watchdog 1,864 s, ipmi 1,743 s). The items
    on it are B2 (completion lines), B4 and B5 (the profile switches inside every x86_64 development gate), C1/C2
    (overlap) and C8 (hibernate-ports split per port), so THE ORDER stands: B4, B5 and B2 next.

B4 - the staging is switched when a step needs another state (implemented, host-verified; real runs below)
  - `src/tools/staging-state.sh`:
    - every refusal also appends the command that fixes it to `$LIBER_STATE_REFUSAL` when that is set
      (`state_refusal_fix`);
    - the fix commands carry the profile (`profile_env`, `staging_build_command`, `volume_build_command`);
    - `record_staging_profile` notes a change of profile: "staging: the <arch> staging switched to the <profile>
      profile (<parts>)".
  - `qemu-run.sh` collects every refusal of one boot before it refuses: the profile, and on an x86_64 run with no
    `BOOT_IMAGE` and no `TEST` the bootable volume's mode against `${LIBER_DMA_MODE:-enforcing-required}`. One
    refusal then names every state the boot needs, and a runner fixes them together before the one retry.
  - `verify.sh`:
    - exports `LIBER_STAGING_RUNNER=1` ("under verify.sh, which says so in the environment");
    - `run_one_step` gives every step `LIBER_STATE_REFUSAL=<outfile>.refusal`;
    - `record_one_step`: on 75, with a fix and no retry yet, the step is retried by `retry_refused_step` - at once
      in the foreground, or queued (`state_queue`) and retried at the end of `drain_guests` when it was a
      backgrounded guest, because other guests may be reading the staging the fix rebuilds;
    - `retry_refused_step` runs the deduplicated fixes, then the step again (the command is kept in
      `step_commands`), and records that run as the step's. A failed fix, or a second refusal, is a failure with
      no envelope and no history;
    - `staging_fingerprint` (every `built-*.profile` and every `system-volume-bootable-*.dma-mode`) is taken
      before the loop, and `restore_staging`, after the last barrier, runs only when it changed. It puts every
      target with a non-shipping part back to shipping and re-signs the x86_64 bootable volume
      `enforcing-required` when it is in another mode. It is skipped for a sealed snapshot.
    - IMPLEMENTATION DECISION: "restores ... once, when the run ends" is applied only when the run changed the
      staging. Otherwise every prepared-plan run of the verify-scheduler gate, and every run over a staging the
      user left in development, would trigger a full rebuild that no step of it asked for.
  - `src/tools/guest-gate.sh`: under `LIBER_STAGING_RUNNER=1` a gate never marks the shipping staging for restore.
    Run by hand it still rebuilds it on the way out.
  - `identity.rs`: `LIBER_STAGING_RUNNER` and `LIBER_STATE_REFUSAL` are permitted (27 entries). Nested `identity`
    and `run-start` calls inside steps see both.
  - Tests:
    - `check-verify-scheduler.sh` case 8 (prepared plans through the private copy with the recording cargo):
      - refused once with a fix: the fix runs, the step runs once after it, and gets one passed envelope and one
        passed record;
      - refused twice: nothing published or recorded, and reported refused;
      - a fix that fails: the same;
      - the run with them fails;
      - with `--jobs 2`, a guest refused while another guest sleeps 2 s is retried only after that guest ended
        (order "ended retried"), and the run passes.
      Watched failing:
      - with the retry removed: four checks of the once-refused step fail, the guest is never retried and the
        run fails;
      - with the retry made immediate in the background context: order "retried ended".
    - `check-guest-verdict.py` `GuestGateImagePreparation.test_the_shipping_staging_is_put_back_by_hand_and_left_under_the_runner`,
      with a `build.sh` stub recording calls: by hand from the shipping profile, one restore (`--arch x86_64`,
      LIBER_DEVELOPMENT unset); under the runner, none. Watched failing on HEAD's guest-gate.sh: one restore under
      the runner.
  - Real-run oracles:
    - (a) two x86_64 development-image gates and then the x86_64 suite under verify.sh: in the x86_64 batch below.
    - (b) the release rehearsal with a refused suite: needs the narrowed release plan, whose closure builds
      aarch64 and riscv64 (all 21 build steps). That is a port build, so it moves to the end-of-job runs.
    - (c) the suite by hand right after `lab.sh scenario-cold`: in the batch.

B5 - one development ISO for all gates (implemented, host-verified; real runs below)
  - `src/harness/mkimage.sh`: `iso` with `LIBER_IMAGE_KEYED=1` writes `$BUILD/$SLUG$DMA_SUFFIX.<key>.iso`
    (libersystem-dev.<key>.iso for `--dma-mode harness`). Its existing hit check (key file plus verified digest)
    then reuses it while the key matches.
    - The generation sweep, which was testiso-only, now covers these keyed media too: only names with a 64-hex key,
      older than an hour and not held open.
    - The installable `libersystem-dev.iso` is never written by a gate.
  - `src/tools/guest-gate.sh` `guest_gate_prepare_image`: `LIBER_IMAGE_KEYED=1 image.sh --format iso --dma-mode
    harness`. Standard output is captured on its own (the path mkimage prints last), and stderr goes to the log; it
    does not rely on the sourcing gate's pipefail.
  - No private copy is kept for "a gate that writes to its medium": none does. qemu-run.sh opens the ISO read-only
    and attaches it as `-cdrom /proc/self/fd/N`, and a persistent RUN_DISK is seeded from it as a separate file.
  - Tests:
    - New `KeyedGateMedium`: the real mkimage.sh, with make_iso, verify_boot_artifacts and image_input_key stubbed
      as PerfImageIsolation stubs them. It is run three times (inputs first, first, second):
      - first, a keyed path and a miss;
      - again, the same path and "cache hit";
      - second, a new path;
      - both generations present, and the installable `libersystem-dev.iso` untouched.
      Watched failing on HEAD's mkimage.sh: the medium went to `.build/boot/libersystem-dev.iso`, the installable
      image's own name.
    - `GuestGateImagePreparation`: the image.sh stub now answers a keyed medium and builds only when that key's
      medium is absent.
      - The old "one private image" test becomes "one keyed image serves both boots": the gate asks with
        `LIBER_IMAGE_KEYED=1` and no `LIBER_IMAGE_OUTPUT`.
      - New `test_two_gates_in_a_row_build_one_medium_and_a_change_builds_a_second`: built, reused, built again
        after the change; the first two gates boot one medium.
      Watched failing on HEAD's guest-gate.sh, which asked for a private medium.
  - Gates run after B4 and B5: `./check.sh --gate
    guest-verdict,verify-scheduler,run-verdict,boot-harness,source-hygiene,verify-model,verify-model-tests` ->
    RESULT ok (388 s).

The x86_64 batch of real runs (2026-10-26, 03:56-04:47Z), three phases on one tree
  - Phase A, B7 reverted (HEAD's guest-gate.sh copied in for this phase only, restored after - `cmp` confirmed):
    `LIBER_VERIFY_STEPS` plan of build x86_64, usb-release, bluetooth-le.
    - build 62 s; usb-release PASS 452 s; bluetooth-le PASS 409 s; the run's end restore rebuilt the staging in the
      shipping profile (122 s) and re-signed the bootable volume `enforcing-required` (60 s). Run 1106 s.
  - Phase B, the tree as implemented: usb-release, bluetooth-le, the whole x86_64 suite, the numa tag,
    capability-trace, the shipping image, signed-boot, perf-anchor.
    - B7: usb-release PASS 322 s (452 before), bluetooth-le PASS 238 s (409 before), the same verdict lines.
    - B5: both gates booted the one keyed medium phase A had built
      (`libersystem-dev.5e407b87….iso`, no second medium written).
    - B4(a): the profile changed twice - into development at usb-release's image, back to shipping when the suite
      refused (`test.sh: REFUSED: the x86_64 staging's 'libs' was built in the development profile ... exit=75
      seconds=1`, then `verify.sh: REFUSED: ... bringing the staging to the state it needs`, the fix 120 s, the
      suite's second run PASS: 585 passed, 456 s). No restore at the end: the fingerprint equalled the start's.
    - G1: `capability-trace` passed after a later `./test.sh --tags numa` (16 passed, 34 s) - it found its trace in
      the suite's guest log, not in the newer numa one.
    - B6: signed-boot - every x86_64 case passed, each refused for its own reason (product, architecture, source
      kind, pairing, mixed releases, the downgrades). The aarch64 half FAILED: `signed-port-clean: guest exited
      without its case's final verdict`. The aarch64 bootable volume in the tree is from 2026-10-13 and the aarch64
      kernel from 2026-10-21; rebuilding it is a port build, which belongs to the end-of-job runs (not performed).
      perf-anchor PASS 54 s, after the image producer it now declares.
  - Phase C: the cold scenario failed at once - my batch script gave lab.sh a path relative to the repository and
    lab.sh resolves it from `src/` - so B4(c) was NOT exercised by this batch; the suite by hand after it passed
    (451 s) because the staging was still shipping. Restore build 57 s. B4(c) is re-run below with C7's oracle.

B2 - a gate's own completion line (implemented, host-verified; real runs pending)
  - `src/harness/guest-console.py`: `--done LINE` and `--settle SECONDS`. After the last command is typed, the
    driver watches whole output lines (colour codes and carriage returns removed, the unterminated tail excluded,
    so the shell's echo of the command never counts); once LINE is printed and the settle window has passed it
    ends the recording, writing `# guest-console: the completion line came, and the settle window after it has
    passed` into the log.
  - `src/tools/guest-gate.sh`: `GUEST_GATE_DONE` / `GUEST_GATE_SETTLE` are passed to the driver; B7 then stops the
    guest with it.
  - Five gates name their line and state their window: firmware-tree (both ports) `tree-fixture: event 2: GPIO
    line 2 high, acknowledged`, 20 s (it types no poweroff; first, as the plan asks); usb-release
    `usb-release-done`, 10 s; usb-legacy `usb-legacy-done`, 10 s; i2c-hid `hidcheck: PASS malformed`, 10 s (30 s on
    an emulated target); bluetooth-le `btclassic: PASS ctkd`, 5 s. Each line is printed by the last command the gate
    types, and each gate's oracle has negative checks, so each keeps a window.
  - Test `CompletionLineTest` (harness-test.py): each gate's script names its line and window; for each, a
    stand-in console prints the line, then a failure line the gate's oracle refuses half-way through the window -
    the driver ends within settle+20 s and the late line is in the log; with no window the same late line is
    missed (why a gate with negative checks keeps one). Watched failing with the completion handling disabled:
    every driver ran to its 90 s deadline.
  - Not yet measured on real gates (the "gates with the most idle after B1 and B7" ranking): pending the next
    real batch.

C1 - steps declare their resources (implemented, host-verified)
  - New `src/tools/verify-model/src/resources.rs`: the declaration language (`guest|host`, `stage:ARCH:r|w`,
    `profile:shipping|development`, `volume:ARCH:MODE:r|w`, `media`, `gadget`, `port:N`, `lab:NAME`, `marks:r|w`,
    `traces`), `parse`, `Declaration::conflicts_with` (a shared write, a one-holder resource, or one staging needed
    in two profiles / one volume in two modes), `GATE_RESOURCES` (one declaration for each of check.sh's 198
    gates), and `declaration_failures` - the host check:
    - follows each gate's command in check.sh to the scripts it runs (shell: a script named after a `/`; Python: a
      script named in a string, and `import lab`), skipping comments, heredoc bodies and messages;
    - requires a guest wherever they call guest_gate_run, run.sh, qemu-run.sh, test.sh, test-kernel.sh,
      guest-verdict.py, lab.sh, lab.py or dev.sh, and a staging write wherever they call build.sh or image.sh - in
      both directions, as the plan states it. A runner's own image build is not a staging write: under the runner
      the staging is brought to the declared state before the step (C2), so it finds it current.
    - `NAMES_WHAT_IT_DOES_NOT_RUN`: six gates whose scripts name runners they only read or test with stand-ins
      (boot-harness, guest-verdict, run-verdict, test-tags, dma-mode-carrier, gate-result-logs), each with a reason.
    - and requires the guest half to agree with how the catalog schedules the gate (`gate_schedules_a_guest`).
  - Wired into `verify-model check` (`self_check_failures`).
  - Watch-fail, the plan's own: with the catalogue as it stood the check named exactly icd-selection, lifecycle,
    foreign-loader-guest, foreign-facilities-guest and ipv6-peer as booting a guest and catalogued as host gates.
    They are in `GATES_THAT_BOOT_A_GUEST` now (96 -> 101), and `docs/verification/P02M0177-guest-cases.md` names
    them (the guest-verdict inventory test requires every listed gate there).
  - Under the plan's rule 64 of my first declarations were wrong ("declared to write a staging, and none of its
    scripts builds" - I had counted the runners as builds); they read now. Eleven gates write: bluetooth-independent,
    build-order, dma-mode-x86_64, hibernate, ipv6-peer, qemu-3d-demo, qemu-3d-extended, qemu-virtio-iommu-x86_64,
    rollback-floor-x86_64, run-verdict, targeted-cache.
  - Tests (verify-model): every gate declares and agrees; a gate added without a declaration is refused; a host
    gate whose script calls guest_gate_run is refused, and a guest gate whose scripts call no runner; a staging
    write is held in both directions; the conflict rules.

C8 - hibernate-ports is one gate per port (implemented; each half's real run pending - port runs)
  - check.sh: `hibernate-ports-aarch64` and `hibernate-ports-riscv64` (`check-hibernate-ports.py <port>`, which
    already took targets); the pair stays a command and becomes an umbrella in the catalog (`UMBRELLA_GATES`).
  - catalog: GATES 195 -> 197, both halves in GATES_THAT_BOOT_A_GUEST; `release-required.toml` carries the two keys
    in place of the one (hand-edited, as the end-of-job regeneration rule says).
  - `verify-model check`: consistent with the two keys.

C3 - a gate's temporary files and marks are its own (implemented, host-verified; real conflict runs pending)
  - check.sh `run_gate`: the injection trace's path is named per gate (`LIBER_INJECTION_TRACE`, mktemp -u) and
    `check-static-injection.sh` writes there; a failure prints and removes only that trace (the glob over every
    `liber-injection-trace.*.log` is gone). `LIBER_INJECTION_TRACE` is on the identity's permitted list.
  - build.sh sweeps step marks whose build pid is gone; `check-run-verdict.sh` runs its builds through
    `own_build` and judges only their marks.
  - A gate guest's monitor and QMP sockets are in its work directory (`GUEST_GATE_MONITOR`, `GUEST_GATE_QMP`,
    passed as `QEMU_MONITOR_SOCKET` / `QEMU_QMP_SOCKET`, which qemu-run.sh honours on every target) and its host
    port is a free one unless HOSTFWD_PORT is given. usb-release, usb-legacy, bluetooth-service and i2c-hid read
    them from there.
  - `staged-consistency`'s failure beside the host lane: not yet reproduced (a run of it beside each host gate is
    pending; if it does not reproduce it goes to F2's ledger).
  - The plan's three conflict reproductions (run-verdict beside a concurrent build, two failing injection gates,
    two x86_64 guests at once) are pending as real runs.

C7 - the lab's and the cold runs' names are private to a run (implemented, host-verified; real oracle pending)
  - lab.py: the `lab boot` guest's serial and control sockets, its two logs and its capture are in DEV_STATE (the
    run's `LIBER_DEV_STATE`, `.build/boot` otherwise); a run with a state directory forwards a free host port.
  - `cmd_scenario_cold`: every name - channel, serial broker, monitor, QMP, both logs, the scenario record, the
    guest's pgid record - is in a state directory: the caller's LIBER_DEV_STATE, or `.build/tmp/cold-TARGET.PID`
    for a run by hand (dev-clean removes day-old `.build/tmp`). The runner is handed that directory; x86_64 gets a
    free host port.
  - It builds nothing when the staging and the bootable volume are current: `staging_current_refusal`
    (staging-state.sh) reads every part's source stamp, the kernel and loader stamps, the profile records, and on
    x86_64 the bootable volume's stamp, profile and mode record. Under `LIBER_STAGING_RUNNER` a stale staging is
    REFUSED with status 75 and the commands that fix it (the runner runs them as a step of its own); by hand it is
    built as before. Scenario fixtures are built only when `stale_fixtures` reports them stale, and that check now
    includes the recipe's own digest (scenario.py), which it skipped before - without it a changed recipe would
    never be rebuilt.
  - Minimal adjacent changes this needed:
    - `KERNEL_SOURCES` in lib.sh, derived and gated in `verify-model check` like VOLUME_SOURCES (the kernel crate's
      closure). The kernel part's stamp hashed VOLUME_SOURCES, which the kernel directory is not in, so a kernel
      edit left it "current".
    - The bootable volume's stamp (`built-ARCH-volume`, kernel on the volume) covers VOLUME_SOURCES and
      KERNEL_SOURCES. build-order compares that stamp by mtime, so it is unaffected.
  - qemu-run.sh: COLD=1 with LIBER_DEV_STATE binds the channel, monitor and QMP in that directory and keeps the cold
    image set (its fixture media are content-keyed templates with private per-run copies; a set per directory would
    rebuild every medium).
  - The Python cold gates (tickless-idle, hibernate-ports, system-suspend-riscv64) give their guests a private state
    directory, read QMP there, and check every target they boot before the first boot: under the runner a stale
    one refuses the step with every fix; by hand `build()` builds only what is stale.
  - The gates that read a cold run's log set LIBER_DEV_STATE and read it there: dfu-tool, ipmi-admin, admin-path,
    tpm-tool, shell-large-output (keeps a failed run's log in `.build/logs/shell-large-output`), qemu-2d-account's
    port half. The lab-boot gates (audio-routing, qemu-2d-demo, qemu-pcie-aer, qemu-pcie-hotplug) boot their own
    guest in a private directory instead of reusing a person's.
  - `lab:cold-ARCH` left the declarations (17 tokens).
  - Test `ColdRunPrivacyTest` (harness-test.py): two runs of one target get two directories and every name the guest
    is handed is in its own; a current staging starts no build; under the runner a stale one exits 75 with nothing
    built or started; by hand a stale one builds the staging and the volume. Watched failing against HEAD's lab.py
    in a private copy: builds (and make-fixtures) on a current staging, `.build/boot` names, exit 1 instead of 75.
  - The real oracle (two cold scenarios of one target started together on a current staging) is pending.

C2 - the scheduler overlaps only what does not conflict (implemented, host-verified)
  - verify-model emits `STEPUSES` for every step (`resources::step_declaration`: a gate's declaration, or the union
    of a merged step's gates, read from its command; builds write their staging in the shipping profile; kernel
    suites read it and boot the media; image producers write the staging and the volume they sign; host and
    conformance suites share nothing; the lifecycle is a gate; development checks hold `lab:dev-instance`) as
    `RESOURCE=ACCESS:STATE` tokens (`Declaration::tokens`). A step without a declaration runs alone.
  - The planner orders by `order_for_slots` (C6 below), whose first key is `Declaration::state_rank`: the shipping
    profile's steps, then the development profile's, each volume mode's together.
  - verify.sh: the barrier scheduler (drain every guest before every host step and every exclusive one) is
    replaced by `dispatch`: one pass over the steps not yet started, in plan order, starting each whose
    prerequisites in this plan are decided, whose lane is free (guest slots, and one host lane beside them), whose
    tokens conflict with nothing running, and whose staging is in the needed state - switched by the runner
    (`resource_command`, run_owned_shell) once nothing running uses it and no earlier pending step still needs the
    state it is in. A guest step waiting only for slots, and a step that runs alone, keep later steps from taking
    their place. Refusals queue and are fixed with nothing running, as before. The end restore now checks the
    bootable volume's profile as well as its mode (a cold run leaves it `enforcing-required` in development).
  - staging-state.sh: `staging_profile_of`, `volume_state_of`.
  - Tests (check-verify-scheduler.sh, a private tree whose `./build.sh` records what it is asked and writes the
    records a build writes): two writers never overlap and a reader waits for a writer; two readers overlap; a
    grouped plan switches to the development profile once, after the shipping group ended, and each volume mode
    once, each mode's steps overlapping and the next mode switched after them; another architecture's step is not
    held back; the end puts back the staging and the volume once; a host step and a guest step run at once at one
    slot. All earlier cases still pass. Watched failing against HEAD's verify.sh: the writers overlapped, nothing
    was switched, the host step waited.

C6 - prices survive a model change; the order serves the wall clock (implemented, host-verified)
  - history.rs: a step's price is its last measured duration under its id and its command (`StepRecord.command`,
    recorded through `verify-model record --command`), whatever model it was measured under; a failure under a new
    model no longer erases it. `step_price_is_older` marks one measured under an older model (for A2).
  - A release's history is the tree's: verify.sh exports `LIBER_VERIFY_HISTORY` (the main tree's
    `.build/state/verify-history.json`) after `run-start`, and `History::path` honours it; it is permitted in the
    identity and cleared for verify-evidence's nested fixture runs.
  - Never-measured: guest steps keep the seed, host steps stay unpriced for `--budget` (unchanged).
  - `order_for_slots`: within a state group the builds first, then the host steps (host suites first, the cheap
    before the dear, the unmeasured last), then the guest steps dearest first; never before a prerequisite.
  - Tests: a price survives a catalog edit and is marked older, and another command under the same id is
    unpriced; a host step never measured has no price; with guest steps of 100, 10, 10 and 10 s the 100-s one comes
    first, the host suites first, the development step after the shipping ones. The three history tests that
    encoded the old rule were rewritten to the new one.

C5 - the width grows by measurement (implemented)
  - `EXCLUSIVE_GUEST_GATES` (65 gates, behind the barrier) is replaced by `ALONE_GATES` (8): qemu-2d-account,
    qemu-3d-demo, qemu-3d-extended, soft2d-performance (measurements of time), bluetooth-transfer and
    typec-tcpci-riscv64 (failed under load on 2026-10-20/21), concurrent-selection and virtio-multiport (oracles of
    the machine's processes). Every other gate overlaps what its declaration allows. A host gate on the list runs
    alone as well.
  - Merge and release default to four guest slots; inner stays at one. The rule for raising it (five L2 runs with
    no load flake in the ledger) is stated where the width is set and beside `ALONE_GATES`; the ledger is F2.
  - The release plan now: 374 steps, every one with a declaration, 8 alone.

Verification after the C group: `cargo test` in verify-model - 167 passed; `verify-model check` - consistent;
check-verify-scheduler.sh - all cases pass; harness-test.py CompletionLineTest, GuestConsoleTest,
ColdRunPrivacyTest - pass; check-guest-verdict.py inventory test - pass.

Host gates after the C group (2026-10-26): `./check.sh --gate
guest-verdict,verify-scheduler,run-verdict,boot-harness,source-hygiene,verify-model,verify-model-tests`.
  - First pass failed in boot-harness on two of my own test defects:
    - CompletionLineTest sent the late line at `settle / 2` of the driver's window, so the no-window run sent it at
      once - fixed with a separate delay, the gate's half window, for both runs;
    - FixtureSetTest's manifest carried a placeholder recipe that the new recipe check now refuses - it now
      carries the real recipe's digest, plus a test that another recipe is stale.
  - verify-model flagged the scheduler fixture's own `build.sh` stand-in as a staging write; verify-scheduler is
    now in `NAMES_WHAT_IT_DOES_NOT_RUN` (staging) with that reason.
  - After the fixes every one of the seven gates passed.

D3 - a failure does not hide the rest (implemented, host-verified; real runs pending)
  - Kernel (`src/kernel/tests.rs` `test_runner`): `TEST_AFTER` (compile-time, like a selection) starts the run
    after the named test, in the suite's own order, in each mode (whole, selection, tags). It prints `resuming
    after <id>`, and an unknown id is a hard failure, as a selection's is. The x86_64 test kernel compiles
    (`TEST=1 TEST_TAGS="" cargo build --tests`); the ports' compile is part of the end-of-job port builds.
  - `src/harness/test-kernel.sh`: after a failed test (`[failed]`), a stall naming the stuck test, a timeout, or a
    guest reset, `resume_after` boots again with `TEST_AFTER=<that test>` and the same tags or selection. The
    resumed boot's logs are `<stem>-restartN-{run,guest}.log`. Every failure is said as it is found, the first
    boot prints `N test(s) of this suite failed: ...`, and the verdict stays failed.
    - It is not resumed after a selection, resume or filter error, nor after a suite that printed `test suite
      complete` (a failure after the suite, e.g. the USB-audio pcap check).
    - The whole suite's envelope discharges the `[ok]` tests of every boot and names all their logs.
    - The stall watcher's and the timeout's test-id patterns had no `.` in them, so neither ever named a namespaced
      id (`kernel.x.y`); both now match it (the resume needs the name).
  - lab.py logs rules: a run with `-restart` logs is a failed run.
  - Tests:
    - `KernelSuiteResumes` (check-guest-verdict.py): the real test-kernel.sh over a stand-in compiler that records
      the resume point and a stand-in runner that plays a six-test suite. One planted failure: every test runs
      exactly once, the failure is reported, one more compile, the restart logs are named after the run. Two
      failures: both reported, three compiles. A passing suite boots once. Watched failing against HEAD's
      test-kernel.sh: t1-t3 ran, t4-t6 never did.
    - `LogsRulesTest.test_a_resumed_run_is_a_failed_run`.

D1 - a port's suite split across guests (implemented, host-verified; the oracle is an end-of-job aarch64 run)
  - `commands::split_suites`, applied in `emit_steps` to every plan the runner gets.
    - A kernel-suite step of aarch64 or riscv64 whose keys are that target's kernel tests is dealt into parts: the
      six heaviest each alone, the rest dealt dearest-first onto the lightest part, by `measured_test_durations`.
    - Each part is `TEST_SELECTION=<its ids> ./test.sh --arch A` with its own tests' keys, in a guest of its own.
      Steps that required the suite require every part.
    - The number of parts is chosen with each part charged the target's measured fixed cost of a run (its compile
      and its boot, `CostModel` fixed term), over the width (4); the count whose estimate - the work over the
      slots, or the dearest part - is least.
    - A step that discharges `suite.kernel` (the release's whole run) is never split, and nor is x86_64's suite,
      which runs whole in minutes and which the inner tier owns.
  - `measured_test_durations`: per target, the newest whole run's logs in `.build/logs/test` - `[ok] (N s)`, on the
    test's line or after its output, and 0.5 s for a bare `[ok]`.
  - Tests: every selected test in exactly one part, the six heaviest alone, each part an exact selection,
    prerequisites carried to every part, `suite.kernel` and x86_64 never split; durations read from the newest
    whole run in preference to a newer selection.
  - Decision (stated here, not in the plan): the split covers the two emulated targets only; the plan's L1 text
    gives x86_64's whole suite to the inner tier and the ports' tests to the merge tier "split across guests".

D2 - the whole suite still runs in every L2 and L3 (holds; no new code)
  - The release plan keeps one whole-suite step per target, carrying `suite.kernel`, and `split_suites` never splits
    a step that discharges it (tested above). Only a whole run's envelope discharges `suite.kernel` (test-kernel.sh,
    unchanged; a `TEST_AFTER` resume does not either).

E1 - a run continues what an identical one left (implemented, host-verified)
  - identity.rs:
    - `PER_RUN_ENVIRONMENT`: the run's own bookkeeping variables - the run dir, the logs holder, the refusal file,
      the injection trace, the gate key, the history path.
    - `continuation_differences`: the tree, each tool's digest (QEMU, toolchain, image tools), the firmware, the
      configurations, and the environment but the run's own variables.
  - evidence.rs:
    - `Envelope.continued_from`.
    - `Run::continue_from(earlier)`: refuses, naming what moved, when the identities differ. Otherwise every passed
      envelope of the earlier run this run has none for is taken over - its logs copied with their digests
      checked, republished as this run's, marked with the run that produced it - and `continued-from` is written.
    - `Run::withdraw`.
    - The collector refuses a taken-over envelope whose producing run, beside this one, is gone or no longer this
      identity (`continued-changed`). The dossier says `Continued: N envelope(s) taken over from run(s) ...` and
      has a `from run` column.
  - `verify-model continue-run --run NEW [--from OLD] [--steps PLAN]`: the newest earlier run marked `level` L2
    beside it that is identical (older ones tried in turn). With the plan, it prints `COVERED <index> <run>` for
    every step all of whose keys were taken over, and withdraws taken-over envelopes of steps only partly covered,
    so the step that will run publishes them again.
  - verify.sh:
    - `release_prepare` marks an in-place release over the whole required set `level` L2.
    - After the plan, an L2 calls `continue-run`; covered steps are decided as `CONTINUED: ... passed in run X`
      and never started.
    - A sealed release (L3) and a narrowed rehearsal continue nothing.
  - Tests:
    - check-verify-evidence.sh section 5, run on its own (the whole gate needs a shipping ISO and a nested
      rehearsal):
      - an L2 that passed one key and failed another is continued by an identical run: the passed envelope is
        taken over with its log, the plan step covered, the failed key not taken over;
      - the dossier names the producing run for the taken-over row and `this run` for the other;
      - a run with another QEMU binary first on PATH is refused by name ("the tool qemu-system-x86_64 moved since
        it ran") and takes nothing;
      - a dossier whose source run's QEMU digest was then changed is refused `continued-changed`.
    - check-verify-scheduler case 11: an in-place release over the whole set is marked L2, a covered step is not
      started again, the rest runs.
    - The real continuation of a real L2 has not been run: only one L2 is in this job's plan, at the end.

E2 - the smaller defects (implemented, host-verified)
  - A release in which a step was skipped (the two-guest gate at `--jobs 1`) now collects its dossier, then exits 6
    with "the release run is INCOMPLETE" (`release_finish` takes the incomplete reason).
    - Fixture: check-verify-scheduler case 10, a private in-place release whose cargo answers `run-start` and
      `dossier`. Watched failing against HEAD's verify.sh: no dossier, no message.
  - `--release` with `--catalog`, `--age`, `--trust` or `--model-hash` builds no snapshot (B6's `release_answers`),
    checked on the tree: each answered with no snapshot, run-start or "every optimisation" line.

F1 - one bounded wait (implemented; compiled for x86_64, runs pending)
  - `src/kernel/tests.rs`: `wait_for(rounds, pump, poll) -> Option<T>` and `wait_until(rounds, pump, holds) -> bool`.
  - The brightness test's `receive` and `peer_closed`, and the gamepad test's `settled`, `until_closed`,
    `acknowledged` and `drive_through`, are written over them (their own loops are gone).

F2 - the flake ledger (written)
  - `docs/verification/FLAKES.md`: the rules (a test listed twice is fixed first; a failure that repeats alone is
    no flake; the width grows only by the ledger; the one bounded wait), the count of L2 runs since the last
    flake attributed to load (0), and the entries of 2026-10-20/21 as the plan's measurements state them. A
    target that measurement did not record is written "not recorded" rather than guessed. The spool test is noted
    as not a flake.

F3 - typec-tcpci-riscv64 (by the plan's second option): it runs alone (`ALONE_GATES`); its bounds are unchanged.

A1 - four presets (implemented, host-verified)
  - `./verify.sh --level L0|L1|L2|L3` (and `--base REV`):
    - L0: the inner tier over the working tree's change.
    - L1: on a clean tree, `--for-range BASE..HEAD --l1-suites` and then `--merge` of its handoff at the same HEAD.
      BASE is `--base`, or `.build/state/verify-last-passed`, which a passed L1 writes and so does a passed L2 over
      a clean tree; with neither it refuses and names `--base`.
    - L2: `--release --in-place`.
    - L3: `--release`, refused before anything is built when `$BUILD_DIR` has less room than
      `.build/state/release-build-size`, which a sealed release writes (the snapshot's `.build` size) for the next.
  - tier.rs `with_suites` (`tier-inner --level L1`): `suite.kernel` x86_64 added and the x86_64 tests the range
    selected one by one dropped (the whole suite runs them), and every port kernel test added; then partitioned as
    any plan.
  - Tests:
    - tier test `an_l1_over_two_commits_plans_both_and_its_merge_runs_the_deferred_suites_at_the_same_head`: each
      commit's keys planned; suite.kernel x86_64 inner; no x86_64 test one by one; every port test deferred; the
      merge at the same HEAD runs every deferred key and its outcomes are accepted.
    - check-verify-scheduler case 12: L1 with no base refused naming --base; L3 with less room refused before
      run-start; an unknown level refused.

A2 - the estimate before and the measured time after (implemented)
  - Before dispatch: `verify-model account --steps PLAN --jobs J --brief` prints `estimate: N step(s), X s of priced
    steps (K priced under an older model, U never measured); no schedule over J slot(s) finishes in under F s`.
  - After the run: `measured: W s wall clock over J guest slot(s); the steps took S s between them`.

A3 - where the time goes (account rewritten for the C2 scheduler)
  - `history::PlannedStep`, `Account::of(steps, history, cost, hash)`: what runs alone, the host lane, the guest
    slots (price x slots), the dearest exclusive chain (writers or holders of one resource, one after another), the
    longest step. Floor = alone + the longest of the host lane, the guest work over the width, the chain and the
    longest step. Unmeasured guest steps are priced at the seed; unmeasured host steps are named.
  - Measured on the release plan at four slots with the prices in the history (most from 2026-10-20/21, 325 of
    362 under an older model):
    - 374 steps, 84,844 s (23.6 h) of priced steps, 12 never measured;
    - alone 3,504 s; host lane 6,170 s; guest work 18,792 s over 4 slots; dearest exclusive chain 7,406 s
      (`stage:x86_64`, its builds and writer gates); longest step 4,305 s (the aarch64 suite, before D1 splits it);
    - floor 22,296 s (6.2 h).
    These prices predate B1, B7 and B2 - boot-volume-controllers on the ports still 3,901 s - so the next L2's
    measurement replaces them.

Verification after D, E, F, A: verify-model `cargo test` 170 passed; `verify-model check` consistent;
check-verify-scheduler.sh all cases; check-guest-verdict.py KernelSuiteResumes; harness-test.py LogsRulesTest,
CompletionLineTest, FixtureSetTest, ColdRunPrivacyTest; check-verify-evidence.sh section 5 alone.

The second x86_64 batch (2026-10-26, from 11:04Z; results as they come)
  - D3, the kernel's half on the real tree: `TEST_SELECTION=<three ids, in the suite's order> TEST_AFTER=<the first>
    ./test.sh --arch x86_64` - the guest printed `resuming after kernel.arch.common.bootmem.a_direct_boot_archive_
    states_its_own_length: the 1 test(s) up to it ran in an earlier boot`, `running 2 selected tests (584 total)`, the
    two after it `[ok]`, and `test suite complete: 2 passed` (PASS, 22 s).
  - F1 and the suite after the kernel's changes: `./test.sh --arch x86_64` PASS, 585 passed (433 s in the guest,
    446 s in all).
  - C3, run-verdict beside a concurrent build: the concurrent `build.sh --part libs` (on a current staging) took 2 s,
    so it was not live when run-verdict looked - not a reproduction. Reproduced instead with a live foreign mark:
    `.build/state/build-step.<pid of a live sleep>`. The run-verdict gate of commit 73a0d898 (before C3) failed on it
    ("a finished build left its step mark behind: .../build-step.864826"). The current gate passed ("no step mark
    (3 build(s) of this gate's)", RESULT ok).
  - C3, two failing injection gates side by side: the new run-verdict case passed in the gate run, and with the
    check.sh of 73a0d898 copied into its private tree each run printed both traces.
  - C3, staged-consistency beside the host lane: run beside `build-order`, it passed (272 s) and `build-order`
    FAILED ("./build.sh --kernel-on-volume --part volume failed"); `build-order` passed alone (111 s).
    `check-staged-consistency.sh` says it: "Every case below MUTATES THE REAL STAGED TREE and puts it back", and
    so do the six injection gates (`check-static-injection.sh`: each target's staged image, backup and restore).
    They are WRITERS:
    - staged-consistency `stage:x86_64:w`;
    - static-image, undeclared-edge, duplicate-edge, malformed-dynamic, malformed-symbol-relocation and
      identity-note `stage:x86_64:w stage:aarch64:w stage:riscv64:w`.
    The static check cannot see a write without a build call, so these seven are in the exemption table, renamed
    `NOT_HELD_TO_ITS_SCRIPTS` since it now holds both kinds, with the reason.
  - B2, C2, C3 together - four development-image gates through the scheduler at four slots (`LIBER_VERIFY_STEPS`
    with the release plan's steps for i2c-hid, usb-legacy, bluetooth-le and usb-release, keys kept):
    - estimate: 2,920 s of priced steps, a floor of 929 s;
    - the runner brought `stage:x86_64` to development (138 s) and the volume to development/harness (72 s), once;
    - all four started together and passed - bluetooth-le 140 s, i2c-hid 164 s, usb-release 176 s, usb-legacy
      274 s - four x86_64 guests at once, none colliding on a socket or a port;
    - the end restore put the staging back. Run 704 s, against 452 + 409 s for two of them one after another in the
      first batch.
  - C7 - two cold scenarios of one target started together on a current staging:
    - the cold run before them, by hand on a shipping staging, named what was stale (libs in shipping; the bootable
      volume not from these sources) and built it, then passed `launch a program`;
    - then `launch-program` and `shell-basics` together: each printed "the x86_64 staging is current in the
      development profile; nothing is built", booted in its own directory (`.build/tmp/cold-x86_64.844649`,
      `.844650`) and passed.
  - B4(c): `./test.sh --arch x86_64` by hand straight after the cold runs: `REFUSED: the x86_64 staging's 'libs' was
    built in the development profile ... build it: env -u LIBER_DEVELOPMENT ./build.sh --arch x86_64`, exit 75 in
    1 s.
  - B6, concurrent-selection, its first run: after rebuilding the x86_64 staging (my verify-model edits had made it
    stale - the gate refused in 1 s on it), PASS in 112 s: two suites overlapping, each on its own medium and logs.

The host gates after the second batch (2026-10-26)
  - `./check.sh --gate verify-model,verify-model-tests,verify-scheduler,guest-verdict,boot-harness,run-verdict,
    source-hygiene,milestone-index,gate-result-logs`: the first six passed; source-hygiene FAILED on
    `src/harness/test-kernel.sh:742` - `failed_test` piped both logs into an awk that stopped at the first failed id
    (`exit`), which under pipefail reads as a failed pipeline when the writer is still writing.
  - Fixed: the awk reads to the end and skips the lines after the first failed id (`found { next }`); what it prints
    is unchanged. `check-guest-verdict.py KernelSuiteResumes` 3 tests OK (91 s); then
    `./check.sh --gate source-hygiene,milestone-index,gate-result-logs` RESULT ok (110 s).
  - `./format.sh --changed`: nothing to change.

The end-of-job runs (2026-10-26/27/28)
  THE L2 (VERIFICATION's one L2): `./verify.sh --release --in-place` (four guest slots).
  - A first attempt from 12:22:56Z was stopped at step 9 of 374: a commit at 12:48Z (48547848, made by the owner while
    it ran) moved the tree's identity from the working tree to the git tree. The next run's E1 lookup refused to continue
    it ("source tree moved"), as E1 requires, and the run began again.
  - Run 20261026T125430Z-48547848-1136383-f9d54c, 12:54:07Z - 22:22:55Z: 374 steps; measured 34,044 s (9.5 h) of wall
    clock over four guest slots, the steps 66,457 s (18.5 h) between them; its estimate before it started was
    83,589 s of priced steps and a floor of 21,755 s. Against the baseline - 22.3 h for 183 steps and about two hours
    more for the other 190, one after another - the whole release list ran in 9.5 h. `.build/logs` before it:
    330,284,339 bytes.
  - 20 steps failed and `capability-trace` did not run (it reads the riscv64 suite's run, which failed). Each failure was
    read from its envelope and logs, and every one that could be the harness's was run again alone. By cause:
    HARNESS, this milestone's, fixed (below): `implementation-mutations`, `qemu-ipmi-admin`, `qemu-tpm-tool`,
    `boot-volume-controllers-aarch64` and `-riscv64`, `signed-boot`, `qemu-2d-demo`; and two that did not fail a step but
    broke the measurement: the history (C6) and the logs rule against D1 (G1). And `firmware-tree` on both ports passed
    but ran 983 and 970 s (B2's completion line, below).
    CODE, failing alone, outside this milestone (OPEN): `iommu-aarch64-direct-gicv2-hostile` (and the two other hostile
    profiles; alone: the same kernel test fails, 44 s); `arch-profile-aarch64-no-dt-absent-1` and its riscv64 twin (alone,
    337 s: the loader prints "kernel loaded" before "FATAL - no DMA mode" and then hangs to the 300-s test timeout; the
    loader last changed in 559e8fb8, another milestone); the riscv64 suite on
    `kernel.services.spool_service_sends_submitted_jobs_exactly_once` (the known spool bound; D3 resumed after it - 538
    ran, 1 failed, the restarted boot ran the other 11 and they passed, verdict failed); `system-suspend-riscv64` (alone,
    486 s: "the clocks differ by 0 ms across sleep, not the reported 5112 ms") and `tickless-idle` on the same clock
    check on both ports - the ports' boot-time clock does not count sleep; `typec-tcpci-riscv64`, which ran alone in the
    L2 (`ALONE_GATES`) and failed on "vbus-stays-off: detached after 330 ms, before tSrcRecover and tSrcTurnOn (1275 ms)
    had passed" - by the ledger's rule a failure alone is no flake, so it is moved there to the code's bound;
    `development-lifecycle` (its protocol fuzz) and `qemu-pcie-hotplug` (its removal order), failing alone in the
    narrowed rehearsal below.
    UNDER LOAD, passing alone: `ipmi` and `sleep` (below).
  - Passed in the L2, closing items whose real runs were pending: `hibernate-ports-aarch64` 2,675 s and `-riscv64`
    3,898 s, each its own step and key (C8; 6,842 s as one step before); `concurrent-selection` 107 s,
    `verify-evidence` 486 s and `perf-anchor` 50 s (B6); `usb-release` 171 s, `usb-legacy` 272 s, `i2c-hid` 160 s and
    `bluetooth-le` 138 s (B2); `staged-consistency` 254 s, `run-verdict` 105 s; the x86_64 and aarch64 suites (F1).
    `tickless-idle` counted the wakes on the console holder's line after the handoff on all three targets (x86_64
    interrupt 36, aarch64 33, riscv64 "the console UART was handed to its driver, whose claim raises interrupt 55
    where the boot armed 63 ... woke the idle machine 23 time(s) on the UART's interrupt 55") - B6's part - and
    failed on the ports' sleep clock above.

  THE FIXES. Each was watched failing first.
  - C6, THE HISTORY WAS OVERWRITTEN BY A STEP. A release exports `LIBER_VERIFY_HISTORY` so the runner records into the
    tree's history; every step inherited it, and the host step that runs verify-model's own tests saved their fixture
    history (another model hash, 265 entries of 0 s) over the tree's at 14:54:19Z, losing every record before it. The
    L1 inner run that followed ended STALE (exit 5) on 1,680 keys past the age window because of it.
    - Fixed: `run_one_step` runs each step with `LIBER_VERIFY_HISTORY` empty; the history is the runner's alone.
    - Repaired: the tree's history was rebuilt from the run's own envelopes (a script in the scratch directory): the
      265 fixture entries dropped, the 244 steps' prices and keys recorded again under the run's model hash
      (73 minutes of `verify-model record` calls). `capability-trace`, which did not run, is not recorded. The
      envelopes carry the steps' keys, not each kernel test's, so the per-test keys stay unrecorded ("never" in
      `./verify.sh --age`) until a suite records them again.
  - B3, `implementation-mutations`: its work copy of the tree had the staging but not the profile records beside it,
    so `test.sh` in the copy refused (75) and the gate read that as "not by the assertion". Fixed: the copy takes the
    tree's `built-x86_64-*.profile` records with the staging they describe (`seed_mutation_inputs`). Passed in the
    narrowed rehearsal (286 s).
  - C1, THE BOOTABLE VOLUME'S MODE. `qemu-ipmi-admin` and `qemu-tpm-tool` failed on "lab: REFUSED: the x86_64 bootable
    volume is not signed for DMA mode 'enforcing-required'": the development-instance gates (`dev.sh up`, `lab.sh
    boot` - their image build signs the shared volume `harness`) declared no volume, so the scheduler ran them beside
    cold runs that need it `enforcing-required`, and the volume was switched under them.
    - A third rule in `verify-model check` (`resources.rs`): the x86_64 bootable volume is declared in the mode a
      gate's boots sign it for - `harness` wherever its scripts bring up a development instance or image
      (`dev.sh up`, `lab.sh boot`, `guest_gate_run`), `enforcing-required` wherever they start a cold run
      (`scenario-cold`, `staging_not_current`); a gate whose boots need both switches the volume under every step
      beside it, so it must run alone (`ALONE_GATES`) and declare one of them. Gates that write the staging themselves
      are exempt (they declare a writer).
    - Watch-failed on HEAD's declarations: the rule named exactly 18 gates - 16 development-instance gates with no
      volume, `watchdog` declared `enforcing-required`, and `serial-handoff` (a cold run and a development instance in
      one gate) not alone. Fixed: the 16 declare `volume:x86_64:harness:r`, `watchdog` `harness`, `serial-handoff` runs
      alone. verify-model's new test `the_x86_64_volume_is_declared_in_the_mode_a_gates_boots_sign_it_for` (a fixture
      of five gates: three failures expected, none for the alone one).
    - `qemu-ipmi-admin` and `qemu-tpm-tool` passed in the narrowed rehearsal (412 s, 230 s).
  - B7 AND B3, A REFUSED PORT GUEST WAS A FAILURE. `boot-volume-controllers-aarch64` and `-riscv64` were declared in the
    development profile, but the gate never enters it, so `qemu-run.sh` refused the staging (shipping wanted) in a
    second; the console driver then waited its 600-s budget for a socket that never came, and the refusal reached the
    runner as "the guest produced no console output".
    - `guest_gate_run` waits for whichever of the driver and the guest ends first; a guest gone first ends its driver
      (after five seconds' grace), and a guest that exited 75 makes the gate exit 75 with the REFUSED line.
      Test `test_a_refused_guest_ends_the_gate_at_once_with_the_refusal` (`check-guest-verdict.py`): watch-failed against
      HEAD's `guest-gate.sh` (60 s, exit 1), passes in seconds with 75 and the refusal.
    - The two declarations name the shipping profile.
  - B7 AND B3 ON THE PORTS' BOOTABLE VOLUME. In the first narrowed rehearsal the ports' boot-volume-controllers were
    refused for their bootable volume ("carries no profile record") and handed the STAGING's build, which never
    rebuilds the volume - so the runner built it and the gate was refused again.
    - `staging-state.sh` `volume_state_refusal`: a volume built in another profile, or with no record of one, or
      signed for another DMA mode, is refused with the VOLUME's own build (`volume_build_command`) as its fix.
    - `qemu-run.sh` checks the volume beside the staging: on x86_64 outside a test run in the mode it pairs with
      (`LIBER_DMA_MODE`, default `enforcing-required`), and on a port's system-disk boot (`SYSTEM_DISK`) `harness`;
      the staging's own parts drop `volume-test` there.
    - `boot-volume-controllers-aarch64` and `-riscv64` declare `volume:ARCH:harness:r`.
    - Tests: `BootableVolumeRefusal` (check-guest-verdict.py) - the real `qemu-run.sh` in a fixture tree: a port's
      system-disk boot with no volume record, and with a volume signed `enforcing-required`, is refused 75 with the
      volume's build as the fix; an x86_64 boot with a volume built in the development profile likewise. Watched
      failing against HEAD's `qemu-run.sh` and `staging-state.sh`: both handed the staging's build.
  - `signed-boot` checked the medium - the run's artifact, from the image producer - against the LIVE bootable volume,
    which steps after the producer had rebuilt ("not built together"). Fixed: inside a run it reads the volume the
    producer stored beside the medium (`evidence_image system-volume-bootable-x86_64.img`). Its port half was then
    refused in the first narrowed rehearsal because its declaration named no port staging, so
    `firmware-tree-aarch64` beside it switched the aarch64 staging to the development profile; it declares
    `stage:aarch64:r stage:riscv64:r` now. Every case, the port half included, passed in the second (204 s).
  - B2, `firmware-tree` on the ports ran 983 and 970 s: its completion line is printed while the guest is still
    booting, before the shell takes the first command, and the driver watched for it only after the last command was
    typed. Fixed (`guest-console.py`): the line counts wherever it comes, and the recording ends once every command has
    been typed and the settle window after the line has passed. Test `test_a_completion_line_printed_during_boot_ends_
    the_recording_once_the_commands_are_typed` (harness-test.py): watch-failed (ran to its 90-s budget), passes in about
    4 s. In the first narrowed rehearsal: 302 s on aarch64 and 362 s on riscv64, each guest log ending "#
    guest-console: the completion line came, and the settle window after it has passed" (1200 s before B1 and B7).
  - G1 AGAINST D1: the logs rule kept each target's newest suite run, which after the L2 was a later selection, so the
    whole port suites' logs - the measured durations D1's planner deals the parts by - were pruned. Fixed (`lab.py`
    `suite_logs_kept`): each target's newest WHOLE run (`tests (all tags)`) is kept too, chosen as the planner chooses it.
    Test `test_the_newest_whole_run_is_kept_beside_a_newer_selection` (LogsRulesTest), watch-failed.
  - QEMU-2D-DEMO, A REGRESSION OF C7: the gate runs its lab guest in a state directory of its own (`LIBER_DEV_STATE`,
    where `qemu-run.sh` puts the guest's control monitor), and `screenshot.sh` looked only in `.build/boot`, so every
    shot booted a throwaway instance of its own and the demo "never reached the screen" (the L2 and the first narrowed
    rehearsal).
    - `screenshot.sh`: `RUN_MON="${LIBER_DEV_STATE:-$BUILD}/qemu-monitor.sock"`.
    - Test `ScreenshotTest` (harness-test.py): `screenshot.sh` in a fixture tree with a stand-in HMP monitor on
      `<state>/qemu-monitor.sock` attaches to it and writes the frame. Watched failing against HEAD's script ("kernel
      ELF not found" - it fell back to booting one).
    - `./check.sh --gate qemu-2d-demo` alone: PASS (157 s), "three live frames prove animation, analytic coverage,
      blending, filtering and a colour glyph".
  - IPMI AND SLEEP RUN ALONE. Both failed under four slots in two runs and passed alone with one slot: in the L2 (when
    they declared no bootable volume and gates beside them re-signed it) ipmi's SSIF boot answered `bmc sensors` with
    "the BMC is unavailable" and sleep's console refused a connection at the Ctrl+Z step; in the first narrowed
    rehearsal, with the volume declared, both development instances "reached a shell but [the] development agent never
    answered" within the bring-up's 30 s. By the ledger's rule (F2) they run alone (`ALONE_GATES`, eleven now), each
    with an entry in `docs/verification/FLAKES.md`, whose count of L2 runs since a flake attributed to load stays 0.
    `typec-tcpci-riscv64` moves from the ledger's open entries to "not a flake".
  - B2, THE GATES WITH THE MOST IDLE AFTER B1 AND B7, ranked by the L2's measured times. Of the release list's guest
    gates, the ones that type no `poweroff` and named no completion line recorded to their whole console budget: in
    the L2 bluetooth-classic 649 s (two boots of a 280-s budget), bluetooth-service 612 s (two of 260 s), power-ups
    566 s and brightness-usb 562 s (each 300 s, and their guest kept to its 480-s deadline by `GUEST_GATE_WINDOW=keep`),
    power-service 381 s, smartcard-service 380 s, bluetooth-transfer 337 s, bluetooth-le-audio 317 s,
    bluetooth-audio 286 s, bluetooth-input 172 s, and the four with the default 70-s budget at about 150 s
    (foreign-loader-guest, foreign-facilities-guest, icd-selection, lifecycle).
    - Thirteen of them name a completion line now: each types `echo <gate>-done` as its last line - the console
      driver types a line only once the shell's prompt is back, so every command before it has returned - and
      states its window: 10 s where the oracle refuses lines (a probe failure, a pairing that must not happen, a
      request a simulated device has no control for), 5 s for foreign-loader-guest (a refused negotiation) and
      lifecycle (its crash, exit and destructor counts), 20 s for bluetooth-transfer, which still needs
      NetworkService's return to its NIC after the PAN link departs, and none for the four whose checks are all of
      lines that must appear (bluetooth-le-audio, bluetooth-audio, foreign-facilities-guest, icd-selection).
    - power-ups and brightness-usb, whose verdicts also read their simulator's log on the host, cover what the
      simulator still records with that window, so `GUEST_GATE_WINDOW=keep` - kept by B7 "until B2 names their
      completion lines" - is gone from `guest-gate.sh`, with its test (`test_a_kept_window_runs_the_guest_to_its_
      deadline`).
    - Not converted: bluetooth-input - after its last command the fixture's keyboard types into the same shell, and
      its oracle refuses a restart after the keyboard's chords, which needs its whole 90-s budget; and the four guest
      gates outside the release list (midi-service, camera-service, modem-service, gamepad-tool), which have no
      measured time to rank.
    - Test: `CompletionLineTest` carries the thirteen - each script names its line and states its window, and for
      each with a window a line its oracle judges, injected half-way through the window, is recorded with it and
      missed without it. Watched failing against HEAD's gate scripts: the thirteen new cases fail ("the gate names
      its completion line"), the five older ones pass.
    - Each run alone, on the fixed tree (`LIBER_DEVELOPMENT=1 ./check.sh --gate G`, one after another; the L2's time,
      under four slots, in brackets): bluetooth-classic 229 s (649), bluetooth-service 217 s (612), power-ups 130 s
      (566), brightness-usb 165 s (562), power-service 163 s (381), smartcard-service 167 s (380), bluetooth-transfer
      191 s (337), bluetooth-le-audio 140 s (317), bluetooth-audio 133 s (286), foreign-loader-guest 130 s (149),
      foreign-facilities-guest 124 s (153), icd-selection 124 s (152), lifecycle 131 s (150) - all PASS.
      bluetooth-classic's and bluetooth-service's cold reboots found the bond the first boot stored. The two guest
      logs a gate keeps (`.build/logs/power-ups/guest.log`, `.build/logs/brightness-usb/guest.log`) end "#
      guest-console: the completion line came, and the settle window after it has passed"; brightness-usb's window
      caught a late line ("BrightnessPolicy: ... stored at 45").

  THE NARROWED REHEARSALS - the failures the L2 left, run again on the fixed tree (`./verify.sh --release --in-place
  --required <the failed keys and their producers>`; rehearsals, as their dossiers say):
  - #1, four guest slots (run 20261027T001029Z-48547848-2630969-e929ee, 00:10 - 01:35Z, 39 steps): 5,007 s of wall
    clock, the steps 7,407 s. PASSED: implementation-mutations (286 s), qemu-ipmi-admin (412 s), qemu-tpm-tool
    (230 s), firmware-tree-aarch64 (302 s) and -riscv64 (362 s), typec-ucsi (1,157 s), watchdog (1,862 s),
    qemu-virtio-iommu-x86_64 (704 s), dev.selftest and both image producers. FAILED for causes of this milestone's,
    fixed above: the ports' boot-volume-controllers, signed-boot's port half, qemu-2d-demo. FAILED, decided by #2:
    development-lifecycle, qemu-pcie-hotplug, sleep, ipmi.
  - #2, one guest slot (run 20261027T014652Z-48547848-2929926-e0714b, 01:46 - 04:30Z, 31 steps): 9,734 s of wall
    clock, the steps 8,399 s. The runner brought `volume:aarch64` and `volume:riscv64` to shipping/harness before
    the ports' boot-volume-controllers ("in 'no recorded state' - bringing it about: env -u LIBER_DEVELOPMENT
    ./build.sh --arch ARCH --kernel-on-volume --dma-mode harness --part volume"). PASSED: boot-volume-controllers-
    aarch64 (1,634 s) and -riscv64 (2,078 s; 3,901 s before B1 and B7), signed-boot with its port half (204 s), ipmi
    (1,725 s), sleep (1,618 s), dev.selftest, both image producers. FAILED ALONE: development-lifecycle -
    `dev.proto-test`'s fuzz timed out sending (`TimeoutError` in `fuzz_rounds`, 67 s), after which `dev.perf-gate` and
    `dev.gpu-restart` found the instance "starting" (3 s each), as in the L2 - and qemu-pcie-hotplug ("the slot was
    powered down at line 1, before the driver let go at line 2 - that is a surprise removal"): both code, both under
    OPEN already. qemu-2d-demo failed as expected (its fix came after the run). The owner's commit 2d365732 landed
    during the run; the dossier says "the tree changed while the run was in progress" (the working tree's identity,
    then the same content as a git tree).

  HOST CHECKS on the fixed tree: verify-model `cargo test` 171 passed; `verify-model check` consistent (model hash
  aaa5673e...); `harness-test.py` 127 tests OK; `check-guest-verdict.py` 114 tests OK; `./format.sh --changed` clean;
  `./check.sh --gate verify-model,verify-model-tests,verify-scheduler,guest-verdict,boot-harness,source-hygiene,
  milestone-index,gate-result-logs` RESULT ok (836 s). The owner committed the first fixes as 2d365732 and 1434284f;
  the thirteen B2 gates and the kept window's removal are in the working tree.

  THE MEASUREMENT RUNS (L0 on three changes, L1 on the first, D1, D4), one after another on one tree:
  - L0, a leaf driver: `./verify.sh --for src/user/drivers/core/src/acpi_fan_driver.rs` (2026-10-27 05:35Z): 4 inner,
    6 deferred keys; estimate 92 s of priced steps; all 4 steps passed; measured 357 s of wall clock over one guest
    slot, the steps 158 s between them (the rest the staging's switch back to the volume's shipping mode, 64 s, and
    the planner); exit 5, STALE - 1,701 keys past the age window, the per-test kernel keys the lost history no longer
    records ("never").
  - L0, a kernel change (`--for src/kernel/sched`, from 05:42Z) was cut off at 06:30Z with 147 of its keys passed,
    when the session running it ended; the host then rebooted (18:32Z), which emptied the scratch directory with this
    section's drafts. It is run again from the start, with the rest, below.
  - THE CHAIN, run again from the start (2026-10-28, `/root/p0206-end/chain.sh` and then `after.sh`, detached, one
    after another on one tree; scratch files outside the repository):
    - L0, a kernel change: `./verify.sh --for src/kernel/sched` (04:29 - 07:41Z): 239 steps (823 inner, 1,238
      deferred keys); estimate 7,478 s of priced steps, floor 7,015 s at one slot; measured 11,483 s of wall clock,
      the steps 7,535 s between them; 238 passed. `run-verdict` FAILED on "a run ended without saying so, or said the
      wrong thing" (its HUP case).
    - L0, a harness change: `./verify.sh --for src/harness/test-kernel.sh` (07:41 - 10:57Z): the same 239 steps;
      estimate 7,499 s, floor 7,044 s; measured 11,665 s, the steps 7,706 s; 238 passed, `run-verdict` FAILED the same.
    - THE run-verdict FAILURE IS AN ARTEFACT OF HOW THE CHAIN WAS STARTED. The chain ran under `nohup`, so SIGHUP was
      ignored from the start, and a process that inherits an ignored signal cannot trap it. The gate's case sends
      HUP to a shell that traps it and expects exit 129. A probe under `nohup` survived its own HUP ("survived").
      `./check.sh --gate run-verdict` started under `setsid` only, on the same tree: RESULT ok (46 s), "a HUP to the
      shell -> RESULT failed exit=129". So the rest ran under `setsid` without `nohup`.
    - THE WALL CLOCK AGAINST THE STEPS. Both L0s spent about 3,950 s (11,483 - 7,535; 11,665 - 7,706) outside their
      steps, over 239 steps: 16.5 s a step. That is `record_one_step`. In the runner, after each step,
      `verify-model record` loads the model (16 to 18.5 s timed by hand), and nothing is dispatched until it returns.
      No item of this milestone covers it. It is recorded in the plan (A3, OPEN) for a milestone of its own.
    - L1 on the leaf change. Inner half: `./verify.sh --for src/user/drivers/core/src/acpi_fan_driver.rs --l1-suites`
      (10:57 - 11:11Z): 5 steps, all passed, 824 s (the steps 731 s); exit 5, STALE (1,116 keys). `--merge` needs a
      commit of exactly the tested content, which is the owner's, so the deferred half ran from the handoff as a
      prepared plan. `deferred-steps.py` took the full plan (`verify-model commands`) and kept every step that carries
      a deferred key: 30 steps, which cover 1,101 of 1,101 deferred keys, none missing. 8 other keys came along, the
      ports' build parts, which the parts read. It ran as `LIBER_VERIFY_STEPS=<that plan> ./verify.sh --jobs 4`.
    - Deferred half, first run (11:12 - 12:00Z): 2,854 s, the steps 9,157 s; 12 of 30 failed. 10 were refused by
      `test.sh`'s guard against stray guests: "a qemu-system-aarch64 already holds this tree's disk images: 1412312
      (.../aarch64-20261028T111704Z-1337472-restart1-run.log)". That was one part's RESUMED boot (D3), whose logs are
      named `<stem>-restartN-{run,guest}.log` after the run whose test failed. The guard's list of a run's own private
      files had `<arch>-<stamp>-<pid>-run.log` and not the `-restartN` form, so every later part of the same target
      took that boot for a foreign run. A HARNESS DEFECT OF THIS MILESTONE: D1 and D3 meet only when parts run beside
      each other. The other 2 failures were both ports' part 8/8 on the spool test.
    - THE FIX (after the chain ended, before `after.sh`):
      - `test.sh` `require_no_stray_qemu`: the private log pattern takes an optional `-restart[0-9]+`
        (`^logs/test/(x86_64|aarch64|riscv64)-[0-9]{8}T[0-9]{6}Z-([0-9]+)(-restart[0-9]+)?-(run\.log|guest\.log|usb-
        audio\.pcap)$`). The owner is still the second group, the pid of the run that failed; that run is the parent
        of the resumed boot (`test-kernel.sh` starts it as a child, `:757`), so it is in the QEMU's ancestry.
      - `src/tools/check-guest-verdict.py` `ExistingQemuAdmission`: `PRIVATE_SCRATCH` gains three resumed-boot names
        (aarch64 `-restart1-run.log`, riscv64 `-restart1-guest.log`, x86_64 `-restart2-usb-audio.pcap`).
      - Watched failing: the fixture against the unfixed `test.sh` in a mirror failed the three new subtests
        ("23 != 0"); with the fix they pass. `check-guest-verdict.py` whole: 114 tests OK. `shfmt -d test.sh` clean.
    - D4 (12:00 - 13:43Z), `src/harness/test-kernel.sh aarch64` with `TEST_TIMEOUT=60m`, debug against
      `CARGO_PROFILE_DEV_OPT_LEVEL=1 CARGO_PROFILE_TEST_OPT_LEVEL=1`, each with its own `CARGO_TARGET_DIR` under
      `.build/tmp/d4`:
      - the whole aarch64 suite, debug: 4,357 s, 546 passed (4,343 s in the guest);
      - at `opt-level` 1: 1,442 s over three boots (1,429 s in the guest); 544 passed. Two failed:
        `kernel.extable.the_exception_table_exists_and_is_small_enough_to_scan` ("the exception table has grown to
        200 entries") and `kernel.hardware.hda_driver_routes_a_codec_and_the_controller_consumes_a_buffer` ("the
        controller's link position counter should advance"). D3 resumed after each;
      - built from nothing: 35 s debug, 42 to 43 s at `opt-level` 1. My first "warm" timing touched a file and
        measured only a no-op rebuild (4 s each), so it was measured again with real content changed: one string in
        a function of `src/kernel/sched/mod.rs` (`after.sh`), the kernel crate rebuilt in 3.6 s debug and 7.2 s at
        `opt-level` 1. The file was restored from a copy and checked with `git diff --quiet`.
      By the plan's rule (adopted only if every outcome is the same and the per-change level is not slower) it is NOT
      ADOPTED: two outcomes differ, and a change rebuilds twice as slowly. The two bounds are those tests' own. The
      target directories (1.3 GB and 1.6 GB) were deleted after the measurement.
    - Deferred half, second run, on the fixed tree (`after.sh`, 13:55 - 15:08Z; inner half again first, 617 s, all 5
      passed, STALE 563): 4,374 s of wall clock over four slots, the steps 14,861 s between them; estimate 12,559 s,
      floor 3,076 s. No refusals. 28 of 30 passed. Both ports' part 8/8 FAILED on
      `kernel.services.spool_service_sends_submitted_jobs_exactly_once` ("SpoolService: a printer did not answer an
      attach or a reset - it is unavailable"). D3 resumed after it in both, and the rest passed (aarch64 8, riscv64 5).
      Each part costs about 12 to 13 s more than its time in the guest (compile under the build lock and boot): for
      example 258 s against 245 s, and 1,866 s against 1,853 s.
    - D1's ORACLE, by `d1.py` over the guest logs (kept as hard links in `.build/tmp/d1-logs`, since G1's sweep keeps
      only a target's newest runs): the aarch64 split against the whole debug suite above. 546 compiled ids; the parts
      546 results, 546 distinct, none twice, none missing, none uncompiled. One verdict differs: the spool test, ok
      in the whole suite and failed in its part.
    - D1's CONTROL: was the spool difference load or the split? Part 8/8 run alone with nothing beside it (15:10 -
      15:45Z): `TEST_SELECTION=<that part's 270 ids, from the prepared plan> ./test.sh --arch aarch64`, memory-capped.
      FAIL, 2,134 s: 261 tests before spool `[ok]`, spool failed with the same "a printer did not answer an attach or
      a reset - it is unavailable" (`services.rs:4593`, "spool_probe inventory ran to its end"), and D3's resumed boot
      ran the 8 after it, all passed. In the whole aarch64 suite spool passed every time: in the L2 (four slots), in
      the debug run alone (35 s), and at `opt-level` 1 (3 s, in a resumed boot that started at test 390). In its part
      it failed every time: twice at four slots, once alone. So it is NOT LOAD, AND NOT THE SPLIT'S DEALING (every id
      ran exactly once). The test passes only after something a test before it in the whole suite leaves. What I
      saw: the service marks a printer unavailable when an attach is not answered within `ANSWER_TICKS` (200 ticks,
      `spool_service.rs:42`, `:442`), and the test's rig answers attaches only when it pumps (`Rig::pump`). Which
      earlier test makes it pass was not looked for. Bisecting a 276-test predecessor set is long, and the fix is the
      spool test's, outside this milestone (EXCLUDES: product bounds; the spool bound is already under OPEN).
      RESULT: D1 IS NOT VERIFIED; its oracle's equal verdict waits for that fix. D1 stays `[ ]`, and the dependence
      is recorded in the plan (D1, OPEN) and in `docs/verification/FLAKES.md` (a note beside riscv64's: no flake
      either).
    - A3, `verify-model account` over each level's own plan (`--steps`; the L0 plans regenerated with
      `verify-model tier-inner --state <scratch> --stdin` from the same paths, the L1 deferred plan as run; the
      release plan with no `--steps`):
      - L0 leaf, 1 slot: 4 steps, 88 s; host lane 54 s; longest 41 s (`build:x86_64:volume`); floor 54 s.
      - L0 kernel = L0 harness, 1 slot: 239 steps, 7,663 s; alone 11 s; host lane 7,199 s; guest 453 s; chain
        2,377 s (`stage:x86_64`); longest 1,429 s (`gate:host:capability-model`); floor 7,210 s.
      - L1 deferred, 4 slots: 30 steps, 11,934 s; host lane 120 s; guest 2,954 s a slot; longest 1,866 s (an aarch64
        part of 270); floor 2,954 s.
      - L2 (release plan), 4 slots: 374 steps, 373 priced (109 older), 1 unpriced (`gate-after-guest:host:capability-
        trace`); 69,983 s; alone 5,548 s; host lane 7,554 s; guest 14,220 s a slot; chain 10,392 s (`stage:x86_64`);
        longest 4,426 s (`guest:aarch64:all`); floor 19,768 s.
      - What no item covers, for a milestone of its own: the per-step record (16.5 s a step, serial in the runner,
        about 3,950 s of each L0 of 239 steps, about 1.7 h over the release plan), and the one host lane, which is
        2.0 h of an L0 of a change that selects everything.
      - L3's figure (its build from nothing, time and size) is measured by its first sealed run, which runs on the
        owner's request (A1) and was not made. A3 stays `[ ]` for that part alone.
  THE PLAN AND THE LEDGER after these runs (`docs/todo/P02M0206.md`, written from drafts once nothing ran):
  - B2, B6, C8, D4 and F1 checked, each with what verified it (D4: NOT ADOPTED, by its rule). D1 and A3 stay open,
    with the reasons above. The status line says the same. VERIFICATION has the L2, the three L0s, the L1 and
    `.build/logs` before and after (330,284,339 bytes before the L2, 331,045,840 after every run). OPEN has the
    defects these runs found outside this milestone: the typec and sleep-clock code, the spool test's dependence,
    `ipmi`/`sleep` under load, `lab.py`'s double close of its lock descriptor, `evidence_store_artifact`'s `.uuid`
    sidecar name, and the runner's per-step record and single host lane.
  - `docs/todo/TODO.md`: the line stays IN PROGRESS (2026-10-28), naming the two items that wait.
  - `docs/verification/FLAKES.md`: the aarch64 spool note.
  CHECKS after the last edits: `shfmt -d test.sh` clean; `./format.sh --changed` nothing to change; `./check.sh --gate
  guest-verdict,source-hygiene,milestone-index` RESULT ok (233 s; `check-guest-verdict.py` 114 tests OK, the guard's
  three resumed-boot cases among them). The guard's fix was also exercised for real: the second deferred half ran
  16 kernel-suite parts at four slots, two of them with resumed boots, and no part was refused.
  NOT PERFORMED: an L3 (the owner's to start); `--merge` of the L1 (it needs a commit, which is the owner's); a
  search for the test whose leftovers make the spool test pass on aarch64; any run of the ports after the guard's fix
  other than the deferred half and the lone part above.
  FINAL STATE (2026-10-28): every item implemented. Every item but D1 and A3 verified. D1 is blocked by the spool
  test's dependence on an earlier test, outside this milestone. A3 has one part, L3's figure, that waits for the
  owner's first sealed run. The working tree holds the guard fix (`test.sh`, `src/tools/check-guest-verdict.py`), the
  plan, TODO and ledger texts and this record, uncommitted. The scratch files in `/root/p0206-end` and the hard links
  in `.build/tmp/d1-logs` were removed after this record was written.
  THE SPOOL TEST'S CAUSE, found after the record above (2026-10-28 15:55 - 16:00Z):
  - `TEST_SELECTION=kernel.services.spool_service_sends_submitted_jobs_exactly_once ./test.sh --arch aarch64` alone:
    FAIL in 77 s, the same "a printer did not answer an attach or a reset - it is unavailable" and `spool: ` empty.
    So on aarch64 the test fails in any fresh boot, not only in its part of the split.
  - A temporary measurement in `run_spool_probe` (one `serial_println!` of `arch::apic::ticks()` from the step's
    start to the probe's spawn, the file restored from a copy afterwards and checked with `git diff --quiet`):
    "spool-diag: inventory: 196 ticks from the step's start to the probe's spawn", then the unavailable printer.
  - The mechanism, read from the code. `Rig::start` (`services.rs`) returns once every simulated printer has SENT its
    attach answer (`Sink::attach` sets `attached`), not once SpoolService has taken it. The service runs only while
    the test pumps. `run_spool_probe` then parses the packages and spawns the dynamic probe without pumping: 196 ticks
    in a fresh aarch64 debug boot. When the service runs again, its loop serves ONE ready handle (`wait_any`, the
    lowest index first) and then calls `tick()`, which marks a printer whose attach is older than `ANSWER_TICKS`
    (200, `spool_service.rs:42`, `:442`) unavailable, though that printer's answer is already in its channel. In the
    whole suite the boot is warm and the gap falls just under 200 ticks, so the test passes.
  - So the defect is in the spool service and its test, both P02M0186's: an answer that has already arrived is judged
    late, and the test lets the deadline pass without running the service. The plan excludes product bounds and
    timeouts. Fixing them here is the owner's decision, and D1's equal verdict waits on it.
  THE OWNER'S DECISIONS (2026-10-28), asked once the cause was known: "Fix the service here", and for L3 "Not yet".
  So the spool service and its probe are fixed below, as an adjacent change that D1's oracle needs. L3 is not run,
  and A3 keeps its one open part.
  THE SPOOL FIX (2026-10-28, 17:30 - 18:05Z):
  - `src/user/services/core/src/spool_service.rs`: `Service::take_answers(&mut self, buf)`, called in the main loop
    after the one served handle and before `tick()`. It reads every printer with a request outstanding through
    `on_answer`, which drains with `try_recv_caps` and does not wait, and loses a printer whose connection closed, as
    `serve` does. The keys go in a fixed array of `jobs::MAX_PRINTERS`, as `pump` keeps its lost printers, so no
    `Vec<u32>` growth routine is imported, which differs per target. So an answer that has arrived is never judged
    late.
  - Watched: before the fix the test failed alone on aarch64 twice (above). With the service fix alone it got past
    `inventory`, `two`, `stall`, `abandon`, `crash`, `detach`, `unplug` and `reset`, and then failed `recovery`:
    "an unavailable printer admitted a job".
  - `src/user/services/core/src/spool_probe.rs` `recovery`: the probe waited until the printer was "not available",
    and then expected `create` to be refused with `Io`. But a printer whose reset is still in flight is not available
    either (`Phase::Attaching`), and `create` refuses it with `Again` (`spool_service.rs`, admission). In a fresh boot
    the probe asked mid-reset. It passed only when the simulated printer happened to answer the reset between the
    probe's two requests. Now, while `create` answers `Again`, the probe waits under its existing deadline, and then
    requires `Io`. Its failure message names the refusal it got.
  - With both fixes, each run alone in a fresh boot, `TEST_SELECTION=kernel.services.spool_service_sends_submitted_
    jobs_exactly_once ./test.sh --arch A`: aarch64 PASS (66 s), with "a printer's reset failed - it is unavailable
    until it is withdrawn" and "PASS recovery"; riscv64 PASS (65 s). On riscv64 it "failed alone, two runs of two" in
    the ledger and was under OPEN, so the same defect is fixed there.
  - `./build.sh --arch aarch64` (twice, 336 s and 266 s) and `./build.sh --arch riscv64` (323 s): RESULT ok, the cross
    builds of the changed service and probe.
  - L0 of the change, `./verify.sh --for src/user/services/core/src/spool_service.rs,src/user/services/core/src/
    spool_probe.rs`: 4 inner steps (the x86_64 user, packages and volume builds and the spool test), all passed, 367 s
    of wall clock; 6 deferred keys, which are the ports' user, packages and volume builds, passed by the builds above;
    exit 5, STALE (563 keys of the lost history).
  - `./check.sh --gate spool-service`: RESULT ok (37 s), "recovery passed". `./check.sh --gate dynamic-report,
    dynamic-report-regressions`: RESULT ok (430 s), "92 tools x 3 targets, 6 waves and 3 whole images match".
    `./format.sh --changed`: nothing to change.
  WHAT D4'S MEASUREMENT LEFT IN THE PLANNER'S INPUT. `measured_test_durations` (`commands.rs`) deals the parts by the
  newest WHOLE run's guest log in `.build/logs/test`. After G1's sweep, that was D4's `opt-level` 1 run, about three
  times faster and with no time for its two failed tests, and the debug whole run of 12:00Z had been swept. So a plan
  made then dealt aarch64 into 7 parts (six alone and one of 540). The D1 run below runs the debug whole suite first,
  which gives the dealing its debug durations back.
  D1 AGAIN, ON THE FIXED TREE (`/root/p0206-d1/chain.sh`, started with `setsid`, 2026-10-28 18:11 - 19:58Z):
  - the compiled ids: `verify-model kernel-tests --arch aarch64`, 546;
  - the whole suite alone, `systemd-run --scope -p MemoryMax=16G ./test.sh --arch aarch64`: PASS, "546 passed
    (4327s)", RESULT ok in 4,340 s. It is also what the planner deals by, so the dealing used debug durations again;
  - the plan: `verify-model commands`, from which a script took the "kernel suite aarch64 part" steps and their
    build prerequisites (15 steps, a prepared plan); the planner dealt 8 parts - the six heaviest alone, one of 269
    and one of 271;
  - the split: `LIBER_VERIFY_STEPS=<that plan> ./verify.sh --jobs 4`: "all 15 step(s) passed", "measured: 2072 s
    wall clock over 4 guest slot(s); the steps took 7006 s between them" (the estimate had said 4,928 s of priced
    steps and no schedule under 1,216 s), RESULT ok in 2,090 s;
  - the oracle, a comparison of the per-test results in the guest logs: whole 546 results, 546 distinct, none twice,
    none failed; parts 546 results, 546 distinct, none twice, none failed; none only in one of them; "verdicts that
    differ: 0". The ids are the compiled ones: 546 compiled, 546 run in the whole suite, none missing, none extra, none
    twice. So every compiled id ran exactly once in the parts, and every verdict equals the whole suite's. D1 is
    checked.
  - A note on the evidence: the chain's second check read the part logs a moment after the comparison and found one
    gone. G1's sweep runs again when a top-level run ends (`lab.py`, the watcher forked by `logs_hold`), and keeps
    per target the newest run and the newest whole run, so the split's part logs are removed once `verify.sh` has
    ended; the comparison had read all eight first. The ids check was then made against the whole run's log, which
    is kept. This is G1 as designed, not a defect: the parts' verdicts are in the ledger (`verify-model record`).
  THE PLAN, TODO AND LEDGER after this (`docs/todo/P02M0206.md`):
  - D1 checked, with VERIFIED 2026-10-28: the first run's one differing verdict, the cause, the owner's decision and
    the fix, the second run, and the split's cost (2,072 s for the aarch64 suite in 8 parts at four slots, against
    4,340 s whole), replacing the "ORACLE NOT MET" paragraph this session had written and not committed.
  - The status line: every item verified but A3's L3 figure, which waits on the first sealed run; the owner said not
    yet on 2026-10-28. F1's and VERIFICATION's spool mentions say the defect is fixed; OPEN's spool entries are marked
    answered, and a bullet records the owner's two answers of 2026-10-28.
  - `docs/todo/TODO.md`: still IN PROGRESS, for A3's L3 figure alone. `docs/verification/FLAKES.md`: the spool
    note says the cause was found and fixed, and the test passes alone on both ports and in the split.
  CHECKS after these edits: `./format.sh --changed` nothing to change; `./check.sh --gate source-hygiene,
  milestone-index` RESULT ok (102 s).
  NOT PERFORMED: an L3 (the owner said "Not yet"); `--merge` of the L1 (it needs a commit, the owner's); the split of
  the riscv64 suite on the fixed tree (D1's oracle names aarch64; riscv64's spool test passes alone); the x86_64
  suite whole on the fixed tree (the L0 ran its spool test, passed); the search for which earlier test kept the
  boot warm enough to hide the defect, which the fix makes moot.
  FINAL STATE (2026-10-28, after D1's second run): every item implemented. Every item verified but A3's L3 figure
  (its build from nothing, time and size), which its first sealed run measures; that run is the owner's to start and
  is not started, so P02M0206 stays IN PROGRESS. D1 is verified, after the spool service's defect it exposed was fixed
  on the owner's word. The working tree holds, uncommitted: the guard fix (`test.sh`,
  `src/tools/check-guest-verdict.py`), the spool fix (`src/user/services/core/src/spool_service.rs`, `spool_probe.rs`),
  the plan, TODO and ledger texts, and this record. The scratch files in `/root/p0206-d1` are removed after this
  record is written.
  WHAT HOLDS A3, stated for the next reader (2026-10-28): the L3 that measures its last figure is a sealed release,
  and `src/tools/release-snapshot.sh` builds it from a detached worktree of a committed revision (`create DIR
  REVISION`). This tree's spool and guard fixes are uncommitted, and commits are the owner's; so is starting the L3,
  which the owner held back on 2026-10-28 ("Not yet"). The run was not started, and A3 stays `[ ]`: its figure is not
  estimated in place of being measured. Plan status line updated with this; `./check.sh --gate source-hygiene,
  milestone-index` to follow the edit.
  That gate run: RESULT ok (102 s).
  THE FIRST L3 (2026-10-29 00:27 - 03:18Z). The owner chose "commit, then run L3" and committed the tree (dedaee14).
  `./verify.sh --level L3`, started with `setsid` from `/root/p0206-l3/run.sh`, with a watcher that would stop it below
  10 GiB free (it never acted: the lowest free was 80.8 GB):
  - "L3: no sealed build has been measured yet - this one starts with the room there is and records its size"; the
    snapshot at `.build/release/dedaee14...`; "estimate: 374 step(s), 70190 s of priced steps ... no schedule over 4
    slot(s) finishes in under 19768 s (5.5 h)".
  - RESULT failed exit=1 in 10,305 s: "measured: 10215 s wall clock over 4 guest slot(s); the steps took 7031 s";
    245 steps ran, 224 passed, 21 failed, 129 BLOCKED; dossier `incomplete` (378 required keys, 154 refusals);
    "the sealed build from nothing took 25625459459 bytes".
  - WHY, step by step (from `/root/p0206-l3/l3.out` and the run's `logs/`):
    1. rustc died on SIGSEGV twice, the host's known intermittent crash: `build riscv64 libs` (`lsmem`, after 839 s)
       and `build x86_64 kernel` (`core`, after 1 s). Everything after them on those targets was BLOCKED: riscv64
       user..volume, x86_64 loader..volume, the images, both suites and every gate that boots a guest or reads all
       three targets. Failed as a consequence: `driver-protocol-note` ("no staged ... for: riscv64"), `build-order`
       (no bootable x86_64 volume), `dynamic-report` (no riscv64 `lsmem` reference), the six image-injection gates
       (`identity-note`, `static-image`, `undeclared-edge`, `duplicate-edge`, `malformed-symbol-relocation`,
       `malformed-dynamic`: "riscv64 baseline packaging failed"), `boot-harness` (4 scenarios name build output
       that was not built), and the verify-model host suite with `verify-model-tests` and `host-tests` (13 and 11
       tests: "no x86_64 kernel test binary has been built").
    2. `kernel suite aarch64 (whole)`, whose builds passed, failed at QEMU's start: "UNIX socket path ... Path must
       be less than 108 bytes". The test run's channel socket is `$QEMU_BUILD_DIR/dev-channel-aarch64-test.$$.sock`
       (`qemu-run.sh:3008`, as old as 2026-08-24), and under the snapshot (`.build/release/<40-hex revision>`, as
       old as 2026-09-10) that path is 130 bytes, against 74 in the tree. So no guest step can boot in a sealed
       release here; the L2s ran in place and never met it.
    3. The snapshot's `.build` starts empty, so the bootstrap's pinned third-party inputs are not there:
       `unicode-segmentation` ("ucd_* is NOT cached - run ./bootstrap.sh"), `model-mutations` and
       `capability-model` (".build/tools/tla2tools.jar is not here"), `bluetooth-codecs` ("aosp-sbc: run
       prepare-bluetooth-oracles.py --only codecs once"). The release plan has no step that provides them.
    4. `targeted-cache` writes into a source file, which the seal refuses: "src/abi/src/lib.rs: Operation not
       permitted" (`tools/check-shared-cache.sh:217`).
    5. This milestone's own: `release_finish` (`verify.sh`) records the snapshot's `.build` size for the next L3's
       room check whatever the outcome, so `.build/state/release-build-size` now holds 25.6 GB from a build that
       never built two of three targets - less than a whole one needs.
  - So A3's L3 figure is NOT measured by this run: the build from nothing did not complete, and the size and time
    above are of a partial build. A3 stays `[ ]`. Classes 2-4 are older than this milestone, and each one fails every
    sealed release on this host whatever the code does; class 1 is the host. Asked the owner how to go on.
  THE OWNER'S DECISIONS (2026-10-29), asked after the first L3: "Opravit tady, L3 znovu" - fix classes 2 to 5 here,
  then commit and run L3 again; and, asked once two more were found while fixing them, "Opravit oba" - fix 6 and 7.
  No retry of a build step that dies on a signal (class 1, the host's rustc) was asked for or added.
  THE FIXES (2026-10-29):
  - Class 2, the socket path. `verify.sh` `release_prepare`: the snapshot is `.build/release/<first 12 digits of the
    revision>`. The tree-relative UNIX sockets a release's steps make are at most 50 bytes past the root (the longest,
    `.build/boot/dev-channel-riscv64-test.PID.sock` with a 7-digit pid); a socket path has 107 bytes. Here the longest
    is 102 bytes (it was 130). Only `verify.sh` names the snapshot's directory (`check-verify-evidence.sh` makes its own
    `fixture-$$`).
  - Class 3, the prepared third-party inputs. `verify.sh` `release_seed_prepared`, called once the snapshot exists:
    every `cache` path `toolchain.lock` names (the TLA+ jar and the 21 UCD files) is copied from the tree when it is
    there, and `.build/bluetooth-oracles` - the pinned codec archives and the radio peers' venv, which only the network
    installs - is copied whole. The gates still check each against its pin; the release fetches nothing.
    Exercised on a scratch snapshot (`/root/p0206-l3/seedtest`, removed): the function copied the jar, 21 UCD files and
    the oracle tree; `./bootstrap.sh --verify` there passed for all 22 lock entries; the three archives matched
    `bluetooth-oracles-sources.json`; the copied venv's python imported `bumble` from the copy.
  - Class 4, `targeted-cache` in a sealed tree. `src/tools/check-shared-cache.sh` targeted mode: when `src/abi/src/
    lib.rs` has no owner write bit (the seal), it is unsealed (`release-snapshot.sh unseal`) for the append and its
    restoration and resealed after, as `perf-gate.py` does with its probe. Exercised on a sealed scratch tree under
    `.build/tmp` (`release-snapshot.sh seal`, immutable attribute applied): the plain append was refused ("Operation not
    permitted", the L3's failure); the gate's lines detected the seal, appended, restored the bytes (`cmp`) and resealed
    (`lsattr` `i`; a second append refused again).
  - Class 5, the size. `verify.sh` `release_finish` records `.build/state/release-build-size` only for a sealed run in
    which no step was blocked or skipped, and otherwise says why it does not. The 25.6 GB the first L3 recorded was
    removed from `.build/state` (no other file read it), so the next L3 starts "with the room there is", as the first.
  - Class 6, found while fixing the others: `host.verify-model`, `gate.verify-model-tests` and `gate.host-tests` run the
    planner's own tests, which plan real changes over the real tree; the model enumerates a target's tests from its
    newest built test kernel (`kerneltests.rs` `test_binaries`, `.build/cargo/kernel/<triple>/debug/deps`), which only a
    kernel suite compiles, and with none every plan is FULL and 13 of them fail. In the tree one was always there; a
    sealed release had none when they ran. `catalog.rs` `CHECKS_THAT_READ_THE_TEST_KERNELS` names the three, and
    `commands.rs` (`release_steps` and `steps_for_items`, through `reads_the_test_kernels`) makes each step carrying one
    require every kernel suite of its plan - as `capability-trace` already waits - so they run after the suites have
    compiled. A split suite's parts inherit the requirement (`split_suites`).
  - Class 7, found in the first L3's and the L2's timings: the aarch64 suite started 79 minutes into the L2 (12:54:30Z,
    its guest log 14:13:27Z) and 111 minutes into the L3 (00:27Z, 02:18Z), its builds long done. `order_for_slots` put
    the steps that run alone among the host steps (`soft2d-performance`, step 221 of the release plan, before every
    guest step), and the runner keeps everything after such a step from starting until it can run alone (`dispatch`,
    `reserve_all`) - so the four guest slots stood idle while the host lane worked through the 200 host steps before
    it. Now a step that runs alone is the last class of its state group (after the guest steps), so the drain it needs
    comes once, at the group's end.
  - Tests, each watched failing: `a_step_that_runs_alone_comes_after_the_guest_steps_of_its_group` (new);
    `a_check_that_reads_the_test_kernels_waits_for_the_suites_of_its_plan` (new, the real planner over a harness change
    that boots every target); and `the_release_plan_discharges_every_required_key_once_with_producers_before_consumers`
    (extended: the three wait for every whole suite and come after them). With both fixes undone in place (the class-3
    branch removed, `reads_the_test_kernels` returning false) all three failed - "the steps that run alone after the
    guest steps ...", "host:verify-model:default waits for guest:aarch64:all: []", "host.verify-model waits for the
    x86_64 suite: []" - and `commands.rs` was restored from a copy (`cmp`). With the fixes: the four ordering and plan
    tests pass, and the whole host suite `cargo test --manifest-path src/tools/verify-model/Cargo.toml`: 173 passed
    (one rustc SIGSEGV on the first compile, retried).
  - `shfmt -d verify.sh src/tools/check-shared-cache.sh` clean; `cargo fmt --check` for verify-model clean; shellcheck
    is not installed here.
  - Gates after these changes: `./check.sh --gate verify-scheduler,targeted-cache,verify-model` RESULT ok (1,370 s):
    "failed-descendant suppression, shared prerequisites, FAIL over INCOMPLETE, seeded costs and the summed guest-slot
    bound all hold", "shared-cache-check: targeted passed" (in place, the unsealed branch; `src/abi/src/lib.rs`
    restored), and the verify-model gate.
  - The owner committed the verify.sh, check-shared-cache.sh and record changes meanwhile (b7bcff48); the planner's
    changes (`catalog.rs`, `commands.rs`, `tests.rs`) and the plan, TODO and this record's newest part are in the tree.
  THE PLAN AND TODO: the status line, A3's L3 bullet (the first L3, what it found, each fix) and an OPEN bullet with the
  owner's answers of 2026-10-29; TODO's line names the second L3, after the owner's commit, as what measures the figure.
  NOT PERFORMED: the second L3, which builds a committed revision and so waits for the owner's commit of the planner's
  changes; any run of the sealed path itself before it (the socket length and the seeding were checked by length and on
  a scratch tree; the gates that need them run in the L3).
  STATE (2026-10-29): every item implemented; every item verified but A3's L3 figure; P02M0206 IN PROGRESS until the
  second L3 has measured it.
  THE SECOND L3 (2026-10-29 16:35Z, after the owner committed b7bcff48 and fa6d5953): `./verify.sh --level L3` from
  `/root/p0206-l3/run.sh` with the disk watcher, the snapshot at `.build/release/fa6d59530aa7` (the 12-digit name),
  the prepared inputs seeded (the jar, 21 UCD files, the oracle tree with its venv - checked in the snapshot).
  - "estimate: 374 step(s), 71914 s of priced steps ... no schedule over 4 slot(s) finishes in under 19769 s".
  - Every build from nothing passed: 21 build steps, no BLOCKED step, 3,266 s of step time, the last ended at
    17:35:54Z (59.5 minutes after the run began). The watcher's last line, 23:36:08Z: 58.6 GB free on `/data`, the
    snapshot 48,275,732,170 bytes.
  - At step 337 of 374 the VM's kernel oopsed (2026-10-30 00:36:45 CET, `Oops: stack segment` in
    `__rmqueue_pcplist+0x6f8`, a non-canonical list pointer, in a `python3` page fault), then RCU stalls and soft
    lockups (`qemu-system-x86`), and the host froze until the owner reset it at 03:41 CET. The run never ended: no
    dossier, no `release-build-size`, no `measured:` line. A3's figure is NOT measured; the numbers above are a partial
    run's.
  - 18 steps had failed by then. Code defects already under OPEN: `arch-profile-{aarch64,riscv64}-no-dt-absent-1`, the
    three IOMMU hostile and transition profiles, `tickless-idle`, `system-suspend-riscv64`, `typec-tcpci-riscv64`. And,
    read from the run's step logs: `boot-harness` (4 shipped scenarios: "no such file" under the snapshot),
    `dynamic-report` (x86_64 `dmesg`, `free`, `lscpu` rows that do not match), `verify-evidence` (its nested fixture's
    `volume-layout` envelope "records failed"), `implementation-mutations` (two mutants "failed the suite, but not by
    the assertion it was written against"), the development lifecycle ("abiprobe is audit-produced and was not staged"),
    `qemu-virtio-iommu-x86_64` ("the bootable volume was signed for DMA mode 'harness'"), `capability-trace`,
    `bluetooth-independent`, `boot-volume-controllers-aarch64` and `typec-ucsi` (the development agent never answered,
    then the `lab.py` double close - "Bad file descriptor"). Recorded under OPEN, for milestones of their own; none of
    them stops A3's figure, which needs only an L3 that ends with nothing blocked.
  THE HOST, investigated on the owner's request (2026-10-30, read only - nothing removed or changed):
  - `journalctl --list-boots`: the boots ending 2026-10-20 00:44 CEST, 2026-10-27 07:30 CET and 2026-10-30 00:36 CET
    each end in a kernel oops followed by RCU stalls and soft lockups: a NULL dereference in `sched_balance_newidle`
    (6.12.111, `qemu-system-aar`), and twice the same `__rmqueue_pcplist+0x6f8` stack-segment fault (6.12.113, `nm`
    and `python3`). `kernel.panic`, `panic_on_oops`, `softlockup_panic` and `hung_task_panic` are all 0, so the VM
    hangs instead of rebooting. Not tainted.
  - `atopsar` at each crash: 150 to 190 GB of 251 GB free, no swap configured, 1 to 3 of 100 cores busy; guest (nested
    KVM) time on 2026-10-30, none on 2026-10-27. So neither load, memory nor nested KVM is required for it.
  - rustc segfaults or general-protection faults in every boot (9, 3, 1, 5 and 4 in the last five), in different
    libraries. The balloon device negotiates statistics only and never inflated.
  - Disk: `/` 54 GB free (43 % used); `/data` 55 GB free (95 %): `.build/cargo` 224 GB, `.build/unsafe-audit` 57 GB,
    the crashed run's sealed snapshot 45 GB, two P02M0177 copies of 48 GB each and its two worktrees of 16 GB each,
    `/data/logs/screen` 14 GB; the journal 217 MB, `/var/log` 1.1 GB (atop 884 MB).
  - Proposed to the owner, not done: `kernel.panic_on_oops=1` and `kernel.panic=10`, so the VM reboots instead of
    hanging; the host's own log for machine-check or EDAC errors, or the provider; a memory test (`memtester`) or
    `debug_pagealloc=on`; and removing the crashed run's snapshot with `release-snapshot.sh remove` before any L3.
  STATE (2026-10-30): every item implemented; every item verified but A3's L3 figure, which waits for an L3 that ends -
  on the owner's word, after the crashed run's snapshot is removed and with the host's crashes in mind. P02M0206 stays
  IN PROGRESS.
  THE OWNER'S ANSWERS (2026-10-30): "Ano, nastav" - `/etc/sysctl.d/90-panic-reboot.conf` written (`kernel.panic_on_oops
  = 1`, `kernel.panic = 10`) and applied with `sysctl -p` (read back: 1 and 10); "Ano, odstraň" - `src/tools/
  release-snapshot.sh remove .build/release/fa6d59530aa7`, rc 0: the directory is gone, the worktree no longer listed,
  the four other worktrees untouched, `/data` from 55 to 100 GB free; "Až po kontrole hostitele" - no L3 is started
  until the owner has had the host checked and says so. The plan's OPEN and status say the same. P02M0206 stays IN
  PROGRESS for A3's L3 figure alone.
