#!/usr/bin/env bash
# A GATE THAT NAMES A TEST NAMES ONE THAT EXISTS.
#
# `tags` and `covers` are different instruments and this is where the difference bites. `covers`
# answers "what would this test catch" and drives the change selector; a `tag` answers "what group
# does this test belong to" and is what `./test.sh --tags` and the gates pull on. Neither is an
# assertion. What a gate asserts on is an ORACLE - a named test that must have run and passed here,
# or a named line its fixture must have printed - and a gate that asks for a whole subject tag while
# meaning five named tests is buying every test that subject will ever acquire.
#
# WHAT ROTS, AND IS WHAT THIS REFUSES: a gate keeps naming a test id after the test is renamed. The
# grep then matches nothing, the gate reports the profile it was asserting about as unproven or -
# worse, where the assertion is an absence - as proven. Counting tests per tag would not notice;
# checking that every id a gate names is an id the tree still declares does.
#
# NOT A DEMAND THAT EVERY GATE NAME ONE. A host-only gate and one asserting directly on an artifact
# have no test ids to name, and requiring some of them would be a rule satisfied by inventing them.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$root/../.." && pwd)"
failed=0

# Every id the tree declares, from the declarations themselves - the same place the model reads.
declared="$(mktemp)"
trap 'rm -f "$declared"' EXIT
grep -rhoE 'id = "kernel\.[a-z0-9_.]+"' "$repo/src/kernel" | sed 's/id = "//; s/"$//' | sort -u >"$declared"
[[ -s "$declared" ]] || {
	echo "gate-oracles: no test ids were found in src/kernel - this check cannot answer and says so rather than passing" >&2
	exit 1
}

named=0
for gate in "$root"/check-*.sh; do
	name="${gate##*/}"
	[[ "$name" == "check-gate-oracles.sh" ]] && continue
	# Code only: several of these files quote an id in prose while explaining what they stopped doing.
	while read -r id; do
		[[ -n "$id" ]] || continue
		named=$((named + 1))
		# A PREFIX IS A GROUP, NOT AN ID, and both are legitimate here: `kernel.mem.numa` is how the
		# numa gate names a family of tests it then completes with a suffix. Accept an id the tree
		# declares, or a prefix of at least one.
		if grep -qxF "$id" "$declared" || grep -qF "$id." "$declared"; then
			continue
		fi
		echo "gate-oracles: $name asserts on '$id', which no test in this tree declares - the grep it is in matches nothing, so the gate is asserting about a test that is not there" >&2
		failed=1
	done < <(grep -vE '^[[:space:]]*#' "$gate" | grep -ohE 'kernel\.[a-z0-9_]+(\.[a-z0-9_]+)+' | sort -u)
done

((failed == 0)) || exit 1
echo "gate-oracles: $named test id(s) named by gates, and every one of them is declared"
