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

PLANNER'S RESPONSE ON M0172 (2026-08-30T23:21:20Z):

All three findings are accepted, and the third is a case where the tree already had the answer I said
should be defined later.

**1. The promised producer matrix and authenticated handoff are not specified - ACCEPTED.**

Correct on both halves: M2 told the implementer to create a table and contained none, and M3 listed
a signed field, a `BootInfo` value and a direct-boot equivalent without fixing their encodings or
saying how each trusted path establishes provenance rather than copying an assertion.

Plan changes: M2 now CONTAINS the matrix - ten rows over x86_64/AArch64/RISC-V, UEFI and direct boot,
public/gate/test/development mode - naming the trusted producer for each. MISSING is given a stated
meaning rather than left as an error: it selects the loud DEGRADED contract, which is what keeps test
and development boots working through the migration; and a missing value on a row that NAMES a
producer refuses, because there the absence means the producer is broken. M3 now fixes the signed
field as a `u32` with two accepted values beside P02M0171's generation, a versioned `BootInfo`
extension carrying the value AND a provenance enum (`signed` or `harness`), and per-path validation
rules - a loader writes `signed` only for a value it verified against a signature it checked. `BootInfo`
is at `VERSION = 2` and this is an appended field plus a bump, which is what that guard exists for.
M8 gains a mutation for a loader writing `signed` provenance for an unverified value.

**2. The signed DMA mode has no boot-wide source or equality rule - ACCEPTED.**

Verified against P02M0171, which latches equality for its generation across every manifest a boot
composes - and M0172 added a field to the same manifests with no such rule, so two individually valid
same-release manifests could disagree about whether enforcement is required.

Plan changes: M3 gains a BOOT-WIDE EQUALITY LATCH on P02M0171's terms - every selected signed
manifest carries the same mode, the first verified manifest latches it, a later one that differs
refuses the boot. The plan states explicitly that there is NO authoritative manifest and NO precedence
order, with the reason: a precedence order is a rule an attacker composes media against. M8 gains the
mixed-mode signed-set refusal fixture, and the mutation list gains removing the latch.

**3. M4 treats name normalization as undefined and can diverge from the existing identity -
ACCEPTED.**

The auditor is right and this is the useful kind of correction: `system-manifest`'s `validate_name`
already constrains a program name to 1-64 bytes of ASCII alphanumerics plus `_` and `-`, and the
parser rejects duplicates. Defining a second normalization would create two answers for one identity,
which is the defect M4 exists to prevent - so my "define it later" would have manufactured the problem
it was meant to solve.

Plan changes: the grammar is ADOPTED VERBATIM and the plan says there is NO normalization step at all
- a name is canonical or it is rejected at manifest validation - with the maximum width taken from
the manifest's own bound. The ABI representation is chosen here rather than deferred: a FIXED 64-BYTE
NUL-PADDED FIELD in the claim request, stamped on the grant and the report, fixed rather than
length-prefixed because the claim boundary is a `repr(C)` struct and a variable-length field there is
a second parser, and 64 because that is the manifest's bound so the two cannot drift. It stays a
DECLARED field; `ClaimKey`'s reserved padding is not repurposed. M8 gains an end-to-end identity test
proving the persisted selection, the generated table, the claim request, the kernel's stamp and the
report all carry the same 64 bytes.

**Plan re-check.** Eight items, unchanged in count and order, with the fail-closed change still last.
Everything M2, M3 and M4 previously deferred to implementation is now in the file, and the Definition
of done names the matrix, the provenance and the equality latch as separate clauses. No source code
was modified.

AUDITOR'S RE-AUDIT OF PLAN M0172 (2026-08-31T00:17:04Z):

Rating: 5/10

1. **The producer matrix and missing-value rule contradict one another and contain overlapping rows
   with no precedence.** The matrix assigns `no-iommu` to any `--no-iommu` run, but also assigns
   `ABSENT` to any test-mode or development instance (`docs/todo/P02M0172.md:58-75`), so a development
   or test `--no-iommu` run has two answers. The following text says a missing value deliberately
   selects the loud degraded contract for those migration paths (`:77-82`), while M6 says every
   missing mode is refused (`:161-168`). It also leaves an authenticated-loader development path
   ambiguous: “development instance = ABSENT” conflicts with the rule that a producer-backed loader
   path missing its signed field refuses. Make the rows disjoint or define explicit precedence, and
   state one missing-mode behavior per path before fail-closed admission.

2. **The trusted direct-boot handoff has no carrier and occurs too late in the current boot order.**
   M3 says the host harness writes a `BootInfo` field with `harness` provenance
   (`docs/todo/P02M0172.md:107-117`). Direct AArch64 and RISC-V entry receives a raw DTB, however, and
   the kernel constructs and publishes its own BootInfo later
   (`src/kernel/arch/aarch64/boot.rs:947-968`, `:988-1028`;
   `src/kernel/arch/riscv64/boot.rs:608-629`, `:677-705`). On both production paths, `device::init` and
   `dma_policy::init` run before that construction (`src/kernel/arch/aarch64/boot.rs:1044-1052`;
   `src/kernel/arch/riscv64/boot.rs:719-726`). Thus the harness cannot write the promised field into
   an object that does not yet exist, and admission consumes no authenticated mode before deciding.
   Define the actual trusted early carrier (for example a bounded fw_cfg or FDT property), its
   validation/provenance rule, and parsing before DMA admission; merely naming the later BootInfo field
   is not an implementable direct-boot contract.

PLANNER'S RESPONSE ON M0172 (2026-08-31T01:12:30Z):

Both findings ACCEPTED and applied. The second is the more serious: the handoff I specified could not
have been implemented at all.

**1. The producer matrix has overlapping rows and two contradictory missing-value rules - ACCEPTED.**

Verified: a development `--no-iommu` run matched both "any target, `--no-iommu`" and "any target,
development instance" and had two answers, and the text that followed said a missing value selects
the loud degraded contract while M6 said every missing mode is refused.

Plan changes: the matrix is rebuilt with RUN MODE as the outer key, decided first, because a test or
development boot is a different kind of thing from a public one and the flag only chooses between the
two enforcing values WITHIN a public or gate run. The rows are now disjoint by construction - test,
development, public-UEFI (four rows), public-direct (two rows), gate - and a development run with
`--no-iommu` matches exactly one.

The missing-value rule is stated ONCE and derived from the row: where the producer is "nothing" (test
and development) ABSENT is the CORRECT value and selects the loud degraded contract, permanently
rather than as a migration allowance; where the producer is NAMED, ABSENT is a BROKEN PRODUCER and
refuses, because a machine whose opinion was lost is not a machine without one. M6's sentence was
rewritten to say the same thing instead of refusing every absence.

**2. The trusted direct-boot handoff has no carrier and occurs too late - ACCEPTED.**

The auditor is right and this was unimplementable as written. On a direct AArch64 or RISC-V boot the
kernel receives a raw DTB and CONSTRUCTS its own `BootInfo` later - and `device::init` and
`dma_policy::init` both run BEFORE that construction. So "the harness writes a `BootInfo` field with
`harness` provenance" asked a host to write into an object that does not exist yet, and admission
would have decided before any authenticated mode was parsed.

Plan changes: M3's last bullet becomes **THE TRUSTED DIRECT-BOOT CARRIER, which is NOT `BootInfo` and
could not have been**, naming what the kernel actually receives early: on x86_64 direct, an `fw_cfg`
file of this product's own - beside the profile scalar and separate from it - carrying the mode and
the `harness` provenance in a fixed-length record; on AArch64/RISC-V direct, a property under this
product's own node in the device tree the harness passes, read by the existing FDT parser in the same
pass that reads the memory map and the interrupt controller. Both are parsed BEFORE
`dma_policy::init`, and the plan states that ordering as a REQUIREMENT rather than an implementation
note, because a mode parsed after admission is a mode admission did not use. `BootInfo` still carries
the value onward for reporting and userspace; it is not where admission gets it. The provenance rule
is unchanged - the value reads `harness`, never `signed` - and a malformed or absent record makes the
mode ABSENT, which on a direct row refuses because that row names a producer.

**Plan re-check.** Item count unchanged at eight. The matrix, the missing-value rule and M6 now agree,
and every path in the matrix names a carrier that exists at the point admission reads it. No source
code was modified.
