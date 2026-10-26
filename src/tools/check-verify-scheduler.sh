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

# Without a conservative host/build seed, the whole dependent closure is unpriced. A normal
# unbudgeted run still executes it once so subsequent plans can use the recorded measurement.
plan="$work/unpriced"
trace="$work/unpriced.trace"
{
	printf 'STATUS\tfull\tprepared\n'
	step 0 unknown "unpriced shared prerequisite" "echo shared >> '$trace'"
	printf 'STEPUNPRICED\t0\tno current measurement or conservative seed\n'
	step 1 1 "first priced dependent" "echo first >> '$trace'" "id-0"
	step 2 1 "second priced dependent" "echo second >> '$trace'" "id-0"
	step 3 1 "independent priced branch" "echo independent >> '$trace'"
} >"$plan"
rc=0
run_plan "$plan" --budget 3 || rc=$?
check "unpriced shared prerequisites and both dependents are skipped" 3 "$(grep -c 'SKIPPED (budget)' "$out" || true)"
check "only the complete priced branch runs" independent "$(cat "$trace")"
check "the unpriced branch is incomplete" 6 "$rc"
: >"$trace"
rc=0
run_plan "$plan" || rc=$?
check "unbudgeted execution can measure every branch" 0 "$rc"
check "the shared prerequisite runs once without a budget" "shared first second independent" "$(tr '\n' ' ' <"$trace" | sed 's/ *$//')"
# An all-unpriced plan has no defensible numeric minimum.
sed '/independent priced branch/d; /\t3\t/d' "$plan" >"$work/only-unpriced"
rc=0
run_plan "$work/only-unpriced" --budget 9000 || rc=$?
check "an unknown minimum is stated truthfully" 1 "$(grep -c 'minimum budget is unknown' "$out" || true)"
check "even a large explicit budget cannot invent a measurement" 3 "$(grep -c 'SKIPPED (budget)' "$out" || true)"
# This is the plan protocol after the unbudgeted measurement has been loaded by the model.
sed '/^STEPUNPRICED/d; s/unknown/571/' "$plan" >"$work/measured"
: >"$trace"
rc=0
run_plan "$work/measured" --budget 574 || rc=$?
check "the measured shared branch is admitted with sufficient budget" 0 "$rc"
check "the admitted prerequisite still runs once" "shared first second independent" "$(tr '\n' ' ' <"$trace" | sed 's/ *$//')"

# Exercise the actual fast-command recorder without touching this checkout's history. The planner
# fixture persists the exact duration handed to `record`; Rust history tests cover loading it.
learning="$work/learning"
mkdir -p "$learning/src/tools" "$learning/bin"
cp "$repo/verify.sh" "$repo/lib.sh" "$learning/"
cp "$repo/src/tools/result-logs.sh" "$learning/src/tools/"
# The staging-state helpers lib.sh sources.
cp "$repo/src/tools/staging-state.sh" "$learning/src/tools/"
# The evidence helper verify.sh sources; a no-op here, since no run is collecting.
cp "$repo/src/tools/evidence.sh" "$learning/src/tools/"
cat >"$learning/bin/cargo" <<'RECORDER'
#!/usr/bin/env bash
set -euo pipefail
while (($#)); do
	if [[ "$1" == --seconds ]]; then printf '%s\n' "$2" >"$LEARNED_SECONDS"; exit 0; fi
	shift
done
exit 1
RECORDER
chmod +x "$learning/bin/cargo"
plan="$learning/plan"
{
	printf 'STATUS\tfull\tprepared\n'
	step 0 unknown "the fast unpriced gate" "true"
	printf 'KEY\t0\tgate.fixture host host default\nSTEPUNPRICED\t0\tunmeasured\n'
} >"$plan"
out="$learning/unbudgeted.log"
rc=0
(cd "$learning" && PATH="$learning/bin:$PATH" LEARNED_SECONDS="$learning/seconds" LIBER_VERIFY_STEPS="$plan" ./verify.sh) >"$out" 2>&1 || rc=$?
check "a fast unbudgeted command reaches the production recorder" 0 "$rc"
learned="$(cat "$learning/seconds")"
check "a completed sub-second command has a positive rounded measurement" 1 "$((learned > 0))"
sed "/^STEPUNPRICED/d; s/unknown/$learned/" "$plan" >"$learning/measured-plan"
out="$learning/budgeted.log"
rc=0
(cd "$learning" && PATH="$learning/bin:$PATH" LEARNED_SECONDS="$learning/seconds" LIBER_VERIFY_STEPS="$learning/measured-plan" ./verify.sh --budget "$learned") >"$out" 2>&1 || rc=$?
check "the recorded fast command fits a later budget" 0 "$rc"
check "the later budget starts the measured command" 1 "$(grep -c 'the fast unpriced gate' "$out" || true)"

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

# The normal merge parser supplies two slots without an explicit --jobs. The prepared steps
# exercise the same scheduler while making no claim about a commit or an actual guest verdict.
plan="$work/merge-default"
trace="$work/merge-default.trace"
{
	printf 'STATUS\tfull\tprepared\n'
	for index in 0 1; do
		printf 'STEP\t%s\t0\tport %s\tsh -c "echo start-%s >> %s; sleep 1; echo end-%s >> %s"\t\n' "$index" "$index" "$index" "$trace" "$index" "$trace"
		printf 'STEPID\t%s\tid-%s\nSTEPCOST\t%s\t1\nSTEPGUESTS\t%s\t1\n' "$index" "$index" "$index" "$index"
	done
} >"$plan"
rc=0
run_plan "$plan" --merge "$work/handoff" || rc=$?
check "merge starts both guest steps with its default slots" 0 "$rc"
check "both starts precede either end under the merge default" 2 "$(head -2 "$trace" | grep -c '^start-' || true)"
: >"$trace"
rc=0
run_plan "$plan" --merge "$work/handoff" --jobs 1 || rc=$?
check "an explicit one-slot merge remains serial" "start-0 end-0 start-1 end-1" "$(tr '\n' ' ' <"$trace" | sed 's/ *$//')"
for budget in 0 1; do
	: >"$trace"
	rc=0
	run_plan "$plan" --merge "$work/handoff" --budget "$budget" || rc=$?
	check "merge rejects budget $budget before starting work" 1 "$rc"
	check "the budget refusal names the incompatible mode" 1 "$(grep -c -- '--budget cannot be combined with --merge' "$out" || true)"
	check "budget $budget starts no guest" 0 "$(wc -c <"$trace")"
done

# 8. A RESOURCE BARRIER IS DISTINCT FROM THE GUEST-SLOT BOUND. Earlier ordinary guests
# finish before a shared fixture starts; later ones wait for its cleanup. Fixture failure does
# not become an invented prerequisite of unrelated work. These keyless scheduler fixtures are
# unit evidence only, never an acceptance run or a replacement for the ordinary FULL plan.
plan="$work/exclusive"
trace="$work/exclusive.trace"
{
	printf 'STATUS\tfull\tprepared\n'
	for index in 0 1 2; do
		command="echo start-$index >> '$trace'; sleep 1; echo end-$index >> '$trace'"
		[[ "$index" != 1 ]] || command+="; false"
		step "$index" 1 "isolation step $index" "$command"
		printf 'STEPGUESTS\t%s\t1\n' "$index"
		[[ "$index" != 1 ]] || printf 'STEPEXCLUSIVE\t1\t1\n'
	done
} >"$plan"
rc=0
run_plan "$plan" --jobs 2 || rc=$?
check "the exclusive scenario waits on both sides, including failure cleanup" "start-0 end-0 start-1 end-1 start-2 end-2" "$(tr '\n' ' ' <"$trace" | sed 's/ *$//')"
check "its actual failure is reported" 1 "$rc"
check "an unrelated later guest is not suppressed" 0 "$(grep -c 'BLOCKED:' "$out" || true)"

# An exclusive step can itself require two concurrent guests. Isolation must not rewrite its
# capacity claim or run the gate under an artificial --jobs 1.
plan="$work/exclusive-wide"
{
	printf 'STATUS\tfull\tprepared\n'
	step 0 1 "exclusive overlap gate" "test \"\$LIBER_CONCURRENT_GUESTS\" -ge 2"
	printf 'STEPGUESTS\t0\t2\nSTEPEXCLUSIVE\t0\t1\n'
} >"$plan"
rc=0
run_plan "$plan" --jobs 2 || rc=$?
check "an exclusive overlap gate retains its two-slot allowance" 0 "$rc"
rc=0
run_plan "$plan" --jobs 1 || rc=$?
check "the same required overlap gate remains incomplete with only one slot" 6 "$rc"
check "the explicit slot refusal remains visible" 1 "$(grep -c 'it starts 2 guests at once and --jobs is 1' "$out" || true)"

# A malformed declaration fails before commands start rather than silently enabling overlap.
plan="$work/exclusive-invalid"
{
	printf 'STATUS\tfull\tprepared\n'
	step 0 1 "must not run" "touch '$work/invalid-ran'"
	printf 'STEPGUESTS\t0\t1\nSTEPEXCLUSIVE\t0\tmaybe\n'
} >"$plan"
rc=0
run_plan "$plan" --jobs 2 || rc=$?
check "an invalid isolation declaration is refused" 1 "$rc"
check "invalid metadata starts no step" absent "$([[ -e "$work/invalid-ran" ]] && echo present || echo absent)"

# 7. A REFUSED STEP IS NO OUTCOME. A step that exits `STATE_REFUSED` (`staging-state.sh`) found what it
#    boots in the wrong state and tested nothing: in a release rehearsal it leaves no envelope and the
#    history gets no record of it, where a step that FAILED gets both. A private copy with a `cargo`
#    that records what it was asked, so nothing reaches this checkout's history or a real run.
refusal="$work/refusal"
mkdir -p "$refusal/src/tools/verify-model" "$refusal/bin" "$refusal/run/logs"
cp "$repo/verify.sh" "$repo/lib.sh" "$refusal/"
cp "$repo/src/tools/result-logs.sh" "$repo/src/tools/evidence.sh" "$repo/src/tools/staging-state.sh" "$refusal/src/tools/"
cat >"$refusal/bin/cargo" <<'RECORDER'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$CARGO_CALLS"
echo "fixture: recorded"
RECORDER
chmod +x "$refusal/bin/cargo"
printf '"gate.refusing / host / host / default",\n"gate.failing / host / host / default",\n' >"$refusal/required.toml"
plan="$refusal/plan"
{
	printf 'STATUS\tfull\tprepared\n'
	step 0 1 "the refusing gate" "exit 75"
	printf 'KEY\t0\tgate.refusing / host / host / default\n'
	step 1 1 "the failing gate" "false"
	printf 'KEY\t1\tgate.failing / host / host / default\n'
} >"$plan"
out="$refusal/out.log"
rc=0
(cd "$refusal" && PATH="$refusal/bin:$PATH" CARGO_CALLS="$refusal/calls" LIBER_VERIFY_STEPS="$plan" LIBER_VERIFY_RUN="$refusal/run" LIBER_RELEASE_REQUIRED="$refusal/required.toml" ./verify.sh) >"$out" 2>&1 || rc=$?
touch "$refusal/calls"
check "a refused step is reported as refused" 1 "$(grep -c 'REFUSED: the refusing gate' "$out" || true)"
check "a refused step publishes no envelope" 0 "$(grep -c -- '--key gate.refusing' "$refusal/calls" || true)"
check "a refused step records no history" 0 "$(grep -c -- 'record --step-id id-0' "$refusal/calls" || true)"
check "a failed step still publishes its envelope" 1 "$(grep -c -- '--key gate.failing' "$refusal/calls" || true)"
check "a failed step still records its history" 1 "$(grep -c -- 'record --step-id id-1' "$refusal/calls" || true)"
check "a run with a refused step does not pass" 1 "$((rc != 0))"

# 8. A REFUSAL THAT NAMES ITS FIX IS FIXED AND RUN AGAIN, ONCE. The step hands the runner the command
#    that brings the state about (`LIBER_STATE_REFUSAL`); the runner runs it with nothing else running
#    and runs the step again, and that run is the step's outcome - its envelope and its history, and
#    nothing for the refused run. Refused twice, or with a fix that fails, it is a failure. A guest
#    refused while another guest still runs is fixed only once that one has ended.
state="$refusal/state"
mkdir -p "$state"
refuse_until() {
	# The state file that makes the step run, and the fix that makes it.
	printf 'if [[ -e %q ]]; then echo ran >>%q; else echo %q >>"$LIBER_STATE_REFUSAL"; exit 75; fi' "$1" "$state/runs" "touch $1"
}
plan="$refusal/plan-retry"
{
	printf 'STATUS\tfull\tprepared\n'
	step 0 1 "the step refused once" "$(refuse_until "$state/fixed")"
	printf 'KEY\t0\tgate.refused-once / host / host / default\n'
	step 1 1 "the step refused twice" "echo 'true' >>\"\$LIBER_STATE_REFUSAL\"; exit 75"
	printf 'KEY\t1\tgate.refused-twice / host / host / default\n'
	step 2 1 "the step whose fix fails" "echo 'false' >>\"\$LIBER_STATE_REFUSAL\"; exit 75"
	printf 'KEY\t2\tgate.unfixable / host / host / default\n'
} >"$plan"
printf '"gate.refused-once / host / host / default",\n"gate.refused-twice / host / host / default",\n"gate.unfixable / host / host / default",\n' >"$refusal/required-retry.toml"
: >"$refusal/calls"
rm -rf "$refusal/run" && mkdir -p "$refusal/run/logs"
out="$refusal/retry.log"
rc=0
(cd "$refusal" && PATH="$refusal/bin:$PATH" CARGO_CALLS="$refusal/calls" LIBER_VERIFY_STEPS="$plan" LIBER_VERIFY_RUN="$refusal/run" LIBER_RELEASE_REQUIRED="$refusal/required-retry.toml" ./verify.sh) >"$out" 2>&1 || rc=$?
check "a refusal with a fix brings the state about first" 1 "$(grep -c 'bringing the staging to the state it needs, then running it once more' "$out" | awk '{print ($1 >= 1)}')"
check "the step refused once runs once, after its fix" "ran" "$(cat "$state/runs" 2>/dev/null)"
check "and its second run is its outcome: one passed envelope" 1 "$(grep -- '--key gate.refused-once' "$refusal/calls" | grep -c -- '--outcome passed' || true)"
check "and one passed record" 1 "$(grep -- 'record --step-id id-0' "$refusal/calls" | grep -vc -- '--failed' || true)"
check "a step refused twice publishes and records nothing" 0 "$(grep -c -- '--key gate.refused-twice\|record --step-id id-1' "$refusal/calls" || true)"
check "a step whose fix fails publishes and records nothing" 0 "$(grep -c -- '--key gate.unfixable\|record --step-id id-2' "$refusal/calls" || true)"
check "and both are reported as refused" 2 "$(grep -c 'REFUSED: the step refused twice - it needs\|REFUSED: the step whose fix fails - it needs' "$out" || true)"
check "a run with a step refused for good does not pass" 1 "$((rc != 0))"

plan="$refusal/plan-retry-guests"
rm -f "$state/fixed-guest" "$state/runs" "$state/order"
{
	printf 'STATUS\tfull\tprepared\n'
	printf 'STEP\t0\t0\tthe guest refused while another runs\tif [[ -e %q ]]; then echo retried >>%q; else echo %q >>"$LIBER_STATE_REFUSAL"; exit 75; fi\t\n' "$state/fixed-guest" "$state/order" "touch $state/fixed-guest"
	printf 'STEPID\t0\tid-0\nSTEPCOST\t0\t1\nSTEPGUESTS\t0\t1\n'
	printf 'STEP\t1\t0\tthe guest still running\tsleep 2; echo ended >>%q\t\n' "$state/order"
	printf 'STEPID\t1\tid-1\nSTEPCOST\t1\t1\nSTEPGUESTS\t1\t1\n'
} >"$plan"
rc=0
run_plan "$plan" --jobs 2 || rc=$?
check "a guest refused beside another is fixed and retried after the barrier" "ended retried" "$(tr '\n' ' ' <"$state/order" | sed 's/ *$//')"
check "and the run passes" 0 "$rc"

# 9. OVERLAP IS WHAT THE DECLARATIONS ALLOW, AND THE STAGING IS SWITCHED ONCE PER GROUP. A private copy whose
#    `./build.sh` only records what it was asked and writes the records a real build writes, so the staging's state
#    and every switch are observed: two writers of one staging never overlap, a reader waits for a writer, two
#    readers overlap, the steps that need the development profile run as one group between two switches with the
#    steps that need one volume mode together inside it, and the host lane runs beside a guest.
staging="$work/staging"
mkdir -p "$staging/src/tools" "$staging/bin" "$staging/.build/state" "$staging/.build/boot"
cp "$repo/verify.sh" "$repo/lib.sh" "$staging/"
cp "$repo/src/tools/result-logs.sh" "$repo/src/tools/evidence.sh" "$repo/src/tools/staging-state.sh" "$staging/src/tools/"
cat >"$staging/bin/cargo" <<'RECORDER'
#!/usr/bin/env bash
exit 0
RECORDER
cat >"$staging/build.sh" <<'BUILD'
#!/usr/bin/env bash
# What a build would leave: the profile of the parts it built, and the bootable volume's mode.
set -euo pipefail
profile=shipping
[[ "${LIBER_DEVELOPMENT:-0}" == 1 ]] && profile=development
args=" $* "
echo "switch $profile${args//  / }" >>"$STAGING_TRACE"
if [[ "$args" == *" --kernel-on-volume "* ]]; then
	mode="${args#* --dma-mode }"
	mode="${mode%% *}"
	echo "$profile" >.build/state/built-x86_64-volume.profile
	echo "$mode" >.build/boot/system-volume-bootable-x86_64.dma-mode
else
	for part in libs user packages volume-test; do echo "$profile" >".build/state/built-x86_64-$part.profile"; done
fi
BUILD
chmod +x "$staging/bin/cargo" "$staging/build.sh"
shipped() {
	local part
	: >"$staging/.build/boot/system-volume-bootable-x86_64.img"
	for part in libs user packages volume-test volume; do echo shipping >"$staging/.build/state/built-x86_64-$part.profile"; done
	echo enforcing-required >"$staging/.build/boot/system-volume-bootable-x86_64.dma-mode"
	: >"$staging/trace"
}
guest_step() {
	# index label uses [cost]
	printf 'STEP\t%s\t0\t%s\tsh -c "echo start-%s >> %s; sleep 1; echo end-%s >> %s"\t\n' "$1" "$2" "$2" "$staging/trace" "$2" "$staging/trace"
	printf 'STEPID\t%s\tid-%s\nSTEPCOST\t%s\t%s\nSTEPGUESTS\t%s\t1\nSTEPUSES\t%s\t%s\n' "$1" "$1" "$1" "${4:-1}" "$1" "$1" "$3"
}
run_staging() {
	out="$staging/out.$RANDOM"
	local rc=0
	(cd "$staging" && PATH="$staging/bin:$PATH" STAGING_TRACE="$staging/trace" LIBER_VERIFY_STEPS="$1" ./verify.sh --jobs "$2") >"$out" 2>&1 || rc=$?
	return "$rc"
}
trace_of() { tr '\n' ' ' <"$staging/trace" | sed 's/ *$//'; }

shipped
plan="$staging/writers"
{
	printf 'STATUS\tfull\tprepared\n'
	guest_step 0 w1 "stage:x86_64=w:shipping"
	guest_step 1 w2 "stage:x86_64=w:shipping"
	guest_step 2 r1 "stage:x86_64=r:shipping media=r:-"
	guest_step 3 r2 "stage:x86_64=r:shipping media=r:-"
} >"$plan"
rc=0
run_staging "$plan" 4 || rc=$?
check "the declared plan runs" 0 "$rc"
check "two writers of one staging never overlap, and a reader waits for a writer" "start-w1 end-w1 start-w2 end-w2" "$(trace_of | cut -d' ' -f1-4)"
check "two readers of one state overlap" "start-r1 start-r2" "$(trace_of | cut -d' ' -f5-6 | tr ' ' '\n' | sort | tr '\n' ' ' | sed 's/ *$//')"
check "and nothing was switched" 0 "$(grep -c '^switch' "$staging/trace" || true)"

shipped
plan="$staging/groups"
{
	printf 'STATUS\tfull\tprepared\n'
	guest_step 0 ship1 "stage:x86_64=r:shipping media=r:-"
	guest_step 1 ship2 "stage:x86_64=r:shipping media=r:-"
	guest_step 2 enf1 "stage:x86_64=r:development media=r:- volume:x86_64=r:development/enforcing-required"
	guest_step 3 enf2 "stage:x86_64=r:development media=r:- volume:x86_64=r:development/enforcing-required"
	guest_step 4 har1 "stage:x86_64=r:development media=r:- volume:x86_64=r:development/harness"
	guest_step 5 har2 "stage:x86_64=r:development media=r:- volume:x86_64=r:development/harness"
	guest_step 6 aside "stage:aarch64=r:shipping media=r:-"
} >"$plan"
for part in libs user packages volume-test; do echo shipping >"$staging/.build/state/built-aarch64-$part.profile"; done
rc=0
run_staging "$plan" 4 || rc=$?
check "the grouped plan runs" 0 "$rc"
order="$(trace_of)"
check "the development profile is switched to once, after the shipping group has ended" 1 "$(grep -c '^switch development --arch x86_64 $' "$staging/trace" || true)"
check "and each volume mode once" "1 1" "$(grep -c '^switch development .*--dma-mode enforcing-required' "$staging/trace" || true) $(grep -c '^switch development .*--dma-mode harness' "$staging/trace" || true)"
check "the shipping group ends before the switch" yes "$(awk '/^end-ship/ {ended=NR} /^switch development --arch x86_64 *$/ {to=NR} END {print ((ended && to > ended) ? "yes" : "no")}' "$staging/trace")"
check "one volume mode's steps run together, and the next mode is switched to after them" yes "$(awk '/^end-enf/ {ended=NR} /dma-mode harness/ {to=NR} /^start-har/ && !first {first=NR} END {print ((ended && to > ended && first > to) ? "yes" : "no")}' "$staging/trace")"
check "the steps of one state overlap" yes "$(awk '/^start-enf/ {starts++} /^end-enf/ && !ended {ended=NR; print ((starts == 2) ? "yes" : "no")}' "$staging/trace")"
check "another architecture's staging is not held back by this one's switches" yes "$(awk '/^start-aside/ {aside=NR} /^switch development --arch x86_64 *$/ {to=NR} END {print ((aside && aside < to) ? "yes" : "no")}' "$staging/trace")"
check "the run puts the staging back once, at its end" "shipping shipping enforcing-required 1 1" "$(cat "$staging/.build/state/built-x86_64-libs.profile" "$staging/.build/state/built-x86_64-volume.profile" "$staging/.build/boot/system-volume-bootable-x86_64.dma-mode" | tr '\n' ' ')$(grep -c '^switch shipping --arch x86_64 *$' "$staging/trace" || true) $(grep -c '^switch shipping .*--dma-mode enforcing-required' "$staging/trace" || true)"
[[ "$order" == *start-ship* ]] || failed=1

# THE HOST LANE RUNS BESIDE THE GUESTS: a host step and a guest step start before either ends, even at one guest slot.
shipped
plan="$staging/lanes"
{
	printf 'STATUS\tfull\tprepared\n'
	guest_step 0 guest "stage:x86_64=r:shipping media=r:-"
	printf 'STEP\t1\t0\thost\tsh -c "echo start-host >> %s; sleep 1; echo end-host >> %s"\t\n' "$staging/trace" "$staging/trace"
	printf 'STEPID\t1\tid-1\nSTEPCOST\t1\t1\nSTEPGUESTS\t1\t0\nSTEPUSES\t1\t\n'
} >"$plan"
rc=0
run_staging "$plan" 1 || rc=$?
check "a host step and a guest step run at once" "start-guest start-host" "$(trace_of | cut -d' ' -f1-2 | tr ' ' '\n' | sort | tr '\n' ' ' | sed 's/ *$//')"

# 10. A RELEASE WRITES ITS DOSSIER EVEN WHEN A STEP WAS SKIPPED. At `--jobs 1` the two-guest gate is not started, and
#     the run used to end INCOMPLETE with no dossier at all - the dossier is what names the keys it lacks. A private
#     in-place release whose `cargo` answers `run-start` and `dossier` and records what it is asked.
release="$work/release"
mkdir -p "$release/src/tools/verify-model" "$release/bin"
cp "$repo/verify.sh" "$repo/lib.sh" "$release/"
cp "$repo/src/tools/result-logs.sh" "$repo/src/tools/evidence.sh" "$repo/src/tools/staging-state.sh" "$release/src/tools/"
: >"$release/src/tools/verify-model/Cargo.toml"
cat >"$release/bin/cargo" <<'RECORDER'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$CARGO_CALLS"
case " $* " in
*" run-start "*)
	mkdir -p "$RELEASE_RUN/logs"
	echo "$RELEASE_RUN"
	;;
*" dossier "*) printf '# Release dossier - run fixture (INCOMPLETE)\n' >"$RELEASE_RUN/dossier.md" ;;
esac
exit 0
RECORDER
chmod +x "$release/bin/cargo"
plan="$release/plan"
{
	printf 'STATUS\tfull\tprepared\n'
	printf 'STEP\t0\t0\tthe two-guest gate\ttrue\t\n'
	printf 'STEPID\t0\tid-0\nSTEPCOST\t0\t1\nSTEPGUESTS\t0\t2\n'
} >"$plan"
out="$release/out.log"
rc=0
(cd "$release" && PATH="$release/bin:$PATH" CARGO_CALLS="$release/calls" RELEASE_RUN="$release/run" LIBER_VERIFY_STEPS="$plan" ./verify.sh --release --in-place --jobs 1) >"$out" 2>&1 || rc=$?
touch "$release/calls"
check "a release with a step skipped for the slots is INCOMPLETE" 6 "$rc"
check "and its dossier is collected all the same" 1 "$(grep -c ' dossier ' "$release/calls" || true)"
check "and said" 1 "$(grep -c 'the release run is INCOMPLETE' "$out" || true)"

# 11. AN L2 CONTINUES WHAT AN IDENTICAL ONE LEFT: a step all of whose keys the earlier run passed is not started again,
#     and the rest runs. The model's `continue-run` (stood in for here) decides what an identical earlier run covers.
cat >"$release/bin/cargo" <<'RECORDER'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$CARGO_CALLS"
case " $* " in
*" run-start "*)
	mkdir -p "$RELEASE_RUN/logs"
	echo "$RELEASE_RUN"
	;;
*" continue-run "*) printf 'COVERED\t0\tthe-earlier-run\n' ;;
*" dossier "*) printf '# Release dossier - run fixture (rehearsal (in place))\n' >"$RELEASE_RUN/dossier.md" ;;
esac
exit 0
RECORDER
plan="$release/plan-continued"
{
	printf 'STATUS\tfull\tprepared\n'
	printf 'STEP\t0\t1\tthe step the earlier run passed\ttouch %q\t\n' "$release/ran-0"
	printf 'STEPID\t0\tid-0\nSTEPCOST\t0\t1\nSTEPGUESTS\t0\t0\nKEY\t0\tgate.covered / host / host / default\n'
	printf 'STEP\t1\t1\tthe step it did not\ttouch %q\t\n' "$release/ran-1"
	printf 'STEPID\t1\tid-1\nSTEPCOST\t1\t1\nSTEPGUESTS\t1\t0\nKEY\t1\tgate.uncovered / host / host / default\n'
} >"$plan"
rm -rf "$release/run"
: >"$release/calls"
out="$release/continued.log"
rc=0
(cd "$release" && PATH="$release/bin:$PATH" CARGO_CALLS="$release/calls" RELEASE_RUN="$release/run" LIBER_VERIFY_STEPS="$plan" ./verify.sh --release --in-place) >"$out" 2>&1 || rc=$?
check "an in-place release over the whole set is an L2, and is marked so" L2 "$(cat "$release/run/level" 2>/dev/null)"
check "the step the earlier identical run passed is not started again" "absent present" "$([[ -e "$release/ran-0" ]] && echo present || echo absent) $([[ -e "$release/ran-1" ]] && echo present || echo absent)"
check "and the run says where its envelope came from" 1 "$(grep -c 'CONTINUED: the step the earlier run passed - passed in run the-earlier-run' "$out" || true)"
check "and finishes" 0 "$rc"

if ((failed != 0)); then
	echo "verify-scheduler: the shell scheduler did not behave as the milestone requires" >&2
	exit 1
fi
echo "verify-scheduler: failed-descendant suppression, shared prerequisites, FAIL over INCOMPLETE, seeded costs and the summed guest-slot bound all hold"
