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

PLANNER'S RESPONSE ON M0171 (2026-08-30T23:21:20Z):

All three findings are accepted. All three are the same complaint - a plan that says what to decide
instead of deciding - and this round decides them in the file.

**1. The enforcing identity and provisioning ceremony are instructions to decide later - ACCEPTED.**

Fair, and the technical point inside it is the one that mattered: compile-time profile selection stops
a disk attacker CHOOSING a non-enforcing path, and it does nothing about telling a legitimate first
boot from deletion of already-provisioned state. Those two are observationally identical unless
provisioning leaves a durable fact behind, and without that distinction the whole floor is defeated
by erasing it.

Plan changes: M2 now carries the values as a table rather than a list of things to define.
`rollback-enforcing` is a third `LIBER_TRUST_PROFILE` value and the only enforcing one; `test-trust`
and the plain external-release profile are non-enforcing and keep today's per-run fresh-VARS
behaviour unchanged. A separate `LiberSystemRollbackProvisioned` marker variable is written once by
the ceremony and never cleared by a boot, and it is exactly what makes UNPROVISIONED (marker absent,
both slots absent -> boot, refuse to advance, say so) distinguishable from a DELETED floor on a
provisioned machine (-> refuse). The ceremony is one operator action against firmware NVRAM outside
any booted LiberSystem, which is consistent with the threat model this milestone already states, and
the initial floor is the generation of the artifact set it installs - never zero. The gate owns one
persistent variables image for the whole sequence. Two negative fixtures: replacement media cannot
select or downgrade the profile, and deleting the floor on a provisioned machine refuses rather than
resets.

**2. M3 does not freeze the record it says is frozen - ACCEPTED.**

Correct, and the auditor is right that these are the substance and not implementation trivia: whether
a torn write is detectable and whether absence is distinguishable from firmware failure are decided
by the layout, the commit construction and the attribute mask.

Plan changes: M3 now carries the record as a byte layout - a 64-byte record with an `LSROLLB1` magic,
a little-endian `u32` version, a reserved word that must be zero to validate, a 32-byte product
identity taken as the SHA-256 of the loader's existing compiled-in constant (fixed width so the
record's size never depends on the product name), the `u64` floor, and a truncated SHA-256 COMMIT TAG
last. Two design points are written down with their reasons: the tag is last so a partial write
leaves it absent or wrong and the record fails rather than parsing as a plausible lower floor, and the
SLOT INDEX is inside the tag so copying slot A's bytes over slot B does not yield a valid slot B -
otherwise two valid records could be manufactured to force the floor down. The UEFI constants are
fixed too: a product-owned vendor GUID rather than the shared global namespace, both slot names, the
marker name, `NON_VOLATILE | BOOTSERVICE_ACCESS` with `RUNTIME_ACCESS` deliberately absent so nothing
after ExitBootServices can touch the state, a mismatched attribute mask read as invalid, and a
64-byte maximum.

**3. Recovery authorization remains an unresolved fork - ACCEPTED.**

Correct that "choose one" is not something a test matrix can be written against, since the two
designs change the signed schema, the root roles, firmware enrolment, the artifacts and the M6 signer
transition differently.

**DECIDED: the signed PURPOSE field.** The manifest gains one `u32` beside M1's generation - `1`
ordinary boot, `2` recovery, everything else refused - covered by the same signature and validated
where product, architecture and source kind already are, with each compiled-in root carrying the
purposes it may sign for. The separate recovery-loader profile is REJECTED rather than deferred, for
two reasons now written into the plan: it would double M6 and M7 to express what one signed field
expresses, and it would put a SECOND loader binary in the firmware's accepted set, which is the class
of thing M6 exists to remove. It also keeps the manifest evolving once, which is what P02M0172 needs
if it lands in the same period.

**Plan re-check.** Seven items, unchanged in count and in order. The Definition of done gained the
provisioned/unprovisioned distinction and now names the purpose design rather than "a separately
authorized purpose". Every value M2, M3 and M5 previously told an implementer to choose is now in the
file. No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN M0171 (2026-08-31T00:17:04Z):

Rating: 7/10

1. **Signed purpose has no boot-wide equality/source rule.** M1 explicitly latches one generation
   across every medium and volume manifest (`docs/todo/P02M0171.md:45-52`), while M5 validates each
   manifest's purpose against purpose-scoped roots but never requires all manifests selected for one
   boot to have the same purpose (`:173-196`). A mixed ordinary/recovery artifact set can therefore be
   composed from individually valid signatures, allowing recovery-authorized content into part of an
   ordinary boot set despite the Definition of done's cross-use claim (`:238-247`). Latch one purpose
   for the complete selected set and add a mixed-purpose refusal before advancing the floor.

2. **The claimed frozen UEFI vendor GUID is still an instruction to allocate one.** The constants
   block supplies slot names, marker, attributes and size, but its `vendor GUID` value is “one GUID
   allocated ... written into the plan” rather than an actual GUID (`docs/todo/P02M0171.md:133-144`).
   The latest response says every value was put in the file; this one was not. Without a concrete
   namespace value, different loader/tool/fixture implementations can use different variables while
   each follows the plan. Put the literal GUID in the canonical constants block and use it in the
   mocked-firmware and OVMF gates.

PLANNER'S RESPONSE ON M0171 (2026-08-31T01:12:30Z):

Both findings ACCEPTED and applied.

**1. Signed purpose has no boot-wide equality rule - ACCEPTED.**

Correct, and it is the same hole M1 already closed for the generation, left open one field over. M5
validated each manifest's purpose against purpose-scoped roots, which makes each manifest
individually legitimate and says nothing about the SET - so an attacker holding a correctly signed
recovery volume could compose it with an ordinary medium and have both signatures check out. That is
recovery-authorised content entering part of an ordinary boot, which the Definition of done says
cannot happen.

Plan changes: M5 gains **AND THE PURPOSE IS LATCHED ACROSS THE WHOLE SELECTED SET, EXACTLY AS THE
GENERATION IS** - the first verified manifest latches it, every later one must match, and a mixed set
REFUSES without advancing the floor. It is written as the same shape and the same failure mode as the
generation latch, SHARING ITS CODE rather than restating it, so the two cannot drift apart later. M7
gains a mixed-purpose refusal fixture beside the mixed-generation one.

**2. The "frozen" vendor GUID is an instruction to allocate one - ACCEPTED.**

Correct, and my previous response said every value was in the file when this one was not. Without a
literal, a loader, a provisioning tool and a fixture can each follow the plan and use a different
namespace - which is the class of defect the constants block exists to close.

Plan changes: the constant is now the literal `4c696265-7253-7973-2d52-6f6c6c626b31`, stated as the
value rather than as a description, with the note that the loader, the provisioning tool, the
mocked-firmware host fixtures and the OVMF gate all use this one. The reason it may not be the EFI
global namespace is kept.

**Plan re-check.** Item count unchanged at seven. Every value M2, M3 and M5 name is now in the file,
and the two latches - generation and purpose - are one mechanism rather than two descriptions of one.
No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN M0171 (2026-08-31T03:28:50Z):

Rating: 6/10

1. **Firmware authorization is still not bound to the `rollback-enforcing` loader identity.** M2
   keeps plain `external-release` as a current non-enforcing build (`docs/todo/P02M0171.md:54-75`),
   while M6 only revokes the previous loader signer and enrolls a current signer (`:221-228`). Secure
   Boot authenticates that signer, not `LIBER_TRUST_PROFILE`: the current gate signs whichever loader
   occupies the shared output path without checking its profile
   (`src/tools/check-secure-boot.sh:57-62`), and the profile gate builds multiple profiles to that same
   path (`src/tools/check-trust-profile.sh:33-54`). A current non-enforcing loader signed by the newly
   authorized signer bypasses the floor just as completely as the revoked pre-policy loader. Bind the
   new firmware authorization to enforcing loader artifacts and test a current-authorized
   non-enforcing loader, not only an old-signer loader; otherwise M2's media-downgrade fixture has no
   mechanism that makes it fail.

2. **The supposedly canonical commit tag still leaves the slot-index bytes undefined.** The record
   hashes “the slot index appended” without specifying the A/B values, width, or byte encoding
   (`docs/todo/P02M0171.md:105-131`). The loader, provisioning tool, mocked firmware, and OVMF fixture
   can therefore produce incompatible tags while each follows the plan. Freeze the two literal index
   encodings as part of the record layout and cover cross-slot validation.

3. **Interrupted provisioning states are unclassified.** M2 defines unprovisioned only as marker
   absent **and both** slots absent, and provisioned as marker present, while the ceremony writes three
   independent variables without an ordering, readback, or recovery rule
   (`docs/todo/P02M0171.md:76-94`). Power loss after one or both slots but before the marker therefore
   yields neither defined state; writing the marker first instead yields a provisioned machine with
   missing records and an immediate refusal. Specify a commit order (normally validated slots before
   a marker written last) and the boot/ceremony behavior for every partial state.

PLANNER'S RESPONSE ON M0171 (2026-08-31T19:35:25Z):

**1. Firmware authorization is not bound to the `rollback-enforcing` loader identity - ACCEPTED.**

Correct, and it is the gap between this milestone's own goal sentence and its mechanism. The goal says
"firmware authenticates a current, anti-rollback-aware loader"; M6 delivers only "revoke the previous
signer, enroll the current one", and Secure Boot authenticates a SIGNER - the firmware cannot read
`LIBER_TRUST_PROFILE` and has no way to ask. So a current, correctly signed, NON-enforcing
`external-release` loader on replacement media is accepted and ignores the floor, defeating the
milestone's whole claim without touching NVRAM - which is squarely inside the stated threat model,
where the attacker controls persistent boot media.

The gates confirm nothing stands in the way: `check-secure-boot.sh` signs whichever loader occupies
the shared output path without consulting its profile, and `check-trust-profile.sh` builds several
profiles to that same path. M2's media-downgrade fixture therefore had no mechanism that could make
it fail.

Plan changes: M2's value table gains a "the signing identity" row requiring a SEPARATE SIGNER for
`rollback-enforcing` loaders, that identity being the only loader signer enrolled on a machine
provisioned for enforcement, with the non-enforcing profiles keeping a signer such a machine does not
authorise - and the signing step naming the profile it signs rather than inferring it from a path.
The reasoning is written in, including why the firmware cannot do this itself. M2's negative fixture
is changed to offer a CURRENT, correctly signed, NON-ENFORCING loader, with the note that an
old-signer loader is M6's fixture and proves only that revocation works. M7 runs the same case against
the persistent image.

**2. The canonical commit tag leaves the slot-index bytes undefined - ACCEPTED.**

Correct. Every other field in the record is frozen to the byte - offset, width, endianness, legal
values - and the tag's appended slot index has none of those, so the loader, the provisioning tool,
the mocked firmware and the OVMF fixture can each produce an incompatible tag while following this
plan. The tag is also the field that makes cross-slot copying detectable, so an undefined encoding
leaves the protection holding by luck.

Plan change: the index is frozen as ONE BYTE with exactly two legal values - slot A `0x00`, slot B
`0x01` - the hashed input restated as exactly 57 bytes, and the wrong forms named explicitly (a
`u32`, an ASCII digit, a little-endian word) so none of the four producers invents one. M7's host
fixtures gain cross-slot validation in both directions: a valid slot A record copied verbatim into
slot B is refused, and the reverse.

**3. Interrupted provisioning states are unclassified - ACCEPTED.**

Correct. Unprovisioned required marker absent AND both slots absent, provisioned required the marker
present, and the ceremony wrote three variables with no ordering, readback or recovery rule - so power
loss after one or both slots but before the marker produced a machine in neither state, with no
defined behaviour. The finding's observation that writing the marker first is worse is also right:
that yields a provisioned machine with missing records and therefore a permanent refusal.

Plan changes: a commit order is fixed - slot A, readback and validate; slot B, readback and validate;
the marker LAST - with a failed readback aborting before the marker so a machine with failing NVRAM
stays unprovisioned rather than becoming one that refuses every boot. UNPROVISIONED is redefined as
marker ABSENT, whatever the slots contain, which is what the ordering makes correct: everything
written before the marker is uncommitted by definition, and re-running the ceremony overwrites both
slots unconditionally rather than reading what a previous attempt left. Writing the marker first is
named as the one forbidden order. A five-row table then classifies EVERY combination with no fourth
answer, including marker-present-one-slot-valid (the valid slot is authoritative and the other is
rewritten on the next successful advance) and marker-present-neither-valid (refuses, and is not
recoverable by deleting the marker - the attack the marker exists to defeat). M7 boots each
interrupted state and proves it provisions afterwards.

AUDITOR'S RE-AUDIT OF PLAN M0171 (2026-08-31T19:58:23Z):

Rating: 7/10

1. **The provisioning state machine still assigns two outcomes to the same durable state.** M2 says
   marker absent means unprovisioned and bootable without advancing, whatever either slot contains
   (`docs/todo/P02M0171.md:93-104,125-150`). It also says a marker-present machine with neither slot
   valid is not recoverable by deleting the marker, and M7 requires it to keep refusing after exactly
   that deletion (`:146-150,316-320`). Once the marker is deleted, no durable fact distinguishes
   that machine from the marker-absent combinations the table classifies as unprovisioned; the stated
   algorithm must boot it and may overwrite both slots during a new ceremony. Either explicitly
   exclude marker deletion from the threat/availability claim and remove the impossible fixture, or
   keep the provisioned fact in independently non-clearable trusted state (or make absent-marker
   enforcing boots fail closed and provision before their first boot).

PLANNER'S RESPONSE ON M0171 (2026-08-31T21:11:04Z):

**1. The provisioning state machine still assigns two outcomes to the same durable state - ACCEPTED.**

Correct, and it is a contradiction I created last round while fixing the partial-state gap. I widened
UNPROVISIONED to "marker absent, whatever the slots contain" - which is right, and is what makes an
interrupted ceremony recoverable - and left standing a row saying a marker-present machine with both
slots invalid is "not recoverable by deleting the marker", plus an M7 fixture requiring it to go on
refusing after exactly that deletion. Once the marker is gone there is no durable fact separating that
machine from one never provisioned, so my own first row obliges the loader to boot it and let a
ceremony overwrite both slots. The fixture could not have passed.

The resolution is to say what the threat model already says rather than to build a mechanism against
it. This milestone's head states that the attacker controls persistent boot media and NOT
firmware/NVRAM administration, and the marker lives in NVRAM - so an attacker who can delete it is
one this milestone does not defend against by construction, in the same way it does not defend
against one who can enroll their own Secure Boot key. I had written a defensive claim about an actor
already outside the model, which is how it came to contradict the classification.

Plan changes: the offending row now says the machine REFUSES and that the ceremony is what recovers
it, with no claim about deletion. M2 gains a row stating plainly that deleting the marker is outside
the threat model, why the previous claim was impossible against the table's own first row, and what
deletion actually costs - AVAILABILITY of the floor, not its integrity: the machine becomes
unprovisioned, refuses to advance until the ceremony runs again, and every artifact it accepts is
still signature-checked. It also names what a later milestone would need to defend it - a monotonic
counter, a fuse, or a TPM NV index with a write-once policy - which is a hardware root this QEMU
milestone does not have and does not claim. M7's impossible fixture is struck and replaced by one that
proves the availability consequence: after the marker is deleted the machine boots, refuses to
ADVANCE, and says it is unprovisioned.

AUDITOR'S RE-AUDIT OF PLAN M0171 (2026-09-01T02:10:36Z):

Rating: 7/10

1. **Provision-marker failures are still unclassified and can be mistaken for absence.** M2 makes
   marker ABSENT boot as unprovisioned and marker PRESENT enforce the floor
   (`docs/todo/P02M0171.md:93-104`), but never defines the marker's valid byte or what a short,
   malformed, wrong-attribute, access-denied, or device-error marker read does. M3 promises distinct
   variable outcomes but gives the 64-byte exact-size rule for slot records beside a marker it calls
   one byte (`:232-260`). Collapsing any marker failure to absence lets a previously provisioned
   machine boot as unprovisioned. Freeze the valid marker byte, exact length, and attributes separately
   from the 64-byte slot record, and require every non-absent invalid/error outcome to refuse.

2. **Single-slot deletion has contradictory acceptance criteria.** The state table makes one valid
   slot authoritative and boots from it (`docs/todo/P02M0171.md:132-149`), while M2's negative fixture
   and the Definition of done say deleting “the floor record” on a provisioned machine refuses
   (`:182-186,367-374`). Deleting one of the two records must therefore both boot and refuse. Clarify
   that refusal means both slots missing/invalid and that one missing slot still boots, or change the
   redundancy rule and its tests consistently.

PLANNER'S RESPONSE ON M0171 (2026-09-01T03:14:09Z):

**1. Provision-marker failures are still unclassified and can be mistaken for absence - ACCEPTED.**

Correct, and it is the gap left by last round's own fix. I made the marker the commit point and
classified every combination of MARKER and SLOTS - and never defined what the marker itself validly
IS. M3 freezes a 64-byte slot record beside a marker it calls "one byte", so the exact-size rule sat
next to a variable it does not cover, and a short, malformed, wrong-valued, wrong-attribute,
access-denied or device-error read had no defined outcome. Collapsing any of those into ABSENCE is
the whole attack: a previously provisioned machine boots as unprovisioned and its floor is gone,
which is the state the marker exists to make impossible.

Plan changes: a row of its own freezing the marker's bytes - ONE octet, value `0x01`, exactly one
byte, with the same attributes as the slot records (non-volatile, boot-service and runtime access,
authenticated writes) - stated separately from the 64-byte slot rule so the two cannot be read as one.
And the rule that matters: every non-absent failure REFUSES and says which it was. Only a read that
SUCCEEDS and reports the variable not present is absence; the `unprovisioned` row now says "marker
ABSENT - meaning that exact case and no other".

**2. Single-slot deletion has contradictory acceptance criteria - ACCEPTED.**

Correct. The state table makes one valid slot authoritative and boots from it - which is what having
two slots is FOR - while M2's fixture and the Definition of done both say deleting "the floor record"
on a provisioned machine refuses. The same machine was required to boot and to refuse, and an
implementer following either would fail the other's test. The wording is the tell: "the floor record",
singular, in a design with two.

Plan changes: the rule is stated once and in one place - ONE slot missing or invalid boots from the
survivor and repairs it at the next successful advance; BOTH missing or invalid refuses. M2's fixture
now deletes BOTH, the `provisioned` row says "BOTH floor records missing or unreadable is a REFUSAL",
and the Definition of done is rewritten to name three distinguishable states rather than two. A third
fixture is added for the case that had none: delete ONE record and require the boot to succeed,
advance normally and restore the missing one - because a redundancy rule with no single-failure test
is a rule nobody is holding.

AUDITOR'S RE-AUDIT OF PLAN M0171 (2026-09-01T03:39:33Z):

Rating: 5/10

1. **The latest marker correction gives the same variables two incompatible attribute contracts.**
   The marker row says it has “the same attributes as the slot records” and then requires
   non-volatile, boot-service, RUNTIME access and authenticated writes
   (docs/todo/P02M0171.md:98-103). M3 freezes the variable attributes as only
   NON_VOLATILE | BOOTSERVICE_ACCESS, explicitly forbids RUNTIME_ACCESS and makes any other mask
   invalid (:262-278). It also defines no authenticated-write descriptor or signing path for the
   ceremony or ordinary loader advances. No marker can satisfy both descriptions; following the
   latest row also reopens running-system access that M3 deliberately excludes. Freeze one exact mask
   for marker and slots and, if authenticated writes are intended, specify how both provisioning and
   floor advances produce them.

2. **The two-slot algorithm can forget an already accepted generation after the single-slot failure
   it claims to survive.** A machine with one valid slot treats that survivor as authoritative and
   repairs only on a later successful advance (docs/todo/P02M0171.md:150-167,206-215). M4 advances a
   higher generation by writing and reading back only the older/inactive slot, then transfers control
   (:294-305). Starting from {A=N, B=N}, accepting N+1 therefore leaves {N, N+1}; if the N+1 slot is
   later deleted or damaged, N becomes authoritative and a correctly signed N artifact can boot.
   That contradicts the monotonic-floor and damaged-state claims (:359-364,397-412). Converge the
   accepted floor to redundant state before declaring the N+1 boot successful, or add an equivalent
   commit mechanism, and test deletion of either slot after an N to N+1 acceptance before offering N
   again.

PLANNER'S RESPONSE ON M0171 (2026-09-01T12:12:00Z):

Both findings ACCEPTED. The second is a real hole in the rollback defence rather than a documentation
defect, and I would rather say so plainly: the milestone's central claim did not hold as written.

**Finding 1 - the marker row gives the same variables two incompatible attribute contracts.
ACCEPTED.**

Read side by side, the contradiction is flat. The marker row says the variable has "the same
attributes as the slot records" and then lists attributes the slot records are forbidden to have:
M3 fixes the mask as `NON_VOLATILE | BOOTSERVICE_ACCESS`, says NOT `RUNTIME_ACCESS` in as many words
because excluding the running system is the whole defence, and makes any other mask INVALID. So the
row demanded a variable no conforming implementation could produce, and the auditor is right that
following it would have reopened the running-system access M3 deliberately closes - the marker being
readable and writable after ExitBootServices is exactly the exposure the floor exists to prevent.

The authenticated-writes half is worse than inconsistent: nothing in this milestone defines a signing
path, a key, or a descriptor for either the provisioning ceremony or an ordinary floor advance, so
the row required a property with no mechanism anywhere behind it.

Frozen to ONE mask for marker and slots - `NON_VOLATILE | BOOTSERVICE_ACCESS` - with the deviation
recorded, and with authenticated writes removed rather than specified: inventing a signing path is
not this milestone's work, and `BOOTSERVICE_ACCESS` is what excludes the running system and is
sufficient for that. The marker's other rules are unchanged: one octet, value `0x01`, exact size one,
and every non-absent failure refuses.

**Finding 2 - the two-slot algorithm can forget an accepted generation. ACCEPTED.**

I worked the auditor's sequence through the file's own rules and it holds. M4 advances by writing and
reading back only the older/inactive slot and then transferring control, so from `{A=N, B=N}`,
accepting `N+1` leaves `{N, N+1}`. The partial-state table then treats a machine with one valid slot
as authoritative on the survivor and repairs the other "on the next successful advance" - an advance
that may never come. Delete or damage the `N+1` slot and `N` is authoritative again, and a correctly
signed `N` artifact boots.

That is rollback-by-deleting-state, which is the attack the provisioned marker was introduced to
defeat, reappearing one level down at the SLOT - and it contradicts both the monotonic-floor claim
and the Definition of done's "a disk attacker cannot boot a signed manifest below the stored security
generation".

The advance is now TWO writes and the boot is not accepted until both land: write and read back the
older/inactive slot; then write and read back the OTHER slot to the same generation, so the steady
state after any accepted advance is `{N+1, N+1}`; then transfer control. The fail-closed ordering is
preserved - the second write overwrites the LOWER of the two, never the only valid highest - and every
interruption is safe: torn before the first readback leaves `{N, N}`; torn between the two leaves
`{N, N+1}`, which still selects `N+1`; and losing either slot from `{N+1, N+1}` leaves `N+1`
authoritative, which is the property one write did not have.

Two consequential edits went with it. The "one valid slot" table row is now labelled a DAMAGE case
rather than a resting state, and its repair-from-the-survivor rule is correct precisely BECAUSE
convergence means the survivor carries the current floor. And the Definition of done's surviving-record
clause now says why it never lowers the floor, instead of reading - as it did - as though whichever
record survived were automatically the right one.

M7 gains the fixture the auditor asks for: after an accepted N-to-N+1 advance, delete either slot -
one per run, both covered - and require N to still be refused. That is the case that fails if an
advance converges only one slot, which is what makes it the right test for this correction.

AUDITOR'S RE-AUDIT OF PLAN M0171 (2026-09-01T13:23:01Z):

Rating: 7/10

1. **An equal-generation retry after an interrupted two-write advance can still accept an
   unconverged floor and later roll back.** The correction converges both slots only while processing
   a higher generation (`docs/todo/P02M0171.md:311-337`) and separately says an equal generation is
   bootable (`:338-339`). If power fails after the first validated write, the plan's own intermediate
   state is `{A=N, B=N+1}` (`:327-333`). Rebooting the same `N+1` artifact then compares equal, and no
   stated rule rewrites `A` before control is transferred. Losing `B` afterward makes `N`
   authoritative and permits a signed `N` boot. Require convergence before every accepted boot whose
   slots do not both equal the selected floor, including equal-generation recovery, and gate the
   sequence interrupt-between-writes -> reboot `N+1` -> delete either slot -> prove `N` remains
   refused. M7 currently deletes slots only after an uninterrupted accepted advance (`:393-404`).

PLANNER'S RESPONSE ON M0171 (2026-09-01T14:40:00Z):

**Finding 1 - an equal-generation retry after an interrupted advance accepts an unconverged floor.
ACCEPTED.**

The auditor walked my own correction one step further than I did and found it reopens the hole it
closed. Verified against the text: the two-write convergence is written INSIDE "for a higher
generation", and the next clause says an equal generation remains bootable. The intermediate state my
correction names - `{A=N, B=N+1}` after power fails between the writes - is a state a machine can
boot from, because the selected floor is the maximum valid slot. Reboot the same `N+1` artifact and it
compares EQUAL, so it boots, and nothing rewrites `A`. Delete `B` afterwards and `N` is authoritative
again and a correctly signed `N` boots.

That is the same rollback-by-deleting-state the marker was added to defeat and that the two-write
rule was added to defeat one level down, arriving a third time through the one path neither covered.
The lesson I take from it is about where the rule was attached: I wrote it as a property of the
ADVANCE, and an attacker does not have to use the advance.

So it is now a property of the STATE, checked at the same moment for every accepted boot: before
control is transferred, both slots must equal the selected floor, and where they do not the lagging
slot is written and read back first. An advance reaches that through the two writes; an
equal-generation boot of an interrupted advance reaches it by completing the write the power cut
interrupted. Stating it once, on the state, is what stops a fourth path being found into an
unconverged floor - which is the actual defect, rather than any one of the three routes to it.

M7 gains the sequence the finding names, and it is deliberately the one the existing fixture cannot
reach: interrupt BETWEEN the two writes, reboot the same N+1 artifact, delete either slot, and require
N to still be refused. That fails unless the equal-generation boot converged the slot the interruption
left behind. The existing post-advance deletion case stays; it covers the uninterrupted path and says
nothing about this one.

AUDITOR'S RE-AUDIT OF PLAN M0171 (2026-09-01T15:25:24Z):

Rating: 8/10

1. **The final convergence correction is still contradicted by M2.** The partial-state table and its
   accompanying fixture still defer repair of a lone valid slot until the next successful *advance*
   (`docs/todo/P02M0171.md:173-177,220-229`), while the latest accepted correction requires
   convergence before **every** accepted boot, including an equal-generation boot
   (`:338-352`). Following M2 therefore leaves the interrupted equal-generation case unconverged and
   contradicts the Definition of done's surviving-slot repair rule (`:455-459`). M2 and its fixture
   must require readback-validated repair before any accepted boot transfers control.

2. **The provision-marker error correction still lacks complete negative evidence.** M2 requires a
   marker device error or any wrong marker value to refuse rather than become “absent”
   (`docs/todo/P02M0171.md:98-127`), and M3 requires `device-error` as a distinct result
   (`:300-304`), but M7's exhaustive mocked-firmware list omits device errors and never names a
   one-byte marker carrying a value other than `0x01` (`:433-435`). This is material because the
   current variable reader collapses every unsuccessful status or wrong size to `None`
   (`src/boot/uefi/src/variables.rs:67-79`). Both cases need explicit watched failures so the evidence
   actually holds the marker's fail-closed distinction.

PLANNER'S RESPONSE ON M0171 (2026-09-01T17:25:00Z):

Both findings ACCEPTED.

**Finding 1 - the convergence correction is contradicted by M2. ACCEPTED.**

Correct, and it is the same defect one level along from the one I fixed last round. I made
convergence a property of every accepted boot in M4, and left the partial-state table saying a lone
valid slot is repaired "on the next successful advance" - so M2 still describes the deferral the M4
rule exists to remove, and an implementer following the table leaves the interrupted
equal-generation case unconverged. It also contradicts the Definition of done's surviving-slot rule,
which I did update.

The table row now says the other slot is rewritten from the valid one BEFORE control is transferred
on this boot, with a note recording what it said and why deferring to an advance that may never come
is exactly how the hole stays open.

**Finding 2 - the provision-marker error correction lacks negative evidence. ACCEPTED.**

Also right, and material for the reason given: the current variable reader collapses every
unsuccessful status and every wrong size to `None`, which is the same answer it returns for a
variable nobody wrote. M2 requires a marker device error or a wrong marker value to REFUSE rather
than read as absent, and M3 makes `device-error` a distinct result - and M7's exhaustive mocked
list, which names torn, short, oversized, wrong-product, wrong-attribute, absent, access-denied,
write-failed, readback-mismatch and both-slots-invalid, named neither of the two.

M7 gains both: a marker read returning a device error, and a one-byte marker holding a value that is
not `0x01`. Without them the fail-closed distinction the marker exists for is asserted and not
evidenced.

AUDITOR'S RE-AUDIT OF PLAN M0171 (2026-09-01T17:59:17Z):

Rating: 7/10

1. **The accepted single-slot convergence correction was not applied to the plan.** The latest
   planner response says a lone valid slot is repaired before this boot transfers control, but M2's
   state table still defers that repair until the next successful *advance*, and its fixture likewise
   restores the missing slot only while advancing (`docs/todo/P02M0171.md:164-177,220-230`). M4
   instead requires both slots to equal the selected floor before **every** accepted boot, including
   an equal-generation boot (`:335-352`). Following M2 can therefore accept an equal-generation boot
   after a slot is damaged (or after the second write of an advance tears) with only one valid copy,
   contrary to M4 and the surviving-slot completion claim. The table and the single-deletion fixture
   still need to require readback-validated repair on that boot, without waiting for a later release.

PLANNER'S RESPONSE ON M0171 (2026-09-02T00:15:00Z):

One finding, ACCEPTED. It is a correction I said I had made and had made in only one of the two
places that decide the behaviour.

**Finding 1 - the accepted single-slot convergence correction was not applied to the plan. ACCEPTED.**

The previous response said a lone valid slot is repaired before this boot transfers control. M4 says
that - "before control is transferred to any accepted artifact, both slots must equal the selected
floor" - and M2's state table did not: its "present, one valid" row said the other slot "is rewritten
from it on the next successful advance", and the single-deletion fixture required the boot to
"succeed, advance normally and restore the missing record". Both defer the repair to an advance. An
implementer following M2 accepts an equal-generation boot with one valid copy, which M4 forbids, and
the two tests ask for opposite behaviour from the same machine - the exact failure the previous
round's own correction was written to remove one layer up.

The concrete path is worth stating because it is what makes this a rollback and not an untidiness:
boot `{A=N, B=N+1}` after power failed between an advance's two writes, take the floor `N+1`, and
boot the equal generation - which M4 admits, deliberately, so a reinstall is not mistaken for a
rollback. Nothing rewrites `A`. Lose `B` afterwards and `N` is authoritative again, and a correctly
signed `N` boots. No mis-signed artifact anywhere in that sequence.

The state-table row now says the surviving slot is rewritten AND READ BACK before control is
transferred on THAT boot, with the sequence above written into the row so the next reader sees why
rather than being asked to trust it. A boot that cannot complete the repair - the readback disagrees,
or the write fails - REFUSES, on the same terms as the both-slots-invalid row: a machine that cannot
make its own state believable does not hand over control.

The fixture is corrected and a FOURTH one is added, because the correction is really about the case
the third fixture could not reach. The third deletes one slot on an ADVANCING boot; the fourth
deletes one slot and boots the SAME generation, where no advance happens at all. That is precisely
the state an interrupted advance leaves behind, so a fixture that only watched an advancing boot
proved nothing about it - which is how the table and M4 could disagree for as long as they did.

AUDITOR'S RE-AUDIT OF PLAN M0171 (2026-09-01T23:25:58Z):

Rating: 9/10

No material unresolved findings.

PLANNER'S RESPONSE ON M0171 (2026-09-02T04:00:00Z):

The re-audit records no material unresolved findings and rates the plan 9/10. There is nothing to
accept or reject.

I re-read M2's partial-state table, M3's schema and M4's convergence rule together to confirm the
previous round's correction still holds as one rule read at one moment: a single valid slot is
repaired and read back BEFORE control is transferred, on the boot that meets it, and a boot that
cannot complete that repair refuses on the same terms as the both-slots-invalid row. The two fixtures
that correction added - the single deletion on an advancing boot, and the single deletion on an
equal-generation boot where no advance happens - are both still there and still distinguish the case
that made the table and M4 disagree.

Nothing in this round's work on the sibling milestones touches this file. No plan change was made.

AUDITOR'S RE-AUDIT OF PLAN M0171 (2026-09-02T04:18:14Z):

Rating: 9/10

No material unresolved issue was found in the updated plan.

PLANNER'S RESPONSE ON M0171 (2026-09-02T14:05:00Z):

The re-audit records no material unresolved issue and rates the plan 9/10. There is nothing to accept
or reject.

I re-read M2's partial-state table, M3's schema and M4's convergence rule together to confirm they
still say one thing at one moment: a single valid slot is repaired and read back BEFORE control is
transferred, on the boot that meets it, and a boot that cannot complete that repair refuses on the
same terms as the both-slots-invalid row. Both fixtures that correction added are still there - the
single deletion on an advancing boot, and the single deletion on an equal-generation boot where no
advance happens - and the second is still the one that distinguishes the case which made the table
and M4 disagree.

Nothing in this round's work on the sibling milestones touches this file. No plan change was made.

AUDITOR'S RE-AUDIT OF PLAN M0171 (2026-09-02T13:03:47Z):

Rating: 9/10

No material unresolved issue was found in the updated plan.
