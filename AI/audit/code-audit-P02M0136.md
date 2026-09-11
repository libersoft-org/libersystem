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
