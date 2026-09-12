#!/usr/bin/env bash
# The graphics profiles are code, and every document about them is generated from that code.
#
# WHAT THIS REFUSES. `Render2D Core Profile 1` and `Render3D Core Profile 1` are closed, enumerated
# lists. A profile that exists only as a table in a Markdown file drifts away from the backend and
# the conformance suite the first time somebody adds a feature in one place - so the table, the
# backend checklist, the conformance matrix and the capability report are all written from the
# enumeration, with a hash over the canonical form so a change to a profile is a line in a diff
# rather than something a reviewer might notice.
#
# AND THE THREE CHECKS NO REVIEWER RELIABLY CATCHES: every `Backend`-owned feature has a handler,
# every feature has at least one conformance test, and no test claims a feature outside the profile.
# The claims are markers in the source - `@handles:` on the code, `@covers:` on the test - so a
# deleted handler takes its claim with it, which a registry beside the code would not.
#
# WHAT IS NOT PERFORMED TODAY, AND SAYS SO. No backend and no conformance suite exist yet; the first
# two checks range over nothing and the tool prints NOT PERFORMED with the count awaiting a claim,
# rather than printing a pass a reader would take for one. That is also why the scanner proves it
# REFUSES before it is trusted to APPROVE: a clean tree says nothing about a scan that has stopped
# matching.
set -euo pipefail

cd "$(dirname "$0")/../.."
TOOL="src/tools/profile-doc/Cargo.toml"
PROFILE="src/user/libs/graphics/profile/Cargo.toml"

# THE PROFILE'S OWN FIXTURES FIRST: the list is closed, no name appears twice, every group has
# something in it, the queries are owned by `render2d` and the minima refuse a declaration below
# them. Everything below reads that list, so a broken list would be checked against itself.
cargo test --quiet --offline --manifest-path "$PROFILE" || exit 1

cargo run --quiet --offline --manifest-path "$TOOL" -- --self-test || exit 1
cargo run --quiet --offline --manifest-path "$TOOL" -- --check || exit 1
