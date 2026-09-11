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
