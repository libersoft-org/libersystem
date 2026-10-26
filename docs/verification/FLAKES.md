# Flakes

A flake is a test or a gate that failed and then passed with no change to the tree. Each one is written here when it
is seen: what it was, the date, the target, the load the machine was under, and its cause once it is known.

The rules this ledger carries:

- A TEST LISTED TWICE IS FIXED BEFORE ANYTHING ELSE IN IT. A second entry for one test is not a second sighting of
  bad luck; it is a defect with a reproduction rate.
- A FAILURE THAT REPEATS ALONE IS NOT A FLAKE. It is a defect of the code or of its bound, and it goes to the work
  that owns that code, not here.
- THE WIDTH GROWS ONLY BY THIS LEDGER. `verify.sh` runs the merge and release levels with four guest slots. The width
  is raised only after five L2 runs (`./verify.sh --release --in-place`) in a row in which no entry here is attributed
  to load; a gate that fails under load and passes alone runs alone (`ALONE_GATES` in the verify model's catalog)
  until its bound holds on an emulated target.
- Waiting in a kernel test goes through the one bounded wait (`wait_for` and `wait_until` in `src/kernel/tests.rs`):
  pump until the condition holds, at most a stated number of rounds. A test that pumps once and expects its answer is
  fixed to use it when it flakes.

## L2 runs since the last flake attributed to load

0 - the last were seen on 2026-10-20/21, below.

## Entries

| Test or gate | Seen | Target | Load | Cause | Status |
| --- | --- | --- | --- | --- | --- |
| The gamepad check of the service suite | 2026-10-20/21 | riscv64 | the release rehearsal, `--jobs 2` | It pumped the service once and read its answer; with the service on another core the answer had not been produced yet. It passed alone five times of five. | Fixed on 2026-10-20/21 with a bounded wait; its helpers now use the suites' one wait. |
| The display brightness tests | 2026-10-20/21 | not recorded | the release rehearsal | One pump, then the reply or the closed channel expected; a late completion took a second pass. | Fixed on 2026-10-20/21 with a bounded wait (`peer_closed`); it now uses the suites' one wait. |
| The kernel transport test | 2026-10-20/21 | not recorded | the release rehearsal | One pump, then the answer expected. | Fixed on 2026-10-20/21. |
| The transport test's patience | 2026-10-20/21 | not recorded | under load | A patience of three ticks, which load outran. | Open; seen once. |
| `bluetooth-transfer` | 2026-10-20/21 | x86_64 | under load | The Bluetooth fixture's OPP model timed out. | Open; seen once. The gate runs alone. |
| `typec-tcpci-riscv64` | 2026-10-20/21 | riscv64 | under load | A different timing step failed in each run: the TCPCI partner's steps are timed for a native target. | Open; seen in more than one run. The gate runs alone. |

Not a flake: the spool test on riscv64 (`spool_service_sends_submitted_jobs_exactly_once`) fails alone, two runs of
two, on the spool service's own two-second answer bound. It belongs to that service's bound, not to this ledger.
