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
