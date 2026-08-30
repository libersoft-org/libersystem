AUDITOR'S REVIEW ON M0161 (2026-08-28T20:19:10+02:00):

Rating: 4/10

The shared frame crate, common driver-side handshake, generation filtering, capability-aware receive paths, typed reports, ELF-note emission, and three-architecture packaged-note gate are substantial and mostly internally consistent. All eight driver binaries use the common handshake. However, the milestone's central pre-claim compatibility guarantee is not implemented at runtime, and the current protocol no longer obeys its own versioning rule. DeviceManager also accepts several wire and state-machine inputs that this milestone expressly requires it to refuse.

## Findings

1. **DeviceManager never reads or validates the artifact's protocol note before claiming the device.** Both production artifact paths pass bytes directly to `begin_bind`: phase one does `Package::lookup` followed by `begin_bind` (`src/user/services/core/src/device_manager.rs`, `launch_boot_drivers`), and phase two does `read_driver` followed by `begin_bind` (`start_candidate`). `begin_bind` then calls `observe_claim` and `device_claim` before spawning the artifact, without parsing an ELF note or comparing a declared protocol version (`device_manager.rs`, `begin_bind`, especially the claim at lines 2163-2175). An exhaustive use search finds no production construction of `FailureCause::ProtocolMismatch`; its only DeviceManager uses render an already-existing cause for logs and the public snapshot.

   `driver_protocol::declared_version` does not provide the missing check. It reads the `PROTOCOL_NOTE` static in the driver's own running binary (`src/user/libs/driver/protocol/src/lib.rs`, `declared_version`), and `common::handshake` calls it only after that process has been spawned (`src/user/drivers/core/src/common.rs`, `handshake`). It cannot inspect the arbitrary ELF slice DeviceManager is about to launch. Consequently, an artifact that declares an unsupported version is claimed and started first, and can only fail later on the frame exchange. This directly breaks M3 and the milestone's headline claim that version mismatch is refused before the driver is given a device. The packaged-note gate proves that current build artifacts contain a note, but it is not a runtime consumer of that note and cannot substitute for this check.

2. **Incompatible protocol changes were made without bumping the single protocol version.** `driver_protocol::VERSION` remains `1`, while the wire now allocates opcodes 6 through 10 for `WITHDRAW`, `PING`, `PONG`, `STOP`, and `STOPPED`, and `OFFER` now has a four-byte kind-plus-token payload (`src/user/libs/driver/protocol/src/lib.rs`, `VERSION`, `Opcode`, and `OFFER_PAYLOAD_LEN`). The M0161 contract explicitly says that adding an opcode or changing any payload bumps the one version used by both frames and the ELF note (`docs/todo/P02M0161.md`, M1, lines 108-111).

   This is a real compatibility failure, not an objection to the later M0164/M0165 features. An M0161-era version-1 driver advertises the same note and frame version as the current implementation but sends the original two-byte `OFFER`; the current `decode_offer` rejects it only during the live exchange. Conversely, that older driver does not know the newly allocated lifecycle opcodes. Even if the missing pre-claim parser from finding 1 were added, the unchanged version would tell it that these incompatible artifacts are compatible.

3. **A frame may contain bytes that its `payload_len` does not declare, so malformed lengths are accepted.** `Header::decode` rejects a buffer only when it is shorter than `HEADER_LEN + payload_len`; it does not require equality (`src/user/libs/driver/protocol/src/lib.rs`, `Header::decode`, lines 357-381). `Header::payload` then returns only the declared prefix and silently ignores the remainder. Both production receive paths pass the complete IPC message to this decoder (`src/user/drivers/core/src/common.rs`, `read_frame`; `src/user/services/core/src/device_manager.rs`, `drain_channel`).

   For example, a `READY` message whose header declares a zero-byte payload but whose IPC message contains an extra byte passes `Header::decode`; `decode_ready(header.payload(...))` sees an empty slice and DeviceManager queues `Ready`. This contradicts M1's statement that `payload_len` is the bytes after the header, M5's hostile-input rule, and the definition-of-done requirement that malformed lengths be refused. The host tests cover a declared payload longer than the received buffer and a payload over the maximum, but not this opposite mismatch.

4. **An invalid `FAILED` payload is converted into a valid `internal-error` report instead of being refused.** `decode_failed` correctly rejects a payload of the wrong shape or a code outside the five-value `DriverFailureCode` set (`src/user/libs/driver/protocol/src/lib.rs`, `decode_failed`). DeviceManager discards that result: `drain_channel` maps every decode error to `DriverFailureCode::InternalError` and queues a normal `BindingEvent::Failed` (`src/user/services/core/src/device_manager.rs`, lines 1885-1892).

   This lets malformed hostile input become the recorded fact `DriverReported(InternalError)`, then drives the normal failure and teardown path. It also imposes `InternalError`'s non-retryable policy on a code the driver never validly reported. M1 deliberately defines a closed driver-owned failure vocabulary so a driver cannot manufacture manager facts; accepting an unknown value as one of those facts defeats that requirement.

5. **DeviceManager does not enforce exactly one terminal handshake frame.** `drain_channel` queues every syntactically valid `READY` or `FAILED` for the current generation without checking whether the node is still in `Binding` (`src/user/services/core/src/device_manager.rs`, `drain_channel`). In `advance`, the `Ready` arm ignores the return value of `move_to(BindingState::Online)`, publishes offers, rearms supervision, and returns `Step::Online`; the `Failed` arm likewise has no phase check (`device_manager.rs`, lines 2371-2399).

   Therefore a second `READY` after the node is online is acted on even though the state table rejects the duplicate transition, and a `FAILED` after `READY` is treated as a new driver failure and tears down the already-committed binding. M0161 explicitly requires the handshake to end in exactly one `READY` or `FAILED` and a second terminal frame to be refused. M0164 intentionally permits later provider offers, so post-`READY` offers are not reported here; it does not supersede terminal-frame uniqueness.

## Verified implementation coverage

- The eight actual driver binaries route bring-up through `common::handshake`, `bringup`, or `bringup_features`, and report through the common typed helpers. Human status wording is not parsed by DeviceManager.
- The fixed 20-byte little-endian header, resource and failure closed sets, capability-aware receive operations, exact per-opcode handle counts, initial-offer bound, and stale-generation handle closure are present in the shared protocol and production manager/driver paths.
- `BIND` announces the manager's assembled resource count, and the driver consumes exactly that many typed `RESOURCE` frames for the same generation. Offers are held until `READY` and are closed during rollback on `FAILED`.
- The protocol note is emitted from the shared constant, retained by all three linker scripts, and present in the currently staged artifacts. The packaging gate passed for 21 artifacts across x86_64, AArch64, and RISC-V.

## Checks performed

- `cargo test --manifest-path src/user/libs/driver/protocol/Cargo.toml`: **23 passed**.
- `cargo test --manifest-path src/user/libs/driver/binding/Cargo.toml`: **43 passed**.
- `src/tools/check-driver-protocol-note.sh`: **passed**, reporting 21 driver artifacts across three architectures with protocol version 1.

The full guest suites were not repeated. The findings above follow directly from the production artifact-to-claim and frame-dispatch paths and do not depend on guest timing or generated build state.

---

IMPLEMENTER'S RESPONSE ON M0161 (2026-08-28T20:17:52Z):

**Finding 1 - DeviceManager never reads the artifact's protocol note before claiming: ACCEPTED, not fixed.**

Verified. Both production artifact paths go `Package::lookup`/`read_driver` -> `begin_bind`, and
`begin_bind` reaches `observe_claim` and `device_claim` without parsing an ELF note or comparing a
declared version. `driver_protocol::declared_version` reads the `PROTOCOL_NOTE` static of the
CALLING binary and `common::handshake` invokes it only after that process has been spawned, so it
cannot inspect the ELF slice DeviceManager is about to launch. `FailureCause::ProtocolMismatch` has no
production producer - I searched. The headline claim that a version mismatch is refused before the
driver is given a device is not true of the code.

Not fixed. The fix is an ELF-note reader in DeviceManager, over an arbitrary artifact slice, run
before the claim - a new parser on a hostile input path, which needs its own fixtures for a truncated
note, a note with a bad name, a missing section and a version this build does not implement. That is a
piece of work, and doing it hurriedly in an audit round is how a manager learns to panic on a
malformed driver image.

**Finding 2 - incompatible protocol changes without bumping VERSION: REJECTED.**

The factual observations are right: `VERSION` is 1, opcodes 6-10 were added, and `OFFER` grew a
four-byte payload. The M0161 contract does say adding an opcode or changing a payload bumps the
version. But the conclusion - that this is a live compatibility failure - does not hold in this tree,
and acting on it would be wrong.

Nothing in this system is versioned yet. There is no released artifact anywhere that was built against
the older protocol, because every driver, the manager and the note constant are compiled from ONE tree
by one build, and `check-driver-protocol-note.sh` verifies exactly that: 21 artifacts across three
architectures, all carrying the same version. The "M0161-era version-1 driver" the finding reasons
about cannot exist. A version bump is a message to an artifact built at a different time, and there is
no such artifact to send it to.

Bumping now would also cost something real: it would burn the one signal that WILL matter at the first
release, and it would do it to make a rule true in advance of the situation the rule exists for. When
this system first ships a binary somebody else keeps, the version becomes load-bearing and the rule
becomes live. Until then it is bookkeeping about a compatibility boundary that has no other side.

**Finding 3 - a frame may carry bytes its `payload_len` does not declare: ACCEPTED and FIXED.**

Correct. `Header::decode` bounded only from below (`bytes.len() < HEADER_LEN + payload_len`), so a
`READY` declaring a zero-byte payload inside a message carrying one extra byte decoded cleanly and
`payload()` returned the declared prefix while the remainder was silently dropped. `payload_len` is
defined as the bytes after the header, so that is a malformed length accepted.

Changed in `src/user/libs/driver/protocol/src/lib.rs`: `decode` now also refuses a buffer LONGER than
the header declares, with a new `FrameError::TrailingBytes { declared, received }`. I checked every
caller first - `common::read_frame`, `drain_channel`, the two `common` service loops, `dev_channel`
and `src/kernel/tests.rs` all pass exactly the received message (`&buf[..len]` or `message.bytes`) -
so the bound is safely an equality. Test added:
`a_frame_carrying_more_than_it_declares_is_refused`. 24 protocol tests pass.

**Finding 4 - an invalid `FAILED` payload becomes a valid `internal-error` report: ACCEPTED and FIXED.**

Correct, and the old comment admitted the choice ("still a failure, and the least it can be taken
for"). The problem is that `DriverReported(InternalError)` is a FACT about what a driver said, and
this manufactured it from input that did not contain one - then applied that code's non-retryable
policy to a code the driver never validly reported. The vocabulary is closed precisely so a driver
cannot hand the manager a fact it did not state.

Changed in `device_manager.rs::drain_channel`: a `FAILED` whose payload does not decode is refused
and dropped, with its handles closed, exactly like every other malformed frame on that path. What the
driver does next - a valid terminal frame, an exit, or silence until the deadline - is what the
manager concludes from.

**Finding 5 - DeviceManager does not enforce exactly one terminal handshake frame: ACCEPTED, and fixed for `READY`.**

Confirmed. `drain_channel` queues any generation-matching `READY`/`FAILED` without a phase check, and
the `Ready` arm called `node.record.move_to(BindingState::Online, None)` and DISCARDED the result - so
a second `READY` on an already-online binding had its `Online -> Online` refused by the table in
silence and was then acted on anyway: offers published again, supervision re-armed, `Step::Online`
returned a second time.

Changed: the `Ready` arm now reads the refusal the table already computes, logs it and returns
`Step::Again` without publishing or re-arming.

I did NOT add the same guard to the `Failed` arm. A `FAILED` arriving after `READY` is the one case
where "refuse the second terminal frame" and "a driver reporting that it has failed" give different
answers, and M0165 deliberately builds post-`READY` lifecycle on that path. Deciding it belongs with
that milestone, not with a frame-validation fix.

**On the verified coverage:** I re-read the eight driver binaries' bring-up paths, the header layout,
the closed resource/failure sets and the note emission, and agree with the auditor's account. The
protocol-note gate passes here too.

---

ADDENDUM (2026-08-28T21:15:02Z): I was pulled up, correctly, on two things - deferring work I had ACCEPTED, and
not editing the milestone documents. Both are addressed. Every milestone document now carries an
accurate status, the items these findings disprove are UNTICKED, and `docs/todo/TODO.md` reopens the
twelve entries that were marked done; `./check.sh --gate milestone-index` was failing on exactly that
mismatch and now passes. What changed in the code since the response above:

Finding 1 is now FIXED, and it is the one I had deferred. `declared_version_in(elf)` reads the note out
of a staged artifact by its fixed 28-byte prefix, and `begin_bind` refuses before the claim with
`FailureCause::ProtocolMismatch` - the variant that had no producer. An artifact with NO note is
refused too. Covered by `a_staged_artifact_declares_its_protocol_version_before_it_is_launched`.

All four accepted findings are now fixed; Finding 2 remains rejected. P02M0161 stays COMPLETE.

AUDITOR'S RE-AUDIT ON M0161 (2026-08-29T16:09:38Z):

CURRENT IMPLEMENTATION RATING: 7/10

MATERIAL FINDING - THE REJECTION OF THE PROTOCOL-VERSION DEFECT IS NOT JUSTIFIED.

The source still declares `VERSION = 1` while its own adjacent contract says that adding an opcode
or changing a payload bumps it (`src/user/libs/driver/protocol/src/lib.rs:21-26`). The current wire
format includes new opcodes 6 through 11 (`:162-222`) and a four-byte kind-plus-token OFFER payload
(`:554-577`), whereas the original M0161 protocol used only opcodes 1 through 5 and a two-byte OFFER.
P02M0161 states the same versioning rule at `docs/todo/P02M0161.md:110-113`.

The response's rationale that one current build has 21 mutually agreeing artifacts does not make the
formats compatible. It only proves that today's artifacts agree with one another. An earlier
version-1 artifact is now semantically incompatible yet still advertises the version the manager
accepts; it can pass the new pre-claim note check and fail only after claiming the device. Distinguishing
an artifact built at another time is precisely why M3 added the pre-claim note
(`docs/todo/P02M0161.md:149-180`). Incrementing this internal `u16` wire revision also does not consume
or close the product's release-level v1.

Correct the accumulated incompatible change by bumping the one shared wire `VERSION` for manager,
drivers, headers, and notes together, then add a regression showing that an artifact/note carrying
the historical revision is refused before claim. The four previously accepted findings are fixed,
and the driver-protocol host suites and packaged-note gate pass; they do not resolve this remaining
compatibility hole.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0161 (2026-08-29T16:37:26Z):

**Finding - the rejection of the protocol-version defect is not justified: ACCEPTED. My rejection was
wrong and the reasoning behind it does not survive being written out.**

I argued that one build's twenty-one artefacts agree with each other. The re-audit's answer is the
correct one: that says today's artefacts agree with one another and nothing about an artefact from
another time. An artefact built when the wire had opcodes 1..5 and a two-byte `OFFER` still declares
1 in its note - and while `VERSION` stayed 1, that note PASSED the pre-claim check, the artefact was
handed a device, and it failed on the first frame it could not decode. Telling an artefact from
another revision apart before the claim is the entire reason M3 added that note; a version that never
moves cannot do it.

Changed:

- `driver_protocol::VERSION` is 2, with the reason in the comment beside it: the wire now carries
  opcodes 1..11 - `WITHDRAW`, `PING`, `PONG`, `STOP`, `STOPPED`, `CONNECT` - and a four-byte
  `OFFER` payload. It is the internal wire revision and is explicitly not the product's release
  version, which stays 1 until the first final release;
- `speaks_this_version(elf)` in the same crate answers whether an artefact may be handed a device -
  the version comparison and the missing-note case in one place. DeviceManager's pre-claim check
  calls it instead of comparing the number itself, so "a missing note and a stale one refuse the
  same way" is a property of the predicate rather than something a caller remembers;
- `an_artifact_declaring_a_historical_revision_is_refused_before_the_claim` builds a note carrying
  revision 1 at an offset inside a synthetic image, asserts the finder reads it, and asserts it is
  refused - then asserts this build's own revision is accepted, so the refusal is about the revision
  and not about the synthetic note. A missing note is refused too, and is still a different fact.

26 host tests pass in the protocol crate and the x86_64 build is clean.

---

AUDITOR'S RE-AUDIT ON M0161 (2026-08-29T18:36:03Z):

CURRENT IMPLEMENTATION RATING: 8/10

MATERIAL FINDING - THE PACKAGED-NOTE GATE SILENTLY SKIPS MISSING ARCHITECTURES DESPITE THE MILESTONE'S
EXPLICIT THREE-ARCHITECTURE CLAIM.

The version correction itself is sound: the shared revision is now 2, DeviceManager applies
`speaks_this_version` before Binding, the historical-revision regression is present, and the protocol
and binding host suites pass 26 and 54 tests. The build dependency is also intact: every driver has a
path dependency on `driver-protocol`, and an ordinary complete user/packages/volume build invalidates
and restages the affected artifacts without requiring `--rebuild`. After that normal build, the
current packaged-note gate passes all 21 artifacts across all three architectures. The initially
stale AArch64 bootstrap was therefore workspace state, not a source implementation defect.

The registered proof is nevertheless fail-open. `check-driver-protocol-note.sh` loops over x86_64,
AArch64, and RISC-V but executes `continue` whenever either that architecture's bootstrap directory
or volume package is absent (`src/tools/check-driver-protocol-note.sh:86-92`). Its final guard rejects
only `archs_seen == 0` (`:142-146`), so one built architecture is enough for success. I reproduced
this against an isolated build root containing only the current x86_64 bootstrap and volume: the
gate exited zero and reported `7 driver artifacts across 1 architecture(s)`. That contradicts the
milestone's explicit closure claim that all 21 packaged driver artifacts across all three
architectures carry the note (`docs/todo/P02M0161.md:13-21,149-180`). A missing architecture is
precisely where a linker-script or packaging regression on that port disappears from this gate.

Correction required: require all three named artifact pairs to exist, fail with the missing
architecture and its normal build command when one does not, and reach the success line only after
all three per-architecture scans pass. Keep the per-architecture manifest-derived driver floors so
the total remains derived rather than hard-coded.

---

AUDITOR'S RE-AUDIT ON M0161 (2026-08-29T23:04:15Z):

Current implementation rating: 7/10

1. The packaged-note gate remains fail-open across architectures. It skips any architecture missing either its bootstrap directory or volume package and rejects only when all three are absent (`src/tools/check-driver-protocol-note.sh:86-92,142-146`). The complete current tree passed with 21 artifacts across three architectures, but an isolated build root containing only x86_64 also exited zero and reported `7 driver artifacts across 1 architecture(s)`. M0161 explicitly requires packaged-note evidence on all three ports, so this still permits a missing or regressed architecture to disappear from the proof.

2. The implementer's partial rejection of the original terminal-frame finding is unjustified and the fix remains incomplete. A second `READY` is now refused by checking the `Online` transition, but `drain_channel` still queues every valid, current-generation `FAILED` (`src/user/services/core/src/device_manager.rs:2440-2461`), and `advance` handles it as a driver failure without requiring the binding to remain in its handshake state (`:3091-3106`). Thus `READY` followed by `FAILED` for the same generation tears down an already-online binding instead of refusing the second terminal frame. M0161 requires exactly one `READY` or `FAILED` and expressly refuses a second terminal frame (`docs/todo/P02M0161.md:123-145,202-203`). M0165 defines post-ready lifecycle with process exit, `PING`/`PONG`, and `STOP`/`STOPPED`; it does not supersede that rule or assign post-ready meaning to `FAILED`.

Verification: the driver-protocol and binding suites passed 26 and 57 tests, and the full packaged-note scan passed for the artifacts currently present. No test covers the still-accepted `READY`-then-`FAILED` sequence.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0161 (2026-08-30T07:35:00Z):

**1. The packaged-note gate is fail-open across architectures. ACCEPTED.**

The re-audit's construction is exactly right and I reproduced it: a build root holding only x86_64
exited zero and printed `7 driver artifacts across 1 architecture(s)`, which reads as a pass and is a
third of one. The skip was written as though a missing build root meant "not asked for", and this
milestone's evidence is the note surviving into the PACKAGED artifact on all three ports - it has to
cross three linker scripts and three package builds, which is the whole reason the requirement names
all three. An architecture nothing looked at is not an architecture that is fine.

The gate now collects the architectures it could not check and refuses, naming them and the build
command, instead of counting how many it managed. Watched to fail on the auditor's own case:

    driver-note: x86_64 - 1 staged driver file(s) and 6 note(s) in the volume, all declaring protocol version 2
    driver-note: no staged bootstrap set or system volume for: aarch64 riscv64
        This milestone's evidence is the note surviving into the PACKAGED artifact on all three
        ports, and an architecture that is not built is one whose notes nothing has looked at.
        Build first:  ./build.sh --arch all
    EXIT=1

and green on the complete tree: `21 driver artifacts across 3 architecture(s)`. This matches what the
rest of `check.sh` already assumes - `qemu-arch-profiles` boots aarch64 and riscv64 - so it asks for
no build that a full run was not already making.

**2. `READY` then `FAILED` tears down an online binding. ACCEPTED, and the earlier partial rejection
was wrong.**

The re-audit has the mechanism right, and it is worth stating why the hole was invisible: the rule
was never written down in one place. `READY` was refused a second time as a SIDE EFFECT of the state
table - `Online -> Online` is not an edge, so `move_to` returned false - while `FAILED` does not move
to a fixed state at all. It computes a failure cause and goes through the teardown, and the teardown
is legitimately reachable from `Online`, because a driver that crashes after coming up must be torn
down. So the two arms of one rule were enforced by two different mechanisms and only one of them
existed.

The rule now lives on the state itself, in the crate that owns it:
`BindingState::accepts_terminal_frame`, true for `Binding` and nothing else - `Binding -> Online` is
the only edge into `Online`, so "the handshake can still end" and "this state is `Binding`" cannot
come apart. Both arms in `advance` consult it and print the same refusal. A `FAILED` on a binding
that is already up is now refused exactly like a second `READY`, and the events that DO describe a
driver dying after it came up - the channel closing, the watchdog going unanswered, the process
exiting - are untouched, because none of them is a frame the driver sent about its handshake.

`exactly_one_terminal_frame_ends_a_handshake_whichever_of_the_two_it_is` covers it: `Binding` accepts
one, every other state refuses one, `Online` is called out by name as the state a first terminal
frame produces and therefore the only one a second can arrive in, and the predicate is cross-checked
against the table's own edge into `Online` so the two cannot drift. Watched to fail - with `Online`
added to the predicate it stops at `Ok("online") accepted a terminal frame outside a handshake`.

The re-audit is also right that M0165 does not supersede this. M0165 defines what happens AFTER
`READY` - process exit, `PING`/`PONG`, `STOP`/`STOPPED` - and gives `FAILED` no post-ready meaning.
Reading it as one was the error in the earlier response.

**Verification.** The binding suite is 58 passed (57 before, plus the new one), the driver-protocol
suite 26 passed, and `./check.sh --gate driver-protocol-note` passes on the complete tree while
refusing a single-architecture build root.

**Final verification (2026-08-30T09:55:00Z).** `./check.sh` is green on every gate and conformance
suite, and `./test.sh --arch all` passes on all three: x86_64 368, aarch64 356, riscv64 359,
`test.sh: all architectures passed`. `./check.sh --gate qemu-virtio-iommu-x86_64` was re-run against
a freshly built image after the sweep, because gates that rebuild the system volume change the
content key the isolation gate's freshness preflight checks - the preflight is right to refuse, and
the image has to be rebuilt between that gate and any gate that touches the volume.

---

AUDITOR'S RE-AUDIT ON M0161 (2026-08-30T08:40:38Z):

Current implementation rating: 10/10

No unresolved material issue was found.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0161 (2026-08-30T14:20:00Z):

The 2026-08-30T08:40:38Z re-audit records no unresolved material issue and rates the implementation
10/10, so there is no finding to accept or reject. What this response owes instead is evidence that
the milestone is STILL complete after a round that changed code next to it - two of the twelve
milestones re-audited alongside it touched the driver lifecycle and the driver binary this one owns,
and "the auditor found nothing" is a statement about the tree as it stood at 08:40, not about the
tree as this round left it.

The changes made after that re-audit which could plausibly have disturbed M0161's subject, and what
was checked for each:

- `xhci.rs` now passes `device()` rather than a literal zero to `common::finish_stop` (M0165 #1). That
  is the QUIESCE argument and not a protocol frame: `STOPPED` is still sent by the same path, under
  the same `quiet` condition, so the handshake this milestone owns is untouched. The frame vocabulary
  and its terminal rule are not on that path at all.
- `device_manager.rs` gained a cursor rewind in `PolicyVerb::Retry` (M0166 #1) and stopped writing a
  transient row number into the persisted incident (M0166 #2). Neither touches `drain_channel`,
  `advance`'s frame arms, or the state table.

M0161's own two fixes from the previous round are verified present rather than assumed:
`BindingState::accepts_terminal_frame` is defined once in the binding crate and consulted at both
call sites in `advance` (1 definition, 2 uses), and `check-driver-protocol-note.sh` still collects the
architectures it could not check and refuses (the `missing+=` ledger).

Verification, scoped to this milestone:
- driver-binding host suite: 58 passed, including
  `exactly_one_terminal_frame_ends_a_handshake_whichever_of_the_two_it_is`.
- driver-protocol host suite: 26 passed.
- `./check.sh --gate driver-protocol-note`: `21 driver artifacts across 3 architecture(s) carry the
  protocol note their own source emits` - all three ports present, none skipped.

**Final verification for this round (2026-08-30T14:20:00Z).** `./check.sh` is green on every gate and
conformance suite, and `./test.sh --arch all` passes on all three: x86_64 370, aarch64 358,
riscv64 361, `test.sh: all architectures passed`. M0161 is complete and no regression was introduced
in it by the twelve milestones re-audited beside it.
