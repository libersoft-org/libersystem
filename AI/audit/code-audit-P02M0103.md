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
