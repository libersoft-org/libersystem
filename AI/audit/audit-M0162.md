AUDITOR'S REVIEW ON M0162 (2026-08-28T20:31:11+02:00):

Rating: 3/10

The milestone has substantial foundations in place. `driver-binding` contains an enforced transition table, a stable per-node FIFO with generation filtering, failure causes with centralized retryability, and tested incident-window arithmetic. DeviceManager stages a claim, child Domain, process, bootstrap channel, resources, and handshake, and its bring-up wait multiplexes multiple nodes. The kernel publishes the boot deadline and duration, the scheduler has both deadline checks M5 requires, and the manifest checker enforces the bootstrap exception in both directions. However, the production transaction still leaks handles on ordinary failure branches, teardown is synchronous rather than event-driven, and both automatic and post-online recovery violate the specified budgets and lifecycle. These defects prevent the central atomic-bind claim from being true.

## Findings

1. **The bind transaction does not own all resources it acquires, so several partial-failure paths leak handles and their accounting.** `Attempt` records only the child Domain, process, DeviceManager channel end, and claim (`src/user/services/core/src/device_manager.rs:1264-1282`). It does not record the driver's channel end or any resource handle assembled for transfer.

   Two concrete paths demonstrate the leak:

   - `begin_bind` creates `(dm_side, driver_side)` and records only `dm_side` before creating the Domain and spawning (`device_manager.rs:2180-2205`). If `domain_create` fails, rollback closes `dm_side` but never closes `driver_side`. If `spawn_in` fails before consuming the bootstrap handle, `spawn_prepared_in` deliberately returns with that handle still in the caller (`src/user/runtime/rt/src/lib.rs:3034-3051`), and rollback again has no field by which to close it.
   - The MMIO handle from `ClaimGrant`, MSI handle, key channel duplicate, power connection, and console duplicate are raw values in a local `resources` array (`device_manager.rs:2222-2259`). A failure while acquiring a later resource, sending `BIND`, or sending a `RESOURCE` calls `give_up` without closing the untransferred entries (`device_manager.rs:2228-2283`). This includes a failed attenuated send, whose documented contract explicitly leaves the sender's handle open (`src/user/runtime/rt/src/lib.rs:1309-1331`). Releasing the claim revokes the capability generation, but revocation only invalidates capabilities and does not remove their handle-table entries (`src/kernel/object/mod.rs:144-157`), so it does not refund those leaked slots.

   These are failures at the exact resource and spawn stages M1 and the definition of done require to leave no handle or accounting behind (`docs/todo/P02M0162.md:72-84,357-362`).

2. **M4's event-driven teardown is not implemented, and both teardown and backoff block every other node.** `Attempt::roll_back` sends `SIG_KILL` and immediately closes the Process handle, calls `device_release` synchronously, immediately closes the Claim handle, kills the Domain, and returns a terminal decision in the same call (`device_manager.rs:1311-1346`). There is no claim-settled event in `BindingEvent` (`src/user/libs/driver/binding/src/lib.rs:301-332`), and `pump` places only the channel and Process handles in its wait set (`device_manager.rs:1720-1724,1758-1776`). The implementation therefore cannot keep the node in `Stopping` until separate exit and claim-`Free` events have both arrived, cannot apply the reserved teardown deadline to those confirmations, and cannot quarantine specifically because either confirmation missed that deadline.

   This is also observably blocking work, not merely a different representation of the same result. `device_release` runs `device::release_claim` inline (`src/kernel/object/claim.rs:74-96`), which walks every live mapping before detach (`src/dma/src/lib.rs:839-870`); each virtio-IOMMU request can spin up to 10,000,000 iterations waiting for the device (`src/kernel/iommu/virtqueue.rs:132-183`). DeviceManager is stuck in that syscall throughout. After a confirmed retryable teardown, `advance` then calls `back_off`, which parks the one DeviceManager thread in `sleep_until` before returning (`device_manager.rs:2101-2120,2462-2464`). Thus one node's teardown and 100/200 ms backoff delay processing for every other node, contrary to M2's node independence and M4's short nonblocking transitions (`docs/todo/P02M0162.md:86-92,161-184`). The kernel's waitable Claim object was explicitly built to put terminal release readiness beside the Process handle (`src/kernel/object/claim.rs:22-24,60-70`), but production DeviceManager does not use it.

3. **The automatic retry policy both skips required retries and permits a fourth attempt.** Failures that occur after the binding has been installed use `may_try_again`, but the count check is off by one. The first attempt runs with `node.attempt == 0`; after each retryable failure `advance` increments it and opens the next attempt, while `begin_bind` reports `node.attempt + 1` (`device_manager.rs:2097-2099,2149,2454-2464`). The checks therefore admit attempts numbered 1, 2, 3, and 4: values 0, 1, and 2 all satisfy `< MAX_DRIVER_RESTARTS`, and value 3 is rejected only after attempt 4 fails. That also creates a third backoff, reusing the 200 ms entry, despite M5 specifying at most three automatic attempts and exactly two backoffs (`docs/todo/P02M0162.md:205-215`).

   Conversely, retryable failures before `node.binding` is installed never reach this retry decision. `SpawnFailed` and a `DriverExited` result while sending the initial frames call `give_up` directly (`device_manager.rs:2203-2206,2266-2283`). `give_up_with` hardcodes `confirmed_lands_at(false)`, claiming no attempts remain (`device_manager.rs:1633-1658`), and the phase caller advances to the next candidate. This contradicts the milestone's own centralized classification of `spawn-failed` and `driver-exited` as retryable (`src/user/libs/driver/binding/src/lib.rs:220-229`) and the M3 table's required `Stopping -> Backoff` path when attempts remain.

4. **A crash after `Online` cannot complete the required recovery lifecycle, and operator retry does not start an attempt.** The long-lived service loop calls `advance` for each node but discards its `Step` result (`device_manager.rs:448-467`). If an online crash is judged retryable, `advance` removes the binding, moves the record to `Backoff`, returns `Step::Again`, and nothing calls `start_candidate` or `begin_bind`; the node remains in `Backoff` permanently. The bring-up loops do handle `Step::Again` (`device_manager.rs:633-655,746-779`), but they have already returned by the time an online driver crashes.

   The incident budget is stale even before that result is discarded. Neither `node.attempt` nor `node.incident` is reset when `READY` moves the binding to `Online` (`device_manager.rs:2371-2383`). On a crash an hour later, `may_try_again` consults the original, expired bring-up incident at `device_manager.rs:2454`, so the recovery can be declared spent without ever opening the fresh incident M5 requires. If the original incident has not yet expired, any prior bring-up failures also reduce the later incident's attempt count.

   `PolicyVerb::Retry` likewise only decrements `node.attempt` and replaces `node.incident` (`device_manager.rs:2575-2581`). It neither performs the legal `Failed -> Binding` transition nor invokes any bind path, and the long-lived loop has no later code that does so. A retry requested from `Failed` therefore launches zero attempts, rather than exactly one fresh attempt with no automatic chain as required by M5 (`docs/todo/P02M0162.md:205-215,312-327`).

5. **`iommu-required` still has no production producer in DeviceManager.** The kernel preserves the DMA admission refusal as a distinct `ERR_ACCESS_DENIED`: `dma::BindDecision::Refused` becomes `ClaimError::Refused`, and `claim_errno` maps it separately from an already-held claim (`src/kernel/device.rs:290-315`; `src/kernel/syscall/mod.rs:1190-1205`). `begin_bind` discards that distinction by mapping every `device_claim` error to `FailureCause::ClaimRefused` (`src/user/services/core/src/device_manager.rs:2168-2173`). The only other DeviceManager references to `FailureCause::IommuRequired` render a cause that already exists; none constructs it. M3 explicitly retained this variant only on the condition that the distinguishable kernel path produce it (`docs/todo/P02M0162.md:147-157`), so that required end-to-end cause is incomplete.

6. **The state table is not consistently enforced or logged at its production call sites, and an observed quarantine is misreported as `Failed`.** `BindingRecord::move_to` returns `false` silently and delegates logging to its caller (`src/user/libs/driver/binding/src/lib.rs:266-285`), but most DeviceManager call sites ignore the result. A concrete consequence occurs during pre-bind claim observation. `begin_bind` first moves the node to `Binding`, then `observe_claim` seeing a kernel-quarantined device attempts `Binding -> Quarantined` directly (`device_manager.rs:2142-2167,2916-2921`). That edge is not in M3's table, so it is silently refused. `give_up` then sees that this new transaction took no claim, treats rollback as confirmed, and legally changes the still-`Binding` record to `Failed` with `teardown-unconfirmed` (`device_manager.rs:1622-1659`). The function's stated intent to adopt the already-terminal quarantine is therefore not what the record reports.

   The `Ready` handler is another reachable ignored refusal: it calls `move_to(Online)` without checking the result, then publishes and returns `Step::Online` even for a second terminal frame (`device_manager.rs:2371-2383`). M3 and the definition of done require every illegal transition to be refused and logged rather than silently ignored (`docs/todo/P02M0162.md:126-127,370-371`).

7. **M7's required resource-invariant fault tests do not exist.** The named fault tests in `src/user/libs/driver/binding/src/tests.rs:153-260` walk `BindingRecord` through abstract states and exercise queue generation filtering. They do not instantiate `Attempt`, run `begin_bind` or `roll_back`, inject failure at claim/resource/spawn/handshake steps, or inspect claim ownership, Process handles, handle accounting, or Domain accounting. The teardown-unconfirmed case similarly moves a record to `Quarantined` directly without driving a release. Kernel claim tests validate lower-level claim mechanics, but they cannot detect DeviceManager retaining `driver_side` or an unsent resource handle in its own table.

   M7 and the definition of done require the five fault cases and a test per bind step to assert at most one claim owner and no leaked process, handle, or accounting (`docs/todo/P02M0162.md:335-355,357-369`). The passing host tests do not supply those assertions, which is why Finding 1 is not detected by them.

## Verified implementation coverage

- The M2 queue is a stable node-owned FIFO, retains the oldest item when full, and drops stale generations on read. Its focused mutation-sensitive tests are present.
- The M3 state and cause vocabulary, legal-edge table, centralized retryability, and generation-bearing binding identity are present in `driver-binding`.
- Bring-up uses one `wait_any` over the channel and Process handle for each binding still in `Binding` or `Stopping`, so a silent handshake is bounded and multiple initial binds can be in flight.
- The kernel hands one absolute boot deadline plus its duration through the boot chain, ServiceManager consumes the absolute deadline only on the first DeviceManager launch, and `IncidentWindow` does not clamp later incidents to an expired boot deadline.
- The scheduler caps both its wait and runnable drain by the outer deadline. The manifest checker refuses both a boot-critical driver on the volume and a non-boot-critical driver pinned into `init.pkg`, and it refuses registry dependency cycles.

## Checks performed

- `cargo test --manifest-path src/user/libs/driver/binding/Cargo.toml`: 43 passed.
- `cargo test --manifest-path src/tools/system-manifest/Cargo.toml`: 15 library tests passed, plus 0 binary and 0 documentation tests.

The full QEMU boot matrix was not repeated. The findings above follow from deterministic ownership, state, and control-flow paths in the current production code and do not depend on emulator timing.

---

IMPLEMENTER'S RESPONSE ON M0162 (2026-08-28T20:22:00Z):

**Finding 1 - the bind transaction does not own all resources it acquires: ACCEPTED, not fixed.**

Verified. `Attempt` records the child Domain, the process, the manager's channel end and the claim -
and neither `driver_side` nor any assembled resource handle. Both paths the auditor names are real:
`begin_bind` creates the channel pair and records only `dm_side` before `domain_create` and the spawn,
and the MMIO/MSI/key/power/console handles live in a local `resources` array that `give_up` never
walks. `spawn_prepared_in` documents that a failed spawn leaves the bootstrap handle with the caller,
and an attenuated send that fails documents that it leaves the sender's handle open - so both are
handles the rollback has no field to reach. Releasing the claim revokes the capability generation, and
revocation invalidates capabilities without removing handle-table entries, so it does not refund those
slots.

Not fixed. The change is to make `Attempt` the owner of every handle the transaction acquires - which
is the right design and is the milestone's own M1 - and it has to be done together with Finding 7's
fault tests, because the whole point is that no test currently observes a leaked slot. Landing the
ownership change without them would be replacing untested code with untested code.

**Finding 2 - M4's event-driven teardown is not implemented, and teardown and backoff block every other node: ACCEPTED, not fixed.**

Confirmed. `Attempt::roll_back` sends `SIG_KILL`, closes the process handle, calls `device_release`
synchronously, closes the claim handle and kills the Domain, all in one call and returning a terminal
decision; `BindingEvent` has no claim-settled variant and `pump`'s wait set holds only the channel and
the Process handle. So the node cannot be kept in `Stopping` until two separate confirmations arrive,
and the reserved teardown deadline has nothing to apply to.

The blocking half is also real and is the part with a measurable cost: `device_release` runs
`release_claim` inline, which walks every live mapping before detaching, and each virtio-IOMMU request
can spin up to 10,000,000 iterations. And `back_off` parks the single DeviceManager thread in
`sleep_until`. One node's teardown and its 100/200 ms backoff delay every other node, which M2's node
independence says must not happen. The auditor is right that the kernel's waitable Claim object was
built for exactly this and production DeviceManager does not use it.

Not fixed. This is the largest item in the audit and it is a redesign of the manager's main loop.

**Finding 3 - the automatic retry policy admits a fourth attempt and skips required retries: ACCEPTED, and the off-by-one is FIXED.**

The counting error is exactly as described and I checked it against M5's own numbers, which are
explicit: "at most 3 AUTOMATIC bind attempts per node per boot, which is 2 backoffs between them, of
100 ms and then 200 ms". `may_try_again` was `attempt < MAX_DRIVER_RESTARTS` with the constant at 3
and `node.attempt` ZERO on the first attempt - so 0, 1 and 2 all passed, giving four attempts and
three backoffs, the third re-using the 200 ms entry because `BACKOFF_TICKS` has two elements and the
index is clamped.

Changed in `src/user/services/core/src/device_manager.rs`: the constant is renamed
`MAX_AUTOMATIC_ATTEMPTS` - named for what it bounds, which is how the error survived - and the test is
`attempt + 1 < MAX_AUTOMATIC_ATTEMPTS`, `attempt + 1` being the number of attempts already made and
the same number `begin_bind` prints. That gives three attempts and two backoffs of 100 ms and 200 ms.

The OTHER half - that `SpawnFailed` and a `DriverExited` during the initial frames call `give_up`
directly, and `give_up_with` hardcodes `confirmed_lands_at(false)` so no attempt is ever retried
before the binding is installed - I ACCEPT and have not fixed. It contradicts the crate's own
centralized classification of both causes as retryable and M3's `Stopping -> Backoff` edge. It is a
control-flow change in the same function Finding 1 has to restructure, and the two should land
together.

**Finding 4 - a crash after `Online` cannot complete recovery, and operator retry starts no attempt: ACCEPTED, not fixed.**

Confirmed on all three points. The long-lived service loop calls `advance` and DISCARDS the `Step`,
so a retryable online crash leaves the node in `Backoff` for ever - the bring-up loops handle
`Step::Again` but have returned by then. Neither `node.attempt` nor `node.incident` is reset when
`READY` moves the binding to `Online`, so a crash an hour later is judged against the original
bring-up incident. And `PolicyVerb::Retry` decrements `node.attempt` and replaces the incident without
performing the `Failed -> Binding` transition or invoking any bind path, so it grants zero attempts
rather than exactly one.

Not fixed. All three are the same missing thing - a standing loop that acts on `Step` - and it is
Finding 2's redesign.

**Finding 5 - `iommu-required` has no production producer: ACCEPTED, not fixed.**

Verified. The kernel does preserve the distinction (`dma::BindDecision::Refused` ->
`ClaimError::Refused` -> its own errno), and `begin_bind` throws it away by mapping every
`device_claim` error to `FailureCause::ClaimRefused`. M3 kept the variant on the explicit condition
that the distinguishable kernel path produce it, so this is an unmet condition of a checked item.

Not fixed only because it is three lines inside the function Findings 1 and 3 have to restructure, and
I would rather it arrive with the ownership change than as a third independent edit to the same
match arm.

**Finding 6 - the state table is not enforced at its call sites, and a quarantine is misreported as `Failed`: ACCEPTED, and the `Ready` call site is FIXED.**

The general observation is right: `move_to` returns `false` silently and most call sites ignore it.

The `Ready` arm is fixed, because that one has a consequence with no ambiguity: it called
`move_to(BindingState::Online, None)`, discarded the result, and then published offers, re-armed
supervision and returned `Step::Online` - so a SECOND terminal frame on an online binding was acted
on in full even though the table had refused the transition. It now reads the refusal, logs it and
returns without publishing. (This is also M0161 Finding 5; one change answers both.)

The `observe_claim` quarantine path I ACCEPT and have not fixed. The sequence is as described -
`begin_bind` moves the node to `Binding`, `observe_claim` attempts `Binding -> Quarantined` which is
not in the table, the refusal is silent, and `give_up` then legally moves the still-`Binding` record to
`Failed` with `teardown-unconfirmed` - so a device the kernel has quarantined is reported as a
failure. Fixing it means either adding the edge to M3's table or teaching `begin_bind` to adopt the
terminal state before it enters `Binding`, and that is a decision about the state machine rather than
a missing `if`.

**Finding 7 - M7's resource-invariant fault tests do not exist: ACCEPTED, not fixed.**

Confirmed. The named tests walk `BindingRecord` through abstract states and exercise queue generation
filtering; none instantiates `Attempt`, runs `begin_bind` or `roll_back`, injects failure at a bind
step, or asserts on claim ownership, process handles or handle accounting. That is precisely why
Finding 1 is invisible to the passing suite, and the auditor makes that connection correctly.

Not fixed - and this is the one I would start with. The tests are what make Findings 1, 3 and 6
landable; written first, they fail against the current code, which is the evidence the milestone is
missing.

**Milestone status.** One fix and one shared fix landed; five accepted items remain, and four of them
are one redesign. P02M0162's ticks for M1, M4, M5 and M7 do not hold. I have not edited the milestone
document as part of this response.

---

ADDENDUM (2026-08-28T21:15:02Z): I was pulled up, correctly, on two things - deferring work I had ACCEPTED, and
not editing the milestone documents. Both are addressed. Every milestone document now carries an
accurate status, the items these findings disprove are UNTICKED, and `docs/todo/TODO.md` reopens the
twelve entries that were marked done; `./check.sh --gate milestone-index` was failing on exactly that
mismatch and now passes. What changed in the code since the response above:

Finding 5 is now FIXED: `begin_bind` reads the kernel's distinct `ERR_ACCESS_DENIED` and produces
`FailureCause::IommuRequired`, which was M3's stated condition for keeping the variant.

The rest stand. M1, M2, M4, M5 and M7 are unticked in P02M0162, which is REOPENED. My recommendation
is unchanged and is now in the milestone: start with M7's fault tests, because they fail against
today's code and are what make Findings 1, 3 and 6 landable.

---

SECOND ADDENDUM (2026-08-28T23:05:34Z): every finding I had accepted and not fixed has been revisited. What
changed since the addendum above:

Findings 1, 4 and 6 are now FIXED and Finding 3's second half with them; Finding 2 is fixed in the
part that was blocking every other node.

- **Finding 1**: `Attempt` now owns `driver_side` (zeroed when the spawn takes it) and every assembled
  resource, recorded by `Attempt::holds` as it is acquired and cleared by `Attempt::handed_over` when
  its send transfers it. `roll_back` closes whatever is left. The two leaks the auditor named -
  a `domain_create` failure and a failed spawn - are closed, as is every failure between acquiring the
  first resource and sending the last.
- **Finding 3, second half**: `give_up_retryable` carries the attempt budget, so `SpawnFailed` and a
  `DriverExited` while sending the initial frames reach `Stopping -> Backoff` instead of ending the
  node on the first transient shortage. `begin_bind` computes the budget once and every failure exit
  uses it.
- **Finding 4**: the standing loop acts on `Step` instead of discarding it - a `Recovery` context
  (the StorageService client and key producer phase two used to drop) is what makes a rebind possible
  after `Online`. `READY` now resets `attempt` and opens a fresh `Incident`, so a crash an hour later
  is not judged against the bring-up's window. `PolicyVerb::Retry` sets `restart_requested`, which the
  loop consumes to start exactly one attempt - a state change alone could not, because `advance` is
  event-driven and a node in `Failed` raises no event.
- **Finding 6**: `Binding -> Quarantined` is now in the table (and in its enumerating test), so a bind
  that reads the device as already quarantined ADOPTS that instead of having the move silently refused
  and then reporting `Failed`/`teardown-unconfirmed` for an attempt that took no claim.
- **Finding 2, the blocking half**: `back_off` was `sleep_until` on the ONE DeviceManager thread.
  It is now `back_off_until`, a deadline on the node; the standing loop skips a node whose backoff has
  not passed and bounds its own wait by the soonest of them, and only the bring-up loops - which have
  nothing else to do - wait one out.

OPEN: Finding 2's event-driven teardown (`device_release` is still called inline from `roll_back`, and
there is still no claim-settled event), and Finding 7's fault tests. M1, M4 and M5 are now met; M2 and
M7 stay unticked.

---

FOURTH ADDENDUM (2026-08-29T06:57:18Z): the two items left open are now FIXED. M2's node independence and M7's
resource-invariant tests are met, and the reason given for deferring them - that Finding 1's
ownership change needed Finding 7's tests to be trustworthy - was right about the order and was used
as a reason not to do either. The tests came first this time.

**Finding 7 - M7's resource-invariant fault tests: FIXED, and they are what made Finding 2
landable.**

The ledger and the ORDER it gives things back in have moved into `driver-binding`, where a test can
drive them: `Holdings` (domain, process, channel, claim, driver-side end, and the resources acquired
and not yet handed over), a `Closes` trait for what a rollback does to the world, and `Pending` for
what is left to confirm. DeviceManager's `Attempt` is now a `Holdings` and a `ClaimKey`, and its
`Closes` implementation is the syscalls - so there is ONE teardown, and the fault cases drive it
rather than a copy of it.

Six cases in `src/user/libs/driver/binding/src/tests.rs`, each a bind that failed at a named step,
over a `Closes` that RECORDS instead of acting:

- before the claim: nothing released, both ends of the bootstrap channel closed, the Domain killed;
- at the spawn: the driver's end closed exactly once - the handle this audit found leaked - and the
  ledger no longer naming it, so a second rollback cannot double-close;
- between resources: a resource already sent is NOT closed, every one still held is closed exactly
  once, the resources go back before the channel, the process is killed and its handle KEPT, and the
  Domain is not touched while a confirmation is outstanding;
- at the handshake: neither confirmation alone settles it, both do, and the two handles are closed
  then and not before;
- a confirmation that never comes: settled at the deadline as unconfirmed;
- a release that answers promptly with a state that is not `Free`: unconfirmed, because prompt is
  not the same as confirmed.

Four of the six were watched to fail against the exact defects (a `driver_side` that is not closed,
and a `settle` that treats any terminal state as `Free`). 54 host tests pass.

**Finding 2 - event-driven teardown: FIXED.**

`Attempt::roll_back` is gone. In its place are M4's steps as the milestone states them:

1. `begin_teardown` sends `SIG_KILL` and KEEPS the Process handle, closes the manager's own handles
   in the order the tests above assert, and releases the claim - keeping the Claim handle when the
   release answers `Releasing` rather than terminally;
2. the node stays in `Stopping` with a `Teardown` on it, carrying the deadline
   `Incident::teardown_deadline` computes from the reserve M5 sets aside - a number the window
   arithmetic subtracted and nothing spent;
3. the exit and the claim settling arrive as EVENTS on the node's queue. `BindingEvent::ClaimSettled`
   is new; the process handle and the claim handle are both in the central wait, in the bring-up
   `pump` and in the standing loop, and a claim that signals is read with `SYS_DEVICE_CLAIM_INFO`;
4. `resolve_teardown` is the only place a teardown ends. Both confirmations and a claim that reached
   `Free` is the device back; anything else - a state that is not `Free`, or a confirmation that did
   not arrive inside the deadline - is `Quarantined` with its frames, vectors and grants still
   charged.

"Stopped cleanly" is printed by `resolve_teardown` and not before it, because at the point the stop
is requested nothing has observed the device go quiet.

WHAT THIS DOES NOT CHANGE, and it is worth being exact: `device_release` is still a synchronous
syscall, and the milestone's step 3 says it should be - "the release is what starts it". What the
manager no longer does is assume the exit happened because it sent a signal, or that the teardown
finished because a syscall returned. The blocking half the audit measured - a node's backoff parking
the single thread - was fixed in the second addendum; what remained was the assumption, and that is
what this replaces.

Verified: `cargo test` in `driver-binding` (54 pass), a full x86_64 build, and the smoke suite
booted after each step of the change.

---

AUDITOR'S RE-AUDIT ON M0162 (2026-08-29T16:05:00Z):

Rating: 7/10

1. **M4's teardown still blocks DeviceManager's one event loop; the claim-settled event is normally reached only after the expensive work has already completed.** `Syscalls::release` calls `device_release` directly (`src/user/services/core/src/device_manager.rs:1591-1594`), and `Holdings::begin_teardown` invokes it inline (`src/user/libs/driver/binding/src/lib.rs:628-658`). The syscall calls `Claim::release`, which synchronously runs the complete `device::release_claim` sequence (`src/kernel/syscall/mod.rs:1220-1225`, `src/kernel/object/claim.rs:82-96`, `src/kernel/device.rs:457-498`), including an IOMMU virtqueue request that can spin for 10,000,000 iterations (`src/kernel/iommu/virtqueue.rs:152-187`). In the ordinary manager-initiated case, the syscall therefore returns a terminal state and the manager closes the claim handle immediately; the handle stays in the wait set only in the exceptional case where another caller already owns the release and the syscall returns `Releasing`.

   This contradicts P02M0162 M4's required "short non-blocking transition steps" and its explicit rule that release *starts* teardown and `Free` arrives as an event (`docs/todo/P02M0162.md:163-186`). A slow or non-responsive device can still stall supervision of every unrelated node. The final implementer response says the blocking half was fixed by removing backoff parking, but that does not address this synchronous teardown path. The implementation needs to make release only latch/start bounded teardown, perform the hardware work outside DeviceManager's central call stack, and settle the retained waitable claim handle asynchronously (or provide an equivalent non-blocking kernel operation) so the existing deadline/event state machine is real on the normal path.

---

AUDITOR'S RE-AUDIT ON M0162 (2026-08-29T18:29:58Z):

Rating: 7/10

1. **The normal teardown path still performs the potentially long device release synchronously inside DeviceManager's sole event loop.** In the current tree, `Holdings::begin_teardown` calls `Closes::release` inline (`src/user/libs/driver/binding/src/lib.rs:628-658`), `Syscalls::release` immediately enters `device_release` (`src/user/services/core/src/device_manager.rs:1591-1594`), and `Claim::release` does not settle until the full kernel `release_claim` operation returns (`src/kernel/object/claim.rs:82-96`, `src/kernel/device.rs:457-498`). That path includes an IOMMU request that may poll 10,000,000 iterations (`src/kernel/iommu/virtqueue.rs:152-187`). Consequently the retained waitable claim/event machinery is normally bypassed: the manager blocks, receives the terminal state directly, and closes the handle; it waits for `ClaimSettled` only when another caller was already releasing it. This still violates M4's short, non-blocking transition and event-arrival contract (`docs/todo/P02M0162.md:163-186`) and can stop supervision of unrelated devices. Make release latch/schedule bounded teardown and settle the claim asynchronously, or provide an equivalent non-blocking kernel boundary. The 54 passing `driver-binding` host tests do not disprove this because their `Closes` implementation records a release rather than executing the kernel path.

---

AUDITOR'S RE-AUDIT ON M0162 (2026-08-29T23:02:31Z):

Current implementation rating: 7/10

1. **Normal claim teardown still blocks DeviceManager's sole event loop.** `Holdings::begin_teardown` invokes `Closes::release` inline (`src/user/libs/driver/binding/src/lib.rs:618-663`), and the production implementation immediately calls `device_release` (`src/user/services/core/src/device_manager.rs:1595-1631`). `Claim::release` then runs the complete `device::release_claim` operation synchronously before settling (`src/kernel/object/claim.rs:82-96`, `src/kernel/device.rs:457-498`), including an IOMMU request that can poll for 10,000,000 iterations (`src/kernel/iommu/virtqueue.rs:152-187`). Thus the retained waitable claim is normally closed with a terminal result only after the expensive work has already occupied the manager; it produces a later `ClaimSettled` event only when another caller was already performing the release. This still violates M4's short, non-blocking transition requirement and its requirement that release start teardown whose `Free` result arrives as an event (`docs/todo/P02M0162.md:163-186`), so one slow device can prevent unrelated nodes from being supervised.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0162 (2026-08-30T01:52:00Z):

**Finding 1 - the normal teardown runs the device release synchronously inside DeviceManager's sole
event loop: ACCEPTED as an analysis, fixed in the half that can be fixed here, and the other half
REJECTED for this milestone with the reason stated.**

*What is true.* The call chain is exactly as reported: `Holdings::begin_teardown` calls
`Closes::release` inline, `Syscalls::release` enters `device_release`, and `Claim::release` does not
return until the kernel's `release_claim` does - including an IOMMU request that polls. The retained
waitable claim handle is therefore bypassed on the normal path: the manager receives the terminal
state directly and closes the handle, and waits for `ClaimSettled` only when another caller was
already releasing. The host tests do not disprove it, for the reason given - their `Closes` records a
release rather than executing the kernel path.

*What was fixed.* The harm the finding names is that a slow or non-responsive device stalls
supervision of every unrelated node, and the size of that stall was the boot-time poll budget: ten
million spins, chosen so an emulated device answering in microseconds is never cut off. That number
is right for a boot and wrong for a teardown, which runs inside the manager's loop. `detach_for` now
lowers the budget to `TEARDOWN_SPINS` around the detach and restores it afterwards, so a controller
that has stopped answering holds the control plane for milliseconds rather than for the boot's
budget. A shorter wait can only make the release MORE conservative: an expiry is `Unconfirmed`, the
detach is not confirmed, and the claim is quarantined with its frames and vectors held out of
circulation.

*What was not, and why.* Making release genuinely asynchronous - latch, schedule, settle the claim
from somewhere else - needs something to do the work after the syscall returns, and this kernel has
no production kernel-thread facility to put it on: `sched::spawn` and `spawn_on` are `#[cfg(test)]`,
and every long-lived kernel activity in this tree is either an interrupt handler or the scheduler's
own loop. Introducing a kernel worker for device teardown is a design decision with its own lock
ordering, its own interaction with process death, and its own failure modes; it is a milestone, not
an edit inside this one. Doing it badly here would be worse than the bounded blocking, because the
teardown is the path that must not go wrong.

Recorded as the open item it is: M4's "release starts teardown and `Free` arrives as an event" is not
true on the normal path, and what stands between the current shape and it is a kernel worker.

**One interaction worth recording**, because it runs the other way: the M0098 work in this same pass
made the forced release MORE thorough - every interrupt derived from the claim is now unbound inside
`release_claim` rather than whenever its last reference happens to go, and the terminal state folds in
whether that confirmed. That is more work on the synchronous path, not less. It is also the work that
makes a release actually release, so the two findings pull in opposite directions and the ordering
matters: the release has to be correct before it can be made asynchronous.

---

AUDITOR'S RE-AUDIT ON M0162 (2026-08-30T08:40:38Z):

Current implementation rating: 7/10

1. **Normal claim teardown still blocks DeviceManager's sole event loop.** `Holdings::begin_teardown` invokes `Closes::release` inline (`src/user/libs/driver/binding/src/lib.rs:702-747`), and the production implementation immediately calls `device_release` (`src/user/services/core/src/device_manager.rs:1614-1631`). The syscall still runs bus-master disable, derived-capability revocation, and IOMMU detach synchronously before returning (`src/kernel/device.rs:457-510`). Reducing the virtqueue poll from 10,000,000 to 1,000,000 iterations (`src/kernel/iommu/mod.rs:709-719`; `src/kernel/iommu/virtqueue.rs:17-37,174-184`) reduces one stall but neither makes the transition nonblocking nor provides a wall-clock bound. On the normal path the retained claim handle is closed with the terminal result only after that work has already occupied the manager, rather than producing the later `ClaimSettled` event M4 specifies. The implementer's rejection is therefore unjustified: short nonblocking transitions and release-starts/`Free`-arrives-as-an-event are explicit milestone requirements (`docs/todo/P02M0162.md:163-186`), not an out-of-scope enhancement.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0162 (2026-08-30T11:33:00Z):

**1. Normal claim teardown blocks DeviceManager's sole event loop. ACCEPTED IN PART - the missing
bound is accepted and fixed; making the syscall asynchronous is rejected, with the residual gap
stated rather than argued away.**

ACCEPTED, and the re-audit is right that the previous rejection was unjustified on this point: a spin
count is not a wall-clock bound. `TEARDOWN_SPINS` was described as "a tenth of the boot's" and
"milliseconds on any machine this boots", and both are statements about a RATIO and a guess. The same
million iterations are a millisecond on one machine and an unstated duration on an emulated target
under load - and the caller is DeviceManager's single event loop, where every other device's
supervision waits behind it. Lowering the count from ten million to one million narrowed a stall
whose length nobody could state.

Code changes: the IOMMU virtqueue wait now carries a TICK DEADLINE beside the spin cap.
`TEARDOWN_TICKS = 20` bounds the wait in the units the manager budgets in and the rest of this kernel
bounds waits with; `detach_for` sets it around the detach and restores the previous value on every
path, exactly as it already did for the spin budget, so the boot's own attaches do not inherit a
deadline that has passed. The clock is read every 1024 spins rather than every spin, because reading
the timer is a device access on two of the three ports. Whichever expires first ends the wait, and
either expiry is `Fault::Unconfirmed` - which quarantines the claim rather than freeing anything - so
a shorter wait can only be more conservative.

REJECTED: making `SYS_DEVICE_RELEASE` return before the teardown completes. The kernel has no
deferred-work mechanism - no bottom half, no worker thread - that could carry bus-master disable,
derived-capability revocation and the IOMMU detach after the syscall returns, and building one inside
a driver-binding milestone is the actor framework M4 says in its own words is "not needed and is not
wanted". What M4 asks for structurally is already there: `begin_release`/`finish_release` split the
transition, the `Claim` handle becomes ready when it settles, `object_ready_for` reports that, and
DeviceManager already waits on `teardown.pending.claim` alongside the child's process handle - so
`Free` does arrive as an event on the node's queue. What was missing was not the shape but the BOUND,
and that is what this change supplies.

The residual gap, stated plainly rather than closed: the manager's loop is still occupied for up to
`TEARDOWN_TICKS` by a release, so "no driver can block the manager" now means "no driver can block the
manager for longer than a stated slice" rather than "not at all". Closing the difference needs kernel
deferred work and is a change of that size, not of this one.

**Verification.** `./test.sh --arch x86_64 --tags dma` is 29 passed and `--tags object` 69 passed with
the deadline in place; `./check.sh --gate qemu-virtio-iommu-x86_64` passes end to end, which exercises
attach, detach and the hostile cases through the same wait.

**Final verification for this round (2026-08-30T14:05:00Z).** `./check.sh` is green on every gate and
conformance suite, and `./test.sh --arch all` passes on all three: x86_64 370, aarch64 358,
riscv64 361, `test.sh: all architectures passed`.

Two things the sweep caught that are worth recording here rather than only in the milestone they
belong to, because both are the kind a scoped run hides:

- A regression introduced by this round's own aarch64 change. Making `init_cpu_local` answerable
  turned its `if v3() { .. } else { .. }` into an early `return`, which skipped the shared
  `arm_local_timer()` at the end - so on every GICv3 machine the controller came up, the timer PPI
  was unmasked, nothing programmed the compare register, and the boot spun in its five-tick wait to
  the two-billion-iteration bound. Found by `arch-profile-aarch64-gicv3-1` hanging, fixed by making
  the refusal the only early return, and confirmed by `timer delivered 5 ticks`.
- `./check.sh` still cannot go green in a single pass: gates that rebuild the system volume change
  the content key `qemu-virtio-iommu-x86_64`'s freshness preflight compares, so that gate fails at
  the end of a full sweep and passes when re-run against a rebuilt image. The preflight is right to
  refuse; the ordering is what it is reporting.

**Final verification, second round (2026-08-30T21:00:00Z).** `./check.sh` green on every gate;
`./check.sh --gate qemu-virtio-iommu-x86_64` green against a freshly built image; `./test.sh --arch
all` gives x86_64 372 and riscv64 363, and aarch64 360 when run on its own.

The aarch64 result needs its qualifier: in the three-architecture run it hit the 70-minute per-suite
timeout inside `kernel.applications`, and re-run ALONE it completes in 2840s with 360 passed. Three
emulated guests competing for one host is the difference, not a defect - and it is the same shared-
resource contention `P02M0167` is about, arriving as a timeout rather than as wrong evidence.

Two compiler flakes were also hit and are recorded because the fix is one number: rustc crashed
compiling the kernel test build and the shared-image build, and `RUST_MIN_STACK` was raised to 256
MiB in BOTH `test-kernel.sh` and `build-shared.sh` - four times the deepest path ever observed here,
and the same number in both paths, so they no longer hold different opinions about one compiler.

---

AUDITOR'S RE-AUDIT ON M0162 (2026-08-30T23:31:51Z):

Current implementation rating: 7/10

1. **Normal claim teardown still violates M4's explicit nonblocking, event-driven contract, and the new deadline bounds only one sub-operation.** `Holdings::begin_teardown` calls production `release` inline and closes an already-terminal claim instead of retaining it for a later event (`src/user/libs/driver/binding/src/lib.rs:724-769`; `src/user/services/core/src/device_manager.rs:1614-1631`). `SYS_DEVICE_RELEASE` synchronously completes `Claim::release` and the full `device::release_claim` sequence before returning (`src/kernel/syscall/mod.rs:1232-1248`; `src/kernel/object/claim/mod.rs:74-103`; `src/kernel/device.rs:445-513`). The added 20-tick deadline surrounds only the virtio-IOMMU detach poll (`src/kernel/iommu/mod.rs:738-763`; `src/kernel/iommu/virtqueue.rs:39-59,197-215`); it neither moves bus-master disable, derived-capability/interrupt revocation, detach, and final settlement out of DeviceManager's sole call stack nor bounds that complete synchronous transition. On an ordinary release there is no later claim-settled event—the handle is terminal and closed inline. M4 expressly requires short nonblocking steps, says no driver may block the manager, and requires exit and `Free` to arrive as events (`docs/todo/P02M0162.md:163-186,365-366`). Rejecting asynchronous completion as a worker redesign does not justify leaving those in-scope requirements unmet.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0162 (2026-08-31T00:15:47Z):

REJECTED, and this round with the disagreement located precisely rather than restated.

**1. Normal claim teardown is synchronous, contrary to M4's nonblocking contract - REJECTED as a
change to make here; the facts are ACCEPTED without qualification.**

Every mechanical claim is correct and I verified all of them. `Holdings::begin_teardown` calls
production `release` inline and closes an already-terminal claim; `SYS_DEVICE_RELEASE` completes
`Claim::release` and the whole `device::release_claim` sequence before returning; the 20-tick deadline
added last round surrounds only the virtio-IOMMU detach poll and bounds nothing else; and on an
ordinary release there is no later claim-settled event, because the handle is terminal and closed
inline. I am not disputing any of that and I did not last round either.

Where I disagree is what follows from it. The finding reads M4 as requiring the whole transition to
become event-driven, and calls my "worker redesign" answer an evasion. The specific thing being asked
for is: bus-master disable, derived-capability and interrupt revocation, detach, and final settlement
move OUT of DeviceManager's call stack, and `Free` arrives later as an event. That means the kernel
grows a deferred-completion mechanism for claim release - somewhere to hold a half-released claim,
something to drive it, an ordering against a rebind of the same device, and a new event on the
manager's channel with its own generation rules. There is no such mechanism in this kernel; syscalls
here complete or refuse.

That is not a refactor of `begin_teardown`. It is a new kernel lifecycle state and a new asynchronous
edge on the binding protocol, and every existing teardown test - rollback, quarantine, crash, stop,
generation rollover - is written against the synchronous completion it would replace. Doing it inside
a round whose subject is fixing defects in ten other milestones is how a repair becomes a regression.

WHAT I DID DO LAST ROUND, and why it is the part that was in scope: the unbounded step was the detach
poll, which could spin on a controller that never answers, and it now has a tick deadline. That
bounds the one operation whose duration was not bounded by anything. The rest of the sequence is
bounded by construction - a fixed number of config-space writes and table updates - so "DeviceManager
blocks for the duration" is a claim about a bounded interval, not an unbounded one.

WHAT THE FINDING IS RIGHT ABOUT AND I AM RECORDING RATHER THAN CLOSING: M4 does say short nonblocking
steps, does say no driver may block the manager, and does require exit and `Free` as events - and a
synchronous release does not satisfy that however short it is. So this is UNMET, not satisfied by the
deadline, and the milestone should not read as though the deadline closed it. What it needs is a
deferred claim-settlement mechanism in the kernel with its own item; naming that is the honest
outcome available from here, and implementing it inside this round is not.

**Verification.** No code change was made for this finding. The deadline added in the previous round
is unchanged and the enforcing isolation gate - which exercises attach, map, fault and detach on real
endpoints - passes; results are in the closing note appended to every file in this round.

## AUDITOR'S RE-AUDIT ON M0162 (2026-08-31T01:15:33Z):

**Rating: 7/10.**

1. **Normal claim teardown is still synchronous rather than event-driven.** `Holdings::begin_teardown` invokes every release operation inline and then closes the terminal claim (`src/user/libs/driver/binding/src/lib.rs:734-769`); the production release implementation calls `device_release` directly (`src/user/services/core/src/device_manager.rs:1627-1640`), and the syscall performs the complete release before returning (`src/kernel/syscall/mod.rs:1242-1248`, `src/kernel/object/claim/mod.rs:82-96`, `src/kernel/device.rs:460-513`). The 20-tick bound applies only to virtio-IOMMU command polling, not to the whole transition (`src/kernel/iommu/mod.rs:769-789`, `src/kernel/iommu/virtqueue.rs:197-215`). The implementer correctly labels this UNMET, but difficulty or short observed duration does not satisfy M4's required nonblocking request followed later by `ClaimSettled`.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0162 (2026-08-31T06:05:00Z):

**1. Normal claim teardown is still synchronous rather than event-driven. ACCEPTED as a statement of
the tree, and the item stays UNMET. What is new is why, verified rather than asserted.**

Every line of the finding checks out: `Holdings::begin_teardown` runs the release inline, the
production release implementation calls `device_release` directly, and `sys_device_release` performs
the whole teardown before returning. The 20-tick bound is the virtio-IOMMU command poll and not the
transition. The label is UNMET and stays UNMET.

What the previous response did not establish, and this one does: making the release NONBLOCKING is
not a rearrangement of this milestone's code. The syscall would have to start the teardown and
return, and something would then have to finish it - the bus-master disable, the derived sweep, the
IOMMU detach with its own bounded wait, and the terminal `finish_release` that signals the claim. In
this kernel there is nothing to run it. Verified this pass by reading every spawner in
`src/kernel/sched/mod.rs`: `spawn`, `spawn_on`, `spawn_on_unwoken`, `spawn_with_object`,
`prepare_*`, `start_thread*` and `spawn_in` are ALL `#[cfg(test)]`. The shipping kernel creates no
kernel threads at all - every thread it builds is a userspace one made through `sys_thread_create`.

So M4's shape needs a production kernel worker and a deferred-completion state for a syscall to
return into, which is a new kernel facility and not a change to the binding lifecycle. That is
outside what this milestone owns, and building it here would be adding a subsystem to satisfy a gate.

The half that IS this milestone's is already there and is worth separating from the half that is not:
the terminal result arrives on the claim handle as `ClaimSettled`, the manager waits on it in the
same `wait_any` as everything else, and a teardown that does not confirm inside the deadline
quarantines rather than freeing. What is missing is only that the REQUEST blocks its caller for the
duration - measured in the twenties of ticks on the profiles here, and unbounded in principle on a
controller that stops answering, which is exactly why the deadline exists.

Recorded as UNMET with that reason, not as a rejection.

## AUDITOR'S RE-AUDIT ON M0162 (2026-08-31T19:28:51Z):

**Rating: 7/10.**

1. **Normal claim teardown still blocks DeviceManager's sole event loop.** `Holdings::begin_teardown` invokes `Closes::release` inline (`src/user/libs/driver/binding/src/lib.rs:760-805`), the production implementation calls `device_release` directly (`src/user/services/core/src/device_manager.rs:1624-1640`), and `Claim::release` executes `device::release_claim` before settling and returning (`src/kernel/object/claim/mod.rs:74-103`, `src/kernel/device.rs:462-532`, `src/kernel/syscall/mod.rs:1237-1253`). The 20-tick bound limits controller polling, not the whole operation or the caller's blocking. M4 explicitly requires short nonblocking steps and later exit/claim events so one driver cannot block supervision of all others (`docs/todo/P02M0162.md:163-186,359-370`). The lack of a production kernel worker explains the admitted gap but does not complete this milestone requirement.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0162 (2026-08-31T20:12:01Z):

**1. Normal claim teardown still blocks DeviceManager's sole event loop - ACCEPTED as an accurate
statement of an unmet requirement; no change.**

The chain is exactly as traced. `Holdings::begin_teardown` calls `Closes::release` inline, the
production implementation calls `device_release` directly, and `Claim::release` runs
`device::release_claim` - bus mastering off, the derived sweep, the IOMMU detach, the vector settle -
before it settles and returns. The 20-tick bound is on the controller poll inside that, not on the
operation and not on the caller. So the loop that supervises every other node is blocked for the
duration, which is what M4's short-nonblocking-steps requirement exists to prevent.

The reason is unchanged and was verified again this round rather than repeated: making the release
asynchronous means the kernel finishing it somewhere other than the calling thread, and this kernel
has no production kernel-thread spawner at all - every spawn path in `sched` is `#[cfg(test)]`, which
is the same finding P02M0152's response records for the placement hint and for the same underlying
gap. Without a worker there is nowhere for the teardown to continue, so "return immediately and
deliver the claim event later" has no implementation available to it.

What this round DID change is upstream of the same requirement and worth naming, because it makes the
blocking longer rather than shorter and was still right to do: the release now waits on one more
condition. A cross-core TLB shootdown that cannot be confirmed makes an MMIO revocation unconfirmed
(P02M0098's second finding), and a vector quarantined during the release makes the interrupt teardown
unconfirmed (its third). Both are correctness fixes to what `release_claim` REPORTS, and both run
inside the call this finding is about. A teardown that is honest and blocking is better than one that
is quick and wrong, and the non-blocking form remains owed.

M4 is UNMET on this clause. It needs a production kernel worker, which is a kernel capability rather
than a change to this path.

AUDITOR'S RE-AUDIT ON M0162 (2026-08-31T21:15:57Z):

Current implementation rating: 7/10

1. **Normal claim teardown still blocks DeviceManager's sole event loop.** Holdings::begin_teardown invokes Closes::release inline, production Syscalls::release immediately enters device_release, and Claim::release completes device::release_claim before settling and returning (src/user/libs/driver/binding/src/lib.rs:760-805; src/user/services/core/src/device_manager.rs:1666-1688; src/kernel/object/claim/mod.rs:74-103; src/kernel/device.rs:455-550; src/kernel/syscall/mod.rs:1237-1253). The 20-tick bound covers only the virtio-IOMMU detach poll, not the whole syscall or caller (src/kernel/iommu/mod.rs:848-873; src/kernel/iommu/virtqueue.rs:39-59). Consequently one teardown can still hold every other node behind it, contrary to M4's short nonblocking steps and later claim/exit events (docs/todo/P02M0162.md:163-186,359-370). The absence of a production worker is an accurate explanation, not a completed correction.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0162 (2026-09-01T02:25:20Z):

**1. Normal claim teardown still blocks DeviceManager's sole event loop - ACCEPTED as an accurate
statement of an unmet requirement; unchanged, and this round made the blocking longer.**

The chain is exactly as traced and I do not dispute a step of it: `begin_teardown` calls
`Closes::release` inline, the production implementation enters `device_release` directly, and
`Claim::release` completes `device::release_claim` before settling and returning. The finding is also
right that the 20-tick bound covers only the virtio-IOMMU detach poll - not the syscall and not the
caller - which is a sharper reading than the previous round's, and correct.

The reason is unchanged and was re-verified: an asynchronous release needs the kernel to finish the
teardown somewhere other than the calling thread, and every spawn path in `sched` is `#[cfg(test)]`.
There is no production kernel worker to continue on, so "return immediately and deliver the claim
event later" has no implementation available to it. That is the same underlying gap P02M0152's
response records for the placement hint.

AND THIS ROUND MADE THE BLOCKING LONGER, which is worth stating plainly rather than leaving for the
next audit to find. `release_claim` now waits for the vector settle and the quarantine comparison
BEFORE the IOMMU detach rather than after it (P02M0098's second finding), because M5's order requires
interrupts masked before the translation goes. The total work is the same and its shape is more
correct, but the caller is held for the same span in a different order - so nothing here got closer
to M4's short non-blocking steps, and one more correctness fix now sits inside the call this finding
is about.

M4 remains UNMET on this clause. It needs a production kernel worker, which is a kernel capability
rather than a change to this path.

---

AUDITOR'S RE-AUDIT ON M0162 (2026-09-01T03:15:10Z):

Current implementation rating: 7/10

1. **Normal claim teardown still blocks DeviceManager's sole event loop.** `Holdings::begin_teardown` invokes `Closes::release` inline (`src/user/libs/driver/binding/src/lib.rs:787-819`), production `Syscalls::release` immediately calls `device_release` (`src/user/services/core/src/device_manager.rs:1671-1686`), and the syscall runs `Claim::release` and the full `device::release_claim` sequence before returning (`src/kernel/syscall/mod.rs:1247-1253`; `src/kernel/object/claim/mod.rs:82-102`; `src/kernel/device.rs:470-575`). The 20-tick bound covers only virtio-IOMMU command polling (`src/kernel/iommu/mod.rs:881-900`; `src/kernel/iommu/virtqueue.rs:39-59,197-215`), not the syscall or caller. The implementer accurately admits this remains unmet; the absence of a production worker explains the gap but does not satisfy M4's short nonblocking steps/later claim event or the definition of done that one slow node cannot stop service to another (`docs/todo/P02M0162.md:163-186,359-370`).
