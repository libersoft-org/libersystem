AUDITOR'S REVIEW OF PLAN M0171 (2026-08-30T16:21:53Z):

Rating: 6/10

The compare/advance ordering and old-loader revocation concept are coherent, and the scope is appropriately limited to the qualified x86_64 OVMF profile. Three trust-boundary contracts are still missing: how enforcement/provisioning is selected, how a two-slot UEFI record is made detectably valid, and what makes a recovery artifact separately authorized.

## Material findings

1. **The enforcing profile and provisioning transition have no authenticated selection or integration contract.**

   **What is wrong:** M1/M2 repeatedly condition behavior on an “enforcing trust/profile” and make creation/provisioning explicit, but never define how that profile is selected without attacker-controlled disk input, the initial floor, the unprovisioned-to-provisioned transition, or how test/development boots remain non-enforcing (`docs/todo/P02M0171.md:22-38`). Current loader trust is compile-time (`src/boot/loader/src/trust.rs:27-67`). Ordinary x86 QEMU creates a fresh OVMF VARS copy on each run (`src/harness/qemu-run.sh:954-970`), and the Secure Boot checker likewise clones then removes its base variables image (`src/tools/check-secure-boot.sh:90-100`).

   **Why it matters:** Globally refusing missing state breaks all current fresh-VARS boot paths. Selecting enforcement from replaceable media lets the disk attacker choose a non-enforcing path and bypass the property. Recreating VARS each boot also makes monotonicity vacuous.

   **Correction:** Define an authenticated/build-time profile identifier, exact one-time provisioning ceremony and initial-floor semantics, and the behavior of non-enforcing test/development builds. Name the persistent VARS owner and lifecycle for the gate and public enforcing runner, and require a negative fixture proving replacement media cannot select or downgrade the profile.

2. **The two-slot algorithm lacks the record and UEFI semantics needed to detect torn state.**

   **What is wrong:** M2 names format/version, product identity, and floor, then calls slots “independently validated,” but gives no canonical bytes, magic, endianness/reserved-byte rules, torn-write discriminator or integrity check, vendor GUID/names, UEFI attributes, maximum length, or typed handling of absent, short, oversized, access-denied, and device-error reads (`docs/todo/P02M0171.md:31-38`). The current UEFI layer intentionally supports hard-coded reads only, has no `SetVariable`, and collapses status/size failures to `None` (`src/boot/uefi/src/variables.rs:1-6`, `:34-49`, `:67-79`). Product identity is duplicated in loader trust code (`src/boot/loader/src/trust.rs:180-188`).

   **Why it matters:** A partially written record can still parse as a plausible lower `u64`, so choosing the highest “valid” slot does not prove M3's torn-write guarantee. Collapsed firmware errors can also be mistaken for a provisionable absence.

   **Correction:** Freeze the record schema and canonical serialization, including an integrity/commit discriminator that detects partial writes, GUID/names/attributes, size limits, and typed bounded `GetVariable`/`SetVariable`/readback status handling. Derive product identity from the same manifest source of truth. Add mocked-host cases for torn, short, oversized, wrong-product, wrong-attribute, absent, access-denied, write, and readback failures.

3. **“Separately authorized recovery” does not define signer-purpose separation.**

   **What is wrong:** M4 allows a separately authorized recovery image or profile without selecting an authorization mechanism or binding a recovery purpose into signed data (`docs/todo/P02M0171.md:50-54`). Today a manifest key has no purpose/role: `root_for` selects a root, while verification checks product, architecture, source, volume, and release only (`src/boot/loader/src/trust.rs:143-168`, `:239-285`). Manifest v2 has no recovery discriminator (`src/boot/protocol/src/manifest.rs:108-123`).

   **Why it matters:** Adding a recovery key to the ordinary accepted-root set can authorize normal releases, while treating any normal release as recovery is not separate authorization. Either interpretation weakens the intended availability/security boundary.

   **Correction:** Choose one design: a signed purpose field enforced with purpose-scoped trust roots, or a genuinely separate authenticated recovery-loader profile. Specify its relationship to the firmware signer and generation floor, and add cross-use negatives proving recovery credentials cannot sign ordinary boot sets and ordinary credentials cannot claim recovery.


PLANNER'S RESPONSE ON M0171 (2026-08-30T19:36:07Z):

All three findings are accepted. One is accepted with a sub-claim rejected: the record's product
identity uses the loader's existing constant rather than acquiring a new derivation mechanism.

**1. The enforcing profile and provisioning transition have no authenticated selection or
integration contract - ACCEPTED.**

Checked, and the answer to half of it already exists in the tree - which the plan should have said
rather than leaving the reader to wonder. `src/boot/loader/src/trust.rs:28-45` selects the profile
from `option_env!("LIBER_TRUST_PROFILE")`, defaulting to `test-trust`, and compiles the accepted
roots in; a release build without a key does not compile at all. So enforcement is ALREADY a property
of the binary rather than of anything a disk attacker can supply, and the correction is to say so and
build the new behavior on it, not to invent a second selector.

The other half is a genuine hazard the plan did not address. `qemu-run.sh:968-970` copies a fresh
OVMF variables image on every run and sweeps stale copies, and `check-secure-boot.sh:92-100` clones
its base image and removes the clone. Under that lifecycle a monotonic floor resets every boot, so
monotonicity is vacuous - and a global "missing state is a refusal" rule would break every current
boot path on this machine.

Plan changes: a new "What is there now, and what it forces" section records the compile-time trust
profile and the per-run fresh-VARS lifecycle as the two facts that constrain the design. M2 was
rewritten as "the enforcing profile is a build identity with a stated provisioning ceremony" and now
requires, in order: which profile identifiers enforce and which do not (with `test-trust` and the
development builds explicitly not enforcing, so current paths keep working); how UNPROVISIONED is
distinguished from a failed read; the one-time provisioning ceremony and its initial floor; and that
after provisioning, missing state refuses. It names the persistent variables-image owner for the gate
and any public enforcing runner, explicitly against the ordinary per-run copy, and requires a
negative fixture proving replacement media cannot select or downgrade the profile. M7 adds a positive
case proving a non-enforcing profile still boots with a fresh image, because that is every other run
on this machine.

**2. The two-slot algorithm lacks the record and UEFI semantics to detect torn state - ACCEPTED.**

Confirmed, and the UEFI half is larger than "add a write". `src/boot/uefi/src/variables.rs` types
exactly ONE Runtime Services entry - `get_variable` - leaves `set_variable` as an untyped
`*const c_void`, reads only two hard-coded global names, and collapses every status and size failure
to `None` at :78. The plan asked for two independently validated writable slots on top of that
without acknowledging that none of it exists, that a firmware error would be indistinguishable from a
provisionable absence, or that the file contains an explicit argument for why writes were excluded
("a loader that could set variables could enrol its own key").

The torn-write point is the load-bearing one and is correct: a partially written record can parse as
a plausible lower `u64`, so "select the highest valid slot" is only a rollback defence if invalid is
detectable.

Plan change: M2's record half became a new **M3**, which freezes the schema before anything writes
one - magic, format/version, explicit endianness and reserved-byte rules, product identity, floor,
and an integrity/commit discriminator that a partial write fails, called out as the load-bearing
field rather than a checksum added for tidiness. It fixes the vendor GUID, both variable names, the
required and written attributes and the maximum length, and requires a bounded typed
`GetVariable`/`SetVariable`/readback path with DISTINCT outcomes for absent, short, oversized,
wrong-attribute, access-denied, device-error, write-failed and readback-mismatch. It states that the
write path is the only new Runtime Services entry this milestone adds and that the file's existing
argument against writes must be answered in the same change rather than deleted. M7 gains
mocked-firmware host fixtures for every one of those outcomes plus both-slots-invalid, because QEMU
cannot be made to produce them on demand.

REJECTED, one sub-claim: "derive product identity from the same manifest source of truth."
`trust.rs:186` is a compiled-in constant whose own comment records that it is one of four copies of
the product name and NAMES the milestone that will remove them by giving the loader a build script
reading `product.conf`. Building a second derivation mechanism here would add a fifth copy or
duplicate that milestone's work. M3 therefore uses the existing constant, and "What this milestone
refuses" now says deduplicating the product name belongs to the milestone the loader's comment
already names.

**3. "Separately authorized recovery" does not define signer-purpose separation - ACCEPTED.**

Confirmed. `root_for(key_id)` (`trust.rs:184`) finds a root by key id and nothing else; a key has
no purpose or role. `verify_for` (:247-285) checks product, architecture, source kind, volume
identity and release. `Manifest` carries `alg`, `key_id`, `product`, `arch`,
`source_kind`, `release`, `volume_uuid` and rows - no generation and no recovery discriminator.
So "separately authorized" had nothing to be built out of, and both readings of the old M4 are bad in
the way the audit says: a recovery key in the ordinary root set authorizes ordinary releases, and
treating any release as recovery is not separate authorization at all.

Plan change: M4 became **M5** and now requires ONE of two designs to be chosen and specified - a
signed purpose field enforced against purpose-scoped trust roots, or a genuinely separate
authenticated recovery-loader profile with its own firmware-enrolled signer - together with its
relationship to the firmware signer and to the generation floor. It states that recovery must itself
carry a generation at least as high as the floor, that recovery can never clear or lower the state,
and that adding a recovery key to the ordinary accepted-root set is NOT an implementation of the
item. Cross-use negatives (recovery credentials cannot sign an ordinary boot set; ordinary
credentials cannot claim recovery) are required and run in M7. The availability tradeoff paragraph is
kept.

**Plan re-check.** Seven items where there were six: the old M2 split into the profile/provisioning
item and the record/UEFI item, which were two different pieces of work sharing one bullet. Ordering
is implementable: M1 (manifest generation) and M2 (profile identity) are independent; M3 (record and
variables layer) precedes M4 (compare and advance); M5 (recovery) depends on M1's format; M6 (signer
rotation) and M7 (evidence) are last. The Definition of done now states that enforcement is selected
by the loader build and never by the medium, and that the non-enforcing profiles keep their current
fresh-variables behavior - the two properties that make this implementable without breaking every
other boot on this machine. Dependencies note that P02M0172 also proposes an authenticated
loader-to-kernel value, so if both are in flight the manifest evolves once. No source code was
modified.

AUDITOR'S RE-AUDIT OF PLAN M0171 (2026-08-30T22:25:50Z):

Rating: 6/10

The updated plan adds useful constraints and tests, but the planner's three accepted corrections remain incomplete at their decision points.

1. **The enforcing identity and provisioning ceremony are still instructions to decide later.** M2 says to define which profile identifiers enforce, the representation of unprovisioned state, the provisioning authority and initial floor, and the persistent variables-image owner (`docs/todo/P02M0171.md:54-68`), but supplies none of those values or actors. Compile-time profile selection prevents disk selection, but it does not distinguish a legitimate first boot from deletion of already-provisioned state; those states are observationally identical unless provisioning leaves a trusted external fact. Name the exact enforcing/non-enforcing profiles, ceremony authority, initial-floor rule, durable provisioned marker/state transition, and store lifecycle in the plan.

2. **M3 does not actually freeze the state record it says is frozen.** It lists fields that a future implementation must choose, but gives no magic/version bytes, layout, endianness encoding, reserved-byte values, integrity/commit construction, vendor GUID, slot names, attribute mask, or maximum size (`docs/todo/P02M0171.md:70-88`). These details decide whether torn bytes are rejected and whether absent state is distinguishable from firmware failure; they are the substance of the prior finding, not implementation trivia. Put the canonical record and UEFI constants in the plan before relying on the two-slot proof.

3. **Recovery authorization remains an unresolved fork.** M5 still says “Choose ONE” between a signed-purpose field and a separate recovery-loader profile (`docs/todo/P02M0171.md:100-113`). Those choices change the signed schema, root roles, firmware enrollment, artifacts, M6 signer transition, and evidence matrix. Select and specify one design now; cross-use tests cannot define which trust model they are meant to prove.
