#!/usr/bin/env bash
# Build or run one kernel test suite with optional tags and a bounded wall-clock timeout.
set -euo pipefail

ARCH="${1:?usage: test-kernel.sh <x86_64|aarch64|riscv64> [tag,tag,...] [--build-only] [--verbose]}"
shift
TAGS=""
BUILD_ONLY=0
VERBOSE=0
for arg in "$@"; do
	case "$arg" in
	--build-only)
		BUILD_ONLY=1
		;;
	--verbose)
		VERBOSE=1
		;;
	--*)
		echo "unknown option: $arg" >&2
		exit 2
		;;
	*)
		if [[ -n "$TAGS" ]]; then
			echo "expected at most one tag list" >&2
			exit 2
		fi
		TAGS="$arg"
		;;
	esac
done
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO_ROOT="$(cd "$ROOT/.." && pwd)"
LOG_DIR="$REPO_ROOT/.build/logs/test"
mkdir -p "$LOG_DIR"
LOG_STEM="${ARCH}-$(date -u +%Y%m%dT%H%M%SZ)-$$"
GUEST_LOG="$LOG_DIR/$LOG_STEM-guest.log"
RUN_LOG="$LOG_DIR/$LOG_STEM-run.log"

print_full_logs() {
	cat "$RUN_LOG"
	if [[ "$BUILD_ONLY" != "1" ]]; then cat "$GUEST_LOG"; fi
}

print_failure_logs() {
	local tail_lines="${TEST_FAILURE_TAIL:-80}"
	echo "[test-$ARCH] logs: $RUN_LOG $GUEST_LOG" >&2
	echo "[test-$ARCH] run log tail (last $tail_lines lines):" >&2
	tail -n "$tail_lines" "$RUN_LOG" >&2
	if [[ "$BUILD_ONLY" != "1" && -s "$GUEST_LOG" ]]; then
		echo "[test-$ARCH] guest log tail (last $tail_lines lines):" >&2
		tail -n "$tail_lines" "$GUEST_LOG" >&2
	fi
}

case "$ARCH" in
x86_64)
	TARGET_ARGS=()
	;;
aarch64)
	TARGET_ARGS=(--target aarch64-unknown-none)
	;;
riscv64)
	TARGET_ARGS=(--target riscv64gc-unknown-none-elf)
	;;
*)
	echo "unknown test architecture: $ARCH" >&2
	exit 2
	;;
esac

# Budgets are per architecture, because one number cannot serve all three: x86_64 runs under
# KVM and the other two are emulated, measured at 13.6x and 20.9x its wall time for the same
# suite (2026-07-31: 146 tests in 79 s, 138 in 1074 s, 139 in 1653 s).
#
# A single 15m budget was therefore not a budget at all - it was under the real runtime of two
# of the three targets, so `just test-all` could not pass, and the failure reads as TIMEOUT,
# which everyone hears as "it hung" rather than "the number is wrong". Nobody could tell
# whether aarch64 and riscv64 were green, and the way around it was to run tags instead, where
# the same 3m budget is under an emulated tag run and fails the same way.
#
# Each figure below is a measurement times headroom, not a guess: full suites at about 1.7x
# the measured time, tag runs sized so an x86_64 run that fits 3m still fits after the
# emulation multiplier.
case "$ARCH" in
x86_64)
	FULL_TIMEOUT=15m
	TAG_TIMEOUT=3m
	;;
aarch64)
	# 45m, matching riscv64: both are TCG-emulated, and the asymmetry was arbitrary.
	#
	# Raising a watchdog is normally the wrong answer here, and this tree has said so: a timeout
	# from ONE test grown to twelve cases was fixed by splitting it, not by waiting longer. This is
	# the other case. Measured 2026-08-06: aarch64 completed 142 of 150 tests in the 30 minutes it
	# had - about 12.7 seconds each, so the suite needs roughly 32 - while riscv64, the SLOWER
	# target, finished all 150 in 2161s of its 2700 and sat at 80% of its budget.
	#
	# The suite has grown across many milestones and this number did not, so it had drifted to
	# 99.4% before the three tests that tipped it, which means any addition at all would have. The
	# tests that tipped it were cheapened first and re-checked to still fail without their fixes;
	# what is left is not a hot spot to split but a budget that stopped matching the work.
	FULL_TIMEOUT=60m
	TAG_TIMEOUT=15m
	;;
riscv64)
	# 90m. Measured 2026-08-10: the suite declared 217 tests and 45 minutes bought 82 of them, with
	# one test alone taking 707 s - so the previous budget could not finish the suite and the run
	# ended at the wall, reporting a TIMEOUT that reads exactly like a hang. That run shared the
	# machine with concurrent builds, so its rate is a pessimistic bound; the clean-rate projection
	# from the 2026-08-06 measurement (150 tests in 2161 s) is about 52 minutes for today's count.
	#
	# 90m covers the clean projection with room and stays under the contended extrapolation. The
	# number is no longer alone: `verify-model check` compares it against the model's measured cost
	# for this target and fails when the budget falls under it, which is what stops the next drift
	# from being discovered forty-five minutes into a sweep.
	FULL_TIMEOUT=90m
	TAG_TIMEOUT=15m
	;;
*)
	FULL_TIMEOUT=15m
	TAG_TIMEOUT=3m
	;;
esac

if [[ "$BUILD_ONLY" == "1" ]]; then
	# A build is host work and does not run the guest, so it is not affected by emulation.
	DEFAULT_TIMEOUT=3m
	if [[ -n "$TAGS" ]]; then
		MODE="build-only tags=$TAGS"
	else
		MODE="build-only all tags"
	fi
elif [[ -n "$TAGS" ]]; then
	DEFAULT_TIMEOUT="$TAG_TIMEOUT"
	MODE="tags=$TAGS"
else
	DEFAULT_TIMEOUT="$FULL_TIMEOUT"
	MODE="all tags"
fi
LIMIT="${TEST_TIMEOUT:-$DEFAULT_TIMEOUT}"
echo "[test-$ARCH] $MODE (timeout $LIMIT)"
START_SECONDS="$SECONDS"
if [[ -n "${LIBER_TIMING_LOG:-}" ]]; then
	LIBER_TIMING_LOG="$(realpath -m "$LIBER_TIMING_LOG")"
	export LIBER_TIMING_LOG
	printf '%s\ttest_driver\tstart\n' "$(date +%s%N)" >>"$LIBER_TIMING_LOG"
fi

TEST_ARGS=(test "${TARGET_ARGS[@]}")
if [[ "$BUILD_ONLY" == "1" ]]; then
	TEST_ARGS+=(--no-run)
fi

# A per-TEST watchdog, because the per-suite one cannot tell a slow run from a stopped one.
#
# `--timeout` bounds the whole suite, so a run that stops dead on test 83 of 228 burns the entire
# remaining budget before saying anything, and what it then says is "TIMEOUT, last test: X" - which
# is also what a genuinely slow run says. That ambiguity cost two hours: a riscv64 run was read as a
# livelock and chased through four subsystems, and the region it was in really does take minutes per
# test. Nothing on the line distinguished the two.
#
# This watches PROGRESS rather than wall-clock: as long as tests keep completing, it says nothing,
# however long the suite takes. When none completes for `TEST_STALL` seconds it kills the guest and
# names the test that was running, which is the answer somebody would otherwise get by staring at a
# log for twenty minutes.
#
# The default is generous on purpose. The slowest single test on emulated riscv64 runs into minutes,
# and a watchdog that fires on a slow test is worse than none: it would turn a passing run into a
# failure and teach everyone to raise it until it never fires.
STALL="${TEST_STALL:-900}"
STALL_MARK="$RUN_LOG.stall"
: >"$STALL_MARK"
rm -f "$STALL_MARK.hit"
# COMPLETED TESTS, from the runner's own completion lines rather than from any `[ok]` anywhere.
#
# It counted every occurrence of `[ok]` in either log, and that string is ordinary display text: a
# test whose own output contains it, a repeated banner, a driver reporting a device as ok - all of
# them reset the quiet counter, so a real hang could be held off indefinitely by output that
# completes nothing. The runner prints `<test id>...\t[ok]` per finished test, so a completion is a
# line that has BOTH the `...` opener and the marker, which display text does not.
progress_count() {
	cat "$RUN_LOG" "$GUEST_LOG" 2>/dev/null | grep -c -E '^[[:alnum:]_.]+\.\.\..*\[ok\]' || true
}
# IT KILLS ITS OWN RUN'S GUEST, BY PID, AND NOTHING ELSE'S.
#
# This used to end with `pkill -f "qemu-system-$ARCH"`, which matches by PATTERN - so a watchdog left
# behind by a run whose cargo and QEMU had been killed kept polling its own log, saw no progress, and
# shot down an unrelated guest started minutes later. That happened while debugging P02M0133 and cost
# a measurement: the victim run reported `FAIL (exit 101, 54s)` with `signal: 9, SIGKILL` over a
# guest log full of passing tests, which reads exactly like a kernel defect and is not one.
#
# Two changes, both about the watchdog only ever affecting the run that started it: it waits for the
# guest to EXIST and remembers its pid, and it exits when its parent is gone as well as when its
# stall mark is removed. A watchdog whose run has already ended has nothing left to watch.
(
	last=-1
	quiet=0
	parent=$$
	while sleep 30; do
		[[ -f "$STALL_MARK" ]] || break
		kill -0 "$parent" 2>/dev/null || break
		now="$(progress_count)"
		if [[ "$now" != "$last" ]]; then
			last="$now"
			quiet=0
		else
			quiet=$((quiet + 30))
		fi
		if ((quiet >= STALL)); then
			stuck="$(grep -h -E '^[[:alnum:]_]+\.\.\.' "$RUN_LOG" "$GUEST_LOG" 2>/dev/null | tail -1 | sed -E 's/\.\.\..*$//' || true)"
			printf '%s\t%s\n' "${stuck:-unknown}" "$quiet" >"$STALL_MARK.hit"
			# By pid, and only a guest this shell is an ancestor of - `pgrep` matches every QEMU on
			# the machine, so ancestry is what makes the victim ours.
			#
			# COLLECTED WHOLE, then chosen from. This pipeline used to end in `head -n 1` inside a
			# command substitution: with more than one candidate, `head` closes the pipe, the loop
			# upstream dies of SIGPIPE, `pipefail` makes that the pipeline's status, and `set -e`
			# ends the WATCHER - at the exact moment it was about to kill the guest, and after it
			# had already written its `.hit` file. The stall bound then silently collapsed back to
			# the suite timeout, and the run was classified by a mark left by a watchdog that never
			# acted. `check-source-hygiene.sh` names this shape, and this was the tree's own
			# outstanding violation of it.
			candidates=()
			while read -r pid; do
				walk="$pid"
				while [[ -n "$walk" && "$walk" != "1" ]]; do
					if [[ "$walk" == "$parent" ]]; then
						candidates+=("$pid")
						break
					fi
					walk="$(ps -o ppid= -p "$walk" 2>/dev/null | tr -d ' ')"
				done
			done < <(pgrep -f "qemu-system-$ARCH" 2>/dev/null || true)
			if ((${#candidates[@]} == 1)); then
				kill "${candidates[0]}" 2>/dev/null || true
			elif ((${#candidates[@]} == 0)); then
				echo "test-kernel: stalled for ${quiet}s and no guest of this run is running - nothing to stop" >&2
			else
				# Ambiguous ownership is stated rather than resolved by picking the first. One run
				# owns one guest; more than one means the assumption this watchdog acts on is wrong,
				# and killing an arbitrary member of the set is how it would take down the wrong one.
				echo "test-kernel: stalled for ${quiet}s but this run has ${#candidates[@]} guests (${candidates[*]}) - refusing to guess which to stop" >&2
			fi
			break
		fi
	done
) &
STALL_WATCHER=$!

set +e
(
	cd "$ROOT/kernel"
	# TEST_SELECTION is read by the runner ahead of TEST_TAGS: an exact list of stable IDs, which is
	# what a selector's answer actually is. Both are `option_env!`, so they are compile-time - the
	# kernel is rebuilt for a different selection, which is why the runner refuses an unknown ID
	# rather than skipping it.
	TEST=1 TEST_TAGS="$TAGS" TEST_SELECTION="${TEST_SELECTION:-}" SERIAL="file:$GUEST_LOG" timeout --kill-after=5s "$LIMIT" cargo "${TEST_ARGS[@]}"
) >"$RUN_LOG" 2>&1
status=$?
set -e
rm -f "$STALL_MARK"
kill "$STALL_WATCHER" 2>/dev/null || true
wait "$STALL_WATCHER" 2>/dev/null || true
if [[ -f "$STALL_MARK.hit" ]]; then
	stuck="$(cut -f1 "$STALL_MARK.hit")"
	quiet="$(cut -f2 "$STALL_MARK.hit")"
	rm -f "$STALL_MARK.hit"
	if [[ "$VERBOSE" != "1" ]]; then print_failure_logs; fi
	echo "[test-$ARCH] NO PROGRESS in $stuck - no test completed for ${quiet}s, so the guest was stopped" >&2
	echo "[test-$ARCH] What this knows is that nothing FINISHED in that window, not that anything is wedged - it cannot tell those apart and neither can a person reading the log. Two ways to find out: the [ok] (N s) figures above say what this target's slow tests actually cost, and TEST_STALL=<seconds> raises the window if one of them legitimately exceeds it." >&2
	exit 125
fi
if [[ -n "${LIBER_TIMING_LOG:-}" ]]; then printf '%s\ttest_driver\tend\n' "$(date +%s%N)" >>"$LIBER_TIMING_LOG"; fi
elapsed=$((SECONDS - START_SECONDS))
if [[ "$VERBOSE" == "1" ]]; then print_full_logs; fi

if [[ "$status" -eq 124 || "$status" -eq 137 ]]; then
	if [[ "$VERBOSE" != "1" ]]; then print_failure_logs; fi
	if [[ "$BUILD_ONLY" == "1" ]]; then
		echo "[test-$ARCH] BUILD TIMEOUT after $LIMIT" >&2
		exit 124
	fi
	last="$(grep -h -E '^[[:alnum:]_]+\.\.\.' "$RUN_LOG" "$GUEST_LOG" | tail -1 | sed -E 's/\.\.\..*$//' || true)"
	[[ -n "$last" ]] || last="unknown"
	echo "[test-$ARCH] TIMEOUT after $LIMIT; last test: $last" >&2
	exit 124
fi
if [[ "$BUILD_ONLY" == "1" ]]; then
	if [[ "$status" -eq 0 ]]; then
		echo "[test-$ARCH] BUILD PASS (${elapsed}s); logs: $RUN_LOG"
	else
		if [[ "$VERBOSE" != "1" ]]; then print_failure_logs; fi
		echo "[test-$ARCH] BUILD FAIL (exit $status, ${elapsed}s); logs: $RUN_LOG" >&2
	fi
	exit "$status"
fi
if [[ "$status" -eq 0 ]] && ! grep -hEq '^test suite complete: [0-9]+ passed' "$RUN_LOG" "$GUEST_LOG"; then
	if [[ "$VERBOSE" != "1" ]]; then print_failure_logs; fi
	# In test mode the only legitimate way out is the debug-exit device: the runner writes
	# 0x10 for pass (QEMU exits 33, mapped to 0) or 0x11 for fail (35). A plain 0 means QEMU
	# ended without the guest writing that port at all, and with `-no-reboot` that is what a
	# guest RESET looks like - a triple fault, or a shutdown nothing in a test should be
	# asking for. The old wording here was "QEMU exited successfully", which reads as benign
	# and sent one investigation looking for a clean shutdown; the guest did not exit, it
	# died in a way that leaves no message because the fault outran the fault handler.
	echo "[test-$ARCH] GUEST RESET: QEMU ended without the debug-exit signal, so the guest reset or powered off" >&2
	echo "[test-$ARCH] the last line of the guest log names the test it happened in; every test after it never ran" >&2
	exit 1
fi
if [[ "$status" -eq 0 ]]; then
	result="$(grep -hE '^test suite complete: [0-9]+ passed' "$RUN_LOG" "$GUEST_LOG" | tail -1 | tr -d '\r')"
	echo "[test-$ARCH] PASS: $result (${elapsed}s); logs: $RUN_LOG $GUEST_LOG"
else
	if [[ "$VERBOSE" != "1" ]]; then print_failure_logs; fi
	echo "[test-$ARCH] FAIL (exit $status, ${elapsed}s); logs: $RUN_LOG $GUEST_LOG" >&2
fi
exit "$status"
