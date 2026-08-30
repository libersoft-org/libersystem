AUDITOR'S REVIEW OF PLAN M0173 (2026-08-30T16:21:53Z):

Rating: 6/10

The plan is unusually strong on the AArch64 early raw-DMA exception, closed endpoint censuses, interrupt-path evidence, generic lifecycle reuse, and fail-closed firmware transition. Three omissions still prevent the five-row matrix from proving its claim: it does not model the distinct test/shipping artifacts within a row, it leaves the RISC-V PCI host's bypass setting implicit, and it has no concrete pre-transition quiesce sequence for the UEFI xHCI endpoint.

## Material findings

1. **The five profile rows conflate mutually exclusive test and shipping artifacts.**

   **What is wrong:** The AArch64-direct-GICv2 and RISC-V-direct rows require both hostile EDU cases and ordinary shipping endpoint effects (`docs/todo/P02M0173.md:30-36`), but M5 creates only five profile keys and never defines constituent boots, artifact identities, or per-variant endpoint censuses (`:98-108`). EDU support and arbitrary PCI BAR access are test-only (`src/kernel/iommu/mod.rs:17-21` and the architecture PCI `function_bar` helpers), and AArch64/RISC-V test boots run `test_main()` and exit before production userspace (`src/kernel/arch/aarch64/boot.rs:898-910`; `src/kernel/arch/riscv64/boot.rs:549-567`). The harness also changes its endpoint set between test, ordinary, interactive, and development modes (`src/harness/qemu-run.sh:1308-1337`, `:1521-1546`). The existing x86 gate explicitly uses one hostile test-kernel boot and a second shipping-image boot for this reason (`src/tools/check-qemu-virtio-iommu-x86_64.sh:4-11`, `:94-103`, `:142-193`).

   **Why it matters:** One green base-profile key can bind hostile evidence to a test kernel that never exercises shipping drivers, or bind shipping effects to a kernel that cannot run EDU. Development/interactive-only bus masters can also disappear behind the base census despite the plan's promise of separate censuses.

   **Correction:** Define each catalog row as an explicit composite result, or split it into required subkeys. Name every boot phase, exact test versus shipping/development artifact digest, variant-specific topology/census, effect oracle, and result log. Require M0170's dossier to bind all constituent artifacts/results and to reject a row with any missing phase.

2. **RISC-V still relies on the PCI host bridge's bypass default.**

   **What is wrong:** M1 explicitly sets and mutates `default-bus-bypass-iommu=off` only for AArch64 (`docs/todo/P02M0173.md:51-55`). RISC-V `virt` exposes the equivalent on its GPEX host bridge rather than as that ARM machine property: the qualified local QEMU 10.0.11 reports `gpex-pcihost.bypass-iommu` with a current default of off, and accepts an explicit `-global gpex-pcihost.bypass-iommu=off`. The plan leaves that default unpinned.

   **Why it matters:** Reading the virtio-IOMMU controller's own bypass byte as off does not prove that the upstream PCI host routes endpoint DMA through the controller. A QEMU default or override change can permit untranslated DMA while controller logs and ordinary traffic still look healthy.

   **Correction:** Make the qualified RISC-V topology explicitly force GPEX host bypass off, inspect/assert the effective topology, and add the opposite-setting mutation. Record the QEMU capability/version through M0170 so unsupported versions refuse rather than silently omit the property.

3. **The firmware handoff has no xHCI quiescence contract.**

   **What is wrong:** Both UEFI topologies attach qemu-xHCI with USB storage/input devices (`src/harness/qemu-run.sh:1296-1306`, `:1509-1519`), and firmware may initialize it. M3 correctly says BME clearing alone does not drain issued work, but gives concrete completion/readback procedures only for virtio and NVMe (`docs/todo/P02M0173.md:68-76`). The current xHCI reset sequence exists only in the later userspace driver (`src/user/drivers/core/src/xhci.rs:484-487`), after the IOMMU transition it would need to protect.

   **Why it matters:** Firmware-issued xHCI DMA can remain live across BME clear and bypass-off. That creates exactly the unquiesced in-flight window the plan says is unacceptable and invalidates the UEFI enforcing claim.

   **Correction:** Require an early, pre-transition xHCI halt/reset with completion and register readback, plus a gate that fails when it is omitted or times out. If that cannot be implemented safely, remove xHCI/USB from the enforcing UEFI topology or refuse that profile; do not infer quiescence from BME alone.

