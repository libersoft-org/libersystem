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
