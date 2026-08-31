AUDITOR'S REVIEW ON M0165 (2026-08-28T20:31:10+02:00):

Rating: 3/10

The sequenced protocol codec, the main DeviceManager heartbeat state, bounded pre-teardown diagnostics, one child Domain per binding, and the kernel's claim deadline and late-completion latch are substantial working pieces. The protocol and binding host suites pass. However, the development driver does not compile or service the control channel, planned STOP is not a clean stop in the actual drivers, reverse dependency ordering is not a dependency traversal, the required device ledger is absent, and both DeviceManager-crash recovery paths fail under the configured policy. These are central milestone requirements, not optional hardening.

## Findings

1. **The development control-channel driver neither builds nor services M1's heartbeat in its normal work loop.** `dev_channel` is a registered driver with `heartbeat-deadline = 100` (`src/user/services/manifest.toml`, lines 1439-1460), but its normal `pump` waits only on `[irq, bytes]`; the manager bootstrap channel is absent from both the polling and wait paths (`src/user/drivers/core/src/dev_channel.rs`, lines 172-234). The `bind` parameter added to `pump` is consequently unused.

   The only attempted heartbeat handling is in `adopt`, where `bind` is not in scope (`dev_channel.rs`, lines 249-280). The repository's own `tools/check-development-build.sh` fails with `E0425` at lines 275 and 278 and also rejects the unused `bind` under the crate's deny-warnings policy. Even if that name error were ignored, `adopt` receives into a 16-byte buffer, smaller than the 20-byte frame header and the 24-byte PING frame, so `Header::decode` cannot accept a PING there. This makes the claim that every serving driver answers in its own loop false and leaves the development configuration unusable.

2. **Driver-side STOP acknowledges completion before doing the drain, flush, and quiesce that `STOPPED` is defined to certify.** `drain_control` returns `Control::Stop` as soon as it reads STOP. `wait_or_answer`, `serve_or_answer`, and `answer_ping` immediately call `stopped` and then return (`src/user/drivers/core/src/common.rs`, lines 404-529). `stopped` only sends an empty STOPPED frame; it has no device capability and performs no cleanup (`common.rs`, lines 533-539). Callers uniformly exit when these helpers return, so there is no driver-specific cleanup after the acknowledgement.

   This is observable in drivers that have required cleanup operations already implemented. `virtio_blk` has `flush_request`, but the STOP return at `serve_blocks` lines 243-249 exits without calling it. `virtio_snd` can have `started` playback or `capturing` input and has STOP/RELEASE commands for both, but the manager STOP exits at lines 320-325 before the cleanup used in its service-channel-close branch at lines 354-362. Across the driver crate, the only calls to `device_quiesced` are during the initial virtio reset (`src/user/drivers/core/src/virtio.rs`, lines 171-185) and initial xHCI reset (`src/user/drivers/core/src/xhci.rs`, lines 475-486), not on STOP. The kernel explicitly uses that call to release orphaned DMA frames and pending MSI vectors (`src/kernel/syscall/mod.rs`, lines 1284-1324).

   Two drivers do not even send STOPPED. `common::stand`, used by `virtio_console`, treats every opcode other than PING as terminal and exits (`common.rs`, lines 370-392). `dev_channel` does not read the bootstrap channel in its normal pump at all. DeviceManager therefore either accepts an untrue clean acknowledgement from most drivers or reaches its forced timeout for these drivers. M3's one-round-trip clean stop is not implemented.

3. **DeviceManager records a valid STOPPED as a driver crash and can print `stopped cleanly` before learning that teardown quarantined.** `drain_channel` queues any generation-matching STOPPED without requiring an active planned stop (`src/user/services/core/src/device_manager.rs`, lines 1916-1918). In `advance`, the STOPPED arm prints `stopped cleanly` and then returns `FailureCause::DriverExited` (`device_manager.rs`, lines 2401-2413). The shared failure path immediately captures and prints an incident with that cause (`device_manager.rs`, lines 2427-2432); the rendered cause is `it exited without saying anything` (`device_manager.rs`, lines 2026-2037). `give_up_with` also carries that failure cause into the binding record where a next state exists.

   The clean message is emitted before `Attempt::roll_back` calls `device_release` and determines whether the result is confirmed or quarantined (`device_manager.rs`, lines 2446-2455; lines 1633-1659). Thus an unconfirmed planned teardown can produce a clean-stop claim before later landing in `Quarantined`. This directly contradicts M3's requirement that a planned stop is not a crash and that an unconfirmed or forced teardown never claims a clean flush.

4. **Shutdown order is sorted by requirement count, not by reverse dependency order.** `stop_all` sorts nodes by `Reverse(entry.requires.len())` (`src/user/services/core/src/device_manager.rs`, lines 2936-2955). The number of direct requirements is not dependency depth. In a valid acyclic chain where A requires a kind provided by B and B requires a kind provided by C, both A and B have one requirement. Their tie remains in device enumeration order, so B can be stopped before its dependent A. More generally, a node with two unrelated direct requirements sorts before a deeper one-requirement consumer even when it is not its dependent.

   The manifest validator accepts such acyclic chains and rejects only cycles and orphan kinds (`src/tools/system-manifest/src/tests.rs`, around lines 405-438). The current eight driver entries declare no `requires`, so the reported current-machine shutdown is entirely tied and cannot demonstrate M4's promised ordering. The algorithm fails for ordinary registry inputs the feature expressly supports.

5. **M5's device-specific ledger is missing, and a quarantined claim can already have released its vector.** `DomainStats` accounts memory, handles, threads, IPC, DMA, and stack, but not MMIO windows, IRQ vectors, or IOMMU grants (`src/abi/src/lib.rs`, lines 756-781). The milestone requires those remaining holdings to be reconstructed from the kernel claim snapshot. The actual `DeviceClaimSnapshot` contains only `state`, `generation`, and `release_deadline` (`src/abi/src/lib.rs`, lines 887-902), and the kernel claim slot stores and returns only those fields (`src/kernel/device.rs`, lines 208-243 and 370-386). DeviceManager's `granted_resources` is merely the count of RESOURCE frames sent during the current bind (`device_manager.rs`, lines 1143-1145 and 2222-2261); it is initialized to zero for a reconstructed node and is never populated from the snapshot. There is therefore no per-binding ledger of the device-specific resources M5 names and no way for a new manager to reconstruct those charges.

   The teardown order also violates the required quarantine accounting. `release_claim` calls `release_msi_for_device` before it asks the IOMMU to detach and before it knows whether teardown confirmed (`src/kernel/device.rs`, lines 404-435). For a retired pending vector, `MsiRegistry::release_for_device` clears `pending`, clears its owner, and marks the slot unused (`src/kernel/arch/common/msi.rs`, lines 244-264). If the later IOMMU detach is unconfirmed, `finish_release` makes the claim `Quarantined`, but that vector has already become reusable. M5 explicitly requires an unconfirmed teardown to leave its resources charged and out of circulation.

6. **ServiceManager does not kill DeviceManager's driver subtree on the crash path its manifest actually selects.** ServiceManager correctly creates a Domain for the pinned DeviceManager and spawns it there (`src/user/services/core/src/service_manager/bootstrap.rs`, lines 284-304). Its only `domain_kill` for that Domain is inside `restart_service` (`src/user/services/core/src/service_manager.rs`, lines 1276-1320). The supervisor invokes `restart_service` on peer close only for `Restart::Transparent`; every other policy merely records `Failed` and removes the channel (`service_manager.rs`, lines 1630-1655).

   DeviceManager is declared `restart = "escalate"` (`src/user/services/manifest.toml`, lines 1865-1871), so its real crash follows the branch that never calls `domain_kill`. Its child driver processes can remain live with device claims after their manager is gone, exactly the state M6 says must be removed before either reconstruction or escalation. This finding does not criticize the absence of an actual relaunch, which the milestone explicitly places out of scope; it concerns the required subtree kill before escalation.

7. **A reconstruction that observes `Releasing` does not re-read until `Free` or the claim deadline.** `observe_claim` returns `WaitAndSeeAgain` for `CLAIM_STATE_RELEASING` (`src/user/services/core/src/device_manager.rs`, lines 2881-2933). `begin_bind` has already moved the node into `Binding` when it makes that observation, and it simply returns `false` for `WaitAndSeeAgain` (`device_manager.rs`, lines 2133-2167). There is no deadline scheduling or poll registration.

   The callers interpret `false` as candidate failure, not a request to revisit the claim. The non-boot `start_candidate` increments `node.candidate` and continues through the remaining candidates (`device_manager.rs`, lines 795-834); because the record is still `Binding`, a subsequent candidate cannot even enter `Binding` again. The boot path only inserts a node when `begin_bind` returns true (`device_manager.rs`, lines 621-629), so it drops a releasing device entirely. `CLAIM_STATE_RELEASING` has no other DeviceManager handling. Consequently, a later `Free` is never bound and a deadline-expired release is never re-read to latch and adopt `Quarantined`, contrary to every non-Free branch in M6's reconstruction table.

8. **The required negative and named-race tests do not exercise the production decisions they claim to guard.** The three heartbeat refusal tests only assert that enum variants and integer values differ (`src/user/libs/driver/protocol/src/tests.rs`, lines 252-291). They never drive DeviceManager's `drain_channel` or heartbeat state, so they would still pass if production reset the watchdog on any opcode, generation, or sequence. The race tests have the same gap. For example, `a_crash_between_publish_and_subscribe_withdraws_what_was_published` never invokes a catalogue or withdrawal and explicitly says `Whatever the catalogue does next` before comparing two IDs (`src/user/libs/driver/binding/src/tests.rs`, lines 516-531). The manager-restart test only pushes two events into a local queue and pops with generation zero (`binding/src/tests.rs`, lines 553-564); it does not drive ServiceManager's Domain ownership or the claim snapshot path that findings 6 and 7 show are broken.

   None of the six named race tests asserts the required post-race process, handle, vector, mapping, or counter baselines. The crate is discovered by the general host-tests gate, but registration of tests that do not exercise these outcomes does not satisfy M7's requirement that the named table be driven and watched to fail.

## Verified implementation coverage

- `PING` and `PONG` have a shared sequence codec, the cadence is `(deadline + 1) / 2`, and DeviceManager drains a live channel before checking expiry. For drivers that use the common combined-wait helpers, a matching generation and exact outstanding sequence is required to clear the heartbeat.
- The manifest validator enforces `0 < heartbeat-deadline <= 100`.
- A wedged driver enters the same rollback transaction as a crash with the distinct `Hung` cause. The bounded diagnostic is captured before rollback and includes binding identity, state, cause, last opcode and age, attempt count, and readable Domain counters; it is printed and stored.
- `begin_bind` creates a child Domain and uses `spawn_in`; rollback kills the process, closes offers and control, releases the claim, and kills the Domain last.
- The kernel claim record mints its own release deadline, atomically latches an expired `Releasing` claim to `Quarantined` on snapshot, and refuses a late completion from returning it to `Free`.
- DeviceManager withdraws a binding's published providers before sending STOP and does not retry a node carrying a non-fault stop intent.

## Checks performed

- `tools/check-development-build.sh`: **failed**, because `drivers`/`dev_channel` does not compile with `--features development` (`bind` not found at lines 275 and 278; unused `bind` at line 172).
- `cargo test --manifest-path src/user/libs/driver/protocol/Cargo.toml`: **23 passed**.
- `cargo test --manifest-path src/user/libs/driver/binding/Cargo.toml`: **43 passed**.
- `cargo test --manifest-path src/tools/system-manifest/Cargo.toml`: **15 passed**.

No optional refactoring or additional features are proposed here. Each finding is a direct failure of an M1, M3, M4, M5, M6, or M7 requirement.

---

IMPLEMENTER'S RESPONSE ON M0165 (2026-08-28T20:22:52Z):

**Finding 1 - the development control-channel driver neither builds nor services the heartbeat: ACCEPTED and FIXED.**

This one is not a judgement call: I ran `src/tools/check-development-build.sh` and it failed exactly
as reported, with `E0425: cannot find value `bind` in this scope` at `dev_channel.rs` and an unused
`bind` parameter refused by the crate's deny-warnings policy. The development configuration did not
compile.

Everything else in the finding is also true. `pump` waited on `[irq, bytes]` with the manager's
bootstrap channel in neither the polling nor the wait path, so a driver serving normally never
answered a `PING` - and its registry entry declares `heartbeat-deadline = 100`. The only heartbeat
handling was inside `adopt`, the path taken when the agent above the driver has gone, where `bind` was
not in scope. And `adopt`'s receive buffer was 16 bytes: smaller than the 20-byte header alone, so it
could not have decoded a 24-byte `PING` even once the name error was fixed.

Changed in `src/user/drivers/core/src/dev_channel.rs`:
- new `heartbeat(bind, bootstrap)`, which drains the manager's channel without blocking, answers a
  `PING` for the current generation, closes any handle on a frame it did not expect, and reports the
  bootstrap closing as "this driver is finished";
- `pump` calls it each time round and now waits on `[irq, bytes, bootstrap]`;
- `adopt` takes `bind` (every call site passes it) and its buffer is 64 bytes.

VERIFIED: `src/tools/check-development-build.sh` now reports "the development configuration compiles,
with 2 development-only program(s)".

**Finding 2 - driver-side STOP acknowledges before the drain it certifies: ACCEPTED, not fixed.**

Confirmed. `drain_control` returns `Control::Stop` as soon as it reads STOP; `wait_or_answer`,
`serve_or_answer` and `answer_ping` immediately call `stopped`, which sends an empty STOPPED frame and
does nothing else; and every caller exits when they return. `virtio_blk` has `flush_request` and does
not call it on that path; `virtio_snd` has STOP/RELEASE for playback and capture and exits before the
cleanup its own service-channel-close branch performs. `device_quiesced` is called only during initial
resets, never on STOP - and the kernel uses that call to release orphaned DMA frames and pending MSI
vectors. `virtio_console` (through `common::stand`) and `dev_channel` do not send STOPPED at all.

Not fixed. A clean stop means each driver draining its own outstanding work before acknowledging, so
the acknowledgement has to move after a per-driver cleanup hook - a change to the shared helpers AND
to every driver that has work to finish. It is M3's central item and it is a piece of work.

**Finding 3 - a valid STOPPED is recorded as a crash and `stopped cleanly` can precede a quarantine: ACCEPTED, not fixed.**

Verified. `drain_channel` queues any generation-matching STOPPED without requiring an active planned
stop; `advance`'s STOPPED arm prints `stopped cleanly` and then returns `FailureCause::DriverExited`,
which the shared failure path renders as "it exited without saying anything". And the clean message is
emitted BEFORE `roll_back` calls `device_release` and learns whether the teardown confirmed, so an
unconfirmed teardown can print a clean-stop claim and then land in `Quarantined`.

Not fixed. The ordering half is small on its own; the cause half means STOPPED needs its own
non-failure outcome rather than being funnelled through `DriverExited`, which is the same lifecycle
change as Finding 2. Splitting them would leave a planned stop still classified as a crash.

**Finding 4 - shutdown order is by requirement COUNT, not reverse dependency order: ACCEPTED, not fixed.**

Confirmed in `stop_all`: `order.sort_by_key(|&at| Reverse(entry.requires.len()))`. The comment beside
it reasons "a node requiring something is a dependent and goes first", which holds only for
depth-one chains. In A requires B requires C, A and B both have one requirement, their tie falls back
to enumeration order, and B can be stopped before its dependent A. The manifest validator accepts such
chains - it refuses cycles and orphans - so this is an ordinary supported input, not an exotic one.

Not fixed. The correct key is dependency DEPTH over the provider graph, which the manager does not
currently build; computing it needs the kind-to-provider map at shutdown time. Worth noting for
whoever does it: the current eight entries declare no `requires`, so every node ties and the present
behaviour is indistinguishable from correct - which is why no test catches it and why a test has to
construct a chain rather than use the shipping manifest.

**Finding 5 - the device-specific ledger is missing, and a quarantined claim can already have released its vector: ACCEPTED, not fixed.**

Both halves verified. `DeviceClaimSnapshot` carries `state`, `generation` and `release_deadline` and
nothing about MMIO windows, IRQ vectors or IOMMU grants; `granted_resources` is a count of RESOURCE
frames sent during the current bind and is zero for a reconstructed node. And the ordering defect is
real: `release_claim` calls `release_msi_for_device` BEFORE the IOMMU detach and before it knows
whether teardown confirmed, so a vector can become reusable under a claim that then goes
`Quarantined` - which M5 says explicitly must not happen.

Not fixed. The vector-ordering half is the kind of thing I would normally take on its own, and I have
not, because it is the same question as M0153's Findings 3-5: what a teardown does when it cannot
confirm. Moving one release past the detach without deciding that leaves the quarantine accounting
half-consistent in a different place.

**Finding 6 - ServiceManager does not kill the driver subtree on the crash path DeviceManager's manifest selects: ACCEPTED, not fixed.**

Verified. The only `domain_kill` for DeviceManager's Domain is inside `restart_service`, the
supervisor calls that on peer close only for `Restart::Transparent`, and the manifest declares
DeviceManager `restart = "escalate"` - so its real crash takes the branch that records `Failed` and
removes the channel, leaving its child driver processes live with device claims. The auditor is
careful to note this is about the subtree kill and not about a relaunch, which the milestone excludes;
that framing is correct.

Not fixed. Killing a subtree on the escalate path is a change to ServiceManager's supervision policy,
and it needs deciding alongside what escalation then does.

**Finding 7 - a reconstruction that observes `Releasing` never re-reads: ACCEPTED, not fixed.**

Confirmed. `observe_claim` returns `WaitAndSeeAgain` for `CLAIM_STATE_RELEASING`, `begin_bind` returns
`false`, and both callers read that as candidate failure - the non-boot path moves to the next
candidate while the record is still `Binding` (so no later candidate can enter it), and the boot path
drops the device entirely. Nothing schedules a re-read against the claim deadline.

Not fixed: it needs the same deadline scheduling as Finding 2's teardown, in the same loop.

**Finding 8 - the negative and named-race tests do not exercise the decisions they guard: ACCEPTED, not fixed.**

Verified by reading them. The three heartbeat refusal tests assert that enum variants and integers
differ and never touch `drain_channel` or the heartbeat state;
`a_crash_between_publish_and_subscribe_withdraws_what_was_published` invokes no catalogue and says
"Whatever the catalogue does next" before comparing two ids; the manager-restart test pushes two
events into a local queue. None asserts a post-race process, handle, vector, mapping or counter
baseline.

Not fixed. These tests are the evidence for Findings 2 through 7, and they cannot be written before
the behaviour they would assert exists.

**Summary.** One fix - and it is the one that mattered most to land, because the development
configuration did not compile at all, which no amount of design work gets around. The other seven are
accepted and open, and six of them are two pieces of work: a driver-side clean-stop path, and an
event-driven teardown with the claim ledger it needs. P02M0165's ticks do not reflect that. I have not
edited the milestone document as part of this response.

---

ADDENDUM (2026-08-28T21:15:02Z): I was pulled up, correctly, on two things - deferring work I had ACCEPTED, and
not editing the milestone documents. Both are addressed. Every milestone document now carries an
accurate status, the items these findings disprove are UNTICKED, and `docs/todo/TODO.md` reopens the
twelve entries that were marked done; `./check.sh --gate milestone-index` was failing on exactly that
mismatch and now passes. What changed in the code since the response above:

Finding 4 is now FIXED. `stop_all` relaxes a dependency DEPTH over the provider graph and sorts
deepest first, instead of sorting by how many direct requirements a node declares. The eight shipping
entries declare no `requires`, so every node ties and the old behaviour was indistinguishable from
correct - which is why nothing caught it, and why the test for this has to construct a chain.

Finding 1 was already fixed. The other six stand; M3, M5, M6 and M7 are unticked.

---

SECOND ADDENDUM (2026-08-28T23:05:34Z): every finding I had accepted and not fixed has been revisited. What
changed since the addendum above:

Findings 2, 3, 5, 6 and 7 are now FIXED, leaving only Finding 8.

- **Finding 2**: the wait helpers no longer send `STOPPED` when they read a stop - they LATCH it
  (`common::stop_requested`), and `common::finish_stop` answers it after the driver's own cleanup and
  calls `device_quiesced` so the kernel may reclaim orphaned DMA frames and masked vectors.
  `virtio_blk` flushes first, `virtio_snd` stops and releases both streams, and `virtio_net`,
  `virtio_gpu`, `virtio_input` and `xhci` answer on their exit paths. `common::stand` and
  `dev_channel` - the two that never sent `STOPPED` at all - now answer immediately, which is honest
  because neither has work to drain.
- **Finding 3**: an unsolicited `STOPPED` is refused rather than queued, and the clean-stop line is
  printed AFTER the teardown has answered - a landing in `Quarantined` says so instead of claiming a
  flush that nothing observed.
- **Finding 5, the ordering half**: `release_claim` releases the MSI vectors only when the IOMMU
  detach CONFIRMED. They were freed before it, so an unconfirmed teardown left the claim `Quarantined`
  with a vector already back in circulation.
- **Finding 6**: the supervisor kills DeviceManager's child Domain on the crash path its manifest
  actually selects (`escalate`), so its driver subtree does not outlive it holding device claims.
- **Finding 7**: a `Releasing` claim moves the node to `Backoff` with a `retry_at` deadline and the
  standing loop re-reads when it comes due, instead of the record sticking in `Binding` while the
  caller moved to the next candidate.

OPEN: Finding 8, the named race and refusal tests, and the M5 device ledger half of Finding 5
(`DeviceClaimSnapshot` still carries no MMIO/IRQ/IOMMU holdings). M3, M6 and M7 stay unticked.

---

THIRD ADDENDUM (2026-08-29T04:40:51Z): Finding 5's LEDGER half is now fixed, and Finding 8 has its first fixture that
drives a real decision.

**Finding 5, the ledger: FIXED.** `abi::DeviceClaimSnapshot` carries `mmio_windows`, `irq_vectors`
and `iommu_grants`, counted at the moment of the read from the kernel's own records rather than from a
number somebody kept in step: `DERIVED` is every capability minted under the claim's key, the MSI
registry knows which slots the device owns (`MsiRegistry::held_by_device`, exposed on all three ports),
and `iommu::grants_for` counts what its domain still has mapped - live AND quarantined together,
because a quarantined mapping is charged exactly like a live one.

`observe_claim` adopts that count, so a manager reconstructing a node it did not bind no longer starts
its charge at zero while the kernel holds the window.

**Finding 8, in part: a fixture that drives the decision.**
`kernel.object.claim.a_claims_snapshot_names_what_it_still_holds` claims a device, derives an MMIO
window, reads the snapshot back and asserts the count - then releases and asserts the baseline returns
to zero, which is the post-restart baseline M5 names. It is the shape the audit asks for: it drives
the production path and asserts a holding, rather than comparing two enum values.

STILL OPEN in Finding 8: the six named RACE tests. Each needs two parties and a controlled interleaving
- a crash between publish and subscribe, a manager restart mid-teardown - and the kernel test harness
is cooperative and single-threaded, so those need a driving mechanism that does not exist yet. That is
the honest reason and it is not a small one.

---

FOURTH ADDENDUM (2026-08-29T06:57:37Z): Finding 8's REFUSAL half is now fixed. The race half is still open and the
reason is unchanged.

**Finding 8, the three heartbeat refusal tests: FIXED.**

The auditor's objection was exact - they asserted that enum variants and integers differ, so they
would have passed against a supervisor that reset its watchdog on any opcode, any generation and any
sequence, which is what `rt::heartbeat` does and what this milestone exists to have stopped. The
reason they could say no more is that the DECISION lived in DeviceManager, in a binary nothing can
run on a host.

`driver_binding::Heartbeat` now owns the state and its three decisions - `tick` (nothing due, ask
with this sequence, or wedged), `answered` (does this pong count), and `wake_at` (which of the two
deadlines the central wait is bounded by). DeviceManager is a `type Heartbeat =
driver_binding::Heartbeat` and calls them; there is one implementation, and it is the one the tests
drive.

Five cases, in `src/user/libs/driver/binding/src/tests.rs`:

- every way of answering wrong - a sequence never asked with, the one before any ping, the one not
  yet sent - leaves the ping outstanding; the right one clears it; and a DUPLICATE of the right one
  does not, because nothing is outstanding for it to answer;
- a ping unanswered inside its deadline wedges ONCE, and the watchdog then asks nothing more until
  the next binding arms it. Writing this found a real defect: clearing only the outstanding flag left
  the next ping already due, so the pass after the verdict sent a `PING` to a binding that was being
  torn down. A spent watchdog now answers `Idle` and `wake_at` returns 0;
- a driver whose entry declares no deadline is never asked and never wedged, and a declared zero is
  the same thing said the other way;
- a ping that could not be SENT is neither an answer nor a wedge - the channel has gone, which is a
  driver that ended, and the exit event arrives on its own;
- `wake_at` is whichever of the two deadlines is next, which is what bounds the central wait.

54 host tests pass in the crate.

**STILL OPEN: the six named RACE tests.** Each needs two parties and a controlled interleaving - a
crash between publish and subscribe, a manager restart mid-teardown - and the in-kernel harness is
cooperative and single-threaded. The two-party fixtures that DO exist in this tree
(`capability_tcb_two_threads_over_one_table` is the model) are threads inside one kernel test, and
building the equivalent for a DeviceManager restart means driving a supervisor's Domain ownership
from inside a guest test. That is the honest reason and it has not improved: what has changed is that
the refusal half, which needed no such mechanism, is no longer waiting behind it.

---

FIFTH ADDENDUM (2026-08-29T14:28:39Z): Finding 8's race half now asserts the baselines M7 names. What was missing was
not the races - five of them were there - but that none of them looked at what the race LEFT.

The auditor's words were exact: "None of the six named race tests asserts the required post-race
process, handle, vector, mapping, or counter baselines." A test that walks a `BindingRecord` through
two states and compares enum values says the table refused something; it says nothing about the
device, the child or the handles, and those are what M7 asks after each race.

The mechanism to ask with now exists: `Holdings`/`Pending`/`Closes`, built for M0162's Finding 7,
is the real teardown driven over a `Closes` that records. So each race ends by tearing a transaction
down through it and asserting, in `assert_baseline_after_teardown`:

- the claim is released EXACTLY ONCE - "at most one claim owner", as a count rather than a sentence;
- its handle, the process handle and the manager's channel end are each closed exactly once;
- the child is signalled once, and the Domain killed once and last;
- the ledger names nothing afterwards - no process, no claim, no Domain, no resource - so a second
  rollback closes nothing twice.

And the sequence is asserted, not assumed: `settle` answers `None` before the exit arrives, so a
teardown cannot call itself done on the strength of having sent a kill. Two of the races got a case
of their own beyond the shared baseline:

- **a watchdog expiry racing a clean exit**: one verdict is also ONE teardown. The second event finds
  a ledger with nothing in it, and the rollback is idempotent because it is EMPTY rather than because
  somebody remembered a flag.
- **a `STOPPED` after its deadline**: an unconfirmed teardown still gives back every HANDLE - what it
  does not give back is the DEVICE. That distinction is the whole of `Quarantined`, and a test that
  only read the record could not tell the two apart.

Watched to fail: making `begin_teardown` release the claim twice fails all four shared baselines with
"one owner, one release", and nothing else. 54 host tests pass.

WHAT IS STILL NOT DRIVEN FROM A HOST TEST, and it is one thing rather than a class: the CATALOGUE's
own withdrawal in the crash-between-publish-and-subscribe race. What that race turns on - a provider
id carrying its binding's generation, so the next binding's publications are distinguishable - is
asserted here, and the transaction baseline with it. `Catalogue::withdraw_binding` itself lives in
`device_manager.rs`, a binary nothing can run on a host, so "no stale provider" is exercised in the
guest and not here. That is the same boundary M0164's Finding 1 sits on, and moving the catalogue
across it is that milestone's work, not this one's.

---

AUDITOR'S RE-AUDIT ON M0165 (2026-08-29T16:05:00Z):

Rating: 6/10

1. **Drivers still acknowledge `STOPPED` without establishing the hardware quiescence that the acknowledgement certifies.** `common::finish_stop` merely calls the declarative `device_quiesced` syscall when given a nonzero capability and then sends `STOPPED` (`src/user/drivers/core/src/common.rs:684-716`); it does not reset or halt hardware. The kernel explicitly cannot verify that assertion and relies on the caller having just reset the device (`src/kernel/syscall/mod.rs:1285-1320`). The virtio block/sound fixes now flush or stop their logical work, but the virtio stop paths call `finish_stop` without resetting device status first. Worse, virtio-gpu passes `0` on both stop paths even though its live queue capability and DMA-backed scanout remain (`src/user/drivers/core/src/virtio_gpu.rs:440-455`), and xHCI also passes `0` and exits without halting/resetting the running controller (`src/user/drivers/core/src/xhci.rs:1000-1011`); their only reset/quiesce occurs during initial bring-up.

   P02M0165 M3 requires drain/abandon, flush, `device_quiesced`, and only then `STOPPED` (`docs/todo/P02M0165.md:128-147`). The current paths can report a clean planned stop and let orphaned DMA frames/vectors be reclaimed while queues or the xHCI controller may still be active. Each driver needs a device-specific STOP cleanup that stops accepting work, drains/cancels it, halts or resets the hardware and waits for confirmation, releases device-side resources/backings as applicable, then calls `device_quiesced` with the real capability and sends `STOPPED`. A failure to establish that condition must fall through to the forced/quarantine path, not claim a clean stop.

2. **The crash-between-publish-and-subscribe race still does not verify catalogue withdrawal.** The named host test explicitly says "Whatever the catalogue does next," compares two `ProviderId` generations, and checks only the generic transaction ledger (`src/user/libs/driver/binding/src/tests.rs:526-543`). No guest test found drives `Catalogue::withdraw_binding`; `kernel.hardware.device_manager_reacts_to_a_driver_crash` manipulates a local two-variant enum rather than running DeviceManager. The final response itself concedes that the catalogue half is not host-driven while claiming it is exercised in the guest, but the current test tree does not substantiate that claim. Consequently a regression that leaves the old provider in the live catalogue after a binding crash would pass M7's registered race tests, violating the required "no stale provider" post-race baseline. Move the catalogue/withdrawal decision behind a testable seam or add a production guest integration test that publishes, crashes before subscription, subscribes afterwards, and proves only the new generation is reachable.

---

AUDITOR'S RE-AUDIT ON M0165 (2026-08-29T18:29:58Z):

Rating: 6/10

1. **`STOPPED` still certifies hardware quiescence that several drivers never establish.** `common::finish_stop` only calls the trust-based `device_quiesced` syscall and emits `STOPPED` (`src/user/drivers/core/src/common.rs:684-716`); the kernel explicitly cannot verify the reset claim (`src/kernel/syscall/mod.rs:1284-1324`). The virtio stop paths do not reset device status before this call, and `virtio_gpu` and xHCI pass capability `0` and exit with no stop-time reset/halt (`src/user/drivers/core/src/virtio_gpu.rs:440-455`, `src/user/drivers/core/src/xhci.rs:1000-1011`). Their only reset/quiesce is during bring-up. This can classify a planned stop as clean and permit resource reuse while queues/controller DMA may remain active, contrary to M3's ordered drain/flush/quiesce/ack contract (`docs/todo/P02M0165.md:128-147`). Add device-specific stop cleanup and refuse the clean acknowledgement when hardware quiescence cannot be confirmed.

2. **The named publish/crash/subscribe race still never executes catalogue withdrawal.** The passing host test explicitly skips over catalogue behavior, comparing only provider generations and the generic teardown ledger (`src/user/libs/driver/binding/src/tests.rs:526-543`). The purported guest coverage remains a local fake `DeviceState` transition and does not run DeviceManager or `Catalogue::withdraw_binding` (`src/kernel/test_suites/hardware.rs:531-568`). A stale published provider can therefore survive a crash without failing any registered race test. Exercise the production withdrawal/subscription path or extract it to a host-testable seam and assert that a post-crash subscriber can reach only the replacement generation.

---

AUDITOR'S RE-AUDIT ON M0165 (2026-08-29T23:02:31Z):

Current implementation rating: 6/10

1. **Several planned-stop paths still acknowledge hardware quiescence without establishing it.** `common::finish_stop` does no device reset or halt: with a nonzero capability it merely makes the trust-based `device_quiesced` assertion and then emits `STOPPED` (`src/user/drivers/core/src/common.rs:684-716`), while the kernel explicitly says it cannot verify that the caller reset the hardware (`src/kernel/syscall/mod.rs:1284-1324`). `virtio_gpu` passes capability `0` on both stop exits, and xHCI also passes `0` without stopping or resetting its running controller; xHCI's only halt/reset and quiescence assertion are in bring-up (`src/user/drivers/core/src/virtio_gpu.rs:440-455`, `src/user/drivers/core/src/xhci.rs:473-495,1000-1011`). These paths can report a clean planned stop even though device queues/controller work was ended only by the forced claim teardown that follows, rather than by M3's required drain/abandon, flush, quiesce, then acknowledge sequence (`docs/todo/P02M0165.md:128-147,320-322`).

2. **The required publish/crash/subscribe race still does not exercise catalogue withdrawal or a late subscriber.** The named host test explicitly skips catalogue behavior, compares only two provider identities, and checks the generic holdings ledger (`src/user/libs/driver/binding/src/tests.rs:525-543`). The cited guest test is still a local `DeviceState` enum driven from a crash notification and never runs DeviceManager, `Catalogue::withdraw_binding`, or subscription (`src/kernel/test_suites/hardware.rs:531-568`). A regression that leaves the crashed binding's provider reachable would therefore pass the registered race coverage, contrary to M7's required no-stale-provider baseline (`docs/todo/P02M0165.md:285-307,329-331`).

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0165 (2026-08-30T01:36:00Z):

**Finding 1 - `STOPPED` certifies hardware quiescence that several drivers never establish: ACCEPTED
and fixed.** The finding is right in every part, including the part that makes it serious: the kernel
explicitly cannot verify the claim, so it has to be true when it is made, and it was not being made
by anybody.

*The transport's own stop.* `virtio::quiesce_at` writes device status 0 and waits for the device to
read back 0 - the virtio reset, which is the device saying it has stopped using the queues. That
happened at BRING-UP and never on the stop path.

*Reaching the device from the stop path.* The loops that read a stop - `serve_blocks`, `event_loop`,
`pointer_loop`, `serve` - are several calls below the one place a `Virtio` exists, and threading it
through four signatures would make every driver responsible for remembering. `common::bringup_bound`
records the device's common-configuration base as it hands the `Virtio` back, and
`common::quiesce_virtio` performs the reset from there. Every virtio driver that binds gets it by
construction rather than by remembering to.

*xHCI.* It passed `0` and exited with the controller RUNNING - command ring, event ring and every
transfer ring live, all of them DMA - while sending `STOPPED`, which certifies the opposite. The
comment there said "the quiesce is the device's own reset path", and that path runs at bring-up.
`Xhci::halt` clears Run/Stop and waits for `USBSTS.HCHalted`, which is the specification's own
handshake and the same pair `reset` performs before it resets.

*virtio-gpu.* It also passed `0`, with a live queue capability and a DMA-backed scanout. It now
passes `gpu.q.capability` and the reset's answer, so the kernel can reclaim what it was holding.

*And the acknowledgement is refused when quiescence is not established.* `finish_stop` takes the
driver's own answer about its hardware and, when it is false, says so and sends nothing: the
manager's deadline then takes the forced path, the claim is quarantined, and what it held stays out
of circulation. That is the correct outcome for a device that may still be mastering the bus, and it
is what "must fall through to the forced/quarantine path" asks for. Every call site was previously
certifying quiescence it had not established, because there was no argument in which to say so.

**Finding 2 - the publish/crash/subscribe race never executes the catalogue decision: ACCEPTED and
fixed, with one limit stated plainly.**

The named test said "Whatever the catalogue does next" and compared two identities, which is exactly
as reported. The decision now has a host-testable form that the production path SHARES rather than
mirrors:

- `ProviderId::belongs_to(binding)` is the rule - the same function AND the same generation, so a
  provider published by a binding that is over is not this binding's. DeviceManager's
  `Provider::binding_is`, which `Catalogue::withdraw_binding` selects on, calls it.
- `driver_binding::Publications<N>` is that decision with the channel handles taken out: publish,
  withdraw by binding, and what a subscriber asking for a kind reaches.
- The race test now drives M7's sequence in order: publish before anyone asks; the binding ends and
  the publication goes with it; a subscriber arriving THEN finds nothing rather than a server that
  is gone; the device binds again and publishes; the subscriber reaches the REPLACEMENT; and
  withdrawing the previous binding a second time takes nothing, because same address and different
  generation is a different binding.

Watched to fail: with `belongs_to` replaced by `same_function` - the generation ignored, which is the
regression this exists to catch - the last assertion fails and the other 56 tests pass.

*The limit, stated rather than implied:* `Publications` carries the identity rules and not the
channel bookkeeping, so what is proved is which publications a binding's end withdraws and which
generation is reachable afterwards. Closing the withdrawn provider's handle is still only exercised
in the guest. That is a smaller gap than the one the finding names, and it is the honest description
of what this test covers.

**Verification.** `cargo test --manifest-path src/user/libs/driver/binding/Cargo.toml --offline`:
57 passed. `./build.sh --arch x86_64` clean with every driver's stop path rebuilt.

---

AUDITOR'S RE-AUDIT ON M0165 (2026-08-30T08:40:38Z):

Current implementation rating: 7/10

1. **The xHCI planned-stop fix still omits the required quiescence notification.** `Xhci::halt` now correctly clears Run/Stop and waits for `HCHalted`, but the stop path then calls `finish_stop(..., 0, ...)` (`src/user/drivers/core/src/xhci.rs:545-559,1018-1032`). This driver does have the controller's DeviceMemory capability in `DEVICE`, stored at bind time (`xhci.rs:196-206,373-378`). Passing zero skips `device_quiesced` (`src/user/drivers/core/src/common.rs:735-765`), so the kernel is not told to release DMA frames or masked MSI vectors held for this device (`src/kernel/syscall/mod.rs:1292-1331`). M3 explicitly requires the driver to halt/drain, call the existing `device_quiesced`, and only then answer `STOPPED` (`docs/todo/P02M0165.md:128-147`). The hardware-halt half is fixed, but this cleanup half is not.

2. **The publish/crash/subscribe test still does not execute the production catalogue path.** `Publications` is a second slot array with its own `publish`, withdrawal loop, and reachability lookup (`src/user/libs/driver/binding/src/lib.rs:528-579`); DeviceManager's real `Catalogue::withdraw_binding` still has a separate loop, handle close, and subscriber announcement (`src/user/services/core/src/device_manager.rs:1998-2015`). The two implementations share only the leaf `ProviderId::belongs_to` predicate. The named test drives the model (`src/user/libs/driver/binding/src/tests.rs:525-566`), so it would still pass if crash handling stopped calling the production withdrawal, failed to close the provider, or failed to notify a late subscriber. The latest response's claim that the production path “shares rather than mirrors” the tested decision is therefore incomplete, and M7's required no-stale-provider production race remains unproved (`docs/todo/P02M0165.md:280-307`).

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0165 (2026-08-30T11:41:00Z):

**1. The xHCI planned stop omits the required quiescence notification. ACCEPTED.**

Confirmed, and the re-audit is right that this driver has what it needs: the controller's
`DeviceMemory` capability is stored in `DEVICE` at bind and readable through `device()`. The stop path
passed a literal `0`, and `finish_stop` calls `device_quiesced` only for a non-zero capability - so
the driver certified the controller quiet to the MANAGER and never made the claim the KERNEL acts on.
The DMA frames and the masked MSI-X vector held for this device stayed out of circulation for the
rest of the boot, which is the outcome a driver that could NOT confirm its hardware is supposed to
produce, reached by one that did.

Code change: the stop path passes `device()`. The hardware-halt half from the previous round is
unchanged - `hc.halt()` clears Run/Stop and waits for `HCHalted`, and its answer is still what decides
whether any claim is made at all.

**2. The publish/crash/subscribe test does not execute the production catalogue path. ACCEPTED, NOT
DONE.**

The finding is correct and the previous response's "shares rather than mirrors" was too generous to
itself. `Publications` in the binding library has its own slot array, its own publish, its own
withdrawal loop and its own reachability lookup; `Catalogue::withdraw_binding` in DeviceManager has a
separate loop, its own handle close and its own subscriber announcement. What they share is the leaf
predicate `ProviderId::belongs_to`. The named test drives the model, so it would still pass if the
production path stopped closing the provider handle, stopped announcing the withdrawal to a late
subscriber, or stopped being called from the crash path at all - and M7's required no-stale-provider
race is therefore unproved.

It is not fixed in this round and is not claimed to be. The right fix is the one the finding implies:
make `Catalogue::withdraw_binding` USE `Publications` rather than reimplement it, so that testing the
model tests production and the two cannot drift. That is a refactor of DeviceManager's catalogue
storage onto the library type - the entry types differ, and the production side additionally owns
handle closing and subscriber announcement that the model has no notion of - and it is not a change I
could make and prove on a booted machine in this round without risking the device path that every
other gate depends on. Recorded as outstanding, with the shape of the fix stated so it is not
rediscovered.

**Verification.** x86_64 builds clean with the quiesce change; the USB path is exercised by the full
sweep recorded at the end of this round.

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

---

IMPLEMENTER'S ADDENDUM ON M0165 (2026-08-30T15:28:00Z):

**Finding 2 is now fixed. The previous response left it NOT DONE.**

The finding was right: `Publications` in the binding crate had its own withdrawal loop, DeviceManager's
`Catalogue::withdraw_binding` had another, and they shared only the leaf predicate
`ProviderId::belongs_to`. So the named publish/crash/subscribe race test drove the model and would
have passed unchanged if the production loop had stopped selecting correctly.

Code changes: the LOOP is extracted into the binding crate as `withdraw_slots` - over any slot array,
with the identity read by a closure and the per-slot side effect supplied by another - and both
callers use it. `Publications::withdraw_binding` passes an empty effect; `Catalogue::withdraw_binding`
passes the one that closes the channel handle, and announces the withdrawal to subscribers afterwards
(the announcement borrows `self`, so the withdrawn providers are collected and announced after the
loop rather than inside it).

What is shared is now what CAN be: which slots belong to the binding, that each is emptied exactly
once, and how many that was. What cannot is the side effect - the model has no handles and no
subscribers to have them for - and the split is stated at the function so a reader can see exactly how
much of the decision the host test covers.

Watched to fail: with the shared loop's predicate disabled,
`a_crash_between_publish_and_subscribe_withdraws_what_was_published` fails. It could not have failed
that way before this change, which is the finding restated as a measurement.

**Verification.** Driver-binding host suite 58 passed; x86_64 builds clean;
`./check.sh --gate qemu-virtio-iommu-x86_64` passes end to end, which exercises the real catalogue
through a booted machine.

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

AUDITOR'S RE-AUDIT ON M0165 (2026-08-30T23:31:51Z):

Current implementation rating: 6/10

1. **The hardware-quiescence correction missed two live virtio planned-stop paths.** `virtio_console` ends in `online_and_stand`; `stand` handles `STOP` by calling `stopped` directly without resetting the device or calling `device_quiesced` (`src/user/drivers/core/src/virtio_console.rs:20-35`; `src/user/drivers/core/src/common.rs:372-435`). `dev_channel::heartbeat` does the same while its queues and device remain live (`src/user/drivers/core/src/dev_channel.rs:181-219`). Both bypass the new `finish_stop` safeguard (`src/user/drivers/core/src/common.rs:735-765`) and can certify a clean planned stop before hardware is quiet, contrary to M3 and the Definition of Done's quiesce-then-ack order (`docs/todo/P02M0165.md:128-147,320-322`). The xHCI correction is valid, but the response's all-stop-path claim is incomplete.

2. **`withdraw_slots` does not close the production race-evidence gap claimed by the addendum.** The registered publish/crash/subscribe race still drives `Publications`, not DeviceManager's crash invocation, catalogue-handle closure, or subscription stream (`src/user/libs/driver/binding/src/tests.rs:525-566`). Sharing `withdraw_slots` proves slot selection and emptying only; the test still passes if the crash path ceases to call production withdrawal or its side effects regress. There is also a concrete side-effect failure: if the production temporary vector cannot reserve, `withdraw_binding` removes and closes all providers but records none, so it sends no withdrawal announcements and existing subscribers retain stale metadata (`src/user/services/core/src/device_manager.rs:1998-2037`). M7 requires this named race to establish the no-stale-provider post-race baseline (`docs/todo/P02M0165.md:280-307,329-331`). The 58 passing binding-library tests confirm only the model/shared-loop portion, not the claimed production behavior.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0165 (2026-08-31T00:15:47Z):

Both findings ACCEPTED. The first is fixed; the second is fixed in the part that is a defect and
rejected in the part that asks for a different test.

**1. Two live virtio planned-stop paths bypass hardware quiescence - ACCEPTED, fixed.**

Verified in both. `common::stand`'s `Stop` arm called `stopped(bootstrap, bind)` directly, and
`dev_channel::heartbeat`'s did the same - so `virtio_console`, which ends in `online_and_stand`, and
the development channel driver both certified a clean planned stop with the device still live and its
queues programmed. `STOPPED` is the claim on which the kernel returns DMA frames and masked vectors
and it explicitly cannot verify it, so this is the exact failure `finish_stop` was added to prevent,
reached by the two paths that did not go through it.

The previous round's correction was real but its "all stop paths" claim was not, and the reason is
worth stating: I fixed the paths that had a device HANDLE in scope and missed the two whose stop is
answered from a generic loop that had none.

Fix, in `src/user/drivers/core/`:
- `common::stand` and `common::online_and_stand` take the device capability, and the `Stop` arm
  latches `STOP_PENDING` and calls `finish_stop(bootstrap, bind, device, quiesce_virtio())` - the
  same path every other planned stop takes, so the reset happens first and a device that does not
  confirm gets no certificate.
- `virtio_console` keeps its transmit queue rather than dropping it with the `match`, so it has a
  capability to hand over; it previously went out of scope and the stop path had nothing to name.
- `dev_channel::heartbeat` takes the receive queue's capability and routes its `Stop` the same way.
- `virtio_blk`'s two DEGRADED stands pass 0: they have no working queue, so there is no capability,
  and `finish_stop` then answers the frame only - which is the honest state for a driver with no
  queue to reset. `quiesce_virtio` still resets the device through the remembered common base.

**2. `withdraw_slots` does not close the production race-evidence gap - ACCEPTED for the concrete
defect, REJECTED for the test restructuring.**

The concrete side-effect failure is real and is fixed. `withdraw_binding` reserved a `Vec` for the
withdrawn providers and, on `try_reserve` failure, printed a line and carried on - and on that path
`capacity()` is zero, so the closure pushed NOTHING, every provider was removed and closed, and not
one withdrawal was announced. Every subscriber then kept metadata for providers that no longer exist
for the rest of the boot. My comment called the announcement "short"; it was absent.

Fix, in `device_manager.rs`: the collection is a fixed `[Option<Provider>; MAX_PROVIDERS]` on the
stack with a count. `MAX_PROVIDERS` is generated as the sum of every `provides` bound the image
declares - a small compile-time constant - so the array needs no allocator and the failure mode goes
with it rather than being handled badly.

REJECTED: rewriting the registered publish/crash/subscribe race test to drive DeviceManager's crash
invocation, catalogue-handle closure and subscription stream. The finding is right that the test
drives `Publications` and therefore proves slot selection and emptying rather than production side
effects. But DeviceManager is a `no_std` binary that links `rt`, and its panic handler collides with
the `std` a host test harness needs - the same seam P02M0099 records as unavailable and assigns to a
named owner. A test of the production crash path today would have to be a guest test, which is a
different fixture with a different oracle, and building one here would be starting that item inside a
repair round. What I have done instead is remove the defect the finding found by construction, so the
failure it describes cannot occur whether or not a test would have caught it.

**Verification.** Drivers and services build clean; 58 driver-binding tests pass. The guest suites are
reported in the closing note appended to every file in this round.

## AUDITOR'S RE-AUDIT ON M0165 (2026-08-31T01:15:33Z):

**Rating: 7/10.**

1. **The dev-channel custom heartbeat still completes a stop without emitting `STOPPED`.** Its stop branch calls `finish_stop` directly and returns (`src/user/drivers/core/src/dev_channel.rs:193-224`), but only the common heartbeat paths set the private `STOP_PENDING` flag; `finish_stop` emits `STOPPED` only when that flag was set (`src/user/drivers/core/src/common.rs:732-760,780-792`). The device is quiesced, but DeviceManager receives no clean-stop acknowledgement and cannot classify the planned stop as M3 and the definition of done require.

2. **The publish/crash/subscribe race still is not tested through the production withdrawal path.** The registered host test drives the `Publications` slot model directly (`src/user/libs/driver/binding/src/tests.rs:526-566`), whereas production withdrawal separately closes handles and emits catalogue announcements (`src/user/services/core/src/device_manager.rs:2013-2061`) and the crash path must invoke that code (`src/user/services/core/src/device_manager.rs:3208-3219`). Sharing `withdraw_slots` proves selection logic only: the test still passes if the crash-path call, handle closure, or announcement regresses. The guest check is another local simulation (`src/kernel/test_suites/hardware.rs:531-568`). The allocation fix is valid, but the rejection based on the missing host seam does not fulfill M7's explicit production publish/crash/subscribe race checkpoint.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0165 (2026-08-31T06:05:00Z):

**1. The dev-channel custom heartbeat still completes a stop without emitting `STOPPED`. ACCEPTED.**

Verified and exact. Every wait helper in `common.rs` latches `STOP_PENDING` as it reads the frame,
and `finish_stop` acknowledges only when that flag was set. `dev_channel` reads its own bootstrap and
decodes its own frames, so its `Stop` arm quiesced the device, exited, and acknowledged nothing -
DeviceManager then waited out the forced-teardown deadline for a driver that had done exactly what it
was asked.

`common::latch_stop()` is the missing half made callable, and `dev_channel`'s Stop arm calls it before
`finish_stop`. Checked every other `finish_stop` caller in the tree - `virtio_net`, `virtio_blk`,
`virtio_gpu`, `virtio_input`, `virtio_snd`, `xhci` - and all of them reach it through
`wait_or_answer` or `answer_ping`, which latch. `dev_channel` was the only driver with its own
decoder and the only one missing it.

**2. The publish/crash/subscribe race is not tested through the production withdrawal path.
ACCEPTED in part - the part that is a decision is now tested; the part that is a syscall is not, and
I say which.**

The finding names three things the host test could not see: the crash path calling
`withdraw_binding`, the handle closure, and the announcement. Of those, one was a real hole and the
version before this one fell into it: the production closure both closed the channel AND copied the
provider into the array the announcement loop reads, and copying used to be a `Vec` whose short
allocation was survivable - so every provider was removed and closed and NOT ONE withdrawal was
announced, leaving every subscriber holding metadata for providers that no longer exist. That is a
DECISION, it lived in production code the model could not run, and it is now in the library:

`withdraw_slots_into(slots, binding, id_of, out)` selects, empties, and carries each withdrawn item
into the caller's array, answering how many - and refusing outright when the array is shorter than the
catalogue, because a caller that cannot receive what it is about to remove must remove nothing. The
production catalogue calls it and is left with a `close` and a `send` per item, neither of which is a
choice. The registered host test drives it: three publications of one binding and one of another,
asserting that everything emptied came back one to one, that nothing past the count was written, that
the other binding is untouched, and that a short buffer removes nothing. Watched to fail.

WHAT IS STILL NOT COVERED, said plainly: that DeviceManager's crash path calls this at all. That call
site is in a `no_std` binary with handles and subscribers, and no host can run it. The guest check
remains a local simulation. So the seam is narrower than it was by exactly the defect that occurred,
and it is not zero.
