#!/usr/bin/env bash
# Build or run one kernel test suite with optional tags and a bounded wall-clock timeout.
set -euo pipefail

ARCH="${1:?usage: test-kernel.sh <x86_64|aarch64|riscv64> [tag,tag,...] [--build-only]}"
shift
TAGS=""
BUILD_ONLY=0
for arg in "$@"; do
	case "$arg" in
	--build-only)
		BUILD_ONLY=1
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
GUEST_LOG="$(mktemp "${TMPDIR:-/tmp}/libersystem-test-${ARCH}-guest.XXXXXX.log")"
RUN_LOG="$(mktemp "${TMPDIR:-/tmp}/libersystem-test-${ARCH}-run.XXXXXX.log")"
trap 'rm -f "$GUEST_LOG" "$RUN_LOG"' EXIT

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
cat "$RUN_LOG"
if [[ "$BUILD_ONLY" != "1" ]]; then
	cat "$GUEST_LOG"
fi

if [[ "$status" -eq 124 || "$status" -eq 137 ]]; then
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
	exit "$status"
fi
if [[ "$status" -eq 0 ]] && ! grep -hEq '^test suite complete: [0-9]+ passed' "$RUN_LOG" "$GUEST_LOG"; then
	echo "[test-$ARCH] INCOMPLETE: QEMU exited successfully without the test-suite completion marker" >&2
	exit 1
fi
exit "$status"
