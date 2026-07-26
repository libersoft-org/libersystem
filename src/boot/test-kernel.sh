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
LOG_DIR="$REPO_ROOT/.build/test-logs"
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

if [[ "$BUILD_ONLY" == "1" ]]; then
	DEFAULT_TIMEOUT=3m
	if [[ -n "$TAGS" ]]; then
		MODE="build-only tags=$TAGS"
	else
		MODE="build-only all tags"
	fi
elif [[ -n "$TAGS" ]]; then
	DEFAULT_TIMEOUT=3m
	MODE="tags=$TAGS"
else
	DEFAULT_TIMEOUT=15m
	MODE="all tags"
fi
LIMIT="${TEST_TIMEOUT:-$DEFAULT_TIMEOUT}"
echo "[test-$ARCH] $MODE (timeout $LIMIT)"
START_SECONDS="$SECONDS"

TEST_ARGS=(test "${TARGET_ARGS[@]}")
if [[ "$BUILD_ONLY" == "1" ]]; then
	TEST_ARGS+=(--no-run)
fi

set +e
(
	cd "$ROOT/kernel"
	TEST=1 TEST_TAGS="$TAGS" SERIAL="file:$GUEST_LOG" timeout --kill-after=5s "$LIMIT" cargo "${TEST_ARGS[@]}"
) >"$RUN_LOG" 2>&1
status=$?
set -e
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
	echo "[test-$ARCH] INCOMPLETE: QEMU exited successfully without the test-suite completion marker" >&2
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
