#!/usr/bin/env bash
# EVERY ENTRY POINT ENDS WITH A VERDICT, AND THIS PROVES IT BY MAKING THEM FAIL.
#
# The verdict exists because a failed run and a running run were indistinguishable: both produced a
# log that had stopped growing, and a build that had failed in nine seconds was waited on for thirty
# minutes. A gate that only ran the scripts SUCCESSFULLY would prove nothing about that - the whole
# defect lives on the failure paths, which is where the shapes differ.
#
# SO EVERY CASE BELOW MAKES SOMETHING GO WRONG IN A DIFFERENT WAY and requires the same two things
# out of it: exactly one RESULT line from the invocation that was asked for, and a terminal file
# whose four fields say the same as that line. A bad flag is the script's own refusal; a signal is
# the case where no code of ours runs at all; a stalled step is the case where nothing is wrong yet
# and the run has to say so while it is still stalled.
#
# AND THE ASSERTIONS ARE THEMSELVES TESTED. A gate whose checker cannot fail is a gate that approves
# anything, so the helper is driven over a missing line, a duplicate line, another script's line and
# mismatched fields, and each of those has to be refused.

set -euo pipefail
SCRIPT_NAME=check-run-verdict.sh
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

failed=0
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

note() {
	echo "check-run-verdict: $*" >&2
}

# QUIET WHILE THE CHECKER IS CHECKING ITSELF. The self-test drives the assertion over logs it must
# REFUSE, and a refusal that set the gate's own failure flag would make the self-test fail the gate.
assert_quiet=0
complain() {
	if ((assert_quiet)); then
		return 0
	fi
	note "$*"
	failed=1
}

# A FRESH DESTINATION PER RUN, which is the contract this file is checking: the reader may only
# trust a record it allocated a new path for.
fresh_status() {
	local dir
	dir="$(mktemp -d "$scratch/status.XXXXXX")"
	printf '%s/run.status' "$dir"
}

# The one assertion: exactly one verdict line from `$script`, with the expected outcome and exit
# code, a nonnegative integer elapsed time, and a status file carrying the same four values.
#
# `expect_one_verdict <label> <script> <outcome> <exit> <log> <status-file> <actual-exit>`
expect_one_verdict() {
	local what="$1" script="$2" outcome="$3" want_exit="$4" log="$5" status_file="$6" actual="$7"
	local lines count line
	lines="$(grep -E "^$script: RESULT (ok|failed) exit=[0-9]+ seconds=[0-9]+\$" "$log" 2>/dev/null || true)"
	count="$(printf '%s' "$lines" | grep -c . || true)"
	if [[ "$count" != 1 ]]; then
		complain "$what: expected exactly one $script verdict line, found $count"
		return 1
	fi
	line="$lines"
	if [[ "$line" != "$script: RESULT $outcome exit=$want_exit "* ]]; then
		complain "$what: expected '$script: RESULT $outcome exit=$want_exit', got '$line'"
		return 1
	fi
	local seconds="${line##*seconds=}"
	case "$seconds" in
	"" | *[!0-9]*)
		complain "$what: elapsed time is not a nonnegative integer in '$line'"
		return 1
		;;
	esac
	if [[ -n "$actual" && "$actual" != "$want_exit" ]]; then
		complain "$what: the process exited $actual and the verdict said $want_exit"
		return 1
	fi
	if [[ -n "$status_file" ]]; then
		if [[ ! -f "$status_file" ]]; then
			complain "$what: no terminal record at $status_file"
			return 1
		fi
		local want
		want="$(printf 'script=%s\noutcome=%s\nexit=%s\nseconds=%s\n' "$script" "$outcome" "$want_exit" "$seconds")"
		if [[ "$(cat "$status_file")" != "$want" ]]; then
			complain "$what: the record and the line disagree; the record is $(tr '\n' ' ' <"$status_file")"
			return 1
		fi
	fi
	note "$what -> ${line#*: }"
	return 0
}

# Run one entry point with a fresh destination and assert its verdict.
expect_run() {
	local what="$1" script="$2" outcome="$3" want_exit="$4"
	shift 4
	local log status_file actual=0
	log="$scratch/${what// /_}.log"
	status_file="$(fresh_status)"
	set +e
	RUN_STATUS_FILE="$status_file" "$@" >/dev/null 2>"$log"
	actual=$?
	set -e
	expect_one_verdict "$what" "$script" "$outcome" "$want_exit" "$log" "$status_file" "$actual" || true
}

# ---------------------------------------------------------------- the checker checks itself
#
# Each of these is a log that must NOT be accepted. A gate that approves them approves anything.
self_test() {
	local log="$scratch/self.log" status_file
	status_file="$(fresh_status)"
	printf 'script=gen.sh\noutcome=ok\nexit=0\nseconds=0\n' >"$status_file"

	: >"$log"
	expect_one_verdict "self: no line" gen.sh ok 0 "$log" "" "" >/dev/null 2>&1 && return 1

	printf 'gen.sh: RESULT ok exit=0 seconds=0\ngen.sh: RESULT ok exit=0 seconds=0\n' >"$log"
	expect_one_verdict "self: duplicate lines" gen.sh ok 0 "$log" "" "" >/dev/null 2>&1 && return 1

	printf 'build.sh: RESULT ok exit=0 seconds=0\n' >"$log"
	expect_one_verdict "self: another script" gen.sh ok 0 "$log" "" "" >/dev/null 2>&1 && return 1

	printf 'gen.sh: RESULT failed exit=1 seconds=0\n' >"$log"
	expect_one_verdict "self: wrong outcome" gen.sh ok 0 "$log" "" "" >/dev/null 2>&1 && return 1

	printf 'gen.sh: RESULT ok exit=0 seconds=0\n' >"$log"
	expect_one_verdict "self: mismatched record" gen.sh ok 0 "$log" "$status_file" "" >/dev/null 2>&1
	local kept=$?
	printf 'gen.sh: RESULT ok exit=0 seconds=4\n' >"$log"
	expect_one_verdict "self: mismatched seconds" gen.sh ok 0 "$log" "$status_file" "" >/dev/null 2>&1 && return 1

	printf 'gen.sh: RESULT ok exit=0 seconds=0\n' >"$log"
	expect_one_verdict "self: process exited otherwise" gen.sh ok 0 "$log" "" 1 >/dev/null 2>&1 && return 1

	[[ "$kept" == 0 ]] || return 1
	return 0
}
assert_quiet=1
self_test_result=0
self_test || self_test_result=1
assert_quiet=0
if ((self_test_result == 0)); then
	note "the assertion refuses a missing, duplicated, foreign, mismatched and contradicted verdict"
else
	complain "the assertion accepted a verdict it must refuse - every result below is worthless"
fi

# ---------------------------------------------------------------- the five entry points
#
# A refusal from each script's own argument handling, which exits long before any work, and a
# successful path that does no work either. Both must carry the same shape.
expect_run "build.sh refuses a part" build.sh failed 1 ./build.sh --part nosuchpart
expect_run "test.sh refuses an architecture" test.sh failed 1 ./test.sh --arch nosucharch
expect_run "check.sh refuses a gate" check.sh failed 1 ./check.sh --gate nosuchgate
expect_run "gen.sh refuses a flag" gen.sh failed 1 ./gen.sh --nosuchflag
expect_run "verify.sh refuses a flag" verify.sh failed 1 ./verify.sh --nosuchflag

expect_run "build.sh lists its parts" build.sh ok 0 ./build.sh --help
expect_run "test.sh lists its tags" test.sh ok 0 ./test.sh --help
expect_run "check.sh lists its gates" check.sh ok 0 ./check.sh --list
expect_run "gen.sh lists its packages" gen.sh ok 0 ./gen.sh --list
expect_run "verify.sh answers a model query" verify.sh ok 0 ./verify.sh --model-hash

# THE FOUR MODEL QUERIES USED TO `exec`, which replaces the shell and takes its EXIT trap with it -
# so each of them answered with no verdict at all. One is enough to hold the change; the others run
# the same code path.
expect_run "verify.sh answers --trust" verify.sh ok 0 ./verify.sh --trust

# ---------------------------------------------------------------- signals
#
# No code of ours runs on the way in: the trap is all there is, and bash runs an EXIT trap with `$?`
# still zero after a signal - which is how a shot-down run claimed success.
for signal in HUP INT QUIT TERM; do
	number="$(kill -l "$signal")"
	cat >"$scratch/signalled-$signal.sh" <<'INNER'
SCRIPT_NAME=signalled.sh
source "$1/lib.sh"
arm_run_verdict
kill -"$2" $$
sleep 30
INNER
	expect_run "a $signal to the shell" signalled.sh failed "$((128 + number))" bash "$scratch/signalled-$signal.sh" "$ROOT" "$signal"
done

# AND A SIGNAL WHILE A CHILD IS RUNNING, which is the case a foreground child used to defer until it
# finished. The child must be stopped and reaped, not left behind.
cat >"$scratch/child.sh" <<'INNER'
SCRIPT_NAME=child.sh
source "$1/lib.sh"
arm_run_verdict
echo started >"$2"
run_owned sleep 45
INNER
marker="$scratch/child.started"
child_log="$scratch/child.log"
child_status="$(fresh_status)"
rm -f "$marker"
RUN_STATUS_FILE="$child_status" bash "$scratch/child.sh" "$ROOT" "$marker" >/dev/null 2>"$child_log" &
child_shell=$!
for _ in $(seq 1 100); do
	[[ -f "$marker" ]] && break
	sleep 0.1
done
sleep 0.3
kill -TERM "$child_shell" 2>/dev/null || true
child_exit=0
wait "$child_shell" || child_exit=$?
expect_one_verdict "a TERM while a child runs" child.sh failed 143 "$child_log" "$child_status" "$child_exit" || true
if pgrep -P "$child_shell" >/dev/null 2>&1; then
	complain "the interrupted run left a child of $child_shell behind"
else
	note "the interrupted run left no child behind"
fi

# ---------------------------------------------------------------- nested ownership
#
# A child announces ITSELF and never its parent: it keeps its own verdict line and has no terminal
# destination unless its caller gives it a different fresh one.
cat >"$scratch/parent.sh" <<'INNER'
SCRIPT_NAME=parent.sh
source "$1/lib.sh"
arm_run_verdict
"$1/gen.sh" --list >/dev/null
exit 7
INNER
parent_log="$scratch/parent.log"
parent_status="$(fresh_status)"
set +e
RUN_STATUS_FILE="$parent_status" bash "$scratch/parent.sh" "$ROOT" >/dev/null 2>"$parent_log"
parent_exit=$?
set -e
expect_one_verdict "a nested run announces itself" parent.sh failed 7 "$parent_log" "$parent_status" "$parent_exit" || true
if grep -qE '^gen\.sh: RESULT ok' "$parent_log"; then
	note "the child kept its own verdict line"
else
	complain "the child produced no verdict line of its own"
fi

# ---------------------------------------------------------------- the terminal record's lifecycle
#
# A destination that already exists is NOT this run's. Taking it would destroy somebody else's
# record and would make a stale success look like an answer.
taken="$scratch/taken.status"
printf 'script=somebody-else.sh\noutcome=ok\nexit=0\nseconds=1\n' >"$taken"
before="$(cat "$taken")"
set +e
RUN_STATUS_FILE="$taken" ./gen.sh --list >/dev/null 2>"$scratch/taken.log"
taken_exit=$?
set -e
if [[ "$taken_exit" != 0 ]]; then
	complain "a refused destination changed the run's own result (exit $taken_exit)"
elif [[ "$(cat "$taken")" != "$before" ]]; then
	complain "a pre-existing destination was overwritten"
elif ! grep -q "already exists" "$scratch/taken.log"; then
	complain "a pre-existing destination was not diagnosed"
else
	note "a pre-existing destination is diagnosed, left alone, and does not change the result"
fi

# A RELATIVE DESTINATION IS THE INVOCATION'S OWN DIRECTORY, not wherever the run later moves to.
relative_root="$(mktemp -d "$scratch/relative.XXXXXX")"
(cd "$relative_root" && RUN_STATUS_FILE="here/run.status" "$ROOT/gen.sh" --list >/dev/null 2>"$relative_root/log")
if [[ -f "$relative_root/here/run.status" ]]; then
	note "a relative destination resolves against the directory the run started in"
else
	complain "a relative destination did not land beside the invocation"
fi

# A DESTINATION THAT CANNOT BE PUBLISHED DOES NOT CHANGE THE RESULT.
set +e
RUN_STATUS_FILE="/proc/self/mem/run.status" ./gen.sh --list >/dev/null 2>"$scratch/unwritable.log"
unwritable_exit=$?
set -e
if [[ "$unwritable_exit" != 0 ]]; then
	complain "an unusable destination changed the run's own result (exit $unwritable_exit)"
elif ! grep -qE "no terminal record|could not be published" "$scratch/unwritable.log"; then
	complain "an unusable destination was not diagnosed"
else
	note "an unusable destination is diagnosed and the work keeps its own result"
fi

# NOTHING PARTIAL IS LEFT AT OR BESIDE THE DESTINATION.
# `-print -quit` RATHER THAN A PIPE INTO `head`: under `pipefail` a reader that stops early makes
# the producer's SIGPIPE the pipeline's status, so finding something would read as a failure.
leftovers="$(find "$scratch" -name '.run-verdict.*' -print -quit 2>/dev/null)"
if [[ -n "$leftovers" ]]; then
	complain "a temporary record was left behind: $leftovers"
else
	note "no partial record was left behind"
fi

# ---------------------------------------------------------------- a private tree, for the failures
#                                                                    that must not touch this one
#
# THE HEAVY CASES NEED A REAL FAILURE FROM A SUBORDINATE TOOL, and making one happen in the live tree
# would damage its sources, images, stamps or verification history. So the entry scripts and their
# shared helper are COPIED - unchanged, because their reporting and control flow are the thing under
# test - into a root whose subordinate tools are stubs that fail on purpose. Each stub records that it
# was reached, and the case asserts that it was: a fixture that silently did not run would otherwise
# look exactly like a passing case.
private_root() {
	local root
	root="$(mktemp -d "$scratch/tree.XXXXXX")"
	local file
	for file in lib.sh build.sh test.sh check.sh gen.sh verify.sh; do
		cp "$ROOT/$file" "$root/$file"
	done
	mkdir -p "$root/src/tools" "$root/src/harness" "$root/src/kernel" "$root/.build/state" "$root/.build/boot" "$root/bin"
	# `verify.sh` sources these two before it does anything else; empty ones are enough for the
	# branches below, which publish no evidence and write no result log.
	: >"$root/src/tools/evidence.sh"
	: >"$root/src/tools/result-logs.sh"
	printf '%s' "$root"
}

# A stub that says it was reached and then fails the way the real thing would.
stub() {
	local path="$1" status="$2" message="$3" reached="$4"
	mkdir -p "$(dirname "$path")"
	cat >"$path" <<STUB
#!/usr/bin/env bash
echo reached >>"$reached"
echo "$message" >&2
exit $status
STUB
	chmod +x "$path"
}

# A SUBORDINATE REFUSAL PROPAGATES. `build-shared.sh` is where a real build fails in the way that
# started this milestone - a diagnostic in its own words and a nonzero status - and the outer verdict
# has to be `failed` with that status rather than the words being searched for.
tree="$(private_root)"
reached="$tree/reached"
stub "$tree/src/tools/build-shared.sh" 3 "build-shared: image graph did not stop after emitting its ET_REL seed object" "$reached"
cat >"$tree/src/tools/system-manifest.sh" <<'STUB'
#!/usr/bin/env bash
echo "fixture=fixture"
STUB
chmod +x "$tree/src/tools/system-manifest.sh"
broken_log="$scratch/broken-build.log"
broken_status="$(fresh_status)"
set +e
RUN_STATUS_FILE="$broken_status" "$tree/build.sh" --arch x86_64 --part libs >/dev/null 2>"$broken_log"
broken_exit=$?
set -e
expect_one_verdict "a subordinate build refusal" build.sh failed 3 "$broken_log" "$broken_status" "$broken_exit" || true
if [[ -s "$reached" ]]; then
	note "the broken-build fixture reached its subordinate tool"
else
	complain "the broken-build fixture never reached build-shared.sh - the case proves nothing"
fi
if grep -q "ET_REL seed object" "$broken_log"; then
	note "and the subordinate diagnostic survived beside the verdict"
else
	complain "the subordinate diagnostic was lost"
fi

# `set -e` AFTER SETUP, which is the path an argument refusal cannot stand in for: the run is past
# its own validation, the observer and its cleanups are registered, and a tool it calls dies with a
# status of its own.
tree="$(private_root)"
reached="$tree/reached"
stub "$tree/bin/cargo" 37 "cargo: the fixture compiler refuses this build" "$reached"
cat >"$tree/src/tools/source-path.sh" <<'STUB'
#!/usr/bin/env bash
echo "kernel"
STUB
chmod +x "$tree/src/tools/source-path.sh"
seterr_log="$scratch/set-e.log"
seterr_status="$(fresh_status)"
set +e
PATH="$tree/bin:$PATH" RUN_STATUS_FILE="$seterr_status" "$tree/build.sh" --arch x86_64 --part kernel >/dev/null 2>"$seterr_log"
seterr_exit=$?
set -e
expect_one_verdict "a compiler that fails after setup" build.sh failed 37 "$seterr_log" "$seterr_status" "$seterr_exit" || true
if [[ -s "$reached" ]]; then
	note "the set -e fixture reached its compiler stub"
else
	complain "the set -e fixture never reached the compiler - the case proves nothing"
fi
if compgen -G "$tree/.build/state/build-step.*" >/dev/null; then
	complain "a failed build left its step mark behind"
else
	note "and the failed build still removed its step mark"
fi

# A STALE IMAGE, which is a refusal from a pre-flight rather than from argument handling: the volume
# and its stamp are both there and the stamp does not match the sources.
tree="$(private_root)"
: >"$tree/.build/boot/system-volume-x86_64.img"
printf 'a digest from an older build\n' >"$tree/.build/state/built-x86_64-volume-test"
mkdir -p "$tree/src/user"
printf 'fn main() {}\n' >"$tree/src/user/fixture.rs"
stale_log="$scratch/stale.log"
stale_status="$(fresh_status)"
set +e
RUN_STATUS_FILE="$stale_status" "$tree/test.sh" --arch x86_64 --build-only >/dev/null 2>"$stale_log"
stale_exit=$?
set -e
expect_one_verdict "a stale image" test.sh failed 1 "$stale_log" "$stale_status" "$stale_exit" || true
if grep -q "does not match the sources" "$stale_log"; then
	note "the stale-image fixture produced the pre-flight's own refusal"
else
	complain "the stale-image fixture failed for some other reason: $(tail -2 "$stale_log" | tr '\n' ' ')"
fi

# THE VERIFICATION BRANCHES. The planner is a stub, so the branch is exercised without a real model:
# a plan that cannot be produced is a failure with a verdict, and the prepared-plan executor runs
# synthetic steps whose commands are `true` and `false` and carries the failing one's status out.
tree="$(private_root)"
reached="$tree/reached"
stub "$tree/bin/cargo" 5 "the fixture planner refuses to plan" "$reached"
plan_log="$scratch/plan.log"
plan_status="$(fresh_status)"
set +e
PATH="$tree/bin:$PATH" RUN_STATUS_FILE="$plan_status" "$tree/verify.sh" --for src/abi --plan >/dev/null 2>"$plan_log"
plan_exit=$?
set -e
if [[ "$plan_exit" == 0 ]]; then
	complain "a planner that refused produced a successful verify.sh"
else
	expect_one_verdict "a planner that refuses" verify.sh failed "$plan_exit" "$plan_log" "$plan_status" "$plan_exit" || true
fi
if [[ -s "$reached" ]]; then
	note "the planner branch reached its stub"
else
	complain "the planner branch never called the planner"
fi

# THE PREPARED-PLAN EXECUTOR, over steps that need nothing from the tree.
tree="$(private_root)"
steps="$tree/steps.tsv"
printf 'STEP\t1\ta step that works\ttrue\nSTEP\t2\ta step that fails\tfalse\n' >"$steps"
exec_log="$scratch/executor.log"
exec_status="$(fresh_status)"
set +e
LIBER_VERIFY_STEPS="$steps" RUN_STATUS_FILE="$exec_status" "$tree/verify.sh" --for src/abi >/dev/null 2>"$exec_log"
exec_exit=$?
set -e
if [[ "$exec_exit" == 0 ]]; then
	complain "a prepared plan with a failing step reported success"
else
	expect_one_verdict "a prepared plan with a failing step" verify.sh failed "$exec_exit" "$exec_log" "$exec_status" "$exec_exit" || true
fi

# A MODEL QUERY THAT FAILS, which is the `exec` fix's most direct proof: the planner's own status has
# to reach the dispatcher, and `exec` could not let it.
tree="$(private_root)"
reached="$tree/reached"
stub "$tree/bin/cargo" 5 "the fixture planner refuses to answer" "$reached"
query_log="$scratch/query-fails.log"
query_status="$(fresh_status)"
set +e
PATH="$tree/bin:$PATH" RUN_STATUS_FILE="$query_status" "$tree/verify.sh" --catalog >/dev/null 2>"$query_log"
query_exit=$?
set -e
expect_one_verdict "a model query that fails" verify.sh failed 5 "$query_log" "$query_status" "$query_exit" || true
if [[ -s "$reached" ]]; then
	note "and the model query reached the planner it was answering from"
else
	complain "the model-query fixture never called the planner"
fi

# AND ONE THAT SUCCEEDS, so the branch is not proved only by its failures.
tree="$(private_root)"
cat >"$tree/bin/cargo" <<'STUB'
#!/usr/bin/env bash
echo "a plan the fixture produced"
STUB
chmod +x "$tree/bin/cargo"
plan_ok_log="$scratch/plan-ok.log"
plan_ok_status="$(fresh_status)"
set +e
PATH="$tree/bin:$PATH" RUN_STATUS_FILE="$plan_ok_status" "$tree/verify.sh" --for src/abi --plan >/dev/null 2>"$plan_ok_log"
plan_ok_exit=$?
set -e
expect_one_verdict "a plan that is produced" verify.sh ok 0 "$plan_ok_log" "$plan_ok_status" "$plan_ok_exit" || true

# THE SWEEP BRANCH, whose cleanup is a WORKTREE. Registered the moment the worktree exists, so a
# failure after that still removes it - which is the property that matters and the one an EXIT trap
# replaced by a later `trap` would have lost.
tree="$(private_root)"
(
	cd "$tree" &&
		git init --quiet . &&
		git config user.email fixture@example.invalid &&
		git config user.name fixture &&
		git add -A >/dev/null 2>&1 &&
		git commit --quiet -m "the fixture tree"
) >/dev/null 2>&1 || note "the sweep fixture could not make a repository; the case is skipped"
if [[ -d "$tree/.git" ]]; then
	stub "$tree/bin/cargo" 6 "the fixture planner refuses the sweep" "$tree/reached"
	sweep_log="$scratch/sweep.log"
	sweep_status="$(fresh_status)"
	set +e
	PATH="$tree/bin:$PATH" RUN_STATUS_FILE="$sweep_status" "$tree/verify.sh" --sweep >/dev/null 2>"$sweep_log"
	sweep_exit=$?
	set -e
	if [[ "$sweep_exit" == 0 ]]; then
		complain "a sweep whose planner refused reported success"
	else
		expect_one_verdict "a sweep that fails after its worktree exists" verify.sh failed "$sweep_exit" "$sweep_log" "$sweep_status" "$sweep_exit" || true
	fi
	if compgen -G "$tree/.build/sweep/*" >/dev/null; then
		local left=("$tree"/.build/sweep/*)
		complain "the sweep left its worktree behind: ${left[0]}"
	else
		note "the sweep removed the worktree it owned"
	fi
fi

# ---------------------------------------------------------------- cleanup composition
#
# Cleanups run ONCE, in order, even when one of them fails; the original status survives all of them;
# and a failure between acquiring a resource and the rest of setup still cleans up.
cat >"$scratch/cleanups.sh" <<'INNER'
SCRIPT_NAME=cleanups.sh
source "$1/lib.sh"
arm_run_verdict
# CAPTURED, because `$2` inside a function is the FUNCTION's argument and not this script's - which
# is the same trap that broke the generator's argument passing an hour ago.
trace="$2"
first() {
	echo first >>"$trace"
}
second() {
	echo second >>"$trace"
	return 1
}
third() {
	echo third >>"$trace"
}
register_run_cleanup first
register_run_cleanup second
register_run_cleanup third
exit 9
INNER
cleanup_trace="$scratch/cleanup.trace"
cleanup_log="$scratch/cleanup.log"
cleanup_status="$(fresh_status)"
set +e
RUN_STATUS_FILE="$cleanup_status" bash "$scratch/cleanups.sh" "$ROOT" "$cleanup_trace" >/dev/null 2>"$cleanup_log"
cleanup_exit=$?
set -e
expect_one_verdict "cleanups compose" cleanups.sh failed 9 "$cleanup_log" "$cleanup_status" "$cleanup_exit" || true
if [[ "$(tr '\n' ' ' <"$cleanup_trace")" == "first second third " ]]; then
	note "every cleanup ran once, in order, and a failing one did not stop the rest"
else
	complain "the cleanups ran as '$(tr '\n' ' ' <"$cleanup_trace")'"
fi
if grep -q "cleanup 'second' failed" "$cleanup_log"; then
	note "and the failing cleanup was diagnosed without replacing the run's status"
else
	complain "a failing cleanup was not diagnosed"
fi

# ---------------------------------------------------------------- the boundary this cannot cross
#
# A SIGKILL leaves no chance to report, and the honest answer is that a missing record is UNKNOWN
# rather than success. This documents the limit by demonstrating it, and it is the one case where
# the absence of a record is the expected observation.
cat >"$scratch/abrupt.sh" <<'INNER'
SCRIPT_NAME=abrupt.sh
source "$1/lib.sh"
arm_run_verdict
echo started >"$2"
sleep 30
INNER
abrupt_marker="$scratch/abrupt.started"
abrupt_status="$(fresh_status)"
rm -f "$abrupt_marker"
RUN_STATUS_FILE="$abrupt_status" setsid bash "$scratch/abrupt.sh" "$ROOT" "$abrupt_marker" >/dev/null 2>&1 &
abrupt_pid=$!
for _ in $(seq 1 100); do
	[[ -f "$abrupt_marker" ]] && break
	sleep 0.1
done
kill -KILL "$abrupt_pid" 2>/dev/null || true
wait "$abrupt_pid" 2>/dev/null || true
if [[ -f "$abrupt_status" ]]; then
	complain "a SIGKILLed run published a terminal record, which nothing can promise"
else
	note "a SIGKILLed run leaves no record: absence is unknown, never success"
fi

# ---------------------------------------------------------------- the build's stall observer
#
# The window is validated before anything is created for it, and an invalid value ends the run the
# ordinary way rather than being read as zero - which is what a word compared with `-ge` becomes.
expect_run "a word as a stall window" build.sh failed 1 ./build.sh --stall abc --part libs --arch x86_64
expect_run "a negative stall window" build.sh failed 1 ./build.sh --stall -5 --part libs --arch x86_64
expect_run "a fractional stall window" build.sh failed 1 ./build.sh --stall 1.5 --part libs --arch x86_64
expect_run "a stall window out of range" build.sh failed 1 ./build.sh --stall 99999999999 --part libs --arch x86_64
expect_run "a stall flag with no value" build.sh failed 1 ./build.sh --stall

# AN INVALID ENVIRONMENT DOES NOT DEFEAT A VALID FLAG: the flag is the one the person typed.
stall_log="$scratch/stall-override.log"
stall_status="$(fresh_status)"
set +e
BUILD_STALL=nonsense RUN_STATUS_FILE="$stall_status" ./build.sh --stall 0 --part libs --arch x86_64 >/dev/null 2>"$stall_log"
stall_exit=$?
set -e
expect_one_verdict "a valid flag over an invalid environment" build.sh ok 0 "$stall_log" "$stall_status" "$stall_exit" || true

# A LIVE STALL IS REPORTED WHILE IT IS STILL STALLED, once per episode, and a second step rearms it.
# One second of window against steps that take longer than that.
timing_log="$scratch/stall-timing.log"
timing_status="$(fresh_status)"
set +e
RUN_STATUS_FILE="$timing_status" ./build.sh --stall 1 --part libs,user --arch x86_64 >/dev/null 2>"$timing_log"
timing_exit=$?
set -e
expect_one_verdict "a build watched with a one-second window" build.sh ok 0 "$timing_log" "$timing_status" "$timing_exit" || true
stalls="$(grep -c 'STALLED - no step has started' "$timing_log" || true)"
if ((stalls == 0)); then
	complain "a one-second window over slower steps reported no stall at all"
else
	labels="$(grep 'STALLED - no step has started' "$timing_log" | sed -E "s/.*the last one was '(.*)'.*/\1/" | sort -u | tr '\n' ' ')"
	note "the observer reported $stalls stall(s), naming: $labels"
fi

# AND WITH OBSERVATION DISABLED THERE IS NO OBSERVER AND NO REPORT.
quiet_log="$scratch/stall-quiet.log"
quiet_status="$(fresh_status)"
set +e
RUN_STATUS_FILE="$quiet_status" ./build.sh --stall 0 --part libs --arch x86_64 >/dev/null 2>"$quiet_log"
quiet_exit=$?
set -e
expect_one_verdict "a build with observation disabled" build.sh ok 0 "$quiet_log" "$quiet_status" "$quiet_exit" || true
if grep -q 'STALLED' "$quiet_log"; then
	complain "observation was disabled and the observer reported anyway"
else
	note "a disabled window starts no observer"
fi

# THE OBSERVER AND ITS SLEEPER ARE GONE WHEN THE BUILD IS, and so is the mark.
if compgen -G "$ROOT/.build/state/build-step.*" >/dev/null; then
	marks=("$ROOT"/.build/state/build-step.*)
	complain "a finished build left its step mark behind: ${marks[0]}"
else
	note "a finished build leaves no step mark"
fi

if ((failed)); then
	note "a run ended without saying so, or said the wrong thing"
	exit 1
fi
note "every entry point ends with one verdict, its record agrees with it, and a stalled build says so while it is stalled"
