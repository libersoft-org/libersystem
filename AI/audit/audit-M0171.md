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

