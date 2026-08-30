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
