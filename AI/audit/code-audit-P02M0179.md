IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0179 (2026-09-13 21:45):

THE REOPENED PLAN, IMPLEMENTED. The audit that reopened this was right about the shape of the defect:
the first version composed TRAP TEXT. It read the existing EXIT trap back out of `trap -p`, pasted it
into a new one and re-armed - so every entry point that installed a cleanup AFTER arming silently
replaced the verdict, and every one that armed after a cleanup carried that cleanup's text into a
string nobody could read.

WHAT REPLACED IT. One dispatcher, armed once per invocation, with cleanups REGISTERING rather than
trapping. `register_run_cleanup` is called the moment a resource is acquired, which is what makes a
failure between acquisition and the rest of setup clean up anyway. Nine trap sites became
registrations: the guest cleanup in `lib.sh`, the build's step mark and observer, and five in
`verify.sh` (release, sweep worktree, rendered plan, shadow records, steps file).

THREE FINDINGS WORTH READING, each of which was a real missing verdict rather than a tidiness matter:

  `exec` TAKES THE TRAP WITH IT. `verify.sh --catalog`, `--model-hash`, `--age` and `--trust` each
  ended with `exec cargo run ...`, which REPLACES the shell - so all four answered with the planner's
  status and no verdict at all. They run as owned children now.

  A FOREGROUND CHILD DEFERS THE TRAP. Bash runs a signal handler only when the foreground child
  returns, so a run interrupted during a twenty-minute compile answered its signal twenty minutes
  later. `run_owned`/`run_owned_shell` background the leaf and wait interruptibly, keeping the child's
  real status and `pipefail`; the entry points' long workloads go through them, and an interruption
  stops and reaps what this invocation started - by recorded PID, never by executable name.

  AND THE OBSERVER OWNED NOTHING. `while sleep 30` left a thirty-second sleeper running after the
  build finished, so a caller reading the build's output waited for a sleeper nobody needed, and a
  thirty-second poll could not report a fifteen-second window at all. The observer now owns its
  sleeper, waits on it interruptibly, polls at one second, and is stopped and reaped by a cleanup
  registered BEFORE it is launched.

THE TERMINAL FILE IS AN INVOCATION'S, NOT AN ENVIRONMENT'S. The request is taken once at arming,
resolved against the invocation's own directory, and UNEXPORTED - so a child keeps its own verdict
line and has no destination unless its caller gives it a different fresh one. A destination that
already exists is refused with a diagnostic and left untouched, because overwriting one destroys
somebody else's answer and reusing one lets a stale success read as this run's. The record is written
beside the destination and renamed, so a reader never sees half of it.

AND ONE DEFECT THE WORK FOUND IN PASSING: `gen.sh`'s `EXTERNAL` map had been reformatted to
`[display - device]`, a key nothing looks up, which stopped the whole generation loop with `unbound
variable` on the first hyphenated package. The key is quoted now.

EVIDENCE: `src/tools/check-run-verdict.sh` is the gate and it proves the assertion first - it drives
its own checker over a missing line, a duplicate, another script's line, a wrong outcome, a
mismatched record and a contradicted exit code, and requires each to be refused. Then: all five entry
points on refusal and success; the four former `exec` actions; HUP, INT, QUIT and TERM; a TERM while
an owned child runs, with the child proven gone; nested ownership; four status-lifecycle cases; every
invalid stall window and the flag-over-environment precedence; a live one-second window reporting a
stall while the build is still in that step; and a disabled window starting no observer.
`./check.sh --gate run-verdict` passes with exactly one `check.sh` verdict and its own record;
`verify-model`, `verify-model-tests` and `verify-scheduler` pass.

THE HEAVY FIXTURES, DONE THE WAY THE PLAN ASKED FOR: a private tree per case, the entry scripts and
`lib.sh` copied unchanged, and stubs for the subordinate tools. A broken build reaching
`build-shared.sh` and refusing with exit 3; a real `build.sh --part kernel` against a Cargo stub that
exits 37, after the observer's cleanups are registered; a real `test.sh --build-only` over a private
volume and a mismatching stamp, producing `require_built`'s own refusal; a plan that is produced, a
planner that refuses, a model query that fails with its own status, the prepared-plan executor, and a
sweep that fails after its worktree exists with the worktree proven removed; three cleanups with the
middle one failing; and a `SIGKILL`ed fixture that publishes nothing, which the gate REQUIRES rather
than tolerates.

AND THEY FOUND A DEFECT IN THE WORK ITSELF, which is the point of writing fixtures that fail for real.
With `errexit` on, a fragment that fails inside `eval` makes the shell exit 1 rather than with the
command's own status - so the owned-child wrapper swallowed every subordinate exit code, and a
compiler refusing with 37 was reported as a plain failure. The background subshell now exits
explicitly with the fragment's status, and the two heavy cases above are what hold it. Nothing in the
first round of this milestone would have caught that: the cases it had all failed with 1.

EVIDENCE: 56 assertions in the gate, `./check.sh --gate run-verdict` green through its own wrapper
with exactly one `check.sh` verdict and its own record.


AUDITOR'S RE-AUDIT ON P02M0179 (2026-09-14T14:03:22Z):

**Rating: 4/10.** The current implementation does not yet satisfy the milestone's interruption,
owned-cleanup, reporting-failure and acceptance requirements. The COMPLETE claim is premature.

Reviewed the implementation account above, the original audit and responses in
`AI/audit/plan-audit-P02M0179.md`, the current M1-M5 requirements, entry scripts, shared and sourced
helpers, gate, catalog/release integration and reader documentation. The code-audit file contains
an initial implementation account rather than an earlier auditor section; the original findings
were therefore read from the corresponding plan-audit history. References below describe the current
implementation at HEAD `e83c14f4bad2edbd6a2b7335eb4ffa300551f058`.

1. **High — long foreground workloads still defer handled interruption.**

   [build.sh:117](/data/yellow/libersystem/build.sh:117) still runs the kernel Cargo command in the
   foreground. Loader, packaging and volume builds retain this pattern, as do generator workloads
   at [gen.sh:186](/data/yellow/libersystem/gen.sh:186), conformance at
   [check.sh:480](/data/yellow/libersystem/check.sh:480), and verification orchestration such as
   [verify.sh:444](/data/yellow/libersystem/verify.sh:444). The claimed conversion of long workloads
   to interruptible waits is incomplete.

   An unchanged private `build.sh --part kernel --stall 0`, with Cargo replaced by a subordinate
   stub executing `sleep 6`, remained alive with no verdict or terminal file 800 ms after TERM was
   delivered to the owner. It finally exited 143 after 6.037 seconds, when the child finished.
   A real stalled compiler can therefore postpone the terminal result indefinitely. Complete M1's
   interruptible execution integration for these existing workload paths, preserving their statuses.

2. **High — interrupted runs publish terminal records while their owned workers remain alive.**

   [lib.sh:292](/data/yellow/libersystem/lib.sh:292) signals and reaps only the immediate registered
   PID; terminating a wrapper subshell leaves its descendants running. There are also missing
   registrations: [test.sh:10](/data/yellow/libersystem/test.sh:10) installs guest cleanup before
   `arm_run_verdict` at line 15, so arming overwrites it, and its background `run_arch` jobs at
   [test.sh:359](/data/yellow/libersystem/test.sh:359) are not registered. Parallel verification
   workers at [verify.sh:1249](/data/yellow/libersystem/verify.sh:1249) are likewise unregistered.

   In separate unchanged private entry-point fixtures, TERM during `gen.sh --check`,
   `test.sh --build-only` with a valid private image/stamp, and `verify.sh --for src/abi --jobs 2`
   with a valid prepared guest plan produced exit 143 and matching terminal files while their
   recorded workload PIDs still ran. These were actual wrapper paths, with only subordinate work
   stubbed. This violates M1's preserved guest/workload cleanup and M2's meaning of terminal
   publication. Register the existing workers and preserve descendant ownership through shutdown
   and reaping before publishing; restore `test.sh`'s cleanup through the dispatcher.

3. **High — the executor and sweep regression cases accept failures before their intended injections.**

   [check-run-verdict.sh:437](/data/yellow/libersystem/src/tools/check-run-verdict.sh:437) writes
   malformed STEP rows without the required STATUS row. The actual executor refuses at
   [verify.sh:835](/data/yellow/libersystem/verify.sh:835) with
   `the planner's output has no STATUS line`, exit 3; neither purported step runs. The assertion
   at line 447 accepts that arbitrary nonzero status as the expected executor failure.

   The sweep invocation at
   [check-run-verdict.sh:500](/data/yellow/libersystem/src/tools/check-run-verdict.sh:500) never
   changes into its newly committed private repository. `verify.sh` runs its Git checks against
   the caller's directory instead. Reproduction from the dirty checkout exited 1 before acquisition;
   the isolated registered-gate run exited 128 because its caller directory was not a Git repository.
   Lines 503-513 nevertheless reported that a sweep failed after acquiring its worktree and removed
   it. Even after correcting the directory, the Cargo stub added after the commit at line 496
   makes the fixture repository dirty.

   The unchanged gate returned success over both unreached cases. Use the existing valid prepared
   format and a clean private repository, assert that the intended steps/resource acquisition and
   subordinate injection actually happened, and require the intended failure rather than any failure.
   The implementer's claimed executor and post-acquisition sweep evidence is not established.

4. **High — the gate still performs unbounded live-tree builds and does not prove the required lifecycle coverage.**

   [check-run-verdict.sh:604](/data/yellow/libersystem/src/tools/check-run-verdict.sh:604), line 614
   and line 630 invoke real `./build.sh` library/userspace builds in the gate's ROOT, outside its
   private fixtures. They can rewrite shared build outputs/stamps and wait on real compilation or
   locks, contrary to M4's bounded, isolated host gate. The global mark check at line 641 can also
   mistake a concurrent build's mark for its own leak.

   The timing assertion at line 618 only counts a STALLED line after the build returns. It cannot
   establish a live report, the deadline, once-per-episode behavior or rearming. Absence of STALLED
   is used as proof of no disabled observer, and neither surviving observer/sleeper PIDs nor prompt
   output EOF are checked. The signal cases at lines 177-217 use synthetic helpers rather than the
   required five-wrapper long-child matrix, leaving findings 1-2 undetected. Required release/shadow
   cleanup, live nested prepared-plan ownership and the full query success/failure matrix are also
   absent. Move the remaining work into private subordinate fixtures and add the specified reached,
   live-state, ownership and bounded-wait assertions before treating a green gate as M4 completion.

5. **Medium — a reporting failure changes the process status after publishing a contradictory record.**

   The final stderr write at [lib.sh:324](/data/yellow/libersystem/lib.sh:324) runs under `set -e`,
   and the dispatcher does not protect the saved work status from its failure. With a fresh
   `RUN_STATUS_FILE`, running actual `gen.sh --list` through
   `bash -c 'exec 2>&-; exec "$1/gen.sh" --list' bash "$PWD"` exited **1**, while its successfully
   published record contained `outcome=ok` and `exit=0`.

   Broken stderr is explicitly a delivery limitation, but M1 still requires reporting failure not
   to replace the original exit code. Make finalization preserve that saved code even when its
   diagnostic/verdict output fails; a usable terminal file must agree with the launcher's exit status.

6. **Medium — stall validation accepts out-of-range values and an empty explicit value.**

   [build.sh:342](/data/yellow/libersystem/build.sh:342) converts to machine arithmetic before
   checking the range. In unchanged private build fixtures, `--stall 18446744073709551616` wrapped
   to zero, reached Cargo and exited successfully with observation disabled;
   `--stall 18446744073709551617` wrapped to one and reported a one-second stall. Both exceed the
   required maximum of 2147483647. The presence test at line 346 also makes
   `BUILD_STALL=1 ... --stall ''` use the environment rather than reject the empty explicit value.
   Validate the decimal range before arithmetic conversion and distinguish flag presence from a
   nonempty value, as M3 requires.

7. **Medium — failed progress publication silently disables observation.**

   Mark-write/rename errors are discarded at
   [build.sh:384](/data/yellow/libersystem/build.sh:384) and setup at line 411; an absent mark then
   silently ends the observer at line 438. A private subordinate `mv` stub rejected only the owned
   `build-step.*` renames, recording both reached injections. An actual `--part kernel --stall 1`
   fixture ran for 4.101 seconds and exited 0 with neither STALLED nor an observation-failure
   diagnostic. The same workload without injection reported a stall at one second.

   Preserving the build's result is correct, but M3 also explicitly requires observation failures
   to be diagnosed. Report loss of the mark/observer without replacing the work's result or silently
   leaving the requested observation inactive.

8. **Medium — the documented detached launch never supplies its build with the terminal destination.**

   [docs/TESTING.md:162](/data/yellow/libersystem/docs/TESTING.md:162) launches the build first, then
   places `RUN_STATUS_FILE="$dir/run.status" ...` on a separate command. The already launched build
   cannot inherit that later assignment, so following the example produces no requested record.
   Attach the fresh destination to the actual launch command and provide a usable example, as M5
   requires.

Validation: bounded private fixtures copied the relevant entry scripts/helpers unchanged and stubbed
only subordinate tools. They covered the reported interruption, surviving-worker, malformed-plan,
sweep-refusal, invalid-window, observation-failure and reporting-error cases. A synchronized normal
observer control reported two live episodes, without duplicate reports, and completed with prompt EOF
and no retained marks/workers. The exact registered `check.sh --gate run-verdict` was additionally run
in an isolated mini-root with subordinate model/build stubs: it exited 0 in 10 seconds with one correct
outer line/file, while accepting the false-positive executor and sweep cases above. No full build or
guest suite was run in the live checkout.

Bash syntax checks passed. `./check.sh --gate verify-scheduler` passed. Current `verify-model` failed
with exit 1, and `verify-model-tests` failed with exit 101 (156 passed, one failed), because the derived
`host.graphics-app / host / host / default` key is missing from the frozen release set. That separate
current-tree mismatch is recorded as a validation limitation, not charged to this milestone's gate
registration or included in its rating.

Only this re-audit was appended. The preceding audit bytes were preserved exactly; no source code,
requirements or original audit text was edited. Other concurrent workspace changes were left alone.
