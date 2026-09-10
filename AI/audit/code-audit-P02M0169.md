IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0169 (2026-09-09T13:47:05Z):

Scope: the six items of `docs/todo/P02M0169.md` - the exhaustive grant vocabulary (M1), rollback and
recovery ownership for every PermissionManager launch shape (M2), the pre-commit group seal (M3),
effect-observing launch fixtures (M4), the LSIDL `@rights` validation and generation corrections
(M5), and the scheduler documentation (M6). This record is appended to as the work progresses; the
verification section at the end says what was actually run.

## M6 - scheduler prose (done first, no code behaviour changed)

`src/kernel/sched/mod.rs`:
- The module header no longer calls the scheduler cooperative and non-migrating. It now states the
  implemented boundary: timer-preemptive per-core round robin with a one-tick quantum once `init()`
  arms `PREEMPTION_ENABLED`, explicit remote placement through `enqueue_on` plus a wake IPI, and
  wake-side migration onto the waker's core with no balancer and no affinity.
- The migration note in `enqueue` no longer claims that an address space live on two cores has no
  shootdown. It describes `mem::tlb::shootdown` as implemented - a whole-buffer flush on every other
  online core that WAITS for each acknowledgement before frames are returned - and names the open
  refinements (per-address-space active-CPU mask, per-page invalidation).
- The AP idle-loop note no longer says "in this cooperative model". It states the fact that survives:
  deadline expiry is checked only by the BSP's bounded drain, while ordinary wakes enqueue on the
  waking core.
- The local descriptions of the cooperative TEST drivers (`run_until_idle_until`, the bounded drain,
  `spawn_on_unwoken`) were left as they are - they describe test harnesses accurately.
No scheduling policy, balancing, affinity or TLB behaviour changed.

## M1 - the grant vocabulary is exhaustive against the schema

`src/user/services/core/src/permission_manager.rs`:
- `VOCABULARY` already carried all 23 schema capabilities (Session and DevicePolicy were added on
  2026-09-02 with the `grant-vocabulary` gate). The stale comment block that still said "`Session`
  IS DELIBERATELY ABSENT" and described the manager as holding no session client was replaced by an
  accurate one.
- A COMPILE-TIME exhaustiveness assertion was added as an anonymous `const _: () = { ... }` beside
  the array: `VOCABULARY.len() == core::mem::variant_count::<Capability>()` (a variant added to
  `security.lsidl` and not placed in the order fails the build), and a `const` loop refuses any
  ordinal walked twice. `#![feature(variant_count)]` is enabled in the binary's crate root; the
  pinned nightly (`nightly-2026-06-16`) still gates it, verified by compiling a snippet without the
  feature (E0658).
- Both Session and DevicePolicy are granted from `Clients::for_capability` through the ordinary
  duplicate path; a held client of 0 is a failed grant that now CANCELS the prepared launch (M2)
  instead of starting the component without the authority it requested.

## M2 - every launch has one rollback owner until commit, and one recovery owner after it

Design as the plan decides it: the transaction is a CONNECTION. `Transaction::open` mints a fresh
ProcessService connection with `service_connect(procsvc)` - the manager's existing `CAP_PROCESS`
client is a client of a `serve_multi` service, which answers the reserved CONNECT on any of its
channels - and the connection is closed on every path out.

`src/user/services/core/src/permission_manager.rs`:
- `struct Transaction { chan, client, stages }`, `struct Stage { started, manager_side }`, and the
  outcome enums `Fault { Refused, Uncertain }`, `Release { Started, Refused, StartFailed, Uncertain }`,
  `GroupRelease { Committed, Refused, Partial, Uncertain }`.
- `Transaction::fault` classifies the generated client's answer: `Some(Err(CommitUncertain))` (the
  generated mapping of PeerClosed / ReceiveFailed / TimedOut / NoMemory / Malformed) and `None` (a
  reply the client could not decode) are UNCERTAIN; every other typed error is a refusal;
  `send_refused()` reads `last_error()` to tell a request that never left (`SendRefused`/`NoRoute`)
  from one the service answered, which decides whether the transferred bootstrap end is still ours
  to close and whether a release error is a pre-start refusal or a post-removal start failure.
- `Transaction::abandon(fault)`: on `Refused` every stage is `cancel`led BY NAME on the connection
  (synchronous, so the record and its Domain are gone when it returns), then every manager side
  and task handle is closed and the connection is closed; on `Uncertain` NOTHING more is asked on
  the connection - handles closed, connection dropped, ProcessService's disconnect cleanup abandons
  what was prepared, and a late reply lands on a dead endpoint.
- Recovery after an uncertain release: `recover_started(task)` sends `SIG_KILL` through the
  retained task handle and waits (bounded by `RECOVERY_TICKS`) for termination; pipelines use
  `recover_group(group)` through the group handle; `reap_through(procsvc)` then asks the ordinary
  client for a `list`, which is ProcessService's reap. The result is reported as
  `Error::CommitUncertain`, never as a refusal.
- All four shapes were rewritten on the helper: `launch_under_manifest`, `run_tool_under_manifest`
  (now `Result<StartResult, Error>`), `run_pipeline_under_manifest` (now
  `Result<PipelineResult, Error>`, owning the stdio endpoints - see below) and
  `run_tool_over_file` (now `Result`). Every early return after a prepare goes through
  `transaction.abandon(..)`; no path closes `started.task` without cancelling any more.
- The bounded `imgconv` launch goes through the new `launch-prepared-bounded` op and is released
  only after the same grant transaction as every other tool.
- NO DEADLINE is set on transaction calls. Decision: a prepare loads a program off the volume,
  which on an emulated target takes as long as it takes, and a deadline firing on a slow but healthy
  load would turn a launch into a kill; ProcessService's death is `PeerClosed`, which is handled,
  and `TimedOut` is classified in the same match arm so a deadline can be added without changing
  the recovery.
- The pipeline's stdio endpoints (edges, terminal, error duplicates) are now closed by the
  transaction for exactly the stages that were not installed; `Service::run_pipeline` no longer
  closes every edge on failure (that closed numbers already consumed by a transfer, after grants in
  between had minted new handles under them).

`src/user/services/core/src/process_service.rs`:
- `release` answers three ways: `Ok(true)` started, `Ok(false)` pre-start refusal, `Err(Invalid)`
  post-removal start failure - and on the third it `forget`s the record and its Domain itself.
- `release_group` forgets every member whose start failed and still reports `Err(Invalid)`.
- `launch_prepared_bounded` (op 10): `launch-bounded`'s Domain behind `launch-prepared`'s gate.
- `hold_prepared` is the shared holder for both prepared variants. It clears `Spawned.process`
  after the task handle is transferred in the reply, because the transfer CONSUMES the number from
  the service's table and `Spawned::abandon` used to close it again on cancel - a stale number that
  a later launch could have reused (latent defect fixed in passing; `abandon` now closes `process`
  only when nonzero).

`src/idl/process.lsidl`: op 10 `launch-prepared-bounded` and the doc paragraphs stating the three
release answers and the connection-is-the-transaction rule. Bindings regenerated with `./gen.sh`.

## M3 - a pipeline is sealed while cancellation is still possible

`run_pipeline_under_manifest` creates the process group with `process_group_create` over the
PREPARED members' task handles (each carries MANAGE; `sys_process_group_create` asks nothing of a
member a prepared process cannot satisfy) BEFORE `release_group`. A creation failure cancels every
prepared member and returns `NotFound`; a refused group release closes the group and cancels;
`Partial`/`Uncertain` kill through the group, wait, reap and answer `CommitUncertain`.

`src/kernel/syscall/mod.rs`: `#[cfg(test)] pub static FAIL_NEXT_GROUP_CREATE` makes the next
`sys_process_group_create` answer `ERR_NO_MEMORY` once - the only way the group-creation rollback can
be driven, since nothing a ring-3 caller can arrange makes that syscall fail.

## M5 - LSIDL refuses an unenforceable `@rights` promise

`src/tools/lsidl-gen/src/ast.rs`, `parser.rs`: `Param` carries `rights_declared` and
`rights_non_names` beside the resolved names, so `@rights()` and `@rights(1)` are distinguishable
from no annotation.
`validate.rs`: `check_rights` refuses, with its own message each: a non-name argument, an empty list,
a repeated right, an unknown right (kept), a parameter not WRITTEN as `handle<resource>` (option,
list, result, record), a named alias (local or imported, "even where it resolves to a handle"), a
method whose result cannot answer `denied` (local enums by their cases, imported ones by the
resolver's `contains_denied`), and a stream-returning method.
`codegen.rs`: on the denial path every handle the request carried is released through
`crate::codec::release_handle` and only THEN is `request_handles.clear()` emitted (it used to be
emitted before the guards). `src/wire/src/lib.rs` declares `liber_handle_release` and
`release_handle`; `rt` implements it as `close`; the display-proto host stub records releases.
Tests: `lsidl-gen` gained fixtures for every rejected shape (nested x4, local alias, imported alias
after resolution, missing-denied x3, stream x2, `@rights()`, `@rights(1)`, repeated) and a positive
generated-source test (`handle_carries(t, 1024, 1)`, the release, `Denied`, clear after release);
display-proto gained `a_refusal_releases_the_handle_it_refused_and_an_acceptance_does_not` (eight
repeated refusals release exactly one handle each). `docs/LSIDL.md`'s annotation table and the
`@rights` paragraph now state the enforcement and the accepted shape; `security.lsidl`'s comment on
`run-interactive` says the validator refuses the option shape.

## M4 - the launch regression observes effects, not comments

`src/kernel/tests.rs` (base permission scenario, cached fixture):
- the manager is handed live `CONFIG`/`DEVICE` stand-ins (server ends the test keeps; they were
  dead-peer clients), a `DEVPOLICY` client and a `SESSION` client;
- `kill --kill 1` through `run`: the test answers the session's `job-signal` (op 12) and reads
  what `kill` printed;
- `lsdev --incident 0` through `run`: the test answers the manager's CONNECT mints for the device
  and config grants and the config `seal`, then `lsdev`'s incident request on the POLICY grant
  (dropped without answer, so the fallback runs), its device `get` (`not-found`) and its config
  `list` on the connection received under CONFIG - which is the read that desynchronises when the
  policy grant is skipped;
- the cross-owner negative against the real ProcessService: two CONNECT-minted connections, a
  prepare on each, `release-group` naming both from one connection - refused - then each owner's
  cancel and the process list.
- `PERMISSION_COHORT` grew to 15 entries for the three new consumers.
`src/kernel/tests.rs` (fault scenario, `run_permission_fault_scenario`): the test PLAYS
ProcessService (`ProcessStandIn`) behind the manager's `PROCESS` capability, refuses the manager's
own start-up launches, then drives 15 scripted cases (mint refused; prepare refused / reply lost /
reply garbled then a LATE reply; grant failure after prepare; bounded prepare then refused release;
selected-file mint failure; release refused / start failed / reply lost; group release refused /
partial / reply lost; group creation failure through the kernel hook; a committed pipeline) and
records the manager's reply, every request it made per connection, whether each stand-in Process
was killed, and the domain handle count before and after. It also drives eight `run` requests whose
stdout capability lacks `send`, refused by the generated guard, with the handle baseline per attempt.
`src/kernel/test_suites/applications.rs`: four new tests assert the above.

## Verification (so far)

- `cargo build --target x86_64-unknown-none` in `src/user/services/core`: PASSED (after the
  `variant_count` assertion was made anonymous).
- `cargo test` in `src/tools/lsidl-gen`: PASSED, 55 tests.
- `cargo test --manifest-path user/libs/protocol/display-proto/Cargo.toml` (from `src/`): PASSED, 9.
- `bash src/tools/check-milestone-index.sh`: PASSED (self-test incl. the new `[i]` cases).
- `./gen.sh`: regenerated; the ABI manifests changed only by the added op.
- The kernel test suite for the new fixtures: NOT YET RUN (x86_64 build in progress).

## The fault cohort's first runs (2026-09-09T16:00:00Z onward): what the fixture found

The cohort test `kernel.applications.a_launch_transaction_rolls_back_on_every_fault` was run five
times on x86_64 before it reached its handle-accounting assertion. Each run stopped at a different
truth, and they are recorded here in order because three of them were defects in the STAND-IN and
one was a defect in the production manager the fixture exists to find:

1. `release refused` saw ops `[6, 9]` instead of `[6, 7, 9]`: every case that should have reached a
   release cancelled first. Cause A: the stand-in dropped the prepare request's message - and with it
   the CHILD'S END of the bootstrap channel the real ProcessService keeps in the prepared record - so
   the manager's next send on `manager_side` failed with a closed peer. Fix: `ProcessStandIn.held`
   keeps every prepare message for the case. Cause B, found once A was fixed: the stand-in echoed the
   request's bare name (`date`) in the start-result, and `executable::logical_name` strips the
   executable suffix the real service reports (`date.lsexe`) - no suffix, no policy name, abandon.
   Fix: the stand-in answers with the artifact name.
   The comment that said `date`'s grant was minted with CONNECT on the time client was wrong -
   `Capability::Time` is a narrowed duplicate - and is corrected; the stand-in no longer pretends to
   answer a mint nobody sends.
2. `bounded prepare, then a grant fails` then answered a SUCCESS: `grant_volumes` grants a volume the
   manager holds no client for as ZERO by design, so the `imgconv` launch this case relied on
   committed. The case now injects a real fault - `FaultCase.drop_bootstrap` makes the stand-in drop
   the prepared launch's bootstrap end, so the first grant cannot be delivered - and expects
   `[10, 9]`, which is what the bounded prepared path owes.
3. With that fault injected, the handle count assertion failed: `left: 10, right: 9` - ONE HANDLE
   MORE in the manager's Domain after the case. That is a real leak in PermissionManager:
   `send_blocking` leaves a handle in the sender's table when the send fails, and every hand-over on
   `manager_side` read the result without closing the handle - the caller's stdout console, the
   control channel, every per-capability grant, the selected-file grant and the volume bundle. Fix:
   `hand_over(manager_side, tag, handle)` closes the handle when the send fails, and every site
   that used to call `send_blocking` with a capability goes through it (the pipeline's stage
   endpoints keep `close_from`, which already owns their cleanup). The placeholder sends (handle 0)
   are unchanged.

Verification of these is in the section below, once the rebuilt userspace has been run.

## Verification (2026-09-09T16:05:00Z, after the rebuilt userspace)

Passed, with the command that produced the result:
- `TEST_SELECTION=kernel.applications.a_launch_transaction_rolls_back_on_every_fault ./test.sh --arch x86_64`:
  PASSED, 1 test in 16 s (`.build/logs/test/x86_64-20260909T160157Z-*`), after the three stand-in
  corrections and the `hand_over` leak fix above. All fifteen scripted cases and the eight
  rights-refusal attempts ran; every case's handle count came back equal.
- `./test.sh --arch x86_64 --tags permission-service,process-service`: PASSED, 32 tests in 86 s.
- `./test.sh --arch x86_64 --tags boot`: PASSED, 15 tests (the whole-system boot chain with the
  changed PermissionManager).
- `cargo test` in `src/tools/lsidl-gen` (55) and `user/libs/protocol/display-proto` (9): PASSED
  earlier in this record; unchanged since.
- `bash src/tools/check-milestone-index.sh`: PASSED (the `[i]` state).

Not yet run at the time of this entry (recorded here so they are not claimed):
- `./check.sh --gate grant-vocabulary` and `--gate verify-model-tests` - the latter cannot pass
  until the aarch64 and riscv64 test kernels are rebuilt, because the model's built-suite inventory
  reads every architecture's test binary and those two predate the P02M0172 test changes. Both
  are run at the end of the batch with the slow builds.

## Verification, continued (2026-09-09T19:50:00Z)

- `./check.sh --gate grant-vocabulary`: PASSED.
- `./check.sh --gate verify-model-tests`: PASSED (148 tests), once the aarch64 and riscv64 test
  kernels had been rebuilt (`harness/test-kernel.sh <arch> --build-only`) so the model's built-suite
  inventory matched the source; the reach analysis also learned that `permission_run_request` and
  `permission_run_with_file_request` launch a tool by name, which is how the two governed-launch
  fixtures (`bin.kill`, `bin.lsdev`) declare what they reach.
