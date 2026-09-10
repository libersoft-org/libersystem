#!/usr/bin/env bash
# TWO SUITES OF ONE ARCHITECTURE, AT THE SAME TIME, EACH REPORTING ITS OWN SELECTION.
#
# Concurrent selections require this standing proof. The
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
# THE PHASE LOGS OUTLIVE THIS SCRIPT when a run is collecting evidence: copied into the run from the
# EXIT trap, before the directory is removed - on failure too, which is when they matter.
# shellcheck source=evidence.sh
source "$(dirname "${BASH_SOURCE[0]}")/evidence.sh"
# THIS IS A NAMED GATE, AND IT SAYS SO BEFORE INVOKING ANYTHING. The run mode is the one carrier of
# which matrix row a boot is on, set by the outermost entry point that knows and left alone by the
# runners it invokes - so a gate's test-kernel phase runs under `gate` and not on the `test` row.
export LIBER_RUN_MODE="${LIBER_RUN_MODE:-gate}"
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
# shellcheck source=result-logs.sh
. "$HERE/result-logs.sh"

bash "$REPO_ROOT/src/harness/test-media-generations.sh" || exit 1

work="$(mktemp -d)"
trap 'evidence_keep_gate "$work"/a.log "$work"/b.log; rm -rf "$work"' EXIT

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
B_TAGS="domain"
# AND EACH SELECTION IS LARGE ENOUGH THAT THE GUESTS MEET (corrected 2026-09-09).
#
# Two selections of two tests each never overlapped: the build lock serialises the two compiles, a
# two-test guest lives about nine seconds, and the second compile takes about as long - so the first
# guest had exited before the second booted, every time, and the gate proved isolation between two
# runs that were never simultaneous. The watcher below now REQUIRES the overlap, so the selections
# have to make it happen whichever run takes the lock first: the whole `kernel.object` family, read
# from the declarations the model reads, split alternately into two disjoint halves of roughly
# fifty tests - a guest of half a minute against a compile of a few seconds, in either order.
mapfile -t object_ids < <(grep -rhoE 'id = "kernel\.object\.[a-z0-9_.]+"' "$REPO_ROOT/src/kernel" | sed 's/^id = "//; s/"$//' | sort -u)
((${#object_ids[@]} >= 20)) || fail "the kernel.object family has ${#object_ids[@]} declared tests, too few to make two guests that meet"
a_ids=()
b_ids=()
for i in "${!object_ids[@]}"; do
	if ((i % 2 == 0)); then a_ids+=("${object_ids[$i]}"); else b_ids+=("${object_ids[$i]}"); fi
done
A_SELECTION="$(
	IFS=,
	echo "${a_ids[*]}"
)"
B_SELECTION="$(
	IFS=,
	echo "${b_ids[*]}"
)"
echo "concurrent-selection: selection A has ${#a_ids[@]} tests, selection B ${#b_ids[@]}, disjoint halves of the kernel.object family"

# AND THE NUMBER OF GUESTS IS THE SCHEDULER'S ANSWER, NOT THIS SCRIPT'S.
#
# This started two suites unconditionally. Under `verify.sh --jobs 1` that made two QEMUs run on a
# machine whose one answer to "how many may run" was one - a second scheduler with a width of two,
# which exceeds the declared concurrency limit. The runner declares the budget it is willing
# to hand this step; the gate refuses rather than exceeding it, and a budget that cannot hold the
# overlap is a budget this gate cannot be proved in.
#
# Unset means nobody is scheduling - a person typing `./check.sh --gate concurrent-selection`, which
# is the same exemption `test.sh --arch all` has and for the same reason.
NEEDS_GUESTS=2
if [[ -n "${LIBER_CONCURRENT_GUESTS:-}" ]] && ((LIBER_CONCURRENT_GUESTS < NEEDS_GUESTS)); then
	fail "this gate starts $NEEDS_GUESTS guests at once and the runner allows ${LIBER_CONCURRENT_GUESTS} - it cannot be proved inside that budget"
fi

# THE OVERLAP IS OBSERVED, NOT ASSUMED (added 2026-09-09).
#
# Two suites started at the same moment do not necessarily have their GUESTS running at the same
# moment: the build lock serialises their compiles, so the second guest boots only after the second
# compile, and a first guest that finished by then proves nothing about isolation while both run.
# A watcher counts the machine's `qemu-system-x86_64` processes above the number that were there
# before this gate started - by executable name, which cannot match this script - and records the
# first moment two of this gate's guests are alive together. No such moment is a failure of the gate,
# not a pass with a caveat.
baseline="$(ps -eo comm | grep -c '^qemu-system-x86' || true)"
watch_overlap() {
	local a="$1" b="$2" n
	while kill -0 "$a" 2>/dev/null || kill -0 "$b" 2>/dev/null; do
		n="$(ps -eo comm | grep -c '^qemu-system-x86' || true)"
		if ((n - baseline >= 2)); then
			date +%s >"$work/overlap"
			return 0
		fi
		sleep 0.2
	done
}

echo "concurrent-selection: starting two x86_64 suites at once, differing in BOTH selection and tags"
(cd "$REPO_ROOT" && TEST_SELECTION="$A_SELECTION" ./test.sh --arch x86_64 --tags "$A_TAGS") >"$work/a.log" 2>&1 &
a_pid=$!
(cd "$REPO_ROOT" && TEST_SELECTION="$B_SELECTION" ./test.sh --arch x86_64 --tags "$B_TAGS") >"$work/b.log" 2>&1 &
b_pid=$!
watch_overlap "$a_pid" "$b_pid" &
watch_pid=$!

# BOTH ARE WAITED FOR BEFORE EITHER IS JUDGED, so a failure in the first does not leave the second
# running past the end of this script and into whatever runs next.
wait "$a_pid"
a_status=$?
wait "$b_pid"
b_status=$?
wait "$watch_pid" 2>/dev/null || true

# AND BOTH HAD TO SUCCEED. A collision in this tree does not produce a wrong answer - the medium
# builder recomputes its input key and DIES when a producer replaces an input, so a run that
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

# EACH RESULT NAMES THE EXECUTABLE IT RAN, AND THEY ARE TWO EXECUTABLES.
#
# The staging step publishes the selection-specific kernel read-only and digests it under the build
# lock; the runner prints that digest beside the medium's into the run log; the suite prints it as
# its `STAGED-KERNEL` line. A result is bound to a binary when the three agree, and two selections
# are two binaries - `TEST_SELECTION` is compiled in - so the two digests must differ. A run whose
# guest log names the OTHER run's digest booted the other's kernel however its own tests read.
staged_digest() {
	sed -n 's/^\[test-x86_64\] STAGED-KERNEL sha256=\([0-9a-f]\{64\}\) .*$/\1/p' "$1" | tail -1
}
medium_digest() {
	sed -n 's/^qemu-run: medium sha256=\([0-9a-f]\{64\}\) .*$/\1/p' "$1" | tail -1
}
a_staged="$(staged_digest "$work/a.log")"
b_staged="$(staged_digest "$work/b.log")"
[[ "$a_staged" =~ ^[0-9a-f]{64}$ ]] || fail "the first run did not name the digest of the kernel it staged"
[[ "$b_staged" =~ ^[0-9a-f]{64}$ ]] || fail "the second run did not name the digest of the kernel it staged"
[[ "$a_staged" != "$b_staged" ]] || fail "two different selections staged one digest ($a_staged) - the compile-time selection did not reach the binary"
grep -qF "staged kernel sha256=$a_staged" "$work/a.result" || fail "the first run's own log does not bind its guest to the kernel it staged ($a_staged)"
grep -qF "staged kernel sha256=$b_staged" "$work/b.result" || fail "the second run's own log does not bind its guest to the kernel it staged ($b_staged)"
! grep -qF "staged kernel sha256=$b_staged" "$work/a.result" || fail "the first run's log names the SECOND run's kernel"
! grep -qF "staged kernel sha256=$a_staged" "$work/b.result" || fail "the second run's log names the FIRST run's kernel"
a_medium="$(medium_digest "$work/a.result")"
b_medium="$(medium_digest "$work/b.result")"
[[ "$a_medium" =~ ^[0-9a-f]{64}$ && "$b_medium" =~ ^[0-9a-f]{64}$ ]] || fail "a run did not bind the medium it booted by digest"
[[ "$a_medium" != "$b_medium" ]] || fail "two kernels were carried by one medium ($a_medium) - the medium is not keyed on the staged executable"
grep -q "held as descriptor" "$work/a.result" || fail "the first run's medium was handed to QEMU by name rather than through a held descriptor"

# AND THE GUESTS WERE ALIVE AT THE SAME TIME.
[[ -f "$work/overlap" ]] || fail "the two guests never ran at the same time (baseline $baseline QEMU process(es)), so this gate observed no overlap and proves nothing about isolation under it"

echo "concurrent-selection: both suites passed while overlapping, each on its own medium, its own logs and its own selection"
echo "concurrent-selection:   kernel A sha256=${a_staged:0:16}... on medium ${a_medium:0:16}...; kernel B sha256=${b_staged:0:16}... on medium ${b_medium:0:16}..."
echo "concurrent-selection:   two guests observed alive together (above a baseline of $baseline)"
echo "concurrent-selection:   selection A ($A_TAGS) -> $(basename "${a_logs[0]}")"
echo "concurrent-selection:   selection B ($B_TAGS) -> $(basename "${b_logs[0]}")"
