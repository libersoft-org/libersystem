#!/usr/bin/env bash
# TWO SUITES OF ONE ARCHITECTURE, AT THE SAME TIME, EACH REPORTING ITS OWN SELECTION.
#
# P02M0167's definition of done asks for exactly this and nothing in the tree performed it. The
# machinery it is about is all there - the selection-specific kernel is compiled and staged under the
# build lock, the medium is content-addressed on that staged kernel, the loader is staged the same
# way, and every run's logs are named by its own pid - but each of those was argued for in a comment
# rather than demonstrated together. The failure this replaces was REPRODUCED once, by hand, and then
# fixed with no standing proof; a property with no gate is a property that regresses quietly.
#
# WHY THE ASSERTION IS "EACH REPORTS ITS OWN SELECTION" AND NOT "BOTH PASSED".
#
# Two runs that both pass prove nothing about isolation: they would both pass if one had booted the
# other's kernel. `TEST_TAGS` is a COMPILE-TIME filter - `option_env!`, baked into the binary - so the
# tags a guest announces are a property of the executable that booted, not of the command line that
# asked for it. Two runs with DIFFERENT tags therefore give each guest a different, checkable identity,
# and a run that boots the other's staged kernel says so in its own log.
#
# The two selections are deliberately small and disjoint so the gate costs two short guests rather
# than two full suites.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
# shellcheck source=result-logs.sh
. "$HERE/result-logs.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() {
	echo "concurrent-selection: $*" >&2
	exit 1
}

# The two selections. Each is a tag the kernel suite actually has, and they do not overlap - so the
# effective filter each guest prints is unique to its own run.
A_TAGS="dma"
B_TAGS="domain"

echo "concurrent-selection: starting two x86_64 suites at once - tags '$A_TAGS' and '$B_TAGS'"
(cd "$REPO_ROOT" && ./test.sh --arch x86_64 --tags "$A_TAGS") >"$work/a.log" 2>&1 &
a_pid=$!
(cd "$REPO_ROOT" && ./test.sh --arch x86_64 --tags "$B_TAGS") >"$work/b.log" 2>&1 &
b_pid=$!

# BOTH ARE WAITED FOR BEFORE EITHER IS JUDGED, so a failure in the first does not leave the second
# running past the end of this script and into whatever runs next.
wait "$a_pid"
a_status=$?
wait "$b_pid"
b_status=$?

# AND BOTH HAD TO SUCCEED. A collision in this tree does not produce a wrong answer - the medium
# builder recomputes its input key and DIES, which is the failure P02M0167 reproduced - so a run that
# failed is the symptom this gate is looking for and not a reason to skip the rest of it.
if ((a_status != 0)); then
	tail -25 "$work/a.log" >&2
	fail "the '$A_TAGS' suite failed while a second suite of the same architecture was running (exit $a_status)"
fi
if ((b_status != 0)); then
	tail -25 "$work/b.log" >&2
	fail "the '$B_TAGS' suite failed while a second suite of the same architecture was running (exit $b_status)"
fi

# THE LOGS EACH RUN SAID IT WROTE, never the newest on disk - which is the read this whole gate exists
# to make impossible to get away with.
mapfile -t a_logs < <(result_logs "$work/a.log") || fail "the '$A_TAGS' run did not say which logs it wrote"
mapfile -t b_logs < <(result_logs "$work/b.log") || fail "the '$B_TAGS' run did not say which logs it wrote"
((${#a_logs[@]})) || fail "the '$A_TAGS' run named no readable log"
((${#b_logs[@]})) || fail "the '$B_TAGS' run named no readable log"

# TWO RUNS, TWO SETS OF FILES. A shared result log would make every assertion below meaningless.
for path in "${a_logs[@]}"; do
	for other in "${b_logs[@]}"; do
		[[ "$path" != "$other" ]] || fail "both runs wrote to $path - they did not have their own result logs"
	done
done

cat "${a_logs[@]}" >"$work/a.result"
cat "${b_logs[@]}" >"$work/b.result"

# EACH GUEST ANNOUNCES THE FILTER IT WAS COMPILED WITH. A run that booted the other's staged kernel
# prints the other's tags here, which is precisely the collision the staging is meant to prevent.
selection_of() {
	sed -n 's/^.*test tags: requested=\([^,]*\),.*$/\1/p' "$1" | tail -1
}
a_seen="$(selection_of "$work/a.result")"
b_seen="$(selection_of "$work/b.result")"
[[ -n "$a_seen" ]] || fail "the '$A_TAGS' run's log does not say which selection its guest ran"
[[ -n "$b_seen" ]] || fail "the '$B_TAGS' run's log does not say which selection its guest ran"
[[ "$a_seen" == "$A_TAGS" ]] || fail "the run asked for '$A_TAGS' and its guest ran '$a_seen' - it booted a kernel built for another selection"
[[ "$b_seen" == "$B_TAGS" ]] || fail "the run asked for '$B_TAGS' and its guest ran '$b_seen' - it booted a kernel built for another selection"
[[ "$a_seen" != "$b_seen" ]] || fail "both guests ran the same selection, so the two runs were not independent"

echo "concurrent-selection: both suites passed while overlapping, each on its own medium and its own logs"
echo "concurrent-selection:   '$A_TAGS' -> $(basename "${a_logs[0]}")"
echo "concurrent-selection:   '$B_TAGS' -> $(basename "${b_logs[0]}")"
