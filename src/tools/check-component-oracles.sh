#!/usr/bin/env bash
# EVERY STAGED DRIVER AND SERVICE HAS AN ORACLE THAT NAMES IT, or a written reason why it has none.
#
# A change to one driver used to select no guest test at all - nine of twelve hundred keys, three
# builds, no boot - and the answer to that is not "run the whole suite" and not "run nothing". It is
# that something in the suite FAILS when that driver breaks, and says which driver it was about.
#
# THE ANNOTATION IS NOT THE ORACLE. Writing `covers = ["bin.virtio_input"]` on a test that never
# asserts anything which fails when that driver breaks moves the plan and catches nothing. What this
# check can see is the annotation; what makes it worth having is the rule above it, which is why the
# exception list carries a REASON rather than a name on its own.
#
# THE CONTRACT IS A READINESS STATE PLUS AN OBSERVABLE EFFECT, and the state is not one word for
# both halves: a DRIVER reaches `Online`, a SERVICE reaches `Ready`. Neither state is the oracle on
# its own - `virtio_console` reports online having published no provider at all - so what a test
# asserts is the effect: a banner that went out through a transmit queue, a file served, a lease
# renewed.
#
# A CENSUS, NOT AN EXAMPLE. One driver demonstrated proves the mechanism and not the coverage, and an
# exception nobody can enumerate is a gap that reads like a decision.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$root/../.." && pwd)"
manifest="$repo/src/user/services/manifest.toml"
exceptions="$root/component-oracle-exceptions.txt"
[[ -f "$manifest" ]] || {
	echo "component-oracles: $manifest is missing" >&2
	exit 1
}

covered="$(mktemp)"
trap 'rm -f "$covered"' EXIT
grep -rhoE 'covers = \[[^]]*\]' "$repo/src/kernel" | grep -ohE 'bin\.[a-z0-9_]+' | sort -u >"$covered"

missing=0
counted=0
while read -r role name; do
	counted=$((counted + 1))
	if grep -qxF "bin.$name" "$covered"; then
		continue
	fi
	reason=""
	if [[ -f "$exceptions" ]]; then
		reason="$(awk -F'\t' -v want="$name" '$1 == want { print $2 }' "$exceptions")"
	fi
	if [[ -n "$reason" ]]; then
		continue
	fi
	echo "component-oracles: the $role '$name' is staged and no test covers bin.$name - give it a test that fails when it breaks, or add a line to ${exceptions##*/} saying why it has none" >&2
	missing=$((missing + 1))
done < <(awk '
	/^\[\[programs\]\]/ { name = ""; role = "" }
	/^name = "/ { gsub(/^name = "|"$/, ""); name = $0 }
	/^role = "/ { gsub(/^role = "|"$/, ""); role = $0
		if ((role == "driver" || role == "service") && name != "") print role, name }
' "$manifest" | sort -u)

((missing == 0)) || {
	echo "component-oracles: $missing of $counted staged driver(s) and service(s) have neither an oracle nor a stated reason" >&2
	exit 1
}
echo "component-oracles: all $counted staged driver(s) and service(s) have an oracle naming them or a written reason"
