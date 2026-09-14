#!/usr/bin/env bash
# THE TEXT CONFORMANCE GATE: a pinned corpus of authored faces, shaped and compared.
#
# WHAT THIS IS FOR, AND WHY IT IS NOT THE UNIT TESTS. A shaping unit test hands the shaper one lookup
# and checks that it fires, which is necessary and is not conformance: it cannot show a face whose
# script list, feature list and lookup list must be walked in the order the format defines, with
# several scripts in one face and positional features that must reach one letter and not its
# neighbour. This reads WHOLE FACES and compares against expectations written from each face's own
# design.
#
# THE CORPUS IS AUTHORED, WHICH IS WHY IT EXISTS AT ALL. The font licence decision admits public
# domain and Unlicense only, and no face a text stack would reach for is in that set - so for months
# there was no corpus and this gate could not be built. `src/tools/font-gen` authors one instead, and
# that turns out to be the better answer rather than the available one: to assert an expected glyph
# INDEX and POSITION a gate has to know what the face DECLARES, and for an imported face that
# knowledge is reverse-engineered and written down as a guess.
#
# THREE THINGS RUN HERE AND EACH ANSWERS A DIFFERENT QUESTION.
#   1  the generator's own `--check`, so the pinned corpus is exactly what the generator produces
#   2  the gate's `--self-test`, so a corpus that drifted, an expectation one unit out of true, a
#      glyph name nothing declares and a wrong visual order are each REFUSED
#   3  FOUR DELIBERATE DEFECTS in the shaper itself, each watched to fail. A gate that has never
#      failed is a gate nobody has tested: every case in it passes today, and passing is what a case
#      does when the thing it checks has quietly stopped being checked.
#
# THE MUTATIONS ARE ON A COPY. Nothing here writes the tree.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

fail() {
	echo "text-corpus: $*" >&2
	exit 1
}

# The corpus is what the generator says it is, before anything reads it.
cargo run --quiet --offline --manifest-path src/tools/font-gen/Cargo.toml -- --check || exit 1

# The gate proves it refuses, and then it approves.
cargo run --quiet --offline --manifest-path src/tools/text-corpus/Cargo.toml -- --self-test || exit 1
cargo run --quiet --offline --manifest-path src/tools/text-corpus/Cargo.toml || exit 1

# -------------------------------------------------------------------------------------------------
# The mutations.
# -------------------------------------------------------------------------------------------------

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# A PRIVATE TREE WITH THE SAME SHAPE, because the crates reach each other by relative path and the
# tool finds the corpus by walking up to `product.conf`. Copying the layout rather than rewriting the
# manifests keeps the mutated build identical to the real one in everything but the one line.
mkdir -p "$work/src/user/libs" "$work/src/boot" "$work/src/tools" "$work/src/tests"
cp -r src/user/libs/text "$work/src/user/libs/text"
cp -r src/boot/protocol "$work/src/boot/protocol"
cp -r src/tools/text-corpus "$work/src/tools/text-corpus"
cp -r src/tests/fonts "$work/src/tests/fonts"
cp product.conf "$work/product.conf"

# A mutation that matches nothing is a gate that stopped testing what it names, so a miss is an error
# rather than a skip. This is the same helper the model mutations use, for the same reason.
mutate() {
	python3 "$HERE/model-mutate.py" "$work/$1" "$2" "$3"
}

restore() {
	cp "$1" "$work/$2"
}

# Each mutation: a name, the file, the literal to replace, what to replace it with, and the case that
# must notice. The case name is not decoration - a mutation caught by a DIFFERENT case than the one
# it was aimed at means one of the two cases is not testing what it says.
run_mutation() {
	local name="$1" file="$2" old="$3" new="$4" expect="$5"
	restore "$file" "$file"
	mutate "$file" "$old" "$new" || fail "$name: the mutation did not apply"
	local output status=0
	output="$(cd "$work" && cargo run --quiet --offline --manifest-path src/tools/text-corpus/Cargo.toml 2>&1)" || status=$?
	restore "$file" "$file"
	if [[ "$status" -eq 0 ]]; then
		fail "$name: the corpus still passed with the defect in place"
	fi
	if ! grep -qF "$expect" <<<"$output"; then
		echo "$output" >&2
		fail "$name: the run failed, and NOT on the case this defect was aimed at: $expect"
	fi
	echo "text-corpus: the defect '$name' is caught by: $expect"
}

# 1. `ccmp` NEVER APPLIED. It is the feature every shaper applies for every script, and a stack that
#    left it out draws Thai's sara am with a glyph no face means to be used alone.
#    THE TAG IS CHANGED RATHER THAN THE LINE DELETED, because a deleted line leaves `features` with no
#    mutation and `warnings = "deny"` turns that into a build failure - which is not the defect being
#    tested, and would be caught for the wrong reason.
run_mutation "ccmp is never selected" \
	"src/user/libs/text/font-shape/src/shape.rs" \
	'(*b"ccmp", crate::buffer::GLOBAL)' \
	'(*b"zzzz", crate::buffer::GLOBAL)' \
	"sara am is decomposed and its upper half attaches"

# 2. A POSITIONAL FEATURE APPLIED GLOBALLY. `isol` runs first of the four, so a global mask turns
#    every letter into its isolated form and the joining lookups after it find nothing left to do -
#    which is a word rendered as a row of disconnected shapes.
run_mutation "isol applies everywhere" \
	"src/user/libs/text/font-shape/src/scripts.rs" \
	'(*b"isol", ISOL),' \
	'(*b"isol", GLOBAL),' \
	"two joining letters are initial and final"

# 3. THE INDIC REORDERING SKIPPED, which leaves every pre-base vowel sign drawn after its consonant.
run_mutation "the Indic reordering does not run" \
	"src/user/libs/text/font-shape/src/shape.rs" \
	'Shaper::Indic => scripts::reorder_indic(&characters, buffer),' \
	'Shaper::Indic => scripts::reorder_indic(&characters[..0], buffer),' \
	"a pre-base vowel sign is drawn before its consonant"

# 4. A MARK MEASURED FROM AFTER ITS BASE rather than from the base itself, which leaves every accent
#    one letter's width to the right - the defect that looks correct on a narrow base.
run_mutation "a mark is measured from after its base" \
	"src/user/libs/text/font-shape/src/gpos.rs" \
	'a wide base is an accent floating over the NEXT letter.
	let advance_between: i32 = buffer.positions[base_at..at]' \
	'a wide base is an accent floating over the NEXT letter.
	let advance_between: i32 = buffer.positions[base_at + 1..at]' \
	"a mark is placed by its anchors"

echo "text-corpus: the corpus passes, the gate refuses what it should, and four deliberate defects are each caught"
