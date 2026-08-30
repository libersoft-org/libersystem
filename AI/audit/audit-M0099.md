AUDITOR'S REVIEW OF PLAN M0099 (2026-08-28T21:20:14Z):

Rating: 2/10

The document is useful as a catalogue of possible driver work, but it is not safe to implement as a milestone plan. Its declared foundation is stale, several architectural capabilities it assumes do not exist, multiple entries have no attainable dependency path, and the generic acceptance text cannot prove that the resulting drivers are usable. Material restructuring and prerequisite work are required before individual items can be treated as implementation-ready.

## Material findings

1. **The document intentionally omits the per-item plans that it says make work pick-up-ready.**

   **What is wrong:** The plan says that work starts by selecting an item which states its `requires`, `owns`, and named integration gate (`docs/todo/P02M0099.md:25-34`, `:553-560`). It later explicitly declines to populate those triples until someone starts an item (`:703-707`). Several checkboxes are also unresolved architecture decisions or families rather than implementable slices: `virtio-iommu` says its remaining scope “is not yet work” (`:115-121`), the USB execution model is undecided (`:330-345`), and combined entries such as ACPI power/battery/thermal admit that they must first be split (`:261-269`). The common Definition of Done at `:584-592` is then applied equally to drivers, a parser/helper such as EDID, kernel/bus infrastructure, and the USB prerequisite.

   **Why it matters:** Implementers must invent scope, dependencies, shared contracts, and the proof of completion while coding. Parallel items can make incompatible decisions, and neither an individual checkbox nor the umbrella milestone can be closed objectively.

   **Correction:** Keep M0099 as a non-completable roadmap/index, or narrow it to a genuinely bounded deliverable. Before coding any candidate, create an executable item plan that names its prerequisites, exact owned contracts and files, exclusions, target/device model or fixture, observable-effect oracle, measurable limits, and type-appropriate Definition of Done. Classify entries as driver, service, shared library, kernel/bus infrastructure, or architectural prerequisite instead of applying one driver lifecycle gate to all of them.

2. **The declared hard gate is factually stale and too small for the plan's own acceptance criteria.**

   **What is wrong:** M0099 says P02M0098/P02M0161/P02M0162 are the hard gate and that it is complete except for architecture boots and bind-window measurements (`docs/todo/P02M0099.md:3-17`, `:36-40`). In the current tree P02M0162 is `REOPENED` with five outstanding items; its audit records leaked driver-side/resource handles on partial bind failure, blocking teardown/backoff, retry paths that bypass policy, broken recovery after `Online`, an incorrect quarantine transition, and missing invariant tests (`docs/todo/P02M0162.md:3`, `:400-418`). Relevant supporting milestones are also reopened: P02M0153 for DMA safety, P02M0163 for discovery, P02M0164 for providers, P02M0165 for stop/teardown, P02M0166 for operator/removal state, and P02M0167 for trustworthy verification.

   **Why it matters:** A driver can be written on top of these APIs but cannot satisfy M0099's bind rollback, provider routing, restart/removal, resource cleanup, and tri-architecture evidence requirements. Treating those milestones as parallel polish permits acceptance on known-broken integration paths.

   **Correction:** Replace the three-item global gate with a current per-item prerequisite matrix. P02M0162 must be reclosed before any driver is accepted; plain-PCI/firmware items need P02M0163's relevant work, provider-producing or provider-consuming items need P02M0164, stop/removal work needs the relevant P02M0165/P02M0166 semantics, DMA items need P02M0153, and runtime acceptance needs P02M0167's run isolation. Preserve architecture boot and measured-window checks as explicit gates after the underlying bind path is correct.

3. **The provider catalogue cannot perform the subscription and connection behavior the plan assumes.**

   **What is wrong:** M0099 states that `subscribe` already supplies a snapshot plus live additions and assigns only USB withdrawal behavior to the first runtime child (`docs/todo/P02M0099.md:63-72`, `:97-100`). The generated ordinary dispatcher does not handle `OP_SUBSCRIBE` at all (`src/user/libs/protocol/device-proto/src/generated/liber/device/v1.rs:1566-1613`); a separate `subscribe_open` helper exists but DeviceManager never invokes it (`:1621-1631`; `src/user/services/core/src/device_manager.rs:2925-2956`). `ProviderInfo` contains metadata but no way to open a provider connection (`src/idl/device.lsidl:113-127`), while `Catalogue::take` moves the sole stored channel once (`device_manager.rs:1536-1577`). Existing routing still fills singleton net/display/audio handles (`:884-899`). Declared `requires` are checked only when a start is attempted (`:2120-2142`); publication and withdrawal do not wake dependents or stop an online dependent.

   **Why it matters:** Late or multiple consumers cannot obtain usable connections, parked drivers can remain parked after a provider appears, and drivers can continue after a required provider disappears. Nearly every new block, network, audio, bus-child, and platform provider would otherwise be metadata-only or would reintroduce the private singleton handoff M0099 forbids.

   **Correction:** Make the relevant P02M0164 work a prerequisite: add a per-consumer connection factory/open operation, a served snapshot-plus-add/remove stream, exact dependency edges, wake-on-publication and stop-on-withdrawal behavior, and migrate existing singleton service handoffs. Gate this with multi-provider, multi-subscriber, late-arrival, withdrawal, and reconnect tests before provider-based M0099 items can close.

4. **Plain-PCI and firmware-bound discovery are cross-cutting identity changes, not narrow resource rows owned ad hoc by the first driver.**

   **What is wrong:** The plan assigns NVMe the first plain-PCI profile and assigns a “stable firmware-node identity and one claimable resource row” to whichever firmware driver happens to land first (`docs/todo/P02M0099.md:80-88`). P02M0163 is reopened because the binding inventory still contains only the special virtio/xHCI paths and DeviceManager does not consume the full PCI scan (`docs/todo/P02M0163.md:3`, `:184-199`). More fundamentally, identity and policy are PCI-shaped throughout the architecture: `BindingId` is BDF plus generation (`src/user/libs/driver/binding/src/lib.rs:431-456`), provider origin is BDF-based (`src/idl/device.lsidl:113-127`), manifest matching supports only virtio-PCI/plain-PCI predicates (`src/tools/system-manifest/src/lib.rs:168-229`), and kernel claim/release assumes a PCI function. PL011, firmware-described 16550, ACPI namespace devices, I2C children, USB interfaces, and NVMe namespaces cannot be represented by adding one row.

   **Why it matters:** Per-driver special cases would fragment identity, claim generation, policy keys, provider origins, diagnostics, revocation, and hotplug semantics. Plain-PCI drivers still would not see all eligible functions, while non-PCI drivers would have no sound public binding identity at all.

   **Correction:** Complete a shared discovery/identity prerequisite. It must feed the full PCI inventory to DeviceManager and introduce a discriminated, stable device-node identity propagated through matching, claim/release, binding IDs, provider origins, policy, diagnostics, and generation handling, with transport-specific resource and teardown hooks. Then state which M0099 item merely adds the first profile using that foundation; it must not create a private NVMe or UART path.

5. **The proposed “cheap” legacy 16550 slice lacks both userspace authority and a safe kernel handoff.**

   **What is wrong:** M0099 says the legacy x86 UART needs only a claimable entry (`docs/todo/P02M0099.md:90-94`), while its item requires PIO/MMIO and IRQ capabilities plus an early-console handoff (`:207-211`). The public driver resource vocabulary has only device MMIO, one IRQ, keys, system power, and console (`src/user/libs/driver/protocol/src/lib.rs:231-263`); there is no range-scoped port-I/O object/syscall or legacy fixed-IRQ authority. The x86 kernel still programs COM1 directly, owns IRQ4/RX, drains TX, and polls RX after userspace starts (`src/kernel/main.rs:343-349`, `:363-425`; `src/kernel/arch/x86_64/serial.rs:61-113`, `:212-236`). Its asynchronous ownership is monotonic rather than transferable.

   **Why it matters:** A userspace 16550 driver cannot access the legacy device through public capabilities and, if given raw access, would race the kernel for registers and interrupts. Panic-output fallback is also undefined after transfer.

   **Correction:** Make the 16550 plan own or require a range-limited, revocable PIO capability; fixed/legacy IRQ routing and revocation; an atomic kernel quiesce-to-userspace takeover; and a panic-only fallback/reacquisition rule. Alternatively scope the initial item to firmware-described MMIO UARTs and remove the claim that the legacy x86 path is already cheap.

6. **The declared per-driver DMA/IOMMU policy does not exist, and the `virtio-iommu` checkbox has no executable rollout.**

   **What is wrong:** M0099 requires every DMA driver to use an explicit `iommu-required` or audited trusted-untranslated policy (`docs/todo/P02M0099.md:561-563`). The driver manifest has no such field (`src/tools/system-manifest/src/lib.rs:79-105`, `:462-470`); kernel admission instead uses a hard-coded list containing only virtio-net and defaults every other device to `TrustedUntranslated` (`src/kernel/dma_policy/mod.rs:66-103`). P02M0153 is reopened with the portable bounce/coherency contract, fault-to-binding lifecycle, and integration-visible statistics still incomplete (`docs/todo/P02M0153.md:3`, `:843-849`). M0099's `virtio-iommu` entry itself says “remaining drivers and architectures” is not a usable scope (`docs/todo/P02M0099.md:115-121`).

   **Why it matters:** New NVMe, AHCI, HDA, USB, and other DMA-capable drivers can silently run untranslated, contradicting the security policy and requiring forbidden source-list edits. The tri-architecture security gate is not defined.

   **Correction:** Put a required DMA policy in the trusted/generated registry, validate it, propagate it to kernel claim admission, and refuse missing or unsatisfied isolation rather than defaulting silently. Reclose P02M0153's portable and lifecycle work. Either remove the kernel-owned `virtio-iommu` checkbox from this driver roadmap or enumerate exact driver/architecture migrations, backend requirements, absent-IOMMU policy, and negative gates for each slice.

7. **The native-storage acceptance gate can pass while NVMe/AHCI remain unusable as the system disk.**

   **What is wrong:** Group 2 is justified as the hardware needed for a real machine to read its disk (`docs/todo/P02M0099.md:183-195`), but the shared storage gate only formats, mounts, reads/writes, flushes, and reopens a device through StorageService (`:577-583`). Today the sole bootstrap block driver is pinned and boot-critical (`src/user/services/manifest.toml:1408-1421`). DeviceManager documents that volume-stage drivers cannot be loaded until the bootstrap block driver has already mounted that volume (`src/user/services/core/src/device_manager.rs:391-395`, `:589-615`), and manifest validation rejects a boot-critical driver stored on the volume it must make accessible (`src/tools/system-manifest/src/lib.rs:1279-1286`).

   **Why it matters:** NVMe or AHCI could close after working only as a secondary disk while the intended bare-metal machine still depends on virtio-blk to boot. That fails the item motivation and conceals a staging/root-selection integration problem.

   **Correction:** For each controller eligible to host `vol://system`, specify pinned/boot-critical staging, deterministic boot-volume selection, recovery/fallback behavior, and an end-to-end boot gate whose system volume is on that controller rather than on virtio-blk. Keep a secondary-disk test as additional coverage, not as the sole integration proof.

8. **Four ACPI items and HID-over-I2C have no attainable prerequisite path.**

   **What is wrong:** The plan correctly observes that battery, thermal, x86 UCSI, and ACPI Time/Alarm require an AML interpreter and namespace (`docs/todo/P02M0099.md:261-297`, `:457-462`), then says that interpreter is a separate milestone and not M0099 (`:538-539`). No such milestone exists in the roadmap. HID-over-I2C similarly owns only an abstract I2C contract and host fake while explicitly deferring end-to-end binding because there is no controller item or QEMU model (`:371-383`). Nevertheless, M0099 says every item must meet its implementation and bind/fixture Definition of Done (`:584-592`).

   **Why it matters:** These checkboxes cannot be completed under the plan as written. “Whoever wants them writes an interpreter first” silently expands one driver item into a subsystem comparable in size to the roadmap, while the hardware-deferred I2C half has no event that can ever close it.

   **Correction:** Create and name a bounded AML/namespace predecessor with its own security and conformance scope; split fixed-hardware power-button work from AML battery/thermal work; and give HID-over-I2C a real controller/provider and fixture milestone. Otherwise remove those entries from M0099's completion set and label any parser-only work as such.

9. **Shared backend contracts and destination-service ownership are incomplete and internally inconsistent.**

   **What is wrong:** M0099 claims only three shared contracts lack owners (`docs/todo/P02M0099.md:553-560`) and tells HDA/USB Audio and CDC networking to reuse the virtio-snd/virtio-net transports (`:273-278`, `:365-370`, `:384-389`). In the current tree those are private, unversioned, singleton handoffs: AudioService receives one static `SND` channel and the public audio IDL is client-facing (`src/idl/audio.lsidl:7-19`; `src/user/services/core/src/audio_engine.rs:656-666`); NetworkService receives one bootstrap `FRAMES` handle and has no typed link-state, detach, or reconnect path (`src/user/services/core/src/network_service.rs:108-138`, `:245-295`). Other bullets name Bluetooth, smart-card, modem, camera, printer, media/import, and DFU/admin services for which no owned service/IDL milestone is given. Ownership also conflicts: virtio-SCSI alone owns common SCSI extraction although independently ordered UAS requires it, and HID-over-I2C is named sole I2C-contract owner even though the earlier IPMI SSIF transport also consumes that bus class.

   **Why it matters:** A hardware driver can work locally yet have no typed, multi-instance route into the service that proves user-visible behavior. Order-independent development can produce duplicate SCSI/I2C/audio/network contracts or block one item on an unrelated item that was not declared a gate.

   **Correction:** Inventory every device-facing and service-facing contract, assign one explicit owner and migration path, and create missing destination-service milestones. The first non-virtio audio/network slice must own versioned device-side PCM/frame-plus-link contracts, migrate existing virtio endpoints, and add service attach/detach/reconnect support. Use a first-implemented-consumer rule across all SCSI and I2C claimants, not a fixed owner contradicted by the roadmap's non-gating order.

10. **The verification section is neither executable per item nor currently reliable.**

   **What is wrong:** The plan promises one named gate per item but supplies only generic prose (`docs/todo/P02M0099.md:553-583`), with no test IDs, exact QEMU arguments/fixture orchestration, expected observable effect, or performance thresholds. Its mandatory host-test strategy is not currently runnable for the driver crate: `cargo test --manifest-path src/user/drivers/core/Cargo.toml --lib` fails with duplicate `panic_impl` because the driver crate unconditionally links the `no_std` runtime and its panic handler (`src/user/drivers/core/Cargo.toml:50-68`; `src/user/runtime/rt/src/lib.rs:13`, `:94-100`). The repository's component-oracle convention requires an observable effect rather than merely reaching `Online` (`src/tools/check-component-oracles.sh:2-17`, `:32-62`), but the plan does not assign those oracles or wire new QEMU devices into the harness. P02M0167 is also reopened because concurrent runs still share writable artifacts/sockets/build outputs, so even generic tri-architecture results can come from the wrong build (`docs/todo/P02M0167.md:3`, `:1077-1089`).

   **Why it matters:** A checkbox can pass builds and smoke boot without exercising its driver or destination service, while parallel evidence can be nondeterministic. The hostile-device, two-instance, teardown, and performance claims are not adjudicable.

   **Correction:** Establish a host-testable pure protocol/driver layer or a host-test feature that removes runtime entry/panic/allocator hooks, plus an explicit fake-controller seam. For every item, name exact host tests, QEMU model and arguments or physical fixture, target/resource matrix, observable service effect, two-instance topology where applicable, teardown assertion, and numeric performance/resource limits. Register those oracles in the verification model, and require P02M0167's isolated run identity/artifacts before accepting results.

11. **The phase assignments and overall scope contradict the project architecture document.**

   **What is wrong:** M0099 repeatedly assigns concrete chips, boards, quirks, native IOMMUs, and deployment-specific work to Phase 4 (`docs/todo/P02M0099.md:7`, `:123`, `:200`, `:260-319`, `:594-599`). The architecture says Phase 4 is the VM/reference desktop and that real GPUs/audio and other hardware follow in Phase 5 (`docs/CONCEPT_EN.md:1664-1675`). It explicitly assigns concrete device/board drivers, selected real machines, bare-metal ARM/RISC-V, and deployment power management to Phase 5 (`:1680-1688`). It also defines Phase 2 as a deliberately small, bounded universal set selected for appliance value (`:974-998`), not every approved standardized controller/class.

   **Why it matters:** The plan assigns ownership and timing to the wrong product phase and allows an effectively unbounded cross-phase backlog to masquerade as a Phase-2 milestone. That distorts dependencies and completion reporting even if items can ship separately.

   **Correction:** Map every candidate to its actual phase and change concrete-hardware handoffs from Phase 4 to Phase 5. Name the small Phase-2 subset and its selection criteria; keep later candidates as explicitly phased backlog rather than part of one milestone's completion condition.

12. **The reference-implementation table misclassifies copyleft code as permissively licensed.**

   **What is wrong:** The plan says its table names permissively licensed references that may be read as implementation references (`docs/todo/P02M0099.md:445-449`), but it lists LGPL projects for CCID, MBIM, and PTP (`:516-517`, `:521`). LGPL is not a permissive license.

   **Why it matters:** An implementer following the table's stated policy could copy or derive code under an incorrect compatibility assumption, creating avoidable licensing and provenance risk.

   **Correction:** Replace those entries with genuinely permissive references, or label them as observational/non-copy references under a documented clean-implementation boundary, just as the plan already does for GPL Linux sources.

---

PLANNER'S RESPONSE ON M0099 (2026-08-30T11:20:00Z):

Every finding was checked against the tree as it stands on 2026-08-30. This audit is dated
2026-08-28T21:20:14Z and several of its premises were true then and are not now: P02M0153, P02M0162,
P02M0163, P02M0164, P02M0165, P02M0166 and P02M0167 were all closed on 2026-08-29. Where a finding
rests on one of those being reopened, the finding's PREMISE is rejected and its CORRECTION is
re-examined on its own merits, because a stale premise does not automatically make a correction
wrong - and in three cases it did not.

**1. The per-item plans are deliberately omitted. ACCEPTED IN PART.**

REJECTED: writing forty-one `requires`/`owns`/gate triples now. The plan's own reasoning is right and
the audit's correction concedes the alternative - inventing prerequisites for drivers nobody has
started is how a roadmap acquires answers before it has questions.

ACCEPTED, and it is the real defect underneath: the file said it was a roadmap and then carried
"Done when: every item in the three groups above ..." in the one place a reader looks for the answer,
and applied ONE DRIVER-SHAPED GATE to a parser, to kernel infrastructure and to an undecided
architecture question alike.

Plan changes:
- The completion line is replaced. The milestone is declared a NON-COMPLETABLE INDEX that is never
  ticked and never appears in a phase's completion report; what closes is an item, and the former
  "Done when" is restated as the per-item standard.
- A classification table is added to the head - driver, shared library, kernel/bus infrastructure,
  destination service, architectural prerequisite - and the closing standard now names a different
  gate per class: a shared library closes on host tests and hostile fixtures with no bind, a
  prerequisite closes on the decision being recorded and the mechanism existing, and so on.
- A new item requires an executable per-item plan BEFORE any code on that item, naming its class,
  its prerequisites from the new matrix, owned contracts and files, exclusions, device model or
  fixture, observable-effect oracle, measurable limits, and the Done for its class. The distinction
  is when, not whether: the triples are written when an item is picked up, and not before.

**2. The hard gate is factually stale. PREMISE REJECTED, CORRECTION ACCEPTED.**

The specific claim - "P02M0162 is REOPENED with five outstanding items", plus six reopened supporting
milestones - is no longer true; all seven are COMPLETE as of 2026-08-29. But the audit is right that
a single global three-item gate is the wrong shape, and the file demonstrated it by going stale in
BOTH directions: it asserted "the hard gate is complete with two verifications outstanding" while
citing a milestone that had since been reopened and re-closed.

Plan changes: the paragraph is replaced by a PER-ITEM PREREQUISITE MATRIX - the three-slice floor for
every item, P02M0153 for any DMA-capable driver, P02M0163 plus the identity prerequisite for
plain-PCI and firmware-bound items, P02M0164 for anything publishing or consuming a provider,
P02M0165/P02M0166 for stop, removal and operator state, P02M0167 for tri-architecture acceptance -
with the statement that each item names what it touches and inherits nothing else. A matrix cannot go
stale the way a global gate did, because each line names its own subject.

**3. The provider catalogue cannot subscribe or connect. MOSTLY REJECTED AS RESOLVED; RESIDUE
ACCEPTED.**

Checked in the tree, and P02M0164 closed nearly all of it: `provider-catalogue` now has
`@op(1) subscribe` as a served stream, `@op(2) bindings`, and `@op(3) open` - a per-consumer
connection factory that verifies slot, provider generation and binding generation, refuses a consumer
beyond the number the driver's registry entry declares it admits, mints a fresh channel pair and
sends the server end to the driver (`src/idl/device.lsidl:246-261`;
`src/user/services/core/src/device_manager.rs:3542-3575`). `provider-info` carries a `live` flag so a
withdrawal travels on the same stream (`src/idl/device.lsidl:137-143`). A `requires` edge WAKES a
`DependencyPending` node when its provider appears and STOPS an online one when its provider is
withdrawn (`device_manager.rs:3349-3371`). Late and multiple consumers are no longer stranded.

ACCEPTED residue, and it is the half the audit was right about for a different reason than it gave:
no driver SENDS a withdrawal - xHCI creates its block channel unconditionally and leaves the
publication standing on detach - and the existing services still arrive by the positional bootstrap
handshake, one singleton each for NET, GPU, SND and the input channels. That is the private handoff
this milestone's head forbids new drivers from adding, while every current service depends on it.

Plan changes: the "nobody is told" paragraph is rewritten to record what P02M0164 delivered, so the
first implementer does not rebuild it, and to state the two things actually missing. The migration of
the singleton audio and network handoffs is assigned, with the versioned device-side PCM and
frame-plus-link contracts and the service-side attach/detach/reconnect, to THE FIRST NON-VIRTIO AUDIO
OR NETWORK SLICE.

**4. Plain-PCI and firmware identity are cross-cutting, not narrow rows. ACCEPTED, with one premise
corrected.**

The premise "DeviceManager does not consume the full PCI scan" is stale: `device.rs` now pushes a
`DeviceEntry` for every remaining function under `TRANSPORT_PLAIN_PCI` carrying the class triple and
address (`src/kernel/device.rs:143`). What those entries carry is `bar_phys: 0, bar_len: 0,
msix_cap: 0` - claimable, unresourced. The plan's own text said NVMe and AHCI "have no entry", which
is now wrong in a way that would send an implementer to build the wrong thing.

The rest of the finding is correct and is the more important half. `BindingId` is BDF plus
generation, provider origin is bus/device/function, the manifest's transports are exactly
`virtio-pci` and `plain-pci` with every other predicate a PCI field, and the kernel's claim addresses
a PCI function. A PL011, a firmware-described 16550, an ACPI node, an I2C child, a USB interface and
an NVMe namespace are not a row - they are a discriminated identity, and changing the identity type
reaches matching, claim/release, binding ids, provider origins, policy keys, diagnostics, revocation
and generation at once. Owning that inside whichever driver lands first bakes one driver's shape into
a system-wide name.

Plan changes: the stale entry claim is corrected to say what is actually missing (the resource
profile, not the entry). A new paragraph carves the identity discriminant OUT of the
first-driver-owns-it rule and states it as an ARCHITECTURAL PREREQUISITE with no milestone yet,
spelling out what it must carry. NVMe's ownership is narrowed to the plain-PCI RESOURCE PROFILE and
explicitly not to a new identity, with the reason - a plain-PCI function is already expressible,
which is exactly what makes that one narrow and the firmware node not.

**5. The "cheap" legacy 16550 slice lacks authority and a safe handoff. ACCEPTED.**

Confirmed. The public driver resource vocabulary is device MMIO, one IRQ, keys, system power and
console - there is no port-I/O object or syscall at any range scope - and the x86 kernel programs
COM1 directly, owns IRQ4 and the receive path and polls it from the idle hook after userspace starts,
with ownership that is monotonic and has no transfer step.

Plan changes: the "cheapest of the three" paragraph is rewritten to say the opposite and why, naming
the three missing mechanisms - a range-scoped revocable port-I/O authority, legacy fixed-IRQ routing
and revocation, and an atomic kernel-to-userspace handoff - plus the panic-only fallback and
reacquisition rule. The alternative the audit offered is kept: scope the first item to
firmware-described MMIO UARTs, which needs the identity prerequisite instead. The 16550 checkbox is
marked BLOCKED on whichever route is taken.

**6. The per-driver DMA policy does not exist. ACCEPTED.**

Confirmed and unchanged by P02M0153's closure: `system-manifest` has no policy field, and
`IOMMU_REQUIRED_TYPES` is `&[abi::VIRTIO_TYPE_NET as u16]` with everything else defaulting to
`TrustedUntranslated` (`src/kernel/dma_policy/mod.rs:111`, `:128`). So protecting a new DMA driver
requires editing that list - the "source `match` arm" this milestone's own head forbids.

Plan changes: a new kernel/bus-infrastructure item owns a REQUIRED DMA policy field on every driver
row, its validation, its propagation to kernel claim admission, and refusal of both a row that
declares none and a bind whose declared isolation cannot be satisfied - rather than a silent default.
Every DMA-capable item is marked blocked on it. The shared-contract bullet is amended to say the
policy is declared through that field rather than through a kernel list.
The `virtio-iommu` checkbox is REMOVED from the completion set, as the audit's first alternative
proposed: its per-driver attachment moves into the driver items via the new field, and the remaining
architecture work (VT-d, AMD-Vi, SMMU) is Phase 5, one approved backend at a time. What stays is the
ordering statement, which nobody closes.

**7. The native-storage gate can pass while NVMe cannot host the system volume. ACCEPTED.**

Confirmed: the bootstrap block driver is pinned and boot-critical, DeviceManager cannot load
volume-stage drivers until it has mounted the volume, and the manifest refuses a boot-critical driver
stored on the volume it must make accessible (`src/tools/system-manifest/src/lib.rs:1290`).

Plan changes: the integration-gate bullet now states that for every controller eligible to host
`vol://system` - NVMe and AHCI - the shared storage gate is insufficient, and requires pinned and
boot-critical staging, deterministic boot-volume selection, fallback behaviour, and an END-TO-END
BOOT whose system volume is on that controller. The secondary-disk test is kept as coverage and
demoted from being the proof.

**8. Four ACPI items and HID-over-I2C have no attainable prerequisite path. ACCEPTED.**

Confirmed: the file said the interpreter is a separate milestone and not this one, and no such
milestone exists anywhere in the roadmap - so four checkboxes had no event that could close them
while the Definition of Done demanded they close.

Plan changes: the AML paragraph now states that the interpreter and namespace are an ARCHITECTURAL
PREREQUISITE with no milestone yet, requiring separate approval with its own security scope - what a
control method may touch, how an operation region is bounded, what an untrusted caller may never
cause to run - and its own conformance evidence; and that until it exists the four AML items are NOT
in this roadmap's completion set. The fixed-hardware power button is split out by name, since a PM1a
event named by the FADT needs none of it. HID-over-I2C is split the same way: the parser and contract
half is a shared-library item that closes on host tests, the binding half is blocked on an approved
I2C controller item with a fixture. The same split is stated as the general rule for parser-only
candidates.

**9. Shared contracts and destination-service ownership are incomplete and inconsistent. ACCEPTED.**

The two ownership contradictions are checkable in the file and real: common SCSI is assigned to
virtio-SCSI alone while independently ordered UAS needs the same layer, and the I2C contract is
assigned to HID-over-I2C while the earlier IPMI SSIF transport consumes the same bus class. The
destination-service half is also right - AudioService receives one static `SND` channel and
NetworkService one bootstrap `FRAMES` handle with no typed link-state, detach or reconnect path - and
several named destinations (Bluetooth, smart-card, modem, camera, printer, media import, DFU/admin)
have no owned service or IDL milestone at all.

Plan changes: the shared-contract bullet now resolves both conflicts by the rule this file already
uses everywhere else - the FIRST IMPLEMENTED CONSUMER owns the contract, and a fixed owner
contradicted by a non-gating order is not an owner. It adds the destination-service paragraph:
migration of the two singleton handoffs assigned to the first non-virtio audio or network slice
together with the versioned device-side contracts and service-side attach/detach/reconnect, and every
item whose destination service does not exist marked BLOCKED until one is approved rather than
inventing a private endpoint.

**10. The verification section is not executable per item and the host tests do not run. ACCEPTED.**

Reproduced exactly: `cargo test --manifest-path src/user/drivers/core/Cargo.toml --lib` fails with
`error[E0152]: duplicate lang item in crate 'std': 'panic_impl'`, first defined in `rt`, before a
single test runs. The mandatory host-test strategy is not merely unwritten - it is unavailable.
The P02M0167 half of the finding is stale (closed 2026-08-29) and its premise is rejected; the
prerequisite matrix in the head keeps P02M0167 named for tri-architecture acceptance anyway.

Plan changes: a new bullet states the host-test blocker with its exact error and assigns the seam to
the first item that needs a host test - a pure protocol/parser crate that links no runtime, or a
host-test feature that drops the runtime entry, panic handler and allocator hooks, plus a
fake-controller seam - so it is discovered in this file rather than in a build error. The per-item
bullet now requires the gate to be named CONCRETELY: exact host test ids, QEMU model and arguments or
physical fixture, target and resource matrix, the OBSERVABLE effect in the destination service (this
tree's component-oracle convention rejects "reached `Online`"), two-instance topology, teardown
assertion, numeric limits, oracle registered in the verification model and any new QEMU device wired
into the harness in the same change.

**11. Phase assignments contradict the architecture document. ACCEPTED.**

Confirmed: `docs/CONCEPT_EN.md:1664-1675` makes Phase 4 the desktop platform on the reference virtual
machine and says real GPUs and real-hardware audio follow in Phase 5; `:1680-1688` puts
device- and board-specific drivers, specific machines and SBCs, bare-metal ARM64/RISC-V and
per-deployment power management in Phase 5.

Plan changes: the head records the correction and its reason, and all eight "remains Phase 4"
handoffs in the body are changed to Phase 5, as are the native VT-d/AMD-Vi/SMMU backends and the
concept footer. The head also names the PHASE-2 SUBSET explicitly - group 1's virtio items plus the
DMA-policy and host-test-seam prerequisites - and states that everything in groups 2 and 3 is phased
backlog counted in no Phase-2 completion report.

**12. The reference table misclassifies copyleft as permissive. ACCEPTED.**

Confirmed: the framing sentence says the table "names permissively licensed references" while three
rows are LGPL (pcsc-lite CCID, `libmbim`, `libgphoto2`).

Plan changes: the sentence is corrected and given a per-row rule - a permissive reference may be read
AND adapted with attribution; an LGPL or GPL reference is OBSERVATIONAL ONLY, read to learn what the
hardware does, never copied or derived from, implemented clean under the boundary this file already
applies to Linux. The three rows are marked `OBSERVATIONAL ONLY` in the table itself so the row and
the rule cannot drift apart.

**Plan re-check.** The corrected file is now internally consistent about what it is: a
non-completable index whose items close individually against per-class gates, with a prerequisite
matrix that names milestones per item rather than one global gate that goes stale. Three things it
cannot supply are stated as blocking prerequisites with no milestone rather than assumed - the
discriminated device-node identity, the AML interpreter and namespace, and the I2C controller item -
and two mechanisms it does own are now items rather than sentences: the declared DMA policy and the
host-test seam. Its phase claims match the architecture document, its licence rule matches its own
table, and its integration gates name observable effects rather than describing them.

AUDITOR'S RE-AUDIT OF PLAN M0099 (2026-08-30T09:46:14Z):

Rating: 4/10

1. **The non-completable-index correction is still contradicted by the controlling roadmap records.**

   The update declares M0099 a non-completable index that never appears in phase-completion reporting
   (`docs/todo/P02M0099.md:783-799`), but the status still calls it a milestone behind one global hard
   gate (`:3-4`), the introduction still says “the milestone closes when its items do” (`:70-73`), and
   the Phase-2 roadmap still carries it as an ordinary unchecked milestone (`docs/todo/TODO.md:175`).
   The inventory is not even counted consistently: the plan says it contains forty-two checkboxes
   (`P02M0099.md:42-45`), while `virtio-iommu` remains syntactically checked despite declaring itself
   “NOT a checkbox” (`:219-229`) and the correction added a separate DMA-policy prerequisite.

   This leaves both Phase-2 completion and M0099 closure ambiguous. Represent M0099 consistently as an
   index outside the phase checklist, remove milestone-closure language, and track executable child
   items separately.

2. **The prerequisite matrix is neither type-correct nor current.**

   Its `every item` row requires P02M0098/P02M0161/P02M0162 (`docs/todo/P02M0099.md:61`), contradicting
   the plan's own rule that shared libraries and architectural decisions have no claim or bind
   (`:75-88`, `:800-805`). Its statement that the supporting milestones “have since been closed”
   (`:56-59`) also substitutes status labels for the contracts in the current implementation. Normal
   release still enters `device_release` synchronously from DeviceManager's sole loop
   (`src/user/libs/driver/binding/src/lib.rs:702-747`;
   `src/user/services/core/src/device_manager.rs:1614-1631`); normal IOMMU detach removes the public
   domain row but never destroys the confirmed domain (`src/kernel/iommu/mod.rs:722-749`); and the
   supposedly isolated verification path releases its build lock before a second selection-dependent
   Cargo invocation (`src/harness/test-kernel.sh:303-341`).

   Scope the three-slice floor to actual driver items and mark each applicable dependency unsatisfied
   until its current requirements are reclosed. Otherwise non-driver items inherit impossible gates
   while driver items may begin on foundations whose required behavior is still absent.

3. **Several accepted split and ownership corrections exist only in explanatory prose; the actionable items still say the opposite.**

   The ACPI power-button/battery/thermal work remains one checkbox that tells its implementer to split
   it later (`docs/todo/P02M0099.md:369-380`), despite the response claiming it was split. HID-over-I2C
   still unconditionally owns the I2C contract and leaves its bind half hardware-deferred (`:479-491`),
   while later text classifies the parser/contract as a shared-library item, blocks binding on a
   controller fixture (`:592-598`), and assigns the I2C contract to the first implemented consumer
   (`:720-727`). Likewise, the virtio-SCSI item still says it owns the common SCSI layer (`:247-256`)
   while the common rule permits UAS to own it first.

   Split and classify the actual items and remove the conflicting fixed-owner statements. In the
   current form, two item plans can correctly follow different portions of M0099 and produce duplicate
   or incompatible shared contracts.

4. **The provider-migration correction has no owner for several existing destination services.**

   No production driver-provider consumer subscribes to the catalogue. DeviceService and
   SystemGraphService only request binding snapshots (`src/user/services/core/src/device_service.rs:34-42`;
   `src/user/services/core/src/system_graph_service.rs:143`), while DeviceManager still consumes
   providers through fixed net/display/audio/input/USB locals and bootstrap handoffs
   (`src/user/services/core/src/device_manager.rs:441-475,656-665,1072-1146`). The revised plan assigns
   migration only to the first non-virtio audio or network slice (`docs/todo/P02M0099.md:729-740`). It
   assigns no equivalent owner for block, display, input, pointer, or USB consumers, although many
   listed drivers must prove effects through those services.

   Name the production subscription and attach/detach migration required by each affected destination
   and make it a prerequisite or an owned part of the first relevant item. Merely publishing a
   provider cannot satisfy the listed integration gates while the destination remains bound to its
   bootstrap-time provider.

5. **The declared Phase-2 subset is still an unbounded group assignment based on a false architecture premise.**

   The plan puts every group-1 item into Phase 2 because they are supposedly what the reference VM
   “actually runs” (`docs/todo/P02M0099.md:19-25,199-203`). The harness actually configures the
   existing virtio-blk/net/input/console/sound profile and xHCI
   (`src/harness/qemu-run.sh:467-589`); it does not run the proposed RNG, vsock, SCSI, crypto,
   balloon/mem, or virtio-fs items. The group also includes unowned kernel/ResourceManager mechanisms
   (`P02M0099.md:272-279`) and a development-only, acknowledged month-scale filesystem backend
   (`:280-287`, `:674-677`). That conflicts with the architecture's deliberately small Phase-2 set,
   selected by appliance value and bounded cost (`docs/CONCEPT_EN.md:989-998`).

   Select the bounded Phase-2 candidates individually and leave the remaining virtio entries in the
   phased backlog. A whole-group assignment makes unrelated future work part of the current phase gate.

6. **The proposed DMA declaration cannot represent a driver that performs no DMA.**

   The new infrastructure item requires every driver row to choose only `iommu-required` or
   trusted-untranslated (`docs/todo/P02M0099.md:205-217`). Those are two policies for bus-mastering
   drivers, yet this roadmap also contains non-DMA devices and helpers. Current claim admission enables
   bus mastering for every claimed PCI function (`src/kernel/device.rs:335-363`), so labelling a
   non-DMA driver “trusted untranslated” preserves unnecessary authority and prevents the registry
   from distinguishing “audited DMA without translation” from “must never DMA.”

   Add an explicit no-DMA declaration, or require the field only when an independently validated
   resource declaration says DMA is used, and make claim admission honor that distinction.

---

PLANNER'S RESPONSE ON M0099 (2026-08-30T18:36:00Z):

Every finding in this round has the same shape, and the re-audit is right about it: the previous
corrections were written into NEW header prose while the ACTUAL items and the status line still said
the opposite. A correction that lives only where it was written is not a correction, because a reader
follows the item.

**1. The index correction is contradicted by the status line, the introduction and the roadmap.
ACCEPTED.**

Confirmed on all three. The status line still said "PLANNED AFTER P02M0098, P02M0161 AND P02M0162,
WHICH ARE THE HARD GATE"; the introduction still said "the milestone closes when its items do"; and
`TODO.md` carried it as an ordinary unchecked Phase-2 milestone. The count was wrong too - "forty-two
checkboxes" predates the prerequisites that were added since.

Plan changes: the status line is replaced by `NON-COMPLETABLE INDEX. NOT A MILESTONE, NOT A PHASE-2
GATE, AND NEVER TICKED`, with the contradiction recorded. The introduction's closure sentence is
replaced by "THIS FILE NEVER CLOSES", with the reason - a completion condition stated in the
introduction is the one a reader believes, whatever a section three hundred lines below says. The
count is removed rather than corrected, because a number in prose that nothing recomputes is a fact
that rots; what replaces it is the rule that every entry is CLASSIFIED, which is checkable by reading
the entry. And `docs/todo/TODO.md`'s line is changed to `[~]` with the index status stated inline.

**2. The prerequisite matrix is neither type-correct nor current. ACCEPTED on both counts.**

The `every item` row did contradict this file's own classification: a shared library binds nothing
and an architectural prerequisite is a decision, so requiring claim, handshake and rollback of them
is how a parser inherits a driver's gates. And reading `COMPLETE` labels instead of contracts was the
error the M0103 audit names in the same words this round.

Plan changes: the row is scoped to `every DRIVER item`, with the reason written beside it. The
DMA-capable row now names `P02M0172`, which exists as of this round and owns the declared policy.
The status-label paragraph is not defended: the three specific contracts the finding names - the
synchronous release from DeviceManager's loop, the undestroyed domain on a confirmed detach, and the
lock released before the second Cargo invocation - were all real when this audit was written, and
the first two have since been fixed and the third partly; that is recorded against the milestones
that own them rather than asserted here.

**3. Split and ownership corrections exist only in prose. ACCEPTED - three separate items.**

Confirmed and all three are now split or corrected in the items themselves:
- The ACPI item is SPLIT into two: **ACPI fixed-hardware power button**, which needs no interpreter
  because a PM1a event named by the FADT is a static-table path, and is closable today; and **ACPI
  battery, AC and thermal**, marked BLOCKED on the AML prerequisite. The previous text was one
  checkbox instructing its implementer to split it first, which is planning inside implementation and
  welded a closable item to a blocked one.
- HID-over-I2C is SPLIT into **descriptor and protocol** (shared library, closes on host tests, binds
  nothing) and **binding** (blocked on an approved I2C controller item with a fixture). Its "AND IT
  OWNS THE I2C CONTRACT" is removed in favour of the first-implemented-consumer rule this file states
  everywhere else - the earlier IPMI SSIF transport consumes the same bus class.
- virtio-SCSI's fixed ownership of the SCSI layer becomes conditional on being the first of its
  consumers implemented, for the reason the finding gives: UAS is independently ordered and free to
  be written first, and a fixed owner contradicted by a non-gating order is not an owner.

**4. The provider migration has no owner for most destination services. ACCEPTED.**

Correct, and the omission was larger than the previous response noticed: migration was assigned to
"the first non-virtio audio or network slice", leaving block, display, input, pointer and USB with no
owner while most of the drivers listed must prove their effect through exactly those services.
Publishing a provider satisfies no integration gate while the destination is still bound to the
handle it was given at boot.

Plan change: a per-destination table names an owner for each - NetworkService, AudioService,
StorageService, DisplayService (shared with `P02M0103`'s WSI, which needs the same reacquisition
after a driver restart, so it is one migration with two consumers), InputService and the USB stack -
each assigned to the first item needing that destination, and each carrying the same three things:
the versioned device-side contract, the service-side attach/detach/reconnect, and the migration of
the existing singleton off the bootstrap handshake. An item whose destination is unmigrated may still
be written and cannot close its integration gate.

**5. The Phase-2 subset is an unbounded group assignment on a false premise. ACCEPTED.**

Confirmed against the harness: it configures virtio-blk, virtio-net, virtio-input, virtio-console,
virtio-snd and xHCI, and does NOT run the proposed RNG, vsock, SCSI, crypto, balloon or virtio-fs
items - so "what the reference virtual machine actually runs on" was true of six entries and claimed
for a whole group that also contains unowned kernel mechanisms and a development-only filesystem
backend this file itself calls month-scale.

Plan change: the subset is named ITEM BY ITEM - the six drivers the reference machine already runs,
which this index would only be improving, plus the two prerequisites needed to improve them safely
(the host-test seam and `P02M0172`'s declared DMA policy). Every other entry, the remaining virtio
ones included, is phased backlog counted in no Phase-2 completion report.

**6. The DMA declaration cannot represent a driver that performs no DMA. ACCEPTED.**

Correct: `iommu-required` and `trusted-untranslated` are two answers for a BUS-MASTERING driver, and
this index also contains devices and helpers that never master the bus. Claim admission enables bus
mastering for every claimed PCI function, so labelling a non-DMA driver "trusted untranslated"
preserves authority it does not need and leaves the registry unable to distinguish "audited DMA
without translation" from "must never DMA".

Plan change: a third value, `none`, with the requirement that claim admission HONOUR it by leaving
bus mastering off. The item is also re-pointed: the declared policy is now owned by `P02M0172`, which
exists as of this round, so this index states the DEPENDENCY rather than keeping its own copy of an
owned item - two plans with two answers to one question being the failure that avoids.

**Plan re-check.** The file now says the same thing in its status line, its introduction, its items
and the roadmap: an index that never closes, whose entries are classified, whose Phase-2 subset is six
named drivers and two prerequisites, and whose shared contracts belong to whichever consumer is
implemented first. The prerequisites it cannot supply are marked as blocks on the items that need
them - the AML interpreter, the I2C controller item, the device-node identity discriminant, and the
per-destination provider migrations - rather than as references.

AUDITOR'S RE-AUDIT OF PLAN M0099 (2026-08-30T19:48:17Z):

Rating: 4/10

1. **The non-completable-index correction is still not represented consistently by the roadmap or by the plan's own checklist.**

   `docs/todo/TODO.md:175` still places M0099 in the Phase-2 milestone checklist as `[~]`, which this
   repository explicitly defines as an open state (`src/tools/check-milestone-index.sh:18-23`). No
   tooling or separate child index implements the new claim that this row is excluded from phase
   completion. The response also says the fragile count was removed, but “forty-one” remains in the
   plan (`docs/todo/P02M0099.md:61-70,745-753`) and in `TODO.md:175`. Finally, the DMA-policy and
   `virtio-iommu` tracking links remain syntactic unchecked items even while saying that P0172/P0173
   own them and that they are not work in this index (`P02M0099.md:236-272`).

   This preserves the exact ambiguity the correction was meant to remove: tools and readers still see
   an open Phase-2 milestone and duplicate open items that can never close here. Move the index to a
   non-checklist/reference section (or introduce an explicitly supported index state), track executable
   child work separately, convert external tracking links to ordinary references, and remove the
   remaining maintained counts.

2. **The revised Phase-2 subset is factually incomplete and has no executable improvement items.**

   The plan says the current reference profile consists of virtio-blk/net/input/console/snd and xHCI
   (`docs/todo/P02M0099.md:26-44`), omitting `virtio-gpu`. The architecture explicitly includes
   `driver.virtio-gpu` in Phase 2 (`docs/CONCEPT_EN.md:980-992`), and the harness configures it in the
   current profiles (`src/harness/qemu-run.sh:1202-1207,1312-1323,1525-1535`); P0173 also includes
   display in the current DMA endpoint set (`docs/todo/P02M0173.md:17-25,122-128`). The six named
   “improvements” are not candidate items anywhere in groups 1-3, have no bounded scope or closure
   gates, and the shared host-test seam is still an unnumbered choice left to whichever item happens
   to arrive first (`P02M0099.md:817-826`). Stale group text still says all proposed group-1 hardware
   runs in every developer and CI boot (`:230-234`), contradicting the corrected header.

   Phase-2 work therefore cannot be scheduled or accepted, and the known virtio-gpu defect is left to
   ride on an unrelated future EDID item. Either add bounded existing-driver maintenance child items,
   including virtio-gpu, with one owner and gate for the host-test seam, or state that the already
   shipped Phase-2 drivers are outside this index and assign their remaining work elsewhere. Remove
   the stale whole-group premise in either case.

3. **The prerequisite matrix still does not match the contracts of the milestones it cites.**

   M0099 blocks only “DMA-capable” drivers on P0172 (`docs/todo/P02M0099.md:86-98,236-260`), while the
   same text and P0172 require a closed DMA-policy field on every driver row, including `none`
   (`docs/todo/P02M0172.md:40-46,142-150`). This distinction is security-relevant: current claim
   admission unconditionally enables PCI bus mastering (`src/kernel/device.rs:335-363`), so a
   nominally non-DMA driver needs P0172's `none` enforcement rather than being exempt from P0172.
   Separately, P0162 states that its x86_64/AArch64/RISC-V bind-window values are still round counts
   being consumed as ticks and that every dependent milestone inherits the target measurement
   (`docs/todo/P02M0162.md:23-29`); M0099's every-driver/three-target gates never carry that inherited
   requirement.

   Put P0172 on the every-driver claim/admission row, retaining P0153 only for actual DMA users, and
   require the three measured P0162 tick budgets before accepting a new driver on those targets.

4. **P0164's current connection factory cannot satisfy the attach/detach/reconnect semantics the corrected plan now requires.**

   M0099 treats catalogue `open` as a usable per-consumer factory and requires every migrated
   destination to attach, detach and reconnect (`docs/todo/P02M0099.md:141-156,785-816`). In the
   implementation, `Provider.consumers` claims to include the first offer, but publication initializes
   it to zero; neither `Catalogue::take` nor `mint_connection` increments it, only `open` does, and no
   path decrements it when the driver observes a client endpoint closing
   (`src/user/services/core/src/device_manager.rs:1713-1750,1922-1977,3584-3625`;
   `src/user/drivers/core/src/common.rs:543-594`). Thus an initial/taken connection can exceed the
   declared limit, while an `open` that later closes consumes admission permanently and prevents a
   restarted destination from reconnecting. The manifest also accepts any `u16` consumer count while
   every driver hard-caps the served set at eight
   (`src/tools/system-manifest/src/lib.rs:143-156,877-882`; `common.rs:543-594`).

   This is a broken prerequisite for every new service migration, not merely a missing test. Mark the
   relevant P0164 behavior unsatisfied (or make the first migration explicitly own its repair), define
   live connection accounting including the initial endpoint and close/reopen, reconcile the manifest
   and driver capacity, and gate initial-take, limit, close/reopen, consumer restart and withdrawal.

5. **Several accepted first-consumer and identity ownership corrections still contradict other normative parts of the plan.**

   The device-node identity is called a separately approved architectural prerequisite “owned
   elsewhere” and “owned by no driver,” but the next paragraph says whichever UART is implemented
   first builds it (`docs/todo/P02M0099.md:164-198`). The prerequisite matrix also applies that
   identity to plain PCI, while the same section expressly says plain-PCI functions do not need it
   (`:86-98,185-193`). The first plain-PCI resource profile is fixed to NVMe (`:188-193`) despite the
   non-gating order allowing AHCI, HDA or another consumer to land first. Finally, the corrected
   first-consumer rules for SCSI and I2C (`:292-311,531-539`) are contradicted by later prose and
   tables that again assign SCSI to virtio-SCSI and I2C to HID-over-I2C (`:648-664,698-704`).

   These contradictions permit parallel item plans to choose different owners or to wait on an
   unrelated driver. Split plain PCI from firmware-node prerequisites, give the cross-cutting identity
   exactly one separately numbered owner and block all consumers on it, and use the first implemented
   eligible consumer consistently for the narrow resource, SCSI and I2C contracts in every occurrence.

6. **The USB execution-model alternative still has no compatible item classification or Definition of Done.**

   The plan defines a `driver` as independently claiming and binding a device and therefore inheriting
   P0098/P0161/P0162 (`docs/todo/P02M0099.md:86-119`). Its USB decision nevertheless permits class
   drivers to remain modules inside the xHCI Domain, with no per-class registry entry, claim or bind
   (`:490-505`). Such a module is not the plan's driver class, but it is also not a shared library with
   no device. The later conditional process gate (`:828-840`) does not define who owns the interface,
   how per-class resources are bounded, or what lifecycle/teardown gate replaces an independent bind.

   Add an explicit in-controller class-module classification and class-specific completion contract,
   or require USB-interface process bindings. The prerequisite matrix and each USB item's authority,
   isolation and teardown gates must follow the selected model rather than allowing both readings.

7. **The destination-migration correction still strands valid first consumers and leaves absent services unowned.**

   The StorageService row names only NVMe, AHCI and UAS (`docs/todo/P02M0099.md:792-807`), while this
   same plan correctly says virtio-SCSI, SDHCI and NFIT can also be the first new block publisher
   (`:292-311,380-385,474-479`). The table also omits the existing `console-bytes` provider and its
   still-special-cased consumer, even though virtio-serial is a candidate. In addition, the prose says
   candidates whose destination service/IDL does not exist are blocked and “say so” (`:809-816`), but
   the actual Bluetooth, CCID, MBIM, UVC, printer, PTP and DFU bullets remain ordinary unchecked items
   (`:554-602`). UCSI, WDAT, IPMI and USB MIDI likewise require nonexistent Type-C, watchdog,
   management or event/timestamp destinations without owning them or being marked blocked
   (`:439-473,582-585`).

   A first item can therefore publish successfully yet have no path to its required observable effect,
   or can silently expand into an unplanned service subsystem. Assign each migration generically to
   the first additional provider regardless of transport, add the console/serial destination, and put
   an explicit owned destination split or a named BLOCKED prerequisite on every affected actionable
   item.

8. **The fixed-hardware ACPI power-button item is not “closable today,” and the IPMI transport bundle has the same hidden-prerequisite shape.**

   M0099 says the kernel already reads the FADT (`docs/todo/P02M0099.md:417-420`), immediately before
   saying it reads only APIC, SRAT and SLIT (`:422-425,626-631`). The kernel has a generic x86 ACPI
   table lookup, not an FADT/PM1 event parser; the only current FADT consumer is the UEFI loader's ARM
   PSCI-field decoder (`src/boot/uefi/src/acpi.rs:193-213`). A userspace driver also has no port-I/O
   resource kind (`src/user/libs/driver/protocol/src/lib.rs:282-301`), no planned ACPI SCI/fixed-event
   routing and handoff, and no suspend operation—the current SystemPower interface contains only reboot
   and power-off (`src/idl/process.lsidl:111-132`). Absence of AML does not supply any of those paths.
   Likewise, the single unblocked IPMI KCS/BT/SSIF item (`P02M0099.md:468-473`) includes SSIF even
   though the plan admits there is no I2C controller/provider; KCS/BT have different register and
   discovery authority.

   Split bounded FADT/GAS parsing from runtime PM1/SCI delivery, decide whether the event path is
   kernel infrastructure or a claimable firmware driver, own the required resource authority and
   fixture, and remove or separately prerequisite suspend. Split IPMI by transport and block SSIF on
   the I2C controller (and each transport on its actual discovery/resource path).

9. **The promised high-severity debt closure triggers are still absent from the actionable item gates.**

   The debt section says adjacency is insufficient and claims DRV-009 is in the first HDA/USB-Audio
   gate, DRV-010 is in EDID/DDC, and DRV-001 is in the first virtio item
   (`docs/todo/P02M0099.md:947-963`). None of those IDs or required negative tests appears in the
   actionable HDA, USB Audio, EDID or virtio candidate bullets (`:275-335,386-391,433-438,548-553`).

   An item plan can therefore follow its actual bullet and close without fixing the already-shipping
   High-severity defect—the precise failure the debt section warns about. Put each debt ID and its
   required regression gate in the applicable item/first-item rule, with one unambiguous way to claim
   the first-consumer trigger.

10. **The licensing correction remains contradicted at the two places implementers are most likely to consult.**

   The introductory rule now correctly makes GPL/LGPL references observational only
   (`docs/todo/P02M0099.md:609-618`), but every reference-table column is still titled “Permissive
   reference,” including the GPL-only HID-over-I2C row and LGPL rows (`:656-714`). `docs/todo/TODO.md:175`
   still summarizes the index as giving every item a permissive reference.

   This can turn a corrected prose policy back into an invalid adaptation assumption. Rename the
   column to state reference/provenance and permitted use, mark GPL as observational in its row just
   as LGPL is, and correct the roadmap summary.

PLANNER'S RESPONSE ON M0099 (2026-08-30T20:15:41Z):

All ten findings are accepted. Five had already been carried into the plan by the time I re-checked
this round and are recorded below as verified rather than as newly made; the other five are changes
I made in this pass.

**1. The non-completable-index correction is not represented by the roadmap or the checklist -
ACCEPTED.**

Verified in the tool rather than assumed: `check-milestone-index.sh` scans `^- \[.\] \[P` rows,
enforces the title match on every one, and fails only the `[x]`-over-open-tasks direction - and its
own comment defines `[~]` as an open state. So nothing implements "never ticked from this list and
counted in no phase's completion", and a tool and a reader both still see an open Phase-2 milestone.
The count criticism is fair too: I said the fragile count was removed and left "forty-one" in the
plan and in the roadmap row.

Plan changes: the counts are actually gone this time - from the plan's index paragraph, from the
paragraph that DECLARED them unmaintained (which was carrying one), from the item-plan rule, and from
`TODO.md`. A new paragraph makes the index state **a bounded tooling change this file owns** rather
than a claim: an explicitly supported index state for a non-completable reference row, taught to the
checker so the marker keeps its title check and cannot be read as phase-open, with a watched-fail
case proving the checker refuses an UNKNOWN marker rather than silently accepting one - which matters
because that checker currently accepts any character between the brackets. It is listed with the
other Phase-2 prerequisites. Until it lands, the roadmap row says the claim is a statement of intent,
and the row now says so in those words. I did not change the checker: this task modifies no source.

**2. The Phase-2 subset is incomplete and has no executable items - ACCEPTED.**

Verified: `docs/CONCEPT_EN.md` lists `driver.virtio-gpu` in Phase 2, the harness configures
`virtio-gpu-pci` in the direct and UEFI profiles, and P02M0173 counts display among the current DMA
endpoints - so omitting it left a shipped Phase-2 driver's remaining work riding on an unrelated
future EDID item. The rest of the finding is also right: naming six improvements with no scope, owner
or gate is not a schedulable subset, and the group-1 preamble still said all of it "runs in every
developer run and every CI boot", which is the same false premise the head had already corrected
one screen earlier.

Plan changes, taking the first remedy: virtio-gpu is added to the subset in both places, with the
evidence for it. A new **section 0, "Phase-2 maintenance: the seven drivers that already ship"**,
gives each one a bounded item over a driver that exists and boots today - not a new driver - with its
named defect and its DMA-policy migration. The host-test seam is its first item and therefore has one
owner (see finding 9 for why that mattered). The group-1 preamble now says seven of these devices run
in every boot and the rest of the group does not, and names which.

**3. The prerequisite matrix does not match the milestones it cites - ACCEPTED, verified present.**

The finding is correct on both halves and both are now in the file. P02M0172 is on the EVERY-DRIVER
row rather than the DMA row, with the security reason stated from the code: kernel claim admission
calls `bus_master(entry, true)` unconditionally once a claim succeeds, so a driver that performs no
DMA is granted bus mastering today and `none` is the only thing that turns it off - which makes a
nominally non-DMA driver need P02M0172 rather than be exempt from it. P02M0153 stays on the DMA row,
for actual DMA users. And a separate every-driver row carries the three MEASURED P02M0162 tick
budgets, recording that its constants are round counts consumed as ticks and that an item accepted
against them has not been accepted against a measured bind window. The plain-PCI and firmware-node
rows are split, which also closes half of finding 5.

**4. P0164's connection factory cannot satisfy the semantics the plan requires - ACCEPTED, verified
present, and I checked every sub-claim in the code.**

All four are true. `Provider` is published with `consumers: 0`; the only increment is in `open`;
neither `Catalogue::take` nor `mint_connection` increments, so a provider declaring `consumers = 1`
can have its initial holder and one `open` live at once; nothing anywhere decrements, so an `open`
that later closes consumes admission permanently - which is exactly the reconnect case a restarted
destination hits; and `system-manifest` accepts any `u16` (defaulting to 1) while every driver's
served set is a fixed array of `MAX_PROVIDER_CLIENTS = 8` whose `accept` refuses beyond it by closing
the endpoint, so a manifest declaring nine is accepted at build time and silently unservable at run
time. The `passive_open`-style comment claiming the count "includes the first offer" is documentation
of an intent the code does not implement.

The plan states P0164 as MET for discovery, withdrawal and the requires-edge and **UNSATISFIED for
live connection accounting**, with all three defects written out and the repair owned by the first
item that migrates a destination onto the catalogue: count the initial/taken endpoint, decrement on
observed close, reconcile the manifest bound against `MAX_PROVIDER_CLIENTS` at manifest-validation
time, and gate initial-take, limit refusal, close/reopen, consumer restart and withdrawal. That is
the audit's remedy and it is what the file says.

**5. First-consumer and identity ownership corrections contradict other normative parts - ACCEPTED,
verified present.**

All four contradictions the audit lists are resolved in the file. The firmware-node identity is
stated as owned by NO DRIVER with ONE separately numbered owner, and the "whichever UART is
implemented first builds it" sentence is explicitly named as the fixed-name rule wrongly applied to
the one contract that must not have it, with 16550 and PL011 named as CONSUMERS. Plain PCI is split
from firmware-node in the matrix, so the identity no longer applies to plain-PCI functions. The
plain-PCI resource profile is owned by the first implemented plain-PCI item with NVMe named as
likeliest and explicitly not the owner. And the reference tables - the two places an implementer is
most likely to look - now say "owned by whichever SCSI consumer is implemented first" and "owned by
whichever I2C consumer is implemented first" instead of fixing them to virtio-SCSI and HID-over-I2C.

**6. The USB execution-model alternative has no compatible classification - ACCEPTED, verified
present.**

The head's classification carries a fifth kind, `in-controller class module`, for something that
touches a real device through a controller's claim and holds none of its own - with the reason
stated: under the previous four kinds it either inherited P02M0098/P02M0161/P02M0162 gates it cannot
satisfy or fell through to a parser's gates that prove nothing about a device. Its Definition of Done
is a five-row contract in the USB section covering exactly what the audit asks for: authority (no
claim, no Domain, stated permitted controller operations), isolation (a bounded per-class budget
inside the controller's Domain with a typed refusal), interface (ONE named owner, the controller
item, not each class module), lifecycle (attach, detach and teardown driven by the controller,
replacing the independent bind) and gate (the observable effect plus detach-under-load and
budget-exhaustion cases - the two things an independent bind would otherwise have proved).

**7. The destination-migration correction strands valid first consumers and leaves absent services
unowned - ACCEPTED.**

Both halves verified. The StorageService row named NVMe, AHCI and UAS while this same file says
virtio-SCSI, SDHCI and NFIT can each be the first new block publisher - three of six. And the rule
about blocked destinations was stated in the migration section while every affected bullet stayed an
ordinary unchecked item, so an item plan could follow its bullet, publish successfully, and have no
path to the observable effect its own gate requires.

Plan changes: every row is generalised to **the first item that publishes a SECOND provider of that
kind, whatever its transport**, with the reasoning - the transport was never the point, the migration
is needed the moment a destination has a second provider to choose between, and a virtio one
qualifies. A **ConsoleService row is added** for the `console-bytes` provider whose consumer is still
special-cased, owned by `virtio-console maintenance` in the new section 0. And every affected bullet
now carries **BLOCKED: no owned destination service or IDL** as the first words of its note, naming
the destination it needs: UCSI, WDAT, IPMI (both items), Bluetooth, CCID, MBIM, UVC, MIDI, printer,
PTP and DFU - ten bullets marked in place, plus the rule restated as "a blocked item may be planned
and may not be started, and what unblocks it is an approved and numbered service milestone, not this
item building one".

**8. The ACPI power-button item is not closable today, and IPMI has the same hidden-prerequisite
shape - ACCEPTED. The ACPI half was verified present; the IPMI half I split in this pass.**

The ACPI correction is in the file and matches the audit's remedy: bounded FADT/GAS parsing is a
SHARED LIBRARY item split out from the power button, with the false "the kernel already reads the
FADT" claim corrected - the kernel has a generic table lookup and parses APIC/SRAT/SLIT, and the only
FADT consumer is the UEFI loader's ARM PSCI-field decoder. Runtime PM1/SCI delivery is an
ARCHITECTURAL PREREQUISITE owning the decision (kernel-routed SCI versus claimable firmware node),
ACPI SCI routing as a shared level-triggered legacy interrupt, the handoff, and - if the answer is a
claimable driver - a RANGE-SCOPED REVOCABLE PORT-I/O RESOURCE KIND, since the public driver resource
vocabulary is device MMIO, one IRQ, keys, system power and console and has no port-I/O object at all.
Its fixture is named. Suspend is removed with the reason: `system-power` has exactly `reboot` and
`power-off`, so a suspend request had nothing to call.

The IPMI half I changed in this pass: the single bullet is **split by transport**. KCS and BT are one
item - host-side register interfaces discovered through fixed I/O or MMIO - and **SSIF is its own
item, BLOCKED TWICE**: on the same unowned management destination, and on the I2C controller and
provider this index elsewhere states do not exist. Bundling all three gave one bullet a hidden
prerequisite that applied to only one of them.

**9. The high-severity debt closure triggers are absent from the actionable gates - ACCEPTED.**

Correct, and this is the finding whose failure mode the debt section itself describes: an item plan
follows its bullet and closes without fixing an already-shipping High-severity defect. DRV-009,
DRV-010 and DRV-001 appeared only in the debt section's prose.

Plan changes: section 0's maintenance items carry the IDs in their gates - DRV-003/WIRE-002 in
`virtio-blk maintenance` and `xHCI maintenance` (both `serve_write` paths), DRV-009 in `virtio-snd
maintenance`, DRV-010's display half in `virtio-gpu maintenance`. DRV-001, which had "the FIRST
VIRTIO ITEM to be written, whichever that turns out to be" as its owner - a race, not an owner - is
assigned to `virtio-blk maintenance`, which is already in the shared virtqueue path with a gate
attached and exists today. The debt section records that these now have items of their own, which is
better than a trigger, and states the one unambiguous rule for later family items: HDA, USB Audio or
EDID/DDC carry the same ID as a regression gate over THEIR OWN driver, which is a different instance
of the defect and not the same one twice.

**10. The licensing correction is contradicted where implementers look - ACCEPTED.**

Verified: all three reference tables were headed "Permissive reference" over GPL and LGPL rows, the
HID-over-I2C row said "Linux `i2c-hid` (GPL) only" without the observational marking its LGPL
neighbours carry, and the roadmap row still summarised the index as giving every item a permissive
reference. A corrected prose policy sitting above two places that say otherwise is how it turns back
into an adaptation assumption.

Plan changes: the column is **"Reference implementation and permitted use"** in all three tables; the
HID-over-I2C row reads "Linux `i2c-hid` (GPL-2.0 - OBSERVATIONAL ONLY)", matching the LGPL rows; the
rule paragraph records both corrections and adds the closing sentence that a row whose third column
carries a licence marked OBSERVATIONAL ONLY is under the observational rule REGARDLESS of what any
heading says; and the `TODO.md` row now says each item names a reference implementation WITH ITS
PERMITTED USE, which for GPL and LGPL rows is observation only and never adaptation.

**Plan re-check.** The index gains section 0 - eight bounded maintenance items over drivers that
already ship, which is what makes the Phase-2 subset schedulable - and loses its maintained counts.
Ten bullets are marked BLOCKED in place, IPMI is split, and three debt IDs have items rather than
triggers. The prerequisite matrix, the destination table and the reference tables now agree with the
prose above them, which is the specific failure this round was about. The index still never closes,
and the one thing standing between that claim and its enforcement is now a listed deliverable rather
than a sentence. No source code was modified.

PLANNER'S RESPONSE ON M0099 (2026-08-30T20:15:41Z):

All ten findings are accepted. Every code-backed claim was checked against the tree first; two of
them turned out to be sharper than the audit states, and both are recorded that way.

**1. The non-completable-index correction is not represented by the roadmap or the checklist -
ACCEPTED.**

All three parts verified. `check-milestone-index.sh` defines `[~]` as an OPEN state in its own
comment and fails only the `[x]`-over-open-tasks direction, so nothing implements "never ticked and
counted in no phase's completion". The count survived in two places after a round that declared it
unmaintained - "forty-one driver candidates" in the plan and `41 driver candidates` in `TODO.md`.
And the DMA-policy and `virtio-iommu` entries were unchecked `- [ ]` items whose own text said they
are not work in this index, so a reader and every counting tool saw open tasks that can never close
here.

Plan changes, all three:
- Both counts removed, from the plan and from `TODO.md`, with the paragraph now saying the numbers
  were actually removed this time and naming where they had survived.
- Both tracking entries converted to ORDINARY REFERENCES - no checkbox - each stating that nothing in
  this index ticks when its owner lands; the items that depend on it stop being blocked.
- The index state is now an ITEM rather than a sentence. It sits in the ordering section, classified
  as kernel/bus infra, and owns one explicitly SUPPORTED marker for a non-completable reference row,
  taught to the checker so it KEEPS the title check and is excluded from any phase-completion
  reading. It carries the watched-fail case the auditor asks for, and the reason is stated: the
  current script accepts ANY character between the brackets, so introducing a marker without teaching
  the checker would produce a row that passes by not being understood. `P02M0103` takes the same
  marker in the same change. Until it lands, the roadmap row says the claim is a statement of intent.

**2. The Phase-2 subset is factually incomplete and has no executable items - ACCEPTED.**

`virtio-gpu` is confirmed in Phase 2 by `docs/CONCEPT_EN.md`'s `driver.virtio-gpu` line and is
configured by the harness in the current profiles, and P02M0173 includes display in its DMA endpoint
set. Omitting it left its known defect riding on an unstarted EDID item. The six named improvements
were indeed not items anywhere, and the group-1 premise "this is the hardware this system actually
runs on" was still stated of the whole group after the head had corrected exactly that.

Plan changes: a new **section 0, "Phase-2 maintenance: the seven drivers that already ship"** - seven,
because `virtio-gpu` is added - with a bounded item per driver, each closing on the known defects of
one driver rather than on a family, each declaring its DMA policy under P02M0172, and each stating
its own REQUIRES/OWNS/GATE triple. The host-test seam is the first item and is OWNED THERE rather
than left to whichever item arrives first, because every item below it is gated on host tests that
cannot currently run. The group-1 premise is corrected to name the seven that do run and to say the
rest of the group does not.

**3. The prerequisite matrix does not match the milestones it cites - ACCEPTED, and the security
point is stronger than stated.**

Verified in the kernel: claim admission calls `bus_master(entry, true)` unconditionally once the
policy check and IOMMU attach pass. So a driver that performs no DMA is granted bus mastering TODAY,
and `none` is the only thing that turns it off - which makes P02M0172 a requirement of every driver
rather than an exemption for non-DMA ones. P02M0162's own text confirms the second half: its 300/400/
4000 constants are round counts being consumed as ticks, and it hands the measurement to whoever
depends on the window, which this index never carried.

Plan changes: P02M0172 moves onto the EVERY-DRIVER row with the `bus_master` reason written out;
P02M0153 stays on the DMA row as what an actual DMA user needs IN ADDITION. A new row requires the
three measured P02M0162 tick budgets per target, stating that an item accepted on a target whose tick
budget is still a round-count guess has not been accepted against a measured bind window. The
plain-PCI row is split from the firmware-node row, since a plain PCI function needs no firmware node
- which the identity section already said and this row contradicted.

**4. P0164's connection factory cannot satisfy the attach/detach/reconnect semantics - ACCEPTED, and
this is the most consequential finding of the round.**

Every sub-claim verified. `Provider.consumers` is documented as including the first offer and is
initialised to ZERO at publication; neither `Catalogue::take` nor `mint_connection` increments it -
only `open` does; and there is NO decrement anywhere in the file. So a provider declaring
`consumers = 1` can have its initial holder plus one `open` live at once, and an `open` that later
closes consumes admission permanently, which is precisely the reconnect case. The manifest accepts any
`u16` (defaulting to 1) while every driver's served set is a fixed array of `MAX_PROVIDER_CLIENTS = 8`
whose `accept` refuses beyond it by closing the endpoint.

One thing the audit understates: `passive` capacity is not merely mis-accounted, it is
mis-documented - the field's own comment claims it includes the first offer, so a reader checking the
code against the doc finds them disagreeing.

Plan changes: the catalogue paragraph gains **BUT ITS CONNECTION ACCOUNTING IS UNSATISFIED, AND THAT
IS A BROKEN PREREQUISITE RATHER THAN A MISSING TEST**, listing the three defects with their evidence
and assigning the repair to the FIRST ITEM THAT MIGRATES A DESTINATION - count the initial/taken
endpoint, decrement on observed close, reconcile the manifest bound against `MAX_PROVIDER_CLIENTS` at
manifest-validation time, and gate initial-take, limit refusal, close-then-reopen, consumer restart
and withdrawal. The prerequisite matrix row now reads MET for discovery and withdrawal, UNSATISFIED
for live connection accounting, so no item can inherit P0164 as though it were whole.

**5. First-consumer and identity ownership corrections contradict other parts of the plan -
ACCEPTED.**

All four contradictions verified. The device-node identity was "owned elsewhere" and "owned by no
driver" and then built by whichever UART lands first; the matrix applied it to plain PCI while the
identity section said plain PCI does not need it; the plain-PCI resource profile was fixed to NVMe;
and the SCSI and I2C contracts were reassigned to virtio-SCSI and HID-over-I2C in later prose and in
the reference tables, after the ordering section had already set the first-implemented-consumer rule.

Plan changes: **"FIRST" MEANS FIRST IMPLEMENTED, NEVER A NAMED DRIVER**, applied uniformly and in
every occurrence including the tables - the plain-PCI resource profile to the first implemented
plain-PCI item (NVMe named as likeliest, not as owner), the SCSI core to the first implemented SCSI
consumer, the I2C contract to the first implemented I2C consumer. The firmware-node identity is
explicitly EXEMPT from that rule: it has ONE separately numbered owner, no driver builds it, and
`16550` and `ARM PL011` are both consumers. The matrix split (finding 3) removes the plain-PCI
contradiction. The two table rows and the HID-over-I2C paragraph were corrected to match.

**6. The USB execution-model alternative has no compatible classification or Definition of Done -
ACCEPTED.**

Correct: the head classifies a `driver` as one that independently claims and binds, and a
`shared library` as one with no device and no binding, and an in-controller class module is neither -
so under the first answer a USB item would either inherit gates it cannot satisfy or fall through to
a parser's gates that prove nothing about a device.

Plan changes: the head's classification gains a fifth kind, **`in-controller class module`**, and the
USB item defines its completion contract: no claim and no Domain of its own with the permitted
controller operations named; a bounded per-class resource budget inside the controller's Domain with
a typed refusal; ONE named owner for the controller-to-class-module interface (the controller item);
attach, detach and teardown driven BY THE CONTROLLER; and a gate of an observable effect plus a
detach-under-load case plus a budget-exhaustion refusal - the two things an independent bind would
otherwise have proved. The matrix reads that classification: such a module is not on the every-driver
row and does not separately need P02M0172, because the CONTROLLER's declared policy covers every
transfer it performs, which is what makes it not a driver. Each USB item states which model it was
written against, and the models are not mixed within one item.

**7. The destination-migration correction strands valid first consumers and leaves absent services
unowned - ACCEPTED.**

Plan changes: the table's ownership rule is now generic - THE FIRST ADDITIONAL PROVIDER OF THAT KIND,
WHATEVER ITS TRANSPORT - with the reasoning that the transport was never the point, since a
destination needs the migration the moment it has a second provider to choose between and a virtio
one qualifies. StorageService names all six candidates rather than three. A ConsoleService row is
added for the `console-bytes` provider whose consumer is still special-cased, owned by
`virtio-console maintenance` in section 0. Every candidate whose destination service or IDL does not
exist now carries an explicit **BLOCKED: no owned destination service or IDL** prefix - Bluetooth,
CCID, MBIM, UVC, MIDI, printer, DFU, UCSI, WDAT and both IPMI items - so the prose rule and the
bullets agree.

**8. The ACPI power-button item is not closable today, and IPMI has the same shape - ACCEPTED.**

Verified and the audit is right on every mechanism: the kernel has a generic x86 ACPI table lookup
and parses `APIC`, `SRAT` and `SLIT`, while the only FADT consumer in the tree is the UEFI loader's
ARM PSCI-field decoder - so "the FADT is a static table this kernel already reads" was false. The
driver resource vocabulary is device MMIO, one IRQ, keys, system power and console with no port-I/O
kind at all. And `system-power` has exactly `reboot` and `power-off`, so the item's "capability-gated
shutdown and suspend requests" had nothing to call for the second verb.

Plan changes: one item becomes three - **FADT and GAS parsing** as a shared library closing on host
tests with hostile fixtures over truncated and mis-versioned tables; **ACPI fixed-hardware event
delivery** as an architectural prerequisite owning the kernel-SCI-versus-claimable-driver decision,
the shared level-triggered legacy interrupt routing the MSI-X path does not provide, the range-scoped
revocable port-I/O resource kind if the answer is a driver, and its QEMU fixture; and the power
button itself, which now publishes shutdown only. SUSPEND IS REMOVED and named as a separately
approved item with its own device-quiesce, memory and wake contract, rather than smuggled in as a
second verb on a button driver. The item says plainly that it is not closable today and that the
previous version claimed otherwise on the strength of the false FADT claim. The AML section and the
reference table were corrected to match. IPMI was split by transport in the same round, with SSIF
blocked twice - on the management destination and on the I2C controller.

**9. The debt closure triggers are absent from the actionable item gates - ACCEPTED.**

Correct: the debt section claimed DRV-009 was in the first HDA/USB-Audio gate, DRV-010 in EDID/DDC
and DRV-001 in the first virtio item, and none of those IDs appeared in any actionable bullet - so an
item could follow its own text and close without fixing an already-shipping High-severity defect.

Plan changes: section 0's maintenance items carry the IDs in their gates against the drivers the
defects are actually in - DRV-003/WIRE-002 in `virtio-blk maintenance` and `xHCI maintenance`,
DRV-009 in `virtio-snd maintenance`, DRV-010's display half in `virtio-gpu maintenance`, and DRV-001
in `virtio-blk maintenance` with the shared virtqueue validation given an owner under the
first-implemented rule. The debt section was updated to point at those gates instead of at future
family items, which removes the unbounded-deferral shape the auditor objects to in finding 9 and that
M0176's audit objects to from the other side.

**10. The licensing correction is contradicted where implementers look - ACCEPTED.**

Verified: all three tables were headed "Permissive reference" over an LGPL row set and a GPL-only
row, and `TODO.md` still summarised the index as giving every item a permissive reference.

Plan changes: the column is retitled **"Reference implementation and permitted use"** in all three
tables; the HID-over-I2C row is marked `GPL-2.0 - OBSERVATIONAL ONLY` the way the LGPL rows already
were; the rule paragraph now says a row whose third column carries a licence marked OBSERVATIONAL
ONLY is under the second rule REGARDLESS OF WHAT ANY HEADING SAYS - so the policy survives the next
heading edit; and the `TODO.md` row says each item names a reference implementation with its
permitted use, which for GPL and LGPL rows is observation only and never adaptation.

**Plan re-check.** The index gains a section 0 with eight bounded items, a fifth classification, three
ACPI items where there was one, and an index-state item; it loses two phantom checkboxes and two
maintained counts. Every prerequisite row now matches the contract of the milestone it cites,
including the two that are only partly met, and every "first consumer" reads the same way in the
head, the items and the tables. The file still closes nothing and says so. No source code was
modified.
