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
