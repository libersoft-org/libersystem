#!/usr/bin/env bash
# Run one kernel test suite with optional tags and a bounded wall-clock timeout.
set -euo pipefail

ARCH="${1:?usage: test-kernel.sh <x86_64|aarch64|riscv64> [tag,tag,...]}"
TAGS="${2:-}"
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

if [[ -n "$TAGS" ]]; then
	DEFAULT_TIMEOUT=3m
	MODE="tags=$TAGS"
else
	DEFAULT_TIMEOUT=15m
	MODE="all tags"
fi
LIMIT="${TEST_TIMEOUT:-$DEFAULT_TIMEOUT}"
echo "[test-$ARCH] $MODE (timeout $LIMIT)"

set +e
(
	cd "$ROOT/kernel"
	TEST=1 TEST_TAGS="$TAGS" SERIAL="file:$GUEST_LOG" timeout --kill-after=5s "$LIMIT" cargo test "${TARGET_ARGS[@]}"
) >"$RUN_LOG" 2>&1
status=$?
set -e
cat "$RUN_LOG"
cat "$GUEST_LOG"

if [[ "$status" -eq 124 || "$status" -eq 137 ]]; then
	last="$(grep -h -E '^[[:alnum:]_]+\.\.\.' "$RUN_LOG" "$GUEST_LOG" | tail -1 | sed -E 's/\.\.\..*$//' || true)"
	[[ -n "$last" ]] || last="unknown"
	echo "[test-$ARCH] TIMEOUT after $LIMIT; last test: $last" >&2
	exit 124
fi
exit "$status"
