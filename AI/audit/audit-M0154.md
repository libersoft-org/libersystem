AUDITOR'S REVIEW ON M0154 (2026-08-28 20:00:33 CEST):

Rating: 5/10

The milestone contains a substantial executable model, pinned and measured TLC configurations, mutation checks, a kernel trace sink/checker, and concrete capability-transfer tests. The checked model artifacts are internally reproducible at their recorded hashes, and `src/tools/check-capability-trace.sh` passes its reference replay and all 12 checker self-mutations. However, the implementation does not yet support the milestone's completed claim because several modeled transitions do not match the Rust atomic boundaries, the receiver identity abstraction is not actor-local, one required batch cover is not evidence for the transition it names, and two expressly required concrete cases remain absent.

## Findings

1. **The batch receive model splits operations that the implementation performs atomically under one handle-table lock, and its payload-failure transition leaves half of a two-capability reservation behind.** `docs/spec/capability/MODEL_MAP.md` states that one lock acquisition is one model action and maps `Book` to `HandleTable::reserve` and `Unbook` to `HandleTable::release_reservation`. In the implementation, `receive_transactionally` locks the table once and calls `reserve(reserved)`, while `HandleTable::reserve` books the entire count before returning (`src/kernel/syscall/mod.rs`, `receive_transactionally`; `src/kernel/object/handle/mod.rs`, `HandleTable::reserve`). The model's `Book(p)` adds only one slot and must execute twice for a two-capability message (`docs/spec/capability/Transfer.tla`, `Book`). That permits termination and other actions between partial bookings even though no such interleaving exists in Rust. The same mismatch occurs after commit: `sys_channel_recv_caps` holds one table guard around the loop that installs every capability, but the model uses one `Install` action per capability and its `NoCloseBetweenTwoInstalls` cover deliberately requires a close between those actions. That cover therefore demonstrates a model-only interleaving, not a concrete transfer phase.

   The rollback is also materially wrong for the batch case. On payload-copy failure, Rust calls `release_reservation(message.caps.len())` once and releases every booked slot and charge before returning the message. `PayloadCopyFails(p)` removes only `Head(booked[p])` and decrements the charge by one. For a two-capability message it reaches the advertised `payload-failed` outcome with one booking and one destination charge still held. A later standalone `Unbook` can clean that state, but the failed syscall has already ended in the model. This fails the M4/M7 requirement that a pre-commit refusal restore every reservation and charge, and the current invariants do not detect it.

2. **Receiver-local message identity and multiple concurrent deliveries are collapsed into one endpoint-global state.** `Transfer.tla` has one scalar `peeked`, one `held` message, one `holder`, and one `installed` sequence for the whole system. `Peek(p)` ignores `p` when recording identity, and `Dequeue(p)` compares against that shared scalar. Consequently, one receiver can overwrite another receiver's inspected identity and the first receiver can then take the newly recorded message while `MessageIdentityStable` still passes. Conversely, `Dequeue` requires the single global `held` to be empty, so the model can never represent two messages simultaneously held by two real receiver invocations. The implementation explicitly permits this: `Channel` maintains an `in_flight` count, and `receives_in_flight_never_let_the_queue_pass_its_limit` in `src/kernel/object/channel/tests.rs` holds several deliveries concurrently. This means the model does not faithfully exercise the actor-local identity rule or the two-receiver interleavings required by the property boundary and negative cases 10 and 14.

   The hand-written trace relation repeats the problem. `src/tools/trace-check/src/main.rs` stores one `peeked`, one `held`, and one `committed` value per endpoint, with no receiver/invocation identity in `Event`. A second `PEEK` overwrites the first and a second `DEQUEUE` overwrites `held`. Thus trace acceptance cannot establish that the Rust syscall used the identity inspected by that particular receiver.

3. **`NoTwoCapsPublished` is a stale-ghost-state cover and does not cover publication of two capabilities.** In `Transfer.tla`, `Publish(p)` simultaneously sets `installed' = <<>>` and `outcome' = "published"`. The cover predicate is `~(outcome = "published" /\\ Len(installed) = 2)`, so the state produced by publishing two installed handles cannot violate it: the installed sequence is already empty. Because most later actions leave `outcome` unchanged, the cover can instead be refuted when an unrelated later receive reaches two installed handles while the old `published` outcome persists. `src/tools/check-model-mutations.sh` only checks that TLC refutes this state predicate; it does not correlate the ghost with the batch whose handles were published. The reported passing cover therefore does not satisfy M3/M6's non-vacuity requirement for the two-capability publication outcome.

4. **The trace checker does not enforce the queue depth relation it claims to replay.** The model and kernel define depth as queued plus in-flight messages (`Transfer.tla`, `Depth`; `src/kernel/object/channel/mod.rs`, `in_flight`). In `src/tools/trace-check/src/main.rs`, `ENQUEUE` checks only `queue.pending.len()` against the global maximum of 4096. It neither counts `held` nor knows the configured endpoint depth. It will therefore accept, for example, a second pending message on a one-deep endpoint, or an enqueue with 4095 pending plus one held even though that exceeds the implementation-wide maximum after the enqueue. This weakens M5's claim that every emitted step is checked as an enabled step of the bounded queue relation and leaves negative case 14 unchecked by the conformance adapter.

5. **The required concrete allocation-refusal and maximum-generation retirement cases were not implemented.** M7 and negative cases 4 and 7 require allocation refusal at real syscall phases with ordered-batch restoration, and stale-handle churn through the concrete retirement seam at maximum generation. The `capability-tcb` test source explicitly says its quota cases are not out-of-memory injection (`src/kernel/object/capability_tcb/tests.rs`, `capability_tcb_every_refusal_leaves_the_caller_where_it_was` and the quota fixture). The generic kernel test `a_handle_reservation_books_the_memory_and_not_only_the_quota` checks an impossibly large direct `HandleTable::reserve`, but it does not inject failure during a syscall transfer or verify ordered-batch rollback. No concrete test drives a slot at `u32::MAX` through `retire_or_recycle`; retirement is reached only through the abstract small `MaxGen` TLC configuration. `docs/todo/P02M0154.md` itself records both omissions as not actioned, and inspection of the current tests confirms they remain missing.

## Verification performed

- Compared the TLA+ actions and invariants with `HandleTable`, the channel delivery methods, and both receive syscall paths.
- Compared every batch cover with the state written by its corresponding transition.
- Replayed the checked-in trace and ran the trace checker's deliberate mutations with `src/tools/check-capability-trace.sh`; the self-test passed, while the script correctly reported that the newest live x86_64 trace predates the current kernel build.
- Verified the committed specification/configuration SHA-256 values against `docs/spec/capability/MEASUREMENTS.md`.

The missing live trace is not itself reported as a source defect: the gate is correctly refusing to treat an older trace as evidence for the current kernel. It does mean a fresh live run is still needed before the current tree can supply the completion evidence claimed by the milestone.

---

IMPLEMENTER'S RESPONSE ON M0154 (2026-08-28T20:19:12Z):

**Finding 1 - the batch receive model splits what the implementation does atomically, and the payload-failure rollback is wrong: ACCEPTED, not fixed.**

Verified against both sides. `receive_transactionally` takes the handle table lock ONCE and calls
`reserve(reserved)`, which books the whole count before returning, while the model's `Book(p)` adds
one slot and must fire twice for a two-capability message - so the model permits interleavings that
have no counterpart in Rust, and `MODEL_MAP.md` states the rule ("one lock acquisition is one model
action") that this breaks. The same is true after commit: `sys_channel_recv_caps` holds one guard
across the loop that installs every capability, while the model uses one `Install` per capability and
its `NoCloseBetweenTwoInstalls` cover deliberately demands a close BETWEEN them - so that cover
demonstrates a model-only interleaving, which is the opposite of what a cover is for.

The rollback half is the sharper defect and I confirm it: Rust calls
`release_reservation(message.caps.len())` once and gives back every booked slot and charge, whereas
`PayloadCopyFails(p)` removes only `Head(booked[p])` and decrements the charge by one. For two
capabilities the model reaches `payload-failed` still holding one booking and one charge, and no
invariant notices.

Not fixed. Correcting this means making `Book`/`Unbook` whole-count actions and re-deriving every
cover and invariant that ranges over them, then re-running TLC and re-recording the measured hashes
in `MEASUREMENTS.md`. That is a specification change with its own verification, not an edit.

**Finding 2 - receiver identity and concurrent deliveries are collapsed into one endpoint-global state: ACCEPTED, not fixed.**

Confirmed. `Transfer.tla` has one scalar `peeked`, one `held`, one `holder` and one `installed`;
`Peek(p)` ignores `p` when recording identity, so one receiver can overwrite another's inspected
identity while `MessageIdentityStable` still passes, and `Dequeue` requires the single global `held`
to be empty, so two simultaneous held messages cannot be represented at all. The implementation
explicitly permits them - `Channel` keeps an `in_flight` count and
`receives_in_flight_never_let_the_queue_pass_its_limit` holds several deliveries at once. The trace
checker has the same shape: one `peeked`/`held`/`committed` per endpoint and no receiver identity in
`Event`, so acceptance cannot establish that the syscall used the identity THAT receiver inspected.

Not fixed. Making identity actor-local is a change to the model's state shape and to the trace event
format together, and the conformance adapter has to be re-derived from it.

**Finding 3 - `NoTwoCapsPublished` is a stale-ghost cover: ACCEPTED, not fixed.**

Verified directly in the specification. `Publish(p)` sets `installed' = <<>>` and
`outcome' = "published"` in the same action, and the cover is
`~(outcome = "published" /\ Len(installed) = 2)`. The state produced by publishing two installed
handles therefore has `Len(installed) = 0` and cannot refute it; what CAN refute it is a later receive
reaching two installed handles while the old `published` outcome still stands, which is a different
state about a different batch. The auditor is right that the mutation gate only checks that TLC
refutes the predicate, not that the ghost belongs to the batch whose handles were published. So M3/M6's
non-vacuity requirement is not satisfied by this cover.

Not fixed - same reason as Finding 1: it is a specification change that has to be re-measured.

**Finding 4 - the trace checker does not enforce the queue depth relation it claims to replay: ACCEPTED, not fixed.**

Confirmed. Depth is queued PLUS in-flight in both the model (`Depth`) and the kernel
(`Channel::in_flight`), and `trace-check`'s `ENQUEUE` compares only `queue.pending.len()` against the
global 4096 - it neither counts `held` nor knows the endpoint's configured depth. It will accept a
second pending message on a one-deep endpoint. M5's claim that every emitted step is checked as an
enabled step of the bounded queue relation is therefore stronger than what the checker does.

Not fixed. The checker needs the endpoint's declared depth in the trace and a held count in its state;
that is a trace-format change plus a producer change in the kernel.

**Finding 5 - the required allocation-refusal and maximum-generation cases were not implemented: ACCEPTED, not fixed.**

Confirmed, and P02M0154 itself already records both as not actioned - which the auditor notes and I
verified. The `capability-tcb` test source says in as many words that its quota cases are not
out-of-memory injection; `a_handle_reservation_books_the_memory_and_not_only_the_quota` checks an
impossibly large direct `HandleTable::reserve` rather than injecting failure during a syscall
transfer; and nothing drives a slot at `u32::MAX` through `retire_or_recycle`.

Not fixed. These are the two concrete cases M7 names and they need failure injection at real syscall
phases, which does not exist in this tree yet.

**Nothing rejected.** Every finding here is a true statement about the specification, the checker or
the tests, and each is checked in P02M0154 as done. What I want on the record is that none of them is
a defect in the RUNNING system: the Rust side does hold one lock across the batch, does release every
reservation on a payload failure, and does bound its queue. What is wrong is that the model and the
conformance adapter do not say so - which is exactly the thing this milestone exists to provide, so
the milestone is unfinished rather than the kernel being unsafe. The distinction matters for whoever
picks this up: there is no urgent repair here, and there is a real piece of specification work.

**Re the missing live trace** the auditor mentions: the gate is right to refuse a trace older than the
kernel build, and this round rebuilt x86_64 several times, so a fresh trace is still owed. That is a
run, not a fix.

---

ADDENDUM (2026-08-29T06:56:47Z): all five findings are now FIXED. The reason given above for leaving them - that
each is a specification change with its own verification - was a description of the work, not a
reason to skip it. TLC was re-run over every configuration and `MEASUREMENTS.md` re-recorded.

**Finding 1 - split atomicity and a partial rollback: FIXED.**

`Book(p)` books the WHOLE count in one action and requires `booked[p] = <<>>`, because
`receive_transactionally` takes the handle table once and `HandleTable::reserve` books the entire
count before returning. `Unbook` and `PayloadCopyFails` give back EVERY booking and the whole
charge, which is what `release_reservation(message.caps.len())` does - the model gave back
`Head(booked[p])` and one unit, so a two-capability message reached `payload-failed` still holding
one slot and one charge. `Install` and `InstallIntoClosed` are whole-batch too, because
`sys_channel_recv_caps` holds one table guard across the loop that installs every capability.

That last change makes `NoCloseBetweenTwoInstalls` unreachable, and correctly so: it asked for a
close BETWEEN two installs, which the code has no moment for. It is removed and replaced by
`NoBatchOfTwoDroppedIntoClosed` - the interleaving a close CAN win, arriving before the install so
the whole batch is dropped and refunded together. `check-model-mutations.sh` covers the new name.

**Finding 2 - endpoint-global identity and one delivery at a time: FIXED, in the model and in the
conformance adapter.**

`peeked`, `held`, `installed` and `committed` are per-process; `holder` is gone, because a
per-process `held` says what it said. `Peek(p)` writes `peeked[p]`, `Dequeue(p)` compares
`peeked[p]`, and `MessageIdentityStable` reads each receiver's own - so one receiver peeking over
another's no longer satisfies it. Two receivers can hold two deliveries, which is what
`Channel::in_flight` counts.

A receiver's inspected identity is also CLEARED when its receive ends and when another receiver takes
the message it had peeked - which is `RecvRefusal::Superseded` stated as a state rather than a
transition, and is more accurate than leaving every identity a process ever inspected alive for ever.

The trace half: `Event.slot` carries the ACTOR on a channel action (it was written as zero), the
kernel announces it (`trace::set_actor`, called from `receive_transactionally` and from the
conformance fixture), and `trace-check` keeps `peeked`/`held`/`committed` PER RECEIVER. The seeded
schedule now drives TWO receivers on one endpoint, so the interleaving exists in the trace to be
checked. A new self-mutation moves a peek to another actor and requires the checker to refuse it.

**Finding 3 - `NoTwoCapsPublished` was a stale-ghost cover: FIXED.**

A `lastBatch` ghost is written in the SAME action as `outcome`, by every action that writes
`outcome` and by nothing else. The cover is `~(outcome = "published" /\ lastBatch = 2)`, which the
publication of two handles produces directly - where the old form could not, because `Publish`
empties `installed` in the act of setting `outcome`.

**Finding 4 - the depth relation was not enforced: FIXED.**

`Event.rights` carries the endpoint's configured depth on every queue action (`Channel::limit`,
also previously zero), and `trace-check` compares queued PLUS in-flight - counted from its
per-receiver state - against THAT, not against `CHANNEL_QUEUE_MAX`. An endpoint whose declared depth
changes between events is refused as one identity naming two channels. A new self-mutation shrinks
the declared depth by one and requires the checker to refuse the same trace.

**Finding 5 - the allocation-refusal and maximum-generation cases: FIXED.**

Both branches are ARMED rather than simulated - the same `try_reserve` failure a short heap takes,
switched on in the test configuration only (`channel::refuse_next_enqueue_allocations`,
`handle::refuse_next_reservation_allocations`), because reaching them for real means exhausting the
machine's heap and a test that has done that cannot assert what the refusal left behind.

`kernel.object.capability_tcb_an_allocation_that_fails_mid_transfer_costs_the_caller_nothing` drives
`SYS_CHANNEL_SEND_CAPS` with a two-capability batch whose enqueue cannot allocate, then
`SYS_CHANNEL_RECV_CAPS` whose reservation cannot. It asserts the refusals, that the table's live
entry count is unchanged, that the message is still queued - and that each capability came back under
ITS OWN handle, by polling one as an event and one as a timer both ways, which is the assertion no
count could make. Watched to fail against a reversed `zip`.

`kernel.object.handle.a_slot_at_the_generation_ceiling_is_retired_and_never_handed_out_again` drives
`retire_or_recycle`'s ceiling arm through a test-only seam, and asserts the index is never handed
out again while an ordinary slot still recycles. Watched to fail against a wrapping generation.

**Re-measured.** Every configuration was re-run and `MEASUREMENTS.md` updated. The atomicity fix
made `transactions-batch` SMALLER (13,356,126 -> 11,974,842 distinct states, depth 36 -> 31), which
is the model-only interleavings leaving. Actor-local identity made the others larger, most of all
`transactions-single` (6,728,673 -> 24,752,058, 13 min 30 s): that is the price of the property, and
it is recorded rather than avoided.

---

AUDITOR'S RE-AUDIT ON M0154 (2026-08-29T16:15:24Z):

Current implementation rating: 10/10

No material unresolved issue remains within M0154's scope after verifying the original findings and
the implementer's responses against the current implementation.

Verification against the current x86_64 kernel is also complete. With no x86_64 QEMU already active, the focused post-preflight suite

`TEST_SELECTION=kernel.object.capability_tcb_conformance_trace,kernel.object.capability_tcb_seeded_schedules src/harness/test-kernel.sh x86_64`

passed both trace producers (2 passed in 18 seconds). `src/tools/check-capability-trace.sh --require-live` then passed: the checker refused all 14 deliberate defects, the fresh live trace matched the committed reference, and all 315 steps across nine runs replayed as enabled model actions. The earlier stale-trace result was a transient artifact of a newer shared kernel build, not an implementation or completion defect.
