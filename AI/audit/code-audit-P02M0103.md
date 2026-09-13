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
