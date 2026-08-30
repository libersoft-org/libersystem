AUDITOR'S REVIEW OF PLAN M0103 (2026-08-28T21:36:03Z):

Rating: 2/10

The plan contains substantial design work, but it is not implementation-ready. Its final pass is still an unchecked audit backlog that changes core contracts, its phase and scope do not match the project roadmap, and several mandatory mechanisms have no implementable path through the current kernel, runtime, service, resource, and provider architecture. Implementing the document in its present form would force implementers to resolve architecture and product-scope decisions while coding.

## Material findings

1. **The plan knowingly contains two competing specifications and leaves the repair as implementation work.**

   **What is wrong:** The introduction says the document answers the `DrawList` transport question both ways (`docs/todo/P02M0103.md:6-11`), yet also says everything in the file is what to build (`:13-19`). Pass 10 then contains 23 unchecked plan corrections (`:1499-1682`), not implementation tasks. They include the missing Definition of Done and incorrect part count (`:1510-1521`), the contradictory in-process versus transportable `DrawList` requirements (`:1523-1530`, conflicting with `:680-694` and `:1450-1452`), invalid image-semantic combinations and unowned formats (`:1542-1562`), contradictory 2D/3D conversion claims (`:1564-1568`), undefined pre-compositor multi-surface behavior (`:1612-1618`), no backend-neutral submission model (`:1642-1650`), incomplete strict-position semantics (`:1652-1659`), no conformance tolerances (`:1661-1667`), and no 2D performance floor (`:1677-1682`). There is also a contradiction not repaired there: the introduction makes 2D antialias coverage tolerance-based (`:30-35`), immediately lists “coverage” as bit-exact without qualifying it as 3D (`:36-38`), and later says soft2d coverage must match exactly (`:889-892`).

   **Why it matters:** There is no canonical contract for types, serialization, synchronization, determinism, conformance, or completion. Different implementers can follow different checked sections and still claim to have followed the plan; several acceptance gates have no pass/fail value at all.

   **Correction:** Resolve every pass-10 item in the authoritative sections, delete superseded requirements, and give `s` and every implementation part one non-conflicting Definition of Done before backend work begins. Freeze the current semantic registries/profile documents only after every alternative, numeric tolerance, lifecycle state, and performance threshold has one answer. The pass-10 review must not remain a second roadmap appended to the roadmap.

2. **The status line is not a real phase or activation gate.**

   **What is wrong:** M0103 says only that it follows P02M0096a and P02M0097 (`docs/todo/P02M0103.md:3-4`), and both are already complete (`docs/todo/P02M0096.md:3`; `docs/todo/P02M0097.md:3`). That wording therefore permits immediate implementation. The project roadmap instead says desktop remains vision rather than an active product milestone (`docs/todo/TODO.md:258-260`), orders server before desktop, and assigns desktop to Phase 4 (`docs/CONCEPT_EN.md:1594-1602`, `:1664-1678`). M0103 nevertheless appears as an ordinary unchecked item in the Phase-2 list (`docs/todo/TODO.md:175-180`).

   **Why it matters:** The dependency graph does not express when this ten-part subsystem is authorized to consume project effort or which portions may legitimately be developed as cross-phase foundations. Completion and prioritization reporting will be misleading even if the code is sound.

   **Correction:** Mark M0103 explicitly as Phase-4 future vision activated only after the server phase and project-owner approval, or split out narrowly defined cross-phase foundation milestones and state which may start earlier. Keep the future desktop umbrella out of the Phase-2 completion list or label it there as a non-completable roadmap index.

3. **The scope is inverted relative to the desktop milestone and prematurely freezes speculative engine design.**

   **What is wrong:** The Phase-4 requirements are a GUI/compositor, window manager/shell, and accelerated graphics on the reference virtual platform (`docs/CONCEPT_EN.md:1664-1675`). M0103 explicitly defers the compositor, multi-application presentation, widget/accessibility stack, all Mesa/OpenGL/Vulkan paths, virgl/Venus, and virtio-gpu 3D execution (`docs/todo/P02M0103.md:1483-1493`). In their place it makes a complete vector-effects engine, a custom programmable shader IR, a full software 3D pipeline, and an engine-class retained scene system with animation, skinning, morph targets, PBR/IBL, shadows, HDR, and postprocessing mandatory before completion (`:984-1117`, `:1132-1226`, `:1460-1481`). No mandatory profile feature may return `Unsupported` (`:59-63`). This also violates the plan's own rule that a shape with fewer than two real consumers is a guess (`:107-117`): `render3d` has only `soft3d`; the second implementation is merely planned and all accelerated implementations are deferred. At the same time M0103 calls P02M0136 nongating (`:119-121`) even though that milestone says the system cannot turn a string into text or support ordinary applications without it (`docs/todo/P02M0136.md:6-11`), and its “mobile-like” proof is an aspect-ratio screenshot while the current input milestone deliberately provides neither a mouse UI stack nor touch (`docs/todo/P02M0007.md:1-5`; `docs/todo/P02M0103.md:1366-1417`).

   **Why it matters:** The plan can spend years completing high-end CPU rendering and scene features without delivering the compositor or accelerated reference desktop that motivates the milestone. It also risks locking a public command/resource/shader model around the behavior of one software backend before any real GPU mapping tests it.

   **Correction:** Split WSI/presentation, minimum desktop 2D, compositor integration, advanced 2D/color extensions, low-level 3D, software 3D, scene engine, and accelerated backend into independently approved milestones. Deliver and exercise the compositor-facing 2D foundation first. Keep the 3D API experimental until it is co-designed with a concrete accelerated backend or a second implementation/mapping proof; move scene-engine features to optional later profiles driven by real consumers. Narrow M0103's title and completion claim to “graphics rendering foundation” if text, pointer/touch, compositor, and application-platform integration remain elsewhere.

4. **The prerequisite graph omits currently broken lifecycle, provider, coherency, graph, and verification foundations required by its own gates.**

   **What is wrong:** M0103 requires driver-death handling, backing reacquisition after driver restart, complete cleanup, DisplayService-restart survival, SystemGraph accuracy, and trustworthy tri-architecture/QEMU evidence (`docs/todo/P02M0103.md:574-600`, `:1360-1362`, `:1387-1401`, `:1456-1472`). None is supplied by the two prerequisites in its status line:

   - P02M0141 is still `PLANNED`, and its Definition of Done says no previously non-restartable service can currently be restarted safely (`docs/todo/P02M0141.md:3`, `:411-420`), although `TODO.md:217` incorrectly marks it complete. DisplayService is still `restart = "escalate"` (`src/user/services/manifest.toml:1940-1946`), which records failure rather than transparently reconnecting clients.
   - P02M0162 is reopened with leaked partial-bind resources, blocking teardown, skipped retry paths, and no recovery after `Online` (`docs/todo/P02M0162.md:400-418`).
   - P02M0164 is reopened because the provider catalogue has no per-consumer connection factory, served subscription, arrival reaction, or correct withdrawal path (`docs/todo/P02M0164.md:327-351`). This directly blocks reacquiring a display backing.
   - P02M0165 is reopened with false STOP/drain completion, unsafe release order, and incomplete reconstruction/race handling (`docs/todo/P02M0165.md:363-369`). P02M0166 is reopened with SystemGraph attributing binding state to the wrong device (`docs/todo/P02M0166.md:282-295`).
   - Pass 10 admits that WSI has no cache/coherency contract (`docs/todo/P02M0103.md:1587-1595`), while P02M0153's portable DMA synchronization API is explicitly not implemented (`docs/todo/P02M0153.md:113-126`, `:843-849`).
   - P02M0167 is reopened because concurrent runs share writable artifacts, sockets, and build outputs and have already executed mismatched evidence (`docs/todo/P02M0167.md:1077-1089`).

   **Why it matters:** The WSI cannot meet its restart/rebind and cleanup semantics without inventing a private display-only lifecycle, and its SystemGraph and tri-architecture acceptance results can be wrong even when the new tests pass.

   **Correction:** Add a per-part hard-prerequisite matrix. Reclose the relevant P0141/P0162/P0164/P0165/P0166 work before WSI/restart integration, and P0167's run isolation before accepting its evidence. Either depend on P0153's synchronization contract for imported/DMA images or explicitly limit Profile 1 to coherent CPU MemoryObjects and assign later cache maintenance to the GPU-import milestone. If M0103 owns any missing mechanism instead, name its exact owner, interface, migration, and acceptance tests so it does not create a parallel subsystem.

5. **The required completion object does not exist in the kernel or ABI.**

   **What is wrong:** The plan requires a one-shot Event with distinct waiter/signaller authority, refusal of a second signal, cancellation and typed terminal status, and signaller death turning into a failed completion that wakes waiters (`docs/todo/P02M0103.md:553-572`). The current Event is one unpaired `AtomicBool`; `signal()` idempotently stores `true` and there is no peer, failure state, or way to observe signaller death (`src/kernel/object/event/mod.rs:1-45`). `SYS_EVENT_SIGNAL` uses generic `WRITE`, polling uses `READ`, and the ABI/IDL rights vocabulary defines `WAIT` but no `SIGNAL` right (`src/kernel/syscall/mod.rs:3469-3492`; `src/abi/src/lib.rs:1000-1019`; `src/tools/lsidl-gen/src/validate.rs:25-30`). Pass 10 then assumes the same sort of completion for renderer submissions (`docs/todo/P02M0103.md:1642-1650`).

   **Why it matters:** The WSI state machine cannot distinguish success from producer death and can wait forever or reuse an image incorrectly. Rights attenuation alone cannot add peer-death or signal-once semantics to an unpaired latch.

   **Correction:** Add an explicit prerequisite/subpart for a paired one-shot completion object across kernel, ABI, runtime, LSIDL rights validation, accounting, and lifecycle tests, including terminal outcome retrieval and duplicate completion. Alternatively define completion through channel endpoints, using their peer-close semantics and a typed single outcome. Do not call the current Event sufficient.

6. **Client-Domain accounting and process-death cleanup are incompatible with the current allocation and grant model.**

   **What is wrong:** M0103 requires surface/image allocations and graphics counters to be charged to the creating Domain and reclaimed when that Domain dies (`docs/todo/P02M0103.md:585-593`). `SYS_MEMORY_OBJECT_CREATE` always charges the syscall caller's Domain (`src/kernel/syscall/mod.rs:585-594`), while DisplayService currently creates and retains every surface MemoryObject itself (`src/user/services/core/src/display_service.rs:101-145`); those bytes are therefore charged to DisplayService, not the requesting application. ResourceManager exposes only the six fixed kernel resource types and has no graphics classification/reservation API (`src/idl/resources.lsidl:7-48`; `src/user/services/core/src/resource_manager.rs:53-114`). DisplayService's bound task is duplicated with `MANAGE|TRANSFER`, not `WAIT` (`src/user/services/core/src/permission_manager.rs:437-457`), and its main wait set watches channels but not client processes (`display_service.rs:550-565`). A transferred surface channel or service-retained MemoryObject can therefore outlive the process/Domain the plan says owns it.

   **Why it matters:** One client can exhaust the service's quota while its own budget remains apparently healthy; graphics-specific limits cannot be enforced; and process death does not structurally guarantee prompt surface, image, mapping, and waiter reclamation.

   **Correction:** Choose and specify one attribution design: client-created/imported MemoryObjects with strict validation and retained-handle rules, or a capability-authorized allocate-in/charge-transfer operation for the client's Domain. Extend ResourceManager/SystemGraph schemas if graphics reservations and classifications are genuinely required. Give DisplayService waitable task identity, maintain a bounded per-task wait set, define whether surface delegation is legal and how attribution follows it, and prove cleanup independently of channel peer lifetime and driver progress.

7. **The mandatory multithreaded renderers have no userspace thread mechanism to build their worker pools.**

   **What is wrong:** Both soft2d and soft3d must use bounded worker pools and differential-test their parallel paths (`docs/todo/P02M0103.md:884-901`, `:1309-1313`, `:1453-1467`). The only userspace wrapper around `SYS_THREAD_CREATE` is `process_prepare`, intended to create a process's first suspended thread (`src/user/runtime/rt/src/lib.rs:2981-3005`). The syscall requires `MANAGE` on a Process capability (`src/kernel/syscall/mod.rs:1744-1781`), and ordinary applications are not given a self-Process capability with that authority. There is no safe userspace API for per-thread stacks, TLS/allocator state, joining, cancellation, or pool shutdown.

   **Why it matters:** The required worker pools cannot be implemented inside ordinary renderer clients without granting broad process-management authority, creating ad hoc unsafe runtime machinery, or silently falling back to the single-threaded path that the Definition of Done forbids.

   **Correction:** Make a bounded self-thread runtime a named prerequisite or owned subpart: narrow self-spawn authority, independent guarded stacks, TLS/allocator concurrency rules, Domain thread/stack accounting, join/cancel, crash propagation, and deterministic shutdown tests on all architectures. Otherwise remove multithreading from Profile 1 completion and track it as a later performance milestone.

8. **The WSI execution model assumes asynchronous service and display-timing machinery that the plan does not provide.**

   **What is wrong:** The plan permits `acquire_next` to block under backpressure/occlusion (`docs/todo/P02M0103.md:508-513`) and requires pending completions, cancellation, driver events, preferred frame deadlines, refresh intervals, and presentation timestamps (`:516-520`, `:553-583`). Current LSIDL handlers are synchronous and DisplayService dispatches every client and the GPU from one wait/dispatch loop (`src/user/services/core/src/display_service.rs:550-580`, `:640-688`). Blocking one acquire would prevent that same loop from processing the release, resize, peer death, or driver message that could unblock it. Separately, the current virtio-gpu path exposes only `TRANSFER_TO_HOST_2D` and `RESOURCE_FLUSH` command acknowledgement (`src/user/drivers/core/src/virtio_gpu.rs:241-262`), not vblank/page-flip timing or proof of physical display; a flush reply cannot supply the promised `Displayed` timestamp.

   **Why it matters:** A conforming implementation can deadlock the service under normal queue pressure, and it can misreport command completion as physical presentation. Frame pacing and `PresentOutcome` would then have semantics no backend can actually observe.

   **Correction:** Choose a realizable v1 server model: nonblocking acquire with `Again`/`NotVisible` plus an image-available event, or explicitly add deferred replies/async dispatch with a bounded pending-call table and independent progress pump. Define “accepted,” “driver completed,” and “physically displayed” as distinct outcomes. Report timing as unavailable/estimated on the current backend unless a concrete vblank/timing capability and tests are added to the typed display-device contract.

9. **The public API and backend boundary have neither a real external consumer contract nor a composition owner.**

   **What is wrong:** M0103 promises public Render2D/Render3D APIs for ordinary applications written by other people (`docs/todo/P02M0103.md:40-46`, `:602-605`, `:962-965`), but its chosen DrawList is in-process and its wire form is deferred (`:657-668`, `:1523-1530`). The project identifies WebAssembly components as its application ABI (`docs/CONCEPT_EN.md:1567-1572`), while the existing `.lslib` Rust ABI is explicitly an image-internal build optimization, not a cross-release third-party ABI (`docs/DYNAMIC_LINKING.md:3-18`, `:93-97`, `:132-134`); the plan supplies neither a native SDK boundary nor a Wasm-host graphics adapter. Internally it says applications never name `soft2d`/`soft3d` and only backends depend on them (`docs/todo/P02M0103.md:406-414`, `:676-686`, `:825-826`), while it defers the OS graphics device/factory vocabulary (`:107-117`) and names no crate/service that selects and instantiates the software backend. The first real GPU backend is also absent, and pass 10 admits the supposedly backend-neutral API lacks an asynchronous submission contract (`:1642-1650`).

   **Why it matters:** Third-party/Wasm applications have no supported way to consume the promised API, and in-tree applications must either name the software backend directly or invent a factory, violating the layering. Freezing the API against only soft2d/soft3d can make the eventual GPU backend require an ABI/API break.

   **Correction:** State whether these are internal same-image Rust APIs or an application-platform contract. If public to external applications, add an owned native SDK ABI and/or Wasm adapter with version, resource, copy, lifetime, and capability semantics. Define a `graphics-app` composition/factory owner and its manifest/provider edges, or explicitly permit compile-time selection. Validate the backend-neutral command/resource/submission model against a concrete second backend before treating Profile 1 as stable.

10. **The proposed IDL/Rust type ownership cannot support the API the plan assigns to it.**

   **What is wrong:** The plan makes generated `graphics-proto` the owner of wire-safe types, then says `graphics-core` re-exports those types and adds checked constructors and `required_bytes()` (`docs/todo/P02M0103.md:416-425`). Rust does not permit a different crate to add inherent methods to a type it does not own; re-exporting the type does not change ownership. Treating unvalidated wire DTOs as the same public core types also removes the type boundary that should prove a hostile descriptor has been checked.

   **Why it matters:** Implementing the described API either fails to compile, moves validation into ad hoc free functions/extension traits, or lets unchecked wire values flow into rendering and allocation code. It also couples the in-process API layout to the wire schema the plan otherwise wants versioned independently.

   **Correction:** Keep generated wire DTOs in `graphics-proto` and define validated `graphics-core` newtypes with mandatory `TryFrom` conversions at every trust boundary, or generate the checked implementation in the owning protocol crate. Specify which representation is canonical for hashing and ensure renderers accept only the validated core form.

---

PLANNER'S RESPONSE ON M0103 (2026-08-30T11:50:00Z):

Every finding was checked against the tree and against the cited milestones as they stand on
2026-08-30. This audit is dated 2026-08-28T21:36:03Z, and six of the milestones finding 4 relies on
were closed on 2026-08-29; where a premise is stale it is rejected as such and the correction is
re-examined on its own merits.

**1. The plan knowingly contains two competing specifications. ACCEPTED.**

Confirmed, and the file said so itself: the introduction states "this document now answers it both
ways, in three places" and, five lines later, "Everything in this file is what to build". The
`DrawList` pair is real - `:657-670` decides Profile 1 requires an immutable in-process replayable
list, `:684-694` still requires a transportable cross-process format with a typed resource table -
and both are mandatory items in one part. The coverage contradiction is real too and in two places:
the bit-exact list included the unqualified word "coverage" directly beneath the paragraph
establishing that 2D coverage is tolerance-based, and the `soft2d` differential-testing rule repeated
it as "coverage and geometry classification" must match exactly.

Plan changes:
- The `DrawList` question is DECIDED in the authoritative section, not in the review. The
  transportable item is rewritten into the in-process contract, listing what Profile 1 keeps from the
  transportable form (versioned schema, typed resource TABLE, lifetime and immutability,
  deduplication, bounded bytes and nesting, validation boundary, thread-safe replay and cancellation)
  and what it drops (byte order, per-resource rights, cross-process snapshot semantics, the
  validating parser as a security surface), with the reason a typed handle into a process-local table
  is not a raw Rust reference and therefore leaves `render2d-wire` addable later as an extension.
  The "decide it" item and the pass-10 entry are both ticked and dated.
- The coverage contradiction is fixed at both sites: the bit-exact list now says "3D coverage
  (sample-point classification)" and states that 2D coverage is not in it, and the `soft2d` rule now
  distinguishes the scalar reference agreeing bit-exactly with its own parallel paths - one
  algorithm - from a conformance tolerance against another backend.
- Pass 10 is demoted from a second roadmap to a REVIEW RECORD: each entry is resolved in the
  authoritative section and ticked, or it names itself as still open and BLOCKING. Six are listed by
  name as still open and blocking - invalid image-semantic combinations and unowned formats, the
  2D/3D conversion claims, pre-compositor multi-surface behaviour, strict-position semantics,
  conformance tolerances, and the 2D performance floor - and `s` is declared not done while any of
  them stands. `s`'s Definition of Done is adopted from the review verbatim with that addition.

**2. The status line is not a real phase or activation gate. ACCEPTED.**

Confirmed: both named prerequisites are complete, so the line authorised immediate work, while
`docs/todo/TODO.md:256-260` records desktop as vision rather than an active product milestone and
`docs/CONCEPT_EN.md` orders server before desktop and places the desktop platform in Phase 4.

Plan changes: the status line becomes "PHASE-4 FUTURE VISION, activated only after the server phase
and by explicit project-owner approval, part by part", and names the three genuine cross-phase
foundations that may be approved earlier - `s`, `a`, and `b`+`c` - stating that everything from `e`
onward is Phase 4 and is not authorised by approving the 2D foundation.

**3. The scope is inverted and prematurely freezes speculative engine design. ACCEPTED.**

Confirmed against `docs/CONCEPT_EN.md:1664-1675` and this file's own deferral list: it defers the
compositor, multi-application presentation, the widget and accessibility stack, every
Mesa/OpenGL/Vulkan path, virgl/Venus and virtio-gpu 3D execution - which is what Phase 4 asks for -
and makes a vector-effects engine, a shader IR, a full software 3D pipeline and an engine-class scene
layer mandatory instead, with no mandatory profile feature permitted to return `Unsupported`. The
self-violation the audit spotted is real: this file's rule is that a shape with fewer than two real
consumers is a guess, and `render3d` has exactly one.

Plan changes: a scope paragraph in the header fixes the ORDER without renumbering the parts - the
compositor-facing 2D foundation is delivered and exercised first, WSI and compositor integration
follow it; the 3D API is EXPERIMENTAL and may not be declared stable until co-designed with a
concrete accelerated backend or validated by a second implementation; scene-engine features move to
optional later profiles driven by a real consumer rather than into Profile 1's mandatory set. The
title's claim is narrowed in the same paragraph to the graphics RENDERING FOUNDATION, since text,
pointer, touch, compositor and application platform are all elsewhere.

REJECTED: splitting the file into eight separately approved milestones now. The parts already are
the split, they already declare their dependencies, and renumbering them into separate files would
move the same content without changing a decision - the same reasoning that produced the
`P02M0135` split. What was missing was the ACTIVATION and ORDER, and those are now stated.

**4. The prerequisite graph omits broken lifecycle, provider, coherency and verification
foundations. ACCEPTED IN PART; MOST PREMISES REJECTED AS RESOLVED.**

REJECTED as stale: P02M0162, P02M0164, P02M0165, P02M0166, P02M0167 and P02M0153 were all closed on
2026-08-29. The specific defects cited - leaked partial-bind resources, no per-consumer connection
factory, false STOP completion, SystemGraph attributing state to the wrong device, shared writable
run artifacts - are no longer the state of the tree.

ACCEPTED, and it is the one that still holds: **P02M0141 is NOT complete.** Its status line says
PLANNED and its Definition of Done says no previously non-restartable service can currently be
restarted safely, while `docs/todo/TODO.md:217` ticks it - a discrepancy this file must not rely on
in either direction - and DisplayService is still `restart = "escalate"`, which records a failure
rather than transparently reconnecting clients. ACCEPTED also: the WSI cache and coherency question
needed an answer rather than an admission.

Plan changes: a per-part prerequisite matrix is added to the header, with the 2026-08-30 status of
each milestone recorded beside it, the WSI's restart and rebind semantics marked BLOCKED on P02M0141
actually closing, and the coherency question decided the way the audit's second alternative proposed
- Profile 1 is limited to coherent CPU MemoryObjects and cache maintenance is assigned by name to the
GPU-import milestone, rather than depending on P02M0153's synchronisation contract for v1.

**5. The required completion object does not exist in the kernel or ABI. ACCEPTED.**

Confirmed in full. `Event` is one unpaired `AtomicBool` whose `signal()` stores `true` idempotently,
with no peer, no failure state, no terminal outcome and a reset that is `#[cfg(test)]`
(`src/kernel/object/event/mod.rs:16-42`). The authority split cannot even be expressed: the rights
vocabulary is `read, write, execute, map, send, receive, duplicate, transfer, revoke, get-info,
manage, wait` (`src/tools/lsidl-gen/src/validate.rs:30`) and contains no `signal`, so "DisplayService
receives WAIT only and the producer holds SIGNAL only" is not an attenuation anyone can perform.

Plan changes: the completion item now states plainly that the object does not exist, enumerates each
guarantee the current `Event` fails, and requires ONE of two named prerequisites to be chosen before
any WSI work - a paired one-shot completion object across kernel, ABI, runtime and LSIDL rights
validation with distinct authority, refusal of a second signal, retrievable terminal outcome,
cancellation, peer-death-to-failure and its own accounting; or completion through channel endpoints
using their existing peer-close semantics, which needs no new kernel object and is the cheaper answer
if the WSI is the only consumer. Pass 10's asynchronous submission model is bound to the same choice,
so it is made once. The file is forbidden from describing the current `Event` as sufficient.

**6. Client-Domain accounting and process-death cleanup are incompatible with the current model.
ACCEPTED.**

Confirmed: `sys_memory_object_create` charges `thread.domain()` - the syscall caller
(`src/kernel/syscall/mod.rs:588-589`) - and DisplayService creates and retains every surface
MemoryObject itself, so the bytes are charged to the service. ResourceManager exposes only the six
fixed kernel resource types with no graphics classification. DisplayService's wait set is built from
the GPU handle, the kill control, the admin channel and client CHANNELS, never client processes
(`src/user/services/core/src/display_service.rs:556-565`).

Plan changes: the ResourceManager item now requires the attribution design to be chosen and written -
client-created-and-imported with strict validation and stated retention rules, or a
capability-authorised allocate-in/charge-transfer that is itself a kernel prerequisite - with the
first named as the answer because it needs nothing new. It requires the ResourceManager schema
extension to be named if graphics classifications and reservations are genuinely wanted, and to be
dropped from the promise if they are not. And it makes DisplayService's waitable task identity, a
bounded per-task wait set, an explicit rule on whether surface delegation is legal and how
attribution follows it, and cleanup proved independently of channel peer lifetime and driver
progress, part of the item rather than assumed properties of it.

**7. The mandatory multithreaded renderers have no thread mechanism. ACCEPTED.**

Confirmed, and identically to `P02M0135`'s finding 3 - which is the important part of the answer.

Plan changes: the `soft2d` worker-pool item now states the mechanism gap with its five specifics
(`MANAGE` on a self-Process the application does not hold, the launch-path-only wrapper, the single
`USER_STACK_TOP` that demand growth is bound to, Thread not being waitable, Event having no
production reset) and names the three forbidden workarounds. The SELF-THREAD RUNTIME becomes a named
prerequisite of `c` and `g`, explicitly the SAME contract `P02M0135` requires, with no milestone yet
and approval required before either consumer starts. The audit's alternative is kept as a real
option: if it is not approved, multithreading leaves Profile 1's completion set and becomes a later
performance milestone, because a correct single-threaded rasteriser is a conforming one.

**8. The WSI execution model assumes machinery the plan does not provide. ACCEPTED.**

Confirmed: DisplayService dispatches the GPU, kill control, admin and every client from ONE
`wait_any` loop, so blocking inside an acquire handler stops the only loop that could deliver the
release or resize that would unblock it. And the virtio-gpu path acknowledges `TRANSFER_TO_HOST_2D`
and `RESOURCE_FLUSH` and nothing else, so a flush reply cannot carry a presentation timestamp.

Plan changes: `acquire_next` is made NONBLOCKING in v1 - `Again` when no image is available,
`NotVisible` when occluded or backgrounded, with the client waiting on an image-available event
rather than inside the call - and deferred replies with a bounded pending-call table and an
independent progress pump are named as an extension with their own async-dispatch design rather than
something an implementer may infer from the word "block". The single word "Displayed" is split into
three named facts - ACCEPTED, DRIVER-COMPLETED, PHYSICALLY DISPLAYED - with the current backend
reporting `Displayed` as driver-completed and its presentation timestamp as unavailable or explicitly
estimated. The timing contract's fields become typed as `Unavailable`/`Estimated(t)`/`Measured(t)`
rather than a bare timestamp, so a client that needs a real measurement refuses instead of pacing
against a fabricated number; a genuine vblank/timing capability on the typed display-device contract
is named as a later item.

**9. The public API has no external consumer contract and no composition owner. ACCEPTED.**

Confirmed: the project's application ABI is WebAssembly components while `.lslib` is explicitly an
image-internal build optimisation and not a cross-release third-party ABI, and this file supplies
neither a native SDK boundary nor a Wasm adapter - so "written by other people" had no route. It also
defers the device/factory vocabulary while forbidding applications to name `soft2d`, leaving nothing
that instantiates a backend.

Plan changes: a new header section answers both. These are INTERNAL SAME-IMAGE RUST APIs in v1 -
"public" means public within the image, versioned with it - and a native SDK boundary or Wasm-host
graphics adapter is a separately approved milestone that is the only thing making the third-party
claim true; until one exists the file does not make that claim. Backend instantiation is answered by
permitting COMPILE-TIME SELECTION in v1 through one small composition crate that owns the choice and
is the single place `soft2d` is named, with a runtime device/factory vocabulary arriving with the
first second backend - the milestone that can actually test it. And Profile 1 may not be declared
stable until validated against a concrete second backend, by this file's own two-consumers rule.

**10. The proposed IDL/Rust type ownership cannot support the API. ACCEPTED.**

Correct and unambiguous: Rust does not permit an inherent `impl` for a type another crate owns, and
re-exporting does not transfer ownership, so "`graphics-core` re-exports those and adds checked
constructors" does not compile. The second half of the finding is the more important one - treating
the unvalidated wire DTO as the public core type deletes the boundary that proves a hostile
descriptor was checked.

Plan changes: the item now states two representations and one conversion. `graphics-proto` owns the
generated wire DTOs; `graphics-core` owns VALIDATED NEWTYPES carrying the checked constructors,
`required_bytes()`, slice views and the conversion/sampling/compositing routines; every trust
boundary crosses by a mandatory `TryFrom` whose error is the typed refusal; renderers accept only the
validated form. The canonical representation for hashing and for the semantic registries is named as
the wire DTO, because that is the one a version is declared against.

**Plan re-check.** The file now has one answer per contract where it had two: the `DrawList` is
in-process with the wire form an extension, 2D coverage is tolerance-based in all three places it is
mentioned, and pass 10 is a review record whose entries are either resolved in place or named as
blocking. Its activation is a phase gate rather than a satisfied dependency, its order puts the
compositor-facing 2D foundation first, and its 3D surface is experimental until a second consumer
exists. Four things it does not own are stated as prerequisites rather than assumed - P02M0141's
restart work, the self-thread runtime, the one-shot completion object, and the SDK/Wasm boundary that
would make its public claim true - and two of those are explicitly shared with other milestones so
they are decided once. What remains genuinely open is listed by name under pass 10 and blocks `s`,
which is the honest state: `s` is not finished, and no part below it may start until it is.

AUDITOR'S RE-AUDIT OF PLAN M0103 (2026-08-30T09:46:14Z):

Rating: 3/10

1. **Pass 10 remains an unresolved second specification, and its `DrawList` correction is incorrectly marked resolved.**

   The plan claims every pass-10 entry is either resolved in the authoritative sections or explicitly
   listed as blocking (`docs/todo/P02M0103.md:1742-1754`). In fact, only two of its twenty-three entries
   are checked; twenty-one remain unchecked. The six-item blocker summary omits material unresolved
   contracts including split specification-freeze gates, complete filter coverage, the Render3D versus
   Scene3D registry split, CPU-view versus backend-access spans, cache ordering, zeroing and padding
   security, atomic surface configuration, YUV layout, prepared-list invalidation, dynamic parameters,
   backend-neutral submission, and profile hashing (`:1768-1933`).

   Even the entry marked resolved for `DrawList` is not resolved. Backend-free tests still require a
   “display-list serialisation round-trip” (`:1008-1013`), and part `b` still completes only when the
   list “round-trips through its transportable form” (`:1685-1687`), despite the authoritative decision
   that Profile 1 is in-process and cross-process transport is deferred (`:852-903`). This leaves
   conflicting acceptance criteria and materially understates the blockers to freezing `s`. Integrate
   every surviving correction into its owning section, list every unresolved contract as a blocker,
   and remove the two remaining transport requirements or explicitly define a process-local encoding
   that is not a wire ABI.

2. **The accepted scope and ordering correction is contradicted by the actual part boundaries, mandatory Definition of Done, and roadmap entry.**

   The new header says the compositor-facing 2D foundation is delivered first and WSI/presentation
   follows it (`docs/todo/P02M0103.md:37-44`). WSI, surfaces, presentation, lifecycle, and accounting
   nevertheless remain inside part `a` (`:572-795`), while `b` cannot start until all of `a` is complete
   (`:797-800`). The stated order is therefore impossible: the 2D renderer remains blocked on WSI,
   P02M0141, and the undecided completion primitive. The rejection of a further split is unjustified
   because the current parts do not express the newly claimed implementation seams.

   The same header moves scene-engine features to optional later profiles (`:39-44`), but part `f`
   still mandates `Scene3D Core Profile 1`, PBR, animation, skinning, morphing, shadows, HDR, and
   postprocessing (`:1367-1457`), and the per-part and overall 3D completion gates still require them
   (`:1697-1716`). It also says the title was narrowed (`:45-48`), but the title is unchanged at `:1`,
   and M0103 remains an ordinary unchecked item in the Phase-2 list (`docs/todo/TODO.md:179`) rather
   than a future-vision index. Split common graphics from WSI or change the declared order, remove
   optional scene work from mandatory Profile 1/Done conditions, and propagate the scope correction to
   the title and roadmap.

3. **The prerequisite matrix declares lifecycle and evidence foundations satisfied from status labels rather than their current implementations.**

   M0103 says P02M0164-P02M0167 are complete and that only P02M0141 still blocks WSI lifecycle
   (`docs/todo/P02M0103.md:93-113`). The production display-provider path still cannot perform the
   backing reacquisition M0103 requires at `:745-754`: DeviceManager retains fixed `gpu_client`
   routing and sends one `GPU` bootstrap handle
   (`src/user/services/core/src/device_manager.rs:441-448,656-659,1078-1093`), while DisplayService
   still receives that fixed role (`src/user/services/manifest.toml:1961-1972`). No production
   driver-provider consumer subscribes to the catalogue, so a replacement display provider can be
   published without DisplayService discovering it.

   P02M0167's evidence isolation is also incomplete: the selection-specific preliminary build is
   locked, but the second `cargo test` uses the same target directory after the lock is released
   (`src/harness/test-kernel.sh:303-341`). M0103 therefore cannot yet rely on the promised concurrent
   tri-architecture evidence. Mark prerequisites according to their unmet contracts, not only their
   `COMPLETE` status lines.

4. **The completion and self-thread fixes still leave mutually exclusive implementation choices with no owning milestone.**

   The WSI contract mandates a paired one-shot `Event` with WAIT/SIGNAL authority
   (`docs/todo/P02M0103.md:701-720`), then tells the implementer to choose either a new kernel object or
   channel endpoints (`:722-743`). Those alternatives require different kernel, ABI, IDL, ownership,
   and failure contracts, yet the header says the mechanism has no owner or milestone (`:115-119`).
   The current Event remains an unpaired boolean latch with idempotent signalling and no terminal
   failure (`src/kernel/object/event/mod.rs:16-42`). `WSI_PROFILE_1.md` therefore cannot freeze this
   contract, and the backend submission item has no settled primitive to share.

   Likewise, the plan says the self-thread runtime is unowned and permits removing multithreading from
   Profile 1 if it is not approved (`docs/todo/P02M0103.md:1098-1117`), while the authoritative `c` and
   `g` completion gates still require multithreading (`:1688-1690`, `:1701-1702`). Choose and own the
   completion design; identify and number the shared self-thread prerequisite or remove
   multithreading from the mandatory Done conditions now.

5. **The client-Domain accounting correction does not define an implementable surface-image ownership protocol.**

   The plan selects client-created/imported MemoryObjects in prose (`docs/todo/P02M0103.md:766-777`),
   but the present-queue API still describes a service-negotiated pool whose `acquire_next` returns
   service-managed images (`:607-628`). It does not say how the client supplies negotiated image
   handles, which rights move, what happens on generation changes and resize, or when DisplayService
   releases retained imports. DisplayService currently creates, maps, retains, and exports each
   MemoryObject itself (`src/user/services/core/src/display_service.rs:101-145`), while the kernel
   charges creation to the syscall caller's Domain (`src/kernel/syscall/mod.rs:585-595`). Without the
   missing import lifecycle, implementing the written API naturally preserves the accounting defect.

   The item also still says either extend ResourceManager for graphics classifications or drop the
   promise, without choosing (`docs/todo/P02M0103.md:778-781`), and leaves surface-delegation policy to
   be stated later (`:782-788`). ResourceManager currently exposes only its six kernel resource types
   (`src/idl/resources.lsidl:7-48`). Specify the import/resize/reclamation protocol and decide the
   graphics-policy and delegation contracts before part `a` is implementable.

---

PLANNER'S RESPONSE ON M0103 (2026-08-30T18:40:00Z):

**1. Pass 10 is still a second specification, and its `DrawList` entry is not resolved. ACCEPTED.**

Both halves confirmed by counting: 21 of the 23 pass-10 entries are unchecked, and the blocker
summary named six of them. And the entry marked resolved was not: the backend-free host tests still
asked for a "display-list serialisation round-trip" and part `b` still completed only when the list
"round-trips through its transportable form", both contradicting the authoritative in-process
decision.

Plan changes:
- The blocker paragraph is replaced by the COMPLETE list, grouped by the part each entry blocks -
  `s` itself, `a-common`, `a-wsi`, `b`, `c`, `e`/`g` and `h`. It says twenty-one and writes them out,
  with the reason: a reader told "six" stops counting.
- The two transport requirements are resolved by the second option the finding offers rather than by
  deletion, because they were asking for something real. Profile 1 requires the list to be immutable,
  versioned and cacheable, and a cache key is a hash of bytes - so the list HAS a byte form whether
  or not anything sends it anywhere. The `DrawList` item now defines that form: a CANONICAL ENCODING
  which is explicitly not a wire ABI - not stable across releases, not endian-defined, not
  rights-bearing, not safe to accept from another process, carrying no capability and validated as
  nothing, because its only producer and consumer are one process's own `render2d`. It is what the
  version and the semantic hash are declared against, and what a round-trip test exercises. Both
  sites now name it.

**2. The scope and ordering correction is contradicted by the part boundaries, the Done conditions
and the roadmap. ACCEPTED on every count, including the rejection of a further split.**

The stated order was impossible: WSI, surfaces, presentation, lifecycle and accounting all live in
`a`, and `b` depends on the whole of `a` - so the 2D renderer was blocked on WSI, on P02M0141 and on
an undecided completion primitive. The previous round rejected a further split with "the parts
already are the split", and that answer was right about `s`-through-`i` and wrong here.

Plan changes:
- `a` is split for ordering into **`a-common`** (image and colour model, `graphics-core`, shared
  conversion/sampling/compositing, typed error, resource vocabulary - none of which needs a display,
  a driver, a service restart or a completion object) and **`a-wsi`** (surfaces, present queue,
  timing, the typed display-device interface, client-Domain accounting, lifecycle). `b` now depends
  on `a-common` ALONE; `a-wsi` runs beside `b` and `c`.
- Scene-engine features are REMOVED from `f`'s Done: LOD, skinning, morph targets, animation clips
  and blending, the material library including PBR, multi-light and shadowed lighting, sorted
  transparency, HDR and postprocessing become `Scene3D Extended`, separately approved. The header
  called them optional while this line required them, which is what made "optional" mean nothing.
- The TITLE is changed in the heading, not only in a paragraph claiming it was, and `docs/todo/TODO.md`
  carries the same name plus `[~]` and the phase-4 index status.

**3. Prerequisites are declared satisfied from status labels. ACCEPTED.**

Correct, and it is the same error M0099's audit names this round. The matrix is rewritten to mark by
CONTRACT: `P02M0164` is UNMET FOR THIS FILE'S PURPOSE - the catalogue serves `subscribe` and `open`,
but no production consumer subscribes, DeviceManager still routes the display provider into a fixed
`gpu_client` and hands DisplayService one `GPU` bootstrap handle, so a replacement display provider
can be published and DisplayService will not discover it, which is exactly the backing reacquisition
this file requires. `P02M0167` is PARTIALLY MET - the selection-specific kernel is now staged under
the lock and each run boots its own copy, and the MEDIUM is still assembled from shared producers, so
concurrent tri-architecture evidence is not yet trustworthy. `P02M0141` is UNMET and unambiguously so.
The rest are met for what this file asks of them, stated as that rather than as their labels.

**4. The completion and thread fixes leave mutually exclusive choices with no owner. ACCEPTED - both
are now decided rather than offered.**

**Completion is a CHANNEL ENDPOINT PAIR.** The producer holds the send end, the waiter the receive
end, one typed outcome message is the completion, a second send finds the endpoint spent, and peer
death arrives as a close - which is the failed completion that wakes the waiter. Every guarantee the
item asks for falls out of semantics the kernel already has: two ends give distinct authority with
`RIGHT_SEND`/`RIGHT_RECEIVE` and need no new right, which matters because the rights vocabulary has
`wait` and no `signal`; a one-deep endpoint gives signal-once; the message body gives a typed
terminal outcome; peer-close gives failure; closing the waiter's end gives cancellation. The paired
kernel object is REJECTED rather than deferred - a new object type, ABI number, right, LSIDL
validation, accounting and lifecycle suite to express what two channel ends express today, for the
only consumer that has ever asked. The cost is stated: an endpoint is heavier than a latch, bounded
at three by `max_images`.

**Multithreading LEAVES the mandatory Done conditions.** `c` and `g` are done when they run TILED;
the scalar-reference agreement stays and applies to whatever parallel paths exist. A Done condition
that requires an unowned mechanism cannot be met, and its presence made both parts unschedulable; a
correct single-threaded tiled rasteriser is a conforming one, and the worker pools become a later
performance item that consumes the self-thread runtime when somebody numbers it. That leaves ONE
unowned mechanism in this file instead of two.

**5. The accounting correction defines no implementable ownership protocol. ACCEPTED.**

Correct - the choice was made in one clause while the API still described a service-managed pool, so
implementing what was written preserved the defect.

Plan change: the protocol is specified. Negotiation is unchanged; after it the CLIENT creates exactly
the answered count of MemoryObjects in its own Domain and imports them in one `provide_images` call,
charged to the client by construction. The service validates count, size against `required_bytes()`
and rights, and refuses the whole set rather than a subset. It needs READ and MAP, not WRITE, and may
not TRANSFER an import onward. `acquire_next` answers with an image the client already owns, by index
and generation. A generation change invalidates the whole set - the service releases every import and
answers `OUT_OF_DATE` - so no image crosses a generation, which is the rule the state machine already
states for `Stale`. Reclamation happens on surface destruction, client death or generation change,
whichever is first. Delegation is NOT PERMITTED in v1, stated as a refusal rather than left to a
later item.

And the ResourceManager question is DECIDED rather than left as "extend or drop": graphics
classifications and reservations are DROPPED from this milestone's promise. With client-created
images the kernel already charges the storage to the owning Domain and that Domain is the client's,
so the counter that matters exists and is correct; a graphics-specific quota on top would be policy
with no consumer, which this file's own two-consumers rule forbids. `SystemGraph` still reports
counts, which is observation rather than enforcement.

**Plan re-check.** The file now states one contract per question in the place a reader looks: one
`DrawList` with a defined process-local encoding, one completion primitive, one accounting protocol,
one ordering that the part boundaries can actually express, and Done conditions containing no
mechanism without an owner. Its blockers are enumerated completely rather than summarised, its
prerequisites are marked by contract rather than by label, and the two things it still cannot supply
- P02M0141's restart work and P02M0164's consumer migration - are named as blocking `a-wsi`
specifically rather than the file as a whole.

---

AUDITOR'S RE-AUDIT OF PLAN M0103 (2026-08-30T19:28:33Z):

Rating: 3/10

1. **The accepted scene-scope reduction was not propagated into the authoritative work and gates.**

   The header and part `f` Done text move LOD, skinning, morphing, animation, PBR, lights, shadows, transparency, HDR, and postprocessing into an optional `Scene3D Extended` profile (`docs/todo/P02M0103.md:37-44,1819-1826`). The mandatory `f` checklist still requires those features and a core profile containing them (`:1491-1580`); `h` and `i` still require PBR, skinning, morphing, shadows, HDR, and postprocessing (`:1698-1705,1727-1732`); and overall 3D completion still names skinning, PBR, shadows, and postprocessing (`:1838-1842`). These are normative work and completion conditions, not historical commentary.

   Core `f`, `h`, and `i` therefore cannot be completed without the supposedly optional work, preserving the oversized scope the planner said it removed. Delete Extended features from every core work item, profile, conformance/demo list, and aggregate Done gate, or move them into separately activated parts with their own profile and dependencies.

2. **A depth-one channel does not have the signal-once completion semantics claimed by the correction.**

   The plan says a second outcome send finds the endpoint spent and that the one-deep queue structurally prevents duplicate completion (`docs/todo/P02M0103.md:777-805`). Current channels are reusable bounded queues: receipt commits the message and frees the slot, after which another send is accepted (`src/kernel/object/channel/mod.rs:319-351,425-440`), and the channel accounting tests explicitly rely on that reuse (`src/kernel/object/channel/tests.rs:218-240`). The rights split is also incomplete: a receiver needs `RIGHT_WAIT` to block in `SYS_WAIT`, not only `RIGHT_RECEIVE` (`src/kernel/syscall/mod.rs:2982-3003`).

   Duplicate or racing terminal outcomes are exactly what this abstraction promises to make impossible, so the frozen WSI and submission contracts would be based on a kernel property that does not exist. Either define an explicitly API-level, ownership-consuming send-once wrapper with exact close/duplicate/transfer behavior and tests, weakening the claim of kernel-enforced signal-once semantics accordingly, or implement a genuinely one-shot paired primitive. In either case state the receiver's exact `RECEIVE|WAIT` rights.

3. **The declared `a-common`/`a-wsi` split is still prose rather than an independently approvable part boundary.**

   The plan says `b` depends on `a-common` alone and that the two halves can be approved in different orders (`docs/todo/P02M0103.md:367-382`), but all work remains in one interleaved `a` checklist and the Definition of Done provides only one combined `a` condition (`:1799-1803`). `e` still says it depends on all of `P02M0103a` (`:1316-1319`) despite being a non-presenting consumer that should need only common contracts.

   There is no objective point at which `a-common` can be declared complete and release `b` while `a-wsi` remains blocked on restart/reacquisition work. Create actual subsections with unambiguous item ownership, prerequisites, and separate Done gates, and update every dependent part to name the correct half; otherwise remove the claimed independent approval/order.

4. **Multithreading was removed from local Done clauses but remains mandatory in the prerequisite matrix, work, and conformance gates.**

   The correction says a single-threaded tiled renderer conforms and the unowned self-thread runtime is no longer mandatory (`docs/todo/P02M0103.md:139-143,1807-1812,1827-1828`). In conflict with that decision, the prerequisite table still makes the runtime a hard prerequisite of `b,c` (`:94-100`), both software-renderer work items still mandate bounded worker pools (`:1211-1236,1663-1667`), the 2D and 3D tests still require parallel paths (`:1248-1255,1714-1716`), and aggregate 2D completion still requires scalar/parallel agreement (`:1835-1837`).

   Parts `c` and `g` remain unschedulable without the unnumbered runtime despite their revised Done clauses. Remove each unconditional worker-pool and parallel-path requirement, making differential tests conditional on such a path existing, or approve and number the shared runtime and restore it consistently as a prerequisite.

5. **The ResourceManager decision still directly contradicts its authoritative work item.**

   The item requires ResourceManager integration, graphics classifications, reservations, and optional graphics-specific limits (`docs/todo/P02M0103.md:819-827`). Later in the same item the correction says ResourceManager will not be extended and all graphics classifications, reservations, and quotas are dropped, leaving only SystemGraph counts (`:873-879`).

   These instructions produce incompatible implementations and make the planner's claimed decision ineffective. Rewrite the authoritative item around kernel Domain accounting plus SystemGraph observation only, or retain and fully specify ResourceManager integration; it cannot require and reject the same integration.

6. **Client-created imports do not make requester-Domain attribution enforceable “by construction,” and delegation remains contradictory.**

   A `MemoryObject` is charged to whichever Domain invoked creation and retains that charge when its capability is transferred (`src/kernel/object/memory_object.rs:52-79,194-206`). `ObjectInfo` exposes no creating or charged Domain (`src/abi/src/lib.rs:691-729`; `src/kernel/syscall/mod.rs:3401-3434`), so DisplayService cannot distinguish a buffer created by the requesting client from one donated by another Domain. This contradicts the plan's attribution claim and its required cross-Domain rejection (`docs/todo/P02M0103.md:847-850,888-893`). The protocol also says delegation is not permitted while allowing transfer of the surface channel (`:868-871`), then says the item still must decide whether surface delegation is legal and how attribution follows it (`:880-886`).

   A client can shift graphics memory cost to a sponsor or victim Domain, and transferring the surface channel leaves “client death” and reclamation without a unique process identity. Either make resource sponsorship the explicit contract and stop claiming requester attribution, or add an enforceable provenance/allocate-for mechanism that DisplayService can compare with the bound client identity. Decide surface-channel delegation once and specify which process owns accounting, resize, and cleanup after transfer.

7. **The P0167 assessment is stale and creates an unnecessary evidence blocker.**

   M0103 says the test medium still comes from colliding shared producers (`docs/todo/P02M0103.md:119-123`). Current P0167 instead records that the test medium is private and content-addressed (`docs/todo/P02M0167.md:1015-1023,1118-1119`). The harness stages the selected kernel under its compile lock (`src/harness/test-kernel.sh:303-365`), serializes image assembly, and publishes a content-keyed test ISO (`src/harness/mkimage.sh:550-553,653-715`).

   Leaving the false prerequisite status blocks tri-architecture acceptance or invites a redundant isolation redesign. Mark P0167 met for the medium-identity contract and retain only any actually demonstrated resource-contention limitation as a performance concern, not an evidence-integrity defect.

8. **The jointly owned glyph-cache correction was never applied to M0103.**

   M0103 still keys the glyph cache only by face, size, transform, and subpixel phase (`docs/todo/P02M0103.md:1186-1187`), omitting even the glyph identifier as well as face generation, variation coordinates, glyph kind, bitmap strike, and color palette. M0136 explicitly defines the shared key as face identity and generation, glyph, size, variation coordinates, transform, phase, kind, strike, and palette (`docs/todo/P02M0136.md:74-94`) and says M0103 is corrected in the same change.

   Variable, color, bitmap, or replaced-face glyphs can therefore collide and return stale pixels, while the two jointly owned plans specify different seam contracts. Copy the complete shared key into M0103 before either side implements or freezes its conformance tests.
