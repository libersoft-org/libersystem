#!/usr/bin/env bash
# THE SHELL SCHEDULER, DRIVEN OVER PLANS WRITTEN FOR IT.
#
# `verify.sh` decides five things the model cannot: which step is blocked by a prerequisite that
# failed, which is blocked by one that was itself blocked, what an unmeasured cost is worth to a
# budget, whether a run that both failed and skipped reports FAIL or INCOMPLETE, and how many guests
# may be in flight at once. Until this gate existed none of them was reachable by a test - the only
# way to reach the executor was to run a real plan over a real tree - so its one defect class was an
# ordering one found by reading, three times in four days.
#
# The plans below are the planner's own format with no `KEY` lines, which is what keeps this gate off
# the verification history: `record_one_step` files nothing for a step with no keys. Every command is
# `true` or `false`, so the whole matrix costs milliseconds and boots nothing.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$root/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
failed=0

# Run the executor over a prepared plan and answer with its exit code, leaving its output in $out.
out=""
run_plan() {
	local plan="$1"
	shift
	out="$work/out.$RANDOM"
	local rc=0
	(cd "$repo" && LIBER_VERIFY_STEPS="$plan" ./verify.sh "$@") >"$out" 2>&1 || rc=$?
	return "$rc"
}

check() {
	local what="$1" expected="$2" actual="$3"
	if [[ "$expected" == "$actual" ]]; then
		echo "verify-scheduler:   $what"
		return 0
	fi
	echo "verify-scheduler: $what - expected '$expected', got '$actual'" >&2
	sed -n '1,40p' "$out" >&2
	failed=1
}

step() {
	# index cost label command [requires...]
	local index="$1" cost="$2" label="$3" command="$4"
	shift 4
	printf 'STEP\t%s\t0\t%s\t%s\t\n' "$index" "$label" "$command"
	printf 'STEPID\t%s\tid-%s\n' "$index" "$index"
	printf 'STEPCOST\t%s\t%s\n' "$index" "$cost"
	printf 'STEPGUESTS\t%s\t0\n' "$index"
	local req
	for req in "$@"; do printf 'STEPREQ\t%s\t%s\n' "$index" "$req"; done
}

# 1. A FAILED PREREQUISITE BLOCKS ITS DEPENDENT, AND A BLOCKED ONE BLOCKS ITS OWN.
#
# The second half is the correction of 2026-09-01: suppression that stops one level down lets a
# grandchild run against an output nothing produced.
plan="$work/descendant"
{
	printf 'STATUS\tfull\tprepared\n'
	step 0 1 "the one that fails" "false"
	step 1 1 "its dependent" "true" "id-0"
	step 2 1 "its grandchild" "true" "id-1"
	step 3 1 "an unrelated step" "true"
} >"$plan"
rc=0
run_plan "$plan" || rc=$?
check "a failed step is reported" 1 "$(grep -c 'FAILED: the one that fails' "$out" || true)"
check "its dependent is blocked" 1 "$(grep -c 'BLOCKED: its dependent' "$out" || true)"
check "its GRANDCHILD is blocked too" 1 "$(grep -c 'BLOCKED: its grandchild' "$out" || true)"
check "an unrelated step still runs" 1 "$(grep -c 'an unrelated step' "$out" || true)"
check "the run fails" 1 "$rc"

# 2. A PREREQUISITE SHARED BY TWO BRANCHES BLOCKS BOTH, AND IS RUN ONCE.
plan="$work/shared"
{
	printf 'STATUS\tfull\tprepared\n'
	step 0 1 "the shared prerequisite" "false"
	step 1 1 "the first branch" "true" "id-0"
	step 2 1 "the second branch" "true" "id-0"
} >"$plan"
rc=0
run_plan "$plan" || rc=$?
check "the shared prerequisite runs once" 1 "$(grep -c 'FAILED: the shared prerequisite' "$out" || true)"
check "the first branch is blocked" 1 "$(grep -c 'BLOCKED: the first branch' "$out" || true)"
check "and so is the second" 1 "$(grep -c 'BLOCKED: the second branch' "$out" || true)"

# 3. `FAIL` OUTRANKS `INCOMPLETE`. A run that skipped work for a budget AND found a defect is a
#    failure: reporting INCOMPLETE would let a defect be read as an unfinished run.
plan="$work/precedence"
{
	printf 'STATUS\tfull\tprepared\n'
	step 0 1 "the cheap one that fails" "false"
	step 1 9000 "the expensive one" "true"
} >"$plan"
rc=0
run_plan "$plan" --budget 5 || rc=$?
check "the expensive step is skipped for the budget" 1 "$(grep -c 'SKIPPED (budget): the expensive one' "$out" || true)"
check "and the run reports FAIL, not INCOMPLETE" 0 "$(grep -c 'INCOMPLETE' "$out" || true)"
check "with a non-zero exit" 1 "$rc"

# 4. AN UNMEASURED COST IS NOT A FREE ONE, AND THE RUNNER CHARGES THE SEED IT IS GIVEN.
#
#    THIS CASE USED TO ENCODE THE DEFECT IT IS NAMED AFTER (corrected 2026-09-02). It supplied a
#    step priced at `STEPCOST 0` and asserted that a five-second budget ran it - which is the
#    "unknown priced at zero is the cheapest thing in every plan" that M4 forbids, written down as
#    an expectation. The runner cannot tell an unmeasured step from a genuinely instant one; what it
#    owes is to charge whatever the plan says and to refuse what will not fit. So the seeded step is
#    priced ABOVE the budget here and must be skipped, and the planner's half - never emitting a
#    zero for a step nobody has timed - is asserted where it lives, in `verify-model`'s own
#    `an_unmeasured_step_is_never_priced_at_zero`.
plan="$work/unmeasured"
{
	printf 'STATUS\tfull\tprepared\n'
	step 0 20 "the seeded step" "true"
	step 1 9000 "the expensive one" "true"
} >"$plan"
rc=0
run_plan "$plan" --budget 5 || rc=$?
check "a step seeded above the budget is skipped, not treated as free" 1 "$(grep -c 'SKIPPED (budget): the seeded step' "$out" || true)"
check "the expensive one is not started either" 1 "$(grep -c 'SKIPPED (budget): the expensive one' "$out" || true)"
check "and a run that only skipped is INCOMPLETE" 1 "$(grep -c 'INCOMPLETE' "$out" || true)"
rc=0
run_plan "$plan" --budget 30 || rc=$?
check "and it runs once the budget covers its seed" 1 "$(grep -c 'the seeded step' "$out" || true)"

# 5. A STEP THAT WANTS MORE GUEST SLOTS THAN `--jobs` HAS IS REFUSED RATHER THAN TRIMMED.
#    A gate whose subject is overlap and which runs one guest proves nothing and would report a pass.
plan="$work/slots"
{
	printf 'STATUS\tfull\tprepared\n'
	printf 'STEP\t0\t0\ttwo guests at once\ttrue\t\n'
	printf 'STEPID\t0\tid-0\n'
	printf 'STEPCOST\t0\t1\n'
	printf 'STEPGUESTS\t0\t2\n'
} >"$plan"
rc=0
run_plan "$plan" --jobs 1 || rc=$?
check "a step needing two slots is refused under --jobs 1" 1 "$(grep -c 'it starts 2 guests at once and --jobs is 1' "$out" || true)"
rc=0
run_plan "$plan" --jobs 2 || rc=$?
check "and runs under --jobs 2" 1 "$(grep -c 'two guests at once' "$out" || true)"

# 6. GUEST WORK IS WHAT THE PLAN DECLARES, and a non-guest step waits for it before deciding whether
#    its prerequisite failed. This is the 2026-09-02 classifier change and the 2026-09-01 barrier
#    order together: under `--jobs 2` the failing guest is backgrounded, and the gate that reads it
#    must still be blocked.
plan="$work/parallel"
{
	printf 'STATUS\tfull\tprepared\n'
	printf 'STEP\t0\t0\tthe guest that fails\tfalse\t\n'
	printf 'STEPID\t0\tid-0\n'
	printf 'STEPCOST\t0\t1\n'
	printf 'STEPGUESTS\t0\t1\n'
	printf 'STEP\t1\t0\tanother guest\ttrue\t\n'
	printf 'STEPID\t1\tid-1\n'
	printf 'STEPCOST\t1\t1\n'
	printf 'STEPGUESTS\t1\t1\n'
	step 2 1 "the gate that reads it" "true" "id-0"
} >"$plan"
rc=0
run_plan "$plan" --jobs 2 || rc=$?
check "the backgrounded guest's failure is recorded" 1 "$(grep -c 'FAILED: the guest that fails' "$out" || true)"
check "and its dependent is blocked despite running in parallel" 1 "$(grep -c 'BLOCKED: the gate that reads it' "$out" || true)"

# 7. THE SLOTS IN FLIGHT ARE SUMMED, NOT COUNTED.
#
#    Case 5 runs the two-slot step ALONE, so it cannot see this: the capacity loop counted background
#    PROCESSES, one array entry per step, which equals the number of guests only while every step
#    wants one. Under `--jobs 2` the two-guest step held a single entry, the next one-guest step read
#    the count as 1 and started beside it - three guests under a bound of two, from the runner that
#    exists to be the only answer to that question.
#
#    Overlap is observed rather than inferred: the wide step brackets a sleep with two lines in a
#    shared file, and the narrow step writes one. Interleaving is the defect and ordering is the fix.
plan="$work/oversubscribe"
trace="$work/oversubscribe.trace"
: >"$trace"
{
	printf 'STATUS\tfull\tprepared\n'
	printf 'STEP\t0\t0\tthe wide step\tsh -c "echo wide-start >> %s; sleep 2; echo wide-end >> %s"\t\n' "$trace" "$trace"
	printf 'STEPID\t0\tid-0\n'
	printf 'STEPCOST\t0\t1\n'
	printf 'STEPGUESTS\t0\t2\n'
	printf 'STEP\t1\t0\tthe narrow step\tsh -c "echo narrow >> %s"\t\n' "$trace"
	printf 'STEPID\t1\tid-1\n'
	printf 'STEPCOST\t1\t1\n'
	printf 'STEPGUESTS\t1\t1\n'
} >"$plan"
rc=0
run_plan "$plan" --jobs 2 || rc=$?
check "both steps ran" 0 "$rc"
check "the one-slot step waited for the two-slot step instead of joining it" "wide-start wide-end narrow" "$(tr '\n' ' ' <"$trace" | sed 's/ *$//')"

if ((failed != 0)); then
	echo "verify-scheduler: the shell scheduler did not behave as the milestone requires" >&2
	exit 1
fi
echo "verify-scheduler: failed-descendant suppression, shared prerequisites, FAIL over INCOMPLETE, seeded costs and the summed guest-slot bound all hold"
