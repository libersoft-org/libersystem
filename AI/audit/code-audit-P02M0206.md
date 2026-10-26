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
