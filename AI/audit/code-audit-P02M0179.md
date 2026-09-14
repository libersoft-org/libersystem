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
