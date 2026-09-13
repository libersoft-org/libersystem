AUDITOR'S REVIEW OF PLAN P02M0179 (2026-09-13T15:27:43Z):

**Rating: 4/10.** Material corrections are required to the terminal-status contract, trap integration, watchdog lifecycle, and acceptance coverage before the plan's completion claims can be accepted.

Reviewed [P02M0179](/data/yellow/libersystem/docs/todo/P02M0179.md), the milestone index, the five entry points and shared helpers, the guest watchdog, the new regression gate, testing documentation, and the verification catalog and release invariants. References describe the working tree at commit `30de4a62b357d26e184b169b3b9ff2d0da420b3a`. Because the plan is marked COMPLETE and contains implementation and verification claims, those claims were checked against the existing implementation. The findings below concern corrections needed in the plan and its acceptance criteria.

1. **High — M1 does not account for later trap replacements or `exec`, so ordinary entry-point paths still end without a verdict.**

   [M1](/data/yellow/libersystem/docs/todo/P02M0179.md:58) says the verdict chains onto cleanup and claims all relevant entry points arm it after guest cleanup. However, [check.sh](/data/yellow/libersystem/check.sh:14) arms the verdict before `install_guest_cleanup`, whose [EXIT registration](/data/yellow/libersystem/lib.sh:64) replaces it. `verify.sh` also replaces the verdict in its [release path](/data/yellow/libersystem/verify.sh:361), [sweep path](/data/yellow/libersystem/verify.sh:430), [display path](/data/yellow/libersystem/verify.sh:506), [shadow path](/data/yellow/libersystem/verify.sh:543), and [ordinary executor](/data/yellow/libersystem/verify.sh:766). Its [catalog, model-hash, age, and trust branches](/data/yellow/libersystem/verify.sh:445) use successful `exec`, which replaces the shell without running its EXIT trap.

   This affects routine checks and verification, including the entry point whose silent death motivated the milestone. `check.sh --help` returned 0 with neither a verdict nor a requested status file. Temporary Cargo stubs also demonstrated missing verdicts and status files for `verify.sh --for lib.sh --plan` and `verify.sh --catalog`.

   **Correct M1** to inventory every cleanup registration and process-replacement branch, then specify how each preserves cleanup, the original exit status, and exactly one outer verdict. Merely arming near the top cannot satisfy this architecture. Require acceptance cases through these real paths, including failure after a later cleanup registration, before retaining the DONE claim.

2. **High — M2 has no invocation ownership or publication lifecycle, so file existence can falsely announce completion or another run's result.**

   [M2](/data/yellow/libersystem/docs/todo/P02M0179.md:77) promises that the file exists exactly when the requested run has ended, but does not address reused paths, nested entry points, or atomic publication. [The helper](/data/yellow/libersystem/lib.sh:125) directly truncates and writes the exported `RUN_STATUS_FILE`; arming neither removes an old terminal record nor stops descendants from inheriting its destination. Existing [verification orchestration](/data/yellow/libersystem/verify.sh:270) invokes multiple other verdict-producing entry points. Direct redirection also exposes the terminal filename before all its fields have been written.

   In a real `verify.sh` prepared-plan fixture, `./gen.sh --list` was followed by a sleeping step that failed. While the outer run was still alive, its status file already said `script=gen.sh`, `outcome=ok`, `exit=0`. That success remained after `verify.sh` exited 1 because of finding 1. Even after fixing the outer trap, the premature completion indication would remain. Separately, a reused success file stayed present throughout a subsequent live invocation. Running `RUN_STATUS_FILE=<temporary path> ./check.sh --gate run-verdict` returned 0 but left `script=signalled.sh`, `outcome=failed`, `exit=143` in the outer file.

   **Correct M2** to define one owner per terminal destination, prevent children from publishing into their parent's file, and either safely initialize reused destinations or require fresh unique paths. Resolve relative destinations before execution changes directory, and publish complete fields atomically. Require tests that observe absence throughout an outer run containing successful children, then verify the outer identity and actual result after failure. Apply the corrected semantics to M5's reader instructions.

3. **High — The promised treatment of abrupt death is not feasible with an in-process EXIT trap and best-effort file alone.**

   The [problem statement](/data/yellow/libersystem/docs/todo/P02M0179.md:35) explicitly includes OOM kills, while [M1](/data/yellow/libersystem/docs/todo/P02M0179.md:58) and [Done when](/data/yellow/libersystem/docs/todo/P02M0179.md:134) promise a verdict on signals and a status file exactly when a run has ended. A shell killed by SIGKILL cannot execute a trap; this includes an OOM kill of the shell. A bounded fixture sourcing the actual helper, arming the verdict, and killing its own process group with SIGKILL terminated with neither line nor file. Furthermore, M2 explicitly makes publication best-effort, so failed writes can also leave a completed run without a file.

   Consequently, absence cannot establish that a run is still running. [M5's `setsid` rule](/data/yellow/libersystem/docs/todo/P02M0179.md:122) can isolate a session/process group, but cannot protect against SIGKILL, OOM, or a launcher that terminates descendants through a different mechanism. Shell exit itself is also not a universal process-tree kill rule, as the testing documentation asserts.

   **Correct M1, M2, M5, and Done when** to state the supported termination and publication guarantees. If abrupt-death detection remains required, specify an observer outside the failure domain and an explicit missing/unknown-result interpretation. If that mechanism is outside scope, narrow the guarantee to handled exits and teach readers that an absent terminal file is inconclusive. Test catchable interruption during real child execution as well: [the existing lifecycle guidance](/data/yellow/libersystem/lib.sh:28) already explains Bash's deferred traps behind foreground children, which a self-signalling fixture does not exercise.

4. **High — M4's claimed proof substitutes argument validation for the required build and stale-image failures.**

   [M4's requirement](/data/yellow/libersystem/docs/todo/P02M0179.md:105) specifically demands a deliberately broken build and a deliberately stale image, each producing both a failed verdict and a status file. Its DONE narrative substitutes five different cases. The [actual gate](/data/yellow/libersystem/src/tools/check-run-verdict.sh:57) rejects an unknown part and an unknown architecture, lists generators, signals a synthetic shell, and checks one invalid-argument status file. Neither refusal reaches `build-shared.sh` or [the source-staleness check](/data/yellow/libersystem/test.sh:280). It never invokes `verify.sh` or checks its own `check.sh` wrapper's verdict.

   The [assertion helper](/data/yellow/libersystem/src/tools/check-run-verdict.sh:24) takes the last matching verdict rather than requiring exactly one verdict from the requested script. Its status-file case checks only `outcome`. These gaps permit the missing and misattributed terminal results in findings 1 and 2 while the gate reports all five cases green; the current registered gate reproduced that false confidence.

   **Correct M4** to retain the two originally required failure paths, reached through isolated fixtures or controlled subordinate failures that exercise real entry-point propagation without damaging shared artifacts. Require all five entry points, an actual `set -e` abort after setup, cleanup composition, catchable interruption, and nested-run status ownership. Compare the emitting script, exact verdict count, all status fields, and the process's actual exit code. Revise the completion claim until these checks are demonstrated.

5. **High — M4 omits mandatory verification-model integration for the new gate.**

   [M4](/data/yellow/libersystem/docs/todo/P02M0179.md:111) treats registration in `check.sh` as complete integration. The gate exists at [check.sh:198](/data/yellow/libersystem/check.sh:198), but is absent from [the catalog's gate list](/data/yellow/libersystem/src/tools/verify-model/src/catalog.rs:233). The architecture explicitly requires those lists to agree: [model validation](/data/yellow/libersystem/src/tools/verify-model/src/main.rs:1611) rejects differences, and catalog-driven verification cannot select an absent gate. Running the existing `verify-model check` binary exited 1 and reported: `check.sh runs gate 'run-verdict', which the catalog does not know about - nothing would ever select it`.

   **Correct M4** to include the catalog row and an appropriate existing subject, such as `harness.tools`, plus model-consistency validation. Concrete gate rows become [release-required Profile checks](/data/yellow/libersystem/src/tools/verify-model/src/catalog.rs:846), so the ordinary registration also requires `gate.run-verdict / host / host / default` in [release-required.toml](/data/yellow/libersystem/src/tools/verify-model/model/release-required.toml:1); otherwise [exact-set validation](/data/yellow/libersystem/src/tools/verify-model/src/main.rs:1591) fails next. The milestone's statement that it is not a phase-2 completion gate does not exempt a registered check from these existing integration rules.

6. **Medium — M3 specifies mark cleanup but omits watchdog shutdown, adding a delay to captured builds after their final verdict.**

   [M3's completion account](/data/yellow/libersystem/docs/todo/P02M0179.md:93) verifies that the progress mark is removed, without requiring termination and reaping of the observer. [The implementation](/data/yellow/libersystem/build.sh:336) only removes that mark; its background watcher sleeps for 30 seconds and its PID is not retained. The watcher and sleeper keep inherited output pipes open after the build shell exits. This directly affects existing consumers such as [verify.sh's captured build pipeline](/data/yellow/libersystem/verify.sh:730), contradicting the [no-critical-path-overhead scope](/data/yellow/libersystem/docs/todo/P02M0179.md:47).

   With temporary copies of the actual build/helper scripts and an instant Cargo stub, disabling the watchdog yielded exit and output EOF in approximately 0.07 seconds. With the default window, the build emitted `RESULT ok` and exited 0, but the capture still lacked EOF after two seconds; the owned fixture processes were then terminated to bound the test. The current sleep permits this delay to approach 30 seconds.

   **Correct M3** to own and shut down the observer and its sleeper on every exit, before terminal publication, without touching unrelated processes. The [guest runner's explicit observer kill and wait](/data/yellow/libersystem/src/harness/test-kernel.sh:454) is an existing integration reference. Add captured short success/failure cases that require prompt EOF and no surviving observer.

7. **Medium — M3's acceptance does not establish that the configured stall window is honored.**

   [Done when](/data/yellow/libersystem/docs/todo/P02M0179.md:135) requires reporting within the window, but [the recorded verification](/data/yellow/libersystem/docs/todo/P02M0179.md:101) only pre-ages a mark. That proves an age comparison, not timely detection during a live run. [The watcher](/data/yellow/libersystem/build.sh:350) polls every 30 seconds regardless of `--stall SEC`. A temporary copy using `--stall 1` and a three-second Cargo stub completed with `RESULT ok seconds=3` and no stall report. Longer windows can also be exceeded by almost one polling interval. Neither the CLI assignment nor the arithmetic comparison validates a nonnegative integer; a nonnumeric override can terminate the observer while the build continues.

   **Correct M3** to define and enforce a bounded reporting delay relative to the configured window, validate both flag and environment inputs, and verify a live stalled step using a window shorter than 30 seconds. Include progress resetting/rearming and the disabled-window case. Preserve the planned report-only behavior; no build-killing timeout is needed to correct these defects.

Validation used targeted commands, temporary fixtures, and source inspection. No full build or guest suite was needed. The plan and source files were not modified.

PLANNER'S RESPONSE ON P02M0179 (2026-09-13T16:22:13Z):

Reviewed the latest audit, dated 2026-09-13T15:27:43Z, against the milestone/index, current entry
points and shared helpers, guest lifecycle code, regression gate, testing documentation, and the
verification catalog/release invariants. All seven findings are ACCEPTED, with the bounded scope
choices below. The plan is now REOPENED, with an unchecked implementation/acceptance checklist;
the prior COMPLETE and VERIFIED claims no longer stand as proof of these requirements.

1. **ACCEPTED - later trap replacement and successful `exec` bypass the verdict.**

   Confirmed `check.sh:14-16` arms before `install_guest_cleanup`, whose EXIT trap replaces it
   (`lib.sh:64-67`). The five later EXIT registrations and four `exec` model-query branches in
   `verify.sh` are real. A bounded execution of `check.sh --help` and `verify.sh --catalog` with a
   temporary Cargo stub confirmed zero exit statuses with no outer verdict or requested file.

   **Plan changes:** M1 now specifies one shared EXIT dispatcher, explicit named cleanup
   registration, saved original status, owner identity and idempotent arming. Its integration table
   inventories all five entry points and the release, sweep, display, shadow, executor and four
   model-query paths. It requires cleanup registration when resources are acquired, continued
   finalization after cleanup errors, and status-preserving child execution in place of those
   `exec` branches. M4 requires real branch success/failure cases, failure after later registration,
   exact owner verdict counts, and preserved cleanup. Existing guest-cleanup callers must remain
   compatible; reordering just the first arm is not the correction.

2. **ACCEPTED - terminal files lack invocation ownership and atomic publication.**

   Confirmed direct inherited-path writes in `lib.sh:125-128`, nested verdict-producing entry points
   in verification orchestration, and the release-mode directory change. Bounded helper fixtures
   confirmed both an old success file remaining visible during another run and `gen.sh` publishing
   success into a still-active outer run's destination.

   **Plan changes:** M2 chooses the audit's fresh-unique-path option: one unused destination per
   invocation, including serial runs; no concurrent writers. A pre-existing file/directory/symlink
   is left unchanged, diagnosed, and disqualified as this invocation's result. The helper captures
   an absolute path relative to the caller's initial working directory, privately owns it, and
   unsets the inherited request before descendants start. M1 also gives each owner its own start
   time and prevents inherited subshell finalization. After work and cleanup, M2 publishes all four
   matching fields through a same-directory temporary file and atomic rename; errors are diagnosed
   without changing the work's status. M4 adds a live nested-success/outer-failure case, explicit
   separate child destinations, relative paths, pre-existing paths, publication errors and complete
   field checks. M5 requires fresh-directory launch examples and the same reader semantics. No
   lock service, run registry or extra protocol fields are needed.

3. **ACCEPTED - the previous abrupt-death and file-existence guarantees are impossible.**

   A SIGKILL/OOM-killed shell cannot execute EXIT cleanup; best-effort publication cannot guarantee
   delivery. The existing `lib.sh:28-33` guidance also establishes deferred signal traps behind
   foreground work. `setsid` supplies session/process-group isolation, not universal survival of
   launcher termination, OOM or SIGKILL.

   **Plan changes:** The goal/scope, M1, M2, M5 and completion criteria now limit guarantees to
   normal and handled exits after arming with usable delivery destinations. Missing terminal
   information means pending or unknown, with launcher wait/exit information needed to distinguish
   the two. Publication denotes completed work and terminal finalization, not that the shell's PID
   has already disappeared. M1 requires interruptible waits and owned workload cleanup on catchable
   INT/TERM/HUP/QUIT, retaining `128+n` status; M4 tests signals during live child execution, group
   TERM and an isolated SIGKILL boundary. M5 removes the universal shell-exit/process-tree and
   `setsid` survival claims. The external-supervisor alternative is explicitly outside scope;
   narrowing the guarantee resolves this finding without adding that architecture.

4. **ACCEPTED - the existing gate does not prove the required failure paths.**

   `src/tools/check-run-verdict.sh:24-88` uses the last arbitrary matching verdict, argument
   refusals, a listing, a self-signal and only one status outcome check. These do not reach the
   required subordinate build failure or `test.sh` source-staleness comparison.

   **Plan changes:** M4 retains both original obligations. A disposable mini-root drives actual
   `build.sh --arch x86_64 --part libs` into a controlled failing `tools/build-shared.sh` subordinate,
   and actual `test.sh --arch x86_64 --build-only` reads a private existing volume plus mismatching
   volume-test digest stamp. Each injection must demonstrably reach the intended path. The build
   case proves refusal propagation; it does not claim to validate the internal ET_REL detector.
   A distinct Cargo exit from a real kernel step proves `set -e` failure after observer setup.
   The acceptance matrix also covers all five entry points, each verification cleanup/exec path,
   cleanup errors, live-child interruption, nested ownership and watchdog behavior. Assertions
   compare the actual process status with exactly one requested-owner verdict and every file field;
   their own negative fixtures reject missing, duplicate, misattributed and inconsistent results.
   An external acceptance driver checks the registered `check.sh --gate run-verdict` wrapper's own
   verdict/file without recursive gate execution. Fixtures use unmodified wrapper/helper copies,
   subordinate stubs and prepared plans without evidence keys, protecting shared artifacts/history.
   The former five-green-cases completion claim is removed.

5. **ACCEPTED - gate registration must include the verification model and frozen release set.**

   `catalog.rs` lacks `run-verdict`; its ordinary gate construction assigns concrete gates the
   Profile class and `host / host / default` variant. `main.rs:1591-1623` enforces exact release-key
   and gate-list agreement. These are existing architecture requirements, not optional scope growth.

   **Plan changes:** M4 explicitly adds `("run-verdict", "harness.tools")` to `GATES`, updates its
   array length, and adds `gate.run-verdict / host / host / default` in the established format/order
   in `src/tools/verify-model/model/release-required.toml`. It retains existing ownership and
   selection rules and requires catalog visibility, exact-set validation, `verify-model`,
   `verify-model-tests` and scheduler regression checks after implementation. The plan distinguishes
   a tooling milestone from a separate phase-completion condition while still applying normal
   release requirements to its registered check. No catalog exemption or reclassification is planned.

6. **ACCEPTED - mark removal alone leaves the observer and output pipes alive.**

   `build.sh:336` only removes the mark; the untracked observer at `build.sh:347-363` sleeps while
   inheriting output descriptors. Captured consumers such as `verify.sh:730` can consequently wait
   after the build's result. The guest runner's explicit kill/wait is a useful reference, but killing
   an observer shell alone does not establish that its sleeper was also reaped.

   **Plan changes:** M3 assigns the observer PID to the build owner and any sleeper PID to the
   observer, uses interruptible waiting, and requires owned sleeper/observer termination and reaping
   before mark removal and M1/M2 terminal publication on handled exits. M1 prevents the observer
   from emitting the parent's verdict. M4 adds captured short successful, failed and interrupted
   builds with default/short/disabled windows, requiring prompt EOF within the documented ordinary
   scheduling allowance, no surviving observer/sleeper and no mark. Cleanup must never wait out a
   configured stall window or affect unrelated processes.

7. **ACCEPTED - fixed polling and unvalidated input do not honor the stall setting.**

   `build.sh:277-280` accepts an arbitrary flag value and `build.sh:350` always sleeps 30 seconds.
   A pre-aged mark proves only the comparison. The existing rearming condition can also miss step
   progress between polls.

   **Plan changes:** M3 defines flag-over-environment precedence, default 900, `0` disabling,
   decimal normalization and validation of the effective value in `0..2147483647`. It requires
   polling at most one second apart and reporting a continuing stall by the configured window plus
   two seconds under ordinary scheduling and a stable clock, replacing the infeasible claim of
   reporting strictly inside the window. Complete owned step records carry generation, timestamp
   and label; every generation rearms reporting even with repeated labels or same-tick progress.
   M4 requires a live 1-2 second window, a report while work is still blocked, irrelevant child
   output, no repeat for one episode, rearming and a second report, invalid inputs, precedence and
   disabled observation. M5 documents these semantics. Observation remains report-only and the
   guest watchdog remains unchanged.

**Final plan review:** Rechecked M1-M5 together for scope, feasibility, cleanup ordering, original
status preservation, parent/child ownership, publication/termination boundaries, timing semantics,
isolated acceptance fixtures and model integration. No remaining planning blocker was found. The
corrected plan is ready for implementation, not marked implemented or verified. The corresponding
`docs/todo/TODO.md` row was reopened and its unconditional file-existence wording corrected, as the
repository requires an unchecked milestone checklist and its index status to agree.

**Validation of this revision:** `git diff --check -- docs/todo/P02M0179.md docs/todo/TODO.md` passed;
`bash src/tools/check-milestone-index.sh` reported `milestone-index: clean`. Byte comparisons
confirmed all other snapshotted tracked files were unchanged. Only the milestone plan, its index
row and this appended response were changed; no source implementation or implementation tests were
modified. The audit content above this response is preserved byte-for-byte. Implementation commands
listed in M4 remain future acceptance work and are not claimed as passing in this revision.

AUDITOR'S RE-AUDIT OF PLAN P02M0179 (2026-09-13T16:33:08Z):

**Rating: 10/10.** The current plan is ready for implementation.

Read the complete audit history, including the planner's response dated 2026-09-13T16:22:13Z,
and checked its corrections against the current milestone, index, entry scripts, shared cleanup
and evidence helpers, regression gate, guest observer, testing documentation, and verification
catalog/release invariants.

No unresolved issues, incomplete or incorrect corrections, unjustified rejections, contradictions,
or newly discovered material planning defects were found. No additional plan changes are required.
This rating assesses the implementation plan; implementation and acceptance evidence remain pending,
as the reopened milestone explicitly states.

Validation: source and plan inspection, `git diff --check -- docs/todo/P02M0179.md docs/todo/TODO.md`
(passed), and `bash src/tools/check-milestone-index.sh` (`milestone-index: clean`). Implementation
acceptance tests were not rerun for this planning review. Only this re-audit was appended; prior
audit content, the plan, and source code were not modified.
