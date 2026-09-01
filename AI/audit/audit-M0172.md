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

AUDITOR'S RE-AUDIT OF PLAN M0172 (2026-08-31T03:28:50Z):

Rating: 5/10

1. **The revised producer matrix is still not disjoint in the current harness, because “test” and
   “gate” are overlapping run modes with no discriminator.** M2 assigns every test boot `ABSENT` and
   every named gate its declared mode (`docs/todo/P02M0172.md:55-93`). The existing IOMMU gate invokes
   `test.sh` (`src/tools/check-qemu-virtio-iommu-x86_64.sh:116-119`), whose runner sets `TEST=1`
   (`src/harness/test-kernel.sh:387-393`); M0173's gate rows likewise deliberately contain test-kernel
   phases. The plan says run mode is decided first but defines neither the exclusive mode value nor
   which signal makes a gate-owned test boot a `gate` instead of `test`. Implemented over the current
   signals, one boot has both answers. Define one authoritative mutually exclusive run-mode carrier
   and test every gate/test boundary before claiming the rows are disjoint.

2. **The non-x86 enforcing rollout cannot land in the declared dependency order.** M2 assigns
   `enforcing-required` to public AArch64/RISC-V defaults and says named-producer absence refuses
   (`docs/todo/P02M0172.md:75-108`), and M6 refuses required bindings without working enforcement
   (`:214-224`). P02M0173 exclusively owns adding those architectures' virtio-IOMMU topology and is
   explicitly planned *after* M0172 (`docs/todo/P02M0173.md:1-15,60-73,168-176`); the current non-x86
   harness has endpoints but no such controller. Completing M0172 first therefore either breaks the
   public non-x86 boots or requires M0172 to implement M0173's work. Define an explicit intermediate
   mode/landing transition, or coordinate the fail-closed switch with M0173 instead of imposing a
   dependency cycle.

3. **The accepted direct-carrier correction still does not define a wire contract.** M3 now names “an
   `fw_cfg` file of this product's own” and “a property under this product's own node,” but supplies no
   fw_cfg filename, FDT node/property names, record length/layout, numeric provenance encoding, or FDT
   cell/byte encoding (`docs/todo/P02M0172.md:145-169`). No such DMA carrier currently exists; merely
   saying “fixed-length” does not let the harness producer and three kernel consumers agree on bytes.
   Freeze the names and exact record encodings, including malformed/unknown handling, before claiming
   every matrix path has an existing carrier at admission time.

PLANNER'S RESPONSE ON M0172 (2026-08-31T19:35:25Z):

**1. The revised producer matrix is not disjoint, because "test" and "gate" overlap with no
discriminator - ACCEPTED.**

Correct, and the existing enforcing gate is the proof rather than a hypothetical.
`check-qemu-virtio-iommu-x86_64.sh` invokes `test.sh`, whose runner sets `TEST=1`, and it supplies
its own enforcing controller through `QEMU_EXTRA`. That boot is a `gate` row by ownership and a
`test` row by `TEST=1`, and the two rows answer oppositely - "as that gate declares" against
`ABSENT`. The harness makes the collision visible from the other side too: `qemu_run_x86_64` reads
`TEST=1` as "untranslated" and attaches no controller, while the gate has attached one. P02M0173's
gate rows are deliberately test-kernel phases as well, so this is the normal case. "Run mode is
decided first" named no carrier and no value, so the rule was unimplementable.

Plan change: M2 gains an authoritative carrier. `LIBER_RUN_MODE` holds exactly one of `test`,
`development`, `public` or `gate` and is the only thing consulted for the outer key; `TEST`, the
development scalar, `--no-iommu` and `BOOT_IMAGE` keep their meanings and none is read as a run mode
- `TEST=1` selects the test KERNEL and MEDIUM, which is a different question from what policy the
boot runs under. Who sets it is stated: a named gate sets `gate` before invoking anything and the
invoked runner does not overwrite it, `test.sh` sets `test` ONLY WHEN UNSET (which is what makes the
gate's value survive its own use of the test runner), `run.sh` sets `public`, the development
instance sets `development`. Unset is a broken producer and refuses. And the boundary is tested
rather than asserted: each named gate proves it runs under `gate`, and the enforcing IOMMU gate
specifically proves its test-kernel phase does not fall to the `test` row.

**2. The non-x86 enforcing rollout cannot land in the declared dependency order - ACCEPTED.**

Correct. `virtio-iommu-pci` is attached only inside `qemu_run_x86_64`, so the non-x86 harness has
endpoints and no controller; M6 makes an `enforcing-required` mode refuse when enforcement is absent
or failed; and P02M0173, which exclusively owns that topology, is planned after this milestone and
lists it among its own prerequisites. Landing M0172 first would refuse every public non-x86 boot. The
"once P02M0173's profiles exist" clause was in the producer column while the mode column said
`enforcing-required` unconditionally - a dependency cycle written as a footnote. The finding is also
right that the public DIRECT rows carry it, not only the UEFI ones, so the break is not hypothetical
for a harness that boots those architectures directly today.

Plan changes: the matrix splits the direct row by architecture and both non-x86 public rows point at
a stated LANDING TRANSITION instead of carrying a value. While this milestone lands they are
`no-iommu` PRODUCED by the same named producer as every other public row - a trusted, produced, named
mode and not an absence, so `iommu-required` still refuses, only `trusted-untranslated` enters the
loud degraded inventory, and a missing value still refuses. That is deliberate: it keeps the
fail-closed change landing everywhere at once with only the VALUE differing, rather than parking those
rows on the test/development absence rule. When P02M0173 lands its topology it flips both rows, which
is an item of P02M0173 and needs no admission change because M6 already refuses an enforcing mode
without working enforcement. Dependencies records it as a two-sided obligation - this milestone must
not ship `enforcing-required` there, and P02M0173 is not done until it has flipped them - because
either order taken alone breaks the boots.

**3. The accepted direct-carrier correction does not define a wire contract - ACCEPTED.**

Correct. The correction named a shape and no names, lengths or encodings, and no such carrier exists
in the tree to copy them from, so one harness producer and three kernel consumers had nothing to agree
on. "Fixed-length" is not a length. This file's sibling freezes a 64-byte record to the offset, so the
standard is established.

Plan change: ONE FORMAT carried two ways, frozen to the byte. An 8-byte record: magic `LSDM` at 0,
format version 1 at 4, DMA mode at 5 (`1` = `enforcing-required`, `2` = `no-iommu`), provenance at
6 (`2` = `harness`), reserved zero at 7. Deliberate decisions written in rather than left implicit:
there is no encoding for absence, because absence is the record not being there and M2 already answers
that; `1` (`signed`) is MALFORMED on this carrier and is refused rather than trusted, because a
direct-boot carrier is exactly where a media-controlled value must not be able to claim
authentication; and the provenance is one byte so the enum has a single encoding across both carriers
and the signed manifest field. The x86_64 carrier is the `fw_cfg` file `opt/org.libersystem.dma-mode`
containing exactly those bytes; the device-tree carrier is the property `libersystem,dma-mode` under
this product's node, as a BYTE STRING and not cells, so no cell endianness applies and the two
carriers are byte-identical. Malformed and absent yield the same outcome - mode ABSENT, and the
named-producer rule refuses - distinguished only in what is reported, and nothing about a malformed
record is ever interpreted, which is what stops a corrupt record from being a downgrade. The
Definition of done gains the carrier, the run mode and the non-x86 landing rule.

AUDITOR'S RE-AUDIT OF PLAN M0172 (2026-08-31T19:58:23Z):

Rating: 6/10

1. **The run-mode correction never carries run mode to the component that must distinguish allowed
   from fatal absence.** `LIBER_RUN_MODE` is a host-shell discriminator
   (`docs/todo/P02M0172.md:63-89`), while kernel admission must treat the identical absent DMA-mode
   state as expected for test/development but fatal for public/gate
   (`:91-102,156-172,313-323`). The planned `BootInfo` extension carries only DMA mode and
   provenance (`:197-205`), and the direct carrier contains magic, version, DMA mode, provenance
   and reserved bytes—no run mode (`:221-253`). Current `BootInfo` likewise has no such field
   (`src/boot/protocol/src/lib.rs:158-218`). Consequently an early kernel cannot distinguish an
   intentionally carrier-less test/development boot from a public/gate boot whose named producer lost
   its carrier, which defeats the fail-closed half of the matrix. Carry a trusted run-mode value into
   early kernel state, or give test/development an explicit DMA-mode record too so absence has one
   universally fatal meaning.

PLANNER'S RESPONSE ON M0172 (2026-08-31T21:11:04Z):

**1. The run-mode correction never carries run mode to the component that must distinguish allowed
from fatal absence - ACCEPTED.**

Correct, and it is the second half of a fix whose first half I did complete. Last round I gave run
mode one authoritative carrier because the gate/test rows overlapped in the harness - which was the
right fix for the question asked - and left the matrix's ABSENCE rule reading two ways: expected on
test and development, fatal on every named-producer row. The kernel is what has to tell those apart
at admission time and it cannot. `LIBER_RUN_MODE` is a host-shell variable, the planned `BootInfo`
field carries only the DMA mode and its provenance, the direct carrier's record carries magic,
version, mode, provenance and a reserved byte, and current `BootInfo` has no such field at all. So
an early kernel cannot distinguish a test boot that legitimately carries no record from a public boot
whose producer lost one, and the fail-closed half of the matrix was unimplementable as written.

Of the finding's two remedies I took the second, because it removes the problem instead of moving it.
Carrying a run mode into early kernel state would give admission a SECOND trusted fact to validate,
with its own provenance question - and the kernel does not need to know what kind of boot this is, it
needs the mode. So EVERY boot carries a record now, test and development included, stating the
degraded value explicitly with `harness` provenance, produced by the same paths that already build
those boots. Absence becomes universally fatal, and `LIBER_RUN_MODE` goes back to deciding only
which VALUE the producer writes - a host-side decision, where it already lives.

That is smaller as well as more correct, and it fixes something the previous version could not test.
"Absence means different things on different rows" needs a fixture per row and a way to tell the rows
apart at admission time; "absence is always fatal" is one fixture, and every boot that works proves
its producer ran.

Plan changes: the matrix's test and development rows carry `no-iommu (degraded)` with a named
producer instead of `ABSENT` with "nothing produces one". The missing-value rule is rewritten - the
degraded contract is unchanged and is now reached by a VALUE rather than by a silence, and absence is
a broken producer on every row with no exception to remember. M6's carve-out for the test and
development rows is struck; that sentence has now agreed with M2 in opposite directions twice and is
settled. The direct carrier's malformed-and-absent paragraph drops "because those rows name a
producer" for "because every row now names a producer". The Definition of done gains two lines: run
mode never reaches the kernel, and every boot carries a record.

AUDITOR'S RE-AUDIT OF PLAN M0172 (2026-09-01T02:10:36Z):

Rating: 6/10

1. **Test/development UEFI boots have no coherent DMA-mode handoff source.** M2 now requires every
   test and development boot to carry `harness` provenance (`docs/todo/P02M0172.md:91-106,160-208`),
   but M3 defines `harness` only for trusted *direct-boot* fw_cfg/FDT carriers and `signed` provenance
   for the loader's BootInfo path (`:226-259,299-308`). Current x86_64 test/development boots traverse
   OVMF, and non-x86 UEFI paths are also in scope. The plan neither says how those rows produce the
   record nor defines source precedence/equality when signed BootInfo and a harness carrier disagree.
   Split the UEFI rows and build/sign degraded manifests for them, or freeze authoritative source
   selection and conflict refusal across signed BootInfo and the harness carrier.

2. **The accepted FDT wire contract still lacks its node identity.** M3 freezes the property
   `libersystem,dma-mode` only “under this product's own node,” but supplies no node name/path or
   `compatible`, and no existing tree supplies one to inherit (`docs/todo/P02M0172.md:246-290`). The
   harness producer and early kernel consumers can therefore choose different nodes while following
   the stated bytes. Freeze the node identity and add a producer/consumer fixture using it.

PLANNER'S RESPONSE ON M0172 (2026-09-01T03:14:09Z):

**1. Test/development UEFI boots have no coherent DMA-mode handoff source - ACCEPTED.**

Correct, and it is the hole last round's own fix opened. Removing the "absence is expected here"
carve-out was right - it is what let absence mean one thing everywhere - and it left the test and
development rows required to carry a produced record with no carrier on their path. Both carriers M3
defines are DIRECT-boot ones: an `fw_cfg` file and a device-tree property. x86_64 test and
development boots go through OVMF, and the non-x86 UEFI rows are in scope too. And the plan never
said what happens when a signed `BootInfo` field and a harness carrier both appear.

Plan changes, two decisions made here rather than left open. The UEFI carrier for test and
development is the SAME `fw_cfg` file: it is a machine mechanism rather than a direct-boot one,
available to a UEFI guest exactly as to a directly booted one, and the harness already writes the
development profile scalar through it - so one record serves both paths and `harness` provenance
means the same thing on each. Building and signing degraded manifests for every test boot was the
alternative and is refused: it puts a signing step in front of the suite for a value the harness
already controls. And precedence: the SIGNED source wins, and a boot presenting BOTH refuses -
including when they agree, because two producers that agree by luck are still two producers and the
matrix gives each row one.

**2. The accepted FDT wire contract still lacks its node identity - ACCEPTED.**

Correct. I froze the property name, the byte layout, the value encodings and the malformed handling,
and wrote "under this product's own node" - and no tree in this system defines such a node, so there
was nothing to inherit. The harness producer and the three kernel consumers could each choose a
different path while following every byte I had frozen, which is the exact failure freezing a record
is supposed to prevent. Freezing the contents of a thing and not its name leaves it unaddressable.

Plan changes: the node is `/libersystem`, an immediate child of the root, with
`compatible = "libersystem,boot-policy"`, and the CONSUMER MATCHES ON `compatible` rather than on the
path - so a machine that places the node elsewhere is still read correctly and a node with the right
name and the wrong `compatible` is not this record. M8 gains a producer/consumer pair fixture, run
for both carriers since they share a format on purpose, with the negatives that distinguish a frozen
identity from a convention: wrong `compatible`, right `compatible` under a different node name
(which must still be found), wrong length, and a `signed` provenance byte on a carrier where only
`harness` is legal. A frozen record only one side has ever written is a record with one
implementation.

AUDITOR'S RE-AUDIT OF PLAN M0172 (2026-09-01T03:39:33Z):

Rating: 7/10

1. **The accepted UEFI-carrier correction is still unusable on the in-scope AArch64 UEFI path.** M3
   now assigns test/development UEFI boots the fw_cfg record and asserts it is available just as on
   direct boot (docs/todo/P02M0172.md:320-338). The current AAVMF path, however, hands the kernel no
   device tree (src/kernel/arch/aarch64/boot.rs:425-449); that branch leaves fwcfg_base at zero
   (:523-554), and the only non-x86 reader immediately returns None for base zero
   (src/kernel/arch/common/fwcfg/mod.rs:138-153). Attaching the machine device therefore does not
   create the promised early carrier, and universal missing-mode refusal would break AArch64
   test/development UEFI boots. Define how this no-DT path locates and reads the record or receives a
   loader-validated harness value, and add an AArch64 UEFI producer/consumer admission fixture rather
   than only a generic UEFI case.

PLANNER'S RESPONSE ON M0172 (2026-09-01T12:12:00Z):

**Finding 1 - the UEFI-carrier correction is unusable on the AArch64 UEFI path. REJECTED on its
central claim, which is false of the current tree; one half of its remedy adopted anyway.**

The finding rests on "the current AAVMF path hands the kernel no device tree", cited to
`src/kernel/arch/aarch64/boot.rs`. That sentence is in the tree - it is the COMMENT above the no-DT
fallback branch, explaining why that branch exists - and it does not describe what the AArch64 UEFI
boot actually does. Measured on this machine rather than argued:

    aarch64: GICv2 from the device tree - distributor 0x8000000+0x10000, cpu interface 0x8010000+0x10000
    aarch64: DTB parsed - 1 bank(s), 8 CPU(s)

That is this tree's ordinary aarch64 suite, booted through AAVMF, in a run of 361 tests that passed.
The firmware hands the tree on and the kernel parses it. Two further checks follow from the same
parse: `fdt` looks for an `fw-cfg` node and extracts its base into `fwcfg_base` - the same structure
the GIC came out of - and the kernel's own `boot_profile()` reads `opt/org.libersystem/profile`
through exactly that base, which is the mechanism the plan already relies on when it says "the
harness already writes the development profile scalar through it".

So `fwcfg_base` is not zero on this path, the promised early carrier does exist, and universal
missing-mode refusal would not break AArch64 test or development UEFI boots. The plan's own reasoning
- that `fw_cfg` is a MACHINE mechanism available to a UEFI guest exactly as to a directly booted one
- is the thing I verified, and it stands.

The no-DT branch the finding read is reachable only when a machine publishes no tree AND the build
authorises it through `LIBER_NO_DT_PROFILE`, which no caller in this tree sets. It is guarded dead
code, not the AArch64 UEFI path. I record that here because the same stale comment is what a future
reader will find first.

ONE HALF OF THE REMEDY IS ADOPTED, because it is right for a different reason than the one given. The
finding asks for an AArch64 UEFI producer/consumer admission fixture rather than only a generic UEFI
case, and the plan's fixture said "a UEFI test boot and a direct one carrying the same record reach
the same admission decision" without naming an architecture. The cross-path availability of the
carrier is precisely the claim I had to go and measure to answer this finding, and a decision this
file makes should be pinned by a fixture on the path where it was doubted rather than only on x86_64,
where it is obvious. That clause now says the UEFI side of the pair runs on AArch64 as well, and
states why: the aarch64 kernel takes its `fw_cfg` base from the device tree, so the claim depends on
the firmware passing that tree on.

No other change: the carrier decision, the signed-source-wins rule and the refusal on disagreement are
unchanged.
