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


PLANNER'S RESPONSE ON M0172 (2026-08-30T19:36:07Z):

All four findings are accepted. Two of them were ambiguities rather than errors, and the plan now
DECIDES them instead of restating the options - which is what an implementation plan is for.

**1. The proposed DMA boot-profile scalar conflicts with the development-profile channel -
ACCEPTED.**

Confirmed on every architecture. `arch/x86_64/mod.rs:164-179` (and the AArch64 and RISC-V twins)
reads `opt/org.libersystem/profile` from fw_cfg and recognizes exactly `development` and
`development-trace`, with an unrecognized name reading as no profile - and the function's own
comment says the value is ADVISORY. Those exact strings gate security-sensitive userspace behavior in
`dev_protocol.rs`, the harness injects the file only on that branch, and test mode deliberately
carries no profile metadata at all. Putting `enforcing-required`/`no-iommu` into that scalar
either breaks development semantics or makes every current test, direct and non-x86 boot fail closed
because nobody taught it to answer.

Plan change: a new "Two boundaries this plan fixes before implementation" section states the decision
- the DMA mode is a SEPARATE typed field and the advisory scalar stays advisory - and M2 was rewritten
to own it. M2 now requires the value/producer/missing-value behaviour to be written as a TABLE over
architecture x boot path x run mode rather than as prose, and requires every producer and consumer to
be migrated BEFORE admission is made fail-closed. M8 adds an assertion that no producer named in that
table is missing a value.

**2. The authenticated loader-to-kernel handoff is absent and the dependency graph permits
incompatible format changes - ACCEPTED.**

Confirmed. `BootInfo` is at `VERSION = 2` and its documented v2 addition is `root` - the
loader's chosen system volume. There is no authenticated policy field, and P02M0150 deliberately did
not extend `BootInfo` because its trust decision stayed inside the loader. This decision is
kernel-owned, so the handoff has to be built rather than assumed. Confirmed also that the old
dependency list named P02M0153, P02M0161-P02M0166 and P02M0099 and named neither P02M0150 nor
P02M0171, while P02M0171 independently adds a signed generation to the same manifest.

Plan change: a new **M3** owns the handoff and names its three parts - the signed manifest field and
exactly what the loader validates, a versioned `BootInfo` extension carrying the value AND its
PROVENANCE (so the kernel can distinguish a loader-authenticated value from a harness-trusted one),
and the trusted direct-boot equivalent for profiles with no loader. It states that a
boot-media-controlled value may never present itself as authenticated policy. Dependencies now name
P02M0150 and P02M0171, with the composition rule written out: if both are in flight the manifest
evolves ONCE and one compatibility matrix covers both fields, and neither may land a second
incompatible format change. M8 adds a mutation that must fail when a DMA mode whose provenance says
it came from replaceable media is accepted.

**3. A new "stable ID" is not reconciled with the identity already persisted and reported -
ACCEPTED, and decided.**

Confirmed. Generated entries are identified by `name` (`device_manager.rs:4526`), operator policy
persists `select=<name>` (:3703-3706), and P02M0166 already calls that the registry entry ID. The
claim boundary carries device index and privilege, and `ClaimKey` carries device index, padding and
generation - no entry identity anywhere. So the old M2's "give each generated registry entry a stable
ID" would have created a second name for one thing.

DECIDED rather than left open: the canonical identity is the validated program NAME, which already
exists, is already unique, is already persisted and is already reported. Inventing a new manifest ID
field would orphan every stored operator preference and give reporting, userspace selection and
kernel admission three ways to name one entry, for no gain. What IS genuinely missing is the part the
audit asks for next: the encoding, normalization, width and ABI representation that name needs to
cross the claim boundary. Plan change: the new section states the decision, and a new **M4** requires
those to be defined and states explicitly that the identity is added as a declared field and NOT
written into `ClaimKey`'s reserved padding. "What this milestone refuses" now names both a second
entry identity and the padding shortcut.

**4. Kernel validation of the attempted candidate is ambiguous relative to operator selection and
fallback - ACCEPTED, and decided.**

Confirmed as a genuine fork with incompatible implementations, not a wording problem. DeviceManager
can persistently select any matching named candidate including a lower-priority one
(`device_manager.rs:3694-3706`), and the old M2 said only that the kernel "validates that
privileged handoff against a table generated from the same manifest and the concrete device", which
does not say whether it recomputes priority.

DECIDED: privileged DeviceManager's selected entry is AUTHORITATIVE, and the kernel validates
membership plus device plus mode plus policy plus generation. The reason is written into the plan
rather than left implicit - operator policy can deliberately select a lower-priority candidate, and a
kernel that recomputed priority would silently overrule the operator, which defeats the purpose of
the `select` verb. Plan change: a new **M5** freezes the boundary, lists the four things the kernel
checks, and states the trust model as a sentence. M8 requires the case the boundary exists for - an
operator has persistently selected a lower-priority candidate, and the kernel admits it because the
entry is declared for that device, and refuses when it is not - plus a mutation that must fail when
the kernel recomputes priority.

**Plan re-check.** Eight items where there were five, because the handoff, the identity and the
authority boundary were each doing work inside one overloaded M2. Ordering is implementable and
matches the data flow: M1 (manifest field) -> M2 (DMA mode field and producer table) -> M3
(authenticated handoff) -> M4 (identity across the claim boundary) -> M5 (authority boundary) -> M6
(remove the permissive default) -> M7 (rebuild selection) -> M8 (fixtures). Fail-closed admission is
explicitly the LAST behavioural change, after every producer has been migrated, which is the ordering
that keeps this from breaking the current test and non-x86 paths on the way in. The Definition of
done now states the producer coverage and the non-overruling of operator selection as separate
falsifiable clauses. No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN M0172 (2026-08-30T22:25:50Z):

Rating: 6/10

The authority-boundary decision and removal of the permissive fallback are sound, but three integration contracts are still incomplete.

1. **The promised producer matrix and authenticated handoff are not actually specified.** M2 tells the implementer to create a table for every architecture, boot path, and run mode, but the plan contains no such table or values (`docs/todo/P02M0172.md:48-59`). M3 similarly lists a signed field, a `BootInfo` value/provenance, and a direct-boot equivalent without fixing their encodings or defining how each trusted entry path establishes provenance rather than copying an untrusted assertion (`:61-72`). Add the actual matrix, missing-value semantics, versioned handoff layout, and per-path validation/producer rules before fail-closed admission is implementable.

2. **The signed DMA mode has no boot-wide source/equality rule.** A boot can validate multiple medium and volume manifests; M0171 explicitly latches equality for its generation field. M0172 adds DMA mode to authenticated metadata but never chooses one authoritative manifest or requires every selected signed manifest to carry the same value (`docs/todo/P02M0172.md:40-72`). Two individually valid same-release manifests can therefore disagree about enforcement. Define the authoritative source or an equality latch and add a mixed-mode signed-set refusal fixture.

3. **M4 incorrectly treats name normalization as undefined and can diverge from the existing manifest identity.** The system-manifest parser already constrains program names to 1-64 ASCII alphanumeric, underscore, or hyphen bytes and rejects duplicates (`src/tools/system-manifest/src/lib.rs:901-902`, `:1207-1213`). M4 says to define normalization, encoding, and maximum width later (`docs/todo/P02M0172.md:74-84`) while the request/claim/grant/report ABI still carries no entry name. Adopt the existing grammar verbatim, choose the bounded ABI representation and migration now, and test that persisted selection, generated tables, kernel claims, and reporting use identical bytes.
