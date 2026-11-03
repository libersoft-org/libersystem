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
  THE HOST CHECK, left to the implementer by the owner (2026-10-30: "to bys mel asi zkontrolovat ty"): the VM cannot see
  its physical host, so what it can check was checked. `memtester 8G 1` twenty times side by side (160 of 251 GB,
  `/root/memtest`, under `systemd-run --scope -p MemoryMax=170G`): all 20 finished their loop with no failure, in 77
  minutes. `apt-get update` then `apt-cache policy linux-image-amd64`: 6.12.113-1 is the newest. No oops, lockup or
  segfault in the kernel log since the reset of 2026-10-30 03:41 CET. That rules out plainly bad memory, not an
  intermittent fault below the VM.
  THE THIRD L3 (2026-10-30 23:32:03Z - 2026-10-31 07:45:51Z, at 23df6e6a, `/root/p0206-l3/run.sh` with the disk watcher;
  the second run's files moved to `/root/p0206-l3/second`):
  - "estimate: 374 step(s), 76958 s of priced steps (355 priced under an older model, 1 never measured); no schedule
    over 4 slot(s) finishes in under 20730 s (5.8 h)".
  - IT ENDED: RESULT failed exit=1 in 29,628 s; "measured: 29523 s wall clock over 4 guest slot(s); the steps took
    66267 s between them"; 21 steps failed, 4 BLOCKED; the dossier `incomplete`. The host did not crash.
  - THE BUILD FROM NOTHING: the 21 build steps all passed, 3,264 s of step time; the last build step's log was written
    at 00:32:20Z, 60 minutes after the run started (run id 20261030T233244Z).
  - THE SIZE: "the sealed build's size is not recorded - 4 step(s) were blocked and 0 skipped" - the rule of
    2026-10-29 counted any blocked step, and the riscv64 suite's failure blocked the three test-kernel readers and
    `capability-trace`. So a second watcher (`/root/p0206-l3/size.sh`, started at 03:50Z when the four blocked steps
    were seen) sampled `du -sb` of the snapshot's `.build` each minute: 50,236,422,147 bytes at its largest (04:22:52Z),
    48,150,596,434 at 07:44:21Z after the last step, 47,714,685,717 at 07:45:21Z, the last sample before the snapshot
    was removed. The disk watcher's lowest free figure on `/data` was 57.8 GB.
  - So A3's L3 figure is measured by a sealed run that ended: the build from nothing 3,264 s of steps and 60 minutes of
    wall clock; its `.build` 47.7 GB at the end, 50.2 GB at its largest. `.build/state/release-build-size` was written
    by hand with the largest (50,236,422,147), which is the room the next L3 is checked against; the run would have
    written its end size had the rule been right.
  - The first guest step, the aarch64 suite, started at 23:54:08Z, 21 minutes in (its guest log's name), where it
    waited 79 minutes in the L2: the class-7 fix holds in a real run.
  - Failed: the code defects already under OPEN (`arch-profile-{aarch64,riscv64}-no-dt-absent-1`, the three IOMMU
    hostile and transition profiles, `system-suspend-riscv64`, `tickless-idle`, `qemu-pcie-hotplug`); the second L3's
    `boot-harness`, `verify-evidence`, `implementation-mutations`, `dynamic-report`, `boot-volume-controllers-aarch64`,
    `bluetooth-independent`, `typec-ucsi` and the development lifecycle; and new: `soft2d-performance` ("image-convert
    ... OVER", "a scene is over its frozen budget"), `watchdog` (the development agent never answered, then "Bad file
    descriptor"), `bluetooth-input` ("bt-fixture did not bind (iommu-required)"), `qemu-modem-service` and the riscv64
    suite - the last two fixed below.
  THREE FIXES AFTER IT (2026-10-31, the run over, nothing else running):
  - `verify.sh` `release_finish`: the size is recorded unless a BUILD step (`build:*`) failed or was blocked. Exercised
    on fake runs (the rule's lines run under `bash -euo pipefail`): a failed suite and a blocked reader - 0 unbuilt; a
    blocked build - 1. `shfmt -d` clean.
  - `src/user/services/core/src/spool_probe.rs`: the riscv64 suite failed on "spool-probe: FAIL reset: a job of 1000
    bytes was refused: Exhausted" - the reset step closed its two jobs and made a third at once, while the service still
    counted the closed ones against the grant context's two (the step dates from 2026-09-24). `Probe::admitted` waits
    out `Exhausted` under a 200-tick deadline, as the inventory step's own loop does, and `send`'s staging and submit
    are `Probe::submitted`, which the reset step now uses on the admitted job. It is a race, not watched failing on
    demand: the failure is the L3's own log. Then `./build.sh --arch A` and `TEST_SELECTION=kernel.services.
    spool_service_sends_submitted_jobs_exactly_once ./test.sh --arch A`: x86_64 PASS (26 s), riscv64 PASS (55 s),
    aarch64 PASS (54 s); the builds RESULT ok (298, 276 and 296 s).
  - `test.sh`'s guard against stray guests: `qemu-modem-service` failed in one second on "a qemu-system-x86_64 already
    holds this tree's disk images: 1878728 (.../boot/virtio-serialport-dev-liber-tcpci.0KM44w.1878728.out)" - the
    typec-tcpci gate's private development instance, whose files `qemu-run.sh` names after its state directory
    (`-dev-$(basename "$LIBER_DEV_STATE")`, a `mktemp` name with a dot), which no pattern of the guard knew. Steps that
    ran one after another never met it; overlap did. Another state's set (`boot/{virtio-blk,virtio-console,
    virtio-serialport,fat-media,iso-media,udf-media,usb-media}-dev-*`) is admitted now; the run's own state's set still
    refuses. `check-guest-verdict.py` `test_another_development_states_set_is_admitted_and_its_own_is_not`, five names:
    before the fix all five refused ("23 != 0"); after it the module's 115 tests pass. A real `./test.sh --arch x86_64`
    selection passed (32 s).
  - Gates: `./check.sh --gate spool-service,verify-scheduler` RESULT ok (383 s); `--gate dynamic-report,
    dynamic-report-regressions` RESULT ok (430 s), "41 checks passed"; `--gate guest-verdict` RESULT ok (132 s).
    `./format.sh --changed`: nothing to change.
  THE PLAN, TODO AND LEDGER: A3 checked with the third L3's text; the status line COMPLETE (2026-10-31); OPEN has the
  third L3's findings, the three fixed here and the rest for milestones of their own; TODO's line `[x]` COMPLETE;
  `docs/verification/FLAKES.md` has `soft2d-performance`, `watchdog` and `typec-ucsi` and the spool test's reset race.
  NOT PERFORMED: another L3 after these three fixes (none of them changes the build from nothing that A3 measures);
  the spool test in a whole riscv64 suite after the fix; `--merge` of the L1 (it needs a commit, the owner's).
  FINAL STATE (2026-10-31): every item of P02M0206 implemented and verified, A3 included; the milestone COMPLETE. The
  working tree holds, uncommitted: `verify.sh`, `test.sh`, `src/tools/check-guest-verdict.py`,
  `src/user/services/core/src/spool_probe.rs`, the plan, TODO, the ledger and this record.
  THE SIZE RULE, HELD TO A RUN (2026-10-31, after the record above): A1 says the first sealed run "records its size for
  the next", and the third L3 did not; the fixed rule had been exercised only on its own lines. `src/tools/
  check-verify-scheduler.sh` case 13: a committed fixture tree (`git init` in the gate's temporary directory), released
  sealed by the real `verify.sh --release` over a real `release-snapshot.sh` snapshot, with a prepared plan of a build
  step, a suite that fails and a reader the suite blocks - "a sealed release whose suite failed and blocked a reader
  still records its build's size", "and says so"; and with the build step failing - "one whose build failed records
  none", "and says why". The fixture needed `.build/state`, which the tree always has. Watched: with `release_finish`'s
  condition put back to the old one (any blocked or skipped step) the gate failed on the first two checks
  ("expected '1', got '0'") and `verify.sh` was restored from a copy (`cmp`); with the fix `./check.sh --gate
  verify-scheduler` RESULT ok (345 s). `shfmt -d` clean.
  VERIFICATION in the plan has the L3 beside the L2: 21 failed and 4 blocked against 20 and 1; two of the L2's seven
  harness failures (`implementation-mutations`, `boot-volume-controllers-aarch64`) failed again sealed and pass in the
  tree (OPEN). The plan's VERIFICATION asks for one L2 with its failures set beside the baseline, which is recorded,
  and for A3's account, which now has the L3 figure; it does not ask an L3 to pass.
  FINAL STATE (2026-10-31, after this): unchanged - P02M0206 COMPLETE; the working tree also holds
  `src/tools/check-verify-scheduler.sh`.

IMPLEMENTER'S FOURTH L3 AND WHAT IT FOUND ON P02M0206 (2026-11-01, appended; nothing above is changed)
  THE RUN: `./verify.sh --level L3` at 0399f460, the committed tree that closed the milestone, started on the owner's
  word ("Commitnul jsem, spusť L3") at 01:28:25Z under `/root/p0206-l3/run.sh`, with the disk watcher (stop below
  10 GiB) and the `.build` sampler beside it; it ended at 10:26:33Z, exit 1, 32,288 s.
  - The room check ran for the first time: "L3: 105852293120 bytes free, and the last sealed build took 50236422147".
  - The build from nothing: 21 build steps, all passed, 3,326 s between them, the last ending 61 minutes in (the
    history's `last_run`); the aarch64 suite started 22 minutes in.
  - "measured: 32184 s wall clock over 4 guest slot(s); the steps took 68765 s between them"; the estimate was
    75,659 s of priced steps and a floor of 20,666 s.
  - The run recorded its size itself: "release: the sealed build from nothing took 47733479018 bytes". The sampler saw
    the same `.build` reach 50,657,664,712 bytes (06:27:50Z), and the disk watcher saw /data's free space fall to
    57,072,873,472 at 07:13:15Z, 48,779,419,648 below the 105,852,293,120 free at the start.
  - 21 steps failed, none was blocked; the dossier "incomplete - 378 row(s), 378 required key(s), 25 refusal(s)".
  WHAT IT VERIFIED OF THE THIRD L3'S FIXES: the riscv64 suite passed (the spool probe's reset); `qemu-modem-service`
  passed beside development instances (the guard); the run wrote its size without a hand; nothing was blocked.
  AGAINST THE THIRD L3: gone `bluetooth-input`, the riscv64 suite, `qemu-modem-service`, `soft2d-performance`; new
  `capability-trace` (blocked in the third), `brightness-acpi`, `hibernate`, `targeted-cache`.
  WHAT ITS LOGS SHOW, step by step (the run's kept logs; the snapshot's own logs went with it):
  - `watchdog`, `typec-ucsi`, the development lifecycle: each instance's serial log has "dma: claim of
    virtio-console at 00:1e.0 under entry `dev_channel` REFUSED - the named entry is not one this image declares" and
    "DeviceManager: dev-channel did not bind (iommu-required)"; the L2's lifecycle log has "bind window dev_channel = 61
    tick(s)" and no refusal, the third L3's has the same refusal. The kernel's DMA registry carries `dev_channel` only
    when built with `LIBER_DEVELOPMENT=1` (`src/kernel/build.rs`), so these instances booted a shipping kernel. The
    three booted one image (sha256 218e2349...), a `mkimage` cache hit whose key covers the kernel and the volume, so
    the shipping kernel was in the shared staging when that image's inputs were snapshotted. Who put it there: the
    runner's own lines in the development group - "needs stage:x86_64 in 'development', and it is in 'no recorded
    state'" (parts in two profiles) and a volume "in 'shipping/harness'" - and the steps that write those states: the
    development image's producer, declared `profile:shipping` like every producer (`resources::step_declaration`) and
    run as `./image.sh --format iso --dma-mode harness`, which builds in its caller's profile; and five gates declared
    `profile:development` that boot the lab's guest by `lab.sh boot` (`check-qemu-{pcie-hotplug,pcie-aer,2d-demo,
    3d-demo,2d-account}.sh`), whose `cmd_boot` builds `image_command()` in the caller's environment - shipping under
    `verify.sh`, which sets no profile. `qemu-pcie-hotplug` ran beside the three in this run. Only `audio-routing`
    passed `LIBER_DEVELOPMENT=1`. The second L3's entry ("abiprobe is audit-produced and was not staged") and the
    third's ("the development agent never answered" under load) named the wrong cause. NOT REPRODUCED: the race
    itself; `bluetooth-input`'s "did not bind (iommu-required)" in the third L3 and `ipmi`/`sleep` in the L2 are likely
    the same and not shown to be.
  - `verify-evidence`: "evidence: this run produced no libersystem.iso". It ran at step 267, the shipping image's
    producer at 287; the catalog's `GATE_IMAGE_INPUTS` did not list it although `check-verify-evidence.sh:129` reads
    the run's image. The order had hidden that since 2cf1de38.
  - `capability-trace`: "the newest x86_64 trace is older than the kernel beside it" (also the second L3). The suite
    boots a staged copy of the TEST kernel; the check compares with `.build/cargo/kernel/x86_64-unknown-none/debug/
    kernel`, which the shipping image's producer (step 287, "Compiling kernel" after "build.sh: kernel (x86_64)")
    relinked after the suite (step 256). It did not run in the L2 (the plan's second-L3 list said it passed there).
  - `boot-volume-controllers-aarch64`: "aarch64's AHCI boots need firmware with SATA drivers - build it:
    src/tools/build-aavmf-ahci.sh" - `.build/firmware/AAVMF_{CODE,VARS}.ahci.fd` not in the snapshot.
  - `boot-harness`: `test_every_shipped_scenario_still_validates` - "no such file .../.build/fixtures/x86_64/
    {uname-shadow,fixture-text,process-proto-shadow}" for four scenarios; the set is made by `make-fixtures.py` in
    development runs only.
  - `implementation-mutations`: five of five "failed the suite, but not by the assertion"; the suite runs in
    `.build/mutations/tree`, and under the snapshot its guest's channel socket
    (`.../.build/mutations/tree/.build/boot/dev-channel-x86_64-test.PID.sock`) is 123 bytes, past the 107 a UNIX socket
    path has (95 in place). Inferred from the path, not from the guest's log, which went with the snapshot.
  - `bluetooth-independent`: "PAN network process exited"; the PAN socket under its log directory is 125 bytes sealed
    (97 in place). Inferred the same way.
  - `dynamic-report`: "docs/DYNAMIC_EXECUTABLES.tsv is stale", 276 rows: `undefined_imports` name the `rt` crate's
    mangled hash `_RNvCscyLjaQ4FWMZ_2rt...` committed and `_RNvCsayKYdNeLnv3_2rt...` built, and `provider_bytes`
    differ (451080 against 451216) - the report depends on where the tree is built. Not this milestone's.
  - `qemu-pcie-hotplug`: run in place on 2026-11-01 with the development profile (below) it fails the same way, "the
    slot was powered down at line 1, before the driver let go at line 2" - code, as recorded.
  - `hibernate` (boot 13 "late-refusal: the instance did not come up"), `brightness-acpi` ("the region holds 70, not
    60"), `targeted-cache` (rustc SIGILL in `system-manifest`): in the ledger, seen once.
  FIXES (2026-11-01, after the run ended, nothing else running):
  - `src/tools/verify-model/src/resources.rs` `step_declaration`: a producer's profile is the one its command builds in
    (`LIBER_DEVELOPMENT=1` first); `catalog.rs` `IMAGE_PRODUCERS`: the development image's command is
    `LIBER_DEVELOPMENT=1 ./image.sh --format iso --dma-mode harness`. `tests.rs` `a_steps_declaration_follows_from_
    what_it_is` asserts its tokens; with the old catalog it failed with "stage:x86_64=w:shipping",
    "volume:x86_64=w:shipping/harness" - the run's own "shipping/harness".
  - `check-qemu-{pcie-hotplug,pcie-aer,2d-demo,3d-demo,2d-account}.sh`: `LIBER_DEVELOPMENT=1` on `lab.sh boot`, as
    `check-audio-routing.sh` has it. `tests.rs` `a_development_gate_boots_the_labs_guest_in_the_development_profile`:
    every non-comment `lab.sh boot` line in the scripts of a gate whose declaration names the development profile
    carries it; with the hot-plug gate's prefix removed it failed naming that line.
  - `catalog.rs` `GATE_IMAGE_INPUTS` lists `verify-evidence`; `tests.rs` `a_gate_that_reads_a_runs_image_requires_
    the_producer_of_it` reads every `$(evidence_image NAME)` in the scripts a gate reaches and requires the producer of
    NAME (the bootable volume's is the shipping image's); with the old list it failed: "gate `verify-evidence` reads
    the run's `libersystem.iso` ... and does not require `image.libersystem-iso`: []".
  - `src/tools/check-capability-trace.sh`: inside a run (`LIBER_VERIFY_RUN`) the newest x86_64 guest log the run
    kept that carries "captrace: begin" is the trace, and it is not held to the kernel file's age; otherwise as
    before. `LIBER_VERIFY_RUN=<the fourth L3's run> check-capability-trace.sh --require-live`: "the live trace matches
    the reference and replays against the model"; outside a run, today's tree: "the newest x86_64 trace is older than
    the kernel beside it", the release's failure.
  - `verify.sh`: `release_seed_prepared` copies `.build/firmware/AAVMF_{CODE,VARS}.ahci.fd` (sparse); and the size:
    `release_watch_room` samples `df` every 2 s from before the snapshot is made until the run's shell is gone, and
    `release_finish` records the larger of the end's `du -sb` and the start's free space less the lowest. Case 13 of
    `check-verify-scheduler.sh`, "the room recorded is the run's fullest, not its end" (a build holds 64 MiB of
    `fallocate` for five seconds): passes; with `needed=$sealed` it failed ("expected '1', got '0'"), `verify.sh`
    restored from a copy (`cmp`).
  - `src/harness/scenarios/make-fixtures.py --names` prints the fixture names and builds nothing;
    `harness-test.py` validates the shipped scenarios against those names in the test's own build root, with the
    tree's staged image linked in. The 11 `FixtureSetTest` tests pass, and the one passes with `.build/fixtures` moved
    aside; the old check with it moved aside refused exactly the four scenarios of the run.
  - Plan order after the fixes (`./verify.sh --release --plan`): `verify-evidence` at 267 requires
    `producer:x86_64:image.libersystem-iso` (240); the development image's producer moved from among the shipping
    steps to 331, beside the development gates.
  - Checks: `cargo test` in verify-model, 175 passed; `verify-model check`, "model is consistent"; `./check.sh --gate
    verify-scheduler,boot-harness` RESULT ok (440 s); `shfmt -d` clean on every changed script.
  - The five gates with the development profile, run in place one guest at a time (`/root/p0206-fix4`):
    `qemu-pcie-aer` RESULT ok (142 s), `qemu-2d-demo` ok (149 s), `qemu-2d-account` ok (558 s), `qemu-3d-demo` ok
    (191 s); `qemu-pcie-hotplug` failed on its removal order (223 s), as above.
  - `./check.sh --gate source-hygiene,milestone-index` RESULT ok (103 s); `./format.sh --changed`: nothing to change.
  NOT PERFORMED: a sealed run of these fixes - they need a commit, the owner's, and a fifth L3 on the owner's word;
  the race that put a shipping kernel under the development instances, which is read from the logs and not
  reproduced; any fix of the socket paths, which waits for the owner's choice (per site, or a snapshot beside the
  repository whose root is as long as the tree's); the `ipmi`, `sleep` and `bluetooth-input` failures, which are
  likely the same cause and not shown to be.
  THE PLAN, TODO AND LEDGER: the status line IN PROGRESS (reopened 2026-11-01); A3 has the fourth L3; VERIFICATION has
  it beside the third; OPEN has "Found by the fourth L3", which corrects the second and third L3's entries for
  `capability-trace`, the development lifecycle, `watchdog` and `typec-ucsi`; TODO's line `[ ]` IN PROGRESS; the
  ledger has `hibernate`, `brightness-acpi`, `targeted-cache`, and `watchdog`/`typec-ucsi` as not flakes.
  FINAL STATE (2026-11-01): P02M0206 IN PROGRESS. The working tree holds, uncommitted: `verify.sh`,
  `src/tools/verify-model/src/{catalog,resources,tests}.rs`, `src/tools/check-capability-trace.sh`,
  `src/tools/check-verify-scheduler.sh`, `src/tools/check-qemu-{pcie-hotplug,pcie-aer,2d-demo,3d-demo,2d-account}.sh`,
  `src/harness/scenarios/make-fixtures.py`, `src/harness/harness-test.py`, the plan, TODO, the ledger and this record.
  THE SOCKET PATHS, ANSWERED (2026-11-01, after the record above): the owner chose "Snapshot vedle repa" (the snapshot
  beside the repository), then "Commitnu, pak spusť L3".
  - `verify.sh` `release_snapshot_path`: the snapshot of REVISION is `<the repository's parent>/.r<first digits>`, the
    name as long as the repository's (eleven characters beside `libersystem`: `/data/yellow/.r0399f4606`, 24 bytes as
    `/data/yellow/libersystem` is), so every path in a sealed release is as long as in the tree; a name shorter than
    nine keeps seven digits. `release_prepare` refuses a directory at that path that `git worktree list` does not
    name, before the path becomes the run's (so no cleanup removes it); the L3's room check and `release_watch_room`
    read the free space of the snapshot's parent.
  - `check-verify-scheduler.sh` case 13, its fixture tree renamed `sealed-tree` (eleven characters): "the snapshot is
    beside the tree, its root as long as the tree's", "and is gone when the run ends", "a directory at the snapshot's
    path that is not this repository's worktree is refused", "and left as it was". `./check.sh --gate
    verify-scheduler` RESULT ok (364 s). Watched: with the old location (`$BUILD_DIR/release/<12>`) and no refusal
    the gate failed - "expected '/tmp/tmp.Ppe2bpIjQL 31', got '/tmp/tmp.Ppe2bpIjQL/sealed-tree/.build/release 59'",
    the foreign marker gone ("got ''") - and `verify.sh` was restored from a copy (`cmp`). `shfmt -d` clean.
  - NOT PERFORMED: the two gates in a sealed run (the fifth L3 is that run).
  FINAL STATE (2026-11-01, after this): P02M0206 IN PROGRESS; every finding of the fourth L3 that is this milestone's
  or its sealed path's is fixed in the tree; the owner commits, and the fifth L3 verifies them.
  - Then `source-hygiene` refused the fixture's `sed ... | head -1` (a pipefail script piping into a reader that stops
    early); it reads the line with `awk ... {print $NF; exit}` now. `./check.sh --gate source-hygiene,milestone-index`
    RESULT ok (102 s) and `--gate verify-scheduler` again RESULT ok (363 s), the four checks above among its passes.
  THE DEVELOPMENT GROUP THROUGH THE SCHEDULER (2026-11-01 19:34-20:30Z, while the commit is the owner's): the release
  plan's eleven development-group steps (`gate-guest` qemu-2d-account, qemu-3d-demo, serial-handoff, watchdog,
  typec-ucsi, qemu-pcie-hotplug, qemu-2d-demo, qemu-pcie-aer, audio-routing; the development image's producer; the
  lifecycle), keys and declarations kept, as a prepared plan (`LIBER_VERIFY_STEPS=/root/p0206-fix4/plan-devgroup.txt
  ./verify.sh --jobs 4`, in place, the fixes in the tree; a `--release --in-place` run refuses `LIBER_VERIFY_STEPS` as
  an undeclared override). 3,375 s; "measured: 3357 s wall clock over 4 guest slot(s); the steps took 4766 s".
  - Passed: qemu-2d-account 560 s, qemu-3d-demo 184 s, serial-handoff 200 s, the producer, qemu-2d-demo 158 s,
    qemu-pcie-aer, audio-routing, typec-ucsi 1,029 s ("PASS - UCSI 1.2 and 2.1 ..."), watchdog 1,819 s ("PASS - the
    i6300esb, the TCO, a WDAT over the TCO and the BMC's ..."). The two that failed in the fourth L3 ran beside the
    five `lab.sh boot` gates and passed.
  - The staging: switched once into the development profile at the start (the tree had been left mixed by the gates
    run before), then only between the development volume's modes; no "shipping/harness" and no "no recorded state"
    inside the group.
  - Failed: `qemu-pcie-hotplug` (its removal order, code); the lifecycle - its instance came up ("readiness: shell
    prompt and agent handshake"), `dev.selftest` passed (25 s), `dev.proto-test` failed in its fuzz ("the guest never
    completed a handshake", the code defect under OPEN since the L2), and `dev.perf-gate` and `dev.gpu-restart` then
    found "the development instance is starting".
  - The run's end put the staging back in the shipping profile and the volume `enforcing-required`.
  NOT PERFORMED: the same in a sealed run - the fifth L3, after the owner's commit.
  THE ROOM FOR THE NEXT L3: `.build/state/release-build-size` was 47,733,479,018 (the fourth run's end, by the old
  rule) and holds 48,779,419,648 now, written by hand: what the changed rule takes from that run - the larger of the
  end's `.build` and the disk it took at its fullest, here by the disk watcher's one-minute samples.
  FINAL STATE (2026-11-01, last): P02M0206 COMPLETE again. Every finding of the fourth L3 that is this milestone's or its
  sealed path's is fixed and verified by its oracle, watched failing first, and by the gates it changes; the
  development group passed through the scheduler but for `qemu-pcie-hotplug`'s removal order and the lifecycle's
  protocol fuzz, code defects recorded under OPEN for milestones of their own (A3's rule). The plan's VERIFICATION does
  not require an L3 to pass; the fifth sealed run, which the owner asked for after committing, is NOT PERFORMED - the
  tree is uncommitted, and a sealed release refuses a dirty tree. The plan's status line, its OPEN and TODO's line say
  so. The working tree holds, uncommitted, the 13 source and script files listed above, the plan, TODO, the ledger and
  this record.
  ANSWERED (2026-11-01): asked whether P02M0206 stays COMPLETE or goes back to IN PROGRESS until the fifth sealed run,
  the owner chose "Nechat COMPLETE, commitnu" - it stays COMPLETE, the owner commits, and the fifth L3 follows as the
  confirmation; a finding of it that is this milestone's reopens it. Recorded in the plan's status.

IMPLEMENTER'S FIFTH L3 AND WHAT IT FOUND ON P02M0206 (2026-11-02, appended; nothing above is changed)
  THE RUN: `./verify.sh --level L3` at df0cfad1, the owner's commit of the fourth L3's fixes (the owner's record of the
  answer above followed as dc6d9b94 while it ran), started at 22:42:06Z on 2026-11-01 under `/root/p0206-l3/run.sh`,
  with the disk watcher and the `.build` sampler beside it; it ended at 07:10:09Z, exit 1, 30,483 s.
  - The room check: "L3: 104879861760 bytes free, and the last sealed build took 48779419648". The snapshot beside the
    repository: "release snapshot: df0cfad1... at /data/yellow/.rdf0cfad1e".
  - The build from nothing: 21 build steps, all passed, 3,326 s between them, the last published at 23:43:24Z, 61
    minutes after the run's start (22:42:45Z).
  - "measured: 30379 s wall clock over 4 guest slot(s); the steps took 72052 s between them"; the estimate was 74,960 s
    of priced steps and a floor of 20,541 s.
  - The size, by the changed rule: "release: the sealed build from nothing took 48522143005 bytes, and the run
    49633091584 bytes of the disk at its fullest - 49633091584 recorded for the next L3"; `.build/state/
    release-build-size` holds 49633091584. The external disk watcher's lowest free space, 55,354,019,840, is the
    run's own low (start 104,987,111,424 less 49,633,091,584). The sampler's largest apparent `.build`,
    51,591,602,024 at 04:11Z, counts sparse images whole.
  - 14 steps failed, none was blocked; the dossier "incomplete - 378 row(s), 378 required key(s), 17 refusal(s)".
  WHAT IT VERIFIED OF THE FOURTH L3'S FIXES (the run's envelopes): `watchdog` passed (1,850 s), `typec-ucsi` (1,000 s),
  `verify-evidence` (489 s), `capability-trace` ("the live trace matches the reference and replays against the
  model"), `boot-harness` (78 s), `boot-volume-controllers-aarch64` (1,647 s), `implementation-mutations` ("5
  deliberate kernel defects, each caught by the assertion written against it"), `bluetooth-independent` (1,228 s); the
  lifecycle's instance: "bind window dev_channel = 60 tick(s) from BIND to READY", "readiness: shell prompt and agent
  handshake", `dev.selftest` passed; the five `lab.sh boot` gates passed but `qemu-pcie-hotplug` (code); the
  run recorded its room at its fullest. Also passed: `hibernate`, `brightness-acpi`, `targeted-cache` (each seen once
  in the fourth), `ipmi` and `sleep`.
  AGAINST THE FOURTH L3: gone the eleven above (`watchdog`, `typec-ucsi`, `verify-evidence`, the gates that read a guest
  run, `boot-volume-controllers-aarch64`, `boot-harness`, `implementation-mutations`, `bluetooth-independent`,
  `hibernate`, `brightness-acpi`, `targeted-cache`); still the code defects (`arch-profile-{aarch64,riscv64}-no-dt-
  absent-1`, three IOMMU profiles, `dynamic-report`, `system-suspend-riscv64`, `tickless-idle`, `qemu-pcie-hotplug`,
  the lifecycle's development checks); new `smp-core-cap`, `virtio-gpu-edid-aarch64`, `typec-tcpci-riscv64`, `balloon`.
  WHAT ITS LOGS SHOW (the run's kept logs):
  - `smp-core-cap` (step 288, 2 s): "test.sh: a qemu-system-x86_64 already holds this tree's disk images: 1266901
    (/data/yellow/.rdf0cfad1e/.build/mutations/tree/.build/logs/test/x86_64-20261102T010409Z-1250280-restart1-run.log)".
    The holder was the mutation suite's resumed boot, running in `check-implementation-mutations.sh`'s copy of the
    tree (`WORK="$REPO/.build/mutations/tree"`, filled by `rsync`, no links) in another guest slot. `test.sh`'s
    `require_no_stray_qemu` reads every writable descriptor under the tree's `.build/`, and the copy's own `.build` is
    under it; no rule admitted it. In place the copy is under `.build` too, so the overlap, not the snapshot, made it:
    in the fourth L3 the mutation suite failed on its socket path before it booted a guest. THIS MILESTONE'S (C4, the
    guard, under C2's overlap) - the third L3's `qemu-modem-service` refusal was the same class.
  - `typec-tcpci-riscv64`: "vbus-stays-off: detached after 1240 ms, before tSrcRecover and tSrcTurnOn (1275 ms) had
    passed" - the step that fails alone (the ledger's "not a flake"); passed in the third and fourth. Code.
  - The lifecycle: `dev.proto-test` passed its taxonomy, publication, terminal, registry and launch sections ("restarting
    the agent, so nothing is launched..." then ten "ok"), then "FAIL 16 pipelined requests are answered in order, 32768
    B each: (1, {(4, 0)}, False, True) expected (16, {(4, 0)}, True, True)", "round trip for the whole burst: 90026 ms",
    and "-- fuzz / proto-test: the guest never completed a handshake"; the instance's `dev-serial.log` ends with
    "agent.dev: online (registry)", "DeviceManager: a provider-catalogue client closed its connection; the slot is given
    back" and a bare "system_manager". The 2026-11-01 rehearsal had the pipeline pass (287 ms) and the fuzz fail. The
    protocol defect under OPEN since the L2, one section earlier; cause NOT ESTABLISHED.
  - `balloon`: "the program that needed 768 MiB did not get it (... launched memhog as koid 3126 memhog: an object could
    not be mapped ...)", after the first two stages passed as before (512 MiB given; stopped at 1219 MiB with 229 MiB
    free). The instance's serial log was in its private state under `/tmp`, removed by its trap. Passed in the third and
    fourth.
  - `virtio-gpu-edid-aarch64`: "edid-oracle: FAIL no acknowledged provider-backed presentation followed discovery",
    "EDID on failed on aarch64 (see /data/yellow/.rdf0cfad1e/src/../.build/logs/virtio-gpu-edid-aarch64)" - gone with
    the snapshot. Nothing rebuilt the aarch64 staging while it ran (the runner's lines in that window are x86_64's);
    `hibernate-ports-aarch64` ran beside it. Passed in the third and fourth.
  - `dynamic-report`, the IOMMU and `arch-profile` profiles, `system-suspend-riscv64`, `tickless-idle`,
    `qemu-pcie-hotplug`: as recorded for the fourth.
  FIX (2026-11-02, after the run ended, nothing else running):
  - `test.sh` `require_no_stray_qemu`: after `relative="${target#"$build/"}"`, `[[ "$relative" == */.build/* ]] &&
    continue` - a file under another tree's `.build` nested in this one's is that tree's; the copy's own guard answers
    for it. Only that descriptor is skipped; a file of this tree the same process holds still refuses.
  - `src/tools/check-guest-verdict.py` `ExistingQemuAdmission.test_another_trees_build_nested_in_this_one_is_admitted`:
    three nested files (the run's own holder's log name, a nested virtio-blk image, a nested `libersystem.iso`, each
    writable, with another owner's pid) admitted; each beside a writable `boot/libersystem.iso` of this tree refused;
    each refused with the exemption removed (`mutation="nested_tree_is_ours"`, which asserts the line is there once).
  - Watched failing: with `test.sh` from HEAD (copy kept, restored, `cmp` equal) the test failed on all three subtests,
    "AssertionError: 23 != 0" at its first assertion - the guard's refusal.
  - Checks: `check-guest-verdict.py ExistingQemuAdmission` 7 tests OK; `./check.sh --gate guest-verdict` RESULT ok
    (132 s, 116 tests); `shfmt -d test.sh` clean.
  NOT PERFORMED: the fix in a sealed run (a sixth L3, after the owner's commit); a live reproduction with two guests -
  the fixture runs the guard's own text against real descriptors with `pgrep` stubbed; the causes of `balloon`'s and
  `virtio-gpu-edid-aarch64`'s failures, whose guest logs are gone; the cause of the lifecycle's protocol failure.
  THE PLAN, TODO AND LEDGER: the status line COMPLETE (2026-11-02, reopened the same day), the sixth sealed run named as
  not performed; C4 has the nested tree; A3 has the fifth L3; VERIFICATION has it beside the fourth; OPEN has "Found by
  the fifth L3"; TODO's line says the same; the ledger has `balloon` and `virtio-gpu-edid-aarch64` (seen once), the
  fifth's passes of `hibernate`, `brightness-acpi`, `targeted-cache`, `ipmi`, `sleep`, and `typec-tcpci-riscv64`'s
  failure again.
  FINAL STATE (2026-11-02): P02M0206 COMPLETE again. The fifth L3's one finding of this milestone is fixed and verified
  by its oracle, watched failing first, and the gate it changes. The working tree holds, uncommitted: `test.sh`,
  `src/tools/check-guest-verdict.py`, the plan, TODO, the ledger and this record.

IMPLEMENTER'S RECORD: A FAILED STEP'S LOGS KEPT IN THE RUN (2026-11-02, appended; nothing above is changed)
  ASKED: after the fifth L3 the implementer proposed that a sealed run keep the guest logs of its failed steps before its
  tree is removed, two of its failures having been unreadable; the owner answered "OK, udelej to". Implemented over
  5edc358a (the owner's commit of the C4 guard fix and the fifth L3's record).
  CORRECTION of the fifth L3's record above: `balloon`'s logs were not removed by its trap - `check-balloon.sh:43-53`
  copies them from its private state to `.build/logs/balloon` in its EXIT trap; in the sealed run that directory was in
  the snapshot and went with it, as `virtio-gpu-edid-aarch64`'s did. The plan's OPEN and the ledger say so now.
  WHAT WAS BUILT:
  - `verify.sh` `run_one_step` writes the wall clock it began at as a third field of its outcome file
    (`printf -v began '%(%s)T' -1`); `record_one_step` reads it, and on a failed step inside a run calls
    `keep_failed_step_logs` before `release_publish_fallback` removes the step's output.
  - `keep_failed_step_logs INDEX KEYS_FILE BEGAN`: under the step's first key, through `evidence_keep` (the model's
    `keep-log`), it keeps (1) for a step `./check.sh --gate NAME`, the directories `.build/logs/NAME`, `NAME.*`,
    `NAME-{x86_64,aarch64,riscv64}`, and NAME without an architecture suffix (`BASE`, `BASE.*`) - only their files
    newer than BEGAN (`find -newermt`); (2) every path the step's output names that contains `.build`, resolved
    (`realpath -m`; a relative one against `src/` and then the tree root) inside the tree's `.build` and outside
    `.build/runs`: a directory under `.build/logs` other than `.build/logs/test`, the same way; a regular file under
    `.build/logs` or ending in `.log`. Deduplicated; each passed to `keep-log` as a symlink named by its path under
    `.build` with `/` as `+`, so the copy carries the path; at most `FAILED_STEP_LOG_BYTES` (64 MiB) a step in path
    order, the rest named in a note ("not kept, past the ... bytes a failed step keeps: ..."). It says what it kept:
    "kept N log file(s) this failed step wrote or named, B bytes, under 'KEY'".
  - Not added: `NAME-*` in general - `ipmi-admin` beside `ipmi` and `hibernate-ports` beside `hibernate` are other
    gates' directories, and gates overlap.
  - For a gate the envelope is published by `check.sh` before the runner keeps these, so it does not name them; they
    sit in the same `logs/<key>/` directory. For a step the runner publishes for, `kept_logs` names them.
  ORACLE: `src/tools/check-verify-scheduler.sh` case 14 - a sealed release over the committed fixture tree of case 13,
  whose `check.sh` writes `.build/logs/NAME/serial.log`, a 65 MiB `huge.log` (sparse), an `old.log` dated an hour back,
  `.build/boot/fixture-boot.log` and `.build/state/fixture.elf`, names the last two, and fails for `fixture-gate` and
  passes for `passing-gate`. Checked after the run, with the snapshot gone: the run holds
  `logs+fixture-gate+serial.log` and `boot+fixture-boot.log` with their contents, and not `state+fixture.elf`,
  `logs+fixture-gate+old.log`, `logs+fixture-gate+huge.log` (named in the note instead) or `logs+passing-gate+
  serial.log`; and the note "kept 2 log file(s) ... under 'gate.fixture-gate / host / host / default'". The fixture's
  `cargo` (case 11's recorder, which case 13 copies) learned `keep-log` as a copy of each `--log` by its name.
  - The first fixture run put `keep-log` in case 10's recorder, which case 11 overwrites before case 13 copies it: the
    positive checks failed with the new runner too, so the first watch-fail proved nothing and was discarded. Moved to
    case 11's recorder; then, in this order: with the new `verify.sh` the whole gate passed (rc 0, every case); with
    `verify.sh` from HEAD (copy kept, restored, `cmp` equal) it failed exactly the four checks that need the feature -
    "a failed gate's own log is in the run - expected 'fixture-gate serial', got ''", "and a log its output names",
    "nor a file past the bound, which is named instead - expected 'absent 1', got 'absent 0'", "and the run says what
    it kept".
  - The real `keep-log` (`cargo run -- keep-log`, scratch run directory, two path-named symlinks): two regular files
    `logs/gate.balloon+host+host+default/logs+virtio-gpu-edid-aarch64+guest` and `.../logs+balloon+serial.log`, each
    holding the target's bytes, not links.
  - The extraction over the fifth L3's kept step logs reads: `virtio-gpu-edid-aarch64` - `/data/yellow/.rdf0cfad1e/
    src/../.build/logs/virtio-gpu-edid-aarch64`; the IOMMU gicv2-hostile profile - its four `.build/logs/test/
    aarch64-...` logs and `.build/state/kernel-test-aarch64.1025785.elf` (dropped: no log); `tickless-idle` -
    `../.build/boot/tickless-{aarch64,riscv64,x86_64}.log`; `smp-core-cap` - the nested tree's run log; `balloon` -
    nothing named (its directory by name). The run's own evidence paths are under `.build/runs` or outside the
    sealed tree and are dropped.
  CHECKS: `shfmt -d verify.sh src/tools/check-verify-scheduler.sh` clean; `bash -n verify.sh`. `./verify.sh --plan`
  for this change selects 87 % of a full run (a harness change selects everything); not run - the sixth L3 is the run
  for it, on the owner's word.
  NOT PERFORMED: a real failed step in a real run keeping real logs (the sixth L3 will show it, or not if nothing
  fails); the 64 MiB bound against a real failure's logs.

IMPLEMENTER'S SIXTH L3 AND WHAT IT FOUND ON P02M0206 (2026-11-03, appended; nothing above is changed)
  THE RUN: `./verify.sh --level L3` at f5296c8e, the owner's commit of the kept failed-step logs (5edc358a, C4's
  nested-tree guard, before it), started at 22:31:36Z on 2026-11-02 under `/root/p0206-l3/run.sh`, with the disk
  watcher and the `.build` sampler beside it; it ended at 07:12:17Z, exit 1, 31,241 s.
  - The room check: "L3: 104203849728 bytes free, and the last sealed build took 49633091584". The snapshot: "release
    snapshot: f5296c8e... at /data/yellow/.rf5296c8e4"; it is gone after the run.
  - The build from nothing: 21 build steps, all passed, 3,303 s between them, the first published at 22:32:42Z and
    the last at 23:32:34Z.
  - "measured: 31138 s wall clock over 4 guest slot(s); the steps took 72891 s between them"; the estimate was 75,224 s
    of priced steps and a floor of 20,572 s.
  - The size: "release: the sealed build from nothing took 48268338924 bytes, and the run 49517023232 bytes of the disk
    at its fullest - 49517023232 recorded for the next L3"; `.build/state/release-build-size` holds 49517023232. The
    external watcher, sampling each minute, saw 54,925,316,096 free at its lowest; the sampler's largest apparent
    `.build`, 51,527,505,285 at 04:03Z.
  - 11 steps failed, none was blocked; the dossier "incomplete - 378 row(s), 378 required key(s), 14 refusal(s)"
    (the lifecycle's step carries four of the keys).
  WHAT IT VERIFIED OF THE TWO COMMITTED CHANGES:
  - C4's nested tree: `smp-core-cap` passed in 41 s ("a machine past the supported count boots on 64 cores, says so,
    and loses nothing"); it started at step 262 while `implementation-mutations` (step 257) was between its catches,
    and that gate passed too ("5 deliberate kernel defects, each caught by the assertion written against it").
  - The kept logs: five failed steps printed the note - the three IOMMU profiles "kept 4 log file(s) ..., 14372 /
    14518 / 18579 bytes", `system-suspend-riscv64` 4 files, 183,515 bytes, `tickless-idle` 3 files, 578,778 bytes;
    no "not kept" note. In the run, `logs/gate.iommu-aarch64-direct-gicv2-hostile+.../logs+test+aarch64-...-guest.log`
    holds "not confirmed detached"; `logs/gate.system-suspend-riscv64+.../logs+system-suspend-riscv64+unsupported-
    serial.log` holds "the SBI System Suspend extension is not offered by this firmware"; `logs/gate.tickless-idle+.../
    boot+tickless-{x86_64,aarch64,riscv64}.log`. No kernel `.elf` was copied.
  - The six failed steps that kept nothing, and why: `arch-profile-{aarch64,riscv64}-no-dt-absent-1` (the gate keeps
    its own `<profile>.log` with its envelope, and the output names no `.build` log); `dynamic-report` and
    `soft2d-performance` (host gates; the output is the evidence); the development lifecycle (keeps `dev-serial.log`,
    `dev-qemu.log`, `up.log`, `down.log` and each check's log itself, as in every run since 2026-10-26);
    `qemu-pcie-hotplug` (its lab state is `mktemp -d "${TMPDIR:-/tmp}/liber-lab.XXXXXX"`, removed by its `cleanup` on
    EXIT; nothing under `.build`). `grep` over `src/tools/check-*.sh` for that `mktemp` and a later `.build/logs` copy
    finds four more with no copy: `check-audio-routing.sh`, `check-dfu-tool.sh`, `check-qemu-pcie-aer.sh`,
    `check-tpm-tool.sh`. Recorded as a limit under OPEN, not changed.
  THE ELEVEN FAILURES, each against the fifth's `l3.out` (`/root/p0206-l3/fifth`, the sixth's in `.../sixth`), the same
  message the same number of times in both: "loaded a kernel after refusing the DMA mode" (2 and 2), "was not confirmed
  detached" (3, 3), "SBI System Suspend extension is not offered" (2, 2), "an idle core woke" (3, 3), "the slot was
  powered down at line 1" (1, 1), "DYNAMIC_EXECUTABLES.tsv is stale" (both). Then:
  - `soft2d-performance`: "image-convert ... 16.386 ... 16.063 ... OVER", running alone (`ALONE_GATES`); the third L3's
    line was "16.119 ... 16.063 ... OVER". Passed in the fourth and fifth. FLAKES.md now has it as not a flake.
  - The development lifecycle: `dev.selftest` passed; `dev.proto-test` "FAIL 16 pipelined requests are answered in
    order, 32768 B each: (1, {(4, 0)}, False, True)", "round trip for the whole burst: 90030 ms", then "proto-test: the
    guest never completed a handshake"; `dev.perf-gate` and `dev.gpu-restart` "the development instance is starting".
    The fifth's kept logs (its run directory, `20261101T224245Z-df0cfad1-...`) show the same: 90,026 ms, the same
    tuple. Both serial logs: no panic (`grep -ciE "panic|fault at|oops"` 0), "agent.dev: online (registry)" twice -
    the boot's and the restart `proto-test` asks for before its launch section ("restarting the agent, so nothing is
    launched..."), so the fifth's record "ends mid-line after the agent's restart" read that restart as more than it
    was. `lab.py` names an instance "starting" when its broker holds the lock and `WAIT 3` on the control socket
    sees no prompt (`dev_state`, `dev_ready`).
  THE LIFECYCLE ALONE (the oracle for load against code): `./check.sh --gate development-lifecycle` in the tree at
  f5296c8e (only FLAKES.md changed, under docs/), no other guest on the machine (`ps -eo comm` showed no qemu), output
  in `/root/p0206-fix5/lifecycle-alone.log`: up, `dev.selftest` passed (24 s), `dev.proto-test` FAILED (exit 1, 69 s)
  with a traceback ending in `group_fuzz` -> `fuzz_rounds` -> `Peer.send` -> `sendall` "TimeoutError: timed out";
  `dev.perf-gate` and `dev.gpu-restart` "the development instance is starting"; "3 of 4 development checks failed";
  `RESULT failed exit=1 seconds=327`. In place there is no run directory, so only the tail of `proto-test`'s output is
  printed and whether its pipeline line passed is not shown; 69 s is less than the pipeline group's 60 s send window
  plus a 30 s read that a stall costs, so it did not stall there. The in-place rehearsal of 2026-11-01
  (`/root/p0206-fix4/rehearsal.out`): "ok 16 pipelined requests ... 287 ms", then "the guest never completed a
  handshake". So it fails with nothing beside it, one group later than under load: not load, not a flake, not this
  milestone's - the protocol defect listed since 2026-10-20/21.
  RECORDED: P02M0206.md - the status's last sentence, A3 (THE SIXTH L3), G1 (as a release sees it), VERIFICATION,
  OPEN ("Found by the sixth L3"); TODO.md's line; FLAKES.md (`soft2d-performance` not a flake; the passes of the
  sixth beside each entry seen). Outputs archived in `/root/p0206-l3/sixth`.
  NOT PERFORMED: the 64 MiB bound against a real failure (the largest step kept 578,778 bytes); a seventh L3.

AUDITOR'S RE-AUDIT ON P02M0206 (2026-11-03T13:10:51Z):

SCOPE AND METHOD
  This file has no earlier auditor's section, so this is the first audit of the implementation. It is checked against
  `docs/todo/P02M0206.md` (items A to G, VERIFICATION, EXCLUDES, the owner's decisions and OPEN), the plan's line in
  TODO.md and `docs/verification/FLAKES.md`, by reading the code the records name at HEAD 6f5b200d (`git diff a97a9ec0
  HEAD`). The code read: `verify.sh`, `test.sh`, `lib.sh`, `build.sh`, `src/tools/staging-state.sh`, `guest-gate.sh`,
  `evidence.sh`, `result-logs.sh`, `check-shared-cache.sh`; the verify model (`resources.rs`, `commands.rs`,
  `catalog.rs`, `history.rs`, `evidence.rs`, `identity.rs`, `tier.rs`, `shadow.rs`, `main.rs`); `src/harness/`
  (`lab.py`, `guest-console.py`, `test-kernel.sh`, `perf-gate.py`); and `src/kernel/tests.rs`.
  Run, host-only, all passed:
    - verify-model `cargo test` (175 passed), `verify-model check` ("model is consistent"), `verify-model release-plan`
      (374 steps, 11 that run alone), `tier-inner` over a planner change, `./verify.sh --age`;
    - harness-test.py LogsRulesTest, LogsLockTest, GuestConsoleTest and CompletionLineTest;
    - check-guest-verdict.py KernelSuiteResumes and GuestGateStopsWithItsDriver.
  Simulated in a scratch directory: `suite_logs_kept`, the resume in `test-kernel.sh`, and `guest-console.py`.
  No guest, no level and no suite was run.

FINDINGS

1. HIGH - E1: a continued L2 does not carry the earlier run's image artifacts, so every image-reading gate it runs
   again fails.
   Where: src/tools/verify-model/src/evidence.rs:189-215 (`Run::continue_from`); src/tools/evidence.sh:103-112
   (`evidence_image`); verify.sh:1773-1810; src/tools/verify-model/src/main.rs:2118-2120.
   Evidence:
     - `continue_from` copies only each passed envelope's `logs` (`for log in &envelope.logs`, :200) and republishes
       the envelope (:212). Nothing copies the earlier run's `artifacts/`.
     - A step all of whose keys were taken over is printed `COVERED` (main.rs:2120) and decided "CONTINUED" without
       running (verify.sh:1805). That includes the image producers.
     - Inside a run, `evidence_image` reads only `$LIBER_VERIFY_RUN/artifacts/NAME`. Otherwise it fails with
       "evidence: this run produced no $name ... a gate does not take one from the tree instead" (evidence.sh:110).
   Impact:
     - Take E1's own case: an earlier L2 whose image producer passed early, and whose later steps failed or never
       ran. In the run that continues it, four gates fail because of the harness, not the code: `signed-boot`,
       `secure-boot`, `qemu-virtio-iommu-x86_64` and `verify-evidence`. These are the four scripts that call
       `evidence_image`.
     - The dossier of the continued run also names producer outputs that its run does not hold.
     - The L2 of 2026-10-26 is an example: `signed-boot` failed there after its producer had passed.
     - The oracles cannot see this. `check-verify-evidence.sh` section 5 continues a plan of two gates with no
       producer, and `check-verify-scheduler.sh` case 11 stands in for `continue-run` (:585).
   Resolution: when a producer's envelope is taken over, take the earlier run's `artifacts/` files with it, digests
   checked as for logs. Alternatively, never cover a producer step whose artifact a pending step reads. Add that case
   to section 5.

2. MEDIUM - A1: the L1 preset stops after its inner half whenever the tier ends STALE or SHADOW, which today is every
   run in this tree. Its passing path was never run or tested.
   Where: verify.sh:312-333; src/tools/verify-model/src/tier.rs:535-560; verify.sh:1925-1941.
   Evidence:
     - The preset requires `((inner_status == 6)) && [[ -n "$handoff" ]] || die "L1: the inner tier did not end with
       a handoff (status $inner_status)"` (:327). It then requires `"$0" --merge ... || die "L1: the merge did not
       pass"` (:329) before it writes `verify-last-passed`.
     - An inner run exits 6 only at INCOMPLETE. `evidence_level` returns STALE first whenever a key of the catalogue
       is past the age window and not selected (tier.rs:553-555). The runner then exits 5 (4 for SHADOW) after it
       has printed the handoff, unless `--allow-shadow` is given.
     - `./verify.sh --age` here says "292 of 2074 keys are stale ... (292 have never run)". They include the ten
       umbrella gates (`gate.hibernate-ports`, `gate.iommu-ports`, ...), which the catalogue says nothing can select
       (catalog.rs:924), so no level ever clears them.
     - The records agree. All three L0s and both runs of the L1's inner half ended "exit 5, STALE".
     - The L1 was run as two halves started by hand, and `--merge` never ran. `check-verify-scheduler.sh` case 12
       tests only the preset's refusals. `.build/state/verify-last-passed` does not exist.
   Impact:
     - The working rule "at a milestone's end L1" cannot be followed with `./verify.sh --level L1`.
     - The preset stops after the inner half with a message that is false, since the handoff was written. It never
       runs the merge and never records BASE.
     - The records' "until a full run records them" (VERIFICATION, the L0s) does not hold either: six L3s later,
       the 292 keys still say "never".
   Resolution: let the preset accept 4 or 5 from the inner half when it printed its handoff, run the merge, and
   record BASE when the merge's steps all passed, saying which level it ended at. Or state in the preset that L1
   needs `--allow-shadow`. Either way, add the preset's passing path to case 12.

3. MEDIUM - D3 regression: after a resumed suite, `verify.sh --shadow` grades only the last resumed boot, so a failed
   sweep can be recorded as consistent.
   Where: src/harness/test-kernel.sh:906 and :926 (and :823, :868, :890); src/tools/result-logs.sh:18;
   verify.sh:39-47 and :823-824; src/tools/verify-model/src/shadow.rs:56-99 and :462-467.
   Evidence:
     - The first boot prints `RESULT-LOGS` (:906). `resume_after` (:926) then starts a nested `test-kernel.sh`,
       whose own `RESULT-LOGS` line comes after it. `result_logs` takes `... | tail -1`, the last line.
     - The shadow sweep ignores test.sh's status (`./test.sh --arch "$target" 2>&1 | tee "$sweep_capture" || true`)
       and parses only that last log.
     - The resumed boot prints "running K tests (all tags)" and "test suite complete: K passed" for the tests after
       the failure. So `total_declared` is K, every outcome passed, and the Void guard (`outcomes.len() < total`)
       does not fire. The failed test appears only in the first boot's log.
     - Simulated with the real `test-kernel.sh` in the KernelSuiteResumes fixture: the last line names
       `...-restart1-guest.log` and is followed by "PASS: test suite complete".
     - Before D3, the log ended with the failing test still open and parsed as Failed.
   Impact: a sweep with a failing test reads as clean. The shadow comparison then misses a candidate miss and can
   record trust it has not earned. The gates that call `result_logs` check test.sh's status first, so they are not
   affected.
   Resolution: print the first boot's `RESULT-LOGS` last, after `resume_after`. Alternatively, make the shadow read
   every boot of the run, or refuse a sweep whose status is not zero.

4. MEDIUM - C1/C2/C5: some steps write what their declarations say they only read, and they now overlap guests that
   check it.
   Where: src/tools/verify-model/src/commands.rs:470 (at a97a9ec0, :275 had `exclusive_guests: true`) and
   tests.rs:2926; src/harness/perf-gate.py:52, :61-83 and :124-163; resources.rs:216, :237, :358 and :431;
   src/tools/check-shared-cache.sh:222-224; test.sh:313; lib.sh:466.
   Evidence:
     - The development lifecycle lost its run-alone flag in this milestone. It is now `exclusive_guests: false`,
       locked by the test "the lifecycle's instance is its own, with its own port", and no record says so.
     - Its step runs `dev.perf-gate`, a wall-clock gate measured "on an idle host": `NO_CHANGE_BUDGET = 1.0` and
       `PHASE_BUDGETS = {'build': 3.6, 'publish': 0.6, 'run': 2.6}`. C5 says timing-sensitive gates run alone.
     - perf-gate also appends a probe to `src/user/apps/tools/src/uname.rs` and rebuilds the staged `uname` with
       `dev-build.sh`. Yet the step is declared `stage:x86_64:r` (resources.rs:237).
     - `targeted-cache` appends to `src/abi/src/lib.rs` across a `dev-build.sh` call (check-shared-cache.sh:222-224).
       It is declared `host stage:x86_64:w` only.
     - `abi` and `user` are both in `VOLUME_SOURCES` (lib.sh:466). On every architecture `test.sh` compares that
       digest when it starts, and on a mismatch it dies with exit 1 ("the build does not match the sources",
       test.sh:313). That is a failure, not a refusal.
     - Since the fourth L3's fix, `boot-harness` validates the shipped scenarios against the tree's real
       `.build/image/x86_64-unknown-none` (harness-test.py:1286-1287). It is still declared plain `host`, and its
       exemption still says "against fakes and copies in temporary trees" (resources.rs:431).
   Impact:
     - Before this milestone, the barrier and the lifecycle's run-alone flag kept these steps apart. Three things
       can now happen:
         - an aarch64 or riscv64 `test.sh` that starts inside either source edit fails for the harness (a suite, a
           part, an IOMMU row or an arch-profile row);
         - the lifecycle's timing budgets are measured beside up to three guests;
         - a staging switch can run under `boot-harness`.
     - This is latent so far. In every run since 2026-10-26, perf-gate found the instance "starting" and measured
       nothing, and no such failure is in the kept logs.
     - These are the false failures C1 and C2 exist to prevent. C1's check cannot see them, because it counts only
       `build.sh` and `image.sh` as writers.
   Resolution:
     - Run the lifecycle alone again, or record the decision not to.
     - Declare `targeted-cache` a writer of every architecture's staging.
     - Declare the lifecycle a writer of `stage:x86_64`.
     - Declare `boot-harness` a reader of `stage:x86_64`.

5. LOW - A1 and the first L3's class 6: L0 no longer runs the planner's own tests for a change that selects
   everything.
   Where: src/tools/verify-model/src/commands.rs:481 and :673; tier.rs:162-176.
   Evidence:
     - `host.verify-model`, `gate.verify-model-tests` and `gate.host-tests` now require every kernel suite of their
       plan, the ports' suites included. The inner tier defers whatever requires a deferred step.
     - `tier-inner` over `src/tools/verify-model/src/history.rs` gives 820 inner keys and 1241 deferred. All three
       of these are deferred; only `gate.verify-model` (`check`) stays inner.
     - A1 lists what L0 sets aside: the ports, the development guests and the guest-booting gates. Host tests are
       not on that list.
     - A3's L0 figures (2026-10-28) predate this change.
   Impact: an L0 of a change to the planner, the harness, the kernel or `rt` does not run the planner's unit tests;
   they wait for L1.
   Resolution: require only the suites that a step of the same tier runs (in place, one test kernel is always
   there), or require them only in a sealed tree.

6. LOW - G1 with D1: a failed part's or selection's logs are removed the moment the run that made them ends.
   Where: src/harness/lab.py:2687-2694 (`suite_logs_kept`) and :2804-2823 (`logs_hold`).
   Evidence:
     - For each target, `suite_logs_kept` walks the runs newest first and stops at the first passed run.
     - A part of the same split that started later and passed counts as "a later run of that target".
     - The holder sweeps once more after its owner exits, which is the implementer's addition beyond the plan. That
       sweep applies the rule at once.
     - Simulated: four aarch64 parts with part 2 failed. Part 2's run and guest logs are removed.
     - The implementer met this in the first L1. The parts' logs were "kept as hard links in `.build/tmp/d1-logs`,
       since G1's sweep keeps only a target's newest runs" (this file, line 1082).
   Impact: an L1 or an L0 has no run directory, and `keep_failed_step_logs` needs one. So the guest log of a failed
   part is gone before anyone reads it, which is the opposite of "keeps what a person reads".
   Resolution: count a run as superseded only by a later passing run that covers it (a whole run, or the same
   selection). Alternatively, keep a failed run's logs until the start sweep of the next top-level run.

7. LOW - D3 in a tag run: resuming after the last requested non-smoke test stops on the kernel's filter check.
   Where: src/kernel/tests.rs:3661-3663, applied to the tests after `TEST_AFTER`.
   Evidence:
     - The check `if selected_non_smoke == 0 { serial_println!("test filter error: requested tags selected no
       non-smoke tests"); arch::exit_qemu(false); }` is evaluated over the remaining tests only.
     - So the resumed boot fails at once and prints a second FAIL with its log tail.
     - Any smoke tests after the resume point never run.
   Impact: D3's oracle, "runs every later test", is met only for whole runs and selections. The verdict stays
   failed.
   Resolution: skip that check on a resumed run.

VERIFIED AS CORRECT (not repeated item by item)
  - These items do what the plan and the records say: B1, B2, B5, B6, B7, C3, C4 (with its nested-tree and
    development-state exemptions), C6, C7, C8, D1, D2, E2, F1, F2 and F3.
  - The declarations cover all 198 gates and the release plan's 374 steps.
  - The dispatcher overlaps only steps whose declarations do not conflict, and switches each staging state once per
    group.
  - A refused step publishes no envelope and is retried once.
  - The room rule records the run at its fullest.
  - A release run keeps its failed steps' logs, within the 64 MiB bound.
  - The owner's decisions are followed: no time figures, D4 at the end, the snapshot beside the repository, and
    failed-step logs kept in the run. D4's "not adopted" follows its own rule.
  - The plan's status, TODO.md's line and the ledger agree with each other and with the records.

Rating: 7/10. Most of the milestone is carefully implemented and verified by oracles watched failing and by six
sealed runs. Against that: E1's continuation fails the image gates in its main case, the L1 preset cannot complete
in this tree, D3 silently weakens the shadow comparison, and several overlap declarations understate what their
steps write.
