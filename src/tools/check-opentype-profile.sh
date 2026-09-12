#!/usr/bin/env bash
# The OpenType profile is code, and the document about it is generated from that code.
#
# WHAT THIS REFUSES. `OpenType Profile 1` is the closed list of what a font may contain and this
# system will read: the tables and their versions, the `GSUB`/`GPOS` lookup types, flags and
# subtable formats, the conditional mechanisms, the variation mechanisms, the `COLR` paint graph and
# its composite modes, the bitmap strikes, and the named scripts and languages. Anything outside it
# is a typed `Unsupported` refusal rather than a parse that half works - which matters more here than
# anywhere else in this tree, because a font is untrusted content that arrives from a document, a
# download or a package, and font parsers are a classic memory-safety target.
#
# AND THE EXCLUSIONS ARE PART OF THE LIST. A reader cannot tell a deliberate omission from a
# forgotten one by looking at what IS supported, so the refused structures are enumerated with their
# reasons, and the fixtures hold nothing to being both supported and excluded.
#
# WHY A GATE RATHER THAN A DOCUMENT NOBODY RUNS. The parser this profile bounds is not written yet;
# the profile's publication is a START GATE for it, and a start gate that drifts before the work
# begins has bounded nothing. The hash over the canonical form is what makes a change to the list a
# line in a diff.
set -euo pipefail

cd "$(dirname "$0")/../.."
PROFILE="src/user/libs/text/opentype-profile/Cargo.toml"
TOOL="src/tools/profile-doc/Cargo.toml"

# THE PROFILE'S OWN FIXTURES FIRST: both halves of "correct at any coordinate", every lookup type the
# format has, the paints whose variable counterparts exist and the one that has none, every script
# named with its shaping class, and nothing both supported and excluded. The document below is
# written from that list, so a broken list would be checked against itself.
cargo test --quiet --offline --manifest-path "$PROFILE" || exit 1

# Then the generated document and its hash. `--check` covers every profile this tool writes; the
# graphics gate runs the same command for its own half, and running it twice costs a second.
cargo run --quiet --offline --manifest-path "$TOOL" -- --check || exit 1
