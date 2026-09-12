IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-10T09:30:00Z):

Scope: `docs/todo/P02M0136.md` - the text foundation (shaping, fallback, layout). Read in full
before any work, together with the milestones it names.

BLOCKER, reported rather than worked around: this milestone's shaping and library work is gated on
`P02M0103b` (the frozen `GlyphRun` and font-resource contract) and its end-to-end gate on
`P02M0103c` (the conforming `soft2d` renderer), and `P02M0103` is a PHASE-4 FUTURE VISION milestone
that is "ACTIVATED ONLY AFTER THE SERVER PHASE AND BY EXPLICIT PROJECT-OWNER APPROVAL, PART BY
PART" (its own status line and the `[i]` row in `docs/todo/TODO.md`). Neither `P02M0103b` nor
`P02M0103c` has been activated in this job - see `code-audit-P02M0103.md`, which reports that a
blanket "implement P02M0103" is not the part-by-part activation the plan reserves to the owner. So
the dependency this milestone declares does not exist yet, and its status line says as much:
"FUTURE TRACK. NOT A PHASE-2 COMPLETION GATE."

What the plan says is startable regardless: its FIRST item, "THE FONT PACKAGE ROLE AND THE
CATALOGUE ARE BUILT HERE, FIRST", which "depends on nothing in P02M0103". Read closely, that item
is itself substantial and self-contained enough to be its own effort: it freezes a font
destination (`vol://system/share/fonts`), adds a manifest role (`FONTDIR`) whose ServiceManager
dispatch is the one case that does not duplicate the provider's root but mints a scoped
`volume-admin.open-directory` client, stages a pinned last-resort face and a Unicode conformance
corpus with their licences, and adds a whole `role = "service"` font-catalogue service reached
through the Factory role and a new PermissionManager `font-catalogue` capability with its enum
variant, ordinal, held client, grant arm and bootstrap tag. That is a milestone-sized piece of
work on its own, and the rest of P02M0136 (the OpenType profile, the shaping pipeline, the
`GlyphRun` producer, the bounds, the Unicode host gates and the guest render gate) cannot begin
until `P02M0103b`'s contract is frozen - which is the ordered dependency the file states twice.

Decision: nothing implemented. Starting the font-package-and-catalogue item alone would deliver a
font destination and a catalogue service with no shaping, no layout and no renderer to consume
them - a partial milestone whose only consumer (the shaping work) is blocked - and the plan orders
that item FIRST precisely so the blocked work can follow it, not so it can ship alone. Per the job's
rule against stubs and partial required behaviour, this is reported as blocked rather than
half-built.

What would unblock it: project-owner activation of `P02M0103b` (and `P02M0103c` for the end-to-end
gate), after which the font-package item leads and the shaping/library/integration work follows in
the order the file fixes.

Verification: not applicable - no change was made. Commands run: read of `docs/todo/P02M0136.md`,
`docs/todo/P02M0103.md`, `docs/todo/TODO.md`.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-10T20:43:15Z):


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-10T20:43:20Z):

Scope: `docs/todo/P02M0136.md`. Nothing was implemented.

## The dependency, and why it is a real one

This milestone's own ordering puts the font package and catalogue FIRST so that the shaping, layout
and rendering work can follow it. Its output is a `GlyphRun` that `render2d` consumes, and `render2d`
is `P02M0103b`. Building the font destination and the catalogue service on their own would deliver a
place to put fonts and a service to list them, with no shaping to use them and no renderer to draw
what shaping produced - the partial layer this job's rules forbid, and the reason the plan orders the
item first rather than shipping it alone.

`P02M0103` is `PHASE-4 FUTURE VISION` and was not started, so the consumer this milestone's pipeline
ends at does not exist.

## What is separable

The two host-testable halves that need no renderer: the CLOSED OpenType profile - what this system
parses and what it refuses - and the normative Unicode conformance harness the plan names, run
against the pinned version's own files. Both are checks rather than pipeline, and both would still be
true when the pipeline arrives. A dedicated run should take them first.

Verification: not applicable - no change was made.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-12T03:30:47Z):

ONE DELIVERABLE OF THE FIRST ITEM, COMPLETE: THE READ-ONLY DIRECTORY MINT.

WHY THIS ONE AND WHY ONLY THIS ONE. The first item is the only unconditional one in the file -
everything else depends on it AND on `P02M0103b`'s frozen `GlyphRun` and font-resource contract, and
the Done gate additionally on `P02M0103c`'s conforming renderer. Neither `P02M0103b` nor
`P02M0103c` exists; `P02M0103` is an `[i]` index row whose parts are activated "by explicit
approval". Within the first item, the read-only mint is the deliverable that is complete in itself:
it is useful to any directory client, it is fully testable, and it does not half-build a service.

WHAT IT IS. `volume-admin.open-directory` gains `writable: bool`, in the shape `open-file` already
carries, and `Scope::Directory` carries the flag. A read-only directory scope is an ALLOWLIST of
`OP_OPEN`, `OP_LIST`, `OP_READ` and `OP_WATCH` - every other opcode is refused, including one added
to `volume` later, which is the direction a default has to fail in.

AND THE ALLOWLIST IS THE POINT, WHICH THE PLAN HAD ALREADY WORKED OUT. Reusing the read-only FILE
rule mints a "read-only" client that can still `mkdir` and `rmdir`: that rule consults `writable` for
file scopes only, and its denial list omits both opcodes, which the op table then admits for any path
inside the scope. I confirmed that reading before writing the rule, and the gate names those two
opcodes individually for exactly that reason.

THE GATE, AND WHAT IT FOUND ABOUT ITSELF. It attempts all nine mutating opcodes individually against
a read-only grant and requires each to be refused; it requires an unclassified opcode to be refused
too; it requires `list` to be admitted, because a scope that refused everything would pass every
other assertion and be useless; and it is watched to fail by minting the same grant `writable: true`,
which accepts `mkdir` and `rmdir`.
ITS FIRST VERSION WAS MEASURING THE WRONG THING. A write-stream attempt without the byte count and
the channel the request carries fails to PARSE - the service answers with a correlation of zero and
the scope check is never reached. It looked like a refusal and was a decoder complaint.

THE OPCODE NUMBERS COME FROM THE TABLE THAT DEFINES THEM. The kernel test crate now takes
`storage-proto` for its constants, which is the same reason it already takes `driver-protocol` and
`driver-binding`: a hand-written copy of a wire table is the copy that drifts.

VERIFICATION PERFORMED:
- `./test.sh --arch x86_64`: 391 passed, including the new gate.
- `./check.sh --gate source-hygiene,artifact-metadata,milestone-index,staged-consistency`: pass.
- `./gen.sh --accept-breaking`: the IDL change is recorded in the ABI ledger as a deliberate
  pre-release break; sixteen packages regenerated.

WHAT REMAINS OF THIS MILESTONE, and it is most of it. Of the first item: the canonical font
destination in `P02M0097`'s layout and the manifest validator, the pinned licensed last-resort face
and conformance corpus under the dependency policy, and the catalogue SERVICE with its protocol, its
two consumer edges (the `Factory` role and a PermissionManager capability minted as a fresh
sub-connection) and their negative gates. Then the closed OpenType profile, the ordered shaping
pipeline with typed intermediates, the `GlyphRun` production, the bounded recursive work, the
normative Unicode conformance host gates, and the guest gate - the last four of which cannot start
until `P02M0103b` freezes the `GlyphRun` and font-resource contract, and the guest gate additionally
needs `P02M0103c`'s conforming renderer.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-12T04:11:31Z):

## What was delivered

The shared font-resource contract, frozen: `src/user/libs/text/font-contract`. No dependencies,
`no_std` except under test, so both sides of the seam can link it and it is testable on the host.

This is the item the milestone calls its first CONTRACT item's partner - "freeze ONE shared
font-resource contract before either side implements against it" - and the reason to do it now
rather than later is the reason the plan gives: it is what the text stack and the 2D renderer have to
agree about, and until it exists every item on either side is implemented against a wording.

  - `face.rs` - a face is a content-derived FILE identity plus a face INDEX, because a collection is
    one file with several faces sharing its identity. The identity is the catalogue's, issued from
    the file, never computed by a client from a buffer the client can write. `Generation` is what
    invalidates: it is carried by everything derived from a face.
  - `face.rs`'s `FaceBytes` is the OWNERSHIP RULE AS A TYPE. Its only constructors take or copy the
    bytes, so a decoded structure cannot borrow from the caller's transport buffer - which stays
    writable, because this kernel has no seal for a memory object and the caller created it. The
    thing that is left undefended is named in the type's own documentation rather than implied: a
    caller can corrupt its OWN parse by writing the buffer before the take.
  - `fixed.rs` - 26.6, with round-half-to-even at the ONE site where a scaled font unit becomes a run
    value, computed on integers so the answer does not depend on the host's floating point. Overflow
    is a typed refusal with two cases - a value outside 26.6, and a sum of admissible values that
    left it - and never a saturation.
  - `glyph.rs` - the six decoded forms enumerated; `SubpixelLayout` and `RasterisationMode`;
    `SubpixelPhase` quantised to quarter pixels; `VariationCoordinates` normalised 2.14 and bounded;
    `TransformKey`, which refuses a non-finite matrix and normalises negative zero so a key that is
    hashed and compared cannot fail to match itself or split one transform into two entries.
  - `run.rs` - `GlyphRun`, face-, script- and direction-homogeneous BY CONSTRUCTION: the face is
    carried once and a run of two faces is unrepresentable. Units fixed as the plan fixes them.
    `cache_key` is built from the run, so a consumer cannot assemble a key that omits a field.
  - `cluster.rs` - the cluster mapping with all four things it must carry, and its invariants
    CHECKED in the constructor: logical order, no overlap, inside the source, a visual order that is
    a permutation, and caret slices inside their storage.
  - `cache.rs` - `GlyphCacheKey`, one normative definition with eleven fields.

## Verification

PERFORMED and passing:

  - `cargo test` in `src/user/libs/text/font-contract` - 11 fixtures, 11 passed, 0 failed, plus the
    doctest. The field-by-field negative case covers file, face index, generation, glyph, size,
    variation coordinates, transform, subpixel phase, kind, strike and palette, and all five
    rasterisation modes pairwise - with the POSITIVE case beside it, because a key that never matched
    itself would satisfy every negative one. Round-half-to-even is asserted at ties in both
    directions and at non-ties; overflow is asserted to refuse rather than saturate, including a run
    whose advance leaves 26.6; the transform key refuses NaN and infinity and folds `-0.0`; the
    variation bound refuses rather than truncates; the ownership rule is shown by writing the
    transport buffer after the copy and finding what the library holds unchanged; the cluster mapping
    refuses each of the four malformed shapes and accepts the valid one; a ligature's caret stops are
    recovered; and a bidi selection of one contiguous range is shown to be two pieces on screen.
  - `system-manifest check` - exit 0 with the new `[[sources]]` row.
  - `./check.sh --gate verify-model` - model consistent, 136 crates; the crate's host suite is
    enumerated automatically and its release-required key recorded.
  - `./check.sh --gate milestone-index`, `check-source-hygiene.sh` - clean.

NOT PERFORMED:

  - nothing in this crate has a consumer yet, so no integration is exercised. That is the point of
    freezing it first, and it is why the plan puts the freeze before either side; it is also the
    honest limit of what these fixtures show.

## What was NOT delivered, and what blocks it

The milestone's first item - the font destination, the catalogue service, its two consumer edges, the
staging tool and `lsfont` - remains open apart from the read-only directory mint delivered earlier.
It is a milestone-sized item on its own: a `role = "service"` program, an LSIDL interface of four
operations plus an admin one, a `PermissionManager` capability and grant arm, a `ServiceManager`
`FONTDIR` role, a staged metadata sidecar with closed vocabularies, five ceilings with an ordered
publish/withdraw rule, and the guest gates for each. None of it is started, and none of it is blocked
by anything missing - it is unbuilt, not blocked.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-12T04:21:18Z):

## What was delivered

The font catalogue's DECISION CORE, in `service-logic`, where a pure decision can be judged without
a boot. Four modules and 28 fixtures, all passing inside that crate's suite of 445.

`font_record` - the staged metadata sidecar and its closed vocabularies.

  The catalogue parses no font: family, style, axes, face index and format come out of OpenType and
  TTC bytes, so anything deriving them would be the second unprofiled parser the milestone's ordering
  exists to prevent. The record is DECLARED, in text, because a person authors it in the review that
  adds the face. What is validated: the vocabulary is closed (four formats, nine widths, three
  slants, weight 1..=1000, an axis tag of four graphic characters with an ORDERED design range), the
  encoded WIRE record fits `MAX_FACE_METADATA_BYTES`, a non-zero face index is only meaningful in a
  collection, and the declared digest is the digest of the bytes beside it.
  What is NOT validated, and the module says so in its own documentation rather than leaving it to be
  inferred: whether the declaration is TRUE. That oracle is owed by the profiled parser.
  `declares_same_identity_as` is the relabelling comparison the withdrawal rule stands on - family,
  style, format, face index and axes, and deliberately NOT the digest, since a replacement always
  changes the bytes.

`font_scan` - the ordered publish/withdraw rule, all three steps.

  Step 1 marks what can no longer be SERVED, by both doors: a rejected replacement and a face that is
  gone. Step 2 publishes the admissible set when it fits all three ceilings. Step 3 publishes the
  previous generation minus that set, adding nothing - and when nothing was withdrawn, the generation
  does not advance. A valid no-change scan publishes nothing either, which is what makes a generation
  mean "something differs" rather than "a scan ran".

`font_clients` - sixteen live connections and sixteen subscriptions, one per client.

`font_rescan` - one scan in flight; every COMPLETED scan re-arms the delay, whatever it found; a
request inside it is refused with `try-again-at`; a failed scan spends no publication allowance.

## Verification

PERFORMED and passing:

  - `cargo test` in `src/user/services/logic` - 445 passed, 0 failed. The 28 font fixtures cover, one
    case each: the ordinary declaration read as what it declares; every closed vocabulary refusing a
    value outside it, and the weight range at 0, 1000 and 1001; a repeated field and each of the
    seven missing ones; a malformed line, an empty value and an unknown key; a digest that is not the
    bytes beside it, a short digest and a non-hex one; the family at its bound and one past it; eight
    axes and a ninth; an axis whose range is out of order, whose tag is short, and which has too few
    or too many numbers; a face index on a non-collection; the relabelling comparison in both
    directions; and the three installation ceilings as arithmetic.
  - The four runtime cases the ceiling rule names, each its own fixture: 65 new faces publish nothing
    and leave the previous generation current, with 64 accepted to show the bound is where it says;
    a NEW record past the metadata ceiling does the same; a REPLACEMENT past it withdraws that face
    at a new generation while every other face carries over; and a removal is withdrawn at a new
    generation even when the scan breaches a ceiling, with nothing new published.
  - The capture case: a relabelled replacement is refused, the face is withdrawn, LIST no longer
    offers it, and the new declaration is published under no name at all.
  - The worst-case reply fixture - sixty-four records at exactly 256 bytes - is strictly larger than
    its records and inside 20480, watched to fail by adding one byte to one record.
  - `./check.sh --gate verify-model` - model consistent. `check-source-hygiene.sh` - clean.

NOT PERFORMED:

  - none of this is reached by a running system yet: there is no catalogue service, so nothing
    exercises these decisions over a channel, against a real directory, or in a guest. They are the
    decisions the service will make, tested where they can be tested at all.

## What was NOT delivered, and it is unbuilt rather than blocked

The canonical destination in the volume layout and the manifest validator's admitted set; the
catalogue as a `role = "service"` program; its LSIDL interface (LIST, RESOLVE-INFO, RESOLVE-INTO,
SUBSCRIBE) and the admin interface beside it; the `FONTDIR` read-only mint in ServiceManager; the
`font-catalogue` capability and its fresh-sub-connection grant in PermissionManager; the `FONTADMIN`
route; the licensed last-resort face and the conformance corpus; the staging tool; `lsfont`; and
every guest gate. The read-only directory mint they all stand on was delivered earlier in this
milestone.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-12T04:28:13Z):

## What was delivered

The catalogue's PROTOCOL, frozen: `src/idl/font.lsidl`, generated into `font-proto` and re-exported
by the aggregate `proto` crate like every other package.

`font-catalogue` has the four operations the plan allows and no more - LIST, RESOLVE-INFO,
RESOLVE-INTO, SUBSCRIBE - and `font-catalogue-admin` carries RESCAN on a second interface, because
authority to READ a font must not become authority to make the machine work.

  - RESOLVE IS TWO OPERATIONS, which is the shape this kernel can express: a memory object is charged
    to the Domain that CREATED it and stays charged after the handle moves, so the catalogue cannot
    be the one to create it, and the caller must be told the size before it can. INFO allocates
    nothing; INTO takes the caller's object.
  - THE HANDLE CARRIES `@rights(map, write)`, which the generated dispatch enforces before the
    service is called, and the resource is `@kernel(memory-object)`, so a handle of any other kernel
    object is refused rather than checked for rights it happens to have. The schema's own
    documentation says plainly that `transfer` is present and cannot be otherwise - the typed
    transport consumes request handles that way - and that the catalogue not passing the object on is
    a code obligation with a gate rather than a right it lacks.
  - THE REFUSALS ARE TYPED AND CARRY THE RETRY: `stale` carries the generation that is current now,
    `too-small` the length required. The schema documents that `stale` covers both the generation
    moving and the bytes not matching the identity they would be served under, which is the case the
    generation alone cannot reach because a watch hint can be dropped.
  - THE BOUNDS ARE IN THE SCHEMA: `family` 64 bytes, `style` 32, eight axes, a 32-byte digest, and a
    LIST reply of at most 64 records - the last enforced in the generated decoder, so an
    over-long reply is refused at the boundary rather than accepted and counted later.

## Verification

PERFORMED and passing:

  - `./gen.sh` then `./gen.sh --check` - 16 packages, the aggregate and both graphics profiles
    regenerate to what is on disk; no drift. The generated bound appears in the decoder as
    `(v28 <= 64).then_some(v28)?`.
  - `cargo build` of `font-proto` and of the aggregate `proto` crate - both clean under
    `warnings = "deny"`.
  - `./check.sh --gate declared-interfaces` - 31 interfaces defined, every declared role reference
    resolves. `--gate verify-model` - model consistent, 627 checks; the new package's key recorded in
    `release-required.toml`. `--gate dependency-policy` and `--gate build-order` - passed.
    `check-source-hygiene.sh` - clean. `system-manifest check` - exit 0 with the new source row.

NOT PERFORMED:

  - nothing implements or calls this interface yet, so no request has ever crossed it. The schema is
    checked; the service is not written.

## A note on what registration this package has

`font-proto` carries a `[[sources]]` row and deliberately NO `[[libraries]]` row: it is generated and
checked, and staged nowhere, because the service it describes does not exist. The library row and its
provider edges arrive with the first consumer, which is the catalogue itself.

## Addendum to the protocol section (2026-09-12T04:37:26Z)

The `@bound(64)` on the LIST reply was refused by the ABI check as a breaking change and accepted
with `./gen.sh --accept-breaking`, which is what that flag is for on a pre-release schema. The
manifest now records `... :bound=64`, and `./gen.sh --check` reports no drift. Worth recording
because it is the mechanism working rather than a nuisance: the bound IS part of the recorded ABI, so
raising or removing it later is a reviewed change rather than an edit nobody sees.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-12T05:59:28Z):

## What was delivered

The font catalogue as a RUNNING SERVICE, with both consumer edges wired and the operator tool that
owns the one privileged operation. This is the first item's remaining body, apart from the staged
face and the guest gates.

`src/user/services/core/src/font_catalogue.rs` - a `role = "service"` program.

  - Its authority is ONE READ-ONLY DIRECTORY. It holds no StorageService root and scans nothing else.
  - It parses no font. It reads a declaration staged beside each face, digests the face, and calls
    the decision core already built in `service-logic` - which is why this program is the plumbing
    and not the policy.
  - RESOLVE IS TWO OPERATIONS because the caller must own the memory. `resolve-into` maps the
    caller's object, copies, unmaps and CLOSES the handle on every path, including every refusal:
    not retaining it is a code obligation rather than a right the transport could withhold, and it is
    written as one function so there is no path that forgets.
  - THE BYTES ARE CHECKED AGAINST THE IDENTITY THEY WOULD BE SERVED UNDER, not only against the
    generation. The watch is a hint and a hint can be dropped, so a read admitted under a
    still-current generation could otherwise return a replacement's bytes under the previous
    identity. A mismatch is the same typed staleness refusal and triggers an immediate rescan.
  - SUBSCRIBE is intercepted before the generated dispatch, because a live stream's producer end has
    to outlive the reply - the shape the device catalogue already uses. The current generation is
    queued before the consumer end is released, so a subscriber is never left guessing.
  - THE ADMIN INTERFACE IS THE SAME PROGRAM ON A CHANNEL IT RECOGNISES. The `ADMIN` serve root is
    seeded into the serve set, which is what makes its channel value knowable; an ordinary
    connection is minted from the other root and can never carry it. `rescan` refuses anything else.

`ServiceManager` - `open_storage_directory_read_only`, and the `FONTDIR` case beside the
`config_service` one. The path is `font_record::FONT_DIRECTORY` rather than a second copy: a role
carries no path, and two copies of one path is a mint that succeeds over a directory nobody reads.

`PermissionManager` - two capabilities, held apart for the reason `device-policy` is held apart from
`device`. `font-catalogue` is MINTED FRESH per grant, joining `config`, `device` and `network`,
because a duplicate shares a reply queue and two applications would answer each other's calls.
`font-admin` is the admin ROOT itself, because the catalogue recognises an operator by the channel,
and a sub-connection would arrive on one it does not know - the shape `POLICYOWNER` already has.

`src/user/apps/tools/src/lsfont.rs` and the `font-client` / `font-client-provider` pair - the
operator tool, and the concrete clients a dynamic consumer links. The pair exists because the
shared-image inventory REFUSED the direct route: instantiating the generic transport in a tool
monomorphises the whole codec into it, which is a shared image in name only. The gate named the
residual symbol; the fix is the convention every other interface already follows.

`system-manifest` - the canonical font destination admitted, by SHAPE: a face (`.ttf`, `.otf`,
`.ttc`) or the declaration for one (`sans.ttf.face`), directly under `share/fonts` and nowhere
deeper.

## Verification

PERFORMED and passing:

  - `./build.sh` for x86_64: the service, the tool, the two client crates and every consumer compile
    and link into the shared image; `build-shared` reports `status=0`.
  - `system-manifest check` - exit 0, with the new program, service, three roles, two library rows,
    three source rows and the `permission_manager` dependency and roles.
  - `cargo test` in `system-manifest` - 19 passed, including the new destination fixture: four
    admitted names and four refused ones. IT FOUND A REAL HOLE: `share/fonts/.face` passed the first
    rule, because a name that is only the suffix still ends with it. The rule now requires the
    declaration to name a face - `sans.ttf.face` - which also makes the sidecar convention checkable
    rather than a habit.
  - `./gen.sh --check` - no drift; the two new capabilities and the font package regenerate to what
    is on disk. The capability addition was refused by the ABI check as breaking and accepted with
    `--accept-breaking`, which is what that flag is for on a pre-release schema.
  - The grant-vocabulary assertion in `permission_manager` - a compile-time check that the array IS
    the enum - held both new capabilities to being listed, which is exactly how it is supposed to
    behave: the build failed until they were.

NOT PERFORMED:

  - no guest has booted this service. Nothing here has been exercised at run time: not the mint, not
    a list, not a resolve, not a subscription, not the operator's scan.
  - the three-architecture build and the dynamic-report refresh were still running when this entry
    was written; the x86_64 half is what is reported above.

## What is still missing from this item, and one of it needs a decision

  - THE STAGED FACE. The catalogue starts with an empty publication - the scope mint succeeds over a
    directory that does not exist yet, so this is not a boot hazard - but nothing can be listed or
    resolved until a face is staged. The pinned last-resort face is a LICENSED THIRD-PARTY BINARY
    entering the tree under the dependency policy, with an upstream revision recorded, and that is a
    reviewed import rather than something to do unilaterally. It is the one thing here that is
    blocked on a decision rather than on work.
  - THE GUEST GATES: both consumer routes exercised, two applications holding the capability at once
    with interleaved calls, the client and subscriber ceilings at their bound and one past, a
    replacement invalidating what was derived, and the negative case of an application without the
    capability. Every one of them needs a staged face to be meaningful.

## Addendum: the verification the entry above said was still running (2026-09-12T06:21:59Z)

  - `./build.sh --arch all` - x86_64, aarch64 and riscv64 all build the service, the tool and both
    client crates into the shared image.
  - `./check.sh --refresh dynamic-report` then the gate - `lsfont` is inventoried on all three
    architectures, its font imports resolving to `font-client` and NO generic transport residual.
    The residual is what the first attempt had, and the gate naming the symbol is what sent the tool
    through the client pair every other interface uses.
  - `./check.sh --gate verify-model` - model consistent: 139 crates, 287 components, 1188 edges.
    `--gate declared-interfaces`, `--gate graphics-profile`, `--gate dependency-policy`,
    `--gate grant-vocabulary`, `--gate milestone-index` - all passed. `./gen.sh --check` - no drift.
  - Host suites: `service-logic` 446 passed, `font-contract` 11, `graphics-profile` 6,
    `system-manifest` 19. `check-source-hygiene.sh` clean.

Still NOT PERFORMED: no guest has booted any of this. The service has never served a request.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-12T13:54:09Z):

## What was delivered

`OpenType Profile 1`, published and closed: `src/user/libs/text/opentype-profile`, with
`docs/gen/opentype/profile-1.md` generated from it and hashed.

This item is the START GATE for the parser that reads untrusted font bytes, so publishing it before
the parser is the whole point: a profile written afterwards describes what was built rather than
stating what was required.

  - 29 TABLES, each with the major versions it is admitted at and one line saying why it is in the
    profile. A table with no version field of its own admits any, and says so, rather than carrying a
    check that always passes while looking like one that does something.
  - BOTH HALVES OF "CORRECT AT ANY COORDINATE", which is the sentence this item was written about.
    The outline half is the Type 2 and CFF2 charstring interpreters with subroutines and `blend`; the
    metric half is `HVAR`, `VVAR`, `MVAR`, the item variation store and the delta-set index map.
    A font instanced without the second has correct outlines at the wrong advances - text that is
    subtly mis-spaced at every non-default coordinate.
  - ALL EIGHT `GSUB` AND ALL NINE `GPOS` LOOKUP TYPES with their subtable formats, the six lookup
    flags including the two that are a class rather than a bit, and the shared structure formats.
    `cmap` format 14 is in it: without Unicode variation sequences a document asking for one form of
    a character gets the default one.
  - THE CONDITIONAL MECHANISMS, which a table list forgets: `FeatureVariations` decides WHICH
    lookups apply at a coordinate, and `Device`/`VariationIndex` are how a `GPOS` value varies. With
    the variation tables and neither of these, positioning is right at the default instance only.
  - THE `COLR` v1 PAINT GRAPH enumerated by paint, with the 28 composite modes - the same set the 2D
    profile carries, deliberately, because a profile naming a mode the renderer does not have is a
    promise nothing can keep.
  - 32 SCRIPTS, EACH NAMED, with the shaping class it needs. "Indic" is not an entry: Devanagari and
    Malayalam are both "Indic" and their reordering differs, so a category would make a missing
    script and a wrong one look the same from outside.
  - 19 LANGUAGES, each with what selecting it CHANGES. An unknown `LangSys` falls back to `dflt`
    rather than refusing - the one fallback in the profile, stated so the difference is deliberate.
  - THE EXCLUSIONS, with their reasons and the tags each refuses.

## Verification

PERFORMED and passing:

  - `cargo test` in `opentype-profile` - 9 fixtures, 9 passed.
  - `./check.sh --gate opentype-profile` - the fixtures, then the generated document and its hash.
  - `./gen.sh --check` - no drift; every profile regenerates to what is on disk.
  - `./check.sh --gate verify-model` - model consistent, 629 checks, the new gate in the catalog and
    its key in `release-required.toml`. `--gate milestone-index` and `check-source-hygiene.sh` clean.
  - `system-manifest check` - exit 0 with the new source row.

WHAT THE FIXTURES FOUND, which is the part worth recording: three defects in the LIST, before any
gate existed to run over it.

  - A paint rule that admitted `format + 1` for every paint admitted a format 33 above
    `PaintComposite` at 32. Not every paint has a variable counterpart - a layer list carries no
    numbers of its own to vary - so `varies` is now a field of the entry rather than an assumption.
  - The "nothing is both supported and excluded" check scraped a tag out of the exclusion's prose,
    and read "`avar` version 2" as excluding `avar`, which IS supported. An exclusion now names the
    tags it refuses in a field, and a narrow exclusion names none - which is what makes the check
    exact instead of a heuristic that would also have missed a real contradiction.
  - A language entry said "Sindhi letter forms", which does not say what selecting it changes. The
    rule that every language must state its effect is what caught it.

In each case the model was corrected and the test left alone, which is the direction that matters.

NOT PERFORMED:

  - nothing reads this profile yet. The parser it bounds is the next item and is not written; until
    it is, what is checked is that the list is closed, consistent and unchanged - not that anything
    obeys it.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-12T14:15:36Z):

## What was delivered

Unicode segmentation at 17.0.0: the tables, the three algorithms over them, and the normative
conformance run that measures them.

`toolchain.lock` gains sixteen `[ucd_*]` blocks - each with the upstream URL, the version and the
SHA-256 of the file as fetched. `./bootstrap.sh` fetches them once, deliberately, verifying the
digest BEFORE the file is where anything reads it; every tool and gate below reads the cache and
never the network, which is the tree's existing rule for pinned external artifacts.

`src/tools/ucd-gen` writes the whole of `unicode-tables/src/generated.rs` - nine property tables with
their enums and `Extended_Pictographic`. Two decisions worth naming:

  - THE ENUMS ARE GENERATED BESIDE THE TABLES. A hand-written enum indexing a generated table is two
    lists that must agree with nothing checking that they do, and the day they stop agreeing every
    character in one range quietly takes another category's rules.
  - IT REFUSES TO DEFAULT A VALUE IT DOES NOT KNOW. A property value the release has and the
    generator's list does not is an error, not an `Other`. That is how `HH` - the unambiguous hyphen
    class this release added - was found rather than filed under `XX` and never breaking.

`unicode-tables` also carries the classes SHAPING needs, which a segmentation-only table set misses:
`Joining_Type`, `Indic_Syllabic_Category`, `Indic_Positional_Category`, `Script`, `General_Category`
and `Indic_Conjunct_Break`.

`unicode-segmentation` implements UAX #29 grapheme cluster and word boundaries and UAX #14 line
break opportunities, as the documents' numbered rules in the documents' order, with each rule named
at the line that implements it. No tailoring.

`src/tools/unicode-conformance` runs the three normative files IN FULL and prints the code points and
both boundary sets for a failure, because "1273 of 9836 passed" is a number nobody can act on.

## Verification

PERFORMED and passing:

  - THE NORMATIVE CONFORMANCE FILES, every case: `GraphemeBreakTest` 766 of 766, `WordBreakTest` 1944
    of 1944, `LineBreakTest` 19338 of 19338.
  - `cargo test` in `unicode-tables` - 4 fixtures: every table sorted, disjoint, non-empty and
    carrying no run of the default value; spot values a person can check by hand; the UCD spelling
    recoverable from each variant; and the binary search at its edges.
  - `cargo test` in `unicode-segmentation` - 6 fixtures, written so a reader can see what each
    algorithm is FOR without opening a conformance file.
  - `ucd-gen --check` - the tables regenerate to what is on disk. The generator formats its own
    output, so generating and checking answer the same thing after `./format.sh` has run.
  - `./check.sh --gate unicode-segmentation`, `--gate verify-model` (model consistent),
    `--gate dependency-policy`, `check-source-hygiene.sh`, `system-manifest check` - all pass.

WHAT THE CONFORMANCE RUN CORRECTED, which is the part worth recording, because none of it would have
been found by a test written by the same person who wrote the code:

  - WORD BOUNDARIES: the rules that ignore combining marks are numbered AFTER the ones about spaces
    and joiners, and I had applied the filtering first. A space, a combining mark and a space came
    out as one word instead of two. 9 cases.
  - LINE BREAKING: 1077 of 19338 failed against my first implementation, which was written from the
    shape of the rules rather than from their exact 17.0 text. Rewriting them from the numbered rule
    set - including the six sub-rules of LB19, the fifteen of LB25 and the east-asian width LB30
    turns on - took it to 22.
  - THE LAST 22 were a zero-width joiner at the start of a text: the rule prohibiting a break after
    one is numbered before the rule that turns an unattached mark into a letter, and my resolution
    order lost it. The fix also had to distinguish "this run contained a joiner" from "this run ENDS
    with one", which a joiner followed by a combining mark is the only case to show.

NOT PERFORMED:

  - nothing consumes these yet. The shaping stack that will read the joining types and the Indic
    categories is the next items and is not written; what is checked here is that the boundaries are
    Unicode's, not that anything downstream uses them.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-12T14:25:56Z):

## What was delivered

The Unicode bidirectional algorithm, whole: `src/user/libs/text/unicode-bidi`.

Every rule of UAX #9 is implemented and named at the code that implements it - P2/P3, X1 to X10,
BD9's isolate matching, BD13's isolating run sequences, W1 to W7, N0 with BD16's bracket stack, N1
and N2, I1 and I2, L1, L2 and L4. Three decisions worth recording:

  - THE WEAK AND NEUTRAL RULES RUN PER ISOLATING RUN SEQUENCE, not over the paragraph. That is the
    part an implementation written from the rule names alone gets wrong: over the paragraph it gives
    the right answer for text with no isolates and the wrong one for text with any.
  - THE BRACKET RULE IS THE ONLY ONE THAT NEEDS THE CHARACTERS, so the algorithm takes them when it
    has them and works from classes alone when it does not - which is what lets the class-sequence
    conformance file drive it at all.
  - MIRRORING IS ANSWERED, NOT APPLIED. Which glyph is drawn is the font's decision; a face may carry
    its own mirrored form through `rtlm`, and substituting here would take that away.

`toolchain.lock` gains four more pinned files: `DerivedBidiClass.txt`, `BidiBrackets.txt`,
`BidiTest.txt` and `BidiCharacterTest.txt`.

THE BIDI CLASS NEEDED A TABLE SHAPE THE OTHERS DID NOT. Its default is per BLOCK rather than once -
an unassigned code point in the Hebrew or Arabic blocks is `R` or `AL`, stated in the file's
`@missing` lines rather than in its records - so the generator now reads those lines, keeps the runs
whose value is the default for this property alone, and the lookup distinguishes "not mentioned" from
"mentioned as the default". Without that, an unassigned code point in the middle of Arabic text would
have been laid out left to right.

## Verification

PERFORMED and passing:

  - `BidiTest.txt` - 770241 cases, all of them. `BidiCharacterTest.txt` - 91707 cases, all of them.
    Both at every paragraph direction the file states, with rule L1 applied as the file assumes.
  - `cargo test` in `unicode-bidi` - 8 fixtures.
  - `./check.sh --gate unicode-segmentation` now covers five normative files and three crates;
    `--gate verify-model` consistent; `system-manifest check`, `check-source-hygiene.sh` clean.
  - The generated tables still regenerate to what is on disk after `./format.sh`.

NOT PERFORMED:

  - nothing consumes the levels yet. The shaping stack and line layout that will read them are later
    items; what is checked is that the ORDER is Unicode's, not that anything draws it.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-12T14:38:01Z):

## What was delivered

`src/user/libs/text/font-parse`: a font read as HOSTILE INPUT. The item is NOT ticked - what it asks
for is larger than this - and the plan now records exactly which half is built.

  - No `unsafe`, no allocation, no panic path. One bounded reader that every byte goes through, so
    there is one thing to get right rather than one per read site; `checked_add` on every offset,
    because an offset plus a length that wraps is precisely the shape of a crafted font.
  - Two kinds of refusal kept apart. `Malformed` names the table and what contradicted itself;
    `Unsupported` carries what was outside `OpenType Profile 1`. A broken font and a font for another
    system are different answers, and the profile check happens at the ONE place a table is opened
    rather than at each place one is read - where it would be missing from the table nobody thought
    about.
  - Read today: the table directory including collections, `head`, `hhea`, `maxp`, `hmtx` with the
    compressed side-bearing tail, `loca` in both forms, `glyf` simple and composite, and `cmap`
    formats 4, 6 and 12.

THE OUTLINE WALK IS CONSTANT-STACK, and the first version was not. Decoding the run-length compressed
flags into an array and then reading the two coordinate arrays is about 190 kB of stack at the
format's maximum point count, in a function that RECURSES for composite glyphs - a stack overflow a
crafted font can ask for, which is the failure this item exists to refuse. It now walks the flag
stream twice and advances three readers in step.

## Verification

PERFORMED and passing: `cargo test` in `font-parse` - 6 fixtures.

  - A font THIS TREE BUILT is read: metrics, advances, a `cmap` lookup, a simple outline, a composite
    resolved through its component, and the empty glyph that is not an error.
  - EVERY TRUNCATION of that font, and EVERY byte of it flipped four ways, through every path the
    parser has. Exhaustive rather than random: a few hundred bytes is cheaper to cover completely
    than to argue about. What is asserted is that it ANSWERS - a test demanding a particular answer
    from a mutated font would be worthless.
  - The crafted shapes named one at a time: a directory entry pointing past the file, a file that is
    not a font, a face index a single-face file does not have, a design grid of zero, a `loca` format
    the field has no third value for, more horizontal metrics than glyphs, a table the profile
    excludes, and a composite glyph that refers to itself.
  - `./check.sh --gate opentype-profile` now runs the parser's fixtures beside the profile's, so the
    two cannot drift; `--gate verify-model` consistent; `system-manifest check` and
    `check-source-hygiene.sh` clean.

WHAT THE FIXTURES FOUND, both in the parser rather than in the tests:

  - `unitsPerEm` was read at the wrong offset and came back as the FLAGS field - which is zero, so
    every font would have been refused as having a design grid of zero.
  - `numberOfHMetrics` was read two bytes early, at the metric data format. A real font's format is
    0, so every font would have declared no horizontal metrics and been refused.

Both would have made the parser reject every real font it was ever given, and neither is visible
from reading the code - only from a font laid out the way the specification actually lays one out.

## What is NOT delivered, and the one thing that is blocked

  - `CFF`/`CFF2` charstrings, `GSUB`/`GPOS`, `GDEF`, `COLR`/`CPAL`, the bitmap strikes, the variation
    tables, `name`, `OS/2` and `post`. Unbuilt, not blocked.
  - THE TRUTH ORACLE, which this item owns: parse every STAGED face and require what the parser
    recovers to equal the declared record. It cannot be built until a face is staged, and that waits
    on the licensed last-resort face - the one decision in this milestone that is not mine to make.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-12T14:47:56Z):

## What was delivered

`src/user/libs/text/font-shape`: the shaping machinery over `GSUB` and `GPOS`. The item is NOT
ticked, and its own last sentence is the reason - "Latin alone is the case that makes a design look
finished and proves nothing". What is built is the machinery and the part of it Latin needs.

  - Script and language system selection, with the three-deep fallback the format defines. A shaper
    that stopped at the first miss renders a Turkish font with no Turkish forms and says nothing.
  - The feature list including the REQUIRED feature, which is applied whatever the caller asked for,
    and the ordinary features which are applied only when asked - because which of those are on is
    the caller's policy, and a shaper that applied everything gives a caller no way to turn one off.
  - `Coverage` formats 1 and 2, `ClassDef` formats 1 and 2, the lookup list, and the lookup flags -
    which are not decoration: a lookup that says it ignores marks and is applied to them anyway
    attaches an accent to an accent.
  - `GSUB` 1, 2, 3, 4 and 7 (extension); `GPOS` 1, 2, 4, 6 and 9.
  - A buffer every rule rewrites IN PLACE, where each glyph carries the cluster it came from - a
    ligature is one glyph for three characters and a decomposition three for one, and the cluster is
    what a caret and a hit test are later built from.

LOOKUPS RUN IN LOOKUP-LIST ORDER. Several features name the same lookup and the format says the
lookup list decides; applying them feature by feature changes the result for any font whose features
overlap, which is most of them.

EVERYTHING OUTSIDE THAT IS REFUSED BY NAME rather than skipped. A lookup type quietly skipped renders
text the font asked to change - which is worse than a refusal because nobody sees it.

## Verification

PERFORMED and passing: `cargo test` in `font-shape` - 6 fixtures, over fonts built here carrying
exactly one ligature, one kerning pair and one mark attachment, which no real font does and which is
what makes an assertion about the result readable.

  - A ligature forms, keeps its first component's cluster and takes its OWN advance.
  - A feature the caller did not ask for does not run.
  - A kerning pair ADJUSTS an advance rather than setting one, and a pair the font does not carry
    leaves both glyphs alone.
  - A mark attaches at the anchors the font gives, takes no advance of its own, and records what it
    attached to.
  - A font with no layout tables shapes to its own advances rather than being refused - most fonts in
    the world have neither table.
  - The buffer's own operations keep clusters through decomposition and ligation, and a ligature past
    the end of the buffer changes nothing rather than reading past it.

WHAT THE FIXTURES CORRECTED, both ordering defects invisible from the code:

  - The advances were read from `hmtx` BEFORE substitution ran, so a ligature came out carrying its
    first component's width. They are now read after, along with the `GDEF` classes, because both are
    facts about glyphs the substitution may have replaced.
  - A mark was moved back by the advances after its base rather than including the base's own. On any
    wide base that puts the accent over the NEXT letter.

## What is NOT delivered

Contextual and chaining substitution and positioning (`GSUB` 5, 6, 8 and `GPOS` 7, 8), cursive
attachment (`GPOS` 3), mark-to-ligature (`GPOS` 5), and every per-script shaper the profile names:
cursive joining, Indic reordering, Khmer, Myanmar and Hangul. The properties those shapers are
written in terms of are already generated and tested - joining type, syllabic and positional
category, script - so what is missing is the shapers themselves rather than anything they need.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-12T14:59:05Z):

## What was delivered

The rest of the layout engine, and the first two script shapers.

EVERY LOOKUP TYPE THE PROFILE NAMES is now applied: `GSUB` 1 to 8 and `GPOS` 1 to 9.

  - The contextual and chaining types are ONE implementation for all four, because that is what they
    are: one structure with two payloads, differing only in which table the nested lookups come from.
    THE BACKTRACK IS READ NEAREST-FIRST - entry 0 is the glyph immediately before the match - which
    is the single thing implementations of this get wrong; read forwards it matches the mirror of
    what the font asked for and fires almost at random.
  - Reverse chaining substitution is applied RIGHT TO LEFT, which is what "reverse" means: a
    substitution made at one position must be visible to the rule applied before it.
  - Cursive attachment (`GPOS` 3), which is what joins Arabic letters into a written line rather than
    a row of shapes that touch, and mark-to-ligature (`GPOS` 5), where the component a mark belongs
    to is found from its cluster - putting it on the first component is an accent under the wrong
    half of a ligature.
  - Extension lookups are unwrapped once, in the driver, and an extension that extends an extension
    is refused as a font pointing at itself.

THE PER-GLYPH FEATURE MASK, which is what makes a script shaper possible at all. `fina` selects the
final form of a letter and must apply to the last letter of a word and to no other; a shaper that
could only turn features on globally would render a word made entirely of endings. A lookup named by
two features takes both masks rather than being applied twice.

THE CURSIVE SHAPER: the joining form each letter takes from what it can reach, with marks TRANSPARENT
- the rule a shaper looking at its immediate neighbour gets wrong, and in Arabic a vowel mark sits
between letters constantly, so that shaper breaks the join almost everywhere.

THE INDIC SHAPER'S FIRST HALF: syllable identification over the generated
`Indic_Syllabic_Category`, and the pre-base vowel reordering. A vowel sign written after its
consonant is DRAWN before it; that is the writing system rather than a font feature, and a shaper
that leaves the order alone renders Devanagari with every one of them on the wrong side.

## Verification

PERFORMED and passing: `cargo test` in `font-shape` - 10 fixtures, and the two gates.

  - The cursive forms for eight words, including the two cases that matter: a letter that joins only
    backwards does not let the next one reach it, and a MARK between two letters is transparent.
  - The masks: the first letter carries `init` and not `fina`, the middle `medi`, the last `fina` -
    and a font's `fina` lookup applied under those masks reaches the last letter ALONE.
  - The shaper chosen from the characters rather than from what the caller declared.
  - Indic: one syllable for a consonant and its vowel sign, the pre-base sign moved in front, a
    post-base sign left alone, two syllables reordering independently, and a conjunct - consonant,
    virama, consonant - recognised as ONE syllable.

WHAT THE FIXTURES FOUND, and it was in the generated tables rather than in the shaper:
`ArabicShaping.txt` states in its HEADER, not in its records, that an unlisted code point is
`Joining_Type=T` when its general category is `Mn`, `Me` or `Cf`. Without that derived default every
Arabic vowel mark was non-joining rather than transparent - so a mark between two letters broke the
join, which in Arabic is most places. The generator now applies it, and the tables crate has a
fixture for it. A second assertion in that fixture was mine and wrong: ZERO WIDTH SPACE is a FORMAT
character despite its name, so it is transparent too.

## What is NOT delivered

The Khmer, Myanmar and Hangul shapers, and the rest of the Indic model - reph movement, the half and
below forms, and the feature ordering those need. The item stays open.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-12T15:04:17Z):

## What was delivered

The remaining script shapers, and the entry point that makes the whole item true. The shaping item is
now TICKED.

`shape_run` takes CHARACTERS and a buffer of the glyphs they mapped to, picks the shaper the script
needs, runs its pass before any lookup, and applies that shaper's features. Everything below it takes
glyphs and feature tags - which is enough for Latin and enough for nothing else, because the joining
forms, the reordering and the jamo composition all need the characters.

  - HANGUL COMPOSES. Almost every font carries the precomposed syllables and not the jamo arranged
    into them; the composition is arithmetic rather than a table, which is exactly why it is a shaper
    and not a feature. A lead jamo with nothing to compose with is left alone rather than dropped.
  - MYANMAR reorders by CATEGORY: the medial `ra` first, then the pre-base vowel.
  - KHMER shares the Indic cluster model, because its coeng IS an `Invisible_Stacker` and the
    generated syllabic categories already say so - which is worth stating, since sharing an
    implementation for the wrong reason is how two scripts come to be rendered by one engine badly.
  - THE UNIVERSAL shaper is a cluster model WITHOUT reordering for Thai, Lao and Tibetan, and saying
    that plainly is better than leaving a reader to wonder which reordering it forgot.
  - `script_tag` answers the tag the FONT indexes its features by, which is not the Unicode script
    name: OpenType has `deva` and `dev2` for Devanagari and a font written for one carries no
    features under the other.

## Verification

PERFORMED and passing: `cargo test` in `font-shape` - 15 fixtures.

  - EVERY SHAPING CLASS THE PROFILE NAMES has a shaper, checked against
    `opentype_profile::scripts::SCRIPTS` itself rather than against a list written beside it. A class
    added to the profile without a shaper here fails this fixture rather than rendering a script with
    the wrong engine.
  - Hangul: three jamo to one syllable, two jamo with no trailing consonant, a lead with nothing to
    compose with left alone, and an already-composed syllable passing through.
  - Myanmar: the medial and the pre-base vowel both moved in front, in that order, and a consonant
    with neither left where it is.
  - `shape_run` end to end on an Arabic run (masks from the joining forms, so `fina` reaches the last
    letter alone), a Hangul run (the characters themselves change, which is why the call answers
    them), a Devanagari run (reordered before any lookup sees it) and a Latin run (untouched by any
    of it - the case that proves nothing on its own).
  - `./check.sh --gate opentype-profile` and `--gate unicode-segmentation` pass; hygiene clean.

WHAT A FIXTURE FOUND, in the syllable model rather than in the new shapers: a medial or subjoined
consonant is part of the cluster it follows ALWAYS - that is what makes it medial - and requiring a
virama before it, as a full consonant needs, split a Myanmar cluster at its medial `ra`. The medial
then never reached the front of its cluster, which is the whole of what the Myanmar shaper does.

## What this item does NOT claim

The shapers are the models, not a conformance run: there is no normative test file for OpenType
shaping the way there is for segmentation and bidi, and the honest verification is a real font
rendered and compared - which is what this milestone's guest gate is for and what the staged face is
still waiting on.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-12T15:12:33Z):

## What was delivered

`src/user/libs/text/text-pipeline`: the ordered pipeline, as TYPES, and the canonical-equivalence
policy it is built around. The item is ticked.

THE ORDER IS ENFORCED BY THE TYPE STRUCTURE rather than by a comment. `Source`, `Items`, `Levelled`,
`Faced`, `Shaped`, `Measured`, `Lines`, `Visual` - each is produced only by the stage before it, so
the one mistake the item names cannot be written here: bidi reordering is applied PER LINE, after
the breaks are known, and `Visual` is reachable from `Lines` and from nowhere else. A paragraph
reordered once and then wrapped is wrong wherever it wraps, and the error is invisible until a line
happens to break inside a right-to-left run.

THE CANONICAL-EQUIVALENCE POLICY IS PRESERVATION, in all three of its parts:

  - The buffer is NEVER rewritten. Every offset reported is into the caller's own UTF-8. Normalising
    and then reporting offsets into the normalised copy is how a text engine returns caret positions
    that do not exist in the caller's string.
  - Coverage is asked in a CANONICAL VIEW, before fallback rather than inside shaping: a face covers
    a cluster when it covers the composition OR the full decomposition. Atomicity alone was not
    enough and the previous version of the plan stopped there - a face may have precomposed U+00E9
    and no combining acute, or the reverse, and fallback runs before shaping could resolve anything.
  - The canonical ORDERING of marks is part of the view. Two marks written in either order are one
    string; a view that did not sort them would call two identical clusters different.

THE DECOMPOSITION DATA IS GENERATED from `UnicodeData.txt` and `CompositionExclusions.txt`, pinned by
SHA-256 like the rest of the UCD. Two decisions in the generator worth recording: a COMPATIBILITY
decomposition is dropped, because `<font> 0066` says a character looks like an `f` rather than being
one; and the composition exclusions are removed from the composite table, because a canonical view
built without them invents spellings Unicode says do not exist and then asks a font to cover them.

## Verification

PERFORMED and passing: `cargo test` in `text-pipeline` - 6 fixtures - and in `unicode-tables` for the
new lookups, both inside `./check.sh --gate unicode-segmentation`.

  - The canonical view of a cluster is the same for both its spellings, and for two marks written in
    either order.
  - THE REGRESSION THE ITEM ASKS FOR, in both cases it names: a font set with precomposed Latin in
    one face and the marks in another gives `café` the same face whichever way it is spelled, and
    with the precomposed face absent both spellings reach the mark face - again the same decision.
    The non-Latin case is Devanagari KA WITH NUKTA, which is also a composition EXCLUSION, so its
    canonical view's composed form IS the decomposed one.
  - The offsets are the caller's: four clusters either way, with spans differing only as the two
    inputs' byte lengths do.
  - Itemisation does not split a cluster, and a character that decides no script joins the run it is
    in rather than starting one.
  - The visual order is produced PER LINE, after wrapping - two lines from a mandatory break, two
    orders, each computed on its own line's levels with rule L1 applied to that line.

## What this item does NOT claim

The shaping stage is a type here rather than an implementation: what fills a `ShapedRun` is
`font-shape`, and what chooses among real faces is the catalogue. The pipeline owns the ORDER and the
canonical-equivalence question; the stages it sequences are elsewhere and two of them - real face
fallback and boundary-sensitive reshaping - have nothing to run against until a face is staged.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-12T15:16:55Z):

## What was delivered

Font fallback as a POLICY: `text-pipeline`'s `fallback` module. The item is ticked.

  - THREE BANDS, STATED: faces preferred for this script AND language, then for the script whatever
    the language, then the general ones, and the last resort LAST - in the list rather than as a
    special case afterwards, so a caller that walks the order and finds nothing has been told
    everything the policy knows.
  - THE ORDER IS TOTAL AND INDEPENDENT OF ENUMERATION. A stated rank decides within a band and the
    FACE ID breaks every tie - the catalogue's content-derived identity, not a position. That is the
    whole point of the item: fallback that depends on directory order is a rendering difference
    between two machines, and neither machine can see it because both are "just using the system
    font".
  - THE LANGUAGE BAND IS LOAD-BEARING. One Han face is chosen for Japanese and another for Chinese
    because the same character is drawn differently in the two.
  - WHOLE CLUSTERS, asked in the canonical view: a face that covers only part of a cluster does not
    cover it, so a base letter and its mark cannot go to two different faces. A run ends where the
    face changes, which is what makes the shaped runs face-homogeneous as the shared contract
    requires.

## Verification

PERFORMED and passing: `cargo test` in `text-pipeline` - 9 fixtures - inside
`./check.sh --gate unicode-segmentation`.

  - The three bands in order, for a Japanese Han run and a Chinese one, and for a script nothing is
    preferred for.
  - THE SAME FACES GIVEN IN REVERSE ORDER PRODUCE THE SAME ANSWER, which is the property the item is
    actually about.
  - A stated rank decides within a band, with the face id breaking the tie.
  - A cluster is never split: a face with the base letter and not its mark is passed over for one
    that has both.
  - The runs are face-homogeneous, and a cluster nothing covers fails the whole run rather than
    leaving a hole in it.

## What this item does NOT claim

The coverage answers come from a trait the caller implements, because the faces belong to the
catalogue rather than to this layer. What is checked is the POLICY - the order, the bands, the
atomicity - over a face set the fixtures state. Whether a real face covers what its catalogue record
says is the truth oracle's question, and it still waits on a staged face.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-12T15:22:36Z):

## What was delivered

Variable fonts, the METRIC half - which is the half this item singles out, and the reason `HVAR` and
`MVAR` are profile entries rather than optional extras. `font-parse`'s `variations`.

  - `fvar`: the axes and the named instances. A named instance is a POSITION, not a separate face.
  - NORMALISATION WITH EACH HALF OF THE AXIS SCALED SEPARATELY. A default is almost never the
    midpoint - weight runs 100 to 900 with a default of 400 - so a single linear scale puts every
    instance in the wrong place above or below the default.
  - `avar`, applied. It is how a font says the middle of its axis is not the middle of its design;
    skipping it is visibly the wrong weight on a non-linear axis rather than a rounding difference.
    Version 2 is refused by name, as the profile excludes it.
  - `HVAR`: the delta-set index map, the item variation store and the region scalars. A region that
    does not apply contributes ZERO rather than a reduced amount - treating it as partially applied
    is how an instance ends up between two masters that were never meant to be mixed.

## Verification

PERFORMED and passing: `cargo test` in `font-parse` - 10 fixtures - inside
`./check.sh --gate opentype-profile`.

The fixture font is a VARIABLE one this tree builds: a weight axis whose default is not its midpoint,
a named instance at 700, an `avar` that moves the middle of the upper half, and an `HVAR` delta that
applies over the top half of the axis alone.

  - The axes and the named instance read back; an axis index the font does not have is refused.
  - The default normalises to zero and the ends to the full range; 650 is half way up and 700 is 60%
    of the way, which a single scale over the whole axis would have got wrong; a coordinate outside
    the axis is CLAMPED rather than extrapolated.
  - `avar`'s stated points map exactly and the space between them linearly; a font with none is
    unchanged by it.
  - The advance delta is zero at the default, whole at the top of the axis, half at half way, and
    ZERO below the region rather than negative. A glyph the store has no delta for varies not at all,
    and a font with no `HVAR` is not an error.

TWO DEFECTS THE FIXTURES FOUND, both in the fixture's own font rather than in the reader - which is
worth recording because it is what building a font by hand is for: the `HVAR` header is a version and
FOUR offsets rather than three, and the item variation store's own header is twelve bytes rather than
eight. Either mistake points a table at its own middle, and the reader refused both rather than
reading whatever was there.

## What is NOT delivered, and the item is NOT ticked

The OUTLINE half: `gvar` deltas and CFF2 `blend`. I ticked this item and then unticked it: it says
"outlines AND metrics", and an instance with correct metrics and default outlines is a font drawn at
the wrong weight with the right spacing - no better than the reverse. The metric half is what the
profile's own reasoning singles out, which is why it was built first; it is not the whole item.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-12T15:43:49Z):

THE OUTLINE HALF OF VARIABLE-FONT SUPPORT, AND `MVAR` WITH IT.

WHAT WAS BUILT

`src/user/libs/text/font-parse/src/gvar.rs` is new. It reads the glyph variation data array in both
its offset forms - the short one stores HALF the offset as `loca` does, and one bit says which, so
reading that bit wrong halves or doubles every offset in the table and lands the parser in the middle
of somebody else's tuple. Tuple headers are read with shared and embedded peak tuples and with
intermediate regions, which is how a master applies over PART of an axis rather than all the way to
its end. Both packed encodings are decoded in place: the point-number list is delta-encoded, so
reading one number means walking to it, and that walk is the price of not decoding a glyph-sized list
onto the stack of a function that recurses for composites.

THE INFERENCE IS NOT AN OPTIMISATION, IT IS THE FORMAT. A tuple names a few of a glyph's points and
leaves the rest to be interpolated from their neighbours. A parser that moved only what was listed
would move a stem and leave the serif it carries behind - the outline tears rather than failing, and
nothing about the result says which table was misread. It runs per contour, because a point
interpolated from the next contour's points pulls two separate shapes together; it wraps around the
contour, because a contour is closed; outside the two references the nearer one's delta is taken
whole rather than extrapolated, which would put a spike where the font has a straight edge; and two
references at the same coordinate that moved by different amounts answer ZERO, because they say
nothing about a point between them and picking one of the two would be a number the font never wrote.

A COMPOSITE IS ADDRESSED BY COMPONENT. Its entries move whole components, and applying them to the
expanded points of those components would move an accent by the delta meant for the letter under it.
`glyf` grew `is_composite` and `components` for this, and its own composite walk was rewritten to go
through the same component reader - the placement rules are now stated once instead of once per
caller that needs to know where a component sits.

THE FOUR PHANTOM POINTS ARE PART OF THE COUNT. Every glyph's delta list ends with four points the
glyph does not contain. Leaving them out of the count makes the y delta run start four values early,
which moves every point of every varied glyph by somebody else's number - a defect that produces
plausible-looking wrong shapes rather than an error. They are counted, and `phantom_advance_delta`
answers them: a face that states its width variation through `gvar` alone, with no `HVAR`, is read
correctly instead of being treated as a face whose widths do not vary.

`MVAR` was added to `variations.rs`. `HVAR` makes the glyphs sit at the right distance along a line;
`MVAR` makes the LINES sit at the right distance from each other. A weight axis that thickens the
strokes almost always raises the ascender with them, and a layout reading `hhea` alone sets every
instance on the leading of the default one - lines that touch at one end of the axis and drift apart
at the other, which reads as a layout bug rather than a table nobody opened. The value-record stride
is taken from the font rather than hard-coded, so a later minor version that adds fields to the record
is walked correctly instead of read out of the middle of the previous record. The seven metrics are
named constants, so a call site cannot be one typo away from silently reading a metric the font does
not vary. The shared item variation store reader it goes through was parameterised by table tag, so a
fault in `MVAR` is now reported as `MVAR` rather than as `HVAR`.

NO ALLOCATION AND NO HIDDEN STACK. Every buffer belongs to the caller. A composite spends the first
part of them on its own components and hands the REST to each component it walks, so a component
cannot overwrite the delta that places it, and nesting is bounded by the buffer the caller was willing
to pay for rather than by a number this parser invented. A glyph that does not fit is a refusal: half
a glyph drawn is a shape the font does not contain, and a caller that receives one has no way to tell.

WHAT WAS VERIFIED, AND HOW

`cargo test -p font-parse`: 18 passed, 0 failed. Eight of those are new or rewritten. The fixture font
is a variable face this tree BUILDS rather than imports - a licensed third-party binary is a reviewed
import and none of what is checked here needs one, and a font written here can be made to contradict
itself on purpose, which a real one cannot.

  - a square whose right side is referenced and whose other two points are INFERRED, checked at the
    default coordinate (unchanged), at the top of the axis, half way up, and below the region;
  - a triangle whose apex sits half way between its two referenced base points and takes half their
    difference, with its y interpolated between two references that moved equally;
  - a composite whose component varies on its own account and is placed by its own component delta;
  - a phantom-point advance with no `HVAR` present in the face at all;
  - a buffer too small for the glyph, and a delta buffer too small for the phantom points, both
    refused;
  - `MVAR` at four coordinates for two varied metrics and two the font does not vary;
  - both new tables put through the exhaustive corruption sweep: every byte of the table, XORed with
    four patterns, every glyph, four coordinates. What is asserted there is that it ANSWERS - no
    panic, no read past the end, no walk that does not come back.

`./build.sh --part user -- -p font-parse -p font-shape` for x86_64: built, with `-D warnings`. The
dependent crates were run rather than assumed: `font-shape` 15 passed, `text-pipeline` 9 passed,
`font-contract` 11 passed, `unicode-segmentation` 6 passed. `./src/tools/check-opentype-profile.sh`
passes, 15 profile fixtures and the generated documents matching. `./format.sh` was run and the tests
re-run after it. Clippy is not one of this tree's gates, but the two new files were put through
`cargo clippy -D warnings` and are clean under it.

NOT PERFORMED: the aarch64 and riscv64 builds, and the full sweep. This is a leaf library with no
architecture-specific code and nothing outside `src/user/libs/text/` was touched; they belong to the
end of the milestone rather than to this change.

DEFECTS FOUND IN MY OWN WORK WHILE BUILDING IT

  - `interpolate` returned one of the two deltas when both references sat at the same coordinate. It
    must return zero unless the two deltas agree: with them disagreeing there is no direction to
    interpolate along, and picking one silently moves a point by a number the font never stated.
  - the first draft treated a private point-number count of zero as "a list of no points" instead of
    the format's shorthand for ALL points, which would have refused every tuple written that way.
  - the first draft counted only the glyph's own points, omitting the four phantom points, which puts
    the y delta run four values early.

WHY THE ITEM IS STILL OPEN

The plan's item says "outlines AND metrics correct at any coordinate". Both are now true for every
face this parser produces outlines for. What remains is CFF2 `blend`, and it cannot be written before
the CFF2 charstring interpreter it is an operator of - which is already named as remaining work under
the closed-profile item and is where it belongs. Until then a CFF2 face is refused WHOLESALE rather
than drawn at the wrong weight: a refusal the caller can see, not a wrong shape it cannot. I recorded
`gvar` and CFF2 `blend` last turn as the two reasons this box was not ticked, and I am not ticking it
now on the strength of having done one of them.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-12T15:59:19Z):

THE PRODUCER OF THE SHARED RUN: `font-run`.

WHY IT IS A CRATE

The seam was frozen in `font-contract` before either side implemented against it, and it is a
definition rather than a producer: `GlyphRun` and `ClusterMap` are BORROWED types, so something has
to be the layout that owns the storage. Nothing was. `font-shape` produces a buffer in font units
and `text-pipeline` deliberately takes "the glyphs, as the caller's shaper produced them" - putting
the conversion in either would have given the shaper a dependency on the renderer's seam, or given
the pipeline a dependency on a font parser it was designed not to have. So it is its own crate, with
one job: the single conversion between font units and the seam.

WHAT IT DECIDES, EACH IN ONE PLACE

  - THE NUMERIC FORM. Font units become 26.6 through the contract's own `from_font_units`, which
    rounds half to EVEN, and through nothing else. A second rounding downstream would make the result
    depend on the order two libraries were written in, which is a difference nobody can see in a
    picture and nobody can reproduce from one.
  - THE Y AXIS. A font measures upward from the baseline; the seam's device space measures downward.
    The flip is here. Passing font units straight through puts every mark on the wrong side of its
    base, which reads as a broken font rather than as a sign nobody wrote down.
  - THE ORDER. The glyphs are reversed into visual order here and the clusters are not. Shaping never
    reverses - every `GSUB` and `GPOS` rule is written about the order the text was typed, and a
    shaper that reversed early would apply `fina` to the first letter of an Arabic word - so the
    reversal belongs at the one point where the result stops being about the string and starts being
    about the picture.
  - WHAT A CLUSTER IS. A grapheme cluster, over the run's own source, as absolute offsets into the
    WHOLE string: a hit test asks about the string a person typed. Deriving clusters from the buffer
    would have made a combining mark its own caret stop, which is a caret position that is not a
    place in the text, and would have lost every cluster a ligature absorbed - which is precisely the
    caret position the mapping exists to carry.
  - WHICH FORM EACH GLYPH IS. Outline, `COLR` v0 layers, `COLR` v1 paint graph or a bitmap strike,
    read off the face's own indices. Richest first, because a face carrying both a paint graph and a
    bitmap for one glyph means the paint graph and drew the bitmap for consumers that cannot paint.
    The `CBLC` index subtables are walked rather than the size record's start and end glyph, because
    the record's range is its outer claim and a glyph inside it may still be absent - and telling a
    rasteriser to draw a bitmap that is not there is a missing glyph rather than a fallback to the
    outline that is.
  - WHICH STRIKE. The smallest one at least as large as the text, and the largest available when
    every strike is smaller. Scaling a bitmap down keeps the detail the designer drew; scaling one up
    does not, and a blurred glyph is the kind of defect that is never filed and never fixed.

WHAT WAS ADDED BESIDE IT

`GDEF`'s ligature caret list is read in `carets.rs`, in all three of its formats: a design
coordinate, a POINT INDEX into the glyph's own outline - resolved by walking that outline, because
the point is where the designer put it and nowhere else - and a coordinate with a device table, whose
adjustment is a hinting correction stated per pixel size and is deliberately not applied by a
pipeline that does not hint. The dividing position of `fi` is not derivable from its width; it is
not two equal halves, and the designer said where it divides.

`glyf` grew `is_composite` and `components`, and its own composite walk now goes through the same
component reader, so the placement rules are stated once rather than once per caller.

WHAT WAS VERIFIED, AND HOW

`cargo test -p font-run`: 11 passed, 0 failed. The fixtures are fonts this tree BUILDS - a ligature
caret list, a `COLR` version 0 list beside a version 1 one, three bitmap strikes at three sizes, and
a design grid chosen so that the rounding tie is exact rather than approximate. What each asserts:

  - the conversion at one site, including the tie: on a grid where every odd unit count lands on a
    half pixel, 1.5 and 2.5 both answer 2, where round-half-up would answer 2 and 3;
  - the y flip, on a mark raised above the baseline;
  - a right-to-left run drawn in visual order and mapped in logical order, with the first character
    drawn last and a whole-string selection still contiguous;
  - a combining mark that does not begin a cluster of its own - one cluster, two glyphs;
  - a ligature that keeps the cluster it absorbed, with that cluster owning no glyph and pointing at
    the one that swallowed it, and the face's own dividing caret converted to 26.6;
  - a glyph marked with the form the face offers, version 1 winning over version 0 where both cover
    it, and the palette present only where the kind uses it;
  - a strike chosen by size, at, above and below the strikes available, and an outline for a glyph no
    strike holds;
  - a value outside 26.6 refusing the run by name rather than saturating;
  - a run longer than the seam can index refused by name;
  - a run naming a range outside its own text refused;
  - a produced run yielding the cache key a consumer rasterises through.

`cargo clippy -D warnings` over the whole crate including its tests: clean. `./format.sh` was run
and the tests re-run after it. `verify-model host-suites` discovers the crate: 90 runnable suites
against a floor of 55, and `font-run` is among them - so it is in the host gate without anybody
remembering to add it. The crate is registered in `user/services/manifest.toml` as a source, which
is how every library that stages nowhere yet is carried, and in the release-required host list beside
its siblings.

NOT PERFORMED: the aarch64 and riscv64 builds, the full sweep, and the in-kernel suite. This is a
leaf library with no architecture-specific code and no consumer in the image yet; they belong to the
end of the milestone.

WHAT THIS ITEM DELIBERATELY DOES NOT DO

Caret and selection GEOMETRY - where a caret actually lands and how a selection is drawn - is the
line layout item's, and it is the item that has a run boundary to be on two sides of. This one
carries what that geometry is derived from: the source range per cluster, the intra-ligature carets,
the logical-to-visual mapping, and the affinity type a caret query is asked with.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0136 (2026-09-12T16:11:05Z):

THE NUMERIC CEILINGS: FROZEN AS VALUES, AND MOSTLY ENFORCED.

THE PROBLEM THIS ITEM NAMES, RESTATED

Every length and index in this parser is checked against the table it is in. That is what stops a
crafted font reading somebody else's memory, and it stops nothing else a font can do. A
STRUCTURALLY VALID face can ask for a composite glyph nested inside itself, a contextual rule whose
output is a thousand times its input, or an axis count that makes every delta cost a thousand region
scalars - and every offset in it is in range at every read. What that exhausts is time, stack and the
caller's memory, and none of it is a parse error anywhere.

WHAT WAS BUILT

`opentype-profile` gained a `limits` module. Every ceiling in the plan's table is a constant, and
beside them is a published list giving each one's unit, its KIND and what a font or a document could
do without it. The kind is part of the data and not decoration: it decides what a ceiling can
promise. An `internal` ceiling is a property of the face and a larger document does not change it; a
`proportional` one caps a multiplier and not a product; an `absolute` one caps a document, and is
the only kind that makes the proportional ones bound anything. A fixture holds the constants and the
published table to being the same values - a call site uses the constant and the document is
generated from the table, so a table that drifted would publish a ceiling nothing enforces, which is
exactly the failure a written-down limit is meant to prevent.

The ceilings are part of the canonical form the PROFILE HASH is taken over, and they have their own
section in the generated document. Raising one quietly is now a line in a diff.

THE NUMBERS ARE READ RATHER THAN RESTATED, AND TWO OF THEM WERE WRONG

Three ceilings already existed as local constants. Two matched the profile by luck. Two did not:

  - `font-shape`'s nested-lookup ceiling was 8 where the profile freezes 64;
  - `font-parse`'s variation-axis ceiling was 16 where the profile freezes 64.

That is the drift this item exists to end, found by doing the thing the item asks for. Every site now
takes the profile's constant, so there is one number and one place to change it.

WHAT REFUSES TODAY

  - the file's own size and a single table's, both before anything inside them is read;
  - the composite nesting depth, now a typed ceiling refusal rather than a bad-glyph one;
  - THE EXPANDED POINT COUNT, and this one is new behaviour rather than a renamed constant. The
    previous bound was per glyph description: a glyph could have up to 65535 points. Depth alone
    bounds the stack and not the work, and five levels of nesting MULTIPLY - a composite of ten
    composites of ten composites is a thousand glyphs' worth of points with every offset in range.
    The count is now accumulated across the WHOLE walk and capped at the profile's ten thousand;
  - the variation axis count and the item variation store's region count;
  - the nested-lookup depth, the feature count and the selected-lookup count;
  - the absolute input ceiling on a shaping run, checked BEFORE the script shaper's own pass copies
    and rewrites the run - doing that work in order to discover the run was too long is the shape of
    exhaustion the ceiling exists to prevent;
  - the absolute output ceiling on a run, with the proportional expansion rule checked beside it;
  - the absolute input and output ceilings on a paragraph, the output one counted across the whole
    paragraph rather than per run, because a per-run check passes a paragraph made of many runs;
  - the fallback face count, which otherwise walks every face in the catalogue once per cluster and
    asks each of them in both canonical spellings.

REFUSALS, NOT TRUNCATIONS - AND TWO BEHAVIOUR CHANGES

A nested lookup past the depth was declined SILENTLY, and a lookup past the count was DROPPED. Both
leave a run shaped as though the font had not asked for the rule: a document rendered wrong with
nothing anywhere to say so, which is the failure that is never reported because it does not look like
one. Both are now refusals. `Unsupported::Exceeded` carries which ceiling and by how much, because
"too complex" is not something a report, a staging tool or a person can act on, and a conformance
suite cannot assert on it either.

ALLOCATION

Every allocation in the run producer whose size comes from input is `try_reserve_exact` with a typed
refusal. Userspace infallible allocation ends the process when it fails, so a layout that allocated
infallibly could not produce the typed refusal this milestone promises - it would take the caller's
whole program down over one paragraph.

THE ONE NUMBER DELIBERATELY STATED TWICE

The bidi depth of 125 is UAX #9's and not this profile's choice. A Unicode implementation that had to
link an OpenType profile to learn its own algorithm's maximum depth would have the dependency the
wrong way round, so `unicode-bidi` keeps it and the profile RECORDS it, with a comment in each place
saying which is which.

WHAT WAS VERIFIED, AND HOW

  opentype-profile  13 passed - including the constants-match-the-table fixture, the absolute-ceiling
                    fixture (both directions present, and the output ceiling reachable within the
                    expansion rule, or one of the two says nothing), and the one that requires every
                    ceiling to carry a unit, a kind and a reason
  font-parse        21 passed - a table over the ceiling refused by name; a composite whose two
                    components expand to twelve thousand points refused while the six-thousand-point
                    leaf on its own is drawn; a face declaring sixty-five axes refused by name; the
                    two-glyph cycle now refused by the depth ceiling while a direct self-reference is
                    still refused by name one level down
  font-shape        17 passed - a run one past the input ceiling refused and a run exactly at it
                    accepted; more features than the ceiling refused rather than dropped
  font-run          11 passed - a run one past the output ceiling refused by name, one exactly at it
                    accepted
  text-pipeline     11 passed - a paragraph whose two runs are together one glyph over refused, which
                    a per-run check would have passed; the fallback walk not reaching the face one
                    past the ceiling, and reaching the one at it
  font-contract     11 passed        unicode-bidi  8 passed

`./src/tools/check-opentype-profile.sh` passes with the regenerated document and its new hash.
`./format.sh` was run and every suite re-run after it.

NOT PERFORMED: the aarch64 and riscv64 builds and the full sweep.

WHY THE ITEM IS STILL OPEN

Five ceilings have no enforcement site, because the code they bound does not exist yet, and a ceiling
with no code bounds nothing. The two charstring ones belong to the `CFF`/`CFF2` interpreter and the
two paint-graph ones to the `COLR` decoder - both named as remaining work under the parser item -
and the three pass and retry ceilings belong to the line layout item, which is where a layout loop
will first exist to be iterated. I am not ticking a box that says "bound the recursive work" while
the two recursive structures with the deepest recursion in the format have no bound in code.
