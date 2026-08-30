AUDITOR'S REVIEW OF PLAN M0172 (2026-08-30T16:21:53Z):

Rating: 5/10

The required closed DMA-policy field, removal of the permissive device-type fallback, and enforcement intent are sound. The plan is not yet safely integrable because it conflates DMA mode with an existing development profile, omits the authenticated loader-to-kernel handoff and dependency ordering, leaves registry identity/wire migration undefined, and does not decide who owns candidate selection policy.

## Material findings

1. **The proposed DMA boot-profile scalar conflicts with the existing development-profile channel.**

   **What is wrong:** M2 introduces `enforcing-required|no-iommu` as one boot-profile value (`docs/todo/P02M0172.md:36-40`). The kernel currently recognizes only `development` and `development-trace` on each architecture (`src/kernel/arch/x86_64/mod.rs:160-178`; `src/kernel/arch/aarch64/mod.rs:141-160`; `src/kernel/arch/riscv64/mod.rs:160-175`), and exact `development` values control security-sensitive userspace behavior (`src/user/services/core/src/dev_protocol.rs:82-91`, `:386-390`). The harness accepts only those values, injects fw_cfg only on that branch, and deliberately runs test mode without IOMMU/profile metadata (`src/harness/qemu-run.sh:804-820`, `:991-1010`, `:1214-1220`).

   **Why it matters:** Reusing or replacing this scalar breaks development/hot-artifact semantics. Requiring the new values without migrating every producer makes current test, direct, and non-x86 paths fail closed unintentionally.

   **Correction:** Make DMA mode an orthogonal typed boot-contract field, or define a closed cross-product that explicitly preserves development and trace semantics. Enumerate x86_64/AArch64/RISC-V × UEFI/direct × test/development/public/gate values, their trusted producers, and missing-value behavior; migrate every producer and consumer before enabling fail-closed admission.

2. **The authenticated loader-to-kernel handoff is absent, and the dependency graph permits incompatible format changes.**

   **What is wrong:** M2/M4 say authenticated boot metadata binds the profile and the kernel consumes it, but `BootInfo` v2 has no authenticated profile field (`src/boot/protocol/src/lib.rs:34-46`, `:155-217`). P02M0150 explicitly avoided extending BootInfo because its then-current trust decision stayed in the loader (`docs/todo/P02M0150.md:119-121`); this new decision is kernel-owned. M0171 independently adds a signed manifest generation/version, yet M0172 lists neither M0150 nor M0171 as a dependency (`docs/todo/P02M0172.md:51-71`).

   **Why it matters:** A boot-media-controlled value can masquerade as authenticated policy, and independently implemented manifest/BootInfo changes can be incompatible or cause consecutive format churn.

   **Correction:** Define the signed field and exact loader validation, a versioned `BootInfo` handoff with provenance, and the trusted direct-boot equivalent. Add P02M0150/P02M0171 ordering or an explicit composition rule; if both land together, prefer one coordinated manifest evolution and compatibility test matrix.

3. **A new “stable ID” is not reconciled with the identity already persisted and reported.**

   **What is wrong:** Generated entries currently use program `name` as identity (`src/user/services/core/src/device_manager.rs:4526-4553`), operator policy persists `select=<name>` (`:3635-3763`), and P02M0166 explicitly calls that the registry entry ID (`docs/todo/P02M0166.md:169-172`). M2 introduces another stable ID without defining whether it replaces the name, its format/uniqueness/width, or stored-selection migration (`docs/todo/P02M0172.md:30-35`). The claim boundary currently carries only device index and privilege through `device_claim`; `ClaimKey` contains device index, padding, and generation, with no entry identity (`src/user/runtime/rt/src/lib.rs:2425-2441`; `src/kernel/syscall/mod.rs:1120-1178`; `src/abi/src/lib.rs:845-878`).

   **Why it matters:** Operator preferences can be orphaned, or reporting, userspace selection, and kernel admission can refer to different entries. Without a defined ABI representation, “kernel validates the exact attempted entry” is not implementable.

   **Correction:** State whether the canonical ID is the validated program name or a new manifest field. Define uniqueness, normalization/encoding, width, persistence migration, and display semantics. Then specify the request/claim/grant/report layouts and tests across generated tables, DeviceManager, runtime, ABI/syscall, and kernel; do not repurpose padding implicitly.

4. **Kernel validation of the attempted candidate is ambiguous relative to operator selection and fallback.**

   **What is wrong:** DeviceManager can persistently select any matching named candidate, including a lower-priority one (`src/user/services/core/src/device_manager.rs:3694-3706`, `:3750-3762`). M5 requires different-priority policy/fallback coverage, but M2 never states whether the kernel validates mere membership and device/profile/policy, recomputes priority, or verifies an authorized selection/fallback state (`docs/todo/P02M0172.md:30-35`, `:57-60`).

   **Why it matters:** Kernel priority recomputation breaks intentional operator selection; membership-only validation makes privileged DeviceManager the policy authority. Either can be valid, but they are different trust models and produce incompatible implementations and tests.

   **Correction:** Freeze the authority boundary. Preferably state that privileged DeviceManager's selected matching entry is authoritative and the kernel validates generated membership plus device, profile, policy, and generation; otherwise carry a kernel-verifiable selection authorization. Include an operator-selected lower-priority candidate and real fallback attempt in the positive/negative matrix.

