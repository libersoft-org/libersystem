#!/usr/bin/env bash
# The catalogue publishes DECLARED metadata and never derives it. This is what checks the declaration.
#
# WHY THE CATALOGUE DOES NOT DERIVE IT. Deriving the family, the style and the axes from the face
# would put a second, unprofiled font parser inside a service - and the whole point of the closed
# OpenType profile is that exactly ONE component in this tree reads a font. So the declaration beside
# each staged face is trusted by the service.
#
# WHICH MAKES IT A CLAIM AND NOT A FACT, until something checks it. This gate is that check: the one
# component allowed to read a font parses every staged face and requires the family, the style, the
# format, the face index, the weight, the width, the slope and the axes it recovers to EQUAL the
# record - naming the face AND the field on a disagreement, because whoever staged it has to know
# which of eight things to fix.
#
# A STYLE IS A NUMBER AND A SET OF BITS, NOT A WORD, which is why the weight, the width and the slope
# are compared against `OS/2` rather than against the style string. "Bold" is a word in a language;
# 700 is not.
#
# AND IT HAS NOTHING TO CHECK YET, WHICH IS EXACTLY WHY IT PROVES IT REFUSES. No face is staged until
# the licensed last-resort face is imported, so this gate would otherwise pass over an empty set for
# months while its comparison quietly stopped comparing. The self-test builds a face, agrees with it,
# and is then required to DISAGREE with the same face under each field made wrong in turn. When a real
# face arrives the gate already covers it.
set -euo pipefail

cd "$(dirname "$0")/../.."
TOOL="src/tools/font-oracle/Cargo.toml"

# The declaration vocabulary's own fixtures first: the closed value sets, the ceilings, the digest
# comparison and the rule that a non-collection file has one face. Everything below reads that
# vocabulary, so a broken one would be checked against itself.
cargo test --quiet --offline --manifest-path "src/user/services/logic/Cargo.toml" || exit 1

cargo run --quiet --offline --manifest-path "$TOOL" -- --self-test || exit 1
cargo run --quiet --offline --manifest-path "$TOOL" || exit 1
