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


PLANNER'S RESPONSE ON M0173 (2026-08-30T19:36:07Z):

All three findings are accepted. Finding 2 was verified against the qualified QEMU on this machine
rather than taken on the audit's word, and the measurement changes the emphasis slightly.

**1. The five profile rows conflate mutually exclusive test and shipping artifacts - ACCEPTED.**

Confirmed, and it is a hard incompatibility rather than a tidiness problem.
`src/kernel/iommu/mod.rs:20` is `#[cfg(test)] pub mod edu;`, with the reason written beside it -
a production kernel carrying a bounded arbitrary-DMA engine would carry the capability this work
exists to remove. And both non-x86 test boots end at `crate::test_main(); super::exit_qemu(true)`
inside `#[cfg(test)]` (`arch/aarch64/boot.rs:902-910`, `arch/riscv64/boot.rs:555-568`), before
the production userspace chain below them. So a test kernel cannot exercise a shipping driver and a
shipping kernel cannot host EDU: a row demanding "hostile EDU cases plus ordinary shipping endpoint
effects" under one key is asking for evidence no single artifact can produce. The existing x86_64
gate already solves this the only way it can be solved, with one hostile test-kernel boot and a
second shipping-image boot.

Plan change: the matrix was rebuilt as a COMPOSITE table - profile row, phase, artifact, required
evidence - with the incompatibility stated above it as the reason. Every phase names its own artifact
digest, topology, endpoint census, effect oracle and result log; a row is green only when every phase
is, and a missing phase is a failure rather than a partial pass. The sentence about test, interactive
and development variants was strengthened: each variant a phase actually boots carries its own
generated census, and the base census may not stand in for a development-only bus master. M5 gives
each phase its own subkey and requires P02M0170's dossier to bind every constituent artifact and
result and to REJECT a row with any missing phase rather than reading its exit code. A line was added
to "What this milestone refuses" so a future reader does not collapse the phases back into one
command.

**2. RISC-V still relies on the PCI host bridge's bypass default - ACCEPTED, and measured.**

Verified directly against the qualified QEMU 10.0.11 on this machine rather than accepted as stated.
`qom-list-properties` for `gpex-pcihost` reports `bypass-iommu` as a bool with
`default-value: False`, and the ARM `default-bus-bypass-iommu` the plan named is a property of
`virt-machine` which RISC-V `virt` does not have. So the audit is right on both counts: the plan's
M1 sentence was architecture-specific without saying so, and RISC-V was left on an unpinned default.

The measurement refines the argument rather than weakening it. The default is currently OFF, so this
is not a live hole today - which is exactly why it is worth pinning: nothing in the gate would fail
if a future QEMU changed it, while the controller logs and ordinary traffic continued to look
healthy.

Plan change: M1 now says "pin the upstream host bridge on BOTH architectures, with the property each
machine actually has" - `default-bus-bypass-iommu=off` on the ARM `virt` machine and
`-global gpex-pcihost.bypass-iommu=off` on RISC-V - and states the reason: reading the controller's
own bypass byte as off does not prove the upstream host routes endpoint DMA through the controller.
It requires the effective topology to be asserted, the opposite-setting mutation on both
architectures, and the QEMU capability and version recorded through P02M0170 so a version without
the property REFUSES rather than silently omitting it. The Definition of done now says bypass is
confirmed off at the controller AND at the upstream host bridge, explicitly set rather than
defaulted.

**3. The firmware handoff has no xHCI quiescence contract - ACCEPTED.**

Confirmed. `qemu-run.sh:512-518` attaches `qemu-xhci` with `usb-kbd`, `usb-tablet` and
`usb-storage`, and both UEFI topologies include it; firmware may have initialized the controller and
issued transfers. M3 was internally inconsistent about exactly this: it correctly said BME clearing
alone does not drain issued work and then gave concrete completion and readback procedures only for
virtio and NVMe. The only xHCI reset sequence in the tree is in the userspace driver
(`user/drivers/core/src/xhci.rs`), which runs long after the transition it would need to protect -
so firmware-issued xHCI DMA can stay live across BME clear and bypass-off, which is precisely the
unquiesced in-flight window the item declares unacceptable.

Plan change: M3 now requires an EARLY pre-transition xHCI halt and reset on the enforcing UEFI
profiles - run stop, wait for HCHalted, reset, wait for the controller-not-ready bit to clear - with
register readback and a bounded timeout, and a gate that fails when it is absent or times out. It
takes the audit's fallback too: if that cannot be implemented safely, remove qemu-xHCI and its USB
devices from the enforcing UEFI topology or refuse the profile. The generalized sentence was
rewritten so the rule is stated once for all device classes: quiescence is never inferred from BME
alone, and a kernel with no safe reset path for any firmware-touched endpoint refuses the profile.
The Definition of done gained a clause requiring every firmware-touched endpoint to be quiesced by a
procedure written for its device class.

**Plan re-check.** The item count is unchanged at seven - all three corrections belong inside
existing items - and the plan is longer only where it now states facts an implementer would otherwise
have to rediscover. The matrix is the one structural change, and it makes the profile table
executable: five rows, ten phases, each with an artifact and an oracle. Ordering is unchanged and
still implementable: M1 (topology) -> M2 (portable backend) -> M3 (bypass transition) -> M4
(`DmaAddress` consumers) -> M5/M6 (evidence) -> M7 (enforcing becomes ordinary). The dependency on
P02M0170 is unchanged and now carries more weight, since the dossier is what enforces phase
completeness. No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN M0173 (2026-08-30T22:25:50Z):

Rating: 10/10

No unresolved material findings. The updated plan is internally consistent, its evidence phases have explicit artifacts and oracles, and its composite-row contract is representable by M0170's catalog prerequisites and per-key evidence envelopes. The planner's corrections are complete and independently supported by the current harness, kernel paths, and qualified QEMU properties.

PLANNER'S RESPONSE ON M0173 (2026-08-30T23:21:20Z):

No findings. The re-audit rates the plan 10/10 and records no unresolved material issue, so there is
nothing to accept or reject and no plan change was made.

Re-checked anyway against the changes this round made elsewhere, because two of this plan's
prerequisites moved:

- P02M0170's catalog model gained producer rows for shipping images, a per-phase evidence envelope
  that persists log BYTES rather than paths, and a stable-snapshot requirement for medium
  construction. This plan's five composite rows and ten phase subkeys are expressible in all three -
  a phase is a key with an artifact digest and an evidence envelope, which is what M0170 now defines.
- P02M0172's DMA-mode matrix now names the gate rows explicitly ("each named gate states its value
  and its topology together, produced by one harness path"), which is the contract M1 and M7 of this
  plan rely on. Its identity field is a fixed 64-byte name, which changes nothing here.

No edit to `docs/todo/P02M0173.md` was needed or made. No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN M0173 (2026-08-31T00:17:04Z):

Rating: 10/10

No unresolved material findings.
