IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0103 (2026-09-09T19:55:00Z):

Scope asked for: "implement P02M0103". What the plan and the roadmap say, verified before touching
anything:

- `docs/todo/P02M0103.md`, status line: "PHASE-4 FUTURE VISION. NOT AN ACTIVE PRODUCT MILESTONE AND
  NOT A PHASE-2 COMPLETION GATE. ACTIVATED ONLY AFTER THE SERVER PHASE AND BY EXPLICIT PROJECT-OWNER
  APPROVAL, PART BY PART." The file then names which parts are separately approvable as cross-phase
  foundations (`s`, `a-common`, `b`+`c`), withdraws `a-wsi` from that list, and records that
  everything from `e` onward is Phase 4 and "is not approved by approving the 2D foundation".
- `docs/todo/TODO.md` carries the milestone as `[i]` - the index state the milestone-index gate
  knows for a row that is title-checked, never counted open and never tickable - with the note
  "activated part by part after the server phase and by explicit approval, with only `s`, `a-common`
  and `b`+`c` approvable as cross-phase foundations".

BLOCKER, reported rather than worked around: a blanket "implement P02M0103" is not the activation
the plan requires. The plan makes activation a project-owner decision taken PART BY PART, after the
server phase, and the roadmap row agrees; nothing in this job's request names a part, and starting
the 2D foundation (`s`, `a-common`, `b`, `c`) on the strength of a list that also names ten other
milestones would be this implementer inventing the approval the plan reserves to the owner. The
`[i]` state exists precisely so that a tool and a reader do not see an open Phase-2 milestone here.

What was done: nothing in the tree for this milestone. No code, no documents, no ticks.

What would unblock it: an explicit, written activation of named parts - `P02M0103s` (the normative
specifications, "may be written at any time"), `P02M0103a-common`, or `P02M0103b`+`c` - from the
project owner, recorded in `docs/todo/TODO.md` or in the milestone's status line. `P02M0136` (the
text foundation) waits on `b`+`c` for its shaping and library work; its own audit says so.

Verification: not applicable - no change was made. Commands run: `sed -n 1,60p
docs/todo/P02M0103.md`, `grep -n P02M0103 docs/todo/TODO.md`.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0103 (2026-09-10T20:43:15Z):


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0103 (2026-09-10T20:43:20Z):

Scope: `docs/todo/P02M0103.md`. Nothing was implemented. This entry states the size honestly rather
than repeating "it is large".

## What the file actually contains

2631 lines describing a graphics platform in five parts: `P02M0103s`, the normative specification
documents that freeze the image, colour, WSI, render2d, render3d, shader-IR and scene profiles with
canonical semantic hashes and conformance thresholds; `P02M0103a-common`, the image and colour model
with its multi-plane form and the migration of the existing pixel types onto it; `P02M0103a-wsi`,
surfaces, a present queue with timing and damage, and a typed display-device interface replacing the
current byte protocol; and `b` and `c`, the 2D and 3D backends that build on them.

Its own status line is `PHASE-4 FUTURE VISION. NOT AN ACTIVE PRODUCT MILESTONE AND NOT A PHASE-2
COMPLETION GATE`, and the per-part prerequisite matrix at the top names two things nothing in this
tree supplies.

## Why it was not started

Not because it is blocked - the specification part says in as many words that it "may be written at
any time" - but because delivering ANY of it to this repository's standard is a body of work on the
scale of everything else in this job put together, and delivering a fraction of it is worse than
delivering none: a frozen profile document that the backend then contradicts is a specification
nobody can rely on, and a colour model with no consumer is a type nobody has tested against a real
pipeline.

The one part that is genuinely separable is `P02M0103s`, the specification documents. They are
writing rather than implementation, they have no build dependency, and they are what `a`, `b` and `c`
are checked against. A run that had this milestone alone in front of it should start there.

Verification: not applicable - no change was made.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0103 (2026-09-12T03:57:49Z):

## What was delivered

The first three items of `P02M0103b`, as one unit: the closed profile, its limits, and the machine-
readable form everything else is generated from and checked against.

`src/user/libs/graphics/profile` - a crate with NO dependencies, `no_std` except under test.

  - `render2d.rs`: `enum Render2DFeature`, 110 variants, and `RENDER2D_CORE_PROFILE_1` over them -
    every line of the plan's table, at the granularity of "something a backend can fail to do".
    `CapButt` and `CapRound` are separate entries because a rasteriser can get one right and the
    other wrong, and a matrix that could only say "strokes" would report a half-working backend as
    conforming.
  - `render3d.rs`: `enum Render3DFeature`, 89 variants, `RENDER3D_CORE_PROFILE_1`, same shape. The
    2D part's item requires BOTH profiles machine-readable, which is why the 3D list is written in
    the 2D part's unit; nothing else of the 3D part is touched or started.
  - `limits.rs`: `Render2DLimits` with all fifteen bounds the plan names, `RENDER2D_PROFILE_1_MINIMA`
    as the guaranteed floor, and `meets_profile_1()` refusing a declaration below it BY NAMING the
    field that is short.
  - `capability.rs`: the one comparison every check is made of - what the profile requires and the
    claims do not, and what the claims have and the profile does not - with a `Range` so the handler
    check ranges over `Backend`-owned features only. Allocation-free, so the same comparison can run
    in a host tool and in a guest self-report.

`src/tools/profile-doc` - the generator and the checker, whose only dependencies are the profile
itself and the tree's existing SHA-256. From the two constants it writes, under `docs/gen/render2d`
and `docs/gen/render3d`: `profile-1.md` (the documentation table and, for 2D, the guaranteed
minima), `backend-checklist.md`, `conformance-matrix.md`, `capability-report.md`, and
`profile-1.canonical` - the canonical form the profile hash is taken over, one line per feature in
profile order plus the minima, so a feature added, removed or REORDERED and a minimum lowered all
change a hash printed in the table.

`src/tools/check-graphics-profile.sh`, registered in `check.sh` and in the verification catalog -
the profile's own fixtures, the tool's self-test, then the regenerate-and-compare and the three
checks.

## The three checks, and how a claim is made

A claim is a marker in the source: `@handles: <feature>` on the code that implements it, `@covers:
<feature>` on the test that measures it. A deleted handler takes its claim with it, which a registry
beside the code would not - it would go on claiming coverage for code that no longer exists.

  - every `Backend`-owned feature has a handler
  - every feature has at least one conformance test
  - no claim names a feature the profile does not have, and no backend claims a feature the profile
    says no backend owns

The third is refused unconditionally; a typo and an extension look the same from here and both must
be.

## Verification

PERFORMED and passing:

  - `cargo test` in `src/user/libs/graphics/profile` - 6 fixtures, 6 passed, 0 failed. The list is
    closed (no name twice, no feature twice, no unknown group, no empty group, something
    backend-owned) for BOTH profiles; twelve Porter-Duff operators plus additive `Plus`, eleven
    separable and four non-separable blend modes counted rather than read; the geometry queries
    owned by `render2d` and never by a backend; the 3D depth formats, sample counts, readbacks and
    `GraphicsCore`-owned colour formats present; the minima refusing a short declaration BY NAME;
    and the two profiles' name sets disjoint, which the claim scanner depends on.
  - `profile-doc --self-test` - 6 cases, all as expected: a claim is found where it is written with
    its file and line; a sentence that only MENTIONS the marker is not a claim (the failure that
    made this self-test necessary - the first scan reported the generator's own documentation as
    features); a name outside the profile is refused; a handler for a feature no backend owns is
    refused; and what is not claimed is still reported missing.
  - `profile-doc --check` - the five generated documents of each profile match what the profiles
    generate.
  - `./gen.sh --check` - no drift, sixteen LSIDL packages and both profiles.
  - `./check.sh --gate graphics-profile`, `--gate verify-model`, `--gate gate-oracles`,
    `--gate source-hygiene`, `--gate dependency-policy` - all passed. The manifest validates with the
    new `[[sources]]` row (`system-manifest check`, exit 0).

NOT PERFORMED, and the gate prints it rather than a pass:

  - the backend-handler check. No backend exists; it ranges over nothing. 89 backend-owned features
    in 2D and 80 in 3D await a claim.
  - the conformance-coverage check. No conformance suite exists; 110 features in 2D and 89 in 3D
    await a test.

Both halves become real the moment a backend or a suite declares its first marker, which is the
point of building the gate before them: a checklist produced after an implementation is a
description of what was built rather than a statement of what was required. The self-test is what
stands in for them today - a clean tree proves nothing about a scan that has stopped matching.

## What was NOT delivered

The remaining fifteen items of `P02M0103b` - stroke scaling under a transform, the `render2d`
library itself, `Canvas`, `DrawList`, the resource table, validation, damage, caching and the rest.
The profile is what they are all measured against and it exists now; none of them is started.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0103 (2026-09-12T18:08:42Z):

`IMAGE_COLOR_PROFILE_1.md`: THE FIRST OF THE NORMATIVE DOCUMENTS, AND IT IS GENERATED.

WHY GENERATED AND NOT WRITTEN

This document carries the matrices, the transfer constants and the rounding rules that decide whether
two implementations produce the same pixels. That makes it the worst thing in this tree to let drift
from the code, and a hand-written normative document drifts the first time somebody changes a
constant without opening it. So the REGISTRY is the source, the document is written from it, and a
SHA-256 over the canonical form makes a change to any value a line in a diff. It is written to
`docs/graphics/` rather than `docs/gen/` because other documents cite it by name and a citation into
a generated directory reads as a build artefact; its banner says where it comes from.

WHAT IS FROZEN

  - THE TWELVE STORAGE FORMATS, with channel order defined by the format NAME and never by host
    endianness, their widths, encoding and byte count - and the canonical intermediate NAMED:
    premultiplied linear `R16G16B16A16_FLOAT` for every layer, filter intermediate and offscreen
    composite. Repeated compositing through eight-bit sRGB bands, and a blur over an eight-bit
    intermediate is where it shows first.
  - THE ALPHA MODES EACH FORMAT ADMITS, and they FALL OUT of what its channels are rather than being
    listed per format and getting out of step. An `X8` format is opaque only; an ALPHA-ONLY format
    admits `straight` alone, because premultiplication is a relation between colour and alpha and
    there is no colour to have been multiplied. I had it as two booleans first and the alpha-only
    case came out wrong; it is one enumeration now and the modes are derived.
  - THE SIX IMAGE SEMANTICS AND THE OPERATIONS EACH ADMITS. A colour-managed pipeline that cannot
    tell a colour from a measurement will transform the measurement, and the artefact looks like a
    lighting bug. Filtering an IDENTITY image averages two object ids into a third that names a
    different object, and the bug that follows is a click landing on the wrong thing.
  - THREE PRIMARY SETS AND EIGHT COLOUR SPACES; sRGB WITH ITS LINEAR SEGMENT, which is the part that
    gets dropped - a 2.2 power law is close enough to look right and wrong enough that two
    implementations disagree in the darks, which is where banding lives.
  - PQ'S CONSTANTS AS THE TWELVE-BIT FRACTIONS THE STANDARD STATES, not as rounded decimals. HLG's
    three, with `b` and `c` written out beside their derivations from `a`.
  - BRADFORD AND ITS INVERSE, named and written down, because "adapts between white points" is
    satisfied by three different matrices in common use and they do not agree.
  - DIFFUSE WHITE AT 203 cd/m²; EXTENDED REINHARD ON LUMINANCE as the one tone-mapping operator, with
    its single parameter - chosen over a filmic curve because it has one parameter, is exactly
    reproducible, and is defined on luminance so it does not shift hue, where a filmic curve is
    prettier and is five constants two implementations copy from different sources; and
    HUE-PRESERVING DESATURATION BY BISECTION in a FIXED sixteen steps as the gamut-mapping rule,
    because clipping each channel shifts hue most on exactly the saturated colours a wide-gamut image
    was made for, and a fixed step count is what makes two implementations agree rather than "until
    it converges".
  - THE BAYER 8x8 DITHER MATRIX, with its phase anchored to the TARGET's origin. Error diffusion
    carries state ACROSS pixels, which makes a tile-parallel renderer's output depend on how it
    decomposed the image - not merely vague here but incompatible with the architecture - and a
    tile-relative phase makes the pattern restart at every tile boundary, which is the artefact that
    looks like a seam.
  - THE ROUNDING AT EVERY BOUNDARY: clamp-then-round-half-away-from-zero into an integer channel,
    NaN to zero, infinity clamped, round-to-nearest-ties-to-even into a half, subnormals PRESERVED,
    one canonical quiet NaN, and reserved bits ignored on read and written all-set so a producer
    cannot leak stale bytes.
  - THE THREE YUV LAYOUTS with plane order, PER-PLANE PITCH, bit placement - P010's ten bits are the
    HIGH ten of a little-endian word, which is the half a reader assumes is the low ten - the
    odd-extent rule, the three matrices, both ranges at both depths, chroma siting and
    reconstruction; and THE ORDER OF OPERATIONS, which is the part an RGB recipe gets wrong: the
    matrix produces ENCODED RGB, and only then does transfer decoding happen. Applying an RGB
    sampling recipe to YUV bytes decodes the transfer function of a signal that is not yet a colour.
  - ALLOCATION AND PADDING, the HDR metadata each transfer function needs, and the THREE BYTE SPANS,
    which are three different numbers: confusing them is how a buffer is accepted that cannot hold
    the image, or how one that is exactly big enough is refused.

THE FIXTURES CHECK THE VALUES, NOT THEIR PRESENCE

That is the difference that matters for a transcribed constant. sRGB's two segments must MEET at
their thresholds; PQ's `c1 = c3 - c2 + 1` identity must hold; HLG's `b` and `c` must be the
derivations of `a`; the Bradford pair must actually multiply to the identity; the dither matrix must
be a permutation of its own range; the ten-bit YUV ranges must be the eight-bit ones scaled rather
than re-derived; and the YUV order must place the matrix before the transfer function. Each of those
catches a paste from the wrong source, which is how these numbers actually go wrong.

WHAT WAS VERIFIED, AND HOW

`cargo test -p graphics-profile`: 15 passed, 0 failed - 6 of them new.
`./src/tools/check-graphics-profile.sh`: passes, regenerating and comparing every profile document
including this one. `cargo clippy -D warnings` over the profile crate: clean, after a first version
of one fixture turned out to be a vacuous assertion over a constant and was rewritten to check the
colour-space list instead. `./format.sh` was run and the suite re-run.

NOT PERFORMED: aarch64 and riscv64, and the full sweep. This is an enumeration crate with no
architecture-specific code.

WHAT `s-common` STILL NEEDS

`docs/GRAPHICS.md`, the other half of this freeze point. `s-common` releases `a-common`, which
releases `b`, which releases `c` - soft2d - so this is the first of four documents on the path the
text milestone's guest gate is waiting for.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0103 (2026-09-12T18:19:44Z):

`docs/GRAPHICS.md`, AND THE CORRECTION TO THE THREE BYTE SPANS.

THE NAMING DECISION, WHICH IS WHAT THIS DOCUMENT IS FOR

`framebuffer` meant five things in this tree: the UEFI boot surface in `bootproto`, the virtio-gpu
resource's DMA backing, the DisplayService scanout, an application's surface, and `pix::Target`. A
word that means five things is one every conversation has to disambiguate and every interface
eventually gets wrong - and the way it gets wrong is that somebody passes one of the five where
another was meant, which type-checks whenever both are a pointer and a length.

Each has its own name now, and `FRAMEBUFFER` is the NARROW one: the legacy linear boot surface and
nothing else. It was the word that meant everything.

GENERATED, FOR THE SAME REASON AS THE COLOUR PROFILE

A naming decision written in prose drifts the first time somebody adds a sixth meaning. The five
names, the ten ownership edges, the four integration routes and the six validation boundaries are a
registry; the document is written from it, with a SHA-256 over its canonical form.

THE OWNERSHIP IS AN ENUMERATION RATHER THAN A DIAGRAM. A diagram is checked by whoever reads it; a
list of edges is checked by a fixture, which holds the graph acyclic - a cycle is the failure that
turns a stack into a knot, and it is the one a picture never shows. Every edge states what CROSSES
it, because an edge with no payload is a dependency nobody has thought about.

THE ROUTES ARE FROZEN BEFORE ANY OF THEM EXISTS, because a provisional `gl*` or `vk*` API introduced
to draw a demo is the API the tree then has. Each names what it REFUSES, since the absence is the
decision, and the two the plan names by name - a common GL/Vulkan command language invented here, and
virtqueue descriptors reaching applications - are refused in the registry where a fixture can see
them rather than in a sentence.

THE BOUNDARIES ARE WHERE UNTRUSTED INPUT IS VALIDATED, and the point of naming one is that the layer
below may then ASSUME. A stack where every layer re-checks is one where the check that matters is the
one nobody wrote because everybody assumed somebody else had; a stack where none does is the other
failure. Six of them, including the one a driver is most likely to skip: what a DEVICE writes back is
untrusted input too, and a reply is not trustworthy because it came from hardware.

THE CORRECTION

The image and colour profile I published earlier today froze the wrong THREE BYTE SPANS: the minimum
row, the pitch and the visible bytes. Reading `a-common` for this item showed the three it names are
`minimum_visible_bytes`, `backend_access_span` and `allocation_len` - three different QUESTIONS
about one image rather than three sizes:

  - what a borrowed CPU view needs, which EXCLUDES the final row's padding, so a legal final row with
    no padding after it is accepted and a validator demanding `pitch * height` refuses buffers that
    are exactly big enough;
  - what the selected display or DMA backend may touch, which INCLUDES that padding where a scanout
    engine fetching whole rows reads it, and which a presentable or DMA image must OWN;
  - what the allocation actually is, which is a fact about memory and not a second answer to "how big
    is the image".

An implementation that stores one number answers all three with it and is wrong about two. The row
quantities are stated beside them as what the spans are computed FROM. My own fixture had not caught
it because it only checked that the three differed from each other, which they did.

AND THE FIVE FREEZE REQUIREMENTS

Reading the per-part Done clause also showed two of the five were not met by the colour profile as
first written. Added: the GUARANTEED MINIMA - a profile with no minima promises nothing, because
"supports large images" is satisfied by an implementation that refuses at 513 pixels and an
application written against it discovers the real limit in front of a user - and the CONFORMANCE
TOLERANCES, so "passes conformance" has a boundary rather than a judgement. The dither's tolerance is
EXACT, because its matrix and its phase are both stated: two implementations that disagree by
anything disagree about the rule rather than about arithmetic. The initialisation contract went in
with them, where the uninitialised case is a DISCLOSURE rather than an aesthetic problem.

WHAT WAS VERIFIED, AND HOW

`cargo test -p graphics-profile`: 22 passed, 0 failed - 7 more than this morning. The new ones hold
the five names distinct, the ownership graph acyclic in both the two-node and the long-path sense,
every route to naming its refusal, every boundary to naming what it checks, the pitch minimum to
actually reaching the extent minimum at the widest format (or the two minima contradict each other
and an application obeying both is still refused), and the dither tolerance to being the one exact
comparison.

`./src/tools/check-graphics-profile.sh` passes, regenerating and comparing every profile document.
`cargo clippy -D warnings`: clean, after two assertions over pairs of constants were moved into
`const` blocks - a runtime assertion over two literals is a test that cannot fail at a moment when
it could still matter. `./format.sh` was run and the suite re-run.

NOT PERFORMED: aarch64 and riscv64, and the full sweep.

`s-common` NOW HAS BOTH ITS DOCUMENTS, its registry, its gate, its minima and its tolerances. It
releases `a-common`, which releases `b`, which releases `c` - soft2d - which is what the text
milestone's guest gate is waiting for.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0103 (2026-09-12T18:29:04Z):

`RENDER2D_PROFILE_1.md`: THE HALF A FEATURE LIST CANNOT CARRY.

THE LIST WAS ALREADY THERE

`docs/gen/render2d/profile-1.md` names every entry the profile carries. Two implementations can
agree on that list entirely and produce different pixels from the same draw list, because a name does
not say what `ColorBurn` does at zero, what `SoftLight` does below a quarter, what a boolean union
does with an open subpath, or how far a curve may deviate from its flattening. This document is those
answers, generated from three registries.

WHAT IS FROZEN

  - THIRTEEN COMPOSITING OPERATORS, each as its `Fa`/`Fb` pair under one equation. The factors ARE
    the operator, which makes them checkable against one another rather than thirteen paragraphs -
    and a fixture holds every pair distinct, because two operators with the same factors are one
    operator under two names. `Plus` is not among Porter and Duff's twelve and is there anyway:
    additive light is what a glow and an emissive overlay are, and an implementation without it grows
    a private one.
  - TWELVE SEPARABLE BLEND MODES. The two with a division state their ENDPOINTS rather than leaving
    them to whatever the division produces, and `SoftLight`'s `D(Cb)` is a piecewise function of the
    BACKDROP with its threshold on the backdrop - the part that is got wrong.
  - FOUR NON-SEPARABLE MODES WITH THEIR WHOLE COLOUR MODEL: `Lum`, `Sat`, `SetLum`, `SetSat` and
    `ClipColor`. The clip is the step the mode's name does not imply, and it PRESERVES LUMINANCE
    while reducing chroma rather than clamping each channel - which is what every implementation that
    omits it gets wrong on saturated colours. `Lum`'s coefficients are the compositing
    specification's own fixed triple and stay fixed in every colour space: using the destination's
    luminance would make `Luminosity` give a different result for the same two colours depending on
    which space they were tagged with, and the mode is defined on the numbers rather than on the
    light.
  - THE GEOMETRY NUMBERS. A quarter of a PHYSICAL pixel, because flattening in logical pixels makes a
    curve twice as coarse on a two-times display - facets on exactly the screens that show them best.
    A maximum depth of sixteen with a stated outcome that is NOT a refusal, because a legal drawing
    must not fail for a reason nobody can act on. A projective `w` epsilon, with the horizon clipped
    in homogeneous space BEFORE the divide: dividing first produces a vertex at ten million pixels and
    a rasteriser that spends a second on one triangle. And a point-coincidence epsilon that is a
    SEPARATE, smaller number, because merging vertices a quarter of a pixel apart collapses thin
    features that were meant to be there.
  - ALL EIGHT BOOLEAN ANSWERS. The result is POLYGONISED and the document says so: preserving curves
    needs exact curve-curve intersection, whose answer is approximate anyway, so the honest form is
    the polygon at a tolerance the caller knows.
  - THE LCD NUMBERS the word "LCD" does not carry: the five-tap FIR, and the gamma coverage is blended
    through. Blending it linearly makes light-on-dark text look bolder than dark-on-light at the same
    weight, which is the artefact reported as "the font renders too thin" - a gamma question rather
    than a font question.
  - THE CONTRACTS A RECORDING API HAS THAT A DRAWING API DOES NOT. Eight prepared-list dependencies,
    each with the reason a change invalidates it, so `is_compatible` is a list rather than a
    judgement and a test can change one ALONE. A mismatch as a TYPED requirement and never a silent
    re-prepare, because a caller getting a full preparation sixty times a second has a performance bug
    it cannot see. CONTENT IS NOT STRUCTURE, so a new video frame refreshes one cache and re-flattens
    nothing. The reusable builder allocating nothing within its reservation and refusing BEFORE any
    replay begins, because a list that half-drew and then refused has already put pixels on screen.
    And the per-node filter contract, whose bounds map is the whole reason a blur over a small dirty
    region does not cost a full-screen blur.
  - THE GUARANTEED MINIMA, which already existed in `Render2DLimits` and are now published in the
    document that promises them.

WHAT WAS VERIFIED, AND HOW

`cargo test -p graphics-profile`: 28 passed, 0 failed - 6 more, each checking a VALUE rather than a
presence: every operator pair distinct and the four that define the rest exactly right; `ColorDodge`
and `ColorBurn` stating their endpoints and `SoftLight`'s threshold being on the backdrop; the
luminance coefficients summing to one and the clip preserving luminance; the coincidence epsilon
strictly below the flattening tolerance; every one of the plan's eight boolean questions having an
answer, and no answer short enough to be a restatement; and every prepared-list dependency the plan
names being present with a reason.

`./src/tools/check-graphics-profile.sh` passes: 13 operators, 12 separable and 4 non-separable blend
modes, 8 boolean answers and 8 prepared dependencies, hashed, and every generated document matching.
`cargo clippy -D warnings`: clean, after four more assertions over pairs of constants were moved into
`const` blocks. `./format.sh` was run and the suite re-run.

NOT PERFORMED: aarch64 and riscv64, and the full sweep.

WHERE THIS SITS

`s-2d` releases `b`, and `b` releases `c` - soft2d. `s-common` and `s-2d` now both have their
documents, their registries and their gate. The remaining specification on that path is none: the
next work is `a-common`, the code the two freezes release.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0103 (2026-09-12T18:47:18Z):

`graphics-core`: THE CANONICAL IMAGE MODEL.

NINE ITEMS, ONE CRATE

The plan lists the image model, the two coordinate spaces, the format set, the alpha rules, the
colour model, the semantics, the borrowed views, owned images and their zeroing as nine items. They
are one crate, because a layout without a format set, an alpha rule, a colour model and a meaning is
a struct nobody can validate - the constructor that makes the type worth having needs all of them.

THE PROFILE IS THE LIST AND THE CRATE IS THE CODE

Neither restates the other. Every format name, byte count, alpha rule, colour constant, byte-span
definition and operation table comes from `graphics-profile`'s frozen registry, and a fixture holds
the enumeration and the registry to describing the same twelve formats IN THE SAME ORDER with the
same widths and the same alpha rules. A second copy of a list is a second answer, and the second
answer is the one that is out of date.

THE THREE SPANS, IMPLEMENTED AS THREE

  - `minimum_visible_bytes` EXCLUDES the final row's padding. A buffer that is exactly big enough for
    the image it holds is accepted; a validator demanding `pitch * height` refuses it, and that is
    the ordinary case for a tightly packed last row.
  - `backend_access_span` takes whether the backend reads that padding as an ARGUMENT rather than
    assuming. Assuming it always does makes every presentable image own bytes it does not need;
    assuming it never does is the out-of-bounds read.
  - `OwnedImage` allocates the backend span, because an allocation one row-padding short is an
    out-of-bounds read BY HARDWARE - which no bounds check in this process can catch.

TWO COORDINATE SPACES, TWO TYPES

The drawing space is `f32` and SIGNED: an unsigned parameter makes "half a pixel to the left of the
origin" unrepresentable, and geometry has to clip off the left edge as naturally as off the right.
The pixel space is what damage, a scissor, a surface extent and everything on the wire is in. Both
HALF-OPEN, so adjacent rectangles tile - a closed rectangle is why two adjacent damage regions redraw
a shared column twice, which is invisible until it flickers.

A NaN EXTENT IS EMPTY, and saying so needs the comparison written out rather than a plain `<= 0.0`:
every comparison with NaN is false, so the plain form answers "not empty" for a rectangle nobody can
place, and the rectangle is then drawn. Clippy pushed back on the negated comparison, which was the
right prompt to write the reason down rather than the right prompt to change the behaviour.

THE COLOUR MATRICES ARE DERIVED AND NOT TABULATED

A tabulated matrix is a fourth place the primaries live and the first one somebody updates without
the others. The derivation is arithmetic over the chromaticities the profile already froze, and the
fixture checks it by the property every correct derivation has - THE PRIMARIES MUST REPRODUCE THEIR
OWN WHITE POINT - and by a round trip, which is what catches a TRANSPOSED matrix, since a
transposition is still invertible and passes every other check.

Bradford adaptation is skipped when the white points match, and that is not an optimisation: every
space in the profile is D65, so the common case would otherwise pay a matrix pair that changes
nothing except in the last bits. `libm` supplies the transcendentals; `core` has none, and a
hand-rolled `powf` in a colour pipeline is a different colour pipeline.

WHAT THE CONSTRUCTORS REPLACE

An application computing a length and building an aliasing mutable slice out of a raw pointer -
`from_raw_parts_mut(surface.addr() as *mut u8, target_len)` - which is unsound whenever the length is
wrong and is exactly as easy to write when it is. A view that cannot be constructed from a short
buffer cannot be used over one.

AND AN OWNED IMAGE IS ZEROED, WHICH IS A SECURITY PROPERTY

A buffer recycled between Domains holds whatever the last one put in it. An image whose bytes were
never written shows that, and the bug reads as a flicker rather than as a disclosure - so it is not
reported. Export ZEROES the padding rather than trusting it to have stayed zero, because a drawing
routine that wrote a whole pitch is not a bug and its padding is still not content; two exports of one
image are then the same bytes, which is what makes it hashable and a golden comparison meaningful.

WHAT WAS VERIFIED, AND HOW

`cargo test -p graphics-core`: 9 passed, 0 failed, over the enumeration-against-registry check, the
three spans as three numbers with a buffer of exactly the visible bytes accepted and one byte fewer
refused, every way a layout has been wrong, a 5:6:5 firmware mode accepted and an overlapping one
refused, the operation table keyed by meaning, a zeroed allocation that owns the backend span, both
coordinate spaces half-open with a signed origin, the colour matrices against their white points and
their round trip, and every transfer function through its own inverse at sixty-five points.

A FIXTURE OF MINE WAS WRONG AND THE CODE WAS RIGHT: I asserted that Display P3's green is outside Rec.
2020, and it is the other way round - P3 is a SUBSET. The fixture now checks both directions, which
is the stronger statement.

`cargo clippy -D warnings`: clean. `verify-model`: 157 passed, with the crate in the
release-required host list and discovered by `host-suites` - 92 runnable suites. `./build.sh --part
user` for x86_64: built. `./format.sh` was run and the suite re-run.

NOT PERFORMED: aarch64 and riscv64, and the full sweep.

WHERE THIS SITS

`a-common` was 23 items and is now 13. What remains there is the multi-plane model, converging and
migrating `pix`, and the surface, present-queue, timing, damage and completion work that belongs with
the WSI. `b` - `render2d` itself - is what `c`, soft2d, is waiting for.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0103 (2026-09-12T19:04:14Z):

`render2d`: THE DRAWING API, AND THE BOUNDARY A GPU BACKEND LATER ARRIVES AT.

TWELVE ITEMS, ONE CRATE

A canvas without paints, paths, layers, filters, glyph runs, a list, a prepared key and an error type
is a struct with no calls on it. They went in together.

WHAT A CANVAS PRODUCES IS A LIST AND NOT PIXELS

That indirection is the design, and it pays for itself before the GPU backend it was also chosen for
exists: the same list is cacheable per component, analysable for damage, replayable, and TESTABLE
WITHOUT ANY BACKEND AT ALL. Every fixture in this crate is a host test for that reason.

THE DECISIONS, AND WHY EACH IS THE WAY IT IS

  - THE STATE AT THE TIME OF THE CALL IS RECORDED WITH IT. A list whose commands referred to a
    mutable state object would draw differently depending on when it was replayed.
  - `restore` WITHOUT A `save` IS AN ERROR AND NOT A NO-OP. Treating it as one is how a component
    that restores once too often silently inherits its parent's clip - and the drawing that results
    is wrong somewhere else, in a component that did nothing. An UNCLOSED LAYER is refused rather
    than replayed, because its contents went into an offscreen nothing composites: a drawing that is
    simply missing, with nothing to say why.
  - DEDUPLICATION IS NOT AN OPTIMISATION. A component drawing one rounded rectangle forty times
    records it once, which is what keeps the resource ceiling meaningful - a list storing forty
    copies would exceed it for a drawing that has one shape in it.
  - STROKE SCALING IS A CHOICE AND BOTH ARMS ARE WANTED. A shape scaled up should usually get a
    thicker outline; a hairline, a selection rectangle and a diagram's grid should stay one pixel at
    any zoom. An API with only the first makes the second a caller dividing by its own zoom, which is
    wrong under rotation and meaningless under perspective.
  - THE TRANSFORM IS PROJECTIVE, because the affine version is the one that has to be replaced later.
    A point at or beyond the horizon has NO image and answers `None`: dividing by a `w` near zero
    produces a vertex at ten million pixels and a rasteriser that spends a second on one triangle. A
    rotated rectangle's bounds take all FOUR corners, because a bound from two is smaller than the
    drawing - which is how a damage rectangle comes to clip the thing it was computed for.
  - A PATH ANSWERS A HIT TEST, under both fill rules, flattened at the profile's ONE tolerance. That
    is why a point can never be inside for a hit test and outside for the fill that drew it, and an
    application that cannot ask implements its own geometry. Tight bounds are separate from loose
    ones because a control point is often well outside the curve, and a layer sized by the loose
    bound allocates a bigger offscreen every frame.
  - A FILTER GRAPH IS ACYCLIC BY CONSTRUCTION - a node may only read nodes before it - rather than by
    a check a later edit can defeat. Its bounds map runs BACKWARDS from the output, which is the
    direction the question runs.
  - EVERY HANDLE IS TYPED. A single integer index shared by paths, images, fonts and filters is an
    index a validator cannot check: index seven is a valid path and a valid image, and the drawing
    that confused them still replays.
  - THE ERROR TYPE DISTINGUISHES WHOSE FAULT IT IS. An out-of-range handle is a defect in the
    recorder; a limit exceeded is a drawing that has to be split or simplified, with the ceiling
    NAMED so the caller knows which way; an unbalanced save is a control-flow bug three functions
    away. A single "invalid" would leave every one of them to be found by reading drawing code.

THE CANONICAL ENCODING, AND THE CONTENT-VERSUS-STRUCTURE RULE

A cache key is a hash of bytes, so the list has a byte form whether or not anything sends it
anywhere - and it is explicitly NOT a wire ABI: not stable across releases, not endian-defined, not
rights-bearing, not safe to accept from another process. A fixture requires every field the encoding
carries to CHANGE the digest, because a field the encoding loses is a field a cache HIT loses.

The content generation is in the ENCODING and not in the PREPARED KEY, and that difference is the
whole rule: two frames of a video are different DRAWINGS, so they must not share a cached raster, and
they are the same STRUCTURE, so the prepared list stays valid. A fixture checks both halves at once.

WHAT WAS VERIFIED, AND HOW

`cargo test -p render2d`: 12 passed, 0 failed, all without a backend - recording, deduplication, the
save stack in both directions, the unclosed layer, an unknown handle refused by name, a ceiling
refused while building, the acyclic graph and its backwards bounds map, the projective horizon, a hit
test under both fill rules with the two rules actually differing, the encoding distinguishing every
field it carries, each prepared dependency changed ALONE, and the operator and blend enumerations held
to the frozen registry.

`cargo clippy -D warnings`: clean, after three NaN comparisons were written out with `partial_cmp` -
which was the right prompt to write the reason down rather than to change the behaviour: every
comparison with NaN is false, so a plain `<= 0.0` lets a NaN through and divides by it.
`verify-model`: 157 passed, with the crate in the release-required host list and discovered by
`host-suites` - 93 runnable suites. `./format.sh` was run and the suite re-run.

NOT PERFORMED: aarch64 and riscv64, and the full sweep.

WHERE THIS SITS

`b` was 19 items and is now 4: what remains there is boolean path operations, the shared flattening
implementation, the remaining query surface, and the backend-free host-test roster. `c` - soft2d, the
CPU implementation - is what the text milestone's guest gate is ultimately waiting for, and it now has
an API to implement.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0103 (2026-09-12T19:29:42Z):

P02M0103b IS COMPLETE. The four items that were open are closed: path OPERATIONS and QUERIES, the ONE
flattening with the horizon rule answered, the backend-free host-test roster, and the prepared/backend
split that the previous section already carried.

WHAT WAS DELIVERED

`render2d::flatten` is now the single flattening for the whole crate and it clips against the horizon
the way the profile froze it: `HORIZON_RULE` says "clip the segment against w = epsilon in homogeneous
space before dividing; a segment entirely beyond it is dropped, and the primitive is refused only if
nothing survives", and that is now what the code does rather than what the document said. Curves are
subdivided by de Casteljau ON THE HOMOGENEOUS CONTROL POINTS, which is the correct split for a
projected curve: the image of a Bezier under a projective transform is a rational Bezier carrying its
own `w`, so splitting before the divide splits the curve that is actually drawn. The convex hull
decides a drop without subdividing - every point of the curve is a convex combination of its control
points, so a curve whose control points are all beyond the horizon is beyond it everywhere. The
flatness test is device-space distance from the CHORD's line, bounded by the profile's
`MAX_SUBDIVISION_DEPTH`, and a non-finite deviation counts as flat so an overflowed curve stops
recursing instead of subdividing into more overflow.

`Error::BeyondHorizon` existed in the enumeration and nothing produced it. It is now produced by
`flatten_checked`, and the `Canvas` calls that take geometry - `fill_path`, `stroke_path`, `set_clip` -
refuse at the call that made the mistake rather than at replay. The check costs nothing on an affine
transform, because an affine transform has no horizon and every point of it has an image.

`Transform::inverse` is the full 3x3 adjugate and not the affine shortcut, because a projective
transform is exactly the case where the shortcut is wrong. A singular transform returns `None` rather
than one of the many points that map to each image. `map_homogeneous` exposes the pre-divide map that
the clipping needs.

The boolean output order gained its third key. The frozen answer is "sorted by their bounding box's
minimum y then minimum x then their first point"; the implementation stopped at the box, which leaves
two contours sharing a box corner - a shape and the hole that touches it there - in whatever order the
traversal found them.

One square root now serves the crate. There were two private copies, in `transform.rs` and in
`query.rs`, and a third was about to be written in `flatten.rs`.

WHAT WAS VERIFIED, AND HOW

`cargo test` for the crate: 21 passed, 0 failed, every one of them WITHOUT a backend. The three new
fixtures are the horizon (a square straddling it keeps the half that has an image and comes back OPEN,
because a fill that closed it would close it across the gap the horizon made; a square entirely beyond
it is a REFUSAL and not an empty drawing; the refusal arrives at `fill_path`, `stroke_path` and
`set_clip`; an affine transform refuses nothing), the four boolean answers the overlapping-squares
fixture did not reach (an open subpath closed with a straight segment, XOR, the canonical order, and
determinism asserted as byte equality of verbs and points), and projective composition plus inversion
inside the transform fixture.

`cargo clippy --all-targets` with `-D warnings`: clean. `./format.sh` was run and the suite re-run.

NOT PERFORMED: the full gate sweep, aarch64 and riscv64. They are deferred to the end of the next
chunk rather than run twice.

WHERE THIS SITS

`b` is 0 open items. `c` - soft2d, the CPU implementation of the whole profile - is next, and it is
what the text milestone's guest gate is waiting for. The milestone as a whole is 95 open items.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0103 (2026-09-12T21:01:11Z):

P02M0103c IS IMPLEMENTED AND ONE OF ITS FOURTEEN ITEMS IS NOT MET. `soft2d` exists, implements the
whole of `Render2D Core Profile 1`, and is registered as a source, a host suite and a benchmark. The
performance floor is over its ceilings and the item stays open; the numbers are below and in
`docs/PERF.md`.

WHAT WAS DELIVERED

`src/user/libs/graphics/soft2d` - eleven modules. The two phases are the whole shape of it:
`prepare` validates the list, flattens every path under its own command's transform, converts every
stroke into a fill (caps, joins, miter limit, dashing by arc length), builds the edge lists, computes
per-command device bounds, bins them into 64-pixel tiles, builds the image pyramids, works out the
filter expansion from the graphs the list actually carries rather than from the profile's ceiling, and
reserves the layer surfaces, the clip masks, the coverage row and the span buffers - then refuses with
a named limit if the total is over the profile's scratch ceiling. `render` replays and allocates
nothing.

The rasteriser is EXACT IN X AND SAMPLED IN Y at sixteen sub-scanlines, with an active edge list so a
six-hundred-edge stroke is not tested against every sub-scanline of every tile. Antialiasing off is
one sample at the pixel centre in BOTH directions, which is what a pixel-exact grid needs. The aliased
integer line is a separate path with the rule the milestone states - both endpoints included, ties
toward the smaller minor coordinate, clipped before rasterising - and it is reached by a one-pixel
aliased stroke of an open polyline, not guessed at for shapes.

Clipping is a coverage mask with a rectangle fast path that costs no storage, an inverse flag that is
one subtraction over the same mask, and an alpha-mask clip from an image. Both were missing from the
API: `Command::PushClip` gained `inverse` and `Command::PushClipMask` is new, because the profile
lists "rect, rounded rect, path, nested, alpha mask, inverse" and `b` had the first four.
`FilterNode::Backdrop` is new for the same reason: a frosted panel is a blur of its backdrop, and
without the node the whole class of backdrop effects has to be built by drawing the scene twice.
Dash patterns got their own resource kind - they had been sharing the gradient stop table, where a
dash length is an offset with a colour attached.

THE ONE PIXEL PIPELINE IS NOW SHARED AND CALLED. `graphics-core` gained `pixel` (the stage order,
the decoder and encoder, the packed-layout pair, the transfer tables, the quantiser and the ordered
dither), `composite` (the thirteen operators and sixteen blend modes with their frozen equations, the
luminance and saturation model and the gamut clip) and `sample` (nearest, bilinear, Mitchell bicubic,
the pyramid and a bounded anisotropic tap). `render2d` re-exports the operator, blend, quality and
spread enumerations from there rather than declaring its own; `pix` calls the shared compositor and
the shared packer instead of its private integer blend; DisplayService reaches the same code through
`pix::blit`, which is its copy-and-scale path.

That change corrected a defect. `pix`'s integer source-over divided by `out_alpha * 255` in
integers; over the whole 8-bit space, compared against the exact answer, it was wrong by up to 255
levels where the resulting alpha was small. The shared path is wrong by at most one, which is the
rounding. One webp fixture pinned a hash of the OLD composite and now pins the corrected one; the
decoded frames' hashes are unchanged, because the decoder was never the question.

WHAT WAS VERIFIED, AND HOW

`cargo test` for `soft2d`: 20 passed, all without a display. Rectangles half-open and exact, coverage
along a known edge at half a pixel, both fill rules differing, caps and joins reaching where they
should, dashing leaving the gaps it states, clips nesting and inverting and rounding, group opacity
against a hand-computed reference (half over half is three quarters when composited twice and a half
when composited once), every operator and every blend mode reaching the pixels, every filter node
alone plus a composed shadow and a backdrop blur, every glyph kind and the cache keying them apart,
conservative damage clipped to the target, guarded canaries around a target with pitch padding, the
wide span path bit-identical to the scalar reference at twelve lengths around the lane width, a
cancelled frame stopping at a tile boundary, prepared-list invalidation by name, and hostile input -
twelve extreme coordinates under four transforms each, with the canary checked every iteration.

`graphics-core`: 17 passed, including the transfer tables held to the profile's round-trip tolerance
on all four transfer functions, the premultiply-before-interpolate halo case, the pyramid averaging in
linear light, and every operator and blend mode against its frozen equation.
`render2d`: 21 passed. `pix`: 9 passed. `check-host-tests.sh`: 94 suites, all green.
`cargo clippy --all-targets -D warnings` on all five crates: clean. `./format.sh` was run.

THE MEASUREMENT, AND THE ITEM THAT IS NOT MET

`./bench.sh --suite soft2d`, 640x480, five warmup frames and thirty measured, on the Xeon 8272CL
recorded in `docs/PERF.md`:

    scene           prepare    replay median   replay p99   ceiling
    UI-basic         1.1 ms        79.3 ms       81.9 ms    16.7 ms
    UI-effects       1.3 ms       385.1 ms      401.7 ms    66.7 ms
    vector-stress    7.2 ms       245.8 ms      247.3 ms    66.7 ms
    image-stress    43.5 ms       267.2 ms      287.4 ms    16.7 ms

That is between four and sixteen times over. The ceilings were not moved and the budgets were not
raised: the item stays open and the runner is registered in `bench.sh` as a measurement rather than
in `check.sh` as a gate, because a gate that cannot pass is not a gate. The numbers are already three
to six times better than the first working version (289, 2401, 1198 and 786 ms), through five changes
that were worth making on their own - transfer tables instead of a power per channel per pixel,
shaders built once per frame instead of once per tile, an active edge list, an `f32` tile working copy
with the canonical half-float format kept for layers and filter intermediates where the profile fixes
it, and surfaces addressing their own bytes instead of building a checked view per pixel.
`docs/PERF.md` records what the remaining gap is made of, measured: an EMPTY list costs 21 ms, which
is the tile decode and re-encode alone; one full-screen antialiased fill costs 30 ms more, of which
13 ms is the sixteen sub-scanlines and 17 ms the span composite. Closing it needs an exact-area
rasteriser, a wider span composite and an opaque-fill path that skips reading the backdrop.

NOT PERFORMED: the full gate sweep, aarch64 and riscv64, and the YUV source of the image scene, which
needs the multi-plane image model `P02M0103a-common` still owes.

WHERE THIS SITS

`c` is 1 open item of fourteen. The milestone is 82 open items, down from 95.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0103 (2026-09-12T21:09:06Z):

THE MULTI-PLANE IMAGE MODEL IS IMPLEMENTED, which was the largest functional hole left in the 2D half:
every mandatory format is RGB or RGBA, so a video player, a camera preview and a hardware decoder had
to convert every frame to RGBA before `render2d` would draw it - a full-frame conversion on the CPU,
once per frame, for the one workload where that cost is least affordable.

WHAT WAS DELIVERED

`graphics-core::planar`: `PlanarFormat` (NV12, I420, P010), `YuvMatrix` (BT.601, BT.709, BT.2020),
`YuvRange`, `MultiPlaneLayout` with a checking constructor, and `MultiPlaneView` with the plane
reconstruction. A SEPARATE TYPE AND NOT A WIDER `ImageLayout`, so a single-plane image never grows a
plane count, a subsampling factor, a chroma siting and a range it cannot use.

Every rule comes from the frozen registry rather than being restated: the per-layout pitch rule (an
interleaved chroma row is TWICE its sample count and a planar one is once, which is the arithmetic
that puts a decoder half a row out), the odd-extent rule (the chroma extent is the ceiling of half the
luma extent and the final sample is REPLICATED rather than read past), the limited and full ranges at
both bit depths, P010's ten bits in the HIGH bits of a little-endian word with the low six ignored on
read, the siting (left horizontally, centre vertically) with bilinear reconstruction, and the crop
alignment. The inverse matrix is DERIVED from each entry's two coefficients rather than tabulated.

AND THE ORDER OF OPERATIONS IS THE PROFILE'S, which is the part a recipe written for RGB gets wrong:
planes are reconstructed and the matrix applied FIRST, producing ENCODED RGB; only then does transfer
decoding, primary conversion and premultiplication happen. `Sampler` gained a PLANAR source rather
than a second sampler: the texel fetch differs and everything after it - the transfer function, the
primaries, the premultiply, the filter, the pyramid - is the same code, which is what "drawn through
the same shared pipeline" has to mean if a video frame is to composite identically to an image of it.
`Pyramid::from_sampler` is how a planar source gets a pyramid without a second reconstruction.

`soft2d`'s `ImageSource` gained `planes`, defaulting to `None`: a source that has planes answers
there INSTEAD of at `image`, because a decoded frame has no single-plane view to fall back to and
synthesising one would be the conversion this model removes.

WHAT WAS VERIFIED, AND HOW

`graphics-core`: 19 passed. The new fixtures pin the registry agreement by name, both pitch rules, the
odd extent, the malformed-plane REFUSAL, limited-range white at 235 against full-range white at 255
with the same bytes read as the other range being a different colour, BT.601 against BT.709 against
BT.2020 on identical bytes, P010's low six bits being ignored, I420's three planes agreeing with
NV12's two, the crop alignment in both directions, and a short plane refused rather than read past.
The sampler fixture checks that white in is white in LIGHT - which is the transfer function having
been applied AFTER the matrix and not before it - and that a pyramid built from a planar sampler
averages in that same light.

`soft2d`: 21 passed, including a video frame drawn end to end: limited-range 235 comes out white,
16 comes out black, the frame is opaque, and the edge between the halves lands in the middle of the
doubled destination rather than shifted by a wrongly sited reconstruction.

`cargo clippy --all-targets -D warnings`: clean. `./format.sh` was run.

The benchmark's image scene gained the YUV source it always named - a full-screen NV12 frame in
Rec. 2020 limited range - which is why that row moved from 267 ms to 419 ms: it is the workload the
scene is for, and it was previously absent because the model did not exist. `docs/PERF.md` is updated.

NOT PERFORMED: the full gate sweep, aarch64 and riscv64.

WHERE THIS SITS

`a-common` is 2 open items: converging the remaining pixel-plane types with the wire-side IDL package,
and migrating `pix::RgbaImage` onto the model. The milestone is 81 open items.

IMPLEMENTER'S VERIFICATION NOTE ON P02M0103 (2026-09-13T00:23:14Z):

THE GRAPHICS WORK IS IN THE IMAGE NOW, which it was not before: `graphics-profile` and
`graphics-core` are declared libraries staged at `lib/graphics/`, and `pix` links against the second
of them - so the one pixel pipeline is not merely shared in the source tree, it is the code the
shipped `pix.lslib` calls. Three things had to be settled for that:

THE LIBRARY CATEGORY. `system-manifest` reads a library's staging directory out of its source path
when the path's leaf IS the owner's name, which is most of them; `graphics-core` lives in
`user/libs/graphics/core`, so it is named explicitly beside the five that were already named. The
alternative was renaming the directory or the crate, and both make the import in every consumer read
worse than one line in the table that already exists for this.

THE TRANSCENDENTALS. `graphics-core` uses `libm` for the transfer functions' `powf`, and `libm` is
not a provider anything else in the image links against - so the archive is linked INTO the library,
which is what `vorbis` already does for the same reason and through the same branch.

AND THE BUILD PROVED IT: `./build.sh --arch all` stages both libraries on all three architectures,
and `LIBER_DEVELOPMENT=1 ./build.sh --arch all` does too.

VERIFIED: `check-host-tests.sh` - 94 suites, all green, including `graphics-core` (19),
`graphics-profile` (30), `render2d` (21), `soft2d` (21) and `pix` (10). The `graphics-profile` gate is
green: every generated document matches its registry, with the WSI profile now among them.
`source-hygiene` is clean. `cargo clippy --all-targets` with `-D warnings` is clean on every crate
this work touched.

NOT PERFORMED: the full gate sweep was stopped part way - one aarch64 IOMMU gate takes over an hour
under TCG here - and three gates are red for reasons this work did not cause (`foreign-facilities-guest`
needs the foreign audit substrate staged, and `boot-harness` and `qemu-arch-profiles` fail on a
DMA-mode change in the harness and loader that this work does not touch).

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0103 (2026-09-13T04:30:00Z):

THE `s` FREEZE IS COMPLETE. All eight of its items are done, and the part went 70 to 78.

WHAT WAS FROZEN, AND WHAT EACH FREEZE HAD TO DECIDE RATHER THAN DESCRIBE:

  RENDER3D_PROFILE_1.md. The feature list already existed and said what a backend must be able to do;
  this says what the answers ARE. Seven colour formats with all eight capability columns and full
  names rather than `RGBA8` shorthand, ten vertex formats with their normalisation rules, eight
  rasteriser state fields with the depth-bias equation written out, the EXACT 2x and 4x sample
  positions with nine MSAA answers, five depth formats, twelve sampler answers including the LOD
  formula and the cube seam rule, seven hazard rules, six submission rules and eleven minima. The
  three questions the plan named as the freeze's own are answered rather than listed: `Depth32F`
  compares the STORED FLOATS and does not quantise the incoming depth; `ClipCoordQ` is seven rules
  including the non-finite refusal and an epsilon that is the SAME value the 2D profile uses; and the
  submission/readback lifecycle is bound before a backend exists.

  SHADER_IR_1.md. Fifteen numeric answers closing every behaviour a shading language usually leaves
  open - signed overflow wraps, division by zero has a value, an out-of-bounds read returns ZERO
  rather than clamping to a plausible neighbour - plus ten accuracy bounds, eleven layout rules,
  seven stage rules covering helper lanes and post-`discard` derivatives, seven StrictF32 rules, and
  seven encoding rules under which an unknown instruction id is a REFUSAL rather than a skip.

  SCENE3D_PROFILE_1.md and SCENE3D_EXTENDED_1.md. The core carries the hierarchy, the camera, three
  queues, culling, instancing, four materials with their equations, unshadowed multi-light
  accumulation IN A FIXED ORDER - because floating-point addition is not associative - and picking.
  The extended carries the PBR terms as equations with a `why this one` column each, the split-sum
  approximation with BOTH halves, named shadow bias values, the bloom knee and kernel, the fog
  equation, and a root-motion policy CHOSEN rather than required.

  THE THREE CROSS-CUTTING ITEMS. Guaranteed minima exist for all four profiles under one naming
  convention and `profile-doc --check` now REFUSES a document that does not state a floor its
  registry publishes - which found two floors missing from the 2D document the first time it ran.
  Twenty-one conformance thresholds across five profiles are in one registry and rendered into each
  document's conformance chapter and into the canonical form its hash covers, with the classifications
  that have NO tolerance in the table saying so. Seven profiles are bound to their documents by
  `docs/gen/profiles.manifest`, the canonical encoding is eleven stated rules in
  `graphics_profile::hashing`, and the gate recomputes every hash from the registry and refuses three
  separate disagreements.

  AND THE GRAPHICS_2D/3D MOVE, which is a MOVE and not a write: neither document exists, so what the
  item owns is where their content lives. The part-`i` item that publishes them is restated to
  publish OVERVIEWS - every clause it dropped is one a frozen profile document now carries.

EVIDENCE: 33 new tests in `graphics-profile` (68 total, green). They are consistency tests rather
than correctness tests, and that distinction is worth stating: nothing can test whether 4 ULP is the
right bound for a sine, because it is a decision. What they hold is that no question is answered
twice, that every sample position is inside its pixel and the 4x grid is genuinely rotated, that no
format row contradicts itself, that every material carries an equation, that the no-tolerance entries
say NONE, and that the hash excludes itself and the prose. `./gen.sh --check` reports no drift across
16 packages, the aggregate and every profile.

NOT PERFORMED: nothing here has been implemented against. These are specifications, and a
specification's real test is a backend built to it disagreeing with another - which is parts `e`
through `i` and is not started.

A JUDGEMENT THAT SHOULD BE READ BY SOMEBODY ELSE: every number in these four documents is a choice
this implementation made. They follow mainstream practice where one exists - the rotated-grid sample
positions, GGX with Smith height-correlated visibility, column-major matrices, the 0.04 dielectric
F0 - and where practice is split the reason for the side taken is written beside the value. A
reviewer who disagrees with one should say so now: after a backend exists, changing one is a version
change and a re-measurement.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0103 (2026-09-13 18:20):

THE PIXEL-PLANE CONVERGENCE, WHICH IS THE `a-common` ITEM'S LAST OPEN HALF. Four types described a
plane of pixels in this tree: `pix::Image`, `pix::Target`, `term::Geometry`/`term::Raster` and
`surface::Mapping`. They are now one description - `graphics_core::ImageLayout` - and one packer.

WHAT CHANGED, FILE BY FILE:

  `graphics-core`   `PackedRgbLayout::from_masks`, `ImageLayout::scanout` and
                      `PixelFormat::packed_masks` (plus `PixelStorage::packed_masks`, which asks the
                      same question of either arm). `wire.rs` gained `From<wire::PixelFormat>` in
                      both directions, so a consumer holding ONE field rather than a whole descriptor
                      goes through the same door.
  `term`            `Geometry` deleted. `Raster` is `{ base, layout, packed }`; `new` takes an
                      `&ImageLayout` and refuses only what is its OWN limit (an element wider than
                      the `u32` it packs into, and a `pitch * height` that does not fit). Its private
                      `channel`/`pack` arithmetic is `graphics_core::pixel::write_packed`.
  `surface`         `Mapping` carries an `ImageLayout`; `framebuffer()` is `layout()`. The format
                      check is on the core enumeration after the wire conversion, and the length
                      check is `backend_access_span(true)` rather than a hand-multiplied product.
  `pix`             `Target::from_layout` (adopt a description the display already made);
                      `channels()` returns `Option` and comes from the registry; `BGRA8_MASKS` and
                      the `Known(_) => BGRA8_MASKS` fallback are gone.
  `console_service` `geometry()` deleted; `make_surface` takes the layout the mapping reported.
                      `Console::fb` is an `Option<ImageLayout>` rather than a `Framebuffer` that had
                      to have a zero-valued default for the headless case.
  `imgview`         the viewport arithmetic takes an `Extent2D` (which is all it ever used) and the
                      destination is `Target::from_layout`; the eight-argument rebuild is gone.
  kernel            `console.rs` holds the one-way adapter from the boot-protocol framebuffer record
                      into the image model. `graphics-core` is named in the kernel's dependency list;
                      it was already in its graph through `pix`.

WHAT I DELIBERATELY DID NOT DO. `PixelFormat::packed_masks` does NOT describe the fourth lane of
`B8G8R8X8`/`B8G8R8A8`. The shared packer writes a declared reserved span with ALL BITS SET, so
describing one would turn every `0x00rrggbb` a blitter writes into `0xffrrggbb` and would write over
a destination's alpha - three blit tests failed on exactly that byte when it was tried. The boot
console's pixel output is therefore byte-for-byte what it was.

NOT PERFORMED: the guest boot suite. The kernel console and ConsoleService are both on this path and
a boot run is what proves them; it is held for the end of the whole job with the other long runs, by
the project owner's instruction that long tests run last and only over what needs them.

IMPLEMENTER'S FOLLOW-UP ON P02M0103 (2026-09-13 20:10):

THE GUEST RUN FOUND TWO DEFECTS THE HOST SUITES COULD NOT, and both are worth reading as evidence
about where this kind of change goes wrong rather than as two fixed bugs.

ONE: `term::Raster::new` REFUSED A NAMED FORMAT. I converged the renderer onto `ImageLayout` and made
its constructor take only `PixelStorage::PackedRgbUnorm` - which is what firmware describes and what
the boot console therefore hands it. A display server hands over `PixelStorage::Known(B8G8R8X8Unorm)`.
So the boot console kept drawing and every userspace VT went blank: `surface::Mapping` mapped the
pixels, ConsoleService reported online, and `make_surface` answered `None` for every VT. Nothing in
the host suites could see it - `term`'s own tests build their rasters from masks, because that is what
`term`'s own callers did before this change. The fix is one line (`layout.storage.packed_masks()`),
and the test that would have caught it now exists: a raster built from each arm, required to pack the
same pixel to the same bytes.

TWO: A WIRE ORDINAL MOVED UNDER TWO HAND-WRITTEN HARNESSES. `liber:display@1`'s own one-member
`pixel-format` was deleted in favour of `liber:graphics@1`'s twelve-member one, and `b8g8r8x8` moved
from 0 to 3. Every producer and consumer that NAMES the value was unaffected. Two harnesses in
`src/kernel/tests.rs` stand in for DisplayService and hand-wrote the byte as `0`, which is now
`a8-unorm`; the client refused the surface, imgview exited, and the harness waited three minutes for a
present. That is the exact drift the kernel's own dependency list already warns about for
`device-proto` and `network-proto` - "answering it from a hand-written copy of the wire format is how
the two drift apart" - so `graphics-proto` joins them and both harnesses name the value.

WHAT THIS SAYS ABOUT THE CHANGE ITSELF: the convergence is right and the two defects were in the
seams, which is where a convergence puts its risk. Both seams are now covered by a test that runs in
milliseconds on the host, plus the guest tags that caught them.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0103a-wsi (2026-09-13 22:10):

DAMAGE IS A BOUNDED LIST NOW, ON BOTH SIDES. The item named the cost and it was not theoretical: a
client updating two opposite corners of a screen sent their bounding box, and the driver unioned
everything in its queue on top of that.

THE INTERFACE. `present` takes `liber:graphics@1`'s `damage-region` - a `whole` variant and a list
bounded at the profile's own sixteen - instead of four numbers. A pre-release ABI break of
`liber:display@1`, taken with `--accept-breaking`, because nothing external depends on it.

THE NINE ANSWERS `WSI Profile 1` FREEZES ARE WHAT THE SERVICE IMPLEMENTS, and three of them are
places an implementation would ordinarily go wrong:
  - An EMPTY list is "nothing changed" and must COMPLETE. It is tempting to answer `invalid`, and
    that would make the profile's own answer a failure every client has to work around.
  - A rectangle outside the extent is a typed refusal and NEVER a clamp, and the frame is refused
    whole: every rectangle is checked before any of them is drawn, because half a frame is a frame
    nobody asked for.
  - More than the bound is refused by the DECODER rather than by the service, which is what makes
    "the caller's problem" true rather than hopeful.

AND THE DRIVER HALF IS WHERE THE SAVING ACTUALLY IS. `drivers::gpu::DamageSet` merges two rectangles
only when their bounding box is no larger than the two of them apart - true when they overlap or
touch, false for two corners. A full set merges the cheapest pair rather than dropping a rectangle,
because a dropped rectangle leaves the screen showing something that is no longer there. That rule is
the profile's: a backend MAY merge when merging is cheaper than transferring separately, and what is
forbidden is the unconditional union.

THE HARNESSES WERE UPDATED THE WAY TODAY'S EARLIER LESSON SAYS. Both kernel harnesses that speak this
wire now ENCODE and DECODE the damage through the generated codec instead of laying bytes out at
offsets - which is the same defect class that cost three minutes of timeout this morning when a
renumbered enum moved under a hand-written zero.

EVIDENCE: 3 new host tests in `drivers::gpu`, 45 in the crate, green. In the guest: a two-corner
present on a scaled surface moves two source pixels rather than the four of its bounding box, an
empty present completes and transfers nothing, an out-of-bounds rectangle refuses the whole frame,
and `boot,display,console,imgview` pass 23 tests.

NOT PERFORMED: the driver's byte protocol still carries ONE rectangle per message. It no longer
matters for the union - the service sends one message per rectangle and the driver keeps them apart -
and replacing that protocol with a typed LSIDL interface is its own item in this part.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0103a-wsi (2026-09-13 23:05):

OUTPUT COLOUR METADATA, in both halves the item asks for.

THE REPORT. `surface-info` carries `output-colour`: the space plus SDR white, minimum, maximum and
maximum frame-average luminance. It travels with the surface rather than being a second call, because
a client that had to ask separately is a client that draws one frame before it knows.

THE DECISION WORTH REVIEWING IS THE FOUR `none`s. Nothing in this system asks a panel what it can
show - DDC needs a bus no driver here can reach - so DisplayService reports the colour space, which
follows from the format, and declines to invent the luminances. That is the profile's own rule for
absent HDR metadata ("a refusal to assume rather than a default to invent"), and it is why the fields
are `option<f32>` rather than numbers with a documented default: a zero would be a display that emits
no light, and a plausible default would be a guess nobody could tell from a measurement.

THE CONSUMPTION. `OutputLuminance::tone_map_white` is `max / sdr_white` when both are reported and
believable, and the profile's constant otherwise. `Encoder::new` keeps its meaning (an unknown
destination) and `Encoder::new_for_output` is the one that asks. Six impossible descriptions fall back
rather than compute: a zero white, a peak below diffuse white, NaN, infinity, and either half of the
pair on its own.

AND IT REACHES THE RENDERER rather than stopping at the library boundary: `render2d`'s
`TargetDescription` carries it and `soft2d` hands it to the encoder that writes each tile back. It is
NOT in the prepared key, and that is a decision rather than an omission - the tone curve is applied
at encode time, so a display that changes what it can show changes the pixels and not the flattened
geometry.

NOT PERFORMED: nothing produces a non-`none` luminance yet, so the bright-display path is exercised by
host tests rather than by a guest. The first real numbers arrive with EDID over a DDC transport, which
is a blocked item in `P02M0099` - and when they do, nothing above the service changes shape.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0103 (2026-09-14 00:30):

THE RASTERISER ACCUMULATES AREA NOW, AND IT WAS A CORRECTNESS CHANGE THAT PAID FOR ITSELF.

WHAT WAS WRONG. `graphics-profile::thresholds` freezes render2d antialiasing coverage at 2/255 per
pixel AGAINST THE ANALYTIC AREA, with 1/255 mean. `soft2d` cut each pixel row into sixteen
sub-scanlines, computed and sorted the crossings on each, and added the inside intervals at a
sixteenth of a level - so a vertical edge was analytic and a near-horizontal one was quantised, up to
8/255 out. The file's own comment said the sampling was "below what the conformance tolerance for 2D
coverage allows", which is the claim that was wrong. Nothing had ever compared a drawn frame against
an analytic area, so neither statement had been tested against the other.

HOW IT WAS FOUND. The text milestone's guest conformance run draws a corpus through
`render2d`/`soft2d` and compares against an oracle computed from the outline's geometry rather than
from a captured baseline. At a size where every edge lands on a pixel boundary all 18432 pixels of
the target agreed exactly; at a fractional size the pixels a VERTICAL edge cut agreed to half a code
value and the pixels a HORIZONTAL edge cut were out by 6/255. One measurement, two directions, and
the difference between them named the cause.

WHAT REPLACED IT. Each edge is clipped to the pixel row it crosses and then to each pixel column it
passes through, and two numbers are accumulated per pixel: the signed vertical extent it spans there,
and the area of that pixel lying to the RIGHT of it. A left-to-right sweep turns the pair into the
winding-weighted coverage of every pixel. The fill rule applies to that accumulated value - saturated
for non-zero, folded for even-odd - rather than to a per-sub-scanline winding. Everything left of a
tile is accumulated into a seed rather than by clamping the edge's x, so the answer does not depend
on how the frame was tiled.

THE ALIASED PATH IS UNCHANGED AND STAYS A SAMPLE AT THE PIXEL'S CENTRE. "A pixel is in or it is out"
is a different question from "how much of it is covered", and thresholding an area answers it
differently for a sliver narrower than half a pixel. Two rules, two paths, said out loud.

WHAT HOLDS IT. Two new fixtures in `soft2d`: an axis-aligned rectangle of fractional size drawn at
nine offsets walking a whole pixel in both axes, every pixel compared against the product of its two
one-dimensional overlaps - exactly where the analytic coverage is 0 or 1, within the frozen bounds
elsewhere; and a long shallow triangle whose upper edge crosses one pixel row every six columns, held
to its own area, which is where a quantised near-horizontal edge loses systematically. Quantising the
accumulated value to sixteenths makes the first fail at 4.2/255, so the comparison is live.

AND IT IS THE FIRST OF THE THREE THINGS THE PERFORMANCE ITEM NAMED. Sixteen sweeps each with a sort
became one pass: `vector-stress` 245.8 -> 97.0 ms, `UI-basic` 78.8 -> 46.4 ms, `UI-effects`
385.1 -> 332.0 ms, `image-stress` 419.0 -> 377.7 ms, same host, same frozen fixtures. The floor is
still not met and the ceilings stay where they are; what the change did was take `vector-stress` from
3.8x over to 1.5x, and make the 21 ms empty-list floor the largest single term in `UI-basic`.

IMPLEMENTER'S 3D FOUNDATION ON P02M0103 (2026-09-14 06:30):

SIX ITEMS CLOSED, and all six are the same kind of work: a rule the plan states in prose becoming a
type and an arithmetic that a conformance suite can compare.

`render-math` IS THE CONTRACT AND THE VALUES ARE PICKED. "State the handedness" is not a
specification; a handedness is. Radians, column-major, `M * v`, right-handed world and view space,
clip depth in `[0, 1]`, counter-clockwise front faces, `+Y` up, the camera along `-Z`, NDC `+1` at the
top, a top-left row origin, and `y_window = (1 - (y_ndc * 0.5 + 0.5)) * height`. The constructors are
named for what they encode - `perspective_rh_zo`, `orthographic_rh_zo`, `look_at_rh` - and there is no
`perspective`, because a generically named constructor is how a caller picks the wrong convention and
finds out three layers later.

THE FOUR CHOICES THAT ONLY WORK TOGETHER HAVE ONE FIXTURE BETWEEN THEM. `facing` takes NDC positions
and `window_from_ndc` is documented as coming after it, and a single test asserts BOTH halves: a
counter-clockwise triangle in NDC is the front, and the same three vertices are CLOCKWISE once the
viewport has inverted Y. An implementation that culled after the inversion satisfies every other
sentence in the plan and fails that one.

IT CARRIES ITS OWN `sqrt`, `sin`, `cos` AND `acos` because `no_std` has none and taking a maths
library here would put one in every consumer. Each is held against the host's `f64` version over the
range it is used in, including at every quadrant boundary, which is where a range reduction is wrong
if it is wrong anywhere.

`render3d` IS THE BACKEND-NEUTRAL API AND IT READS THE FROZEN PROFILE RATHER THAN RESTATING IT. The
sample positions, the depth format table and the minimum limits live in `graphics-profile`, are
generated into `RENDER3D_PROFILE_1.md` and are bound to it by hash; a second copy would be a second
list to keep in step, and the first one somebody corrects without the other. Five items:

  MSAA        the positions, the shading rate DERIVED from the shader rather than set, the centroid
                as a centre of mass rather than a covered sample, alpha-to-coverage in INDEX order
                rather than by a dither pattern, the order of the two maskings, and two resolve rules
  depth       `Stored` with TWO shapes, because a normalised format compares integers and a float one
                compares floats; the `Depth32F` answer implemented as written; the bias unit that is
                the format's own; and the stencil test BEFORE the depth test with the operation
                selected by both outcomes
  limits      the profile's minimums as the floor, checked by iterating the FROZEN TABLE so a minimum
                added without a field here is a refusal; a texture checked against the extent AND the
                bytes; and negotiation that never grants more than was asked for
  blend       every factor and operation the plan names, with the three things the enumeration alone
                would not have settled: `SrcAlphaSaturate`'s asymmetry, `Min`/`Max` ignoring the
                factors, and the write mask applying AFTER the blend
  errors      exactly the twelve named, with three pairs kept apart on purpose and a fixture that
                builds one of each and asserts every pair is distinct

Forty-five fixtures across the two crates, each holding a CONVENTION rather than an implementation:
the values are hand-computed or computed in `f64` from the definition, so a rewrite that changes the
arithmetic and keeps the contract passes.

IMPLEMENTER'S 3D API ON P02M0103 (2026-09-14 07:40):

FOUR MORE ITEMS, and `render3d` is now the whole backend-neutral surface: descriptors, views,
hazards, the command model and the submission model. Forty-eight fixtures.

THE RESOURCE MODEL'S ONE IDEA IS THE SUBRESOURCE. A shadow pass renders to ONE CUBE FACE AND ONE MIP,
so attachments are built from a `TextureViewDesc` rather than from whole textures - and every hazard
rule is then stated about `overlaps`, which is what makes "a texture sampled while it is an
attachment" refuse the real conflict and ADMIT the common case of sampling one mip while rendering
into another. A whole-texture test would refuse both.

`Contents` HAS THREE STATES BECAUSE A REPORT NEEDS THEM. `Undefined` is a subresource nothing has
written and `Discarded` is one a pass threw away; both refuse a read and they say different things.
`after` carries the rule that LOADING WHAT WAS DISCARDED IS STILL DISCARDED - a pass that keeps
contents nobody may read has kept nothing, and a store does not make them readable.

THE INTEROP ITEM'S DELIVERABLE IS THAT THERE IS NO BRIDGE TYPE. A compatible single-sample colour
target IS the `OwnedImage`; `bridge_for_attachment` answers `Direct`. What gets built instead, and
did not, is a `Texture::from_image` that copies. What is NOT compatible is named as one operation -
`Resolve`, `ConvertFormat`, `ConvertColourSpace`, `ConvertAlpha` - IN THE ORDER THE OPERATIONS RUN,
so a caller is never told to convert something it has to resolve anyway.

THE COMMAND MODEL VALIDATES AT THE CALL. A list that records anything and validates at submission has
moved every error message away from the code that caused it. Every binding is per pass, with its own
fixture, because carrying a pipeline across a pass boundary would make its compatibility check
silent. The base vertex is SIGNED and the range is checked through it, which catches the read BEFORE
the buffer that a per-index check at draw time cannot.

THE SUBMISSION MODEL WAS DEFINED BEFORE EITHER BACKEND, which is the item's own instruction and the
reason it is worth stating: a software backend finishes inside `submit` and a GPU one does not, and an
API shaped around the first has nowhere to put the second. `Status` has five values rather than an
`Option<Result>`, because "no answer yet" and "no answer ever" are different things with the same
shape in that type. The completion is ownership-consuming, so "waited for exactly once" is a property
of the type. And resources are released IN SUBMISSION ORDER rather than as each finishes, with the
fixture that makes it visible: the second settles first and nothing comes back until the first does.

IMPLEMENTER'S SHADER MODEL AND CLIPPER ON P02M0103 (2026-09-14 08:40):

`render-shader` IS THE LARGEST SINGLE ITEM OF PART `e` AND IT IS DONE. What makes it worth its size
is what it is NOT: Rust closures. A closure is the obvious shortcut and a dead end, because a future
GPU backend cannot take one - so the day that backend arrives every shader in every application would
be rewritten. An IR costs more now and costs nothing then.

THREE THINGS ARE UNREPRESENTABLE RATHER THAN CHECKED, which is stronger than validating them. There
is no call instruction, so recursion cannot be written. Control flow is structured with no label and
no jump, so there is no irreducible graph to analyse. And `Loop` CARRIES ITS TRIP COUNT AS A FIELD,
so an unbounded loop does not type-check - a `while` is a `Loop` with a `Break` and the bound is
still required.

THE POSITION DEPENDENCY SLICE IS THE PART WORTH READING. StrictF32 applies to every DATA AND CONTROL
dependency of a position, and the control half is the one a naive implementation misses: a position
SELECTED by `if (sin(t) > 0)` is as backend-dependent as one computed from `sin(t)`, because two
backends take different branches at the boundary. The fixture for it is the discriminating one - two
positions, neither touching the transcendental, refused because the BRANCH does. And the slice is
complete rather than one step: a matrix built from a transcendental is what a real shader looks like.

WHICH OPERATIONS ARE STRICT IS DERIVED FROM THE FROZEN ACCURACY TABLE. A zero-ULP bound is what "a
strict definition" means, so `sqrt` is allowed and `inversesqrt` at two ULP is not - and an accuracy
corrected in the profile corrects this without a second edit here. The refusal is AT MODULE LOAD and
names the operation, its bound and how far from the position it was found, because "this shader is
not deterministic" is not something anybody can act on.

`ClipCoordQ` IS A NEWTYPE AND NOT A `Vec4`, because "a vec4" is not a definition: what the type
carries is the promise that the value came out of the vertex stage and has NOT been divided, and a
divided position would pass every check and be wrong. The plane order is held against the frozen list
name for name - clipping is not associative in floating point, so the order is the contract - and the
three answers the profile distinguishes are three rather than two: a non-finite component REFUSES the
primitive, everything behind the eye CULLS, and a straddling primitive is cut against
`w = CLIP_W_EPSILON` FIRST.

WHAT THE StrictF32 ITEM STILL OWES, AND IT IS NOT THIS ITEM'S TO BUILD: "quantized raster coverage"
needs a rasteriser, which is `soft3d`, and the cross-architecture cases that place
transcendental-derived vertices on both sides of a clip boundary need the 3D conformance suite. Both
are their own items in the parts below, and the box stays open until they exist.

## 2026-09-14 - `scene3d`, the retained scene layer (implementer note)

Built `src/user/libs/graphics/scene3d` against the FROZEN `Scene3D Core Profile 1` registry rather
than against the milestone prose. Nine modules, 52 host fixtures, all green; registered in
`src/user/services/manifest.toml` `[[sources]]` and in `release-required.toml` as `host.scene3d`.

Things worth recording for whoever touches this next:

1. **The milestone's `BlinnPhong` attenuation and the frozen profile's disagree.** The milestone
   carries `1 / max(kc + kl*d + kq*d*d, EPS)` from an earlier pass; `graphics-profile::scene3d`
   freezes `1 / (1 + d^2/r^2)` with a multiplicative `saturate(1 - (d/range)^4)^2`. The frozen,
   hashed registry is the authority, so that is what is implemented. The `EPS` the milestone asks to
   be named has nothing to guard under the frozen form - no term can be zero, because a zero source
   radius is refused where the light enters. The rest of the milestone's clause IS implemented:
   specular carries its own colour, lighting never changes alpha, and the clamp is applied in linear
   light before the transfer function.

2. **The milestone defers the sorted transparent queue to `f-ext`; the frozen profile puts it in the
   core.** Three queues are implemented. Deferring the third would claim conformance to a document
   this layer does not implement.

3. **Two real defects found in neighbouring code while building this.**
   - `render-math` had no infinite far plane. The scene profile permits one ("expressed as a far of
     infinity") and `perspective_rh_zo` refuses a non-finite `far`, so a conforming camera could not
     be built at all. Added `perspective_infinite_rh_zo` with a fixture that holds it against the
     finite form at `far = 1e9` element by element.
   - `render_math::quaternion::sin_cos` was `pub(crate)`. The spot-cone fall-off needs a cosine and
     this stack has no libm, so it is now `pub`. Nothing else in the tree computed a cosine outside
     `render-math`, which is why it had not come up.

4. **`f32::round`, `powf`, `exp` and `ln` are still absent.** The specular exponent is therefore an
   INTEGER in `1..=1024` evaluated by squaring. This is exact and identical on every target, which
   matters more here than a fractional exponent does - but a future `render-shader` lowering will
   want `Transcendental::Pow`, and the two must then agree at the integer exponents.

5. **What is left in section `f`: the closed `enum Scene3DFeature` registry.** The guaranteed minima
   and every rule are already in `graphics-profile::scene3d` and hashed into
   `docs/graphics/SCENE3D_PROFILE_1.md`. What does not exist is the feature ENUM with per-feature
   `Handles:`/`Covers:` source markers that `profile-doc` scans for the other two profiles. Adding it
   changes the coverage report, not the hashed specification - the two are separate outputs in
   `profile-doc` - so the freeze is not at risk.

### The closed `Scene3DFeature` list (same day)

Section `f` is now complete. Two notes for later:

- **The scene spec's slug moved to `scene3d-spec`.** It had the plain `scene3d` slug, which is where
  a feature list belongs under the convention `render2d`/`render2d-spec` and `render3d`/
  `render3d-spec` already set. The canonical file moved directories; the hash did not change, which
  is the evidence the move was a rename. Only `profile-doc/src/main.rs` referenced the old path.
- **`profile-doc` routed claims with a two-profile special case** (`if slug == "render2d" { (2d, 3d) }
  else { (3d, 2d) }`). That is now "the profile that owns this name, and anything a different profile
  owns", so a fourth profile is a row in the array rather than an edit to the routing.
- **render2d and render3d still report `NOT PERFORMED` for coverage.** Neither has a single
  `@covers:` marker, so their matrices are empty and the gate says so rather than passing. scene3d is
  the first profile with real coverage. Marking up the other two is not this milestone's work, but it
  is worth knowing the gate is a no-op for them until somebody does it.

## 2026-09-14 - `soft3d`, the CPU implementation (implementer note, part 1)

`src/user/libs/graphics/soft3d` now holds the foundation: `fixed`, `raster`, `clip`, `interp`,
`geometry`, `value`, `interpreter`, `texture` and `pass`. 54 host fixtures, all green. Registered in
the shared-image manifest and in the release list as `host.soft3d`.

Four of part `g`'s eleven items are ticked: the rasteriser, the qualifier-aware clipper, the shader
interpreter and the sampler/texture set. What is NOT done yet, so nobody has to re-derive it:

- **The PREPARE/EXECUTE split.** `soft2d` has it and `g`'s first item requires the same shape here.
  The pieces are all written as free functions over explicit state, so the split is a matter of
  introducing the two types and moving the entry points - not a rewrite.
- **Multiple colour attachments.** `pass::write_fragment` takes ONE `Colour`. The blend state,
  write mask and integer refusal are all per attachment already; what is missing is the loop.
- **Depth bias.** `render3d::depth::bias` exists and is not yet applied; it needs the maximum of
  `|dz/dx|` and `|dz/dy|` over the primitive, which the triangle setup has the data for.
- **Instancing at the draw level.** `geometry::assemble` handles topologies, restart and the base
  vertex; the instance count and the base instance are a draw parameter the frame driver owns, and
  there is no frame driver yet.
- **Ingestion validation, Domain accounting and frame-allocation reuse.** `raster::Bins` already
  reuses its storage across frames; nothing else does yet.
- **The frame-scheduling integration gate** is blocked on `a-wsi` by its own terms.

Two things changed OUTSIDE this crate, both because the profile was incomplete:

1. **`render3d_spec` gained `LINE_POINT_RULES`,** ten frozen answers about line and point
   rasterisation: the diamond-exit rule and why it rather than a midpoint walk, line width, the line
   interpolation parameter, point size and its clamp, point coverage as a half-open square, that
   BACK-FACE CULLING APPLIES TO TRIANGLES ONLY, how lines and points clip, and their depth rule. The
   milestone's own words are that a profile mandating six topologies and specifying one is a profile
   with five gaps; this closes them. `docs/graphics/RENDER3D_PROFILE_1.md` regenerated with a new
   hash, which is expected - the hash binds the document to the registry and the registry grew.

2. **`render-shader` gained `Sampling::{Pixel, Centroid, Sample}`.** The frozen profile has FIVE
   interpolation qualifiers and the IR had three, so `centroid smooth` was unwritable - a real gap,
   not a naming one. The profile's own words are that `centroid` "chooses an evaluation LOCATION and
   does not change the interpolation rule", so it is a separate field rather than two more
   `Interpolation` variants; that also makes `centroid flat` unwritable, which is the combination
   that means nothing. `Module::per_sample_shading()` is now derived from the varyings, because the
   shading rate is a consequence of the shader and not a state a caller sets, and `validate` refuses
   a location qualifier on a `flat` varying or on a vertex stage.

### `soft3d` part 2: the frame driver, and what is precisely still missing

`soft3d::frame` is the PREPARE/EXECUTE boundary and the whole pipeline now runs end to end: a
clip-space triangle through the vertex stage, the clipper, setup, binning, per-tile rasterisation,
the fragment stage and the attachments. Three more of part `g`'s items are ticked (the split, the
geometry set, the passes and attachments); 64 fixtures.

Two things were added that the milestone lists under the "REAL software renderer" item and that are
worth knowing are already there: **hierarchical depth** and **render-to-texture**.

The hi-z bound is READ BACK FROM THE DEPTH BUFFER once per tile rather than inferred from the
triangles that covered it. The first attempt inferred it - narrow the tile's bound when a triangle
covers the whole tile - and it never fired, because after clipping a polygon is fan-triangulated and
each piece covers only part of a tile. Reading the buffer is exact, costs one pass over the tile, and
cannot be wrong; the bound is frame-scoped, so `clear` between draws keeps it and only `reset_depth`
drops it, which is what lets a later draw be rejected by an earlier one's depth.

**WHAT IS STILL MISSING IN PART `g`, precisely:**

1. **Per-primitive allocation.** `stage_triangle` clones a `Vec<f32>` per vertex per qualifier, and
   the clipper allocates its polygon buffers per primitive. The item requires "allocate nothing per
   primitive during traversal and clipping". The fix is inline fixed-capacity varying storage bounded
   by a stated maximum - 32 components is the GL ES 2 floor and is defensible - which makes
   `clip::Vertex` `Copy` and removes every allocation below the frame level. It is a contained
   refactor of `clip.rs` and `frame.rs` and nothing else depends on the current shape.
2. **Domain accounting.** "Account peak memory to the process Domain" needs the process API wired
   in; nothing in the graphics stack reaches it yet.
3. **SIMD interpolation and shading, and a texture cache.** The honesty rule exempts the
   DIFFERENTIAL TEST until a parallel path exists - it does not exempt the features. Tiling, binning,
   mip selection and hierarchical depth are all in; these two are not.
4. **The frame-scheduling integration gate** is blocked on `a-wsi` by its own terms.
5. **Host tests for `g`** as its own item: the fixtures exist and cover clipping, winding, the
   qualifiers, depth, stencil, shared-edge fill, sub-pixel coverage, every topology, instancing,
   addressing, filtering, mip selection and MSAA - but the item also names cross-architecture
   comparison, which needs the conformance suite in `h`.

### `soft3d` part 3: allocation-free traversal, and what the fuzz found

Part `g` is 10 of 11. The last item is the frame-scheduling integration gate, blocked on `a-wsi` by
its own terms. 80 fixtures.

**The no-allocation claim is now a measurement.** The test build installs a counting global allocator
with a THREAD-LOCAL counter - so the harness's own parallelism cannot make one fixture's count
another's - and a fixture runs two warm-up frames and asserts the third asks the allocator for
nothing. It counts `realloc` as well as `alloc`.

Getting there took four changes. Three were predictable: inline fixed-capacity varyings (32
components, the GL ES 2 floor), inline shader values up to 16 words with a heap fallback for arrays
only, and a reusable interpreter `Machine`. **The fourth was invisible until it was measured:**
`core::mem::take` on a `Box` builds a fresh default one, so taking the clipper's workspace out of the
scratch allocated a new workspace per primitive. It is an `Option<Box<_>>` now. Nothing about the
code looked wrong; only the counter found it.

**The fuzz found two real defects in this backend on its first run,** both in the sampler and both
from the same cause - a texture coordinate is SHADER OUTPUT and can be anything:

1. `f32 as i64` saturates at `i64::MAX` for an infinite coordinate, and the `+1` that finds the
   neighbouring texel then overflowed. Bounded at `2^24` now, which is where an `f32` stops naming
   adjacent integers, so the clamp cannot lose a texel a shader could have meant.
2. A non-finite LOD - the `log2` of a derivative the shader produced - carried a NaN through the
   trilinear blend and out through every channel. A non-finite level is the sharpest one now.

Neither was reachable from any fixture written by hand, because both need a value nobody writes on
purpose.

**Two things to know before writing more clipping fixtures.** A triangle that CONTAINS the clip
volume comes back as its four-cornered cross-section - two fan triangles, not several - so a fixture
that wants many pieces needs one that CUTS THE CORNERS instead. And an unclipped triangle cannot be
put through `raster::setup` to compare fields, because it reaches outside the raster grid by
construction; the comparison is done in floating point with the same arithmetic the rasteriser uses.

**`render3d::clip::classify_positions`** was added so a backend that keeps its varyings inline can
reach the same classification without building the `Vec`-carrying `ClipVertex` - otherwise it would
allocate exactly what it was avoiding, or reimplement "what is outside" as a second answer.

## 2026-09-14 - `a-wsi`: the display contract, replaced

Six of the eight items are closed. The whole x86_64 build is green and every client in the tree moved
in the same change - there is NO compatibility path, which is what the item asked for.

**What replaced what.** `liber:display@1` was one interface with `acquire`, `present`, `release` and
`input-focus` at CONNECTION level and a synchronous present. It is now three: `display` (a
connection, whose only calls are `create-surface` and `image-limits`), `surface` (a capability with
its own present queue, its own event stream and its own configuration snapshot) and `display-admin`.
`input-focus` moved to the surface, because focus is a property of the thing that has it.

**Two things the tooling refused, both correctly.**

1. `record present-queue` originally carried `list<buffer>`. The IDL generator refused it: a
   collection of capabilities has a count no schema can bound, and an encode that stopped part way
   could not hand back the ones it had already taken. Images are fetched one at a time by
   `image(index)` instead - which is also how the negotiated count stays out of the ABI.
2. `gen.sh` refused the removal of `display.input-focus` until `--accept-breaking` was passed. That
   is the flag for exactly this: nothing is versioned before the first release and no external
   software depends on it.

**The console needed a shadow buffer, and the reason is worth recording.** It drew straight into the
one surface it had. A present QUEUE hands out a different image from one frame to the next, and every
presented image must contain a COMPLETE valid frame - damage is a hint about what changed and never
permission to leave the rest undefined. A console that kept drawing into whichever image it was given
would present a frame missing everything drawn while a different image was current. So it now draws
into a shadow it owns, tracks per image whether that image has ever held a complete frame, and fills
a fresh one whole before presenting it. `imgview` needed none of this: it redraws its entire picture
on every change, so it presents `full` into whichever image it is handed.

**What is left in `a-wsi`:**

- **Domain accounting through the kernel's counters, observed through SystemGraph.** Nothing in the
  display path reports to a Domain yet.
- **The hostile-input and lifecycle tests.** The item lists them precisely - malformed descriptors,
  overflowed image sizes, invalid generations, duplicate acquire and present, completion races,
  driver death, truncated messages, forged imports, a surface channel offered from a second process,
  per-connection exhaustion and cleanup during in-flight presentation - and none exists yet. The
  guest harness starts the real DisplayService, so that is where they belong.

**And `soft3d`'s last item unblocks with these two**: the frame-scheduling integration gate is
written against exactly this contract - acquire, render, PRODUCER_READY, wait for PRESENT_DONE, pace
against the timing contract - and every piece of it now exists.

## Implementer note - 2026-09-14, a-wsi accounting and observation

The display contract's client-facing half is now the present-queue model end to end, and the
accounting item is closed with it.

What changed, in the order it had to happen:

1. `SYS_CHANNEL_SEND_CAPS_ATTENUATED` (syscall 85). The single-handle attenuating send could not
   carry a completion PAIR, so a service wanting to hand over two endpoints with different masks had
   to send two messages for a record the schema says is one. The new call takes
   `[count, CapTransfer * count]` and applies a mask per capability, under the same all-or-nothing
   transaction as the ordinary multi-capability send. `rt::send_caps_blocking_attenuated` wraps it.
   Covered by `kernel.object.channel.a_multi_capability_attenuating_send_narrows_each_one_on_its_own`.

2. `surface.image` -> `surface.provide-image`. The client creates its own memory objects and imports
   them; DisplayService allocates no client pixels. `image-object` is declared
   `@kernel(memory-object)` so the generated `@rights(read, map)` guard checks the object TYPE as
   well as the rights. The service validates the object's own size rather than a client-declared
   length.

3. Every capability the service hands a client is now attenuated by the SEND. The surface endpoint
   has no `transfer` and no `duplicate`, which is what makes "one process identity owns a surface" a
   kernel property rather than a comment. The old code's apologetic note about needing a kernel
   change to strip rights on receipt was wrong: `SYS_CHANNEL_SEND_ATTENUATED` already existed and the
   service was simply not using it.

4. Two defects found on the way, both from the surface rewrite and neither caught by any test:
   - the emergency KILL searched `clients` for a channel equal to `state.active`, which is a SURFACE
     channel now and never a connection channel, so the command revoked nothing at all;
   - `surface.close` tore the surface down INSIDE the handler, closing the very channel its declared
     `result<unit, error>` reply had to travel on. The teardown is deferred by one loop iteration.

5. Bounds, typed exhaustion, a waitable bound task in the wait loop, and a `display-stats` root that
   answers live counts beside their bounds and can do nothing else.

The kernel harnesses were rewritten with it: `display_service_restores_the_console_surface` speaks
the queue contract through the generated codecs, `SurfaceHost` in `tests.rs` is a stand-in display
service for the two viewer harnesses, and the permission-manager scenario's display half goes through
the same one.

## Implementer note - 2026-09-14, the frame loop an application has

`graphics-app` is the shared helper the 2D and 3D sides both use, and it is deliberately two halves:

- `Pacing` is the policy and has no syscalls in it. Everything the integration gate names - the
  in-flight bound, the deadline arithmetic, the rebuild-before-everything rule, the background
  throttle, `again` waiting for the event - is a function of state and a clock reading, so each is a
  host fixture. The crate's DEFAULT feature set is this half alone: `rt` defines `panic_impl`, so a
  test binary that linked it collides with `std`'s, and `runtime` is what every consumer in the image
  turns on.
- `FrameLoop` is the syscall half: acquire, map, present, park, rebuild.

Two defects the gate found, both of which would have been invisible in a unit test:

1. `rt::clock()` answers the scheduler's TICK counter (100 Hz) and `frame-timing` is in NANOSECONDS.
   The first version paced in nanoseconds and passed the result where an absolute tick deadline
   belongs, which is a wait of about four months. `abi` now states `TICKS_PER_SECOND` and
   `NANOS_PER_TICK` beside the calls that take a deadline, DeviceManager's private `100` reads them,
   and the conversion lives at the one place the two units meet.
2. `sched::run_until_idle()` sleeps to the nearest THREAD deadline and keeps going. Against a client
   that paces itself on a timer it never returns: the harness's first call ran the probe to its own
   iteration ceiling before the loop saw a second pass. A harness driving such a client needs
   `run_until_idle_until(ticks + 1)`.

And one correctness point worth keeping: a present settles by SERIAL rather than by decrementing a
counter, because the same present settles by two routes - the completion event and the PRESENT_DONE
release - and a loop that reads both would otherwise free the same image twice.

PRODUCER_READY is now used rather than merely minted: the loop signals it when the render is
complete, and the service drains the endpoint from the present or the abandon that follows. A queue
nothing read would fill after a few dozen frames, after which a client doing exactly what the
contract asks starts failing to signal.

## The 2D conformance suite, and the four things writing it found (2026-09-14)

`user/libs/graphics/conformance2d` walks `Render2D Core Profile 1` entry by entry: 110 scenes, one
per profile feature, plus two that are not features at all - wide-gamut composition and the
quantisation at the end of a frame - because those are properties of the path every feature takes.
`bin/test2d-conformance-sw.lsexe` is the few lines around it, and the guest gate
`kernel.applications.the_2d_profile_conforms_on_the_target` runs it inside a booted system.

WHAT MADE THE SCENES WRITABLE AT ALL is the LINEAR target. A stored byte is the value times 255, so
a pass condition is arithmetic the profile states - "half multiplied by four fifths is two fifths" -
rather than an expectation plus a second implementation of the sRGB encoding. The sRGB path is not
untested by that choice: `ImageColorSpaceConversion` is its own scene and checks both directions.

FOUR THINGS IT FOUND, and none of them would have shown up in a unit test of the code that has them:

1. SEVEN PROFILE ENTRIES HAD NOTHING BEHIND THEM. `ShapeRoundedRect`, `ShapeCircle`, `ShapeEllipse`,
   `ShapeArc`, `ShapeLine`, `ShapePolyline` and `ShapePolygon` were in the closed list and
   `PathBuilder` had `add_rect` alone. They are now `render2d::shape` with their conventions frozen
   in `graphics_profile::geometry::SHAPE_RULES` and generated into the spec: winding, start point,
   the four-cubic approximation and its control ratio, the arc's angle convention and what it does
   about the current point, the one-factor radius fit, and the refusal of a negative radius.
2. SIX FILTER NODES HAD NOTHING BEHIND THEM EITHER: `FilterConvolution`, the two morphologies,
   `FilterDisplacementMap`, `FilterCrop` and `FilterTile`. Each now has a node, a bounds map, a
   ceiling, a canonical encoding and an evaluator, and each one's meaning is frozen in the profile's
   per-node table rather than left to an implementation - "convolution" and "morphology" are
   families of definitions.
3. `Path::tight_bounds` PUT A CUBIC'S EXTREMUM IN THE WRONG PLACE. The derivative's middle
   coefficient was scaled by two where the others were scaled by three, so the peak of a symmetric
   curve came out at three quarters of the way along instead of the half - the bound on a curve
   peaking at 75 read 66.7. A layer sized by it is allocated short and a damage rectangle computed
   from it leaves a strip undrawn.
4. A GLYPH RUN WAS NOT TRANSFORMED. `soft2d` put the form at the pen's USER-space coordinates, so
   text in a scrolled, scaled or rotated drawing stayed where it was recorded while the drawing moved
   around it. The pen is now mapped through the transform, and the subpixel phase is taken from the
   DEVICE position - which is the drift subpixel positioning exists to remove, and which the
   untransformed phase reintroduced for every fractional translation.

AND ONE THING ABOUT THE GATE ITSELF. Its first version printed nothing at all: a loop that only
polls its end of a channel spins at the same priority as the program it is waiting for. With nothing
else to pump, the harness has to call `sched::run_until_idle()` so the suite gets the processor.

TWO SCENE-DESIGN POINTS WORTH KEEPING:
  * The operator scene uses a source alpha of SIX TENTHS. At a half, `as` and `1 - as` are the same
    number and four pairs of operators become indistinguishable - which is how a suite passes a
    backend with `Xor` and `DestinationAtop` swapped.
  * Expectations that depend on a pixel's position are computed from the pixel CENTRE. A bilinear
    ramp probed at x=12 is `(12.5 - 8) / 16` and not "a quarter", and the difference is larger than
    any tolerance worth having.

## The interactive 2D demo, and a boot-chain hang that is not its doing (2026-09-14)

`bin/test2d-sw.lsexe` draws one scene through `render2d` into a real surface's images, paced by
`graphics-app`'s frame loop, and reports what it did. The gate
`kernel.services.the_2d_demo_draws_a_real_scene_with_real_damage` drives it against a real
DisplayService with a stand-in GPU and reads the DAMAGE at the device end.

WHAT THE GATE ACTUALLY PROVES, and it is the damage: the multi-rect phase's two distant regions reach
the driver as TWO rectangles rather than as one union covering the screen between them. That is the
whole cost the damage model exists to avoid, and the only place it is observable is the device end of
the path.

FOUR THINGS THIS COST AN ITERATION EACH, all of them worth keeping:
1. THE ARGUMENTS ARE IN THE LAUNCH CONTEXT, not in its bytes. A program that scans the encoded record
   for its own flags finds none, runs with its defaults - and this demo's default is "run until
   stopped", so the harness waited for a program that was never going to finish, with no output at
   all because the hang was before its first print.
2. A CAPABILITY IT WAITED FOR AND THE HARNESS DID NOT SEND. `recv_tagged` BLOCKS; the demo waited for
   `INPUT_KEYS` in a harness with no input service, which is the same silent hang from the other end.
3. THE SURFACE HAS TO ASK FOR THE SCREEN'S OWN SIZE. A surface with a logical size of its own is not
   reconfigured when the output changes, so a demo that asked for 640x480 never saw the resize -
   and the resize phase is the one this scene is built around.
4. A SCENE LAID OUT IN FIXED PIXELS PUTS ITS OBJECTS PAST THE EDGE of a small surface, and a damage
   rectangle that leaves the surface is a present the service REFUSES. The scene is now laid out from
   the extent each frame arrives with.

AND A HANG THAT IS NOT THIS WORK'S. `kernel.boot.init_package_starts_system_manager` - the full
boot-chain test - hangs after ConsoleService's first frame, on x86_64, with a 15-minute suite budget
and again with a 900-second one. It was ALREADY recorded as failed in the last verify run before this
session (`.build/state/verify-tiers/run.7ZBjqB/outcomes.tsv`, 2026-09-13), and the changes in this
session touch nothing in the boot chain: the graphics libraries, two new staged tools, a library, the
verification model, two host checkers and the kernel test suites.
THE REAL SYSTEM BOOTS. `./image.sh` plus `./run.sh --no-iommu` reaches "shell attached", with sixteen
services reporting online and both DisplayService and ConsoleService presenting a frame - so what is
stuck is the TEST's own collection, not the system it boots. What that test waits for and never gets
is where a fix starts: `SystemGraphService: online` and `Shell: online` are the two reports of its
twenty-three that do not appear on the serial log of a real boot either.

## The boot chain was hanging, and the display service's observation root was why (2026-09-14)

`./test.sh` on x86_64 timed out - the whole suite, not one test - with the serial log ending at
"ConsoleService: a frame reached the display". After the two fixes below the FULL suite passes: 399
tests in 234 seconds.

1. `sched::run_until_idle()` NEVER RETURNS ONCE THE SYSTEM IS UP. It returns when the run queue
   empties and the next deadline is beyond its own, and a booted system has ConsoleService in it
   pacing frames - so the boot test's drain, taken right after `spawn_system_manager`, ran the guest
   for ever and the report-collection loop under it was never entered even once. The bounded drain
   `run_until_idle_until(ticks + n)` is the fix, the same one the frame-loop gate needed, and the
   loop now prints how many reports it has every five hundred passes so a stall is a POSITION rather
   than a silence.
2. AND THEN THE REAL DEFECT CAME OUT FROM UNDER IT: two of the twenty-four services never reported.
   `SystemGraphService` and, after it, the `Shell`. DisplayService's OBSERVATION root answered
   `resources()` and nothing else - no `CONNECT_OP` - while ServiceManager's bootstrap for the graph
   service mints an independent connection from it with `service_connect`, which sends the reserved
   connect opcode and BLOCKS on the reply. There was no reply, so the supervisor stopped inside its
   own bootstrap: no serve root for the graph service, no shell after it, on a system whose display
   was working perfectly. The root is now a factory like every other root here - it mints a channel
   per observer, because two observers sharing one take each other's replies - and the loop waits on
   the root plus every connection minted from it.
   THE INDEX ARITHMETIC IS THE PART TO READ TWICE. The wait list is positional: device stream, device
   channel, providers, kill, admin, THEN the observation channels, then clients, then surfaces, then
   watched processes. Turning one observation slot into `stats_count` of them moves every index after
   it, and getting that wrong wedges the service in a way that looks exactly like the bug being
   fixed - it did, for one run.

WHAT THIS SAYS ABOUT THE PATTERN. Every root in this system is a factory; a root that answers only
its own typed operations is a root nothing can mint from, and the failure is not a refusal - it is a
caller blocked for ever, one process away from the thing that looks broken.

## What a live screen found that no host test did (2026-09-15)

`./check.sh --gate qemu-2d-demo` boots a guest, runs `test2d-sw` and reads three timed frames back as
pixels. Writing it found three things in one afternoon, and the first two were in the DEMO while the
third was in the backend every drawing in this system goes through.

1. DAMAGE IS OWED TO THE QUEUE'S DEPTH AND NOT TO THE LAST FRAME. With two images, the image being
   drawn into now is the one presented TWO frames ago, so a damage rectangle covering what moved
   since the last frame leaves that image's older content on screen wherever the thing moved in
   between. It looked like a trail of stale bands behind everything that moved.
2. A PHASE CHANGE IS A CHANGE TO THE WHOLE PICTURE. Switching from an animating background to a still
   one changes every pixel relative to what each image in the queue holds, so the first frames of the
   new phase - one per image - have to be whole-surface presents. Without that, bands of an older
   background stood where nothing had been damaged since.
3. AND THE ONE THAT WAS NOT THE DEMO'S: A RECTANGULAR CLIP WAS NOT APPLIED AT ALL OUTSIDE ITS OWN
   TILES. `fill_edges` skips the per-pixel clip test when the stack is all rectangles, on the stated
   grounds that a rectangular clip "is already in the bounds" - and the bounds handed to it were the
   TILE's, never intersected with the clip's. In every tile the clip's shape does not reach, the
   level pushed is the empty rectangle that clips everything away, and it was skipped along with the
   rest: the drawing came out in full. On screen it was the far end of a scrolling column standing
   outside the rounded panel that was clipping it perfectly three tiles higher up. The fix is one
   intersection at each of the three call sites - fills, glyph runs and aliased lines - and the host
   test that pins it draws into a 256 by 256 target BECAUSE a single-tile target cannot tell the two
   behaviours apart.

WHAT THAT SAYS ABOUT THE HOST SUITE. Every one of the 112 conformance scenes passes on a 32-pixel
target, and none of them could have caught a per-tile clip: the defect needs a drawing bigger than a
tile, a clip in one part of it and a shape in another. A live screen is 1280 by 800 and every scene
on it is that drawing.

## The output scale, and the four backend changes under it (2026-09-15)

THE SCALE PHASE WAS IMPLEMENTED AND UNREACHABLE, and what was missing was in the SERVICE rather than
in the demo. `DisplayService` answered every surface `scale = 1:1` and had no source for another
value, so the demo's own rule for telling a scale change from a resize - the same logical extent with
a different ratio - could never fire. `display-admin` has `set-scale` now, beside `set-visible` and
for the same reason: a client that could set the scale would be deciding how much memory every OTHER
client's images need, which is an answer only something holding the whole screen may give.

THE TWO EXTENTS STOPPED BEING ONE NUMBER. `Surface` carried a single extent that served as both the
logical and the physical one, and a `console` flag doing duty as "native-sized". It now carries its
logical extent, its physical one - the logical times the output's ratio - and whether it asked for
the output's own size. That last separation was a defect on its own and not a tidiness: `console` is
the FIRST native surface, so a SECOND surface that asked for the output's size did not follow a
resize at all. It kept an extent the output no longer had, and was told about the change by a
configuration whose numbers had not moved.

THE RATIO IS BOUNDED AND APPLIED WHOLE. One eighth to eight, no zero on either side, and every
surface's new physical extent is computed BEFORE any of them is touched - a scale that one surface
cannot express leaves the output at the scale it had, rather than a screen whose windows disagree
about what a logical pixel is. The refusals are asserted against the real service and the accepted
case is asserted twice: as the configuration the service emits, and as the phase the demo reports
having entered.

AND THE RECORDING STOPPED ALLOCATING. `restart` already kept the builder's tables and their
capacity; `finish` then CLONED the commands and every resource vector into a fresh `DrawList`, sixty
times a second, for ever. `finish_into` writes into a list the caller already holds and `clone_from`
keeps the allocation. One rule for every refusal: an error leaves the caller's list EMPTY, which
covers both the canvas's balance checks - which refuse before writing anything, and would otherwise
leave the PREVIOUS frame's list where a caller that ignored the error could present it again - and
the list's own validation, which refuses half way through. The fixture asserts CAPACITY and not
length, because an equal length is exactly what a reallocating implementation also produces.

FOUR BACKEND CHANGES, EACH MEASURED ON ITS OWN, taking `UI-basic` from 46.1 ms to 29.2 ms and
`UI-effects` from 336.1 ms to 232.6 ms. The conformance suite is unchanged by all four: 112 passed,
0 failed, 0 unsupported, 0 untested, on the target.

1. A TILE NOTHING READS THE BACKDROP OF IS NOT DECODED. The performance item had narrowed this
   itself - "a tile that an opaque fill covers entirely should never be DECODED into the working
   space in the first place" - and the conditions are the interesting part: a solid fully opaque
   paint at full opacity, `Normal` over or copied, over an axis-aligned RECTANGLE, with no clip
   pushed and no layer open. Each is a way a pixel could otherwise depend on what was under it, and
   the scratch holds the PREVIOUS tile's pixels, so a tile wrongly believed covered shows them. The
   cover rounds INWARD, which is the opposite of every other bound in this backend: `cover` rounds
   outward because a bound one pixel too small clips a drawing, and this rounds inward because a
   cover one pixel too large claims a partially covered pixel is fully painted.
2. A RECTANGULAR CLIP NEEDS NO MASK, and this was not on anybody's list. `clip.rs`'s own header says
   the axis-aligned rectangle keeps a fast path that needs no storage at all - and nothing ever
   pushed one. Every clip rasterised its edges into a full-tile mask, zeroed first, in every tile of
   the frame; `UI-basic` clips fifty times over eighty tiles. Taken only when the rectangle is
   PIXEL-ALIGNED, because a rectangle level answers one or zero and an edge between two pixel centres
   has an answer in between.
3. THE WORKING INTERMEDIATE IS FOUR SINGLES AND WAS FOUR HALVES. No hardware half conversion is in
   reach of a `no_std` build here, so every read and write of a working pixel went through a branchy
   software routine, twice per channel. It is MORE accurate rather than less - the arithmetic above
   it was `f32` throughout and the half rounded between every pair of composites - and it costs
   hundreds of kilobytes against a sixty-four megabyte ceiling. Worth 30% of `UI-effects`.
4. A ROW IS ONLY AS WIDE AS THE SHAPE REACHED. The area rasteriser zeroed two accumulators across the
   whole width of its bounds, summed them across it, evaluated the winding rule at every column, and
   handed its caller a full-width row to scan for the covered part - while a row of a thin stroke
   crossing a sixty-four-wide tile touches two or three columns. It tracks the columns its edges
   reached, keeps the accumulators clean by zeroing only those in the same pass that reads them, and
   emits the covered run with the index it starts at. The columns outside have ONE answer each,
   which is a fill where it is not zero and nothing at all where it is.

WHERE IT STILL IS NOT ENOUGH, IN ONE NUMBER. `UI-basic` composites about 470,000 pixels in 29 ms:
sixty-two nanoseconds each, a hundred and sixty cycles per pixel for an arithmetic that is a
multiply and an add per channel. The remaining factor is not another term of this kind - it is the
shape of the per-pixel path itself, and the milestone already declines to call that a tail of the
item.

AND THE MEASUREMENTS THE ITEM ASKED FOR ARE RECORDED. The live 640x480 figure turned out to be the
RELEASE build on the target all along: `docs/PERF.md` claimed the debug profile because `./build.sh`
builds the static services and drivers at `dev`, and the staged PIE applications do not come from
there - `build-shared` compiles every consumer object and every provider library with `--release`,
into an image target directory that has no `debug` tree at all. The HiDPI figure is new and is taken
in the guest suite rather than the live boot, because nothing in a booted image SETS a scale: the
ratio is the system's to choose and the only thing holding the admin channel is PermissionManager.
That is a missing operator control rather than a missing capability, and it belongs to whoever writes
the compositor.

## The live gate was intermittent and the scene was drawing over its own text (2026-09-15)

IT FAILED, PASSED ON A RE-RUN, AND FAILED AGAIN, always with the same line: "the colour glyph
contributes 0 pixels of its own palette, so it is drawn in the run's paint or not at all". Two wrong
diagnoses were made before the pixels were looked at - a rendering regression from the backend
changes, then a race in the gate's timing - and both were wrong. The captures settled it in one
reading:

    shot1, y=80, x=162..178:  (24,54,93) (250,183,25) (51,25,0) (250,183,25) (24,54,93)
    shot2, y=80, x=162..178:  (229,51,127) x 5

The first is the colour glyph drawn exactly right: an orange ring around a dark centre. The second is
a flat magenta over the whole of it. THE SCENE WAS DRAWING OVER ITS OWN TEXT. The multi-rect phase's
patch sits at `(0.08h, 0.08h)` from the top-left corner and BLINKS between two sizes six frames
apart; at the larger one it reaches `0.24h`, and the line of text was at `0.11h`. Half the frames of
the demo had no visible text at all.

IT IS A LATENT DEFECT THE BACKEND CHANGES EXPOSED rather than caused. The gate passed for as long as
its captures happened to land on small-patch frames, and a faster renderer changes which tick a
capture at a fixed wall-clock lands on. What the gate was reporting was true - the colour glyph was
not on the screen - and what it blamed was not.

THE FIX IS IN THE SCENE, because a line of text that is invisible half the time is a defect in the
drawing and not in the measurement. It moved to `0.30h`, which is below the patch's band and left of
the star, with room either side; the checker's sampled band moved with it, because where the text is
and where the check looks are one fact and not two.

AND TWO WEAKNESSES IN THE CHECKER WENT WITH IT, both of the same shape - a question asked in a way
that could be answered by something other than what it was about.

1. EVERY CHECK WAS ASKED OF THE FIRST FRAME, over a scene whose objects move across the whole
   drawing. A capture with a mover parked on a sampled region reads as a property that is not drawn.
   Each is now asked of all three captures and needs one to answer. That is not a weakening: three
   captures two seconds apart of a drawing that never drew a colour glyph contain no colour glyph.
2. THE CONSOLE PASSED A CHECK MEANT FOR THE DEMO. The gate slept six seconds and then captured; a
   capture before the demo's first present shows the boot log, which is WHITE TEXT ON BLACK - so
   "the line of text is being drawn" passed on a console screen while every colour check failed, and
   the gate blamed the colour glyph for its own timing. It now waits for a frame that carries the
   scene's coloured background, which no console screen has, and asks the same question of each of
   the three captures.

VERIFIED IN BOTH DIRECTIONS. A synthetic console frame - black with white text on it - fails all six
checks and is named as what it is; the demo's own frames pass. Three consecutive green gate runs
after the change, where before it was roughly one in two.

## A software square root, and three rounds of guessing before anyone measured (2026-09-15)

THE FIRST FOUR OPTIMISATIONS WERE CHOSEN BY REASONING AND THREE OF THEM MOVED ALMOST NOTHING. The
frozen benchmark says WHETHER the floor is met; it does not say why it is not, and I treated a
plausible story about where the time ought to be as though it were a measurement. A widened span
loop, a faster tile read, a hoisted storage dispatch - each was a sound idea, each was worth a
sentence of justification, and each was worth under three percent.

SO THE RUNNER GAINED A PROBE MODE. `SOFT2D_BENCH_PROBE=1 ./bench.sh --suite soft2d` runs scenes small
enough to SUBTRACT from each other, at the same extent as the frozen four: an empty list, one pixel
in every tile, one full-screen fill opaque and translucent, a hundred small ones. Nothing in it is a
budget and nothing is frozen - it is a measuring instrument, and the scenes are chosen so the
difference between two rows is one thing. Three minutes of it found more than the preceding hour.

WHAT IT FOUND: one opaque full-screen fill cost 26.5 ms for 307,200 pixels - eighty-six nanoseconds
each, on an arithmetic that is a multiply and an add per channel. That is not a constant factor, it
is something structurally wrong, and it was:

    pub(crate) fn sqrt_f32(value: f32) -> f32 {
        let mut estimate = f32::from_bits((value.to_bits() >> 1) + (127u32 << 22));
        for _ in 0..4 { estimate = 0.5 * (estimate + value / estimate); }
        estimate
    }

FOUR SERIALLY DEPENDENT DIVISIONS. An f32 divide is a dozen cycles and each iteration waits on the
last, so it cannot pipeline behind itself - and the encode table is indexed by the SQUARE ROOT of its
input, so this ran three times for every pixel of every frame. `libm::sqrtf` lowers to the hardware
instruction on every target this builds for and is correctly rounded rather than approximate.
26.9 ms to 16.2 ms from that one line.

THERE WERE TWO COPIES OF IT. `render2d` had a private one - "without pulling a math crate into this
layer" - whose documentation said "two Newton steps" while the loop ran four, on the FLATTENING path,
so every curve segment of every prepared list paid it. `graphics_core` is already below `render2d`
and already had the answer; there is one square root in the stack now, and `vector-stress`'s
preparation fell from 7.7 ms to 6.7 ms with it.

AND ONE MORE THE PROBE POINTED AT: the row converters read each channel with
`get(index).copied().unwrap_or(0)` - four branches and a panic path per pixel, which the compiler
cannot remove because a chunk whose size is a runtime value could be shorter than index three. The
two four-byte orders every target in this tree presents are spelt out against `chunks_exact(4)`,
whose size the compiler knows; every other format still goes through the general path unchanged. The
tile round trip fell from 21.0 ms to 17.0 ms.

THE RESULT, AND WHAT IS STILL LEFT:

    scene            before    after    ceiling   over by
    UI-basic         46.1 ms   17.7 ms   16.7 ms   1.06x
    UI-effects      336.1 ms  211.4 ms   66.7 ms   3.2x
    vector-stress    99.0 ms   76.6 ms   66.7 ms   1.15x
    image-stress    380.6 ms  362.8 ms   16.7 ms  21.7x

The conformance suite is unchanged by all seven: 112 passed, 0 failed, 0 unsupported, 0 untested.
That had to be true for any of them to be worth having, and the square root's replacement being MORE
accurate rather than less is why it could be made at all.

WHAT IS LEFT IS NAMED RATHER THAN ESTIMATED. Of `UI-basic`'s 17.7 ms, about 17 is the TILE ROUND TRIP
- a decode and an encode of every pixel it touches - and about 6 is the compositing. That is a
per-pixel transfer conversion with a table lookup in it, which is the thing that does not vectorise,
and closing it means changing what the intermediate IS rather than finding another constant factor.

THE LESSON IS THE PROBE AND NOT THE SQUARE ROOT. A benchmark that reports a verdict without a
breakdown invites exactly what happened here: four changes justified by a story. The instrument is
committed beside the scenes so the next person starts where this ended.

## Four more, and where the floor actually stops (2026-09-15, continued)

AFTER THE SQUARE ROOT, FOUR MORE, all found by asking the probe rather than by reasoning:

- A FULLY COVERED OPAQUE RUN IS A COPY. `Cs + Cb * (1 - as)` with `as = 1` is `Cs + Cb * 0`, and for
  any finite backdrop that is exactly `Cs` - so the backdrop read, the four multiplies of the
  coverage scale and the eight of the blend all compute a number already in hand. This is the
  INTERIOR of every filled shape; the edge, where coverage is partial, falls through unchanged. The
  conditions are the ones that make it an identity and no wider - a solid fully opaque paint, a
  normal blend, source-over or a straight source, every weight in the run at one - and the scan for
  that last one costs a pass and saves three. `UI-basic` 17.7 ms to 16.85 ms.
- THE DITHER ROW IS THE SAME FOR EVERY PIXEL OF A ROW. `dither_offset` takes `y % 8` and `x % 8` and
  indexes a matrix, so a row of a tile did sixty-four pairs of modulos to read eight numbers.
- THE BLUR'S SECOND PASS READS ITS COLUMN AS A RUN. It walked columns with `get(x, y)` per pixel -
  local coordinates, a bounds check and an offset recomputed for each, twice, once each way - while
  the first pass had used spans for its rows all along. `Surface` gained `read_column`/`write_column`
  for it.
- AND THE BLUR'S EDGE TEST LEFT THE INNER LOOP. Every tap asked whether it had fallen off the source,
  `2r + 1` times per pixel; a pixel at least `r` from either end cannot have. The interior runs
  without the test, in the same order, which is what makes it the same number and not a close one.

THE FINAL STATE, AND IT IS A FLOOR OF A DIFFERENT KIND:

    scene            before    after    ceiling   over by
    UI-basic         46.1 ms   16.8 ms   16.7 ms   1.01x
    UI-effects      336.1 ms  202.8 ms   66.7 ms   3.0x
    vector-stress    99.0 ms   75.8 ms   66.7 ms   1.14x
    image-stress    380.6 ms  353.3 ms   16.7 ms  21.2x

WHAT EACH REMAINING GAP WOULD COST, because "keep optimising" is not an answer any of them has:

- `UI-basic` and `vector-stress` are dominated by the TILE ROUND TRIP - a decode and an encode of
  every pixel touched, against about 6 ms of actual compositing. It is a per-pixel transfer
  conversion with a TABLE LOOKUP in it, which is precisely the shape that does not vectorise.
  Closing it means changing what the working intermediate IS, not finding another constant factor.
- `UI-effects` is a DIRECT GAUSSIAN: `2r + 1` taps of a four-channel multiply-add per pixel per pass,
  sixty at this scene's sigma. Every constant factor around it is now gone and the arithmetic is what
  remains. Going faster means a box-blur approximation - and the profile specifies a Gaussian, so
  that is a change to the profile rather than to this backend.
- `image-stress` is the fixture question the item itself raises, and the item itself says deciding it
  is the project owner's.

THE ITEM ASKS FOR ALL FOUR AND SO IT CANNOT BE TICKED, however close two of them now are. That is
worth saying plainly rather than leaving a reader to infer it from a table: two scenes are at or
within fifteen percent of ceilings they were two and a half times over this morning, and the other
two are not blocked on effort.

## The one that moved nothing on the benchmark and matters most (2026-09-15)

A TILE WAS DECODED AND RE-ENCODED WHOLE. A drawing that touched three pixels of a sixty-four-row tile
paid sixty-four rows of transfer conversion for them, in each direction. What CAN change in a tile is
bounded by the commands binned to it, and `prepare` already computes those bounds for the binning -
so the round trip is over their union.

    probe                  before     after
    dot-per-tile           17.0 ms    0.081 ms      one pixel in each of eighty tiles
    hundred-small-opaque   21.9 ms    18.5 ms
    the four frozen scenes  unchanged

IT MOVES NONE OF THE FOUR FROZEN SCENES, because every one of them covers the frame. It is here
anyway, and it is probably the most useful of the eleven changes made today: a compositor updating one
damaged corner is the case the tiling exists for, and it was paying the whole frame's conversion to do
it. A benchmark suite whose scenes all redraw everything cannot see that, which is worth remembering
about benchmark suites.

THE WHOLE TILE IS STILL THE ANSWER WHENEVER ANYTHING IN IT IS NOT A PLAIN DRAW. A layer composites
back over bounds that are a `BeginLayer` FIELD rather than a binned bound, and a filter reaches past
what it reads by its declared expansion. Rather than reason about each, a tile whose bin holds a
layer, a layer end or a clip mask keeps its whole round trip - conservative, and free on the drawings
this is for.

THE FIXTURE ASSERTS BOTH HALVES, because the two failure modes are opposite and only one of them is
visible in a picture. A region computed too SMALL clips the drawing, which a pixel check catches. A
region computed too LARGE is merely slow - but a region computed WRONG, or a store that wrote back
pixels it had not loaded, corrupts the target silently. So the target outside the drawing is asserted
BYTE-IDENTICAL to what was there before, over a gradient rather than a flat fill so that a pixel
written back from the wrong place is a different value. A decode and an encode are a lossy pair for
some formats: a pixel that made the trip without needing to may come back changed by a least
significant bit, and that is how a redraw of one corner comes to alter a whole tile.

## What the probes settle about the two scenes still in reach (2026-09-16)

`vector-stress` CANNOT REACH ITS CEILING BY SHADER WORK, and the probe settles it rather than an
argument. The scene's own geometry drawn with a SOLID paint is 58.2 ms against a 66.7 ms ceiling; the
same geometry with its linear gradient is 74.2 ms. So the shader is 16 ms of the 75, and removing ALL
of it would still leave only thirteen percent of headroom for everything else.

WHAT IS LEFT IS THE EXACT-AREA ACCUMULATION OVER NEAR-HORIZONTAL EDGES. A stroke of a flat curve has
outline edges that cross many columns of every row they touch, and the rasteriser clips each edge to
each column it crosses - which is where that algorithm is most expensive and is the point of it: the
coverage is exact. That is the algorithm and not its constants, and the constants around it are now
taken out.

TWO DIVISIONS PER PIXEL REMAIN IN THE GRADIENT and cannot be removed. `projection / length_squared`
and the ramp's `(position - low) / span` both divide by a value that is loop-invariant - per shader
and per stop pair respectively - so a reciprocal computed once would turn each into a multiply. It
would also change the pixels: `x / y` and `x * (1 / y)` are not the same number, which was measured
on this tree's own byte normalisation (126 of 256 values differ in the last place). What CAN be
hoisted was: the gradient's axis and its squared length were recomputed for every pixel from two
fields of a shader that is built once per frame.

THE IMAGE SCENE'S SHAPE, FOR WHOEVER TAKES THE FIXTURE QUESTION. A full-screen bilinear draw is 82 ms
and a bicubic one 263, which fits `a + b * texels` with `b` about 15 ms per texel per frame and `a`
about 22. So a texel fetch - offset, bounds check, four channel loads, three transfer-table lookups
and the premultiply - is roughly 128 cycles, and the scene draws between four and sixteen of them per
pixel across twenty-five commands. That is not a constant factor waiting to be found; it is what
sampling an encoded image costs one texel at a time, and the ways out are a decoded-texel cache, a
byte-indexed decode table that is exact for 8-bit sources, or SIMD. Each is a change to the numeric
path or to the memory budget, which is why the item calls the ceiling a decision rather than a task.

---

## IMPLEMENTER NOTE (2026-09-18): "imgview cannot be closed" was the TERMINAL, not the viewer

REPORTED AS: `cd wallpapers; imgview logo.webp` shows the image, then Escape does nothing, Ctrl+C
does nothing, and after roughly ten Ctrl+C and half a minute it goes away.

MEASURED ON A LIVE x86_64 GUEST, with the whole key path instrumented at the kernel debug port
(`debug_write`, not the console - a diagnostic that goes through ConsoleService cannot be trusted to
report a fault in ConsoleService, and the first two instrumented runs deadlocked on exactly that).

WHAT THE INSTRUMENTATION FOUND, in order:

1. The raw-key path is intact end to end. `virtio_input` sends the HID usage to the key sink,
   InputService's `record_key` runs, the focus proof validates, `subscribe-keys` returns a live
   stream, and `handle_code` sees the Escape. Every hypothesis about a missing `KEYS` channel, a
   stale `key_sink`, an unmapped keycode or a lost focus proof was wrong, and each was disproved by
   a print rather than by reading.
2. The viewer LEAVES ITS LOOP on Escape. What it could not do was finish: the teardown ran to
   `set_stdin(0)` and then the whole console output path stalled, which is what made the first two
   runs look like a hang inside `Surface::drop`.
3. The stall was the instrumentation's own `eprint`. With the diagnostics moved off the console, the
   surface close, the deferred `remove_surface`, the `CONSOLE` focus hand-back and the restore
   present all completed, and the prompt came back.

THE DEFECT IS WHAT HAPPENS ON THE PATH THE USER ACTUALLY TOOK. They type in the terminal, so their
Escape and their Ctrl+C are BYTES on the serial line, not keyboard events:

- Ctrl+C reaches `feed_tty`, which turns it into SIG_INT for the foreground job - ABOVE the raw/cooked
  branch, so it works in raw mode too. `imgview` did not arm `catch_interrupt`, so SIG_INT terminated
  it where it stood. Nothing in a terminated process runs: the raw, unechoed mode it had asked the
  terminal for was never given back.
- `CLEAR_FG` - the message the shell sends however the job ended - did not restore the line
  discipline. So the shell came back to a RAW terminal and delivered every keystroke on its own:
  `ls` answered "unknown command: l" and then "unknown command: s". That is the "nothing responds any
  more", and it is also why more Ctrl+C looked like it eventually did something.

FIXED IN THREE PLACES:

- `console_service.rs`, `tty_fg_winsize`: releasing the tty puts `ld.cooked` and `ld.echo` back. The
  release is the one event that happens whatever killed the job, which is why the restore belongs
  there and not in each tool.
- `imgview.rs`: `catch_interrupt()` plus an `interrupted()` poll on both sides of the wait, so Ctrl+C
  leaves through the same teardown as `q`.
- `imgview.rs`, `handle_serial_byte`: 0x03 and 0x04 exit. A terminal that is not holding the viewer
  as a foreground job passes them through as bytes, and they fell to the catch-all.

The two silent `return`s after `input_focus()` and `subscribe_keys()` now say which one refused; a
viewer that could not take the keyboard was indistinguishable from one ignoring every key, and the
two are fixed in different places.

TESTS, AND EACH WAS DRIVEN BY A MUTATION BEFORE IT WAS BELIEVED:

- `kernel.services.a_tty_a_job_left_raw_comes_back_cooked` - a headless ConsoleService with a minted
  ConsoleSink privilege so the test can TYPE (`console_input::feed_serial`): cooked delivers the
  line whole, raw delivers single bytes, and after `CLEAR_FG` the next line arrives whole again.
  Removing the two restore lines fails it on the last assertion.
- `imgview_interactions` gained `RawInterrupt` (0x03 as a byte) and `CaughtInterrupt` (the pending
  flag plus a wake, exactly as the kernel drives a tty's Ctrl+C). Both assert the viewer CLOSES ITS
  SURFACE, which is what says it left through the teardown. Removing `catch_interrupt()` fails the
  second; removing the `0x03 | 0x04` arm fails the first.

Guest evidence: 97 tests over `imgview,console,shell,service,network`, and by hand on a live guest
the user's own sequence - `cd wallpapers`, `imgview logo.webp` - exits on keyboard Escape, on serial
Escape and on Ctrl+C, with `echo` working normally afterwards in all three.

ALSO FIXED, same report: `ping`. A timeout printed nothing on the CLI path ("timeouts are silent
losses"), so a link where every probe times out produced one header and then apparent silence until
the summary; it now prints `Request timeout for icmp_seq=N` as it happens. And a timeout rendered
through the wire record as `"ttl": 0, "rtt-us": 0` - a measurement of zero rather than the absence of
one - now renders `null`, which is what the statistics block already said.

---

## 2026-09-19 - the decoded source, and a mip chain whose top level was not decoded

TWO CHANGES, ONE PERFORMANCE AND ONE CORRECTNESS, both in the graphics stack.

**1. soft2d samples a decoded source for `Bilinear` and `Bicubic`.** The earlier probes had located
the largest single term in the whole 2D suite: a transfer decode PER TAP, 139 of bicubic's 254 ms.
`Pyramid` already held its levels in canonical premultiplied linear float and a `Mipmapped` draw
already sampled that, so the move was to extend the copy to the two magnifying qualities. What made
it a decision rather than a patch is `max_prepared_scratch_bytes`: a decoded level zero is sixteen
bytes a texel against a sixty-four megabyte profile limit.

- `wants_decoded(list, image)` beside `wants_pyramid`: the images this list samples `Bilinear` or
  `Bicubic`. The pyramid build runs in two passes, required first.
- An optional copy is GIVEN BACK, newest first, until the prepared total is under the ceiling. A
  list that fitted before this existed still fits. An optional copy that will not allocate is a
  `continue`, not an error; a required one that will not allocate is still `Err`.
- `paint.rs` builds its `Sampler` over `pyramid.level(0)` when the quality is not `Mipmapped`.
- `pixel.rs` gained an identity-decode shortcut, because the copy is read back through the same
  decoder and the canonical format needs none of it.
- An optional copy is LEVEL ZERO ALONE: `Pyramid::base_from_sampler` / `base_with`, with
  `from_sampler` now the chain built on top of the base. A magnifying draw reads level zero and
  nothing else, so the halvings were 12.6 ms of UI-effects' preparation and a third of its bytes for
  a level no draw touches.

Measured: bicubic 256.4 -> 135.5 ms, bilinear 73.9 -> 48.8, YUV 164.7 -> 55.0, wide gamut 86.4 ->
49.5, and the mipmapped control 74.0 -> 74.0 unmoved. image-stress 329.75 -> 189.6 ms, UI-effects
176.99 -> 150.9. Numbers and the trade are in `docs/PERF.md`.

**2. `soft3d::texture::generate_mips` left its top level encoded.** The function decodes the top
level, builds every level below it from that decoded light, then sets `transfer = Linear` and
`premultiplied = true`. The last statement was meant to store the decoded top level back and instead
assigned `levels[0]` to itself - the decoded copy was the loop's initial `source` and was overwritten
on the first iteration.

The consequence is not a crash and not a missing feature: a MAGNIFIED fragment reads level zero and
gets an sRGB number treated as light. `0.5` reads as `0.5` where the chain's own answer is `0.214`.
Every minified fragment is correct, so the texture has a step between level zero and level one at
exactly the distance where the two meet. A straight-alpha source was not premultiplied either.

WHY THE TEST DID NOT CATCH IT: `mip_generation_is_a_box_filter_in_linear_light_and_odd_sizes_halve_by_flooring`
asserted the declared transfer and that level ONE averaged decoded values - both of which the defect
satisfies. It now pins level zero too, plus a straight-alpha case over both levels. Driven by
mutation: restoring the self-assignment fails on `0.5` against `0.214`.

Host suites after both: graphics-core 28, soft2d 28, soft3d 81, render2d 33, render3d 61,
conformance2d and conformance3d green.

## The two suites on the two emulated ports (2026-09-19)

The decoded-source fix above is inside the mip chain, and a mip chain is exactly the kind of code
that can be right on the target it was written on and wrong on another. So both conformance suites
were run on all three ports after it, with `--tags image,slow`: aarch64 34 tests in 2097 s, riscv64
34 in 2489 s, against an x86_64 run of the same suites the same day.

THE THREE PORTS DO NOT MERELY PASS - THEY REPORT THE SAME COUNTS:

    test2d-conformance   112 passed, 0 failed, 0 unsupported, 0 untested   conforms
    test3d-conformance   render3d 89, scene3d 71, 160 total, all zero      conforms

`0 untested` per registry rather than as one total is the coverage half of part `h`, and per-registry
is what stops a suite that stopped exercising one of them hiding inside the other's count. The 2D and
3D demos and `imgview` ran beside them on both emulated ports and passed.

AND ONE TIMING LESSON, because it cost an hour of suspicion. `imgconv_cross_volume` reads 94 s in a
`drivers,pci,slow,storage,usb,network,service` run on aarch64 and 1351 s in this `image,slow` one,
with riscv64 agreeing at 1098 s. The code is identical; the first selection RUNS THE DRIVER TESTS,
so `vol://system` is served by a bound block driver by the time the application tests reach it. A
timing comparison across two runs is only a measurement when both selected the same work.

## vector-stress crossed its ceiling, and the benchmark was not where anybody would look (2026-09-19)

THE SCENE. `vector-stress` had sat ON its line for a day - 66.79, 66.84, 66.54, 66.97 against 66.7,
one of four under - and the record said so rather than rounding it down. What was still inside the
linear gradient's per-pixel loop was `length_squared <= 0.0`, asked of every pixel about a value
that does not vary along a span or at all. It is decided once per row now and the pixels are
identical: the same `ramp.at(1.0)` the arm already produced.

MEASURED BOTH WAYS, THREE RUNS EACH, because a third of a millisecond on a scene sitting on its
budget is exactly the size of claim that needs it:

    the test inside the loop   66.835  66.473  66.606   one of three OVER
    the test hoisted out       66.157  66.320  66.250   three of three MET

The margin is under one percent and the p99 still crosses. Three of three under is a different fact
from one of three and it is the one this scene now has; it is not the same as comfortable. THE FLOOR
STILL ASKS FOR FOUR AND HAS TWO - `UI-effects` at 156.4 against 66.7 needs a box-blur approximation
the profile forbids, and `image-stress` at 194.5 against 16.7 waits on the fixture question the item
puts to the project owner.

WHAT WAS NOT DONE AND WHY. The obvious faster gradient is incremental - the projection along a span
is affine, so it could be one add per pixel - and that is a DIFFERENT computation whose float error
accumulates along the row. This profile requires every path to agree with the scalar reference, so
what is hoisted is the dispatch and an invariant test, never an operation.

## And the benchmark this floor is defined by was undiscoverable (2026-09-19)

`bench.sh` carried `soft2d` in its suite map and not in its help or `--list` - along with `lico`. So
the one suite part `c`'s floor is measured by was the one the documented entry point did not
mention. The help is built from the map now, a suite with no description is a hard error rather than
a silent omission, and the guard is proven to refuse before it is trusted to print: it adds a suite
with no description, requires the listing to fail, and takes it away again.

## perf-anchor was red on the ORDER of the builds, not on the code (2026-09-19)

`check.sh --gate perf-anchor` failed in one second with `guest exited without its case's final
verdict`. Running that boot by hand names it: `./build.sh --arch x86_64` rebuilds the system volume
signed for DMA mode `harness`, and the `development-trace` medium the gate boots is signed
`enforcing-required`, so `mkimage` refuses the pair and the guest never starts. The fix is the
command the tool prints. Green after it, on both halves - the anchor published to the harness
profile and withheld from the interactive one.

## The 3D frame, and a route removed rather than guessed at (2026-09-19)

`fetch` wrapped an address on all three axes per tap. Every texture in the benchmark scene, the
conformance suite and the demo has depth one, and there the caller only ever asks for `z = 0` -
`sample_level` builds the second plane exclusively when `depth > 1` - and every wrap mode maps index
zero inside an extent of one to zero. So a third of the per-tap addressing computed a constant.

    addressing all three axes   966.3  991.3  944.3   median 966.3 ms
    skipping a flat z           937.3  933.9  924.8   median 933.9 ms

Three and three, because a three percent claim on a measurement with a forty-millisecond spread
needs both sides; the ranges barely overlap. The conformance suite is unchanged at `112 passed,
0 failed` and `160 passed, 0 failed`, which is what "the same answer, computed less often" has to
mean. The floor is 33 ms, so this is a factor of twenty-eight rather than twenty-nine.

AND THE TEXEL CACHE IS A ROUTE THAT WAS TRIED AND REMOVED. `soft3d` builds a direct-mapped cache
whose own note says what it buys - an address computation per axis and a transfer decode per channel
on an sRGB texture without a mip chain, which is a `pow`. NOTHING USES IT: the conformance harness,
`test3d-sw` and the benchmark all call `texture::sample`, and `sample_cached`'s only consumers are
its own tests. Wiring it into the benchmark moved the texturing stage from 289.6 to 288.1 ms -
inside the spread - because the fixture's checkerboards are not that case. The wiring came back out,
and what is recorded instead is the fact worth keeping: a cache exists for a cost every renderer
here pays and none of them asks for it.

## image-stress was two scenes, and an over-claim of mine was corrected (2026-09-19)

THE OWNER ANSWERED THE QUESTION THE ITEM PUT TO THEM: two scenes, not a different ceiling. The split
follows the lines the scene's own comments already drew - `image-resample` is eight downscales and
three quality upscales over one source with source and target in the same space; `image-convert` is
twelve wide-gamut tiles, a full-frame wide-gamut draw and a full-frame YUV draw from planes.

    image-resample    80.6 ms   16.7 ms   4.8x
    image-convert    127.1 ms   16.7 ms   7.6x

The single number was 11.4x and said which half was slow only by accident of how they were summed.
Frozen counts split with them: 11/1 and 14/2, summing to the 25/3 the one scene had.

AND A CORRECTION TO MY OWN RECORD FROM EARLIER THE SAME DAY. I wrote that the linear-gradient hoist
made `vector-stress` "met" on three runs under 66.7. Six later runs of the same binary read 67.13,
67.09, 67.46, 67.69, 67.24 and 67.41 - all over. The difference is not the gradient, which is exact
and conformance-proved: the suite gained a fifth scene in between, and this scene moves by more than
the half a percent the change buys whenever anything else in the process moves. The hoist is real;
the verdict was not. `vector-stress` is still AT its line, which is what this file said before I
claimed otherwise.

## `f-ext`'s first deliverable: the closed feature list, and the LOD rules that had none

THE PART WAS ACTIVATED AND ITS FIRST DEBT WAS NOT AN IMPLEMENTATION. `f-ext`'s header says it carries
a CLOSED ENUMERATED feature list on the same terms as the 2D and 3D profiles, and it carried
equations and no list. Every other item of the part says "a test per feature", and until now that
phrase had nothing to range over: a rule is prose a reader checks, a feature is a name a generator
can range over, and the conformance suite, the capability report and the backend checklist are all
built over the second kind.

WHAT WAS ADDED, in `src/user/libs/graphics/profile/src/scene3d_extended.rs`:

  - `ExtendedFeature`, 52 variants in seven groups - material 11, environment 6, shadows 10,
    postprocess 6, animation 11, detail 6, limits 2 - and `SCENE3D_EXTENDED_PROFILE_1` over them
    through the same `profile!` macro the other three lists use, so the name of a feature is the
    variant's own spelling rather than a second string beside it.
  - `SCENE3D_EXTENDED_GROUPS`, `in_extended_profile_1` and `entry_by_name`, matching `scene3d.rs`.

EVERY VARIANT COMES FROM A RULE THAT WAS ALREADY FROZEN, with one exception, and the exception is the
defect this part was flagged for.

LEVEL OF DETAIL HAD NO STATED RULE ANYWHERE. The item asks for "LOD selection with STATED
THRESHOLDS" and `SCENE3D_EXTENDED_1.md` stated none - no coverage, no distance, no hysteresis - so
that half of the item could only ever have been implemented against an invented rule. A feature with
no stated rule cannot go in a closed list, so `LEVEL_OF_DETAIL_RULES` was written: nine answers
covering the selection metric (screen coverage, not raw distance, because distance selects a
different level at the same apparent size when the field of view or the viewport changes), the
threshold ladder and its refusal when not descending, the default ladder, hysteresis at a tenth of
the threshold against the level held last frame, the first frame with no previous level, what
happens below the last threshold, a mesh with one level, WHICH bounds the coverage is taken from
(the current ones, after morphing and skinning - a rest-pose coverage steps a raised arm down a
level while it is still on screen), and that the chosen level is READABLE, which is the difference
from culling: culling must not change the picture and a level of detail is chosen to.

They are CHOSEN RATHER THAN REQUIRED, on the same terms the root-motion rule already uses, and
`s-3d-ext`'s own criterion asks exactly this: "the choices its own rows leave open have one answer
each".

AND THE HASH MOVED, WHICH IS THE POINT RATHER THAN A SIDE EFFECT. `198e9d18...` became `ef2117e5...`.
The part's header says its first item is an amendment to a frozen normative specification and that
the ordering rules exist to stop an implementer doing that QUIETLY; this is the loud version. The
closed list is hashed with the equations - a list that could change without moving the hash would be
a second statement of the profile that nothing holds to the first.

A SEVENTH LIMIT CAME WITH IT. `max_lod_levels`, minimum 4, in the registry and enforced in
`scene3d::Limits` beside the six. The field comment there used to say the six were the six the freeze
named and that adding a seventh to a frozen document was the ordering defect `s-3d-ext` prevents;
that comment is now true in a different way and says so. `max_transparent_items` is STILL left out,
and for the reason that comment already gave: a transparent item is a drawable and `max_drawables`
bounds it. Moving a frozen hash is for a rule the profile is missing, not for a second name over a
bound that already exists.

HELD BY FIVE NEW FIXTURES in the profile crate (73 pass, was 68):

  - the list is closed: no name twice, no feature twice, no unknown group, no empty group.
  - EVERY entry is `Scene3D`-owned, which is a claim rather than a default. The closed-list helper
    the two core profiles share asserts that SOMETHING is backend-owned; that is right there and
    wrong here, because Extended adds no rasteriser capability at all. Shadows are a depth target
    and comparison sampling, the environment is a cube map with mips, bloom is a render-target
    pyramid and skinning is arithmetic in a vertex stage - every one already in `Render3D Core
    Profile 1`. If that stops being true the fix is to add the capability to the 3D profile, where a
    backend checklist will demand a handler for it.
  - no Extended name collides with a Render2D, Render3D or Scene3D core name, which is the
    disjointness `profile-doc` relies on to route a `@covers` claim to exactly one profile, and
    every entry looks up to itself by name.
  - every rule section has features and every feature group has rules, which is the join that stops
    the list becoming a second, drifting statement of the same profile.
  - the level-of-detail rules answer the four questions two implementations would otherwise differ
    on, named one by one so a later edit cannot drop one and leave the feature stated in the list
    and unstated in the profile.

`profile-doc --check` reports the generated documents match every profile; the scene3d crate's own
57 fixtures pass with the seventh limit joined by name.

WHAT THIS DOES NOT DO is implement any of the 52. It makes them nameable, which is what every
remaining item of the part needs before it can claim to cover one.

## `f-ext`: level of detail, skinning, morph targets and animation

THREE MODULES IN `scene3d`, each with the rules it implements written above it and a fixture per
rule. They are the deformation and detail halves of the part; PBR, environment lighting, shadows and
post-processing are not in this pass.

`detail.rs` - LEVEL OF DETAIL. `Ladder` (finest level plus strictly descending coarser ones),
`Detail::{Level, Vanished}`, `coverage_perspective`, `coverage_orthographic`.

  - The finest level CARRIES NO THRESHOLD and the type says so rather than a comment: `Ladder` holds
    `finest: u32` beside `coarser: Vec<Level>`. A ladder whose first entry had a threshold would
    carry a number nothing reads, and a number nothing reads is one an author sets and then wonders
    why it did nothing.
  - A ladder that does not strictly descend is REFUSED at load and not sorted. Sorting draws a scene
    the author did not write and hides the error for ever. Equal thresholds are refused too: the
    second level could never be selected, which is a level somebody wrote that nothing will draw.
  - Hysteresis is applied to the level ACTUALLY HELD rather than to the one the ladder would pick,
    so the widened band belongs to the current level. A big jump still lands where the ladder says:
    hysteresis decides WHETHER the level is left, not where it goes, so an object that moves far in
    one frame does not step down one level per frame.
  - The eye at the sphere's centre answers `INFINITY` rather than dividing by zero. Worth naming: a
    NaN there compares false against every threshold and selects the finest level ANYWAY - by
    accident rather than by decision, which is the kind of correct-looking behaviour that survives
    until the day the comparison order changes.
  - The vanishing coverage gets the same tenth, because it is a threshold like any other and a
    drawable sitting on it would otherwise blink. `select` therefore takes the whole previous
    `Detail` rather than a level index, so "was it drawn last frame" is answerable.

`deform.rs` - SKINNING AND MORPH TARGETS. `Influences` (normalised on construction), `Pose`
(`joint_world * inverse_bind`, composed once per frame), `MorphTarget`, `morphed`, `check_targets`,
`deformed_bounds`.

  - There is no such thing as an unnormalised `Influences`: the type normalises in `new` and refuses
    a set that sums to zero. A vertex with no joint stays at the origin while the mesh moves, which
    reads as a tear in the geometry and gets traced to the renderer before the exporter.
  - A joint index past the skeleton contributes nothing and its weight goes with it, rather than the
    whole frame being refused. One bad index in one vertex of one asset should not be a frame that
    does not draw, and the remaining weights are renormalised so what is left is the pose the other
    joints give.
  - `check_targets` runs once for the whole set rather than per vertex, because discovering a short
    target at vertex 4,000 leaves the first 3,999 already displaced.
  - `deformed_bounds` is where MORPH THEN SKIN lives, in one place, so the order is not open-coded
    wherever bounds are wanted. It is the join to the level-of-detail rule: the fixture holds a
    character whose raised arm grows the bound, and the grown bound selecting the finer level.

`animate.rs` - CLIPS. `Clip`, `Track`, `Channel`, `Key`, `Sampled`, `SampledPose`.

  - Named `SampledPose` and deliberately not `Pose`, which in this crate is the skeleton's composed
    matrices. The two are a stage apart and one name for both would be the kind of collision a
    reader resolves by guessing.
  - Linear for translation and scale, `Quat::slerp` for rotation - which already chooses the sign,
    so the shorter arc is the crate's and not a second implementation of it. The fixture holds the
    measurable consequence: a 10 degree turn written with its end quaternion negated interpolates
    to 5 degrees at the midpoint and not to 175.
  - Root motion is EXTRACTED unless the clip declares it keeps it, and a clip that keeps it gets a
    zero delta rather than the motion twice.
  - A looping clip's seam is checked at load. Rotations are compared by the ABSOLUTE dot, so `q` and
    `-q` close the loop: they are one rotation, and a comparison that missed it would refuse a clip
    for a full turn the author never wrote.
  - Keyframe times must STRICTLY ascend: two keys at one time make the value at that instant depend
    on which one the sampler found first.

AND A SECOND AMENDMENT, FOR THE SAME REASON AS THE FIRST. The looping rule says a seam must close
"within the conformance threshold" and the threshold table had no row for a pose - the same shape of
gap as the missing LOD thresholds. Two rows were added to `thresholds.rs`: "a sampled pose" and "a
clip's loop seam", both `<= 1e-5` per component with `1 - |dot| <= 1e-5` for rotation. The "why"
states what the tolerance has to distinguish: a wider one would admit a renderer that interpolated
quaternions linearly and normalised, which is the cheaper alternative this profile refuses BECAUSE
it produces a visibly uneven rotation speed. The hash moved again, `ef2117e5` to `570ab514`.

ONE `no_std` DEFECT FOUND BY THE BUILD AND NOT BY THE TESTS. `Clip::at` wrapped a looping time with
`%`, which on `f32` lowers to `fmodf` - a libm call this tree provides no library for, so
`build-shared` reported "library scene3d import fmodf has no direct provider". The host tests had
passed: `cargo test` builds against `std`, and the missing symbol only exists on the target. It is
now a truncation through `i64`, which is the same operation `fmodf` performs, a couple of
instructions, and SATURATES rather than wrapping on a time larger than an `i64` holds.

24 new fixtures, 57 to 81 in the crate, all passing.

## `f-ext`: the physically based material

`pbr.rs` IN `scene3d`, WITH THE PROFILE'S OWN EQUATIONS AND NOTHING ELSE. `PbrMaterial` carries the
factors, `PbrSurface` the sampled texels, and `shade` computes the direct term the profile writes:
GGX, Smith height-correlated visibility, Schlick Fresnel, Lambert diffuse multiplied by `(1 - F)`
and `(1 - metallic)`, summed in the order and with the `dot(N,L)` placement the profile states -
which is exactly what published implementations differ about.

IT IS A SEPARATE TYPE AND NOT A FIFTH `MaterialKind`. The core profile's material list is four
entries and adding a fifth to that enum would change what `Scene3D Core Profile 1` means. Extended
is additive, so the PBR material lives beside the core one and a scene that never claimed the part
never constructs it.

A THIRD AMENDMENT, FOR THE THIRD TIME FOR THE SAME REASON. The item enumerates what this profile has
to pick - "the metallic workflow's base-colour interpretation, how occlusion is applied, the units of
emissive, tangent-space handedness and the normal-map convention, ... and the colour space of EVERY
map" - and the frozen registry picked the equations and none of that. Without them a conformance
scene has no single expected answer and, in the item's own words, "the material is only a name". So
`PBR_MATERIAL_RULES` was written, nine answers, and the hash moved a third time (`570ab514` to
`5827421c`). Each answer is a thing two implementations would otherwise decide separately:

  - the base colour has TWO meanings mixed by `metallic` - diffuse albedo at 0, the metal's F0 at 1 -
    and a metal has no diffuse term at all.
  - occlusion reaches the AMBIENT and environment terms only. Applying it to direct light would
    darken a surface a placed light demonstrably reaches, which is a shadow the scene cannot remove.
  - emissive is linear radiance added last, unattenuated, untouched by occlusion or shadow. A source
    is not a receiver.
  - the normal map is tangent space, +Y UP, bitangent `cross(normal, tangent) * tangent.w`. The other
    convention inverts every crevice into a bump - the surface still looks lit, and lit from the
    wrong side.
  - a MISSING TANGENT means the normal map is not applied. Deriving one from screen-space
    derivatives makes the frame depend on the rasteriser's derivative rule, so a mesh whose author
    did not export tangents would look different on each backend.
  - base colour and emissive are `Color` and decoded; metallic-roughness, normal and occlusion are
    `Data` and never are. The one rule that costs nothing to get wrong and changes every pixel.
  - glTF's packing: roughness in green, metallic in blue, red and alpha ignored.
  - `Opaque` / `Mask` / `Blend`, with `Mask` discarding STRICTLY below its own threshold.
  - a double-sided surface flips its normal BEFORE anything reads it, so the normal map and the
    lighting both see the flipped one.

THE RESULT IS NOT CLAMPED TO 1, and that is a decision the core material does not take: `material.rs`
saturates in linear light because its output is a display value. The post-processing rules put tone
mapping LAST, after bloom and fog, precisely because both are defined on linear radiance - a
material that clamped its own output would throw away everything bloom exists to spread before bloom
ever saw it. A NaN is still refused, because one NaN pixel spreads through a bloom pyramid into the
whole frame.

`render_math::sqrt` IS NOW PUBLIC. It was `pub(crate)` while every caller was a vector length; the
Smith term takes the square root of two scalars. The alternative was a second Newton iteration in
the scene layer, which is the one thing a shared maths crate exists to prevent - and a second
implementation would also be a second set of fixtures, drifting at the last bit.

TEN FIXTURES, EVERY EXPECTED VALUE COMPUTED BY HAND FROM THE EQUATIONS. The head-on dielectric is
worked out in full in the test - `D = 1 / (pi * a^2) = 5.0929582`, `V = 0.25`, `F = F0 = 0.04`,
diffuse `0.96 / pi`, total `0.3565071` - and the three terms are also asserted separately, so a
failure says WHICH of the four moved rather than only that the sum did.

AND ONE FIXTURE OF MINE WAS WRONG BEFORE IT WAS RIGHT. I asserted that a dielectric shades BRIGHTER
than a metal of the same base colour, reasoning that it carries a diffuse term the metal does not.
It shades darker: the metal's F0 IS its base colour, 0.5 in green against a dielectric's 0.04, so
the metal's specular alone is three times the dielectric's whole answer. A test that compared
magnitudes would have passed for a renderer that had the two the wrong way round. Both are now
computed from the profile and asserted by value, with the decomposition written beside them.

34 new fixtures in the crate, 57 to 91.

## `f-ext`: environment lighting, shadows and post-processing

THREE MORE MODULES, AND THE PART'S SEVEN FEATURE GROUPS NOW ALL HAVE ONE. 55 fixtures in the crate
for the Extended work, 57 to 112 in all.

`environment.rs` - THE SPLIT SUM, BOTH HALVES, FROM THE SAME TERMS THE DIRECT LIGHTING USES. The
profile says the BRDF table is "generated by the same GGX and Smith terms above" and that is meant
literally: `brdf_integration` calls `pbr::visibility_smith`, not the second Smith with its own `k`
that the usual image-based-lighting derivation substitutes. Two Smith terms in one renderer is two
materials - one for its lights and one for its sky.

  - MEASURED AND ASSERTED: the table is exactly `(1.0, 4.1e-17)` at a smooth surface head-on, which
    is what the equations give when the lobe is a delta along the normal. `A` falls to 0.307 at
    roughness 1, and `B` rises to 0.140 at `dot(N,V)` = 0.1 against 0.022 at 0.5 - roughness takes
    energy out and grazing puts it into the bias, and neither number is a tuning constant.
  - THE WHITE FURNACE: a sky of uniform radiance 1 delivers an irradiance of exactly `pi` to a
    surface of any orientation. Measured 3.1416056 at the pole and 3.1416214 on the diagonal. It is
    the one test that catches a wrong band factor, a wrong basis normalisation and a missing solid
    angle all at once, and the residual is the quadrature's rather than the convolution's.
  - PREFILTERING A UNIFORM ENVIRONMENT RETURNS IT UNCHANGED at every roughness and direction, which
    is the prefilter's own furnace: a wrong weight, a missing normalisation or a sample left out of
    the divisor all show up there and in no single image.
  - NO `acos` ANYWHERE in the importance sample. The usual derivation writes `theta = acos(sqrt(..))`
    and then takes its sine and cosine again; the cosine IS that square root. In a `no_std` crate
    that is not tidiness - an inverse cosine is a series whose error would enter all 1024 samples.
  - `prefilter_levels(256)` is 6, which is exactly the profile's `environment_prefilter_levels`
    minimum, and the fixture asserts the code and the limit agree rather than both being 6 by
    coincidence.

`shadow.rs` - THE BIAS WHERE THE 3D PROFILE PUTS IT. The depth-bias equation is the 3D profile's
own, `offset = constant * r + slope * m` with `r = 2^(exponent(z) - 23)`, and the bias is applied in
the SHADOW PASS rather than subtracted in the lighting pass. Having both arrangements would bias
twice, which is why the module says which one it is.

  - COMPARE THEN FILTER, and the fixture holds what the other order costs: a receiver at 0.5 against
    a map where three of nine taps are behind it is lit three ninths, while filtering first averages
    to 0.367, compares once, and calls the whole footprint shadowed. That difference is a halo
    around every silhouette.
  - THE CASCADE SPLITS ARE WORKED OUT BY HAND in the fixture - 14.456, 30.25, 53.436, 100 for near 1
    and far 100 at lambda 0.5 - and the lambda ends are asserted to BE the two schemes, so a wrong
    blend cannot hide in the middle.
  - THE LAST SPLIT IS FORCED TO THE FAR PLANE EXACTLY. The blend of two schemes that both end at
    `far` ends a few bits away from it in `f32`, and a fragment at the far plane must not fall off
    the end of the last cascade because of a rounding difference.
  - A TIE IN THE CUBE-FACE MAJOR AXIS GOES TO THE EARLIER AXIS, so a direction exactly on a face
    edge picks one face rather than depending on which comparison the compiler evaluated first.

`postprocess.rs` - AND THE ONE FINDING IN THIS PASS THAT IS NOT ABOUT NEW CODE. See below.

  - The bloom knee's quadratic is `(L - T + K)^2 / (4K)`, which is zero with zero slope at `T - K`
    and equals `L - T` with unit slope at `T + K` - so the curve AND its derivative are continuous
    across the knee, which is what "and a quadratic between" has to mean for two implementations to
    agree. Hand-checked at 0.5, 1.0 and 1.5.
  - Both pyramid kernels partition unity, and the fixture asserts a constant field survives both. A
    mistyped weight, a missing tap and a wrong divisor all show up there.
  - Fog is `exp(-(density * distance)^2)`, hand-checked at `exp(-1)` and `exp(-4)`.

## THE 2D TONE MAP HAS A 47 PER CENT STEP AT DIFFUSE WHITE

FOUND BY WRITING THE 3D ONE, and it is in `graphics-core`, not in anything this pass wrote.

`pixel.rs`'s `tone_mapped` passes a luminance at or below 1 through UNCHANGED and applies extended
Reinhard above it. The curve is not the identity at 1 - it is `1 * (1 + 1/16) / 2` = 0.53125 - so
the two branches do not meet. Computed from the code:

    L = 0.9990   operator 0.530953   the 2D path 0.999000
    L = 1.0000   operator 0.531250   the 2D path 1.000000
    L = 1.0001   operator 0.531280   the 2D path 0.531280

A pixel at a luminance of 1.0001 is scaled to 53 per cent of its value and its neighbour at 1.0000
is not. That boundary runs through the middle of every lit surface in an HDR source, and what it
produces is a hard edge wherever luminance crosses diffuse white.

WHAT THE GUARD LOOKS LIKE IT WAS FOR: mapping only the highlights and leaving the rest alone. That
is a legitimate choice, and it needs a curve that IS the identity at its knee. Extended Reinhard is
not one. The guard and the curve are from two different designs.

NOTHING PINS THE BEHAVIOUR. There is no fixture over `tone_mapped`'s values anywhere in
`graphics-core`'s 856 test lines - only over `tone_map_white`, which is the constant rather than the
curve. The image-colour profile names the operator and the white point and says nothing about a
threshold, so the guard is the implementation's and not the profile's.

WHAT THIS PASS DID ABOUT IT: `scene3d::postprocess::tone_map` implements the operator the profile
NAMES, over the whole range and continuous, and says in its own comment why it has no such guard and
that the divergence is recorded here. It was not copied into the new code, and the 2D path was not
changed either - changing it changes every image that path produces, which is a decision about
output and not a defect fix an implementer takes while writing something else. The two now disagree
below a luminance of 1, and reconciling them is the open question.

## `exp`, `ln` AND `powf` IN `render-math`

NEEDED BY TWO OF THE RULES ABOVE AND BY NOTHING BEFORE THEM: a logarithmic cascade split at an
arbitrary cascade count, and exponential-squared fog. Neither is expressible with the square root
the crate already had - `x^(i/n)` is a chain of square roots only when `n` is a power of two, and
`max_shadow_cascades` is a minimum rather than a fixed four.

Written the way `sqrt` and `sin_cos` already were, for the reason that module states: this crate is
a layer that cannot take `libm` without putting it in every consumer.

  - `ln` decomposes `x = m * 2^e` from the bits, folds `m` about `sqrt(2)` so the series argument
    `|s| <= 0.1716`, and takes six odd terms of `2 * atanh(s)`. A SUBNORMAL IS SCALED INTO RANGE
    FIRST, because its stored exponent is zero and its mantissa is not the number's - reading the
    bits as a normal would answer for a different value entirely.
  - `exp` reduces by `k * ln(2)` with round-to-nearest so `|r| <= 0.347`, nine Taylor terms, and
    builds `2^k` from the exponent field. It SATURATES at the format's boundary rather than
    producing a denormal, which is what a fog factor at a distance of a million wants.
  - `powf` is `exp(y * ln(x))` with the two limits that are not: a power of zero is one for every
    base and a base of zero is zero for every positive power. Both are the limits and both are what
    a caller means; through the logarithm they are a NaN and an infinity.

Three fixtures against `f64` across twelve orders of magnitude, including a round trip through both
- which catches a wrong `ln(2)` in either, since they would have to be wrong by the same amount in
opposite directions to pass it - and `powf(x, 0.5)` against the Newton `sqrt` beside it.

## `f-ext`: the four playback modes the item asked for and the profile did not state

FOURTH AMENDMENT, FOURTH TIME FOR THE SAME REASON. The animation item names "step, linear and cubic
interpolation; loop, clamp and ping-pong; cross-fade and blending between several clips" and
morph-weight tracks. `ANIMATION_RULES` fixed the interpolation as linear and spherical-linear and
the ends as looping or not, and said nothing about the rest. `ANIMATION_PLAYBACK_RULES` now answers
eleven questions, and nine features joined the closed list with them. The hash moved to `869c0d3f`.

WHAT EACH ANSWER HAD TO DECIDE, because an implementation would otherwise decide it alone:

  - THE MODE IS PER TRACK AND NOT PER CLIP. A visibility flag wants step, a slide wants linear and a
    bounce wants cubic; one mode for a whole clip is what makes an author fake the others with extra
    keys, and a faked curve is one nothing can retime.
  - THE CUBIC IS HERMITE WITH AUTHORED TANGENTS, not Catmull-Rom. A derived tangent changes when a
    NEIGHBOURING key moves, so editing one key alters a curve three keys away and an author cannot
    fix a bounce without breaking the landing.
  - A CUBIC ROTATION IS COMPONENT-WISE AND THEN NORMALISED, NOT SLERPED - and the rule says why that
    is not a contradiction of the linear rule, which refuses exactly that for the linear case. A
    cubic through four control points has no spherical form at all, so the choice there is between a
    component-wise cubic and no cubic. A reader who had just read the linear rule would expect the
    same refusal, which is why it is written down rather than left to be inferred.
  - PING-PONG'S TURNING FRAMES ARE VISITED ONCE PER PERIOD. Holding the last frame for two frames
    while the direction changes is a stutter at both ends, once a cycle, for the life of the clip.
    A ping-pong needs no closed seam, because its ends meet themselves.
  - A TARGET ONLY ONE CLIP DRIVES IS TAKEN FROM IT UNCHANGED, at full value. The other clip says
    NOTHING about that joint, which is not the same as saying it should be at rest; scaling it by
    its clip's weight would pull the joint toward the origin as the blend moves away, which is a
    limb collapsing rather than a blend.
  - ROOT MOTION UNDER A BLEND IS THE SAME WEIGHTED SUM. Adding the two deltas would make a character
    cross-fading from a walk to a run briefly move faster than either clip ever asks for.
  - MORPH WEIGHTS ARE NOT NORMALISED ACROSS TARGETS, for the reason the displacement rule already
    gives: they are additive, and normalising them would make a second expression undo half of the
    first.

AND I CORRECTED MY OWN RULE BEFORE ANYTHING WAS BUILT ON IT. I first justified per-second cubic
tangents by saying that per-span ones would make a retimed clip change SHAPE rather than speed. That
is backwards: with per-second tangents a longer span DOES reach further, which is the whole point of
a velocity. What per-second buys is INVARIANCE TO KEY DENSITY - a slope of one unit per second means
the same however far apart the neighbouring keys are, so inserting a key at the value and slope the
curve already has there reproduces the same curve, where per-span tangents either side would have to
be rewritten. The rule now says that, and the fixture asserts the behaviour the corrected reason
predicts: the same out-tangent over half the span reaches half as far.

THE MODULE WAS RESTRUCTURED RATHER THAN EXTENDED. `Curve<T>` is now `Step | Linear | Cubic` with
`CubicKey` carrying the two tangents, because a cubic key IS a different thing from a linear one and
a single key type with tangents nobody reads is a trap an author falls into. `Track` addresses a
`Target::Joint` or a `Target::Morph` rather than carrying a bare joint index that would have had to
mean two things. A private `Interpolate` trait carries `blend`, `hermite`, `finite` and `seam` for
`Vec3`, `Quat` and `f32`, so the span walk, the end-holding and the tangent scaling are written ONCE
- three copies of them would be three places for an off-by-one at the last key.

AND ONE THING I WROTE AND THEN CAUGHT: the first version of the loop-seam check had a `seam` helper
that returned `0.0` unconditionally, which would have made the check pass for every clip. It was a
placeholder left behind while working out how to compare three different value types with one
function. The answer is the trait method above - a distance for a translation, a magnitude for a
scalar, `1 - |dot|` for a rotation - and the existing fixture for the open seam would have failed
had it shipped, which is what it is for.

117 fixtures in the crate, 57 before this part started.

## `f-ext`: the wiring into `Scene`

THE MODULES WERE REACHABLE AND NOTHING REACHED THEM, which is the state the three items above each
recorded as their own remaining work. Two joins close most of it.

THE LEVEL OF DETAIL IS SELECTED PER DRAWABLE PER VIEW, and the split between the two is the design
decision rather than a detail. `Drawable` carries the LADDER, because a ladder belongs to the mesh
and is the same everywhere. `detail::ViewDetail` carries the LEVEL HELD LAST FRAME, one per view,
because coverage is a function of where the camera is: two cameras looking at one scene pick
different levels for one drawable, and a single cached level on the drawable would make each view's
hysteresis overwrite the other's. That is a flicker which appears only once a second view exists and
which is traced to anything but the cache, so the type makes it impossible instead.

`detail::select` MIRRORS `queue::build`: `&mut Scene`, calls `update` itself, walks the drawables. It
reads the CAMERA'S SHAPE OUT OF THE PROJECTION MATRIX and not out of the fields the camera was built
from, for the same reason `cull` extracts its planes that way - a camera may carry a projection its
constructors cannot describe, and a coverage computed from a remembered field of view would be a
coverage for a different camera than the one the vertices go through. Column-major, row 1 of column
1 is `cot(fov/2)` or `1 / half_height`, and row 3 of column 3 tells the two apart: zero when the
projection divides by `w`, one when it does not.

A DRAWABLE WITH NO BOUNDS KEEPS ITS FINEST LEVEL rather than being given one at random. The core
profile already says an unbounded drawable is never culled; handing it a coarse mesh would be the
same disappearance by another route.

`animate::apply` DRIVES NODES THROUGH A `Skeleton`, because a clip talks about JOINTS and a scene is
made of NODES and the two numberings are not the same one - assuming `joint == node` is what stops
one clip driving two characters built at different times.

AND A CHANNEL THE CLIP DOES NOT DRIVE IS LEFT ALONE, which is the property that function exists for.
Writing an identity into it would make a clip that only ROTATES a wrist also move that wrist to the
origin and scale it to nothing; the clip would look correct in isolation and destroy any pose it was
blended into. A joint the skeleton does not map is REFUSED rather than skipped: animating part of a
character and leaving the rest in its bind pose reads as a broken rig rather than as a mismatched
pair of asset and clip.

TWO OF MY OWN FIXTURES WERE WRONG BEFORE THEY WERE RIGHT, AND BY THE SAME NUMBER. I computed the
coverage of a drawable whose local box runs from (-1,-1,-1) to (1,1,1) as though its bounding sphere
had a radius of 1. It is `sqrt(3)` = 1.7320508 - the sphere has to contain the box's CORNERS, which
is the core profile's own derivation and the number a reader most easily assumes wrong. Both
fixtures now state the radius in their working, and the orthographic one picks a half-height of
`2 * sqrt(3)` so its coverage lands on 0.5 exactly, which also holds the profile's "AT OR BELOW": a
coverage equal to a threshold belongs to the level that threshold names.

121 fixtures in `scene3d`, 57 before this part started. Every graphics crate passes: profile 73,
render-math 22, render3d 61, soft3d 81, render-shader 20, core 28, soft2d 28, render2d 33.

## `f-ext`: the conformance suite now walks the Extended profile

THE LAST THING EVERY ITEM OF THE PART WAS WAITING ON. `conformance3d` walked the two core profiles
and the Extended list was not among them, so "a test per feature" - which every item of the part
says - was held by `scene3d`'s own fixtures. That is a weaker claim than the part asks for: a
fixture proves THIS implementation does what the rule says, and a conformance case is what proves a
SECOND one does the same.

SIXTY SCENES IN SEVEN MODULES, one per entry, with `EXTENDED_CASES` walked by the same `run` the
core halves use and the same untested-feature check behind it: a feature added to the profile with
no scene is a failure of the suite.

A THIRD TALLY AND NOT A THIRD SET OF NUMBERS IN THE FIRST TWO. `Summary::complete()` deliberately
leaves Extended out, because the profile is OPTIONAL AS A WHOLE and folding it in would make
"conforms" mean something the profile does not say. `complete_with_extended()` is the separate claim
for a layer that says it carries the part, and the guest program prints the two on separate lines -
so a reader can tell a core-conforming implementation WITHOUT the part from one that claims it and
fails it.

AND A DEFECT IN MY OWN CLOSED LIST, FOUND BY WRITING THE SCENE FOR IT. The list had
`IrradianceCubeMap` AND `IrradianceSphericalHarmonics` as two entries. The profile says the
irradiance term is a cosine-convolved cube map and that "nine spherical-harmonic coefficients are
the PERMITTED ALTERNATIVE" - so two entries demand BOTH, which is a stricter claim than the profile
makes and one no conforming implementation would satisfy. They are now one entry, `IrradianceTerm`,
whose scene checks the ANSWER - a uniform sky delivering `pi` to every orientation - rather than
which of the two forms produced it. The enum says so where the entry is.

`shadow::MAP_FORMAT` WAS ADDED FOR THE SAME REASON. `ShadowMapDepth32F` was a feature whose rule
lived only in prose; there was nothing for a scene to read. The constant is
`render3d::DepthFormat::Depth32F`, so the scene checks the format the module actually names rather
than a sentence about it.

THE WHOLE SUITE PASSES ON THE HOST: 60 extended scenes beside the 89 render3d and 71 scene3d ones,
0 failed, 0 unsupported, 0 untested, and one case per entry asserted by count so neither list can
drift past the other.

## Part `e` is closed, and part `i`'s remaining route was computed rather than attempted

`e` HAD ONE OPEN ITEM AND TWO REASONS FOR IT, AND BOTH ARE GONE. The first was the approval, which
arrived on 2026-09-21. The second was written when it was true - "the shader model, the strict-float
rules and the rest of this part are untouched" - and the two items carrying them are now `[x]`, the
second closed on 2026-09-18 when `soft3d` and the conformance suite gave its third half somewhere to
live.

THE LIST WAS CHECKED AGAINST THE ITEM'S OWN ENUMERATION GROUP BY GROUP rather than assumed, because
a list that exists is not the same as a list that says what the item asks for. 89 entries in nine
groups, and the nine are the nine the item names: resources 8, geometry 16, pipeline 10, passes 9,
depth 15, msaa 6, sampling 15, formats 7, readback 3. Two group counts do not match a naive reading
of the prose and both are deliberate: "MSAA resolve" is counted once under `msaa` rather than twice
under `passes`, and the depth formats sit in `depth` rather than being repeated in `formats`.

BOTH CLAUSES OF THE CRITERION ARE ENFORCED AND NOT STATED. "A conforming backend implements every
entry" is `Tally::complete` requiring `untested` empty; "`Unsupported` is reserved for post-profile
extensions" is the same function requiring `unsupported` zero, so a backend REFUSING something in
Profile 1 does not conform rather than having a gap. `soft3d` answers all 89.

## THE 3D FLOOR: WHAT PARALLEL TILES CAN BUY, COMPUTED BEFORE BUILDING THEM

ITEM `i` NAMED PARALLEL TILE EXECUTION AS THE REMAINING ROUTE AND `SYS_PROCESS_SELF` UNBLOCKED IT.
Before writing a threaded rasteriser I computed what it can reach, from the measurement already in
`docs/PERF.md` rather than from a guess - and the answer changes what the item is waiting for.

THE STAGE TABLE IS CUMULATIVE AND THE SPLIT IT IMPLIES IS STARK: geometry 0.9 ms, everything else
933.0. Transform, clip and cull run before the tile loop; rasterisation, depth, interpolation, the
fragment stage, texturing and blending run inside it. Tiles are independent - disjoint pixels,
disjoint hierarchical-depth state, which I checked in `shade_bins` rather than assumed - so the
parallel fraction is 99.9 per cent and the frame is `0.9 + 933.0 / workers`:

    workers   frame       against 33.3 ms
    1         933.9 ms    28.0x over
    4         234.2 ms     7.0x
    8         117.5 ms     3.5x
    16         59.2 ms     1.8x
    30         32.0 ms    UNDER, and the first count that is

THIRTY, AND THAT IS THE OPTIMISTIC END: perfect scaling, balanced tiles, no synchronisation cost, no
memory-bandwidth saturation - none of which a software rasteriser writing a 640x480 attachment gets.

PER FRAGMENT IT IS SHARPER AND HARDER TO ARGUE WITH. 442,474 shaded fragments over 933.0 ms is
2,109 ns a fragment; the floor allows 73. The interpreter table says where it goes: the lit stage
with two texture reads is 41 IR statements at 35 ns each, 1,448 ns - more than two thirds of the
fragment before the rasteriser, the depth test or the blend is counted. One core at 73 ns would need
the shader stage under 2 ns a statement, a handful of machine instructions per IR statement. That is
not a tuned interpreter; it is a JIT, which the shader-model item explicitly deferred.

SO THE FLOOR HAS TWO ROUTES AND NEITHER IS "KEEP OPTIMISING": about thirty cores for one 640x480
frame, or a compiled fragment stage. Both are decisions rather than effort - the first is a
statement about what a frame may cost in machine and the second is the deferred JIT - and stating
which is which is what the item's own discipline asks for: "this box stays open with a measurement
under it rather than a plan".

WHAT I DID NOT DO, AND WHY IT IS RECORDED HERE RATHER THAN DONE. A threaded tile loop is real work
and would give four times on the documented host, so it is not wasted - but it is also a delicate
refactor of the most correctness-critical code in the renderer (one `Machine` and one `Stats` per
worker, the attachments split by tile-row bands, the hierarchical-depth state partitioned with
them), and the arithmetic above says it closes no box on the documented host. Building it first and
discovering that afterwards would have been the same conclusion at a much higher price.

## The Extended profile is proved in a booted guest, not only on the host

`kernel.applications.the_3d_profiles_conform_on_the_target` now asserts all three profiles:
`render3d 89`, `scene3d 71` and `scene3d-extended 60`, each with 0 failed, 0 unsupported and 0
untested, 220 scenes in all. That is the half a host fixture cannot give - the equations are exact
and their expected values come from the profile, so what a guest run adds is that the ARITHMETIC
AGREES THERE: the same GGX, the same Smith, the same white furnace, on the target's floating point
rather than on the machine that built the tree.

THE EXTENDED CLAIM IS ASSERTED SEPARATELY FROM "conforms", because it is a separate claim: the
profile is optional as a whole, so a layer carrying neither would still conform.

AND TWO MISTAKES OF MINE IN THE SAME FIFTEEN MINUTES, BOTH WORTH RECORDING BECAUSE BOTH ARE THE SAME
SHAPE. The first: the guest test's read loop breaks on a line containing "conforms", so the verdict
I added AFTER that line was never read, and the test failed saying the program had not printed
something it had. The break is now on the program's LAST line and says why. The second: my first fix
matched on text and landed in the 2D conformance test, which has a character-for-character identical
loop - so I quietly broke a passing test while trying to fix a failing one, and the failing one did
not move. The second attempt located the 3D occurrence by position after restoring the 2D one, and
both tests pass together, which is why they were run together rather than one at a time.

## The Extended part's join, its demo phase, and one feature that reached nothing (2026-09-21)

Three pieces of `f-ext` closed together, and the order they closed in is the finding.

**THE PBR MATERIAL'S JOIN WAS A DECISION AND IT IS WRITTEN DOWN BEFORE IT IS BUILT.** A drawable
names one material in one of two tables, and which table is part of the NAME rather than a flag
beside an index: `MaterialRef::Core` or `MaterialRef::Extended`. The shape that was rejected is a
core index with an optional Extended index beside it, whose ambiguous case is a drawable that one
pass reads as a `BlinnPhong` surface and another as a physically based one. `Drawable::new` keeps its
signature and means core, so a program implementing only the core profile never spells the enum -
which is what "the core stays unaware of the part" means in code rather than in a sentence.

**AND THE QUEUE RULE WAS MOVED RATHER THAN COPIED.** `queue`, `writes_depth` and `writes_id` are
functions of the blending and of nothing else, so they became methods on `Blending` and both material
families delegate. A second copy on the Extended type is how the two come to disagree about what a
transparent surface does, and the symptom - a PBR surface hiding what is behind it while a core one
does not - reads as a defect in the shading.

**THE LEVEL OF DETAIL REACHED NOTHING, AND A FIXTURE WRITTEN FOR A DIFFERENT CLAUSE FOUND IT.**
`detail::select` chose a level every frame into a `ViewDetail` that nothing read: the recorder drew
`drawable.mesh`. Twenty-four fixtures and sixty conformance scenes held the DECISION and none of them
touched the effect. What found it was `a_scene_that_uses_every_extended_feature_emits_only_render3d_commands`,
whose geometry source makes a mesh identifier its own vertex buffer so the recorded list says WHICH
level was drawn. The level is resolved into `Queued::mesh` now, where the per-view list already is,
and a vanished drawable is dropped with its own tally beside `culled`.

**THE DEMO PHASE PUT THE PROFILE'S SHAPE AGAINST A BACKEND FOR THE FIRST TIME**, and the backend
decided one thing the scene layer had left open: `Render3D Core Profile 1` has no depth-texture
binding and no comparison sampler, so the shadow pass writes light-space depth into a COLOUR
attachment and the frame reads it back into a texture between the passes. That is a real constraint
of the frozen profile rather than a demo shortcut, and it is where a GPU backend would bind the
attachment directly.

TWO THINGS THE COMPILER COULD NOT CATCH AND THE FIRST RUN WOULD HAVE:

- `BinaryOp::Cross` in the shader IR is THREE-COMPONENT and refuses anything else, which is right -
  a cross product of four-vectors is not defined. The tangent frame was written over `vec4`s and
  would have failed at `prepare` with a type mismatch. Caught by reading the interpreter rather than
  by running it, because the run costs a full image build.
- The source was edited while a build of it was in flight, which moves the digest and makes the
  `test.sh` after it refuse. The build was stopped and restarted rather than gambling on which half
  of the tree it had already read.

WHAT THE MEASUREMENT SAID: the light's map holds 18,512 of 262,144 texels, which is the sphere's
silhouette at that projection computed independently before the run and matched by it. A number
predicted and then measured is a different kind of evidence from a number read off a passing test.
