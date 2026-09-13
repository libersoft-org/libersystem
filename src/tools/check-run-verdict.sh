#!/usr/bin/env bash
# EVERY ENTRY POINT ENDS WITH A VERDICT, AND THIS PROVES IT BY MAKING THEM FAIL.
#
# The verdict exists because a failed run and a running run were indistinguishable: both produced a
# log that had stopped growing, and a build that had failed in nine seconds was waited on for thirty
# minutes. A gate that only ran the scripts SUCCESSFULLY would prove nothing about that - the whole
# defect lives on the failure paths, which is where the shapes differ.
#
# So each case below makes a script fail in a DIFFERENT way and requires the same one line out of it:
# a bad flag (the script's own refusal), a stale image (a pre-flight refusal from a sub-script), and
# a signal (no code of ours runs at all). Signals are the case that was wrong when this was written -
# bash runs an EXIT trap with `$?` still zero after one, so a shot-down run claimed success.

set -euo pipefail
SCRIPT_NAME=check-run-verdict.sh
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

failed=0
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

# One case: run something that must fail, and require the verdict line with the expected outcome.
expect_verdict() {
	local what="$1" wanted_outcome="$2" wanted_exit="$3"
	shift 3
	local log="$scratch/$what.log"
	set +e
	"$@" >/dev/null 2>"$log"
	local status=$?
	set -e
	local line
	line="$(grep -E ': RESULT (ok|failed) exit=[0-9]+ seconds=[0-9]+$' "$log" | tail -1 || true)"
	if [[ -z "$line" ]]; then
		echo "check-run-verdict: $what produced NO verdict line - a reader cannot tell it ended" >&2
		failed=1
		return
	fi
	if [[ "$line" != *"RESULT $wanted_outcome"* ]]; then
		echo "check-run-verdict: $what reported '$line' and the run $wanted_outcome" >&2
		failed=1
		return
	fi
	if [[ -n "$wanted_exit" && "$line" != *"exit=$wanted_exit "* ]]; then
		echo "check-run-verdict: $what reported '$line' and exited $wanted_exit" >&2
		failed=1
		return
	fi
	if [[ "$wanted_outcome" == "failed" && "$status" == 0 ]]; then
		echo "check-run-verdict: $what said it failed and exited 0" >&2
		failed=1
		return
	fi
	echo "check-run-verdict: $what -> ${line#*: }"
}

# A SCRIPT'S OWN REFUSAL. `die` exits 1 from the middle of argument parsing, long before any work.
expect_verdict "a refused flag" failed 1 ./build.sh --part nosuchpart
expect_verdict "a refused architecture" failed 1 ./test.sh --arch nosucharch

# SUCCESS, so the gate is not just proving that everything fails. `--list` does no work and cannot
# be slow, which is what makes it usable in a gate.
expect_verdict "a listing" ok 0 ./gen.sh --list

# A SIGNAL, where no code of ours runs at all and the EXIT trap is all there is. 143 is 128+SIGTERM,
# the shell's own convention; reporting `ok` here is the failure this case exists for.
cat >"$scratch/signalled.sh" <<'INNER'
SCRIPT_NAME=signalled.sh
source "$1/lib.sh"
arm_run_verdict
kill -TERM $$
sleep 30
INNER
expect_verdict "a signalled run" failed 143 bash "$scratch/signalled.sh" "$ROOT"

# AND THE STATUS FILE, whose EXISTENCE is the signal a watcher reads. It must not exist while a run
# is going and must exist when it has ended, whichever way it ended.
status="$scratch/run.status"
rm -f "$status"
RUN_STATUS_FILE="$status" ./build.sh --part nosuchpart >/dev/null 2>&1 || true
if [[ ! -f "$status" ]]; then
	echo "check-run-verdict: a failed run wrote no status file, so 'has it finished' has no answer" >&2
	failed=1
elif ! grep -q '^outcome=failed$' "$status"; then
	echo "check-run-verdict: the status file does not carry the outcome: $(tr '\n' ' ' <"$status")" >&2
	failed=1
else
	echo "check-run-verdict: a failed run left a status file saying so"
fi

if [[ "$failed" != 0 ]]; then
	echo "check-run-verdict: FAILED" >&2
	exit 1
fi
echo "check-run-verdict: every entry point ends with a verdict, on a refusal, on success and on a signal"
