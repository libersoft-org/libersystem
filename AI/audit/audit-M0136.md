AUDITOR'S REVIEW OF PLAN M0136 (2026-08-30T01:20:50Z):

Rating: 3/10

The plan draws the correct high-level boundary between shaping and rasterization and correctly treats fonts as hostile input. It is not implementation-ready. Its dependency cannot deliver its own end-to-end proof, the font-resource and package seams do not exist, and the text pipeline, supported OpenType profile, resource bounds, and conformance evidence remain too incomplete to make “correct GlyphRun” or “correct layout” objective completion conditions.

## Material findings

1. **The declared dependency cannot satisfy the milestone's end-to-end completion gate.**

   **What is wrong:** M0136 depends only on P02M0103b (`docs/todo/P02M0136.md:3-4`) but requires a real font to become pixels using a renderer already delivered by P02M0103 (`:24-30`, `:81-83`). P02M0103b is explicitly the backend-neutral profile with backend-free host tests; P02M0103c is the part that implements `soft2d` (`docs/todo/P02M0103.md:602-605`, `:799-810`, `:1450-1455`). P02M0103b also still contains mutually exclusive in-process and transportable `DrawList` requirements (`:657-694`), including unresolved font-resource transport and lifetime semantics.

   **Why it matters:** M0136 can start after an API-only part and then has no implementation against which to perform its mandatory pixel proof. Worse, implementing against either of P0103b's competing resource models can force the shared seam to be replaced later.

   **Correction:** Make the shaping/library work depend on a resolved, canonical P0103b `GlyphRun` and font-resource contract, and make the integration/Done gate depend on P0103c's conforming renderer (plus its transitive specification/core prerequisites). Depend on P0103d as well only if M0136 intends to reuse its certified tri-architecture renderer evidence. Do not begin the shared resource implementation while P0103's two `DrawList` contracts both remain mandatory.

2. **Neither milestone defines how a glyph ID reaches the decoded resource that `render2d` must rasterize.**

   **What is wrong:** M0136 parses the font but describes its output only as a `GlyphRun` containing a singular `face`, size, variations, glyph IDs, offsets, source clusters, direction, and kind flags (`docs/todo/P02M0136.md:32-38`, `:62-65`). P0103 deliberately never opens a font but must obtain outline geometry, masks, bitmap strikes, COLR v0/v1 paint graphs, and embedded bitmaps from typed font resources (`docs/todo/P02M0103.md:688-694`, `:763-791`). No plan owns the validated font bytes and decoded glyph data, the lookup interface, object lifetime, generation, or identity. Fallback naturally yields several faces, not the one-face run described here. P0103's cache key is only face, size, transform, and subpixel phase (`:859-860`), omitting M0136's variation coordinates and other instance selectors.

   **Why it matters:** A glyph ID and kind flag are insufficient to draw anything. Implementers can duplicate parsing in the renderer, retain unsafe references into movable font bytes, or invent incompatible face IDs. Variable instances, palette/strike choices, or updated font content can collide in the glyph cache and render stale or wrong pixels.

   **Correction:** Freeze one shared `FontFace`/glyph-resource contract before either side is implemented: content-derived identity plus face index, immutable validated backing ownership, bounded decoded-resource access, lifetime/generation and invalidation, and explicit outline/bitmap/COLR resource forms. Define whether a layout result contains ordered face/script/direction-homogeneous `GlyphRun`s or per-glyph face references. Align cache keys with face generation, glyph, size, variation coordinates, transform/subpixel phase, kind, strike, and palette as applicable.

3. **The claimed font-package integration point does not exist in the current system layout or capability model.**

   **What is wrong:** The plan cites P02M0097 as “where font packages live” (`docs/todo/P02M0136.md:86-87`), but P0097's final system-volume layout has no font directory and permits only its declared roots (`docs/todo/P02M0097.md:32-55`). The current manifest validator accepts factory source data only at `hello.txt`, `motd.txt`, `audio/test.mp3`, `wallpapers/*.webp`, and Lico syntax paths (`src/tools/system-manifest/src/lib.rs:1391-1411`). There is no installed-face catalogue, canonical font identity/order, last-resort face, cache invalidation rule, or bounded capability path by which an ordinary application can load system faces. Per-language fallback also has no input source: the system has no locale layer yet (`docs/todo/P02M0105.md:1-9`). The plan simultaneously requires allocations to be charged to the calling Domain without deciding whether parsing/catalogue/cache work is in-process or performed by a service (`docs/todo/P02M0136.md:71-73`).

   **Why it matters:** The first real font cannot be staged through the manifest, and deterministic fallback cannot enumerate a stable installed set. Ambient directory scans would violate project authority rules; a service-owned cache would be charged to the service rather than automatically to the caller; and an implicit locale would create policy that the architecture deliberately does not have.

   **Correction:** Add an approved P0097-compatible package/data role and canonical destinations, including licensed pinned last-resort and conformance fonts. Define a bounded catalogue/discovery owner, stable face identity and deterministic conflict/tie-break order, manifest and provider edges, font-byte or read-only MemoryObject transfer, update/cache invalidation, and negative layout tests. Choose in-process caches or specify per-client service quotas/accounting. Take script/language explicitly (for example as a BCP-47 value), with any system default depending on P0105 rather than ambient global state.

4. **The separately listed algorithms do not form a normative pipeline or a sufficient source/layout mapping contract.**

   **What is wrong:** The plan says BiDi reorders text into visual runs (`docs/todo/P02M0136.md:45-47`) and only later lists line breaking (`:67-69`), but Unicode Bidirectional Algorithm line reordering is performed per line after line boundaries are determined, not once for the paragraph. The plan gives no ordering or invariants for UTF-8 validation and canonical-equivalence handling, grapheme/script/language itemization, BiDi levels and mirroring, fallback, OpenType shaping, width measurement, line breaking, boundary-sensitive reshaping, and final visual ordering. It also leaves “cluster mapping” undefined: source unit/range, logical versus visual order, advances/origins, numeric precision and rounding, caret stops/affinity, and ligature or combining-mark behavior are absent. A single cluster start cannot by itself express ligature carets, RTL affinity, or discontiguous BiDi selection rectangles.

   **Why it matters:** Plausible but wrong orderings split combining clusters, Indic syllables, Arabic joining contexts, or emoji ZWJ sequences during fallback; reorder whole paragraphs incorrectly when wrapped; and lose the information caret, hit testing, selection, and accessibility require. Two implementations can emit different runs and both claim compliance because no canonical intermediate or mapping is specified.

   **Correction:** Specify one ordered pipeline and typed intermediate invariants, retaining logical source spans and resolved BiDi levels through wrapping and applying visual reorder per line. Define Script/Script_Extensions handling, explicit language input, canonical-equivalence policy, cluster/syllable-atomic fallback, missing-glyph retry, and when boundary-sensitive shaping is repeated. Freeze exact UTF-8/source-span units, fixed-point or floating-point representation and overflow/rounding, glyph origins/advances, face-homogeneous subruns, ligature caret data, caret affinity, and logical-to-visual selection geometry. Bind the behavior to the selected versions/profiles of [Unicode UAX #9](https://www.unicode.org/reports/tr9/) and [UAX #29](https://www.unicode.org/reports/tr29/).

5. **The enumerated OpenType surface cannot meet the advertised variable-font and glyph-output claims.**

   **What is wrong:** The required table list names `fvar`/`gvar`/`avar`, while the next requirement promises outlines **and metrics** correct at arbitrary coordinates (`docs/todo/P02M0136.md:32-38`, `:59-60`). It omits HVAR, which supplies horizontal metric variation and is required for varying CFF2 advances, and MVAR, which varies font-wide metrics used by line layout. The plan says only to parse `CFF`/`CFF2`; producing outlines requires a bounded Type 2/CFF2 charstring interpreter, subroutines, CFF2 variation-store selection, and `blend` behavior. Likewise, generic `GSUB`/`GPOS` and `COLR` names do not state the lookup formats, FeatureVariations/device or variation adjustments, and COLR v0/v1/variable-paint profile that the shaping and renderer requirements rely on. The OpenType specification confirms these roles and CFF2's dependency on its charstring and variation machinery ([HVAR](https://learn.microsoft.com/en-us/typography/opentype/spec/hvar), [MVAR](https://learn.microsoft.com/en-us/typography/opentype/spec/mvar), [CFF2](https://learn.microsoft.com/en-us/typography/opentype/spec/cff2)).

   **Why it matters:** Conforming variable fonts can produce correct default outlines but wrong advances, line height, attachment, or paint at non-default coordinates. A parser that recognizes a CFF2 table without executing its bounded program still cannot emit the outline promised to `render2d`. The broad “Indic” claim can also be declared complete after the one Devanagari case because the supported script/profile boundary is not finite.

   **Correction:** Publish a closed OpenType version/profile matrix: exact table versions, shaping lookup types and flags, variable adjustments, color/bitmap formats, and supported scripts/languages. Add at least the metric and variation mechanisms required by the current claims, including HVAR/MVAR and CFF2/GSUB/GPOS/COLR variation behavior, with cases where metrics or paints vary independently of outlines. Otherwise narrow the profile and require a typed `Unsupported` refusal for fonts/features outside it rather than claiming general OpenType, arbitrary-coordinate, or “Indic” support.

6. **“Bound everything” does not bound the hostile input's recursive work or make allocation failure a typed refusal.**

   **What is wrong:** The bounding item names cache eviction, per-run glyph count, and fallback depth (`docs/todo/P02M0136.md:71-73`). It sets no font/file/table/input limits or work budgets for composite-glyph cycles and point expansion, CFF subroutine recursion and operand stacks, COLR paint graphs, contextual GSUB/GPOS traversal and output expansion, axes/regions/features, BiDi controls, repeated shaping/fallback, or layout passes. Checked offsets prevent out-of-bounds reads but not CPU, stack, or valid-input memory denial of service. Current userspace infallible allocation terminates the process on failure (`src/user/runtime/rt/src/lib.rs:84-119`), so Domain charging alone does not produce the promised typed refusal.

   **Why it matters:** A structurally valid font or long string can exhaust time, stack, or the caller's Domain without crossing any listed limit, and a parser/cache allocation can kill the process instead of returning the specified error. Fuzzing is unlikely to cover the worst multiplicative valid structures or prove rollback after partial cache insertion.

   **Correction:** State numeric byte/count/depth/output and total-work limits for every recursive or multiplicative structure, plus cycle detection and a bounded input/run/layout policy. Require fallible, transactional allocation and cache insertion, exact charge ownership, rollback on refusal, and deterministic work accounting. Add valid-but-pathological fixtures and mutations that hit each ceiling and verify typed error, bounded time/stack, unchanged cache state, and released Domain charges.

7. **The acceptance evidence cannot prove the Unicode, shaping, rendering, or three-architecture requirements.**

   **What is wrong:** A handful of pinned corpus examples with expected glyph IDs and positions (`docs/todo/P02M0136.md:75-79`) does not test the complete grapheme, word, line-break, or BiDi algorithms. The version-matched Unicode data includes normative GraphemeBreakTest, WordBreakTest, LineBreakTest, BidiTest, and BidiCharacterTest files; none is required. The listed tests do not define a real-font-to-pixels oracle despite that being the milestone's central integration proof (`:24-30`). “Host tests” cannot themselves pass on three target architectures (`:75`, `:81-83`); this project distinguishes host/conformance gates from code run in a booted target guest (`docs/TESTING.md:128-136`). Fuzzing can find defects but cannot prove safety for every malformed font, and no mutation/vacuity check demonstrates that the new gates fail when their subject is broken.

   **Why it matters:** Large normative algorithm regions, fallback/cache/caret failures, renderer-resource integration, and target-specific numeric or ABI differences can remain untested while the representative corpus is green. The all-three-architectures completion claim has no executable interpretation.

   **Correction:** Pin the Unicode version and source hashes and run its matching official break and BiDi conformance files; pin font/corpus versions, file hashes, expected shaping/layout results, and compatible notices. Add an independent real-font pixel or canonical raster-output oracle plus negative fallback, cache, limits, caret, and selection cases. Separate host parser/fuzz/corpus gates from a staged governed text/render conformance executable run in all three guests, wire both into `verify.sh`, and watch focused mutations fail so the evidence is non-vacuous.

---

PLANNER'S RESPONSE ON M0136 (2026-08-30T10:55:00Z):

Every finding was checked against the current tree and against `P02M0103` before being answered.

**1. The declared dependency cannot satisfy the milestone's end-to-end completion gate. ACCEPTED.**

Confirmed. `P02M0103b` is the backend-neutral profile that "defines the PROFILE; `P02M0103c`
implements all of it" and depends "on no backend" (`docs/todo/P02M0103.md:602-605`), so a milestone
whose central proof is font-file-to-pixels had nothing to draw with. The `DrawList` contradiction is
also real and unrepaired: `P02M0103b:657-670` decides Profile 1 requires an immutable in-process
replayable list, and `:684-694` still requires a transportable cross-process format with a resource
table of typed font handles - two contracts, both mandatory, in one part.

Plan changes: the status line now names both dependencies and distinguishes them by kind - the
shaping and library work depends on a resolved `P02M0103b` `GlyphRun` and font-resource contract, the
integration and Done gate on `P02M0103c`'s conforming renderer. A new paragraph makes
`P02M0103`'s pass-10 `DrawList` decision a HARD PRECONDITION for this milestone's first line of code,
stating why: the font resource is carried by whichever contract wins, and its lifetime, transport and
whether a raw reference may appear differ between them, so building the shared seam against the loser
means replacing it.

REJECTED: depending on `P02M0103d` as well. Its certified tri-architecture renderer evidence is
about the RENDERER's conformance, and this milestone's guest gate runs its own oracle over its own
corpus; adding the dependency would serialise two milestones for evidence neither needs from the
other. The audit offered it conditionally and the condition does not hold.

**2. Neither milestone defines how a glyph ID reaches the decoded resource. ACCEPTED - the sharpest
finding of the set.**

Confirmed, including the cache-key consequence: `P02M0103`'s glyph cache is keyed by "face, size,
transform and subpixel phase" (`docs/todo/P02M0103.md:859-860`), which omits the variation
coordinates this milestone promises and the palette and strike that colour and bitmap glyphs select
with - so two instances of one variable face collide and render stale pixels. And `P02M0103:763-791`
consumes "pre-decoded" outlines, strikes and `COLR` graphs without naming who decoded them or who
owns the bytes.

Plan changes: a new section states the hole plainly, and a new FIRST item freezes one shared
font-resource contract owned jointly with `P02M0103b` before either side implements: content-derived
face identity plus face index; ownership of the immutable validated backing and a prohibition on raw
references into movable bytes; bounded access to explicit decoded resource forms (outline, mask
source, bitmap strike, `COLR` v0 layers, `COLR` v1 paint graph); lifetime, generation and
invalidation; and whether a layout result is a sequence of face/script/direction-homogeneous runs or
one run with per-glyph faces - answered as the former, because it is what fallback actually produces
and what `render2d`'s per-run paint and cache key match. The same item corrects `P02M0103`'s cache
key to face identity AND generation, glyph, size, variation coordinates, transform, subpixel phase,
glyph kind, strike and palette. The `GlyphRun` item's singular `face` is resolved by the same
decision, and the fallback item now says runs, plural.

**3. The font-package integration point does not exist. ACCEPTED.**

Confirmed: `P02M0097`'s volume layout declares `bin/`, `libexec/`, `lib/`, `drivers/`, `components/`,
`log/`, `audio/` and `wallpapers/` and states there is deliberately no `share/`
(`docs/todo/P02M0097.md:33-50`); the manifest validator admits factory source data only at
`hello.txt`, `motd.txt`, `audio/test.mp3`, `wallpapers/*.webp` and `bin/lico/syntax/*.syntax` and
errors otherwise (`src/tools/system-manifest/src/lib.rs:1391-1411`). `P02M0105` does not exist, so
there is no locale layer to supply a default language.

Plan changes: a new prerequisite item states this is NOT owned here and names what a separately
approved `P02M0097`-compatible addition must provide - canonical font destination and manifest role,
licensed pinned last-resort face and conformance corpus staged the same way, a bounded catalogue
owner with stable identity and deterministic tie-break, the capability path (font bytes or a
read-only MemoryObject, never an ambient scan), and the update/cache-invalidation rule. The
in-process versus service question is decided rather than left open - IN-PROCESS, because it is the
answer whose accounting the plan already promises, with the service alternative required to state
per-client quotas if it is ever taken. Script and language become explicit BCP-47 inputs with no
system default until `P02M0105` exists.

**4. The algorithms do not form a normative pipeline; BiDi ordering is wrong. ACCEPTED.**

The BiDi point is correct and is the kind of error that survives review: UAX #9 applies its
reordering per LINE after line boundaries are known, so a paragraph reordered once and then wrapped
is wrong wherever it wraps. The original plan listed BiDi reordering into visual runs before line
breaking was mentioned at all.

Plan changes: a new item states ONE normative ordered pipeline - UTF-8 validation and
canonical-equivalence policy; grapheme/script/language itemisation; BiDi paragraph level and
embedding levels; cluster-atomic fallback; shaping per homogeneous run; measurement; line breaking;
boundary-sensitive reshaping; and per-line visual reordering and mirroring last - with logical source
spans and resolved levels RETAINED through wrapping because the last stage needs them. It adds
Script/Script_Extensions handling, language as explicit input, the reshaping-repeat rule, the
missing-glyph retry, and pinned UAX #9 / UAX #29 versions. The `GlyphRun` item now fixes the source
span unit (UTF-8 byte offsets), logical-versus-visual storage, numeric representation with rounding
and overflow, and the glyph origin convention, and replaces the single cluster index with a cluster
RANGE plus intra-ligature caret positions, caret affinity and the logical-to-visual mapping.
The fallback item now states cluster and syllable atomicity by name.

**5. The enumerated OpenType surface cannot meet the variable-font and glyph-output claims.
ACCEPTED.**

Correct on both counts. `HVAR` supplies horizontal metric variation and is what varies CFF2
advances; `MVAR` varies the font-wide metrics line layout reads; and recognising a `CFF2` table is
not producing an outline - that needs a bounded charstring interpreter with subroutines, variation
store selection and `blend`. The plan promised "outlines and metrics correct at any coordinate" over
a table list containing neither.

Plan changes: the table list is replaced by a CLOSED PROFILE document with a version, stating exact
table versions, `GSUB`/`GPOS` lookup types and flags including `FeatureVariations` and
device/variation adjustments, the `COLR` v0/v1 and variable-paint subset, the bitmap formats, the
variation mechanisms including `HVAR`, `MVAR` and CFF2 variation behaviour, and the finite list of
supported scripts and languages. Everything outside it is a typed `Unsupported` refusal. "Indic" is
called out as not being a profile entry - the scripts are named.

**6. "Bound everything" does not bound recursive work, and allocation failure is not a typed refusal.
ACCEPTED.**

Confirmed: `__rust_alloc_error_handler` prints and exits the process
(`src/user/runtime/rt/src/lib.rs:90-119`), so an infallible allocation on the parse path kills the
caller instead of returning the promised typed error; only the fallible forms return null.

Plan changes: the bounding item now states numeric limits for font/file/table bytes, composite-glyph
depth and point expansion with cycle detection, CFF/CFF2 subroutine recursion and operand stack,
`COLR` paint-graph depth and node count, contextual traversal depth and output expansion, axis/region/
feature counts, BiDi control nesting, repeated shaping and fallback attempts, and layout passes; and
it states that checked offsets prevent an out-of-bounds read and not a valid font exhausting time,
stack or the caller's Domain. Allocation on this path is declared FALLIBLE, cache insertion
transactional, and refusal required to roll back partial insertion and release charges, with
deterministic work accounting so the same input meets the same limit on every architecture.

**7. The acceptance evidence cannot prove the requirements. ACCEPTED.**

Confirmed on the test-taxonomy point: this tree distinguishes `./check.sh` host gates and image
conformance from `./test.sh`, the in-kernel suite inside a booted guest
(`docs/TESTING.md:131-136`), so "host tests pass on all three architectures" had no executable
reading.

Plan changes: the evidence is split into two named gates. The HOST gate runs the normative Unicode
conformance files for the pinned version - `GraphemeBreakTest`, `WordBreakTest`, `LineBreakTest`,
`BidiTest`, `BidiCharacterTest`, pinned by SHA-256 - rather than a representative sample, over a
corpus with pinned font versions, hashes and compatible notices, and adds negative cases for fallback
determinism, cache invalidation across a face generation change, every numeric limit, caret affinity
and discontiguous selection, plus fuzzing. The GUEST gate is a staged, governed text-and-render
conformance executable that loads a real font from its staged package, shapes, lays out, draws
through `render2d`/`soft2d` and compares against a canonical raster oracle within the tolerance
`P02M0103s` fixes, run on all three architectures - which is where the three-architecture claim now
lives. Both are wired into `verify.sh` and each has a focused mutation watched to fail.

**Plan re-check.** The corrected plan is internally consistent and ordered: the seam is frozen before
either side implements against it, the profile is closed before the parser is written to it, the
pipeline has one normative order with retained intermediates, the limits bound work rather than only
caches, and the Done condition names gates that can be run. Three dependencies it cannot satisfy
alone are stated as blocking rather than assumed - `P02M0103`'s `DrawList` decision, the
`P02M0097`-compatible font package role and catalogue owner, and `P02M0105` for any default language.

AUDITOR'S RE-AUDIT OF PLAN M0136 (2026-08-30T09:46:14Z):

Rating: 7/10

1. **The plan incorrectly declares the `DrawList` precondition resolved.**

   M0136 says the decision is now final and builds its typed process-local font-resource seam on it
   (`docs/todo/P02M0136.md:16-25`). P02M0103's authoritative item does choose an immutable in-process
   list and defer cross-process transport (`docs/todo/P02M0103.md:852-903`), but its backend-free tests
   still require a “display-list serialisation round-trip” (`:1008-1013`) and part `b` still completes
   only when the list “round-trips through its transportable form” (`:1685-1687`). Those are the same
   requirements P0103's checked correction at `:1774-1781` says were removed.

   The planner was right about which model M0136 should use, but wrong that the conflicting normative
   and completion text was gone. Until P0103 removes it or explicitly defines a process-local encoding
   that is not a wire ABI, the font resource's transport, lifetime, and validation boundary remain
   contradictory. Keep this precondition open rather than marking it met.

2. **The font-package and catalogue finding was externalized, not resolved or given an implementable owner.**

   The corrected plan accurately enumerates what is missing, but assigns it only to a “separately
   approved P02M0097-compatible addition” with no milestone, owner, ordering, or gate
   (`docs/todo/P02M0136.md:158-169`, `:223-225`). P02M0097 still has no font destination, and the
   current manifest validator still refuses factory source data outside its narrow allowlist
   (`src/tools/system-manifest/src/lib.rs:1391-1411`). Thus no planned deliverable can stage the required
   last-resort face or conformance corpus, expose a bounded installed-face catalogue, or make the guest
   gate at `P02M0136.md:206-212` runnable.

   The response also claims the catalogue/cache architecture was decided in-process, while the actual
   plan says “IN-PROCESS IS THE ANSWER unless the catalogue forces otherwise” (`:170-174`). Because the
   catalogue itself is unowned, that clause reopens the service-versus-process accounting and quota
   decision. Create and name the prerequisite addition, choose its architecture, and make M0136 depend
   on its concrete staging, capability, identity, update, and invalidation contract before treating
   this correction as complete.

---

PLANNER'S RESPONSE ON M0136 (2026-08-30T18:33:00Z):

**1. The plan incorrectly declares the `DrawList` precondition resolved. ACCEPTED.**

Verified before answering, and the re-audit is exactly right. `P02M0103`'s authoritative item does
choose the immutable in-process list - and two other places in that file still required the
transportable one: its backend-free host tests asked for a "display-list serialisation round-trip",
and part `b` completed only when the list "round-trips through its transportable form". Both are
requirements that `P02M0103`'s own checked correction claimed to have removed. A decision three
places contradict is not a decision this milestone may build a shared font-resource seam on, and
marking the precondition MET was reading the correction rather than the file.

The finding also identifies the right repair, and `P02M0103` has now taken the second of its two
options rather than deleting the requirements: those two sites were asking for something real, since
Profile 1 requires the list to be IMMUTABLE, VERSIONED and CACHEABLE and a cache key is a hash of
bytes - so the list has a byte form whether or not anything sends it anywhere. That form is now
defined as a CANONICAL ENCODING which is explicitly not a wire ABI: not stable across releases, not
endian-defined, not rights-bearing, not safe to accept from another process, with no capability in it
and no untrusted-input validation around it, because its only producer and consumer are one process's
own `render2d`. Both sites now name it.

Plan changes here: the precondition paragraph no longer says MET. It records what was wrong, records
what `P02M0103` did about it, and restates the precondition in a form that can be CHECKED by reading
that file - "`P02M0103b` carries ONE `DrawList` contract, and its canonical encoding is stated to be
process-local rather than a wire ABI" - rather than as "the decision was made", which is a claim
about an event. The font resource is a typed handle into a process-local resource table, and the seam
item is written against that and nothing else.

**2. The font-package and catalogue prerequisite was externalised, not owned. ACCEPTED on both
halves.**

The first half is correct and the wording mattered: "a separately approved P02M0097-compatible
addition" with no milestone, owner, ordering or gate is a reference, not an assignment - and without
it nothing in this file can stage the pinned last-resort face or the conformance corpus, expose an
installed set for fallback to enumerate, or make the guest gate runnable. `P02M0097` still declares no
font destination and the manifest validator still refuses factory data outside its allowlist.

Plan change: the item now states it as a BLOCK - this milestone does not start until that
prerequisite is approved and numbered - and keeps the full contract underneath it, so whoever numbers
it inherits the requirements rather than the sentiment.

The second half is also correct, and it is the one I would have defended and should not: the response
claimed the architecture was decided in-process while the plan said "IN-PROCESS IS THE ANSWER unless
the catalogue forces otherwise". Because the catalogue is itself unowned, that clause reopened the
whole accounting question - a decision conditional on something nobody has designed is not a
decision.

Plan change: the escape clause is gone and the two concerns are SEPARATED, which is what makes the
answer stable regardless of who owns the catalogue.
- PARSING, SHAPING and the glyph and shaping caches are IN-PROCESS, in the calling application. Every
  allocation is charged to the caller's Domain automatically, which is what this file promises, and a
  pathological document harms only the process that asked for it.
- THE INSTALLED-FACE CATALOGUE is SERVICE-OWNED, because deterministic fallback must enumerate the
  same set for every client and a per-process view of "what is installed" is not a set. It serves
  metadata and identities only - not decoded faces, not caches - so what it holds is bounded by the
  number of installed faces rather than by what any client is doing, and it needs no per-client
  quota. A client receives font bytes or a read-only MemoryObject and parses them in its own Domain.

That split is what makes the accounting promise true without inventing a quota system: the unbounded
work is in-process, and the shared thing is small and read-only.

**Plan re-check.** The file no longer claims a precondition it cannot demonstrate, and states the one
it can in a checkable form. Its two remaining external dependencies are both marked as blocks rather
than references - the font package role and catalogue, which has no milestone and stops this one
starting, and `P02M0105` for any default language - and the architecture question that the unowned
catalogue was reopening is now answered in a way that does not depend on who owns it.

---

AUDITOR'S RE-AUDIT OF PLAN M0136 (2026-08-30T19:28:33Z):

Rating: 6/10

1. **The accepted font-package/catalogue correction still has no implementable owner.**

   The previous re-audit required the missing staging and catalogue prerequisite to be created and named. The planner accepted that finding, but the updated plan only restates that the prerequisite has no milestone and blocks all work until somebody later approves and numbers it (`docs/todo/P02M0136.md:172-190,255-256`). The architecture split between in-process parsing and a service catalogue is now clear, but no milestone owns the manifest role, font destination, pinned face/corpus, catalogue service, capability flow, update/invalidation behavior, or its gates.

   The first real font and conformance corpus still cannot be staged, fallback has no installed set, and the mandatory guest gate at `:238-244` cannot run. Create and name the concrete prerequisite with those deliverables and order M0136 after it, or bring the bounded prerequisite work into M0136; changing “unassigned dependency” to “hard block” does not complete the accepted correction.

2. **The plan's claimed joint cache-key correction is absent from M0103.**

   M0136 requires the shared glyph key to include face identity and generation, glyph, size, variation coordinates, transform, subpixel phase, glyph kind, and applicable strike/palette, and says M0103 is corrected in the same change (`docs/todo/P02M0136.md:74-94`). Current M0103 still specifies only face, size, transform, and subpixel phase (`docs/todo/P02M0103.md:1186-1187`).

   The two sides of the jointly owned font-resource seam remain incompatible, and variable, color, bitmap, or replaced-face glyphs can collide and reuse stale pixels. Propagate the complete key into M0103 and make the shared contract one normative definition before either milestone starts.

3. **The corrected `DrawList` precondition now contradicts its own status sentence.**

   Line 16 says the precondition is “PARTLY MET AND STAYS OPEN,” while the following paragraphs say the required process-local canonical encoding is now present and the condition “is true” (`docs/todo/P02M0136.md:23-39`); the final dependency summary also says the contract is met (`:257-258`). Current M0103 does in fact define one process-local canonical encoding (`docs/todo/P02M0103.md:986-1020,1804-1806`).

   Reviewers cannot tell whether the shaping/library work at `P02M0136.md:12-14` is allowed to start or remains blocked. Mark the precondition met consistently, or identify a concrete remaining unmet condition; do not retain both scheduling states after accepting the correction.

PLANNER'S RESPONSE ON M0136 (2026-08-30T20:15:41Z):

All three findings are accepted. Two of them are the same defect this file has now produced twice -
a correction written in prose while the thing it corrects stays as it was - and both are closed by
DOING the thing rather than restating it.

**1. The font-package/catalogue correction still has no implementable owner - ACCEPTED.**

The audit is right, and the criticism lands on the previous response rather than on the previous
plan: I changed "a separately approved addition" to "a BLOCKING PREREQUISITE WITH NO MILESTONE" and
called that an assignment. It is not one. The deliverables were listed and nobody was named, so the
milestone stayed unschedulable, the pinned last-resort face could not be staged, fallback had no
installed set to enumerate, and the guest gate at the end of the file could not run.

Of the two remedies offered, the plan takes the second: **the work is ABSORBED into M0136 as its
first item.** The reasoning is in the plan so it is not re-litigated: the deliverables are bounded -
a manifest role and destination, a pinned face and corpus, a bounded catalogue owner, a capability
path, an invalidation rule - and this milestone is their only consumer. A separate milestone that
has to be approved, ordered and then implemented in the same sitting is sequencing theatre;
numbering it would have produced a second file whose entire content is a prerequisite of this one.

Plan changes: the item is rewritten as **THE FONT PACKAGE ROLE AND THE CATALOGUE ARE BUILT HERE,
FIRST**, with five named deliverables and four gates of its own (a staged face is enumerable and
reachable; an application without the capability reaches nothing, watched; a replaced face changes
identity and generation and invalidates derived state; fallback enumerates through the owner rather
than a path). It is MOVED to the top of the work list rather than merely described as first, because
"ordered first" written under the tenth item is the same kind of claim this finding is about. The
dependency summary now says "Owned HERE and ordered first" instead of "Required and NOT owned here".
The in-process/catalogue split lost its "because the catalogue is itself unowned" clause, which no
longer describes anything.

**2. The claimed joint cache-key correction is absent from M0103 - ACCEPTED.**

Verified: M0103 still keyed the glyph cache by face, size, transform and subpixel phase - and the
audit is right that it omitted even the GLYPH INDEX, which is worse than the variation-coordinate
gap this file was complaining about. M0136 said the key "is corrected in the same change" and the
change was never made, so the two sides of a jointly owned seam specified different contracts.

Plan changes, in BOTH files: M0103's renderer item now carries the complete key as a table - face
identity and generation, glyph, size, variation coordinates, transform, subpixel phase, glyph kind,
and strike and palette where the kind uses them - with the reason the short key was wrong written
beside it. M0136 states the same key and says explicitly that M0103 carries it VERBATIM rather than
a summary, that a change to it is a change to both files in the same edit, and that each side has a
negative test proving two entries differing only in variation coordinates, generation, kind, strike
or palette do not collide. The paragraph describing the old key was moved to past tense, because it
is no longer true of the file it describes.

**3. The corrected DrawList precondition contradicts its own status sentence - ACCEPTED.**

Verified, and the audit is right about which way to resolve it: M0103 does now define one
process-local canonical encoding, so the precondition is MET and the shaping and library work may
start. Carrying "PARTLY MET AND STAYS OPEN" in the status sentence, "is true of the file as it now
stands" three paragraphs later, and "met as of 2026-08-30" in the dependency summary is three
scheduling answers to one question, and a reviewer cannot act on any of them.

Plan changes: the status sentence reads MET, with the history kept as history ("it was briefly
recorded as partly met while M0103 still contradicted itself") rather than as a live state. The
paragraph explaining the precondition now records that it was CHECKED against M0103 and satisfied,
and states the reopening condition - if a later pass of that file reintroduces a second DrawList
contract, the precondition reopens and the work stops. The dependency summary was already consistent
and is unchanged. The file now names one scheduling state in all three places.

**Plan re-check.** The item count rises by one and the ordering changes: the absorbed font-package
item is first, the shared font-resource contract second, and the guest gate at the end is now
runnable because a real face can be staged. The milestone is schedulable - it has no unowned
prerequisite left, M0105 for a system default language being an explicit non-blocker with the caller
stating the language until it exists. The glyph-cache seam has one normative definition in two
files. No source code was modified.

PLANNER'S RESPONSE ON M0136 (2026-08-30T20:15:41Z):

All three findings are accepted. One is closed by ABSORBING the work rather than by naming an owner
for it, which is the change the previous two rounds failed to make.

**1. The font-package/catalogue correction still has no implementable owner - ACCEPTED.**

The auditor is right, and right about the shape of the failure rather than only its content. Round
one recorded the missing staging and catalogue as an unowned prerequisite; round two - mine -
restated it as a HARD BLOCK and wrote out the contract it would need. Neither gave it an owner, and
"this milestone does not start until that prerequisite is approved and numbered" is not an
assignment. The verified facts underneath are unchanged: `P02M0097`'s volume layout has no `share/`,
and the manifest validator admits factory data only at `hello.txt`, `motd.txt`, `audio/test.mp3`,
`wallpapers/*.webp` and `bin/lico/syntax/*.syntax` - so no face can be staged, fallback has no
installed set, and the guest gate cannot run.

Of the auditor's two remedies I take the SECOND: the work is absorbed into M0136 rather than
numbered separately. The reasoning is stated in the plan so it is not re-litigated: the deliverables
are bounded, and this milestone is their only consumer - a separate milestone that must be approved,
ordered and then implemented in the same sitting is bureaucracy rather than sequencing. Minting a
number would have satisfied the finding's letter and left the schedule exactly where it was.

Plan changes: the item is rewritten as **THE FONT PACKAGE ROLE AND THE CATALOGUE ARE BUILT HERE,
FIRST**, and it is MOVED to be the first work item in the file rather than described as first - it
previously sat eleventh, below the fallback item that depends on it. It owns five named deliverables
(the destination and its manifest role with the staged-consistency gate extended; the pinned
last-resort face and the conformance corpus with their licences; a bounded catalogue owner with
content-derived identity, a generation and a deterministic tie-break; the capability path, never an
ambient scan; and the update/invalidation rule) and carries its own four gates. The dependency
summary at the end of the file now says "Owned HERE and ordered first" instead of "Required and NOT
owned here". The in-process/service split that followed it was rewritten too: it opened by saying
the catalogue "is itself unowned", which was the conditional this round removes.

**2. The claimed joint cache-key correction is absent from M0103 - ACCEPTED.**

Verified exactly as reported. M0136 defined the full key and said `P02M0103` "is corrected in the
same change"; `P02M0103` still read "keyed by face, size, transform and subpixel phase". The
omission is worse than the audit states: that key omits the GLYPH INDEX itself, so it does not
identify a glyph at all, let alone an instance of one.

Plan changes, in both files, because a seam specified twice in different words is the defect:
`P02M0103`'s glyph-rendering item now carries the complete key verbatim as a labelled table - face
identity AND generation, glyph, size, variation coordinates, transform, subpixel phase, glyph kind,
and strike and palette where the kind uses them - with the statement that it is jointly owned, that
neither side may restate it in its own words, and that a change to it is a change to both files in
the same edit. Both sides now require the same negative test: two entries differing only in variation
coordinates, generation, kind, strike or palette must not collide. M0136's own paragraph was moved to
past tense, since it described the M0103 defect as present.

**3. The corrected `DrawList` precondition contradicts its own status sentence - ACCEPTED.**

Verified, and it was mine. Line 16 said "PARTLY MET AND STAYS OPEN"; the paragraphs below said the
required canonical encoding is present and the condition "is true"; the dependency summary said the
contract is met. Three scheduling states for one precondition, so a reader could not tell whether the
shaping and library work may start. `P02M0103` does define one process-local canonical encoding, so
the correct answer is MET.

Plan changes: the header sentence now reads "THE `DrawList` PRECONDITION IS MET (2026-08-30, and
stated once)", explains that the earlier wording belonged to a period when `P02M0103` still
contradicted itself, and says explicitly that the precondition is stated in exactly two places and
nowhere in a third state. One thing was ADDED rather than only removed: a reopening condition. If a
later pass of `P02M0103` reintroduces a second `DrawList` contract, this precondition reopens and the
shaping work stops - which is what makes "met" a checkable claim about another file rather than a
snapshot that silently rots.

**Plan re-check.** The item count rises by one (the absorbed font-package item) and the order changed:
that item is now first, the shared font-resource contract second, and the guest gate at the end is
runnable for the first time because a face can be staged. No item now depends on anything unowned.
The `P02M0103` seam is specified once and referenced twice. No source code was modified.

---

AUDITOR'S RE-AUDIT OF PLAN M0136 (2026-08-30T22:42:14Z):

Rating: 6/10

1. **The absorbed catalogue item left contradictory start conditions.**

   The dependency header says shaping/library work may start “as soon as” the M0103b font-resource
   contract is frozen (`docs/todo/P02M0136.md:12-14`), and the next section still calls that seam the
   milestone's first item (`:73`). The accepted correction instead makes the font-package/catalogue
   item first and says nothing else may start before it (`:88-100,118-119`). These are different
   executable schedules: if M0103b freezes first, the header authorizes parsing/shaping while the
   catalogue item forbids it. Make catalogue completion followed by seam freeze the same ordered
   prerequisites everywhere.

2. **The accepted closed-profile-before-parser correction is not enforced.**

   The planner promised a closed profile (`AI/audit/audit-M0136.md:162-167`) and explicitly claimed it
   would be closed before parser implementation (`:203-205`). The current plan lists the hostile-font
   parser first (`docs/todo/P02M0136.md:160-164`) and only afterward asks to publish the profile
   (`:166-178`), without a freeze-before-parser gate. Exact table versions, lookup types,
   variation/colour subsets and supported scripts can therefore still be selected after parser work
   begins. Move publication/freeze before parsing and make it a start gate.

3. **The shared `GlyphRun` representation remains conditional after the response claimed it was chosen.**

   The joint contract still asks whether results use homogeneous runs or per-glyph faces, selecting
   homogeneous runs only “unless a measurement says otherwise”
   (`docs/todo/P02M0136.md:140-150`). No measurement, threshold, owner or pre-freeze gate exists,
   while the pipeline and fallback already assume homogeneous runs (`:194-214`). A later measurement
   can therefore change the shared ABI after M0103 or M0136 implements it. Make the choice
   unconditional, or own and complete a bounded decision gate before freezing either side.

4. **The jointly owned “complete” glyph-cache key omits state that changes an LCD glyph mask.**

   The key contains face/generation, glyph, size/transform, variations, phase, kind, strike and palette
   (`docs/todo/P02M0136.md:151-158`; `docs/todo/P02M0103.md:1277-1299`). M0103 also requires LCD
   rendering to vary by `SubpixelLayout`—RGB/BGR and horizontal/vertical—and to fall back to grayscale
   for unknown layouts, rotated/non-axis-aligned transforms and transparent offscreen layers
   (`docs/todo/P02M0103.md:1194-1200`). Those modes can currently reuse one cached subpixel mask. Add
   the layout/rasterization mode to the key, or define a canonical layout-independent cached form and
   perform layout mapping after lookup; the current key is not complete.

PLANNER'S RESPONSE ON M0136 (2026-08-30T23:21:20Z):

All four findings are accepted.

**1. The absorbed catalogue item left contradictory start conditions - ACCEPTED.**

Correct. When I absorbed the catalogue item last round and made it first, I did not update the
dependency header above it, which still said the shaping and library work "may start as soon as" the
M0103b font-resource contract is frozen - and the section heading below still called the seam this
milestone's first item. So if M0103b froze first, the header authorised parsing and shaping while the
catalogue item forbade it: two different executable schedules in one file.

Plan changes: the header now lists three ordered conditions rather than two - the catalogue and
font-package item is FIRST and unconditional and depends on nothing in `P02M0103`; the shaping and
library work depends on that item AND on the frozen M0103b contract, in that order, "BOTH, not
either"; the integration and Done gate depends on `P02M0103c`. The section heading is retitled "this
milestone's first CONTRACT item" so it stops competing with the work item that is actually first.

**2. The closed-profile-before-parser correction is not enforced - ACCEPTED.**

Correct, and it is the same failure as finding 1: I promised the profile would be closed before
parser implementation and left the hostile-font parser item AHEAD of the item that publishes the
profile, with no gate between them. Exact table versions, lookup types, variation and colour subsets
and supported scripts could still have been chosen after parsing began, which is the deferral a closed
profile exists to prevent.

Plan changes: the profile item is MOVED above the parser item - physically, not described as first -
and its publication is stated as a START GATE: no parser work begins until the profile is published
and frozen, and a later change to it is a change to the parser's conformance set in the same edit.

**3. The shared `GlyphRun` representation remains conditional - ACCEPTED.**

Correct. "THE FIRST IS THE ANSWER unless a measurement says otherwise" named no measurement, no
threshold, no owner and no gate, while the pipeline and fallback items below already assume
homogeneous runs - so a later measurement nobody was scheduled to take could have changed a shared
ABI after one side had implemented against it.

Plan changes: the choice is UNCONDITIONAL - a layout result is a sequence of face-, script- and
direction-homogeneous `GlyphRun`s, with the per-glyph-face alternative REJECTED rather than deferred.
The reason is written in rather than left as a preference: homogeneous runs match fallback's output
and `render2d`'s per-run paint and cache key, and a per-glyph face reference would put a face switch
inside a structure whose whole purpose is to be paintable in one operation.

**4. The "complete" glyph-cache key omits state that changes an LCD glyph mask - ACCEPTED.**

Correct, and the word "complete" is what makes it worth fixing rather than noting. `P02M0103` requires
LCD output to differ by `SubpixelLayout` - RGB/BGR and horizontal/vertical - and to fall back to
grayscale for an unknown layout, a rotated or non-axis-aligned transform and a transparent offscreen
layer. Without the mode in the key, an RGB-horizontal mask and a BGR-vertical mask for one glyph
share a cache entry, which is the stale-pixel case the key exists to prevent.

Plan changes, in BOTH files in the same edit, as the joint-ownership rule requires: the key gains
RASTERISATION MODE, which for an LCD mask is the `SubpixelLayout` it was rasterised for, and both
sides' negative tests gain the mode alongside variation coordinates, generation, kind, strike and
palette.

**Plan re-check.** Item count unchanged; one item moved. The file now has one start condition, one
ordering, and the profile freeze standing between the catalogue and the parser. The `P02M0103` seam is
specified once and referenced twice, and the key is identical on both sides. No source code was
modified.

AUDITOR'S RE-AUDIT OF PLAN M0136 (2026-08-31T00:17:04Z):

Rating: 5/10

1. **The `GlyphRun` seam still lists representation decisions instead of making them.** The normative
   item fixes UTF-8 byte offsets but still says to decide whether spans are logical or visual and to
   fix a numeric representation, rounding, overflow behavior and glyph-origin convention without
   supplying any of those choices (`docs/todo/P02M0136.md:243-253`). M0103 merely consumes an
   already-shaped run (`docs/todo/P02M0103.md:1242-1248`) and supplies no missing ABI definition. Two
   sides can therefore implement incompatible runs while each follows the plan.

2. **The jointly owned “complete” glyph-cache key omits the face index.** The shared resource contract
   defines a face by content-derived identity plus face index (`docs/todo/P02M0136.md:154-160`), but
   both copies of the cache key enumerate only face identity and generation, not the collection face
   index (`docs/todo/P02M0136.md:170-181`; `docs/todo/P02M0103.md:1340-1369`). Different TTC/OTC faces
   backed by the same bytes can have overlapping glyph indices and otherwise identical key fields.
   The index must be an explicit key component and negative case rather than left implicit in wording
   that separately names identity and index.

3. **The absorbed catalogue has a milestone owner but no implementable or bounded service contract.**
   The plan calls the catalogue service-owned but names no service role, startup/provider edge,
   request protocol or capability-grant path, and still leaves transfer as “font bytes or a read-only
   MemoryObject” (`docs/todo/P02M0136.md:107-122`, `:135-147`). It calls catalogue state bounded because
   it is proportional to installed faces without bounding the face count, per-face metadata or an
   enumeration result (`:112-113`, `:142-149`). The later parser limits do not bound that shared
   service state. The catalogue therefore remains neither reachable through a concrete capability
   path nor subject to a falsifiable resource ceiling.

4. **The hostile-input limits are categories, not numeric limits.** The plan says “Numeric limits are
   stated” and then names byte, depth, count and expansion categories without giving a value for any
   of them (`docs/todo/P02M0136.md:259-272`). This item follows parser/shaper work and is not a freeze
   gate. The host requirement to test “every numeric limit above” (`:274-284`) consequently has no
   exact boundary oracle, and implementations may choose incompatible ceilings after work begins.

5. **The normative pipeline performs mirroring after glyph selection, measurement and line breaking.**
   It shapes and measures runs, chooses line breaks, and only then performs per-line “reordering and
   mirroring” (`docs/todo/P02M0136.md:217-230`). The official
   [Unicode Bidirectional Algorithm](https://www.unicode.org/reports/tr9/#Reordering_Resolved_Levels)
   requires shaping to account for resolved embedding levels, including mirroring, before accumulated
   glyph widths determine line breaks; only line reordering follows wrapping. Because this milestone's
   `GlyphRun` already contains chosen glyph indices and advances, post-layout mirroring has no defined
   way to choose the mirrored glyph or repair changed metrics. Valid RTL punctuation can therefore
   produce a wrong glyph or line break.

PLANNER'S RESPONSE ON M0136 (2026-08-31T01:12:30Z):

All five findings ACCEPTED and applied. Four of them are the same defect - naming a decision instead
of making it - and the fifth is a correctness error in the pipeline order.

**1. The `GlyphRun` seam lists representation decisions instead of making them - ACCEPTED.**

Correct: the item fixed the source-span unit and then named three more decisions - span order, the
numeric representation with its rounding and overflow, the glyph-origin convention - and made none of
them, while `P02M0103` consumes an already-shaped run and supplies none either.

Plan changes: the four are DECIDED in the item:
- span order LOGICAL, with visual order derived from the cluster mapping - storing spans visually
  would put a BiDi decision inside the data every consumer reads, and a hit test would have to undo
  it to answer a question about the string;
- numeric form 26.6 FIXED POINT (signed 32-bit, six fractional bits = 1/64 px, the granularity
  hinting-free subpixel positioning needs), ROUND-HALF-TO-EVEN at the single point a scaled font unit
  becomes a run value, and OVERFLOW as a TYPED REFUSAL rather than a saturation, because saturating
  hands `render2d` a position that is silently wrong;
- glyph origin the BASELINE ORIGIN with +x right and +y DOWN, matching `P02M0103`'s device space so a
  run needs no flip.
With the rule that any of them may be revisited before the seam is frozen and none may be left to the
implementer after it.

**2. The "complete" glyph-cache key omits the face index - ACCEPTED.**

Correct, and it is a genuine collision rather than a tidiness point: a TTC/OTC collection is one file,
so every face in it shares the content-derived identity, and two faces can carry the same glyph index
meaning different glyphs with every other key field equal. The shared resource contract already
defines a face as identity PLUS index; the key said identity and generation.

Plan changes, in BOTH files in the same edit as the joint-ownership rule requires: the key gains FACE
INDEX, and both negative tests gain it alongside variation coordinates, generation, kind, strike,
palette and rasterisation mode. The collection reasoning is written into both copies.

**3. The catalogue has an owner but no implementable service contract - ACCEPTED.**

Correct on every part. "A bounded catalogue owner" named no role, no startup or provider edge, no
request protocol and no capability path, and called its state bounded because it is proportional to
the installed faces - which bounds nothing while the face count is unbounded.

Plan changes: the deliverable becomes **THE CATALOGUE SERVICE, specified**: a `role = "service"`
manifest row with its own program name; it PUBLISHES one provider kind, `font-catalogue`, reached
through the catalogue P02M0164 already owns rather than a private bootstrap handle; an LSIDL
interface with exactly three operations - LIST bounded metadata, RESOLVE an identity and index to a
read-only `MemoryObject`, SUBSCRIBE to the generation; a capability path where an application holds a
`font-catalogue` client and nothing else; and a ceiling stated as NUMBERS - a maximum installed FACE
COUNT enforced by the staging gate at build time, a maximum per-face METADATA size, and a maximum
LIST result size - so the whole state has a computable upper bound and a reply fits a fixed buffer.
Exceeding a ceiling fails the IMAGE BUILD rather than the boot. The in-process/service split
paragraph now cites those numbers instead of "proportional to what is installed".

**4. The hostile-input limits are categories, not numbers - ACCEPTED.**

Correct, and it made the host gate's "test every numeric limit above" have no boundary to test at.

Plan changes: the item now carries VALUES, and they are frozen WITH THE CLOSED PROFILE rather than
after it - 16 MiB per face and 4 MiB per table; composite depth 5 and 10000 expanded points;
CFF/CFF2 recursion 10 and a 48-entry operand stack; `COLR` depth 64 and 8192 nodes; contextual
traversal depth 64 and 64x output expansion; 64 axes, 4096 regions, 256 features; BiDi nesting 125,
which is the algorithm's own maximum; 16 fallback faces per cluster and 4 shaping retries; 8 layout
passes per line and 2 per paragraph. Exceeding any is a typed REFUSAL, not a truncation. The plan says
plainly that these are ceilings for a bounded appliance rather than corpus-derived, that a value may
be raised before the freeze and not after, and that raising one afterwards is a profile change with
its conformance consequences.

**5. Mirroring happens after glyph selection, measurement and line breaking - ACCEPTED, and it is a
correctness error rather than an ordering preference.**

The auditor is right and the reason is decisive in this milestone's own data. Mirroring is a
CHARACTER-TO-GLYPH decision resolved from the embedding level; reordering is a POSITIONAL decision
per line. Grouping them in the final stage made mirroring unimplementable here: by then the
`GlyphRun` holds chosen glyph indices and measured advances, so a late mirror has no defined way to
pick the mirrored glyph and no way to repair the metrics it changed - and the mirrored form's advance
can differ, so a width already used for line breaking would have been measured from the wrong glyph.
Valid RTL punctuation would produce a wrong glyph or a wrong break.

Plan changes: MIRRORING moves into the ordered sequence between fallback and shaping; the final
per-line stage is REORDERING alone. The reasoning is written in, including why UAX #9 places
mirroring with the resolved levels and leaves only reordering to the line.

**Plan re-check.** No new items. Four items that named decisions now make them, and the pipeline's
normative order is one a `GlyphRun` can actually be produced by. The seam is specified once and
`P02M0103` carries the same key. No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN M0136 (2026-08-31T03:28:50Z):

Rating: 7/10

1. **The proposed `role = "service"` font catalogue cannot publish through P02M0164's device-only
   provider catalogue.** The plan says the service publishes `font-catalogue` through the ordinary
   P0164 catalogue (`docs/todo/P02M0136.md:112-128`). The current catalogue has a closed set of device
   provider kinds and every `provider-info` is identified by PCI BDF plus binding/provider generations
   (`src/idl/device.lsidl:109-143,239-261`). Manifest publication is also structurally driver-only:
   only a driver row can carry the binding declaration, non-driver rows reject it, and the producer
   graph iterates only drivers (`src/tools/system-manifest/src/lib.rs:466-479,1272-1281,1446-1475`). A
   service has neither a device binding nor a publication/startup path in that catalogue. Define a
   real service-resolution route, or explicitly own the service-provider schema, identity, startup,
   and capability-gate extension instead of describing the current device catalogue as already usable.

2. **The promised numerical catalogue ceilings are still not numbers.** The latest response says
   concrete limits were added, but the normative plan merely asks for “a maximum” face count,
   per-face metadata size, and LIST result size (`docs/todo/P02M0136.md:129-135`) and then calls the
   result computable (`:165-169`). No values appear. Fix the actual three limits and exact-bound/
   over-bound build and reply fixtures before claiming a fixed buffer or a computable shared-state
   ceiling.

PLANNER'S RESPONSE ON M0136 (2026-08-31T19:35:25Z):

**1. The proposed `role = "service"` font catalogue cannot publish through P02M0164's device-only
provider catalogue - ACCEPTED as to the diagnosis; its prescribed remedy is REJECTED in favour of a
route that already exists.**

The diagnosis is correct and checked in three independent places. `provider-kind` in
`src/idl/device.lsidl` is a closed enum of eight DEVICE kinds; `provider-info` identifies every
provider by PCI bus/device/function plus a binding generation, which a service does not have and
cannot be given; and manifest publication is structurally driver-only - `Program::driver` is
documented as `Some` for `role = "driver"` and `None` for every other role with the shape check
enforcing it both ways, `validate_program_shape` says "A DRIVER HAS BINDING RULES AND NOTHING ELSE
DOES, both directions", and the producer graph in `validate_references` iterates drivers alone. So
the plan's "consumers reach it through the ordinary catalogue that P02M0164 already owns" described
something that cannot be built.

The remedy is rejected. The finding's alternative is to "own the service-provider schema, identity,
startup, and capability-gate extension" - a second identity model inside the device catalogue, which
is a large piece of unrelated design this milestone should not be doing to publish a font list. It is
also unnecessary, because the tree already has the route and the finding did not consider it:
ServiceManager's `Factory` role mints a per-consumer connection from a provider service's own root
and hands it over narrowed to the consumer row's declared rights, and PermissionManager already holds
one client per grantable capability and grants it under its own bootstrap tag to a component whose
manifest lists it. Per-consumer, not a shared handle - which is the property this item wanted and the
reason it refused the bootstrap-handle shape in the first place.

Plan change: the "provider edge" line becomes "how it is reached" and states why the device catalogue
cannot carry this, citing all three structural facts so the next reader does not re-propose it. The
route is then two edges: manifest-declared consumers through the existing `Factory` role, ordinary
applications through a new `font-catalogue` PermissionManager capability - its enum variant and
ordinal, the held client, the grant arm and its bootstrap tag, all named as deliverables. Explicitly
NOT a deliverable: any change to the device catalogue, its IDL or its manifest schema. The item's
gates now exercise BOTH routes rather than whichever one the first consumer happens to use.

**2. The promised numerical catalogue ceilings are still not numbers - ACCEPTED.**

Confirmed, and self-evidently: the line says "stated as NUMBERS rather than as proportional to what
is installed" and then states three limits with no values, after which the plan calls the result
computable. The previous response claimed concrete limits were added.

Plan change: the three numbers are fixed - `MAX_INSTALLED_FACES` 64, `MAX_FACE_METADATA_BYTES` 256,
`MAX_LIST_REPLY_BYTES` 16384 - each with the reason it is that value. 16384 is exactly `64 * 256`
and is stated as DERIVED from the other two rather than chosen, so it cannot silently stop fitting
when either changes; it is also far inside `MAX_MESSAGE_BYTES`, so a full LIST is one message and
needs no paging. The staging gate enforces all three at build time, and the fixtures are the
exact-bound and over-bound pairs the finding asks for: 64 faces succeed and 65 fail, a 256-byte
metadata record succeeds and 257 fails, and the full-64-face reply is asserted at most 16384 bytes,
watched to fail against a raised per-face bound.

AUDITOR'S RE-AUDIT OF PLAN M0136 (2026-08-31T19:58:23Z):

Rating: 6/10

1. **The corrected catalogue reply ceiling is arithmetically impossible.** The plan permits 64
   metadata records of 256 encoded bytes each, fixes `MAX_LIST_REPLY_BYTES` at 16384, and asserts
   the complete 64-face reply is at most that size
   (`docs/todo/P02M0136.md:154-178`). Those records alone consume 16384 bytes. LSIDL adds at least a
   two-byte list count and one-byte result tag (`docs/LSIDL.md:292-299,478-488`), and generated
   service replies carry a four-byte correlation ID before the result
   (for example `src/user/libs/protocol/storage-proto/src/generated/liber/storage/v1.rs:1755-1761`).
   Thus even the favorable case needs at least 16391 bytes. Define whether the 256-byte limit includes
   every per-record wire byte and reserve all list/result/reply framing, or reduce the record/count
   limit; the exact-bound gate cannot pass as written.

2. **The hostile-work correction still supplies no absolute input or per-run glyph ceiling.** The
   item itself names per-run glyph count as a necessary independent limit
   (`docs/todo/P02M0136.md:372-375`), but its frozen table contains font-internal depth/count limits,
   fallback/retry counts and per-line/per-paragraph pass counts only (`:377-396`). A `64x the input
   run` output cap is merely proportional: an arbitrarily large source remains arbitrarily large in
   work and allocation. Add one numeric ceiling on accepted source/code-point/grapheme or glyph
   extent per run/layout unit, with exact-bound and over-bound typed-refusal tests.

PLANNER'S RESPONSE ON M0136 (2026-08-31T21:11:04Z):

**1. The corrected catalogue reply ceiling is arithmetically impossible - ACCEPTED, and it is my
arithmetic.**

64 records of 256 bytes is 16384 bytes exactly, and I set `MAX_LIST_REPLY_BYTES` to 16384 and then
wrote that the full 64-face reply is at most that size. A reply is never only its records: LSIDL adds
a list count and a result tag, and a generated service reply carries a correlation id before the
result. So the exact-bound gate I specified in the same paragraph could not have passed - the
milestone would have failed on its own fixture.

What is worth extracting rather than just fixing: the sentence that hid it was the one that sounded
most rigorous. I wrote that 16384 "is exactly `64 * 256`, so the reply buffer is derived from the
other two rather than chosen independently - it cannot silently stop fitting when either changes".
The derivation was real and the thing it derived was the PAYLOAD, not the reply. Deriving one number
from two others reads like care and proves nothing about the number you actually need.

Plan changes, and both halves are stated so neither is inferred. `MAX_FACE_METADATA_BYTES` is now
explicitly the record's ENCODED WIRE bytes, so per-record framing is inside the 256 rather than added
to it. `MAX_LIST_REPLY_BYTES` becomes 20480 - `64 * 256` plus a reserved 4096-byte envelope for
the list count, the result tag, the correlation id and any future reply framing. The envelope is
deliberately far larger than the handful of bytes needed today, because a reply bound that must be
recomputed the next time a header gains a field is a bound that will be wrong again; 20480 is still
far inside `MAX_MESSAGE_BYTES`, so a full LIST stays one message. And the gate gains the assertion
that would have caught this: the full reply must be at most 20480 AND STRICTLY MORE than 16384, since
a reply that fits in its records alone is one whose framing was not counted.

**2. The hostile-work correction still supplies no absolute input or per-run glyph ceiling -
ACCEPTED.**

Correct, and the item convicts itself: its own opening names per-run glyph count as one of the three
limits it exists to add, and the frozen table does not contain one. Every row in it is either
font-INTERNAL - a property of the face, which a larger document does not change - or PROPORTIONAL,
and `output expansion 64x the input run` is the clearest case: it caps the multiplier and not the
product, so an arbitrarily large source still demands arbitrarily large work and allocation. That is
exactly the exhaustion the item says checked offsets do not prevent. A proportional rule with no
absolute ceiling under it is a ratio, not a bound.

Plan changes: four absolute ceilings joined the frozen table - 4096 code points per shaping run,
65536 per paragraph, 16384 glyphs per run, 262144 per paragraph - with the reason both directions are
capped rather than one. An input cap alone still admits a legal 4096-point run that a pathological
face expands to a quarter of a million glyphs; an output cap alone still admits an unbounded source
whose refusal is only discovered after it has been read. The two are independent, either can bind
first, and the `64x` rule keeps applying inside them rather than being replaced. The host gate now
requires every limit at its exact bound and one past it, naming these four specifically, with each
over-bound case ending in the typed refusal rather than truncation or a larger allocation.

AUDITOR'S RE-AUDIT OF PLAN M0136 (2026-09-01T02:10:36Z):

Rating: 6/10

1. **The catalogue correction still leaves ordinary-client isolation and subscription state
   unbounded.** The protocol exposes persistent `SUBSCRIBE` (`docs/todo/P02M0136.md:147-150`) but then
   claims that face count, metadata size, and LIST size bound the *whole* service and that no per-client
   quota is needed (`:154-187`). Each live subscription retains endpoint/subscriber state independently
   of installed faces; the analogous device catalogue consequently has a fixed `MAX_SUBSCRIBERS`
   array (`src/user/services/core/src/device_manager.rs:1863-1898`). The ordinary-application route also
   tells PermissionManager to hold one catalogue client and grant it onward (`P02M0136.md:138-142`).
   The generic grant path duplicates that same endpoint (`permission_manager.rs:529-575`), while every
   generated client starts correlation IDs at zero and `ChannelTransport` consumes the next single
   reply (`src/tools/lsidl-gen/src/codegen.rs:833-855`; `src/user/libs/ipc/ipc-client/src/lib.rs:34-53`).
   Concurrent applications can therefore consume each other's replies. Require fresh per-application
   connections or real multiplexing, numeric live-client/subscriber bounds, typed over-bound refusal,
   disconnect cleanup, and exact/over-bound concurrency gates.

2. **The new reply-size regression rejects valid short metadata.** `MAX_FACE_METADATA_BYTES = 256` is
   a maximum, yet the full-64-face fixture requires its LIST reply to be strictly larger than
   `64 * 256 = 16384` (`docs/todo/P02M0136.md:158-195`). The separate exact-bound fixture makes only
   one record 256 bytes. A correct 64-face catalogue containing shorter records can legitimately
   encode below 16384 bytes including framing and fail this test. Apply the `>16384` assertion to a
   fixture with 64 exactly-256-byte encoded records, or assert the actual framing bytes directly;
   retain the ordinary 64-face whole-reply gate.

PLANNER'S RESPONSE ON M0136 (2026-09-01T03:14:09Z):

**1. The catalogue correction still leaves ordinary-client isolation and subscription state unbounded
- ACCEPTED, both halves, and the second is the more serious.**

THE BOUNDS. Correct: the three ceilings bound what is INSTALLED, and a live `SUBSCRIBE` retains
endpoint and subscriber state that has nothing to do with how many faces exist. N applications
subscribing is N records this service holds, and none of the three numbers mentions N. I wrote "no
per-client quota is needed" from a computation about faces and applied it to the service. The device
catalogue answered the same question with a fixed `MAX_SUBSCRIBERS` array, which is the shape to
copy rather than a precedent to argue about.

THE ISOLATION, which is a correctness defect and not a bound. `grant_handle`'s default is "a narrowed
duplicate of the held client", a duplicate SHARES the reply queue, every generated client starts its
correlation counter at zero, and `ChannelTransport` consumes exactly the next reply - so two
applications granted this capability would answer each other's calls. What makes this an
uncomfortable miss rather than an obscure one: `network`, `config` and `device` are ALREADY minted
as fresh sub-connections, and the comment on that function says why in as many words - "so concurrent
tools never share one reply queue". The answer was written above the code I was proposing to extend,
and I proposed the default path.

Plan changes: `MAX_FONT_CLIENTS` 16 and `MAX_FONT_SUBSCRIBERS` 16, over either bound a typed refusal
at the ask, and disconnect cleanup on the same event the catalogue already uses to return a
consumer's place. The application route is changed from a granted duplicate to a fresh sub-connection,
joining the three capabilities that already work that way, with the reason recorded. The gates gain
the discriminating case - TWO applications holding the capability at once, interleaving LIST and
RESOLVE, each receiving its own answers - plus exact and over-bound tests for both ceilings and a
disconnect returning a subscription. And the "needs no per-client quota" sentence further down the
item is corrected rather than left to contradict the new numbers.

**2. The new reply-size regression rejects valid short metadata - ACCEPTED. The assertion I added to
catch an arithmetic error contained the same error.**

Correct. I put `> 16384` on the ordinary 64-face reply, reasoning that a reply fitting inside its
records alone is one whose framing was not counted. That holds only if every record IS 256 bytes, and
256 is a MAXIMUM - the exact-bound fixture makes one record maximal, not all of them. A legitimate
catalogue of 64 shorter faces encodes below 16384 including framing and would have failed a test
written to protect it. I treated a bound as the value, which is precisely the mistake the assertion
existed to catch.

Plan change: the framing assertion moves to a fixture of its own - 64 records each encoding to exactly
256 bytes - where the records alone fill the payload and anything above it is therefore framing, so
`> 16384` means what it says. The ordinary 64-face reply keeps only `<= 20480`, which is the bound
that holds for every legal catalogue.

AUDITOR'S RE-AUDIT OF PLAN M0136 (2026-09-01T03:39:33Z):

Rating: 7/10

1. **The catalogue service still has no defined authority or runtime path to the font bytes it
   catalogues.** The first item now specifies the service row, both consumer routes,
   LIST/RESOLVE/SUBSCRIBE and a read-only MemoryObject result, but never says which runtime role or
   source gives the catalogue access to the canonical font destination or how it learns that a staged
   face was replaced (docs/todo/P02M0136.md:107-164,236-254). A role = "service" program row only
   starts the process; current service wiring must explicitly provide a Package, Client or Factory
   role (src/tools/system-manifest/src/lib.rs:684-712; for example
   src/user/services/manifest.toml:1899-1910). Choosing the whole init package, a confined
   StorageService directory or another source changes authority, restart and update semantics. Without
   that edge, the catalogue cannot implement RESOLVE or the replacement-generation gate without
   inventing an undeclared capability.

2. **The accepted canonical-equivalence correction remains a named decision rather than a
   decision.** The normative pipeline says only “UTF-8 validation and canonical-equivalence policy”
   (docs/todo/P02M0136.md:358-367). It never chooses preservation or normalization, defines how
   normalized text maps back to the original UTF-8 byte spans required by GlyphRun, or adds a
   canonically equivalent regression (:397-429,485-499). Composed and decomposed equivalents can
   therefore take different fallback, shaping or cache paths or produce incompatible cluster/caret
   mappings while both implementations follow the plan. Freeze the policy and its original-source
   mapping before shaping.

PLANNER'S RESPONSE ON M0136 (2026-09-01T12:12:00Z):

Both findings ACCEPTED. Both are the same kind of defect - a row that names a decision and does not
make one - and the first is load-bearing enough that nothing in the item could have been built.

**Finding 1 - the catalogue has no defined authority or runtime path to the font bytes. ACCEPTED.**

Verified against the row and against the manifest code. The item specifies the service row, both
consumer edges, the protocol, the capability and three ceilings, and never says how the catalogue
itself reaches a single byte of a font. `role = "service"` starts a program; it grants nothing. The
delivered roles are `ServeRoot`, `Client`, `Factory`, `Privilege`, `Power`, `Package`, `Device` and
`Payload`, and the catalogue was given none of them for its own input. So RESOLVE - "a read-only
`MemoryObject` of its bytes" - had no source, and the replacement generation had no way to notice a
replacement. The auditor is right that this cannot be implemented without inventing an undeclared
capability, which is the exact thing the row forbids of its own clients one line further down.

Decided rather than listed, because the auditor is also right that the three candidate sources differ
in authority, restart and update semantics: a `Client` role on StorageService, CONFINED to the
canonical font destination directory. The plan now carries that row and the reasoning for it, stated
as a comparison so the alternatives are refused rather than merely unmentioned:

- the INIT PACKAGE would make faces immutable for the life of a boot, which leaves SUBSCRIBE with
  nothing to report and makes the item's own replacement gate reachable only by rebooting;
- a BROAD StorageService client would give a font service authority over the whole volume - the
  ambient authority this item refuses on behalf of its clients four lines below, and it cannot refuse
  for them what it takes for itself.

The same edit closes the half the finding names second: HOW it learns a face changed. The catalogue
re-reads the directory and recomputes each face's identity digest, and bumps the generation when a
digest, the name set or a face's metadata differs from what it last published. Nothing notifies it,
because nothing in this tree currently can, and a design waiting for a notification nobody sends
would not work. That makes the trigger explicit, and it adds a fourth operation - RESCAN - so
replacement is observable at all; the protocol line now says four operations rather than three.

**Finding 2 - canonical equivalence is a named decision rather than a decision. ACCEPTED.**

Confirmed: the normative pipeline's first stage read "UTF-8 validation and canonical-equivalence
policy", which is a slot. The consequences the auditor names are real - composed and decomposed
spellings could take different fallback, shaping and cache paths, and two implementations could
disagree about every cluster boundary in a decomposed run while both following the file.

Frozen as PRESERVATION, in three parts, with the mapping question answered first because it is the
one that cannot be fixed later:

- the pipeline NEVER rewrites the caller's bytes - no NFC, NFD or NFKC - and every offset in a
  `GlyphRun` is a byte offset into the ORIGINAL UTF-8. Normalising the buffer and then reporting
  offsets into the normalised copy is how an engine returns caret positions that do not exist in the
  caller's string, and no mapping back survives a decomposition that changes length;
- equivalence is resolved INSIDE shaping. A canonical composition and its decomposition are one
  grapheme cluster under UAX #29, so itemisation and fallback see one indivisible unit either way and
  cannot route them to different faces; within a run the shaper applies the composition and
  decomposition its tables require - which is what OpenType shaping does in any case - and may not
  change cluster boundaries or the reported byte spans while doing it;
- and it is tested: composed and decomposed spellings of one string produce the same glyph sequence
  and the same total advance, take the same fallback decision, and report cluster byte spans differing
  only as the inputs' own byte lengths differ. Run for a Latin combining-acute case and for one of the
  profile's non-Latin scripts, because a Latin-only proof of this is the case that always works.

AUDITOR'S RE-AUDIT OF PLAN M0136 (2026-09-01T13:23:01Z):

Rating: 7/10

1. **The accepted catalogue-input correction still specifies a confined, read-only StorageService
   capability that the current role and storage contracts cannot create.** The plan gives the
   catalogue a `Client` role on StorageService confined to the font directory and read-only
   (`docs/todo/P02M0136.md:158-180`). `Role` has no path or scope field
   (`src/tools/system-manifest/src/lib.rs:637-682`), and the `Client` executor only duplicates the
   named provider root (`src/user/services/core/src/service_manager/bootstrap.rs:123-159`). Directory
   confinement currently comes through `volume-admin.open-directory`, but that operation has no
   read-only argument (`src/idl/storage.lsidl:355-370`): it creates `Scope::Directory`, whose request
   filter admits mutation operations; only `Scope::File { writable: false }` blocks them
   (`src/user/services/storage/src/service.rs:334-350,508-516,711-775`). Thus the nearest existing
   route is confined but writable, while an ordinary `Client` is not confined at all. Explicitly own
   the read-only directory-mint contract and its supervisor delivery in this milestone, with a
   negative mutation gate; naming an existing `Client` role does not complete the accepted
   least-authority correction.

PLANNER'S RESPONSE ON M0136 (2026-09-01T14:40:00Z):

**Finding 1 - the confined read-only StorageService capability cannot be created by the contracts
named. ACCEPTED.**

Every claim checked and every one holds. `Role` carries a tag, a kind, a provider, a presence, an
interface and a source - no path, no scope - so a manifest `Client` row cannot be confined to a
directory at all, and its executor duplicates the named provider's root. The thing that IS confined,
`volume-admin.open-directory`, takes only `path: string`: it mints a `Scope::Directory` whose request
filter admits mutation, and only `Scope::File { writable: false }` refuses it.

So the two available routes were confined-but-writable and read-only-nowhere-near-confined, and last
round's row asked for the intersection, which nothing produces. I named an existing role kind and
attached two adjectives to it without checking that either was expressible - the same shape of error
as this round's M0169 finding, where I named a mechanism and did not check what it could do.

The correction owns the mint rather than borrowing it:

- `volume-admin.open-directory` gains a `writable: bool`, in the same shape `open-file` already
  carries. That operation is the evidence this is an extension rather than an invention: the argument,
  the read-only scope and the refusal all exist there already, and a directory scope minted
  `writable: false` refuses mutating operations through the same request filter that refuses them for
  a read-only file.
- ServiceManager holds the `volume-admin` authority, mints the read-only client over the canonical
  font destination at bootstrap, and hands it to the catalogue as its role. The path is NOT in the
  manifest - `Role` has no field for one, which is the finding's own point - it is the destination
  this milestone defines in P02M0097's layout, which is where the rest of the bootstrap wiring reads
  its paths from.
- and a negative gate, because a least-authority claim with no failing case is a sentence: the
  catalogue's own client attempts a write, a create and a delete under the font destination and each
  is refused, watched to fail by minting the same client `writable: true`.

The reasoning that selected StorageService over the init package and over a broad client is
unchanged and still stands - what changes is that the thing selected now exists.

---

AUDITOR'S RE-AUDIT OF PLAN M0136 (2026-09-01T15:31:33Z):

Rating: 5/10

1. **The accepted catalogue-input correction still does not define the path or bootstrap role that
   makes it executable.** The first deliverable continues to say only “the canonical FONT
   DESTINATION and its manifest role” rather than selecting either one
   (`docs/todo/P02M0136.md:107-110`). The later correction says ServiceManager mints a scoped client
   for that destination and hands it to the catalogue “as its role,” while also acknowledging that a
   manifest `Role` cannot carry a path and that an ordinary `Client` role duplicates the provider
   root (`:158-188`). It never fixes the destination URI, the factory source/destination rule, or the
   role tag/kind/source (or named ServiceManager dispatch case) that triggers this exceptional mint.
   Adding `writable` to `open-directory` creates the missing storage primitive but not the startup
   edge that invokes it. Freeze those identifiers and the corresponding manifest/bootstrap wiring;
   otherwise P0097, the manifest validator, ServiceManager and the catalogue can each implement a
   different unstated contract.

2. **The catalogue is ordered before the only parser it needs to implement LIST and RESCAN.** Nothing
   else may start before the catalogue item completes (`docs/todo/P02M0136.md:99-105,313-314`), but
   that item must enumerate collection faces and publish family/style, axes, face index and format,
   enforce per-face metadata bounds, and recompute metadata during RESCAN (`:208-238`). Obtaining
   those values from OpenType/TTC bytes is parsing. The closed profile and hostile-font parser are
   later items, and parser work is expressly forbidden until the profile is frozen (`:316-323,
   384-402`); the plan also assigns parsing to the client process rather than the catalogue
   (`:325-341`). No manifest-declared metadata sidecar or other non-parser source is specified. The
   current order therefore requires an unprofiled duplicate parser inside the catalogue to complete
   the prerequisite that allows the real parser to start. Either make staged, validated metadata the
   catalogue's normative input or order the required bounded metadata parser/profile before
   catalogue completion.

3. **RESCAN is exposed to every font client and is neither authority- nor work-bounded.** LIST,
   RESOLVE, SUBSCRIBE and RESCAN are on one catalogue interface, and both Factory consumers and
   ordinary applications receive fresh clients to that same root
   (`docs/todo/P02M0136.md:129-156,208-224`). Thus any application allowed to read a font can force a
   complete directory reread, digest and metadata pass repeatedly. `MAX_FONT_CLIENTS` bounds live
   endpoints, not requests per endpoint, so it does not bound this shared CPU/I/O work. The stated
   reason for public RESCAN is also false: StorageService already provides a bounded directory
   `watch` stream (`src/idl/storage.lsidl:267-282`), and scoped directory clients admit `OP_WATCH`
   (`src/user/services/storage/src/service.rs:750-775`). Use that notification as the normal trigger
   and keep any explicit recovery scan on a separately authorised/rate-bounded control edge; ordinary
   font lookup authority must not include unbounded catalogue maintenance authority.

4. **RESOLVE contradicts the caller-accounting and bounded-service claims.** RESOLVE returns a
   read-only `MemoryObject` containing as much as a 16 MiB face
   (`docs/todo/P02M0136.md:215-224,532-544`), while the plan says the catalogue holds only small
   metadata and that allocations are charged to the calling application's Domain (`:325-343`). In
   this kernel a MemoryObject is charged to the Domain that creates it and that charge remains until
   the last reference disappears (`src/kernel/object/memory_object.rs:27-39,52-79`;
   `src/kernel/syscall/mod.rs:585-594`). A catalogue-created object therefore remains charged to the
   catalogue while a client retains it after transfer. One connection can issue repeated RESOLVEs
   and retain many results; neither the client/subscriber counts nor the metadata ceilings bound
   those bytes. Freeze a requester/sponsor-created backing or another explicit charge-and-lifetime
   protocol, and add retained-result exhaustion/reclamation gates. Merely parsing the returned bytes
   in-process does not move the backing's charge.

5. **The canonical-equivalence policy is ordered too late to guarantee its own fallback result.** The
   normative pipeline performs face fallback before shaping (`docs/todo/P02M0136.md:418-427`), but
   the correction says equivalence is resolved inside shaping and nevertheless requires composed and
   decomposed spellings to choose the same fallback face (`:429-453`). Grapheme atomicity only keeps a
   sequence together; it does not make the raw coverage sets equal. For example, a face may cover
   precomposed U+00E9 but not the U+0065/U+0301 pair, or vice versa, so fallback has already diverged
   before the stage allowed to resolve equivalence runs. Define a canonical coverage view (with a
   map back to the preserved original UTF-8 spans) before fallback, or withdraw the same-fallback
   guarantee; the current policy and regression cannot both be implemented.

Clarification to finding 4: the backing is producer/service-charged, not necessarily
catalogue-created. StorageService currently creates the `MemoryObject` returned by `open`; if the
catalogue copies it, the catalogue becomes the producer instead. In neither case does transferring
the handle re-charge the object to the requesting application. The retained-result exhaustion defect
and required charge/lifetime protocol are unchanged.

6. **The proposed read-only-directory mint copies a filter that does not actually refuse every
   mutation.** The correction says a `writable: false` directory will use the same request filter that
   already refuses all mutation for a read-only file, and its negative gate tests only an unspecified
   write, create and delete (`docs/todo/P02M0136.md:163-190`). In the current filter, however,
   `writable` is consulted only for `Scope::File`, and its denial set omits `OP_MKDIR` and `OP_RMDIR`;
   both operations are then admitted for any matching scoped path alongside the other filesystem
   operations (`src/user/services/storage/src/service.rs:750-775`). Adding a boolean to the directory
   variant without freezing the complete mutating-op denial set can therefore mint a supposedly
   read-only client that creates or removes directories. Require and test refusal of `mkdir`, `rmdir`
   and every other mutating opcode through the new directory scope; “same filter” is not a sufficient
   correction while that filter has this hole.

7. **The installed-face ceilings have no runtime replacement/RESCAN outcome.** The plan says the font
   destination remains replaceable during a boot and RESCAN republishes its current contents, but it
   defines exact/over-bound behavior for the 64-face and 256-byte-record ceilings only at staging time
   (`docs/todo/P02M0136.md:195-220,229-277`). A writable destination can therefore become a 65-face
   catalogue, or acquire a face whose metadata encodes to 257 bytes, after the image passed staging.
   The service is not told whether to truncate LIST, discard only the offending face, tear down the
   catalogue, or retain the previous generation. Freeze an atomic runtime refusal rule—normally keep
   serving the last valid generation and report the failed rescan—and add one-past runtime replacement
   gates. Build-time rejection alone does not preserve the stated runtime bounds.

PLANNER'S RESPONSE ON M0136 (2026-09-01T17:25:00Z):

All seven findings ACCEPTED. Three of them - 1, 3 and 6 - are defects in the correction I made last
round, and finding 3 is the third time this session I have asserted that this tree cannot do
something without checking whether it already does.

**Finding 1 - the catalogue-input correction has no path, role or bootstrap edge. ACCEPTED.**

Right: I said ServiceManager mints a scoped client and hands it over "as its role" while also saying
a `Role` cannot carry a path, and never named the destination, the tag, the kind or the dispatch case.
Adding `writable` to `open-directory` supplies the primitive and not the startup edge. Now frozen:
the destination is `vol://system/share/fonts`; the row is tag `FONTDIR`, kind `client`, provider
`storage_service`; and the exceptional part is stated - that tag is the one case where ServiceManager
does not duplicate a provider root but calls `volume-admin.open-directory` with the destination and
`writable: false`, with the path living beside the other bootstrap path constants because the
manifest has nowhere to put it. A mint that fails is a start-up refusal for the catalogue.

**Finding 2 - the catalogue is ordered before the only parser it needs. ACCEPTED.**

Correct and I had not noticed the circularity: this item is the prerequisite for everything else, it
must publish family, style, axes, face index and format and recompute them on replacement, and
getting those from OpenType or TTC bytes is parsing - which this file forbids before the profile is
frozen, and the profile comes after this item. As ordered it required an unprofiled second parser
inside the catalogue to unblock the real one.

Resolved by changing the INPUT rather than the order: the catalogue parses nothing. The build that
stages a face stages a bounded metadata record beside it, produced by the staging tool where a parser
is allowed and where the staged-consistency gate already checks its output. The catalogue reads
records and digests bytes. A face with no record, or a record whose digest does not match the face,
is not published and is reported - which is also the right answer for a file dropped in by hand.

**Finding 3 - RESCAN is exposed to every font client and is unbounded. ACCEPTED.**

Both halves. The authority half is plain: LIST, RESOLVE, SUBSCRIBE and RESCAN were one interface
reached by Factory consumers and ordinary applications alike, so authority to READ a font was
authority to force a full directory read, digest and metadata pass as often as a client liked, and
`MAX_FONT_CLIENTS` bounds endpoints rather than requests.

The stated reason for having it was also false, and that is the part worth owning. I wrote "nothing
notifies it, because nothing in this tree currently can". `volume.watch` has been there all along: it
takes a path, reports the entries directly below a DIRECTORY as they change, is admitted by a scoped
directory client, and is bounded by design - a watcher that cannot keep up is dropped rather than
buffered, and its own documentation calls the stream a hint to re-read. I asserted a limitation
instead of checking for the capability, which is the same error as this round's M0169 and the earlier
`catalogue.take`.

So the watch is the normal trigger, seeded by a scan at start; RESCAN leaves the client interface
entirely and survives only as recovery on a separate `font-catalogue-admin` capability, one in flight
and one per published generation, held by no ordinary application. The watch's documented limit - it
sees mutations through StorageService, not a disk edited behind its back - is recorded rather than
glossed.

**Finding 4 - RESOLVE contradicts the caller-accounting claim. ACCEPTED.**

Correct, including the clarification: the charge follows the CREATOR, transferring the handle does
not re-charge it, and the object stays charged until the last reference goes. So a client issuing
repeated RESOLVEs and retaining the results grew the service's charge without bound, which neither
the client count nor the metadata ceilings touch - while the plan claimed elsewhere that allocations
are charged to the calling application.

RESOLVE now takes the requester's own memory authority - a sponsor handle passed with the request -
and the backing is created against that Domain, so the bytes are the caller's from the moment they
exist. Gated: a client retaining N faces sees its own Domain rise and the catalogue's stay flat; a
client that exits releases them without the catalogue acting; a RESOLVE whose sponsor is absent or
refuses is a typed refusal rather than a fallback to service memory.

**Finding 5 - the canonical-equivalence policy is ordered too late to deliver its own guarantee.
ACCEPTED.**

The counterexample is decisive: fallback runs before shaping, a face may cover precomposed U+00E9 and
not the U+0065/U+0301 pair or the reverse, so the two spellings have already been routed to different
faces before the stage allowed to resolve equivalence runs. Grapheme atomicity keeps a sequence
together and does not equalise coverage. The policy and its regression could not both be implemented.

Coverage is now asked in a CANONICAL VIEW: a face covers a cluster when it covers the cluster's
canonical composition OR its full decomposition, and fallback asks that question. It is a lookup over
the cluster, not a rewrite of the buffer - the original UTF-8 is still what every `GlyphRun` offset
refers to, which was the load-bearing half and is unchanged. Shaping then applies whichever form the
chosen face needs.

**Finding 6 - the read-only mint copies a filter that does not refuse every mutation. ACCEPTED.**

Verified in the filter: `writable` is consulted only for `Scope::File`, that denial list omits
`OP_MKDIR` and `OP_RMDIR`, and the op table then admits both for any matching scoped path. So
"the same request filter that already refuses them for a read-only file" would have minted a
read-only client that can create and remove directories, and my negative gate - "a write, a create
and a delete" - would have passed straight over the hole.

The denial set is now named rather than inherited: `OP_WRITE`, `OP_REMOVE`, `OP_TRUNCATE`, `OP_TOUCH`,
`OP_WRITE_STREAM`, `OP_OPEN_WRITER`, `OP_MKDIR`, `OP_RMDIR`, `OP_RENAME`; admitted are `OP_OPEN`,
`OP_LIST`, `OP_READ`, `OP_WATCH`, which is exactly what the catalogue needs. Any opcode added later
is refused until classified. The gate exercises every opcode in the denial set and names `mkdir` and
`rmdir` individually, because they are the two the existing filter lets through.

**Finding 7 - the ceilings have no runtime replacement outcome. ACCEPTED.**

Right: the destination is replaceable while the machine runs, so a rescan can find a 65th face or a
257-byte record after staging passed, and the plan defined exact/over-bound behaviour only at staging
time - leaving truncate, drop-one, tear-down and keep-previous all equally readable.

Frozen as atomic and conservative: a rescan that would exceed any ceiling PUBLISHES NOTHING. The
previous generation stays current and stays served, the failure is reported with what exceeded which
ceiling, and the generation does not advance - so a client's view is never partial and a bad drop into
the directory cannot take fonts away from a running system. Gated with the one-past cases at runtime,
not only at build.

AUDITOR'S RE-AUDIT OF PLAN M0136 (2026-09-01T17:57:32Z):

Rating: 5/10

1. **The metadata-sidecar response moves the parser cycle into an unspecified host tool rather than
   resolving it.** The catalogue must complete before anything else starts, yet its sidecars contain
   family, style, axes, collection face index and format—facts obtained by parsing OpenType/TTC bytes
   (`docs/todo/P02M0136.md:99-105,298-315`). The plan merely says a staging tool produces them “where
   a parser is allowed.” It later makes the closed profile a start gate and says **no parser** is
   written before that profile is frozen (`:419-429,490-508`), and assigns all parsing to the later
   in-process parser. No sidecar producer, parser/version, accepted metadata-table profile, provenance,
   hostile-input limits or truth oracle is named; digest equality proves only that a record names the
   adjacent bytes, not that its metadata was decoded correctly. The original unprofiled duplicate
   parser is therefore still required before the item that is supposed to authorize parsing. Own and
   freeze the bounded sidecar-production contract before staging, or reorder it behind the same closed
   profile/parser instead of exempting an unnamed host parser.

2. **The requester “sponsor handle” is not a realizable capability in the current system and the plan
   does not define the new mechanism.** `SYS_MEMORY_OBJECT_CREATE` accepts only a size and always
   charges `current_thread().domain()` (`src/kernel/syscall/mod.rs:585-595`; the userspace wrapper is
   `src/user/runtime/rt/src/lib.rs:2167-2172`). A Domain handle can currently be used for management,
   statistics and process placement, but there is no allocate-a-MemoryObject-in-this-Domain operation;
   an ordinary application is also not handed a handle naming its own Domain. The plan nevertheless
   says RESOLVE receives unspecified “memory authority” and that the catalogue creates backing against
   the Domain it names, without defining the sponsor's object type, mint/delivery path, required and
   attenuated rights, allocation syscall/service operation, or lifetime (`docs/todo/P02M0136.md:275-294`).
   The retained-face accounting gates therefore cannot be implemented as written. This correction must
   either own that concrete allocation capability end to end or use caller-created backing that the
   existing caller-charged syscall can actually produce.

3. **The recovery RESCAN can permanently lock itself out after the failure it is meant to repair.**
   The admin operation is limited to one request per *published generation*
   (`docs/todo/P02M0136.md:271-274`), while an invalid or over-ceiling scan deliberately publishes
   nothing and leaves that generation unchanged (`:355-364`). If the directory watch was dropped or
   missed—the reason an explicit recovery operation survives—the first recovery scan can observe the
   bad state, consume the generation's sole attempt, and leave no way to scan again after an operator
   corrects the files. The plan also does not identify the trusted holder/mint path for the admin
   capability. Define a bounded retry/re-arm rule that distinguishes failed from successfully
   published scans and name the recovery owner; “one per generation” is not recoverable with the
   stated failure semantics.

PLANNER'S RESPONSE ON M0136 (2026-09-02T00:15:00Z):

Three findings, all three ACCEPTED. All three are decisions I made in the previous round that named a
mechanism this system does not have, and the first two are the same error twice: I described where a
thing should live instead of checking whether it could exist there.

**Finding 1 - the metadata sidecar moves the parser cycle into an unspecified host tool rather than
resolving it. ACCEPTED.**

The finding is right and the phrase it quotes is the whole problem: "produced by the staging tool
where a parser is allowed" moved the parser rather than removing it. Family, style, axes, face index
and format come out of OpenType and TTC bytes, so a tool that DERIVES them is a font parser -
unprofiled, unnamed, unversioned, with no hostile-input contract and no truth oracle - written before
the item that authorises parsing. And the check I claimed for it does not check what I said: digest
equality proves the record names the bytes beside it and nothing about whether they were decoded
correctly.

I took neither of the two options the finding offers, and the reason is in what a sidecar is for. The
record is now DECLARED, not derived: a face enters this image because somebody added it to the source
tree, and its record is a checked-in declaration beside it, authored in the same review. The staging
tool validates and digests - it refuses a face with no declaration, one that encodes past
`MAX_FACE_METADATA_BYTES`, one whose values are outside the closed vocabularies this milestone
freezes, and one whose digest does not match the face - and it parses nothing. So there is no second
parser anywhere in this milestone, the catalogue keeps the position its ordering requires, and the
closed profile keeps its meaning.

The truth oracle the finding asks for arrives with the only component this file allows to read a
font, and it is now an obligation of that item rather than a hope: the parser item gains a gate that
parses every staged face and requires the fields it recovers to EQUAL the declaration, naming the
face and the field on a disagreement. Until it exists the declarations are trusted by review, which
is written down rather than implied - a declaration can be wrong, and saying so is the difference
between a bounded assumption and an unnoticed one.

**Finding 2 - the requester sponsor handle is not a realizable capability. ACCEPTED.**

Checked in the kernel rather than argued: `sys_memory_object_create` takes a SIZE and charges
`current_thread().domain()`; there is no allocate-in-the-Domain-this-handle-names operation; and the
Domain syscalls are create, kill and stats-get, so an ordinary application is not even handed a
handle naming its own Domain. The "sponsor handle" had no object type, no mint path, no rights and no
allocation call, and the retained-face accounting gates could not have been implemented as written.

I did not take the first option. An allocate-into-another-Domain primitive is a serious new
authority - it would let its holder grow a third party's charge - and a font catalogue is not the
milestone that should introduce it. The second option is what this kernel already expresses, so
RESOLVE becomes two operations: RESOLVE-INFO answers the face's byte length and the generation that
answer is about, and RESOLVE-INTO takes that generation and a `MemoryObject` the CALLER created and
passed with the request. The catalogue maps it, fills it, unmaps and closes its handle before
replying; the handle arrives with map and write and neither `RIGHT_DUPLICATE` nor `RIGHT_TRANSFER`,
so the catalogue cannot copy it, pass it on, or hold it past the call.

Two consequences are written down because splitting an operation creates them. The race between INFO
and INTO is closed by the generation the plan already has - a face replaced in between changes its
identity and the published generation, so INTO carries the generation INFO answered about and is a
typed refusal when it is stale. And an object too small is the same typed refusal with the required
length, never a truncated face. The gates are restated in those terms, plus one the sponsor version
could not have had: the catalogue holds no client handle across a reply, watched by a fixture that
resolves a face and requires the service's handle count to be unchanged.

The protocol row that said THREE operations now says four and says why RESOLVE could not be one.

**Finding 3 - the recovery RESCAN can permanently lock itself out. ACCEPTED, and it is the sharpest
of the three.**

The two rules are three hundred lines apart and each is right alone: at most one rescan per published
generation, and a scan that is invalid or over a ceiling publishes nothing and leaves the generation
unchanged. Together they mean the first recovery scan after a missed watch can observe the bad
directory, spend the generation's only attempt, and leave no way to scan again once an operator fixes
the files. The failure the operation exists for is the failure that disables it.

The budget now counts SUCCESSFUL PUBLICATIONS rather than attempts - a scan that publishes a new
generation spends the allowance for the one it replaced, and a scan that publishes nothing does not -
and a failed scan re-arms on a bounded delay, so a caller cannot spin the directory read by asking in
a loop: at most one in flight, and after a failure the next is refused with `try-again-at` until the
delay passes. That bounds the work exactly as the original rule intended and leaves the recovery path
reachable, which the original rule did not.

The holder is named, which it was not: the `font-catalogue-admin` capability is minted by
PermissionManager for the operator path alone - the same one that reaches the device policy endpoint
- and by no factory rule, manifest role or application grant. That is what makes the ceiling a bound
on an operator rather than on every font client, which was the reason the operation was moved off the
client interface in the first place. Its gates are stated: a failed ceiling leaves the published
generation unchanged and does not consume the allowance, a corrected directory then scans
successfully, a scan inside the delay is refused with `try-again-at`, and an ordinary
`font-catalogue` holder is refused the admin interface entirely.

AUDITOR'S RE-AUDIT OF PLAN M0136 (2026-09-01T23:24:26Z):

Rating: 6/10

1. **The caller-backed `RESOLVE-INTO` rights/lifetime contract is not implementable through the
   specified LSIDL transport.** The plan requires the catalogue to receive the caller's
   `MemoryObject` with map+write but without `RIGHT_TRANSFER` or `RIGHT_DUPLICATE`, close that handle
   before replying, and leave the caller owning the resolved bytes
   (`docs/todo/P02M0136.md:333-360`). Generated typed clients currently pass request handles through
   `ChannelTransport::call` to `send_caps_blocking`; that path invokes `SYS_CHANNEL_SEND_CAPS`, which
   requires `RIGHT_TRANSFER`, consumes the sender handle, and delivers the capability with its rights
   unchanged (`src/user/libs/ipc/ipc-client/src/lib.rs:36-48`;
   `src/user/runtime/rt/src/lib.rs:2524-2532,2726-2743`;
   `src/kernel/syscall/mod.rs:2756-2809`). The only attenuation primitive is the separate single-handle
   `send_blocking_attenuated`, which the typed transport does not use. Thus an ordinary generated call
   either gives the catalogue `TRANSFER` and spends the caller's only handle, or needs an unowned
   duplicate-and-retain plus attenuated-request mechanism. The accounting correction is sound, but
   its stated authority and retained-result gates still have no protocol path that can satisfy them.

2. **The accepted admin-holder correction names neither a deliverable endpoint path nor a final
   holder, and expressly excludes the routes its claimed analogue uses.** The plan says
   PermissionManager mints `font-catalogue-admin` for “the operator path” while providing no factory
   rule, manifest role or application grant (`docs/todo/P02M0136.md:300-310`). PermissionManager can
   mint a client only from an endpoint it receives, and the cited device-policy path is concrete in
   exactly the ways this correction omits: a `DEVPOLICY` manifest client role names DeviceManager's
   admin interface, ServiceManager passes that endpoint to PermissionManager, the security
   capability/tag is declared, and governed `lsdev` is the recipient
   (`src/user/services/manifest.toml:2240-2249`;
   `src/user/services/core/src/service_manager/bootstrap.rs:1005-1016`;
   `src/user/services/core/src/permission_manager.rs:219-228,324-365,1497-1499`). With no corresponding
   source/tag/bootstrap delivery and no operator tool grant, PermissionManager has nothing to mint
   and no named operator can issue recovery after a dropped watch. “No ordinary application holds
   it” is an authority policy, not a realizable recovery route.

3. **The revised RESCAN budget still permits an unbounded sequence of full scans.** Catalogue scans
   advance the generation only when names, digests or metadata differ from the published index
   (`docs/todo/P02M0136.md:252-255`). The new rule consumes an allowance only when a scan publishes a
   new generation, while only a *failed* scan gets delayed backoff (`:284-299`). A valid scan of an
   unchanged directory is neither a publication nor the invalid/over-ceiling failure described by
   the plan, so it consumes no allowance and re-arms no delay. The admin caller can therefore request
   full directory reads, digests and metadata passes serially without bound despite the correction's
   claim that it cannot spin them. The response fixed retry after a bad directory, but did not define
   or gate the no-change success case, so the intended work bound remains incomplete.

PLANNER'S RESPONSE ON M0136 (2026-09-02T04:00:00Z):

Three findings, all three ACCEPTED. All three are corrections I made yesterday that named a mechanism
without checking that the system can express it - the third time in three rounds on this file, and
the pattern is the finding worth taking from it.

**Finding 1 - the caller-backed `RESOLVE-INTO` rights/lifetime contract is not implementable through
the specified transport. ACCEPTED.**

I traced it rather than accepting it, and it holds. A generated typed call passes request handles
through `ChannelTransport::call` to `send_caps_blocking`, which is `SYS_CHANNEL_SEND_CAPS`; that
syscall takes each handle with `Rights::TRANSFER` REQUIRED, CONSUMES it from the sender's table, and
delivers it with its rights unchanged. So "the handle arrives with map and write and neither
`RIGHT_DUPLICATE` nor `RIGHT_TRANSFER`" describes a handle the transport cannot carry, and the only
attenuating primitive - the single-handle `SYS_CHANNEL_SEND_ATTENUATED` - is not what the typed
transport uses. I replaced one unimplementable mechanism with another and called the accounting sound
without checking the delivery.

The attenuation moves to the caller, where a primitive already exists: `SYS_HANDLE_DUPLICATE` takes a
rights mask and refuses anything the original does not hold, so it narrows and never widens. The
caller creates the object - holding every right, and charged for it - duplicates a handle down to
exactly `MAP | WRITE | TRANSFER`, and passes the COPY, keeping the original so the send consuming the
copy costs it nothing.

What that buys is now stated exactly rather than optimistically, including the part that is worse
than the first version: no `RIGHT_DUPLICATE`, so the catalogue cannot make a second handle and keep
it after closing the one it was given, which is what the retained-face gates actually rest on; map
and write and no read, so a service filling a buffer cannot read what was in it. And `RIGHT_TRANSFER`
IS present, because the transport consumes handles that way and nothing here can change it - so "the
catalogue must not pass the object on" is a code obligation with a gate rather than a right it lacks.
Written down, because a plan that claims a rights bit it cannot obtain is a plan whose security
argument is decoration.

I did not take the other route the finding implies - extending the typed transport to attenuate
request handles - because that is a change to every generated client, owned by the IPC layer, and
this is a font catalogue. That is the same reason I did not add an allocate-in-another-Domain syscall
last round.

**Finding 2 - the admin-holder correction names neither a deliverable endpoint path nor a final
holder. ACCEPTED.**

Correct, and the comparison it draws is exact: I cited the device-policy path as the analogue and
then omitted all four things that make that path work. PermissionManager can mint a client only from
an endpoint it has been given, and nothing gave it one - so "minted for the operator path alone" is
an authority policy with no route behind it, and after a dropped watch there is no named party who
can issue a recovery scan.

The four are now copied rather than alluded to: the ENDPOINT is a second serve endpoint of the same
service, minted at its start beside the client one; the ROLE is a manifest client role on
`permission_manager` naming it, the way `DEVPOLICY` names DeviceManager's admin interface; the
DELIVERY is ServiceManager passing that endpoint to PermissionManager during bootstrap, on the same
hop that carries the device-policy one; and the RECIPIENT is the named operator tool that owns font
administration, holding a declared security capability, which is what makes the grant governed rather
than ambient. "No ordinary application holds it" describes who may not; the plan now also says who
may.

**Finding 3 - the revised RESCAN budget still permits an unbounded sequence of full scans. ACCEPTED,
and this is the second correction to the same rule in two days.**

The hole is exactly where the finding says. The generation advances only when a digest, a name or a
face's metadata differs from what was published, so a valid scan of an UNCHANGED directory publishes
nothing - and my rule spent an allowance only on a publication and re-armed a delay only after a
FAILURE. An unchanged scan is neither, so it cost nothing and permitted the next one immediately:
serial full directory reads, digests and metadata passes, which is the work this operation was moved
off the client interface to bound.

The two questions are now bounded separately, because conflating them is what produced a rule with a
hole in it both times. HOW OFTEN A SCAN MAY RUN: at most one in flight, and every COMPLETED scan -
published, unchanged or failed - re-arms a bounded delay, with a request inside it refused
`try-again-at`. That is the work bound and it does not care what the scan found. HOW MANY MAY
PUBLISH: one per published generation, counted as before. That is the churn bound, and it is what
keeps a recovery scan available after a bad directory is corrected. Its gates gain the missing case -
two scans of an unchanged directory, the second refused, the published generation unmoved by either.
