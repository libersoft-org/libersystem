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
