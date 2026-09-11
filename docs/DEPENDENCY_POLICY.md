# Dependency and licensing policy

THIS DOCUMENT IS WRITTEN BEFORE ANY UPSTREAM SOURCE IS FETCHED, and the order is the point. A policy
recorded after a dependency has been chosen is a policy fitted to the choice: it describes what the
thing already in the tree happens to satisfy, and the check it then performs can only pass. What
follows was written against no particular dependency, and the first one it governs is the first one
that arrives.

## What this repository's own code is

Everything LiberSystem writes is released under the Unlicense, which is the `LICENSE` file at the
root of this repository. That is a dedication to the public domain: it imposes no attribution
requirement, no notice requirement and no reciprocal obligation on anybody who takes it.

The consequence that matters here is one-directional. Code that arrives from elsewhere does NOT
become Unlicensed by being placed in this tree, and nothing in this repository may be read as making
that claim. An imported file keeps the licence it was published under, keeps its own notice, and
keeps its attribution; the root `LICENSE` covers LiberSystem's own work and says nothing about it.

## What may enter the tree, and under what terms

Imported sources - an upstream library, a compiler runtime, a conformance suite, a generated table -
keep their own compatible notices, and each import records three things exactly:

  SOURCE     the upstream project and the URL it was obtained from, written down rather than
             remembered. "Where did this come from" is the question a licence audit starts with, and
             a tree that cannot answer it has to re-derive the answer from file contents.
  VERSION    the exact revision - a tag or a commit, never a branch - and the SHA-256 of the source
             archive as fetched. A branch names whatever it points at today, so a dependency pinned
             to one is not pinned.
  PROVENANCE the patch series this repository applies on top, in order, each with its own digest.
             An upstream file that has been modified is no longer the upstream file, and a review of
             the upstream revision does not cover the modification.

COMPATIBLE MEANS PERMISSIVE, AND THE TEST IS SPECIFIC. A licence is compatible with this policy when
it permits use, modification and redistribution in both source and binary form without imposing
terms on LiberSystem's own code - Unlicense, public domain, MIT, BSD-2/3-Clause, ISC, Apache-2.0,
Zlib and the more-permissive per-file exceptions those projects carry. A licence that requires
derived or linked work to be published under its own terms is NOT compatible and does not enter this
tree, however convenient the component is. That is a decision about what this project is, not a
judgement about the licence.

A DUAL-LICENSED UPSTREAM IS RECORDED UNDER THE TERM THIS PROJECT TAKES IT UNDER, and under that one
only. "Available under A or B" is the upstream's offer; which of them was accepted is this
repository's fact, and leaving it unrecorded leaves the obligation unknown.

## What the build refuses

Three refusals, each of which is a build failure rather than a warning:

AN UNREVIEWED LICENCE. An imported file whose licence has not been recorded by this policy does not
build. Not "is flagged" - does not build. A warning on a licence question is a warning that gets
read once and then scrolls past, and the failure it is trying to prevent is discovered by somebody
else, later, in a distributed binary.

A DOWNLOADED-AT-BUILD SOURCE. The build fetches nothing. Sources are fetched ONCE, by a person, and
are then vendored in the tree or content-addressed by digest. A build that downloads is a build
whose output depends on what a remote server served at that moment, which makes it unreproducible
and makes its licence audit a statement about a file nobody kept. This applies to the audit-only
inputs of `P02M0135` exactly as it applies to anything shipped.

A COPIED IMPLEMENTATION WITH LOST ATTRIBUTION. Code that came from somewhere else and is presented
as this project's own is refused, and the refusal is not about legal exposure. An implementation
whose origin has been erased cannot be updated when the original is fixed, cannot be audited against
the original's own tests, and misleads every reader who assumes the surrounding conventions apply to
it. If a file is adapted rather than copied whole, the adaptation says so and names what it adapted.

## How the record binds to what is built

THE IMAGE IDENTITY RECORDS BIND GENERATED OBJECTS TO THIS INVENTORY. An artifact in a built image is
traceable to the sources it was produced from, and for an imported source that means traceable to
its recorded revision, archive digest and patch series. The binding is the point: a licence
inventory that lists what is in the tree, while the image is built from something else, documents a
tree nobody ships.

An import whose recorded digest does not match the source actually compiled is the same failure as
an unreviewed licence, and fails the same way.

## Audit-only inputs

An AUDIT-ONLY input is fetched, compiled, inspected and quarantine-staged for a gate, and nothing it
produces is imported into a shipping image or named by a production manifest. `P02M0135`'s pinned
Vulkan loader configuration is the first of these.

IT IS UNDER THE SAME LICENCE REVIEW AS ANYTHING ELSE THAT ENTERS THE TREE, and the reason is that
"audit-only" describes what is done with the output, not whether the source is present. The source
IS present: it is in the tree, it is compiled, and an artifact derived from it traverses the guest's
launch path. Exempting it would make the exemption the interesting case - the one place where an
unreviewed licence sits in the repository, justified by a distinction the file system does not make.

What audit-only DOES change is the scope of the obligation, and that is recorded rather than
assumed: a notice requirement that attaches to redistribution of a binary is not triggered by an
artifact that is never distributed, while a notice requirement that attaches to possession of the
source is. The recorded review states which applies.
