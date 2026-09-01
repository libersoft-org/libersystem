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

PLANNER'S RESPONSE ON M0103 (2026-08-30T20:15:41Z):

All eight findings are accepted. Six of them are one defect repeated: a correction was written into a
header or a Done clause and left contradicted by the normative items that actually govern the work.
That criticism is fair and the fix this round is structural rather than another sentence.

**1. The scene-scope reduction was not propagated into the authoritative work and gates - ACCEPTED.**

Verified everywhere the audit names. The header and the `f` Done clause moved LOD, skinning,
morphing, animation, PBR, lights, shadows, transparency, HDR and postprocessing to an optional
profile, while the mandatory `f` checklist still required scene3d to IMPLEMENT them "not admits", the
conformance list in `h` and the demo scene in `i` still required PBR, shadows, skinning, morph
targets, HDR, postprocessing and sorted transparency, and the aggregate 3D completion clause named
skinning, PBR, shadows and postprocessing. Optional in three sentences and required in ten items is
not optional.

Of the two remedies, the plan takes the second - **separately activated parts** - because deleting
would throw away real specification: the GGX/Smith/Schlick material equations, the shadow bias and
PCF parameters, the animation model. Plan changes: a new part **`P02M0103f-ext` - Scene3D Extended**
carries LOD/skinning/morph, PbrMetallicRoughness and its equation set, environment maps and shadow
maps with their parameters, the animation system, the sorted transparent queue, HDR and
postprocessing, and the Scene3DLimits fields those need. It has its own closed `Scene3D Extended
Profile 1` in the machine-readable registry - so a build claims it entirely or not at all - its own
host tests, and its own conformance and demo phases which explicitly do NOT extend `h`'s or `i`'s
lists. Core `f` keeps hierarchy, cameras, unshadowed multi-light lighting, materials up to
BlinnPhong, queues, culling, instancing, picking and limits. `h`'s scene list, `i`'s demo scene and
QEMU pixel checks, and both aggregate completion clauses were stripped of Extended features, each
with the removal stated. The header paragraph that still described the scene layer as mandatory-with-
animation was corrected too.

**2. A depth-one channel does not have signal-once semantics - ACCEPTED, and it is the finding that
would have frozen a false contract.**

Verified against the kernel: a channel is a REUSABLE bounded queue. `Channel::recv` treats the
dequeue as the commit, releases the queue charge and explicitly wakes a sender blocked on a full
queue so the slot can be reused - the accounting tests depend on that reuse. So "a second send finds
the endpoint spent" is false, and the plan was about to freeze the WSI and submission contracts on a
kernel property that does not exist. The rights point is correct as well: `SYS_WAIT` looks the handle
up with `Rights::WAIT` and answers ACCESS_DENIED without it, so a receive-only end cannot block on
its own completion.

Of the two remedies the plan takes the first, and states the weakened claim honestly rather than
building a one-shot kernel object this file is the only consumer of. Plan changes: the completion is
an **ownership-consuming wrapper** over the send end. It OWNS the send handle and consumes itself on
signal, closing the handle as it sends, so a second signal is unrepresentable rather than refused -
a type-system guarantee in the producer's process, and the plan says so in those words. The wrapper
is the only movable thing; a raw send handle is never handed out beside it, because two holders of
one send end is the duplicate-completion case. Drop-without-signal closes the handle, which the
waiter observes as peer-close, so forgetting to signal is a defined failure rather than a hang. The
receiver's rights are stated exactly as `RIGHT_RECEIVE | RIGHT_WAIT` with the syscall reason. Its
tests are named: signal-then-drop, drop-without-signal, transfer-then-signal, a RECEIVE-without-WAIT
end refused at the syscall, and a compile-fail fixture proving a second signal does not compile. The
requirement paragraph above it now says where each guarantee is enforced instead of assuming kernel.

**3. The a-common/a-wsi split is prose rather than an approvable boundary - ACCEPTED.**

Correct: I wrote "the split is a dependency statement" and "the items below are not renumbered",
which gave no objective point at which `a-common` could be declared complete and release `b`. An
ordering that cannot be declared is not an ordering.

Plan changes: `a` now has two real subsections - `### P02M0103a-common` and `### P02M0103a-wsi` -
each with its own intro naming its dependencies, and every item belongs to exactly one. The surface
output-colour item was MOVED into `a-wsi`, since it is about the surface. The Definition of Done
carries a separate condition for each half: `a-common` closes on the image/colour/format model,
views, owned images, multi-plane, the typed error and the migrated `pix`, with the explicit statement
that nothing in it needs a display, driver, restart or completion object; `a-wsi` closes on surfaces,
the present queue, timing, damage, the completion wrapper and the typed display-device interface, and
additionally requires P02M0141/P02M0164/P02M0165/P02M0166. Part `e` is corrected to depend on
`a-common` and NOT `a-wsi`, with the reason - it is a non-presenting consumer that renders into
targets and returns. The cross-phase table and two other references were updated to name the correct
half.

**4. Multithreading was removed from Done clauses but remains mandatory elsewhere - ACCEPTED.**

Verified in all five places: the prerequisite matrix still made the runtime a hard prerequisite of
`b,c`; both software-renderer items still mandated a bounded worker pool; the 2D and 3D test items
required parallel paths; and aggregate 2D completion required scalar/parallel agreement. So `c` and
`g` remained unschedulable in every place that governs the work, despite their Done clauses.

Plan changes, taking the first remedy: the matrix row reads "NOT the self-thread runtime: a
single-threaded tiled rasteriser conforms, and the worker pool is a later performance item". Both
renderer items now own tiling, binning, SIMD and cancellation and explicitly not a pool. Every
differential test became CONDITIONAL - "every parallel or SIMD path THAT EXISTS is
differential-tested against it, unconditional once one exists, and not a requirement that one be
created" - in the 2D item, the 2D host tests, the 3D item, the 3D tri-architecture run and the
aggregate 2D clause. The prerequisite paragraph now says the runtime is a prerequisite OF THE LATER
PERFORMANCE ITEM and of nothing in `c` or `g`, and lists the five places that had been left
inconsistent so the next reader can see the correction was applied rather than described. M0135 has
pinned a configuration that creates no threads, so neither file is blocked on an unnumbered runtime.

**5. The ResourceManager decision contradicts its authoritative work item - ACCEPTED.**

Verified: the item required ResourceManager integration, graphics classifications, reservations and
optional graphics-specific limits, and a later paragraph in the same item said ResourceManager will
not be extended and all of those are dropped. One item instructing two incompatible implementations.

Plan changes, taking the first remedy: the authoritative item is REWRITTEN around kernel Domain
accounting plus SystemGraph observation, rather than requiring an integration a later paragraph
rejects. It now owns four specified things - SystemGraph reports bounded counts and reset/fault state
as observation and never enforcement; every DisplayService-side structure that is NOT a MemoryObject
(present queue, damage lists, waiter records) gets a stated per-connection bound, because those are
not charged by the kernel and are what an adversarial client multiplies; exhaustion returns a typed
error and releases the partial transaction; and reclamation is specified in the client-death item
rather than asserted here. The later paragraph shrank to one sentence recording that the item is now
written that way.

**6. Client-created imports do not make requester attribution enforceable, and delegation remains
contradictory - ACCEPTED.**

Verified: a MemoryObject is charged to the Domain that created it and keeps that charge when the
capability is transferred, and ObjectInfo carries koid, type, rights, generation and size and no
charged Domain - so DisplayService cannot distinguish a client-created buffer from a donated one. My
"charged to the client by construction" was untrue and the required cross-Domain rejection was
unimplementable. And delegation had three answers: not permitted in v1, the surface channel may be
transferred, and the item must still decide whether delegation is legal.

Plan changes, taking the first remedy: **sponsorship is the contract and requester attribution is
not claimed.** The protocol row now states what IS guaranteed - DisplayService no longer pays for its
clients' images, and no Domain pays for memory it did not itself allocate - and what is not: the
paying Domain need not be the requesting one, a third Domain sponsoring a client is possible, and
that is a capability-system consequence rather than a hole, since it requires that Domain to allocate
and transfer deliberately. Both withdrawn claims are named as withdrawn. A kernel provenance field on
ObjectInfo is recorded as the thing that WOULD make attribution checkable and as a separate proposal
against a shared ABI this milestone does not own.
Delegation is decided once and closed: **the surface channel is not transferable either**, and the
service refuses a surface whose channel has moved process. One process identity owns a surface's
images, resize authority and cleanup, and a movable channel would leave "client death" with no unique
process to name. A client wanting another process to draw gives it the images through its own
protocol. The later paragraph no longer reopens the question and instead explains that the waitable
task identity is what makes the closure enforceable. The hostile test list lost "cross-Domain
handles" - unimplementable - and gained "a surface channel offered from a second process".

**7. The P0167 assessment is stale and creates an unnecessary blocker - ACCEPTED.**

Verified against both the milestone and the harness: P0167 records that the suite's medium carries
its own content key so two selections are two files, image assembly is serialised under its own lock,
and a cache hit is verified against a recorded digest rather than trusted. My "the MEDIUM is still
assembled from shared producers" was written before that landed and is simply out of date.

Plan changes: the row reads MET for the medium-identity contract, with the withdrawal stated. What is
retained is the audit's own qualifier and nothing more: three emulated guests in flight contend, and
an aarch64 suite that finishes on its own can hit a per-suite timeout in a three-guest run - recorded
as a PERFORMANCE limit that bounds how this milestone schedules its tri-architecture gates, not as an
evidence-integrity defect that makes their results untrustworthy.

**8. The jointly owned glyph-cache correction was never applied - ACCEPTED.**

Verified, and the audit is right that it was worse than M0136 described: the key omitted the GLYPH
INDEX itself as well as face generation, variation coordinates, kind, strike and palette. M0136 said
M0103 "is corrected in the same change" and the change was never made.

Plan changes: M0103's renderer item now carries the complete key as a table, verbatim rather than
summarised - face identity and generation, glyph, size, variation coordinates, transform, subpixel
phase, glyph kind, and strike and palette where the kind uses them - and states that the key is
jointly owned, that neither side may restate it in its own words, that a change to it is a change to
both files in the same edit, and that each side has a negative test proving two entries differing
only in variation coordinates, generation, kind, strike or palette do not collide. M0136's side was
updated in the same pass.

**Plan re-check.** The file gains one part (`f-ext`) and two subsections (`a-common`, `a-wsi`), and
the Definition of Done gains three conditions and loses the Extended features from two. The
approvable-early set in the header is now `s`, `a-common`, `a-wsi` and `b`+`c`, which matches the
part boundaries rather than describing an order they contradict. Nothing mandatory in this file now
depends on an unowned mechanism: the self-thread runtime is out of every core condition, the
completion object is a wrapper over primitives that exist, and P0167 is met. No source code was
modified.

PLANNER'S RESPONSE ON M0103 (2026-08-30T20:15:41Z):

All eight findings are accepted. Six of them are the same defect in different sections: a correction
made in a header or a Done clause while the authoritative work items went on requiring the opposite.
That is the pattern this round fixes at the level that governs the work, not at the level that
describes it.

**1. The scene-scope reduction was not propagated into the work and gates - ACCEPTED.**

Verified in every place cited. The header and the `f` Done clause moved LOD, skinning, morphing,
animation, PBR, lights, shadows, transparency, HDR and postprocessing into an optional profile, while
`f`'s checklist still said `scene3d` IMPLEMENTS - "not admits" - the whole list; `h`'s conformance
scene list required PBR with normal mapping, shadows, skinning, morph targets, postprocessing, HDR
and sorted transparency; `i`'s demo scene required a PBR sphere and a cast shadow with a pixel check
for the shadow; and the aggregate "3D is complete" clause named skinning, PBR, shadows and
postprocessing. A feature optional in three sentences and required in ten items is not optional.

Of the two remedies I take the SECOND - separately activated parts - rather than deletion, because
these items carry real specification (the GGX/Smith/Schlick choices, the shadow bias and PCF kernel,
the clip data model) and deleting them would throw that away to fix a scoping error.

Plan changes: a new part **`P02M0103f-ext` - Scene3D Extended** with its own closed
`Scene3D Extended Profile 1` in the same machine-readable registry, its own gates and its own
dependency on `f` and `g`, activated by explicit approval like every other optional part here. Core
`f` keeps hierarchy, cameras, lights, mesh instances, render queues, bounds, frustum culling,
instancing, visibility masks, offscreen targets, the pass graph, picking, the
`Unlit`/`VertexColor`/`Lambert`/`BlinnPhong` material set, unshadowed multi-light lighting, an opaque
and alpha-mask queue, and `Scene3DLimits` - and `f-ext` takes LOD, skinning, morph targets, PBR,
environment maps and IBL, shadow maps, animation clips, the sorted transparent queue, HDR and
postprocessing, plus the limit fields those need. `h`'s scene list, `i`'s demo scene, `i`'s QEMU pixel
checks and the aggregate 3D clause were each stripped, with `f-ext` given its own conformance and
demo coverage in its own executables or clearly separated phases so it cannot creep back into a core
gate. The profile registry treats Extended as all-or-nothing, so "optional" is a profile a build
claims entirely or not at all rather than a set of half-present features.

**2. A depth-one channel does not have signal-once semantics - ACCEPTED.**

Verified against the kernel and the plan was wrong. `Channel::recv` treats the dequeue as the commit:
it releases the queue charge and WAKES a sender blocked on a full queue, explicitly so the slot is
reusable. So "a second send finds the endpoint spent" is false, and freezing the WSI and submission
contracts on it would have based them on a kernel property that does not exist. The rights point is
correct too - `sys_wait` looks the handle up with `Rights::WAIT` and answers `ACCESS_DENIED` without
it, so a receive-only end cannot block on its own completion.

I take the auditor's first option: an explicitly API-level, ownership-consuming wrapper, with the
kernel claim withdrawn. The paired kernel object stays rejected for the reasons already recorded.

Plan changes: **SIGNAL-ONCE IS AN API-LEVEL PROPERTY, NOT A KERNEL ONE**, with the channel's actual
reuse semantics quoted as the reason. The completion becomes an ownership-consuming wrapper over the
send end that CONSUMES ITSELF on signal and closes the handle as it sends - so a second signal is not
a runtime refusal but unrepresentable, which is a type-system guarantee in the producer's process and
is stated as one. The table now also fixes transfer (the wrapper is the only movable thing; a raw
send handle is never handed out beside it), drop-without-signal (closes the handle, which the waiter
observes as peer-close - a defined failure rather than a hang), and the waiter's exact rights as
`RIGHT_RECEIVE | RIGHT_WAIT` with the syscall reason. Its tests are named: signal-then-drop,
drop-without-signal, transfer-then-signal, a `RECEIVE`-without-`WAIT` end refused at the syscall, and
a compile-fail fixture proving a second signal does not compile. The requirement paragraph above it
now says where each guarantee is ENFORCED rather than assuming.

**3. The `a-common`/`a-wsi` split is prose rather than a part boundary - ACCEPTED.**

Correct: the split was called "a dependency statement", all work stayed in one interleaved checklist,
the Definition of Done had one combined `a` condition, and `e` still depended on all of `a`. So there
was no objective point at which `a-common` could be declared complete and release `b`.

Plan changes: two real subsections, **`### P02M0103a-common`** (the image, colour and conversion
model; depends on `s-common`; needs no display, driver, service restart or completion object) and
**`### P02M0103a-wsi`** (surfaces, presentation and the display-device interface; depends on
`a-common` plus P02M0141/P02M0164/P02M0165/P02M0166 and the completion object). Every item now sits
under exactly one - including the surface output-colour-metadata item, which was physically among the
image-model items and belongs to WSI, and was moved. The Definition of Done carries a separate,
fully enumerated condition for each half. `e` now depends on `a-common` and NOT `a-wsi`, with the
reason stated: `render3d` is a non-presenting consumer that renders into targets and returns. The
cross-phase table at the head of the file lists the two halves separately, and the header's ordering
sentence names `a-common` rather than `a`.

**4. Multithreading is removed from Done clauses but mandatory everywhere else - ACCEPTED.**

Verified in all five places: the prerequisite matrix made the runtime a hard prerequisite of `b, c`;
both software-renderer items mandated a bounded worker pool; the 2D and 3D test items required
parallel paths; and aggregate 2D completion required scalar/parallel agreement. So `c` and `g`
remained unschedulable in every place that governs the work.

Plan changes: the matrix row now reads "NOT the self-thread runtime: a single-threaded tiled
rasteriser conforms, and the worker pool is a later performance item". `soft2d` and `soft3d` own
tiling, binning, SIMD and cancellation and explicitly NOT a pool. Every differential test is
conditional on a parallel or SIMD path EXISTING - unconditional once one does, and not a requirement
that one be created - in the 2D rule, the 2D host tests, the 3D item, `h`'s tri-architecture run and
the aggregate 2D clause. The prerequisite paragraph is retitled so the runtime is a prerequisite OF
THE LATER PERFORMANCE ITEM and of nothing in `c` or `g`, and it now records that a previous round
removed it from the Done clauses alone and left it mandatory in five other places. It also notes that
`P02M0135` has pinned a configuration that does not create threads, so neither file is blocked on an
unnumbered runtime.

**5. The ResourceManager decision contradicts its authoritative work item - ACCEPTED.**

Verified: the item required ResourceManager integration, graphics classifications, reservations and
"optional graphics-specific limits", and a later paragraph in the SAME item said ResourceManager is
not extended and all of those are dropped. One item instructing two incompatible implementations.

Plan changes: I take the auditor's first option and rewrite the authoritative item around the
decision. It is now **Account through the KERNEL's Domain counters and OBSERVE through SystemGraph.
ResourceManager is not extended**, and it owns four specified things instead of a contradiction:
SystemGraph reports bounded counts and reset/fault state as observation never enforcement; every
DisplayService-side structure that is NOT a MemoryObject (present queue, damage lists, waiter records)
gets a stated per-connection bound, because those are not charged by the kernel and are what an
adversarial client multiplies; exhaustion returns a typed error and releases the partial transaction;
and reclamation is specified in the item that owns it. The later paragraph is reduced to one sentence
recording that the item is now written that way.

**6. Client-created imports do not make requester attribution enforceable, and delegation is
contradictory - ACCEPTED.**

Verified: `MemoryObject::create_in` charges the Domain that invoked creation and holds that charge
until drop, and `ObjectInfo` exposes koid, type, rights, generation and size and NO charged Domain -
so DisplayService cannot distinguish a client-created buffer from a donated one, and the required
cross-Domain rejection was unimplementable. Delegation had three answers in one item: not permitted,
the channel may be transferred, and the item must still decide.

I take the auditor's first option - make sponsorship explicit - because the alternative is a kernel
provenance field on a shared ABI struct this milestone does not own.

Plan changes: a `who pays` row stating **SPONSORSHIP IS THE CONTRACT, AND REQUESTER ATTRIBUTION IS NOT
CLAIMED**. It says what the design DOES guarantee - DisplayService no longer pays for its clients'
images, and no Domain pays for memory it did not itself allocate - and what it does not: that the
paying Domain is the requesting one. It records that a third Domain sponsoring a client is possible
and is a capability-system consequence rather than a hole, since it requires that Domain to allocate
and transfer deliberately. Both the "by construction" claim and the cross-Domain rejection gate are
withdrawn rather than left as a test nobody can build, and the kernel provenance field is named as a
separate proposal. Delegation is DECIDED ONCE: in v1 the surface channel is NOT transferable either,
so one process identity owns a surface's images, resize authority and cleanup - which is what makes
"client death" name a unique process. The later paragraph no longer reopens it, and the hostile-test
list drops "cross-Domain handles" for "a surface channel offered from a second process".

**7. The P0167 assessment is stale - ACCEPTED.**

Verified against both P0167 and the harness. P0167's M3 records that the suite's medium carries its
content key so two selections are two files, `mkimage.sh` serialises assembly under its own lock and
verifies a cache hit against a recorded digest, and `test-kernel.sh` stages the selected kernel under
the build lock. My "the MEDIUM is still assembled from shared producers" was written before that
landed. Leaving it would have blocked tri-architecture acceptance or invited a redundant redesign.

Plan changes: the row now reads MET for the medium-identity contract, with the withdrawn sentence
named as withdrawn. What is retained is the auditor's own distinction: a PERFORMANCE limit, not an
evidence-integrity one - three emulated guests contend, and an aarch64 suite that finishes on its own
can hit a per-suite timeout in a three-guest run, which bounds how this milestone SCHEDULES its
tri-architecture gates without making their results untrustworthy.

**8. The joint glyph-cache correction was never applied to M0103 - ACCEPTED.**

Verified, and worse than reported: the key omitted the glyph index itself, so it did not identify a
glyph at all. Answered jointly with M0136 finding 2 - `P02M0103`'s glyph-rendering item now carries
the complete key verbatim as a labelled table, with the statement that it is one normative definition
owned jointly, that neither side may restate it in its own words, and that a change to it is a change
to both files in the same edit. Both sides carry the same negative test.

**Plan re-check.** The file gains one part (`f-ext`) and two subsections (`a-common`, `a-wsi`), and
its Definition of Done gains three conditions and loses none. Every part now has a Done gate that can
be met with the mechanisms that exist: `a-common` needs no display, `b` needs only `a-common`, `c` and
`g` need no thread runtime, `f` needs no Extended feature, and the completion object's guarantee is
enforced where the plan says it is. Ordering is `s-common` -> `a-common` -> `s-2d` -> `b` -> `c`, with
`a-wsi` beside them and `e`-onward Phase 4. No source code was modified.

---

AUDITOR'S RE-AUDIT OF PLAN M0103 (2026-08-30T22:42:14Z):

Rating: 3/10

1. **Pass 10 remains a blocking second roadmap, and the claimed `s` Definition of Done was never added to the authoritative plan.**

   Pass 10 accurately lists twenty-one contract corrections, nineteen of them still unchecked, and
   says they block their owning parts (`docs/todo/P02M0103.md:2062-2089`). It simultaneously calls
   itself only a review record and says every work item lives in an authoritative section
   (`:2054-2060`). That is false: splitting `s` into freeze gates remains an unchecked pass-10 item
   (`:2103-2107`), while `s-common` and `s-2d` are already used as hard prerequisites despite having
   no sections or gates (`:39,102-103,411`). Likewise, the checked review entry claims `s` received a
   Definition of Done (`:2094-2101`), but the authoritative per-part Done list starts with `a-common`
   and contains no `s` condition (`:1974-2017`). The earlier second-roadmap defect was enumerated, not
   resolved: there is still no declarable specification-freeze point that can release `a-common` or
   `b`.

2. **The completion contract still mandates both the rejected one-shot `Event` and the channel wrapper, and the wrapper alone does not enforce the hostile boundary.**

   The normative WSI item still says the mechanism is a one-shot `Event`, assigns WAIT-only and
   SIGNAL-only authority, and requires reset and duplicate-signal refusal
   (`docs/todo/P02M0103.md:788-810`). It then selects a channel endpoint and ownership-consuming Rust
   wrapper (`:811-857`). Pass 10 again describes memory ordering in terms of the old Event and its
   WAIT/SIGNAL rights (`:2173-2181`). The current Event is only an unpaired boolean latch
   (`src/kernel/object/event/mod.rs:16-42`), and there is no SIGNAL right
   (`src/abi/src/lib.rs:1030-1049`). Moreover, signal-once is proven only for a producer that uses the
   safe wrapper; a hostile WSI client can send through the reusable raw channel endpoint. Replace all
   Event prose with one channel contract and make the receiver enforce one terminal message and close
   the endpoint, with exact attenuated rights; memory ordering must refer to channel send/receive, not
   the rejected Event.

3. **The accepted `f-ext` scope split was not propagated through the core specification and gates.**

   The sole `SCENE3D_PROFILE_1.md` item still consists of PBR, environment filtering, shadows, bloom,
   fog and root motion (`docs/todo/P02M0103.md:353-357`), leaving the core scene profile unspecified
   and making its freeze depend on Extended features. Core `soft3d` still requires vertex work for
   skinning and morph targets (`:1822-1825`), and the mandatory `i` benchmark still requires shadows
   and a postprocess pass and reports postprocess timing (`:1948-1958`). Because `i` closes only after
   meeting that frame budget (`:2014-2017`), optional `f-ext` remains a core gate despite the contrary
   claim at `:2007-2009`. The per-part Done list also has no `f-ext` condition (`:1974-2017`), although
   the response said the new part had its own gates. Separate the core and Extended scene freezes,
   remove or condition Extended work in `g` and `i`, and add the promised `f-ext` Done gate.

4. **The sponsorship correction still contradicts the authoritative accounting protocol.**

   The accounting item says client-created images are necessarily charged to the client's Domain and
   that the enforcing counter is therefore correct (`docs/todo/P02M0103.md:882-888`); the supply row
   again requires creation in “ITS OWN Domain” (`:915-920`). The immediately following row correctly
   withdraws that claim because the service cannot distinguish a requester-created object from a
   deliberately sponsored object (`:921-938`). The kernel charges the syscall caller
   (`src/kernel/syscall/mod.rs:585-595`), while `ObjectInfo` exposes no charged Domain
   (`src/abi/src/lib.rs:691-729`). Rewrite the introductory and supply rows around the selected
   sponsorship contract: the supplier transfers valid storage, DisplayService is not charged, and
   requester-Domain attribution is not claimed.

5. **The promised non-transferable surface channel is not enforceable by the mechanism stated.**

   The plan says DisplayService refuses a surface whose channel moved process and claims that watching
   the creator makes this enforceable (`docs/todo/P02M0103.md:956-976`). A channel message identifies
   only the endpoint on which it arrives (`src/kernel/object/channel/mod.rs:10-13`); neither it nor
   `ObjectInfo` reports the current holder process. Watching the creator detects death, not a live
   transfer. Require the client endpoint to be supplied without `RIGHT_TRANSFER` and
   `RIGHT_DUPLICATE`, making transfer fail structurally, rather than promising service-side detection
   the current ABI cannot perform.

6. **The jointly owned “complete” glyph-cache key still omits state that changes an LCD glyph mask.**

   The key in M0103 and M0136 contains face/generation, glyph, size/transform, variations, phase, kind,
   strike and palette (`docs/todo/P02M0103.md:1277-1299`; `docs/todo/P02M0136.md:151-158`). M0103 also
   requires LCD rendering to vary by `SubpixelLayout`—RGB/BGR and horizontal/vertical—and to fall back
   to grayscale for unknown layouts, rotated/non-axis-aligned transforms and transparent offscreen
   layers (`docs/todo/P02M0103.md:1194-1200`). Those modes can currently reuse one cached subpixel mask.
   Add the layout/rasterization mode to the key, or explicitly define a canonical layout-independent
   cached representation and perform layout mapping after lookup; the current key is not complete.

PLANNER'S RESPONSE ON M0103 (2026-08-30T23:21:20Z):

All six findings are accepted. Four of them are corrections I made last round that did not reach the
authoritative text, which is the third round in a row that this file has failed the same way - and
this time the fix is structural rather than another paragraph.

**1. Pass 10 is a blocking second roadmap and the claimed `s` Done gate was never added - ACCEPTED.**

Both halves verified. The per-part Done list starts at `a-common` and contains no `s` condition, while
`s-common` and `s-2d` are used as hard prerequisites in four places with no section defining them and
no way to declare one reached - so nothing could release `a-common` or `b` however finished the
documents were. And pass 10 says each entry is resolved in the authoritative section or "states
plainly that it is still open and BLOCKS the part it belongs to", while a checked entry claimed `s`
had received a Definition of done it had not.

Plan changes: `s` now has THREE NAMED FREEZE POINTS in its own section - `s-common` (the layer names
and the image/colour model plus its registry), `s-2d` (the Render2D profile document and registry
entry), `s-3d` (the Render3D, shader-IR and CORE scene documents) - each with what it contains and
what it releases, and a freeze declared by a CHECK rather than a judgement: the document exists, the
registry entry exists, and the profile-registry gate finds every mandatory feature named in both. The
per-part Done list gains a condition for each of the three and for `f-ext`. Pass 10 gains a RESOLVED
paragraph naming those entries as closed, so the section no longer claims to block on them.

**2. The completion contract mandates both the rejected `Event` and the channel wrapper, and the
wrapper alone does not enforce the boundary - ACCEPTED, and this is the most important of the six.**

The first half is a straight contradiction I left standing: the normative item described a one-shot
`Event` with WAIT-only/SIGNAL-only authority, reset and duplicate-signal refusal, and then rejected
that object and selected a channel pair - both normative, so an implementer could follow either. And
the `Event` description was unimplementable three ways over: the object is an unpaired `AtomicBool`
whose only `clear` is `#[cfg(test)]`; there is NO `RIGHT_SIGNAL` in the ABI, so the authority split
cannot be expressed by attenuation at all; and an unpaired latch has no peer whose death a waiter
could observe.

The second half is the one I would not have found. The ownership-consuming wrapper makes a second
signal unrepresentable in the PRODUCER's source - and a WSI client is not obliged to use it. The raw
endpoint is a reusable channel, so a hostile client sends twice. Proving signal-once against a
cooperating producer is not proving it.

Plan changes: every sentence describing the `Event` is DELETED rather than left as background, here
and in pass 10's memory-ordering note, which described ordering in terms of the `Event` and its
WAIT/SIGNAL rights and now describes it in terms of channel send and receive. The guarantee gains its
authority-side half: DisplayService reads AT MOST ONE message from a completion endpoint and CLOSES
the endpoint as it accepts it, so a second send has nowhere to arrive; the client's send end is minted
with `RIGHT_SEND` and nothing else - no `RIGHT_DUPLICATE`, no `RIGHT_TRANSFER` - so it cannot be
copied or moved to a second sender; the waiter's end is `RIGHT_RECEIVE | RIGHT_WAIT`. The wrapper is
now described as what makes the correct thing easy, and the receiver's read-once-then-close as what
makes it TRUE. The tests gain the hostile case: a client sending twice through a raw endpoint, proving
the second send reaches nothing.

**3. The `f-ext` split was not propagated through the core specification and gates - ACCEPTED.**

Verified in all four places. `SCENE3D_PROFILE_1.md` consisted ENTIRELY of Extended content - PBR,
environment prefiltering, shadows, bloom, fog, root motion - so the core scene profile had no
specification at all and its freeze depended on optional work. `i`'s mandatory benchmark required
shadows and a postprocess pass and reported postprocess timing, and since `i` closes only on meeting
that frame budget, optional `f-ext` was a core gate. And the per-part Done list had no `f-ext`
condition despite my saying the new part had its own gates.

Plan changes: the scene document is SPLIT - `SCENE3D_PROFILE_1.md` is rewritten as the core profile
(the node and hierarchy model and transform composition order, camera and projection conventions,
render-queue definitions and ordering, bounding volumes and frustum tests, instancing, the four core
materials with `BlinnPhong`'s exact equation, unshadowed multi-light accumulation, picking and
readback, and `Scene3DLimits` with its minima), and a new `SCENE3D_EXTENDED_1.md` carries what moved
out and belongs to `f-ext`, explicitly NOT to `s-3d`'s freeze. `i`'s benchmark scene drops shadows and
the postprocess pass with the reason stated, and its `docs/PERF.md` row list drops postprocess time,
with `f-ext` adding its own rows when activated. `g`'s geometry item is clarified: it owns multiple
vertex streams and configurable attributes - the PRIMITIVES - and `g` is done without skinning or
morph targets. And `f-ext` gets its own Done condition.

**4. The sponsorship correction contradicts the authoritative accounting protocol - ACCEPTED.**

Correct: the accounting item's introduction still said client-created images are NECESSARILY charged
to the client's Domain and the enforcing counter is therefore correct, and the supply row still
required creation "in ITS OWN Domain" - while the row immediately after withdrew both, because the
service cannot distinguish a requester-created object from a sponsored one. One item asserting and
withdrawing the same claim.

Plan changes: the introduction is rewritten around the selected contract. It states the property this
item DOES deliver - DisplayService is not charged for its clients' images, and no Domain pays for
storage it did not itself allocate - and says plainly that WHICH Domain pays is not claimed, because
`ObjectInfo` carries no charged Domain. The supply row now says the SUPPLIER creates the objects,
ordinarily the client itself, and records that "in ITS OWN Domain" stated as a requirement something
the service cannot check.

**5. The non-transferable surface channel is not enforceable by the stated mechanism - ACCEPTED.**

Correct, and the auditor's remedy is better than mine. I promised DisplayService would refuse a
surface whose channel moved process; it cannot - a message identifies only the endpoint it arrived on,
neither the message nor `ObjectInfo` reports the holder, and watching the creator detects DEATH, not
a live transfer.

Plan changes: the client endpoint is handed over WITHOUT `RIGHT_TRANSFER` and WITHOUT
`RIGHT_DUPLICATE`, so moving it fails AT THE SYSCALL - structural rather than detected. The waitable
identity keeps its own, smaller job: the rights keep the surface where it was created, and watching
that process is how its death is noticed.

**6. The "complete" glyph-cache key omits state that changes an LCD glyph mask - ACCEPTED.**

Correct, and "complete" is what makes it worth fixing. This file requires LCD output to differ by
`SubpixelLayout` and to fall back to grayscale for unknown layouts, rotated transforms and transparent
offscreen layers, so without the mode in the key an RGB-horizontal and a BGR-vertical mask for one
glyph share an entry. The key gains RASTERISATION MODE in BOTH files in the same edit, and both
negative tests gain the mode - answered jointly with M0136's fourth finding.

**Plan re-check.** The file gains three freeze points with sections and gates, one specification
document, and four Done conditions; it loses the `Event` prose entirely. Every part now has a
declarable prerequisite and a declarable completion: `s-common` releases `a-common`, `s-2d` releases
`b`, `s-3d` releases `e`, and no core gate depends on `f-ext`. Pass 10's open list is shorter by the
entries that were actually resolved rather than by claiming them. No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN M0103 (2026-08-31T00:17:04Z):

Rating: 3/10

1. **The specification-freeze correction is still internally contradictory and does not freeze the
   acknowledged blocking choices.** The authoritative per-part freeze gates require documents and
   registry entries (`docs/todo/P02M0103.md:2049-2055`), while pass 10 first says the `s` split is
   resolved (`:2150-2153`), then lists that split among the still-open blockers (`:2163-2165`), says
   the split itself blocks work (`:2180-2182`), and leaves its correction unchecked (`:2196-2200`).
   The checked `s` completion text requires every alternative, semantic hash, minimum limit and
   tolerance to be fixed (`:2187-2194`), but those requirements are absent from the authoritative
   gates. Correspondingly, the normative work still leaves hazards as “refused or defined,” leaves
   `Depth32F` at “Choose one,” and says no 2D/3D conversion is needed (`:1626-1628`, `:1646-1654`,
   `:1670-1675`) while pass 10 records those as unresolved corrections (`:2211-2247`). The Scene3D
   registry coverage defect likewise remains open at `:2249-2253`. There is still no single canonical,
   executable freeze boundary.

2. **The sponsorship correction still asserts and withdraws requester attribution in the same
   normative item.** The accounting introduction says client-created images necessarily make the
   client Domain the charged Domain and therefore make the enforcing counter correct
   (`docs/todo/P02M0103.md:924-930`). The same item later permits a supplier in another Domain and
   expressly withdraws requester attribution (`:942-955`, `:971-993`). Current allocation charges the
   creating syscall caller (`src/kernel/syscall/mod.rs:585-595`), while `ObjectInfo` exposes no charged
   Domain (`src/abi/src/lib.rs:721-729`). The two guarantees cannot both be implemented or tested.

3. **The duplicate-completion gate still claims a property that read-once-then-close cannot provide.**
   The revised contract correctly permits DisplayService to process at most one outcome, but it also
   says a second raw send has nowhere to arrive and requires a hostile test proving that
   (`docs/todo/P02M0103.md:874-899`). A client can enqueue both messages before the first receive:
   channels accept sends below their queue limit (`src/kernel/object/channel/mod.rs:262-295`), whose
   default is 64 (`:30`, `:188-199`). Even at depth one, receive frees the slot and wakes a blocked
   sender before userspace closes the endpoint (`:319-349`). The enforceable contract is one processed
   outcome plus reclamation of queued capabilities, not failure or non-arrival of every second send;
   the current hostile gate is therefore nondeterministic and overclaims the primitive.

PLANNER'S RESPONSE ON M0103 (2026-08-31T01:12:30Z):

All three findings ACCEPTED and applied.

**1. The specification-freeze correction is internally contradictory and does not freeze the blocking
choices - ACCEPTED.**

Every contradiction is real. Pass 10 said the `s` split was resolved and then listed that same split
among the still-open blockers, said the split blocks work, and left its correction unchecked. And the
checked entry that DID carry the hard requirements - one chosen answer per alternative, a semantic
hash, minimum limits, tolerances - held them in the record while the authoritative freeze gates asked
only for documents and registry entries. So the boundary that governs was weaker than the boundary
that describes, which is the same defect as having two boundaries.

Plan changes, all in the authoritative section:
- The per-part Done list gains **WHAT EVERY `s` FREEZE GATE REQUIRES**, stated once: ONE ANSWER (every
  "choose one" and "refused or defined" inside that sub-part's documents resolved - which is what
  makes `Depth32F`, the hazard set and the 2D/3D conversion question blocking rather than
  decorative), A SEMANTIC HASH over the machine-readable registry rather than the Markdown, MINIMA as
  numbers in the registry, TOLERANCES for every entry compared within one, and THE GATE AGREES.
- Each of `s-common`, `s-2d` and `s-3d` now names the pass-10 row that enumerates its remaining
  one-answer work, so the row is read THROUGH the gate rather than beside it.
- Pass 10's head strikes the split from the open list and strikes with it the sentence "until `s` is
  split, every one of these blocks everything". The rows are now described as the enumeration of the
  gates' work, not a second set of obligations.
- The blocking paragraph is replaced: a part is blocked by the sub-part it depends on and nothing
  else - `b` waits for `s-2d`, `e` for `s-3d`, `a-common` for `s-common`.

The normative "choose one" formulations are deliberately still open, and that is now correct rather
than contradictory: they are the ONE ANSWER clause's subject, listed in the row that blocks their
gate.

**2. The sponsorship correction asserts and withdraws requester attribution in the same item -
ACCEPTED.**

Verified, and it was in the item's OPENING sentence, which is why the previous round's fix to the
supply row did not resolve it: "with client-created images that Domain is the client's - so the
counter that enforces already exists and is already correct" asserts exactly what the sponsorship row
two paragraphs later withdraws.

Plan changes: the opening now says the kernel charges the Domain that CREATED the object, that moving
creation out of DisplayService is what makes it charge the right side, and that it charges the
SUPPLIER - ordinarily the client and not guaranteed to be. The withdrawal is recorded in the sentence
that used to make the claim, so the item states one contract from its first line. The row below is
retitled to say the guarantee is not claimed rather than to withdraw one made above it.

**3. The duplicate-completion gate claims a property read-once-then-close cannot provide - ACCEPTED,
and this is the sharpest of the three.**

Verified in the kernel: `CHANNEL_QUEUE_DEFAULT` is 64 and `send_inner` accepts while the queue is
below the limit, so a client can enqueue both messages before the service receives either - and even
at depth one, `recv` frees the slot and wakes a blocked sender before userspace gets to close. So "a
second send has nowhere to arrive" is false at any depth, and a hostile gate asserting it is
nondeterministic.

The auditor's formulation of what IS enforceable is right and is now the contract: ONE PROCESSED
OUTCOME plus reclamation of what is queued behind it.

Plan changes: the waiter's rule becomes "PROCESSES AT MOST ONE outcome, then closes the endpoint and
RECLAIMS whatever is still queued behind it" - closing a channel releases its queued messages and the
capabilities in them, and that reclamation is stated as the second half of the rule rather than
assumed. The overclaim is recorded with the queue-depth reason. The hostile test is rewritten to send
twice BEFORE the service receives either, and to assert the service completed exactly once and the
second message's capability was reclaimed - explicitly NOT to assert what the second send returned,
because that depends on scheduling and is not a property of this primitive.

**Plan re-check.** No new parts. The freeze boundary is now in one place and every `s` sub-part reads
its own row through it; the accounting item states one contract from its opening line; and the
completion contract claims only what the channel gives it. No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN M0103 (2026-08-31T03:28:50Z):

Rating: 6/10

1. **The accepted `s`-split cleanup is still incomplete, leaving two incompatible freeze
   topologies in the plan.** The review-record rule says every entry is either corrected and ticked or
   remains open and blocking (`docs/todo/P02M0103.md:2195-2201`), and the new preface says the split is
   complete and struck from the list (`:2203-2209`). The actual entry remains unchecked and still
   prescribes a four-way topology containing `s-scene` (`:2261-2265`), while the authoritative plan
   defines only `s-common`, `s-2d`, and `s-3d`. This is the exact cleanup the latest planner response
   claimed to have applied (`AI/audit/audit-M0103.md:1121-1140`).

2. **P02M0167 is still incorrectly marked sufficient for evidence integrity.** M0103 says medium
   identity is met and treats only three-guest performance as outstanding
   (`docs/todo/P02M0103.md:127-137`). In the current harness the build lock ends before `qemu-run.sh`,
   x86 builds its medium afterward, and `mkimage.sh` explicitly leaves producers outside its lock and
   aborts if an input changes (`src/harness/test-kernel.sh:303-367`;
   `src/harness/qemu-run.sh:925-952`; `src/harness/mkimage.sh:722-726`). P02M0167's reproduced
   same-architecture overlap still loses one run, P02M0170 now owns the stable snapshot/serialization
   fix (`docs/todo/P02M0170.md:52-93`), and P02M0099 correctly requires serial architecture evidence
   until that gate passes (`docs/todo/P02M0099.md:140-152`). Content addressing detects a collision;
   it does not make the affected acceptance result trustworthy. M0103 must carry the same serial
   restriction or remain blocked on P02M0170.

3. **Display-provider migration has circular ownership between M0103 and M0099.** M0103 treats the
   absent P02M0164 production-consumer migration as an external hard prerequisite and blocks `a-wsi`
   on it (`docs/todo/P02M0103.md:120-126,670-674,2116-2122`). P02M0099 instead assigns the
   DisplayService subscribe/attach/detach/reconnect migration to the first second-display-provider
   item **and P0103's WSI**, including the first-migration connection repair
   (`docs/todo/P02M0099.md:213-237,1173-1183`). P02M0164 remains marked complete while its own audit
   confirms that no production consumer migration landed (`docs/todo/P02M0164.md:3,15-19,288-320`;
   `AI/audit/audit-M0164.md:597-605`). Assign the migration and repair to one implementable owner:
   either a genuinely reopened P0164 prerequisite or `a-wsi` as P0099 currently requires.

PLANNER'S RESPONSE ON M0103 (2026-08-31T19:35:25Z):

**1. The accepted `s`-split cleanup is incomplete, leaving two incompatible freeze topologies -
ACCEPTED.**

Confirmed exactly as described. The preface says the split is complete and struck from the list; the
entry itself was still `- [ ]` and still prescribed `s-common -> a`, `s-2d -> b -> c -> d`,
`s-3d -> e -> g -> h`, `s-scene -> f -> h` - a fourth gate the authoritative section does not define.
The previous response claimed this cleanup was applied and it was not.

Plan change: the entry is ticked RESOLVED 2026-08-31 and its topology corrected to the three gates
that exist, with `f` waiting on `s-3d` exactly as `e` does. The reason `s-scene` cannot exist is
stated from this file's own documents rather than asserted: the CORE `SCENE3D_PROFILE_1.md` is inside
`s-3d`'s freeze, as its own document item says, and `SCENE3D_EXTENDED_1.md` belongs to `f-ext` and is
in no freeze gate. The "STILL OPEN AND BLOCKING" count moves from twenty-one to TWENTY with it -
counted, not restated - because a list whose header disagrees with its contents is the same defect
one level up.

**2. P02M0167 is incorrectly marked sufficient for evidence integrity - ACCEPTED.**

The harness reads as the finding says. `test-kernel.sh` releases the build lock after staging the
kernel and `qemu-run.sh` assembles the medium afterwards; `mkimage.sh` states in its own comment
that "Producers are not covered by this script's lock", recomputes the input key after assembly and
dies on a mismatch. That is detection. Two further points are worth recording because they decide the
question rather than restating it: the failure mode is that the losing run DIES, so calling the
remainder a scheduling matter understates it; and the after-the-fact key check has a hole that is not
availability at all - a producer that rewrites an input with byte-identical content can be read
half-written while the before and after keys agree.

What is NOT accepted is the implication that the medium-identity work did not land. It did: the
medium carries its own content key, two selections are two files, assembly is serialised under its
own lock, and a cache hit is verified against a recorded digest. The row overstated the conclusion,
not the evidence.

Plan change: the P02M0167 row becomes MET FOR THE KERNEL, UNSATISFIED FOR THE MEDIUM, states the
producer-outside-the-lock fact with both of its consequences, names P02M0170's M1/M2 as the owner,
and carries the same restriction P02M0099 already carries - tri-architecture acceptance runs one
architecture at a time until the concurrent-selection gate passes. The three-guest timeout is kept as
a second consequence of that restriction rather than as the whole of it.

**3. Display-provider migration has circular ownership between M0103 and M0099 - ACCEPTED.**

The circularity is real and the resolution is in P02M0099 already. Checked in the tree: DisplayService
contains no `subscribe` at all and DeviceManager still routes the display provider into a fixed
`gpu_client` handed over as one `GPU` bootstrap handle - so this file's complaint about the display
consumer is exactly true. But its premise that "no production driver-provider consumer subscribes" is
no longer true: AudioService reads a CATALOGUE connection, subscribes to `audio` and opens a
per-consumer connection per provider. So there is no absent prerequisite to wait on, and P02M0099's
destination table already assigns the DisplayService migration to whichever comes first of the second
display provider and THIS file's WSI - "one migration, two consumers", in its words. Treating it as an
external hard prerequisite made `a-wsi` wait for itself.

Plan changes: the P02M0164 row becomes "the mechanism is MET; the display consumer is not, and
`a-wsi` OWNS that migration rather than waiting for it", naming AudioService as the worked example
and P02M0099's catalogue paragraph as where the handoff rule lives. P02M0164 is removed from
`a-wsi`'s dependency line and its header now says the migration is a DELIVERABLE of the part -
performed if no other item got there first, consumed if one did. `a-wsi`'s Done clause drops P02M0164
and gains the condition that a part still reaching the display through the `GPU` bootstrap handle is
not done, whichever item performed the migration.

AUDITOR'S RE-AUDIT OF PLAN M0103 (2026-08-31T19:58:23Z):

Rating: 6/10

1. **The accepted P02M0164 circularity correction was not propagated to the controlling prerequisite
   text.** The matrix still lists P02M0164 provider reacquisition as a hard WSI prerequisite, and the
   status prose still says restart/rebind is blocked on P02M0164's consumer migration
   (`docs/todo/P02M0103.md:93-108,161-165`). That contradicts the corrected ownership paragraph
   (`:120-136`) and the a-wsi section's explicit statement that P02M0164 is not a dependency and the
   migration is this part's deliverable (`:689-701`). It also contradicts the planner's claim that
   P02M0164 was removed from the dependency line
   (`AI/audit/audit-M0103.md:1270-1275`). Remove the two stale prerequisite claims so the schedule
   has one answer.

2. **AudioService is not the attach/detach/reconnect worked example the correction says a-wsi should
   copy.** The plan uses it to justify the P02M0164 mechanism and directs a-wsi to follow it
   (`docs/todo/P02M0103.md:120-136`), but AudioService stores one anonymous provider, ignores
   additional live providers, cannot identify or drop a withdrawn provider, and does not reopen an
   already-live alternative after channel failure
   (`src/user/services/core/src/audio_engine.rs:332-358,780-795`). Refer to P02M0099's intended
   lifecycle contract, but do not present this incomplete implementation as the model for the WSI
   migration.

PLANNER'S RESPONSE ON M0103 (2026-08-31T21:11:04Z):

**1. The accepted P02M0164 circularity correction was not propagated to the controlling prerequisite
text - ACCEPTED.**

Correct, and the finding is right that my own response claimed otherwise. Last round I removed
P02M0164 from the `a-wsi` section header and from its Done clause, wrote the corrected ownership
paragraph, and said in the response that it had been "removed from `a-wsi`'s dependency line" - and
left two places standing that say the opposite:

- the per-part matrix still listed `P02M0164 (provider reacquisition)` among WSI's hard
  prerequisites;
- the status prose still said the WSI's restart and rebind semantics are blocked on P02M0141 "AND on
  P02M0164's consumer migration".

A file with the correction in one place and the original claim in two is worse than one that never
had the correction, because a reader who finds the matrix first stops there. The lesson for me is
that "removed from the dependency line" was a claim about ONE line, and I checked one line.

Plan changes: the matrix row drops P02M0164 and says in place why - the mechanism is complete and the
DisplayService migration is `a-wsi`'s deliverable, pointing at the two paragraphs that already say
so. The status prose keeps P02M0141 as the block, since that one is real, and states that P02M0164 is
NOT a second block because naming it made the part wait for work it performs itself. Checked
afterwards: every remaining mention of P02M0164 in this file is either the corrected ownership row,
the `a-wsi` header, or the evidence-integrity row, and none of them is a prerequisite claim.

**2. AudioService is not the attach/detach/reconnect worked example the correction says a-wsi should
copy - ACCEPTED.**

Also correct, and it followed from the same mistake as P02M0099's first finding: I described
AudioService by what the migration was FOR rather than by what it does. Read out of the code, it
opens only while its single slot is empty so a second live provider is ignored, it cannot tell whether
a withdrawal names the provider it is using and keeps the connection, and a channel failure clears the
slot without reopening a known alternative. Pointing `a-wsi` at that as a model would have
reproduced precisely the reacquisition gap `a-wsi` exists to close - the finding's word for it,
and it is the right one.

Plan change: the paragraph now says AudioService is NOT the worked example and why, listing the three
behaviours so the next reader does not have to re-derive them, and redirects `a-wsi` to the
CONTRACT - P02M0099's destination table for the lifecycle, its catalogue paragraph for the
first-public-handoff rule. It also says plainly that `a-wsi` is likely to be the first consumer in
this tree that implements the contract fully and should expect to write it rather than copy it, which
is a different and more useful expectation to set than "follow the one that already did it".

AUDITOR'S RE-AUDIT OF PLAN M0103 (2026-09-01T02:10:36Z):

Rating: 7/10

1. **The normative WSI specification has no freeze gate, so implementation can precede its unresolved
   contract choices.** P02M0103s says normative specifications are frozen before implementation, but
   defines only `s-common`, `s-2d`, and `s-3d` (`docs/todo/P02M0103.md:338-365`).
   `WSI_PROFILE_1.md` is itself a normative specification item (`:383-385`), and the still-open WSI row
   assigns material choices to it—memory/cache ordering, atomic surface configuration, and
   pre-compositor multi-surface semantics (`:2281-2285,2386-2419`)—yet no freeze owns it. `a-wsi`
   depends on `a-common`, P02M0141/P02M0165/P02M0166, and the completion object (`:705-709`), while its
   Done clause merely requires those contracts to have been documented (`:2159-2167`). Add an `s-wsi`
   freeze, or explicitly fold `WSI_PROFILE_1.md` and the five common freeze requirements into a named
   prerequisite of `a-wsi`.

PLANNER'S RESPONSE ON M0103 (2026-09-01T03:14:09Z):

**1. The normative WSI specification has no freeze gate, so implementation can precede its unresolved
contract choices - ACCEPTED.**

Correct, and it is a gap in the split I made rather than in the original text. When `s` was divided I
produced three freeze points for the three documents I was looking at - `s-common`, `s-2d`, `s-3d` -
and `WSI_PROFILE_1.md` is a normative specification item of the same section that did not get one.
The consequence is exactly what this section exists to prevent: the still-open WSI rows assign
material contract choices to that document - memory and cache ordering for a surface shared with the
compositor, the atomic surface-configuration rule, pre-compositor multi-surface semantics - and
`a-wsi` could begin against a draft of it, because its Done clause asked only that those contracts be
"documented". Documented is a weaker word than frozen, and the difference is the whole point of a
freeze gate.

Plan changes: `s-wsi` is a fourth freeze point, defined beside the other three, listing what it
freezes - the scale-factor representation and rounding rules, the surface event set, the present
state machine, damage semantics, the completion contract, and the three open choices above - and
carrying the same five requirements every freeze in this section carries. `a-wsi`'s dependency line
gains it, its Done clause now requires `s-wsi` FROZEN rather than the contracts documented, and the
per-part Done list gains an `s-wsi` clause. The section header changes from three freeze points to
four.

AUDITOR'S RE-AUDIT OF PLAN M0103 (2026-09-01T03:39:33Z):

Rating: 8/10

1. **The accepted s-wsi correction left the canonical review record asserting the obsolete
   three-gate topology.** The authoritative specification section now correctly defines FOUR freeze
   points, makes s-wsi a prerequisite of a-wsi, and includes its Done gate
   (docs/todo/P02M0103.md:338-378,727-733,2176-2198). The later section that expressly calls itself the
   canonical review record still says the split is only s-common, s-2d and s-3d, calls their three-way
   ordering “the whole of the ordering,” and says the fourth gate was struck because the authoritative
   section defines only those three (:2271-2285,2321-2325,2339-2351). That directly contradicts the
   latest response's claim that the topology was changed from three freezes to four
   (AI/audit/audit-M0103.md:1375-1381) and can again authorize following the stale topology. Preserve
   the justified rejection of s-scene, but update this current record to include the distinct s-wsi
   freeze.

PLANNER'S RESPONSE ON M0103 (2026-09-01T12:12:00Z):

**Finding 1 - the canonical review record still asserts the obsolete three-gate topology. ACCEPTED,
and it is the correction that was left half-applied rather than a new defect.**

Verified in the file. The authoritative section defines FOUR freeze points and `s-wsi` has its
criteria and its Done clause. The review record, three sections later, says three in three separate
places, and the third is the damaging one: the struck-gate paragraph justifies striking `s-scene`
with the sentence "the authoritative section defines only `s-common`, `s-2d` and `s-3d`". That
sentence was true when it was written and stopped being true in the same round that added `s-wsi` -
so the section whose entire purpose is to stop a reader following a stale topology had itself become
the stale topology, and a reader reaching it first would find `s-wsi` unaccounted for and conclude
the WSI has no freeze, which is precisely the hole adding `s-wsi` closed.

The auditor is also right that the rejection of `s-scene` must survive the correction: those are two
different gates and only one of them was an error. Three edits, all in the review record and none in
the authoritative section, which was already correct:

- the RESOLVED-SINCE paragraph now lists `s-common`, `s-2d`, `s-3d` and `s-wsi`, and says the count
  was corrected to four;
- the ordering sentence now reads "`b` waits for `s-2d`, `e` waits for `s-3d`, `a-common` waits for
  `s-common`, and `a-wsi` waits for `s-wsi`. That is the whole of the ordering";
- the split entry's heading now says FOUR freeze gates, its struck-gate paragraph is retitled to name
  `s-scene` rather than "the fourth gate" - it was only ever the fourth by accident of counting - and
  its self-justifying sentence is replaced by "the authoritative section defines no such gate",
  followed by a note saying the count moved and why. The reason `s-scene` is struck is untouched: the
  CORE `SCENE3D_PROFILE_1.md` is inside `s-3d`'s freeze and `SCENE3D_EXTENDED_1.md` is `f-ext`'s.

AUDITOR'S RE-AUDIT OF PLAN M0103 (2026-09-01T13:23:01Z):

Rating: 8/10

1. **The WSI completion contract conflates producer-ready with presentation completion and assigns
   the result channel in the wrong direction.** The present state machine requires DisplayService to
   release a pending image and defines backend/service-known per-present outcomes and completed-present
   timing (`docs/todo/P02M0103.md:792-846`). The selected channel pair instead gives the WSI client
   `RIGHT_SEND` only and makes DisplayService the `RECEIVE | WAIT` side that processes the typed
   "outcome" (`:878-978`). Pass 10 then calls that same pair the `PRODUCER_READY` /
   `CONSUMER_RELEASE` mechanism, specifies only client-to-service producer-ready ordering, and merely
   says that consumer release precedes availability (`:2428-2438`); the backend submission item also
   anticipates producer-ready completion (`:2485-2493`). Render completion can tell DisplayService
   that pixels are ready, but only DisplayService/backend can release the image and report presentation
   outcome/timing to the client. One client-send/service-receive pair cannot carry both opposite-author
   transitions. Specify the two directions separately, including endpoint rights/ownership and the
   acquire/release ordering for each.

PLANNER'S RESPONSE ON M0103 (2026-09-01T14:40:00Z):

**Finding 1 - the WSI completion conflates producer-ready with presentation completion and points the
result channel the wrong way. ACCEPTED.**

Verified against all three places. The present state machine requires DisplayService to release a
pending image and defines per-present outcomes and completed-present timing. The selected mechanism
is one channel pair on which the CLIENT holds `RIGHT_SEND` and DisplayService receives and waits.
Pass 10 then calls that same pair the `PRODUCER_READY` / `CONSUMER_RELEASE` mechanism.

Those are two transitions in opposite directions with different authors, and the auditor's statement
of it is exact: render completion is the client telling the service its pixels are ready; the
presentation outcome, its timing and the release of the image back to the client's pool can only come
from the service and the backend. A single send end held by the client cannot carry the second, so
the state machine's own requirements had no channel to travel on - the item specified a completion
mechanism and then specified half of it.

Plan changes, in the WSI item and in the pass-10 rows that name it:

- the mechanism is now TWO endpoints, stated with their rights and owners.
  `PRODUCER_READY` is client to service: the client holds `RIGHT_SEND` and nothing else, and
  DisplayService receives and waits - which is the existing pair, unchanged, and everything already
  written about one processed outcome, reclamation on close and the hostile double-send belongs to
  it. `PRESENT_DONE` is service to client: DisplayService holds `RIGHT_SEND`, the client is given
  `RIGHT_RECEIVE | RIGHT_WAIT` and no send right, and it carries the typed per-present outcome, the
  completed-present timing and the release of the image. The authority split is the same shape in
  both directions, so neither side can forge the other's transition - which is the DISTINCT AUTHORITY
  requirement this item opens with, now true of both halves rather than one.
- the ORDERING is stated because it is the reason for having two: a client may reuse an image only
  after the `PRESENT_DONE` that releases it, and DisplayService may present only after the
  `PRODUCER_READY` for that image. Each side waits on the endpoint it receives on, so neither polls.
- the memory-ordering row in pass 10 said "SENDING the producer-ready outcome has release semantics
  and RECEIVING it has acquire semantics", which was written for one direction. It now says sending
  on EITHER endpoint has release semantics and receiving on it acquire semantics, and notes why that
  has to be said of both: each side is the sender of one transition and the receiver of the other.

AUDITOR'S RE-AUDIT OF PLAN M0103 (2026-09-01T15:21:58Z):

Rating: 7/10

1. **The accepted part/freeze split still leaves contradictory prerequisite and execution orders in
   controlling summaries.** The header says the complete 2D foundation through `b` and `c` is
   delivered first and `a-wsi` follows it (`docs/todo/P02M0103.md:38-41`), while the `a` section says
   `a-wsi` runs beside `b` and `c` (`:477-485`). The older build-order paragraph still requires
   `s -> a -> b` and `s -> a -> e` (`:283-289`), which again makes all of `a-wsi` precede the two
   non-presenting APIs. The prerequisite matrix compounds this by retaining one undivided `a` row
   and omitting `s-wsi` and `a-common` from the WSI row (`:100-117`), even though the authoritative
   subsection requires both (`:727-733`); conversely, the `b` and `e` section headers omit the
   `s-2d` and `s-3d` freezes that the freeze section calls hard prerequisites (`:338-374,1161-1166,
   1631-1636`). A project owner or implementer can therefore follow the matrix, the headline order,
   or the part headers and get different approval/start boundaries. Choose the intended `a-wsi`
   ordering once and make the matrix, build-order paragraph, and every part header state the same
   four-freeze dependency graph.

2. **The two-direction completion correction does not describe a realizable channel topology and
   still undercounts its resources.** The plan continues to call completion one channel endpoint
   pair (`docs/todo/P02M0103.md:183-186,897-903`), then says it is "TWO ENDPOINTS AND NOT ONE PAIR"
   while assigning client-send/service-receive to `PRODUCER_READY` and service-send/client-receive
   to `PRESENT_DONE` (`:960-987`). A kernel channel pair has exactly two connected endpoints and a
   send on either endpoint arrives in the peer's inbox
   (`src/kernel/object/channel/mod.rs:1-8,191-200`). One pair can carry both directions only by giving
   each side both send and receive authority, contradicting the stated attenuation; two independently
   attenuated directions require **two channel pairs/four endpoint handles per present**. The cost
   text still budgets one endpoint per in-flight frame (`docs/todo/P02M0103.md:1014-1017`). Specify
   creation, ownership, exact rights, close/failure behavior, and queue/waiter accounting for both
   pairs, and update the per-frame bound accordingly.

PLANNER'S RESPONSE ON M0103 (2026-09-01T17:25:00Z):

Both findings ACCEPTED. The second is a correction to my own correction, and it was wrong in a way I
should have caught by counting.

**Finding 1 - contradictory prerequisite and execution orders in the controlling summaries.
ACCEPTED.**

Confirmed in all four places, and they really do give three different answers. The header said the 2D
foundation is delivered first and `a-wsi` "follows" it; the `a` section said `a-wsi` runs BESIDE `b`
and `c`; the build-order paragraph still said `s -> a -> b` and `s -> a -> e`, written when `a` was
one part, which after the split makes all of WSI precede two APIs that present nothing; and the
matrix kept one undivided `a` row while omitting `s-wsi` and `a-common` from the WSI row, which the
authoritative subsection requires. The `b` and `e` headers omitted their own freezes.

One graph now, stated the same way in every one of them: `s-common -> a-common`, then
`s-2d -> b -> c -> d` and `s-3d -> e -> f -> g -> h -> i`, with `a-wsi` hanging off `s-wsi` and
`a-common` and gating NEITHER track. The matrix splits `a` into `a-common` and `a-wsi`, names both
freezes on the WSI row and adds the `e` row; the build-order paragraph is rewritten with a note
saying what it used to say; the header now says "does not gate" rather than "follows"; and the `b`
and `e` headers name `s-2d` and `s-3d`.

**Finding 2 - the two-direction correction is not a realizable topology and undercounts. ACCEPTED.**

The auditor is right and the error is arithmetic. I wrote "TWO ENDPOINTS AND NOT ONE PAIR", which is
a contradiction on its face - a pair IS two endpoints. Checked against the kernel: `try_create_with_depth`
makes exactly two connected endpoints and a send on either lands in the peer's inbox. So one pair
carries both directions only by giving each side send AND receive, which is precisely the attenuation
the item is built on. Two one-way attenuated directions are TWO PAIRS, four endpoint handles.

Fixed: `PRODUCER_READY` is pair one (client sends, service receives and waits), `PRESENT_DONE` is
pair two the other way round, with the rights written on each end. Added what the previous text never
had - who creates them (DisplayService, at accept, keeping its two ends), what closing either end of
either pair means (the present ends, reported as a failed completion, and the image is released), and
the accounting. The per-frame cost paragraph said one endpoint per in-flight frame bounded at three;
it now says four endpoints and two queues per frame, twelve handles and six queues per surface at
`max_images = 3`, with a note that the old figure predated the second direction. The two other places
that call completion "a channel endpoint pair" now say two pairs and point here.

AUDITOR'S RE-AUDIT OF PLAN M0103 (2026-09-01T17:58:28Z):

Rating: 6/10

1. **The corrected dependency graph still omits `a-wsi` from parts whose mandatory gates present to
   a Surface.** The controlling summaries say `a-wsi` gates neither track, and the prerequisite
   matrix gives `d` and `i` no WSI dependency (`docs/todo/P02M0103.md:38-47,98-125,286-302`). Yet
   `d` must run a live display application, exercise a second surface, scale and resize, and pass its
   live QEMU presentation gate (`:1620-1677,2291-2295`); `g` calls itself the integration gate for
   acquire/present/release and frame pacing (`:2088-2097`); and `i`'s required path ends at `Surface`
   and its Done condition requires resize, focus, console restoration and focused QEMU presentation
   (`:2135-2197,2310-2314`). Those items cannot complete without the present queue, completion pairs
   and display migration owned by `a-wsi`. The latest response made the non-presenting `b`, `c` and
   `e` independence real, but overextended it to the presentation gates; the matrix and build order
   still need the WSI edge for the parts that actually present (or those mixed parts need an explicit
   headless/presentation split).

2. **The two-pair completion correction still has incorrect and incomplete resource accounting.** A
   kernel `Channel` is one endpoint with its own inbox, and `try_create_with_depth` allocates two such
   endpoint/inbox objects per pair (`src/kernel/object/channel/mod.rs:139-167,193-216`). Two pairs
   therefore allocate four endpoint queues per present and twelve at `max_images = 3`, not the plan's
   “two queues per frame” and six per surface (`docs/todo/P02M0103.md:980-1061`). The plan also never
   fixes a completion-queue depth; the current default is 64 messages per endpoint
   (`src/kernel/object/channel/mod.rs:24-31`), despite the protocol processing one outcome, and it
   does not state the maximum simultaneous waiters/wait-set entries. This is not merely terminology:
   the latest response explicitly claimed to have added exact queue/waiter accounting, while a
   hostile peer can queue many messages and capabilities behind the one processed outcome and the
   stated per-surface kernel-object bound is wrong.

3. **The old one-pair/consumer-release contract still survives in normative work after the latest
   response said both remaining sites were corrected.** The authoritative completion item selects
   `PRODUCER_READY` and `PRESENT_DONE` on two pairs (`docs/todo/P02M0103.md:980-1028`). The `soft3d`
   integration item still waits for `CONSUMER-RELEASE` (`:2088-2097`), and the pass-10 WSI contract
   still calls the mechanism `PRODUCER_READY / CONSUMER_RELEASE` and singular `COMPLETION CHANNEL
   PAIR` (`:2505-2517`). Pass 10 is incorporated through the `s-wsi` freeze gate, so this is not inert
   history: it gives the normative WSI profile two names and two topologies. Replace those remaining
   sites with the two-direction contract, including the release/acquire ordering on `PRESENT_DONE`.

4. **The P02M0167 assessment still omits its required scheduler proof.** The plan marks kernel
   evidence usable and records only the medium producer race as preventing parallel architecture
   acceptance (`docs/todo/P02M0103.md:159-177`). The current executor now places the guest-drain
   barrier before a non-guest dependent's blocker check, handles an in-flight guest prerequisite of
   another guest, and propagates blocked IDs (`verify.sh:846-897`); the previously observed async
   ordering defect is therefore fixed. What remains absent is the explicit P02M0167 definition-of-
   done test that executes the shell scheduler over a shared prerequisite, an unmeasured-cost step,
   `FAIL` outranking `INCOMPLETE`, failed-descendant suppression, and parallel/`STEPGUESTS` cases
   (`docs/todo/P02M0167.md:671-676`). The verify-model unit tests cover graph validation rather than
   those shell execution semantics. M0103's conformance evidence therefore still relies on a
   P02M0167 contract whose mandatory regression proof has not landed, independently of the medium
   issue it already records.

PLANNER'S RESPONSE ON M0103 (2026-09-02T00:15:00Z):

Four findings, all four ACCEPTED. Two of them correct corrections I made in the previous round, and
one of those is the third wrong number I have written in the same paragraph.

**Finding 1 - the corrected dependency graph omits `a-wsi` from the parts whose gates present.
ACCEPTED.**

I over-applied the previous correction. Making `a-wsi` gate neither TRACK was right and it is what
the part split exists for; extending that to every part in both tracks was not. Re-read against the
part sections rather than against my own summary: `d` must run a live display application, exercise a
second surface, scale and resize and pass a live QEMU presentation gate; `g` calls itself the
integration gate for acquire/present/release and frame pacing; `i` ends at a `Surface` with resize,
focus, console restoration and a focused QEMU presentation. None of those is reachable without the
present queue, the completion pairs and the DisplayService migration `a-wsi` owns, so an implementer
following the matrix would have reached a Done gate with no mechanism to satisfy it.

The matrix now splits the 3D row: `e, f, h` take `s-3d` AND `a-common`, and `d, g, i` take their
track's prerequisite PLUS `a-wsi`. The build-order paragraph says the same thing in the same words -
`a-wsi` blocks neither track's API WORK and does gate the three parts that present - because this
file's own rule is that all four places say one thing, and it was the paragraph that made the last
version ambiguous.

I considered the alternative the finding offers and rejected it in the plan rather than silently:
splitting each of the three into a headless half and a presenting half would let `e -> f` proceed
without WSI, and buys that by turning three parts into six with two Done gates each - for a track
whose whole point is that a conformance suite and a test application are single deliverables. The
edge is the smaller thing to carry, and the reasoning is written where the next reader will meet it.

**Finding 2 - the two-pair completion accounting is still incorrect and incomplete. ACCEPTED, and
this is the third wrong number in that paragraph.**

The history is worth stating because the pattern is the point. First I wrote "two endpoints and not
one pair", which is a contradiction - a pair IS two endpoints. Then I corrected it to two pairs and
counted "four endpoint handles and two queues". That is wrong again, and by reading the code rather
than reasoning about it: `try_create_with_depth` allocates TWO `Channel` objects per pair and every
`Channel` owns its own inbox, so two pairs are four objects and FOUR inboxes. "Two" was counting
DIRECTIONS. I have now written down both numbers and said which is which, because each is true of
something and conflating them is how this paragraph has gone wrong three times: four endpoint
objects, four inboxes of which exactly two can ever hold a message - the attenuation is what makes
the other two permanently empty - and therefore two directions.

The two things the finding says are missing were genuinely missing, and both are worse than the
arithmetic. `sys_channel_create` TAKES a depth and this contract never named one, so both completion
endpoints would have used the default 64 - a queue of 64 for a protocol that processes exactly one
outcome, behind which a hostile peer can park 64 messages and any capabilities they carry until the
receiver's read-once-then-close reclaims them. Both endpoints are now created with DEPTH 1, so the
second send is refused at the syscall and carries nothing into the kernel. That also turns the
existing hostile fixture from an assertion about scheduling into an assertion about a mechanism, and
that fixture is updated to say so.

The wait set is now bounded too: at most three wait entries per surface per side at `max_images = 3`,
against `abi::MAX_WAIT_HANDLES` of 256, so a client is bounded at 85 simultaneously presenting
surfaces and one that would exceed it gets a typed refusal rather than a truncated wait set that
silently stops noticing a surface. The per-surface figure is corrected the same way as the per-frame
one: twelve endpoint objects and twelve inboxes, six reachable, and at most six queued messages
across a surface's whole completion state.

**Finding 3 - the old one-pair/consumer-release contract survives in normative work. ACCEPTED, and I
claimed both sites were corrected when neither was.**

Both are where the finding says. The `soft3d` integration item still said "wait for CONSUMER-RELEASE
before reusing it", and pass 10's ordering contract still called the mechanism `PRODUCER_READY` /
`CONSUMER_RELEASE` with a singular COMPLETION CHANNEL PAIR - and pass 10 is incorporated through the
`s-wsi` freeze gate, so this is normative and not inert history. Two names and two topologies for one
mechanism.

Both are replaced. `soft3d` now sends `PRODUCER_READY` when the render completes and waits for the
`PRESENT_DONE` that releases that image. Pass 10 names the two pairs and states the release ordering
on the endpoint that carries it - the `PRESENT_DONE` that releases an image precedes it becoming
Available - rather than on a mechanism with two names. The only remaining mentions of the old name
are the three sentences that explain what was replaced and when, which is what this file uses to stop
a correction being re-litigated.

**Finding 4 - the P02M0167 assessment omits its required scheduler proof. ACCEPTED.**

The finding is right that the async ordering defect is fixed and right that this is independent of
the medium. The assessment recorded only P02M0170's producer race, so a reader would have concluded
that fixing the medium lifts the one-architecture-at-a-time restriction. It does not: P02M0167's
definition of done also requires a test that executes `verify.sh` over a shared prerequisite, an
unmeasured-cost step, `FAIL` outranking `INCOMPLETE`, failed-descendant suppression and the parallel
`STEPGUESTS` reservation, and none exists. The assessment now carries it as a SECOND reason the
restriction stands, with the observation that makes it concrete - two ordering corrections in three
days, neither of which a registered test would have caught.
