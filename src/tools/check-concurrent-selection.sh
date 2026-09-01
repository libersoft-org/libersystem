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

# THE TWO SELECTIONS, AND THEY DIFFER IN BOTH DIMENSIONS (corrected 2026-09-01).
#
# A previous version varied only `TEST_TAGS` and left `TEST_SELECTION` unset, so `test-kernel.sh`
# compiled both kernels with the same empty selection. That proves TAG isolation, and the requirement
# is simultaneous suites with different `TEST_SELECTION` AND different `TEST_TAGS`, each reporting
# its own selection - so the gate was weaker than the clause it exists for.
#
# `TEST_SELECTION` is an exact list of stable test IDs and the runner HARD-FAILS on one it does not
# have, which is what makes a stale ID here a loud failure rather than a smaller run. The two sets are
# disjoint, so each guest's log names its own tests and none of the other's - a stronger identity than
# the tags line, and the one the definition of done asks for.
A_TAGS="dma"
A_SELECTION="kernel.object.channel.channel_endpoint_semantics,kernel.object.channel.blocking_wait_wakes_on_message"
B_TAGS="domain"
B_SELECTION="kernel.object.channel.a_returned_message_is_still_charged_to_the_sender,kernel.object.channel.channel_message_and_capability_transfer"

# AND THE NUMBER OF GUESTS IS THE SCHEDULER'S ANSWER, NOT THIS SCRIPT'S.
#
# This started two suites unconditionally. Under `verify.sh --jobs 1` that made two QEMUs run on a
# machine whose one answer to "how many may run" was one - a second scheduler with a width of two,
# which is the thing P02M0167's rule exists to forbid. The runner declares the budget it is willing
# to hand this step; the gate refuses rather than exceeding it, and a budget that cannot hold the
# overlap is a budget this gate cannot be proved in.
#
# Unset means nobody is scheduling - a person typing `./check.sh --gate concurrent-selection`, which
# is the same exemption `test.sh --arch all` has and for the same reason.
NEEDS_GUESTS=2
if [[ -n "${LIBER_CONCURRENT_GUESTS:-}" ]] && ((LIBER_CONCURRENT_GUESTS < NEEDS_GUESTS)); then
	fail "this gate starts $NEEDS_GUESTS guests at once and the runner allows ${LIBER_CONCURRENT_GUESTS} - it cannot be proved inside that budget"
fi

echo "concurrent-selection: starting two x86_64 suites at once, differing in BOTH selection and tags"
(cd "$REPO_ROOT" && TEST_SELECTION="$A_SELECTION" ./test.sh --arch x86_64 --tags "$A_TAGS") >"$work/a.log" 2>&1 &
a_pid=$!
(cd "$REPO_ROOT" && TEST_SELECTION="$B_SELECTION" ./test.sh --arch x86_64 --tags "$B_TAGS") >"$work/b.log" 2>&1 &
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

# EACH GUEST RAN ITS OWN SELECTION AND ONLY ITS OWN.
#
# `TEST_SELECTION` is compiled in, so the tests a guest runs are a property of the executable that
# booted rather than of the command line that asked for it. A run that booted the other's staged
# kernel therefore runs the OTHER's tests, and says so by name in its own log - which is the
# collision the per-run staging exists to prevent, and the reason this assertion is on the test IDs
# rather than on "both passed". Two runs that both pass prove nothing about isolation.
ran_ids() {
	# The id is the start of each test line, before the `...`.
	grep -aoE '^kernel\.[a-z_.0-9]+' "$1" | sed 's/\.*$//' | sort -u
}
a_ran="$(ran_ids "$work/a.result")"
b_ran="$(ran_ids "$work/b.result")"
[[ -n "$a_ran" ]] || fail "the first run's log names no test that ran"
[[ -n "$b_ran" ]] || fail "the second run's log names no test that ran"

# ITS OWN, ALL OF THEM. A selection the runner could not satisfy is a hard failure inside the guest,
# so a missing id here means the run booted something else.
check_ran() {
	local which="$1" selection="$2" ran="$3" id
	while IFS= read -r id; do
		[[ -z "$id" ]] && continue
		grep -qxF "$id" <<<"$ran" || fail "the $which run selected '$id' and its guest did not run it - it booted a kernel built for another selection"
	done < <(tr ',' '\n' <<<"$selection")
}
check_ran first "$A_SELECTION" "$a_ran"
check_ran second "$B_SELECTION" "$b_ran"

# AND NONE OF THE OTHER'S, which is the half that catches a swap rather than a stale log.
check_absent() {
	local which="$1" selection="$2" ran="$3" id
	while IFS= read -r id; do
		[[ -z "$id" ]] && continue
		grep -qxF "$id" <<<"$ran" && fail "the $which run ran '$id', which belongs to the OTHER selection - the two guests were not independent"
	done < <(tr ',' '\n' <<<"$selection")
	return 0
}
check_absent first "$B_SELECTION" "$a_ran"
check_absent second "$A_SELECTION" "$b_ran"

echo "concurrent-selection: both suites passed while overlapping, each on its own medium, its own logs and its own selection"
echo "concurrent-selection:   selection A ($A_TAGS) -> $(basename "${a_logs[0]}")"
echo "concurrent-selection:   selection B ($B_TAGS) -> $(basename "${b_logs[0]}")"
