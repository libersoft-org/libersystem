#!/usr/bin/env bash
# "Every numeric limit is tested" is a claim, and this is what makes it checkable.
#
# WHAT THIS REFUSES. The text milestone asks for each frozen ceiling to be exercised AT ITS EXACT
# BOUND AND ONE PAST IT, and that is a sentence nothing enforces. A ceiling tested only past its value
# could be off by one in either direction and nothing would say so - which for a limit that decides
# whether a document is refused is exactly the difference between "long" and "too long". A ceiling
# with no fixture at all is worse: a number in a published table that no code has been shown to
# respect.
#
# SO THE CLAIM IS A TABLE AND THE TABLE IS CHECKED, in both directions: a ceiling the profile
# publishes with nothing claiming to exercise it is refused, a claim naming a ceiling the profile does
# not publish is refused, and a claim naming a fixture that is not in the file it says it is in is
# refused - so a renamed or deleted test is caught rather than silently stopping.
#
# THREE STATES AND NOT TWO. A ceiling is exercised at its bound, or ENFORCED IN CODE WITHOUT A BOUND
# FIXTURE, or has NO SITE because the code it bounds is not written. The last two are printed loudly
# rather than folded into the pass: a gate that counted them as covered would be approving the absence
# of the thing it exists to check, and a reader deciding what to build next needs to know which of the
# two a ceiling is in.
#
# AND IT PROVES IT REFUSES BEFORE IT APPROVES. A checker exercised only over a currently-valid tree is
# not a checker.
set -euo pipefail

cd "$(dirname "$0")/../.."
TOOL="src/tools/text-limits/Cargo.toml"

# The fixtures the table points at, run rather than merely named: a gate that checked only that a test
# EXISTS would pass over one that fails.
for crate in opentype-profile font-parse font-shape font-run text-pipeline text-layout unicode-bidi; do
	cargo test --quiet --offline --manifest-path "src/user/libs/text/$crate/Cargo.toml" || exit 1
done

cargo run --quiet --offline --manifest-path "$TOOL" -- --self-test || exit 1
cargo run --quiet --offline --manifest-path "$TOOL" || exit 1
