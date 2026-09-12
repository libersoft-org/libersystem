#!/usr/bin/env bash
# Segmentation is generated from a pinned Unicode release, and measured against Unicode's own answers.
#
# WHAT THIS REFUSES. Three things a text stack gets wrong quietly: a property table that has drifted
# from the release it claims, a grapheme cluster boundary in the middle of a letter, and a line break
# where the algorithm does not allow one. None of them crashes anything - they put a cursor inside a
# character, delete half an emoji, or break a line where no language would - so nothing but a
# conformance run finds them.
#
# THE ANSWERS ARE UNICODE'S, NOT THIS TREE'S. `GraphemeBreakTest`, `WordBreakTest` and
# `LineBreakTest` are run IN FULL: every case the release publishes, including the ones no
# implementer would think to write. A representative sample is exactly the set of cases somebody
# already believed they handled.
#
# AND THE TABLES ARE REGENERATED AND COMPARED, because a generated file that has been edited is a
# generated file that lies about where it came from. The UCD is pinned by SHA-256 in
# `toolchain.lock`; this gate reads the cache and never the network.
set -euo pipefail

cd "$(dirname "$0")/../.."
TABLES="src/user/libs/text/unicode-tables/Cargo.toml"
SEGMENTATION="src/user/libs/text/unicode-segmentation/Cargo.toml"
GENERATOR="src/tools/ucd-gen/Cargo.toml"
CONFORMANCE="src/tools/unicode-conformance/Cargo.toml"

if ! ./bootstrap.sh --verify >/dev/null 2>&1; then
	echo "unicode-segmentation: the pinned UCD is not cached - run ./bootstrap.sh" >&2
	./bootstrap.sh --verify || true
	exit 1
fi

# The tables' own shape first - sorted, disjoint, and answering what the UCD says for characters a
# person can check by hand. Everything below reads them.
cargo test --quiet --offline --manifest-path "$TABLES" || exit 1
cargo test --quiet --offline --manifest-path "$SEGMENTATION" || exit 1

# Then that they are what the pinned release generates, rather than what somebody edited them into.
cargo run --quiet --offline --manifest-path "$GENERATOR" -- --check || exit 1

# Then Unicode's own answers, every case of them.
cargo run --quiet --offline --manifest-path "$CONFORMANCE" || exit 1
